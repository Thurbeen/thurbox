use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::Path;
use std::sync::{
    atomic::{AtomicBool, AtomicU64, Ordering},
    Arc, Mutex,
};
use std::time::SystemTime;

use anyhow::Result;
use tokio::sync::mpsc;
use tracing::{debug, error};

use crate::agent::provider::AgentProvider;
use crate::session::{SessionConfig, SessionInfo};

pub(crate) fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
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
#[derive(Clone, Default)]
pub struct TermSignals {
    title: Arc<Mutex<Option<String>>>,
    /// `now_millis()` of the most recent attention signal; `0` = none yet.
    attention_at: Arc<AtomicU64>,
    /// Message text from the most recent OSC 9/777 notification, if any.
    notification: Arc<Mutex<Option<String>>>,
}

impl TermSignals {
    fn store_title(&self, raw: &[u8]) {
        let s = String::from_utf8_lossy(raw).trim().to_string();
        if let Ok(mut guard) = self.title.lock() {
            *guard = (!s.is_empty()).then_some(s);
        }
    }

    /// Mark an attention signal, optionally with notification message text.
    fn signal_attention(&self, message: Option<String>) {
        self.attention_at.store(now_millis(), Ordering::Relaxed);
        if let Some(msg) = message {
            let msg = msg.trim().to_string();
            if let Ok(mut guard) = self.notification.lock() {
                *guard = (!msg.is_empty()).then_some(msg);
            }
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

    fn unhandled_osc(&mut self, _: &mut vt100::Screen, params: &[&[u8]]) {
        // Desktop-notification escapes carry the agent's status message.
        //   OSC 9 ; <message>
        //   OSC 777 ; notify ; <title> ; <body>
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
            _ => {}
        }
    }
}

/// Session terminal parser, specialized to capture terminal signals via
/// [`TermSignals`]. The captured `Screen` is callback-independent, so
/// rendering is unaffected.
pub type SessionParser = vt100::Parser<TermSignals>;

/// Metadata returned when discovering existing sessions from the backend.
#[derive(Clone)]
pub struct DiscoveredSession {
    /// Backend-specific ID (e.g., tmux pane_id).
    pub backend_id: String,
    /// Window name or label.
    pub name: String,
    /// Whether the process is still running.
    pub is_alive: bool,
}

/// A newly spawned session from the backend.
pub struct SpawnedSession {
    /// Backend-specific session identifier.
    pub backend_id: String,
    /// Streaming output bytes from the session.
    pub output: Box<dyn Read + Send>,
    /// Input write handle to send bytes to the session.
    pub input: Box<dyn Write + Send>,
}

/// A reconnected session from the backend.
pub struct AdoptedSession {
    /// Streaming output bytes from the session.
    pub output: Box<dyn Read + Send>,
    /// Input write handle to send bytes to the session.
    pub input: Box<dyn Write + Send>,
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

    /// Reconnect to an existing session.
    fn adopt(&self, backend_id: &str, rows: u16, cols: u16) -> Result<AdoptedSession>;

    /// Discover existing sessions managed by this backend.
    fn discover(&self) -> Result<Vec<DiscoveredSession>>;

    /// Resize a session's terminal.
    fn resize(&self, backend_id: &str, rows: u16, cols: u16) -> Result<()>;

    /// Check if a session's process has exited.
    fn is_dead(&self, backend_id: &str) -> Result<bool>;

    /// Kill/destroy a session (for Ctrl+X close).
    fn kill(&self, backend_id: &str) -> Result<()>;

    /// Detach from a session without killing it (for Ctrl+Q quit).
    fn detach(&self, backend_id: &str) -> Result<()>;

    /// Default shell command for companion shell panes.
    fn default_shell(&self) -> String {
        std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string())
    }

    /// Return the PID of the process running in a backend pane.
    fn pane_pid(&self, backend_id: &str) -> Result<Option<u32>>;
}

/// Internal bundle of I/O handles before wiring.
struct SessionIo {
    output: Box<dyn Read + Send>,
    input: Box<dyn Write + Send>,
    backend_id: String,
    /// Whether these handles came from a fresh spawn or an adopt.
    mode: WireMode,
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

/// Wired-up I/O state: parser, channels, and exit tracking.
struct WiredState {
    parser: Arc<Mutex<SessionParser>>,
    input_tx: mpsc::Sender<Vec<u8>>,
    exited: Arc<AtomicBool>,
    last_output_at: Arc<AtomicU64>,
    last_title: Arc<Mutex<Option<String>>>,
    attention_at: Arc<AtomicU64>,
    notification: Arc<Mutex<Option<String>>>,
}

/// A companion shell pane running alongside an agent session.
pub struct ShellPane {
    pub parser: Arc<Mutex<SessionParser>>,
    input_tx: mpsc::Sender<Vec<u8>>,
    backend_id: String,
    /// Kept alive so the reader loop's Arc clone has a peer.
    #[allow(dead_code)]
    exited: Arc<AtomicBool>,
    #[allow(dead_code)]
    last_output_at: Arc<AtomicU64>,
    /// Captured OSC title for the shell pane (unused; kept for symmetry).
    #[allow(dead_code)]
    last_title: Arc<Mutex<Option<String>>>,
}

impl ShellPane {
    pub fn send_input(&self, data: Vec<u8>) -> Result<()> {
        send_to_input_channel(&self.input_tx, data, "Shell")
    }

    /// Build a ShellPane from wired-up I/O state.
    fn from_wired(state: WiredState, backend_id: String) -> Self {
        Self {
            parser: state.parser,
            input_tx: state.input_tx,
            backend_id,
            exited: state.exited,
            last_output_at: state.last_output_at,
            last_title: state.last_title,
        }
    }
}

/// The bare remote host name for a session's backend (e.g. `devbox` for an
/// `ssh:devbox` backend), or `None` for local backends. Drives the session
/// list's remote indicator.
fn remote_host_from_backend(backend: &Arc<dyn SessionBackend>) -> Option<String> {
    backend
        .name()
        .strip_prefix(crate::session::SSH_BACKEND_PREFIX)
        .map(str::to_string)
}

/// A running session connected to a backend.
pub struct Session {
    pub info: SessionInfo,
    pub parser: Arc<Mutex<SessionParser>>,
    input_tx: mpsc::Sender<Vec<u8>>,
    backend_id: String,
    backend: Arc<dyn SessionBackend>,
    provider: Arc<dyn AgentProvider>,
    exited: Arc<AtomicBool>,
    last_output_at: Arc<AtomicU64>,
    /// Latest OSC window title the agent emitted (live activity text).
    last_title: Arc<Mutex<Option<String>>>,
    /// `now_millis()` of the latest attention signal (bell / OSC 9 / OSC 777).
    attention_at: Arc<AtomicU64>,
    /// Message text from the latest OSC 9/777 notification, if any.
    notification: Arc<Mutex<Option<String>>>,
    /// `now_millis()` of the last attention acknowledgement (set while the
    /// session is the active one). Attention is pending when
    /// `attention_at > attention_ack_at`.
    attention_ack_at: u64,
    pub shell_pane: Option<ShellPane>,
    /// Session environment variables, passed to shell pane spawns.
    env: HashMap<String, String>,
}

impl Session {
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
        info.agent_session_id = config.agent_session_id.clone();
        info.cwd = config.cwd.clone();
        if !config.agent.is_empty() {
            info.agent = config.agent.clone();
        }
        info.backend_id = Some(spawned.backend_id.clone());
        info.remote_host = remote_host_from_backend(backend);
        debug!(session_id = %info.id, backend_id = %spawned.backend_id, "Spawned session via backend");

        Ok(Self::wire_io(
            info,
            rows,
            cols,
            SessionIo {
                output: spawned.output,
                input: spawned.input,
                backend_id: spawned.backend_id,
                mode: WireMode::Spawn,
            },
            backend,
            provider,
            env,
        ))
    }

    /// Reconnect to an existing backend session.
    pub fn adopt(
        name: String,
        rows: u16,
        cols: u16,
        backend_id: &str,
        backend: &Arc<dyn SessionBackend>,
        provider: &Arc<dyn AgentProvider>,
        env: HashMap<String, String>,
    ) -> Result<Self> {
        let adopted = backend.adopt(backend_id, rows, cols)?;

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
            SessionIo {
                output: adopted.output,
                input: adopted.input,
                backend_id: backend_id.to_string(),
                mode: WireMode::Adopt,
            },
            backend,
            provider,
            env,
        ))
    }

    /// Create parser, spawn reader/writer loops for the given I/O handles.
    fn wire_up(rows: u16, cols: u16, io: SessionIo) -> (WiredState, String) {
        let last_title = Arc::new(Mutex::new(None));
        let attention_at = Arc::new(AtomicU64::new(0));
        let notification = Arc::new(Mutex::new(None));
        let parser = Arc::new(Mutex::new(vt100::Parser::new_with_callbacks(
            rows,
            cols,
            1000,
            TermSignals {
                title: Arc::clone(&last_title),
                attention_at: Arc::clone(&attention_at),
                notification: Arc::clone(&notification),
            },
        )));

        let exited = Arc::new(AtomicBool::new(false));
        let last_output_at = Arc::new(AtomicU64::new(initial_output_at(io.mode)));

        let (input_tx, input_rx) = mpsc::channel(INPUT_CHANNEL_CAPACITY);
        tokio::spawn(Self::writer_loop(io.input, input_rx));

        let parser_clone = Arc::clone(&parser);
        let exited_clone = Arc::clone(&exited);
        let last_output_clone = Arc::clone(&last_output_at);
        tokio::task::spawn_blocking(move || {
            Self::reader_loop(io.output, parser_clone, exited_clone, last_output_clone);
        });

        let state = WiredState {
            parser,
            input_tx,
            exited,
            last_output_at,
            last_title,
            attention_at,
            notification,
        };
        (state, io.backend_id)
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
        let (state, backend_id) = Self::wire_up(rows, cols, io);
        Self {
            info,
            parser: state.parser,
            input_tx: state.input_tx,
            backend_id,
            backend: Arc::clone(backend),
            provider: Arc::clone(provider),
            exited: state.exited,
            last_output_at: state.last_output_at,
            last_title: state.last_title,
            attention_at: state.attention_at,
            notification: state.notification,
            attention_ack_at: 0,
            shell_pane: None,
            env,
        }
    }

    fn reader_loop(
        mut reader: Box<dyn Read + Send>,
        parser: Arc<Mutex<SessionParser>>,
        exited: Arc<AtomicBool>,
        last_output_at: Arc<AtomicU64>,
    ) {
        let mut buf = [0u8; 4096];
        loop {
            match reader.read(&mut buf) {
                Ok(0) => {
                    debug!("Session reader: EOF");
                    break;
                }
                Ok(n) => {
                    let data = &buf[..n];
                    last_output_at.store(now_millis(), Ordering::Relaxed);
                    if let Ok(mut p) = parser.lock() {
                        p.process(data);
                    }
                }
                Err(e) => {
                    debug!("Session reader error: {e}");
                    break;
                }
            }
        }
        exited.store(true, Ordering::SeqCst);
    }

    async fn writer_loop(
        mut writer: Box<dyn Write + Send>,
        mut input_rx: mpsc::Receiver<Vec<u8>>,
    ) {
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

    pub fn send_input(&self, data: Vec<u8>) -> Result<()> {
        send_to_input_channel(&self.input_tx, data, "Session")
    }

    pub fn resize(&self, rows: u16, cols: u16) {
        if let Err(e) = self.backend.resize(&self.backend_id, rows, cols) {
            tracing::warn!("Failed to resize session: {e}");
            return;
        }
        if let Ok(mut parser) = self.parser.lock() {
            parser.screen_mut().set_size(rows, cols);
        }
        if let Some(shell) = &self.shell_pane {
            if let Err(e) = self.backend.resize(&shell.backend_id, rows, cols) {
                tracing::warn!("Failed to resize shell pane: {e}");
                return;
            }
            if let Ok(mut parser) = shell.parser.lock() {
                parser.screen_mut().set_size(rows, cols);
            }
        }
    }

    pub fn has_exited(&self) -> bool {
        self.exited.load(Ordering::SeqCst)
    }

    pub fn millis_since_last_output(&self) -> u64 {
        now_millis().saturating_sub(self.last_output_at.load(Ordering::Relaxed))
    }

    /// Latest OSC window title the agent emitted, if any (live activity text).
    pub fn agent_title(&self) -> Option<String> {
        self.last_title.lock().ok().and_then(|t| t.clone())
    }

    /// Whether the agent has signalled for attention (bell / OSC 9 / OSC 777)
    /// since it was last acknowledged. Cleared via [`Self::acknowledge_attention`].
    pub fn needs_attention(&self) -> bool {
        self.attention_at.load(Ordering::Relaxed) > self.attention_ack_at
    }

    /// Message text from the latest attention notification, if any.
    pub fn notification(&self) -> Option<String> {
        self.notification.lock().ok().and_then(|n| n.clone())
    }

    /// Acknowledge any pending attention signal (called while the session is
    /// the active/selected one — the user is already looking at it).
    pub fn acknowledge_attention(&mut self) {
        self.attention_ack_at = now_millis();
    }

    /// Return the backend-specific session identifier.
    pub fn backend_id(&self) -> &str {
        &self.backend_id
    }

    /// Return the backend name.
    pub fn backend_name(&self) -> &str {
        self.backend.name()
    }

    /// Return the PID of the process running in this session's backend pane.
    pub fn pane_pid(&self) -> Result<Option<u32>> {
        self.backend.pane_pid(&self.backend_id)
    }

    /// Clone the backend handle + id so a background task can query the pane
    /// PID (a control-mode round-trip, slow for remote SSH backends) off the UI
    /// thread. The backend is `Send + Sync`, so the clone is cheap to move.
    pub fn backend_handle(&self) -> (Arc<dyn SessionBackend>, String) {
        (Arc::clone(&self.backend), self.backend_id.clone())
    }

    /// Restart the session: kill the old pane, spawn a fresh one with new config.
    ///
    /// Uses the agent's resume args (when defined) so it picks up the
    /// existing conversation instead of starting fresh.
    pub fn restart(&mut self, config: &SessionConfig, rows: u16, cols: u16) -> Result<()> {
        self.backend.kill(&self.backend_id)?;

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

        let (state, backend_id) = Self::wire_up(
            rows,
            cols,
            SessionIo {
                output: spawned.output,
                input: spawned.input,
                backend_id: spawned.backend_id,
                mode: WireMode::Spawn,
            },
        );

        self.backend_id = backend_id;
        self.parser = state.parser;
        self.input_tx = state.input_tx;
        self.exited = state.exited;
        self.last_output_at = state.last_output_at;
        self.env = config.env.clone();
        self.info.backend_id = Some(self.backend_id.clone());
        if !config.agent.is_empty() {
            self.info.agent = config.agent.clone();
        }

        debug!(session_id = %self.info.id, backend_id = %self.backend_id, "Restarted session");
        Ok(())
    }

    /// Kill/destroy the backend session (for Ctrl+X close).
    pub fn kill(&self) {
        self.kill_shell_pane();
        if let Err(e) = self.backend.kill(&self.backend_id) {
            tracing::warn!("Failed to kill session: {e}");
        }
    }

    /// Detach from the backend session without killing it (for Ctrl+Q quit).
    pub fn detach(self) {
        if let Some(shell) = &self.shell_pane {
            if let Err(e) = self.backend.detach(&shell.backend_id) {
                tracing::warn!("Failed to detach shell pane: {e}");
            }
        }
        if let Err(e) = self.backend.detach(&self.backend_id) {
            tracing::warn!("Failed to detach session: {e}");
        }
        drop(self.input_tx);
        debug!("Session detached");
    }

    /// Lazily spawn a companion shell pane.
    ///
    /// `cwd` is the directory the shell starts in — the caller passes the
    /// session's *launch* cwd (the multi-repo symlink workspace when there is
    /// one, so the shell lands where the agent does), falling back to the
    /// primary repo (`info.cwd`) when `None`. Uses `$SHELL` (fallback `/bin/sh`)
    /// as the command. The window name uses the `tbs-` prefix to distinguish
    /// from the agent's `tb-` windows.
    pub fn ensure_shell_pane(
        &mut self,
        rows: u16,
        cols: u16,
        cwd: Option<&std::path::Path>,
    ) -> Result<()> {
        if self.shell_pane.is_some() {
            return Ok(());
        }

        let shell_cmd = self.backend.default_shell();
        let window_name = crate::agent::tmux::shell_window_name(&self.info.name);

        let env = self.env.clone();
        let cwd = cwd.or(self.info.cwd.as_deref());

        let spawned = self
            .backend
            .spawn(&window_name, &shell_cmd, &[], cwd, &env, rows, cols)?;

        let (state, backend_id) = Self::wire_up(
            rows,
            cols,
            SessionIo {
                output: spawned.output,
                input: spawned.input,
                backend_id: spawned.backend_id,
                mode: WireMode::Spawn,
            },
        );

        self.info.shell_backend_id = Some(backend_id.clone());
        self.shell_pane = Some(ShellPane::from_wired(state, backend_id));

        debug!(session_id = %self.info.id, "Spawned shell pane");
        Ok(())
    }

    /// Re-adopt an existing shell pane from a backend_id (for restore on restart).
    pub fn adopt_shell_pane(&mut self, backend_id: &str, rows: u16, cols: u16) -> Result<()> {
        let adopted = self.backend.adopt(backend_id, rows, cols)?;

        let (state, bid) = Self::wire_up(
            rows,
            cols,
            SessionIo {
                output: adopted.output,
                input: adopted.input,
                backend_id: backend_id.to_string(),
                mode: WireMode::Adopt,
            },
        );

        self.info.shell_backend_id = Some(bid.clone());
        self.shell_pane = Some(ShellPane::from_wired(state, bid));

        debug!(session_id = %self.info.id, backend_id = %backend_id, "Adopted shell pane");
        Ok(())
    }

    /// Kill the shell pane if it exists.
    fn kill_shell_pane(&self) {
        if let Some(shell) = &self.shell_pane {
            if let Err(e) = self.backend.kill(&shell.backend_id) {
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
        let (input_tx, _input_rx) = mpsc::channel(INPUT_CHANNEL_CAPACITY);
        Self {
            info: SessionInfo::new(name.to_string()),
            parser: Arc::new(Mutex::new(vt100::Parser::new_with_callbacks(
                24,
                80,
                0,
                TermSignals::default(),
            ))),
            input_tx,
            backend_id: String::new(),
            backend: Arc::clone(backend),
            provider: Arc::clone(provider),
            exited: Arc::new(AtomicBool::new(false)),
            last_output_at: Arc::new(AtomicU64::new(now_millis())),
            last_title: Arc::new(Mutex::new(None)),
            attention_at: Arc::new(AtomicU64::new(0)),
            notification: Arc::new(Mutex::new(None)),
            attention_ack_at: 0,
            shell_pane: None,
            env: HashMap::new(),
        }
    }
}

#[cfg(test)]
mod tests {
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
    fn now_millis_returns_reasonable_value() {
        let ms = now_millis();
        // Should be after 2024-01-01 (1704067200000 ms since epoch).
        assert!(ms > 1_704_067_200_000);
    }

    #[test]
    fn remote_host_from_backend_strips_ssh_prefix() {
        let host = crate::session::HostDef {
            name: "devbox".into(),
            destination: "me@devbox".into(),
            socket: None,
            session: None,
            ssh_opts: vec![],
            worktrees_dir: None,
        };
        let ssh: Arc<dyn SessionBackend> =
            Arc::new(crate::agent::tmux::TmuxBackend::from_host(&host));
        assert_eq!(remote_host_from_backend(&ssh).as_deref(), Some("devbox"));

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

        // No signal yet.
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
