//! Remote-host definitions — pure data describing SSH targets thurbox can run
//! sessions on.
//!
//! Loaded from `~/.config/thurbox/hosts.toml` by
//! [`crate::agent::host_config`]. Kept here in `session` (the dependency sink)
//! so both `agent` (which builds the SSH tmux backend) and `git` (which runs
//! `git` over SSH for remote worktrees) can depend on the same type without
//! crossing the module-isolation rules.

use serde::{Deserialize, Serialize};

/// The backend-name prefix for SSH hosts. A host named `devbox` is registered
/// (and persisted in `backend_type`) as `ssh:devbox`.
pub const SSH_BACKEND_PREFIX: &str = "ssh:";

/// Whether a backend name refers to a remote SSH host (`ssh:<name>`).
pub fn is_ssh_backend(backend_name: &str) -> bool {
    backend_name.starts_with(SSH_BACKEND_PREFIX)
}

/// A single remote host reachable over SSH.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostDef {
    /// Short, unique name. The backend is registered as `ssh:<name>`.
    pub name: String,
    /// SSH destination (e.g. `me@devbox`), resolved via the user's
    /// `~/.ssh/config`.
    pub destination: String,
    /// Optional override for the remote `tmux -L` socket name. Defaults to the
    /// same socket thurbox uses locally.
    #[serde(default)]
    pub socket: Option<String>,
    /// Optional override for the remote tmux session name.
    #[serde(default)]
    pub session: Option<String>,
    /// Extra `ssh` flags inserted before the destination (e.g.
    /// `["-o", "ControlMaster=auto"]`).
    #[serde(default)]
    pub ssh_opts: Vec<String>,
    /// Optional absolute remote directory under which git worktrees are
    /// created. When unset, the remote `$HOME/.local/share/thurbox/worktrees`
    /// is resolved at spawn time.
    #[serde(default)]
    pub worktrees_dir: Option<String>,
}

impl HostDef {
    /// The backend name this host registers under: `ssh:<name>`.
    pub fn backend_name(&self) -> String {
        format!("{SSH_BACKEND_PREFIX}{}", self.name)
    }
}

/// All configured remote hosts, in declaration order.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostRegistry {
    #[serde(default)]
    pub hosts: Vec<HostDef>,
}

impl HostRegistry {
    /// Look up a host by its bare name (not the `ssh:` backend name).
    pub fn get(&self, name: &str) -> Option<&HostDef> {
        self.hosts.iter().find(|h| h.name == name)
    }

    /// Look up a host by its `ssh:<name>` backend name.
    pub fn get_by_backend(&self, backend_name: &str) -> Option<&HostDef> {
        let bare = backend_name.strip_prefix(SSH_BACKEND_PREFIX)?;
        self.get(bare)
    }

    /// All host names in declaration order.
    pub fn names(&self) -> Vec<&str> {
        self.hosts.iter().map(|h| h.name.as_str()).collect()
    }

    /// Whether any remote hosts are configured.
    pub fn is_empty(&self) -> bool {
        self.hosts.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backend_name_prefixes_with_ssh() {
        let h = HostDef {
            name: "devbox".into(),
            destination: "me@devbox".into(),
            socket: None,
            session: None,
            ssh_opts: vec![],
            worktrees_dir: None,
        };
        assert_eq!(h.backend_name(), "ssh:devbox");
    }

    #[test]
    fn registry_lookup_by_name_and_backend() {
        let reg = HostRegistry {
            hosts: vec![HostDef {
                name: "devbox".into(),
                destination: "me@devbox".into(),
                socket: None,
                session: None,
                ssh_opts: vec![],
                worktrees_dir: None,
            }],
        };
        assert_eq!(reg.get("devbox").unwrap().destination, "me@devbox");
        assert_eq!(reg.get_by_backend("ssh:devbox").unwrap().name, "devbox");
        assert!(reg.get_by_backend("devbox").is_none());
        assert!(reg.get_by_backend("local-tmux").is_none());
    }

    #[test]
    fn parses_minimal_and_full_toml() {
        let toml = r#"
[[hosts]]
name = "minimal"
destination = "host1"

[[hosts]]
name = "full"
destination = "me@host2"
socket = "tb2"
session = "tb2"
ssh_opts = ["-o", "ControlMaster=auto"]
worktrees_dir = "/home/me/wt"
"#;
        let reg: HostRegistry = toml::from_str(toml).unwrap();
        assert_eq!(reg.hosts.len(), 2);
        let minimal = reg.get("minimal").unwrap();
        assert_eq!(minimal.destination, "host1");
        assert!(minimal.socket.is_none());
        assert!(minimal.ssh_opts.is_empty());
        let full = reg.get("full").unwrap();
        assert_eq!(full.socket.as_deref(), Some("tb2"));
        assert_eq!(full.ssh_opts, ["-o", "ControlMaster=auto"]);
        assert_eq!(full.worktrees_dir.as_deref(), Some("/home/me/wt"));
    }
}
