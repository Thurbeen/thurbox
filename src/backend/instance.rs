//! Which server an instance's sessions live on (ADR-12): the socket name a
//! tmux-compatible multiplexer is addressed by with `-L`, here and on a host.
//!
//! The instance's address, not one multiplexer's: every adapter that speaks
//! the tmux protocol runs its server under it, so a name learned while driving
//! one multiplexer on a host is the one every other there uses too.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use anyhow::{bail, Result};

/// Dedicated tmux socket name for an instance running out of the **default**
/// data dir — isolates thurbox sessions from the user's tmux. Dev builds use
/// "thurbox-dev" to avoid interfering with an installed release binary. An
/// instance relocated by `THURBOX_DATA_DIR` derives its own name from this one
/// (`derived_socket`). Also the last-resort fallback when a host's
/// configured socket sanitizes to empty in psmux's hook command.
pub const TMUX_SOCKET: &str = if cfg!(dev_build) {
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
pub(crate) fn local_socket() -> String {
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

pub(crate) fn learned_host_socket(backend_name: &str) -> Option<String> {
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
/// Single source of truth shared by the adapters built for a host and the psmux
/// hook-signal rewrite (which must bake the socket into the command — psmux has
/// no `$TMUX`-style in-pane socket resolution to rely on).
pub fn host_socket(host: &crate::session::HostDef) -> String {
    host.socket
        .clone()
        .or_else(|| learned_host_socket(&host.backend_name()))
        .unwrap_or_else(|| TMUX_SOCKET.to_string())
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
pub(crate) fn known_host_socket(host: &crate::session::HostDef) -> Result<String> {
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

#[cfg(test)]
mod tests {
    use super::*;

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
        // Empty counts as unset — a sandbox script exporting `THURBOX_SOCKET=`
        // must not produce `-L ''`.
        std::env::set_var(SOCKET_OVERRIDE_ENV, "");
        assert_eq!(local_socket(), TMUX_SOCKET);
        std::env::remove_var(SOCKET_OVERRIDE_ENV);
        assert_eq!(local_socket(), TMUX_SOCKET);
    }
}
