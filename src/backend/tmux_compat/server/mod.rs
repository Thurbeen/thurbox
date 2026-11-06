//! The tmux protocol's session backend: one server of a multiplexer that speaks
//! the tmux command and control-mode grammar, on this machine or on a host.
//!
//! [`Server`] is everything tmux and psmux share. It is generic over a
//! [`TmuxCompatible`] multiplexer, and every way one of them differs from the
//! other is a body in that multiplexer's adapter (`backend::tmux`,
//! `backend::psmux`) — never a branch on a binary's name or the build OS here.

use std::borrow::Cow;
use std::collections::HashMap;
use std::marker::PhantomData;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};

use anyhow::{bail, Context, Result};
use tracing::{debug, info, warn};

use crate::backend::contract::{
    AdoptedSession, BackendLiveness, Capabilities, DiscoveredSession, Key, Owner, PaneChunk,
    PaneId, PaneIo, PaneSize, PaneState, PaneStream, Placed, SessionBackend, SpawnedSession,
    StatusChannel, WindowLifecycle, WindowRole, WindowSpec,
};
use crate::backend::identity::{window_name_for, Located, WindowIndex, SHELL_WINDOW_PREFIX};
use crate::backend::instance::{host_socket, known_host_socket, learned_host_socket, local_socket};
use crate::backend::tmux_compat::control_mode::{
    self, is_broken_pipe, is_recv_timeout, mouse_seed_bytes, shell_escape, title_seed_bytes,
    ControlMode, ControlModeReader, ControlModeWriter, ControlPolicy, Missing, PaneInput, Refusal,
    SIZED_BY, SIZER_OPTION,
};
use crate::backend::tmux_compat::transport::{SocketDir, TmuxTransport};
use crate::session::{HostDef, Multiplexer, Platform};
use crate::shell::HostLauncher;

/// A multiplexer that speaks the tmux command and control-mode protocol: tmux
/// itself, or a clone of it. Each one is an adapter of its own implementing
/// this, and a [`Server`] of it is that adapter's session backend.
///
/// Every item is a fact the multiplexer was measured to have, or a command it
/// was measured to need, answered by its adapter. The shared code asks what a
/// server can do; it never asks which multiplexer it is.
pub trait TmuxCompatible: Send + Sync + 'static {
    /// The multiplexer this is: the route a backend of it serves, and the
    /// binary it runs.
    const MULTIPLEXER: Multiplexer;

    /// Whether a `#{@…}` answers with a **window's** own option. Where it does
    /// not, a stamp identifies nothing (ADR-13): none is written, one read back
    /// is dropped, and a window is found by its name alone.
    const WINDOW_OPTIONS: bool;

    /// Whether windows take tmux's window settings (`remain-on-exit`,
    /// `window-size`) — at birth, and as a pane's retention later.
    const WINDOW_SETTINGS: bool;

    /// Whether the server's `%window-close` and `%layout-change` can be taken
    /// at their word: a close ends its panes' readers, and a size is reported
    /// to the grid. Where they cannot, liveness is polled, no pane size is
    /// reported, and a close is only a hint ([`ControlPolicy::confirm_close`]).
    const WINDOW_EVENTS: bool;

    /// Whether a command can report `ERROR:` while exiting zero.
    /// Such an answer is a failure even though the process status says
    /// otherwise; accepting it turns the diagnostic into pane contents.
    const EXIT_ZERO_ERRORS: bool = false;

    /// Whether a pane or window id (`%3`, `@3`) is numbered per SESSION rather
    /// than per server. Where it is, a bare `-t %3` resolves in whichever
    /// session the server routes it to, so every id target is qualified with
    /// thurbox's session (`qualify_target`).
    const SESSION_SCOPED_IDS: bool = false;

    /// Whether a pane's window is killed through `kill-pane` rather than
    /// `kill-window`, for a server whose `kill-window` misreads a pane id in
    /// the pane slot. Only right where a thurbox window holds one pane.
    const KILL_BY_PANE: bool = false;

    /// Whether pane output must be enabled and disabled with `refresh-client
    /// -A`. Servers that stream attached panes automatically do not take it.
    const PANE_MONITORING: bool;

    /// Whether a command's reply is queued behind the pane output ahead of it
    /// in a tagged block, which is what makes a snapshot exact.
    const SNAPSHOTS: bool;

    /// Whether `capture-pane -S n -E n` returns visible row `n` alone and
    /// `#{cursor_x}`/`#{cursor_y}` say where the cursor is — what reading the
    /// text before the cursor needs. Off unless measured: the wrong row read
    /// back would be worse than not knowing.
    const CURSOR_ROW_CAPTURE: bool = false;

    /// Whether one invocation takes a `;`-separated command list, so the whole
    /// session config can go in one process (#1243).
    const COMMAND_LISTS: bool;

    /// Whether a semicolon-separated control-mode command list answers with
    /// one response block rather than one block per command.
    const COMMAND_LIST_SINGLE_REPLY: bool;

    /// Attempts to start a missing session when the server disappears during
    /// cold bootstrap. A stable server still takes the usual single pass.
    const COLD_START_ATTEMPTS: usize = 1;

    /// Final wait for a server still coming up after the create client failed.
    const COLD_START_GRACE: std::time::Duration = std::time::Duration::ZERO;

    /// Retry a transient "no server" reply even when a later probe succeeds.
    const RETRY_NO_SERVER_ERROR: bool = false;

    /// Size arguments for the initial placeholder window.
    const BOOTSTRAP_SIZE_ARGS: &[&str] = &["-x", "80", "-y", "24"];

    /// Read-only command used to check whether the session answers.
    const SESSION_PROBE_COMMAND: &str = "has-session";

    /// Whether the server's socket lives in a directory thurbox chooses
    /// (`TMUX_TMPDIR`, [`SocketDir`]) and can be recreated there with
    /// `SIGUSR1` once lost — tmux's manual promises both. psmux has no socket
    /// directory, and RMUX has not been measured.
    const SOCKET_DIR: bool = false;

    /// Whether a one-shot `new-window -a -t <session>:{end} -P -F` appends the
    /// window last and answers with its pane — what the headless spawn stamps
    /// and retains it by.
    const ONE_SHOT_SPAWN_ANSWERS: bool;

    /// How long after `create_window` to look for the window again, on a
    /// server whose window closes with its program (no `remain-on-exit`):
    /// there, a program that cannot start leaves nothing behind, and a create
    /// that only asked whether `new-window` ran reported a session that never
    /// started. `None` where a dead pane stays to be seen.
    const SPAWN_SETTLE: Option<std::time::Duration> = None;

    /// Whether a resize can be made conditional on who sizes the window
    /// (`if-shell -F` over [`SIZER_OPTION`]). Where it cannot, the last
    /// instance to paint a pane sizes it.
    const CONDITIONAL_RESIZE: bool;

    /// The `set-option` flag scoping a server-wide option.
    const SERVER_SCOPE: &str;

    /// Whether a server-wide option also needs the session to route its client.
    const SERVER_OPTIONS_NEED_TARGET: bool = false;

    /// The flags that make a one-shot format answer (`display-message -p`,
    /// `list-windows -F`) keep its separators whatever the locale (see
    /// `PANE_STATE_UTF8_FLAG`'s reason in the tmux adapter), before the command
    /// itself.
    const DISPLAY_FLAGS: &[&str];

    /// Refuse a server too old to give a new pane its own console or to read
    /// the pane targets thurbox sends, from its `#{version}` answer, or `None`
    /// when nothing is known to need refusing. Asked of the running server
    /// before a pane is named or born (`Server::admit_server`); the binary is
    /// judged by [`Self::check_banner`], there and before it starts a server.
    const VERSION_FLOOR: Option<fn(&str, &str) -> Result<()>>;

    /// Refuse a `-V` banner this adapter cannot work with, or that would start
    /// a server on `socket` too old to give a pane its own console.
    fn check_banner(banner: &str, socket: &str) -> Result<()>;

    /// Refuse command diagnostics in a listing even when its status claims
    /// success. Captured terminal contents are not listings.
    fn check_listing(_listing: &str) -> Result<()> {
        Ok(())
    }

    /// The session config only this multiplexer takes, applied after the shared
    /// options and best-effort or fatal as each says.
    fn session_config(session: &str) -> Vec<ConfigOption>;

    /// A control-mode argument, quoted the way this server's tokenizer reads.
    /// POSIX quoting is the default; a different tokenizer overrides it.
    fn quote(arg: &str) -> String {
        shell_escape(arg)
    }

    /// What `answer` — one line of this multiplexer's refusal, exactly as it
    /// printed it — says is not there, or `None` when it is not one of this
    /// multiplexer's own absence replies. Every refusal path reads it: a
    /// one-shot's stderr, a control-mode `%error` and a status-zero `ERROR:`.
    /// tmux's words are the default (`tmux_answered_absent`).
    fn answered_absent(answer: &str) -> Option<Missing> {
        tmux_answered_absent(answer)
    }

    /// The `new-window` flags carrying `env` into the window, if the server
    /// honours them; empty where the environment rides in the command itself.
    fn env_flags(env: &HashMap<String, String>) -> String {
        env.iter()
            .map(|(k, v)| format!(" -e {}", shell_escape(&format!("{k}={v}"))))
            .collect()
    }

    /// The command a new window runs, as a control-mode `new-window` line
    /// carries it — or why the server could not run it.
    fn window_command(
        server: &Server<Self>,
        window_name: &str,
        command: &str,
        args: &[String],
        _env: &HashMap<String, String>,
    ) -> Result<String>
    where
        Self: Sized,
    {
        Ok(server.posix_window_command(window_name, command, args))
    }

    /// The environment and program that close a one-shot `new-window`'s argv
    /// — or why the server could not run it.
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

    /// The one-shot argvs that deliver `text` into `target`, in order — more
    /// than one where a single line would be too long for the server's host.
    /// By default one: the `bracketed_paste`-wrapped bytes, taken literally
    /// (`send-keys -l`).
    fn paste_args(target: &str, text: &str) -> Vec<Vec<String>> {
        vec![vec![
            "send-keys".into(),
            "-t".into(),
            target.into(),
            "-l".into(),
            bracketed_paste(text),
        ]]
    }

    /// The `run-shell` script that pastes `text` into `target` on `mux -L
    /// socket`, waits a beat so the paste is consumed, then presses Enter —
    /// run through the server's own shell — or why that shell cannot run it.
    /// By default a plain `sh` one-liner, quoted as the immediate commands
    /// pass each argument, since a host may name a socket with a space in it.
    fn deferred_paste_script(mux: &str, socket: &str, target: &str, text: &str) -> Result<String> {
        let (mux, socket, target) = (
            shell_escape(mux),
            shell_escape(socket),
            shell_escape(target),
        );
        let text = shell_escape(&bracketed_paste(text));
        Ok(format!(
            "{mux} -L {socket} send-keys -t {target} -l {text}; \
             sleep 0.2; {mux} -L {socket} send-keys -t {target} Enter"
        ))
    }

    /// How a pane's input is typed through control mode, and whether a paste
    /// goes another way. By default every keystroke as `send-keys -H` hex, and
    /// a paste through control mode (`set-buffer` and `paste-buffer -p`), so
    /// the server frames it only for an app that asked.
    fn pane_input(_transport: &TmuxTransport, _socket: &str) -> Arc<dyn PaneInput> {
        Arc::new(HexKeys)
    }

    /// What a control-mode connection to this server may expect of it.
    fn control_policy(transport: &TmuxTransport, session: &str) -> ControlPolicy;

    /// Whether a hook in this server's panes can report its state through the
    /// pane option ([`control_mode::REMOTE_HOOK_STATE_OPTION`]) and have it
    /// read back. Where it cannot, the backend has no status channel: no hook
    /// command is offered, nothing is recorded, and a listing is refused.
    const HOOK_STATUS: bool;

    /// The in-pane command that sets the option, the state word to follow —
    /// whatever this server needs to find its own pane from inside one. By
    /// default it names neither server nor pane: inside a pane the binary
    /// finds both (tmux from `$TMUX` and `$TMUX_PANE`), whichever socket the
    /// server is on, and the command needs no shell variable a hook file
    /// might refuse.
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

    /// The control-mode listing of every pane of `session` with its hook-state
    /// option, as the hook poller runs it ([`ControlPolicy::status_poll`]).
    fn hook_poll_command(session: &str) -> String {
        format!(
            "list-panes -s -t {} -F {}",
            Self::quote(session),
            Self::quote(&hook_state_format())
        )
    }

    /// The version word a `-V` banner names: the word after this
    /// multiplexer's name on the last line that has one, or `None` when no
    /// line does.
    fn banner_version(banner: &str) -> Option<&str> {
        named_version(banner, &[Self::MULTIPLEXER.name()])
    }
}

/// The `list-panes` format giving every pane with its hook-state option, one
/// `<pane id> <state>` per line.
fn hook_state_format() -> String {
    format!(
        "#{{pane_id}} #{{{}}}",
        control_mode::REMOTE_HOOK_STATE_OPTION
    )
}

/// The word after any of `names` on the last line of `banner` naming one.
/// That line or nothing: an earlier line is an older name for the same
/// binary, and reading past a suffixed one (`3.3.8-dev`) to it would judge a
/// version the binary does not claim to be.
pub(in crate::backend) fn named_version<'a>(banner: &'a str, names: &[&str]) -> Option<&'a str> {
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

/// The `(major, minor, patch)` a version word names, each part read up to its
/// first non-digit (`3.3a`, `3.3.8-dev`) and the patch 0 where there is none.
/// `None` without a major and a minor.
pub(in crate::backend) fn parse_version(word: &str) -> Option<(u32, u32, u32)> {
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

/// Wrap `text` in the bracketed-paste escape sequences (`ESC[200~ … ESC[201~`)
/// so a multi-line prompt is delivered as a single paste — the embedded
/// newlines insert as text instead of submitting the prompt on the first one.
/// [`PaneIo::send_text`](crate::backend::PaneIo::send_text) is
/// how the TUI reaches this too: the kernel's prompt commands call it rather
/// than framing the paste themselves. The trailing `Enter` is sent separately.
fn bracketed_paste(text: &str) -> String {
    format!("\x1b[200~{text}\x1b[201~")
}

/// The default pane input ([`TmuxCompatible::pane_input`]).
struct HexKeys;

impl PaneInput for HexKeys {
    fn send_keys(&self, pane_id: &str, buf: &[u8]) -> Vec<String> {
        control_mode::hex_send_keys_commands(pane_id, buf)
    }

    fn paste(&self, _pane_id: &str, _text: &str) -> Option<Result<()>> {
        None
    }
}

/// The tmux session name grouping every thurbox window. Dev builds use
/// "thurbox-dev" to avoid interfering with an installed release binary.
pub const TMUX_SESSION: &str = if cfg!(dev_build) {
    "thurbox-dev"
} else {
    "thurbox"
};

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

/// What [`WindowLifecycle::window_panes`] lists, read by [`named_panes`].
const WINDOW_PANES_FORMAT: &str = "#{pane_id}|#{window_name}|#{pane_dead}";

/// The panes of the windows named exactly `window_name` in a
/// [`WINDOW_PANES_FORMAT`] listing, each with whether it is dead.
fn named_panes(listing: &str, window_name: &str) -> Vec<(String, bool)> {
    let mut found = Vec::new();
    for line in listing.lines() {
        let parts: Vec<&str> = line.trim().splitn(3, '|').collect();
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
    found
}

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
    // option (`stamps`, [`TmuxCompatible::WINDOW_OPTIONS`] — ADR-13): there the answer is one
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

/// `target` as `-t` should carry it on a server whose ids are numbered per
/// session ([`TmuxCompatible::SESSION_SCOPED_IDS`]): a pane id in the pane slot
/// of `session` (`thurbox:.%3`), a window id in its window slot
/// (`thurbox:@3`). Anything else — a target that already names its session, a
/// session name — is left as it is, as is every target where ids are
/// server-wide.
///
/// Measured on psmux 3.3.7, whose socket hosts a `__warm__` session beside
/// thurbox's with the same `%N`/`@N` numbers: a bare `kill-window -t @7`
/// killed the other session's `@7`, and a bare `display-message -t %2`
/// answered for the other session's `%2`. 3.3.8 refuses an ambiguous bare id
/// instead (psmux #569) and documents the pane-slot form, which 3.3.7 reads as
/// no pane at all, running the command on the session's active pane (#1557) —
/// hence psmux's 3.3.8 floor.
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

/// The tmux window option carrying the id of the session row that owns a
/// window — the identity a window has that a name and a pane id do not.
///
/// A window *name* is neither unique (two sessions may be given the same one,
/// and ADR-24 mirrors a host's names verbatim) nor injective
/// (`sanitize_window_name` collapses `a:b` and `a.b` onto one), while tmux
/// reissues pane ids from `%0` every time its server starts. A window option
/// survives both, stored once on the thing it describes — the same channel
/// [`StatusChannel::record_hook_state`] uses for hook state. See ADR-25.
pub const WINDOW_SESSION_OPTION: &str = "@thurbox_session";

/// The tmux window option saying what a stamped window is *for*.
///
/// Part of the address rather than decoration: a session owns an agent window
/// and a companion shell window, and both carry its id.
pub const WINDOW_ROLE_OPTION: &str = "@thurbox_role";

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
/// set for a session (see [`SESSION_OPTS`](bootstrap::SESSION_OPTS)) — and stated even when the answer is
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
/// server-wide default ([`WINDOW_OPTS`](bootstrap::WINDOW_OPTS)), and "afterwards" was a second message
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
fn birth_options(window_name: &str) -> [(&'static str, &'static str); 2] {
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
/// The `-d` path cannot use that and names its window; see
/// [`create_local_window`].
fn birth_option_commands<M: TmuxCompatible>(window_name: &str) -> Vec<String> {
    if !M::WINDOW_SETTINGS {
        return Vec::new();
    }
    birth_options(window_name)
        .iter()
        .map(|(key, value)| format!("set-window-option {key} {value}"))
        .collect()
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

/// `ssh`'s own failure code — see [`crate::session_ops::host_cli::Reach`],
/// which draws the same distinction one layer up.
const SSH_ERROR_EXIT: i32 = 255;

/// The most a command issued from the interface's own loop may wait for its
/// answer, where one is wanted.
///
/// Sized against the two things it sits between: a healthy round trip over an
/// already-open connection is sub-millisecond, and the budget a keypress has
/// before the interface reads as frozen is a fraction of a second. Generous
/// enough that a loaded machine or a slow link still gets a real answer; short
/// enough that a link carrying nothing costs a hitch instead of a freeze.
const LOOP_COMMAND_BUDGET: std::time::Duration = std::time::Duration::from_millis(250);

/// What a failed one-shot's multiplexer said is missing, given the layer that
/// failed (`is_ssh` + the exit `code`) and its one line of `answer`.
///
/// **Layer before text**, and the order is the point. `ssh` exits 255 for its
/// own failures and passes a remote command's status through untouched
/// (a remote `exit 7` exits 7), so 255 is ssh saying the question never
/// arrived — no matter what the stderr underneath happens to resemble.
/// Only once the transport is ruled out does the multiplexer's own answer get
/// to speak, and then only in the exact words its adapter says it uses
/// ([`TmuxCompatible::answered_absent`]).
///
/// Everything else is "could not tell", which the caller must treat as the
/// unanswered question it is: an empty listing means *there is nothing to
/// kill*, and that is not a conclusion to reach by guessing.
fn mux_answered<M: TmuxCompatible>(
    is_ssh: bool,
    code: Option<i32>,
    answer: &str,
) -> Option<Missing> {
    match code {
        // ssh's own error, so nothing on the host ever saw the question.
        Some(SSH_ERROR_EXIT) if is_ssh => None,
        // Killed by a signal: it did not finish, and whatever reached stderr
        // before that is a fragment of an answer rather than one. There is no
        // layer to reason from, so there is nothing to conclude.
        None => None,
        _ => M::answered_absent(answer),
    }
}

/// What a failed one-shot's stderr says is missing, when it is exactly one of
/// the multiplexer's absence replies.
///
/// Another stderr line means another failure happened beside it. SSH advisory
/// banners are the one transport-owned exception; [`mux_stderr`] removes
/// those first.
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

/// Whether a failed listing is exactly the multiplexer's reply that its server
/// holds nothing. A refusal does not make any stdout trustworthy.
fn listing_output_is_absence<M: TmuxCompatible>(
    is_ssh: bool,
    output: &std::process::Output,
) -> bool {
    output.stdout.is_empty() && output_answered::<M>(is_ssh, output) == Some(Missing::Server)
}

/// tmux's own "nothing there" replies — the default
/// [`TmuxCompatible::answered_absent`], which another adapter extends with its
/// own words.
///
/// Each is matched from its first byte with a value after it, because an
/// adapter's reading is what stands between a host that was down for a minute
/// and a teardown that reports nothing to kill. Over-reporting a live host as
/// unanswered costs one cheap retry, while the reverse costs an orphaned agent
/// nobody ever looks for again.
///
/// `error connecting to` is the trap: tmux prints it for a socket that is not
/// there (`(No such file or directory)`) **and** for one it cannot open while
/// a server is very much alive behind it — `(Permission denied)` on another
/// user's socket, `(Connection refused)` on a stale one. Only the first is an
/// answer. Widening this list to cover more wordings is the trap it looks like
/// a fix: each new string makes the classifier more confidently wrong about
/// the next one nobody anticipated.
pub(in crate::backend) fn tmux_answered_absent(answer: &str) -> Option<Missing> {
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
    // tmux < 3.4's wording for the same thing, then a server up with no
    // session by that name.
    if names("no server running on ") || names("can't find session: ") {
        return Some(Missing::Server);
    }
    if names("can't find pane: ") || names("can't find window: ") {
        return Some(Missing::Target);
    }
    None
}

/// One command of the session config — a `set-option`, or an `if-shell` an
/// adapter guards one with — and whether failing it means the server cannot
/// host sessions.
pub struct ConfigOption {
    pub args: Vec<String>,
    pub fatal: bool,
}

impl ConfigOption {
    /// `set-option <args…>`.
    pub fn set(args: &[&str], fatal: bool) -> Self {
        let mut all = vec!["set-option".to_string()];
        all.extend(args.iter().map(ToString::to_string));
        Self { args: all, fatal }
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

/// Delay between pasting text and pressing Enter, so the target app has taken
/// the paste in before it is submitted.
const SEND_KEYS_ENTER_DELAY: std::time::Duration = std::time::Duration::from_millis(200);

/// Hard cap on the number of scrollback lines a capture returns.
const MAX_CAPTURE_LINES: u32 = 10_000;

/// Rows of history a capture or a snapshot carries for a terminal that keeps
/// `scrollback_lines`: as many as it keeps, under the ceiling every capture
/// here has.
fn history_lines(scrollback_lines: usize) -> usize {
    scrollback_lines.min(MAX_CAPTURE_LINES as usize)
}

/// A server of the tmux-compatible multiplexer `M` — sessions persist in `<mux>
/// -L <socket>` on this machine or on a host reached over SSH or in a WSL
/// distro.
///
/// Uses control mode (`-C`) for all I/O after `ensure_ready()`. Local and
/// remote differ only in the [`TmuxTransport`] that launches the binary; the
/// protocol is identical.
pub struct Server<M: TmuxCompatible> {
    /// How the binary is launched (local `Command` vs `ssh <dest> <mux> …`).
    pub(in crate::backend) transport: TmuxTransport,
    /// Socket name passed via `-L` (e.g. `thurbox`) as configured; read
    /// through [`Self::socket`], which prefers what the host's own CLI said.
    pub(in crate::backend) socket: String,
    /// Session name grouping all thurbox windows.
    pub(in crate::backend) session: String,
    /// The route this backend serves, as the registry and a persisted
    /// `backend_type` spell it (`local:tmux`, `ssh:<host>:psmux`).
    pub(in crate::backend) name: String,
    /// The connection to the server, opened apart from it: see [`Connection`].
    pub(in crate::backend) link: Connection,
    /// Set by [`WindowLifecycle::shutdown`]: from then on no connection is
    /// opened, so a worker still holding the registry as the process quits
    /// cannot bring back the one quit just closed.
    pub(in crate::backend) closed: std::sync::atomic::AtomicBool,
    /// Rows of history an adopt captures and a snapshot carries: what the
    /// terminal rebuilt from them keeps, from the settings the composition root
    /// built this backend with.
    history_lines: usize,
    /// The host an off-local backend was built from, for the agent `PATH`
    /// its windows get ([`crate::agent::host_path`]). `None` locally.
    pub(in crate::backend) host: Option<HostDef>,
    /// The OS of the machine the multiplexer runs on — this one's, or the
    /// host's ([`HostDef::platform`]). What decides the shells a pane and the
    /// server get, independently of the multiplexer and of the OS thurbox was
    /// built for.
    pub(in crate::backend) platform: Platform,
    multiplexer: PhantomData<M>,
}

/// A control-mode connection to a [`Server`], and what it has learned of the
/// server behind it.
///
/// Separate from the [`Server`] it reaches: the server says which multiplexer
/// runs where, and connecting to it is a step of its own, the same one on this
/// machine and on a host. [`Server::transient`] is the same server with a
/// fresh connection.
pub(in crate::backend) struct Connection {
    /// The live connection, taken only long enough to clone or swap the
    /// handle: [`ControlMode`] serialises its own commands, so a caller's round
    /// trip — up to `COMMAND_TIMEOUT` on a link that stopped answering — is
    /// never anybody else's wait (#1472).
    pub(in crate::backend) control: Mutex<Option<Arc<ControlMode>>>,
    /// Held while a connection is opened, by [`Server::ensure_control`] and
    /// [`Server::reconnect_control`], so a handshake happens once however many
    /// callers find the connection missing or dead, and outside `control`.
    pub(super) connecting: Mutex<()>,
    /// Connections replaced but still answering what was sent on them
    /// ([`ControlMode::retire`]), for [`WindowLifecycle::shutdown`] to close.
    pub(super) retiring: Mutex<Vec<std::sync::Weak<ControlMode>>>,
    /// Bumped under `control` by each connection installed, which may be to a
    /// server started since: what an admission ([`Server::admit_server`]) is
    /// good for.
    pub(super) generation: std::sync::atomic::AtomicU64,
    /// The `generation` the server was found new enough at over that
    /// connection, plus one; zero for never.
    pub(super) admitted: std::sync::atomic::AtomicU64,
    /// Held while the multiplexer is asked, so callers arriving together —
    /// every pane re-registered after a reconnect — wait for one question:
    /// they take a connection's answer from `admitted`, and a failure on the
    /// same connection from the generation and reason held here. A timeout
    /// reconnects, which is a new connection, so those behind it ask that
    /// one afresh; a one-shot's success is not kept, so each of them asks
    /// again.
    pub(super) admitting: Mutex<Option<(u64, String)>>,
    /// Counts the questions settled, so a waiter can tell one was answered
    /// while it waited.
    pub(super) questions: std::sync::atomic::AtomicU64,
    /// This connection's name in [`SIZER_OPTION`] — see
    /// [`Server::resize_window`].
    pub(in crate::backend) sizer: String,
}

impl Connection {
    /// No connection yet: one is opened on first use.
    fn new() -> Self {
        Self {
            control: Mutex::new(None),
            connecting: Mutex::new(()),
            retiring: Mutex::new(Vec::new()),
            generation: std::sync::atomic::AtomicU64::new(0),
            admitted: std::sync::atomic::AtomicU64::new(0),
            admitting: Mutex::new(None),
            questions: std::sync::atomic::AtomicU64::new(0),
            sizer: sizer_name(),
        }
    }
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

impl<M: TmuxCompatible> Server<M> {
    /// `M` on this machine, named by the route it serves (`local:tmux`,
    /// `local:psmux`) whatever OS this is, capturing as much history as a
    /// terminal keeping `scrollback_lines` holds.
    pub fn local(scrollback_lines: usize) -> Self {
        let name = crate::session::Route::local(Some(M::MULTIPLEXER)).format();
        let socket = local_socket();
        let mut transport = TmuxTransport::local(M::MULTIPLEXER.name());
        if M::SOCKET_DIR && crate::shell::local_platform() == Platform::Posix {
            if let Some(dir) = crate::backend::instance::local_socket_dir(&socket) {
                transport = transport.with_socket_dir(SocketDir::Dir(dir.display().to_string()));
            }
        }
        Self::with_transport(transport, socket, TMUX_SESSION, name, scrollback_lines)
    }

    /// A backend over an explicit transport. A remote one is taken for a POSIX
    /// host until [`Self::for_host`] says which host it is.
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
            link: Connection::new(),
            closed: std::sync::atomic::AtomicBool::new(false),
            history_lines: history_lines(scrollback_lines),
            host: None,
            platform,
            multiplexer: PhantomData,
        }
    }

    /// `M` on `host`, reached the way its entry says and running as its
    /// platform — see [`Self::on_host`].
    pub fn for_host(host: &HostDef, scrollback_lines: usize) -> Self {
        Self::on_host(
            host,
            HostLauncher::for_host(host),
            host.platform(),
            scrollback_lines,
        )
    }

    /// `M` on `host`, reached through `launcher` on a machine of `platform`:
    /// its binary over SSH, or inside a WSL distro via `wsl.exe`. Named by the
    /// route it drives (`ssh:<host.name>:<mux>` / `wsl:…`) whatever the host
    /// prefers — a row written for tmux is served by tmux on a host that has
    /// since moved to something else. Its socket is the host's
    /// ([`host_socket`]), never this instance's own, and its session name is
    /// the default unless the host overrides it.
    pub fn on_host(
        host: &HostDef,
        launcher: HostLauncher,
        platform: Platform,
        scrollback_lines: usize,
    ) -> Self {
        let session = host
            .session
            .clone()
            .unwrap_or_else(|| TMUX_SESSION.to_string());
        let mut transport = TmuxTransport::remote(launcher, M::MULTIPLEXER.name())
            .with_shell(crate::shell::HostShell::for_platform(platform));
        if M::SOCKET_DIR && platform == Platform::Posix {
            transport = transport.with_socket_dir(SocketDir::HostData {
                app: crate::paths::app_dir_name().to_string(),
            });
        }
        let mut backend = Self::with_transport(
            transport,
            host_socket(host),
            session,
            host.route(Some(M::MULTIPLEXER)).format(),
            scrollback_lines,
        );
        backend.host = Some(host.clone());
        backend.platform = platform;
        backend
    }

    /// The socket this backend talks to: the configured one, unless the host's
    /// own CLI has since reported a different one (see
    /// [`crate::backend::instance::learn_host_socket`]). Resolved per
    /// call so a backend registered at startup follows the host.
    pub(in crate::backend) fn socket(&self) -> String {
        self.host
            .as_ref()
            .and_then(|host| learned_host_socket(&host.backend_name()))
            .unwrap_or_else(|| self.socket.clone())
    }

    /// `<mux> -L <socket> <args…>` on this server, in the directory its
    /// socket was found in ([`Self::socket_dir`]).
    pub(in crate::backend) fn mux_command(&self, args: &[&str]) -> Result<Command> {
        Ok(self
            .located_transport()
            .tmux_command(&self.socket(), args)?)
    }

    /// `id` as a `-t` target on this server — see [`qualify_target`] — once
    /// the server is one that reads it ([`Self::admit_server`]).
    pub(in crate::backend) fn target<'a>(&self, id: &'a str) -> Result<Cow<'a, str>> {
        self.admit_server()?;
        Ok(self.spell_target(id))
    }

    /// How [`Self::target`] spells `id`, asking the server nothing.
    pub(in crate::backend) fn spell_target<'a>(&self, id: &'a str) -> Cow<'a, str> {
        qualify_target(&self.session, id, M::SESSION_SCOPED_IDS)
    }

    /// The command and target that kill the window `id` is in
    /// ([`TmuxCompatible::KILL_BY_PANE`]), once the server is one that reads
    /// the target.
    pub(in crate::backend) fn kill_command<'a>(
        &self,
        id: &'a str,
    ) -> Result<(&'static str, Cow<'a, str>)> {
        self.admit_server()?;
        Ok(self.kill_spelling(id))
    }

    /// How [`Self::kill_command`] kills `id`'s window, asking the server
    /// nothing.
    pub(in crate::backend) fn kill_spelling<'a>(
        &self,
        id: &'a str,
    ) -> (&'static str, Cow<'a, str>) {
        let verb = match M::KILL_BY_PANE && control_mode::is_valid_pane_id(id) {
            true => "kill-pane",
            false => "kill-window",
        };
        (verb, self.spell_target(id))
    }

    /// Whether a `#{@...}` this multiplexer answers with is a **window's**
    /// option ([`TmuxCompatible::WINDOW_OPTIONS`]). Where it is not, a stamp
    /// read back identifies nothing, and [`parse_discovered`] drops it rather
    /// than reading one session's id as every window's.
    pub(in crate::backend) fn stamps_are_per_window(&self) -> bool {
        M::WINDOW_OPTIONS
    }

    /// One `list-windows`, with an empty answer only when the multiplexer
    /// itself said there is nothing to list.
    ///
    /// [`discover`](WindowLifecycle::discover) gates on the session probe and reads
    /// its failure as "no windows", which over a transport conflates the two
    /// answers a teardown must never confuse: *the host says it holds nothing*
    /// and *the host did not answer*. A force delete taken while a host was
    /// briefly unreachable therefore reported nothing to kill, recorded no
    /// error, and left the agent running there for good. Here an unrecognised
    /// failure is an error, so the caller can say so and come back later.
    ///
    /// Also one round trip instead of two: `list-windows` on an absent server
    /// gives exactly the refusal `has-session` was asked for.
    ///
    /// An absent server is looked for once more ([`Self::relocate`]) before
    /// it is believed: a server whose socket was lost still holds every
    /// window, and a listing that read it as empty would have each of them
    /// relaunched beside the original.
    pub(in crate::backend) fn discover_answered(&self) -> Result<Vec<DiscoveredSession>> {
        if let Some(windows) = self.discover_answered_once()? {
            return Ok(windows);
        }
        match self.relocate(true) {
            Search::Found => Ok(self.discover_answered_once()?.unwrap_or_default()),
            // A server holding every window out of reach is not an empty one:
            // the caller would relaunch each window beside the original.
            Search::Stuck(why) => Err(self.out_of_reach(&why)),
            Search::Absent => Ok(Vec::new()),
        }
    }

    /// One [`Self::discover_answered`] listing; `None` when the multiplexer
    /// said there is nothing to list.
    fn discover_answered_once(&self) -> Result<Option<Vec<DiscoveredSession>>> {
        let args = ["list-windows", "-t", &self.session, "-F", DISCOVER_FORMAT];
        // Run it here rather than through `run_tmux`, which formats the
        // failure into a message: the whole point is to keep the exit status,
        // because that is the layer talking and the message is only text.
        let output = self
            .mux_command(&args)?
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            // The launcher would not even start — no `ssh`/`wsl.exe`/`tmux` on
            // this machine. Nothing was asked, so nothing was answered, and
            // `launch_failure` says which of the three is not there.
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
        // Only an exit-zero reply is hidden failure; a non-zero one falls
        // through so psmux's own "no server running" (exit 1) reads as absence.
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

    /// Refuse input for a pane whose program has exited. Sessions run with
    /// `remain-on-exit=on` (`SESSION_OPTS`), so a dead agent leaves its window
    /// in place and `send-keys` still exits 0 while discarding the keystrokes —
    /// which is how the mailbox wake once came to report `woke: true` at a pane
    /// nothing was listening to.
    ///
    /// A question that goes unanswered reads as "not dead", so a hiccup costs a
    /// send attempt rather than silently dropping a prompt; a missing pane is
    /// then refused by the send itself.
    /// `target` is `pane` as [`Self::target`] spelled it, admitted already.
    fn refuse_exited(&self, pane: &str, target: &str) -> Result<()> {
        let dead = self
            .tmux_output(&["display-message", "-p", "-t", target, "#{pane_dead}"])
            .is_ok_and(|answer| parse_pane_dead(&answer));
        if dead {
            bail!("its pane {pane} has exited and accepts no input");
        }
        Ok(())
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
    /// can be told, and only a server with [`TmuxCompatible::WINDOW_EVENTS`]
    /// sends one.
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
        // window events, a reconnecting control mode) keeps the old behaviour of no mapping and
        // no EOF from a window close, and says so in the log. A multiplexer the
        // version floor refuses (`target`) is the exception: the attach fails,
        // since nothing could be named to it safely.
        //
        // On tmux the same question also reads the pane's size and who sizes it,
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
                // The pane id leads the answer so whose answer it is can be
                // checked: psmux 3.3.7 and 3.3.8 answer a `-t` naming a pane
                // they cannot find with the *active* pane's fields and no
                // error, even qualified, and a mapping learnt from that answer
                // ends the wrong reader on the active window's next close.
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
            // A pane whose output overflowed its channel is caught up by a
            // snapshot, where one can be taken.
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
        // Before any byte is read, so the history seed — captured at this size
        // — is parsed at it.
        if let (Some(size), Some(((rows, cols), sized_by))) = (&size, learned) {
            size.report(rows, cols);
            size.set_sized_elsewhere(sized_by.is_some_and(|name| name != self.link.sizer));
        }
        Ok((reader, size))
    }

    /// Unregister a pane sender (causes the reader to get EOF).
    /// Note: Currently removes all senders for this pane. For true instance-specific
    /// unregistration, we would need to track which sender belongs to which instance.
    fn unregister_pane(&self, pane_id: &str) -> Result<()> {
        self.with_control(|ctrl| {
            // A pane left paused would stream nothing to whoever adopts it next.
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

    /// Create a writer for a specific pane.
    fn pane_writer(&self, pane_id: &str) -> Result<ControlModeWriter> {
        // How keystrokes are encoded, and whether a paste goes another way,
        // is the multiplexer's (`TmuxCompatible::pane_input`).
        let input = M::pane_input(&self.transport, &self.socket());
        // The writer's only use of the id is as a `-t` target, and psmux's
        // paste is a one-shot client that the server routes (`PsmuxPaste`).
        let target = self.target(pane_id)?.into_owned();
        self.with_control(|ctrl| Ok(ctrl.writer(target, input)))
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
        if M::PANE_MONITORING {
            // A pane this connection holds back stays paused through `on`, and
            // this reader wants its output.
            self.with_control(|ctrl| ctrl.show_pane(pane_id))?;
            self.ctrl_command(&format!(
                "refresh-client -A {}",
                M::quote(&format!("{pane_id}:on"))
            ))?;
        }

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
    /// capture into history (tmux clamps to what exists). The title and mouse
    /// modes ride along because the capture cannot carry them — see
    /// [`Self::pane_state_seed`].
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
        // Ahead of the history, not after it: the capture ends wherever the
        // pane's last line ended, and appending to a run that stopped mid
        // escape sequence would feed the parser a spliced one.
        let mut seed = self.pane_state_seed(pane_id);
        seed.extend(history_seed_bytes(output.stdout));
        Ok(seed)
    }

    /// The pane's mouse modes and window title replayed as terminal bytes:
    /// what an app set before this process read its output, and which neither
    /// the capture nor a repaint brings back.
    ///
    /// Agents use the window title as their activity line — Claude Code writes
    /// the task it is on — and thurbox reads it off the PTY, so a restart that
    /// joins the stream mid-flight shows nothing until the agent next repaints
    /// it. tmux kept the value: `#{pane_title}` *is* the last OSC the pane
    /// emitted. Replaying it puts it back through the same callback a live
    /// title takes (`TermSignals`'s title callback), so nothing downstream
    /// learns a second way of being told.
    ///
    /// The mouse modes are what decide whether a wheel tick is forwarded to
    /// the app (`forward_wheel` reads them off this parser), and an app turns
    /// them on once, at startup — Codex does. Without them a Codex adopted by
    /// a later interface could not be scrolled at all: its alternate screen
    /// has no scrollback to scroll locally instead. See [`mouse_seed_bytes`].
    ///
    /// Best-effort by construction, and kept apart from the capture: a mux
    /// that answers this differently (psmux is unverified here) loses the
    /// activity line and the modes, never the scrollback.
    fn pane_state_seed(&self, pane_id: &str) -> Vec<u8> {
        // One query for all of it: a pane that never had a title set reads
        // back as the host's own short name, which is tmux's default rather
        // than anything an agent said. The title is last because it alone may
        // contain the separator.
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
    ) -> crate::backend::contract::BackendResult<SpawnedSession> {
        // It is the server's code that births the pane, and one started before
        // an upgrade keeps running the old.
        self.admit_server()?;
        // The command, its quoting and how the environment reaches it are the
        // multiplexer's: tmux takes joined trailing tokens and `-e`, psmux one
        // folded token.
        let shell_cmd = M::window_command(self, window_name, command, args, env)?;
        let cwd_part = match cwd {
            Some(dir) => format!(" -c {}", M::quote(&dir.to_string_lossy())),
            None => String::new(),
        };
        let env_part = M::env_flags(env);
        let escaped_window_name = M::quote(window_name);
        let session = &self.session;
        // The window's own options ride along in the same command list — see
        // `birth_options` for why neither can be a message of its own.
        let new_window = format!(
            "new-window -t {session} -n {escaped_window_name} -P -F '{SPAWN_FORMAT}'{cwd_part}{env_part} {shell_cmd}"
        );
        let options = birth_option_commands::<M>(window_name);
        let cmds: Vec<&str> = std::iter::once(new_window.as_str())
            .chain(options.iter().map(String::as_str))
            .collect();
        let result = self.ctrl_command_list(&cmds)?;
        // Two fields, and the second is optional in practice: a multiplexer
        // that prints only the pane id (psmux's `-P -F` support is unverified
        // against the documented divergences, ADR-13) leaves `window_id` None
        // and `register_pane` falls back to asking, exactly as before.
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
        // Dropped rather than fatal: a window id that is not one costs this pane
        // the ability to notice its own ending, which is what the pre-#1107
        // behaviour was — not a reason to refuse a window that started fine.
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
    ) -> crate::backend::contract::BackendResult<AdoptedSession> {
        // backend_id comes from the shared DB — never interpolate it unvalidated.
        if !control_mode::is_valid_pane_id(backend_id) {
            return Err(
                anyhow::anyhow!("refusing to adopt invalid pane id: {backend_id:?}").into(),
            );
        }
        // Opt-in split timing (THURBOX_PERF_LOG): the history capture is an
        // independent `tmux capture-pane` subprocess, while `connect_pane`
        // drives the serialized control-mode connection. Restore prefetches
        // the captures in parallel and passes them in (ADR-P9), so
        // `capture_ms` here reads 0 on that path; a `None` seed (a mid-run
        // adopt) still captures inline, before connecting so seeded history
        // can't duplicate live output. Best-effort: adoption must survive a
        // failed capture — except one saying the pane is gone. Nothing after it
        // notices: psmux answers `display-message` for a missing pane with its
        // active pane's fields, and the adopted session read as a quiet live
        // one with no pane behind it (#1433).
        let perf_log = std::env::var_os("THURBOX_PERF_LOG").is_some();

        let capture_start = perf_log.then(std::time::Instant::now);
        let seed = match seed {
            Some(seed) => seed,
            None => match self.capture_history(backend_id) {
                Ok(seed) => seed,
                Err(e) if matches!(e, crate::backend::BackendError::Absent(_)) => {
                    return Err(crate::backend::BackendError::Absent(format!(
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
            output: seeded(seed, connected.output),
            input: connected.input,
            seed_len,
            size: connected.size,
        })
    }

    fn capture_history(
        &self,
        backend_id: &str,
    ) -> crate::backend::contract::BackendResult<Vec<u8>> {
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

    fn request_snapshot(&self, backend_id: &str) -> crate::backend::contract::BackendResult<()> {
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
        // Asked on the loop, so it does not wait for the answer. A pane whose output was
        // held back streams again first, so the snapshot sits in its stream
        // ahead of everything it does not already show.
        Ok(self.with_control(|ctrl| {
            ctrl.show_pane(backend_id)?;
            ctrl.request_snapshot(backend_id, self.history_lines)
        })?)
    }

    /// Only over a network, where a pane's output is what the link carries;
    /// a local pane streams as it always has. Needs a snapshot to come back
    /// from ([`TmuxCompatible::SNAPSHOTS`]) and pane output that can be paused
    /// ([`TmuxCompatible::PANE_MONITORING`]); the connection checks for format
    /// subscriptions, which carry what the pane would have printed.
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
    ) -> crate::backend::contract::BackendResult<crate::backend::contract::PaneSnapshot> {
        if !control_mode::is_valid_pane_id(backend_id) {
            return Err(
                anyhow::anyhow!("refusing to snapshot invalid pane id: {backend_id:?}").into(),
            );
        }
        if !M::SNAPSHOTS {
            return Err(anyhow::anyhow!("{} cannot snapshot a pane", self.transport.mux()).into());
        }
        // Asked, then waited for apart from the asking, so several searches'
        // round trips overlap: a search reads many panes at once.
        Ok(self
            .with_control(|ctrl| ctrl.ask_snapshot(backend_id, self.history_lines))?
            .wait()
            .map_err(|e| self.classified(e))?)
    }

    fn output_times(&self) -> Result<std::collections::HashMap<String, u64>> {
        if !self.stamps_are_per_window() {
            return Ok(std::collections::HashMap::new());
        }
        // A stale row's pane id may now belong to somebody else. Read only
        // windows whose own stamps identify the session, excluding its shell.
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
            if owner.parse::<crate::session::SessionId>().is_err() || role == "shell" {
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

    fn resize(
        &self,
        backend_id: &str,
        rows: u16,
        cols: u16,
    ) -> crate::backend::contract::BackendResult<()> {
        self.resize_window(backend_id, rows, cols)
    }

    fn claim_size(
        &self,
        backend_id: &str,
        rows: u16,
        cols: u16,
    ) -> crate::backend::contract::BackendResult<()> {
        self.claim_window_size(backend_id, rows, cols)
    }

    fn detach(&self, backend_id: &str) -> crate::backend::contract::BackendResult<()> {
        // Disable output monitoring for this pane.
        if M::PANE_MONITORING {
            let off = format!(
                "refresh-client -A {}",
                M::quote(&format!("{backend_id}:off"))
            );
            if let Err(e) = self.ctrl_command_detached(&[&off], 1) {
                warn!("Failed to disable output monitoring during detach: {e}");
            }
        }
        // Remove the pane sender — the ControlModeReader gets EOF.
        let _ = self.unregister_pane(backend_id);
        Ok(())
    }
}

impl<M: TmuxCompatible> WindowLifecycle for Server<M> {
    fn check_available(&self) -> crate::backend::contract::BackendResult<()> {
        // `tmux -L <socket> -V` prints the version without connecting, and over
        // the SSH transport this verifies remote connectivity at the same time.
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
        // The binary's own verdict, whatever server it would talk to: psmux
        // before 3.3.8 reads `-t` in the client, so a new server does not make
        // an old binary safe (`admit_server` holds it to that per pane too).
        M::check_banner(&version_str, &self.socket())?;
        debug!("multiplexer version: {}", version_str.trim());
        Ok(())
    }

    fn ensure_ready(&self) -> crate::backend::contract::BackendResult<()> {
        self.refuse_if_closed()?;
        self.ensure_session_configured()?;
        Ok(self.ensure_control()?)
    }

    fn set_pane_retention(
        &self,
        backend_id: &str,
        keep: bool,
    ) -> crate::backend::contract::BackendResult<()> {
        // The guard every neighbour carries, for the reason a target makes it
        // worth carrying: tmux resolves `-t` as a window *name* as readily as
        // an id, so a caller passing anything else would quietly set
        // `remain-on-exit` on whatever window that name picked out.
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
    ) -> crate::backend::contract::BackendResult<Vec<(String, bool)>> {
        // A name lookup rather than an identity one: a program window carries no
        // session id to resolve, only its deterministic name. Matched exactly
        // rather than by prefix — tmux's own name matching is FNMATCH-ish, which
        // would make `tbp-x-watch` findable by `tbp-x-watc`.
        let listing = self.tmux_output(&[
            "list-windows",
            "-t",
            &self.session,
            "-F",
            WINDOW_PANES_FORMAT,
        ])?;
        Ok(named_panes(&listing, window_name))
    }

    fn discover(&self) -> crate::backend::contract::BackendResult<Vec<DiscoveredSession>> {
        // Before control mode has started, one answered `list-windows`: a
        // headless caller asking what a server holds must neither bring one
        // into being nor read a host that did not answer as a host holding
        // nothing.
        //
        // Neither lists a host whose socket is a guess: a sweep that read one
        // would clear its backoff and ask it every pass.
        if !self.attached() {
            self.known_socket()?;
            return Ok(self.discover_answered()?);
        }
        // A session the probe cannot see is classified by the answered
        // listing, which tells a server that holds nothing from one that did
        // not answer — read as empty, the second clears a sweep's backoff and
        // lets a relaunch start a second agent.
        if !self.session_exists() {
            return self.discover_answered().map_err(Into::into);
        }
        // Once control mode is up, route through `ctrl_command` so a dead
        // connection is reconnected, and the listing retried when it never
        // reached tmux (like every other control-mode call).
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

    fn create_window(
        &self,
        spec: &WindowSpec<'_>,
    ) -> crate::backend::contract::BackendResult<PaneId> {
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
    ) -> crate::backend::contract::BackendResult<(PaneId, BackendLiveness)> {
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
                return Err(crate::backend::contract::BackendError::Refused(
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
                    return Err(crate::backend::contract::BackendError::Refused(
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

    fn locate(&self, owner: Owner<'_>) -> crate::backend::contract::BackendResult<Placed> {
        self.known_socket()?;
        // A one-shot `list-windows`, and deliberately nothing more: starting
        // control mode would bring a server into being where there was none.
        // Answered, so an unreachable host is an `Err` rather than a listing
        // that reads the same as "no such window" — see `TmuxCompatible::answered_absent`.
        // One listing serves both roles: an ssh round trip per role would
        // double the cost of every remote teardown.
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

    fn rename_windows(
        &self,
        owner: Owner<'_>,
        to: &str,
    ) -> crate::backend::contract::BackendResult<()> {
        self.known_socket()?;
        // Located under the name the session *had*, stamp first, so a
        // namesake's window is never the one renamed. The name is more than
        // looks where a window carries no stamp (no window options, or one spawned before
        // stamping): the name is then all that finds it.
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
    ) -> crate::backend::contract::BackendResult<()> {
        if !control_mode::is_valid_pane_id(backend_id) {
            return Err(
                anyhow::anyhow!("refusing to stamp an invalid pane id: {backend_id:?}").into(),
            );
        }
        // Nothing to write where an option is not a window's
        // ([`Self::stamps_are_per_window`]): such a server would take this as
        // a *global* one and hand it back as every window's identity. Ok rather
        // than an error for the same reason `set_pane_retention` is — the
        // caller is not being refused, there is simply no per-window option to
        // set, and `WindowIndex` resolves by name there (ADR-25).
        if !self.stamps_are_per_window() {
            return Ok(());
        }
        // `-w`: the option belongs to the window, not the pane, so a pane that
        // is split or replaced inside it does not take the identity with it.
        // Spelled once: on a one-shot path each `target` asks the version.
        let target = self.target(backend_id)?;
        if !self.attached() {
            // One-shot, for a caller with no connection of its own — a
            // headless restore or `session register` claiming a window.
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
        // As at creation, and for the same reason: the invariant a stamp is only
        // meaningful under is enforced where the stamp is written, rather than
        // left to a resolver that is required to refuse the pair.
        let _ = self.retire_duplicate_windows(session_id, role);
        Ok(())
    }

    fn reap_exited(&self) {
        if self.known_socket().is_err() {
            return;
        }
        // `run-shell` answers once its own child is reaped, and tmux reaps
        // every exited child it has in the same pass.
        if let Err(e) = self.one_shot("run-shell (reap)", &["run-shell", "true"]) {
            debug!("could not have the server reap its exited panes: {e:#}");
        }
    }

    fn pane_state(&self, pane: &str) -> crate::backend::contract::BackendResult<PaneState> {
        self.known_socket()?;
        // One `display-message` for everything the multiplexer knows, plus at
        // most one `ps` to turn the cheap command *name* into the foreground
        // process's argv.
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
        // The tty is the machine the pane runs on; this `ps` reads this one.
        if !self.transport.is_remote() {
            if let Some((argv0, command)) = tty.as_deref().and_then(foreground_process_on_tty) {
                state.foreground_process = Some(argv0);
                state.foreground_command = Some(command);
            }
        }
        Ok(state)
    }

    /// Read off the `env PATH=…` prefix a local spawn writes in front of the
    /// window's program, which tmux keeps verbatim in `#{pane_start_command}`.
    /// Deliberately not `/proc/<pid>/environ`: reading another process's
    /// environment needs `PTRACE_MODE_READ`, which Debian and Ubuntu restrict
    /// to a tracer's own descendants by default (`kernel.yama.ptrace_scope =
    /// 1`) — so it would answer for a `doctor` run from the TUI and refuse the
    /// same question typed into a terminal. tmux's own record has no such
    /// rule, and no platform gate either.
    fn pane_path(&self, pane: &str) -> crate::backend::contract::BackendResult<Option<String>> {
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

    fn is_dead(&self, backend_id: &str) -> crate::backend::contract::BackendResult<bool> {
        // Bounded, because the only caller is the interface's own loop deciding
        // where a chord goes (`coordinator::input`'s passthrough gate). The
        // unbounded ask ran out `COMMAND_TIMEOUT`, reconnected and ran out
        // again on a link that had stopped carrying anything — twenty-odd
        // seconds of an interface that answered nothing, to settle a question
        // whose honest answer when the host says nothing is the one the caller
        // already reads an error as: not known to be dead. Not `admit_server`
        // either, for the same reason as `resize`: a connection not admitted
        // is asked nothing, and answers that error.
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

    fn kill(&self, backend_id: &str) -> crate::backend::contract::BackendResult<()> {
        if !self.attached() {
            // The teardown path's kill: one-shot, because opening control mode
            // to kill a window would *create* the server and the thurbox
            // session where they are absent — how tearing a session down came
            // to leave empty servers on other people's machines.
            self.known_socket()?;
            return Ok(self.kill_window_at(backend_id)?);
        }
        // Admitted before the reader is let go: a refused kill leaves the pane
        // running, and it must keep its reader.
        // The window, not only the pane — see `kill_window_at`.
        let (verb, target) = self.kill_command(backend_id)?;
        let _ = self.unregister_pane(backend_id);
        match self.ctrl_command(&format!("{verb} -t {target}")) {
            Err(e) if !typed_absence(&e) => Err(e.into()),
            _ => Ok(()),
        }
    }

    fn pane_pid(&self, backend_id: &str) -> crate::backend::contract::BackendResult<Option<u32>> {
        // Spaces, not `PANE_STATE_SEP`: the control client carries no
        // `DISPLAY_FLAGS`, so a tmux that does not take it for UTF-8 prints
        // that byte as `_`. The pid and the pane id hold no space, so the
        // window name, last, keeps any of its own.
        let format = "#{pane_pid} #{pane_id} #{window_name}";
        let target = self.target(backend_id)?;
        let raw = if self.attached() {
            self.ctrl_command(&format!("display-message -t {target} -p '{format}'"))?
        } else {
            // A pane that is already gone has no pid, which is not an error.
            self.tmux_output(&["display-message", "-p", "-t", &target, format])
                .unwrap_or_default()
        };
        Ok(parse_pane_pid_answer(backend_id, &raw))
    }

    fn pane_pids(&self) -> crate::backend::contract::BackendResult<HashMap<String, u32>> {
        let result = self.ctrl_command("list-panes -a -F '#{pane_id} #{pane_pid}'")?;
        Ok(control_mode::parse_pane_pids(&result))
    }

    fn pane_ids(
        &self,
    ) -> crate::backend::contract::BackendResult<std::collections::HashSet<String>> {
        let result = self.ctrl_command("list-panes -a -F '#{pane_id}'")?;
        Ok(control_mode::parse_pane_ids(&result))
    }

    fn shutdown(&self) {
        // Closed on the calling thread, which is what lets quit fan the
        // (blocking) teardown out across backends — explicitly, because a
        // caller mid-command still holds a clone of the handle, and waiting
        // for its drop would leave the child and reader thread to outlive
        // quit. That caller's command fails, and `closed` keeps it from
        // reconnecting. Idempotent: the mutex holds `None` afterwards.
        self.closed
            .store(true, std::sync::atomic::Ordering::Release);
        let taken = self.link.control.lock().ok().and_then(|mut c| {
            // The admission was the connection's, and goes with it.
            self.link
                .generation
                .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
            c.take()
        });
        if let Some(control) = taken {
            control.close();
        }
        let retiring = self
            .link
            .retiring
            .lock()
            .map(|mut retiring| std::mem::take(&mut *retiring))
            .unwrap_or_default();
        for control in retiring.iter().filter_map(std::sync::Weak::upgrade) {
            control.close();
        }
    }

    /// The shell-pane command must match the OS of the machine the pane runs
    /// on ([`Self::platform`](Server)), not the local binary's — reading
    /// the local `$SHELL`/`%COMSPEC%` shipped e.g. `/bin/zsh` to a remote
    /// Windows pane ("CommandNotFoundException"). Remote hosts get a shell
    /// that exists there by construction: `powershell` on a Windows host,
    /// whichever multiplexer serves it, and `/bin/sh` on a POSIX/WSL host (the
    /// local `$SHELL` may not be installed there). This machine gets its own
    /// `$SHELL`, or `%COMSPEC%` on Windows.
    ///
    /// This is only the *bootstrap* for a remote Unix pane: `spawn` upgrades
    /// it to the user's own interactive login shell via
    /// `remote_shell_pane_command` so the pane matches an `ssh <host>` login
    /// (rc files, prompt, aliases, `PATH`).
    fn default_shell(&self) -> String {
        crate::shell::HostShell::for_platform(self.platform)
            .default_shell(self.transport.is_remote())
    }
}

impl<M: TmuxCompatible> PaneIo for Server<M> {
    fn send_text(
        &self,
        pane: &str,
        text: &str,
        submit: bool,
    ) -> crate::backend::contract::BackendResult<()> {
        self.known_socket()?;
        let target = self.target(pane)?;
        self.refuse_exited(pane, &target)?;
        // Bracketed-paste-wrapped either way (see `TmuxCompatible::paste_args`), so the
        // text arrives literally: no shell is involved, and the wrap is also
        // what keeps a leading `-` from reading as a flag and a newline from
        // submitting the line before it.
        for paste in M::paste_args(&target, text) {
            let argv: Vec<&str> = paste.iter().map(String::as_str).collect();
            self.one_shot(&paste[0], &argv)?;
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
    ) -> crate::backend::contract::BackendResult<()> {
        self.known_socket()?;
        // A detached timer on the server: the headless caller exits long
        // before the agent it launched is ready for input.
        // Spelled, then admitted: a prompt too long to schedule is refused
        // without asking the server anything.
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

    fn send_key(&self, pane: &str, key: &Key) -> crate::backend::contract::BackendResult<String> {
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
    ) -> crate::backend::contract::BackendResult<String> {
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

    fn text_before_cursor(
        &self,
        pane: &str,
    ) -> crate::backend::contract::BackendResult<Option<String>> {
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
        // One visible row, unjoined: `capture` joins wrapped lines, which
        // would put the cursor's row somewhere else.
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

impl<M: TmuxCompatible> SessionBackend for Server<M> {
    fn name(&self) -> &str {
        &self.name
    }

    /// Each answer is what this multiplexer can report, never the machine's
    /// OS or thurbox's: a server that reports a deleted window in control mode
    /// (`%window-close`, [`TmuxCompatible::WINDOW_EVENTS`]) ends the pane's
    /// stream with it and is not polled; a snapshot is exact only where a reply
    /// queues behind the pane output ahead of it
    /// ([`TmuxCompatible::SNAPSHOTS`]); an exited agent's window stays only
    /// where the window can be told to keep its dead pane
    /// ([`TmuxCompatible::WINDOW_SETTINGS`], `remain-on-exit`).
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            needs_liveness_poll: !M::WINDOW_EVENTS,
            supports_snapshots: M::SNAPSHOTS,
            keeps_exited_windows: M::WINDOW_SETTINGS,
        }
    }
}

/// What a search for a server concluded — see `Server::find_server`.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Search {
    /// A server answers, or answers again after `SIGUSR1`.
    Found,
    /// No server is running for the socket: one may be started.
    Absent,
    /// A server is running for the socket and cannot be reached — it did not
    /// answer `SIGUSR1`, or is bound to a socket file that does not answer.
    /// Nothing may be started: a second server is the leak this guards
    /// against, and the first one's pending `SIGUSR1` would take the path
    /// from it later. Says why.
    Stuck(String),
}

/// The largest piece of an adopted pane's history handed to the reader at
/// once. The reader parses a chunk under the parser's lock, which a paint also
/// takes, so a scrollback replayed whole would hold a frame for all of it.
const SEED_CHUNK_BYTES: usize = 4096;

/// `live`, behind the captured history `seed` as chunks of at most
/// [`SEED_CHUNK_BYTES`]. Each piece is cut as the reader asks for it, so the
/// history is never held twice.
fn seeded(
    seed: Vec<u8>,
    live: crate::backend::contract::PaneOutput,
) -> crate::backend::contract::PaneOutput {
    let pieces = (0..seed.len()).step_by(SEED_CHUNK_BYTES).map(move |start| {
        let end = seed.len().min(start + SEED_CHUNK_BYTES);
        PaneChunk::Output(seed[start..end].to_vec())
    });
    Box::new(pieces.chain(live))
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
/// Captured rather than inherited — see [`Server::one_shot`] (AXI
/// principle 6, "an agent reads one stream"). tmux says nothing at all for
/// some failures, hence the fallback to the bare status.
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

/// Whether a multiplexer hid a failure behind a successful process status.
/// psmux 3.3.8 does this for unsupported tmux target syntax, on stdout; that
/// stream is inspected only where the answer is structured protocol, because
/// captured pane contents may legitimately begin `ERROR:`. Stderr remains a
/// failure channel for every one-shot command. Informational SSH advisory lines
/// are transport noise, not a remote command answer.
fn mux_reported_error(out: &std::process::Output, is_ssh: bool, stdout_is_protocol: bool) -> bool {
    !mux_stderr(out, is_ssh).trim().is_empty()
        || (stdout_is_protocol
            && String::from_utf8_lossy(&out.stdout)
                .trim_start()
                .starts_with("ERROR:"))
}

/// Stderr that came from the command rather than an informational SSH banner.
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

/// How tmux names each of [`Key::NAMED`] — `ctrl-<letter>` is `C-<letter>`.
///
/// `enter`, `escape`, `tab`, `backspace` and `ctrl-<letter>` are also the set
/// psmux implements (its adapter's key encoding, `backend::psmux`);
/// the rest are tmux-only, which is what a Windows host runs into.
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
    // `DC` is tmux's own (terminfo-derived) name for Delete. Current tmux also
    // answers to `Delete` — both send `\x1b[3~` — but an older one that did not
    // would type the *name* into the pane rather than refuse it, so the table
    // names the conservative one.
    ("delete", "DC"),
];

/// `key` as tmux's `send-keys` spells it.
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

/// The part of `line` that fills its first `cells` terminal columns. A wide
/// character takes two, which is how the multiplexer counts the cursor.
fn cells_before(line: &str, cells: usize) -> String {
    let mut width = 0;
    line.chars()
        .take_while(|c| {
            width += unicode_width::UnicodeWidthChar::width(*c).unwrap_or(0);
            width <= cells
        })
        .collect()
}

/// Whether a `display-message` answer came from the pane it was asked about.
///
/// It need not have: against a target it cannot resolve, `display-message`
/// exits 0 and answers for the client's current pane — or, for a pane id that
/// is gone, prints nothing. Reporting a stranger's shell as this pane's
/// foreground process is the plausible wrong answer every field is built to
/// avoid. A pane id must come back as itself; callers using a window target
/// must receive that window.
fn answered_for(target: &str, window: Option<&str>, pane: Option<&str>) -> bool {
    match target.split_once(":=") {
        Some((_, name)) => window == Some(name),
        None => pane == Some(target),
    }
}

/// `pane_pid`'s `#{pane_pid} #{pane_id} #{window_name}` answer, or `None`
/// unless it is `target`'s own.
///
/// Read off the last line: RMUX's control mode can put a pane's `%output`
/// ahead of it in the same reply, and the answer is always one line.
fn parse_pane_pid_answer(target: &str, raw: &str) -> Option<u32> {
    let answer = raw.trim_end_matches(['\r', '\n']).lines().last()?;
    let mut fields = answer.splitn(3, ' ');
    let (pid, pane, window) = (fields.next(), fields.next(), fields.next());
    if !answered_for(target, window, pane) {
        return None;
    }
    pid.and_then(|pid| pid.parse().ok())
}

/// Where `register_pane` learnt a pane lives.
#[derive(Debug, PartialEq, Eq)]
struct PaneLocation {
    window: String,
    /// Rows and columns, where they were asked for.
    size: Option<(u16, u16)>,
    sized_by: Option<String>,
}

/// `register_pane`'s `#{pane_id} #{window_id} [rows cols [sizer]]` answer, or
/// `None` unless it is `pane`'s own and names a window.
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

/// Field `n` of a separated `display-message` answer, `None` when empty.
fn pane_answer_field(raw: &str, n: usize) -> Option<String> {
    normalized_pane_answer(raw)
        .split(PANE_STATE_SEP)
        .nth(n)
        .filter(|field| !field.is_empty())
        .map(str::to_string)
}

/// What [`Server::pane_state_seed`] asks `display-message` for: the six mouse
/// flags [`mouse_seed_bytes`] reads, comma-separated, then the host and title.
const MOUSE_FLAGS_FORMAT_THEN_TITLE: &str = "#{mouse_standard_flag},#{mouse_button_flag},\
     #{mouse_all_flag},#{mouse_any_flag},#{mouse_sgr_flag},#{mouse_utf8_flag}|\
     #{host_short}|#{pane_title}";

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
    // The pane id, which `answered_for` reads on its own.
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

/// The agent's command as an **absolute path**, resolved against thurbox's own
/// `PATH`, so the multiplexer never has to resolve it.
///
/// thurbox used to hand tmux a bare name (`claude`) and let tmux find it. Which
/// resolver ran, and with which `PATH`, was not thurbox's to choose:
///
/// - tmux copies the *client's* `PATH` into the new pane only for an
///   **unattached** client (`spawn.c`: "the session one is replaced from the
///   client … only unattached clients"). thurbox's control-mode client is
///   attached, so [`Server::spawn`](PaneStream::spawn) — a restart, a plugin program, the
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
/// already a path, when nothing on `PATH` matches, or on a Windows machine (a
/// bare name there wants `PATHEXT` semantics this deliberately does not have). A `command` that is a shell function, an alias,
/// or a binary installed *after* this resolves therefore behaves exactly as it
/// did before: resolution is an improvement where it succeeds, never a new way
/// to fail.
pub(crate) fn resolve_local_program(command: &str) -> String {
    if crate::shell::local_platform() == Platform::Windows {
        return command.to_string();
    }
    match crate::paths::resolve_on_path(command) {
        Some(path) => path.to_string_lossy().into_owned(),
        None => command.to_string(),
    }
}

/// The window's environment and the program it runs, which close a one-shot
/// `new-window`'s arguments on a server that honours `-e` and runs a window
/// command through a POSIX shell: tmux's [`TmuxCompatible::push_window_program`].
pub(in crate::backend) fn push_posix_window_program(
    tmux: &mut Command,
    command: &str,
    args: &[String],
    env: &HashMap<String, String>,
) {
    for (k, v) in env {
        tmux.args(["-e", &format!("{k}={v}")]);
    }
    // Pass the command + args as a single argv list. tmux treats trailing args
    // as the command to run inside the window. Resolved here for the same
    // reason the control-mode path resolves it (see `resolve_local_program`):
    // this path happens to get thurbox's own `PATH` because its client is
    // unattached, but a session must not launch differently depending on which
    // of the two created it — a session created here and later restarted
    // through control mode would otherwise resolve against two different
    // environments.
    let program = resolve_local_program(command);
    // `PATH` is the one variable `-e` cannot carry, so the CLI's directory
    // rides in the command instead (see `path_prefix_args`) — but **how many
    // arguments** that leaves is itself load-bearing, so the prefix is spelled
    // to keep the count tmux would have seen.
    //
    // tmux runs a **one-argument** window command through its `default-shell`
    // and a multi-argument one through `execvp` (`spawn.c`). A command session
    // with no args is the one-argument case, and `--command "sleep 300"` only
    // ever worked because that shell split it. Pushing the prefix as two more
    // argv entries moved it to `execvp`, which has no splitting to do: the pane
    // died instantly with status 127 and a `sleep 300: No such file` from
    // `env`. So with no args the prefix joins the same single token and the
    // shell still does the splitting it always did.
    if args.is_empty() {
        // One token means a shell reads it, and a shell reads text — so this is
        // the one place the prefix has to be spellable as text. An unspellable
        // one (a `PATH` that is not UTF-8) and an absent one lead to the same
        // command: the program alone, exactly as before.
        //
        // The program itself is **not** escaped: it is what the shell was
        // already splitting, and escaping it now would break the very commands
        // this branch exists to keep working.
        match shell_prefix_tokens() {
            Some(mut token) => {
                token.push(program);
                tmux.arg(token.join(" "));
            }
            None => {
                tmux.arg(program);
            }
        }
    } else {
        // Several tokens already go to `execvp`, so the prefix rides as argv
        // and the `PATH` keeps its bytes.
        for arg in path_prefix_args() {
            tmux.arg(arg);
        }
        tmux.arg(program);
    }
    for a in args {
        tmux.arg(a);
    }
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
/// So the CLI's own directory goes in front ([`crate::paths::resolve_cli_binary`] — the
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
fn path_prefix_args() -> Vec<std::ffi::OsString> {
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
fn shell_prefix_tokens() -> Option<Vec<String>> {
    let args = path_prefix_args();
    if args.is_empty() {
        return None;
    }
    args.iter()
        .map(|a| a.to_str().map(control_mode::shell_escape))
        .collect()
}

/// This process's `PATH` with the directory holding this build's `thurbox-cli`
/// in front. `None` when [`crate::paths::resolve_cli_binary`] fell back to a bare name —
/// there is no directory to add, and pinning a `PATH` with nothing to add to it
/// would only restate what the pane was going to inherit anyway.
#[cfg(not(windows))]
fn path_with_cli_directory() -> Option<std::ffi::OsString> {
    let cli = crate::paths::resolve_cli_binary();
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

/// Never on a Windows machine, which has no POSIX `env` to put in front of a
/// program — and whose own multiplexer runs a command its own way (psmux folds
/// the environment into a PowerShell token).
#[cfg(windows)]
fn path_prefix_args() -> Vec<std::ffi::OsString> {
    Vec::new()
}

/// Whether a server's `#{version}` answer is a version and nothing else:
/// dotted numbers, at least two (`3.3.8`, `3.4`). What [`admit_server`]
/// will hold a floor against — a floor check lets an answer it cannot read
/// through, which is right for a banner and wrong for the server a pane is
/// about to be named to.
///
/// [`admit_server`]: Server::admit_server
pub(in crate::backend) fn is_version_answer(answer: &str) -> bool {
    let parts: Vec<&str> = answer.split('.').collect();
    parts.len() >= 2
        && parts
            .iter()
            .all(|part| part.bytes().all(|b| b.is_ascii_digit()) && part.parse::<u32>().is_ok())
}

fn typed_absence(error: &anyhow::Error) -> bool {
    matches!(
        error.downcast_ref::<crate::backend::contract::BackendError>(),
        Some(crate::backend::contract::BackendError::Absent(_))
    )
}

/// Assert that each of an adapter's own "nothing there" `replies`, as its
/// binary printed it, reads as absence through the one classifier every
/// refusal path uses — a one-shot's stderr and a control-mode `%error` — and
/// as an empty listing exactly when it is marked `true`: a server that holds
/// nothing (no server, or no thurbox session on it), rather than a pane or
/// window gone from a live one.
#[cfg(all(test, unix))]
pub(in crate::backend) fn assert_absence_replies<M: TmuxCompatible>(replies: &[(&str, bool)]) {
    use std::os::unix::process::ExitStatusExt;
    let backend = Server::<M>::local(crate::session::settings::default_scrollback_lines());
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
                crate::backend::BackendError::Absent(_)
            ),
            "{name} one-shot: {reply}"
        );
        assert!(
            matches!(
                backend.refusal(reply, String::new()),
                crate::backend::BackendError::Absent(_)
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

mod bootstrap;
mod connection;
mod locator;
mod sizing;
mod spawn;
mod status;
#[cfg(test)]
mod tests;
