//! Command launch paths for tmux and psmux. Protocol behavior is selected by
//! the backend, while this module owns local, SSH, and WSL process execution.

use crate::shell::{posix_quote, ssh_command, wsl_command};
use std::process::Command;

pub const DEFAULT_MUX: &str = if cfg!(windows) { "psmux" } else { "tmux" };

#[derive(Debug, Clone)]
pub enum LaunchPath {
    Local,
    Ssh {
        destination: String,
        ssh_opts: Vec<String>,
    },
    Wsl {
        distro: String,
    },
}

impl LaunchPath {
    fn command(&self, binary: &str, socket: &str, args: &[&str]) -> Command {
        let mut cmd = match self {
            Self::Local => {
                let mut cmd = Command::new(binary);
                cmd.arg("-L").arg(socket).args(args);
                cmd
            }
            Self::Ssh {
                destination,
                ssh_opts,
            } => Self::prefixed(ssh_command(destination, ssh_opts), binary, socket, args),
            Self::Wsl { distro } => Self::prefixed(wsl_command(distro), binary, socket, args),
        };
        strip_mux_nesting_env(&mut cmd);
        cmd
    }
    fn prefixed(mut cmd: Command, binary: &str, socket: &str, args: &[&str]) -> Command {
        // These tokens pass through the host's POSIX command parser.
        for token in std::iter::once(binary)
            .chain(["-L", socket])
            .chain(args.iter().copied())
        {
            cmd.arg(posix_quote(token));
        }
        cmd
    }
    fn is_remote(&self) -> bool {
        !matches!(self, Self::Local)
    }
    fn is_ssh(&self) -> bool {
        matches!(self, Self::Ssh { .. })
    }
    fn launcher(&self, binary: &'static str) -> &'static str {
        match self {
            Self::Local => binary,
            Self::Ssh { .. } => "ssh",
            Self::Wsl { .. } => "wsl.exe",
        }
    }
}

/// The operations for which tmux and psmux have different wire behavior.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MuxProtocol {
    Tmux,
    Psmux,
}

impl MuxProtocol {
    pub fn sends_implicit_attach_response(self) -> bool {
        self == Self::Tmux
    }
    pub fn validates_response_blocks(self) -> bool {
        self == Self::Tmux
    }
    pub fn supports_command_lists(self) -> bool {
        self == Self::Tmux
    }
    pub fn supports_window_options(self) -> bool {
        self == Self::Tmux
    }
    pub fn supports_size_reports(self) -> bool {
        self == Self::Tmux
    }
    pub fn supports_snapshots(self) -> bool {
        self == Self::Tmux
    }
    pub fn supports_subscriptions(self) -> bool {
        self == Self::Tmux
    }
    pub fn uses_posix_login_shell(self) -> bool {
        self == Self::Tmux
    }
    pub fn needs_psmux_encoding(self) -> bool {
        self == Self::Psmux
    }
}

/// Only process execution agrees across mux implementations.
pub trait MuxTransport: Clone + Send + Sync + 'static {
    const BINARY: &'static str;
    const PROTOCOL: MuxProtocol;
    fn local() -> Self;
    fn from_host(host: &crate::session::HostDef) -> Self;
    fn path(&self) -> LaunchPath;
    fn mux_command(&self, socket: &str, args: &[&str]) -> Command {
        self.path().command(Self::BINARY, socket, args)
    }
    fn is_remote(&self) -> bool {
        self.path().is_remote()
    }
    fn is_ssh(&self) -> bool {
        self.path().is_ssh()
    }
    fn launcher(&self) -> &'static str {
        self.path().launcher(Self::BINARY)
    }
}

/// A tmux command path. WSL always runs Linux tmux.
#[derive(Debug, Clone)]
pub enum TmuxTransport {
    Local,
    Ssh {
        destination: String,
        ssh_opts: Vec<String>,
    },
    Wsl {
        distro: String,
    },
}

impl MuxTransport for TmuxTransport {
    const BINARY: &'static str = "tmux";
    const PROTOCOL: MuxProtocol = MuxProtocol::Tmux;
    fn local() -> Self {
        Self::Local
    }
    fn from_host(host: &crate::session::HostDef) -> Self {
        if host.is_wsl() {
            Self::Wsl {
                distro: host.distro_name(),
            }
        } else {
            Self::Ssh {
                destination: host.destination.clone(),
                ssh_opts: host.ssh_opts.clone(),
            }
        }
    }
    fn path(&self) -> LaunchPath {
        match self {
            Self::Local => LaunchPath::Local,
            Self::Ssh {
                destination,
                ssh_opts,
            } => LaunchPath::Ssh {
                destination: destination.clone(),
                ssh_opts: ssh_opts.clone(),
            },
            Self::Wsl { distro } => LaunchPath::Wsl {
                distro: distro.clone(),
            },
        }
    }
}
impl TmuxTransport {
    pub fn tmux_command(&self, socket: &str, args: &[&str]) -> Command {
        self.mux_command(socket, args)
    }
}

/// A native psmux path, local on Windows or reached through SSH.
#[derive(Debug, Clone)]
pub struct PsmuxTransport {
    path: LaunchPath,
}

impl MuxTransport for PsmuxTransport {
    const BINARY: &'static str = "psmux";
    const PROTOCOL: MuxProtocol = MuxProtocol::Psmux;
    fn local() -> Self {
        Self {
            path: LaunchPath::Local,
        }
    }
    fn from_host(host: &crate::session::HostDef) -> Self {
        Self {
            path: LaunchPath::Ssh {
                destination: host.destination.clone(),
                ssh_opts: host.ssh_opts.clone(),
            },
        }
    }
    fn path(&self) -> LaunchPath {
        self.path.clone()
    }
}

// An outer pane's environment can redirect a command away from this socket.
const MUX_NESTING_ENV: &[&str] = &[
    "TMUX",
    "TMUX_PANE",
    "PSMUX",
    "PSMUX_PANE",
    "PSMUX_SESSION",
    "PSMUX_TARGET_SESSION",
];
pub(crate) fn strip_mux_nesting_env(cmd: &mut Command) {
    for var in MUX_NESTING_ENV {
        cmd.env_remove(var);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn transports_fix_the_binary_independently_of_the_host_route() {
        let host = crate::session::HostDef {
            destination: "host".into(),
            ..Default::default()
        };
        for (command, expected) in [
            (
                TmuxTransport::from_host(&host).mux_command("thurbox", &["-V"]),
                "tmux",
            ),
            (
                PsmuxTransport::from_host(&host).mux_command("thurbox", &["-V"]),
                "psmux",
            ),
        ] {
            let args: Vec<_> = command
                .get_args()
                .map(|a| a.to_string_lossy().into_owned())
                .collect();
            assert!(args.contains(&expected.to_string()), "{args:?}");
        }
    }
}
