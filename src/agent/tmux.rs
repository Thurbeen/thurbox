//! tmux backend: the transport that launches `tmux`, the tmux dialect of the
//! shared mux core (`MuxDialect`), and the [`SessionBackend`] over both.
//!
//! Everything here is tmux's own: POSIX quoting and login shells, the full
//! session config, `send-keys -H`, the sizer negotiation. Pane bookkeeping
//! both multiplexers share lives in the mux core.

use std::collections::HashMap;
use std::path::Path;
use std::process::Command;

use anyhow::{bail, Context, Result};

use super::backend::{
    AdoptedSession, BackendLiveness, DiscoveredSession, PanePath, PaneState, SessionBackend,
    SpawnedSession, WindowRole,
};
use super::control_mode::{self, shell_escape, PaneSnapshot, SIZER_OPTION};
use super::mux::{
    bracketed_paste, path_prefix_args, resolve_local_program, set_window_option_commands,
    shell_prefix_tokens, tmux_size, ConfigOption, MuxBackend, MuxDialect, HEARTBEAT_INTERVAL_SECS,
    SESSION_OPTS, SHELL_WINDOW_PREFIX, WINDOW_OPTS,
};
use super::transport::{LaunchPath, MuxTransport};

/// A tmux command path. WSL always runs Linux tmux.
#[derive(Debug, Clone)]
pub enum TmuxTransport {
    Local,
    Ssh {
        destination: String,
        ssh_opts: Vec<String>,
    },
    Wsl {
        distro: String,
    },
}

impl MuxTransport for TmuxTransport {
    const BINARY: &'static str = "tmux";
    fn local() -> Self {
        Self::Local
    }
    fn from_host(host: &crate::session::HostDef) -> Self {
        if host.is_wsl() {
            Self::Wsl {
                distro: host.distro_name(),
            }
        } else {
            Self::Ssh {
                destination: host.destination.clone(),
                ssh_opts: host.ssh_opts.clone(),
            }
        }
    }
    fn path(&self) -> LaunchPath {
        match self {
            Self::Local => LaunchPath::Local,
            Self::Ssh {
                destination,
                ssh_opts,
            } => LaunchPath::Ssh {
                destination: destination.clone(),
                ssh_opts: ssh_opts.clone(),
            },
            Self::Wsl { distro } => LaunchPath::Wsl {
                distro: distro.clone(),
            },
        }
    }
}

impl TmuxTransport {
    pub fn tmux_command(&self, socket: &str, args: &[&str]) -> Command {
        self.mux_command(socket, args)
    }
}

/// The `terminal-features` slot thurbox writes `*:clipboard` into — see
/// `session_config`. High enough that neither tmux's defaults nor a
/// hand-appended list reaches it.
const CLIPBOARD_FEATURE_SLOT: &str = "terminal-features[100]";

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

    let major: u32 = parts[0]
        .parse()
        .with_context(|| format!("Cannot parse tmux major version from: {version_str}"))?;
    // Minor might have a trailing letter (e.g., "3a"), strip non-digits.
    let minor_str: String = parts[1].chars().take_while(char::is_ascii_digit).collect();
    let minor: u32 = minor_str
        .parse()
        .with_context(|| format!("Cannot parse tmux minor version from: {version_str}"))?;

    Ok((major, minor))
}

/// Enforce the minimum-version gate against `tmux -V` output.
///
/// The `>= 3.2` floor applies to a `tmux …` banner. Any other banner that
/// answered `-V` is accepted as-is: a drop-in build numbering itself
/// independently still implements the control-mode feature set.
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

/// The program a **local** window should launch: the agent's command,
/// resolved against thurbox's own `PATH` — see [`resolve_local_program`].
/// A remote/WSL backend passes through: its `PATH` is the *host's*, and its
/// window command is login-wrapped instead ([`login_wrap_for_remote`]).
fn program_for_window(backend: &MuxBackend<TmuxTransport>, command: &str) -> String {
    if backend.transport().is_remote() {
        return command.to_string();
    }
    resolve_local_program(command)
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
    let mut parts = vec![shell_escape(command)];
    for arg in args {
        parts.push(shell_escape(arg));
    }
    parts.join(" ")
}

/// Wrap a window command in a **login** shell for a remote/WSL backend so the
/// user's profile `PATH` is present. Agents are commonly installed under
/// `~/.local/bin` (e.g. `claude`), which the login profile adds to `PATH`; a
/// non-login shell skips those files, so the agent binary isn't found, the
/// window command exits 1, and the pane dies instantly — the remote session
/// appears to "not launch". `exec` replaces the wrapper so no extra process
/// lingers.
///
/// Local backends pass through, but **not** because they inherit the user's
/// interactive `PATH` — that claim used to stand here and was wrong (see
/// [`resolve_local_program`], which is what makes them safe now). They are not
/// wrapped because thurbox can resolve a local command itself, and an absolute
/// path needs no shell's `PATH` at all; a wrap would only add a second shell
/// whose own quoting rules could differ.
///
/// `/bin/sh -l` reads `~/.profile` but not the user's own shell's files
/// (`~/.zshenv`, `~/.zprofile`), so the host's login `PATH` is assigned
/// inside the wrap too ([`crate::agent::host_path`]) — the same `PATH` a
/// delegated create gives the pane.
///
/// Done here — not via tmux `default-command` — because that value round-trips
/// through the remote transport's per-arg shell-quoting, where a `-l` flag's
/// space would be re-split into a stray `set-option` argument.
fn login_wrap_for_remote(backend: &MuxBackend<TmuxTransport>, shell_cmd: &str) -> String {
    if !backend.transport().is_remote() {
        return shell_cmd.to_string();
    }
    let path = backend
        .host()
        .and_then(crate::agent::host_path::assignment_for)
        .unwrap_or_default();
    let inner = shell_escape(&format!("{path}exec {shell_cmd}"));
    format!("/bin/sh -lc {inner}")
}

/// The window command for a **remote/WSL** companion shell pane: the user's
/// own login shell, interactively — the same environment an `ssh <host>`
/// login gives you, not a bare `/bin/sh`.
///
/// [`MuxDialect::REMOTE_SHELL`] is `/bin/sh` here (guaranteed to exist), and
/// [`login_wrap_for_remote`] would run it as `/bin/sh -lc 'exec /bin/sh'` — a
/// login-sourced but then bare POSIX shell. That drops everything a real SSH
/// login loads from the account's shell: its rc files (`~/.bashrc` /
/// `~/.zshrc`), prompt, aliases, functions, and `PATH` additions. SSH runs the
/// shell recorded in the user's passwd entry (which `$SHELL` reflects), so we
/// do the same: bootstrap through the always-present `/bin/sh -l` (which
/// login-sources the profile and thus exports `$SHELL`), then `exec` `"$SHELL"`
/// as a **login** shell — tmux gives it a PTY, so it's interactive and sources
/// the interactive rc chain too. If `$SHELL` is unset/broken the guard falls
/// back to a plain `/bin/sh -l` so the pane still opens.
///
/// The fallback is a `command -v` **guard**, never `exec "$SHELL" -l
/// 2>/dev/null || …`: bash (and zsh) decide interactivity from
/// `isatty(stdin) && isatty(stderr)`, and an `exec … 2>/dev/null`
/// redirection **persists** into the exec'd shell — with stderr no longer a
/// TTY the shell starts **non-interactive** (no prompt, no rc files, no
/// readline), which reads as a blank "not loading" pane. So we probe
/// `$SHELL` with `command -v` (whose own `2>/dev/null` is harmless) and only
/// then `exec` it with all three std streams still on the PTY.
fn remote_shell_pane_command() -> String {
    let inner = shell_escape(
        "command -v \"$SHELL\" >/dev/null 2>&1 && exec \"$SHELL\" -l; exec /bin/sh -l",
    );
    format!("/bin/sh -lc {inner}")
}

impl MuxDialect for TmuxTransport {
    const WINDOW_OPTIONS: bool = true;
    const COMMAND_LISTS: bool = true;
    const SIZE_REPORTS: bool = true;
    const IMPLICIT_ATTACH_RESPONSE: bool = true;
    const STRICT_RESPONSE_BLOCKS: bool = true;
    const SUBSCRIPTIONS: bool = true;
    const POLLS_LIVENESS: bool = false;
    const UTF8_FLAG: bool = true;
    const ONESHOT_PANE_REPORT: bool = true;
    const REMOTE_SHELL: &'static str = "/bin/sh";

    fn check_banner(banner: &str, _socket: &str) -> Result<()> {
        check_min_version(banner)
    }

    fn session_config(session: &str, default_command: Option<&str>) -> Vec<ConfigOption> {
        let mut config = Vec::new();
        let mut set = |args: &[&str], fatal: bool| config.push(ConfigOption::set(args, fatal));
        // Use a non-login shell so that macOS path_helper (/etc/zprofile)
        // doesn't clobber PATH additions from ~/.zshenv (e.g. cargo, asdf).
        // For a remote backend the local `$SHELL` path may not exist on the
        // remote host, so fall back to a POSIX shell there.
        if let Some(shell) = default_command {
            set(&["-s", "default-command", shell], true);
        }

        // Server-wide options every supported tmux understands. A failure here
        // means the server can't host sessions, so it is propagated.
        set(&["-s", "default-terminal", "xterm-256color"], true);
        set(&["-s", "extended-keys", "on"], true);

        // `extended-keys-format csi-u` is best-effort: the option landed in tmux
        // 3.5, but thurbox's floor is 3.2, so an older tmux rejects it ("invalid
        // option"). It is advisory only — thurbox injects keystroke bytes directly
        // via `send-keys` (not through tmux's key forwarder), so it never
        // re-encodes what an agent receives; it just sets what `tmux show-options`
        // reports, which some agents (notably `pi`) probe at startup and warn about
        // unless it is `csi-u`. Ignoring the error keeps a 3.2–3.4 host working (pi
        // users there simply miss the hint) while 3.5+ hosts get the preferred
        // format.
        set(&["-s", "extended-keys-format", "csi-u"], false);

        // The two silent gates that would otherwise drop an OSC 52 clipboard
        // write originating **inside** a pane (thurbox's own copy, or an
        // agent's). Both are no-ops-on-failure by design, hence best-effort:
        //
        // 1. `set-clipboard` must be exactly `on`. tmux's `input_osc_52_parse`
        //    bails on `!= 2`, and the shipped default is `external` (1) — which
        //    forwards tmux's *own* copy-mode yanks but **discards** an
        //    application's OSC 52 with no error and no visual artifact. This is
        //    the default-broken case: without it every other part of the
        //    clipboard path is dead under tmux.
        // 2. The `Ms` terminfo capability must be present, or
        //    `tty_set_selection` returns early — a second, independent silent
        //    drop. A `*:clipboard` entry in `terminal-features` injects it for
        //    every terminal (tmux 3.2+, matching thurbox's floor; the pre-3.2
        //    form was a raw `terminal-overrides` Ms= string). Written at the end
        //    of the list, below.
        //
        // Security tradeoff: `set-clipboard on` lets any process in a pane set
        // the user's system clipboard — an exfiltration channel, and why tmux
        // moved the default to `external` in 2.6. Scoped here to thurbox's own
        // socket, and the price of copy working at all over SSH.
        set(&["-s", "set-clipboard", "on"], false);

        for (key, val) in SESSION_OPTS {
            set(&["-t", session, key, val], true);
        }
        // Apps inside tmux can inspect this option before deciding whether to
        // request mouse reports. With it off, a full-screen app may leave wheel
        // capture disabled even though thurbox can forward those reports.
        set(&["-t", session, "mouse", "on"], true);

        // Window-level options — see `WINDOW_OPTS` for why these are global to
        // the server and why failing to set one is not fatal.
        for (key, val) in WINDOW_OPTS {
            set(&["-w", "-g", key, val], false);
        }

        // The `*:clipboard` feature goes into a fixed slot, and only while that
        // slot is empty. Appending it grew the list by one entry a run, since
        // this runs on every spawn and the server outlives thurbox (#1278); an
        // unconditional write to the slot would overwrite an entry the user's
        // `~/.tmux.conf` put there. Reading the list from Rust first would cost
        // a process per session create, and a format cannot test the whole
        // array on 3.2 (`#{terminal-features}` expands to "") — but it can read
        // one index. `-a` fills the first free index, so appended entries
        // never land on this one.
        let slot = format!("#{{{CLIPBOARD_FEATURE_SLOT}}}");
        let write = format!("set-option -qs {CLIPBOARD_FEATURE_SLOT} *:clipboard");
        config.push(ConfigOption::command(
            vec!["if-shell".into(), "-F".into(), slot, String::new(), write],
            false,
        ));
        config
    }

    fn birth_option_commands(window_name: &str) -> Vec<String> {
        set_window_option_commands(window_name)
    }

    fn quote_arg(s: &str) -> String {
        shell_escape(s)
    }

    fn env_args(env: &HashMap<String, String>) -> String {
        env.iter()
            .map(|(k, v)| format!(" -e {}", shell_escape(&format!("{k}={v}"))))
            .collect()
    }

    fn window_command(
        backend: &MuxBackend<Self>,
        window_name: &str,
        command: &str,
        args: &[String],
        _env: &HashMap<String, String>,
    ) -> String {
        // A remote/WSL companion shell pane (`tbs-` window) opens the user's own
        // interactive login shell — the SSH-login environment — instead of the
        // bare `/bin/sh` the generic login-wrap would produce (see
        // `remote_shell_pane_command`). Agent windows (`tb-`) keep the
        // standard path.
        let remote = backend.transport().is_remote();
        if remote && window_name.starts_with(SHELL_WINDOW_PREFIX) {
            return remote_shell_pane_command();
        }
        let program = program_for_window(backend, command);
        let shell_cmd = build_shell_command(&program, args);
        // A remote pane's `PATH` is the host's, restored by the login wrap; a
        // local one is inherited from this process, which need not have the
        // CLI its hooks call on it (see `path_prefix_args`).
        let shell_cmd = match remote {
            true => shell_cmd,
            // A shell reads this whole string, so the prefix has to be UTF-8
            // here; a `PATH` that is not gets no prefix rather than a mangled
            // one (see `path_prefix_args`).
            false => shell_prefix_tokens()
                .map(|tokens| {
                    tokens
                        .into_iter()
                        .chain(std::iter::once(shell_cmd.clone()))
                        .collect::<Vec<_>>()
                        .join(" ")
                })
                .unwrap_or(shell_cmd),
        };
        login_wrap_for_remote(backend, &shell_cmd)
    }

    fn send_keys_commands(pane_id: &str, buf: &[u8]) -> Vec<String> {
        control_mode::hex_send_keys_commands(pane_id, buf)
    }

    fn resize_commands(
        backend_id: &str,
        rows: u16,
        cols: u16,
        sizer: &str,
    ) -> (Vec<String>, usize) {
        // A pane is the size of the rect ONE instance paints it into. Several
        // instances attached to one server each paint their own rect, and when
        // each resized to its own, whichever painted last — a toast taking a
        // row is enough — re-wrapped the agent for everybody. So the window
        // names its sizer (`SIZER_OPTION`), and a paint resizes only a window
        // that is this instance's to size: one nobody claims, one it already
        // sizes, or any window at all while it is the only client attached,
        // which is what makes a sizer that quit or crashed let go.
        // `claim_size` is how the name changes hands.
        //
        // Decided by tmux, in this same list, so a decision costs no round trip
        // and two instances cannot both win it. The shape is fixed on purpose:
        // the response queue expects a known number of `%begin` blocks per
        // list, and `if-shell` answers with one more block for each command it
        // runs — four taken and one declined, measured on tmux 3.7c. So the
        // name is settled first by a `set-option -F` that always answers once,
        // and each resize is its own `if-shell` whose else runs one command
        // too: five blocks, whichever way it goes. A pane that is gone fails
        // the first command and tmux drops the rest, which the queue expects
        // of any list; an inner command failing does NOT stop the list, which
        // is why the sizes are clamped to what tmux accepts (`tmux_size`).
        let (rows, cols) = tmux_size(rows, cols);
        let window = format!("resize-window -t {backend_id} -x {cols} -y {rows}");
        let pane = format!("resize-pane -t {backend_id} -x {cols} -y {rows}");
        let me = sizer;
        let may = format!(
            "#{{||:#{{==:#{{session_attached}},1}},#{{||:#{{==:#{{{SIZER_OPTION}}},}},#{{==:#{{{SIZER_OPTION}}},{me}}}}}}}"
        );
        let settle =
            format!("set-option -F -w -t {backend_id} {SIZER_OPTION} '#{{?{may},{me},#{{{SIZER_OPTION}}}}}'");
        let mine = format!("#{{==:#{{{SIZER_OPTION}}},{me}}}");
        let only_if_mine = |cmd: &str| {
            format!("if-shell -F -t {backend_id} '{mine}' '{cmd}' 'display-message -p \"\"'")
        };
        (vec![settle, only_if_mine(&window), only_if_mine(&pane)], 5)
    }

    /// The bracketed-paste-wrapped bytes, delivered literally (`send-keys -l`).
    fn paste_args(target: &str, text: &str) -> Vec<String> {
        vec![
            "send-keys".to_string(),
            "-t".to_string(),
            target.to_string(),
            "-l".to_string(),
            bracketed_paste(text),
        ]
    }

    /// A plain `sh` one-liner: `run-shell` runs it through the server's
    /// POSIX shell.
    fn deferred_prompt_script(socket: &str, target: &str, text: &str) -> String {
        let bin = Self::BINARY;
        let escaped_target = shell_escape(target);
        // Bracketed-paste wrap (see `bracketed_paste`) so multi-line prompts don't
        // submit early; `-l` makes the multiplexer deliver the bytes literally.
        let escaped_text = shell_escape(&bracketed_paste(text));
        format!(
            "{bin} -L {socket} send-keys -t {escaped_target} -l {escaped_text}; \
             sleep 0.2; \
             {bin} -L {socket} send-keys -t {escaped_target} Enter"
        )
    }

    /// The loop runs via the server's shell, so the CLI path is escaped for it.
    fn heartbeat_loop_command(cli_path: &Path) -> String {
        let cli = shell_escape(&cli_path.display().to_string());
        format!(
            "while true; do {cli} automation tick >/dev/null 2>&1; sleep {HEARTBEAT_INTERVAL_SECS}; done"
        )
    }

    fn push_window_program(
        tmux: &mut Command,
        command: &str,
        args: &[String],
        env: &HashMap<String, String>,
    ) {
        for (k, v) in env {
            tmux.args(["-e", &format!("{k}={v}")]);
        }
        // Pass the command + args as a single argv list. tmux treats trailing args
        // as the command to run inside the window. Resolved here for the same
        // reason the control-mode path resolves it (see `resolve_local_program`):
        // this path happens to get thurbox's own `PATH` because its client is
        // unattached, but a session must not launch differently depending on which
        // of the two created it — a session created here and later restarted
        // through control mode would otherwise resolve against two different
        // environments.
        let program = resolve_local_program(command);
        // `PATH` is the one variable `-e` cannot carry, so the CLI's directory
        // rides in the command instead (see `path_prefix_args`) — but **how many
        // arguments** that leaves is itself load-bearing, so the prefix is spelled
        // to keep the count tmux would have seen.
        //
        // tmux runs a **one-argument** window command through its `default-shell`
        // and a multi-argument one through `execvp` (`spawn.c`). A command session
        // with no args is the one-argument case, and `--command "sleep 300"` only
        // ever worked because that shell split it. Pushing the prefix as two more
        // argv entries moved it to `execvp`, which has no splitting to do: the pane
        // died instantly with status 127 and a `sleep 300: No such file` from
        // `env`. So with no args the prefix joins the same single token and the
        // shell still does the splitting it always did.
        if args.is_empty() {
            // One token means a shell reads it, and a shell reads text — so this is
            // the one place the prefix has to be spellable as text. An unspellable
            // one (a `PATH` that is not UTF-8) and an absent one lead to the same
            // command: the program alone, exactly as before.
            //
            // The program itself is **not** escaped: it is what the shell was
            // already splitting, and escaping it now would break the very commands
            // this branch exists to keep working.
            match shell_prefix_tokens() {
                Some(mut token) => {
                    token.push(program);
                    tmux.arg(token.join(" "));
                }
                None => {
                    tmux.arg(program);
                }
            }
        } else {
            // Several tokens already go to `execvp`, so the prefix rides as argv
            // and the `PATH` keeps its bytes.
            for arg in path_prefix_args() {
                tmux.arg(arg);
            }
            tmux.arg(program);
        }
        for a in args {
            tmux.arg(a);
        }
    }
}

pub struct TmuxBackend {
    pub(crate) core: MuxBackend<TmuxTransport>,
}

impl Default for TmuxBackend {
    fn default() -> Self {
        Self::local()
    }
}

impl TmuxBackend {
    pub fn new() -> Self {
        Self::local()
    }
    pub fn with_transport(
        transport: TmuxTransport,
        socket: impl Into<String>,
        session: impl Into<String>,
        name: impl Into<String>,
    ) -> Self {
        Self {
            core: MuxBackend::with_transport(transport, socket, session, name),
        }
    }
    pub(crate) fn set_name(&mut self, name: impl Into<String>) {
        self.core.set_name(name);
    }
    pub fn local() -> Self {
        let mut core = MuxBackend::<TmuxTransport>::local();
        core.set_name("local-tmux");
        Self { core }
    }

    pub fn from_host(host: &crate::session::HostDef) -> Self {
        let mut core = MuxBackend::<TmuxTransport>::from_host(host);
        core.set_name(host.backend_name());
        Self { core }
    }
}

impl SessionBackend for TmuxBackend {
    fn send_text(
        &self,
        session_id: &str,
        session_name: &str,
        text: &str,
        submit: bool,
    ) -> Result<()> {
        self.core
            .session_send_text(session_id, session_name, text, submit)
    }
    fn send_key(&self, session_id: &str, session_name: &str, key: &str) -> Result<()> {
        self.core.session_send_key(session_id, session_name, key)
    }
    fn send_text_after(
        &self,
        session_id: &str,
        session_name: &str,
        text: &str,
        delay_secs: u64,
    ) -> Result<()> {
        self.core
            .session_send_text_after(session_id, session_name, text, delay_secs)
    }
    fn capture_text(
        &self,
        session_id: &str,
        session_name: &str,
        lines: u32,
        ansi: bool,
    ) -> Result<String> {
        self.core
            .session_capture_text(session_id, session_name, lines, ansi)
    }
    fn pane_state(&self, session_id: &str, session_name: &str) -> PaneState {
        self.core.session_pane_state(session_id, session_name)
    }
    fn pane_path(&self, session_id: &str, session_name: &str) -> PanePath {
        self.core.session_pane_path(session_id, session_name)
    }
    fn has_window(&self, session_id: &str, session_name: &str) -> bool {
        self.core.session_has_window(session_id, session_name)
    }
    fn rename_windows(&self, session_id: &str, from: &str, to: &str) -> Result<()> {
        self.core.session_rename_windows(session_id, from, to)
    }
    fn claim_running_window(&self, session_id: &str, session_name: &str) -> Result<Option<String>> {
        self.core
            .session_claim_running_window(session_id, session_name)
    }
    fn name(&self) -> &str {
        self.core.name()
    }
    fn needs_liveness_poll(&self) -> bool {
        self.core.needs_liveness_poll()
    }
    fn check_available(&self) -> Result<()> {
        self.core.check_available()
    }
    fn ensure_ready(&self) -> Result<()> {
        self.core.ensure_ready()
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
        self.core
            .spawn(window_name, command, args, cwd, env, rows, cols)
    }
    fn spawn_headless(
        &self,
        session_id: &str,
        window_name: &str,
        command: &str,
        args: &[String],
        cwd: Option<&Path>,
        env: &HashMap<String, String>,
    ) -> Result<String> {
        self.core
            .spawn_headless(session_id, window_name, command, args, cwd, env)
    }
    fn headless_liveness(&self, session_id: &str, session_name: &str) -> Result<BackendLiveness> {
        self.core.headless_liveness(session_id, session_name)
    }
    fn headless_live_pane(&self, session_id: &str, session_name: &str) -> Result<Option<String>> {
        self.core.headless_live_pane(session_id, session_name)
    }
    fn headless_owned_panes_in(
        &self,
        windows: &[DiscoveredSession],
        session_id: &str,
        session_name: &str,
    ) -> Vec<String> {
        self.core
            .headless_owned_panes_in(windows, session_id, session_name)
    }
    fn kill_headless(
        &self,
        session_id: &str,
        session_name: &str,
        agent_pane: &str,
        shell_pane: &str,
    ) -> Result<bool> {
        self.core
            .kill_headless(session_id, session_name, agent_pane, shell_pane)
    }
    fn headless_pane_pid(
        &self,
        backend_id: &str,
        session_id: &str,
        name: &str,
    ) -> Result<Option<u32>> {
        self.core.headless_pane_pid(backend_id, session_id, name)
    }
    fn headless_discover(&self) -> Result<Vec<DiscoveredSession>> {
        self.core.headless_discover()
    }

    fn adopt(
        &self,
        backend_id: &str,
        rows: u16,
        cols: u16,
        seed: Option<Vec<u8>>,
    ) -> Result<AdoptedSession> {
        self.core.adopt(backend_id, rows, cols, seed)
    }
    fn capture_history(&self, backend_id: &str) -> Result<Vec<u8>> {
        self.core.capture_history(backend_id)
    }
    fn title_seed(&self, backend_id: &str) -> Vec<u8> {
        self.core.title_seed(backend_id)
    }
    fn supports_snapshots(&self) -> bool {
        true
    }
    fn request_snapshot(&self, backend_id: &str) -> Result<()> {
        self.core.request_snapshot(backend_id)
    }
    fn snapshot(&self, backend_id: &str) -> Result<PaneSnapshot> {
        self.core.snapshot(backend_id)
    }

    fn discover(&self) -> Result<Vec<DiscoveredSession>> {
        self.core.discover()
    }
    fn stamp_window(&self, backend_id: &str, session_id: &str, role: WindowRole) -> Result<()> {
        self.core.stamp_window(backend_id, session_id, role)
    }
    fn window_panes(&self, window_name: &str) -> Result<Vec<(String, bool)>> {
        self.core.window_panes(window_name)
    }
    fn set_pane_retention(&self, backend_id: &str, keep: bool) -> Result<()> {
        self.core.set_pane_retention(backend_id, keep)
    }
    fn resize(&self, backend_id: &str, rows: u16, cols: u16) -> Result<()> {
        self.core.resize(backend_id, rows, cols)
    }
    fn claim_size(&self, backend_id: &str, rows: u16, cols: u16) -> Result<()> {
        self.core.claim_size(backend_id, rows, cols)
    }
    fn is_dead(&self, backend_id: &str) -> Result<bool> {
        self.core.is_dead(backend_id)
    }
    fn kill(&self, backend_id: &str) -> Result<()> {
        self.core.kill(backend_id)
    }
    fn detach(&self, backend_id: &str) -> Result<()> {
        self.core.detach(backend_id)
    }
    fn default_shell(&self) -> String {
        self.core.default_shell()
    }
    fn pane_pid(&self, backend_id: &str) -> Result<Option<u32>> {
        self.core.pane_pid(backend_id)
    }
    fn pane_pids(&self) -> Result<HashMap<String, u32>> {
        self.core.pane_pids()
    }
    fn pane_ids(&self) -> Result<std::collections::HashSet<String>> {
        self.core.pane_ids()
    }
    fn shutdown(&self) {
        self.core.shutdown()
    }
    fn take_hook_state_events(&self) -> Vec<(String, String)> {
        self.core.take_hook_state_events()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        // A drop-in build numbers itself independently and may not print a
        // `tmux ` banner; once it answers `-V` it is accepted regardless.
        assert!(check_min_version("psmux 0.3.1").is_ok());
        assert!(check_min_version("psmux 1.0").is_ok());
        assert!(check_min_version("pmux 0.1").is_ok());
    }

    /// An executable on a directory only *this process* has on `PATH` — the
    /// shape an agent installed by `fish_add_path` is in.
    #[cfg(unix)]
    fn agent_only_thurbox_can_see(dir: &std::path::Path, name: &str) -> std::path::PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let p = dir.join(name);
        std::fs::write(&p, b"#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o700)).unwrap();
        p
    }

    /// The whole point of the fix: what tmux is handed must not need tmux's own
    /// `PATH` (nor the `PATH` of the shell tmux runs a single-token command
    /// with) to be found.
    #[test]
    #[cfg(unix)]
    fn a_local_window_command_is_an_absolute_path() {
        let dir = tempfile::TempDir::new().unwrap();
        let expected = agent_only_thurbox_can_see(dir.path(), "tbx-spawn-probe");

        // Through the shared helper: `PATH` is process state, and the unit
        // tests that set it run concurrently under plain `cargo test`.
        let (local, free, remote) = crate::paths::with_path(dir.path(), || {
            (
                program_for_window(&MuxBackend::<TmuxTransport>::local(), "tbx-spawn-probe"),
                resolve_local_program("tbx-spawn-probe"),
                // A remote host's PATH is the host's, so its command is the
                // host's to resolve — and it is login-wrapped instead.
                program_for_window(
                    &MuxBackend::<TmuxTransport>::from_host(&crate::session::HostDef {
                        name: "devbox".into(),
                        destination: "me@devbox".into(),
                        ..Default::default()
                    }),
                    "tbx-spawn-probe",
                ),
            )
        });

        assert_eq!(local, expected.to_string_lossy());
        assert_eq!(free, expected.to_string_lossy());
        assert_eq!(remote, "tbx-spawn-probe");
    }

    #[test]
    fn build_shell_command_simple() {
        let cmd = build_shell_command("claude", &[]);
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
        let cmd = build_shell_command("claude", &args);
        assert_eq!(cmd, "claude --resume abc-123 --permission-mode default");
    }

    #[test]
    fn build_shell_command_with_spaces_in_args() {
        let args = vec![
            "--allowed-tools".to_string(),
            "Read Bash(git:*)".to_string(),
        ];
        let cmd = build_shell_command("claude", &args);
        assert_eq!(cmd, "claude --allowed-tools 'Read Bash(git:*)'");
    }

    #[test]
    fn build_shell_command_escapes_command_path() {
        // The command token is interpreted by the server's shell, so a path
        // with a space (or any metacharacter) must be quoted, not left bare —
        // otherwise the shell would split it and the launch would break.
        let cmd = build_shell_command("/opt/My Agents/codex", &["--foo".to_string()]);
        assert_eq!(cmd, "'/opt/My Agents/codex' --foo");
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
        assert_eq!(remote_shell_pane_command(), EXPECT);

        // The interactive shell must keep stderr on the PTY — a stray
        // `exec … 2>` would make it non-interactive.
        assert!(!EXPECT.contains("-l 2>"));
    }

    #[test]
    fn login_wrap_wraps_remote_command_in_login_shell() {
        // Remote/WSL: the window command runs under a login shell so the user's
        // profile PATH (e.g. `~/.local/bin/claude`) is present, or the agent
        // binary isn't found and the pane dies instantly.
        let backend =
            MuxBackend::<TmuxTransport>::from_host(&crate::session::HostDef::wsl("Ubuntu"));
        let wrapped = login_wrap_for_remote(&backend, "claude --resume x");
        assert_eq!(wrapped, "/bin/sh -lc 'exec claude --resume x'");
    }

    #[test]
    fn login_wrap_assigns_the_hosts_login_path() {
        let host = crate::session::HostDef {
            name: "login-wrap-path".into(),
            destination: "me@devbox".into(),
            ..Default::default()
        };
        crate::agent::host_path::seed(
            &host,
            Some(crate::agent::host_path::HostEnv {
                home: Some("/home/me".into()),
                base: vec!["/usr/bin".into()],
                shell_login: Some(vec!["/home/me/.local/bin".into(), "/usr/bin".into()]),
                sh_login: None,
            }),
        );
        let backend = MuxBackend::<TmuxTransport>::from_host(&host);
        assert_eq!(
            login_wrap_for_remote(&backend, "claude"),
            "/bin/sh -lc 'PATH=/home/me/.local/bin:/usr/bin; export PATH; exec claude'"
        );
    }

    #[test]
    fn login_wrap_is_noop_for_local() {
        // Local backends inherit the user's interactive PATH — no wrap needed.
        let backend = MuxBackend::<TmuxTransport>::local();
        assert_eq!(login_wrap_for_remote(&backend, "claude"), "claude");
    }

    #[test]
    fn paste_prompt_args_wraps_literally_for_tmux() {
        assert_eq!(
            TmuxTransport::paste_args("thurbox:tb-demo", "line one\nline two"),
            vec![
                "send-keys",
                "-t",
                "thurbox:tb-demo",
                "-l",
                "\x1b[200~line one\nline two\x1b[201~",
            ]
        );
    }
}
