//! The dispatch machinery: accept a command, run it on its own thread, keep
//! the in-flight record a plugin can draw. Split from the protocol (`mod.rs`)
//! and the effects (`execute.rs`) so the write side reads as its three
//! halves.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};

use super::execute::execute;
use super::Command;

/// How far along a command is.
///
/// `Stage` carries a name from the operation itself — creation reports which
/// part of the pipeline it reached, because "working" is exactly the wrong
/// answer when the question is *why is this slow*.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Phase {
    Queued,
    Running,
    Stage(String),
    Failed,
}

impl Phase {
    pub fn as_str(&self) -> &str {
        match self {
            Phase::Queued => "queued",
            Phase::Running => "running",
            Phase::Stage(name) => name,
            Phase::Failed => "failed",
        }
    }
}

/// A command that has been accepted but whose effect is not yet visible.
#[derive(Debug, Clone)]
pub struct InFlight {
    pub id: u64,
    pub kind: &'static str,
    pub session: String,
    /// What the command concerns when it names no session yet — the repository
    /// a creation is for, so a pending row can be grouped where the session
    /// will actually land.
    pub subject: Option<String>,
    pub phase: Phase,
    /// Set once the command has failed; retained briefly so a plugin can show
    /// it, then swept.
    pub error: Option<String>,
}

impl InFlight {
    /// Note that the worker has started, unless this row is already finished.
    ///
    /// A phase only ever moves forward. The worker announces itself from its own
    /// thread, so the announcement can be scheduled *after* something has already
    /// resolved the row — and an unguarded write then puts a failed command back
    /// on the progress line, where it hides the error that explains it. Only a
    /// row still reading `Queued` has anything to learn from "it started".
    pub(super) fn started(&mut self) {
        if self.phase == Phase::Queued {
            self.phase = Phase::Running;
        }
    }
}

/// A phase change reported from a running command.
pub(super) struct Progress {
    pub(super) id: u64,
    pub(super) phase: String,
}

/// A finished command, reported back to the loop.
struct Done {
    id: u64,
    error: Option<String>,
}

/// How long a failed command stays readable before being swept.
const FAILURE_LINGER_MS: u64 = 8_000;

/// Accepts commands, runs them off the render path, and reports progress.
pub struct CommandBus {
    inflight: Arc<Mutex<Vec<InFlight>>>,
    finished_tx: Sender<Done>,
    finished_rx: Receiver<Done>,
    progress_tx: Sender<Progress>,
    progress_rx: Receiver<Progress>,
    next_id: AtomicU64,
    /// When each failure was recorded, so it can be swept after lingering.
    failed_at: Vec<(u64, std::time::Instant)>,
}

impl CommandBus {
    pub fn new() -> Self {
        let (finished_tx, finished_rx) = channel();
        let (progress_tx, progress_rx) = channel();
        Self {
            inflight: Arc::new(Mutex::new(Vec::new())),
            finished_tx,
            finished_rx,
            progress_tx,
            progress_rx,
            next_id: AtomicU64::new(1),
            failed_at: Vec::new(),
        }
    }

    /// Accept a command. Returns immediately, always.
    pub fn dispatch(&self, command: Command) -> u64 {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let entry = InFlight {
            id,
            kind: command.kind(),
            session: command.session().to_string(),
            subject: command.subject(),
            phase: Phase::Queued,
            error: None,
        };
        if let Ok(mut inflight) = self.inflight.lock() {
            inflight.push(entry);
        }

        let inflight = self.inflight.clone();
        let finished = self.finished_tx.clone();
        let progress = self.progress_tx.clone();
        // One thread per command: a slow operation on an unreachable host must
        // not queue behind, or in front of, anything else.
        std::thread::spawn(move || {
            if let Ok(mut list) = inflight.lock() {
                if let Some(entry) = list.iter_mut().find(|entry| entry.id == id) {
                    entry.started();
                }
            }
            let error = execute(&command, id, &progress).err();
            let _ = finished.send(Done { id, error });
        });
        id
    }

    /// Report a command finished, as a worker would.
    ///
    /// The worker path is a thread and a channel; a test that wants to observe
    /// what `poll` does with a completion needs the completion without the
    /// thread.
    #[doc(hidden)]
    pub fn finish_for_test(&self, id: u64, error: Option<String>) {
        let _ = self.finished_tx.send(Done { id, error });
    }

    /// Fold finished commands into the in-flight list.
    ///
    /// Returns true when anything completed, so the caller can refresh the
    /// snapshot immediately rather than waiting out the refresh interval —
    /// which is what makes a delete feel instant.
    pub fn poll(&mut self) -> bool {
        // Phase changes first, so a command that reports and then finishes in
        // the same window does not show a stale stage.
        let mut changed = self.drain_progress();
        changed |= self.drain_finished();
        changed |= self.sweep_expired_failures();
        changed
    }

    /// Apply the stage each running command last reported.
    fn drain_progress(&mut self) -> bool {
        let mut changed = false;
        while let Ok(update) = self.progress_rx.try_recv() {
            let Ok(mut list) = self.inflight.lock() else {
                continue;
            };
            if let Some(entry) = list.iter_mut().find(|entry| entry.id == update.id) {
                entry.phase = Phase::Stage(update.phase);
                changed = true;
            }
        }
        changed
    }

    /// Retire completed commands: a success leaves the list, a failure lingers
    /// long enough to be read.
    fn drain_finished(&mut self) -> bool {
        let mut changed = false;
        while let Ok(done) = self.finished_rx.try_recv() {
            changed = true;
            let Ok(mut list) = self.inflight.lock() else {
                continue;
            };
            match done.error {
                // Success: the effect is in the database now, so the row has
                // nothing left to say.
                None => list.retain(|entry| entry.id != done.id),
                Some(error) => {
                    if let Some(entry) = list.iter_mut().find(|entry| entry.id == done.id) {
                        entry.phase = Phase::Failed;
                        entry.error = Some(error);
                    }
                    self.failed_at.push((done.id, std::time::Instant::now()));
                }
            }
        }
        changed
    }

    /// Sweep failures that have been readable long enough.
    fn sweep_expired_failures(&mut self) -> bool {
        let expired: Vec<u64> = self
            .failed_at
            .iter()
            .filter(|(_, at)| at.elapsed().as_millis() as u64 >= FAILURE_LINGER_MS)
            .map(|(id, _)| *id)
            .collect();
        if expired.is_empty() {
            return false;
        }
        if let Ok(mut list) = self.inflight.lock() {
            list.retain(|entry| !expired.contains(&entry.id));
        }
        self.failed_at.retain(|(id, _)| !expired.contains(id));
        true
    }

    /// Everything accepted but not yet done, for publishing to plugins.
    /// Whether anything is in flight, without cloning the list.
    ///
    /// The animation clock asks this on every published frame, and
    /// [`Self::inflight`] clones under a lock — a second copy a frame to answer
    /// a question about emptiness.
    pub fn has_inflight(&self) -> bool {
        self.inflight
            .lock()
            .map(|list| !list.is_empty())
            .unwrap_or(false)
    }

    pub fn inflight(&self) -> Vec<InFlight> {
        self.inflight
            .lock()
            .map(|list| list.clone())
            .unwrap_or_default()
    }

    /// The first command that is still *running*, for the progress line.
    ///
    /// A failed entry stays in the list for `FAILURE_LINGER_MS` so a pane can
    /// draw a failed row, and that is longer than a message is retained — so
    /// describing one as progress does not merely mislabel it, it hides the
    /// error explaining the failure for the whole time that error is readable.
    /// The failure is reported through the message band instead.
    pub fn first_running(&self) -> Option<InFlight> {
        self.inflight
            .lock()
            .ok()?
            .iter()
            .find(|entry| entry.phase != Phase::Failed)
            .cloned()
    }

    /// Whether any command is in flight for a session.
    pub fn is_busy(&self, session: &str) -> bool {
        self.inflight
            .lock()
            .map(|list| {
                list.iter()
                    .any(|entry| entry.session == session && entry.phase != Phase::Failed)
            })
            .unwrap_or(false)
    }
}

impl Default for CommandBus {
    fn default() -> Self {
        Self::new()
    }
}
