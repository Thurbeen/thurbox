//! The session-backend contract: the trait every multiplexer adapter
//! implements and the values that cross it.
//!
//! Names no adapter, no protocol and no global config — a type that only one
//! adapter can produce does not belong here.

use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;

use anyhow::Result;

/// Classified at the adapter/transport, before callers add context.
#[derive(Debug)]
pub enum BackendError {
    Absent(String),
    Unreachable(String),
    Refused(String),
    Busy(String),
    Io(String),
}

impl std::fmt::Display for BackendError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Absent(m)
            | Self::Unreachable(m)
            | Self::Refused(m)
            | Self::Busy(m)
            | Self::Io(m) => f.write_str(m),
        }
    }
}
impl std::error::Error for BackendError {}
impl From<std::io::Error> for BackendError {
    fn from(e: std::io::Error) -> Self {
        let message = e.to_string();
        match e.kind() {
            std::io::ErrorKind::PermissionDenied => Self::Refused(message),
            std::io::ErrorKind::ConnectionRefused
            | std::io::ErrorKind::ConnectionReset
            | std::io::ErrorKind::NotConnected
            | std::io::ErrorKind::BrokenPipe
            | std::io::ErrorKind::TimedOut => Self::Unreachable(message),
            _ => Self::Io(message),
        }
    }
}
impl From<anyhow::Error> for BackendError {
    fn from(e: anyhow::Error) -> Self {
        if let Some(classified) = e.downcast_ref::<Self>() {
            let message = format!("{e:#}");
            return match classified {
                Self::Absent(_) => Self::Absent(message),
                Self::Unreachable(_) => Self::Unreachable(message),
                Self::Refused(_) => Self::Refused(message),
                Self::Busy(_) => Self::Busy(message),
                Self::Io(_) => Self::Io(message),
            };
        }
        // A control-mode reply that never arrived: the reader is gone or the
        // server stopped answering.
        if e.chain()
            .any(|cause| cause.is::<std::sync::mpsc::RecvTimeoutError>())
        {
            return Self::Unreachable(format!("{e:#}"));
        }
        if let Some(io) = e.downcast_ref::<std::io::Error>() {
            return Self::from(std::io::Error::new(io.kind(), format!("{e:#}")));
        }
        Self::Io(format!("{e:#}"))
    }
}
pub type BackendResult<T> = std::result::Result<T, BackendError>;

/// A pane's size as its backend reports it, handed from the backend's output
/// reader to the loop that feeds the pane's grid.
///
/// A pane on a server other clients share is not necessarily the size of the
/// rect this instance paints it into: another thurbox may be sizing it (see
/// `tmux_compat::Server::resize`). A backend that can say what size the pane really is
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
    /// The pane is now `rows` × `cols`. Called by the pane's stream as it
    /// hands over a [`PaneChunk::Resized`], so the size is applied before any
    /// later byte.
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

/// What a pane's reader is handed, in the order the pane's output stream
/// carried it. Every item that is not bytes travels in the same stream as the
/// bytes, so the reader takes it at exactly the point it belongs to: there is
/// no second channel, and so no second ordering to keep.
#[derive(Debug, Clone)]
pub enum PaneChunk {
    /// Bytes the pane printed.
    Output(Vec<u8>),
    /// The pane as the multiplexer holds it at exactly this point of its
    /// output: every byte handed over before this chunk is in it, and none
    /// handed over after. It rebuilds a grid that was dropped
    /// ([`WiredPane::restore`](crate::backend::pane::WiredPane::restore)).
    Snapshot(Box<PaneSnapshot>),
    /// A snapshot as [`Self::Snapshot`] is, for a reader whose output was
    /// dropped for want of room: it replaces the grid the reader has, which
    /// is missing whatever the dropped bytes drew.
    Resync(Box<PaneSnapshot>),
    /// The pane is now this size. In the stream rather than beside it for the
    /// reason a snapshot is: output a program writes after its window was
    /// resized is laid out for the new size, and whatever was queued before
    /// it for the old one.
    Resized { rows: u16, cols: u16 },
    /// Whether another client is named as the pane's sizer. A hint with no
    /// place in the stream, carried here only because this is where the pane's
    /// reader listens.
    SizedElsewhere(bool),
    /// The title and input modes of a pane whose output is held back
    /// ([`SessionBackend::hide_output`]), as terminal bytes to parse but not
    /// to count as output: the report of what the pane already was.
    State(Vec<u8>),
}

impl From<Vec<u8>> for PaneChunk {
    fn from(bytes: Vec<u8>) -> Self {
        Self::Output(bytes)
    }
}

/// A pane's output as its reader takes it: chunks in stream order, ending when
/// the pane is let go of. `next` blocks until the next chunk is there.
pub type PaneOutput = Box<dyn Iterator<Item = PaneChunk> + Send>;

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
    /// Hints, never proof: a restarted server can reissue these ids, so they
    /// cannot distinguish duplicate unstamped window names.
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

/// The pane [`SessionBackend::create_window`] opened. Never empty: a backend
/// that cannot name the pane it made fails the create instead, because a
/// window found by its name alone may be a namesake's (issue #1456).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaneId(String);

impl PaneId {
    /// `Err` for an empty id.
    pub fn new(id: impl Into<String>) -> Result<Self> {
        let id = id.into();
        anyhow::ensure!(
            !id.trim().is_empty(),
            "the backend named no pane for the new window"
        );
        Ok(Self(id))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::ops::Deref for PaneId {
    type Target = str;
    fn deref(&self) -> &str {
        &self.0
    }
}

impl From<PaneId> for String {
    fn from(id: PaneId) -> Self {
        id.0
    }
}

impl std::fmt::Display for PaneId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
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

/// A key a caller presses into a pane, in thurbox's own spelling — `enter`,
/// `page-up`, `ctrl-c` — which each backend says in its own grammar.
///
/// A closed set because a multiplexer need not validate a key name: tmux types
/// an unrecognized one into the pane as literal text, so a typo would inject
/// `Escpe` into an agent's prompt rather than fail. Only [`Key::parse`] makes
/// one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Key(String);

impl Key {
    /// Every named key, canonically spelled. `ctrl-a` … `ctrl-z` are parsed
    /// generically and deliberately not listed.
    pub const NAMED: &[&str] = &[
        "enter",
        "escape",
        "tab",
        "backspace",
        "space",
        "up",
        "down",
        "left",
        "right",
        "home",
        "end",
        "page-up",
        "page-down",
        "delete",
    ];

    /// Alternate spellings, each resolving to one canonical name — forgiving
    /// on purpose, so an integrator writing `esc` or `pgup` need not look the
    /// table up, while the name echoed back is always the one spelling.
    const ALIASES: &[(&str, &str)] = &[
        ("return", "enter"),
        ("esc", "escape"),
        ("bspace", "backspace"),
        ("pageup", "page-up"),
        ("pgup", "page-up"),
        ("pagedown", "page-down"),
        ("pgdn", "page-down"),
        ("del", "delete"),
    ];

    /// Resolve a caller's spelling, or `None` for a key thurbox does not know.
    ///
    /// Case-insensitive, and `ctrl-c`, `ctrl+c`, `C-c` and `c+c` are the same
    /// key: the separator and the `ctrl`/`c` prefix are what people actually
    /// spell differently, and half-supporting them would mean a typo lands as
    /// text in an agent's prompt.
    pub fn parse(input: &str) -> Option<Self> {
        let lower = input.trim().to_ascii_lowercase();
        let lower = Self::ALIASES
            .iter()
            .find(|(alias, _)| *alias == lower)
            .map_or(lower.as_str(), |(_, canonical)| *canonical);
        if let Some(name) = Self::NAMED.iter().find(|name| **name == lower) {
            return Some(Self((*name).to_string()));
        }
        let rest = ["ctrl-", "ctrl+", "c-", "c+"]
            .iter()
            .find_map(|prefix| lower.strip_prefix(prefix))?;
        let mut chars = rest.chars();
        let letter = chars.next().filter(char::is_ascii_lowercase)?;
        if chars.next().is_some() {
            return None;
        }
        Some(Self(format!("ctrl-{letter}")))
    }

    /// The canonical spelling.
    pub fn name(&self) -> &str {
        &self.0
    }

    /// The letter of a `ctrl-<letter>` key.
    pub fn ctrl(&self) -> Option<char> {
        self.0
            .strip_prefix("ctrl-")
            .and_then(|rest| rest.chars().next())
    }
}

/// A pane's live state *around* its rendered text: where the cursor sits,
/// what is running in the foreground, and where that process thinks it is.
///
/// Every field is independently optional and never guessed. A backend that
/// cannot answer one, a pane that went away between two questions, or a
/// machine with no way to read a process's argv each leave the affected fields
/// `None` rather than a plausible wrong value — the caller can then say
/// "unknown" instead of acting on a fabrication.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct PaneState {
    /// Cursor row, 0-based, relative to the visible pane.
    pub cursor_row: Option<u32>,
    /// Cursor column, 0-based.
    pub cursor_col: Option<u32>,
    /// The foreground process's argv0 — its executable as invoked.
    pub foreground_process: Option<String>,
    /// The foreground process's **full** command line.
    ///
    /// `Some` only when the process was really resolved, which is also what
    /// makes this the field worth reading: a Node-based agent CLI is a bare
    /// `node` in every command-*name* view, and only its argv distinguishes
    /// `node …/cursor-agent/cli.js` from a REPL.
    pub foreground_command: Option<String>,
    /// The foreground process's live working directory — where it is, not the
    /// directory the session was launched in.
    pub foreground_cwd: Option<String>,
    /// Whether the pane's program has **exited**.
    ///
    /// A pane kept after its program exits keeps naming whatever last ran
    /// there. Without this, an agent that crashed reports its own name as the
    /// foreground process: a plausible wrong answer rather than an honest
    /// absence, which a caller reconciling a latched state against reality must
    /// not be handed.
    pub dead: Option<bool>,
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
    /// The session's output stream.
    pub output: PaneOutput,
    /// Input write handle to send bytes to the session.
    pub input: Box<dyn Write + Send>,
    /// Where the pane's real size is reported, when the backend can say.
    pub size: Option<PaneSize>,
}

/// A reconnected session from the backend.
pub struct AdoptedSession {
    /// The session's output stream.
    pub output: PaneOutput,
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
    /// The qualified route this backend serves (`local:tmux`,
    /// `ssh:<host>:psmux`), as logs and `runtime status` name it.
    fn name(&self) -> &str;

    /// Backends whose stream does not reliably end when a window is deleted
    /// request periodic discovery of attached sessions. tmux reports close
    /// events in control mode and opts out; psmux and backends without that
    /// guarantee keep polling.
    fn needs_liveness_poll(&self) -> bool {
        true
    }

    /// Whether an agent that exits leaves its window behind ([`BackendLiveness::Exited`]).
    ///
    /// Where it does not, an exit and a window somebody closed both read as
    /// [`BackendLiveness::Missing`], which a repairer relaunches — so a relaunch
    /// there is recorded and a crash loop is held back
    /// (`session_ops::restart`).
    fn keeps_exited_windows(&self) -> bool {
        true
    }

    /// Check if the backend is available/healthy.
    fn check_available(&self) -> BackendResult<()>;

    /// Initialize the backend (e.g., start tmux server).
    fn ensure_ready(&self) -> BackendResult<()>;

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
    ) -> BackendResult<SpawnedSession>;

    /// Reconnect to an existing session. `seed` is the pre-captured pane state
    /// to prepend to the live stream (see [`Self::capture_history`]);
    /// `None` makes the backend capture it itself — the two paths produce the
    /// same bytes, `Some` just lets restore overlap the captures (ADR-P9).
    /// Errs when the backend reports the pane gone, rather than connecting to
    /// nothing.
    fn adopt(
        &self,
        backend_id: &str,
        rows: u16,
        cols: u16,
        seed: Option<Vec<u8>>,
    ) -> BackendResult<AdoptedSession>;

    /// Capture the pane state the live stream cannot replay — its scrollback
    /// history, and any terminal state the backend can read back, such as the
    /// window title an agent uses as its activity line — as terminal bytes
    /// suitable for seeding a fresh parser, to pass into [`Self::adopt`]. An
    /// independent subprocess per pane, safe to run concurrently across
    /// sessions — unlike `adopt`'s control-mode connect, which is serialized.
    /// Default: nothing (backends without a capture facility adopt with an
    /// empty scrollback, exactly as if the capture had failed).
    fn capture_history(&self, _backend_id: &str) -> BackendResult<Vec<u8>> {
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
    /// byte it describes, as a [`PaneChunk::Snapshot`] the reader loop takes
    /// up in order. Returns once asked, not once answered.
    fn request_snapshot(&self, _backend_id: &str) -> BackendResult<()> {
        Err(anyhow::anyhow!("this backend cannot snapshot a pane").into())
    }

    /// The pane's state as it stands, answered to the caller rather than put
    /// into the stream: for a reader that wants the text and has no parser to
    /// keep in step (the content search of a pane that has no grid).
    fn snapshot(&self, _backend_id: &str) -> BackendResult<PaneSnapshot> {
        Err(anyhow::anyhow!("this backend cannot snapshot a pane").into())
    }

    /// Stop sending `backend_id`'s output to this interface while it has no
    /// grid ([`WiredPane::evict`](crate::backend::pane::WiredPane::evict)),
    /// keeping what the interface still reads off it — its title, input modes
    /// and when it last printed — arriving another way. The next
    /// [`Self::request_snapshot`] streams it again. Best-effort: a pane whose
    /// output cannot be held back keeps streaming. Default: it keeps streaming.
    ///
    /// Only worth doing where the output crosses a network: a pane's output is
    /// what a remote session costs the link, shown or not.
    fn hide_output(&self, _backend_id: &str) {}

    /// Last output of each session's agent window, keyed by its owning
    /// session id, in epoch milliseconds. Empty when ownership or output
    /// times cannot be reported. This never streams output.
    fn output_times(&self) -> Result<std::collections::HashMap<String, u64>> {
        Ok(std::collections::HashMap::new())
    }

    /// The pane's current title and mouse modes replayed as terminal bytes, or
    /// nothing — the part of [`Self::capture_history`] that is not history, for
    /// an adopt that captures no history ([`Session::adopt_dormant`](crate::backend::Session::adopt_dormant)). An agent's title
    /// is its activity line, and its mouse modes decide whether the wheel
    /// reaches it; both were set before the interface attached and exist
    /// nowhere else. Default: nothing.
    fn state_seed(&self, _backend_id: &str) -> Vec<u8> {
        Vec::new()
    }

    /// Every thurbox window the backend holds, as it answers for them.
    ///
    /// `Ok` with an empty list only when the backend itself says it holds
    /// nothing; a question that went unanswered — an unreachable host, a
    /// server that could not be asked — is an `Err`. The difference is what
    /// every teardown rests on: an empty answer means there is nothing to
    /// kill.
    fn discover(&self) -> BackendResult<Vec<DiscoveredSession>>;

    /// Open `spec.owner`'s `spec.role` window, detached, stamped with its owner
    /// as it is created, and return its pane — what [`Self::adopt`] later
    /// attaches to. The headless half of a session's lifecycle: create,
    /// restart and restore open a window through here and nothing attaches to
    /// it until an interface does.
    fn create_window(&self, spec: &WindowSpec<'_>) -> BackendResult<PaneId>;

    /// Create a window and observe its startup after `settle`. Backends may
    /// combine this observation with their post-create identity maintenance.
    fn create_window_observed(
        &self,
        spec: &WindowSpec<'_>,
        settle: std::time::Duration,
    ) -> BackendResult<(PaneId, BackendLiveness)> {
        let pane = self.create_window(spec)?;
        std::thread::sleep(settle);
        let state = self.pane_state(pane.as_str())?;
        let located = match spec.role {
            WindowRole::Agent => self.locate(spec.owner)?.agent,
            WindowRole::Shell => self.locate(spec.owner)?.shell,
            WindowRole::Program => {
                if self.discover()?.iter().any(|window| {
                    window.backend_id == pane.as_str()
                        && window.session == spec.owner.session_id
                        && window.role == spec.role
                }) {
                    Located::At(pane.as_str().to_string())
                } else {
                    Located::Unknown
                }
            }
        };
        match located {
            Located::At(ref found) if found == pane.as_str() => {}
            Located::Absent => return Ok((pane, BackendLiveness::Missing)),
            _ => {
                return Err(BackendError::Refused(
                    "created window ownership could not be confirmed".into(),
                ))
            }
        }
        let liveness = match state.dead {
            Some(true) => BackendLiveness::Exited,
            Some(false) => BackendLiveness::Live,
            None => BackendLiveness::Unknown,
        };
        Ok((pane, liveness))
    }

    /// Where `owner`'s two windows are, from one listing the backend
    /// answered — [`crate::backend::identity::WindowIndex`]'s rule (ADR-25),
    /// plus whatever the backend alone can do about an ambiguous answer. `Err`
    /// when it could not answer, which a caller must never read as absence.
    fn locate(&self, owner: Owner<'_>) -> BackendResult<Placed>;

    /// Follow a row's rename with the windows named after it, found under the
    /// name it had (`owner.name`). A window left under the old name is lost to
    /// a backend that finds windows by name, so ambiguity is an error rather
    /// than a skip.
    fn rename_windows(&self, owner: Owner<'_>, to: &str) -> BackendResult<()>;

    /// Stamp a window with the identity every reconciler resolves it by: which
    /// session row owns it, and in what role (on a tmux-protocol server, the
    /// `@thurbox_session` and `@thurbox_role` window options).
    ///
    /// A backend with no place to keep one returns `Ok` and its windows read
    /// as unstamped, which [`crate::backend::identity::WindowIndex`] resolves
    /// by name — a decision each backend makes, not a default it inherits.
    fn stamp_window(
        &self,
        backend_id: &str,
        session_id: &str,
        role: WindowRole,
    ) -> BackendResult<()>;

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
    fn window_panes(&self, window_name: &str) -> BackendResult<Vec<(String, bool)>>;

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
    fn set_pane_retention(&self, backend_id: &str, keep: bool) -> BackendResult<()>;

    /// Type `text` into a pane, pressing Enter after it when `submit`.
    ///
    /// The text arrives literally — never read as keys or flags — and a
    /// multi-line text is one input, not a line submitted per newline.
    /// `submit = false` leaves it in the agent's composer unsent, which is what
    /// "type it, check what the pane shows, then submit" needs.
    ///
    /// Refuses a pane whose program has exited: a pane kept after its program
    /// exits may still accept input and discard it, and every caller reads
    /// success as "the agent got it".
    fn send_text(&self, pane: &str, text: &str, submit: bool) -> BackendResult<()>;

    /// [`Self::send_text`] with `submit`, `delay` from now, scheduled where the
    /// pane lives so it happens after the caller has returned: a headless
    /// command hands a freshly launched agent its prompt once it has had time to
    /// boot, and there is no process of its own left to wait in. Returns once
    /// scheduled.
    fn send_text_after(
        &self,
        pane: &str,
        text: &str,
        delay: std::time::Duration,
    ) -> BackendResult<()>;

    /// Press one key in a pane — no text, no Enter. Refuses an exited pane, as
    /// [`Self::send_text`] does. Returns the key as this backend spelled it.
    fn send_key(&self, pane: &str, key: &Key) -> BackendResult<String>;

    /// A pane's rendered text, with up to `lines` of history before the visible
    /// region, and its styling as SGR sequences when `ansi`.
    fn capture(&self, pane: &str, lines: u32, ansi: bool) -> BackendResult<String>;

    /// The text on the cursor's row to the left of the cursor — what has been
    /// typed into the input line the cursor is on, prompt included. `Ok(None)`:
    /// this backend cannot tell, which a caller must treat as unknown rather
    /// than as an empty line.
    fn text_before_cursor(&self, _pane: &str) -> BackendResult<Option<String>> {
        Ok(None)
    }

    /// What is around a pane's text — see [`PaneState`]. `Err` when the backend
    /// could not ask; an answer it could not read is a default field, not an
    /// error.
    fn pane_state(&self, pane: &str) -> BackendResult<PaneState>;

    /// The `PATH` thurbox handed the pane's program when it opened the window,
    /// read back from the backend's own record of it. `Ok(None)`: the pane is
    /// there, and its `PATH` is not one thurbox wrote or one this backend can
    /// read — which a caller must report as unknown, never as a working one.
    fn pane_path(&self, pane: &str) -> BackendResult<Option<String>>;

    /// Match a pane to the rect it is painted into.
    ///
    /// On a multiplexer other clients share, this may be declined: a pane
    /// another client is sizing stays that client's size (see
    /// `tmux_compat::Server::resize`), and a backend that declines reports the size the
    /// pane really is through [`PaneSize`].
    fn resize(&self, backend_id: &str, rows: u16, cols: u16) -> BackendResult<()>;

    /// Size a pane to `rows` × `cols` and become the client that sizes it — the
    /// user is typing into it here. Default: a plain [`Self::resize`], for a
    /// backend nothing else shares.
    fn claim_size(&self, backend_id: &str, rows: u16, cols: u16) -> BackendResult<()> {
        self.resize(backend_id, rows, cols)
    }

    /// Check if a session's process has exited.
    fn is_dead(&self, backend_id: &str) -> BackendResult<bool>;

    /// Kill a pane and the window it is in — attached or not, so a
    /// teardown needs no interface. Idempotent: a pane already gone is what
    /// the kill wanted.
    fn kill(&self, backend_id: &str) -> BackendResult<()>;

    /// Detach from a session without killing it (for Ctrl+Q quit).
    fn detach(&self, backend_id: &str) -> BackendResult<()>;

    /// Default shell command for companion shell panes: one that exists on
    /// the machine this backend's panes run on. Each backend answers for its
    /// own machine — the OS thurbox was built for is not that machine's once
    /// a pane runs on a host.
    fn default_shell(&self) -> String;

    /// Return the PID of the process running in a backend pane, attached or
    /// not.
    fn pane_pid(&self, backend_id: &str) -> BackendResult<Option<u32>>;

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
    fn pane_pids(&self) -> BackendResult<HashMap<String, u32>> {
        Err(anyhow::anyhow!("batched pane pid lookup not supported by this backend").into())
    }

    /// Every pane the backend lists, dead ones included, in one round trip.
    ///
    /// Whether a pane still *exists*, which [`Self::pane_pids`] cannot say: a
    /// pane is missing from that map whenever it has no pid to report.
    ///
    /// Default: unsupported.
    fn pane_ids(&self) -> BackendResult<HashSet<String>> {
        Err(anyhow::anyhow!("pane listing not supported by this backend").into())
    }

    /// The command a hook running in one of this backend's panes runs, with
    /// the state word appended, to report that state **through this backend**
    /// — in place of `thurbox-cli session signal --state`, which cannot reach
    /// this instance's database from a pane on another machine. Shell text,
    /// free of `"` and `\` so it can be spliced into a JSON, TOML or JS hook
    /// file as it stands.
    ///
    /// `None`: this backend has no status channel a hook can reach. The hook
    /// files are then not shipped and the session reports no status — which a
    /// reader sees as unknown, never as idle. Each backend answers for its own
    /// channel; nothing about it is read from the multiplexer's name or the
    /// host's OS.
    fn hook_signal_command(&self) -> Option<String>;

    /// Record `state` as `pane`'s hook state in this backend's status channel,
    /// where [`Self::take_hook_state_events`] and [`Self::hook_states`] read
    /// it: what `session signal` does after writing the database, so a peer
    /// attached to this backend sees the change live. `Err` when there is no
    /// channel to record it in, or the backend could not be asked.
    fn record_hook_state(&self, pane: &str, state: &str) -> BackendResult<()>;

    /// Every pane's hook state as this backend's status channel holds it, in
    /// one round trip, attached or not — the headless poll's read. `Ok` with an
    /// empty list only when the backend answered that none is held; an
    /// unanswered question, or a backend with no channel, is an `Err`, which
    /// the poll reads as no news rather than as every pane gone quiet.
    fn hook_states(&self) -> BackendResult<Vec<(String, String)>>;

    /// Drain the `(pane, hook-state)` changes this backend's status channel
    /// pushed to an attached interface since the last drain.
    ///
    /// Poll-style shared state (like the `TermSignals` atomics): the interface's
    /// tick drains this and persists each state exactly as a local
    /// `thurbox-cli session signal` would have. Empty when nothing changed or
    /// nothing is attached.
    fn take_hook_state_events(&self) -> Vec<(String, String)>;

    /// Keep `program args…` running every `every` on this backend's machine,
    /// with no interface attached — the automation heartbeat. Infrastructure,
    /// not a session: [`Self::discover`] never lists it. Idempotent: a no-op
    /// when it is already running.
    fn ensure_heartbeat(
        &self,
        program: &Path,
        args: &[String],
        every: std::time::Duration,
    ) -> BackendResult<()>;

    /// Whether the heartbeat [`Self::ensure_heartbeat`] keeps is running. `Err`
    /// when the backend could not be asked.
    fn heartbeat_running(&self) -> BackendResult<bool>;

    /// Stop the heartbeat. Returns whether there was one to stop.
    fn stop_heartbeat(&self) -> BackendResult<bool>;

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

#[cfg(test)]
mod tests {
    use super::*;

    /// A control-mode reply that never came is the backend not answering, not
    /// an I/O failure of unknown meaning, however much context wraps it.
    #[test]
    fn a_reply_timeout_is_unreachable_through_context() {
        let error =
            anyhow::Error::new(std::sync::mpsc::RecvTimeoutError::Timeout).context("list-windows");
        assert!(matches!(
            BackendError::from(error),
            BackendError::Unreachable(_)
        ));
    }

    #[test]
    fn every_named_key_parses_as_itself() {
        for name in Key::NAMED {
            assert_eq!(Key::parse(name).expect("a listed key").name(), *name);
        }
    }

    #[test]
    fn a_key_accepts_the_spellings_people_write() {
        // Separator, prefix and case are the three things spelled differently;
        // all the forms are the one key, and the canonical name is what the
        // caller gets back to depend on.
        for spelling in ["ctrl-c", "ctrl+c", "C-c", "c+C", " CTRL-C "] {
            let key = Key::parse(spelling).expect("{spelling} should parse");
            assert_eq!(key.name(), "ctrl-c");
            assert_eq!(key.ctrl(), Some('c'));
        }
        assert_eq!(Key::parse("esc").unwrap().name(), "escape");
        assert_eq!(Key::parse("RETURN").unwrap().name(), "enter");
        assert_eq!(Key::parse("pgup").unwrap().name(), "page-up");
        assert_eq!(Key::parse("enter").unwrap().ctrl(), None);
    }

    #[test]
    fn a_key_refuses_what_a_multiplexer_would_type_as_text() {
        for bad in [
            "",
            "escpe",
            "ctrl-",
            "ctrl-cc",
            "ctrl-1",
            "Enter Enter",
            "F1",
            "c",
        ] {
            assert!(Key::parse(bad).is_none(), "{bad:?} should not parse");
        }
    }

    #[test]
    fn every_control_letter_parses() {
        for letter in 'a'..='z' {
            let key = Key::parse(&format!("ctrl-{letter}")).expect("ctrl-<letter>");
            assert_eq!(key.ctrl(), Some(letter));
        }
    }
}
