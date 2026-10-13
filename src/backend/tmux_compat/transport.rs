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

use crate::shell::{posix_quote, HostLauncher};

/// The directory a tmux server's socket lives under — the `TMUX_TMPDIR` every
/// command to it carries, under which tmux puts `tmux-<uid>/<socket>`.
///
/// It matters because tmux's own default is `/tmp`, and macOS sweeps old
/// entries out of `/private/tmp`: a unix socket's access time is not bumped by
/// `connect()`, so a server in daily use loses its socket after a few days. A
/// server whose socket is gone keeps running, unreachable, and a `new-session`
/// on the same name starts a second one beside it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SocketDir {
    /// `TMUX_TMPDIR` left as the environment has it — tmux's own `/tmp` when
    /// nothing sets it. Where every server ran before thurbox chose a
    /// directory, and still the place to look for one started then.
    Inherited,
    /// This absolute directory on the machine the multiplexer runs on.
    Dir(String),
    /// A host's thurbox data dir's `tmux/` — `${XDG_DATA_HOME:-$HOME/.local/share}/<app>/tmux`,
    /// resolved by the host's own shell, since this machine knows neither its
    /// `$HOME` nor its XDG.
    HostData { app: String },
}

/// The longest socket path `bind()` takes on any POSIX host: `sun_path` is
/// 104 bytes on macOS (108 on Linux), the terminating NUL included — the
/// same bound `backend::instance` holds a local directory to.
const SOCKET_PATH_MAX: usize = 103;

impl SocketDir {
    /// The `TMUX_TMPDIR` this names, as a POSIX `sh` expression for the
    /// machine the multiplexer runs on — the right side of an assignment, so
    /// it is never field-split and its quotes are removed.
    ///
    /// A `TMUX_TMPDIR` already set on the host wins over
    /// [`Self::HostData`], as it does locally when the directory is chosen
    /// (`backend::instance::local_socket_dir`): it is test and sandbox tooling
    /// scoping its servers, or an operator's own choice. A [`Self::Dir`] is
    /// that choice already made, so it is taken as it stands.
    ///
    /// A [`Self::HostData`] directory a `socket` path would not fit under
    /// (103 bytes, as `backend::instance` checks it locally), or
    /// that cannot be created, falls back to tmux's default there and then.
    /// The test is the same for every command, so all of them land on the
    /// same side of it.
    pub fn shell_expr(&self, socket: &str) -> String {
        match self {
            Self::Inherited => "${TMUX_TMPDIR:-/tmp}".to_string(),
            Self::Dir(dir) => posix_quote(dir),
            Self::HostData { app } => format!(
                "$(d=${{TMUX_TMPDIR:-${{XDG_DATA_HOME:-$HOME/.local/share}}/{}/tmux}}; \
                 s=$d/tmux-$(id -u)/{}; \
                 if [ \"${{#s}}\" -le {} ] && {{ [ -d \"$d\" ] || mkdir -p -m 700 \"$d\"; }} 2>/dev/null; \
                 then printf %s \"$d\"; else printf %s \"${{TMUX_TMPDIR:-/tmp}}\"; fi)",
                posix_quote(app),
                posix_quote(socket),
                SOCKET_PATH_MAX
            ),
        }
    }

    /// Where to say the socket is, in a log line.
    pub fn describe(&self) -> String {
        match self {
            Self::Inherited => "$TMUX_TMPDIR (tmux's default: /tmp)".to_string(),
            Self::Dir(dir) => dir.clone(),
            Self::HostData { app } => format!(
                "the host's $XDG_DATA_HOME/{app}/tmux (its /tmp when a socket would not fit there)"
            ),
        }
    }
}

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
    /// The `TMUX_TMPDIR` every command carries. [`SocketDir::Inherited`]
    /// unless the adapter chose one: psmux has no socket directory at all.
    socket_dir: SocketDir,
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
            socket_dir: SocketDir::Inherited,
        }
    }

    /// `mux` on the host `launcher` reaches.
    pub fn remote(launcher: HostLauncher, mux: impl Into<String>) -> Self {
        Self {
            launcher: Some(launcher),
            mux: mux.into(),
            socket_dir: SocketDir::Inherited,
        }
    }

    /// This transport, with every command it builds carrying `dir`.
    pub fn with_socket_dir(mut self, dir: SocketDir) -> Self {
        self.socket_dir = dir;
        self
    }

    /// The socket directory this transport's commands carry.
    pub fn socket_dir(&self) -> &SocketDir {
        &self.socket_dir
    }

    /// Build a [`Command`] running `<mux> -L <socket> <args…>`, behind the
    /// launcher for a remote host ([`crate::shell::launch`], which adds
    /// nothing of its own: `-L` is the tmux grammar's, so it is written here).
    ///
    /// Nesting env vars are stripped (see `strip_mux_nesting_env`) so the
    /// command targets thurbox's own server even when thurbox runs inside a pane.
    ///
    /// The socket directory ([`Self::socket_dir`]) is set in the command's
    /// environment here, and on a host by a `sh -c` in front of the
    /// multiplexer that resolves it there and creates it (`0700`) when it is
    /// missing — its `$HOME` is not known here. [`SocketDir::Inherited`] adds
    /// nothing on either side.
    pub fn tmux_command(&self, socket: &str, args: &[&str]) -> Command {
        let argv: Vec<&str> = ["-L", socket]
            .into_iter()
            .chain(args.iter().copied())
            .collect();
        let mut cmd = match (&self.launcher, &self.socket_dir) {
            (_, SocketDir::Inherited) => {
                crate::shell::launch(self.launcher.as_ref(), &self.mux, &argv)
            }
            (None, SocketDir::Dir(dir)) => {
                let mut cmd = crate::shell::launch(None, &self.mux, &argv);
                cmd.env("TMUX_TMPDIR", dir);
                cmd
            }
            (Some(_), dir) | (None, dir @ SocketDir::HostData { .. }) => {
                let script = format!(
                    "d={}; [ -d \"$d\" ] || mkdir -p -m 700 \"$d\"; \
                     TMUX_TMPDIR=$d; export TMUX_TMPDIR; exec \"$@\"",
                    dir.shell_expr(socket)
                );
                let mut args = vec!["sh", self.mux.as_str()];
                args.extend(argv);
                self.shell_command(&script, &args)
            }
        };
        strip_mux_nesting_env(&mut cmd);
        cmd
    }

    /// `sh -c <script> <args…>` on the machine the multiplexer runs on — the
    /// first of `args` is `$0`. For the questions only a shell there can
    /// answer: where a directory resolves, which process has a socket bound.
    pub fn shell_command(&self, script: &str, args: &[&str]) -> Command {
        let mut cmd = match &self.launcher {
            None => {
                let mut cmd = Command::new("sh");
                cmd.arg("-c").arg(script).args(args);
                cmd
            }
            Some(launcher) => launcher.shell_c_args(script, args),
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
    fn a_local_socket_dir_rides_in_the_environment() {
        let cmd = local()
            .with_socket_dir(SocketDir::Dir("/home/me/.local/share/thurbox/tmux".into()))
            .tmux_command("thurbox", &["has-session"]);
        let (prog, args) = program_and_args(&cmd);
        assert_eq!(prog, "tmux");
        assert_eq!(args, ["-L", "thurbox", "has-session"]);
        let dir = cmd
            .get_envs()
            .find(|(k, _)| *k == "TMUX_TMPDIR")
            .and_then(|(_, v)| v);
        assert_eq!(
            dir.map(|v| v.to_string_lossy().into_owned()).as_deref(),
            Some("/home/me/.local/share/thurbox/tmux")
        );
    }

    /// A host's directory is under its own `$HOME`, which only its shell
    /// knows: the command goes through `sh -c`, and the multiplexer's argv
    /// follows as the script's own arguments, each one word.
    #[test]
    fn a_hosts_socket_dir_is_resolved_by_its_own_shell() {
        let t = ssh("me@devbox", vec![], "tmux").with_socket_dir(SocketDir::HostData {
            app: "thurbox".into(),
        });
        let (prog, args) = program_and_args(&t.tmux_command("my sock", &["has-session"]));
        assert_eq!(prog, "ssh");
        let tail = &args[args.len() - 5..];
        assert_eq!(tail, ["sh", "tmux", "-L", "'my sock'", "has-session"]);
        let script = &args[args.len() - 6];
        assert!(script.contains("XDG_DATA_HOME"), "{script}");
        assert!(script.contains("mkdir -p -m 700"), "{script}");
        assert_eq!(args[args.len() - 8..args.len() - 6], ["sh", "-c"]);
    }

    #[test]
    fn an_explicit_socket_dir_is_taken_as_it_stands() {
        assert_eq!(
            SocketDir::Inherited.shell_expr("thurbox"),
            "${TMUX_TMPDIR:-/tmp}"
        );
        assert_eq!(
            SocketDir::Dir("/a b".into()).shell_expr("thurbox"),
            "'/a b'"
        );
    }

    /// What a host's shell makes of [`SocketDir::HostData`] with `env`.
    #[cfg(unix)]
    fn host_data_dir(env: &[(&str, &str)], socket: &str) -> String {
        let expr = SocketDir::HostData {
            app: "thurbox".into(),
        }
        .shell_expr(socket);
        let mut cmd = Command::new("sh");
        cmd.env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default());
        cmd.envs(env.iter().copied());
        let out = cmd
            .args(["-c", &format!("printf %s \"{expr}\"")])
            .output()
            .expect("sh");
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    /// A host's data dir holds its sockets when one fits there; a data dir
    /// too deep for a socket path leaves the host on tmux's default rather
    /// than failing every command with `File name too long`.
    #[cfg(unix)]
    #[test]
    fn a_hosts_socket_dir_falls_back_where_a_socket_would_not_fit() {
        let home = tempfile::tempdir().expect("tempdir");
        let home = home.path().to_str().expect("utf-8");
        assert_eq!(
            host_data_dir(&[("HOME", home)], "thurbox"),
            format!("{home}/.local/share/thurbox/tmux")
        );
        assert!(std::path::Path::new(&format!("{home}/.local/share/thurbox/tmux")).is_dir());
        let deep = format!("{home}/{}", "d".repeat(120));
        assert_eq!(
            host_data_dir(&[("HOME", home), ("XDG_DATA_HOME", &deep)], "thurbox"),
            "/tmp"
        );
        assert!(
            !std::path::Path::new(&deep).exists(),
            "nothing is made there"
        );
        assert_eq!(
            host_data_dir(
                &[
                    ("HOME", home),
                    ("XDG_DATA_HOME", &deep),
                    ("TMUX_TMPDIR", "/x")
                ],
                "thurbox"
            ),
            "/x",
            "a TMUX_TMPDIR already set wins"
        );
    }

    #[test]
    fn is_remote_reflects_variant() {
        assert!(!local().is_remote());
        assert!(ssh("h", vec![], "tmux").is_remote());
        assert!(wsl("Ubuntu").is_remote());
    }
}
