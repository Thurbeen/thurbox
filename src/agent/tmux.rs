use std::collections::{HashMap, VecDeque};
use std::io::{BufRead, BufReader, Cursor, Read, Write};
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
use crate::agent::transport::{TmuxTransport, DEFAULT_MUX};

/// Dedicated tmux socket name — isolates thurbox sessions from the user's tmux.
/// Dev builds use "thurbox-dev" to avoid interfering with an installed release binary.
const TMUX_SOCKET: &str = if cfg!(dev_build) {
    "thurbox-dev"
} else {
    "thurbox"
};

/// Env var overriding the **local** multiplexer socket name.
///
/// Unix test/sandbox tooling scopes the socket by pointing `TMUX_TMPDIR` at a
/// private directory, but psmux (native Windows) has no socket-directory
/// concept — every `-L <name>` resolves machine-wide, so without this override
/// a scoped test on Windows would share (and could tear down) the user's real
/// `thurbox`/`thurbox-dev` server. Remote hosts are unaffected (their socket
/// comes from `hosts.toml`).
pub const SOCKET_OVERRIDE_ENV: &str = "THURBOX_SOCKET";

/// The local multiplexer socket name: [`SOCKET_OVERRIDE_ENV`] when set and
/// non-empty, else the compile-time default.
fn local_socket() -> String {
    std::env::var(SOCKET_OVERRIDE_ENV)
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| TMUX_SOCKET.to_string())
}

/// tmux session name used to group all thurbox windows.
/// Dev builds use "thurbox-dev" to avoid interfering with an installed release binary.
const TMUX_SESSION: &str = if cfg!(dev_build) {
    "thurbox-dev"
} else {
    "thurbox"
};

/// Build a [`Command`] for the local multiplexer on the thurbox socket:
/// `<DEFAULT_MUX> -L <TMUX_SOCKET> <args…>`. The headless one-shot helpers below
/// (send/capture/spawn/kill/heartbeat) bypass the [`TmuxTransport`] seam — they
/// are local-only — so this centralizes the binary name (`tmux`, or `psmux` on
/// Windows) and socket instead of hardcoding `tmux` at each call site.
fn local_mux_command(args: &[&str]) -> Command {
    let mut cmd = Command::new(DEFAULT_MUX);
    cmd.arg("-L").arg(local_socket()).args(args);
    // Strip nesting env so these one-shots target thurbox's own socket even when
    // thurbox is launched inside a tmux/psmux pane (see `strip_mux_nesting_env`).
    crate::agent::transport::strip_mux_nesting_env(&mut cmd);
    cmd
}

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

/// Build the `session:=window` tmux target for a thurbox agent session.
///
/// The `=` prefix forces tmux to match the window name exactly. Without
/// it tmux falls back to FNMATCH-style prefix matching, so a target of
/// `tb-foo` would resolve ambiguously when both `tb-foo` and
/// `tb-foo-bar` exist — `send-keys`/`capture-pane` then fails with
/// "ambiguous window" and the caller's text is silently dropped.
fn window_target(session_name: &str) -> String {
    format!("{TMUX_SESSION}:={}", agent_window_name(session_name))
}

/// Minimum tmux version required.
const MIN_TMUX_VERSION: (u32, u32) = (3, 2);

/// Parse a `tmux -V` version string (e.g. `"tmux 3.4"`, `"tmux 3.3a"`) into a
/// `(major, minor)` pair. Shared by the local and remote backends.
fn parse_tmux_version(version_str: &str) -> Result<(u32, u32)> {
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

    Ok((major, minor))
}

/// Enforce the minimum-version gate against a multiplexer's `-V` output.
///
/// The `>= 3.2` floor only applies to **real tmux** (a `tmux …` banner). A
/// drop-in clone like psmux numbers itself independently and may print a
/// different banner, so once it has answered `-V` it is accepted as-is — it
/// implements the control-mode feature set regardless of its own number.
fn check_min_version(version_output: &str) -> Result<()> {
    let trimmed = version_output.trim();
    if let Some(rest) = trimmed.strip_prefix("tmux ") {
        let (major, minor) = parse_tmux_version(rest)?;
        if (major, minor) < MIN_TMUX_VERSION {
            bail!(
                "tmux {major}.{minor} is too old; thurbox requires >= {}.{}",
                MIN_TMUX_VERSION.0,
                MIN_TMUX_VERSION.1
            );
        }
    }
    Ok(())
}

/// Timeout for waiting for a control mode command response.
const COMMAND_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// Delay between sending command text and pressing Enter via tmux, used by the
/// synchronous `send_prompt_now` path.
const SEND_KEYS_ENTER_DELAY: std::time::Duration = std::time::Duration::from_millis(200);

/// Hard cap on the number of scrollback lines `capture_pane_text` will return.
const MAX_CAPTURE_LINES: u32 = 10_000;

/// A tmux backend — sessions persist in `tmux -L <socket>` on either the local
/// machine or a remote host reached over SSH.
///
/// Uses tmux control mode (`-C`) for all I/O after `ensure_ready()`. The only
/// thing that differs between local and remote is the [`TmuxTransport`] used to
/// launch the `tmux` process; the protocol layer is identical.
pub struct TmuxBackend {
    /// How `tmux` is launched (local `Command` vs `ssh <dest> tmux …`).
    transport: TmuxTransport,
    /// tmux socket name passed via `-L` (e.g. `thurbox`).
    socket: String,
    /// tmux session name grouping all thurbox windows.
    session: String,
    /// Backend name used by the registry / persisted `backend_type`
    /// (`local-tmux` or `ssh:<host>`).
    name: String,
    control: Mutex<Option<ControlMode>>,
}

/// The local tmux backend. Thin alias-constructor over [`TmuxBackend`] kept for
/// existing call sites; `LocalTmuxBackend::new()` builds a local-transport backend.
pub type LocalTmuxBackend = TmuxBackend;

impl Default for TmuxBackend {
    fn default() -> Self {
        Self::local()
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
    /// `(pane_id, state)` pairs from `%subscription-changed` notifications
    /// (remote hook status — see [`crate::session::REMOTE_HOOK_STATE_OPTION`]),
    /// pushed by the reader thread and drained by the app tick via
    /// [`Self::take_sub_events`]. Bounded (drop-oldest): a short-lived
    /// connection (e.g. a headless spawn's) has no drainer.
    sub_events: Arc<Mutex<VecDeque<(String, String)>>>,
    reader_handle: Mutex<Option<JoinHandle<()>>>,
    child: Mutex<Child>,
}

/// Cap on queued subscription events. Status transitions are rare and the
/// queue is drained every TUI tick — the cap only guards an undrained
/// connection against unbounded growth.
const SUB_EVENTS_CAP: usize = 256;

impl ControlMode {
    /// Start a control mode connection to the thurbox tmux session over the
    /// given transport (local or ssh).
    fn start(transport: &TmuxTransport, socket: &str, session: &str) -> Result<Self> {
        // -C (single C): control mode with echo — works with piped stdin.
        // -CC (double C) requires a TTY and fails with "tcgetattr: Inappropriate ioctl".
        let mut child = transport
            .tmux_command(socket, &["-C", "attach-session", "-t", session])
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
        let sub_events: Arc<Mutex<VecDeque<(String, String)>>> =
            Arc::new(Mutex::new(VecDeque::new()));

        let reader_stdin = Arc::clone(&stdin);
        let reader_pane_senders = Arc::clone(&pane_senders);
        let reader_queue = Arc::clone(&response_queue);
        let reader_sub_events = Arc::clone(&sub_events);

        let reader_handle = std::thread::Builder::new()
            .name("tmux-control-reader".into())
            .spawn(move || {
                Self::reader_thread(
                    stdout,
                    reader_stdin,
                    reader_pane_senders,
                    reader_queue,
                    reader_sub_events,
                );
            })
            .context("Failed to spawn control reader thread")?;

        let control = Self {
            stdin,
            pane_senders,
            response_queue,
            sub_events,
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

        // Subscribe to the remote-hook status option of every pane of the
        // attached session (tmux pushes `%subscription-changed` on change) —
        // how an off-local agent's hooks reach the local status derivation.
        // tmux-only (psmux has no format subscriptions) and best-effort: a
        // refusal must not brick the whole backend, status just stays dark.
        // Armed here — not per pane — so `reconnect_control` re-arms for free
        // and panes created later are covered (`%*` is session-scoped).
        if !transport.uses_psmux() {
            let arm = format!(
                "refresh-client -B '{}:%*:#{{{}}}'",
                crate::session::REMOTE_HOOK_SUBSCRIPTION,
                crate::session::REMOTE_HOOK_STATE_OPTION,
            );
            if let Err(e) = control.send_command(&arm) {
                warn!("failed to arm the remote-hook status subscription: {e:#}");
            }
        }

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
        sub_events: Arc<Mutex<VecDeque<(String, String)>>>,
    ) {
        let mut reader = BufReader::new(stdout);
        // Accumulates response lines for the current in-flight command.
        let mut collecting: Option<Vec<String>> = None;
        let mut line_buf = Vec::new();

        loop {
            line_buf.clear();
            match reader.read_until(b'\n', &mut line_buf) {
                Ok(0) => break,
                Ok(_) => {}
                Err(e) => {
                    debug!("Control reader I/O error: {e}");
                    break;
                }
            }
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
                    Self::dispatch_output(&pane_senders, &pane_id, data);
                }
                Notification::Begin => {
                    collecting = Some(Vec::new());
                }
                end_or_error @ (Notification::End | Notification::Error) => {
                    let lines = collecting.take().unwrap_or_default();
                    let is_error = matches!(end_or_error, Notification::Error);
                    Self::deliver_response(&response_queue, lines, is_error);
                }
                Notification::Pause { pane_id } => {
                    Self::resume_pane(&stdin, &pane_id);
                }
                // Consumed even mid-%begin block: tmux never interleaves
                // notifications inside response bodies, so this can't eat a
                // response line. Empty value = the pane option is unset.
                Notification::SubscriptionChanged {
                    name,
                    pane_id,
                    value,
                } => {
                    if name == crate::session::REMOTE_HOOK_SUBSCRIPTION && !value.is_empty() {
                        if let Ok(mut events) = sub_events.lock() {
                            if events.len() >= SUB_EVENTS_CAP {
                                events.pop_front();
                            }
                            events.push_back((pane_id, value));
                        }
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

    /// Broadcast a `%output` payload to every reader registered for `pane_id`.
    ///
    /// Uses `try_send` so the reader thread never blocks: a full channel drops
    /// the chunk rather than stalling (which would deadlock `%pause` handling).
    fn dispatch_output(pane_senders: &PaneSendersMapShared, pane_id: &str, mut data: Vec<u8>) {
        let Ok(senders) = pane_senders.lock() else {
            return;
        };
        let Some(tx_vec) = senders.get(pane_id) else {
            return;
        };
        // Single-sender is the dominant case (one reader per pane): move `data`
        // into it instead of cloning. Only fan-out (multiple registered
        // instances) pays for a clone.
        for (i, tx) in tx_vec.iter().enumerate() {
            let chunk = if i + 1 == tx_vec.len() {
                std::mem::take(&mut data)
            } else {
                data.clone()
            };
            match tx.try_send(chunk) {
                Ok(()) => {}
                Err(std::sync::mpsc::TrySendError::Full(_dropped)) => {
                    debug!(pane_id = %pane_id, "Pane output channel full, dropping chunk");
                }
                Err(std::sync::mpsc::TrySendError::Disconnected(_)) => {}
            }
        }
    }

    /// Deliver a completed `%begin`/`%end`(`%error`) block to the next waiter.
    ///
    /// Responses with no waiter in the queue (e.g. from `send_command_nowait`)
    /// are simply discarded.
    fn deliver_response(
        response_queue: &Arc<Mutex<VecDeque<SyncSender<CommandResponse>>>>,
        lines: Vec<String>,
        is_error: bool,
    ) {
        if let Ok(mut queue) = response_queue.lock() {
            if let Some(tx) = queue.pop_front() {
                let _ = tx.send(CommandResponse { lines, is_error });
            }
        }
    }

    /// Drain the queued `(pane_id, state)` remote-hook status events.
    fn take_sub_events(&self) -> Vec<(String, String)> {
        self.sub_events
            .lock()
            .map(|mut events| events.drain(..).collect())
            .unwrap_or_default()
    }

    /// Respond to a `%pause` by asking tmux to resume output for the pane.
    fn resume_pane(stdin: &Arc<Mutex<ChildStdin>>, pane_id: &str) {
        let cmd = format!(
            "refresh-client -A '{}:continue'\n",
            pane_id.replace('\'', "'\\''")
        );
        if let Ok(mut s) = stdin.lock() {
            let _ = s.write_all(cmd.as_bytes());
            let _ = s.flush();
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

impl TmuxBackend {
    /// Build the local tmux backend (`tmux -L thurbox`).
    pub fn new() -> Self {
        Self::local()
    }

    /// Build the local tmux backend, named `local-tmux`.
    pub fn local() -> Self {
        Self {
            transport: TmuxTransport::Local,
            socket: local_socket(),
            session: TMUX_SESSION.to_string(),
            name: "local-tmux".to_string(),
            control: Mutex::new(None),
        }
    }

    /// Build a tmux backend over an explicit transport (used by the SSH backend).
    pub fn with_transport(
        transport: TmuxTransport,
        socket: impl Into<String>,
        session: impl Into<String>,
        name: impl Into<String>,
    ) -> Self {
        Self {
            transport,
            socket: socket.into(),
            session: session.into(),
            name: name.into(),
            control: Mutex::new(None),
        }
    }

    /// Build an off-local tmux backend for `host` — `tmux` over SSH for an SSH
    /// host, or `tmux` inside a WSL distro via `wsl.exe`. The backend is named
    /// `ssh:<host.name>` / `wsl:<host.name>` and uses the same socket/session
    /// names as the local backend unless the host overrides them.
    pub fn from_host(host: &crate::session::HostDef) -> Self {
        let socket = host
            .socket
            .clone()
            .unwrap_or_else(|| TMUX_SOCKET.to_string());
        let session = host
            .session
            .clone()
            .unwrap_or_else(|| TMUX_SESSION.to_string());
        let transport = if host.is_wsl() {
            TmuxTransport::Wsl {
                distro: host.distro_name(),
                mux: host.mux(),
            }
        } else {
            TmuxTransport::Ssh {
                destination: host.destination.clone(),
                ssh_opts: host.ssh_opts.clone(),
                mux: host.mux(),
            }
        };
        Self::with_transport(transport, socket, session, host.backend_name())
    }

    /// Run a tmux command and return its stdout (used before control mode is available).
    fn tmux_output(&self, args: &[&str]) -> Result<String> {
        let output = self.run_tmux(args)?;
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    }

    /// Run a tmux command, returning Ok(()) on success (used before control mode is available).
    fn tmux_run(&self, args: &[&str]) -> Result<()> {
        self.run_tmux(args)?;
        Ok(())
    }

    /// Execute a tmux command on the thurbox socket and check for errors.
    fn run_tmux(&self, args: &[&str]) -> Result<std::process::Output> {
        let output = self
            .transport
            .tmux_command(&self.socket, args)
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
        self.tmux_run(&["has-session", "-t", &self.session]).is_ok()
    }

    /// Apply server + session config to the tmux session.
    ///
    /// Idempotent (`set-option` overwrites), so it is safe to call on every
    /// [`ensure_ready`](Self::ensure_ready) — the session may have been created
    /// elsewhere (e.g. a headless spawn) without these options, and re-applying
    /// is the single source of truth for both the TUI and headless paths.
    fn apply_session_config(&self) -> Result<()> {
        // Use a non-login shell so that macOS path_helper (/etc/zprofile)
        // doesn't clobber PATH additions from ~/.zshenv (e.g. cargo, asdf).
        // For a remote backend the local `$SHELL` path may not exist on the
        // remote host, so fall back to a POSIX shell there.
        //
        // On Windows (psmux) we deliberately do NOT pin `default-command`: the
        // local `$SHELL`/`/bin/sh` don't exist, and forcing a Windows shell here
        // would have to match psmux's own command-execution model. Letting psmux
        // use its native ConPTY default shell is the safe choice.
        #[cfg(not(windows))]
        {
            let shell = self.config_shell();
            self.tmux_run(&["set-option", "-s", "default-command", &shell])?;
        }

        // Server-wide options
        let server_opts = [
            ("default-terminal", "xterm-256color"),
            ("extended-keys", "on"),
        ];
        for (key, val) in &server_opts {
            self.tmux_run(&["set-option", "-s", key, val])?;
        }

        // Session-level options
        for (key, val) in SESSION_OPTS {
            self.tmux_run(&["set-option", "-t", &self.session, key, val])?;
        }

        Ok(())
    }

    /// Ensure the thurbox tmux session exists and its options are applied,
    /// **without** starting control mode.
    ///
    /// Shared by [`ensure_ready`](Self::ensure_ready) (which then starts control
    /// mode) and the headless spawn paths ([`spawn_window`],
    /// [`ensure_automation_heartbeat`]) that drive tmux via one-shot commands and
    /// must not open a control-mode connection.
    fn ensure_session_configured(&self) -> Result<()> {
        if !self.session_exists() {
            debug!(
                "Creating tmux session '{}' on socket '{}'",
                self.session, self.socket
            );
            self.run_tmux(&[
                "new-session",
                "-d",
                "-s",
                &self.session,
                "-x",
                "80",
                "-y",
                "24",
            ])
            .context("Failed to create tmux session")?;
            // Cheap defensiveness on Windows: poll until the freshly-created
            // session answers `has-session` before applying options. (The
            // `no server running on 'thurbox__thurbox'` failure that originally
            // motivated this was actually psmux session *nesting*, now fixed at
            // the root by `strip_mux_nesting_env`; this poll is a harmless belt
            // against any genuinely-async `new-session -d` and a no-op when the
            // first probe succeeds — which it does on the normal path.)
            #[cfg(windows)]
            self.wait_for_session_ready();
        }
        self.apply_session_config()
    }

    /// Poll (up to 5s) until the freshly-created session answers `has-session`.
    /// Defensive belt against an async `new-session -d`; normally a no-op (the
    /// first probe succeeds). See
    /// [`ensure_session_configured`](Self::ensure_session_configured).
    #[cfg(windows)]
    fn wait_for_session_ready(&self) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while std::time::Instant::now() < deadline {
            if self.session_exists() {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    }

    /// The shell tmux should use for `default-command`. Local uses the user's
    /// `$SHELL`; a remote backend uses a POSIX shell guaranteed to exist on the
    /// remote host. Not used on Windows (psmux keeps its native default shell —
    /// see [`apply_session_config`](Self::apply_session_config)).
    ///
    /// The value must be a single, space-free token: it round-trips through the
    /// remote transport's per-argument shell-quoting (`ssh`/`wsl.exe`), where a
    /// space would be re-split by the remote shell into extra `set-option` args.
    /// The login-shell `PATH` fix for remote agents (e.g. `claude` under
    /// `~/.local/bin`) is applied at the *window command* instead — see
    /// [`build_shell_command`](Self::build_shell_command) /
    /// [`login_wrap_for_remote`](Self::login_wrap_for_remote).
    #[cfg(not(windows))]
    fn config_shell(&self) -> String {
        if self.transport.is_remote() {
            "/bin/sh".to_string()
        } else {
            std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string())
        }
    }

    /// Build the shell command string to pass to tmux new-window.
    ///
    /// The whole string is interpreted by the multiplexer server's shell, so
    /// **every** token — the command itself as well as each argument — is
    /// shell-escaped. Leaving the command unescaped would break (or allow
    /// injection through) a command path containing a space or shell
    /// metacharacter; `shell_escape` is a no-op for ordinary binary names so the
    /// common case (`claude`, `/usr/bin/codex`) is unchanged.
    fn build_shell_command(command: &str, args: &[String]) -> String {
        let mut parts = vec![control_mode::shell_escape(command)];
        for arg in args {
            parts.push(control_mode::shell_escape(arg));
        }
        parts.join(" ")
    }

    /// Wrap a window command in a **login** shell for a remote/WSL backend so the
    /// user's profile `PATH` is present. Agents are commonly installed under
    /// `~/.local/bin` (e.g. `claude`), which the login profile adds to `PATH`; a
    /// non-login shell skips those files, so the agent binary isn't found, the
    /// window command exits 1, and the pane dies instantly — the remote session
    /// appears to "not launch". `exec` replaces the wrapper so no extra process
    /// lingers. Local backends already inherit the user's interactive `PATH`, so
    /// they pass through unchanged — and so does a **psmux** remote (a Windows
    /// SSH host), which has no `/bin/sh` to wrap with (psmux windows are built by
    /// [`psmux_window_command`] instead).
    ///
    /// Done here — not via tmux `default-command` — because that value round-trips
    /// through the remote transport's per-arg shell-quoting, where a `-l` flag's
    /// space would be re-split into a stray `set-option` argument.
    fn login_wrap_for_remote(&self, shell_cmd: &str) -> String {
        if self.transport.is_remote() && !self.transport.uses_psmux() {
            let inner = control_mode::shell_escape(&format!("exec {shell_cmd}"));
            format!("/bin/sh -lc {inner}")
        } else {
            shell_cmd.to_string()
        }
    }

    /// Build the PowerShell command a psmux window runs: set the env vars, then
    /// launch the agent.
    ///
    /// psmux ignores `new-window -e` — env vars never reach the window's
    /// process — so they are folded into the command itself (`Set-Item Env:K
    /// 'v'; …`, chosen over `$env:K` so the string stays `$`-free). psmux runs
    /// the window command via `powershell -NoLogo -Command <string>`, whose
    /// Win32 command line strips unescaped double quotes — so all quoting is
    /// PowerShell **single** quotes (`''` = literal `'`), which Win32
    /// tokenization passes through. A raw `"` or newline would break the outer
    /// framing on either delivery path (below) with no escape that survives,
    /// so both are neutralized to spaces.
    ///
    /// Two callers deliver this string as **one unit** (verified against psmux
    /// 3.3.6; both needed because psmux drops what tmux would keep):
    /// - [`psmux_window_command`](Self::psmux_window_command) wraps it in
    ///   double quotes for a control-mode `new-window` line, whose parser keeps
    ///   only the *first* trailing token (tmux joins them) — the agent launched
    ///   with no args. psmux's tokenizer concatenates adjacent `'…'` segments
    ///   but passes `'` through `"…"` tokens untouched (backslash is literal
    ///   everywhere, so `C:\` paths are safe) — hence single quotes inside,
    ///   double quotes outside.
    /// - [`spawn_window`] passes it verbatim as a single argv token (the argv
    ///   path joins trailing tokens fine, but still ignores `-e`).
    fn psmux_window_powershell(
        command: &str,
        args: &[String],
        env: &HashMap<String, String>,
    ) -> String {
        fn ps_quote(s: &str) -> String {
            format!("'{}'", s.replace('\'', "''"))
        }
        let mut ps = String::new();
        // Sort for a deterministic command (HashMap iteration order isn't).
        let mut pairs: Vec<_> = env.iter().collect();
        pairs.sort();
        for (k, v) in pairs {
            ps.push_str(&format!("Set-Item Env:{k} {}; ", ps_quote(v)));
        }
        ps.push_str(&format!("& {}", ps_quote(command)));
        for a in args {
            ps.push(' ');
            ps.push_str(&ps_quote(a));
        }
        ps.replace(['"', '\n'], " ")
    }

    /// [`psmux_window_powershell`](Self::psmux_window_powershell) framed as one
    /// **double-quoted** control-mode token for a `new-window` line.
    fn psmux_window_command(
        command: &str,
        args: &[String],
        env: &HashMap<String, String>,
    ) -> String {
        format!("\"{}\"", Self::psmux_window_powershell(command, args, env))
    }

    /// Run a closure with a reference to the active control mode, or bail if
    /// it has not been started yet.
    ///
    /// Centralizes the "lock + assert started" invariant in one place so
    /// callers receive a guaranteed-live `&ControlMode` and never touch the
    /// `Option` directly. This replaces a former pattern where each call site
    /// re-asserted the invariant with `guard.as_ref().unwrap()` after a
    /// separate `is_none()` check — fragile, since a refactor of the check
    /// could silently leave the `unwrap`s reachable.
    fn with_control<F, R>(&self, f: F) -> Result<R>
    where
        F: FnOnce(&ControlMode) -> Result<R>,
    {
        let guard = self
            .control
            .lock()
            .map_err(|e| anyhow::anyhow!("control lock: {e}"))?;
        let ctrl = guard.as_ref().ok_or_else(|| {
            anyhow::anyhow!("Control mode not started — call ensure_ready() first")
        })?;
        f(ctrl)
    }

    /// Drop the dead control mode connection and start a fresh one.
    fn reconnect_control(&self) -> Result<()> {
        let mut guard = self
            .control
            .lock()
            .map_err(|e| anyhow::anyhow!("control lock: {e}"))?;
        // Start the replacement *before* touching `guard`, and only store it on
        // success. A failed `start()` propagates via `?` while the existing
        // handle stays in place — so a retry reconnects cleanly instead of
        // hitting `control = None` and reporting the misleading "call
        // ensure_ready() first". Assigning `Some(fresh)` drops the dead
        // ControlMode (its cleanup) as it replaces it.
        let fresh = ControlMode::start(&self.transport, &self.socket, &self.session)?;
        *guard = Some(fresh);
        debug!("Control mode reconnected successfully");
        Ok(())
    }

    /// Send a command via control mode and return the response.
    /// On broken pipe or timeout, reconnects control mode and retries once.
    fn ctrl_command(&self, cmd: &str) -> Result<String> {
        let result = self.with_control(|ctrl| ctrl.send_command(cmd));
        match result {
            Ok(val) => Ok(val),
            Err(err) if is_broken_pipe(&err) || is_recv_timeout(&err) => {
                warn!("Control mode error, reconnecting: {err:#}");
                self.reconnect_control()?;
                self.with_control(|ctrl| ctrl.send_command(cmd))
            }
            Err(err) => Err(err),
        }
    }

    /// Send a command via control mode without waiting for a response.
    /// On broken pipe, reconnects control mode and retries once.
    fn ctrl_command_nowait(&self, cmd: &str) -> Result<()> {
        let result = self.with_control(|ctrl| ctrl.send_command_nowait(cmd));
        match result {
            Ok(()) => Ok(()),
            Err(err) if is_broken_pipe(&err) => {
                warn!("Control mode broken pipe (nowait), reconnecting: {err:#}");
                self.reconnect_control()?;
                self.with_control(|ctrl| ctrl.send_command_nowait(cmd))
            }
            Err(err) => Err(err),
        }
    }

    /// Register a pane sender and return the corresponding reader.
    /// Multiple instances can register the same pane; output will be broadcast to all.
    fn register_pane(&self, pane_id: &str) -> Result<ControlModeReader> {
        let (tx, rx) = sync_channel(PANE_CHANNEL_CAPACITY);
        self.with_control(|ctrl| {
            let mut senders = ctrl
                .pane_senders
                .lock()
                .map_err(|e| anyhow::anyhow!("pane_senders lock: {e}"))?;
            senders
                .entry(pane_id.to_string())
                .or_insert_with(Vec::new)
                .push(tx);
            Ok(())
        })?;
        Ok(ControlModeReader::new(rx))
    }

    /// Unregister a pane sender (causes the reader to get EOF).
    /// Note: Currently removes all senders for this pane. For true instance-specific
    /// unregistration, we would need to track which sender belongs to which instance.
    fn unregister_pane(&self, pane_id: &str) -> Result<()> {
        self.with_control(|ctrl| {
            let mut senders = ctrl
                .pane_senders
                .lock()
                .map_err(|e| anyhow::anyhow!("pane_senders lock: {e}"))?;
            senders.remove(pane_id);
            Ok(())
        })
    }

    /// Create a writer for a specific pane.
    fn pane_writer(&self, pane_id: &str) -> Result<ControlModeWriter> {
        // psmux lacks tmux's `send-keys -H`, so the writer encodes keystrokes
        // differently for it (see `control_mode::send_keys_commands`).
        let psmux = self.transport.uses_psmux();
        self.with_control(|ctrl| {
            Ok(ControlModeWriter {
                stdin: Arc::clone(&ctrl.stdin),
                pane_id: pane_id.to_string(),
                psmux,
            })
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

    /// Capture a pane's scrollback history + visible screen as terminal bytes
    /// suitable for seeding a fresh vt100 parser.
    ///
    /// The control-mode `%output` stream only carries bytes emitted after the
    /// pane is connected, so an adopted session would otherwise start with an
    /// empty scrollback — the forced repaint restores the visible screen but
    /// not the history above it. `-e` keeps colors, `-J` rejoins wrapped lines
    /// so they re-wrap at the adopting panel's width, `-S -<n>` extends the
    /// capture into history (tmux clamps to what exists).
    fn capture_history_seed(&self, pane_id: &str) -> Result<Vec<u8>> {
        let lines = crate::session::settings::global()
            .scrollback_lines
            .min(MAX_CAPTURE_LINES as usize);
        let start = format!("-{lines}");
        let output = self.run_tmux(&[
            "capture-pane",
            "-e",
            "-p",
            "-J",
            "-S",
            &start,
            "-t",
            pane_id,
        ])?;
        Ok(history_seed_bytes(output.stdout))
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

impl SessionBackend for TmuxBackend {
    fn name(&self) -> &str {
        &self.name
    }

    fn check_available(&self) -> Result<()> {
        // `tmux -L <socket> -V` prints the version without connecting, and over
        // the SSH transport this verifies remote connectivity at the same time.
        let output = self
            .transport
            .tmux_command(&self.socket, &["-V"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .context("tmux is not installed or not in PATH")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            bail!("tmux -V failed: {}", stderr.trim());
        }

        let version_str = String::from_utf8_lossy(&output.stdout);
        check_min_version(&version_str)?;
        debug!("multiplexer version: {}", version_str.trim());
        Ok(())
    }

    fn ensure_ready(&self) -> Result<()> {
        self.ensure_session_configured()?;

        // Start control mode if not already running.
        let mut guard = self
            .control
            .lock()
            .map_err(|e| anyhow::anyhow!("control lock: {e}"))?;
        if guard.is_none() {
            debug!("Starting tmux control mode");
            *guard = Some(ControlMode::start(
                &self.transport,
                &self.socket,
                &self.session,
            )?);
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
        // psmux can't take the command as joined trailing tokens nor env via
        // `-e` (see `psmux_window_command`); everything is folded into one
        // token there. tmux keeps the byte-identical multi-token + `-e` path.
        let psmux = self.transport.uses_psmux();
        let shell_cmd = if psmux {
            Self::psmux_window_command(command, args, env)
        } else {
            self.login_wrap_for_remote(&Self::build_shell_command(command, args))
        };

        let cwd_part = match cwd {
            Some(dir) => format!(" -c {}", control_mode::shell_escape(&dir.to_string_lossy())),
            None => String::new(),
        };
        let env_part: String = if psmux {
            String::new()
        } else {
            env.iter()
                .map(|(k, v)| format!(" -e {}", shell_escape(&format!("{k}={v}"))))
                .collect()
        };
        let escaped_window_name = shell_escape(window_name);
        let session = &self.session;
        let cmd = format!(
            "new-window -t {session} -n {escaped_window_name} -P -F '#{{pane_id}}'{cwd_part}{env_part} {shell_cmd}"
        );
        let result = self.ctrl_command(&cmd)?;
        let pane_id = result.trim().to_string();
        if !control_mode::is_valid_pane_id(&pane_id) {
            bail!("tmux new-window returned an invalid pane id: {pane_id:?}");
        }

        debug!(pane_id = %pane_id, "tmux window created via control mode");

        let connected = self.connect_pane(&pane_id, rows, cols)?;

        Ok(SpawnedSession {
            backend_id: pane_id,
            output: connected.output,
            input: connected.input,
        })
    }

    fn adopt(&self, backend_id: &str, rows: u16, cols: u16) -> Result<AdoptedSession> {
        // backend_id comes from the shared DB — never interpolate it unvalidated.
        if !control_mode::is_valid_pane_id(backend_id) {
            bail!("refusing to adopt invalid pane id: {backend_id:?}");
        }
        // Opt-in split timing (THURBOX_PERF_LOG): `capture_history_seed` is an
        // independent `tmux capture-pane` subprocess (parallelizable), while
        // `connect_pane` drives the serialized control-mode connection. Knowing
        // their ratio decides whether parallelizing the capture half is worth it.
        let perf_log = std::env::var_os("THURBOX_PERF_LOG").is_some();

        // Capture before connecting so seeded history can't duplicate live
        // output. Best-effort: adoption must survive a failed capture.
        let capture_start = perf_log.then(std::time::Instant::now);
        let seed = self.capture_history_seed(backend_id).unwrap_or_else(|e| {
            warn!("Failed to capture history for pane {backend_id}: {e}");
            Vec::new()
        });
        let capture_ms = capture_start.map(|s| s.elapsed().as_millis() as u64);

        let connect_start = perf_log.then(std::time::Instant::now);
        let connected = self.connect_pane(backend_id, rows, cols)?;
        if let (Some(capture_ms), Some(start)) = (capture_ms, connect_start) {
            tracing::info!(
                pane = %backend_id,
                capture_ms,
                connect_ms = start.elapsed().as_millis() as u64,
                "adopt_split"
            );
        }
        if seed.is_empty() {
            return Ok(connected);
        }
        // Prepend the captured history to the live stream — the reader loop
        // feeds it into the parser first, populating the UI scrollback.
        Ok(AdoptedSession {
            output: Box::new(Cursor::new(seed).chain(connected.output)),
            input: connected.input,
        })
    }

    fn discover(&self) -> Result<Vec<DiscoveredSession>> {
        if !self.session_exists() {
            return Ok(Vec::new());
        }

        // Once control mode is up, route through `ctrl_command` so a dead
        // connection is transparently reconnected + retried (like every other
        // control-mode call) instead of failing the discovery. Before control
        // mode has started, fall back to a one-shot direct tmux command.
        let control_started = {
            let guard = self
                .control
                .lock()
                .map_err(|e| anyhow::anyhow!("control lock: {e}"))?;
            guard.is_some()
        };
        let result = if control_started {
            self.ctrl_command(&format!(
                "list-windows -t {} -F '#{{pane_id}}|#{{window_name}}|#{{pane_dead}}'",
                self.session
            ))?
        } else {
            self.tmux_output(&[
                "list-windows",
                "-t",
                &self.session,
                "-F",
                "#{pane_id}|#{window_name}|#{pane_dead}",
            ])?
        };

        let mut sessions = Vec::new();
        for line in result.lines() {
            let parts: Vec<&str> = line.splitn(3, '|').collect();
            if parts.len() < 3 {
                continue;
            }

            let window_name = parts[1];
            // Only discover windows with our prefix (tb- for Claude, tbs- for shells).
            if !window_name.starts_with(WINDOW_PREFIX) {
                continue;
            }

            if !control_mode::is_valid_pane_id(parts[0]) {
                warn!(
                    "Skipping discovered window with invalid pane id: {:?}",
                    parts[0]
                );
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

    fn take_hook_state_events(&self) -> Vec<(String, String)> {
        // Lock `control` directly — `with_control` errors before the control
        // connection starts, and "no connection yet" is simply "no events".
        self.control
            .lock()
            .ok()
            .and_then(|guard| guard.as_ref().map(ControlMode::take_sub_events))
            .unwrap_or_default()
    }

    /// The shell-pane command must match the **host's** OS, not the local
    /// binary's — the trait default reads the local `$SHELL`/`%COMSPEC%`,
    /// which shipped e.g. `/bin/zsh` to a remote Windows pane
    /// ("CommandNotFoundException"). Remote hosts get a shell that exists
    /// there by construction: `powershell` on a psmux (Windows) host — the
    /// same interpreter psmux wraps every window command in — and `/bin/sh`
    /// on a Unix/WSL host (the local `$SHELL` may not be installed there; the
    /// login wrap gives it the profile `PATH`). Local backends keep the trait
    /// default's behavior.
    fn default_shell(&self) -> String {
        if !self.transport.is_remote() {
            #[cfg(windows)]
            {
                return std::env::var("COMSPEC").unwrap_or_else(|_| "cmd.exe".to_string());
            }
            #[cfg(not(windows))]
            {
                return std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string());
            }
        }
        if self.transport.uses_psmux() {
            "powershell".to_string()
        } else {
            "/bin/sh".to_string()
        }
    }
}

/// Wrap `text` in the bracketed-paste escape sequences (`ESC[200~ … ESC[201~`)
/// so a multi-line prompt is delivered as a single paste — the embedded
/// newlines insert as text instead of submitting the prompt on the first one.
/// Mirrors the TUI's `App::send_prompt_to_session`; the trailing `Enter` is
/// still sent separately by the caller. tmux delivers these bytes literally via
/// `send-keys -l`.
fn bracketed_paste(text: &str) -> String {
    format!("\x1b[200~{text}\x1b[201~")
}

/// Send text immediately to a session pane (no scheduling), followed by Enter.
///
/// Targets the tmux window named `tb-<session_name>` in the thurbox tmux
/// session and uses a "paste text → brief delay → press Enter" sequence so the
/// target app has time to process the pasted input.
pub fn send_prompt_now(session_name: &str, text: &str) -> Result<()> {
    let target = window_target(session_name);
    let payload = bracketed_paste(text);

    let status = local_mux_command(&["send-keys", "-t", &target, "-l", &payload])
        .status()
        .context("Failed to run tmux send-keys for prompt text")?;
    if !status.success() {
        bail!("tmux send-keys (text) exited with status {status}");
    }

    std::thread::sleep(SEND_KEYS_ENTER_DELAY);

    let status = local_mux_command(&["send-keys", "-t", &target, "Enter"])
        .status()
        .context("Failed to run tmux send-keys for Enter")?;
    if !status.success() {
        bail!("tmux send-keys (Enter) exited with status {status}");
    }
    Ok(())
}

/// Window name for the headless automation heartbeat keeper. Deliberately NOT
/// `tb-` prefixed so [`LocalTmuxBackend::discover`] ignores it — it is
/// infrastructure, not a session.
const HEARTBEAT_WINDOW: &str = "automation-heartbeat";

/// How often the heartbeat keeper invokes `automation tick`.
const HEARTBEAT_INTERVAL_SECS: u64 = 60;

/// List the window names in the thurbox tmux session (empty if the server is
/// not running).
fn list_window_names() -> Vec<String> {
    let Ok(out) =
        local_mux_command(&["list-windows", "-t", TMUX_SESSION, "-F", "#{window_name}"]).output()
    else {
        return Vec::new();
    };
    if !out.status.success() {
        return Vec::new();
    }
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(|s| s.to_string())
        .collect()
}

/// Whether the agent window `tb-<session_name>` currently exists in the thurbox
/// tmux server. Used by the headless dispatcher to skip `send` automations
/// whose target session is no longer running rather than failing into a dead
/// pane.
pub fn window_exists(session_name: &str) -> bool {
    let want = agent_window_name(session_name);
    list_window_names().contains(&want)
}

/// Schedule a one-shot prompt delivery into a session's window after
/// `delay_secs`, via a detached `tmux run-shell` timer.
///
/// Used by the headless automation dispatcher to deliver a Spawn automation's
/// prompt once the freshly launched agent CLI has had time to boot — offline
/// there is no TUI deferred-input queue to lean on. Local-tmux scoped.
pub fn send_prompt_after_delay(session_name: &str, text: &str, delay_secs: u64) -> Result<()> {
    let target = window_target(session_name);
    let script = deferred_prompt_script(&target, text);
    let status = local_mux_command(&["run-shell", "-b", "-d", &delay_secs.to_string(), &script])
        .status()
        .context("Failed to schedule tmux run-shell for deferred prompt")?;
    if !status.success() {
        bail!("tmux run-shell (deferred prompt) exited with status {status}");
    }
    Ok(())
}

/// Build the `run-shell` script that pastes the prompt, waits a beat so the
/// bracketed paste is consumed, then presses Enter. `run-shell` executes the
/// script via the multiplexer server's shell, so the syntax is platform-specific.
///
/// POSIX path (`tmux` on Linux/macOS): a plain `sh` one-liner.
#[cfg(not(windows))]
fn deferred_prompt_script(target: &str, text: &str) -> String {
    let escaped_target = shell_escape(target);
    let socket = local_socket();
    // Bracketed-paste wrap (see `bracketed_paste`) so multi-line prompts don't
    // submit early; `-l` makes the multiplexer deliver the bytes literally.
    let escaped_text = shell_escape(&bracketed_paste(text));
    format!(
        "{DEFAULT_MUX} -L {socket} send-keys -t {escaped_target} -l {escaped_text}; \
         sleep 0.2; \
         {DEFAULT_MUX} -L {socket} send-keys -t {escaped_target} Enter"
    )
}

/// Windows path (`psmux`): psmux's `run-shell` is not a POSIX shell, so drive the
/// sequence through PowerShell explicitly (`Start-Sleep` for the sub-second beat).
/// PowerShell single-quoted literals escape an embedded `'` by doubling it.
#[cfg(windows)]
fn deferred_prompt_script(target: &str, text: &str) -> String {
    let t = ps_single_quote(target);
    let socket = local_socket();
    let body = ps_single_quote(&bracketed_paste(text));
    format!(
        "powershell -NoProfile -Command \"{DEFAULT_MUX} -L {socket} send-keys -t {t} -l {body}; \
         Start-Sleep -Milliseconds 200; \
         {DEFAULT_MUX} -L {socket} send-keys -t {t} Enter\""
    )
}

/// Wrap `s` in a PowerShell single-quoted literal, doubling embedded quotes.
#[cfg(windows)]
fn ps_single_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

/// Ensure the automation heartbeat keeper window is running.
///
/// Creates a detached tmux window that loops `<cli_path> automation tick` every
/// `HEARTBEAT_INTERVAL_SECS` seconds, so automations fire even with no TUI
/// attached. The live window also keeps the tmux server alive, so spawn-only
/// automations work with no other sessions. Idempotent — a no-op when the
/// keeper already exists. `cli_path` is the absolute path to `thurbox-cli`.
///
pub fn ensure_automation_heartbeat(cli_path: &Path) -> Result<()> {
    TmuxBackend::local().ensure_session_configured()?;
    if list_window_names().iter().any(|w| w == HEARTBEAT_WINDOW) {
        return Ok(());
    }
    let loop_cmd = heartbeat_loop_command(cli_path);
    let status = local_mux_command(&[
        "new-window",
        "-d",
        "-t",
        TMUX_SESSION,
        "-n",
        HEARTBEAT_WINDOW,
        &loop_cmd,
    ])
    .status()
    .context("Failed to create automation heartbeat window")?;
    if !status.success() {
        bail!("tmux new-window (heartbeat) exited with status {status}");
    }
    debug!("Armed automation heartbeat keeper window");
    Ok(())
}

/// The keeper's loop, as the window command. It runs via the server's shell,
/// so the CLI path is escaped for it.
#[cfg(not(windows))]
fn heartbeat_loop_command(cli_path: &Path) -> String {
    let cli = shell_escape(&cli_path.display().to_string());
    format!(
        "while true; do {cli} automation tick >/dev/null 2>&1; sleep {HEARTBEAT_INTERVAL_SECS}; done"
    )
}

/// Windows: psmux runs a window command via `powershell -NoLogo -Command`, so
/// the keeper loop is PowerShell — handed over as **one argv token**, dodging
/// psmux's trailing-token handling entirely (same delivery as
/// `psmux_window_powershell`, whose quoting rules the `ps_single_quote` here
/// shares). This used to be a no-op ("no POSIX shell for the keeper loop"),
/// which silently degraded headless automation firing to TUI-only on Windows.
#[cfg(windows)]
fn heartbeat_loop_command(cli_path: &Path) -> String {
    let cli = ps_single_quote(&cli_path.display().to_string());
    format!(
        "while ($true) {{ & {cli} automation tick *> $null; Start-Sleep {HEARTBEAT_INTERVAL_SECS} }}"
    )
}

/// Resolve the path to the `thurbox-cli` binary that sits next to the currently
/// running executable (TUI or CLI), falling back to a bare `thurbox-cli` on
/// `PATH` when resolution fails.
///
/// The platform executable suffix (`.exe` on Windows, empty elsewhere) is
/// applied via [`std::env::consts::EXE_SUFFIX`], so the self/sibling match works
/// for `thurbox-cli.exe` too.
pub fn resolve_cli_binary() -> std::path::PathBuf {
    let cli_name = format!("thurbox-cli{}", std::env::consts::EXE_SUFFIX);
    if let Ok(exe) = std::env::current_exe() {
        if exe.file_name().and_then(|n| n.to_str()) == Some(cli_name.as_str()) {
            return exe;
        }
        if let Some(dir) = exe.parent() {
            let sibling = dir.join(&cli_name);
            if sibling.exists() {
                return sibling;
            }
        }
    }
    std::path::PathBuf::from(cli_name)
}

/// Capture the rendered contents of a session's pane.
///
/// Returns the visible terminal text. `lines` controls how many lines of
/// scrollback to include before the visible region (capped to a sane max).
pub fn capture_pane_text(session_name: &str, lines: u32) -> Result<String> {
    let target = window_target(session_name);
    let lines = lines.min(MAX_CAPTURE_LINES);
    let start = format!("-{lines}");

    let output = local_mux_command(&["capture-pane", "-p", "-J", "-t", &target, "-S", &start])
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

/// Convert raw `capture-pane -p` output into vt100 parser input: drop the
/// unused blank bottom of the visible pane and turn bare `\n` line endings
/// into `\r\n` so each seeded line starts at column 0.
fn history_seed_bytes(mut raw: Vec<u8>) -> Vec<u8> {
    while raw.last() == Some(&b'\n') {
        raw.pop();
    }
    let mut seed = Vec::with_capacity(raw.len() + raw.len() / 8);
    for b in raw {
        if b == b'\n' {
            seed.push(b'\r');
        }
        seed.push(b);
    }
    seed
}

/// Session-level tmux options applied to the thurbox tmux session.
///
/// Single source of truth for both the TUI and headless paths — applied
/// (alongside the server-wide options + `default-command`) by
/// [`TmuxBackend::apply_session_config`]. In particular `remain-on-exit=on` is
/// required so a failed agent process leaves its tmux window visible with the
/// error instead of silently vanishing.
const SESSION_OPTS: &[(&str, &str)] = &[
    ("remain-on-exit", "on"),
    ("status", "off"),
    ("history-limit", "5000"),
    // Allow each window to size independently of the smallest attached client.
    ("window-size", "manual"),
];

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
    // Ensure the session exists and is configured, without opening a
    // control-mode connection (headless one-shot path).
    TmuxBackend::local().ensure_session_configured()?;

    let window_name = agent_window_name(session_name);
    let mut tmux = local_mux_command(&[
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
    if cfg!(windows) {
        // psmux (the local mux on Windows) ignores `-e`, so the env must be
        // folded into the window command itself; delivered as a single argv
        // token (see `psmux_window_powershell`).
        tmux.arg(TmuxBackend::psmux_window_powershell(command, args, env));
    } else {
        for (k, v) in env {
            tmux.args(["-e", &format!("{k}={v}")]);
        }
        // Pass the command + args as a single argv list. tmux treats trailing
        // args as the command to run inside the window.
        tmux.arg(command);
        for a in args {
            tmux.arg(a);
        }
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

/// Headless spawn of an agent window on a remote host over SSH.
///
/// Returns the remote tmux pane id (`%N`). Unlike the local [`spawn_window`]
/// (which leaves `backend_id` empty for the TUI to resolve by name), this drives
/// the SSH backend's control mode to learn the real pane id up front. The
/// control-mode connection is dropped when this returns; the remote tmux keeps
/// the window alive for the TUI to adopt later.
pub fn spawn_window_remote(
    host: &crate::session::HostDef,
    session_name: &str,
    command: &str,
    args: &[String],
    cwd: Option<&Path>,
    env: &HashMap<String, String>,
) -> Result<String> {
    let backend = TmuxBackend::from_host(host);
    backend
        .check_available()
        .context("remote host is unreachable or tmux is missing")?;
    backend.ensure_ready()?;
    let window_name = agent_window_name(session_name);
    // Headless: no live terminal, so use a sane default geometry. The TUI
    // resizes the pane to its real dimensions when it adopts the session.
    let spawned = backend.spawn(&window_name, command, args, cwd, env, 24, 80)?;
    Ok(spawned.backend_id)
}

/// Kill a remote tmux pane on `host` by its pane id (`%N`), best-effort.
///
/// Mirror of [`kill_window`] for the SSH transport. Used to tear down a window
/// that was spawned remotely but could not be tracked (e.g. the DB write failed
/// after the spawn), so it does not leak as an orphaned remote window.
pub fn kill_pane_remote(host: &crate::session::HostDef, backend_id: &str) -> Result<()> {
    let backend = TmuxBackend::from_host(host);
    backend.ensure_ready()?;
    backend.kill(backend_id)
}

/// Kill the tmux window `tb-<session_name>` if it exists.
pub fn kill_window(session_name: &str) -> Result<()> {
    let target = window_target(session_name);
    let output = local_mux_command(&["kill-window", "-t", &target])
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

/// Resolve the agent window `tb-<session_name>` and return the OS pid of its
/// pane's foreground process (`#{pane_pid}`), or `None` when the window is gone
/// or the pid can't be read.
///
/// One-shot on the local socket. Used by the force-teardown path to reap a live
/// pane process **before** removing its cwd on Windows, where a directory that
/// is a live process's cwd cannot be removed (`os error 32`); Unix permits it,
/// so callers only need the returned pid on Windows.
pub fn window_pane_pid(session_name: &str) -> Result<Option<u32>> {
    let target = window_target(session_name);
    let output = local_mux_command(&["display-message", "-p", "-t", &target, "#{pane_pid}"])
        .output()
        .context("Failed to run tmux display-message for pane pid")?;
    if !output.status.success() {
        // No such window (already torn down) — not an error for the caller.
        return Ok(None);
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    Ok(stdout.trim().parse::<u32>().ok())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::control_mode::{
        decode_octal, format_send_keys, parse_notification, shell_escape,
    };

    // The control-mode primitives are re-exported through this module. Their
    // behavior is covered exhaustively in `control_mode`'s own test module;
    // this single smoke check just asserts the re-export path still resolves
    // (the per-case bodies that used to be duplicated here added no coverage).
    #[test]
    fn control_mode_reexports_resolve() {
        assert_eq!(shell_escape("hello world"), "'hello world'");
        assert_eq!(decode_octal("\\033"), vec![27]);
        assert_eq!(format_send_keys("%1", b"A"), "send-keys -t %1 -H 41\n");
        assert_eq!(
            parse_notification("%pause %1"),
            Notification::Pause {
                pane_id: "%1".to_string()
            }
        );
    }

    // --- parse_tmux_version tests ---

    #[test]
    fn parse_tmux_version_plain() {
        assert_eq!(parse_tmux_version("tmux 3.4").unwrap(), (3, 4));
    }

    #[test]
    fn parse_tmux_version_trailing_letter() {
        assert_eq!(parse_tmux_version("tmux 3.3a").unwrap(), (3, 3));
    }

    #[test]
    fn parse_tmux_version_without_prefix() {
        assert_eq!(parse_tmux_version("3.2").unwrap(), (3, 2));
    }

    #[test]
    fn parse_tmux_version_rejects_garbage() {
        assert!(parse_tmux_version("not a version").is_err());
    }

    // --- check_min_version (multiplexer version gate) ---

    #[test]
    fn min_version_accepts_recent_tmux() {
        assert!(check_min_version("tmux 3.4").is_ok());
        assert!(check_min_version("tmux 3.2").is_ok());
    }

    #[test]
    fn min_version_rejects_old_tmux() {
        assert!(check_min_version("tmux 2.8").is_err());
    }

    #[test]
    fn min_version_accepts_non_tmux_clone() {
        // psmux numbers itself independently and may not print a `tmux ` banner;
        // once it answers `-V` it is accepted regardless of its own version.
        assert!(check_min_version("psmux 0.3.1").is_ok());
        assert!(check_min_version("psmux 1.0").is_ok());
        assert!(check_min_version("pmux 0.1").is_ok());
    }

    #[test]
    fn resolve_cli_binary_uses_platform_exe_suffix() {
        let p = resolve_cli_binary();
        let name = p.file_name().unwrap().to_string_lossy();
        assert_eq!(name, format!("thurbox-cli{}", std::env::consts::EXE_SUFFIX));
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

    #[test]
    fn build_shell_command_escapes_command_path() {
        // The command token is interpreted by the server's shell, so a path
        // with a space (or any metacharacter) must be quoted, not left bare —
        // otherwise the shell would split it and the launch would break.
        let cmd =
            LocalTmuxBackend::build_shell_command("/opt/My Agents/codex", &["--foo".to_string()]);
        assert_eq!(cmd, "'/opt/My Agents/codex' --foo");
    }

    #[test]
    fn backend_default_has_no_control_mode() {
        let backend = LocalTmuxBackend::new();
        let guard = backend.control.lock().unwrap();
        assert!(guard.is_none());
    }

    #[test]
    fn local_backend_is_named_local_tmux_with_local_transport() {
        let backend = LocalTmuxBackend::new();
        assert_eq!(backend.name(), "local-tmux");
        assert!(!backend.transport.is_remote());
    }

    #[test]
    fn from_host_builds_named_ssh_backend() {
        let host = crate::session::HostDef {
            name: "devbox".into(),
            destination: "me@devbox".into(),
            ssh_opts: vec!["-o".into(), "ControlMaster=auto".into()],
            ..Default::default()
        };
        let backend = TmuxBackend::from_host(&host);
        assert_eq!(backend.name(), "ssh:devbox");
        assert!(backend.transport.is_remote());
        // Falls back to the default socket/session when the host omits them.
        assert_eq!(backend.socket, TMUX_SOCKET);
        assert_eq!(backend.session, TMUX_SESSION);
    }

    #[test]
    fn from_host_builds_named_wsl_backend() {
        let host = crate::session::HostDef::wsl("Ubuntu");
        let backend = TmuxBackend::from_host(&host);
        assert_eq!(backend.name(), "wsl:Ubuntu");
        assert!(backend.transport.is_remote());
        assert!(matches!(
            backend.transport,
            TmuxTransport::Wsl { ref distro, .. } if distro == "Ubuntu"
        ));
        assert_eq!(backend.socket, TMUX_SOCKET);
        assert_eq!(backend.session, TMUX_SESSION);
    }

    #[test]
    fn local_socket_honors_env_override() {
        // nextest runs one process per test, so env mutation can't race other
        // tests reading `local_socket()`.
        std::env::set_var(SOCKET_OVERRIDE_ENV, "thurbox-lab-test");
        assert_eq!(local_socket(), "thurbox-lab-test");
        assert_eq!(TmuxBackend::local().socket, "thurbox-lab-test");
        // Empty counts as unset — a sandbox script exporting `THURBOX_SOCKET=`
        // must not produce `-L ''`.
        std::env::set_var(SOCKET_OVERRIDE_ENV, "");
        assert_eq!(local_socket(), TMUX_SOCKET);
        std::env::remove_var(SOCKET_OVERRIDE_ENV);
        assert_eq!(local_socket(), TMUX_SOCKET);
    }

    #[test]
    fn default_shell_matches_host_os_not_local() {
        // The local $SHELL (e.g. /bin/zsh) may not exist on the host: a remote
        // Windows pane got "CommandNotFoundException", a zsh-less Linux host a
        // dead pane. Remote backends pick by transport.
        let winbox = TmuxBackend::from_host(&crate::session::HostDef {
            name: "winbox".into(),
            destination: "me@winbox".into(),
            multiplexer: Some("psmux".into()),
            ..Default::default()
        });
        assert_eq!(winbox.default_shell(), "powershell");

        let devbox = TmuxBackend::from_host(&crate::session::HostDef {
            name: "devbox".into(),
            destination: "me@devbox".into(),
            ..Default::default()
        });
        assert_eq!(devbox.default_shell(), "/bin/sh");

        let wsl = TmuxBackend::from_host(&crate::session::HostDef::wsl("Ubuntu"));
        assert_eq!(wsl.default_shell(), "/bin/sh");

        // Local keeps the platform default ($SHELL / %COMSPEC%).
        let local = TmuxBackend::local();
        #[cfg(not(windows))]
        assert_eq!(
            local.default_shell(),
            std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string())
        );
        #[cfg(windows)]
        assert_eq!(
            local.default_shell(),
            std::env::var("COMSPEC").unwrap_or_else(|_| "cmd.exe".to_string())
        );
    }

    #[test]
    fn login_wrap_wraps_remote_command_in_login_shell() {
        // Remote/WSL: the window command runs under a login shell so the user's
        // profile PATH (e.g. `~/.local/bin/claude`) is present, or the agent
        // binary isn't found and the pane dies instantly.
        let backend = TmuxBackend::from_host(&crate::session::HostDef::wsl("Ubuntu"));
        let wrapped = backend.login_wrap_for_remote("claude --resume x");
        assert_eq!(wrapped, "/bin/sh -lc 'exec claude --resume x'");
    }

    #[test]
    fn login_wrap_is_noop_for_local() {
        // Local backends inherit the user's interactive PATH — no wrap needed.
        let backend = TmuxBackend::local();
        assert_eq!(backend.login_wrap_for_remote("claude"), "claude");
    }

    // --- psmux_window_command tests ---
    // psmux keeps only the FIRST trailing new-window token (tmux joins them) and
    // ignores `-e` entirely, so the whole launch — env included — must be one
    // double-quoted token of PowerShell (verified against psmux 3.3.6).

    #[test]
    fn psmux_window_command_is_one_double_quoted_token() {
        let args = vec!["--session-id".to_string(), "abc-123".to_string()];
        let cmd = TmuxBackend::psmux_window_command("claude", &args, &HashMap::new());
        assert_eq!(cmd, "\"& 'claude' '--session-id' 'abc-123'\"");
    }

    #[test]
    fn psmux_window_command_folds_env_as_set_item() {
        // `Set-Item Env:K 'v'` (not `$env:K`) keeps the string `$`-free; sorted
        // for determinism. Values with spaces survive the PS single quotes.
        let mut env = HashMap::new();
        env.insert("THURBOX_SESSION".to_string(), "id-1".to_string());
        env.insert("B".to_string(), "x y".to_string());
        let cmd = TmuxBackend::psmux_window_command("claude", &[], &env);
        assert_eq!(
            cmd,
            "\"Set-Item Env:B 'x y'; Set-Item Env:THURBOX_SESSION 'id-1'; & 'claude'\""
        );
    }

    #[test]
    fn psmux_window_command_escapes_and_sanitizes() {
        // A literal ' doubles (PowerShell escaping); a raw " or newline would
        // terminate the outer token / split the control-mode line, so both are
        // neutralized to spaces. Backslash paths pass through untouched (psmux
        // treats backslash literally everywhere).
        let args = vec!["it's".to_string(), "say \"hi\"\nnow".to_string()];
        let cmd =
            TmuxBackend::psmux_window_command("C:\\Tools\\claude.exe", &args, &HashMap::new());
        assert_eq!(cmd, "\"& 'C:\\Tools\\claude.exe' 'it''s' 'say  hi  now'\"");
    }

    #[test]
    fn login_wrap_is_noop_for_psmux_remote() {
        // A Windows SSH host (multiplexer = "psmux") has no `/bin/sh`; wrapping
        // would replace the agent command with one that can't start at all.
        let host = crate::session::HostDef {
            name: "winbox".into(),
            destination: "me@winbox".into(),
            multiplexer: Some("psmux".into()),
            ..Default::default()
        };
        let backend = TmuxBackend::from_host(&host);
        assert_eq!(backend.login_wrap_for_remote("claude"), "claude");
    }

    #[test]
    fn from_host_honors_socket_and_session_overrides() {
        let host = crate::session::HostDef {
            name: "vm".into(),
            destination: "vm".into(),
            socket: Some("tb-vm".into()),
            session: Some("sess-vm".into()),
            ..Default::default()
        };
        let backend = TmuxBackend::from_host(&host);
        assert_eq!(backend.socket, "tb-vm");
        assert_eq!(backend.session, "sess-vm");
    }

    // Compile-time check: channel capacity must be large enough to buffer heavy output.
    const _: () = assert!(PANE_CHANNEL_CAPACITY >= 1024);

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

    #[test]
    fn window_target_uses_exact_match_prefix() {
        // Without `=`, tmux treats the window name as a pattern and will
        // resolve `tb-foo` ambiguously when both `tb-foo` and
        // `tb-foo-bar` exist. The `=` prefix forces exact-match lookup.
        let t = window_target("foo");
        assert!(t.ends_with(":=tb-foo"), "got {t}");
    }

    // --- history_seed_bytes tests (adopt-time scrollback seeding) ---

    #[test]
    fn history_seed_converts_newlines_and_trims_trailing_blanks() {
        let raw = b"line1\nline2\n\n\n".to_vec();
        assert_eq!(history_seed_bytes(raw), b"line1\r\nline2".to_vec());
    }

    #[test]
    fn history_seed_empty_capture_yields_empty_seed() {
        assert_eq!(history_seed_bytes(Vec::new()), Vec::<u8>::new());
        assert_eq!(history_seed_bytes(b"\n\n\n".to_vec()), Vec::<u8>::new());
    }

    #[test]
    fn history_seed_preserves_escape_sequences_and_inner_blanks() {
        let raw = b"\x1b[31mred\x1b[0m\n\nplain\n".to_vec();
        assert_eq!(
            history_seed_bytes(raw),
            b"\x1b[31mred\x1b[0m\r\n\r\nplain".to_vec()
        );
    }

    #[test]
    fn seeded_parser_exposes_history_as_scrollback() {
        // Feed more lines than the screen height: the overflow must land in
        // the parser's scrollback, scrollable from the UI.
        let mut parser = vt100::Parser::new(5, 80, 100);
        let raw: Vec<u8> = (1..=10)
            .map(|i| format!("line{i}\n"))
            .collect::<String>()
            .into_bytes();
        parser.process(&history_seed_bytes(raw));

        parser.screen_mut().set_scrollback(usize::MAX);
        assert_eq!(parser.screen().scrollback(), 5);
        assert!(parser.screen().contents().contains("line1"));
        parser.screen_mut().set_scrollback(0);
        assert!(parser.screen().contents().contains("line10"));
    }
}
