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

use crate::backend::contract::SessionBackend;
use crate::backend::tmux_compat::control_mode::{ControlPolicy, PaneInput, SEND_KEYS_CHUNK_BYTES};
use crate::backend::tmux_compat::server::{ConfigOption, Server, TmuxCompatible};
use crate::backend::tmux_compat::transport::TmuxTransport;
use crate::session::{HostDef, Multiplexer, Platform};
use crate::shell::{powershell_quote, HostLauncher};

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

    /// psmux has neither `remain-on-exit` nor `window-size`.
    const WINDOW_SETTINGS: bool = false;

    /// psmux sends no `%window-close` and no `%layout-change`.
    const WINDOW_EVENTS: bool = false;
    const PANE_MONITORING: bool = true;

    /// Nothing has verified that a psmux reply queues behind the pane output
    /// ahead of it, and its blocks are framed the old way
    /// ([`ControlPolicy::tagged_blocks`]).
    const SNAPSHOTS: bool = false;

    /// Not verified to take a `;` list as one invocation: each option is set
    /// on its own.
    const COMMAND_LISTS: bool = false;
    const COMMAND_LIST_SINGLE_REPLY: bool = false;

    /// psmux's `new-window -P -F` support is unverified against the documented
    /// divergences (ADR-13), and `{end}` is tmux's shorthand: the one-shot path
    /// targets the session and finds the window by its name.
    const ONE_SHOT_SPAWN_ANSWERS: bool = false;

    /// No `if-shell -F` to decide with: the last instance to paint wins.
    const CONDITIONAL_RESIZE: bool = false;

    /// psmux 3.3.8 refuses `-s` ("unknown flag -s") and keeps one option table
    /// anyway, so it gets `-g`, which 3.3.7 and 3.3.8 both take.
    const SERVER_SCOPE: &str = "-g";

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
    /// the window command (`psmux_window_powershell`).
    fn env_flags(_env: &HashMap<String, String>) -> String {
        String::new()
    }

    fn window_command(
        _server: &Server<Self>,
        _window_name: &str,
        command: &str,
        args: &[String],
        env: &HashMap<String, String>,
    ) -> String {
        psmux_window_command(command, args, env)
    }

    /// One argv token: the environment and the program as the PowerShell
    /// `psmux_window_powershell` builds, since `-e` would be ignored.
    fn push_window_program(
        cmd: &mut Command,
        command: &str,
        args: &[String],
        env: &HashMap<String, String>,
    ) {
        cmd.arg(psmux_window_powershell(command, args, env));
    }

    /// psmux's own `send-paste`, which wraps and writes the payload itself (see
    /// `PsmuxPaste` for why key-encoded markers do not survive there): a raw
    /// newline inside a psmux command argument is cut by the server's
    /// line-oriented read, so a multi-line prompt arrived truncated *and* its
    /// tail ran as a psmux command (psmux #560).
    fn paste_args(target: &str, text: &str) -> Vec<String> {
        send_paste_args(target, text)
    }

    fn deferred_paste_script(mux: &str, socket: &str, target: &str, text: &str) -> String {
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
            subscriptions: false,
            status_poll: (transport.is_remote() && Self::HOOK_STATUS)
                .then(|| hook_poll_command(session)),
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
    // but mangles adjacent `'…'` segments (see [`psmux_window_command`]). The
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
/// - [`psmux_window_command`] wraps it in
///   double quotes for a control-mode `new-window` line, whose parser keeps
///   only the *first* trailing token (tmux joins them) — the agent launched
///   with no args. psmux's tokenizer concatenates adjacent `'…'` segments
///   but passes `'` through `"…"` tokens untouched (backslash is literal
///   everywhere, so `C:\` paths are safe) — hence single quotes inside,
///   double quotes outside.
/// - the headless spawn passes it verbatim as a single argv token
///   ([`TmuxCompatible::push_window_program`]; the argv path joins trailing
///   tokens fine, but still ignores `-e`).
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
        ps.push_str(&format!("Set-Item Env:{k} {}; ", powershell_quote(v)));
    }
    ps.push_str(&format!("& {}", powershell_quote(command)));
    for a in args {
        ps.push(' ');
        ps.push_str(&powershell_quote(a));
    }
    ps.replace(['"', '\n'], " ")
}

/// [`psmux_window_powershell`] framed as one
/// **double-quoted** control-mode token for a `new-window` line.
fn psmux_window_command(command: &str, args: &[String], env: &HashMap<String, String>) -> String {
    format!("\"{}\"", psmux_window_powershell(command, args, env))
}

/// The `run-shell` script that pastes the prompt, waits a beat, then presses
/// Enter: PowerShell, since psmux's `run-shell` is not a POSIX shell, with the
/// prompt travelling as psmux's own base64 `send-paste` payload (see
/// [`send_paste_args`]), which also keeps the script free of the prompt's
/// newlines and quotes.
fn deferred_paste_script(mux: &str, socket: &str, target: &str, text: &str) -> String {
    // Only the socket: a quoted program name is a string to PowerShell,
    // not a command, and a multiplexer's binary name needs no quoting.
    let socket = powershell_quote(socket);
    let t = powershell_quote(target);
    let payload = base64::engine::general_purpose::STANDARD.encode(text.as_bytes());
    format!(
        "powershell -NoProfile -Command \"{mux} -L {socket} send-paste -t {t} {payload}; \
         Start-Sleep -Milliseconds 200; \
         {mux} -L {socket} send-keys -t {t} Enter\""
    )
}

/// Build the psmux-compatible `send-keys` command line(s) for `buf`.
///
/// psmux supports `send-keys -l` (literal text) and key-names (`Enter`, `Tab`,
/// `Escape`, `BSpace`, `C-<letter>`, …). Encode the
/// input with those primitives: contiguous printable/UTF-8 runs go
/// out as one `-l` literal command, each control byte as its key-name. Navigation
/// sequences use one named key command: splitting ESC from the rest makes psmux
/// dispatch a standalone Escape before the arrow sequence reaches the pane.
pub fn psmux_send_keys_commands(pane_id: &str, buf: &[u8]) -> Vec<String> {
    let mut cmds = Vec::new();
    let mut literal: Vec<u8> = Vec::new();
    let mut i = 0;
    while i < buf.len() {
        if let Some((len, name)) = psmux_navigation_key(&buf[i..]) {
            flush_psmux_literal(pane_id, &mut literal, &mut cmds);
            cmds.push(format!("send-keys -t {pane_id} {name}\n"));
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

/// Max text bytes per `send-paste` command. The base64 payload travels as a
/// process argument, and Windows caps a whole command line at ~32,767 chars —
/// which base64 reaches at ~24 KB of text. 8 KB leaves generous headroom for
/// the rest of the argv while keeping an ordinary paste a single command.
const PASTE_CHUNK_BYTES: usize = 8 * 1024;

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
/// only), and its `send-keys` encoding cannot carry a paste: an ESC byte has to
/// go out as its own `Escape` key-name, which reaches the pane as a standalone
/// PTY write, so the agent sees a bare Escape keypress instead of the
/// `ESC[200~` opening marker and then reads every embedded CR that follows as
/// Enter — a pasted stack trace was submitted one line at a time (issue #916).
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
            .tmux_command(&self.socket, &argv)
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
        let args = Psmux::paste_args("thurbox:tb-demo", "line one\nline two");
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

    #[test]
    fn psmux_window_command_is_one_double_quoted_token() {
        let args = vec!["--session-id".to_string(), "abc-123".to_string()];
        let cmd = psmux_window_command("claude", &args, &HashMap::new());
        assert_eq!(cmd, "\"& 'claude' '--session-id' 'abc-123'\"");
    }

    #[test]
    fn psmux_window_command_folds_env_as_set_item() {
        // `Set-Item Env:K 'v'` (not `$env:K`) keeps the string `$`-free; sorted
        // for determinism. Values with spaces survive the PS single quotes.
        let mut env = HashMap::new();
        env.insert("THURBOX_SESSION".to_string(), "id-1".to_string());
        env.insert("B".to_string(), "x y".to_string());
        let cmd = psmux_window_command("claude", &[], &env);
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
        let cmd = psmux_window_command("C:\\Tools\\claude.exe", &args, &HashMap::new());
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
        let backend = PsmuxBackend::for_host(&host);
        assert_eq!(backend.login_wrap_for_remote("claude"), "claude");
        assert_eq!(
            Psmux::window_command(&backend, "tb-x", "claude", &[], &HashMap::new()),
            "\"& 'claude'\"",
            "the window's command is the PowerShell token, never a POSIX wrap"
        );
    }

    #[test]
    fn a_deferred_prompt_names_the_servers_own_mux_and_socket() {
        let psmux = deferred_paste_script("psmux", "sock", "%3", "it's\nhere");
        assert!(psmux.starts_with("powershell -NoProfile -Command \"psmux -L 'sock' send-paste"));
        // Base64: the prompt's newline and quote never reach the script.
        assert!(!psmux.contains("it's"), "{psmux}");
    }

    #[test]
    fn a_deferred_prompt_quotes_a_socket_name_the_host_configured() {
        let psmux = deferred_paste_script("psmux", "my sock", "%3", "hi");
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
            vec![
                "send-keys -t %1 Escape\n",
                "send-keys -t %1 -l -N 1 \"[200~\"\n"
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
    fn psmux_bracketed_paste_splits_markers_from_text() {
        // A paste arrives wrapped in `\x1b[200~ … \x1b[201~`; the ESC bytes
        // become `Escape`, the rest stays literal — reconstructing the wrapper.
        // Which is why a paste never takes this encoding: the split ESC reaches
        // the pane as a bare Escape keypress, so the marker is lost and every CR
        // after it is Enter. The writer sends it through `PsmuxPaste` instead.
        assert_eq!(
            psmux_send_keys_commands("%1", b"\x1b[200~hi\x1b[201~"),
            vec![
                "send-keys -t %1 Escape\n".to_string(),
                "send-keys -t %1 -l -N 1 \"[200~hi\"\n".to_string(),
                "send-keys -t %1 Escape\n".to_string(),
                "send-keys -t %1 -l -N 1 \"[201~\"\n".to_string(),
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

    /// A huge paste is split so no single command line exceeds Windows' ~32 KB
    /// cap — losslessly, and never mid-character (psmux drops a payload that
    /// isn't valid UTF-8).
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
