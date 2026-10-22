//! The tmux protocol's session backend: one server of a multiplexer that speaks
//! the tmux command and control-mode grammar, on this machine or on a host.
//!
//! [`Server`] is everything tmux and psmux share. It is generic over a
//! [`TmuxCompatible`] multiplexer, and every way one of them differs from the
//! other is a body in that multiplexer's adapter (`backend::tmux`,
//! `backend::psmux`) — never a branch on a binary's name or the build OS here.

use std::borrow::Cow;
use std::collections::HashMap;
use std::io::{Cursor, Read};
use std::marker::PhantomData;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};

use anyhow::{bail, Context, Result};
use tracing::{debug, info, warn};

use crate::backend::contract::{
    AdoptedSession, DiscoveredSession, Key, Owner, PaneId, PaneSize, PaneState, Placed,
    SessionBackend, SpawnedSession, WindowRole, WindowSpec,
};
use crate::backend::identity::{window_name_for, Located, WindowIndex, SHELL_WINDOW_PREFIX};
use crate::backend::instance::{host_socket, known_host_socket, learned_host_socket, local_socket};
use crate::backend::tmux_compat::control_mode::{
    self, is_broken_pipe, is_recv_timeout, mouse_seed_bytes, shell_escape, title_seed_bytes,
    ControlMode, ControlModeReader, ControlModeWriter, ControlPolicy, PaneInput, SIZED_BY,
    SIZER_OPTION,
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

    /// The flags that make a `display-message` answer keep its separators
    /// whatever the locale (see `PANE_STATE_UTF8_FLAG`'s reason in the tmux
    /// adapter), before the command itself.
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
    fn paste_args(target: &str, text: &str) -> Vec<Vec<String>>;

    /// The `run-shell` script that pastes `text` into `target` on `mux -L
    /// socket`, waits a beat so the paste is consumed, then presses Enter —
    /// run through the server's own shell — or why that shell cannot run it.
    fn deferred_paste_script(mux: &str, socket: &str, target: &str, text: &str) -> Result<String>;

    /// How a pane's input is typed through control mode, and whether a paste
    /// goes another way.
    fn pane_input(transport: &TmuxTransport, socket: &str) -> Arc<dyn PaneInput>;

    /// What a control-mode connection to this server may expect of it.
    fn control_policy(transport: &TmuxTransport, session: &str) -> ControlPolicy;

    /// Whether a hook in this server's panes can report its state through the
    /// pane option ([`control_mode::REMOTE_HOOK_STATE_OPTION`]) and have it
    /// read back. Where it cannot, the backend has no status channel: no hook
    /// command is offered, nothing is recorded, and a listing is refused.
    const HOOK_STATUS: bool;

    /// The in-pane command that sets the option, the state word to follow —
    /// whatever this server needs to find its own pane from inside one.
    fn hook_signal_command(server: &Server<Self>) -> String
    where
        Self: Sized;
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

/// What [`SessionBackend::window_panes`] lists, read by [`named_panes`].
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
/// [`SessionBackend::record_hook_state`] uses for hook state. See ADR-25.
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

/// The most a command issued from the interface's own loop may wait for its
/// answer, where one is wanted.
///
/// Sized against the two things it sits between: a healthy round trip over an
/// already-open connection is sub-millisecond, and the budget a keypress has
/// before the interface reads as frozen is a fraction of a second. Generous
/// enough that a loaded machine or a slow link still gets a real answer; short
/// enough that a link carrying nothing costs a hitch instead of a freeze.
const LOOP_COMMAND_BUDGET: std::time::Duration = std::time::Duration::from_millis(250);

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

/// Whether a failed listing is exactly the multiplexer's absence reply.
///
/// A refusal does not make any stdout trustworthy, and another stderr line
/// means another failure happened beside it. SSH advisory banners are the one
/// transport-owned exception; [`mux_stderr`] removes those before this check.
fn listing_output_is_absence(is_ssh: bool, output: &std::process::Output) -> bool {
    if !output.stdout.is_empty() {
        return false;
    }
    let stderr = mux_stderr(output, is_ssh);
    let mut answers = stderr
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty());
    let Some(answer) = answers.next() else {
        return false;
    };
    answers.next().is_none()
        && listing_is_absence(is_ssh, output.status.code(), answer)
        && raw_listing_answered_absent(answer)
}

/// The unwrapped refusal shapes a one-shot listing can emit.
///
/// [`mux_answered_absent`] also reads errors after callers have added command
/// context, so it deliberately searches within a string. Raw process output
/// has no such wrapper and must match one documented shape from its first byte.
fn raw_listing_answered_absent(error: &str) -> bool {
    if error == "psmux: no server running on session" {
        return true;
    }
    if let Some(socket) = error.strip_prefix("psmux: no server running on session '") {
        return socket
            .strip_suffix('\'')
            .is_some_and(|socket| !socket.is_empty() && !socket.contains('\''));
    }
    if let Some(path) = error.strip_prefix("error connecting to ") {
        return path
            .strip_suffix(" (No such file or directory)")
            .is_some_and(|path| !path.is_empty());
    }
    [
        "no server running on ",
        "can't find session: ",
        "session not found: ",
    ]
    .into_iter()
    .any(|prefix| {
        error
            .strip_prefix(prefix)
            .is_some_and(|value| !value.is_empty())
    })
}

/// Whether a multiplexer's stderr is its *own* answer that there is nothing to
/// act on, rather than any of the ways a question can fail to be answered.
///
/// The distinction the remote teardown rests on, and the reason this list is
/// as short as it is. Only the exact answers tmux and psmux are documented to
/// give for "there is no server" and "there is no such session" count;
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
    // The server is up and holds no session by that name — tmux, then psmux.
    error.contains("can't find session") || error.contains("session not found")
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

/// Rows of history a snapshot carries: as many as the rebuilt terminal keeps,
/// under the same ceiling every capture here has.
fn snapshot_history() -> usize {
    crate::session::settings::global()
        .scrollback_lines
        .min(MAX_CAPTURE_LINES as usize)
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
    /// The live connection, taken only long enough to clone or swap the
    /// handle: [`ControlMode`] serialises its own commands, so a caller's round
    /// trip — up to `COMMAND_TIMEOUT` on a link that stopped answering — is
    /// never anybody else's wait (#1472).
    pub(in crate::backend) control: Mutex<Option<Arc<ControlMode>>>,
    /// Held while a connection is opened, by [`Self::ensure_control`] and
    /// [`Self::reconnect_control`], so a handshake happens once however many
    /// callers find the connection missing or dead, and outside `control`.
    connecting: Mutex<()>,
    /// Connections replaced but still answering what was sent on them
    /// ([`ControlMode::retire`]), for [`SessionBackend::shutdown`] to close.
    retiring: Mutex<Vec<std::sync::Weak<ControlMode>>>,
    /// Set by [`SessionBackend::shutdown`]: from then on no connection is
    /// opened, so a worker still holding the registry as the process quits
    /// cannot bring back the one quit just closed.
    pub(in crate::backend) closed: std::sync::atomic::AtomicBool,
    /// Bumped under `control` by each connection installed, which may be to a
    /// server started since: what an admission ([`Self::admit_server`]) is
    /// good for.
    connection_generation: std::sync::atomic::AtomicU64,
    /// The `connection_generation` the server was found new enough at over
    /// that connection, plus one; zero for never.
    admitted: std::sync::atomic::AtomicU64,
    /// Held while the multiplexer is asked, so callers arriving together —
    /// every pane re-registered after a reconnect — wait for one question:
    /// they take a connection's answer from `admitted`, and a failure on the
    /// same connection from the generation and reason held here. A timeout
    /// reconnects, which is a new connection, so those behind it ask that
    /// one afresh; a one-shot's success is not kept, so each of them asks
    /// again.
    admitting: Mutex<Option<(u64, String)>>,
    /// Counts the questions settled, so a waiter can tell one was answered
    /// while it waited.
    questions: std::sync::atomic::AtomicU64,
    /// This backend's name in [`SIZER_OPTION`] — see [`Self::resize`].
    pub(in crate::backend) sizer: String,
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

/// `(rows, cols)` within what `resize-window` accepts, so a resize an `if-shell`
/// runs can never be the one that fails (see `Server::resize`). tmux's
/// bounds are 1 and `WINDOW_MAXIMUM`, 10000.
fn tmux_size(rows: u16, cols: u16) -> (u16, u16) {
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

impl<M: TmuxCompatible> Default for Server<M> {
    fn default() -> Self {
        Self::local()
    }
}

impl<M: TmuxCompatible> Server<M> {
    /// [`Self::local`].
    pub fn new() -> Self {
        Self::local()
    }

    /// `M` on this machine, named by the route it serves (`local:tmux`,
    /// `local:psmux`) whatever OS this is.
    pub fn local() -> Self {
        let name = crate::session::Route::local(Some(M::MULTIPLEXER)).format();
        let socket = local_socket();
        let mut transport = TmuxTransport::local(M::MULTIPLEXER.name());
        if M::SOCKET_DIR && Platform::local() == Platform::Posix {
            if let Some(dir) = crate::backend::instance::local_socket_dir(&socket) {
                transport = transport.with_socket_dir(SocketDir::Dir(dir.display().to_string()));
            }
        }
        Self::with_transport(transport, socket, TMUX_SESSION, name)
    }

    /// A backend over an explicit transport. A remote one is taken for a POSIX
    /// host until [`Self::for_host`] says which host it is.
    pub fn with_transport(
        transport: TmuxTransport,
        socket: impl Into<String>,
        session: impl Into<String>,
        name: impl Into<String>,
    ) -> Self {
        let platform = if transport.is_remote() {
            Platform::Posix
        } else {
            Platform::local()
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
            host: None,
            platform,
            multiplexer: PhantomData,
        }
    }

    /// `M` on `host`, reached the way its entry says and running as its
    /// platform — see [`Self::on_host`].
    pub fn for_host(host: &HostDef) -> Self {
        Self::on_host(host, HostLauncher::for_host(host), host.platform())
    }

    /// `M` on `host`, reached through `launcher` on a machine of `platform`:
    /// its binary over SSH, or inside a WSL distro via `wsl.exe`. Named by the
    /// route it drives (`ssh:<host.name>:<mux>` / `wsl:…`) whatever the host
    /// prefers — a row written for tmux is served by tmux on a host that has
    /// since moved to something else. Its socket is the host's
    /// ([`host_socket`]), never this instance's own, and its session name is
    /// the default unless the host overrides it.
    pub fn on_host(host: &HostDef, launcher: HostLauncher, platform: Platform) -> Self {
        let session = host
            .session
            .clone()
            .unwrap_or_else(|| TMUX_SESSION.to_string());
        let mut transport = TmuxTransport::remote(launcher, M::MULTIPLEXER.name());
        if platform == Platform::Windows {
            transport = transport.on_windows();
        }
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

    /// The transport, carrying the socket directory the server was found in —
    /// what a control-mode client is started over.
    fn located_transport(&self) -> TmuxTransport {
        self.host_transport().with_socket_dir(self.socket_dir())
    }

    /// The transport, carrying the host's `PATH` (`path_prepend`, then its
    /// login shells', then the launcher's — [`crate::agent::host_path`]), so
    /// every command thurbox runs there finds the multiplexer where the
    /// new-session flow's probe found it (#1461). Asked per command rather
    /// than at construction: the first ask probes the host, and building a
    /// backend must not wait on one.
    fn host_transport(&self) -> TmuxTransport {
        let path = match (&self.host, self.platform) {
            (Some(host), Platform::Posix) => crate::agent::host_path::assignment_for(host),
            _ => None,
        };
        self.transport.clone().with_path(path)
    }

    /// Whether this server's socket directory is thurbox's to choose and to
    /// look in ([`TmuxCompatible::SOCKET_DIR`]) — only on a POSIX machine.
    fn manages_socket_dir(&self) -> bool {
        M::SOCKET_DIR && self.platform == Platform::Posix
    }

    /// Where a server started now would put its socket: what the host's own
    /// CLI said its servers use, once it has said, else the directory this
    /// backend was built with.
    fn preferred_socket_dir(&self) -> SocketDir {
        let learned = self.host.as_ref().and_then(|host| {
            crate::backend::instance::learned_host_socket_dir(&host.backend_name())
        });
        match learned {
            Some(Some(dir)) => SocketDir::Dir(dir),
            Some(None) => SocketDir::Inherited,
            None => self.transport.socket_dir().clone(),
        }
    }

    /// Every directory a running server for this socket may be in, the
    /// preferred one first: that one, the one this backend was built with
    /// when a host has since said otherwise, and tmux's default — where every
    /// server ran before thurbox chose a directory, and so where one started
    /// by an earlier build still is.
    pub(in crate::backend) fn socket_dir_candidates(&self) -> Vec<SocketDir> {
        let mut dirs = vec![self.preferred_socket_dir()];
        for dir in [self.transport.socket_dir().clone(), SocketDir::Inherited] {
            if !dirs.contains(&dir) {
                dirs.push(dir);
            }
        }
        dirs
    }

    /// The machine and socket a located directory is remembered under: one
    /// answer per server, shared by every backend this process builds for it.
    fn located_key(&self) -> String {
        let machine = self
            .host
            .as_ref()
            .map_or_else(|| "local".to_string(), HostDef::backend_name);
        format!("{machine}\0{}", self.socket())
    }

    /// The directory this server's socket is in. Found once per process
    /// ([`Self::socket_file_seen`], else [`Self::find_server`]) before the
    /// first command, and again whenever a
    /// command finds no server ([`Self::relocate`]); remembered in between.
    pub(in crate::backend) fn socket_dir(&self) -> SocketDir {
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

    /// Look for this socket's server again, after a command found none:
    /// [`Search::Found`] when one was — answering in a directory, or brought
    /// back by `SIGUSR1` — so the caller asks again rather than starting a
    /// second, and [`Search::Stuck`] when one runs out of reach.
    ///
    /// `throttled` is for the questions asked on a timer — a listing, a
    /// reconnect — which on a host with no server would otherwise pay a
    /// search per pass: those repeat the last search's verdict within
    /// [`RELOCATE_EVERY`] of it, a server out of reach included. What is about
    /// to start a server always searches.
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

    /// The error a server out of reach ([`Search::Stuck`]) is reported as.
    fn out_of_reach(&self, why: &str) -> anyhow::Error {
        crate::backend::contract::BackendError::Unreachable(format!(
            "{}: {why}; not starting a second server beside it \
             (see 'An orphaned tmux server' in docs/CONFIG.md)",
            self.name
        ))
        .into()
    }

    /// After the session probe found nothing and before anything starts a
    /// server: whether looking for it again ([`Self::relocate`]) brought the
    /// session back. Says at INFO which way it went when it did not — into a
    /// running server, or a new one — and fails when a server runs out of
    /// reach ([`Search::Stuck`]), which nothing may start a second beside.
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

    /// On first use, a local socket file at the preferred path is taken as
    /// the answer, so the common start costs no process; a stale file fails
    /// the first command, which then asks again in full.
    fn socket_file_seen(&self) -> Option<SocketDir> {
        let preferred = self.preferred_socket_dir();
        (!self.transport.is_remote() && local_socket_file(&preferred, &self.socket()))
            .then_some(preferred)
    }

    /// Find the running server for this socket: the directory to use, and
    /// whether a server is there.
    ///
    /// A server whose socket file is gone keeps running — macOS sweeps
    /// `/tmp`, and a socket's access time is not bumped by `connect()` — and
    /// nothing reaches it. Starting another on the same name is how one Mac
    /// came to hold 93 processes and 3.1 GB of agents nobody could see, 13 of
    /// them duplicates of sessions resumed into the new server. So before
    /// anything concludes there is no server, [`LOCATE_SCRIPT`] asks on the
    /// machine itself: each candidate directory ([`Self::socket_dir_candidates`])
    /// in turn, then for a live server still bound to a candidate's missing
    /// socket path, which it sends `SIGUSR1` — tmux's documented way to
    /// recreate a removed socket. A path where a socket file exists is never
    /// signalled: tmux unlinks the path before binding, and would orphan
    /// whichever server owns it now.
    ///
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

    /// Refuse a multiplexer below [`TmuxCompatible::VERSION_FLOOR`] before a
    /// pane is named to it, or a pane born on it.
    ///
    /// Both halves of it: the running server, since an upgrade leaves a server
    /// started before it on the old code, and the client binary, since psmux
    /// before 3.3.8 resolves a `-t` in the client too (#1557). Asked once per
    /// control connection; a one-shot answer is not kept, since nothing would
    /// tell it the server had been replaced. Only the multiplexer's own words
    /// for a missing server or a missing session on it — nothing broader — are
    /// let through, since nothing named there can arrive; any other answer
    /// that is not a version new enough refuses.
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
        // Guards no state a panic could leave half-written.
        let mut last_failure = self
            .admitting
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        // Read before asking: a connection installed while the question is out
        // leaves this admission stale, and the next caller asks again.
        let generation = current();
        if self.admitted.load(Ordering::Acquire) == generation {
            return Ok(());
        }
        // A question settled while this caller waited, and failed on this
        // connection: asking again would wait it out once more.
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

    /// [`Self::admit_server`]'s question, to the client binary and then the
    /// server.
    fn ask_versions(
        &self,
        attached: bool,
        refuse_old: fn(&str, &str) -> Result<()>,
    ) -> Result<Admission> {
        // Read strictly here, then judged by the adapter's own binary check,
        // whose advice is about the binary: its server may be fine, and
        // restarting that would end every agent and change nothing.
        let mux = self.transport.mux();
        let banner = self.tmux_output(&["-V"])?;
        let Some(client) = banner_version(&banner) else {
            bail!(
                "the {mux} binary did not give a version thurbox can hold to its floor (it \
                 answered {banner:?}); put a released {mux} first on PATH"
            );
        };
        // The value just read strictly is the one judged: the whole banner,
        // read leniently, could name another line or drop a suffix.
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

    /// The server's `#{version}` from a one-shot, or `None` when the
    /// multiplexer said in its own words that there is no server or no session
    /// — and only then: `can't find window` is also what psmux 3.3.8 says of a
    /// live server refusing a target, and that server runs what comes next.
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
        if listing_output_is_absence(is_ssh, &output) {
            return Ok(None);
        }
        if !output.status.success() || !mux_stderr(&output, is_ssh).trim().is_empty() {
            return Err(anyhow::Error::from(self.command_failure(&args, &output))
                .context("could not ask the server its version"));
        }
        let answer = String::from_utf8_lossy(&output.stdout).trim().to_string();
        // psmux reports some failures on stdout with status 0
        // (`EXIT_ZERO_ERRORS`).
        if let Some(error) = answer
            .strip_prefix("ERROR:")
            .filter(|_| M::EXIT_ZERO_ERRORS)
        {
            if !error.contains('\n') && raw_listing_answered_absent(error.trim()) {
                return Ok(None);
            }
            bail!("the server refused the version question: {answer}");
        }
        Ok(Some(answer))
    }

    /// Refuse a status verb on a server whose hooks have no channel
    /// ([`TmuxCompatible::HOOK_STATUS`]).
    fn refuse_without_hook_status(&self) -> Result<()> {
        if !M::HOOK_STATUS {
            bail!(
                "{} carries no hook status: its pane-option channel is not proven",
                self.name
            );
        }
        Ok(())
    }

    /// The thurbox session on this server, created when it is not there — as
    /// the backend would create it, peers racing to included, and only once
    /// the backend agrees its binary may start one. No session config: the
    /// keeper needs none, and every spawn or attach applies it.
    fn ensure_heartbeat_session(&self) -> Result<()> {
        let exists = || self.session_exists();
        if exists() || self.found_after_all()? {
            return Ok(());
        }
        self.check_available()?;
        let mut args = vec!["new-session", "-d", "-s", &self.session];
        args.extend_from_slice(M::BOOTSTRAP_SIZE_ARGS);
        let out = self
            .mux_command(&args)?
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .map_err(|e| {
                self.transport
                    .launch_failure("Failed to run tmux command", e)
            })?;
        if !out.status.success() && !exists() {
            bail!(
                "{} new-session (heartbeat) {}",
                self.transport.mux(),
                mux_failure(&out, self.transport.is_ssh())
            );
        }
        // As `ensure_session_configured` waits after creating one: a server
        // whose `new-session -d` returns before the session answers would
        // refuse the keeper's `new-window`.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !exists() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        Ok(())
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
    /// option ([`TmuxCompatible::WINDOW_OPTIONS`]). Where it is not, a stamp
    /// read back identifies nothing, and [`parse_discovered`] drops it rather
    /// than reading one session's id as every window's.
    pub(in crate::backend) fn stamps_are_per_window(&self) -> bool {
        M::WINDOW_OPTIONS
    }

    /// One `list-windows`, with an empty answer only when the multiplexer
    /// itself said there is nothing to list.
    ///
    /// [`discover`](SessionBackend::discover) gates on the session probe and reads
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
                self.transport
                    .launch_failure("Failed to run tmux command", e)
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
                "tmux {} failed: {}",
                args.join(" "),
                mux_failure(&output, self.transport.is_ssh())
            );
        }
        if listing_output_is_absence(self.transport.is_ssh(), &output) {
            return Ok(None);
        }
        Err(self.command_failure(&args, &output).into())
    }

    /// A fresh backend on the same server, with no connection of its own yet —
    /// for work that opens control mode and should not leave it open on the
    /// backend the rest of the process shares.
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
            host: self.host.clone(),
            platform: self.platform,
            multiplexer: PhantomData,
        }
    }

    /// Whether [`SessionBackend::shutdown`] has run.
    fn is_closed(&self) -> bool {
        self.closed.load(std::sync::atomic::Ordering::Acquire)
    }

    /// Refuse to open a connection once shut down.
    fn refuse_if_closed(&self) -> Result<()> {
        if self.is_closed() {
            bail!("{} is shut down", self.name);
        }
        Ok(())
    }

    /// Whether this backend holds a control-mode connection, which is what
    /// decides whether a command goes through it or runs one-shot.
    pub(in crate::backend) fn attached(&self) -> bool {
        self.control.lock().is_ok_and(|control| control.is_some())
    }

    /// Refuse to act on a host whose socket is a guess (see
    /// [`known_host_socket`]). Always fine locally.
    fn known_socket(&self) -> Result<()> {
        match &self.host {
            Some(host) => known_host_socket(host).map(drop),
            None => Ok(()),
        }
    }

    /// [`SessionBackend::create_window`] on a host: the control-mode spawn,
    /// through a connection dropped once the window exists. The host's server
    /// keeps the window for an interface to adopt later.
    fn create_remote_window(&self, spec: &WindowSpec<'_>) -> Result<PaneId> {
        let backend = self.transient();
        backend
            .check_available()
            .context("remote host is unreachable or tmux is missing")?;
        backend.ensure_ready()?;
        let window_name = window_name_for(spec.role, spec.owner.name);
        // Headless: no live terminal, so use a sane default geometry. The TUI
        // resizes the pane to its real dimensions when it adopts the session.
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

    /// Refuse a window whose pane is already gone
    /// [`TmuxCompatible::SPAWN_SETTLE`] after it was created. Only `pane_id`
    /// itself counts: a live namesake under the same `tb-<name>` is another
    /// session's window (issue #1456).
    ///
    /// A listing that does not answer refuses nothing: the window was
    /// created, and a question left unanswered is not evidence it died.
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

    /// Why a create is refused when its window closed with its program.
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

    /// [`SessionBackend::window_panes`], asked through the control connection
    /// where there is one: a one-shot listing over ssh is re-read by the
    /// host's login shell, and `cmd.exe` takes the format's `|` for a pipe.
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

    /// What `owner`'s `role` window is, where the listing alone could not say.
    ///
    /// A multiplexer without window options (ADR-13) stamps nothing, so
    /// local recovery can select the sole fresh live namesake, but multiple live
    /// names stay unknown. A restarted server can reissue a remembered pane id
    /// to another same-name window. Locally, one stamp on two
    /// windows is repairable, and here is where repairing it matters:
    /// `stamped_match` refuses the pair by design, so without this nothing
    /// would ever look again and the session stayed unaddressable for good
    /// (issue #1207). The refusal itself is untouched — the choice is made by
    /// *retiring* a window, which is a write, and never by reading one of two
    /// as the answer.
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
        if self.transport.is_remote()
            || self
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

    /// Run a one-shot command whose failure is reported as `what` rather than
    /// by its argv: a paste's argv is the text being pasted.
    ///
    /// `output()` rather than `status()` is the point of it: a `status()` child
    /// inherits this process's stderr, so the multiplexer's own `can't find
    /// pane` would land there directly — a second, unstructured stream beside
    /// the error document the CLI puts on stdout. Captured, it becomes part of
    /// the one answer.
    fn one_shot(&self, what: &str, args: &[&str]) -> Result<std::process::Output> {
        let output = self
            .mux_command(args)?
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .map_err(|e| {
                self.transport
                    .launch_failure("Failed to run tmux command", e)
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

    /// Kill the window a pane is in with a one-shot command rather than
    /// through control mode.
    ///
    /// The teardown path's kill on a host. Opening control mode needs
    /// [`ensure_ready`](SessionBackend::ensure_ready) — and that *creates* the
    /// server and the thurbox session when they are absent, so tearing a
    /// session down on a host would leave an empty server behind. The window
    /// rather than the pane, so a window somebody split does not keep a
    /// process running in its other pane. One already gone is not an error:
    /// the teardown got what it wanted.
    fn kill_window_oneshot(&self, pane: &str) -> Result<()> {
        let (verb, target) = self.kill_command(pane)?;
        match self.run_tmux(&[verb, "-t", &target]) {
            Err(e) if !typed_absence(&e) => Err(e),
            _ => Ok(()),
        }
    }

    /// Execute a tmux command on the thurbox socket and check for errors.
    fn run_tmux(&self, args: &[&str]) -> Result<std::process::Output> {
        let output = self
            .mux_command(args)?
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .map_err(|e| {
                self.transport
                    .launch_failure("Failed to run tmux command", e)
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
    ) -> crate::backend::contract::BackendError {
        let detail = mux_failure(output, self.transport.is_ssh());
        let message = format!("tmux {} failed: {detail}", args.join(" "));
        if self.transport.is_ssh() && output.status.code() == Some(SSH_ERROR_EXIT) {
            return crate::backend::contract::BackendError::Unreachable(message);
        }
        if output.status.code().is_none() {
            return crate::backend::contract::BackendError::Io(message);
        }
        if listing_output_is_absence(self.transport.is_ssh(), output) {
            return crate::backend::contract::BackendError::Absent(message);
        }
        match control_mode::classify_refusal(&detail, message) {
            crate::backend::contract::BackendError::Absent(message)
                if !output.stdout.is_empty() =>
            {
                crate::backend::contract::BackendError::Refused(message)
            }
            classified => classified,
        }
    }

    /// Check if the thurbox tmux session exists.
    fn session_exists(&self) -> bool {
        self.tmux_run(&[M::SESSION_PROBE_COMMAND, "-t", &self.session])
            .is_ok()
    }

    /// Apply server + session config to the tmux session.
    ///
    /// Idempotent (`set-option` overwrites), so it is safe to call on every
    /// [`ensure_ready`](Self::ensure_ready) — the session may have been created
    /// elsewhere (e.g. a headless spawn) without these options, and re-applying
    /// is the single source of truth for both the TUI and headless paths.
    ///
    /// Where the server takes command lists the whole config is **one**
    /// invocation (#1243): it runs on every `session create`, and as ten
    /// processes it was most of that command's cost. A failure in the list is
    /// then re-run one option at a time, which is what tells a fatal option
    /// from a best-effort one.
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
                Err(e) => debug!("tmux option {} not set: {e}", option.args.join(" ")),
            }
        }
        Ok(())
    }

    /// Every `set-option` [`apply_session_config`](Self::apply_session_config)
    /// runs, in order.
    pub(in crate::backend) fn session_config(&self) -> Vec<ConfigOption> {
        let scope = M::SERVER_SCOPE;
        let mut config = Vec::new();
        let mut set = |args: &[&str], fatal: bool| {
            let mut command = args.to_vec();
            if M::SERVER_OPTIONS_NEED_TARGET && args.first() == Some(&scope) {
                command.splice(1..1, ["-t", &self.session]);
            }
            config.push(ConfigOption::set(&command, fatal));
        };
        // Use a non-login shell so that macOS path_helper (/etc/zprofile)
        // doesn't clobber PATH additions from ~/.zshenv (e.g. cargo, asdf).
        // For a remote backend the local `$SHELL` path may not exist on the
        // remote host, so fall back to a POSIX shell there.
        //
        // On a Windows machine we deliberately do NOT pin `default-command`:
        // `$SHELL` and `/bin/sh` don't exist there, and forcing a Windows shell
        // would have to match the multiplexer's own command-execution model, so
        // its native default shell is the safe choice. Decided by the platform
        // of the machine the server runs on — not by the OS thurbox was built
        // for (a Windows thurbox driving a WSL distro left its tmux on the
        // login shell) and not by the multiplexer's name.
        if self.platform == Platform::Posix {
            set(&[scope, "default-command", &self.config_shell()], true);
        }

        // Server-wide options every supported multiplexer understands. A
        // failure here means the server can't host sessions, so it is
        // propagated.
        set(&[scope, "default-terminal", "xterm-256color"], true);
        set(&[scope, "extended-keys", "on"], true);

        // `extended-keys-format csi-u` is best-effort: the option landed in tmux
        // 3.5, but thurbox's floor is 3.2, so an older tmux rejects it ("invalid
        // option"). It is advisory only — thurbox injects keystroke bytes directly
        // via `send-keys` (not through tmux's key forwarder), so it never
        // re-encodes what an agent receives; it just sets what `tmux show-options`
        // reports, which some agents (notably `pi`) probe at startup and warn about
        // unless it is `csi-u`. Ignoring the error keeps a 3.2–3.4 host working (pi
        // users there simply miss the hint) while 3.5+ hosts get the preferred
        // format.
        set(&[scope, "extended-keys-format", "csi-u"], false);

        for (key, val) in SESSION_OPTS {
            set(&["-t", &self.session, key, val], true);
        }

        // Window-level options — see `WINDOW_OPTS` for why these are global to
        // the server and why failing to set one is not fatal.
        for (key, val) in WINDOW_OPTS {
            set(&["-w", "-g", key, val], false);
        }

        // What only this multiplexer takes — tmux's clipboard and mouse
        // options, which psmux has no equivalent of.
        config.extend(M::session_config(&self.session));
        config
    }

    /// Ensure the thurbox tmux session exists and its options are applied,
    /// **without** starting control mode.
    ///
    /// Shared by [`ensure_ready`](Self::ensure_ready) (which then starts control
    /// mode) and the headless spawn paths ([`create_local_window`],
    /// [`SessionBackend::ensure_heartbeat`]) that drive tmux via one-shot commands and
    /// must not open a control-mode connection.
    pub(in crate::backend) fn ensure_session_configured(&self) -> Result<()> {
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
        !crate::agent::preflight::is_missing_dependency(error)
            && (!self.session_exists() || (M::RETRY_NO_SERVER_ERROR && typed_absence(error)))
    }

    fn ensure_session_configured_once(&self) -> Result<()> {
        // The common case — the session is there — asked and configured in one
        // process: `has-session` failing stops the list before any option is
        // set, and the path below then says why.
        if M::COMMAND_LISTS {
            let config = self.session_config();
            let list = config_command_list(&["has-session", "-t", &self.session], &config);
            if self.tmux_run(&list).is_ok() {
                return Ok(());
            }
        }
        if !self.session_exists() && !self.found_after_all()? {
            // No session to ask for `#{version}` yet, and creating one may start
            // a server — with an idle shell in it — that every spawn would then
            // refuse. The binary is what would start it, so it answers instead.
            M::check_banner(&self.tmux_output(&["-V"])?, &self.socket())?;
            debug!(
                "Creating tmux session '{}' on socket '{}'",
                self.session,
                self.socket()
            );
            let mut args = vec!["new-session", "-d", "-s", &self.session];
            args.extend_from_slice(M::BOOTSTRAP_SIZE_ARGS);
            if let Err(e) = self.run_tmux(&args) {
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
            // Cheap defensiveness: poll until the freshly-created session
            // answers the session probe before applying options. (The `no server
            // running on 'thurbox__thurbox'` failure that originally motivated
            // this on psmux was session *nesting*, now fixed at the root by
            // `strip_mux_nesting_env`; this poll is a harmless belt against any
            // genuinely-async `new-session -d`, and one session probe when the
            // first probe succeeds — which it does on the normal path, and only
            // when a session had to be created at all.)
            self.wait_for_session_ready(std::time::Duration::from_secs(5));
        }
        self.apply_session_config()
    }

    /// Poll until the freshly-created session answers its probe.
    /// Defensive belt against an async `new-session -d`; normally a no-op (the
    /// first probe succeeds). See
    /// [`ensure_session_configured`](Self::ensure_session_configured).
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

    /// The shell tmux should use for `default-command`. Local uses the user's
    /// `$SHELL`; a remote backend uses a POSIX shell guaranteed to exist on the
    /// remote host. Not used on a Windows machine (its multiplexer keeps its
    /// native default shell — see [`session_config`](Self::session_config)).
    ///
    /// The value must be a single, space-free token: it round-trips through the
    /// remote transport's per-argument shell-quoting (`ssh`/`wsl.exe`), where a
    /// space would be re-split by the remote shell into extra `set-option` args.
    /// The login-shell `PATH` fix for remote agents (e.g. `claude` under
    /// `~/.local/bin`) is applied at the *window command* instead — see
    /// [`build_shell_command`](Self::build_shell_command) /
    /// [`login_wrap_for_remote`](Self::login_wrap_for_remote).
    pub(in crate::backend) fn config_shell(&self) -> String {
        if self.transport.is_remote() {
            "/bin/sh".to_string()
        } else {
            std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string())
        }
    }

    /// The program a **local** window should launch: the agent's command,
    /// resolved against thurbox's own `PATH` — see [`resolve_local_program`].
    /// A remote/WSL backend passes through: its `PATH` is the *host's*, and its
    /// window command is login-wrapped instead
    /// ([`login_wrap_for_remote`](Self::login_wrap_for_remote)).
    pub(in crate::backend) fn program_for_window(&self, command: &str) -> String {
        if self.transport.is_remote() {
            return command.to_string();
        }
        resolve_local_program(command)
    }

    /// Build the shell command string to pass to tmux new-window.
    ///
    /// The whole string is interpreted by the multiplexer server's shell, so
    /// **every** token — the command itself as well as each argument — is
    /// shell-escaped. Leaving the command unescaped would break (or allow
    /// injection through) a command path containing a space or shell
    /// metacharacter; `shell_escape` is a no-op for ordinary binary names so the
    /// common case (`claude`, `/usr/bin/codex`) is unchanged.
    pub(in crate::backend) fn build_shell_command(command: &str, args: &[String]) -> String {
        let mut parts = vec![control_mode::shell_escape(command)];
        for arg in args {
            parts.push(control_mode::shell_escape(arg));
        }
        parts.join(" ")
    }

    /// Wrap a window command in a **login** shell for a remote/WSL backend so the
    /// user's profile `PATH` is present. Agents are commonly installed under
    /// `~/.local/bin` (e.g. `claude`), which the login profile adds to `PATH`; a
    /// non-login shell skips those files, so the agent binary isn't found, the
    /// window command exits 1, and the pane dies instantly — the remote session
    /// appears to "not launch". `exec` replaces the wrapper so no extra process
    /// lingers. A **Windows** host passes through, whatever multiplexer serves
    /// it: it has no `/bin/sh` to wrap with (psmux's adapter builds its windows'
    /// commands itself).
    ///
    /// Local backends pass through too, but **not** because they inherit the
    /// user's interactive `PATH` — that claim used to stand here and was wrong
    /// (see [`resolve_local_program`], which is what makes them safe now). They
    /// are not wrapped because thurbox can resolve a local command itself, and
    /// an absolute path needs no shell's `PATH` at all; a wrap would only add a
    /// second shell whose own quoting rules could differ.
    ///
    /// `/bin/sh -l` reads `~/.profile` but not the user's own shell's files
    /// (`~/.zshenv`, `~/.zprofile`), so the host's login `PATH` is assigned
    /// inside the wrap too ([`crate::agent::host_path`]) — the same `PATH` a
    /// delegated create gives the pane.
    ///
    /// Done here — not via tmux `default-command` — because that value round-trips
    /// through the remote transport's per-arg shell-quoting, where a `-l` flag's
    /// space would be re-split into a stray `set-option` argument.
    pub(in crate::backend) fn login_wrap_for_remote(&self, shell_cmd: &str) -> String {
        if self.transport.is_remote() && self.platform == crate::session::Platform::Posix {
            let path = self
                .host
                .as_ref()
                .and_then(crate::agent::host_path::assignment_for)
                .unwrap_or_default();
            let inner = control_mode::shell_escape(&format!("{path}exec {shell_cmd}"));
            format!("/bin/sh -lc {inner}")
        } else {
            shell_cmd.to_string()
        }
    }

    /// The window command for a **remote/WSL** companion shell pane: the user's
    /// own login shell, interactively — the same environment an `ssh <host>`
    /// login gives you, not a bare `/bin/sh`.
    ///
    /// [`default_shell`](Self::default_shell) returns `/bin/sh` for a remote
    /// Unix host (guaranteed to exist), and the generic
    /// [`login_wrap_for_remote`] would run it as `/bin/sh -lc 'exec /bin/sh'` —
    /// a login-sourced but then bare POSIX shell. That drops everything a real
    /// SSH login loads from the account's shell: its rc files (`~/.bashrc` /
    /// `~/.zshrc`), prompt, aliases, functions, and `PATH` additions. SSH runs
    /// the shell recorded in the user's passwd entry (which `$SHELL` reflects),
    /// so we do the same: bootstrap through the always-present `/bin/sh -l`
    /// (which login-sources the profile and thus exports `$SHELL`), then `exec`
    /// `"$SHELL"` as a **login** shell — tmux gives it a PTY, so it's
    /// interactive and sources the interactive rc chain too. If `$SHELL` is
    /// unset/broken the guard falls back to a plain `/bin/sh -l` so the pane
    /// still opens.
    ///
    /// The fallback is a `command -v` **guard**, never `exec "$SHELL" -l
    /// 2>/dev/null || …`: bash (and zsh) decide interactivity from
    /// `isatty(stdin) && isatty(stderr)`, and an `exec … 2>/dev/null`
    /// redirection **persists** into the exec'd shell — with stderr no longer a
    /// TTY the shell starts **non-interactive** (no prompt, no rc files, no
    /// readline), which reads as a blank "not loading" pane. So we probe
    /// `$SHELL` with `command -v` (whose own `2>/dev/null` is harmless) and only
    /// then `exec` it with all three std streams still on the PTY.
    ///
    /// Windows hosts keep [`default_shell`]'s `powershell` (no `/bin/sh`),
    /// whichever multiplexer serves them; local backends use the platform
    /// default directly.
    pub(in crate::backend) fn remote_shell_pane_command(&self) -> String {
        let inner = control_mode::shell_escape(
            "command -v \"$SHELL\" >/dev/null 2>&1 && exec \"$SHELL\" -l; exec /bin/sh -l",
        );
        format!("/bin/sh -lc {inner}")
    }

    /// The command a new window runs on a server that runs a window command
    /// through a POSIX shell, as the `new-window` line carries it: a POSIX
    /// host's login-shell bootstrap for a companion shell pane, or the program
    /// itself (login-wrapped on a POSIX host). What a tmux window runs; split
    /// out of [`SessionBackend::spawn`] so which of the two a window gets is
    /// testable without a server.
    pub(in crate::backend) fn posix_window_command(
        &self,
        window_name: &str,
        command: &str,
        args: &[String],
    ) -> String {
        // A remote/WSL companion shell pane (`tbs-` window) opens the user's own
        // interactive login shell — the SSH-login environment — instead of the
        // bare `/bin/sh` the generic login-wrap would produce (see
        // `remote_shell_pane_command`). Agent windows (`tb-`) keep the standard
        // path, and so does a Windows host on any multiplexer: it has no
        // `/bin/sh` to bootstrap from.
        let is_remote_shell_pane = self.transport.is_remote()
            && self.platform == Platform::Posix
            && window_name.starts_with(SHELL_WINDOW_PREFIX);
        if is_remote_shell_pane {
            return self.remote_shell_pane_command();
        }
        let program = self.program_for_window(command);
        let shell_cmd = Self::build_shell_command(&program, args);
        // A remote pane's `PATH` is the host's, restored by the login
        // wrap; a local one is inherited from this process, which need not
        // have the CLI its hooks call on it (see `path_prefix_args`).
        let shell_cmd = match self.transport.is_remote() {
            true => shell_cmd,
            // A shell reads this whole string, so the prefix has to be
            // UTF-8 here; a `PATH` that is not gets no prefix rather than a
            // mangled one (see `path_prefix_args`).
            false => shell_prefix_tokens()
                .map(|tokens| {
                    tokens
                        .into_iter()
                        .chain(std::iter::once(shell_cmd.clone()))
                        .collect::<Vec<_>>()
                        .join(" ")
                })
                .unwrap_or(shell_cmd),
        };
        self.login_wrap_for_remote(&shell_cmd)
    }

    /// The live connection, or an error when none has been started.
    ///
    /// A clone of the handle, so the caller's round trip runs with the lock
    /// already let go.
    fn connection(&self) -> Result<Arc<ControlMode>> {
        self.control
            .lock()
            .map_err(|e| anyhow::anyhow!("control lock: {e}"))?
            .clone()
            .ok_or_else(|| anyhow::anyhow!("Control mode not started — call ensure_ready() first"))
    }

    /// Run a closure with the active control mode, or bail if it has not been
    /// started yet.
    fn with_control<F, R>(&self, f: F) -> Result<R>
    where
        F: FnOnce(&ControlMode) -> Result<R>,
    {
        let ctrl = self.connection()?;
        f(&ctrl)
    }

    /// Start control mode unless it is running.
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
        debug!("Starting tmux control mode");
        let fresh = ControlMode::start(
            &self.located_transport(),
            &self.socket(),
            &self.session,
            &self.sizer,
            &M::control_policy(&self.transport, &self.session),
        )?;
        self.install(fresh)
    }

    /// Replace the connection `dead` with a fresh one; `None` replaces whatever
    /// is there.
    ///
    /// Callers that found the same connection dead queue on `connecting`, and
    /// all but the first find it already replaced and reuse the replacement
    /// instead of making a handshake each. The old connection keeps serving
    /// everybody else until the new one is in place.
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
        // A failed `start()` leaves the existing handle in place, so a retry
        // reconnects cleanly instead of hitting `control = None` and reporting
        // the misleading "call ensure_ready() first".
        let start = || {
            ControlMode::start(
                &self.located_transport(),
                &self.socket(),
                &self.session,
                &self.sizer,
                &M::control_policy(&self.transport, &self.session),
            )
        };
        // A connection that cannot be made again may be a server whose socket
        // went while the old connection held on: look for it once more.
        let fresh = match start() {
            Ok(fresh) => fresh,
            Err(_) if self.relocate(true) == Search::Found => start()?,
            Err(e) => return Err(e),
        };
        self.install(fresh)?;
        debug!("Control mode reconnected successfully");
        Ok(())
    }

    /// Make `fresh` the connection, unless [`SessionBackend::shutdown`] came
    /// first, and retire whatever it replaces.
    ///
    /// Retired rather than closed: other callers may be waiting on the old
    /// connection for answers to commands that have already run. Both it and
    /// a refused `fresh` are dealt with after the lock is let go, since a
    /// teardown waits on the child.
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

    /// [`Self::install`]'s step under the lock `shutdown` takes the connection
    /// out under: refused once shut down, so a handshake that outlives a quit
    /// is torn down instead of left behind. Answers the connection replaced.
    ///
    /// The replaced connection is recorded as retiring before `control` is let
    /// go, because `shutdown` reads `retiring` only after taking `control`:
    /// recorded later, a quit in between found nothing to close and the old
    /// connection's child outlived it.
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

    /// Send a command via control mode and return the response.
    ///
    /// A connection that fails is replaced, but the command is sent again only
    /// when tmux cannot have seen it — the write itself failed. One that was
    /// written and never answered may have run, and running a `new-window`
    /// twice opens two windows, so the caller hears the failure instead.
    fn ctrl_command(&self, cmd: &str) -> Result<String> {
        self.ctrl_command_list(&[cmd])
    }

    /// [`Self::ctrl_command`] for a command list, using the adapter's reply
    /// count (see `ControlMode::send_command_list`).
    fn ctrl_command_list(&self, cmds: &[&str]) -> Result<String> {
        let blocks = if M::COMMAND_LIST_SINGLE_REPLY {
            1
        } else {
            cmds.len()
        };
        let ctrl = self.connection()?;
        match ctrl.send_command_list(cmds, blocks) {
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

    /// Ask one question on a budget, for a caller that must not be made to
    /// wait: a link that has stopped carrying anything cannot overrun it.
    ///
    /// No reconnect on failure, unlike every other path here. A reconnect is a
    /// fresh ssh handshake plus the implicit attach response read back
    /// synchronously — precisely the unbounded wait this exists to avoid — and
    /// the callers that *can* wait will reconnect soon enough.
    fn ctrl_command_within(&self, cmd: &str, budget: std::time::Duration) -> Result<String> {
        self.with_control(|ctrl| ctrl.send_command_within(cmd, budget))
    }

    /// Send a command whose answer nobody reads, without waiting for it.
    ///
    /// Reconnect-free for [`Self::ctrl_command_within`]'s reasons.
    /// `blocks` is tmux's count; a one-block server needs one waiter slot.
    fn ctrl_command_detached(&self, cmds: &[&str], blocks: usize) -> Result<()> {
        let blocks = if M::COMMAND_LIST_SINGLE_REPLY {
            1
        } else {
            blocks
        };
        self.with_control(|ctrl| ctrl.send_command_detached(cmds, blocks))
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
            let (tx, reader) = ctrl.pane_channel(pane_id, M::SNAPSHOTS.then(snapshot_history));
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
            size.set_sized_elsewhere(sized_by.is_some_and(|name| name != self.sizer));
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
                "refresh-client -A '{}:on'",
                pane_id.replace('\'', "'\\''")
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

impl<M: TmuxCompatible> SessionBackend for Server<M> {
    /// A server that reports a deleted window in control mode
    /// (`%window-close`) ends the pane's stream with it; one that does not is
    /// polled. What the multiplexer can report decides it
    /// ([`TmuxCompatible::WINDOW_EVENTS`]) — not the machine's OS, nor
    /// thurbox's.
    fn needs_liveness_poll(&self) -> bool {
        !M::WINDOW_EVENTS
    }
    /// Only where the window can be told to keep its dead pane
    /// ([`TmuxCompatible::WINDOW_SETTINGS`], `remain-on-exit`).
    fn keeps_exited_windows(&self) -> bool {
        M::WINDOW_SETTINGS
    }
    fn name(&self) -> &str {
        &self.name
    }

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
                "tmux new-window returned an invalid pane id: {pane_id:?}"
            )
            .into());
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
            output: Box::new(Cursor::new(seed).chain(connected.output)),
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

    /// Only where a reply queues behind the pane output ahead of it, which is
    /// the whole of what makes a snapshot exact
    /// ([`TmuxCompatible::SNAPSHOTS`]).
    fn supports_snapshots(&self) -> bool {
        M::SNAPSHOTS
    }

    fn request_snapshot(&self, backend_id: &str) -> crate::backend::contract::BackendResult<()> {
        if !control_mode::is_valid_pane_id(backend_id) {
            return Err(
                anyhow::anyhow!("refusing to snapshot invalid pane id: {backend_id:?}").into(),
            );
        }
        if !self.supports_snapshots() {
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
            ctrl.request_snapshot(backend_id, snapshot_history())
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
        if !self.supports_snapshots() {
            return Err(anyhow::anyhow!("{} cannot snapshot a pane", self.transport.mux()).into());
        }
        // Asked, then waited for apart from the asking, so several searches'
        // round trips overlap: a search reads many panes at once.
        Ok(self
            .with_control(|ctrl| ctrl.ask_snapshot(backend_id, snapshot_history()))?
            .wait()?)
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
        let pane_id = PaneId::new(self.create_local_window(spec)?)?;
        self.confirm_spawned(spec, &pane_id)?;
        Ok(pane_id)
    }

    fn locate(&self, owner: Owner<'_>) -> crate::backend::contract::BackendResult<Placed> {
        self.known_socket()?;
        // A one-shot `list-windows`, and deliberately nothing more: starting
        // control mode would bring a server into being where there was none.
        // Answered, so an unreachable host is an `Err` rather than a listing
        // that reads the same as "no such window" — see `mux_answered_absent`.
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
        if !self.attached() {
            // One-shot, for a caller with no connection of its own — a
            // headless restore or `session register` claiming a window.
            for (option, value) in [
                (WINDOW_SESSION_OPTION, session_id),
                (WINDOW_ROLE_OPTION, role.as_str()),
            ] {
                if !value.is_empty() {
                    self.tmux_run(&[
                        "set-option",
                        "-w",
                        "-t",
                        &self.target(backend_id)?,
                        option,
                        value,
                    ])?;
                }
            }
        } else {
            let target = self.target(backend_id)?;
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
        // left to a resolver that is required to refuse the pair. The sweep is
        // a local one-shot, so a host's server is left to the teardown that
        // can reach it.
        if !self.transport.is_remote() {
            let _ = self.retire_duplicate_windows(session_id, role);
        }
        Ok(())
    }

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

    fn output_times(&self) -> Result<std::collections::HashMap<String, u64>> {
        if !self.stamps_are_per_window() {
            return Ok(std::collections::HashMap::new());
        }
        // A stale row's pane id may now belong to somebody else. Read only
        // windows whose own stamps identify the session, excluding its shell.
        let output = self.run_tmux(&[
            "list-windows",
            "-a",
            "-F",
            "#{@thurbox_session}\t#{@thurbox_role}\t#{window_activity}",
        ])?;
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

    fn resize(
        &self,
        backend_id: &str,
        rows: u16,
        cols: u16,
    ) -> crate::backend::contract::BackendResult<()> {
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
        //
        // Spelled without `admit_server`, whose question is a round trip the
        // paint must not wait on. The pane was attached through `target`,
        // which asked the server then; after a reconnect it goes unasked here
        // until the next `target` call, which re-attaching the panes makes.
        let (rows, cols) = tmux_size(rows, cols);
        let target = self.spell_target(backend_id);
        let window = format!("resize-window -t {target} -x {cols} -y {rows}");
        let pane = format!("resize-pane -t {target} -x {cols} -y {rows}");
        if !M::CONDITIONAL_RESIZE {
            // No `if-shell -F` to decide with: the last instance to paint wins.
            return Ok(self.ctrl_command_detached(&[&window, &pane], 2)?);
        }
        // A pane is the size of the rect ONE instance paints it into. Several
        // instances attached to one server each paint their own rect, and when
        // each resized to its own, whichever painted last — a toast taking a
        // row is enough — re-wrapped the agent for everybody. So the window
        // names its sizer (`SIZER_OPTION`), and a paint resizes only a window
        // that is this instance's to size: one nobody claims, one it already
        // sizes, or any window at all while it is the only client attached,
        // which is what makes a sizer that quit or crashed let go.
        // `claim_size` is how the name changes hands.
        //
        // Decided by tmux, in this same list, so a decision costs no round trip
        // and two instances cannot both win it. The shape is fixed on purpose:
        // the response queue expects a known number of `%begin` blocks per
        // list, and `if-shell` answers with one more block for each command it
        // runs — four taken and one declined, measured on tmux 3.7c. So the
        // name is settled first by a `set-option -F` that always answers once,
        // and each resize is its own `if-shell` whose else runs one command
        // too: five blocks, whichever way it goes. A pane that is gone fails
        // the first command and tmux drops the rest, which the queue expects
        // of any list; an inner command failing does NOT stop the list, which
        // is why the sizes are clamped to what tmux accepts (`tmux_size`).
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
    ) -> crate::backend::contract::BackendResult<()> {
        if !M::CONDITIONAL_RESIZE {
            return self.resize(backend_id, rows, cols);
        }
        // Unconditional: this is the instance being typed into, which is what
        // decides who sizes (see `resize`). Spelled without `admit_server`, as
        // `resize` is: this runs on the input loop.
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

    fn is_dead(&self, backend_id: &str) -> crate::backend::contract::BackendResult<bool> {
        // Bounded, because the only caller is the interface's own loop deciding
        // where a chord goes (`coordinator::input`'s passthrough gate). The
        // unbounded ask ran out `COMMAND_TIMEOUT`, reconnected and ran out
        // again on a link that had stopped carrying anything — twenty-odd
        // seconds of an interface that answered nothing, to settle a question
        // whose honest answer when the host says nothing is the one the caller
        // already reads an error as: not known to be dead. Spelled without
        // `admit_server` for the same reason, as `resize` is.
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
            return match self.transport.is_remote() {
                true => Ok(self.kill_window_oneshot(backend_id)?),
                false => self.kill_window_at(backend_id).map_err(Into::into),
            };
        }
        // Admitted before the reader is let go: a refused kill leaves the pane
        // running, and it must keep its reader.
        // The window, not only the pane — see `kill_window_oneshot`.
        let (verb, target) = self.kill_command(backend_id)?;
        let _ = self.unregister_pane(backend_id);
        match self.ctrl_command(&format!("{verb} -t {target}")) {
            Err(e) if !typed_absence(&e) => Err(e.into()),
            _ => Ok(()),
        }
    }

    fn detach(&self, backend_id: &str) -> crate::backend::contract::BackendResult<()> {
        // Disable output monitoring for this pane.
        if M::PANE_MONITORING {
            let off = format!(
                "refresh-client -A '{}:off'",
                backend_id.replace('\'', "'\\''")
            );
            if let Err(e) = self.ctrl_command_detached(&[&off], 1) {
                warn!("Failed to disable output monitoring during detach: {e}");
            }
        }
        // Remove the pane sender — the ControlModeReader gets EOF.
        let _ = self.unregister_pane(backend_id);
        Ok(())
    }

    fn pane_pid(&self, backend_id: &str) -> crate::backend::contract::BackendResult<Option<u32>> {
        let format =
            format!("#{{pane_pid}}{PANE_STATE_SEP}#{{window_name}}{PANE_STATE_SEP}#{{pane_id}}");
        let target = self.target(backend_id)?;
        let raw = if self.attached() {
            self.ctrl_command(&format!("display-message -t {target} -p '{format}'"))?
        } else {
            // A pane that is already gone has no pid, which is not an error.
            self.tmux_output(&["display-message", "-p", "-t", &target, &format])
                .unwrap_or_default()
        };
        if !answered_for(
            backend_id,
            pane_answer_field(&raw, 1).as_deref(),
            pane_answer_field(&raw, 2).as_deref(),
        ) {
            return Ok(None);
        }
        Ok(pane_answer_field(&raw, 0).and_then(|pid| pid.parse().ok()))
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
        let taken = self.control.lock().ok().and_then(|mut c| {
            // The admission was the connection's, and goes with it.
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

    fn take_hook_state_events(&self) -> Vec<(String, String)> {
        // `try_lock`, not `lock`: this runs on the UI thread every tick, and a
        // skipped drain only defers queued events to the next tick.
        let control = self.control.try_lock().ok().and_then(|guard| guard.clone());
        control
            .map(|ctrl| ctrl.take_sub_events())
            .unwrap_or_default()
    }

    fn hook_signal_command(&self) -> Option<String> {
        M::HOOK_STATUS.then(|| M::hook_signal_command(self))
    }

    fn record_hook_state(
        &self,
        pane: &str,
        state: &str,
    ) -> crate::backend::contract::BackendResult<()> {
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

    /// Read-only by design — no `ensure_ready`, so a poll never creates the
    /// server or the session. A server that says it has no such session (or
    /// that is not running) holds no states; a question that never reached
    /// one — an unreachable host, a missing binary — is an `Err`.
    fn hook_states(&self) -> crate::backend::contract::BackendResult<Vec<(String, String)>> {
        self.refuse_without_hook_status()?;
        match self.run_tmux(&["has-session", "-t", &self.session]) {
            Ok(_) => {}
            Err(e) if typed_absence(&e) => return Ok(Vec::new()),
            Err(e) => return Err(e.into()),
        }
        let format = format!(
            "#{{pane_id}} #{{{}}}",
            control_mode::REMOTE_HOOK_STATE_OPTION
        );
        let body = self.run_tmux(&["list-panes", "-s", "-t", &self.session, "-F", &format])?;
        Ok(control_mode::parse_pane_hook_states(
            String::from_utf8_lossy(&body.stdout).trim(),
        ))
    }

    /// A detached window, not `tb-` prefixed so [`Self::discover`] ignores
    /// it. The live window also keeps the server alive, so spawn-only
    /// automations work with no other sessions. Asked of the backend serving
    /// this machine's default multiplexer: `check_available` decides whether
    /// its binary may start a server at all (an old psmux may not).
    fn ensure_heartbeat(
        &self,
        program: &Path,
        args: &[String],
        every: std::time::Duration,
    ) -> crate::backend::contract::BackendResult<()> {
        // A probe nobody answered is not a reason to stop arming: readying the
        // session below either starts the server or fails with its own error.
        if self.heartbeat_running().unwrap_or(false) {
            return Ok(());
        }
        self.ensure_heartbeat_session()?;
        let loop_cmd = heartbeat_loop_command(self.platform, program, args, every)?;
        let out = self
            .mux_command(&[
                "new-window",
                "-d",
                "-t",
                &self.session,
                "-n",
                HEARTBEAT_WINDOW,
                &loop_cmd,
            ])?
            .output()
            .map_err(|e| {
                self.transport
                    .launch_failure("Failed to create automation heartbeat window", e)
            })?;
        // Asked again rather than believed: the status may be a failing user
        // hook's and not this window's — see the same read in
        // `create_local_window`. There is no `-P` answer to trust here, so the
        // listing is what says whether the window exists.
        if !out.status.success() && !self.heartbeat_running()? {
            return Err(anyhow::anyhow!(
                "{} new-window (heartbeat) {}",
                self.transport.mux(),
                mux_failure(&out, self.transport.is_ssh())
            )
            .into());
        }
        debug!("Armed automation heartbeat keeper window on {}", self.name);
        Ok(())
    }

    /// No server, or no session on it, is a heartbeat not running. Any other
    /// failed listing is an unanswered question (`listing_is_absence`).
    fn heartbeat_running(&self) -> crate::backend::contract::BackendResult<bool> {
        let out = self
            .mux_command(&["list-windows", "-t", &self.session, "-F", "#{window_name}"])?
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .map_err(|e| {
                self.transport
                    .launch_failure("Failed to run tmux command", e)
            })?;
        if !out.status.success() {
            if listing_output_is_absence(self.transport.is_ssh(), &out) {
                return Ok(false);
            }
            return Err(anyhow::anyhow!(
                "{} list-windows (heartbeat) {}",
                self.transport.mux(),
                mux_failure(&out, self.transport.is_ssh())
            )
            .into());
        }
        Ok(String::from_utf8_lossy(&out.stdout)
            .lines()
            .any(|w| w == HEARTBEAT_WINDOW))
    }

    /// Automations stop firing headlessly until something arms it again —
    /// which any `automation` write does, so this is a pause, not a removal.
    fn stop_heartbeat(&self) -> crate::backend::contract::BackendResult<bool> {
        if !self.heartbeat_running()? {
            return Ok(false);
        }
        let target = format!("{}:{HEARTBEAT_WINDOW}", self.session);
        match self.run_tmux(&["kill-window", "-t", &target]) {
            Ok(_) => Ok(true),
            Err(e) if typed_absence(&e) => Ok(false),
            Err(e) => Err(e.into()),
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
        use crate::session::Platform;
        match (self.transport.is_remote(), self.platform) {
            (false, Platform::Windows) => {
                std::env::var("COMSPEC").unwrap_or_else(|_| "cmd.exe".to_string())
            }
            (false, Platform::Posix) => {
                std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string())
            }
            (true, Platform::Windows) => "powershell".to_string(),
            (true, Platform::Posix) => "/bin/sh".to_string(),
        }
    }
}

/// Where a server was last found, and when it was last searched for in full
/// (`None`: only its socket file was seen, on first use).
#[derive(Clone)]
struct LocatedDir {
    dir: SocketDir,
    searched: Option<std::time::Instant>,
    /// What that search concluded, for a throttled question to repeat.
    outcome: Search,
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

/// How long a search for a server stands for the throttled questions — see
/// `Server::relocate`.
const RELOCATE_EVERY: std::time::Duration = std::time::Duration::from_secs(30);

/// The socket directory each server was last found in, by
/// `Server::located_key` — see `Server::socket_dir`.
fn located_dirs() -> &'static Mutex<HashMap<String, LocatedDir>> {
    static LOCATED: std::sync::OnceLock<Mutex<HashMap<String, LocatedDir>>> =
        std::sync::OnceLock::new();
    LOCATED.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Whether a socket file for `socket` is in `dir` on this machine. A file,
/// not a server: tmux never unlinks one, so it can outlive its server.
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

/// The body of [`locate_script`], after the candidate directories `d0…` and
/// their count `n`. `$1` is the multiplexer, `$2` the socket name.
///
/// It answers with one line, or none when no server was found:
///
/// - `answered <i>` — a server answers in candidate `i`;
/// - `recovered <i> <pid> <others> <path>` — none answered, but `pid` was
///   bound to candidate `i`'s missing socket path, was sent `SIGUSR1`, and now
///   answers (`others` more servers were bound there too);
/// - `unanswered <i> <pid> <others> <path>` — the same, and it has not answered;
/// - `bound <pids> <path>` — a socket file is at `path` and does not answer,
///   while `pids` are bound to it: nothing is signalled. It is given the
///   same three seconds as `SIGUSR1` first, because a peer's `new-session`
///   binds the socket between the first `answers` and `bound` — every
///   relaunch after a reboot searches at once — and a server still starting
///   then reads as one out of reach.
///
/// The bound-socket question is asked of the kernel, because a server's
/// command line need not name its socket: `/proc/net/unix` lists each
/// listening socket by its bound path, the unlinked included, and
/// `/proc/<pid>/fd` says whose it is — one `ls` for every process this user
/// can read. Elsewhere (macOS) `lsof` reports the same bound path. A path is
/// spelled as tmux binds it: the directory resolved, `tmux-<uid>` beneath —
/// and read whole, spaces and all: it is everything after `/proc/net/unix`'s
/// seven fixed columns, and reaches `awk` through the environment, where
/// `-v` would have interpreted its backslashes.
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

/// The script `Server::find_server` runs where the multiplexer does: `candidates`
/// as `d0…`, then [`LOCATE_BODY`].
fn locate_script(candidates: &[SocketDir], socket: &str) -> String {
    let mut script = String::new();
    for (i, dir) in candidates.iter().enumerate() {
        script.push_str(&format!("d{i}={}\n", dir.shell_expr(socket)));
    }
    script.push_str(&format!("n={}\n", candidates.len()));
    script.push_str(LOCATE_BODY);
    script
}

/// What [`LOCATE_BODY`] said.
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

/// Read [`LOCATE_BODY`]'s answer; anything else is [`ServerFound::Nothing`].
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

/// Window name for the headless automation heartbeat keeper. Deliberately NOT
/// `tb-` prefixed so [`Server::discover`](SessionBackend::discover) ignores it — it is
/// infrastructure, not a session.
const HEARTBEAT_WINDOW: &str = "automation-heartbeat";

/// The keeper's loop, as the window command: `program args…` every `every`,
/// in the shell of the machine the server runs on (`platform`) — a POSIX one,
/// or on Windows a PowerShell script handed to `powershell.exe` explicitly
/// ([`crate::shell::powershell_launcher`]), since psmux runs a window command
/// in whichever shell it resolves, `cmd.exe` on a server started over ssh.
/// Handed over as **one argv token** either way, which on psmux also dodges
/// its trailing-token handling.
fn heartbeat_loop_command(
    platform: Platform,
    program: &Path,
    args: &[String],
    every: std::time::Duration,
) -> Result<String> {
    let secs = every.as_secs().max(1);
    let path = program.display().to_string();
    match platform {
        Platform::Posix => {
            let argv: Vec<String> = std::iter::once(path)
                .chain(args.iter().cloned())
                .map(|a| shell_escape(&a))
                .collect();
            Ok(format!(
                "while true; do {} >/dev/null 2>&1; sleep {secs}; done",
                argv.join(" ")
            ))
        }
        Platform::Windows => {
            let argv: Vec<String> = std::iter::once(path)
                .chain(args.iter().cloned())
                .map(|a| crate::shell::powershell_quote(&a))
                .collect();
            crate::shell::powershell_launcher(&format!(
                "while ($true) {{ & {} *> $null; Start-Sleep {secs} }}",
                argv.join(" ")
            ))
            .context("the automation heartbeat")
        }
    }
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
/// [`Server::apply_session_config`].
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
const SESSION_OPTS: &[(&str, &str)] = &[("status", "off"), ("history-limit", "5000")];

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
const WINDOW_OPTS: &[(&str, &str)] = &[
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
///   attached, so [`Server::spawn`](SessionBackend::spawn) — a restart, a plugin program, the
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
    if Platform::local() == Platform::Windows {
        return command.to_string();
    }
    match crate::paths::resolve_on_path(command) {
        Some(path) => path.to_string_lossy().into_owned(),
        None => command.to_string(),
    }
}

impl<M: TmuxCompatible> Server<M> {
    /// [`SessionBackend::create_window`] on this machine's server: a one-shot
    /// `new-window`, no control mode, so nothing attaches to what it opens.
    ///
    /// Returns the new pane's id (`%N`). The window is named after its owner —
    /// which is *not* unique (two sessions can share a name) — so it is stamped
    /// with the owner's id before this returns and every later lookup resolves
    /// that (ADR-25).
    ///
    /// A pane id on stdout outranks a non-zero exit status, which on this path
    /// can belong to a user's tmux hook rather than to the window — see the
    /// read below.
    ///
    /// A server without [`TmuxCompatible::ONE_SHOT_SPAWN_ANSWERS`] is not asked
    /// for the id: it is the one pane under the window's name that was not
    /// there before `new-window` ([`Self::new_named_pane`]). With no id to
    /// weigh, a non-zero status is the whole answer there.
    fn create_local_window(&self, spec: &WindowSpec<'_>) -> Result<String> {
        let session_id = spec.owner.session_id;
        // Ensure the session exists and is configured, without opening a
        // control-mode connection (headless one-shot path).
        self.ensure_session_configured()?;
        // The headless twin of the check in `spawn`.
        self.admit_server()?;

        let window_name = window_name_for(spec.role, spec.owner.name);
        // Created at the END of the session's window list, so the retention
        // below can name the window this command just made: `{end}` is the last
        // window and `-a` appends after it, so within this one command list
        // `{end}` is exactly the new one. The window's *name* cannot say that —
        // `tb-<session name>` is not unique (two sessions can share a name,
        // which is why the stamp exists), and tmux resolves a duplicate name to
        // the lowest index, which is the older window (measured, tmux 3.2a). A
        // server without the shorthand keeps the plain session target: it gets
        // no retention write either.
        let create_target = if M::ONE_SHOT_SPAWN_ANSWERS {
            format!("{}:{{end}}", self.session)
        } else {
            format!("{}:", self.session)
        };
        // The stamp rides in the same command list as the creation, like the
        // birth options: two `set-option` processes fewer on every `session
        // create` (#1243). `{end}` still names the new window here, and the pane
        // id it would otherwise be written against is not known until the list
        // returns.
        let stamped = M::WINDOW_OPTIONS;
        let before = if M::ONE_SHOT_SPAWN_ANSWERS {
            Vec::new()
        } else {
            self.window_panes(&window_name).with_context(|| {
                format!("list the windows named {window_name} before adding one")
            })?
        };
        let run_new_window = || {
            let mut tmux = self.new_window_command(
                &window_name,
                &create_target,
                spec.command,
                spec.args,
                spec.cwd,
                spec.env,
            )?;
            if stamped {
                for (option, value) in [
                    (WINDOW_SESSION_OPTION, session_id),
                    (WINDOW_ROLE_OPTION, spec.role.as_str()),
                ] {
                    if !value.is_empty() {
                        tmux.args([";", "set-option", "-w", "-t", &create_target, option, value]);
                    }
                }
            }
            tmux.output().map_err(|e| {
                self.transport
                    .launch_failure("Failed to run tmux new-window for headless spawn", e)
            })
        };
        let mut output = run_new_window()?;
        let deadline = std::time::Instant::now() + M::COLD_START_GRACE;
        loop {
            let stderr = String::from_utf8_lossy(&output.stderr);
            if output.status.success()
                || !M::RETRY_NO_SERVER_ERROR
                || !mux_answered_absent(&stderr)
                || std::time::Instant::now() >= deadline
            {
                break;
            }
            // A "no server running" reply did not deliver new-window. A
            // different error may mean it did create it.
            std::thread::sleep(std::time::Duration::from_millis(250));
            output = run_new_window()?;
        }
        let pane_id = if M::ONE_SHOT_SPAWN_ANSWERS {
            new_window_pane_id(&output.stdout)
        } else {
            String::new()
        };
        if !output.status.success() {
            // The exit status is not this command's verdict on its own. tmux
            // hands a command-mode client the status of the last `run-shell` its
            // command list triggered, and a *hook* counts: an `after-new-window`
            // left behind by an uninstalled tmux plugin runs a script that is no
            // longer there, `/bin/sh` answers 127, and the client exits 127
            // although `new-window` succeeded and already printed the pane id
            // (measured, tmux 3.5a; stderr is empty, so the message named
            // nothing either). The pane id is the answer to `-P`, so where there
            // is one the window exists and refusing it tears down a session that
            // started fine (issue #1154). The control-mode path never sees this:
            // its reply block carries the `-P` answer alone and no exit status at
            // all.
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
                self.transport.mux(),
                self.socket()
            );
        }
        let pane_id = if M::ONE_SHOT_SPAWN_ANSWERS {
            pane_id
        } else {
            self.new_named_pane(spec, &window_name, &before)?
        };
        // What stamping does after writing a stamp. A server without window
        // options writes none (ADR-13).
        if stamped {
            let _ = self.retire_duplicate_windows(session_id, spec.role);
        }
        Ok(pane_id)
    }

    /// The pane `new-window` just made on a server that does not answer `-P`:
    /// the one under `window_name` that `before` did not list. psmux does not
    /// reuse pane ids within a server's life, so a new id is the new window,
    /// and a namesake — allowed, and unstamped there (ADR-13) — is never taken
    /// for it. No new id means the window already closed with its program.
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

    /// The `new-window` command list [`Self::create_local_window`] runs: the
    /// window created detached at `create_target` running `command` — and,
    /// where the server takes them, its birth options chained into the same
    /// invocation.
    fn new_window_command(
        &self,
        window_name: &str,
        create_target: &str,
        command: &str,
        args: &[String],
        cwd: Option<&Path>,
        env: &HashMap<String, String>,
    ) -> Result<Command> {
        let mut tmux = self.mux_command(&["new-window", "-d"])?;
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

        // Chained into the same command list as the creation, not sent after
        // it — `birth_options` has the measurement. This path passes `-d`, so
        // the new window is not current and the bare form the control-mode path
        // uses is not available; `{end}` names it instead, which is why the
        // window is created there.
        if M::WINDOW_SETTINGS && M::ONE_SHOT_SPAWN_ANSWERS {
            for (key, value) in birth_options(window_name) {
                tmux.args([";", "set-window-option", "-t", create_target]);
                tmux.args([key, value]);
            }
        }
        Ok(tmux)
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
    /// This machine's server only, and only one with window options: a server
    /// that keeps `@` options in one server-global map (psmux, ADR-13) hands the
    /// same value back for every window, so every window there would read as
    /// stamped for whoever was stamped last; the stamp is withheld at both ends for
    /// that reason (issue #1168) and this must not be the one place that believes
    /// it.
    fn retire_duplicate_windows(&self, session_id: &str, role: WindowRole) -> Option<String> {
        if !M::WINDOW_OPTIONS || session_id.is_empty() {
            return None;
        }
        let output = self
            .mux_command(&["list-windows", "-t", &self.session, "-F", RETIRE_FORMAT])
            .ok()?
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

    /// Kill the window `target` is in on this machine's server, with a
    /// one-shot command. One already gone is not an error.
    fn kill_window_at(&self, target: &str) -> Result<()> {
        let (verb, target) = self.kill_command(target)?;
        let output = self
            .mux_command(&[verb, "-t", &target])?
            .output()
            .context("Failed to run tmux kill-window")?;
        if !output.status.success()
            || (M::EXIT_ZERO_ERRORS && mux_reported_error(&output, self.transport.is_ssh(), false))
        {
            return match self.command_failure(&[verb, "-t", &target], &output) {
                crate::backend::contract::BackendError::Absent(_) => Ok(()),
                error => Err(error.into()),
            };
        }

        Ok(())
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
fn is_version_answer(answer: &str) -> bool {
    let parts: Vec<&str> = answer.split('.').collect();
    parts.len() >= 2
        && parts
            .iter()
            .all(|part| part.bytes().all(|b| b.is_ascii_digit()) && part.parse::<u32>().is_ok())
}

/// The version a `-V` banner names, held to [`is_version_answer`]: the word
/// after `psmux ` or `tmux ` on the last line that names one (psmux 3.3.7
/// prints both, 3.3.6 only the second). `None` when no line does.
fn banner_version(banner: &str) -> Option<&str> {
    let rest = banner.lines().rev().find_map(|line| {
        let line = line.trim();
        line.strip_prefix("psmux ")
            .or_else(|| line.strip_prefix("tmux "))
    })?;
    // That line or nothing: an earlier line is an older name for the same
    // binary, and reading past a suffixed one (`3.3.8-dev`) to it would judge
    // a version the binary does not claim to be.
    rest.split_whitespace()
        .next()
        .filter(|v| is_version_answer(v))
}

/// What [`Server::admit_server`]'s question found.
enum Admission {
    /// The multiplexer said there is no server or no session.
    NothingThere,
    Admitted,
}

fn typed_absence(error: &anyhow::Error) -> bool {
    matches!(
        error.downcast_ref::<crate::backend::contract::BackendError>(),
        Some(crate::backend::contract::BackendError::Absent(_))
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::identity::agent_window_name;
    use crate::backend::instance::{
        host_socket, known_host_socket, learn_host_socket, TMUX_SOCKET,
    };
    use crate::backend::tmux_compat::control_mode::PANE_CHANNEL_CAPACITY;

    /// A multiplexer for the shared code's own tests, which may name no
    /// adapter: tmux's name, so the routes read as a row's would, and POSIX
    /// window commands. What it answers beyond those is no adapter's claim.
    struct TestMux;

    type TestBackend = Server<TestMux>;

    #[cfg(unix)]
    #[test]
    fn an_unfinished_or_mixed_process_reply_is_not_typed_absence() {
        use std::os::unix::process::ExitStatusExt;
        let backend = TestBackend::new();
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
                    crate::backend::BackendError::Absent(_)
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

    impl TmuxCompatible for TestMux {
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
            _: &HashMap<String, String>,
        ) -> Result<String> {
            Ok(server.posix_window_command(window_name, command, args))
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

    /// A multiplexer with a version floor: below `2.0` is refused.
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

    /// A stand-in multiplexer with a floor, logging every command it is given
    /// beside itself; `answer` is printed as its version, one-shot and over
    /// control mode alike. Answers the backend, and how often the version was
    /// asked so far.
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
        );
        (backend, asked)
    }

    /// An admission is good for the connection it was asked on: a replacement
    /// may be to a server started since, on other code, so it is asked again —
    /// once, however many panes are then named.
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

    #[test]
    fn a_banner_names_the_version_on_its_last_line_that_has_one() {
        assert_eq!(
            banner_version("tmux 3.3.7\npsmux 3.3.7 (05cc5d4 2026-07-20)\n"),
            Some("3.3.7")
        );
        assert_eq!(banner_version("tmux 3.3.6\n"), Some("3.3.6"));
        assert_eq!(banner_version("psmux 3.4.0 (20aa1d7)"), Some("3.4.0"));
        for banner in [
            "",
            "3.3.8",
            "psmux",
            "psmux dev",
            "ERROR: x",
            // The last line names a pre-release, and an earlier one is not
            // read in its place.
            "tmux 3.3.7\npsmux 3.3.8-dev (x)\n",
            "tmux 3.3.9\npsmux 3.3.9-dev (x)\n",
        ] {
            assert_eq!(banner_version(banner), None, "{banner:?}");
        }
    }

    /// The admission was the connection's: once it is shut down, the next
    /// name asks again rather than ride on it.
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

    /// Over control mode an answer that is not a version may be a reply that
    /// belonged to another command, so it admits nothing.
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
        use std::io::Read;
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

        let pane = backend
            .connect_pane("%1", Some("@1"), 24, 80)
            .expect("attach without monitoring command");
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        let reader = std::thread::spawn(move || {
            let mut output = [0; 8];
            let mut pane_output = pane.output;
            let _ = tx.send(pane_output.read_exact(&mut output).map(|_| output));
        });
        let output = rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("streamed output arrived")
            .expect("streamed output read");
        reader.join().expect("reader thread");
        assert_eq!(&output, b"streamed");
        backend.detach("%1").expect("detach");
        drop(backend);
        let commands = std::fs::read_to_string(mux.with_extension("log")).expect("command log");
        assert!(commands.contains("resize-window"), "{commands}");
        assert!(!commands.contains("refresh-client -A"), "{commands}");
    }

    /// Exercise overflow through the subprocess, control reader and bounded
    /// pane channel, up to the reader a pane's grid is fed from. Not reading
    /// makes the overflow deterministic without depending on how quickly a
    /// real tmux batches bytes. Installing what comes out onto a grid is
    /// `pane`'s, tested there: this module may not name it.
    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_overflowed_pane_is_caught_up_without_blocking_control() {
        use std::io::Read;
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
        ));
        // Shut down even on assertion failure, so the runtime's blocking reader
        // cannot outlive the subprocess that owns its channel.
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
        let (mut reader, _) = backend
            .register_pane("%1", Some("@1"))
            .expect("resident pane");

        control
            .send_command("display-message -p flood")
            .expect("control replies still arrive while the pane channel is full");
        // A barrier after the flood proves that all notifications reached the
        // dispatcher before the consumer is released. No further output is
        // needed to trigger recovery, even if the snapshot itself found no room.
        control
            .send_command("display-message -p barrier")
            .expect("another command is not held behind the flooded pane");

        // Read on a thread of its own, so a reader that is never handed the
        // resync fails the test at its deadline instead of hanging it.
        let (items, received) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut buf = [0u8; 256];
            loop {
                let item = match reader.read(&mut buf) {
                    Ok(0) => return,
                    Ok(n) => Ok(buf[..n].to_vec()),
                    Err(e) => match crate::backend::contract::SnapshotArrived::take(e) {
                        Some(arrived) => Err(arrived),
                        None => continue,
                    },
                };
                if items.send(item).is_err() {
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
        let arrived = loop {
            match next() {
                Ok(bytes) => {
                    assert_eq!(bytes, b"\x1b[", "only output from before the gap");
                    queued += 1;
                }
                Err(arrived) => break arrived,
            }
        };
        assert!(queued <= PANE_CHANNEL_CAPACITY, "{queued}");
        assert!(arrived.resync, "a resync, which replaces a grid it has");
        assert_eq!(
            *arrived.snapshot,
            crate::backend::contract::PaneSnapshot {
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
        assert_eq!(next().ok(), Some(b"!".to_vec()), "output flows again");
    }

    /// A control server standing in for a host whose link has gone slow: every
    /// connection it starts logs `start`, waits out [`STALL`] first when a
    /// `.slow` file sits beside it, and answers `list-windows` only after
    /// [`STALL`] too. Everything else is answered at once.
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
        let backend =
            Server::<UnmonitoredMux>::with_transport(transport, "unused", "unused", "local:tmux");
        backend.ensure_control().expect("control client");
        (backend, mux)
    }

    #[cfg(unix)]
    const STALL: std::time::Duration = std::time::Duration::from_secs(3);

    /// Block until the fake mux's log satisfies `seen`.
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

    /// #1472: the paint path's resize waited for the control lock while
    /// another caller's round trip held it, and was refused as busy once
    /// [`LOOP_COMMAND_BUDGET`] ran out — every frame, for every pane of a host
    /// whose link had gone slow. Commands are already serialised on the
    /// connection itself, so a stalled answer must cost nobody else a wait.
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

    /// #1472: a reconnect held the control lock through the new connection's
    /// start — an ssh handshake on a remote host — so the paint path stalled
    /// for as long as the handshake took.
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

    /// Another caller's reconnect must not take the connection out from under a
    /// command already sent on it: closing it lost the answer, and the caller
    /// resent the command on the new connection — a `new-window` that tmux had
    /// already run opened a second window.
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

    /// A command written whose answer never came may have run, so it is not
    /// sent again — the caller hears the failure instead. Only a write that
    /// failed outright, which tmux never saw, is retried.
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

    /// A quit that lands while a reconnect is replacing the connection must
    /// still close the one replaced. It was recorded as retiring only after
    /// the swap let go of `control`, so a `shutdown` in that gap took the new
    /// connection, found nothing retiring, and the old one's child outlived
    /// the quit. With the swap parked at its record step (by holding
    /// `retiring`), the new connection must not be visible yet.
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

    /// The whole point of the fix: what tmux is handed must not need tmux's own
    /// `PATH` (nor the `PATH` of the shell tmux runs a single-token command
    /// with) to be found.
    #[test]
    #[cfg(unix)]
    fn a_local_window_command_is_an_absolute_path() {
        let dir = tempfile::TempDir::new().unwrap();
        let expected = agent_only_thurbox_can_see(dir.path(), "tbx-spawn-probe");

        // Through the shared helper: `PATH` is process state, and the unit
        // tests that set it run concurrently under plain `cargo test`.
        let (local, free, remote) = crate::paths::with_path(dir.path(), || {
            (
                TestBackend::local().program_for_window("tbx-spawn-probe"),
                resolve_local_program("tbx-spawn-probe"),
                // A remote host's PATH is the host's, so its command is the
                // host's to resolve — and it is login-wrapped instead.
                TestBackend::for_host(&crate::session::HostDef {
                    name: "devbox".into(),
                    destination: "me@devbox".into(),
                    ..Default::default()
                })
                .program_for_window("tbx-spawn-probe"),
            )
        });

        assert_eq!(local, expected.to_string_lossy());
        assert_eq!(free, expected.to_string_lossy());
        assert_eq!(remote, "tbx-spawn-probe");
    }

    /// Where a server is looked for: the directory a new one would use
    /// first, then where the host's CLI or an earlier build put one.
    #[test]
    fn a_hosts_server_is_looked_for_where_its_own_thurbox_said_first() {
        let host_data = SocketDir::HostData {
            app: crate::paths::app_dir_name().into(),
        };
        let backend = |name: &str| {
            let host = crate::session::HostDef {
                name: name.into(),
                destination: "me@devbox".into(),
                ..Default::default()
            };
            let mut backend = TestBackend::for_host(&host);
            backend.transport = backend.transport.clone().with_socket_dir(host_data.clone());
            (host, backend)
        };
        let (_, unasked) = backend("socket-dir-unasked");
        assert_eq!(
            unasked.socket_dir_candidates(),
            [host_data.clone(), SocketDir::Inherited]
        );
        let (older, older_backend) = backend("socket-dir-older-cli");
        crate::backend::instance::learn_host_socket_dir(&older, None);
        assert_eq!(
            older_backend.socket_dir_candidates(),
            [SocketDir::Inherited, host_data.clone()],
            "a CLI that reported no directory spawns into tmux's default"
        );
        let (newer, newer_backend) = backend("socket-dir-newer-cli");
        crate::backend::instance::learn_host_socket_dir(&newer, Some("/srv/me/thurbox/tmux"));
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

    /// Runs [`LOCATE_BODY`] against one candidate directory holding a
    /// listening socket bound by this process, with a stand-in multiplexer
    /// whose `list-sessions` succeeds once `ready` exists.
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

    /// The race behind a cold start's `not starting a second server beside
    /// it`: every relaunch after a reboot looks for the server at once, and a
    /// peer's `new-session` binds the socket between this search asking it and
    /// looking for who is bound to it. A server still starting is not one out
    /// of reach.
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
        // The command token is interpreted by the server's shell, so a path
        // with a space (or any metacharacter) must be quoted, not left bare —
        // otherwise the shell would split it and the launch would break.
        let cmd = TestBackend::build_shell_command("/opt/My Agents/codex", &["--foo".to_string()]);
        assert_eq!(cmd, "'/opt/My Agents/codex' --foo");
    }

    #[test]
    fn backend_default_has_no_control_mode() {
        let backend = TestBackend::new();
        let guard = backend.control.lock().unwrap();
        assert!(guard.is_none());
    }

    #[test]
    fn for_host_builds_named_ssh_backend() {
        let host = crate::session::HostDef {
            name: "devbox".into(),
            destination: "me@devbox".into(),
            ssh_opts: vec!["-o".into(), "ControlMaster=auto".into()],
            ..Default::default()
        };
        let backend = TestBackend::for_host(&host);
        assert_eq!(backend.name(), "ssh:devbox:tmux");
        assert!(backend.transport.is_remote());
        // Falls back to the default socket/session when the host omits them.
        assert_eq!(backend.socket, TMUX_SOCKET);
        assert_eq!(backend.session, TMUX_SESSION);
    }

    /// A socket a host's thurbox reported is that instance's address, so every
    /// multiplexer on the host reaches it — one learned while driving tmux is
    /// the one its psmux backend uses too.
    #[test]
    fn a_learned_socket_is_the_hosts_whichever_multiplexer_learned_it() {
        let host = crate::session::HostDef {
            name: "learned-socket-host".into(),
            destination: "me@learned".into(),
            ..Default::default()
        };
        learn_host_socket(&host, "thurbox-elsewhere");
        assert_eq!(TestBackend::for_host(&host).socket(), "thurbox-elsewhere");
        let mut served = host.clone();
        served.multiplexer = Some("psmux".into());
        assert_eq!(host_socket(&served), "thurbox-elsewhere");
        assert_eq!(known_host_socket(&served).unwrap(), "thurbox-elsewhere");
    }

    #[test]
    fn for_host_builds_named_wsl_backend() {
        let host = crate::session::HostDef::wsl("Ubuntu");
        let backend = TestBackend::for_host(&host);
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
        // The local $SHELL (e.g. /bin/zsh) may not exist on the host: a remote
        // Windows pane got "CommandNotFoundException", a zsh-less Linux host a
        // dead pane. Remote backends pick by transport.
        let winbox = TestBackend::for_host(&crate::session::HostDef {
            name: "winbox".into(),
            destination: "me@winbox".into(),
            multiplexer: Some("psmux".into()),
            ..Default::default()
        });
        assert_eq!(winbox.default_shell(), "powershell");

        let devbox = TestBackend::for_host(&crate::session::HostDef {
            name: "devbox".into(),
            destination: "me@devbox".into(),
            ..Default::default()
        });
        assert_eq!(devbox.default_shell(), "/bin/sh");

        let wsl = TestBackend::for_host(&crate::session::HostDef::wsl("Ubuntu"));
        assert_eq!(wsl.default_shell(), "/bin/sh");

        // Local keeps the platform default ($SHELL / %COMSPEC%).
        let local = TestBackend::local();
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

    /// The `default-command` a backend pins on its server, if any.
    fn pinned_default_command(backend: &TestBackend) -> Option<String> {
        backend.session_config().into_iter().find_map(|option| {
            let at = option.args.iter().position(|a| a == "default-command")?;
            option.args.get(at + 1).cloned()
        })
    }

    fn windows_host(mux: &str) -> crate::session::HostDef {
        crate::session::HostDef {
            name: "winbox".into(),
            destination: "me@winbox".into(),
            multiplexer: Some(mux.into()),
            platform: Some(crate::session::Platform::Windows),
            ..Default::default()
        }
    }

    /// A WSL distro is Linux whatever machine drives it, so its server gets
    /// `/bin/sh` as `default-command` from a Windows thurbox as from a Linux
    /// one. Simulated: the build OS is replaced by `Platform::local`'s test
    /// override, which is the only way a Linux run reaches the Windows branch.
    #[test]
    fn a_windows_thurbox_pins_posix_default_command_on_a_wsl_host() {
        use crate::session::{platform::simulate_local, HostDef, Platform};
        for local in Platform::ALL {
            let pinned = simulate_local(local, || {
                pinned_default_command(&TestBackend::for_host(&HostDef::wsl("Ubuntu")))
            });
            assert_eq!(pinned.as_deref(), Some("/bin/sh"), "thurbox on {local:?}");
        }
    }

    /// A Windows host is Windows whichever multiplexer serves it: PowerShell
    /// for its shell panes, its own native `default-command`, no `/bin/sh -lc`
    /// wrap — the platform is the host's, never inferred from `psmux`.
    #[test]
    fn a_windows_host_on_a_non_psmux_multiplexer_keeps_native_shell_semantics() {
        use crate::session::{platform::simulate_local, Platform};
        for local in Platform::ALL {
            simulate_local(local, || {
                let backend = TestBackend::for_host(&windows_host("tmux"));
                assert_eq!(backend.transport.mux(), "tmux");
                assert_eq!(backend.default_shell(), "powershell", "from {local:?}");
                assert_eq!(pinned_default_command(&backend), None, "from {local:?}");
                assert_eq!(backend.login_wrap_for_remote("agent"), "agent");
            });
        }
    }

    /// A companion shell pane on a Windows host opens PowerShell whichever
    /// multiplexer serves it: the POSIX login-shell bootstrap is `/bin/sh`,
    /// which such a host does not have. A POSIX host keeps the bootstrap.
    #[test]
    fn a_windows_hosts_shell_pane_is_not_bootstrapped_through_sh() {
        use crate::session::HostDef;
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
        let windows = TestBackend::for_host(&windows_host("tmux"));
        assert_eq!(shell_pane(&windows), "powershell");
        let posix = TestBackend::for_host(&HostDef {
            name: "devbox".into(),
            destination: "me@devbox".into(),
            ..Default::default()
        });
        assert_eq!(shell_pane(&posix), posix.remote_shell_pane_command());
    }

    /// The backend a route builds keeps the host's platform, the host's
    /// launcher and the route's multiplexer, each unaffected by the others and
    /// by the OS thurbox is built for.
    #[test]
    fn a_backend_keeps_platform_launcher_and_multiplexer_apart() {
        use crate::session::{platform::simulate_local, HostDef, HostKind, Multiplexer, Platform};
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
                        let backend = simulate_local(local, || TestBackend::for_host(&host));
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
                            host.route(Some(Multiplexer::Tmux)).format(),
                            "{case}"
                        );
                        assert!(!backend.needs_liveness_poll(), "{case}");
                    }
                }
            }
        }
    }

    /// A legacy entry is Windows because it names psmux; a backend built to
    /// serve one of its rows on another multiplexer is still on Windows.
    #[test]
    fn a_route_on_another_multiplexer_keeps_the_hosts_platform() {
        let legacy = crate::session::HostDef {
            name: "win".into(),
            destination: "me@win".into(),
            multiplexer: Some("psmux".into()),
            ..Default::default()
        };
        let backend = TestBackend::for_host(&legacy);
        assert_eq!(backend.transport.mux(), "tmux");
        assert_eq!(backend.platform, crate::session::Platform::Windows);
        assert_eq!(backend.default_shell(), "powershell");
    }

    #[test]
    fn remote_shell_pane_opens_users_login_shell() {
        // The companion shell pane on a remote/WSL host should give the user
        // their own interactive login shell (the SSH-login environment: rc
        // files, prompt, aliases, PATH) — not the bare `/bin/sh` the generic
        // login-wrap would produce. Bootstrap through the always-present
        // `/bin/sh -l` (exports `$SHELL`), then `exec "$SHELL" -l`.
        //
        // Crucially the `$SHELL` probe is a `command -v` guard, NOT
        // `exec "$SHELL" -l 2>/dev/null`: an `exec … 2>/dev/null` redirection
        // persists into the exec'd shell, drops stderr off the TTY, and bash/zsh
        // then start non-interactive (no prompt) — a blank pane.
        const EXPECT: &str =
            "/bin/sh -lc 'command -v \"$SHELL\" >/dev/null 2>&1 && exec \"$SHELL\" -l; exec /bin/sh -l'";
        let ssh = TestBackend::for_host(&crate::session::HostDef {
            name: "devbox".into(),
            destination: "me@devbox".into(),
            ..Default::default()
        });
        assert_eq!(ssh.remote_shell_pane_command(), EXPECT);

        let wsl = TestBackend::for_host(&crate::session::HostDef::wsl("Ubuntu"));
        assert_eq!(wsl.remote_shell_pane_command(), EXPECT);

        // The interactive shell must keep stderr on the PTY — a stray
        // `exec … 2>` would make it non-interactive.
        assert!(!EXPECT.contains("-l 2>"));
    }

    #[test]
    fn login_wrap_wraps_remote_command_in_login_shell() {
        // Remote/WSL: the window command runs under a login shell so the user's
        // profile PATH (e.g. `~/.local/bin/claude`) is present, or the agent
        // binary isn't found and the pane dies instantly.
        let backend = TestBackend::for_host(&crate::session::HostDef::wsl("Ubuntu"));
        let wrapped = backend.login_wrap_for_remote("claude --resume x");
        assert_eq!(wrapped, "/bin/sh -lc 'exec claude --resume x'");
    }

    #[test]
    fn login_wrap_assigns_the_hosts_login_path() {
        let host = crate::session::HostDef {
            name: "login-wrap-path".into(),
            destination: "me@devbox".into(),
            ..Default::default()
        };
        crate::agent::host_path::seed(
            &host,
            Some(crate::agent::host_path::HostEnv {
                home: Some("/home/me".into()),
                base: vec!["/usr/bin".into()],
                shell_login: Some(vec!["/home/me/.local/bin".into(), "/usr/bin".into()]),
                sh_login: None,
                multiplexers: Vec::new(),
            }),
        );
        let backend = TestBackend::for_host(&host);
        assert_eq!(
            backend.login_wrap_for_remote("claude"),
            "/bin/sh -lc 'PATH=/home/me/.local/bin:/usr/bin; export PATH; exec claude'"
        );
    }

    /// The commands thurbox runs on a host itself resolve the multiplexer on
    /// the `PATH` its probe found it on, as the window command does (#1461):
    /// a Mac's Homebrew tmux is on the login `PATH`, not sshd's.
    #[test]
    fn a_hosts_multiplexer_commands_carry_its_login_path() {
        let host = crate::session::HostDef {
            name: "mux-command-path".into(),
            destination: "me@mac".into(),
            path_prepend: vec!["/opt/x/bin".into()],
            ..Default::default()
        };
        crate::agent::host_path::seed(
            &host,
            Some(crate::agent::host_path::HostEnv {
                home: Some("/Users/me".into()),
                base: vec!["/usr/bin".into()],
                shell_login: Some(vec!["/opt/homebrew/bin".into(), "/usr/bin".into()]),
                sh_login: None,
                multiplexers: vec![crate::session::Multiplexer::Tmux],
            }),
        );
        let backend = TestBackend::for_host(&host);
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
        // Local backends inherit the user's interactive PATH — no wrap needed.
        let backend = TestBackend::local();
        assert_eq!(backend.login_wrap_for_remote("claude"), "claude");
    }

    #[test]
    fn for_host_honors_socket_and_session_overrides() {
        let host = crate::session::HostDef {
            name: "vm".into(),
            destination: "vm".into(),
            socket: Some("tb-vm".into()),
            session: Some("sess-vm".into()),
            ..Default::default()
        };
        let backend = TestBackend::for_host(&host);
        assert_eq!(backend.socket, "tb-vm");
        assert_eq!(backend.session, "sess-vm");
    }

    /// The same refusal through the contract: a teardown, a restart's listing
    /// and a rename on such a host all stop before a command reaches it —
    /// nothing is killed on a server of this build's guessing, and none of
    /// them opens control mode, which would create one.
    #[test]
    fn nothing_is_done_on_a_host_whose_socket_is_a_guess() {
        let host = crate::session::HostDef {
            name: "devbox".into(),
            destination: "me@devbox".into(),
            ..Default::default()
        };
        let backend = TestBackend::for_host(&host);
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

    use crate::backend::identity::tests::listed;
    use crate::backend::identity::{program_window_name, shell_window_name};
    use crate::backend::tmux_compat::control_mode::{
        decode_octal, format_send_keys, parse_notification, shell_escape, Notification,
    };

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
        assert_eq!(decode_octal(b"\\033"), vec![27]);
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

    // --- local command resolution ---

    /// An executable on a directory only *this process* has on `PATH` — the
    /// shape an agent installed by `fish_add_path` is in.
    #[cfg(unix)]
    fn agent_only_thurbox_can_see(dir: &std::path::Path, name: &str) -> std::path::PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let p = dir.join(name);
        std::fs::write(&p, b"#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o700)).unwrap();
        p
    }

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

    // --- named keys ---

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
        // A gone pane id answers nothing; an unresolved target answers for
        // the client's current pane.
        assert!(!answered_for("%3", None, None));
        assert!(!answered_for("%3", Some("tb-y"), Some("%0")));
        assert!(answered_for("thurbox:=tb-x", Some("tb-x"), Some("%9")));
        assert!(!answered_for("thurbox:=tb-x", Some("tb-y"), Some("%9")));
    }

    /// Where ids are per session, a bare `%2` reached the `__warm__` session's
    /// `%2` (measured on psmux 3.3.7), so no id leaves without its session.
    /// Where they are server-wide (tmux), nothing changes.
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

    /// Where ids are server-wide and windows are killed as windows (tmux), the
    /// kill is what it always was. psmux's is pinned beside its adapter.
    #[test]
    fn a_kill_of_a_server_wide_id_is_unchanged() {
        assert_eq!(
            TestBackend::local().kill_spelling("%8"),
            ("kill-window", Cow::Borrowed("%8"))
        );
    }

    /// A `display-message` answered for another pane — psmux's answer for a
    /// pane it cannot find — records no window, so no close of the active
    /// window can end this pane's reader.
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

    const ONE: &str = "11111111-1111-4111-8111-111111111111";
    const TWO: &str = "22222222-2222-4222-8222-222222222222";

    /// A status question that never reached a server is no answer: the poll
    /// must keep the held states rather than be told there are none.
    #[test]
    fn an_unanswered_status_listing_is_an_error() {
        let backend = TestBackend::with_transport(
            TmuxTransport::local("thurbox-test-no-such-multiplexer"),
            "thurbox-test",
            "thurbox-test",
            "local:tmux",
        );
        assert!(backend.hook_states().is_err());
    }

    /// A multiplexer binary that answers every command it is not told about
    /// with `stderr` and exit 1, and `list-windows` with `windows`.
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
        )
    }

    /// tmux prints `error connecting to` for a socket it cannot open while a
    /// server is alive behind it — another user's, or a stale one. That is a
    /// question nobody answered, so status and the heartbeat must say so
    /// rather than report no states and no heartbeat.
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
            assert!(
                backend.heartbeat_running().is_err(),
                "heartbeat_running: {refused}"
            );
        }
        for absent in [
            "error connecting to /tmp/tmux-1/thurbox-test (No such file or directory)",
            "can't find session: thurbox-test",
        ] {
            let backend = answering_mux(dir.path(), "", absent);
            assert_eq!(backend.hook_states().unwrap(), Vec::new(), "{absent}");
            assert!(!backend.heartbeat_running().unwrap(), "{absent}");
        }
    }

    /// A kill that failed is not "there was none to stop".
    #[cfg(unix)]
    #[test]
    fn a_heartbeat_that_could_not_be_killed_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let backend = answering_mux(
            dir.path(),
            HEARTBEAT_WINDOW,
            "error connecting to /tmp/tmux-1/thurbox-test (Permission denied)",
        );
        assert!(backend.heartbeat_running().unwrap());
        assert!(backend.stop_heartbeat().is_err());
    }

    /// The headless status poll reads an empty listing as "every pane quiet",
    /// so only the server's own "nothing here" may become one.
    #[test]
    fn only_the_server_saying_no_session_is_an_empty_status_answer() {
        for absent in [
            "tmux has-session -t thurbox failed: can't find session: thurbox",
            "tmux has-session -t thurbox failed: no server running on /tmp/tmux-1/thurbox",
            "tmux has-session -t thurbox failed: error connecting to /tmp/tmux-1/thurbox (No such file or directory)",
        ] {
            assert!(mux_answered_absent(absent), "{absent}");
        }
        for unanswered in [
            "tmux has-session -t thurbox failed: ssh: connect to host box port 22: Connection refused",
            "Failed to run tmux command: No such file or directory (os error 2)",
            "tmux has-session -t thurbox failed: error connecting to /tmp/tmux-1/thurbox (Permission denied)",
        ] {
            assert!(!mux_answered_absent(unanswered), "{unanswered}");
        }
    }

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
        // a server without window options keeps everywhere else.
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

    // --- mouse_seed_bytes tests (adopt-time mouse-mode restore) ---

    #[test]
    fn mouse_seed_replays_the_mode_codex_asks_for() {
        // What tmux reports for a running Codex: `?1003` with SGR encoding.
        assert_eq!(mouse_seed_bytes("0,0,1,1,1,0"), b"\x1b[?1003h\x1b[?1006h");
    }

    #[test]
    fn mouse_seed_is_empty_for_a_pane_with_no_tracking() {
        assert!(mouse_seed_bytes("0,0,0,0,0,0").is_empty());
        // An encoding alone tracks nothing, so it is not replayed either.
        assert!(mouse_seed_bytes("0,0,0,0,1,0").is_empty());
        assert!(mouse_seed_bytes("").is_empty());
    }

    #[test]
    fn mouse_seed_falls_back_to_press_reporting_on_an_unknown_mode_flag() {
        // A server that leaves a flag's format unexpanded still says some
        // mode is on; the wheel needs no more than `?1000`.
        assert_eq!(mouse_seed_bytes(",,,1,1,"), b"\x1b[?1000h\x1b[?1006h");
    }

    #[test]
    fn mouse_seed_lands_a_parser_in_the_reported_mode() {
        let mut parser = vt100::Parser::new(2, 2, 0);
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
        assert!(
            seed.len() <= control_mode::MAX_TITLE_SEED_BYTES + 6,
            "{}",
            seed.len()
        );
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

    /// The Windows keeper names its interpreter, like every psmux window: a
    /// server started over Windows OpenSSH would otherwise hand the loop to
    /// `cmd.exe` and the keeper window would close at once.
    #[test]
    fn the_windows_heartbeat_loop_runs_in_named_powershell() {
        use base64::Engine as _;
        let program = heartbeat_loop_command(
            Platform::Windows,
            Path::new("C:\\bin\\thurbox-cli.exe"),
            &["automation".to_string(), "tick".to_string()],
            std::time::Duration::from_secs(30),
        )
        .expect("the keeper fits a Windows command line");
        let encoded = program
            .strip_prefix("powershell.exe -NoLogo -EncodedCommand ")
            .expect("an explicit PowerShell launch");
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .expect("base64");
        let units: Vec<u16> = bytes
            .chunks_exact(2)
            .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
            .collect();
        assert_eq!(
            String::from_utf16(&units).expect("UTF-16LE"),
            "while ($true) { & 'C:\\bin\\thurbox-cli.exe' 'automation' 'tick' *> $null; \
             Start-Sleep 30 }"
        );
    }
}
