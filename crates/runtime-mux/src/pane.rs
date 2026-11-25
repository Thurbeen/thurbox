use std::collections::HashMap;
use std::io::Write;
use std::path::Path;
use std::sync::{
    atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering},
    Arc, Mutex,
};
use std::time::SystemTime;

use anyhow::Result;
use tokio::sync::mpsc;
use tracing::{debug, error};

use crate::contract::{
    pack_size, unpack_size, AdoptedSession, PaneChunk, PaneOutput, PaneSize, PaneSnapshot,
    SessionBackend, SpawnedSession,
};
use crate::osc8;
use crate::WindowRole;
use crate::{HyperlinkTable, SessionId};

pub(crate) fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn vt_floor(rows: u16, cols: u16) -> (u16, u16) {
    (rows.max(2), cols.max(2))
}

fn utf8_ready_prefix_len(buf: &[u8]) -> usize {
    let len = buf.len();
    // A truncated tail is at most 3 bytes, so only the last 3 can matter.
    let start = len.saturating_sub(3);
    let mut i = len;
    while i > start {
        i -= 1;
        let b = buf[i];
        // Anything that is not a UTF-8 continuation byte (0x80..=0xbf) starts a
        // character — ASCII or a multi-byte lead.
        if !(0x80..=0xbf).contains(&b) {
            // A lead byte (or ASCII). Determine the sequence's expected length;
            // if not all of it is present yet, cut before it.
            let expected = match b {
                0x00..=0x7f => 1,
                0xc0..=0xdf => 2,
                0xe0..=0xef => 3,
                _ => 4,
            };
            return if len - i < expected { i } else { len };
        }
        // Continuation byte (0x80..=0xbf): keep scanning back for its lead.
    }
    // No lead byte within the last 3 bytes — malformed tail; don't hold it back.
    len
}

#[derive(Clone, Default)]
pub struct TermSignals {
    title: Arc<Mutex<Option<String>>>,
    attention_at: Arc<AtomicU64>,
    notification: Arc<Mutex<Option<String>>>,
    meta_gen: Arc<AtomicU64>,
    hyperlinks: HyperlinkTable,
    pending_link: Option<osc8::PendingHyperlink>,
    app_copy: Arc<AppCopy>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppCopyRequest {
    pub target: Vec<u8>,
    pub data: Vec<u8>,
}

const PENDING_APP_COPIES: usize = 8;

#[derive(Debug, Default)]
pub struct AppCopy {
    focus: AtomicU64,
    pending: Mutex<std::collections::VecDeque<(u64, AppCopyRequest)>>,
}

impl AppCopy {
    pub fn focus(&self, token: u64) {
        if let Ok(mut pending) = self.pending.lock() {
            pending.clear();
        }
        self.focus.store(token, Ordering::Release);
    }

    pub fn blur(&self) {
        self.focus.store(0, Ordering::Release);
        if let Ok(mut pending) = self.pending.lock() {
            pending.clear();
        }
    }

    pub fn take(&self) -> Vec<AppCopyRequest> {
        let held = self.focus.load(Ordering::Acquire);
        let Ok(mut pending) = self.pending.lock() else {
            return Vec::new();
        };
        pending
            .drain(..)
            .filter(|(token, _)| *token != 0 && *token == held)
            .map(|(_, request)| request)
            .collect()
    }

    fn offer(&self, target: &[u8], data: &[u8]) {
        let token = self.focus.load(Ordering::Acquire);
        if token == 0 {
            return;
        }
        if let Ok(mut pending) = self.pending.lock() {
            if pending.len() == PENDING_APP_COPIES {
                pending.pop_front();
            }
            pending.push_back((
                token,
                AppCopyRequest {
                    target: target.to_vec(),
                    data: data.to_vec(),
                },
            ));
        }
    }
}

impl TermSignals {
    fn store_title(&self, raw: &[u8]) {
        let s = String::from_utf8_lossy(raw).trim().to_string();
        if let Ok(mut guard) = self.title.lock() {
            *guard = (!s.is_empty()).then_some(s);
        }
        // After the write, so a reader that observes the new generation also
        // observes the new value.
        self.meta_gen.fetch_add(1, Ordering::Release);
    }

    pub fn hyperlinks(&self) -> &HyperlinkTable {
        &self.hyperlinks
    }

    fn signal_attention(&self, message: Option<String>) {
        self.attention_at.store(now_millis(), Ordering::Relaxed);
        if let Some(msg) = message {
            let msg = msg.trim().to_string();
            if let Ok(mut guard) = self.notification.lock() {
                *guard = (!msg.is_empty()).then_some(msg);
            }
            self.meta_gen.fetch_add(1, Ordering::Release);
        }
    }
}

impl vt100::Callbacks for TermSignals {
    fn set_window_title(&mut self, _: &mut vt100::Screen, title: &[u8]) {
        self.store_title(title);
    }

    fn set_window_icon_name(&mut self, _: &mut vt100::Screen, icon_name: &[u8]) {
        // Some CLIs emit only OSC `1` (icon name); treat it as the title too.
        self.store_title(icon_name);
    }

    fn audible_bell(&mut self, _: &mut vt100::Screen) {
        // BEL: the cross-agent "done / needs you" signal (e.g. Claude's
        // `preferredNotifChannel terminal_bell`). No message text.
        self.signal_attention(None);
    }

    fn unhandled_osc(&mut self, screen: &mut vt100::Screen, params: &[&[u8]]) {
        // Desktop-notification escapes carry the agent's status message.
        //   OSC 9 ; <message>
        //   OSC 777 ; notify ; <title> ; <body>
        // A hyperlink pair brackets its label's glyphs.
        //   OSC 8 ; <params> ; <uri>   …label…   OSC 8 ; ;
        match params {
            [b"9", msg] => self.signal_attention(Some(String::from_utf8_lossy(msg).into_owned())),
            [b"777", kind, rest @ ..] if kind.eq_ignore_ascii_case(b"notify") => {
                let msg = rest
                    .iter()
                    .map(|p| String::from_utf8_lossy(p))
                    .collect::<Vec<_>>()
                    .join(": ");
                self.signal_attention(Some(msg));
            }
            [b"8", fields @ ..] => {
                let uri = osc8::uri_from_fields(fields);
                osc8::handle(screen, &mut self.pending_link, &mut self.hyperlinks, &uri);
            }
            _ => {}
        }
    }

    fn copy_to_clipboard(&mut self, _: &mut vt100::Screen, ty: &[u8], data: &[u8]) {
        self.app_copy.offer(ty, data);
    }

    // `paste_from_clipboard` keeps vt100's no-op: an app is never told what is
    // on the clipboard, and tmux is kept from answering either
    // (`set-clipboard external`, `Tmux::session_config` in `tmux`).
}

pub type SessionParser = vt100::Parser<TermSignals>;

struct SessionIo {
    output: PaneOutput,
    input: Box<dyn Write + Send>,
    backend_id: String,
    mode: WireMode,
    seed_len: usize,
    resident: bool,
    size: Option<PaneSize>,
}

impl SessionIo {
    fn spawned(spawned: SpawnedSession) -> Self {
        Self {
            output: spawned.output,
            input: spawned.input,
            backend_id: spawned.backend_id,
            mode: WireMode::Spawn,
            seed_len: 0,
            resident: true,
            size: spawned.size,
        }
    }

    fn adopted(adopted: AdoptedSession, backend_id: &str) -> Self {
        Self {
            output: adopted.output,
            input: adopted.input,
            backend_id: backend_id.to_string(),
            mode: WireMode::Adopt,
            seed_len: adopted.seed_len,
            resident: true,
            size: adopted.size,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WireMode {
    Spawn,
    Adopt,
}

fn initial_output_at(mode: WireMode) -> u64 {
    match mode {
        WireMode::Spawn => now_millis(),
        WireMode::Adopt => 0,
    }
}

const INPUT_CHANNEL_CAPACITY: usize = 1024;

fn send_to_input_channel(tx: &mpsc::Sender<Vec<u8>>, data: Vec<u8>, what: &str) -> Result<()> {
    use tokio::sync::mpsc::error::TrySendError;
    tx.try_send(data).map_err(|e| match e {
        TrySendError::Full(_) => anyhow::anyhow!("{what} input channel full (writer stalled)"),
        TrySendError::Closed(_) => anyhow::anyhow!("{what} input channel closed"),
    })
}

struct SignalCells {
    last_title: Arc<Mutex<Option<String>>>,
    attention_at: Arc<AtomicU64>,
    notification: Arc<Mutex<Option<String>>>,
    meta_gen: Arc<AtomicU64>,
    app_copy: Arc<AppCopy>,
}

impl SignalCells {
    fn new() -> Self {
        Self {
            last_title: Arc::new(Mutex::new(None)),
            attention_at: Arc::new(AtomicU64::new(0)),
            notification: Arc::new(Mutex::new(None)),
            meta_gen: Arc::new(AtomicU64::new(0)),
            app_copy: Arc::default(),
        }
    }

    fn term_signals(&self) -> TermSignals {
        TermSignals {
            title: Arc::clone(&self.last_title),
            attention_at: Arc::clone(&self.attention_at),
            notification: Arc::clone(&self.notification),
            meta_gen: Arc::clone(&self.meta_gen),
            app_copy: Arc::clone(&self.app_copy),
            ..Default::default()
        }
    }
}

const DORMANT_GRID: (u16, u16) = (2, 2);

const SNAPSHOT_RETRY_MS: u64 = 2_000;

#[derive(Default)]
struct Residency {
    dormant: AtomicBool,
    requested_at: AtomicU64,
    changes: AtomicU64,
    landed: (Mutex<()>, std::sync::Condvar),
    scrollback: usize,
}

impl Residency {
    fn resident(scrollback: usize) -> Self {
        Self {
            scrollback,
            ..Self::default()
        }
    }

    fn dormant(scrollback: usize) -> Self {
        Self {
            dormant: AtomicBool::new(true),
            scrollback,
            ..Self::default()
        }
    }

    fn is_resident(&self) -> bool {
        !self.dormant.load(Ordering::Acquire)
    }
}

fn carried_state(screen: &vt100::Screen) -> Vec<u8> {
    let mut out = Vec::new();
    if screen.alternate_screen() {
        out.extend_from_slice(b"\x1b[?1049h");
    }
    out.extend_from_slice(screen.input_mode_formatted().as_bytes());
    if screen.hide_cursor() {
        out.extend_from_slice(b"\x1b[?25l");
    }
    out.extend_from_slice(screen.attributes_formatted().as_bytes());
    out
}

pub fn snapshot_seed(snapshot: &PaneSnapshot, dormant: &vt100::Screen) -> Vec<u8> {
    let mut out = Vec::new();
    let write_rows = |out: &mut Vec<u8>, rows: &[String]| {
        for (i, row) in rows.iter().enumerate() {
            if i > 0 {
                out.extend_from_slice(b"\r\n");
            }
            out.extend_from_slice(row.as_bytes());
        }
    };
    write_rows(&mut out, &snapshot.normal);
    if let Some(alternate) = &snapshot.alternate {
        out.extend_from_slice(b"\x1b[m\x1b[?1049h");
        write_rows(&mut out, alternate);
    }
    let (x, y) = snapshot.cursor;
    out.extend_from_slice(format!("\x1b[m\x1b[{};{}H", y + 1, x + 1).as_bytes());
    out.extend_from_slice(dormant.input_mode_formatted().as_bytes());
    if dormant.hide_cursor() {
        out.extend_from_slice(b"\x1b[?25l");
    }
    out.extend_from_slice(dormant.attributes_formatted().as_bytes());
    out
}

pub fn parser_from_snapshot(snapshot: &PaneSnapshot, scrollback: usize) -> SessionParser {
    let (rows, cols) = vt_floor(snapshot.rows, snapshot.cols);
    let blank = vt100::Parser::new(
        DORMANT_GRID.0.try_into().unwrap(),
        DORMANT_GRID.1.try_into().unwrap(),
        0,
    );
    let mut parser = vt100::Parser::new_with_callbacks(
        rows.try_into().unwrap(),
        cols.try_into().unwrap(),
        scrollback,
        TermSignals::default(),
    );
    parser.process(&snapshot_seed(snapshot, blank.screen()));
    parser
}

struct ReaderSignals {
    exited: Arc<AtomicBool>,
    last_output_at: Arc<AtomicU64>,
    output_seq: Arc<AtomicU64>,
}

pub struct WiredPane {
    pub parser: Arc<Mutex<SessionParser>>,
    input_tx: mpsc::Sender<Vec<u8>>,
    pub(crate) backend_id: String,
    exited: Arc<AtomicBool>,
    last_output_at: Arc<AtomicU64>,
    output_seq: Arc<AtomicU64>,
    label: &'static str,
    residency: Arc<Residency>,
    backend: Option<Arc<dyn SessionBackend>>,
    size: Option<PaneSize>,
    wanted: AtomicU32,
    app_copy: Arc<AppCopy>,
}

impl WiredPane {
    pub fn app_copy(&self) -> &Arc<AppCopy> {
        &self.app_copy
    }

    pub fn send_input(&self, data: Vec<u8>) -> Result<()> {
        self.claim_size();
        send_to_input_channel(&self.input_tx, data, self.label)
    }

    pub fn claim_size(&self) {
        let (Some(backend), Some(size)) = (&self.backend, &self.size) else {
            return;
        };
        let wanted = self.wanted.load(Ordering::Relaxed);
        let Some((rows, cols)) = unpack_size(wanted) else {
            return;
        };
        if size.current() == wanted {
            return;
        }
        if let Err(e) = backend.claim_size(&self.backend_id, rows, cols) {
            debug!(pane = %self.backend_id, "could not claim the pane's size: {e:#}");
        }
    }

    pub fn retake_size(&self) {
        let (Some(backend), Some(size)) = (&self.backend, &self.size) else {
            return;
        };
        if !size.take_released() {
            return;
        }
        let wanted = self.wanted.load(Ordering::Relaxed);
        let Some((rows, cols)) = unpack_size(wanted) else {
            return;
        };
        if size.current() == wanted {
            return;
        }
        if let Err(e) = backend.resize(&self.backend_id, rows, cols) {
            debug!(pane = %self.backend_id, "could not take the pane's size back: {e:#}");
        }
    }

    pub fn sized_elsewhere(&self) -> bool {
        self.size.as_ref().is_some_and(PaneSize::sized_elsewhere)
    }

    pub fn last_output_at(&self) -> u64 {
        self.last_output_at.load(Ordering::Relaxed)
    }

    pub fn output_seq(&self) -> u64 {
        self.output_seq.load(Ordering::Acquire)
    }

    pub fn output_seq_cell(&self) -> &AtomicU64 {
        &self.output_seq
    }

    pub fn has_exited(&self) -> bool {
        self.exited.load(Ordering::SeqCst)
    }

    pub fn backend_id(&self) -> &str {
        &self.backend_id
    }

    pub fn resize(&self, backend: &dyn SessionBackend, rows: u16, cols: u16) -> Result<()> {
        let (rows, cols) = vt_floor(rows, cols);
        self.wanted.store(pack_size(rows, cols), Ordering::Relaxed);
        backend.resize(&self.backend_id, rows, cols)?;
        if self.size.is_none() {
            if let Ok(mut parser) = self.parser.lock() {
                if self.residency.is_resident() {
                    parser
                        .screen_mut()
                        .set_size(rows.try_into().unwrap(), cols.try_into().unwrap());
                }
            }
        }
        Ok(())
    }

    pub fn is_resident(&self) -> bool {
        self.residency.is_resident()
    }

    pub fn content_stamp(&self) -> u64 {
        self.last_output_at()
            .wrapping_add(self.residency.changes.load(Ordering::Acquire))
    }

    pub fn evict(&self) -> bool {
        let Ok(mut parser) = self.parser.lock() else {
            return false;
        };
        if !self.residency.is_resident() {
            return false;
        }
        let mut signals = std::mem::take(parser.callbacks_mut());
        // Positions on a grid that is going; the rebuild records them again
        // from whatever links the capture carries.
        signals.hyperlinks = HyperlinkTable::default();
        signals.pending_link = None;
        let carried = carried_state(parser.screen());
        let mut dormant = vt100::Parser::new_with_callbacks(
            DORMANT_GRID.0.try_into().unwrap(),
            DORMANT_GRID.1.try_into().unwrap(),
            0,
            signals,
        );
        dormant.process(&carried);
        *parser = dormant;
        self.residency.dormant.store(true, Ordering::Release);
        true
    }

    pub fn restore(&self, backend: &dyn SessionBackend) -> bool {
        if self.is_resident() {
            return false;
        }
        let now = now_millis();
        let asked = self.residency.requested_at.load(Ordering::Acquire);
        if asked != 0 && now.saturating_sub(asked) < SNAPSHOT_RETRY_MS {
            return false;
        }
        match backend.request_snapshot(&self.backend_id) {
            Ok(()) => {
                self.residency.requested_at.store(now, Ordering::Release);
                true
            }
            Err(e) => {
                debug!(pane = %self.backend_id, "could not ask for a snapshot: {e:#}");
                false
            }
        }
    }

    pub fn wait_resident(&self, budget: std::time::Duration) -> bool {
        let deadline = std::time::Instant::now() + budget;
        let (lock, landed) = &self.residency.landed;
        let Ok(mut guard) = lock.lock() else {
            return self.is_resident();
        };
        while !self.is_resident() {
            let left = deadline.saturating_duration_since(std::time::Instant::now());
            if left.is_zero() {
                break;
            }
            guard = match landed.wait_timeout(guard, left) {
                Ok((guard, _)) => guard,
                Err(_) => return self.is_resident(),
            };
        }
        self.is_resident()
    }
}

pub struct ShellPane {
    wired: WiredPane,
}

impl std::ops::Deref for ShellPane {
    type Target = WiredPane;
    fn deref(&self) -> &WiredPane {
        &self.wired
    }
}

pub struct ProgramPane {
    wired: WiredPane,
    backend: Arc<dyn SessionBackend>,
    pub program: String,
}

impl std::ops::Deref for ProgramPane {
    type Target = WiredPane;
    fn deref(&self) -> &WiredPane {
        &self.wired
    }
}

impl ProgramPane {
    #[allow(clippy::too_many_arguments)]
    pub fn spawn(
        backend: Arc<dyn SessionBackend>,
        window_name: &str,
        program: &str,
        args: &[String],
        cwd: Option<&Path>,
        env: &HashMap<String, String>,
        rows: u16,
        cols: u16,
        scrollback: usize,
    ) -> Result<Self> {
        let spawned = backend.spawn(window_name, program, args, cwd, env, rows, cols)?;
        // No session id: a program belongs to a plugin, and the role is what
        // keeps its window from ever resolving as somebody's agent.
        if let Err(e) = backend.stamp_window(&spawned.backend_id, "", WindowRole::Program) {
            debug!("could not stamp the program window {window_name}: {e:#}");
        }
        let (wired, _signals) = Session::wire_up(
            rows,
            cols,
            SessionIo::spawned(spawned),
            &backend,
            "Program",
            scrollback,
        );
        debug!(program, window_name, "spawned a plugin's program pane");
        Ok(Self {
            wired,
            backend,
            program: program.to_string(),
        })
    }

    pub fn adopt(
        backend: Arc<dyn SessionBackend>,
        backend_id: &str,
        program: &str,
        rows: u16,
        cols: u16,
        scrollback: usize,
    ) -> Result<Self> {
        let adopted = backend.adopt(backend_id, rows, cols, None)?;
        let (wired, _signals) = Session::wire_up(
            rows,
            cols,
            SessionIo::adopted(adopted, backend_id),
            &backend,
            "Program",
            scrollback,
        );
        debug!(program, backend_id, "adopted a plugin's program pane");
        Ok(Self {
            wired,
            backend,
            program: program.to_string(),
        })
    }

    pub fn resize(&self, rows: u16, cols: u16) -> bool {
        if let Err(e) = self.wired.resize(self.backend.as_ref(), rows, cols) {
            tracing::debug!(
                "could not resize program pane {}: {e:#}",
                self.wired.backend_id
            );
            return false;
        }
        true
    }

    pub fn kill(&self) {
        if let Err(e) = self.backend.kill(&self.wired.backend_id) {
            tracing::warn!(
                "could not kill program pane {}: {e:#}",
                self.wired.backend_id
            );
        }
    }
}

#[derive(Debug, Clone)]
pub struct PaneInfo {
    pub id: SessionId,
    pub name: String,
    pub cwd: Option<std::path::PathBuf>,
    pub backend_id: Option<String>,
    pub shell_backend_id: Option<String>,
    pub remote_host: Option<String>,
}

impl PaneInfo {
    pub fn new(name: String) -> Self {
        Self {
            id: SessionId::default(),
            name,
            cwd: None,
            backend_id: None,
            shell_backend_id: None,
            remote_host: None,
        }
    }
}

pub struct Session {
    pub info: PaneInfo,
    wired: WiredPane,
    backend: Arc<dyn SessionBackend>,
    shell_backend: Option<Arc<dyn SessionBackend>>,
    signals: SignalCells,
    last_synced_meta_gen: u64,
    attention_ack_at: u64,
    pub shell_pane: Option<ShellPane>,
    placeholder: bool,
}

impl std::ops::Deref for Session {
    type Target = WiredPane;
    fn deref(&self) -> &WiredPane {
        &self.wired
    }
}

impl Session {
    pub fn is_dead(&self) -> Result<bool> {
        Ok(self.backend.is_dead(self.backend_id())?)
    }

    pub fn shell_is_dead(&self) -> Option<Result<bool>> {
        self.shell_pane.as_ref().map(|shell| {
            self.shell_backend()
                .is_dead(shell.backend_id())
                .map_err(Into::into)
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub fn adopt(
        name: String,
        rows: u16,
        cols: u16,
        backend_id: &str,
        backend: &Arc<dyn SessionBackend>,
        seed: Option<Vec<u8>>,
        scrollback: usize,
    ) -> Result<Self> {
        let adopted = backend.adopt(backend_id, rows, cols, seed)?;

        debug!(
            backend_id = %backend_id,
            parser_rows = rows,
            parser_cols = cols,
            "Adopting session"
        );

        let mut info = PaneInfo::new(name);
        info.backend_id = Some(backend_id.to_string());
        debug!(session_id = %info.id, backend_id = %backend_id, "Adopted session via backend");

        Ok(Self::wire_io(
            info,
            rows,
            cols,
            SessionIo::adopted(adopted, backend_id),
            backend,
            scrollback,
        ))
    }

    #[allow(clippy::too_many_arguments)]
    pub fn adopt_io(
        name: String,
        rows: u16,
        cols: u16,
        backend_id: &str,
        backend: &Arc<dyn SessionBackend>,
        adopted: AdoptedSession,
        scrollback: usize,
    ) -> Self {
        let mut info = PaneInfo::new(name);
        info.backend_id = Some(backend_id.to_string());
        let io = SessionIo {
            resident: false,
            ..SessionIo::adopted(adopted, backend_id)
        };
        Self::wire_io(info, rows, cols, io, backend, scrollback)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn adopt_dormant(
        name: String,
        rows: u16,
        cols: u16,
        backend_id: &str,
        backend: &Arc<dyn SessionBackend>,
        scrollback: usize,
    ) -> Result<Self> {
        // Only the title and modes, not `None`: `None` is the backend capturing
        // the whole history itself. The seed goes through the stream like any
        // other, so the two cells' callbacks report it and `seed_len` keeps it
        // from reading as output.
        let adopted =
            backend.adopt(backend_id, rows, cols, Some(backend.state_seed(backend_id)))?;
        let mut info = PaneInfo::new(name);
        info.backend_id = Some(backend_id.to_string());
        debug!(session_id = %info.id, backend_id = %backend_id, "Adopted session without a grid");
        let (wired, signals) = Self::wire_up(
            rows,
            cols,
            SessionIo {
                resident: false,
                ..SessionIo::adopted(adopted, backend_id)
            },
            backend,
            "Session",
            scrollback,
        );
        backend.hide_output(backend_id);
        Ok(Self {
            info,
            wired,
            backend: Arc::clone(backend),
            shell_backend: None,
            signals,
            last_synced_meta_gen: u64::MAX,
            attention_ack_at: 0,
            shell_pane: None,
            placeholder: false,
        })
    }

    pub fn supports_snapshots(&self) -> bool {
        !self.placeholder && self.backend.capabilities().supports_snapshots
    }

    fn wired_pane(&self, shell: bool) -> Option<&WiredPane> {
        if shell {
            self.shell_pane.as_ref().map(|pane| &pane.wired)
        } else {
            Some(&self.wired)
        }
    }

    pub fn evict_pane(&self, shell: bool) -> bool {
        let Some(pane) = self.wired_pane(shell) else {
            return false;
        };
        if !self.backend_of(shell).capabilities().supports_snapshots
            || self.placeholder
            || !pane.evict()
        {
            return false;
        }
        self.backend_of(shell).hide_output(pane.backend_id());
        true
    }

    pub fn restore_pane(&self, shell: bool) -> bool {
        self.wired_pane(shell)
            .is_some_and(|pane| pane.restore(self.backend_of(shell).as_ref()))
    }

    fn wire_up(
        rows: u16,
        cols: u16,
        io: SessionIo,
        backend: &Arc<dyn SessionBackend>,
        label: &'static str,
        scrollback: usize,
    ) -> (WiredPane, SignalCells) {
        let signals = SignalCells::new();
        // The floor applies at construction too: a session first painted into a
        // one-column pane would otherwise panic on its first line of output,
        // before any resize could correct it.
        let (rows, cols) = vt_floor(rows, cols);
        // The rect the caller is about to paint into, which the render path's
        // own memo starts from too — so the first claim knows it. Taken before
        // a dormant pane's grid size replaces `rows`/`cols`: the memo matches
        // the rect, so no paint resizes the pane to correct it, and a claim
        // would size it to the dormant grid.
        let wanted = pack_size(rows, cols);
        let (residency, (rows, cols), kept) = if io.resident {
            (Residency::resident(scrollback), (rows, cols), scrollback)
        } else {
            (Residency::dormant(scrollback), DORMANT_GRID, 0)
        };
        let residency = Arc::new(residency);
        let parser = Arc::new(Mutex::new(vt100::Parser::new_with_callbacks(
            rows.try_into().unwrap(),
            cols.try_into().unwrap(),
            kept,
            signals.term_signals(),
        )));

        let exited = Arc::new(AtomicBool::new(false));
        let last_output_at = Arc::new(AtomicU64::new(initial_output_at(io.mode)));
        let output_seq = Arc::new(AtomicU64::new(0));

        let (input_tx, input_rx) = mpsc::channel(INPUT_CHANNEL_CAPACITY);
        Self::spawn_writer(io.input, input_rx);

        let parser_clone = Arc::clone(&parser);
        let residency_clone = Arc::clone(&residency);
        let reader_signals = ReaderSignals {
            exited: Arc::clone(&exited),
            last_output_at: Arc::clone(&last_output_at),
            output_seq: Arc::clone(&output_seq),
        };
        let seed_len = io.seed_len;
        let size = io.size.clone();
        tokio::task::spawn_blocking(move || {
            Self::reader_loop(
                io.output,
                parser_clone,
                reader_signals,
                residency_clone,
                seed_len,
                size,
            );
        });

        let wired = WiredPane {
            parser,
            input_tx,
            backend_id: io.backend_id,
            exited,
            last_output_at,
            output_seq,
            label,
            residency,
            backend: Some(Arc::clone(backend)),
            size: io.size,
            wanted: AtomicU32::new(wanted),
            app_copy: Arc::clone(&signals.app_copy),
        };
        (wired, signals)
    }

    fn wire_io(
        info: PaneInfo,
        rows: u16,
        cols: u16,
        io: SessionIo,
        backend: &Arc<dyn SessionBackend>,
        scrollback: usize,
    ) -> Self {
        let (wired, signals) = Self::wire_up(rows, cols, io, backend, "Session", scrollback);
        Self {
            info,
            wired,
            backend: Arc::clone(backend),
            shell_backend: None,
            signals,
            last_synced_meta_gen: u64::MAX,
            attention_ack_at: 0,
            shell_pane: None,
            placeholder: false,
        }
    }

    pub fn placeholder(
        mut info: PaneInfo,
        rows: u16,
        cols: u16,
        backend: &Arc<dyn SessionBackend>,
        scrollback: usize,
    ) -> Self {
        info.backend_id = None;

        let signals = SignalCells::new();
        let (rows, cols) = vt_floor(rows, cols);
        let parser = Arc::new(Mutex::new(vt100::Parser::new_with_callbacks(
            rows.try_into().unwrap(),
            cols.try_into().unwrap(),
            scrollback,
            signals.term_signals(),
        )));
        let host = info.remote_host.clone().unwrap_or_else(|| "?".into());
        let notice = format!(
            "\r\n  \u{2298} Remote host '{host}' unreachable \u{2014} retrying\u{2026}\r\n\r\n  \
             This session will reconnect automatically when the host comes back.\r\n  \
             Press restart to retry now, or delete to remove it.\r\n"
        );
        if let Ok(mut p) = parser.lock() {
            p.process(notice.as_bytes());
        }

        // A dead input channel: the receiver is dropped immediately, so any
        // keystroke `try_send` fails fast and the byte is discarded.
        let (input_tx, _dead_rx) = mpsc::channel(INPUT_CHANNEL_CAPACITY);

        Self {
            info,
            wired: WiredPane {
                parser,
                input_tx,
                backend_id: String::new(),
                exited: Arc::new(AtomicBool::new(false)),
                last_output_at: Arc::new(AtomicU64::new(0)),
                output_seq: Arc::new(AtomicU64::new(0)),
                label: "Session",
                residency: Arc::new(Residency::resident(scrollback)),
                backend: None,
                size: None,
                wanted: AtomicU32::new(0),
                app_copy: Arc::clone(&signals.app_copy),
            },
            backend: Arc::clone(backend),
            shell_backend: None,
            signals,
            last_synced_meta_gen: u64::MAX,
            attention_ack_at: 0,
            shell_pane: None,
            placeholder: true,
        }
    }

    pub fn is_placeholder(&self) -> bool {
        self.placeholder
    }

    fn reader_loop(
        reader: PaneOutput,
        parser: Arc<Mutex<SessionParser>>,
        signals: ReaderSignals,
        residency: Arc<Residency>,
        mut seed_len: usize,
        size: Option<PaneSize>,
    ) {
        // Bytes of a trailing, not-yet-complete UTF-8 character held back from
        // the previous chunk. The agent's output is a single byte stream, but
        // tmux's `%output` framing (and the OS read boundary under it) can fall
        // in the middle of a multi-byte character. vt100 is NOT robust to a
        // `process()` chunk that ends mid-codepoint — it can swallow a following
        // control byte (e.g. a newline), misplacing later output — so we never
        // hand it a truncated tail. `carry` is at most 3 bytes (a 4-byte char
        // missing one).
        let mut carry: Vec<u8> = Vec::new();
        for chunk in reader {
            // Taken between chunks, never during one: a reported size is a
            // chunk of its own (`PaneChunk::Resized`), so every byte before it
            // is already in the grid and none after it is. A pane with no grid
            // has nothing to resize; the grid it gets back is built at the
            // pane's size as tmux reports it then.
            if let Some((rows, cols)) = size.as_ref().and_then(PaneSize::take) {
                let (rows, cols) = vt_floor(rows, cols);
                if let Ok(mut p) = parser.lock() {
                    if residency.is_resident() {
                        p.screen_mut()
                            .set_size(rows.try_into().unwrap(), cols.try_into().unwrap());
                    }
                }
            }
            match chunk {
                PaneChunk::Output(bytes) => {
                    let n = bytes.len();
                    let mut data = std::mem::take(&mut carry);
                    data.extend_from_slice(&bytes);
                    let ready = utf8_ready_prefix_len(&data);
                    carry = data.split_off(ready);
                    Self::feed_parser(&parser, &data);
                    // Bytes beyond the seed boundary are live activity; a chunk
                    // that is entirely within the seed is not. Activity records
                    // the chunk even when its trailing character is still
                    // buffered. The redraw sequence moves only when the parser
                    // has taken a complete prefix, and after it has done so.
                    if seed_len < n {
                        signals
                            .last_output_at
                            .store(now_millis(), Ordering::Relaxed);
                    }
                    seed_len = seed_len.saturating_sub(n);
                    if ready > 0 {
                        signals.output_seq.fetch_add(1, Ordering::Release);
                        crate::output_wake::notify(&signals.output_seq);
                    }
                }
                // A hidden pane's title and modes: parsed, never taken for
                // output, since what they say is state the pane already had.
                PaneChunk::State(state) => Self::feed_parser(&parser, &state),
                PaneChunk::Snapshot(snapshot) => {
                    Self::install(&parser, &residency, &snapshot, false);
                    Self::rebuilt(&signals);
                }
                PaneChunk::Resync(snapshot) => {
                    carry.clear();
                    Self::install(&parser, &residency, &snapshot, true);
                    Self::rebuilt(&signals);
                }
                PaneChunk::Resized { .. } | PaneChunk::SizedElsewhere(_) => {}
            }
        }
        debug!("Session reader: stream ended");
        // Flush any leftover partial UTF-8 sequence, since no more bytes are
        // coming to complete it.
        Self::feed_parser(&parser, &carry);
        signals.output_seq.fetch_add(1, Ordering::Release);
        signals.exited.store(true, Ordering::SeqCst);
    }

    fn rebuilt(signals: &ReaderSignals) {
        signals.output_seq.fetch_add(1, Ordering::Release);
        crate::output_wake::notify(&signals.output_seq);
    }

    fn install(
        parser: &Mutex<SessionParser>,
        residency: &Residency,
        snapshot: &PaneSnapshot,
        resync: bool,
    ) {
        if let Ok(mut parser) = parser.lock() {
            let rebuild = if residency.is_resident() {
                resync
            } else {
                !resync || residency.requested_at.load(Ordering::Acquire) != 0
            };
            if rebuild {
                let seed = snapshot_seed(snapshot, parser.screen());
                let mut signals = std::mem::take(parser.callbacks_mut());
                // Positions on the grid being replaced, as `evict` drops them.
                signals.hyperlinks = HyperlinkTable::default();
                signals.pending_link = None;
                let (rows, cols) = vt_floor(snapshot.rows, snapshot.cols);
                let mut rebuilt = vt100::Parser::new_with_callbacks(
                    rows.try_into().unwrap(),
                    cols.try_into().unwrap(),
                    residency.scrollback,
                    signals,
                );
                rebuilt.process(&seed);
                *parser = rebuilt;
                residency.dormant.store(false, Ordering::Release);
                residency.changes.fetch_add(1, Ordering::AcqRel);
            }
        }
        residency.requested_at.store(0, Ordering::Release);
        let (lock, landed) = &residency.landed;
        // Taken so a waiter cannot check the flag, miss this wake-up and then
        // sleep out its budget.
        let _guard = lock.lock();
        landed.notify_all();
    }

    fn feed_parser(parser: &Mutex<SessionParser>, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        if let Ok(mut p) = parser.lock() {
            p.process(bytes);
        }
    }

    fn spawn_writer(mut writer: Box<dyn Write + Send>, mut input_rx: mpsc::Receiver<Vec<u8>>) {
        let spawned = std::thread::Builder::new()
            .name("pane-writer".into())
            .spawn(move || {
                while let Some(data) = input_rx.blocking_recv() {
                    if let Err(e) = writer.write_all(&data) {
                        error!("Session writer error: {e}");
                        break;
                    }
                    if let Err(e) = writer.flush() {
                        error!("Session flush error: {e}");
                        break;
                    }
                }
                debug!("Session writer thread exiting");
            });
        if let Err(e) = spawned {
            error!("failed to spawn a pane writer: {e}");
        }
    }

    pub fn resize(&self, rows: u16, cols: u16) -> bool {
        // A placeholder has no live pane; only resize its local notice buffer.
        // Talking to the (possibly-down) backend here would issue a blocking
        // ssh resize on the UI thread — the freeze we're avoiding. Floored for
        // the reason `WiredPane::resize` (the live path) documents.
        if self.placeholder {
            let (rows, cols) = vt_floor(rows, cols);
            if let Ok(mut parser) = self.wired.parser.lock() {
                parser
                    .screen_mut()
                    .set_size(rows.try_into().unwrap(), cols.try_into().unwrap());
            }
            // Nothing to reach, and nothing to ask again for.
            return true;
        }
        if let Err(e) = self.wired.resize(self.backend.as_ref(), rows, cols) {
            tracing::warn!("Failed to resize session: {e}");
            return false;
        }
        true
    }

    pub fn resize_shell(&self, rows: u16, cols: u16) -> bool {
        let Some(shell) = &self.shell_pane else {
            // No shell, so nothing to reach and nothing left at a stale width.
            return true;
        };
        // [`Self::resize`]'s placeholder branch, spelled out here too — grid
        // included. Reporting `true` without sizing the local buffer would let
        // the render path memoize a size the parser never took, leaving the
        // pane wrapped at the old width with nothing to ask again: the #1220
        // failure, one pane down. Nothing constructs a placeholder in v2 (see
        // the field), so this costs a branch and buys the two halves being
        // unable to come apart if one ever does.
        if self.placeholder {
            let (rows, cols) = vt_floor(rows, cols);
            if let Ok(mut parser) = shell.parser.lock() {
                parser
                    .screen_mut()
                    .set_size(rows.try_into().unwrap(), cols.try_into().unwrap());
            }
            return true;
        }
        if let Err(e) = shell
            .wired
            .resize(self.shell_backend().as_ref(), rows, cols)
        {
            tracing::warn!("Failed to resize shell pane: {e}");
            return false;
        }
        true
    }

    #[cfg(test)]
    pub fn mark_exited_for_test(&self) {
        self.wired.exited.store(true, Ordering::SeqCst);
    }

    #[cfg(test)]
    pub fn backdate_output_for_test(&self, ms: u64) {
        let now = now_millis();
        self.wired
            .last_output_at
            .store(now.saturating_sub(ms), Ordering::Relaxed);
    }

    pub fn millis_since_last_output(&self) -> u64 {
        now_millis().saturating_sub(self.wired.last_output_at.load(Ordering::Relaxed))
    }

    pub fn agent_title(&self) -> Option<String> {
        self.signals.last_title.lock().ok().and_then(|t| t.clone())
    }

    pub fn sync_agent_meta(&mut self) -> Option<(Option<String>, Option<String>)> {
        let gen = self.signals.meta_gen.load(Ordering::Acquire);
        if gen == self.last_synced_meta_gen {
            return None;
        }
        self.last_synced_meta_gen = gen;
        Some((self.agent_title(), self.notification()))
    }

    pub fn needs_attention(&self) -> bool {
        self.signals.attention_at.load(Ordering::Relaxed) > self.attention_ack_at
    }

    pub fn notification(&self) -> Option<String> {
        self.signals
            .notification
            .lock()
            .ok()
            .and_then(|n| n.clone())
    }

    pub fn acknowledge_attention(&mut self) {
        self.attention_ack_at = now_millis();
    }

    pub fn backend_name(&self) -> &str {
        self.backend.name()
    }

    pub fn pane_pid(&self) -> Result<Option<u32>> {
        Ok(self.backend.pane_pid(&self.wired.backend_id)?)
    }

    fn shell_backend(&self) -> &Arc<dyn SessionBackend> {
        self.shell_backend.as_ref().unwrap_or(&self.backend)
    }

    fn backend_of(&self, shell: bool) -> &Arc<dyn SessionBackend> {
        match shell {
            true => self.shell_backend(),
            false => &self.backend,
        }
    }

    pub fn backend_handle(&self) -> (Arc<dyn SessionBackend>, String) {
        (Arc::clone(&self.backend), self.wired.backend_id.clone())
    }

    pub fn kill(&self) {
        // A placeholder owns no live backend pane (see `placeholder`).
        if self.placeholder {
            return;
        }
        self.kill_shell_pane();
        if let Err(e) = self.backend.kill(&self.wired.backend_id) {
            tracing::warn!("Failed to kill session: {e}");
        }
    }

    pub fn detach(self) {
        // A placeholder owns no live backend pane — detaching would issue a
        // blocking ssh call (possibly to a down host) for nothing.
        if self.placeholder {
            return;
        }
        if let Some(shell) = &self.shell_pane {
            if let Err(e) = self.shell_backend().detach(&shell.wired.backend_id) {
                tracing::warn!("Failed to detach shell pane: {e}");
            }
        }
        if let Err(e) = self.backend.detach(&self.wired.backend_id) {
            tracing::warn!("Failed to detach session: {e}");
        }
        drop(self.wired);
        debug!("Session detached");
    }

    pub fn shell_over(
        adopted: AdoptedSession,
        backend: &Arc<dyn SessionBackend>,
        backend_id: &str,
        rows: u16,
        cols: u16,
        scrollback: usize,
    ) -> ShellPane {
        // Without its grid until the stream's first snapshot, as an agent's
        // stream pane (`adopt_io`): that snapshot is the shell's screen, with
        // what it showed before this view came. A stream with no screen keeper
        // sends a blank one.
        let io = SessionIo {
            resident: false,
            ..SessionIo::adopted(adopted, backend_id)
        };
        let (wired, _) = Self::wire_up(rows, cols, io, backend, "Shell", scrollback);
        ShellPane { wired }
    }

    pub fn install_shell_over(&mut self, shell: ShellPane, backend: Arc<dyn SessionBackend>) {
        self.shell_backend = Some(backend);
        self.install_shell(shell);
    }

    pub fn install_shell(&mut self, shell: ShellPane) {
        self.info.shell_backend_id = Some(shell.backend_id.clone());
        self.shell_pane = Some(shell);
    }

    fn kill_shell_pane(&self) {
        if let Some(shell) = &self.shell_pane {
            if let Err(e) = self.shell_backend().kill(&shell.wired.backend_id) {
                tracing::warn!("Failed to kill shell pane: {e}");
            }
        }
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn stub(name: &str, backend: &Arc<dyn SessionBackend>) -> Self {
        Self::stub_with_input_rx(name, backend).0
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn stub_with_input_rx(
        name: &str,
        backend: &Arc<dyn SessionBackend>,
    ) -> (Self, mpsc::Receiver<Vec<u8>>) {
        let (input_tx, input_rx) = mpsc::channel(INPUT_CHANNEL_CAPACITY);
        // Wire TermSignals to the session's accessor cells exactly like
        // `wire_up`, so bytes injected via `feed_output_for_test` drive
        // `agent_title`/`needs_attention` the same way live PTY output does.
        let signals = SignalCells::new();
        let parser = Arc::new(Mutex::new(vt100::Parser::new_with_callbacks(
            24.try_into().unwrap(),
            80.try_into().unwrap(),
            0,
            signals.term_signals(),
        )));
        let session = Self {
            info: PaneInfo::new(name.to_string()),
            wired: WiredPane {
                parser,
                input_tx,
                backend_id: String::new(),
                exited: Arc::new(AtomicBool::new(false)),
                last_output_at: Arc::new(AtomicU64::new(now_millis())),
                output_seq: Arc::new(AtomicU64::new(0)),
                label: "Session",
                residency: Arc::default(),
                backend: None,
                size: None,
                wanted: AtomicU32::new(0),
                app_copy: Arc::clone(&signals.app_copy),
            },
            backend: Arc::clone(backend),
            shell_backend: None,
            signals,
            last_synced_meta_gen: u64::MAX,
            attention_ack_at: 0,
            shell_pane: None,
            placeholder: false,
        };
        (session, input_rx)
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn feed_output_for_test(&self, bytes: &[u8]) {
        // Strictly-increasing bump: two feeds within the same millisecond must
        // still read as *new* output to `App::detect_output_redraw`'s signature.
        let prev = self.wired.last_output_at.load(Ordering::Relaxed);
        self.wired
            .last_output_at
            .store(now_millis().max(prev + 1), Ordering::Relaxed);
        if let Ok(mut p) = self.wired.parser.lock() {
            p.process(bytes);
        }
        self.wired.output_seq.fetch_add(1, Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn output(chunks: Vec<PaneChunk>) -> PaneOutput {
        Box::new(chunks.into_iter())
    }

    fn print_copy(copy: &Arc<AppCopy>, text: &str) {
        let mut parser = vt100::Parser::new_with_callbacks(
            4.try_into().unwrap(),
            20.try_into().unwrap(),
            0,
            TermSignals {
                app_copy: Arc::clone(copy),
                ..Default::default()
            },
        );
        let encoded =
            base64::Engine::encode(&base64::engine::general_purpose::STANDARD, text.as_bytes());
        parser.process(format!("\x1b]52;c;{encoded}\x07").as_bytes());
    }

    #[test]
    fn an_osc52_write_is_kept_only_under_the_focus_it_was_made_in() {
        let copy = Arc::new(AppCopy::default());
        let request = |data: &str| AppCopyRequest {
            target: b"c".to_vec(),
            data: base64::Engine::encode(&base64::engine::general_purpose::STANDARD, data)
                .into_bytes(),
        };

        print_copy(&copy, "unfocused");
        assert_eq!(copy.take(), [], "a pane without focus keeps nothing");

        copy.focus(1);
        print_copy(&copy, "focused");
        print_copy(&copy, "and-again");
        assert_eq!(copy.take(), [request("focused"), request("and-again")]);
        assert_eq!(copy.take(), [], "taken once");

        // Made under focus, then focus left and came back: the write belongs
        // to a focus that ended.
        print_copy(&copy, "before-blur");
        copy.blur();
        copy.focus(2);
        assert_eq!(copy.take(), []);

        // The reader read token 2, focus moved on to 3, then the write landed.
        copy.focus(3);
        copy.pending
            .lock()
            .unwrap()
            .push_back((2, request("raced")));
        assert_eq!(copy.take(), [], "an old token's write is refused");

        for i in 0..PENDING_APP_COPIES + 2 {
            print_copy(&copy, &format!("flood-{i}"));
        }
        let kept = copy.take();
        assert_eq!(kept.len(), PENDING_APP_COPIES, "a flood is bounded");
        assert_eq!(
            kept.last(),
            Some(&request(&format!("flood-{}", PENDING_APP_COPIES + 1)))
        );
    }

    #[test]
    fn input_channel_overflow_fails_fast_without_blocking() {
        let (tx, _rx) = mpsc::channel(INPUT_CHANNEL_CAPACITY);
        for _ in 0..INPUT_CHANNEL_CAPACITY {
            send_to_input_channel(&tx, vec![b'x'], "Session").unwrap();
        }
        let err = send_to_input_channel(&tx, vec![b'x'], "Session").unwrap_err();
        assert!(err.to_string().contains("full"), "got: {err}");
    }

    #[test]
    fn a_blocked_pane_write_leaves_the_runtime_free() {
        struct Stuck(std::sync::mpsc::Receiver<()>);
        impl Write for Stuck {
            fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
                let _ = self.0.recv_timeout(std::time::Duration::from_secs(3));
                Ok(buf.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }

        let (release, stuck) = std::sync::mpsc::channel();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .expect("runtime");
        let waited = runtime.block_on(async {
            let (tx, rx) = mpsc::channel(INPUT_CHANNEL_CAPACITY);
            Session::spawn_writer(Box::new(Stuck(stuck)), rx);
            tx.send(b"x".to_vec()).await.expect("queue input");
            let started = std::time::Instant::now();
            tokio::spawn(async {}).await.expect("task");
            started.elapsed()
        });
        let _ = release.send(());
        assert!(
            waited < std::time::Duration::from_secs(1),
            "a task waited {waited:?} behind a blocked pane write"
        );
    }

    #[test]
    fn input_channel_closed_reports_closed() {
        let (tx, rx) = mpsc::channel::<Vec<u8>>(INPUT_CHANNEL_CAPACITY);
        drop(rx);
        let err = send_to_input_channel(&tx, vec![b'x'], "Session").unwrap_err();
        assert!(err.to_string().contains("closed"), "got: {err}");
    }

    #[test]
    fn a_grid_at_the_floor_survives_what_an_agent_prints() {
        // The floor is not cosmetic: at one row vt100 underflows in
        // `row_inc_scroll` as soon as output wraps, and at one column it
        // underflows in `col_wrap` as soon as a double-width character arrives.
        // Both used to be reachable — a one-cell pane is what a cramped layout
        // hands a session — and both killed the reader thread, which poisons the
        // parser mutex and blanks that pane for the rest of the run.
        for (rows, cols) in [(0, 0), (1, 1), (1, 2), (2, 1), (1, 40), (3, 1)] {
            let (rows, cols) = vt_floor(rows, cols);
            let mut parser =
                vt100::Parser::new(rows.try_into().unwrap(), cols.try_into().unwrap(), 0);
            parser
                .screen_mut()
                .set_size(rows.try_into().unwrap(), cols.try_into().unwrap());
            // A wide character (the `col_wrap` path) and enough plain text to
            // wrap and scroll (the `row_inc_scroll` path).
            parser.process("🚀 done\r\n".repeat(4).as_bytes());
            parser.process("hello world hello world".as_bytes());
        }
    }

    #[test]
    fn the_grid_floor_leaves_a_usable_size_alone() {
        assert_eq!(vt_floor(24, 80), (24, 80));
        assert_eq!(vt_floor(2, 2), (2, 2));
    }

    #[test]
    fn now_millis_returns_reasonable_value() {
        let ms = now_millis();
        // Should be after 2024-01-01 (1704067200000 ms since epoch).
        assert!(ms > 1_704_067_200_000);
    }

    #[test]
    fn utf8_ready_prefix_passes_complete_input() {
        assert_eq!(utf8_ready_prefix_len(b""), 0);
        assert_eq!(utf8_ready_prefix_len(b"hello"), 5);
        // "é" = c3 a9, complete.
        assert_eq!(utf8_ready_prefix_len(&[b'a', 0xc3, 0xa9]), 3);
        // "你好" complete (two 3-byte chars).
        assert_eq!(utf8_ready_prefix_len("你好".as_bytes()), 6);
    }

    #[test]
    fn utf8_ready_prefix_holds_back_truncated_tail() {
        // Lone 2-byte lead → hold all of it.
        assert_eq!(utf8_ready_prefix_len(&[b'a', 0xc3]), 1);
        // 3-byte lead with one continuation, missing one → hold the two.
        assert_eq!(utf8_ready_prefix_len(&[b'x', 0xe4, 0xbd]), 1);
        // 4-byte lead alone, and with 1 and 2 continuations → all held.
        assert_eq!(utf8_ready_prefix_len(&[b'x', 0xf0]), 1);
        assert_eq!(utf8_ready_prefix_len(&[b'x', 0xf0, 0x9f]), 1);
        assert_eq!(utf8_ready_prefix_len(&[b'x', 0xf0, 0x9f, 0x8e]), 1);
        // Same 4-byte char, fully present → nothing held.
        assert_eq!(utf8_ready_prefix_len(&[b'x', 0xf0, 0x9f, 0x8e, 0x89]), 5);
        // The realistic read-boundary case: a complete "é" (c3 a9) followed by
        // the lead byte of the next char → hold only that fresh lead.
        assert_eq!(utf8_ready_prefix_len(&[0xc3, 0xa9, 0xe6]), 2);
    }

    #[test]
    fn utf8_ready_prefix_does_not_buffer_garbage() {
        // Continuation bytes with no lead in the last 3 → pass through (no
        // unbounded carry).
        assert_eq!(utf8_ready_prefix_len(&[0x80, 0x80, 0x80, 0x80]), 4);
    }

    mod utf8_chunking {
        use proptest::prelude::*;

        use super::utf8_ready_prefix_len;

        fn rows(p: &vt100::Parser) -> Vec<String> {
            let s = p.screen();
            let (r, c) = s.size();
            let (r, c) = (r.get(), c.get());
            (0..r)
                .map(|y| {
                    let mut t = String::new();
                    for x in 0..c {
                        let sym = s.cell(y, x).map(|cl| cl.contents()).unwrap_or_default();
                        t.push_str(if sym.is_empty() { " " } else { sym });
                    }
                    t.trim_end().to_string()
                })
                .collect()
        }

        fn whole(bytes: &[u8]) -> Vec<String> {
            let mut p = vt100::Parser::new(10.try_into().unwrap(), 38.try_into().unwrap(), 0);
            p.process(bytes);
            rows(&p)
        }

        fn carry_chunked(bytes: &[u8], sizes: &[usize]) -> Vec<String> {
            let mut p = vt100::Parser::new(10.try_into().unwrap(), 38.try_into().unwrap(), 0);
            let mut carry: Vec<u8> = Vec::new();
            let (mut pos, mut i) = (0usize, 0usize);
            while pos < bytes.len() {
                let sz = sizes.get(i % sizes.len()).copied().unwrap_or(1).max(1);
                let end = (pos + sz).min(bytes.len());
                let mut data = std::mem::take(&mut carry);
                data.extend_from_slice(&bytes[pos..end]);
                let ready = utf8_ready_prefix_len(&data);
                carry = data.split_off(ready);
                assert!(carry.len() <= 3, "carry must stay bounded");
                if !data.is_empty() {
                    p.process(&data);
                }
                pos = end;
                i += 1;
            }
            if !carry.is_empty() {
                p.process(&carry);
            }
            rows(&p)
        }

        #[test]
        fn regression_midcodepoint_newline_widechars() {
            // f é \n 日本語. Chunked [2, 100] so the first read ends on "f" plus
            // the lead byte of "é" and the rest arrives next.
            let bytes = b"f\xc3\xa9\n\xe6\x97\xa5\xe6\x9c\xac\xe8\xaa\x9e";
            assert_eq!(carry_chunked(bytes, &[2, 100]), whole(bytes));
        }

        fn valid_utf8_output() -> impl Strategy<Value = Vec<u8>> {
            let token = prop_oneof![
                proptest::string::string_regex("[ -~]{0,8}")
                    .unwrap()
                    .prop_map(String::into_bytes),
                (
                    proptest::string::string_regex("[0-9;]{0,6}").unwrap(),
                    prop::sample::select(vec![b'm', b'H', b'J', b'K', b'A', b'B']),
                )
                    .prop_map(|(params, fin)| {
                        let mut v = vec![0x1b, b'['];
                        v.extend(params.bytes());
                        v.push(fin);
                        v
                    }),
                proptest::string::string_regex("[ -~]{0,8}")
                    .unwrap()
                    .prop_map(|s| {
                        let mut v = vec![0x1b, b']'];
                        v.extend(s.bytes());
                        v.push(0x07);
                        v
                    }),
                prop::sample::select(vec!["你好", "🎉", "café", "日本語", "→★", "a\u{0301}"])
                    .prop_map(|s| s.as_bytes().to_vec()),
                prop::sample::select(vec![b'\n', b'\r', b'\t', 0x08]).prop_map(|b| vec![b]),
            ];
            prop::collection::vec(token, 0..40).prop_map(|tokens| tokens.concat())
        }

        proptest! {
            #[test]
            fn carry_makes_chunking_invariant(
                bytes in valid_utf8_output(),
                sizes in prop::collection::vec(1usize..40, 1..16),
            ) {
                prop_assert_eq!(carry_chunked(&bytes, &sizes), whole(&bytes));
            }
        }
    }

    #[test]
    fn wire_mode_adopt_starts_stale_spawn_starts_fresh() {
        // Mirrors `App::refresh_session_statuses`: a session is `Busy` while
        // `now - last_output_at <= ACTIVITY_TIMEOUT_MS` (1000 ms in app/mod.rs).
        const ACTIVITY_TIMEOUT_MS: u64 = 1000;

        // Adopt: stale timestamp so the post-adopt SIGWINCH repaint doesn't
        // read as activity → NOT busy.
        let adopt = initial_output_at(WireMode::Adopt);
        assert!(now_millis().saturating_sub(adopt) > ACTIVITY_TIMEOUT_MS);

        // Spawn: "now" so a fresh process counts as active → busy.
        let spawn = initial_output_at(WireMode::Spawn);
        assert!(now_millis().saturating_sub(spawn) <= ACTIVITY_TIMEOUT_MS);
    }

    #[test]
    fn reader_loop_does_not_stamp_output_for_replayed_scrollback() {
        let parser = Arc::new(Mutex::new(vt100::Parser::new_with_callbacks(
            24.try_into().unwrap(),
            80.try_into().unwrap(),
            0,
            TermSignals::default(),
        )));
        let exited = Arc::new(AtomicBool::new(false));
        let last_output_at = Arc::new(AtomicU64::new(initial_output_at(WireMode::Adopt)));

        let seed = b"replayed history\r\n$ ".to_vec();
        let seed_len = seed.len();
        Session::reader_loop(
            output(vec![seed.into()]),
            Arc::clone(&parser),
            ReaderSignals {
                exited: Arc::clone(&exited),
                last_output_at: Arc::clone(&last_output_at),
                output_seq: Arc::new(AtomicU64::new(0)),
            },
            Arc::default(),
            seed_len,
            None,
        );

        assert_eq!(
            last_output_at.load(Ordering::Relaxed),
            initial_output_at(WireMode::Adopt),
            "scrollback replay alone must not read as activity"
        );
    }

    #[test]
    fn a_pane_is_released_once_when_its_sizer_goes() {
        let size = PaneSize::default();
        size.set_sized_elsewhere(false);
        assert!(!size.take_released(), "nobody was sizing it");
        size.set_sized_elsewhere(true);
        assert!(!size.take_released());
        size.set_sized_elsewhere(false);
        assert!(size.take_released());
        assert!(!size.take_released(), "taken once");
    }

    #[test]
    fn reader_loop_resizes_the_grid_where_the_stream_says() {
        let parser = Arc::new(Mutex::new(vt100::Parser::new_with_callbacks(
            4.try_into().unwrap(),
            20.try_into().unwrap(),
            0,
            TermSignals::default(),
        )));
        let size = PaneSize::default();
        // Reported as the backend's stream does, as the chunk is handed over.
        let reports = size.clone();
        let script = vec![
            b"aaaaaaaaaaaaaaa\r\n".to_vec().into(),
            PaneChunk::Resized { rows: 4, cols: 10 },
            b"bbbbbbbbbbbbbbb".to_vec().into(),
        ]
        .into_iter()
        .inspect(move |chunk| {
            if let PaneChunk::Resized { rows, cols } = chunk {
                reports.report(*rows, *cols);
            }
        });
        Session::reader_loop(
            Box::new(script),
            Arc::clone(&parser),
            ReaderSignals {
                exited: Arc::new(AtomicBool::new(false)),
                last_output_at: Arc::new(AtomicU64::new(0)),
                output_seq: Arc::new(AtomicU64::new(0)),
            },
            Arc::default(),
            0,
            Some(size.clone()),
        );

        let parser = parser.lock().unwrap();
        assert_eq!(
            parser.screen().size(),
            (4.try_into().unwrap(), 10.try_into().unwrap())
        );
        let rows: Vec<String> = parser.screen().rows(0, 10).collect();
        assert_eq!(rows[..3], ["aaaaaaaaaa", "bbbbbbbbbb", "bbbbb"]);
        assert_eq!(size.current(), pack_size(4, 10));
    }

    #[test]
    fn reader_loop_replaces_a_resident_grid_with_a_resync() {
        let snapshot = || {
            Box::new(PaneSnapshot {
                cols: 20,
                rows: 4,
                cursor: (9, 0),
                normal: vec!["\x1b[32mRECOVERED\x1b[0m".into()],
                alternate: None,
            })
        };
        fn bytes(b: &[u8]) -> PaneChunk {
            PaneChunk::Output(b.to_vec())
        }
        let run = |script: Vec<PaneChunk>| {
            let parser = Arc::new(Mutex::new(vt100::Parser::new_with_callbacks(
                4.try_into().unwrap(),
                20.try_into().unwrap(),
                0,
                TermSignals::default(),
            )));
            Session::reader_loop(
                output(script),
                Arc::clone(&parser),
                ReaderSignals {
                    exited: Arc::new(AtomicBool::new(false)),
                    last_output_at: Arc::new(AtomicU64::new(0)),
                    output_seq: Arc::new(AtomicU64::new(0)),
                },
                Arc::default(),
                0,
                None,
            );
            let parser = parser.lock().unwrap();
            let screen = parser.screen();
            (screen.contents(), screen.cell(0, 0).unwrap().fgcolor())
        };

        // `\x1b[` and then, after the gap, `!`: parsed as one sequence.
        let (contents, _) = run(vec![bytes(b"old\x1b["), bytes(b"!")]);
        assert_eq!(contents, "old", "the gap corrupts the grid");

        let (contents, colour) = run(vec![
            bytes(b"old\x1b["),
            PaneChunk::Resync(snapshot()),
            bytes(b"!"),
        ]);
        assert_eq!(contents, "RECOVERED!");
        assert_eq!(colour, vt100::Color::Idx(2));

        let (contents, _) = run(vec![bytes(b"old"), PaneChunk::Snapshot(snapshot())]);
        assert_eq!(contents, "old", "a plain snapshot keeps a resident grid");

        // A character's first byte held from before the gap must not join
        // bytes that follow the resync, even ones that would complete it.
        let (contents, _) = run(vec![
            bytes(b"old\xc3"),
            PaneChunk::Resync(snapshot()),
            bytes(b"\xa9!"),
        ]);
        assert!(contents.starts_with("RECOVERED"), "{contents:?}");
        assert!(contents.ends_with('!'), "{contents:?}");
        assert!(
            !contents.contains('\u{e9}'),
            "joined across the gap: {contents:?}"
        );
    }

    #[test]
    fn reader_loop_moves_the_output_seq_after_each_parse_seed_included() {
        let parser = Arc::new(Mutex::new(vt100::Parser::new_with_callbacks(
            24.try_into().unwrap(),
            80.try_into().unwrap(),
            0,
            TermSignals::default(),
        )));
        let seq = Arc::new(AtomicU64::new(0));
        let seed = b"replayed history\r\n$ ".to_vec();
        let seed_len = seed.len();
        Session::reader_loop(
            output(vec![seed.into()]),
            Arc::clone(&parser),
            ReaderSignals {
                exited: Arc::new(AtomicBool::new(false)),
                last_output_at: Arc::new(AtomicU64::new(initial_output_at(WireMode::Adopt))),
                output_seq: Arc::clone(&seq),
            },
            Arc::default(),
            seed_len,
            None,
        );
        assert!(
            seq.load(Ordering::Acquire) > 0,
            "the seed is content to paint"
        );
        assert!(parser
            .lock()
            .unwrap()
            .screen()
            .contents()
            .contains("replayed history"));
    }

    #[test]
    fn reader_loop_waits_for_a_split_character_before_moving_output_seq() {
        struct SplitReader {
            step: u8,
            split: std::sync::mpsc::Sender<()>,
            release: std::sync::mpsc::Receiver<()>,
        }

        impl Iterator for SplitReader {
            type Item = PaneChunk;
            fn next(&mut self) -> Option<PaneChunk> {
                match self.step {
                    0 => {
                        self.step = 1;
                        Some(vec![0xc3].into())
                    }
                    1 => {
                        self.split.send(()).expect("announce split");
                        self.release.recv().expect("release split");
                        self.step = 2;
                        Some(vec![0xa9].into())
                    }
                    _ => None,
                }
            }
        }

        let (split_tx, split_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let parser = Arc::new(Mutex::new(vt100::Parser::new_with_callbacks(
            24.try_into().unwrap(),
            80.try_into().unwrap(),
            0,
            TermSignals::default(),
        )));
        let seq = Arc::new(AtomicU64::new(0));
        let parser_in_reader = Arc::clone(&parser);
        let seq_in_reader = Arc::clone(&seq);
        let reader = std::thread::spawn(move || {
            Session::reader_loop(
                Box::new(SplitReader {
                    step: 0,
                    split: split_tx,
                    release: release_rx,
                }),
                parser_in_reader,
                ReaderSignals {
                    exited: Arc::new(AtomicBool::new(false)),
                    last_output_at: Arc::new(AtomicU64::new(0)),
                    output_seq: seq_in_reader,
                },
                Arc::default(),
                0,
                None,
            );
        });

        split_rx
            .recv_timeout(std::time::Duration::from_secs(1))
            .expect("reader reached the split");
        assert_eq!(
            seq.load(Ordering::Acquire),
            0,
            "a buffered character is not parsed output"
        );
        assert!(parser.lock().unwrap().screen().contents().is_empty());

        release_tx.send(()).expect("finish character");
        reader.join().expect("reader loop");
        assert!(parser.lock().unwrap().screen().contents().contains('é'));
        assert!(seq.load(Ordering::Acquire) > 0);
    }

    #[test]
    fn reader_loop_carries_a_split_character_and_marks_the_end() {
        let parser = Arc::new(Mutex::new(vt100::Parser::new_with_callbacks(
            4.try_into().unwrap(),
            20.try_into().unwrap(),
            0,
            TermSignals::default(),
        )));
        let exited = Arc::new(AtomicBool::new(false));
        let last_output_at = Arc::new(AtomicU64::new(0));

        // "é" is 0xC3 0xA9: the first read ends on its lead byte, and the
        // stream ends on the lead byte of "日".
        let reader = output(vec![
            b"f\xc3".to_vec().into(),
            b"\xa9\r\nok\xe6".to_vec().into(),
        ]);
        Session::reader_loop(
            reader,
            Arc::clone(&parser),
            ReaderSignals {
                exited: Arc::clone(&exited),
                last_output_at: Arc::clone(&last_output_at),
                output_seq: Arc::new(AtomicU64::new(0)),
            },
            Arc::default(),
            0,
            None,
        );

        let screen = parser.lock().unwrap().screen().contents();
        assert!(screen.starts_with("fé\nok"), "{screen:?}");
        assert!(exited.load(Ordering::SeqCst));
        assert!(last_output_at.load(Ordering::Relaxed) > 0);
    }

    #[test]
    fn reader_loop_stamps_output_for_live_bytes_after_the_seed() {
        let parser = Arc::new(Mutex::new(vt100::Parser::new_with_callbacks(
            24.try_into().unwrap(),
            80.try_into().unwrap(),
            0,
            TermSignals::default(),
        )));
        let exited = Arc::new(AtomicBool::new(false));
        let last_output_at = Arc::new(AtomicU64::new(initial_output_at(WireMode::Adopt)));

        let seed = b"replayed history\r\n$ ".to_vec();
        let seed_len = seed.len();
        let reader = output(vec![seed.into(), b"fresh agent output\r\n".to_vec().into()]);
        Session::reader_loop(
            reader,
            Arc::clone(&parser),
            ReaderSignals {
                exited: Arc::clone(&exited),
                last_output_at: Arc::clone(&last_output_at),
                output_seq: Arc::new(AtomicU64::new(0)),
            },
            Arc::default(),
            seed_len,
            None,
        );

        let stamped = last_output_at.load(Ordering::Relaxed);
        assert!(
            now_millis().saturating_sub(stamped) < 5_000,
            "live output past the seed boundary must stamp fresh activity"
        );
    }

    #[test]
    fn adopted_scrollback_replay_does_not_outlive_a_standing_blocked_state() {
        let parser = Arc::new(Mutex::new(vt100::Parser::new_with_callbacks(
            24.try_into().unwrap(),
            80.try_into().unwrap(),
            0,
            TermSignals::default(),
        )));
        let exited = Arc::new(AtomicBool::new(false));
        let last_output_at = Arc::new(AtomicU64::new(initial_output_at(WireMode::Adopt)));

        let seed = b"...\r\nPermission needed to run `rm -rf build`\r\n".to_vec();
        let seed_len = seed.len();
        Session::reader_loop(
            output(vec![seed.into()]),
            parser,
            ReaderSignals {
                exited,
                last_output_at: Arc::clone(&last_output_at),
                output_seq: Arc::new(AtomicU64::new(0)),
            },
            Arc::default(),
            seed_len,
            None,
        );

        let quiet_for_ms = now_millis().saturating_sub(last_output_at.load(Ordering::Relaxed));
        let state_age_ms = 3 * 60 * 60 * 1000; // hook_state_at stamped 3 hours ago

        assert!(
            quiet_for_ms >= state_age_ms,
            "a restart replaying scrollback must not make a genuinely blocked session look \
             recently active: quiet for {quiet_for_ms}ms against a state {state_age_ms}ms old"
        );
    }

    #[test]
    fn title_capture_extracts_osc_title() {
        let title = Arc::new(Mutex::new(None));
        let mut parser = vt100::Parser::new_with_callbacks(
            24.try_into().unwrap(),
            80.try_into().unwrap(),
            0,
            TermSignals {
                title: Arc::clone(&title),
                ..Default::default()
            },
        );
        // OSC 2 (set window title), BEL-terminated.
        parser.process(b"\x1b]2;working on tests\x07");
        assert_eq!(title.lock().unwrap().as_deref(), Some("working on tests"));

        // OSC 0 (set icon name + title) updates it too.
        parser.process(b"\x1b]0;done\x07");
        assert_eq!(title.lock().unwrap().as_deref(), Some("done"));

        // ST-terminated, which is what the adopt-time replay of a pane title
        // emits (`title_seed_bytes`) — the restored activity line depends on
        // this arriving through the same callback a live title does.
        parser.process(b"\x1b]2;restored from the pane title\x1b\\");
        assert_eq!(
            title.lock().unwrap().as_deref(),
            Some("restored from the pane title")
        );

        // An empty title clears the cell rather than storing "".
        parser.process(b"\x1b]2;\x07");
        assert_eq!(*title.lock().unwrap(), None);
    }

    #[test]
    fn attention_signals_are_captured() {
        let attention_at = Arc::new(AtomicU64::new(0));
        let notification = Arc::new(Mutex::new(None));
        let mut parser = vt100::Parser::new_with_callbacks(
            24.try_into().unwrap(),
            80.try_into().unwrap(),
            0,
            TermSignals {
                attention_at: Arc::clone(&attention_at),
                notification: Arc::clone(&notification),
                ..Default::default()
            },
        );

        assert_eq!(attention_at.load(Ordering::Relaxed), 0);

        // Terminal bell → attention, no message.
        parser.process(b"\x07");
        assert!(attention_at.load(Ordering::Relaxed) > 0);
        assert_eq!(*notification.lock().unwrap(), None);

        // OSC 9 desktop notification → attention + message text.
        parser.process(b"\x1b]9;Claude is waiting for your input\x07");
        assert_eq!(
            notification.lock().unwrap().as_deref(),
            Some("Claude is waiting for your input")
        );

        // OSC 777 notify form → attention + joined title/body.
        parser.process(b"\x1b]777;notify;Claude;Task done\x07");
        assert_eq!(
            notification.lock().unwrap().as_deref(),
            Some("Claude: Task done")
        );
    }
}
