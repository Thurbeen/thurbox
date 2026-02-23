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

/// Internal env key used to pass the target VM ID through `SessionBackend::spawn()`.
///
/// Injected by `Session::spawn/restart/ensure_shell_pane` when `SessionConfig.vm_id` is set;
/// consumed by `QemuVmBackend::spawn()` to route the session to the correct VM.
pub(crate) const VM_ID_ENV_KEY: &str = "__THURBOX_VM_ID";

pub(crate) fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

/// Metadata returned when discovering existing sessions from the backend.
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
    /// Captured screen content for parser seeding (output produced before streaming started).
    pub initial_screen: Vec<u8>,
}

/// A reconnected session from the backend.
pub struct AdoptedSession {
    /// Streaming output bytes from the session.
    pub output: Box<dyn Read + Send>,
    /// Input write handle to send bytes to the session.
    pub input: Box<dyn Write + Send>,
    /// Captured screen content for parser seeding.
    pub initial_screen: Vec<u8>,
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

    /// Prepare a VM for session spawning (e.g., establish SSH control mode).
    ///
    /// No-op for non-VM backends. VM backends use this to set up the control
    /// mode connection after provisioning completes.
    fn prepare_vm(&self, _vm_id: &str) -> Result<()> {
        Ok(())
    }

    /// Default shell command for companion shell panes.
    ///
    /// Local backends use `$SHELL`; VM backends return the VM's default shell.
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
    initial_screen: Vec<u8>,
    backend_id: String,
}

/// Wired-up I/O state: parser, channels, and exit tracking.
struct WiredState {
    parser: Arc<Mutex<vt100::Parser>>,
    input_tx: mpsc::UnboundedSender<Vec<u8>>,
    exited: Arc<AtomicBool>,
    last_output_at: Arc<AtomicU64>,
}

/// A companion shell pane running alongside an agent session.
pub struct ShellPane {
    pub parser: Arc<Mutex<vt100::Parser>>,
    input_tx: mpsc::UnboundedSender<Vec<u8>>,
    backend_id: String,
    /// Kept alive so the reader loop's Arc clone has a peer.
    #[allow(dead_code)]
    exited: Arc<AtomicBool>,
    #[allow(dead_code)]
    last_output_at: Arc<AtomicU64>,
}

impl ShellPane {
    pub fn send_input(&self, data: Vec<u8>) -> Result<()> {
        self.input_tx
            .send(data)
            .map_err(|_| anyhow::anyhow!("Shell input channel closed"))
    }

    /// Build a ShellPane from wired-up I/O state.
    fn from_wired(state: WiredState, backend_id: String) -> Self {
        Self {
            parser: state.parser,
            input_tx: state.input_tx,
            backend_id,
            exited: state.exited,
            last_output_at: state.last_output_at,
        }
    }
}

/// A running session connected to a backend.
pub struct Session {
    pub info: SessionInfo,
    pub parser: Arc<Mutex<vt100::Parser>>,
    input_tx: mpsc::UnboundedSender<Vec<u8>>,
    backend_id: String,
    backend: Arc<dyn SessionBackend>,
    provider: Arc<dyn AgentProvider>,
    exited: Arc<AtomicBool>,
    last_output_at: Arc<AtomicU64>,
    pub shell_pane: Option<ShellPane>,
    /// Environment variables from the role, passed to shell pane spawns.
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
        let window_name = format!("tb-{name}");

        // Build env map, injecting VM_ID_ENV_KEY if a VM target is specified.
        let mut env = config.permissions.env.clone();
        if let Some(ref vm_id) = config.vm_id {
            env.insert(VM_ID_ENV_KEY.to_string(), vm_id.clone());
        }

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
        info.additional_dirs = config.additional_dirs.clone();
        if !config.role.is_empty() {
            info.role = config.role.clone();
        }
        info.backend_id = Some(spawned.backend_id.clone());
        debug!(session_id = %info.id, backend_id = %spawned.backend_id, "Spawned session via backend");

        Ok(Self::wire_io(
            info,
            rows,
            cols,
            SessionIo {
                output: spawned.output,
                input: spawned.input,
                initial_screen: spawned.initial_screen,
                backend_id: spawned.backend_id,
            },
            backend,
            provider,
            config.permissions.env.clone(),
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
            initial_screen_bytes = adopted.initial_screen.len(),
            parser_rows = rows,
            parser_cols = cols,
            "Adopting session with initial screen"
        );

        let mut info = SessionInfo::new(name);
        info.backend_id = Some(backend_id.to_string());
        debug!(session_id = %info.id, backend_id = %backend_id, "Adopted session via backend");

        Ok(Self::wire_io(
            info,
            rows,
            cols,
            SessionIo {
                output: adopted.output,
                input: adopted.input,
                initial_screen: adopted.initial_screen,
                backend_id: backend_id.to_string(),
            },
            backend,
            provider,
            env,
        ))
    }

    /// Create parser, spawn reader/writer loops for the given I/O handles.
    fn wire_up(rows: u16, cols: u16, io: SessionIo) -> (WiredState, String) {
        let parser = Arc::new(Mutex::new(vt100::Parser::new(rows, cols, 1000)));

        if !io.initial_screen.is_empty() {
            if let Ok(mut p) = parser.lock() {
                p.process(&io.initial_screen);
            }
        }

        let exited = Arc::new(AtomicBool::new(false));
        let last_output_at = Arc::new(AtomicU64::new(now_millis()));

        let (input_tx, input_rx) = mpsc::unbounded_channel();
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
            shell_pane: None,
            env,
        }
    }

    fn reader_loop(
        mut reader: Box<dyn Read + Send>,
        parser: Arc<Mutex<vt100::Parser>>,
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
        mut input_rx: mpsc::UnboundedReceiver<Vec<u8>>,
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
        self.input_tx
            .send(data)
            .map_err(|_| anyhow::anyhow!("Session input channel closed"))
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

    /// Restart the session: kill the old pane, spawn a fresh one with new config.
    ///
    /// Uses `--resume` so the agent picks up the conversation while getting
    /// freshly-resolved role permissions.
    pub fn restart(&mut self, config: &SessionConfig, rows: u16, cols: u16) -> Result<()> {
        self.backend.kill(&self.backend_id)?;

        let args = self.provider.build_args(config);
        let window_name = format!("tb-{}", self.info.name);

        // Inject __THURBOX_VM_ID for VM-backed sessions (same as Session::spawn).
        let mut env = config.permissions.env.clone();
        if let Some(ref vm_id) = config.vm_id {
            env.insert(VM_ID_ENV_KEY.to_string(), vm_id.clone());
        }

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
                initial_screen: spawned.initial_screen,
                backend_id: spawned.backend_id,
            },
        );

        self.backend_id = backend_id;
        self.parser = state.parser;
        self.input_tx = state.input_tx;
        self.exited = state.exited;
        self.last_output_at = state.last_output_at;
        self.env = config.permissions.env.clone();
        self.info.backend_id = Some(self.backend_id.clone());
        if !config.role.is_empty() {
            self.info.role = config.role.clone();
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

    /// Lazily spawn a companion shell pane in the same cwd.
    ///
    /// Uses `$SHELL` (fallback `/bin/sh`) as the command.
    /// The window name uses `tbs-` prefix to distinguish from the agent's `tb-` windows.
    pub fn ensure_shell_pane(&mut self, rows: u16, cols: u16) -> Result<()> {
        if self.shell_pane.is_some() {
            return Ok(());
        }

        let shell_cmd = self.backend.default_shell();
        let window_name = format!("tbs-{}", self.info.name);

        // Inject __THURBOX_VM_ID for VM-backed sessions so the backend
        // knows which VM to create the shell pane in.
        let mut env = self.env.clone();
        if let Some(ref vm_id) = self.info.vm_id {
            env.insert(VM_ID_ENV_KEY.to_string(), vm_id.clone());
        }

        let spawned = self.backend.spawn(
            &window_name,
            &shell_cmd,
            &[],
            self.info.cwd.as_deref(),
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
                initial_screen: spawned.initial_screen,
                backend_id: spawned.backend_id,
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
                initial_screen: adopted.initial_screen,
                backend_id: backend_id.to_string(),
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
        let (input_tx, _input_rx) = mpsc::unbounded_channel();
        Self {
            info: SessionInfo::new(name.to_string()),
            parser: Arc::new(Mutex::new(vt100::Parser::new(24, 80, 0))),
            input_tx,
            backend_id: String::new(),
            backend: Arc::clone(backend),
            provider: Arc::clone(provider),
            exited: Arc::new(AtomicBool::new(false)),
            last_output_at: Arc::new(AtomicU64::new(now_millis())),
            shell_pane: None,
            env: HashMap::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn now_millis_returns_reasonable_value() {
        let ms = now_millis();
        // Should be after 2024-01-01 (1704067200000 ms since epoch).
        assert!(ms > 1_704_067_200_000);
    }

    #[test]
    fn vm_id_env_key_is_internal() {
        // The key should start with __ to signal it's an internal implementation detail.
        assert!(VM_ID_ENV_KEY.starts_with("__"));
    }
}
