use std::collections::{HashMap, VecDeque};
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{sync_channel, SyncSender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use anyhow::{bail, Context, Result};
use tracing::{debug, warn};

use crate::agent::backend::{AdoptedSession, DiscoveredSession, SessionBackend, SpawnedSession};
use crate::agent::control_mode::{
    self, shell_escape, CommandResponse, ControlModeReader, ControlModeWriter, Notification,
    PaneSendersMapShared, PANE_CHANNEL_CAPACITY,
};

/// Dedicated tmux socket name — isolates thurbox sessions from the user's tmux.
/// Dev builds use "thurbox-dev" to avoid interfering with an installed release binary.
const TMUX_SOCKET: &str = if cfg!(dev_build) {
    "thurbox-dev"
} else {
    "thurbox"
};

/// tmux session name used to group all thurbox windows.
/// Dev builds use "thurbox-dev" to avoid interfering with an installed release binary.
const TMUX_SESSION: &str = if cfg!(dev_build) {
    "thurbox-dev"
} else {
    "thurbox"
};

/// Window-name prefix for thurbox-managed tmux windows. Combined with the
/// sanitized session name (`{prefix}{sanitized_name}`) to form the tmux
/// window target.
pub(crate) const WINDOW_PREFIX: &str = "tb-";

/// Prefix for the companion shell window a session lazily spawns.
pub(crate) const SHELL_WINDOW_PREFIX: &str = "tbs-";

/// Sanitize a session name into a tmux-safe window-name component.
///
/// tmux parses target strings as `session:window`, and — depending on
/// version and context (e.g. `run-shell` scripts, `display-message`
/// format expansion) — treats whitespace, colons, commas, and `.` as
/// delimiters within the target string. Any character outside
/// `[A-Za-z0-9_-]` is replaced with `_` so the produced window name
/// round-trips cleanly through every tmux CLI/control-mode call.
///
/// The resulting string is deterministic — callers must use it both at
/// window-creation time and at lookup time for matching to succeed.
pub(crate) fn sanitize_window_name(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for c in name.chars() {
        if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
            out.push(c);
        } else {
            out.push('_');
        }
    }
    out
}

/// Build the tmux window name for a thurbox agent session: `tb-<safe>`.
pub(crate) fn agent_window_name(session_name: &str) -> String {
    format!("{WINDOW_PREFIX}{}", sanitize_window_name(session_name))
}

/// Build the tmux window name for a session's companion shell pane.
pub(crate) fn shell_window_name(session_name: &str) -> String {
    format!(
        "{SHELL_WINDOW_PREFIX}{}",
        sanitize_window_name(session_name)
    )
}

/// Build the `session:window` tmux target for a thurbox agent session.
fn window_target(session_name: &str) -> String {
    format!("{TMUX_SESSION}:{}", agent_window_name(session_name))
}

/// Minimum tmux version required.
const MIN_TMUX_VERSION: (u32, u32) = (3, 2);

/// Timeout for waiting for a control mode command response.
const COMMAND_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// Delay (in seconds) between sending command text and pressing Enter via tmux,
/// giving the target application time to process the input.
const SEND_KEYS_ENTER_DELAY_SECS: &str = "0.2";

/// Same delay as a `Duration`, used by the synchronous `send_prompt_now` path.
const SEND_KEYS_ENTER_DELAY: std::time::Duration = std::time::Duration::from_millis(200);

/// Hard cap on the number of scrollback lines `capture_pane_text` will return.
const MAX_CAPTURE_LINES: u32 = 10_000;

/// Local tmux backend — sessions persist in `tmux -L thurbox`.
///
/// Uses tmux control mode (`-C`) for all I/O after `ensure_ready()`.
pub struct LocalTmuxBackend {
    control: Mutex<Option<ControlMode>>,
}

impl Default for LocalTmuxBackend {
    fn default() -> Self {
        Self {
            control: Mutex::new(None),
        }
    }
}

/// A live tmux control mode connection.
///
/// Commands are sent serially (stdin lock ensures ordering) and responses arrive
/// in the same order. We use a FIFO queue instead of matching command numbers,
/// which avoids numbering mismatches between our counter and tmux's internal
/// counter (e.g., from `send_command_nowait` calls that still consume a tmux
/// command number).
struct ControlMode {
    stdin: Arc<Mutex<ChildStdin>>,
    pane_senders: PaneSendersMapShared,
    /// FIFO queue of response channels — one per `send_command()` call, in order.
    response_queue: Arc<Mutex<VecDeque<SyncSender<CommandResponse>>>>,
    reader_handle: Mutex<Option<JoinHandle<()>>>,
    child: Mutex<Child>,
}

impl ControlMode {
    /// Start a control mode connection to the thurbox tmux session.
    fn start() -> Result<Self> {
        // -C (single C): control mode with echo — works with piped stdin.
        // -CC (double C) requires a TTY and fails with "tcgetattr: Inappropriate ioctl".
        let mut child = Command::new("tmux")
            .arg("-L")
            .arg(TMUX_SOCKET)
            .arg("-C")
            .arg("attach-session")
            .arg("-t")
            .arg(TMUX_SESSION)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .context("Failed to start tmux control mode")?;

        let stdin = child
            .stdin
            .take()
            .context("Failed to get control mode stdin")?;
        let stdout = child
            .stdout
            .take()
            .context("Failed to get control mode stdout")?;

        let stdin = Arc::new(Mutex::new(stdin));
        let pane_senders: PaneSendersMapShared =
            Arc::new(Mutex::new(std::collections::HashMap::new()));
        let response_queue: Arc<Mutex<VecDeque<SyncSender<CommandResponse>>>> =
            Arc::new(Mutex::new(VecDeque::new()));

        let reader_stdin = Arc::clone(&stdin);
        let reader_pane_senders = Arc::clone(&pane_senders);
        let reader_queue = Arc::clone(&response_queue);

        let reader_handle = std::thread::Builder::new()
            .name("tmux-control-reader".into())
            .spawn(move || {
                Self::reader_thread(stdout, reader_stdin, reader_pane_senders, reader_queue);
            })
            .context("Failed to spawn control reader thread")?;

        let control = Self {
            stdin,
            pane_senders,
            response_queue,
            reader_handle: Mutex::new(Some(reader_handle)),
            child: Mutex::new(child),
        };

        // Drain the implicit attach response (%begin/%end) that tmux sends
        // when a control mode client connects. We send a no-op command and
        // wait for its response — this synchronizes with the reader thread
        // and guarantees all prior unsolicited responses have been consumed.
        control.send_command("refresh-client")?;

        // Enable flow control (pause-after=5 seconds of buffered output).
        control.send_command("refresh-client -f pause-after=5")?;

        Ok(control)
    }

    /// Background thread that reads and dispatches control mode output.
    ///
    /// Responses arrive in FIFO order matching `send_command()` calls.
    /// We track a single in-flight response at a time (`%begin` → collect
    /// lines → `%end`/`%error`), then pop the next waiter from the queue.
    /// Commands sent via `send_command_nowait()` also produce `%begin`/`%end`
    /// blocks, but no waiter is in the queue for them — those responses are
    /// simply discarded.
    fn reader_thread(
        stdout: std::process::ChildStdout,
        stdin: Arc<Mutex<ChildStdin>>,
        pane_senders: PaneSendersMapShared,
        response_queue: Arc<Mutex<VecDeque<SyncSender<CommandResponse>>>>,
    ) {
        let mut reader = BufReader::new(stdout);
        // Accumulates response lines for the current in-flight command.
        let mut collecting: Option<Vec<String>> = None;
        let mut line_buf = Vec::new();

        loop {
            line_buf.clear();
            match reader.read_until(b'\n', &mut line_buf) {
                Ok(0) => break, // EOF
                Ok(_) => {}
                Err(e) => {
                    debug!("Control reader I/O error: {e}");
                    break;
                }
            }
            // Strip trailing newline.
            if line_buf.last() == Some(&b'\n') {
                line_buf.pop();
            }
            // Lossy conversion: tmux control mode is mostly ASCII, but raw
            // bytes can appear (e.g., in %extended-output). Replacing
            // invalid sequences with U+FFFD is safe — the octal-encoded
            // payload in %output lines is always valid ASCII.
            let line = String::from_utf8_lossy(&line_buf);

            match control_mode::parse_notification(&line) {
                Notification::Output { pane_id, data } => {
                    if let Ok(senders) = pane_senders.lock() {
                        if let Some(tx_vec) = senders.get(&pane_id) {
                            // Broadcast output to all registered instances
                            for tx in tx_vec {
                                match tx.try_send(data.clone()) {
                                    Ok(()) => {}
                                    Err(std::sync::mpsc::TrySendError::Full(_dropped)) => {
                                        // Channel full — drop this chunk rather than blocking.
                                        // The reader thread MUST stay unblocked to handle
                                        // %pause notifications and avoid deadlock.
                                        debug!(
                                            pane_id = %pane_id,
                                            "Pane output channel full, dropping chunk"
                                        );
                                    }
                                    Err(std::sync::mpsc::TrySendError::Disconnected(_)) => {}
                                }
                            }
                        }
                    }
                }
                Notification::Begin => {
                    collecting = Some(Vec::new());
                }
                end_or_error @ (Notification::End | Notification::Error) => {
                    let lines = collecting.take().unwrap_or_default();
                    if let Ok(mut queue) = response_queue.lock() {
                        if let Some(tx) = queue.pop_front() {
                            let _ = tx.send(CommandResponse {
                                lines,
                                is_error: matches!(end_or_error, Notification::Error),
                            });
                        }
                    }
                }
                Notification::Pause { pane_id } => {
                    let cmd = format!(
                        "refresh-client -A '{}:continue'\n",
                        pane_id.replace('\'', "'\\''")
                    );
                    if let Ok(mut s) = stdin.lock() {
                        let _ = s.write_all(cmd.as_bytes());
                        let _ = s.flush();
                    }
                }
                Notification::Other(text) => {
                    if let Some(ref mut lines) = collecting {
                        lines.push(text);
                    }
                }
            }
        }

        // EOF — control mode connection ended. Close all pane senders so readers get EOF.
        debug!("Control reader thread exiting");
        if let Ok(mut senders) = pane_senders.lock() {
            senders.clear();
        }
    }

    /// Send a command and wait for its response.
    fn send_command(&self, cmd: &str) -> Result<String> {
        let (tx, rx) = sync_channel(1);

        // Enqueue our response channel before sending, so the reader thread
        // can deliver the response even if it arrives before we start waiting.
        {
            let mut queue = self
                .response_queue
                .lock()
                .map_err(|e| anyhow::anyhow!("response_queue lock: {e}"))?;
            queue.push_back(tx);
        }

        {
            let mut stdin = self
                .stdin
                .lock()
                .map_err(|e| anyhow::anyhow!("stdin lock: {e}"))?;
            writeln!(stdin, "{cmd}")?;
            stdin.flush()?;
        }

        let response = rx
            .recv_timeout(COMMAND_TIMEOUT)
            .context(format!("Timeout waiting for response to: {cmd}"))?;

        if response.is_error {
            bail!("tmux command failed: {cmd}: {}", response.lines.join("\n"));
        }

        Ok(response.lines.join("\n"))
    }

    /// Send a command without waiting for a response.
    ///
    /// **Caution**: The response (`%begin`/`%end`) will still arrive on the
    /// control mode stream. If a `send_command` call follows before the
    /// response is consumed, the nowait response may steal the waiter.
    /// Only use this when no `send_command` follows, or when the caller
    /// is the reader thread itself (e.g., pause resume).
    fn send_command_nowait(&self, cmd: &str) -> Result<()> {
        let mut stdin = self
            .stdin
            .lock()
            .map_err(|e| anyhow::anyhow!("stdin lock: {e}"))?;
        writeln!(stdin, "{cmd}")?;
        stdin.flush()?;
        Ok(())
    }
}

impl Drop for ControlMode {
    fn drop(&mut self) {
        // Try to gracefully detach.
        if let Ok(mut stdin) = self.stdin.lock() {
            let _ = writeln!(stdin, "detach-client");
            let _ = stdin.flush();
        }

        // Give the child a moment to exit gracefully, then force-kill so the
        // reader thread gets EOF promptly and we never block indefinitely.
        if let Ok(mut child) = self.child.lock() {
            let exited = (0..3).any(|_| {
                if matches!(child.try_wait(), Ok(Some(_))) {
                    return true;
                }
                std::thread::sleep(std::time::Duration::from_millis(100));
                false
            });
            if !exited {
                let _ = child.kill();
                let _ = child.wait();
            }
        }

        // Reader thread should exit now that the child is dead (stdout closed).
        if let Ok(mut handle) = self.reader_handle.lock() {
            if let Some(h) = handle.take() {
                let _ = h.join();
            }
        }
    }
}

/// Check if an error is caused by a broken pipe (control mode stdin closed).
fn is_broken_pipe(err: &anyhow::Error) -> bool {
    err.chain().any(|cause| {
        cause
            .downcast_ref::<std::io::Error>()
            .is_some_and(|e| e.kind() == std::io::ErrorKind::BrokenPipe)
    })
}

/// Check if an error is caused by a recv timeout (reader thread died, response never arrives).
fn is_recv_timeout(err: &anyhow::Error) -> bool {
    err.chain().any(|cause| {
        cause
            .downcast_ref::<std::sync::mpsc::RecvTimeoutError>()
            .is_some()
    })
}

impl LocalTmuxBackend {
    pub fn new() -> Self {
        Self::default()
    }

    /// Run a tmux command and return its stdout (used before control mode is available).
    fn tmux_output(&self, args: &[&str]) -> Result<String> {
        let output = Self::run_tmux(args)?;
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    }

    /// Run a tmux command, returning Ok(()) on success (used before control mode is available).
    fn tmux_run(&self, args: &[&str]) -> Result<()> {
        Self::run_tmux(args)?;
        Ok(())
    }

    /// Execute a tmux command on the thurbox socket and check for errors.
    fn run_tmux(args: &[&str]) -> Result<std::process::Output> {
        let output = Command::new("tmux")
            .arg("-L")
            .arg(TMUX_SOCKET)
            .args(args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .context("Failed to run tmux command")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            bail!("tmux {} failed: {}", args.join(" "), stderr.trim());
        }

        Ok(output)
    }

    /// Check if the thurbox tmux session exists.
    fn session_exists(&self) -> bool {
        self.tmux_run(&["has-session", "-t", TMUX_SESSION]).is_ok()
    }

    /// Apply initial config to the tmux server.
    fn apply_config(&self) -> Result<()> {
        // Use a non-login shell so that macOS path_helper (/etc/zprofile)
        // doesn't clobber PATH additions from ~/.zshenv (e.g. cargo, asdf).
        let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string());
        self.tmux_run(&["set-option", "-s", "default-command", &shell])?;

        // Server-wide options
        let server_opts = [
            ("default-terminal", "xterm-256color"),
            ("extended-keys", "on"),
        ];
        for (key, val) in &server_opts {
            self.tmux_run(&["set-option", "-s", key, val])?;
        }

        // Session-level options
        let session_opts = [
            ("remain-on-exit", "on"),
            ("status", "off"),
            ("history-limit", "5000"),
            // Allow each window to have its own size, not constrained
            // by the smallest attached client.
            ("window-size", "manual"),
        ];
        for (key, val) in &session_opts {
            self.tmux_run(&["set-option", "-t", TMUX_SESSION, key, val])?;
        }

        Ok(())
    }

    /// Build the shell command string to pass to tmux new-window.
    fn build_shell_command(command: &str, args: &[String]) -> String {
        let mut parts = vec![command.to_string()];
        for arg in args {
            parts.push(control_mode::shell_escape(arg));
        }
        parts.join(" ")
    }

    /// Get a reference to the active control mode, or bail.
    fn control(&self) -> Result<std::sync::MutexGuard<'_, Option<ControlMode>>> {
        let guard = self
            .control
            .lock()
            .map_err(|e| anyhow::anyhow!("control lock: {e}"))?;
        if guard.is_none() {
            bail!("Control mode not started — call ensure_ready() first");
        }
        Ok(guard)
    }

    /// Drop the dead control mode connection and start a fresh one.
    fn reconnect_control(&self) -> Result<()> {
        let mut guard = self
            .control
            .lock()
            .map_err(|e| anyhow::anyhow!("control lock: {e}"))?;
        *guard = None; // Drop dead ControlMode (triggers cleanup)
        *guard = Some(ControlMode::start()?);
        debug!("Control mode reconnected successfully");
        Ok(())
    }

    /// Send a command via control mode and return the response.
    /// On broken pipe or timeout, reconnects control mode and retries once.
    fn ctrl_command(&self, cmd: &str) -> Result<String> {
        let result = {
            let guard = self.control()?;
            guard.as_ref().unwrap().send_command(cmd)
        };
        match result {
            Ok(val) => Ok(val),
            Err(err) if is_broken_pipe(&err) || is_recv_timeout(&err) => {
                warn!("Control mode error, reconnecting: {err:#}");
                self.reconnect_control()?;
                let guard = self.control()?;
                guard.as_ref().unwrap().send_command(cmd)
            }
            Err(err) => Err(err),
        }
    }

    /// Send a command via control mode without waiting for a response.
    /// On broken pipe, reconnects control mode and retries once.
    fn ctrl_command_nowait(&self, cmd: &str) -> Result<()> {
        let result = {
            let guard = self.control()?;
            guard.as_ref().unwrap().send_command_nowait(cmd)
        };
        match result {
            Ok(()) => Ok(()),
            Err(err) if is_broken_pipe(&err) => {
                warn!("Control mode broken pipe (nowait), reconnecting: {err:#}");
                self.reconnect_control()?;
                let guard = self.control()?;
                guard.as_ref().unwrap().send_command_nowait(cmd)
            }
            Err(err) => Err(err),
        }
    }

    /// Register a pane sender and return the corresponding reader.
    /// Multiple instances can register the same pane; output will be broadcast to all.
    fn register_pane(&self, pane_id: &str) -> Result<ControlModeReader> {
        let guard = self.control()?;
        let ctrl = guard.as_ref().unwrap();
        let (tx, rx) = sync_channel(PANE_CHANNEL_CAPACITY);
        {
            let mut senders = ctrl
                .pane_senders
                .lock()
                .map_err(|e| anyhow::anyhow!("pane_senders lock: {e}"))?;
            senders
                .entry(pane_id.to_string())
                .or_insert_with(Vec::new)
                .push(tx);
        }
        Ok(ControlModeReader::new(rx))
    }

    /// Unregister a pane sender (causes the reader to get EOF).
    /// Note: Currently removes all senders for this pane. For true instance-specific
    /// unregistration, we would need to track which sender belongs to which instance.
    fn unregister_pane(&self, pane_id: &str) -> Result<()> {
        let guard = self.control()?;
        let ctrl = guard.as_ref().unwrap();
        let mut senders = ctrl
            .pane_senders
            .lock()
            .map_err(|e| anyhow::anyhow!("pane_senders lock: {e}"))?;
        // Remove all senders for this pane (all instances lose the pane)
        senders.remove(pane_id);
        Ok(())
    }

    /// Create a writer for a specific pane.
    fn pane_writer(&self, pane_id: &str) -> Result<ControlModeWriter> {
        let guard = self.control()?;
        let ctrl = guard.as_ref().unwrap();
        Ok(ControlModeWriter {
            stdin: Arc::clone(&ctrl.stdin),
            pane_id: pane_id.to_string(),
        })
    }

    /// Connect I/O to an existing pane: start monitoring, resize to correct
    /// dimensions, and create writer.
    fn connect_pane(&self, pane_id: &str, rows: u16, cols: u16) -> Result<AdoptedSession> {
        let reader = self.register_pane(pane_id)?;
        // Must use send_command (waited) here — a nowait call would leave an
        // unclaimed %begin/%end response in the stream that steals the next
        // send_command waiter.
        self.ctrl_command(&format!(
            "refresh-client -A '{}:on'",
            pane_id.replace('\'', "'\\''")
        ))?;

        // Resize to the TUI panel dimensions. force_resize triggers a
        // SIGWINCH, making TUI applications (like claude) repaint at the
        // correct dimensions through the normal output stream, which the
        // reader_loop processes with all escape sequences intact.
        self.force_resize(pane_id, rows, cols)?;

        let writer = self.pane_writer(pane_id)?;

        Ok(AdoptedSession {
            output: Box::new(reader),
            input: Box::new(writer),
        })
    }

    /// Resize a pane, forcing a SIGWINCH even if dimensions haven't changed.
    fn force_resize(&self, pane_id: &str, rows: u16, cols: u16) -> Result<()> {
        // Briefly resize to different dimensions to guarantee a SIGWINCH,
        // then resize to the actual target. This causes TUI apps to repaint.
        if rows > 1 {
            self.resize(pane_id, rows - 1, cols)?;
        } else {
            self.resize(pane_id, rows + 1, cols)?;
        }
        self.resize(pane_id, rows, cols)?;
        Ok(())
    }
}

impl SessionBackend for LocalTmuxBackend {
    fn name(&self) -> &str {
        "local-tmux"
    }

    fn check_available(&self) -> Result<()> {
        let output = Command::new("tmux")
            .arg("-V")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .context("tmux is not installed or not in PATH")?;

        if !output.status.success() {
            bail!("tmux -V failed");
        }

        let version_str = String::from_utf8_lossy(&output.stdout);
        let version_str = version_str.trim();
        // Parse "tmux X.Y" or "tmux X.Ya" (e.g., "tmux 3.4" or "tmux 3.3a")
        let version_part = version_str.strip_prefix("tmux ").unwrap_or(version_str);

        let parts: Vec<&str> = version_part.split('.').collect();
        if parts.len() < 2 {
            bail!("Cannot parse tmux version from: {version_str}");
        }

        let major: u32 = parts[0].parse().context(format!(
            "Cannot parse tmux major version from: {version_str}"
        ))?;
        // Minor might have a trailing letter (e.g., "3a"), strip non-digits.
        let minor_str: String = parts[1]
            .chars()
            .take_while(|c| c.is_ascii_digit())
            .collect();
        let minor: u32 = minor_str.parse().context(format!(
            "Cannot parse tmux minor version from: {version_str}"
        ))?;

        if (major, minor) < MIN_TMUX_VERSION {
            bail!(
                "tmux {major}.{minor} is too old; thurbox requires >= {}.{}",
                MIN_TMUX_VERSION.0,
                MIN_TMUX_VERSION.1
            );
        }

        debug!("tmux version: {version_str}");
        Ok(())
    }

    fn ensure_ready(&self) -> Result<()> {
        if !self.session_exists() {
            debug!("Creating tmux session '{TMUX_SESSION}' on socket '{TMUX_SOCKET}'");
            let output = Command::new("tmux")
                .arg("-L")
                .arg(TMUX_SOCKET)
                .args([
                    "new-session",
                    "-d",
                    "-s",
                    TMUX_SESSION,
                    "-x",
                    "80",
                    "-y",
                    "24",
                ])
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .output()
                .context("Failed to create tmux session")?;

            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                bail!("Failed to create tmux session: {}", stderr.trim());
            }

            self.apply_config()?;
        }

        // Start control mode if not already running.
        let mut guard = self
            .control
            .lock()
            .map_err(|e| anyhow::anyhow!("control lock: {e}"))?;
        if guard.is_none() {
            debug!("Starting tmux control mode");
            *guard = Some(ControlMode::start()?);
        }

        Ok(())
    }

    fn spawn(
        &self,
        window_name: &str,
        command: &str,
        args: &[String],
        cwd: Option<&Path>,
        env: &HashMap<String, String>,
        rows: u16,
        cols: u16,
    ) -> Result<SpawnedSession> {
        let shell_cmd = Self::build_shell_command(command, args);

        let cwd_part = match cwd {
            Some(dir) => format!(" -c {}", control_mode::shell_escape(&dir.to_string_lossy())),
            None => String::new(),
        };
        let env_part: String = env
            .iter()
            .map(|(k, v)| format!(" -e {}", shell_escape(&format!("{k}={v}"))))
            .collect();
        let escaped_window_name = shell_escape(window_name);
        let cmd = format!(
            "new-window -t {TMUX_SESSION} -n {escaped_window_name} -P -F '#{{pane_id}}'{cwd_part}{env_part} {shell_cmd}"
        );
        let result = self.ctrl_command(&cmd)?;
        let pane_id = result.trim().to_string();

        debug!(pane_id = %pane_id, "tmux window created via control mode");

        let connected = self.connect_pane(&pane_id, rows, cols)?;

        Ok(SpawnedSession {
            backend_id: pane_id,
            output: connected.output,
            input: connected.input,
        })
    }

    fn adopt(&self, backend_id: &str, rows: u16, cols: u16) -> Result<AdoptedSession> {
        self.connect_pane(backend_id, rows, cols)
    }

    fn discover(&self) -> Result<Vec<DiscoveredSession>> {
        if !self.session_exists() {
            return Ok(Vec::new());
        }

        // Use control mode if available, otherwise fall back to direct tmux command.
        let result = {
            let guard = self
                .control
                .lock()
                .map_err(|e| anyhow::anyhow!("control lock: {e}"))?;
            if let Some(ref ctrl) = *guard {
                ctrl.send_command(&format!(
                    "list-windows -t {TMUX_SESSION} -F '#{{pane_id}}|#{{window_name}}|#{{pane_dead}}'"
                ))?
            } else {
                self.tmux_output(&[
                    "list-windows",
                    "-t",
                    TMUX_SESSION,
                    "-F",
                    "#{pane_id}|#{window_name}|#{pane_dead}",
                ])?
            }
        };

        let mut sessions = Vec::new();
        for line in result.lines() {
            let parts: Vec<&str> = line.splitn(3, '|').collect();
            if parts.len() < 3 {
                continue;
            }

            let window_name = parts[1];
            // Only discover windows with our prefix (tb- for Claude, tbs- for shells).
            if !window_name.starts_with("tb-") {
                continue;
            }

            sessions.push(DiscoveredSession {
                backend_id: parts[0].to_string(),
                name: window_name.to_string(),
                is_alive: parts[2] != "1",
            });
        }

        Ok(sessions)
    }

    fn resize(&self, backend_id: &str, rows: u16, cols: u16) -> Result<()> {
        // Resize the window first — panes cannot exceed their window's dimensions.
        self.ctrl_command(&format!(
            "resize-window -t {backend_id} -x {cols} -y {rows}"
        ))?;

        // Then resize the pane within the window.
        self.ctrl_command(&format!("resize-pane -t {backend_id} -x {cols} -y {rows}"))?;

        Ok(())
    }

    fn is_dead(&self, backend_id: &str) -> Result<bool> {
        let result = self.ctrl_command(&format!(
            "display-message -t {backend_id} -p '#{{pane_dead}}'"
        ))?;
        Ok(result.trim() == "1")
    }

    fn kill(&self, backend_id: &str) -> Result<()> {
        let _ = self.unregister_pane(backend_id);
        self.ctrl_command(&format!("kill-pane -t {backend_id}"))?;
        Ok(())
    }

    fn detach(&self, backend_id: &str) -> Result<()> {
        // Disable output monitoring for this pane.
        if let Err(e) = self.ctrl_command_nowait(&format!(
            "refresh-client -A '{}:off'",
            backend_id.replace('\'', "'\\''")
        )) {
            warn!("Failed to disable output monitoring during detach: {e}");
        }
        // Remove the pane sender — the ControlModeReader gets EOF.
        let _ = self.unregister_pane(backend_id);
        Ok(())
    }

    fn pane_pid(&self, backend_id: &str) -> Result<Option<u32>> {
        let result = self.ctrl_command(&format!(
            "display-message -t {backend_id} -p '#{{pane_pid}}'"
        ))?;
        Ok(result.trim().parse().ok())
    }
}

/// Schedule a command to be sent to a tmux session window after a delay.
///
/// Uses `tmux run-shell -b -d <seconds>` so the timer fires independently of
/// Thurbox. Before sending, the shell script checks `cancelled_at` in the DB
/// (via `sqlite3` CLI) and marks `executed_at` after sending.
pub fn schedule_tmux_command(
    session_name: &str,
    command_text: &str,
    delay_seconds: u64,
    command_id: i64,
    db_path: &Path,
) -> Result<()> {
    let escaped_text = shell_escape(command_text);
    let escaped_db = shell_escape(&db_path.display().to_string());
    let target = window_target(session_name);
    let escaped_target = shell_escape(&target);

    // Shell script that checks cancellation before sending, then marks executed.
    // Sends the text first, waits for the app to process, then sends Enter.
    let script = format!(
        "cancelled=$(sqlite3 {escaped_db} \
         \"SELECT cancelled_at FROM scheduled_commands WHERE id={command_id};\"); \
         if [ -z \"$cancelled\" ]; then \
         tmux -L {TMUX_SOCKET} send-keys -t {escaped_target} {escaped_text}; \
         sleep {SEND_KEYS_ENTER_DELAY_SECS}; \
         tmux -L {TMUX_SOCKET} send-keys -t {escaped_target} Enter; \
         sqlite3 {escaped_db} \
         \"UPDATE scheduled_commands SET executed_at=$(date +%s)000 WHERE id={command_id};\"; \
         fi"
    );

    let status = Command::new("tmux")
        .arg("-L")
        .arg(TMUX_SOCKET)
        .arg("run-shell")
        .arg("-b")
        .arg("-d")
        .arg(delay_seconds.to_string())
        .arg(script)
        .status()
        .context("Failed to spawn tmux run-shell for scheduled command")?;

    if !status.success() {
        bail!(
            "tmux run-shell exited with status {} for command {}",
            status,
            command_id
        );
    }

    Ok(())
}

/// Send text immediately to a session pane (no scheduling), followed by Enter.
///
/// Used by the MCP `send_prompt` tool. Targets the tmux window named
/// `tb-<session_name>` in the thurbox tmux session and uses the same
/// "type text → brief delay → press Enter" sequence that `schedule_tmux_command`
/// uses so the target app has time to process the typed input.
pub fn send_prompt_now(session_name: &str, text: &str) -> Result<()> {
    let target = window_target(session_name);

    let status = Command::new("tmux")
        .args(["-L", TMUX_SOCKET, "send-keys", "-t", &target, text])
        .status()
        .context("Failed to run tmux send-keys for prompt text")?;
    if !status.success() {
        bail!("tmux send-keys (text) exited with status {status}");
    }

    std::thread::sleep(SEND_KEYS_ENTER_DELAY);

    let status = Command::new("tmux")
        .args(["-L", TMUX_SOCKET, "send-keys", "-t", &target, "Enter"])
        .status()
        .context("Failed to run tmux send-keys for Enter")?;
    if !status.success() {
        bail!("tmux send-keys (Enter) exited with status {status}");
    }
    Ok(())
}

/// Capture the rendered contents of a session's pane.
///
/// Returns the visible terminal text. `lines` controls how many lines of
/// scrollback to include before the visible region (capped to a sane max).
pub fn capture_pane_text(session_name: &str, lines: u32) -> Result<String> {
    let target = window_target(session_name);
    let lines = lines.min(MAX_CAPTURE_LINES);
    let start = format!("-{lines}");

    let output = Command::new("tmux")
        .args([
            "-L",
            TMUX_SOCKET,
            "capture-pane",
            "-p",
            "-J",
            "-t",
            &target,
            "-S",
            &start,
        ])
        .output()
        .context("Failed to run tmux capture-pane")?;
    if !output.status.success() {
        bail!(
            "tmux capture-pane exited with status {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Session-level tmux options applied to the thurbox tmux session.
///
/// Mirrored by [`LocalTmuxBackend::apply_config`] for the TUI path; the
/// headless path applies only this subset.
const HEADLESS_SESSION_OPTS: &[(&str, &str)] = &[
    ("remain-on-exit", "on"),
    ("status", "off"),
    ("history-limit", "5000"),
    // Allow each window to size independently of the smallest attached client.
    ("window-size", "manual"),
];

/// Run `tmux has-session -t <thurbox session>` and return whether it exists.
fn tmux_session_exists() -> Result<bool> {
    let status = Command::new("tmux")
        .args(["-L", TMUX_SOCKET, "has-session", "-t", TMUX_SESSION])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .context("Failed to run tmux has-session")?;
    Ok(status.success())
}

/// Create the thurbox tmux session (`-d -s <name>`) at a default 80x24 size.
fn tmux_create_session() -> Result<()> {
    let output = Command::new("tmux")
        .args([
            "-L",
            TMUX_SOCKET,
            "new-session",
            "-d",
            "-s",
            TMUX_SESSION,
            "-x",
            "80",
            "-y",
            "24",
        ])
        .output()
        .context("Failed to create tmux session")?;
    if !output.status.success() {
        bail!(
            "Failed to create tmux session: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(())
}

/// Ensure the thurbox tmux session exists (headless — no control mode).
///
/// Used by [`spawn_window`] so callers that don't hold a
/// [`LocalTmuxBackend`] can still create sessions safely.
fn ensure_tmux_session_headless() -> Result<()> {
    if !tmux_session_exists()? {
        tmux_create_session()?;
    }
    // Apply options unconditionally — the TUI may have created the session
    // without them, and `set-option` is idempotent. In particular,
    // `remain-on-exit=on` is required so a failed claude process leaves its
    // tmux window visible with the error instead of silently vanishing.
    for (k, v) in HEADLESS_SESSION_OPTS {
        let _ = Command::new("tmux")
            .args(["-L", TMUX_SOCKET, "set-option", "-t", TMUX_SESSION, k, v])
            .status();
    }
    Ok(())
}

/// Spawn a new tmux window running `command` with `args` in `cwd`.
///
/// Thin helper for headless callers (CLI, MCP) that don't need PTY I/O
/// streams. Returns on success once the window exists; the command runs
/// inside it. Window name is `tb-<session_name>`.
pub fn spawn_window(
    session_name: &str,
    command: &str,
    args: &[String],
    cwd: Option<&Path>,
    env: &HashMap<String, String>,
) -> Result<()> {
    ensure_tmux_session_headless()?;

    let window_name = agent_window_name(session_name);
    let mut tmux = Command::new("tmux");
    tmux.args([
        "-L",
        TMUX_SOCKET,
        "new-window",
        "-d",
        "-t",
        &format!("{TMUX_SESSION}:"),
        "-n",
        &window_name,
    ]);
    if let Some(dir) = cwd {
        tmux.args(["-c", &dir.to_string_lossy()]);
    }
    for (k, v) in env {
        tmux.args(["-e", &format!("{k}={v}")]);
    }
    // Pass the command + args as a single argv list. tmux treats trailing args
    // as the command to run inside the window.
    tmux.arg(command);
    for a in args {
        tmux.arg(a);
    }

    let output = tmux
        .output()
        .context("Failed to run tmux new-window for headless spawn")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!(
            "tmux new-window exited {} for window {}: {}",
            output.status,
            window_name,
            stderr.trim()
        );
    }
    Ok(())
}

/// Kill the tmux window `tb-<session_name>` if it exists.
pub fn kill_window(session_name: &str) -> Result<()> {
    let target = window_target(session_name);
    let output = Command::new("tmux")
        .args(["-L", TMUX_SOCKET, "kill-window", "-t", &target])
        .output()
        .context("Failed to run tmux kill-window")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        // It's fine if the window is already gone.
        if stderr.contains("can't find window") || stderr.contains("window not found") {
            return Ok(());
        }
        bail!(
            "tmux kill-window exited {} for {}: {}",
            output.status,
            target,
            stderr.trim()
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::io::Read;

    use super::*;
    use crate::agent::control_mode::{
        decode_octal, format_send_keys, parse_notification, shell_escape,
    };

    // --- shell_escape tests (verify re-export works) ---

    #[test]
    fn shell_escape_empty() {
        assert_eq!(shell_escape(""), "''");
    }

    #[test]
    fn shell_escape_simple() {
        assert_eq!(shell_escape("hello"), "hello");
    }

    #[test]
    fn shell_escape_path() {
        assert_eq!(shell_escape("/home/user/repos/app"), "/home/user/repos/app");
    }

    #[test]
    fn shell_escape_with_spaces() {
        assert_eq!(shell_escape("hello world"), "'hello world'");
    }

    #[test]
    fn shell_escape_with_quotes() {
        assert_eq!(shell_escape("it's"), "'it'\\''s'");
    }

    #[test]
    fn shell_escape_flag_value() {
        assert_eq!(shell_escape("--permission-mode"), "--permission-mode");
    }

    #[test]
    fn shell_escape_tool_pattern() {
        assert_eq!(shell_escape("Read Bash(git:*)"), "'Read Bash(git:*)'");
    }

    // --- build_shell_command tests ---

    #[test]
    fn build_shell_command_simple() {
        let cmd = LocalTmuxBackend::build_shell_command("claude", &[]);
        assert_eq!(cmd, "claude");
    }

    #[test]
    fn build_shell_command_with_args() {
        let args = vec![
            "--resume".to_string(),
            "abc-123".to_string(),
            "--permission-mode".to_string(),
            "default".to_string(),
        ];
        let cmd = LocalTmuxBackend::build_shell_command("claude", &args);
        assert_eq!(cmd, "claude --resume abc-123 --permission-mode default");
    }

    #[test]
    fn build_shell_command_with_spaces_in_args() {
        let args = vec![
            "--allowed-tools".to_string(),
            "Read Bash(git:*)".to_string(),
        ];
        let cmd = LocalTmuxBackend::build_shell_command("claude", &args);
        assert_eq!(cmd, "claude --allowed-tools 'Read Bash(git:*)'");
    }

    // --- decode_octal tests (verify import works) ---

    #[test]
    fn decode_octal_esc() {
        assert_eq!(decode_octal("\\033"), vec![27]); // ESC
    }

    #[test]
    fn decode_octal_backslash() {
        assert_eq!(decode_octal("\\134"), vec![b'\\']);
    }

    #[test]
    fn decode_octal_newline() {
        assert_eq!(decode_octal("\\012"), vec![b'\n']);
    }

    #[test]
    fn decode_octal_passthrough() {
        assert_eq!(decode_octal("hello"), b"hello");
    }

    #[test]
    fn decode_octal_incomplete() {
        assert_eq!(decode_octal("\\01"), b"\\01");
    }

    #[test]
    fn decode_octal_non_octal_digits() {
        assert_eq!(decode_octal("\\089"), b"\\089");
    }

    #[test]
    fn decode_octal_mixed() {
        assert_eq!(
            decode_octal("A\\033[1mB"),
            vec![b'A', 27, b'[', b'1', b'm', b'B']
        );
    }

    #[test]
    fn decode_octal_consecutive() {
        assert_eq!(decode_octal("\\033\\033"), vec![27, 27]);
    }

    #[test]
    fn decode_octal_empty() {
        assert_eq!(decode_octal(""), b"");
    }

    #[test]
    fn decode_octal_trailing_backslash() {
        assert_eq!(decode_octal("a\\"), b"a\\");
    }

    #[test]
    fn decode_octal_max_value() {
        assert_eq!(decode_octal("\\377"), vec![0xFF]);
    }

    // --- parse_notification tests (verify import works) ---

    #[test]
    fn parse_output_notification() {
        let n = parse_notification("%output %42 hello\\033[1m");
        assert_eq!(
            n,
            Notification::Output {
                pane_id: "%42".to_string(),
                data: vec![b'h', b'e', b'l', b'l', b'o', 27, b'[', b'1', b'm'],
            }
        );
    }

    #[test]
    fn parse_extended_output_notification() {
        let n = parse_notification("%extended-output %2 0 : \\033[?2026hA\\033[?2026l");
        assert_eq!(
            n,
            Notification::Output {
                pane_id: "%2".to_string(),
                data: vec![
                    27, b'[', b'?', b'2', b'0', b'2', b'6', b'h', b'A', 27, b'[', b'?', b'2', b'0',
                    b'2', b'6', b'l'
                ],
            }
        );
    }

    #[test]
    fn parse_begin_notification() {
        assert_eq!(
            parse_notification("%begin 1234567890 7 0"),
            Notification::Begin
        );
    }

    #[test]
    fn parse_end_notification() {
        assert_eq!(parse_notification("%end 1234567890 7 0"), Notification::End);
    }

    #[test]
    fn parse_error_notification() {
        assert_eq!(
            parse_notification("%error 1234567890 3 0"),
            Notification::Error
        );
    }

    #[test]
    fn parse_pause_notification() {
        assert_eq!(
            parse_notification("%pause %42"),
            Notification::Pause {
                pane_id: "%42".to_string()
            }
        );
    }

    #[test]
    fn parse_other_notification() {
        assert_eq!(
            parse_notification("some random line"),
            Notification::Other("some random line".to_string())
        );
    }

    #[test]
    fn parse_output_no_data() {
        assert_eq!(
            parse_notification("%output %42"),
            Notification::Other("%output %42".to_string())
        );
    }

    #[test]
    fn parse_extended_output_no_colon_separator() {
        assert_eq!(
            parse_notification("%extended-output %2 0 data"),
            Notification::Other("%extended-output %2 0 data".to_string())
        );
    }

    #[test]
    fn parse_output_empty_data() {
        let n = parse_notification("%output %42 ");
        assert_eq!(
            n,
            Notification::Output {
                pane_id: "%42".to_string(),
                data: vec![],
            }
        );
    }

    // --- format_send_keys tests (verify import works) ---

    #[test]
    fn format_send_keys_single_byte() {
        assert_eq!(format_send_keys("%42", b"A"), "send-keys -t %42 -H 41\n");
    }

    #[test]
    fn format_send_keys_multi_byte() {
        assert_eq!(
            format_send_keys("%42", b"ABC"),
            "send-keys -t %42 -H 41 42 43\n"
        );
    }

    #[test]
    fn format_send_keys_empty() {
        assert_eq!(format_send_keys("%42", &[]), "send-keys -t %42 -H\n");
    }

    #[test]
    fn format_send_keys_escape_sequence() {
        assert_eq!(
            format_send_keys("%1", &[0x1b, b'[', b'A']),
            "send-keys -t %1 -H 1b 5b 41\n"
        );
    }

    // --- ControlModeReader tests (verify import works) ---

    #[test]
    fn control_mode_reader_data_delivery() {
        let (tx, rx) = std::sync::mpsc::sync_channel(16);
        let mut reader = ControlModeReader::new(rx);

        tx.send(b"hello".to_vec()).unwrap();
        let mut buf = [0u8; 16];
        let n = reader.read(&mut buf).unwrap();
        assert_eq!(&buf[..n], b"hello");
    }

    #[test]
    fn control_mode_reader_eof_on_sender_drop() {
        let (tx, rx) = std::sync::mpsc::sync_channel(16);
        let mut reader = ControlModeReader::new(rx);

        drop(tx);
        let mut buf = [0u8; 16];
        let n = reader.read(&mut buf).unwrap();
        assert_eq!(n, 0); // EOF
    }

    #[test]
    fn control_mode_reader_partial_reads() {
        let (tx, rx) = std::sync::mpsc::sync_channel(16);
        let mut reader = ControlModeReader::new(rx);

        tx.send(b"hello world".to_vec()).unwrap();

        let mut buf = [0u8; 5];
        let n = reader.read(&mut buf).unwrap();
        assert_eq!(&buf[..n], b"hello");

        let n = reader.read(&mut buf).unwrap();
        assert_eq!(&buf[..n], b" worl");

        let n = reader.read(&mut buf).unwrap();
        assert_eq!(&buf[..n], b"d");
    }

    #[test]
    fn control_mode_reader_multiple_sends() {
        let (tx, rx) = std::sync::mpsc::sync_channel(16);
        let mut reader = ControlModeReader::new(rx);

        tx.send(b"aaa".to_vec()).unwrap();
        tx.send(b"bbb".to_vec()).unwrap();

        let mut buf = [0u8; 16];
        let n = reader.read(&mut buf).unwrap();
        assert_eq!(&buf[..n], b"aaa");

        let n = reader.read(&mut buf).unwrap();
        assert_eq!(&buf[..n], b"bbb");
    }

    #[test]
    fn control_mode_writer_is_send() {
        fn assert_send<T: Send>() {}
        assert_send::<ControlModeWriter>();
    }

    #[test]
    fn control_mode_reader_is_send() {
        fn assert_send<T: Send>() {}
        assert_send::<ControlModeReader>();
    }

    #[test]
    fn backend_default_has_no_control_mode() {
        let backend = LocalTmuxBackend::new();
        let guard = backend.control.lock().unwrap();
        assert!(guard.is_none());
    }

    #[test]
    fn control_mode_reader_exact_size_buffer() {
        let (tx, rx) = std::sync::mpsc::sync_channel(16);
        let mut reader = ControlModeReader::new(rx);

        tx.send(b"abc".to_vec()).unwrap();
        let mut buf = [0u8; 3];
        let n = reader.read(&mut buf).unwrap();
        assert_eq!(n, 3);
        assert_eq!(&buf[..n], b"abc");
    }

    #[test]
    fn try_send_drops_when_channel_full() {
        let (tx, _rx) = std::sync::mpsc::sync_channel::<Vec<u8>>(1);

        tx.send(b"first".to_vec()).unwrap();

        match tx.try_send(b"second".to_vec()) {
            Err(std::sync::mpsc::TrySendError::Full(_)) => {} // expected
            other => panic!("Expected TrySendError::Full, got: {other:?}"),
        }
    }

    // Compile-time check: channel capacity must be large enough to buffer heavy output.
    const _: () = assert!(PANE_CHANNEL_CAPACITY >= 1024);

    #[test]
    fn parse_pause_notification_with_leading_percent() {
        assert_eq!(
            parse_notification("%pause %123"),
            Notification::Pause {
                pane_id: "%123".to_string()
            }
        );
    }

    #[test]
    fn shell_escape_allows_equals_comma() {
        assert_eq!(shell_escape("key=val,other"), "key=val,other");
    }

    #[test]
    fn env_flag_simple_value() {
        // Simple key=value should not be quoted.
        let env_part: String = [("RUST_LOG".to_string(), "debug".to_string())]
            .into_iter()
            .collect::<std::collections::HashMap<_, _>>()
            .iter()
            .map(|(k, v)| format!(" -e {}", shell_escape(&format!("{k}={v}"))))
            .collect();
        assert_eq!(env_part, " -e RUST_LOG=debug");
    }

    #[test]
    fn env_flag_value_with_spaces() {
        // Values with spaces must be quoted as a single KEY=VALUE unit.
        let env_part: String = [("MSG".to_string(), "hello world".to_string())]
            .into_iter()
            .collect::<std::collections::HashMap<_, _>>()
            .iter()
            .map(|(k, v)| format!(" -e {}", shell_escape(&format!("{k}={v}"))))
            .collect();
        assert_eq!(env_part, " -e 'MSG=hello world'");
    }

    #[test]
    fn decode_octal_overflow_wraps() {
        assert_eq!(decode_octal("\\400"), vec![0u8]);
    }

    #[test]
    fn parse_extended_output_missing_pane_space() {
        assert_eq!(
            parse_notification("%extended-output %2 : data"),
            Notification::Other("%extended-output %2 : data".to_string())
        );
    }

    // --- window-name sanitization tests ---

    #[test]
    fn sanitize_window_name_passes_through_safe_chars() {
        assert_eq!(sanitize_window_name("abc-123_XYZ"), "abc-123_XYZ");
    }

    #[test]
    fn sanitize_window_name_replaces_spaces() {
        // Bug: session names with spaces broke `tmux send-keys` / capture
        // because the target string `session:window with spaces` was
        // re-split by tmux into `session`, `window`, `with`, `spaces`.
        assert_eq!(sanitize_window_name("Foo Bar"), "Foo_Bar");
    }

    #[test]
    fn sanitize_window_name_replaces_tmux_delimiters() {
        // Colons, dots, commas all have meaning inside tmux target strings.
        assert_eq!(sanitize_window_name("a:b.c,d"), "a_b_c_d");
    }

    #[test]
    fn sanitize_window_name_replaces_non_ascii() {
        assert_eq!(sanitize_window_name("café"), "caf_");
    }

    #[test]
    fn agent_and_shell_window_names_share_sanitization() {
        assert_eq!(agent_window_name("Foo Bar"), "tb-Foo_Bar");
        assert_eq!(shell_window_name("Foo Bar"), "tbs-Foo_Bar");
    }
}
