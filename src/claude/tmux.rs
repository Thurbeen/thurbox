use std::collections::{HashMap, VecDeque};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{sync_channel, SyncSender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use anyhow::{bail, Context, Result};
use tracing::{debug, warn};

use crate::claude::backend::{AdoptedSession, DiscoveredSession, SessionBackend, SpawnedSession};

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

/// Minimum tmux version required.
const MIN_TMUX_VERSION: (u32, u32) = (3, 2);

/// Per-pane output channel capacity. Sized large enough to buffer heavy output
/// bursts; chunks are dropped (not blocked) when full to keep the reader thread alive.
const PANE_CHANNEL_CAPACITY: usize = 4096;

/// Timeout for waiting for a control mode command response.
const COMMAND_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// Type alias for pane sender broadcast map.
/// Maps pane IDs to vectors of sync senders for multi-instance output broadcast.
type PaneSendersMap = HashMap<String, Vec<SyncSender<Vec<u8>>>>;
type PaneSendersMapShared = Arc<Mutex<PaneSendersMap>>;

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

struct CommandResponse {
    lines: Vec<String>,
    is_error: bool,
}

/// Parsed notification from the tmux control mode protocol.
#[derive(Debug, PartialEq)]
enum Notification {
    Output { pane_id: String, data: Vec<u8> },
    Begin,
    End,
    Error,
    Pause { pane_id: String },
    Other(String),
}

/// Per-pane reader that receives output via an mpsc channel.
///
/// Implements `Read` so it plugs directly into the existing `Session::reader_loop`.
struct ControlModeReader {
    receiver: std::sync::mpsc::Receiver<Vec<u8>>,
    buffer: Vec<u8>,
    pos: usize,
}

impl ControlModeReader {
    fn new(receiver: std::sync::mpsc::Receiver<Vec<u8>>) -> Self {
        Self {
            receiver,
            buffer: Vec::new(),
            pos: 0,
        }
    }
}

impl Read for ControlModeReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        // Drain leftover buffered data first.
        if self.pos < self.buffer.len() {
            let remaining = &self.buffer[self.pos..];
            let n = remaining.len().min(buf.len());
            buf[..n].copy_from_slice(&remaining[..n]);
            self.pos += n;
            if self.pos == self.buffer.len() {
                self.buffer.clear();
                self.pos = 0;
            }
            return Ok(n);
        }

        // Block until the next chunk arrives.
        match self.receiver.recv() {
            Ok(data) => {
                let n = data.len().min(buf.len());
                buf[..n].copy_from_slice(&data[..n]);
                if n < data.len() {
                    self.buffer = data;
                    self.pos = n;
                }
                Ok(n)
            }
            Err(_) => Ok(0), // Channel closed → EOF.
        }
    }
}

/// Per-pane writer that sends input via `send-keys -H` through the shared control stdin.
struct ControlModeWriter {
    stdin: Arc<Mutex<ChildStdin>>,
    pane_id: String,
}

impl Write for ControlModeWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        let cmd = format_send_keys(&self.pane_id, buf);
        let mut stdin = self
            .stdin
            .lock()
            .map_err(|e| std::io::Error::other(format!("stdin lock: {e}")))?;
        stdin.write_all(cmd.as_bytes())?;
        stdin.flush()?;
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
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
        let pane_senders: PaneSendersMapShared = Arc::new(Mutex::new(HashMap::new()));
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

            match parse_notification(&line) {
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

        // Join the reader thread.
        if let Ok(mut handle) = self.reader_handle.lock() {
            if let Some(h) = handle.take() {
                let _ = h.join();
            }
        }

        // Ensure the child process is cleaned up.
        if let Ok(mut child) = self.child.lock() {
            let _ = child.wait();
        }
    }
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
            parts.push(shell_escape(arg));
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

    /// Send a command via control mode and return the response.
    fn ctrl_command(&self, cmd: &str) -> Result<String> {
        let guard = self.control()?;
        guard.as_ref().unwrap().send_command(cmd)
    }

    /// Send a command via control mode without waiting for a response.
    fn ctrl_command_nowait(&self, cmd: &str) -> Result<()> {
        let guard = self.control()?;
        guard.as_ref().unwrap().send_command_nowait(cmd)
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

    /// Capture the full screen content (including scrollback) from a pane.
    ///
    /// `-S -` starts from the beginning of scrollback history,
    /// `-e` includes ANSI escape sequences (colors, bold, etc.),
    /// `-p` prints to stdout (captured via control mode response).
    fn capture_pane(&self, pane_id: &str) -> Vec<u8> {
        let cmd = format!("capture-pane -t {pane_id} -p -e -S -");
        let content = self.ctrl_command(&cmd).unwrap_or_default();
        debug!(
            pane_id = %pane_id,
            bytes = content.len(),
            lines = content.lines().count(),
            "capture-pane result"
        );
        content.into_bytes()
    }

    /// Connect I/O to an existing pane: start monitoring, capture screen,
    /// resize (triggers app redraw), and create writer.
    fn connect_pane(&self, pane_id: &str, rows: u16, cols: u16) -> Result<AdoptedSession> {
        let reader = self.register_pane(pane_id)?;
        // Must use send_command (waited) here — a nowait call would leave an
        // unclaimed %begin/%end response in the stream that steals the next
        // send_command waiter (e.g., capture-pane below).
        self.ctrl_command(&format!(
            "refresh-client -A '{}:on'",
            pane_id.replace('\'', "'\\''")
        ))?;
        // Capture the current screen as a quick initial approximation (colors
        // and backgrounds may be approximate since capture-pane -p renders text
        // rather than replaying the original escape sequences).
        let initial_screen = self.capture_pane(pane_id);
        let writer = self.pane_writer(pane_id)?;

        // Resize to the target dimensions. If the pane was already at this
        // size, force a SIGWINCH by briefly shrinking by one row and resizing
        // back. This makes TUI applications (like claude) repaint their full
        // screen through the normal output stream, which the reader_loop
        // processes with all escape sequences intact.
        self.force_resize(pane_id, rows, cols)?;

        Ok(AdoptedSession {
            output: Box::new(reader),
            input: Box::new(writer),
            initial_screen,
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
        rows: u16,
        cols: u16,
    ) -> Result<SpawnedSession> {
        let shell_cmd = Self::build_shell_command(command, args);

        let cwd_part = match cwd {
            Some(dir) => format!(" -c {}", shell_escape(&dir.to_string_lossy())),
            None => String::new(),
        };
        let cmd = format!(
            "new-window -t {TMUX_SESSION} -n {window_name} -P -F '#{{pane_id}}'{cwd_part} {shell_cmd}"
        );
        let result = self.ctrl_command(&cmd)?;
        let pane_id = result.trim().to_string();

        debug!(pane_id = %pane_id, "tmux window created via control mode");

        let connected = self.connect_pane(&pane_id, rows, cols)?;

        Ok(SpawnedSession {
            backend_id: pane_id,
            output: connected.output,
            input: connected.input,
            initial_screen: connected.initial_screen,
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
            // Only discover windows with our prefix.
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
}

/// Decode tmux control mode octal escapes in `%output` data.
///
/// Scans for `\` followed by exactly 3 octal digits (0-7). Emits the decoded byte.
/// All other characters pass through unchanged.
fn decode_octal(input: &str) -> Vec<u8> {
    let mut result = Vec::with_capacity(input.len());
    let bytes = input.as_bytes();
    let mut i = 0;

    while i < bytes.len() {
        if bytes[i] == b'\\' && i + 3 < bytes.len() {
            let d0 = bytes[i + 1];
            let d1 = bytes[i + 2];
            let d2 = bytes[i + 3];
            if is_octal(d0) && is_octal(d1) && is_octal(d2) {
                let val = (d0 - b'0') as u16 * 64 + (d1 - b'0') as u16 * 8 + (d2 - b'0') as u16;
                result.push(val as u8);
                i += 4;
                continue;
            }
        }
        result.push(bytes[i]);
        i += 1;
    }

    result
}

fn is_octal(b: u8) -> bool {
    (b'0'..=b'7').contains(&b)
}

/// Parse a line from tmux control mode into a notification.
fn parse_notification(line: &str) -> Notification {
    if let Some(rest) = line.strip_prefix("%output ") {
        // Format: %output %<pane_id> <octal-encoded data>
        if let Some(space_idx) = rest.find(' ') {
            let pane_id = rest[..space_idx].to_string();
            let data = decode_octal(&rest[space_idx + 1..]);
            return Notification::Output { pane_id, data };
        }
    }

    if let Some(rest) = line.strip_prefix("%extended-output ") {
        // Format: %extended-output %<pane_id> <age> : <octal-encoded data>
        // The " : " separator divides metadata from payload.
        if let Some(colon_idx) = rest.find(" : ") {
            let meta = &rest[..colon_idx];
            let data = decode_octal(&rest[colon_idx + 3..]);
            // meta is "%<pane_id> <age>" — extract pane_id.
            if let Some(space_idx) = meta.find(' ') {
                let pane_id = meta[..space_idx].to_string();
                return Notification::Output { pane_id, data };
            }
        }
    }

    if line.starts_with("%begin ") {
        return Notification::Begin;
    }

    if line.starts_with("%end ") {
        return Notification::End;
    }

    if line.starts_with("%error ") {
        return Notification::Error;
    }

    if let Some(rest) = line.strip_prefix("%pause ") {
        // Format: %pause %<pane_id>
        return Notification::Pause {
            pane_id: rest.trim().to_string(),
        };
    }

    Notification::Other(line.to_string())
}

/// Format a `send-keys -H` command for a pane.
///
/// Each byte is encoded as two hex digits.
fn format_send_keys(pane_id: &str, bytes: &[u8]) -> String {
    use std::fmt::Write;
    // "send-keys -t %NN -H" + " XX" per byte + "\n"
    let mut cmd = String::with_capacity(20 + pane_id.len() + bytes.len() * 3 + 1);
    write!(cmd, "send-keys -t {pane_id} -H").unwrap();
    for &b in bytes {
        write!(cmd, " {b:02x}").unwrap();
    }
    cmd.push('\n');
    cmd
}

/// Shell-escape a string for safe inclusion in a tmux command.
fn shell_escape(s: &str) -> String {
    if s.is_empty() {
        return "''".to_string();
    }
    // If the string contains no special characters, return as-is.
    if s.chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '/' | ':' | '=' | ','))
    {
        return s.to_string();
    }
    // Wrap in single quotes, escaping existing single quotes.
    format!("'{}'", s.replace('\'', "'\\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- shell_escape tests ---

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
            "dontAsk".to_string(),
        ];
        let cmd = LocalTmuxBackend::build_shell_command("claude", &args);
        assert_eq!(cmd, "claude --resume abc-123 --permission-mode dontAsk");
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

    // --- decode_octal tests ---

    #[test]
    fn decode_octal_esc() {
        assert_eq!(decode_octal("\\033"), vec![27]); // ESC
    }

    #[test]
    fn decode_octal_backslash() {
        // A literal backslash is \134 in octal.
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
        // Backslash followed by fewer than 3 digits passes through.
        assert_eq!(decode_octal("\\01"), b"\\01");
    }

    #[test]
    fn decode_octal_non_octal_digits() {
        // \089 — 8 and 9 are not octal digits.
        assert_eq!(decode_octal("\\089"), b"\\089");
    }

    #[test]
    fn decode_octal_mixed() {
        // "A\033[1mB" → A, ESC, [, 1, m, B
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
        // Backslash at end of string (no digits follow).
        assert_eq!(decode_octal("a\\"), b"a\\");
    }

    #[test]
    fn decode_octal_max_value() {
        // \377 = 255 = 0xFF — maximum single-byte octal value.
        assert_eq!(decode_octal("\\377"), vec![0xFF]);
    }

    // --- parse_notification tests ---

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
        // %output with pane_id but no trailing space/data → falls through to Other.
        assert_eq!(
            parse_notification("%output %42"),
            Notification::Other("%output %42".to_string())
        );
    }

    #[test]
    fn parse_extended_output_no_colon_separator() {
        // %extended-output without " : " separator → falls through to Other.
        assert_eq!(
            parse_notification("%extended-output %2 0 data"),
            Notification::Other("%extended-output %2 0 data".to_string())
        );
    }

    #[test]
    fn parse_output_empty_data() {
        // %output with pane_id and trailing space but no data bytes.
        let n = parse_notification("%output %42 ");
        assert_eq!(
            n,
            Notification::Output {
                pane_id: "%42".to_string(),
                data: vec![],
            }
        );
    }

    // --- format_send_keys tests ---

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
        // ESC [ A (up arrow)
        assert_eq!(
            format_send_keys("%1", &[0x1b, b'[', b'A']),
            "send-keys -t %1 -H 1b 5b 41\n"
        );
    }

    // --- ControlModeReader tests ---

    #[test]
    fn control_mode_reader_data_delivery() {
        let (tx, rx) = sync_channel(16);
        let mut reader = ControlModeReader::new(rx);

        tx.send(b"hello".to_vec()).unwrap();
        let mut buf = [0u8; 16];
        let n = reader.read(&mut buf).unwrap();
        assert_eq!(&buf[..n], b"hello");
    }

    #[test]
    fn control_mode_reader_eof_on_sender_drop() {
        let (tx, rx) = sync_channel(16);
        let mut reader = ControlModeReader::new(rx);

        drop(tx);
        let mut buf = [0u8; 16];
        let n = reader.read(&mut buf).unwrap();
        assert_eq!(n, 0); // EOF
    }

    #[test]
    fn control_mode_reader_partial_reads() {
        let (tx, rx) = sync_channel(16);
        let mut reader = ControlModeReader::new(rx);

        tx.send(b"hello world".to_vec()).unwrap();

        // Read in small chunks.
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
        let (tx, rx) = sync_channel(16);
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
        let (tx, rx) = sync_channel(16);
        let mut reader = ControlModeReader::new(rx);

        tx.send(b"abc".to_vec()).unwrap();
        // Buffer exactly matches data size — no leftover.
        let mut buf = [0u8; 3];
        let n = reader.read(&mut buf).unwrap();
        assert_eq!(n, 3);
        assert_eq!(&buf[..n], b"abc");
    }

    #[test]
    fn try_send_drops_when_channel_full() {
        // Verify the try_send pattern used in reader_thread:
        // when channel is full, data is dropped without blocking.
        let (tx, _rx) = sync_channel::<Vec<u8>>(1);

        // Fill the channel.
        tx.send(b"first".to_vec()).unwrap();

        // Second send should fail (Full), not block.
        match tx.try_send(b"second".to_vec()) {
            Err(std::sync::mpsc::TrySendError::Full(_)) => {} // expected
            other => panic!("Expected TrySendError::Full, got: {other:?}"),
        }
    }

    // Compile-time check: channel capacity must be large enough to buffer heavy output.
    const _: () = assert!(PANE_CHANNEL_CAPACITY >= 1024);

    #[test]
    fn parse_pause_notification_with_leading_percent() {
        // Pane IDs from tmux always start with %.
        assert_eq!(
            parse_notification("%pause %123"),
            Notification::Pause {
                pane_id: "%123".to_string()
            }
        );
    }

    #[test]
    fn shell_escape_allows_equals_comma() {
        // Equals and comma are safe characters.
        assert_eq!(shell_escape("key=val,other"), "key=val,other");
    }

    #[test]
    fn decode_octal_overflow_wraps() {
        // \400 = 256, which wraps to 0 as u8. In practice tmux only
        // produces 0-377 (0-255), so this documents the truncation behavior.
        assert_eq!(decode_octal("\\400"), vec![0u8]);
    }

    #[test]
    fn parse_extended_output_missing_pane_space() {
        // %extended-output with " : " but no space in metadata falls through.
        assert_eq!(
            parse_notification("%extended-output %2 : data"),
            Notification::Other("%extended-output %2 : data".to_string())
        );
    }
}
