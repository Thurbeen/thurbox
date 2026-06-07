//! Transport seam for the tmux backend.
//!
//! The tmux control-mode protocol is identical whether tmux runs on the local
//! machine or on a remote host reached over SSH (see [`crate::agent::control_mode`]).
//! The *only* thing that differs is how the `tmux` process is launched: a bare
//! `Command::new("tmux")` locally, or `ssh <dest> tmux …` remotely.
//!
//! [`TmuxTransport`] captures exactly that difference and nothing else. It builds
//! [`Command`]s; it never touches I/O, threading, or the protocol.

use std::process::Command;

use crate::shell::{posix_quote, ssh_command};

/// How to launch `tmux` for a backend: directly, or wrapped in `ssh`.
#[derive(Debug, Clone)]
pub enum TmuxTransport {
    /// Run tmux on the local machine.
    Local,
    /// Run tmux on a remote host over SSH. `destination` is an ssh target
    /// (resolved via the user's `~/.ssh/config`); `ssh_opts` are extra flags
    /// (e.g. `-o ControlMaster=auto`) inserted before the destination.
    Ssh {
        destination: String,
        ssh_opts: Vec<String>,
    },
}

impl TmuxTransport {
    /// Build a [`Command`] running `tmux -L <socket> <args…>`, wrapped in `ssh`
    /// for the remote variant.
    ///
    /// For the SSH variant the remote command tokens are re-split by the remote
    /// login shell, so each token is shell-escaped to survive intact. Simple
    /// tokens (`tmux`, `-L`, the socket name) pass through unquoted.
    pub fn tmux_command(&self, socket: &str, args: &[&str]) -> Command {
        match self {
            TmuxTransport::Local => {
                let mut cmd = Command::new("tmux");
                cmd.arg("-L").arg(socket).args(args);
                cmd
            }
            TmuxTransport::Ssh {
                destination,
                ssh_opts,
            } => {
                let mut cmd = ssh_command(destination, ssh_opts);
                cmd.arg(posix_quote("tmux"));
                cmd.arg(posix_quote("-L"));
                cmd.arg(posix_quote(socket));
                for a in args {
                    cmd.arg(posix_quote(a));
                }
                cmd
            }
        }
    }

    /// Whether this transport reaches tmux over SSH.
    pub fn is_remote(&self) -> bool {
        matches!(self, TmuxTransport::Ssh { .. })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn program_and_args(cmd: &Command) -> (String, Vec<String>) {
        let prog = cmd.get_program().to_string_lossy().into_owned();
        let args = cmd
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        (prog, args)
    }

    #[test]
    fn local_builds_bare_tmux() {
        let t = TmuxTransport::Local;
        let cmd = t.tmux_command("thurbox", &["has-session", "-t", "thurbox"]);
        let (prog, args) = program_and_args(&cmd);
        assert_eq!(prog, "tmux");
        assert_eq!(args, ["-L", "thurbox", "has-session", "-t", "thurbox"]);
    }

    #[test]
    fn ssh_wraps_tmux_with_opts_and_destination() {
        let t = TmuxTransport::Ssh {
            destination: "me@devbox".into(),
            ssh_opts: vec!["-o".into(), "ControlMaster=auto".into()],
        };
        let cmd = t.tmux_command("thurbox", &["has-session", "-t", "thurbox"]);
        let (prog, args) = program_and_args(&cmd);
        assert_eq!(prog, "ssh");
        assert_eq!(
            args,
            [
                "-o",
                "ControlMaster=auto",
                "me@devbox",
                "tmux",
                "-L",
                "thurbox",
                "has-session",
                "-t",
                "thurbox",
            ]
        );
    }

    #[test]
    fn is_remote_reflects_variant() {
        assert!(!TmuxTransport::Local.is_remote());
        assert!(TmuxTransport::Ssh {
            destination: "h".into(),
            ssh_opts: vec![],
        }
        .is_remote());
    }
}
