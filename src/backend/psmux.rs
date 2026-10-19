//! The psmux adapter: psmux's answers to [`TmuxCompatible`].
//!
//! psmux is a native-Windows clone of tmux that speaks its command and
//! control-mode protocol — with the divergences measured against it (ADR-13)
//! and kept here: its keystrokes go as key-names and `-l` literals (compatible
//! with versions before `send-keys -H`), a paste through its own `send-paste`, a window's command and
//! environment as one PowerShell token, and its server option scope, version
//! floor and control-mode framing are its own. What it shares with tmux is
//! [`Server`]; nothing here is tmux's, and nothing in the shared code asks
//! whether a server is psmux.
//!
//! An adapter for a multiplexer, not for an OS: psmux is the default on a
//! Windows machine ([`Multiplexer::default_for`]), and drives a host of any
//! platform the same way.

use std::collections::HashMap;
use std::process::Command;
use std::sync::Arc;

use anyhow::{bail, Context, Result};
use base64::Engine as _;
use tracing::warn;

use crate::backend::contract::{SessionBackend, WindowRole};
use crate::backend::tmux_compat::control_mode::{ControlPolicy, PaneInput, SEND_KEYS_CHUNK_BYTES};
use crate::backend::tmux_compat::server::{ConfigOption, Server, TmuxCompatible};
use crate::backend::tmux_compat::transport::TmuxTransport;
use crate::session::{HostDef, Multiplexer, Platform};
use crate::shell::{
    powershell_launcher, powershell_quote, CommandLineTooLong, HostLauncher, WINDOWS_CMD_LINE_MAX,
};

/// psmux.
pub struct Psmux;

/// A psmux server's session backend.
pub type PsmuxBackend = Server<Psmux>;

/// psmux on this machine, whatever OS it is.
pub fn local() -> Arc<dyn SessionBackend> {
    Arc::new(PsmuxBackend::local())
}

/// psmux on `host`, reached through `launcher` on a machine of `platform`.
pub fn on_host(
    host: &HostDef,
    launcher: HostLauncher,
    platform: Platform,
) -> Arc<dyn SessionBackend> {
    Arc::new(PsmuxBackend::on_host(host, launcher, platform))
}

impl TmuxCompatible for Psmux {
    const MULTIPLEXER: Multiplexer = Multiplexer::Psmux;

    /// `set-option -w -t <pane> @k v` stores one option for the whole server
    /// and `#{@k}` expands to it on every window (measured against psmux 3.3.6
    /// — ADR-13), so a stamp read back from psmux identifies nothing.
    const WINDOW_OPTIONS: bool = false;

    /// psmux has neither `remain-on-exit` nor `window-size`, so an agent that
    /// exits takes its window with it: the agent window reports its own exit
    /// instead (`EXIT_REPORT`).
    const WINDOW_SETTINGS: bool = false;

    /// psmux sends `%window-close`, but not one to act on: on 3.3.7 a close
    /// was measured naming a window still alive while the one that went was
    /// never named, so a close is only a hint, confirmed against the session's
    /// panes before any reader ends (`Self::control_policy`). No
    /// `%layout-change` that reports a pane's size either.
    const WINDOW_EVENTS: bool = false;

    /// Unsupported tmux target syntax prints `ERROR:` on stdout and exits 0
    /// on psmux 3.3.8.
    const EXIT_ZERO_ERRORS: bool = true;

    /// psmux numbers panes and windows per session (psmux #569), and its socket
    /// hosts a `__warm__` session beside thurbox's with the same numbers.
    const SESSION_SCOPED_IDS: bool = true;

    /// psmux 3.3.7 resolves `kill-window -t thurbox:.%8` to the session's
    /// ACTIVE window (fixed in 3.3.8, dc183e3), where `kill-pane` with the same
    /// target kills `%8`. A thurbox window holds one pane, so that is its
    /// window gone.
    const KILL_BY_PANE: bool = true;
    const PANE_MONITORING: bool = true;

    /// Nothing has verified that a psmux reply queues behind the pane output
    /// ahead of it, and its blocks are framed the old way
    /// ([`ControlPolicy::tagged_blocks`]).
    const SNAPSHOTS: bool = false;

    /// Not verified to take a `;` list as one invocation: each option is set
    /// on its own.
    const COMMAND_LISTS: bool = false;
    const COMMAND_LIST_SINGLE_REPLY: bool = false;

    // A cold psmux server can refuse `new-session` or disappear before the
    // first `set-option`, even when its `new-session` client exited successfully.
    const COLD_START_ATTEMPTS: usize = 3;

    /// The server can become reachable well after a failed create client exits.
    const COLD_START_GRACE: std::time::Duration = std::time::Duration::from_secs(20);

    const RETRY_NO_SERVER_ERROR: bool = true;

    /// Supplying `-x/-y` prevents psmux from claiming its warm server. The
    /// placeholder's dimensions do not control agent windows.
    const BOOTSTRAP_SIZE_ARGS: &[&str] = &[];

    /// `has-session` deletes the port file on a refused connection even when
    /// the server is still starting. `list-windows` leaves it for that server.
    const SESSION_PROBE_COMMAND: &str = "list-windows";

    /// psmux's `new-window -P -F` support is unverified against the documented
    /// divergences (ADR-13), and `{end}` is tmux's shorthand: the one-shot path
    /// targets the session and finds the window by its name.
    const ONE_SHOT_SPAWN_ANSWERS: bool = false;

    /// psmux has no `remain-on-exit`. Long enough for a Windows PowerShell
    /// with a light profile to start and fail to find the program it was told
    /// to run; a slow profile (conda, oh-my-posh) can outlast it, which only
    /// misses a dead window — it never refuses a live one.
    const SPAWN_SETTLE: Option<std::time::Duration> = Some(std::time::Duration::from_millis(1500));

    /// No `if-shell -F` to decide with: the last instance to paint wins.
    const CONDITIONAL_RESIZE: bool = false;

    /// psmux 3.3.8 refuses `-s` ("unknown flag -s") and keeps one option table
    /// anyway, so it gets `-g`, which 3.3.7 and 3.3.8 both take.
    const SERVER_SCOPE: &str = "-g";

    /// Without `-t`, psmux may route `set-option -g` to `__default` even when
    /// the thurbox session is live under the same socket.
    const SERVER_OPTIONS_NEED_TARGET: bool = true;

    /// psmux has no locale sanitizing, and need not know tmux's `-u`.
    const DISPLAY_FLAGS: &[&str] = &[];

    const VERSION_FLOOR: Option<fn(&str, &str) -> Result<()>> = Some(check_psmux_version);

    /// The 3.3.7 floor, on the binary that would start a server: psmux numbers
    /// itself independently, so this is its own floor and never tmux's. A
    /// running server is judged by its own `#{version}` where panes are born
    /// ([`Self::VERSION_FLOOR`]).
    fn check_banner(banner: &str, socket: &str) -> Result<()> {
        check_psmux_version(banner, socket)
    }

    fn check_listing(listing: &str) -> Result<()> {
        if let Some(error) = listing.lines().find(|line| line.starts_with("ERROR:")) {
            bail!("psmux listing failed: {error}");
        }
        Ok(())
    }

    /// Nothing beyond the shared options: psmux has no OSC 52 clipboard
    /// forwarding (a local Windows session copies via the native clipboard path
    /// instead) and no `mouse` option to set.
    fn session_config(_session: &str) -> Vec<ConfigOption> {
        Vec::new()
    }

    /// psmux's tokenizer can't read POSIX `'\''` escapes, so a `-c`/`-n` value
    /// gets the double-quote framing it does parse.
    fn quote(arg: &str) -> String {
        psmux_quote(arg)
    }

    /// None: psmux ignores `new-window -e`, so the environment is folded into
    /// the window's script (`psmux_window_powershell`).
    fn env_flags(_env: &HashMap<String, String>) -> String {
        String::new()
    }

    fn window_command(
        _server: &Server<Self>,
        window_name: &str,
        command: &str,
        args: &[String],
        env: &HashMap<String, String>,
    ) -> Result<String> {
        psmux_window_command(window_name, command, args, env)
    }

    /// One argv token: the environment and the program as the PowerShell
    /// launch `psmux_window_program` builds, since `-e` would be ignored.
    fn push_window_program(
        cmd: &mut Command,
        window_name: &str,
        command: &str,
        args: &[String],
        env: &HashMap<String, String>,
    ) -> Result<()> {
        cmd.arg(psmux_window_program(window_name, command, args, env)?);
        Ok(())
    }

    /// psmux's own `send-paste`, which wraps and writes the payload itself (see
    /// `PsmuxPaste` for why key-encoded markers do not survive there): a raw
    /// newline inside a psmux command argument is cut by the server's
    /// line-oriented read, so a multi-line prompt arrived truncated *and* its
    /// tail ran as a psmux command (psmux #560). One per `paste_chunks`
    /// piece, as `PsmuxPaste::deliver` sends them.
    fn paste_args(target: &str, text: &str) -> Vec<Vec<String>> {
        paste_chunks(text)
            .into_iter()
            .map(|chunk| send_paste_args(target, chunk))
            .collect()
    }

    fn deferred_paste_script(mux: &str, socket: &str, target: &str, text: &str) -> Result<String> {
        deferred_paste_script(mux, socket, target, text)
    }

    /// Key-names and `-l` literals for keystrokes, and a paste out of band
    /// (`PsmuxPaste`).
    fn pane_input(transport: &TmuxTransport, socket: &str) -> Arc<dyn PaneInput> {
        Arc::new(PsmuxInput {
            paste: PsmuxPaste::new(transport.clone(), socket.to_string()),
        })
    }

    /// No implicit attach reply, untagged blocks, no subscriptions.
    ///
    /// tmux answers the `attach-session` carried on argv with a `%begin`/`%end`
    /// block of its own. **psmux does not** (measured against psmux
    /// 3.3.6 — ADR-13): its command counter starts at 1 for the *client's* first
    /// command, so the attach is never numbered at all.
    ///
    /// ```text
    /// $ printf 'display-message -p first\ndisplay-message -p second\n' \
    ///     | psmux -L s -C attach-session -t thurbox
    /// %begin 1789657328 1 1     <- the first command sent, not the attach
    /// %begin 1789657328 2 1
    /// ```
    ///
    /// Draining there waits on a `read_until` for a block that never comes, which
    /// is the whole of issue #1168's headline symptom: `ensure_ready` never
    /// returns, so the discovery worker never reports, `Terminals::discovered`
    /// stays empty, and **every** session renders "session has no pane yet" with
    /// nothing logged — the interface never attaches a single pane on Windows. The
    /// blocking read ends only when psmux closes the pipe, which is the other face
    /// of it ("control mode closed before sending its implicit attach response").
    ///
    /// No format subscriptions either, so a remote psmux connection **polls**
    /// the hook-state option — but only where a producer can exist: a *remote*
    /// psmux host, once its status channel is open ([`Self::HOOK_STATUS`]). A
    /// local psmux session signals via `thurbox-cli` straight into the DB.
    fn control_policy(transport: &TmuxTransport, session: &str) -> ControlPolicy {
        ControlPolicy {
            flow_control_command: Some("refresh-client -f pause-after=5"),
            implicit_attach_reply: false,
            tagged_blocks: false,
            command_list_single_reply: Self::COMMAND_LIST_SINGLE_REPLY,
            subscriptions: false,
            status_poll: (transport.is_remote() && Self::HOOK_STATUS)
                .then(|| hook_poll_command(session)),
            confirm_close: Some(live_panes_command(session)),
        }
    }

    /// **Closed.** The channel rests on behaviours not yet proven against
    /// psmux 3.3.6 — an in-pane `set-option -p` without `-t` (no
    /// `$TMUX_PANE` guarantee), `#{@user_option}` expansion for the poller,
    /// and claude accepting a forward-slash `--settings` path on Windows.
    /// Closed means the old strip behaviour exactly: hook configs are not
    /// shipped, the agent launches clean (surfaced as
    /// `SessionInfo::hook_wiring`), and a psmux host's sessions report no
    /// status — unknown, not idle. `scripts/dev/e2e/windows-vm.sh test` probes
    /// the first two and holds them against this answer, which it reads from
    /// `thurbox-cli runtime status --json` (`hook_status`); open it only with
    /// evidence for all three.
    const HOOK_STATUS: bool = false;

    /// psmux has no `TMUX_TMPDIR`-style socket directory — every `-L <name>`
    /// resolves machine-wide — and no `$TMUX` to find its server by, so the
    /// socket is baked into the command. The name is user-authored
    /// (`hosts.toml`) yet spliced into JSON/JS/TOML hook text and tokenized by
    /// psmux unquoted, so it keeps only `[A-Za-z0-9._-]`: a violating name
    /// lands dark on a wrong socket rather than corrupting the shipped file.
    fn hook_signal_command(server: &Server<Self>) -> String {
        let socket = server.socket();
        let safe: String = socket
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
            .collect();
        if safe != socket {
            warn!(
                "{} socket {socket:?} has characters unsafe for a hook command; using {safe:?}",
                server.name
            );
        }
        let socket = if safe.is_empty() {
            crate::backend::instance::TMUX_SOCKET.to_string()
        } else {
            safe
        };
        format!(
            "{} -L {socket} set-option -p {} ",
            Self::MULTIPLEXER.name(),
            crate::backend::tmux_compat::control_mode::REMOTE_HOOK_STATE_OPTION
        )
    }
}

/// The listing the hook poller runs: every pane of `session` with its
/// hook-state option.
fn hook_poll_command(session: &str) -> String {
    // Double-quoted framing: psmux's tokenizer passes `'` through `"…"` tokens
    // but concatenates adjacent `'…'` segments (verified against 3.3.6). The
    // session name is user-authored hosts.toml text embedded in a wire command,
    // so it gets the same double-quote framing, minus the `"`/`\` it can't
    // carry — mirroring the socket sanitization in `hook_signal_command`.
    let session_safe: String = session
        .chars()
        .filter(|c| !matches!(c, '"' | '\\'))
        .collect();
    format!(
        "list-panes -s -t \"{session_safe}\" -F \"#{{pane_id}} #{{{}}}\"",
        crate::backend::tmux_compat::control_mode::REMOTE_HOOK_STATE_OPTION,
    )
}

/// The control-mode command listing the panes `session` still has, which a
/// `%window-close` is checked against ([`ControlPolicy::confirm_close`]).
/// Framed as [`hook_poll_command`] frames its session, for the same reason.
fn live_panes_command(session: &str) -> String {
    let session_safe: String = session
        .chars()
        .filter(|c| !matches!(c, '"' | '\\'))
        .collect();
    format!("list-panes -s -t \"{session_safe}\" -F \"#{{pane_id}}\"")
}

/// psmux's pane input: [`psmux_send_keys_commands`] for keystrokes, and a
/// whole bracketed paste through [`PsmuxPaste`].
struct PsmuxInput {
    paste: PsmuxPaste,
}

impl PaneInput for PsmuxInput {
    fn send_keys(&self, pane_id: &str, buf: &[u8]) -> Vec<String> {
        psmux_send_keys_commands(pane_id, buf)
    }

    fn paste(&self, pane_id: &str, text: &str) -> Option<Result<()>> {
        Some(self.paste.deliver(pane_id, text))
    }
}

/// The first psmux whose server gives every new pane its own console.
///
/// Before 3.3.7 (psmux#450) the server's console attach/detach — which every
/// `send-keys C-c`, bracketed paste and mouse or VT injection performs — left
/// its std handle slots on freed, recycled values, and each pane born after
/// that inherits them. The pane's shell and the agent it launches then have a
/// stdin that is not the pane at all: Claude Code reports "stdin is unreadable
/// (EISDIR)" (ENOTCONN, …, depending on what the value was recycled into),
/// falls into `--print` and exits, and nothing it writes reaches the pane.
/// Measured on Windows 11: after a burst of `send-keys C-c`, every window 3.3.6
/// created was born that way and every one 3.3.8 created was not.
const MIN_PSMUX_VERSION: (u32, u32, u32) = (3, 3, 7);

/// Refuse a psmux older than [`MIN_PSMUX_VERSION`].
///
/// Reads the server's `#{version}` answer (a bare `3.3.6`) as well as a `-V`
/// banner: psmux 3.3.6 prints `tmux 3.3.6`, later ones add a `psmux X.Y.Z (…)`
/// line, which wins when present. An answer with no readable version is let
/// through: it proves nothing about the fix either way.
fn check_psmux_version(version_output: &str, socket: &str) -> Result<()> {
    let version = version_output.lines().rev().find_map(|line| {
        let line = line.trim();
        let rest = line
            .strip_prefix("psmux ")
            .or_else(|| line.strip_prefix("tmux "))
            .unwrap_or(line);
        let mut parts = rest.split_whitespace().next()?.split('.').map(|p| {
            let digits: String = p.chars().take_while(char::is_ascii_digit).collect();
            digits.parse::<u32>().ok()
        });
        Some((
            parts.next()??,
            parts.next()??,
            parts.next().flatten().unwrap_or(0),
        ))
    });
    match version {
        Some(v) if v < MIN_PSMUX_VERSION => bail!(
            "psmux {}.{}.{} is too old: its server can start an agent with a stdin that is \
             not its pane (\"stdin is unreadable (EISDIR)\"). Upgrade psmux to {}.{}.{} or \
             newer, then restart its server (`psmux -L {socket} kill-server`) — a running \
             server keeps the old code",
            v.0,
            v.1,
            v.2,
            MIN_PSMUX_VERSION.0,
            MIN_PSMUX_VERSION.1,
            MIN_PSMUX_VERSION.2,
        ),
        _ => Ok(()),
    }
}

/// What an agent window runs once its agent returns: the exit status, piped to
/// `thurbox-cli session exited`, which records it with the pane's last lines.
///
/// psmux has no `remain-on-exit`, so the window closes with the PowerShell that
/// launched the agent, and nothing outside it can learn the status or read the
/// screen afterwards — every exit reads as a window that simply vanished. The
/// window is still open while this runs, which is what leaves the screen there
/// to capture.
///
/// Piped rather than passed as an argument: `LASTEXITCODE` is unset when the
/// program never ran, and an empty argument is dropped by Windows PowerShell
/// 5.1, so an empty pipe is the one spelling that says "no status" the same
/// way everywhere. `Get-Variable` keeps the string `$`-free, as `Set-Item`
/// does for the environment. A machine with no `thurbox-cli` on PATH prints an
/// error into a window that is closing anyway.
const EXIT_REPORT: &str =
    "; Get-Variable LASTEXITCODE -ValueOnly -ErrorAction Ignore | thurbox-cli session exited";

/// Build the PowerShell script a psmux window runs: set the env vars, then
/// launch the agent — and, in an agent window, report its exit
/// ([`EXIT_REPORT`]).
///
/// psmux ignores `new-window -e` — env vars never reach the window's
/// process — so they are folded into the script itself (`Set-Item Env:K
/// 'v'; …`, chosen over `$env:K` so the string stays `$`-free). Quoting is
/// PowerShell **single** quotes (`''` = literal `'`). A raw `"` in a value is
/// neutralized to a space: Windows PowerShell 5.1 hands an embedded `"` to the
/// program unescaped, which splits the argument there. A newline is too, a
/// rule kept from when the script itself rode the window's command line,
/// where a shell ends the command at one; encoded it would survive, but no
/// agent's argv or env relies on an embedded newline.
///
/// The script is not what the window runs: psmux runs a single-string window
/// command through the shell it resolves, which is `cmd.exe` on a server
/// started over Windows OpenSSH (psmux 3.4.0 #683, `cached_shell` and
/// `build_command` in its `src/pane.rs`). [`psmux_window_program`] names the
/// interpreter around it.
fn psmux_window_powershell(
    window_name: &str,
    command: &str,
    args: &[String],
    env: &HashMap<String, String>,
) -> String {
    let mut ps = String::new();
    // Sort for a deterministic command (HashMap iteration order isn't).
    let mut pairs: Vec<_> = env.iter().collect();
    pairs.sort();
    for (k, v) in pairs {
        ps.push_str(&format!("Set-Item Env:{k} {}; ", powershell_quote(v)));
    }
    ps.push_str(&format!("& {}", powershell_quote(command)));
    for a in args {
        ps.push(' ');
        ps.push_str(&powershell_quote(a));
    }
    let mut ps = ps.replace(['"', '\n'], " ");
    if WindowRole::from_window_name(window_name) == Some(WindowRole::Agent) {
        ps.push_str(EXIT_REPORT);
    }
    ps
}

/// The program a psmux window runs: [`psmux_window_powershell`] handed to
/// `powershell.exe` explicitly ([`powershell_launcher`]), so it runs in
/// PowerShell whichever shell psmux would have picked.
///
/// Two callers deliver it as **one unit** (both needed because psmux drops
/// what tmux would keep):
/// - [`psmux_window_command`] wraps it in double quotes for a control-mode
///   `new-window` line, whose parser keeps only the *first* trailing token
///   (tmux joins them).
/// - the headless spawn passes it verbatim as a single argv token
///   ([`TmuxCompatible::push_window_program`]; the argv path joins trailing
///   tokens fine, but still ignores `-e`).
///
/// Either way psmux 3.4.0 spawns it directly — a bare program with
/// arguments and no shell metacharacter (`try_direct_spawn`) — and an older
/// one through its shell, where the encoded script means nothing either.
///
/// Agent `args` and `env` come from config with no bound, so a long one can
/// take the program past what that shell runs: refused here with
/// [`crate::shell::CommandLineTooLong`], before anything reaches psmux.
fn psmux_window_program(
    window_name: &str,
    command: &str,
    args: &[String],
    env: &HashMap<String, String>,
) -> Result<String> {
    powershell_launcher(&psmux_window_powershell(window_name, command, args, env))
        .with_context(|| format!("window {window_name} cannot start on a Windows host"))
}

/// [`psmux_window_program`] framed as one **double-quoted** control-mode
/// token for a `new-window` line.
fn psmux_window_command(
    window_name: &str,
    command: &str,
    args: &[String],
    env: &HashMap<String, String>,
) -> Result<String> {
    Ok(format!(
        "\"{}\"",
        psmux_window_program(window_name, command, args, env)?
    ))
}

/// The `run-shell` script that pastes the prompt, waits a beat, then presses
/// Enter: PowerShell, since psmux's `run-shell` is not a POSIX shell, with the
/// prompt travelling as psmux's own base64 `send-paste` payload (see
/// [`send_paste_args`]), which also keeps the script free of the prompt's
/// newlines and quotes.
///
/// The whole prompt rides in that one line, so a script past
/// [`WINDOWS_CMD_LINE_MAX`] is refused with [`CommandLineTooLong`]: one
/// `run-shell` cannot be chunked, and two scheduled `-b` would race.
fn deferred_paste_script(mux: &str, socket: &str, target: &str, text: &str) -> Result<String> {
    // Only the socket: a quoted program name is a string to PowerShell,
    // not a command, and a multiplexer's binary name needs no quoting.
    let socket = powershell_quote(socket);
    let t = powershell_quote(target);
    let payload = base64::engine::general_purpose::STANDARD.encode(text.as_bytes());
    let script = format!(
        "powershell -NoProfile -Command \"{mux} -L {socket} send-paste -t {t} {payload}; \
         Start-Sleep -Milliseconds 200; \
         {mux} -L {socket} send-keys -t {t} Enter\""
    );
    if script.len() > WINDOWS_CMD_LINE_MAX {
        return Err(CommandLineTooLong {
            len: script.len(),
            max: WINDOWS_CMD_LINE_MAX,
        })
        .context("the deferred prompt cannot reach a Windows host");
    }
    Ok(script)
}

/// Shift+Enter as `agent::input::key_to_bytes` encodes it.
///
/// Under psmux it is typed as Ctrl+J, the one newline key every agent probed
/// in a psmux pane (psmux 3.3.8, Windows 11) takes as one. ConPTY drops
/// `CSI 13;2u` before an app that reads console key events sees it (vibe's
/// Rust TUI, PowerShell's line editor: nothing happened); win32-input-mode's
/// Shift+Enter reaches an app that reads VT bytes as a bare CR, which Claude
/// Code took as Enter and submitted the prompt. Ctrl+J inserted a newline in
/// vibe, Claude Code and PowerShell alike. On a selection menu it is Enter.
const SHIFT_ENTER: &[u8] = b"\x1b[13;2u";

/// Build the psmux-compatible `send-keys` command line(s) for `buf`.
///
/// psmux supports `send-keys -l` (literal text) and key-names (`Enter`, `Tab`,
/// `Escape`, `BSpace`, `C-<letter>`, …). Encode the
/// input with those primitives: contiguous printable/UTF-8 runs go
/// out as one `-l` literal command, each control byte as its key-name. Navigation
/// sequences use one named key command: splitting ESC from the rest makes psmux
/// dispatch a standalone Escape before the arrow sequence reaches the pane.
/// Shift+Enter (`CSI 13;2u`) is sent as `C-j` (see `SHIFT_ENTER`). Every other
/// escape sequence — a mouse report, an Alt chord, a modified F-key — goes out
/// whole as one literal (`psmux_escape_sequence`): split, the pane got
/// `[<35;…M` typed into it, with the ESC lost.
pub fn psmux_send_keys_commands(pane_id: &str, buf: &[u8]) -> Vec<String> {
    let mut cmds = Vec::new();
    let mut literal: Vec<u8> = Vec::new();
    let mut i = 0;
    while i < buf.len() {
        if buf[i..].starts_with(SHIFT_ENTER) {
            flush_psmux_literal(pane_id, &mut literal, &mut cmds);
            cmds.push(format!("send-keys -t {pane_id} C-j\n"));
            i += SHIFT_ENTER.len();
            continue;
        }
        if let Some((len, name)) = psmux_navigation_key(&buf[i..]) {
            flush_psmux_literal(pane_id, &mut literal, &mut cmds);
            cmds.push(format!("send-keys -t {pane_id} {name}\n"));
            i += len;
            continue;
        }
        if let Some((len, args)) = psmux_escape_sequence(&buf[i..]) {
            flush_psmux_literal(pane_id, &mut literal, &mut cmds);
            cmds.push(format!("send-keys -t {pane_id} -l -N 1 {args}\n"));
            i += len;
            continue;
        }
        let b = buf[i];
        match psmux_key_name(b) {
            Some(name) => {
                flush_psmux_literal(pane_id, &mut literal, &mut cmds);
                cmds.push(format!("send-keys -t {pane_id} {name}\n"));
            }
            None => literal.push(b),
        }
        i += 1;
    }
    flush_psmux_literal(pane_id, &mut literal, &mut cmds);
    cmds
}

/// Recognize the xterm navigation encodings produced by `key_to_bytes`, plus
/// SS3 cursor keys when an application has enabled DECCKM. psmux owns the
/// pane's cursor mode and emits the right form for an unmodified named key.
fn psmux_navigation_key(buf: &[u8]) -> Option<(usize, String)> {
    let prefix = buf.get(..2)?;
    if prefix != b"\x1b[" && prefix != b"\x1bO" {
        return None;
    }
    let body = &buf[2..];
    let key = |suffix| match suffix {
        b'A' => Some("Up"),
        b'B' => Some("Down"),
        b'C' => Some("Right"),
        b'D' => Some("Left"),
        b'H' => Some("Home"),
        b'F' => Some("End"),
        _ => None,
    };
    if let Some(&suffix) = body.first() {
        if let Some(name) = key(suffix) {
            return Some((3, name.to_string()));
        }
    }
    if prefix[1] != b'[' {
        return None;
    }
    if body.len() >= 2 && body[1] == b'~' {
        let name = match body[0] {
            b'2' => "Insert",
            b'3' => "Delete",
            b'5' => "PageUp",
            b'6' => "PageDown",
            _ => return None,
        };
        return Some((4, name.to_string()));
    }
    // The longest supported form has four bytes after CSI (for example 5;5~).
    let nav = &body[..body.len().min(4)];
    let semicolon = nav.iter().position(|&b| b == b';')?;
    let suffix_pos = nav
        .iter()
        .position(|&b| matches!(b, b'A'..=b'D' | b'H' | b'F' | b'~'))?;
    if suffix_pos <= semicolon || suffix_pos != semicolon + 2 {
        return None;
    }
    let modifier = body[semicolon + 1];
    if !(b'2'..=b'8').contains(&modifier) {
        return None;
    }
    let base = if body[suffix_pos] == b'~' {
        match &body[..semicolon] {
            b"2" => "Insert",
            b"3" => "Delete",
            b"5" => "PageUp",
            b"6" => "PageDown",
            _ => return None,
        }
    } else if &body[..semicolon] == b"1" {
        key(body[suffix_pos])?
    } else {
        return None;
    };
    let bits = modifier - b'1';
    let mut name = String::new();
    if bits & 4 != 0 {
        name.push_str("C-");
    }
    if bits & 2 != 0 {
        name.push_str("M-");
    }
    if bits & 1 != 0 {
        name.push_str("S-");
    }
    name.push_str(base);
    Some((suffix_pos + 3, name))
}

/// A complete escape sequence at the head of `buf` that has no psmux key-name,
/// as its length and the `send-keys -l` arguments that type it whole: the ESC
/// as psmux's `0x1b` codepoint token, the rest as a literal. `None` for a lone
/// or unfinished ESC, which stays the `Escape` key.
///
/// One literal is what reaches the pane as one read (verified on psmux 3.3.8
/// over control mode); `send-keys -H` would too, but psmux 3.3.6 lacked it.
/// Recognised: a CSI sequence (with xterm's `CSI M` mouse report, whose three
/// fields follow the final byte), an SS3 key, and an Alt chord (ESC + one
/// printable character).
fn psmux_escape_sequence(buf: &[u8]) -> Option<(usize, String)> {
    if buf.first() != Some(&0x1b) {
        return None;
    }
    let (len, rest) = match *buf.get(1)? {
        b'[' | b'O' if buf.len() == 2 => (2, &buf[1..]),
        b'[' => {
            let body = &buf[2..];
            let params = body
                .iter()
                .take_while(|b| (0x30..=0x3f).contains(*b))
                .count();
            let inter = body[params..]
                .iter()
                .take_while(|b| (0x20..=0x2f).contains(*b))
                .count();
            let fin = *body.get(params + inter)?;
            if !(0x40..=0x7e).contains(&fin) {
                return None;
            }
            let end = 2 + params + inter + 1;
            if fin == b'M' && params + inter == 0 {
                // xterm's default/UTF-8 mouse encoding: three fields, each one
                // character offset by 32, after the final byte.
                let (fields, used) = mouse_fields(&buf[end..])?;
                let mut args = vec!["0x1b".to_string(), psmux_quote("[M")];
                args.extend(fields.iter().map(|c| format!("0x{:x}", *c as u32)));
                return Some((end + used, args.join(" ")));
            }
            (end, &buf[1..end])
        }
        b'O' => {
            let key = *buf.get(2)?;
            if !(0x40..=0x7e).contains(&key) {
                return None;
            }
            (3, &buf[1..3])
        }
        // Alt + a printable character (UTF-8 included); Alt + a control byte
        // stays Escape and that byte's key-name.
        first => {
            let end = 1 + utf8_width(first);
            let ch = std::str::from_utf8(buf.get(1..end)?).ok()?.chars().next()?;
            if ch.is_control() {
                return None;
            }
            (end, &buf[1..end])
        }
    };
    let rest = std::str::from_utf8(rest).ok()?;
    Some((len, format!("0x1b {}", psmux_literal_args(rest))))
}

/// The three fields of a `CSI M` mouse report: each a UTF-8 character (`?1005`)
/// or, where the bytes are not UTF-8, a single raw byte (the default encoding's
/// 8-bit coordinates), with the bytes they took. psmux types a field as its
/// codepoint, so a raw byte past 0x7f — a coordinate past 95 in the default
/// encoding — arrives as that codepoint's UTF-8; such an app should ask for SGR.
fn mouse_fields(buf: &[u8]) -> Option<([char; 3], usize)> {
    let mut fields = ['\0'; 3];
    let mut used = 0;
    for field in &mut fields {
        let tail = buf.get(used..)?;
        let first = *tail.first()?;
        let width = utf8_width(first);
        match tail.get(..width).and_then(|s| std::str::from_utf8(s).ok()) {
            Some(s) => {
                *field = s.chars().next()?;
                used += width;
            }
            None => {
                *field = char::from(first);
                used += 1;
            }
        }
    }
    Some((fields, used))
}

/// The length of the UTF-8 sequence `first` leads, read from its lead byte.
fn utf8_width(first: u8) -> usize {
    match first {
        0xc0..=0xdf => 2,
        0xe0..=0xef => 3,
        0xf0..=0xf7 => 4,
        _ => 1,
    }
}

/// Map a control byte to the psmux key-name that injects exactly that byte, or
/// `None` for a printable / UTF-8 byte (which joins an `-l` literal run).
fn psmux_key_name(b: u8) -> Option<String> {
    Some(match b {
        b'\r' => "Enter".to_string(),
        b'\t' => "Tab".to_string(),
        0x1b => "Escape".to_string(),
        0x7f => "BSpace".to_string(),
        // Ctrl+letter: 0x01..=0x1a → C-a..C-z (covers e.g. LF 0x0a → C-j).
        0x01..=0x1a => format!("C-{}", (b'a' + b - 1) as char),
        _ => return None,
    })
}

/// Emit the pending printable run as one or more `send-keys -l -N 1` commands
/// and clear it. Long runs are split at `SEND_KEYS_CHUNK_BYTES` (on char
/// boundaries) so no control-mode line gets over-long.
///
/// The `-N 1` is load-bearing, not a stray repeat count. psmux's control-mode
/// reader runs every line through a send-coalescing pass
/// (`coalesce_send_commands` in psmux) that decodes each send's bytes and
/// re-emits them re-quoted with the POSIX `'\''` escape — which psmux's own
/// tokenizer cannot read back, so any `'` in the text arrived in the pane as
/// `\` (`it's` was typed as `it\s`), regardless of how the client framed it.
/// The decoder bails on a `-N` flag, letting the original line reach the
/// direct send-keys handler, whose single parse handles the argument encoding
/// of [`psmux_literal_args`] correctly. Verified against psmux 3.3.6.
fn flush_psmux_literal(pane_id: &str, literal: &mut Vec<u8>, cmds: &mut Vec<String>) {
    if literal.is_empty() {
        return;
    }
    let text = String::from_utf8_lossy(literal).into_owned();
    let emit = |chunk: &str, cmds: &mut Vec<String>| {
        cmds.push(format!(
            "send-keys -t {pane_id} -l -N 1 {}\n",
            psmux_literal_args(chunk)
        ));
    };
    let mut chunk = String::new();
    for ch in text.chars() {
        if !chunk.is_empty() && chunk.len() + ch.len_utf8() > SEND_KEYS_CHUNK_BYTES {
            emit(&chunk, cmds);
            chunk.clear();
        }
        chunk.push(ch);
    }
    if !chunk.is_empty() {
        emit(&chunk, cmds);
    }
    literal.clear();
}

/// Encode one printable run as the argument list of a psmux `send-keys -l`
/// command.
///
/// Quoting alone is not enough, because psmux classifies arguments *after*
/// tokenizing (which strips the quotes) and drops every one that
/// `starts_with('-')` as an unknown flag — so a typed `-` never reached the
/// pane (issue #920). It also rewrites any argument shaped like tmux's `0xNN`
/// hex codepoint (the encoding iTerm2's gateway sends) into the character it
/// names, so a run literally spelling `0x41` would arrive as `A`.
///
/// Both are escaped by emitting the offending *leading* character as its own
/// `0xNN` argument: psmux converts that back to the same character and, in
/// literal mode, joins the arguments with no separator, so the run is
/// reassembled exactly. Escaping repeats until the remainder is safe — `--x`
/// needs both hyphens escaped, and `-0x41` needs the hyphen and then the `0`.
fn psmux_literal_args(run: &str) -> String {
    let mut args: Vec<String> = Vec::new();
    let mut rest = run;
    while let Some(ch) = rest.chars().next() {
        if !psmux_arg_is_reinterpreted(rest) {
            break;
        }
        args.push(format!("0x{:x}", ch as u32));
        rest = &rest[ch.len_utf8()..];
    }
    if !rest.is_empty() {
        args.push(psmux_quote(rest));
    }
    args.join(" ")
}

/// Whether psmux would read `arg` as anything other than the literal text it
/// spells: a flag (any leading `-`, quoted or not) or a `0xNN` hex codepoint.
fn psmux_arg_is_reinterpreted(arg: &str) -> bool {
    if arg.starts_with('-') {
        return true;
    }
    arg.strip_prefix("0x")
        .or_else(|| arg.strip_prefix("0X"))
        .is_some_and(|hex| !hex.is_empty() && hex.chars().all(|c| c.is_ascii_hexdigit()))
}

/// Double-quote `s` for a psmux `send-keys -l` argument. Always quotes, even a
/// bare word, so whitespace never splits the run into several arguments (a
/// leading `-` needs more than quoting — see `psmux_literal_args`). Double
/// quotes — not POSIX single quotes — because psmux's tokenizer has no working
/// escape for a `'` inside `'…'`, but inside `"…"` it passes `'` through and
/// reads exactly two escapes: `\"` (literal quote) and `\\` (literal
/// backslash); any other backslash stays literal, so both are escaped here. A
/// literal run never contains a newline (LF and CR map to key-names), but the
/// control-mode line is `\n`-delimited, so newlines are replaced defensively.
/// Also the argument encoding for any other psmux control-mode line (e.g.
/// `new-window -c/-n`, [`TmuxCompatible::quote`]) — same tokenizer, so a POSIX
/// `'\''` escape would arrive mangled there too.
pub fn psmux_quote(s: &str) -> String {
    format!(
        "\"{}\"",
        s.replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('\n', " ")
    )
}

/// Max text bytes per `send-paste` command: the most whose line stays within
/// [`WINDOWS_CMD_LINE_MAX`] in its longest form. A paste to a host's server is
/// a one-shot ssh command, and when the host's socket or multiplexer is not a
/// word every Windows login shell passes through, `launch_on_windows` runs it
/// as an encoded PowerShell script: base64, then UTF-16, then base64 again,
/// so a payload byte costs 32/9 chars (issue #1503). Sized for that rather
/// than the bare line, which a socket name in `hosts.toml` alone can decide.
const PASTE_CHUNK_BYTES: usize = (WINDOWS_CMD_LINE_MAX - PASTE_LINE_RESERVE) * 9 / 32;

/// What an encoded `send-paste` line spends besides its payload: the
/// `powershell.exe … -EncodedCommand` prefix, the launch script around the
/// argv, the multiplexer's path, `-L <socket>` and `send-paste -t <pane>`.
const PASTE_LINE_RESERVE: usize = 2048;

/// Split `text` into `send-paste`-sized pieces on **char** boundaries: psmux
/// decodes the payload as UTF-8 and drops it whole if that fails, so a
/// multi-byte character must never straddle two chunks.
fn paste_chunks(text: &str) -> Vec<&str> {
    if text.len() <= PASTE_CHUNK_BYTES {
        return vec![text];
    }
    let mut chunks = Vec::new();
    let mut start = 0;
    while start < text.len() {
        let mut end = (start + PASTE_CHUNK_BYTES).min(text.len());
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        chunks.push(&text[start..end]);
        start = end;
    }
    chunks
}

/// The `send-paste` argv delivering `text` into `pane_id`.
///
/// The payload is standard base64 — psmux's own client encodes a paste the same
/// way, and it is what the server decodes. It also keeps CR/LF off the wire: a
/// raw newline inside a psmux command argument is cut by the server's
/// line-oriented read, which delivers a truncated payload and then executes the
/// tail as a psmux command (psmux #560).
fn send_paste_args(pane_id: &str, text: &str) -> Vec<String> {
    vec![
        "send-paste".to_string(),
        "-t".to_string(),
        pane_id.to_string(),
        base64::engine::general_purpose::STANDARD.encode(text.as_bytes()),
    ]
}

/// Out-of-band paste channel for a psmux backend.
///
/// psmux's control-mode dispatcher implements no paste command at all
/// (`paste-buffer`, `set-buffer` and psmux's own `send-paste` are CLI/server
/// only), and its `send-keys` encoding cannot carry a paste: every embedded CR
/// goes out as the `Enter` key, and the markers reach the pane as separate
/// writes rather than around the text — a pasted stack trace was submitted one
/// line at a time (issue #916).
///
/// So a paste is handed to psmux's *own* paste path with a one-shot
/// `psmux send-paste` (the same command psmux's client uses for a Ctrl+Shift+V):
/// it normalizes CRLF for ConPTY, writes the markers contiguously with the text,
/// and adds them only when the pane's app actually enabled bracketed paste.
/// Verified present since psmux 3.3.6.
#[derive(Debug, Clone)]
struct PsmuxPaste {
    transport: TmuxTransport,
    socket: String,
}

impl PsmuxPaste {
    fn new(transport: TmuxTransport, socket: String) -> Self {
        Self { transport, socket }
    }

    /// Deliver `text` to `pane_id` as a paste. Blocks until psmux has applied it
    /// (the psmux CLI round-trips a barrier before exiting), so a keystroke
    /// written to control mode afterwards cannot overtake it. Callers reach this
    /// through the session's writer task, never the UI thread, so the wait only
    /// holds back that session's own later input — the ordering we want.
    ///
    /// A paste past [`PASTE_CHUNK_BYTES`] goes out as several commands, each its
    /// own paste — the text still arrives whole and no CR submits. An error on a
    /// *later* chunk is reported but not returned: the first chunk is in the
    /// pane already.
    fn deliver(&self, pane_id: &str, text: &str) -> Result<()> {
        for (i, chunk) in paste_chunks(text).into_iter().enumerate() {
            if let Err(e) = self.send_one(pane_id, chunk) {
                if i == 0 {
                    return Err(e);
                }
                warn!("psmux send-paste truncated after {i} chunk(s): {e:#}");
                return Ok(());
            }
        }
        Ok(())
    }

    fn send_one(&self, pane_id: &str, text: &str) -> Result<()> {
        let args = send_paste_args(pane_id, text);
        let argv: Vec<&str> = args.iter().map(String::as_str).collect();
        let out = self
            .transport
            .tmux_command(&self.socket, &argv)?
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .output()
            .context("failed to run psmux send-paste")?;
        if !out.status.success() {
            bail!(
                "psmux send-paste exited with {}: {}",
                out.status,
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    fn cold_start_backend(root: &tempfile::TempDir, scenario: &str) -> PsmuxBackend {
        use std::os::unix::fs::PermissionsExt;

        let mux = root.path().join("psmux-probe");
        let session = root.path().join("session");
        let attempts = root.path().join("attempts");
        let script = format!(
            "#!/bin/sh\n\
             session='{}'\n\
             attempts='{}'\n\
             scenario='{}'\n\
             [ \"$1\" = -L ] && shift 2\n\
             case \"$1\" in\n\
               -V) echo 'psmux 3.3.8' ;;\n\
               has-session)\n\
                 if [ \"$scenario\" = destructive_probe ] && [ -f \"$session\" ]; then\n\
                   rm \"$session\"\n\
                   exit 1\n\
                 fi\n\
                 test -f \"$session\" ;;\n\
               list-windows)\n\
                 test -f \"$session\" || exit 1\n\
                 case \"$scenario\" in\n\
                   namesake_*)\n\
                     echo '%1|tb-check|0'\n\
                     if [ -f \"$attempts.window_created\" ]; then\n\
                       if [ \"$scenario\" = namesake_lives ]; then\n\
                         echo '%2|tb-check|0'\n\
                       elif [ \"$scenario\" = namesake_dies_later ] && \
                            [ ! -f \"$attempts.listed\" ]; then\n\
                         touch \"$attempts.listed\"\n\
                         echo '%2|tb-check|0'\n\
                       fi\n\
                     fi ;;\n\
                   *)\n\
                     if [ -f \"$attempts.window_created\" ] && [ \"$scenario\" != window_dies ]; then\n\
                       echo '%1|tb-check|0'\n\
                     fi ;;\n\
                 esac ;;\n\
               display-message) echo '3.3.8' ;;\n\
               new-session)\n\
                 if [ \"$scenario\" = needs_unsized_bootstrap ]; then\n\
                   for arg do\n\
                     if [ \"$arg\" = -x ] || [ \"$arg\" = -y ]; then\n\
                       echo \"psmux: failed to create session 'thurbox'\" >&2\n\
                       exit 1\n\
                     fi\n\
                   done\n\
                 fi\n\
                 count=0\n\
                 [ -f \"$attempts\" ] && count=$(wc -l < \"$attempts\")\n\
                 echo attempt >> \"$attempts\"\n\
                 if [ \"$scenario\" = delayed_after_refusal ]; then\n\
                   if [ \"$count\" -eq 0 ]; then\n\
                     (sleep 0.3; touch \"$session\") >/dev/null 2>&1 &\n\
                   fi\n\
                   echo \"psmux: failed to create session 'thurbox'\" >&2\n\
                   exit 1\n\
                 fi\n\
                 if [ \"$scenario\" = always_refused ] || \
                    {{ [ \"$scenario\" = refused_once ] && [ \"$count\" -eq 0 ]; }}; then\n\
                   echo \"psmux: failed to create session 'thurbox'\" >&2\n\
                   exit 1\n\
                 fi\n\
                 if [ \"$scenario\" = vanished_once ] && [ \"$count\" -eq 0 ]; then\n\
                   exit 0\n\
                 fi\n\
                 touch \"$session\" ;;\n\
               set-option)\n\
                 if [ ! -f \"$session\" ]; then\n\
                   echo 'psmux: no server running' >&2\n\
                   exit 1\n\
                 fi\n\
                 if [ \"$scenario\" = needs_server_target ]; then\n\
                   global=false; targeted=false; prev=''\n\
                   for arg do\n\
                     [ \"$arg\" = -g ] && global=true\n\
                     [ \"$prev\" = -t ] && [ \"$arg\" = thurbox ] && targeted=true\n\
                     prev=$arg\n\
                   done\n\
                   if [ \"$global\" = true ] && [ \"$targeted\" != true ]; then\n\
                     echo 'psmux: no server running on session private-socket__default' >&2\n\
                     exit 1\n\
                   fi\n\
                 fi\n\
                 if [ \"$scenario\" = transient_option_failure ] && \
                    [ ! -f \"$attempts.option_ready\" ]; then\n\
                   if [ ! -f \"$attempts.option_started\" ]; then\n\
                     touch \"$attempts.option_started\"\n\
                     (sleep 0.5; touch \"$attempts.option_ready\") >/dev/null 2>&1 &\n\
                   fi\n\
                   echo 'psmux: no server running on session' >&2\n\
                   exit 1\n\
                 fi\n\
                 if [ \"$scenario\" = live_option_failure ]; then\n\
                   echo 'invalid option' >&2\n\
                   exit 1\n\
                 fi ;;\n\
               new-window)\n\
                 if [ \"$scenario\" = transient_window_failure ] && \
                    [ ! -f \"$attempts.window_ready\" ]; then\n\
                   if [ ! -f \"$attempts.window_started\" ]; then\n\
                     touch \"$attempts.window_started\"\n\
                     (sleep 0.5; touch \"$attempts.window_ready\") >/dev/null 2>&1 &\n\
                   fi\n\
                   echo 'psmux: no server running on session' >&2\n\
                   exit 1\n\
                 fi\n\
                 touch \"$attempts.window_created\" ;;\n\
               *) exit 2 ;;\n\
             esac\n",
            session.display(),
            attempts.display(),
            scenario
        );
        std::fs::write(&mux, script).expect("write fake psmux");
        std::fs::set_permissions(&mux, std::fs::Permissions::from_mode(0o700)).expect("chmod");
        PsmuxBackend::with_transport(
            TmuxTransport::local(mux.to_string_lossy()),
            "private-socket",
            "thurbox",
            "local:psmux",
        )
    }

    /// A psmux whose `list-windows` writes `stderr` and exits `code`.
    #[cfg(unix)]
    fn listing_backend(root: &tempfile::TempDir, stderr: &str, code: i32) -> PsmuxBackend {
        use std::os::unix::fs::PermissionsExt;

        let mux = root.path().join("psmux-listing");
        let script = format!("#!/bin/sh\necho \"{stderr}\" >&2\nexit {code}\n");
        std::fs::write(&mux, script).expect("write fake psmux");
        std::fs::set_permissions(&mux, std::fs::Permissions::from_mode(0o700)).expect("chmod");
        PsmuxBackend::with_transport(
            TmuxTransport::local(mux.to_string_lossy()),
            "private-socket",
            "thurbox",
            "local:psmux",
        )
    }

    /// A psmux that appends each command line it runs to `log` and answers a
    /// pane as live.
    #[cfg(unix)]
    fn recording_backend(root: &tempfile::TempDir) -> (PsmuxBackend, std::path::PathBuf) {
        use std::os::unix::fs::PermissionsExt;

        let mux = root.path().join("psmux-recording");
        let log = root.path().join("lines");
        let script = format!(
            "#!/bin/sh\necho \"$*\" >> '{}'\n[ \"$3\" = display-message ] && echo 0\nexit 0\n",
            log.display()
        );
        std::fs::write(&mux, script).expect("write fake psmux");
        std::fs::set_permissions(&mux, std::fs::Permissions::from_mode(0o700)).expect("chmod");
        let backend = PsmuxBackend::with_transport(
            TmuxTransport::local(mux.to_string_lossy()),
            "private-socket",
            "thurbox",
            "local:psmux",
        );
        (backend, log)
    }

    /// `session send` is a one-shot `send-paste` per chunk, so a long text
    /// never puts a line past what a Windows host's `cmd.exe` runs — and still
    /// arrives whole (issue #1489).
    #[cfg(unix)]
    #[test]
    fn a_long_send_text_goes_out_in_lines_a_windows_host_runs() {
        let root = tempfile::tempdir().expect("tempdir");
        let (backend, log) = recording_backend(&root);
        let text = "é".repeat(PASTE_CHUNK_BYTES * 2);

        backend.send_text("%1", &text, false).expect("send");

        let lines = std::fs::read_to_string(&log).expect("log");
        let pastes: Vec<&str> = lines.lines().filter(|l| l.contains("send-paste")).collect();
        assert!(pastes.len() > 1, "{} paste line(s)", pastes.len());
        let mut sent = Vec::new();
        for line in &pastes {
            assert!(line.len() <= WINDOWS_CMD_LINE_MAX, "{} chars", line.len());
            let payload = line.rsplit(' ').next().expect("payload");
            sent.extend(
                base64::engine::general_purpose::STANDARD
                    .decode(payload)
                    .expect("base64"),
            );
        }
        assert_eq!(String::from_utf8(sent).expect("utf-8"), text);
    }

    /// A deferred prompt whose `run-shell` script is past the Windows limit is
    /// refused with the reason, before anything reaches psmux (issue #1489).
    #[cfg(unix)]
    #[test]
    fn a_deferred_prompt_past_the_windows_command_line_is_refused() {
        let root = tempfile::tempdir().expect("tempdir");
        let (backend, log) = recording_backend(&root);
        let prompt = "x".repeat(WINDOWS_CMD_LINE_MAX);

        let error = backend
            .send_text_after("%1", &prompt, std::time::Duration::from_secs(1))
            .unwrap_err();

        assert!(
            format!("{error:#}").contains("Windows limit is 8191"),
            "{error:#}"
        );
        assert!(!log.exists(), "{}", std::fs::read_to_string(&log).unwrap());
    }

    /// A prompt that fits is still scheduled as one `run-shell`.
    #[cfg(unix)]
    #[test]
    fn a_deferred_prompt_that_fits_is_scheduled() {
        let root = tempfile::tempdir().expect("tempdir");
        let (backend, log) = recording_backend(&root);

        backend
            .send_text_after("%1", "hello", std::time::Duration::from_secs(1))
            .expect("scheduled");

        let lines = std::fs::read_to_string(&log).expect("log");
        assert!(lines.contains("run-shell -b -d 1"), "{lines}");
    }

    /// psmux 3.3.8 exits 1 with this wording when its server is gone. That is
    /// its answer that nothing is there — what lets a restart respawn a
    /// session whose server died — not a listing that went unanswered.
    #[cfg(unix)]
    #[test]
    fn psmux_no_server_reply_is_an_empty_listing() {
        let root = tempfile::tempdir().expect("tempdir");
        let backend = listing_backend(
            &root,
            "psmux: no server running on session 'private-socket__thurbox'",
            1,
        );
        assert!(backend.discover_answered().expect("absence").is_empty());
    }

    /// The same words behind exit 0 are a hidden failure, not an answer.
    #[cfg(unix)]
    #[test]
    fn psmux_stderr_behind_a_zero_exit_is_still_an_error() {
        let root = tempfile::tempdir().expect("tempdir");
        let backend = listing_backend(&root, "psmux: something went wrong", 0);
        assert!(backend.discover_answered().is_err());
    }

    #[cfg(unix)]
    #[test]
    fn psmux_no_server_reply_with_unexpected_stdout_is_an_error() {
        use std::os::unix::fs::PermissionsExt;

        let root = tempfile::tempdir().expect("tempdir");
        let mux = root.path().join("psmux-listing");
        std::fs::write(
            &mux,
            "#!/bin/sh\nprintf '%s\\n' 'malformed listing'\nprintf '%s\\n' \"psmux: no server running on session 'private-socket__thurbox'\" >&2\nexit 1\n",
        )
        .expect("write fake psmux");
        std::fs::set_permissions(&mux, std::fs::Permissions::from_mode(0o700)).expect("chmod");
        let backend = PsmuxBackend::with_transport(
            TmuxTransport::local(mux.to_string_lossy()),
            "private-socket",
            "thurbox",
            "local:psmux",
        );

        assert!(backend.discover_answered().is_err());
    }

    #[cfg(unix)]
    #[test]
    fn psmux_no_server_reply_with_a_second_error_is_an_error() {
        let root = tempfile::tempdir().expect("tempdir");
        let backend = listing_backend(
            &root,
            "psmux: no server running on session 'private-socket__thurbox'\npermission denied",
            1,
        );
        assert!(backend.discover_answered().is_err());
    }

    #[cfg(unix)]
    #[test]
    fn psmux_no_server_reply_mixed_with_a_same_line_error_is_an_error() {
        for stderr in [
            "permission denied: psmux: no server running on session 'private-socket__thurbox'",
            "psmux: no server running on session 'private-socket__thurbox': permission denied",
        ] {
            let root = tempfile::tempdir().expect("tempdir");
            let backend = listing_backend(&root, stderr, 1);
            assert!(backend.discover_answered().is_err(), "accepted: {stderr}");
        }
    }

    #[cfg(unix)]
    fn attempt_count(root: &tempfile::TempDir) -> usize {
        std::fs::read_to_string(root.path().join("attempts"))
            .expect("new-session was attempted")
            .lines()
            .count()
    }

    #[cfg(unix)]
    #[test]
    fn psmux_retries_a_failed_cold_session_bootstrap() {
        let root = tempfile::tempdir().expect("tempdir");
        let backend = cold_start_backend(&root, "refused_once");

        backend
            .ensure_session_configured()
            .expect("cold psmux session recovers");
        assert_eq!(attempt_count(&root), 2);
    }

    #[cfg(unix)]
    #[test]
    fn psmux_uses_its_default_initial_window_size() {
        let root = tempfile::tempdir().expect("tempdir");
        let backend = cold_start_backend(&root, "needs_unsized_bootstrap");

        backend
            .ensure_session_configured()
            .expect("psmux starts without explicit initial size");
        assert_eq!(attempt_count(&root), 1);
    }

    #[cfg(unix)]
    #[test]
    fn psmux_avoids_a_destructive_has_session_probe() {
        let root = tempfile::tempdir().expect("tempdir");
        let backend = cold_start_backend(&root, "destructive_probe");

        backend
            .ensure_session_configured()
            .expect("bootstrap uses a non-destructive session probe");
        assert_eq!(attempt_count(&root), 1);
    }

    #[cfg(unix)]
    #[test]
    fn psmux_waits_for_a_late_server_after_create_refusals() {
        let root = tempfile::tempdir().expect("tempdir");
        let backend = cold_start_backend(&root, "delayed_after_refusal");

        backend
            .ensure_session_configured()
            .expect("late server becomes available on the same socket");
        assert!(attempt_count(&root) <= 3);
    }

    #[cfg(unix)]
    #[test]
    fn psmux_retries_when_a_successful_create_left_no_server() {
        let root = tempfile::tempdir().expect("tempdir");
        let backend = cold_start_backend(&root, "vanished_once");

        backend
            .ensure_session_configured()
            .expect("cold psmux session recovers");
        assert_eq!(attempt_count(&root), 2);
    }

    #[cfg(unix)]
    #[test]
    fn psmux_does_not_retry_an_option_error_on_a_live_session() {
        let root = tempfile::tempdir().expect("tempdir");
        let backend = cold_start_backend(&root, "live_option_failure");

        let error = backend.ensure_session_configured().unwrap_err().to_string();
        assert!(error.contains("invalid option"), "{error}");
        assert_eq!(attempt_count(&root), 1);
    }

    #[cfg(unix)]
    #[test]
    fn psmux_retries_a_transient_option_failure_on_a_live_session() {
        let root = tempfile::tempdir().expect("tempdir");
        let backend = cold_start_backend(&root, "transient_option_failure");

        backend
            .ensure_session_configured()
            .expect("psmux recovers after a temporary connection failure");
        assert_eq!(attempt_count(&root), 1);
    }

    #[cfg(unix)]
    #[test]
    fn psmux_targets_global_options_at_its_session() {
        let root = tempfile::tempdir().expect("tempdir");
        let backend = cold_start_backend(&root, "needs_server_target");

        backend
            .ensure_session_configured()
            .expect("global options route to the live psmux session");
        assert_eq!(attempt_count(&root), 1);
    }

    #[cfg(unix)]
    #[test]
    fn psmux_retries_a_window_refused_during_cold_start() {
        let root = tempfile::tempdir().expect("tempdir");
        let backend = cold_start_backend(&root, "transient_window_failure");
        let env = HashMap::new();
        let spec = crate::backend::contract::WindowSpec {
            owner: crate::backend::contract::Owner::new("session-id", "check"),
            role: crate::backend::contract::WindowRole::Agent,
            command: "cmd.exe",
            args: &[],
            cwd: None,
            env: &env,
        };

        backend
            .create_window(&spec)
            .expect("window appears after the server answers");
        assert!(root.path().join("attempts.window_created").exists());
    }

    /// psmux keeps no window once its program exits, so `new-window`
    /// succeeding says nothing about whether the program started: a window
    /// gone by the time it is looked for again is a failed create, not a
    /// session reported `created` with nothing behind it.
    #[cfg(unix)]
    #[test]
    fn psmux_refuses_a_window_that_died_at_once() {
        let root = tempfile::tempdir().expect("tempdir");
        let backend = cold_start_backend(&root, "window_dies");
        let env = HashMap::new();
        let spec = crate::backend::contract::WindowSpec {
            owner: crate::backend::contract::Owner::new("session-id", "check"),
            role: crate::backend::contract::WindowRole::Agent,
            command: "no-such-agent",
            args: &[],
            cwd: None,
            env: &env,
        };

        let error = backend.create_window(&spec).unwrap_err().to_string();
        assert!(root.path().join("attempts.window_created").exists());
        assert!(error.contains("tb-check exited"), "{error}");
        assert!(error.contains("`no-such-agent`"), "{error}");
    }

    #[cfg(unix)]
    fn check_spec(env: &HashMap<String, String>) -> crate::backend::contract::WindowSpec<'_> {
        crate::backend::contract::WindowSpec {
            owner: crate::backend::contract::Owner::new("session-id", "check"),
            role: crate::backend::contract::WindowRole::Agent,
            command: "no-such-agent",
            args: &[],
            cwd: None,
            env,
        }
    }

    /// A namesake is allowed, and psmux windows carry no stamp, so a live
    /// `tb-check` that was there before says nothing about the new one: a
    /// window that never showed up is refused next to it (issue #1456).
    #[cfg(unix)]
    #[test]
    fn psmux_refuses_a_dead_window_next_to_a_live_namesake() {
        let root = tempfile::tempdir().expect("tempdir");
        let backend = cold_start_backend(&root, "namesake_dies");
        let env = HashMap::new();

        let error = backend
            .create_window(&check_spec(&env))
            .unwrap_err()
            .to_string();
        assert!(error.contains("tb-check exited"), "{error}");
    }

    /// The new window's pane is the one checked after the settle, not any live
    /// pane under its name.
    #[cfg(unix)]
    #[test]
    fn psmux_refuses_its_own_pane_dying_while_a_namesake_lives() {
        let root = tempfile::tempdir().expect("tempdir");
        let backend = cold_start_backend(&root, "namesake_dies_later");
        let env = HashMap::new();

        let error = backend
            .create_window(&check_spec(&env))
            .unwrap_err()
            .to_string();
        assert!(error.contains("tb-check exited"), "{error}");
    }

    /// An agent whose window program is past what `cmd.exe` will run is
    /// refused with the reason, before anything reaches psmux — not created as
    /// a window that dies at once and reads as a dead spawn (issue #1457).
    #[cfg(unix)]
    #[test]
    fn psmux_refuses_a_window_program_past_the_windows_command_line() {
        let root = tempfile::tempdir().expect("tempdir");
        let backend = cold_start_backend(&root, "ok");
        let env = HashMap::new();
        let args = vec![format!("--append-system-prompt={}", "x".repeat(3_500))];
        let spec = crate::backend::contract::WindowSpec {
            args: &args,
            ..check_spec(&env)
        };

        let error = format!("{:#}", backend.create_window(&spec).unwrap_err());
        assert!(error.contains("Windows limit is 8191"), "{error}");
        assert!(
            !root.path().join("attempts.window_created").exists(),
            "new-window ran"
        );
    }

    #[cfg(unix)]
    #[test]
    fn psmux_answers_the_new_pane_beside_a_live_namesake() {
        let root = tempfile::tempdir().expect("tempdir");
        let backend = cold_start_backend(&root, "namesake_lives");
        let env = HashMap::new();

        let pane = backend.create_window(&check_spec(&env)).expect("created");
        assert_eq!(pane.as_str(), "%2");
    }

    #[cfg(unix)]
    #[test]
    fn psmux_stops_after_three_failed_cold_bootstraps() {
        let root = tempfile::tempdir().expect("tempdir");
        let backend = cold_start_backend(&root, "always_refused");

        let error = backend.ensure_session_configured().unwrap_err().to_string();
        assert!(error.contains("Failed to create tmux session"), "{error}");
        assert_eq!(attempt_count(&root), 3);
    }

    #[test]
    fn psmux_surveys_live_panes_when_close_notifications_are_unavailable() {
        let host = crate::session::HostDef {
            name: "windows".into(),
            multiplexer: Some("psmux".into()),
            ..Default::default()
        };
        assert!(PsmuxBackend::for_host(&host).needs_liveness_poll());
        assert!(
            PsmuxBackend::local().needs_liveness_poll(),
            "on this machine too, whatever OS it is"
        );
    }

    /// psmux 3.3.8 answers `set-option -s` with "unknown flag -s", which failed
    /// every session setup against it; 3.3.7 took either scope.
    #[test]
    fn psmux_server_options_are_set_in_the_global_scope() {
        assert_eq!(Psmux::SERVER_SCOPE, "-g");
    }

    /// psmux 3.3.6 answers `-V` with a bare `tmux 3.3.6`, which the tmux gate
    /// reads as tmux 3.3 and passes. Its server then hands panes born after a
    /// `send-keys C-c` std handles that are no longer the pane's console, and
    /// the agent reports "stdin is unreadable (EISDIR)" and exits.
    #[test]
    fn psmux_older_than_3_3_7_is_refused_with_the_upgrade() {
        let err = check_psmux_version("tmux 3.3.6\n", "thurbox")
            .unwrap_err()
            .to_string();
        assert!(err.contains("3.3.6"), "{err}");
        assert!(err.contains("3.3.7"), "{err}");
        assert!(err.contains("`psmux -L thurbox kill-server`"), "{err}");
        assert!(check_psmux_version("tmux 3.3.5", "thurbox").is_err());
        assert!(check_psmux_version("psmux 3.2.9", "thurbox").is_err());
    }

    /// `#{version}` is answered by the running server, which is what matters:
    /// upgrading the binary leaves a server started before it on the old code.
    /// The heartbeat asks a backend's `check_available` before it starts a
    /// server, so an old psmux is refused there too, not only where panes are
    /// born.
    #[test]
    fn an_old_psmux_banner_is_refused_before_a_server_starts() {
        assert!(Psmux::check_banner("tmux 3.3.6\n", "thurbox").is_err());
        assert!(Psmux::check_banner("tmux 3.3.8\npsmux 3.3.8 (x)\n", "thurbox").is_ok());
    }

    /// An old binary on `PATH` beside a server that is new enough is no
    /// reason to refuse: the server is what births panes. Without a server, or
    /// with an old one, the banner's refusal stands.
    #[test]
    fn a_safe_running_server_outranks_an_old_binary() {
        use crate::backend::tmux_compat::server::admit_banner;
        let old = Psmux::check_banner("tmux 3.3.6\n", "thurbox");
        assert!(admit_banner::<Psmux>(old, Some("3.3.8".into()), "thurbox").is_ok());
        let old = Psmux::check_banner("tmux 3.3.6\n", "thurbox");
        assert!(admit_banner::<Psmux>(old, Some("3.3.6".into()), "thurbox").is_err());
        let old = Psmux::check_banner("tmux 3.3.6\n", "thurbox");
        assert!(admit_banner::<Psmux>(old, None, "thurbox").is_err());
    }

    #[test]
    fn a_running_server_is_judged_by_its_own_version() {
        assert!(check_psmux_version("3.3.6\n", "thurbox").is_err());
        assert!(check_psmux_version("3.3.8", "thurbox").is_ok());
    }

    #[test]
    fn psmux_3_3_7_and_newer_is_accepted() {
        assert!(
            check_psmux_version("tmux 3.3.8\npsmux 3.3.8 (66cf613 2026-08-18)\n", "thurbox")
                .is_ok()
        );
        assert!(check_psmux_version("tmux 3.3.7\npsmux 3.3.7", "thurbox").is_ok());
        assert!(check_psmux_version("psmux 3.4.0", "thurbox").is_ok());
        assert!(check_psmux_version("psmux 4.0", "thurbox").is_ok());
        // A pre-release suffix on the patch is still that patch, not 0.
        assert!(check_psmux_version("psmux 3.3.9-dev", "thurbox").is_ok());
    }

    /// Measured on psmux 3.3.7: `kill-window -t thurbox:.%8` killed the
    /// session's active window, and a bare `kill-window -t @7` another
    /// session's `@7`. A pane's window goes by `kill-pane` on the qualified
    /// pane; a window by id goes qualified; a window by name is already
    /// qualified and is left as it is.
    #[test]
    fn a_psmux_kill_reaches_the_pane_it_names_in_thurboxs_session() {
        let psmux = PsmuxBackend::local();
        let session = psmux.session.clone();
        assert_eq!(
            psmux.kill_command("%8"),
            ("kill-pane", format!("{session}:.%8").into())
        );
        assert_eq!(
            psmux.kill_command("@7"),
            ("kill-window", format!("{session}:@7").into())
        );
        assert_eq!(
            psmux.kill_command("thurbox:=tb-x"),
            ("kill-window", "thurbox:=tb-x".into())
        );
    }

    /// Every pane a psmux command names goes out in thurbox's session, never as
    /// a bare `%N` the server could route to its `__warm__` session.
    #[test]
    fn a_psmux_pane_target_names_thurboxs_session() {
        let psmux = PsmuxBackend::local();
        assert_eq!(psmux.target("%2"), format!("{}:.%2", psmux.session));
    }

    /// A close is confirmed against the session's own panes, listed in the
    /// framing psmux's tokenizer reads.
    #[test]
    fn a_psmux_close_is_confirmed_against_the_sessions_panes() {
        let policy = Psmux::control_policy(&TmuxTransport::local("psmux"), "thurbox");
        assert_eq!(
            policy.confirm_close.as_deref(),
            Some("list-panes -s -t \"thurbox\" -F \"#{pane_id}\"")
        );
    }

    /// A banner this cannot read says nothing about the fix, and refusing it
    /// would lock out every later psmux that changes how it prints `-V`.
    #[test]
    fn an_unreadable_psmux_banner_is_not_refused() {
        assert!(check_psmux_version("", "thurbox").is_ok());
        assert!(check_psmux_version("psmux (dev build)", "thurbox").is_ok());
    }

    /// psmux gets its own `send-paste`: the bracketed markers are psmux's to add,
    /// and the base64 payload keeps the prompt's newlines off a command wire that
    /// would otherwise cut the line and run the tail as a command (psmux #560).
    #[test]
    fn paste_prompt_args_uses_send_paste_for_psmux() {
        let args = Psmux::paste_args("thurbox:tb-demo", "line one\nline two").concat();
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

    /// The script a window program hands `powershell.exe`, decoded back.
    fn decoded_script(program: &str) -> String {
        let encoded = program
            .strip_prefix("powershell.exe -NoLogo -EncodedCommand ")
            .unwrap_or_else(|| panic!("not an explicit PowerShell launch: {program}"));
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .expect("base64");
        let units: Vec<u16> = bytes
            .chunks_exact(2)
            .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
            .collect();
        String::from_utf16(&units).expect("UTF-16LE")
    }

    #[test]
    fn psmux_window_command_is_one_double_quoted_token() {
        let args = vec!["--session-id".to_string(), "abc-123".to_string()];
        let cmd = psmux_window_command("tbs-x", "claude", &args, &HashMap::new()).unwrap();
        let program = cmd
            .strip_prefix('"')
            .and_then(|c| c.strip_suffix('"'))
            .expect("double-quoted");
        assert_eq!(
            decoded_script(program),
            "& 'claude' '--session-id' 'abc-123'"
        );
    }

    /// The window runs PowerShell by name, whatever shell psmux resolves: on a
    /// server started over Windows OpenSSH that shell is `cmd.exe`, which
    /// killed every window handed a bare PowerShell script. Nothing in the
    /// program is syntax to `cmd.exe`, PowerShell or psmux's tokenizer, and it
    /// is the same program on the control-mode and the headless argv path.
    #[test]
    fn the_window_program_names_powershell_and_carries_no_shell_syntax() {
        let mut env = HashMap::new();
        env.insert("THURBOX_SESSION".to_string(), "id-1".to_string());
        let args = vec!["it's".to_string(), "a & b | c; %PATH% $x".to_string()];
        let program = psmux_window_program("tb-x", "C:\\Tools\\claude.exe", &args, &env).unwrap();
        let tokens: Vec<&str> = program.split(' ').collect();
        assert_eq!(
            tokens[..3],
            ["powershell.exe", "-NoLogo", "-EncodedCommand"]
        );
        assert_eq!(tokens.len(), 4, "{program}");
        assert!(tokens[3]
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '/' | '=')));
        assert_eq!(
            decoded_script(&program),
            psmux_window_powershell("tb-x", "C:\\Tools\\claude.exe", &args, &env)
        );

        let mut cmd = Command::new("psmux");
        Psmux::push_window_program(&mut cmd, "tb-x", "C:\\Tools\\claude.exe", &args, &env).unwrap();
        let argv: Vec<String> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(argv, [program]);
    }

    /// An ordinary agent window — every `THURBOX_*` variable a spawn sets, a
    /// pinned session id and the exit report — is well inside the limit
    /// a Windows host puts on its program (issue #1457).
    #[test]
    fn a_typical_agent_window_fits_a_windows_command_line_with_room() {
        let id = "0b9f3c62-7d1e-4a55-9a43-2f6c1e8d7b10";
        let env: HashMap<String, String> = [
            ("THURBOX_SESSION", id),
            ("THURBOX_SOCKET", "thurbox"),
            (
                "THURBOX_METRICS_DIR",
                "C:\\Users\\user\\AppData\\Local\\thurbox\\metrics",
            ),
            (
                "THURBOX_DATA_DIR",
                "C:\\Users\\user\\AppData\\Local\\thurbox",
            ),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
        let args = vec!["--session-id".to_string(), id.to_string()];

        let program = psmux_window_program("tb-check", "claude", &args, &env).unwrap();
        assert!(
            program.len() < WINDOWS_CMD_LINE_MAX / 2,
            "{}",
            program.len()
        );
    }

    #[test]
    fn psmux_window_script_folds_env_as_set_item() {
        // `Set-Item Env:K 'v'` (not `$env:K`) keeps the string `$`-free; sorted
        // for determinism. Values with spaces survive the PS single quotes.
        let mut env = HashMap::new();
        env.insert("THURBOX_SESSION".to_string(), "id-1".to_string());
        env.insert("B".to_string(), "x y".to_string());
        assert_eq!(
            psmux_window_powershell("tbs-x", "claude", &[], &env),
            "Set-Item Env:B 'x y'; Set-Item Env:THURBOX_SESSION 'id-1'; & 'claude'"
        );
    }

    #[test]
    fn an_agent_window_reports_its_exit() {
        assert_eq!(
            psmux_window_powershell("tb-x", "claude", &[], &HashMap::new()),
            "& 'claude'; Get-Variable LASTEXITCODE -ValueOnly -ErrorAction Ignore \
             | thurbox-cli session exited"
        );
        assert!(!EXIT_REPORT.contains(['"', '\n', '$', '{', '}']));
    }

    #[test]
    fn only_an_agent_window_reports_its_exit() {
        for name in ["tbs-x", "tbp-x", "other"] {
            let cmd = psmux_window_powershell(name, "pwsh", &[], &HashMap::new());
            assert!(!cmd.contains("session exited"), "{name}: {cmd}");
        }
    }

    #[test]
    fn psmux_window_script_escapes_and_sanitizes() {
        // A literal ' doubles (PowerShell escaping); a raw " or newline is
        // neutralized to a space (Windows PowerShell 5.1 would hand the program
        // an unescaped `"`). Backslash paths pass through untouched.
        let args = vec!["it's".to_string(), "say \"hi\"\nnow".to_string()];
        let cmd = psmux_window_powershell("tbs-x", "C:\\Tools\\claude.exe", &args, &HashMap::new());
        assert_eq!(cmd, "& 'C:\\Tools\\claude.exe' 'it''s' 'say  hi  now'");
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
        let backend = PsmuxBackend::for_host(&host);
        assert_eq!(backend.login_wrap_for_remote("claude"), "claude");
        let cmd = Psmux::window_command(&backend, "tbs-x", "claude", &[], &HashMap::new()).unwrap();
        assert_eq!(
            decoded_script(cmd.trim_matches('"')),
            "& 'claude'",
            "the window's command is the PowerShell launch, never a POSIX wrap"
        );
    }

    #[test]
    fn a_deferred_prompt_names_the_servers_own_mux_and_socket() {
        let psmux = deferred_paste_script("psmux", "sock", "%3", "it's\nhere").expect("fits");
        assert!(psmux.starts_with("powershell -NoProfile -Command \"psmux -L 'sock' send-paste"));
        // Base64: the prompt's newline and quote never reach the script.
        assert!(!psmux.contains("it's"), "{psmux}");
    }

    #[test]
    fn a_deferred_prompt_quotes_a_socket_name_the_host_configured() {
        let psmux = deferred_paste_script("psmux", "my sock", "%3", "hi").expect("fits");
        assert!(psmux.contains("psmux -L 'my sock' send-paste"), "{psmux}");
        assert!(psmux.contains("psmux -L 'my sock' send-keys"), "{psmux}");
    }

    /// tmux answers the `attach-session` carried on argv with a `%begin`/`%end`
    /// block of its own; psmux does not, and its command counter proves it —
    /// the client's first command is numbered 1, so the attach was never
    /// numbered (ADR-13, issue #1168). Draining a block psmux will never send
    /// is a blocking read that returns only when psmux closes the pipe. Nor
    /// does psmux subscribe, frame its blocks with tags, or have a producer for
    /// a status poll while the hook rewrite is gated off.
    #[test]
    fn a_psmux_connection_expects_only_what_psmux_does() {
        let ssh = TmuxTransport::remote(
            HostLauncher::Ssh {
                destination: "me@winbox".into(),
                ssh_opts: Vec::new(),
            },
            "psmux",
        );
        for transport in [TmuxTransport::local("psmux"), ssh] {
            let policy = Psmux::control_policy(&transport, "thurbox");
            assert!(!policy.implicit_attach_reply);
            assert!(!policy.tagged_blocks);
            assert!(!policy.subscriptions);
            assert_eq!(
                policy.status_poll.is_some(),
                transport.is_remote() && Psmux::HOOK_STATUS
            );
        }
        assert_eq!(
            hook_poll_command("a\"b\\c"),
            "list-panes -s -t \"abc\" -F \"#{pane_id} #{@thurbox_state}\""
        );
    }

    /// The line ssh hands a Windows host's login shell: its words after the
    /// destination, space-joined, as ssh sends them.
    fn host_line(cmd: &Command) -> String {
        let args: Vec<String> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        let dest = args.iter().position(|a| a == "me@winbox").expect("dest");
        args[dest + 1..].join(" ")
    }

    /// Windows OpenSSH runs a one-shot command through its login shell —
    /// `cmd.exe` by default, PowerShell when configured — and `cmd.exe`
    /// ignores single quotes, so a POSIX-quoted `'#{pane_id}|#{window_name}'`
    /// was a pipe into a command named `'#{window_name}'`, and remote teardown
    /// could not list the host's windows (issue #1460). The line must be words
    /// no shell reads as syntax, and still carry psmux's argv exactly.
    #[test]
    fn a_one_shot_reaches_a_windows_login_shell_as_plain_words() {
        let host = HostDef {
            name: "winbox".into(),
            destination: "me@winbox".into(),
            multiplexer: Some("psmux".into()),
            ..Default::default()
        };
        let backend = PsmuxBackend::for_host(&host);
        let format = "#{pane_id}|#{window_name}|#{pane_dead}";
        let line = host_line(
            &backend
                .mux_command(&["list-windows", "-t", "thurbox", "-F", format])
                .expect("command"),
        );

        let syntax: Vec<char> = line
            .chars()
            .filter(|c| !(c.is_ascii_alphanumeric() || " -_./:=+%".contains(*c)))
            .collect();
        assert!(
            syntax.is_empty(),
            "{syntax:?} reaches the login shell: {line}"
        );

        let encoded = line.rsplit(' ').next().expect("payload");
        let utf16 = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .expect("base64");
        let units: Vec<u16> = utf16
            .chunks(2)
            .map(|b| u16::from_le_bytes([b[0], b[1]]))
            .collect();
        let script = String::from_utf16(&units).expect("utf-16");
        assert!(
            script.contains(&format!(
                "$s.Arguments = '-L {} list-windows -t thurbox -F {format}'",
                crate::backend::instance::host_socket(&host)
            )),
            "{script}"
        );
    }

    /// A full chunk's line fits the Windows command line a one-shot runs in
    /// (issue #1457) — bare, and as the encoded PowerShell script a socket
    /// name with a space (or `@`, `,` …) sends it as, about 3.6 times its
    /// payload. Sized for the bare line only, a paste over ~2 KB to such a
    /// host was refused (issue #1503).
    #[test]
    fn a_full_paste_chunk_fits_a_windows_command_line() {
        for (socket, encoded) in [(None, false), (Some("my sock"), true)] {
            let host = HostDef {
                name: "winbox".into(),
                destination: "me@winbox".into(),
                multiplexer: Some("psmux".into()),
                socket: socket.map(Into::into),
                ..Default::default()
            };
            let backend = PsmuxBackend::for_host(&host);
            for text in ["x".repeat(2500), "é".repeat(PASTE_CHUNK_BYTES)] {
                for paste in Psmux::paste_args("%123", &text) {
                    let argv: Vec<&str> = paste.iter().map(String::as_str).collect();
                    let line = host_line(&backend.mux_command(&argv).expect("fits"));
                    assert_eq!(line.contains("-EncodedCommand"), encoded, "{line}");
                }
            }
        }
    }

    /// One stamp read back from psmux is every window's (ADR-13), so nothing
    /// is read as a stamp, written as one, or retained by window option.
    #[test]
    fn psmux_is_never_read_as_stamping_its_windows() {
        let host = crate::session::HostDef {
            name: "winbox".into(),
            destination: "me@winbox".into(),
            multiplexer: Some("psmux".into()),
            ..Default::default()
        };
        for backend in [PsmuxBackend::local(), PsmuxBackend::for_host(&host)] {
            assert!(!backend.stamps_are_per_window(), "{}", backend.name());
            assert!(!backend.supports_snapshots(), "{}", backend.name());
        }
    }

    // --- psmux send-keys encoding tests ---
    //
    // Regression: psmux 3.3.6 has no `send-keys -H`, so on Windows the hex path
    // injected the literal text "62" when the user typed `b` (0x62), and Enter /
    // Backspace did nothing. The psmux encoding must use `-l` literals + key-names.

    #[test]
    fn psmux_printable_char_uses_literal_not_hex() {
        // Typing `b` must inject `b`, not the literal text "62".
        assert_eq!(
            psmux_send_keys_commands("%1", b"b"),
            vec!["send-keys -t %1 -l -N 1 \"b\"\n".to_string()]
        );
    }

    #[test]
    fn psmux_printable_run_is_one_literal_command() {
        assert_eq!(
            psmux_send_keys_commands("%1", b"hello world"),
            vec!["send-keys -t %1 -l -N 1 \"hello world\"\n".to_string()]
        );
    }

    #[test]
    fn psmux_enter_backspace_tab_escape_use_key_names() {
        assert_eq!(
            psmux_send_keys_commands("%1", b"\r"),
            vec!["send-keys -t %1 Enter\n".to_string()]
        );
        assert_eq!(
            psmux_send_keys_commands("%1", &[0x7f]),
            vec!["send-keys -t %1 BSpace\n".to_string()]
        );
        assert_eq!(
            psmux_send_keys_commands("%1", b"\t"),
            vec!["send-keys -t %1 Tab\n".to_string()]
        );
        assert_eq!(
            psmux_send_keys_commands("%1", &[0x1b]),
            vec!["send-keys -t %1 Escape\n".to_string()]
        );
    }

    #[test]
    fn psmux_ctrl_letters_map_to_c_prefix() {
        assert_eq!(
            psmux_send_keys_commands("%1", &[0x03]), // Ctrl+C
            vec!["send-keys -t %1 C-c\n".to_string()]
        );
        assert_eq!(
            psmux_send_keys_commands("%1", &[0x01]), // Ctrl+A
            vec!["send-keys -t %1 C-a\n".to_string()]
        );
        assert_eq!(
            psmux_send_keys_commands("%1", &[0x1a]), // Ctrl+Z
            vec!["send-keys -t %1 C-z\n".to_string()]
        );
        assert_eq!(
            psmux_send_keys_commands("%1", &[0x0a]), // LF → Ctrl+J
            vec!["send-keys -t %1 C-j\n".to_string()]
        );
    }

    #[test]
    fn psmux_navigation_sequences_use_one_named_key() {
        for (bytes, name) in [
            (&b"\x1b[A"[..], "Up"),
            (&b"\x1b[B"[..], "Down"),
            (&b"\x1b[C"[..], "Right"),
            (&b"\x1b[D"[..], "Left"),
            (&b"\x1bOA"[..], "Up"),
            (&b"\x1bOB"[..], "Down"),
            (&b"\x1bOC"[..], "Right"),
            (&b"\x1bOD"[..], "Left"),
            (&b"\x1b[H"[..], "Home"),
            (&b"\x1b[F"[..], "End"),
            (&b"\x1b[5~"[..], "PageUp"),
            (&b"\x1b[6~"[..], "PageDown"),
            (&b"\x1b[1;5A"[..], "C-Up"),
            (&b"\x1b[1;2D"[..], "S-Left"),
            (&b"\x1b[5;5~"[..], "C-PageUp"),
            (&b"\x1b[6;2~"[..], "S-PageDown"),
        ] {
            assert_eq!(
                psmux_send_keys_commands("%1", bytes),
                vec![format!("send-keys -t %1 {name}\n")],
                "{}",
                String::from_utf8_lossy(bytes)
            );
        }
    }

    #[test]
    fn psmux_navigation_keeps_adjacent_text_and_unrecognized_sequences() {
        assert_eq!(
            psmux_send_keys_commands("%1", b"a\x1b[DZ"),
            vec![
                "send-keys -t %1 -l -N 1 \"a\"\n",
                "send-keys -t %1 Left\n",
                "send-keys -t %1 -l -N 1 \"Z\"\n",
            ]
        );
        assert_eq!(
            psmux_send_keys_commands("%1", b"\x1b[200~"),
            vec!["send-keys -t %1 -l -N 1 0x1b \"[200~\"\n"]
        );
    }

    #[test]
    fn psmux_unnamed_escape_sequences_are_typed_whole() {
        for (bytes, args) in [
            // A mouse report: split, the pane got `[<35;10;5M` and no ESC.
            (&b"\x1b[<35;10;5M"[..], "0x1b \"[<35;10;5M\""),
            (&b"\x1b[<0;10;5m"[..], "0x1b \"[<0;10;5m\""),
            (&b"\x1b[15;5~"[..], "0x1b \"[15;5~\""),
            (&b"\x1bOP"[..], "0x1b \"OP\""),
            (&b"\x1bb"[..], "0x1b \"b\""),
            ("\x1bé".as_bytes(), "0x1b \"é\""),
            // A leading `-` is still kept from reading as a flag.
            (&b"\x1b-"[..], "0x1b 0x2d"),
            (&b"\x1b[M #!"[..], "0x1b \"[M\" 0x20 0x23 0x21"),
        ] {
            assert_eq!(
                psmux_send_keys_commands("%1", bytes),
                vec![format!("send-keys -t %1 -l -N 1 {args}\n")],
                "{}",
                String::from_utf8_lossy(bytes)
            );
        }
    }

    #[test]
    fn psmux_shift_enter_is_ctrl_j() {
        // Typed whole, `CSI 13;2u` did nothing in vibe's TUI or PowerShell.
        assert_eq!(
            psmux_send_keys_commands("%1", b"a\x1b[13;2ub"),
            vec![
                "send-keys -t %1 -l -N 1 \"a\"\n",
                "send-keys -t %1 C-j\n",
                "send-keys -t %1 -l -N 1 \"b\"\n",
            ]
        );
    }

    #[test]
    fn psmux_a_lone_or_unfinished_escape_stays_the_escape_key() {
        assert_eq!(
            psmux_send_keys_commands("%1", b"\x1b\x1b"),
            vec!["send-keys -t %1 Escape\n", "send-keys -t %1 Escape\n"]
        );
        // Alt + a control byte: Escape, then that byte's key-name.
        assert_eq!(
            psmux_send_keys_commands("%1", b"\x1b\r"),
            vec!["send-keys -t %1 Escape\n", "send-keys -t %1 Enter\n"]
        );
        assert_eq!(
            psmux_send_keys_commands("%1", b"\x1b[13;2"),
            vec![
                "send-keys -t %1 Escape\n",
                "send-keys -t %1 -l -N 1 \"[13;2\"\n"
            ]
        );
    }

    #[test]
    fn psmux_a_sequence_between_text_keeps_its_neighbours() {
        assert_eq!(
            psmux_send_keys_commands("%1", b"x\x1b[<35;10;5My"),
            vec![
                "send-keys -t %1 -l -N 1 \"x\"\n",
                "send-keys -t %1 -l -N 1 0x1b \"[<35;10;5M\"\n",
                "send-keys -t %1 -l -N 1 \"y\"\n",
            ]
        );
    }

    #[test]
    fn psmux_literal_single_quote_survives() {
        // Regression: psmux's send-coalescing re-quoted literals with the
        // POSIX `'\''` escape its own parser can't read back, so `it's` was
        // typed into the pane as `it\s`. The `-N 1` opts out of coalescing and
        // the double-quote framing passes `'` through untouched.
        assert_eq!(
            psmux_send_keys_commands("%1", b"it's"),
            vec!["send-keys -t %1 -l -N 1 \"it's\"\n".to_string()]
        );
    }

    #[test]
    fn psmux_literal_escapes_backslash_and_double_quote() {
        // psmux's double-quote tokenizer reads exactly `\"` and `\\`; both
        // must be escaped so Windows paths and quoted text round-trip.
        assert_eq!(
            psmux_send_keys_commands("%1", br#"say "hi" C:\p"#),
            vec!["send-keys -t %1 -l -N 1 \"say \\\"hi\\\" C:\\\\p\"\n".to_string()]
        );
    }

    #[test]
    fn psmux_literal_escapes_a_leading_hyphen() {
        // Regression (#920): psmux classifies arguments after tokenizing, so
        // the quotes are gone by the time it drops everything starting with
        // `-` as a flag — a typed `-` was silently swallowed. It comes back as
        // psmux's own `0xNN` codepoint form, which decodes to the same char.
        assert_eq!(
            psmux_send_keys_commands("%1", b"-"),
            vec!["send-keys -t %1 -l -N 1 0x2d\n".to_string()]
        );
        // Only the leading hyphens need escaping; the rest stays one literal.
        assert_eq!(
            psmux_send_keys_commands("%1", b"--flag=a-b"),
            vec!["send-keys -t %1 -l -N 1 0x2d 0x2d \"flag=a-b\"\n".to_string()]
        );
    }

    #[test]
    fn psmux_literal_escapes_a_hex_codepoint_lookalike() {
        // psmux rewrites a `0xNN` argument into the character it names, so a
        // run literally spelling `0x41` would have arrived as `A`.
        assert_eq!(
            psmux_send_keys_commands("%1", b"0x41"),
            vec!["send-keys -t %1 -l -N 1 0x30 \"x41\"\n".to_string()]
        );
        // A hyphen ahead of one still leaves a lookalike behind it.
        assert_eq!(
            psmux_send_keys_commands("%1", b"-0x41"),
            vec!["send-keys -t %1 -l -N 1 0x2d 0x30 \"x41\"\n".to_string()]
        );
        // Not a lookalike: text past the hex digits is ordinary literal text.
        assert_eq!(
            psmux_send_keys_commands("%1", b"0x41z"),
            vec!["send-keys -t %1 -l -N 1 \"0x41z\"\n".to_string()]
        );
    }

    #[test]
    fn psmux_literal_args_escapes_an_all_hyphen_run() {
        assert_eq!(psmux_literal_args("---"), "0x2d 0x2d 0x2d");
        assert_eq!(psmux_literal_args(""), "");
    }

    #[test]
    fn psmux_mixed_text_then_enter() {
        // The common "type a command and submit" path.
        assert_eq!(
            psmux_send_keys_commands("%1", b"ls\r"),
            vec![
                "send-keys -t %1 -l -N 1 \"ls\"\n".to_string(),
                "send-keys -t %1 Enter\n".to_string(),
            ]
        );
    }

    #[test]
    fn psmux_bracketed_paste_keeps_markers_whole() {
        // The markers are typed whole like any other sequence, but a paste
        // still never takes this encoding: each CR in it would be Enter. The
        // writer sends it through `PsmuxPaste` instead.
        assert_eq!(
            psmux_send_keys_commands("%1", b"\x1b[200~hi\x1b[201~"),
            vec![
                "send-keys -t %1 -l -N 1 0x1b \"[200~\"\n".to_string(),
                "send-keys -t %1 -l -N 1 \"hi\"\n".to_string(),
                "send-keys -t %1 -l -N 1 0x1b \"[201~\"\n".to_string(),
            ]
        );
    }

    // --- psmux out-of-band paste ---

    #[test]
    fn paste_chunks_keeps_an_ordinary_paste_whole() {
        assert_eq!(paste_chunks("one\ntwo"), vec!["one\ntwo"]);
        let exact = "x".repeat(PASTE_CHUNK_BYTES);
        assert_eq!(paste_chunks(&exact), vec![exact.as_str()]);
    }

    /// A huge paste is split so no single command line exceeds Windows'
    /// `cmd.exe` cap — losslessly, and never mid-character (psmux drops a
    /// payload that isn't valid UTF-8).
    #[test]
    fn paste_chunks_splits_large_input_on_char_boundaries() {
        // Multi-byte chars straddling the cut: 2 bytes each, odd-sized prefix.
        let text = format!("{}{}", "a", "é".repeat(PASTE_CHUNK_BYTES));
        let chunks = paste_chunks(&text);
        assert!(chunks.len() > 1);
        assert!(chunks.iter().all(|c| c.len() <= PASTE_CHUNK_BYTES));
        assert_eq!(chunks.concat(), text);
    }

    #[test]
    fn send_paste_args_targets_the_pane_with_a_base64_payload() {
        assert_eq!(
            send_paste_args("%7", "hi\nthere"),
            vec!["send-paste", "-t", "%7", "aGkKdGhlcmU="]
        );
    }

    /// The payload must never put a raw CR/LF (or a quote) on psmux's
    /// line-oriented command wire — base64 is what keeps it off (psmux #560).
    #[test]
    fn send_paste_args_payload_is_wire_safe() {
        let args = send_paste_args("%1", "first\r\nsecond \"quoted\" \\ '");
        let payload = args.last().unwrap();
        assert!(!payload
            .bytes()
            .any(|b| matches!(b, b'\r' | b'\n' | b'"' | b'\\' | b'\'' | b' ')));
        assert_eq!(
            base64::engine::general_purpose::STANDARD
                .decode(payload)
                .unwrap(),
            b"first\r\nsecond \"quoted\" \\ '"
        );
    }

    #[test]
    fn psmux_utf8_char_goes_to_literal() {
        assert_eq!(
            psmux_send_keys_commands("%1", "é".as_bytes()),
            vec!["send-keys -t %1 -l -N 1 \"é\"\n".to_string()]
        );
    }

    #[test]
    fn psmux_long_run_splits_on_char_boundary() {
        let input = "é".repeat(400); // 800 bytes, each char 2 bytes
        let cmds = psmux_send_keys_commands("%1", input.as_bytes());
        assert!(cmds.len() > 1, "expected a long run to span >1 command");
        // Reassemble the quoted literals back into the original text.
        let mut text = String::new();
        for cmd in &cmds {
            let inner = cmd
                .trim_end()
                .strip_prefix("send-keys -t %1 -l -N 1 \"")
                .and_then(|s| s.strip_suffix('"'))
                .expect("literal command shape");
            text.push_str(inner);
        }
        assert_eq!(text, input);
    }

    /// The channel is closed until psmux is proven (see [`Psmux::HOOK_STATUS`]),
    /// and closed is an answer every caller can see: no hook command, nothing
    /// recorded, a listing refused rather than read as every pane quiet — and
    /// none of it runs a process to find that out.
    #[test]
    fn a_closed_status_channel_offers_and_reads_nothing() {
        let host = crate::session::HostDef {
            name: "winbox".into(),
            destination: "me@winbox.invalid".into(),
            multiplexer: Some("psmux".into()),
            ..Default::default()
        };
        for backend in [PsmuxBackend::local(), PsmuxBackend::for_host(&host)] {
            assert_eq!(backend.hook_signal_command(), None);
            assert!(backend.record_hook_state("%1", "working").is_err());
            assert!(backend.hook_states().is_err());
        }
    }

    /// The form the channel would use once open: the socket baked in, made
    /// safe to splice into a hook file, and never empty.
    #[test]
    fn the_hook_command_names_a_safe_socket() {
        let host = |socket: &str| crate::session::HostDef {
            name: "winbox".into(),
            destination: "me@winbox.invalid".into(),
            multiplexer: Some("psmux".into()),
            socket: Some(socket.into()),
            ..Default::default()
        };
        let command =
            |socket: &str| Psmux::hook_signal_command(&PsmuxBackend::for_host(&host(socket)));
        assert_eq!(
            command("we\"ird sock\\et"),
            "psmux -L weirdsocket set-option -p @thurbox_state "
        );
        assert_eq!(
            command("\"\\ "),
            format!(
                "psmux -L {} set-option -p @thurbox_state ",
                crate::backend::instance::TMUX_SOCKET
            )
        );
    }
}
