//! Discovery: each backend's window list, surveyed on a worker and paced per
//! backend, which tells a window that is gone from one not looked for yet.

use std::collections::HashMap;
use std::sync::mpsc::{Receiver, Sender};

use super::attach::ATTACH_RETRY_INTERVAL;
use super::Terminals;

/// The discovery side of [`Terminals`]: what each backend's last survey found,
/// when each may next be surveyed, and what is in flight.
pub(super) struct Discovery {
    /// Each backend's last window listing, which validates the pane id a row
    /// carries — see [`Terminals::pane_is_stale`].
    pub(super) discovered: HashMap<String, WindowPanes>,
    /// When each backend may next be surveyed. It is a round trip per backend
    /// — a local process, or an ssh command — so it is throttled per backend
    /// and only runs while some row actually needs resolving. A survey that
    /// came back empty-handed pushes its backend out to [`ATTACH_RETRY_INTERVAL`]
    /// so an unreachable host is probed on one schedule rather than two.
    ///
    /// Stamped when a survey *returns*, not when it is issued — so the interval
    /// separates one round trip from the next however long it took, and a slow
    /// one is not re-issued the instant it gives up. `discovering` is what holds
    /// a second off while the first is still out.
    pub(super) discovery_due: HashMap<String, std::time::Instant>,
    /// How many times each backend's window list has been read successfully.
    ///
    /// The difference between "this session's window is gone" and "we have not
    /// looked yet", which is the whole of whether a respawn is warranted: a host
    /// that cannot be reached must not have its sessions relaunched.
    ///
    /// A *count* rather than a flag, because "we have looked" is not the question
    /// — "we have looked since this row appeared" is. A row created after the last
    /// survey is invisible to it, and answering from the older listing reports a
    /// live session as having lost its agent (see [`Self::missing_agents`]).
    pub(super) surveys: HashMap<String, u64>,
    pub(super) unreachable: std::collections::HashSet<String>,
    /// The survey count each waiting row was first seen at.
    ///
    /// Its backend has to get *past* this number before the row's absence from the
    /// window list means anything.
    pub(super) waiting_since: HashMap<String, u64>,
    /// Backends whose window list is being read on a worker.
    pub(super) discovering: std::collections::HashSet<String>,
    pub(super) discovered_rx: (Sender<Discovered>, Receiver<Discovered>),
}

impl Discovery {
    pub(super) fn new() -> Self {
        Self {
            discovered: HashMap::new(),
            discovery_due: HashMap::new(),
            surveys: HashMap::new(),
            unreachable: std::collections::HashSet::new(),
            waiting_since: HashMap::new(),
            discovering: std::collections::HashSet::new(),
            discovered_rx: std::sync::mpsc::channel(),
        }
    }
}

/// Window names read off one backend, on their way back to the loop.
pub(super) struct Discovered {
    pub(super) backend: String,
    pub(super) readied: bool,
    pub(super) panes: crate::backend::BackendResult<WindowPanes>,
}

/// One backend's thurbox windows, indexed by the identity each one carries.
///
/// A window name is not unique — two sessions can be given the same one, and
/// sanitising collapses others together — so the index keys on the session id
/// stamped on the window (ADR-25) and keeps every namesake, which is what lets
/// ambiguity be *reported* rather than resolved by whichever tmux listed last.
pub(super) type WindowPanes = crate::backend::identity::WindowIndex;

/// How often a *local* backend's panes may be looked up by window name.
///
/// Discovery is one `list-windows` per backend — cheap, but not 60× a second.
/// This is also how fast a freshly spawned session finds its window, so it
/// stays tight for the server on this machine.
pub(super) const DISCOVERY_INTERVAL: std::time::Duration = std::time::Duration::from_millis(500);

/// The same, for a backend reached over ssh or `wsl.exe`.
///
/// A remote listing is an ssh round trip rather than a local process, and
/// nothing remote is waiting on it the way a local spawn is: a remote spawn
/// records its real pane id. Pacing remote rows apart from the local cadence
/// is what keeps a listing from being two ssh commands a second for as long as
/// one row is unattached.
pub(super) const REMOTE_DISCOVERY_INTERVAL: std::time::Duration = std::time::Duration::from_secs(5);

/// Whether a per-backend timer has come due. An entry is *when it may next
/// run*, so an absent one has never run and is due now.
pub(super) fn due_now(
    schedule: &HashMap<String, std::time::Instant>,
    name: &str,
    now: std::time::Instant,
) -> bool {
    !schedule.get(name).is_some_and(|due| *due > now)
}

/// How long to leave `backend` alone between window listings — the local
/// cadence, or the remote one for a backend whose listing travels over ssh.
pub(super) fn discovery_interval(backend: &str) -> std::time::Duration {
    if crate::session::Route::is_remote_key(backend) {
        REMOTE_DISCOVERY_INTERVAL
    } else {
        DISCOVERY_INTERVAL
    }
}

/// One survey, as its worker runs it.
pub(super) fn survey(
    backend: &std::sync::Arc<dyn crate::backend::SessionBackend>,
    name: String,
    already_ready: bool,
) -> Discovered {
    discover_windows(backend, name, already_ready)
}

/// One backend's window inventory, on the discovery worker.
///
/// Split out of [`Terminals::refresh_discovery`] so the throttle and the spawn
/// stay readable beside the two fallible host calls this makes.
pub(super) fn discover_windows(
    backend: &std::sync::Arc<dyn crate::backend::SessionBackend>,
    name: String,
    already_ready: bool,
) -> Discovered {
    // Same rule attach uses: ready once per backend, and only for one a session
    // actually lives on. An offline host fails here and is simply not discovered
    // this round.
    if !already_ready {
        if let Err(e) = backend.ensure_ready() {
            tracing::warn!("could not ready {name} to list its windows: {e:#}");
            return Discovered {
                backend: name,
                readied: false,
                panes: Err(e),
            };
        }
    }
    let panes = match backend.discover() {
        Ok(found) => Ok(WindowPanes::from_listing(found)),
        Err(e) => {
            tracing::warn!("could not list windows on {name}: {e:#}");
            Err(e)
        }
    };
    Discovered {
        backend: name,
        readied: true,
        panes,
    }
}

impl Terminals {
    /// Fold finished window listings back in.
    pub(super) fn collect_discovered(&mut self) {
        while let Ok(done) = self.discovery.discovered_rx.1.try_recv() {
            self.discovery.discovering.remove(&done.backend);
            // A survey that could not ready its backend or could not list it
            // learned nothing, and the next one a moment later would learn the
            // same nothing: hold it off as long as the attach it serves is held
            // off, so a down host costs one connect attempt per interval rather
            // than a continuous stream of them.
            let interval = if done.readied && done.panes.is_ok() {
                discovery_interval(&done.backend)
            } else {
                ATTACH_RETRY_INTERVAL
            };
            self.discovery
                .discovery_due
                .insert(done.backend.clone(), std::time::Instant::now() + interval);
            if done.readied {
                self.ready.borrow_mut().insert(done.backend.clone());
            }
            match done.panes {
                Ok(panes) => {
                    self.discovery.unreachable.remove(&done.backend);
                    *self
                        .discovery
                        .surveys
                        .entry(done.backend.clone())
                        .or_insert(0) += 1;
                    self.discovery.discovered.insert(done.backend, panes);
                }
                Err(error) => {
                    if matches!(error, crate::backend::BackendError::Unreachable(_)) {
                        self.discovery.unreachable.insert(done.backend.clone());
                    } else {
                        self.discovery.unreachable.remove(&done.backend);
                    }
                    self.discovery.discovered.remove(&done.backend);
                }
            }
        }
    }

    /// Re-read window names on each of `backends`, which the caller has already
    /// reduced to the ones that have a row waiting.
    ///
    /// Throttled **per backend**, at its own cadence: this is a `list-windows`
    /// per backend, and a session waiting for its window to appear would
    /// otherwise issue one per frame. One shared clock would pace a remote
    /// backend at the local rate, which is the whole cost this avoids — see
    /// [`REMOTE_DISCOVERY_INTERVAL`].
    pub(super) fn refresh_discovery(&mut self, backends: &[String]) {
        let now = std::time::Instant::now();
        for name in backends.iter().map(String::as_str) {
            if self.discovery.discovering.contains(name) {
                continue;
            }
            if !due_now(&self.discovery.discovery_due, name, now) {
                continue;
            }
            let Some(backend) = self.backend_for(name) else {
                continue;
            };
            let already_ready = self.backend_is_ready(name);
            let tx = self.discovery.discovered_rx.0.clone();
            let backend_name = name.to_string();
            self.discovery.discovering.insert(backend_name.clone());
            let runtime = self.runtime.clone();
            std::thread::spawn(move || {
                let _guard = runtime.as_ref().map(|handle| handle.enter());
                let _ = tx.send(survey(&backend, backend_name, already_ready));
            });
        }
    }

    /// Whether a listing has contradicted the pane id a row carries.
    ///
    /// The database's `backend_id` is a *hint*: tmux hands out fresh pane ids every
    /// time its server starts, so after a reboot every persisted id names a pane
    /// that is not there. v1 never hit this — its restore matched windows by name
    /// and respawned what it could not find, so a stored id could not outlive its
    /// server.
    ///
    /// Read as "does this session's own window hold that pane" rather than "does
    /// the pane exist", because a restarted server reissues ids from `%0`: `%1`
    /// after a reboot is somebody else's agent, and attaching to it would send this
    /// session's keystrokes there.
    pub(super) fn pane_is_stale(
        &self,
        row: &crate::kernel::snapshot::SessionRow,
        pane: &str,
    ) -> bool {
        self.surveyed_since(row) && !self.pane_placed(row, pane)
    }

    /// Whether this row's backend has been listed since the row appeared.
    ///
    /// The freshness half of every question asked of a listing, and the reason
    /// absence means anything at all: a listing that predates the row cannot speak
    /// for it. An unsurveyed backend — every remote one, which is never asked —
    /// therefore answers nothing.
    pub(super) fn surveyed_since(&self, row: &crate::kernel::snapshot::SessionRow) -> bool {
        let surveys = self
            .discovery
            .surveys
            .get(&self.key(&row.backend))
            .copied()
            .unwrap_or(0);
        let seen_at = self
            .discovery
            .waiting_since
            .get(&row.id)
            .copied()
            .unwrap_or(surveys);
        surveys > seen_at
    }

    /// Whether the latest listing puts `pane` in this session's own window.
    ///
    /// The *positive* reading, deliberately without the freshness gate: "a listing
    /// says this pane is yours" is an assertion, where "no listing mentions it" is
    /// only an absence — and absence is the half that has to know how old the
    /// listing is.
    pub(super) fn pane_placed(
        &self,
        row: &crate::kernel::snapshot::SessionRow,
        pane: &str,
    ) -> bool {
        self.discovery
            .discovered
            .get(&self.key(&row.backend))
            .is_some_and(|windows| windows.places_agent(&row.id, &row.name, pane))
    }

    pub(super) fn liveness(
        &self,
        row: &crate::kernel::snapshot::SessionRow,
    ) -> crate::backend::BackendLiveness {
        let backend = self.key(&row.backend);
        if self.discovery.unreachable.contains(&backend) {
            return crate::backend::BackendLiveness::Unreachable;
        }
        self.discovery
            .discovered
            .get(&backend)
            .map(|windows| windows.agent_liveness(&row.id, &row.name))
            .unwrap_or(crate::backend::BackendLiveness::Unknown)
    }
}
