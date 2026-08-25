//! Live agent terminals behind session-backed surfaces.
//!
//! This is the one place the kernel touches the session engine's *runtime*
//! rather than its stored state. A plugin places a `surface` naming a session;
//! this attaches to that session's real tmux pane, keeps a vt100 parser fed
//! from it, and paints the result with `tui_term` — the same path v1 uses in
//! `ui::terminal_view`.
//!
//! It reaches `crate::agent` through fully-qualified paths only, never `use`,
//! so every crossing into the side-effect layer is visible at its call site —
//! the rule `session_ops` and `cli` already follow.
//!
//! Three subsystems, one per file: the attach state machine here (plus the
//! shared [`Terminals`] state), plugin program panes in `programs`, and
//! link detection / selection reads / OSC 8 re-printing in [`links`].
//!
//! Two things are deliberately *not* here: spawning sessions and sending them
//! anything beyond keystrokes. Those are commands, and the command bus is a
//! later change. Attaching to what already exists needs none of it.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender};
use std::sync::Arc;

use ratatui::layout::{Position, Rect};
use ratatui::style::Style;
use ratatui::Frame;
use tui_term::widget::PseudoTerminal;

use super::paint::SurfaceProvider;
use super::snapshot::Snapshot;

pub mod links;
mod programs;

pub use links::{drawn_link_paints, paint_hyperlinks, HyperlinkPaint};
pub use programs::{validate_program_name, ProgramKey};

use programs::ProgramSlot;

/// Why one session has no live pane, and what was tried.
///
/// The attempt is part of the record on purpose: a failure keyed only by session
/// latches forever, so a pane that appears a moment later — which is the normal
/// case for a session this interface just created — never gets attached. Keyed
/// by the pane that failed, the next *different* candidate is tried.
struct Failure {
    /// The pane id that failed, or `None` when there was none to try.
    pane: Option<String>,
    message: String,
    /// When it failed, so the same attempt is made again eventually rather than
    /// never: a host that was down comes back, and nothing else would notice.
    at: std::time::Instant,
}

/// An attach that finished on a worker, on its way back to the loop.
struct Attached {
    session: String,
    /// The pane it tried, so the result can be matched against what the row
    /// still wants — a session can be deleted, or its pane change, mid-attach.
    pane: String,
    backend: String,
    /// Whether the backend's control-mode connection is now open. Reported back
    /// rather than assumed: the readying happens on the worker, and until it
    /// has, no second attach on that backend may start.
    readied: bool,
    /// Whether the pane was resolved by window name rather than taken from the
    /// row's own `backend_id` — a successful adoption is then worth persisting,
    /// so the legacy row stops depending on its (non-unique) name.
    via_name: bool,
    session_handle: Result<crate::agent::Session, String>,
}

/// Window names read off one backend, on their way back to the loop.
struct Discovered {
    backend: String,
    readied: bool,
    panes: Option<WindowPanes>,
}

/// The panes behind each window name on one backend.
///
/// A `Vec` because a name is not unique: two sessions can be given the same one,
/// and sanitising collapses others together. Keeping every match is what lets the
/// ambiguity be *reported* rather than resolved by whichever tmux listed last.
type WindowPanes = HashMap<String, Vec<String>>;

/// How often panes may be looked up by window name.
///
/// Discovery is one `list-windows` per backend — cheap, but not 60× a second.
const DISCOVERY_INTERVAL: std::time::Duration = std::time::Duration::from_millis(500);

/// How long the *same* failed attempt is left alone before it is made again.
///
/// v1 retries a down host on the same cadence (`REMOTE_RETRY_INTERVAL`). Without
/// it a session whose host was offline at startup stays dead for the life of the
/// process, because the candidate pane never changes.
const ATTACH_RETRY_INTERVAL: std::time::Duration = std::time::Duration::from_secs(20);

/// The key a plugin leaves in `store` to ask for terminal text.
///
/// A parameterised read, like the creation flow's repository questions: nobody
/// wants every agent's screen on every frame, so it is served only while
/// something is asking. Its value is what is being searched for, which is also
/// what makes "asking" and "having a query" the same state.
pub const WANT_CONTENT: &str = "want_content";

/// Most lines of one screen handed to a search.
///
/// v1 caps the same scan at 500 (`CONTENT_LINE_CAP`) — a bound that never binds,
/// since a screen is tens of rows. Kept at the same number so the two searches
/// cannot disagree about what they read.
const CONTENT_LINE_CAP: usize = 500;

/// The suffix that addresses a session's companion shell as its own surface.
const SHELL_SUFFIX: &str = "#shell";

/// A session we have attached to, plus the size we last told it about.
struct Live {
    session: crate::agent::Session,
    /// Last size pushed to the pane. A terminal that is not resized to its
    /// visible rect renders at the wrong width, so this is tracked per pane
    /// and pushed whenever the rect changes.
    size: Cell<(u16, u16)>,
    /// Where this session's surface was last painted, so a mouse position can
    /// be converted into a grid position.
    rect: Cell<Rect>,
    /// Whether the shell — rather than the agent — was the view painted into
    /// that rect. The two take turns in one rect, so a pointer landing in it
    /// has to be told which pane it is over.
    shell_visible: Cell<bool>,
}

impl Live {
    /// The parser of the pane currently *painted* into this session's rect.
    ///
    /// The agent and its companion shell take turns in one rect, so every reader
    /// that answers "what is on screen" — the text a copy takes, the links a
    /// click resolves, the screen a search scans — has to ask this rather than
    /// reach for `session.parser`. Reaching for it directly is what made copying
    /// out of a shell fail: a one-line selection read the agent's blank row and
    /// reported "nothing to copy", and a taller one copied the agent's text from
    /// under the shell the user was looking at.
    fn visible_parser(&self) -> &Arc<std::sync::Mutex<crate::agent::SessionParser>> {
        match (self.shell_visible.get(), &self.session.shell_pane) {
            (true, Some(shell)) => &shell.parser,
            _ => &self.session.parser,
        }
    }

    /// Send bytes to the pane currently painted into this session's rect.
    ///
    /// The write half of [`Self::visible_parser`], beside it so the two cannot
    /// come to disagree: a wheel tick answered by one pane must not be delivered
    /// to the other.
    fn send_visible_input(&self, bytes: Vec<u8>) -> anyhow::Result<()> {
        match (self.shell_visible.get(), &self.session.shell_pane) {
            (true, Some(shell)) => shell.send_input(bytes),
            _ => self.session.send_input(bytes),
        }
    }
}

/// One surface's extracted rows and the output stamp they were read at.
type CachedRows = (u64, std::rc::Rc<Vec<String>>);

/// Owns every live terminal, keyed by session id.
pub struct Terminals {
    backends: crate::agent::BackendRegistry,
    /// Extracted screen rows per surface, keyed on the output stamp they were
    /// read at.
    ///
    /// One walk of a vt100 grid builds a `String` per row from a per-cell
    /// `contents()` call — ~10,000 allocations for a 200×50 grid — and three
    /// readers want the same rows on the same frame: the link scan, the
    /// click-time URL resolve, and the OSC 8 repaint (which runs per painted
    /// frame for every session that ever printed a link). Sharing one
    /// extraction per output stamp turns that into a map hit for all but the
    /// first asker. `RefCell` because every reader takes `&self` on the UI
    /// thread.
    rows_cache: RefCell<HashMap<String, CachedRows>>,
    /// Kept beside the backends because a pane needs more than a connection: a
    /// remote session's launch directory is resolved against its `HostDef`.
    hosts: crate::session::HostRegistry,
    agents: crate::session::AgentRegistry,
    live: HashMap<String, Live>,
    /// Backends whose control-mode connection has been opened. Readying is
    /// blocking (an ssh connect for a remote host), so it happens once, lazily,
    /// and only for a backend a session actually lives on.
    ready: RefCell<std::collections::HashSet<String>>,
    /// Why a session could not be attached, so the pane can say so instead of
    /// looking empty. Kept per session and cleared on a successful attach.
    failed: HashMap<String, Failure>,
    /// Panes found by window name, per backend: `tb-<name>` → pane id.
    ///
    /// The legacy resolution path: rows persisted before local spawns recorded
    /// their pane id (and psmux spawns, which cannot report one) carry no id of
    /// their own, and are matched to a pane through this listing. Without it
    /// such a session is real, its agent is running, and the interface says
    /// "session has no pane yet" forever. It also validates a *carried* id —
    /// see [`Self::pane_is_stale`].
    discovered: HashMap<String, WindowPanes>,
    /// When discovery last ran. It is a tmux round trip per backend, so it is
    /// throttled and only runs while some row actually needs resolving.
    discovered_at: Option<std::time::Instant>,
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
    surveys: HashMap<String, u64>,
    /// The survey count each waiting row was first seen at.
    ///
    /// Its backend has to get *past* this number before the row's absence from the
    /// window list means anything.
    waiting_since: HashMap<String, u64>,
    /// Last-known activity/notification per session. Cached because the read is
    /// generation-gated: an unchanged session reports nothing, so the previous
    /// value has to be kept somewhere.
    meta: HashMap<String, AgentMeta>,
    /// Moves only when [`Self::meta`] actually writes or drops an entry.
    meta_version: u64,
    /// Moves whenever the set of attach failures does. Published on each
    /// session row, so it gates that group alongside `meta_version`.
    failed_version: u64,
    /// Attaches running on workers: session id → the backend it is on.
    ///
    /// Attaching is the one thing here that blocks for a *long* time — an ssh
    /// connect to a host that is down runs to its timeout, and the history
    /// capture is a round trip per pane. On the render thread that is the whole
    /// interface frozen before its first paint, so it happens on a worker and
    /// the result is collected here.
    attaching: HashMap<String, String>,
    attached: (Sender<Attached>, Receiver<Attached>),
    /// Panes adopted by window-name resolution, waiting for the loop to persist
    /// their id onto the row ([`Self::drain_adopted_panes`]).
    adopted: Vec<(String, String)>,
    /// Backends whose window list is being read on a worker.
    discovering: std::collections::HashSet<String>,
    discovered_rx: (Sender<Discovered>, Receiver<Discovered>),
    /// The runtime the interface runs on, so a worker can enter it.
    ///
    /// Adopting a pane wires its reader and writer as tokio tasks, which need a
    /// reactor in scope — a bare thread has none, and the adopt panics there.
    /// Captured once rather than looked up per attach so the failure mode is
    /// "no runtime at construction", not a surprise mid-session.
    runtime: Option<tokio::runtime::Handle>,
    /// Programs plugins asked for, keyed by [`ProgramKey`].
    ///
    /// Here beside `live` rather than in a struct of their own, because a pane
    /// needs everything this struct already holds — the backend registry, the
    /// paint seam, the redraw stamp, the rect memo — and `SurfaceProvider` has
    /// one implementor by design, so a second provider is not on the table.
    programs: HashMap<ProgramKey, ProgramSlot>,
}

impl Terminals {
    /// Build the backend registry the same way the v1 binary does: the local
    /// multiplexer plus every configured or discovered host. How that set is
    /// assembled — and why nothing is readied here — is the registry's own
    /// knowledge (`BackendRegistry::from_configured_hosts`), not the kernel's.
    pub fn new() -> Self {
        let (backends, hosts, _warnings) = crate::agent::BackendRegistry::from_configured_hosts();

        Self {
            backends,
            hosts,
            agents: crate::agent::agent_config::load_or_seed(),
            live: HashMap::new(),
            ready: RefCell::new(std::collections::HashSet::new()),
            failed: HashMap::new(),
            discovered: HashMap::new(),
            discovered_at: None,
            surveys: HashMap::new(),
            waiting_since: HashMap::new(),
            meta: HashMap::new(),
            meta_version: 0,
            failed_version: 0,
            attaching: HashMap::new(),
            attached: std::sync::mpsc::channel(),
            adopted: Vec::new(),
            discovering: std::collections::HashSet::new(),
            discovered_rx: std::sync::mpsc::channel(),
            runtime: tokio::runtime::Handle::try_current().ok(),
            programs: HashMap::new(),
            rows_cache: RefCell::new(HashMap::new()),
        }
    }

    /// Attach to sessions that appeared, and drop those that went away.
    ///
    /// Called once per frame from the event loop. An attach is attempted once per
    /// (session, pane) — so a pane that cannot be adopted does not retry 20×/s,
    /// and a session whose pane only *becomes* known later is still picked up.
    ///
    /// A row with no pane id of its own is resolved by **window name** — the
    /// legacy path for rows persisted before local spawns recorded their pane id
    /// (and for psmux, which cannot report one). A successful name-resolved
    /// adoption is queued for the loop to persist, so it happens once per row.
    pub fn sync(&mut self, snapshot: &Snapshot, rows: u16, cols: u16) {
        self.collect_discovered();
        self.collect_attached(rows, cols);
        self.drop_lost_panes(snapshot);
        // A surface that lost its parser takes its cached rows with it.
        self.rows_cache
            .borrow_mut()
            .retain(|surface, _| self.surface_parser(surface).is_some());

        // Only pay for discovery while something needs it, and never for a remote
        // row: a remote spawn drives control mode and records the real pane id, so
        // a remote row is not something a window name can fix.
        //
        // A row that already names a pane is surveyed too — a persisted pane id is
        // a hint rather than a fact, see [`Self::pane_is_stale`] — which costs
        // nothing extra: one listing per backend, throttled by
        // `DISCOVERY_INTERVAL`.
        let unresolved: Vec<(&str, &str)> = snapshot
            .sessions
            .iter()
            .filter(|row| !self.live.contains_key(&row.id) && !self.attaching.contains_key(&row.id))
            .map(|row| (row.id.as_str(), row.backend.as_str()))
            .filter(|(_, backend)| !crate::session::is_remote_backend(backend))
            .collect();

        // Stamp each row with where its backend's survey count stood when it first
        // showed up unresolved. A row that has stopped waiting forgets its stamp,
        // so a session that is deleted and restored is judged afresh.
        let still_waiting: std::collections::HashSet<&str> =
            unresolved.iter().map(|(id, _)| *id).collect();
        self.waiting_since
            .retain(|id, _| still_waiting.contains(id.as_str()));
        for (id, backend) in &unresolved {
            let seen_at = self.surveys.get(*backend).copied().unwrap_or(0);
            self.waiting_since
                .entry((*id).to_string())
                .or_insert(seen_at);
        }

        let mut waiting: Vec<String> = unresolved
            .iter()
            .map(|(_, backend)| (*backend).to_string())
            .collect();
        waiting.sort_unstable();
        waiting.dedup();
        if !waiting.is_empty() {
            self.refresh_discovery(&waiting);
        }

        for row in &snapshot.sessions {
            if self.live.contains_key(&row.id) || self.attaching.contains_key(&row.id) {
                continue;
            }
            // The row's own pane id, unless a listing contradicts it: a
            // contradicted one is worth less than the window name that produced
            // it, and falling through to `None` here is also what lets
            // [`Self::missing_agents`] relaunch a session whose window is gone for
            // good.
            let (candidate, via_name) = match row.backend_id.clone() {
                Some(id) if !self.pane_is_stale(row, &id) => (Some(id), false),
                _ => (self.pane_by_name(&row.backend, &row.name), true),
            };
            // The same attempt would fail the same way; a different one is worth
            // making.
            if self.failed.get(&row.id).is_some_and(|failure| {
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
            if !self.backend_is_ready(&row.backend) && self.opening(&row.backend) {
                continue;
            }
            self.start_attach(row, backend_id, via_name, rows, cols);
        }

        // Anything no longer in the snapshot has been deleted; dropping the
        // Session detaches it without touching the pane. An attach still in
        // flight is left to finish and discarded on arrival — a worker cannot
        // be cancelled, and its result is matched against the live rows.
        let present: std::collections::HashSet<&str> = snapshot
            .sessions
            .iter()
            .map(|row| row.id.as_str())
            .collect();
        self.live.retain(|id, _| present.contains(id.as_str()));
        let failures = self.failed.len();
        self.failed.retain(|id, _| present.contains(id.as_str()));
        if self.failed.len() != failures {
            self.mark_failures_changed();
        }
    }

    /// Let go of a session whose pane died, so it is re-attached rather than
    /// painting a frozen last screen forever.
    ///
    /// `has_exited` is set when a session's output **stream** ends, which is a
    /// narrower signal than it looks. Control mode carries every pane on a
    /// backend down one connection, so this fires when the *connection* goes —
    /// a host or ssh dropping, a local tmux server dying — and **not** when a
    /// single pane is killed. A restart therefore cannot be caught here, however
    /// tempting it looks: it says so itself, through [`Terminals::forget`].
    ///
    /// Also let go when the row's pane id has *moved*: the interface is holding a
    /// pane the session no longer claims. That covers a remote restart, which
    /// records the new pane id; a local one has none to record and forgets
    /// instead.
    fn drop_lost_panes(&mut self, snapshot: &Snapshot) {
        let lost: Vec<(String, Option<String>, bool)> = snapshot
            .sessions
            .iter()
            .filter_map(|row| {
                let live = self.live.get(&row.id)?;
                let remote = crate::session::is_remote_backend(&row.backend);
                let moved = row.backend_id.as_deref().is_some_and(|id| {
                    !id.is_empty()
                        && id != live.session.backend_id()
                        // Locally the only way a row's id can differ from the pane
                        // being held is that the id is a phantom: a local restart
                        // records no id at all, it forgets instead. So a local id
                        // may evict a live pane only when a listing actually places
                        // it in this session's window — otherwise an id left over
                        // from a previous tmux server drops the pane just resolved
                        // by name, on every frame, forever.
                        && (remote || self.pane_placed(row, id))
                });
                (live.session.has_exited() || moved)
                    .then(|| (row.id.clone(), row.backend_id.clone(), remote))
            })
            .collect();
        for (id, pane, remote) in lost {
            self.live.remove(&id);
            if remote {
                // The backend has to be readied again before the next attach can
                // adopt anything: the connection this session died with is the
                // one every other session on that host shares.
                self.ready.borrow_mut().clear();
                self.fail(&id, pane, "host unreachable".to_string());
            } else {
                // Locally the pane is simply gone. Recorded against the pane that
                // died, so the retry rule treats a *different* candidate — the
                // one a restart just created — as worth trying at once.
                self.fail(&id, pane, "session has no pane yet".to_string());
            }
        }
    }

    /// Whether a backend's control-mode connection is already open.
    fn backend_is_ready(&self, backend: &str) -> bool {
        self.ready.borrow().contains(backend)
    }

    /// Whether some worker is already opening this backend's connection.
    fn opening(&self, backend: &str) -> bool {
        self.discovering.contains(backend) || self.attaching.values().any(|name| name == backend)
    }

    /// Hand one session's attach to a worker.
    ///
    /// Everything the worker needs is cloned across: the backend and the agent
    /// provider are both behind an `Arc`, and the resulting `Session` owns its
    /// own reader/writer threads, so nothing here is borrowed from the loop.
    fn start_attach(
        &mut self,
        row: &super::snapshot::SessionRow,
        backend_id: String,
        via_name: bool,
        rows: u16,
        cols: u16,
    ) {
        let Some(backend) = self.backends.get(&row.backend).cloned() else {
            self.fail(
                &row.id,
                Some(backend_id),
                format!("no backend named {}", row.backend),
            );
            return;
        };
        // Only consulted when relaunching, but adopt wants one.
        let Some(def) = self
            .agents
            .get(&row.agent)
            .or_else(|| self.agents.default_agent())
            .cloned()
        else {
            self.fail(
                &row.id,
                Some(backend_id),
                format!("no agent definition for {}", row.agent),
            );
            return;
        };

        let already_ready = self.backend_is_ready(&row.backend);
        let tx = self.attached.0.clone();
        let session = row.id.clone();
        let name = row.name.clone();
        let backend_name = row.backend.clone();
        let pane = backend_id.clone();
        self.attaching.insert(session.clone(), backend_name.clone());
        let runtime = self.runtime.clone();
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
            let result = session_handle.and_then(|()| {
                let seed = backend.capture_history(&pane).ok();
                let provider: Arc<dyn crate::agent::AgentProvider> =
                    Arc::new(crate::agent::GenericProvider::new(def));
                crate::agent::Session::adopt(
                    name,
                    rows,
                    cols,
                    &pane,
                    &backend,
                    &provider,
                    HashMap::new(),
                    seed,
                )
                .map_err(|e| e.to_string())
            });
            let _ = tx.send(Attached {
                session,
                pane,
                backend: backend_name,
                readied,
                via_name,
                session_handle: result,
            });
        });
    }

    /// Fold finished attaches back into the live set.
    fn collect_attached(&mut self, rows: u16, cols: u16) {
        while let Ok(done) = self.attached.1.try_recv() {
            self.attaching.remove(&done.session);
            if done.readied {
                self.ready.borrow_mut().insert(done.backend);
            }
            match done.session_handle {
                Ok(session) => {
                    // A pane that had to be resolved by window name is worth
                    // persisting: names are not unique, so the row must not
                    // depend on one past this first adoption. The loop drains
                    // these and writes the id back (`drain_adopted_panes`).
                    if done.via_name {
                        self.adopted.push((done.session.clone(), done.pane));
                    }
                    self.live.insert(
                        done.session.clone(),
                        Live {
                            session,
                            size: Cell::new((rows, cols)),
                            rect: Cell::new(Rect::default()),
                            shell_visible: Cell::new(false),
                        },
                    );
                    if self.failed.remove(&done.session).is_some() {
                        self.mark_failures_changed();
                    }
                }
                Err(e) => self.fail(&done.session, Some(done.pane), e),
            }
        }
    }

    /// Fold finished window listings back in.
    fn collect_discovered(&mut self) {
        while let Ok(done) = self.discovered_rx.1.try_recv() {
            self.discovering.remove(&done.backend);
            if done.readied {
                self.ready.borrow_mut().insert(done.backend.clone());
            }
            if let Some(panes) = done.panes {
                *self.surveys.entry(done.backend.clone()).or_insert(0) += 1;
                self.discovered.insert(done.backend, panes);
            }
        }
    }

    /// Take the panes adopted by window-name resolution since the last call,
    /// as `(session id, pane id)` pairs for the loop to persist. Legacy rows
    /// (spawned before local spawns recorded their pane id) migrate this way:
    /// one successful adoption, and the row stops depending on its name.
    pub fn drain_adopted_panes(&mut self) -> Vec<(String, String)> {
        std::mem::take(&mut self.adopted)
    }

    /// Let go of a session's terminal, so the next sync attaches afresh.
    ///
    /// Told, not inferred. Inference does not work here: `has_exited` is set when
    /// a session's output *stream* ends, and tmux control mode carries every pane
    /// on a backend down one connection — killing a pane leaves that stream wide
    /// open, so a restarted session looked perfectly alive while showing a pane
    /// that no longer existed. The restart knows; this is how it says so.
    ///
    /// The recorded failure is cleared too, or the next attach would be held off
    /// by the retry interval and the session would sit frozen for another 20s
    /// after the pane it needs is already there.
    pub fn forget(&mut self, session: &str) {
        self.live.remove(session);
        if self.failed.remove(session).is_some() {
            self.mark_failures_changed();
        }
    }

    /// Record why a session has no pane, and what was tried.
    fn fail(&mut self, session: &str, pane: Option<String>, message: String) {
        self.mark_failures_changed();
        self.failed.insert(
            session.to_string(),
            Failure {
                pane,
                message,
                at: std::time::Instant::now(),
            },
        );
    }

    /// Re-read window names on each of `backends`, which the caller has already
    /// reduced to the local ones that have a row waiting.
    ///
    /// Throttled: this is a `list-windows` per backend, and a session waiting for
    /// its window to appear would otherwise issue one per frame.
    fn refresh_discovery(&mut self, backends: &[String]) {
        if self
            .discovered_at
            .is_some_and(|at| at.elapsed() < DISCOVERY_INTERVAL)
        {
            return;
        }
        self.discovered_at = Some(std::time::Instant::now());

        for name in backends.iter().map(String::as_str) {
            if self.discovering.contains(name) {
                continue;
            }
            let Some(backend) = self.backends.get(name).cloned() else {
                continue;
            };
            let already_ready = self.backend_is_ready(name);
            let tx = self.discovered_rx.0.clone();
            let backend_name = name.to_string();
            self.discovering.insert(backend_name.clone());
            let runtime = self.runtime.clone();
            std::thread::spawn(move || {
                let _guard = runtime.as_ref().map(|handle| handle.enter());
                let _ = tx.send(discover_windows(&backend, backend_name, already_ready));
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
    fn pane_is_stale(&self, row: &super::snapshot::SessionRow, pane: &str) -> bool {
        self.surveyed_since(row) && !self.pane_placed(row, pane)
    }

    /// Whether this row's backend has been listed since the row appeared.
    ///
    /// The freshness half of every question asked of a listing, and the reason
    /// absence means anything at all: a listing that predates the row cannot speak
    /// for it. An unsurveyed backend — every remote one, which is never asked —
    /// therefore answers nothing.
    fn surveyed_since(&self, row: &super::snapshot::SessionRow) -> bool {
        let surveys = self.surveys.get(&row.backend).copied().unwrap_or(0);
        let seen_at = self.waiting_since.get(&row.id).copied().unwrap_or(surveys);
        surveys > seen_at
    }

    /// Whether the latest listing puts `pane` in the window this session's name
    /// produces.
    ///
    /// The *positive* reading, deliberately without the freshness gate: "a listing
    /// says this pane is yours" is an assertion, where "no listing mentions it" is
    /// only an absence — and absence is the half that has to know how old the
    /// listing is.
    fn pane_placed(&self, row: &super::snapshot::SessionRow, pane: &str) -> bool {
        let window = crate::agent::tmux::agent_window_name(&row.name);
        self.discovered
            .get(&row.backend)
            .and_then(|windows| windows.get(&window))
            .is_some_and(|panes| panes.iter().any(|known| known == pane))
    }

    /// The pane of the window a session's name would have produced.
    ///
    /// `None` when there is no such window — the session's agent has not been
    /// launched yet, or has been killed — and also when the name is ambiguous,
    /// because keystrokes going to the wrong agent is worse than a pane that
    /// says why it is not attached.
    fn pane_by_name(&self, backend: &str, session_name: &str) -> Option<String> {
        let window = crate::agent::tmux::agent_window_name(session_name);
        let panes = self.discovered.get(backend)?.get(&window)?;
        match panes.len() {
            1 => panes.first().cloned(),
            _ => None,
        }
    }

    /// Forward keystrokes to a session's pane.
    ///
    /// Returns false when the session is not attached, so the caller can leave
    /// the key for something else rather than swallowing it silently.
    #[must_use = "the caller decides whether the keystroke was consumed from this"]
    pub fn send(&self, session: &str, bytes: Vec<u8>) -> bool {
        // A shell surface is addressed `<id>#shell`, the same spelling
        // `render_session` resolves — so the view you are looking at is the pane
        // your keystrokes reach. Without this leg the shell drew but could not
        // be typed into, which only became visible once it stopped being a pane
        // of its own and became a tab of the terminal.
        if let Some(id) = session.strip_suffix(SHELL_SUFFIX) {
            return match self
                .live
                .get(id)
                .and_then(|live| live.session.shell_pane.as_ref())
            {
                Some(shell) => shell.send_input(bytes).is_ok(),
                None => false,
            };
        }
        match self.live.get(session) {
            Some(live) => live.session.send_input(bytes).is_ok(),
            None => false,
        }
    }

    /// The live entry a surface key names, and the parser it is showing.
    ///
    /// One resolver for both spellings a surface arrives as: an explicit
    /// `<id>#shell` asks for the shell, and a bare id asks for whichever pane is
    /// painted into that session's rect ([`Live::visible_parser`]). Every reader
    /// of a grid goes through it, so a copy, a click on a link and a content
    /// search cannot disagree about which of the two panes the user is looking
    /// at.
    fn surface_parser(
        &self,
        surface: &str,
    ) -> Option<(&Live, &Arc<std::sync::Mutex<crate::agent::SessionParser>>)> {
        if let Some(id) = surface.strip_suffix(SHELL_SUFFIX) {
            let live = self.live.get(id)?;
            return Some((live, &live.session.shell_pane.as_ref()?.parser));
        }
        let live = self.live.get(surface)?;
        Some((live, live.visible_parser()))
    }

    /// The visible contents of a session's terminal, as text.
    ///
    /// Visible is meant literally: a session showing its companion shell reports
    /// the shell's screen, because that is the one the user is reading and the one
    /// a copy or a search is about.
    ///
    /// Read here rather than in a worker because the parser lives beside a
    /// `!Send` VM — which is the compile-time guarantee working, not an
    /// inconvenience.
    pub fn visible_text(&self, session: &str) -> Option<String> {
        let (_, parser) = self.surface_parser(session)?;
        let parser = parser.lock().ok()?;
        let screen = parser.screen();
        let (rows, cols) = screen.size();
        let mut out = String::new();
        for row in 0..rows {
            let line = screen.contents_between(row, 0, row, cols);
            out.push_str(line.trim_end());
            out.push('\n');
        }
        Some(out)
    }

    /// What each named session's terminal is showing, for a search to scan.
    ///
    /// Only sessions with a live pane appear: an unreachable host or a session
    /// whose pane has not been adopted has no parser to read, so it contributes
    /// nothing. v1's content search has the same limit for the same reason.
    ///
    /// Read on this thread rather than a worker because the parsers live beside
    /// the `!Send` Lua VM — the compile-time guarantee working, not an
    /// inconvenience. It is the same walk `links` already makes on every
    /// publish, so it costs what that costs.
    pub fn screens(&self, sessions: &[String]) -> HashMap<String, String> {
        sessions
            .iter()
            .filter_map(|id| {
                let text = self.visible_text(id)?;
                let capped: String = text
                    .lines()
                    .rev()
                    .take(CONTENT_LINE_CAP)
                    .collect::<Vec<_>>()
                    .into_iter()
                    .rev()
                    .collect::<Vec<_>>()
                    .join("\n");
                Some((id.clone(), capped))
            })
            .collect()
    }

    /// Where a session's processes are launched — v1's
    /// `App::session_process_cwd_existing`, which is what its shell opens in.
    ///
    /// For one repository that is the repository. For several it is the symlink
    /// workspace the agent itself is running in, so switching to the shell lands
    /// you where the agent is rather than in whichever member happens to be
    /// primary. This is the *non-building* variant: the workspace already exists
    /// (the agent is in it), and the ensure-style rebuild would `rm -rf` its cwd
    /// out from under it.
    ///
    /// Falls back to the recorded cwd whenever the workspace cannot be named,
    /// because a shell in the primary repository beats a shell wherever the
    /// multiplexer happened to be.
    pub fn launch_cwd(&self, row: &super::snapshot::SessionRow) -> Option<PathBuf> {
        crate::session_ops::spawn::existing_launch_cwd(
            row.agent_session_id.as_deref(),
            row.cwd.as_deref(),
            row.member_dirs.len(),
            row.remote_host.as_deref().and_then(|n| self.hosts.get(n)),
        )
    }

    /// Open a shell beside the agent, in `cwd` — see [`Terminals::launch_cwd`].
    ///
    /// Idempotent: `ensure_shell_pane` returns early when one exists, so a
    /// plugin can command this every time you press the key.
    pub fn open_shell(
        &mut self,
        session: &str,
        rows: u16,
        cols: u16,
        cwd: Option<&std::path::Path>,
    ) -> Result<(), String> {
        let live = self
            .live
            .get_mut(session)
            .ok_or("this session has no live pane to attach a shell to")?;
        // Born at the size of the rect it will be painted into, when that is
        // known. The caller only has the whole terminal's size, and the
        // render-time resize below cannot correct it: the agent view shares
        // this memo and has already set it, so the size looks settled while the
        // new pane is a screen wide.
        let rect = live.rect.get();
        let (rows, cols) = if rect.width > 0 && rect.height > 0 {
            (rect.height, rect.width)
        } else {
            (rows, cols)
        };
        // `Session::adopt` builds a fresh `SessionInfo`, so its own `cwd` is
        // always `None` here — v2 attaches rather than restoring the persisted
        // row. Without passing one the shell inherits the multiplexer's
        // directory, which is wherever thurbox was started.
        live.session
            .ensure_shell_pane(rows, cols, cwd)
            .map_err(|e| e.to_string())
    }

    /// The pane id of a session's companion shell, once it has one.
    ///
    /// Read by the loop so the id can be **persisted**: a shell lives in its own
    /// tmux window (`tbsh-…`), so it outlives the interface — but the fact that
    /// this session had one lives only in the `Session` object, which does not.
    /// Without persisting it, restarting thurbox (or a session) forgets the shell
    /// you had open and leaves its window orphaned, and the next `shell` key
    /// spawns a second one beside it. v1 keeps `shell_backend_id` on the row for
    /// exactly this reason.
    pub fn shell_pane_id(&self, session: &str) -> Option<String> {
        self.live
            .get(session)?
            .session
            .shell_pane
            .as_ref()
            .map(|pane| pane.backend_id.clone())
    }

    /// Re-attach a session to a shell window it already had.
    ///
    /// Mirrors v1's `readopt_shell_pane`, including its guard: a pane id that no
    /// longer names a live pane is ignored rather than adopted, so a shell whose
    /// window was closed outside thurbox does not come back as a dead surface.
    pub fn readopt_shell(&mut self, session: &str, pane: &str, rows: u16, cols: u16) -> bool {
        let Some(live) = self.live.get_mut(session) else {
            return false;
        };
        if live.session.shell_pane.is_some() {
            return true;
        }
        match live.session.adopt_shell_pane(pane, rows, cols) {
            Ok(()) => true,
            Err(e) => {
                tracing::debug!("could not re-adopt shell pane {pane}: {e:#}");
                false
            }
        }
    }

    /// Whether a session has a shell pane open.
    pub fn has_shell(&self, session: &str) -> bool {
        self.live
            .get(session)
            .is_some_and(|live| live.session.shell_pane.is_some())
    }

    /// Forget where every surface was painted, at the start of a frame.
    ///
    /// A rect is recorded while painting and was never taken back, so a session
    /// whose surface stopped being drawn — a pane removed, an alternate no longer
    /// selected, a plugin that changed its mind — kept the rect it last held, and a
    /// click or wheel tick landing there was routed to a session that is not on
    /// screen. Cleared each frame, only what actually painted can be hit.
    pub fn forget_rects(&self) {
        for live in self.live.values() {
            live.rect.set(Rect::default());
        }
        for slot in self.programs.values() {
            slot.rect.set(Rect::default());
        }
    }

    /// Where a session's surface was last painted, so a click can be mapped
    /// into its grid.
    ///
    /// `None` once the surface is not on screen: an empty rect cannot contain a
    /// pointer, so a stale one would only ever be a wrong answer.
    pub fn last_rect(&self, session: &str) -> Option<Rect> {
        // A program surface keeps its rect on its own slot, so the one accessor
        // answers for both kinds — callers ask about "a surface", not about a
        // session.
        if let Some(key) = self.program_key(session) {
            return self
                .programs
                .get(key)
                .map(|slot| slot.rect.get())
                .filter(|rect| rect.width > 0 && rect.height > 0);
        }
        self.live
            .get(session)
            .map(|live| live.rect.get())
            .filter(|rect| rect.width > 0 && rect.height > 0)
    }

    /// Hand a wheel tick to whatever terminal is under `(x, y)`, if that
    /// terminal wants it.
    ///
    /// Modern agent TUIs (Claude Code, vim, htop) turn on mouse tracking and
    /// scroll themselves; their alternate screen keeps no vt100 scrollback, so
    /// scrolling locally would be a silent no-op. Forwarding is therefore tried
    /// first and `false` means "nobody here wants it" — the caller then scrolls
    /// the pane itself. v1 draws the same line in `try_forward_wheel_to_pty`.
    pub fn forward_wheel(&self, x: u16, y: u16, up: bool) -> bool {
        let position = Position::new(x, y);
        let Some((_, live)) = self
            .live
            .iter()
            .find(|(_, live)| live.rect.get().contains(position))
        else {
            return false;
        };
        let parser = live.visible_parser();
        // Only the SGR encoding is emitted below, so a pane asking for one of
        // the older ones is left to the local fallback rather than sent bytes it
        // would misread.
        let wants = match parser.lock() {
            Ok(parser) => {
                let screen = parser.screen();
                screen.mouse_protocol_mode() != vt100::MouseProtocolMode::None
                    && screen.mouse_protocol_encoding() == vt100::MouseProtocolEncoding::Sgr
            }
            Err(_) => false,
        };
        if !wants {
            return false;
        }

        // The rect is the surface's own content area — the plugin's frame is
        // outside it — so the offset needs no border adjustment. PTY cells are
        // 1-based.
        let rect = live.rect.get();
        let col = u32::from(x - rect.x) + 1;
        let row = u32::from(y - rect.y) + 1;
        // Xterm wheel buttons: 64 up, 65 down. SGR press is `CSI < Cb ; Cx ; Cy M`.
        let button = if up { 64 } else { 65 };
        let bytes = format!("\x1b[<{button};{col};{row}M").into_bytes();
        match live.send_visible_input(bytes) {
            Ok(()) => true,
            Err(e) => {
                tracing::warn!("forwarding a wheel tick to the pty failed: {e}");
                false
            }
        }
    }

    /// Why a session has no live pane, if it failed to attach.
    ///
    /// Surfaced so the pane can explain itself rather than showing an empty
    /// box — "not attached" with no reason is the least useful thing a
    /// terminal can say.
    pub fn failure(&self, session: &str) -> Option<&str> {
        self.failed
            .get(session)
            .map(|failure| failure.message.as_str())
    }

    /// Every attach failure, keyed by session id.
    ///
    /// Owned rather than borrowed because the record carries what was *tried* as
    /// well as the message, and only the message is a plugin's business.
    pub fn failures(&self) -> HashMap<String, String> {
        self.failed
            .iter()
            .map(|(session, failure)| (session.clone(), failure.message.clone()))
            .collect()
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
            .filter(|row| !self.attaching.contains_key(&row.id))
            // A listing that predates the row cannot speak for it: a session
            // created after the last one is simply not in it, and relaunching on
            // that silence kills the agent the spawn just started. So everything
            // below is read from a listing taken *since* the row appeared.
            .filter(|row| self.surveyed_since(row))
            // A row that names a live pane is not missing its agent — it is failing
            // to attach to one, which is a different problem with a different fix.
            // A row naming a pane the listing does not place in its window *is*
            // missing it: that is the phantom id a restarted tmux server left
            // behind, and nothing else will clear it.
            .filter(
                |row| match row.backend_id.as_deref().filter(|id| !id.is_empty()) {
                    None => true,
                    Some(pane) => !self.pane_placed(row, pane),
                },
            )
            .filter(|row| self.pane_by_name(&row.backend, &row.name).is_none())
            .map(|row| row.id.clone())
            .collect()
    }

    /// Drain every backend's queued remote hook reports.
    ///
    /// The events are `(backend, pane id, state)` — pane ids collide across
    /// hosts, so the backend is part of the identity, not decoration. Only the
    /// tmux backend produces any; the rest return nothing.
    pub fn drain_hook_events(&self) -> Vec<(String, String, String)> {
        self.backends
            .all_backends()
            .flat_map(|backend| {
                let name = backend.name().to_string();
                backend
                    .take_hook_state_events()
                    .into_iter()
                    .map(move |(pane, state)| (name.clone(), pane, state))
            })
            .collect()
    }

    /// Whether a session currently has a live pane.
    pub fn is_attached(&self, session: &str) -> bool {
        self.live.contains_key(session)
    }

    /// A session's backend plus pane id, for work that must run off this thread.
    ///
    /// Resolving a pane's root pid is a control-mode round trip, so the metrics
    /// sampler needs the handle rather than the answer — the `Arc` is what lets
    /// a worker ask without borrowing anything the loop owns.
    pub fn backend_handle(
        &self,
        session: &str,
    ) -> Option<(Arc<dyn crate::agent::SessionBackend>, String)> {
        self.live
            .get(session)
            .map(|live| live.session.backend_handle())
    }

    /// When the pane behind a surface name last produced output, as epoch
    /// milliseconds. `None` when nothing is attached there.
    ///
    /// This is the redraw signal for a *surface*: its cells live outside the
    /// node tree, so tree equality cannot tell whether it changed. Comparing
    /// this stamp against the one a renderer last painted at can — and it is a
    /// single atomic load, which is why v1 reads the same field rather than
    /// diffing screens.
    ///
    /// Accepts the `<id>#shell` spelling, so the view you are looking at is the
    /// pane whose output is checked.
    pub fn output_stamp(&self, surface: &str) -> Option<u64> {
        if let Some(key) = self.program_key(surface) {
            return self
                .programs
                .get(key)
                .map(|slot| slot.pane.last_output_at());
        }
        if let Some(id) = surface.strip_suffix(SHELL_SUFFIX) {
            return self
                .live
                .get(id)
                .and_then(|live| live.session.shell_pane.as_ref())
                .map(|shell| shell.last_output_at());
        }
        self.live
            .get(surface)
            .map(|live| live.session.last_output_at())
    }

    /// A cheap signature of every live pane's last output.
    ///
    /// **The redraw signal for the loop**, and the reason it exists rather than
    /// the per-surface stamp below: a frame is only painted when something marked
    /// the screen dirty, and nothing marked it dirty when an agent printed. The
    /// per-surface check runs *inside* the paint, so it could say a frame had
    /// changed but never cause one — leaving output to appear at the 250ms floor
    /// instead of at once. v1 sums the same atomics in its loop
    /// (`App::detect_output_redraw`); this is that.
    ///
    /// Shell panes are included: a shell is a surface you watch too, and its
    /// output has exactly the same claim on a repaint.
    pub fn output_generation(&self) -> u64 {
        let sessions = self.live.values().fold(0u64, |acc, live| {
            let shell = live
                .session
                .shell_pane
                .as_ref()
                .map(|pane| pane.last_output_at())
                .unwrap_or(0);
            acc.wrapping_add(live.session.last_output_at())
                .wrapping_add(shell)
        });
        // A plugin's program is summed in too, or a frame would only be painted at
        // the forced-redraw floor while it produced output — which for a full-screen program is
        // the difference between playable and not.
        self.programs.values().fold(sessions, |acc, slot| {
            acc.wrapping_add(slot.pane.last_output_at())
        })
    }

    /// Milliseconds since a session last produced output, if attached.
    ///
    /// The signal a change-driven repaint needs: no output and no input means
    /// there is nothing new to paint.
    pub fn millis_since_output(&self, session: &str) -> Option<u64> {
        self.live
            .get(session)
            .map(|live| live.session.millis_since_last_output())
    }

    /// The activity text and attention notification each live agent last
    /// emitted, keyed by session id.
    ///
    /// These come off the PTY, not the database, which is why they are read
    /// here rather than in the snapshot: the reader thread parses the agent's
    /// OSC window title (activity) and its OSC 9/777 message (notification).
    /// `sync_agent_meta` gates on a generation counter, so an unchanged session
    /// costs one atomic load rather than two mutex locks and two `String`
    /// clones — the ADR-P10 reason v1 does the same.
    pub fn meta(&mut self) -> &HashMap<String, AgentMeta> {
        self.sync_meta();
        self.meta_map()
    }

    /// The mutating half of [`Self::meta`], split out so a caller can end the
    /// `&mut` borrow and then hold the map by reference — the publish path
    /// used to clone the whole map (two `String`s per live session per frame)
    /// purely to release the borrow, which is the exact per-frame clone the
    /// ADR-P10 gating exists to avoid.
    pub fn sync_meta(&mut self) {
        let mut moved = false;
        for (id, live) in &mut self.live {
            if let Some((activity, notification)) = live.session.sync_agent_meta() {
                let entry = self.meta.entry(id.clone()).or_default();
                // Compared before assigning, not assigned blind. This runs on
                // every publish, and a write that changed nothing would still
                // move the version below and invalidate every cached tree —
                // which is how a change-signal quietly becomes worthless
                // (`frame-cost`).
                if entry.activity != activity || entry.notification != notification {
                    entry.activity = activity;
                    entry.notification = notification;
                    moved = true;
                }
            }
        }
        // A session that went away keeps no stale title.
        let before = self.meta.len();
        self.meta.retain(|id, _| self.live.contains_key(id));
        if moved || self.meta.len() != before {
            self.mark_meta_changed();
        }
    }

    /// The map [`Self::sync_meta`] maintains, borrowed.
    pub fn meta_map(&self) -> &HashMap<String, AgentMeta> {
        &self.meta
    }

    /// How many times [`Self::meta`] has actually changed an entry.
    pub fn meta_version(&self) -> u64 {
        self.meta_version
    }

    fn mark_meta_changed(&mut self) {
        self.meta_version = self.meta_version.wrapping_add(1);
    }

    fn mark_failures_changed(&mut self) {
        self.failed_version = self.failed_version.wrapping_add(1);
    }

    /// How many times the set of attach failures has changed.
    pub fn failed_version(&self) -> u64 {
        self.failed_version
    }
}

/// One backend's window inventory, on the discovery worker.
///
/// Split out of [`Terminals::refresh_discovery`] so the throttle and the spawn
/// stay readable beside the two fallible host calls this makes.
fn discover_windows(
    backend: &std::sync::Arc<dyn crate::agent::SessionBackend>,
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
                panes: None,
            };
        }
    }
    let panes = match backend.discover() {
        Ok(found) => {
            let mut by_name: WindowPanes = HashMap::new();
            for window in found {
                by_name
                    .entry(window.name)
                    .or_default()
                    .push(window.backend_id);
            }
            Some(by_name)
        }
        Err(e) => {
            tracing::warn!("could not list windows on {name}: {e:#}");
            None
        }
    };
    Discovered {
        backend: name,
        readied: true,
        panes,
    }
}

/// What an agent reported about itself over its own terminal.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AgentMeta {
    /// The OSC window title, which agents use as a live activity line.
    pub activity: Option<String>,
    /// The message from the most recent attention notification.
    pub notification: Option<String>,
}

impl Default for Terminals {
    fn default() -> Self {
        Self::new()
    }
}

impl SurfaceProvider for Terminals {
    fn render_program(
        &self,
        frame: &mut Frame,
        area: Rect,
        surface: &str,
    ) -> super::paint::ProgramPaint {
        let Some(key) = self.program_key(surface).cloned() else {
            return super::paint::ProgramPaint::NotStarted;
        };
        let Some(slot) = self.programs.get(&key) else {
            return super::paint::ProgramPaint::NotStarted;
        };
        if slot.pane.has_exited() {
            return super::paint::ProgramPaint::Exited(slot.program.clone());
        }

        // Recorded so a click and a wheel can be resolved against this rect, the
        // same way a session surface's is.
        slot.rect.set(area);
        // Matched to the rect on change only: a program told its size every frame
        // is a program sent a SIGWINCH every frame.
        let wanted = (area.height, area.width);
        if slot.size.get() != wanted {
            slot.size.set(wanted);
            slot.pane.resize(area.height, area.width);
        }
        let Ok(parser) = slot.pane.parser.lock() else {
            return super::paint::ProgramPaint::NotStarted;
        };
        frame.render_widget(
            PseudoTerminal::new(parser.screen()).style(Style::default()),
            area,
        );
        super::paint::ProgramPaint::Painted
    }

    fn render_session(&self, frame: &mut Frame, area: Rect, session: &str, scroll: u16) -> bool {
        // A session's shell is addressed as `<id>#shell`, so it is a second
        // surface over the same primitive rather than a second node kind.
        if let Some(id) = session.strip_suffix(SHELL_SUFFIX) {
            let Some(live) = self.live.get(id) else {
                return false;
            };
            let Some(shell) = &live.session.shell_pane else {
                return false;
            };
            // The shell is opened at the terminal's size, not the pane's, and
            // while it is the visible view nothing else drives a resize — so it
            // is matched to its rect here, exactly as the agent surface below
            // is. `Session::resize` sizes both panes, which is right: they take
            // turns in the same rect.
            live.rect.set(area);
            live.shell_visible.set(true);
            let wanted = (area.height, area.width);
            if live.size.get() != wanted {
                live.size.set(wanted);
                live.session.resize(area.height, area.width);
            }
            let Ok(parser) = shell.parser.lock() else {
                return false;
            };
            links::clear_uncovered(frame, area, parser.screen());
            frame.render_widget(
                PseudoTerminal::new(parser.screen()).style(Style::default()),
                area,
            );
            return true;
        }

        let Some(live) = self.live.get(session) else {
            return false;
        };

        // The pane must match the rect it is painted into, or the agent wraps
        // at the wrong width. `resize` is a no-op when nothing changed, but the
        // comparison keeps a tmux round-trip off every frame.
        live.rect.set(area);
        live.shell_visible.set(false);
        let wanted = (area.height, area.width);
        if live.size.get() != wanted {
            live.size.set(wanted);
            live.session.resize(area.height, area.width);
        }

        let Ok(mut parser) = live.session.parser.lock() else {
            return false;
        };
        // Scrollback is a property of the screen, not of the widget, so it is
        // set before reading and left where the plugin asked for it.
        parser.screen_mut().set_scrollback(usize::from(scroll));
        links::clear_uncovered(frame, area, parser.screen());
        frame.render_widget(
            PseudoTerminal::new(parser.screen()).style(Style::default()),
            area,
        );
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kernel::snapshot::SessionRow;

    fn row(id: &str, backend: &str, backend_id: Option<&str>) -> SessionRow {
        SessionRow {
            id: id.to_string(),
            name: "demo".to_string(),
            agent: "claude".to_string(),
            status: "idle".to_string(),
            cwd: None,
            repo: None,
            repos: Vec::new(),
            branch: None,
            base_branch: None,
            backend: backend.to_string(),
            backend_id: backend_id.map(str::to_string),
            remote_host: None,
            agent_session_id: None,
            parent_id: None,
            display_order: None,
            worktree_count: 0,
            git: None,
            hook_state: None,
            shell_backend_id: None,
            member_dirs: Vec::new(),
        }
    }

    fn snapshot(rows: Vec<SessionRow>) -> Snapshot {
        Snapshot {
            sessions: rows,
            taken_at_ms: 0,
            ..Snapshot::default()
        }
    }

    #[test]
    fn a_session_with_no_pane_is_recorded_rather_than_retried() {
        let mut terminals = Terminals::new();
        terminals.sync(&snapshot(vec![row("a", "local-tmux", None)]), 24, 80);
        assert!(!terminals.is_attached("a"));
        assert_eq!(terminals.failure("a"), Some("session has no pane yet"));
        // And the attempt is recorded as "there was no pane", which is what lets
        // a window appearing later be picked up instead of latched out.
        assert_eq!(terminals.failed["a"].pane, None);
    }

    #[test]
    fn an_unknown_backend_is_reported_not_panicked() {
        let mut terminals = Terminals::new();
        terminals.sync(&snapshot(vec![row("a", "ssh:nowhere", Some("%1"))]), 24, 80);
        assert!(!terminals.is_attached("a"));
        let message = terminals.failure("a").unwrap_or_default().to_string();
        assert!(message.contains("no backend"), "{message}");
        assert_eq!(
            terminals.failed["a"].pane.as_deref(),
            Some("%1"),
            "the pane that failed is remembered, so the same one is not retried"
        );
    }

    /// Seed a completed survey of `backend`: one listing, having found `windows`.
    fn surveyed(terminals: &mut Terminals, backend: &str, windows: &[(&str, &[&str])]) {
        terminals.surveys.insert(backend.to_string(), 1);
        terminals.discovered.insert(
            backend.to_string(),
            windows
                .iter()
                .map(|(window, panes)| {
                    (
                        (*window).to_string(),
                        panes.iter().map(|p| (*p).to_string()).collect(),
                    )
                })
                .collect(),
        );
    }

    /// A rebooted machine leaves every persisted pane id naming a pane that no
    /// longer exists. Retrying one fails on `resize-window` — `can't find pane` —
    /// once per retry interval for the life of the process, which is what this
    /// stops: the contradicted id is dropped, and the session is reported as
    /// missing its agent so it can be relaunched.
    #[test]
    fn a_pane_id_a_restarted_server_invalidated_is_dropped_rather_than_retried() {
        let mut terminals = Terminals::new();
        let snapshot = snapshot(vec![row("a", "local-tmux", Some("%822"))]);
        // The row was already waiting when the survey ran, so the survey speaks
        // for it — and it found no window of this session's.
        terminals.waiting_since.insert("a".to_string(), 0);
        surveyed(&mut terminals, "local-tmux", &[("tb-other", &["%1"])]);

        terminals.sync(&snapshot, 24, 80);

        assert_eq!(
            terminals.failed["a"].pane, None,
            "the stale id must not be what was tried"
        );
        assert_eq!(terminals.failure("a"), Some("session has no pane yet"));
        assert_eq!(
            terminals.missing_agents(&snapshot),
            vec!["a".to_string()],
            "a session whose window is gone needs its agent relaunched"
        );
    }

    /// The pane exists, but under another session's window — which is exactly what
    /// a restarted server produces, since it reissues ids from `%0`. Attaching
    /// would aim this session's keystrokes at somebody else's agent.
    #[test]
    fn a_pane_id_reissued_to_another_window_is_stale() {
        let mut terminals = Terminals::new();
        let row = row("a", "local-tmux", Some("%1"));
        terminals.waiting_since.insert("a".to_string(), 0);
        surveyed(&mut terminals, "local-tmux", &[("tb-other", &["%1"])]);

        assert!(terminals.pane_is_stale(&row, "%1"));
    }

    #[test]
    fn a_pane_id_the_survey_confirms_is_kept() {
        let mut terminals = Terminals::new();
        let row = row("a", "local-tmux", Some("%1"));
        terminals.waiting_since.insert("a".to_string(), 0);
        surveyed(&mut terminals, "local-tmux", &[("tb-demo", &["%1"])]);

        assert!(!terminals.pane_is_stale(&row, "%1"));
        assert!(
            terminals.missing_agents(&snapshot(vec![row])).is_empty(),
            "a session whose pane is right there must never be relaunched"
        );
    }

    /// The freshness rule `missing_agents` already lives by, applied to the same
    /// question: a listing that predates the row cannot speak for it, and a
    /// backend nobody has surveyed — every remote one, which is never asked —
    /// answers nothing at all.
    #[test]
    fn an_unsurveyed_backend_contradicts_nothing() {
        let mut terminals = Terminals::new();
        let row = row("a", "ssh:devbox", Some("%1"));
        assert!(!terminals.pane_is_stale(&row, "%1"));

        // A survey the row is not old enough to be judged by is no better.
        surveyed(&mut terminals, "ssh:devbox", &[("tb-other", &["%9"])]);
        terminals.waiting_since.insert("a".to_string(), 1);
        assert!(!terminals.pane_is_stale(&row, "%1"));
    }

    #[test]
    fn a_vanished_session_is_dropped() {
        let mut terminals = Terminals::new();
        terminals.sync(&snapshot(vec![row("a", "local-tmux", None)]), 24, 80);
        assert!(terminals.failed.contains_key("a"));

        terminals.sync(&snapshot(Vec::new()), 24, 80);
        assert!(terminals.failed.is_empty(), "state must not leak");
        assert!(terminals.live.is_empty());
    }

    #[test]
    fn sending_to_an_unattached_session_reports_failure() {
        let terminals = Terminals::new();
        assert!(!terminals.send("nope", vec![b'x']));
    }

    #[test]
    fn an_unattached_session_has_no_output_age() {
        let terminals = Terminals::new();
        assert_eq!(terminals.millis_since_output("nope"), None);
    }

    #[test]
    fn a_single_repository_session_opens_its_shell_in_that_repository() {
        let terminals = Terminals::new();
        let mut row = row("s1", "local-tmux", Some("%1"));
        row.cwd = Some(PathBuf::from("/src/alpha"));
        row.member_dirs = vec![PathBuf::from("/src/alpha")];
        assert_eq!(
            terminals.launch_cwd(&row),
            Some(PathBuf::from("/src/alpha"))
        );
    }

    #[test]
    fn a_multi_repository_session_opens_its_shell_in_the_workspace() {
        // Where the agent itself is running — not whichever member is primary,
        // which is what the recorded cwd names.
        let terminals = Terminals::new();
        let mut row = row("s1", "local-tmux", Some("%1"));
        row.cwd = Some(PathBuf::from("/src/alpha"));
        row.member_dirs = vec![PathBuf::from("/src/alpha"), PathBuf::from("/src/beta")];
        row.agent_session_id = Some("abc-123".to_string());

        let resolved = terminals.launch_cwd(&row).expect("a directory");
        assert!(
            resolved.ends_with("workspaces/abc-123"),
            "the symlink workspace, not a member: {}",
            resolved.display()
        );
    }

    #[test]
    fn a_workspace_that_cannot_be_named_falls_back_to_the_recorded_directory() {
        // No agent session id means no workspace path, and a shell in the
        // primary repository still beats one wherever tmux happened to be.
        let terminals = Terminals::new();
        let mut row = row("s1", "local-tmux", Some("%1"));
        row.cwd = Some(PathBuf::from("/src/alpha"));
        row.member_dirs = vec![PathBuf::from("/src/alpha"), PathBuf::from("/src/beta")];
        assert_eq!(
            terminals.launch_cwd(&row),
            Some(PathBuf::from("/src/alpha"))
        );
    }
}
