//! Loading and seeding of the remote-host config file.
//!
//! Remote SSH hosts are defined declaratively in
//! `~/.config/thurbox/hosts.toml`. Each entry becomes a selectable session
//! backend named `ssh:<name>`. On first run the file is seeded with a
//! commented-out example so a fresh install registers *zero* remote backends
//! and behaves exactly as before. If the file exists but cannot be read or
//! parsed, we fall back to an empty registry rather than failing to start.

use std::collections::HashSet;
use std::path::PathBuf;
use std::process::Command;

use crate::session::{HostDef, HostRegistry, WslRepairPlan};

/// Seed contents for `hosts.toml` on first run: full field documentation plus a
/// commented-out example, but no active hosts.
pub const SEED_HOSTS_TOML: &str = r#"# Thurbox hosts  —  ~/.config/thurbox/hosts.toml
#
# Each [[hosts]] entry describes an off-local target thurbox can run agent
# sessions on: a remote machine over SSH, or a local WSL distro. A host named
# "<name>" registers a session backend ("ssh:<name>" or "wsl:<name>"), offered
# in the new-session host picker (TUI) and selectable with
# `thurbox-cli session create --host <name>`. The agent process, its tmux
# window, and any git worktrees all live on the host (or inside the distro);
# only the TUI runs locally.
#
# SSH hosts: thurbox shells out to the system `ssh` binary, so authentication,
# keys, and connection details all come from your ~/.ssh/config — thurbox never
# handles credentials itself. The remote host needs `tmux` >= 3.2 and `git`.
#
# WSL distros are AUTO-DISCOVERED (via `wsl.exe -l -q`) and appear in the host
# picker with NO config here — only add a [[hosts]] entry with kind = "wsl"
# when you want to override defaults (e.g. worktrees_dir). The distro needs
# `tmux` >= 3.2 and `git` installed inside it; worktrees live in the distro's
# own Linux filesystem (fast), not on /mnt/c.
#
# Running thurbox INSIDE a distro discovers its siblings, but never the distro
# itself: that one is this machine, and a session on it is a plain local
# session (no --host). An entry whose `distro` is that one is ignored with a
# startup warning. An entry merely *named* after it works as written, but it
# records its sessions under the very name an older thurbox mislabelled local
# sessions with, so thurbox warns and leaves every row under that name alone.
# Name a NEW entry after the distro it reaches and there is nothing to warn
# about; renaming an EXISTING one moves no row, and leaves its recorded
# sessions behind under a name no host registers.
#
# This file starts empty (every entry below is commented out): a fresh install
# registers zero SSH hosts (WSL distros still auto-discover) and otherwise
# behaves like a local-only setup. Uncomment and edit an entry to add one.
#
# Unknown keys are reported on startup (and fail `thurbox-cli config
# validate`) but don't break the load.
#
# Fields per [[hosts]] entry:
#
#   name           (string, required)
#       Short, unique identifier. Registers the backend as "ssh:<name>" /
#       "wsl:<name>" and is the value `--host` expects. Example: "devbox".
#
#   kind           (string, optional, default: "ssh")
#       Transport: "ssh" (a remote machine) or "wsl" (a local WSL distro).
#
#   destination    (string, required for kind = "ssh")
#       SSH target passed straight to `ssh`. Either "user@host" or a Host alias
#       defined in your ~/.ssh/config. Example: "me@devbox". Ignored for WSL.
#
#   distro         (string, optional; kind = "wsl" only)
#       WSL distro name (as `wsl.exe -l -q` reports it). Defaults to `name`.
#
#   ssh_opts       (array of strings, optional, default: []; ssh only)
#       Extra flags inserted before the destination, one token per array
#       element (e.g. "-p" then "2222"). thurbox does NOT expand `~`, so use
#       absolute paths for things like `-i <keyfile>`.
#
#   socket         (string, optional, default: "thurbox")
#       Host `tmux -L` socket name. Override only to avoid colliding with
#       another thurbox/tmux server on the same host.
#
#   session        (string, optional, default: "thurbox")
#       Host tmux session name that groups thurbox's windows.
#
#   worktrees_dir  (string, optional)
#       Absolute directory (on the host / inside the distro) under which git
#       worktrees are created. When unset, thurbox uses
#       $HOME/.local/share/thurbox/worktrees there ($HOME resolved on first use).
#
#   multiplexer    (string, optional, default: "tmux")
#       Multiplexer binary on the host. Set to "psmux" when an SSH host is a
#       Windows machine (psmux speaks the same control-mode wire protocol);
#       WSL distros use "tmux".
#
#   share_sessions (bool, optional, default: true)
#       The host's own thurbox database is the record of the sessions on it:
#       thurbox mirrors that database into this one (a session made on the
#       host, or by another thurbox reaching it, shows up here) and creates,
#       deletes, restarts and restores sessions there by running the host's
#       own `thurbox-cli` — which it provisions under
#       ~/.local/share/thurbox/bin/ on the host when the host has none
#       (a release archive of this thurbox's version, checksum-verified;
#       a dev build ships its own binary under thurbox-dev/bin/ instead).
#       Set to false to use the host exactly as before: worktrees and hooks
#       driven from here, nothing mirrored, nothing installed there.
#
config_version = 1

# ──────────────────────────────────────────────────────────────────────────
# Minimal SSH host — the two required fields are enough (uncomment and edit)
# ──────────────────────────────────────────────────────────────────────────
#
# Relies on your ~/.ssh/config for auth and connection tuning. Registers the
# "ssh:laptop" backend, selectable in the host picker / with --host laptop.
#
# [[hosts]]
# name = "laptop"               # → backend "ssh:laptop", value for --host
# destination = "me@laptop"     # "user@host" or a ~/.ssh/config Host alias
#
# ──────────────────────────────────────────────────────────────────────────
# Fully annotated SSH host — every optional field, shown with its default
# ──────────────────────────────────────────────────────────────────────────
#
# [[hosts]]
# name = "devbox"
# destination = "me@devbox"
#
# # ControlMaster reuses one SSH connection so reconnects are instant;
# # ControlPersist keeps it warm; ServerAliveInterval drops half-open links.
# # One token per array element; thurbox does NOT expand `~` (use abs paths).
# ssh_opts = ["-o", "ControlMaster=auto", "-o", "ControlPersist=10m", "-o", "ServerAliveInterval=15"]
#
# # Optional overrides, shown with their defaults:
# # socket = "thurbox"          # remote `tmux -L` socket; change to avoid a clash
# # session = "thurbox"         # remote tmux session grouping thurbox windows
# # worktrees_dir = "/home/me/.local/share/thurbox/worktrees"  # abs remote path
# # multiplexer = "tmux"        # set to "psmux" for a Windows remote host
#
# ──────────────────────────────────────────────────────────────────────────
# WSL distro — only needed to OVERRIDE auto-discovery (distros appear with no
# entry here). Use this to pin a custom worktrees_dir or distro name.
# ──────────────────────────────────────────────────────────────────────────
#
# [[hosts]]
# name = "ubuntu"               # → backend "wsl:ubuntu", value for --host
# kind = "wsl"
# distro = "Ubuntu-22.04"       # the wsl.exe distro name (defaults to `name`)
# # worktrees_dir = "/home/me/.local/share/thurbox/worktrees"  # abs path in WSL
"#;

/// Path to the remote-host config file: `~/.config/thurbox/hosts.toml`
/// (sibling of `config.toml`).
pub fn hosts_config_path() -> Option<PathBuf> {
    crate::paths::config_file().map(|p| p.with_file_name("hosts.toml"))
}

/// Load the remote-host registry, seeding the config file with a commented-out
/// example when it is absent. Any read/parse error degrades gracefully to an
/// empty registry so the TUI always starts (with local-only sessions); the
/// warnings are logged here (headless callers) — the TUI uses
/// [`load_or_seed_with_warnings`] to surface them in the status bar too.
pub fn load_or_seed() -> HostRegistry {
    let (registry, warnings) = load_or_seed_with_warnings();
    for w in &warnings {
        tracing::warn!("{w}");
    }
    registry
}

/// [`load_or_seed`], also returning user-facing warnings for anything that
/// silently degraded (parse error → no remote hosts, seed failure, …).
pub fn load_or_seed_with_warnings() -> (HostRegistry, Vec<String>) {
    match load_or_seed_result() {
        Ok(loaded) => loaded,
        Err(failure) => (HostRegistry::default(), vec![failure]),
    }
}

/// [`load_or_seed_with_warnings`] before the degradation: `Err` is a
/// `hosts.toml` whose contents could **not be established** — an unresolvable
/// path, a seed that could not be written, an unreadable file, a parse error.
/// The `Ok` warnings are the benign ones (an unknown key), where the registry
/// still is what the file says.
///
/// Only one caller needs the distinction, and it needs it badly:
/// [`wsl_repair_plan`] rewrites persisted rows according to what the file
/// says, so reading "could not parse" as "describes no hosts" would relabel a
/// genuinely remote session — silently, once, and unrecoverably. Everything
/// else wants the degraded registry and a warning.
fn load_or_seed_result() -> Result<(HostRegistry, Vec<String>), String> {
    let Some(path) = hosts_config_path() else {
        return Err("Could not resolve hosts.toml path; no remote hosts".into());
    };

    if !path.exists() {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("Failed to create config dir for hosts.toml: {e}"))?;
        }
        std::fs::write(&path, SEED_HOSTS_TOML)
            .map_err(|e| format!("Failed to seed hosts.toml: {e}"))?;
        tracing::info!(path = %path.display(), "Seeded hosts.toml (no active hosts)");
        return Ok((HostRegistry::default(), Vec::new()));
    }

    let contents =
        std::fs::read_to_string(&path).map_err(|e| format!("Failed to read hosts.toml: {e}"))?;
    super::agent_config::parse_toml_reporting_unknown::<HostRegistry>(&contents, "hosts.toml")
        .map_err(|e| {
            format!(
                "hosts.toml: {}; no remote hosts",
                super::agent_config::compact_toml_error(&e.to_string())
            )
        })
}

/// Load configured hosts (`hosts.toml`) and append auto-discovered local WSL
/// distros, returning user-facing warnings. This is what the TUI and the
/// headless `--host` resolver use so WSL distros are selectable with zero
/// config; the discovered set never overrides an explicitly configured host of
/// the same name.
///
/// Neither half may claim the WSL distro thurbox is running inside as a
/// **loopback** ([`HostDef::is_wsl_loopback`]): that one is this machine, so
/// it is dropped. This is the one chokepoint every caller shares, so settling
/// it here (`settle_wsl_self_hosts`) is what keeps a local session local — and
/// the spellings that pass names the rows the one-time repair owes
/// ([`wsl_repair_plan`], which reads the *served* registry these two steps
/// build so it can tell a stale spelling from a live host's own).
pub fn load_all_with_warnings() -> (HostRegistry, Vec<String>) {
    let (mut reg, mut warnings) = load_or_seed_with_warnings();
    warnings.extend(settle_wsl_self_hosts(&mut reg).0);
    augment_with_wsl(&mut reg);
    (reg, warnings)
}

/// [`load_all_with_warnings`] for headless callers: logs the warnings instead
/// of surfacing them in the UI.
pub fn load_all() -> HostRegistry {
    let (reg, warnings) = load_all_with_warnings();
    for w in &warnings {
        tracing::warn!("{w}");
    }
    reg
}

/// [`load_all_with_warnings`], resolved once and held for the process lifetime.
///
/// Loading is not just a file read: WSL discovery walks `$PATH` for `wsl.exe`
/// and runs `wsl.exe -l -q` wherever it finds one — on Windows, and inside a
/// distro, where interop puts it on `PATH` — and the callers on the TUI's hot
/// paths (the snapshot rebuild, `session_ops::resolve_host` per diff request,
/// the metrics/usage sampler, the command drain) were each paying that per
/// call. The same reasoning as `git::remote::remote_home_cache`: `hosts.toml`
/// is read once at startup — the backend registry is built from it then — so
/// repointing a host already requires a restart, and this cache can never be
/// staler than the registry it feeds.
///
/// Cold callers stay on the uncached loaders **deliberately**: `thurbox-cli
/// config validate`/`show` must report the file as it is now, and the
/// short-lived headless paths (`session_ops::spawn`/`delete`/`remote_hooks`,
/// one load per invocation) gain nothing from a process-lifetime pin.
pub fn cached_registry() -> &'static (HostRegistry, Vec<String>) {
    static CACHE: std::sync::OnceLock<(HostRegistry, Vec<String>)> = std::sync::OnceLock::new();
    CACHE.get_or_init(load_all_with_warnings)
}

/// Settle every configured entry that claims the WSL distro thurbox runs
/// inside, in place, returning the warnings it owes the user and the backend
/// names the loopback bug can have written.
///
/// Two rules, and only one of them concerns a row:
///
/// - A [loopback](HostDef::is_wsl_loopback) points `wsl.exe` back at us. It
///   describes this machine as somewhere else, so it is **dropped** — and the
///   rows it wrote are candidates for [`WslRepairPlan::to_local`], under its
///   own backend name as well as the discovered one.
/// - A [shadow](HostDef::shadows_current_wsl_distro) reaches a real sibling
///   while *registering* as `wsl:<us>`. It is **kept exactly as written** and
///   warned about: under that name the bug's local rows and this host's own
///   sibling rows are the same spelling, so nothing there can be classified.
///
/// A candidate is only a candidate: it is [`wsl_repair_plan`] that decides
/// which are safe to rewrite, because that needs the registry as it ends up —
/// including auto-discovery, which this pass runs before.
fn settle_wsl_self_hosts(reg: &mut HostRegistry) -> (Vec<String>, Vec<String>) {
    let mut warnings = Vec::new();
    let mut candidates = Vec::new();

    // The base case, owed before any entry is read: the backend name
    // auto-discovery offered for the current distro until it was filtered.
    if let Some(distro) = crate::session::current_wsl_distro() {
        candidates.push(format!("{}{distro}", crate::session::WSL_BACKEND_PREFIX));
    }

    let mut settled = Vec::with_capacity(reg.hosts.len());
    for host in std::mem::take(&mut reg.hosts) {
        if host.is_wsl_loopback() {
            warnings.push(format!(
                "hosts.toml: ignoring host '{}' — it names the WSL distro thurbox \
                 is running in ('{}'), so sessions on it are local, not remote. \
                 Create them with no --host.",
                host.name,
                host.distro_name()
            ));
            candidates.push(host.backend_name());
            continue;
        }
        if host.shadows_current_wsl_distro() {
            warnings.push(format!(
                "hosts.toml: host '{}' is named after the WSL distro thurbox runs in, \
                 so it records its sessions on '{}' — the same name an older \
                 thurbox wrote onto local sessions by mistake. The host works as \
                 written and keeps its own sessions; but thurbox cannot tell a \
                 mislabelled local session under that name from one of this host's, \
                 so it has left every row recorded there exactly as it found it.",
                host.name,
                host.backend_name()
            ));
        }
        settled.push(host);
    }
    reg.hosts = settled;

    candidates.sort_by_key(|n| n.to_ascii_lowercase());
    candidates.dedup_by(|a, b| a.eq_ignore_ascii_case(b));

    (warnings, candidates)
}

/// The row rewrites the one-time WSL repair owes, or the reason nothing can be
/// said yet.
///
/// `Err` is what keeps the repair honest, and it means one thing: a **question
/// that could not be asked** — `hosts.toml` did not parse, the WSL distros
/// could not be enumerated. Neither is "no hosts configured", so
/// `session_ops::repair_wsl_loopback_rows` touches nothing, leaves the owed
/// mark in place, and comes back. A plan, by contrast, is an answer, even when
/// part of it is [`withheld`](WslRepairPlan::withheld).
///
/// Built from the registry as callers will *see* it — `settle_wsl_self_hosts`
/// then `augment_with`, the same two steps in the same order as
/// [`load_all_with_warnings`] — so what the repair rewrites and what a session
/// resolves against cannot disagree about who owns a spelling.
pub fn wsl_repair_plan() -> Result<WslRepairPlan, String> {
    // Asked first, because it decides the answer on its own: only a loopback
    // can have written one of these rows, and only a thurbox running *inside*
    // a distro can have a loopback. So off WSL there is nothing to repair
    // whatever `hosts.toml` says — and a failure only defers when the answer
    // depends on what failed, or an unrelated typo in that file would defer
    // this forever and re-parse it on every invocation.
    if crate::session::current_wsl_distro().is_none() {
        return Ok(WslRepairPlan::default());
    }
    let (mut reg, _) = load_or_seed_result()?;
    let candidates = settle_wsl_self_hosts(&mut reg).1;
    if any_candidate_a_sibling_could_claim(&candidates) {
        augment_with(&mut reg, discover_wsl_hosts()?);
    }
    Ok(wsl_repair_plan_for(candidates, &reg))
}

/// Whether enumerating the distros could change any candidate's verdict.
///
/// Discovery only ever *adds* hosts ([`augment_with`]), so it can only add
/// claims — and never one for `wsl:<us>`, the spelling it filters out
/// ([`wsl_hosts_from`]). So the only candidate it can decide is one spelled
/// after a *different* distro, which is a hand-written loopback under a `name`
/// that is not this distro's. Nothing hand-written, or an entry named after
/// the distro it points at, means the repair asks `wsl.exe` nothing: no
/// subprocess on the startup path, and no outcome that depends on whether this
/// machine has a working one.
fn any_candidate_a_sibling_could_claim(candidates: &[String]) -> bool {
    let ours = crate::session::current_wsl_distro()
        .map(|d| format!("{}{d}", crate::session::WSL_BACKEND_PREFIX));
    candidates
        .iter()
        .any(|c| !ours.as_deref().is_some_and(|o| o.eq_ignore_ascii_case(c)))
}

/// Split `candidates` into what the repair may rewrite and what it must leave
/// alone, given the registry that will `serve` them.
///
/// A candidate is withheld when a host the registry serves registers under
/// exactly that backend name: rows there may be that host's own, and a live
/// host's sessions relabelled local look for their windows on the wrong tmux
/// server. Full backend names, not bare host names, because that is what a row
/// resolves through — an `ssh:` host named after a distro serves none of its
/// rows.
///
/// Withholding is the **answer** for that name, not a deferral of one: the
/// rows under a claimed spelling are permanently indistinguishable, so no
/// later start can classify them any better. See
/// `session_ops::repair_wsl_loopback_rows`, which retires the repair on this.
fn wsl_repair_plan_for(candidates: Vec<String>, serving: &HostRegistry) -> WslRepairPlan {
    let claimed: HashSet<String> = serving
        .hosts
        .iter()
        .map(|h| h.backend_name().to_ascii_lowercase())
        .collect();
    let (withheld, to_local) = candidates
        .into_iter()
        .partition(|n| claimed.contains(&n.to_ascii_lowercase()));
    WslRepairPlan { to_local, withheld }
}

/// Append auto-discovered WSL distros to `reg`.
///
/// Best-effort, as every caller but the repair wants: a distro that could not
/// be enumerated is one the host picker does not offer, which is the same
/// outcome as not having it.
fn augment_with_wsl(reg: &mut HostRegistry) {
    match discover_wsl_hosts() {
        Ok(discovered) => augment_with(reg, discovered),
        Err(e) => tracing::debug!(error = %e, "no WSL distros auto-discovered"),
    }
}

/// Append `discovered` to `reg`, skipping any whose name already matches a
/// configured host (so a hand-written `hosts.toml` entry for a distro — e.g.
/// with a custom `worktrees_dir` — wins over the bare discovered one).
///
/// Pure, so the load path and the repair path cannot disagree about which host
/// ends up serving a name.
fn augment_with(reg: &mut HostRegistry, discovered: Vec<HostDef>) {
    let configured: HashSet<&str> = reg.hosts.iter().map(|h| h.name.as_str()).collect();
    let fresh: Vec<HostDef> = discovered
        .into_iter()
        .filter(|h| !configured.contains(h.name.as_str()))
        .collect();
    reg.hosts.extend(fresh);
}

/// Infrastructure distros `wsl.exe -l -q` reports that aren't interactive
/// shells — filtered out of auto-discovery (matched case-insensitively).
const WSL_INFRA_DISTROS: &[&str] = &["docker-desktop", "docker-desktop-data"];

/// What [`discover_wsl_hosts`] should answer instead of running `wsl.exe`.
///
/// `wsl_exe_available()` is unconditionally true on Windows, and the CI gate
/// runs the suite there, so without this the repair's outcome is decided by
/// whether the runner happens to have a distro installed. Tests pin the list.
#[cfg(test)]
static DISCOVERY_STUB: std::sync::Mutex<Option<Result<Vec<HostDef>, String>>> =
    std::sync::Mutex::new(None);

/// Run `f` with WSL discovery answering `stub` instead of consulting the
/// machine's own `wsl.exe`, restoring what was there before.
///
/// Serialized on a process-wide lock, and the **only** way a test may set it,
/// for the reason `session::host_def::with_wsl_distro` gives: under plain
/// `cargo test` these tests share one process. Nest it *inside*
/// `with_wsl_distro` when a test needs both, so the two locks are always taken
/// in one order.
#[cfg(test)]
pub(crate) fn with_discovered_wsl<T>(
    stub: Result<Vec<HostDef>, String>,
    f: impl FnOnce() -> T,
) -> T {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let saved = DISCOVERY_STUB
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .replace(stub);
    let out = f();
    *DISCOVERY_STUB.lock().unwrap_or_else(|e| e.into_inner()) = saved;
    out
}

/// Auto-discover installed WSL distros as [`HostDef`]s (kind
/// [`HostKind::Wsl`](crate::session::HostKind::Wsl)), separating "there are
/// none" from "could not say".
///
/// The distro thurbox is itself running inside is **not** among them:
/// `wsl.exe` lists it like any other, but it is this machine
/// ([`HostDef::is_wsl_loopback`] argues what registering it cost). Its
/// siblings are still discovered, so a thurbox inside one distro reaches the
/// rest.
///
/// No `wsl.exe` at all (not Windows, nothing on `PATH`) is an **answer**:
/// there is no WSL here, so there are no distros — and interop puts `wsl.exe`
/// on `PATH` inside a distro, so that case does not overlap with running in
/// one. `Ok(empty)` accordingly.
///
/// `wsl.exe` failing when it *is* there is not an answer: distros may exist
/// and be unlisted. That is `Err`, which is what lets `wsl_repair_plan`
/// withhold instead of reading the silence as "no host claims this name".
pub(crate) fn discover_wsl_hosts() -> Result<Vec<HostDef>, String> {
    #[cfg(test)]
    if let Some(stub) = DISCOVERY_STUB
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
    {
        return stub;
    }
    if !wsl_exe_available() {
        return Ok(Vec::new());
    }
    let output = match Command::new("wsl.exe").arg("-l").arg("-q").output() {
        Ok(o) if o.status.success() => o,
        Ok(o) => return Err(format!("wsl.exe -l -q failed: {}", o.status)),
        Err(e) => return Err(format!("could not run wsl.exe: {e}")),
    };
    Ok(wsl_hosts_from(parse_wsl_distros(&output.stdout)))
}

/// The discoverable hosts among `distros`: one [`HostDef::wsl`] each, minus the
/// loopback. Split out from [`discover_wsl_hosts`] so the exclusion is testable
/// without a live `wsl.exe`.
fn wsl_hosts_from(distros: Vec<String>) -> Vec<HostDef> {
    distros
        .into_iter()
        .map(HostDef::wsl)
        .filter(|h| !h.is_wsl_loopback())
        .collect()
}

/// Whether `wsl.exe` can be invoked: always attempted on Windows; elsewhere
/// only when it resolves on `PATH` (WSL interop exposes it inside a distro, so
/// thurbox running in one WSL distro can still reach its siblings).
fn wsl_exe_available() -> bool {
    cfg!(windows) || crate::paths::which_on_path("wsl.exe")
}

/// Parse `wsl.exe -l -q` output into distro names.
///
/// `wsl.exe` emits **UTF-16LE** on Windows (decoded by [`decode_wsl_output`]);
/// each line is one distro name. Blank lines, the UTF-8/16 BOM, infrastructure
/// distros ([`WSL_INFRA_DISTROS`]), and surrounding whitespace are stripped.
/// Pure + unit-tested so the decoding/filtering is verified without a live
/// `wsl.exe`.
pub(crate) fn parse_wsl_distros(bytes: &[u8]) -> Vec<String> {
    decode_wsl_output(bytes)
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .filter(|l| !WSL_INFRA_DISTROS.contains(&l.to_ascii_lowercase().as_str()))
        .map(str::to_string)
        .collect()
}

/// Decode `wsl.exe` console output, which is UTF-16LE on Windows. Detected by
/// the characteristic interleaved NUL high-bytes of ASCII text; any other
/// producer is decoded as UTF-8. The BOM and stray NULs are stripped.
fn decode_wsl_output(bytes: &[u8]) -> String {
    let sample = bytes.chunks_exact(2).take(8);
    let sampled = sample.clone().count();
    let utf16_like = sampled > 0 && sample.filter(|c| c[1] == 0).count() * 2 > sampled;
    let decoded = if utf16_like {
        let units: Vec<u16> = bytes
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        String::from_utf16_lossy(&units)
    } else {
        String::from_utf8_lossy(bytes).into_owned()
    };
    decoded.replace(['\u{feff}', '\u{0}'], "")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a UTF-16LE byte buffer the way `wsl.exe` emits it.
    fn utf16le(s: &str) -> Vec<u8> {
        s.encode_utf16().flat_map(u16::to_le_bytes).collect()
    }

    #[test]
    fn parse_wsl_distros_decodes_utf16le() {
        let bytes = utf16le("Ubuntu\r\nDebian\r\n");
        assert_eq!(parse_wsl_distros(&bytes), ["Ubuntu", "Debian"]);
    }

    #[test]
    fn parse_wsl_distros_handles_bom_blank_lines_and_utf8() {
        // UTF-16LE with a leading BOM and a trailing blank line.
        let bytes = utf16le("\u{feff}Ubuntu-22.04\r\n\r\n");
        assert_eq!(parse_wsl_distros(&bytes), ["Ubuntu-22.04"]);
        // A UTF-8 producer (non-Windows mock) still parses.
        assert_eq!(parse_wsl_distros(b"Alpine\nFedora\n"), ["Alpine", "Fedora"]);
    }

    #[test]
    fn parse_wsl_distros_filters_infra_distros() {
        let bytes = utf16le("Ubuntu\r\ndocker-desktop\r\ndocker-desktop-data\r\n");
        assert_eq!(parse_wsl_distros(&bytes), ["Ubuntu"]);
    }

    #[test]
    fn parse_wsl_distros_empty_input_yields_no_distros() {
        assert!(parse_wsl_distros(&[]).is_empty());
        assert!(parse_wsl_distros(&utf16le("")).is_empty());
        assert!(parse_wsl_distros(&utf16le("\r\n  \r\n")).is_empty());
    }

    /// A configured entry for a distro wins over the discovered one, so its
    /// overrides survive; every other discovered distro is added.
    #[test]
    fn augment_with_keeps_configured_entries() {
        let mut reg = HostRegistry {
            config_version: None,
            hosts: vec![HostDef {
                name: "Ubuntu".into(),
                kind: crate::session::HostKind::Wsl,
                worktrees_dir: Some("/custom/wt".into()),
                ..Default::default()
            }],
        };

        augment_with(
            &mut reg,
            vec![HostDef::wsl("Ubuntu"), HostDef::wsl("Debian")],
        );

        assert_eq!(reg.names(), ["Ubuntu", "Debian"]);
        assert_eq!(
            reg.get("Ubuntu").unwrap().worktrees_dir.as_deref(),
            Some("/custom/wt"),
            "the configured entry is the one that survives"
        );
    }

    #[test]
    fn discovery_offers_every_distro_but_the_one_we_are_in() {
        crate::session::host_def::with_wsl_distro(Some("MagicDebian"), || {
            let hosts = wsl_hosts_from(vec![
                "Ubuntu".to_string(),
                "MagicDebian".to_string(),
                "MagicDebianPerso".to_string(),
            ]);
            assert_eq!(
                hosts.iter().map(|h| h.name.as_str()).collect::<Vec<_>>(),
                ["Ubuntu", "MagicDebianPerso"],
                "the current distro is this machine; a sibling is a real host"
            );
        });
        // Off WSL there is nothing to exclude.
        crate::session::host_def::with_wsl_distro(None, || {
            assert_eq!(wsl_hosts_from(vec!["Ubuntu".to_string()]).len(), 1);
        });
    }

    fn wsl(name: &str, distro: &str) -> HostDef {
        HostDef {
            name: name.into(),
            kind: crate::session::HostKind::Wsl,
            distro: Some(distro.into()),
            ..Default::default()
        }
    }

    fn registry(hosts: Vec<HostDef>) -> HostRegistry {
        HostRegistry {
            config_version: None,
            hosts,
        }
    }

    /// The three steps `wsl_repair_plan` runs, with `discovered` standing in
    /// for what `wsl.exe -l -q` reports. Only the subprocess is swapped;
    /// settling, augmenting and withholding are the same code the real path
    /// takes.
    fn settle_and_plan(
        reg: &mut HostRegistry,
        discovered: Vec<HostDef>,
    ) -> (Vec<String>, WslRepairPlan) {
        let (warnings, candidates) = settle_wsl_self_hosts(reg);
        augment_with(reg, discovered);
        (warnings, wsl_repair_plan_for(candidates, reg))
    }

    #[test]
    fn a_configured_loopback_is_dropped_and_its_rows_healed() {
        crate::session::host_def::with_wsl_distro(Some("MagicDebian"), || {
            let mut reg = registry(vec![
                wsl("self", "MagicDebian"),
                HostDef::wsl("Ubuntu"),
                HostDef {
                    name: "devbox".into(),
                    destination: "me@devbox".into(),
                    ..Default::default()
                },
            ]);

            let (warnings, plan) = settle_and_plan(&mut reg, Vec::new());

            assert_eq!(reg.names(), ["Ubuntu", "devbox"]);
            assert_eq!(warnings.len(), 1);
            assert!(
                warnings[0].contains("'self'") && warnings[0].contains("MagicDebian"),
                "the warning must name the entry and the distro: {}",
                warnings[0]
            );
            // Its own backend name as well as the discovered one: the rows it
            // wrote are spelled `wsl:self`.
            assert_eq!(plan.to_local, ["wsl:MagicDebian", "wsl:self"]);
            assert!(plan.withheld.is_empty());
        });
    }

    /// A loopback's own backend name is only a *candidate*. `name` is a free
    /// label, so the name a mistaken entry squats on can be a live sibling's —
    /// and dropping the entry hands that name straight back to discovery, so
    /// the real distro re-registers under exactly the spelling the repair was
    /// about to relabel local. Its sessions run in that distro; healing them
    /// would send every attach, diff and delete to the local tmux server.
    #[test]
    fn a_loopbacks_own_name_is_withheld_when_a_discovered_sibling_claims_it() {
        crate::session::host_def::with_wsl_distro(Some("Ubuntu"), || {
            let mut reg = registry(vec![wsl("Debian", "Ubuntu")]);

            let (_, plan) = settle_and_plan(&mut reg, vec![HostDef::wsl("Debian")]);

            assert_eq!(
                reg.names(),
                ["Debian"],
                "the entry is dropped and discovery re-registers the real distro"
            );
            assert_eq!(plan.to_local, ["wsl:Ubuntu"], "the base case still heals");
            assert_eq!(plan.withheld, ["wsl:Debian"]);
        });
    }

    /// The same collision without discovery: two configured entries share a
    /// `name`, and only one of them is the loopback. The survivor serves that
    /// spelling, so the repair must not claim its rows are this machine's.
    #[test]
    fn a_loopbacks_own_name_is_withheld_when_a_configured_host_survives_on_it() {
        crate::session::host_def::with_wsl_distro(Some("Ubuntu"), || {
            let mut reg = registry(vec![wsl("dev", "Ubuntu"), wsl("dev", "Debian")]);

            let (_, plan) = settle_and_plan(&mut reg, Vec::new());

            assert_eq!(reg.names(), ["dev"]);
            assert_eq!(plan.to_local, ["wsl:Ubuntu"]);
            assert_eq!(plan.withheld, ["wsl:dev"]);
        });
    }

    /// An `ssh:` host named after a distro claims nothing: a row spelled
    /// `wsl:<name>` resolves through the backend name, which that host does
    /// not serve, so withholding on the bare name would strand rows for no
    /// reason.
    #[test]
    fn a_bare_name_held_by_an_ssh_host_does_not_withhold_the_wsl_spelling() {
        crate::session::host_def::with_wsl_distro(Some("Ubuntu"), || {
            let mut reg = registry(vec![
                wsl("Debian", "Ubuntu"),
                HostDef {
                    name: "Debian".into(),
                    destination: "me@elsewhere".into(),
                    ..Default::default()
                },
            ]);

            let (_, plan) = settle_and_plan(&mut reg, Vec::new());

            assert_eq!(plan.to_local, ["wsl:Debian", "wsl:Ubuntu"]);
            assert!(plan.withheld.is_empty());
        });
    }

    /// Distros that could not be enumerated are a question that could not be
    /// asked, so the plan is `Err` and the repair stays owed — never a plan
    /// that reads the silence as "nothing claims this name".
    #[test]
    fn an_unenumerable_wsl_yields_no_plan_at_all() {
        let temp = tempfile::TempDir::new().unwrap();
        let _guard = crate::paths::TestPathGuard::new(temp.path());
        let path = hosts_config_path().unwrap();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            "[[hosts]]\nname = \"self\"\nkind = \"wsl\"\ndistro = \"MagicDebian\"\n",
        )
        .unwrap();

        crate::session::host_def::with_wsl_distro(Some("MagicDebian"), || {
            with_discovered_wsl(Err("wsl.exe -l -q failed".to_string()), || {
                assert!(
                    wsl_repair_plan().is_err(),
                    "a candidate named after no distro we know needs the list"
                );
            });
            // The base spelling is one discovery can never claim, so a list it
            // could not produce decides nothing: no subprocess, and the same
            // answer on a machine with a broken `wsl.exe`.
            std::fs::write(&path, "").unwrap();
            with_discovered_wsl(
                Err("wsl.exe must not be consulted here".to_string()),
                || {
                    assert_eq!(
                        wsl_repair_plan().unwrap().to_local,
                        ["wsl:MagicDebian"],
                        "the base case is answered without asking"
                    );
                },
            );
        });
    }

    /// A host named after the current distro reaches a real sibling, so the
    /// entry itself is right and is left exactly as written. What it costs is
    /// the repair: it serves `wsl:<us>`, so the rows there are its own and the
    /// bug's at once and none of them is rewritten.
    #[test]
    fn a_configured_shadow_is_kept_as_written_and_its_rows_are_left_alone() {
        crate::session::host_def::with_wsl_distro(Some("MagicDebian"), || {
            let mut reg = registry(vec![
                HostDef {
                    worktrees_dir: Some("/srv/wt".into()),
                    ..wsl("MagicDebian", "MagicDebianPerso")
                },
                HostDef::wsl("Ubuntu"),
            ]);

            let (warnings, plan) = settle_and_plan(&mut reg, Vec::new());

            assert_eq!(reg.names(), ["MagicDebian", "Ubuntu"]);
            assert_eq!(
                reg.get("MagicDebian").unwrap().worktrees_dir.as_deref(),
                Some("/srv/wt"),
                "the entry is untouched, overrides and all"
            );
            assert!(plan.to_local.is_empty());
            assert_eq!(plan.withheld, ["wsl:MagicDebian"]);
            assert_eq!(warnings.len(), 1);
            assert!(
                warnings[0].contains("'wsl:MagicDebian'")
                    && warnings[0].contains("left every row recorded there"),
                "the warning must name the spelling and say the rows were left alone: {}",
                warnings[0]
            );
        });
    }

    /// Nothing about a shadow is decided by what else is in the file: a second
    /// one, or another host holding the sibling's name, changes neither the
    /// registry nor the plan. Every entry the user wrote keeps working, and the
    /// one unreadable spelling is still the only thing withheld.
    #[test]
    fn other_entries_do_not_change_what_a_shadow_settles_to() {
        crate::session::host_def::with_wsl_distro(Some("MagicDebian"), || {
            let mut reg = registry(vec![
                wsl("MagicDebian", "MagicDebianPerso"),
                wsl("MagicDebian", "Debian-12"),
                wsl("MagicDebianPerso", "Debian-12"),
                HostDef {
                    name: "devbox".into(),
                    destination: "me@devbox".into(),
                    ..Default::default()
                },
            ]);

            let (warnings, plan) = settle_and_plan(&mut reg, Vec::new());

            assert_eq!(
                reg.names(),
                ["MagicDebian", "MagicDebian", "MagicDebianPerso", "devbox"],
                "no entry is dropped or renamed"
            );
            assert!(plan.to_local.is_empty());
            assert_eq!(plan.withheld, ["wsl:MagicDebian"]);
            assert_eq!(warnings.len(), 2, "one per shadow: {warnings:?}");
        });
    }

    /// The two entries at their most confusing: one is a loopback *named*
    /// after the sibling, the other a shadow reaching it. Each is settled on
    /// its own rule and neither reaches into the other's spelling — the
    /// loopback's rows are this machine's and are healed, the shadow's name is
    /// withheld.
    #[test]
    fn a_loopback_is_still_healed_alongside_a_shadow() {
        crate::session::host_def::with_wsl_distro(Some("MagicDebian"), || {
            let mut reg = registry(vec![
                wsl("MagicDebianPerso", "MagicDebian"),
                wsl("MagicDebian", "MagicDebianPerso"),
            ]);

            let (_, plan) = settle_and_plan(&mut reg, Vec::new());

            assert_eq!(reg.names(), ["MagicDebian"], "only the loopback is dropped");
            assert_eq!(plan.to_local, ["wsl:MagicDebianPerso"]);
            assert_eq!(plan.withheld, ["wsl:MagicDebian"]);
        });
    }

    #[test]
    fn off_wsl_nothing_is_settled_and_nothing_is_owed() {
        crate::session::host_def::with_wsl_distro(None, || {
            let mut reg = registry(vec![HostDef::wsl("Ubuntu"), wsl("work", "Debian")]);

            let (warnings, plan) = settle_and_plan(&mut reg, Vec::new());

            assert_eq!(reg.names(), ["Ubuntu", "work"]);
            assert!(warnings.is_empty());
            assert!(plan.is_empty() && plan.withheld.is_empty());
        });
    }

    /// The plan is decided by what `hosts.toml` says, so a file that cannot be
    /// parsed is not an answer — it must not read as "no entry claims
    /// `wsl:<us>`", which would relabel a shadow host's sibling sessions local.
    #[test]
    fn an_unparseable_hosts_toml_yields_no_plan_at_all() {
        let temp = tempfile::TempDir::new().unwrap();
        let _guard = crate::paths::TestPathGuard::new(temp.path());
        let path = hosts_config_path().unwrap();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "[[hosts]\nname = \"MagicDebian\"\n").unwrap();

        crate::session::host_def::with_wsl_distro(Some("MagicDebian"), || {
            assert!(
                wsl_repair_plan().is_err(),
                "an unreadable file is a failure, not an empty registry"
            );
            // The degrading loader still answers, for every other caller.
            let (reg, warnings) = load_or_seed_with_warnings();
            assert!(reg.is_empty() && !warnings.is_empty());
        });
    }

    #[test]
    fn seed_toml_parses_to_empty_registry() {
        let reg: HostRegistry = toml::from_str(SEED_HOSTS_TOML).unwrap();
        assert!(reg.is_empty());
    }

    /// Seed ships both a minimal and a fully-annotated example, kept commented
    /// (the empty-registry test above proves they don't register).
    #[test]
    fn seed_toml_documents_minimal_and_full_examples() {
        for marker in ["Minimal SSH host", "Fully annotated SSH host", "WSL distro"] {
            assert!(
                SEED_HOSTS_TOML.contains(marker),
                "hosts.toml seed must include the '{marker}' example"
            );
        }
    }

    /// The seeded `hosts.toml` is the primary documentation users see, so it
    /// must describe every configurable field. Guards against adding a
    /// `HostDef` field without documenting it here.
    #[test]
    fn seed_toml_documents_every_host_field() {
        for field in [
            "name",
            "kind",
            "destination",
            "distro",
            "ssh_opts",
            "socket",
            "session",
            "worktrees_dir",
            "multiplexer",
        ] {
            assert!(
                SEED_HOSTS_TOML.contains(field),
                "hosts.toml seed must document the '{field}' field"
            );
        }
    }

    #[test]
    fn load_or_seed_writes_file_when_absent_and_stays_empty() {
        let temp = tempfile::TempDir::new().unwrap();
        let _guard = crate::paths::TestPathGuard::new(temp.path());

        let path = hosts_config_path().unwrap();
        assert!(!path.exists());

        let reg = load_or_seed();
        assert!(reg.is_empty());
        assert!(path.exists(), "hosts.toml should have been seeded");

        // Second call reads the seeded file and is still empty.
        assert!(load_or_seed().is_empty());
    }

    #[test]
    fn load_or_seed_falls_back_on_malformed_file() {
        let temp = tempfile::TempDir::new().unwrap();
        let _guard = crate::paths::TestPathGuard::new(temp.path());

        let path = hosts_config_path().unwrap();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "this is not = valid toml {{{").unwrap();

        assert!(load_or_seed().is_empty());
    }

    #[test]
    fn load_or_seed_reads_configured_host() {
        let temp = tempfile::TempDir::new().unwrap();
        let _guard = crate::paths::TestPathGuard::new(temp.path());

        let path = hosts_config_path().unwrap();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            "[[hosts]]\nname = \"devbox\"\ndestination = \"me@devbox\"\n",
        )
        .unwrap();

        let reg = load_or_seed();
        assert_eq!(reg.names(), ["devbox"]);
        assert_eq!(reg.get("devbox").unwrap().backend_name(), "ssh:devbox");
    }
}
