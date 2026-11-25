#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Missing {
    Server,
    Target,
}

#[derive(Debug)]
pub(crate) struct Refusal {
    pub command: String,
    pub reply: String,
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} failed: {}", self.command, self.reply)
    }
}

impl std::error::Error for Refusal {}

use std::collections::{HashMap, VecDeque};
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{sync_channel, Receiver, SyncSender};
use std::sync::{Arc, Condvar, Mutex, Weak};
use std::thread::JoinHandle;

use anyhow::{bail, Context, Result};
use tracing::{debug, warn};

use super::transport::TmuxTransport;
use crate::contract::{PaneChunk, PaneSize, PaneSnapshot};

pub const PANE_CHANNEL_CAPACITY: usize = 4096;

const SNAPSHOT_FORMAT: &str =
    "#{pane_width} #{pane_height} #{cursor_x} #{cursor_y} #{alternate_on}";

pub fn snapshot_commands(pane_id: &str, history: usize, styled: bool) -> Vec<String> {
    let start = format!("-{history}");
    let e = if styled { " -e" } else { "" };
    vec![
        format!("display-message -p -t {pane_id} '{SNAPSHOT_FORMAT}'"),
        format!("capture-pane -p{e} -J -S {start} -t {pane_id}"),
        format!("capture-pane -a -q -p{e} -J -S {start} -t {pane_id}"),
    ]
}

fn snapshot_commands_one_block(pane_id: &str, history: usize, styled: bool) -> Vec<String> {
    let mut commands = snapshot_commands(pane_id, history, styled);
    let marker = format!("__thurbox_snapshot_{}__", uuid::Uuid::new_v4().simple());
    commands.insert(1, format!("display-message -p '{marker}normal__'"));
    commands.insert(3, format!("display-message -p '{marker}alternate__'"));
    commands
}

pub fn parse_snapshot(mut blocks: Vec<Vec<String>>) -> Option<PaneSnapshot> {
    if blocks.len() == 1 {
        let lines = blocks.pop()?;
        let normal = lines.iter().position(|line| {
            line.starts_with("__thurbox_snapshot_") && line.ends_with("__normal__")
        })?;
        let marker = lines[normal].strip_suffix("normal__")?;
        let alternate_marker = format!("{marker}alternate__");
        let alternate = lines.iter().position(|line| line == &alternate_marker)?;
        if normal != 1 || alternate <= normal {
            return None;
        }
        blocks = vec![
            lines[..normal].to_vec(),
            lines[normal + 1..alternate].to_vec(),
            lines[alternate + 1..].to_vec(),
        ];
    }
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

pub(crate) struct PendingSnapshot {
    rx: Receiver<Reply>,
    cmd: String,
    pane: String,
}

impl PendingSnapshot {
    pub(crate) fn wait(self) -> Result<PaneSnapshot> {
        let response = ControlMode::await_blocks(self.rx, &self.cmd, COMMAND_TIMEOUT)?;
        parse_snapshot(response.blocks)
            .with_context(|| format!("unexpected answer to a snapshot of {}", self.pane))
    }
}

macro_rules! sizer_option {
    () => {
        "@thurbox_sizer"
    };
}

pub const SIZER_OPTION: &str = sizer_option!();

const SIZER_SUBSCRIPTION: &str = "thurbox-sizer";

pub const REMOTE_HOOK_STATE_OPTION: &str = "@thurbox_state";

pub const REMOTE_HOOK_SUBSCRIPTION: &str = "thurbox-status";

const HIDDEN_SUBSCRIPTION: &str = "thurbox-hidden";

const HIDDEN_FORMAT: &str = "#{window_activity}|\
     #{keypad_cursor_flag},#{keypad_flag},#{bracket_paste_flag},#{cursor_flag}|\
     #{mouse_standard_flag},#{mouse_button_flag},#{mouse_all_flag},#{mouse_any_flag},\
     #{mouse_sgr_flag},#{mouse_utf8_flag}|#{host_short}|#{pane_title}";

type HiddenPanes = Arc<Mutex<HashMap<String, Option<String>>>>;

pub(crate) fn hidden_state(value: &str) -> Option<(String, Vec<u8>)> {
    let mut fields = value.splitn(5, '|');
    let activity = fields.next()?.to_string();
    let modes = fields.next()?;
    let mouse = fields.next()?;
    let host = fields.next()?;
    let title = fields.next()?;
    let mut out = String::new();
    for (flag, (on, off)) in modes.split(',').zip([
        ("\x1b[?1h", "\x1b[?1l"),
        ("\x1b=", "\x1b>"),
        ("\x1b[?2004h", "\x1b[?2004l"),
        ("\x1b[?25h", "\x1b[?25l"),
    ]) {
        match flag.trim() {
            "1" => out.push_str(on),
            "0" => out.push_str(off),
            _ => {}
        }
    }
    // A terminal tracks one mouse mode at a time.
    out.push_str("\x1b[?1000l\x1b[?1002l\x1b[?1003l\x1b[?1005l\x1b[?1006l");
    let mut bytes = out.into_bytes();
    bytes.extend(mouse_seed_bytes(mouse));
    bytes.extend(title_seed_bytes(host, title));
    Some((activity, bytes))
}

pub(crate) const MAX_TITLE_SEED_BYTES: usize = 512;

pub(crate) fn mouse_seed_bytes(flags: &str) -> Vec<u8> {
    let on: Vec<bool> = flags.split(',').map(|f| f.trim() == "1").collect();
    let flag = |i: usize| on.get(i).copied().unwrap_or(false);
    let mut out = String::new();
    let tracking = [(2, "1003"), (1, "1002"), (0, "1000"), (3, "1000")]
        .into_iter()
        .find_map(|(i, mode)| flag(i).then_some(mode));
    if let Some(mode) = tracking {
        out.push_str(&format!("\x1b[?{mode}h"));
        if flag(4) {
            out.push_str("\x1b[?1006h");
        } else if flag(5) {
            out.push_str("\x1b[?1005h");
        }
    }
    out.into_bytes()
}

pub(crate) fn title_seed_bytes(host_short: &str, title: &str) -> Vec<u8> {
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

pub const SIZED_BY: &str = concat!("#{?#{==:#{session_attached},1},,#{", sizer_option!(), "}}");

pub type PaneSendersMap = HashMap<String, Vec<PaneSender>>;
pub type PaneSendersMapShared = Arc<Mutex<PaneSendersMap>>;

pub struct PaneSender {
    tx: SyncSender<PaneChunk>,
    resync: Option<Arc<Resync>>,
}

impl From<SyncSender<PaneChunk>> for PaneSender {
    fn from(tx: SyncSender<PaneChunk>) -> Self {
        Self { tx, resync: None }
    }
}

enum Sent {
    Kept,
    Gone,
}

pub(crate) struct Resync {
    pane: String,
    history: usize,
    command_list_single_reply: bool,
    outbox: Weak<Mutex<Outbox>>,
    response_queue: Weak<Mutex<VecDeque<Waiter>>>,
    behind: AtomicBool,
    state: Mutex<ResyncState>,
}

#[derive(Default)]
struct ResyncState {
    asked: bool,
    parked: Option<Box<PaneSnapshot>>,
}

impl Resync {
    fn is_behind(&self) -> bool {
        self.behind.load(Ordering::Acquire)
    }

    fn ask(&self, state: &mut ResyncState) {
        if state.asked {
            return;
        }
        let (Some(outbox), Some(queue)) = (self.outbox.upgrade(), self.response_queue.upgrade())
        else {
            return;
        };
        match ControlMode::enqueue_snapshot(
            &outbox,
            &queue,
            self.command_list_single_reply,
            &self.pane,
            self.history,
        ) {
            Ok(()) => state.asked = true,
            Err(e) => debug!(pane_id = %self.pane, "could not ask for a resync: {e:#}"),
        }
    }
}

impl PaneSender {
    fn send_output(&self, chunk: PaneChunk) -> Sent {
        let in_step = self.resync.as_ref().is_none_or(|r| !r.is_behind());
        let chunk = if in_step {
            match self.tx.try_send(chunk) {
                Ok(()) => return Sent::Kept,
                Err(std::sync::mpsc::TrySendError::Disconnected(_)) => return Sent::Gone,
                Err(std::sync::mpsc::TrySendError::Full(chunk)) => chunk,
            }
        } else {
            chunk
        };
        let Some(resync) = &self.resync else {
            debug!("Pane output channel full, dropping chunk");
            return Sent::Kept;
        };
        let Ok(mut state) = resync.state.lock() else {
            return Sent::Kept;
        };
        if !in_step && !resync.is_behind() {
            drop(state);
            return self.send_output(chunk);
        }
        if in_step {
            debug!(pane_id = %resync.pane, "Pane output channel full, dropping output until a snapshot catches it up");
        }
        resync.behind.store(true, Ordering::Release);
        state.parked = None;
        resync.ask(&mut state);
        Sent::Kept
    }

    fn send_event(&self, event: PaneChunk) -> Result<bool, ()> {
        if let Some(resync) = self.resync.as_ref().filter(|r| r.is_behind()) {
            if let Ok(mut state) = resync.state.lock() {
                if state.parked.take().is_some() {
                    resync.ask(&mut state);
                }
            }
        }
        match self.tx.try_send(event) {
            Ok(()) => Ok(true),
            Err(std::sync::mpsc::TrySendError::Full(_)) => Ok(false),
            Err(std::sync::mpsc::TrySendError::Disconnected(_)) => Err(()),
        }
    }

    fn send_snapshot(&self, snapshot: Option<&PaneSnapshot>) -> Sent {
        if let Some(resync) = self.resync.as_ref().filter(|r| r.is_behind()) {
            let Ok(mut state) = resync.state.lock() else {
                return Sent::Kept;
            };
            if resync.is_behind() {
                state.asked = false;
                let Some(snapshot) = snapshot else {
                    resync.behind.store(false, Ordering::Release);
                    return Sent::Kept;
                };
                let chunk = PaneChunk::Resync(Box::new(snapshot.clone()));
                return match self.tx.try_send(chunk) {
                    Ok(()) => {
                        state.parked = None;
                        resync.behind.store(false, Ordering::Release);
                        Sent::Kept
                    }
                    Err(std::sync::mpsc::TrySendError::Full(_)) => {
                        state.parked = Some(Box::new(snapshot.clone()));
                        Sent::Kept
                    }
                    Err(std::sync::mpsc::TrySendError::Disconnected(_)) => Sent::Gone,
                };
            }
        }
        let Some(snapshot) = snapshot else {
            return Sent::Kept;
        };
        match self
            .tx
            .try_send(PaneChunk::Snapshot(Box::new(snapshot.clone())))
        {
            Err(std::sync::mpsc::TrySendError::Disconnected(_)) => Sent::Gone,
            _ => Sent::Kept,
        }
    }
}

pub type PaneWindowsMap = HashMap<String, String>;
pub type PaneWindowsMapShared = Arc<Mutex<PaneWindowsMap>>;

pub type PaneSizesMap = HashMap<String, PaneSize>;
pub type PaneSizesMapShared = Arc<Mutex<PaneSizesMap>>;

pub struct CommandResponse {
    pub blocks: Vec<Vec<String>>,
    pub is_error: bool,
}

impl CommandResponse {
    fn lines(&self) -> Vec<String> {
        self.blocks.concat()
    }
}

struct Waiter {
    tx: SyncSender<Reply>,
    blocks: usize,
    seq: u64,
    splice: Option<String>,
}

type ResponseQueue = Arc<Mutex<VecDeque<Waiter>>>;

type Reply = std::result::Result<CommandResponse, std::io::Error>;

struct Line {
    seq: u64,
    text: String,
}

struct Outbox {
    lines: Option<std::sync::mpsc::Sender<Line>>,
    next: u64,
    written: Arc<Written>,
}

#[derive(Default)]
struct Written {
    state: Mutex<(u64, bool)>,
    moved: Condvar,
}

impl Written {
    fn advance(&self, to: u64, stopped: bool) {
        if let Ok(mut state) = self.state.lock() {
            state.0 = state.0.max(to);
            state.1 |= stopped;
        }
        self.moved.notify_all();
    }

    fn wait_for(&self, seq: u64) {
        let Ok(state) = self.state.lock() else {
            return;
        };
        drop(
            self.moved
                .wait_while(state, |(written, stopped)| *written < seq && !*stopped),
        );
    }
}

type SharedOutbox = Arc<Mutex<Outbox>>;

struct Answer {
    waiter: Waiter,
    blocks: Vec<Vec<String>>,
}

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
    WindowClose {
        window_id: String,
    },
    SubscriptionChanged {
        name: String,
        pane_id: String,
        value: String,
    },
    LayoutChange {
        window_id: String,
        rows: u16,
        cols: u16,
    },
    Other(String),
}

pub struct ControlModeReader {
    receiver: std::sync::mpsc::Receiver<PaneChunk>,
    size: PaneSize,
    resync: Option<Arc<Resync>>,
}

impl ControlModeReader {
    pub fn new(receiver: std::sync::mpsc::Receiver<PaneChunk>) -> Self {
        Self {
            receiver,
            size: PaneSize::default(),
            resync: None,
        }
    }

    fn recv_chunk(&self) -> Result<PaneChunk, std::sync::mpsc::RecvError> {
        let Some(resync) = &self.resync else {
            return self.receiver.recv();
        };
        match self.receiver.try_recv() {
            Ok(chunk) => return Ok(chunk),
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                return Err(std::sync::mpsc::RecvError)
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
        }
        match self.take_parked(resync) {
            Some(chunk) => chunk,
            None => self.receiver.recv(),
        }
    }

    fn take_parked(
        &self,
        resync: &Resync,
    ) -> Option<Result<PaneChunk, std::sync::mpsc::RecvError>> {
        let mut state = resync.state.lock().ok()?;
        match self.receiver.try_recv() {
            Ok(chunk) => return Some(Ok(chunk)),
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                return Some(Err(std::sync::mpsc::RecvError))
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
        }
        let snapshot = state.parked.take()?;
        resync.behind.store(false, Ordering::Release);
        Some(Ok(PaneChunk::Resync(snapshot)))
    }

    pub fn size(&self) -> PaneSize {
        self.size.clone()
    }
}

impl Iterator for ControlModeReader {
    type Item = PaneChunk;

    fn next(&mut self) -> Option<PaneChunk> {
        loop {
            match self.recv_chunk().ok()? {
                PaneChunk::Resized { rows, cols } => {
                    self.size.report(rows, cols);
                    return Some(PaneChunk::Resized { rows, cols });
                }
                PaneChunk::SizedElsewhere(elsewhere) => {
                    self.size.set_sized_elsewhere(elsewhere);
                }
                chunk => return Some(chunk),
            }
        }
    }
}

pub const SEND_KEYS_CHUNK_BYTES: usize = 512;

pub fn hex_send_keys_commands(pane_id: &str, buf: &[u8]) -> Vec<String> {
    buf.chunks(SEND_KEYS_CHUNK_BYTES)
        .map(|chunk| format_send_keys(pane_id, chunk))
        .collect()
}

const PASTE_START: &[u8] = b"\x1b[200~";
const PASTE_END: &[u8] = b"\x1b[201~";

pub fn bracketed_paste_text(buf: &[u8]) -> Option<&str> {
    let inner = buf.strip_prefix(PASTE_START)?.strip_suffix(PASTE_END)?;
    let has_marker = |m: &[u8]| inner.windows(m.len()).any(|w| w == m);
    if has_marker(PASTE_START) || has_marker(PASTE_END) {
        return None;
    }
    std::str::from_utf8(inner).ok()
}

pub trait PaneInput: Send + Sync {
    fn send_keys(&self, pane_id: &str, buf: &[u8]) -> Vec<String>;

    fn paste(&self, pane_id: &str, text: &str) -> Option<Result<()>>;

    fn pastes_out_of_band(&self) -> bool {
        false
    }
}

const SET_BUFFER_CHUNK_BYTES: usize = 384;

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

pub struct ControlModeWriter {
    outbox: SharedOutbox,
    replies: ResponseQueue,
    pub pane_id: String,
    pub input: Arc<dyn PaneInput>,
}

impl Write for ControlModeWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        let cmds = if buf.starts_with(PASTE_START) {
            let Some(text) = bracketed_paste_text(buf) else {
                warn!("dropped a paste that is not one bracketed frame");
                return Ok(buf.len());
            };
            if text.is_empty() {
                return Ok(buf.len());
            }
            if self.input.pastes_out_of_band() {
                self.wait_written();
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
        ControlMode::write_unanswered(&self.outbox, &self.replies, &cmds).map_err(|e| {
            e.downcast::<std::io::Error>()
                .unwrap_or_else(std::io::Error::other)
        })?;
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl ControlModeWriter {
    fn wait_written(&self) {
        let Ok(outbox) = self.outbox.lock() else {
            return;
        };
        let (seq, written) = (outbox.next, Arc::clone(&outbox.written));
        drop(outbox);
        written.wait_for(seq);
    }
}

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
    let rest = line.strip_prefix(b"%extended-output ")?;
    let split = rest.windows(3).position(|w| w == b" : ")?;
    let meta = &rest[..split];
    let pane_id = &meta[..meta.iter().position(|&b| b == b' ')?];
    Some(Notification::Output {
        pane_id: String::from_utf8_lossy(pane_id).into_owned(),
        data: decode_octal(&rest[split + 3..]),
    })
}

fn parse_window_close(line: &str) -> Option<Notification> {
    let rest = line
        .strip_prefix("%window-close ")
        .or_else(|| line.strip_prefix("%unlinked-window-close "))?;
    let window_id = rest.trim();
    // Some tmux versions put a layout after the `%window-close` id.
    let window_id = window_id.split_whitespace().next().unwrap_or(window_id);
    if window_id.is_empty() {
        return None;
    }
    Some(Notification::WindowClose {
        window_id: window_id.to_string(),
    })
}

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

fn parse_subscription_changed(line: &str) -> Option<Notification> {
    let rest = line.strip_prefix("%subscription-changed ")?;
    let mut tokens = rest.splitn(6, ' ');
    let name = tokens.next()?.to_string();
    for _ in 0..3 {
        tokens.next()?;
    }
    let pane_id = tokens.next()?.to_string();
    if !is_valid_pane_id(&pane_id) {
        return None;
    }
    let tail = tokens.next().unwrap_or("");
    let value = if let Some(v) = tail.strip_prefix(": ") {
        v.to_string()
    } else if tail == ":" {
        String::new()
    } else if let Some(idx) = tail.find(" : ") {
        tail[idx + 3..].to_string()
    } else if tail.ends_with(" :") {
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

pub fn is_valid_pane_id(s: &str) -> bool {
    s.strip_prefix('%')
        .is_some_and(|rest| !rest.is_empty() && rest.bytes().all(|b| b.is_ascii_digit()))
}

pub fn is_valid_window_id(s: &str) -> bool {
    s.strip_prefix('@')
        .is_some_and(|rest| !rest.is_empty() && rest.bytes().all(|b| b.is_ascii_digit()))
}

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

pub fn parse_pane_ids(body: &str) -> std::collections::HashSet<String> {
    body.lines()
        .map(str::trim)
        .filter(|id| is_valid_pane_id(id))
        .map(str::to_string)
        .collect()
}

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

pub fn format_send_keys(pane_id: &str, bytes: &[u8]) -> String {
    use std::fmt::Write;
    let mut cmd = String::with_capacity(20 + pane_id.len() + bytes.len() * 3 + 1);
    write!(cmd, "send-keys -t {pane_id} -H").unwrap();
    for &b in bytes {
        write!(cmd, " {b:02x}").unwrap();
    }
    cmd.push('\n');
    cmd
}

pub fn shell_escape(s: &str) -> String {
    crate::shell::posix_quote(&s.replace('\n', " "))
}

const COMMAND_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

const GRACEFUL_EXIT_POLLS: u32 = 10;
const GRACEFUL_EXIT_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(5);

const RETIRE_POLL: std::time::Duration = std::time::Duration::from_millis(20);

pub(crate) struct ControlMode {
    outbox: SharedOutbox,
    pub(crate) pane_senders: PaneSendersMapShared,
    pub(crate) pane_windows: PaneWindowsMapShared,
    pub(crate) pane_sizes: PaneSizesMapShared,
    response_queue: ResponseQueue,
    command_list_single_reply: bool,
    sub_events: Arc<Mutex<VecDeque<(String, String)>>>,
    subscriptions: bool,
    hidden: HiddenPanes,
    visibility: Mutex<()>,
    alive: Arc<AtomicBool>,
    reader_handle: Mutex<Option<JoinHandle<()>>>,
    child: Mutex<Child>,
    closed: AtomicBool,
}

const SUB_EVENTS_CAP: usize = 256;

const HOOK_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(1000);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ControlPolicy {
    pub flow_control_command: Option<&'static str>,
    pub implicit_attach_reply: bool,
    pub tagged_blocks: bool,
    pub command_list_single_reply: bool,
    pub subscriptions: bool,
    pub status_poll: Option<String>,
    pub confirm_close: Option<String>,
}

impl ControlMode {
    pub(crate) fn start(
        transport: &TmuxTransport,
        socket: &str,
        session: &str,
        sizer: &str,
        policy: &ControlPolicy,
    ) -> Result<Self> {
        // tmux -CC needs a TTY ("tcgetattr: Inappropriate ioctl"); -C works with piped stdin.
        let mut child = transport
            .tmux_command(socket, &["-C", "attach-session", "-t", session])?
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| {
                transport.launch_failure(
                    &format!("Failed to start {} control mode", transport.mux()),
                    e,
                )
            })?;

        let stdin = child
            .stdin
            .take()
            .context("Failed to get control mode stdin")?;
        let stdout = child
            .stdout
            .take()
            .context("Failed to get control mode stdout")?;

        // tmux answers the argv `attach-session` with one `%begin`/`%end` block: drain it before any waiter exists.
        let mut reader = BufReader::new(stdout);
        if policy.implicit_attach_reply {
            Self::drain_implicit_attach_response(&mut reader)?;
        }

        let response_queue: ResponseQueue = Arc::new(Mutex::new(VecDeque::new()));
        let outbox = Self::spawn_writer(stdin, &response_queue)?;
        let pane_senders: PaneSendersMapShared =
            Arc::new(Mutex::new(std::collections::HashMap::new()));
        let pane_windows: PaneWindowsMapShared =
            Arc::new(Mutex::new(std::collections::HashMap::new()));
        let pane_sizes: PaneSizesMapShared = Arc::default();
        let sub_events: Arc<Mutex<VecDeque<(String, String)>>> =
            Arc::new(Mutex::new(VecDeque::new()));
        let alive = Arc::new(AtomicBool::new(true));
        let hidden: HiddenPanes = Arc::default();

        let reader_outbox = Arc::clone(&outbox);
        let reader_pane_senders = Arc::clone(&pane_senders);
        let reader_sizer = sizer.to_string();
        let reader_pane_windows = Arc::clone(&pane_windows);
        let reader_pane_sizes = Arc::clone(&pane_sizes);
        let reader_queue = Arc::clone(&response_queue);
        let reader_sub_events = Arc::clone(&sub_events);
        let reader_alive = Arc::clone(&alive);
        let reader_hidden = Arc::clone(&hidden);

        let strict_blocks = policy.tagged_blocks;
        let confirm_close = policy.confirm_close.clone();
        let reader_handle = std::thread::Builder::new()
            .name("tmux-control-reader".into())
            .spawn(move || {
                Self::reader_thread(
                    reader,
                    reader_outbox,
                    reader_pane_senders,
                    reader_pane_windows,
                    reader_pane_sizes,
                    reader_queue,
                    reader_sub_events,
                    reader_hidden,
                    strict_blocks,
                    confirm_close,
                    &reader_sizer,
                );
                reader_alive.store(false, Ordering::Relaxed);
            })
            .context("Failed to spawn control reader thread")?;

        let control = Self {
            outbox,
            pane_senders,
            pane_windows,
            pane_sizes,
            response_queue,
            command_list_single_reply: policy.command_list_single_reply,
            sub_events,
            subscriptions: policy.subscriptions,
            hidden,
            visibility: Mutex::new(()),
            alive,
            reader_handle: Mutex::new(Some(reader_handle)),
            child: Mutex::new(child),
            closed: AtomicBool::new(false),
        };

        if let Some(command) = policy.flow_control_command {
            control.send_command(command)?;
        }

        // `%*` is session-scoped in tmux, so one subscription covers panes created later.
        if policy.subscriptions {
            let arm = format!(
                "refresh-client -B '{}:%*:#{{{}}}'",
                REMOTE_HOOK_SUBSCRIPTION, REMOTE_HOOK_STATE_OPTION,
            );
            if let Err(e) = control.send_command(&arm) {
                warn!("failed to arm the remote-hook status subscription: {e:#}");
            }
            let arm = format!("refresh-client -B '{SIZER_SUBSCRIPTION}:%*:{SIZED_BY}'");
            if let Err(e) = control.send_command(&arm) {
                warn!("failed to arm the pane sizer subscription: {e:#}");
            }
        } else if let Some(command) = &policy.status_poll {
            control.spawn_hook_poller(command.clone());
        }

        Ok(control)
    }

    fn spawn_hook_poller(&self, cmd: String) {
        let outbox = Arc::clone(&self.outbox);
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
                    let body = match Self::send_command_on(&outbox, &queue, &cmd, 1) {
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

    fn next_control_line(reader: &mut impl BufRead, line_buf: &mut Vec<u8>) -> Option<String> {
        Self::read_control_line(reader, line_buf)
            .then(|| String::from_utf8_lossy(line_buf).into_owned())
    }

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

    #[allow(clippy::too_many_arguments)]
    fn reader_thread(
        mut reader: BufReader<std::process::ChildStdout>,
        outbox: SharedOutbox,
        pane_senders: PaneSendersMapShared,
        pane_windows: PaneWindowsMapShared,
        pane_sizes: PaneSizesMapShared,
        response_queue: ResponseQueue,
        sub_events: Arc<Mutex<VecDeque<(String, String)>>>,
        hidden: HiddenPanes,
        strict_blocks: bool,
        confirm_close: Option<String>,
        sizer: &str,
    ) {
        let mut collecting: Option<(Option<String>, Vec<String>)> = None;
        let mut answering: Option<Answer> = None;
        let mut line_buf = Vec::new();

        while Self::read_control_line(&mut reader, &mut line_buf) {
            if !matches!(collecting, Some((Some(_), _))) {
                if let Some(Notification::Output { pane_id, data }) = parse_output(&line_buf) {
                    Self::dispatch_output(&pane_senders, &pane_id, data);
                    continue;
                }
            }
            let line = String::from_utf8_lossy(&line_buf).into_owned();
            // tmux writes a command's output in one piece, never with a notification inside it.
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
                    Self::on_pause(&outbox, &response_queue, &hidden, &pane_id);
                }
                Notification::WindowClose { window_id } => match &confirm_close {
                    None => {
                        Self::close_window_panes(&pane_senders, &pane_windows, &window_id);
                        Self::forget_sizes(&pane_sizes, &pane_windows);
                    }
                    Some(listing) => Self::spawn_close_check(
                        &outbox,
                        &response_queue,
                        &pane_senders,
                        &pane_windows,
                        &pane_sizes,
                        listing,
                        &window_id,
                    ),
                },
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
                Notification::SubscriptionChanged {
                    name,
                    pane_id,
                    value,
                } => {
                    Self::on_subscription(
                        &pane_senders,
                        &sub_events,
                        &hidden,
                        sizer,
                        &name,
                        pane_id,
                        value,
                    );
                }
                Notification::Other(text) => {
                    if let Some((_, lines)) = &mut collecting {
                        lines.push(text);
                    }
                }
            }
        }

        debug!("Control reader thread exiting");
        Self::drop_panes(&pane_senders, &pane_windows, &pane_sizes);
    }

    fn drop_panes(
        pane_senders: &PaneSendersMapShared,
        pane_windows: &PaneWindowsMapShared,
        pane_sizes: &PaneSizesMapShared,
    ) {
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

    fn close_window_panes(
        pane_senders: &PaneSendersMapShared,
        pane_windows: &PaneWindowsMapShared,
        window_id: &str,
    ) {
        let gone: Vec<String> = match pane_windows.lock() {
            Ok(windows) => windows
                .iter()
                .filter(|(_, window)| window.as_str() == window_id)
                .map(|(pane, _)| pane.clone())
                .collect(),
            Err(_) => return,
        };
        if gone.is_empty() {
            return;
        }
        Self::close_panes(pane_senders, pane_windows, &gone);
        debug!(window_id, panes = ?gone, "window closed, its pane readers get EOF");
    }

    fn close_panes(
        pane_senders: &PaneSendersMapShared,
        pane_windows: &PaneWindowsMapShared,
        panes: &[String],
    ) {
        if let Ok(mut windows) = pane_windows.lock() {
            for pane in panes {
                windows.remove(pane);
            }
        }
        if let Ok(mut senders) = pane_senders.lock() {
            for pane in panes {
                senders.remove(pane);
            }
        }
    }

    fn forget_sizes(pane_sizes: &PaneSizesMapShared, pane_windows: &PaneWindowsMapShared) {
        if let Ok(mut sizes) = pane_sizes.lock() {
            let windows = pane_windows.lock().ok();
            sizes.retain(|pane, _| windows.as_ref().is_some_and(|w| w.contains_key(pane)));
        }
    }

    fn spawn_close_check(
        outbox: &SharedOutbox,
        response_queue: &ResponseQueue,
        pane_senders: &PaneSendersMapShared,
        pane_windows: &PaneWindowsMapShared,
        pane_sizes: &PaneSizesMapShared,
        listing: &str,
        window_id: &str,
    ) {
        let registered: Vec<String> = match pane_senders.lock() {
            Ok(senders) => senders.keys().cloned().collect(),
            Err(_) => return,
        };
        if registered.is_empty() {
            return;
        }
        let outbox = Arc::clone(outbox);
        let queue = Arc::clone(response_queue);
        let senders = Arc::clone(pane_senders);
        let windows = Arc::clone(pane_windows);
        let sizes = Arc::clone(pane_sizes);
        let listing = listing.to_string();
        let window_id = window_id.to_string();
        let spawned = std::thread::Builder::new()
            .name("window-close-check".into())
            .spawn(move || {
                let live = match Self::send_command_on(&outbox, &queue, &listing, 1) {
                    Ok(live) => live,
                    Err(e) => {
                        debug!(window_id, "could not confirm a window close: {e:#}");
                        return;
                    }
                };
                let gone = Self::confirm_closes(&registered, &live);
                if gone.is_empty() {
                    debug!(window_id, "window close named no pane that went; ignored");
                    return;
                }
                Self::close_panes(&senders, &windows, &gone);
                Self::forget_sizes(&sizes, &windows);
                debug!(window_id, panes = ?gone, "panes gone, their readers get EOF");
            });
        if let Err(e) = spawned {
            warn!("failed to spawn a window-close check: {e}");
        }
    }

    fn confirm_closes(registered: &[String], listing: &str) -> Vec<String> {
        let live = parse_pane_ids(listing);
        if live.is_empty() {
            return Vec::new();
        }
        registered
            .iter()
            .filter(|pane| !live.contains(pane.as_str()))
            .cloned()
            .collect()
    }

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

    fn send_event(pane_senders: &PaneSendersMapShared, pane_id: &str, event: PaneChunk) -> bool {
        let Ok(mut senders) = pane_senders.lock() else {
            return false;
        };
        let Some(tx_vec) = senders.get_mut(pane_id) else {
            return false;
        };
        let mut all = true;
        let mut gone = Vec::new();
        for (i, tx) in tx_vec.iter().enumerate() {
            match tx.send_event(event.clone()) {
                Ok(true) => {}
                Ok(false) => {
                    debug!(pane_id = %pane_id, "Pane channel full, dropping an event");
                    all = false;
                }
                Err(()) => gone.push(i),
            }
        }
        if !gone.is_empty() {
            all = false;
            Self::prune_senders(&mut senders, pane_id, &gone);
        }
        all
    }

    fn prune_senders(senders: &mut PaneSendersMap, pane_id: &str, gone: &[usize]) {
        let Some(tx_vec) = senders.get_mut(pane_id) else {
            return;
        };
        for &i in gone.iter().rev() {
            tx_vec.remove(i);
        }
        if tx_vec.is_empty() {
            senders.remove(pane_id);
        }
        debug!(pane_id = %pane_id, dropped = gone.len(), "unregistered readers that went away");
    }

    fn dispatch_output(pane_senders: &PaneSendersMapShared, pane_id: &str, data: Vec<u8>) {
        let Ok(mut senders) = pane_senders.lock() else {
            return;
        };
        let Some(tx_vec) = senders.get(pane_id) else {
            return;
        };
        let mut data = Some(data);
        let mut gone = Vec::new();
        for (i, tx) in tx_vec.iter().enumerate() {
            let this = if i + 1 == tx_vec.len() {
                data.take()
            } else {
                data.clone()
            };
            let Some(this) = this else {
                break;
            };
            if let Sent::Gone = tx.send_output(PaneChunk::Output(this)) {
                gone.push(i);
            }
        }
        if !gone.is_empty() {
            Self::prune_senders(&mut senders, pane_id, &gone);
        }
    }

    fn dispatch_snapshot(
        pane_senders: &PaneSendersMapShared,
        pane_id: &str,
        snapshot: Option<&PaneSnapshot>,
    ) {
        let Ok(mut senders) = pane_senders.lock() else {
            return;
        };
        let Some(tx_vec) = senders.get(pane_id) else {
            return;
        };
        let gone: Vec<usize> = tx_vec
            .iter()
            .enumerate()
            .filter(|(_, tx)| matches!(tx.send_snapshot(snapshot), Sent::Gone))
            .map(|(i, _)| i)
            .collect();
        if !gone.is_empty() {
            Self::prune_senders(&mut senders, pane_id, &gone);
        }
    }

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
        if let Some(pane) = &answer.waiter.splice {
            let snapshot = (!is_error)
                .then(|| parse_snapshot(answer.blocks.clone()))
                .flatten();
            Self::dispatch_snapshot(pane_senders, pane, snapshot.as_ref());
        }
        let _ = answer.waiter.tx.send(Ok(CommandResponse {
            blocks: answer.blocks,
            is_error,
        }));
    }

    pub(crate) fn take_sub_events(&self) -> Vec<(String, String)> {
        self.sub_events
            .lock()
            .map(|mut events| events.drain(..).collect())
            .unwrap_or_default()
    }

    fn resume_pane(outbox: &SharedOutbox, replies: &ResponseQueue, pane_id: &str) {
        if !is_valid_pane_id(pane_id) {
            warn!("not resuming output of a paused pane named {pane_id:?}");
            return;
        }
        let cmd = format!("refresh-client -A {pane_id}:continue");
        let _ = Self::write_unanswered(outbox, replies, &[cmd]);
    }

    fn on_pause(
        outbox: &SharedOutbox,
        replies: &ResponseQueue,
        hidden: &HiddenPanes,
        pane_id: &str,
    ) {
        if !hidden.lock().is_ok_and(|h| h.contains_key(pane_id)) {
            Self::resume_pane(outbox, replies, pane_id);
        }
    }

    fn on_subscription(
        pane_senders: &PaneSendersMapShared,
        sub_events: &Arc<Mutex<VecDeque<(String, String)>>>,
        hidden: &HiddenPanes,
        sizer: &str,
        name: &str,
        pane_id: String,
        value: String,
    ) {
        if name == REMOTE_HOOK_SUBSCRIPTION && !value.is_empty() {
            Self::queue_sub_events(sub_events, vec![(pane_id, value)]);
        } else if name.starts_with(HIDDEN_SUBSCRIPTION) {
            Self::dispatch_hidden(pane_senders, hidden, &pane_id, &value);
        } else if name == SIZER_SUBSCRIPTION {
            let elsewhere = !value.is_empty() && value != sizer;
            Self::send_event(pane_senders, &pane_id, PaneChunk::SizedElsewhere(elsewhere));
        }
    }

    fn dispatch_hidden(
        pane_senders: &PaneSendersMapShared,
        hidden: &HiddenPanes,
        pane_id: &str,
        value: &str,
    ) {
        let Some((activity, state)) = hidden_state(value) else {
            return;
        };
        let moved = {
            let Ok(mut hidden) = hidden.lock() else {
                return;
            };
            let Some(last) = hidden.get_mut(pane_id) else {
                return;
            };
            let moved = last.as_ref().is_some_and(|last| *last != activity);
            *last = Some(activity);
            moved
        };
        let chunk = if moved {
            PaneChunk::Output(state)
        } else {
            PaneChunk::State(state)
        };
        Self::send_event(pane_senders, pane_id, chunk);
    }

    pub(crate) fn hide_pane(&self, pane_id: &str) -> Result<bool> {
        if !self.subscriptions || !is_valid_pane_id(pane_id) {
            return Ok(false);
        }
        let _visibility = self
            .visibility
            .lock()
            .map_err(|e| anyhow::anyhow!("visibility lock: {e}"))?;
        let readers = self
            .pane_senders
            .lock()
            .map_err(|e| anyhow::anyhow!("pane_senders lock: {e}"))?
            .get(pane_id)
            .map_or(0, Vec::len);
        if readers != 1 {
            return Ok(false);
        }
        {
            let mut hidden = self
                .hidden
                .lock()
                .map_err(|e| anyhow::anyhow!("hidden lock: {e}"))?;
            if hidden.insert(pane_id.to_string(), None).is_some() {
                return Ok(true);
            }
        }
        let subscribe =
            format!("refresh-client -B '{HIDDEN_SUBSCRIPTION}{pane_id}:{pane_id}:{HIDDEN_FORMAT}'");
        let pause = format!("refresh-client -A '{pane_id}:pause'");
        self.send_command_detached(&[&subscribe, &pause], 2)?;
        Ok(true)
    }

    pub(crate) fn show_pane(&self, pane_id: &str) -> Result<()> {
        let _visibility = self
            .visibility
            .lock()
            .map_err(|e| anyhow::anyhow!("visibility lock: {e}"))?;
        if !self.unhide(pane_id)? {
            return Ok(());
        }
        let unsubscribe = format!("refresh-client -B '{HIDDEN_SUBSCRIPTION}{pane_id}'");
        let resume = format!("refresh-client -A '{pane_id}:continue'");
        self.send_command_detached(&[&unsubscribe, &resume], 2)
    }

    fn unhide(&self, pane_id: &str) -> Result<bool> {
        Ok(self
            .hidden
            .lock()
            .map_err(|e| anyhow::anyhow!("hidden lock: {e}"))?
            .remove(pane_id)
            .is_some())
    }

    pub(crate) fn send_command(&self, cmd: &str) -> Result<String> {
        Self::send_command_on(&self.outbox, &self.response_queue, cmd, 1)
    }

    pub(crate) fn send_command_list(&self, cmds: &[&str], blocks: usize) -> Result<String> {
        if cmds.is_empty() {
            bail!("an empty command list has nothing to send");
        }
        Self::send_command_on(
            &self.outbox,
            &self.response_queue,
            &cmds.join(" ; "),
            blocks,
        )
    }

    fn send_command_on(
        outbox: &SharedOutbox,
        response_queue: &ResponseQueue,
        cmd: &str,
        blocks: usize,
    ) -> Result<String> {
        let rx = Self::enqueue_command_on(outbox, response_queue, cmd, blocks, None)?;
        Self::await_response(rx, cmd, COMMAND_TIMEOUT)
    }

    fn enqueue_command_on(
        outbox: &SharedOutbox,
        response_queue: &ResponseQueue,
        cmd: &str,
        blocks: usize,
        splice: Option<String>,
    ) -> Result<Receiver<Reply>> {
        let mut outbox = outbox
            .lock()
            .map_err(|e| anyhow::anyhow!("outbox lock: {e}"))?;
        Self::enqueue_locked(&mut outbox, response_queue, cmd, blocks, splice)
    }

    fn enqueue_locked(
        outbox: &mut Outbox,
        response_queue: &ResponseQueue,
        cmd: &str,
        blocks: usize,
        splice: Option<String>,
    ) -> Result<Receiver<Reply>> {
        let gone =
            || std::io::Error::new(std::io::ErrorKind::BrokenPipe, "control connection closed");
        let Some(lines) = &outbox.lines else {
            return Err(gone().into());
        };
        let seq = outbox.next;
        let (tx, rx) = sync_channel(1);
        response_queue
            .lock()
            .map_err(|e| anyhow::anyhow!("response_queue lock: {e}"))?
            .push_back(Waiter {
                tx,
                blocks,
                seq,
                splice,
            });
        let line = Line {
            seq,
            text: format!("{cmd}\n"),
        };
        if lines.send(line).is_err() {
            if let Ok(mut queue) = response_queue.lock() {
                queue.pop_back();
            }
            outbox.lines = None;
            return Err(gone().into());
        }
        outbox.next += 1;
        Ok(rx)
    }

    fn write_unanswered(
        outbox: &SharedOutbox,
        response_queue: &ResponseQueue,
        cmds: &[String],
    ) -> Result<()> {
        let mut outbox = outbox
            .lock()
            .map_err(|e| anyhow::anyhow!("outbox lock: {e}"))?;
        for cmd in cmds {
            Self::enqueue_locked(
                &mut outbox,
                response_queue,
                cmd.trim_end_matches('\n'),
                1,
                None,
            )?;
        }
        Ok(())
    }

    fn spawn_writer(stdin: ChildStdin, replies: &ResponseQueue) -> Result<SharedOutbox> {
        let (tx, rx) = std::sync::mpsc::channel();
        let written = Arc::new(Written::default());
        let outbox = Arc::new(Mutex::new(Outbox {
            lines: Some(tx),
            next: 0,
            written: Arc::clone(&written),
        }));
        let weak = Arc::downgrade(&outbox);
        let replies = Arc::clone(replies);
        std::thread::Builder::new()
            .name("tmux-control-writer".into())
            .spawn(move || {
                Self::writer_thread(stdin, &rx, &weak, &replies, &written);
                written.advance(0, true);
            })
            .context("Failed to spawn control writer thread")?;
        Ok(outbox)
    }

    fn writer_thread(
        mut stdin: ChildStdin,
        lines: &Receiver<Line>,
        outbox: &Weak<Mutex<Outbox>>,
        replies: &ResponseQueue,
        written: &Written,
    ) {
        while let Ok(line) = lines.recv() {
            let sent = stdin
                .write_all(line.text.as_bytes())
                .and_then(|()| stdin.flush());
            if let Err(e) = sent {
                debug!("control writer stopping: {e}");
                Self::fail_unwritten(outbox, replies, line.seq, &e);
                return;
            }
            written.advance(line.seq + 1, false);
        }
    }

    fn fail_unwritten(
        outbox: &Weak<Mutex<Outbox>>,
        replies: &ResponseQueue,
        seq: u64,
        e: &std::io::Error,
    ) {
        let outbox = outbox.upgrade();
        let mut held = outbox.as_ref().and_then(|outbox| outbox.lock().ok());
        if let Some(outbox) = held.as_mut() {
            outbox.lines = None;
        }
        let unwritten = replies
            .lock()
            .map(|mut queue| {
                let at = queue
                    .iter()
                    .position(|waiter| waiter.seq >= seq)
                    .unwrap_or(queue.len());
                queue.split_off(at)
            })
            .unwrap_or_default();
        drop(held);
        for waiter in unwritten {
            let _ = waiter
                .tx
                .send(Err(std::io::Error::new(e.kind(), e.to_string())));
        }
    }

    pub(crate) fn writer(&self, pane_id: String, input: Arc<dyn PaneInput>) -> ControlModeWriter {
        ControlModeWriter {
            outbox: Arc::clone(&self.outbox),
            replies: Arc::clone(&self.response_queue),
            pane_id,
            input,
        }
    }

    fn await_response(
        rx: Receiver<Reply>,
        cmd: &str,
        budget: std::time::Duration,
    ) -> Result<String> {
        let response = Self::await_blocks(rx, cmd, budget)?;
        Ok(response.lines().join("\n"))
    }

    fn await_blocks(
        rx: Receiver<Reply>,
        cmd: &str,
        budget: std::time::Duration,
    ) -> Result<CommandResponse> {
        let response = rx
            .recv_timeout(budget)
            .with_context(|| format!("Timeout waiting for response to: {cmd}"))?
            .with_context(|| format!("Failed to send: {cmd}"))?;

        if response.is_error {
            let reply = response.lines().join("\n");
            return Err(Refusal {
                command: cmd.to_string(),
                reply,
            }
            .into());
        }

        Ok(response)
    }

    pub(crate) fn send_command_within(
        &self,
        cmd: &str,
        budget: std::time::Duration,
    ) -> Result<String> {
        let rx = Self::enqueue_command_on(&self.outbox, &self.response_queue, cmd, 1, None)?;
        Self::await_response(rx, cmd, budget)
    }

    pub(crate) fn request_snapshot(&self, pane_id: &str, history: usize) -> Result<()> {
        Self::enqueue_snapshot(
            &self.outbox,
            &self.response_queue,
            self.command_list_single_reply,
            pane_id,
            history,
        )
    }

    fn enqueue_snapshot(
        outbox: &SharedOutbox,
        response_queue: &ResponseQueue,
        command_list_single_reply: bool,
        pane_id: &str,
        history: usize,
    ) -> Result<()> {
        let cmds = if command_list_single_reply {
            snapshot_commands_one_block(pane_id, history, true)
        } else {
            snapshot_commands(pane_id, history, true)
        };
        Self::enqueue_command_on(
            outbox,
            response_queue,
            &cmds.join(" ; "),
            if command_list_single_reply {
                1
            } else {
                cmds.len()
            },
            Some(pane_id.to_string()),
        )
        .map(drop)
    }

    pub(crate) fn pane_channel(
        &self,
        pane_id: &str,
        resync_history: Option<usize>,
    ) -> (PaneSender, ControlModeReader) {
        let (tx, rx) = sync_channel(PANE_CHANNEL_CAPACITY);
        let mut reader = ControlModeReader::new(rx);
        let resync = resync_history.map(|history| {
            Arc::new(Resync {
                pane: pane_id.to_string(),
                history,
                command_list_single_reply: self.command_list_single_reply,
                outbox: Arc::downgrade(&self.outbox),
                response_queue: Arc::downgrade(&self.response_queue),
                behind: AtomicBool::new(false),
                state: Mutex::default(),
            })
        });
        reader.resync = resync.clone();
        (PaneSender { tx, resync }, reader)
    }

    pub(crate) fn ask_snapshot(&self, pane_id: &str, history: usize) -> Result<PendingSnapshot> {
        let cmds = if self.command_list_single_reply {
            snapshot_commands_one_block(pane_id, history, false)
        } else {
            snapshot_commands(pane_id, history, false)
        };
        let cmd = cmds.join(" ; ");
        let blocks = if self.command_list_single_reply {
            1
        } else {
            cmds.len()
        };
        let rx = Self::enqueue_command_on(&self.outbox, &self.response_queue, &cmd, blocks, None)?;
        Ok(PendingSnapshot {
            rx,
            cmd,
            pane: pane_id.to_string(),
        })
    }

    pub(crate) fn send_command_detached(&self, cmds: &[&str], blocks: usize) -> Result<()> {
        if cmds.is_empty() {
            bail!("an empty command list has nothing to send");
        }
        Self::enqueue_command_on(
            &self.outbox,
            &self.response_queue,
            &cmds.join(" ; "),
            blocks,
            None,
        )
        .map(drop)
    }
}

impl Drop for ControlMode {
    fn drop(&mut self) {
        self.close();
    }
}

impl ControlMode {
    pub(crate) fn retire(self: Arc<Self>) {
        Self::drop_panes(&self.pane_senders, &self.pane_windows, &self.pane_sizes);
        let spawned = std::thread::Builder::new()
            .name("control-retire".into())
            .spawn({
                let this = Arc::clone(&self);
                move || {
                    let deadline = std::time::Instant::now() + COMMAND_TIMEOUT;
                    while Arc::strong_count(&this) > 1 && std::time::Instant::now() < deadline {
                        std::thread::sleep(RETIRE_POLL);
                    }
                    this.close();
                }
            });
        if spawned.is_err() {
            self.close();
        }
    }

    pub(crate) fn close(&self) {
        if self.closed.swap(true, Ordering::AcqRel) {
            return;
        }
        self.alive.store(false, Ordering::Relaxed);

        if let Ok(mut outbox) = self.outbox.lock() {
            if let Some(lines) = outbox.lines.take() {
                let _ = lines.send(Line {
                    seq: outbox.next,
                    text: "detach-client\n".to_string(),
                });
            }
        }

        // A `try_wait` right after `detach-client` never succeeds: tmux has not processed it yet.
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
        if let Ok(mut queue) = self.response_queue.lock() {
            queue.clear();
        }

        if let Ok(mut handle) = self.reader_handle.lock() {
            if let Some(h) = handle.take() {
                let _ = h.join();
            }
        }
    }
}

pub(crate) fn is_broken_pipe(err: &anyhow::Error) -> bool {
    err.chain().any(|cause| {
        cause
            .downcast_ref::<std::io::Error>()
            .is_some_and(|e| e.kind() == std::io::ErrorKind::BrokenPipe)
    })
}

pub(crate) fn is_recv_timeout(err: &anyhow::Error) -> bool {
    err.chain().any(|cause| {
        cause
            .downcast_ref::<std::sync::mpsc::RecvTimeoutError>()
            .is_some()
    })
}

#[cfg(test)]
mod tests;
