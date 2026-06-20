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

/// The local multiplexer binary: `psmux` on Windows (a native, drop-in tmux
/// replacement with an identical control-mode wire protocol), `tmux` elsewhere.
/// psmux also installs `tmux`/`pmux` aliases, but `psmux` is the canonical name.
pub const DEFAULT_MUX: &str = if cfg!(windows) { "psmux" } else { "tmux" };

/// How to launch the multiplexer for a backend: directly, or wrapped in `ssh`.
#[derive(Debug, Clone)]
pub enum TmuxTransport {
    /// Run the multiplexer ([`DEFAULT_MUX`]) on the local machine.
    Local,
    /// Run the multiplexer on a remote host over SSH. `destination` is an ssh
    /// target (resolved via the user's `~/.ssh/config`); `ssh_opts` are extra
    /// flags (e.g. `-o ControlMaster=auto`) inserted before the destination;
    /// `mux` is the remote multiplexer binary (defaults to `tmux`, but a host in
    /// `hosts.toml` can set `multiplexer = "psmux"` for a Windows target).
    Ssh {
        destination: String,
        ssh_opts: Vec<String>,
        mux: String,
    },
}

/// Environment variables a tmux/psmux server reads to resolve a *nested*
/// client's default target. If thurbox is itself launched inside a tmux/psmux
/// pane, these leak into the multiplexer subcommands it spawns and make a bare
/// `-t <session>` resolve against the *outer* session instead of the thurbox
/// socket — on psmux this surfaces as `set-option -t thurbox` failing with
/// `no server running on 'thurbox__thurbox'` (psmux concatenates
/// `PSMUX_TARGET_SESSION = <socket>__<session>`). Stripping them makes thurbox's
/// explicit `-L <socket> -t <session>` always target its own server, whether the
/// host OS is Windows (psmux) or Unix (thurbox launched from inside tmux).
const MUX_NESTING_ENV: &[&str] = &[
    "TMUX",
    "TMUX_PANE",
    "PSMUX",
    "PSMUX_PANE",
    "PSMUX_SESSION",
    "PSMUX_TARGET_SESSION",
];

/// Remove the multiplexer-nesting env vars (see [`MUX_NESTING_ENV`]) from `cmd`
/// so a multiplexer subcommand never inherits an outer pane's target context.
pub(crate) fn strip_mux_nesting_env(cmd: &mut Command) {
    for var in MUX_NESTING_ENV {
        cmd.env_remove(var);
    }
}

impl TmuxTransport {
    /// Build a [`Command`] running `<mux> -L <socket> <args…>`, wrapped in `ssh`
    /// for the remote variant.
    ///
    /// For the SSH variant the remote command tokens are re-split by the remote
    /// login shell, so each token is shell-escaped to survive intact. Simple
    /// tokens (the binary name, `-L`, the socket name) pass through unquoted.
    ///
    /// Nesting env vars are stripped (see `strip_mux_nesting_env`) so the
    /// command targets thurbox's own server even when thurbox runs inside a pane.
    pub fn tmux_command(&self, socket: &str, args: &[&str]) -> Command {
        let mut cmd = match self {
            TmuxTransport::Local => {
                let mut cmd = Command::new(DEFAULT_MUX);
                cmd.arg("-L").arg(socket).args(args);
                cmd
            }
            TmuxTransport::Ssh {
                destination,
                ssh_opts,
                mux,
            } => {
                let mut cmd = ssh_command(destination, ssh_opts);
                cmd.arg(posix_quote(mux));
                cmd.arg(posix_quote("-L"));
                cmd.arg(posix_quote(socket));
                for a in args {
                    cmd.arg(posix_quote(a));
                }
                cmd
            }
        };
        strip_mux_nesting_env(&mut cmd);
        cmd
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
    fn local_builds_bare_mux() {
        let t = TmuxTransport::Local;
        let cmd = t.tmux_command("thurbox", &["has-session", "-t", "thurbox"]);
        let (prog, args) = program_and_args(&cmd);
        assert_eq!(prog, DEFAULT_MUX);
        assert_eq!(args, ["-L", "thurbox", "has-session", "-t", "thurbox"]);
    }

    #[test]
    fn ssh_wraps_mux_with_opts_and_destination() {
        let t = TmuxTransport::Ssh {
            destination: "me@devbox".into(),
            ssh_opts: vec!["-o".into(), "ControlMaster=auto".into()],
            mux: "tmux".into(),
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
    fn ssh_honors_custom_multiplexer() {
        let t = TmuxTransport::Ssh {
            destination: "me@winbox".into(),
            ssh_opts: vec![],
            mux: "psmux".into(),
        };
        let cmd = t.tmux_command("thurbox", &["has-session"]);
        let (prog, args) = program_and_args(&cmd);
        assert_eq!(prog, "ssh");
        assert_eq!(args, ["me@winbox", "psmux", "-L", "thurbox", "has-session"]);
    }

    #[test]
    fn tmux_command_strips_nesting_env() {
        let cmd = TmuxTransport::Local.tmux_command("thurbox", &["has-session"]);
        // Removed vars surface in get_envs() as (key, None).
        let removed: Vec<String> = cmd
            .get_envs()
            .filter(|(_, v)| v.is_none())
            .map(|(k, _)| k.to_string_lossy().into_owned())
            .collect();
        for var in MUX_NESTING_ENV {
            assert!(
                removed.contains(&var.to_string()),
                "expected nesting env `{var}` to be removed"
            );
        }
    }

    #[test]
    fn is_remote_reflects_variant() {
        assert!(!TmuxTransport::Local.is_remote());
        assert!(TmuxTransport::Ssh {
            destination: "h".into(),
            ssh_opts: vec![],
            mux: "tmux".into(),
        }
        .is_remote());
    }
}
