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

use crate::session::{SessionConfig, SessionInfo};

/// Default permission mode passed to the Claude CLI when no explicit mode is configured.
const DEFAULT_PERMISSION_MODE: &str = "default";

pub(crate) fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

/// Build the CLI argument list from a SessionConfig.
///
/// This is extracted as a pure function for testability.
pub fn build_claude_args(config: &SessionConfig) -> Vec<String> {
    let mut args = Vec::new();

    if let Some(ref session_id) = config.resume_session_id {
        args.push("--resume".to_string());
        args.push(session_id.clone());
    } else if let Some(ref session_id) = config.claude_session_id {
        args.push("--session-id".to_string());
        args.push(session_id.clone());
    }

    // Role permission flags — default to "default" when no mode is configured.
    let mode = config
        .permissions
        .permission_mode
        .as_deref()
        .unwrap_or(DEFAULT_PERMISSION_MODE);
    args.push("--permission-mode".to_string());
    args.push(mode.to_string());
    if !config.permissions.allowed_tools.is_empty() {
        args.push("--allowed-tools".to_string());
        args.push(config.permissions.allowed_tools.join(" "));
    }
    if !config.permissions.disallowed_tools.is_empty() {
        args.push("--disallowed-tools".to_string());
        args.push(config.permissions.disallowed_tools.join(" "));
    }
    if let Some(ref tools) = config.permissions.tools {
        args.push("--tools".to_string());
        args.push(tools.clone());
    }
    if let Some(ref prompt) = config.permissions.append_system_prompt {
        args.push("--append-system-prompt".to_string());
        args.push(prompt.clone());
    }

    for dir in &config.additional_dirs {
        args.push("--add-dir".to_string());
        args.push(dir.display().to_string());
    }

    args
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
    fn spawn(
        &self,
        window_name: &str,
        command: &str,
        args: &[String],
        cwd: Option<&Path>,
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

/// A running session connected to a backend.
pub struct Session {
    pub info: SessionInfo,
    pub parser: Arc<Mutex<vt100::Parser>>,
    input_tx: mpsc::UnboundedSender<Vec<u8>>,
    backend_id: String,
    backend: Arc<dyn SessionBackend>,
    exited: Arc<AtomicBool>,
    last_output_at: Arc<AtomicU64>,
}

impl Session {
    /// Spawn a new session via the given backend.
    pub fn spawn(
        name: String,
        rows: u16,
        cols: u16,
        config: &SessionConfig,
        backend: &Arc<dyn SessionBackend>,
    ) -> Result<Self> {
        let args = build_claude_args(config);
        let window_name = format!("tb-{name}");

        let spawned = backend.spawn(
            &window_name,
            "claude",
            &args,
            config.cwd.as_deref(),
            rows,
            cols,
        )?;

        let mut info = SessionInfo::new(name);
        info.claude_session_id = config.claude_session_id.clone();
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
        ))
    }

    /// Reconnect to an existing backend session.
    pub fn adopt(
        name: String,
        rows: u16,
        cols: u16,
        backend_id: &str,
        backend: &Arc<dyn SessionBackend>,
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
    ) -> Self {
        let (state, backend_id) = Self::wire_up(rows, cols, io);
        Self {
            info,
            parser: state.parser,
            input_tx: state.input_tx,
            backend_id,
            backend: Arc::clone(backend),
            exited: state.exited,
            last_output_at: state.last_output_at,
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

    /// Restart the session: kill the old pane, spawn a fresh one with new config.
    ///
    /// Uses `--resume` so Claude picks up the conversation while getting
    /// freshly-resolved role permissions.
    pub fn restart(&mut self, config: &SessionConfig, rows: u16, cols: u16) -> Result<()> {
        self.backend.kill(&self.backend_id)?;

        let args = build_claude_args(config);
        let window_name = format!("tb-{}", self.info.name);
        let spawned = self.backend.spawn(
            &window_name,
            "claude",
            &args,
            config.cwd.as_deref(),
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
        self.info.backend_id = Some(self.backend_id.clone());
        if !config.role.is_empty() {
            self.info.role = config.role.clone();
        }

        debug!(session_id = %self.info.id, backend_id = %self.backend_id, "Restarted session");
        Ok(())
    }

    /// Kill/destroy the backend session (for Ctrl+X close).
    pub fn kill(&self) {
        if let Err(e) = self.backend.kill(&self.backend_id) {
            tracing::warn!("Failed to kill session: {e}");
        }
    }

    /// Detach from the backend session without killing it (for Ctrl+Q quit).
    pub fn detach(self) {
        if let Err(e) = self.backend.detach(&self.backend_id) {
            tracing::warn!("Failed to detach session: {e}");
        }
        drop(self.input_tx);
        debug!("Session detached");
    }

    /// Create a lightweight stub for unit tests (no real backend process).
    #[cfg(test)]
    pub fn stub(name: &str, backend: &Arc<dyn SessionBackend>) -> Self {
        let (input_tx, _input_rx) = mpsc::unbounded_channel();
        Self {
            info: SessionInfo::new(name.to_string()),
            parser: Arc::new(Mutex::new(vt100::Parser::new(24, 80, 0))),
            input_tx,
            backend_id: String::new(),
            backend: Arc::clone(backend),
            exited: Arc::new(AtomicBool::new(false)),
            last_output_at: Arc::new(AtomicU64::new(now_millis())),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::session::RolePermissions;

    #[test]
    fn build_args_empty_config() {
        let config = SessionConfig::default();
        let args = build_claude_args(&config);
        assert_eq!(args, vec!["--permission-mode", "default"]);
    }

    #[test]
    fn build_args_no_permissions() {
        let config = SessionConfig {
            claude_session_id: Some("abc-123".to_string()),
            ..SessionConfig::default()
        };
        let args = build_claude_args(&config);
        assert_eq!(
            args,
            vec!["--session-id", "abc-123", "--permission-mode", "default"]
        );
    }

    #[test]
    fn build_args_resume_takes_precedence() {
        let config = SessionConfig {
            resume_session_id: Some("resume-id".to_string()),
            claude_session_id: Some("session-id".to_string()),
            ..SessionConfig::default()
        };
        let args = build_claude_args(&config);
        assert_eq!(
            args,
            vec!["--resume", "resume-id", "--permission-mode", "default"]
        );
    }

    #[test]
    fn build_args_with_permission_mode() {
        let config = SessionConfig {
            permissions: RolePermissions {
                permission_mode: Some("plan".to_string()),
                ..RolePermissions::default()
            },
            ..SessionConfig::default()
        };
        let args = build_claude_args(&config);
        assert_eq!(args, vec!["--permission-mode", "plan"]);
    }

    #[test]
    fn build_args_with_allowed_tools() {
        let config = SessionConfig {
            permissions: RolePermissions {
                allowed_tools: vec!["Read".to_string(), "Bash(git:*)".to_string()],
                ..RolePermissions::default()
            },
            ..SessionConfig::default()
        };
        let args = build_claude_args(&config);
        assert_eq!(
            args,
            vec![
                "--permission-mode",
                "default",
                "--allowed-tools",
                "Read Bash(git:*)"
            ]
        );
    }

    #[test]
    fn build_args_with_disallowed_tools() {
        let config = SessionConfig {
            permissions: RolePermissions {
                disallowed_tools: vec!["Edit".to_string()],
                ..RolePermissions::default()
            },
            ..SessionConfig::default()
        };
        let args = build_claude_args(&config);
        assert_eq!(
            args,
            vec!["--permission-mode", "default", "--disallowed-tools", "Edit"]
        );
    }

    #[test]
    fn build_args_with_tools_empty_string() {
        let config = SessionConfig {
            permissions: RolePermissions {
                tools: Some(String::new()),
                ..RolePermissions::default()
            },
            ..SessionConfig::default()
        };
        let args = build_claude_args(&config);
        assert_eq!(args, vec!["--permission-mode", "default", "--tools", ""]);
    }

    #[test]
    fn build_args_with_system_prompt() {
        let config = SessionConfig {
            permissions: RolePermissions {
                append_system_prompt: Some("Be careful".to_string()),
                ..RolePermissions::default()
            },
            ..SessionConfig::default()
        };
        let args = build_claude_args(&config);
        assert_eq!(
            args,
            vec![
                "--permission-mode",
                "default",
                "--append-system-prompt",
                "Be careful"
            ]
        );
    }

    #[test]
    fn now_millis_returns_reasonable_value() {
        let ms = now_millis();
        // Should be after 2024-01-01 (1704067200000 ms since epoch).
        assert!(ms > 1_704_067_200_000);
    }

    #[test]
    fn build_args_with_additional_dirs() {
        let config = SessionConfig {
            additional_dirs: vec![
                PathBuf::from("/home/user/repo2"),
                PathBuf::from("/home/user/repo3"),
            ],
            ..SessionConfig::default()
        };
        let args = build_claude_args(&config);
        assert_eq!(
            args,
            vec![
                "--permission-mode",
                "default",
                "--add-dir",
                "/home/user/repo2",
                "--add-dir",
                "/home/user/repo3",
            ]
        );
    }

    #[test]
    fn build_args_all_fields() {
        let config = SessionConfig {
            claude_session_id: Some("id-1".to_string()),
            additional_dirs: vec![PathBuf::from("/extra")],
            permissions: RolePermissions {
                permission_mode: Some("plan".to_string()),
                allowed_tools: vec!["Read".to_string()],
                disallowed_tools: vec!["Edit".to_string()],
                tools: Some("default".to_string()),
                append_system_prompt: Some("Focus".to_string()),
            },
            ..SessionConfig::default()
        };
        let args = build_claude_args(&config);
        assert_eq!(
            args,
            vec![
                "--session-id",
                "id-1",
                "--permission-mode",
                "plan",
                "--allowed-tools",
                "Read",
                "--disallowed-tools",
                "Edit",
                "--tools",
                "default",
                "--append-system-prompt",
                "Focus",
                "--add-dir",
                "/extra",
            ]
        );
    }
}
