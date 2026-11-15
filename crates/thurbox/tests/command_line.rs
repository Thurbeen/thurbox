//! The `thurbox` binary's command line, run as a new user runs it: in an
//! empty home with no config, and with a stand-in `fleet` on `PATH` that
//! records every call. Answering a flag, or refusing to start without a
//! terminal, must leave that home as it found it and never start Fleet.
#![cfg(unix)]

use std::os::fd::{FromRawFd, OwnedFd};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

/// An empty home, its XDG directories inside it, and a `bin` holding the
/// recording `fleet`.
struct Sandbox {
    root: tempfile::TempDir,
}

impl Sandbox {
    fn new() -> Self {
        let root = tempfile::tempdir().expect("tempdir");
        let bin = root.path().join("bin");
        std::fs::create_dir_all(&bin).expect("bin");
        std::fs::create_dir_all(root.path().join("home")).expect("home");
        let fleet = bin.join("fleet");
        std::fs::write(
            &fleet,
            format!(
                "#!/bin/sh\necho \"$*\" >> '{}'\nexit 1\n",
                root.path().join("fleet-calls").display()
            ),
        )
        .expect("fleet stand-in");
        std::fs::set_permissions(&fleet, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        Self { root }
    }

    fn home(&self) -> PathBuf {
        self.root.path().join("home")
    }

    /// `thurbox args`, stdin from nowhere and stdout to a pipe: no terminal.
    fn run(&self, args: &[&str]) -> Output {
        self.run_on(args, Stdio::null(), Stdio::piped())
    }

    /// `thurbox args` with the given stdin and stdout; stderr is captured.
    fn run_on(&self, args: &[&str], stdin: Stdio, stdout: Stdio) -> Output {
        let home = self.home();
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_thurbox"));
        cmd.args(args)
            .current_dir(self.root.path())
            .stdin(stdin)
            .stdout(stdout)
            .stderr(Stdio::piped())
            .env("HOME", &home)
            .env("XDG_CONFIG_HOME", home.join(".config"))
            .env("XDG_DATA_HOME", home.join(".local/share"))
            .env("XDG_STATE_HOME", home.join(".local/state"))
            .env("XDG_CACHE_HOME", home.join(".cache"))
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    self.root.path().join("bin").display(),
                    std::env::var("PATH").unwrap_or_default()
                ),
            );
        for var in [
            "THURBOX_CONFIG_DIR",
            "THURBOX_DATA_DIR",
            "THURBOX_SOCKET",
            "THURBOX_SOCKET_FOR",
            "THURBOX_SESSION",
            "FLEET_CONFIG",
            "TMUX",
            "TMUX_TMPDIR",
        ] {
            cmd.env_remove(var);
        }
        cmd.output().expect("run thurbox")
    }

    /// Nothing written in the home, and Fleet never run.
    fn assert_untouched(&self) {
        let written = files(&self.home());
        assert!(written.is_empty(), "thurbox wrote {written:?}");
        let calls = std::fs::read_to_string(self.root.path().join("fleet-calls"));
        assert!(calls.is_err(), "thurbox ran fleet: {calls:?}");
    }
}

/// A pseudo-terminal: the controller, kept open while the test runs, and the
/// terminal side handed to thurbox.
fn pty() -> (OwnedFd, OwnedFd) {
    let (mut controller, mut terminal) = (-1, -1);
    // SAFETY: openpty writes two valid descriptors into the out-params on
    // success; the name, termios and winsize pointers may be null.
    let rc = unsafe {
        libc::openpty(
            &mut controller,
            &mut terminal,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    assert_eq!(rc, 0, "openpty: {}", std::io::Error::last_os_error());
    // SAFETY: both descriptors were just returned by openpty and nobody else
    // owns them.
    unsafe {
        (
            OwnedFd::from_raw_fd(controller),
            OwnedFd::from_raw_fd(terminal),
        )
    }
}

fn files(dir: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let path = entry.path();
        if path.is_dir() {
            found.extend(files(&path));
        }
        found.push(path);
    }
    found
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

#[test]
fn version_prints_and_exits() {
    let sandbox = Sandbox::new();
    let out = sandbox.run(&["--version"]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert!(
        text(&out.stdout).starts_with("thurbox "),
        "{}",
        text(&out.stdout)
    );
    sandbox.assert_untouched();
}

#[test]
fn help_prints_and_exits() {
    let sandbox = Sandbox::new();
    let out = sandbox.run(&["--help"]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    let help = text(&out.stdout);
    assert!(help.contains("Usage: thurbox"), "{help}");
    assert!(
        help.contains("thurbox-cli"),
        "help points scripts at thurbox-cli: {help}"
    );
    sandbox.assert_untouched();
}

#[test]
fn an_unknown_flag_is_refused_with_a_usage_line() {
    let sandbox = Sandbox::new();
    let out = sandbox.run(&["--no-such-flag"]);
    assert_eq!(out.status.code(), Some(2), "{}", text(&out.stderr));
    assert!(
        text(&out.stderr).contains("Usage: thurbox"),
        "{}",
        text(&out.stderr)
    );
    sandbox.assert_untouched();
}

#[test]
fn without_a_terminal_thurbox_exits_before_it_writes_or_starts_fleet() {
    let sandbox = Sandbox::new();
    let out = sandbox.run(&[]);
    assert_eq!(out.status.code(), Some(1), "{}", text(&out.stderr));
    let stderr = text(&out.stderr);
    assert!(stderr.contains("terminal"), "{stderr}");
    assert!(stderr.contains("thurbox-cli"), "{stderr}");
    sandbox.assert_untouched();
}

/// A terminal on stdin alone is not enough: the interface draws on stdout.
#[test]
fn a_terminal_on_stdin_alone_is_refused() {
    let sandbox = Sandbox::new();
    let (_controller, terminal) = pty();
    let out = sandbox.run_on(&[], Stdio::from(terminal), Stdio::piped());
    assert_eq!(out.status.code(), Some(1), "{}", text(&out.stderr));
    assert!(
        text(&out.stderr).contains("terminal"),
        "{}",
        text(&out.stderr)
    );
    sandbox.assert_untouched();
}

/// A terminal on stdout alone is not enough: the interface reads keys from
/// stdin.
#[test]
fn a_terminal_on_stdout_alone_is_refused() {
    let sandbox = Sandbox::new();
    let (_controller, terminal) = pty();
    let out = sandbox.run_on(&[], Stdio::null(), Stdio::from(terminal));
    assert_eq!(out.status.code(), Some(1), "{}", text(&out.stderr));
    assert!(
        text(&out.stderr).contains("terminal"),
        "{}",
        text(&out.stderr)
    );
    sandbox.assert_untouched();
}
