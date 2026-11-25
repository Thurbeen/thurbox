use std::process::Command;

use crate::shell::{posix_quote, CommandLineTooLong, HostLauncher};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SocketDir {
    Inherited,
    Dir(String),
    HostData { app: String },
}

const SOCKET_PATH_MAX: usize = 103;

impl SocketDir {
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

#[derive(Debug, Clone)]
pub struct TmuxTransport {
    launcher: Option<HostLauncher>,
    mux: String,
    socket_dir: SocketDir,
    shell: crate::shell::HostShell,
    path: Option<String>,
}

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
    crate::shell::strip_telemetry_env(cmd);
}

impl TmuxTransport {
    pub fn local(mux: impl Into<String>) -> Self {
        Self {
            launcher: None,
            mux: mux.into(),
            socket_dir: SocketDir::Inherited,
            shell: crate::shell::HostShell::Posix,
            path: None,
        }
    }

    pub fn remote(launcher: HostLauncher, mux: impl Into<String>) -> Self {
        Self {
            launcher: Some(launcher),
            mux: mux.into(),
            socket_dir: SocketDir::Inherited,
            shell: crate::shell::HostShell::Posix,
            path: None,
        }
    }

    pub fn with_shell(mut self, shell: crate::shell::HostShell) -> Self {
        self.shell = shell;
        self
    }

    pub fn with_socket_dir(mut self, dir: SocketDir) -> Self {
        self.socket_dir = dir;
        self
    }

    pub fn with_path(mut self, assignment: Option<String>) -> Self {
        self.path = assignment;
        self
    }

    pub fn socket_dir(&self) -> &SocketDir {
        &self.socket_dir
    }

    pub fn tmux_command(&self, socket: &str, args: &[&str]) -> Result<Command, CommandLineTooLong> {
        let argv: Vec<&str> = ["-L", socket]
            .into_iter()
            .chain(args.iter().copied())
            .collect();
        let mut cmd = match (&self.launcher, &self.socket_dir) {
            (Some(launcher), SocketDir::Inherited)
                if self.shell == crate::shell::HostShell::PowerShell =>
            {
                self.shell.launch(launcher, &self.mux, &argv)?
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

    pub fn waiter_command(&self, socket: &str, channel: &str) -> Option<Command> {
        if self.shell == crate::shell::HostShell::PowerShell {
            return None;
        }
        let script = format!(
            "d={}; [ -d \"$d\" ] || mkdir -p -m 700 \"$d\"; \
             TMUX_TMPDIR=$d; export TMUX_TMPDIR; \
             exec flock -n \"$d/$2.waiter\" \"$1\" -L \"$2\" wait-for \"$3\"",
            self.socket_dir.shell_expr(socket)
        );
        Some(self.shell_command(&script, &["sh", &self.mux, socket, channel]))
    }

    pub fn powershell_command(&self, script: &str) -> Command {
        match &self.launcher {
            Some(launcher) => crate::shell::HostShell::PowerShell.run_script(launcher, script),
            None => crate::shell::local_powershell(script),
        }
    }

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

    pub fn is_remote(&self) -> bool {
        self.launcher.is_some()
    }

    pub fn mux(&self) -> &str {
        &self.mux
    }

    pub fn launcher(&self) -> &str {
        match &self.launcher {
            Some(launcher) => launcher.program(),
            None => &self.mux,
        }
    }

    pub fn launch_failure(&self, context: &str, err: std::io::Error) -> anyhow::Error {
        let launcher = self.is_remote().then(|| self.launcher());
        crate::preflight::launch_failure(launcher, &self.mux, context, err)
    }

    pub fn is_ssh(&self) -> bool {
        matches!(self.launcher, Some(HostLauncher::Ssh { .. }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_waiter_waits_on_the_server_in_its_own_socket_directory() {
        let wsl = TmuxTransport::remote(
            HostLauncher::Wsl {
                distro: "Ubuntu".into(),
            },
            "tmux",
        )
        .with_socket_dir(SocketDir::HostData {
            app: "runtime-mux".into(),
        });
        let cmd = wsl.waiter_command("rm", "ch").expect("a POSIX waiter");
        let argv: Vec<String> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert!(cmd.get_program().to_string_lossy().ends_with("wsl.exe"));
        let script = &argv[argv.len() - 5];
        assert!(
            script.starts_with(&format!(
                "d={};",
                SocketDir::HostData {
                    app: "runtime-mux".into()
                }
                .shell_expr("rm")
            )) && script
                .contains("exec flock -n \"$d/$2.waiter\" \"$1\" -L \"$2\" wait-for \"$3\""),
            "{script}"
        );
        assert_eq!(argv[argv.len() - 4..], ["sh", "tmux", "rm", "ch"]);
        let windows =
            ssh("me@win", vec![], "psmux").with_shell(crate::shell::HostShell::PowerShell);
        assert!(windows.waiter_command("rm", "ch").is_none());
    }

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
        // wsl.exe back-translates the caller's cwd through the distro name; `--cd /` bypasses that.
        #[cfg(unix)]
        let prefix: &[&str] = &["-d", "Ubuntu", "--cd", "/"];
        #[cfg(not(unix))]
        let prefix: &[&str] = &["-d", "Ubuntu"];
        let expected: Vec<&str> = prefix
            .iter()
            .copied()
            .chain([
                "--exec",
                "sh",
                "-c",
                crate::shell::WSL_PROFILE_EXEC,
                "sh",
                "tmux",
                "-L",
                "thurbox",
                "has-session",
                "-t",
                "thurbox",
            ])
            .collect();
        assert_eq!(args, expected);
    }

    #[test]
    fn tmux_command_strips_nesting_env() {
        let cmd = local()
            .tmux_command("thurbox", &["has-session"])
            .expect("command");
        // `Command::get_envs` lists a removed variable as (key, None).
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

    use crate::shell::is_telemetry_var;

    #[test]
    fn only_the_telemetry_variables_are_telemetry() {
        for var in [
            "OTEL_SERVICE_NAME",
            "OTEL_EXPORTER_OTLP_ENDPOINT",
            "TRACEPARENT",
            "TRACESTATE",
        ] {
            assert!(is_telemetry_var(var), "{var}");
        }
        for var in ["PATH", "HOTEL_X", "TRACEPARENTS", "TMUX_TMPDIR"] {
            assert!(!is_telemetry_var(var), "{var}");
        }
    }

    #[test]
    fn a_telemetry_variable_in_any_case_is_telemetry() {
        for var in [
            "otel_exporter_otlp_endpoint",
            "Otel_Service_Name",
            "traceparent",
            "TraceState",
        ] {
            assert!(is_telemetry_var(var), "{var}");
        }
        assert!(!is_telemetry_var("hotel_x"));
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

    #[cfg(unix)]
    fn host_data_dir(env: &[(&str, &str)], socket: &str) -> String {
        host_data_dir_in("sh", env, socket)
    }

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

    #[cfg(unix)]
    #[test]
    fn a_hosts_socket_path_is_measured_in_bytes_not_characters() {
        let base = tempfile::tempdir().expect("tempdir");
        let base = base.path().to_str().expect("utf-8");
        // SAFETY: `getuid` cannot fail and touches no memory.
        let uid = unsafe { libc::getuid() };
        let tail = format!("/thurbox/tmux/tmux-{uid}/thurbox");
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
