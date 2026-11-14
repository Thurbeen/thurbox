//! How thurbox reaches a Fleet: the one place a `fleet …` invocation becomes
//! a process (ADR-41). Every call thurbox makes of Fleet's command line —
//! the commands, the watch, the manager, an attach, an automation's fire,
//! `doctor`'s probes — is built here, so a Fleet on another machine is a
//! launcher on the link rather than a change at each call site.
//!
//! The launcher is the runtime's ([`crate::shell::launch`]): each word
//! POSIX-quoted for the remote shell, and ssh hardened (`BatchMode`,
//! `ConnectTimeout`, `ServerAlive*`) and multiplexed (`ControlMaster`). The
//! link adds no flag of its own and changes no stream: stdin, stdout, stderr
//! and the exit status are the remote `fleet`'s, so a JSON answer, a watch's
//! NDJSON, a Task on stdin and an exit code (3 not found, 6 lease held) read
//! the same either way.

use std::io::Write;
use std::process::{Command, Stdio};

use crate::shell::HostLauncher;

/// One Fleet's command line: the `fleet` program, on this machine or behind
/// a launcher.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FleetLink {
    /// `None`: this machine.
    launcher: Option<HostLauncher>,
    /// A name on the `PATH` of the machine Fleet runs on, or a path there.
    program: String,
}

impl FleetLink {
    /// The Fleet `program` runs on this machine.
    pub fn local(program: impl Into<String>) -> Self {
        Self {
            launcher: None,
            program: program.into(),
        }
    }

    /// The Fleet `program` runs on the machine `launcher` reaches.
    pub fn over(launcher: HostLauncher, program: impl Into<String>) -> Self {
        Self {
            launcher: Some(launcher),
            program: program.into(),
        }
    }

    /// The `fleet` program, as configured.
    pub fn program(&self) -> &str {
        &self.program
    }

    /// `fleet <args>`, not yet started. Its streams are the caller's to set,
    /// as for any [`Command`].
    pub fn command(&self, args: &[&str]) -> Command {
        crate::shell::launch(self.launcher.as_ref(), &self.program, args)
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
    use crate::shell::HostLauncher;

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
            let link = FleetLink::over(
                HostLauncher::Ssh {
                    destination: "devbox".into(),
                    ssh_opts: Vec::new(),
                },
                PROGRAM,
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

    /// Over ssh each word is quoted for the remote shell, after the hardened
    /// options and the destination, so free text arrives as one word.
    #[test]
    fn an_ssh_call_quotes_each_word_after_the_destination() {
        let link = FleetLink::over(
            HostLauncher::Ssh {
                destination: "me@devbox".into(),
                ssh_opts: vec!["-p".into(), "2222".into()],
            },
            "fleet",
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
