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
//! the rule `cli` already follows.
//!
//! The shared [`Terminals`] state and its per-frame [`Terminals::sync`] live
//! here. Each job it runs has a file of its own: attaching a pane on a worker
//! in `attach`, surveying each backend's windows and mirroring a host's rows
//! in `discovery`, drawing a surface in `paint`, companion shells in `shells`,
//! plugin programs in `programs`, and link detection / selection reads / OSC 8
//! re-printing in [`links`].
//!
//! Two things are deliberately *not* here: spawning sessions and sending them
//! anything beyond keystrokes. Those are commands, and the command bus is a
//! later change. Attaching to what already exists needs none of it.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender};
use std::sync::Arc;

use ratatui::layout::{Position, Rect};

use super::snapshot::Snapshot;
use crate::session::WORKING_QUIET_MS;

mod attach;
mod discovery;
mod fleet;
pub mod links;
mod paint;
mod programs;
mod shells;
pub mod stream_pane;

pub use shells::ShellResult;

pub use paint::CursorOn;

pub use links::{drawn_link_paints, paint_hyperlinks, HyperlinkPaint};
pub use programs::{plan_keys, validate_program_name, KeysPlan, ProgramKey, ProgramTransition};

use paint::Painted;
use programs::ProgramSlot;

/// The suffix that addresses a session's companion shell as its own surface.
const SHELL_SUFFIX: &str = "#shell";

/// One mouse report, in the encoding the program inside the pane asked for.
///
/// `button` is xterm's Cb before the +32 offset: 0 for the left button, 32 for
/// a move with it held, 64/65 for the wheel (a press with no release). Every
/// encoding is emitted, because a program that asks for the mouse at all and
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
/// * `Utf8` (`?1005`) — the default encoding with each field written as a
///   UTF-8 character rather than a byte, which lifts the cap to 2015. A pane
///   attached after its app asked for it is put back in it
///   (`mouse_seed_bytes`), so it must be answered as well as any other.
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
        vt100::MouseProtocolEncoding::Default | vt100::MouseProtocolEncoding::Utf8 => {
            let button = if press { button } else { 3 };
            let utf8 = encoding == vt100::MouseProtocolEncoding::Utf8;
            let mut out = b"\x1b[M".to_vec();
            for n in [button, col, row] {
                let value = n + 32;
                if utf8 {
                    let c = char::from_u32(value).filter(|_| value <= 2047)?;
                    out.extend_from_slice(c.encode_utf8(&mut [0; 4]).as_bytes());
                } else {
                    out.push(u8::try_from(value).ok()?);
                }
            }
            Some(out)
        }
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
    session: crate::backend::Session,
    /// The agent's own pane.
    agent: Painted,
    /// The companion shell's, independent of the agent's in every respect —
    /// which slot it sits in, how big it is, and whether it is on screen at all.
    shell: Painted,
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
    fn parser(&self) -> Option<&'a Arc<std::sync::Mutex<crate::backend::SessionParser>>> {
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
            (true, None) => Err(anyhow::anyhow!("this session has no companion shell")),
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
    fn wired(&self) -> Option<&'a crate::backend::pane::WiredPane> {
        if self.shell {
            return self.live.session.shell_pane.as_deref();
        }
        Some(&*self.live.session)
    }

    /// Whether the parser holds this pane's grid. A pane that does not exist
    /// has nothing to hold, which is the same as holding it.
    fn is_resident(&self) -> bool {
        self.wired()
            .is_none_or(crate::backend::pane::WiredPane::is_resident)
    }

    /// How much output this pane's parser has taken — see
    /// [`crate::backend::pane::WiredPane::output_seq`].
    fn output_seq(&self) -> Option<u64> {
        Some(self.wired()?.output_seq())
    }

    fn output_seq_cell(&self) -> Option<&'a std::sync::atomic::AtomicU64> {
        Some(self.wired()?.output_seq_cell())
    }
}

/// One surface's extracted rows and the output stamp they were read at.
type CachedRows = (u64, std::rc::Rc<Vec<String>>);

/// Owns every live terminal, keyed by session id.
pub struct Terminals {
    /// The process's one registry, built at its composition root and shared
    /// with the command bus: attach and every lifecycle command drive the same
    /// backends.
    backends: std::sync::Arc<crate::backend::BackendRegistry>,
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
    /// Last-known activity/notification per session. Cached because the read is
    /// generation-gated: an unchanged session reports nothing, so the previous
    /// value has to be kept somewhere.
    meta: HashMap<String, AgentMeta>,
    /// Moves only when [`Self::meta`] actually writes or drops an entry.
    meta_version: u64,
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
    shelling: HashMap<String, shells::Shelling>,
    shelled: (Sender<shells::Shelled>, Receiver<shells::Shelled>),
    shell_results: Vec<ShellResult>,
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
    /// Lines of scrollback each pane's grid keeps: `settings.toml`'s
    /// `scrollback_lines`, as the settings this was built with say.
    scrollback: usize,
    /// What a local tmux pane is viewed through when `settings.toml`'s
    /// `stream_panes` is on ([`Self::view_through`]).
    stream_view: Option<stream_pane::View>,
    /// The `fleet` binary a Fleet session's pane is attached through
    /// ([`Self::attach_fleet_through`]).
    fleet: Option<PathBuf>,
    /// What stream panes say about input they dropped, for the interface
    /// to report ([`Self::take_input_notices`]).
    input_notices: (Sender<String>, Receiver<String>),
    /// The pane whose OSC 52 writes are accepted — the focused surface's, as of
    /// the last [`Self::take_app_copy`] — and the last focus token handed out.
    app_copy_focus: Option<std::sync::Arc<crate::backend::pane::AppCopy>>,
    app_copy_tokens: u64,
    /// The surface [`Self::focus`] last handed its size, and the pane behind
    /// it — the edge it acts on.
    size_focus: Option<(String, String)>,
    /// Attaches in flight and attaches that failed — see [`attach::Attach`].
    attach: attach::Attach,
    /// What each backend's window list said — see [`discovery::Discovery`].
    discovery: discovery::Discovery,
}

impl Terminals {
    pub fn set_agents(&mut self, agents: crate::session::AgentRegistry) {
        self.agents = agents;
    }

    /// Open each local tmux pane through `view`, a terminal stream, from now
    /// on. A stream pane keeps its grid while hidden: it comes back only
    /// through its stream.
    pub fn view_through(&mut self, view: stream_pane::View) {
        self.stream_view = Some(view);
        self.keep_hidden = None;
    }

    /// What stream panes said since the last call about input they dropped
    /// (`input dropped: <session> isn't reading`), for the interface to
    /// report where it reports a command's error: a notice in the pane's
    /// own grid would be painted over by a full-screen agent.
    pub fn take_input_notices(&self) -> Vec<String> {
        self.input_notices.1.try_iter().collect()
    }

    /// Open each Fleet session's pane through the terminal stream `fleet
    /// attach` names, `program` being the `fleet` binary (ADR-41). A stream
    /// pane keeps its grid while hidden, as [`Self::view_through`]'s do.
    pub fn attach_fleet_through(&mut self, program: PathBuf) {
        self.fleet = Some(program);
        self.keep_hidden = None;
    }

    /// Terminals over the process's registry. How that set is assembled — and
    /// why nothing is readied when it is — is the composition root's, not the
    /// kernel's; the hosts are the same cached read of `hosts.toml` it was
    /// built from. `settings` are the ones the process loaded: what a pane keeps
    /// and how long a hidden one stays resident are fixed for its lifetime.
    pub fn with_registry(
        backends: std::sync::Arc<crate::backend::BackendRegistry>,
        settings: &crate::session::settings::Settings,
    ) -> Self {
        let hosts = crate::config::hosts::cached_registry().0.clone();

        Self {
            backends,
            hosts,
            agents: crate::agent::agent_config::load_or_seed(),
            live: HashMap::new(),
            ready: RefCell::new(std::collections::HashSet::new()),
            meta: HashMap::new(),
            meta_version: 0,
            printing: std::collections::HashSet::new(),
            printing_version: 0,
            shelling: HashMap::new(),
            shelled: std::sync::mpsc::channel(),
            shell_results: Vec::new(),
            attach: attach::Attach::new(),
            discovery: discovery::Discovery::new(),
            runtime: tokio::runtime::Handle::try_current().ok(),
            programs: HashMap::new(),
            program_transitions: Vec::new(),
            rows_cache: RefCell::new(HashMap::new()),
            keep_hidden: match settings.hidden_terminal_secs {
                0 => None,
                secs => Some(std::time::Duration::from_secs(secs)),
            },
            scrollback: settings.scrollback_lines,
            stream_view: None,
            fleet: None,
            input_notices: std::sync::mpsc::channel(),
            app_copy_focus: None,
            app_copy_tokens: 0,
            size_focus: None,
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
    /// (and psmux spawns from before they learned theirs). A successful name-resolved
    /// adoption is queued for the loop to persist, so it happens once per row.
    pub fn sync(&mut self, snapshot: &Snapshot, rows: u16, cols: u16) {
        self.collect_discovered();
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
        let unresolved: Vec<(&str, String)> = snapshot
            .sessions
            .iter()
            .filter(|row| {
                !self.live.contains_key(&row.id) && !self.attach.attaching.contains_key(&row.id)
            })
            // A Fleet session has no window to list: its pane is the stream
            // `fleet attach` names.
            .filter(|row| row.backend != fleet::BACKEND)
            .map(|row| (row.id.as_str(), self.key(&row.backend)))
            .collect();

        // Stamp each row with where its backend's survey count stood when it first
        // showed up unresolved. A row that has stopped waiting forgets its stamp,
        // so a session that is deleted and restored is judged afresh.
        let still_waiting: std::collections::HashSet<&str> =
            unresolved.iter().map(|(id, _)| *id).collect();
        self.discovery
            .waiting_since
            .retain(|id, _| still_waiting.contains(id.as_str()));
        for (id, backend) in &unresolved {
            let seen_at = self.discovery.surveys.get(backend).copied().unwrap_or(0);
            self.discovery
                .waiting_since
                .entry((*id).to_string())
                .or_insert(seen_at);
        }

        let mut waiting: Vec<String> = unresolved
            .iter()
            .map(|(_, backend)| backend.clone())
            .collect();
        waiting.extend(snapshot.sessions.iter().filter_map(|row| {
            self.live.get(&row.id)?;
            self.backend_for(&row.backend)
                .filter(|backend| backend.capabilities().needs_liveness_poll)
                .map(|_| self.key(&row.backend))
        }));
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
        self.collect_shells(snapshot);
        self.attach
            .live_since
            .retain(|id, _| present.contains(id.as_str()));
        let failures = self.attach.failed.len();
        self.attach
            .failed
            .retain(|id, _| present.contains(id.as_str()));
        if self.attach.failed.len() != failures {
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
    /// give it back ([`crate::backend::pane::WiredPane::evict`]).
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

    /// The key a row's per-backend state is kept under: the route it is
    /// served by, qualified — so a legacy `ssh:box` row and a new
    /// `ssh:box:tmux` one share one survey, one connection and one backoff,
    /// and the key is the serving backend's own name.
    fn key(&self, backend: &str) -> String {
        match crate::session::Route::parse(backend) {
            Ok(route) => self.hosts.qualify(&route).format(),
            Err(_) => backend.to_string(),
        }
    }

    /// The backend serving a row's route (or a [`Self::key`]), if one is
    /// registered for it. Never another: an unqualified route is settled the
    /// way it was written, and one nothing serves has no backend.
    fn backend_for(&self, backend: &str) -> Option<Arc<dyn crate::backend::SessionBackend>> {
        let route = crate::session::Route::parse(backend).ok()?;
        self.backends.get(&self.hosts.qualify(&route)).cloned()
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
        self.attach.live_since.remove(session);
        self.discovery.discovery_due.clear();
        if self.attach.failed.remove(session).is_some() {
            self.mark_failures_changed();
        }
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

    /// The OSC 52 writes the focused pane's app made since the last call,
    /// oldest first — and the only pane whose writes are kept at all.
    ///
    /// `focused` is the surface raw input goes to, or `None` when no session
    /// pane is taking the keys (a modal or a field has them, say). Focus moving to another pane, or to none, revokes
    /// the old pane's gate before anything is read, so a background app — local
    /// or on a remote host — never writes the user's clipboard, and a write it
    /// made while hidden is not released when it is brought forward (see
    /// [`crate::backend::pane::AppCopy`]). A program pane is not a session pane
    /// and resolves to none here.
    pub fn take_app_copy(
        &mut self,
        focused: Option<&str>,
    ) -> Vec<crate::backend::pane::AppCopyRequest> {
        let current = focused
            .and_then(|surface| self.pane(surface))
            .and_then(|pane| pane.wired())
            .map(|wired| wired.app_copy());
        let unchanged = match (&self.app_copy_focus, current) {
            (Some(held), Some(now)) => std::sync::Arc::ptr_eq(held, now),
            (None, None) => true,
            _ => false,
        };
        if !unchanged {
            let current = current.cloned();
            if let Some(old) = self.app_copy_focus.take() {
                old.blur();
            }
            if let Some(new) = &current {
                self.app_copy_tokens += 1;
                new.focus(self.app_copy_tokens);
            }
            self.app_copy_focus = current;
        }
        self.app_copy_focus
            .as_ref()
            .map(|copy| copy.take())
            .unwrap_or_default()
    }

    /// Hand the focused surface its pane's size, once per focus gained.
    ///
    /// On a pane several instances show, a paint resizes only a pane that is
    /// this instance's to size, and input is what hands the size over
    /// (`WiredPane::send_input`). Focus says the same thing input does — the
    /// user is at this instance — so a pane focused here takes the size of the
    /// rect it was just painted into, and is not left at another instance's
    /// size until the first keystroke. Called after the paint, so that rect is
    /// this frame's.
    ///
    /// On the edge only: keeping the focus is not gaining it, or two instances
    /// focused on one pane would take it from each other every frame. A pane
    /// replaced under the focused surface is a new pane gaining it. `None`
    /// (focus on a pane with no terminal, or on a modal) clears the edge, so
    /// coming back is focusing again. A pane already at this size costs an
    /// atomic comparison and sends nothing, and one nobody else shares was
    /// already sized by its paint.
    pub fn focus(&mut self, surface: Option<&str>) {
        let Some(surface) = surface else {
            self.size_focus = None;
            return;
        };
        let wired = match self.program_key(surface) {
            Some(key) => self.programs.get(key).map(|slot| &*slot.pane),
            None => self.pane(surface).and_then(|pane| pane.wired()),
        };
        // A surface with no pane yet — focused before it attached — is asked
        // again next time rather than marked as handed its size.
        let Some(wired) = wired else {
            return;
        };
        // Keyed on the pane as well as the surface: a restart replaces the pane
        // under a name that keeps the focus, and the new pane has never had it.
        if self
            .size_focus
            .as_ref()
            .is_some_and(|(held, pane)| held == surface && pane == wired.backend_id())
        {
            return;
        }
        wired.claim_size();
        self.size_focus = Some((surface.to_string(), wired.backend_id().to_string()));
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
        &Arc<std::sync::Mutex<crate::backend::SessionParser>>,
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
        let (rows, cols) = (rows.get(), cols.get());
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
        let scrollback = self.scrollback;
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
                                .map(|snapshot| crate::backend::pane::parser_from_snapshot(&snapshot, scrollback))
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

    /// Where a session's companion shell opens: its workspace, as Fleet
    /// reports it.
    pub fn launch_cwd(&self, row: &super::snapshot::SessionRow) -> Option<PathBuf> {
        row.cwd.clone()
    }

    /// The session behind `surface` and the host it runs on, for a session on
    /// an `ssh:`/`wsl:` backend; `None` for a local one or an unknown surface.
    ///
    /// The companion shell shares its session's host, so either surface of a
    /// remote session answers.
    pub fn remote_host_of(
        &self,
        surface: &str,
        snapshot: &Snapshot,
    ) -> Option<(String, crate::session::HostDef)> {
        let (id, _) = split_surface(surface);
        let row = snapshot.session(id)?;
        let host = self.hosts.get(row.remote_host.as_deref()?)?;
        Some((row.id.clone(), host.clone()))
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
            .map(|pane| pane.backend_id().to_string())
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

    /// Drain every backend's queued remote hook reports.
    ///
    /// The events are `(backend, pane id, state)` — pane ids collide across
    /// hosts, so the backend is part of the identity, not decoration. The
    /// backend is named by the qualified route it serves. A backend with no
    /// status channel returns nothing.
    pub fn drain_hook_events(&self) -> Vec<(String, String, String)> {
        self.backends
            .all_backends()
            .flat_map(|(route, backend)| {
                let name = route.format();
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
    ) -> Option<(Arc<dyn crate::backend::SessionBackend>, String)> {
        self.live
            .get(session)
            .map(|live| live.session.backend_handle())
    }

    /// When the pane behind a surface name last produced output, as epoch
    /// milliseconds — moved as well when its grid is rebuilt, since that
    /// changes the cells without the pane printing
    /// ([`crate::backend::pane::WiredPane::content_stamp`]). `None` when
    /// nothing is attached there. Compare it; do not read it as a time.
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
        self.pane(surface)?.content_stamp()
    }

    /// The [`output_seq`](crate::backend::pane::WiredPane::output_seq) of the
    /// pane behind a surface name, `<id>#shell` and program surfaces included.
    /// `None` when nothing is attached there.
    ///
    /// Exact where [`Self::output_stamp`] is not: it moves once per chunk the
    /// parser took, and only after taking it. What an echo is waited on by.
    pub fn output_seq(&self, surface: &str) -> Option<u64> {
        if let Some(key) = self.program_key(surface) {
            return self.programs.get(key).map(|slot| slot.pane.output_seq());
        }
        self.pane(surface)?.output_seq()
    }

    /// The cell behind [`Self::output_seq`] for a surface, which is how a
    /// wake-up is armed for that pane alone (`backend::output_wake`).
    pub fn output_seq_cell(&self, surface: &str) -> Option<&std::sync::atomic::AtomicU64> {
        if let Some(key) = self.program_key(surface) {
            return self
                .programs
                .get(key)
                .map(|slot| slot.pane.output_seq_cell());
        }
        self.pane(surface)?.output_seq_cell()
    }

    /// Output generation of every attached pane, including hidden search sources.
    pub fn output_generation(&self) -> u64 {
        let sessions = self.live.values().fold(0u64, |acc, live| {
            acc.wrapping_add(live.session.output_seq()).wrapping_add(
                live.session
                    .shell_pane
                    .as_ref()
                    .map_or(0, |pane| pane.output_seq()),
            )
        });
        self.programs.values().fold(sessions, |acc, slot| {
            acc.wrapping_add(slot.pane.output_seq())
        })
    }

    /// Redraw generation of the surfaces painted in the last frame.
    ///
    /// Hidden panes still parse output, but their cells cannot change the
    /// screen. A newly shown surface restores its grid through the normal
    /// paint path; input and layout changes already owe that first frame.
    pub fn visible_output_generation(&self) -> u64 {
        let sessions = self.live.values().fold(0u64, |acc, live| {
            let agent = if live.agent.on_screen() {
                live.session.output_seq()
            } else {
                0
            };
            let shell = if live.shell.on_screen() {
                live.session
                    .shell_pane
                    .as_ref()
                    .map_or(0, |pane| pane.output_seq())
            } else {
                0
            };
            acc.wrapping_add(agent).wrapping_add(shell)
        });
        self.programs
            .values()
            .filter(|slot| slot.painted.on_screen())
            .fold(sessions, |acc, slot| {
                acc.wrapping_add(slot.pane.output_seq())
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
}

/// What an agent reported about itself over its own terminal.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AgentMeta {
    /// The OSC window title, which agents use as a live activity line.
    pub activity: Option<String>,
    /// The message from the most recent attention notification.
    pub notification: Option<String>,
}

/// property this module's sharing used to break (#1220).
/// The agent pane and its companion shell are independent surfaces — the
#[cfg(test)]
mod decoupling;

#[cfg(test)]
mod tests {
    use super::attach::{Attached, ATTACH_RETRY_INTERVAL};
    use super::discovery::{
        discover_windows, discovery_interval, due_now, Discovered, DISCOVERY_INTERVAL,
        REMOTE_DISCOVERY_INTERVAL,
    };
    use super::paint::{grid_top, paint_sized_elsewhere, screen_widget, FromRow};
    use super::*;
    use crate::kernel::snapshot::SessionRow;

    #[test]
    fn repeated_offline_attach_results_reuse_the_interface() {
        use crate::kernel::host::{Epoch, LuaHost, Published, RenderContext};

        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("plugins")).unwrap();
        std::fs::write(
            dir.path().join("plugins/probe.lua"),
            r#"return { name = "probe", slot = "center", pure = true,
                render = function()
                    return { text = thurbox.sessions[1].attach_error or "connected" }
                end }"#,
        )
        .unwrap();
        let host = LuaHost::new(dir.path());
        assert!(host.error.is_none(), "{:?}", host.error);
        let mut terminals = Terminals::with_registry(
            Arc::new(crate::backend::registry::inert()),
            &crate::session::settings::Settings::default(),
        );
        terminals.hosts = Default::default();
        let snapshot = Snapshot {
            sessions: (0..10)
                .map(|i| row(&format!("offline-{i}"), "ssh:offline:tmux", Some("%1")))
                .collect(),
            ..Default::default()
        };
        let themes = crate::kernel::theme::Themes::load(None);
        let registry = crate::kernel::registry::Registry::default();
        let diffs = crate::kernel::diff::DiffStore::new();
        let repos = crate::kernel::repos::RepoStore::with_hosts(Default::default());
        let render = |terminals: &Terminals| {
            host.publish(&Published {
                epoch: Epoch {
                    failed: terminals.failed_version(),
                    ..Default::default()
                },
                snapshot: &snapshot,
                attach_errors: &terminals.failures(),
                inflight: &[],
                themes: &themes,
                registry: &registry,
                diffs: &diffs,
                links: &Default::default(),
                search: None,
                meta: &Default::default(),
                metrics: &Default::default(),
                status_rows: 0,
                can_open: false,
                inventory: &[],
                ui_dir: "ui",
                settings: &Default::default(),
                repos: &repos,
                wants: &Default::default(),
                focus: None,
                selection: None,
                hovered: None,
                printing: &Default::default(),
            })
            .unwrap();
            format!(
                "{:?}",
                host.render(
                    0,
                    RenderContext {
                        width: 80,
                        height: 24,
                        focused: false,
                        elapsed: 0.0,
                        frame: 0,
                    }
                )
                .unwrap()
                .node
            )
        };
        let complete = |terminals: &mut Terminals, id: &str, message: &str| {
            terminals
                .attach
                .attached
                .0
                .send(Attached {
                    session: id.into(),
                    pane: "%1".into(),
                    backend: "ssh:offline:tmux".into(),
                    readied: false,
                    via_name: false,
                    session_handle: Err(message.into()),
                })
                .unwrap();
            terminals.sync(&snapshot, 24, 80);
        };
        for row in &snapshot.sessions {
            complete(&mut terminals, &row.id, "host unreachable");
        }
        assert!(render(&terminals).contains("host unreachable"));
        let version = terminals.failed_version();
        let skipped = host.skipped_renders();
        let reused = host.reused_groups();
        for _ in 0..3 {
            for row in &snapshot.sessions {
                // Expire each retry without sleeping: completed worker results
                // must refresh the private timer without invalidating Lua.
                terminals.attach.failed.get_mut(&row.id).unwrap().at -= ATTACH_RETRY_INTERVAL;
                complete(&mut terminals, &row.id, "host unreachable");
                assert!(render(&terminals).contains("host unreachable"));
            }
        }
        println!("30 repeated offline results: {} failure invalidations, {} cached renders, {} reused groups",
            terminals.failed_version() - version, host.skipped_renders() - skipped,
            host.reused_groups() - reused);
        assert_eq!(host.skipped_renders() - skipped, 30);
        assert_eq!(terminals.failed_version(), version);

        complete(&mut terminals, "offline-0", "permission denied");
        assert!(render(&terminals).contains("permission denied"));
        assert_eq!(terminals.failed_version(), version + 1);
        terminals.forget("offline-0");
        assert!(render(&terminals).contains("connected"));
        assert_eq!(terminals.failed_version(), version + 2);
    }

    #[test]
    fn retry_bookkeeping_is_private_to_the_terminal_store() {
        let mut terminals = Terminals::with_registry(
            Arc::new(crate::backend::registry::inert()),
            &crate::session::settings::Settings::default(),
        );
        terminals.fail("a", Some("%1".into()), "host unreachable".into());
        let version = terminals.failed_version();
        terminals.attach.failed.get_mut("a").unwrap().at -= ATTACH_RETRY_INTERVAL;
        let retry_started = std::time::Instant::now();
        terminals.fail("a", Some("%2".into()), "host unreachable".into());
        assert_eq!(terminals.failed_version(), version);
        let failure = &terminals.attach.failed["a"];
        assert_eq!(failure.pane.as_deref(), Some("%2"));
        assert!(
            failure.at >= retry_started,
            "a retry must restart its cooldown"
        );
        terminals.fail("b", Some("%2".into()), "host unreachable".into());
        assert_eq!(terminals.failed_version(), version + 1);
        terminals.sync(&Snapshot::default(), 24, 80);
        assert!(terminals.failures().is_empty());
        assert_eq!(terminals.failed_version(), version + 2);
    }

    /// A grid taller than its rect shows its bottom rows — where the newest
    /// output and the prompt are — and a selection over them copies those rows.
    #[test]
    fn a_grid_taller_than_its_rect_shows_its_bottom() {
        use ratatui::widgets::Widget;
        let mut parser = vt100::Parser::new(6.try_into().unwrap(), 4.try_into().unwrap(), 0);
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
        let mut parser = vt100::Parser::new(2.try_into().unwrap(), 8.try_into().unwrap(), 0);
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
        // `?1005` is the default encoding with each field a UTF-8 character,
        // so a small coordinate reads the same and a large one takes two bytes
        // instead of running out at 223.
        assert_eq!(
            mouse_report(E::Utf8, 64, 12, 3, true).expect("utf8"),
            b"\x1b[M`,#"
        );
        assert_eq!(
            mouse_report(E::Utf8, 0, 300, 3, false).expect("utf8"),
            "\x1b[M#\u{14c}#".as_bytes()
        );
    }

    pub(super) fn row(id: &str, backend: &str, backend_id: Option<&str>) -> SessionRow {
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
            key: None,
            reports_as: None,
            detected_agent: None,
            shell_backend_id: None,
            member_dirs: Vec::new(),
            meta: Default::default(),
        }
    }

    struct FakeLivenessBackend(std::sync::atomic::AtomicU8);

    impl crate::backend::PaneStream for FakeLivenessBackend {
        fn spawn(
            &self,
            _: &str,
            _: &str,
            _: &[String],
            _: Option<&std::path::Path>,
            _: &HashMap<String, String>,
            _: u16,
            _: u16,
        ) -> crate::backend::contract::BackendResult<crate::backend::SpawnedSession> {
            unreachable!()
        }

        fn adopt(
            &self,
            _: &str,
            _: u16,
            _: u16,
            _: Option<Vec<u8>>,
        ) -> crate::backend::contract::BackendResult<crate::backend::AdoptedSession> {
            unreachable!()
        }

        fn resize(&self, _: &str, _: u16, _: u16) -> crate::backend::contract::BackendResult<()> {
            Ok(())
        }

        fn detach(&self, _: &str) -> crate::backend::contract::BackendResult<()> {
            Ok(())
        }
    }

    impl crate::backend::WindowLifecycle for FakeLivenessBackend {
        fn pane_state(
            &self,
            _: &str,
        ) -> crate::backend::contract::BackendResult<crate::backend::PaneState> {
            Err(anyhow::anyhow!("this stub has no panes to read").into())
        }

        fn pane_path(&self, _: &str) -> crate::backend::contract::BackendResult<Option<String>> {
            Err(anyhow::anyhow!("this stub has no panes to read").into())
        }

        fn create_window(
            &self,
            _: &crate::backend::WindowSpec<'_>,
        ) -> crate::backend::contract::BackendResult<crate::backend::PaneId> {
            Err(anyhow::anyhow!(
                "{}: a stub opens no windows",
                crate::backend::SessionBackend::name(self)
            )
            .into())
        }

        fn locate(
            &self,
            _: crate::backend::Owner<'_>,
        ) -> crate::backend::contract::BackendResult<crate::backend::Placed> {
            Ok(crate::backend::Placed {
                agent: crate::backend::Located::Absent,
                shell: crate::backend::Located::Absent,
            })
        }

        fn rename_windows(
            &self,
            _: crate::backend::Owner<'_>,
            _: &str,
        ) -> crate::backend::contract::BackendResult<()> {
            Ok(())
        }

        fn stamp_window(
            &self,
            _: &str,
            _: &str,
            _: crate::backend::WindowRole,
        ) -> crate::backend::contract::BackendResult<()> {
            Ok(())
        }

        fn window_panes(
            &self,
            _: &str,
        ) -> crate::backend::contract::BackendResult<Vec<(String, bool)>> {
            Ok(Vec::new())
        }

        fn set_pane_retention(
            &self,
            _: &str,
            _: bool,
        ) -> crate::backend::contract::BackendResult<()> {
            Ok(())
        }

        fn shutdown(&self) {}

        fn default_shell(&self) -> String {
            "/bin/sh".to_string()
        }

        fn check_available(&self) -> crate::backend::contract::BackendResult<()> {
            Ok(())
        }

        fn ensure_ready(&self) -> crate::backend::contract::BackendResult<()> {
            Ok(())
        }

        fn discover(
            &self,
        ) -> crate::backend::contract::BackendResult<Vec<crate::backend::DiscoveredSession>>
        {
            match self.0.load(std::sync::atomic::Ordering::Relaxed) {
                0 => Ok(Vec::new()),
                1 => Err(crate::backend::BackendError::Unreachable(
                    "host unreachable".into(),
                )),
                3 => Err(crate::backend::BackendError::Refused(
                    "permission denied".into(),
                )),
                _ => Ok(vec![crate::backend::DiscoveredSession {
                    backend_id: "%1".into(),
                    name: "tb-demo".into(),
                    is_alive: false,
                    session: "a".into(),
                    role: crate::backend::WindowRole::Agent,
                }]),
            }
        }

        fn is_dead(&self, _: &str) -> crate::backend::contract::BackendResult<bool> {
            Ok(false)
        }

        fn kill(&self, _: &str) -> crate::backend::contract::BackendResult<()> {
            Ok(())
        }

        fn pane_pid(&self, _: &str) -> crate::backend::contract::BackendResult<Option<u32>> {
            Ok(None)
        }
    }

    impl crate::backend::PaneIo for FakeLivenessBackend {
        fn send_text(
            &self,
            _: &str,
            _: &str,
            _: bool,
        ) -> crate::backend::contract::BackendResult<()> {
            Err(anyhow::anyhow!("this stub has no panes to type into").into())
        }

        fn send_text_after(
            &self,
            _: &str,
            _: &str,
            _: std::time::Duration,
        ) -> crate::backend::contract::BackendResult<()> {
            Err(anyhow::anyhow!("this stub has no panes to type into").into())
        }

        fn send_key(
            &self,
            _: &str,
            _: &crate::backend::Key,
        ) -> crate::backend::contract::BackendResult<String> {
            Err(anyhow::anyhow!("this stub has no panes to type into").into())
        }

        fn capture(
            &self,
            _: &str,
            _: u32,
            _: bool,
        ) -> crate::backend::contract::BackendResult<String> {
            Err(anyhow::anyhow!("this stub has no panes to read").into())
        }
    }

    impl crate::backend::StatusChannel for FakeLivenessBackend {
        fn hook_signal_command(&self) -> Option<String> {
            None
        }

        fn record_hook_state(
            &self,
            _: &str,
            _: &str,
        ) -> crate::backend::contract::BackendResult<()> {
            Err(anyhow::anyhow!("this stub has no status channel").into())
        }

        fn hook_states(&self) -> crate::backend::contract::BackendResult<Vec<(String, String)>> {
            Err(anyhow::anyhow!("this stub has no status channel").into())
        }

        fn take_hook_state_events(&self) -> Vec<(String, String)> {
            Vec::new()
        }

        fn ensure_heartbeat(
            &self,
            _: &std::path::Path,
            _: &[String],
            _: std::time::Duration,
        ) -> crate::backend::contract::BackendResult<()> {
            Err(anyhow::anyhow!("this stub keeps no heartbeat").into())
        }

        fn heartbeat_running(&self) -> crate::backend::contract::BackendResult<bool> {
            Ok(false)
        }

        fn stop_heartbeat(&self) -> crate::backend::contract::BackendResult<bool> {
            Ok(false)
        }
    }

    impl crate::backend::SessionBackend for FakeLivenessBackend {
        fn name(&self) -> &str {
            "fake"
        }
        fn capabilities(&self) -> crate::backend::Capabilities {
            crate::backend::Capabilities::NONE
        }
    }

    /// With `stream_panes` on, a session whose row names `local:tmux` (what a
    /// new local session stores) attaches through the view program.
    #[cfg(unix)]
    #[test]
    fn a_local_tmux_row_attaches_through_the_stream_view() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("viewed");
        let program = dir.path().join("runtime-mux");
        // A stand-in view: it notes how it ran and closes without a hello.
        std::fs::write(
            &program,
            format!("#!/bin/sh\nprintf '%s ' \"$@\" > '{}'\n", marker.display()),
        )
        .unwrap();
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
        let backend: std::sync::Arc<dyn crate::backend::SessionBackend> =
            std::sync::Arc::new(FakeLivenessBackend(std::sync::atomic::AtomicU8::new(0)));
        let mut terminals = Terminals::with_registry(
            std::sync::Arc::new(crate::backend::registry::inert()),
            &crate::session::settings::Settings::default(),
        );
        let route = crate::session::Route::local(Some(crate::session::Multiplexer::Tmux));
        std::sync::Arc::get_mut(&mut terminals.backends)
            .expect("unshared registry")
            .register(route.clone(), backend);
        terminals.view_through(stream_pane::View {
            program,
            socket: "thurbox-test".into(),
            scrollback: 1000,
        });
        let row = row("a", &route.format(), Some("%1"));
        assert_eq!(row.backend, "local:tmux");
        terminals.start_attach(&row, "%1".into(), false, 24, 80);
        // The control-mode path would reach the fake's `adopt`, which panics.
        let _ = terminals
            .attach
            .attached
            .1
            .recv_timeout(std::time::Duration::from_secs(10));
        let argv = std::fs::read_to_string(&marker)
            .expect("the attach did not go through the stream view");
        assert!(argv.contains("view --socket thurbox-test"), "{argv}");
        assert!(argv.contains("--pane %1"), "{argv}");
    }

    #[test]
    fn fake_backend_only_relaunches_confirmed_missing_once() {
        use std::sync::atomic::Ordering;
        let fake = std::sync::Arc::new(FakeLivenessBackend(std::sync::atomic::AtomicU8::new(0)));
        let backend: std::sync::Arc<dyn crate::backend::SessionBackend> = fake.clone();
        let mut terminals = Terminals::with_registry(
            std::sync::Arc::new(crate::backend::registry::inert()),
            &crate::session::settings::Settings::default(),
        );
        let route = crate::session::Route::remote(
            crate::session::Via::Ssh,
            "fake",
            Some(crate::session::Multiplexer::Tmux),
        );
        std::sync::Arc::get_mut(&mut terminals.backends)
            .expect("unshared registry")
            .register(route.clone(), backend.clone());
        terminals.discovery.waiting_since.insert("a".into(), 0);
        let rows = snapshot(vec![row("a", &route.format(), Some("%1"))]);
        let mut notified = std::collections::HashSet::new();

        for mode in [0, 0, 1, 3, 2] {
            fake.0.store(mode, Ordering::Relaxed);
            let observed = discover_windows(&backend, route.format(), true);
            terminals.discovery.discovered_rx.0.send(observed).unwrap();
            terminals.collect_discovered();
            let missing = terminals.missing_agents(&rows);
            match mode {
                0 => {
                    assert_eq!(missing, ["a"]);
                    for id in missing {
                        notified.insert(id);
                    }
                    assert_eq!(notified.len(), 1, "duplicate notification");
                }
                1 => {
                    assert_eq!(
                        terminals.liveness(&rows.sessions[0]),
                        crate::backend::BackendLiveness::Unreachable
                    );
                    assert!(missing.is_empty());
                }
                3 => {
                    assert_eq!(
                        terminals.liveness(&rows.sessions[0]),
                        crate::backend::BackendLiveness::Unknown
                    );
                    assert!(missing.is_empty());
                }
                _ => {
                    assert_eq!(
                        terminals.liveness(&rows.sessions[0]),
                        crate::backend::BackendLiveness::Exited
                    );
                    assert!(missing.is_empty());
                }
            }
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
        let mut terminals = Terminals::with_registry(
            std::sync::Arc::new(crate::backend::registry::inert()),
            &crate::session::settings::Settings::default(),
        );
        terminals.sync(&snapshot(vec![row("a", "local-tmux", None)]), 24, 80);
        assert!(!terminals.is_attached("a"));
        assert_eq!(terminals.failure("a"), Some("session has no pane yet"));
        // And the attempt is recorded as "there was no pane", which is what lets
        // a window appearing later be picked up instead of latched out.
        assert_eq!(terminals.attach.failed["a"].pane, None);
    }

    /// Stands in for the local backend and refuses to open, so an attach
    /// started through it goes no further than the attempt.
    struct RefusingLocal;

    impl crate::backend::PaneStream for RefusingLocal {
        fn spawn(
            &self,
            _: &str,
            _: &str,
            _: &[String],
            _: Option<&std::path::Path>,
            _: &HashMap<String, String>,
            _: u16,
            _: u16,
        ) -> crate::backend::contract::BackendResult<crate::backend::SpawnedSession> {
            unreachable!()
        }

        fn adopt(
            &self,
            _: &str,
            _: u16,
            _: u16,
            _: Option<Vec<u8>>,
        ) -> crate::backend::contract::BackendResult<crate::backend::AdoptedSession> {
            unreachable!()
        }

        fn resize(&self, _: &str, _: u16, _: u16) -> crate::backend::contract::BackendResult<()> {
            Ok(())
        }

        fn detach(&self, _: &str) -> crate::backend::contract::BackendResult<()> {
            Ok(())
        }
    }

    impl crate::backend::WindowLifecycle for RefusingLocal {
        fn pane_state(
            &self,
            _: &str,
        ) -> crate::backend::contract::BackendResult<crate::backend::PaneState> {
            Err(anyhow::anyhow!("this stub has no panes to read").into())
        }

        fn pane_path(&self, _: &str) -> crate::backend::contract::BackendResult<Option<String>> {
            Err(anyhow::anyhow!("this stub has no panes to read").into())
        }

        fn create_window(
            &self,
            _: &crate::backend::WindowSpec<'_>,
        ) -> crate::backend::contract::BackendResult<crate::backend::PaneId> {
            Err(anyhow::anyhow!(
                "{}: a stub opens no windows",
                crate::backend::SessionBackend::name(self)
            )
            .into())
        }

        fn locate(
            &self,
            _: crate::backend::Owner<'_>,
        ) -> crate::backend::contract::BackendResult<crate::backend::Placed> {
            Ok(crate::backend::Placed {
                agent: crate::backend::Located::Absent,
                shell: crate::backend::Located::Absent,
            })
        }

        fn rename_windows(
            &self,
            _: crate::backend::Owner<'_>,
            _: &str,
        ) -> crate::backend::contract::BackendResult<()> {
            Ok(())
        }

        fn stamp_window(
            &self,
            _: &str,
            _: &str,
            _: crate::backend::WindowRole,
        ) -> crate::backend::contract::BackendResult<()> {
            Ok(())
        }

        fn window_panes(
            &self,
            _: &str,
        ) -> crate::backend::contract::BackendResult<Vec<(String, bool)>> {
            Ok(Vec::new())
        }

        fn set_pane_retention(
            &self,
            _: &str,
            _: bool,
        ) -> crate::backend::contract::BackendResult<()> {
            Ok(())
        }

        fn shutdown(&self) {}

        fn default_shell(&self) -> String {
            "/bin/sh".to_string()
        }

        fn check_available(&self) -> crate::backend::contract::BackendResult<()> {
            Ok(())
        }

        fn ensure_ready(&self) -> crate::backend::contract::BackendResult<()> {
            Err(anyhow::anyhow!("not opened in a unit test").into())
        }

        fn discover(
            &self,
        ) -> crate::backend::contract::BackendResult<Vec<crate::backend::DiscoveredSession>>
        {
            Ok(Vec::new())
        }

        fn is_dead(&self, _: &str) -> crate::backend::contract::BackendResult<bool> {
            Ok(false)
        }

        fn kill(&self, _: &str) -> crate::backend::contract::BackendResult<()> {
            Ok(())
        }

        fn pane_pid(&self, _: &str) -> crate::backend::contract::BackendResult<Option<u32>> {
            Ok(None)
        }
    }

    impl crate::backend::PaneIo for RefusingLocal {
        fn send_text(
            &self,
            _: &str,
            _: &str,
            _: bool,
        ) -> crate::backend::contract::BackendResult<()> {
            Err(anyhow::anyhow!("this stub has no panes to type into").into())
        }

        fn send_text_after(
            &self,
            _: &str,
            _: &str,
            _: std::time::Duration,
        ) -> crate::backend::contract::BackendResult<()> {
            Err(anyhow::anyhow!("this stub has no panes to type into").into())
        }

        fn send_key(
            &self,
            _: &str,
            _: &crate::backend::Key,
        ) -> crate::backend::contract::BackendResult<String> {
            Err(anyhow::anyhow!("this stub has no panes to type into").into())
        }

        fn capture(
            &self,
            _: &str,
            _: u32,
            _: bool,
        ) -> crate::backend::contract::BackendResult<String> {
            Err(anyhow::anyhow!("this stub has no panes to read").into())
        }
    }

    impl crate::backend::StatusChannel for RefusingLocal {
        fn hook_signal_command(&self) -> Option<String> {
            None
        }

        fn record_hook_state(
            &self,
            _: &str,
            _: &str,
        ) -> crate::backend::contract::BackendResult<()> {
            Err(anyhow::anyhow!("this stub has no status channel").into())
        }

        fn hook_states(&self) -> crate::backend::contract::BackendResult<Vec<(String, String)>> {
            Err(anyhow::anyhow!("this stub has no status channel").into())
        }

        fn take_hook_state_events(&self) -> Vec<(String, String)> {
            Vec::new()
        }

        fn ensure_heartbeat(
            &self,
            _: &std::path::Path,
            _: &[String],
            _: std::time::Duration,
        ) -> crate::backend::contract::BackendResult<()> {
            Err(anyhow::anyhow!("this stub keeps no heartbeat").into())
        }

        fn heartbeat_running(&self) -> crate::backend::contract::BackendResult<bool> {
            Ok(false)
        }

        fn stop_heartbeat(&self) -> crate::backend::contract::BackendResult<bool> {
            Ok(false)
        }
    }

    impl crate::backend::SessionBackend for RefusingLocal {
        fn name(&self) -> &str {
            "local-tmux"
        }
        fn capabilities(&self) -> crate::backend::Capabilities {
            crate::backend::Capabilities::NONE
        }
    }

    /// A row stored as `tmux` — the column's old default — is a local session,
    /// so it attaches through the local backend like a `local-tmux` row rather
    /// than failing to find a backend of that name.
    #[test]
    fn a_legacy_tmux_row_attaches_through_the_local_backend() {
        let mut terminals = Terminals::with_registry(
            std::sync::Arc::new(crate::backend::registry::inert()),
            &crate::session::settings::Settings::default(),
        );
        std::sync::Arc::get_mut(&mut terminals.backends)
            .expect("unshared registry")
            .register(
                crate::session::Route::local(Some(crate::session::Multiplexer::platform_default())),
                std::sync::Arc::new(RefusingLocal),
            );
        terminals.sync(&snapshot(vec![row("a", "tmux", Some("%1"))]), 24, 80);
        assert_eq!(terminals.failure("a"), None);
        // Surveyed on the local backend, the one a `local-tmux` row shares: the
        // attach waits on that survey, as every first attach on a backend does.
        assert_eq!(terminals.key("tmux"), terminals.key("local-tmux"));
        assert!(
            terminals
                .discovery
                .discovering
                .contains(&terminals.key("tmux")),
            "the local backend was not asked"
        );
    }

    #[test]
    fn an_unknown_backend_is_reported_not_panicked() {
        let mut terminals = Terminals::with_registry(
            std::sync::Arc::new(crate::backend::registry::inert()),
            &crate::session::settings::Settings::default(),
        );
        terminals.sync(&snapshot(vec![row("a", "ssh:nowhere", Some("%1"))]), 24, 80);
        assert!(!terminals.is_attached("a"));
        let message = terminals.failure("a").unwrap_or_default().to_string();
        assert!(message.contains("no backend"), "{message}");
        assert_eq!(
            terminals.attach.failed["a"].pane.as_deref(),
            Some("%1"),
            "the pane that failed is remembered, so the same one is not retried"
        );
    }

    /// Seed a completed survey of `backend`: one listing, having found `windows`
    /// — each an unstamped window, the shape every window had before ADR-25 and
    /// the one these tests are about (a stamped window is unambiguous by
    /// construction).
    fn surveyed(terminals: &mut Terminals, backend: &str, windows: &[(&str, &[&str])]) {
        let backend = &terminals.key(backend);
        terminals.discovery.surveys.insert(backend.to_string(), 1);
        let listing = windows.iter().flat_map(|(window, panes)| {
            panes.iter().map(|pane| crate::backend::DiscoveredSession {
                backend_id: (*pane).to_string(),
                name: (*window).to_string(),
                is_alive: true,
                session: String::new(),
                role: crate::backend::WindowRole::Agent,
            })
        });
        terminals.discovery.discovered.insert(
            backend.to_string(),
            crate::backend::identity::WindowIndex::from_listing(listing),
        );
    }

    /// A rebooted machine leaves every persisted pane id naming a pane that no
    /// longer exists. Retrying one fails on `resize-window` — `can't find pane` —
    /// once per retry interval for the life of the process, which is what this
    /// stops: the contradicted id is dropped, and the session is reported as
    /// missing its agent so it can be relaunched.
    #[test]
    fn a_pane_id_a_restarted_server_invalidated_is_dropped_rather_than_retried() {
        let mut terminals = Terminals::with_registry(
            std::sync::Arc::new(crate::backend::registry::inert()),
            &crate::session::settings::Settings::default(),
        );
        let snapshot = snapshot(vec![row("a", "local-tmux", Some("%822"))]);
        // The row was already waiting when the survey ran, so the survey speaks
        // for it — and it found no window of this session's.
        terminals.discovery.waiting_since.insert("a".to_string(), 0);
        surveyed(&mut terminals, "local-tmux", &[("tb-other", &["%1"])]);

        terminals.sync(&snapshot, 24, 80);

        assert_eq!(
            terminals.attach.failed["a"].pane, None,
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
        let mut terminals = Terminals::with_registry(
            std::sync::Arc::new(crate::backend::registry::inert()),
            &crate::session::settings::Settings::default(),
        );
        let row = row("a", "local-tmux", Some("%1"));
        terminals.discovery.waiting_since.insert("a".to_string(), 0);
        surveyed(&mut terminals, "local-tmux", &[("tb-other", &["%1"])]);

        assert!(terminals.pane_is_stale(&row, "%1"));
    }

    #[test]
    fn a_pane_id_the_survey_confirms_is_kept() {
        let mut terminals = Terminals::with_registry(
            std::sync::Arc::new(crate::backend::registry::inert()),
            &crate::session::settings::Settings::default(),
        );
        let row = row("a", "local-tmux", Some("%1"));
        terminals.discovery.waiting_since.insert("a".to_string(), 0);
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
        let mut terminals = Terminals::with_registry(
            std::sync::Arc::new(crate::backend::registry::inert()),
            &crate::session::settings::Settings::default(),
        );
        let row = row("a", "ssh:devbox", Some("%1"));
        assert!(!terminals.pane_is_stale(&row, "%1"));

        // A survey the row is not old enough to be judged by is no better.
        surveyed(&mut terminals, "ssh:devbox", &[("tb-other", &["%9"])]);
        terminals.discovery.waiting_since.insert("a".to_string(), 1);
        assert!(!terminals.pane_is_stale(&row, "%1"));
    }

    /// Ambiguity is not absence. Two `tb-demo` windows with no stamp between
    /// them cannot be told apart, and relaunching on that silence is how a
    /// *third* agent appears beside the two that already collide.
    #[test]
    fn ambiguous_namesakes_are_never_relaunched() {
        let mut terminals = Terminals::with_registry(
            std::sync::Arc::new(crate::backend::registry::inert()),
            &crate::session::settings::Settings::default(),
        );
        let row = row("a", "local-tmux", None);
        terminals.discovery.waiting_since.insert("a".to_string(), 0);
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
        let mut terminals = Terminals::with_registry(
            std::sync::Arc::new(crate::backend::registry::inert()),
            &crate::session::settings::Settings::default(),
        );
        let row = row("a", "local-tmux", None);
        terminals.discovery.waiting_since.insert("a".to_string(), 0);
        let local = terminals.key("local-tmux");
        terminals.discovery.surveys.insert(local.clone(), 1);
        terminals.discovery.discovered.insert(
            local,
            crate::backend::identity::WindowIndex::from_listing([
                crate::backend::DiscoveredSession {
                    backend_id: "%1".into(),
                    name: "tb-demo".into(),
                    is_alive: true,
                    session: "b".into(),
                    role: crate::backend::WindowRole::Agent,
                },
            ]),
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
        let mut terminals = Terminals::with_registry(
            std::sync::Arc::new(crate::backend::registry::inert()),
            &crate::session::settings::Settings::default(),
        );
        terminals.sync(&snapshot(vec![row("a", "local-tmux", None)]), 24, 80);
        assert!(terminals.attach.failed.contains_key("a"));

        terminals.sync(&snapshot(Vec::new()), 24, 80);
        assert!(terminals.attach.failed.is_empty(), "state must not leak");
        assert!(terminals.live.is_empty());
    }

    #[test]
    fn sending_to_an_unattached_session_reports_failure() {
        let terminals = Terminals::with_registry(
            std::sync::Arc::new(crate::backend::registry::inert()),
            &crate::session::settings::Settings::default(),
        );
        assert!(!terminals.send("nope", vec![b'x']));
    }

    #[test]
    fn an_unattached_session_has_no_output_age() {
        let terminals = Terminals::with_registry(
            std::sync::Arc::new(crate::backend::registry::inert()),
            &crate::session::settings::Settings::default(),
        );
        assert_eq!(terminals.millis_since_output("nope"), None);
    }

    #[test]
    fn a_single_repository_session_opens_its_shell_in_that_repository() {
        let terminals = Terminals::with_registry(
            std::sync::Arc::new(crate::backend::registry::inert()),
            &crate::session::settings::Settings::default(),
        );
        let mut row = row("s1", "local-tmux", Some("%1"));
        row.cwd = Some(PathBuf::from("/src/alpha"));
        row.member_dirs = vec![PathBuf::from("/src/alpha")];
        assert_eq!(
            terminals.launch_cwd(&row),
            Some(PathBuf::from("/src/alpha"))
        );
    }

    #[test]
    fn a_workspace_that_cannot_be_named_falls_back_to_the_recorded_directory() {
        // No agent session id means no workspace path, and a shell in the
        // primary repository still beats one wherever tmux happened to be.
        let terminals = Terminals::with_registry(
            std::sync::Arc::new(crate::backend::registry::inert()),
            &crate::session::settings::Settings::default(),
        );
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
        let mut terminals = Terminals::with_registry(
            std::sync::Arc::new(crate::backend::registry::inert()),
            &crate::session::settings::Settings::default(),
        );
        terminals
            .discovery
            .discovered_rx
            .0
            .send(Discovered {
                backend: "ssh:devbox".to_string(),
                readied: false,
                panes: Err(crate::backend::BackendError::Unreachable("offline".into())),
            })
            .unwrap();
        terminals.collect_discovered();

        let due = terminals.discovery.discovery_due["ssh:devbox"];
        assert!(
            due > std::time::Instant::now() + REMOTE_DISCOVERY_INTERVAL,
            "a fruitless survey must wait longer than an ordinary one"
        );
    }

    #[test]
    fn a_survey_that_found_windows_keeps_the_ordinary_cadence() {
        let mut terminals = Terminals::with_registry(
            std::sync::Arc::new(crate::backend::registry::inert()),
            &crate::session::settings::Settings::default(),
        );
        terminals
            .discovery
            .discovered_rx
            .0
            .send(Discovered {
                backend: "ssh:devbox".to_string(),
                readied: true,
                panes: Ok(crate::backend::identity::WindowIndex::default()),
            })
            .unwrap();
        terminals.collect_discovered();

        let due = terminals.discovery.discovery_due["ssh:devbox"];
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
        let mut terminals = Terminals::with_registry(
            std::sync::Arc::new(crate::backend::registry::inert()),
            &crate::session::settings::Settings::default(),
        );
        terminals.discovery.discovery_due.insert(
            "local-tmux".to_string(),
            std::time::Instant::now() + ATTACH_RETRY_INTERVAL,
        );

        terminals.forget("a");

        assert!(
            due_now(
                &terminals.discovery.discovery_due,
                "local-tmux",
                std::time::Instant::now()
            ),
            "a restart must not wait out a backoff for the listing that finds its new pane"
        );
    }
}
