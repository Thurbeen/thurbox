use std::borrow::Cow;
use std::collections::HashMap;
use std::io::{Cursor, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::sync_channel;
use std::sync::{Arc, Mutex};

use anyhow::{bail, Context, Result};
use base64::Engine as _;
use tracing::{debug, warn};

use crate::backend::contract::{
    AdoptedSession, DiscoveredSession, Key, Owner, PaneSize, PaneState, Placed, SessionBackend,
    SpawnedSession, WindowRole, WindowSpec,
};
use crate::backend::identity::{window_name_for, Located, WindowIndex, SHELL_WINDOW_PREFIX};
use crate::backend::tmux_compat::control_mode::{
    self, is_broken_pipe, is_recv_timeout, shell_escape, ControlMode, ControlModeReader,
    ControlModeWriter, PANE_CHANNEL_CAPACITY, SIZED_BY, SIZER_OPTION,
};
use crate::backend::tmux_compat::transport::{TmuxTransport, DEFAULT_MUX};

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
/// `tmux_socket`), keyed by the host's machine (`ssh:<name>`), not by route:
/// the name is the host thurbox's *instance* address (ADR-12), derived from
/// its data directory, and every multiplexer that instance drives there runs
/// under it. So a socket learned while driving tmux on a host is the one its
/// psmux or rmux sessions use too. A host entry with no explicit
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
/// Single source of truth shared by [`TmuxBackend::from_host`] and the psmux
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
/// `<DEFAULT_MUX> -L <TMUX_SOCKET> <args…>`. The local-only one-shots below
/// (the heartbeat, status, the duplicate-window sweep) bypass the
/// [`TmuxTransport`] seam, so this centralizes the binary name (`tmux`, or
/// `psmux` on Windows) and socket instead of hardcoding `tmux` at each call
/// site.
fn local_mux_command(args: &[&str]) -> Command {
    let mut cmd = Command::new(DEFAULT_MUX);
    cmd.arg("-L").arg(local_socket()).args(args);
    // Strip nesting env so these one-shots target thurbox's own socket even when
    // thurbox is launched inside a tmux/psmux pane (see `strip_mux_nesting_env`).
    crate::backend::tmux_compat::transport::strip_mux_nesting_env(&mut cmd);
    cmd
}

/// What a **local** one-shot reports when the multiplexer will not start.
///
/// Every helper here bypasses the [`TmuxTransport`] seam because it is
/// local-only, so the transport that failed is always the local one. See
/// [`TmuxTransport::launch_failure`] for why a `NotFound` is answered
/// with a sentence rather than with `os error 2`.
fn local_launch_failure(context: &'static str, err: std::io::Error) -> anyhow::Error {
    TmuxTransport::local().launch_failure(context, err)
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

/// Build the `session:=window` tmux target for a thurbox agent session.
///
/// The `=` prefix forces tmux to match the window name exactly. Without
/// it tmux falls back to FNMATCH-style prefix matching, so a target of
/// `tb-foo` would resolve ambiguously when both `tb-foo` and
/// `tb-foo-bar` exist — `send-keys`/`capture-pane` then fails with
/// "ambiguous window" and the caller's text is silently dropped.
fn window_target(window_name: &str) -> String {
    format!("{TMUX_SESSION}:={window_name}")
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
fn birth_option_commands(window_name: &str, psmux: bool) -> Vec<String> {
    if psmux {
        // psmux has neither option.
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
/// Local tmux only. psmux keeps `@` options in one server-global map and hands
/// the same value back for every window (ADR-13), so every window there would
/// read as stamped for whoever was stamped last; the stamp is withheld at both
/// ends for that reason (issue #1168) and this must not be the one place that
/// believes it.
fn retire_duplicate_windows(session_id: &str, role: WindowRole) -> Option<String> {
    if local_mux_is_psmux() || session_id.is_empty() {
        return None;
    }
    let output = local_mux_command(&["list-windows", "-t", TMUX_SESSION, "-F", RETIRE_FORMAT])
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
        match kill_window_at(window) {
            Ok(()) => {
                retired = true;
                warn!(
                    "retired {window}: it carried session {session_id}'s {} stamp, which \
                     {keep} now holds alone",
                    role.as_str()
                );
            }
            Err(e) => warn!("could not retire {window}, stamped for session {session_id}: {e:#}"),
        }
    }
    retired.then(|| keep.clone())
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

/// How often [`TmuxBackend::ctrl_command_within`] retries the control lock
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

/// Whether the local multiplexer is psmux, which has no usable window options
/// (ADR-13) and so leaves every window unstamped.
///
/// The one place the name fallback still stands: with nothing stamped, two
/// namesakes are genuinely indistinguishable, and refusing to act would be a
/// regression on Windows rather than the safety it is everywhere else.
fn local_mux_is_psmux() -> bool {
    DEFAULT_MUX == "psmux"
}

/// Minimum tmux version required.
const MIN_TMUX_VERSION: (u32, u32) = (3, 2);

/// Parse a `tmux -V` version string (e.g. `"tmux 3.4"`, `"tmux 3.3a"`) into a
/// `(major, minor)` pair. Shared by the local and remote backends.
fn parse_tmux_version(version_str: &str) -> Result<(u32, u32)> {
    let version_part = version_str.strip_prefix("tmux ").unwrap_or(version_str);

    let parts: Vec<&str> = version_part.split('.').collect();
    if parts.len() < 2 {
        bail!("Cannot parse tmux version from: {version_str}");
    }

    let major: u32 = parts[0]
        .parse()
        .with_context(|| format!("Cannot parse tmux major version from: {version_str}"))?;
    // Minor might have a trailing letter (e.g., "3a"), strip non-digits.
    let minor_str: String = parts[1].chars().take_while(char::is_ascii_digit).collect();
    let minor: u32 = minor_str
        .parse()
        .with_context(|| format!("Cannot parse tmux minor version from: {version_str}"))?;

    Ok((major, minor))
}

/// Enforce the minimum-version gate against a multiplexer's `-V` output.
///
/// The `>= 3.2` floor only applies to **real tmux** (a `tmux …` banner). A
/// drop-in clone like psmux numbers itself independently and may print a
/// different banner, so once it has answered `-V` it is accepted as-is — it
/// implements the control-mode feature set regardless of its own number. The
/// psmux floor is a different question, asked where panes are born: see
/// [`check_psmux_version`].
fn check_min_version(version_output: &str) -> Result<()> {
    let trimmed = version_output.trim();
    if let Some(rest) = trimmed.strip_prefix("tmux ") {
        let (major, minor) = parse_tmux_version(rest)?;
        if (major, minor) < MIN_TMUX_VERSION {
            bail!(
                "tmux {major}.{minor} is too old; thurbox requires >= {}.{}",
                MIN_TMUX_VERSION.0,
                MIN_TMUX_VERSION.1
            );
        }
    }
    Ok(())
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

/// The `terminal-features` slot thurbox writes `*:clipboard` into — see
/// `session_config`. High enough that neither tmux's defaults nor a
/// hand-appended list reaches it.
const CLIPBOARD_FEATURE_SLOT: &str = "terminal-features[100]";

/// One command of the session config — a `set-option`, or the `if-shell` that
/// guards [`CLIPBOARD_FEATURE_SLOT`] — and whether failing it means the server
/// cannot host sessions.
struct ConfigOption {
    args: Vec<String>,
    fatal: bool,
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

/// The `set-option` flag for a server-wide option. psmux 3.3.8 refuses `-s`
/// ("unknown flag -s") and keeps one option table anyway, so it gets `-g`,
/// which 3.3.7 and 3.3.8 both take.
fn server_option_scope(psmux: bool) -> &'static str {
    if psmux {
        "-g"
    } else {
        "-s"
    }
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

/// Longest agent activity line replayed from a pane title at adopt time.
///
/// A title is one line in the session list, and the value comes back from a
/// host: bounding it here means a pane whose title is a megabyte cannot make
/// the seed one.
const MAX_TITLE_SEED_BYTES: usize = 512;

/// A tmux backend — sessions persist in `tmux -L <socket>` on either the local
/// machine or a remote host reached over SSH.
///
/// Uses tmux control mode (`-C`) for all I/O after `ensure_ready()`. The only
/// thing that differs between local and remote is the [`TmuxTransport`] used to
/// launch the `tmux` process; the protocol layer is identical.
pub struct TmuxBackend {
    /// How `tmux` is launched (local `Command` vs `ssh <dest> tmux …`).
    transport: TmuxTransport,
    /// tmux socket name passed via `-L` (e.g. `thurbox`) as configured; read
    /// through [`Self::socket`], which prefers what the host's own CLI said.
    socket: String,
    /// tmux session name grouping all thurbox windows.
    session: String,
    /// Backend name used by the registry / persisted `backend_type`
    /// (`local-tmux` or `ssh:<host>`).
    name: String,
    control: Mutex<Option<ControlMode>>,
    /// Set by [`SessionBackend::shutdown`]: from then on no connection is
    /// opened, so a worker still holding the registry as the process quits
    /// cannot bring back the one quit just closed.
    closed: std::sync::atomic::AtomicBool,
    /// This backend's name in [`SIZER_OPTION`] — see [`Self::resize`].
    sizer: String,
    /// The host an off-local backend was built from, for the agent `PATH`
    /// its windows get ([`crate::agent::host_path`]). `None` locally.
    host: Option<crate::session::HostDef>,
    /// The OS of the machine the multiplexer runs on — this one's, or the
    /// host's ([`HostDef::platform`](crate::session::HostDef::platform)). What
    /// decides the shells a pane and the server get, independently of the
    /// multiplexer and of the OS thurbox was built for.
    platform: crate::session::Platform,
}

/// `(rows, cols)` within what `resize-window` accepts, so a resize an `if-shell`
/// runs can never be the one that fails (see `TmuxBackend::resize`). tmux's
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

impl Default for TmuxBackend {
    fn default() -> Self {
        Self::local()
    }
}

impl TmuxBackend {
    /// Build the local tmux backend (`tmux -L thurbox`).
    pub fn new() -> Self {
        Self::local()
    }

    /// Build the local backend on this platform's own multiplexer, named by
    /// the route it serves (`local-tmux`, or `local-psmux` on Windows).
    pub fn local() -> Self {
        Self {
            transport: TmuxTransport::local(),
            socket: local_socket(),
            session: TMUX_SESSION.to_string(),
            name: crate::session::Route::local(Some(
                crate::session::Multiplexer::platform_default(),
            ))
            .format(),
            control: Mutex::new(None),
            closed: std::sync::atomic::AtomicBool::new(false),
            sizer: sizer_name(),
            host: None,
            platform: crate::session::Platform::local(),
        }
    }

    /// Build a tmux backend over an explicit transport (used by the SSH backend).
    /// A remote one is taken for a POSIX host until [`Self::from_host`] says
    /// which host it is.
    pub fn with_transport(
        transport: TmuxTransport,
        socket: impl Into<String>,
        session: impl Into<String>,
        name: impl Into<String>,
    ) -> Self {
        let platform = if transport.is_remote() {
            crate::session::Platform::Posix
        } else {
            crate::session::Platform::local()
        };
        Self {
            transport,
            socket: socket.into(),
            session: session.into(),
            name: name.into(),
            control: Mutex::new(None),
            closed: std::sync::atomic::AtomicBool::new(false),
            sizer: sizer_name(),
            host: None,
            platform,
        }
    }

    /// The backend serving `mux` on `host`, named by that route
    /// (`ssh:<name>:<mux>`). The binary is `mux`, whatever the host's entry
    /// prefers — a row written for tmux is served by tmux on a host that has
    /// since moved to something else — while the host itself is kept as
    /// configured.
    pub fn for_route(host: &crate::session::HostDef, mux: crate::session::Multiplexer) -> Self {
        let mut backend = Self::from_host(&host.served_by(mux));
        backend.host = Some(host.clone());
        backend
    }

    /// Build an off-local tmux backend for `host` — its multiplexer binary
    /// over SSH for an SSH host, or inside a WSL distro via `wsl.exe`. Named by
    /// the route it drives (`ssh:<host.name>:<mux>` / `wsl:…`) and using the
    /// same socket/session names as the local backend unless the host
    /// overrides them.
    pub fn from_host(host: &crate::session::HostDef) -> Self {
        let socket = host_socket(host);
        let session = host
            .session
            .clone()
            .unwrap_or_else(|| TMUX_SESSION.to_string());
        let transport =
            TmuxTransport::remote(crate::shell::HostLauncher::for_host(host), host.mux());
        let mux = host
            .multiplexer()
            .unwrap_or(crate::session::Multiplexer::Tmux);
        let name = host.route(Some(mux)).format();
        let mut backend = Self::with_transport(transport, socket, session, name);
        backend.host = Some(host.clone());
        backend.platform = host.platform();
        backend
    }

    /// The socket this backend talks to: the configured one, unless the host's
    /// own CLI has since reported a different one (see [`learn_host_socket`]).
    /// Resolved per call so a backend registered at startup follows the host.
    fn socket(&self) -> String {
        self.host
            .as_ref()
            .and_then(|host| learned_host_socket(&host.backend_name()))
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
    /// option.
    ///
    /// psmux's is not: `set-option -w -t <pane> @k v` stores one option for the
    /// whole server and `#{@k}` expands to it on every window (measured against
    /// psmux 3.3.6 — ADR-13). So a stamp read back from psmux identifies
    /// nothing, and [`parse_discovered`] drops it rather than reading one
    /// session's id as every window's.
    fn stamps_are_per_window(&self) -> bool {
        !self.transport.uses_psmux()
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
            .tmux_command(&self.socket(), &args)
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
            closed: std::sync::atomic::AtomicBool::new(self.is_closed()),
            sizer: sizer_name(),
            host: self.host.clone(),
            platform: self.platform,
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
    fn attached(&self) -> bool {
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
    fn create_remote_window(&self, spec: &WindowSpec<'_>) -> Result<String> {
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
        Ok(spawned.backend_id)
    }

    /// What `owner`'s `role` window is, where the listing alone could not say.
    ///
    /// A multiplexer without window options (psmux, ADR-13) stamps nothing, so
    /// two namesakes are indistinguishable there and the name is the answer —
    /// on a host, only through the pane the row remembers. Locally, one stamp
    /// on two windows is repairable, and here is where repairing it matters:
    /// `stamped_match` refuses the pair by design, so without this nothing
    /// would ever look again and the session stayed unaddressable for good
    /// (issue #1207). The refusal itself is untouched — the choice is made by
    /// *retiring* a window, which is a write, and never by reading one of two
    /// as the answer.
    fn settle(&self, owner: Owner<'_>, role: WindowRole) -> Result<Located> {
        let remembered = match role {
            WindowRole::Shell => owner.shell_pane,
            _ => owner.agent_pane,
        };
        if self.transport.uses_psmux() {
            let name = window_name_for(role, owner.name);
            if !self.transport.is_remote() {
                return Ok(Located::At(window_target(&name)));
            }
            if remembered.is_empty() {
                return Ok(Located::Unknown);
            }
            // The remembered pane's own window, asked for by pane: a window
            // lists its *selected* pane, which after a split need not be the
            // one the row remembers. A server that restarted reissues ids, so
            // one now in a window of another name is not this row's.
            let panes = self.tmux_output(&[
                "list-panes",
                "-s",
                "-t",
                &self.session,
                "-F",
                "#{pane_id}|#{window_name}",
            ])?;
            return Ok(
                match window_of_pane(&panes, remembered) == Some(name.as_str()) {
                    true => Located::At(remembered.to_string()),
                    false => Located::Unknown,
                },
            );
        }
        if self.transport.is_remote() || retire_duplicate_windows(owner.session_id, role).is_none()
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
            .transport
            .tmux_command(&self.socket(), args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .map_err(|e| {
                self.transport
                    .launch_failure("Failed to run tmux command", e)
            })?;
        if !output.status.success() {
            bail!("{} {what} {}", self.transport.mux(), mux_failure(&output));
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
    fn refuse_exited(&self, pane: &str) -> Result<()> {
        let dead = self
            .tmux_output(&["display-message", "-p", "-t", pane, "#{pane_dead}"])
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
        match self.run_tmux(&["kill-window", "-t", pane]) {
            Err(e) if !already_gone(&format!("{e:#}")) => Err(e),
            _ => Ok(()),
        }
    }

    /// Execute a tmux command on the thurbox socket and check for errors.
    fn run_tmux(&self, args: &[&str]) -> Result<std::process::Output> {
        let output = self
            .transport
            .tmux_command(&self.socket(), args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .map_err(|e| {
                self.transport
                    .launch_failure("Failed to run tmux command", e)
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
    /// On tmux the whole config is **one** invocation (#1243): it runs on every
    /// `session create`, and as ten processes it was most of that command's
    /// cost. A failure in the list is then re-run one option at a time, which is
    /// what tells a fatal option from a best-effort one.
    fn apply_session_config(&self) -> Result<()> {
        let config = self.session_config();
        if !self.transport.uses_psmux() && self.tmux_run(&config_command_list(&[], &config)).is_ok()
        {
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
    fn session_config(&self) -> Vec<ConfigOption> {
        let psmux = self.transport.uses_psmux();
        let scope = server_option_scope(psmux);
        let mut config = Vec::new();
        let mut set = |args: &[&str], fatal: bool| {
            let mut all = vec!["set-option".to_string()];
            all.extend(args.iter().map(ToString::to_string));
            config.push(ConfigOption { args: all, fatal });
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
        if self.platform == crate::session::Platform::Posix {
            set(&[scope, "default-command", &self.config_shell()], true);
        }

        // Server-wide options every supported tmux understands. A failure here
        // means the server can't host sessions, so it is propagated.
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

        // The two silent gates that would otherwise drop an OSC 52 clipboard
        // write originating **inside** a pane (thurbox's own copy, or an
        // agent's). Both are no-ops-on-failure by design, hence best-effort:
        //
        // 1. `set-clipboard` must be exactly `on`. tmux's `input_osc_52_parse`
        //    bails on `!= 2`, and the shipped default is `external` (1) — which
        //    forwards tmux's *own* copy-mode yanks but **discards** an
        //    application's OSC 52 with no error and no visual artifact. This is
        //    the default-broken case: without it every other part of the
        //    clipboard path is dead under tmux.
        // 2. The `Ms` terminfo capability must be present, or
        //    `tty_set_selection` returns early — a second, independent silent
        //    drop. A `*:clipboard` entry in `terminal-features` injects it for
        //    every terminal (tmux 3.2+, matching thurbox's floor; the pre-3.2
        //    form was a raw `terminal-overrides` Ms= string). Written at the end
        //    of the list, below.
        //
        // Security tradeoff: `set-clipboard on` lets any process in a pane set
        // the user's system clipboard — an exfiltration channel, and why tmux
        // moved the default to `external` in 2.6. Scoped here to thurbox's own
        // socket, and the price of copy working at all over SSH.
        //
        // Skipped on psmux, which has no OSC 52 clipboard forwarding (a local
        // Windows session copies via the native clipboard path instead).
        if !psmux {
            set(&["-s", "set-clipboard", "on"], false);
        }

        for (key, val) in SESSION_OPTS {
            set(&["-t", &self.session, key, val], true);
        }
        // Apps inside tmux can inspect this option before deciding whether to
        // request mouse reports. With it off, a full-screen app may leave wheel
        // capture disabled even though thurbox can forward those reports.
        if !psmux {
            set(&["-t", &self.session, "mouse", "on"], true);
        }

        // Window-level options — see `WINDOW_OPTS` for why these are global to
        // the server and why failing to set one is not fatal.
        for (key, val) in WINDOW_OPTS {
            set(&["-w", "-g", key, val], false);
        }

        // The `*:clipboard` feature goes into a fixed slot, and only while that
        // slot is empty. Appending it grew the list by one entry a run, since
        // this runs on every spawn and the server outlives thurbox (#1278); an
        // unconditional write to the slot would overwrite an entry the user's
        // `~/.tmux.conf` put there. Reading the list from Rust first would cost
        // a process per session create, and a format cannot test the whole
        // array on 3.2 (`#{terminal-features}` expands to "") — but it can read
        // one index. `-a` fills the first free index, so appended entries
        // never land on this one.
        if !psmux {
            let slot = format!("#{{{CLIPBOARD_FEATURE_SLOT}}}");
            let write = format!("set-option -qs {CLIPBOARD_FEATURE_SLOT} *:clipboard");
            config.push(ConfigOption {
                args: vec!["if-shell".into(), "-F".into(), slot, String::new(), write],
                fatal: false,
            });
        }
        config
    }

    /// Ensure the thurbox tmux session exists and its options are applied,
    /// **without** starting control mode.
    ///
    /// Shared by [`ensure_ready`](Self::ensure_ready) (which then starts control
    /// mode) and the headless spawn paths ([`create_local_window`],
    /// [`ensure_automation_heartbeat`]) that drive tmux via one-shot commands and
    /// must not open a control-mode connection.
    fn ensure_session_configured(&self) -> Result<()> {
        // The common case — the session is there — asked and configured in one
        // process: `has-session` failing stops the list before any option is
        // set, and the path below then says why.
        if !self.transport.uses_psmux() {
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
            if self.transport.uses_psmux() {
                check_psmux_version(&self.tmux_output(&["-V"])?, &self.socket())?;
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
            // Cheap defensiveness on psmux: poll until the freshly-created
            // session answers `has-session` before applying options. (The
            // `no server running on 'thurbox__thurbox'` failure that originally
            // motivated this was actually psmux session *nesting*, now fixed at
            // the root by `strip_mux_nesting_env`; this poll is a harmless belt
            // against any genuinely-async `new-session -d` and a no-op when the
            // first probe succeeds — which it does on the normal path.)
            if self.transport.uses_psmux() {
                self.wait_for_session_ready();
            }
        }
        self.apply_session_config()
    }

    /// Poll (up to 5s) until the freshly-created session answers `has-session`.
    /// Defensive belt against an async `new-session -d`; normally a no-op (the
    /// first probe succeeds). See
    /// [`ensure_session_configured`](Self::ensure_session_configured).
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
    fn config_shell(&self) -> String {
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
    fn program_for_window(&self, command: &str) -> String {
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
    fn build_shell_command(command: &str, args: &[String]) -> String {
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
    /// it: it has no `/bin/sh` to wrap with (psmux windows are built by
    /// [`psmux_window_command`] instead).
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
    fn login_wrap_for_remote(&self, shell_cmd: &str) -> String {
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
    fn remote_shell_pane_command(&self) -> String {
        let inner = control_mode::shell_escape(
            "command -v \"$SHELL\" >/dev/null 2>&1 && exec \"$SHELL\" -l; exec /bin/sh -l",
        );
        format!("/bin/sh -lc {inner}")
    }

    /// The command a new window runs, as the `new-window` line carries it:
    /// psmux's one PowerShell token, a POSIX host's login-shell bootstrap for
    /// a companion shell pane, or the program itself (login-wrapped on a
    /// POSIX host). Split out of [`SessionBackend::spawn`] so which of the
    /// three a window gets is testable without a server.
    fn window_command(
        &self,
        window_name: &str,
        command: &str,
        args: &[String],
        env: &HashMap<String, String>,
    ) -> String {
        let psmux = self.transport.uses_psmux();
        // A remote/WSL companion shell pane (`tbs-` window) opens the user's own
        // interactive login shell — the SSH-login environment — instead of the
        // bare `/bin/sh` the generic login-wrap would produce (see
        // `remote_shell_pane_command`). Agent windows (`tb-`) and psmux hosts
        // keep the standard path, and so does a Windows host on any
        // multiplexer: it has no `/bin/sh` to bootstrap from.
        let is_remote_shell_pane = self.transport.is_remote()
            && self.platform == crate::session::Platform::Posix
            && !psmux
            && window_name.starts_with(SHELL_WINDOW_PREFIX);
        if psmux {
            Self::psmux_window_command(command, args, env)
        } else if is_remote_shell_pane {
            self.remote_shell_pane_command()
        } else {
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
    /// - [`psmux_window_command`](Self::psmux_window_command) wraps it in
    ///   double quotes for a control-mode `new-window` line, whose parser keeps
    ///   only the *first* trailing token (tmux joins them) — the agent launched
    ///   with no args. psmux's tokenizer concatenates adjacent `'…'` segments
    ///   but passes `'` through `"…"` tokens untouched (backslash is literal
    ///   everywhere, so `C:\` paths are safe) — hence single quotes inside,
    ///   double quotes outside.
    /// - [`create_local_window`] passes it verbatim as a single argv token (the argv
    ///   path joins trailing tokens fine, but still ignores `-e`).
    fn psmux_window_powershell(
        command: &str,
        args: &[String],
        env: &HashMap<String, String>,
    ) -> String {
        let mut ps = String::new();
        // Sort for a deterministic command (HashMap iteration order isn't).
        let mut pairs: Vec<_> = env.iter().collect();
        pairs.sort();
        for (k, v) in pairs {
            ps.push_str(&format!("Set-Item Env:{k} {}; ", ps_single_quote(v)));
        }
        ps.push_str(&format!("& {}", ps_single_quote(command)));
        for a in args {
            ps.push(' ');
            ps.push_str(&ps_single_quote(a));
        }
        ps.replace(['"', '\n'], " ")
    }

    /// [`psmux_window_powershell`](Self::psmux_window_powershell) framed as one
    /// **double-quoted** control-mode token for a `new-window` line.
    fn psmux_window_command(
        command: &str,
        args: &[String],
        env: &HashMap<String, String>,
    ) -> String {
        format!("\"{}\"", Self::psmux_window_powershell(command, args, env))
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
        self.refuse_if_closed()?;
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
    /// can be told, and psmux sends none.
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
        // there, as it always was — a backend that cannot answer (psmux, a
        // reconnecting control mode) keeps the old behaviour of no mapping and
        // no EOF from a window close, and says so in the log.
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
                let format = if self.transport.uses_psmux() {
                    "#{window_id}".to_string()
                } else {
                    format!("#{{window_id}} #{{pane_height}} #{{pane_width}} {SIZED_BY}")
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
        let reports = window_id.is_some() && !self.transport.uses_psmux();
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
        // psmux lacks tmux's `send-keys -H`, so the writer encodes keystrokes
        // differently for it (see `control_mode::send_keys_commands`) and routes
        // a paste out of band (see `control_mode::PsmuxPaste`).
        let psmux = self.transport.uses_psmux();
        let paste =
            psmux.then(|| control_mode::PsmuxPaste::new(self.transport.clone(), self.socket()));
        self.with_control(|ctrl| {
            Ok(ControlModeWriter {
                stdin: Arc::clone(&ctrl.stdin),
                pane_id: pane_id.to_string(),
                psmux,
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
    /// beside it is not, so a mux that answers this differently (psmux is
    /// unverified here) loses the line rather than the scrollback.
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

impl SessionBackend for TmuxBackend {
    /// tmux reports a deleted window in control mode (`%window-close`), so
    /// its stream ends with the window; psmux sends no such event, and a
    /// multiplexer driven here that is not tmux is not assumed to. What the
    /// multiplexer can report decides it — not the machine's OS, nor thurbox's.
    fn needs_liveness_poll(&self) -> bool {
        self.transport.mux() != crate::session::Multiplexer::Tmux.name()
    }
    fn name(&self) -> &str {
        &self.name
    }

    fn check_available(&self) -> Result<()> {
        // `tmux -L <socket> -V` prints the version without connecting, and over
        // the SSH transport this verifies remote connectivity at the same time.
        let output = self
            .transport
            .tmux_command(&self.socket(), &["-V"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .context("tmux is not installed or not in PATH")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            bail!("tmux -V failed: {}", stderr.trim());
        }

        let version_str = String::from_utf8_lossy(&output.stdout);
        check_min_version(&version_str)?;
        debug!("multiplexer version: {}", version_str.trim());
        Ok(())
    }

    fn ensure_ready(&self) -> Result<()> {
        self.refuse_if_closed()?;
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
        // psmux can't take the command as joined trailing tokens nor env via
        // `-e` (see `psmux_window_command`); everything is folded into one
        // token there. tmux keeps the byte-identical multi-token + `-e` path.
        let psmux = self.transport.uses_psmux();
        // Asked of the server, per spawn: it is the server's code that births
        // the pane, and one started before an upgrade keeps running the old.
        if psmux {
            let version = self.ctrl_command("display-message -p '#{version}'")?;
            check_psmux_version(&version, &self.socket())?;
        }
        let shell_cmd = self.window_command(window_name, command, args, env);

        // psmux's tokenizer can't read POSIX `'\''` escapes (see
        // `psmux_quote`), so its `-c`/`-n` values get the double-quote framing
        // it does parse; tmux keeps the byte-identical single-quote path.
        let quote_arg = |s: &str| {
            if psmux {
                control_mode::psmux_quote(s)
            } else {
                control_mode::shell_escape(s)
            }
        };
        let cwd_part = match cwd {
            Some(dir) => format!(" -c {}", quote_arg(&dir.to_string_lossy())),
            None => String::new(),
        };
        let env_part: String = if psmux {
            String::new()
        } else {
            env.iter()
                .map(|(k, v)| format!(" -e {}", shell_escape(&format!("{k}={v}"))))
                .collect()
        };
        let escaped_window_name = quote_arg(window_name);
        let session = &self.session;
        // The window's own options ride along in the same command list — see
        // `birth_options` for why neither can be a message of its own.
        let new_window = format!(
            "new-window -t {session} -n {escaped_window_name} -P -F '{SPAWN_FORMAT}'{cwd_part}{env_part} {shell_cmd}"
        );
        let options = birth_option_commands(window_name, psmux);
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

    fn adopt(
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

    fn capture_history(&self, backend_id: &str) -> Result<Vec<u8>> {
        if !control_mode::is_valid_pane_id(backend_id) {
            bail!("refusing to capture invalid pane id: {backend_id:?}");
        }
        self.capture_history_seed(backend_id)
    }

    fn title_seed(&self, backend_id: &str) -> Vec<u8> {
        if !control_mode::is_valid_pane_id(backend_id) {
            return Vec::new();
        }
        self.pane_title_seed(backend_id)
    }

    /// Not on psmux: nothing there has verified that a reply queues behind the
    /// pane output ahead of it, which is the whole of what makes a snapshot
    /// exact, and its blocks are framed the old way (see
    /// `ControlMode::reader_thread`).
    fn supports_snapshots(&self) -> bool {
        !self.transport.uses_psmux()
    }

    fn request_snapshot(&self, backend_id: &str) -> Result<()> {
        if !control_mode::is_valid_pane_id(backend_id) {
            bail!("refusing to snapshot invalid pane id: {backend_id:?}");
        }
        if !self.supports_snapshots() {
            bail!("psmux cannot snapshot a pane in step with its output");
        }
        // Asked on the loop, so it waits for the lock no longer than any other
        // loop command, and not at all for the answer.
        self.with_control_until(std::time::Instant::now() + LOOP_COMMAND_BUDGET, |ctrl| {
            ctrl.request_snapshot(backend_id, snapshot_history())
        })
    }

    fn snapshot(&self, backend_id: &str) -> Result<crate::backend::contract::PaneSnapshot> {
        if !control_mode::is_valid_pane_id(backend_id) {
            bail!("refusing to snapshot invalid pane id: {backend_id:?}");
        }
        if !self.supports_snapshots() {
            bail!("psmux cannot snapshot a pane");
        }
        // Asked under the control lock, which keeps the answer's place in the
        // queue, and waited for outside it: a search reads many panes at once.
        self.with_control(|ctrl| ctrl.ask_snapshot(backend_id, snapshot_history()))?
            .wait()
    }

    fn set_pane_retention(&self, backend_id: &str, keep: bool) -> Result<()> {
        // The guard every neighbour carries, for the reason a target makes it
        // worth carrying: tmux resolves `-t` as a window *name* as readily as
        // an id, so a caller passing anything else would quietly set
        // `remain-on-exit` on whatever window that name picked out.
        if !control_mode::is_valid_pane_id(backend_id) {
            bail!("refusing to set remain-on-exit on invalid pane id: {backend_id:?}");
        }
        if self.transport.uses_psmux() {
            return Ok(());
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

    fn window_panes(&self, window_name: &str) -> Result<Vec<(String, bool)>> {
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

    fn discover(&self) -> Result<Vec<DiscoveredSession>> {
        // Before control mode has started, one answered `list-windows`: a
        // headless caller asking what a server holds must neither bring one
        // into being nor read a host that did not answer as a host holding
        // nothing.
        //
        // Neither lists a host whose socket is a guess: a sweep that read one
        // would clear its backoff and ask it every pass.
        if !self.attached() {
            self.known_socket()?;
            return self.discover_answered();
        }
        // A session `has-session` cannot see is classified by the answered
        // listing, which tells a server that holds nothing from one that did
        // not answer — read as empty, the second clears a sweep's backoff and
        // lets a relaunch start a second agent.
        if !self.session_exists() {
            return self.discover_answered();
        }
        // Once control mode is up, route through `ctrl_command` so a dead
        // connection is transparently reconnected + retried (like every other
        // control-mode call) instead of failing the discovery.
        let result = self.ctrl_command(&format!(
            "list-windows -t {} -F '{DISCOVER_FORMAT}'",
            self.session
        ))?;

        let stamps = self.stamps_are_per_window();
        Ok(result
            .lines()
            .filter_map(|line| parse_discovered(line, stamps))
            .collect())
    }

    fn create_window(&self, spec: &WindowSpec<'_>) -> Result<String> {
        match self.transport.is_remote() {
            true => self.create_remote_window(spec),
            false => create_local_window(self, spec),
        }
    }

    fn locate(&self, owner: Owner<'_>) -> Result<Placed> {
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

    fn rename_windows(&self, owner: Owner<'_>, to: &str) -> Result<()> {
        self.known_socket()?;
        // Located under the name the session *had*, stamp first, so a
        // namesake's window is never the one renamed. The name is more than
        // looks where a window carries no stamp (psmux, or one spawned before
        // stamping): the name is then all that finds it.
        let index = WindowIndex::from_listing(self.discover_answered()?);
        for role in [WindowRole::Agent, WindowRole::Shell] {
            match index.locate(owner.session_id, owner.name, role, false) {
                Located::At(pane) => {
                    self.tmux_run(&["rename-window", "-t", &pane, &window_name_for(role, to)])?;
                }
                Located::Absent => {}
                Located::Unknown => bail!(
                    "several windows are named after '{}' and none is stamped as this \
                     session's, so there is no telling which one to rename",
                    owner.name
                ),
            }
        }
        Ok(())
    }

    fn stamp_window(&self, backend_id: &str, session_id: &str, role: WindowRole) -> Result<()> {
        if !control_mode::is_valid_pane_id(backend_id) {
            bail!("refusing to stamp an invalid pane id: {backend_id:?}");
        }
        // Nothing to write where an option is not a window's
        // ([`Self::stamps_are_per_window`]): psmux would take this as a
        // *global* one and hand it back as every window's identity. Ok rather
        // than an error for the same reason `set_remain_on_exit` is — the
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
                    self.tmux_run(&["set-option", "-w", "-t", backend_id, option, value])?;
                }
            }
        } else {
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
        }
        // As at creation, and for the same reason: the invariant a stamp is only
        // meaningful under is enforced where the stamp is written, rather than
        // left to a resolver that is required to refuse the pair. The sweep is
        // a local one-shot, so a host's server is left to the teardown that
        // can reach it.
        if !self.transport.is_remote() {
            let _ = retire_duplicate_windows(session_id, role);
        }
        Ok(())
    }

    fn send_text(&self, pane: &str, text: &str, submit: bool) -> Result<()> {
        self.known_socket()?;
        self.refuse_exited(pane)?;
        // Bracketed-paste-wrapped either way (see `paste_prompt_args`), so the
        // text arrives literally: no shell is involved, and the wrap is also
        // what keeps a leading `-` from reading as a flag and a newline from
        // submitting the line before it.
        let paste = paste_prompt_args(pane, text, self.transport.uses_psmux());
        let argv: Vec<&str> = paste.iter().map(String::as_str).collect();
        self.one_shot(&paste[0], &argv)?;
        if !submit {
            return Ok(());
        }
        std::thread::sleep(SEND_KEYS_ENTER_DELAY);
        self.one_shot("send-keys (Enter)", &["send-keys", "-t", pane, "Enter"])?;
        Ok(())
    }

    fn send_text_after(&self, pane: &str, text: &str, delay: std::time::Duration) -> Result<()> {
        self.known_socket()?;
        // A detached timer on the server: the headless caller exits long
        // before the agent it launched is ready for input.
        let script = deferred_prompt_script(
            self.transport.mux(),
            &self.socket(),
            pane,
            text,
            self.transport.uses_psmux(),
        );
        let secs = delay.as_secs().to_string();
        self.one_shot(
            "run-shell (deferred prompt)",
            &["run-shell", "-b", "-d", &secs, &script],
        )?;
        Ok(())
    }

    fn send_key(&self, pane: &str, key: &Key) -> Result<String> {
        self.known_socket()?;
        self.refuse_exited(pane)?;
        let name = tmux_key_name(key);
        self.one_shot(
            &format!("send-keys ({name})"),
            &["send-keys", "-t", pane, &name],
        )?;
        Ok(name)
    }

    fn capture(&self, pane: &str, lines: u32, ansi: bool) -> Result<String> {
        self.known_socket()?;
        let start = format!("-{}", lines.min(MAX_CAPTURE_LINES));
        let mut args = vec!["capture-pane", "-p", "-J", "-t", pane, "-S", &start];
        if ansi {
            args.push("-e");
        }
        let output = self.one_shot("capture-pane", &args)?;
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }

    fn pane_state(&self, pane: &str) -> Result<PaneState> {
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
        let mut argv = Vec::with_capacity(6);
        if !self.transport.uses_psmux() {
            argv.push(PANE_STATE_UTF8_FLAG);
        }
        argv.extend(["display-message", "-p", "-t", pane, &format]);
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
    fn pane_path(&self, pane: &str) -> Result<Option<String>> {
        self.known_socket()?;
        let format = format!(
            "#{{pane_start_command}}{PANE_STATE_SEP}#{{window_name}}{PANE_STATE_SEP}#{{pane_id}}"
        );
        let mut argv = Vec::with_capacity(6);
        if !self.transport.uses_psmux() {
            argv.push(PANE_STATE_UTF8_FLAG);
        }
        argv.extend(["display-message", "-p", "-t", pane, &format]);
        let output = self.one_shot("display-message (pane path)", &argv)?;
        let raw = String::from_utf8_lossy(&output.stdout);
        let start_command = pane_answer_field(&raw, 0).unwrap_or_default();
        let window = pane_answer_field(&raw, 1);
        if !answered_for(
            pane,
            window.as_deref(),
            pane_answer_field(&raw, 2).as_deref(),
        ) {
            bail!("pane {pane} is not there to read");
        }
        Ok(path_from_prefix(&start_command))
    }

    fn resize(&self, backend_id: &str, rows: u16, cols: u16) -> Result<()> {
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
        let (rows, cols) = tmux_size(rows, cols);
        let window = format!("resize-window -t {backend_id} -x {cols} -y {rows}");
        let pane = format!("resize-pane -t {backend_id} -x {cols} -y {rows}");
        if self.transport.uses_psmux() {
            // No `if-shell -F` to decide with: the last instance to paint wins.
            return self.ctrl_command_detached(&[&window, &pane], 2);
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
        let settle =
            format!("set-option -F -w -t {backend_id} {SIZER_OPTION} '#{{?{may},{me},#{{{SIZER_OPTION}}}}}'");
        let mine = format!("#{{==:#{{{SIZER_OPTION}}},{me}}}");
        let only_if_mine = |cmd: &str| {
            format!("if-shell -F -t {backend_id} '{mine}' '{cmd}' 'display-message -p \"\"'")
        };
        self.ctrl_command_detached(&[&settle, &only_if_mine(&window), &only_if_mine(&pane)], 5)
    }

    fn claim_size(&self, backend_id: &str, rows: u16, cols: u16) -> Result<()> {
        if self.transport.uses_psmux() {
            return self.resize(backend_id, rows, cols);
        }
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

    fn is_dead(&self, backend_id: &str) -> Result<bool> {
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

    fn kill(&self, backend_id: &str) -> Result<()> {
        if !self.attached() {
            // The teardown path's kill: one-shot, because opening control mode
            // to kill a window would *create* the server and the thurbox
            // session where they are absent — how tearing a session down came
            // to leave empty servers on other people's machines.
            self.known_socket()?;
            return match self.transport.is_remote() {
                true => self.kill_window_oneshot(backend_id),
                false => kill_window_at(backend_id),
            };
        }
        let _ = self.unregister_pane(backend_id);
        // The window, not only the pane — see `kill_window_oneshot`.
        match self.ctrl_command(&format!("kill-window -t {backend_id}")) {
            Err(e) if !already_gone(&format!("{e:#}")) => Err(e),
            _ => Ok(()),
        }
    }

    fn detach(&self, backend_id: &str) -> Result<()> {
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

    fn pane_pid(&self, backend_id: &str) -> Result<Option<u32>> {
        if !self.attached() {
            // A pane that is already gone has no pid, which is not an error.
            return Ok(self
                .tmux_output(&["display-message", "-p", "-t", backend_id, "#{pane_pid}"])
                .ok()
                .and_then(|pid| pid.trim().parse().ok()));
        }
        let result = self.ctrl_command(&format!(
            "display-message -t {backend_id} -p '#{{pane_pid}}'"
        ))?;
        Ok(result.trim().parse().ok())
    }

    fn pane_pids(&self) -> Result<HashMap<String, u32>> {
        let result = self.ctrl_command("list-panes -a -F '#{pane_id} #{pane_pid}'")?;
        Ok(control_mode::parse_pane_pids(&result))
    }

    fn pane_ids(&self) -> Result<std::collections::HashSet<String>> {
        let result = self.ctrl_command("list-panes -a -F '#{pane_id}'")?;
        Ok(control_mode::parse_pane_ids(&result))
    }

    fn shutdown(&self) {
        // Taking the connection out runs `ControlMode::drop` on the calling
        // thread, which is what lets quit fan the (blocking) teardown out
        // across backends. Idempotent: the mutex holds `None` afterwards, so
        // `TmuxBackend`'s own drop later is a no-op.
        //
        // `lock()` rather than `try_lock()`: a contended lock means another
        // thread is mid-command on this connection, and skipping the teardown
        // would leak the child + reader thread for the process lifetime.
        self.closed
            .store(true, std::sync::atomic::Ordering::Release);
        drop(self.control.lock().ok().and_then(|mut c| c.take()));
    }

    fn take_hook_state_events(&self) -> Vec<(String, String)> {
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

    /// The shell-pane command must match the OS of the machine the pane runs
    /// on ([`Self::platform`](TmuxBackend)), not the local binary's — reading
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

/// Wrap `text` in the bracketed-paste escape sequences (`ESC[200~ … ESC[201~`)
/// so a multi-line prompt is delivered as a single paste — the embedded
/// newlines insert as text instead of submitting the prompt on the first one.
/// Used by [`SessionBackend::send_text`], which is how the TUI reaches this too
/// — the kernel's prompt commands call it rather than framing the paste
/// themselves. The trailing `Enter` is sent separately. tmux delivers
/// these bytes literally via `send-keys -l`.
fn bracketed_paste(text: &str) -> String {
    format!("\x1b[200~{text}\x1b[201~")
}

/// The one-shot argv that delivers `text` into `target` as one paste.
///
/// tmux takes the bracketed-paste-wrapped bytes literally (`send-keys -l`).
/// psmux instead gets its own `send-paste`, which wraps and writes the payload
/// itself (see [`control_mode::PsmuxPaste`] for why key-encoded markers do not
/// survive there): a raw newline inside a psmux command argument is cut by the
/// server's line-oriented read, so a multi-line prompt arrived truncated *and*
/// its tail ran as a psmux command (psmux #560).
fn paste_prompt_args(target: &str, text: &str, psmux: bool) -> Vec<String> {
    if psmux {
        return vec![
            "send-paste".to_string(),
            "-t".to_string(),
            target.to_string(),
            base64::engine::general_purpose::STANDARD.encode(text.as_bytes()),
        ];
    }
    vec![
        "send-keys".to_string(),
        "-t".to_string(),
        target.to_string(),
        "-l".to_string(),
        bracketed_paste(text),
    ]
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
/// Captured rather than inherited — see [`TmuxBackend::one_shot`] (AXI
/// principle 6, "an agent reads one stream"). tmux says nothing at all for
/// some failures, hence the fallback to the bare status.
fn mux_failure(out: &std::process::Output) -> String {
    let stderr = String::from_utf8_lossy(&out.stderr);
    let detail = stderr.trim();
    if detail.is_empty() {
        return format!("exited with status {}", out.status);
    }
    detail.to_string()
}

/// How tmux names each of [`Key::NAMED`] — `ctrl-<letter>` is `C-<letter>`.
///
/// `enter`, `escape`, `tab`, `backspace` and `ctrl-<letter>` are also the set
/// psmux implements (see [`crate::backend::tmux_compat::control_mode::send_keys_commands`]);
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

/// Whether a `display-message` answer came from the pane it was asked about.
///
/// It need not have: against a target it cannot resolve, `display-message`
/// exits 0 and answers for the client's current pane — or, for a pane id that
/// is gone, prints nothing. Reporting a stranger's shell as this pane's
/// foreground process is the plausible wrong answer every field is built to
/// avoid. A pane id must come back as itself; a window target (the name
/// psmux's unstamped windows are reached by) must come back as that window.
fn answered_for(target: &str, window: Option<&str>, pane: Option<&str>) -> bool {
    match target.split_once(":=") {
        Some((_, name)) => window == Some(name),
        None => pane == Some(target),
    }
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
/// `tb-` prefixed so [`TmuxBackend::discover`] ignores it — it is
/// infrastructure, not a session.
const HEARTBEAT_WINDOW: &str = "automation-heartbeat";

/// How often the heartbeat keeper invokes `automation tick`.
const HEARTBEAT_INTERVAL_SECS: u64 = 60;

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

/// The `run-shell` script that pastes the prompt, waits a beat so the paste is
/// consumed, then presses Enter. `run-shell` executes it through the
/// multiplexer server's own shell: a plain `sh` one-liner for tmux, and for
/// psmux — whose `run-shell` is not a POSIX shell — PowerShell, with the prompt
/// travelling as psmux's own base64 `send-paste` payload (see
/// [`paste_prompt_args`]), which also keeps the script free of the prompt's
/// newlines and quotes.
fn deferred_prompt_script(
    mux: &str,
    socket: &str,
    target: &str,
    text: &str,
    psmux: bool,
) -> String {
    if psmux {
        let t = ps_single_quote(target);
        let payload = base64::engine::general_purpose::STANDARD.encode(text.as_bytes());
        return format!(
            "powershell -NoProfile -Command \"{mux} -L {socket} send-paste -t {t} {payload}; \
             Start-Sleep -Milliseconds 200; \
             {mux} -L {socket} send-keys -t {t} Enter\""
        );
    }
    let escaped_target = shell_escape(target);
    // Bracketed-paste wrap (see `bracketed_paste`) so multi-line prompts don't
    // submit early; `-l` makes the multiplexer deliver the bytes literally.
    let escaped_text = shell_escape(&bracketed_paste(text));
    format!(
        "{mux} -L {socket} send-keys -t {escaped_target} -l {escaped_text}; \
         sleep 0.2; \
         {mux} -L {socket} send-keys -t {escaped_target} Enter"
    )
}

/// Wrap `s` in a PowerShell single-quoted literal — the shared
/// [`crate::shell::powershell_quote`], under this file's historical name. Not
/// `#[cfg(windows)]`: `psmux_window_powershell` quotes for a psmux *host* from
/// any local OS.
fn ps_single_quote(s: &str) -> String {
    crate::shell::powershell_quote(s)
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
    TmuxBackend::local().ensure_session_configured()?;
    if list_window_names().iter().any(|w| w == HEARTBEAT_WINDOW) {
        return Ok(());
    }
    let loop_cmd = heartbeat_loop_command(cli_path);
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
    // and not this window's — see the same read in `create_local_window`. There is no
    // `-P` answer to trust here, so the listing is what says whether the window
    // exists.
    if !out.status.success() && !automation_heartbeat_running() {
        bail!("tmux new-window (heartbeat) {}", mux_failure(&out));
    }
    debug!("Armed automation heartbeat keeper window");
    Ok(())
}

/// The keeper's loop, as the window command. It runs via the server's shell,
/// so the CLI path is escaped for it.
#[cfg(not(windows))]
fn heartbeat_loop_command(cli_path: &Path) -> String {
    let cli = shell_escape(&cli_path.display().to_string());
    format!(
        "while true; do {cli} automation tick >/dev/null 2>&1; sleep {HEARTBEAT_INTERVAL_SECS}; done"
    )
}

/// Windows: psmux runs a window command via `powershell -NoLogo -Command`, so
/// the keeper loop is PowerShell — handed over as **one argv token**, dodging
/// psmux's trailing-token handling entirely (same delivery and
/// `ps_single_quote` quoting as `psmux_window_powershell`). This used to be a
/// no-op ("no POSIX shell for the keeper loop"),
/// which silently degraded headless automation firing to TUI-only on Windows.
#[cfg(windows)]
fn heartbeat_loop_command(cli_path: &Path) -> String {
    let cli = ps_single_quote(&cli_path.display().to_string());
    format!(
        "while ($true) {{ & {cli} automation tick *> $null; Start-Sleep {HEARTBEAT_INTERVAL_SECS} }}"
    )
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
/// outright, so the separator survives whatever the environment says.
/// psmux is excluded: it has no such sanitizing and need not know the flag.
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
/// [`TmuxBackend::apply_session_config`].
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
///   attached, so [`TmuxBackend::spawn`] — a restart, a plugin program, the
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
/// already a path, when nothing on `PATH` matches, or on Windows (psmux runs
/// its own command model, and a bare name there wants `PATHEXT` semantics this
/// deliberately does not have). A `command` that is a shell function, an alias,
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

/// [`SessionBackend::create_window`] on this machine's server: a one-shot
/// `new-window`, no control mode, so nothing attaches to what it opens.
///
/// Returns the new pane's id (`%N`). The window is named after its owner —
/// which is *not* unique (two sessions can share a name) — so it is stamped
/// with the owner's id before this returns and every later lookup resolves
/// that (ADR-25).
///
/// A pane id on stdout outranks a non-zero exit status, which on this path can
/// belong to a user's tmux hook rather than to the window — see the read below.
///
/// On Windows the local mux is psmux, whose `new-window -P -F` support is
/// unverified against the documented divergences (ADR-13) — there the id is
/// not asked for and an empty string is returned, so the stamp is written
/// against the window name instead (and is best-effort, like the psmux carve-
/// outs elsewhere). With no id to weigh, a non-zero status is the whole answer
/// there, exactly as before.
fn create_local_window(backend: &TmuxBackend, spec: &WindowSpec<'_>) -> Result<String> {
    let session_id = spec.owner.session_id;
    // Ensure the session exists and is configured, without opening a
    // control-mode connection (headless one-shot path).
    backend.ensure_session_configured()?;
    if local_mux_is_psmux() {
        check_local_psmux_server()?;
    }

    let window_name = window_name_for(spec.role, spec.owner.name);
    // Created at the END of the session's window list, so the retention below
    // can name the window this command just made: `{end}` is the last window
    // and `-a` appends after it, so within this one command list `{end}` is
    // exactly the new one. The window's *name* cannot say that — `tb-<session
    // name>` is not unique (two sessions can share a name, which is why the
    // stamp exists), and tmux resolves a duplicate name to the lowest index,
    // which is the older window (measured, tmux 3.2a). psmux keeps the plain
    // session target: it gets no retention write either, and the shorthand is
    // tmux's.
    let create_target = if cfg!(windows) {
        format!("{TMUX_SESSION}:")
    } else {
        format!("{TMUX_SESSION}:{{end}}")
    };
    let mut tmux = new_window_command(
        &window_name,
        &create_target,
        spec.command,
        spec.args,
        spec.cwd,
        spec.env,
    );
    // The stamp rides in the same command list as the creation, like the birth
    // options: two `set-option` processes fewer on every `session create`
    // (#1243). `{end}` still names the new window here, and the pane id it
    // would otherwise be written against is not known until the list returns.
    let stamped = !local_mux_is_psmux();
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

    let output = tmux
        .output()
        .map_err(|e| local_launch_failure("Failed to run tmux new-window for headless spawn", e))?;
    // psmux reports no pane id, so the stamp goes on the window name — which is
    // the only handle that path has either way.
    let pane_id = if cfg!(windows) {
        String::new()
    } else {
        new_window_pane_id(&output.stdout)
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
            DEFAULT_MUX,
            local_socket()
        );
    }
    // What stamping does after writing a stamp. psmux writes none (ADR-13),
    // and its windows are found by name.
    if stamped {
        let _ = retire_duplicate_windows(session_id, spec.role);
    }
    Ok(pane_id)
}

/// [`check_psmux_version`] against the local server's own `#{version}` — the
/// headless twin of the check in [`TmuxBackend::spawn`].
fn check_local_psmux_server() -> Result<()> {
    let output = local_mux_command(&["display-message", "-t", TMUX_SESSION, "-p", "#{version}"])
        .output()
        .map_err(|e| local_launch_failure("Failed to ask psmux its version", e))?;
    check_psmux_version(&String::from_utf8_lossy(&output.stdout), &local_socket())
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

/// The `new-window` command list [`create_local_window`] runs: the window created
/// detached at `create_target` running `command` — and, on tmux, its birth
/// options chained into the same invocation.
fn new_window_command(
    window_name: &str,
    create_target: &str,
    command: &str,
    args: &[String],
    cwd: Option<&Path>,
    env: &HashMap<String, String>,
) -> Command {
    let mut tmux = local_mux_command(&["new-window", "-d"]);
    if !cfg!(windows) {
        tmux.arg("-a");
    }
    tmux.args(["-t", create_target, "-n", window_name]);
    if !cfg!(windows) {
        tmux.args(["-P", "-F", "#{pane_id}"]);
    }
    if let Some(dir) = cwd {
        tmux.args(["-c", &dir.to_string_lossy()]);
    }
    push_window_program(&mut tmux, command, args, env);

    // Chained into the same command list as the creation, not sent after it —
    // `birth_options` has the measurement. This path passes `-d`, so the new
    // window is not current and the bare form the control-mode path uses is not
    // available; `{end}` names it instead, which is why the window is created
    // there.
    if !cfg!(windows) {
        for (key, value) in birth_options(window_name) {
            tmux.args([";", "set-window-option", "-t", create_target]);
            tmux.args([key, value]);
        }
    }
    tmux
}

/// The window's environment and the program it runs, which close the
/// `new-window` arguments.
fn push_window_program(
    tmux: &mut Command,
    command: &str,
    args: &[String],
    env: &HashMap<String, String>,
) {
    if cfg!(windows) {
        // psmux (the local mux on Windows) ignores `-e`, so the env must be
        // folded into the window command itself; delivered as a single argv
        // token (see `psmux_window_powershell`).
        tmux.arg(TmuxBackend::psmux_window_powershell(command, args, env));
        return;
    }
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

/// Never on Windows: psmux runs its own command model and this path does not
/// reach it — [`push_window_program`] folds the environment into a PowerShell
/// token there instead.
#[cfg(windows)]
fn path_prefix_args() -> Vec<std::ffi::OsString> {
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
    list_hook_states_on(&TmuxBackend::from_host(host))
}

/// [`list_remote_hook_states`] for this machine's own server: the pane option
/// a session created *from afar* on this host sets (its hooks were rewritten to
/// that form), which nothing here read before sessions were shared.
pub fn list_local_hook_states() -> Result<Vec<(String, String)>> {
    list_hook_states_on(&TmuxBackend::local())
}

fn list_hook_states_on(backend: &TmuxBackend) -> Result<Vec<(String, String)>> {
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
fn known_host_socket(host: &crate::session::HostDef) -> Result<String> {
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

/// The name of the window `pane` is in, from a `list-panes -F
/// '#{pane_id}|#{window_name}'` answer.
fn window_of_pane<'a>(listing: &'a str, pane: &str) -> Option<&'a str> {
    listing
        .lines()
        .filter_map(|line| line.split_once('|'))
        .find(|(id, _)| *id == pane)
        .map(|(_, window)| window)
}

/// Whether a kill's failure says its target is already gone — named by its
/// pane or by its name — which is what the kill wanted.
fn already_gone(error: &str) -> bool {
    error.contains("can't find window")
        || error.contains("window not found")
        || error.contains("can't find pane")
}

/// Run `kill-window` on this machine's server against an already-resolved
/// target, tolerating a window that is already gone.
fn kill_window_at(target: &str) -> Result<()> {
    let output = local_mux_command(&["kill-window", "-t", target])
        .output()
        .context("Failed to run tmux kill-window")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if already_gone(&stderr) {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::identity::agent_window_name;
    use crate::backend::identity::tests::listed;
    use crate::backend::identity::{program_window_name, shell_window_name};
    use crate::backend::tmux_compat::control_mode::{
        decode_octal, format_send_keys, parse_notification, shell_escape, Notification,
    };

    #[test]
    fn psmux_surveys_live_panes_when_close_notifications_are_unavailable() {
        let host = crate::session::HostDef {
            name: "windows".into(),
            multiplexer: Some("psmux".into()),
            ..Default::default()
        };
        assert!(TmuxBackend::from_host(&host).needs_liveness_poll());
        #[cfg(windows)]
        assert!(TmuxBackend::new().needs_liveness_poll());
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
        assert_eq!(decode_octal(b"\\033"), vec![27]);
        assert_eq!(format_send_keys("%1", b"A"), "send-keys -t %1 -H 41\n");
        assert_eq!(
            parse_notification("%pause %1"),
            Notification::Pause {
                pane_id: "%1".to_string()
            }
        );
    }

    // --- parse_tmux_version tests ---

    #[test]
    fn parse_tmux_version_plain() {
        assert_eq!(parse_tmux_version("tmux 3.4").unwrap(), (3, 4));
    }

    #[test]
    fn parse_tmux_version_trailing_letter() {
        assert_eq!(parse_tmux_version("tmux 3.3a").unwrap(), (3, 3));
    }

    #[test]
    fn parse_tmux_version_without_prefix() {
        assert_eq!(parse_tmux_version("3.2").unwrap(), (3, 2));
    }

    #[test]
    fn parse_tmux_version_rejects_garbage() {
        assert!(parse_tmux_version("not a version").is_err());
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

    // --- check_min_version (multiplexer version gate) ---

    #[test]
    fn min_version_accepts_recent_tmux() {
        assert!(check_min_version("tmux 3.4").is_ok());
        assert!(check_min_version("tmux 3.2").is_ok());
    }

    #[test]
    fn min_version_rejects_old_tmux() {
        assert!(check_min_version("tmux 2.8").is_err());
    }

    #[test]
    fn min_version_accepts_non_tmux_clone() {
        // psmux numbers itself independently and may not print a `tmux ` banner;
        // once it answers `-V` it is accepted regardless of its own version.
        assert!(check_min_version("psmux 0.3.1").is_ok());
        assert!(check_min_version("psmux 1.0").is_ok());
        assert!(check_min_version("pmux 0.1").is_ok());
    }

    /// psmux 3.3.8 answers `set-option -s` with "unknown flag -s", which failed
    /// every session setup against it; 3.3.7 took either scope.
    #[test]
    fn psmux_server_options_are_set_in_the_global_scope() {
        assert_eq!(server_option_scope(true), "-g");
        assert_eq!(server_option_scope(false), "-s");
    }

    // --- check_psmux_version (the psmux#450 floor) ---

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
                TmuxBackend::local().program_for_window("tbx-spawn-probe"),
                resolve_local_program("tbx-spawn-probe"),
                // A remote host's PATH is the host's, so its command is the
                // host's to resolve — and it is login-wrapped instead.
                TmuxBackend::from_host(&crate::session::HostDef {
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

    // --- build_shell_command tests ---

    #[test]
    fn build_shell_command_simple() {
        let cmd = TmuxBackend::build_shell_command("claude", &[]);
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
        let cmd = TmuxBackend::build_shell_command("claude", &args);
        assert_eq!(cmd, "claude --resume abc-123 --permission-mode default");
    }

    #[test]
    fn build_shell_command_with_spaces_in_args() {
        let args = vec![
            "--allowed-tools".to_string(),
            "Read Bash(git:*)".to_string(),
        ];
        let cmd = TmuxBackend::build_shell_command("claude", &args);
        assert_eq!(cmd, "claude --allowed-tools 'Read Bash(git:*)'");
    }

    #[test]
    fn build_shell_command_escapes_command_path() {
        // The command token is interpreted by the server's shell, so a path
        // with a space (or any metacharacter) must be quoted, not left bare —
        // otherwise the shell would split it and the launch would break.
        let cmd = TmuxBackend::build_shell_command("/opt/My Agents/codex", &["--foo".to_string()]);
        assert_eq!(cmd, "'/opt/My Agents/codex' --foo");
    }

    #[test]
    fn backend_default_has_no_control_mode() {
        let backend = TmuxBackend::new();
        let guard = backend.control.lock().unwrap();
        assert!(guard.is_none());
    }

    #[test]
    fn local_backend_is_named_by_its_local_route_with_local_transport() {
        let backend = TmuxBackend::new();
        let expected = if cfg!(windows) {
            "local:psmux"
        } else {
            "local:tmux"
        };
        assert_eq!(backend.name(), expected);
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
        let backend = TmuxBackend::from_host(&host);
        assert_eq!(backend.name(), "ssh:devbox:tmux");
        assert!(backend.transport.is_remote());
        // Falls back to the default socket/session when the host omits them.
        assert_eq!(backend.socket, TMUX_SOCKET);
        assert_eq!(backend.session, TMUX_SESSION);
    }

    /// A route's multiplexer is the binary, whatever the host now prefers,
    /// and the host itself stays as configured — its platform is not read off
    /// the route.
    #[test]
    fn a_backend_for_a_route_runs_that_routes_binary() {
        let host = crate::session::HostDef {
            name: "devbox".into(),
            destination: "me@devbox".into(),
            multiplexer: Some("rmux".into()),
            ..Default::default()
        };
        let backend = TmuxBackend::for_route(&host, crate::session::Multiplexer::Tmux);
        assert_eq!(backend.name(), "ssh:devbox:tmux");
        assert_eq!(backend.transport.mux(), "tmux");
        assert_eq!(backend.host.as_ref(), Some(&host));
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
        for mux in [
            crate::session::Multiplexer::Tmux,
            crate::session::Multiplexer::Psmux,
        ] {
            assert_eq!(
                TmuxBackend::for_route(&host, mux).socket(),
                "thurbox-elsewhere"
            );
        }
        let mut served = host.clone();
        served.multiplexer = Some("psmux".into());
        assert_eq!(host_socket(&served), "thurbox-elsewhere");
        assert_eq!(known_host_socket(&served).unwrap(), "thurbox-elsewhere");
    }

    #[test]
    fn from_host_builds_named_wsl_backend() {
        let host = crate::session::HostDef::wsl("Ubuntu");
        let backend = TmuxBackend::from_host(&host);
        assert_eq!(backend.name(), "wsl:Ubuntu:tmux");
        assert!(backend.transport.is_remote());
        assert_eq!(backend.transport.launcher(), "wsl.exe");
        let argv: Vec<String> = backend
            .transport
            .tmux_command("s", &[])
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(argv[..2], ["-d", "Ubuntu"]);
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
        assert_eq!(TmuxBackend::local().socket, "thurbox-lab-test");
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
        let winbox = TmuxBackend::from_host(&crate::session::HostDef {
            name: "winbox".into(),
            destination: "me@winbox".into(),
            multiplexer: Some("psmux".into()),
            ..Default::default()
        });
        assert_eq!(winbox.default_shell(), "powershell");

        let devbox = TmuxBackend::from_host(&crate::session::HostDef {
            name: "devbox".into(),
            destination: "me@devbox".into(),
            ..Default::default()
        });
        assert_eq!(devbox.default_shell(), "/bin/sh");

        let wsl = TmuxBackend::from_host(&crate::session::HostDef::wsl("Ubuntu"));
        assert_eq!(wsl.default_shell(), "/bin/sh");

        // Local keeps the platform default ($SHELL / %COMSPEC%).
        let local = TmuxBackend::local();
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
    fn pinned_default_command(backend: &TmuxBackend) -> Option<String> {
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
                pinned_default_command(&TmuxBackend::from_host(&HostDef::wsl("Ubuntu")))
            });
            assert_eq!(pinned.as_deref(), Some("/bin/sh"), "thurbox on {local:?}");
        }
    }

    /// A Windows host is Windows whichever multiplexer serves it: PowerShell
    /// for its shell panes, its own native `default-command`, no `/bin/sh -lc`
    /// wrap — the platform is the host's, never inferred from `psmux`.
    #[test]
    fn a_windows_host_on_a_non_psmux_multiplexer_keeps_native_shell_semantics() {
        use crate::session::{platform::simulate_local, Multiplexer, Platform};
        for local in Platform::ALL {
            simulate_local(local, || {
                let backend = TmuxBackend::for_route(&windows_host("tmux"), Multiplexer::Tmux);
                assert!(!backend.transport.uses_psmux());
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
        use crate::session::{HostDef, Multiplexer};
        let window = format!("{SHELL_WINDOW_PREFIX}work");
        let shell_pane = |backend: &TmuxBackend| {
            backend.window_command(&window, &backend.default_shell(), &[], &HashMap::new())
        };
        let windows = TmuxBackend::for_route(&windows_host("tmux"), Multiplexer::Tmux);
        assert_eq!(shell_pane(&windows), "powershell");
        let posix = TmuxBackend::from_host(&HostDef {
            name: "devbox".into(),
            destination: "me@devbox".into(),
            ..Default::default()
        });
        assert_eq!(shell_pane(&posix), posix.remote_shell_pane_command());
    }

    /// Whether a backend polls for dead panes is what its multiplexer can
    /// report — tmux announces `%window-close`, psmux does not — not the OS
    /// thurbox was built for, nor the host's.
    #[test]
    fn liveness_polling_follows_close_events_not_the_build_os() {
        use crate::session::{platform::simulate_local, HostDef, Platform};
        let local_reports_close = DEFAULT_MUX == "tmux";
        for local in Platform::ALL {
            simulate_local(local, || {
                assert_eq!(
                    TmuxBackend::local().needs_liveness_poll(),
                    !local_reports_close,
                    "local {DEFAULT_MUX} on simulated {local:?}"
                );
                assert!(!TmuxBackend::from_host(&windows_host("tmux")).needs_liveness_poll());
                assert!(TmuxBackend::from_host(&windows_host("psmux")).needs_liveness_poll());
                assert!(!TmuxBackend::from_host(&HostDef::wsl("Ubuntu")).needs_liveness_poll());
            });
        }
    }

    /// The backend a route builds keeps the host's platform, the host's
    /// launcher and the route's multiplexer, each unaffected by the others and
    /// by the OS thurbox is built for. Only the multiplexers this adapter
    /// speaks are built: naming rmux or herdr is a route identity, not an
    /// implementation (see `route_host_os_launcher_and_multiplexer_are_independent`).
    #[test]
    fn a_backend_keeps_platform_launcher_and_multiplexer_apart() {
        use crate::session::{platform::simulate_local, HostDef, HostKind, Multiplexer, Platform};
        for local in Platform::ALL {
            for kind in [HostKind::Ssh, HostKind::Wsl] {
                for platform in [Platform::Posix, Platform::Windows] {
                    for mux in [Multiplexer::Tmux, Multiplexer::Psmux] {
                        let host = HostDef {
                            name: "box".into(),
                            kind,
                            destination: "me@box".into(),
                            platform: Some(platform),
                            ..Default::default()
                        };
                        let backend = simulate_local(local, || TmuxBackend::for_route(&host, mux));
                        let case = format!("{kind:?}/{platform:?}/{mux:?} from {local:?}");
                        assert_eq!(backend.platform, host.platform(), "{case}");
                        assert_eq!(backend.transport.mux(), mux.name(), "{case}");
                        let launcher = match kind {
                            HostKind::Ssh => "ssh",
                            HostKind::Wsl => "wsl.exe",
                        };
                        assert_eq!(backend.transport.launcher(), launcher, "{case}");
                        assert_eq!(backend.name(), host.route(Some(mux)).format(), "{case}");
                        assert_eq!(
                            backend.needs_liveness_poll(),
                            mux != Multiplexer::Tmux,
                            "{case}"
                        );
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
        let backend = TmuxBackend::for_route(&legacy, crate::session::Multiplexer::Tmux);
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
        let ssh = TmuxBackend::from_host(&crate::session::HostDef {
            name: "devbox".into(),
            destination: "me@devbox".into(),
            ..Default::default()
        });
        assert_eq!(ssh.remote_shell_pane_command(), EXPECT);

        let wsl = TmuxBackend::from_host(&crate::session::HostDef::wsl("Ubuntu"));
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
        let backend = TmuxBackend::from_host(&crate::session::HostDef::wsl("Ubuntu"));
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
            }),
        );
        let backend = TmuxBackend::from_host(&host);
        assert_eq!(
            backend.login_wrap_for_remote("claude"),
            "/bin/sh -lc 'PATH=/home/me/.local/bin:/usr/bin; export PATH; exec claude'"
        );
    }

    #[test]
    fn login_wrap_is_noop_for_local() {
        // Local backends inherit the user's interactive PATH — no wrap needed.
        let backend = TmuxBackend::local();
        assert_eq!(backend.login_wrap_for_remote("claude"), "claude");
    }

    // --- one-shot prompt delivery ---

    #[test]
    fn paste_prompt_args_wraps_literally_for_tmux() {
        assert_eq!(
            paste_prompt_args("thurbox:tb-demo", "line one\nline two", false),
            vec![
                "send-keys",
                "-t",
                "thurbox:tb-demo",
                "-l",
                "\x1b[200~line one\nline two\x1b[201~",
            ]
        );
    }

    /// psmux gets its own `send-paste`: the bracketed markers are psmux's to add,
    /// and the base64 payload keeps the prompt's newlines off a command wire that
    /// would otherwise cut the line and run the tail as a command (psmux #560).
    #[test]
    fn paste_prompt_args_uses_send_paste_for_psmux() {
        let args = paste_prompt_args("thurbox:tb-demo", "line one\nline two", true);
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
    fn a_deferred_prompt_names_the_servers_own_mux_and_socket() {
        let tmux = deferred_prompt_script("tmux", "sock", "%3", "it's\nhere", false);
        assert!(tmux.starts_with("tmux -L sock send-keys -t "), "{tmux}");
        assert!(
            tmux.ends_with("tmux -L sock send-keys -t '%3' Enter"),
            "{tmux}"
        );
        let psmux = deferred_prompt_script("psmux", "sock", "%3", "it's\nhere", true);
        assert!(psmux.starts_with("powershell -NoProfile -Command \"psmux -L sock send-paste"));
        // Base64: the prompt's newline and quote never reach the script.
        assert!(!psmux.contains("it's"), "{psmux}");
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

    // --- psmux_window_command tests ---
    // psmux keeps only the FIRST trailing new-window token (tmux joins them) and
    // ignores `-e` entirely, so the whole launch — env included — must be one
    // double-quoted token of PowerShell (verified against psmux 3.3.6).

    #[test]
    fn psmux_window_command_is_one_double_quoted_token() {
        let args = vec!["--session-id".to_string(), "abc-123".to_string()];
        let cmd = TmuxBackend::psmux_window_command("claude", &args, &HashMap::new());
        assert_eq!(cmd, "\"& 'claude' '--session-id' 'abc-123'\"");
    }

    #[test]
    fn psmux_window_command_folds_env_as_set_item() {
        // `Set-Item Env:K 'v'` (not `$env:K`) keeps the string `$`-free; sorted
        // for determinism. Values with spaces survive the PS single quotes.
        let mut env = HashMap::new();
        env.insert("THURBOX_SESSION".to_string(), "id-1".to_string());
        env.insert("B".to_string(), "x y".to_string());
        let cmd = TmuxBackend::psmux_window_command("claude", &[], &env);
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
        let cmd =
            TmuxBackend::psmux_window_command("C:\\Tools\\claude.exe", &args, &HashMap::new());
        assert_eq!(cmd, "\"& 'C:\\Tools\\claude.exe' 'it''s' 'say  hi  now'\"");
    }

    #[test]
    fn login_wrap_is_noop_for_psmux_remote() {
        // A Windows SSH host (multiplexer = "psmux") has no `/bin/sh`; wrapping
        // would replace the agent command with one that can't start at all.
        let host = crate::session::HostDef {
            name: "winbox".into(),
            destination: "me@winbox".into(),
            multiplexer: Some("psmux".into()),
            ..Default::default()
        };
        let backend = TmuxBackend::from_host(&host);
        assert_eq!(backend.login_wrap_for_remote("claude"), "claude");
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
        let backend = TmuxBackend::from_host(&host);
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

    /// A pane is placed in its own window, whichever pane of it is selected.
    #[test]
    fn a_pane_is_placed_in_the_window_it_is_in() {
        let listing = "%3|tb-mine\n%5|tb-mine\n%4|tb-theirs\n";
        assert_eq!(window_of_pane(listing, "%5"), Some("tb-mine"));
        assert_eq!(window_of_pane(listing, "%4"), Some("tb-theirs"));
        assert_eq!(window_of_pane(listing, "%9"), None);
        assert_eq!(window_of_pane(listing, ""), None);
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
        let backend = TmuxBackend::for_route(&host, crate::session::Multiplexer::Tmux);
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
        // `local_mux_is_psmux` already keeps for it everywhere else.
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
        assert!(!TmuxBackend::from_host(&psmux).stamps_are_per_window());
        assert!(TmuxBackend::from_host(&tmux).stamps_are_per_window());
        assert_eq!(
            TmuxBackend::local().stamps_are_per_window(),
            !cfg!(windows),
            "the local multiplexer is psmux on Windows and tmux elsewhere"
        );
    }

    #[test]
    fn window_target_uses_exact_match_prefix() {
        // Without `=`, tmux treats the window name as a pattern and will
        // resolve `tb-foo` ambiguously when both `tb-foo` and
        // `tb-foo-bar` exist. The `=` prefix forces exact-match lookup.
        let t = window_target(&agent_window_name("foo"));
        assert!(t.ends_with(":=tb-foo"), "got {t}");
        let shell = window_target(&shell_window_name("foo"));
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
