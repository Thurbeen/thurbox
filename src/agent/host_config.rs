//! Loading and seeding of the remote-host config file.
//!
//! Remote SSH hosts are defined declaratively in
//! `~/.config/thurbox/hosts.toml`. Each entry becomes a selectable session
//! backend named `ssh:<name>`. On first run the file is seeded with a
//! commented-out example so a fresh install registers *zero* remote backends
//! and behaves exactly as before. If the file exists but cannot be read or
//! parsed, we fall back to an empty registry rather than failing to start.

use std::path::PathBuf;

use crate::session::HostRegistry;

/// Seed contents for `hosts.toml` on first run: full field documentation plus a
/// commented-out example, but no active hosts.
pub const SEED_HOSTS_TOML: &str = r#"# Thurbox remote SSH hosts  —  ~/.config/thurbox/hosts.toml
#
# Each [[hosts]] entry describes a remote machine thurbox can run agent sessions
# on, over SSH. A host named "<name>" registers a session backend called
# "ssh:<name>", offered in the new-session host picker (TUI) and selectable with
# `thurbox-cli session create --host <name>`. The agent process, its tmux
# window, and any git worktrees all live on the remote host; only the TUI runs
# locally.
#
# thurbox shells out to the system `ssh` binary, so authentication, keys, and
# connection details all come from your ~/.ssh/config — thurbox never handles
# credentials itself. The remote host needs `tmux` >= 3.2 and `git`.
#
# This file starts empty (every entry below is commented out), so a fresh
# install registers zero remote hosts and behaves exactly like a local-only
# setup. Uncomment and edit an entry to add a host.
#
# Unknown keys are reported on startup (and fail `thurbox-cli config
# validate`) but don't break the load.
#
# Fields per [[hosts]] entry:
#
#   name           (string, required)
#       Short, unique identifier. Registers the backend as "ssh:<name>" and is
#       the value `--host` expects. Example: "devbox".
#
#   destination    (string, required)
#       SSH target passed straight to `ssh`. Either "user@host" or a Host alias
#       defined in your ~/.ssh/config. Example: "me@devbox".
#
#   ssh_opts       (array of strings, optional, default: [])
#       Extra flags inserted before the destination, one token per array
#       element (e.g. "-p" then "2222"). thurbox does NOT expand `~`, so use
#       absolute paths for things like `-i <keyfile>`.
#
#   socket         (string, optional, default: "thurbox")
#       Remote `tmux -L` socket name. Override only to avoid colliding with
#       another thurbox/tmux server on the same remote host.
#
#   session        (string, optional, default: "thurbox")
#       Remote tmux session name that groups thurbox's windows.
#
#   worktrees_dir  (string, optional)
#       Absolute remote directory under which git worktrees are created. When
#       unset, thurbox uses $HOME/.local/share/thurbox/worktrees on the remote
#       (the remote $HOME is resolved over ssh on first use).
#
#   multiplexer    (string, optional, default: "tmux")
#       Remote multiplexer binary. Set to "psmux" when the remote host is a
#       Windows machine (psmux speaks the same control-mode wire protocol).
#
config_version = 1

# ──────────────────────────────────────────────────────────────────────────
# Minimal host — the two required fields are enough (uncomment and edit)
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
# Fully annotated host — every optional field, shown with its default
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seed_toml_parses_to_empty_registry() {
        let reg: HostRegistry = toml::from_str(SEED_HOSTS_TOML).unwrap();
        assert!(reg.is_empty());
    }

    /// Seed ships both a minimal and a fully-annotated example, kept commented
    /// (the empty-registry test above proves they don't register).
    #[test]
    fn seed_toml_documents_minimal_and_full_examples() {
        for marker in ["Minimal host", "Fully annotated host"] {
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
            "destination",
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
