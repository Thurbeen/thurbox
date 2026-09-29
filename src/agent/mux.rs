use std::borrow::Cow;
use std::collections::HashMap;
use std::io::{Cursor, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::sync_channel;
use std::sync::{Arc, Mutex};

use anyhow::{bail, Context, Result};
use tracing::{debug, warn};

use crate::agent::backend::{
    AdoptedSession, DiscoveredSession, Located, PanePath, PaneSize, PaneState, SpawnedSession,
    WindowRole,
};
use crate::agent::control_mode::{
    self, is_broken_pipe, is_recv_timeout, shell_escape, ControlMode, ControlModeReader,
    ControlModeWriter, PANE_CHANNEL_CAPACITY, SIZED_BY, SIZER_OPTION,
};
use crate::agent::registry::{LocalMuxTransport, DEFAULT_MUX};
use crate::agent::transport::MuxTransport;

/// A multiplexer's own answers to the questions the shared core asks.
///
/// tmux and psmux speak one command language that diverges in places that
/// matter (ADR-13): which options exist, how an argument is quoted, how a key
/// or a paste reaches a pane, what a server must be to host an agent. The core
/// asks here instead of checking which multiplexer it drives, and each backend
/// module answers for its own transport — `tmux.rs` for `TmuxTransport`,
/// `psmux.rs` for `PsmuxTransport` — so a divergence, or an optimization one
/// of them can afford, lives beside that backend and nowhere else.
pub(crate) trait MuxDialect: MuxTransport {
    /// `#{@...}` is the window's own option, so a stamp written on one window
    /// identifies that window (ADR-25). Where it is not, nothing is stamped
    /// and a window is known by its name alone.
    const WINDOW_OPTIONS: bool;
    /// A `;`-separated command list runs as one invocation, so the session
    /// config can be applied — and checked — in one process.
    const COMMAND_LISTS: bool;
    /// `display-message` answers a pane's size and sizer, and a window's
    /// resize is reported (`%layout-change`), so the grid can follow the pane.
    const SIZE_REPORTS: bool;
    /// The control-mode argv attach is answered with a `%begin`/`%end` block.
    const IMPLICIT_ATTACH_RESPONSE: bool;
    /// A response block carries the tag of the command it answers.
    const STRICT_RESPONSE_BLOCKS: bool;
    /// Format subscriptions (`refresh-client -B`) push option changes.
    const SUBSCRIPTIONS: bool;
    /// A pane can end without its window-close being announced, so the
    /// interface surveys pane liveness instead of waiting to be told.
    const POLLS_LIVENESS: bool;
    /// `display-message -p` sanitizes control bytes for a client it does not
    /// believe speaks UTF-8, so the pane-state readers pass `-u`.
    const UTF8_FLAG: bool;
    /// A one-shot `new-window` can append (`-a`), answer with the new pane
    /// (`-P -F`) and take the window's birth options in the same command list.
    const ONESHOT_PANE_REPORT: bool;
    /// The server itself — not only the binary — must be asked its version
    /// before it births a pane: one started before an upgrade keeps running
    /// the old code.
    const ASKS_SERVER_VERSION: bool = false;
    /// The shell a remote pane falls back to: one that exists on such a host
    /// by construction.
    const REMOTE_SHELL: &'static str;

    /// Refuse a multiplexer whose `-V` banner says it cannot host sessions.
    fn check_banner(banner: &str, socket: &str) -> Result<()>;
    /// Refuse a server whose `#{version}` (or `-V`) says it cannot birth a
    /// working pane. Asked only where [`Self::ASKS_SERVER_VERSION`].
    fn check_server_version(_version: &str, _socket: &str) -> Result<()> {
        Ok(())
    }
    /// Every `set-option` the session config runs, in order. `default_command`
    /// is the shell a POSIX server should run windows through, when it has one.
    fn session_config(session: &str, default_command: Option<&str>) -> Vec<ConfigOption>;
    /// The commands that follow a control-mode `new-window` in the same list
    /// (see [`birth_options`]).
    fn birth_option_commands(window_name: &str) -> Vec<String>;
    /// Quote one argument of a control-mode command line.
    fn quote_arg(s: &str) -> String;
    /// The `new-window` arguments carrying the window's environment.
    fn env_args(env: &HashMap<String, String>) -> String;
    /// The window command a control-mode `new-window` runs.
    fn window_command(
        backend: &MuxBackend<Self>,
        window_name: &str,
        command: &str,
        args: &[String],
        env: &HashMap<String, String>,
    ) -> String;
    /// The ordered `send-keys` lines delivering `buf` to `pane_id`.
    fn send_keys_commands(pane_id: &str, buf: &[u8]) -> Vec<String>;
    /// An out-of-band paste path, where keys cannot carry a paste intact.
    fn paste_channel(&self, _socket: String) -> Option<control_mode::PasteChannel> {
        None
    }
    /// The command a connection polls hook state with, where there is no
    /// subscription to push it and a producer can exist.
    fn hook_poll_command(&self, _session: &str) -> Option<String> {
        None
    }
    /// The detached command list resizing `backend_id`'s window and pane, and
    /// how many response blocks it answers with.
    fn resize_commands(backend_id: &str, rows: u16, cols: u16, sizer: &str)
        -> (Vec<String>, usize);
    /// The one-shot argv that delivers `text` into `target` as one paste.
    fn paste_args(target: &str, text: &str) -> Vec<String>;
    /// The `run-shell` script that pastes `text` into `target`, waits a beat,
    /// then presses Enter. The server's shell runs it.
    fn deferred_prompt_script(socket: &str, target: &str, text: &str) -> String;
    /// The automation heartbeat keeper's loop, as a window command.
    fn heartbeat_loop_command(cli_path: &Path) -> String;
    /// Close a one-shot `new-window`'s arguments with the window's environment
    /// and the program it runs.
    fn push_window_program(
        cmd: &mut Command,
        command: &str,
        args: &[String],
        env: &HashMap<String, String>,
    );
}

/// Dedicated tmux socket name for an instance running out of the **default**
/// data dir — isolates thurbox sessions from the user's tmux. Dev builds use
/// "thurbox-dev" to avoid interfering with an installed release binary. An
/// instance relocated by `THURBOX_DATA_DIR` derives its own name from this one
/// ([`derived_socket`]). Crate-visible as the last-resort fallback when a
/// host's configured socket sanitizes to empty
/// (`builtin_hooks::remote_signal_target`).
pub(crate) const TMUX_SOCKET: &str = if cfg!(dev_build) {
    "thurbox-dev"
} else {
    "thurbox"
};

/// Env var overriding the **local** multiplexer socket name. Wins over the
/// data-dir derivation below, so tooling that needs a socket by name (the dev
/// sandbox, whose teardown kills it) keeps naming it.
///
/// Unix test/sandbox tooling scopes the socket by pointing `TMUX_TMPDIR` at a
/// private directory, but psmux (native Windows) has no socket-directory
/// concept — every `-L <name>` resolves machine-wide, so without this override
/// a scoped test on Windows would share (and could tear down) the user's real
/// `thurbox`/`thurbox-dev` server. Remote hosts are unaffected (their socket
/// comes from `hosts.toml`).
pub const SOCKET_OVERRIDE_ENV: &str = "THURBOX_SOCKET";

/// Env var naming the **data directory** the injected [`SOCKET_OVERRIDE_ENV`]
/// belongs to. Written beside it by `session_ops::thurbox_env_overrides`, and
/// read here to tell an inherited socket from an operator's own.
///
/// Without the pairing, a socket is a bare string with no owner, and the
/// override above wins unconditionally — including in the one case that must
/// not: thurbox injects the socket into every pane it spawns, so a sandbox, a
/// test harness or an agent that relocates itself with `THURBOX_DATA_DIR`
/// *inside* such a pane inherits a name pointing at the operator's server. The
/// database is then isolated and the tmux server is not, which is worse than no
/// isolation at all because it looks contained. An override with no owner is
/// still honoured outright: that is somebody typing it.
pub const SOCKET_OWNER_ENV: &str = "THURBOX_SOCKET_FOR";

/// The local multiplexer socket name — see [`socket_for`] for the precedence.
fn local_socket() -> String {
    socket_for(
        std::env::var(SOCKET_OVERRIDE_ENV).ok(),
        std::env::var_os(SOCKET_OWNER_ENV)
            .map(PathBuf::from)
            .as_deref(),
        crate::paths::data_directory().as_deref(),
        crate::paths::relocated_data_dir().as_deref(),
    )
}

/// Resolve the socket name from the things that can move it:
/// [`SOCKET_OVERRIDE_ENV`] when set, non-empty and **still this instance's**,
/// else a name derived from a relocated data dir, else the compile-time
/// default. Pure, so the precedence is testable without touching the process
/// environment.
///
/// The data dir is the anchor because it holds the database, and the database
/// is the record of which sessions exist: an instance with its own record of
/// them has no business creating their windows on someone else's server. A
/// relocated **config** dir alone does not move the socket — it shares the
/// default instance's sessions and must keep reaching them.
///
/// `socket_owner` is [`SOCKET_OWNER_ENV`]: the data dir the override was
/// injected for. It is what separates "the operator named this server" (no
/// owner — honoured) from "this came from the pane I am running in" (an owner
/// that no longer matches `data_dir` — dropped, so the derivation below runs).
fn socket_for(
    override_name: Option<String>,
    socket_owner: Option<&Path>,
    data_dir: Option<&Path>,
    relocated_data_dir: Option<&Path>,
) -> String {
    if let Some(name) = override_name.filter(|s| !s.is_empty()) {
        // An owner that still names this instance's data dir — or no owner at
        // all, which is an operator typing the name — keeps the override.
        let inherited_from_elsewhere =
            matches!(socket_owner, Some(owner) if Some(owner) != data_dir);
        if !inherited_from_elsewhere {
            return name;
        }
    }
    match relocated_data_dir {
        Some(dir) => derived_socket(dir),
        None => TMUX_SOCKET.to_string(),
    }
}

/// The socket an instance whose data dir is `dir` runs on: the default name
/// suffixed with a short digest of that directory. Deterministic, so the same
/// relocated instance finds its own server on every run and across releases;
/// distinct, so two of them do not share one. Separator noise is normalized
/// away first, so `/tmp/lab` and `/tmp/lab/` are one instance rather than two.
///
/// A digest collision costs no more than today's behaviour — two instances on
/// one server — and never reaches the default socket, whose name has no suffix.
fn derived_socket(dir: &Path) -> String {
    let normalized: std::path::PathBuf = dir.components().collect();
    let digest = fnv1a32(normalized.to_string_lossy().as_bytes());
    format!("{TMUX_SOCKET}-{digest:08x}")
}

/// FNV-1a, 32 bits. Written out rather than reached for in `std`: this name has
/// to be the same string in every process and every release, and neither
/// `DefaultHasher`'s algorithm nor its seed is guaranteed to be.
fn fnv1a32(bytes: &[u8]) -> u32 {
    let mut hash: u32 = 0x811c_9dc5;
    for b in bytes {
        hash ^= u32::from(*b);
        hash = hash.wrapping_mul(0x0100_0193);
    }
    hash
}

/// The socket name this instance's local sessions live on — what `thurbox-cli
/// version --json` reports so a peer attaching over ssh joins the right server,
/// and so an integrator never has to guess the name. Resolved, not constant:
/// an instance relocated by `THURBOX_DATA_DIR` runs on its own socket (see
/// `socket_for`).
pub fn local_socket_name() -> String {
    local_socket()
}

/// Socket names learned from a host's own `thurbox-cli` (`version --json`'s
/// `tmux_socket`), keyed by backend name. A host entry with no explicit
/// `socket` uses *this* build's socket name by default, which is wrong exactly
/// when the flavours differ — a dev laptop against a release host would attach
/// to an empty `thurbox-dev` server while the host's sessions sit on `thurbox`.
/// `session_ops::host_cli` records what the host said; [`host_socket`] and every
/// backend built for that host consult it at use, so a backend constructed at
/// startup follows the host once it has been asked.
fn learned_host_sockets() -> &'static Mutex<HashMap<String, String>> {
    static LEARNED: std::sync::OnceLock<Mutex<HashMap<String, String>>> =
        std::sync::OnceLock::new();
    LEARNED.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Record the socket a host's own CLI reported for itself. Ignored for a host
/// that pins `socket` in `hosts.toml` — the user's word wins.
pub fn learn_host_socket(host: &crate::session::HostDef, socket: &str) {
    if host.socket.is_some() || socket.is_empty() {
        return;
    }
    if let Ok(mut map) = learned_host_sockets().lock() {
        map.insert(host.backend_name(), socket.to_string());
    }
}

fn learned_host_socket(backend_name: &str) -> Option<String> {
    learned_host_sockets()
        .lock()
        .ok()
        .and_then(|map| map.get(backend_name).cloned())
}

/// The `-L` socket name a remote `host`'s multiplexer runs on: the host's
/// `socket` override, else what its own CLI reported, else the compile-time
/// default. Deliberately **not** this process's own local socket: a relocation
/// here moves *our* sessions, while the host's sessions live wherever the
/// thurbox on that host put them — which is what [`learn_host_socket`] records.
/// Single source of truth shared by backend construction and the psmux
/// hook-signal rewrite (which must bake the socket into the command — psmux has
/// no `$TMUX`-style in-pane socket resolution to rely on).
pub fn host_socket(host: &crate::session::HostDef) -> String {
    host.socket
        .clone()
        .or_else(|| learned_host_socket(&host.backend_name()))
        .unwrap_or_else(|| TMUX_SOCKET.to_string())
}

/// tmux session name used to group all thurbox windows.
/// Dev builds use "thurbox-dev" to avoid interfering with an installed release binary.
const TMUX_SESSION: &str = if cfg!(dev_build) {
    "thurbox-dev"
} else {
    "thurbox"
};

/// Build a [`Command`] for the local multiplexer on the thurbox socket:
/// `<DEFAULT_MUX> -L <TMUX_SOCKET> <args…>`. The headless one-shot helpers below
/// (send/capture/spawn/kill/heartbeat) bypass the transport seam — they
/// are local-only — so this centralizes the binary name (`tmux`, or `psmux` on
/// Windows) and socket instead of hardcoding `tmux` at each call site.
fn local_mux_command(args: &[&str]) -> Command {
    let mut cmd = Command::new(DEFAULT_MUX);
    cmd.arg("-L").arg(local_socket()).args(args);
    // Strip nesting env so these one-shots target thurbox's own socket even when
    // thurbox is launched inside a tmux/psmux pane (see `strip_mux_nesting_env`).
    crate::agent::transport::strip_mux_nesting_env(&mut cmd);
    cmd
}

/// Window-name prefix for thurbox-managed tmux windows. Combined with the
/// sanitized session name (`{prefix}{sanitized_name}`) to form the tmux
/// window target.
pub(crate) const WINDOW_PREFIX: &str = "tb-";

/// Prefix for the companion shell window a session lazily spawns.
pub(crate) const SHELL_WINDOW_PREFIX: &str = "tbs-";

/// Sanitize a session name into a tmux-safe window-name component.
///
/// tmux parses target strings as `session:window`, and — depending on
/// version and context (e.g. `run-shell` scripts, `display-message`
/// format expansion) — treats whitespace, colons, commas, and `.` as
/// delimiters within the target string. Any character outside
/// `[A-Za-z0-9_-]` is replaced with `_` so the produced window name
/// round-trips cleanly through every tmux CLI/control-mode call.
///
/// The resulting string is deterministic — callers must use it both at
/// window-creation time and at lookup time for matching to succeed.
pub(crate) fn sanitize_window_name(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for c in name.chars() {
        if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
            out.push(c);
        } else {
            out.push('_');
        }
    }
    out
}

/// Build the tmux window name for a thurbox agent session: `tb-<safe>`.
pub(crate) fn agent_window_name(session_name: &str) -> String {
    format!("{WINDOW_PREFIX}{}", sanitize_window_name(session_name))
}

/// Build the tmux window name for a session's companion shell pane.
pub(crate) fn shell_window_name(session_name: &str) -> String {
    format!(
        "{SHELL_WINDOW_PREFIX}{}",
        sanitize_window_name(session_name)
    )
}

/// Prefix for a pane a *plugin* asked for, holding a program it named.
///
/// A third prefix rather than reusing `tbs-`: window discovery adopts panes by
/// prefix, and a plugin's pane must never be picked up as a session's anything.
pub(crate) const PROGRAM_WINDOW_PREFIX: &str = "tbp-";

/// Build the tmux window name for a plugin-owned program pane.
///
/// `owner` is a short **digest** of the owning plugin's path, computed by the
/// caller, not the path itself. Two reasons, and the first is a correctness one:
/// [`sanitize_window_name`] maps every character outside `[A-Za-z0-9_-]` to `_`,
/// so `plugins/90_watch.lua` and `plugins.90.watch.lua` would sanitize to the same
/// window and two plugins would share one program. The second is that a path is
/// long enough to make the window list unreadable.
///
/// Deterministic, which is the whole mechanism for finding the window again after
/// a restart — there is no stored pane id to go stale.
pub(crate) fn program_window_name(owner: &str, pane: &str) -> String {
    format!(
        "{PROGRAM_WINDOW_PREFIX}{}-{}",
        sanitize_window_name(owner),
        sanitize_window_name(pane)
    )
}

/// The `list-windows` format `discover` reads: pane, name, liveness, and the
/// two stamps that give the window an identity its name cannot.
///
/// An option a window does not carry expands to the empty string, which is
/// exactly how an unstamped window should read.
///
/// The two option names are spelled out because a `const` cannot interpolate
/// another; `the_discover_format_reads_both_stamps` pins them to the constants.
const DISCOVER_FORMAT: &str =
    "#{pane_id}|#{window_name}|#{pane_dead}|#{@thurbox_session}|#{@thurbox_role}";

/// What `new-window -P -F` is asked to answer with: the pane to attach to, and
/// the window whose close will be that pane's death notice.
///
/// Both in one answer because the window is needed at the same moment the pane
/// is — see `register_pane`, where asking for it separately used to leave a gap
/// a short-lived program could end inside. `the_spawn_format_asks_for_the_window_too`
/// pins it.
const SPAWN_FORMAT: &str = "#{pane_id} #{window_id}";

/// One `list-windows` line, or `None` for a window that is not thurbox's.
///
/// Every thurbox prefix is discovered, not just the agent's: a `tbs-` shell and
/// a `tbp-` program are windows an ownership question can be asked about too,
/// and leaving them out of the listing is what made a name look unambiguous
/// when it was not.
fn parse_discovered(line: &str, stamps: bool) -> Option<DiscoveredSession> {
    let mut parts = line.splitn(5, '|');
    let pane = parts.next()?;
    let name = parts.next()?;
    let dead = parts.next()?;
    // Trailing fields are absent rather than empty on a multiplexer that drops
    // them (ADR-13); an unstamped window is the same answer either way.
    //
    // Dropped outright when this multiplexer's `#{@...}` is not a *window's*
    // option (`stamps`, and psmux is the one — ADR-13): there the answer is one
    // global option handed back for every window, which is not a weaker claim
    // about whose window this is but no claim at all. Believed, it makes one
    // session's id every window's and loses every pane on the server; see
    // `a_global_stamp_is_not_read_as_every_windows_identity`.
    let (session, role) = match stamps {
        true => (parts.next().unwrap_or(""), parts.next().unwrap_or("")),
        false => ("", ""),
    };

    // The name still decides what is ours, because an unstamped window has
    // nothing else — the stamp decides *whose*, which is a different question.
    let by_name = WindowRole::from_window_name(name)?;
    if !control_mode::is_valid_pane_id(pane) {
        warn!("Skipping discovered window with invalid pane id: {pane:?}");
        return None;
    }
    Some(DiscoveredSession {
        backend_id: pane.to_string(),
        name: name.to_string(),
        is_alive: !parse_pane_dead(dead),
        // A stamp is only worth reading if it is a session id: anyone can set a
        // window option, and a multiplexer that does not expand `#{@...}` hands
        // the format string straight back.
        session: match session.parse::<crate::session::SessionId>() {
            Ok(id) => id.to_string(),
            Err(_) => String::new(),
        },
        role: WindowRole::parse(role).unwrap_or(by_name),
    })
}

/// The window name a session's `role` window carries.
fn window_name_for(role: WindowRole, session_name: &str) -> String {
    match role {
        WindowRole::Shell => shell_window_name(session_name),
        _ => agent_window_name(session_name),
    }
}

/// The tmux window option carrying the id of the session row that owns a
/// window — the identity a window has that a name and a pane id do not.
///
/// A window *name* is neither unique (two sessions may be given the same one,
/// and ADR-24 mirrors a host's names verbatim) nor injective
/// (`sanitize_window_name` collapses `a:b` and `a.b` onto one), while tmux
/// reissues pane ids from `%0` every time its server starts. A window option
/// survives both, stored once on the thing it describes — the same channel
/// [`set_own_pane_state`] already uses for hook state. See ADR-25.
pub const WINDOW_SESSION_OPTION: &str = "@thurbox_session";

/// The tmux window option saying what a stamped window is *for*.
///
/// Part of the address rather than decoration: a session owns an agent window
/// and a companion shell window, and both carry its id.
pub const WINDOW_ROLE_OPTION: &str = "@thurbox_role";

impl WindowRole {
    /// The role implied by an unstamped thurbox window name.
    fn from_window_name(name: &str) -> Option<Self> {
        if name.starts_with(SHELL_WINDOW_PREFIX) {
            Some(Self::Shell)
        } else if name.starts_with(PROGRAM_WINDOW_PREFIX) {
            Some(Self::Program)
        } else if name.starts_with(WINDOW_PREFIX) {
            Some(Self::Agent)
        } else {
            None
        }
    }
}

/// Should a window of this name keep its pane's frame after the pane dies?
///
/// Yes for an agent (`tb-`) and no for everything else, and the difference is
/// **how each one's death is noticed**:
///
/// - A session's liveness is read from a *listing* (`#{pane_dead}`, see
///   [`WindowIndex`]), which a kept window answers truthfully. Keeping it is the
///   point: the agent's last screen — the error it printed — stays attachable,
///   and `live_agent_window` still refuses to call the corpse an agent.
/// - A shell (`tbs-`) and a plugin's program (`tbp-`) are read from their pane's
///   output *stream*, and tmux announces a pane's death only by closing its
///   window. A kept window is therefore a death that is never announced: the
///   pane paints a frozen grid, `start_program` answers "already running" for
///   ever, and the editor cannot be reopened (measured on a live session
///   2026-09-11).
///
/// The shell only gets half of that today, and deliberately so for now: `off`
/// makes its death *reportable*, and nothing reports it. `Session::has_exited`
/// reads the agent's pane alone, `ShellPane`'s own flag has no reader anywhere,
/// and `ensure_shell_pane` returns early on a slot that is already filled — so
/// typing `exit` in a `Ctrl+T` shell still leaves a frozen grid that `Ctrl+T`
/// will not replace. That is what it did before this too, by accident rather
/// than by design. Dropping the shell pane when its reader ends is the fix, and
/// it is a different change from this one: it decides what happens to a pane the
/// user is looking at, where this only decides whether tmux tells anyone.
///
/// Stated per window because `remain-on-exit` is a window option that cannot be
/// set for a session (see [`SESSION_OPTS`]) — and stated even when the answer is
/// tmux's own default, because the user's `~/.tmux.conf` is read on thurbox's
/// socket too and may have turned it on globally.
fn keeps_dead_pane(window_name: &str) -> bool {
    matches!(
        WindowRole::from_window_name(window_name),
        Some(WindowRole::Agent)
    )
}

/// The window options a window thurbox creates is given **in the same command
/// list as its creation**, in order.
///
/// Both are window options that cannot be waited for. A window is born with the
/// server-wide default ([`WINDOW_OPTS`]), and "afterwards" was a second message
/// to tmux: a command that exits instantly (a missing agent binary, which #1104
/// made a supported state rather than an error) dies in that gap, takes its
/// window with it, and if that window is the last one on the server tmux exits.
///
/// Measured, tmux 3.2a: five windows created with `sh -c 'exit 7'` and
/// `remain-on-exit` set by a second call were gone every time (`no such
/// window`); created with the option chained into the same command list, the
/// corpse was kept every time. A command list runs to completion before the
/// server returns to its event loop, so there is no moment in it for a pane to
/// be reaped.
pub(crate) fn birth_options(window_name: &str) -> [(&'static str, &'static str); 2] {
    [
        // Both answers are stated, not just the one that differs from the
        // default: the server-wide value is a best-effort write of its own
        // ([`WINDOW_OPTS`]), and a program window that inherited `on` from a
        // user's `~/.tmux.conf` because that write failed is a pane whose death
        // is never announced. It costs nothing to say — this is the same
        // message, not another one.
        (
            "remain-on-exit",
            if keeps_dead_pane(window_name) {
                "on"
            } else {
                "off"
            },
        ),
        // Said per window because it **must not** be the server-wide default:
        // tmux asks for a window's size before that window exists
        // (`spawn_window` calls `default_window_size(…, w = NULL)`), and the
        // manual branch of `clients_calculate_size` reads `w->manual_sx`
        // without checking — a NULL dereference that takes the whole server
        // down. Measured, tmux 3.5a: with `set-option -w -g window-size
        // manual`, *every* `new-window` on a server with no attached client
        // answered `server exited unexpectedly`; with the same option said per
        // window it answers with a pane id. Unguarded in 3.3 … 3.6 (guarded
        // only on tmux master). The option itself is older than the supported
        // floor — tmux 2.9 added it, `manual` and per-window `setw` included
        // (`CHANGES`, 2.8 → 2.9) — so it needs no version gate: measured, tmux
        // 3.2 and 3.2a accept it chained after `new-window`, and survive it
        // server-wide as well. Stating it after the window exists is what
        // `main` did by accident, where a session-scoped write landed on the
        // session's current window and on no other.
        ("window-size", "manual"),
    ]
}

/// [`birth_options`] as the commands that follow `new-window` in a control-mode
/// command list.
///
/// The target is left unsaid on purpose: `new-window` without `-d` makes the
/// window it created current, and the bare form is therefore exactly that
/// window — including when an older window of the same name exists, which
/// `-t <name>` would resolve to instead (measured: the lowest index wins).
/// The `-d` path cannot use that and names its window; see [`spawn_window`].
pub(crate) fn set_window_option_commands(window_name: &str) -> Vec<String> {
    birth_options(window_name)
        .iter()
        .map(|(key, value)| format!("set-window-option {key} {value}"))
        .collect()
}

/// One thurbox window as a listing reported it.
#[derive(Clone, Debug)]
struct ListedWindow {
    pane: String,
    /// The [`WINDOW_SESSION_OPTION`] stamp, empty for a window spawned before
    /// windows were stamped (or by a multiplexer without window options).
    session: String,
    alive: bool,
}

/// A backend's thurbox windows, indexed the two ways ownership is asked about.
///
/// Built from one `list-windows`, so every question below is answered against
/// the same instant rather than a fresh round trip each.
#[derive(Clone, Debug, Default)]
pub struct WindowIndex {
    /// Windows carrying a stamp, by the identity they carry.
    stamped: HashMap<(String, WindowRole), Vec<ListedWindow>>,
    /// Every thurbox window by name, stamped or not. This is what tells an
    /// *ambiguous* name apart from an absent one.
    by_name: HashMap<String, Vec<ListedWindow>>,
    /// The window each listed pane sits in, for the one question asked the
    /// other way round: is *this* pane still ours (see [`Self::places_agent`]).
    by_pane: HashMap<String, (String, String, WindowRole)>,
}

impl WindowIndex {
    /// Index one backend's listing.
    pub fn from_listing(windows: impl IntoIterator<Item = DiscoveredSession>) -> Self {
        let mut index = Self::default();
        for window in windows {
            let listed = ListedWindow {
                pane: window.backend_id,
                session: window.session,
                alive: window.is_alive,
            };
            if !listed.session.is_empty() {
                index
                    .stamped
                    .entry((listed.session.clone(), window.role))
                    .or_default()
                    .push(listed.clone());
            }
            index.by_pane.insert(
                listed.pane.clone(),
                (window.name.clone(), listed.session.clone(), window.role),
            );
            index.by_name.entry(window.name).or_default().push(listed);
        }
        index
    }

    /// Where a session's agent window is, counting one whose pane has exited —
    /// `remain-on-exit` keeps that window in place, and it is still the
    /// session's own (the interface attaches to it to show what went wrong).
    pub fn agent_window(&self, session_id: &str, session_name: &str) -> Located {
        self.locate(session_id, session_name, WindowRole::Agent, false)
    }

    pub fn agent_liveness(
        &self,
        session_id: &str,
        session_name: &str,
    ) -> crate::agent::backend::BackendLiveness {
        use crate::agent::backend::BackendLiveness;
        match self.agent_window(session_id, session_name) {
            Located::At(_) => match self.live_agent_window(session_id, session_name) {
                Located::At(_) => BackendLiveness::Live,
                _ => BackendLiveness::Exited,
            },
            Located::Absent => BackendLiveness::Missing,
            Located::Unknown => BackendLiveness::Unknown,
        }
    }

    /// Where a session's *running* agent window is. The question every
    /// relaunch and liveness gate asks: a dead pane is not an agent.
    pub fn live_agent_window(&self, session_id: &str, session_name: &str) -> Located {
        self.locate(session_id, session_name, WindowRole::Agent, true)
    }

    /// Where a session's companion shell window is. Resolved by the same stamp
    /// as its agent: the row's `shell_backend_id` is only ever set once the
    /// interface has opened the shell, so a session that has one is routinely a
    /// session whose column is NULL.
    pub fn shell_window(&self, session_id: &str, session_name: &str) -> Located {
        self.locate(session_id, session_name, WindowRole::Shell, false)
    }

    /// Whether the listing puts `pane` in an agent window this session may
    /// claim — the positive reading, where a pane the listing does not place is
    /// only an absence.
    ///
    /// Asked of a pane the row already remembers, so it is answered the other
    /// way round from [`Self::agent_window`]: a stamp for this session settles
    /// it, and an *unstamped* window of this session's name is claimable too —
    /// that is the pre-ADR-25 shape, and nothing in the listing contradicts it.
    /// What it will not do is hand over a window stamped for somebody else,
    /// which is what a remembered pane id becomes once a tmux server restart
    /// has reissued it.
    pub fn places_agent(&self, session_id: &str, session_name: &str, pane: &str) -> bool {
        let Some((window, stamp, role)) = self.by_pane.get(pane) else {
            return false;
        };
        *role == WindowRole::Agent
            && if stamp.is_empty() {
                *window == agent_window_name(session_name)
            } else {
                stamp == session_id
            }
    }

    /// The resolution rule, in one place.
    ///
    /// A stamp is proof and is taken first. Without one the *name* is all
    /// there is, and it only decides anything while a single window answers to
    /// it: a lone unstamped window of the right name is this session's (the
    /// migration path for a window spawned before stamping, and for every
    /// window under a multiplexer with no window options), a lone window
    /// stamped for somebody else is theirs, and several windows with no stamp
    /// between them cannot be told apart.
    fn locate(
        &self,
        session_id: &str,
        session_name: &str,
        role: WindowRole,
        live_only: bool,
    ) -> Located {
        match self.stamped_match(session_id, role, live_only) {
            Some(found) => found,
            None => self.named_match(session_id, session_name, role, live_only),
        }
    }

    /// The stamp half: proof, and so taken first.
    ///
    /// `None` means there is no stamp to go on and the name is all that is
    /// left — which is not the same as an answer of [`Located::Absent`], and is
    /// why this is an `Option` rather than a `Located`.
    fn stamped_match(
        &self,
        session_id: &str,
        role: WindowRole,
        live_only: bool,
    ) -> Option<Located> {
        if session_id.is_empty() {
            return None;
        }
        match self
            .stamped
            .get(&(session_id.to_string(), role))?
            .as_slice()
        {
            [] => None,
            // One session, one window per role — two is a listing nobody can
            // act on rather than a choice to make. This holds regardless of
            // liveness: a dead entry does not make the ambiguity go away, and
            // must never fall through to a same-named window that belongs to
            // somebody else.
            [_, _, ..] => Some(Located::Unknown),
            [only] => Some(if !live_only || only.alive {
                Located::At(only.pane.clone())
            } else {
                Located::Absent
            }),
        }
    }

    /// The name half, reached only when no stamp decided it.
    fn named_match(
        &self,
        session_id: &str,
        session_name: &str,
        role: WindowRole,
        live_only: bool,
    ) -> Located {
        let usable = |w: &&ListedWindow| !live_only || w.alive;
        let window = window_name_for(role, session_name);
        let named: Vec<&ListedWindow> = self
            .by_name
            .get(&window)
            .into_iter()
            .flatten()
            .filter(usable)
            .collect();
        match named.as_slice() {
            [] => Located::Absent,
            // A caller with no id of its own (a window addressed only by name)
            // can claim a lone window; one with an id can only claim a window
            // that is not already somebody else's.
            [only] if session_id.is_empty() || only.session.is_empty() => {
                Located::At(only.pane.clone())
            }
            [_] => Located::Absent,
            // Every candidate stamped, none of them ours: definitively not here.
            _ if !session_id.is_empty() && named.iter().all(|w| !w.session.is_empty()) => {
                Located::Absent
            }
            _ => Located::Unknown,
        }
    }
}

/// The `list-windows` format [`retire_duplicate_windows`] reads: the window to
/// act on, and the stamp saying whose it is.
///
/// A listing of its own rather than [`DISCOVER_FORMAT`]'s, because the key the
/// retirement decides by is the one thing a [`WindowIndex`] does not carry.
/// `#{window_id}` is what tmux issued the window, and the order it issued them
/// in; the pane a `WindowIndex` holds is the window's *active* one, which a
/// split would move.
const RETIRE_FORMAT: &str = "#{window_id}|#{@thurbox_session}|#{@thurbox_role}";

/// The windows a listing puts `session_id`'s `role` stamp on, oldest first.
///
/// Ordered by the number in `@N`, and a window id that does not parse is left
/// out entirely: the whole point of the order is to decide which window is
/// killed, and an id nothing can place is not one to decide that about.
fn stamped_windows_in(listing: &str, session_id: &str, role: WindowRole) -> Vec<String> {
    let mut found: Vec<(u64, String)> = listing
        .lines()
        .filter_map(|line| {
            let mut parts = line.splitn(3, '|');
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

/// `ssh`'s own failure code — see [`crate::session_ops::host_cli::Reach`],
/// which draws the same distinction one layer up.
const SSH_ERROR_EXIT: i32 = 255;

/// The most a command issued from the interface's own loop may cost it —
/// waiting for the control lock, and for an answer where one is wanted.
///
/// Sized against the two things it sits between: a healthy round trip over an
/// already-open connection is sub-millisecond, and the budget a keypress has
/// before the interface reads as frozen is a fraction of a second. Generous
/// enough that a loaded machine or a slow link still gets a real answer; short
/// enough that a link carrying nothing costs a hitch instead of a freeze.
const LOOP_COMMAND_BUDGET: std::time::Duration = std::time::Duration::from_millis(250);

/// How often [`MuxBackend::ctrl_command_within`] retries the control lock
/// while its budget lasts. Short enough to be invisible next to the budget,
/// long enough not to spin.
const CONTROL_LOCK_POLL: std::time::Duration = std::time::Duration::from_millis(2);

/// Whether a failed listing means the server genuinely holds nothing, given
/// the layer that failed (`is_ssh` + the exit `code`) and what it said.
///
/// **Layer before text**, and the order is the point. `ssh` exits 255 for its
/// own failures and passes a remote command's status through untouched
/// (a remote `exit 7` exits 7), so 255 is ssh saying the question never
/// arrived — no matter what the stderr underneath happens to resemble.
/// Only once the transport is ruled out does the multiplexer's own answer get
/// to speak, and then only in the exact words it is documented to use
/// ([`mux_answered_absent`]).
///
/// Everything else is "could not tell", which the caller must treat as the
/// unanswered question it is: an empty listing here means *there is nothing to
/// kill*, and that is not a conclusion to reach by guessing.
fn listing_is_absence(is_ssh: bool, code: Option<i32>, stderr: &str) -> bool {
    match code {
        // ssh's own error, so nothing on the host ever saw the question.
        Some(SSH_ERROR_EXIT) if is_ssh => false,
        // Killed by a signal: it did not finish, and whatever reached stderr
        // before that is a fragment of an answer rather than one. There is no
        // layer to reason from, so there is nothing to conclude.
        None => false,
        _ => mux_answered_absent(stderr),
    }
}

/// Whether a multiplexer's stderr is its *own* answer that there is nothing to
/// act on, rather than any of the ways a question can fail to be answered.
///
/// The distinction the remote teardown rests on, and the reason this list is
/// as short as it is. Only the exact answers the supported multiplexers are
/// documented to give for "there is no server" and "there is no such session" count;
/// everything else — including a failure whose wording merely resembles one —
/// is unanswered. Over-reporting a live host as unanswered costs one cheap
/// retry, while the reverse costs an orphaned agent nobody ever looks for
/// again.
///
/// `error connecting to` is the trap and the reason this is not a prefix
/// match: tmux prints it for a socket that is not there
/// (`(No such file or directory)`) **and** for one it cannot open while a
/// server is very much alive behind it — `(Permission denied)` on another
/// user's socket, `(Connection refused)` on a stale one. Only the first is an
/// answer; reading the others as absence is exactly the "reachable failure
/// mistaken for absence" this whole path exists to stop. Widening this list to
/// cover more wordings is the trap it looks like a fix: each new string makes
/// the classifier more confidently wrong about the next one nobody anticipated.
fn mux_answered_absent(error: &str) -> bool {
    // The socket has no server behind it. tmux ≥ 3.4 words this as "error
    // connecting to <path> (<reason>)", and only this reason means absence.
    if error.contains("error connecting to") {
        return error.contains("(No such file or directory)");
    }
    // tmux < 3.4's wording for the same thing, which carries no reason at all.
    if error.contains("no server running on") {
        return true;
    }
    // The server is up and holds no session by that name, in the two wordings
    // the supported multiplexers use.
    error.contains("can't find session") || error.contains("session not found")
}

/// One command of the session config — a `set-option`, or the `if-shell` that
/// guards the tmux clipboard feature slot — and whether failing it means the
/// server cannot host sessions.
pub(crate) struct ConfigOption {
    args: Vec<String>,
    fatal: bool,
}

impl ConfigOption {
    /// A config command that is not a `set-option` (see `config_command_list`
    /// for why only `set-option` is given `-q`).
    pub(crate) fn command(args: Vec<String>, fatal: bool) -> Self {
        Self { args, fatal }
    }

    /// `set-option <args…>`.
    pub(crate) fn set(args: &[&str], fatal: bool) -> Self {
        let mut all = vec!["set-option".to_string()];
        all.extend(args.iter().map(ToString::to_string));
        Self { args: all, fatal }
    }

    #[cfg(test)]
    pub(crate) fn args(&self) -> &[String] {
        &self.args
    }
}

/// `prefix` then every option in `config`, as one tmux command list.
///
/// tmux skips the rest of a list after a command fails, so a best-effort
/// option is given `-q`: an option this tmux does not know is then not an
/// error, and cannot stop the options after it (tmux before 3.5 has no
/// `extended-keys-format`). Only `set-option` takes it; `if-shell` refuses the
/// flag and quiets its own inner command instead.
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

/// Delay between sending command text and pressing Enter via tmux, used by the
/// synchronous `send_text_now` path.
const SEND_KEYS_ENTER_DELAY: std::time::Duration = std::time::Duration::from_millis(200);

/// Hard cap on the number of scrollback lines `capture_pane_text` will return.
const MAX_CAPTURE_LINES: u32 = 10_000;

/// Rows of history a snapshot carries: as many as the rebuilt terminal keeps,
/// under the same ceiling every capture here has.
fn snapshot_history() -> usize {
    crate::session::settings::global()
        .scrollback_lines
        .min(MAX_CAPTURE_LINES as usize)
}

/// Longest agent activity line replayed from a pane title at adopt time.
///
/// A title is one line in the session list, and the value comes back from a
/// host: bounding it here means a pane whose title is a megabyte cannot make
/// the seed one.
const MAX_TITLE_SEED_BYTES: usize = 512;

/// Common pane and control connection mechanics for a concrete mux backend.
/// The transport type fixes the mux binary and protocol.
pub(crate) struct MuxBackend<T: MuxDialect> {
    /// The concrete mux launcher.
    transport: T,
    /// tmux socket name passed via `-L` (e.g. `thurbox`) as configured; read
    /// through [`Self::socket`], which prefers what the host's own CLI said.
    socket: String,
    /// tmux session name grouping all thurbox windows.
    session: String,
    /// Backend name used by the registry / persisted `backend_type`
    /// (`local-tmux` or `ssh:<host>`).
    name: String,
    control: Mutex<Option<ControlMode>>,
    /// This backend's name in [`SIZER_OPTION`] — see [`Self::resize`].
    sizer: String,
    /// The host an off-local backend was built from, for the agent `PATH`
    /// its windows get ([`crate::agent::host_path`]). `None` locally.
    host: Option<crate::session::HostDef>,
}

/// `(rows, cols)` within what `resize-window` accepts, so a resize an `if-shell`
/// runs can never be the one that fails (see `MuxDialect::resize_commands`). tmux's
/// bounds are 1 and `WINDOW_MAXIMUM`, 10000.
pub(crate) fn tmux_size(rows: u16, cols: u16) -> (u16, u16) {
    (rows.clamp(1, 10_000), cols.clamp(1, 10_000))
}

/// A name no other client of the server will have: this process, and which of
/// its backends, and when. The time is what keeps an instance on another
/// machine, whose pid may be the same, from reading as this one.
fn sizer_name() -> String {
    static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let nth = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .subsec_nanos();
    format!("{:x}-{nth:x}-{nanos:x}", std::process::id())
}

impl<T: MuxDialect> Default for MuxBackend<T> {
    fn default() -> Self {
        Self::local()
    }
}

impl<T: MuxDialect> MuxBackend<T> {
    pub(crate) fn set_name(&mut self, name: impl Into<String>) {
        self.name = name.into();
    }

    pub(crate) fn transport(&self) -> &T {
        &self.transport
    }

    /// The host an off-local backend was built from; `None` locally.
    pub(crate) fn host(&self) -> Option<&crate::session::HostDef> {
        self.host.as_ref()
    }

    /// Build the local protocol backend on thurbox's socket.
    pub fn local() -> Self {
        Self {
            transport: T::local(),
            socket: local_socket(),
            session: TMUX_SESSION.to_string(),
            name: "local-tmux".to_string(),
            control: Mutex::new(None),
            sizer: sizer_name(),
            host: None,
        }
    }

    /// Build a tmux backend over an explicit transport (used by the SSH backend).
    pub fn with_transport(
        transport: T,
        socket: impl Into<String>,
        session: impl Into<String>,
        name: impl Into<String>,
    ) -> Self {
        Self {
            transport,
            socket: socket.into(),
            session: session.into(),
            name: name.into(),
            control: Mutex::new(None),
            sizer: sizer_name(),
            host: None,
        }
    }

    /// Build an off-local tmux backend for `host` — `tmux` over SSH for an SSH
    /// host, or `tmux` inside a WSL distro via `wsl.exe`. The backend is named
    /// `ssh:<host.name>` / `wsl:<host.name>` and uses the same socket/session
    /// names as the local backend unless the host overrides them.
    pub fn from_host(host: &crate::session::HostDef) -> Self {
        let socket = host_socket(host);
        let session = host
            .session
            .clone()
            .unwrap_or_else(|| TMUX_SESSION.to_string());
        let transport = T::from_host(host);
        let mut backend = Self::with_transport(transport, socket, session, host.backend_name());
        backend.host = Some(host.clone());
        backend
    }

    /// The socket this backend talks to: the configured one, unless the host's
    /// own CLI has since reported a different one (see [`learn_host_socket`]).
    /// Resolved per call so a backend registered at startup follows the host.
    fn socket(&self) -> String {
        self.host
            .as_ref()
            .map(host_socket)
            .unwrap_or_else(|| self.socket.clone())
    }

    /// Run a tmux command and return its stdout (used before control mode is available).
    fn tmux_output(&self, args: &[&str]) -> Result<String> {
        let output = self.run_tmux(args)?;
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    }

    /// Run a tmux command, returning Ok(()) on success (used before control mode is available).
    fn tmux_run(&self, args: &[&str]) -> Result<()> {
        self.run_tmux(args)?;
        Ok(())
    }

    /// Whether a `#{@...}` this multiplexer answers with is a **window's**
    /// option ([`MuxDialect::WINDOW_OPTIONS`]). Where it is not, a stamp read
    /// back identifies nothing, and [`parse_discovered`] drops it rather than
    /// reading one session's id as every window's.
    fn stamps_are_per_window(&self) -> bool {
        T::WINDOW_OPTIONS
    }

    /// One `list-windows`, with an empty answer only when the multiplexer
    /// itself said there is nothing to list.
    ///
    /// [`discover`](SessionBackend::discover) gates on `has-session` and reads
    /// its failure as "no windows", which over a transport conflates the two
    /// answers a teardown must never confuse: *the host says it holds nothing*
    /// and *the host did not answer*. A force delete taken while a host was
    /// briefly unreachable therefore reported nothing to kill, recorded no
    /// error, and left the agent running there for good. Here an unrecognised
    /// failure is an error, so the caller can say so and come back later.
    ///
    /// Also one round trip instead of two: `list-windows` on an absent server
    /// gives exactly the refusal `has-session` was asked for.
    fn discover_answered(&self) -> Result<Vec<DiscoveredSession>> {
        let args = ["list-windows", "-t", &self.session, "-F", DISCOVER_FORMAT];
        // Run it here rather than through `run_tmux`, which formats the
        // failure into a message: the whole point is to keep the exit status,
        // because that is the layer talking and the message is only text.
        let output = self
            .transport
            .mux_command(&self.socket(), &args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            // The launcher would not even start — no `ssh`/`wsl.exe`/`tmux` on
            // this machine. Nothing was asked, so nothing was answered, and
            // `launch_failure` says which of the three is not there.
            .map_err(|e| {
                crate::agent::preflight::launch_failure(
                    &self.transport,
                    "Failed to run tmux command",
                    e,
                )
            })?;
        if output.status.success() {
            let stamps = self.stamps_are_per_window();
            return Ok(String::from_utf8_lossy(&output.stdout)
                .lines()
                .filter_map(|line| parse_discovered(line, stamps))
                .collect());
        }
        let stderr = String::from_utf8_lossy(&output.stderr);
        if listing_is_absence(self.transport.is_ssh(), output.status.code(), stderr.trim()) {
            return Ok(Vec::new());
        }
        // Plain stderr, as `run_tmux` reports it: `agent` may not reach into
        // `git` (its stderr cleaner lives there), and the architecture test
        // enforces that.
        bail!("tmux {} failed: {}", args.join(" "), stderr.trim())
    }

    /// Kill a pane with a one-shot command rather than through control mode.
    ///
    /// The teardown path's kill. [`SessionBackend::kill`] goes through control
    /// mode, which a caller must open with
    /// [`ensure_ready`](SessionBackend::ensure_ready) — and that *creates* the
    /// server and the thurbox session when they are absent, so tearing a
    /// session down on a host would leave an empty server behind. A pane that
    /// is already gone is not an error: the teardown got what it wanted.
    fn kill_pane_oneshot(&self, pane: &str) -> Result<()> {
        match self.run_tmux(&["kill-pane", "-t", pane]) {
            Ok(_) => Ok(()),
            Err(e) if format!("{e:#}").contains("find pane") => Ok(()),
            Err(e) => Err(e),
        }
    }

    /// Execute a tmux command on the thurbox socket and check for errors.
    fn run_tmux(&self, args: &[&str]) -> Result<std::process::Output> {
        let output = self
            .transport
            .mux_command(&self.socket(), args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .map_err(|e| {
                crate::agent::preflight::launch_failure(
                    &self.transport,
                    "Failed to run tmux command",
                    e,
                )
            })?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            bail!("tmux {} failed: {}", args.join(" "), stderr.trim());
        }

        Ok(output)
    }

    /// Check if the thurbox tmux session exists.
    fn session_exists(&self) -> bool {
        self.tmux_run(&["has-session", "-t", &self.session]).is_ok()
    }

    /// Apply server + session config to the tmux session.
    ///
    /// Idempotent (`set-option` overwrites), so it is safe to call on every
    /// [`ensure_ready`](Self::ensure_ready) — the session may have been created
    /// elsewhere (e.g. a headless spawn) without these options, and re-applying
    /// is the single source of truth for both the TUI and headless paths.
    ///
    /// Where command lists exist ([`MuxDialect::COMMAND_LISTS`]) the whole
    /// config is **one** invocation (#1243): it runs on every `session create`,
    /// and as ten processes it was most of that command's cost. A failure in
    /// the list is then re-run one option at a time, which is what tells a
    /// fatal option from a best-effort one.
    fn apply_session_config(&self) -> Result<()> {
        let config = self.session_config();
        if T::COMMAND_LISTS && self.tmux_run(&config_command_list(&[], &config)).is_ok() {
            return Ok(());
        }
        for option in &config {
            let args: Vec<&str> = option.args.iter().map(String::as_str).collect();
            match self.tmux_run(&args) {
                Ok(()) => {}
                Err(e) if option.fatal => return Err(e),
                Err(e) => debug!("tmux option {} not set: {e}", option.args.join(" ")),
            }
        }
        Ok(())
    }

    /// Every `set-option` [`apply_session_config`](Self::apply_session_config)
    /// runs, in order — the dialect's config for this session.
    fn session_config(&self) -> Vec<ConfigOption> {
        T::session_config(&self.session, self.default_command().as_deref())
    }

    /// The shell a POSIX server runs windows through (`default-command`), or
    /// `None` where there is no POSIX shell to pin.
    fn default_command(&self) -> Option<String> {
        #[cfg(windows)]
        {
            None
        }
        #[cfg(not(windows))]
        {
            Some(self.config_shell())
        }
    }

    /// Ensure the thurbox tmux session exists and its options are applied,
    /// **without** starting control mode.
    ///
    /// Shared by [`ensure_ready`](Self::ensure_ready) (which then starts control
    /// mode) and the headless spawn paths ([`spawn_window`],
    /// [`ensure_automation_heartbeat`]) that drive tmux via one-shot commands and
    /// must not open a control-mode connection.
    fn ensure_session_configured(&self) -> Result<()> {
        // The common case — the session is there — asked and configured in one
        // process: `has-session` failing stops the list before any option is
        // set, and the path below then says why.
        if T::COMMAND_LISTS {
            let config = self.session_config();
            let list = config_command_list(&["has-session", "-t", &self.session], &config);
            if self.tmux_run(&list).is_ok() {
                return Ok(());
            }
        }
        if !self.session_exists() {
            // No session to ask for `#{version}` yet, and creating one may start
            // a server — with an idle shell in it — that every spawn would then
            // refuse. The binary is what would start it, so it answers instead.
            if T::ASKS_SERVER_VERSION {
                T::check_server_version(&self.tmux_output(&["-V"])?, &self.socket())?;
            }
            debug!(
                "Creating tmux session '{}' on socket '{}'",
                self.session,
                self.socket()
            );
            if let Err(e) = self.run_tmux(&[
                "new-session",
                "-d",
                "-s",
                &self.session,
                "-x",
                "80",
                "-y",
                "24",
            ]) {
                // The check above is not a lock, and after a reboot every
                // session's relaunch runs it at once: one wins and the rest are
                // told `duplicate session`. Failing them is how a machine came
                // back with all but one of its agents missing — each loser
                // aborted its whole respawn, kept the pane id the dead server
                // had given it, and that id then named whichever window the
                // winner got. The session we wanted exists either way, so ask
                // rather than assume, and only report a failure that left none.
                if !self.session_exists() {
                    // A launcher that never started is already the whole story
                    // (`preflight::launch_failure`), and this context in front
                    // of it costs the reader the part that names the binary and
                    // the fix — a message row is one line wide.
                    if crate::agent::preflight::is_missing_dependency(&e) {
                        return Err(e);
                    }
                    return Err(e).context("Failed to create tmux session");
                }
                debug!(
                    "tmux session '{}' was created by a peer; continuing",
                    self.session
                );
            }
            // Cheap defensiveness on Windows: poll until the freshly-created
            // session answers `has-session` before applying options. (The
            // `no server running on 'thurbox__thurbox'` failure that originally
            // motivated this was actually session *nesting*, now fixed at the
            // root by `strip_mux_nesting_env`; this poll is a harmless belt
            // against any genuinely-async `new-session -d` and a no-op when the
            // first probe succeeds — which it does on the normal path.)
            #[cfg(windows)]
            self.wait_for_session_ready();
        }
        self.apply_session_config()
    }

    /// Poll (up to 5s) until the freshly-created session answers `has-session`.
    /// Defensive belt against an async `new-session -d`; normally a no-op (the
    /// first probe succeeds). See
    /// [`ensure_session_configured`](Self::ensure_session_configured).
    #[cfg(windows)]
    fn wait_for_session_ready(&self) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while std::time::Instant::now() < deadline {
            if self.session_exists() {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    }

    /// The shell tmux should use for `default-command`. Local uses the user's
    /// `$SHELL`; a remote backend uses a POSIX shell guaranteed to exist on the
    /// remote host. Not used on Windows, and a dialect with no POSIX shell
    /// ignores it (see [`MuxDialect::session_config`]).
    ///
    /// The value must be a single, space-free token: it round-trips through the
    /// remote transport's per-argument shell-quoting (`ssh`/`wsl.exe`), where a
    /// space would be re-split by the remote shell into extra `set-option` args.
    /// The login-shell `PATH` fix for remote agents (e.g. `claude` under
    /// `~/.local/bin`) is applied at the *window command* instead — see
    /// `TmuxTransport`'s window command.
    #[cfg(not(windows))]
    fn config_shell(&self) -> String {
        if self.transport.is_remote() {
            "/bin/sh".to_string()
        } else {
            std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string())
        }
    }

    /// Run a closure with a reference to the active control mode, or bail if
    /// it has not been started yet.
    ///
    /// Centralizes the "lock + assert started" invariant in one place so
    /// callers receive a guaranteed-live `&ControlMode` and never touch the
    /// `Option` directly. This replaces a former pattern where each call site
    /// re-asserted the invariant with `guard.as_ref().unwrap()` after a
    /// separate `is_none()` check — fragile, since a refactor of the check
    /// could silently leave the `unwrap`s reachable.
    fn with_control<F, R>(&self, f: F) -> Result<R>
    where
        F: FnOnce(&ControlMode) -> Result<R>,
    {
        let guard = self
            .control
            .lock()
            .map_err(|e| anyhow::anyhow!("control lock: {e}"))?;
        let ctrl = guard.as_ref().ok_or_else(|| {
            anyhow::anyhow!("Control mode not started — call ensure_ready() first")
        })?;
        f(ctrl)
    }

    /// Drop the dead control mode connection and start a fresh one.
    fn reconnect_control(&self) -> Result<()> {
        let mut guard = self
            .control
            .lock()
            .map_err(|e| anyhow::anyhow!("control lock: {e}"))?;
        // Start the replacement *before* touching `guard`, and only store it on
        // success. A failed `start()` propagates via `?` while the existing
        // handle stays in place — so a retry reconnects cleanly instead of
        // hitting `control = None` and reporting the misleading "call
        // ensure_ready() first". Assigning `Some(fresh)` drops the dead
        // ControlMode (its cleanup) as it replaces it.
        let fresh =
            ControlMode::start(&self.transport, &self.socket(), &self.session, &self.sizer)?;
        *guard = Some(fresh);
        debug!("Control mode reconnected successfully");
        Ok(())
    }

    /// Send a command via control mode and return the response.
    /// On broken pipe or timeout, reconnects control mode and retries once.
    fn ctrl_command(&self, cmd: &str) -> Result<String> {
        self.ctrl_command_list(&[cmd])
    }

    /// [`Self::ctrl_command`] for a command list, answered once every command
    /// in it has answered (see `ControlMode::send_command_list`).
    fn ctrl_command_list(&self, cmds: &[&str]) -> Result<String> {
        let result = self.with_control(|ctrl| ctrl.send_command_list(cmds));
        match result {
            Ok(val) => Ok(val),
            Err(err) if is_broken_pipe(&err) || is_recv_timeout(&err) => {
                warn!("Control mode error, reconnecting: {err:#}");
                self.reconnect_control()?;
                self.with_control(|ctrl| ctrl.send_command_list(cmds))
            }
            Err(err) => Err(err),
        }
    }

    /// Ask one question on a budget, for a caller that must not be made to
    /// wait: the control lock and the answer together get `budget`, and
    /// neither a lock held by someone else's round trip nor a link that has
    /// stopped carrying anything can overrun it.
    ///
    /// No reconnect on failure, unlike every other path here. A reconnect is a
    /// fresh ssh handshake plus the implicit attach response read back
    /// synchronously — precisely the unbounded wait this exists to avoid — and
    /// the callers that *can* wait will reconnect soon enough.
    fn ctrl_command_within(&self, cmd: &str, budget: std::time::Duration) -> Result<String> {
        let deadline = std::time::Instant::now() + budget;
        self.with_control_until(deadline, |ctrl| {
            ctrl.send_command_within(
                cmd,
                deadline.saturating_duration_since(std::time::Instant::now()),
            )
        })
    }

    /// Send a command whose answer nobody reads, without waiting for it.
    ///
    /// Bounded and reconnect-free for [`Self::ctrl_command_within`]'s reasons —
    /// the budget covers only the lock, there being no answer to wait for.
    /// `blocks` is what [`ControlMode::send_command_detached`] says it is.
    fn ctrl_command_detached(&self, cmds: &[&str], blocks: usize) -> Result<()> {
        self.with_control_until(std::time::Instant::now() + LOOP_COMMAND_BUDGET, |ctrl| {
            ctrl.send_command_detached(cmds, blocks)
        })
    }

    /// [`Self::with_control`], but it will not wait past `deadline` for the lock.
    ///
    /// The plain lock is held across a whole round trip, so one backend is one
    /// queue and a caller can be made to wait out someone else's command as
    /// well as its own — the mirror pass and the attach worker share this lock
    /// with the loop. A caller that must not block bounds the wait here and
    /// takes "busy" for an answer.
    fn with_control_until<F, R>(&self, deadline: std::time::Instant, f: F) -> Result<R>
    where
        F: FnOnce(&ControlMode) -> Result<R>,
    {
        let guard = loop {
            match self.control.try_lock() {
                Ok(guard) => break guard,
                Err(std::sync::TryLockError::Poisoned(e)) => bail!("control lock: {e}"),
                Err(std::sync::TryLockError::WouldBlock) => {
                    if std::time::Instant::now() >= deadline {
                        bail!("control mode is busy with another command");
                    }
                    std::thread::sleep(CONTROL_LOCK_POLL);
                }
            }
        };
        let ctrl = guard
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("Control mode not started"))?;
        f(ctrl)
    }

    /// Send a command via control mode without waiting for a response.
    /// On broken pipe, reconnects control mode and retries once.
    fn ctrl_command_nowait(&self, cmd: &str) -> Result<()> {
        let result = self.with_control(|ctrl| ctrl.send_command_nowait(cmd));
        match result {
            Ok(()) => Ok(()),
            Err(err) if is_broken_pipe(&err) => {
                warn!("Control mode broken pipe (nowait), reconnecting: {err:#}");
                self.reconnect_control()?;
                self.with_control(|ctrl| ctrl.send_command_nowait(cmd))
            }
            Err(err) => Err(err),
        }
    }

    /// Register a pane sender and return the corresponding reader.
    /// Multiple instances can register the same pane; output will be broadcast to all.
    ///
    /// `window_id` is the window the pane lives in, when the caller already
    /// knows it. tmux announces a pane's death only by the window it was in
    /// (`%window-close @3`), so without that mapping a pane cannot notice its
    /// own ending — and the announcement is one-shot, so learning the window
    /// late is the same as never learning it.
    ///
    /// Answers with where the pane's size will be reported, when it will be: a
    /// `%layout-change` names a window, so only a pane whose window is known
    /// can be told, and a dialect without [`MuxDialect::SIZE_REPORTS`] sends
    /// none.
    fn register_pane(
        &self,
        pane_id: &str,
        window_id: Option<&str>,
    ) -> Result<(ControlModeReader, Option<PaneSize>)> {
        // Handed in by whoever created the pane: `new-window` is already asked
        // to answer (`-P -F`), and answering with the window as well as the
        // pane costs nothing (see `SPAWN_FORMAT`). Asking separately is what
        // this used to do, and it put a serialized control-mode round trip —
        // queued behind every other command in flight — between the window
        // existing and the mapping being written. A program that ended inside
        // that gap had its `%window-close` arrive with nothing to match it
        // against, and no later wait brings it back.
        //
        // Asked here only when nobody could hand it over: `adopt`, which is
        // given a pane id out of the database and nothing else. Best-effort
        // there, as it always was — a backend that cannot answer (one without
        // size reports, a reconnecting control mode) keeps the old behaviour of no mapping and
        // no EOF from a window close, and says so in the log.
        //
        // With size reports the same question also reads the pane's size and who sizes it,
        // for the grid. A pane being adopted may be another instance's to size,
        // in which case the resize `connect_pane` sends next is declined, and a
        // declined resize changes nothing — no `%layout-change` would ever say
        // what size the grid should be. Read before that resize, which is
        // right either way: declined, the size is unchanged; honoured, the
        // change is reported after it, in the stream.
        let mut learned = None;
        let window_id = match window_id {
            Some(id) => Some(id.to_string()),
            None => {
                let format = if T::SIZE_REPORTS {
                    format!("#{{window_id}} #{{pane_height}} #{{pane_width}} {SIZED_BY}")
                } else {
                    "#{window_id}".to_string()
                };
                match self.ctrl_command(&format!("display-message -t {pane_id} -p '{format}'")) {
                    Ok(out) => {
                        let mut fields = out.split_whitespace();
                        let window = fields
                            .next()
                            .map(str::to_string)
                            .filter(|id| control_mode::is_valid_window_id(id));
                        let rows = fields.next().and_then(|n| n.parse::<u16>().ok());
                        let cols = fields.next().and_then(|n| n.parse::<u16>().ok());
                        let sized_by = fields.next().map(str::to_string);
                        learned = rows.zip(cols).map(|size| (size, sized_by));
                        window
                    }
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
        let (tx, rx) = sync_channel(PANE_CHANNEL_CAPACITY);
        let reader = ControlModeReader::new(rx);
        let reports = window_id.is_some() && T::SIZE_REPORTS;
        let size = reports.then(|| reader.size());
        self.with_control(|ctrl| {
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
                return Ok(());
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
            Ok(())
        })?;
        // Before any byte is read, so the history seed — captured at this size
        // — is parsed at it.
        if let (Some(size), Some(((rows, cols), sized_by))) = (&size, learned) {
            size.report(rows, cols);
            size.set_sized_elsewhere(sized_by.is_some_and(|name| name != self.sizer));
        }
        Ok((reader, size))
    }

    /// Unregister a pane sender (causes the reader to get EOF).
    /// Note: Currently removes all senders for this pane. For true instance-specific
    /// unregistration, we would need to track which sender belongs to which instance.
    fn unregister_pane(&self, pane_id: &str) -> Result<()> {
        self.with_control(|ctrl| {
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

    /// Create a writer for a specific pane.
    fn pane_writer(&self, pane_id: &str) -> Result<ControlModeWriter> {
        // Keys in the dialect's own spelling, and a paste out of band where
        // the dialect has a channel for it.
        let paste = self.transport.paste_channel(self.socket());
        self.with_control(|ctrl| {
            Ok(ControlModeWriter {
                stdin: Arc::clone(&ctrl.stdin),
                pane_id: pane_id.to_string(),
                encode: T::send_keys_commands,
                paste: paste.clone(),
            })
        })
    }

    /// Connect I/O to an existing pane: start monitoring, resize to correct
    /// dimensions, and create writer.
    fn connect_pane(
        &self,
        pane_id: &str,
        window_id: Option<&str>,
        rows: u16,
        cols: u16,
    ) -> Result<AdoptedSession> {
        let (reader, size) = self.register_pane(pane_id, window_id)?;
        // Must use send_command (waited) here — a nowait call would leave an
        // unclaimed %begin/%end response in the stream that steals the next
        // send_command waiter.
        self.ctrl_command(&format!(
            "refresh-client -A '{}:on'",
            pane_id.replace('\'', "'\\''")
        ))?;

        // Resize to the TUI panel dimensions. force_resize triggers a
        // SIGWINCH, making TUI applications (like claude) repaint at the
        // correct dimensions through the normal output stream, which the
        // reader_loop processes with all escape sequences intact.
        self.force_resize(pane_id, rows, cols)?;

        let writer = self.pane_writer(pane_id)?;

        Ok(AdoptedSession {
            output: Box::new(reader),
            input: Box::new(writer),
            seed_len: 0,
            size,
        })
    }

    /// Capture a pane's window title, scrollback history and visible screen as
    /// terminal bytes suitable for seeding a fresh vt100 parser.
    ///
    /// The control-mode `%output` stream only carries bytes emitted after the
    /// pane is connected, so an adopted session would otherwise start with an
    /// empty scrollback — the forced repaint restores the visible screen but
    /// not the history above it. `-e` keeps colors, `-J` rejoins wrapped lines
    /// so they re-wrap at the adopting panel's width, `-S -<n>` extends the
    /// capture into history (tmux clamps to what exists). The title rides
    /// along because the capture cannot carry it — see [`Self::pane_title_seed`].
    fn capture_history_seed(&self, pane_id: &str) -> Result<Vec<u8>> {
        let lines = crate::session::settings::global()
            .scrollback_lines
            .min(MAX_CAPTURE_LINES as usize);
        let start = format!("-{lines}");
        let output = self.run_tmux(&[
            "capture-pane",
            "-e",
            "-p",
            "-J",
            "-S",
            &start,
            "-t",
            pane_id,
        ])?;
        // Ahead of the history, not after it: the capture ends wherever the
        // pane's last line ended, and appending to a run that stopped mid
        // escape sequence would feed the parser a spliced one.
        let mut seed = self.pane_title_seed(pane_id);
        seed.extend(history_seed_bytes(output.stdout));
        Ok(seed)
    }

    /// The pane's window title replayed as an OSC 2, or empty when the pane
    /// has none worth restoring.
    ///
    /// Agents use the window title as their activity line — Claude Code writes
    /// the task it is on — and thurbox reads it off the PTY, so a restart that
    /// joins the stream mid-flight shows nothing until the agent next repaints
    /// it. tmux kept the value: `#{pane_title}` *is* the last OSC the pane
    /// emitted. Replaying it puts it back through the same callback a live
    /// title takes (`TermSignals`'s title callback), so nothing downstream
    /// learns a second way of being told.
    ///
    /// Best-effort by construction: a pane title is a nicety and the history
    /// beside it is not, so a mux that answers this differently (not every
    /// dialect is verified here) loses the line rather than the scrollback.
    fn pane_title_seed(&self, pane_id: &str) -> Vec<u8> {
        // One query for both halves: a pane that never had a title set reads
        // back as the host's own short name, which is tmux's default rather
        // than anything an agent said.
        let out = match self.run_tmux(&[
            "display-message",
            "-p",
            "-t",
            pane_id,
            "#{host_short}|#{pane_title}",
        ]) {
            Ok(out) => out,
            Err(e) => {
                debug!(pane = %pane_id, "could not read pane title: {e:#}");
                return Vec::new();
            }
        };
        let line = String::from_utf8_lossy(&out.stdout);
        let Some((host, title)) = line.lines().next().and_then(|l| l.split_once('|')) else {
            return Vec::new();
        };
        title_seed_bytes(host, title)
    }

    /// Resize a pane, forcing a SIGWINCH even if dimensions haven't changed.
    fn force_resize(&self, pane_id: &str, rows: u16, cols: u16) -> Result<()> {
        // Briefly resize to different dimensions to guarantee a SIGWINCH,
        // then resize to the actual target. This causes TUI apps to repaint.
        if rows > 1 {
            self.resize(pane_id, rows - 1, cols)?;
        } else {
            self.resize(pane_id, rows + 1, cols)?;
        }
        self.resize(pane_id, rows, cols)?;
        Ok(())
    }
}

impl<T: MuxDialect> MuxBackend<T> {
    pub(crate) fn headless_owned_panes_in(
        &self,
        windows: &[DiscoveredSession],
        session_id: &str,
        session_name: &str,
    ) -> Vec<String> {
        let index = WindowIndex::from_listing(windows.iter().cloned());
        [
            index.agent_window(session_id, session_name),
            index.shell_window(session_id, session_name),
        ]
        .into_iter()
        .filter_map(Located::pane)
        .collect()
    }
    pub(crate) fn headless_pane_pid(
        &self,
        backend_id: &str,
        session_id: &str,
        name: &str,
    ) -> Result<Option<u32>> {
        if self.host.is_none() {
            self.window_pane_pid(session_id, name)
        } else {
            self.pane_pid(backend_id)
        }
    }
    pub(crate) fn headless_discover(&self) -> Result<Vec<DiscoveredSession>> {
        if let Some(host) = self.host.as_ref() {
            known_host_socket(host)?;
        }
        self.discover_answered()
    }
    pub(crate) fn spawn_headless(
        &self,
        session_id: &str,
        window_name: &str,
        command: &str,
        args: &[String],
        cwd: Option<&Path>,
        env: &HashMap<String, String>,
    ) -> Result<String> {
        match self.host.as_ref() {
            Some(_) => {
                self.check_available()?;
                self.ensure_ready()?;
                let spawned = self.spawn(
                    &agent_window_name(window_name),
                    command,
                    args,
                    cwd,
                    env,
                    24,
                    80,
                )?;
                let pane_id = spawned.backend_id;
                let stamp = self.stamp_window(&pane_id, session_id, WindowRole::Agent);
                complete_remote_headless_spawn(pane_id, stamp, window_name)
            }
            None => self.spawn_window(session_id, window_name, command, args, cwd, env),
        }
    }

    pub(crate) fn headless_liveness(
        &self,
        session_id: &str,
        session_name: &str,
    ) -> Result<crate::agent::backend::BackendLiveness> {
        let index = match self.host.as_ref() {
            Some(host) => {
                known_host_socket(host)?;
                WindowIndex::from_listing(self.discover_answered()?)
            }
            None => self.window_index()?,
        };
        // The old headless path treated a retained dead pane as needing a
        // relaunch; retain that behavior while unknown and unreachable remain
        // refusals.
        Ok(match index.agent_liveness(session_id, session_name) {
            crate::agent::backend::BackendLiveness::Exited => {
                crate::agent::backend::BackendLiveness::Missing
            }
            other => other,
        })
    }

    pub(crate) fn headless_live_pane(
        &self,
        session_id: &str,
        session_name: &str,
    ) -> Result<Option<String>> {
        let index = WindowIndex::from_listing(self.headless_discover()?);
        match index.live_agent_window(session_id, session_name) {
            Located::Unknown => bail!("ambiguous agent window"),
            located => Ok(located.pane()),
        }
    }

    pub(crate) fn kill_headless(
        &self,
        session_id: &str,
        session_name: &str,
        agent_pane: &str,
        shell_pane: &str,
    ) -> Result<bool> {
        match self.host.as_ref() {
            Some(host) => {
                known_host_socket(host)?;
                let index = WindowIndex::from_listing(self.discover_answered()?);
                let agent = kill_located(
                    self,
                    index.agent_window(session_id, session_name),
                    agent_pane,
                    session_name,
                    &host.name,
                )?;
                kill_located(
                    self,
                    index.shell_window(session_id, session_name),
                    shell_pane,
                    session_name,
                    &host.name,
                )?;
                Ok(agent)
            }
            None => {
                let index = self.window_index()?;
                let targets = [
                    index.agent_window(session_id, session_name).pane(),
                    index.shell_window(session_id, session_name).pane(),
                ];
                let mut killed = false;
                for target in targets.into_iter().flatten() {
                    self.kill_window_at(&target)?;
                    killed = true;
                }
                Ok(killed)
            }
        }
    }

    pub(crate) fn needs_liveness_poll(&self) -> bool {
        T::POLLS_LIVENESS
    }
    pub(crate) fn name(&self) -> &str {
        &self.name
    }

    pub(crate) fn check_available(&self) -> Result<()> {
        // `tmux -L <socket> -V` prints the version without connecting, and over
        // the SSH transport this verifies remote connectivity at the same time.
        let output = self
            .transport
            .mux_command(&self.socket(), &["-V"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .context("tmux is not installed or not in PATH")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            bail!("tmux -V failed: {}", stderr.trim());
        }

        let version_str = String::from_utf8_lossy(&output.stdout);
        T::check_banner(&version_str, &self.socket())?;
        debug!("multiplexer version: {}", version_str.trim());
        Ok(())
    }

    pub(crate) fn ensure_ready(&self) -> Result<()> {
        self.ensure_session_configured()?;

        // Start control mode if not already running.
        let mut guard = self
            .control
            .lock()
            .map_err(|e| anyhow::anyhow!("control lock: {e}"))?;
        if guard.is_none() {
            debug!("Starting tmux control mode");
            *guard = Some(ControlMode::start(
                &self.transport,
                &self.socket(),
                &self.session,
                &self.sizer,
            )?);
        }

        Ok(())
    }

    #[allow(clippy::too_many_arguments)] // Mirrors SessionBackend::spawn for both concrete backends.
    pub(crate) fn spawn(
        &self,
        window_name: &str,
        command: &str,
        args: &[String],
        cwd: Option<&Path>,
        env: &HashMap<String, String>,
        rows: u16,
        cols: u16,
    ) -> Result<SpawnedSession> {
        // Asked of the server, per spawn: it is the server's code that births
        // the pane, and one started before an upgrade keeps running the old.
        if T::ASKS_SERVER_VERSION {
            let version = self.ctrl_command("display-message -p '#{version}'")?;
            T::check_server_version(&version, &self.socket())?;
        }
        let shell_cmd = T::window_command(self, window_name, command, args, env);
        let quote_arg = T::quote_arg;
        let cwd_part = match cwd {
            Some(dir) => format!(" -c {}", quote_arg(&dir.to_string_lossy())),
            None => String::new(),
        };
        let env_part = T::env_args(env);
        let escaped_window_name = quote_arg(window_name);
        let session = &self.session;
        // The window's own options ride along in the same command list — see
        // `birth_options` for why neither can be a message of its own.
        let new_window = format!(
            "new-window -t {session} -n {escaped_window_name} -P -F '{SPAWN_FORMAT}'{cwd_part}{env_part} {shell_cmd}"
        );
        let options = T::birth_option_commands(window_name);
        let cmds: Vec<&str> = std::iter::once(new_window.as_str())
            .chain(options.iter().map(String::as_str))
            .collect();
        let result = self.ctrl_command_list(&cmds)?;
        // Two fields, and the second is optional in practice: a multiplexer
        // that prints only the pane id (not every dialect's `-P -F` is verified
        // against the documented divergences, ADR-13) leaves `window_id` None
        // and `register_pane` falls back to asking, exactly as before.
        let answer = result.trim();
        let (pane_id, window_id) = match answer.split_once(char::is_whitespace) {
            Some((pane, window)) => (pane.trim().to_string(), Some(window.trim().to_string())),
            None => (answer.to_string(), None),
        };
        if !control_mode::is_valid_pane_id(&pane_id) {
            bail!("tmux new-window returned an invalid pane id: {pane_id:?}");
        }
        // Dropped rather than fatal: a window id that is not one costs this pane
        // the ability to notice its own ending, which is what the pre-#1107
        // behaviour was — not a reason to refuse a window that started fine.
        let window_id = window_id.filter(|id| {
            let ok = control_mode::is_valid_window_id(id);
            if !ok {
                debug!("tmux new-window answered with an unusable window id: {id:?}");
            }
            ok
        });

        debug!(pane_id = %pane_id, window_id = ?window_id, "tmux window created via control mode");

        let connected = self.connect_pane(&pane_id, window_id.as_deref(), rows, cols)?;

        Ok(SpawnedSession {
            backend_id: pane_id,
            output: connected.output,
            input: connected.input,
            size: connected.size,
        })
    }

    pub(crate) fn adopt(
        &self,
        backend_id: &str,
        rows: u16,
        cols: u16,
        seed: Option<Vec<u8>>,
    ) -> Result<AdoptedSession> {
        // backend_id comes from the shared DB — never interpolate it unvalidated.
        if !control_mode::is_valid_pane_id(backend_id) {
            bail!("refusing to adopt invalid pane id: {backend_id:?}");
        }
        // Opt-in split timing (THURBOX_PERF_LOG): the history capture is an
        // independent `tmux capture-pane` subprocess, while `connect_pane`
        // drives the serialized control-mode connection. Restore prefetches
        // the captures in parallel and passes them in (ADR-P9), so
        // `capture_ms` here reads 0 on that path; a `None` seed (a mid-run
        // adopt) still captures inline, before connecting so seeded history
        // can't duplicate live output. Best-effort: adoption must survive a
        // failed capture.
        let perf_log = std::env::var_os("THURBOX_PERF_LOG").is_some();

        let capture_start = perf_log.then(std::time::Instant::now);
        let seed = seed.unwrap_or_else(|| {
            self.capture_history(backend_id).unwrap_or_else(|e| {
                warn!("Failed to capture history for pane {backend_id}: {e}");
                Vec::new()
            })
        });
        let capture_ms = capture_start.map(|s| s.elapsed().as_millis() as u64);

        let connect_start = perf_log.then(std::time::Instant::now);
        // No window id to hand over: `adopt` is given a pane id out of the
        // database, so `register_pane` asks for the window itself.
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
        // Prepend the captured history to the live stream — the reader loop
        // feeds it into the parser first, populating the UI scrollback. It
        // must not be mistaken for live activity either, which is what
        // `seed_len` tells the reader loop to guard against (see
        // `Session::reader_loop`).
        let seed_len = seed.len();
        Ok(AdoptedSession {
            output: Box::new(Cursor::new(seed).chain(connected.output)),
            input: connected.input,
            seed_len,
            size: connected.size,
        })
    }

    pub(crate) fn capture_history(&self, backend_id: &str) -> Result<Vec<u8>> {
        if !control_mode::is_valid_pane_id(backend_id) {
            bail!("refusing to capture invalid pane id: {backend_id:?}");
        }
        self.capture_history_seed(backend_id)
    }

    pub(crate) fn title_seed(&self, backend_id: &str) -> Vec<u8> {
        if !control_mode::is_valid_pane_id(backend_id) {
            return Vec::new();
        }
        self.pane_title_seed(backend_id)
    }

    pub(crate) fn request_snapshot(&self, backend_id: &str) -> Result<()> {
        if !control_mode::is_valid_pane_id(backend_id) {
            bail!("refusing to snapshot invalid pane id: {backend_id:?}");
        }
        // Asked on the loop, so it waits for the lock no longer than any other
        // loop command, and not at all for the answer.
        self.with_control_until(std::time::Instant::now() + LOOP_COMMAND_BUDGET, |ctrl| {
            ctrl.request_snapshot(backend_id, snapshot_history())
        })
    }

    pub(crate) fn snapshot(&self, backend_id: &str) -> Result<control_mode::PaneSnapshot> {
        if !control_mode::is_valid_pane_id(backend_id) {
            bail!("refusing to snapshot invalid pane id: {backend_id:?}");
        }
        // Asked under the control lock, which keeps the answer's place in the
        // queue, and waited for outside it: a search reads many panes at once.
        self.with_control(|ctrl| ctrl.ask_snapshot(backend_id, snapshot_history()))?
            .wait()
    }

    pub(crate) fn set_pane_retention(&self, backend_id: &str, keep: bool) -> Result<()> {
        // The guard every neighbour carries, for the reason a target makes it
        // worth carrying: tmux resolves `-t` as a window *name* as readily as
        // an id, so a caller passing anything else would quietly set
        // `remain-on-exit` on whatever window that name picked out.
        if !control_mode::is_valid_pane_id(backend_id) {
            bail!("refusing to set remain-on-exit on invalid pane id: {backend_id:?}");
        }
        let keep = if keep { "on" } else { "off" };
        self.tmux_run(&[
            "set-window-option",
            "-t",
            backend_id,
            "remain-on-exit",
            keep,
        ])
    }

    pub(crate) fn window_panes(&self, window_name: &str) -> Result<Vec<(String, bool)>> {
        // A name lookup rather than an identity one: a program window carries no
        // session id to resolve, only its deterministic name. Matched exactly
        // rather than by prefix — tmux's own name matching is FNMATCH-ish, which
        // would make `tbp-x-watch` findable by `tbp-x-watc`.
        let listing = self.tmux_output(&[
            "list-windows",
            "-t",
            &self.session,
            "-F",
            "#{pane_id}|#{window_name}|#{pane_dead}",
        ])?;
        let mut found = Vec::new();
        for line in listing.lines() {
            let parts: Vec<&str> = line.splitn(3, '|').collect();
            if parts.len() < 3 || parts[1] != window_name {
                continue;
            }
            // An unparseable id is dropped rather than reported dead: the caller
            // would try to kill it, and a target tmux cannot resolve is not a
            // corpse, it is noise.
            if !control_mode::is_valid_pane_id(parts[0]) {
                continue;
            }
            found.push((parts[0].to_string(), parse_pane_dead(parts[2])));
        }
        Ok(found)
    }

    pub(crate) fn discover(&self) -> Result<Vec<DiscoveredSession>> {
        if !self.session_exists() {
            return Ok(Vec::new());
        }

        // Once control mode is up, route through `ctrl_command` so a dead
        // connection is transparently reconnected + retried (like every other
        // control-mode call) instead of failing the discovery. Before control
        // mode has started, fall back to a one-shot direct tmux command.
        let control_started = {
            let guard = self
                .control
                .lock()
                .map_err(|e| anyhow::anyhow!("control lock: {e}"))?;
            guard.is_some()
        };
        let result = if control_started {
            self.ctrl_command(&format!(
                "list-windows -t {} -F '{DISCOVER_FORMAT}'",
                self.session
            ))?
        } else {
            self.tmux_output(&["list-windows", "-t", &self.session, "-F", DISCOVER_FORMAT])?
        };

        let stamps = self.stamps_are_per_window();
        Ok(result
            .lines()
            .filter_map(|line| parse_discovered(line, stamps))
            .collect())
    }

    pub(crate) fn stamp_window(
        &self,
        backend_id: &str,
        session_id: &str,
        role: WindowRole,
    ) -> Result<()> {
        if !control_mode::is_valid_pane_id(backend_id) {
            bail!("refusing to stamp an invalid pane id: {backend_id:?}");
        }
        // Nothing to write where an option is not a window's
        // ([`Self::stamps_are_per_window`]): the multiplexer would take this as
        // a *global* one and hand it back as every window's identity. Ok rather
        // than an error for the same reason `set_remain_on_exit` is — the
        // caller is not being refused, there is simply no per-window option to
        // set, and `WindowIndex` resolves by name there (ADR-25).
        if !self.stamps_are_per_window() {
            return Ok(());
        }
        // `-w`: the option belongs to the window, not the pane, so a pane that
        // is split or replaced inside it does not take the identity with it.
        if !session_id.is_empty() {
            self.ctrl_command(&format!(
                "set-option -w -t {backend_id} {WINDOW_SESSION_OPTION} {}",
                shell_escape(session_id)
            ))?;
        }
        self.ctrl_command(&format!(
            "set-option -w -t {backend_id} {WINDOW_ROLE_OPTION} {}",
            role.as_str()
        ))?;
        // As in `stamp_oneshot`, and for the same reason: this is the
        // other half of the paths that write a stamp (the interface's own
        // spawn, and an adopt-by-name). The sweep is a local one-shot, so a
        // host's server is left to the teardown that can reach it.
        if !self.transport.is_remote() {
            let _ = self.retire_duplicate_windows(session_id, role);
        }
        Ok(())
    }

    pub(crate) fn resize(&self, backend_id: &str, rows: u16, cols: u16) -> Result<()> {
        // Sent, not asked. The caller is the render thread matching the pane to
        // the rect it is painting into, and the answer is of no use to the
        // frame: a resize tells the *agent* how to wrap, which is a message to
        // the host. Waiting for the confirmation put a control-mode round trip
        // inside the paint, and on a link that had gone bad that was the whole
        // interface frozen until the command timed out.
        //
        // Still two resizes and still in this order — a pane cannot exceed its
        // window — but as one list, so they take the lock once and tmux runs
        // them without returning to its event loop in between. Sent separately
        // they could be refused separately, and a window resized around a pane
        // that was not leaves the agent wrapping at the old width until some
        // later rect change asks again.
        let (cmds, blocks) = T::resize_commands(backend_id, rows, cols, &self.sizer);
        let cmds: Vec<&str> = cmds.iter().map(String::as_str).collect();
        self.ctrl_command_detached(&cmds, blocks)
    }

    pub(crate) fn claim_size(&self, backend_id: &str, rows: u16, cols: u16) -> Result<()> {
        // Unconditional: this is the instance being typed into, which is what
        // decides who sizes (see `resize`).
        let (rows, cols) = tmux_size(rows, cols);
        self.ctrl_command_detached(
            &[
                &format!(
                    "set-option -w -t {backend_id} {SIZER_OPTION} {}",
                    self.sizer
                ),
                &format!("resize-window -t {backend_id} -x {cols} -y {rows}"),
                &format!("resize-pane -t {backend_id} -x {cols} -y {rows}"),
            ],
            3,
        )
    }

    pub(crate) fn is_dead(&self, backend_id: &str) -> Result<bool> {
        // Bounded, because the only caller is the interface's own loop deciding
        // where a chord goes (`coordinator::input`'s passthrough gate). The
        // unbounded ask ran out `COMMAND_TIMEOUT`, reconnected and ran out
        // again on a link that had stopped carrying anything — twenty-odd
        // seconds of an interface that answered nothing, to settle a question
        // whose honest answer when the host says nothing is the one the caller
        // already reads an error as: not known to be dead.
        let result = self.ctrl_command_within(
            &format!("display-message -t {backend_id} -p '#{{pane_dead}}'"),
            LOOP_COMMAND_BUDGET,
        )?;
        Ok(result.trim() == "1")
    }

    pub(crate) fn kill(&self, backend_id: &str) -> Result<()> {
        let _ = self.unregister_pane(backend_id);
        self.ctrl_command(&format!("kill-pane -t {backend_id}"))?;
        Ok(())
    }

    pub(crate) fn detach(&self, backend_id: &str) -> Result<()> {
        // Disable output monitoring for this pane.
        if let Err(e) = self.ctrl_command_nowait(&format!(
            "refresh-client -A '{}:off'",
            backend_id.replace('\'', "'\\''")
        )) {
            warn!("Failed to disable output monitoring during detach: {e}");
        }
        // Remove the pane sender — the ControlModeReader gets EOF.
        let _ = self.unregister_pane(backend_id);
        Ok(())
    }

    pub(crate) fn pane_pid(&self, backend_id: &str) -> Result<Option<u32>> {
        let result = self.ctrl_command(&format!(
            "display-message -t {backend_id} -p '#{{pane_pid}}'"
        ))?;
        Ok(result.trim().parse().ok())
    }

    pub(crate) fn pane_ids(&self) -> Result<std::collections::HashSet<String>> {
        let result = self.ctrl_command("list-panes -a -F '#{pane_id}'")?;
        Ok(control_mode::parse_pane_ids(&result))
    }

    pub(crate) fn pane_pids(&self) -> Result<HashMap<String, u32>> {
        let result = self.ctrl_command("list-panes -a -F '#{pane_id} #{pane_pid}'")?;
        Ok(control_mode::parse_pane_pids(&result))
    }

    pub(crate) fn shutdown(&self) {
        // Taking the connection out runs `ControlMode::drop` on the calling
        // thread, which is what lets quit fan the (blocking) teardown out
        // across backends. Idempotent: the mutex holds `None` afterwards, so
        // `TmuxBackend`'s own drop later is a no-op.
        //
        // `lock()` rather than `try_lock()`: a contended lock means another
        // thread is mid-command on this connection, and skipping the teardown
        // would leak the child + reader thread for the process lifetime.
        drop(self.control.lock().ok().and_then(|mut c| c.take()));
    }

    pub(crate) fn take_hook_state_events(&self) -> Vec<(String, String)> {
        // `try_lock`, not `lock`: this runs on the UI thread every tick, and a
        // background restore thread holds `control` across `ControlMode::start`
        // (an ssh connect + waited commands, up to tens of seconds on a slow
        // host) — blocking here would stall the first frame ADR-P7 protects.
        // A contended lock means no connection is serving events yet, and a
        // skipped drain only defers queued events to the next tick.
        self.control
            .try_lock()
            .ok()
            .and_then(|guard| guard.as_ref().map(ControlMode::take_sub_events))
            .unwrap_or_default()
    }

    /// The shell-pane command must match the **host's** OS, not the local
    /// binary's — the trait default reads the local `$SHELL`/`%COMSPEC%`,
    /// which shipped e.g. `/bin/zsh` to a remote Windows pane
    /// ("CommandNotFoundException"). Remote hosts get the dialect's
    /// [`MuxDialect::REMOTE_SHELL`], one that exists there by construction.
    /// Local backends keep the trait default's behavior.
    pub(crate) fn default_shell(&self) -> String {
        if !self.transport.is_remote() {
            #[cfg(windows)]
            {
                return std::env::var("COMSPEC").unwrap_or_else(|_| "cmd.exe".to_string());
            }
            #[cfg(not(windows))]
            {
                return std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string());
            }
        }
        T::REMOTE_SHELL.to_string()
    }
}

fn complete_remote_headless_spawn(
    pane_id: String,
    stamp: Result<()>,
    window_name: &str,
) -> Result<String> {
    if let Err(e) = stamp {
        warn!("could not stamp the remote window for '{window_name}': {e:#}");
    }
    Ok(pane_id)
}

/// Wrap `text` in the bracketed-paste escape sequences (`ESC[200~ … ESC[201~`)
/// so a multi-line prompt is delivered as a single paste — the embedded
/// newlines insert as text instead of submitting the prompt on the first one.
/// Used by [`MuxBackend::send_text_now`], which is how the TUI reaches this too — the
/// kernel's prompt commands call it rather than framing the paste themselves.
/// The trailing `Enter` is still sent separately by the caller. tmux delivers
/// these bytes literally via `send-keys -l`.
pub(crate) fn bracketed_paste(text: &str) -> String {
    format!("\x1b[200~{text}\x1b[201~")
}

/// Whether a `#{pane_dead}` format string reports an exited pane.
///
/// Only the literal `1` means dead: `display-message` against a *missing*
/// window still exits 0 printing nothing, so an empty value must read as "not
/// dead" and leave the missing-window diagnosis to `send-keys`, which does
/// fail on it.
fn parse_pane_dead(output: &str) -> bool {
    output.trim() == "1"
}

/// How a failed one-shot multiplexer command reads inside an error.
///
/// `output()` rather than `status()` at every call site above is the point: a
/// `status()` child inherits this process's stderr, so tmux's own `can't find
/// window: tb-<name>` lands there directly — a second, unstructured stream
/// beside the error document the CLI puts on stdout (AXI principle 6, "an
/// agent reads one stream"). Captured, the same sentence becomes part of the
/// one answer. tmux says nothing at all for some failures, hence the fallback
/// to the bare status.
fn mux_failure(out: &std::process::Output) -> String {
    let stderr = String::from_utf8_lossy(&out.stderr);
    let detail = stderr.trim();
    if detail.is_empty() {
        return format!("exited with status {}", out.status);
    }
    detail.to_string()
}

/// The special keys [`SessionBackend::send_key`](crate::agent::backend::SessionBackend::send_key) can deliver, as
/// `(canonical spelling, tmux key name)`.
///
/// A closed table because tmux does **not** validate a key name: an
/// unrecognized one is typed into the pane as literal text, so a typo would
/// silently inject `Escpe` into an agent's prompt rather than fail. `ctrl-a` …
/// `ctrl-z` are resolved generically by [`resolve_key`] and deliberately not
/// listed here.
///
/// Not every dialect implements every name: psmux's key encoding covers
/// `enter`, `escape`, `tab`, `backspace` and `ctrl-<letter>`, which is what a
/// Windows host runs into.
pub const NAMED_KEYS: &[(&str, &str)] = &[
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
    // `DC` is tmux's own (terminfo-derived) name for Delete. Current tmux also
    // answers to `Delete` — both send `\x1b[3~` — but an older one that did not
    // would type the *name* into the pane rather than refuse it, so the table
    // names the conservative one.
    ("delete", "DC"),
];

/// Alternate spellings accepted for a canonical name in [`NAMED_KEYS`].
///
/// Forgiving on purpose — an integrator writing `esc` or `pgup` should not have
/// to look the table up — but every alias resolves to one canonical name, which
/// is what the CLI echoes back, so there is a single spelling to depend on.
const KEY_ALIASES: &[(&str, &str)] = &[
    ("return", "enter"),
    ("esc", "escape"),
    ("bspace", "backspace"),
    ("pageup", "page-up"),
    ("pgup", "page-up"),
    ("pagedown", "page-down"),
    ("pgdn", "page-down"),
    ("del", "delete"),
];

/// A key name resolved from what a caller spelled.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedKey {
    /// The canonical thurbox spelling (`ctrl-c`, `page-up`).
    pub name: String,
    /// The tmux key name to hand `send-keys` (`C-c`, `PageUp`).
    pub tmux: String,
}

/// Resolve a caller's key spelling, or `None` for one thurbox does not know.
///
/// Case-insensitive, and `ctrl-c`, `ctrl+c`, `C-c` and `c+c` are all the same
/// key — the separator and the `ctrl`/`c` prefix are the two things people
/// actually spell differently, and half-supporting them would mean a typo lands
/// as text in an agent's prompt.
pub fn resolve_key(input: &str) -> Option<ResolvedKey> {
    let lower = input.trim().to_ascii_lowercase();
    let lower = KEY_ALIASES
        .iter()
        .find(|(alias, _)| *alias == lower)
        .map_or(lower.as_str(), |(_, canonical)| *canonical);
    if let Some((name, tmux)) = NAMED_KEYS.iter().find(|(name, _)| *name == lower) {
        return Some(ResolvedKey {
            name: (*name).to_string(),
            tmux: (*tmux).to_string(),
        });
    }
    let rest = ["ctrl-", "ctrl+", "c-", "c+"]
        .iter()
        .find_map(|prefix| lower.strip_prefix(prefix))?;
    let mut chars = rest.chars();
    let letter = chars.next().filter(char::is_ascii_lowercase)?;
    if chars.next().is_some() {
        return None;
    }
    Some(ResolvedKey {
        name: format!("ctrl-{letter}"),
        tmux: format!("C-{letter}"),
    })
}

/// Window name for the headless automation heartbeat keeper. Deliberately NOT
/// `tb-` prefixed so [`MuxBackend::discover`] ignores it — it is
/// infrastructure, not a session.
const HEARTBEAT_WINDOW: &str = "automation-heartbeat";

/// How often the heartbeat keeper invokes `automation tick`.
pub(crate) const HEARTBEAT_INTERVAL_SECS: u64 = 60;

/// List the window names in the thurbox tmux session (empty if the server is
/// not running).
fn list_window_names() -> Vec<String> {
    let Ok(out) =
        local_mux_command(&["list-windows", "-t", TMUX_SESSION, "-F", "#{window_name}"]).output()
    else {
        return Vec::new();
    };
    if !out.status.success() {
        return Vec::new();
    }
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::to_string)
        .collect()
}

/// Ensure the automation heartbeat keeper window is running.
///
/// Creates a detached tmux window that loops `<cli_path> automation tick` every
/// `HEARTBEAT_INTERVAL_SECS` seconds, so automations fire even with no TUI
/// attached. The live window also keeps the tmux server alive, so spawn-only
/// automations work with no other sessions. Idempotent — a no-op when the
/// keeper already exists. `cli_path` is the absolute path to `thurbox-cli`.
///
/// Whether the automation heartbeat keeper window is running right now.
///
/// The keeper is created implicitly by anything that arms an automation, is not
/// a session, and so appears in no session listing. That made it the one thing
/// thurbox puts on a tmux server that nothing could see or reclaim; this and
/// [`stop_automation_heartbeat`] are what make it accountable.
pub fn automation_heartbeat_running() -> bool {
    list_window_names().iter().any(|w| w == HEARTBEAT_WINDOW)
}

/// Stop the heartbeat keeper. Returns whether there was one to stop.
///
/// Automations stop firing headlessly until something arms it again — which any
/// `automation` write does, so this is a pause rather than a removal.
pub fn stop_automation_heartbeat() -> bool {
    if !automation_heartbeat_running() {
        return false;
    }
    let target = format!("{TMUX_SESSION}:{HEARTBEAT_WINDOW}");
    local_mux_command(&["kill-window", "-t", &target])
        .output()
        .map(|out| out.status.success())
        .unwrap_or(false)
}

pub fn ensure_automation_heartbeat(cli_path: &Path) -> Result<()> {
    MuxBackend::<LocalMuxTransport>::local().ensure_session_configured()?;
    if list_window_names().iter().any(|w| w == HEARTBEAT_WINDOW) {
        return Ok(());
    }
    let loop_cmd = LocalMuxTransport::heartbeat_loop_command(cli_path);
    let out = local_mux_command(&[
        "new-window",
        "-d",
        "-t",
        TMUX_SESSION,
        "-n",
        HEARTBEAT_WINDOW,
        &loop_cmd,
    ])
    .output()
    .context("Failed to create automation heartbeat window")?;
    // Asked again rather than believed: the status may be a failing user hook's
    // and not this window's — see the same read in `spawn_window`. There is no
    // `-P` answer to trust here, so the listing is what says whether the window
    // exists.
    if !out.status.success() && !automation_heartbeat_running() {
        bail!("tmux new-window (heartbeat) {}", mux_failure(&out));
    }
    debug!("Armed automation heartbeat keeper window");
    Ok(())
}

/// Resolve the path to the `thurbox-cli` binary that sits next to the currently
/// running executable (TUI or CLI), falling back to a bare `thurbox-cli` on
/// `PATH` when resolution fails.
///
/// The platform executable suffix (`.exe` on Windows, empty elsewhere) is
/// applied via [`std::env::consts::EXE_SUFFIX`], so the self/sibling match works
/// for `thurbox-cli.exe` too.
pub fn resolve_cli_binary() -> std::path::PathBuf {
    let cli_name = format!("thurbox-cli{}", std::env::consts::EXE_SUFFIX);
    if let Ok(exe) = std::env::current_exe() {
        if exe.file_name().and_then(std::ffi::OsStr::to_str) == Some(cli_name.as_str()) {
            return exe;
        }
        if let Some(dir) = exe.parent() {
            let sibling = dir.join(&cli_name);
            if sibling.exists() {
                return sibling;
            }
        }
    }
    std::path::PathBuf::from(cli_name)
}

/// The OSC 2 that restores `title` as a pane's window title, or empty when
/// there is nothing to restore.
///
/// Suppressed for a title equal to `host_short`, which is what tmux seeds a
/// pane with and therefore means "no agent ever set one" — replaying it would
/// put a hostname in the session list where the activity line goes. Control
/// characters are dropped because the value is remote-controlled text and the
/// sequence is terminated by one.
fn title_seed_bytes(host_short: &str, title: &str) -> Vec<u8> {
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

/// Separator for the one-shot `display-message` that reads a pane's whole
/// state. ASCII unit separator: paths and command names may contain spaces,
/// tabs and newlines, so a whitespace delimiter would split a value in half.
///
/// Keeping it intact costs a flag — see [`PANE_STATE_UTF8_FLAG`] — and an
/// alternate spelling — see [`PANE_STATE_SEP_ESCAPED`].
const PANE_STATE_SEP: char = '\x1f';

/// How tmux 3.4 and older spell [`PANE_STATE_SEP`] back.
///
/// Those versions run every `display-message -p` answer through `vis(3)`
/// (`VIS_OCTAL|VIS_CSTYLE|VIS_NOSLASH`) *before* the UTF-8 check, so a control
/// byte comes back as its printable octal escape whatever
/// [`PANE_STATE_UTF8_FLAG`] says: the separator arrives as the four characters
/// `\037` and the whole answer then parses as one field, reporting every pane
/// field null. tmux 3.5 dropped that pass and prints the byte itself. Both
/// spellings are accepted so one parser covers every tmux in the field —
/// ubuntu-24.04, which CI runs on, still ships 3.4.
const PANE_STATE_SEP_ESCAPED: &str = "\\037";

/// One `display-message` answer with its trailing newline gone and the
/// separator in whichever spelling this tmux used reduced to the raw byte.
///
/// Shared by every reader of a separated answer: a parser that knew only one
/// spelling would see the whole line as a single field on the other, which
/// reads as "tmux told us nothing" rather than as a parse it got wrong.
fn normalized_pane_answer(raw: &str) -> Cow<'_, str> {
    let trimmed = raw.trim_end_matches(['\n', '\r']);
    if trimmed.contains(PANE_STATE_SEP_ESCAPED) {
        Cow::Owned(trimmed.replace(PANE_STATE_SEP_ESCAPED, &PANE_STATE_SEP.to_string()))
    } else {
        Cow::Borrowed(trimmed)
    }
}

/// tmux's `-u` — "assume the terminal supports UTF-8".
///
/// tmux decides a client speaks UTF-8 from `LC_ALL`/`LC_CTYPE`/`LANG`, and
/// sanitizes what it prints for one that does not: every control byte becomes
/// `_`, [`PANE_STATE_SEP`] included. Under `LC_ALL=C` or no locale at all — a
/// systemd unit, a cron job, most containers — the whole answer then parses as
/// one field and every pane-state field reports null. `-u` sets the flag
/// outright, so the separator survives whatever the environment says. Passed
/// only where the dialect sanitizes ([`MuxDialect::UTF8_FLAG`]).
const PANE_STATE_UTF8_FLAG: &str = "-u";

/// The `PATH` out of an `env PATH=… <program> …` window command, or `None` when
/// the command does not open with one.
///
/// Anchored at the second token rather than searched for, because that is
/// where the prefix puts it (and nothing else writes this shape): a `PATH=`
/// appearing anywhere else is an argument of the agent's own, and reading it
/// as the pane's environment would be a confident wrong answer.
///
/// **A command session's whole window command is one token, and tmux hands it
/// back quoted** — `"…/env PATH=… sh -c 'sleep 300'"`. That still parses, and
/// not by luck this has to be careful about: the opening quote belongs to the
/// *first* token, which is the program, and this reads the second. The closing
/// one is on the last token, which it never looks at.
///
/// tmux also quotes an individual token holding whitespace, so a `PATH` with a
/// space in a component fails this and reads as unknown. Both failure
/// directions are the safe one — a `PATH` this cannot read is reported as
/// unverifiable, never as a working one.
fn path_from_prefix(start_command: &str) -> Option<String> {
    let mut tokens = start_command.split_ascii_whitespace();
    tokens.next()?;
    tokens.next()?.strip_prefix("PATH=").map(str::to_owned)
}

/// Split one `display-message` answer into a [`PaneState`], the pane's tty, and
/// the name of the window it actually came from.
///
/// An empty field is `None`, not an empty string: tmux prints nothing for a
/// format it cannot expand, and "" would read downstream as a real answer.
fn parse_pane_state(raw: &str) -> (PaneState, Option<String>, Option<String>) {
    let line = normalized_pane_answer(raw);
    let mut fields = line.split(PANE_STATE_SEP);
    let mut next = || fields.next().filter(|f| !f.is_empty());

    let cursor_row = next().and_then(|f| f.parse().ok());
    let cursor_col = next().and_then(|f| f.parse().ok());
    let command = next().map(str::to_string);
    let cwd = next().map(str::to_string);
    let tty = next().map(str::to_string);
    // `1`/`0`; anything else (a multiplexer that does not know the format) is
    // an absent answer, not a live pane.
    let dead = next().and_then(|f| match f {
        "1" => Some(true),
        "0" => Some(false),
        _ => None,
    });

    let window = next().map(str::to_string);

    (
        PaneState {
            cursor_row,
            cursor_col,
            foreground_process: command,
            foreground_command: None,
            foreground_cwd: cwd,
            dead,
        },
        tty,
        window,
    )
}

/// The `(argv0, full command line)` of `tty`'s foreground process group.
///
/// One `ps` listing every process on the tty: each row carries the tty's
/// foreground process group id (`tpgid`), so the rows whose own `pgid` equals
/// it *are* the foreground job, and the group leader is its command. Asking
/// `ps` for `tpgid` directly (rather than opening the tty and calling
/// `tcgetpgrp`) keeps this to a subprocess that works the same on Linux and
/// macOS, and leaves the tty untouched.
fn foreground_process_on_tty(tty: &str) -> Option<(String, String)> {
    // Both procps and BSD `ps` take the bare name; the `/dev/` prefix tmux
    // reports is accepted by neither uniformly.
    let name = tty.strip_prefix("/dev/").unwrap_or(tty);
    let out = Command::new("ps")
        .args(["-o", "pid=,pgid=,tpgid=,args=", "-t", name])
        .output()
        .ok()
        .filter(|out| out.status.success())?;
    parse_ps_foreground(&String::from_utf8_lossy(&out.stdout))
}

/// Pick the foreground job out of `ps -o pid=,pgid=,tpgid=,args= -t <tty>`.
///
/// The group *leader* (`pid == pgid`) is preferred over the rest of its
/// pipeline, so a `node … | tee` reports the node. A `tpgid` of `-1` means no
/// foreground group (nothing has the tty), and `0` is `ps` reporting it does
/// not know — neither is a process, so both yield nothing.
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

/// One `ps` row: three numeric columns then the command line.
///
/// Split by hand rather than with `splitn`, because `ps` right-aligns its
/// numeric columns — a narrow pid beside a wide one is padded with *several*
/// spaces, which `splitn` hands back as empty fields.
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

/// Convert raw `capture-pane -p` output into vt100 parser input: drop the
/// unused blank bottom of the visible pane and turn bare `\n` line endings
/// into `\r\n` so each seeded line starts at column 0.
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

/// Session-level tmux options applied to the thurbox tmux session.
///
/// Single source of truth for both the TUI and headless paths — applied
/// (alongside the server-wide options + `default-command`) by
/// [`MuxBackend::apply_session_config`].
///
/// **Session options only.** `set-option -t <session> <key>` does not mean "for
/// this session" when `<key>` is a *window* option: tmux resolves the target
/// down to the session's CURRENT window and sets it there (measured, tmux
/// 3.2a — the option is on `@0` and a window created a moment later does not
/// have it). Since `apply_session_config` runs on every `ensure_ready`, which
/// window ends up carrying such an option is an accident of timing. Window
/// options therefore live in [`WINDOW_OPTS`] when every window should have them,
/// and in [`birth_options`] when a window has to be given them as it is created:
/// `remain-on-exit`, which depends on what the window is *for*, and
/// `window-size`, which no tmux in the supported range survives as a
/// server-wide default.
pub(crate) const SESSION_OPTS: &[(&str, &str)] = &[("status", "off"), ("history-limit", "5000")];

/// Window options applied to **every** window on thurbox's own tmux server.
///
/// Set with `-w -g` rather than per session: a window option has no session
/// scope to be set at (see [`SESSION_OPTS`]), and the alternative — setting it
/// on each window as it is born — would miss any window thurbox did not create.
/// The blast radius is thurbox's own socket, which holds nothing else.
///
/// `window-size` is **not** here, and must not be: made the server-wide default
/// it kills the server on every window creation from an unattached client (see
/// [`birth_options`], where it is said per window instead). Best-effort either
/// way — `resize-window -x/-y` already flips a window to `manual` when it
/// resizes it (measured, tmux 3.2a), and thurbox resizes every pane it paints.
pub(crate) const WINDOW_OPTS: &[(&str, &str)] = &[
    // The default a window is BORN with, so the one role that wants a corpse
    // asks for it (in the same command list as its creation — see
    // `birth_options`) and nothing else inherits one. Said here rather than
    // left to tmux's own default because the user's `~/.tmux.conf` is read on
    // thurbox's socket too, and `set -g remain-on-exit on` there would have
    // every window born keeping its corpse — a program pane whose death is
    // then never announced, since tmux reports a pane's death only by closing
    // its window.
    //
    // Which role loses a race is not a choice between the two: born `off`, an
    // agent window whose command exits instantly used to vanish before its
    // `on` arrived, taking the server with it when it was the last one. Neither
    // role waits on a round trip now.
    ("remain-on-exit", "off"),
];

/// The agent's command as an **absolute path**, resolved against thurbox's own
/// `PATH`, so the multiplexer never has to resolve it.
///
/// thurbox used to hand tmux a bare name (`claude`) and let tmux find it. Which
/// resolver ran, and with which `PATH`, was not thurbox's to choose:
///
/// - tmux copies the *client's* `PATH` into the new pane only for an
///   **unattached** client (`spawn.c`: "the session one is replaced from the
///   client … only unattached clients"). thurbox's control-mode client is
///   attached, so [`MuxBackend::spawn`] — a restart, a plugin program, the
///   shell pane — got the `PATH` of whatever first started the tmux **server**.
/// - tmux runs a window command given as a **single** argument through its
///   `default-shell` (`spawn.c`: `execl(shell, argv0, "-c", cmd)`), and only a
///   multi-argument one through `execvp`. So an agent with no args was launched
///   by a shell thurbox never chose, under that shell's quoting and `PATH`.
///
/// Both are why a fish user saw a spawn fail where a zsh user did not. zsh and
/// bash put their `PATH` additions in `~/.zshenv` / `~/.profile`, which any
/// shell that starts a tmux server sources, so the server's `PATH` and the
/// interactive one agree. fish's `fish_add_path` writes `fish_user_paths`, which
/// **only fish** applies — so a server started from anything else never sees
/// them, for the life of that server. And the exit status tells you which
/// resolver spoke: `execvp` failing makes the pane exit **1**, a shell that
/// cannot find the command exits **127**.
///
/// An absolute path is immune to both: `execvp` and every shell take it as-is.
///
/// Best-effort by design — the command is returned **unchanged** when it is
/// already a path, when nothing on `PATH` matches, or on Windows (whose
/// multiplexer runs its own command model, and a bare name there wants
/// `PATHEXT` semantics this deliberately does not have). A `command` that is a shell function, an alias,
/// or a binary installed *after* this resolves therefore behaves exactly as it
/// did before: resolution is an improvement where it succeeds, never a new way
/// to fail.
pub(crate) fn resolve_local_program(command: &str) -> String {
    if cfg!(windows) {
        return command.to_string();
    }
    match crate::paths::resolve_on_path(command) {
        Some(path) => path.to_string_lossy().into_owned(),
        None => command.to_string(),
    }
}

/// The pane id out of a one-shot `new-window -P -F '#{pane_id}'`'s stdout.
///
/// The **first** line, not the whole of it: anything a hook's `run-shell`
/// prints is appended after the `-P` answer on the same stream, so trimming the
/// lot yields an id with a shell's complaint stuck to it — which then fails
/// validation and loses a window that exists.
fn new_window_pane_id(stdout: &[u8]) -> String {
    String::from_utf8_lossy(stdout)
        .lines()
        .next()
        .unwrap_or_default()
        .trim()
        .to_string()
}

/// `env PATH=<…>` in front of a window's program, or nothing.
///
/// A pane runs with the `PATH` of the thurbox that spawned it — tmux replaces
/// the session environment's from an **unattached** client, which both local
/// spawn paths are. Usually that is the right answer and there is nothing to
/// do. It is not the right answer when the spawning thurbox is a `thurbox-cli`
/// invoked over ssh by a TUI delegating `session create` to this host
/// (ADR-24): sshd hands a non-interactive command its own `PATH`
/// (`/usr/local/bin:/usr/bin:/bin:/usr/games`), which has no `~/.local/bin` on
/// it — where `thurbox-cli` installs. The status hooks are a **bare** name
/// (`thurbox-cli session signal --state <s> || true`), so on such a host every
/// one of them resolved nothing and the `|| true` swallowed it: the host's own
/// rows never gained a `hook_state`, and every session on it read as
/// statusless on the TUI mirroring them.
///
/// So the CLI's own directory goes in front ([`resolve_cli_binary`] — the
/// process running knows where its sibling is even when `PATH` does not).
/// Prepended, never replaced.
///
/// **Why it rides in the command rather than in `-e`.** `PATH` is the one
/// variable tmux will not take that way: `new-window -e PATH=…` and
/// `set-environment -g PATH …` are both ignored, and the client's wins
/// (verified against tmux 3.5a — a sibling `-e FOO=bar` in the same command
/// arrives). `env` `exec`s, so it leaves no process behind, and the program it
/// is handed is already absolute ([`resolve_local_program`]) — the `PATH` is
/// for what the pane runs *later*, not for reaching the agent.
///
/// Empty (no prefix at all) when there is no CLI directory to add or no `env`
/// to add it with: an improvement where it succeeds, never a new way to fail.
#[cfg(not(windows))]
pub(crate) fn path_prefix_args() -> Vec<std::ffi::OsString> {
    let (Some(path), Some(env_bin)) = (path_with_cli_directory(), posix_env_binary()) else {
        return Vec::new();
    };
    // Carried as `OsString` the whole way, never through `to_string_lossy`: a
    // Unix `PATH` is bytes, not UTF-8, and replacing an offending one would
    // hand the pane a **corrupted** `PATH` — losing it every lookup that used
    // to work, which is worse than the lookup this exists to add.
    let mut assignment = std::ffi::OsString::from("PATH=");
    assignment.push(&path);
    vec![env_bin.into_os_string(), assignment]
}

/// [`path_prefix_args`] as **shell-escaped text**, for the two places a whole
/// window command is one string a shell will read.
///
/// `None` when the prefix cannot be spelled as text — a `PATH` that is not
/// UTF-8 — which the callers take as "no prefix", never as a mangled one. Also
/// `None` when there was no prefix to begin with, since an empty one and an
/// unspellable one lead to the same command.
pub(crate) fn shell_prefix_tokens() -> Option<Vec<String>> {
    let args = path_prefix_args();
    if args.is_empty() {
        return None;
    }
    args.iter()
        .map(|a| a.to_str().map(control_mode::shell_escape))
        .collect()
}

/// This process's `PATH` with the directory holding this build's `thurbox-cli`
/// in front. `None` when [`resolve_cli_binary`] fell back to a bare name —
/// there is no directory to add, and pinning a `PATH` with nothing to add to it
/// would only restate what the pane was going to inherit anyway.
#[cfg(not(windows))]
fn path_with_cli_directory() -> Option<std::ffi::OsString> {
    let cli = resolve_cli_binary();
    let dir = cli.parent().filter(|d| !d.as_os_str().is_empty())?;
    path_led_by(dir, &std::env::var_os("PATH").unwrap_or_default())
}

/// `inherited` with `dir` moved to the front: prepended if it was absent,
/// promoted if it was already there. Never duplicated — a `PATH` that names one
/// directory twice is a lookup the reader has to think about twice.
///
/// **Empty components are dropped**, for the reason
/// [`crate::paths::resolve_on_path`] skips them: POSIX reads one as "the
/// current directory", and the directory current in a pane is the session's
/// worktree — the agent's own checkout, whose contents are the last thing that
/// should shadow a binary. An unset `PATH` splits into exactly one of those, so
/// this is also what makes the empty case answer with the directory alone.
#[cfg(not(windows))]
fn path_led_by(dir: &Path, inherited: &std::ffi::OsStr) -> Option<std::ffi::OsString> {
    let dirs = std::iter::once(dir.to_path_buf())
        .chain(std::env::split_paths(inherited).filter(|d| d != dir && !d.as_os_str().is_empty()))
        .collect::<Vec<_>>();
    std::env::join_paths(dirs).ok()
}

/// `env`, preferring what `PATH` resolves and falling back to the path POSIX
/// gives it. `None` on a machine with neither, where the prefix is skipped
/// rather than risked.
#[cfg(not(windows))]
fn posix_env_binary() -> Option<std::path::PathBuf> {
    crate::paths::resolve_on_path("env").or_else(|| {
        let posix = std::path::PathBuf::from("/usr/bin/env");
        posix.is_file().then_some(posix)
    })
}

/// Never on Windows: its multiplexer runs its own command model, and its
/// [`MuxDialect::push_window_program`] folds the environment into the window
/// command instead.
#[cfg(windows)]
pub(crate) fn path_prefix_args() -> Vec<std::ffi::OsString> {
    Vec::new()
}

/// One-shot read of every pane's remote-hook state option on `host`:
/// `list-panes -s -t <session> -F "#{pane_id} #{@thurbox_state}"` over the
/// host launcher, parsed to the set `(pane_id, state)` pairs. The headless
/// status poll (`session_ops::remote_hooks::poll_remote_hook_states`) uses it
/// to keep remote hook states flowing with no TUI attached. Read-only by
/// design — no `ensure_ready`, so a poll never creates the remote
/// server/session; an unreachable host or absent server is an `Err` the
/// caller treats as "no reports this cycle".
pub fn list_remote_hook_states(host: &crate::session::HostDef) -> Result<Vec<(String, String)>> {
    crate::agent::registry::host_mux(host).hook_states()
}

/// [`list_remote_hook_states`] for this machine's own server: the pane option
/// a session created *from afar* on this host sets (its hooks were rewritten to
/// that form), which nothing here read before sessions were shared.
pub fn list_local_hook_states() -> Result<Vec<(String, String)>> {
    list_hook_states_on(&MuxBackend::<LocalMuxTransport>::local())
}

/// The one-shot reads a relaunch probe or a status poll runs against a
/// host's server, whichever multiplexer the host runs. Chosen by
/// [`crate::agent::registry::host_mux`]; none of them starts control mode.
pub(crate) trait HostMux {
    fn discover_answered(&self) -> Result<Vec<DiscoveredSession>>;
    fn hook_states(&self) -> Result<Vec<(String, String)>>;
}

impl<T: MuxDialect> HostMux for MuxBackend<T> {
    fn discover_answered(&self) -> Result<Vec<DiscoveredSession>> {
        MuxBackend::discover_answered(self)
    }
    fn hook_states(&self) -> Result<Vec<(String, String)>> {
        list_hook_states_on(self)
    }
}

fn list_hook_states_on<T: MuxDialect>(backend: &MuxBackend<T>) -> Result<Vec<(String, String)>> {
    if !backend.session_exists() {
        return Ok(Vec::new());
    }
    let session = backend.session.clone();
    let format = format!(
        "#{{pane_id}} #{{{}}}",
        crate::session::REMOTE_HOOK_STATE_OPTION
    );
    let body = backend.tmux_output(&["list-panes", "-s", "-t", &session, "-F", &format])?;
    Ok(control_mode::parse_pane_hook_states(&body))
}

/// Where a session's *running* agent window is, on the local server or on
/// `host`. A one-shot listing, no control mode: this is what the headless
/// relaunch and teardown paths ask.
///
/// The answer is three-valued on purpose. [`Located::Unknown`] — several
/// windows share the name and none is stamped — is not absence, and a caller
/// that relaunches on it puts a third agent beside the two that already
/// collide.
pub fn agent_window(
    host: Option<&crate::session::HostDef>,
    session_id: &str,
    session_name: &str,
) -> Result<Located> {
    // A one-shot `list-windows`, and deliberately nothing more: `discover`
    // answers empty for a server that is not there, where starting control
    // mode would bring one into being. `discover_answered` (not `discover`)
    // so an unreachable host surfaces as `Err`, not as an empty listing that
    // reads the same as "no such window" — see `mux_answered_absent`.
    let index = WindowIndex::from_listing(match host {
        Some(host) => {
            known_host_socket(host)?;
            crate::agent::registry::host_mux(host).discover_answered()?
        }
        None => MuxBackend::<LocalMuxTransport>::local().discover_answered()?,
    });
    Ok(index.live_agent_window(session_id, session_name))
}

fn rename_session_windows_on<T: MuxDialect>(
    backend: &MuxBackend<T>,
    session_id: &str,
    from: &str,
    to: &str,
) -> Result<()> {
    let index = WindowIndex::from_listing(backend.discover_answered()?);
    for role in [WindowRole::Agent, WindowRole::Shell] {
        match index.locate(session_id, from, role, false) {
            Located::At(pane) => {
                backend.tmux_run(&["rename-window", "-t", &pane, &window_name_for(role, to)])?;
            }
            Located::Absent => {}
            Located::Unknown => bail!(
                "several windows are named after '{from}' and none is stamped as this \
                 session's, so there is no telling which one to rename"
            ),
        }
    }
    Ok(())
}

/// Record a hook state on the pane this process runs in — the pane option a
/// remote observer's control-mode subscription reads — so a status reported
/// through the CLI reaches a peer within a second, not at the mirror's
/// cadence. `$TMUX` is `<socket path>,<pid>,<session index>`; the socket is
/// addressed by path (`-S`) because it is whichever server the pane is on,
/// which need not be this build's own. Silently nothing outside tmux.
pub fn set_own_pane_state(state: &str) -> Result<()> {
    let (Some(tmux), Some(pane)) = (
        std::env::var_os("TMUX").map(|s| s.to_string_lossy().into_owned()),
        std::env::var_os("TMUX_PANE").map(|s| s.to_string_lossy().into_owned()),
    ) else {
        return Ok(());
    };
    let Some(socket_path) = own_socket_path(&tmux) else {
        return Ok(());
    };
    if !control_mode::is_valid_pane_id(&pane) {
        return Ok(());
    }
    let status = Command::new(DEFAULT_MUX)
        .args([
            "-S",
            &socket_path,
            "set-option",
            "-p",
            "-t",
            &pane,
            crate::session::REMOTE_HOOK_STATE_OPTION,
            state,
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .context("run tmux set-option on the own pane")?;
    if !status.success() {
        bail!("tmux set-option exited {status}");
    }
    Ok(())
}

/// The socket path in a `$TMUX` value (`<path>,<pid>,<index>`).
pub(crate) fn own_socket_path(tmux_env: &str) -> Option<String> {
    let path = tmux_env.split(',').next()?.trim();
    (!path.is_empty()).then(|| path.to_string())
}

/// The socket a remote operation on `host` may act on, or why it must not act
/// at all.
///
/// A host that runs a thurbox of its own owns the socket its sessions live on:
/// its `hosts.toml` override, or what its CLI reported ([`learn_host_socket`]).
/// This build's compile-time default is a guess about somebody else's machine —
/// a dev build would aim at `thurbox-dev` while the host's release binary runs
/// `thurbox`, and a host with a relocated data dir derives a name of its own —
/// so a teardown refuses rather than acting on it. With sharing off nothing but
/// this thurbox writes there, so the default is ours by construction.
pub fn known_host_socket(host: &crate::session::HostDef) -> Result<String> {
    if let Some(socket) = host
        .socket
        .clone()
        .or_else(|| learned_host_socket(&host.backend_name()))
    {
        return Ok(socket);
    }
    if !host.shareable() {
        return Ok(TMUX_SOCKET.to_string());
    }
    bail!(
        "socket unknown for host '{}': it runs a thurbox of its own and has not \
         reported which socket that is (set `socket` in hosts.toml, or make its \
         thurbox-cli reachable)",
        host.name
    )
}

/// Kill what a listing placed, if it placed anything this session may claim.
fn kill_located<T: MuxDialect>(
    backend: &MuxBackend<T>,
    located: Located,
    remembered: &str,
    session_name: &str,
    host_name: &str,
) -> Result<bool> {
    match located {
        Located::At(pane) => backend.kill_pane_oneshot(&pane).map(|()| true),
        // Already gone, or never this session's to begin with.
        Located::Absent => Ok(false),
        Located::Unknown if !T::WINDOW_OPTIONS && !remembered.is_empty() => {
            backend.kill_pane_oneshot(remembered).map(|()| true)
        }
        Located::Unknown => {
            debug!("not killing a window on {host_name}: '{session_name}' is ambiguous there");
            Ok(false)
        }
    }
}

/// One-shot operations on this backend's own server — the session-scoped
/// half of the `SessionBackend` pane operations, and what the platform
/// wrappers below call for the local server.
impl<T: MuxDialect> MuxBackend<T> {
    /// A one-shot multiplexer command on this backend's server.
    fn oneshot(&self, args: &[&str]) -> Command {
        self.transport.mux_command(&self.socket(), args)
    }

    /// What a one-shot reports when the multiplexer will not start (see
    /// [`crate::agent::preflight::launch_failure`]).
    fn launch_failure(&self, context: &'static str, err: std::io::Error) -> anyhow::Error {
        crate::agent::preflight::launch_failure(&self.transport, context, err)
    }

    /// Build the `session:=window` tmux target for a thurbox agent session.
    ///
    /// The `=` prefix forces tmux to match the window name exactly. Without
    /// it tmux falls back to FNMATCH-style prefix matching, so a target of
    /// `tb-foo` would resolve ambiguously when both `tb-foo` and
    /// `tb-foo-bar` exist — `send-keys`/`capture-pane` then fails with
    /// "ambiguous window" and the caller's text is silently dropped.
    fn window_target(&self, window_name: &str) -> String {
        format!("{}:={window_name}", self.session)
    }

    /// Leave one window carrying `session_id`'s `role` stamp, and say which one
    /// kept it — or `None` when there was never more than one to choose between.
    ///
    /// ADR-25 gives a stamp its meaning under one invariant: one session, one
    /// window per role. Two windows carrying it is not a weaker answer but no
    /// answer at all — [`WindowIndex::stamped_match`] returns [`Located::Unknown`]
    /// by design, and from then on the session cannot be sent to, killed, captured
    /// or renamed. A restart is kill-then-spawn and a repairer relaunches anything
    /// a listing says is gone, so the two overlapping put one stamp on two windows
    /// and the session was lost for good (issue #1207).
    ///
    /// **The highest window id keeps the identity.** The rule is that rather than
    /// "the window I just stamped" because both racers run this: "mine wins" has
    /// each of them retire the other's and can leave the session no window at all,
    /// while a key tmux issues in order and never reissues while the server lives
    /// makes every sweep reach the same verdict from any listing that sees both.
    /// That is also what closes the gap a window opens between being created and
    /// being stamped — the sweep runs *after* each stamp, so the last stamp to land
    /// is followed by a listing that sees every earlier one, whichever order the
    /// windows were created in.
    ///
    /// It is the right half to keep, too. The newest window is the one the most
    /// recent restart asked for, and where both panes resume one conversation it is
    /// the connection that displaced the other — the loser is the pane that printed
    /// "another connection took over this session".
    ///
    /// Liveness is deliberately **not** the key. It changes between two listings
    /// taken a moment apart, so two sweeps could each keep what the other retired,
    /// and a session with no window at all is the one outcome worse than the pair.
    /// A newest window whose pane has already exited is kept and stays on screen
    /// (`remain-on-exit`), which is how the operator gets to see why it exited.
    ///
    /// Only where the local multiplexer has window options. One that keeps `@`
    /// options in a single server-global map hands the same value back for every
    /// window (ADR-13), so every window there would read as stamped for whoever was
    /// stamped last; the stamp is withheld at both ends for that reason (issue
    /// #1168) and this must not be the one place that believes it.
    pub(crate) fn retire_duplicate_windows(
        &self,
        session_id: &str,
        role: WindowRole,
    ) -> Option<String> {
        if !T::WINDOW_OPTIONS || session_id.is_empty() {
            return None;
        }
        let output = self
            .oneshot(&["list-windows", "-t", TMUX_SESSION, "-F", RETIRE_FORMAT])
            .output()
            .ok()?;
        if !output.status.success() {
            return None;
        }
        let listing = String::from_utf8_lossy(&output.stdout);
        let windows = stamped_windows_in(&listing, session_id, role);
        let (keep, retire) = windows.split_last()?;
        let mut retired = false;
        for window in retire {
            match self.kill_window_at(window) {
                Ok(()) => {
                    retired = true;
                    warn!(
                        "retired {window}: it carried session {session_id}'s {} stamp, which \
                         {keep} now holds alone",
                        role.as_str()
                    );
                }
                Err(e) => {
                    warn!("could not retire {window}, stamped for session {session_id}: {e:#}")
                }
            }
        }
        retired.then(|| keep.clone())
    }

    /// Stamp a window on the local server with the identity every reconciler
    /// resolves it by.
    ///
    /// `target` is the new window's pane id, or its `session:=window` target where
    /// the spawn could not report one. Best-effort by design, and so returns
    /// nothing to check — but **not attempted at all** on a multiplexer without
    /// window options (`MuxDialect::WINDOW_OPTIONS`, ADR-13), where the
    /// sole-namesake rule in [`WindowIndex`] carries the identity as the name
    /// fallback did before. Writing one there is not the harmless no-op it reads
    /// as: such a multiplexer keeps a server-global option under the name and
    /// answers `#{@...}` with it for every window, so a single stamp made every
    /// window look like one session's and cost the interface every pane it had
    /// (issue #1168).
    pub(crate) fn stamp_oneshot(&self, target: &str, session_id: &str, role: WindowRole) {
        if !T::WINDOW_OPTIONS {
            return;
        }
        for (option, value) in [
            (WINDOW_SESSION_OPTION, session_id),
            (WINDOW_ROLE_OPTION, role.as_str()),
        ] {
            if value.is_empty() {
                continue;
            }
            match self
                .oneshot(&["set-option", "-w", "-t", target, option, value])
                .output()
            {
                Ok(out) if out.status.success() => {}
                Ok(out) => debug!(
                    "could not stamp {target} with {option}: {}",
                    String::from_utf8_lossy(&out.stderr).trim()
                ),
                Err(e) => debug!("could not stamp {target} with {option}: {e}"),
            }
        }
        // The invariant the stamp is only meaningful under, enforced where the
        // stamp is written rather than left to a resolver that is required to
        // refuse the pair — see `retire_duplicate_windows`.
        let _ = self.retire_duplicate_windows(session_id, role);
    }

    /// Every thurbox window on the local server, indexed.
    pub(crate) fn window_index(&self) -> Result<WindowIndex> {
        Ok(WindowIndex::from_listing(self.discover()?))
    }

    /// Resolve the local tmux target for acting on a session's agent pane, or
    /// `None` when nothing on the server is this session's to act on.
    ///
    /// Every one-shot helper that acts on a session's pane goes through here, so
    /// the stamp decides in one place. What it replaced was `tb-<name>`, a target
    /// tmux matches exactly and resolves to an arbitrary one of a session's
    /// namesakes.
    pub(crate) fn agent_target(&self, session_id: &str, session_name: &str) -> Option<String> {
        self.owned_target(session_id, session_name, WindowRole::Agent)
    }

    /// [`agent_target`] for any of a session's windows — its agent, or the
    /// companion shell a teardown has to take down with it.
    pub(crate) fn owned_target(
        &self,
        session_id: &str,
        session_name: &str,
        role: WindowRole,
    ) -> Option<String> {
        match self.locate_here(session_id, session_name, role) {
            Located::At(pane) => Some(pane),
            Located::Absent => None,
            // One stamp on two windows is repairable, and here is where repairing
            // it matters: `stamped_match` refuses the pair by design, so without
            // this nothing would ever look again and the session stayed
            // unaddressable for good (issue #1207). The refusal itself is
            // untouched — the choice is made by *retiring* a window, which is a
            // write, and never by reading one of two as the answer. A name that is
            // ambiguous rather than a stamp retires nothing and falls through
            // exactly as before.
            Located::Unknown => match self.retire_duplicate_windows(session_id, role) {
                Some(_) => match self.locate_here(session_id, session_name, role) {
                    Located::At(pane) => Some(pane),
                    _ => None,
                },
                None => (!T::WINDOW_OPTIONS)
                    .then(|| self.window_target(&window_name_for(role, session_name))),
            },
        }
    }

    /// One listing of the local server, resolved. [`Located::Unknown`] is also what
    /// a listing that could not be taken answers: not knowing is not absence.
    pub(crate) fn locate_here(
        &self,
        session_id: &str,
        session_name: &str,
        role: WindowRole,
    ) -> Located {
        match self.window_index() {
            Ok(index) => index.locate(session_id, session_name, role, false),
            Err(e) => {
                debug!("could not list windows to resolve '{session_name}': {e:#}");
                Located::Unknown
            }
        }
    }

    /// Whether `target`'s pane has exited. The one-shot mirror of
    /// [`MuxBackend::is_dead`], which asks the same question over control mode.
    ///
    /// Errors read as "not dead" so a tmux hiccup degrades to the previous
    /// behavior (attempt the send) rather than silently dropping a prompt.
    pub(crate) fn pane_is_dead(&self, target: &str) -> bool {
        self.oneshot(&["display-message", "-p", "-t", target, "#{pane_dead}"])
            .output()
            .ok()
            .filter(|out| out.status.success())
            .map(|out| parse_pane_dead(&String::from_utf8_lossy(&out.stdout)))
            .unwrap_or(false)
    }

    /// Type text into a session pane (no scheduling), submitting it or not.
    ///
    /// Targets the session's own pane via `agent_target`, which refuses a window
    /// stamped for another session (ADR-25). Submitting uses a "paste text → brief delay → press
    /// Enter" sequence so the target app has time to process the pasted input.
    ///
    /// `submit = false` types the text and stops: it lands in the agent's composer
    /// unsent, which is what a "type it, check what the pane shows, then submit"
    /// protocol needs — submitting on the way in fires every steer the instant it
    /// is typed. [`Self::send_key_now`] with `enter` is the other half.
    ///
    /// The text goes out as one paste either way (see
    /// `MuxDialect::paste_args`), so it arrives literally: no shell is involved, and
    /// the wrap is also what keeps a leading `-` from reading as a `send-keys`
    /// flag and a newline from submitting the line before it.
    ///
    /// Refuses a pane whose process has exited. Sessions run with
    /// `remain-on-exit=on` (`SESSION_OPTS`), so a dead agent leaves its window in
    /// place and `send-keys` still exits 0 while discarding the keystrokes. Every
    /// caller reads that success as "the agent got it" — which is how the mailbox
    /// wake came to report `woke: true` at a pane nothing was listening to — so the
    /// liveness check belongs here, once, rather than in each of them.
    pub(crate) fn send_text_now(
        &self,
        session_id: &str,
        session_name: &str,
        text: &str,
        submit: bool,
    ) -> Result<()> {
        let Some(target) = self.agent_target(session_id, session_name) else {
            bail!("session '{session_name}' has no window of its own here");
        };
        if self.pane_is_dead(&target) {
            bail!("session '{session_name}' has exited; its pane accepts no input");
        }
        let paste = T::paste_args(&target, text);
        let paste_argv: Vec<&str> = paste.iter().map(String::as_str).collect();
        let out = self
            .oneshot(&paste_argv)
            .output()
            .context("Failed to paste prompt text into the session pane")?;
        if !out.status.success() {
            bail!("{} {} {}", T::BINARY, paste[0], mux_failure(&out));
        }

        if !submit {
            return Ok(());
        }

        std::thread::sleep(SEND_KEYS_ENTER_DELAY);

        let out = self
            .oneshot(&["send-keys", "-t", &target, "Enter"])
            .output()
            .context("Failed to send Enter to the session pane")?;
        if !out.status.success() {
            bail!("{} send-keys (Enter) {}", T::BINARY, mux_failure(&out));
        }
        Ok(())
    }

    /// Send one named special key to a session pane — no text, no Enter.
    ///
    /// `tmux_key` is a [`ResolvedKey::tmux`] name, never a caller's string: tmux
    /// types an unrecognized name into the pane instead of refusing it.
    ///
    /// Refuses a dead pane for the same reason [`send_text_now`] does — `send-keys`
    /// exits 0 into a `remain-on-exit` corpse, so success would be a lie.
    pub(crate) fn send_key_now(
        &self,
        session_id: &str,
        session_name: &str,
        tmux_key: &str,
    ) -> Result<()> {
        let Some(target) = self.agent_target(session_id, session_name) else {
            bail!("session '{session_name}' has no window of its own here");
        };
        if self.pane_is_dead(&target) {
            bail!("session '{session_name}' has exited; its pane accepts no input");
        }
        let out = self
            .oneshot(&["send-keys", "-t", &target, tmux_key])
            .output()
            .context("Failed to send a key to the session pane")?;
        if !out.status.success() {
            bail!("{} send-keys ({tmux_key}) {}", T::BINARY, mux_failure(&out));
        }
        Ok(())
    }

    /// Whether the session has an agent pane of its own on the thurbox tmux server.
    ///
    /// A window stamped for a namesake does not count (ADR-25). Used by the
    /// headless dispatcher to skip `send`
    /// automations whose target session is no longer running rather than failing
    /// into a dead pane.
    pub(crate) fn window_exists(&self, session_id: &str, session_name: &str) -> bool {
        self.agent_target(session_id, session_name).is_some()
    }

    /// Schedule a one-shot prompt delivery into a session's window after
    /// `delay_secs`, via a detached `tmux run-shell` timer.
    ///
    /// Used by the headless automation dispatcher to deliver a Spawn automation's
    /// prompt once the freshly launched agent CLI has had time to boot — offline
    /// there is no TUI deferred-input queue to lean on. Local-tmux scoped.
    pub(crate) fn send_prompt_after_delay(
        &self,
        session_id: &str,
        session_name: &str,
        text: &str,
        delay_secs: u64,
    ) -> Result<()> {
        let Some(target) = self.agent_target(session_id, session_name) else {
            bail!("session '{session_name}' has no window of its own here");
        };
        let script = T::deferred_prompt_script(&self.socket(), &target, text);
        let out = self
            .oneshot(&["run-shell", "-b", "-d", &delay_secs.to_string(), &script])
            .output()
            .context("Failed to schedule tmux run-shell for deferred prompt")?;
        if !out.status.success() {
            bail!("tmux run-shell (deferred prompt) {}", mux_failure(&out));
        }
        Ok(())
    }

    /// Capture the rendered contents of a session's pane.
    ///
    /// Returns the visible terminal text. `lines` controls how many lines of
    /// scrollback to include before the visible region (capped to a sane max).
    /// With `ansi`, tmux emits the styling escape sequences too (`capture-pane
    /// -e`) instead of flattening the screen to plain text.
    pub(crate) fn capture_pane_text(
        &self,
        session_id: &str,
        session_name: &str,
        lines: u32,
        ansi: bool,
    ) -> Result<String> {
        let Some(target) = self.agent_target(session_id, session_name) else {
            bail!("session '{session_name}' has no window of its own here");
        };
        let lines = lines.min(MAX_CAPTURE_LINES);
        let start = format!("-{lines}");

        let mut args = vec!["capture-pane", "-p", "-J", "-t", &target, "-S", &start];
        if ansi {
            args.push("-e");
        }
        let output = self
            .oneshot(&args)
            .output()
            .map_err(|e| self.launch_failure("Failed to run tmux capture-pane", e))?;
        if !output.status.success() {
            bail!(
                "tmux capture-pane exited with status {}: {}",
                output.status,
                String::from_utf8_lossy(&output.stderr)
            );
        }
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }

    /// Read a session pane's cursor position, foreground process and live cwd.
    ///
    /// Best-effort by construction — see [`PaneState`]. One `display-message` for
    /// everything tmux knows, plus at most one `ps` to turn the cheap command
    /// *name* into the foreground process's argv. `session_id`/`session_name`
    /// resolve the window the same way [`Self::capture_pane_text`] does, so the state
    /// describes the pane the capture came from.
    pub(crate) fn pane_state(&self, session_id: &str, session_name: &str) -> PaneState {
        let Some(target) = self.agent_target(session_id, session_name) else {
            return PaneState::default();
        };
        let format = [
            "#{cursor_y}",
            "#{cursor_x}",
            "#{pane_current_command}",
            "#{pane_current_path}",
            "#{pane_tty}",
            "#{pane_dead}",
            "#{window_name}",
        ]
        .join(&PANE_STATE_SEP.to_string());

        let mut argv = Vec::with_capacity(6);
        if T::UTF8_FLAG {
            argv.push(PANE_STATE_UTF8_FLAG);
        }
        argv.extend(["display-message", "-p", "-t", &target, &format]);

        let Some(raw) = self
            .oneshot(&argv)
            .output()
            .ok()
            .filter(|out| out.status.success())
            .map(|out| String::from_utf8_lossy(&out.stdout).into_owned())
        else {
            return PaneState::default();
        };

        let (mut state, tty, window) = parse_pane_state(&raw);
        // `display-message` against a target it cannot resolve does **not** fail:
        // it answers for the client's current pane and exits 0. Reporting a
        // stranger's shell as this session's foreground process is the plausible
        // wrong answer every field here is built to avoid, so the answer is only
        // kept when it demonstrably came from this session's own window.
        if window.as_deref() != Some(agent_window_name(session_name).as_str()) {
            return PaneState::default();
        }
        if let Some((argv0, command)) = tty.as_deref().and_then(foreground_process_on_tty) {
            state.foreground_process = Some(argv0);
            state.foreground_command = Some(command);
        }
        state
    }

    /// The `PATH` thurbox handed a local session's **agent pane**, read back off
    /// the pane's own start command.
    ///
    /// A hook command names `thurbox-cli` bare (`thurbox-cli session signal --state
    /// <s> || true`), so the only `PATH` that decides whether it resolves is the
    /// pane's. Anything else — notably the `PATH` of whatever process is asking —
    /// is a different machine-state question that happens to have the same shape,
    /// and answering one with the other is how `session doctor` reported healthy
    /// wiring for panes that could not find the binary at all.
    ///
    /// The evidence is the `env PATH=…` prefix a local spawn writes in front of the
    /// window's program, which tmux keeps verbatim in `#{pane_start_command}`. That is deliberately not
    /// `/proc/<pid>/environ`: reading another process's environment needs
    /// `PTRACE_MODE_READ`, which Debian and Ubuntu restrict to a tracer's own
    /// descendants by default (`kernel.yama.ptrace_scope = 1`) — so it would answer
    /// for a `doctor` run from the TUI and refuse the same question typed into a
    /// terminal, which is the way it is usually asked. tmux's own record has no
    /// such rule, and no platform gate either.
    ///
    /// Three answers, not two. "This machine's server holds no agent window for the
    /// session" and "the window is there and its `PATH` is not one thurbox wrote"
    /// are different facts, and rounding the second to the first is what let a
    /// broken pane report healthy: a parked session has nothing to verify, while a
    /// live pane that cannot be verified is a live pane that may not work.
    /// Remote sessions are the caller's to exclude: this reads **this** machine's
    /// server.
    pub(crate) fn agent_pane_path(&self, session_id: &str, session_name: &str) -> PanePath {
        let Some(target) = self.agent_target(session_id, session_name) else {
            return PanePath::Absent;
        };
        let format = format!("#{{pane_start_command}}{PANE_STATE_SEP}#{{window_name}}");
        let mut argv = Vec::with_capacity(6);
        if T::UTF8_FLAG {
            argv.push(PANE_STATE_UTF8_FLAG);
        }
        argv.extend(["display-message", "-p", "-t", &target, &format]);
        let Some(raw) = self
            .oneshot(&argv)
            .output()
            .ok()
            .filter(|out| out.status.success())
            .map(|out| String::from_utf8_lossy(&out.stdout).into_owned())
        else {
            return PanePath::Absent;
        };
        let line = normalized_pane_answer(&raw);
        let mut fields = line.split(PANE_STATE_SEP);
        let (Some(start_command), Some(window)) = (fields.next(), fields.next()) else {
            return PanePath::Absent;
        };
        // `display-message` against a target it cannot resolve does **not** fail:
        // it answers for the client's current pane and exits 0. That is the trap
        // `pane_state` guards against too, and here it would hand back a stranger's
        // `PATH` as this session's.
        if window != agent_window_name(session_name) {
            return PanePath::Absent;
        }
        match path_from_prefix(start_command) {
            Some(path) => PanePath::Known(path),
            None => PanePath::Unknown,
        }
    }

    /// Spawn a new tmux window running `command` with `args` in `cwd`.
    ///
    /// Thin helper for headless callers (CLI, MCP) that don't need PTY I/O
    /// streams. Returns the new pane's id (`%N`) on success; the command runs
    /// inside it. Window name is `tb-<session_name>` — which is *not* unique (two
    /// sessions can share a name), so the window is stamped with `session_id`
    /// before this returns and every later lookup resolves that (ADR-25).
    ///
    /// A pane id on stdout outranks a non-zero exit status, which on this path can
    /// belong to a user's tmux hook rather than to the window — see the read below.
    ///
    /// Where the local dialect has no `MuxDialect::ONESHOT_PANE_REPORT` the id
    /// is not asked for and an empty string is returned, so the window is known by
    /// its name instead (and is best-effort, like the other window-option
    /// carve-outs). With no id to weigh, a non-zero status is the whole answer
    /// there, exactly as before.
    pub(crate) fn spawn_window(
        &self,
        session_id: &str,
        session_name: &str,
        command: &str,
        args: &[String],
        cwd: Option<&Path>,
        env: &HashMap<String, String>,
    ) -> Result<String> {
        // Ensure the session exists and is configured, without opening a
        // control-mode connection (headless one-shot path).
        self.ensure_session_configured()?;
        if T::ASKS_SERVER_VERSION {
            self.check_server_version_oneshot()?;
        }

        let window_name = agent_window_name(session_name);
        // Created at the END of the session's window list, so the retention below
        // can name the window this command just made: `{end}` is the last window
        // and `-a` appends after it, so within this one command list `{end}` is
        // exactly the new one. The window's *name* cannot say that — `tb-<session
        // name>` is not unique (two sessions can share a name, which is why the
        // stamp exists), and tmux resolves a duplicate name to the lowest index,
        // which is the older window (measured, tmux 3.2a). A dialect without the
        // one-shot pane report keeps the plain session target: it gets no
        // retention write either, and the shorthand is tmux's.
        let create_target = if T::ONESHOT_PANE_REPORT {
            format!("{TMUX_SESSION}:{{end}}")
        } else {
            format!("{TMUX_SESSION}:")
        };
        let mut tmux =
            self.new_window_command(&window_name, &create_target, command, args, cwd, env);
        // The stamp rides in the same command list as the creation, like the birth
        // options: two `set-option` processes fewer on every `session create`
        // (#1243). `{end}` still names the new window here, and the pane id it
        // would otherwise be written against is not known until the list returns.
        let stamped = T::WINDOW_OPTIONS;
        if stamped {
            for (option, value) in [
                (WINDOW_SESSION_OPTION, session_id),
                (WINDOW_ROLE_OPTION, WindowRole::Agent.as_str()),
            ] {
                if !value.is_empty() {
                    tmux.args([";", "set-option", "-w", "-t", &create_target, option, value]);
                }
            }
        }

        let output = tmux.output().map_err(|e| {
            self.launch_failure("Failed to run tmux new-window for headless spawn", e)
        })?;
        // Without a pane report the window name is the only handle this path has.
        let pane_id = if T::ONESHOT_PANE_REPORT {
            new_window_pane_id(&output.stdout)
        } else {
            String::new()
        };
        if !output.status.success() {
            // The exit status is not this command's verdict on its own. tmux hands
            // a command-mode client the status of the last `run-shell` its command
            // list triggered, and a *hook* counts: an `after-new-window` left
            // behind by an uninstalled tmux plugin runs a script that is no longer
            // there, `/bin/sh` answers 127, and the client exits 127 although
            // `new-window` succeeded and already printed the pane id (measured,
            // tmux 3.5a; stderr is empty, so the message named nothing either).
            // The pane id is the answer to `-P`, so where there is one the window
            // exists and refusing it tears down a session that started fine
            // (issue #1154). The control-mode path never sees this: its reply block
            // carries the `-P` answer alone and no exit status at all.
            if !control_mode::is_valid_pane_id(&pane_id) {
                let stderr = String::from_utf8_lossy(&output.stderr);
                bail!(
                    "tmux new-window exited {} for window {}: {}",
                    output.status,
                    window_name,
                    stderr.trim()
                );
            }
            warn!(
                "tmux answered {} for window {} and created it anyway ({}): a hook \
                 on thurbox's own server failed, which is what an uninstalled \
                 plugin's leftover hook does for the life of that server. `{} -L {} \
                 show-hooks -g` names it. Unset the hook rather than killing that \
                 server — it holds every live session",
                output.status,
                window_name,
                pane_id,
                T::BINARY,
                self.socket()
            );
        }
        // `window_target` takes a window *name*: the session's is `tb-<name>`, and
        // naming the session itself resolved to whatever window that picked out.
        let target = if pane_id.is_empty() {
            self.window_target(&window_name)
        } else {
            pane_id.clone()
        };
        if stamped {
            // What `stamp_oneshot` does after writing the stamp.
            let _ = self.retire_duplicate_windows(session_id, WindowRole::Agent);
        } else {
            self.stamp_oneshot(&target, session_id, WindowRole::Agent);
        }
        Ok(pane_id)
    }

    /// [`MuxDialect::check_server_version`] against the local server's own
    /// `#{version}` — the headless twin of the check in [`MuxBackend::spawn`].
    pub(crate) fn check_server_version_oneshot(&self) -> Result<()> {
        let output = self
            .oneshot(&["display-message", "-t", TMUX_SESSION, "-p", "#{version}"])
            .output()
            .map_err(|e| self.launch_failure("Failed to ask the multiplexer its version", e))?;
        T::check_server_version(&String::from_utf8_lossy(&output.stdout), &self.socket())
    }

    /// The `new-window` command list [`spawn_window`] runs: the window created
    /// detached at `create_target` running `command` — and, where the dialect
    /// takes them, its birth options chained into the same invocation.
    pub(crate) fn new_window_command(
        &self,
        window_name: &str,
        create_target: &str,
        command: &str,
        args: &[String],
        cwd: Option<&Path>,
        env: &HashMap<String, String>,
    ) -> Command {
        let report = T::ONESHOT_PANE_REPORT;
        let mut tmux = self.oneshot(&["new-window", "-d"]);
        if report {
            tmux.arg("-a");
        }
        tmux.args(["-t", create_target, "-n", window_name]);
        if report {
            tmux.args(["-P", "-F", "#{pane_id}"]);
        }
        if let Some(dir) = cwd {
            tmux.args(["-c", &dir.to_string_lossy()]);
        }
        T::push_window_program(&mut tmux, command, args, env);

        // Chained into the same command list as the creation, not sent after it —
        // `birth_options` has the measurement. This path passes `-d`, so the new
        // window is not current and the bare form the control-mode path uses is not
        // available; `{end}` names it instead, which is why the window is created
        // there.
        if report {
            for (key, value) in birth_options(window_name) {
                tmux.args([";", "set-window-option", "-t", create_target]);
                tmux.args([key, value]);
            }
        }
        tmux
    }

    /// Kill the session's own tmux window, if the local server still holds one.
    ///
    /// Resolved through the window's own stamp, so one stamped for another session —
    /// a live namesake's — is never the one that comes down. That is not a
    /// nicety: names are not unique, a soft-deleted row keeps its name until it is
    /// reaped, and `kill-window -t tb-<name>` matches an arbitrary one of them.
    pub(crate) fn kill_window(&self, session_id: &str, session_name: &str) -> Result<()> {
        match self.agent_target(session_id, session_name) {
            Some(target) => self.kill_window_at(&target),
            None => Ok(()),
        }
    }

    /// Kill the session's companion shell window (`tbs-`), if the local server
    /// still holds one.
    ///
    /// The other half of [`kill_window`]: a session owns two windows, and a
    /// teardown that takes only the agent leaves a `tbs-` shell running for a row
    /// that no longer exists. Resolved by the same stamp, so a NULL
    /// `shell_backend_id` — the usual state, since the column is written only when
    /// the interface opens the shell — costs nothing.
    pub(crate) fn kill_shell_window(&self, session_id: &str, session_name: &str) -> Result<()> {
        match self.owned_target(session_id, session_name, WindowRole::Shell) {
            Some(target) => self.kill_window_at(&target),
            None => Ok(()),
        }
    }

    /// Run `kill-window` against an already-resolved target, tolerating a window
    /// that is already gone. Shared by [`kill_window`] and the reap, which resolves
    /// its own target through [`WindowIndex`] (`session_ops::delete`) — the two
    /// differ only in how the target is chosen.
    pub(crate) fn kill_window_at(&self, target: &str) -> Result<()> {
        let output = self
            .oneshot(&["kill-window", "-t", target])
            .output()
            .context("Failed to run tmux kill-window")?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            // It's fine if the window is already gone.
            if stderr.contains("can't find window") || stderr.contains("window not found") {
                return Ok(());
            }
            bail!(
                "tmux kill-window exited {} for {}: {}",
                output.status,
                target,
                stderr.trim()
            );
        }
        Ok(())
    }

    /// Resolve the session's own agent window and return the OS pid of its pane's
    /// foreground process (`#{pane_pid}`), or `None` when the window is gone or the
    /// pid can't be read.
    ///
    /// One-shot on the local socket. Used by the force-teardown path to reap a live
    /// pane process **before** removing its cwd on Windows, where a directory that
    /// is a live process's cwd cannot be removed (`os error 32`); Unix permits it,
    /// so callers only need the returned pid on Windows.
    pub(crate) fn window_pane_pid(
        &self,
        session_id: &str,
        session_name: &str,
    ) -> Result<Option<u32>> {
        let Some(target) = self.agent_target(session_id, session_name) else {
            return Ok(None);
        };
        let output = self
            .oneshot(&["display-message", "-p", "-t", &target, "#{pane_pid}"])
            .output()
            .context("Failed to run tmux display-message for pane pid")?;
        if !output.status.success() {
            // No such window (already torn down) — not an error for the caller.
            return Ok(None);
        }
        let stdout = String::from_utf8_lossy(&output.stdout);
        Ok(stdout.trim().parse::<u32>().ok())
    }
}

/// The `SessionBackend` pane operations, for both concrete mux backends.
///
/// A row whose backend is a host's is reached through that host — its CLI
/// runs these on its own server — so input, capture and adoption are refused
/// here rather than aimed at whatever this machine's server holds under the
/// same name. Rename is the exception: it follows the row's windows on the
/// host's server over the transport, as it always has.
impl<T: MuxDialect> MuxBackend<T> {
    fn here_only(&self, session_name: &str) -> Result<()> {
        match &self.host {
            Some(host) => bail!(
                "session '{session_name}' runs on host '{}'; its pane is reached through that host",
                host.name
            ),
            None => Ok(()),
        }
    }

    pub(crate) fn session_send_text(
        &self,
        session_id: &str,
        session_name: &str,
        text: &str,
        submit: bool,
    ) -> Result<()> {
        self.here_only(session_name)?;
        self.send_text_now(session_id, session_name, text, submit)
    }

    pub(crate) fn session_send_key(
        &self,
        session_id: &str,
        session_name: &str,
        key: &str,
    ) -> Result<()> {
        self.here_only(session_name)?;
        let Some(resolved) = resolve_key(key) else {
            bail!("unknown key '{key}'");
        };
        self.send_key_now(session_id, session_name, &resolved.tmux)
    }

    pub(crate) fn session_send_text_after(
        &self,
        session_id: &str,
        session_name: &str,
        text: &str,
        delay_secs: u64,
    ) -> Result<()> {
        self.here_only(session_name)?;
        self.send_prompt_after_delay(session_id, session_name, text, delay_secs)
    }

    pub(crate) fn session_capture_text(
        &self,
        session_id: &str,
        session_name: &str,
        lines: u32,
        ansi: bool,
    ) -> Result<String> {
        self.here_only(session_name)?;
        self.capture_pane_text(session_id, session_name, lines, ansi)
    }

    pub(crate) fn session_pane_state(&self, session_id: &str, session_name: &str) -> PaneState {
        match self.host {
            Some(_) => PaneState::default(),
            None => self.pane_state(session_id, session_name),
        }
    }

    pub(crate) fn session_pane_path(&self, session_id: &str, session_name: &str) -> PanePath {
        match self.host {
            Some(_) => PanePath::Unknown,
            None => self.agent_pane_path(session_id, session_name),
        }
    }

    pub(crate) fn session_has_window(&self, session_id: &str, session_name: &str) -> bool {
        self.host.is_none() && self.window_exists(session_id, session_name)
    }

    pub(crate) fn session_rename_windows(
        &self,
        session_id: &str,
        from: &str,
        to: &str,
    ) -> Result<()> {
        if let Some(host) = &self.host {
            known_host_socket(host)?;
        }
        rename_session_windows_on(self, session_id, from, to)
    }

    pub(crate) fn session_claim_running_window(
        &self,
        session_id: &str,
        session_name: &str,
    ) -> Result<Option<String>> {
        self.here_only(session_name)?;
        let index = WindowIndex::from_listing(self.discover_answered()?);
        let Some(pane) = index.live_agent_window(session_id, session_name).pane() else {
            return Ok(None);
        };
        self.stamp_oneshot(&pane, session_id, WindowRole::Agent);
        Ok(Some(pane))
    }
}

/// The platform's local multiplexer on thurbox's own socket — what the
/// platform-level one-shots below (and callers with no row to route by) act on.
fn platform() -> MuxBackend<LocalMuxTransport> {
    MuxBackend::<LocalMuxTransport>::local()
}

/// `MuxBackend::stamp_oneshot` on the platform's local server, for a caller with no
/// session row to route by.
pub fn stamp_local_window(target: &str, session_id: &str, role: WindowRole) {
    platform().stamp_oneshot(target, session_id, role)
}

/// `MuxBackend::window_index` on the platform's local server, for a caller with no
/// session row to route by.
pub fn local_window_index() -> Result<WindowIndex> {
    platform().window_index()
}

/// `MuxBackend::send_text_now` on the platform's local server, for a caller with no
/// session row to route by.
pub fn send_text_now(session_id: &str, session_name: &str, text: &str, submit: bool) -> Result<()> {
    platform().send_text_now(session_id, session_name, text, submit)
}

/// `MuxBackend::window_exists` on the platform's local server, for a caller with no
/// session row to route by.
pub fn window_exists(session_id: &str, session_name: &str) -> bool {
    platform().window_exists(session_id, session_name)
}

/// `MuxBackend::spawn_window` on the platform's local server, for a caller with no
/// session row to route by.
pub fn spawn_window(
    session_id: &str,
    session_name: &str,
    command: &str,
    args: &[String],
    cwd: Option<&Path>,
    env: &HashMap<String, String>,
) -> Result<String> {
    platform().spawn_window(session_id, session_name, command, args, cwd, env)
}

/// `MuxBackend::kill_window` on the platform's local server, for a caller with no
/// session row to route by.
pub fn kill_window(session_id: &str, session_name: &str) -> Result<()> {
    platform().kill_window(session_id, session_name)
}

/// `MuxBackend::kill_shell_window` on the platform's local server, for a caller with no
/// session row to route by.
pub fn kill_shell_window(session_id: &str, session_name: &str) -> Result<()> {
    platform().kill_shell_window(session_id, session_name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::control_mode::{
        decode_octal, format_send_keys, parse_notification, shell_escape, Notification,
    };
    use crate::agent::psmux::PsmuxTransport;
    use crate::agent::tmux::TmuxTransport;

    #[test]
    fn local_backend_uses_the_platform_mux_binary() {
        assert_eq!(<LocalMuxTransport as MuxTransport>::BINARY, DEFAULT_MUX);
    }

    #[test]
    fn remote_headless_spawn_keeps_created_pane_when_stamping_fails() {
        let pane_id = "%42".to_string();
        let result = complete_remote_headless_spawn(
            pane_id.clone(),
            Err(anyhow::anyhow!("control connection dropped")),
            "agent",
        );
        assert_eq!(result.unwrap(), pane_id);
    }

    /// Asked of the dialect, not of the host's preference: a tmux route to a
    /// host configured for psmux still hears its windows close.
    #[test]
    fn psmux_surveys_live_panes_when_close_notifications_are_unavailable() {
        let host = crate::session::HostDef {
            name: "windows".into(),
            multiplexer: Some("psmux".into()),
            ..Default::default()
        };
        assert!(MuxBackend::<PsmuxTransport>::from_host(&host).needs_liveness_poll());
        assert!(MuxBackend::<PsmuxTransport>::local().needs_liveness_poll());
        assert!(!MuxBackend::<TmuxTransport>::from_host(&host).needs_liveness_poll());
        assert!(!MuxBackend::<TmuxTransport>::local().needs_liveness_poll());
    }

    /// The one distinction the remote teardown rests on. Each answer below is
    /// what tmux 3.5/3.7 actually printed when asked for a listing it could not
    /// give (captured against the linux-container e2e host); each failure below
    /// is a question that was never answered. Reading the second group as the
    /// first is what let a force delete against a host that was down for a
    /// minute report nothing to kill and leave the agent running there.
    #[test]
    fn only_the_multiplexers_own_refusal_counts_as_an_empty_answer() {
        let answers = [
            "error connecting to /tmp/tmux-0/thurbox (No such file or directory)",
            "no server running on /tmp/tmux-0/thurbox",
            "can't find session: thurbox",
            "session not found: thurbox",
        ];
        for answer in answers {
            assert!(
                mux_answered_absent(answer),
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
            assert!(!mux_answered_absent(failure), "nothing answered: {failure}");
        }
    }

    /// The trap `error connecting to` sets, and the reason it is not a prefix
    /// match. tmux prints it both for a socket that is not there and for one it
    /// cannot open **while a server is alive behind it** — the `(Permission
    /// denied)` line below is what a live server on another user's socket
    /// actually prints (reproduced by chmod-ing a running server's socket dir).
    /// Reading that as absence is a reachable failure mistaken for "nothing to
    /// kill", which is the orphan this whole path exists to prevent.
    #[test]
    fn a_socket_that_cannot_be_opened_is_not_a_server_that_is_not_there() {
        assert!(
            mux_answered_absent(
                "error connecting to /tmp/tmux-0/thurbox (No such file or directory)"
            ),
            "no socket at all is the one reason that means absence"
        );
        for live in [
            "error connecting to /tmp/tmux-1000/thurbox (Permission denied)",
            "error connecting to /tmp/tmux-1000/thurbox (Connection refused)",
            "error connecting to /tmp/tmux-1000/thurbox (Connection reset by peer)",
        ] {
            assert!(
                !mux_answered_absent(live),
                "a server may be alive behind this socket: {live}"
            );
        }
    }

    /// Layer before text. `ssh` exits 255 for its own failures and passes a
    /// remote command's status through untouched, so 255 means the question
    /// never arrived — even when the bytes on stderr happen to read exactly
    /// like tmux answering, which is the case no amount of message-matching
    /// can get right on its own.
    #[test]
    fn ssh_failing_on_its_own_account_is_never_absence() {
        let tmux_said_absent = "no server running on /tmp/tmux-0/thurbox";

        assert!(
            listing_is_absence(true, Some(1), tmux_said_absent),
            "ssh passed through tmux's own answer"
        );
        assert!(
            !listing_is_absence(true, Some(255), tmux_said_absent),
            "255 is ssh's own failure; nothing on the host answered"
        );
        assert!(
            listing_is_absence(false, Some(255), tmux_said_absent),
            "without ssh in the path, 255 carries none of that meaning"
        );
        assert!(
            !listing_is_absence(true, Some(127), "bash: tmux: command not found"),
            "reached the host, but nothing there answered the question"
        );
        assert!(
            !listing_is_absence(true, None, tmux_said_absent),
            "killed by a signal: no status to reason from"
        );
    }

    // The control-mode primitives are re-exported through this module. Their
    // behavior is covered exhaustively in `control_mode`'s own test module;
    // this single smoke check just asserts the re-export path still resolves
    // (the per-case bodies that used to be duplicated here added no coverage).
    #[test]
    fn control_mode_reexports_resolve() {
        assert_eq!(shell_escape("hello world"), "'hello world'");
        assert_eq!(decode_octal("\\033"), vec![27]);
        assert_eq!(format_send_keys("%1", b"A"), "send-keys -t %1 -H 41\n");
        assert_eq!(
            parse_notification("%pause %1"),
            Notification::Pause {
                pane_id: "%1".to_string()
            }
        );
    }

    // --- path_led_by (the PATH a pane is handed) ---

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

    /// Promoted rather than prepended again: the pane must not be handed a
    /// `PATH` naming one directory twice.
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

    /// An unset `PATH` is not an error: the directory alone is a better answer
    /// than declining, and is exactly what the pane needs.
    #[cfg(not(windows))]
    #[test]
    fn an_empty_path_becomes_the_cli_directory_alone() {
        let led =
            path_led_by(Path::new("/opt/thurbox/bin"), std::ffi::OsStr::new("")).expect("joinable");
        assert_eq!(led, std::ffi::OsString::from("/opt/thurbox/bin"));
    }

    // --- path_from_prefix (reading a pane's PATH back) ---

    #[test]
    fn a_window_command_opening_with_env_yields_its_path() {
        assert_eq!(
            path_from_prefix("/usr/bin/env PATH=/opt/tbx:/usr/bin /usr/bin/sh -c \"sleep 30\""),
            Some("/opt/tbx:/usr/bin".to_string())
        );
    }

    /// Anchored at the second token: a `PATH=` further along is an argument of
    /// the agent's own, and reading it as the pane's environment would be a
    /// confident wrong answer.
    #[test]
    fn a_path_assignment_elsewhere_in_the_command_is_not_the_panes() {
        assert_eq!(path_from_prefix("/usr/bin/claude --env PATH=/nope"), None);
        assert_eq!(path_from_prefix("/usr/bin/claude"), None);
        assert_eq!(path_from_prefix(""), None);
    }

    /// The shape a **command session** produces, copied from a real
    /// `#{pane_start_command}`: one token for the whole command, quoted by
    /// tmux. The opening quote rides on the program, which this discards.
    #[test]
    fn a_quoted_single_token_command_still_yields_its_path() {
        assert_eq!(
            path_from_prefix("\"/usr/bin/env PATH=/opt/tbx:/usr/bin sh -c 'sleep 300'\""),
            Some("/opt/tbx:/usr/bin".to_string())
        );
    }

    /// tmux quotes a token holding whitespace, so a `PATH` with a space in a
    /// component reads as unknown rather than as a truncated answer.
    #[test]
    fn a_quoted_prefix_reads_as_unknown() {
        assert_eq!(
            path_from_prefix("/usr/bin/env \"PATH=/opt/my tools:/usr/bin\" /usr/bin/sh"),
            None
        );
    }

    #[test]
    fn resolve_cli_binary_uses_platform_exe_suffix() {
        let p = resolve_cli_binary();
        let name = p.file_name().unwrap().to_string_lossy();
        assert_eq!(name, format!("thurbox-cli{}", std::env::consts::EXE_SUFFIX));
    }

    // --- local command resolution ---

    /// Best-effort: a name nothing on `PATH` matches is passed through, so a
    /// shell function, an alias, or a binary installed after this ran keeps
    /// working exactly as it did before.
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
    fn backend_default_has_no_control_mode() {
        let backend = MuxBackend::<TmuxTransport>::local();
        let guard = backend.control.lock().unwrap();
        assert!(guard.is_none());
    }

    #[test]
    fn local_backend_is_named_local_tmux_with_local_transport() {
        let backend = MuxBackend::<TmuxTransport>::local();
        assert_eq!(backend.name(), "local-tmux");
        assert!(!backend.transport.is_remote());
    }

    #[test]
    fn from_host_builds_named_ssh_backend() {
        let host = crate::session::HostDef {
            name: "devbox".into(),
            destination: "me@devbox".into(),
            ssh_opts: vec!["-o".into(), "ControlMaster=auto".into()],
            ..Default::default()
        };
        let backend = MuxBackend::<TmuxTransport>::from_host(&host);
        assert_eq!(backend.name(), "ssh:devbox");
        assert!(backend.transport.is_remote());
        // Falls back to the default socket/session when the host omits them.
        assert_eq!(backend.socket, TMUX_SOCKET);
        assert_eq!(backend.session, TMUX_SESSION);
    }

    #[test]
    fn from_host_builds_named_wsl_backend() {
        let host = crate::session::HostDef::wsl("Ubuntu");
        let backend = MuxBackend::<TmuxTransport>::from_host(&host);
        assert_eq!(backend.name(), "wsl:Ubuntu");
        assert!(backend.transport.is_remote());
        assert!(matches!(
            backend.transport,
            TmuxTransport::Wsl { ref distro, .. } if distro == "Ubuntu"
        ));
        assert_eq!(backend.socket, TMUX_SOCKET);
        assert_eq!(backend.session, TMUX_SESSION);
    }

    #[test]
    fn the_own_socket_path_is_the_first_field_of_tmux_env() {
        assert_eq!(
            own_socket_path("/tmp/tmux-1000/thurbox,4242,0").as_deref(),
            Some("/tmp/tmux-1000/thurbox")
        );
        assert_eq!(own_socket_path(",1,0"), None);
        assert_eq!(own_socket_path(""), None);
    }

    #[test]
    fn a_learned_socket_is_used_unless_the_host_pins_one() {
        let learned = crate::session::HostDef {
            name: "learned-socket-host".into(),
            destination: "me@h".into(),
            ..Default::default()
        };
        assert_eq!(host_socket(&learned), TMUX_SOCKET);
        learn_host_socket(&learned, "thurbox");
        assert_eq!(host_socket(&learned), "thurbox");
        let pinned = crate::session::HostDef {
            name: "pinned-socket-host".into(),
            destination: "me@h".into(),
            socket: Some("mine".into()),
            ..Default::default()
        };
        learn_host_socket(&pinned, "thurbox");
        assert_eq!(host_socket(&pinned), "mine");
    }

    #[test]
    fn a_qualified_psmux_backend_follows_a_socket_learned_after_registration() {
        let host = crate::session::HostDef {
            name: "qualified-socket-host".into(),
            destination: "unused".into(),
            multiplexer: Some("psmux".into()),
            ..Default::default()
        };
        let mut backend = MuxBackend::<TmuxTransport>::from_host(&host);
        backend.set_name(format!("{}:psmux", host.backend_name()));
        let before = backend.socket();
        learn_host_socket(&host, "learned-after-registration");
        assert_ne!(before, "learned-after-registration");
        assert_eq!(backend.socket(), "learned-after-registration");
    }

    #[test]
    fn a_default_instance_keeps_the_build_socket() {
        // The backwards-compatibility guarantee: nothing about an operator's
        // existing instance moves, including one whose `THURBOX_DATA_DIR`
        // merely restates the default (which is what thurbox injects into
        // every session it spawns).
        assert_eq!(socket_for(None, None, None, None), TMUX_SOCKET);
    }

    #[test]
    fn a_relocated_instance_gets_its_own_socket() {
        let lab = socket_for(
            None,
            None,
            Some(Path::new("/tmp/lab/data")),
            Some(Path::new("/tmp/lab/data")),
        );
        let other = socket_for(
            None,
            None,
            Some(Path::new("/tmp/other/data")),
            Some(Path::new("/tmp/other/data")),
        );
        assert_ne!(lab, TMUX_SOCKET, "a relocated instance leaves the default");
        assert_ne!(other, lab, "two of them do not share a server");
        assert_eq!(
            lab,
            socket_for(
                None,
                None,
                Some(Path::new("/tmp/lab/data")),
                Some(Path::new("/tmp/lab/data"))
            ),
            "and it finds the same server on the next run"
        );
        assert!(
            lab.starts_with(TMUX_SOCKET),
            "still recognisable as thurbox's: {lab}"
        );
        assert!(
            lab.chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.')),
            "safe to splice into a `-L` argument: {lab}"
        );
    }

    #[test]
    fn a_relocated_socket_ignores_separator_noise() {
        // One directory named two ways is one instance — otherwise a script
        // with a trailing slash would strand the sessions of one without it.
        assert_eq!(
            socket_for(
                None,
                None,
                Some(Path::new("/tmp/lab/data")),
                Some(Path::new("/tmp/lab/data"))
            ),
            socket_for(
                None,
                None,
                Some(Path::new("/tmp/lab/./data/")),
                Some(Path::new("/tmp/lab/./data/"))
            ),
        );
    }

    #[test]
    fn an_explicit_socket_wins_over_the_derivation() {
        assert_eq!(
            socket_for(
                Some("thurbox-named".into()),
                None,
                Some(Path::new("/tmp/lab")),
                Some(Path::new("/tmp/lab"))
            ),
            "thurbox-named"
        );
        // Empty is unset, and then the relocation still applies.
        assert_eq!(
            socket_for(
                Some(String::new()),
                None,
                Some(Path::new("/tmp/lab")),
                Some(Path::new("/tmp/lab"))
            ),
            socket_for(
                None,
                None,
                Some(Path::new("/tmp/lab")),
                Some(Path::new("/tmp/lab"))
            )
        );
    }

    #[test]
    fn an_inherited_socket_is_dropped_once_the_data_dir_moves() {
        let lab = Path::new("/tmp/lab");
        let home = Path::new("/home/me/.local/share/thurbox");
        // What a pane carries: the spawning instance's socket, tagged with the
        // data dir it belongs to. A child that stays put keeps it...
        assert_eq!(
            socket_for(Some("thurbox".into()), Some(home), Some(home), None),
            "thurbox"
        );
        // ...and one that relocates itself does not: the tag no longer names
        // where this instance's database is, so the name is somebody else's
        // server and the derivation has to run instead.
        assert_eq!(
            socket_for(Some("thurbox".into()), Some(home), Some(lab), Some(lab)),
            derived_socket(lab)
        );
        // An override with no tag at all is an operator naming a server
        // outright, which still wins over everything.
        assert_eq!(
            socket_for(Some("thurbox-named".into()), None, Some(lab), Some(lab)),
            "thurbox-named"
        );
    }

    #[test]
    fn local_socket_honors_env_override() {
        // nextest runs one process per test, so env mutation can't race other
        // tests reading `local_socket()`.
        //
        // The owner tag has to go first. `cargo test` runs inside a live
        // thurbox session on any developer machine, and that session injects
        // the pair — so an inherited `THURBOX_SOCKET_FOR` naming the operator's
        // data dir would make the override below read as inherited rather than
        // typed, and `local_socket()` would derive a socket instead of
        // honouring it.
        std::env::remove_var(SOCKET_OWNER_ENV);
        std::env::set_var(SOCKET_OVERRIDE_ENV, "thurbox-lab-test");
        assert_eq!(local_socket(), "thurbox-lab-test");
        assert_eq!(
            MuxBackend::<TmuxTransport>::local().socket,
            "thurbox-lab-test"
        );
        // Empty counts as unset — a sandbox script exporting `THURBOX_SOCKET=`
        // must not produce `-L ''`.
        std::env::set_var(SOCKET_OVERRIDE_ENV, "");
        assert_eq!(local_socket(), TMUX_SOCKET);
        std::env::remove_var(SOCKET_OVERRIDE_ENV);
        assert_eq!(local_socket(), TMUX_SOCKET);
    }

    #[test]
    fn default_shell_matches_host_os_not_local() {
        // The local $SHELL (e.g. /bin/zsh) may not exist on the host: a remote
        // Windows pane got "CommandNotFoundException", a zsh-less Linux host a
        // dead pane. Remote backends pick by transport.
        let winbox = MuxBackend::<PsmuxTransport>::from_host(&crate::session::HostDef {
            name: "winbox".into(),
            destination: "me@winbox".into(),
            multiplexer: Some("psmux".into()),
            ..Default::default()
        });
        assert_eq!(winbox.default_shell(), "powershell");

        let devbox = MuxBackend::<TmuxTransport>::from_host(&crate::session::HostDef {
            name: "devbox".into(),
            destination: "me@devbox".into(),
            ..Default::default()
        });
        assert_eq!(devbox.default_shell(), "/bin/sh");

        let wsl = MuxBackend::<TmuxTransport>::from_host(&crate::session::HostDef::wsl("Ubuntu"));
        assert_eq!(wsl.default_shell(), "/bin/sh");

        // Local keeps the platform default ($SHELL / %COMSPEC%).
        let local = MuxBackend::<TmuxTransport>::local();
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

    // --- named keys ---

    #[test]
    fn resolve_key_maps_the_named_table_to_tmux_names() {
        for (name, tmux) in NAMED_KEYS {
            let resolved = resolve_key(name).expect("a listed key must resolve");
            assert_eq!(resolved.name, *name);
            assert_eq!(resolved.tmux, *tmux);
        }
    }

    #[test]
    fn resolve_key_accepts_the_spellings_people_write() {
        // Separator, prefix and case are the three things spelled differently;
        // all four forms are the one key, and the canonical name is what the
        // caller gets back to depend on.
        for spelling in ["ctrl-c", "ctrl+c", "C-c", "c+C", " CTRL-C "] {
            let resolved = resolve_key(spelling).expect("{spelling} should resolve");
            assert_eq!(resolved.name, "ctrl-c");
            assert_eq!(resolved.tmux, "C-c");
        }
        // Aliases collapse onto one canonical spelling too.
        assert_eq!(resolve_key("esc").unwrap().name, "escape");
        assert_eq!(resolve_key("RETURN").unwrap().name, "enter");
        assert_eq!(resolve_key("pgup").unwrap().tmux, "PageUp");
    }

    #[test]
    fn resolve_key_refuses_what_tmux_would_type_as_text() {
        // tmux does not validate key names — an unrecognized one is injected
        // into the pane as literal text — so anything outside the table must
        // fail here rather than land in an agent's prompt.
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
            assert!(resolve_key(bad).is_none(), "{bad:?} should not resolve");
        }
    }

    #[test]
    fn resolve_key_covers_every_control_letter() {
        for letter in 'a'..='z' {
            let resolved = resolve_key(&format!("ctrl-{letter}")).expect("ctrl-<letter>");
            assert_eq!(resolved.tmux, format!("C-{letter}"));
        }
    }

    #[test]
    fn from_host_honors_socket_and_session_overrides() {
        let host = crate::session::HostDef {
            name: "vm".into(),
            destination: "vm".into(),
            socket: Some("tb-vm".into()),
            session: Some("sess-vm".into()),
            ..Default::default()
        };
        let backend = MuxBackend::<TmuxTransport>::from_host(&host);
        assert_eq!(backend.socket, "tb-vm");
        assert_eq!(backend.session, "sess-vm");
    }

    #[test]
    fn a_shareable_host_that_has_not_said_which_socket_it_uses_is_refused() {
        // A remote teardown used to fall back on this build's own socket name.
        // On a host running its own thurbox that is a guess about somebody
        // else's machine — a dev build aims at `thurbox-dev` while the host's
        // release binary runs `thurbox`, and a relocated data dir derives a
        // name of its own — so the teardown acted on an empty server, and
        // `ensure_ready` created one there while it was at it.
        let host = crate::session::HostDef {
            name: "devbox".into(),
            destination: "me@devbox".into(),
            ..Default::default()
        };
        let refusal = format!("{:#}", known_host_socket(&host).unwrap_err());
        assert!(
            refusal.contains("socket unknown for host 'devbox'"),
            "{refusal}"
        );

        // Sharing off: nothing but this thurbox writes there, so its own socket
        // is the host's by construction.
        let solo = crate::session::HostDef {
            share_sessions: false,
            ..host.clone()
        };
        assert_eq!(known_host_socket(&solo).unwrap(), TMUX_SOCKET);

        // And a pinned socket answers without asking anyone.
        let pinned = crate::session::HostDef {
            socket: Some("thurbox".into()),
            ..host
        };
        assert_eq!(known_host_socket(&pinned).unwrap(), "thurbox");
    }

    // Compile-time check: channel capacity must be large enough to buffer heavy output.
    const _: () = assert!(PANE_CHANNEL_CAPACITY >= 1024);

    #[test]
    fn env_flag_simple_value() {
        // Simple key=value should not be quoted.
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
        // Values with spaces must be quoted as a single KEY=VALUE unit.
        let env_part: String = [("MSG".to_string(), "hello world".to_string())]
            .into_iter()
            .collect::<std::collections::HashMap<_, _>>()
            .iter()
            .map(|(k, v)| format!(" -e {}", shell_escape(&format!("{k}={v}"))))
            .collect();
        assert_eq!(env_part, " -e 'MSG=hello world'");
    }

    // --- window-name sanitization tests ---

    #[test]
    fn sanitize_window_name_passes_through_safe_chars() {
        assert_eq!(sanitize_window_name("abc-123_XYZ"), "abc-123_XYZ");
    }

    #[test]
    fn sanitize_window_name_replaces_spaces() {
        // Bug: session names with spaces broke `tmux send-keys` / capture
        // because the target string `session:window with spaces` was
        // re-split by tmux into `session`, `window`, `with`, `spaces`.
        assert_eq!(sanitize_window_name("Foo Bar"), "Foo_Bar");
    }

    #[test]
    fn sanitize_window_name_replaces_tmux_delimiters() {
        // Colons, dots, commas all have meaning inside tmux target strings.
        assert_eq!(sanitize_window_name("a:b.c,d"), "a_b_c_d");
    }

    #[test]
    fn sanitize_window_name_replaces_non_ascii() {
        assert_eq!(sanitize_window_name("café"), "caf_");
    }

    #[test]
    fn agent_and_shell_window_names_share_sanitization() {
        assert_eq!(agent_window_name("Foo Bar"), "tb-Foo_Bar");
        assert_eq!(shell_window_name("Foo Bar"), "tbs-Foo_Bar");
    }

    /// A plugin's pane gets a prefix of its own, and its name is deterministic —
    /// which is the entire mechanism for finding the window again after a restart.
    #[test]
    fn a_program_window_is_named_deterministically_and_apart_from_sessions() {
        let once = program_window_name("abcd1234", "watch");
        assert_eq!(once, "tbp-abcd1234-watch");
        assert_eq!(
            once,
            program_window_name("abcd1234", "watch"),
            "deterministic"
        );

        // Distinct prefix, so window discovery cannot adopt one as a session's
        // agent (`tb-`) or its companion shell (`tbs-`).
        assert!(once.starts_with(PROGRAM_WINDOW_PREFIX));
        assert!(!once.starts_with(&format!("{WINDOW_PREFIX}a")));
        assert!(!once.starts_with(SHELL_WINDOW_PREFIX));
    }

    /// Only an agent's window keeps its corpse — and the answer is read off the
    /// *name*, so it is pinned against the three name builders rather than
    /// against hand-written prefixes that could drift from them.
    #[test]
    fn an_agent_window_keeps_its_corpse_and_the_other_two_do_not() {
        assert!(keeps_dead_pane(&agent_window_name("Foo Bar")));
        assert!(!keeps_dead_pane(&shell_window_name("Foo Bar")));
        assert!(!keeps_dead_pane(&program_window_name("abcd1234", "watch")));
        // A window thurbox did not create is not thurbox's to keep open either.
        assert!(!keeps_dead_pane("zsh"));
    }

    /// `remain-on-exit` is a WINDOW option, and `window-size` is one too: neither
    /// can be set for a session, so neither belongs in the session list. Both
    /// are stated as the window is created (`birth_options`).
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

    /// `window-size manual` may be said for a window, never for the server.
    ///
    /// tmux works out a window's size *before* the window exists
    /// (`spawn_window` → `default_window_size(…, w = NULL)`) and the manual
    /// branch of `clients_calculate_size` reads `w->manual_sx` with no NULL
    /// check, so a server whose default is `manual` dies on the next
    /// `new-window` from an unattached client — 3.3 … 3.6. Measured on 3.5a:
    /// `server exited unexpectedly` every time with the server-wide write, a
    /// pane id every time without it. 3.2 and 3.2a have the option too and
    /// survive the server-wide write (measured).
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

    /// A listed window, as `discover` would have reported it.
    fn listed(pane: &str, window: &str, session: &str, role: WindowRole) -> DiscoveredSession {
        DiscoveredSession {
            backend_id: pane.into(),
            name: window.into(),
            is_alive: true,
            session: session.into(),
            role,
        }
    }

    fn dead(mut window: DiscoveredSession) -> DiscoveredSession {
        window.is_alive = false;
        window
    }

    const ONE: &str = "11111111-1111-4111-8111-111111111111";
    const TWO: &str = "22222222-2222-4222-8222-222222222222";

    /// The format is a literal because a `const` cannot interpolate another;
    /// this is what keeps it honest.
    #[test]
    fn the_discover_format_reads_both_stamps() {
        assert!(DISCOVER_FORMAT.contains(&format!("#{{{WINDOW_SESSION_OPTION}}}")));
        assert!(DISCOVER_FORMAT.contains(&format!("#{{{WINDOW_ROLE_OPTION}}}")));
    }

    /// The retirement reads its own listing, so its format is pinned the same
    /// way — and in the order `stamped_windows_in` splits on.
    #[test]
    fn the_retire_format_reads_the_window_and_both_stamps() {
        assert_eq!(
            RETIRE_FORMAT,
            format!("#{{window_id}}|#{{{WINDOW_SESSION_OPTION}}}|#{{{WINDOW_ROLE_OPTION}}}")
        );
    }

    /// The whole rule, on the listing the sweep actually parses. Oldest first,
    /// so the keeper is the last entry — and only this session's `role` windows
    /// are in it at all.
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

    /// A window nothing can place in the order is not one to decide a kill
    /// about — which is also what keeps a multiplexer that hands the format
    /// string back rather than expanding it from being read as a listing.
    #[test]
    fn a_window_id_that_does_not_parse_is_left_out_of_the_order() {
        let listing = format!("#{{window_id}}|{ONE}|agent\n@7|{ONE}|agent\n");
        assert_eq!(
            stamped_windows_in(&listing, ONE, WindowRole::Agent),
            vec!["@7".to_string()]
        );
    }

    /// A pane learns of its own ending only through the window it is in, and
    /// the only free moment to learn which window that is, is the answer to the
    /// command that made it. Dropping `#{window_id}` from here would cost
    /// nothing visible — spawning still works, and the exit simply stops being
    /// announced under load — so it is pinned.
    #[test]
    fn the_spawn_format_asks_for_the_window_too() {
        assert!(SPAWN_FORMAT.contains("#{pane_id}"));
        assert!(SPAWN_FORMAT.contains("#{window_id}"));
        // Parsed by splitting on whitespace, so the two must be separable.
        assert!(SPAWN_FORMAT.split_whitespace().count() == 2);
    }

    /// Both ids are read off the wire, so both are checked the same way.
    #[test]
    fn a_window_id_is_an_at_sign_and_digits() {
        assert!(control_mode::is_valid_window_id("@0"));
        assert!(control_mode::is_valid_window_id("@42"));
        assert!(!control_mode::is_valid_window_id("@"));
        assert!(!control_mode::is_valid_window_id("%3"));
        assert!(!control_mode::is_valid_window_id("@3x"));
        assert!(!control_mode::is_valid_window_id(""));
    }

    /// The stamp is the identity, so a window answers to its session whatever
    /// it is called — which is what lets a renamed session's row and window
    /// disagree for the moment between the two writes without losing it.
    #[test]
    fn a_stamped_window_is_its_sessions_whatever_it_is_named() {
        let index =
            WindowIndex::from_listing([listed("%3", "tb-old_name", ONE, WindowRole::Agent)]);
        assert_eq!(
            index.agent_window(ONE, "new name"),
            Located::At("%3".into())
        );
    }

    /// The bug this whole mechanism exists for: the only `tb-fleet` on the
    /// server belongs to a live namesake, and a teardown resolving the name
    /// would kill it.
    #[test]
    fn a_namesakes_stamped_window_is_never_ours() {
        let index = WindowIndex::from_listing([listed("%7", "tb-fleet", TWO, WindowRole::Agent)]);
        assert_eq!(index.agent_window(ONE, "fleet"), Located::Absent);
    }

    /// The same answer when the row remembers that very pane id — which is the
    /// state a tmux server restart leaves behind, since it reissues ids from
    /// `%0`. Nothing here consults the remembered id, and that is the point.
    #[test]
    fn a_reissued_pane_id_cannot_make_a_namesakes_window_ours() {
        let index = WindowIndex::from_listing([
            listed("%1", "tb-fleet", TWO, WindowRole::Agent),
            listed("%2", "tb-other", ONE, WindowRole::Agent),
        ]);
        // `%1` is what the stale row remembers; it resolves to its own window.
        assert_eq!(index.agent_window(ONE, "fleet"), Located::At("%2".into()));
    }

    /// The migration path: a window spawned before windows were stamped, and
    /// every window under a multiplexer that has no window options.
    #[test]
    fn a_lone_unstamped_namesake_is_adoptable() {
        let index = WindowIndex::from_listing([listed("%4", "tb-fleet", "", WindowRole::Agent)]);
        assert_eq!(index.agent_window(ONE, "fleet"), Located::At("%4".into()));
        // And to a caller with no id of its own at all.
        assert_eq!(index.agent_window("", "fleet"), Located::At("%4".into()));
    }

    /// Ambiguity is not absence. Reading it as absence is what relaunches a
    /// session that is already running, so two colliding windows become three.
    #[test]
    fn unstamped_namesakes_are_unknown_rather_than_absent() {
        let index = WindowIndex::from_listing([
            listed("%1", "tb-fleet", "", WindowRole::Agent),
            listed("%2", "tb-fleet", "", WindowRole::Agent),
        ]);
        assert_eq!(index.agent_window(ONE, "fleet"), Located::Unknown);
        assert!(!index.agent_window(ONE, "fleet").is_absent());
    }

    /// Once every candidate carries a stamp there is no ambiguity left: none of
    /// them is ours, so the window really is gone and a relaunch is right.
    #[test]
    fn stamped_namesakes_that_are_all_someone_elses_read_as_absent() {
        let index = WindowIndex::from_listing([
            listed("%1", "tb-fleet", TWO, WindowRole::Agent),
            listed("%2", "tb-fleet", TWO, WindowRole::Agent),
        ]);
        assert_eq!(index.agent_window(ONE, "fleet"), Located::Absent);
    }

    /// A session stamps both of its windows with the same id, so the role is
    /// what stops its shell being resolved — and killed — as its agent.
    #[test]
    fn a_sessions_shell_window_is_not_its_agent() {
        let index = WindowIndex::from_listing([listed("%5", "tbs-fleet", ONE, WindowRole::Shell)]);
        assert_eq!(index.agent_window(ONE, "fleet"), Located::Absent);
        assert_eq!(index.shell_window(ONE, "fleet"), Located::At("%5".into()));
    }

    /// The rule a teardown reads the companion shell by. `shell_backend_id` is
    /// written only once the interface has opened one, so most rows carry no id
    /// for their shell at all and the stamp is the whole answer — including the
    /// answer that a namesake's shell is *not* this session's to kill.
    #[test]
    fn a_shell_window_is_owned_by_its_stamp_the_way_an_agent_window_is() {
        let theirs = WindowIndex::from_listing([listed("%5", "tbs-fleet", TWO, WindowRole::Shell)]);
        assert_eq!(theirs.shell_window(ONE, "fleet"), Located::Absent);

        // Unstamped and alone: the pre-ADR-25 shape, and psmux's permanent one.
        let legacy = WindowIndex::from_listing([listed("%5", "tbs-fleet", "", WindowRole::Shell)]);
        assert_eq!(legacy.shell_window(ONE, "fleet"), Located::At("%5".into()));

        // Two of them, neither stamped: nobody can say, and a teardown that
        // guessed would take a live session's shell down.
        let ambiguous = WindowIndex::from_listing([
            listed("%5", "tbs-fleet", "", WindowRole::Shell),
            listed("%6", "tbs-fleet", "", WindowRole::Shell),
        ]);
        assert_eq!(ambiguous.shell_window(ONE, "fleet"), Located::Unknown);
    }

    /// `remain-on-exit` keeps a failed agent's window in place so the error is
    /// readable. It is still the session's own window — the interface attaches
    /// to it — but it is not a *running* agent, which is the question every
    /// relaunch gate asks.
    #[test]
    fn a_dead_pane_is_still_the_sessions_window_but_not_a_live_one() {
        let index =
            WindowIndex::from_listing([dead(listed("%6", "tb-fleet", ONE, WindowRole::Agent))]);
        assert_eq!(index.agent_window(ONE, "fleet"), Located::At("%6".into()));
        assert_eq!(index.live_agent_window(ONE, "fleet"), Located::Absent);
        assert!(index.places_agent(ONE, "fleet", "%6"));
        assert!(!index.places_agent(ONE, "fleet", "%9"));
    }

    /// A dead own window must never be treated as "no stamped window at
    /// all" — that reading is what let a live unstamped namesake, sharing
    /// this session's `tb-<name>`, get attributed to this session instead.
    #[test]
    fn a_dead_own_window_does_not_fall_through_to_a_live_namesake() {
        let index = WindowIndex::from_listing([
            dead(listed("%6", "tb-fleet", ONE, WindowRole::Agent)),
            listed("%7", "tb-fleet", "", WindowRole::Agent),
        ]);
        assert_eq!(index.live_agent_window(ONE, "fleet"), Located::Absent);
    }

    /// A remembered pane id still resolves among *unstamped* namesakes, which
    /// is how two pre-ADR-25 sessions sharing a name each attach to their own
    /// pane. It stops resolving the moment a window says whose it is: a stamp
    /// for somebody else is what a reissued pane id turns into.
    #[test]
    fn a_remembered_pane_is_claimable_while_no_window_says_otherwise() {
        let index = WindowIndex::from_listing([
            listed("%1", "tb-fleet", "", WindowRole::Agent),
            listed("%2", "tb-fleet", "", WindowRole::Agent),
        ]);
        assert!(index.places_agent(ONE, "fleet", "%1"));
        assert!(index.places_agent(TWO, "fleet", "%2"));
        // Ambiguous for a caller with nothing but the name to go on.
        assert_eq!(index.agent_window(ONE, "fleet"), Located::Unknown);

        let stamped = WindowIndex::from_listing([listed("%1", "tb-fleet", TWO, WindowRole::Agent)]);
        assert!(!stamped.places_agent(ONE, "fleet", "%1"));
        assert!(stamped.places_agent(TWO, "fleet", "%1"));
        // And a pane nothing listed is nobody's.
        assert!(!stamped.places_agent(TWO, "fleet", "%9"));
    }

    /// Anyone can set a window option, and a multiplexer that does not expand
    /// `#{@...}` hands the format string straight back — so only a stamp that
    /// is a session id is believed.
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

    /// A listing that stops short — the psmux divergence (ADR-13) — reads as an
    /// unstamped window rather than being dropped, and the prefix still says
    /// what the window is.
    #[test]
    fn a_listing_without_the_stamp_fields_still_discovers_the_window() {
        let parsed = parse_discovered("%1|tbs-fleet|0", true).expect("parsed");
        assert_eq!(parsed.session, "");
        assert_eq!(parsed.role, WindowRole::Shell);
        assert!(parsed.is_alive);
        assert!(parse_discovered("%1|someone-elses|0", true).is_none());
        assert!(parse_discovered("not-a-pane|tb-fleet|0", true).is_none());
    }

    /// psmux has **no per-window options**: `set-option -w -t <pane> @k v`
    /// writes a *global* one, and `#{@k}` then expands to it on every window
    /// (measured on a Windows host, psmux 3.3.6 — ADR-13). So the stamp thurbox
    /// wrote for one session is handed back as every window's, and both readings
    /// of that lose the pane: the session it names sees several windows claiming
    /// it, and every *other* session sees its own window claiming somebody else.
    /// Both end at "session has no pane yet", which is the whole of Windows
    /// being unattachable from the second session on.
    #[test]
    fn a_global_stamp_is_not_read_as_every_windows_identity() {
        let listing = |stamps: bool| {
            WindowIndex::from_listing([
                parse_discovered(&format!("%1|tb-first|0|{TWO}|agent"), stamps).expect("parsed"),
                parse_discovered(&format!("%3|tb-second|0|{TWO}|agent"), stamps).expect("parsed"),
            ])
        };

        // A multiplexer whose `#{@...}` is per-window is believed, so one id on
        // two windows is the ambiguity it looks like.
        let stamped = listing(true);
        assert_eq!(stamped.agent_window(TWO, "second"), Located::Unknown);
        assert_eq!(stamped.agent_window(ONE, "first"), Located::Absent);

        // A multiplexer whose `#{@...}` is not per-window says nothing about
        // whose window this is, so the name decides — the pre-ADR-25 shape
        // `MuxDialect::WINDOW_OPTIONS` already keeps for it everywhere else.
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

    /// The role travels on the same global option, so it is dropped with it —
    /// otherwise a session's companion shell reports `agent` and is indexed as
    /// the agent window, which is the same pane confusion one layer down.
    #[test]
    fn a_global_role_never_makes_a_shell_window_an_agent() {
        let shell =
            parse_discovered(&format!("%5|tbs-fleet|0|{ONE}|agent"), false).expect("parsed");
        assert_eq!(shell.role, WindowRole::Shell);
        assert_eq!(shell.session, "");
    }

    /// Which multiplexers those two tests are about, asked where the listing is
    /// read: psmux is the one whose `#{@...}` is not a window's.
    #[test]
    fn only_a_multiplexer_with_window_options_is_read_as_stamping_them() {
        let psmux = crate::session::HostDef {
            name: "winbox".into(),
            destination: "me@winbox".into(),
            multiplexer: Some("psmux".into()),
            ..Default::default()
        };
        let tmux = crate::session::HostDef {
            multiplexer: None,
            ..psmux.clone()
        };
        assert!(!MuxBackend::<PsmuxTransport>::from_host(&psmux).stamps_are_per_window());
        assert!(MuxBackend::<TmuxTransport>::from_host(&tmux).stamps_are_per_window());
        assert_eq!(
            MuxBackend::<LocalMuxTransport>::local().stamps_are_per_window(),
            !cfg!(windows),
            "the local multiplexer is psmux on Windows and tmux elsewhere"
        );
    }

    /// A program window is discovered but can never be adopted as a session's
    /// agent, which is what the role is for.
    ///
    /// Discovery used to filter on the `tb-` prefix, which excluded `tbs-` and
    /// `tbp-` outright — and so hid exactly the windows that make a name
    /// ambiguous. They are listed now; the *role*, not the listing, is what
    /// keeps a plugin's program from resolving as somebody's agent.
    #[test]
    fn a_program_window_is_discovered_but_is_nobodys_agent() {
        let program = program_window_name("abcd1234", "watch");
        assert_eq!(
            WindowRole::from_window_name(&program),
            Some(WindowRole::Program)
        );
        assert_eq!(
            WindowRole::from_window_name(&shell_window_name("s")),
            Some(WindowRole::Shell)
        );
        assert_eq!(
            WindowRole::from_window_name(&agent_window_name("s")),
            Some(WindowRole::Agent)
        );
        assert_eq!(WindowRole::from_window_name("someone-elses"), None);
    }

    /// Why the owner is a **digest** rather than the plugin's path.
    ///
    /// `sanitize_window_name` maps every character outside `[A-Za-z0-9_-]` to
    /// `_`, so two different paths sanitize to one window — and two plugins would
    /// then share a single program. The digest is computed by the caller for
    /// exactly this reason; this pins the hazard that makes it necessary.
    #[test]
    fn sanitizing_a_path_would_collide_which_is_why_the_owner_is_digested() {
        assert_eq!(
            sanitize_window_name("plugins/90_watch.lua"),
            sanitize_window_name("plugins.90.watch.lua"),
            "two distinct paths, one window name — the collision a digest avoids"
        );
        // Digested owners of different paths do not collide.
        assert_ne!(
            program_window_name("aaaa1111", "watch"),
            program_window_name("bbbb2222", "watch")
        );
    }

    #[test]
    fn window_target_uses_exact_match_prefix() {
        // Without `=`, tmux treats the window name as a pattern and will
        // resolve `tb-foo` ambiguously when both `tb-foo` and
        // `tb-foo-bar` exist. The `=` prefix forces exact-match lookup.
        let backend = MuxBackend::<TmuxTransport>::local();
        let t = backend.window_target(&agent_window_name("foo"));
        assert!(t.ends_with(":=tb-foo"), "got {t}");
        let shell = backend.window_target(&shell_window_name("foo"));
        assert!(shell.ends_with(":=tbs-foo"), "got {shell}");
    }

    #[test]
    fn parse_pane_dead_only_accepts_one() {
        assert!(parse_pane_dead("1"));
        assert!(parse_pane_dead("1\n"));
        assert!(!parse_pane_dead("0\n"));

        // A missing window makes `display-message` exit 0 printing nothing.
        // Reading that as dead would mask the `send-keys` "can't find window"
        // error that actually diagnoses it, turning a typo into "has exited".
        assert!(!parse_pane_dead(""));
        assert!(!parse_pane_dead("\n"));

        // Never infer deadness from anything but the flag itself.
        assert!(!parse_pane_dead("10"));
        assert!(!parse_pane_dead("dead"));
    }

    // --- title_seed_bytes tests (adopt-time activity-line restore) ---

    #[test]
    fn title_seed_replays_an_agent_title_as_osc_2() {
        assert_eq!(
            title_seed_bytes("devbox", "\u{2733} Terminal name lost on restart"),
            "\x1b]2;\u{2733} Terminal name lost on restart\x1b\\".as_bytes()
        );
    }

    #[test]
    fn title_seed_suppresses_tmuxs_default_title() {
        // A pane nothing ever titled reads back as the host's own short name.
        assert!(title_seed_bytes("devbox", "devbox").is_empty());
        assert!(title_seed_bytes("devbox", "  devbox  ").is_empty());
        assert!(title_seed_bytes("devbox", "   ").is_empty());
    }

    #[test]
    fn title_seed_drops_control_characters() {
        // The title is remote-controlled text and the sequence it goes into is
        // terminated by an escape, so a title carrying one must not close it.
        let seed = title_seed_bytes("h", "done\x1b\\ + rm -rf\x07\nnext");
        assert_eq!(seed, "\x1b]2;done\\ + rm -rfnext\x1b\\".as_bytes());
    }

    #[test]
    fn title_seed_bounds_a_huge_title() {
        let seed = title_seed_bytes("h", &"\u{00e9}".repeat(4_000));
        // The budget is the payload's; the introducer and terminator sit
        // outside it. Multi-byte chars must not be split to reach it either.
        assert!(seed.len() <= MAX_TITLE_SEED_BYTES + 6, "{}", seed.len());
        assert!(std::str::from_utf8(&seed).is_ok());
    }

    // --- pane state (cursor / foreground process / live cwd) ---

    /// Build the `display-message` answer tmux produces for the format
    /// `pane_state` asks for, so the tests speak in fields rather than bytes.
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
        // Only the `ps` pass can fill this in — the tmux answer never does.
        assert_eq!(state.foreground_command, None);
    }

    #[test]
    fn parse_pane_state_reads_the_answer_an_older_tmux_prints() {
        // Byte for byte what tmux 3.4 — ubuntu-24.04's, and so CI's — answers
        // the same `display-message`: its `vis(3)` pass rewrites the separator
        // to its octal escape, which used to parse as one field and report
        // every pane fact null.
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
        // Why the separator is a control byte and not whitespace: a path may
        // contain spaces, and splitting on them would report half of one.
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
        // A multiplexer that does not know a format expands it to nothing
        // (psmux). An empty string would read downstream as a real answer —
        // a cursor at an unknown row is not a cursor at row 0.
        let (state, tty, _) = parse_pane_state(&pane_state_answer(&["", "", "", "", ""]));
        assert_eq!(state, PaneState::default());
        assert_eq!(tty, None);

        // And a truncated answer leaves the fields it never carried absent
        // rather than shifting later values into earlier slots.
        let (state, tty, _) = parse_pane_state(&pane_state_answer(&["3", "4"]));
        assert_eq!(state.cursor_row, Some(3));
        assert_eq!(state.cursor_col, Some(4));
        assert_eq!(state.foreground_cwd, None);
        assert_eq!(tty, None);
    }

    #[test]
    fn parse_pane_state_reports_which_window_answered() {
        // The field that makes the answer attributable: `display-message`
        // against a target it cannot resolve answers for the client's current
        // pane and exits 0, so without this the caller cannot tell a session's
        // own pane from a stranger's.
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
        // `remain-on-exit=on` keeps a dead pane's frame, and tmux keeps naming
        // the command that died in it — so "what is running here" is only
        // answerable with this flag beside it.
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

        // A multiplexer that does not know the format expands it to nothing,
        // and "not answered" is not "alive".
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
    fn parse_pane_state_survives_a_dead_or_missing_pane() {
        // `display-message` against a window that is gone exits 0 printing an
        // empty line — the same shape `parse_pane_dead` guards against.
        let (state, tty, _) = parse_pane_state("");
        assert_eq!(state, PaneState::default());
        assert_eq!(tty, None);
    }

    #[test]
    fn ps_foreground_prefers_the_group_leader_over_its_pipeline() {
        // `tpgid` is the tty's foreground group; the rows whose own `pgid`
        // equals it are that job, and its leader is the command to report.
        let out = "\
 4210  4210  4300 -bash
 4300  4300  4300 node /opt/cursor-agent/cli.js --resume
 4301  4300  4300 tee /tmp/log
";
        let (argv0, command) = parse_ps_foreground(out).expect("a foreground job");
        assert_eq!(argv0, "node");
        // The whole point of the argv: a bare command *name* is `node` for both
        // an agent CLI and a REPL, and only this tells them apart.
        assert_eq!(command, "node /opt/cursor-agent/cli.js --resume");
    }

    #[test]
    fn ps_foreground_falls_back_to_a_group_member() {
        // The leader can have exited while the rest of its group runs on.
        let out = " 4301  4300  4300 tee /tmp/log\n";
        assert_eq!(
            parse_ps_foreground(out).map(|(argv0, _)| argv0),
            Some("tee".to_string())
        );
    }

    #[test]
    fn ps_foreground_reports_nothing_when_nothing_holds_the_tty() {
        // -1 is "no foreground group"; 0 is `ps` saying it does not know.
        // Neither is a process, and reporting the background shell for either
        // would be a plausible wrong answer rather than an honest absence.
        assert_eq!(parse_ps_foreground(" 4210 4210 -1 -bash\n"), None);
        assert_eq!(parse_ps_foreground(" 4210 4210 0 -bash\n"), None);
        assert_eq!(parse_ps_foreground(""), None);
        // A background job is not the foreground one either.
        assert_eq!(parse_ps_foreground(" 4210 4210 4300 -bash\n"), None);
    }

    #[test]
    fn ps_rows_survive_right_aligned_padding_and_junk() {
        // `ps` pads its numeric columns to the widest value, so a narrow pid
        // arrives behind several spaces — which is what `splitn` mis-parses.
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

    // --- history_seed_bytes tests (adopt-time scrollback seeding) ---

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
        // Feed more lines than the screen height: the overflow must land in
        // the parser's scrollback, scrollable from the UI.
        let mut parser = vt100::Parser::new(5, 80, 100);
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

/// Every dialect answer the shared core relies on, pinned to what the core
/// emitted before the psmux split (`MuxProtocol`, commit `29e17e3`).
///
/// The fixture was written by that commit's own code for these same inputs;
/// what it composed inline (the spawn window command, the resize list, the
/// `new-window` quoting and `-e` arguments) was dumped through a copy of those
/// lines. A dialect that drifts fails here with the key that moved. Unix only:
/// the local one-shot rows are tmux's.
#[cfg(all(test, unix))]
mod dialect_characterization {
    use std::fmt::Write as _;

    use super::*;
    use crate::agent::psmux::PsmuxTransport;
    use crate::agent::tmux::TmuxTransport;

    const BEFORE: &str = include_str!("../../tests/fixtures/mux_dialects_before_split.txt");

    /// Answers that changed on purpose, as `(key, before, after)`.
    const DELIBERATE: &[(&str, &str, &str)] = &[
        // A tmux route polled liveness when its host *preferred* psmux. The
        // dialect answers for the multiplexer actually driven, and the
        // registry builds a host's tmux route with `multiplexer = tmux`.
        (
            "backend tmux@psmuxhost needs_liveness_poll",
            "true",
            "false",
        ),
    ];

    const COMMAND: &str = "/opt/agent/claude";
    const BANNERS: &[&str] = &[
        "tmux 3.4",
        "tmux 2.8",
        "tmux 3.3.6",
        "psmux 3.3.8",
        "3.3.6",
        "",
    ];
    const QUOTED: &[&str] = &["tb-demo", "/tmp/my dir", "it's"];
    const WINDOWS: &[&str] = &["tb-demo", "tbs-demo", "tbp-x-y"];

    fn args() -> Vec<String> {
        vec!["--resume".into(), "it's x".into()]
    }
    fn env2() -> HashMap<String, String> {
        [("B", "x y"), ("A", "1")]
            .into_iter()
            .map(|(k, v)| (k.into(), v.into()))
            .collect()
    }
    fn env1() -> HashMap<String, String> {
        [("A", "1")]
            .into_iter()
            .map(|(k, v)| (k.into(), v.into()))
            .collect()
    }
    fn bufs() -> Vec<Vec<u8>> {
        vec![
            b"ls\r".to_vec(),
            b"\x1b[A".to_vec(),
            b"it's -x 0x41".to_vec(),
            "é".repeat(3).into_bytes(),
            b"\x1b[200~a\nb\x1b[201~".to_vec(),
        ]
    }
    fn verdict(r: Result<()>) -> String {
        match r {
            Ok(()) => "ok".into(),
            Err(e) => format!("err: {e}"),
        }
    }
    fn argv(cmd: &Command) -> Vec<String> {
        std::iter::once(cmd.get_program())
            .chain(cmd.get_args())
            .map(|a| a.to_string_lossy().into_owned())
            .collect()
    }

    fn dump<T: MuxDialect>(out: &mut String, label: &str, b: &MuxBackend<T>) {
        let remote = b.transport.is_remote();
        let mut w = |k: &str, v: String| writeln!(out, "{label} {k} = {v}").unwrap();
        w(
            "mux_command",
            format!(
                "{:?}",
                argv(&b.transport.mux_command("LAUNCH-SOCK", &["-V"]))
            ),
        );
        w(
            "stamps_per_window",
            format!("{}", b.stamps_are_per_window()),
        );
        w("command_lists", format!("{}", T::COMMAND_LISTS));
        w("size_reports", format!("{}", T::SIZE_REPORTS));
        w(
            "implicit_attach",
            format!("{}", T::IMPLICIT_ATTACH_RESPONSE),
        );
        w("strict_blocks", format!("{}", T::STRICT_RESPONSE_BLOCKS));
        w("subscriptions", format!("{}", T::SUBSCRIPTIONS));
        w("asks_server_version", format!("{}", T::ASKS_SERVER_VERSION));
        w(
            "paste_channel",
            format!("{}", b.transport.paste_channel("SOCK".into()).is_some()),
        );
        w(
            "kill_remembered_when_ambiguous",
            format!("{}", !T::WINDOW_OPTIONS),
        );
        w(
            "hook_poll",
            format!("{}", b.transport.hook_poll_command("SESS").is_some()),
        );
        if remote {
            w("default_shell", b.default_shell());
        }
        if remote || !T::WINDOW_OPTIONS {
            let config: Vec<_> = b
                .session_config()
                .iter()
                .map(|o| (o.args.clone(), o.fatal))
                .collect();
            w("session_config", format!("{config:?}"));
        }
        for window in WINDOWS {
            w(
                &format!("window_command {window}"),
                T::window_command(b, window, COMMAND, &args(), &env2()),
            );
            w(
                &format!("birth {window}"),
                format!("{:?}", T::birth_option_commands(window)),
            );
        }
        for s in QUOTED {
            w(&format!("quote_arg {s:?}"), T::quote_arg(s));
        }
        w("env_args", format!("{:?}", T::env_args(&env1())));
        for buf in bufs() {
            w(
                &format!("send_keys {:?}", String::from_utf8_lossy(&buf)),
                format!("{:?}", T::send_keys_commands("%1", &buf)),
            );
        }
        w(
            "resize",
            format!("{:?}", T::resize_commands("%1", 40, 120, "SIZER")),
        );
        w(
            "resize clamped",
            format!("{:?}", T::resize_commands("%1", 0, 20000, "SIZER")),
        );
        w(
            "paste_args",
            format!(
                "{:?}",
                T::paste_args("thurbox:tb-demo", "line one\nline two")
            ),
        );
        for banner in BANNERS {
            w(
                &format!("check_banner {banner:?}"),
                verdict(T::check_banner(banner, "VERSION-SOCK")),
            );
            let server = if T::ASKS_SERVER_VERSION {
                T::check_server_version(banner, "VERSION-SOCK")
            } else {
                Ok(())
            };
            w(&format!("check_server_version {banner:?}"), verdict(server));
        }
        if !T::WINDOW_OPTIONS {
            let mut cmd = Command::new("probe");
            T::push_window_program(&mut cmd, COMMAND, &args(), &env2());
            let pushed: Vec<_> = cmd
                .get_args()
                .map(|a| a.to_string_lossy().into_owned())
                .collect();
            w("push_window_program", format!("{pushed:?}"));
        }
    }

    fn current() -> String {
        let pin = |mut h: crate::session::HostDef| {
            h.socket = Some("SOCK".into());
            h.session = Some("SESS".into());
            h
        };
        let ssh = pin(crate::session::HostDef {
            name: "devbox".into(),
            destination: "me@devbox".into(),
            ..Default::default()
        });
        let win = pin(crate::session::HostDef {
            name: "winbox".into(),
            destination: "me@winbox".into(),
            multiplexer: Some("psmux".into()),
            ..Default::default()
        });
        let wsl = pin(crate::session::HostDef::wsl("Ubuntu"));
        let local_tmux = MuxBackend::<TmuxTransport>::with_transport(
            TmuxTransport::local(),
            "SOCK",
            "SESS",
            "local",
        );
        let local_psmux = MuxBackend::<PsmuxTransport>::with_transport(
            PsmuxTransport::local(),
            "SOCK",
            "SESS",
            "local",
        );
        let mut out = String::new();
        dump(&mut out, "tmux@local", &local_tmux);
        dump(
            &mut out,
            "tmux@ssh",
            &MuxBackend::<TmuxTransport>::from_host(&ssh),
        );
        dump(
            &mut out,
            "tmux@wsl",
            &MuxBackend::<TmuxTransport>::from_host(&wsl),
        );
        dump(
            &mut out,
            "tmux@psmuxhost",
            &MuxBackend::<TmuxTransport>::from_host(&win),
        );
        dump(&mut out, "psmux@local", &local_psmux);
        dump(
            &mut out,
            "psmux@ssh",
            &MuxBackend::<PsmuxTransport>::from_host(&win),
        );
        {
            use crate::agent::backend::SessionBackend;
            use crate::agent::psmux::PsmuxBackend;
            use crate::agent::tmux::TmuxBackend;
            let backends: Vec<(&str, Box<dyn SessionBackend>)> = vec![
                ("tmux@local", Box::new(TmuxBackend::local())),
                ("tmux@ssh", Box::new(TmuxBackend::from_host(&ssh))),
                ("tmux@wsl", Box::new(TmuxBackend::from_host(&wsl))),
                ("tmux@psmuxhost", Box::new(TmuxBackend::from_host(&win))),
                ("psmux@local", Box::new(PsmuxBackend::local())),
                ("psmux@ssh", Box::new(PsmuxBackend::from_host(&win))),
            ];
            for (label, backend) in backends {
                writeln!(out, "backend {label} name = {}", backend.name()).unwrap();
                writeln!(
                    out,
                    "backend {label} needs_liveness_poll = {}",
                    backend.needs_liveness_poll()
                )
                .unwrap();
                writeln!(
                    out,
                    "backend {label} supports_snapshots = {}",
                    backend.supports_snapshots()
                )
                .unwrap();
            }
        }
        let mut w = |k: &str, v: String| writeln!(out, "local {k} = {v}").unwrap();
        w("utf8_flag", format!("{}", LocalMuxTransport::UTF8_FLAG));
        w(
            "oneshot_pane_report",
            format!("{}", LocalMuxTransport::ONESHOT_PANE_REPORT),
        );
        w(
            "windows_stamped",
            format!("{}", LocalMuxTransport::WINDOW_OPTIONS),
        );
        let sock = local_socket();
        w(
            "deferred",
            LocalMuxTransport::deferred_prompt_script(
                &sock,
                "thurbox:tb-demo",
                "line one\nline two",
            ),
        );
        w(
            "heartbeat",
            LocalMuxTransport::heartbeat_loop_command(Path::new("/opt/tbx/thurbox-cli")),
        );
        let with_args = platform().new_window_command(
            "tb-demo",
            "thurbox:{end}",
            COMMAND,
            &args(),
            Some(Path::new("/tmp/w")),
            &env1(),
        );
        w("new_window_command args", format!("{:?}", argv(&with_args)));
        let no_args =
            platform().new_window_command("tb-demo", "thurbox:{end}", COMMAND, &[], None, &env1());
        w(
            "new_window_command no-args",
            format!("{:?}", argv(&no_args)),
        );
        out.replace(&format!("\"-L\", \"{sock}\""), "\"-L\", \"SOCK\"")
            .replace(&format!("-L {sock} "), "-L SOCK ")
    }

    /// `ssh_command` adds its multiplexing options only where `~/.ssh` exists:
    /// a property of the machine, the same before and after the split.
    fn machine_neutral(dump: &str) -> String {
        let multiplex: Vec<String> = crate::shell::SSH_MULTIPLEX_OPTS
            .iter()
            .map(|opt| format!("{opt:?}, "))
            .collect();
        dump.replace(&multiplex.concat(), "")
    }

    #[test]
    fn every_dialect_answers_as_the_core_did_before_the_split() {
        let now = machine_neutral(&current());
        let before = machine_neutral(BEFORE);
        let before: Vec<&str> = before.lines().collect();
        let after: Vec<&str> = now.lines().collect();
        assert_eq!(before.len(), after.len(), "a key was added or dropped");
        let mut moved = Vec::new();
        for (b, a) in before.iter().zip(&after) {
            let deliberate = DELIBERATE.iter().any(|(key, was, is)| {
                *b == format!("{key} = {was}") && *a == format!("{key} = {is}")
            });
            if b != a && !deliberate {
                moved.push(format!("before: {b}\n after: {a}"));
            }
        }
        assert!(moved.is_empty(), "{}", moved.join("\n"));
        for (key, _, is) in DELIBERATE {
            assert!(
                after.contains(&format!("{key} = {is}").as_str()),
                "deliberate change to `{key}` is no longer in effect"
            );
        }
    }
}
