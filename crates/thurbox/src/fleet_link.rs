//! How thurbox reaches a Fleet: the one place a `fleet …` invocation becomes
//! a process (ADR-41). Every call thurbox makes of Fleet's command line —
//! the commands, the watch, the manager, an attach, an automation's fire,
//! `doctor`'s probes — is built here, so a Fleet on another machine is a
//! launcher on the link rather than a change at each call site.
//!
//! The launcher is the runtime's: over ssh ([`crate::shell::launch`]) each
//! word POSIX-quoted for the remote shell, and ssh hardened (`BatchMode`,
//! `ConnectTimeout`, `ServerAlive*`) and multiplexed (`ControlMaster`);
//! through WSL each word as it is, with `wsl.exe --exec`. The
//! link adds only the Fleet's `--config <path>`, before the verb, and changes
//! no stream: stdin, stdout, stderr
//! and the exit status are the remote `fleet`'s, so a JSON answer, a watch's
//! NDJSON, a Task on stdin and an exit code (3 not found, 6 lease held) read
//! the same either way.

use std::io::Write;
use std::process::{Command, Stdio};

use crate::session::settings::{FleetDef, FleetTransport, IMPLIED_FLEET};
use crate::shell::HostLauncher;

/// One Fleet's command line: the `fleet` program, on this machine or behind
/// a launcher.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FleetLink {
    /// The Fleet's name in `settings.toml`.
    name: String,
    /// `None`: this machine.
    launcher: Option<HostLauncher>,
    /// A name on the `PATH` of the machine Fleet runs on, or a path there.
    program: String,
    /// The config file Fleet reads, given as `--config` when set.
    config: Option<String>,
}

impl FleetLink {
    /// The Fleet `program` runs on this machine, under [`IMPLIED_FLEET`].
    pub fn local(program: impl Into<String>) -> Self {
        Self {
            name: IMPLIED_FLEET.to_string(),
            launcher: None,
            program: program.into(),
            config: None,
        }
    }

    /// The Fleet `settings.toml` lists as `[fleets.<name>]`.
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

    /// The one Fleet the interface uses until it follows several
    /// ([`crate::session::settings::Settings::fleet_in_force`]).
    pub fn in_force(settings: &crate::session::settings::Settings) -> Self {
        let (name, def) = settings.fleet_in_force();
        Self::of(&name, &def)
    }

    /// The Fleet's name in `settings.toml`.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The `fleet` program, as configured.
    pub fn program(&self) -> &str {
        &self.program
    }

    /// The config file Fleet is given with `--config`, if any.
    pub fn config(&self) -> Option<&str> {
        self.config.as_deref()
    }

    /// Whether Fleet runs on this machine.
    pub fn is_local(&self) -> bool {
        self.launcher.is_none()
    }

    /// Whether Fleet is reached over ssh, whose own failure (an unreachable
    /// machine, a refused key) is exit 255.
    pub fn is_ssh(&self) -> bool {
        matches!(self.launcher, Some(HostLauncher::Ssh { .. }))
    }

    /// `fleet [--config <path>] <args>`, not yet started. Its streams are the
    /// caller's to set, as for any [`Command`]. `--config` is a global flag
    /// of Fleet's, so it goes before the verb.
    pub fn command(&self, args: &[&str]) -> Command {
        let mut words: Vec<&str> = Vec::with_capacity(args.len() + 2);
        if let Some(config) = &self.config {
            words.extend(["--config", config.as_str()]);
        }
        words.extend(args);
        self.build(&self.program, &words)
    }

    /// `argv` (a program Fleet named, such as an attach's stream) on the
    /// machine this Fleet runs on, not yet started: through the same
    /// launcher, without the Fleet's `--config`.
    pub fn launch(&self, argv: &[String]) -> Result<Command, String> {
        let (program, args) = argv
            .split_first()
            .ok_or("the stream's argv names no program")?;
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        Ok(self.build(program, &args))
    }

    /// `program` with `args` on the Fleet's machine: the one place a call
    /// becomes a process. Through WSL the words go through `wsl.exe --exec`
    /// as they are: without it `wsl.exe` hands the line to a shell, which
    /// would keep the quotes [`crate::shell::launch`] writes for ssh, so a
    /// path with a space would name another program. Over ssh, and on this
    /// machine, that launch is the rule.
    fn build(&self, program: &str, args: &[&str]) -> Command {
        match &self.launcher {
            Some(launcher @ HostLauncher::Wsl { .. }) => {
                let mut command = launcher.command();
                command.arg("--exec").arg(program).args(args);
                command
            }
            launcher => crate::shell::launch(launcher.as_ref(), program, args),
        }
    }

    /// Run `fleet <args>` with `input` on its stdin (none: stdin is closed),
    /// and answer its error when it exits non-zero: the first `error: ` line
    /// it wrote, else all of its stderr.
    pub fn run(&self, args: &[&str], input: Option<&str>) -> Result<(), String> {
        let verb = args.first().copied().unwrap_or_default();
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

/// Every Fleet thurbox lists, and the one in force: what a command is
/// routed by. A session's id is `<fleet>:<task>`, so a command for it goes to
/// that Fleet; a bare Task name, and a creation, go to the Fleet in force
/// (the only one listed, or the first by name).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fleets {
    in_force: FleetLink,
    all: Vec<FleetLink>,
}

impl Fleets {
    /// `all` the Fleets, with `in_force` named among them. One not among
    /// them is taken as the first, so the set is never without one.
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
        }
    }

    /// The Fleets `settings.toml` lists, or the implied local one.
    pub fn of(settings: &crate::session::settings::Settings) -> Self {
        let all = settings
            .fleets()
            .iter()
            .map(|(name, def)| FleetLink::of(name, def))
            .collect();
        Self::new(&settings.fleet_in_force().0, all)
    }

    /// The Fleet a creation, and a bare Task name, goes to.
    pub fn in_force(&self) -> &FleetLink {
        &self.in_force
    }

    /// Every listed Fleet.
    pub fn all(&self) -> &[FleetLink] {
        &self.all
    }

    /// The Fleet `session` is a session of, and its Task's name there: a
    /// session's id is `<fleet>:<task>`, and a bare name is a Task of the
    /// Fleet in force. A Fleet `settings.toml` does not list is refused by
    /// name, never stood in for by another.
    pub fn route<'a>(&self, session: &'a str) -> Result<(&FleetLink, &'a str), String> {
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
        Self {
            in_force: link.clone(),
            all: vec![link],
        }
    }
}

/// A Fleet "on another machine" for tests: a link over `ssh devbox` whose
/// `ssh` is a stand-in that logs each remote command line and runs it with a
/// scripted `fleet` on the remote `PATH` only. A call that bypassed the link
/// would find no such program on this machine, so a logged call is one the
/// link made.
#[cfg(all(test, unix))]
pub(crate) mod testing {
    use std::ffi::OsString;
    use std::path::{Path, PathBuf};

    use super::FleetLink;
    use crate::session::settings::{FleetDef, FleetTransport};

    /// The program name the stand-in Fleet answers to: one no machine has, so
    /// a call run here rather than through the launcher fails to start.
    pub(crate) const PROGRAM: &str = "thurbox-test-remote-fleet";

    pub(crate) struct Remote {
        dir: tempfile::TempDir,
        pub(crate) link: FleetLink,
    }

    impl Remote {
        /// The remote Fleet runs `script` (POSIX sh, its arguments in `$@`).
        pub(crate) fn new(script: &str) -> Self {
            let dir = tempfile::tempdir().expect("remote dir");
            let bin = dir.path().join("bin");
            let remote = dir.path().join("remote");
            std::fs::create_dir_all(&bin).expect("bin");
            std::fs::create_dir_all(&remote).expect("remote");
            let log = dir.path().join("ssh.log");
            // Options and their values, then the destination, then the
            // command line ssh hands the remote shell, which re-splits it.
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

        /// This machine's `PATH` with the stand-in `ssh` first, for
        /// [`crate::paths::with_path`].
        pub(crate) fn path(&self) -> OsString {
            let mut dirs = vec![self.dir.path().join("bin")];
            dirs.extend(std::env::split_paths(
                &std::env::var_os("PATH").unwrap_or_default(),
            ));
            std::env::join_paths(dirs).expect("PATH")
        }

        /// Each remote command line, as `devbox <program> <args…>`.
        pub(crate) fn calls(&self) -> Vec<String> {
            std::fs::read_to_string(self.dir.path().join("ssh.log"))
                .unwrap_or_default()
                .lines()
                .map(str::to_string)
                .collect()
        }

        /// Whether a call starting `devbox <program> <args>` was made.
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

    fn argv(command: &Command) -> Vec<String> {
        std::iter::once(command.get_program())
            .chain(command.get_args())
            .map(|a| a.to_string_lossy().into_owned())
            .collect()
    }

    /// On this machine the call is the program and its arguments, as they
    /// are: nothing is quoted, since no shell reads them.
    #[test]
    fn a_local_call_is_the_program_and_its_arguments() {
        let link = FleetLink::local("/opt/fleet");
        assert_eq!(
            argv(&link.command(&["send", "review", "it's done"])),
            ["/opt/fleet", "send", "review", "it's done"]
        );
    }

    /// Through WSL every word reaches the distro as it is: `wsl.exe --exec`,
    /// no shell and no quoting, so a path with a space names the program it
    /// says. Over ssh the remote shell re-splits the line, so there each word
    /// is quoted. Every call goes through one of these two: `command` (the
    /// verbs, the watch, `run`) and `launch` (an attach's stream).
    #[test]
    fn a_wsl_fleet_gets_each_word_unquoted_through_exec() {
        let link = FleetLink::of(
            "ubuntu",
            &FleetDef {
                transport: FleetTransport::Wsl {
                    wsl: "Ubuntu".into(),
                },
                program: "/opt/my tools/fleet".into(),
                config: Some("/etc/fleet/a b.toml".into()),
            },
        );
        let after_exec = |argv: Vec<String>| {
            assert_eq!(argv[0], "wsl.exe");
            assert_eq!(&argv[1..3], ["-d", "Ubuntu"]);
            let exec = argv.iter().position(|a| a == "--exec").expect("--exec");
            argv[exec + 1..].to_vec()
        };
        assert_eq!(
            after_exec(argv(&link.command(&["send", "review", "it's done"]))),
            [
                "/opt/my tools/fleet",
                "--config",
                "/etc/fleet/a b.toml",
                "send",
                "review",
                "it's done"
            ]
        );
        let stream = link
            .launch(&[
                "/srv/my apps/runtime-mux".into(),
                "view".into(),
                "w 1".into(),
            ])
            .expect("a stream");
        assert_eq!(
            after_exec(argv(&stream)),
            ["/srv/my apps/runtime-mux", "view", "w 1"]
        );
    }

    /// A listed Fleet's config goes before the verb, as Fleet's global flag,
    /// on this machine and over ssh alike.
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

    /// Over ssh each word is quoted for the remote shell, after the hardened
    /// options and the destination, so free text arrives as one word.
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
