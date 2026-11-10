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
    /// The survey count each live session was attached at, so a lost pane is
    /// judged only by a listing taken after the attach.
    pub(super) live_since: HashMap<String, u64>,
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
}

impl Attach {
    pub(super) fn new() -> Self {
        Self {
            failed: HashMap::new(),
            live_since: HashMap::new(),
            failed_version: 0,
            attaching: HashMap::new(),
            attached: std::sync::mpsc::channel(),
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
}

/// How long the *same* failed attempt is left alone before it is made again.
///
/// v1 retries a down host on the same cadence (`REMOTE_RETRY_INTERVAL`). Without
/// it a session whose host was offline at startup stays dead for the life of the
/// process, because the candidate pane never changes.
pub(super) const ATTACH_RETRY_INTERVAL: std::time::Duration = std::time::Duration::from_secs(20);

/// What a remote session shows while the connection it lost is re-opened.
pub(super) const LOST_CONNECTION: &str = "lost the connection to its host; reconnecting";

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
            let candidate = self.candidate_pane(row);
            // The same attempt would fail the same way; a different one is worth
            // making.
            if self.attach.failed.get(&row.id).is_some_and(|failure| {
                failure.pane == candidate && failure.at.elapsed() < ATTACH_RETRY_INTERVAL
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

    /// The pane to attach `row` to: the row's own pane id, unless a listing
    /// contradicts it.
    pub(super) fn candidate_pane(
        &self,
        row: &crate::kernel::snapshot::SessionRow,
    ) -> Option<String> {
        row.backend_id
            .clone()
            .filter(|id| !self.pane_is_stale(row, id))
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
        let lost: Vec<(String, Option<String>, bool)> = snapshot
            .sessions
            .iter()
            .filter_map(|row| {
                let live = self.live.get(&row.id)?;
                let remote = crate::session::Route::is_remote_key(&row.backend);
                let moved = row.backend_id.as_deref().is_some_and(|id| {
                    !id.is_empty()
                        && id != live.session.backend_id()
                        // A local id may evict a live pane only when a listing
                        // actually places it in this session's own window —
                        // otherwise an id left over from a previous tmux server
                        // drops the pane just resolved by name, on every frame,
                        // forever. Remote rows are never surveyed, so there is no
                        // listing to ask and the row's own id has to stand.
                        && (remote || self.pane_placed(row, id))
                });
                let surveyed_after_attach =
                    self.attach.live_since.get(&row.id).is_some_and(|seen| {
                        self.discovery
                            .surveys
                            .get(&self.key(&row.backend))
                            .is_some_and(|now| now > seen)
                    });
                let confirmed_missing = surveyed_after_attach
                    && self.liveness(row) == crate::backend::BackendLiveness::Missing;
                (live.session.has_exited() || moved || confirmed_missing).then(|| {
                    (
                        row.id.clone(),
                        row.backend_id.clone(),
                        remote && !confirmed_missing,
                    )
                })
            })
            .collect();
        for (id, pane, remote) in lost {
            self.live.remove(&id);
            self.attach.live_since.remove(&id);
            if remote {
                // The backend has to be readied again before the next attach can
                // adopt anything: the connection this session died with is the
                // one every other session on that host shares.
                self.ready.borrow_mut().clear();
                self.fail(&id, pane, LOST_CONNECTION.to_string());
                // And tried again on the next frame rather than after
                // `ATTACH_RETRY_INTERVAL`. The connection ending says nothing
                // about the host: it is re-opened on demand, usually at once
                // (a command that timed out has often replaced it already), and
                // holding every session on the host as unreachable for the
                // interval is what made a working host read as down. An attempt
                // that does fail records its own error and waits as usual.
                if let Some(failure) = self.attach.failed.get_mut(&id) {
                    failure.at = failure
                        .at
                        .checked_sub(ATTACH_RETRY_INTERVAL)
                        .unwrap_or(failure.at);
                }
            } else {
                // Locally the pane is simply gone. Recorded against the pane that
                // died, so the retry rule treats a *different* candidate — the
                // one a restart just created — as worth trying at once.
                self.fail(&id, pane, "session has no pane yet".to_string());
            }
        }
    }

    /// Whether a backend's control-mode connection is already open.
    pub(super) fn backend_is_ready(&self, backend: &str) -> bool {
        self.ready.borrow().contains(backend)
    }

    /// Whether some worker is already opening this backend's connection.
    pub(super) fn opening(&self, backend: &str) -> bool {
        self.discovery.discovering.contains(backend)
            || self.attach.attaching.values().any(|name| name == backend)
    }

    /// Attach a Fleet session's pane on a worker, through the stream `fleet
    /// attach` names ([`super::fleet`]). A row whose Task has no session yet
    /// (no key) has nothing to attach to; a failed attempt is made again
    /// after [`ATTACH_RETRY_INTERVAL`], and a stream that ended is attached
    /// afresh, since `drop_lost_panes` drops a pane whose output ended.
    fn attach_fleet(&mut self, row: &crate::kernel::snapshot::SessionRow, rows: u16, cols: u16) {
        let Some(program) = self.fleet.clone() else {
            return;
        };
        let Some(key) = row.key.clone() else {
            self.fail(&row.id, None, "session has no pane yet".to_string());
            return;
        };
        if self.attach.failed.get(&row.id).is_some_and(|failure| {
            failure.pane.as_deref() == Some(key.as_str())
                && failure.at.elapsed() < ATTACH_RETRY_INTERVAL
        }) {
            return;
        }
        let tx = self.attach.attached.0.clone();
        let session = row.id.clone();
        let scrollback = self.scrollback;
        let runtime = self.runtime.clone();
        let notices = super::stream_pane::Notices::new(self.input_notices.0.clone(), &row.name);
        let shell = self
            .shell_route(row.remote_host.as_deref())
            .and_then(|route| self.backend_on(&route));
        self.attach
            .attaching
            .insert(session.clone(), super::fleet::BACKEND.to_string());
        std::thread::spawn(move || {
            // The pane's reader and writer run on the process's runtime.
            let _guard = runtime.as_ref().map(|handle| handle.enter());
            let result =
                super::fleet::open(&program, &session, rows, cols, scrollback, notices, shell);
            let _ = tx.send(Attached {
                session,
                pane: key,
                backend: super::fleet::BACKEND.to_string(),
                readied: false,
                session_handle: result,
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
        // A row whose agent has no definition is not attached. Nothing here
        // launches the agent (that is Fleet's), so the definition itself
        // is not needed — only that there is one, as there always had to be.
        if self
            .agents
            .get(&row.agent)
            .or_else(|| self.agents.default_agent())
            .is_none()
        {
            self.fail(
                &row.id,
                Some(backend_id),
                format!("no agent definition for {}", row.agent),
            );
            return;
        }
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
                            name,
                            rows,
                            cols,
                            &pane,
                            &backend,
                            adopted,
                            HashMap::new(),
                            scrollback,
                        ));
                    }
                    // Nothing is looking at it yet, so where the grid can be had
                    // back later it is not built now — nor its history captured,
                    // which was a round trip per pane on every start.
                    if lazy && backend.capabilities().supports_snapshots {
                        return crate::backend::Session::adopt_dormant(
                            name,
                            rows,
                            cols,
                            &pane,
                            &backend,
                            HashMap::new(),
                            scrollback,
                        )
                        .map_err(|e| e.to_string());
                    }
                    let seed = backend.capture_history(&pane).ok();
                    crate::backend::Session::adopt(
                        name,
                        rows,
                        cols,
                        &pane,
                        &backend,
                        HashMap::new(),
                        seed,
                        scrollback,
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
            });
        });
    }

    /// Fold finished attaches back into the live set.
    pub(super) fn collect_attached(&mut self, rows: u16, cols: u16) {
        while let Ok(done) = self.attach.attached.1.try_recv() {
            self.attach.attaching.remove(&done.session);
            let seen_at = self
                .discovery
                .surveys
                .get(&done.backend)
                .copied()
                .unwrap_or(0);
            if done.readied {
                self.ready.borrow_mut().insert(done.backend);
            }
            match done.session_handle {
                Ok(session) => {
                    self.attach.live_since.insert(done.session.clone(), seen_at);
                    self.live.insert(
                        done.session.clone(),
                        Live {
                            session,
                            // The size it was adopted at; the shell has none
                            // until there is one, and gets its own on the first
                            // frame that paints it.
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
                Err(e) => self.fail(&done.session, Some(done.pane), e),
            }
        }
    }

    /// Record why a session has no pane, and what was tried.
    pub(super) fn fail(&mut self, session: &str, pane: Option<String>, message: String) {
        // Lua reads only the error map; retry bookkeeping is private.
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
    /// well as the message, and only the message is a plugin's business.
    pub fn failures(&self) -> HashMap<String, String> {
        self.attach
            .failed
            .iter()
            .map(|(session, failure)| (session.clone(), failure.message.clone()))
            .collect()
    }

    pub(super) fn mark_failures_changed(&mut self) {
        self.attach.failed_version = self.attach.failed_version.wrapping_add(1);
    }

    /// How many times the published session-to-error map has changed.
    pub fn failed_version(&self) -> u64 {
        self.attach.failed_version
    }
}
