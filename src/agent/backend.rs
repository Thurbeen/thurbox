use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::Path;
use std::sync::{
    atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering},
    Arc, Mutex,
};
use std::time::SystemTime;

use anyhow::Result;
use tokio::sync::mpsc;
use tracing::{debug, error};

use crate::agent::osc8;
use crate::agent::provider::AgentProvider;
use crate::agent::tmux::WindowRole;
use crate::session::{HyperlinkTable, SessionConfig, SessionInfo};

pub(crate) fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

/// The smallest grid a `vt100` parser may be given, as `(rows, cols)`.
///
/// Two, not one, and the difference is a crash. `set_size(0, _)` underflows
/// outright, which is why every size here used to be clamped at 1 — but a
/// **one-row** grid underflows in `row_inc_scroll` the moment output wraps, and a
/// **one-column** grid underflows in `col_wrap` (`cols - width`) the moment a
/// double-width character arrives, which for an agent that prints emoji is the
/// first line it writes. A cramped layout really does compute those rects — a
/// pane one column wide is what a 20-column terminal with the session list open
/// leaves for the terminal — and the panic lands on the *reader* thread, so the
/// process survives while that session's pane is blank for the rest of the run:
/// the unwind poisons the parser mutex, and every reader of it (paint, links,
/// selection, copy) treats a poisoned lock as "no live terminal".
///
/// Nothing is readable at two columns either, so clamping loses nothing a
/// smaller grid would have shown.
fn vt_floor(rows: u16, cols: u16) -> (u16, u16) {
    (rows.max(2), cols.max(2))
}

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
fn pack_size(rows: u16, cols: u16) -> u32 {
    (u32::from(rows) << 16) | u32::from(cols)
}

fn unpack_size(packed: u32) -> Option<(u16, u16)> {
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

    fn take(&self) -> Option<(u16, u16)> {
        unpack_size(self.0.pending.swap(0, Ordering::Relaxed))
    }

    fn current(&self) -> u32 {
        self.0.current.load(Ordering::Relaxed)
    }

    /// The last size reported, `(rows, cols)`.
    pub fn last_reported(&self) -> Option<(u16, u16)> {
        unpack_size(self.current())
    }

    /// Whether the pane was released since this was last asked. A load first,
    /// so the frame that asks and finds nothing writes nothing.
    fn take_released(&self) -> bool {
        self.0.released.load(Ordering::Relaxed) && self.0.released.swap(false, Ordering::Relaxed)
    }
}

/// Length of the prefix of `buf` that is safe to feed to the vt100 parser
/// without splitting a UTF-8 character. Returns `buf.len()` unless `buf` ends
/// with the start of a multi-byte character whose continuation bytes have not
/// all arrived yet, in which case it returns the offset of that incomplete
/// lead byte (so the caller can carry the tail to the next read).
///
/// Only a *plausibly-complete-able* truncated tail is held back: a lead byte
/// missing some of its continuations. A malformed tail (continuation bytes with
/// no lead, or a fully-present sequence) is passed through as-is, so garbage is
/// never buffered unboundedly — the carry is at most 3 bytes (a 4-byte char
/// missing its last byte).
fn utf8_ready_prefix_len(buf: &[u8]) -> usize {
    let len = buf.len();
    // A truncated tail is at most 3 bytes, so only the last 3 can matter.
    let start = len.saturating_sub(3);
    let mut i = len;
    while i > start {
        i -= 1;
        let b = buf[i];
        // Anything that is not a UTF-8 continuation byte (0x80..=0xbf) starts a
        // character — ASCII or a multi-byte lead.
        if !(0x80..=0xbf).contains(&b) {
            // A lead byte (or ASCII). Determine the sequence's expected length;
            // if not all of it is present yet, cut before it.
            let expected = match b {
                0x00..=0x7f => 1,
                0xc0..=0xdf => 2,
                0xe0..=0xef => 3,
                _ => 4,
            };
            return if len - i < expected { i } else { len };
        }
        // Continuation byte (0x80..=0xbf): keep scanning back for its lead.
    }
    // No lead byte within the last 3 bytes — malformed tail; don't hold it back.
    len
}

/// Captures terminal signals the agent emits into shared cells read by the app
/// layer (mirrors the `last_output_at` side channel). The parser fires these
/// callbacks while processing the PTY byte stream:
///
/// - **Title** (OSC `0`/`1`/`2`) → live activity text.
/// - **Attention** — a terminal bell (`BEL`) or a desktop-notification escape
///   (OSC `9`, OSC `777`) means the agent finished or needs input. We record
///   the time of the latest such signal, plus its message text when the OSC
///   carries one. This is how we surface a real "needs attention" state instead
///   of timing-only Busy/Waiting.
/// - **Hyperlinks** (OSC `8`) → the target of each rich-text link the agent
///   printed, which `vt100` itself discards (see the `osc8` module).
#[derive(Clone, Default)]
pub struct TermSignals {
    title: Arc<Mutex<Option<String>>>,
    /// `now_millis()` of the most recent attention signal; `0` = none yet.
    attention_at: Arc<AtomicU64>,
    /// Message text from the most recent OSC 9/777 notification, if any.
    notification: Arc<Mutex<Option<String>>>,
    /// Generation counter bumped after every title/notification write, so the
    /// per-tick status refresh can skip the mutex locks + String clones while
    /// nothing changed (ADR-P10; see [`Session::sync_agent_meta`]).
    meta_gen: Arc<AtomicU64>,
    /// OSC 8 hyperlink runs the agent printed. Unlike the cells above this is
    /// not shared state: readers reach it through the parser lock they already
    /// take to read the screen ([`Self::hyperlink_at`]).
    hyperlinks: HyperlinkTable,
    /// The OSC 8 run whose closing sequence has not arrived yet.
    pending_link: Option<osc8::PendingHyperlink>,
}

impl TermSignals {
    fn store_title(&self, raw: &[u8]) {
        let s = String::from_utf8_lossy(raw).trim().to_string();
        if let Ok(mut guard) = self.title.lock() {
            *guard = (!s.is_empty()).then_some(s);
        }
        // After the write, so a reader that observes the new generation also
        // observes the new value.
        self.meta_gen.fetch_add(1, Ordering::Release);
    }

    /// The OSC 8 runs captured from the agent's output — the targets of its
    /// rich-text links, of which the screen holds only the labels. Resolve a
    /// clicked cell against it with [`HyperlinkTable::resolve`], or list what is
    /// on screen with [`HyperlinkTable::visible_runs`].
    pub fn hyperlinks(&self) -> &HyperlinkTable {
        &self.hyperlinks
    }

    /// Mark an attention signal, optionally with notification message text.
    fn signal_attention(&self, message: Option<String>) {
        self.attention_at.store(now_millis(), Ordering::Relaxed);
        if let Some(msg) = message {
            let msg = msg.trim().to_string();
            if let Ok(mut guard) = self.notification.lock() {
                *guard = (!msg.is_empty()).then_some(msg);
            }
            self.meta_gen.fetch_add(1, Ordering::Release);
        }
    }
}

impl vt100::Callbacks for TermSignals {
    fn set_window_title(&mut self, _: &mut vt100::Screen, title: &[u8]) {
        self.store_title(title);
    }

    fn set_window_icon_name(&mut self, _: &mut vt100::Screen, icon_name: &[u8]) {
        // Some CLIs emit only OSC `1` (icon name); treat it as the title too.
        self.store_title(icon_name);
    }

    fn audible_bell(&mut self, _: &mut vt100::Screen) {
        // BEL: the cross-agent "done / needs you" signal (e.g. Claude's
        // `preferredNotifChannel terminal_bell`). No message text.
        self.signal_attention(None);
    }

    fn unhandled_osc(&mut self, screen: &mut vt100::Screen, params: &[&[u8]]) {
        // Desktop-notification escapes carry the agent's status message.
        //   OSC 9 ; <message>
        //   OSC 777 ; notify ; <title> ; <body>
        // A hyperlink pair brackets its label's glyphs.
        //   OSC 8 ; <params> ; <uri>   …label…   OSC 8 ; ;
        match params {
            [b"9", msg] => self.signal_attention(Some(String::from_utf8_lossy(msg).into_owned())),
            [b"777", kind, rest @ ..] if kind.eq_ignore_ascii_case(b"notify") => {
                let msg = rest
                    .iter()
                    .map(|p| String::from_utf8_lossy(p))
                    .collect::<Vec<_>>()
                    .join(": ");
                self.signal_attention(Some(msg));
            }
            [b"8", fields @ ..] => {
                let uri = osc8::uri_from_fields(fields);
                osc8::handle(screen, &mut self.pending_link, &mut self.hyperlinks, &uri);
            }
            _ => {}
        }
    }
}

/// Session terminal parser, specialized to capture terminal signals via
/// [`TermSignals`]. The captured `Screen` is callback-independent, so
/// rendering is unaffected.
pub type SessionParser = vt100::Parser<TermSignals>;

/// Stamp a freshly spawned window with the session that owns it.
///
/// Best-effort and logged rather than propagated: a window that could not be
/// stamped is still a perfectly good window, and resolves by name for as long
/// as it is the only one with that name — which is what every window did
/// before ADR-25.
fn stamp(
    backend: &Arc<dyn SessionBackend>,
    backend_id: &str,
    session_id: crate::session::SessionId,
    role: WindowRole,
) {
    if let Err(e) = backend.stamp_window(backend_id, &session_id.to_string(), role) {
        debug!(session_id = %session_id, "could not stamp the window: {e:#}");
    }
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
    /// [`crate::agent::tmux::WindowIndex`] for what that leaves resolvable.
    pub session: String,
    /// What the window is for.
    pub role: crate::agent::tmux::WindowRole,
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
    /// session off screen drop its grid ([`WiredPane::evict`]) and get it back
    /// exactly. Default: no, and such a backend's sessions keep theirs.
    fn supports_snapshots(&self) -> bool {
        false
    }

    /// Ask for the pane's state to arrive **in its own output stream**, at the
    /// byte it describes, as a
    /// [`SnapshotArrived`](crate::agent::control_mode::SnapshotArrived) the
    /// reader loop takes up in order. Returns once asked, not once answered.
    fn request_snapshot(&self, _backend_id: &str) -> Result<()> {
        anyhow::bail!("this backend cannot snapshot a pane")
    }

    /// The pane's state as it stands, answered to the caller rather than put
    /// into the stream: for a reader that wants the text and has no parser to
    /// keep in step (the content search of a pane that has no grid).
    fn snapshot(&self, _backend_id: &str) -> Result<crate::agent::control_mode::PaneSnapshot> {
        anyhow::bail!("this backend cannot snapshot a pane")
    }

    /// The pane's current title replayed as terminal bytes, or nothing — the
    /// part of [`Self::capture_history`] that is not history, for an adopt
    /// that captures no history ([`Session::adopt_dormant`]). An agent's title
    /// is its activity line, and one set before the interface attached exists
    /// nowhere else. Default: nothing.
    fn title_seed(&self, _backend_id: &str) -> Vec<u8> {
        Vec::new()
    }

    /// Discover existing sessions managed by this backend.
    fn discover(&self) -> Result<Vec<DiscoveredSession>>;

    /// Stamp a window with the identity every reconciler resolves it by: which
    /// session row owns it, and in what role (see
    /// [`crate::agent::tmux::WINDOW_SESSION_OPTION`]).
    ///
    /// Defaults to doing nothing, for a backend with no place to keep it —
    /// such a backend's windows read as unstamped, which
    /// [`crate::agent::tmux::WindowIndex`] resolves by name as before.
    fn stamp_window(
        &self,
        _backend_id: &str,
        _session_id: &str,
        _role: crate::agent::tmux::WindowRole,
    ) -> Result<()> {
        Ok(())
    }

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
    /// Default: nothing found, so a backend without a window concept simply always
    /// spawns fresh.
    fn window_panes(&self, _window_name: &str) -> Result<Vec<(String, bool)>> {
        Ok(Vec::new())
    }

    /// Say whether the window holding `backend_id` keeps its pane's corpse.
    ///
    /// For a window that already existed: it was made by an **earlier**
    /// interface, possibly one that set `remain-on-exit` for a whole session and
    /// landed it on whichever window was current — and a program window left
    /// carrying `on` is a pane whose exit can never be announced, which is the
    /// state the first restart after an upgrade would otherwise inherit.
    ///
    /// Asked only where the answer is already known from the caller's own
    /// naming, so this is one round trip and no lookup. Default: nothing to say,
    /// for a backend with no window options at all.
    fn set_pane_retention(&self, _backend_id: &str, _keep: bool) -> Result<()> {
        Ok(())
    }

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

    /// Whether a pane is dead *or no longer exists* — `Ok(true)` only on the
    /// multiplexer's word, `Err` when it could not be asked. Distinct from
    /// [`Self::is_dead`] because tmux answers that one for a pane it no longer
    /// has with an empty line, which reads as alive. Called from the interface's
    /// loop, so an implementation that reaches a remote host bounds the wait.
    fn pane_gone(&self, backend_id: &str) -> Result<bool> {
        self.is_dead(backend_id)
    }

    /// Kill/destroy a session (for Ctrl+X close).
    fn kill(&self, backend_id: &str) -> Result<()>;

    /// Detach from a session without killing it (for Ctrl+Q quit).
    fn detach(&self, backend_id: &str) -> Result<()>;

    /// Default shell command for companion shell panes.
    ///
    /// Unix uses `$SHELL` (falling back to `/bin/sh`); Windows uses `%COMSPEC%`
    /// (falling back to `cmd.exe`), since `$SHELL`/`/bin/sh` don't exist there.
    fn default_shell(&self) -> String {
        #[cfg(windows)]
        {
            std::env::var("COMSPEC").unwrap_or_else(|_| "cmd.exe".to_string())
        }
        #[cfg(not(windows))]
        {
            std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string())
        }
    }

    /// Return the PID of the process running in a backend pane.
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
    /// retires the *connection*, and is called once per backend at quit.
    ///
    /// Exists as an explicit method rather than relying on `Drop` so quit can
    /// run every backend's teardown **concurrently**: the registry holds each
    /// backend behind an `Arc`, so dropping it is both hard to sequence and
    /// serial by nature, and each connection's teardown blocks on a child exit.
    /// Total quit cost is then the slowest connection rather than their sum —
    /// which matters because the backend count grows with every configured SSH
    /// host and auto-discovered WSL distro. Must be idempotent: a later `Drop`
    /// still runs and has to be a no-op.
    ///
    /// Default: nothing to tear down.
    fn shutdown(&self) {}
}

/// Internal bundle of I/O handles before wiring.
struct SessionIo {
    output: Box<dyn Read + Send>,
    input: Box<dyn Write + Send>,
    backend_id: String,
    /// Whether these handles came from a fresh spawn or an adopt.
    mode: WireMode,
    /// Replayed-history bytes at the front of `output`, from
    /// [`AdoptedSession::seed_len`] (always 0 for a spawn).
    seed_len: usize,
    /// Whether the parser starts with its grid, or without one until it is
    /// first asked for ([`Session::adopt_dormant`]).
    resident: bool,
    /// Where the pane's real size is reported, when the backend can say.
    size: Option<PaneSize>,
}

impl SessionIo {
    fn spawned(spawned: SpawnedSession) -> Self {
        Self {
            output: spawned.output,
            input: spawned.input,
            backend_id: spawned.backend_id,
            mode: WireMode::Spawn,
            seed_len: 0,
            resident: true,
            size: spawned.size,
        }
    }

    fn adopted(adopted: AdoptedSession, backend_id: &str) -> Self {
        Self {
            output: adopted.output,
            input: adopted.input,
            backend_id: backend_id.to_string(),
            mode: WireMode::Adopt,
            seed_len: adopted.seed_len,
            resident: true,
            size: adopted.size,
        }
    }
}

/// Whether we are wiring a freshly-spawned process or reconnecting to an
/// already-running one. Controls the initial `last_output_at`: a fresh spawn
/// is legitimately starting up (recent activity → `Busy`), whereas an adopt's
/// first output is the forced SIGWINCH repaint, which must not be mistaken for
/// real agent activity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WireMode {
    Spawn,
    Adopt,
}

/// Initial `last_output_at` for a session being wired in `mode`. `Spawn` uses
/// "now" (fresh process is active); `Adopt` uses `0` (stale) so the post-adopt
/// repaint doesn't read as activity — the first *real* output flips it to busy.
fn initial_output_at(mode: WireMode) -> u64 {
    match mode {
        WireMode::Spawn => now_millis(),
        WireMode::Adopt => 0,
    }
}

/// Max pending input messages per session before sends fail fast. Each
/// message is one key/paste payload; a full queue means the tmux stdin
/// writer has stalled, and dropping with an error beats unbounded growth.
const INPUT_CHANNEL_CAPACITY: usize = 1024;

/// Queue input without ever blocking — `send_input` is called from the
/// render/update path, so a stalled writer must surface as an error, not a
/// hang.
fn send_to_input_channel(tx: &mpsc::Sender<Vec<u8>>, data: Vec<u8>, what: &str) -> Result<()> {
    use tokio::sync::mpsc::error::TrySendError;
    tx.try_send(data).map_err(|e| match e {
        TrySendError::Full(_) => anyhow::anyhow!("{what} input channel full (writer stalled)"),
        TrySendError::Closed(_) => anyhow::anyhow!("{what} input channel closed"),
    })
}

/// The shared cells [`TermSignals`] writes from the reader thread; a
/// [`Session`] keeps its own handles to read them back (`agent_title`,
/// `needs_attention`, …). Shell and program panes drop theirs — the parser's
/// `TermSignals` owns its own clones, so nothing dangles.
struct SignalCells {
    last_title: Arc<Mutex<Option<String>>>,
    attention_at: Arc<AtomicU64>,
    notification: Arc<Mutex<Option<String>>>,
    meta_gen: Arc<AtomicU64>,
}

impl SignalCells {
    fn new() -> Self {
        Self {
            last_title: Arc::new(Mutex::new(None)),
            attention_at: Arc::new(AtomicU64::new(0)),
            notification: Arc::new(Mutex::new(None)),
            meta_gen: Arc::new(AtomicU64::new(0)),
        }
    }

    /// The [`TermSignals`] callback bundle writing into these cells.
    fn term_signals(&self) -> TermSignals {
        TermSignals {
            title: Arc::clone(&self.last_title),
            attention_at: Arc::clone(&self.attention_at),
            notification: Arc::clone(&self.notification),
            meta_gen: Arc::clone(&self.meta_gen),
            ..Default::default()
        }
    }
}

/// The grid a pane is left with while it has none of its own ([`WiredPane::evict`]):
/// the smallest `vt_floor` allows. It still reads every byte, which is what
/// keeps the title, a bell, a notification and the input modes current.
const DORMANT_GRID: (u16, u16) = (2, 2);

/// How long an unanswered snapshot request stands before it may be asked
/// again. The answer comes back on the reader thread, so an answer that never
/// comes — a full pane channel dropped it, the command failed — shows up only
/// as this running out.
const SNAPSHOT_RETRY_MS: u64 = 2_000;

/// Whether a pane's parser holds its grid, and the handshake for getting it
/// back. Shared by the pane and its reader thread, which is where a snapshot
/// is installed.
#[derive(Default)]
struct Residency {
    /// Set by [`WiredPane::evict`], cleared when a snapshot is installed.
    dormant: AtomicBool,
    /// `now_millis()` of the snapshot request still unanswered; `0` for none.
    requested_at: AtomicU64,
    /// Moves every time the grid is rebuilt, so what was read off the grid
    /// before stops matching ([`WiredPane::content_stamp`]). Not when it is
    /// dropped: nothing reads a pane off screen off its grid, and a drop that
    /// moved it would cost a frame for a pane nobody is looking at.
    changes: AtomicU64,
    /// Wakes a paint waiting on a snapshot ([`WiredPane::wait_resident`]).
    landed: (Mutex<()>, std::sync::Condvar),
}

impl Residency {
    fn dormant() -> Self {
        Self {
            dormant: AtomicBool::new(true),
            ..Self::default()
        }
    }

    fn is_resident(&self) -> bool {
        !self.dormant.load(Ordering::Acquire)
    }
}

/// Everything the blocking output reader shares with the rest of a pane.
/// Keeping it together prevents the reader's redraw, activity, residency and
/// backend-size signals from drifting apart as those contracts evolve.
struct ReaderState {
    parser: Arc<Mutex<SessionParser>>,
    exited: Arc<AtomicBool>,
    last_output_at: Arc<AtomicU64>,
    residency: Arc<Residency>,
    output_count: Arc<AtomicU64>,
    size: Option<PaneSize>,
}

impl ReaderState {
    fn new(
        parser: Arc<Mutex<SessionParser>>,
        exited: Arc<AtomicBool>,
        last_output_at: Arc<AtomicU64>,
        residency: Arc<Residency>,
        output_count: Arc<AtomicU64>,
        size: Option<PaneSize>,
    ) -> Self {
        Self {
            parser,
            exited,
            last_output_at,
            residency,
            output_count,
            size,
        }
    }
}

/// The state a grid carries that its cells do not: the pen, the input modes a
/// keystroke is encoded by, whether the cursor is hidden, and which screen is
/// up. Replayed into a parser that replaces it, in either direction.
fn carried_state(screen: &vt100::Screen) -> Vec<u8> {
    let mut out = Vec::new();
    if screen.alternate_screen() {
        out.extend_from_slice(b"\x1b[?1049h");
    }
    out.extend(screen.input_mode_formatted());
    if screen.hide_cursor() {
        out.extend_from_slice(b"\x1b[?25l");
    }
    out.extend(screen.attributes_formatted());
    out
}

/// Terminal bytes that rebuild the pane `snapshot` describes, on a parser of
/// its size: the normal screen's history and rows, then the alternate screen
/// if it is up, then the cursor, then what `dormant` kept track of meanwhile
/// (the pen, the input modes and the cursor's visibility).
///
/// Every row is written, blank ones included, so the last row of the capture
/// lands on the bottom row of the screen and the rows above it scroll into the
/// history in order — the screen is the pane's, not the capture's text pushed
/// to the top. Wrapped rows arrive joined and wrap again at the same width.
pub fn snapshot_seed(
    snapshot: &crate::agent::control_mode::PaneSnapshot,
    dormant: &vt100::Screen,
) -> Vec<u8> {
    let mut out = Vec::new();
    let write_rows = |out: &mut Vec<u8>, rows: &[String]| {
        for (i, row) in rows.iter().enumerate() {
            if i > 0 {
                out.extend_from_slice(b"\r\n");
            }
            out.extend_from_slice(row.as_bytes());
        }
    };
    write_rows(&mut out, &snapshot.normal);
    if let Some(alternate) = &snapshot.alternate {
        out.extend_from_slice(b"\x1b[m\x1b[?1049h");
        write_rows(&mut out, alternate);
    }
    let (x, y) = snapshot.cursor;
    out.extend_from_slice(format!("\x1b[m\x1b[{};{}H", y + 1, x + 1).as_bytes());
    out.extend(dormant.input_mode_formatted());
    if dormant.hide_cursor() {
        out.extend_from_slice(b"\x1b[?25l");
    }
    out.extend(dormant.attributes_formatted());
    out
}

/// A parser holding the pane `snapshot` describes, for a reader that wants it
/// as it stands (the content search) and not as the pane's own grid.
pub fn parser_from_snapshot(snapshot: &crate::agent::control_mode::PaneSnapshot) -> SessionParser {
    let (rows, cols) = vt_floor(snapshot.rows, snapshot.cols);
    let blank = vt100::Parser::new(DORMANT_GRID.0, DORMANT_GRID.1, 0);
    let mut parser = vt100::Parser::new_with_callbacks(
        rows,
        cols,
        crate::session::settings::global().scrollback_lines,
        TermSignals::default(),
    );
    parser.process(&snapshot_seed(snapshot, blank.screen()));
    parser
}

/// The wired I/O every pane kind shares: the vt100 parser the reader loop
/// feeds, the writer channel, the exit flag, the output stamp, and the backend
/// pane they belong to. [`ShellPane`], [`ProgramPane`] and [`Session`] each
/// *embed* one (composition — a trait would only re-declare these fields) and
/// `Deref` to it, so existing call sites (`session.parser`,
/// `shell.backend_id`, `pane.send_input(..)`) keep working unchanged while
/// the accessors exist once.
pub struct WiredPane {
    pub parser: Arc<Mutex<SessionParser>>,
    input_tx: mpsc::Sender<Vec<u8>>,
    /// The backend pane this is. For a shell pane it is read so it can be
    /// *persisted*: the window outlives the interface, and re-adopting it on
    /// the next start is what stops a restart forgetting the shell and
    /// orphaning its window. A program pane's is deliberately not persisted —
    /// its window *name* is the identity that survives a restart
    /// (`tmux::program_window_name`), because a deterministic name cannot go
    /// stale where a stored id can.
    pub(crate) backend_id: String,
    exited: Arc<AtomicBool>,
    last_output_at: Arc<AtomicU64>,
    /// Chunks of live output fed to the parser so far — the redraw signal.
    output_count: Arc<AtomicU64>,
    /// Which pane kind this is, for input-channel error messages.
    label: &'static str,
    /// Whether `parser` holds this pane's grid right now — see [`Self::evict`].
    residency: Arc<Residency>,
    /// The backend input is claimed through — `None` where there is no live
    /// pane to size (a placeholder, a test stub).
    backend: Option<Arc<dyn SessionBackend>>,
    /// The pane's real size, where the backend reports it.
    size: Option<PaneSize>,
    /// The size last asked for from the rect this pane is painted into
    /// ([`pack_size`]; 0 before the first).
    wanted: AtomicU32,
}

impl WiredPane {
    /// Send input, taking the pane's size first when it is not the size of the
    /// rect it is painted into here.
    ///
    /// Input is what says which instance the user is at: on a pane several
    /// instances show, the one being typed into sizes it and the others follow
    /// (tmux's own `window-size latest`, with keystrokes as the activity). A
    /// pane already at this instance's size needs nothing, so a claim is one
    /// atomic comparison per keystroke and a message only when it changes
    /// something.
    pub fn send_input(&self, data: Vec<u8>) -> Result<()> {
        self.claim_size();
        send_to_input_channel(&self.input_tx, data, self.label)
    }

    fn claim_size(&self) {
        let (Some(backend), Some(size)) = (&self.backend, &self.size) else {
            return;
        };
        let wanted = self.wanted.load(Ordering::Relaxed);
        let Some((rows, cols)) = unpack_size(wanted) else {
            return;
        };
        if size.current() == wanted {
            return;
        }
        if let Err(e) = backend.claim_size(&self.backend_id, rows, cols) {
            debug!(pane = %self.backend_id, "could not claim the pane's size: {e:#}");
        }
    }

    /// Take the pane's size back when the client that was sizing it has gone.
    ///
    /// Nothing else would: this instance's rect has not changed, so the render
    /// path has no reason to ask, and a pane left at the departed instance's
    /// size would stay letterboxed until the next keystroke. Called by the
    /// render path per painted pane, and costs it one atomic load unless there
    /// is something to do — which is one resize, to the size this instance
    /// already asked for.
    pub fn retake_size(&self) {
        let (Some(backend), Some(size)) = (&self.backend, &self.size) else {
            return;
        };
        if !size.take_released() {
            return;
        }
        let wanted = self.wanted.load(Ordering::Relaxed);
        let Some((rows, cols)) = unpack_size(wanted) else {
            return;
        };
        if size.current() == wanted {
            return;
        }
        if let Err(e) = backend.resize(&self.backend_id, rows, cols) {
            debug!(pane = %self.backend_id, "could not take the pane's size back: {e:#}");
        }
    }

    /// Whether the pane is being sized by another client — a hint for saying
    /// why the grid is not the size of its rect. See [`PaneSize::sized_elsewhere`].
    pub fn sized_elsewhere(&self) -> bool {
        self.size.as_ref().is_some_and(PaneSize::sized_elsewhere)
    }

    /// When this pane last produced output, as epoch milliseconds — a clock,
    /// read as "how long has it been quiet" (the printing spinner, the
    /// stuck-`working` fallback), stored only once the parser holds that output.
    pub fn last_output_at(&self) -> u64 {
        self.last_output_at.load(Ordering::Relaxed)
    }

    /// How many chunks of live output the parser has taken, which is the
    /// lock-free redraw signal: a renderer compares it against the count it last
    /// painted at, so a quiet pane costs one atomic load instead of a repaint.
    /// A count and not the clock above, because two chunks inside one
    /// millisecond are two changes, and moved only after the parser holds the
    /// chunk, so a paint it triggers never shows the grid from before it. What
    /// [`crate::kernel::terminal::Terminals::output_generation`] sums.
    pub fn output_count(&self) -> u64 {
        self.output_count.load(Ordering::Acquire)
    }

    /// Whether the pane's process/stream has ended.
    ///
    /// Read from the reader loop's flag rather than by asking the backend, so
    /// the answer costs an atomic load on a render path. What it enables is
    /// reporting "this exited" instead of painting the frozen grid it left
    /// behind.
    pub fn has_exited(&self) -> bool {
        self.exited.load(Ordering::SeqCst)
    }

    /// Force the "stream ended" state, for tests of what replaces a shell.
    #[cfg(test)]
    pub fn mark_exited_for_test(&self) {
        self.exited.store(true, Ordering::SeqCst);
    }

    /// The backend-specific pane identifier.
    pub fn backend_id(&self) -> &str {
        &self.backend_id
    }

    /// Ask the backend to size the pane to its rect, and size the local vt100
    /// grid with it where the backend cannot report the pane's real size.
    ///
    /// Floored for the reason `vt_floor` documents: a cramped layout really
    /// does compute a one-cell rect, and a grid that small is where vt100
    /// underflows on the next byte written into it. A pane with no grid is
    /// resized at the backend only: its grid comes back at whatever size the
    /// pane has when it does.
    ///
    /// Where the backend reports the pane's real size, the grid is left to that
    /// report rather than set here: the resize may be declined (another client
    /// is sizing the pane), and when it is not, the report arrives in the
    /// output stream at the point the program starts writing for the new size.
    pub fn resize(&self, backend: &dyn SessionBackend, rows: u16, cols: u16) -> Result<()> {
        let (rows, cols) = vt_floor(rows, cols);
        self.wanted.store(pack_size(rows, cols), Ordering::Relaxed);
        backend.resize(&self.backend_id, rows, cols)?;
        if self.size.is_none() {
            if let Ok(mut parser) = self.parser.lock() {
                if self.residency.is_resident() {
                    parser.screen_mut().set_size(rows, cols);
                }
            }
        }
        Ok(())
    }

    /// Whether the parser holds this pane's grid, rather than the two cells it
    /// is left with off screen ([`Self::evict`]).
    pub fn is_resident(&self) -> bool {
        self.residency.is_resident()
    }

    /// [`Self::output_count`], moved as well whenever the grid is rebuilt: the
    /// stamp for anything read off the grid, which a rebuild changes without
    /// the pane printing anything.
    ///
    /// Over the count rather than the `last_output_at` clock, for the reason
    /// the count exists: two chunks inside one millisecond are two changes, and
    /// the count moves only once the parser holds the chunk.
    pub fn content_stamp(&self) -> u64 {
        self.output_count()
            .wrapping_add(self.residency.changes.load(Ordering::Acquire))
    }

    /// Drop this pane's grid and history, keeping two cells that go on
    /// reading its output for what does not need a grid: the title, a bell, a
    /// notification, the input modes, and when it last printed. Returns
    /// whether there was a grid to drop.
    ///
    /// Only for a pane whose backend can give the grid back exactly
    /// ([`SessionBackend::supports_snapshots`]): [`Self::restore`] asks for it,
    /// and the reader loop rebuilds it where the answer lands in the stream.
    pub fn evict(&self) -> bool {
        let Ok(mut parser) = self.parser.lock() else {
            return false;
        };
        if !self.residency.is_resident() {
            return false;
        }
        let mut signals = std::mem::take(parser.callbacks_mut());
        // Positions on a grid that is going; the rebuild records them again
        // from whatever links the capture carries.
        signals.hyperlinks = HyperlinkTable::default();
        signals.pending_link = None;
        let carried = carried_state(parser.screen());
        let mut dormant =
            vt100::Parser::new_with_callbacks(DORMANT_GRID.0, DORMANT_GRID.1, 0, signals);
        dormant.process(&carried);
        *parser = dormant;
        self.residency.dormant.store(true, Ordering::Release);
        true
    }

    /// Ask for this pane's grid back, unless it has one or has already asked
    /// within `SNAPSHOT_RETRY_MS`. Returns at once, and whether this call is
    /// the one that asked; the grid arrives on the reader thread
    /// ([`Self::wait_resident`]).
    pub fn restore(&self, backend: &dyn SessionBackend) -> bool {
        if self.is_resident() {
            return false;
        }
        let now = now_millis();
        let asked = self.residency.requested_at.load(Ordering::Acquire);
        if asked != 0 && now.saturating_sub(asked) < SNAPSHOT_RETRY_MS {
            return false;
        }
        match backend.request_snapshot(&self.backend_id) {
            Ok(()) => {
                self.residency.requested_at.store(now, Ordering::Release);
                true
            }
            Err(e) => {
                debug!(pane = %self.backend_id, "could not ask for a snapshot: {e:#}");
                false
            }
        }
    }

    /// Wait up to `budget` for the grid [`Self::restore`] asked for. Returns
    /// whether the pane has one.
    pub fn wait_resident(&self, budget: std::time::Duration) -> bool {
        let deadline = std::time::Instant::now() + budget;
        let (lock, landed) = &self.residency.landed;
        let Ok(mut guard) = lock.lock() else {
            return self.is_resident();
        };
        while !self.is_resident() {
            let left = deadline.saturating_duration_since(std::time::Instant::now());
            if left.is_zero() {
                break;
            }
            guard = match landed.wait_timeout(guard, left) {
                Ok((guard, _)) => guard,
                Err(_) => return self.is_resident(),
            };
        }
        self.is_resident()
    }
}

/// A companion shell pane running alongside an agent session.
pub struct ShellPane {
    wired: WiredPane,
}

impl std::ops::Deref for ShellPane {
    type Target = WiredPane;
    fn deref(&self) -> &WiredPane {
        &self.wired
    }
}

/// A program a plugin asked for, running in a pane of its own.
///
/// The same wired I/O as [`ShellPane`], reached through the same
/// `Session::wire_up`, so the subtle part (a `vt100` parser fed by a reader
/// task, a writer channel, an exit flag and an output stamp) exists once. What it
/// is *not* is a shell, and not a session: it belongs to a plugin, holds a program
/// that plugin named, and carries its own backend handle so it can be resized and
/// killed without a `Session` to route through.
pub struct ProgramPane {
    wired: WiredPane,
    /// Kept so the pane can resize and kill itself.
    backend: Arc<dyn SessionBackend>,
    /// What is running, for reporting.
    pub program: String,
}

impl std::ops::Deref for ProgramPane {
    type Target = WiredPane;
    fn deref(&self) -> &WiredPane {
        &self.wired
    }
}

impl ProgramPane {
    /// Start `program` in a new pane on `backend`.
    #[allow(clippy::too_many_arguments)]
    pub fn spawn(
        backend: Arc<dyn SessionBackend>,
        window_name: &str,
        program: &str,
        args: &[String],
        cwd: Option<&Path>,
        env: &HashMap<String, String>,
        rows: u16,
        cols: u16,
    ) -> Result<Self> {
        let spawned = backend.spawn(window_name, program, args, cwd, env, rows, cols)?;
        // No session id: a program belongs to a plugin, and the role is what
        // keeps its window from ever resolving as somebody's agent.
        if let Err(e) = backend.stamp_window(
            &spawned.backend_id,
            "",
            crate::agent::tmux::WindowRole::Program,
        ) {
            debug!("could not stamp the program window {window_name}: {e:#}");
        }
        let (wired, _signals) =
            Session::wire_up(rows, cols, SessionIo::spawned(spawned), &backend, "Program");
        debug!(program, window_name, "spawned a plugin's program pane");
        Ok(Self {
            wired,
            backend,
            program: program.to_string(),
        })
    }

    /// Reconnect to a pane that is already running — the restart path, where the
    /// window was found again by its deterministic name.
    pub fn adopt(
        backend: Arc<dyn SessionBackend>,
        backend_id: &str,
        program: &str,
        rows: u16,
        cols: u16,
    ) -> Result<Self> {
        let adopted = backend.adopt(backend_id, rows, cols, None)?;
        let (wired, _signals) = Session::wire_up(
            rows,
            cols,
            SessionIo::adopted(adopted, backend_id),
            &backend,
            "Program",
        );
        debug!(program, backend_id, "adopted a plugin's program pane");
        Ok(Self {
            wired,
            backend,
            program: program.to_string(),
        })
    }

    /// Whether the pane was resized. A caller that memoizes the size it asked
    /// for reads this, so a refused resize is asked again on the next frame
    /// rather than remembered as done — see [`Session::resize`].
    pub fn resize(&self, rows: u16, cols: u16) -> bool {
        if let Err(e) = self.wired.resize(self.backend.as_ref(), rows, cols) {
            tracing::debug!(
                "could not resize program pane {}: {e:#}",
                self.wired.backend_id
            );
            return false;
        }
        true
    }

    /// End the program and take its window with it.
    pub fn kill(&self) {
        if let Err(e) = self.backend.kill(&self.wired.backend_id) {
            tracing::warn!(
                "could not kill program pane {}: {e:#}",
                self.wired.backend_id
            );
        }
    }
}

/// The bare host name for a session's off-local backend (e.g. `devbox` for an
/// `ssh:devbox` backend, `Ubuntu` for a `wsl:Ubuntu` backend), or `None` for
/// local backends. Drives the session list's remote indicator.
fn remote_host_from_backend(backend: &Arc<dyn SessionBackend>) -> Option<String> {
    let name = backend.name();
    name.strip_prefix(crate::session::SSH_BACKEND_PREFIX)
        .or_else(|| name.strip_prefix(crate::session::WSL_BACKEND_PREFIX))
        .map(str::to_string)
}

/// A running session connected to a backend.
pub struct Session {
    pub info: SessionInfo,
    wired: WiredPane,
    backend: Arc<dyn SessionBackend>,
    provider: Arc<dyn AgentProvider>,
    /// The reader thread's title / attention / notification cells, shared with
    /// the parser's [`TermSignals`] (which bumps `meta_gen` on every write).
    signals: SignalCells,
    /// The generation last consumed by [`Self::sync_agent_meta`]. Starts at
    /// `u64::MAX` so the first tick always syncs.
    last_synced_meta_gen: u64,
    /// `now_millis()` of the last attention acknowledgement (set while the
    /// session is the active one). Attention is pending when
    /// `attention_at > attention_ack_at`.
    attention_ack_at: u64,
    pub shell_pane: Option<ShellPane>,
    /// Session environment variables, passed to shell pane spawns.
    env: HashMap<String, String>,
    /// True for a **placeholder** session: a persisted remote session whose host
    /// is currently unreachable, so it has no live backend pane / reader / writer
    /// (its `input_tx` is a dead channel and its `parser` holds a static "host
    /// unreachable" notice). Rendered with `SessionState::Unreachable` and
    /// replaced in place by the real adopted session once the host recovers.
    ///
    /// **Unused in v2.** Its only caller was v1's remote-restore loop, which went
    /// with `src/app`; the kernel derives `Unreachable` from "a remote session
    /// with no live pane" instead (`kernel::snapshot::with_reachability`), so
    /// nothing constructs a placeholder and this is only ever `false`.
    placeholder: bool,
}

impl std::ops::Deref for Session {
    type Target = WiredPane;
    fn deref(&self) -> &WiredPane {
        &self.wired
    }
}

impl Session {
    /// Whether the backend reports this session's pane as exited.
    ///
    /// A round trip, unlike [`WiredPane::has_exited`]: that atomic flag flips
    /// on the reader's EOF, and a window kept by `remain-on-exit` never
    /// delivers one — the pane an agent exited out of is still there, and only
    /// the backend can still be asked. Off the render path for that cost;
    /// called on the deliberate chord that must tell a dead pane from a live
    /// one.
    pub fn is_dead(&self) -> Result<bool> {
        self.backend.is_dead(self.backend_id())
    }

    /// Whether the backend reports this session's companion shell pane as dead.
    ///
    /// `None` when there is no shell pane. Asked of the shell's own backend id,
    /// not the agent's: the two panes die apart (an agent can `/exit` while its
    /// shell runs on, and the reverse), so a caller acting on the surface the
    /// user is looking at must ask the pane that surface names — the same pane
    /// [`crate::kernel::terminal::Terminals::send`] would deliver to.
    pub fn shell_is_dead(&self) -> Option<Result<bool>> {
        self.shell_pane
            .as_ref()
            .map(|shell| self.backend.is_dead(shell.backend_id()))
    }

    /// Spawn a new session via the given backend.
    pub fn spawn(
        name: String,
        rows: u16,
        cols: u16,
        config: &SessionConfig,
        backend: &Arc<dyn SessionBackend>,
        provider: &Arc<dyn AgentProvider>,
    ) -> Result<Self> {
        let args = provider.build_args(config);
        let window_name = crate::agent::tmux::agent_window_name(&name);

        let env = config.env.clone();

        let spawned = backend.spawn(
            &window_name,
            provider.command(),
            &args,
            config.cwd.as_deref(),
            &env,
            rows,
            cols,
        )?;

        let mut info = SessionInfo::new(name);
        // Reuse the caller-supplied id when present (stable identity across a
        // respawn; matches the `THURBOX_SESSION` env injected before launch).
        if let Some(id) = config.session_id {
            info.id = id;
        }
        info.agent_session_id = config.agent_session_id.clone();
        info.cwd = config.cwd.clone();
        if !config.agent.is_empty() {
            info.agent = config.agent.clone();
        }
        info.backend_id = Some(spawned.backend_id.clone());
        stamp(backend, &spawned.backend_id, info.id, WindowRole::Agent);
        info.remote_host = remote_host_from_backend(backend);
        debug!(session_id = %info.id, backend_id = %spawned.backend_id, "Spawned session via backend");

        Ok(Self::wire_io(
            info,
            rows,
            cols,
            SessionIo::spawned(spawned),
            backend,
            provider,
            env,
        ))
    }

    /// Reconnect to an existing backend session. `seed` is the optional
    /// pre-captured pane state (see [`SessionBackend::capture_history`]);
    /// `None` = the backend captures it during the adopt.
    #[allow(clippy::too_many_arguments)]
    pub fn adopt(
        name: String,
        rows: u16,
        cols: u16,
        backend_id: &str,
        backend: &Arc<dyn SessionBackend>,
        provider: &Arc<dyn AgentProvider>,
        env: HashMap<String, String>,
        seed: Option<Vec<u8>>,
    ) -> Result<Self> {
        let adopted = backend.adopt(backend_id, rows, cols, seed)?;

        debug!(
            backend_id = %backend_id,
            parser_rows = rows,
            parser_cols = cols,
            "Adopting session"
        );

        let mut info = SessionInfo::new(name);
        info.backend_id = Some(backend_id.to_string());
        info.remote_host = remote_host_from_backend(backend);
        debug!(session_id = %info.id, backend_id = %backend_id, "Adopted session via backend");

        Ok(Self::wire_io(
            info,
            rows,
            cols,
            SessionIo::adopted(adopted, backend_id),
            backend,
            provider,
            env,
        ))
    }

    /// [`Self::adopt`] for a session nobody is looking at: no history is
    /// captured and no grid is built — only its title is replayed. The parser starts as the two cells
    /// [`WiredPane::evict`] leaves, and the grid is fetched on the first
    /// [`WiredPane::restore`] — which, for a session that is never shown, is
    /// never. Only for a backend that
    /// [`supports_snapshots`](SessionBackend::supports_snapshots).
    #[allow(clippy::too_many_arguments)]
    pub fn adopt_dormant(
        name: String,
        rows: u16,
        cols: u16,
        backend_id: &str,
        backend: &Arc<dyn SessionBackend>,
        provider: &Arc<dyn AgentProvider>,
        env: HashMap<String, String>,
    ) -> Result<Self> {
        // Only the title, not `None`: `None` is the backend capturing the
        // whole history itself. The title goes through the stream like any
        // other, so the two cells' callbacks report it and `seed_len` keeps it
        // from reading as output.
        let adopted =
            backend.adopt(backend_id, rows, cols, Some(backend.title_seed(backend_id)))?;
        let mut info = SessionInfo::new(name);
        info.backend_id = Some(backend_id.to_string());
        info.remote_host = remote_host_from_backend(backend);
        debug!(session_id = %info.id, backend_id = %backend_id, "Adopted session without a grid");
        let (wired, signals) = Self::wire_up(
            rows,
            cols,
            SessionIo {
                resident: false,
                ..SessionIo::adopted(adopted, backend_id)
            },
            backend,
            "Session",
        );
        Ok(Self {
            info,
            wired,
            backend: Arc::clone(backend),
            provider: Arc::clone(provider),
            signals,
            last_synced_meta_gen: u64::MAX,
            attention_ack_at: 0,
            shell_pane: None,
            env,
            placeholder: false,
        })
    }

    /// Whether this session's backend can give a dropped grid back
    /// ([`WiredPane::evict`]).
    pub fn supports_snapshots(&self) -> bool {
        !self.placeholder && self.backend.supports_snapshots()
    }

    /// The agent's pane, or its companion shell's.
    fn wired_pane(&self, shell: bool) -> Option<&WiredPane> {
        if shell {
            self.shell_pane.as_ref().map(|pane| &pane.wired)
        } else {
            Some(&self.wired)
        }
    }

    /// [`WiredPane::evict`] one of this session's panes.
    pub fn evict_pane(&self, shell: bool) -> bool {
        self.supports_snapshots() && self.wired_pane(shell).is_some_and(WiredPane::evict)
    }

    /// [`WiredPane::restore`] one of this session's panes, reporting whether
    /// this call asked.
    pub fn restore_pane(&self, shell: bool) -> bool {
        self.wired_pane(shell)
            .is_some_and(|pane| pane.restore(self.backend.as_ref()))
    }

    /// Create parser, spawn reader/writer loops for the given I/O handles.
    /// `label` names the pane kind in input-channel error messages.
    fn wire_up(
        rows: u16,
        cols: u16,
        io: SessionIo,
        backend: &Arc<dyn SessionBackend>,
        label: &'static str,
    ) -> (WiredPane, SignalCells) {
        let signals = SignalCells::new();
        // The floor applies at construction too: a session first painted into a
        // one-column pane would otherwise panic on its first line of output,
        // before any resize could correct it.
        let (rows, cols) = vt_floor(rows, cols);
        let (residency, (rows, cols), scrollback) = if io.resident {
            (
                Residency::default(),
                (rows, cols),
                crate::session::settings::global().scrollback_lines,
            )
        } else {
            (Residency::dormant(), DORMANT_GRID, 0)
        };
        let residency = Arc::new(residency);
        let parser = Arc::new(Mutex::new(vt100::Parser::new_with_callbacks(
            rows,
            cols,
            scrollback,
            signals.term_signals(),
        )));

        let exited = Arc::new(AtomicBool::new(false));
        let last_output_at = Arc::new(AtomicU64::new(initial_output_at(io.mode)));
        let output_count = Arc::new(AtomicU64::new(0));

        let (input_tx, input_rx) = mpsc::channel(INPUT_CHANNEL_CAPACITY);
        tokio::spawn(Self::writer_loop(io.input, input_rx));

        let reader_state = ReaderState::new(
            Arc::clone(&parser),
            Arc::clone(&exited),
            Arc::clone(&last_output_at),
            Arc::clone(&residency),
            Arc::clone(&output_count),
            io.size.clone(),
        );
        let seed_len = io.seed_len;
        tokio::task::spawn_blocking(move || {
            Self::reader_loop(io.output, reader_state, seed_len);
        });

        let wired = WiredPane {
            parser,
            input_tx,
            backend_id: io.backend_id,
            exited,
            last_output_at,
            output_count,
            label,
            residency,
            backend: Some(Arc::clone(backend)),
            size: io.size,
            // The rect the caller is about to paint into, which the render
            // path's own memo starts from too — so the first claim knows it.
            wanted: AtomicU32::new(pack_size(rows, cols)),
        };
        (wired, signals)
    }

    /// Wire up parser, reader loop, and writer loop for a new session.
    fn wire_io(
        info: SessionInfo,
        rows: u16,
        cols: u16,
        io: SessionIo,
        backend: &Arc<dyn SessionBackend>,
        provider: &Arc<dyn AgentProvider>,
        env: HashMap<String, String>,
    ) -> Self {
        let (wired, signals) = Self::wire_up(rows, cols, io, backend, "Session");
        Self {
            info,
            wired,
            backend: Arc::clone(backend),
            provider: Arc::clone(provider),
            signals,
            last_synced_meta_gen: u64::MAX,
            attention_ack_at: 0,
            shell_pane: None,
            env,
            placeholder: false,
        }
    }

    /// Build a **placeholder** session for a persisted remote session whose host
    /// is currently unreachable. It carries no live backend pane: the reader /
    /// writer loops are never spawned, `input_tx` is a dead channel (keystrokes
    /// are silently dropped), and the `parser` is seeded with a static notice.
    /// The row renders like any other (grouping/ordering/nesting all key off
    /// `info`) but shows [`SessionState::Unreachable`](crate::session::SessionState::Unreachable)
    /// — derived from the attach failure by
    /// [`with_reachability`](crate::session::with_reachability) — until the host
    /// recovers and [`Self::adopt`] replaces it in place.
    pub fn placeholder(
        mut info: SessionInfo,
        rows: u16,
        cols: u16,
        backend: &Arc<dyn SessionBackend>,
        provider: &Arc<dyn AgentProvider>,
        env: HashMap<String, String>,
    ) -> Self {
        info.backend_id = None;

        let signals = SignalCells::new();
        let (rows, cols) = vt_floor(rows, cols);
        let parser = Arc::new(Mutex::new(vt100::Parser::new_with_callbacks(
            rows,
            cols,
            crate::session::settings::global().scrollback_lines,
            signals.term_signals(),
        )));
        let host = info.remote_host.clone().unwrap_or_else(|| "?".into());
        let notice = format!(
            "\r\n  \u{2298} Remote host '{host}' unreachable \u{2014} retrying\u{2026}\r\n\r\n  \
             This session will reconnect automatically when the host comes back.\r\n  \
             Press restart to retry now, or delete to remove it.\r\n"
        );
        if let Ok(mut p) = parser.lock() {
            p.process(notice.as_bytes());
        }

        // A dead input channel: the receiver is dropped immediately, so any
        // keystroke `try_send` fails fast and the byte is discarded.
        let (input_tx, _dead_rx) = mpsc::channel(INPUT_CHANNEL_CAPACITY);

        Self {
            info,
            wired: WiredPane {
                parser,
                input_tx,
                backend_id: String::new(),
                exited: Arc::new(AtomicBool::new(false)),
                last_output_at: Arc::new(AtomicU64::new(0)),
                output_count: Arc::new(AtomicU64::new(0)),
                label: "Session",
                residency: Arc::default(),
                backend: None,
                size: None,
                wanted: AtomicU32::new(0),
            },
            backend: Arc::clone(backend),
            provider: Arc::clone(provider),
            signals,
            last_synced_meta_gen: u64::MAX,
            attention_ack_at: 0,
            shell_pane: None,
            env,
            placeholder: true,
        }
    }

    /// Whether this is a placeholder for an unreachable remote session (no live
    /// backend pane). See [`Self::placeholder`].
    pub fn is_placeholder(&self) -> bool {
        self.placeholder
    }

    /// Blocking read loop feeding the vt100 parser. Runs on a
    /// `spawn_blocking` thread and exits only on EOF/error from `reader`.
    /// Lifecycle contract: every path that retires a `Session` must call
    /// `kill()`/`detach()` so the backend unregisters the pane and this
    /// thread sees EOF — a silent drop leaks the thread (blocked in a read)
    /// for the process lifetime.
    ///
    /// `seed_len` is the number of bytes at the front of `reader` that are
    /// replayed history (an adopt's scrollback seed, chained ahead of the
    /// live stream — see [`crate::agent::tmux::TmuxBackend::adopt`]) rather
    /// than genuine pane activity. Those bytes still reach the parser, but
    /// must not stamp `last_output_at`: a restart/reattach replaying hours of
    /// scrollback would otherwise look identical to the agent having just
    /// printed, defeating [`initial_output_at`]'s deliberate staleness for
    /// `WireMode::Adopt` the instant the reader's first bytes arrive.
    ///
    /// A snapshot the pane asked for ([`WiredPane::restore`]) arrives here too,
    /// between two reads, at exactly the point of the stream it describes — so
    /// installing it before the next read is what keeps a rebuilt grid from
    /// losing or repeating a byte. A partial character held in `carry` stays
    /// held: its first bytes came before the snapshot, which cannot show a
    /// character tmux has not finished either, and the rest come after.
    fn reader_loop(mut reader: Box<dyn Read + Send>, state: ReaderState, mut seed_len: usize) {
        let ReaderState {
            parser,
            exited,
            last_output_at,
            residency,
            output_count,
            size,
        } = state;
        let mut buf = [0u8; 4096];
        // Bytes of a trailing, not-yet-complete UTF-8 character held back from
        // the previous read. The agent's output is a single byte stream, but
        // the OS read boundary (and tmux's `%output` framing) can fall in the
        // middle of a multi-byte character. vt100 is NOT robust to a `process()`
        // chunk that ends mid-codepoint — it can swallow a following control
        // byte (e.g. a newline), misplacing later output — so we never hand it a
        // truncated tail. `carry` is at most 3 bytes (a 4-byte char missing one).
        let mut carry: Vec<u8> = Vec::new();
        loop {
            let read = reader.read(&mut buf);
            // Taken between reads, never during one: a reported size is handed
            // over as an interrupted read of its own (`ControlModeReader`), so
            // every byte before it is already in the grid and none after it is.
            // A pane with no grid has nothing to resize; the grid it gets back
            // is built at the pane's size as tmux reports it then.
            if let Some((rows, cols)) = size.as_ref().and_then(PaneSize::take) {
                let (rows, cols) = vt_floor(rows, cols);
                if let Ok(mut p) = parser.lock() {
                    if residency.is_resident() {
                        p.screen_mut().set_size(rows, cols);
                    }
                }
            }
            match read {
                Ok(0) => {
                    debug!("Session reader: EOF");
                    break;
                }
                Ok(n) => {
                    let mut data = std::mem::take(&mut carry);
                    data.extend_from_slice(&buf[..n]);
                    let ready = utf8_ready_prefix_len(&data);
                    carry = data.split_off(ready);
                    Self::feed_parser(&parser, &data);
                    // Bytes beyond the seed boundary are live activity; a chunk
                    // that is entirely within the seed is not. Marked after the
                    // feed, never before: the count is the loop's cue to repaint,
                    // and a paint between the two would show the old grid with
                    // the cue already spent.
                    if seed_len < n {
                        last_output_at.store(now_millis(), Ordering::Relaxed);
                        output_count.fetch_add(1, Ordering::Release);
                    }
                    seed_len = seed_len.saturating_sub(n);
                }
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {
                    if let Some(snapshot) = crate::agent::control_mode::SnapshotArrived::take(e) {
                        Self::install(&parser, &residency, &snapshot);
                    }
                }
                Err(e) => {
                    debug!("Session reader error: {e}");
                    break;
                }
            }
        }
        // Stream ended (EOF or error): flush any leftover partial UTF-8 sequence,
        // since no more bytes are coming to complete it.
        Self::feed_parser(&parser, &carry);
        exited.store(true, Ordering::SeqCst);
    }

    /// Rebuild a dropped grid from `snapshot`, in place of the two cells that
    /// stood in for it, and wake whoever is waiting for it.
    ///
    /// A pane that has its grid already ignores a snapshot: that grid is
    /// exact, and one rebuilt from a capture is only as exact as tmux's.
    fn install(
        parser: &Mutex<SessionParser>,
        residency: &Residency,
        snapshot: &crate::agent::control_mode::PaneSnapshot,
    ) {
        if let Ok(mut parser) = parser.lock() {
            if !residency.is_resident() {
                let seed = snapshot_seed(snapshot, parser.screen());
                let signals = std::mem::take(parser.callbacks_mut());
                let (rows, cols) = vt_floor(snapshot.rows, snapshot.cols);
                let mut rebuilt = vt100::Parser::new_with_callbacks(
                    rows,
                    cols,
                    crate::session::settings::global().scrollback_lines,
                    signals,
                );
                rebuilt.process(&seed);
                *parser = rebuilt;
                residency.dormant.store(false, Ordering::Release);
                residency.changes.fetch_add(1, Ordering::AcqRel);
            }
        }
        residency.requested_at.store(0, Ordering::Release);
        let (lock, landed) = &residency.landed;
        // Taken so a waiter cannot check the flag, miss this wake-up and then
        // sleep out its budget.
        let _guard = lock.lock();
        landed.notify_all();
    }

    /// Hand `bytes` to the parser, unless there are none — which takes no lock.
    fn feed_parser(parser: &Mutex<SessionParser>, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        if let Ok(mut p) = parser.lock() {
            p.process(bytes);
        }
    }

    async fn writer_loop(mut writer: Box<dyn Write + Send>, mut input_rx: mpsc::Receiver<Vec<u8>>) {
        while let Some(data) = input_rx.recv().await {
            if let Err(e) = writer.write_all(&data) {
                error!("Session writer error: {e}");
                break;
            }
            if let Err(e) = writer.flush() {
                error!("Session flush error: {e}");
                break;
            }
        }
        debug!("Session writer task exiting");
    }

    /// Resize the agent's own pane and grid — and **only** that one.
    /// (`send_input` / `has_exited` / `last_output_at` / `backend_id` come from
    /// the embedded [`WiredPane`].)
    ///
    /// Reports whether it reached the backend. The command is sent rather than
    /// asked (see `TmuxBackend::resize`), so what can still fail is the sending
    /// — a control lock held past the loop's budget. The render path memoizes
    /// the size it asked for to keep a round trip off every frame, and a
    /// memoized failure would leave the agent wrapping at the old width with
    /// nothing to trigger another attempt, so it memoizes only on `true`.
    ///
    /// The companion shell is sized by [`Self::resize_shell`] and by nothing
    /// else. This used to size both, on the assumption that the two take turns
    /// in one rect — true while the shell was a second tab of the centre, and
    /// false the moment an arrangement gives the shell a slot of its own. A
    /// shell in a 35% column then dragged the agent holding `center` down to
    /// its width, every frame, and the agent rendered at the shell's size
    /// (#1220). A pane's size comes from the rect that pane is painted into;
    /// there is no size the two share.
    pub fn resize(&self, rows: u16, cols: u16) -> bool {
        // A placeholder has no live pane; only resize its local notice buffer.
        // Talking to the (possibly-down) backend here would issue a blocking
        // ssh resize on the UI thread — the freeze we're avoiding. Floored for
        // the reason `WiredPane::resize` (the live path) documents.
        if self.placeholder {
            let (rows, cols) = vt_floor(rows, cols);
            if let Ok(mut parser) = self.wired.parser.lock() {
                parser.screen_mut().set_size(rows, cols);
            }
            // Nothing to reach, and nothing to ask again for.
            return true;
        }
        if let Err(e) = self.wired.resize(self.backend.as_ref(), rows, cols) {
            tracing::warn!("Failed to resize session: {e}");
            return false;
        }
        true
    }

    /// Resize the companion shell's pane and grid, if there is one.
    ///
    /// The shell's half of [`Self::resize`], separate because the two panes are
    /// separate surfaces: each is sized from the rect it was painted into, and
    /// neither's geometry is derived from the other's. Reports what that one
    /// reports, and for the same reason — the render path memoizes only a size
    /// that reached the backend.
    pub fn resize_shell(&self, rows: u16, cols: u16) -> bool {
        let Some(shell) = &self.shell_pane else {
            // No shell, so nothing to reach and nothing left at a stale width.
            return true;
        };
        // [`Self::resize`]'s placeholder branch, spelled out here too — grid
        // included. Reporting `true` without sizing the local buffer would let
        // the render path memoize a size the parser never took, leaving the
        // pane wrapped at the old width with nothing to ask again: the #1220
        // failure, one pane down. Nothing constructs a placeholder in v2 (see
        // the field), so this costs a branch and buys the two halves being
        // unable to come apart if one ever does.
        if self.placeholder {
            let (rows, cols) = vt_floor(rows, cols);
            if let Ok(mut parser) = shell.parser.lock() {
                parser.screen_mut().set_size(rows, cols);
            }
            return true;
        }
        if let Err(e) = shell.wired.resize(self.backend.as_ref(), rows, cols) {
            tracing::warn!("Failed to resize shell pane: {e}");
            return false;
        }
        true
    }

    /// Force the session into the "process exited" state, for tests that need to
    /// exercise the exited → `Idle` status branch.
    #[cfg(test)]
    pub fn mark_exited_for_test(&self) {
        self.wired.exited.store(true, Ordering::SeqCst);
    }

    /// Backdate the session's last-output timestamp by `ms`, for tests that need
    /// to exercise the output-quiescence fallback (a stuck `working` state going
    /// quiet → `Idle`).
    #[cfg(test)]
    pub fn backdate_output_for_test(&self, ms: u64) {
        let now = now_millis();
        self.wired
            .last_output_at
            .store(now.saturating_sub(ms), Ordering::Relaxed);
    }

    pub fn millis_since_last_output(&self) -> u64 {
        now_millis().saturating_sub(self.wired.last_output_at.load(Ordering::Relaxed))
    }

    /// Latest OSC window title the agent emitted, if any (live activity text).
    pub fn agent_title(&self) -> Option<String> {
        self.signals.last_title.lock().ok().and_then(|t| t.clone())
    }

    /// Read the agent's title + notification **only when they changed** since
    /// the last call: `None` means unchanged (reuse the previously-synced
    /// values), `Some` carries the fresh pair. The reader thread bumps a
    /// generation counter on every write (`TermSignals::meta_gen`), so the
    /// ~100 Hz status refresh pays one atomic load per session instead of two
    /// mutex locks + two `String` clones (ADR-P10). A generation observed
    /// before its write completes only delays the sync by one ~10 ms tick —
    /// the counter is bumped *after* the value write, never before.
    pub fn sync_agent_meta(&mut self) -> Option<(Option<String>, Option<String>)> {
        let gen = self.signals.meta_gen.load(Ordering::Acquire);
        if gen == self.last_synced_meta_gen {
            return None;
        }
        self.last_synced_meta_gen = gen;
        Some((self.agent_title(), self.notification()))
    }

    /// Whether the agent has signalled for attention (bell / OSC 9 / OSC 777)
    /// since it was last acknowledged. Cleared via [`Self::acknowledge_attention`].
    pub fn needs_attention(&self) -> bool {
        self.signals.attention_at.load(Ordering::Relaxed) > self.attention_ack_at
    }

    /// Message text from the latest attention notification, if any.
    pub fn notification(&self) -> Option<String> {
        self.signals
            .notification
            .lock()
            .ok()
            .and_then(|n| n.clone())
    }

    /// Acknowledge any pending attention signal (called while the session is
    /// the active/selected one — the user is already looking at it).
    pub fn acknowledge_attention(&mut self) {
        self.attention_ack_at = now_millis();
    }

    /// Return the backend name.
    pub fn backend_name(&self) -> &str {
        self.backend.name()
    }

    /// Return the PID of the process running in this session's backend pane.
    pub fn pane_pid(&self) -> Result<Option<u32>> {
        self.backend.pane_pid(&self.wired.backend_id)
    }

    /// Clone the backend handle + id so a background task can query the pane
    /// PID (a control-mode round-trip, slow for remote SSH backends) off the UI
    /// thread. The backend is `Send + Sync`, so the clone is cheap to move.
    pub fn backend_handle(&self) -> (Arc<dyn SessionBackend>, String) {
        (Arc::clone(&self.backend), self.wired.backend_id.clone())
    }

    /// Replace the provider [`Self::restart`] rebuilds its launch args from.
    ///
    /// A session adopted at startup stores a provider built from the plain
    /// registry def; a later restart must use a def resolved (and, for a remote
    /// backend, arg-adapted) *now* — otherwise the relaunch would resurrect
    /// local config paths the host can't see.
    pub fn set_provider(&mut self, provider: Arc<dyn AgentProvider>) {
        self.provider = provider;
    }

    /// Restart the session: kill the old pane, spawn a fresh one with new config.
    ///
    /// Uses the agent's resume args (when defined) so it picks up the
    /// existing conversation instead of starting fresh.
    pub fn restart(&mut self, config: &SessionConfig, rows: u16, cols: u16) -> Result<()> {
        self.backend.kill(&self.wired.backend_id)?;

        let args = self.provider.build_args(config);
        let window_name = crate::agent::tmux::agent_window_name(&self.info.name);

        let env = config.env.clone();

        let spawned = self.backend.spawn(
            &window_name,
            self.provider.command(),
            &args,
            config.cwd.as_deref(),
            &env,
            rows,
            cols,
        )?;

        stamp(
            &self.backend,
            &spawned.backend_id,
            self.info.id,
            WindowRole::Agent,
        );

        let (wired, _signals) = Self::wire_up(
            rows,
            cols,
            SessionIo::spawned(spawned),
            &self.backend,
            "Session",
        );

        self.wired = wired;
        self.env = config.env.clone();
        self.info.backend_id = Some(self.wired.backend_id.clone());
        if !config.agent.is_empty() {
            self.info.agent = config.agent.clone();
        }

        debug!(session_id = %self.info.id, backend_id = %self.wired.backend_id, "Restarted session");
        Ok(())
    }

    /// Kill/destroy the backend session (for Ctrl+X close).
    pub fn kill(&self) {
        // A placeholder owns no live backend pane (see `placeholder`).
        if self.placeholder {
            return;
        }
        self.kill_shell_pane();
        if let Err(e) = self.backend.kill(&self.wired.backend_id) {
            tracing::warn!("Failed to kill session: {e}");
        }
    }

    /// Detach from the backend session without killing it (for Ctrl+Q quit).
    pub fn detach(self) {
        // A placeholder owns no live backend pane — detaching would issue a
        // blocking ssh call (possibly to a down host) for nothing.
        if self.placeholder {
            return;
        }
        if let Some(shell) = &self.shell_pane {
            if let Err(e) = self.backend.detach(&shell.wired.backend_id) {
                tracing::warn!("Failed to detach shell pane: {e}");
            }
        }
        if let Err(e) = self.backend.detach(&self.wired.backend_id) {
            tracing::warn!("Failed to detach session: {e}");
        }
        drop(self.wired);
        debug!("Session detached");
    }

    /// Lazily spawn a companion shell pane.
    ///
    /// `cwd` is the directory the shell starts in — the caller passes the
    /// session's *launch* cwd (the multi-repo symlink workspace when there is
    /// one, so the shell lands where the agent does), falling back to the
    /// primary repo (`info.cwd`) when `None`. The command is the backend's
    /// [`SessionBackend::default_shell`]. The window name uses the `tbs-` prefix
    /// to distinguish from the agent's `tb-` windows.
    pub fn ensure_shell_pane(
        &mut self,
        rows: u16,
        cols: u16,
        cwd: Option<&std::path::Path>,
    ) -> Result<()> {
        match &self.shell_pane {
            Some(pane) if !pane.has_exited() => return Ok(()),
            // Its stream ended. A shell kept in that state paints its last
            // screen forever and swallows every keystroke, so asking for the
            // shell is asking for a live one — but an ended stream is not an
            // ended shell: a dropped ssh link ends it with the window still
            // running whatever was in it. That window is reattached, not
            // orphaned beside a new one. One that is dead, or that the
            // multiplexer no longer knows (`exit` closed it), is killed as far as
            // it still exists and replaced.
            Some(pane) => {
                let old = pane.backend_id().to_string();
                // Replaced only on the multiplexer's word that it is gone: a
                // host that cannot be asked keeps its pane (the error says why),
                // and a live one is reattached, keeping the pane if that fails —
                // dropping it there would let the next ask spawn a second window
                // beside one that never died.
                if !self.backend.pane_gone(&old)? {
                    let kept = self.shell_pane.take();
                    return match self.adopt_shell_pane(&old, rows, cols) {
                        Ok(()) => Ok(()),
                        Err(e) => {
                            self.shell_pane = kept;
                            Err(e)
                        }
                    };
                }
                let _ = self.backend.kill(&old);
                self.shell_pane = None;
                self.info.shell_backend_id = None;
            }
            None => {}
        }

        let shell_cmd = self.backend.default_shell();
        let window_name = crate::agent::tmux::shell_window_name(&self.info.name);

        let env = self.env.clone();
        let cwd = cwd.or(self.info.cwd.as_deref());

        let spawned = self
            .backend
            .spawn(&window_name, &shell_cmd, &[], cwd, &env, rows, cols)?;

        let (wired, _signals) = Self::wire_up(
            rows,
            cols,
            SessionIo::spawned(spawned),
            &self.backend,
            "Shell",
        );

        stamp(
            &self.backend,
            &wired.backend_id,
            self.info.id,
            WindowRole::Shell,
        );
        self.info.shell_backend_id = Some(wired.backend_id.clone());
        self.shell_pane = Some(ShellPane { wired });

        debug!(session_id = %self.info.id, "Spawned shell pane");
        Ok(())
    }

    /// Re-adopt an existing shell pane from a backend_id (for restore on restart).
    pub fn adopt_shell_pane(&mut self, backend_id: &str, rows: u16, cols: u16) -> Result<()> {
        let adopted = self.backend.adopt(backend_id, rows, cols, None)?;

        let (wired, _signals) = Self::wire_up(
            rows,
            cols,
            SessionIo::adopted(adopted, backend_id),
            &self.backend,
            "Shell",
        );

        self.info.shell_backend_id = Some(wired.backend_id.clone());
        self.shell_pane = Some(ShellPane { wired });

        debug!(session_id = %self.info.id, backend_id = %backend_id, "Adopted shell pane");
        Ok(())
    }

    /// Kill the shell pane if it exists.
    fn kill_shell_pane(&self) {
        if let Some(shell) = &self.shell_pane {
            if let Err(e) = self.backend.kill(&shell.wired.backend_id) {
                tracing::warn!("Failed to kill shell pane: {e}");
            }
        }
    }

    /// Create a lightweight stub for unit tests (no real backend process).
    #[cfg(test)]
    pub fn stub(
        name: &str,
        backend: &Arc<dyn SessionBackend>,
        provider: &Arc<dyn AgentProvider>,
    ) -> Self {
        Self::stub_with_input_rx(name, backend, provider).0
    }

    /// Like [`Self::stub`], but also returns the input-channel receiver so a
    /// test can inspect bytes the app sends to the PTY. The caller must keep
    /// the receiver alive for `send_input` to succeed.
    #[cfg(test)]
    pub fn stub_with_input_rx(
        name: &str,
        backend: &Arc<dyn SessionBackend>,
        provider: &Arc<dyn AgentProvider>,
    ) -> (Self, mpsc::Receiver<Vec<u8>>) {
        let (input_tx, input_rx) = mpsc::channel(INPUT_CHANNEL_CAPACITY);
        // Wire TermSignals to the session's accessor cells exactly like
        // `wire_up`, so bytes injected via `feed_output_for_test` drive
        // `agent_title`/`needs_attention` the same way live PTY output does.
        let signals = SignalCells::new();
        let parser = Arc::new(Mutex::new(vt100::Parser::new_with_callbacks(
            24,
            80,
            0,
            signals.term_signals(),
        )));
        let session = Self {
            info: SessionInfo::new(name.to_string()),
            wired: WiredPane {
                parser,
                input_tx,
                backend_id: String::new(),
                exited: Arc::new(AtomicBool::new(false)),
                last_output_at: Arc::new(AtomicU64::new(now_millis())),
                output_count: Arc::new(AtomicU64::new(0)),
                label: "Session",
                residency: Arc::default(),
                backend: None,
                size: None,
                wanted: AtomicU32::new(0),
            },
            backend: Arc::clone(backend),
            provider: Arc::clone(provider),
            signals,
            last_synced_meta_gen: u64::MAX,
            attention_ack_at: 0,
            shell_pane: None,
            env: HashMap::new(),
            placeholder: false,
        };
        (session, input_rx)
    }

    /// Feed raw agent-output bytes into the session exactly as the reader loop
    /// would: bump `last_output_at` and run the bytes through the vt100 parser
    /// (firing `TermSignals` callbacks). This is the test seam for everything
    /// downstream of PTY output — terminal rendering, the output-change redraw
    /// detector, OSC title/bell signals, and buffer-content search.
    #[cfg(test)]
    pub fn feed_output_for_test(&self, bytes: &[u8]) {
        // Strictly-increasing bump, like a live chunk's: two feeds within the
        // same millisecond must still read as *new* output to the loop.
        let prev = self.wired.last_output_at.load(Ordering::Relaxed);
        self.wired
            .last_output_at
            .store(now_millis().max(prev + 1), Ordering::Relaxed);
        if let Ok(mut p) = self.wired.parser.lock() {
            p.process(bytes);
        }
        self.wired.output_count.fetch_add(1, Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;

    #[test]
    fn input_channel_overflow_fails_fast_without_blocking() {
        let (tx, _rx) = mpsc::channel(INPUT_CHANNEL_CAPACITY);
        for _ in 0..INPUT_CHANNEL_CAPACITY {
            send_to_input_channel(&tx, vec![b'x'], "Session").unwrap();
        }
        let err = send_to_input_channel(&tx, vec![b'x'], "Session").unwrap_err();
        assert!(err.to_string().contains("full"), "got: {err}");
    }

    #[test]
    fn input_channel_closed_reports_closed() {
        let (tx, rx) = mpsc::channel::<Vec<u8>>(INPUT_CHANNEL_CAPACITY);
        drop(rx);
        let err = send_to_input_channel(&tx, vec![b'x'], "Session").unwrap_err();
        assert!(err.to_string().contains("closed"), "got: {err}");
    }

    #[test]
    fn a_grid_at_the_floor_survives_what_an_agent_prints() {
        // The floor is not cosmetic: at one row vt100 underflows in
        // `row_inc_scroll` as soon as output wraps, and at one column it
        // underflows in `col_wrap` as soon as a double-width character arrives.
        // Both used to be reachable — a one-cell pane is what a cramped layout
        // hands a session — and both killed the reader thread, which poisons the
        // parser mutex and blanks that pane for the rest of the run.
        for (rows, cols) in [(0, 0), (1, 1), (1, 2), (2, 1), (1, 40), (3, 1)] {
            let (rows, cols) = vt_floor(rows, cols);
            let mut parser = vt100::Parser::new(rows, cols, 0);
            parser.screen_mut().set_size(rows, cols);
            // A wide character (the `col_wrap` path) and enough plain text to
            // wrap and scroll (the `row_inc_scroll` path).
            parser.process("🚀 done\r\n".repeat(4).as_bytes());
            parser.process("hello world hello world".as_bytes());
        }
    }

    #[test]
    fn the_grid_floor_leaves_a_usable_size_alone() {
        assert_eq!(vt_floor(24, 80), (24, 80));
        assert_eq!(vt_floor(2, 2), (2, 2));
    }

    #[test]
    fn now_millis_returns_reasonable_value() {
        let ms = now_millis();
        // Should be after 2024-01-01 (1704067200000 ms since epoch).
        assert!(ms > 1_704_067_200_000);
    }

    #[test]
    fn utf8_ready_prefix_passes_complete_input() {
        assert_eq!(utf8_ready_prefix_len(b""), 0);
        assert_eq!(utf8_ready_prefix_len(b"hello"), 5);
        // "é" = c3 a9, complete.
        assert_eq!(utf8_ready_prefix_len(&[b'a', 0xc3, 0xa9]), 3);
        // "你好" complete (two 3-byte chars).
        assert_eq!(utf8_ready_prefix_len("你好".as_bytes()), 6);
    }

    #[test]
    fn utf8_ready_prefix_holds_back_truncated_tail() {
        // Lone 2-byte lead → hold all of it.
        assert_eq!(utf8_ready_prefix_len(&[b'a', 0xc3]), 1);
        // 3-byte lead with one continuation, missing one → hold the two.
        assert_eq!(utf8_ready_prefix_len(&[b'x', 0xe4, 0xbd]), 1);
        // 4-byte lead alone, and with 1 and 2 continuations → all held.
        assert_eq!(utf8_ready_prefix_len(&[b'x', 0xf0]), 1);
        assert_eq!(utf8_ready_prefix_len(&[b'x', 0xf0, 0x9f]), 1);
        assert_eq!(utf8_ready_prefix_len(&[b'x', 0xf0, 0x9f, 0x8e]), 1);
        // Same 4-byte char, fully present → nothing held.
        assert_eq!(utf8_ready_prefix_len(&[b'x', 0xf0, 0x9f, 0x8e, 0x89]), 5);
        // The realistic read-boundary case: a complete "é" (c3 a9) followed by
        // the lead byte of the next char → hold only that fresh lead.
        assert_eq!(utf8_ready_prefix_len(&[0xc3, 0xa9, 0xe6]), 2);
    }

    #[test]
    fn utf8_ready_prefix_does_not_buffer_garbage() {
        // Continuation bytes with no lead in the last 3 → pass through (no
        // unbounded carry).
        assert_eq!(utf8_ready_prefix_len(&[0x80, 0x80, 0x80, 0x80]), 4);
    }

    /// Property/regression tests for the reader-loop UTF-8 carry: feeding the
    /// vt100 parser through `utf8_ready_prefix_len`-bounded chunks must render
    /// identically to feeding the whole stream, for any chunking — proving
    /// thurbox's read boundaries can never glitch valid agent output. vt100 on
    /// its own does NOT have this property (it can swallow a newline that
    /// follows a mid-codepoint chunk boundary); the carry is what restores it.
    mod utf8_chunking {
        use proptest::prelude::*;

        use super::utf8_ready_prefix_len;

        /// vt100 screen as normalized, right-trimmed visible rows.
        fn rows(p: &vt100::Parser) -> Vec<String> {
            let s = p.screen();
            let (r, c) = s.size();
            (0..r)
                .map(|y| {
                    let mut t = String::new();
                    for x in 0..c {
                        let sym = s.cell(y, x).map(|cl| cl.contents()).unwrap_or_default();
                        t.push_str(if sym.is_empty() { " " } else { sym });
                    }
                    t.trim_end().to_string()
                })
                .collect()
        }

        fn whole(bytes: &[u8]) -> Vec<String> {
            let mut p = vt100::Parser::new(10, 38, 0);
            p.process(bytes);
            rows(&p)
        }

        /// Replays the reader-loop carry logic over `bytes` cut into `sizes`.
        fn carry_chunked(bytes: &[u8], sizes: &[usize]) -> Vec<String> {
            let mut p = vt100::Parser::new(10, 38, 0);
            let mut carry: Vec<u8> = Vec::new();
            let (mut pos, mut i) = (0usize, 0usize);
            while pos < bytes.len() {
                let sz = sizes.get(i % sizes.len()).copied().unwrap_or(1).max(1);
                let end = (pos + sz).min(bytes.len());
                let mut data = std::mem::take(&mut carry);
                data.extend_from_slice(&bytes[pos..end]);
                let ready = utf8_ready_prefix_len(&data);
                carry = data.split_off(ready);
                assert!(carry.len() <= 3, "carry must stay bounded");
                if !data.is_empty() {
                    p.process(&data);
                }
                pos = end;
                i += 1;
            }
            if !carry.is_empty() {
                p.process(&carry);
            }
            rows(&p)
        }

        /// The exact minimal case that exposed the vt100 mid-codepoint bug:
        /// "f" + lead of "é" delivered in one read, then "é"-tail + "\n日本語"
        /// in the next. Without the carry the newline is swallowed and 日本語
        /// lands on the wrong row.
        #[test]
        fn regression_midcodepoint_newline_widechars() {
            // f é \n 日本語. Chunked [2, 100] so the first read ends on "f" plus
            // the lead byte of "é" and the rest arrives next.
            let bytes = b"f\xc3\xa9\n\xe6\x97\xa5\xe6\x9c\xac\xe8\xaa\x9e";
            assert_eq!(carry_chunked(bytes, &[2, 100]), whole(bytes));
        }

        /// Strategy producing valid-UTF-8 agent output: text, CSI/OSC escapes,
        /// wide/combining chars, newlines.
        fn valid_utf8_output() -> impl Strategy<Value = Vec<u8>> {
            let token = prop_oneof![
                proptest::string::string_regex("[ -~]{0,8}")
                    .unwrap()
                    .prop_map(String::into_bytes),
                (
                    proptest::string::string_regex("[0-9;]{0,6}").unwrap(),
                    prop::sample::select(vec![b'm', b'H', b'J', b'K', b'A', b'B']),
                )
                    .prop_map(|(params, fin)| {
                        let mut v = vec![0x1b, b'['];
                        v.extend(params.bytes());
                        v.push(fin);
                        v
                    }),
                proptest::string::string_regex("[ -~]{0,8}")
                    .unwrap()
                    .prop_map(|s| {
                        let mut v = vec![0x1b, b']'];
                        v.extend(s.bytes());
                        v.push(0x07);
                        v
                    }),
                prop::sample::select(vec!["你好", "🎉", "café", "日本語", "→★", "a\u{0301}"])
                    .prop_map(|s| s.as_bytes().to_vec()),
                prop::sample::select(vec![b'\n', b'\r', b'\t', 0x08]).prop_map(|b| vec![b]),
            ];
            prop::collection::vec(token, 0..40).prop_map(|tokens| tokens.concat())
        }

        proptest! {
            /// For valid UTF-8, the carry makes vt100 rendering independent of how
            /// the byte stream is chunked across reads — the core guarantee that
            /// thurbox's transport/read boundaries never corrupt agent output.
            #[test]
            fn carry_makes_chunking_invariant(
                bytes in valid_utf8_output(),
                sizes in prop::collection::vec(1usize..40, 1..16),
            ) {
                prop_assert_eq!(carry_chunked(&bytes, &sizes), whole(&bytes));
            }
        }
    }

    #[test]
    fn remote_host_from_backend_strips_ssh_and_wsl_prefixes() {
        let host = crate::session::HostDef {
            name: "devbox".into(),
            destination: "me@devbox".into(),
            ..Default::default()
        };
        let ssh: Arc<dyn SessionBackend> =
            Arc::new(crate::agent::tmux::TmuxBackend::from_host(&host));
        assert_eq!(remote_host_from_backend(&ssh).as_deref(), Some("devbox"));

        let wsl: Arc<dyn SessionBackend> = Arc::new(crate::agent::tmux::TmuxBackend::from_host(
            &crate::session::HostDef::wsl("Ubuntu"),
        ));
        assert_eq!(remote_host_from_backend(&wsl).as_deref(), Some("Ubuntu"));

        let local: Arc<dyn SessionBackend> = Arc::new(crate::agent::tmux::TmuxBackend::local());
        assert_eq!(remote_host_from_backend(&local), None);
    }

    #[test]
    fn wire_mode_adopt_starts_stale_spawn_starts_fresh() {
        // Mirrors `App::refresh_session_statuses`: a session is `Busy` while
        // `now - last_output_at <= ACTIVITY_TIMEOUT_MS` (1000 ms in app/mod.rs).
        const ACTIVITY_TIMEOUT_MS: u64 = 1000;

        // Adopt: stale timestamp so the post-adopt SIGWINCH repaint doesn't
        // read as activity → NOT busy.
        let adopt = initial_output_at(WireMode::Adopt);
        assert!(now_millis().saturating_sub(adopt) > ACTIVITY_TIMEOUT_MS);

        // Spawn: "now" so a fresh process counts as active → busy.
        let spawn = initial_output_at(WireMode::Spawn);
        assert!(now_millis().saturating_sub(spawn) <= ACTIVITY_TIMEOUT_MS);
    }

    /// The staleness `wire_mode_adopt_starts_stale_spawn_starts_fresh` sets up
    /// used to be destroyed the instant the reader thread's first read landed,
    /// because that read is the replayed scrollback seed on an adopt — see
    /// [`Session::reader_loop`]'s `seed_len` doc. Replaying only the seed must
    /// leave `last_output_at` untouched.
    #[test]
    fn reader_loop_does_not_stamp_output_for_replayed_scrollback() {
        let parser = Arc::new(Mutex::new(vt100::Parser::new_with_callbacks(
            24,
            80,
            0,
            TermSignals::default(),
        )));
        let exited = Arc::new(AtomicBool::new(false));
        let last_output_at = Arc::new(AtomicU64::new(initial_output_at(WireMode::Adopt)));

        let seed = b"replayed history\r\n$ ".to_vec();
        let seed_len = seed.len();
        Session::reader_loop(
            Box::new(Cursor::new(seed)),
            ReaderState::new(
                Arc::clone(&parser),
                Arc::clone(&exited),
                Arc::clone(&last_output_at),
                Arc::default(),
                Arc::new(AtomicU64::new(0)),
                None,
            ),
            seed_len,
        );

        assert_eq!(
            last_output_at.load(Ordering::Relaxed),
            initial_output_at(WireMode::Adopt),
            "scrollback replay alone must not read as activity"
        );
    }

    /// A pane is released only when another client WAS sizing it and no longer
    /// is — never on a first report, and once per release.
    #[test]
    fn a_pane_is_released_once_when_its_sizer_goes() {
        let size = PaneSize::default();
        size.set_sized_elsewhere(false);
        assert!(!size.take_released(), "nobody was sizing it");
        size.set_sized_elsewhere(true);
        assert!(!size.take_released());
        size.set_sized_elsewhere(false);
        assert!(size.take_released());
        assert!(!size.take_released(), "taken once");
    }

    /// A size the backend reports lands between the bytes written before it and
    /// the bytes written after — the old line is not re-wrapped at the new
    /// width, and the new one is.
    #[test]
    fn reader_loop_resizes_the_grid_where_the_stream_says() {
        struct Script(Vec<Result<&'static [u8], (u16, u16)>>, PaneSize);
        impl Read for Script {
            fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
                if self.0.is_empty() {
                    return Ok(0);
                }
                match self.0.remove(0) {
                    Ok(bytes) => {
                        buf[..bytes.len()].copy_from_slice(bytes);
                        Ok(bytes.len())
                    }
                    Err((rows, cols)) => {
                        self.1.report(rows, cols);
                        Err(std::io::ErrorKind::Interrupted.into())
                    }
                }
            }
        }
        let parser = Arc::new(Mutex::new(vt100::Parser::new_with_callbacks(
            4,
            20,
            0,
            TermSignals::default(),
        )));
        let size = PaneSize::default();
        let script = Script(
            vec![
                Ok(b"aaaaaaaaaaaaaaa\r\n".as_slice()),
                Err((4, 10)),
                Ok(b"bbbbbbbbbbbbbbb".as_slice()),
            ],
            size.clone(),
        );
        Session::reader_loop(
            Box::new(script),
            ReaderState::new(
                Arc::clone(&parser),
                Arc::new(AtomicBool::new(false)),
                Arc::new(AtomicU64::new(0)),
                Arc::default(),
                Arc::new(AtomicU64::new(0)),
                Some(size.clone()),
            ),
            0,
        );

        let parser = parser.lock().unwrap();
        assert_eq!(parser.screen().size(), (4, 10));
        let rows: Vec<String> = parser.screen().rows(0, 10).collect();
        assert_eq!(rows[..3], ["aaaaaaaaaa", "bbbbbbbbbb", "bbbbb"]);
        assert_eq!(size.current(), pack_size(4, 10));
    }

    /// A character split across two reads reaches the parser whole, and the
    /// loop marks the session exited once the stream ends — even when it ends
    /// on half a character, which is flushed rather than kept back forever.
    #[test]
    fn reader_loop_carries_a_split_character_and_marks_the_end() {
        let parser = Arc::new(Mutex::new(vt100::Parser::new_with_callbacks(
            4,
            20,
            0,
            TermSignals::default(),
        )));
        let exited = Arc::new(AtomicBool::new(false));
        let last_output_at = Arc::new(AtomicU64::new(0));

        // "é" is 0xC3 0xA9: the first read ends on its lead byte, and the
        // stream ends on the lead byte of "日".
        let reader = Cursor::new(b"f\xc3".to_vec()).chain(Cursor::new(b"\xa9\r\nok\xe6".to_vec()));
        Session::reader_loop(
            Box::new(reader),
            ReaderState::new(
                Arc::clone(&parser),
                Arc::clone(&exited),
                Arc::clone(&last_output_at),
                Arc::default(),
                Arc::new(AtomicU64::new(0)),
                None,
            ),
            0,
        );

        let screen = parser.lock().unwrap().screen().contents();
        assert!(screen.starts_with("fé\nok"), "{screen:?}");
        assert!(exited.load(Ordering::SeqCst));
        assert!(last_output_at.load(Ordering::Relaxed) > 0);
    }

    /// The count is the loop's cue to repaint, so it must not move before the
    /// parser holds what moved it: a frame painted in between shows the old grid,
    /// and with the cue already spent nothing repaints until the next output —
    /// seen as a shell's `ls` missing from the screen until something else
    /// printed.
    #[test]
    fn reader_loop_marks_output_only_once_the_parser_has_it() {
        let parser = Arc::new(Mutex::new(vt100::Parser::new_with_callbacks(
            4,
            20,
            0,
            TermSignals::default(),
        )));
        let last_output_at = Arc::new(AtomicU64::new(0));
        let output_count = Arc::new(AtomicU64::new(0));

        // Held as a paint holds it, so the reader has to wait to feed it.
        let held = parser.lock().unwrap();
        let reader = {
            let (parser, stamp, count) = (
                Arc::clone(&parser),
                Arc::clone(&last_output_at),
                Arc::clone(&output_count),
            );
            std::thread::spawn(move || {
                Session::reader_loop(
                    Box::new(Cursor::new(b"CHANGELOG".to_vec())),
                    ReaderState::new(
                        parser,
                        Arc::new(AtomicBool::new(false)),
                        stamp,
                        Arc::default(),
                        count,
                        None,
                    ),
                    0,
                );
            })
        };
        std::thread::sleep(std::time::Duration::from_millis(300));
        assert_eq!(
            output_count.load(Ordering::SeqCst),
            0,
            "marked while the parser did not yet hold the bytes"
        );
        assert_eq!(last_output_at.load(Ordering::SeqCst), 0);
        drop(held);
        reader.join().unwrap();
        assert_eq!(output_count.load(Ordering::SeqCst), 1);
        assert!(last_output_at.load(Ordering::SeqCst) > 0);
        assert!(parser
            .lock()
            .unwrap()
            .screen()
            .contents()
            .starts_with("CHANGELOG"));
    }

    /// Two chunks inside one millisecond must still read as two changes, since
    /// the loop repaints on a changed count — while the clock beside it stays a
    /// clock: a flood of chunks must not push it past now, where "quiet for"
    /// would read zero long after the output stopped.
    #[test]
    fn reader_loop_counts_every_chunk_and_keeps_the_clock_honest() {
        struct Chunks {
            left: Vec<&'static [u8]>,
            count: Arc<AtomicU64>,
            seen: Arc<Mutex<Vec<u64>>>,
        }
        impl Read for Chunks {
            fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
                self.seen
                    .lock()
                    .unwrap()
                    .push(self.count.load(Ordering::SeqCst));
                let Some(chunk) = self.left.pop() else {
                    return Ok(0);
                };
                buf[..chunk.len()].copy_from_slice(chunk);
                Ok(chunk.len())
            }
        }
        let parser = Arc::new(Mutex::new(vt100::Parser::new_with_callbacks(
            4,
            20,
            0,
            TermSignals::default(),
        )));
        let last_output_at = Arc::new(AtomicU64::new(0));
        let output_count = Arc::new(AtomicU64::new(0));
        let seen = Arc::new(Mutex::new(Vec::new()));
        let chunks: Vec<&'static [u8]> = vec![b"x"; 500];
        Session::reader_loop(
            Box::new(Chunks {
                left: chunks,
                count: Arc::clone(&output_count),
                seen: Arc::clone(&seen),
            }),
            ReaderState::new(
                parser,
                Arc::new(AtomicBool::new(false)),
                Arc::clone(&last_output_at),
                Arc::default(),
                Arc::clone(&output_count),
                None,
            ),
            0,
        );
        let seen = seen.lock().unwrap().clone();
        assert_eq!(seen[..3], [0, 1, 2], "one step per chunk");
        assert_eq!(output_count.load(Ordering::SeqCst), 500);
        assert!(
            last_output_at.load(Ordering::SeqCst) <= now_millis(),
            "the clock ran ahead of now"
        );
    }

    /// The other half: once the seed is exhausted, genuinely live bytes still
    /// stamp `last_output_at` normally.
    #[test]
    fn reader_loop_stamps_output_for_live_bytes_after_the_seed() {
        let parser = Arc::new(Mutex::new(vt100::Parser::new_with_callbacks(
            24,
            80,
            0,
            TermSignals::default(),
        )));
        let exited = Arc::new(AtomicBool::new(false));
        let last_output_at = Arc::new(AtomicU64::new(initial_output_at(WireMode::Adopt)));

        let seed = b"replayed history\r\n$ ".to_vec();
        let seed_len = seed.len();
        let reader = Cursor::new(seed).chain(Cursor::new(b"fresh agent output\r\n".to_vec()));
        Session::reader_loop(
            Box::new(reader),
            ReaderState::new(
                Arc::clone(&parser),
                Arc::clone(&exited),
                Arc::clone(&last_output_at),
                Arc::default(),
                Arc::new(AtomicU64::new(0)),
                None,
            ),
            seed_len,
        );

        let stamped = last_output_at.load(Ordering::Relaxed);
        assert!(
            now_millis().saturating_sub(stamped) < 5_000,
            "live output past the seed boundary must stamp fresh activity"
        );
    }

    /// The fold-level consequence of the two tests above: a session that has
    /// genuinely been `blocked` for hours must still read `blocked` after a
    /// restart/reattach replays its scrollback with no new live output —
    /// composing the reader-loop fix with `with_output_quiescence` the way
    /// `Terminals::sync_printing`'s consumer (`apply_output_quiescence`) does.
    /// Before the fix, the seed replay stamped `last_output_at` to "now",
    /// making the pane look quiet for ~0ms against an hours-old `hook_state_at`
    /// and folding `Blocked` into `Working`/`Idle`.
    #[test]
    fn adopted_scrollback_replay_does_not_outlive_a_standing_blocked_state() {
        use crate::session::{with_output_quiescence, SessionState};

        let parser = Arc::new(Mutex::new(vt100::Parser::new_with_callbacks(
            24,
            80,
            0,
            TermSignals::default(),
        )));
        let exited = Arc::new(AtomicBool::new(false));
        let last_output_at = Arc::new(AtomicU64::new(initial_output_at(WireMode::Adopt)));

        let seed = b"...\r\nPermission needed to run `rm -rf build`\r\n".to_vec();
        let seed_len = seed.len();
        Session::reader_loop(
            Box::new(Cursor::new(seed)),
            ReaderState::new(
                parser,
                exited,
                Arc::clone(&last_output_at),
                Arc::default(),
                Arc::new(AtomicU64::new(0)),
                None,
            ),
            seed_len,
        );

        let quiet_for_ms = now_millis().saturating_sub(last_output_at.load(Ordering::Relaxed));
        let state_age_ms = 3 * 60 * 60 * 1000; // hook_state_at stamped 3 hours ago

        assert_eq!(
            with_output_quiescence(
                SessionState::Blocked,
                Some(quiet_for_ms),
                Some(state_age_ms)
            ),
            SessionState::Blocked,
            "a restart replaying scrollback must not make a genuinely blocked session look done"
        );
    }

    #[test]
    fn title_capture_extracts_osc_title() {
        let title = Arc::new(Mutex::new(None));
        let mut parser = vt100::Parser::new_with_callbacks(
            24,
            80,
            0,
            TermSignals {
                title: Arc::clone(&title),
                ..Default::default()
            },
        );
        // OSC 2 (set window title), BEL-terminated.
        parser.process(b"\x1b]2;working on tests\x07");
        assert_eq!(title.lock().unwrap().as_deref(), Some("working on tests"));

        // OSC 0 (set icon name + title) updates it too.
        parser.process(b"\x1b]0;done\x07");
        assert_eq!(title.lock().unwrap().as_deref(), Some("done"));

        // ST-terminated, which is what the adopt-time replay of a pane title
        // emits (`title_seed_bytes`) — the restored activity line depends on
        // this arriving through the same callback a live title does.
        parser.process(b"\x1b]2;restored from the pane title\x1b\\");
        assert_eq!(
            title.lock().unwrap().as_deref(),
            Some("restored from the pane title")
        );

        // An empty title clears the cell rather than storing "".
        parser.process(b"\x1b]2;\x07");
        assert_eq!(*title.lock().unwrap(), None);
    }

    #[test]
    fn attention_signals_are_captured() {
        let attention_at = Arc::new(AtomicU64::new(0));
        let notification = Arc::new(Mutex::new(None));
        let mut parser = vt100::Parser::new_with_callbacks(
            24,
            80,
            0,
            TermSignals {
                attention_at: Arc::clone(&attention_at),
                notification: Arc::clone(&notification),
                ..Default::default()
            },
        );

        assert_eq!(attention_at.load(Ordering::Relaxed), 0);

        // Terminal bell → attention, no message.
        parser.process(b"\x07");
        assert!(attention_at.load(Ordering::Relaxed) > 0);
        assert_eq!(*notification.lock().unwrap(), None);

        // OSC 9 desktop notification → attention + message text.
        parser.process(b"\x1b]9;Claude is waiting for your input\x07");
        assert_eq!(
            notification.lock().unwrap().as_deref(),
            Some("Claude is waiting for your input")
        );

        // OSC 777 notify form → attention + joined title/body.
        parser.process(b"\x1b]777;notify;Claude;Task done\x07");
        assert_eq!(
            notification.lock().unwrap().as_deref(),
            Some("Claude: Task done")
        );
    }
}
