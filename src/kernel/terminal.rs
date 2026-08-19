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
//! Two things are deliberately *not* here: spawning sessions and sending them
//! anything beyond keystrokes. Those are commands, and the command bus is a
//! later change. Attaching to what already exists needs none of it.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender};
use std::sync::Arc;

use ratatui::buffer::Buffer;
use ratatui::layout::{Position, Rect};
use ratatui::style::Style;
use ratatui::Frame;
use tui_term::widget::PseudoTerminal;
use unicode_width::UnicodeWidthChar;

use super::paint::SurfaceProvider;
use super::snapshot::Snapshot;

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

/// Owns every live terminal, keyed by session id.
pub struct Terminals {
    backends: crate::agent::BackendRegistry,
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
    /// A **locally** created session has no pane id of its own — `spawn` leaves
    /// it "for the TUI to resolve by name" (`session_ops::spawn`), which is what
    /// v1 does when it adopts windows at restore. Without this the session is
    /// real, its agent is running, and the interface says "session has no pane
    /// yet" forever.
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
    /// Attaches running on workers: session id → the backend it is on.
    ///
    /// Attaching is the one thing here that blocks for a *long* time — an ssh
    /// connect to a host that is down runs to its timeout, and the history
    /// capture is a round trip per pane. On the render thread that is the whole
    /// interface frozen before its first paint, so it happens on a worker and
    /// the result is collected here.
    attaching: HashMap<String, String>,
    attached: (Sender<Attached>, Receiver<Attached>),
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
    /// Here beside `live` rather than in a module of their own, because a pane
    /// needs everything this struct already holds — the backend registry, the
    /// paint seam, the redraw stamp, the rect memo — and `SurfaceProvider` has
    /// one implementor by design, so a second provider is not on the table.
    programs: HashMap<ProgramKey, ProgramSlot>,
}

/// Which plugin's pane, and which of that plugin's panes.
///
/// The plugin is identified by **path**, the identity trust, the disabled set,
/// `run`'s attribution and the inventory all already use. Keying on a declared
/// name instead would let two files claim one pane by declaring the same name,
/// and would move a pane when its author renamed the plugin.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ProgramKey {
    /// The owning plugin's path, relative to the interface directory.
    pub plugin: String,
    /// The name the plugin gave this pane.
    pub name: String,
}

impl ProgramKey {
    pub fn new(plugin: &str, name: &str) -> Self {
        Self {
            plugin: plugin.to_string(),
            name: name.to_string(),
        }
    }

    /// The surface id this pane is addressed by.
    ///
    /// Prefixed so it cannot be confused with a session id, which is a UUID, and
    /// so a paint can tell the two apart by inspection.
    pub fn surface_id(&self) -> String {
        format!("{PROGRAM_SURFACE_PREFIX}{}#{}", self.plugin, self.name)
    }
}

/// Does this surface id name a plugin's program rather than a session?
///
/// A prefix test, and deliberately **not** a parse. Splitting the id back into
/// `(plugin, name)` looked obvious and is ambiguous in both directions: a pane
/// name may contain the separator, and so — on POSIX — may a file name, so
/// neither the first nor the last `#` is reliably the right one. Since the keys
/// are what *generate* these ids, the way back is to look one up
/// ([`Terminals::program_key`]) rather than to take it apart.
pub fn is_program_surface(id: &str) -> bool {
    id.starts_with(PROGRAM_SURFACE_PREFIX)
}

/// Refuse a pane name that would make an unreadable window or an ambiguous id.
///
/// Checked where the ask arrives, so the author gets a message instead of a pane
/// that never appears. The name reaches a tmux window name, which tmux parses as
/// part of a target string.
pub fn validate_program_name(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err("a program pane needs a name".to_string());
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err(format!(
            "program pane name {name:?} must be letters, digits, `-` or `_`"
        ));
    }
    Ok(())
}

/// Marks a surface id as naming a plugin's program rather than a session.
///
/// A session id is a UUID, so no session can collide with this — but it is
/// spelled out rather than relying on that, because "a session id is a UUID" is a
/// fact about the id generator and this is a parsing rule.
pub const PROGRAM_SURFACE_PREFIX: &str = "program:";

/// The most panes one plugin may hold at once.
///
/// Matches `runs::MAX_CONCURRENT` so there is one number to remember rather than
/// two. `run`'s other bounds deliberately do not transfer — an output cap is
/// meaningless for a screen overwritten in place, and a timeout is the opposite of
/// what an interactive program wants — which leaves how many as the only thing
/// left to bound, and the reason the capability is granted separately.
pub const MAX_PROGRAMS_PER_PLUGIN: usize = 4;

/// Which panes belong to a plugin that is no longer loaded.
///
/// Separated from releasing them so the decision is testable without a
/// multiplexer — releasing kills a real process, choosing what to release is a set
/// difference. The same split `admit_program` makes, for the same reason.
fn stale_program_keys<'a>(
    keys: impl Iterator<Item = &'a ProgramKey>,
    live: &[String],
) -> Vec<ProgramKey> {
    keys.filter(|key| !live.contains(&key.plugin))
        .cloned()
        .collect()
}

/// May a plugin already holding `held` panes take one more, for `program`?
///
/// Pure, and separated from starting the pane for the reason `bundled::decide`
/// is: the decision is the part worth testing, and testing it through the spawn
/// would need a multiplexer, which the unit suite deliberately does not have.
///
/// The refusal is a message rather than a silent no, because the plugin surfaces
/// it — a pane that says why it is empty is the difference between a bound and a
/// bug.
fn admit_program(plugin: &str, held: usize, program: &str) -> Result<(), String> {
    if program.trim().is_empty() {
        return Err("a program pane needs a program to run".to_string());
    }
    if held >= MAX_PROGRAMS_PER_PLUGIN {
        return Err(format!(
            "{plugin} already holds {held} program panes (the limit is \
             {MAX_PROGRAMS_PER_PLUGIN})"
        ));
    }
    Ok(())
}

/// One plugin's program pane, and the rect it was last painted into.
struct ProgramSlot {
    pane: crate::agent::backend::ProgramPane,
    /// Last rect painted, so a resize happens on change rather than per frame.
    size: Cell<(u16, u16)>,
    /// Where it was last painted, so a click can be mapped into its grid. Cleared
    /// each frame with every other surface's, so a pane that stopped drawing
    /// cannot be hit (see [`Terminals::forget_rects`]).
    rect: Cell<Rect>,
    /// What the plugin asked to run, kept so a restart can re-adopt with the same
    /// program recorded and a report can name it.
    program: String,
}

impl Terminals {
    /// Build the backend registry the same way the v1 binary does: the local
    /// multiplexer plus every configured or discovered host.
    pub fn new() -> Self {
        let local: Arc<dyn crate::agent::SessionBackend> =
            Arc::new(crate::agent::tmux::LocalTmuxBackend::new());
        let mut backends = crate::agent::BackendRegistry::new(local);

        // Hosts must not block startup: registration is cheap, and readiness is
        // only probed when a session on that host is actually attached.
        let (hosts, _warnings) = crate::agent::host_config::load_all_with_warnings();
        for host in &hosts.hosts {
            backends.register(Arc::new(crate::agent::tmux::TmuxBackend::from_host(host)));
        }

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
            attaching: HashMap::new(),
            attached: std::sync::mpsc::channel(),
            discovering: std::collections::HashSet::new(),
            discovered_rx: std::sync::mpsc::channel(),
            runtime: tokio::runtime::Handle::try_current().ok(),
            programs: HashMap::new(),
        }
    }

    /// Attach to sessions that appeared, and drop those that went away.
    ///
    /// Called once per frame from the event loop. An attach is attempted once per
    /// (session, pane) — so a pane that cannot be adopted does not retry 20×/s,
    /// and a session whose pane only *becomes* known later is still picked up.
    ///
    /// A row with no pane id of its own is resolved by **window name**, which is
    /// what makes a locally created session usable: `spawn` deliberately leaves
    /// the id empty for the interface to resolve, exactly as v1 does when it
    /// adopts windows at restore.
    pub fn sync(&mut self, snapshot: &Snapshot, rows: u16, cols: u16) {
        self.collect_discovered();
        self.collect_attached(rows, cols);
        self.drop_lost_panes(snapshot);

        // Only pay for discovery while something needs it, and never for a remote
        // row: a remote spawn drives control mode and records the real pane id, so
        // a remote row without one is not something a window name can fix.
        let unresolved: Vec<(&str, &str)> = snapshot
            .sessions
            .iter()
            .filter(|row| {
                row.backend_id.is_none()
                    && !self.live.contains_key(&row.id)
                    && !self.attaching.contains_key(&row.id)
            })
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
            let candidate = match row.backend_id.clone() {
                Some(id) => Some(id),
                None => self.pane_by_name(&row.backend, &row.name),
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
            self.start_attach(row, backend_id, rows, cols);
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
        self.failed.retain(|id, _| present.contains(id.as_str()));
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
                let moved = row
                    .backend_id
                    .as_deref()
                    .is_some_and(|id| !id.is_empty() && id != live.session.backend_id());
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
                    self.live.insert(
                        done.session.clone(),
                        Live {
                            session,
                            size: Cell::new((rows, cols)),
                            rect: Cell::new(Rect::default()),
                            shell_visible: Cell::new(false),
                        },
                    );
                    self.failed.remove(&done.session);
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
        self.failed.remove(session);
    }

    /// Record why a session has no pane, and what was tried.
    fn fail(&mut self, session: &str, pane: Option<String>, message: String) {
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

    // ── programs a plugin asked for ────────────────────────────────────────

    /// Start `program` in a pane belonging to `key.plugin`, or keep the one it
    /// already has.
    ///
    /// **Idempotent**, deliberately: a plugin asks on every frame — the pattern
    /// `run` established — so asking for a pane that exists must be a map lookup
    /// and not a second copy of the program.
    ///
    /// The pane is born at `rows`/`cols`, which the caller passes as the rect it
    /// will be painted into where that is known. `open_shell` documents why: the
    /// render-time resize cannot correct a bad birth size when the size memo has
    /// already been set, so the pane looks settled while being a screen wide.
    pub fn start_program(
        &mut self,
        key: &ProgramKey,
        program: &str,
        args: &[String],
        cwd: Option<&std::path::Path>,
        rows: u16,
        cols: u16,
    ) -> Result<(), String> {
        if let Some(slot) = self.programs.get(key) {
            // Already running, or finished and not yet reaped. A finished one is
            // replaced, which is what makes "ask again to restart it" work.
            if !slot.pane.has_exited() {
                return Ok(());
            }
            self.release_program(key);
        }
        admit_program(&key.plugin, self.program_count(&key.plugin), program)?;

        // Local only, by design: a plugin-owned pane has no session and therefore
        // no host, so there is nothing to resolve one from. Recorded as an open
        // question rather than guessed at.
        let backend = Arc::clone(self.backends.default_backend());
        // Inline rather than on a worker, following `ensure_shell_pane`, which
        // spawns from the command drain the same way. Rule 5's concern is a *down
        // remote host* running out its ssh timeout; readying and spawning on the
        // local multiplexer is a round trip on a socket that is already there.
        if let Err(e) = backend.ensure_ready() {
            return Err(format!("could not reach the local multiplexer: {e:#}"));
        }

        let window = crate::agent::tmux::program_window_name(
            &super::bundled::digest(&key.plugin),
            &key.name,
        );
        let env: HashMap<String, String> = HashMap::new();
        let (rows, cols) = (rows.max(1), cols.max(1));

        // An existing window of that name is this same pane from a previous run of
        // the interface: the name is deterministic, so finding it IS the
        // re-adoption path — there is no stored pane id that could go stale.
        let existing = self.find_program_window(&backend, &window);
        let pane = match existing {
            Some(backend_id) => crate::agent::backend::ProgramPane::adopt(
                Arc::clone(&backend),
                &backend_id,
                program,
                rows,
                cols,
            ),
            None => crate::agent::backend::ProgramPane::spawn(
                Arc::clone(&backend),
                &window,
                program,
                args,
                cwd,
                &env,
                rows,
                cols,
            ),
        }
        .map_err(|e| format!("could not start {program}: {e:#}"))?;

        self.programs.insert(
            key.clone(),
            ProgramSlot {
                pane,
                size: Cell::new((rows, cols)),
                rect: Cell::new(Rect::default()),
                program: program.to_string(),
            },
        );
        Ok(())
    }

    /// A window of this exact name already running on `backend`, if there is one.
    ///
    /// This is the whole of re-adoption after a restart: the name is deterministic
    /// (`tmux::program_window_name`), so it is enough to look. Nothing is
    /// persisted, and therefore nothing can be stale — which is the failure a
    /// stored pane id invites and this repository has been bitten by before.
    ///
    /// A tmux round trip, so it is called when a pane is *started* and never per
    /// frame.
    fn find_program_window(
        &self,
        backend: &Arc<dyn crate::agent::SessionBackend>,
        window: &str,
    ) -> Option<String> {
        // `find_window`, not `discover`: discovery filters to agent windows
        // (`tb-`) and a program pane's prefix is `tbp-`, so it would never be seen
        // there. That is also why it cannot be adopted as a session.
        backend.find_window(window).ok().flatten()
    }

    /// The key a surface id names, by matching against the keys that generate
    /// them — see [`is_program_surface`] for why this is a lookup and not a parse.
    ///
    /// Linear over a map holding at most [`MAX_PROGRAMS_PER_PLUGIN`] per plugin,
    /// on a path that runs once per program surface per frame.
    pub fn program_key(&self, surface: &str) -> Option<&ProgramKey> {
        if !is_program_surface(surface) {
            return None;
        }
        self.programs.keys().find(|key| key.surface_id() == surface)
    }

    /// How many panes a plugin is holding.
    pub fn program_count(&self, plugin: &str) -> usize {
        self.programs
            .keys()
            .filter(|key| key.plugin == plugin)
            .count()
    }

    /// Give up one pane, killing the program in it.
    ///
    /// Killed rather than left running: unlike a session, nothing in the interface
    /// could ever show it again, and an unreachable program is worse than a closed
    /// one.
    pub fn release_program(&mut self, key: &ProgramKey) -> bool {
        match self.programs.remove(key) {
            Some(slot) => {
                slot.pane.kill();
                true
            }
            None => false,
        }
    }

    /// Release every pane held by a plugin that is no longer loaded.
    ///
    /// Mirrors `runs::retain_plugins`, which `reload_interface` already calls for
    /// the same reason: a plugin edited away, renamed, removed or turned off must
    /// not leave anything of itself behind across reloads.
    pub fn retain_program_plugins(&mut self, live: &[String]) {
        for key in stale_program_keys(self.programs.keys(), live) {
            self.release_program(&key);
        }
    }

    /// Send bytes to a plugin's program.
    #[must_use = "the caller decides whether the keystroke was consumed from this"]
    pub fn send_to_program(&self, key: &ProgramKey, bytes: Vec<u8>) -> bool {
        let Some(slot) = self.programs.get(key) else {
            return false;
        };
        if slot.pane.has_exited() {
            return false;
        }
        slot.pane.send_input(bytes).is_ok()
    }

    /// What is running in a pane, and whether it has ended — for a pane that wants
    /// to report rather than paint a frozen grid.
    pub fn program_state(&self, key: &ProgramKey) -> Option<(&str, bool)> {
        self.programs
            .get(key)
            .map(|slot| (slot.program.as_str(), slot.pane.has_exited()))
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

    /// Text inside a selection, read from the session's own vt100 grid.
    ///
    /// Reading the grid rather than the painted frame is what makes a selection
    /// made after scrolling copy the history you are looking at, and what
    /// rejoins a soft-wrapped line so a wrapped URL pastes intact.
    pub fn selected_text(
        &self,
        session: &str,
        selection: &crate::session::selection::Selection,
        pane_origin: (u16, u16),
    ) -> Option<String> {
        let (_, parser) = self.surface_parser(session)?;
        let parser = parser.lock().ok()?;
        Some(crate::session::selection::extract_text_from_screen(
            parser.screen(),
            selection,
            pane_origin,
        ))
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

    /// Links visible in a session's terminal.
    ///
    /// Read kernel-side because a terminal is a *surface*: its text is in no
    /// tree, so a decorator could never walk it. That is also where OSC 8
    /// rich-text links live — those print only their label, so a plain-text
    /// scan alone would miss them entirely.
    ///
    /// OSC 8 runs come first and a plain-text match on a cell one of them
    /// already covers is dropped, mirroring the precedence v1's `url_at_click`
    /// applies: where a rich-text escape wraps a bare URL, the escape's target
    /// is what a terminal honours, so listing both would offer the same link
    /// twice.
    pub fn links(&self, session: &str) -> Vec<(String, usize, usize)> {
        let Some((_, parser)) = self.surface_parser(session) else {
            return Vec::new();
        };
        let Ok(parser) = parser.lock() else {
            return Vec::new();
        };
        let rows = crate::session::links::extract_screen_rows(parser.screen());
        let table = parser.callbacks().hyperlinks();

        let mut found: Vec<(String, usize, usize)> = table
            .visible_runs(&rows)
            .into_iter()
            .map(|run| (run.url.to_string(), run.row, run.col))
            .collect();
        found.extend(
            crate::session::links::detect_urls(&rows)
                .into_iter()
                .filter(|link| {
                    rows.get(link.row)
                        .and_then(|row| table.resolve(row, link.start_col))
                        .is_none()
                })
                .map(|link| (link.url, link.row, link.start_col)),
        );
        found
    }

    /// The URL under one cell of a session's grid, if any.
    ///
    /// v1's `url_at_click` (`src/app/mod.rs`), coordinates already converted to
    /// the grid: the OSC 8 table is asked first and the plain-text scan runs
    /// only when it declines, because for a rich-text link the escape is the
    /// *only* place the target exists — the screen holds nothing but the label.
    pub fn url_at(&self, session: &str, row: usize, col: usize) -> Option<String> {
        let (_, parser) = self.surface_parser(session)?;
        let parser = parser.lock().ok()?;
        let rows = crate::session::links::extract_screen_rows(parser.screen());
        rows.get(row)
            .and_then(|text| parser.callbacks().hyperlinks().resolve(text, col))
            .map(str::to_string)
            .or_else(|| {
                let detected = crate::session::links::detect_urls(&rows);
                crate::session::links::url_at_position(&detected, row, col).map(str::to_string)
            })
    }

    /// Every visible OSC 8 run, as cells already drawn into `buf`.
    ///
    /// The outer terminal is the only thing that can open a link when thurbox
    /// runs over ssh, and it knows nothing of ratatui's buffer — so v1 learned
    /// to re-print the runs it just painted wrapped in the escape
    /// ([`paint_hyperlinks`]). Reading the glyphs back out of the frame rather
    /// than off the vt100 grid is what makes a covering modal, a scrolled pane
    /// or a repainted row emit nothing instead of a link over cells that no
    /// longer show it.
    pub fn hyperlink_paints(&self, session: &str, buf: &Buffer) -> Vec<HyperlinkPaint> {
        let Some((live, parser)) = self.surface_parser(session) else {
            return Vec::new();
        };
        let Ok(parser) = parser.lock() else {
            return Vec::new();
        };
        // A session whose agent never printed a link pays one bool check per
        // frame and nothing else.
        if parser.callbacks().hyperlinks().is_empty() {
            return Vec::new();
        }
        let inner = live.rect.get();
        if inner.width == 0 || inner.height == 0 {
            return Vec::new();
        }

        let rows = crate::session::links::extract_screen_rows(parser.screen());
        let mut paints = Vec::new();
        for run in parser.callbacks().hyperlinks().visible_runs(&rows) {
            if run.row >= usize::from(inner.height) || run.col >= usize::from(inner.width) {
                continue;
            }
            let x = inner.x.saturating_add(run.col as u16);
            let y = inner.y.saturating_add(run.row as u16);
            if let Some(cells) = drawn_label_cells(buf, inner, x, y, run.label) {
                paints.push(HyperlinkPaint {
                    x,
                    y,
                    url: run.url.to_string(),
                    cells,
                });
            }
        }
        paints
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
            // A row that names a pane is not missing its agent — it is failing to
            // attach to one, which is a different problem with a different fix.
            .filter(|row| row.backend_id.as_deref().unwrap_or_default().is_empty())
            // A survey that predates the row cannot speak for it: a session
            // created after the last window listing is simply not in it, and
            // relaunching on that silence kills the agent the spawn just started.
            // The backend has to have been read again *since* the row appeared.
            .filter(|row| {
                let surveys = self.surveys.get(&row.backend).copied().unwrap_or(0);
                let seen_at = self.waiting_since.get(&row.id).copied().unwrap_or(surveys);
                surveys > seen_at
            })
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
        for (id, live) in &mut self.live {
            if let Some((activity, notification)) = live.session.sync_agent_meta() {
                let entry = self.meta.entry(id.clone()).or_default();
                entry.activity = activity;
                entry.notification = notification;
            }
        }
        // A session that went away keeps no stale title.
        self.meta.retain(|id, _| self.live.contains_key(id));
        &self.meta
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

/// One run of already-drawn cells to re-print wrapped in an OSC 8 hyperlink.
///
/// The cells are carried rather than re-derived so the re-print reproduces the
/// frame exactly — same glyphs, same colours — and reads as a link only to the
/// terminal, never as a repaint to the eye.
#[derive(Debug, Clone, PartialEq)]
pub struct HyperlinkPaint {
    pub x: u16,
    pub y: u16,
    pub url: String,
    pub cells: Vec<(String, Style)>,
}

/// The cells `label` occupies in the drawn frame, or `None` if the frame no
/// longer prints it there.
///
/// v1's `helpers::drawn_label_cells`, including its asymmetry: a run clipped by
/// the pane's right edge is linked as far as it is visible (`break`), while a
/// glyph that does not match is the frame having moved on and drops the run
/// whole. Advancing by display width skips ratatui's filler cell after a wide
/// glyph, which is what re-printing that glyph does to the cursor.
fn drawn_label_cells(
    buf: &Buffer,
    inner: Rect,
    x: u16,
    y: u16,
    label: &str,
) -> Option<Vec<(String, Style)>> {
    let mut cells = Vec::new();
    let mut cx = x;
    for ch in label.chars() {
        let width = UnicodeWidthChar::width(ch).unwrap_or(1).max(1) as u16;
        if cx.saturating_add(width) > inner.right() {
            break;
        }
        let cell = buf.cell(Position::new(cx, y))?;
        if !cell.symbol().starts_with(ch) {
            return None;
        }
        cells.push((
            cell.symbol().to_string(),
            Style::new()
                .fg(cell.fg)
                .bg(cell.bg)
                .add_modifier(cell.modifier),
        ));
        cx = cx.saturating_add(width);
    }
    (!cells.is_empty()).then_some(cells)
}

/// Re-print each run wrapped in OSC 8, so the terminal thurbox itself runs in
/// can open it.
///
/// Written straight to stdout *after* the backend has flushed the frame, so it
/// cannot interleave with ratatui's own output.
///
/// Bracketed in DECSC/DECRC, because the frame has already placed the caret and
/// this walks it away. `draw` positions the caret last and it stays *shown*, so
/// a focused text field's caret was left sitting wherever the final link run
/// ended — and, since the loop repaints on the forced-redraw floor, it jumped
/// back to the field and away again several times a second. That reads as a
/// cursor blinking in the wrong place rather than as one that moved, which is
/// why it looked like a rendering fault. Restoring here rather than leaving it
/// to the next `draw` keeps the two independent: any number of frames may pass
/// before the next one, and every one of them is a frame with a stray caret.
pub fn paint_hyperlinks(paints: &[HyperlinkPaint]) -> std::io::Result<()> {
    use crossterm::cursor::{MoveTo, RestorePosition, SavePosition};
    use crossterm::queue;
    use crossterm::style::{Print, PrintStyledContent, ResetColor};
    use ratatui::backend::IntoCrossterm;
    use std::io::Write;

    let mut out = std::io::stdout();
    queue!(out, SavePosition)?;
    for paint in paints {
        queue!(
            out,
            MoveTo(paint.x, paint.y),
            Print(crate::session::hyperlink::osc8_open(&paint.url))
        )?;
        for (symbol, style) in &paint.cells {
            let content = (*style).into_crossterm();
            queue!(out, PrintStyledContent(content.apply(symbol.as_str())))?;
        }
        queue!(
            out,
            Print(crate::session::hyperlink::OSC8_CLOSE),
            ResetColor
        )?;
    }
    queue!(out, RestorePosition)?;
    out.flush()
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

    // ── programs a plugin asked for ────────────────────────────────────────

    /// A program surface is distinguishable from a session's by inspection, and a
    /// session id must never read as one — otherwise a plugin could aim
    /// keystrokes at somebody's agent.
    #[test]
    fn a_program_surface_is_told_from_a_session_surface() {
        let key = ProgramKey::new("plugins/90_watch.lua", "watch");
        assert!(is_program_surface(&key.surface_id()));
        for not_a_program in [
            "5f9c1f6e-1b2a-4c3d-8e9f-0a1b2c3d4e5f",
            "5f9c1f6e-1b2a-4c3d-8e9f-0a1b2c3d4e5f#shell",
            "",
        ] {
            assert!(!is_program_surface(not_a_program), "{not_a_program:?}");
        }
    }

    /// The id is resolved by looking up the key that generated it, never by
    /// splitting it.
    ///
    /// Splitting looked obvious and is ambiguous in both directions — a pane name
    /// may contain the separator, and on POSIX so may a file name — so neither the
    /// first nor the last `#` is reliably the right one. A lookup cannot be wrong.
    #[test]
    fn a_program_surface_resolves_by_lookup_not_by_splitting() {
        let terminals = Terminals::new();
        // Nothing is running, so nothing resolves — including a well-formed id.
        let key = ProgramKey::new("plugins/90_watch.lua", "watch");
        assert!(terminals.program_key(&key.surface_id()).is_none());
        assert!(terminals.program_key("not-a-program").is_none());
    }

    /// The name reaches a tmux window name, which tmux parses as part of a target
    /// string — so it is checked where the ask arrives rather than mangled.
    #[test]
    fn a_program_pane_name_is_checked_when_it_is_asked_for() {
        for good in ["watch", "top-2", "my_pane", "a1"] {
            assert!(validate_program_name(good).is_ok(), "{good}");
        }
        for bad in ["", "watch#2", "a b", "../x", "hé"] {
            assert!(validate_program_name(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn a_plugin_may_not_hold_unbounded_panes() {
        for held in 0..MAX_PROGRAMS_PER_PLUGIN {
            assert!(
                admit_program("plugins/90_watch.lua", held, "watch").is_ok(),
                "{held} should be admitted"
            );
        }
        let refused = admit_program("plugins/90_watch.lua", MAX_PROGRAMS_PER_PLUGIN, "watch")
            .expect_err("the limit is a limit");
        // The refusal names the plugin and the limit, because the plugin shows it.
        assert!(refused.contains("90_watch.lua"), "{refused}");
        assert!(
            refused.contains(&MAX_PROGRAMS_PER_PLUGIN.to_string()),
            "{refused}"
        );
    }

    #[test]
    fn a_pane_with_no_program_named_is_refused() {
        // Otherwise the pane spawns the backend's idea of an empty command line.
        assert!(admit_program("plugins/90_watch.lua", 0, "   ").is_err());
        assert!(admit_program("plugins/90_watch.lua", 0, "").is_err());
    }

    /// Nothing running means nothing to send to, and nothing to report — asserted
    /// because "no pane" must not read as "a pane that swallowed your key".
    ///
    /// This return value is **load-bearing for key routing**: the loop consumes a
    /// keystroke only when a send reports delivery. A pane whose surface names a
    /// session that is no longer live therefore does not swallow `Esc`, which is
    /// what keeps the user from being trapped in a pane showing a dead terminal.
    #[test]
    fn an_absent_program_pane_accepts_nothing_and_reports_nothing() {
        let terminals = Terminals::new();
        let key = ProgramKey::new("plugins/90_watch.lua", "watch");
        assert_eq!(terminals.program_count("plugins/90_watch.lua"), 0);
        assert!(!terminals.send_to_program(&key, b"x".to_vec()));
        assert!(terminals.program_state(&key).is_none());
    }

    /// A plugin that is still loaded keeps its pane; one that is gone does not.
    ///
    /// The first half is the one that matters most: a reload happens after every
    /// edit, and losing a running program to one would make reloading unusable.
    #[test]
    fn a_reload_keeps_a_live_plugins_pane_and_releases_a_vanished_ones() {
        let watch = ProgramKey::new("plugins/90_watch.lua", "watch");
        let gone = ProgramKey::new("plugins/91_deleted.lua", "top");
        let keys = [watch.clone(), gone.clone()];

        // Both still loaded: nothing is released.
        let live = vec![
            "plugins/90_watch.lua".to_string(),
            "plugins/91_deleted.lua".to_string(),
        ];
        assert!(stale_program_keys(keys.iter(), &live).is_empty());

        // One edited away, renamed or turned off: only its pane goes.
        let live = vec!["plugins/90_watch.lua".to_string()];
        assert_eq!(stale_program_keys(keys.iter(), &live), vec![gone]);

        // Every plugin gone (a failed reload leaves none loaded): all of them.
        let all: Vec<ProgramKey> = stale_program_keys(keys.iter(), &[]);
        assert_eq!(all.len(), 2);
        let _ = watch;
    }
}
