//! Native Windows psmux backend: the transport that launches `psmux`, the
//! psmux dialect of the shared mux core (`MuxDialect`), and the
//! [`SessionBackend`] over both.
//!
//! psmux clones tmux's command language and diverges from it where it matters
//! (ADR-13): no per-window options, no `send-keys -H`, no paste command in
//! control mode, no subscriptions or size reports, its own tokenizer, and a
//! PowerShell command model. Each of those answers lives here.

use std::collections::HashMap;
use std::path::Path;
use std::process::Command;
use std::sync::Arc;

use anyhow::{bail, Context, Result};
use base64::Engine as _;
use tracing::warn;

use super::backend::{
    AdoptedSession, BackendLiveness, DiscoveredSession, PanePath, PaneState, SessionBackend,
    SpawnedSession, WindowRole,
};
use super::control_mode::{PaneSnapshot, PasteChannel, SEND_KEYS_CHUNK_BYTES};
use super::mux::{
    tmux_size, ConfigOption, MuxBackend, MuxDialect, HEARTBEAT_INTERVAL_SECS, SESSION_OPTS,
    WINDOW_OPTS,
};
use super::transport::{LaunchPath, MuxTransport};

/// A native psmux path, local on Windows or reached through SSH.
#[derive(Debug, Clone)]
pub struct PsmuxTransport {
    path: LaunchPath,
}

impl MuxTransport for PsmuxTransport {
    const BINARY: &'static str = "psmux";
    fn local() -> Self {
        Self {
            path: LaunchPath::Local,
        }
    }
    fn from_host(host: &crate::session::HostDef) -> Self {
        Self {
            path: LaunchPath::Ssh {
                destination: host.destination.clone(),
                ssh_opts: host.ssh_opts.clone(),
            },
        }
    }
    fn path(&self) -> LaunchPath {
        self.path.clone()
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
/// - [`psmux_window_command`] wraps it in double quotes for a control-mode
///   `new-window` line, whose parser keeps only the *first* trailing token
///   (tmux joins them) — the agent launched with no args. psmux's tokenizer
///   concatenates adjacent `'…'` segments but passes `'` through `"…"` tokens
///   untouched (backslash is literal everywhere, so `C:\` paths are safe) —
///   hence single quotes inside, double quotes outside.
/// - the one-shot `spawn_window` passes it verbatim as a single argv token
///   (the argv path joins trailing tokens fine, but still ignores `-e`).
fn psmux_window_powershell(
    command: &str,
    args: &[String],
    env: &HashMap<String, String>,
) -> String {
    let quote = crate::shell::powershell_quote;
    let mut ps = String::new();
    // Sort for a deterministic command (HashMap iteration order isn't).
    let mut pairs: Vec<_> = env.iter().collect();
    pairs.sort();
    for (k, v) in pairs {
        ps.push_str(&format!("Set-Item Env:{k} {}; ", quote(v)));
    }
    ps.push_str(&format!("& {}", quote(command)));
    for a in args {
        ps.push(' ');
        ps.push_str(&quote(a));
    }
    ps.replace(['"', '\n'], " ")
}

/// [`psmux_window_powershell`] framed as one **double-quoted** control-mode
/// token for a `new-window` line.
fn psmux_window_command(command: &str, args: &[String], env: &HashMap<String, String>) -> String {
    format!("\"{}\"", psmux_window_powershell(command, args, env))
}

/// Build the psmux-compatible `send-keys` command line(s) for `buf`.
///
/// psmux supports `send-keys -l` (literal text) and key-names (`Enter`, `Tab`,
/// `Escape`, `BSpace`, `C-<letter>`, …) but not tmux's `-H` hex flag. Encode the
/// exact byte stream with those primitives: contiguous printable/UTF-8 runs go
/// out as one `-l` literal command, each control byte as its key-name. Because
/// every key-name injects exactly the byte it stands for, multi-byte sequences
/// round-trip — an arrow key (`\x1b[A`) becomes `Escape` then literal `[A`,
/// which the pane's PTY receives back as `\x1b[A`.
fn psmux_send_keys_commands(pane_id: &str, buf: &[u8]) -> Vec<String> {
    let mut cmds = Vec::new();
    let mut literal: Vec<u8> = Vec::new();
    for &b in buf {
        match psmux_key_name(b) {
            Some(name) => {
                flush_psmux_literal(pane_id, &mut literal, &mut cmds);
                cmds.push(format!("send-keys -t {pane_id} {name}\n"));
            }
            None => literal.push(b),
        }
    }
    flush_psmux_literal(pane_id, &mut literal, &mut cmds);
    cmds
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
/// leading `-` needs more than quoting — see [`psmux_literal_args`]). Double
/// quotes — not POSIX single quotes — because psmux's tokenizer has no working
/// escape for a `'` inside `'…'`, but inside `"…"` it passes `'` through and
/// reads exactly two escapes: `\"` (literal quote) and `\\` (literal
/// backslash); any other backslash stays literal, so both are escaped here. A
/// literal run never contains a newline (LF and CR map to key-names), but the
/// control-mode line is `\n`-delimited, so newlines are replaced defensively.
/// Also the argument encoding for any other psmux control-mode line (e.g.
/// `new-window -c/-n`) — same tokenizer, so
/// [`control_mode::shell_escape`]'s POSIX `'\''` idiom would arrive mangled
/// there too.
fn psmux_quote(s: &str) -> String {
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
fn psmux_send_paste_args(pane_id: &str, text: &str) -> Vec<String> {
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
type PasteCommand = dyn Fn(&str, &[&str]) -> std::process::Command + Send + Sync;

#[derive(Clone)]
struct PsmuxPaste {
    command: Arc<PasteCommand>,
    socket: String,
}

impl std::fmt::Debug for PsmuxPaste {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PsmuxPaste")
            .field("socket", &self.socket)
            .finish()
    }
}

impl PsmuxPaste {
    fn new(transport: PsmuxTransport, socket: String) -> Self {
        let command =
            Arc::new(move |socket: &str, args: &[&str]| transport.mux_command(socket, args));
        Self { command, socket }
    }

    /// Deliver `text` to `pane_id` as a paste. Blocks until psmux has applied it
    /// (the psmux CLI round-trips a barrier before exiting), so a keystroke
    /// written to control mode afterwards cannot overtake it. Callers reach this
    /// through the session's writer task, never the UI thread, so the wait only
    /// holds back that session's own later input — the ordering we want.
    ///
    /// A paste past [`PASTE_CHUNK_BYTES`] goes out as several commands, each its
    /// own paste — the text still arrives whole and no CR submits. An error on a
    /// *later* chunk is reported but not returned: the caller's fallback would
    /// re-send text the pane already has.
    fn send(&self, pane_id: &str, text: &str) -> Result<()> {
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
        let args = psmux_send_paste_args(pane_id, text);
        let argv: Vec<&str> = args.iter().map(String::as_str).collect();
        let out = (self.command)(&self.socket, &argv)
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

impl MuxDialect for PsmuxTransport {
    // `set-option -w -t <pane> @k v` stores one option for the whole server
    // and `#{@k}` expands to it on every window (measured against psmux 3.3.6).
    const WINDOW_OPTIONS: bool = false;
    const COMMAND_LISTS: bool = false;
    const SIZE_REPORTS: bool = false;
    // Draining a block psmux never sends would hang before the reader starts.
    const IMPLICIT_ATTACH_RESPONSE: bool = false;
    const STRICT_RESPONSE_BLOCKS: bool = false;
    const SUBSCRIPTIONS: bool = false;
    const POLLS_LIVENESS: bool = true;
    // psmux has no such sanitizing and need not know the flag.
    const UTF8_FLAG: bool = false;
    // `new-window -P -F` is unverified against the documented divergences.
    const ONESHOT_PANE_REPORT: bool = false;
    const ASKS_SERVER_VERSION: bool = true;
    // The same interpreter psmux wraps every window command in.
    const REMOTE_SHELL: &'static str = "powershell";

    fn check_banner(banner: &str, socket: &str) -> Result<()> {
        check_psmux_version(banner, socket)
    }

    fn check_server_version(version: &str, socket: &str) -> Result<()> {
        check_psmux_version(version, socket)
    }

    /// No `default-command`: `$SHELL` and `/bin/sh` don't exist on Windows,
    /// and forcing a Windows shell would have to match psmux's own
    /// command-execution model, so psmux keeps its native ConPTY default shell.
    /// Decided by the multiplexer, not by the OS thurbox runs on: a Linux
    /// thurbox driving a psmux host used to pin `/bin/sh` there. No clipboard
    /// options either — psmux has no OSC 52 forwarding (a local Windows session
    /// copies via the native clipboard path instead) — and no `mouse`.
    ///
    /// Server-wide options take `-g`: psmux 3.3.8 refuses `-s` ("unknown flag
    /// -s") and keeps one option table anyway, and 3.3.7 and 3.3.8 both take
    /// `-g`.
    fn session_config(session: &str, _default_command: Option<&str>) -> Vec<ConfigOption> {
        let mut config = vec![
            ConfigOption::set(&["-g", "default-terminal", "xterm-256color"], true),
            ConfigOption::set(&["-g", "extended-keys", "on"], true),
            ConfigOption::set(&["-g", "extended-keys-format", "csi-u"], false),
        ];
        for (key, val) in SESSION_OPTS {
            config.push(ConfigOption::set(&["-t", session, key, val], true));
        }
        for (key, val) in WINDOW_OPTS {
            config.push(ConfigOption::set(&["-w", "-g", key, val], false));
        }
        config
    }

    /// psmux has neither birth option.
    fn birth_option_commands(_window_name: &str) -> Vec<String> {
        Vec::new()
    }

    /// psmux's tokenizer can't read POSIX `'\''` escapes (see [`psmux_quote`]),
    /// so a `-c`/`-n` value gets the double-quote framing it does parse.
    fn quote_arg(s: &str) -> String {
        psmux_quote(s)
    }

    /// Nothing: psmux ignores `-e`, and the environment rides in the window
    /// command instead ([`psmux_window_powershell`]).
    fn env_args(_env: &HashMap<String, String>) -> String {
        String::new()
    }

    /// psmux can't take the command as joined trailing tokens nor env via
    /// `-e`, so everything is folded into one PowerShell token.
    fn window_command(
        _backend: &MuxBackend<Self>,
        _window_name: &str,
        command: &str,
        args: &[String],
        env: &HashMap<String, String>,
    ) -> String {
        psmux_window_command(command, args, env)
    }

    fn send_keys_commands(pane_id: &str, buf: &[u8]) -> Vec<String> {
        psmux_send_keys_commands(pane_id, buf)
    }

    fn paste_channel(&self, socket: String) -> Option<PasteChannel> {
        let paste = PsmuxPaste::new(self.clone(), socket);
        Some(PasteChannel::new(move |pane, text| paste.send(pane, text)))
    }

    /// Only where a producer can exist: a *remote* psmux host with the hook
    /// rewrite enabled ([`crate::session::psmux_hook_rewrite_supported`]). A
    /// local psmux (Windows) session signals via `thurbox-cli` straight into
    /// the DB and never sets the pane option.
    fn hook_poll_command(&self, session: &str) -> Option<String> {
        if !self.is_remote() || !crate::session::psmux_hook_rewrite_supported() {
            return None;
        }
        // Double-quoted framing: psmux's tokenizer passes `'` through `"…"`
        // tokens but mangles adjacent `'…'` segments (see
        // `psmux_window_command`). The session name is user-authored
        // hosts.toml text embedded in a wire command, so it gets the same
        // double-quote framing, minus the `"`/`\` it can't carry — mirroring
        // the socket sanitization in `builtin_hooks::remote_signal_target`.
        let session_safe: String = session
            .chars()
            .filter(|c| !matches!(c, '"' | '\\'))
            .collect();
        Some(format!(
            "list-panes -s -t \"{session_safe}\" -F \"#{{pane_id}} #{{{}}}\"",
            crate::session::REMOTE_HOOK_STATE_OPTION,
        ))
    }

    /// No `if-shell -F` to decide with: the last instance to paint wins.
    fn resize_commands(
        backend_id: &str,
        rows: u16,
        cols: u16,
        _sizer: &str,
    ) -> (Vec<String>, usize) {
        let (rows, cols) = tmux_size(rows, cols);
        (
            vec![
                format!("resize-window -t {backend_id} -x {cols} -y {rows}"),
                format!("resize-pane -t {backend_id} -x {cols} -y {rows}"),
            ],
            2,
        )
    }

    /// psmux's own `send-paste`, which wraps and writes the payload itself
    /// (see [`PsmuxPaste`] for why key-encoded markers do not survive there): a
    /// raw newline inside a psmux command argument is cut by the server's
    /// line-oriented read, so a multi-line prompt arrived truncated *and* its
    /// tail ran as a psmux command (psmux #560).
    fn paste_args(target: &str, text: &str) -> Vec<String> {
        psmux_send_paste_args(target, text)
    }

    /// psmux's `run-shell` is not a POSIX shell, so the sequence is driven
    /// through PowerShell explicitly (`Start-Sleep` for the sub-second beat).
    /// PowerShell single-quoted literals escape an embedded `'` by doubling it.
    ///
    /// The prompt travels as psmux's own base64 `send-paste` payload (see
    /// [`MuxDialect::paste_args`]) — which also keeps the script free of the
    /// prompt's newlines and quotes.
    fn deferred_prompt_script(socket: &str, target: &str, text: &str) -> String {
        let bin = Self::BINARY;
        let t = crate::shell::powershell_quote(target);
        let payload = base64::engine::general_purpose::STANDARD.encode(text.as_bytes());
        format!(
            "powershell -NoProfile -Command \"{bin} -L {socket} send-paste -t {t} {payload}; \
             Start-Sleep -Milliseconds 200; \
             {bin} -L {socket} send-keys -t {t} Enter\""
        )
    }

    /// psmux runs a window command via `powershell -NoLogo -Command`, so the
    /// keeper loop is PowerShell — handed over as **one argv token**, dodging
    /// psmux's trailing-token handling entirely (same delivery and quoting as
    /// [`psmux_window_powershell`]). This used to be a no-op ("no POSIX shell
    /// for the keeper loop"), which silently degraded headless automation
    /// firing to TUI-only on Windows.
    fn heartbeat_loop_command(cli_path: &Path) -> String {
        let cli = crate::shell::powershell_quote(&cli_path.display().to_string());
        format!(
            "while ($true) {{ & {cli} automation tick *> $null; Start-Sleep {HEARTBEAT_INTERVAL_SECS} }}"
        )
    }

    /// psmux ignores `-e`, so the env is folded into the window command itself,
    /// delivered as a single argv token (see [`psmux_window_powershell`]).
    fn push_window_program(
        cmd: &mut Command,
        command: &str,
        args: &[String],
        env: &HashMap<String, String>,
    ) {
        cmd.arg(psmux_window_powershell(command, args, env));
    }
}

pub struct PsmuxBackend {
    protocol: MuxBackend<PsmuxTransport>,
}

impl PsmuxBackend {
    pub fn local() -> Self {
        let mut protocol = MuxBackend::<PsmuxTransport>::local();
        protocol.set_name("local-psmux");
        Self { protocol }
    }

    pub fn from_host(host: &crate::session::HostDef) -> Self {
        let mut protocol = MuxBackend::<PsmuxTransport>::from_host(host);
        protocol.set_name(format!("{}:psmux", host.backend_name()));
        Self { protocol }
    }
}

impl SessionBackend for PsmuxBackend {
    fn send_text(
        &self,
        session_id: &str,
        session_name: &str,
        text: &str,
        submit: bool,
    ) -> Result<()> {
        self.protocol
            .session_send_text(session_id, session_name, text, submit)
    }
    fn send_key(&self, session_id: &str, session_name: &str, key: &str) -> Result<()> {
        self.protocol
            .session_send_key(session_id, session_name, key)
    }
    fn send_text_after(
        &self,
        session_id: &str,
        session_name: &str,
        text: &str,
        delay_secs: u64,
    ) -> Result<()> {
        self.protocol
            .session_send_text_after(session_id, session_name, text, delay_secs)
    }
    fn capture_text(
        &self,
        session_id: &str,
        session_name: &str,
        lines: u32,
        ansi: bool,
    ) -> Result<String> {
        self.protocol
            .session_capture_text(session_id, session_name, lines, ansi)
    }
    fn pane_state(&self, session_id: &str, session_name: &str) -> PaneState {
        self.protocol.session_pane_state(session_id, session_name)
    }
    fn pane_path(&self, session_id: &str, session_name: &str) -> PanePath {
        self.protocol.session_pane_path(session_id, session_name)
    }
    fn has_window(&self, session_id: &str, session_name: &str) -> bool {
        self.protocol.session_has_window(session_id, session_name)
    }
    fn rename_windows(&self, session_id: &str, from: &str, to: &str) -> Result<()> {
        self.protocol.session_rename_windows(session_id, from, to)
    }
    fn claim_running_window(&self, session_id: &str, session_name: &str) -> Result<Option<String>> {
        self.protocol
            .session_claim_running_window(session_id, session_name)
    }
    fn name(&self) -> &str {
        self.protocol.name()
    }
    fn needs_liveness_poll(&self) -> bool {
        true
    }
    fn check_available(&self) -> Result<()> {
        self.protocol.check_available()
    }
    fn ensure_ready(&self) -> Result<()> {
        self.protocol.ensure_ready()
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
        self.protocol
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
        self.protocol
            .spawn_headless(session_id, window_name, command, args, cwd, env)
    }
    fn headless_liveness(&self, session_id: &str, session_name: &str) -> Result<BackendLiveness> {
        self.protocol.headless_liveness(session_id, session_name)
    }
    fn headless_live_pane(&self, session_id: &str, session_name: &str) -> Result<Option<String>> {
        self.protocol.headless_live_pane(session_id, session_name)
    }
    fn headless_owned_panes_in(
        &self,
        windows: &[DiscoveredSession],
        session_id: &str,
        session_name: &str,
    ) -> Vec<String> {
        self.protocol
            .headless_owned_panes_in(windows, session_id, session_name)
    }
    fn kill_headless(
        &self,
        session_id: &str,
        session_name: &str,
        agent_pane: &str,
        shell_pane: &str,
    ) -> Result<bool> {
        self.protocol
            .kill_headless(session_id, session_name, agent_pane, shell_pane)
    }
    fn headless_pane_pid(
        &self,
        backend_id: &str,
        session_id: &str,
        name: &str,
    ) -> Result<Option<u32>> {
        self.protocol
            .headless_pane_pid(backend_id, session_id, name)
    }
    fn headless_discover(&self) -> Result<Vec<DiscoveredSession>> {
        self.protocol.headless_discover()
    }

    fn adopt(
        &self,
        backend_id: &str,
        rows: u16,
        cols: u16,
        seed: Option<Vec<u8>>,
    ) -> Result<AdoptedSession> {
        self.protocol.adopt(backend_id, rows, cols, seed)
    }
    fn capture_history(&self, backend_id: &str) -> Result<Vec<u8>> {
        self.protocol.capture_history(backend_id)
    }
    fn title_seed(&self, backend_id: &str) -> Vec<u8> {
        self.protocol.title_seed(backend_id)
    }
    fn supports_snapshots(&self) -> bool {
        false
    }
    fn request_snapshot(&self, _backend_id: &str) -> Result<()> {
        anyhow::bail!("psmux cannot snapshot a pane in step with its output")
    }
    fn snapshot(&self, _backend_id: &str) -> Result<PaneSnapshot> {
        anyhow::bail!("psmux cannot snapshot a pane")
    }

    fn discover(&self) -> Result<Vec<DiscoveredSession>> {
        self.protocol.discover()
    }
    fn stamp_window(&self, _backend_id: &str, _session_id: &str, _role: WindowRole) -> Result<()> {
        // psmux stores these options globally, so a stamp would misidentify every window.
        Ok(())
    }
    fn window_panes(&self, window_name: &str) -> Result<Vec<(String, bool)>> {
        self.protocol.window_panes(window_name)
    }
    fn set_pane_retention(&self, _backend_id: &str, _keep: bool) -> Result<()> {
        Ok(())
    }
    fn resize(&self, backend_id: &str, rows: u16, cols: u16) -> Result<()> {
        self.protocol.resize(backend_id, rows, cols)
    }
    fn claim_size(&self, backend_id: &str, rows: u16, cols: u16) -> Result<()> {
        self.resize(backend_id, rows, cols)
    }
    fn is_dead(&self, backend_id: &str) -> Result<bool> {
        self.protocol.is_dead(backend_id)
    }
    fn kill(&self, backend_id: &str) -> Result<()> {
        self.protocol.kill(backend_id)
    }
    fn detach(&self, backend_id: &str) -> Result<()> {
        self.protocol.detach(backend_id)
    }
    fn default_shell(&self) -> String {
        self.protocol.default_shell()
    }
    fn pane_pid(&self, backend_id: &str) -> Result<Option<u32>> {
        self.protocol.pane_pid(backend_id)
    }
    fn pane_pids(&self) -> Result<HashMap<String, u32>> {
        self.protocol.pane_pids()
    }
    fn pane_ids(&self) -> Result<std::collections::HashSet<String>> {
        self.protocol.pane_ids()
    }
    fn shutdown(&self) {
        self.protocol.shutdown()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// psmux 3.3.8 answers `set-option -s` with "unknown flag -s", which failed
    /// every session setup against it; 3.3.7 took either scope.
    #[test]
    fn psmux_server_options_are_set_in_the_global_scope() {
        let config = PsmuxTransport::session_config("thurbox", Some("/bin/sh"));
        assert!(config.iter().any(|option| option.args()[1] == "-g"));
        assert!(config.iter().all(|option| option.args()[1] != "-s"));
        // No POSIX shell to pin, whatever the core offers.
        assert!(config
            .iter()
            .all(|option| !option.args().contains(&"default-command".to_string())));
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
        let args = PsmuxTransport::paste_args("thurbox:tb-demo", "line one\nline two");
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
    fn a_psmux_remote_window_is_never_login_wrapped() {
        // A Windows SSH host (multiplexer = "psmux") has no `/bin/sh`; wrapping
        // would replace the agent command with one that can't start at all.
        let host = crate::session::HostDef {
            name: "winbox".into(),
            destination: "me@winbox".into(),
            multiplexer: Some("psmux".into()),
            ..Default::default()
        };
        let backend = MuxBackend::<PsmuxTransport>::from_host(&host);
        assert_eq!(
            PsmuxTransport::window_command(&backend, "tb-x", "claude", &[], &HashMap::new()),
            "\"& 'claude'\""
        );
    }

    // --- psmux send-keys encoding tests ---
    //
    // Regression: psmux has no `send-keys -H`, so on Windows the hex path
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
    fn psmux_arrow_sequence_splits_escape_then_literal() {
        // An arrow key arrives as `\x1b[A`; psmux reconstructs the same bytes
        // from `Escape` + literal `[A`.
        assert_eq!(
            psmux_send_keys_commands("%1", b"\x1b[A"),
            vec![
                "send-keys -t %1 Escape\n".to_string(),
                "send-keys -t %1 -l -N 1 \"[A\"\n".to_string(),
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
        // This encoding is only the *fallback* for a psmux pane (the split ESC
        // reaches the pane as a bare Escape keypress, so the marker is lost);
        // the live path is `PsmuxPaste`.
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

    // --- psmux out-of-band paste (`PsmuxPaste`) ---

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
            psmux_send_paste_args("%7", "hi\nthere"),
            vec!["send-paste", "-t", "%7", "aGkKdGhlcmU="]
        );
    }

    /// The payload must never put a raw CR/LF (or a quote) on psmux's
    /// line-oriented command wire — base64 is what keeps it off (psmux #560).
    #[test]
    fn send_paste_args_payload_is_wire_safe() {
        let args = psmux_send_paste_args("%1", "first\r\nsecond \"quoted\" \\ '");
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
}
