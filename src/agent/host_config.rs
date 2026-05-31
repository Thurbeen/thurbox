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

/// Seed contents for `hosts.toml` on first run: documentation + a commented-out
/// example, but no active hosts.
pub const SEED_HOSTS_TOML: &str = r#"# Thurbox remote SSH hosts.
#
# Each [[hosts]] entry describes a remote machine thurbox can run agent
# sessions on, over SSH. Each becomes a backend named "ssh:<name>", selectable
# when creating a session. Authentication and host details resolve via your
# ~/.ssh/config — thurbox shells out to the system `ssh` binary.
#
# Uncomment and edit to add a host:
#
# [[hosts]]
# name = "devbox"
# destination = "me@devbox"
# # ControlMaster keeps one SSH connection alive so reconnects are instant;
# # ServerAliveInterval drops half-open links promptly.
# ssh_opts = ["-o", "ControlMaster=auto", "-o", "ControlPersist=10m", "-o", "ServerAliveInterval=15"]
# # Optional: absolute remote dir for git worktrees (defaults to
# # $HOME/.local/share/thurbox/worktrees on the remote).
# # worktrees_dir = "/home/me/.local/share/thurbox/worktrees"
"#;

/// Path to the remote-host config file: `~/.config/thurbox/hosts.toml`
/// (sibling of `config.toml`).
pub fn hosts_config_path() -> Option<PathBuf> {
    crate::paths::config_file().map(|p| p.with_file_name("hosts.toml"))
}

/// Load the remote-host registry, seeding the config file with a commented-out
/// example when it is absent. Any read/parse error degrades gracefully to an
/// empty registry so the TUI always starts (with local-only sessions).
pub fn load_or_seed() -> HostRegistry {
    let Some(path) = hosts_config_path() else {
        tracing::warn!("Could not resolve hosts.toml path; no remote hosts");
        return HostRegistry::default();
    };

    if !path.exists() {
        if let Some(parent) = path.parent() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                tracing::warn!(error = %e, "Failed to create config dir for hosts.toml");
                return HostRegistry::default();
            }
        }
        if let Err(e) = std::fs::write(&path, SEED_HOSTS_TOML) {
            tracing::warn!(error = %e, "Failed to seed hosts.toml");
            return HostRegistry::default();
        }
        tracing::info!(path = %path.display(), "Seeded hosts.toml (no active hosts)");
        return HostRegistry::default();
    }

    match std::fs::read_to_string(&path) {
        Ok(contents) => match toml::from_str::<HostRegistry>(&contents) {
            Ok(reg) => reg,
            Err(e) => {
                tracing::warn!(error = %e, "Failed to parse hosts.toml; no remote hosts");
                HostRegistry::default()
            }
        },
        Err(e) => {
            tracing::warn!(error = %e, "Failed to read hosts.toml; no remote hosts");
            HostRegistry::default()
        }
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
