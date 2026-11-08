//! Discovery: each backend's window list, surveyed on a worker and paced per
//! backend, which resolves a row with no pane of its own and tells a window
//! that is gone from one not looked for yet; and the mirror pass that reconciles
//! a shareable host's own rows.

use std::collections::HashMap;
use std::sync::mpsc::{Receiver, Sender};

use super::attach::ATTACH_RETRY_INTERVAL;
use super::Terminals;
use crate::kernel::snapshot::Snapshot;

/// The discovery side of [`Terminals`]: what each backend's last survey found,
/// when each may next be surveyed or mirrored, and what is in flight.
pub(super) struct Discovery {
    /// Panes found by window name, per backend: `tb-<name>` → pane id.
    ///
    /// The legacy resolution path: rows persisted before local spawns recorded
    /// their pane id (and psmux spawns from before they learned theirs) carry
    /// no id of their own, and are matched to a pane through this listing. Without it
    /// such a session is real, its agent is running, and the interface says
    /// "session has no pane yet" forever. It also validates a *carried* id —
    /// see [`Self::pane_is_stale`].
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
    /// a second off while the first is still out. [`Self::refresh_mirrors`]
    /// paces itself the same way.
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
    /// Hosts with a mirror pass in flight, and when each host may next be
    /// mirrored — absent until its first pass returns.
    pub(super) mirroring: std::collections::HashSet<String>,
    pub(super) mirror_due: HashMap<String, std::time::Instant>,
    pub(super) mirror_rx: (Sender<Mirrored>, Receiver<Mirrored>),
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
            mirroring: std::collections::HashSet::new(),
            mirror_due: HashMap::new(),
            mirror_rx: std::sync::mpsc::channel(),
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
/// records its real pane id, and the rows that do need a listing — mirrored
/// from a host's own database — describe sessions that already exist. Sharing
/// made remote rows discoverable at all (before it, they were skipped
/// outright); pacing them apart from the local cadence is what keeps that from
/// being two ssh commands a second for as long as one row is unattached.
pub(super) const REMOTE_DISCOVERY_INTERVAL: std::time::Duration = std::time::Duration::from_secs(5);

/// How often a shareable host's database is mirrored into local rows. One
/// multiplexed ssh round trip per host per interval, on a worker; the
/// pipelines mirror again right after anything they delegate, so this is the
/// cadence for changes *other* observers made.
pub(super) const MIRROR_INTERVAL: std::time::Duration = std::time::Duration::from_secs(10);

/// How long a host is left alone after a mirror pass that could not run.
///
/// [`crate::session_ops::host_cli::usable`] caches a `Yes` for the process
/// lifetime — a host's CLI does not change under us — so a host that was
/// reachable at its first probe and has since gone down keeps a usable
/// verdict, and every pass runs its ssh out to the connect timeout. Backing
/// the *pass* off is what bounds that; the verdict is a separate question
/// with its own retry.
pub(super) const MIRROR_RETRY_INTERVAL: std::time::Duration = std::time::Duration::from_secs(60);

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

/// One survey, as its worker runs it: [`discover_windows`], then — when it
/// reached the host — clearing that host's "not answering" verdict. A host
/// that answered is not down, and `session list` reads that verdict; done
/// here, on the worker, because it reads and may rewrite a file.
pub(super) fn survey(
    backend: &std::sync::Arc<dyn crate::backend::SessionBackend>,
    name: String,
    already_ready: bool,
) -> Discovered {
    let done = discover_windows(backend, name, already_ready);
    if done.readied && done.panes.is_ok() {
        crate::session_ops::host_cli::host_answered(&done.backend);
    }
    done
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

/// A mirror pass that came back from its worker.
pub(super) struct Mirrored {
    pub(super) backend: String,
    pub(super) report: Result<crate::session_ops::mirror::MirrorReport, String>,
}

/// One mirror pass for `host`, on the worker: its own database handle, the
/// CLI probe (cached across passes), and the reconcile.
pub(super) fn mirror_host(
    host: &crate::session::HostDef,
) -> Result<crate::session_ops::mirror::MirrorReport, String> {
    let cli = match crate::session_ops::host_cli::usable(host) {
        crate::session_ops::host_cli::Usable::Yes(cli) => cli,
        crate::session_ops::host_cli::Usable::No(reason) => return Err(reason),
    };
    let path = crate::paths::database_file().ok_or("could not resolve the database path")?;
    // `open_existing`: the TUI ran the schema pass at startup, and this worker
    // reopens every `MIRROR_INTERVAL` per host. `open` would replay the
    // migrations, re-issue the WAL pragma — which takes the write lock — and
    // run both retention prunes, every ten seconds, against the database the
    // loop is reading. Same rule `kernel::repos` follows.
    let db = crate::storage::Database::open_existing(&path)
        .map_err(|e| format!("open database: {e}"))?;
    crate::session_ops::mirror::mirror_host(&db, host, &cli).map_err(|e| e.to_string())
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

    /// Mirror every shareable host whose last pass is older than
    /// `MIRROR_INTERVAL` (or has never run), one worker each, whether or not a
    /// row lives on it: a host the observer has never used can hold sessions a
    /// peer created. Readying the host — the ssh connect, the CLI probe, a
    /// provisioning on first use — all happens on that worker, never here.
    pub(super) fn refresh_mirrors(&mut self) {
        let now = std::time::Instant::now();
        let due: Vec<(String, crate::session::HostDef)> = self
            .hosts
            .hosts
            .iter()
            .filter(|host| host.shareable())
            .map(|host| (host.backend_name(), host))
            .filter(|(name, _)| {
                !self.discovery.mirroring.contains(name)
                    && due_now(&self.discovery.mirror_due, name, now)
            })
            .map(|(name, host)| (name, host.clone()))
            .collect();
        for (name, host) in due {
            self.discovery.mirroring.insert(name.clone());
            let tx = self.discovery.mirror_rx.0.clone();
            let runtime = self.runtime.clone();
            std::thread::spawn(move || {
                let _guard = runtime.as_ref().map(|handle| handle.enter());
                let report = mirror_host(&host);
                let _ = tx.send(Mirrored {
                    backend: name,
                    report,
                });
            });
        }
    }

    /// Fold in finished mirror passes. The rows themselves arrive through the
    /// database — the snapshot's `data_version` poll — so all that is kept here
    /// is when each host was last mirrored, and a log line for a pass that
    /// changed something or could not run.
    pub(super) fn collect_mirrored(&mut self) {
        while let Ok(done) = self.discovery.mirror_rx.1.try_recv() {
            self.discovery.mirroring.remove(&done.backend);
            let interval = if done.report.is_ok() {
                MIRROR_INTERVAL
            } else {
                MIRROR_RETRY_INTERVAL
            };
            self.discovery
                .mirror_due
                .insert(done.backend.clone(), std::time::Instant::now() + interval);
            match done.report {
                Ok(report) if report.changed() => tracing::info!(
                    "mirrored {}: {} adopted, {} updated, {} deleted, {} restored, {} tombstoned",
                    done.backend,
                    report.adopted.len(),
                    report.updated.len(),
                    report.deleted.len(),
                    report.restored.len(),
                    report.tombstoned.len()
                ),
                Ok(_) => {}
                Err(e) => tracing::debug!("mirror of {} skipped: {e}", done.backend),
            }
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

    /// The pane of this session's own agent window, as the listing places it.
    ///
    /// `None` when there is no such window — the agent has not been launched
    /// yet, or has been killed — when the only window of that name is stamped
    /// for a different session, and when the name is ambiguous: keystrokes
    /// going to the wrong agent are worse than a pane that says why it is not
    /// attached.
    pub(super) fn pane_by_name(&self, row: &crate::kernel::snapshot::SessionRow) -> Option<String> {
        self.discovery
            .discovered
            .get(&self.key(&row.backend))?
            .agent_window(&row.id, &row.name)
            .pane()
    }

    /// Sessions whose agent is gone: the window they name does not exist on a
    /// backend we have successfully looked at.
    ///
    /// This is v1's restore-time question. When it finds no matching window v1
    /// **respawns** the agent (`respawn_stale_session`), which is how a session
    /// survives a reboot or a dead tmux server — restart thurbox and the agents
    /// come back. Answering it needs the survey to have actually happened:
    /// "we have not looked yet" and "we looked and it is gone" are the same
    /// silence otherwise, and relaunching on the first would spawn a second agent
    /// beside a perfectly good one.
    pub fn missing_agents(&self, snapshot: &Snapshot) -> Vec<String> {
        snapshot
            .sessions
            .iter()
            .filter(|row| !self.live.contains_key(&row.id))
            .filter(|row| !self.attach.attaching.contains_key(&row.id))
            // A session parked by `session stop` has no pane *because that was
            // asked for*. Everything below reads a missing pane as damage and
            // repairs it, which here would mean the interface silently undoing
            // the stop within a tick of it happening.
            .filter(|row| !row.stopped)
            // A listing that predates the row cannot speak for it: a session
            // created after the last one is simply not in it, and relaunching on
            // that silence kills the agent the spawn just started. So everything
            // below is read from a listing taken *since* the row appeared.
            .filter(|row| self.surveyed_since(row))
            // A row that names a live pane is not missing its agent — it is failing
            // to attach to one, which is a different problem with a different fix.
            // A row naming a pane the listing does not place in its own window *is*
            // missing it: that is the phantom id a restarted tmux server left
            // behind, and nothing else will clear it.
            .filter(
                |row| match row.backend_id.as_deref().filter(|id| !id.is_empty()) {
                    None => true,
                    Some(pane) => !self.pane_placed(row, pane),
                },
            )
            // Only a listing that positively says the window is *absent* may
            // relaunch. An ambiguous name — several windows, none of them
            // stamped — resolves to nothing, and respawning on that is how a
            // third agent appears beside the two that already collide.
            .filter(|row| self.liveness(row).permits_relaunch())
            .map(|row| row.id.clone())
            .collect()
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
