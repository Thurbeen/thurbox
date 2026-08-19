//! Remote-host definitions — pure data describing the off-local targets
//! thurbox can run sessions on: SSH machines and local WSL distros.
//!
//! Loaded from `~/.config/thurbox/hosts.toml` by
//! [`crate::agent::host_config`] (and, for WSL, auto-discovered there too).
//! Kept here in `session` (the dependency sink) so both `agent` (which builds
//! the tmux backend) and `git` (which runs `git` on the host for remote
//! worktrees) can depend on the same type without crossing the
//! module-isolation rules.
//!
//! A WSL distro is modeled as "SSH without the ssh": the only difference from
//! a remote host is the launch prefix (`wsl.exe -d <distro>` instead of
//! `ssh <dest>`). tmux, git, the agent, and the worktrees all run *inside* the
//! distro at native Linux paths, so everything downstream of the launcher is
//! identical to the SSH path.

use serde::{Deserialize, Serialize};

/// The backend-name prefix for SSH hosts. A host named `devbox` is registered
/// (and persisted in `backend_type`) as `ssh:devbox`.
pub const SSH_BACKEND_PREFIX: &str = "ssh:";

/// The backend-name prefix for WSL distros. A distro named `Ubuntu` is
/// registered (and persisted in `backend_type`) as `wsl:Ubuntu`.
pub const WSL_BACKEND_PREFIX: &str = "wsl:";

/// Whether a backend name refers to a remote SSH host (`ssh:<name>`).
pub fn is_ssh_backend(backend_name: &str) -> bool {
    backend_name.starts_with(SSH_BACKEND_PREFIX)
}

/// Whether a backend name refers to a WSL distro (`wsl:<distro>`).
pub fn is_wsl_backend(backend_name: &str) -> bool {
    backend_name.starts_with(WSL_BACKEND_PREFIX)
}

/// Whether a backend name refers to any off-local host (SSH or WSL) — i.e. one
/// that needs a launch prefix and runs git/worktrees somewhere other than the
/// local filesystem. Local backends (`""`, `tmux`, `local-tmux`) are not.
pub fn is_remote_backend(backend_name: &str) -> bool {
    is_ssh_backend(backend_name) || is_wsl_backend(backend_name)
}

/// How thurbox reaches a host: over SSH, or into a local WSL distro.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum HostKind {
    /// A remote machine reached with `ssh <destination>`.
    #[default]
    Ssh,
    /// A local Windows Subsystem for Linux distro reached with
    /// `wsl.exe -d <distro>`.
    Wsl,
}

/// A single off-local host: an SSH machine or a local WSL distro.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct HostDef {
    /// Short, unique name. The backend is registered as `ssh:<name>` or
    /// `wsl:<name>` depending on [`kind`](Self::kind).
    pub name: String,
    /// Transport kind: SSH (default) or WSL.
    #[serde(default)]
    pub kind: HostKind,
    /// SSH destination (e.g. `me@devbox`), resolved via the user's
    /// `~/.ssh/config`. Required for [`HostKind::Ssh`]; ignored for WSL.
    #[serde(default)]
    pub destination: String,
    /// WSL distro name (e.g. `Ubuntu`). Defaults to [`name`](Self::name) when
    /// unset. Ignored for [`HostKind::Ssh`].
    #[serde(default)]
    pub distro: Option<String>,
    /// Optional override for the host's `tmux -L` socket name. Defaults to the
    /// same socket thurbox uses locally.
    #[serde(default)]
    pub socket: Option<String>,
    /// Optional override for the host's tmux session name.
    #[serde(default)]
    pub session: Option<String>,
    /// Extra `ssh` flags inserted before the destination (e.g.
    /// `["-o", "ControlMaster=auto"]`). SSH-only.
    #[serde(default)]
    pub ssh_opts: Vec<String>,
    /// Optional absolute directory (inside the host / distro) under which git
    /// worktrees are created. When unset, the host's
    /// `$HOME/.local/share/thurbox/worktrees` is resolved at spawn time.
    #[serde(default)]
    pub worktrees_dir: Option<String>,
    /// Optional multiplexer binary on the host. Defaults to `tmux` (the WSL
    /// distro and a Unix SSH host both run `tmux`); set to `psmux` for a
    /// Windows SSH host (psmux speaks the same control-mode wire protocol).
    #[serde(default)]
    pub multiplexer: Option<String>,
}

impl HostDef {
    /// Construct an auto-discovered WSL host for `distro` with all defaults.
    pub fn wsl(distro: impl Into<String>) -> Self {
        let distro = distro.into();
        Self {
            name: distro.clone(),
            kind: HostKind::Wsl,
            distro: Some(distro),
            ..Self::default()
        }
    }

    /// Whether this host is a WSL distro.
    pub fn is_wsl(&self) -> bool {
        self.kind == HostKind::Wsl
    }

    /// The WSL distro name (the explicit `distro` field, else the host `name`).
    /// Only meaningful for [`HostKind::Wsl`].
    pub fn distro_name(&self) -> String {
        self.distro.clone().unwrap_or_else(|| self.name.clone())
    }

    /// The backend name this host registers under: `ssh:<name>` or
    /// `wsl:<name>`.
    pub fn backend_name(&self) -> String {
        let prefix = match self.kind {
            HostKind::Ssh => SSH_BACKEND_PREFIX,
            HostKind::Wsl => WSL_BACKEND_PREFIX,
        };
        format!("{prefix}{}", self.name)
    }

    /// A short detail string for the host picker (the SSH destination, or
    /// `WSL` for a distro).
    pub fn picker_detail(&self) -> String {
        match self.kind {
            HostKind::Ssh => self.destination.clone(),
            HostKind::Wsl => "WSL".to_string(),
        }
    }

    /// The host's multiplexer binary (`tmux` unless overridden).
    pub fn mux(&self) -> String {
        self.multiplexer
            .clone()
            .unwrap_or_else(|| "tmux".to_string())
    }

    /// Whether this host is **native Windows** — no POSIX shell, `\` paths,
    /// PowerShell rather than `sh`.
    ///
    /// The multiplexer is the proxy for the platform: `psmux` is a
    /// native-Windows tmux clone (ConPTY, no WSL), so choosing it *is* the
    /// declaration that the host is Windows. There is no separate platform
    /// field to disagree with, and a WSL distro is Linux inside — it runs
    /// `tmux` — so it is correctly not Windows here.
    ///
    /// The name is spelled out rather than shared with `agent::transport`'s
    /// `DEFAULT_MUX` / `TmuxTransport::uses_psmux` (which asks the *protocol*
    /// question, not the platform one): `session` is the leaf module and may
    /// reference nothing, so the two must be kept in step by hand.
    pub fn is_windows(&self) -> bool {
        self.mux() == "psmux"
    }
}

/// All configured remote hosts, in declaration order.
///
/// Unknown fields are tolerated but reported: the loader names every
/// unrecognized key in a startup warning.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostRegistry {
    /// Config-format version, for future migrations. Currently `1`.
    #[serde(default)]
    pub config_version: Option<u32>,
    #[serde(default)]
    pub hosts: Vec<HostDef>,
}

impl HostRegistry {
    /// Look up a host by its bare name (not the `ssh:` backend name).
    pub fn get(&self, name: &str) -> Option<&HostDef> {
        self.hosts.iter().find(|h| h.name == name)
    }

    /// Look up a host by its `ssh:<name>` or `wsl:<name>` backend name.
    ///
    /// The prefix selects *a* backend spelling but is **not** checked against
    /// the host's own [`kind`](HostDef::kind): `ssh:ubuntu` finds a host named
    /// `ubuntu` even if it is a WSL distro. That is deliberate — a name is
    /// unique across kinds (`host_config::load_all` dedupes by it), and the only
    /// way the two disagree is a persisted `backend_type` written before the
    /// host's kind was changed, where resolving to the host that now carries
    /// that name is what lets the session re-adopt instead of going
    /// unreachable.
    pub fn get_by_backend(&self, backend_name: &str) -> Option<&HostDef> {
        let bare = backend_name
            .strip_prefix(SSH_BACKEND_PREFIX)
            .or_else(|| backend_name.strip_prefix(WSL_BACKEND_PREFIX))?;
        self.get(bare)
    }

    /// Look up a host by **either** spelling: the `ssh:`/`wsl:` backend name or
    /// the bare name.
    ///
    /// The two spellings both circulate as "the host" and which one a caller
    /// holds depends on where it came from — a session row and the interface's
    /// host picker carry the backend name, `hosts.toml` and `--host` carry the
    /// bare one. Resolving only one of them is the mistake this exists to stop:
    /// the new-session flow handed `spawn` a backend name and every remote
    /// creation failed with "Unknown host 'ssh:devbox'".
    pub fn resolve(&self, key: &str) -> Option<&HostDef> {
        self.get_by_backend(key).or_else(|| self.get(key))
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

    fn registry() -> HostRegistry {
        HostRegistry {
            config_version: None,
            hosts: vec![
                HostDef {
                    name: "devbox".into(),
                    destination: "me@devbox".into(),
                    ..Default::default()
                },
                HostDef::wsl("Ubuntu"),
                HostDef {
                    name: "winbox".into(),
                    destination: "me@winbox".into(),
                    multiplexer: Some("psmux".into()),
                    ..Default::default()
                },
            ],
        }
    }

    #[test]
    fn resolve_accepts_both_spellings_of_a_host() {
        // Which spelling a caller holds depends on where it came from, and
        // resolving only one of them refused every session the new-session flow
        // tried to create on a host ("Unknown host 'ssh:devbox'").
        let hosts = registry();
        for key in ["devbox", "ssh:devbox"] {
            assert_eq!(
                hosts.resolve(key).map(|h| h.name.as_str()),
                Some("devbox"),
                "{key} should resolve"
            );
        }
        for key in ["Ubuntu", "wsl:Ubuntu"] {
            assert_eq!(
                hosts.resolve(key).map(|h| h.name.as_str()),
                Some("Ubuntu"),
                "{key} should resolve"
            );
        }
    }

    #[test]
    fn resolve_does_not_invent_a_host() {
        let hosts = registry();
        for key in ["", "nope", "ssh:nope", "wsl:nope", "ssh:", "devbox2"] {
            assert!(hosts.resolve(key).is_none(), "{key} must not resolve");
        }
    }

    #[test]
    fn the_backend_prefix_is_not_checked_against_the_hosts_kind() {
        // A name is unique across kinds, so the prefix carries no information
        // the name does not — and the one case where they disagree is a
        // persisted `backend_type` written before the host's kind changed, where
        // matching on the name is what lets the session re-adopt rather than go
        // unreachable. Asserted so the leniency is a decision, not an accident.
        let hosts = registry();
        assert_eq!(
            hosts.resolve("ssh:Ubuntu").map(|h| h.kind),
            Some(HostKind::Wsl)
        );
        assert_eq!(
            hosts.resolve("wsl:devbox").map(|h| h.kind),
            Some(HostKind::Ssh)
        );
    }

    #[test]
    fn is_windows_is_declared_by_the_multiplexer_alone() {
        let hosts = registry();
        assert!(hosts.resolve("winbox").expect("winbox").is_windows());
        // Every other shape is POSIX: the default, an explicit `tmux`, and a
        // WSL distro (Linux inside, whatever machine hosts it).
        assert!(!hosts.resolve("devbox").expect("devbox").is_windows());
        assert!(!hosts.resolve("Ubuntu").expect("Ubuntu").is_windows());
        let explicit_tmux = HostDef {
            name: "box".into(),
            multiplexer: Some("tmux".into()),
            ..Default::default()
        };
        assert!(!explicit_tmux.is_windows());
    }

    #[test]
    fn backend_name_prefixes_with_ssh() {
        let h = HostDef {
            name: "devbox".into(),
            destination: "me@devbox".into(),
            ..Default::default()
        };
        assert_eq!(h.backend_name(), "ssh:devbox");
    }

    #[test]
    fn wsl_constructor_and_backend_name() {
        let h = HostDef::wsl("Ubuntu");
        assert!(h.is_wsl());
        assert_eq!(h.kind, HostKind::Wsl);
        assert_eq!(h.distro_name(), "Ubuntu");
        assert_eq!(h.backend_name(), "wsl:Ubuntu");
        assert_eq!(h.picker_detail(), "WSL");
        // WSL distros run `tmux` inside the distro, same as a Unix SSH host.
        assert_eq!(h.mux(), "tmux");

        // An SSH host (the default kind) keeps the ssh prefix and is not WSL.
        let ssh = HostDef {
            name: "devbox".into(),
            destination: "me@devbox".into(),
            ..Default::default()
        };
        assert!(!ssh.is_wsl());
        assert_eq!(ssh.picker_detail(), "me@devbox");
    }

    #[test]
    fn distro_name_falls_back_to_host_name() {
        let h = HostDef {
            name: "work".into(),
            kind: HostKind::Wsl,
            distro: None,
            ..Default::default()
        };
        assert_eq!(h.distro_name(), "work");
    }

    #[test]
    fn backend_predicates_classify_prefixes() {
        assert!(is_ssh_backend("ssh:devbox"));
        assert!(!is_ssh_backend("wsl:Ubuntu"));
        assert!(is_wsl_backend("wsl:Ubuntu"));
        assert!(!is_wsl_backend("ssh:devbox"));
        assert!(is_remote_backend("ssh:devbox"));
        assert!(is_remote_backend("wsl:Ubuntu"));
        assert!(!is_remote_backend("local-tmux"));
        assert!(!is_remote_backend(""));
    }

    #[test]
    fn registry_lookup_by_name_and_backend() {
        let reg = HostRegistry {
            config_version: None,
            hosts: vec![
                HostDef {
                    name: "devbox".into(),
                    destination: "me@devbox".into(),
                    ..Default::default()
                },
                HostDef::wsl("Ubuntu"),
            ],
        };
        assert_eq!(reg.get("devbox").unwrap().destination, "me@devbox");
        assert_eq!(reg.get_by_backend("ssh:devbox").unwrap().name, "devbox");
        assert_eq!(reg.get_by_backend("wsl:Ubuntu").unwrap().name, "Ubuntu");
        assert!(reg.get_by_backend("devbox").is_none());
        assert!(reg.get_by_backend("local-tmux").is_none());
    }

    #[test]
    fn parses_wsl_host_from_toml() {
        let toml = r#"
[[hosts]]
name = "Ubuntu"
kind = "wsl"

[[hosts]]
name = "custom"
kind = "wsl"
distro = "Debian"
worktrees_dir = "/home/me/wt"
"#;
        let reg: HostRegistry = toml::from_str(toml).unwrap();
        let u = reg.get("Ubuntu").unwrap();
        assert!(u.is_wsl());
        assert_eq!(u.distro_name(), "Ubuntu");
        assert_eq!(u.backend_name(), "wsl:Ubuntu");
        let c = reg.get("custom").unwrap();
        assert_eq!(c.distro_name(), "Debian");
        assert_eq!(c.worktrees_dir.as_deref(), Some("/home/me/wt"));
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
