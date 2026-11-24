//! Attaching a session's pane: the worker that adopts it, and the failures
//! that keep a pane from looking empty.

use std::cell::Cell;
use std::collections::HashMap;
use std::sync::mpsc::{Receiver, Sender};

use ratatui::layout::Rect;

use super::{Live, Painted, Terminals};
use crate::kernel::snapshot::Snapshot;

/// The attach side of [`Terminals`]: what is being attached on a worker, what
/// came back, and why a session has no pane.
pub(super) struct Attach {
    /// Why a session could not be attached, so the pane can say so instead of
    /// looking empty. Kept per session and cleared on a successful attach.
    pub(super) failed: HashMap<String, Failure>,
    /// Moves whenever the set of attach failures does. Published on each
    /// session row, so it gates that group alongside `meta_version`.
    pub(super) failed_version: u64,
    /// Attaches running on workers: session id → the backend it is on.
    ///
    /// Attaching is the one thing here that blocks for a *long* time — an ssh
    /// connect to a host that is down runs to its timeout, and the history
    /// capture is a round trip per pane. On the render thread that is the whole
    /// interface frozen before its first paint, so it happens on a worker and
    /// the result is collected here.
    pub(super) attaching: HashMap<String, String>,
    pub(super) attached: (Sender<Attached>, Receiver<Attached>),
    pub(super) retry_after: std::time::Duration,
    pub(super) starts: HashMap<String, (Start, std::time::Instant)>,
    pub(super) start_wait: std::time::Duration,
}

/// Where a start the user asked for stands, until the session's pane is live.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Start {
    /// Fleet is resuming the agent, or its pane is being attached.
    Pending,
    /// Fleet refused the start, or did not resume the agent in time.
    Failed(String),
}

pub(super) const START_WAIT: std::time::Duration = std::time::Duration::from_secs(90);

impl Attach {
    pub(super) fn new() -> Self {
        Self {
            failed: HashMap::new(),
            failed_version: 0,
            attaching: HashMap::new(),
            attached: std::sync::mpsc::channel(),
            retry_after: ATTACH_RETRY_INTERVAL,
            starts: HashMap::new(),
            start_wait: START_WAIT,
        }
    }
}

/// Why one session has no live pane, and what was tried.
///
/// The attempt is part of the record on purpose: a failure keyed only by session
/// latches forever, so a pane that appears a moment later — which is the normal
/// case for a session this interface just created — never gets attached. Keyed
/// by the pane that failed, the next *different* candidate is tried.
pub(super) struct Failure {
    /// The pane id that failed, or `None` when there was none to try.
    pub(super) pane: Option<String>,
    pub(super) message: String,
    /// When it failed, so the same attempt is made again eventually rather than
    /// never: a host that was down comes back, and nothing else would notice.
    /// Set one retry interval earlier for a failure that is to be retried at
    /// once (see `drop_lost_panes`).
    pub(super) at: std::time::Instant,
    pub(super) tried_at: std::time::Instant,
    pub(super) refused_in: Option<FleetRowState>,
}

#[derive(Debug, Clone, PartialEq)]
pub(super) struct FleetRowState {
    status: crate::session::SessionState,
    stopped: bool,
    deleting: bool,
    state_why: Option<String>,
    key: Option<String>,
}

impl FleetRowState {
    fn of(row: &crate::kernel::snapshot::SessionRow) -> Self {
        Self {
            status: row.status,
            stopped: row.stopped,
            deleting: row.deleting.is_some(),
            state_why: row.state_why.clone(),
            key: row.key.clone(),
        }
    }
}

/// An attach that finished on a worker, on its way back to the loop.
pub(super) struct Attached {
    pub(super) session: String,
    /// The pane it tried, so the result can be matched against what the row
    /// still wants — a session can be deleted, or its pane change, mid-attach.
    pub(super) pane: String,
    pub(super) backend: String,
    /// Whether the backend's control-mode connection is now open. Reported back
    /// rather than assumed: the readying happens on the worker, and until it
    /// has, no second attach on that backend may start.
    pub(super) readied: bool,
    pub(super) session_handle: Result<crate::backend::Session, String>,
    pub(super) started: std::time::Instant,
    pub(super) refused_in: Option<FleetRowState>,
}

/// How long the *same* failed attempt is left alone before it is made again.
///
/// v1 retries a down host on the same cadence (`REMOTE_RETRY_INTERVAL`). Without
/// it a session whose host was offline at startup stays dead for the life of the
/// process, because the candidate pane never changes.
pub(super) const ATTACH_RETRY_INTERVAL: std::time::Duration = std::time::Duration::from_secs(20);

pub(super) const PAUSED: &str = "the session is paused: start it to resume its agent";

pub(super) const DELETING: &str = "the Task is being deleted: Fleet is releasing its session";

pub(super) const ENDED: &str = "the session ended: Fleet has nothing to attach";

impl Terminals {
    /// Start an attach for every row that is neither live nor attaching, unless
    /// the same attempt just failed or its backend is still being opened.
    pub(super) fn attach_unresolved(&mut self, snapshot: &Snapshot, rows: u16, cols: u16) {
        for row in &snapshot.sessions {
            if self.live.contains_key(&row.id) || self.attach.attaching.contains_key(&row.id) {
                continue;
            }
            if row.backend == super::fleet::BACKEND {
                self.attach_fleet(row, rows, cols);
                continue;
            }
            let candidate = row.backend_id.clone();
            // The same attempt would fail the same way; a different one is worth
            // making.
            if self.attach.failed.get(&row.id).is_some_and(|failure| {
                failure.pane == candidate && failure.at.elapsed() < self.attach.retry_after
            }) {
                continue;
            }
            let Some(backend_id) = candidate else {
                self.fail(&row.id, None, "session has no pane yet".to_string());
                continue;
            };
            // The first attach on a backend is also what opens its control-mode
            // connection, so the others wait for it rather than racing to open
            // the same one several times over.
            let backend = self.key(&row.backend);
            if !self.backend_is_ready(&backend) && self.opening(&backend) {
                continue;
            }
            self.start_attach(row, backend_id, rows, cols);
        }
    }

    /// Let go of a session whose pane died, so it is re-attached rather than
    /// painting a frozen last screen forever.
    ///
    /// `has_exited` is set when a session's output **stream** ends. Control mode
    /// carries every pane on a backend down one connection, so it fires when the
    /// *connection* goes — a host or ssh dropping, a local tmux server dying —
    /// and, since the kernel started reading tmux's window-close notifications,
    /// when a single pane's window closes as well. A restart is therefore caught
    /// here on its own; [`Terminals::forget`] remains the faster path, because
    /// the restart already knows and need not wait to be told.
    ///
    /// Also let go when the row's pane id has *moved*: the interface is holding a
    /// pane the session no longer claims. That covers a restart on either
    /// transport — both record the pane they spawned — and a row whose window
    /// was re-adopted somewhere else.
    pub(super) fn drop_lost_panes(&mut self, snapshot: &Snapshot) {
        let lost: Vec<(String, Option<String>)> = snapshot
            .sessions
            .iter()
            .filter_map(|row| {
                let live = self.live.get(&row.id)?;
                let moved = row
                    .backend_id
                    .as_deref()
                    .is_some_and(|id| !id.is_empty() && id != live.session.backend_id());
                (live.session.has_exited() || moved)
                    .then(|| (row.id.clone(), row.backend_id.clone()))
            })
            .collect();
        for (id, pane) in lost {
            self.live.remove(&id);
            // Recorded against the pane that died, so the retry rule treats a
            // *different* candidate as worth trying at once.
            self.fail(&id, pane, "session has no pane yet".to_string());
        }
    }

    /// Whether a backend's control-mode connection is already open.
    pub(super) fn backend_is_ready(&self, backend: &str) -> bool {
        self.ready.borrow().contains(backend)
    }

    /// Whether some worker is already opening this backend's connection.
    pub(super) fn opening(&self, backend: &str) -> bool {
        self.attach.attaching.values().any(|name| name == backend)
    }

    fn attach_fleet(&mut self, row: &crate::kernel::snapshot::SessionRow, rows: u16, cols: u16) {
        let Some(link) = row.fleet().and_then(|fleet| self.fleet.get(fleet)).cloned() else {
            return;
        };
        let task = row.task().to_string();
        let Some(key) = row.key.clone() else {
            self.fail(&row.id, None, "session has no pane yet".to_string());
            return;
        };
        let left_pause = row.deleting.is_none()
            && !row.stopped
            && row.status != crate::session::SessionState::Failed
            && self.failure(&row.id) == Some(PAUSED);
        let resuming = row.deleting.is_none()
            && row.state_why.as_deref() == Some(crate::kernel::snapshot::fleet::RESUMING);
        if left_pause || resuming && !self.attach.starts.contains_key(&row.id) {
            self.start_requested(&row.id);
        }
        let unattachable = if row.deleting.is_some() {
            Some(DELETING)
        } else if row.state_why.as_deref() == Some(crate::kernel::snapshot::fleet::SESSION_ENDED) {
            Some(ENDED)
        } else if row.stopped && resuming {
            return;
        } else if row.stopped {
            Some(PAUSED)
        } else {
            None
        };
        if let Some(why) = unattachable {
            if self.failure(&row.id) != Some(why) {
                self.fail(&row.id, None, why.to_string());
            }
            return;
        }
        let start_pending_since = match self.attach.starts.get(&row.id) {
            Some((Start::Pending, at)) => Some(*at),
            _ => None,
        };
        let state = FleetRowState::of(row);
        if self.attach.failed.get(&row.id).is_some_and(|failure| {
            let held = failure.refused_in.as_ref() == Some(&state)
                || failure.pane.as_deref() == Some(key.as_str())
                    && failure.at.elapsed() < self.attach.retry_after;
            held && start_pending_since.is_none_or(|at| failure.tried_at >= at)
        }) {
            return;
        }
        let tx = self.attach.attached.0.clone();
        let session = row.id.clone();
        let scrollback = self.scrollback;
        let runtime = self.runtime.clone();
        let notices = super::stream_pane::Notices::new(self.input_notices.0.clone(), &row.name);
        self.attach
            .attaching
            .insert(session.clone(), super::fleet::BACKEND.to_string());
        let started = std::time::Instant::now();
        std::thread::spawn(move || {
            let _guard = runtime.as_ref().map(|handle| handle.enter());
            let (session_handle, refused_in) =
                match super::fleet::open(&link, &task, rows, cols, scrollback, notices) {
                    Ok(opened) => (Ok(opened), None),
                    Err(failure) => (Err(failure.message), failure.refused.then_some(state)),
                };
            let _ = tx.send(Attached {
                session,
                pane: key,
                backend: super::fleet::BACKEND.to_string(),
                readied: false,
                session_handle,
                started,
                refused_in,
            });
        });
    }

    /// Hand one session's attach to a worker.
    ///
    /// Everything the worker needs is cloned across: the backend is behind an
    /// `Arc`, and the resulting `Session` owns its own reader/writer threads,
    /// so nothing here is borrowed from the loop.
    pub(super) fn start_attach(
        &mut self,
        row: &crate::kernel::snapshot::SessionRow,
        backend_id: String,
        rows: u16,
        cols: u16,
    ) {
        let Some(backend) = self.backend_for(&row.backend) else {
            self.fail(
                &row.id,
                Some(backend_id),
                format!("no backend serves {}", row.backend),
            );
            return;
        };
        // The host a remote session's list row names, resolved here rather
        // than inside the backend contract, which reads no global config.
        let remote_host = crate::kernel::snapshot::remote_host_of(&row.backend);

        let backend_name = self.key(&row.backend);
        let already_ready = self.backend_is_ready(&backend_name);
        let tx = self.attach.attached.0.clone();
        let session = row.id.clone();
        let name = row.name.clone();
        let pane = backend_id.clone();
        self.attach
            .attaching
            .insert(session.clone(), backend_name.clone());
        let runtime = self.runtime.clone();
        let lazy = self.keep_hidden.is_some();
        let stream_view = self
            .stream_view
            .clone()
            .filter(|_| super::stream_pane::views(&row.backend));
        let notices = self.input_notices.0.clone();
        let scrollback = self.scrollback;
        let started = std::time::Instant::now();
        std::thread::spawn(move || {
            let _guard = runtime.as_ref().map(|handle| handle.enter());
            let mut readied = already_ready;
            let mut session_handle = Ok(());
            if !already_ready {
                match backend.ensure_ready() {
                    Ok(()) => readied = true,
                    Err(e) => session_handle = Err(format!("{backend_name}: {e}")),
                }
            }
            // The history capture is a round trip of its own, and seeding the
            // parser with it is what makes an adopted pane show the conversation
            // that is already there rather than a blank screen until the agent
            // next prints. A failure to read it is not a failure to attach.
            let result = session_handle
                .and_then(|()| {
                    if let Some(view) = &stream_view {
                        let argv = view.argv(&pane);
                        let adopted =
                            super::stream_pane::adopt_reporting(&argv, rows, cols, notices, &name)
                                .map_err(|e| format!("the stream pane: {e}"))?;
                        return Ok(crate::backend::Session::adopt_io(
                            name, rows, cols, &pane, &backend, adopted, scrollback,
                        ));
                    }
                    // Nothing is looking at it yet, so where the grid can be had
                    // back later it is not built now — nor its history captured,
                    // which was a round trip per pane on every start.
                    if lazy && backend.capabilities().supports_snapshots {
                        return crate::backend::Session::adopt_dormant(
                            name, rows, cols, &pane, &backend, scrollback,
                        )
                        .map_err(|e| e.to_string());
                    }
                    let seed = backend.capture_history(&pane).ok();
                    crate::backend::Session::adopt(
                        name, rows, cols, &pane, &backend, seed, scrollback,
                    )
                    .map_err(|e| e.to_string())
                })
                .map(|mut adopted| {
                    adopted.info.remote_host = remote_host;
                    adopted
                });
            let _ = tx.send(Attached {
                session,
                pane,
                backend: backend_name,
                readied,
                session_handle: result,
                started,
                refused_in: None,
            });
        });
    }

    /// Fold finished attaches back into the live set.
    pub(super) fn collect_attached(&mut self, rows: u16, cols: u16) {
        while let Ok(done) = self.attach.attached.1.try_recv() {
            self.attach.attaching.remove(&done.session);
            if done.readied {
                self.ready.borrow_mut().insert(done.backend);
            }
            match done.session_handle {
                Ok(session) => {
                    self.live.insert(
                        done.session.clone(),
                        Live {
                            session,
                            agent: Painted {
                                size: Cell::new((rows, cols)),
                                rect: Cell::new(Rect::default()),
                                ..Default::default()
                            },
                            shell: Painted::default(),
                        },
                    );
                    if self.attach.failed.remove(&done.session).is_some() {
                        self.mark_failures_changed();
                    }
                }
                Err(e) => {
                    self.fail_attempt(&done.session, Some(done.pane), e, done.started);
                    if let Some(failure) = self.attach.failed.get_mut(&done.session) {
                        failure.refused_in = done.refused_in;
                    }
                }
            }
        }
    }

    /// Record why a session has no pane, and what was tried.
    pub(super) fn fail(&mut self, session: &str, pane: Option<String>, message: String) {
        self.fail_attempt(session, pane, message, std::time::Instant::now());
    }

    pub(super) fn fail_attempt(
        &mut self,
        session: &str,
        pane: Option<String>,
        message: String,
        tried_at: std::time::Instant,
    ) {
        if self
            .attach
            .failed
            .get(session)
            .map(|failure| &failure.message)
            != Some(&message)
        {
            self.mark_failures_changed();
        }
        self.attach.failed.insert(
            session.to_string(),
            Failure {
                pane,
                message,
                at: std::time::Instant::now(),
                tried_at,
                refused_in: None,
            },
        );
    }

    /// Why a session has no live pane, if it failed to attach.
    ///
    /// Surfaced so the pane can explain itself rather than showing an empty
    /// box — "not attached" with no reason is the least useful thing a
    /// terminal can say.
    pub fn failure(&self, session: &str) -> Option<&str> {
        self.attach
            .failed
            .get(session)
            .map(|failure| failure.message.as_str())
    }

    /// Every attach failure, keyed by session id.
    ///
    /// Owned rather than borrowed because the record carries what was *tried* as
    /// well as the message, and only the message is a mod's business.
    pub fn failures(&self) -> HashMap<String, String> {
        self.attach
            .failed
            .iter()
            .map(|(session, failure)| (session.clone(), failure.message.clone()))
            .collect()
    }

    /// Note that the user asked Fleet to start `session`: its pane reads as
    /// starting until it attaches, its attach fails, or the start fails. A
    /// recorded attach failure is dropped, so the attach is made at once
    /// rather than after the retry interval.
    pub fn start_requested(&mut self, session: &str) {
        self.attach.failed.remove(session);
        self.attach.starts.insert(
            session.to_string(),
            (Start::Pending, std::time::Instant::now()),
        );
        self.mark_failures_changed();
    }

    /// Note that Fleet refused to start `session`, and why.
    pub fn start_failed(&mut self, session: &str, error: &str) {
        if let Some(start) = self.attach.starts.get_mut(session) {
            start.0 = Start::Failed(error.to_string());
            self.mark_failures_changed();
        }
    }

    /// Whether any start is still pending, which keeps the animation clock
    /// running.
    pub fn starting(&self) -> bool {
        self.attach
            .starts
            .values()
            .any(|(start, _)| *start == Start::Pending)
    }

    /// Every start not yet settled by a live pane, keyed by session id.
    pub fn starts(&self) -> HashMap<String, Start> {
        self.attach
            .starts
            .iter()
            .map(|(session, (start, _))| (session.clone(), start.clone()))
            .collect()
    }

    pub(super) fn settle_starts(&mut self, snapshot: &Snapshot) {
        let live = &self.live;
        let failed = &self.attach.failed;
        let wait = self.attach.start_wait;
        let before = self.attach.starts.len();
        let mut timed_out = false;
        self.attach.starts.retain(|session, (start, at)| {
            if live.contains_key(session) || snapshot.session(session).is_none() {
                return false;
            }
            if *start != Start::Pending {
                return true;
            }
            if failed
                .get(session)
                .is_some_and(|failure| failure.pane.is_some() && failure.tried_at >= *at)
            {
                return false;
            }
            if at.elapsed() >= wait {
                *start = Start::Failed(format!(
                    "Fleet did not resume it within {} s",
                    wait.as_secs()
                ));
                timed_out = true;
            }
            true
        });
        if timed_out || self.attach.starts.len() != before {
            self.mark_failures_changed();
        }
    }

    pub(super) fn mark_failures_changed(&mut self) {
        self.attach.failed_version = self.attach.failed_version.wrapping_add(1);
    }

    /// How many times the published session-to-error map has changed.
    pub fn failed_version(&self) -> u64 {
        self.attach.failed_version
    }
}
