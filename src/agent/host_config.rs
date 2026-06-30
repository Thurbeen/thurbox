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

use crate::session::{HostDef, HostRegistry};

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
# WSL distros are AUTO-DISCOVERED on Windows (via `wsl.exe -l -q`) and appear in
# the host picker with NO config here — only add a [[hosts]] entry with
# kind = "wsl" when you want to override defaults (e.g. worktrees_dir). The
# distro needs `tmux` >= 3.2 and `git` installed inside it; worktrees live in
# the distro's own Linux filesystem (fast), not on /mnt/c.
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
    let Some(path) = hosts_config_path() else {
        return (
            HostRegistry::default(),
            vec!["Could not resolve hosts.toml path; no remote hosts".into()],
        );
    };

    if !path.exists() {
        if let Some(parent) = path.parent() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                return (
                    HostRegistry::default(),
                    vec![format!("Failed to create config dir for hosts.toml: {e}")],
                );
            }
        }
        if let Err(e) = std::fs::write(&path, SEED_HOSTS_TOML) {
            return (
                HostRegistry::default(),
                vec![format!("Failed to seed hosts.toml: {e}")],
            );
        }
        tracing::info!(path = %path.display(), "Seeded hosts.toml (no active hosts)");
        return (HostRegistry::default(), Vec::new());
    }

    match std::fs::read_to_string(&path) {
        Ok(contents) => {
            match super::agent_config::parse_toml_reporting_unknown::<HostRegistry>(
                &contents,
                "hosts.toml",
            ) {
                Ok((reg, warnings)) => (reg, warnings),
                Err(e) => (
                    HostRegistry::default(),
                    vec![format!(
                        "hosts.toml: {}; no remote hosts",
                        super::agent_config::compact_toml_error(&e.to_string())
                    )],
                ),
            }
        }
        Err(e) => (
            HostRegistry::default(),
            vec![format!("Failed to read hosts.toml: {e}")],
        ),
    }
}

/// Load configured hosts (`hosts.toml`) and append auto-discovered local WSL
/// distros, returning user-facing warnings. This is what the TUI and the
/// headless `--host` resolver use so WSL distros are selectable with zero
/// config; the discovered set never overrides an explicitly configured host of
/// the same name.
pub fn load_all_with_warnings() -> (HostRegistry, Vec<String>) {
    let (mut reg, warnings) = load_or_seed_with_warnings();
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

/// Append auto-discovered WSL distros to `reg`, skipping any whose name already
/// matches a configured host (so a hand-written `hosts.toml` entry for a distro
/// — e.g. with a custom `worktrees_dir` — wins over the bare discovered one).
fn augment_with_wsl(reg: &mut HostRegistry) {
    let configured: HashSet<&str> = reg.hosts.iter().map(|h| h.name.as_str()).collect();
    let discovered: Vec<HostDef> = discover_wsl_hosts()
        .into_iter()
        .filter(|h| !configured.contains(h.name.as_str()))
        .collect();
    reg.hosts.extend(discovered);
}

/// Infrastructure distros `wsl.exe -l -q` reports that aren't interactive
/// shells — filtered out of auto-discovery (matched case-insensitively).
const WSL_INFRA_DISTROS: &[&str] = &["docker-desktop", "docker-desktop-data"];

/// Auto-discover installed WSL distros as [`HostDef`]s (kind
/// [`HostKind::Wsl`](crate::session::HostKind::Wsl)).
///
/// Runs `wsl.exe -l -q` and parses its output. Returns empty when `wsl.exe`
/// isn't available (any non-Windows host, or Windows without WSL) or the
/// command fails — discovery is strictly best-effort and never blocks startup.
pub(crate) fn discover_wsl_hosts() -> Vec<HostDef> {
    if !wsl_exe_available() {
        return Vec::new();
    }
    let output = match Command::new("wsl.exe").arg("-l").arg("-q").output() {
        Ok(o) if o.status.success() => o,
        Ok(o) => {
            tracing::debug!(
                status = ?o.status,
                "wsl.exe -l -q failed; no WSL distros auto-discovered"
            );
            return Vec::new();
        }
        Err(e) => {
            tracing::debug!(error = %e, "could not run wsl.exe; no WSL distros auto-discovered");
            return Vec::new();
        }
    };
    parse_wsl_distros(&output.stdout)
        .into_iter()
        .map(HostDef::wsl)
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

    #[test]
    fn augment_with_wsl_keeps_configured_entries() {
        // A configured host named "Ubuntu" must not be duplicated by discovery.
        // (discover_wsl_hosts() returns empty off-Windows here, but the dedup
        // logic is exercised directly.)
        let mut reg = HostRegistry {
            config_version: None,
            hosts: vec![HostDef {
                name: "Ubuntu".into(),
                kind: crate::session::HostKind::Wsl,
                worktrees_dir: Some("/custom/wt".into()),
                ..Default::default()
            }],
        };
        let before = reg.hosts.len();
        augment_with_wsl(&mut reg);
        // The configured Ubuntu survives untouched; on a non-WSL host nothing
        // else is added.
        assert_eq!(
            reg.get("Ubuntu").unwrap().worktrees_dir.as_deref(),
            Some("/custom/wt")
        );
        assert!(reg.hosts.len() >= before);
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
