//! Shared tmux control mode I/O infrastructure.
//!
//! Both halves of control mode live here: the transport-agnostic protocol
//! (notification parsing, octal decoding, the per-pane reader/writer) and the
//! live `ControlMode` connection itself (the `-C` child process, its reader
//! thread, the FIFO response queue and the hook poller). A tmux-protocol
//! backend drives one `ControlMode` across its local and SSH/WSL
//! (`TmuxTransport`) transports — the wire protocol is identical over either.
//! What a server can be expected to do on that wire is its adapter's to say
//! ([`ControlPolicy`], [`PaneInput`]).

use std::collections::{HashMap, VecDeque};
use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Child, ChildStdin, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{sync_channel, Receiver, SyncSender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use anyhow::{bail, Context, Result};
use tracing::{debug, warn};

use super::transport::TmuxTransport;
use crate::backend::contract::{PaneSize, PaneSnapshot, SnapshotArrived};

/// Per-pane output channel capacity. Sized large enough to buffer heavy output
/// bursts; chunks are dropped (not blocked) when full to keep the reader thread alive.
pub const PANE_CHANNEL_CAPACITY: usize = 4096;

/// What a pane's reader is handed, in the order the control-mode stream
/// carried it.
#[derive(Debug, Clone)]
pub enum PaneChunk {
    /// Bytes the pane printed.
    Output(Vec<u8>),
    /// The pane as tmux holds it at exactly this point of its output: every
    /// byte handed over before this chunk is in it, and none handed over after.
    ///
    /// It can be exact because of where it travels. tmux queues a pane's
    /// `%output` in the same callback that parses those bytes into its own
    /// screen, and queues a command's reply behind every block already queued
    /// (`window.c`, `control.c`), so the reply to a capture sits in the stream
    /// at the byte its capture describes. The reader thread sees both in that
    /// order, which is why it — and not the thread that asked — puts the
    /// snapshot into the pane's channel.
    Snapshot(Box<PaneSnapshot>),
    /// The pane's window is now this size — see [`Notification::LayoutChange`].
    ///
    /// In this channel rather than beside it for the reason a snapshot is: the
    /// output a program writes after its window was resized is laid out for the
    /// new size, and whatever was queued before it for the old one.
    Resized { rows: u16, cols: u16 },
    /// Whether another client is named as the pane's sizer. A hint with no
    /// place in the stream, carried here only because this is where the pane's
    /// reader listens.
    SizedElsewhere(bool),
}

impl From<Vec<u8>> for PaneChunk {
    fn from(bytes: Vec<u8>) -> Self {
        Self::Output(bytes)
    }
}

/// The format [`snapshot_commands`] asks `display-message` for, and
/// [`parse_snapshot`] reads back.
const SNAPSHOT_FORMAT: &str =
    "#{pane_width} #{pane_height} #{cursor_x} #{cursor_y} #{alternate_on}";

/// The command list whose answer is a [`PaneSnapshot`] of `pane_id`, with at
/// most `history` rows of history.
///
/// One list, so tmux runs it without returning to its event loop and reading
/// the pane in between: the size, the cursor and both captures describe the
/// same instant. The current grid is captured without `-a`, which is the
/// alternate screen while one is up; `-a -q` then yields the normal screen
/// behind it, or nothing at all when there is no alternate screen.
///
/// `styled` keeps the SGR sequences (`-e`), which a rebuilt grid needs and a
/// reader of text does not: they are most of the bytes of a coloured history.
pub fn snapshot_commands(pane_id: &str, history: usize, styled: bool) -> Vec<String> {
    let start = format!("-{history}");
    let e = if styled { " -e" } else { "" };
    vec![
        format!("display-message -p -t {pane_id} '{SNAPSHOT_FORMAT}'"),
        format!("capture-pane -p{e} -J -S {start} -t {pane_id}"),
        format!("capture-pane -a -q -p{e} -J -S {start} -t {pane_id}"),
    ]
}

/// Read the answer to [`snapshot_commands`], one entry per `%begin`/`%end`
/// block. `None` for anything that is not that answer.
pub fn parse_snapshot(mut blocks: Vec<Vec<String>>) -> Option<PaneSnapshot> {
    if blocks.len() != 3 {
        return None;
    }
    let saved = blocks.pop()?;
    let current = blocks.pop()?;
    let fields: Vec<u16> = blocks
        .pop()?
        .first()?
        .split_whitespace()
        .map(|field| field.parse().ok())
        .collect::<Option<_>>()?;
    let [cols, rows, x, y, alternate] = fields[..] else {
        return None;
    };
    if cols == 0 || rows == 0 {
        return None;
    }
    let (normal, alternate) = if alternate == 1 {
        (saved, Some(current))
    } else {
        (current, None)
    };
    Some(PaneSnapshot {
        cols,
        rows,
        cursor: (x, y),
        normal,
        alternate,
    })
}

/// A snapshot asked for with [`ControlMode::ask_snapshot`], not yet answered.
pub(in crate::backend) struct PendingSnapshot {
    rx: Receiver<CommandResponse>,
    cmd: String,
    pane: String,
}

impl PendingSnapshot {
    /// The answer, or the error the command ran into.
    pub(in crate::backend) fn wait(self) -> Result<PaneSnapshot> {
        let response = ControlMode::await_blocks(self.rx, &self.cmd, COMMAND_TIMEOUT)?;
        parse_snapshot(response.blocks)
            .with_context(|| format!("unexpected answer to a snapshot of {}", self.pane))
    }
}

/// The one spelling of [`SIZER_OPTION`], so [`SIZED_BY`] can be built from it
/// at compile time rather than restate it.
macro_rules! sizer_option {
    () => {
        "@thurbox_sizer"
    };
}

/// The window option naming the client that sizes a window, when several
/// thurbox instances show it — see `Server::resize`.
pub const SIZER_OPTION: &str = sizer_option!();

/// The format subscription reporting [`SIZED_BY`] per pane, so an instance can
/// say its pane is being sized elsewhere.
const SIZER_SUBSCRIPTION: &str = "thurbox-sizer";

/// The pane **user option** a hook running in a tmux-protocol pane sets to
/// report its state (`set-option -p @thurbox_state <working|blocked|done|idle>`):
/// these adapters' status channel. The headless poll lists it, and an attached
/// connection receives changes through [`REMOTE_HOOK_SUBSCRIPTION`] (tmux) or
/// a poll (psmux). Protocol vocabulary, so it lives with the protocol: another
/// backend's status channel need not be a pane option at all.
pub const REMOTE_HOOK_STATE_OPTION: &str = "@thurbox_state";

/// Name of the control-mode format subscription
/// (`refresh-client -B <name>:%*:#{@thurbox_state}`) that pushes
/// [`REMOTE_HOOK_STATE_OPTION`] changes as `%subscription-changed`
/// notifications for every pane of the attached session.
pub const REMOTE_HOOK_SUBSCRIPTION: &str = "thurbox-status";

/// Who sizes a pane, as far as anybody else is concerned: the
/// [`SIZER_OPTION`] while more than one client is attached, and nobody once a
/// client is alone — an alone client may size any pane (`Server::resize`),
/// so a name left behind by an instance that has gone no longer counts. tmux
/// re-evaluates a subscription as clients come and go, which is what tells the
/// instance left behind that the size is its own again.
pub const SIZED_BY: &str = concat!("#{?#{==:#{session_attached},1},,#{", sizer_option!(), "}}");

/// Maps pane IDs to sync senders for multi-instance output broadcast.
pub type PaneSendersMap = HashMap<String, Vec<SyncSender<PaneChunk>>>;
pub type PaneSendersMapShared = Arc<Mutex<PaneSendersMap>>;

/// Which window each registered pane lives in, so a window's closing can be
/// turned into EOF for the readers of the panes that went with it.
///
/// tmux announces a death by WINDOW (`%window-close @3`) and streams output by
/// PANE (`%output %7 …`), and nothing in the protocol relates the two: the
/// `%window-add` that opened it carried no pane, and the panes are gone by the
/// time the close arrives, so it cannot be asked afterwards either. Recorded
/// when the pane is registered, which is the one moment both ids are in hand.
pub type PaneWindowsMap = HashMap<String, String>;
pub type PaneWindowsMapShared = Arc<Mutex<PaneWindowsMap>>;

/// Where each registered pane's reader applies the sizes tmux reports, for a
/// size its channel had no room for (see `ControlMode::dispatch_resize`).
pub type PaneSizesMap = HashMap<String, PaneSize>;
pub type PaneSizesMapShared = Arc<Mutex<PaneSizesMap>>;

/// Response from a tmux control mode command.
pub struct CommandResponse {
    /// Every block's lines, in order: one block per command of a list.
    pub blocks: Vec<Vec<String>>,
    pub is_error: bool,
}

impl CommandResponse {
    /// Every line of every block, as one list.
    fn lines(&self) -> Vec<String> {
        self.blocks.concat()
    }
}

/// A sent line waiting for its answer, and how many `%begin`/`%end` blocks that
/// answer spans. tmux answers a command list (`a ; b ; c`) with one block per
/// command it runs, and nothing on the wire says which blocks belong together —
/// each carries a command number of its own (measured, tmux 3.2 and 3.7c) — so
/// only the sender can know.
struct Waiter {
    tx: SyncSender<CommandResponse>,
    blocks: usize,
    /// For a [`snapshot_commands`] list: the pane whose output stream the
    /// answer is put into, as a [`PaneChunk::Snapshot`], at the point it
    /// arrived.
    splice: Option<String>,
}

/// The waiters, in the order their lines were written.
type ResponseQueue = Arc<Mutex<VecDeque<Waiter>>>;

/// A waiter whose command list has answered some of its blocks.
struct Answer {
    waiter: Waiter,
    blocks: Vec<Vec<String>>,
}

/// The `<time> <number>` a `%begin`, `%end` or `%error` line carries.
///
/// tmux ends a block with the same two numbers it began it with, and that is
/// the only way to tell a block's end from a line of its body: a command's
/// output is written raw, so a captured screen line can read `%end …` or
/// `%output …` as easily as anything else.
fn block_tag(line: &str) -> Option<(&str, &str)> {
    let rest = line
        .strip_prefix("%begin ")
        .or_else(|| line.strip_prefix("%end "))
        .or_else(|| line.strip_prefix("%error "))?;
    let mut fields = rest.split_whitespace();
    Some((fields.next()?, fields.next()?))
}

fn joined_tag((time, number): (&str, &str)) -> String {
    format!("{time} {number}")
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
    /// A window is gone, with every pane that was in it — the program in it
    /// exited, or something killed it.
    ///
    /// tmux spells this two ways and means the same thing by both:
    /// `%window-close` for a window of the session the client is attached to,
    /// `%unlinked-window-close` for one that has already been unlinked from it.
    /// Measured 2026-09-11 (tmux control mode, program exiting on its own): the
    /// notification that actually arrives for a `new-window` of the attached
    /// session is the UNLINKED one, because tmux unlinks before it announces.
    /// Treating only the first spelling as a death is therefore the same as
    /// treating none of them as one.
    WindowClose {
        window_id: String,
    },
    /// A `refresh-client -B` format subscription reported a changed value
    /// (tmux >= 3.2). Carries the remote hook state for
    /// [`REMOTE_HOOK_SUBSCRIPTION`].
    SubscriptionChanged {
        name: String,
        pane_id: String,
        value: String,
    },
    /// A window's size changed, whoever changed it — `%layout-change`, which
    /// tmux sends to every control client for every `resize-window`, even one
    /// that leaves the size where it was (measured, tmux 3.7c). The size is the
    /// window's; thurbox's windows hold one pane each, so it is the pane's too.
    LayoutChange {
        window_id: String,
        rows: u16,
        cols: u16,
    },
    Other(String),
}

/// Per-pane reader that receives output via an mpsc channel.
///
/// Implements `Read` so it plugs directly into the existing `Session::reader_loop`.
/// A [`PaneChunk::Snapshot`] comes out of `read` as a [`SnapshotArrived`] error.
pub struct ControlModeReader {
    receiver: std::sync::mpsc::Receiver<PaneChunk>,
    buffer: Vec<u8>,
    pos: usize,
    size: PaneSize,
}

impl ControlModeReader {
    pub fn new(receiver: std::sync::mpsc::Receiver<PaneChunk>) -> Self {
        Self {
            receiver,
            buffer: Vec::new(),
            pos: 0,
            size: PaneSize::default(),
        }
    }

    /// Where this reader leaves the sizes tmux reports for its pane, for the
    /// loop that feeds the pane's grid.
    pub fn size(&self) -> PaneSize {
        self.size.clone()
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

        // Block until the next chunk arrives. A size is handed over as an
        // interrupted read of its own, as a snapshot is: every byte before it
        // has been returned and none after it has, so the caller resizes its
        // grid at exactly the point in the stream tmux resized the pane.
        loop {
            match self.receiver.recv() {
                Ok(PaneChunk::Snapshot(snapshot)) => {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::Interrupted,
                        SnapshotArrived(snapshot),
                    ))
                }
                Ok(PaneChunk::Resized { rows, cols }) => {
                    self.size.report(rows, cols);
                    return Err(std::io::ErrorKind::Interrupted.into());
                }
                Ok(PaneChunk::SizedElsewhere(elsewhere)) => {
                    self.size.set_sized_elsewhere(elsewhere);
                }
                Ok(PaneChunk::Output(data)) => {
                    let n = data.len().min(buf.len());
                    buf[..n].copy_from_slice(&data[..n]);
                    if n < data.len() {
                        self.buffer = data;
                        self.pos = n;
                    }
                    return Ok(n);
                }
                Err(_) => return Ok(0), // Channel closed → EOF.
            }
        }
    }
}

/// Max input bytes encoded into a single `send-keys` command. Each byte
/// becomes 3 chars (` XX`) under `-H`, so the command line stays ≈ `prefix +
/// 3·512` ≈ 1.6 KB — well under tmux's per-command line limit (which would
/// truncate a longer line). Larger writes are split across several commands.
pub const SEND_KEYS_CHUNK_BYTES: usize = 512;

/// Split `buf` into the ordered `send-keys -H` command lines for `pane_id`.
///
/// The byte-exact hex flag (each byte → two hex digits), chunked at
/// `SEND_KEYS_CHUNK_BYTES` so no single control-mode line gets over-long; the
/// raw bytes span the chunks and the receiving pane reassembles them. tmux's
/// encoding; a server without `-H` encodes its own way ([`PaneInput`]). A paste
/// does not come here — see [`ControlModeWriter`].
pub fn hex_send_keys_commands(pane_id: &str, buf: &[u8]) -> Vec<String> {
    buf.chunks(SEND_KEYS_CHUNK_BYTES)
        .map(|chunk| format_send_keys(pane_id, chunk))
        .collect()
}

/// The bracketed-paste markers a paste payload is wrapped in.
const PASTE_START: &[u8] = b"\x1b[200~";
const PASTE_END: &[u8] = b"\x1b[201~";

/// The pasted text inside a bracketed-paste payload (`ESC[200~ … ESC[201~`), or
/// `None` when `buf` is not exactly one such payload — ordinary keystrokes, a
/// payload split across writes, a marker in the middle (two pastes, or pasted
/// marker text), or non-UTF-8 bytes. [`ControlModeWriter`] refuses a write
/// that opens with a marker and is not one, rather than typing it out.
///
/// The markers are stripped: the server is handed the bare text and re-adds
/// them itself, only when the receiving app has bracketed paste on —
/// [`PaneInput::paste`], or `tmux_paste_commands` where that declines.
pub fn bracketed_paste_text(buf: &[u8]) -> Option<&str> {
    let inner = buf.strip_prefix(PASTE_START)?.strip_suffix(PASTE_END)?;
    let has_marker = |m: &[u8]| inner.windows(m.len()).any(|w| w == m);
    if has_marker(PASTE_START) || has_marker(PASTE_END) {
        return None;
    }
    std::str::from_utf8(inner).ok()
}

/// How a pane's input reaches it through control mode — the server's own
/// encoding, chosen by its adapter.
pub trait PaneInput: Send + Sync {
    /// The ordered control-mode lines that type `buf` into `pane_id`.
    fn send_keys(&self, pane_id: &str, buf: &[u8]) -> Vec<String>;

    /// Deliver `text`, a whole bracketed paste, to `pane_id` out of band, or
    /// `None` where the server pastes through control mode itself
    /// (`tmux_paste_commands`). An `Err` drops the paste: typing it out instead
    /// would make every CR in it `Enter`.
    fn paste(&self, pane_id: &str, text: &str) -> Option<Result<()>>;
}

/// Max text bytes per `set-buffer` line of `tmux_paste_commands`. Every
/// escape `tmux_quote` writes is at most four characters for one byte, so the
/// line stays within the budget [`SEND_KEYS_CHUNK_BYTES`] keeps `send-keys` to.
const SET_BUFFER_CHUNK_BYTES: usize = 384;

/// Split `text` into pieces of at most `max` bytes, never inside a character.
fn chunks_on_char_boundaries(text: &str, max: usize) -> Vec<&str> {
    let mut chunks = Vec::new();
    let mut start = 0;
    while start < text.len() {
        let mut end = (start + max).min(text.len());
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        chunks.push(&text[start..end]);
        start = end;
    }
    chunks
}

/// Double-quote `s` as one tmux command argument that survives a control-mode
/// line: tmux's parser reads `\n`, `\r`, `\t` and `\NNN` inside `"…"` (since
/// 3.0), so no raw line break ends the line early, and `$`/`~` are escaped
/// because tmux expands them there. A C1 control is written as `\u00NN`.
fn tmux_quote(s: &str) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for ch in s.chars() {
        match ch {
            '\\' | '"' | '$' | '~' => {
                out.push('\\');
                out.push(ch);
            }
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_ascii_control() => write!(out, "\\{:03o}", c as u32).unwrap(),
            c if c.is_control() => write!(out, "\\u{:04x}", c as u32).unwrap(),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// The control-mode lines that paste `text` into `pane_id` on a server that
/// takes no paste out of band.
///
/// The text goes into a buffer and `paste-buffer -p` puts it in the pane, so it
/// is **the server** that decides the markers: they are written only when the
/// pane's app has mode 2004 on, from its own record of the pane. thurbox's grid
/// cannot answer that — a pane adopted after a restart never showed this
/// process the `ESC[?2004h` that turned the mode on. `-r` keeps line feeds as
/// they are (tmux would turn each into a CR) and `-d` deletes the buffer once
/// pasted.
///
/// The buffer is this paste's alone — process id and a sequence number — since
/// two interfaces pasting into one pane over two connections would otherwise
/// interleave their `set-buffer -a` and `paste-buffer -d` on one buffer.
fn tmux_paste_commands(pane_id: &str, text: &str) -> Vec<String> {
    static PASTES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let buffer = format!(
        "thurbox-paste-{}-{}",
        std::process::id(),
        PASTES.fetch_add(1, Ordering::Relaxed)
    );
    let mut cmds: Vec<String> = chunks_on_char_boundaries(text, SET_BUFFER_CHUNK_BYTES)
        .into_iter()
        .enumerate()
        .map(|(i, chunk)| {
            let append = if i == 0 { "" } else { " -a" };
            format!("set-buffer{append} -b {buffer} -- {}\n", tmux_quote(chunk))
        })
        .collect();
    cmds.push(format!("paste-buffer -d -p -r -b {buffer} -t {pane_id}\n"));
    cmds
}

/// Per-pane writer that sends input via control-mode `send-keys` through the
/// shared control stdin, encoded the way the server's adapter says
/// ([`PaneInput`]).
///
/// A write that opens with `ESC[200~` is a paste, and a paste is never typed
/// out: in a key encoding every CR is `Enter`, so a paste typed out runs each
/// line it holds. It goes to the server's own paste path instead, which frames
/// it only for an app that asked; one that is not a single clean frame, or that
/// the server could not take, is dropped with a warning.
pub struct ControlModeWriter {
    pub stdin: Arc<Mutex<std::process::ChildStdin>>,
    pub pane_id: String,
    pub input: Arc<dyn PaneInput>,
}

impl Write for ControlModeWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        // `Ok` either way for a paste that is dropped: an error here ends the
        // writer task, and with it every later keystroke to this pane.
        let cmds = if buf.starts_with(PASTE_START) {
            let Some(text) = bracketed_paste_text(buf) else {
                warn!("dropped a paste that is not one bracketed frame");
                return Ok(buf.len());
            };
            if text.is_empty() {
                return Ok(buf.len());
            }
            match self.input.paste(&self.pane_id, text) {
                Some(Ok(())) => return Ok(buf.len()),
                Some(Err(e)) => {
                    warn!("out-of-band paste failed; the paste is dropped: {e:#}");
                    return Ok(buf.len());
                }
                None => tmux_paste_commands(&self.pane_id, text),
            }
        } else {
            self.input.send_keys(&self.pane_id, buf)
        };
        let mut stdin = self
            .stdin
            .lock()
            .map_err(|e| std::io::Error::other(format!("stdin lock: {e}")))?;
        for cmd in cmds {
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
/// All other bytes pass through unchanged — including raw bytes `>= 0x80`,
/// which tmux does not escape.
pub fn decode_octal(bytes: &[u8]) -> Vec<u8> {
    let mut result = Vec::with_capacity(bytes.len());
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
    if let Some(output) = parse_output(line.as_bytes()) {
        return output;
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

    if let Some(close) = parse_window_close(line) {
        return close;
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

    if let Some(n) = parse_layout_change(line) {
        return n;
    }

    Notification::Other(line.to_string())
}

/// `%output` and `%extended-output`: a pane's bytes.
///
/// Read from the raw line, never from text: tmux escapes only bytes below
/// `0x20` and `\`, passes the rest through, and cuts a pane's output into lines
/// wherever its read ended — often inside a multi-byte character. Decoding a
/// line as UTF-8 on its own turns both halves of that character into U+FFFD,
/// which vt100 drops; the reader's `carry` rejoins them only if they reach it
/// as bytes (`tests/lazy_terminals.rs`).
fn parse_output(line: &[u8]) -> Option<Notification> {
    if let Some(rest) = line.strip_prefix(b"%output ") {
        // Format: %output %<pane_id> <octal-encoded data>
        let split = rest.iter().position(|&b| b == b' ')?;
        return Some(Notification::Output {
            pane_id: String::from_utf8_lossy(&rest[..split]).into_owned(),
            data: decode_octal(&rest[split + 1..]),
        });
    }
    // Format: %extended-output %<pane_id> <age> : <octal-encoded data>
    // The " : " separator divides metadata from payload.
    let rest = line.strip_prefix(b"%extended-output ")?;
    let split = rest.windows(3).position(|w| w == b" : ")?;
    // meta is "%<pane_id> <age>" — extract pane_id.
    let meta = &rest[..split];
    let pane_id = &meta[..meta.iter().position(|&b| b == b' ')?];
    Some(Notification::Output {
        pane_id: String::from_utf8_lossy(pane_id).into_owned(),
        data: decode_octal(&rest[split + 3..]),
    })
}

/// `%window-close` and `%unlinked-window-close`, naming the window that went.
fn parse_window_close(line: &str) -> Option<Notification> {
    let rest = line
        .strip_prefix("%window-close ")
        .or_else(|| line.strip_prefix("%unlinked-window-close "))?;
    let window_id = rest.trim();
    // `%window-close` can carry a layout after the id in some tmux
    // versions; the id is the first token either way.
    let window_id = window_id.split_whitespace().next().unwrap_or(window_id);
    if window_id.is_empty() {
        return None;
    }
    Some(Notification::WindowClose {
        window_id: window_id.to_string(),
    })
}

/// `%layout-change @<window> <layout> <visible-layout> <flags>`, as the window
/// and the size its layout gives it.
///
/// A layout is `<checksum>,<cols>x<rows>,<x>,<y>…`, and its second field is the
/// whole window's size whatever the panes inside it are, so nothing past that
/// field is read.
fn parse_layout_change(line: &str) -> Option<Notification> {
    let mut tokens = line.strip_prefix("%layout-change ")?.split_whitespace();
    let window_id = tokens.next()?;
    if !is_valid_window_id(window_id) {
        return None;
    }
    let size = tokens.next()?.split(',').nth(1)?;
    let (cols, rows) = size.split_once('x')?;
    Some(Notification::LayoutChange {
        window_id: window_id.to_string(),
        rows: rows.parse().ok()?,
        cols: cols.parse().ok()?,
    })
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

/// A tmux window id is `@<digits>`.
///
/// Checked for a different reason than the pane id: a window id is never
/// interpolated into a command, only compared against the one `%window-close`
/// carries. A value that is not an id would therefore never match and never
/// complain — indistinguishable from a pane whose death is simply not
/// announced, which is the failure this mapping exists to prevent.
pub fn is_valid_window_id(s: &str) -> bool {
    s.strip_prefix('@')
        .is_some_and(|rest| !rest.is_empty() && rest.bytes().all(|b| b.is_ascii_digit()))
}

/// Parse `list-panes -F "#{pane_id} #{@thurbox_state}"` output into the
/// `(pane_id, value)` pairs whose option is **set**: one `%<id> [value]` line
/// per pane; empty values (option unset) and malformed lines are skipped —
/// wire data never panics. Shared by the hook poller's diff below and the
/// headless listing (`SessionBackend::hook_states`).
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

/// Parse `list-panes -a -F "#{pane_id}"` output into the set of panes the
/// server has, dead ones included. Malformed lines are skipped. Backs
/// `SessionBackend::pane_ids`, the existence question a pid cannot answer.
pub fn parse_pane_ids(body: &str) -> std::collections::HashSet<String> {
    body.lines()
        .map(str::trim)
        .filter(|id| is_valid_pane_id(id))
        .map(str::to_string)
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

/// Diff one hook-poll result against the previous poll, returning the
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
pub(in crate::backend) struct ControlMode {
    pub(in crate::backend) stdin: Arc<Mutex<ChildStdin>>,
    pub(in crate::backend) pane_senders: PaneSendersMapShared,
    /// Where each registered pane lives, for turning `%window-close` into EOF.
    /// Written by `register_pane`/`unregister_pane`, read by the reader thread.
    pub(in crate::backend) pane_windows: PaneWindowsMapShared,
    /// Where each pane's reader applies a size — written and read as
    /// `pane_windows` is, and only for a pane whose sizes are reported.
    pub(in crate::backend) pane_sizes: PaneSizesMapShared,
    /// FIFO queue of waiters — one per command written, in the order written.
    /// Every sender takes a place, including the ones that will not read the
    /// answer (`send_command_detached`) or will stop waiting for it
    /// (`send_command_within`): the place is what keeps the queue aligned with
    /// the wire, not the caller's interest in what comes back.
    response_queue: ResponseQueue,
    /// `(pane_id, state)` pairs from `%subscription-changed` notifications
    /// (remote hook status — see [`REMOTE_HOOK_STATE_OPTION`]),
    /// pushed by the reader thread and drained by the app tick via
    /// [`Self::take_sub_events`]. Bounded (drop-oldest): a short-lived
    /// connection (e.g. a headless spawn's) has no drainer.
    sub_events: Arc<Mutex<VecDeque<(String, String)>>>,
    /// True while this connection lives; cleared on reader EOF and in `Drop`.
    /// The hook poller checks it each cycle so a replaced connection's
    /// poller winds down instead of writing into a dead pipe forever.
    alive: Arc<AtomicBool>,
    reader_handle: Mutex<Option<JoinHandle<()>>>,
    child: Mutex<Child>,
}

/// Cap on queued subscription events. Status transitions are rare and the
/// queue is drained every TUI tick — the cap only guards an undrained
/// connection against unbounded growth.
const SUB_EVENTS_CAP: usize = 256;

/// How often a hook poller lists pane options — matches tmux's own ≤1/s
/// subscription-report cadence, so both channels have the same worst-case
/// status latency.
const HOOK_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(1000);

/// What a control-mode connection may expect of the server it talks to — its
/// adapter's answer, measured, never a guess from the binary's name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ControlPolicy {
    /// Command that limits buffered pane output, when this control server
    /// offers such a facility.
    pub flow_control_command: Option<&'static str>,
    /// Whether the server answers the `attach-session` carried on argv with a
    /// `%begin`/`%end` block of its own, which
    /// `ControlMode::drain_implicit_attach_response` must consume before any
    /// waiter exists. Waiting for one a server never sends never returns.
    pub implicit_attach_reply: bool,
    /// Whether a reply block's `%end` carries its `%begin`'s tag, so a block
    /// can hold any line a pane showed. Where it does not, any `%end` ends the
    /// block, as before tags were read.
    pub tagged_blocks: bool,
    /// Whether the server pushes format subscriptions (`refresh-client -B`):
    /// the remote-hook status and which client sizes each pane.
    pub subscriptions: bool,
    /// Where status cannot be subscribed to but can be asked for: the command
    /// listing every pane's hook-state option, polled each
    /// `HOOK_POLL_INTERVAL` into the queue a subscription would feed.
    pub status_poll: Option<String>,
}

impl ControlMode {
    /// Start a control mode connection to the thurbox tmux session over the
    /// given transport (local or ssh).
    /// `sizer` is this client's name in [`SIZER_OPTION`], so a pane named for
    /// anybody else can be reported as sized elsewhere.
    pub(in crate::backend) fn start(
        transport: &TmuxTransport,
        socket: &str,
        session: &str,
        sizer: &str,
        policy: &ControlPolicy,
    ) -> Result<Self> {
        // -C (single C): control mode with echo — works with piped stdin.
        // -CC (double C) requires a TTY and fails with "tcgetattr: Inappropriate ioctl".
        let mut child = transport
            .tmux_command(socket, &["-C", "attach-session", "-t", session])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            // No `tmux`/`ssh`/`wsl.exe` on this machine at all: the message
            // names which, where thurbox looked and the fix, rather than the
            // errno the launcher raised.
            .map_err(|e| transport.launch_failure("Failed to start tmux control mode", e))?;

        let stdin = child
            .stdin
            .take()
            .context("Failed to get control mode stdin")?;
        let stdout = child
            .stdout
            .take()
            .context("Failed to get control mode stdout")?;

        // Consume tmux's implicit response to the `-C attach-session` command
        // carried on argv — the one `%begin`/`%end` block tmux emits on
        // connect that is not a reply to anything we send. This must happen
        // synchronously, before the reader thread exists: `deliver_response`
        // matches replies to waiters by queue position only, so if the
        // reader thread instead raced a `send_command` no-op meant to drain
        // this block — as this used to — whichever finished first decided
        // whether that no-op's waiter received this block (harmless, since
        // both are empty) or the *real* reply to the no-op did, silently
        // shifting every later response one FIFO slot for the rest of the
        // connection's life. Draining here, before any waiter can exist,
        // makes that race impossible instead of merely unlikely.
        let mut reader = BufReader::new(stdout);
        if policy.implicit_attach_reply {
            Self::drain_implicit_attach_response(&mut reader)?;
        }

        let stdin = Arc::new(Mutex::new(stdin));
        let pane_senders: PaneSendersMapShared =
            Arc::new(Mutex::new(std::collections::HashMap::new()));
        let pane_windows: PaneWindowsMapShared =
            Arc::new(Mutex::new(std::collections::HashMap::new()));
        let pane_sizes: PaneSizesMapShared = Arc::default();
        let response_queue: ResponseQueue = Arc::new(Mutex::new(VecDeque::new()));
        let sub_events: Arc<Mutex<VecDeque<(String, String)>>> =
            Arc::new(Mutex::new(VecDeque::new()));
        let alive = Arc::new(AtomicBool::new(true));

        let reader_stdin = Arc::clone(&stdin);
        let reader_pane_senders = Arc::clone(&pane_senders);
        let reader_sizer = sizer.to_string();
        let reader_pane_windows = Arc::clone(&pane_windows);
        let reader_pane_sizes = Arc::clone(&pane_sizes);
        let reader_queue = Arc::clone(&response_queue);
        let reader_sub_events = Arc::clone(&sub_events);
        let reader_alive = Arc::clone(&alive);

        let strict_blocks = policy.tagged_blocks;
        let reader_handle = std::thread::Builder::new()
            .name("tmux-control-reader".into())
            .spawn(move || {
                Self::reader_thread(
                    reader,
                    reader_stdin,
                    reader_pane_senders,
                    reader_pane_windows,
                    reader_pane_sizes,
                    reader_queue,
                    reader_sub_events,
                    strict_blocks,
                    &reader_sizer,
                );
                reader_alive.store(false, Ordering::Relaxed);
            })
            .context("Failed to spawn control reader thread")?;

        let control = Self {
            stdin,
            pane_senders,
            pane_windows,
            pane_sizes,
            response_queue,
            sub_events,
            alive,
            reader_handle: Mutex::new(Some(reader_handle)),
            child: Mutex::new(child),
        };

        if let Some(command) = policy.flow_control_command {
            control.send_command(command)?;
        }

        // Subscribe to the remote-hook status option of every pane of the
        // attached session (tmux pushes `%subscription-changed` on change) —
        // how an off-local agent's hooks reach the local status derivation.
        // Only where the server has format subscriptions, and best-effort: a
        // refusal must not brick the whole backend, status just stays dark.
        // Armed here — not per pane — so `reconnect_control` re-arms for free
        // and panes created later are covered (`%*` is session-scoped).
        if policy.subscriptions {
            let arm = format!(
                "refresh-client -B '{}:%*:#{{{}}}'",
                REMOTE_HOOK_SUBSCRIPTION, REMOTE_HOOK_STATE_OPTION,
            );
            if let Err(e) = control.send_command(&arm) {
                warn!("failed to arm the remote-hook status subscription: {e:#}");
            }
            // Which client sizes each pane, for the interface to say why a
            // pane is not the size of its rect. The same passive mechanism and
            // the same best effort: without it the hint is simply never shown.
            let arm = format!("refresh-client -B '{SIZER_SUBSCRIPTION}:%*:{SIZED_BY}'");
            if let Err(e) = control.send_command(&arm) {
                warn!("failed to arm the pane sizer subscription: {e:#}");
            }
        } else if let Some(command) = &policy.status_poll {
            // Unlike the subscription (passive — zero recurring cost), the
            // poller is a 1 Hz command, so the adapter asks for it only where a
            // producer can exist.
            control.spawn_hook_poller(command.clone());
        }

        Ok(control)
    }

    /// A server with no format subscriptions **polls** the remote-hook pane
    /// option instead, where its adapter asks for it
    /// ([`ControlPolicy::status_poll`]): a background thread runs `cmd` — a
    /// listing of every pane of the session with its `@thurbox_state` — each
    /// [`HOOK_POLL_INTERVAL`], diffs against the previous poll
    /// ([`diff_polled_hook_states`]), and feeds changes into the same
    /// `sub_events` queue the tmux subscription uses — everything downstream
    /// (`take_hook_state_events` → the app's drain) is shared. Best-effort: a
    /// command failure ends the thread (the connection is dying; a reconnect's
    /// fresh `ControlMode` spawns a fresh poller), and an idle server pays one
    /// cheap command per second on an already-persistent connection.
    ///
    /// The thread is deliberately **detached** (not joined in `Drop`): a poll
    /// blocked in its command timeout when the connection dies would stall the
    /// drop for [`COMMAND_TIMEOUT`]; instead it exits on its own via the
    /// `alive` flag or the dead pipe shortly after.
    fn spawn_hook_poller(&self, cmd: String) {
        let stdin = Arc::clone(&self.stdin);
        let queue = Arc::clone(&self.response_queue);
        let events = Arc::clone(&self.sub_events);
        let alive = Arc::clone(&self.alive);
        let spawned = std::thread::Builder::new()
            .name("hook-poller".into())
            .spawn(move || {
                let mut last = std::collections::HashMap::new();
                loop {
                    std::thread::sleep(HOOK_POLL_INTERVAL);
                    if !alive.load(Ordering::Relaxed) {
                        break;
                    }
                    let body = match Self::send_command_on(&stdin, &queue, &cmd, 1) {
                        Ok(body) => body,
                        Err(e) => {
                            debug!("hook poller stopping: {e:#}");
                            break;
                        }
                    };
                    let changed = diff_polled_hook_states(&mut last, &body);
                    Self::queue_sub_events(&events, changed);
                }
            });
        if let Err(e) = spawned {
            warn!("failed to spawn the hook poller: {e}");
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

    /// One newline-terminated control-mode line into `line_buf`, without its
    /// newline; `false` at EOF / on an I/O error (both of which end the
    /// reader).
    fn read_control_line(reader: &mut impl BufRead, line_buf: &mut Vec<u8>) -> bool {
        line_buf.clear();
        match reader.read_until(b'\n', line_buf) {
            Ok(0) => return false,
            Ok(_) => {}
            Err(e) => {
                debug!("Control reader I/O error: {e}");
                return false;
            }
        }
        if line_buf.last() == Some(&b'\n') {
            line_buf.pop();
        }
        true
    }

    /// [`Self::read_control_line`] as text, or `None` where it returns `false`.
    ///
    /// Lossy, which is only safe for a line that holds whole characters. A
    /// pane's output does not (see [`parse_output`]), so the reader takes
    /// `%output` from the bytes before converting anything. A command's reply
    /// does: tmux writes it in one piece, and a `capture-pane` line is a row
    /// of whole cells.
    fn next_control_line(reader: &mut impl BufRead, line_buf: &mut Vec<u8>) -> Option<String> {
        Self::read_control_line(reader, line_buf)
            .then(|| String::from_utf8_lossy(line_buf).into_owned())
    }

    /// Synchronously consume tmux's implicit `%begin`/`%end`(`%error`) reply
    /// to the `-C attach-session` command carried on argv, before the reader
    /// thread (and thus any `response_queue` waiter) exists — see the call
    /// site in [`Self::start`] for why that ordering matters. Any notification
    /// lines ahead of the block (tmux has been observed to send `%output` /
    /// `%session-changed` first) are harmless to skip here: nothing is
    /// registered to receive them yet.
    fn drain_implicit_attach_response(reader: &mut impl BufRead) -> Result<()> {
        let mut line_buf = Vec::new();
        while let Some(line) = Self::next_control_line(reader, &mut line_buf) {
            if !matches!(parse_notification(&line), Notification::Begin) {
                continue;
            }
            while let Some(line) = Self::next_control_line(reader, &mut line_buf) {
                if matches!(
                    parse_notification(&line),
                    Notification::End | Notification::Error
                ) {
                    return Ok(());
                }
            }
            bail!("control mode closed mid-way through its implicit attach response");
        }
        bail!("control mode closed before sending its implicit attach response");
    }

    /// Background thread that reads and dispatches control mode output.
    ///
    /// Responses arrive in FIFO order matching `send_command()` calls.
    /// We track a single in-flight block at a time (`%begin` → collect
    /// lines → `%end`/`%error`), then hand it to the waiter at the front of
    /// the queue, which a command list keeps for as many blocks as it has
    /// commands (see [`Self::deliver_response`]).
    /// Commands sent via `send_command_nowait()` also produce `%begin`/`%end`
    /// blocks, but no waiter is in the queue for them — those responses are
    /// simply discarded.
    #[allow(clippy::too_many_arguments)]
    fn reader_thread(
        mut reader: BufReader<std::process::ChildStdout>,
        stdin: Arc<Mutex<ChildStdin>>,
        pane_senders: PaneSendersMapShared,
        pane_windows: PaneWindowsMapShared,
        pane_sizes: PaneSizesMapShared,
        response_queue: ResponseQueue,
        sub_events: Arc<Mutex<VecDeque<(String, String)>>>,
        strict_blocks: bool,
        sizer: &str,
    ) {
        // The in-flight block: the tag its `%begin` carried, and its lines.
        let mut collecting: Option<(Option<String>, Vec<String>)> = None;
        let mut answering: Option<Answer> = None;
        let mut line_buf = Vec::new();

        while Self::read_control_line(&mut reader, &mut line_buf) {
            // A pane's bytes, taken before the line is text (`parse_output`).
            // Not inside a tagged block, whose every line is the reply's.
            if !matches!(collecting, Some((Some(_), _))) {
                if let Some(Notification::Output { pane_id, data }) = parse_output(&line_buf) {
                    Self::dispatch_output(&pane_senders, &pane_id, data);
                    continue;
                }
            }
            let line = String::from_utf8_lossy(&line_buf).into_owned();
            // Inside a block every line is the command's output until the
            // `%end`/`%error` that carries the `%begin`'s tag — tmux writes a
            // command's output in one piece, never with a notification in it,
            // and that output can be anything a pane showed (`capture-pane`).
            if let Some((Some(tag), lines)) = &mut collecting {
                let ends = (line.starts_with("%end ") || line.starts_with("%error "))
                    && block_tag(&line).map(joined_tag).as_ref() == Some(tag);
                if !ends {
                    lines.push(line);
                    continue;
                }
                let lines = std::mem::take(lines);
                collecting = None;
                let is_error = line.starts_with("%error ");
                Self::deliver_response(
                    &response_queue,
                    &pane_senders,
                    &mut answering,
                    lines,
                    is_error,
                );
                continue;
            }
            match parse_notification(&line) {
                Notification::Output { pane_id, data } => {
                    Self::dispatch_output(&pane_senders, &pane_id, data);
                }
                Notification::Begin => {
                    // A server whose blocks are not known to carry tmux's tag
                    // ([`ControlPolicy::tagged_blocks`]) has a block framed as
                    // it was before tags were read: any `%end` ends it.
                    let tag = block_tag(&line).map(joined_tag).filter(|_| strict_blocks);
                    collecting = Some((tag, Vec::new()));
                }
                end_or_error @ (Notification::End | Notification::Error) => {
                    let lines = collecting
                        .take()
                        .map(|(_, lines)| lines)
                        .unwrap_or_default();
                    let is_error = matches!(end_or_error, Notification::Error);
                    Self::deliver_response(
                        &response_queue,
                        &pane_senders,
                        &mut answering,
                        lines,
                        is_error,
                    );
                }
                Notification::Pause { pane_id } => {
                    Self::resume_pane(&stdin, &pane_id);
                }
                Notification::WindowClose { window_id } => {
                    Self::close_window_panes(&pane_senders, &pane_windows, &window_id);
                    if let Ok(mut sizes) = pane_sizes.lock() {
                        let windows = pane_windows.lock().ok();
                        sizes.retain(|pane, _| {
                            windows.as_ref().is_some_and(|w| w.contains_key(pane))
                        });
                    }
                }
                Notification::LayoutChange {
                    window_id,
                    rows,
                    cols,
                } => {
                    Self::dispatch_resize(
                        &pane_senders,
                        &pane_windows,
                        &pane_sizes,
                        &window_id,
                        rows,
                        cols,
                    );
                }
                // Consumed even mid-%begin block: tmux never interleaves
                // notifications inside response bodies, so this can't eat a
                // response line. Empty value = the pane option is unset.
                Notification::SubscriptionChanged {
                    name,
                    pane_id,
                    value,
                } => {
                    if name == REMOTE_HOOK_SUBSCRIPTION && !value.is_empty() {
                        Self::queue_sub_events(&sub_events, vec![(pane_id, value)]);
                    } else if name == SIZER_SUBSCRIPTION {
                        let elsewhere = !value.is_empty() && value != sizer;
                        Self::send_event(
                            &pane_senders,
                            &pane_id,
                            PaneChunk::SizedElsewhere(elsewhere),
                        );
                    }
                }
                Notification::Other(text) => {
                    if let Some((_, lines)) = &mut collecting {
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
        if let Ok(mut windows) = pane_windows.lock() {
            windows.clear();
        }
        if let Ok(mut sizes) = pane_sizes.lock() {
            sizes.clear();
        }
    }

    /// A window closed: drop the senders of every pane that was in it, which is
    /// what gives those panes' `reader_loop`s their EOF.
    ///
    /// Without this the channel simply goes quiet, and quiet is indistinguishable
    /// from a program that has nothing to say: `exited` stays false, so nothing
    /// fires `program.exited`, the surface keeps painting the grid the program
    /// left behind, and `start_program` keeps answering "already running" to
    /// every request to open it again. That last one has no error path — it
    /// returns `Ok(())` — so the symptom is a pane that cannot be reopened and a
    /// log with nothing in it. (Found 2026-09-11: `:q` in the editor pane, and
    /// no click or key would ever bring it back.)
    fn close_window_panes(
        pane_senders: &PaneSendersMapShared,
        pane_windows: &PaneWindowsMapShared,
        window_id: &str,
    ) {
        let gone: Vec<String> = match pane_windows.lock() {
            Ok(mut windows) => {
                let gone: Vec<String> = windows
                    .iter()
                    .filter(|(_, window)| window.as_str() == window_id)
                    .map(|(pane, _)| pane.clone())
                    .collect();
                for pane in &gone {
                    windows.remove(pane);
                }
                gone
            }
            Err(_) => return,
        };
        if gone.is_empty() {
            // A window nothing was reading — every window the interface did not
            // open itself, which is most of them on a shared server.
            return;
        }
        if let Ok(mut senders) = pane_senders.lock() {
            for pane in &gone {
                senders.remove(pane);
            }
        }
        debug!(window_id, panes = ?gone, "window closed, its pane readers get EOF");
    }

    /// Tell the readers of every pane in `window_id` that it is now `rows` ×
    /// `cols`, in line with the output around it (see [`PaneChunk`]).
    ///
    /// Only a pane registered with its window can be told, which is every pane
    /// this connection wired up once it learnt the window
    /// ([`crate::backend::tmux`]'s `register_pane`). The reader thread must never
    /// block, but a size must not be dropped the way output is when a channel is
    /// full: a resident grid has nothing that would ever correct it. So a size
    /// with no room in the channel goes straight to where the pane's reader
    /// applies sizes, at its next read — early for the bytes still queued, and
    /// the program repaints after a resize anyway.
    fn dispatch_resize(
        pane_senders: &PaneSendersMapShared,
        pane_windows: &PaneWindowsMapShared,
        pane_sizes: &PaneSizesMapShared,
        window_id: &str,
        rows: u16,
        cols: u16,
    ) {
        let panes: Vec<String> = match pane_windows.lock() {
            Ok(windows) => windows
                .iter()
                .filter(|(_, window)| window.as_str() == window_id)
                .map(|(pane, _)| pane.clone())
                .collect(),
            Err(_) => return,
        };
        for pane in &panes {
            if Self::send_event(pane_senders, pane, PaneChunk::Resized { rows, cols }) {
                continue;
            }
            if let Some(size) = pane_sizes.lock().ok().and_then(|s| s.get(pane).cloned()) {
                size.report(rows, cols);
            }
        }
    }

    /// Hand one event to every reader of `pane_id`, dropping it for a full
    /// channel as output is dropped: the reader thread must never block.
    /// Whether every reader took it.
    fn send_event(pane_senders: &PaneSendersMapShared, pane_id: &str, event: PaneChunk) -> bool {
        let Ok(senders) = pane_senders.lock() else {
            return false;
        };
        let Some(tx_vec) = senders.get(pane_id) else {
            return false;
        };
        let mut all = true;
        for tx in tx_vec {
            if tx.try_send(event.clone()).is_err() {
                debug!(pane_id = %pane_id, "Pane channel full or gone, dropping an event");
                all = false;
            }
        }
        all
    }

    /// Broadcast a `%output` payload to every reader registered for `pane_id`.
    ///
    /// Uses `try_send` so the reader thread never blocks: a full channel drops
    /// the chunk rather than stalling (which would deadlock `%pause` handling).
    fn dispatch_output(pane_senders: &PaneSendersMapShared, pane_id: &str, data: Vec<u8>) {
        Self::dispatch(pane_senders, pane_id, PaneChunk::Output(data));
    }

    /// Hand `chunk` to every reader registered for `pane_id`, never blocking —
    /// see [`Self::dispatch_output`].
    fn dispatch(pane_senders: &PaneSendersMapShared, pane_id: &str, chunk: PaneChunk) {
        let Ok(senders) = pane_senders.lock() else {
            return;
        };
        let Some(tx_vec) = senders.get(pane_id) else {
            return;
        };
        // Single-sender is the dominant case (one reader per pane): move the
        // chunk into it instead of cloning. Only fan-out (multiple registered
        // instances) pays for a clone.
        let mut chunk = Some(chunk);
        for (i, tx) in tx_vec.iter().enumerate() {
            let this = if i + 1 == tx_vec.len() {
                chunk.take()
            } else {
                chunk.clone()
            };
            let Some(this) = this else {
                return;
            };
            match tx.try_send(this) {
                Ok(()) => {}
                Err(std::sync::mpsc::TrySendError::Full(_dropped)) => {
                    debug!(pane_id = %pane_id, "Pane output channel full, dropping chunk");
                }
                Err(std::sync::mpsc::TrySendError::Disconnected(_)) => {}
            }
        }
    }

    /// Add a completed `%begin`/`%end`(`%error`) block to the answer in
    /// progress — the next waiter's, when none is — and hand the answer over
    /// once its last block is in, or at an error: tmux drops the rest of a list
    /// at its first failing command, so no more blocks of it will come.
    ///
    /// Responses with no waiter in the queue (e.g. from `send_command_nowait`)
    /// are simply discarded.
    fn deliver_response(
        response_queue: &ResponseQueue,
        pane_senders: &PaneSendersMapShared,
        answering: &mut Option<Answer>,
        lines: Vec<String>,
        is_error: bool,
    ) {
        let mut answer = match answering.take() {
            Some(answer) => answer,
            None => {
                let Ok(mut queue) = response_queue.lock() else {
                    return;
                };
                let Some(waiter) = queue.pop_front() else {
                    return;
                };
                Answer {
                    waiter,
                    blocks: Vec::new(),
                }
            }
        };
        answer.blocks.push(lines);
        answer.waiter.blocks = answer.waiter.blocks.saturating_sub(1);
        if !is_error && answer.waiter.blocks > 0 {
            *answering = Some(answer);
            return;
        }
        // Here, on this thread, and before the next line is read: every
        // `%output` ahead of this answer has been handed to the pane already and
        // none behind it has, which is the one place the snapshot is exact.
        if let (false, Some(pane)) = (is_error, &answer.waiter.splice) {
            if let Some(snapshot) = parse_snapshot(answer.blocks.clone()) {
                Self::dispatch(pane_senders, pane, PaneChunk::Snapshot(Box::new(snapshot)));
            }
        }
        let _ = answer.waiter.tx.send(CommandResponse {
            blocks: answer.blocks,
            is_error,
        });
    }

    /// Drain the queued `(pane_id, state)` remote-hook status events.
    pub(in crate::backend) fn take_sub_events(&self) -> Vec<(String, String)> {
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
    pub(in crate::backend) fn send_command(&self, cmd: &str) -> Result<String> {
        Self::send_command_on(&self.stdin, &self.response_queue, cmd, 1)
    }

    /// Send commands as one command list (`a ; b ; c`) and wait for the number
    /// of reply blocks this server sends for the list.
    ///
    /// One line and not several because tmux runs a list without returning to
    /// its event loop in between (what `birth_options` relies on). Each entry
    /// must be a single command: an uncounted extra block reaches the next
    /// waiter.
    pub(in crate::backend) fn send_command_list(
        &self,
        cmds: &[&str],
        blocks: usize,
    ) -> Result<String> {
        if cmds.is_empty() {
            bail!("an empty command list has nothing to send");
        }
        Self::send_command_on(&self.stdin, &self.response_queue, &cmds.join(" ; "), blocks)
    }

    /// [`Self::send_command`] without `&self`, so background threads holding
    /// only the shared handles (the hook poller) can issue commands.
    ///
    /// Both locks are held across enqueue **and** write: concurrent senders
    /// (a backend caller vs the poller) must not interleave one thread's
    /// waiter-push with another's stdin-write, or the FIFO waiter order stops
    /// matching the on-wire command order and every later response is
    /// delivered one command off. A failed write pops the just-enqueued
    /// waiter for the same reason.
    ///
    /// `blocks` is how many commands `cmd` holds (see [`Waiter`]).
    fn send_command_on(
        stdin: &Arc<Mutex<ChildStdin>>,
        response_queue: &ResponseQueue,
        cmd: &str,
        blocks: usize,
    ) -> Result<String> {
        let rx = Self::enqueue_command_on(stdin, response_queue, cmd, blocks, None)?;
        Self::await_response(rx, cmd, COMMAND_TIMEOUT)
    }

    /// Write `cmd` and take a place in the waiter queue, without waiting.
    ///
    /// Split out of [`Self::send_command_on`] so a caller can decline the wait
    /// ([`Self::send_command_detached`]) or shorten it
    /// ([`Self::send_command_within`]) while every caller keeps the one
    /// invariant that matters: a place in the queue per command written, in
    /// the order written.
    ///
    /// `splice` names the pane whose output stream the answer is also put
    /// into, for a [`snapshot_commands`] list (see [`Waiter::splice`]).
    fn enqueue_command_on(
        stdin: &Arc<Mutex<ChildStdin>>,
        response_queue: &ResponseQueue,
        cmd: &str,
        blocks: usize,
        splice: Option<String>,
    ) -> Result<Receiver<CommandResponse>> {
        let (tx, rx) = sync_channel(1);

        {
            // Lock order: stdin first, queue only for the brief push/pop.
            // Holding stdin across enqueue AND write keeps the FIFO waiter
            // order matching the on-wire command order for concurrent senders
            // (a backend caller vs the hook poller) — while never holding the
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
                queue.push_back(Waiter { tx, blocks, splice });
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

        Ok(rx)
    }

    /// Wait for an enqueued command's answer, for at most `budget`.
    fn await_response(
        rx: Receiver<CommandResponse>,
        cmd: &str,
        budget: std::time::Duration,
    ) -> Result<String> {
        let response = Self::await_blocks(rx, cmd, budget)?;
        Ok(response.lines().join("\n"))
    }

    /// [`Self::await_response`], keeping each command's block apart.
    fn await_blocks(
        rx: Receiver<CommandResponse>,
        cmd: &str,
        budget: std::time::Duration,
    ) -> Result<CommandResponse> {
        let response = rx
            .recv_timeout(budget)
            .with_context(|| format!("Timeout waiting for response to: {cmd}"))?;

        if response.is_error {
            bail!(
                "tmux command failed: {cmd}: {}",
                response.lines().join("\n")
            );
        }

        Ok(response)
    }

    /// [`Self::send_command`] on a budget of the caller's choosing.
    ///
    /// For the callers that are the interface's own loop, where the answer is
    /// worth a short wait and nothing is worth a long one.
    pub(in crate::backend) fn send_command_within(
        &self,
        cmd: &str,
        budget: std::time::Duration,
    ) -> Result<String> {
        let rx = Self::enqueue_command_on(&self.stdin, &self.response_queue, cmd, 1, None)?;
        Self::await_response(rx, cmd, budget)
    }

    /// Ask for a [`PaneSnapshot`] of `pane_id` and do not wait for it: it is
    /// put into the pane's own output stream as a [`PaneChunk::Snapshot`], at
    /// the byte it describes, for that pane's reader to take up in order.
    pub(in crate::backend) fn request_snapshot(&self, pane_id: &str, history: usize) -> Result<()> {
        let cmds = snapshot_commands(pane_id, history, true);
        Self::enqueue_command_on(
            &self.stdin,
            &self.response_queue,
            &cmds.join(" ; "),
            cmds.len(),
            Some(pane_id.to_string()),
        )
        .map(drop)
    }

    /// Ask for a [`PaneSnapshot`] of `pane_id` as unstyled text, handed back to
    /// the caller rather than put into the stream — for a reader that wants the
    /// text as it stands (the content search). Returns once asked; the answer
    /// is [`PendingSnapshot::wait`]ed for separately, so a caller holding the
    /// backend's control lock can let go of it first and several searches'
    /// round trips overlap.
    pub(in crate::backend) fn ask_snapshot(
        &self,
        pane_id: &str,
        history: usize,
    ) -> Result<PendingSnapshot> {
        let cmds = snapshot_commands(pane_id, history, false);
        let cmd = cmds.join(" ; ");
        let rx =
            Self::enqueue_command_on(&self.stdin, &self.response_queue, &cmd, cmds.len(), None)?;
        Ok(PendingSnapshot {
            rx,
            cmd,
            pane: pane_id.to_string(),
        })
    }

    /// Send a command and never wait for its answer.
    ///
    /// The answer still comes — control mode replies to everything — so a place
    /// is kept for it and the receiver dropped, which makes `deliver_response`
    /// discard it (its `send` already tolerates a receiver that is gone). That
    /// kept place is the whole difference from [`Self::send_command_nowait`],
    /// whose documented hazard is exactly its absence: with no place of its
    /// own, the answer is handed to whichever waiter is next in line and every
    /// later response is delivered one command off for the life of the
    /// connection.
    ///
    /// For a command whose *effect* is the point and whose answer nothing
    /// reads — a resize tells the agent how to wrap, and no frame is waiting on
    /// the confirmation.
    ///
    /// Takes a list for the same reason [`Self::send_command_list`] does: tmux
    /// runs a list without returning to its event loop in between, and one list
    /// is one enqueue under one lock. Two calls could take the lock separately
    /// and have the second refused, leaving the first applied on its own.
    ///
    /// `blocks` is how many `%begin` blocks tmux answers the list with, which
    /// is one per command **plus one per command an `if-shell` in it runs** —
    /// those answer separately (measured, tmux 3.7c). A wrong count hands the
    /// surplus to the next waiter and shifts every later answer.
    pub(in crate::backend) fn send_command_detached(
        &self,
        cmds: &[&str],
        blocks: usize,
    ) -> Result<()> {
        if cmds.is_empty() {
            bail!("an empty command list has nothing to send");
        }
        Self::enqueue_command_on(
            &self.stdin,
            &self.response_queue,
            &cmds.join(" ; "),
            blocks,
            None,
        )
        .map(drop)
    }

    /// Send a command without waiting for a response.
    ///
    /// **Caution**: The response (`%begin`/`%end`) will still arrive on the
    /// control mode stream. If a `send_command` call follows before the
    /// response is consumed, the nowait response may steal the waiter.
    /// Only use this when no `send_command` follows, or when the caller
    /// is the reader thread itself (e.g., pause resume).
    pub(in crate::backend) fn send_command_nowait(&self, cmd: &str) -> Result<()> {
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
        // Wind down the (detached) hook poller; it observes the flag on
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
pub(in crate::backend) fn is_broken_pipe(err: &anyhow::Error) -> bool {
    err.chain().any(|cause| {
        cause
            .downcast_ref::<std::io::Error>()
            .is_some_and(|e| e.kind() == std::io::ErrorKind::BrokenPipe)
    })
}

/// Check if an error is caused by a recv timeout (reader thread died, response never arrives).
pub(in crate::backend) fn is_recv_timeout(err: &anyhow::Error) -> bool {
    err.chain().any(|cause| {
        cause
            .downcast_ref::<std::sync::mpsc::RecvTimeoutError>()
            .is_some()
    })
}

#[cfg(test)]
mod tests;
