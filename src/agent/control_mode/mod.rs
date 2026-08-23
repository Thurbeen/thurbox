//! Shared tmux control mode I/O infrastructure.
//!
//! Both halves of control mode live here: the transport-agnostic protocol
//! (notification parsing, octal decoding, the per-pane reader/writer and the
//! psmux encodings) and the live `ControlMode` connection itself (the `-C`
//! child process, its reader thread, the FIFO response queue and the psmux
//! hook poller). `TmuxBackend` drives one `ControlMode` per backend across its
//! local and SSH/WSL (`TmuxTransport`) transports — the wire protocol is
//! identical over either.

use std::collections::{HashMap, VecDeque};
use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Child, ChildStdin, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{sync_channel, SyncSender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use anyhow::{bail, Context, Result};
use base64::Engine as _;
use tracing::{debug, warn};

use super::transport::TmuxTransport;

/// Per-pane output channel capacity. Sized large enough to buffer heavy output
/// bursts; chunks are dropped (not blocked) when full to keep the reader thread alive.
pub const PANE_CHANNEL_CAPACITY: usize = 4096;

/// Maps pane IDs to sync senders for multi-instance output broadcast.
pub type PaneSendersMap = HashMap<String, Vec<SyncSender<Vec<u8>>>>;
pub type PaneSendersMapShared = Arc<Mutex<PaneSendersMap>>;

/// Response from a tmux control mode command.
pub struct CommandResponse {
    pub lines: Vec<String>,
    pub is_error: bool,
}

/// Parsed notification from the tmux control mode protocol.
#[derive(Debug, PartialEq)]
pub enum Notification {
    Output {
        pane_id: String,
        data: Vec<u8>,
    },
    Begin,
    End,
    Error,
    Pause {
        pane_id: String,
    },
    /// A `refresh-client -B` format subscription reported a changed value
    /// (tmux >= 3.2). Carries the remote hook state for
    /// [`crate::session::REMOTE_HOOK_SUBSCRIPTION`].
    SubscriptionChanged {
        name: String,
        pane_id: String,
        value: String,
    },
    Other(String),
}

/// Per-pane reader that receives output via an mpsc channel.
///
/// Implements `Read` so it plugs directly into the existing `Session::reader_loop`.
pub struct ControlModeReader {
    receiver: std::sync::mpsc::Receiver<Vec<u8>>,
    buffer: Vec<u8>,
    pos: usize,
}

impl ControlModeReader {
    pub fn new(receiver: std::sync::mpsc::Receiver<Vec<u8>>) -> Self {
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

/// Max input bytes encoded into a single `send-keys -H` command. Each byte
/// becomes 3 chars (` XX`), so the command line stays ≈ `prefix + 3·512` ≈ 1.6
/// KB — well under tmux's per-command line limit (which would truncate a longer
/// line). `send_keys_commands` splits larger writes across multiple commands.
const SEND_KEYS_CHUNK_BYTES: usize = 512;

/// Split `buf` into the ordered `send-keys` command lines for `pane_id`.
///
/// Two encodings: real tmux uses the byte-exact `send-keys -H` hex flag (each
/// byte → two hex digits), chunked at `SEND_KEYS_CHUNK_BYTES` so no single
/// control-mode line gets over-long (tmux truncates those); the raw bytes —
/// including the bracketed-paste markers — span the chunks and the receiving
/// pane reassembles them. psmux (the native-Windows tmux clone) does **not**
/// implement `-H` — given `-H 62` it injects the literal text "62" instead of
/// byte 0x62, so the whole `-H` path is silently broken there — so
/// `psmux = true` selects the key-name/literal encoding it does support
/// (`psmux_send_keys_commands`).
pub fn send_keys_commands(pane_id: &str, buf: &[u8], psmux: bool) -> Vec<String> {
    if psmux {
        return psmux_send_keys_commands(pane_id, buf);
    }
    buf.chunks(SEND_KEYS_CHUNK_BYTES)
        .map(|chunk| format_send_keys(pane_id, chunk))
        .collect()
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
/// `new-window -c/-n` in [`super::tmux`]) — same tokenizer, so
/// [`shell_escape`]'s POSIX `'\''` idiom would arrive mangled there too.
pub(crate) fn psmux_quote(s: &str) -> String {
    format!(
        "\"{}\"",
        s.replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('\n', " ")
    )
}

/// The bracketed-paste markers a paste payload is wrapped in.
const PASTE_START: &[u8] = b"\x1b[200~";
const PASTE_END: &[u8] = b"\x1b[201~";

/// The pasted text inside a bracketed-paste payload (`ESC[200~ … ESC[201~`), or
/// `None` when `buf` is not exactly one such payload — ordinary keystrokes, a
/// payload split across writes, a marker in the middle (two pastes, or pasted
/// marker text), or non-UTF-8 bytes. Those keep the key encoding.
///
/// The markers are stripped: [`PsmuxPaste`] hands psmux the bare text and psmux
/// re-adds them itself, only when the receiving app has bracketed paste on.
fn bracketed_paste_text(buf: &[u8]) -> Option<&str> {
    let inner = buf.strip_prefix(PASTE_START)?.strip_suffix(PASTE_END)?;
    let has_marker = |m: &[u8]| inner.windows(m.len()).any(|w| w == m);
    if has_marker(PASTE_START) || has_marker(PASTE_END) {
        return None;
    }
    std::str::from_utf8(inner).ok()
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
#[derive(Debug, Clone)]
pub struct PsmuxPaste {
    transport: TmuxTransport,
    socket: String,
}

impl PsmuxPaste {
    pub fn new(transport: TmuxTransport, socket: String) -> Self {
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

/// Per-pane writer that sends input via control-mode `send-keys` through the
/// shared control stdin. `psmux` selects the key-name/literal encoding when the
/// backend is psmux instead of tmux's `-H` hex (see [`send_keys_commands`]), and
/// `paste` carries that backend's out-of-band paste channel ([`PsmuxPaste`]).
pub struct ControlModeWriter {
    pub stdin: Arc<Mutex<std::process::ChildStdin>>,
    pub pane_id: String,
    pub psmux: bool,
    pub paste: Option<PsmuxPaste>,
}

impl Write for ControlModeWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        // A paste cannot be encoded as psmux key-names (see `PsmuxPaste`), so it
        // goes out of band. A failure falls through to the key encoding: a
        // degraded paste beats a dropped one.
        if let Some(paste) = self.paste.as_ref() {
            if let Some(text) = bracketed_paste_text(buf) {
                match paste.send(&self.pane_id, text) {
                    Ok(()) => return Ok(buf.len()),
                    Err(e) => warn!("psmux send-paste failed, falling back to send-keys: {e:#}"),
                }
            }
        }
        let mut stdin = self
            .stdin
            .lock()
            .map_err(|e| std::io::Error::other(format!("stdin lock: {e}")))?;
        for cmd in send_keys_commands(&self.pane_id, buf, self.psmux) {
            stdin.write_all(cmd.as_bytes())?;
        }
        stdin.flush()?;
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Decode tmux control mode octal escapes in `%output` data.
///
/// Scans for `\` followed by exactly 3 octal digits (0-7). Emits the decoded byte.
/// All other characters pass through unchanged.
pub fn decode_octal(input: &str) -> Vec<u8> {
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
pub fn parse_notification(line: &str) -> Notification {
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

    if let Some(n) = parse_subscription_changed(line) {
        return n;
    }

    Notification::Other(line.to_string())
}

/// Parse a `%subscription-changed` notification (tmux >= 3.2 format
/// subscriptions, armed via `refresh-client -B`).
///
/// Wire format (tmux man page): `%subscription-changed name session-id
/// window-id window-index pane-id ... : value` — "any arguments after pane-id
/// up until a single ':' are for future use and should be ignored". Parsed
/// positionally (name = token 0, pane id = token 4, validated) with the value
/// being everything after the first ` : ` separator past the pane token,
/// verbatim — it may legally be empty or contain spaces/colons. Any shape
/// violation returns `None` (→ `Notification::Other`); wire data never
/// panics.
fn parse_subscription_changed(line: &str) -> Option<Notification> {
    let rest = line.strip_prefix("%subscription-changed ")?;
    let mut tokens = rest.splitn(6, ' ');
    let name = tokens.next()?.to_string();
    // session-id, window-id, window-index — positional, unused.
    for _ in 0..3 {
        tokens.next()?;
    }
    let pane_id = tokens.next()?.to_string();
    if !is_valid_pane_id(&pane_id) {
        return None;
    }
    // Whatever follows the pane id: `[future-use tokens ]: value`.
    let tail = tokens.next().unwrap_or("");
    let value = if let Some(v) = tail.strip_prefix(": ") {
        v.to_string()
    } else if tail == ":" {
        String::new()
    } else if let Some(idx) = tail.find(" : ") {
        tail[idx + 3..].to_string()
    } else if tail.ends_with(" :") {
        // Future-use tokens then an empty value ("a b :").
        String::new()
    } else {
        return None;
    };
    Some(Notification::SubscriptionChanged {
        name,
        pane_id,
        value,
    })
}

/// A tmux pane id is `%<digits>`. Pane ids are interpolated unquoted into
/// control-mode commands (`send-keys -t`, `kill-pane -t`, …), so anything
/// else must be rejected where ids enter the system (spawn/adopt/discover).
pub fn is_valid_pane_id(s: &str) -> bool {
    s.strip_prefix('%')
        .is_some_and(|rest| !rest.is_empty() && rest.bytes().all(|b| b.is_ascii_digit()))
}

/// Parse `list-panes -F "#{pane_id} #{@thurbox_state}"` output into the
/// `(pane_id, value)` pairs whose option is **set**: one `%<id> [value]` line
/// per pane; empty values (option unset) and malformed lines are skipped —
/// wire data never panics. Shared by the psmux poller's diff below and the
/// headless status poll (`session_ops::remote_hooks::poll_remote_hook_states`).
pub fn parse_pane_hook_states(body: &str) -> Vec<(String, String)> {
    body.lines()
        .filter_map(|line| {
            let line = line.trim();
            let (pane_id, value) = match line.split_once(' ') {
                Some((id, v)) => (id, v.trim()),
                None => (line, ""),
            };
            (is_valid_pane_id(pane_id) && !value.is_empty())
                .then(|| (pane_id.to_string(), value.to_string()))
        })
        .collect()
}

/// Parse `list-panes -a -F "#{pane_id} #{pane_pid}"` output into a
/// `pane_id → pid` map — the same line shape [`parse_pane_hook_states`] reads,
/// with a pid where the option value was. Malformed lines and non-numeric pids
/// are skipped: wire data never panics. Backs the batched
/// `SessionBackend::pane_pids` the metrics sampler uses.
pub fn parse_pane_pids(body: &str) -> std::collections::HashMap<String, u32> {
    body.lines()
        .filter_map(|line| {
            let (pane_id, pid) = line.trim().split_once(' ')?;
            if !is_valid_pane_id(pane_id) {
                return None;
            }
            Some((pane_id.to_string(), pid.trim().parse().ok()?))
        })
        .collect()
}

/// Diff one psmux hook-poll result against the previous poll, returning the
/// `(pane_id, value)` pairs to report — the poller-side equivalent of tmux's
/// `%subscription-changed` edge semantics.
///
/// `body` is parsed by [`parse_pane_hook_states`]. Reported: a pane's
/// **non-empty** value seen for the first time (parity with the
/// subscription's arm-time catch-up report) or changed since the last poll.
/// Not reported: an unchanged value (steady state stays silent), an empty
/// value (option unset — also *clears* the pane's entry, like a vanished
/// pane, so a respawned pane's state re-reports).
pub fn diff_polled_hook_states(
    last: &mut std::collections::HashMap<String, String>,
    body: &str,
) -> Vec<(String, String)> {
    let mut current = std::collections::HashMap::new();
    let mut changed = Vec::new();
    for (pane_id, value) in parse_pane_hook_states(body) {
        if last.get(&pane_id).map(String::as_str) != Some(value.as_str()) {
            changed.push((pane_id.clone(), value.clone()));
        }
        current.insert(pane_id, value);
    }
    *last = current;
    changed
}

/// Format a `send-keys -H` command for a pane.
///
/// Each byte is encoded as two hex digits.
pub fn format_send_keys(pane_id: &str, bytes: &[u8]) -> String {
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

/// Shell-escape a string for safe inclusion in a tmux control mode command.
///
/// Tmux control mode is line-delimited — each `\n` starts a new command.
/// Literal newlines in arguments (e.g. `--append-system-prompt`) would split
/// the command and corrupt the protocol, so they are replaced with spaces.
pub fn shell_escape(s: &str) -> String {
    // Strip the protocol-breaking newlines, then apply standard POSIX
    // single-quote escaping (shared with the SSH/git paths).
    crate::shell::posix_quote(&s.replace('\n', " "))
}

/// Timeout for waiting for a control mode command response.
const COMMAND_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// How many times [`ControlMode::drop`] re-checks for a graceful child exit
/// before force-killing, and how long it waits between checks. The product is
/// the per-connection ceiling on a graceful detach (~50 ms); a control-mode
/// client that has not exited by then is not going to, and killing it is
/// harmless (see the rationale in `impl Drop for ControlMode`).
const GRACEFUL_EXIT_POLLS: u32 = 10;
const GRACEFUL_EXIT_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(5);

/// A live tmux control mode connection.
///
/// Commands are sent serially (stdin lock ensures ordering) and responses arrive
/// in the same order. We use a FIFO queue instead of matching command numbers,
/// which avoids numbering mismatches between our counter and tmux's internal
/// counter (e.g., from `send_command_nowait` calls that still consume a tmux
/// command number).
pub(super) struct ControlMode {
    pub(super) stdin: Arc<Mutex<ChildStdin>>,
    pub(super) pane_senders: PaneSendersMapShared,
    /// FIFO queue of response channels — one per `send_command()` call, in order.
    response_queue: Arc<Mutex<VecDeque<SyncSender<CommandResponse>>>>,
    /// `(pane_id, state)` pairs from `%subscription-changed` notifications
    /// (remote hook status — see [`crate::session::REMOTE_HOOK_STATE_OPTION`]),
    /// pushed by the reader thread and drained by the app tick via
    /// [`Self::take_sub_events`]. Bounded (drop-oldest): a short-lived
    /// connection (e.g. a headless spawn's) has no drainer.
    sub_events: Arc<Mutex<VecDeque<(String, String)>>>,
    /// True while this connection lives; cleared on reader EOF and in `Drop`.
    /// The psmux hook poller checks it each cycle so a replaced connection's
    /// poller winds down instead of writing into a dead pipe forever.
    alive: Arc<AtomicBool>,
    reader_handle: Mutex<Option<JoinHandle<()>>>,
    child: Mutex<Child>,
}

/// Cap on queued subscription events. Status transitions are rare and the
/// queue is drained every TUI tick — the cap only guards an undrained
/// connection against unbounded growth.
const SUB_EVENTS_CAP: usize = 256;

/// How often the psmux hook poller lists pane options — matches tmux's own
/// ≤1/s subscription-report cadence, so both channels have the same worst-case
/// status latency.
const PSMUX_HOOK_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(1000);

impl ControlMode {
    /// Start a control mode connection to the thurbox tmux session over the
    /// given transport (local or ssh).
    pub(super) fn start(transport: &TmuxTransport, socket: &str, session: &str) -> Result<Self> {
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
        let alive = Arc::new(AtomicBool::new(true));

        let reader_stdin = Arc::clone(&stdin);
        let reader_pane_senders = Arc::clone(&pane_senders);
        let reader_queue = Arc::clone(&response_queue);
        let reader_sub_events = Arc::clone(&sub_events);
        let reader_alive = Arc::clone(&alive);

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
                reader_alive.store(false, Ordering::Relaxed);
            })
            .context("Failed to spawn control reader thread")?;

        let control = Self {
            stdin,
            pane_senders,
            response_queue,
            sub_events,
            alive,
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
        } else if transport.is_remote() && crate::session::psmux_hook_rewrite_supported() {
            // Unlike the subscription (passive — zero recurring cost), the
            // poller is a 1 Hz command, so it only runs where a producer can
            // exist: a *remote* psmux host with the hook rewrite enabled. A
            // local psmux (Windows) session signals via `thurbox-cli` straight
            // into the DB and never sets the pane option.
            control.spawn_psmux_hook_poller(session);
        }

        Ok(control)
    }

    /// psmux has no format subscriptions, so a remote psmux connection
    /// **polls** the remote-hook pane option instead (once
    /// [`crate::session::psmux_hook_rewrite_supported`] is flipped — the same
    /// gate that enables shipping the rewritten hooks that set it): a
    /// background thread lists every pane of the session with its
    /// `@thurbox_state` each [`PSMUX_HOOK_POLL_INTERVAL`], diffs against the
    /// previous poll ([`diff_polled_hook_states`]), and feeds
    /// changes into the same `sub_events` queue the tmux subscription uses —
    /// everything downstream (`take_hook_state_events` → the app's drain) is
    /// shared. Best-effort: a command failure ends the thread (the connection
    /// is dying; a reconnect's fresh `ControlMode` spawns a fresh poller), and
    /// an idle server pays one cheap command per second on an
    /// already-persistent connection.
    ///
    /// The thread is deliberately **detached** (not joined in `Drop`): a poll
    /// blocked in its command timeout when the connection dies would stall the
    /// drop for [`COMMAND_TIMEOUT`]; instead it exits on its own via the
    /// `alive` flag or the dead pipe shortly after.
    fn spawn_psmux_hook_poller(&self, session: &str) {
        let stdin = Arc::clone(&self.stdin);
        let queue = Arc::clone(&self.response_queue);
        let events = Arc::clone(&self.sub_events);
        let alive = Arc::clone(&self.alive);
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
        let cmd = format!(
            "list-panes -s -t \"{session_safe}\" -F \"#{{pane_id}} #{{{}}}\"",
            crate::session::REMOTE_HOOK_STATE_OPTION,
        );
        let spawned = std::thread::Builder::new()
            .name("psmux-hook-poller".into())
            .spawn(move || {
                let mut last = std::collections::HashMap::new();
                loop {
                    std::thread::sleep(PSMUX_HOOK_POLL_INTERVAL);
                    if !alive.load(Ordering::Relaxed) {
                        break;
                    }
                    let body = match Self::send_command_on(&stdin, &queue, &cmd) {
                        Ok(body) => body,
                        Err(e) => {
                            debug!("psmux hook poller stopping: {e:#}");
                            break;
                        }
                    };
                    let changed = diff_polled_hook_states(&mut last, &body);
                    Self::queue_sub_events(&events, changed);
                }
            });
        if let Err(e) = spawned {
            warn!("failed to spawn the psmux hook poller: {e}");
        }
    }

    /// Append polled hook changes to the subscription queue, oldest dropped
    /// first once it is full — the same bound the passive tmux subscription
    /// honours.
    fn queue_sub_events(
        events: &Arc<Mutex<VecDeque<(String, String)>>>,
        changed: Vec<(String, String)>,
    ) {
        if changed.is_empty() {
            return;
        }
        let Ok(mut events) = events.lock() else {
            return;
        };
        for ev in changed {
            if events.len() >= SUB_EVENTS_CAP {
                events.pop_front();
            }
            events.push_back(ev);
        }
    }

    /// One newline-terminated control-mode line, or `None` at EOF / on an I/O
    /// error (both of which end the reader).
    ///
    /// Lossy conversion: tmux control mode is mostly ASCII, but raw bytes can
    /// appear (e.g. in `%extended-output`). Replacing invalid sequences with
    /// U+FFFD is safe — the octal-encoded payload in `%output` lines is always
    /// valid ASCII.
    fn next_control_line(
        reader: &mut BufReader<std::process::ChildStdout>,
        line_buf: &mut Vec<u8>,
    ) -> Option<String> {
        line_buf.clear();
        match reader.read_until(b'\n', line_buf) {
            Ok(0) => return None,
            Ok(_) => {}
            Err(e) => {
                debug!("Control reader I/O error: {e}");
                return None;
            }
        }
        if line_buf.last() == Some(&b'\n') {
            line_buf.pop();
        }
        Some(String::from_utf8_lossy(line_buf).into_owned())
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

        while let Some(line) = Self::next_control_line(&mut reader, &mut line_buf) {
            match parse_notification(&line) {
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
                        Self::queue_sub_events(&sub_events, vec![(pane_id, value)]);
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
    pub(super) fn take_sub_events(&self) -> Vec<(String, String)> {
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
    pub(super) fn send_command(&self, cmd: &str) -> Result<String> {
        Self::send_command_on(&self.stdin, &self.response_queue, cmd)
    }

    /// [`Self::send_command`] without `&self`, so background threads holding
    /// only the shared handles (the psmux hook poller) can issue commands.
    ///
    /// Both locks are held across enqueue **and** write: concurrent senders
    /// (a backend caller vs the poller) must not interleave one thread's
    /// waiter-push with another's stdin-write, or the FIFO waiter order stops
    /// matching the on-wire command order and every later response is
    /// delivered one command off. A failed write pops the just-enqueued
    /// waiter for the same reason.
    fn send_command_on(
        stdin: &Arc<Mutex<ChildStdin>>,
        response_queue: &Arc<Mutex<VecDeque<SyncSender<CommandResponse>>>>,
        cmd: &str,
    ) -> Result<String> {
        let (tx, rx) = sync_channel(1);

        {
            // Lock order: stdin first, queue only for the brief push/pop.
            // Holding stdin across enqueue AND write keeps the FIFO waiter
            // order matching the on-wire command order for concurrent senders
            // (a backend caller vs the psmux poller) — while never holding the
            // queue lock across the pipe write, so a write blocked on a wedged
            // transport can't stall the reader thread (whose response dispatch
            // needs the queue lock) or any other `send_command` caller beyond
            // the command itself.
            let mut stdin = stdin
                .lock()
                .map_err(|e| anyhow::anyhow!("stdin lock: {e}"))?;
            {
                let mut queue = response_queue
                    .lock()
                    .map_err(|e| anyhow::anyhow!("response_queue lock: {e}"))?;
                queue.push_back(tx);
            }
            if let Err(e) = writeln!(stdin, "{cmd}").and_then(|()| stdin.flush()) {
                // Un-enqueue our waiter — still under the stdin lock, so no
                // other sender can have pushed after us: the back is ours.
                if let Ok(mut queue) = response_queue.lock() {
                    queue.pop_back();
                }
                return Err(e.into());
            }
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
    pub(super) fn send_command_nowait(&self, cmd: &str) -> Result<()> {
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
        // Wind down the (detached) psmux hook poller; it observes the flag on
        // its next cycle, or exits via the dead pipe once the child is killed.
        self.alive.store(false, Ordering::Relaxed);

        // Try to gracefully detach.
        if let Ok(mut stdin) = self.stdin.lock() {
            let _ = writeln!(stdin, "detach-client");
            let _ = stdin.flush();
        }

        // Give the child a moment to exit gracefully, then force-kill so the
        // reader thread gets EOF promptly and we never block indefinitely.
        //
        // The check goes *after* the sleep: `try_wait` runs immediately after
        // `detach-client` is flushed, long before tmux has processed it, so a
        // leading check never succeeds and only costs a full interval. Every
        // backend pays this at quit, so the interval is kept short.
        //
        // Force-killing is safe: the tmux *server* and the agent panes are
        // independent processes, so this only tears down the control-mode
        // client. The graceful `detach-client` above is a courtesy, which is
        // why the budget can be this aggressive.
        if let Ok(mut child) = self.child.lock() {
            let exited = (0..GRACEFUL_EXIT_POLLS).any(|_| {
                std::thread::sleep(GRACEFUL_EXIT_POLL_INTERVAL);
                matches!(child.try_wait(), Ok(Some(_)))
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
pub(super) fn is_broken_pipe(err: &anyhow::Error) -> bool {
    err.chain().any(|cause| {
        cause
            .downcast_ref::<std::io::Error>()
            .is_some_and(|e| e.kind() == std::io::ErrorKind::BrokenPipe)
    })
}

/// Check if an error is caused by a recv timeout (reader thread died, response never arrives).
pub(super) fn is_recv_timeout(err: &anyhow::Error) -> bool {
    err.chain().any(|cause| {
        cause
            .downcast_ref::<std::sync::mpsc::RecvTimeoutError>()
            .is_some()
    })
}

#[cfg(test)]
mod tests;
