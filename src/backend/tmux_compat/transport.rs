//! Transport seam for the tmux backend.
//!
//! The tmux control-mode protocol is identical whether tmux runs on the local
//! machine or on a remote host reached over SSH (see [`crate::backend::tmux_compat::control_mode`]).
//! The *only* thing that differs is how the `tmux` process is launched: a bare
//! `Command::new("tmux")` locally, or `ssh <dest> tmux …` remotely.
//!
//! [`TmuxTransport`] is two independent halves: the **launcher** that reaches
//! the host ([`HostLauncher`], shared with every other remote command) and the
//! **multiplexer** binary run there. It builds [`Command`]s; it never touches
//! I/O, threading, or the protocol, and it knows nothing of the host's OS.

use std::process::Command;

use crate::shell::{posix_quote, HostLauncher};

/// The local multiplexer binary: the platform default's name — `psmux` on
/// Windows (a native, drop-in tmux replacement with an identical control-mode
/// wire protocol), `tmux` elsewhere. psmux also installs `tmux`/`pmux`
/// aliases, but `psmux` is the canonical name. Derived rather than restated,
/// so the binary run here and the route a local row is written under cannot
/// disagree.
pub const DEFAULT_MUX: &str = crate::session::Multiplexer::platform_default().name();

/// How to launch the multiplexer for a backend: which launcher reaches its
/// machine (none, for this one) and which multiplexer binary runs there.
#[derive(Debug, Clone)]
pub struct TmuxTransport {
    /// `None` runs the multiplexer on this machine; otherwise `ssh …` or
    /// `wsl.exe …` reaches the host it runs on. `wsl.exe` forwards the
    /// whitespace-free tokens used here to the in-distro shell like `ssh`
    /// does (see [`crate::shell::wsl_command`]), so the same control-mode
    /// protocol and POSIX quoting apply to both.
    launcher: Option<HostLauncher>,
    /// The multiplexer binary: [`DEFAULT_MUX`] locally, the route's
    /// multiplexer on a host.
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
    /// This machine's own multiplexer, run directly.
    pub fn local() -> Self {
        Self {
            launcher: None,
            mux: DEFAULT_MUX.to_string(),
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
    /// launcher for a remote host.
    ///
    /// Through a launcher the command tokens are re-split by the host's login
    /// shell, so each token is shell-escaped to survive intact. Simple tokens
    /// (the binary name, `-L`, the socket name) pass through unquoted. `-L` is
    /// the multiplexer's flag and is added here, never by the launcher.
    ///
    /// Nesting env vars are stripped (see `strip_mux_nesting_env`) so the
    /// command targets thurbox's own server even when thurbox runs inside a pane.
    pub fn tmux_command(&self, socket: &str, args: &[&str]) -> Command {
        let mut cmd = match &self.launcher {
            None => {
                let mut cmd = Command::new(&self.mux);
                cmd.arg("-L").arg(socket).args(args);
                cmd
            }
            Some(launcher) => {
                let mut cmd = launcher.command();
                for token in [self.mux.as_str(), "-L", socket].iter().chain(args) {
                    cmd.arg(posix_quote(token));
                }
                cmd
            }
        };
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
        crate::agent::preflight::launch_failure(launcher, context, err)
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

    /// Whether the multiplexer is psmux (the native-Windows tmux clone), whose
    /// protocol divergences — no `send-keys -H`, no per-window options, no
    /// `%window-close` — branch on this. A question about the multiplexer,
    /// never about the host's OS: see
    /// [`crate::backend::tmux_compat::control_mode::send_keys_commands`].
    pub fn uses_psmux(&self) -> bool {
        self.mux == "psmux"
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
        let t = TmuxTransport::local();
        let cmd = t.tmux_command("thurbox", &["has-session", "-t", "thurbox"]);
        let (prog, args) = program_and_args(&cmd);
        assert_eq!(prog, DEFAULT_MUX);
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
        let cmd = TmuxTransport::local().tmux_command("thurbox", &["has-session"]);
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
    fn uses_psmux_reflects_mux_binary() {
        assert_eq!(TmuxTransport::local().uses_psmux(), cfg!(windows));
        assert!(ssh("h", vec![], "psmux").uses_psmux());
        assert!(!ssh("h", vec![], "tmux").uses_psmux());
        // A WSL distro runs Linux `tmux`, which supports the `-H` hex flag, so
        // it must NOT take the psmux keystroke-encoding path.
        let wsl = wsl("Ubuntu");
        assert_eq!(wsl.mux(), "tmux");
        assert!(!wsl.uses_psmux());
    }

    #[test]
    fn is_remote_reflects_variant() {
        assert!(!TmuxTransport::local().is_remote());
        assert!(ssh("h", vec![], "tmux").is_remote());
        assert!(wsl("Ubuntu").is_remote());
    }
}
