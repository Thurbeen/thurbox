//! How a tmux-compatible multiplexer is launched.
//!
//! The control-mode protocol is identical whether the server runs on this
//! machine or on a host reached over SSH or in a WSL distro (see
//! [`crate::backend::tmux_compat::control_mode`]). What differs is how the
//! binary is started: directly, or behind a [`HostLauncher`].
//!
//! [`TmuxTransport`] is two independent halves: the **launcher** that reaches
//! the host and the **binary** run there, which the adapter using it names. It
//! builds [`Command`]s; it never touches I/O, threading, or the protocol, and it
//! knows neither the host's OS nor which multiplexer the binary is.

use std::process::Command;

use crate::shell::HostLauncher;

/// How to launch the multiplexer for a backend: which launcher reaches its
/// machine (none, for this one) and which binary runs there.
#[derive(Debug, Clone)]
pub struct TmuxTransport {
    /// `None` runs the multiplexer on this machine; otherwise `ssh …` or
    /// `wsl.exe …` reaches the host it runs on. `wsl.exe` forwards the
    /// whitespace-free tokens used here to the in-distro shell like `ssh`
    /// does (see [`crate::shell::wsl_command`]), so the same control-mode
    /// protocol and POSIX quoting apply to both.
    launcher: Option<HostLauncher>,
    /// The multiplexer binary, as the adapter using this names it.
    mux: String,
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
    /// `mux` on this machine, run directly.
    pub fn local(mux: impl Into<String>) -> Self {
        Self {
            launcher: None,
            mux: mux.into(),
        }
    }

    /// `mux` on the host `launcher` reaches.
    pub fn remote(launcher: HostLauncher, mux: impl Into<String>) -> Self {
        Self {
            launcher: Some(launcher),
            mux: mux.into(),
        }
    }

    /// Build a [`Command`] running `<mux> -L <socket> <args…>`, behind the
    /// launcher for a remote host ([`crate::shell::launch`], which adds
    /// nothing of its own: `-L` is the tmux grammar's, so it is written here).
    ///
    /// Nesting env vars are stripped (see `strip_mux_nesting_env`) so the
    /// command targets thurbox's own server even when thurbox runs inside a pane.
    pub fn tmux_command(&self, socket: &str, args: &[&str]) -> Command {
        let argv: Vec<&str> = ["-L", socket]
            .into_iter()
            .chain(args.iter().copied())
            .collect();
        let mut cmd = crate::shell::launch(self.launcher.as_ref(), &self.mux, &argv);
        strip_mux_nesting_env(&mut cmd);
        cmd
    }

    /// Whether this transport reaches the multiplexer through a launcher
    /// (SSH or WSL) rather than running it directly on the local machine.
    pub fn is_remote(&self) -> bool {
        self.launcher.is_some()
    }

    /// The multiplexer binary this transport runs, wherever it runs it.
    pub fn mux(&self) -> &str {
        &self.mux
    }

    /// The program this transport actually executes on **this** machine.
    ///
    /// The multiplexer itself locally; the launcher (`ssh`, `wsl.exe`) for a
    /// remote backend, whose own multiplexer runs on the host and cannot be
    /// what failed to start here. Read by [`Self::launch_failure`], which has
    /// to name the binary that is missing rather than the one it was on the
    /// way to.
    pub fn launcher(&self) -> &str {
        match &self.launcher {
            Some(launcher) => launcher.program(),
            None => &self.mux,
        }
    }

    /// What a failure to launch through this transport means, in words a user
    /// can act on — see [`crate::agent::preflight::launch_failure`]. A remote
    /// transport's missing binary is its launcher; a local one's is the
    /// multiplexer.
    pub fn launch_failure(&self, context: &'static str, err: std::io::Error) -> anyhow::Error {
        let launcher = self.is_remote().then(|| self.launcher());
        crate::agent::preflight::launch_failure(launcher, &self.mux, context, err)
    }

    /// Whether the multiplexer is reached over `ssh`, and so whether ssh's own
    /// exit conventions apply to a failed command.
    ///
    /// Read by the teardown's listing to tell "ssh could not deliver the
    /// question" (exit 255, ssh's documented own-error code) from "the
    /// multiplexer answered", which is the one distinction that decides
    /// whether an empty result means there is nothing to kill. A WSL distro is
    /// deliberately not included: `wsl.exe` has no such convention, and
    /// claiming it does would be the guess this exists to avoid.
    pub fn is_ssh(&self) -> bool {
        matches!(self.launcher, Some(HostLauncher::Ssh { .. }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ssh(destination: &str, ssh_opts: Vec<String>, mux: &str) -> TmuxTransport {
        TmuxTransport::remote(
            HostLauncher::Ssh {
                destination: destination.into(),
                ssh_opts,
            },
            mux,
        )
    }

    fn local() -> TmuxTransport {
        TmuxTransport::local("tmux")
    }

    fn wsl(distro: &str) -> TmuxTransport {
        TmuxTransport::remote(
            HostLauncher::Wsl {
                distro: distro.into(),
            },
            "tmux",
        )
    }

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
        let t = local();
        let cmd = t.tmux_command("thurbox", &["has-session", "-t", "thurbox"]);
        let (prog, args) = program_and_args(&cmd);
        assert_eq!(prog, "tmux");
        assert_eq!(args, ["-L", "thurbox", "has-session", "-t", "thurbox"]);
    }

    #[test]
    fn ssh_wraps_mux_with_opts_and_destination() {
        let t = ssh(
            "me@devbox",
            vec!["-o".into(), "ControlMaster=auto".into()],
            "tmux",
        );
        let cmd = t.tmux_command("thurbox", &["has-session", "-t", "thurbox"]);
        let (prog, args) = program_and_args(&cmd);
        assert_eq!(prog, "ssh");
        // User opts, then the always-appended set (fail-fast hardening plus
        // multiplexing when the machine has an `~/.ssh` —
        // crate::shell::ssh_appended_opts), then the destination + remote cmd.
        let mut expected: Vec<String> = vec!["-o".into(), "ControlMaster=auto".into()];
        expected.extend(
            crate::shell::ssh_appended_opts()
                .iter()
                .map(|s| s.to_string()),
        );
        expected.extend(
            [
                "me@devbox",
                "tmux",
                "-L",
                "thurbox",
                "has-session",
                "-t",
                "thurbox",
            ]
            .iter()
            .map(|s| s.to_string()),
        );
        assert_eq!(args, expected);
    }

    #[test]
    fn ssh_honors_custom_multiplexer() {
        let t = ssh("me@winbox", vec![], "psmux");
        let cmd = t.tmux_command("thurbox", &["has-session"]);
        let (prog, args) = program_and_args(&cmd);
        assert_eq!(prog, "ssh");
        let mut expected: Vec<String> = crate::shell::ssh_appended_opts()
            .iter()
            .map(|s| s.to_string())
            .collect();
        expected.extend(
            ["me@winbox", "psmux", "-L", "thurbox", "has-session"]
                .iter()
                .map(|s| s.to_string()),
        );
        assert_eq!(args, expected);
    }

    #[test]
    fn wsl_wraps_mux_with_distro() {
        let t = wsl("Ubuntu");
        let cmd = t.tmux_command("thurbox", &["has-session", "-t", "thurbox"]);
        let (prog, args) = program_and_args(&cmd);
        assert_eq!(prog, "wsl.exe");
        // A Unix caller passes `--cd /` (see `shell::wsl_command`) so wsl.exe
        // doesn't inherit a caller cwd missing from — or mangled into — the
        // target distro.
        #[cfg(unix)]
        let prefix: &[&str] = &["-d", "Ubuntu", "--cd", "/"];
        #[cfg(not(unix))]
        let prefix: &[&str] = &["-d", "Ubuntu"];
        let expected: Vec<&str> = prefix
            .iter()
            .copied()
            .chain(["tmux", "-L", "thurbox", "has-session", "-t", "thurbox"])
            .collect();
        assert_eq!(args, expected);
    }

    #[test]
    fn tmux_command_strips_nesting_env() {
        let cmd = local().tmux_command("thurbox", &["has-session"]);
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
    fn a_transport_runs_the_binary_its_adapter_names() {
        assert_eq!(TmuxTransport::local("psmux").mux(), "psmux");
        assert_eq!(ssh("h", vec![], "psmux").mux(), "psmux");
        assert_eq!(wsl("Ubuntu").mux(), "tmux");
    }

    #[test]
    fn is_remote_reflects_variant() {
        assert!(!local().is_remote());
        assert!(ssh("h", vec![], "tmux").is_remote());
        assert!(wsl("Ubuntu").is_remote());
    }
}
