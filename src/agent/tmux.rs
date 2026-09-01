use std::borrow::Cow;
use std::collections::HashMap;
use std::io::{Cursor, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::sync_channel;
use std::sync::{Arc, Mutex};

use anyhow::{bail, Context, Result};
use base64::Engine as _;
use tracing::{debug, warn};

use crate::agent::backend::{AdoptedSession, DiscoveredSession, SessionBackend, SpawnedSession};
use crate::agent::control_mode::{
    self, is_broken_pipe, is_recv_timeout, shell_escape, ControlMode, ControlModeReader,
    ControlModeWriter, PANE_CHANNEL_CAPACITY,
};
use crate::agent::transport::{TmuxTransport, DEFAULT_MUX};

/// Dedicated tmux socket name for an instance running out of the **default**
/// data dir — isolates thurbox sessions from the user's tmux. Dev builds use
/// "thurbox-dev" to avoid interfering with an installed release binary. An
/// instance relocated by `THURBOX_DATA_DIR` derives its own name from this one
/// ([`derived_socket`]). Crate-visible as the last-resort fallback when a
/// host's configured socket sanitizes to empty
/// (`builtin_hooks::remote_signal_target`).
pub(crate) const TMUX_SOCKET: &str = if cfg!(dev_build) {
    "thurbox-dev"
} else {
    "thurbox"
};

/// Env var overriding the **local** multiplexer socket name. Wins over the
/// data-dir derivation below, so tooling that needs a socket by name (the dev
/// sandbox, whose teardown kills it) keeps naming it.
///
/// Unix test/sandbox tooling scopes the socket by pointing `TMUX_TMPDIR` at a
/// private directory, but psmux (native Windows) has no socket-directory
/// concept — every `-L <name>` resolves machine-wide, so without this override
/// a scoped test on Windows would share (and could tear down) the user's real
/// `thurbox`/`thurbox-dev` server. Remote hosts are unaffected (their socket
/// comes from `hosts.toml`).
pub const SOCKET_OVERRIDE_ENV: &str = "THURBOX_SOCKET";

/// Env var naming the **data directory** the injected [`SOCKET_OVERRIDE_ENV`]
/// belongs to. Written beside it by `session_ops::thurbox_env_overrides`, and
/// read here to tell an inherited socket from an operator's own.
///
/// Without the pairing, a socket is a bare string with no owner, and the
/// override above wins unconditionally — including in the one case that must
/// not: thurbox injects the socket into every pane it spawns, so a sandbox, a
/// test harness or an agent that relocates itself with `THURBOX_DATA_DIR`
/// *inside* such a pane inherits a name pointing at the operator's server. The
/// database is then isolated and the tmux server is not, which is worse than no
/// isolation at all because it looks contained. An override with no owner is
/// still honoured outright: that is somebody typing it.
pub const SOCKET_OWNER_ENV: &str = "THURBOX_SOCKET_FOR";

/// The local multiplexer socket name — see [`socket_for`] for the precedence.
fn local_socket() -> String {
    socket_for(
        std::env::var(SOCKET_OVERRIDE_ENV).ok(),
        std::env::var_os(SOCKET_OWNER_ENV)
            .map(PathBuf::from)
            .as_deref(),
        crate::paths::data_directory().as_deref(),
        crate::paths::relocated_data_dir().as_deref(),
    )
}

/// Resolve the socket name from the things that can move it:
/// [`SOCKET_OVERRIDE_ENV`] when set, non-empty and **still this instance's**,
/// else a name derived from a relocated data dir, else the compile-time
/// default. Pure, so the precedence is testable without touching the process
/// environment.
///
/// The data dir is the anchor because it holds the database, and the database
/// is the record of which sessions exist: an instance with its own record of
/// them has no business creating their windows on someone else's server. A
/// relocated **config** dir alone does not move the socket — it shares the
/// default instance's sessions and must keep reaching them.
///
/// `socket_owner` is [`SOCKET_OWNER_ENV`]: the data dir the override was
/// injected for. It is what separates "the operator named this server" (no
/// owner — honoured) from "this came from the pane I am running in" (an owner
/// that no longer matches `data_dir` — dropped, so the derivation below runs).
fn socket_for(
    override_name: Option<String>,
    socket_owner: Option<&Path>,
    data_dir: Option<&Path>,
    relocated_data_dir: Option<&Path>,
) -> String {
    if let Some(name) = override_name.filter(|s| !s.is_empty()) {
        // An owner that still names this instance's data dir — or no owner at
        // all, which is an operator typing the name — keeps the override.
        let inherited_from_elsewhere =
            matches!(socket_owner, Some(owner) if Some(owner) != data_dir);
        if !inherited_from_elsewhere {
            return name;
        }
    }
    match relocated_data_dir {
        Some(dir) => derived_socket(dir),
        None => TMUX_SOCKET.to_string(),
    }
}

/// The socket an instance whose data dir is `dir` runs on: the default name
/// suffixed with a short digest of that directory. Deterministic, so the same
/// relocated instance finds its own server on every run and across releases;
/// distinct, so two of them do not share one. Separator noise is normalized
/// away first, so `/tmp/lab` and `/tmp/lab/` are one instance rather than two.
///
/// A digest collision costs no more than today's behaviour — two instances on
/// one server — and never reaches the default socket, whose name has no suffix.
fn derived_socket(dir: &Path) -> String {
    let normalized: std::path::PathBuf = dir.components().collect();
    let digest = fnv1a32(normalized.to_string_lossy().as_bytes());
    format!("{TMUX_SOCKET}-{digest:08x}")
}

/// FNV-1a, 32 bits. Written out rather than reached for in `std`: this name has
/// to be the same string in every process and every release, and neither
/// `DefaultHasher`'s algorithm nor its seed is guaranteed to be.
fn fnv1a32(bytes: &[u8]) -> u32 {
    let mut hash: u32 = 0x811c_9dc5;
    for b in bytes {
        hash ^= u32::from(*b);
        hash = hash.wrapping_mul(0x0100_0193);
    }
    hash
}

/// The socket name this instance's local sessions live on — what `thurbox-cli
/// version --json` reports so a peer attaching over ssh joins the right server,
/// and so an integrator never has to guess the name. Resolved, not constant:
/// an instance relocated by `THURBOX_DATA_DIR` runs on its own socket (see
/// `socket_for`).
pub fn local_socket_name() -> String {
    local_socket()
}

/// Socket names learned from a host's own `thurbox-cli` (`version --json`'s
/// `tmux_socket`), keyed by backend name. A host entry with no explicit
/// `socket` uses *this* build's socket name by default, which is wrong exactly
/// when the flavours differ — a dev laptop against a release host would attach
/// to an empty `thurbox-dev` server while the host's sessions sit on `thurbox`.
/// `session_ops::host_cli` records what the host said; [`host_socket`] and every
/// backend built for that host consult it at use, so a backend constructed at
/// startup follows the host once it has been asked.
fn learned_host_sockets() -> &'static Mutex<HashMap<String, String>> {
    static LEARNED: std::sync::OnceLock<Mutex<HashMap<String, String>>> =
        std::sync::OnceLock::new();
    LEARNED.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Record the socket a host's own CLI reported for itself. Ignored for a host
/// that pins `socket` in `hosts.toml` — the user's word wins.
pub fn learn_host_socket(host: &crate::session::HostDef, socket: &str) {
    if host.socket.is_some() || socket.is_empty() {
        return;
    }
    if let Ok(mut map) = learned_host_sockets().lock() {
        map.insert(host.backend_name(), socket.to_string());
    }
}

fn learned_host_socket(backend_name: &str) -> Option<String> {
    learned_host_sockets()
        .lock()
        .ok()
        .and_then(|map| map.get(backend_name).cloned())
}

/// The `-L` socket name a remote `host`'s multiplexer runs on: the host's
/// `socket` override, else what its own CLI reported, else the compile-time
/// default. Deliberately **not** this process's own local socket: a relocation
/// here moves *our* sessions, while the host's sessions live wherever the
/// thurbox on that host put them — which is what [`learn_host_socket`] records.
/// Single source of truth shared by [`TmuxBackend::from_host`] and the psmux
/// hook-signal rewrite (which must bake the socket into the command — psmux has
/// no `$TMUX`-style in-pane socket resolution to rely on).
pub fn host_socket(host: &crate::session::HostDef) -> String {
    host.socket
        .clone()
        .or_else(|| learned_host_socket(&host.backend_name()))
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

/// Prefix for a pane a *plugin* asked for, holding a program it named.
///
/// A third prefix rather than reusing `tbs-`: window discovery adopts panes by
/// prefix, and a plugin's pane must never be picked up as a session's anything.
pub(crate) const PROGRAM_WINDOW_PREFIX: &str = "tbp-";

/// Build the tmux window name for a plugin-owned program pane.
///
/// `owner` is a short **digest** of the owning plugin's path, computed by the
/// caller, not the path itself. Two reasons, and the first is a correctness one:
/// [`sanitize_window_name`] maps every character outside `[A-Za-z0-9_-]` to `_`,
/// so `plugins/90_watch.lua` and `plugins.90.watch.lua` would sanitize to the same
/// window and two plugins would share one program. The second is that a path is
/// long enough to make the window list unreadable.
///
/// Deterministic, which is the whole mechanism for finding the window again after
/// a restart — there is no stored pane id to go stale.
pub(crate) fn program_window_name(owner: &str, pane: &str) -> String {
    format!(
        "{PROGRAM_WINDOW_PREFIX}{}-{}",
        sanitize_window_name(owner),
        sanitize_window_name(pane)
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

/// Whether `pane_id` (`%N`) is alive and sits in a window named `window`.
///
/// The verification is what makes a *persisted* pane id safe to target: tmux
/// reuses pane numbers after a server restart, so a stored id can point at a
/// different window entirely (a shell, the heartbeat keeper). Checking the
/// window name catches that; a dead or reassigned id falls back to the name.
fn pane_matches_window(pane_id: &str, window: &str) -> bool {
    let out =
        local_mux_command(&["display-message", "-p", "-t", pane_id, "#{window_name}"]).output();
    matches!(out, Ok(o) if o.status.success()
        && String::from_utf8_lossy(&o.stdout).trim() == window)
}

/// Resolve the tmux target for a session's agent pane: the persisted pane id
/// when it still points at this session's own `tb-` window, else the window
/// name (the legacy path, for rows persisted with no pane id).
///
/// The pane id is the precise half — two sessions can share a name, and their
/// windows then share the `tb-<name>` target, which tmux resolves to an
/// arbitrary one of them. Every one-shot helper that acts on a session's pane
/// goes through here so the id wins whenever it is usable.
fn agent_target(session_name: &str, pane_id: &str) -> String {
    if !pane_id.is_empty() && pane_matches_window(pane_id, &agent_window_name(session_name)) {
        return pane_id.to_string();
    }
    window_target(session_name)
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
    let minor_str: String = parts[1].chars().take_while(char::is_ascii_digit).collect();
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

/// Delay between sending command text and pressing Enter via tmux, used by the
/// synchronous `send_prompt_now` path.
const SEND_KEYS_ENTER_DELAY: std::time::Duration = std::time::Duration::from_millis(200);

/// Hard cap on the number of scrollback lines `capture_pane_text` will return.
const MAX_CAPTURE_LINES: u32 = 10_000;

/// Longest agent activity line replayed from a pane title at adopt time.
///
/// A title is one line in the session list, and the value comes back from a
/// host: bounding it here means a pane whose title is a megabyte cannot make
/// the seed one.
const MAX_TITLE_SEED_BYTES: usize = 512;

/// A tmux backend — sessions persist in `tmux -L <socket>` on either the local
/// machine or a remote host reached over SSH.
///
/// Uses tmux control mode (`-C`) for all I/O after `ensure_ready()`. The only
/// thing that differs between local and remote is the [`TmuxTransport`] used to
/// launch the `tmux` process; the protocol layer is identical.
pub struct TmuxBackend {
    /// How `tmux` is launched (local `Command` vs `ssh <dest> tmux …`).
    transport: TmuxTransport,
    /// tmux socket name passed via `-L` (e.g. `thurbox`) as configured; read
    /// through [`Self::socket`], which prefers what the host's own CLI said.
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
        let socket = host_socket(host);
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

    /// The socket this backend talks to: the configured one, unless the host's
    /// own CLI has since reported a different one (see [`learn_host_socket`]).
    /// Resolved per call so a backend registered at startup follows the host.
    fn socket(&self) -> String {
        learned_host_socket(&self.name).unwrap_or_else(|| self.socket.clone())
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
            .tmux_command(&self.socket(), args)
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

        // Server-wide options every supported tmux understands. A failure here
        // means the server can't host sessions, so it is propagated.
        let server_opts = [
            ("default-terminal", "xterm-256color"),
            ("extended-keys", "on"),
        ];
        for (key, val) in &server_opts {
            self.tmux_run(&["set-option", "-s", key, val])?;
        }

        // `extended-keys-format csi-u` is best-effort: the option landed in tmux
        // 3.3, but thurbox's floor is 3.2, so an older tmux rejects it ("invalid
        // option"). It is advisory only — thurbox injects keystroke bytes directly
        // via `send-keys` (not through tmux's key forwarder), so it never
        // re-encodes what an agent receives; it just sets what `tmux show-options`
        // reports, which some agents (notably `pi`) probe at startup and warn about
        // unless it is `csi-u`. Ignoring the error keeps a 3.2 host working (pi
        // users there simply miss the hint) while 3.3+ hosts get the preferred
        // format.
        if let Err(e) = self.tmux_run(&["set-option", "-s", "extended-keys-format", "csi-u"]) {
            debug!("extended-keys-format=csi-u not set (likely tmux < 3.3): {e}");
        }

        self.apply_clipboard_config();

        // Session-level options
        for (key, val) in SESSION_OPTS {
            self.tmux_run(&["set-option", "-t", &self.session, key, val])?;
        }

        Ok(())
    }

    /// Open the two silent gates that would otherwise drop an OSC 52 clipboard
    /// write originating **inside** a pane (thurbox's own copy, or an agent's).
    ///
    /// Both are no-ops-on-failure by design, hence best-effort:
    ///
    /// 1. `set-clipboard` must be exactly `on`. tmux's `input_osc_52_parse`
    ///    bails on `!= 2`, and the shipped default is `external` (1) — which
    ///    forwards tmux's *own* copy-mode yanks but **discards** an
    ///    application's OSC 52 with no error and no visual artifact. This is
    ///    the default-broken case: without it every other part of the
    ///    clipboard path is dead under tmux.
    /// 2. The `Ms` terminfo capability must be present, or `tty_set_selection`
    ///    returns early — a second, independent silent drop. `terminal-features
    ///    ,*:clipboard` injects it for every terminal (tmux 3.2+, matching
    ///    thurbox's floor; the pre-3.2 form was a raw `terminal-overrides` Ms=
    ///    string).
    ///
    /// Security tradeoff: `set-clipboard on` lets any process in a pane set the
    /// user's system clipboard — an exfiltration channel, and why tmux moved
    /// the default to `external` in 2.6. Scoped here to thurbox's own socket,
    /// and the price of copy working at all over SSH.
    ///
    /// Skipped on psmux, which has no OSC 52 clipboard forwarding (a local
    /// Windows session copies via the native clipboard path instead).
    fn apply_clipboard_config(&self) {
        if self.transport.uses_psmux() {
            return;
        }
        for args in [
            ["set-option", "-s", "set-clipboard", "on"],
            ["set-option", "-as", "terminal-features", ",*:clipboard"],
        ] {
            if let Err(e) = self.tmux_run(&args) {
                debug!("clipboard option {} not set: {e}", args[2]);
            }
        }
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
                self.session,
                self.socket()
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

    /// The window command for a **remote/WSL** companion shell pane: the user's
    /// own login shell, interactively — the same environment an `ssh <host>`
    /// login gives you, not a bare `/bin/sh`.
    ///
    /// [`default_shell`](Self::default_shell) returns `/bin/sh` for a remote
    /// Unix host (guaranteed to exist), and the generic
    /// [`login_wrap_for_remote`] would run it as `/bin/sh -lc 'exec /bin/sh'` —
    /// a login-sourced but then bare POSIX shell. That drops everything a real
    /// SSH login loads from the account's shell: its rc files (`~/.bashrc` /
    /// `~/.zshrc`), prompt, aliases, functions, and `PATH` additions. SSH runs
    /// the shell recorded in the user's passwd entry (which `$SHELL` reflects),
    /// so we do the same: bootstrap through the always-present `/bin/sh -l`
    /// (which login-sources the profile and thus exports `$SHELL`), then `exec`
    /// `"$SHELL"` as a **login** shell — tmux gives it a PTY, so it's
    /// interactive and sources the interactive rc chain too. If `$SHELL` is
    /// unset/broken the guard falls back to a plain `/bin/sh -l` so the pane
    /// still opens.
    ///
    /// The fallback is a `command -v` **guard**, never `exec "$SHELL" -l
    /// 2>/dev/null || …`: bash (and zsh) decide interactivity from
    /// `isatty(stdin) && isatty(stderr)`, and an `exec … 2>/dev/null`
    /// redirection **persists** into the exec'd shell — with stderr no longer a
    /// TTY the shell starts **non-interactive** (no prompt, no rc files, no
    /// readline), which reads as a blank "not loading" pane. So we probe
    /// `$SHELL` with `command -v` (whose own `2>/dev/null` is harmless) and only
    /// then `exec` it with all three std streams still on the PTY.
    ///
    /// psmux (Windows) hosts keep [`default_shell`]'s `powershell` (no
    /// `/bin/sh`); local backends use the platform default directly.
    fn remote_shell_pane_command(&self) -> String {
        let inner = control_mode::shell_escape(
            "command -v \"$SHELL\" >/dev/null 2>&1 && exec \"$SHELL\" -l; exec /bin/sh -l",
        );
        format!("/bin/sh -lc {inner}")
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
        let mut ps = String::new();
        // Sort for a deterministic command (HashMap iteration order isn't).
        let mut pairs: Vec<_> = env.iter().collect();
        pairs.sort();
        for (k, v) in pairs {
            ps.push_str(&format!("Set-Item Env:{k} {}; ", ps_single_quote(v)));
        }
        ps.push_str(&format!("& {}", ps_single_quote(command)));
        for a in args {
            ps.push(' ');
            ps.push_str(&ps_single_quote(a));
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
        let fresh = ControlMode::start(&self.transport, &self.socket(), &self.session)?;
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
        // differently for it (see `control_mode::send_keys_commands`) and routes
        // a paste out of band (see `control_mode::PsmuxPaste`).
        let psmux = self.transport.uses_psmux();
        let paste =
            psmux.then(|| control_mode::PsmuxPaste::new(self.transport.clone(), self.socket()));
        self.with_control(|ctrl| {
            Ok(ControlModeWriter {
                stdin: Arc::clone(&ctrl.stdin),
                pane_id: pane_id.to_string(),
                psmux,
                paste: paste.clone(),
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

    /// Capture a pane's window title, scrollback history and visible screen as
    /// terminal bytes suitable for seeding a fresh vt100 parser.
    ///
    /// The control-mode `%output` stream only carries bytes emitted after the
    /// pane is connected, so an adopted session would otherwise start with an
    /// empty scrollback — the forced repaint restores the visible screen but
    /// not the history above it. `-e` keeps colors, `-J` rejoins wrapped lines
    /// so they re-wrap at the adopting panel's width, `-S -<n>` extends the
    /// capture into history (tmux clamps to what exists). The title rides
    /// along because the capture cannot carry it — see [`Self::pane_title_seed`].
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
        // Ahead of the history, not after it: the capture ends wherever the
        // pane's last line ended, and appending to a run that stopped mid
        // escape sequence would feed the parser a spliced one.
        let mut seed = self.pane_title_seed(pane_id);
        seed.extend(history_seed_bytes(output.stdout));
        Ok(seed)
    }

    /// The pane's window title replayed as an OSC 2, or empty when the pane
    /// has none worth restoring.
    ///
    /// Agents use the window title as their activity line — Claude Code writes
    /// the task it is on — and thurbox reads it off the PTY, so a restart that
    /// joins the stream mid-flight shows nothing until the agent next repaints
    /// it. tmux kept the value: `#{pane_title}` *is* the last OSC the pane
    /// emitted. Replaying it puts it back through the same callback a live
    /// title takes (`TermSignals`'s title callback), so nothing downstream
    /// learns a second way of being told.
    ///
    /// Best-effort by construction: a pane title is a nicety and the history
    /// beside it is not, so a mux that answers this differently (psmux is
    /// unverified here) loses the line rather than the scrollback.
    fn pane_title_seed(&self, pane_id: &str) -> Vec<u8> {
        // One query for both halves: a pane that never had a title set reads
        // back as the host's own short name, which is tmux's default rather
        // than anything an agent said.
        let out = match self.run_tmux(&[
            "display-message",
            "-p",
            "-t",
            pane_id,
            "#{host_short}|#{pane_title}",
        ]) {
            Ok(out) => out,
            Err(e) => {
                debug!(pane = %pane_id, "could not read pane title: {e:#}");
                return Vec::new();
            }
        };
        let line = String::from_utf8_lossy(&out.stdout);
        let Some((host, title)) = line.lines().next().and_then(|l| l.split_once('|')) else {
            return Vec::new();
        };
        title_seed_bytes(host, title)
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
            .tmux_command(&self.socket(), &["-V"])
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
                &self.socket(),
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
        // A remote/WSL companion shell pane (`tbs-` window) opens the user's own
        // interactive login shell — the SSH-login environment — instead of the
        // bare `/bin/sh` the generic login-wrap would produce (see
        // `remote_shell_pane_command`). Agent windows (`tb-`) and psmux hosts
        // keep the standard path.
        let is_remote_shell_pane =
            self.transport.is_remote() && !psmux && window_name.starts_with(SHELL_WINDOW_PREFIX);
        let shell_cmd = if psmux {
            Self::psmux_window_command(command, args, env)
        } else if is_remote_shell_pane {
            self.remote_shell_pane_command()
        } else {
            self.login_wrap_for_remote(&Self::build_shell_command(command, args))
        };

        // psmux's tokenizer can't read POSIX `'\''` escapes (see
        // `psmux_quote`), so its `-c`/`-n` values get the double-quote framing
        // it does parse; tmux keeps the byte-identical single-quote path.
        let quote_arg = |s: &str| {
            if psmux {
                control_mode::psmux_quote(s)
            } else {
                control_mode::shell_escape(s)
            }
        };
        let cwd_part = match cwd {
            Some(dir) => format!(" -c {}", quote_arg(&dir.to_string_lossy())),
            None => String::new(),
        };
        let env_part: String = if psmux {
            String::new()
        } else {
            env.iter()
                .map(|(k, v)| format!(" -e {}", shell_escape(&format!("{k}={v}"))))
                .collect()
        };
        let escaped_window_name = quote_arg(window_name);
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

    fn adopt(
        &self,
        backend_id: &str,
        rows: u16,
        cols: u16,
        seed: Option<Vec<u8>>,
    ) -> Result<AdoptedSession> {
        // backend_id comes from the shared DB — never interpolate it unvalidated.
        if !control_mode::is_valid_pane_id(backend_id) {
            bail!("refusing to adopt invalid pane id: {backend_id:?}");
        }
        // Opt-in split timing (THURBOX_PERF_LOG): the history capture is an
        // independent `tmux capture-pane` subprocess, while `connect_pane`
        // drives the serialized control-mode connection. Restore prefetches
        // the captures in parallel and passes them in (ADR-P9), so
        // `capture_ms` here reads 0 on that path; a `None` seed (a mid-run
        // adopt) still captures inline, before connecting so seeded history
        // can't duplicate live output. Best-effort: adoption must survive a
        // failed capture.
        let perf_log = std::env::var_os("THURBOX_PERF_LOG").is_some();

        let capture_start = perf_log.then(std::time::Instant::now);
        let seed = seed.unwrap_or_else(|| {
            self.capture_history(backend_id).unwrap_or_else(|e| {
                warn!("Failed to capture history for pane {backend_id}: {e}");
                Vec::new()
            })
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

    fn capture_history(&self, backend_id: &str) -> Result<Vec<u8>> {
        if !control_mode::is_valid_pane_id(backend_id) {
            bail!("refusing to capture invalid pane id: {backend_id:?}");
        }
        self.capture_history_seed(backend_id)
    }

    fn find_window(&self, window_name: &str) -> Result<Option<String>> {
        // The same listing `discover` reads, without its `tb-` filter and matched
        // exactly rather than by prefix — tmux's own name matching is FNMATCH-ish,
        // which would make `tbp-x-watch` findable by `tbp-x-watc`.
        let listing = self.tmux_output(&[
            "list-windows",
            "-t",
            &self.session,
            "-F",
            "#{pane_id}|#{window_name}|#{pane_dead}",
        ])?;
        for line in listing.lines() {
            let parts: Vec<&str> = line.splitn(3, '|').collect();
            if parts.len() < 3 || parts[1] != window_name {
                continue;
            }
            if parse_pane_dead(parts[2]) || !control_mode::is_valid_pane_id(parts[0]) {
                continue;
            }
            return Ok(Some(parts[0].to_string()));
        }
        Ok(None)
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

    fn pane_pids(&self) -> Result<HashMap<String, u32>> {
        let result = self.ctrl_command("list-panes -a -F '#{pane_id} #{pane_pid}'")?;
        Ok(control_mode::parse_pane_pids(&result))
    }

    fn shutdown(&self) {
        // Taking the connection out runs `ControlMode::drop` on the calling
        // thread, which is what lets quit fan the (blocking) teardown out
        // across backends. Idempotent: the mutex holds `None` afterwards, so
        // `TmuxBackend`'s own drop later is a no-op.
        //
        // `lock()` rather than `try_lock()`: a contended lock means another
        // thread is mid-command on this connection, and skipping the teardown
        // would leak the child + reader thread for the process lifetime.
        drop(self.control.lock().ok().and_then(|mut c| c.take()));
    }

    fn take_hook_state_events(&self) -> Vec<(String, String)> {
        // `try_lock`, not `lock`: this runs on the UI thread every tick, and a
        // background restore thread holds `control` across `ControlMode::start`
        // (an ssh connect + waited commands, up to tens of seconds on a slow
        // host) — blocking here would stall the first frame ADR-P7 protects.
        // A contended lock means no connection is serving events yet, and a
        // skipped drain only defers queued events to the next tick.
        self.control
            .try_lock()
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
    /// on a Unix/WSL host (the local `$SHELL` may not be installed there).
    /// Local backends keep the trait default's behavior.
    ///
    /// This is only the *bootstrap* for a remote Unix pane: `spawn` upgrades
    /// it to the user's own interactive login shell via
    /// `remote_shell_pane_command` so the pane matches an `ssh <host>` login
    /// (rc files, prompt, aliases, `PATH`).
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
/// Used by [`send_prompt_now`], which is how the TUI reaches this too — the
/// kernel's prompt commands call it rather than framing the paste themselves.
/// The trailing `Enter` is still sent separately by the caller. tmux delivers
/// these bytes literally via `send-keys -l`.
fn bracketed_paste(text: &str) -> String {
    format!("\x1b[200~{text}\x1b[201~")
}

/// The one-shot argv that delivers `text` into `target` as one paste.
///
/// tmux takes the bracketed-paste-wrapped bytes literally (`send-keys -l`).
/// psmux instead gets its own `send-paste`, which wraps and writes the payload
/// itself (see [`control_mode::PsmuxPaste`] for why key-encoded markers do not
/// survive there): a raw newline inside a psmux command argument is cut by the
/// server's line-oriented read, so a multi-line prompt arrived truncated *and*
/// its tail ran as a psmux command (psmux #560).
fn paste_prompt_args(target: &str, text: &str, psmux: bool) -> Vec<String> {
    if psmux {
        return vec![
            "send-paste".to_string(),
            "-t".to_string(),
            target.to_string(),
            base64::engine::general_purpose::STANDARD.encode(text.as_bytes()),
        ];
    }
    vec![
        "send-keys".to_string(),
        "-t".to_string(),
        target.to_string(),
        "-l".to_string(),
        bracketed_paste(text),
    ]
}

/// Whether a `#{pane_dead}` format string reports an exited pane.
///
/// Only the literal `1` means dead: `display-message` against a *missing*
/// window still exits 0 printing nothing, so an empty value must read as "not
/// dead" and leave the missing-window diagnosis to `send-keys`, which does
/// fail on it.
fn parse_pane_dead(output: &str) -> bool {
    output.trim() == "1"
}

/// Whether `target`'s pane has exited. The one-shot mirror of
/// [`TmuxBackend::is_dead`], which asks the same question over control mode.
///
/// Errors read as "not dead" so a tmux hiccup degrades to the previous
/// behavior (attempt the send) rather than silently dropping a prompt.
fn pane_is_dead(target: &str) -> bool {
    local_mux_command(&["display-message", "-p", "-t", target, "#{pane_dead}"])
        .output()
        .ok()
        .filter(|out| out.status.success())
        .map(|out| parse_pane_dead(&String::from_utf8_lossy(&out.stdout)))
        .unwrap_or(false)
}

/// Send text immediately to a session pane (no scheduling), followed by Enter.
///
/// The prompt-delivery shape every caller wants: [`send_text_now`] with the
/// Enter kept, which is the behaviour this had before the CLI needed to leave
/// text unsubmitted.
pub fn send_prompt_now(session_name: &str, pane_id: &str, text: &str) -> Result<()> {
    send_text_now(session_name, pane_id, text, true)
}

/// Type text into a session pane (no scheduling), submitting it or not.
///
/// Targets the session's pane via `agent_target` (pane id first, window name
/// as the legacy fallback). Submitting uses a "paste text → brief delay → press
/// Enter" sequence so the target app has time to process the pasted input.
///
/// `submit = false` types the text and stops: it lands in the agent's composer
/// unsent, which is what a "type it, check what the pane shows, then submit"
/// protocol needs — submitting on the way in fires every steer the instant it
/// is typed. [`send_key_now`] with `enter` is the other half.
///
/// The text goes out bracketed-paste-wrapped either way (see
/// `paste_prompt_args`), so it arrives literally: no shell is involved, and
/// the wrap is also what keeps a leading `-` from reading as a `send-keys`
/// flag and a newline from submitting the line before it.
///
/// Refuses a pane whose process has exited. Sessions run with
/// `remain-on-exit=on` (`SESSION_OPTS`), so a dead agent leaves its window in
/// place and `send-keys` still exits 0 while discarding the keystrokes. Every
/// caller reads that success as "the agent got it" — which is how the mailbox
/// wake came to report `woke: true` at a pane nothing was listening to — so the
/// liveness check belongs here, once, rather than in each of them.
pub fn send_text_now(session_name: &str, pane_id: &str, text: &str, submit: bool) -> Result<()> {
    let target = agent_target(session_name, pane_id);
    if pane_is_dead(&target) {
        bail!("session '{session_name}' has exited; its pane accepts no input");
    }
    let paste = paste_prompt_args(&target, text, DEFAULT_MUX == "psmux");
    let paste_argv: Vec<&str> = paste.iter().map(String::as_str).collect();
    let out = local_mux_command(&paste_argv)
        .output()
        .context("Failed to paste prompt text into the session pane")?;
    if !out.status.success() {
        bail!("{DEFAULT_MUX} {} {}", paste[0], mux_failure(&out));
    }

    if !submit {
        return Ok(());
    }

    std::thread::sleep(SEND_KEYS_ENTER_DELAY);

    let out = local_mux_command(&["send-keys", "-t", &target, "Enter"])
        .output()
        .context("Failed to send Enter to the session pane")?;
    if !out.status.success() {
        bail!("{DEFAULT_MUX} send-keys (Enter) {}", mux_failure(&out));
    }
    Ok(())
}

/// How a failed one-shot multiplexer command reads inside an error.
///
/// `output()` rather than `status()` at every call site above is the point: a
/// `status()` child inherits this process's stderr, so tmux's own `can't find
/// window: tb-<name>` lands there directly — a second, unstructured stream
/// beside the error document the CLI puts on stdout (AXI principle 6, "an
/// agent reads one stream"). Captured, the same sentence becomes part of the
/// one answer. tmux says nothing at all for some failures, hence the fallback
/// to the bare status.
fn mux_failure(out: &std::process::Output) -> String {
    let stderr = String::from_utf8_lossy(&out.stderr);
    let detail = stderr.trim();
    if detail.is_empty() {
        return format!("exited with status {}", out.status);
    }
    detail.to_string()
}

/// The special keys [`send_key_now`] can deliver, as
/// `(canonical spelling, tmux key name)`.
///
/// A closed table because tmux does **not** validate a key name: an
/// unrecognized one is typed into the pane as literal text, so a typo would
/// silently inject `Escpe` into an agent's prompt rather than fail. `ctrl-a` …
/// `ctrl-z` are resolved generically by [`resolve_key`] and deliberately not
/// listed here.
///
/// `enter`, `escape`, `tab`, `backspace` and `ctrl-<letter>` are also the set
/// psmux implements (see [`crate::agent::control_mode::send_keys_commands`]);
/// the rest are tmux-only, which is what a Windows host runs into.
pub const NAMED_KEYS: &[(&str, &str)] = &[
    ("enter", "Enter"),
    ("escape", "Escape"),
    ("tab", "Tab"),
    ("backspace", "BSpace"),
    ("space", "Space"),
    ("up", "Up"),
    ("down", "Down"),
    ("left", "Left"),
    ("right", "Right"),
    ("home", "Home"),
    ("end", "End"),
    ("page-up", "PageUp"),
    ("page-down", "PageDown"),
    // `DC` is tmux's own (terminfo-derived) name for Delete. Current tmux also
    // answers to `Delete` — both send `\x1b[3~` — but an older one that did not
    // would type the *name* into the pane rather than refuse it, so the table
    // names the conservative one.
    ("delete", "DC"),
];

/// Alternate spellings accepted for a canonical name in [`NAMED_KEYS`].
///
/// Forgiving on purpose — an integrator writing `esc` or `pgup` should not have
/// to look the table up — but every alias resolves to one canonical name, which
/// is what the CLI echoes back, so there is a single spelling to depend on.
const KEY_ALIASES: &[(&str, &str)] = &[
    ("return", "enter"),
    ("esc", "escape"),
    ("bspace", "backspace"),
    ("pageup", "page-up"),
    ("pgup", "page-up"),
    ("pagedown", "page-down"),
    ("pgdn", "page-down"),
    ("del", "delete"),
];

/// A key name resolved from what a caller spelled.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedKey {
    /// The canonical thurbox spelling (`ctrl-c`, `page-up`).
    pub name: String,
    /// The tmux key name to hand `send-keys` (`C-c`, `PageUp`).
    pub tmux: String,
}

/// Resolve a caller's key spelling, or `None` for one thurbox does not know.
///
/// Case-insensitive, and `ctrl-c`, `ctrl+c`, `C-c` and `c+c` are all the same
/// key — the separator and the `ctrl`/`c` prefix are the two things people
/// actually spell differently, and half-supporting them would mean a typo lands
/// as text in an agent's prompt.
pub fn resolve_key(input: &str) -> Option<ResolvedKey> {
    let lower = input.trim().to_ascii_lowercase();
    let lower = KEY_ALIASES
        .iter()
        .find(|(alias, _)| *alias == lower)
        .map_or(lower.as_str(), |(_, canonical)| *canonical);
    if let Some((name, tmux)) = NAMED_KEYS.iter().find(|(name, _)| *name == lower) {
        return Some(ResolvedKey {
            name: (*name).to_string(),
            tmux: (*tmux).to_string(),
        });
    }
    let rest = ["ctrl-", "ctrl+", "c-", "c+"]
        .iter()
        .find_map(|prefix| lower.strip_prefix(prefix))?;
    let mut chars = rest.chars();
    let letter = chars.next().filter(char::is_ascii_lowercase)?;
    if chars.next().is_some() {
        return None;
    }
    Some(ResolvedKey {
        name: format!("ctrl-{letter}"),
        tmux: format!("C-{letter}"),
    })
}

/// Send one named special key to a session pane — no text, no Enter.
///
/// `tmux_key` is a [`ResolvedKey::tmux`] name, never a caller's string: tmux
/// types an unrecognized name into the pane instead of refusing it.
///
/// Refuses a dead pane for the same reason [`send_text_now`] does — `send-keys`
/// exits 0 into a `remain-on-exit` corpse, so success would be a lie.
pub fn send_key_now(session_name: &str, pane_id: &str, tmux_key: &str) -> Result<()> {
    let target = agent_target(session_name, pane_id);
    if pane_is_dead(&target) {
        bail!("session '{session_name}' has exited; its pane accepts no input");
    }
    let out = local_mux_command(&["send-keys", "-t", &target, tmux_key])
        .output()
        .context("Failed to send a key to the session pane")?;
    if !out.status.success() {
        bail!("{DEFAULT_MUX} send-keys ({tmux_key}) {}", mux_failure(&out));
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
        .map(str::to_string)
        .collect()
}

/// Whether the session's agent pane currently exists in the thurbox tmux
/// server — its persisted pane id when one is usable, the `tb-<session_name>`
/// window otherwise. Used by the headless dispatcher to skip `send`
/// automations whose target session is no longer running rather than failing
/// into a dead pane.
pub fn window_exists(session_name: &str, pane_id: &str) -> bool {
    let want = agent_window_name(session_name);
    (!pane_id.is_empty() && pane_matches_window(pane_id, &want))
        || list_window_names().contains(&want)
}

/// Schedule a one-shot prompt delivery into a session's window after
/// `delay_secs`, via a detached `tmux run-shell` timer.
///
/// Used by the headless automation dispatcher to deliver a Spawn automation's
/// prompt once the freshly launched agent CLI has had time to boot — offline
/// there is no TUI deferred-input queue to lean on. Local-tmux scoped.
pub fn send_prompt_after_delay(
    session_name: &str,
    pane_id: &str,
    text: &str,
    delay_secs: u64,
) -> Result<()> {
    let target = agent_target(session_name, pane_id);
    let script = deferred_prompt_script(&target, text);
    let out = local_mux_command(&["run-shell", "-b", "-d", &delay_secs.to_string(), &script])
        .output()
        .context("Failed to schedule tmux run-shell for deferred prompt")?;
    if !out.status.success() {
        bail!("tmux run-shell (deferred prompt) {}", mux_failure(&out));
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
///
/// The prompt travels as psmux's own base64 `send-paste` payload (see
/// [`paste_prompt_args`]) — which also keeps the script free of the prompt's
/// newlines and quotes.
#[cfg(windows)]
fn deferred_prompt_script(target: &str, text: &str) -> String {
    let t = ps_single_quote(target);
    let socket = local_socket();
    let payload = base64::engine::general_purpose::STANDARD.encode(text.as_bytes());
    format!(
        "powershell -NoProfile -Command \"{DEFAULT_MUX} -L {socket} send-paste -t {t} {payload}; \
         Start-Sleep -Milliseconds 200; \
         {DEFAULT_MUX} -L {socket} send-keys -t {t} Enter\""
    )
}

/// Wrap `s` in a PowerShell single-quoted literal — the shared
/// [`crate::shell::powershell_quote`], under this file's historical name. Not
/// `#[cfg(windows)]`: `psmux_window_powershell` quotes for a psmux *host* from
/// any local OS.
fn ps_single_quote(s: &str) -> String {
    crate::shell::powershell_quote(s)
}

/// Ensure the automation heartbeat keeper window is running.
///
/// Creates a detached tmux window that loops `<cli_path> automation tick` every
/// `HEARTBEAT_INTERVAL_SECS` seconds, so automations fire even with no TUI
/// attached. The live window also keeps the tmux server alive, so spawn-only
/// automations work with no other sessions. Idempotent — a no-op when the
/// keeper already exists. `cli_path` is the absolute path to `thurbox-cli`.
///
/// Whether the automation heartbeat keeper window is running right now.
///
/// The keeper is created implicitly by anything that arms an automation, is not
/// a session, and so appears in no session listing. That made it the one thing
/// thurbox puts on a tmux server that nothing could see or reclaim; this and
/// [`stop_automation_heartbeat`] are what make it accountable.
pub fn automation_heartbeat_running() -> bool {
    list_window_names().iter().any(|w| w == HEARTBEAT_WINDOW)
}

/// Stop the heartbeat keeper. Returns whether there was one to stop.
///
/// Automations stop firing headlessly until something arms it again — which any
/// `automation` write does, so this is a pause rather than a removal.
pub fn stop_automation_heartbeat() -> bool {
    if !automation_heartbeat_running() {
        return false;
    }
    let target = format!("{TMUX_SESSION}:{HEARTBEAT_WINDOW}");
    local_mux_command(&["kill-window", "-t", &target])
        .output()
        .map(|out| out.status.success())
        .unwrap_or(false)
}

pub fn ensure_automation_heartbeat(cli_path: &Path) -> Result<()> {
    TmuxBackend::local().ensure_session_configured()?;
    if list_window_names().iter().any(|w| w == HEARTBEAT_WINDOW) {
        return Ok(());
    }
    let loop_cmd = heartbeat_loop_command(cli_path);
    let out = local_mux_command(&[
        "new-window",
        "-d",
        "-t",
        TMUX_SESSION,
        "-n",
        HEARTBEAT_WINDOW,
        &loop_cmd,
    ])
    .output()
    .context("Failed to create automation heartbeat window")?;
    if !out.status.success() {
        bail!("tmux new-window (heartbeat) {}", mux_failure(&out));
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
/// psmux's trailing-token handling entirely (same delivery and
/// `ps_single_quote` quoting as `psmux_window_powershell`). This used to be a
/// no-op ("no POSIX shell for the keeper loop"),
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
        if exe.file_name().and_then(std::ffi::OsStr::to_str) == Some(cli_name.as_str()) {
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
/// With `ansi`, tmux emits the styling escape sequences too (`capture-pane
/// -e`) instead of flattening the screen to plain text.
pub fn capture_pane_text(
    session_name: &str,
    pane_id: &str,
    lines: u32,
    ansi: bool,
) -> Result<String> {
    let target = agent_target(session_name, pane_id);
    let lines = lines.min(MAX_CAPTURE_LINES);
    let start = format!("-{lines}");

    let mut args = vec!["capture-pane", "-p", "-J", "-t", &target, "-S", &start];
    if ansi {
        args.push("-e");
    }
    let output = local_mux_command(&args)
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

/// The OSC 2 that restores `title` as a pane's window title, or empty when
/// there is nothing to restore.
///
/// Suppressed for a title equal to `host_short`, which is what tmux seeds a
/// pane with and therefore means "no agent ever set one" — replaying it would
/// put a hostname in the session list where the activity line goes. Control
/// characters are dropped because the value is remote-controlled text and the
/// sequence is terminated by one.
fn title_seed_bytes(host_short: &str, title: &str) -> Vec<u8> {
    let title = title.trim();
    if title.is_empty() || title == host_short.trim() {
        return Vec::new();
    }
    let mut text = String::new();
    for c in title.chars().filter(|c| !c.is_control()) {
        if text.len() + c.len_utf8() > MAX_TITLE_SEED_BYTES {
            break;
        }
        text.push(c);
    }
    let text = text.trim_end();
    if text.is_empty() {
        return Vec::new();
    }
    format!("\x1b]2;{text}\x1b\\").into_bytes()
}

/// A session pane's live state *around* its rendered text: where the cursor
/// sits, what is running in the foreground of its tty, and where that process
/// thinks it is.
///
/// Every field is independently optional and never guessed. A multiplexer that
/// does not answer a format (psmux expands an unknown `#{…}` to nothing), a
/// pane that has gone away between the capture and this call, or a platform
/// with no `ps` each leave the affected fields `None` rather than a plausible
/// wrong value — the caller can then say "unknown" instead of acting on a
/// fabrication.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct PaneState {
    /// Cursor row, 0-based, relative to the visible pane (`#{cursor_y}`).
    pub cursor_row: Option<u32>,
    /// Cursor column, 0-based (`#{cursor_x}`).
    pub cursor_col: Option<u32>,
    /// The foreground process's argv0 — its executable as invoked.
    ///
    /// Resolved from the tty's foreground process group where that is possible,
    /// falling back to tmux's `#{pane_current_command}`. The two agree in the
    /// common case; [`foreground_command`](Self::foreground_command) is what
    /// says which one this is.
    pub foreground_process: Option<String>,
    /// The foreground process's **full** command line.
    ///
    /// `Some` only when the process group was really resolved, which is also
    /// what makes this the field worth reading: a Node-based agent CLI is a
    /// bare `node` in every command-*name* view, and only its argv distinguishes
    /// `node …/cursor-agent/cli.js` from a REPL.
    pub foreground_command: Option<String>,
    /// The pane's live working directory (`#{pane_current_path}`) — where the
    /// foreground process is, not the directory the session was launched in.
    pub foreground_cwd: Option<String>,
    /// Whether the pane's command has **exited** (`#{pane_dead}`).
    ///
    /// The backend runs with `remain-on-exit=on`, so a dead pane keeps its
    /// frame — and keeps answering `#{pane_current_command}` with whatever last
    /// ran there. Without this, an agent that crashed reports its own name as
    /// the foreground process: a plausible wrong answer rather than an honest
    /// absence, which is exactly what a caller reconciling a latched state
    /// against reality must not be handed.
    pub dead: Option<bool>,
}

/// Separator for the one-shot `display-message` that reads a pane's whole
/// state. ASCII unit separator: paths and command names may contain spaces,
/// tabs and newlines, so a whitespace delimiter would split a value in half.
///
/// Keeping it intact costs a flag — see [`PANE_STATE_UTF8_FLAG`] — and an
/// alternate spelling — see [`PANE_STATE_SEP_ESCAPED`].
const PANE_STATE_SEP: char = '\x1f';

/// How tmux 3.4 and older spell [`PANE_STATE_SEP`] back.
///
/// Those versions run every `display-message -p` answer through `vis(3)`
/// (`VIS_OCTAL|VIS_CSTYLE|VIS_NOSLASH`) *before* the UTF-8 check, so a control
/// byte comes back as its printable octal escape whatever
/// [`PANE_STATE_UTF8_FLAG`] says: the separator arrives as the four characters
/// `\037` and the whole answer then parses as one field, reporting every pane
/// field null. tmux 3.5 dropped that pass and prints the byte itself. Both
/// spellings are accepted so one parser covers every tmux in the field —
/// ubuntu-24.04, which CI runs on, still ships 3.4.
const PANE_STATE_SEP_ESCAPED: &str = "\\037";

/// tmux's `-u` — "assume the terminal supports UTF-8".
///
/// tmux decides a client speaks UTF-8 from `LC_ALL`/`LC_CTYPE`/`LANG`, and
/// sanitizes what it prints for one that does not: every control byte becomes
/// `_`, [`PANE_STATE_SEP`] included. Under `LC_ALL=C` or no locale at all — a
/// systemd unit, a cron job, most containers — the whole answer then parses as
/// one field and every pane-state field reports null. `-u` sets the flag
/// outright, so the separator survives whatever the environment says.
/// psmux is excluded: it has no such sanitizing and need not know the flag.
const PANE_STATE_UTF8_FLAG: &str = "-u";

/// Read a session pane's cursor position, foreground process and live cwd.
///
/// Best-effort by construction — see [`PaneState`]. One `display-message` for
/// everything tmux knows, plus at most one `ps` to turn the cheap command
/// *name* into the foreground process's argv. `pane_id` is the session's
/// persisted `backend_id`, resolved the same way [`capture_pane_text`] resolves
/// it, so the state describes the pane the capture came from.
pub fn pane_state(session_name: &str, pane_id: &str) -> PaneState {
    let target = agent_target(session_name, pane_id);
    let format = [
        "#{cursor_y}",
        "#{cursor_x}",
        "#{pane_current_command}",
        "#{pane_current_path}",
        "#{pane_tty}",
        "#{pane_dead}",
        "#{window_name}",
    ]
    .join(&PANE_STATE_SEP.to_string());

    let mut argv = Vec::with_capacity(6);
    if DEFAULT_MUX != "psmux" {
        argv.push(PANE_STATE_UTF8_FLAG);
    }
    argv.extend(["display-message", "-p", "-t", &target, &format]);

    let Some(raw) = local_mux_command(&argv)
        .output()
        .ok()
        .filter(|out| out.status.success())
        .map(|out| String::from_utf8_lossy(&out.stdout).into_owned())
    else {
        return PaneState::default();
    };

    let (mut state, tty, window) = parse_pane_state(&raw);
    // `display-message` against a target it cannot resolve does **not** fail:
    // it answers for the client's current pane and exits 0. Reporting a
    // stranger's shell as this session's foreground process is the plausible
    // wrong answer every field here is built to avoid, so the answer is only
    // kept when it demonstrably came from this session's own window.
    if window.as_deref() != Some(agent_window_name(session_name).as_str()) {
        return PaneState::default();
    }
    if let Some((argv0, command)) = tty.as_deref().and_then(foreground_process_on_tty) {
        state.foreground_process = Some(argv0);
        state.foreground_command = Some(command);
    }
    state
}

/// Split one `display-message` answer into a [`PaneState`], the pane's tty, and
/// the name of the window it actually came from.
///
/// An empty field is `None`, not an empty string: tmux prints nothing for a
/// format it cannot expand, and "" would read downstream as a real answer.
fn parse_pane_state(raw: &str) -> (PaneState, Option<String>, Option<String>) {
    let trimmed = raw.trim_end_matches(['\n', '\r']);
    let line = if trimmed.contains(PANE_STATE_SEP_ESCAPED) {
        Cow::Owned(trimmed.replace(PANE_STATE_SEP_ESCAPED, &PANE_STATE_SEP.to_string()))
    } else {
        Cow::Borrowed(trimmed)
    };
    let mut fields = line.split(PANE_STATE_SEP);
    let mut next = || fields.next().filter(|f| !f.is_empty());

    let cursor_row = next().and_then(|f| f.parse().ok());
    let cursor_col = next().and_then(|f| f.parse().ok());
    let command = next().map(str::to_string);
    let cwd = next().map(str::to_string);
    let tty = next().map(str::to_string);
    // `1`/`0`; anything else (a multiplexer that does not know the format) is
    // an absent answer, not a live pane.
    let dead = next().and_then(|f| match f {
        "1" => Some(true),
        "0" => Some(false),
        _ => None,
    });

    let window = next().map(str::to_string);

    (
        PaneState {
            cursor_row,
            cursor_col,
            foreground_process: command,
            foreground_command: None,
            foreground_cwd: cwd,
            dead,
        },
        tty,
        window,
    )
}

/// The `(argv0, full command line)` of `tty`'s foreground process group.
///
/// One `ps` listing every process on the tty: each row carries the tty's
/// foreground process group id (`tpgid`), so the rows whose own `pgid` equals
/// it *are* the foreground job, and the group leader is its command. Asking
/// `ps` for `tpgid` directly (rather than opening the tty and calling
/// `tcgetpgrp`) keeps this to a subprocess that works the same on Linux and
/// macOS, and leaves the tty untouched.
fn foreground_process_on_tty(tty: &str) -> Option<(String, String)> {
    // Both procps and BSD `ps` take the bare name; the `/dev/` prefix tmux
    // reports is accepted by neither uniformly.
    let name = tty.strip_prefix("/dev/").unwrap_or(tty);
    let out = Command::new("ps")
        .args(["-o", "pid=,pgid=,tpgid=,args=", "-t", name])
        .output()
        .ok()
        .filter(|out| out.status.success())?;
    parse_ps_foreground(&String::from_utf8_lossy(&out.stdout))
}

/// Pick the foreground job out of `ps -o pid=,pgid=,tpgid=,args= -t <tty>`.
///
/// The group *leader* (`pid == pgid`) is preferred over the rest of its
/// pipeline, so a `node … | tee` reports the node. A `tpgid` of `-1` means no
/// foreground group (nothing has the tty), and `0` is `ps` reporting it does
/// not know — neither is a process, so both yield nothing.
fn parse_ps_foreground(out: &str) -> Option<(String, String)> {
    let mut leader: Option<(String, String)> = None;
    let mut member: Option<(String, String)> = None;

    for line in out.lines() {
        let Some((pid, pgid, tpgid, args)) = parse_ps_row(line) else {
            continue;
        };
        if tpgid <= 0 || pgid != tpgid || args.is_empty() {
            continue;
        }
        let argv0 = args.split_whitespace().next().unwrap_or(args).to_string();
        let found = (argv0, args.to_string());
        if pid == pgid {
            leader.get_or_insert(found);
        } else {
            member.get_or_insert(found);
        }
    }
    leader.or(member)
}

/// One `ps` row: three numeric columns then the command line.
///
/// Split by hand rather than with `splitn`, because `ps` right-aligns its
/// numeric columns — a narrow pid beside a wide one is padded with *several*
/// spaces, which `splitn` hands back as empty fields.
fn parse_ps_row(line: &str) -> Option<(i64, i64, i64, &str)> {
    let mut rest = line.trim_start();
    let mut nums = [0i64; 3];
    for slot in &mut nums {
        let end = rest.find(char::is_whitespace)?;
        *slot = rest[..end].parse().ok()?;
        rest = rest[end..].trim_start();
    }
    Some((nums[0], nums[1], nums[2], rest.trim_end()))
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
/// streams. Returns the new pane's id (`%N`) on success; the command runs
/// inside it. Window name is `tb-<session_name>` — which is *not* unique (two
/// sessions can share a name), so the returned id is what callers persist as
/// `backend_id` and target thereafter.
///
/// On Windows the local mux is psmux, whose `new-window -P -F` support is
/// unverified against the documented divergences (ADR-13) — there the id is
/// not asked for and an empty string is returned, preserving the
/// resolve-by-name behavior until the e2e probes cover it.
pub fn spawn_window(
    session_name: &str,
    command: &str,
    args: &[String],
    cwd: Option<&Path>,
    env: &HashMap<String, String>,
) -> Result<String> {
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
    if !cfg!(windows) {
        tmux.args(["-P", "-F", "#{pane_id}"]);
    }
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
    if cfg!(windows) {
        return Ok(String::new());
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// Headless spawn of an agent window on a remote host over SSH.
///
/// Returns the remote tmux pane id (`%N`), like the local [`spawn_window`] —
/// but by driving the SSH backend's control mode rather than `new-window -P`.
/// The control-mode connection is dropped when this returns; the remote tmux
/// keeps the window alive for the TUI to adopt later.
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

/// One-shot read of every pane's remote-hook state option on `host`:
/// `list-panes -s -t <session> -F "#{pane_id} #{@thurbox_state}"` over the
/// host launcher, parsed to the set `(pane_id, state)` pairs. The headless
/// status poll (`session_ops::remote_hooks::poll_remote_hook_states`) uses it
/// to keep remote hook states flowing with no TUI attached. Read-only by
/// design — no `ensure_ready`, so a poll never creates the remote
/// server/session; an unreachable host or absent server is an `Err` the
/// caller treats as "no reports this cycle".
pub fn list_remote_hook_states(host: &crate::session::HostDef) -> Result<Vec<(String, String)>> {
    list_hook_states_on(&TmuxBackend::from_host(host))
}

/// [`list_remote_hook_states`] for this machine's own server: the pane option
/// a session created *from afar* on this host sets (its hooks were rewritten to
/// that form), which nothing here read before sessions were shared.
pub fn list_local_hook_states() -> Result<Vec<(String, String)>> {
    list_hook_states_on(&TmuxBackend::local())
}

fn list_hook_states_on(backend: &TmuxBackend) -> Result<Vec<(String, String)>> {
    if !backend.session_exists() {
        return Ok(Vec::new());
    }
    let session = backend.session.clone();
    let format = format!(
        "#{{pane_id}} #{{{}}}",
        crate::session::REMOTE_HOOK_STATE_OPTION
    );
    let body = backend.tmux_output(&["list-panes", "-s", "-t", &session, "-F", &format])?;
    Ok(control_mode::parse_pane_hook_states(&body))
}

/// The live pane of the agent window a session's name produces, on the local
/// server or on `host` — `None` when the window is absent, its pane has died,
/// or the name is ambiguous (two windows; keystrokes to the wrong agent are
/// worse than none). A one-shot listing, no control mode: this is asked by the
/// headless relaunch paths.
pub fn agent_window_pane(
    host: Option<&crate::session::HostDef>,
    session_name: &str,
) -> Result<Option<String>> {
    let backend = match host {
        Some(host) => TmuxBackend::from_host(host),
        None => TmuxBackend::local(),
    };
    let window = agent_window_name(session_name);
    let mut panes = backend
        .discover()?
        .into_iter()
        .filter(|w| w.name == window && w.is_alive)
        .map(|w| w.backend_id);
    let first = panes.next();
    Ok(match (first, panes.next()) {
        (Some(pane), None) => Some(pane),
        _ => None,
    })
}

/// Whether a session's agent window is alive — see [`agent_window_pane`].
pub fn agent_window_alive(
    host: Option<&crate::session::HostDef>,
    session_name: &str,
) -> Result<bool> {
    Ok(agent_window_pane(host, session_name)?.is_some())
}

/// Record a hook state on the pane this process runs in — the pane option a
/// remote observer's control-mode subscription reads — so a status reported
/// through the CLI reaches a peer within a second, not at the mirror's
/// cadence. `$TMUX` is `<socket path>,<pid>,<session index>`; the socket is
/// addressed by path (`-S`) because it is whichever server the pane is on,
/// which need not be this build's own. Silently nothing outside tmux.
pub fn set_own_pane_state(state: &str) -> Result<()> {
    let (Some(tmux), Some(pane)) = (
        std::env::var_os("TMUX").map(|s| s.to_string_lossy().into_owned()),
        std::env::var_os("TMUX_PANE").map(|s| s.to_string_lossy().into_owned()),
    ) else {
        return Ok(());
    };
    let Some(socket_path) = own_socket_path(&tmux) else {
        return Ok(());
    };
    if !control_mode::is_valid_pane_id(&pane) {
        return Ok(());
    }
    let status = Command::new(DEFAULT_MUX)
        .args([
            "-S",
            &socket_path,
            "set-option",
            "-p",
            "-t",
            &pane,
            crate::session::REMOTE_HOOK_STATE_OPTION,
            state,
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .context("run tmux set-option on the own pane")?;
    if !status.success() {
        bail!("tmux set-option exited {status}");
    }
    Ok(())
}

/// The socket path in a `$TMUX` value (`<path>,<pid>,<index>`).
pub(crate) fn own_socket_path(tmux_env: &str) -> Option<String> {
    let path = tmux_env.split(',').next()?.trim();
    (!path.is_empty()).then(|| path.to_string())
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

/// Kill the session's tmux window if it exists — by its persisted pane id when
/// one is usable (precise even when another session shares the name), by the
/// `tb-<session_name>` window name otherwise.
pub fn kill_window(session_name: &str, pane_id: &str) -> Result<()> {
    let target = agent_target(session_name, pane_id);
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
pub fn window_pane_pid(session_name: &str, pane_id: &str) -> Result<Option<u32>> {
    let target = agent_target(session_name, pane_id);
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
        decode_octal, format_send_keys, parse_notification, shell_escape, Notification,
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
    fn the_own_socket_path_is_the_first_field_of_tmux_env() {
        assert_eq!(
            own_socket_path("/tmp/tmux-1000/thurbox,4242,0").as_deref(),
            Some("/tmp/tmux-1000/thurbox")
        );
        assert_eq!(own_socket_path(",1,0"), None);
        assert_eq!(own_socket_path(""), None);
    }

    #[test]
    fn a_learned_socket_is_used_unless_the_host_pins_one() {
        let learned = crate::session::HostDef {
            name: "learned-socket-host".into(),
            destination: "me@h".into(),
            ..Default::default()
        };
        assert_eq!(host_socket(&learned), TMUX_SOCKET);
        learn_host_socket(&learned, "thurbox");
        assert_eq!(host_socket(&learned), "thurbox");
        let pinned = crate::session::HostDef {
            name: "pinned-socket-host".into(),
            destination: "me@h".into(),
            socket: Some("mine".into()),
            ..Default::default()
        };
        learn_host_socket(&pinned, "thurbox");
        assert_eq!(host_socket(&pinned), "mine");
    }

    #[test]
    fn a_default_instance_keeps_the_build_socket() {
        // The backwards-compatibility guarantee: nothing about an operator's
        // existing instance moves, including one whose `THURBOX_DATA_DIR`
        // merely restates the default (which is what thurbox injects into
        // every session it spawns).
        assert_eq!(socket_for(None, None, None, None), TMUX_SOCKET);
    }

    #[test]
    fn a_relocated_instance_gets_its_own_socket() {
        let lab = socket_for(
            None,
            None,
            Some(Path::new("/tmp/lab/data")),
            Some(Path::new("/tmp/lab/data")),
        );
        let other = socket_for(
            None,
            None,
            Some(Path::new("/tmp/other/data")),
            Some(Path::new("/tmp/other/data")),
        );
        assert_ne!(lab, TMUX_SOCKET, "a relocated instance leaves the default");
        assert_ne!(other, lab, "two of them do not share a server");
        assert_eq!(
            lab,
            socket_for(
                None,
                None,
                Some(Path::new("/tmp/lab/data")),
                Some(Path::new("/tmp/lab/data"))
            ),
            "and it finds the same server on the next run"
        );
        assert!(
            lab.starts_with(TMUX_SOCKET),
            "still recognisable as thurbox's: {lab}"
        );
        assert!(
            lab.chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.')),
            "safe to splice into a `-L` argument: {lab}"
        );
    }

    #[test]
    fn a_relocated_socket_ignores_separator_noise() {
        // One directory named two ways is one instance — otherwise a script
        // with a trailing slash would strand the sessions of one without it.
        assert_eq!(
            socket_for(
                None,
                None,
                Some(Path::new("/tmp/lab/data")),
                Some(Path::new("/tmp/lab/data"))
            ),
            socket_for(
                None,
                None,
                Some(Path::new("/tmp/lab/./data/")),
                Some(Path::new("/tmp/lab/./data/"))
            ),
        );
    }

    #[test]
    fn an_explicit_socket_wins_over_the_derivation() {
        assert_eq!(
            socket_for(
                Some("thurbox-named".into()),
                None,
                Some(Path::new("/tmp/lab")),
                Some(Path::new("/tmp/lab"))
            ),
            "thurbox-named"
        );
        // Empty is unset, and then the relocation still applies.
        assert_eq!(
            socket_for(
                Some(String::new()),
                None,
                Some(Path::new("/tmp/lab")),
                Some(Path::new("/tmp/lab"))
            ),
            socket_for(
                None,
                None,
                Some(Path::new("/tmp/lab")),
                Some(Path::new("/tmp/lab"))
            )
        );
    }

    #[test]
    fn an_inherited_socket_is_dropped_once_the_data_dir_moves() {
        let lab = Path::new("/tmp/lab");
        let home = Path::new("/home/me/.local/share/thurbox");
        // What a pane carries: the spawning instance's socket, tagged with the
        // data dir it belongs to. A child that stays put keeps it...
        assert_eq!(
            socket_for(Some("thurbox".into()), Some(home), Some(home), None),
            "thurbox"
        );
        // ...and one that relocates itself does not: the tag no longer names
        // where this instance's database is, so the name is somebody else's
        // server and the derivation has to run instead.
        assert_eq!(
            socket_for(Some("thurbox".into()), Some(home), Some(lab), Some(lab)),
            derived_socket(lab)
        );
        // An override with no tag at all is an operator naming a server
        // outright, which still wins over everything.
        assert_eq!(
            socket_for(Some("thurbox-named".into()), None, Some(lab), Some(lab)),
            "thurbox-named"
        );
    }

    #[test]
    fn local_socket_honors_env_override() {
        // nextest runs one process per test, so env mutation can't race other
        // tests reading `local_socket()`.
        //
        // The owner tag has to go first. `cargo test` runs inside a live
        // thurbox session on any developer machine, and that session injects
        // the pair — so an inherited `THURBOX_SOCKET_FOR` naming the operator's
        // data dir would make the override below read as inherited rather than
        // typed, and `local_socket()` would derive a socket instead of
        // honouring it.
        std::env::remove_var(SOCKET_OWNER_ENV);
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
    fn remote_shell_pane_opens_users_login_shell() {
        // The companion shell pane on a remote/WSL host should give the user
        // their own interactive login shell (the SSH-login environment: rc
        // files, prompt, aliases, PATH) — not the bare `/bin/sh` the generic
        // login-wrap would produce. Bootstrap through the always-present
        // `/bin/sh -l` (exports `$SHELL`), then `exec "$SHELL" -l`.
        //
        // Crucially the `$SHELL` probe is a `command -v` guard, NOT
        // `exec "$SHELL" -l 2>/dev/null`: an `exec … 2>/dev/null` redirection
        // persists into the exec'd shell, drops stderr off the TTY, and bash/zsh
        // then start non-interactive (no prompt) — a blank pane.
        const EXPECT: &str =
            "/bin/sh -lc 'command -v \"$SHELL\" >/dev/null 2>&1 && exec \"$SHELL\" -l; exec /bin/sh -l'";
        let ssh = TmuxBackend::from_host(&crate::session::HostDef {
            name: "devbox".into(),
            destination: "me@devbox".into(),
            ..Default::default()
        });
        assert_eq!(ssh.remote_shell_pane_command(), EXPECT);

        let wsl = TmuxBackend::from_host(&crate::session::HostDef::wsl("Ubuntu"));
        assert_eq!(wsl.remote_shell_pane_command(), EXPECT);

        // The interactive shell must keep stderr on the PTY — a stray
        // `exec … 2>` would make it non-interactive.
        assert!(!EXPECT.contains("-l 2>"));
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

    // --- one-shot prompt delivery ---

    #[test]
    fn paste_prompt_args_wraps_literally_for_tmux() {
        assert_eq!(
            paste_prompt_args("thurbox:tb-demo", "line one\nline two", false),
            vec![
                "send-keys",
                "-t",
                "thurbox:tb-demo",
                "-l",
                "\x1b[200~line one\nline two\x1b[201~",
            ]
        );
    }

    /// psmux gets its own `send-paste`: the bracketed markers are psmux's to add,
    /// and the base64 payload keeps the prompt's newlines off a command wire that
    /// would otherwise cut the line and run the tail as a command (psmux #560).
    #[test]
    fn paste_prompt_args_uses_send_paste_for_psmux() {
        let args = paste_prompt_args("thurbox:tb-demo", "line one\nline two", true);
        assert_eq!(
            args,
            vec![
                "send-paste",
                "-t",
                "thurbox:tb-demo",
                "bGluZSBvbmUKbGluZSB0d28=",
            ]
        );
        assert!(!args.iter().any(|a| a.contains('\n') || a.contains('\x1b')));
    }

    // --- named keys ---

    #[test]
    fn resolve_key_maps_the_named_table_to_tmux_names() {
        for (name, tmux) in NAMED_KEYS {
            let resolved = resolve_key(name).expect("a listed key must resolve");
            assert_eq!(resolved.name, *name);
            assert_eq!(resolved.tmux, *tmux);
        }
    }

    #[test]
    fn resolve_key_accepts_the_spellings_people_write() {
        // Separator, prefix and case are the three things spelled differently;
        // all four forms are the one key, and the canonical name is what the
        // caller gets back to depend on.
        for spelling in ["ctrl-c", "ctrl+c", "C-c", "c+C", " CTRL-C "] {
            let resolved = resolve_key(spelling).expect("{spelling} should resolve");
            assert_eq!(resolved.name, "ctrl-c");
            assert_eq!(resolved.tmux, "C-c");
        }
        // Aliases collapse onto one canonical spelling too.
        assert_eq!(resolve_key("esc").unwrap().name, "escape");
        assert_eq!(resolve_key("RETURN").unwrap().name, "enter");
        assert_eq!(resolve_key("pgup").unwrap().tmux, "PageUp");
    }

    #[test]
    fn resolve_key_refuses_what_tmux_would_type_as_text() {
        // tmux does not validate key names — an unrecognized one is injected
        // into the pane as literal text — so anything outside the table must
        // fail here rather than land in an agent's prompt.
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
            assert!(resolve_key(bad).is_none(), "{bad:?} should not resolve");
        }
    }

    #[test]
    fn resolve_key_covers_every_control_letter() {
        for letter in 'a'..='z' {
            let resolved = resolve_key(&format!("ctrl-{letter}")).expect("ctrl-<letter>");
            assert_eq!(resolved.tmux, format!("C-{letter}"));
        }
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

    /// A plugin's pane gets a prefix of its own, and its name is deterministic —
    /// which is the entire mechanism for finding the window again after a restart.
    #[test]
    fn a_program_window_is_named_deterministically_and_apart_from_sessions() {
        let once = program_window_name("abcd1234", "watch");
        assert_eq!(once, "tbp-abcd1234-watch");
        assert_eq!(
            once,
            program_window_name("abcd1234", "watch"),
            "deterministic"
        );

        // Distinct prefix, so window discovery cannot adopt one as a session's
        // agent (`tb-`) or its companion shell (`tbs-`).
        assert!(once.starts_with(PROGRAM_WINDOW_PREFIX));
        assert!(!once.starts_with(&format!("{WINDOW_PREFIX}a")));
        assert!(!once.starts_with(SHELL_WINDOW_PREFIX));
    }

    /// A program window is invisible to session discovery — which is both a
    /// safety property and the reason `find_window` exists.
    ///
    /// `discover` filters on `tb-`, so `tbp-` can never be adopted as a session's
    /// agent pane. The same filter is why re-finding a program pane needs its own
    /// lookup, and why the companion shell persists a pane id instead (`tbs-`
    /// fails the filter too — the comment there claiming otherwise is wrong).
    #[test]
    fn discovery_cannot_see_a_program_window() {
        let program = program_window_name("abcd1234", "watch");
        assert!(
            !program.starts_with(WINDOW_PREFIX),
            "{program} must fail discovery's filter"
        );
        // And the shell's, for the same reason — pinned so the asymmetry is not
        // mistaken for an oversight later.
        assert!(!shell_window_name("s").starts_with(WINDOW_PREFIX));
        // While an agent window of course passes it.
        assert!(agent_window_name("s").starts_with(WINDOW_PREFIX));
    }

    /// Why the owner is a **digest** rather than the plugin's path.
    ///
    /// `sanitize_window_name` maps every character outside `[A-Za-z0-9_-]` to
    /// `_`, so two different paths sanitize to one window — and two plugins would
    /// then share a single program. The digest is computed by the caller for
    /// exactly this reason; this pins the hazard that makes it necessary.
    #[test]
    fn sanitizing_a_path_would_collide_which_is_why_the_owner_is_digested() {
        assert_eq!(
            sanitize_window_name("plugins/90_watch.lua"),
            sanitize_window_name("plugins.90.watch.lua"),
            "two distinct paths, one window name — the collision a digest avoids"
        );
        // Digested owners of different paths do not collide.
        assert_ne!(
            program_window_name("aaaa1111", "watch"),
            program_window_name("bbbb2222", "watch")
        );
    }

    #[test]
    fn window_target_uses_exact_match_prefix() {
        // Without `=`, tmux treats the window name as a pattern and will
        // resolve `tb-foo` ambiguously when both `tb-foo` and
        // `tb-foo-bar` exist. The `=` prefix forces exact-match lookup.
        let t = window_target("foo");
        assert!(t.ends_with(":=tb-foo"), "got {t}");
    }

    #[test]
    fn parse_pane_dead_only_accepts_one() {
        assert!(parse_pane_dead("1"));
        assert!(parse_pane_dead("1\n"));
        assert!(!parse_pane_dead("0\n"));

        // A missing window makes `display-message` exit 0 printing nothing.
        // Reading that as dead would mask the `send-keys` "can't find window"
        // error that actually diagnoses it, turning a typo into "has exited".
        assert!(!parse_pane_dead(""));
        assert!(!parse_pane_dead("\n"));

        // Never infer deadness from anything but the flag itself.
        assert!(!parse_pane_dead("10"));
        assert!(!parse_pane_dead("dead"));
    }

    // --- title_seed_bytes tests (adopt-time activity-line restore) ---

    #[test]
    fn title_seed_replays_an_agent_title_as_osc_2() {
        assert_eq!(
            title_seed_bytes("devbox", "\u{2733} Terminal name lost on restart"),
            "\x1b]2;\u{2733} Terminal name lost on restart\x1b\\".as_bytes()
        );
    }

    #[test]
    fn title_seed_suppresses_tmuxs_default_title() {
        // A pane nothing ever titled reads back as the host's own short name.
        assert!(title_seed_bytes("devbox", "devbox").is_empty());
        assert!(title_seed_bytes("devbox", "  devbox  ").is_empty());
        assert!(title_seed_bytes("devbox", "   ").is_empty());
    }

    #[test]
    fn title_seed_drops_control_characters() {
        // The title is remote-controlled text and the sequence it goes into is
        // terminated by an escape, so a title carrying one must not close it.
        let seed = title_seed_bytes("h", "done\x1b\\ + rm -rf\x07\nnext");
        assert_eq!(seed, "\x1b]2;done\\ + rm -rfnext\x1b\\".as_bytes());
    }

    #[test]
    fn title_seed_bounds_a_huge_title() {
        let seed = title_seed_bytes("h", &"\u{00e9}".repeat(4_000));
        // The budget is the payload's; the introducer and terminator sit
        // outside it. Multi-byte chars must not be split to reach it either.
        assert!(seed.len() <= MAX_TITLE_SEED_BYTES + 6, "{}", seed.len());
        assert!(std::str::from_utf8(&seed).is_ok());
    }

    // --- pane state (cursor / foreground process / live cwd) ---

    /// Build the `display-message` answer tmux produces for the format
    /// `pane_state` asks for, so the tests speak in fields rather than bytes.
    fn pane_state_answer(fields: &[&str]) -> String {
        format!("{}\n", fields.join(&PANE_STATE_SEP.to_string()))
    }

    #[test]
    fn parse_pane_state_reads_every_field() {
        let (state, tty, _) = parse_pane_state(&pane_state_answer(&[
            "12",
            "34",
            "node",
            "/home/u/repo",
            "/dev/pts/7",
        ]));
        assert_eq!(state.cursor_row, Some(12));
        assert_eq!(state.cursor_col, Some(34));
        assert_eq!(state.foreground_process.as_deref(), Some("node"));
        assert_eq!(state.foreground_cwd.as_deref(), Some("/home/u/repo"));
        assert_eq!(tty.as_deref(), Some("/dev/pts/7"));
        // Only the `ps` pass can fill this in — the tmux answer never does.
        assert_eq!(state.foreground_command, None);
    }

    #[test]
    fn parse_pane_state_reads_the_answer_an_older_tmux_prints() {
        // Byte for byte what tmux 3.4 — ubuntu-24.04's, and so CI's — answers
        // the same `display-message`: its `vis(3)` pass rewrites the separator
        // to its octal escape, which used to parse as one field and report
        // every pane fact null.
        let raw = "12\\03734\\037node\\037/home/u/repo\\037/dev/pts/7\\0370\\037tb-demo\n";
        let (state, tty, window) = parse_pane_state(raw);
        assert_eq!(state.cursor_row, Some(12));
        assert_eq!(state.cursor_col, Some(34));
        assert_eq!(state.foreground_process.as_deref(), Some("node"));
        assert_eq!(state.foreground_cwd.as_deref(), Some("/home/u/repo"));
        assert_eq!(state.dead, Some(false));
        assert_eq!(tty.as_deref(), Some("/dev/pts/7"));
        assert_eq!(window.as_deref(), Some("tb-demo"));
    }

    #[test]
    fn parse_pane_state_keeps_a_path_with_spaces_whole() {
        // Why the separator is a control byte and not whitespace: a path may
        // contain spaces, and splitting on them would report half of one.
        let (state, tty, _) = parse_pane_state(&pane_state_answer(&[
            "0",
            "0",
            "my agent",
            "/home/u/My Repo/sub dir",
            "/dev/pts/1",
        ]));
        assert_eq!(
            state.foreground_cwd.as_deref(),
            Some("/home/u/My Repo/sub dir")
        );
        assert_eq!(state.foreground_process.as_deref(), Some("my agent"));
        assert_eq!(tty.as_deref(), Some("/dev/pts/1"));
    }

    #[test]
    fn parse_pane_state_reports_an_unanswered_field_as_absent() {
        // A multiplexer that does not know a format expands it to nothing
        // (psmux). An empty string would read downstream as a real answer —
        // a cursor at an unknown row is not a cursor at row 0.
        let (state, tty, _) = parse_pane_state(&pane_state_answer(&["", "", "", "", ""]));
        assert_eq!(state, PaneState::default());
        assert_eq!(tty, None);

        // And a truncated answer leaves the fields it never carried absent
        // rather than shifting later values into earlier slots.
        let (state, tty, _) = parse_pane_state(&pane_state_answer(&["3", "4"]));
        assert_eq!(state.cursor_row, Some(3));
        assert_eq!(state.cursor_col, Some(4));
        assert_eq!(state.foreground_cwd, None);
        assert_eq!(tty, None);
    }

    #[test]
    fn parse_pane_state_reports_which_window_answered() {
        // The field that makes the answer attributable: `display-message`
        // against a target it cannot resolve answers for the client's current
        // pane and exits 0, so without this the caller cannot tell a session's
        // own pane from a stranger's.
        let (_, _, window) = parse_pane_state(&pane_state_answer(&[
            "0",
            "0",
            "claude",
            "/w",
            "/dev/pts/2",
            "0",
            "tb-demo",
        ]));
        assert_eq!(window.as_deref(), Some("tb-demo"));
    }

    #[test]
    fn parse_pane_state_reads_whether_the_panes_command_has_exited() {
        // `remain-on-exit=on` keeps a dead pane's frame, and tmux keeps naming
        // the command that died in it — so "what is running here" is only
        // answerable with this flag beside it.
        let (state, _, _) = parse_pane_state(&pane_state_answer(&[
            "0",
            "0",
            "claude",
            "/w",
            "/dev/pts/2",
            "1",
        ]));
        assert_eq!(state.dead, Some(true));
        assert_eq!(state.foreground_process.as_deref(), Some("claude"));

        let (live, _, _) = parse_pane_state(&pane_state_answer(&[
            "0",
            "0",
            "claude",
            "/w",
            "/dev/pts/2",
            "0",
        ]));
        assert_eq!(live.dead, Some(false));

        // A multiplexer that does not know the format expands it to nothing,
        // and "not answered" is not "alive".
        let (unknown, _, _) = parse_pane_state(&pane_state_answer(&[
            "0",
            "0",
            "claude",
            "/w",
            "/dev/pts/2",
            "",
        ]));
        assert_eq!(unknown.dead, None);
    }

    #[test]
    fn parse_pane_state_survives_a_dead_or_missing_pane() {
        // `display-message` against a window that is gone exits 0 printing an
        // empty line — the same shape `parse_pane_dead` guards against.
        let (state, tty, _) = parse_pane_state("");
        assert_eq!(state, PaneState::default());
        assert_eq!(tty, None);
    }

    #[test]
    fn ps_foreground_prefers_the_group_leader_over_its_pipeline() {
        // `tpgid` is the tty's foreground group; the rows whose own `pgid`
        // equals it are that job, and its leader is the command to report.
        let out = "\
 4210  4210  4300 -bash
 4300  4300  4300 node /opt/cursor-agent/cli.js --resume
 4301  4300  4300 tee /tmp/log
";
        let (argv0, command) = parse_ps_foreground(out).expect("a foreground job");
        assert_eq!(argv0, "node");
        // The whole point of the argv: a bare command *name* is `node` for both
        // an agent CLI and a REPL, and only this tells them apart.
        assert_eq!(command, "node /opt/cursor-agent/cli.js --resume");
    }

    #[test]
    fn ps_foreground_falls_back_to_a_group_member() {
        // The leader can have exited while the rest of its group runs on.
        let out = " 4301  4300  4300 tee /tmp/log\n";
        assert_eq!(
            parse_ps_foreground(out).map(|(argv0, _)| argv0),
            Some("tee".to_string())
        );
    }

    #[test]
    fn ps_foreground_reports_nothing_when_nothing_holds_the_tty() {
        // -1 is "no foreground group"; 0 is `ps` saying it does not know.
        // Neither is a process, and reporting the background shell for either
        // would be a plausible wrong answer rather than an honest absence.
        assert_eq!(parse_ps_foreground(" 4210 4210 -1 -bash\n"), None);
        assert_eq!(parse_ps_foreground(" 4210 4210 0 -bash\n"), None);
        assert_eq!(parse_ps_foreground(""), None);
        // A background job is not the foreground one either.
        assert_eq!(parse_ps_foreground(" 4210 4210 4300 -bash\n"), None);
    }

    #[test]
    fn ps_rows_survive_right_aligned_padding_and_junk() {
        // `ps` pads its numeric columns to the widest value, so a narrow pid
        // arrives behind several spaces — which is what `splitn` mis-parses.
        let out = "\
    9     9  4300 sh
 4300  4300  4300 vim notes.md
ERROR: something ps printed
";
        assert_eq!(
            parse_ps_foreground(out),
            Some(("vim".to_string(), "vim notes.md".to_string()))
        );
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
