//! How a multiplexer command reaches its host: locally, over SSH, or inside a
//! WSL distro. Only process execution is shared here; each concrete transport
//! (`TmuxTransport`, `PsmuxTransport`) lives beside its backend, which also
//! owns its protocol (`crate::agent::mux::MuxDialect`).

use crate::shell::{posix_quote, ssh_command, wsl_command};
use std::process::Command;

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

/// Only process execution agrees across mux implementations.
pub trait MuxTransport: Clone + Send + Sync + 'static {
    const BINARY: &'static str;
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
