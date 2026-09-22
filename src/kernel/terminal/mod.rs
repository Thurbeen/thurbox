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
use ratatui::style::{Modifier, Style};
use ratatui::Frame;
use tui_term::widget::PseudoTerminal;

use super::paint::SurfaceProvider;
use super::snapshot::Snapshot;
use crate::session::WORKING_QUIET_MS;

pub mod links;
mod programs;

pub use links::{drawn_link_paints, paint_hyperlinks, HyperlinkPaint};
pub use programs::{plan_keys, validate_program_name, KeysPlan, ProgramKey, ProgramTransition};

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

/// One backend's thurbox windows, indexed by the identity each one carries.
///
/// A window name is not unique — two sessions can be given the same one, and
/// sanitising collapses others together — so the index keys on the session id
/// stamped on the window (ADR-25) and keeps every namesake, which is what lets
/// ambiguity be *reported* rather than resolved by whichever tmux listed last.
type WindowPanes = crate::agent::tmux::WindowIndex;

/// How often a *local* backend's panes may be looked up by window name.
///
/// Discovery is one `list-windows` per backend — cheap, but not 60× a second.
/// This is also how fast a freshly spawned session finds its window, so it
/// stays tight for the server on this machine.
const DISCOVERY_INTERVAL: std::time::Duration = std::time::Duration::from_millis(500);

/// The same, for a backend reached over ssh or `wsl.exe`.
///
/// A remote listing is an ssh round trip rather than a local process, and
/// nothing remote is waiting on it the way a local spawn is: a remote spawn
/// records its real pane id, and the rows that do need a listing — mirrored
/// from a host's own database — describe sessions that already exist. Sharing
/// made remote rows discoverable at all (before it, they were skipped
/// outright); pacing them apart from the local cadence is what keeps that from
/// being two ssh commands a second for as long as one row is unattached.
const REMOTE_DISCOVERY_INTERVAL: std::time::Duration = std::time::Duration::from_secs(5);

/// How long the *same* failed attempt is left alone before it is made again.
///
/// v1 retries a down host on the same cadence (`REMOTE_RETRY_INTERVAL`). Without
/// it a session whose host was offline at startup stays dead for the life of the
/// process, because the candidate pane never changes.
const ATTACH_RETRY_INTERVAL: std::time::Duration = std::time::Duration::from_secs(20);

/// How often a shareable host's database is mirrored into local rows. One
/// multiplexed ssh round trip per host per interval, on a worker; the
/// pipelines mirror again right after anything they delegate, so this is the
/// cadence for changes *other* observers made.
const MIRROR_INTERVAL: std::time::Duration = std::time::Duration::from_secs(10);

/// How long a host is left alone after a mirror pass that could not run.
///
/// [`crate::session_ops::host_cli::usable`] caches a `Yes` for the process
/// lifetime — a host's CLI does not change under us — so a host that was
/// reachable at its first probe and has since gone down keeps a usable
/// verdict, and every pass runs its ssh out to the connect timeout. Backing
/// the *pass* off is what bounds that; the verdict is a separate question
/// with its own retry.
const MIRROR_RETRY_INTERVAL: std::time::Duration = std::time::Duration::from_secs(60);

/// How long a paint waits for the grid of a pane that had none
/// ([`Terminals::render_session`]).
///
/// A local rebuild is a round trip on an open connection plus parsing the
/// history — milliseconds — so this is only reached over a slow link, where a
/// blank pane that fills a moment later beats a frozen interface.
const RESTORE_WAIT: std::time::Duration = std::time::Duration::from_millis(100);

/// The suffix that addresses a session's companion shell as its own surface.
const SHELL_SUFFIX: &str = "#shell";

/// One mouse report, in the encoding the program inside the pane asked for.
///
/// `button` is xterm's Cb before the +32 offset: 0 for the left button, 32 for
/// a move with it held, 64/65 for the wheel (a press with no release). Two
/// encodings are emitted, because a program that asks for the mouse at all and
/// is handed nothing is a pane the mouse is dead in: the alternate screen it
/// is almost certainly on keeps no scrollback, so there is no local fallback
/// to leave it to.
///
/// * `Sgr` (`?1006`) — `CSI < Cb ; Cx ; Cy M`, `m` for a release, and the only
///   encoding with no size limit.
/// * `Default` — xterm's original `CSI M` with each field offset by 32, which
///   caps a coordinate at 223. Past that there is no legal report to send, so
///   `None`: a truncated one would land the event on the wrong cell. A release
///   here has no button of its own — the protocol spells every release 3.
///
/// `Utf8` (`?1005`) is deliberately not emitted. It is ambiguous by
/// construction — a receiver cannot tell it from the default encoding without
/// being told — and no agent asks for it.
fn mouse_report(
    encoding: vt100::MouseProtocolEncoding,
    button: u32,
    col: u32,
    row: u32,
    press: bool,
) -> Option<Vec<u8>> {
    match encoding {
        vt100::MouseProtocolEncoding::Sgr => {
            let end = if press { 'M' } else { 'm' };
            Some(format!("\x1b[<{button};{col};{row}{end}").into_bytes())
        }
        vt100::MouseProtocolEncoding::Default => {
            let button = if press { button } else { 3 };
            let cell = |n: u32| u8::try_from(n + 32).ok();
            Some(vec![
                0x1b,
                b'[',
                b'M',
                cell(button)?,
                cell(col)?,
                cell(row)?,
            ])
        }
        _ => None,
    }
}

/// Where one surface was last painted, and the size its pane was last told
/// about.
///
/// **One per surface, never one per session.** A session shows up to two of
/// them — the agent and its companion shell — and an arrangement may put both
/// on screen at once, in slots of different sizes. Sharing one of these between
/// the two is what made them fight: each frame set the memo from its own rect,
/// found the other's there, and resized on a size that was never its own
/// (#1220). A plugin's program pane holds one of these too, for the same
/// reason.
#[derive(Default)]
struct Painted {
    /// Last size pushed to the pane. A terminal that is not resized to its
    /// visible rect renders at the wrong width, so this is tracked per surface
    /// and pushed whenever that surface's rect changes.
    size: Cell<(u16, u16)>,
    /// Where the surface was last painted, so a mouse position can be converted
    /// into a grid position. Cleared at the start of every frame
    /// ([`Terminals::forget_rects`]), so only what actually painted can be hit.
    rect: Cell<Rect>,
    /// When the surface was last painted: how long it has been off screen,
    /// which is what decides when its grid is dropped ([`Terminals::evict_hidden`]).
    shown_at: Cell<Option<std::time::Instant>>,
    /// The grid row painted at the rect's top edge: 0, except when the grid is
    /// taller than the rect — another instance sizes the pane
    /// ([`paint_sized_elsewhere`]) — and its bottom rows are the ones shown,
    /// since that is where a terminal's newest output and its prompt are. Every
    /// conversion from a screen row to a grid row adds it.
    top: Cell<u16>,
}

impl Painted {
    /// A surface that painted into a rect with room in it.
    fn on_screen(&self) -> bool {
        let rect = self.rect.get();
        rect.width > 0 && rect.height > 0
    }
}

/// The session a surface name belongs to, and whether it names that session's
/// companion shell.
///
/// The one place the `<id>#shell` spelling is read. What comes back says which
/// **pane** was asked for and nothing about what else is on screen: the two are
/// independent surfaces, and a name that resolved differently depending on
/// which of them painted last is exactly the shared tab area this stopped
/// being.
fn split_surface(surface: &str) -> (&str, bool) {
    match surface.strip_suffix(SHELL_SUFFIX) {
        Some(id) => (id, true),
        None => (surface, false),
    }
}

/// The surface name of a session's companion shell.
///
/// Spelled here rather than at each caller so the suffix has one definition —
/// the one the resolver on the other side of it reads back.
pub fn shell_surface(session: &str) -> String {
    format!("{session}{SHELL_SUFFIX}")
}

/// A session we have attached to, and the painted state of each of its panes.
struct Live {
    session: crate::agent::Session,
    /// The agent's own pane.
    agent: Painted,
    /// The companion shell's, independent of the agent's in every respect —
    /// which slot it sits in, how big it is, and whether it is on screen at all.
    shell: Painted,
    /// The shell painting last asked a replacement for
    /// ([`Terminals::take_wanted_shells`]): `Some("")` for none at all, or the
    /// backend id of one that exited. Once per shell, not per frame: a shell
    /// that fails to open repaints its surface every frame, and re-asking each
    /// time would be a multiplexer round trip and an error per frame. Cleared
    /// whenever the shell is seen live, so one that later ends is asked for
    /// again.
    /// Kept here so a restarted or reattached session, a new `Live`, asks
    /// again. The explicit chord asks as often as it is pressed.
    shell_asked: RefCell<Option<String>>,
}

impl Live {
    /// One of this session's two panes, by name.
    fn pane(&self, shell: bool) -> Pane<'_> {
        Pane { live: self, shell }
    }
}

/// One of a session's two panes: the agent's own, or its companion shell's.
///
/// A session has two surfaces, and nearly every question about one of them —
/// how big it is, where it was painted, what it is showing, where a keystroke
/// goes — is a question about exactly one. Carrying the pair around as a
/// `&Live` and a loose `shell` flag is how a caller ends up asking the wrong
/// one, which is the bug this whole module stopped having (#1220). This is that
/// pair resolved once, with the answers hanging off it.
#[derive(Clone, Copy)]
struct Pane<'a> {
    live: &'a Live,
    /// The companion shell rather than the agent.
    shell: bool,
}

impl<'a> Pane<'a> {
    /// Where this pane was painted, and the size it was last told about.
    fn painted(&self) -> &'a Painted {
        if self.shell {
            &self.live.shell
        } else {
            &self.live.agent
        }
    }

    /// This pane's parser, or `None` when the shell was asked for and this
    /// session has none.
    fn parser(&self) -> Option<&'a Arc<std::sync::Mutex<crate::agent::SessionParser>>> {
        match (self.shell, &self.live.session.shell_pane) {
            (true, Some(pane)) => Some(&pane.parser),
            (true, None) => None,
            (false, _) => Some(&self.live.session.parser),
        }
    }

    /// Whether another thurbox is sizing this pane — see
    /// [`paint_sized_elsewhere`].
    fn sized_elsewhere(&self) -> bool {
        match (self.shell, &self.live.session.shell_pane) {
            (true, Some(pane)) => pane.sized_elsewhere(),
            (true, None) => false,
            (false, _) => self.live.session.sized_elsewhere(),
        }
    }

    /// Take this pane's size back if the thurbox sizing it has gone — see
    /// `WiredPane::retake_size`.
    fn retake_size(&self) {
        match (self.shell, &self.live.session.shell_pane) {
            (true, Some(pane)) => pane.retake_size(),
            (true, None) => {}
            (false, _) => self.live.session.retake_size(),
        }
    }

    /// Send bytes to this pane.
    ///
    /// The write half of [`Self::parser`], and it declines in exactly the case
    /// that one does: a shell surface on a session that has no shell is an
    /// error rather than a silent success, so no caller can be told a keystroke
    /// reached a pane that is not there.
    fn send(&self, bytes: Vec<u8>) -> anyhow::Result<()> {
        match (self.shell, &self.live.session.shell_pane) {
            (true, Some(pane)) => pane.send_input(bytes),
            (true, None) => anyhow::bail!("this session has no companion shell"),
            (false, _) => self.live.session.send_input(bytes),
        }
    }

    /// Push a size to this pane, reporting whether it reached the backend —
    /// the render path memoizes only a size that did.
    fn resize(&self, rows: u16, cols: u16) -> bool {
        if self.shell {
            self.live.session.resize_shell(rows, cols)
        } else {
            self.live.session.resize(rows, cols)
        }
    }

    /// Whether this pane painted into a rect with room in it.
    fn on_screen(&self) -> bool {
        self.painted().on_screen()
    }

    /// Whether the pane behind this surface is dead, as the backend reports it
    /// now. `None` when there is no such pane or the backend cannot say.
    fn is_dead(&self) -> Option<bool> {
        if self.shell {
            return self.live.session.shell_is_dead()?.ok();
        }
        self.live.session.is_dead().ok()
    }

    /// When this pane last produced output, as epoch milliseconds.
    fn last_output_at(&self) -> Option<u64> {
        Some(self.wired()?.last_output_at())
    }

    /// [`Self::last_output_at`], moved too when the grid is rebuilt — the
    /// stamp for what is read off the grid.
    fn content_stamp(&self) -> Option<u64> {
        Some(self.wired()?.content_stamp())
    }

    /// The wired pane behind this surface, or `None` for a shell the session
    /// does not have.
    fn wired(&self) -> Option<&'a crate::agent::backend::WiredPane> {
        if self.shell {
            return self.live.session.shell_pane.as_deref();
        }
        Some(&*self.live.session)
    }

    /// Whether the parser holds this pane's grid. A pane that does not exist
    /// has nothing to hold, which is the same as holding it.
    fn is_resident(&self) -> bool {
        self.wired()
            .map_or(true, crate::agent::backend::WiredPane::is_resident)
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
    /// Attached sessions whose `<id>#shell` surface a pane painted while they
    /// had no shell. Drained by the loop, which opens each one
    /// ([`Self::take_wanted_shells`]).
    wanted_shells: RefCell<std::collections::BTreeSet<String>>,
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
    discovery_due: HashMap<String, std::time::Instant>,
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
    /// The sessions whose pane is **currently producing output**, refreshed by
    /// [`Self::sync_printing`] once per publish.
    ///
    /// Membership only, and deliberately nothing else: it is the single fact
    /// the interface needs to animate a `running` session on evidence rather
    /// than on assumption, and it is published in a group of its own because
    /// the answer moves with the agent's output while the session rows do not
    /// (see `kernel::host::publish`).
    printing: std::collections::HashSet<String>,
    /// Moves only when [`Self::printing`] gains or loses a session.
    ///
    /// The reason this is a set-membership version rather than the raw output
    /// clock: `millis_since_output` changes on every byte, and gating a group
    /// on that would rebuild it on every frame under a printing agent — the
    /// exact cost the group gating exists to avoid.
    printing_version: u64,
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
    /// Hosts with a mirror pass in flight, and when each host may next be
    /// mirrored — absent until its first pass returns.
    mirroring: std::collections::HashSet<String>,
    mirror_due: HashMap<String, std::time::Instant>,
    mirror_rx: (Sender<Mirrored>, Receiver<Mirrored>),
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
    /// Spawns and replacements since the last look, **in the order they
    /// happened**.
    ///
    /// `program.exited` is derived by comparing [`Self::program_liveness`]
    /// against the previous look, and the loop applies commands before it
    /// derives — so the map alone cannot answer either question the deriver
    /// has. A restart hides the ending it overwrote, and a program that starts
    /// and dies inside one iteration was never seen running at all.
    ///
    /// One log rather than two lists because the answer depends on the order:
    /// a death is news only if the occupant that died had been *started* after
    /// the last drain or was known running before it. Read as two sets, a
    /// restart on a later frame re-vouches for a death that was already
    /// announced, and the plugin restarts twice for one process
    /// ([`Self::take_program_transitions`]).
    program_transitions: Vec<ProgramTransition>,
    /// How long a session's pane may be off screen before its grid is dropped
    /// ([`Self::evict_hidden`]); `None` keeps every grid for as long as the
    /// pane runs. `settings.toml`'s `hidden_terminal_secs`.
    keep_hidden: Option<std::time::Duration>,
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
            wanted_shells: RefCell::new(std::collections::BTreeSet::new()),
            failed: HashMap::new(),
            discovered: HashMap::new(),
            discovery_due: HashMap::new(),
            surveys: HashMap::new(),
            waiting_since: HashMap::new(),
            meta: HashMap::new(),
            meta_version: 0,
            failed_version: 0,
            printing: std::collections::HashSet::new(),
            printing_version: 0,
            attaching: HashMap::new(),
            attached: std::sync::mpsc::channel(),
            adopted: Vec::new(),
            discovering: std::collections::HashSet::new(),
            discovered_rx: std::sync::mpsc::channel(),
            mirroring: std::collections::HashSet::new(),
            mirror_due: HashMap::new(),
            mirror_rx: std::sync::mpsc::channel(),
            runtime: tokio::runtime::Handle::try_current().ok(),
            programs: HashMap::new(),
            program_transitions: Vec::new(),
            rows_cache: RefCell::new(HashMap::new()),
            keep_hidden: match crate::session::settings::global().hidden_terminal_secs {
                0 => None,
                secs => Some(std::time::Duration::from_secs(secs)),
            },
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
        self.collect_mirrored();
        self.refresh_mirrors();
        self.collect_attached(rows, cols);
        self.drop_lost_panes(snapshot);
        // A surface that lost its parser takes its cached rows with it.
        self.rows_cache
            .borrow_mut()
            .retain(|surface, _| self.surface_parser(surface).is_some());

        // Only pay for discovery while something needs it. Remote rows are
        // surveyed too, since sessions were shared: a row mirrored from a host's
        // database names the pane the host reported, or none at all, and its
        // window is found on the host's server the same way a local one is.
        //
        // A row that already names a pane is surveyed too — a persisted pane id is
        // a hint rather than a fact, see [`Self::pane_is_stale`] — which costs
        // nothing extra: one listing per backend, throttled by that backend's
        // own interval (`DISCOVERY_INTERVAL` locally, `REMOTE_DISCOVERY_INTERVAL`
        // over ssh).
        let unresolved: Vec<(&str, &str)> = snapshot
            .sessions
            .iter()
            .filter(|row| !self.live.contains_key(&row.id) && !self.attaching.contains_key(&row.id))
            .map(|row| (row.id.as_str(), row.backend.as_str()))
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

        self.attach_unresolved(snapshot, rows, cols);
        self.evict_hidden();

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

    /// Override `hidden_terminal_secs` for this instance: how long a pane may
    /// be off screen before its grid is dropped, or `None` to keep every grid.
    pub fn keep_hidden_for(&mut self, keep: Option<std::time::Duration>) {
        self.keep_hidden = keep;
    }

    /// Drop the grid of every session pane that has been off screen for longer
    /// than `hidden_terminal_secs` — or was never shown — where its backend can
    /// give it back ([`crate::agent::backend::WiredPane::evict`]).
    ///
    /// Off screen means not painted in the last frame *and* not painted for a
    /// while: frames are drawn on demand, so a session on screen and quiet can
    /// go a long time between paints, and its rect is what says it is still
    /// there.
    fn evict_hidden(&self) {
        let Some(keep) = self.keep_hidden else {
            return;
        };
        for live in self.live.values() {
            for shell in [false, true] {
                let pane = live.pane(shell);
                if !pane.is_resident() || pane.on_screen() {
                    continue;
                }
                let shown = pane.painted().shown_at.get();
                if shown.is_some_and(|at| at.elapsed() < keep) {
                    continue;
                }
                live.session.evict_pane(shell);
            }
        }
    }

    /// Start an attach for every row that is neither live nor attaching, unless
    /// the same attempt just failed or its backend is still being opened.
    fn attach_unresolved(&mut self, snapshot: &Snapshot, rows: u16, cols: u16) {
        for row in &snapshot.sessions {
            if self.live.contains_key(&row.id) || self.attaching.contains_key(&row.id) {
                continue;
            }
            let (candidate, via_name) = self.candidate_pane(row);
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
    }

    /// The pane to attach `row` to, and whether it was found by window name.
    ///
    /// The row's own pane id, unless a listing contradicts it: a contradicted
    /// one is worth less than the window name that produced it, and falling
    /// through to `None` here is also what lets [`Self::missing_agents`]
    /// relaunch a session whose window is gone for good.
    fn candidate_pane(&self, row: &super::snapshot::SessionRow) -> (Option<String>, bool) {
        match row.backend_id.clone() {
            Some(id) if !self.pane_is_stale(row, &id) => (Some(id), false),
            _ => (self.pane_by_name(row), true),
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
                        // A local id may evict a live pane only when a listing
                        // actually places it in this session's own window —
                        // otherwise an id left over from a previous tmux server
                        // drops the pane just resolved by name, on every frame,
                        // forever. Remote rows are never surveyed, so there is no
                        // listing to ask and the row's own id has to stand.
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
        let lazy = self.keep_hidden.is_some();
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
                let provider: Arc<dyn crate::agent::AgentProvider> =
                    Arc::new(crate::agent::GenericProvider::new(def));
                // Nothing is looking at it yet, so where the grid can be had
                // back later it is not built now — nor its history captured,
                // which was a round trip per pane on every start.
                if lazy && backend.supports_snapshots() {
                    return crate::agent::Session::adopt_dormant(
                        name,
                        rows,
                        cols,
                        &pane,
                        &backend,
                        &provider,
                        HashMap::new(),
                    )
                    .map_err(|e| e.to_string());
                }
                let seed = backend.capture_history(&pane).ok();
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
            // Adopted by name, so the window carries no stamp — this is the one
            // moment its owner is known for certain (the name resolved to
            // exactly one window). Stamping it here is what stops the row
            // depending on a name a later namesake could take; the pane id is
            // persisted for the same reason, by `drain_adopted_panes`.
            if via_name && result.is_ok() {
                if let Err(e) =
                    backend.stamp_window(&pane, &session, crate::agent::tmux::WindowRole::Agent)
                {
                    tracing::debug!(session = %session, "could not stamp the adopted window: {e:#}");
                }
            }
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
                            // The size it was adopted at; the shell has none
                            // until there is one, and gets its own on the first
                            // frame that paints it.
                            agent: Painted {
                                size: Cell::new((rows, cols)),
                                rect: Cell::new(Rect::default()),
                                ..Default::default()
                            },
                            shell: Painted::default(),
                            shell_asked: RefCell::new(None),
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
            // A survey that could not ready its backend or could not list it
            // learned nothing, and the next one a moment later would learn the
            // same nothing: hold it off as long as the attach it serves is held
            // off, so a down host costs one connect attempt per interval rather
            // than a continuous stream of them.
            let interval = if done.readied && done.panes.is_some() {
                discovery_interval(&done.backend)
            } else {
                ATTACH_RETRY_INTERVAL
            };
            self.discovery_due
                .insert(done.backend.clone(), std::time::Instant::now() + interval);
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
    /// Told rather than waited for. `has_exited` does now catch a killed pane —
    /// tmux announces the window's close and the pane's reader ends with it — but
    /// that is a notification arriving in its own time, and the restart knew
    /// before it happened. This is how it says so, immediately.
    ///
    /// The recorded failure is cleared too, or the next attach would be held off
    /// by the retry interval and the session would sit frozen for another 20s
    /// after the pane it needs is already there. The discovery backoff goes for
    /// the same reason: a local restart records no pane id, so the session is
    /// found by *name*, and a survey held off after an earlier failure would
    /// freeze it just as long. One extra listing per backend, on an operation a
    /// person asked for.
    pub fn forget(&mut self, session: &str) {
        self.live.remove(session);
        self.discovery_due.clear();
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
    /// reduced to the ones that have a row waiting.
    ///
    /// Throttled **per backend**, at its own cadence: this is a `list-windows`
    /// per backend, and a session waiting for its window to appear would
    /// otherwise issue one per frame. One shared clock would pace a remote
    /// backend at the local rate, which is the whole cost this avoids — see
    /// [`REMOTE_DISCOVERY_INTERVAL`].
    fn refresh_discovery(&mut self, backends: &[String]) {
        let now = std::time::Instant::now();
        for name in backends.iter().map(String::as_str) {
            if self.discovering.contains(name) {
                continue;
            }
            if !due_now(&self.discovery_due, name, now) {
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

    /// Mirror every shareable host whose last pass is older than
    /// `MIRROR_INTERVAL` (or has never run), one worker each, whether or not a
    /// row lives on it: a host the observer has never used can hold sessions a
    /// peer created. Readying the host — the ssh connect, the CLI probe, a
    /// provisioning on first use — all happens on that worker, never here.
    fn refresh_mirrors(&mut self) {
        let now = std::time::Instant::now();
        let due: Vec<(String, crate::session::HostDef)> = self
            .hosts
            .hosts
            .iter()
            .filter(|host| host.shareable())
            .map(|host| (host.backend_name(), host))
            .filter(|(name, _)| {
                !self.mirroring.contains(name) && due_now(&self.mirror_due, name, now)
            })
            .map(|(name, host)| (name, host.clone()))
            .collect();
        for (name, host) in due {
            self.mirroring.insert(name.clone());
            let tx = self.mirror_rx.0.clone();
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
    fn collect_mirrored(&mut self) {
        while let Ok(done) = self.mirror_rx.1.try_recv() {
            self.mirroring.remove(&done.backend);
            let interval = if done.report.is_ok() {
                MIRROR_INTERVAL
            } else {
                MIRROR_RETRY_INTERVAL
            };
            self.mirror_due
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

    /// Whether the latest listing puts `pane` in this session's own window.
    ///
    /// The *positive* reading, deliberately without the freshness gate: "a listing
    /// says this pane is yours" is an assertion, where "no listing mentions it" is
    /// only an absence — and absence is the half that has to know how old the
    /// listing is.
    fn pane_placed(&self, row: &super::snapshot::SessionRow, pane: &str) -> bool {
        self.discovered
            .get(&row.backend)
            .is_some_and(|windows| windows.places_agent(&row.id, &row.name, pane))
    }

    /// The pane of this session's own agent window, as the listing places it.
    ///
    /// `None` when there is no such window — the agent has not been launched
    /// yet, or has been killed — when the only window of that name is stamped
    /// for a different session, and when the name is ambiguous: keystrokes
    /// going to the wrong agent are worse than a pane that says why it is not
    /// attached.
    fn pane_by_name(&self, row: &super::snapshot::SessionRow) -> Option<String> {
        self.discovered
            .get(&row.backend)?
            .agent_window(&row.id, &row.name)
            .pane()
    }

    /// Forward keystrokes to a session's pane.
    ///
    /// Returns false when the session is not attached, so the caller can leave
    /// the key for something else rather than swallowing it silently.
    #[must_use = "the caller decides whether the keystroke was consumed from this"]
    pub fn send(&self, session: &str, bytes: Vec<u8>) -> bool {
        // A shell surface is addressed `<id>#shell`, the same spelling
        // `render_session` resolves — so the pane you are looking at is the one
        // your keystrokes reach. Without this leg the shell drew but could not
        // be typed into, which only became visible once it stopped being a pane
        // of its own and became a tab of the terminal.
        match self.pane(session) {
            Some(pane) => pane.send(bytes).is_ok(),
            None => false,
        }
    }

    /// The pane a surface name addresses.
    ///
    /// The one place a surface name becomes a pane: `<id>#shell` names the
    /// companion shell and a bare id names the agent, and which one comes back
    /// depends on the name and on nothing else. Resolving it against whichever
    /// of the two painted last is what let a selection in one pane copy out of
    /// the other.
    fn pane(&self, surface: &str) -> Option<Pane<'_>> {
        let (id, shell) = split_surface(surface);
        Some(self.live.get(id)?.pane(shell))
    }

    /// Whether the pane a surface names is dead, as the backend reports it now.
    ///
    /// Resolves the surface the way [`Self::send`] and `surface_parser` do,
    /// and for the same reason: a `<id>#shell` surface is the companion
    /// shell pane, a bare id the session's own pane, and the two die apart. A
    /// caller weighing whether to forward a keystroke or divert it must ask the
    /// pane the keystroke would actually reach — asking the agent while the
    /// shell is on screen judged the wrong pane and could delete a session out
    /// from under a live shell. `None` when the session is not attached, has no
    /// such pane, or the backend cannot say — every caller reads that as "not
    /// known to be dead", so an unsure answer changes nothing.
    pub fn is_dead(&self, surface: &str) -> Option<bool> {
        self.pane(surface)?.is_dead()
    }

    /// Where a surface was painted, and the parser it is showing.
    ///
    /// Every reader of a grid goes through [`Self::pane`] by way of this, so a
    /// copy and a click on a link read the pane they were asked about.
    fn surface_parser(
        &self,
        surface: &str,
    ) -> Option<(
        &Painted,
        &Arc<std::sync::Mutex<crate::agent::SessionParser>>,
    )> {
        let pane = self.pane(surface)?;
        Some((pane.painted(), pane.parser()?))
    }

    /// The contents of one surface's terminal, as text.
    ///
    /// Named by surface, so `<id>#shell` reads the shell's screen and a bare id
    /// reads the agent's. A pane that shows the shell asks for the shell — it
    /// already spells that surface to render it — rather than relying on the
    /// kernel to guess which of the two the user is reading.
    ///
    /// Read on the loop because its caller wants the answer now; the parser is
    /// behind its own mutex, which is what lets the content search read the
    /// same parsers on a worker instead ([`Self::search_sources`]).
    pub fn visible_text(&self, session: &str) -> Option<String> {
        // A pane off screen long enough holds two cells rather than its
        // screen; "nothing to copy" is the honest answer, not two blank rows.
        if !self.pane(session)?.is_resident() {
            return None;
        }
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

    /// The terminals a content search reads, one [`search::Source`] per pane.
    ///
    /// **Both** of a session's panes, each under that session's id: a search
    /// asks which session holds the text, and the answer is the same session
    /// whether the agent printed it or the shell did. Scanning only whichever
    /// pane happened to be on screen is what made a search's answer depend on
    /// the arrangement.
    ///
    /// Only sessions with a live pane appear: an unreachable host or a session
    /// whose pane has not been adopted has no parser to read. A pane whose grid
    /// was dropped is searched all the same: its source reads it back from the
    /// multiplexer, on the worker ([`super::search::Source::restore`]).
    ///
    /// An `Arc` clone and an atomic load per pane — the reading itself happens
    /// on the search worker, under each parser's own lock, which is what the
    /// lock is for: the reader thread already feeds the same parser from off
    /// this thread.
    ///
    /// [`search::Source`]: super::search::Source
    pub fn search_sources(&self, sessions: &[String]) -> Vec<super::search::Source> {
        sessions
            .iter()
            .filter_map(|id| Some((id, self.live.get(id)?)))
            .flat_map(|(id, live)| {
                [false, true].into_iter().filter_map(move |shell| {
                    let pane = live.pane(shell);
                    let wired = pane.wired()?;
                    let restore = (!wired.is_resident()).then(|| {
                        let (backend, pane_id) = live.session.backend_handle();
                        let pane_id = if pane.shell {
                            wired.backend_id().to_string()
                        } else {
                            pane_id
                        };
                        let restore: super::search::Restore = Arc::new(move || {
                            backend
                                .snapshot(&pane_id)
                                .map_err(|e| tracing::debug!(pane = %pane_id, "could not read the pane back: {e:#}"))
                                .ok()
                                .map(|snapshot| crate::agent::backend::parser_from_snapshot(&snapshot))
                        });
                        restore
                    });
                    Some(super::search::Source {
                        session: id.clone(),
                        shell,
                        parser: Arc::clone(pane.parser()?),
                        stamp: pane.last_output_at()?,
                        restore,
                    })
                })
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
    ///
    /// It changes nothing about the agent. Opening a shell used to resize the
    /// agent's pane — the new pane was born at the agent's rect and the two
    /// shared one size memo — which is why a shell appearing anywhere on screen
    /// reflowed the agent (#1220).
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
        // Born at whatever the caller had — the whole terminal — and sized for
        // real by the first frame that paints it, because its size memo starts
        // empty and any rect differs from it. This used to be born at the
        // AGENT's rect, on the reasoning that the render-time resize could not
        // correct a memo the agent had already set. That memo is now the
        // shell's own, so the reasoning is gone with it — and taking a birth
        // size off the other pane was the same coupling in miniature.
        //
        // `Session::adopt` builds a fresh `SessionInfo`, so its own `cwd` is
        // always `None` here — v2 attaches rather than restoring the persisted
        // row. Without passing one the shell inherits the multiplexer's
        // directory, which is wherever thurbox was started.
        let replaced = live
            .session
            .shell_pane
            .as_ref()
            .is_some_and(|pane| pane.has_exited());
        live.session
            .ensure_shell_pane(rows, cols, cwd)
            .map_err(|e| e.to_string())?;
        // A shell replacing one that exited is born at the terminal's size,
        // and the memo still holds the old one's: forgotten, or the first frame
        // would find the rect unchanged and never size the new pty to it.
        if replaced {
            live.shell.size.set((0, 0));
        }
        Ok(())
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

    /// Queue a replacement for `live`'s shell if it has none or it exited,
    /// once per shell ([`Live::shell_asked`]).
    fn want_a_live_shell(&self, id: &str, live: &Live) {
        let current = match &live.session.shell_pane {
            // Live: whatever was asked for came, so a later end is asked about
            // again — even of a pane reattached under the same id.
            Some(pane) if !pane.has_exited() => {
                if live.shell_asked.borrow().is_some() {
                    live.shell_asked.replace(None);
                }
                return;
            }
            Some(pane) => pane.backend_id().to_string(),
            None => String::new(),
        };
        if live.shell_asked.borrow().as_deref() == Some(current.as_str()) {
            return;
        }
        *live.shell_asked.borrow_mut() = Some(current);
        self.wanted_shells.borrow_mut().insert(id.to_string());
    }

    /// Sessions whose shell surface painted with no shell behind it since the
    /// last call, emptied as they are handed over.
    pub fn take_wanted_shells(&self) -> Vec<String> {
        std::mem::take(&mut *self.wanted_shells.borrow_mut())
            .into_iter()
            .collect()
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
            live.agent.rect.set(Rect::default());
            live.shell.rect.set(Rect::default());
        }
        for slot in self.programs.values() {
            slot.painted.rect.set(Rect::default());
        }
    }

    /// Where a surface was last painted, so a click can be mapped into its grid.
    ///
    /// `None` once that surface is not on screen: an empty rect cannot contain a
    /// pointer, so a stale one would only ever be a wrong answer. A session's
    /// two panes answer separately — `<id>#shell` is where the shell was
    /// painted, a bare id where the agent was — so a point is resolved against
    /// the pane it actually landed in.
    pub fn last_rect(&self, session: &str) -> Option<Rect> {
        // A program surface keeps its rect on its own slot, so the one accessor
        // answers for all three kinds — callers ask about "a surface", not about
        // a session.
        if let Some(key) = self.program_key(session) {
            return self
                .programs
                .get(key)
                .filter(|slot| slot.painted.on_screen())
                .map(|slot| slot.painted.rect.get());
        }
        self.pane(session)
            .filter(Pane::on_screen)
            .map(|pane| pane.painted().rect.get())
    }

    /// The grid row a surface painted at the top of its rect: 0, unless its
    /// grid is taller than the rect and its bottom rows are what is shown
    /// (another instance sizes the pane). A caller turning a point into a grid
    /// position adds it.
    pub fn last_top(&self, surface: &str) -> u16 {
        if let Some(key) = self.program_key(surface) {
            return self
                .programs
                .get(key)
                .map_or(0, |slot| slot.painted.top.get());
        }
        self.pane(surface)
            .map_or(0, |pane| pane.painted().top.get())
    }

    /// The pane under `(x, y)`: the session it belongs to, and which of that
    /// session's two it is.
    ///
    /// Both panes are candidates and each is tested against its own rect. Which
    /// one a pointer is over is not a question a session can answer for itself
    /// once the two are on screen at once — asking it that way is what sent a
    /// press in one pane to the program running in the other.
    fn pane_at(&self, x: u16, y: u16) -> Option<(&str, Pane<'_>)> {
        let position = Position::new(x, y);
        self.live.iter().find_map(|(id, live)| {
            [false, true]
                .into_iter()
                .map(|shell| live.pane(shell))
                .find(|pane| pane.on_screen() && pane.painted().rect.get().contains(position))
                .map(|pane| (id.as_str(), pane))
        })
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
        let Some((_, pane)) = self.pane_at(x, y) else {
            return false;
        };
        let Some(parser) = pane.parser() else {
            return false;
        };
        let encoding = match parser.lock() {
            Ok(parser) => {
                let screen = parser.screen();
                (screen.mouse_protocol_mode() != vt100::MouseProtocolMode::None)
                    .then(|| screen.mouse_protocol_encoding())
            }
            Err(_) => None,
        };
        let Some(encoding) = encoding else {
            return false;
        };

        // The rect is the surface's own content area — the plugin's frame is
        // outside it — so the offset needs no border adjustment. PTY cells are
        // 1-based.
        let rect = pane.painted().rect.get();
        let col = u32::from(x - rect.x) + 1;
        let row = u32::from(y - rect.y) + u32::from(pane.painted().top.get()) + 1;
        let button = if up { 64 } else { 65 };
        let Some(bytes) = mouse_report(encoding, button, col, row, true) else {
            return false;
        };
        match pane.send(bytes) {
            Ok(()) => true,
            Err(e) => {
                tracing::warn!("forwarding a wheel tick to the pty failed: {e}");
                false
            }
        }
    }

    /// Hand a left press to the terminal under `(x, y)`, if the program
    /// inside asked for the mouse — any tracking mode hears a press.
    ///
    /// The wheel above answers per tick; a button starts a *gesture*. So the
    /// caller is told which SURFACE took the press — a bare id or `<id>#shell`
    /// — and routes the moves and the release back by that name, the way any
    /// pointer capture works: the surface that heard the press owns the button
    /// until it comes back up, even when the drag wanders off the pane. The
    /// name has to carry which pane, because the other one may be on screen too
    /// and the gesture belongs to exactly one of them.
    pub fn forward_press(&self, x: u16, y: u16) -> Option<String> {
        let (id, pane) = self.pane_at(x, y)?;
        let key = if pane.shell {
            shell_surface(id)
        } else {
            id.to_string()
        };
        self.forward_button(pane, x, y, 0, true, |_| true)
            .then_some(key)
    }

    /// A move with the button held, to the surface that took the press.
    /// Only `?1002`/`?1003` ask to hear these.
    pub fn forward_motion(&self, surface: &str, x: u16, y: u16) -> bool {
        self.pane(surface).is_some_and(|pane| {
            self.forward_button(pane, x, y, 32, true, |mode| {
                matches!(
                    mode,
                    vt100::MouseProtocolMode::ButtonMotion | vt100::MouseProtocolMode::AnyMotion
                )
            })
        })
    }

    /// A move with no button down, to the terminal under `(x, y)` — only
    /// `?1003` asks for these.
    ///
    /// Routed by position, like the wheel and unlike the gesture above: with
    /// no button down there is no press to have chosen an owner, so the move
    /// belongs to whatever pane the pointer is actually over.
    pub fn forward_move(&self, x: u16, y: u16) -> bool {
        let Some((_, pane)) = self.pane_at(x, y) else {
            return false;
        };
        // 35 is "motion, no button": 3 under the 32 move flag.
        self.forward_button(pane, x, y, 35, true, |mode| {
            mode == vt100::MouseProtocolMode::AnyMotion
        })
    }

    /// The release that ends the gesture, to the surface that took the press.
    /// Every mode past X10 (`?9`) asks to hear it.
    pub fn forward_release(&self, surface: &str, x: u16, y: u16) -> bool {
        self.pane(surface).is_some_and(|pane| {
            self.forward_button(pane, x, y, 0, false, |mode| {
                mode != vt100::MouseProtocolMode::Press
            })
        })
    }

    /// Encode and send one button report, if the terminal's tracking mode
    /// `wants` events of this kind.
    ///
    /// Coordinates are clamped into the pane rather than dropped: a drag that
    /// crosses the border still means "as far as you go in that direction" to
    /// the program tracking it, which is how every capture behaves.
    fn forward_button(
        &self,
        pane: Pane<'_>,
        x: u16,
        y: u16,
        button: u32,
        press: bool,
        wants: impl Fn(vt100::MouseProtocolMode) -> bool,
    ) -> bool {
        let Some(parser) = pane.parser() else {
            return false;
        };
        let asked = match parser.lock() {
            Ok(parser) => {
                let screen = parser.screen();
                let mode = screen.mouse_protocol_mode();
                (mode != vt100::MouseProtocolMode::None && wants(mode))
                    .then(|| screen.mouse_protocol_encoding())
            }
            Err(_) => None,
        };
        let Some(encoding) = asked else {
            return false;
        };

        // The rect is the surface's own content area, as for the wheel; a
        // surface no longer painted has an empty one and gets nothing.
        if !pane.on_screen() {
            return false;
        }
        let rect = pane.painted().rect.get();
        let col = u32::from(x.clamp(rect.x, rect.x + rect.width - 1) - rect.x) + 1;
        let row = u32::from(y.clamp(rect.y, rect.y + rect.height - 1) - rect.y)
            + u32::from(pane.painted().top.get())
            + 1;
        let Some(bytes) = mouse_report(encoding, button, col, row, press) else {
            return false;
        };
        match pane.send(bytes) {
            Ok(()) => true,
            Err(e) => {
                tracing::warn!("forwarding a button event to the pty failed: {e}");
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
            .filter(|row| {
                self.discovered
                    .get(&row.backend)
                    .is_some_and(|windows| windows.agent_window(&row.id, &row.name).is_absent())
            })
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
    /// milliseconds — moved as well when its grid is rebuilt, since that
    /// changes the cells without the pane printing
    /// ([`crate::agent::backend::WiredPane::content_stamp`]). `None` when
    /// nothing is attached there. Compare it; do not read it as a time.
    ///
    /// This is the redraw signal for a *surface*: its cells live outside the
    /// node tree, so tree equality cannot tell whether it changed. Comparing
    /// this count against the one a renderer last painted at can — and it is a
    /// single atomic load rather than a diff of screens.
    ///
    /// Accepts the `<id>#shell` spelling, so the view you are looking at is the
    /// pane whose output is checked.
    pub fn output_stamp(&self, surface: &str) -> Option<u64> {
        if let Some(key) = self.program_key(surface) {
            return self.programs.get(key).map(|slot| slot.pane.output_count());
        }
        self.pane(surface)?.content_stamp()
    }

    /// A cheap signature of every live pane's output so far.
    ///
    /// **The redraw signal for the loop**, and the reason it exists rather than
    /// the per-surface stamp below: a frame is only painted when something marked
    /// the screen dirty, and nothing marked it dirty when an agent printed. The
    /// per-surface check runs *inside* the paint, so it could say a frame had
    /// changed but never cause one — leaving output to appear at the 250ms floor
    /// instead of at once. v1 summed the output clocks in its loop
    /// (`App::detect_output_redraw`); this is that, summing counts instead.
    ///
    /// Shell panes are included: a shell is a surface you watch too, and its
    /// output has exactly the same claim on a repaint.
    pub fn output_generation(&self) -> u64 {
        // Content stamps rather than output stamps, so a pane whose grid was
        // rebuilt gets the frame that shows it.
        let sessions = self.live.values().fold(0u64, |acc, live| {
            let shell = live
                .session
                .shell_pane
                .as_ref()
                .map(|pane| pane.content_stamp())
                .unwrap_or(0);
            acc.wrapping_add(live.session.content_stamp())
                .wrapping_add(shell)
        });
        // A plugin's program is summed in too, or a frame would only be painted at
        // the forced-redraw floor while it produced output — which for a full-screen program is
        // the difference between playable and not.
        self.programs.values().fold(sessions, |acc, slot| {
            acc.wrapping_add(slot.pane.output_count())
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

    /// Recompute which sessions are producing output, bumping
    /// [`Self::printing_version`] only when the *set* changes.
    ///
    /// The same signal and the same bound the stuck-`working` fallback uses
    /// ([`WORKING_QUIET_MS`]), for the same reason: a TUI agent animates its
    /// in-progress line while a turn runs, so "printed within the window" is
    /// what separates a turn in flight from a prompt waiting for input. No
    /// process listing can make that distinction, which is why an interface
    /// may animate a `running` session and a headless reader may not.
    ///
    /// Only live panes are considered — a session thurbox has not attached
    /// cannot be observed printing, so it is simply absent and draws static.
    pub fn sync_printing(&mut self) {
        let printing: std::collections::HashSet<String> = self
            .live
            .iter()
            .filter(|(_, live)| live.session.millis_since_last_output() <= WORKING_QUIET_MS)
            .map(|(session, _)| session.clone())
            .collect();
        if printing != self.printing {
            self.printing = printing;
            self.printing_version = self.printing_version.wrapping_add(1);
        }
    }

    /// The sessions producing output as of the last [`Self::sync_printing`].
    pub fn printing(&self) -> &std::collections::HashSet<String> {
        &self.printing
    }

    /// Moves only when [`Self::printing`]'s membership does.
    pub fn printing_version(&self) -> u64 {
        self.printing_version
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

/// Whether a per-backend timer has come due. An entry is *when it may next
/// run*, so an absent one has never run and is due now.
fn due_now(
    schedule: &HashMap<String, std::time::Instant>,
    name: &str,
    now: std::time::Instant,
) -> bool {
    !schedule.get(name).is_some_and(|due| *due > now)
}

/// How long to leave `backend` alone between window listings — the local
/// cadence, or the remote one for a backend whose listing travels over ssh.
fn discovery_interval(backend: &str) -> std::time::Duration {
    if crate::session::is_remote_backend(backend) {
        REMOTE_DISCOVERY_INTERVAL
    } else {
        DISCOVERY_INTERVAL
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
        Ok(found) => Some(WindowPanes::from_listing(found)),
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

/// A mirror pass that came back from its worker.
struct Mirrored {
    backend: String,
    report: Result<crate::session_ops::mirror::MirrorReport, String>,
}

/// One mirror pass for `host`, on the worker: its own database handle, the
/// CLI probe (cached across passes), and the reconcile.
fn mirror_host(
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
    crate::session_ops::mirror::mirror_host(&db, host, &cli)
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

impl Terminals {
    /// This provider painting a cursor on `input` alone: the same grids, and
    /// the block only on the surface the keys go to.
    ///
    /// `PseudoTerminal` paints the cursor as a block on the grid, and every
    /// terminal on screen painted one, so the block said nothing about where
    /// the keys go. `input` is the focused pane's first live surface — the one
    /// the keyboard routes to (`Node::first_live_surface`) — and `None` for a
    /// pane without focus or a float, whose keys go to its Lua. Painted there
    /// only, the cursor is the terminal's own focus cue, and one that needs no
    /// colour to read.
    pub fn cursor_on<'a>(&'a self, input: Option<&'a str>) -> CursorOn<'a> {
        CursorOn {
            terminals: self,
            input,
        }
    }

    fn paint_program(
        &self,
        frame: &mut Frame,
        area: Rect,
        surface: &str,
        cursor: bool,
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
        slot.painted.rect.set(area);
        // Matched to the rect on change only: a program told its size every frame
        // is a program sent a SIGWINCH every frame.
        let wanted = (area.height, area.width);
        if slot.painted.size.get() != wanted && slot.pane.resize(area.height, area.width) {
            slot.painted.size.set(wanted);
        }
        slot.pane.retake_size();
        let Ok(parser) = slot.pane.parser.lock() else {
            return super::paint::ProgramPaint::NotStarted;
        };
        let top = grid_top(parser.screen(), area);
        slot.painted.top.set(top);
        let view = FromRow {
            screen: parser.screen(),
            top,
        };
        frame.render_widget(screen_widget(&view, cursor), area);
        if parser.screen().size() != wanted && slot.pane.sized_elsewhere() {
            paint_sized_elsewhere(frame, area, parser.screen().size());
        }
        super::paint::ProgramPaint::Painted
    }

    fn paint_session(
        &self,
        frame: &mut Frame,
        area: Rect,
        session: &str,
        scroll: u16,
        cursor: bool,
    ) -> bool {
        // A session's shell is addressed as `<id>#shell`, so it is a second
        // surface over the same primitive rather than a second node kind — and
        // ONE path paints either of them, parameterised by which pane the name
        // asked for. It was two, and the two disagreed: each recorded its rect
        // and its size in the same pair of cells, so whichever painted second
        // resized both panes to its own rect and the pane painted first spent
        // the next frame undoing it (#1220).
        let Some(pane) = self.pane(session) else {
            return false;
        };
        // A shell surface painted with no live shell behind it — never opened,
        // or `exit`ed — asks for one: a layout that gives the shell a pane of
        // its own shows it without anyone asking, and a tab showing a dead
        // shell would otherwise show it for good. Noted rather than opened
        // here, because opening is a round trip to the multiplexer and this is
        // the paint.
        if pane.shell {
            self.want_a_live_shell(split_surface(session).0, pane.live);
        }
        let Some(parser) = pane.parser() else {
            return false;
        };
        let painted = pane.painted();
        // One terminal has one size, so it is painted into one rect a frame:
        // the first. A second — an agent pane edited before shell panes existed
        // still offering its Shell tab under a layout that places the shell
        // pane — would resize the pty to its own rect every frame and leave the
        // other showing it wrapped at the wrong width (the #1220 class).
        if painted.on_screen() && painted.rect.get() != area {
            return false;
        }

        // The pane must match the rect it is painted into, or its program wraps
        // at the wrong width. The memo is this surface's own, so the comparison
        // keeps a tmux round-trip off every frame without ever reading a size
        // that belongs to the other pane.
        painted.rect.set(area);
        painted.shown_at.set(Some(std::time::Instant::now()));
        let wanted = (area.height, area.width);
        if painted.size.get() != wanted && pane.resize(area.height, area.width) {
            painted.size.set(wanted);
        }
        pane.retake_size();

        // A pane with no grid asks for it — after the resize, so the snapshot
        // is taken at the size it is about to be shown at. The paint that asks
        // waits a moment for it, so the first frame is the current screen and
        // never an old one (#1242). What does not arrive in time is painted
        // blank and repainted when it lands, which moves the output
        // generation; later paints do not wait again, or a host that stopped
        // answering would stall every frame the pane is on screen.
        if !pane.is_resident() {
            if pane.live.session.restore_pane(pane.shell) {
                if let Some(wired) = pane.wired() {
                    wired.wait_resident(RESTORE_WAIT);
                }
            }
            if !pane.is_resident() {
                frame.render_widget(ratatui::widgets::Clear, area);
                return true;
            }
        }

        let Ok(mut parser) = parser.lock() else {
            return false;
        };
        // Scrollback is a property of the screen, not of the widget, so it is
        // set before reading and left where the plugin asked for it. The shell
        // has one of its own — it is wired up by the same `wire_up` the agent
        // pane is — and it is this surface's offset that is applied here.
        parser.screen_mut().set_scrollback(usize::from(scroll));
        links::clear_uncovered(frame, area, parser.screen());
        let top = grid_top(parser.screen(), area);
        painted.top.set(top);
        let view = FromRow {
            screen: parser.screen(),
            top,
        };
        frame.render_widget(screen_widget(&view, cursor), area);
        if parser.screen().size() != wanted && pane.sized_elsewhere() {
            paint_sized_elsewhere(frame, area, parser.screen().size());
        }
        true
    }
}

/// Say why a terminal is not the size of the rect it is painted into.
///
/// On a server several thurbox instances share, one of them sizes each pane
/// (`TmuxBackend::resize`) and the others show that pane's screen as it is —
/// with blank margins when their rect is bigger, cropped when it is smaller —
/// rather than parsing its output into a grid of their own size. Without a word
/// on it that reads as a rendering bug, so the bottom row says whose size this
/// is and how to take it: typing into the pane hands the size over.
fn paint_sized_elsewhere(frame: &mut Frame, area: Rect, (rows, cols): (u16, u16)) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let text =
        format!(" {cols}\u{d7}{rows} \u{b7} sized by another thurbox \u{b7} type here to resize ");
    let width = u16::try_from(text.chars().count())
        .unwrap_or(u16::MAX)
        .min(area.width);
    let row = Rect {
        x: area.right() - width,
        y: area.bottom() - 1,
        width,
        height: 1,
    };
    let style = Style::default().add_modifier(Modifier::DIM | Modifier::REVERSED);
    frame.render_widget(ratatui::widgets::Paragraph::new(text).style(style), row);
}

/// The grid row to paint at the top of `area`: the bottom rows of a grid
/// taller than the rect, see [`Painted::top`].
fn grid_top(screen: &vt100::Screen, area: Rect) -> u16 {
    screen.size().0.saturating_sub(area.height)
}

/// A terminal grid as a widget, painting its cursor or not.
fn screen_widget<'a>(view: &'a FromRow<'a>, cursor: bool) -> PseudoTerminal<'a, FromRow<'a>> {
    PseudoTerminal::new(view)
        .style(Style::default())
        .cursor(tui_term::widget::Cursor::default().visibility(cursor))
}

/// A grid read from row `top` down, so its bottom rows fill a shorter rect.
struct FromRow<'a> {
    screen: &'a vt100::Screen,
    top: u16,
}

impl tui_term::widget::Screen for FromRow<'_> {
    type C = vt100::Cell;

    fn cell(&self, row: u16, col: u16) -> Option<&Self::C> {
        tui_term::widget::Screen::cell(self.screen, row.checked_add(self.top)?, col)
    }

    fn hide_cursor(&self) -> bool {
        let (row, _) = tui_term::widget::Screen::cursor_position(self.screen);
        tui_term::widget::Screen::hide_cursor(self.screen) || row < self.top
    }

    fn cursor_position(&self) -> (u16, u16) {
        let (row, col) = tui_term::widget::Screen::cursor_position(self.screen);
        (row.saturating_sub(self.top), col)
    }
}

impl SurfaceProvider for Terminals {
    fn render_program(
        &self,
        frame: &mut Frame,
        area: Rect,
        surface: &str,
    ) -> super::paint::ProgramPaint {
        self.paint_program(frame, area, surface, true)
    }

    fn render_session(&self, frame: &mut Frame, area: Rect, session: &str, scroll: u16) -> bool {
        self.paint_session(frame, area, session, scroll, true)
    }
}

/// [`Terminals`] with the cursor on one surface: see [`Terminals::cursor_on`].
pub struct CursorOn<'a> {
    terminals: &'a Terminals,
    input: Option<&'a str>,
}

impl SurfaceProvider for CursorOn<'_> {
    fn render_program(
        &self,
        frame: &mut Frame,
        area: Rect,
        surface: &str,
    ) -> super::paint::ProgramPaint {
        let cursor = self.input == Some(surface);
        self.terminals.paint_program(frame, area, surface, cursor)
    }

    fn render_session(&self, frame: &mut Frame, area: Rect, session: &str, scroll: u16) -> bool {
        let cursor = self.input == Some(session);
        self.terminals
            .paint_session(frame, area, session, scroll, cursor)
    }
}

/// The agent pane and its companion shell are independent surfaces — the
/// property this module's sharing used to break (#1220).
#[cfg(test)]
mod decoupling;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kernel::snapshot::SessionRow;

    /// A grid taller than its rect shows its bottom rows — where the newest
    /// output and the prompt are — and a selection over them copies those rows.
    #[test]
    fn a_grid_taller_than_its_rect_shows_its_bottom() {
        use ratatui::widgets::Widget;
        let mut parser = vt100::Parser::new(6, 4, 0);
        parser.process(b"r0\r\nr1\r\nr2\r\nr3\r\nr4\r\nr5");
        let area = Rect::new(0, 0, 4, 4);
        let top = grid_top(parser.screen(), area);
        assert_eq!(top, 2);
        let view = FromRow {
            screen: parser.screen(),
            top,
        };
        let mut buffer = ratatui::buffer::Buffer::empty(area);
        screen_widget(&view, true).render(area, &mut buffer);
        let rows: Vec<String> = (0..4)
            .map(|y| (0..2).map(|x| buffer[(x, y)].symbol()).collect())
            .collect();
        assert_eq!(rows, ["r2", "r3", "r4", "r5"]);
        assert_eq!(
            buffer[(2, 3)].symbol(),
            "\u{2588}",
            "the cursor, moved up by top"
        );

        use crate::kernel::selection::{PaneBounds, Selection, TermPos};
        let selection = Selection {
            anchor: TermPos { row: 0, col: 0 },
            cursor: TermPos { row: 1, col: 3 },
            dragging: false,
            pane: PaneBounds::from_rect(area),
        };
        assert_eq!(
            crate::kernel::selection::extract_text_from_rows(
                parser.screen(),
                &selection,
                (0, 0),
                top
            ),
            "r2\nr3"
        );
    }

    /// A grid another instance sizes says so on its bottom row, right-aligned,
    /// and a rect too narrow for the whole sentence gets as much as fits.
    #[test]
    fn a_pane_sized_elsewhere_says_whose_size_it_is() {
        for width in [80u16, 12] {
            let mut terminal =
                ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, 5)).unwrap();
            terminal
                .draw(|frame| paint_sized_elsewhere(frame, frame.area(), (3, 60)))
                .unwrap();
            let buffer = terminal.backend().buffer();
            let bottom: String = (0..width).map(|x| buffer[(x, 4)].symbol()).collect();
            let full = " 60\u{d7}3 \u{b7} sized by another thurbox \u{b7} type here to resize ";
            let shown: String = full.chars().take(usize::from(width)).collect();
            assert!(bottom.trim_end().ends_with(shown.trim_end()), "{bottom:?}");
            let above: String = (0..width).map(|x| buffer[(x, 3)].symbol()).collect();
            assert_eq!(above.trim(), "", "only the bottom row is painted");
        }
    }

    #[test]
    fn only_a_terminal_that_holds_focus_paints_its_cursor() {
        use ratatui::widgets::Widget;
        let mut parser = vt100::Parser::new(2, 8, 0);
        parser.process(b"$ ");
        for (cursor, painted) in [(true, "\u{2588}"), (false, " ")] {
            let mut buffer = ratatui::buffer::Buffer::empty(Rect::new(0, 0, 8, 2));
            let view = FromRow {
                screen: parser.screen(),
                top: 0,
            };
            screen_widget(&view, cursor).render(buffer.area, &mut buffer);
            assert_eq!(buffer[(2, 0)].symbol(), painted, "cursor shown: {cursor}");
            assert_eq!(buffer[(0, 0)].symbol(), "$", "the grid paints either way");
        }
    }

    #[test]
    fn a_mouse_event_is_encoded_the_way_the_pane_asked_for_it() {
        use vt100::MouseProtocolEncoding as E;
        assert_eq!(
            mouse_report(E::Sgr, 64, 12, 3, true).expect("sgr"),
            b"\x1b[<64;12;3M".to_vec()
        );
        assert_eq!(
            mouse_report(E::Sgr, 65, 12, 3, true).expect("sgr"),
            b"\x1b[<65;12;3M".to_vec()
        );
        // SGR keeps the button on a release — the final letter is what flips.
        assert_eq!(
            mouse_report(E::Sgr, 0, 12, 3, false).expect("sgr"),
            b"\x1b[<0;12;3m".to_vec()
        );
        // The original encoding offsets every field by 32. A program that asks
        // for the mouse without asking for SGR used to be handed nothing, and
        // its alternate screen leaves no scrollback to fall back on.
        assert_eq!(
            mouse_report(E::Default, 64, 12, 3, true).expect("default"),
            vec![0x1b, b'[', b'M', 96, 44, 35]
        );
        // ...and it spells every release button 3, whatever went down.
        assert_eq!(
            mouse_report(E::Default, 0, 12, 3, false).expect("default"),
            vec![0x1b, b'[', b'M', 35, 44, 35]
        );
        // Past 223 there is no legal report, and a truncated one would land
        // the event on the wrong cell.
        assert_eq!(mouse_report(E::Default, 64, 224, 3, true), None);
        assert!(mouse_report(E::Default, 64, 223, 223, true).is_some());
        // Ambiguous by construction, and asked for by nothing.
        assert_eq!(mouse_report(E::Utf8, 64, 12, 3, true), None);
    }

    fn row(id: &str, backend: &str, backend_id: Option<&str>) -> SessionRow {
        SessionRow {
            id: id.to_string(),
            name: "demo".to_string(),
            agent: "claude".to_string(),
            status: crate::session::SessionState::Idle,
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
            stopped: false,
            hook_state: None,
            reports_as: None,
            detected_agent: None,
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

    /// Seed a completed survey of `backend`: one listing, having found `windows`
    /// — each an unstamped window, the shape every window had before ADR-25 and
    /// the one these tests are about (a stamped window is unambiguous by
    /// construction).
    fn surveyed(terminals: &mut Terminals, backend: &str, windows: &[(&str, &[&str])]) {
        terminals.surveys.insert(backend.to_string(), 1);
        let listing = windows.iter().flat_map(|(window, panes)| {
            panes
                .iter()
                .map(|pane| crate::agent::backend::DiscoveredSession {
                    backend_id: (*pane).to_string(),
                    name: (*window).to_string(),
                    is_alive: true,
                    session: String::new(),
                    role: crate::agent::tmux::WindowRole::Agent,
                })
        });
        terminals
            .discovered
            .insert(backend.to_string(), WindowPanes::from_listing(listing));
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

    /// Ambiguity is not absence. Two `tb-demo` windows with no stamp between
    /// them cannot be told apart, and relaunching on that silence is how a
    /// *third* agent appears beside the two that already collide.
    #[test]
    fn ambiguous_namesakes_are_never_relaunched() {
        let mut terminals = Terminals::new();
        let row = row("a", "local-tmux", None);
        terminals.waiting_since.insert("a".to_string(), 0);
        surveyed(&mut terminals, "local-tmux", &[("tb-demo", &["%1", "%2"])]);

        assert_eq!(
            terminals.pane_by_name(&row),
            None,
            "and none is attached to"
        );
        assert!(
            terminals.missing_agents(&snapshot(vec![row])).is_empty(),
            "a name nobody can resolve must not be read as a missing agent"
        );
    }

    /// A window stamped for a namesake is not this session's to attach to, let
    /// alone to relaunch over — it is somebody else's live agent.
    #[test]
    fn a_namesakes_stamped_window_is_neither_attached_to_nor_relaunched_over() {
        let mut terminals = Terminals::new();
        let row = row("a", "local-tmux", None);
        terminals.waiting_since.insert("a".to_string(), 0);
        terminals.surveys.insert("local-tmux".to_string(), 1);
        terminals.discovered.insert(
            "local-tmux".to_string(),
            WindowPanes::from_listing([crate::agent::backend::DiscoveredSession {
                backend_id: "%1".into(),
                name: "tb-demo".into(),
                is_alive: true,
                session: "b".into(),
                role: crate::agent::tmux::WindowRole::Agent,
            }]),
        );

        assert_eq!(terminals.pane_by_name(&row), None);
        assert_eq!(
            terminals.missing_agents(&snapshot(vec![row])),
            vec!["a".to_string()],
            "this row's own agent really is gone, so it is relaunched — beside \
             the namesake's, not over it"
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

    /// Sharing made remote rows discoverable, and one shared clock would have
    /// paced an ssh `list-windows` at the local cadence: two ssh commands a
    /// second for as long as one remote row was unattached.
    #[test]
    fn a_remote_backend_is_surveyed_on_its_own_cadence() {
        assert_eq!(discovery_interval("local-tmux"), DISCOVERY_INTERVAL);
        assert_eq!(discovery_interval("ssh:devbox"), REMOTE_DISCOVERY_INTERVAL);
        assert_eq!(discovery_interval("wsl:ubuntu"), REMOTE_DISCOVERY_INTERVAL);
        assert!(
            REMOTE_DISCOVERY_INTERVAL > DISCOVERY_INTERVAL,
            "a listing that travels over ssh costs more than a local one"
        );
    }

    /// A down host answers a survey no faster than it answers an attach, and
    /// the next survey would learn the same nothing. Without this the 20 s
    /// attach retry is bypassed by a discovery loop that re-issues the connect
    /// the moment the last one gives up.
    #[test]
    fn a_survey_that_learned_nothing_backs_off_to_the_attach_retry() {
        let mut terminals = Terminals::new();
        terminals
            .discovered_rx
            .0
            .send(Discovered {
                backend: "ssh:devbox".to_string(),
                readied: false,
                panes: None,
            })
            .unwrap();
        terminals.collect_discovered();

        let due = terminals.discovery_due["ssh:devbox"];
        assert!(
            due > std::time::Instant::now() + REMOTE_DISCOVERY_INTERVAL,
            "a fruitless survey must wait longer than an ordinary one"
        );
    }

    #[test]
    fn a_survey_that_found_windows_keeps_the_ordinary_cadence() {
        let mut terminals = Terminals::new();
        terminals
            .discovered_rx
            .0
            .send(Discovered {
                backend: "ssh:devbox".to_string(),
                readied: true,
                panes: Some(WindowPanes::default()),
            })
            .unwrap();
        terminals.collect_discovered();

        let due = terminals.discovery_due["ssh:devbox"];
        assert!(
            due <= std::time::Instant::now() + REMOTE_DISCOVERY_INTERVAL,
            "a survey that worked is not penalised"
        );
    }

    /// `host_cli::usable` caches a `Yes` for the process lifetime, so a host
    /// that goes down after its first probe keeps one — and every pass then
    /// runs its ssh out to the connect timeout. The pass backs off even though
    /// the verdict does not.
    /// A local restart records no pane id, so the session is resolved by name —
    /// and a survey sitting in a failure backoff would freeze it exactly as long
    /// as the attach retry `forget` already clears.
    #[test]
    fn a_restart_surveys_afresh_rather_than_waiting_out_a_backoff() {
        let mut terminals = Terminals::new();
        terminals.discovery_due.insert(
            "local-tmux".to_string(),
            std::time::Instant::now() + ATTACH_RETRY_INTERVAL,
        );

        terminals.forget("a");

        assert!(
            due_now(
                &terminals.discovery_due,
                "local-tmux",
                std::time::Instant::now()
            ),
            "a restart must not wait out a backoff for the listing that finds its new pane"
        );
    }

    #[test]
    fn a_mirror_pass_that_could_not_run_backs_its_host_off() {
        let mut terminals = Terminals::new();
        terminals
            .mirror_rx
            .0
            .send(Mirrored {
                backend: "ssh:devbox".to_string(),
                report: Err("host unreachable".to_string()),
            })
            .unwrap();
        terminals.collect_mirrored();

        let due = terminals.mirror_due["ssh:devbox"];
        assert!(
            due > std::time::Instant::now() + MIRROR_INTERVAL,
            "a host that could not be reached is not asked again in ten seconds"
        );
    }

    #[test]
    fn a_mirror_pass_that_ran_keeps_the_ordinary_cadence() {
        let mut terminals = Terminals::new();
        terminals
            .mirror_rx
            .0
            .send(Mirrored {
                backend: "ssh:devbox".to_string(),
                report: Ok(crate::session_ops::mirror::MirrorReport::default()),
            })
            .unwrap();
        terminals.collect_mirrored();

        let due = terminals.mirror_due["ssh:devbox"];
        assert!(due <= std::time::Instant::now() + MIRROR_INTERVAL);
    }
}
