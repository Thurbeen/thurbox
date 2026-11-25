use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;

use anyhow::Result;

#[derive(Debug)]
pub enum BackendError {
    Absent(String),
    Unreachable(String),
    Refused(String),
    Busy(String),
    Io(String),
}

impl std::fmt::Display for BackendError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Absent(m)
            | Self::Unreachable(m)
            | Self::Refused(m)
            | Self::Busy(m)
            | Self::Io(m) => f.write_str(m),
        }
    }
}
impl std::error::Error for BackendError {}
impl From<std::io::Error> for BackendError {
    fn from(e: std::io::Error) -> Self {
        let message = e.to_string();
        match e.kind() {
            std::io::ErrorKind::PermissionDenied => Self::Refused(message),
            std::io::ErrorKind::ConnectionRefused
            | std::io::ErrorKind::ConnectionReset
            | std::io::ErrorKind::NotConnected
            | std::io::ErrorKind::BrokenPipe
            | std::io::ErrorKind::TimedOut => Self::Unreachable(message),
            _ => Self::Io(message),
        }
    }
}
impl From<anyhow::Error> for BackendError {
    fn from(e: anyhow::Error) -> Self {
        if let Some(classified) = e.downcast_ref::<Self>() {
            let message = format!("{e:#}");
            return match classified {
                Self::Absent(_) => Self::Absent(message),
                Self::Unreachable(_) => Self::Unreachable(message),
                Self::Refused(_) => Self::Refused(message),
                Self::Busy(_) => Self::Busy(message),
                Self::Io(_) => Self::Io(message),
            };
        }
        if e.chain()
            .any(|cause| cause.is::<std::sync::mpsc::RecvTimeoutError>())
        {
            return Self::Unreachable(format!("{e:#}"));
        }
        if let Some(io) = e.downcast_ref::<std::io::Error>() {
            return Self::from(std::io::Error::new(io.kind(), format!("{e:#}")));
        }
        Self::Io(format!("{e:#}"))
    }
}
pub type BackendResult<T> = std::result::Result<T, BackendError>;

#[derive(Clone, Default)]
pub struct PaneSize(Arc<PaneSizeCells>);

#[derive(Default)]
struct PaneSizeCells {
    pending: AtomicU32,
    current: AtomicU32,
    elsewhere: AtomicBool,
    released: AtomicBool,
}

pub(crate) fn pack_size(rows: u16, cols: u16) -> u32 {
    (u32::from(rows) << 16) | u32::from(cols)
}

pub(crate) fn unpack_size(packed: u32) -> Option<(u16, u16)> {
    (packed != 0).then_some(((packed >> 16) as u16, packed as u16))
}

impl PaneSize {
    pub fn report(&self, rows: u16, cols: u16) {
        let packed = pack_size(rows, cols);
        self.0.pending.store(packed, Ordering::Relaxed);
        self.0.current.store(packed, Ordering::Relaxed);
    }

    pub fn set_sized_elsewhere(&self, elsewhere: bool) {
        if self.0.elsewhere.swap(elsewhere, Ordering::Relaxed) && !elsewhere {
            self.0.released.store(true, Ordering::Relaxed);
        }
    }

    pub fn sized_elsewhere(&self) -> bool {
        self.0.elsewhere.load(Ordering::Relaxed)
    }

    pub(crate) fn take(&self) -> Option<(u16, u16)> {
        unpack_size(self.0.pending.swap(0, Ordering::Relaxed))
    }

    pub(crate) fn current(&self) -> u32 {
        self.0.current.load(Ordering::Relaxed)
    }

    pub fn last_reported(&self) -> Option<(u16, u16)> {
        unpack_size(self.current())
    }

    pub(crate) fn take_released(&self) -> bool {
        self.0.released.load(Ordering::Relaxed) && self.0.released.swap(false, Ordering::Relaxed)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaneSnapshot {
    pub cols: u16,
    pub rows: u16,
    pub cursor: (u16, u16),
    pub normal: Vec<String>,
    pub alternate: Option<Vec<String>>,
}

#[derive(Debug, Clone)]
pub enum PaneChunk {
    Output(Vec<u8>),
    Snapshot(Box<PaneSnapshot>),
    Resync(Box<PaneSnapshot>),
    Resized { rows: u16, cols: u16 },
    SizedElsewhere(bool),
    State(Vec<u8>),
}

impl From<Vec<u8>> for PaneChunk {
    fn from(bytes: Vec<u8>) -> Self {
        Self::Output(bytes)
    }
}

pub type PaneOutput = Box<dyn Iterator<Item = PaneChunk> + Send>;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum WindowRole {
    Agent,
    Shell,
    Program,
}

impl WindowRole {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Agent => "agent",
            Self::Shell => "shell",
            Self::Program => "program",
        }
    }

    pub(crate) fn parse(value: &str) -> Option<Self> {
        match value {
            "agent" => Some(Self::Agent),
            "shell" => Some(Self::Shell),
            "program" => Some(Self::Program),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Located {
    At(String),
    Absent,
    Unknown,
}

impl Located {
    pub fn pane(self) -> Option<String> {
        match self {
            Self::At(pane) => Some(pane),
            _ => None,
        }
    }

    pub fn is_absent(&self) -> bool {
        matches!(self, Self::Absent)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Placed {
    pub agent: Located,
    pub shell: Located,
}

#[derive(Clone, Copy, Debug)]
pub struct Owner<'a> {
    pub session_id: &'a str,
    pub name: &'a str,
    pub agent_pane: &'a str,
    pub shell_pane: &'a str,
}

impl<'a> Owner<'a> {
    pub fn new(session_id: &'a str, name: &'a str) -> Self {
        Self {
            session_id,
            name,
            agent_pane: "",
            shell_pane: "",
        }
    }

    pub fn remembering(self, agent_pane: &'a str, shell_pane: &'a str) -> Self {
        Self {
            agent_pane: agent_pane.trim(),
            shell_pane: shell_pane.trim(),
            ..self
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaneId(String);

impl PaneId {
    pub fn new(id: impl Into<String>) -> Result<Self> {
        let id = id.into();
        anyhow::ensure!(
            !id.trim().is_empty(),
            "the backend named no pane for the new window"
        );
        Ok(Self(id))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::ops::Deref for PaneId {
    type Target = str;
    fn deref(&self) -> &str {
        &self.0
    }
}

impl From<PaneId> for String {
    fn from(id: PaneId) -> Self {
        id.0
    }
}

impl std::fmt::Display for PaneId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

pub struct WindowSpec<'a> {
    pub owner: Owner<'a>,
    pub role: WindowRole,
    pub command: &'a str,
    pub args: &'a [String],
    pub cwd: Option<&'a Path>,
    pub env: &'a HashMap<String, String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Key(String);

impl Key {
    pub const NAMED: &[&str] = &[
        "enter",
        "escape",
        "tab",
        "backspace",
        "space",
        "up",
        "down",
        "left",
        "right",
        "home",
        "end",
        "page-up",
        "page-down",
        "delete",
    ];

    const ALIASES: &[(&str, &str)] = &[
        ("return", "enter"),
        ("esc", "escape"),
        ("bspace", "backspace"),
        ("pageup", "page-up"),
        ("pgup", "page-up"),
        ("pagedown", "page-down"),
        ("pgdn", "page-down"),
        ("del", "delete"),
    ];

    pub fn parse(input: &str) -> Option<Self> {
        let lower = input.trim().to_ascii_lowercase();
        let lower = Self::ALIASES
            .iter()
            .find(|(alias, _)| *alias == lower)
            .map_or(lower.as_str(), |(_, canonical)| *canonical);
        if let Some(name) = Self::NAMED.iter().find(|name| **name == lower) {
            return Some(Self((*name).to_string()));
        }
        let rest = ["ctrl-", "ctrl+", "c-", "c+"]
            .iter()
            .find_map(|prefix| lower.strip_prefix(prefix))?;
        let mut chars = rest.chars();
        let letter = chars.next().filter(char::is_ascii_lowercase)?;
        if chars.next().is_some() {
            return None;
        }
        Some(Self(format!("ctrl-{letter}")))
    }

    pub fn name(&self) -> &str {
        &self.0
    }

    pub fn ctrl(&self) -> Option<char> {
        self.0
            .strip_prefix("ctrl-")
            .and_then(|rest| rest.chars().next())
    }
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct PaneState {
    pub cursor_row: Option<u32>,
    pub cursor_col: Option<u32>,
    pub foreground_process: Option<String>,
    pub foreground_command: Option<String>,
    pub foreground_cwd: Option<String>,
    pub dead: Option<bool>,
    pub exit_status: Option<i64>,
    pub reaped: Option<bool>,
    pub title: Option<String>,
}

#[derive(Clone)]
pub struct DiscoveredSession {
    pub backend_id: String,
    pub name: String,
    pub is_alive: bool,
    pub session: String,
    pub role: WindowRole,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendLiveness {
    Live,
    Exited,
    Missing,
    Unreachable,
    Unknown,
}

impl BackendLiveness {
    pub fn permits_relaunch(self) -> bool {
        matches!(self, Self::Missing)
    }
}

pub struct SpawnedSession {
    pub backend_id: String,
    pub output: PaneOutput,
    pub input: Box<dyn Write + Send>,
    pub size: Option<PaneSize>,
}

pub struct AdoptedSession {
    pub output: PaneOutput,
    pub input: Box<dyn Write + Send>,
    pub seed_len: usize,
    pub size: Option<PaneSize>,
}

pub trait PaneStream: Send + Sync {
    #[allow(clippy::too_many_arguments)]
    fn spawn(
        &self,
        window_name: &str,
        command: &str,
        args: &[String],
        cwd: Option<&Path>,
        env: &HashMap<String, String>,
        rows: u16,
        cols: u16,
    ) -> BackendResult<SpawnedSession>;

    fn adopt(
        &self,
        backend_id: &str,
        rows: u16,
        cols: u16,
        seed: Option<Vec<u8>>,
    ) -> BackendResult<AdoptedSession>;

    fn capture_history(&self, _backend_id: &str) -> BackendResult<Vec<u8>> {
        Ok(Vec::new())
    }

    fn request_snapshot(&self, _backend_id: &str) -> BackendResult<()> {
        Err(anyhow::anyhow!("this backend cannot snapshot a pane").into())
    }

    fn snapshot(&self, _backend_id: &str) -> BackendResult<PaneSnapshot> {
        Err(anyhow::anyhow!("this backend cannot snapshot a pane").into())
    }

    fn hide_output(&self, _backend_id: &str) {}

    fn output_times(&self) -> Result<std::collections::HashMap<String, u64>> {
        Ok(std::collections::HashMap::new())
    }

    fn state_seed(&self, _backend_id: &str) -> Vec<u8> {
        Vec::new()
    }

    fn resize(&self, backend_id: &str, rows: u16, cols: u16) -> BackendResult<()>;

    fn claim_size(&self, backend_id: &str, rows: u16, cols: u16) -> BackendResult<()> {
        self.resize(backend_id, rows, cols)
    }

    fn detach(&self, backend_id: &str) -> BackendResult<()>;
}

pub trait WindowLifecycle: Send + Sync {
    fn check_available(&self) -> BackendResult<()>;

    fn ensure_ready(&self) -> BackendResult<()>;

    fn discover(&self) -> BackendResult<Vec<DiscoveredSession>>;

    fn create_window(&self, spec: &WindowSpec<'_>) -> BackendResult<PaneId>;

    fn create_window_observed(
        &self,
        spec: &WindowSpec<'_>,
        settle: std::time::Duration,
    ) -> BackendResult<(PaneId, BackendLiveness)> {
        let pane = self.create_window(spec)?;
        std::thread::sleep(settle);
        let state = self.pane_state(pane.as_str())?;
        let located = match spec.role {
            WindowRole::Agent => self.locate(spec.owner)?.agent,
            WindowRole::Shell => self.locate(spec.owner)?.shell,
            WindowRole::Program => {
                if self.discover()?.iter().any(|window| {
                    window.backend_id == pane.as_str()
                        && window.session == spec.owner.session_id
                        && window.role == spec.role
                }) {
                    Located::At(pane.as_str().to_string())
                } else {
                    Located::Unknown
                }
            }
        };
        match located {
            Located::At(ref found) if found == pane.as_str() => {}
            Located::Absent => return Ok((pane, BackendLiveness::Missing)),
            _ => {
                return Err(BackendError::Refused(
                    "created window ownership could not be confirmed".into(),
                ))
            }
        }
        let liveness = match state.dead {
            Some(true) => BackendLiveness::Exited,
            Some(false) => BackendLiveness::Live,
            None => BackendLiveness::Unknown,
        };
        Ok((pane, liveness))
    }

    fn locate(&self, owner: Owner<'_>) -> BackendResult<Placed>;

    fn rename_windows(&self, owner: Owner<'_>, to: &str) -> BackendResult<()>;

    fn stamp_window(
        &self,
        backend_id: &str,
        session_id: &str,
        role: WindowRole,
    ) -> BackendResult<()>;

    fn window_panes(&self, window_name: &str) -> BackendResult<Vec<(String, bool)>>;

    fn set_pane_retention(&self, backend_id: &str, keep: bool) -> BackendResult<()>;

    fn pane_state(&self, pane: &str) -> BackendResult<PaneState>;

    fn reap_exited(&self) {}

    fn pane_path(&self, pane: &str) -> BackendResult<Option<String>>;

    fn is_dead(&self, backend_id: &str) -> BackendResult<bool>;

    fn kill(&self, backend_id: &str) -> BackendResult<()>;

    fn server_waiter(&self) -> Option<std::process::Command> {
        None
    }

    fn default_shell(&self) -> String;

    fn pane_pid(&self, backend_id: &str) -> BackendResult<Option<u32>>;

    fn pane_pids(&self) -> BackendResult<HashMap<String, u32>> {
        Err(anyhow::anyhow!("batched pane pid lookup not supported by this backend").into())
    }

    fn pane_ids(&self) -> BackendResult<HashSet<String>> {
        Err(anyhow::anyhow!("pane listing not supported by this backend").into())
    }

    fn shutdown(&self);
}

pub trait PaneIo: Send + Sync {
    fn send_text(&self, pane: &str, text: &str, submit: bool) -> BackendResult<()>;

    fn send_text_after(
        &self,
        pane: &str,
        text: &str,
        delay: std::time::Duration,
    ) -> BackendResult<()>;

    fn send_key(&self, pane: &str, key: &Key) -> BackendResult<String>;

    fn capture(&self, pane: &str, lines: u32, ansi: bool) -> BackendResult<String>;

    fn text_before_cursor(&self, _pane: &str) -> BackendResult<Option<String>> {
        Ok(None)
    }
}

pub trait StatusChannel: Send + Sync {
    fn hook_signal_command(&self) -> Option<String>;

    fn record_hook_state(&self, pane: &str, state: &str) -> BackendResult<()>;

    fn hook_states(&self) -> BackendResult<Vec<(String, String)>>;

    fn take_hook_state_events(&self) -> Vec<(String, String)>;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Capabilities {
    pub needs_liveness_poll: bool,
    pub supports_snapshots: bool,
    pub keeps_exited_windows: bool,
}

impl Capabilities {
    pub const NONE: Self = Self {
        needs_liveness_poll: true,
        supports_snapshots: false,
        keeps_exited_windows: true,
    };
}

pub trait SessionBackend: PaneStream + WindowLifecycle + PaneIo + StatusChannel {
    fn name(&self) -> &str;

    fn capabilities(&self) -> Capabilities;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_reply_timeout_is_unreachable_through_context() {
        let error =
            anyhow::Error::new(std::sync::mpsc::RecvTimeoutError::Timeout).context("list-windows");
        assert!(matches!(
            BackendError::from(error),
            BackendError::Unreachable(_)
        ));
    }

    #[test]
    fn every_named_key_parses_as_itself() {
        for name in Key::NAMED {
            assert_eq!(Key::parse(name).expect("a listed key").name(), *name);
        }
    }

    #[test]
    fn a_key_accepts_the_spellings_people_write() {
        for spelling in ["ctrl-c", "ctrl+c", "C-c", "c+C", " CTRL-C "] {
            let key = Key::parse(spelling).expect("{spelling} should parse");
            assert_eq!(key.name(), "ctrl-c");
            assert_eq!(key.ctrl(), Some('c'));
        }
        assert_eq!(Key::parse("esc").unwrap().name(), "escape");
        assert_eq!(Key::parse("RETURN").unwrap().name(), "enter");
        assert_eq!(Key::parse("pgup").unwrap().name(), "page-up");
        assert_eq!(Key::parse("enter").unwrap().ctrl(), None);
    }

    #[test]
    fn a_key_refuses_what_a_multiplexer_would_type_as_text() {
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
            assert!(Key::parse(bad).is_none(), "{bad:?} should not parse");
        }
    }

    #[test]
    fn every_control_letter_parses() {
        for letter in 'a'..='z' {
            let key = Key::parse(&format!("ctrl-{letter}")).expect("ctrl-<letter>");
            assert_eq!(key.ctrl(), Some(letter));
        }
    }
}
