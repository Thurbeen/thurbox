use std::borrow::Cow;
use std::collections::HashMap;
use std::marker::PhantomData;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};

use anyhow::{bail, Context, Result};
use tracing::{debug, info, warn};

use crate::contract::{
    AdoptedSession, BackendLiveness, Capabilities, DiscoveredSession, Key, Owner, PaneChunk,
    PaneId, PaneIo, PaneSize, PaneState, PaneStream, Placed, SessionBackend, SpawnedSession,
    StatusChannel, WindowLifecycle, WindowRole, WindowSpec,
};
use crate::identity::{window_name_for, Located, WindowIndex, SHELL_WINDOW_PREFIX};
use crate::instance::{host_socket, known_host_socket, learned_host_socket, local_socket};
use crate::settings::MuxSettings;
use crate::shell::HostLauncher;
use crate::tmux_compat::control_mode::{
    self, is_broken_pipe, is_recv_timeout, mouse_seed_bytes, shell_escape, title_seed_bytes,
    ControlMode, ControlModeReader, ControlModeWriter, ControlPolicy, Missing, PaneInput, Refusal,
    SIZED_BY, SIZER_OPTION,
};
use crate::tmux_compat::transport::{SocketDir, TmuxTransport};
use crate::{HostDef, Multiplexer, Platform};

pub trait TmuxCompatible: Send + Sync + 'static {
    const MULTIPLEXER: Multiplexer;

    const WINDOW_OPTIONS: bool;

    const WINDOW_SETTINGS: bool;

    const WINDOW_EVENTS: bool;

    const EXIT_ZERO_ERRORS: bool = false;

    const SESSION_SCOPED_IDS: bool = false;

    const KILL_BY_PANE: bool = false;

    const PANE_MONITORING: bool;

    const SNAPSHOTS: bool;

    const CURSOR_ROW_CAPTURE: bool = false;

    const COMMAND_LISTS: bool;

    const COMMAND_LIST_SINGLE_REPLY: bool;

    const COLD_START_ATTEMPTS: usize = 1;

    const COLD_START_GRACE: std::time::Duration = std::time::Duration::ZERO;

    const RETRY_NO_SERVER_ERROR: bool = false;

    const BOOTSTRAP_SIZE_ARGS: &[&str] = &["-x", "80", "-y", "24"];

    const SESSION_PROBE_COMMAND: &str = "has-session";

    const SOCKET_DIR: bool = false;

    const ONE_SHOT_SPAWN_ANSWERS: bool;

    const SPAWN_ON_STDIN: bool = true;

    const SPAWN_SETTLE: Option<std::time::Duration> = None;

    const CONDITIONAL_RESIZE: bool;

    const SERVER_SCOPE: &str;

    const SERVER_OPTIONS_NEED_TARGET: bool = false;

    const DISPLAY_FLAGS: &[&str];

    const VERSION_FLOOR: Option<fn(&str, &str) -> Result<()>>;

    fn check_banner(banner: &str, socket: &str) -> Result<()>;

    fn check_listing(_listing: &str) -> Result<()> {
        Ok(())
    }

    fn session_config(session: &str) -> Vec<ConfigOption>;

    fn quote(arg: &str) -> String {
        shell_escape(arg)
    }

    fn answered_absent(answer: &str) -> Option<Missing> {
        tmux_answered_absent(answer)
    }

    fn env_flags(env: &HashMap<String, String>) -> String {
        env.iter()
            .map(|(k, v)| format!(" -e {}", shell_escape(&format!("{k}={v}"))))
            .collect()
    }

    fn private_env(
        _server: &Server<Self>,
        env: &HashMap<String, String>,
    ) -> Result<HashMap<String, String>>
    where
        Self: Sized,
    {
        Ok(env.clone())
    }

    fn discard_private_env(_server: &Server<Self>, _env: &HashMap<String, String>)
    where
        Self: Sized,
    {
    }

    fn window_command(
        server: &Server<Self>,
        window_name: &str,
        command: &str,
        args: &[String],
        env: &HashMap<String, String>,
    ) -> Result<String>
    where
        Self: Sized,
    {
        Ok(server.posix_window_command(window_name, command, args, task_path(env)))
    }

    fn push_window_program(
        cmd: &mut Command,
        _window_name: &str,
        command: &str,
        args: &[String],
        env: &HashMap<String, String>,
    ) -> Result<()> {
        push_posix_window_program(cmd, command, args, env);
        Ok(())
    }

    fn paste_args(target: &str, text: &str) -> Vec<Vec<String>> {
        let buffer = paste_buffer_name();
        let paste = bracketed_paste(text);
        let mut argvs: Vec<Vec<String>> =
            buffer_chunks(&paste, control_mode::SEND_KEYS_CHUNK_BYTES)
                .into_iter()
                .enumerate()
                .map(|(i, chunk)| {
                    let mut argv: Vec<String> = vec!["set-buffer".into()];
                    if i > 0 {
                        argv.push("-a".into());
                    }
                    argv.extend(["-b".into(), buffer.clone(), "--".into(), chunk.into()]);
                    argv
                })
                .collect();
        argvs.push(vec![
            "paste-buffer".into(),
            "-d".into(),
            "-r".into(),
            UNSANITISED_PASTE.into(),
            "-b".into(),
            buffer,
            "-t".into(),
            target.into(),
        ]);
        argvs
    }

    fn deferred_paste_script(mux: &str, socket: &str, target: &str, text: &str) -> Result<String> {
        let (mux, socket, target) = (
            shell_escape(mux),
            shell_escape(socket),
            shell_escape(target),
        );
        let paste: String = hex_paste(text)
            .into_iter()
            .map(|bytes| {
                format!(
                    "{mux} -L {socket} send-keys -t {target} -H {}; ",
                    bytes.join(" ")
                )
            })
            .collect();
        Ok(format!(
            "{paste}sleep 0.2; {mux} -L {socket} send-keys -t {target} Enter"
        ))
    }

    fn pane_input(_transport: &TmuxTransport, _socket: &str) -> Arc<dyn PaneInput> {
        Arc::new(HexKeys)
    }

    fn control_policy(transport: &TmuxTransport, session: &str) -> ControlPolicy;

    const HOOK_STATUS: bool;

    fn hook_signal_command(_server: &Server<Self>) -> String
    where
        Self: Sized,
    {
        format!(
            "{} set-option -p {} ",
            Self::MULTIPLEXER.name(),
            control_mode::REMOTE_HOOK_STATE_OPTION
        )
    }

    fn hook_poll_command(session: &str) -> String {
        format!(
            "list-panes -s -t {} -F {}",
            Self::quote(session),
            Self::quote(&hook_state_format())
        )
    }

    fn banner_version(banner: &str) -> Option<&str> {
        named_version(banner, &[Self::MULTIPLEXER.name()])
    }
}

fn hook_state_format() -> String {
    format!(
        "#{{pane_id}} #{{{}}}",
        control_mode::REMOTE_HOOK_STATE_OPTION
    )
}

pub(crate) fn named_version<'a>(banner: &'a str, names: &[&str]) -> Option<&'a str> {
    banner
        .lines()
        .rev()
        .find_map(|line| {
            let line = line.trim();
            names.iter().find_map(|name| {
                line.strip_prefix(name)
                    .and_then(|rest| rest.strip_prefix(' '))
            })
        })?
        .split_whitespace()
        .next()
}

pub(crate) fn parse_version(word: &str) -> Option<(u32, u32, u32)> {
    let mut parts = word.split('.').map(|part| {
        let digits: String = part.chars().take_while(char::is_ascii_digit).collect();
        digits.parse::<u32>().ok()
    });
    Some((
        parts.next()??,
        parts.next()??,
        parts.next().flatten().unwrap_or(0),
    ))
}

fn bracketed_paste(text: &str) -> String {
    format!("\x1b[200~{text}\x1b[201~")
}

// tmux 3.7 runs a paste through vis(3) without `-S`; older tmux rejects it and pastes raw.
const UNSANITISED_PASTE: &str = "-S";

fn paste_buffer_name() -> String {
    static PASTES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.subsec_nanos());
    format!(
        "runtime-mux-paste-{}-{}-{nanos}",
        std::process::id(),
        PASTES.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    )
}

// tmux reads an argument's trailing `;` as the end of the command.
fn buffer_chunks(text: &str, max: usize) -> Vec<&str> {
    let mut chunks = Vec::new();
    let mut start = 0;
    while start < text.len() {
        let mut end = (start + max).min(text.len());
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        while end < text.len() && text[start..end].ends_with(';') {
            end = text[..end].trim_end_matches(';').len();
            if end == start {
                end = text[start..]
                    .find(|c| c != ';')
                    .map_or(text.len(), |i| start + i);
                end += text[end..].chars().next().map_or(0, char::len_utf8);
            }
        }
        chunks.push(&text[start..end]);
        start = end;
    }
    chunks
}

// tmux refuses a command line past its message size.
fn hex_paste(text: &str) -> Vec<Vec<String>> {
    bracketed_paste(text)
        .as_bytes()
        .chunks(control_mode::SEND_KEYS_CHUNK_BYTES)
        .map(|chunk| chunk.iter().map(|b| format!("{b:02x}")).collect())
        .collect()
}

struct HexKeys;

impl PaneInput for HexKeys {
    fn send_keys(&self, pane_id: &str, buf: &[u8]) -> Vec<String> {
        control_mode::hex_send_keys_commands(pane_id, buf)
    }

    fn paste(&self, _pane_id: &str, _text: &str) -> Option<Result<()>> {
        None
    }
}

pub const TMUX_SESSION: &str = if cfg!(dev_build) {
    "thurbox-dev"
} else {
    "thurbox"
};

const DISCOVER_FORMAT: &str =
    "#{pane_id}|#{window_name}|#{pane_dead}|#{@thurbox_session}|#{@thurbox_role}";

const WINDOW_PANES_FORMAT: &str = "#{pane_id}|#{window_name}|#{pane_dead}";

fn named_panes(listing: &str, window_name: &str) -> Vec<(String, bool)> {
    let mut found = Vec::new();
    for line in listing.lines() {
        let parts: Vec<&str> = line.trim().splitn(3, '|').collect();
        if parts.len() < 3 || parts[1] != window_name {
            continue;
        }
        if !control_mode::is_valid_pane_id(parts[0]) {
            continue;
        }
        found.push((parts[0].to_string(), parse_pane_dead(parts[2])));
    }
    found
}

const SPAWN_FORMAT: &str = "#{pane_id} #{window_id}";

fn parse_discovered(line: &str, stamps: bool) -> Option<DiscoveredSession> {
    let mut parts = line.splitn(5, '|');
    let pane = parts.next()?;
    let name = parts.next()?;
    let dead = parts.next()?;
    // Where `#{@...}` is not a window option, every window reads one global value.
    let (session, role) = match stamps {
        true => (parts.next().unwrap_or(""), parts.next().unwrap_or("")),
        false => ("", ""),
    };

    let by_name = WindowRole::from_window_name(name)?;
    if !control_mode::is_valid_pane_id(pane) {
        warn!("Skipping discovered window with invalid pane id: {pane:?}");
        return None;
    }
    Some(DiscoveredSession {
        backend_id: pane.to_string(),
        name: name.to_string(),
        is_alive: !parse_pane_dead(dead),
        // A multiplexer that does not expand `#{@...}` hands the format string straight back.
        session: match session.parse::<crate::SessionId>() {
            Ok(id) => id.to_string(),
            Err(_) => String::new(),
        },
        role: WindowRole::parse(role).unwrap_or(by_name),
    })
}

// psmux numbers ids per session, so a bare `@7` can hit the `__warm__` session's window.
pub(crate) fn qualify_target<'a>(session: &str, target: &'a str, scoped: bool) -> Cow<'a, str> {
    if !scoped {
        return Cow::Borrowed(target);
    }
    if control_mode::is_valid_pane_id(target) {
        Cow::Owned(format!("{session}:.{target}"))
    } else if control_mode::is_valid_window_id(target) {
        Cow::Owned(format!("{session}:{target}"))
    } else {
        Cow::Borrowed(target)
    }
}

pub const WINDOW_SESSION_OPTION: &str = "@thurbox_session";

pub const WINDOW_ROLE_OPTION: &str = "@thurbox_role";

// tmux announces a pane's death only by closing its window.
fn keeps_dead_pane(window_name: &str) -> bool {
    matches!(
        WindowRole::from_window_name(window_name),
        Some(WindowRole::Agent)
    )
}

// A tmux command list runs to completion before the server can reap a pane.
fn birth_options(window_name: &str) -> [(&'static str, &'static str); 2] {
    [
        // A user's `~/.tmux.conf` is read on this socket and may set `remain-on-exit on`.
        (
            "remain-on-exit",
            if keeps_dead_pane(window_name) {
                "on"
            } else {
                "off"
            },
        ),
        // tmux 3.3–3.6 crash on a server-wide `window-size manual` with no client attached.
        ("window-size", "manual"),
    ]
}

fn birth_option_commands<M: TmuxCompatible>(window_name: &str) -> Vec<String> {
    if !M::WINDOW_SETTINGS {
        return Vec::new();
    }
    birth_options(window_name)
        .iter()
        .map(|(key, value)| format!("set-window-option {key} {value}"))
        .collect()
}

const RETIRE_FORMAT: &str = "#{window_id}|#{@thurbox_session}|#{@thurbox_role}";

fn stamped_windows_in(listing: &str, session_id: &str, role: WindowRole) -> Vec<String> {
    let mut found: Vec<(u64, String)> = listing
        .lines()
        .filter_map(|line| {
            let mut parts = line.split('|');
            let window = parts.next()?;
            let stamp = parts.next()?;
            let stamped_role = parts.next()?;
            if stamp != session_id || stamped_role != role.as_str() {
                return None;
            }
            Some((window.strip_prefix('@')?.parse().ok()?, window.to_string()))
        })
        .collect();
    found.sort_unstable();
    found.into_iter().map(|(_, window)| window).collect()
}

const SSH_ERROR_EXIT: i32 = 255;

const LOOP_COMMAND_BUDGET: std::time::Duration = std::time::Duration::from_millis(250);

const FRESH_SESSION_READY_WAIT: std::time::Duration = if cfg!(test) {
    std::time::Duration::from_millis(200)
} else {
    std::time::Duration::from_secs(5)
};

fn mux_answered<M: TmuxCompatible>(
    is_ssh: bool,
    code: Option<i32>,
    answer: &str,
) -> Option<Missing> {
    match code {
        // ssh's own error, so nothing on the host ever saw the question.
        Some(SSH_ERROR_EXIT) if is_ssh => None,
        // No exit code means a signal killed it, so stderr holds only a fragment.
        None => None,
        _ => M::answered_absent(answer),
    }
}

fn output_answered<M: TmuxCompatible>(
    is_ssh: bool,
    output: &std::process::Output,
) -> Option<Missing> {
    let stderr = mux_stderr(output, is_ssh);
    let mut answers = stderr
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty());
    let answer = answers.next()?;
    if answers.next().is_some() {
        return None;
    }
    mux_answered::<M>(is_ssh, output.status.code(), answer)
}

fn listing_output_is_absence<M: TmuxCompatible>(
    is_ssh: bool,
    output: &std::process::Output,
) -> bool {
    output.stdout.is_empty() && output_answered::<M>(is_ssh, output) == Some(Missing::Server)
}

// tmux prints `error connecting to` also for a live server whose socket it cannot open.
pub(crate) fn tmux_answered_absent(answer: &str) -> Option<Missing> {
    let names = |prefix: &str| {
        answer
            .strip_prefix(prefix)
            .is_some_and(|value| !value.is_empty())
    };
    if let Some(path) = answer.strip_prefix("error connecting to ") {
        // tmux ≥ 3.4's wording for a socket with no server behind it.
        return path
            .strip_suffix(" (No such file or directory)")
            .filter(|path| !path.is_empty())
            .map(|_| Missing::Server);
    }
    // tmux < 3.4's wording for the same thing, then a server up with no such session.
    if names("no server running on ") || names("can't find session: ") {
        return Some(Missing::Server);
    }
    if names("can't find pane: ") || names("can't find window: ") {
        return Some(Missing::Target);
    }
    None
}

pub struct ConfigOption {
    pub args: Vec<String>,
    pub fatal: bool,
}

impl ConfigOption {
    pub fn set(args: &[&str], fatal: bool) -> Self {
        let mut all = vec!["set-option".to_string()];
        all.extend(args.iter().map(ToString::to_string));
        Self { args: all, fatal }
    }
}

// tmux skips the rest of a list after a failure; `if-shell` refuses `-q`.
fn config_command_list<'a>(prefix: &[&'a str], config: &'a [ConfigOption]) -> Vec<&'a str> {
    let mut list = prefix.to_vec();
    for option in config {
        if !list.is_empty() {
            list.push(";");
        }
        let (verb, rest) = option.args.split_first().expect("a set-option verb");
        list.push(verb.as_str());
        if !option.fatal && verb == "set-option" {
            list.push("-q");
        }
        list.extend(rest.iter().map(String::as_str));
    }
    list
}

const SEND_KEYS_ENTER_DELAY: std::time::Duration = std::time::Duration::from_millis(200);

const MAX_CAPTURE_LINES: u32 = 10_000;

fn history_lines(scrollback_lines: usize) -> usize {
    scrollback_lines.min(MAX_CAPTURE_LINES as usize)
}

pub struct Server<M: TmuxCompatible> {
    pub(crate) transport: TmuxTransport,
    pub(crate) socket: String,
    pub(crate) session: String,
    pub(crate) name: String,
    pub(crate) control: Mutex<Option<Arc<ControlMode>>>,
    connecting: Mutex<()>,
    retiring: Mutex<Vec<std::sync::Weak<ControlMode>>>,
    pub(crate) closed: std::sync::atomic::AtomicBool,
    connection_generation: std::sync::atomic::AtomicU64,
    admitted: std::sync::atomic::AtomicU64,
    admitting: Mutex<Option<(u64, String)>>,
    questions: std::sync::atomic::AtomicU64,
    pub(crate) sizer: String,
    history_lines: usize,
    pub(crate) host: Option<HostDef>,
    pub(crate) platform: Platform,
    multiplexer: PhantomData<M>,
}

fn tmux_size(rows: u16, cols: u16) -> (u16, u16) {
    (rows.clamp(1, 10_000), cols.clamp(1, 10_000))
}

fn sizer_name() -> String {
    static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let nth = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .subsec_nanos();
    format!("{:x}-{nth:x}-{nanos:x}", std::process::id())
}

fn scopes_available() -> bool {
    static AVAILABLE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *AVAILABLE.get_or_init(|| {
        let user_manager_reachable = std::env::var_os("XDG_RUNTIME_DIR")
            .is_some_and(|dir| Path::new(&dir).join("systemd/private").exists());
        cfg!(target_os = "linux")
            && user_manager_reachable
            && in_own_scope(&Command::new("true"))
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .is_ok_and(|status| status.success())
    })
}

fn in_own_scope(command: &Command) -> Command {
    let mut scoped = Command::new("systemd-run");
    scoped
        .args(["--user", "--scope", "--collect", "--quiet", "--"])
        .arg(command.get_program())
        .args(command.get_args());
    for (key, value) in command.get_envs() {
        match value {
            Some(value) => scoped.env(key, value),
            None => scoped.env_remove(key),
        };
    }
    if let Some(dir) = command.get_current_dir() {
        scoped.current_dir(dir);
    }
    scoped
}

impl<M: TmuxCompatible> Server<M> {
    pub fn local(settings: &MuxSettings) -> Self {
        let name = crate::Route::local(M::MULTIPLEXER).format();
        let socket = local_socket(settings);
        let mut transport = TmuxTransport::local(M::MULTIPLEXER.name());
        if M::SOCKET_DIR && crate::shell::local_platform() == Platform::Posix {
            if let Some(dir) = crate::instance::local_socket_dir(&socket, settings) {
                transport = transport.with_socket_dir(SocketDir::Dir(dir.display().to_string()));
            }
        }
        Self::with_transport(
            transport,
            socket,
            TMUX_SESSION,
            name,
            settings.scrollback_lines,
        )
    }

    pub fn with_transport(
        transport: TmuxTransport,
        socket: impl Into<String>,
        session: impl Into<String>,
        name: impl Into<String>,
        scrollback_lines: usize,
    ) -> Self {
        let platform = if transport.is_remote() {
            Platform::Posix
        } else {
            crate::shell::local_platform()
        };
        Self {
            transport,
            socket: socket.into(),
            session: session.into(),
            name: name.into(),
            control: Mutex::new(None),
            connecting: Mutex::new(()),
            retiring: Mutex::new(Vec::new()),
            closed: std::sync::atomic::AtomicBool::new(false),
            connection_generation: std::sync::atomic::AtomicU64::new(0),
            admitted: std::sync::atomic::AtomicU64::new(0),
            admitting: Mutex::new(None),
            questions: std::sync::atomic::AtomicU64::new(0),
            sizer: sizer_name(),
            history_lines: history_lines(scrollback_lines),
            host: None,
            platform,
            multiplexer: PhantomData,
        }
    }

    pub fn for_host(host: &HostDef, settings: &MuxSettings) -> Self {
        Self::on_host(
            host,
            HostLauncher::for_host(host),
            host.platform(),
            settings,
        )
    }

    pub fn on_host(
        host: &HostDef,
        launcher: HostLauncher,
        platform: Platform,
        settings: &MuxSettings,
    ) -> Self {
        let session = host
            .session
            .clone()
            .unwrap_or_else(|| TMUX_SESSION.to_string());
        let mut transport = TmuxTransport::remote(launcher, M::MULTIPLEXER.name())
            .with_shell(crate::shell::HostShell::for_platform(platform));
        if M::SOCKET_DIR && platform == Platform::Posix {
            transport = transport.with_socket_dir(SocketDir::HostData {
                app: crate::instance::app_dir_name().to_string(),
            });
        }
        let mut backend = Self::with_transport(
            transport,
            host_socket(host),
            session,
            host.route(M::MULTIPLEXER).format(),
            settings.scrollback_lines,
        );
        backend.host = Some(host.clone());
        backend.platform = platform;
        backend
    }

    pub(crate) fn socket(&self) -> String {
        self.host
            .as_ref()
            .and_then(|host| learned_host_socket(&host.backend_name()))
            .unwrap_or_else(|| self.socket.clone())
    }

    pub(crate) fn mux_command(&self, args: &[&str]) -> Result<Command> {
        Ok(self
            .located_transport()
            .tmux_command(&self.socket(), args)?)
    }

    pub(crate) fn located_transport(&self) -> TmuxTransport {
        self.host_transport().with_socket_dir(self.socket_dir())
    }

    fn host_transport(&self) -> TmuxTransport {
        let path = match (&self.host, self.platform) {
            (Some(host), Platform::Posix) => crate::host_path::assignment_for(host),
            _ => None,
        };
        self.transport.clone().with_path(path)
    }

    fn manages_socket_dir(&self) -> bool {
        M::SOCKET_DIR && self.platform == Platform::Posix
    }

    fn preferred_socket_dir(&self) -> SocketDir {
        let learned = self
            .host
            .as_ref()
            .and_then(|host| crate::instance::learned_host_socket_dir(&host.backend_name()));
        match learned {
            Some(Some(dir)) => SocketDir::Dir(dir),
            Some(None) => SocketDir::Inherited,
            None => self.transport.socket_dir().clone(),
        }
    }

    pub(crate) fn socket_dir_candidates(&self) -> Vec<SocketDir> {
        let mut dirs = vec![self.preferred_socket_dir()];
        for dir in [self.transport.socket_dir().clone(), SocketDir::Inherited] {
            if !dirs.contains(&dir) {
                dirs.push(dir);
            }
        }
        dirs
    }

    fn located_key(&self) -> String {
        let machine = self
            .host
            .as_ref()
            .map_or_else(|| "local".to_string(), HostDef::backend_name);
        format!("{machine}\0{}", self.socket())
    }

    pub(crate) fn socket_dir(&self) -> SocketDir {
        if !self.manages_socket_dir() {
            return SocketDir::Inherited;
        }
        let key = self.located_key();
        if let Some(found) = located_dirs()
            .lock()
            .ok()
            .and_then(|m| m.get(&key).cloned())
        {
            return found.dir;
        }
        let (dir, searched, outcome) = match self.socket_file_seen() {
            Some(dir) => (dir, None, Search::Found),
            None => {
                let (dir, outcome) = self.find_server();
                (dir, Some(std::time::Instant::now()), outcome)
            }
        };
        if let Ok(mut map) = located_dirs().lock() {
            map.entry(key).or_insert(LocatedDir {
                dir: dir.clone(),
                searched,
                outcome,
            });
        }
        dir
    }

    fn relocate(&self, throttled: bool) -> Search {
        if !self.manages_socket_dir() {
            return Search::Absent;
        }
        let key = self.located_key();
        if throttled {
            let recent = located_dirs().lock().ok().and_then(|m| {
                m.get(&key)
                    .filter(|found| {
                        found
                            .searched
                            .is_some_and(|at| at.elapsed() < RELOCATE_EVERY)
                    })
                    .map(|found| found.outcome.clone())
            });
            if let Some(outcome) = recent {
                return match outcome {
                    Search::Stuck(why) => Search::Stuck(why),
                    Search::Found | Search::Absent => Search::Absent,
                };
            }
        }
        let (dir, outcome) = self.find_server();
        if let Ok(mut map) = located_dirs().lock() {
            map.insert(
                key,
                LocatedDir {
                    dir,
                    searched: Some(std::time::Instant::now()),
                    outcome: outcome.clone(),
                },
            );
        }
        outcome
    }

    fn out_of_reach(&self, why: &str) -> anyhow::Error {
        crate::contract::BackendError::Unreachable(format!(
            "{}: {why}; not starting a second server beside it \
             (see 'An orphaned tmux server' in docs/thurbox/config.md)",
            self.name
        ))
        .into()
    }

    fn found_after_all(&self) -> Result<bool> {
        if !self.manages_socket_dir() {
            return Ok(false);
        }
        let found = match self.relocate(false) {
            Search::Stuck(why) => return Err(self.out_of_reach(&why)),
            Search::Found => true,
            Search::Absent => false,
        };
        if found && self.session_exists() {
            return Ok(true);
        }
        let mux = self.transport.mux();
        let socket = self.socket();
        match found {
            true => info!(
                "{}: the {mux} server for socket '{socket}' has no session '{}'; creating it there",
                self.name, self.session
            ),
            false => info!(
                "{}: no {mux} server is running for socket '{socket}'; starting one in {}",
                self.name,
                self.socket_dir().describe()
            ),
        }
        Ok(false)
    }

    fn socket_file_seen(&self) -> Option<SocketDir> {
        let preferred = self.preferred_socket_dir();
        (!self.transport.is_remote() && local_socket_file(&preferred, &self.socket()))
            .then_some(preferred)
    }

    // macOS sweeps `/tmp`, orphaning a live server's socket; `SIGUSR1` makes tmux recreate it.
    fn find_server(&self) -> (SocketDir, Search) {
        let candidates = self.socket_dir_candidates();
        let preferred = candidates[0].clone();
        let socket = self.socket();
        let script = locate_script(&candidates, &socket);
        let output = self
            .host_transport()
            .shell_command(&script, &["sh", self.transport.mux(), &socket])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output();
        let output = match output {
            Ok(output) if output.status.success() => output,
            Ok(output) => {
                debug!(
                    "{}: could not look for the server of socket '{socket}': {}",
                    self.name,
                    mux_failure(&output, self.transport.is_ssh())
                );
                return (preferred, Search::Absent);
            }
            Err(e) => {
                debug!(
                    "{}: could not look for the server of socket '{socket}': {e}",
                    self.name
                );
                return (preferred, Search::Absent);
            }
        };
        let answer = String::from_utf8_lossy(&output.stdout);
        match parse_located(&answer) {
            ServerFound::Answered(at) => {
                let dir = candidates[at].clone();
                if at != 0 {
                    info!(
                        "{}: adopting the running {} server for socket '{socket}' in {} \
                         rather than starting one in {}; it moves there once this one exits",
                        self.name,
                        self.transport.mux(),
                        dir.describe(),
                        preferred.describe()
                    );
                }
                (dir, Search::Found)
            }
            ServerFound::Recovered {
                at,
                pid,
                others,
                path,
            } => {
                info!(
                    "{}: the {} server for socket '{socket}' (pid {pid}) had lost its \
                     socket {path}; recreated it with SIGUSR1 and reattached",
                    self.name,
                    self.transport.mux()
                );
                if others > 0 {
                    warn!(
                        "{}: {others} more server(s) are bound to {path} with no way back in; \
                         they are orphaned, and killing them is the only remedy",
                        self.name
                    );
                }
                (candidates[at].clone(), Search::Found)
            }
            ServerFound::Unanswered { at, pid, path } => {
                let why = format!(
                    "sent SIGUSR1 to the {} server for socket '{socket}' (pid {pid}) to \
                     recreate {path}, and it has not answered since",
                    self.transport.mux()
                );
                warn!("{}: {why}", self.name);
                (candidates[at].clone(), Search::Stuck(why))
            }
            ServerFound::Bound { pids, path } => {
                let why = format!(
                    "{path} exists but does not answer, and server(s) {pids} are still \
                     bound to it; not signalling them, which would orphan whichever owns it"
                );
                warn!("{}: {why}", self.name);
                (preferred, Search::Stuck(why))
            }
            ServerFound::Nothing => (preferred, Search::Absent),
        }
    }

    pub(crate) fn target<'a>(&self, id: &'a str) -> Result<Cow<'a, str>> {
        self.admit_server()?;
        Ok(self.spell_target(id))
    }

    pub(crate) fn spell_target<'a>(&self, id: &'a str) -> Cow<'a, str> {
        qualify_target(&self.session, id, M::SESSION_SCOPED_IDS)
    }

    pub(crate) fn kill_command<'a>(&self, id: &'a str) -> Result<(&'static str, Cow<'a, str>)> {
        self.admit_server()?;
        Ok(self.kill_spelling(id))
    }

    pub(crate) fn kill_spelling<'a>(&self, id: &'a str) -> (&'static str, Cow<'a, str>) {
        let verb = match M::KILL_BY_PANE && control_mode::is_valid_pane_id(id) {
            true => "kill-pane",
            false => "kill-window",
        };
        (verb, self.spell_target(id))
    }

    fn admitted_here(&self) -> bool {
        use std::sync::atomic::Ordering;
        M::VERSION_FLOOR.is_none()
            || self.admitted.load(Ordering::Acquire)
                == self.connection_generation.load(Ordering::Acquire) + 1
    }

    fn admit_server(&self) -> Result<()> {
        use std::sync::atomic::Ordering;
        let Some(refuse_old) = M::VERSION_FLOOR else {
            return Ok(());
        };
        let current = || self.connection_generation.load(Ordering::Acquire) + 1;
        if self.admitted.load(Ordering::Acquire) == current() {
            return Ok(());
        }
        let asked_before = self.questions.load(Ordering::Acquire);
        let mut last_failure = self
            .admitting
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let generation = current();
        if self.admitted.load(Ordering::Acquire) == generation {
            return Ok(());
        }
        if self.questions.load(Ordering::Acquire) != asked_before {
            if let Some((failed_at, failure)) = last_failure.as_ref() {
                if *failed_at == generation {
                    bail!("{failure}");
                }
            }
        }
        let attached = self.attached();
        let outcome = self.ask_versions(attached, refuse_old);
        *last_failure = outcome
            .as_ref()
            .err()
            .map(|e| (generation, format!("{e:#}")));
        self.questions.fetch_add(1, Ordering::AcqRel);
        if matches!(outcome?, Admission::Admitted) && attached {
            self.admitted.store(generation, Ordering::Release);
        }
        Ok(())
    }

    fn ask_versions(
        &self,
        attached: bool,
        refuse_old: fn(&str, &str) -> Result<()>,
    ) -> Result<Admission> {
        let mux = self.transport.mux();
        let banner = self.tmux_output(&["-V"])?;
        let Some(client) = M::banner_version(&banner).filter(|v| is_version_answer(v)) else {
            bail!(
                "the {mux} binary did not give a version thurbox can hold to its floor (it \
                 answered {banner:?}); put a released {mux} first on PATH"
            );
        };
        M::check_banner(client, &self.socket())?;
        let answer = match attached {
            true => self
                .ctrl_command("display-message -p '#{version}'")
                .context("could not ask the server its version")?,
            false => match self.ask_version_once()? {
                Some(answer) => answer,
                None => return Ok(Admission::NothingThere),
            },
        };
        let version = answer.trim();
        if !is_version_answer(version) {
            bail!("the server did not say its version (it answered {version:?})");
        }
        refuse_old(version, &self.socket())?;
        Ok(Admission::Admitted)
    }

    fn ask_version_once(&self) -> Result<Option<String>> {
        let args = ["display-message", "-t", &self.session, "-p", "#{version}"];
        let output = self
            .mux_command(&args)?
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .map_err(|e| {
                self.transport
                    .launch_failure("Failed to ask the server its version", e)
            })?;
        let is_ssh = self.transport.is_ssh();
        if listing_output_is_absence::<M>(is_ssh, &output) {
            return Ok(None);
        }
        if !output.status.success() || !mux_stderr(&output, is_ssh).trim().is_empty() {
            return Err(anyhow::Error::from(self.command_failure(&args, &output))
                .context("could not ask the server its version"));
        }
        let answer = String::from_utf8_lossy(&output.stdout).trim().to_string();
        // psmux reports some failures on stdout with status 0.
        if let Some(error) = answer
            .strip_prefix("ERROR:")
            .filter(|_| M::EXIT_ZERO_ERRORS)
        {
            if !error.contains('\n') && M::answered_absent(error.trim()) == Some(Missing::Server) {
                return Ok(None);
            }
            bail!("the server refused the version question: {answer}");
        }
        Ok(Some(answer))
    }

    fn refuse_without_hook_status(&self) -> Result<()> {
        if !M::HOOK_STATUS {
            bail!(
                "{} carries no hook status: its pane-option channel is not proven",
                self.name
            );
        }
        Ok(())
    }

    fn tmux_output(&self, args: &[&str]) -> Result<String> {
        let output = self.run_tmux(args)?;
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    }

    fn tmux_run(&self, args: &[&str]) -> Result<()> {
        self.run_tmux(args)?;
        Ok(())
    }

    pub(crate) fn stamps_are_per_window(&self) -> bool {
        M::WINDOW_OPTIONS
    }

    pub(crate) fn discover_answered(&self) -> Result<Vec<DiscoveredSession>> {
        if let Some(windows) = self.discover_answered_once()? {
            return Ok(windows);
        }
        match self.relocate(true) {
            Search::Found => Ok(self.discover_answered_once()?.unwrap_or_default()),
            Search::Stuck(why) => Err(self.out_of_reach(&why)),
            Search::Absent => Ok(Vec::new()),
        }
    }

    fn discover_answered_once(&self) -> Result<Option<Vec<DiscoveredSession>>> {
        let args = ["list-windows", "-t", &self.session, "-F", DISCOVER_FORMAT];
        let output = self
            .mux_command(&args)?
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .map_err(|e| {
                self.transport.launch_failure(
                    &format!("Failed to run {} command", M::MULTIPLEXER.name()),
                    e,
                )
            })?;
        if output.status.success()
            && !(M::EXIT_ZERO_ERRORS && mux_reported_error(&output, self.transport.is_ssh(), true))
        {
            M::check_listing(&String::from_utf8_lossy(&output.stdout))?;
            let stamps = self.stamps_are_per_window();
            return Ok(Some(
                String::from_utf8_lossy(&output.stdout)
                    .lines()
                    .filter_map(|line| parse_discovered(line, stamps))
                    .collect(),
            ));
        }
        // psmux hides some failures behind exit 0; its "no server running" exits 1.
        if output.status.success() {
            bail!(
                "{} {} failed: {}",
                M::MULTIPLEXER.name(),
                args.join(" "),
                mux_failure(&output, self.transport.is_ssh())
            );
        }
        if listing_output_is_absence::<M>(self.transport.is_ssh(), &output) {
            return Ok(None);
        }
        Err(self.command_failure(&args, &output).into())
    }

    fn transient(&self) -> Self {
        Self {
            transport: self.transport.clone(),
            socket: self.socket.clone(),
            session: self.session.clone(),
            name: self.name.clone(),
            control: Mutex::new(None),
            connecting: Mutex::new(()),
            retiring: Mutex::new(Vec::new()),
            closed: std::sync::atomic::AtomicBool::new(self.is_closed()),
            connection_generation: std::sync::atomic::AtomicU64::new(0),
            admitted: std::sync::atomic::AtomicU64::new(0),
            admitting: Mutex::new(None),
            questions: std::sync::atomic::AtomicU64::new(0),
            sizer: sizer_name(),
            history_lines: self.history_lines,
            host: self.host.clone(),
            platform: self.platform,
            multiplexer: PhantomData,
        }
    }

    fn is_closed(&self) -> bool {
        self.closed.load(std::sync::atomic::Ordering::Acquire)
    }

    fn refuse_if_closed(&self) -> Result<()> {
        if self.is_closed() {
            bail!("{} is shut down", self.name);
        }
        Ok(())
    }

    pub(crate) fn attached(&self) -> bool {
        self.control.lock().is_ok_and(|control| control.is_some())
    }

    fn known_socket(&self) -> Result<()> {
        match &self.host {
            Some(host) => known_host_socket(host).map(drop),
            None => Ok(()),
        }
    }

    fn create_remote_window(&self, spec: &WindowSpec<'_>) -> Result<PaneId> {
        let backend = self.transient();
        backend.check_available().with_context(|| {
            format!(
                "remote host is unreachable or {} is missing",
                M::MULTIPLEXER.name()
            )
        })?;
        backend.ensure_ready()?;
        let window_name = window_name_for(spec.role, spec.owner.name);
        let spawned = backend.spawn(
            &window_name,
            spec.command,
            spec.args,
            spec.cwd,
            spec.env,
            24,
            80,
        )?;
        if let Err(e) = backend.stamp_window(&spawned.backend_id, spec.owner.session_id, spec.role)
        {
            debug!(
                "could not stamp the remote window for '{}': {e:#}",
                spec.owner.name
            );
        }
        let pane_id = PaneId::new(spawned.backend_id)?;
        backend.confirm_spawned(spec, &pane_id)?;
        Ok(pane_id)
    }

    fn confirm_spawned(&self, spec: &WindowSpec<'_>, pane_id: &PaneId) -> Result<()> {
        let Some(settle) = M::SPAWN_SETTLE else {
            return Ok(());
        };
        std::thread::sleep(settle);
        let window_name = window_name_for(spec.role, spec.owner.name);
        let panes = match self.spawned_panes(&window_name) {
            Ok(panes) => panes,
            Err(e) => {
                warn!("could not confirm window {window_name} after creating it: {e:#}");
                return Ok(());
            }
        };
        if panes
            .iter()
            .any(|(id, dead)| !dead && id == pane_id.as_str())
        {
            return Ok(());
        }
        Err(self.spawn_exited(spec, &window_name))
    }

    fn spawn_exited(&self, spec: &WindowSpec<'_>, window_name: &str) -> anyhow::Error {
        anyhow::anyhow!(
            "window {window_name} exited within {}ms of starting `{}`: {} keeps no \
             window once its program exits, so nothing of it is left to read. Check \
             that `{}` runs on {}",
            M::SPAWN_SETTLE.unwrap_or_default().as_millis(),
            spec.command,
            self.transport.mux(),
            spec.command,
            self.name
        )
    }

    // Over ssh a one-shot is re-read by the host's login shell, and `cmd.exe` takes `|` for a pipe.
    fn spawned_panes(&self, window_name: &str) -> Result<Vec<(String, bool)>> {
        if !self.attached() {
            return Ok(self.window_panes(window_name)?);
        }
        let listing = self.ctrl_command(&format!(
            "list-windows -t {} -F '{WINDOW_PANES_FORMAT}'",
            self.session
        ))?;
        Ok(named_panes(&listing, window_name))
    }

    fn settle(&self, owner: Owner<'_>, role: WindowRole) -> Result<Located> {
        if !M::WINDOW_OPTIONS {
            if !self.transport.is_remote() {
                return Ok(WindowIndex::from_listing(self.discover_answered()?).locate(
                    owner.session_id,
                    owner.name,
                    role,
                    true,
                ));
            }
            return Ok(Located::Unknown);
        }
        if self
            .retire_duplicate_windows(owner.session_id, role)
            .is_none()
        {
            return Ok(Located::Unknown);
        }
        Ok(WindowIndex::from_listing(self.discover_answered()?).locate(
            owner.session_id,
            owner.name,
            role,
            false,
        ))
    }

    fn one_shot(&self, what: &str, args: &[&str]) -> Result<std::process::Output> {
        let output = self
            .mux_command(args)?
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .map_err(|e| {
                self.transport.launch_failure(
                    &format!("Failed to run {} command", M::MULTIPLEXER.name()),
                    e,
                )
            })?;
        if !output.status.success()
            || (M::EXIT_ZERO_ERRORS && mux_reported_error(&output, self.transport.is_ssh(), false))
        {
            bail!(
                "{} {what} {}",
                self.transport.mux(),
                mux_failure(&output, self.transport.is_ssh())
            );
        }
        Ok(output)
    }

    fn refuse_exited(&self, pane: &str, target: &str) -> Result<()> {
        let dead = self
            .tmux_output(&["display-message", "-p", "-t", target, "#{pane_dead}"])
            .is_ok_and(|answer| parse_pane_dead(&answer));
        if dead {
            bail!("its pane {pane} has exited and accepts no input");
        }
        Ok(())
    }

    fn kill_window_oneshot(&self, pane: &str) -> Result<()> {
        let (verb, target) = self.kill_command(pane)?;
        match self.run_tmux(&[verb, "-t", &target]) {
            Err(e) if !typed_absence(&e) => Err(e),
            _ => Ok(()),
        }
    }

    fn start_server_in_own_scope(&self, args: &[&str]) -> Result<std::process::Output> {
        if self.transport.is_remote() || !scopes_available() {
            return self.run_tmux(args);
        }
        self.run_mux_command(in_own_scope(&self.mux_command(args)?), args)
    }

    fn run_tmux(&self, args: &[&str]) -> Result<std::process::Output> {
        self.run_mux_command(self.mux_command(args)?, args)
    }

    fn run_mux_command(&self, mut command: Command, args: &[&str]) -> Result<std::process::Output> {
        let output = command
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .map_err(|e| {
                self.transport.launch_failure(
                    &format!("Failed to run {} command", M::MULTIPLEXER.name()),
                    e,
                )
            })?;

        if !output.status.success()
            || (M::EXIT_ZERO_ERRORS && mux_reported_error(&output, self.transport.is_ssh(), false))
        {
            return Err(self.command_failure(args, &output).into());
        }

        Ok(output)
    }

    fn command_failure(
        &self,
        args: &[&str],
        output: &std::process::Output,
    ) -> crate::contract::BackendError {
        let detail = mux_failure(output, self.transport.is_ssh());
        let message = format!(
            "{} {} failed: {detail}",
            M::MULTIPLEXER.name(),
            args.join(" ")
        );
        if self.transport.is_ssh() && output.status.code() == Some(SSH_ERROR_EXIT) {
            return crate::contract::BackendError::Unreachable(message);
        }
        if output.status.code().is_none() {
            return crate::contract::BackendError::Io(message);
        }
        match self.refusal(&detail, message) {
            crate::contract::BackendError::Absent(message) if !output.stdout.is_empty() => {
                crate::contract::BackendError::Refused(message)
            }
            classified => classified,
        }
    }

    fn refusal(&self, reply: &str, message: String) -> crate::contract::BackendError {
        let reply = reply.trim();
        if !reply.contains('\n') && M::answered_absent(reply).is_some() {
            return crate::contract::BackendError::Absent(message);
        }
        if reply.starts_with("duplicate session: ") || reply.starts_with("duplicate window: ") {
            return crate::contract::BackendError::Busy(message);
        }
        crate::contract::BackendError::Refused(message)
    }

    fn classified(&self, error: anyhow::Error) -> anyhow::Error {
        match error.downcast::<Refusal>() {
            Ok(refused) => self
                .refusal(
                    &refused.reply,
                    format!("{} {refused}", M::MULTIPLEXER.name()),
                )
                .into(),
            Err(error) => error,
        }
    }

    fn session_exists(&self) -> bool {
        self.tmux_run(&[M::SESSION_PROBE_COMMAND, "-t", &self.session])
            .is_ok()
    }

    fn apply_session_config(&self) -> Result<()> {
        let config = self.session_config();
        if M::COMMAND_LISTS && self.tmux_run(&config_command_list(&[], &config)).is_ok() {
            return Ok(());
        }
        for option in &config {
            let args: Vec<&str> = option.args.iter().map(String::as_str).collect();
            match self.tmux_run(&args) {
                Ok(()) => {}
                Err(e) if option.fatal => return Err(e),
                Err(e) => debug!(
                    "{} option {} not set: {e}",
                    M::MULTIPLEXER.name(),
                    option.args.join(" ")
                ),
            }
        }
        Ok(())
    }

    pub(crate) fn session_config(&self) -> Vec<ConfigOption> {
        let scope = M::SERVER_SCOPE;
        let mut config = Vec::new();
        let mut set = |args: &[&str], fatal: bool| {
            let mut command = args.to_vec();
            if M::SERVER_OPTIONS_NEED_TARGET && args.first() == Some(&scope) {
                command.splice(1..1, ["-t", &self.session]);
            }
            config.push(ConfigOption::set(&command, fatal));
        };
        // A non-login shell keeps macOS path_helper from clobbering the PATH set in ~/.zshenv.
        if self.platform == Platform::Posix {
            set(&[scope, "default-command", &self.config_shell()], true);
        }

        set(&[scope, "default-terminal", "xterm-256color"], true);
        set(&[scope, "extended-keys", "on"], true);

        // `extended-keys-format` landed in tmux 3.5; `pi` warns unless it reads `csi-u`.
        set(&[scope, "extended-keys-format", "csi-u"], false);

        for (key, val) in SESSION_OPTS {
            set(&["-t", &self.session, key, val], true);
        }

        for (key, val) in WINDOW_OPTS {
            set(&["-w", "-g", key, val], false);
        }

        config.extend(M::session_config(&self.session));
        config
    }

    pub(crate) fn ensure_session_configured(&self) -> Result<()> {
        for attempt in 1..M::COLD_START_ATTEMPTS {
            match self.ensure_session_configured_once() {
                Ok(()) => return Ok(()),
                Err(e) if self.retryable_cold_start_error(&e) => {
                    debug!(
                        "{} cold session bootstrap attempt {attempt} lost its server: {e:#}",
                        self.name
                    );
                    std::thread::sleep(std::time::Duration::from_millis(50));
                }
                Err(e) => return Err(e),
            }
        }
        match self.ensure_session_configured_once() {
            Ok(()) => Ok(()),
            Err(e)
                if self.retryable_cold_start_error(&e)
                    && M::COLD_START_GRACE > std::time::Duration::ZERO =>
            {
                self.wait_and_configure(e)
            }
            Err(e) => Err(e),
        }
    }

    fn wait_and_configure(&self, error: anyhow::Error) -> Result<()> {
        let deadline = std::time::Instant::now() + M::COLD_START_GRACE;
        let mut last_error = error;
        loop {
            if self.session_exists() {
                match self.apply_session_config() {
                    Ok(()) => return Ok(()),
                    Err(e) if M::RETRY_NO_SERVER_ERROR && typed_absence(&e) => {
                        last_error = e;
                    }
                    Err(e) => return Err(e),
                }
            }
            if std::time::Instant::now() >= deadline {
                return Err(last_error);
            }
            std::thread::sleep(std::time::Duration::from_millis(250));
        }
    }

    fn retryable_cold_start_error(&self, error: &anyhow::Error) -> bool {
        !crate::preflight::is_missing_dependency(error)
            && (!self.session_exists() || (M::RETRY_NO_SERVER_ERROR && typed_absence(error)))
    }

    fn ensure_session_configured_once(&self) -> Result<()> {
        // tmux stops a command list at its first failure, so a failed `has-session` sets nothing.
        if M::COMMAND_LISTS {
            let config = self.session_config();
            let list = config_command_list(&["has-session", "-t", &self.session], &config);
            if self.tmux_run(&list).is_ok() {
                return Ok(());
            }
        }
        if !self.session_exists() && !self.found_after_all()? {
            M::check_banner(&self.tmux_output(&["-V"])?, &self.socket())?;
            debug!(
                "Creating {} session '{}' on socket '{}'",
                M::MULTIPLEXER.name(),
                self.session,
                self.socket()
            );
            let mut args = vec!["new-session", "-d", "-s", &self.session];
            args.extend_from_slice(M::BOOTSTRAP_SIZE_ARGS);
            if let Err(e) = self.start_server_in_own_scope(&args) {
                // Concurrent relaunches race `new-session`; tmux tells losers `duplicate session`.
                if !self.session_exists() {
                    if crate::preflight::is_missing_dependency(&e) {
                        return Err(e);
                    }
                    return Err(e).with_context(|| {
                        format!("Failed to create {} session", M::MULTIPLEXER.name())
                    });
                }
                debug!(
                    "{} session '{}' was created by a peer; continuing",
                    M::MULTIPLEXER.name(),
                    self.session
                );
            }
            self.wait_for_session_ready(FRESH_SESSION_READY_WAIT);
        }
        self.apply_session_config()
    }

    fn wait_for_session_ready(&self, timeout: std::time::Duration) -> bool {
        let deadline = std::time::Instant::now() + timeout;
        loop {
            if self.session_exists() {
                return true;
            }
            if std::time::Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    }

    // One token only: the ssh and `wsl.exe` transports re-split a value on spaces.
    pub(crate) fn config_shell(&self) -> String {
        if self.transport.is_remote() {
            "/bin/sh".to_string()
        } else {
            std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string())
        }
    }

    pub(crate) fn program_for_window(&self, command: &str, task_path: Option<&str>) -> String {
        if self.transport.is_remote() {
            return command.to_string();
        }
        resolve_local_program_on(command, task_path)
    }

    pub(crate) fn build_shell_command(command: &str, args: &[String]) -> String {
        let mut parts = vec![control_mode::shell_escape(command)];
        for arg in args {
            parts.push(control_mode::shell_escape(arg));
        }
        parts.join(" ")
    }

    pub(crate) fn login_wrap_for_remote(&self, shell_cmd: &str, task_path: Option<&str>) -> String {
        if self.transport.is_remote() && self.platform == crate::Platform::Posix {
            let path = self
                .host
                .as_ref()
                .and_then(|host| match task_path {
                    Some(path) => crate::host_path::task_assignment_for(host, path),
                    None => crate::host_path::assignment_for(host),
                })
                .unwrap_or_default();
            let inner = control_mode::shell_escape(&format!("{path}exec {shell_cmd}"));
            format!("/bin/sh -lc {inner}")
        } else {
            shell_cmd.to_string()
        }
    }

    pub(crate) fn remote_shell_pane_command(&self) -> String {
        let inner = control_mode::shell_escape(
            "command -v \"$SHELL\" >/dev/null 2>&1 && exec \"$SHELL\" -l; exec /bin/sh -l",
        );
        format!("/bin/sh -lc {inner}")
    }

    pub(crate) fn posix_window_command(
        &self,
        window_name: &str,
        command: &str,
        args: &[String],
        task_path: Option<&str>,
    ) -> String {
        let is_remote_shell_pane = self.transport.is_remote()
            && self.platform == Platform::Posix
            && window_name.starts_with(SHELL_WINDOW_PREFIX);
        if is_remote_shell_pane {
            return self.remote_shell_pane_command();
        }
        let program = self.program_for_window(command, task_path);
        let shell_cmd = Self::build_shell_command(&program, args);
        let shell_cmd = match self.transport.is_remote() {
            true => shell_cmd,
            false => shell_prefix_tokens(task_path.map(std::ffi::OsStr::new))
                .map(|tokens| {
                    tokens
                        .into_iter()
                        .chain(std::iter::once(shell_cmd.clone()))
                        .collect::<Vec<_>>()
                        .join(" ")
                })
                .unwrap_or(shell_cmd),
        };
        self.login_wrap_for_remote(&shell_cmd, task_path)
    }

    fn connection(&self) -> Result<Arc<ControlMode>> {
        self.control
            .lock()
            .map_err(|e| anyhow::anyhow!("control lock: {e}"))?
            .clone()
            .ok_or_else(|| anyhow::anyhow!("Control mode not started — call ensure_ready() first"))
    }

    fn with_control<F, R>(&self, f: F) -> Result<R>
    where
        F: FnOnce(&ControlMode) -> Result<R>,
    {
        let ctrl = self.connection()?;
        f(&ctrl).map_err(|e| self.classified(e))
    }

    fn ensure_control(&self) -> Result<()> {
        if self.attached() {
            return Ok(());
        }
        let _connecting = self
            .connecting
            .lock()
            .map_err(|e| anyhow::anyhow!("connecting lock: {e}"))?;
        if self.attached() {
            return Ok(());
        }
        debug!("Starting {} control mode", M::MULTIPLEXER.name());
        let fresh = ControlMode::start(
            &self.located_transport(),
            &self.socket(),
            &self.session,
            &self.sizer,
            &M::control_policy(&self.transport, &self.session),
        )
        .map_err(|e| self.classified(e))?;
        self.install(fresh)
    }

    fn reconnect_control(&self, dead: Option<&Arc<ControlMode>>) -> Result<()> {
        self.refuse_if_closed()?;
        let _connecting = self
            .connecting
            .lock()
            .map_err(|e| anyhow::anyhow!("connecting lock: {e}"))?;
        if let Some(dead) = dead {
            let current = self.connection().ok();
            if current.is_some_and(|current| !Arc::ptr_eq(&current, dead)) {
                return Ok(());
            }
        }
        let start = || {
            ControlMode::start(
                &self.located_transport(),
                &self.socket(),
                &self.session,
                &self.sizer,
                &M::control_policy(&self.transport, &self.session),
            )
            .map_err(|e| self.classified(e))
        };
        let fresh = match start() {
            Ok(fresh) => fresh,
            Err(_) if self.relocate(true) == Search::Found => start()?,
            Err(e) => return Err(e),
        };
        self.install(fresh)?;
        debug!("Control mode reconnected successfully");
        Ok(())
    }

    fn install(&self, fresh: ControlMode) -> Result<()> {
        let fresh = Arc::new(fresh);
        match self.swap(&fresh) {
            Ok(old) => {
                if let Some(old) = old {
                    old.retire();
                }
                Ok(())
            }
            Err(e) => {
                fresh.close();
                Err(e)
            }
        }
    }

    fn swap(&self, fresh: &Arc<ControlMode>) -> Result<Option<Arc<ControlMode>>> {
        let mut guard = self
            .control
            .lock()
            .map_err(|e| anyhow::anyhow!("control lock: {e}"))?;
        if self.is_closed() {
            bail!("{} is shut down", self.name);
        }
        let old = guard.replace(Arc::clone(fresh));
        self.connection_generation
            .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
        if let Some(old) = &old {
            if let Ok(mut retiring) = self.retiring.lock() {
                retiring.retain(|weak| weak.strong_count() > 0);
                retiring.push(Arc::downgrade(old));
            }
        }
        Ok(old)
    }

    fn ctrl_command(&self, cmd: &str) -> Result<String> {
        self.ctrl_command_list(&[cmd])
    }

    fn ctrl_command_list(&self, cmds: &[&str]) -> Result<String> {
        let blocks = if M::COMMAND_LIST_SINGLE_REPLY {
            1
        } else {
            cmds.len()
        };
        let ctrl = self.connection()?;
        match ctrl
            .send_command_list(cmds, blocks)
            .map_err(|e| self.classified(e))
        {
            Ok(val) => Ok(val),
            Err(err) if is_broken_pipe(&err) => {
                warn!("Control mode error, reconnecting: {err:#}");
                self.reconnect_control(Some(&ctrl))?;
                self.with_control(|ctrl| ctrl.send_command_list(cmds, blocks))
            }
            Err(err) if is_recv_timeout(&err) => {
                warn!("Control mode error, reconnecting: {err:#}");
                if let Err(e) = self.reconnect_control(Some(&ctrl)) {
                    debug!("reconnect after an unanswered command failed: {e:#}");
                }
                Err(err)
            }
            Err(err) => Err(err),
        }
    }

    fn ctrl_command_within(&self, cmd: &str, budget: std::time::Duration) -> Result<String> {
        self.with_control(|ctrl| ctrl.send_command_within(cmd, budget))
    }

    fn ctrl_command_detached(&self, cmds: &[&str], blocks: usize) -> Result<()> {
        let blocks = if M::COMMAND_LIST_SINGLE_REPLY {
            1
        } else {
            blocks
        };
        self.with_control(|ctrl| ctrl.send_command_detached(cmds, blocks))
    }

    fn register_pane(
        &self,
        pane_id: &str,
        window_id: Option<&str>,
    ) -> Result<(ControlModeReader, Option<PaneSize>)> {
        let mut learned = None;
        let window_id = match window_id {
            Some(id) => Some(id.to_string()),
            None => {
                // psmux 3.3.7–3.3.8 answer a missing `-t` pane with the active pane's fields.
                let format = if M::WINDOW_EVENTS {
                    format!(
                        "#{{pane_id}} #{{window_id}} #{{pane_height}} #{{pane_width}} {SIZED_BY}"
                    )
                } else {
                    "#{pane_id} #{window_id}".to_string()
                };
                let target = self.target(pane_id)?;
                match self.ctrl_command(&format!("display-message -t {target} -p '{format}'")) {
                    Ok(out) => match parse_pane_location(pane_id, &out) {
                        Some(location) => {
                            learned = location.size.map(|size| (size, location.sized_by));
                            Some(location.window)
                        }
                        None => {
                            warn!(
                                "the answer for {pane_id}'s window was not {pane_id}'s ({}); \
                                 its exit will not be announced",
                                out.trim()
                            );
                            None
                        }
                    },
                    Err(e) => {
                        debug!(
                            "could not learn which window {pane_id} is in ({e:#}); its exit \
                         will not be announced"
                        );
                        None
                    }
                }
            }
        };
        let reports = window_id.is_some() && M::WINDOW_EVENTS;
        let (reader, size) = self.with_control(|ctrl| {
            let (tx, reader) =
                ctrl.pane_channel(pane_id, M::SNAPSHOTS.then_some(self.history_lines));
            let size = reports.then(|| reader.size());
            let mut senders = ctrl
                .pane_senders
                .lock()
                .map_err(|e| anyhow::anyhow!("pane_senders lock: {e}"))?;
            senders
                .entry(pane_id.to_string())
                .or_insert_with(Vec::new)
                .push(tx);
            drop(senders);
            let Some(window_id) = window_id else {
                return Ok((reader, size));
            };
            let mut windows = ctrl
                .pane_windows
                .lock()
                .map_err(|e| anyhow::anyhow!("pane_windows lock: {e}"))?;
            windows.insert(pane_id.to_string(), window_id);
            drop(windows);
            if let Some(size) = &size {
                ctrl.pane_sizes
                    .lock()
                    .map_err(|e| anyhow::anyhow!("pane_sizes lock: {e}"))?
                    .insert(pane_id.to_string(), size.clone());
            }
            Ok((reader, size))
        })?;
        if let (Some(size), Some(((rows, cols), sized_by))) = (&size, learned) {
            size.report(rows, cols);
            size.set_sized_elsewhere(sized_by.is_some_and(|name| name != self.sizer));
        }
        Ok((reader, size))
    }

    fn unregister_pane(&self, pane_id: &str) -> Result<()> {
        self.with_control(|ctrl| {
            if let Err(e) = ctrl.show_pane(pane_id) {
                debug!(pane = %pane_id, "could not resume a hidden pane: {e:#}");
            }
            let mut senders = ctrl
                .pane_senders
                .lock()
                .map_err(|e| anyhow::anyhow!("pane_senders lock: {e}"))?;
            senders.remove(pane_id);
            drop(senders);
            let mut windows = ctrl
                .pane_windows
                .lock()
                .map_err(|e| anyhow::anyhow!("pane_windows lock: {e}"))?;
            windows.remove(pane_id);
            drop(windows);
            ctrl.pane_sizes
                .lock()
                .map_err(|e| anyhow::anyhow!("pane_sizes lock: {e}"))?
                .remove(pane_id);
            Ok(())
        })
    }

    fn pane_writer(&self, pane_id: &str) -> Result<ControlModeWriter> {
        let input = M::pane_input(&self.transport, &self.socket());
        let target = self.target(pane_id)?.into_owned();
        self.with_control(|ctrl| Ok(ctrl.writer(target, input)))
    }

    fn connect_pane(
        &self,
        pane_id: &str,
        window_id: Option<&str>,
        rows: u16,
        cols: u16,
    ) -> Result<AdoptedSession> {
        let (reader, size) = self.register_pane(pane_id, window_id)?;
        // A nowait call would leave an unclaimed %begin/%end block that steals the next waiter.
        if M::PANE_MONITORING {
            self.with_control(|ctrl| ctrl.show_pane(pane_id))?;
            self.ctrl_command(&format!(
                "refresh-client -A {}",
                M::quote(&format!("{pane_id}:on"))
            ))?;
        }

        // A resize sends SIGWINCH, which makes TUI apps repaint at the new size.
        self.force_resize(pane_id, rows, cols)?;

        let writer = self.pane_writer(pane_id)?;

        Ok(AdoptedSession {
            output: Box::new(reader),
            input: Box::new(writer),
            seed_len: 0,
            size,
        })
    }

    fn capture_history_seed(&self, pane_id: &str) -> Result<Vec<u8>> {
        let start = format!("-{}", self.history_lines);
        let output = self.run_tmux(&[
            "capture-pane",
            "-e",
            "-p",
            "-J",
            "-S",
            &start,
            "-t",
            &self.target(pane_id)?,
        ])?;
        let mut seed = self.pane_state_seed(pane_id);
        seed.extend(history_seed_bytes(output.stdout));
        Ok(seed)
    }

    fn pane_state_seed(&self, pane_id: &str) -> Vec<u8> {
        // tmux's default pane title is the host's short name.
        let out = match self.target(pane_id).and_then(|target| {
            self.run_tmux(&[
                "display-message",
                "-p",
                "-t",
                &target,
                MOUSE_FLAGS_FORMAT_THEN_TITLE,
            ])
        }) {
            Ok(out) => out,
            Err(e) => {
                debug!(pane = %pane_id, "could not read pane state: {e:#}");
                return Vec::new();
            }
        };
        let line = String::from_utf8_lossy(&out.stdout);
        let Some((flags, rest)) = line.lines().next().and_then(|l| l.split_once('|')) else {
            return Vec::new();
        };
        let Some((host, title)) = rest.split_once('|') else {
            return Vec::new();
        };
        let mut seed = mouse_seed_bytes(flags);
        seed.extend(title_seed_bytes(host, title));
        seed
    }

    fn force_resize(&self, pane_id: &str, rows: u16, cols: u16) -> Result<()> {
        // tmux sends SIGWINCH only when the size changes, so resize away and back.
        if rows > 1 {
            self.resize(pane_id, rows - 1, cols)?;
        } else {
            self.resize(pane_id, rows + 1, cols)?;
        }
        self.resize(pane_id, rows, cols)?;
        Ok(())
    }
}

pub(crate) struct PrivateEnv<'a, M: TmuxCompatible> {
    server: &'a Server<M>,
    pub(crate) env: HashMap<String, String>,
    kept: bool,
}

impl<'a, M: TmuxCompatible> PrivateEnv<'a, M> {
    pub(crate) fn new(server: &'a Server<M>, env: &HashMap<String, String>) -> Result<Self> {
        Ok(PrivateEnv {
            server,
            env: M::private_env(server, env)?,
            kept: false,
        })
    }

    pub(crate) fn keep(mut self) {
        self.kept = true;
    }
}

impl<M: TmuxCompatible> Drop for PrivateEnv<'_, M> {
    fn drop(&mut self) {
        if !self.kept {
            M::discard_private_env(self.server, &self.env);
        }
    }
}

impl<M: TmuxCompatible> PaneStream for Server<M> {
    fn spawn(
        &self,
        window_name: &str,
        command: &str,
        args: &[String],
        cwd: Option<&Path>,
        env: &HashMap<String, String>,
        rows: u16,
        cols: u16,
    ) -> crate::contract::BackendResult<SpawnedSession> {
        self.admit_server()?;
        let private = PrivateEnv::new(self, env)?;
        let new_window = self.new_window_line(window_name, command, args, cwd, &private.env)?;
        let options = birth_option_commands::<M>(window_name);
        let cmds: Vec<&str> = std::iter::once(new_window.as_str())
            .chain(options.iter().map(String::as_str))
            .collect();
        let result = self.ctrl_command_list(&cmds)?;
        let answer = result.trim();
        let (pane_id, window_id) = match answer.split_once(char::is_whitespace) {
            Some((pane, window)) => (pane.trim().to_string(), Some(window.trim().to_string())),
            None => (answer.to_string(), None),
        };
        if !control_mode::is_valid_pane_id(&pane_id) {
            return Err(anyhow::anyhow!(
                "{} new-window returned an invalid pane id: {pane_id:?}",
                M::MULTIPLEXER.name()
            )
            .into());
        }
        let window_id = window_id.filter(|id| {
            let ok = control_mode::is_valid_window_id(id);
            if !ok {
                debug!(
                    "{} new-window answered with an unusable window id: {id:?}",
                    M::MULTIPLEXER.name()
                );
            }
            ok
        });

        debug!(pane_id = %pane_id, window_id = ?window_id, "window created via control mode");
        private.keep();

        let connected = self.connect_pane(&pane_id, window_id.as_deref(), rows, cols)?;

        Ok(SpawnedSession {
            backend_id: pane_id,
            output: connected.output,
            input: connected.input,
            size: connected.size,
        })
    }

    fn adopt(
        &self,
        backend_id: &str,
        rows: u16,
        cols: u16,
        seed: Option<Vec<u8>>,
    ) -> crate::contract::BackendResult<AdoptedSession> {
        if !control_mode::is_valid_pane_id(backend_id) {
            return Err(
                anyhow::anyhow!("refusing to adopt invalid pane id: {backend_id:?}").into(),
            );
        }
        let perf_log = std::env::var_os("THURBOX_PERF_LOG").is_some();

        let capture_start = perf_log.then(std::time::Instant::now);
        let seed = match seed {
            Some(seed) => seed,
            None => match self.capture_history(backend_id) {
                Ok(seed) => seed,
                Err(e) if matches!(e, crate::BackendError::Absent(_)) => {
                    return Err(crate::BackendError::Absent(format!(
                        "pane {backend_id} is gone: {e:#}"
                    )))
                }
                Err(e) => {
                    warn!("Failed to capture history for pane {backend_id}: {e}");
                    Vec::new()
                }
            },
        };
        let capture_ms = capture_start.map(|s| s.elapsed().as_millis() as u64);

        let connect_start = perf_log.then(std::time::Instant::now);
        let connected = self.connect_pane(backend_id, None, rows, cols)?;
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
        let seed_len = seed.len();
        Ok(AdoptedSession {
            output: seeded(seed, connected.output),
            input: connected.input,
            seed_len,
            size: connected.size,
        })
    }

    fn capture_history(&self, backend_id: &str) -> crate::contract::BackendResult<Vec<u8>> {
        if !control_mode::is_valid_pane_id(backend_id) {
            return Err(
                anyhow::anyhow!("refusing to capture invalid pane id: {backend_id:?}").into(),
            );
        }
        Ok(self.capture_history_seed(backend_id)?)
    }

    fn state_seed(&self, backend_id: &str) -> Vec<u8> {
        if !control_mode::is_valid_pane_id(backend_id) {
            return Vec::new();
        }
        self.pane_state_seed(backend_id)
    }

    fn request_snapshot(&self, backend_id: &str) -> crate::contract::BackendResult<()> {
        if !control_mode::is_valid_pane_id(backend_id) {
            return Err(
                anyhow::anyhow!("refusing to snapshot invalid pane id: {backend_id:?}").into(),
            );
        }
        if !M::SNAPSHOTS {
            return Err(anyhow::anyhow!(
                "{} cannot snapshot a pane in step with its output",
                self.transport.mux()
            )
            .into());
        }
        Ok(self.with_control(|ctrl| {
            ctrl.show_pane(backend_id)?;
            ctrl.request_snapshot(backend_id, self.history_lines)
        })?)
    }

    fn hide_output(&self, backend_id: &str) {
        if !(M::PANE_MONITORING && M::SNAPSHOTS && self.transport.is_remote()) {
            return;
        }
        let hidden = self.with_control(|ctrl| ctrl.hide_pane(backend_id));
        if let Err(e) = hidden {
            debug!(pane = %backend_id, "output of a hidden pane still streams: {e:#}");
        }
    }

    fn snapshot(
        &self,
        backend_id: &str,
    ) -> crate::contract::BackendResult<crate::contract::PaneSnapshot> {
        if !control_mode::is_valid_pane_id(backend_id) {
            return Err(
                anyhow::anyhow!("refusing to snapshot invalid pane id: {backend_id:?}").into(),
            );
        }
        if !M::SNAPSHOTS {
            return Err(anyhow::anyhow!("{} cannot snapshot a pane", self.transport.mux()).into());
        }
        Ok(self
            .with_control(|ctrl| ctrl.ask_snapshot(backend_id, self.history_lines))?
            .wait()
            .map_err(|e| self.classified(e))?)
    }

    fn output_times(&self) -> Result<std::collections::HashMap<String, u64>> {
        if !self.stamps_are_per_window() {
            return Ok(std::collections::HashMap::new());
        }
        let mut argv = M::DISPLAY_FLAGS.to_vec();
        argv.extend([
            "list-windows",
            "-a",
            "-F",
            "#{@thurbox_session}\t#{@thurbox_role}\t#{window_activity}",
        ]);
        let output = self.run_tmux(&argv)?;
        let mut times: std::collections::HashMap<String, u64> = std::collections::HashMap::new();
        for line in String::from_utf8_lossy(&output.stdout).lines() {
            let mut fields = line.split('\t');
            let (Some(owner), Some(role), Some(at)) = (fields.next(), fields.next(), fields.next())
            else {
                continue;
            };
            if owner.parse::<crate::SessionId>().is_err() || role == "shell" {
                continue;
            }
            let Some(at) = at
                .parse::<u64>()
                .ok()
                .and_then(|at| at.checked_mul(1000))
                .filter(|at| *at > 0)
            else {
                continue;
            };
            times
                .entry(owner.to_string())
                .and_modify(|stored| *stored = (*stored).max(at))
                .or_insert(at);
        }
        Ok(times)
    }

    fn resize(&self, backend_id: &str, rows: u16, cols: u16) -> crate::contract::BackendResult<()> {
        // tmux runs one command list without returning to its event loop.
        if !self.admitted_here() {
            return Ok(());
        }
        let (rows, cols) = tmux_size(rows, cols);
        let target = self.spell_target(backend_id);
        let window = format!("resize-window -t {target} -x {cols} -y {rows}");
        let pane = format!("resize-pane -t {target} -x {cols} -y {rows}");
        if !M::CONDITIONAL_RESIZE {
            // No `if-shell -F` on this multiplexer: the last instance to paint wins.
            return Ok(self.ctrl_command_detached(&[&window, &pane], 2)?);
        }
        // tmux answers one `%begin` block per command an `if-shell` runs.
        let me = &self.sizer;
        let may = format!(
            "#{{||:#{{==:#{{session_attached}},1}},#{{||:#{{==:#{{{SIZER_OPTION}}},}},#{{==:#{{{SIZER_OPTION}}},{me}}}}}}}"
        );
        let settle = format!(
            "set-option -F -w -t {target} {SIZER_OPTION} '#{{?{may},{me},#{{{SIZER_OPTION}}}}}'"
        );
        let mine = format!("#{{==:#{{{SIZER_OPTION}}},{me}}}");
        let only_if_mine = |cmd: &str| {
            format!("if-shell -F -t {target} '{mine}' '{cmd}' 'display-message -p \"\"'")
        };
        Ok(self
            .ctrl_command_detached(&[&settle, &only_if_mine(&window), &only_if_mine(&pane)], 5)?)
    }

    fn claim_size(
        &self,
        backend_id: &str,
        rows: u16,
        cols: u16,
    ) -> crate::contract::BackendResult<()> {
        if !M::CONDITIONAL_RESIZE {
            return self.resize(backend_id, rows, cols);
        }
        if !self.admitted_here() {
            return Ok(());
        }
        let (rows, cols) = tmux_size(rows, cols);
        let target = self.spell_target(backend_id);
        Ok(self.ctrl_command_detached(
            &[
                &format!("set-option -w -t {target} {SIZER_OPTION} {}", self.sizer),
                &format!("resize-window -t {target} -x {cols} -y {rows}"),
                &format!("resize-pane -t {target} -x {cols} -y {rows}"),
            ],
            3,
        )?)
    }

    fn detach(&self, backend_id: &str) -> crate::contract::BackendResult<()> {
        if M::PANE_MONITORING {
            let off = format!(
                "refresh-client -A {}",
                M::quote(&format!("{backend_id}:off"))
            );
            if let Err(e) = self.ctrl_command_detached(&[&off], 1) {
                warn!("Failed to disable output monitoring during detach: {e}");
            }
        }
        let _ = self.unregister_pane(backend_id);
        Ok(())
    }
}

impl<M: TmuxCompatible> WindowLifecycle for Server<M> {
    fn check_available(&self) -> crate::contract::BackendResult<()> {
        // `-V` prints the version without connecting; over ssh it also proves the link.
        let output = self
            .mux_command(&["-V"])?
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .with_context(|| format!("{} is not installed or not in PATH", self.transport.mux()))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(
                anyhow::anyhow!("{} -V failed: {}", self.transport.mux(), stderr.trim()).into(),
            );
        }

        let version_str = String::from_utf8_lossy(&output.stdout);
        // psmux before 3.3.8 reads `-t` in the client, so the binary itself is judged.
        M::check_banner(&version_str, &self.socket())?;
        debug!("multiplexer version: {}", version_str.trim());
        Ok(())
    }

    fn ensure_ready(&self) -> crate::contract::BackendResult<()> {
        self.refuse_if_closed()?;
        self.ensure_session_configured()?;
        Ok(self.ensure_control()?)
    }

    fn set_pane_retention(
        &self,
        backend_id: &str,
        keep: bool,
    ) -> crate::contract::BackendResult<()> {
        // tmux resolves `-t` as a window name as readily as an id.
        if !control_mode::is_valid_pane_id(backend_id) {
            return Err(anyhow::anyhow!(
                "refusing to set remain-on-exit on invalid pane id: {backend_id:?}"
            )
            .into());
        }
        if !M::WINDOW_SETTINGS {
            return Ok(());
        }
        let keep = if keep { "on" } else { "off" };
        Ok(self.tmux_run(&[
            "set-window-option",
            "-t",
            &self.target(backend_id)?,
            "remain-on-exit",
            keep,
        ])?)
    }

    fn window_panes(
        &self,
        window_name: &str,
    ) -> crate::contract::BackendResult<Vec<(String, bool)>> {
        // tmux matches window names fnmatch-style, so the match is exact here.
        let listing = self.tmux_output(&[
            "list-windows",
            "-t",
            &self.session,
            "-F",
            WINDOW_PANES_FORMAT,
        ])?;
        Ok(named_panes(&listing, window_name))
    }

    fn discover(&self) -> crate::contract::BackendResult<Vec<DiscoveredSession>> {
        if !self.attached() {
            self.known_socket()?;
            return Ok(self.discover_answered()?);
        }
        if !self.session_exists() {
            return self.discover_answered().map_err(Into::into);
        }
        let result = self.ctrl_command(&format!(
            "list-windows -t {} -F '{DISCOVER_FORMAT}'",
            self.session
        ))?;

        M::check_listing(&result)?;

        let stamps = self.stamps_are_per_window();
        Ok(result
            .lines()
            .filter_map(|line| parse_discovered(line, stamps))
            .collect())
    }

    fn create_window(&self, spec: &WindowSpec<'_>) -> crate::contract::BackendResult<PaneId> {
        if self.transport.is_remote() {
            return Ok(self.create_remote_window(spec)?);
        }
        let pane_id = PaneId::new(self.create_local_window(spec, true)?)?;
        self.confirm_spawned(spec, &pane_id)?;
        Ok(pane_id)
    }

    fn create_window_observed(
        &self,
        spec: &WindowSpec<'_>,
        settle: std::time::Duration,
    ) -> crate::contract::BackendResult<(PaneId, BackendLiveness)> {
        let remote = self.transport.is_remote();
        let pane = if remote || !M::WINDOW_OPTIONS {
            self.create_window(spec)?
        } else {
            PaneId::new(self.create_local_window(spec, false)?)?
        };
        std::thread::sleep(settle);
        let window_name = window_name_for(spec.role, spec.owner.name);
        if !M::WINDOW_OPTIONS {
            let panes = self.spawned_panes(&window_name)?;
            if panes.iter().any(|(id, dead)| id != pane.as_str() && !dead) {
                return Err(crate::contract::BackendError::Refused(
                    "created window identity is ambiguous".into(),
                ));
            }
            let state = match panes.iter().find(|(id, _)| id == pane.as_str()) {
                Some((_, false)) => BackendLiveness::Live,
                Some((_, true)) => BackendLiveness::Exited,
                None => BackendLiveness::Missing,
            };
            return Ok((pane, state));
        }
        let format = format!("{RETIRE_FORMAT}|#{{pane_id}}|#{{pane_dead}}|#{{window_name}}");
        let output = self
            .mux_command(&["list-windows", "-t", &self.session, "-F", &format])?
            .output()
            .context("observe created window")?;
        if !output.status.success() {
            return Err(self.command_failure(&["list-windows"], &output));
        }
        let listing = String::from_utf8_lossy(&output.stdout);
        let mut state = BackendLiveness::Missing;
        for line in listing.lines() {
            let fields: Vec<_> = line.split('|').collect();
            if fields.get(3) == Some(&pane.as_str()) {
                if fields.get(1) != Some(&spec.owner.session_id)
                    || fields.get(2) != Some(&spec.role.as_str())
                    || (spec.owner.session_id.is_empty()
                        && fields.get(5) != Some(&window_name.as_str()))
                {
                    return Err(crate::contract::BackendError::Refused(
                        "created pane is now owned by another session or role".into(),
                    ));
                }
                state = match fields.get(4).copied() {
                    Some("0") => BackendLiveness::Live,
                    Some("1") => BackendLiveness::Exited,
                    _ => BackendLiveness::Unknown,
                };
            }
        }
        let retired = self.retire_windows_in(&listing, spec.owner.session_id, spec.role);
        if retired.iter().any(|window| {
            listing.lines().any(|line| {
                let fields: Vec<_> = line.split('|').collect();
                fields.first() == Some(&window.as_str()) && fields.get(3) == Some(&pane.as_str())
            })
        }) {
            state = BackendLiveness::Missing;
        }
        Ok((pane, state))
    }

    fn locate(&self, owner: Owner<'_>) -> crate::contract::BackendResult<Placed> {
        self.known_socket()?;
        let index = WindowIndex::from_listing(self.discover_answered()?);
        let place = |role| match index.locate(owner.session_id, owner.name, role, false) {
            Located::Unknown => self.settle(owner, role),
            found => Ok(found),
        };
        Ok(Placed {
            agent: place(WindowRole::Agent)?,
            shell: place(WindowRole::Shell)?,
        })
    }

    fn rename_windows(&self, owner: Owner<'_>, to: &str) -> crate::contract::BackendResult<()> {
        self.known_socket()?;
        let index = WindowIndex::from_listing(self.discover_answered()?);
        for role in [WindowRole::Agent, WindowRole::Shell] {
            match index.locate(owner.session_id, owner.name, role, false) {
                Located::At(pane) => {
                    self.tmux_run(&[
                        "rename-window",
                        "-t",
                        &self.target(&pane)?,
                        &window_name_for(role, to),
                    ])?;
                }
                Located::Absent => {}
                Located::Unknown => {
                    return Err(anyhow::anyhow!(
                        "several windows are named after '{}' and none is stamped as this \
                     session's, so there is no telling which one to rename",
                        owner.name
                    )
                    .into())
                }
            }
        }
        Ok(())
    }

    fn stamp_window(
        &self,
        backend_id: &str,
        session_id: &str,
        role: WindowRole,
    ) -> crate::contract::BackendResult<()> {
        if !control_mode::is_valid_pane_id(backend_id) {
            return Err(
                anyhow::anyhow!("refusing to stamp an invalid pane id: {backend_id:?}").into(),
            );
        }
        // Where options are not per window, a stamp would land as a global one.
        if !self.stamps_are_per_window() {
            return Ok(());
        }
        // `-w`: the option belongs to the window, so it survives its pane being split or replaced.
        let target = self.target(backend_id)?;
        if !self.attached() {
            for (option, value) in [
                (WINDOW_SESSION_OPTION, session_id),
                (WINDOW_ROLE_OPTION, role.as_str()),
            ] {
                if !value.is_empty() {
                    self.tmux_run(&["set-option", "-w", "-t", &target, option, value])?;
                }
            }
        } else {
            if !session_id.is_empty() {
                self.ctrl_command(&format!(
                    "set-option -w -t {target} {WINDOW_SESSION_OPTION} {}",
                    shell_escape(session_id)
                ))?;
            }
            self.ctrl_command(&format!(
                "set-option -w -t {target} {WINDOW_ROLE_OPTION} {}",
                role.as_str()
            ))?;
        }
        let _ = self.retire_duplicate_windows(session_id, role);
        Ok(())
    }

    fn reap_exited(&self) {
        if self.known_socket().is_err() {
            return;
        }
        // `run-shell` answers once its child is reaped; tmux reaps every exited child then.
        if let Err(e) = self.one_shot("run-shell (reap)", &["run-shell", "true"]) {
            debug!("could not have the server reap its exited panes: {e:#}");
        }
    }

    fn pane_state(&self, pane: &str) -> crate::contract::BackendResult<PaneState> {
        self.known_socket()?;
        let format = [
            "#{cursor_y}",
            "#{cursor_x}",
            "#{pane_current_command}",
            "#{pane_current_path}",
            "#{pane_tty}",
            "#{pane_dead}",
            "#{window_name}",
            "#{pane_id}",
            "#{pane_dead_status}",
            "#{pane_dead_signal}",
            "#{version}",
            "#{pane_title}",
        ]
        .join(&PANE_STATE_SEP.to_string());
        let mut argv = M::DISPLAY_FLAGS.to_vec();
        let target = self.target(pane)?;
        argv.extend(["display-message", "-p", "-t", &target, &format]);
        let output = self.one_shot("display-message (pane state)", &argv)?;
        let raw = String::from_utf8_lossy(&output.stdout);

        let (mut state, tty, window) = parse_pane_state(&raw);
        if !answered_for(
            pane,
            window.as_deref(),
            pane_answer_field(&raw, 7).as_deref(),
        ) {
            return Ok(PaneState::default());
        }
        if !self.transport.is_remote() {
            if let Some((argv0, command)) = tty.as_deref().and_then(foreground_process_on_tty) {
                state.foreground_process = Some(argv0);
                state.foreground_command = Some(command);
            }
        }
        Ok(state)
    }

    // Reading `/proc/<pid>/environ` needs ptrace access, which yama restricts by default.
    fn pane_path(&self, pane: &str) -> crate::contract::BackendResult<Option<String>> {
        self.known_socket()?;
        let format = format!(
            "#{{pane_start_command}}{PANE_STATE_SEP}#{{window_name}}{PANE_STATE_SEP}#{{pane_id}}"
        );
        let mut argv = M::DISPLAY_FLAGS.to_vec();
        let target = self.target(pane)?;
        argv.extend(["display-message", "-p", "-t", &target, &format]);
        let output = self.one_shot("display-message (pane path)", &argv)?;
        let raw = String::from_utf8_lossy(&output.stdout);
        let start_command = pane_answer_field(&raw, 0).unwrap_or_default();
        let window = pane_answer_field(&raw, 1);
        if !answered_for(
            pane,
            window.as_deref(),
            pane_answer_field(&raw, 2).as_deref(),
        ) {
            return Err(anyhow::anyhow!("pane {pane} is not there to read").into());
        }
        Ok(path_from_prefix(&start_command))
    }

    fn is_dead(&self, backend_id: &str) -> crate::contract::BackendResult<bool> {
        if !self.admitted_here() {
            return Err(anyhow::anyhow!(
                "{} is not admitted on this connection; {backend_id} is not asked",
                self.name
            )
            .into());
        }
        let target = self.spell_target(backend_id);
        let result = self.ctrl_command_within(
            &format!("display-message -t {target} -p '#{{pane_dead}}'"),
            LOOP_COMMAND_BUDGET,
        )?;
        Ok(result.trim() == "1")
    }

    fn kill(&self, backend_id: &str) -> crate::contract::BackendResult<()> {
        if !self.attached() {
            // Opening control mode would create the server and session where they are absent.
            self.known_socket()?;
            return match self.transport.is_remote() {
                true => Ok(self.kill_window_oneshot(backend_id)?),
                false => self.kill_window_at(backend_id).map_err(Into::into),
            };
        }
        let (verb, target) = self.kill_command(backend_id)?;
        let _ = self.unregister_pane(backend_id);
        match self.ctrl_command(&format!("{verb} -t {target}")) {
            Err(e) if !typed_absence(&e) => Err(e.into()),
            _ => Ok(()),
        }
    }

    fn pane_pid(&self, backend_id: &str) -> crate::contract::BackendResult<Option<u32>> {
        // Without `DISPLAY_FLAGS` a control client's tmux may print the separator byte as `_`.
        let format = "#{pane_pid} #{pane_id} #{window_name}";
        let target = self.target(backend_id)?;
        let raw = if self.attached() {
            self.ctrl_command(&format!("display-message -t {target} -p '{format}'"))?
        } else {
            self.tmux_output(&["display-message", "-p", "-t", &target, format])
                .unwrap_or_default()
        };
        Ok(parse_pane_pid_answer(backend_id, &raw))
    }

    fn pane_pids(&self) -> crate::contract::BackendResult<HashMap<String, u32>> {
        let result = self.ctrl_command("list-panes -a -F '#{pane_id} #{pane_pid}'")?;
        Ok(control_mode::parse_pane_pids(&result))
    }

    fn pane_ids(&self) -> crate::contract::BackendResult<std::collections::HashSet<String>> {
        let result = self.ctrl_command("list-panes -a -F '#{pane_id}'")?;
        Ok(control_mode::parse_pane_ids(&result))
    }

    fn shutdown(&self) {
        self.closed
            .store(true, std::sync::atomic::Ordering::Release);
        let taken = self.control.lock().ok().and_then(|mut c| {
            self.connection_generation
                .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
            c.take()
        });
        if let Some(control) = taken {
            control.close();
        }
        let retiring = self
            .retiring
            .lock()
            .map(|mut retiring| std::mem::take(&mut *retiring))
            .unwrap_or_default();
        for control in retiring.iter().filter_map(std::sync::Weak::upgrade) {
            control.close();
        }
    }

    fn server_waiter(&self) -> Option<std::process::Command> {
        self.located_transport()
            .waiter_command(&self.socket(), "runtime-mux-waiter")
    }

    fn default_shell(&self) -> String {
        crate::shell::HostShell::for_platform(self.platform)
            .default_shell(self.transport.is_remote())
    }
}

impl<M: TmuxCompatible> Server<M> {
    fn paste_step(&self, argv: &[String]) -> Result<()> {
        let args: Vec<&str> = argv.iter().map(String::as_str).collect();
        match self.one_shot(&argv[0], &args) {
            Err(e) if argv[0] == "paste-buffer" && refused_unsanitised_paste(&e) => {
                let older: Vec<&str> = args
                    .into_iter()
                    .filter(|a| *a != UNSANITISED_PASTE)
                    .collect();
                self.one_shot(&argv[0], &older).map(drop)
            }
            done => done.map(drop),
        }
    }
}

fn refused_unsanitised_paste(e: &anyhow::Error) -> bool {
    format!("{e:#}").contains(&format!("unknown flag {UNSANITISED_PASTE}"))
}

impl<M: TmuxCompatible> PaneIo for Server<M> {
    fn send_text(
        &self,
        pane: &str,
        text: &str,
        submit: bool,
    ) -> crate::contract::BackendResult<()> {
        self.known_socket()?;
        let target = self.target(pane)?;
        self.refuse_exited(pane, &target)?;
        for paste in M::paste_args(&target, text) {
            self.paste_step(&paste)?;
        }
        if !submit {
            return Ok(());
        }
        std::thread::sleep(SEND_KEYS_ENTER_DELAY);
        self.one_shot("send-keys (Enter)", &["send-keys", "-t", &target, "Enter"])?;
        Ok(())
    }

    fn send_text_after(
        &self,
        pane: &str,
        text: &str,
        delay: std::time::Duration,
    ) -> crate::contract::BackendResult<()> {
        self.known_socket()?;
        let script = M::deferred_paste_script(
            self.transport.mux(),
            &self.socket(),
            &self.spell_target(pane),
            text,
        )?;
        self.admit_server()?;
        let secs = delay.as_secs().to_string();
        self.one_shot(
            "run-shell (deferred prompt)",
            &["run-shell", "-b", "-d", &secs, &script],
        )?;
        Ok(())
    }

    fn send_key(&self, pane: &str, key: &Key) -> crate::contract::BackendResult<String> {
        self.known_socket()?;
        let target = self.target(pane)?;
        self.refuse_exited(pane, &target)?;
        let name = tmux_key_name(key);
        self.one_shot(
            &format!("send-keys ({name})"),
            &["send-keys", "-t", &target, &name],
        )?;
        Ok(name)
    }

    fn capture(
        &self,
        pane: &str,
        lines: u32,
        ansi: bool,
    ) -> crate::contract::BackendResult<String> {
        self.known_socket()?;
        let start = format!("-{}", lines.min(MAX_CAPTURE_LINES));
        let target = self.target(pane)?;
        let mut args = vec!["capture-pane", "-p", "-J", "-t", &target, "-S", &start];
        if ansi {
            args.push("-e");
        }
        let output = self.one_shot("capture-pane", &args)?;
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }

    fn text_before_cursor(&self, pane: &str) -> crate::contract::BackendResult<Option<String>> {
        self.known_socket()?;
        if !M::CURSOR_ROW_CAPTURE {
            return Ok(None);
        }
        let format = format!(
            "#{{cursor_x}}{PANE_STATE_SEP}#{{cursor_y}}{PANE_STATE_SEP}#{{window_name}}{PANE_STATE_SEP}#{{pane_id}}"
        );
        let mut argv = M::DISPLAY_FLAGS.to_vec();
        let target = self.target(pane)?;
        argv.extend(["display-message", "-p", "-t", &target, &format]);
        let output = self.one_shot("display-message (cursor)", &argv)?;
        let raw = String::from_utf8_lossy(&output.stdout);
        let window = pane_answer_field(&raw, 2);
        if !answered_for(
            pane,
            window.as_deref(),
            pane_answer_field(&raw, 3).as_deref(),
        ) {
            return Ok(None);
        }
        let cell = |n| pane_answer_field(&raw, n).and_then(|v| v.parse::<usize>().ok());
        let (Some(col), Some(row)) = (cell(0), cell(1)) else {
            return Ok(None);
        };
        // `capture` joins wrapped lines, which would move the cursor's row.
        let row = row.to_string();
        let output = self.one_shot(
            "capture-pane (cursor row)",
            &[
                "capture-pane",
                "-p",
                "-t",
                &self.target(pane)?,
                "-S",
                &row,
                "-E",
                &row,
            ],
        )?;
        let line = String::from_utf8_lossy(&output.stdout);
        Ok(Some(cells_before(line.trim_end_matches('\n'), col)))
    }
}

impl<M: TmuxCompatible> StatusChannel for Server<M> {
    fn hook_signal_command(&self) -> Option<String> {
        M::HOOK_STATUS.then(|| M::hook_signal_command(self))
    }

    fn take_hook_state_events(&self) -> Vec<(String, String)> {
        let control = self.control.try_lock().ok().and_then(|guard| guard.clone());
        control
            .map(|ctrl| ctrl.take_sub_events())
            .unwrap_or_default()
    }

    fn record_hook_state(&self, pane: &str, state: &str) -> crate::contract::BackendResult<()> {
        self.refuse_without_hook_status()?;
        if !control_mode::is_valid_pane_id(pane) {
            return Err(anyhow::anyhow!("'{pane}' is not a pane id").into());
        }
        Ok(self
            .one_shot(
                "set-option (hook state)",
                &[
                    "set-option",
                    "-p",
                    "-t",
                    &self.target(pane)?,
                    control_mode::REMOTE_HOOK_STATE_OPTION,
                    state,
                ],
            )
            .map(drop)?)
    }

    fn hook_states(&self) -> crate::contract::BackendResult<Vec<(String, String)>> {
        self.refuse_without_hook_status()?;
        match self.run_tmux(&["has-session", "-t", &self.session]) {
            Ok(_) => {}
            Err(e) if typed_absence(&e) => return Ok(Vec::new()),
            Err(e) => return Err(e.into()),
        }
        let format = hook_state_format();
        let body = self.run_tmux(&["list-panes", "-s", "-t", &self.session, "-F", &format])?;
        Ok(control_mode::parse_pane_hook_states(
            String::from_utf8_lossy(&body.stdout).trim(),
        ))
    }
}

impl<M: TmuxCompatible> SessionBackend for Server<M> {
    fn name(&self) -> &str {
        &self.name
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            needs_liveness_poll: !M::WINDOW_EVENTS,
            supports_snapshots: M::SNAPSHOTS,
            keeps_exited_windows: M::WINDOW_SETTINGS,
        }
    }
}

#[derive(Clone)]
struct LocatedDir {
    dir: SocketDir,
    searched: Option<std::time::Instant>,
    outcome: Search,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Search {
    Found,
    Absent,
    Stuck(String),
}

const RELOCATE_EVERY: std::time::Duration = std::time::Duration::from_secs(30);

fn located_dirs() -> &'static Mutex<HashMap<String, LocatedDir>> {
    static LOCATED: std::sync::OnceLock<Mutex<HashMap<String, LocatedDir>>> =
        std::sync::OnceLock::new();
    LOCATED.get_or_init(|| Mutex::new(HashMap::new()))
}

// tmux never unlinks its socket, so the file can outlive the server.
fn local_socket_file(dir: &SocketDir, socket: &str) -> bool {
    #[cfg(unix)]
    {
        let base = match dir {
            SocketDir::Dir(dir) => std::path::PathBuf::from(dir),
            SocketDir::Inherited => std::env::var_os("TMUX_TMPDIR")
                .filter(|d| !d.is_empty())
                .map_or_else(
                    || std::path::PathBuf::from("/tmp"),
                    std::path::PathBuf::from,
                ),
            SocketDir::HostData { .. } => return false,
        };
        // SAFETY: `getuid` cannot fail and touches no memory.
        let uid = unsafe { libc::getuid() };
        base.join(format!("tmux-{uid}"))
            .join(socket)
            .symlink_metadata()
            .is_ok()
    }
    #[cfg(not(unix))]
    {
        let _ = (dir, socket);
        false
    }
}

// `/proc/net/unix` lists bound sockets, unlinked ones included; macOS answers through `lsof`.
const LOCATE_BODY: &str = r#"mux=$1 name=$2 uid=$(id -u)
answers() { TMUX_TMPDIR=$1 "$mux" -L "$name" list-sessions >/dev/null 2>&1; }
answers_within() {
  t=0
  while [ "$t" -lt 30 ] && ! answers "$1"; do sleep 0.1; t=$((t + 1)); done
  answers "$1"
}
sock() {
  if b=$(cd "$1/tmux-$uid" 2>/dev/null && pwd -P); then :
  elif b=$(cd "$1" 2>/dev/null && pwd -P); then b=$b/tmux-$uid
  else b=$1/tmux-$uid; fi
  printf '%s/%s\n' "$b" "$name"
}
bound() {
  if [ -r /proc/net/unix ]; then
    want=$(P=$1 awk '$4 == "00010000" {
      path = $0; for (k = 0; k < 7; k++) sub(/^ *[^ ]+ /, "", path)
      if (path == ENVIRON["P"]) printf " %s ", $7 }' /proc/net/unix)
    [ -n "$want" ] || return 0
    ls -l /proc/[0-9]*/fd 2>/dev/null | awk -v want="$want" '
      /^\/proc\/[0-9]+\/fd:$/ { split($0, a, "/"); pid = a[3]; next }
      / -> socket:\[/ { i = $NF; gsub(/^socket:\[|\]$/, "", i); if (index(want, " " i " ")) print pid }'
  elif command -v lsof >/dev/null 2>&1; then
    lsof -w -a -U -u "$uid" -F pn 2>/dev/null |
      P=$1 awk '/^p/ { pid = substr($0, 2) } /^n/ && substr($0, 2) == ENVIRON["P"] { print pid }'
  fi | sort -un
}
i=0
while [ "$i" -lt "$n" ]; do
  eval "d=\$d$i"
  if answers "$d"; then echo "answered $i"; exit 0; fi
  i=$((i + 1))
done
i=0 seen=' '
while [ "$i" -lt "$n" ]; do
  eval "d=\$d$i"
  p=$(sock "$d")
  case $seen in *" $p "*) i=$((i + 1)); continue ;; esac
  seen="$seen$p "
  pids=$(bound "$p" | tr '\n' ' ')
  if [ -n "$pids" ]; then
    if [ -e "$p" ]; then
      if answers_within "$d"; then echo "answered $i"; exit 0; fi
      echo "bound $(echo $pids | tr ' ' ',') $p"
    else
      set -- $pids
      pid=$1
      shift
      mkdir -p -m 700 "${p%/*}"
      if kill -USR1 "$pid" 2>/dev/null; then
        if answers_within "$d"; then echo "recovered $i $pid $# $p"; else echo "unanswered $i $pid $# $p"; fi
        exit 0
      fi
    fi
  fi
  i=$((i + 1))
done
"#;

fn locate_script(candidates: &[SocketDir], socket: &str) -> String {
    let mut script = String::new();
    for (i, dir) in candidates.iter().enumerate() {
        script.push_str(&format!("d{i}={}\n", dir.shell_expr(socket)));
    }
    script.push_str(&format!("n={}\n", candidates.len()));
    script.push_str(LOCATE_BODY);
    script
}

#[derive(Debug, PartialEq, Eq)]
enum ServerFound {
    Answered(usize),
    Recovered {
        at: usize,
        pid: u32,
        others: usize,
        path: String,
    },
    Unanswered {
        at: usize,
        pid: u32,
        path: String,
    },
    Bound {
        pids: String,
        path: String,
    },
    Nothing,
}

const SEED_CHUNK_BYTES: usize = 4096;

fn seeded(seed: Vec<u8>, live: crate::contract::PaneOutput) -> crate::contract::PaneOutput {
    let pieces = (0..seed.len()).step_by(SEED_CHUNK_BYTES).map(move |start| {
        let end = seed.len().min(start + SEED_CHUNK_BYTES);
        PaneChunk::Output(seed[start..end].to_vec())
    });
    Box::new(pieces.chain(live))
}

fn parse_located(answer: &str) -> ServerFound {
    let line = answer.lines().next().unwrap_or("").trim_end();
    let mut words = line.splitn(5, ' ');
    let verb = words.next().unwrap_or("");
    let mut next = || words.next().unwrap_or("");
    match verb {
        "answered" => next()
            .parse()
            .map_or(ServerFound::Nothing, ServerFound::Answered),
        "recovered" | "unanswered" => {
            let (at, pid, others, path) = (next(), next(), next(), next());
            let (Ok(at), Ok(pid), Ok(others)) = (at.parse(), pid.parse(), others.parse()) else {
                return ServerFound::Nothing;
            };
            let path = path.to_string();
            match verb {
                "recovered" => ServerFound::Recovered {
                    at,
                    pid,
                    others,
                    path,
                },
                _ => ServerFound::Unanswered { at, pid, path },
            }
        }
        "bound" => {
            let rest = line.strip_prefix("bound ").unwrap_or("");
            match rest.split_once(' ') {
                Some((pids, path)) => ServerFound::Bound {
                    pids: pids.to_string(),
                    path: path.to_string(),
                },
                None => ServerFound::Nothing,
            }
        }
        _ => ServerFound::Nothing,
    }
}

fn parse_pane_dead(output: &str) -> bool {
    output.trim() == "1"
}

fn mux_failure(out: &std::process::Output, is_ssh: bool) -> String {
    let stderr = mux_stderr(out, is_ssh);
    let detail = stderr.trim();
    if !detail.is_empty() {
        return detail.to_string();
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    let detail = stdout.trim();
    if detail.starts_with("ERROR:") {
        return detail.to_string();
    }
    format!("exited with status {}", out.status)
}

fn mux_reported_error(out: &std::process::Output, is_ssh: bool, stdout_is_protocol: bool) -> bool {
    !mux_stderr(out, is_ssh).trim().is_empty()
        || (stdout_is_protocol
            && String::from_utf8_lossy(&out.stdout)
                .trim_start()
                .starts_with("ERROR:"))
}

fn mux_stderr(out: &std::process::Output, is_ssh: bool) -> String {
    let stderr = String::from_utf8_lossy(&out.stderr);
    if !is_ssh {
        return stderr.into_owned();
    }
    stderr
        .lines()
        .filter(|line| !line.trim_start().starts_with("**"))
        .collect::<Vec<_>>()
        .join("\n")
}

// psmux implements only `enter`, `escape`, `tab`, `backspace` and `ctrl-<letter>`.
const TMUX_KEYS: &[(&str, &str)] = &[
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
    // `DC` is tmux's terminfo name for Delete; an older tmux types an unknown name into the pane.
    ("delete", "DC"),
];

fn tmux_key_name(key: &Key) -> String {
    if let Some(letter) = key.ctrl() {
        return format!("C-{letter}");
    }
    TMUX_KEYS
        .iter()
        .find(|(name, _)| *name == key.name())
        .map(|(_, tmux)| (*tmux).to_string())
        .expect("every named key has a tmux name (tmux_names_every_key)")
}

fn cells_before(line: &str, cells: usize) -> String {
    let mut width = 0;
    line.chars()
        .take_while(|c| {
            width += unicode_width::UnicodeWidthChar::width(*c).unwrap_or(0);
            width <= cells
        })
        .collect()
}

fn answered_for(target: &str, window: Option<&str>, pane: Option<&str>) -> bool {
    match target.split_once(":=") {
        Some((_, name)) => window == Some(name),
        None => pane == Some(target),
    }
}

fn parse_pane_pid_answer(target: &str, raw: &str) -> Option<u32> {
    let answer = raw.trim_end_matches(['\r', '\n']).lines().last()?;
    let mut fields = answer.splitn(3, ' ');
    let (pid, pane, window) = (fields.next(), fields.next(), fields.next());
    if !answered_for(target, window, pane) {
        return None;
    }
    pid.and_then(|pid| pid.parse().ok())
}

#[derive(Debug, PartialEq, Eq)]
struct PaneLocation {
    window: String,
    size: Option<(u16, u16)>,
    sized_by: Option<String>,
}

fn parse_pane_location(pane: &str, answer: &str) -> Option<PaneLocation> {
    let mut fields = answer.split_whitespace();
    if fields.next() != Some(pane) {
        return None;
    }
    let window = fields
        .next()
        .filter(|id| control_mode::is_valid_window_id(id))?
        .to_string();
    let rows = fields.next().and_then(|n| n.parse::<u16>().ok());
    let cols = fields.next().and_then(|n| n.parse::<u16>().ok());
    Some(PaneLocation {
        window,
        size: rows.zip(cols),
        sized_by: fields.next().map(str::to_string),
    })
}

fn pane_answer_field(raw: &str, n: usize) -> Option<String> {
    normalized_pane_answer(raw)
        .split(PANE_STATE_SEP)
        .nth(n)
        .filter(|field| !field.is_empty())
        .map(str::to_string)
}

const MOUSE_FLAGS_FORMAT_THEN_TITLE: &str = "#{mouse_standard_flag},#{mouse_button_flag},\
     #{mouse_all_flag},#{mouse_any_flag},#{mouse_sgr_flag},#{mouse_utf8_flag}|\
     #{host_short}|#{pane_title}";

const PANE_STATE_SEP: char = '\x1f';

// tmux 3.4 and older run `display-message` answers through vis(3): `\037` for the byte.
const PANE_STATE_SEP_ESCAPED: &str = "\\037";

fn normalized_pane_answer(raw: &str) -> Cow<'_, str> {
    let trimmed = raw.trim_end_matches(['\n', '\r']);
    if trimmed.contains(PANE_STATE_SEP_ESCAPED) {
        Cow::Owned(trimmed.replace(PANE_STATE_SEP_ESCAPED, &PANE_STATE_SEP.to_string()))
    } else {
        Cow::Borrowed(trimmed)
    }
}

// tmux quotes a token holding whitespace.
fn path_from_prefix(start_command: &str) -> Option<String> {
    let mut tokens = start_command.split_ascii_whitespace();
    tokens.next()?;
    tokens.next()?.strip_prefix("PATH=").map(str::to_owned)
}

// tmux prints nothing for a format it cannot expand.
fn parse_pane_state(raw: &str) -> (PaneState, Option<String>, Option<String>) {
    let line = normalized_pane_answer(raw);
    let mut fields = line.split(PANE_STATE_SEP);
    let mut next = || fields.next().filter(|f| !f.is_empty());

    let cursor_row = next().and_then(|f| f.parse().ok());
    let cursor_col = next().and_then(|f| f.parse().ok());
    let command = next().map(str::to_string);
    let cwd = next().map(str::to_string);
    let tty = next().map(str::to_string);
    let dead = next().and_then(|f| match f {
        "1" => Some(true),
        "0" => Some(false),
        _ => None,
    });

    let window = next().map(str::to_string);
    let _ = next();
    // Empty while the pane lives, and for a program killed by a signal.
    let exit_status = next().and_then(|f| f.parse().ok());
    // Empty while the pane lives, for an exit, and before tmux 3.3.
    let exit_signal = next();
    let reaped = match (dead, next()) {
        (Some(true), _) if exit_status.is_some() || exit_signal.is_some() => Some(true),
        // tmux 3.2 has no `pane_dead_signal`, and the floor refuses older.
        (Some(true), Some(version)) if !version.starts_with("3.2") => Some(false),
        _ => None,
    };
    let title = next().map(str::to_string);

    (
        PaneState {
            cursor_row,
            cursor_col,
            foreground_process: command,
            foreground_command: None,
            foreground_cwd: cwd,
            dead,
            exit_status,
            reaped,
            title,
        },
        tty,
        window,
    )
}

fn foreground_process_on_tty(tty: &str) -> Option<(String, String)> {
    // procps and BSD `ps` both take the tty without its `/dev/` prefix.
    let name = tty.strip_prefix("/dev/").unwrap_or(tty);
    let out = Command::new("ps")
        .args(["-o", "pid=,pgid=,tpgid=,args=", "-t", name])
        .output()
        .ok()
        .filter(|out| out.status.success())?;
    parse_ps_foreground(&String::from_utf8_lossy(&out.stdout))
}

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

// `ps` right-aligns numeric columns, so `splitn` would see empty fields.
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

// tmux applies a window option set with `-t <session>` to the session's current window only.
const SESSION_OPTS: &[(&str, &str)] = &[("status", "off"), ("history-limit", "5000")];

const WINDOW_OPTS: &[(&str, &str)] = &[
    // The user's `~/.tmux.conf` is read on this socket too and may set `remain-on-exit on`.
    ("remain-on-exit", "off"),
];

// tmux passes PATH only from an unattached client, and runs one-argument commands in a shell.
pub(crate) fn resolve_local_program(command: &str) -> String {
    if crate::shell::local_platform() == Platform::Windows {
        return command.to_string();
    }
    match crate::path_lookup::resolve_on_path(command) {
        Some(path) => path.to_string_lossy().into_owned(),
        None => command.to_string(),
    }
}

pub(crate) fn resolve_local_program_on(command: &str, task_path: Option<&str>) -> String {
    if crate::shell::local_platform() == Platform::Windows {
        return command.to_string();
    }
    task_path
        .and_then(|path| crate::path_lookup::resolve_on(command, std::ffi::OsStr::new(path)))
        .map(|path| path.to_string_lossy().into_owned())
        .unwrap_or_else(|| resolve_local_program(command))
}

impl<M: TmuxCompatible> Server<M> {
    fn create_local_window(&self, spec: &WindowSpec<'_>, retire: bool) -> Result<String> {
        let session_id = spec.owner.session_id;
        let batch_config =
            M::COMMAND_LISTS && M::ONE_SHOT_SPAWN_ANSWERS && M::VERSION_FLOOR.is_none();
        if !batch_config {
            self.ensure_session_configured()?;
        }
        self.admit_server()?;

        let window_name = window_name_for(spec.role, spec.owner.name);
        // tmux resolves a duplicate window name to the lowest index.
        let create_target = if M::ONE_SHOT_SPAWN_ANSWERS {
            format!("{}:{{end}}", self.session)
        } else {
            format!("{}:", self.session)
        };
        let stamped = M::WINDOW_OPTIONS;
        let before = if M::ONE_SHOT_SPAWN_ANSWERS {
            Vec::new()
        } else {
            self.window_panes(&window_name).with_context(|| {
                format!("list the windows named {window_name} before adding one")
            })?
        };
        let private = PrivateEnv::new(self, spec.env)?;
        let run_new_window = || {
            let mut window = self.new_window_command(
                &window_name,
                &create_target,
                spec.command,
                spec.args,
                spec.cwd,
                &private.env,
            )?;
            if stamped {
                for (option, value) in [
                    (WINDOW_SESSION_OPTION, session_id),
                    (WINDOW_ROLE_OPTION, spec.role.as_str()),
                ] {
                    if !value.is_empty() {
                        window.rest.args([
                            ";",
                            "set-option",
                            "-w",
                            "-t",
                            &create_target,
                            option,
                            value,
                        ]);
                    }
                }
            }
            let (mut tmux, stdin) = self.new_window_process(&window)?;
            run_with_stdin(&mut tmux, stdin.as_deref()).map_err(|e| {
                self.transport.launch_failure(
                    &format!(
                        "Failed to run {} new-window for headless spawn",
                        M::MULTIPLEXER.name()
                    ),
                    e,
                )
            })
        };
        let mut output = run_new_window()?;
        // tmux stops the list at a failed `has-session`, before `new-window`.
        if batch_config
            && !output.status.success()
            && new_window_pane_id(&output.stdout).is_empty()
            && output_answered::<M>(self.transport.is_ssh(), &output) == Some(Missing::Server)
        {
            self.ensure_session_configured()?;
            output = run_new_window()?;
        }
        let deadline = std::time::Instant::now() + M::COLD_START_GRACE;
        loop {
            if output.status.success()
                || !M::RETRY_NO_SERVER_ERROR
                || output_answered::<M>(self.transport.is_ssh(), &output) != Some(Missing::Server)
                || std::time::Instant::now() >= deadline
            {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(250));
            output = run_new_window()?;
        }
        let pane_id = if M::ONE_SHOT_SPAWN_ANSWERS {
            new_window_pane_id(&output.stdout)
        } else {
            String::new()
        };
        if !output.status.success() {
            // tmux exits with the status of the last `run-shell` a hook ran.
            if !control_mode::is_valid_pane_id(&pane_id) {
                let stderr = String::from_utf8_lossy(&output.stderr);
                bail!(
                    "{} new-window exited {} for window {}: {}",
                    M::MULTIPLEXER.name(),
                    output.status,
                    window_name,
                    stderr.trim()
                );
            }
            warn!(
                "{} answered {} for window {} and created it anyway ({}): a hook \
                 on thurbox's own server failed, which is what an uninstalled \
                 plugin's leftover hook does for the life of that server. `{} -L {} \
                 show-hooks -g` names it. Unset the hook rather than killing that \
                 server — it holds every live session",
                M::MULTIPLEXER.name(),
                output.status,
                window_name,
                pane_id,
                self.transport.mux(),
                self.socket()
            );
        }
        let pane_id = if M::ONE_SHOT_SPAWN_ANSWERS {
            pane_id
        } else {
            self.new_named_pane(spec, &window_name, &before)?
        };
        private.keep();
        if stamped && retire {
            let _ = self.retire_duplicate_windows(session_id, spec.role);
        }
        Ok(pane_id)
    }

    // psmux does not reuse pane ids within a server's life.
    fn new_named_pane(
        &self,
        spec: &WindowSpec<'_>,
        window_name: &str,
        before: &[(String, bool)],
    ) -> Result<String> {
        let after = self
            .window_panes(window_name)
            .with_context(|| format!("list the windows named {window_name} after adding one"))?;
        let mut fresh = after
            .into_iter()
            .map(|(id, _)| id)
            .filter(|id| !before.iter().any(|(old, _)| old == id));
        match (fresh.next(), fresh.next()) {
            (Some(id), None) => Ok(id),
            (None, _) => Err(self.spawn_exited(spec, window_name)),
            (Some(_), Some(_)) => bail!(
                "more than one window named {window_name} appeared while creating one, so \
                 which is this session's cannot be told"
            ),
        }
    }

    fn new_window_command(
        &self,
        window_name: &str,
        create_target: &str,
        command: &str,
        args: &[String],
        cwd: Option<&Path>,
        env: &HashMap<String, String>,
    ) -> Result<NewWindow> {
        let configure = M::COMMAND_LISTS && M::ONE_SHOT_SPAWN_ANSWERS && M::VERSION_FLOOR.is_none();
        let config = self.session_config();
        let head = if configure {
            config_command_list(&["has-session", "-t", &self.session], &config)
                .into_iter()
                .map(str::to_string)
                .collect()
        } else {
            Vec::new()
        };
        let mut tmux = Command::new(M::MULTIPLEXER.name());
        tmux.args(["new-window", "-d"]);
        if M::ONE_SHOT_SPAWN_ANSWERS {
            tmux.arg("-a");
        }
        tmux.args(["-t", create_target, "-n", window_name]);
        if M::ONE_SHOT_SPAWN_ANSWERS {
            tmux.args(["-P", "-F", "#{pane_id}"]);
        }
        if let Some(dir) = cwd {
            tmux.args(["-c", &dir.to_string_lossy()]);
        }
        M::push_window_program(&mut tmux, window_name, command, args, env)?;

        if M::WINDOW_SETTINGS && M::ONE_SHOT_SPAWN_ANSWERS {
            for (key, value) in birth_options(window_name) {
                tmux.args([";", "set-window-option", "-t", create_target]);
                tmux.args([key, value]);
            }
        }
        Ok(NewWindow { head, rest: tmux })
    }

    // A tmux command list without `has-session` starts a server where there is none.
    fn new_window_process(&self, window: &NewWindow) -> Result<(Command, Option<String>)> {
        let text = M::SPAWN_ON_STDIN
            .then(|| tmux_command_text(window.rest.get_args()))
            .flatten();
        let mut argv: Vec<&str> = window.head.iter().map(String::as_str).collect();
        if text.is_some() {
            if !argv.is_empty() {
                argv.push(";");
            }
            argv.extend(["source-file", "-"]);
            return Ok((self.mux_command(&argv)?, text));
        }
        if !argv.is_empty() {
            argv.push(";");
        }
        let mut cmd = self.mux_command(&argv)?;
        cmd.args(window.rest.get_args());
        Ok((cmd, None))
    }

    pub(crate) fn new_window_line(
        &self,
        window_name: &str,
        command: &str,
        args: &[String],
        cwd: Option<&Path>,
        env: &HashMap<String, String>,
    ) -> Result<String> {
        // tmux takes joined trailing tokens and `-e`; psmux takes one folded token.
        let shell_cmd = M::window_command(self, window_name, command, args, env)?;
        let cwd_part = match cwd {
            Some(dir) => format!(" -c {}", M::quote(&dir.to_string_lossy())),
            None => String::new(),
        };
        let env_part = M::env_flags(env);
        let escaped_window_name = M::quote(window_name);
        let session = &self.session;
        Ok(format!(
            "new-window -t {session} -n {escaped_window_name} -P -F '{SPAWN_FORMAT}'{cwd_part}{env_part} {shell_cmd}"
        ))
    }

    // tmux issues window ids in order and never reissues one while the server lives.
    fn retire_duplicate_windows(&self, session_id: &str, role: WindowRole) -> Option<String> {
        if !M::WINDOW_OPTIONS || session_id.is_empty() {
            return None;
        }
        let listing = self.retire_listing()?;
        let windows = stamped_windows_in(&listing, session_id, role);
        let keep = windows.last()?.clone();
        (!self
            .retire_windows_in(&listing, session_id, role)
            .is_empty())
        .then_some(keep)
    }

    fn retire_listing(&self) -> Option<String> {
        if self.attached() {
            return self
                .ctrl_command(&format!(
                    "list-windows -t {} -F '{RETIRE_FORMAT}'",
                    self.session
                ))
                .ok();
        }
        let output = self
            .mux_command(&["list-windows", "-t", &self.session, "-F", RETIRE_FORMAT])
            .ok()?
            .output()
            .ok()?;
        output
            .status
            .success()
            .then(|| String::from_utf8_lossy(&output.stdout).into_owned())
    }

    fn retire_windows_in(&self, listing: &str, session_id: &str, role: WindowRole) -> Vec<String> {
        if session_id.is_empty() {
            return Vec::new();
        }
        let windows = stamped_windows_in(listing, session_id, role);
        let Some((keep, retire)) = windows.split_last() else {
            return Vec::new();
        };
        let mut retired = Vec::new();
        for window in retire {
            match self.retire_window(window) {
                Ok(()) => {
                    retired.push(window.clone());
                    warn!(
                        "retired {window}: session {session_id}'s {} stamp is held by {keep}",
                        role.as_str()
                    );
                }
                Err(e) => {
                    warn!("could not retire {window}, stamped for session {session_id}: {e:#}")
                }
            }
        }
        retired
    }

    fn retire_window(&self, window: &str) -> Result<()> {
        if !self.attached() {
            return self.kill_window_at(window);
        }
        let (verb, target) = self.kill_command(window)?;
        match self.ctrl_command(&format!("{verb} -t {target}")) {
            Err(e) if !typed_absence(&e) => Err(e),
            _ => Ok(()),
        }
    }

    fn kill_window_at(&self, target: &str) -> Result<()> {
        let (verb, target) = self.kill_command(target)?;
        let output = self
            .mux_command(&[verb, "-t", &target])?
            .output()
            .with_context(|| format!("Failed to run {} {verb}", M::MULTIPLEXER.name()))?;
        if !output.status.success()
            || (M::EXIT_ZERO_ERRORS && mux_reported_error(&output, self.transport.is_ssh(), false))
        {
            return match self.command_failure(&[verb, "-t", &target], &output) {
                crate::contract::BackendError::Absent(_) => Ok(()),
                error => Err(error.into()),
            };
        }

        Ok(())
    }
}

fn new_window_pane_id(stdout: &[u8]) -> String {
    String::from_utf8_lossy(stdout)
        .lines()
        .next()
        .unwrap_or_default()
        .trim()
        .to_string()
}

struct NewWindow {
    head: Vec<String>,
    rest: Command,
}

fn tmux_command_text<'a>(words: impl Iterator<Item = &'a std::ffi::OsStr>) -> Option<String> {
    let mut line = Vec::new();
    for word in words {
        let word = word.to_str()?;
        line.push(match word {
            ";" => ";".to_string(),
            _ => format!("'{}'", word.replace('\'', "'\\''")),
        });
    }
    Some(line.join(" ") + "\n")
}

fn run_with_stdin(cmd: &mut Command, stdin: Option<&str>) -> std::io::Result<std::process::Output> {
    let Some(text) = stdin else {
        return cmd.output();
    };
    let mut child = cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let written = child
        .stdin
        .take()
        .map(|mut pipe| std::io::Write::write_all(&mut pipe, text.as_bytes()));
    let output = child.wait_with_output()?;
    if let Some(Err(e)) = written {
        if output.stdout.is_empty() && output.stderr.is_empty() {
            return Err(e);
        }
    }
    Ok(output)
}

pub(crate) fn push_posix_window_program(
    tmux: &mut Command,
    command: &str,
    args: &[String],
    env: &HashMap<String, String>,
) {
    for (k, v) in env {
        tmux.args(["-e", &format!("{k}={v}")]);
    }
    let program = resolve_local_program_on(command, task_path(env));
    // tmux runs a one-argument window command through its shell and several through `execvp`.
    if args.is_empty() {
        match shell_prefix_tokens(task_path(env).map(std::ffi::OsStr::new)) {
            Some(mut token) => {
                token.push(program);
                tmux.arg(token.join(" "));
            }
            None => {
                tmux.arg(program);
            }
        }
    } else {
        for arg in path_prefix_args(task_path(env).map(std::ffi::OsStr::new)) {
            tmux.arg(arg);
        }
        tmux.arg(program);
    }
    for a in args {
        tmux.arg(a);
    }
}

// tmux ignores `new-window -e PATH=…`, so the `PATH` rides in front of the program.
#[cfg(not(windows))]
fn path_prefix_args(task_path: Option<&std::ffi::OsStr>) -> Vec<std::ffi::OsString> {
    let (Some(path), Some(env_bin)) = (pane_path(task_path), posix_env_binary()) else {
        return Vec::new();
    };
    // A Unix `PATH` is bytes, not UTF-8.
    let mut assignment = std::ffi::OsString::from("PATH=");
    assignment.push(&path);
    vec![env_bin.into_os_string(), assignment]
}

fn shell_prefix_tokens(task_path: Option<&std::ffi::OsStr>) -> Option<Vec<String>> {
    spelled(&path_prefix_args(task_path))
        .or_else(|| task_path.and_then(|path| spelled(&task_prefix_args(path))))
}

#[cfg(not(windows))]
fn task_prefix_args(task_path: &std::ffi::OsStr) -> Vec<std::ffi::OsString> {
    let (Some(path), Some(env_bin)) = (path_reaching(None, task_path), posix_env_binary()) else {
        return Vec::new();
    };
    let mut assignment = std::ffi::OsString::from("PATH=");
    assignment.push(&path);
    vec![env_bin.into_os_string(), assignment]
}

#[cfg(windows)]
fn task_prefix_args(_task_path: &std::ffi::OsStr) -> Vec<std::ffi::OsString> {
    Vec::new()
}

fn spelled(args: &[std::ffi::OsString]) -> Option<Vec<String>> {
    if args.is_empty() {
        return None;
    }
    args.iter()
        .map(|a| a.to_str().map(control_mode::shell_escape))
        .collect()
}

#[cfg(not(windows))]
fn path_with_program_directory() -> Option<std::ffi::OsString> {
    let dir = crate::instance::program_dir()?;
    path_led_by(&dir, &std::env::var_os("PATH").unwrap_or_default())
}

#[cfg(not(windows))]
fn pane_path(task_path: Option<&std::ffi::OsStr>) -> Option<std::ffi::OsString> {
    let Some(task_path) = task_path else {
        return path_with_program_directory();
    };
    path_reaching(crate::instance::program_dir().as_deref(), task_path)
}

#[cfg(not(windows))]
fn path_reaching(dir: Option<&Path>, task_path: &std::ffi::OsStr) -> Option<std::ffi::OsString> {
    let dirs: Vec<std::path::PathBuf> = std::env::split_paths(task_path)
        .filter(|d| !d.as_os_str().is_empty())
        .collect();
    let lead = dir.filter(|dir| !dirs.iter().any(|d| d == dir));
    std::env::join_paths(lead.map(Path::to_path_buf).into_iter().chain(dirs)).ok()
}

fn task_path(env: &HashMap<String, String>) -> Option<&str> {
    env.get("PATH").map(String::as_str)
}

// POSIX reads an empty `PATH` component as the current directory.
#[cfg(not(windows))]
fn path_led_by(dir: &Path, inherited: &std::ffi::OsStr) -> Option<std::ffi::OsString> {
    let dirs = std::iter::once(dir.to_path_buf())
        .chain(std::env::split_paths(inherited).filter(|d| d != dir && !d.as_os_str().is_empty()))
        .collect::<Vec<_>>();
    std::env::join_paths(dirs).ok()
}

#[cfg(not(windows))]
fn posix_env_binary() -> Option<std::path::PathBuf> {
    crate::path_lookup::resolve_on_path("env").or_else(|| {
        let posix = std::path::PathBuf::from("/usr/bin/env");
        posix.is_file().then_some(posix)
    })
}

#[cfg(windows)]
fn path_prefix_args(_task_path: Option<&std::ffi::OsStr>) -> Vec<std::ffi::OsString> {
    Vec::new()
}

pub(crate) fn is_version_answer(answer: &str) -> bool {
    let parts: Vec<&str> = answer.split('.').collect();
    parts.len() >= 2
        && parts
            .iter()
            .all(|part| part.bytes().all(|b| b.is_ascii_digit()) && part.parse::<u32>().is_ok())
}

enum Admission {
    NothingThere,
    Admitted,
}

fn typed_absence(error: &anyhow::Error) -> bool {
    matches!(
        error.downcast_ref::<crate::contract::BackendError>(),
        Some(crate::contract::BackendError::Absent(_))
    )
}

#[cfg(all(test, unix))]
pub(crate) fn assert_absence_replies<M: TmuxCompatible>(replies: &[(&str, bool)]) {
    use std::os::unix::process::ExitStatusExt;
    let backend = Server::<M>::local(&crate::settings::for_tests());
    let name = M::MULTIPLEXER.name();
    for &(reply, holds_nothing) in replies {
        let output = std::process::Output {
            status: std::process::ExitStatus::from_raw(1 << 8),
            stdout: Vec::new(),
            stderr: format!("{reply}\n").into_bytes(),
        };
        assert!(
            matches!(
                backend.command_failure(&["list-windows"], &output),
                crate::BackendError::Absent(_)
            ),
            "{name} one-shot: {reply}"
        );
        assert!(
            matches!(
                backend.refusal(reply, String::new()),
                crate::BackendError::Absent(_)
            ),
            "{name} control mode: {reply}"
        );
        assert_eq!(
            listing_output_is_absence::<M>(false, &output),
            holds_nothing,
            "{name} listing: {reply}"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tmux_before_3_7_refusing_the_unsanitised_paste_is_told_apart() {
        let refused = anyhow::anyhow!("tmux paste-buffer command paste-buffer: unknown flag -S");
        assert!(refused_unsanitised_paste(&refused));
        let gone = anyhow::anyhow!("tmux paste-buffer can't find pane: %9");
        assert!(!refused_unsanitised_paste(&gone));
    }

    #[test]
    fn a_new_window_is_spelled_as_one_line_tmux_parses_back() {
        let words = [
            "new-window",
            "-e",
            "SECRET=it's $HOME ~ `x` #c\nline2",
            ";",
            "set-option",
            "#{pane_id}",
        ];
        assert_eq!(
            tmux_command_text(words.iter().map(std::ffi::OsStr::new)).unwrap(),
            "'new-window' '-e' 'SECRET=it'\\''s $HOME ~ `x` #c\nline2' ; 'set-option' '#{pane_id}'\n"
        );
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStrExt;
            let bad = std::ffi::OsStr::from_bytes(b"PATH=\xff");
            assert!(tmux_command_text([bad].into_iter()).is_none());
        }
    }

    #[test]
    fn a_pane_pid_answer_is_read_past_pane_output_in_the_reply() {
        let raw = "%output %0 \\033[?1h\\033=\\033[?2004h\n1826732 %1 tb-live\n";
        assert_eq!(parse_pane_pid_answer("%1", raw), Some(1826732));
        assert_eq!(parse_pane_pid_answer("s:=tb-live", raw), Some(1826732));
        assert_eq!(parse_pane_pid_answer("s:=tb-gone", raw), None);
        assert_eq!(
            parse_pane_pid_answer("s:=tb-two words", "7 %2 tb-two words"),
            Some(7)
        );
    }

    #[test]
    fn an_adopt_seed_is_handed_over_in_bounded_pieces_ahead_of_live_output() {
        let seed: Vec<u8> = (0..10_000u32).map(|i| (i % 251) as u8).collect();
        let live = Box::new(std::iter::once(PaneChunk::Output(b"live".to_vec())));
        let chunks: Vec<Vec<u8>> = seeded(seed.clone(), live)
            .map(|chunk| match chunk {
                PaneChunk::Output(bytes) => bytes,
                other => panic!("expected output, got {other:?}"),
            })
            .collect();
        let (last, history) = chunks.split_last().expect("chunks");
        assert_eq!(last, b"live");
        assert_eq!(
            history.iter().map(Vec::len).collect::<Vec<_>>(),
            [4096, 4096, 1808],
            "seed pieces"
        );
        assert_eq!(history.concat(), seed);
    }

    use crate::identity::agent_window_name;
    use crate::instance::{host_socket, known_host_socket, learn_host_socket, TMUX_SOCKET};
    use crate::tmux_compat::control_mode::PANE_CHANNEL_CAPACITY;

    struct TestMux;

    type TestBackend = Server<TestMux>;

    #[test]
    fn a_private_env_is_discarded_unless_the_window_exists() {
        let backend = TestBackend::with_transport(
            TmuxTransport::local("tmux"),
            "thurbox-test",
            "thurbox-test",
            "local:tmux",
            crate::settings::DEFAULT_SCROLLBACK_LINES,
        );
        let env = HashMap::from([("K".to_string(), "v".to_string())]);
        drop(PrivateEnv::new(&backend, &env).expect("written"));
        assert_eq!(
            DISCARDED.with(|d| d.borrow().clone()),
            std::slice::from_ref(&env)
        );
        PrivateEnv::new(&backend, &env).expect("written").keep();
        assert_eq!(
            DISCARDED.with(|d| d.borrow().len()),
            1,
            "a kept env is the window's"
        );
    }

    #[cfg(unix)]
    #[test]
    fn an_unfinished_or_mixed_process_reply_is_not_typed_absence() {
        use std::os::unix::process::ExitStatusExt;
        let backend = TestBackend::local(&crate::settings::for_tests());
        for (status, stdout) in [
            (libc::SIGKILL, Vec::new()),
            (1 << 8, b"partial listing".to_vec()),
        ] {
            let output = std::process::Output {
                status: std::process::ExitStatus::from_raw(status),
                stdout,
                stderr: b"can't find session: thurbox".to_vec(),
            };
            assert!(
                !matches!(
                    backend.command_failure(&["list-windows"], &output),
                    crate::BackendError::Absent(_)
                ),
                "an incomplete answer cannot authorize relaunch"
            );
        }
    }

    #[test]
    fn cells_before_counts_a_wide_character_as_two_columns() {
        assert_eq!(cells_before("❯ hi", 2), "❯ ");
        assert_eq!(cells_before("> 漢字x", 4), "> 漢");
        assert_eq!(cells_before("short", 40), "short");
    }

    struct TypedKeys;

    impl PaneInput for TypedKeys {
        fn send_keys(&self, pane_id: &str, buf: &[u8]) -> Vec<String> {
            control_mode::hex_send_keys_commands(pane_id, buf)
        }
        fn paste(&self, _: &str, _: &str) -> Option<Result<()>> {
            None
        }
    }

    thread_local! {
        static DISCARDED: std::cell::RefCell<Vec<HashMap<String, String>>> =
            const { std::cell::RefCell::new(Vec::new()) };
    }

    impl TmuxCompatible for TestMux {
        fn discard_private_env(_: &Server<Self>, env: &HashMap<String, String>) {
            DISCARDED.with(|d| d.borrow_mut().push(env.clone()));
        }

        const MULTIPLEXER: Multiplexer = Multiplexer::Tmux;
        const WINDOW_OPTIONS: bool = true;
        const WINDOW_SETTINGS: bool = true;
        const WINDOW_EVENTS: bool = true;
        const PANE_MONITORING: bool = true;
        const SNAPSHOTS: bool = true;
        const COMMAND_LISTS: bool = true;
        const COMMAND_LIST_SINGLE_REPLY: bool = false;
        const ONE_SHOT_SPAWN_ANSWERS: bool = true;
        const CONDITIONAL_RESIZE: bool = true;
        const SERVER_SCOPE: &str = "-s";
        const DISPLAY_FLAGS: &[&str] = &[];
        const VERSION_FLOOR: Option<fn(&str, &str) -> Result<()>> = None;
        fn check_banner(_: &str, _: &str) -> Result<()> {
            Ok(())
        }
        fn session_config(_: &str) -> Vec<ConfigOption> {
            Vec::new()
        }
        fn quote(arg: &str) -> String {
            shell_escape(arg)
        }
        fn env_flags(_: &HashMap<String, String>) -> String {
            String::new()
        }
        fn window_command(
            server: &Server<Self>,
            window_name: &str,
            command: &str,
            args: &[String],
            env: &HashMap<String, String>,
        ) -> Result<String> {
            Ok(server.posix_window_command(window_name, command, args, task_path(env)))
        }
        fn push_window_program(
            cmd: &mut Command,
            _: &str,
            command: &str,
            args: &[String],
            env: &HashMap<String, String>,
        ) -> Result<()> {
            push_posix_window_program(cmd, command, args, env);
            Ok(())
        }
        fn paste_args(_: &str, _: &str) -> Vec<Vec<String>> {
            Vec::new()
        }
        fn deferred_paste_script(_: &str, _: &str, _: &str, _: &str) -> Result<String> {
            Ok(String::new())
        }
        fn pane_input(_: &TmuxTransport, _: &str) -> Arc<dyn PaneInput> {
            Arc::new(TypedKeys)
        }
        fn control_policy(_: &TmuxTransport, _: &str) -> ControlPolicy {
            ControlPolicy {
                flow_control_command: Some("refresh-client -f pause-after=5"),
                implicit_attach_reply: true,
                tagged_blocks: true,
                command_list_single_reply: false,
                subscriptions: true,
                status_poll: None,
                confirm_close: None,
            }
        }
        const HOOK_STATUS: bool = true;

        fn hook_signal_command(_: &Server<Self>) -> String {
            "tmux set-option -p @thurbox_state ".to_string()
        }
    }

    #[cfg(unix)]
    struct UnmonitoredMux;

    #[cfg(unix)]
    impl TmuxCompatible for UnmonitoredMux {
        const MULTIPLEXER: Multiplexer = Multiplexer::Tmux;
        const WINDOW_OPTIONS: bool = false;
        const WINDOW_SETTINGS: bool = false;
        const WINDOW_EVENTS: bool = false;
        const PANE_MONITORING: bool = false;
        const SNAPSHOTS: bool = false;
        const COMMAND_LISTS: bool = false;
        const COMMAND_LIST_SINGLE_REPLY: bool = false;
        const ONE_SHOT_SPAWN_ANSWERS: bool = false;
        const CONDITIONAL_RESIZE: bool = false;
        const SERVER_SCOPE: &str = "-s";
        const DISPLAY_FLAGS: &[&str] = &[];
        const VERSION_FLOOR: Option<fn(&str, &str) -> Result<()>> = None;

        fn check_banner(_: &str, _: &str) -> Result<()> {
            Ok(())
        }
        fn session_config(_: &str) -> Vec<ConfigOption> {
            Vec::new()
        }
        fn paste_args(_: &str, _: &str) -> Vec<Vec<String>> {
            Vec::new()
        }
        fn deferred_paste_script(_: &str, _: &str, _: &str, _: &str) -> Result<String> {
            Ok(String::new())
        }
        fn pane_input(_: &TmuxTransport, _: &str) -> Arc<dyn PaneInput> {
            Arc::new(TypedKeys)
        }
        fn control_policy(_: &TmuxTransport, _: &str) -> ControlPolicy {
            ControlPolicy {
                flow_control_command: None,
                implicit_attach_reply: false,
                tagged_blocks: false,
                command_list_single_reply: false,
                subscriptions: false,
                status_poll: None,
                confirm_close: None,
            }
        }
        const HOOK_STATUS: bool = false;
        fn hook_signal_command(_: &Server<Self>) -> String {
            String::new()
        }
    }

    #[cfg(unix)]
    struct FlooredMux;

    #[cfg(unix)]
    fn refuse_below_two(version: &str, _: &str) -> Result<()> {
        match version.trim() {
            "1.0" => bail!("1.0 is too old"),
            _ => Ok(()),
        }
    }

    #[cfg(unix)]
    impl TmuxCompatible for FlooredMux {
        const MULTIPLEXER: Multiplexer = Multiplexer::Tmux;
        const WINDOW_OPTIONS: bool = false;
        const WINDOW_SETTINGS: bool = false;
        const WINDOW_EVENTS: bool = false;
        const PANE_MONITORING: bool = false;
        const SNAPSHOTS: bool = false;
        const COMMAND_LISTS: bool = false;
        const COMMAND_LIST_SINGLE_REPLY: bool = false;
        const ONE_SHOT_SPAWN_ANSWERS: bool = false;
        const CONDITIONAL_RESIZE: bool = false;
        const SERVER_SCOPE: &str = "-s";
        const DISPLAY_FLAGS: &[&str] = &[];
        const VERSION_FLOOR: Option<fn(&str, &str) -> Result<()>> = Some(refuse_below_two);

        fn check_banner(_: &str, _: &str) -> Result<()> {
            Ok(())
        }
        fn session_config(_: &str) -> Vec<ConfigOption> {
            Vec::new()
        }
        fn paste_args(_: &str, _: &str) -> Vec<Vec<String>> {
            Vec::new()
        }
        fn deferred_paste_script(_: &str, _: &str, _: &str, _: &str) -> Result<String> {
            Ok(String::new())
        }
        fn pane_input(_: &TmuxTransport, _: &str) -> Arc<dyn PaneInput> {
            Arc::new(TypedKeys)
        }
        fn control_policy(_: &TmuxTransport, _: &str) -> ControlPolicy {
            ControlPolicy {
                flow_control_command: None,
                implicit_attach_reply: false,
                tagged_blocks: false,
                command_list_single_reply: false,
                subscriptions: false,
                status_poll: None,
                confirm_close: None,
            }
        }
        const HOOK_STATUS: bool = false;
        fn hook_signal_command(_: &Server<Self>) -> String {
            String::new()
        }
    }

    #[cfg(unix)]
    fn floored_mux(
        root: &std::path::Path,
        answer: &str,
    ) -> (Server<FlooredMux>, impl Fn() -> usize) {
        use std::os::unix::fs::PermissionsExt;

        let mux = root.join("floored-mux");
        std::fs::write(
            &mux,
            format!(
                "#!/bin/sh\ncase \"$*\" in\n\
                   *' -C '*) while IFS= read -r line; do\n\
                       printf '%s\\n' \"$line\" >> \"$0.log\"\n\
                       printf '%%begin 1 1 0\\n'\n\
                       case \"$line\" in *'#{{version}}'*) printf '{answer}' ;; esac\n\
                       printf '%%end 1 1 0\\n'\n\
                     done ;;\n\
                   *' -V'*) printf 'tmux 2.0\\n' ;;\n\
                   *) printf '%s\\n' \"$*\" >> \"$0.log\"\n\
                      case \"$*\" in *'#{{version}}'*) printf '{answer}' ;; esac ;;\n\
                 esac\n"
            ),
        )
        .expect("write fake mux");
        std::fs::set_permissions(&mux, std::fs::Permissions::from_mode(0o700)).expect("chmod");
        let log = root.join("floored-mux.log");
        let asked = move || {
            std::fs::read_to_string(&log)
                .unwrap_or_default()
                .matches("#{version}")
                .count()
        };
        let backend = Server::<FlooredMux>::with_transport(
            TmuxTransport::local(mux.to_string_lossy()),
            "unused",
            "unused",
            "local:tmux",
            crate::settings::DEFAULT_SCROLLBACK_LINES,
        );
        (backend, asked)
    }

    #[cfg(unix)]
    #[test]
    fn each_new_connection_asks_the_server_its_version_once() {
        let root = tempfile::tempdir().expect("tempdir");
        let (backend, asked) = floored_mux(root.path(), "2.0\\n");

        backend.target("%1").expect("admitted by a one-shot");
        assert_eq!(asked(), 1);
        backend.ensure_control().expect("control client");
        backend.target("%1").expect("admitted on the connection");
        backend.target("%2").expect("still admitted");
        assert_eq!(asked(), 2);
        backend.reconnect_control(None).expect("replaced");
        backend.target("%1").expect("admitted on the replacement");
        assert_eq!(asked(), 3);
    }

    #[test]
    fn only_dotted_numbers_are_a_version_answer() {
        for answer in ["3.3.8", "3.4", "10.0.1"] {
            assert!(is_version_answer(answer), "{answer}");
        }
        for answer in [
            "",
            " ",
            "3",
            "3.",
            ".3",
            "3.3.7a",
            "tmux 3.3.8",
            "3.3.8\n3.3.7",
            "ERROR: x",
            "3.4294967296",
            "+3.3",
        ] {
            assert!(!is_version_answer(answer), "{answer:?}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn the_loops_name_no_pane_on_a_connection_not_admitted() {
        let resized = |root: &std::path::Path| {
            std::fs::read_to_string(root.join("floored-mux.log"))
                .unwrap_or_default()
                .matches("resize-window")
                .count()
        };

        let root = tempfile::tempdir().expect("tempdir");
        let (backend, _asked) = floored_mux(root.path(), "2.0\\n");
        backend.ensure_control().expect("control client");
        backend.resize("%1", 24, 80).expect("skipped, not failed");
        assert!(backend.is_dead("%1").is_err());
        assert_eq!(resized(root.path()), 0, "named before admission");
        backend.target("%1").expect("admitted");
        backend.resize("%1", 24, 80).expect("resized");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while resized(root.path()) == 0 && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert_eq!(resized(root.path()), 1);

        let root = tempfile::tempdir().expect("tempdir");
        let (backend, _asked) = floored_mux(root.path(), "1.0\\n");
        backend.ensure_control().expect("control client");
        assert!(backend.target("%1").is_err());
        backend.resize("%1", 24, 80).expect("skipped, not failed");
        assert!(backend.is_dead("%1").is_err());
        assert_eq!(resized(root.path()), 0, "named to a refused server");
    }

    #[cfg(unix)]
    #[test]
    fn a_shut_down_connection_takes_its_admission_with_it() {
        let root = tempfile::tempdir().expect("tempdir");
        let (backend, asked) = floored_mux(root.path(), "2.0\\n");
        backend.ensure_control().expect("control client");
        backend.target("%1").expect("admitted on the connection");
        assert_eq!(asked(), 1);
        backend.shutdown();
        backend.target("%1").expect("asked again with a one-shot");
        assert_eq!(asked(), 2);
    }

    #[cfg(unix)]
    fn refusing_mux(root: &std::path::Path, refuse: &str, reply: &str) -> TestBackend {
        use std::os::unix::fs::PermissionsExt;

        let mux = root.join("refusing-mux");
        std::fs::write(
            &mux,
            format!(
                "#!/bin/sh\nreply=\"{reply}\"\nprintf '%%begin 1 1 0\\n%%end 1 1 0\\n'\n\
                 while IFS= read -r line; do\n\
                   case \"$line\" in\n\
                     *'{refuse}'*) printf '%%begin 2 2 0\\n%s\\n%%error 2 2 0\\n' \"$reply\" ;;\n\
                     *) printf '%%begin 2 2 0\\n%%end 2 2 0\\n' ;;\n\
                   esac\n\
                 done\n"
            ),
        )
        .expect("write fake mux");
        std::fs::set_permissions(&mux, std::fs::Permissions::from_mode(0o700)).expect("chmod");
        TestBackend::with_transport(
            TmuxTransport::local(mux.to_string_lossy()),
            "unused",
            "unused",
            "local:tmux",
            crate::settings::DEFAULT_SCROLLBACK_LINES,
        )
    }

    #[cfg(unix)]
    #[test]
    fn a_control_mode_error_is_typed_by_the_adapter() {
        use crate::contract::BackendError;

        let kind = |error: anyhow::Error| match error.downcast::<BackendError>() {
            Ok(BackendError::Absent(_)) => "absent",
            Ok(BackendError::Refused(_)) => "refused",
            Ok(BackendError::Busy(_)) => "busy",
            other => panic!("untyped: {other:?}"),
        };
        let roots: Vec<_> = (0..3)
            .map(|_| tempfile::tempdir().expect("tempdir"))
            .collect();

        let backend = refusing_mux(roots[0].path(), "pause-after", "can't find session: unused");
        let error = backend.ensure_control().expect_err("flow control refused");
        assert_eq!(kind(error), "absent");

        let backend = refusing_mux(roots[1].path(), "kill-window", "can't find window: @9");
        backend.ensure_control().expect("control client");
        let error = backend
            .ctrl_command("kill-window -t @9")
            .expect_err("refused");
        assert_eq!(kind(error), "absent");

        let backend = refusing_mux(
            roots[2].path(),
            "kill-window",
            "unknown command: kill-window",
        );
        backend.ensure_control().expect("control client");
        let error = backend
            .ctrl_command("kill-window -t @9")
            .expect_err("refused");
        assert_eq!(kind(error), "refused");
    }

    #[cfg(unix)]
    #[test]
    fn a_connection_that_answers_no_version_names_no_pane() {
        let root = tempfile::tempdir().expect("tempdir");
        let (backend, _asked) = floored_mux(root.path(), "");
        backend.ensure_control().expect("control client");
        let err = backend.target("%1").unwrap_err().to_string();
        assert!(err.contains("the server did not say its version"), "{err}");
    }

    #[cfg(unix)]
    #[test]
    fn an_unmonitored_pane_streams_output_without_refreshing_on_attach_or_detach() {
        use std::os::unix::fs::PermissionsExt;

        let root = tempfile::tempdir().expect("tempdir");
        let mux = root.path().join("unmonitored-mux");
        std::fs::write(
            &mux,
            "#!/bin/sh\nwhile IFS= read -r line; do\n\
             printf '%s\\n' \"$line\" >> \"$0.log\"\n\
             case \"$line\" in\n\
               *'refresh-client -A'*) printf '%%begin 1 1 0\\n%%error 1 1 0\\n' ;;\n\
               *' ; '*) printf '%%begin 1 1 0\\n%%end 1 1 0\\n%%begin 1 1 0\\n%%end 1 1 0\\n%%output %%1 streamed\\n' ;;\n\
               *) printf '%%begin 1 1 0\\n%%end 1 1 0\\n' ;;\n\
             esac\n\
             done\n",
        )
        .expect("write fake mux");
        std::fs::set_permissions(&mux, std::fs::Permissions::from_mode(0o700)).expect("chmod");
        let transport = TmuxTransport::local(mux.to_string_lossy());
        let backend = Server::<UnmonitoredMux>::with_transport(
            transport.clone(),
            "unused",
            "unused",
            "local:tmux",
            crate::settings::DEFAULT_SCROLLBACK_LINES,
        );
        let control = ControlMode::start(
            &transport,
            "unused",
            "unused",
            "tests",
            &UnmonitoredMux::control_policy(&transport, "unused"),
        )
        .expect("control client");
        *backend.control.lock().unwrap() = Some(Arc::new(control));

        let mut pane = backend
            .connect_pane("%1", Some("@1"), 24, 80)
            .expect("attach without monitoring command");
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        let reader = std::thread::spawn(move || {
            let output = pane.output.find_map(|chunk| match chunk {
                PaneChunk::Output(bytes) => Some(bytes),
                _ => None,
            });
            let _ = tx.send(output);
        });
        let output = rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("streamed output arrived")
            .expect("streamed output read");
        reader.join().expect("reader thread");
        assert_eq!(output, b"streamed");
        backend.detach("%1").expect("detach");
        drop(backend);
        let commands = std::fs::read_to_string(mux.with_extension("log")).expect("command log");
        assert!(commands.contains("resize-window"), "{commands}");
        assert!(!commands.contains("refresh-client -A"), "{commands}");
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_overflowed_pane_is_caught_up_without_blocking_control() {
        use std::os::unix::fs::PermissionsExt;
        use std::time::Duration;

        let root = tempfile::tempdir().expect("tempdir");
        let mux = root.path().join("overflow-mux");
        std::fs::write(
            &mux,
            format!(
                r#"#!/bin/sh
reply() {{
    printf '%%begin 1 1 0\n'
    [ "$#" -eq 0 ] || printf '%s\n' "$1"
    printf '%%end 1 1 0\n'
}}
while IFS= read -r line; do
    case "$line" in
        *'display-message -p flood'*)
            i=0
            while [ "$i" -lt {chunks} ]; do
                printf '%s\n' '%output %1 \033['
                i=$((i + 1))
            done
            printf '%s\n' '%output %1 32mRECOVERED\033[0m'
            reply ;;
        *'capture-pane'*)
            reply '20 4 9 0 0'
            reply "$(printf '\033[32mRECOVERED\033[0m')"
            reply ;;
        *'display-message -p suffix'*)
            printf '%s\n' '%output %1 !'
            reply ;;
        *'resize-window'*)
            reply; reply; reply; reply; reply ;;
        *'#{{pane_id}} #{{window_id}}'*) reply '%1 @1 4 20' ;;
        *) reply ;;
    esac
done
"#,
                chunks = PANE_CHANNEL_CAPACITY + 64,
            ),
        )
        .expect("write control server");
        std::fs::set_permissions(&mux, std::fs::Permissions::from_mode(0o700)).expect("chmod");
        let transport = TmuxTransport::local(mux.to_string_lossy());
        let backend = Arc::new(TestBackend::with_transport(
            transport.clone(),
            "unused",
            "unused",
            "local:tmux",
            crate::settings::DEFAULT_SCROLLBACK_LINES,
        ));
        struct Shutdown(Arc<TestBackend>);
        impl Drop for Shutdown {
            fn drop(&mut self) {
                self.0.shutdown();
            }
        }
        let _shutdown = Shutdown(Arc::clone(&backend));
        let control = Arc::new(
            ControlMode::start(
                &transport,
                "unused",
                "unused",
                "tests",
                &ControlPolicy {
                    flow_control_command: None,
                    implicit_attach_reply: false,
                    tagged_blocks: true,
                    command_list_single_reply: false,
                    subscriptions: false,
                    status_poll: None,
                    confirm_close: None,
                },
            )
            .expect("control client"),
        );
        *backend.control.lock().unwrap() = Some(Arc::clone(&control));
        let (reader, _) = backend
            .register_pane("%1", Some("@1"))
            .expect("resident pane");

        control
            .send_command("display-message -p flood")
            .expect("control replies still arrive while the pane channel is full");
        control
            .send_command("display-message -p barrier")
            .expect("another command is not held behind the flooded pane");

        let (items, received) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            for chunk in reader {
                if items.send(chunk).is_err() {
                    return;
                }
            }
        });
        let next = || {
            received
                .recv_timeout(Duration::from_secs(5))
                .expect("overflowed resident pane was never caught up")
        };

        let mut queued = 0;
        let snapshot = loop {
            match next() {
                PaneChunk::Output(bytes) => {
                    assert_eq!(bytes, b"\x1b[", "only output from before the gap");
                    queued += 1;
                }
                PaneChunk::Resync(snapshot) => break snapshot,
                other => panic!("a resync, which replaces a grid it has, not {other:?}"),
            }
        };
        assert!(queued <= PANE_CHANNEL_CAPACITY, "{queued}");
        assert_eq!(
            *snapshot,
            crate::contract::PaneSnapshot {
                cols: 20,
                rows: 4,
                cursor: (9, 0),
                normal: vec!["\x1b[32mRECOVERED\x1b[0m".into()],
                alternate: None,
            }
        );
        control
            .send_command("display-message -p suffix")
            .expect("output after recovery");
        assert!(
            matches!(next(), PaneChunk::Output(bytes) if bytes == b"!"),
            "output flows again"
        );
    }

    #[cfg(unix)]
    fn stalling_backend(root: &std::path::Path) -> (Server<UnmonitoredMux>, std::path::PathBuf) {
        use std::os::unix::fs::PermissionsExt;

        let mux = root.join("stalling-mux");
        std::fs::write(
            &mux,
            format!(
                "#!/bin/sh\nprintf 'start\\n' >> \"$0.log\"\n\
                 [ -e \"$0.slow\" ] && sleep {secs}\n\
                 while IFS= read -r line; do\n\
                 printf '%s\\n' \"$line\" >> \"$0.log\"\n\
                 case \"$line\" in\n\
                   list-windows*) sleep {secs}; printf '%%begin 1 1 0\\n%%end 1 1 0\\n' ;;\n\
                   *' ; '*) printf '%%begin 1 1 0\\n%%end 1 1 0\\n%%begin 1 1 0\\n%%end 1 1 0\\n' ;;\n\
                   *) printf '%%begin 1 1 0\\n%%end 1 1 0\\n' ;;\n\
                 esac\n\
                 done\n",
                secs = STALL.as_secs()
            ),
        )
        .expect("write fake mux");
        std::fs::set_permissions(&mux, std::fs::Permissions::from_mode(0o700)).expect("chmod");
        let transport = TmuxTransport::local(mux.to_string_lossy());
        let backend = Server::<UnmonitoredMux>::with_transport(
            transport,
            "unused",
            "unused",
            "local:tmux",
            crate::settings::DEFAULT_SCROLLBACK_LINES,
        );
        backend.ensure_control().expect("control client");
        (backend, mux)
    }

    #[cfg(unix)]
    const STALL: std::time::Duration = std::time::Duration::from_secs(1);

    #[cfg(unix)]
    fn await_log(mux: &std::path::Path, seen: impl Fn(&str) -> bool) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !std::fs::read_to_string(mux.with_extension("log")).is_ok_and(|log| seen(&log)) {
            assert!(
                std::time::Instant::now() < deadline,
                "fake mux never got there"
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_paint_is_not_held_behind_another_callers_stalled_round_trip() {
        let root = tempfile::tempdir().expect("tempdir");
        let (backend, mux) = stalling_backend(root.path());
        let backend = Arc::new(backend);
        let slow = std::thread::spawn({
            let backend = Arc::clone(&backend);
            move || backend.ctrl_command("list-windows -a")
        });
        await_log(&mux, |log| log.contains("list-windows"));

        let began = std::time::Instant::now();
        let resized = backend.resize("%1", 24, 80);
        let took = began.elapsed();

        assert!(resized.is_ok(), "{resized:?}");
        assert!(took < LOOP_COMMAND_BUDGET / 2, "the paint waited {took:?}");
        slow.join().expect("slow caller").expect("slow answer");
    }

    #[cfg(unix)]
    #[test]
    fn a_paint_is_not_held_behind_a_slow_reconnect() {
        let root = tempfile::tempdir().expect("tempdir");
        let (backend, mux) = stalling_backend(root.path());
        let backend = Arc::new(backend);
        std::fs::write(mux.with_extension("slow"), "").expect("slow marker");
        let reconnect = std::thread::spawn({
            let backend = Arc::clone(&backend);
            move || backend.reconnect_control(None)
        });
        await_log(&mux, |log| log.matches("start").count() == 2);

        let began = std::time::Instant::now();
        let resized = backend.resize("%1", 24, 80);
        let took = began.elapsed();

        assert!(resized.is_ok(), "{resized:?}");
        assert!(took < LOOP_COMMAND_BUDGET / 2, "the paint waited {took:?}");
        reconnect.join().expect("reconnect").expect("reconnected");
    }

    #[cfg(unix)]
    #[test]
    fn a_command_in_flight_across_a_reconnect_runs_once_and_is_answered() {
        let root = tempfile::tempdir().expect("tempdir");
        let (backend, mux) = stalling_backend(root.path());
        let backend = Arc::new(backend);
        let first = backend.connection().expect("connection");
        let in_flight = std::thread::spawn({
            let backend = Arc::clone(&backend);
            move || backend.ctrl_command("list-windows -a")
        });
        await_log(&mux, |log| log.contains("list-windows"));

        backend
            .reconnect_control(Some(&first))
            .expect("reconnected");
        let answered = in_flight.join().expect("in-flight caller");

        assert!(answered.is_ok(), "{answered:?}");
        let log = std::fs::read_to_string(mux.with_extension("log")).expect("log");
        assert_eq!(log.matches("list-windows").count(), 1, "{log}");
    }

    #[cfg(unix)]
    #[test]
    fn a_command_whose_answer_was_lost_is_not_resent() {
        let root = tempfile::tempdir().expect("tempdir");
        let (backend, mux) = stalling_backend(root.path());
        let backend = Arc::new(backend);
        let first = backend.connection().expect("connection");
        let in_flight = std::thread::spawn({
            let backend = Arc::clone(&backend);
            move || backend.ctrl_command("list-windows -a")
        });
        await_log(&mux, |log| log.contains("list-windows"));

        first.close();
        let answered = in_flight.join().expect("in-flight caller");

        assert!(answered.is_err(), "{answered:?}");
        let log = std::fs::read_to_string(mux.with_extension("log")).expect("log");
        assert_eq!(log.matches("list-windows").count(), 1, "{log}");
    }

    #[cfg(unix)]
    #[test]
    fn a_quit_during_a_reconnect_closes_the_replaced_connection() {
        let root = tempfile::tempdir().expect("tempdir");
        let (backend, _mux) = stalling_backend(root.path());
        let backend = Arc::new(backend);
        let old = backend.connection().expect("connection");
        let fresh = Arc::new(
            ControlMode::start(
                &backend.located_transport(),
                &backend.socket(),
                &backend.session,
                &backend.sizer,
                &UnmonitoredMux::control_policy(&backend.transport, &backend.session),
            )
            .expect("second connection"),
        );

        let parked = backend.retiring.lock().expect("retiring lock");
        let swap = std::thread::spawn({
            let (backend, fresh) = (Arc::clone(&backend), Arc::clone(&fresh));
            move || backend.swap(&fresh).map(|old| old.is_some())
        });
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(300);
        let mut visible = false;
        while !visible && std::time::Instant::now() < deadline {
            let current = backend.control.try_lock().ok().and_then(|c| c.clone());
            visible = current.is_some_and(|c| Arc::ptr_eq(&c, &fresh));
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        drop(parked);
        let swapped = swap.join().expect("swap");
        backend.shutdown();

        assert!(
            !visible,
            "the swap showed before the old connection was recorded"
        );
        assert!(swapped.is_ok_and(|replaced| replaced), "nothing replaced");
        let answer = old.send_command_within("display-message -p ''", LOOP_COMMAND_BUDGET);
        assert!(
            answer.is_err(),
            "the replaced connection outlived the quit: {answer:?}"
        );
    }

    #[test]
    #[cfg(unix)]
    fn a_local_window_command_is_an_absolute_path() {
        let dir = tempfile::TempDir::new().unwrap();
        let expected = agent_only_thurbox_can_see(dir.path(), "tbx-spawn-probe");

        let (local, free, remote) = crate::path_lookup::with_path(dir.path(), || {
            (
                TestBackend::local(&crate::settings::for_tests())
                    .program_for_window("tbx-spawn-probe", None),
                resolve_local_program("tbx-spawn-probe"),
                TestBackend::for_host(
                    &crate::HostDef {
                        name: "devbox".into(),
                        destination: "me@devbox".into(),
                        ..Default::default()
                    },
                    &crate::settings::for_tests(),
                )
                .program_for_window("tbx-spawn-probe", None),
            )
        });

        assert_eq!(local, expected.to_string_lossy());
        assert_eq!(free, expected.to_string_lossy());
        assert_eq!(remote, "tbx-spawn-probe");
    }

    #[test]
    fn a_hosts_server_is_looked_for_where_its_own_thurbox_said_first() {
        let host_data = SocketDir::HostData {
            app: crate::instance::app_dir_name().into(),
        };
        let backend = |name: &str| {
            let host = crate::HostDef {
                name: name.into(),
                destination: "me@devbox".into(),
                ..Default::default()
            };
            let mut backend = TestBackend::for_host(&host, &crate::settings::for_tests());
            backend.transport = backend.transport.clone().with_socket_dir(host_data.clone());
            (host, backend)
        };
        let (_, unasked) = backend("socket-dir-unasked");
        assert_eq!(
            unasked.socket_dir_candidates(),
            [host_data.clone(), SocketDir::Inherited]
        );
        let (older, older_backend) = backend("socket-dir-older-cli");
        crate::instance::learn_host_socket_dir(&older, None);
        assert_eq!(
            older_backend.socket_dir_candidates(),
            [SocketDir::Inherited, host_data.clone()],
            "a CLI that reported no directory spawns into tmux's default"
        );
        let (newer, newer_backend) = backend("socket-dir-newer-cli");
        crate::instance::learn_host_socket_dir(&newer, Some("/srv/me/thurbox/tmux"));
        assert_eq!(
            newer_backend.socket_dir_candidates(),
            [
                SocketDir::Dir("/srv/me/thurbox/tmux".into()),
                host_data,
                SocketDir::Inherited
            ]
        );
    }

    #[test]
    fn what_the_locate_script_says_is_read_word_for_word() {
        assert_eq!(parse_located("answered 1\n"), ServerFound::Answered(1));
        assert_eq!(
            parse_located("recovered 0 4242 1 /private/tmp/tmux-501/a b\n"),
            ServerFound::Recovered {
                at: 0,
                pid: 4242,
                others: 1,
                path: "/private/tmp/tmux-501/a b".into()
            }
        );
        assert_eq!(
            parse_located("unanswered 1 7 0 /tmp/tmux-0/thurbox"),
            ServerFound::Unanswered {
                at: 1,
                pid: 7,
                path: "/tmp/tmux-0/thurbox".into()
            }
        );
        assert_eq!(
            parse_located("bound 7,9 /tmp/tmux-0/thurbox"),
            ServerFound::Bound {
                pids: "7,9".into(),
                path: "/tmp/tmux-0/thurbox".into()
            }
        );
        for nothing in ["", "answered", "answered x", "recovered 0 p 0 /x", "hello"] {
            assert_eq!(parse_located(nothing), ServerFound::Nothing, "{nothing:?}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn the_locate_script_is_valid_sh_and_names_every_candidate_in_order() {
        let script = locate_script(
            &[
                SocketDir::Dir("/home/me/.local/share/thurbox/tmux".into()),
                SocketDir::Inherited,
                SocketDir::HostData {
                    app: "thurbox".into(),
                },
            ],
            "thurbox",
        );
        assert!(script.starts_with(
            "d0=/home/me/.local/share/thurbox/tmux\nd1=${TMUX_TMPDIR:-/tmp}\nd2=$(d="
        ));
        assert!(script.contains("\nn=3\n"));
        let parsed = std::process::Command::new("sh")
            .args(["-n", "-c", &script])
            .output()
            .expect("sh");
        assert!(
            parsed.status.success(),
            "{}",
            String::from_utf8_lossy(&parsed.stderr)
        );
    }

    #[cfg(target_os = "linux")]
    fn locate_beside_a_bound_socket(answer_after: Option<std::time::Duration>) -> ServerFound {
        let root = tempfile::tempdir().expect("tempdir");
        // SAFETY: `getuid` cannot fail and touches no memory.
        let uid = unsafe { libc::getuid() };
        let sockets = root.path().join(format!("tmux-{uid}"));
        std::fs::create_dir_all(&sockets).unwrap();
        let _listener = std::os::unix::net::UnixListener::bind(sockets.join("located")).unwrap();
        let ready = root.path().join("ready");
        let mux = root.path().join("mux");
        std::fs::write(&mux, format!("#!/bin/sh\ntest -e '{}'\n", ready.display())).unwrap();
        std::fs::set_permissions(&mux, std::os::unix::fs::PermissionsExt::from_mode(0o755))
            .unwrap();
        let script = locate_script(
            &[SocketDir::Dir(root.path().display().to_string())],
            "located",
        );
        let script = script.replacen("\"$t\" -lt 30", "\"$t\" -lt 10", 1);
        assert!(script.contains("\"$t\" -lt 10"), "{script}");
        let starting = answer_after.map(|delay| {
            std::thread::spawn(move || {
                std::thread::sleep(delay);
                std::fs::write(ready, "").unwrap();
            })
        });
        let out = std::process::Command::new("sh")
            .args(["-c", &script, "sh"])
            .arg(&mux)
            .arg("located")
            .output()
            .expect("sh");
        if let Some(starting) = starting {
            starting.join().unwrap();
        }
        parse_located(&String::from_utf8_lossy(&out.stdout))
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_server_that_binds_while_the_search_runs_is_found_not_stuck() {
        assert_eq!(
            locate_beside_a_bound_socket(Some(std::time::Duration::from_millis(300))),
            ServerFound::Answered(0)
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_bound_socket_that_never_answers_is_still_refused() {
        assert!(matches!(
            locate_beside_a_bound_socket(None),
            ServerFound::Bound { .. }
        ));
    }

    #[test]
    fn build_shell_command_simple() {
        let cmd = TestBackend::build_shell_command("claude", &[]);
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
        let cmd = TestBackend::build_shell_command("claude", &args);
        assert_eq!(cmd, "claude --resume abc-123 --permission-mode default");
    }

    #[test]
    fn build_shell_command_with_spaces_in_args() {
        let args = vec![
            "--allowed-tools".to_string(),
            "Read Bash(git:*)".to_string(),
        ];
        let cmd = TestBackend::build_shell_command("claude", &args);
        assert_eq!(cmd, "claude --allowed-tools 'Read Bash(git:*)'");
    }

    #[test]
    fn build_shell_command_escapes_command_path() {
        let cmd = TestBackend::build_shell_command("/opt/My Agents/codex", &["--foo".to_string()]);
        assert_eq!(cmd, "'/opt/My Agents/codex' --foo");
    }

    #[test]
    fn backend_default_has_no_control_mode() {
        let backend = TestBackend::local(&crate::settings::for_tests());
        let guard = backend.control.lock().unwrap();
        assert!(guard.is_none());
    }

    #[test]
    fn for_host_builds_named_ssh_backend() {
        let host = crate::HostDef {
            name: "devbox".into(),
            destination: "me@devbox".into(),
            ssh_opts: vec!["-o".into(), "ControlMaster=auto".into()],
            ..Default::default()
        };
        let backend = TestBackend::for_host(&host, &crate::settings::for_tests());
        assert_eq!(backend.name(), "ssh:devbox:tmux");
        assert!(backend.transport.is_remote());
        assert_eq!(backend.socket, TMUX_SOCKET);
        assert_eq!(backend.session, TMUX_SESSION);
    }

    #[test]
    fn a_learned_socket_is_the_hosts_whichever_multiplexer_learned_it() {
        let host = crate::HostDef {
            name: "learned-socket-host".into(),
            destination: "me@learned".into(),
            ..Default::default()
        };
        learn_host_socket(&host, "thurbox-elsewhere");
        assert_eq!(
            TestBackend::for_host(&host, &crate::settings::for_tests()).socket(),
            "thurbox-elsewhere"
        );
        let mut served = host.clone();
        served.multiplexer = Some("psmux".into());
        assert_eq!(host_socket(&served), "thurbox-elsewhere");
        assert_eq!(known_host_socket(&served).unwrap(), "thurbox-elsewhere");
    }

    #[test]
    fn for_host_builds_named_wsl_backend() {
        let host = crate::HostDef::wsl("Ubuntu");
        let backend = TestBackend::for_host(&host, &crate::settings::for_tests());
        assert_eq!(backend.name(), "wsl:Ubuntu:tmux");
        assert!(backend.transport.is_remote());
        assert_eq!(backend.transport.launcher(), "wsl.exe");
        let argv: Vec<String> = backend
            .transport
            .tmux_command("s", &[])
            .expect("command")
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(argv[..2], ["-d", "Ubuntu"]);
        assert_eq!(backend.socket, TMUX_SOCKET);
        assert_eq!(backend.session, TMUX_SESSION);
    }

    #[test]
    fn default_shell_matches_host_os_not_local() {
        let winbox = TestBackend::for_host(
            &crate::HostDef {
                name: "winbox".into(),
                destination: "me@winbox".into(),
                multiplexer: Some("psmux".into()),
                ..Default::default()
            },
            &crate::settings::for_tests(),
        );
        assert_eq!(winbox.default_shell(), "powershell");

        let devbox = TestBackend::for_host(
            &crate::HostDef {
                name: "devbox".into(),
                destination: "me@devbox".into(),
                ..Default::default()
            },
            &crate::settings::for_tests(),
        );
        assert_eq!(devbox.default_shell(), "/bin/sh");

        let wsl = TestBackend::for_host(
            &crate::HostDef::wsl("Ubuntu"),
            &crate::settings::for_tests(),
        );
        assert_eq!(wsl.default_shell(), "/bin/sh");

        let local = TestBackend::local(&crate::settings::for_tests());
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

    fn pinned_default_command(backend: &TestBackend) -> Option<String> {
        backend.session_config().into_iter().find_map(|option| {
            let at = option.args.iter().position(|a| a == "default-command")?;
            option.args.get(at + 1).cloned()
        })
    }

    fn windows_host(mux: &str) -> crate::HostDef {
        crate::HostDef {
            name: "winbox".into(),
            destination: "me@winbox".into(),
            multiplexer: Some(mux.into()),
            platform: Some(crate::Platform::Windows),
            ..Default::default()
        }
    }

    #[test]
    fn a_windows_thurbox_pins_posix_default_command_on_a_wsl_host() {
        use crate::shell::simulate_local;
        use crate::{HostDef, Platform};
        for local in Platform::ALL {
            let pinned = simulate_local(local, || {
                pinned_default_command(&TestBackend::for_host(
                    &HostDef::wsl("Ubuntu"),
                    &crate::settings::for_tests(),
                ))
            });
            assert_eq!(pinned.as_deref(), Some("/bin/sh"), "thurbox on {local:?}");
        }
    }

    #[test]
    fn a_windows_host_on_a_non_psmux_multiplexer_keeps_native_shell_semantics() {
        use crate::shell::simulate_local;
        use crate::Platform;
        for local in Platform::ALL {
            simulate_local(local, || {
                let backend =
                    TestBackend::for_host(&windows_host("tmux"), &crate::settings::for_tests());
                assert_eq!(backend.transport.mux(), "tmux");
                assert_eq!(backend.default_shell(), "powershell", "from {local:?}");
                assert_eq!(pinned_default_command(&backend), None, "from {local:?}");
                assert_eq!(backend.login_wrap_for_remote("agent", None), "agent");
            });
        }
    }

    #[test]
    fn a_windows_hosts_shell_pane_is_not_bootstrapped_through_sh() {
        use crate::HostDef;
        let window = format!("{SHELL_WINDOW_PREFIX}work");
        let shell_pane = |backend: &TestBackend| {
            TestMux::window_command(
                backend,
                &window,
                &backend.default_shell(),
                &[],
                &HashMap::new(),
            )
            .expect("a shell pane's command")
        };
        let windows = TestBackend::for_host(&windows_host("tmux"), &crate::settings::for_tests());
        assert_eq!(shell_pane(&windows), "powershell");
        let posix = TestBackend::for_host(
            &HostDef {
                name: "devbox".into(),
                destination: "me@devbox".into(),
                ..Default::default()
            },
            &crate::settings::for_tests(),
        );
        assert_eq!(shell_pane(&posix), posix.remote_shell_pane_command());
    }

    #[test]
    fn a_backend_keeps_platform_launcher_and_multiplexer_apart() {
        use crate::shell::simulate_local;
        use crate::{HostDef, HostKind, Multiplexer, Platform};
        for local in Platform::ALL {
            for kind in [HostKind::Ssh, HostKind::Wsl] {
                for platform in [Platform::Posix, Platform::Windows] {
                    for preferred in [None, Some("psmux"), Some("rmux")] {
                        let host = HostDef {
                            name: "box".into(),
                            kind,
                            destination: "me@box".into(),
                            platform: Some(platform),
                            multiplexer: preferred.map(str::to_string),
                            ..Default::default()
                        };
                        let backend = simulate_local(local, || {
                            TestBackend::for_host(&host, &crate::settings::for_tests())
                        });
                        let case = format!("{kind:?}/{platform:?}/{preferred:?} from {local:?}");
                        assert_eq!(backend.platform, host.platform(), "{case}");
                        assert_eq!(backend.transport.mux(), "tmux", "{case}");
                        let launcher = match kind {
                            HostKind::Ssh => "ssh",
                            HostKind::Wsl => "wsl.exe",
                        };
                        assert_eq!(backend.transport.launcher(), launcher, "{case}");
                        assert_eq!(
                            backend.name(),
                            host.route(Multiplexer::Tmux).format(),
                            "{case}"
                        );
                        assert!(!backend.capabilities().needs_liveness_poll, "{case}");
                    }
                }
            }
        }
    }

    #[test]
    fn a_route_on_another_multiplexer_keeps_the_hosts_platform() {
        let legacy = crate::HostDef {
            name: "win".into(),
            destination: "me@win".into(),
            multiplexer: Some("psmux".into()),
            ..Default::default()
        };
        let backend = TestBackend::for_host(&legacy, &crate::settings::for_tests());
        assert_eq!(backend.transport.mux(), "tmux");
        assert_eq!(backend.platform, crate::Platform::Windows);
        assert_eq!(backend.default_shell(), "powershell");
    }

    #[test]
    fn remote_shell_pane_opens_users_login_shell() {
        // `exec … 2>/dev/null` persists into the exec'd shell and makes bash/zsh non-interactive.
        const EXPECT: &str =
            "/bin/sh -lc 'command -v \"$SHELL\" >/dev/null 2>&1 && exec \"$SHELL\" -l; exec /bin/sh -l'";
        let ssh = TestBackend::for_host(
            &crate::HostDef {
                name: "devbox".into(),
                destination: "me@devbox".into(),
                ..Default::default()
            },
            &crate::settings::for_tests(),
        );
        assert_eq!(ssh.remote_shell_pane_command(), EXPECT);

        let wsl = TestBackend::for_host(
            &crate::HostDef::wsl("Ubuntu"),
            &crate::settings::for_tests(),
        );
        assert_eq!(wsl.remote_shell_pane_command(), EXPECT);

        assert!(!EXPECT.contains("-l 2>"));
    }

    #[test]
    fn login_wrap_wraps_remote_command_in_login_shell() {
        let backend = TestBackend::for_host(
            &crate::HostDef::wsl("Ubuntu"),
            &crate::settings::for_tests(),
        );
        let wrapped = backend.login_wrap_for_remote("claude --resume x", None);
        assert_eq!(wrapped, "/bin/sh -lc 'exec claude --resume x'");
    }

    #[test]
    fn login_wrap_assigns_the_hosts_login_path() {
        let host = crate::HostDef {
            name: "login-wrap-path".into(),
            destination: "me@devbox".into(),
            ..Default::default()
        };
        crate::host_path::seed(
            &host,
            Some(crate::host_path::HostEnv {
                home: Some("/home/me".into()),
                base: vec!["/usr/bin".into()],
                shell_login: Some(vec!["/home/me/.local/bin".into(), "/usr/bin".into()]),
                sh_login: None,
                multiplexers: Vec::new(),
            }),
        );
        let backend = TestBackend::for_host(&host, &crate::settings::for_tests());
        assert_eq!(
            backend.login_wrap_for_remote("claude", None),
            "/bin/sh -lc 'PATH=/home/me/.local/bin:/usr/bin; export PATH; exec claude'"
        );
    }

    #[test]
    fn login_wrap_assigns_the_tasks_path_behind_the_missing_path_prepend() {
        let host = crate::HostDef {
            name: "login-wrap-task-path".into(),
            destination: "me@devbox".into(),
            path_prepend: vec!["/opt/x/bin".into(), "/usr/bin".into()],
            ..Default::default()
        };
        crate::host_path::seed(
            &host,
            Some(crate::host_path::HostEnv {
                home: Some("/home/me".into()),
                base: vec!["/usr/bin".into()],
                shell_login: Some(vec!["/home/me/.local/bin".into()]),
                sh_login: None,
                multiplexers: Vec::new(),
            }),
        );
        let backend = TestBackend::for_host(&host, &crate::settings::for_tests());
        assert_eq!(
            backend.login_wrap_for_remote("claude", Some("/opt/wrap/bin:/usr/bin")),
            "/bin/sh -lc 'PATH=/opt/x/bin:/opt/wrap/bin:/usr/bin; export PATH; exec claude'"
        );
    }

    #[test]
    fn a_hosts_multiplexer_commands_carry_its_login_path() {
        let host = crate::HostDef {
            name: "mux-command-path".into(),
            destination: "me@mac".into(),
            path_prepend: vec!["/opt/x/bin".into()],
            ..Default::default()
        };
        crate::host_path::seed(
            &host,
            Some(crate::host_path::HostEnv {
                home: Some("/Users/me".into()),
                base: vec!["/usr/bin".into()],
                shell_login: Some(vec!["/opt/homebrew/bin".into(), "/usr/bin".into()]),
                sh_login: None,
                multiplexers: vec![crate::Multiplexer::Tmux],
            }),
        );
        let backend = TestBackend::for_host(&host, &crate::settings::for_tests());
        let argv: Vec<String> = backend
            .mux_command(&["-V"])
            .expect("command")
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        let line = argv.join(" ");
        let assignment = "PATH=/opt/x/bin:/opt/homebrew/bin:/usr/bin; export PATH; ";
        assert!(
            line.contains(&format!("{assignment}exec")),
            "the multiplexer command does not set the host's PATH: {line}"
        );
        assert!(
            line.ends_with(&format!("sh tmux -L {} -V", backend.socket())),
            "the multiplexer runs through the PATH-setting sh: {line}"
        );
    }

    #[test]
    fn login_wrap_is_noop_for_local() {
        let backend = TestBackend::local(&crate::settings::for_tests());
        assert_eq!(backend.login_wrap_for_remote("claude", None), "claude");
    }

    #[test]
    fn for_host_honors_socket_and_session_overrides() {
        let host = crate::HostDef {
            name: "vm".into(),
            destination: "vm".into(),
            socket: Some("tb-vm".into()),
            session: Some("sess-vm".into()),
            ..Default::default()
        };
        let backend = TestBackend::for_host(&host, &crate::settings::for_tests());
        assert_eq!(backend.socket, "tb-vm");
        assert_eq!(backend.session, "sess-vm");
    }

    #[test]
    fn nothing_is_done_on_a_host_whose_socket_is_a_guess() {
        let host = crate::HostDef {
            name: "devbox".into(),
            destination: "me@devbox".into(),
            ..Default::default()
        };
        let backend = TestBackend::for_host(&host, &crate::settings::for_tests());
        let owner =
            Owner::new("00000000-0000-4000-8000-000000000001", "remote").remembering("%3", "");
        let refusals = [
            backend.discover().map(drop),
            backend.locate(owner).map(drop),
            backend.kill("%3"),
            backend.rename_windows(owner, "moved"),
        ];
        for refusal in refusals {
            let refusal = format!("{:#}", refusal.unwrap_err());
            assert!(
                refusal.contains("socket unknown for host 'devbox'"),
                "{refusal}"
            );
        }
        assert!(!backend.attached(), "a refusal opened a connection");
    }

    use crate::identity::tests::listed;
    use crate::identity::{program_window_name, shell_window_name};
    use crate::tmux_compat::control_mode::{
        decode_octal, format_send_keys, parse_notification, shell_escape, Notification,
    };

    #[test]
    fn only_the_multiplexers_own_refusal_counts_as_an_empty_answer() {
        let answers = [
            "error connecting to /tmp/tmux-0/thurbox (No such file or directory)",
            "no server running on /tmp/tmux-0/thurbox",
            "can't find session: thurbox",
        ];
        for answer in answers {
            assert!(
                TestMux::answered_absent(answer) == Some(Missing::Server),
                "the multiplexer answered: {answer}"
            );
        }
        let unanswered = [
            "ssh: connect to host devbox port 22: Connection refused",
            "ssh: connect to host devbox port 22: Operation timed out",
            "Permission denied (publickey).",
            "bash: line 1: tmux: command not found",
            "Failed to run tmux command",
        ];
        for failure in unanswered {
            assert!(
                TestMux::answered_absent(failure).is_none(),
                "nothing answered: {failure}"
            );
        }
    }

    #[test]
    fn a_socket_that_cannot_be_opened_is_not_a_server_that_is_not_there() {
        assert!(
            TestMux::answered_absent(
                "error connecting to /tmp/tmux-0/thurbox (No such file or directory)"
            ) == Some(Missing::Server),
            "no socket at all is the one reason that means absence"
        );
        for live in [
            "error connecting to /tmp/tmux-1000/thurbox (Permission denied)",
            "error connecting to /tmp/tmux-1000/thurbox (Connection refused)",
            "error connecting to /tmp/tmux-1000/thurbox (Connection reset by peer)",
        ] {
            assert!(
                TestMux::answered_absent(live).is_none(),
                "a server may be alive behind this socket: {live}"
            );
        }
    }

    #[test]
    fn ssh_failing_on_its_own_account_is_never_absence() {
        let tmux_said_absent = "no server running on /tmp/tmux-0/thurbox";

        assert!(
            mux_answered::<TestMux>(true, Some(1), tmux_said_absent).is_some(),
            "ssh passed through tmux's own answer"
        );
        assert!(
            mux_answered::<TestMux>(true, Some(255), tmux_said_absent).is_none(),
            "255 is ssh's own failure; nothing on the host answered"
        );
        assert!(
            mux_answered::<TestMux>(false, Some(255), tmux_said_absent).is_some(),
            "without ssh in the path, 255 carries none of that meaning"
        );
        assert!(
            mux_answered::<TestMux>(true, Some(127), "bash: tmux: command not found").is_none(),
            "reached the host, but nothing there answered the question"
        );
        assert!(
            mux_answered::<TestMux>(true, None, tmux_said_absent).is_none(),
            "killed by a signal: no status to reason from"
        );
    }

    #[test]
    fn control_mode_reexports_resolve() {
        assert_eq!(shell_escape("hello world"), "'hello world'");
        assert_eq!(decode_octal(b"\\033"), vec![27]);
        assert_eq!(format_send_keys("%1", b"A"), "send-keys -t %1 -H 41\n");
        assert_eq!(
            parse_notification("%pause %1"),
            Notification::Pause {
                pane_id: "%1".to_string()
            }
        );
    }

    #[cfg(not(windows))]
    #[test]
    fn the_cli_directory_leads_the_path_it_was_missing_from() {
        let inherited = std::ffi::OsString::from("/usr/bin:/bin");
        let led = path_led_by(Path::new("/opt/thurbox/bin"), &inherited).expect("joinable");
        assert_eq!(
            led,
            std::ffi::OsString::from("/opt/thurbox/bin:/usr/bin:/bin")
        );
    }

    #[cfg(not(windows))]
    #[test]
    fn a_directory_already_on_the_path_is_moved_not_duplicated() {
        let inherited = std::ffi::OsString::from("/usr/bin:/opt/thurbox/bin:/bin");
        let led = path_led_by(Path::new("/opt/thurbox/bin"), &inherited).expect("joinable");
        assert_eq!(
            led,
            std::ffi::OsString::from("/opt/thurbox/bin:/usr/bin:/bin")
        );
    }

    #[cfg(not(windows))]
    #[test]
    fn an_empty_path_becomes_the_cli_directory_alone() {
        let led =
            path_led_by(Path::new("/opt/thurbox/bin"), std::ffi::OsStr::new("")).expect("joinable");
        assert_eq!(led, std::ffi::OsString::from("/opt/thurbox/bin"));
    }

    #[cfg(not(windows))]
    #[test]
    fn a_local_control_mode_window_carries_the_tasks_path() {
        let backend = TestBackend::local(&crate::settings::for_tests());
        let command =
            backend.posix_window_command("tb-x", "/bin/agent", &[], Some("/opt/wrap/bin:/usr/bin"));
        let path = path_from_prefix(&command).expect("an env PATH prefix");
        assert!(path.ends_with("/opt/wrap/bin:/usr/bin"), "{command}");
        assert!(command.ends_with(" /bin/agent"), "{command}");
    }

    #[cfg(not(windows))]
    #[test]
    fn a_task_path_keeps_its_order_behind_the_missing_cli_directory() {
        let task = std::ffi::OsStr::new("/opt/wrap/bin:/usr/bin:/bin");
        let path = path_reaching(Some(Path::new("/opt/thurbox/bin")), task).expect("joinable");
        assert_eq!(
            path,
            std::ffi::OsString::from("/opt/thurbox/bin:/opt/wrap/bin:/usr/bin:/bin")
        );
    }

    #[cfg(not(windows))]
    #[test]
    fn a_task_path_that_names_the_cli_directory_is_kept_as_it_is() {
        let task = std::ffi::OsStr::new("/opt/wrap/bin:/opt/thurbox/bin:/usr/bin");
        let path = path_reaching(Some(Path::new("/opt/thurbox/bin")), task).expect("joinable");
        assert_eq!(path, task);
    }

    #[cfg(not(windows))]
    #[test]
    fn a_task_path_is_kept_without_a_cli_directory() {
        let task = std::ffi::OsStr::new("/opt/wrap/bin::/usr/bin");
        let path = path_reaching(None, task).expect("joinable");
        assert_eq!(path, std::ffi::OsString::from("/opt/wrap/bin:/usr/bin"));
    }

    #[test]
    fn a_window_command_opening_with_env_yields_its_path() {
        assert_eq!(
            path_from_prefix("/usr/bin/env PATH=/opt/tbx:/usr/bin /usr/bin/sh -c \"sleep 30\""),
            Some("/opt/tbx:/usr/bin".to_string())
        );
    }

    #[test]
    fn a_path_assignment_elsewhere_in_the_command_is_not_the_panes() {
        assert_eq!(path_from_prefix("/usr/bin/claude --env PATH=/nope"), None);
        assert_eq!(path_from_prefix("/usr/bin/claude"), None);
        assert_eq!(path_from_prefix(""), None);
    }

    #[test]
    fn a_quoted_single_token_command_still_yields_its_path() {
        assert_eq!(
            path_from_prefix("\"/usr/bin/env PATH=/opt/tbx:/usr/bin sh -c 'sleep 300'\""),
            Some("/opt/tbx:/usr/bin".to_string())
        );
    }

    #[test]
    fn a_quoted_prefix_reads_as_unknown() {
        assert_eq!(
            path_from_prefix("/usr/bin/env \"PATH=/opt/my tools:/usr/bin\" /usr/bin/sh"),
            None
        );
    }

    #[cfg(unix)]
    fn agent_only_thurbox_can_see(dir: &std::path::Path, name: &str) -> std::path::PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let p = dir.join(name);
        std::fs::write(&p, b"#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o700)).unwrap();
        p
    }

    #[cfg(unix)]
    #[test]
    fn a_task_path_resolves_the_program_before_this_process_path() {
        let own = tempfile::tempdir().expect("tempdir");
        let task = tempfile::tempdir().expect("tempdir");
        agent_only_thurbox_can_see(own.path(), "tbx-task-path-probe");
        let wanted = agent_only_thurbox_can_see(task.path(), "tbx-task-path-probe");
        let task_path = format!("{}:/usr/bin", task.path().display());
        let resolved = crate::path_lookup::with_path(own.path(), || {
            resolve_local_program_on("tbx-task-path-probe", Some(&task_path))
        });
        assert_eq!(resolved, wanted.to_string_lossy());
    }

    #[cfg(unix)]
    #[test]
    fn a_program_missing_from_the_task_path_resolves_on_this_process_path() {
        let own = tempfile::tempdir().expect("tempdir");
        let wanted = agent_only_thurbox_can_see(own.path(), "tbx-task-path-probe");
        let resolved = crate::path_lookup::with_path(own.path(), || {
            resolve_local_program_on("tbx-task-path-probe", Some("/nonexistent/bin"))
        });
        assert_eq!(resolved, wanted.to_string_lossy());
    }

    #[test]
    fn an_unresolvable_local_command_is_passed_through() {
        assert_eq!(
            resolve_local_program("tbx-agent-that-is-not-installed"),
            "tbx-agent-that-is-not-installed"
        );
        assert_eq!(
            resolve_local_program("/opt/My Agents/codex"),
            "/opt/My Agents/codex"
        );
    }

    #[test]
    fn tmux_names_every_key() {
        for name in Key::NAMED {
            let key = Key::parse(name).expect("a listed key");
            assert!(!tmux_key_name(&key).is_empty(), "{name}");
        }
        assert_eq!(tmux_key_name(&Key::parse("pgup").unwrap()), "PageUp");
        assert_eq!(tmux_key_name(&Key::parse("delete").unwrap()), "DC");
        for letter in 'a'..='z' {
            let key = Key::parse(&format!("ctrl-{letter}")).unwrap();
            assert_eq!(tmux_key_name(&key), format!("C-{letter}"));
        }
    }

    #[test]
    fn an_answer_counts_only_from_the_pane_asked_about() {
        assert!(answered_for("%3", Some("tb-x"), Some("%3")));
        // tmux answers an unresolved target for the client's current pane.
        assert!(!answered_for("%3", None, None));
        assert!(!answered_for("%3", Some("tb-y"), Some("%0")));
        assert!(answered_for("thurbox:=tb-x", Some("tb-x"), Some("%9")));
        assert!(!answered_for("thurbox:=tb-x", Some("tb-y"), Some("%9")));
    }

    #[test]
    fn an_id_target_is_qualified_only_where_ids_are_per_session() {
        assert_eq!(qualify_target("thurbox", "%2", true), "thurbox:.%2");
        assert_eq!(qualify_target("thurbox", "@7", true), "thurbox:@7");
        assert_eq!(
            qualify_target("thurbox", "thurbox:=tb-x", true),
            "thurbox:=tb-x"
        );
        assert_eq!(qualify_target("thurbox", "thurbox", true), "thurbox");
        for id in ["%2", "@7", "thurbox:=tb-x"] {
            assert_eq!(qualify_target("thurbox", id, false), id);
        }
    }

    #[test]
    fn a_kill_of_a_server_wide_id_is_unchanged() {
        assert_eq!(
            TestBackend::local(&crate::settings::for_tests()).kill_spelling("%8"),
            ("kill-window", Cow::Borrowed("%8"))
        );
    }

    #[test]
    fn a_pane_location_is_only_taken_from_the_panes_own_answer() {
        assert_eq!(
            parse_pane_location("%7", "%7 @3 24 80 me\n"),
            Some(PaneLocation {
                window: "@3".into(),
                size: Some((24, 80)),
                sized_by: Some("me".into()),
            })
        );
        assert_eq!(
            parse_pane_location("%7", "%7 @3"),
            Some(PaneLocation {
                window: "@3".into(),
                size: None,
                sized_by: None,
            })
        );
        assert_eq!(parse_pane_location("%9", "%1 @1\n"), None);
        assert_eq!(parse_pane_location("%7", "%7 nonsense"), None);
        assert_eq!(parse_pane_location("%7", ""), None);
    }

    #[test]
    fn a_shareable_host_that_has_not_said_which_socket_it_uses_is_refused() {
        let host = crate::HostDef {
            name: "devbox".into(),
            destination: "me@devbox".into(),
            ..Default::default()
        };
        let refusal = format!("{:#}", known_host_socket(&host).unwrap_err());
        assert!(
            refusal.contains("socket unknown for host 'devbox'"),
            "{refusal}"
        );

        let solo = crate::HostDef {
            share_sessions: false,
            ..host.clone()
        };
        assert_eq!(known_host_socket(&solo).unwrap(), TMUX_SOCKET);

        let pinned = crate::HostDef {
            socket: Some("thurbox".into()),
            ..host
        };
        assert_eq!(known_host_socket(&pinned).unwrap(), "thurbox");
    }

    const _: () = assert!(PANE_CHANNEL_CAPACITY >= 1024);

    #[test]
    fn env_flag_simple_value() {
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
        let env_part: String = [("MSG".to_string(), "hello world".to_string())]
            .into_iter()
            .collect::<std::collections::HashMap<_, _>>()
            .iter()
            .map(|(k, v)| format!(" -e {}", shell_escape(&format!("{k}={v}"))))
            .collect();
        assert_eq!(env_part, " -e 'MSG=hello world'");
    }

    #[test]
    fn an_agent_window_keeps_its_corpse_and_the_other_two_do_not() {
        assert!(keeps_dead_pane(&agent_window_name("Foo Bar")));
        assert!(!keeps_dead_pane(&shell_window_name("Foo Bar")));
        assert!(!keeps_dead_pane(&program_window_name("abcd1234", "watch")));
        assert!(!keeps_dead_pane("zsh"));
    }

    #[test]
    fn the_session_option_list_holds_no_window_options() {
        for (key, _) in SESSION_OPTS {
            assert!(
                !["remain-on-exit", "window-size"].contains(key),
                "{key} is a window option and is silently applied to whichever \
                 window happens to be current"
            );
        }
    }

    #[test]
    fn the_server_wide_window_options_do_not_size_windows_by_hand() {
        for (key, value) in WINDOW_OPTS {
            assert!(
                *key != "window-size",
                "a server-wide `window-size {value}` kills the server on the \
                 next window creation; say it per window (`birth_options`)"
            );
        }
        assert!(
            birth_options("tb-anything")
                .iter()
                .any(|(key, value)| *key == "window-size" && *value == "manual"),
            "the window that is created still has to be told"
        );
    }

    const ONE: &str = "11111111-1111-4111-8111-111111111111";
    const TWO: &str = "22222222-2222-4222-8222-222222222222";

    #[test]
    fn an_unanswered_status_listing_is_an_error() {
        let backend = TestBackend::with_transport(
            TmuxTransport::local("thurbox-test-no-such-multiplexer"),
            "thurbox-test",
            "thurbox-test",
            "local:tmux",
            crate::settings::DEFAULT_SCROLLBACK_LINES,
        );
        assert!(backend.hook_states().is_err());
    }

    #[cfg(unix)]
    fn answering_mux(dir: &std::path::Path, windows: &str, stderr: &str) -> TestBackend {
        use std::os::unix::fs::PermissionsExt;
        let mux = dir.join("mux");
        let script = format!(
            "#!/bin/sh\ncase \"$*\" in\n  *list-windows*) [ -n '{windows}' ] && {{ echo '{windows}'; exit 0; }} ;;\nesac\ncat >&2 <<'EOF'\n{stderr}\nEOF\nexit 1\n"
        );
        std::fs::write(&mux, script).unwrap();
        std::fs::set_permissions(&mux, std::fs::Permissions::from_mode(0o700)).unwrap();
        TestBackend::with_transport(
            TmuxTransport::local(mux.to_string_lossy().into_owned()),
            "thurbox-test",
            "thurbox-test",
            "local:tmux",
            crate::settings::DEFAULT_SCROLLBACK_LINES,
        )
    }

    #[cfg(unix)]
    #[test]
    fn a_socket_that_cannot_be_opened_is_no_answer() {
        let dir = tempfile::tempdir().unwrap();
        for refused in [
            "error connecting to /tmp/tmux-1/thurbox-test (Permission denied)",
            "error connecting to /tmp/tmux-1/thurbox-test (Connection refused)",
        ] {
            let backend = answering_mux(dir.path(), "", refused);
            assert!(backend.hook_states().is_err(), "hook_states: {refused}");
        }
        for absent in [
            "error connecting to /tmp/tmux-1/thurbox-test (No such file or directory)",
            "can't find session: thurbox-test",
        ] {
            let backend = answering_mux(dir.path(), "", absent);
            assert_eq!(backend.hook_states().unwrap(), Vec::new(), "{absent}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn only_the_server_saying_no_session_is_an_empty_status_answer() {
        use std::os::unix::process::ExitStatusExt;
        let backend = TestBackend::local(&crate::settings::for_tests());
        let has_session = |stderr: &str| {
            let output = std::process::Output {
                status: std::process::ExitStatus::from_raw(1 << 8),
                stdout: Vec::new(),
                stderr: stderr.as_bytes().to_vec(),
            };
            matches!(
                backend.command_failure(&["has-session", "-t", "thurbox"], &output),
                crate::BackendError::Absent(_)
            )
        };
        for absent in [
            "can't find session: thurbox",
            "no server running on /tmp/tmux-1/thurbox",
            "error connecting to /tmp/tmux-1/thurbox (No such file or directory)",
        ] {
            assert!(has_session(absent), "{absent}");
        }
        for unanswered in [
            "bash: line 1: tmux: command not found",
            "error connecting to /tmp/tmux-1/thurbox (Permission denied)",
        ] {
            assert!(!has_session(unanswered), "{unanswered}");
        }
    }

    #[test]
    fn the_discover_format_reads_both_stamps() {
        assert!(DISCOVER_FORMAT.contains(&format!("#{{{WINDOW_SESSION_OPTION}}}")));
        assert!(DISCOVER_FORMAT.contains(&format!("#{{{WINDOW_ROLE_OPTION}}}")));
    }

    #[test]
    fn the_retire_format_reads_the_window_and_both_stamps() {
        assert_eq!(
            RETIRE_FORMAT,
            format!("#{{window_id}}|#{{{WINDOW_SESSION_OPTION}}}|#{{{WINDOW_ROLE_OPTION}}}")
        );
    }

    #[test]
    fn a_listing_orders_one_sessions_windows_by_the_id_tmux_issued() {
        let listing =
            format!("@10|{ONE}|agent\n@2|{ONE}|agent\n@3|{TWO}|agent\n@4|{ONE}|shell\n@5||\n");
        assert_eq!(
            stamped_windows_in(&listing, ONE, WindowRole::Agent),
            vec!["@2".to_string(), "@10".to_string()],
            "ordered by the number, not by the text, and nobody else's window is in it"
        );
        assert_eq!(
            stamped_windows_in(&listing, ONE, WindowRole::Shell),
            vec!["@4".to_string()],
            "a session owns one window per role, so the roles are counted apart"
        );
    }

    #[test]
    fn a_window_id_that_does_not_parse_is_left_out_of_the_order() {
        let listing = format!("#{{window_id}}|{ONE}|agent\n@7|{ONE}|agent\n");
        assert_eq!(
            stamped_windows_in(&listing, ONE, WindowRole::Agent),
            vec!["@7".to_string()]
        );
    }

    #[test]
    fn the_spawn_format_asks_for_the_window_too() {
        assert!(SPAWN_FORMAT.contains("#{pane_id}"));
        assert!(SPAWN_FORMAT.contains("#{window_id}"));
        assert!(SPAWN_FORMAT.split_whitespace().count() == 2);
    }

    #[test]
    fn a_window_id_is_an_at_sign_and_digits() {
        assert!(control_mode::is_valid_window_id("@0"));
        assert!(control_mode::is_valid_window_id("@42"));
        assert!(!control_mode::is_valid_window_id("@"));
        assert!(!control_mode::is_valid_window_id("%3"));
        assert!(!control_mode::is_valid_window_id("@3x"));
        assert!(!control_mode::is_valid_window_id(""));
    }

    #[test]
    fn a_shell_window_is_owned_by_its_stamp_the_way_an_agent_window_is() {
        let theirs = WindowIndex::from_listing([listed("%5", "tbs-fleet", TWO, WindowRole::Shell)]);
        assert_eq!(theirs.shell_window(ONE, "fleet"), Located::Absent);

        let legacy = WindowIndex::from_listing([listed("%5", "tbs-fleet", "", WindowRole::Shell)]);
        assert_eq!(legacy.shell_window(ONE, "fleet"), Located::At("%5".into()));

        let ambiguous = WindowIndex::from_listing([
            listed("%5", "tbs-fleet", "", WindowRole::Shell),
            listed("%6", "tbs-fleet", "", WindowRole::Shell),
        ]);
        assert_eq!(ambiguous.shell_window(ONE, "fleet"), Located::Unknown);
    }

    #[test]
    fn only_a_session_id_counts_as_a_stamp() {
        let parsed =
            parse_discovered("%1|tb-fleet|0|#{@thurbox_session}|agent", true).expect("parsed");
        assert_eq!(parsed.session, "");
        assert_eq!(parsed.role, WindowRole::Agent);
        assert_eq!(
            parse_discovered(&format!("%1|tb-fleet|0|{ONE}|agent"), true)
                .unwrap()
                .session,
            ONE
        );
    }

    #[test]
    fn a_listing_without_the_stamp_fields_still_discovers_the_window() {
        let parsed = parse_discovered("%1|tbs-fleet|0", true).expect("parsed");
        assert_eq!(parsed.session, "");
        assert_eq!(parsed.role, WindowRole::Shell);
        assert!(parsed.is_alive);
        assert!(parse_discovered("%1|someone-elses|0", true).is_none());
        assert!(parse_discovered("not-a-pane|tb-fleet|0", true).is_none());
    }

    #[test]
    fn a_global_stamp_is_not_read_as_every_windows_identity() {
        let listing = |stamps: bool| {
            WindowIndex::from_listing([
                parse_discovered(&format!("%1|tb-first|0|{TWO}|agent"), stamps).expect("parsed"),
                parse_discovered(&format!("%3|tb-second|0|{TWO}|agent"), stamps).expect("parsed"),
            ])
        };

        let stamped = listing(true);
        assert_eq!(stamped.agent_window(TWO, "second"), Located::Unknown);
        assert_eq!(stamped.agent_window(ONE, "first"), Located::Absent);

        let unstamped = listing(false);
        assert_eq!(
            unstamped.agent_window(TWO, "second"),
            Located::At("%3".into())
        );
        assert_eq!(
            unstamped.agent_window(ONE, "first"),
            Located::At("%1".into())
        );
    }

    #[test]
    fn a_global_role_never_makes_a_shell_window_an_agent() {
        let shell =
            parse_discovered(&format!("%5|tbs-fleet|0|{ONE}|agent"), false).expect("parsed");
        assert_eq!(shell.role, WindowRole::Shell);
        assert_eq!(shell.session, "");
    }

    #[test]
    fn parse_pane_dead_only_accepts_one() {
        assert!(parse_pane_dead("1"));
        assert!(parse_pane_dead("1\n"));
        assert!(!parse_pane_dead("0\n"));

        // tmux's `display-message` for a missing window exits 0 printing nothing.
        assert!(!parse_pane_dead(""));
        assert!(!parse_pane_dead("\n"));

        assert!(!parse_pane_dead("10"));
        assert!(!parse_pane_dead("dead"));
    }

    #[test]
    fn exit_zero_mux_errors_are_still_errors() {
        let mut output = if cfg!(windows) {
            std::process::Command::new("cmd")
                .args(["/C", "exit", "0"])
                .output()
                .expect("run a successful command")
        } else {
            std::process::Command::new("true")
                .output()
                .expect("run a successful command")
        };
        assert!(output.status.success());

        output.stderr = b"psmux diagnostic".to_vec();
        assert!(mux_reported_error(&output, false, false));
        assert!(mux_reported_error(&output, true, false));
        output.stderr.clear();
        output.stdout = b"ERROR: can't find window".to_vec();
        assert!(mux_reported_error(&output, false, true));
        assert!(!mux_reported_error(&output, false, false));
        output.stdout = b"ordinary command answer".to_vec();
        assert!(!mux_reported_error(&output, false, true));
        output.stderr = b"** informational SSH advisory\n".to_vec();
        assert!(!mux_reported_error(&output, true, true));
        output.stdout = b"ERROR: remote psmux failure".to_vec();
        assert!(mux_reported_error(&output, true, true));
        assert_eq!(mux_failure(&output, true), "ERROR: remote psmux failure");
    }

    #[test]
    fn mouse_seed_replays_the_mode_codex_asks_for() {
        // What tmux reports for a running Codex: `?1003` with SGR encoding.
        assert_eq!(mouse_seed_bytes("0,0,1,1,1,0"), b"\x1b[?1003h\x1b[?1006h");
    }

    #[test]
    fn mouse_seed_is_empty_for_a_pane_with_no_tracking() {
        assert!(mouse_seed_bytes("0,0,0,0,0,0").is_empty());
        assert!(mouse_seed_bytes("0,0,0,0,1,0").is_empty());
        assert!(mouse_seed_bytes("").is_empty());
    }

    #[test]
    fn mouse_seed_falls_back_to_press_reporting_on_an_unknown_mode_flag() {
        assert_eq!(mouse_seed_bytes(",,,1,1,"), b"\x1b[?1000h\x1b[?1006h");
    }

    #[test]
    fn mouse_seed_lands_a_parser_in_the_reported_mode() {
        let mut parser = vt100::Parser::new(2.try_into().unwrap(), 2.try_into().unwrap(), 0);
        parser.process(&mouse_seed_bytes("0,1,0,1,0,1"));
        let screen = parser.screen();
        assert_eq!(
            screen.mouse_protocol_mode(),
            vt100::MouseProtocolMode::ButtonMotion
        );
        assert_eq!(
            screen.mouse_protocol_encoding(),
            vt100::MouseProtocolEncoding::Utf8
        );
    }

    #[test]
    fn title_seed_replays_an_agent_title_as_osc_2() {
        assert_eq!(
            title_seed_bytes("devbox", "\u{2733} Terminal name lost on restart"),
            "\x1b]2;\u{2733} Terminal name lost on restart\x1b\\".as_bytes()
        );
    }

    #[test]
    fn title_seed_suppresses_tmuxs_default_title() {
        assert!(title_seed_bytes("devbox", "devbox").is_empty());
        assert!(title_seed_bytes("devbox", "  devbox  ").is_empty());
        assert!(title_seed_bytes("devbox", "   ").is_empty());
    }

    #[test]
    fn title_seed_drops_control_characters() {
        let seed = title_seed_bytes("h", "done\x1b\\ + rm -rf\x07\nnext");
        assert_eq!(seed, "\x1b]2;done\\ + rm -rfnext\x1b\\".as_bytes());
    }

    #[test]
    fn title_seed_bounds_a_huge_title() {
        let seed = title_seed_bytes("h", &"\u{00e9}".repeat(4_000));
        assert!(
            seed.len() <= control_mode::MAX_TITLE_SEED_BYTES + 6,
            "{}",
            seed.len()
        );
        assert!(std::str::from_utf8(&seed).is_ok());
    }

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
        assert_eq!(state.foreground_command, None);
        assert_eq!(state.title, None);
    }

    #[test]
    fn parse_pane_state_reads_the_title_last() {
        let (state, _, _) = parse_pane_state(&pane_state_answer(&[
            "0",
            "0",
            "codex",
            "/w",
            "/dev/pts/1",
            "0",
            "tb-x",
            "%1",
            "",
            "",
            "3.8",
            "⠋ Action Required: allow command?",
        ]));
        assert_eq!(
            state.title.as_deref(),
            Some("⠋ Action Required: allow command?")
        );
    }

    #[test]
    fn parse_pane_state_reads_the_answer_an_older_tmux_prints() {
        // tmux 3.4's `vis(3)` pass rewrites the separator to its octal escape.
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
        // psmux expands an unknown format to nothing.
        let (state, tty, _) = parse_pane_state(&pane_state_answer(&["", "", "", "", ""]));
        assert_eq!(state, PaneState::default());
        assert_eq!(tty, None);

        let (state, tty, _) = parse_pane_state(&pane_state_answer(&["3", "4"]));
        assert_eq!(state.cursor_row, Some(3));
        assert_eq!(state.cursor_col, Some(4));
        assert_eq!(state.foreground_cwd, None);
        assert_eq!(tty, None);
    }

    #[test]
    fn parse_pane_state_reports_which_window_answered() {
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
    fn parse_pane_state_reads_a_dead_panes_exit_status() {
        let answer = |status: &str| {
            parse_pane_state(&pane_state_answer(&[
                "0",
                "0",
                "env",
                "/w",
                "/dev/pts/2",
                "1",
                "tb-demo",
                "%3",
                status,
            ]))
            .0
            .exit_status
        };
        assert_eq!(answer("1"), Some(1));
        assert_eq!(answer("127"), Some(127));
        assert_eq!(answer(""), None);
    }

    #[test]
    fn parse_pane_state_reads_whether_a_dead_pane_was_reaped() {
        let reaped = |dead: &str, status: &str, signal: &str, version: &str| {
            parse_pane_state(&pane_state_answer(&[
                "0",
                "0",
                "env",
                "/w",
                "/dev/pts/2",
                dead,
                "tb-demo",
                "%3",
                status,
                signal,
                version,
            ]))
            .0
            .reaped
        };
        assert_eq!(reaped("1", "3", "", "3.4"), Some(true));
        assert_eq!(reaped("1", "", "KILL", "3.4"), Some(true));
        assert_eq!(reaped("1", "3", "", "3.2a"), Some(true));
        // Dead on tmux 3.3–3.6 before its program is reaped.
        assert_eq!(reaped("1", "", "", "3.4"), Some(false));
        // tmux 3.2 cannot say whether a signal killed it.
        assert_eq!(reaped("1", "", "", "3.2a"), None);
        assert_eq!(reaped("1", "", "", ""), None);
        assert_eq!(reaped("0", "", "", "3.4"), None);
    }

    #[test]
    fn parse_pane_state_survives_a_dead_or_missing_pane() {
        let (state, tty, _) = parse_pane_state("");
        assert_eq!(state, PaneState::default());
        assert_eq!(tty, None);
    }

    #[test]
    fn ps_foreground_prefers_the_group_leader_over_its_pipeline() {
        // `tpgid` is the tty's foreground group; its leader is the row whose `pgid` matches.
        let out = "\
 4210  4210  4300 -bash
 4300  4300  4300 node /opt/cursor-agent/cli.js --resume
 4301  4300  4300 tee /tmp/log
";
        let (argv0, command) = parse_ps_foreground(out).expect("a foreground job");
        assert_eq!(argv0, "node");
        assert_eq!(command, "node /opt/cursor-agent/cli.js --resume");
    }

    #[test]
    fn ps_foreground_falls_back_to_a_group_member() {
        let out = " 4301  4300  4300 tee /tmp/log\n";
        assert_eq!(
            parse_ps_foreground(out).map(|(argv0, _)| argv0),
            Some("tee".to_string())
        );
    }

    #[test]
    fn ps_foreground_reports_nothing_when_nothing_holds_the_tty() {
        // `ps` reports -1 for no foreground group and 0 for unknown.
        assert_eq!(parse_ps_foreground(" 4210 4210 -1 -bash\n"), None);
        assert_eq!(parse_ps_foreground(" 4210 4210 0 -bash\n"), None);
        assert_eq!(parse_ps_foreground(""), None);
        assert_eq!(parse_ps_foreground(" 4210 4210 4300 -bash\n"), None);
    }

    #[test]
    fn ps_rows_survive_right_aligned_padding_and_junk() {
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
        let mut parser = vt100::Parser::new(5.try_into().unwrap(), 80.try_into().unwrap(), 100);
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
