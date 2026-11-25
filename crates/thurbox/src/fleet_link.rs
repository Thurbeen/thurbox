use std::io::Write;
use std::process::{Command, Stdio};
use std::time::Duration;

pub const REMOTE_PROBE_TIMEOUT: Duration = Duration::from_secs(15);

use crate::session::settings::{FleetDef, FleetTransport, IMPLIED_FLEET};
use crate::shell::HostLauncher;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FleetLink {
    name: String,
    launcher: Option<HostLauncher>,
    program: String,
    config: Option<String>,
}

impl FleetLink {
    pub fn local(program: impl Into<String>) -> Self {
        Self {
            name: IMPLIED_FLEET.to_string(),
            launcher: None,
            program: program.into(),
            config: None,
        }
    }

    pub fn of(name: &str, def: &FleetDef) -> Self {
        let launcher = match &def.transport {
            FleetTransport::Local(_) => None,
            FleetTransport::Ssh { ssh, ssh_opts } => Some(HostLauncher::Ssh {
                destination: ssh.clone(),
                ssh_opts: ssh_opts.clone(),
            }),
            FleetTransport::Wsl { wsl } => Some(HostLauncher::Wsl {
                distro: wsl.clone(),
            }),
        };
        Self {
            name: name.to_string(),
            launcher,
            program: def.program.clone(),
            config: def.config.clone(),
        }
    }

    pub fn in_force(settings: &crate::session::settings::Settings) -> Self {
        let (name, def) = settings.fleet_in_force();
        Self::of(&name, &def)
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn program(&self) -> &str {
        &self.program
    }

    pub fn with_program(&self, program: impl Into<String>) -> Self {
        Self {
            program: program.into(),
            ..self.clone()
        }
    }

    pub fn config(&self) -> Option<&str> {
        self.config.as_deref()
    }

    pub fn is_local(&self) -> bool {
        self.launcher.is_none()
    }

    pub fn local_config(&self) -> Option<(std::path::PathBuf, &'static str)> {
        if let Some(path) = self.config() {
            return Some((std::path::PathBuf::from(path), "settings.toml"));
        }
        if !self.is_local() {
            return None;
        }
        if let Some(path) = std::env::var_os("FLEET_CONFIG").filter(|p| !p.is_empty()) {
            return Some((std::path::PathBuf::from(path), "FLEET_CONFIG"));
        }
        let base = if cfg!(windows) {
            std::env::var_os("APPDATA")
                .filter(|d| !d.is_empty())?
                .into()
        } else if let Some(d) = std::env::var_os("XDG_CONFIG_HOME").filter(|d| !d.is_empty()) {
            std::path::PathBuf::from(d)
        } else {
            std::path::PathBuf::from(std::env::var_os("HOME")?).join(".config")
        };
        Some((base.join("fleet/config.toml"), "the per-user file"))
    }

    pub fn not_running(&self, manager: &serde_json::Value) -> Option<String> {
        if !self.is_local() {
            return None;
        }
        Some(
            self.no_config()
                .unwrap_or_else(|| format!("Fleet not running · {}", self.start_manager(manager))),
        )
    }

    pub fn start_manager(&self, manager: &serde_json::Value) -> String {
        self.supervise(manager, "start")
            .unwrap_or_else(|| self.fleet_command(&["service", "install", "--now"]))
    }

    pub fn restart_manager(&self, manager: &serde_json::Value) -> String {
        self.supervise(manager, "restart").unwrap_or_else(|| {
            format!(
                "stop its `{}` and run it again",
                self.fleet_command(&["up"])
            )
        })
    }

    fn supervise(&self, manager: &serde_json::Value, verb: &str) -> Option<String> {
        let unit = supervisor_unit(manager)?;
        Some(if manager["supervisor"]["kind"] == "systemd" {
            command_line(&["systemctl", "--user", verb, unit])
        } else {
            self.fleet_command(&["service", verb])
        })
    }

    fn fleet_command(&self, verb: &[&str]) -> String {
        let mut words = vec![self.program()];
        if let Some(config) = self.config() {
            words.extend(["--config", config]);
        }
        words.extend(verb);
        command_line(&words)
    }

    pub fn no_config(&self) -> Option<String> {
        if !self.is_local() {
            return None;
        }
        let (path, _) = self.local_config()?;
        if path.exists() {
            return None;
        }
        Some(match self.config() {
            Some(named) => format!(
                "no Fleet config · {}",
                command_line(&["fleet", "--config", named, "init"])
            ),
            None => NO_CONFIG.to_string(),
        })
    }

    pub fn transport(&self) -> String {
        match &self.launcher {
            None => "local Fleet".to_string(),
            Some(HostLauncher::Ssh { destination, .. }) => format!("ssh {destination}"),
            Some(HostLauncher::Wsl { distro }) => format!("wsl {distro}"),
        }
    }

    pub fn is_ssh(&self) -> bool {
        matches!(self.launcher, Some(HostLauncher::Ssh { .. }))
    }

    pub fn command(&self, args: &[&str]) -> Command {
        let mut words: Vec<&str> = Vec::with_capacity(args.len() + 2);
        if let Some(config) = &self.config {
            words.extend(["--config", config.as_str()]);
        }
        words.extend(args);
        self.build(&self.program, &words)
    }

    pub fn launch(&self, argv: &[String]) -> Result<Command, String> {
        let (program, args) = argv
            .split_first()
            .ok_or("the stream's argv names no program")?;
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        Ok(self.build(program, &args))
    }

    pub fn stream(&self, verb: &str, task: &str) -> Result<Command, StreamFailure> {
        let _span = command_span(verb, Some(task));
        let out = self
            .command(&[verb, task, "--json"])
            .stdin(Stdio::null())
            .output()
            .map_err(|e| format!("could not start fleet {verb}: {e}"))?;
        if let Some(why) = self.unreachable(&out) {
            return Err(why.into());
        }
        if !out.status.success() {
            let stderr = String::from_utf8_lossy(&out.stderr);
            let why = stderr
                .lines()
                .find_map(|line| line.strip_prefix("error: "))
                .unwrap_or(stderr.trim());
            return Err(StreamFailure {
                message: format!("fleet {verb}: {why}"),
                exit: out.status.code(),
            });
        }
        let answer: serde_json::Value = serde_json::from_slice(&out.stdout)
            .map_err(|e| format!("fleet {verb} answered no JSON: {e}"))?;
        let argv: Vec<String> = answer["stream"]["argv"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|arg| arg.as_str().map(str::to_string))
            .collect();
        if argv.is_empty() {
            return Err(format!("fleet {verb} answered no stream: {answer}").into());
        }
        Ok(self.launch(&argv)?)
    }

    fn build(&self, program: &str, args: &[&str]) -> Command {
        let mut cmd = crate::shell::launch(self.launcher.as_ref(), program, args);
        if self.launcher.is_none() {
            carry_trace(&mut cmd);
        }
        cmd
    }

    pub fn home(&self, timeout: Duration) -> Option<String> {
        let argv = ["sh", "-c", r#"printf %s "$HOME""#].map(String::from);
        let mut child = self
            .launch(&argv)
            .ok()?
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .ok()?;
        let started = std::time::Instant::now();
        while child.try_wait().ok()?.is_none() {
            if started.elapsed() >= timeout {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let out = child.wait_with_output().ok()?;
        let home = String::from_utf8(out.stdout).ok()?;
        (out.status.success() && home.starts_with('/')).then_some(home)
    }

    pub fn probe_timeout(&self) -> std::time::Duration {
        if self.is_local() {
            PROBE_TIMEOUT
        } else {
            REMOTE_PROBE_TIMEOUT
        }
    }

    pub fn bounded_output(
        &self,
        args: &[&str],
        timeout: std::time::Duration,
    ) -> Result<std::process::Output, String> {
        let _span = command_span(args.first().copied().unwrap_or_default(), None);
        let mut command = self.command(args);
        command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut wrap = process_wrap::std::CommandWrap::from(command);
        #[cfg(unix)]
        wrap.wrap(process_wrap::std::ProcessGroup::leader());
        #[cfg(windows)]
        wrap.wrap(process_wrap::std::JobObject);
        let mut child = wrap
            .spawn()
            .map_err(|e| format!("could not start fleet {}: {e}", args.join(" ")))?;
        let stdout = read_all(child.stdout().take());
        let stderr = read_all(child.stderr().take());
        let deadline = std::time::Instant::now() + timeout;
        let did_not_answer = || {
            format!(
                "the Fleet {} did not answer `fleet {}` within {}s",
                self.name,
                args.join(" "),
                timeout.as_secs()
            )
        };
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) if std::time::Instant::now() < deadline => {
                    std::thread::sleep(std::time::Duration::from_millis(20));
                }
                Ok(None) => {
                    let _ = child.start_kill();
                    let _ = child.wait();
                    return Err(did_not_answer());
                }
                Err(e) => return Err(format!("{}: {e}", self.program)),
            }
        };
        // ssh's ControlPersist master can hold stderr open after the exit.
        let grace = std::time::Instant::now() + PIPE_GRACE;
        let (out, err) = (stdout.by(grace), stderr.by(grace));
        Ok(std::process::Output {
            status,
            stdout: out,
            stderr: err,
        })
    }

    pub fn version(&self) -> Result<serde_json::Value, String> {
        let out = self.bounded_output(&["version", "--json"], self.probe_timeout())?;
        if !out.status.success() {
            return Err(self.unreachable(&out).unwrap_or_else(|| {
                format!(
                    "fleet version: {}",
                    String::from_utf8_lossy(&out.stderr).trim()
                )
            }));
        }
        serde_json::from_slice(&out.stdout)
            .map_err(|e| format!("fleet version answered no JSON: {e}"))
    }

    pub fn unreachable(&self, out: &std::process::Output) -> Option<String> {
        (self.is_ssh() && out.status.code() == Some(255)).then(|| {
            format!(
                "the Fleet {} cannot be reached: {}",
                self.name,
                String::from_utf8_lossy(&out.stderr).trim()
            )
        })
    }

    pub fn run(&self, args: &[&str], input: Option<&str>) -> Result<(), String> {
        let verb = args.first().copied().unwrap_or_default();
        let _span = command_span(verb, None);
        let mut child = self
            .command(args)
            .stdin(if input.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("could not start fleet {verb}: {e}"))?;
        if let (Some(input), Some(mut stdin)) = (input, child.stdin.take()) {
            stdin
                .write_all(input.as_bytes())
                .map_err(|e| format!("fleet {verb}: {e}"))?;
        }
        let out = child
            .wait_with_output()
            .map_err(|e| format!("fleet {verb}: {e}"))?;
        if out.status.success() {
            return Ok(());
        }
        let stderr = String::from_utf8_lossy(&out.stderr);
        let why = stderr
            .lines()
            .find_map(|line| line.strip_prefix("error: "))
            .unwrap_or(stderr.trim());
        Err(format!("fleet {verb}: {why}"))
    }
}

fn command_span(verb: &str, task: Option<&str>) -> opentelemetry::ContextGuard {
    use opentelemetry::trace::{SpanKind, TraceContextExt, Tracer};
    use opentelemetry::KeyValue;
    let tracer = opentelemetry::global::tracer("thurbox");
    let mut attributes = vec![KeyValue::new("fleet.command", verb.to_string())];
    attributes.extend(task.map(|task| KeyValue::new("fleet.name", task.to_string())));
    let span = tracer
        .span_builder(format!("fleet {verb}"))
        .with_kind(SpanKind::Client)
        .with_attributes(attributes)
        .start(&tracer);
    opentelemetry::Context::current_with_span(span).attach()
}

fn carry_trace(cmd: &mut Command) {
    cmd.env_remove("OTEL_SERVICE_NAME")
        .env_remove(otel_env::TRACEPARENT);
    if let Some(parent) = otel_env::traceparent() {
        cmd.env(otel_env::TRACEPARENT, parent);
    }
}

#[derive(Debug)]
pub struct StreamFailure {
    pub message: String,
    pub exit: Option<i32>,
}

impl From<String> for StreamFailure {
    fn from(message: String) -> Self {
        Self {
            message,
            exit: None,
        }
    }
}

const PIPE_GRACE: std::time::Duration = std::time::Duration::from_millis(250);

const PIPE_CAP: usize = 1024 * 1024;

// Closing the pipe would SIGPIPE ssh's ControlPersist master, which outlives the call.
struct Reading {
    kept: std::sync::Arc<std::sync::Mutex<Kept>>,
    done: std::sync::mpsc::Receiver<()>,
}

#[derive(Default)]
struct Kept {
    bytes: Vec<u8>,
    taken: bool,
}

fn read_all(pipe: Option<impl std::io::Read + Send + 'static>) -> Reading {
    let kept = std::sync::Arc::new(std::sync::Mutex::new(Kept::default()));
    let (tx, done) = std::sync::mpsc::channel();
    let into = std::sync::Arc::clone(&kept);
    std::thread::spawn(move || {
        if let Some(mut pipe) = pipe {
            let mut chunk = [0u8; 16 * 1024];
            while let Ok(n @ 1..) = std::io::Read::read(&mut pipe, &mut chunk) {
                if let Ok(mut kept) = into.lock() {
                    if !kept.taken {
                        let room = PIPE_CAP.saturating_sub(kept.bytes.len());
                        kept.bytes.extend_from_slice(&chunk[..n.min(room)]);
                    }
                }
            }
        }
        let _ = tx.send(());
    });
    Reading { kept, done }
}

impl Reading {
    fn by(&self, deadline: std::time::Instant) -> Vec<u8> {
        let _ = self
            .done
            .recv_timeout(deadline.saturating_duration_since(std::time::Instant::now()));
        self.kept
            .lock()
            .map(|mut kept| {
                kept.taken = true;
                std::mem::take(&mut kept.bytes)
            })
            .unwrap_or_default()
    }
}

const PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

pub fn supervisor_unit(manager: &serde_json::Value) -> Option<&str> {
    manager["supervisor"]["unit"].as_str()
}

pub fn command_line(words: &[&str]) -> String {
    command_line_for(cfg!(windows), words)
}

fn command_line_for(powershell: bool, words: &[&str]) -> String {
    let quoted: Vec<String> = words
        .iter()
        .map(|word| match (needs_quotes(word), powershell) {
            (false, _) => word.to_string(),
            (true, true) => format!("'{}'", word.replace('\'', "''")),
            (true, false) => format!("'{}'", word.replace('\'', "'\\''")),
        })
        .collect();
    let line = quoted.join(" ");
    if powershell
        && quoted
            .first()
            .is_some_and(|program| program.starts_with('\''))
    {
        format!("& {line}")
    } else {
        line
    }
}

fn needs_quotes(word: &str) -> bool {
    word.is_empty()
        || !word
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "/._-+,:@%".contains(c))
}

pub const NO_CONFIG: &str = "no Fleet config · fleet init";

pub const TASK_API: &str = "fleet/v1alpha1";

pub fn incompatible(answer: &serde_json::Value) -> Option<String> {
    let apis: Vec<&str> = answer["apiVersions"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|a| a.as_str())
        .collect();
    (!apis.contains(&TASK_API)).then(|| {
        format!(
            "incompatible: fleet {} speaks {}; thurbox writes {TASK_API} — update Fleet",
            answer["version"].as_str().unwrap_or("?"),
            if apis.is_empty() {
                "no API version".to_string()
            } else {
                apis.join(", ")
            }
        )
    })
}

#[derive(Debug, Clone)]
pub struct Fleets {
    in_force: FleetLink,
    all: Vec<FleetLink>,
    admitted: std::sync::Arc<std::sync::Mutex<std::collections::HashMap<String, Option<String>>>>,
}

impl Fleets {
    pub fn new(in_force: &str, all: Vec<FleetLink>) -> Self {
        let chosen = all
            .iter()
            .find(|link| link.name == in_force)
            .or(all.first())
            .cloned()
            .unwrap_or_else(|| FleetLink::local("fleet"));
        Self {
            in_force: chosen,
            all,
            admitted: Default::default(),
        }
    }

    pub fn of(settings: &crate::session::settings::Settings) -> Self {
        let all = settings
            .fleets()
            .iter()
            .map(|(name, def)| FleetLink::of(name, def))
            .collect();
        Self::new(&settings.fleet_in_force().0, all)
    }

    pub fn in_force(&self) -> &FleetLink {
        &self.in_force
    }

    pub fn all(&self) -> &[FleetLink] {
        &self.all
    }

    pub fn named(&self, name: Option<&str>) -> Result<&FleetLink, String> {
        let link = match name.filter(|name| !name.is_empty()) {
            None => &self.in_force,
            Some(name) => self
                .all
                .iter()
                .find(|link| link.name == name)
                .ok_or_else(|| format!("settings.toml lists no Fleet {name}"))?,
        };
        self.admit(link)?;
        Ok(link)
    }

    pub fn route<'a>(&self, session: &'a str) -> Result<(&FleetLink, &'a str), String> {
        let (link, task) = self.find(session)?;
        self.admit(link)?;
        Ok((link, task))
    }

    fn admit(&self, link: &FleetLink) -> Result<(), String> {
        let known = self
            .admitted
            .lock()
            .map_err(|_| "the Fleet check is poisoned")?
            .get(&link.name)
            .cloned();
        let verdict = match known {
            Some(verdict) => verdict,
            None => {
                let verdict = incompatible(&link.version()?);
                if let Ok(mut admitted) = self.admitted.lock() {
                    admitted.insert(link.name.clone(), verdict.clone());
                }
                verdict
            }
        };
        match verdict {
            None => Ok(()),
            Some(why) => Err(format!("the Fleet {} is {why}", link.name)),
        }
    }

    fn find<'a>(&self, session: &'a str) -> Result<(&FleetLink, &'a str), String> {
        match session.split_once(':') {
            None => Ok((&self.in_force, session)),
            Some((fleet, task)) => self
                .all
                .iter()
                .find(|link| link.name == fleet)
                .map(|link| (link, task))
                .ok_or_else(|| {
                    format!("{session} is a session of the Fleet {fleet}, which settings.toml does not list")
                }),
        }
    }
}

impl From<FleetLink> for Fleets {
    fn from(link: FleetLink) -> Self {
        Self::new(&link.name.clone(), vec![link])
    }
}

#[cfg(all(test, unix))]
pub(crate) mod testing {
    use std::ffi::OsString;
    use std::path::{Path, PathBuf};

    use super::FleetLink;
    use crate::session::settings::{FleetDef, FleetTransport};

    pub(crate) const PROGRAM: &str = "thurbox-test-remote-fleet";

    pub(crate) struct Remote {
        dir: tempfile::TempDir,
        pub(crate) link: FleetLink,
    }

    impl Remote {
        pub(crate) fn new(script: &str) -> Self {
            let dir = tempfile::tempdir().expect("remote dir");
            let bin = dir.path().join("bin");
            let remote = dir.path().join("remote");
            std::fs::create_dir_all(&bin).expect("bin");
            std::fs::create_dir_all(&remote).expect("remote");
            let log = dir.path().join("ssh.log");
            // ssh hands its command line to the remote shell, which re-splits it.
            executable(
                &bin.join("ssh"),
                &format!(
                    "while [ $# -gt 0 ]; do case \"$1\" in -o|-p) shift 2 ;; -*) shift ;; *) break ;; esac; done\n\
                     dest=$1; shift\n\
                     printf '%s %s\\n' \"$dest\" \"$*\" >> '{log}'\n\
                     PATH='{remote}':\"$PATH\"\n\
                     eval \"exec $*\"",
                    log = log.display(),
                    remote = remote.display(),
                ),
            );
            executable(&remote.join(PROGRAM), script);
            let link = FleetLink::of(
                "devbox",
                &FleetDef {
                    transport: FleetTransport::Ssh {
                        ssh: "devbox".into(),
                        ssh_opts: Vec::new(),
                    },
                    program: PROGRAM.into(),
                    config: None,
                },
            );
            Self { dir, link }
        }

        pub(crate) fn path(&self) -> OsString {
            let mut dirs = vec![self.dir.path().join("bin")];
            dirs.extend(std::env::split_paths(
                &std::env::var_os("PATH").unwrap_or_default(),
            ));
            std::env::join_paths(dirs).expect("PATH")
        }

        pub(crate) fn calls(&self) -> Vec<String> {
            std::fs::read_to_string(self.dir.path().join("ssh.log"))
                .unwrap_or_default()
                .lines()
                .map(str::to_string)
                .collect()
        }

        pub(crate) fn called(&self, args: &str) -> bool {
            let prefix = format!("devbox {PROGRAM} {args}");
            self.calls().iter().any(|call| call.starts_with(&prefix))
        }

        pub(crate) fn dir(&self) -> PathBuf {
            self.dir.path().to_path_buf()
        }
    }

    fn executable(path: &Path, script: &str) {
        use std::os::unix::fs::PermissionsExt;
        std::fs::write(path, format!("#!/bin/sh\n{script}\n")).expect("script");
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_named_config_is_quoted_in_the_command() {
        let dir = tempfile::tempdir().expect("dir");
        let config = dir.path().join("My Fleet").join("config.toml");
        let link = FleetLink::of(
            "laptop",
            &FleetDef {
                transport: Default::default(),
                program: "fleet".into(),
                config: Some(config.to_string_lossy().into_owned()),
            },
        );
        assert_eq!(
            link.no_config(),
            Some(format!(
                "no Fleet config · fleet --config '{}' init",
                config.display()
            ))
        );
    }

    #[test]
    fn a_command_line_is_quoted_for_the_shell_that_runs_it() {
        let words = ["C:\\My Tools\\fleet.exe", "--config", "it's.toml", "up"];
        assert_eq!(
            command_line_for(false, &words),
            "'C:\\My Tools\\fleet.exe' --config 'it'\\''s.toml' up"
        );
        assert_eq!(
            command_line_for(true, &words),
            "& 'C:\\My Tools\\fleet.exe' --config 'it''s.toml' up"
        );
        assert_eq!(command_line_for(true, &["fleet", "up"]), "fleet up");
    }

    #[test]
    fn the_manager_command_is_the_unit_fleet_names_or_this_fleets_own_command() {
        let link = FleetLink::of(
            "laptop",
            &FleetDef {
                transport: Default::default(),
                program: "/opt/My Fleet/fleet".into(),
                config: Some("/srv/fleet b/config.toml".into()),
            },
        );
        let quoted = "'/opt/My Fleet/fleet' --config '/srv/fleet b/config.toml' up";
        assert_eq!(
            link.fleet_command(&["up"]),
            if cfg!(windows) {
                format!("& {quoted}")
            } else {
                quoted.to_string()
            }
        );
        let none = serde_json::json!({"held": false, "supervisor": null});
        assert_eq!(
            FleetLink::local("fleet").start_manager(&none),
            "fleet service install --now"
        );
        let agent = serde_json::json!({"supervisor": {"kind": "launchd", "unit": "fleet"}});
        assert_eq!(
            FleetLink::local("fleet").start_manager(&agent),
            "fleet service start"
        );
        assert_eq!(
            FleetLink::local("fleet").restart_manager(&agent),
            "fleet service restart"
        );
        let named =
            serde_json::json!({"supervisor": {"kind": "systemd", "unit": "engine.service"}});
        assert_eq!(
            link.start_manager(&named),
            "systemctl --user start engine.service"
        );
        assert_eq!(
            link.restart_manager(&named),
            "systemctl --user restart engine.service"
        );
    }

    fn argv(command: &Command) -> Vec<String> {
        std::iter::once(command.get_program())
            .chain(command.get_args())
            .map(|a| a.to_string_lossy().into_owned())
            .collect()
    }

    fn env_of(command: &Command, name: &str) -> Option<Option<String>> {
        command
            .get_envs()
            .find(|(k, _)| *k == name)
            .map(|(_, v)| v.map(|v| v.to_string_lossy().into_owned()))
    }

    #[test]
    fn a_local_fleet_joins_the_current_trace() {
        use opentelemetry::trace::{
            SpanContext, SpanId, TraceContextExt, TraceFlags, TraceId, TraceState,
        };
        let link = FleetLink::local("fleet");
        let bare = link.command(&["get"]);
        assert_eq!(env_of(&bare, "TRACEPARENT"), Some(None), "removed");
        assert_eq!(env_of(&bare, "OTEL_SERVICE_NAME"), Some(None), "removed");
        let sc = SpanContext::new(
            TraceId::from_hex("4bf92f3577b34da6a3ce929d0e0e4736").unwrap(),
            SpanId::from_hex("00f067aa0ba902b7").unwrap(),
            TraceFlags::SAMPLED,
            true,
            TraceState::default(),
        );
        let _guard = opentelemetry::Context::new()
            .with_remote_span_context(sc)
            .attach();
        assert_eq!(
            env_of(&link.command(&["get"]), "TRACEPARENT"),
            Some(Some(
                "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01".into()
            ))
        );
        let remote = FleetLink::of(
            "box",
            &FleetDef {
                transport: FleetTransport::Ssh {
                    ssh: "devbox".into(),
                    ssh_opts: Vec::new(),
                },
                ..FleetDef::default()
            },
        );
        assert_eq!(env_of(&remote.command(&["get"]), "TRACEPARENT"), None);
    }

    #[test]
    fn a_local_call_is_the_program_and_its_arguments() {
        let link = FleetLink::local("/opt/fleet");
        assert_eq!(
            argv(&link.command(&["send", "review", "it's done"])),
            ["/opt/fleet", "send", "review", "it's done"]
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_probe_is_bounded_when_a_grandchild_holds_the_pipe() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().expect("dir");
        let fleet = dir.path().join("fleet");
        std::fs::write(&fleet, "#!/bin/sh\nsleep 600 &\necho answered\n").expect("fleet");
        std::fs::set_permissions(&fleet, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        let started = std::time::Instant::now();
        let out = FleetLink::local(fleet.to_string_lossy())
            .bounded_output(&["version"], std::time::Duration::from_millis(500));
        assert!(
            started.elapsed() < std::time::Duration::from_secs(2),
            "waited {:?}",
            started.elapsed()
        );
        let out = out.expect("the wrapper answered");
        assert_eq!(String::from_utf8_lossy(&out.stdout), "answered\n");
    }

    #[cfg(unix)]
    #[test]
    fn an_answer_on_stdout_survives_a_held_stderr() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().expect("dir");
        let fleet = dir.path().join("fleet");
        std::fs::write(
            &fleet,
            "#!/bin/sh\nsleep 600 >/dev/null &\necho '{\"apiVersions\":[\"fleet/v1alpha1\"]}'\n",
        )
        .expect("fleet");
        std::fs::set_permissions(&fleet, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        let started = std::time::Instant::now();
        let answer = FleetLink::local(fleet.to_string_lossy())
            .version()
            .expect("the answer on stdout");
        assert_eq!(answer["apiVersions"][0], "fleet/v1alpha1");
        assert!(started.elapsed() < std::time::Duration::from_secs(2));

        std::fs::write(&fleet, "#!/bin/sh\nsleep 600 >/dev/null &\necho answered\n")
            .expect("fleet");
        let started = std::time::Instant::now();
        let out = FleetLink::local(fleet.to_string_lossy())
            .bounded_output(&["version"], std::time::Duration::from_secs(5))
            .expect("answered");
        assert_eq!(String::from_utf8_lossy(&out.stdout), "answered\n");
        assert!(started.elapsed() < std::time::Duration::from_secs(2));
    }

    #[cfg(unix)]
    #[test]
    fn a_pipe_written_on_after_the_answer_stays_bounded() {
        let mut child = std::process::Command::new("sh")
            .args(["-c", "exec yes"])
            .stdout(Stdio::piped())
            .spawn()
            .expect("yes");
        let reading = read_all(child.stdout.take());
        let kept = std::sync::Arc::clone(&reading.kept);
        std::thread::sleep(std::time::Duration::from_millis(300));
        let taken = reading.by(std::time::Instant::now());
        assert!(
            !taken.is_empty() && taken.len() <= PIPE_CAP,
            "{}",
            taken.len()
        );
        std::thread::sleep(std::time::Duration::from_millis(300));
        assert_eq!(
            kept.lock().expect("kept").bytes.len(),
            0,
            "kept after it was taken"
        );
        let _ = child.kill();
        let _ = child.wait();
    }

    #[cfg(unix)]
    #[test]
    fn a_probe_past_its_deadline_is_stopped_with_its_group() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().expect("dir");
        let pid = dir.path().join("pid");
        let fleet = dir.path().join("fleet");
        std::fs::write(
            &fleet,
            format!(
                "#!/bin/sh\nsleep 60 &\necho $! > '{}'\nwait\n",
                pid.display()
            ),
        )
        .expect("fleet");
        std::fs::set_permissions(&fleet, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        let started = std::time::Instant::now();
        let why = FleetLink::local(fleet.to_string_lossy())
            .bounded_output(&["version"], std::time::Duration::from_millis(500))
            .expect_err("it never exits");
        assert!(why.contains("did not answer"), "{why}");
        assert!(started.elapsed() < std::time::Duration::from_secs(3));
        // kill delivers a signal: the process exits a moment later.
        let pid = std::fs::read_to_string(&pid).expect("pid");
        // A zombie answers `kill -0` until it is reaped.
        let alive = || {
            let signalled = std::process::Command::new("kill")
                .args(["-0", pid.trim()])
                .stderr(std::process::Stdio::null())
                .status()
                .is_ok_and(|status| status.success());
            let zombie =
                std::fs::read_to_string(format!("/proc/{}/stat", pid.trim())).is_ok_and(|stat| {
                    stat.rsplit(')')
                        .next()
                        .is_some_and(|rest| rest.trim_start().starts_with('Z'))
                });
            signalled && !zombie
        };
        let gone_by = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while alive() && std::time::Instant::now() < gone_by {
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        assert!(!alive(), "the group's sleep was left running");
    }

    #[test]
    fn a_wsl_fleet_hands_each_word_to_the_profile_script() {
        let wsl = |program: &str, config: Option<&str>| {
            FleetLink::of(
                "ubuntu",
                &FleetDef {
                    transport: FleetTransport::Wsl {
                        wsl: "Ubuntu".into(),
                    },
                    program: program.into(),
                    config: config.map(str::to_string),
                },
            )
        };
        let words = |argv: Vec<String>| {
            assert_eq!(argv[0], "wsl.exe");
            assert_eq!(&argv[1..3], ["-d", "Ubuntu"]);
            let exec = argv.iter().position(|a| a == "--exec").expect("--exec");
            assert_eq!(
                &argv[exec + 1..exec + 5],
                ["sh", "-c", crate::shell::WSL_PROFILE_EXEC, "sh"]
            );
            argv[exec + 5..].to_vec()
        };
        let spaced = wsl("/opt/my tools/fleet", Some("/etc/fleet/a b.toml"));
        assert_eq!(
            words(argv(&spaced.command(&["send", "review", "it's done"]))),
            [
                "/opt/my tools/fleet",
                "--config",
                "/etc/fleet/a b.toml",
                "send",
                "review",
                "it's done"
            ]
        );
        let stream = spaced
            .launch(&[
                "/srv/my apps/runtime-mux".into(),
                "view".into(),
                "w 1".into(),
            ])
            .expect("a stream");
        assert_eq!(
            words(argv(&stream)),
            ["/srv/my apps/runtime-mux", "view", "w 1"]
        );
        assert_eq!(
            words(argv(&wsl("fleet", None).command(&["version", "--json"]))),
            ["fleet", "version", "--json"],
            "a bare name is left for the profile's PATH to find"
        );
    }

    #[test]
    fn a_fleets_config_goes_before_the_verb() {
        let def = |transport| FleetDef {
            transport,
            program: "fleet".into(),
            config: Some("/etc/fleet/a b.toml".into()),
        };
        let local = FleetLink::of("laptop", &def(FleetTransport::default()));
        assert!(local.is_local());
        assert_eq!(
            argv(&local.command(&["version", "--json"])),
            [
                "fleet",
                "--config",
                "/etc/fleet/a b.toml",
                "version",
                "--json"
            ]
        );
        let remote = FleetLink::of(
            "box",
            &def(FleetTransport::Ssh {
                ssh: "me@box".into(),
                ssh_opts: Vec::new(),
            }),
        );
        assert!(remote.is_ssh() && !remote.is_local());
        assert_eq!(remote.name(), "box");
        let argv = argv(&remote.command(&["version", "--json"]));
        let dest = argv.iter().position(|a| a == "me@box").expect("dest");
        assert_eq!(
            &argv[dest + 1..],
            [
                "fleet",
                "--config",
                "'/etc/fleet/a b.toml'",
                "version",
                "--json"
            ]
        );
    }

    #[test]
    fn an_ssh_call_quotes_each_word_after_the_destination() {
        let link = FleetLink::of(
            "devbox",
            &FleetDef {
                transport: FleetTransport::Ssh {
                    ssh: "me@devbox".into(),
                    ssh_opts: vec!["-p".into(), "2222".into()],
                },
                ..FleetDef::default()
            },
        );
        let argv = argv(&link.command(&["send", "review", "it's done"]));
        assert_eq!(argv[0], "ssh");
        assert_eq!(&argv[1..3], ["-p", "2222"]);
        assert!(argv.iter().any(|a| a == "BatchMode=yes"), "{argv:?}");
        let dest = argv.iter().position(|a| a == "me@devbox").expect("dest");
        assert_eq!(
            &argv[dest + 1..],
            ["fleet", "send", "review", "'it'\\''s done'"]
        );
    }
}
