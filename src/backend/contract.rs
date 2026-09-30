//! The session-backend contract: the trait every multiplexer adapter
//! implements and the values that cross it.
//!
//! Names no adapter, no protocol and no global config — a type that only one
//! adapter can produce does not belong here.

use std::collections::{HashMap, HashSet};
use std::io::{Read, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;

use anyhow::Result;

/// A pane's size as its backend reports it, handed from the backend's output
/// reader to the loop that feeds the pane's grid.
///
/// A pane on a server other clients share is not necessarily the size of the
/// rect this instance paints it into: another thurbox may be sizing it (see
/// `TmuxBackend::resize`). A backend that can say what size the pane really is
/// reports it here **in stream order** — between the last byte written for the
/// old size and the first written for the new one — and the grid follows, so
/// an instance that is not sizing still parses the program's output at the
/// width the program is writing for. Held as `None` where the backend cannot
/// say, and then the grid is sized to the rect, as it always was.
#[derive(Clone, Default)]
pub struct PaneSize(Arc<PaneSizeCells>);

#[derive(Default)]
struct PaneSizeCells {
    /// Reported and not yet applied to the grid; 0 once taken.
    pending: AtomicU32,
    /// The last size reported, kept after it is applied.
    current: AtomicU32,
    /// Whether the multiplexer names another client as the pane's sizer.
    elsewhere: AtomicBool,
    /// Set when `elsewhere` goes from true to false, until the pane takes its
    /// own size back ([`WiredPane::retake_size`]).
    released: AtomicBool,
}

/// `(rows, cols)` as one atomic word. Zero is "none": no pane is 0×0.
pub(in crate::backend) fn pack_size(rows: u16, cols: u16) -> u32 {
    (u32::from(rows) << 16) | u32::from(cols)
}

pub(in crate::backend) fn unpack_size(packed: u32) -> Option<(u16, u16)> {
    (packed != 0).then_some(((packed >> 16) as u16, packed as u16))
}

impl PaneSize {
    /// The pane is now `rows` × `cols`. Called by the reader, which then
    /// interrupts its read so the size is applied before any later byte.
    pub fn report(&self, rows: u16, cols: u16) {
        let packed = pack_size(rows, cols);
        self.0.pending.store(packed, Ordering::Relaxed);
        self.0.current.store(packed, Ordering::Relaxed);
    }

    /// Record whether another client is sizing the pane.
    pub fn set_sized_elsewhere(&self, elsewhere: bool) {
        if self.0.elsewhere.swap(elsewhere, Ordering::Relaxed) && !elsewhere {
            self.0.released.store(true, Ordering::Relaxed);
        }
    }

    /// Whether another client is sizing the pane, as last reported. Only a hint
    /// for the interface: it can trail a change by the multiplexer's report
    /// interval, and nothing that decides a size reads it.
    pub fn sized_elsewhere(&self) -> bool {
        self.0.elsewhere.load(Ordering::Relaxed)
    }

    pub(in crate::backend) fn take(&self) -> Option<(u16, u16)> {
        unpack_size(self.0.pending.swap(0, Ordering::Relaxed))
    }

    pub(in crate::backend) fn current(&self) -> u32 {
        self.0.current.load(Ordering::Relaxed)
    }

    /// The last size reported, `(rows, cols)`.
    pub fn last_reported(&self) -> Option<(u16, u16)> {
        unpack_size(self.current())
    }

    /// Whether the pane was released since this was last asked. A load first,
    /// so the frame that asks and finds nothing writes nothing.
    pub(in crate::backend) fn take_released(&self) -> bool {
        self.0.released.load(Ordering::Relaxed) && self.0.released.swap(false, Ordering::Relaxed)
    }
}

/// A pane's screen and history as its multiplexer holds them — enough to
/// rebuild a terminal that was dropped (see
/// [`WiredPane::evict`](crate::backend::pane::WiredPane::evict)). How it is read
/// is the adapter's: tmux answers one command list for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaneSnapshot {
    pub cols: u16,
    pub rows: u16,
    /// Where the cursor is, `(column, row)` from the top-left of the screen.
    pub cursor: (u16, u16),
    /// The normal screen's history and then its rows, one entry per line with
    /// wrapped rows joined, and styled with SGR sequences
    /// when the snapshot was asked for styled.
    pub normal: Vec<String>,
    /// The alternate screen's rows, when that is the one showing.
    pub alternate: Option<Vec<String>>,
}

/// How a [`PaneSnapshot`] reaches a `Read`er: a backend's pane reader returns
/// it as an [`std::io::ErrorKind::Interrupted`] error carrying this.
///
/// The reader loop reads panes through `Box<dyn Read>` — an adopted pane's
/// output is its history seed chained ahead of this reader — so an error kind
/// that means "nothing read, call again" is how an item that is not bytes gets
/// through without a second channel, and so without a second ordering to keep.
/// A reader that does not know about snapshots simply retries, as `Interrupted`
/// asks.
#[derive(Debug)]
pub struct SnapshotArrived(pub Box<PaneSnapshot>);

impl std::fmt::Display for SnapshotArrived {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "a pane snapshot arrived in the output stream")
    }
}

impl std::error::Error for SnapshotArrived {}

impl SnapshotArrived {
    /// The snapshot an error carries, if it is one of these.
    pub fn take(err: std::io::Error) -> Option<Box<PaneSnapshot>> {
        if err.kind() != std::io::ErrorKind::Interrupted {
            return None;
        }
        err.into_inner()?
            .downcast::<SnapshotArrived>()
            .ok()
            .map(|arrived| arrived.0)
    }
}

/// What a thurbox window holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum WindowRole {
    /// A session's agent (`tb-`).
    Agent,
    /// A session's companion shell (`tbs-`).
    Shell,
    /// A plugin's program (`tbp-`). Owned by a plugin rather than a session
    /// row, so it is stamped with a role and no session id — which is what
    /// keeps it from ever resolving as somebody's agent.
    Program,
}

impl WindowRole {
    /// The value a backend stamps as the window's role (on tmux, the
    /// `@thurbox_role` window option).
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Agent => "agent",
            Self::Shell => "shell",
            Self::Program => "program",
        }
    }

    pub(in crate::backend) fn parse(value: &str) -> Option<Self> {
        match value {
            "agent" => Some(Self::Agent),
            "shell" => Some(Self::Shell),
            "program" => Some(Self::Program),
            _ => None,
        }
    }
}

/// Where a listing puts a session's window.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Located {
    /// The window is this pane.
    At(String),
    /// The listing covers the server and nothing on it is this session's.
    Absent,
    /// The listing cannot say: more than one window answers to the name and at
    /// least one of them carries no stamp.
    ///
    /// Never collapse this into [`Located::Absent`]. Reading ambiguity as
    /// absence is what relaunches a session that is already running, so two
    /// colliding windows become three.
    Unknown,
}

impl Located {
    /// The pane, when there is one to act on.
    pub fn pane(self) -> Option<String> {
        match self {
            Self::At(pane) => Some(pane),
            _ => None,
        }
    }

    /// Whether the listing positively says there is no such window. The only
    /// answer a relaunch may act on.
    pub fn is_absent(&self) -> bool {
        matches!(self, Self::Absent)
    }
}

/// A row's two windows, as one listing places them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Placed {
    pub agent: Located,
    pub shell: Located,
}

/// The row a window belongs to, as a caller knows it.
#[derive(Clone, Copy, Debug)]
pub struct Owner<'a> {
    /// The row's id — what a backend stamps a window with, and what resolves
    /// it. Empty only for a caller with no row.
    pub session_id: &'a str,
    /// The row's name, which a window is named after. Not unique: a window is
    /// found by its name only where nothing stamped says otherwise.
    pub name: &'a str,
    /// The panes the row last recorded for its agent and its shell, or empty.
    /// Hints, never proof — a server that restarted reissues pane ids — which
    /// a backend that cannot stamp its windows may fall back on when its
    /// listing cannot tell namesakes apart.
    pub agent_pane: &'a str,
    pub shell_pane: &'a str,
}

impl<'a> Owner<'a> {
    /// A row known by id and name, with no panes remembered.
    pub fn new(session_id: &'a str, name: &'a str) -> Self {
        Self {
            session_id,
            name,
            agent_pane: "",
            shell_pane: "",
        }
    }

    /// The same row, remembering these panes.
    pub fn remembering(self, agent_pane: &'a str, shell_pane: &'a str) -> Self {
        Self {
            agent_pane: agent_pane.trim(),
            shell_pane: shell_pane.trim(),
            ..self
        }
    }
}

/// A window to open for its owner.
pub struct WindowSpec<'a> {
    pub owner: Owner<'a>,
    pub role: WindowRole,
    pub command: &'a str,
    pub args: &'a [String],
    pub cwd: Option<&'a Path>,
    pub env: &'a HashMap<String, String>,
}

/// Metadata returned when discovering existing sessions from the backend.
#[derive(Clone)]
pub struct DiscoveredSession {
    /// Backend-specific ID (e.g., tmux pane_id).
    pub backend_id: String,
    /// Window name or label.
    pub name: String,
    /// Whether the process is still running.
    pub is_alive: bool,
    /// The id of the session row that owns this window, as the window itself
    /// carries it (`@thurbox_session`). Empty for a window spawned before
    /// windows were stamped, or by a multiplexer with no window options — see
    /// [`WindowIndex`](crate::backend::identity::WindowIndex) for what that
    /// leaves resolvable.
    pub session: String,
    /// What the window is for.
    pub role: WindowRole,
}

/// A backend's observation of one agent window. Only `Missing` is proof that
/// a running session should be relaunched; neither a failed probe nor an
/// ambiguous listing can authorize a second agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendLiveness {
    Live,
    Exited,
    Missing,
    Unreachable,
    Unknown,
}

impl BackendLiveness {
    pub fn permits_relaunch(self) -> bool {
        matches!(self, Self::Missing)
    }
}

/// A newly spawned session from the backend.
pub struct SpawnedSession {
    /// Backend-specific session identifier.
    pub backend_id: String,
    /// Streaming output bytes from the session.
    pub output: Box<dyn Read + Send>,
    /// Input write handle to send bytes to the session.
    pub input: Box<dyn Write + Send>,
    /// Where the pane's real size is reported, when the backend can say.
    pub size: Option<PaneSize>,
}

/// A reconnected session from the backend.
pub struct AdoptedSession {
    /// Streaming output bytes from the session.
    pub output: Box<dyn Read + Send>,
    /// Input write handle to send bytes to the session.
    pub input: Box<dyn Write + Send>,
    /// Bytes at the front of `output` that are replayed history rather than
    /// live pane activity (0 when there was none to replay). The reader loop
    /// uses it to hold `last_output_at` back for exactly that many bytes, so a
    /// scrollback replay can't masquerade as fresh output — see
    /// `Session::reader_loop`.
    pub seed_len: usize,
    /// Where the pane's real size is reported, when the backend can say.
    pub size: Option<PaneSize>,
}

/// Trait that all session backends implement. The app layer interacts only through this trait.
pub trait SessionBackend: Send + Sync {
    /// Human-readable name (e.g., "local-tmux", "ssh-remote").
    fn name(&self) -> &str;

    /// Backends whose stream does not reliably end when a window is deleted
    /// request periodic discovery of attached sessions. tmux reports close
    /// events in control mode and opts out; psmux and backends without that
    /// guarantee keep polling.
    fn needs_liveness_poll(&self) -> bool {
        true
    }

    /// Check if the backend is available/healthy.
    fn check_available(&self) -> Result<()>;

    /// Initialize the backend (e.g., start tmux server).
    fn ensure_ready(&self) -> Result<()>;

    /// Spawn a new session running the given command.
    #[allow(clippy::too_many_arguments)]
    fn spawn(
        &self,
        window_name: &str,
        command: &str,
        args: &[String],
        cwd: Option<&Path>,
        env: &HashMap<String, String>,
        rows: u16,
        cols: u16,
    ) -> Result<SpawnedSession>;

    /// Reconnect to an existing session. `seed` is the pre-captured pane state
    /// to prepend to the live stream (see [`Self::capture_history`]);
    /// `None` makes the backend capture it itself — the two paths produce the
    /// same bytes, `Some` just lets restore overlap the captures (ADR-P9).
    fn adopt(
        &self,
        backend_id: &str,
        rows: u16,
        cols: u16,
        seed: Option<Vec<u8>>,
    ) -> Result<AdoptedSession>;

    /// Capture the pane state the live stream cannot replay — its scrollback
    /// history, and any terminal state the backend can read back, such as the
    /// window title an agent uses as its activity line — as terminal bytes
    /// suitable for seeding a fresh parser, to pass into [`Self::adopt`]. An
    /// independent subprocess per pane, safe to run concurrently across
    /// sessions — unlike `adopt`'s control-mode connect, which is serialized.
    /// Default: nothing (backends without a capture facility adopt with an
    /// empty scrollback, exactly as if the capture had failed).
    fn capture_history(&self, _backend_id: &str) -> Result<Vec<u8>> {
        Ok(Vec::new())
    }

    /// Whether this backend can hand back a pane's screen and history in step
    /// with its output ([`Self::request_snapshot`]) — the thing that lets a
    /// session off screen drop its grid ([`WiredPane::evict`](crate::backend::pane::WiredPane::evict)) and get it back
    /// exactly. Default: no, and such a backend's sessions keep theirs.
    fn supports_snapshots(&self) -> bool {
        false
    }

    /// Ask for the pane's state to arrive **in its own output stream**, at the
    /// byte it describes, as a
    /// [`SnapshotArrived`] the
    /// reader loop takes up in order. Returns once asked, not once answered.
    fn request_snapshot(&self, _backend_id: &str) -> Result<()> {
        anyhow::bail!("this backend cannot snapshot a pane")
    }

    /// The pane's state as it stands, answered to the caller rather than put
    /// into the stream: for a reader that wants the text and has no parser to
    /// keep in step (the content search of a pane that has no grid).
    fn snapshot(&self, _backend_id: &str) -> Result<PaneSnapshot> {
        anyhow::bail!("this backend cannot snapshot a pane")
    }

    /// The pane's current title replayed as terminal bytes, or nothing — the
    /// part of [`Self::capture_history`] that is not history, for an adopt
    /// that captures no history ([`Session::adopt_dormant`](crate::backend::Session::adopt_dormant)). An agent's title
    /// is its activity line, and one set before the interface attached exists
    /// nowhere else. Default: nothing.
    fn title_seed(&self, _backend_id: &str) -> Vec<u8> {
        Vec::new()
    }

    /// Every thurbox window the backend holds, as it answers for them.
    ///
    /// `Ok` with an empty list only when the backend itself says it holds
    /// nothing; a question that went unanswered — an unreachable host, a
    /// server that could not be asked — is an `Err`. The difference is what
    /// every teardown rests on: an empty answer means there is nothing to
    /// kill.
    fn discover(&self) -> Result<Vec<DiscoveredSession>>;

    /// Open `spec.owner`'s `spec.role` window, detached, stamped with its owner
    /// as it is created, and return its pane — what [`Self::adopt`] later
    /// attaches to. The headless half of a session's lifecycle: create,
    /// restart and restore open a window through here and nothing attaches to
    /// it until an interface does. An empty pane id is the answer of a
    /// backend that cannot report one; the window is then found by its owner.
    fn create_window(&self, spec: &WindowSpec<'_>) -> Result<String>;

    /// Where `owner`'s two windows are, from one listing the backend
    /// answered — [`crate::backend::identity::WindowIndex`]'s rule (ADR-25),
    /// plus whatever the backend alone can do about an ambiguous answer. `Err`
    /// when it could not answer, which a caller must never read as absence.
    fn locate(&self, owner: Owner<'_>) -> Result<Placed>;

    /// Follow a row's rename with the windows named after it, found under the
    /// name it had (`owner.name`). A window left under the old name is lost to
    /// a backend that finds windows by name, so ambiguity is an error rather
    /// than a skip.
    fn rename_windows(&self, owner: Owner<'_>, to: &str) -> Result<()>;

    /// Stamp a window with the identity every reconciler resolves it by: which
    /// session row owns it, and in what role (see
    /// [`crate::backend::tmux::WINDOW_SESSION_OPTION`]).
    ///
    /// A backend with no place to keep one returns `Ok` and its windows read
    /// as unstamped, which [`crate::backend::identity::WindowIndex`] resolves
    /// by name — a decision each backend makes, not a default it inherits.
    fn stamp_window(&self, backend_id: &str, session_id: &str, role: WindowRole) -> Result<()>;

    /// The pane of every window carrying this **exact** name, and whether that
    /// pane is dead.
    ///
    /// Deliberately separate from [`Self::discover`], which filters to agent
    /// windows (`tb-`) — by design, since it answers "which sessions are running".
    /// A plugin's program pane is found by *name* because the name is its identity
    /// (nothing about it is persisted), so it needs a lookup that is not filtered
    /// to a prefix it does not have. That the shell prefix `tbs-` also fails
    /// `discover`'s filter is why shells persist a pane id instead.
    ///
    /// Dead panes are **reported, not hidden**, and every window of the name is
    /// reported rather than the first: a caller that cannot see a corpse cannot
    /// clear it, and the name is meant to address exactly one window — so the one
    /// caller there is ([`crate::kernel::terminal::Terminals::start_program`])
    /// needs the whole picture to keep that true.
    ///
    fn window_panes(&self, window_name: &str) -> Result<Vec<(String, bool)>>;

    /// Say whether the window holding `backend_id` keeps its pane's corpse.
    ///
    /// For a window that already existed: it was made by an **earlier**
    /// interface, possibly one that set `remain-on-exit` for a whole session and
    /// landed it on whichever window was current — and a program window left
    /// carrying `on` is a pane whose exit can never be announced, which is the
    /// state the first restart after an upgrade would otherwise inherit.
    ///
    /// Asked only where the answer is already known from the caller's own
    /// naming, so this is one round trip and no lookup.
    fn set_pane_retention(&self, backend_id: &str, keep: bool) -> Result<()>;

    /// Match a pane to the rect it is painted into.
    ///
    /// On a multiplexer other clients share, this may be declined: a pane
    /// another client is sizing stays that client's size (see
    /// `TmuxBackend::resize`), and a backend that declines reports the size the
    /// pane really is through [`PaneSize`].
    fn resize(&self, backend_id: &str, rows: u16, cols: u16) -> Result<()>;

    /// Size a pane to `rows` × `cols` and become the client that sizes it — the
    /// user is typing into it here. Default: a plain [`Self::resize`], for a
    /// backend nothing else shares.
    fn claim_size(&self, backend_id: &str, rows: u16, cols: u16) -> Result<()> {
        self.resize(backend_id, rows, cols)
    }

    /// Check if a session's process has exited.
    fn is_dead(&self, backend_id: &str) -> Result<bool>;

    /// Kill a pane and the window it is in — attached or not, so a
    /// teardown needs no interface. Idempotent: a pane already gone is what
    /// the kill wanted.
    fn kill(&self, backend_id: &str) -> Result<()>;

    /// Detach from a session without killing it (for Ctrl+Q quit).
    fn detach(&self, backend_id: &str) -> Result<()>;

    /// Default shell command for companion shell panes: one that exists on
    /// the machine this backend's panes run on. Each backend answers for its
    /// own machine — the OS thurbox was built for is not that machine's once
    /// a pane runs on a host.
    fn default_shell(&self) -> String;

    /// Return the PID of the process running in a backend pane, attached or
    /// not.
    fn pane_pid(&self, backend_id: &str) -> Result<Option<u32>>;

    /// Every live pane's `pane_id → pid` in **one** backend round trip.
    ///
    /// The batched form of [`Self::pane_pid`], for callers that sample many
    /// panes at once (the metrics worker, once per second per session):
    /// each single-pane lookup is a control-mode round trip serialized on the
    /// same connection mutex keystrokes share, so per-session lookups scale
    /// the contention with the session count. A pane absent from an `Ok` map
    /// simply has no pid (it is gone or dead).
    ///
    /// Default: unsupported — an `Err` tells the caller to fall back to
    /// per-pane [`Self::pane_pid`], which every backend must provide.
    fn pane_pids(&self) -> Result<HashMap<String, u32>> {
        anyhow::bail!("batched pane pid lookup not supported by this backend")
    }

    /// Every pane the backend lists, dead ones included, in one round trip.
    ///
    /// Whether a pane still *exists*, which [`Self::pane_pids`] cannot say: a
    /// pane is missing from that map whenever it has no pid to report.
    ///
    /// Default: unsupported.
    fn pane_ids(&self) -> Result<HashSet<String>> {
        anyhow::bail!("pane listing not supported by this backend")
    }

    /// Drain queued `(backend_id, hook-state)` events reported by a remote
    /// agent's hooks (a tmux pane user option pushed over the control-mode
    /// subscription — see [`crate::session::REMOTE_HOOK_STATE_OPTION`]).
    ///
    /// Poll-style shared state (like the `TermSignals` atomics): the app tick
    /// drains this and persists each state exactly as a local
    /// `thurbox-cli session signal` would have. Default: no events — only the
    /// tmux backend produces them.
    fn take_hook_state_events(&self) -> Vec<(String, String)> {
        Vec::new()
    }

    /// Tear down the backend's own long-lived resources (for a tmux backend,
    /// its control-mode connection: child process + reader thread).
    ///
    /// Distinct from [`Self::detach`], which retires one *session*'s pane. This
    /// retires the *connection*, and is called once per backend at quit
    /// ([`BackendRegistry::shutdown_all`](crate::backend::BackendRegistry::shutdown_all)).
    ///
    /// Exists as an explicit method rather than relying on `Drop` so quit can
    /// run every backend's teardown **concurrently**: the registry holds each
    /// backend behind an `Arc`, so dropping it is both hard to sequence and
    /// serial by nature, and each connection's teardown blocks on a child exit.
    /// Total quit cost is then the slowest connection rather than their sum —
    /// which matters because the backend count grows with every configured SSH
    /// host and auto-discovered WSL distro. Must be idempotent: a later `Drop`
    /// still runs and has to be a no-op.
    fn shutdown(&self);
}
