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
//! does not know which multiplexer the binary is. Of the host's OS it knows one
//! bit, whether a native-Windows login shell reads its commands
//! ([`TmuxTransport::on_windows`]), because that changes how a line is quoted.

use std::process::Command;

use crate::shell::{posix_quote, CommandLineTooLong, HostLauncher};

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
    /// same side of it. The path is measured in bytes, as `bind()` does, by
    /// `wc -c`: `${#s}` counts characters in a UTF-8 locale under bash. Only
    /// that count runs in the C locale; the multiplexer keeps its own. The
    /// socket name is assigned before it is measured, never spliced into the
    /// double-quoted `printf` argument, where its single quotes would not
    /// apply.
    pub fn shell_expr(&self, socket: &str) -> String {
        match self {
            Self::Inherited => "${TMUX_TMPDIR:-/tmp}".to_string(),
            Self::Dir(dir) => posix_quote(dir),
            Self::HostData { app } => format!(
                "$(d=${{TMUX_TMPDIR:-${{XDG_DATA_HOME:-$HOME/.local/share}}/{}/tmux}}; \
                 s=$d/tmux-$(id -u)/{}; \
                 b=$(printf %s \"$s\" | LC_ALL=C wc -c | tr -d ' '); \
                 if [ \"$b\" -le {} ] && {{ [ -d \"$d\" ] || mkdir -p -m 700 \"$d\"; }} 2>/dev/null; \
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
    /// The host is native Windows, so a line reaches `cmd.exe` or PowerShell
    /// rather than a POSIX shell ([`crate::shell::launch_on_windows`]).
    windows: bool,
    /// Run on the host before the multiplexer or a [`Self::shell_command`]
    /// script — see [`Self::with_path`]. POSIX hosts only.
    path: Option<String>,
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
            windows: false,
            path: None,
        }
    }

    /// `mux` on the host `launcher` reaches.
    pub fn remote(launcher: HostLauncher, mux: impl Into<String>) -> Self {
        Self {
            launcher: Some(launcher),
            mux: mux.into(),
            socket_dir: SocketDir::Inherited,
            windows: false,
            path: None,
        }
    }

    /// This transport, for a native-Windows host: each remote command is
    /// quoted for its `cmd.exe` or PowerShell login shell, not for POSIX.
    pub fn on_windows(mut self) -> Self {
        self.windows = true;
        self
    }

    /// This transport, with every command it builds carrying `dir`.
    pub fn with_socket_dir(mut self, dir: SocketDir) -> Self {
        self.socket_dir = dir;
        self
    }

    /// This transport, with every command it runs on a POSIX host behind
    /// `assignment` (`PATH=…; export PATH; `, from
    /// [`crate::agent::host_path::assignment_for`]), so the multiplexer
    /// resolves on the `PATH` the probe that offered it searched rather than
    /// on sshd's non-login one. `None` leaves the `PATH` alone.
    pub fn with_path(mut self, assignment: Option<String>) -> Self {
        self.path = assignment;
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
    ///
    /// A host's `PATH` assignment ([`Self::with_path`]) goes in front of the
    /// multiplexer the same way, through the same `sh -c`, so tmux and rmux
    /// resolve their binary alike whatever socket directory they carry.
    ///
    /// On a Windows host ([`Self::on_windows`]) the line is built by
    /// [`crate::shell::launch_on_windows`], which refuses one past the
    /// Windows command-line limit.
    pub fn tmux_command(&self, socket: &str, args: &[&str]) -> Result<Command, CommandLineTooLong> {
        let argv: Vec<&str> = ["-L", socket]
            .into_iter()
            .chain(args.iter().copied())
            .collect();
        let mut cmd = match (&self.launcher, &self.socket_dir) {
            (Some(launcher), SocketDir::Inherited) if self.windows => {
                crate::shell::launch_on_windows(launcher, &self.mux, &argv)?
            }
            (Some(_), SocketDir::Inherited) if self.path.is_some() => {
                let mut args = vec!["sh", self.mux.as_str()];
                args.extend(argv);
                self.shell_command("exec \"$@\"", &args)
            }
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
        Ok(cmd)
    }

    /// `sh -c <script> <args…>` on the machine the multiplexer runs on — the
    /// first of `args` is `$0`. For the questions only a shell there can
    /// answer: where a directory resolves, which process has a socket bound.
    /// On a host the script runs behind its `PATH` assignment
    /// ([`Self::with_path`]), so a script that runs the multiplexer finds it.
    pub fn shell_command(&self, script: &str, args: &[&str]) -> Command {
        let mut cmd = match &self.launcher {
            None => {
                let mut cmd = Command::new("sh");
                cmd.arg("-c").arg(script).args(args);
                cmd
            }
            Some(launcher) => match &self.path {
                Some(path) => launcher.shell_c_args(&format!("{path}{script}"), args),
                None => launcher.shell_c_args(script, args),
            },
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
        let cmd = t
            .tmux_command("thurbox", &["has-session", "-t", "thurbox"])
            .expect("command");
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
        let cmd = t
            .tmux_command("thurbox", &["has-session", "-t", "thurbox"])
            .expect("command");
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
        let cmd = t
            .tmux_command("thurbox", &["has-session"])
            .expect("command");
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
        let cmd = t
            .tmux_command("thurbox", &["has-session", "-t", "thurbox"])
            .expect("command");
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
        let cmd = local()
            .tmux_command("thurbox", &["has-session"])
            .expect("command");
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
            .tmux_command("thurbox", &["has-session"])
            .expect("command");
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
        let (prog, args) = program_and_args(
            &t.tmux_command("my sock", &["has-session"])
                .expect("command"),
        );
        assert_eq!(prog, "ssh");
        let tail = &args[args.len() - 5..];
        assert_eq!(tail, ["sh", "tmux", "-L", "'my sock'", "has-session"]);
        let script = &args[args.len() - 6];
        assert!(script.contains("XDG_DATA_HOME"), "{script}");
        assert!(script.contains("mkdir -p -m 700"), "{script}");
        assert_eq!(args[args.len() - 8..args.len() - 6], ["sh", "-c"]);
    }

    /// A host's `PATH` leads every multiplexer command, with a socket
    /// directory (tmux) or without one (rmux), so both resolve the binary
    /// alike (#1461).
    #[test]
    fn a_hosts_path_leads_its_multiplexer_commands_with_or_without_a_socket_dir() {
        let path = "PATH=/opt/homebrew/bin:/usr/bin; export PATH; ";
        for (mux, dir) in [
            (
                "tmux",
                SocketDir::HostData {
                    app: "thurbox".into(),
                },
            ),
            ("rmux", SocketDir::Inherited),
        ] {
            let t = ssh("me@mac", vec![], mux)
                .with_socket_dir(dir)
                .with_path(Some(path.into()));
            let (prog, args) = program_and_args(&t.tmux_command("s", &["-V"]).expect("command"));
            assert_eq!(prog, "ssh");
            assert_eq!(args[args.len() - 5..], ["sh", mux, "-L", "s", "-V"]);
            let script = &args[args.len() - 6];
            assert!(
                script.starts_with(&posix_quote(path)[..path.len()]),
                "{script}"
            );
            assert!(script.contains("exec \"$@\""), "{script}");
        }
    }

    /// The questions asked of a host's shell run on its `PATH` too: the
    /// socket search runs the multiplexer to see which server answers.
    #[test]
    fn a_hosts_path_leads_its_shell_commands_but_not_a_local_one() {
        let path = "PATH=/opt/homebrew/bin; export PATH; ";
        let (_, args) = program_and_args(
            &ssh("me@mac", vec![], "tmux")
                .with_path(Some(path.into()))
                .shell_command("true", &["sh"]),
        );
        assert!(args
            .iter()
            .any(|a| a.contains("PATH=/opt/homebrew/bin; export PATH; true")));
        let (_, args) = program_and_args(&local().shell_command("true", &["sh"]));
        assert_eq!(args, ["-c", "true", "sh"]);
    }

    #[test]
    fn a_remote_command_without_a_path_is_launched_as_it_was() {
        let (_, args) = program_and_args(
            &ssh("me@box", vec![], "rmux")
                .tmux_command("s", &["-V"])
                .unwrap(),
        );
        assert_eq!(args[args.len() - 4..], ["rmux", "-L", "s", "-V"]);
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
        host_data_dir_in("sh", env, socket)
    }

    /// [`host_data_dir`], with `shell` as the host's `sh`.
    #[cfg(unix)]
    fn host_data_dir_in(shell: &str, env: &[(&str, &str)], socket: &str) -> String {
        let expr = SocketDir::HostData {
            app: "thurbox".into(),
        }
        .shell_expr(socket);
        let mut cmd = Command::new(shell);
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

    /// The socket name reaches the host already quoted for the shell, so it
    /// has to land where that quoting applies: a name with `"` in it must not
    /// end the string it sits in, one with a backquote must not run it, and
    /// the quotes themselves must not count toward the path's length.
    #[cfg(unix)]
    #[test]
    fn a_hosts_socket_name_is_measured_as_the_name_it_is() {
        let base = tempfile::tempdir().expect("tempdir");
        let base = base.path().to_str().expect("utf-8");
        for (i, socket) in ["a\"b", "a$HOME"].into_iter().enumerate() {
            let data = format!("{base}/{i}");
            assert_eq!(
                host_data_dir(&[("HOME", base), ("XDG_DATA_HOME", &data)], socket),
                format!("{data}/thurbox/tmux"),
                "{socket}"
            );
        }
        host_data_dir(&[("HOME", base)], "a`touch $HOME/ran`b");
        assert!(
            !std::path::Path::new(&format!("{base}/ran")).exists(),
            "nothing ran"
        );

        // A name `posix_quote` wraps, on a path exactly at the limit: the
        // path fits, so the data dir is kept.
        // SAFETY: `getuid` cannot fail and touches no memory.
        let uid = unsafe { libc::getuid() };
        let socket = "a b";
        let tail = format!("/thurbox/tmux/tmux-{uid}/{socket}");
        let Some(pad) = SOCKET_PATH_MAX.checked_sub(base.len() + 1 + tail.len()) else {
            eprintln!("skipping: the temp dir leaves no room");
            return;
        };
        let data = format!("{base}/{}", "d".repeat(pad));
        assert_eq!(data.len() + tail.len(), SOCKET_PATH_MAX);
        assert_eq!(
            host_data_dir(&[("HOME", base), ("XDG_DATA_HOME", &data)], socket),
            format!("{data}/thurbox/tmux")
        );
    }

    #[test]
    fn is_remote_reflects_variant() {
        assert!(!local().is_remote());
        assert!(ssh("h", vec![], "tmux").is_remote());
        assert!(wsl("Ubuntu").is_remote());
    }

    /// A socket address is bytes, and a host's `sh` in a UTF-8 locale counts
    /// `${#s}` in characters — bash does, as macOS's `sh` and as a Linux
    /// host's — so a data dir with non-ASCII in it is measured in bytes, or it
    /// passes the check and fails every command with `File name too long`.
    #[cfg(unix)]
    #[test]
    fn a_hosts_socket_path_is_measured_in_bytes_not_characters() {
        let base = tempfile::tempdir().expect("tempdir");
        let base = base.path().to_str().expect("utf-8");
        // SAFETY: `getuid` cannot fail and touches no memory.
        let uid = unsafe { libc::getuid() };
        let tail = format!("/thurbox/tmux/tmux-{uid}/thurbox");
        // As many three-byte characters as fit in 103 *characters*: within
        // the limit counted that way, well over it counted in bytes.
        let Some(wide) = 103usize.checked_sub(base.len() + 1 + tail.len()) else {
            eprintln!("skipping: the temp dir leaves no room");
            return;
        };
        if wide < 10 {
            eprintln!("skipping: the temp dir leaves no room");
            return;
        }
        let deep = format!("{base}/{}", "界".repeat(wide));
        for shell in ["sh", "bash"] {
            if Command::new(shell).arg("-c").arg(":").output().is_err() {
                continue;
            }
            for locale in ["C.UTF-8", "en_US.UTF-8"] {
                assert_eq!(
                    host_data_dir_in(
                        shell,
                        &[("HOME", base), ("XDG_DATA_HOME", &deep), ("LC_ALL", locale)],
                        "thurbox"
                    ),
                    "/tmp",
                    "{shell} under {locale}"
                );
            }
        }
    }
}
