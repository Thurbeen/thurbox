//! The real `thurbox` binary on a real pseudo-terminal.
//!
//! Every other test in the suite renders to a `TestBackend`, which by design
//! never touches a tty — so none of them can see what the binary actually
//! writes: the alternate-screen enter and leave, the mouse-reporting modes, a
//! screen clear that blinks the whole interface, or how the loop behaves when
//! the window is resized under it. The regressions that hurt most live there —
//! a shell left streaming mouse reports, a closed column leaving its border
//! behind, a chord that opened a strip and then typed into the wrong pane —
//! and each one was a coordinator bug, in the loop `main.rs` owns and nothing
//! in-process can drive. This file is where those are asserted.
//!
//! The byte stream is kept twice: verbatim, for the escape sequences, and fed
//! through the same `vt100` the render path uses, for the frame. Assertions on
//! the frame survive any interleaving of diff repaints; assertions on the bytes
//! are the ones nothing else can make.
//!
//! Hermetic: private HOME, config, data and tmux dirs per test, and the
//! network-facing and tmux-arming features off, so a run never touches a real
//! profile or a real tmux server. The scenarios that need a multiplexer (the
//! ones built on `shell_session`) skip where tmux is absent, as
//! `tests/create_e2e.rs` does — a missing multiplexer is an environment fact,
//! not a regression.
//!
//! Unix-only, and on `libc` directly: the PTY is `openpty` + `setsid` +
//! `TIOCSCTTY` + `TIOCSWINSZ`, four calls that are already in the dependency
//! tree, and the Windows ConPTY path is exercised by the windows-vm e2e harness.
#![cfg(unix)]

use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// The guard every tmux server in this file is reaped by — see its own doc.
#[path = "../../runtime-mux/tests/support/tmux_server.rs"]
mod tmux_server;

use tmux_server::TmuxServer;

#[path = "support/otlp_collector.rs"]
mod otlp_collector;

/// How long a frame is given to show something before the test gives up.
/// Generous because a cold CI runner pays for the first paint with the Lua
/// interface load and the SQLite open.
const WAIT: Duration = Duration::from_secs(20);

/// The bytes a terminal sends for the chords the scenarios press.
const CTRL_P: &[u8] = b"\x10";
const CTRL_Q: &[u8] = b"\x11";
const CTRL_Y: &[u8] = b"\x19";
/// What a legacy terminal sends for `ctrl+/` (the search plugin folds
/// `ctrl+/`, `ctrl+7` and `ctrl+_` into one chord).
const CTRL_SLASH: &[u8] = b"\x1f";
const ESC: &[u8] = b"\x1b";
const F1: &[u8] = b"\x1bOP";
const F12: &[u8] = b"\x1b[24~";
const F6: &[u8] = b"\x1b[17~";
const F10: &[u8] = b"\x1b[21~";
const F9: &[u8] = b"\x1b[20~";

/// The `GIT_*` location variables git exports to hook processes — the list
/// `git::GIT_LOCATION_ENV` scrubs, which is crate-private. A suite running
/// under this repository's own pre-commit hook inherits a `GIT_DIR` pointing
/// at the real repository, so every process here drops them.
const GIT_LOCATION_ENV: [&str; 8] = [
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_INDEX_FILE",
    "GIT_COMMON_DIR",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_PREFIX",
    "GIT_NAMESPACE",
];

/// A tmux socket name unique to this process, so parallel tests — and a
/// developer's own `thurbox-dev` server — never share one.
fn private_socket() -> String {
    format!("thurbox-e2e-{}", std::process::id())
}

fn have_tmux() -> bool {
    Command::new("tmux")
        .arg("-V")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// The isolated profile a scenario runs in: every directory the binary reads
/// or writes, under one tempdir that goes away with the test — except the
/// multiplexer's socket directory, which has to be short.
struct Profile {
    root: tempfile::TempDir,
    /// A directory at the front of `PATH`. Empty unless a scenario drops a
    /// stand-in for a binary the real one resolves there — see `fake_ssh`.
    bin: PathBuf,
    /// The scenario's own multiplexer server. A guard: whatever the scenario
    /// does — return, assert, panic on a pty that stopped answering — dropping
    /// it kills the server and takes its socket directory with it.
    server: TmuxServer,
}

impl Profile {
    fn new() -> Self {
        let root = tempfile::tempdir().expect("tempdir");
        for sub in ["home", "config", "data", "bin"] {
            std::fs::create_dir_all(root.path().join(sub)).expect("mkdir");
        }
        // No update check, no version check (both reach the network), and no
        // automation heartbeat (it would arm a tmux keeper window on startup).
        std::fs::write(
            root.path().join("config/settings.toml"),
            "[features]\nautomations = false\nversion_check = false\nauto_update = false\n",
        )
        .expect("seed settings");
        let bin = root.path().join("bin");
        // Fleet is the engine every interface starts (`fleet watch`, `fleet
        // up`), found on `PATH` as `fleet`. This one has no Tasks and no
        // manager to start, so a profile never reaches a real store; the
        // watch ends with the interface that started it.
        write_script(
            &bin.join("fleet"),
            "#!/bin/sh\ncase \"$1\" in\n  watch) while kill -0 $PPID 2>/dev/null; do sleep 1; done ;;\n  up) exit 6 ;;\n  *) echo \"error: no Fleet in this profile\" >&2; exit 1 ;;\nesac\n",
        );
        Self {
            root,
            bin,
            server: TmuxServer::private(&private_socket()),
        }
    }

    /// Put a real Fleet in front of this profile: its own store and config,
    /// the multiplexer runtime on this profile's tmux directory (reaped with
    /// the server guard), and `claude` running `sh`, so a Task's session is a
    /// shell. `None`, with the reason printed, where the binaries are absent.
    fn real_fleet(&self) -> Option<()> {
        let dir = Path::new(env!("CARGO_BIN_EXE_thurbox-cli")).parent()?;
        let (fleet, runtime) = (dir.join("fleet"), dir.join("runtime-mux"));
        if !fleet.is_file() || !runtime.is_file() {
            eprintln!("skipping: build fleet and runtime-mux (`cargo build --workspace`)");
            return None;
        }
        let root = self.root.path();
        std::fs::create_dir_all(root.join("fleet-tmp")).expect("fleet tmp");
        std::fs::write(
            root.join("fleet.toml"),
            format!(
                "version = 1\n\n[data]\ndir = \"fleet-data\"\n\n[runtimes.mux]\nprogram = '{}'\ncallTimeoutMs = 10000\n\n[hosts.local]\nruntime = \"mux\"\n",
                runtime.display()
            ),
        )
        .expect("fleet config");
        write_script(
            &self.bin.join("fleet"),
            &format!(
                "#!/bin/sh\nexec env -i HOME='{r}/home' TMPDIR='{r}/fleet-tmp' PATH='{b}:/usr/bin:/bin' TERM=xterm-256color TMUX_TMPDIR='{t}' '{f}' --config '{r}/fleet.toml' \"$@\"\n",
                r = root.display(),
                b = self.bin.display(),
                t = self.server.tmpdir().display(),
                f = fleet.display(),
            ),
        );
        write_script(&self.bin.join("claude"), "#!/bin/sh\nexec sh\n");
        Some(())
    }

    /// Run this profile's Fleet with `input` on stdin; it must succeed. Its
    /// stdout.
    fn fleet(&self, args: &[&str], input: &str) -> String {
        let mut child = Command::new(self.bin.join("fleet"))
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("run fleet");
        child
            .stdin
            .take()
            .expect("stdin")
            .write_all(input.as_bytes())
            .expect("write");
        let output = child.wait_with_output().expect("fleet");
        assert!(
            output.status.success(),
            "fleet {args:?} failed:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).into_owned()
    }

    /// The Task `name` as Fleet stores it.
    fn fleet_task(&self, name: &str) -> serde_json::Value {
        serde_json::from_str(&self.fleet(&["get", "task", name], "")).expect("a Task as JSON")
    }

    /// A dispatched, interactive Task named `name` working in `dir`, its
    /// session started.
    fn fleet_session(&self, name: &str, dir: &Path) {
        let task = format!(
            r#"{{"apiVersion":"fleet/v1alpha1","kind":"Task","metadata":{{"name":"{name}"}},"spec":{{"harness":"claude-code","host":"local","dispatch":true,"workspace":{{"dir":"{}"}}}}}}"#,
            dir.display()
        );
        self.fleet(&["apply", "-f", "-"], &task);
        self.fleet(&["reconcile"], "");
    }

    fn path(&self, sub: &str) -> PathBuf {
        self.root.path().join(sub)
    }

    /// The environment both binaries need to land in this profile and on its
    /// private multiplexer socket.
    fn apply(&self, cmd: &mut Command) {
        cmd.current_dir(self.root.path());
        cmd.env("HOME", self.path("home"));
        cmd.env("THURBOX_CONFIG_DIR", self.path("config"));
        cmd.env("THURBOX_DATA_DIR", self.path("data"));
        // Pinned socket, cleared owner tag, private socket directory. Run from
        // inside a thurbox pane, an inherited owner would make the pin read as
        // inherited and put the server on a derived socket the guard never
        // names.
        self.server.scope(cmd);
        // `bin` first, so a stand-in dropped there shadows the real binary for
        // every process this profile launches — the TUI and `thurbox-cli` both,
        // which is what a scenario that stubs `ssh` needs (the session is
        // created by one and attached by the other).
        cmd.env(
            "PATH",
            format!(
                "{}:{}",
                self.bin.display(),
                std::env::var("PATH").unwrap_or_default()
            ),
        );
        cmd.env("TERM", "xterm-256color");
        // Keep an explicit INPUTRC from bypassing the profile's private HOME.
        cmd.env_remove("INPUTRC");
        // These tests assert terminal colours and reverse video. An inherited
        // NO_COLOR makes Crossterm omit colour escapes and reset attributes.
        cmd.env_remove("NO_COLOR");
        // A test run inside tmux must not look like one to the binary.
        cmd.env_remove("TMUX");
        // Git exports these to hook processes, so a suite running under this
        // repository's own pre-commit hook would otherwise point every spawn
        // at the real repository.
        for var in GIT_LOCATION_ENV {
            cmd.env_remove(var);
        }
    }
}

/// A pseudo-terminal pair at the given size.
fn openpty(rows: u16, cols: u16) -> (OwnedFd, OwnedFd) {
    let mut master = -1;
    let mut slave = -1;
    let mut size = libc::winsize {
        ws_row: rows,
        ws_col: cols,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    // Apple's libc declares openpty's termios and winsize arguments `*mut`,
    // Linux's `*const`; a `*mut` coerces to either, and a named pointer is
    // what keeps clippy from reading the `&mut` as an unnecessary one on the
    // `*const` side.
    let winsize: *mut libc::winsize = &mut size;
    // SAFETY: openpty writes two valid descriptors into the out-params on
    // success; the name and termios pointers are allowed to be null.
    let rc = unsafe {
        libc::openpty(
            &mut master,
            &mut slave,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            winsize,
        )
    };
    assert_eq!(rc, 0, "openpty failed: {}", std::io::Error::last_os_error());
    // SAFETY: both descriptors were just returned by openpty and are owned by
    // nobody else.
    unsafe { (OwnedFd::from_raw_fd(master), OwnedFd::from_raw_fd(slave)) }
}

/// Device-attribute replies and, on kitty terminals, a flag stack per screen.
///
/// Per screen, as kitty keeps them: flags pushed on the alternate screen are
/// not popped by a pop written after leaving it, which is the order mistake
/// this exists to catch.
#[derive(Default)]
struct TestTerminal {
    kitty: bool,
    /// How far into the output the sequences have been read.
    scanned: usize,
    main: Vec<u32>,
    alternate: Vec<u32>,
    on_alternate: bool,
}

impl TestTerminal {
    fn stack(&mut self) -> &mut Vec<u32> {
        if self.on_alternate {
            &mut self.alternate
        } else {
            &mut self.main
        }
    }

    /// How many flag levels are pushed on the main and the alternate screen.
    fn depths(&self) -> (usize, usize) {
        (self.main.len(), self.alternate.len())
    }

    /// Read the output from where the last call stopped and return the bytes
    /// a terminal would answer with. A sequence split across two reads is
    /// left for the next call.
    fn feed(&mut self, out: &[u8]) -> Vec<u8> {
        let mut reply = Vec::new();
        while let Some(start) = out[self.scanned..]
            .windows(2)
            .position(|w| w == b"\x1b[")
            .map(|at| self.scanned + at)
        {
            let body = &out[start + 2..];
            let Some(end) = body.iter().position(|b| (0x40..=0x7e).contains(b)) else {
                // Incomplete: wait for the rest.
                self.scanned = start;
                return reply;
            };
            let (params, last) = (&body[..end], body[end]);
            match (params.first(), last) {
                (Some(b'?'), b'u') if params.len() == 1 && self.kitty => {
                    let flags = self.stack().last().copied().unwrap_or(0);
                    reply.extend_from_slice(format!("\x1b[?{flags}u").as_bytes());
                }
                (Some(b'>'), b'u') => {
                    let flags = std::str::from_utf8(&params[1..])
                        .ok()
                        .and_then(|n| n.parse().ok())
                        .unwrap_or(0);
                    self.stack().push(flags);
                }
                (Some(b'<'), b'u') => {
                    self.stack().pop();
                }
                (Some(b'?'), b'h') if params == b"?1049" => self.on_alternate = true,
                (Some(b'?'), b'l') if params == b"?1049" => self.on_alternate = false,
                (None, b'c') => reply.extend_from_slice(b"\x1b[?62;22c"),
                _ => {}
            }
            self.scanned = start + 2 + end + 1;
        }
        // Keep a trailing ESC for the next read, in case its `[` follows.
        self.scanned = out.len().saturating_sub(1).max(self.scanned);
        reply
    }
}

enum TerminalMode {
    Silent,
    Legacy,
    Kitty,
}

/// The binary, running on a pty, with everything it has written so far.
struct Tui {
    child: Child,
    master: OwnedFd,
    /// Every byte the binary wrote, verbatim — the escape-sequence record.
    raw: Arc<Mutex<Vec<u8>>>,
    /// Successful terminal replies, separate from the application's output.
    replies: Arc<Mutex<Vec<u8>>>,
    reply_error: Arc<Mutex<Option<std::io::Error>>>,
    /// The same bytes through vt100, for asserting on the visible frame.
    screen: Arc<Mutex<vt100::Parser>>,
    /// The exit status, once seen: `try_wait` reaps, so it is read once.
    exited: Option<ExitStatus>,
    /// The binary's own log, quoted when a wait times out.
    log: PathBuf,
    /// The kitty-protocol half of the terminal, on one that speaks it.
    kitty: Option<Arc<Mutex<TestTerminal>>>,
}

impl Tui {
    /// Launch the binary in `profile` on a `rows`×`cols` terminal.
    fn spawn(profile: &Profile, rows: u16, cols: u16) -> Self {
        Self::spawn_with(profile, rows, cols, |_| {})
    }

    fn spawn_with(
        profile: &Profile,
        rows: u16,
        cols: u16,
        adjust: impl FnOnce(&mut Command),
    ) -> Self {
        Self::spawn_on(profile, rows, cols, adjust, TerminalMode::Legacy)
    }

    /// [`Self::spawn_with`] on a terminal that speaks the kitty keyboard
    /// protocol: it answers the flags query with the flags pushed so far and
    /// the device-attributes query after it, which is how crossterm detects
    /// support. Legacy terminals answer only the device-attributes query; the
    /// silent-terminal scenario covers a query that times out.
    fn spawn_kitty(
        profile: &Profile,
        rows: u16,
        cols: u16,
        adjust: impl FnOnce(&mut Command),
    ) -> Self {
        Self::spawn_on(profile, rows, cols, adjust, TerminalMode::Kitty)
    }

    fn spawn_on(
        profile: &Profile,
        rows: u16,
        cols: u16,
        adjust: impl FnOnce(&mut Command),
        mode: TerminalMode,
    ) -> Self {
        let (master, slave) = openpty(rows, cols);
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_thurbox"));
        profile.apply(&mut cmd);
        adjust(&mut cmd);
        cmd.stdin(Stdio::from(slave.try_clone().expect("dup slave")));
        cmd.stdout(Stdio::from(slave.try_clone().expect("dup slave")));
        cmd.stderr(Stdio::from(slave));
        // SAFETY: only async-signal-safe calls between fork and exec — a new
        // session, and the slave (now fd 0) made its controlling terminal so
        // the child sees SIGWINCH and `isatty` answers yes.
        unsafe {
            cmd.pre_exec(|| {
                if libc::setsid() < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                if libc::ioctl(0, libc::TIOCSCTTY as _, 0) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let child = cmd.spawn().expect("spawn thurbox");

        let raw = Arc::new(Mutex::new(Vec::new()));
        let replies = Arc::new(Mutex::new(Vec::new()));
        let reply_error = Arc::new(Mutex::new(None));
        let screen = Arc::new(Mutex::new(vt100::Parser::new(
            rows.try_into().unwrap(),
            cols.try_into().unwrap(),
            0,
        )));
        let mut reader = std::fs::File::from(master.try_clone().expect("dup master"));
        let terminal = Arc::new(Mutex::new(TestTerminal {
            kitty: matches!(mode, TerminalMode::Kitty),
            ..TestTerminal::default()
        }));
        let kitty = matches!(mode, TerminalMode::Kitty).then(|| Arc::clone(&terminal));
        let mut answer = (!matches!(mode, TerminalMode::Silent)).then(|| {
            (
                terminal,
                std::fs::File::from(master.try_clone().expect("dup master")),
            )
        });
        {
            let raw = Arc::clone(&raw);
            let replies = Arc::clone(&replies);
            let reply_error = Arc::clone(&reply_error);
            let screen = Arc::clone(&screen);
            // Reads until EIO, which is how a pty reports the child gone.
            std::thread::spawn(move || {
                let mut buf = [0u8; 4096];
                while let Ok(n) = reader.read(&mut buf) {
                    if n == 0 {
                        break;
                    }
                    let mut raw = raw.lock().unwrap();
                    raw.extend_from_slice(&buf[..n]);
                    if let Some((terminal, writer)) = answer.as_mut() {
                        let reply = terminal.lock().unwrap().feed(&raw);
                        if !reply.is_empty() {
                            if let Err(error) =
                                writer.write_all(&reply).and_then(|()| writer.flush())
                            {
                                *reply_error.lock().unwrap() = Some(error);
                                break;
                            }
                            replies.lock().unwrap().extend_from_slice(&reply);
                        }
                    }
                    drop(raw);
                    screen.lock().unwrap().process(&buf[..n]);
                }
            });
        }
        Self {
            child,
            master,
            raw,
            replies,
            reply_error,
            screen,
            exited: None,
            log: profile.path("data/thurbox.log"),
            kitty,
        }
    }

    /// On a kitty terminal, wait for every pushed keyboard-flag level to have
    /// been popped again — a level left behind keeps the shell receiving an
    /// escape code for every key release.
    fn assert_keyboard_flags_popped(&self) {
        let kitty = self.kitty.as_ref().expect("a kitty terminal");
        let deadline = Instant::now() + WAIT;
        while Instant::now() < deadline {
            if kitty.lock().unwrap().depths() == (0, 0) {
                return;
            }
            std::thread::sleep(Duration::from_millis(40));
        }
        panic!(
            "keyboard flags still pushed after exit (main, alternate): {:?}",
            kitty.lock().unwrap().depths()
        );
    }

    /// The frame as vt100 reconstructs it, rows trimmed of trailing blanks.
    fn frame(&self) -> String {
        let error = self.reply_error.lock().unwrap();
        assert!(error.is_none(), "terminal reply failed: {error:?}");
        self.screen.lock().unwrap().screen().contents()
    }

    /// One row of the frame, untrimmed, so a column position means something.
    fn row(&self, y: u16) -> String {
        let screen = self.screen.lock().unwrap();
        let screen = screen.screen();
        (0..screen.size().1.get())
            .map(|x| {
                screen
                    .cell(y, x)
                    .map(|cell| cell.contents())
                    .unwrap_or_default()
            })
            .collect()
    }

    /// Whether the cell at `(y, x)` is drawn reversed — how the kernel marks a
    /// surface row.
    fn inverse_at(&self, y: u16, x: u16) -> bool {
        self.screen
            .lock()
            .unwrap()
            .screen()
            .cell(y, x)
            .is_some_and(vt100::Cell::inverse)
    }

    fn raw_len(&self) -> usize {
        self.raw.lock().unwrap().len()
    }

    /// The bytes written from `since` on, lossily decoded for `contains`.
    fn raw_since(&self, since: usize) -> String {
        String::from_utf8_lossy(&self.raw.lock().unwrap()[since..]).into_owned()
    }

    fn send(&mut self, bytes: &[u8]) {
        let mut writer = std::fs::File::from(self.master.try_clone().expect("dup master"));
        writer.write_all(bytes).expect("write to pty");
        writer.flush().expect("flush pty");
    }

    /// Resize the terminal; the kernel raises SIGWINCH in the child for us.
    fn resize(&mut self, rows: u16, cols: u16) {
        let size = libc::winsize {
            ws_row: rows,
            ws_col: cols,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        // SAFETY: TIOCSWINSZ reads one winsize through a valid pointer.
        let rc = unsafe { libc::ioctl(self.master.as_raw_fd(), libc::TIOCSWINSZ, &size) };
        assert_eq!(
            rc,
            0,
            "TIOCSWINSZ failed: {}",
            std::io::Error::last_os_error()
        );
        self.screen
            .lock()
            .unwrap()
            .screen_mut()
            .set_size(rows.try_into().unwrap(), cols.try_into().unwrap());
    }

    /// Poll the frame until `needle` shows up.
    fn wait_for(&self, needle: &str) {
        self.wait_until(&format!("{needle:?} to appear"), |frame| {
            frame.contains(needle)
        });
    }

    /// The inverse, needed after an Escape: the next chord must not be sent
    /// while the overlay is still up, or `ESC` + its first byte reads as one
    /// alt-prefixed sequence and the chord is swallowed.
    fn wait_gone(&self, needle: &str) {
        self.wait_until(&format!("{needle:?} to disappear"), |frame| {
            !frame.contains(needle)
        });
    }

    fn wait_until(&self, what: &str, done: impl Fn(&str) -> bool) {
        self.wait_within(WAIT, what, done);
    }

    /// [`Self::wait_until`] on a budget of the caller's choosing — for the
    /// scenarios where *how long* is the assertion rather than the setup.
    fn wait_within(&self, budget: Duration, what: &str, done: impl Fn(&str) -> bool) {
        let deadline = Instant::now() + budget;
        while Instant::now() < deadline {
            if done(&self.frame()) {
                return;
            }
            std::thread::sleep(Duration::from_millis(40));
        }
        self.give_up(what);
    }

    /// The failure every timeout reports: what was waited for, the frame as it
    /// stands, and the binary's own log — where an attach or spawn failure is
    /// written, since stdout is the interface's.
    fn give_up(&self, what: &str) -> ! {
        panic!(
            "timed out waiting for {what}; final frame:\n{}\n--- thurbox.log ---\n{}",
            self.frame(),
            self.log_tail()
        );
    }

    /// The last lines of the binary's log, for a failure message.
    fn log_tail(&self) -> String {
        let text = self.log_text();
        let lines: Vec<&str> = text.lines().collect();
        lines[lines.len().saturating_sub(30)..].join("\n")
    }

    /// The binary's whole log.
    fn log_text(&self) -> String {
        // The appender rolls daily, so the file carries a date suffix.
        let dir = self.log.parent().expect("log dir");
        let stem = self
            .log
            .file_name()
            .expect("log name")
            .to_string_lossy()
            .into_owned();
        std::fs::read_dir(dir)
            .ok()
            .into_iter()
            .flatten()
            .flatten()
            .filter(|entry| entry.file_name().to_string_lossy().starts_with(&stem))
            .filter_map(|entry| std::fs::read_to_string(entry.path()).ok())
            .collect()
    }

    fn poll_exit(&mut self) -> Option<ExitStatus> {
        if self.exited.is_none() {
            self.exited = self.child.try_wait().expect("try_wait");
        }
        self.exited
    }

    fn alive(&mut self) -> bool {
        self.poll_exit().is_none()
    }

    /// Press `Ctrl+Q` and wait for the process to go; the exit status is the
    /// caller's to judge.
    fn quit(&mut self) -> ExitStatus {
        self.send(CTRL_Q);
        self.wait_exit("Ctrl+Q")
    }

    /// Send `signal` to the binary and wait for it to go; the exit status is
    /// the caller's to judge.
    fn signal(&mut self, signal: libc::c_int) -> ExitStatus {
        // SAFETY: a plain `kill(2)` on a pid this harness spawned and has not
        // yet reaped (`poll_exit` is the only reaper, and `exited` is `None`).
        let sent = unsafe { libc::kill(self.child.id() as libc::pid_t, signal) };
        assert_eq!(
            sent,
            0,
            "kill({signal}) failed: {}",
            std::io::Error::last_os_error()
        );
        self.wait_exit(&format!("signal {signal}"))
    }

    fn wait_exit(&mut self, after: &str) -> ExitStatus {
        let deadline = Instant::now() + WAIT;
        while Instant::now() < deadline {
            if let Some(status) = self.poll_exit() {
                return status;
            }
            std::thread::sleep(Duration::from_millis(40));
        }
        self.give_up(&format!("the process to exit after {after}"));
    }
}

impl Drop for Tui {
    fn drop(&mut self) {
        // A test that panicked mid-scenario must not leave the binary running
        // on a pty nobody reads.
        if self.alive() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

// --- the boot frame, and giving the terminal back --------------------------

#[test]
fn boots_paints_and_quits_restoring_the_terminal() {
    let profile = Profile::new();
    let mut tui = Tui::spawn(&profile, 24, 80);
    tui.wait_for("No sessions yet");

    let raw = tui.raw_since(0);
    assert!(
        raw.contains("\x1b[?1049h"),
        "boot must take the alternate screen"
    );
    assert!(
        raw.contains("\x1b[?1000h"),
        "boot must ask the terminal for mouse reports"
    );

    let status = tui.quit();
    assert!(status.success(), "Ctrl+Q must exit cleanly: {status:?}");
    assert_terminal_restored(&tui.raw_since(0), "a clean exit");
}

/// What every exit owes the terminal. A missing one of these is the "my shell
/// is streaming mouse reports" bug, which no in-process test and no
/// capture-pane assertion can see.
const RESTORE_ESCAPES: [(&str, &str); 5] = [
    ("\x1b[?1049l", "leave the alternate screen"),
    ("\x1b[?1000l", "stop mouse reporting"),
    ("\x1b[?1003l", "stop mouse motion reporting"),
    ("\x1b[?2004l", "disable bracketed paste"),
    ("\x1b[?25h", "show the cursor again"),
];

fn assert_terminal_restored(raw: &str, exit: &str) {
    for (seq, meaning) in RESTORE_ESCAPES {
        assert!(
            raw.contains(seq),
            "{exit} must {meaning} ({seq:?} missing from the byte stream)"
        );
    }
}

#[test]
fn a_signal_restores_the_terminal_before_exiting() {
    let profile = Profile::new();
    let mut tui = Tui::spawn(&profile, 24, 80);
    tui.wait_for("No sessions yet");
    let taken = tui.raw_len();

    // What a session manager, a closed ssh connection or a machine waking from
    // a long sleep sends. The default action runs no hook, which is how the
    // shell that came next was left printing `\x1b[<64;…M` on every scroll.
    let status = tui.signal(libc::SIGTERM);
    assert!(
        !status.success(),
        "a signalled exit must not pass for a clean one: {status:?}"
    );
    assert_eq!(
        status.code(),
        Some(128 + libc::SIGTERM),
        "exit status follows the shell's 128 + signal convention: {status:?}"
    );

    // Only the bytes written AFTER the boot count, so a `…l` from setup could
    // not satisfy this.
    assert_terminal_restored(&tui.raw_since(taken), "a signalled exit");
}

// --- the event loop never waits on the database ----------------------------

// --- diagnostic logs over OTLP ---------------------------------------------

/// `export_diagnostic_logs` bridges the interface's own `tracing` events to the
/// collector, at WARN and above only, while the file log keeps everything
/// (ADR-38, issue #1465 phase 3). The WARN comes from the interface's first
/// read of an `agents.toml` with a key it does not know.
#[test]
fn diagnostic_logs_reach_the_collector_at_warn_and_above_only() {
    use opentelemetry_proto::tonic::common::v1::any_value::Value;
    use opentelemetry_proto::tonic::logs::v1::SeverityNumber;

    let collector = otlp_collector::Collector::start();
    let profile = Profile::new();
    let settings = profile.path("config/settings.toml");
    let mut seeded = std::fs::read_to_string(&settings).expect("read settings");
    seeded.push_str(&format!(
        "\n[telemetry]\nenabled = true\nendpoint = \"{}\"\nexport_diagnostic_logs = true\n",
        collector.endpoint()
    ));
    std::fs::write(&settings, seeded).expect("write settings");
    std::fs::write(
        profile.path("config/agents.toml"),
        "config_version = 1\ndefault = \"claude\"\nstray_key = 1\n\n\
         [[agents]]\nname = \"claude\"\ncommand = \"claude\"\n",
    )
    .expect("write agents.toml");

    let mut tui = Tui::spawn_with(&profile, 24, 80, |cmd| {
        // A suite run inside a session with [telemetry] on inherits these.
        for (key, _) in std::env::vars() {
            if key.starts_with("OTEL_") {
                cmd.env_remove(key);
            }
        }
    });
    tui.wait_for("No sessions yet");

    let body = |record: &opentelemetry_proto::tonic::logs::v1::LogRecord| match record
        .body
        .as_ref()
        .and_then(|b| b.value.as_ref())
    {
        Some(Value::StringValue(s)) => s.clone(),
        other => format!("{other:?}"),
    };
    // Sent by the batch processor while the interface runs, not only at exit.
    let mut records = Vec::new();
    let deadline = Instant::now() + WAIT;
    while !records
        .iter()
        .any(|r| body(r).contains("unknown field `stray_key`"))
    {
        assert!(
            Instant::now() < deadline,
            "the WARN never reached the collector; got {:?}\n--- thurbox.log ---\n{}",
            records.iter().map(body).collect::<Vec<_>>(),
            tui.log_tail()
        );
        std::thread::sleep(Duration::from_millis(100));
        records.extend(otlp_collector::logs(&collector.take()));
    }
    assert!(tui.quit().success());
    records.extend(otlp_collector::logs(&collector.take()));

    let below_warn: Vec<_> = records
        .iter()
        .filter(|r| r.severity_number < SeverityNumber::Warn as i32)
        .map(|r| (r.severity_text.clone(), body(r)))
        .collect();
    assert!(
        below_warn.is_empty(),
        "only WARN and above is exported: {below_warn:?}"
    );
    // The file log is unchanged: still at its own level, so it holds the INFO
    // lines the collector was right not to get, and the WARN as well.
    let file = tui.log_tail();
    assert!(
        file.contains(" INFO thurbox") && file.contains("unknown field `stray_key`"),
        "the file log keeps every level:\n{file}"
    );
}

// --- the kernel-owned overlays ---------------------------------------------

#[test]
fn f1_opens_the_help_overlay_and_escape_closes_it() {
    let profile = Profile::new();
    let mut tui = Tui::spawn(&profile, 40, 120);
    tui.wait_for("No sessions yet");

    tui.send(F1);
    // The overlay's own chrome — title and footer — because those are pinned
    // wherever the list is scrolled; a binding row near the bottom slides
    // below the fold as panes declare more keys.
    tui.wait_for("Keybindings");
    tui.wait_for("rebind");
    // And it rendered the registry, not just a frame: one real binding row.
    tui.wait_for("next session");

    tui.send(ESC);
    tui.wait_gone("Keybindings");
    assert!(tui.quit().success());
}

/// The wheel scrolls help's text a line per report, as it would any document.
///
/// It used to step the *selection* once per notch, dropping the reports behind
/// it, so a spin moved the cursor a few rows while the text stayed put until
/// the cursor passed the middle — and the end of the events list, below the
/// last selectable row, could not be scrolled to at all.
#[test]
fn the_wheel_scrolls_the_help_overlay_by_lines() {
    let profile = Profile::new();
    let mut tui = Tui::spawn(&profile, 40, 120);
    tui.wait_for("No sessions yet");
    tui.send(F1);
    tui.wait_for("Navigation");

    // One report is one line: the body's second line moves up to its first.
    // Only the cells left of the scrollbar, whose thumb moves too.
    let (_, title) = tui.find("Keybindings");
    let (track, _) = tui.find("▲");
    let text =
        |tui: &Tui, y: u16| -> String { tui.row(y).chars().take(usize::from(track)).collect() };
    let second = text(&tui, title + 2);
    let over_help = (60, 20);
    tui.wheel(over_help, false, 1);
    tui.wait_until("one line of scroll", |_| text(&tui, title + 1) == second);

    tui.wheel(over_help, false, 30);
    tui.wait_gone("Navigation");
    tui.wheel(over_help, false, 300);
    tui.wait_for("a plugin ran");

    tui.wheel(over_help, true, 300);
    tui.wait_for("Navigation");
    tui.wheel(over_help, false, 300);
    tui.wait_gone("Navigation");
    // A key that moves nothing still brings the view back to the selection
    // the wheel left behind, on the first row.
    tui.send(b"x");
    tui.wait_for("Navigation");

    tui.send(ESC);
    tui.wait_gone("Keybindings");
    assert!(tui.quit().success());
}

/// Help's scrollbar takes the pointer: a press on the track jumps the text to
/// that point, and a drag along it scrolls with the pointer.
#[test]
fn the_help_overlay_scrollbar_can_be_pressed_and_dragged() {
    let profile = Profile::new();
    let mut tui = Tui::spawn(&profile, 40, 120);
    tui.wait_for("No sessions yet");
    tui.send(F1);
    tui.wait_for("Navigation");

    let (x, _) = tui.find("▲");
    let (top, bottom) = tui.track_extent(x);
    tui.press(0, (x, bottom));
    tui.wait_for("a plugin ran");
    tui.press(0, (x, top));
    tui.wait_for("Navigation");

    tui.drag_down((x, top), bottom);
    tui.wait_for("a plugin ran");

    tui.send(ESC);
    tui.wait_gone("Keybindings");
    assert!(tui.quit().success());
}

#[test]
fn f12_shows_the_per_pane_cost_table() {
    let profile = Profile::new();
    std::fs::write(
        profile.path("config/settings.toml"),
        "[features]\nautomations = false\nversion_check = false\nauto_update = false\n\
         perf_hud = true\n",
    )
    .expect("seed settings");
    let mut tui = Tui::spawn(&profile, 40, 120);
    tui.wait_for("No sessions yet");

    tui.send(F12);
    tui.wait_for("rend/reuse");
    // A ranked row for a bundled pane, not merely the header: `<rank> sessions`
    // followed by a share, inside the table's own borders — which neither the
    // session list's empty state nor the footer's session count can mimic.
    tui.wait_until("a ranked row for the session list", |frame| {
        frame.lines().any(|line| {
            line.split('│').any(|cell| {
                let mut words = cell.split_whitespace();
                words
                    .next()
                    .is_some_and(|rank| rank.chars().all(|c| c.is_ascii_digit()))
                    && words.next() == Some("sessions")
                    && cell.contains('%')
            })
        })
    });

    tui.send(F12);
    tui.wait_gone("rend/reuse");
    assert!(tui.quit().success());
}

#[test]
fn ctrl_y_opens_the_theme_picker_and_escape_closes_it() {
    let profile = Profile::new();
    let mut tui = Tui::spawn(&profile, 40, 120);
    tui.wait_for("No sessions yet");

    tui.send(CTRL_Y);
    // The filter hint rather than the title: the footer band already says
    // `Theme · F4`, so the title alone would match with no picker open.
    tui.wait_for("/ filter themes");
    // Grouped and populated. `Dark` is a group header, which does not move
    // when the presets are reordered — unlike any one palette in a 36-entry
    // list.
    tui.wait_for("Dark");

    tui.send(ESC);
    tui.wait_gone("filter themes");
    assert!(tui.quit().success());
}

// --- a pane that opens itself, and focus ------------------------------------

#[test]
fn the_search_strip_opens_with_focus_in_it() {
    // The typed text is the assertion, not the strip appearing. Focus may only
    // rest on a slot the last painted frame placed, and a pane that opens
    // itself is not in that set until the next paint — so the focus request
    // that came with the chord was once refused, and every letter of the
    // query went to the agent pane instead. Anything that reintroduces that
    // shows up here as a strip with an empty field.
    let profile = Profile::new();
    let mut tui = Tui::spawn(&profile, 40, 120);
    tui.wait_for("No sessions yet");

    tui.send(CTRL_SLASH);
    tui.wait_for("Search");
    tui.send(b"zq");
    tui.wait_for("Search zq");

    tui.send(ESC);
    tui.wait_gone("Search zq");
    assert!(tui.quit().success());
}

#[test]
fn a_saved_search_shortcut_opens_from_the_agent_pane() {
    let profile = Profile::new();
    std::fs::write(
        profile.path("config/ui.json"),
        r#"{"bindings":{"search.open":"ctrl+a"}}"#,
    )
    .expect("saved shortcut");
    let mut tui = Tui::spawn(&profile, 40, 120);
    tui.wait_for("No sessions yet");
    tui.send(b"\x01");
    tui.wait_for("Search");
    assert!(tui.quit().success());
}

#[test]
fn an_unavailable_control_socket_does_not_abort_the_tui() {
    let profile = Profile::new();
    let long_data = profile.path("data").join("x".repeat(100));
    std::fs::create_dir_all(&long_data).expect("long data path");
    let mut tui = Tui::spawn_with(&profile, 40, 120, |cmd| {
        cmd.env("THURBOX_DATA_DIR", &long_data);
    });
    tui.wait_for("interface from");
    tui.wait_for("No sessions yet");
    tui.wait_for("local UI control unavailable");
    assert!(tui.quit().success());
}

#[test]
fn a_queued_startup_notice_waits_for_an_active_error() {
    let interface = interface_plus(
        "91_error.lua",
        r#"return {
  name = "error_probe",
  slot = "sessions",
  render = function() return { type = "text", text = "" } end,
  keys = {
    { key = "ctrl+g", action = "error_probe.say", desc = "say error", scope = "global" },
  },
  on_action = function(action)
    if action == "error_probe.say" then
      command("message", { text = "active error probe", level = "error" })
      return true
    end
    return false
  end,
}"#,
    );
    let profile = Profile::new();
    let long_data = profile.path("data").join("x".repeat(100));
    std::fs::create_dir_all(&long_data).expect("long data path");
    let mut tui = Tui::spawn_with(&profile, 40, 120, |cmd| {
        cmd.env("THURBOX_DATA_DIR", &long_data);
        cmd.env("THURBOX_UI_DIR", interface.path());
    });
    tui.wait_for("interface from");
    tui.send(b"\x07");
    tui.wait_for("active error probe");
    for tick in 0..70 {
        if tick % 10 == 0 {
            tui.send(b"\x07");
        }
        std::thread::sleep(Duration::from_millis(100));
        assert!(
            !tui.frame().contains("local UI control unavailable"),
            "startup notice replaced an active error"
        );
    }
    tui.wait_for("local UI control unavailable");
    assert!(tui.quit().success());
}

#[test]
fn stale_discovery_entries_do_not_hide_a_live_tui() {
    let profile = Profile::new();
    let directory = profile.path("data/ui-control");
    std::fs::create_dir(&directory).expect("control directory");
    std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700))
        .expect("private directory");
    let stale_record = |index: u128| {
        let id = uuid::Uuid::from_u128(index + 1).to_string();
        let endpoint = directory.join(format!("{id}.sock"));
        let record = serde_json::json!({
            "id": id,
            "pid": 0,
            "started_at_unix_ms": 0,
            "label": "stale",
            "terminal": null,
            "endpoint": endpoint,
        });
        let path = directory.join(format!("{id}.json"));
        std::fs::write(&path, record.to_string()).expect("stale record");
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .expect("private record");
    };
    for index in 0..2048 {
        stale_record(index);
    }
    let mut tui = Tui::spawn(&profile, 40, 120);
    tui.wait_for("No sessions yet");
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_thurbox-cli"));
    profile.apply(&mut cmd);
    let output = cmd
        .args(["--json", "ui", "instances"])
        .output()
        .expect("list");
    assert!(output.status.success());
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).expect("JSON");
    assert_eq!(value["instances"].as_array().unwrap().len(), 1);
    assert_eq!(
        std::fs::read_dir(&directory)
            .expect("control directory")
            .count(),
        2,
        "dead records must be pruned after discovery"
    );
    assert!(tui.quit().success());
}

#[test]
fn search_cancel_has_the_same_effect_by_key_and_local_action() {
    let profile = Profile::new();
    let mut headless = Command::new(env!("CARGO_BIN_EXE_thurbox-cli"));
    profile.apply(&mut headless);
    let schema = headless
        .args(["--json", "schema"])
        .output()
        .expect("headless schema");
    assert!(schema.status.success());
    let schema: serde_json::Value = serde_json::from_slice(&schema.stdout).expect("schema JSON");
    assert_eq!(schema["ui_status"], "no_running_ui");
    let mut tui = Tui::spawn(&profile, 40, 120);
    tui.wait_for("No sessions yet");
    let cli = |args: &[&str]| {
        let mut command = Command::new(env!("CARGO_BIN_EXE_thurbox-cli"));
        profile.apply(&mut command);
        let output = command
            .args(["--json", "ui"])
            .args(args)
            .output()
            .expect("UI CLI");
        let value: serde_json::Value =
            serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
                panic!(
                    "JSON from {args:?}: {error}; stderr: {}",
                    String::from_utf8_lossy(&output.stderr)
                )
            });
        (output.status, value)
    };
    let (status, instances) = cli(&["instances"]);
    assert!(status.success());
    let instance = instances["instances"][0]["id"].as_str().expect("instance");
    let (status, catalog) = cli(&["--instance", instance, "actions"]);
    assert!(status.success());
    assert!(catalog["actions"]
        .as_array()
        .unwrap()
        .iter()
        .any(|row| row["name"] == "search.cancel"));
    let mut schema_cmd = Command::new(env!("CARGO_BIN_EXE_thurbox-cli"));
    profile.apply(&mut schema_cmd);
    let schema_output = schema_cmd
        .args(["--json", "schema", "--instance", instance])
        .output()
        .expect("CLI schema");
    assert!(schema_output.status.success());
    let schema: serde_json::Value =
        serde_json::from_slice(&schema_output.stdout).expect("schema JSON");
    assert_eq!(schema["ui_actions"], catalog["actions"]);

    tui.send(CTRL_SLASH);
    tui.wait_for("Search");
    tui.send(ESC);
    tui.wait_gone("Search ");
    let (status, by_key) = cli(&["--instance", instance, "state"]);
    assert!(status.success());

    tui.send(CTRL_SLASH);
    tui.wait_for("Search");
    let (status, _) = cli(&["--instance", instance, "action", "search.cancel"]);
    assert!(
        status.success(),
        "declared search.cancel must be externally callable"
    );
    tui.wait_gone("Search ");
    let (status, by_api) = cli(&["--instance", instance, "state"]);
    assert!(status.success());
    assert_eq!(by_api["search_query"], by_key["search_query"]);
    assert_eq!(by_api["focused_pane"], by_key["focused_pane"]);
    let (status, _) = cli(&[
        "--instance",
        instance,
        "action",
        "search.open",
        "--query",
        "",
    ]);
    assert!(status.success());
    tui.wait_for("Search");
    let (status, refused) = cli(&[
        "--instance",
        instance,
        "input",
        "sessions",
        "--input-text",
        "wrong target",
    ]);
    assert!(
        !status.success(),
        "addressed text cannot reach another pane: {refused}"
    );
    let (status, _) = cli(&[
        "--instance",
        instance,
        "input",
        "search",
        "--input-text",
        "hello",
    ]);
    assert!(status.success());
    let (status, state) = cli(&["--instance", instance, "state"]);
    assert!(status.success());
    assert_eq!(state["search_query"], "hello");
    let (status, _) = cli(&["--instance", instance, "action", "new_session.open"]);
    assert!(status.success());
    let (status, refused) = cli(&[
        "--instance",
        instance,
        "input",
        "search",
        "--input-text",
        "wrong target",
    ]);
    assert!(!status.success(), "a float owns typed input: {refused}");
    let (status, _) = cli(&[
        "--instance",
        instance,
        "input",
        "new_session",
        "--key",
        "esc",
    ]);
    assert!(status.success());
    let (status, _) = cli(&["--instance", instance, "input", "search", "--key", "esc"]);
    assert!(status.success());
    let (status, _) = cli(&["--instance", instance, "action", "help.open"]);
    assert!(status.success());
    tui.wait_for("Keybindings");
    let (status, _) = cli(&["--instance", instance, "input", "modal", "--key", "esc"]);
    assert!(status.success());
    assert!(tui.quit().success());
}

#[test]
fn addressed_text_cannot_answer_a_destructive_confirmation() {
    let Some((profile, mut tui)) = shell_session() else {
        return;
    };
    let mut command = Command::new(env!("CARGO_BIN_EXE_thurbox-cli"));
    profile.apply(&mut command);
    let output = command
        .args(["--json", "ui", "instances"])
        .output()
        .unwrap();
    let instances: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let instance: thurbox::ui_control::Instance =
        serde_json::from_value(instances["instances"][0].clone()).unwrap();
    tui.send(b"\x08");
    tui.send(b"D");
    tui.wait_for("Confirm");
    let reply = thurbox::ui_control::send(
        &instance,
        &thurbox::ui_control::Request::Input {
            target: "confirm".into(),
            input: thurbox::ui_control::InputOperation::Text { text: "y".into() },
        },
    )
    .expect("addressed text reply");
    assert_eq!(reply.result["error"]["code"], "confirmation_required");
    tui.wait_for("Confirm");
    tui.send(ESC);
    tui.wait_gone("Confirm");
    assert!(tui.quit().success());
}

#[test]
fn ui_state_and_watch_report_modal_changes_only_for_the_target_instance() {
    let profile = Profile::new();
    let mut first = Tui::spawn(&profile, 40, 120);
    let mut second = Tui::spawn(&profile, 40, 120);
    first.wait_for("No sessions yet");
    second.wait_for("No sessions yet");
    let cli = |args: &[&str]| -> serde_json::Value {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_thurbox-cli"));
        profile.apply(&mut cmd);
        let output = cmd.args(["--json"]).args(args).output().expect("run cli");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(&output.stdout).expect("JSON")
    };
    let instances = cli(&["ui", "instances"]);
    let entries = instances["instances"].as_array().expect("instances");
    let id_for = |pid| {
        entries
            .iter()
            .find(|row| row["pid"] == pid)
            .and_then(|row| row["id"].as_str())
            .expect("instance id")
            .to_owned()
    };
    let first_id = id_for(first.child.id());
    let second_id = id_for(second.child.id());
    let initial = cli(&["ui", "--instance", &first_id, "state"]);
    assert_eq!(initial["modal"], serde_json::Value::Null);
    assert!(initial["slots"].is_array());
    assert_eq!(
        initial["search"]["selected_result"],
        serde_json::Value::Null
    );
    assert_eq!(
        initial["plugin_state"]["plugins/65_search.lua"]["open"],
        false
    );

    let mut watch_cmd = Command::new(env!("CARGO_BIN_EXE_thurbox-cli"));
    profile.apply(&mut watch_cmd);
    let mut watcher = watch_cmd
        .args(["--json", "ui", "--instance", &first_id, "watch"])
        .stdout(Stdio::piped())
        .spawn()
        .expect("watch stream");
    let stdout = watcher.stdout.take().expect("watch stdout");
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        use std::io::BufRead;
        for line in std::io::BufReader::new(stdout).lines() {
            if sender.send(line.expect("watch line")).is_err() {
                break;
            }
        }
    });
    let snapshot: serde_json::Value =
        serde_json::from_str(&receiver.recv_timeout(WAIT).expect("initial watch snapshot"))
            .expect("snapshot JSON");
    assert_eq!(snapshot["kind"], "snapshot");

    first.send(F1);
    first.wait_for("Keybindings");
    let state = cli(&["ui", "--instance", &first_id, "state"]);
    assert_eq!(state["modal"]["kind"], "help");
    let selection = state["modal"]["selection"]
        .as_u64()
        .expect("modal selection");
    assert!(state["revision"].as_u64().unwrap() > initial["revision"].as_u64().unwrap());
    let deadline = Instant::now() + WAIT;
    loop {
        let Ok(line) = receiver.recv_timeout(Duration::from_millis(200)) else {
            assert!(Instant::now() < deadline, "modal event missing from stream");
            continue;
        };
        let event: serde_json::Value = serde_json::from_str(&line).expect("event JSON");
        if event["kind"] == "overlay.opened" && event["value"]["field"] == "modal" {
            assert_eq!(event["value"]["value"]["kind"], "help");
            break;
        }
        assert!(Instant::now() < deadline, "modal event missing from stream");
    }
    first.send(b"\x1b[B");
    let selection_deadline = Instant::now() + WAIT;
    let moved = loop {
        let moved = cli(&["ui", "--instance", &first_id, "state"]);
        if moved["modal"]["selection"].as_u64() != Some(selection) {
            break moved;
        }
        assert!(
            Instant::now() < selection_deadline,
            "modal selection did not move"
        );
        std::thread::sleep(Duration::from_millis(40));
    };
    assert!(moved["revision"].as_u64().unwrap() > state["revision"].as_u64().unwrap());
    let selection_since = state["revision"].as_u64().unwrap().to_string();
    let selection_events = cli(&[
        "ui",
        "--instance",
        &first_id,
        "watch",
        "--since",
        &selection_since,
        "--once",
    ]);
    assert!(selection_events["events"]
        .as_array()
        .unwrap()
        .iter()
        .any(|event| {
            event["kind"] == "overlay.changed"
                && event["value"]["field"] == "modal"
                && event["value"]["value"]["selection"] == moved["modal"]["selection"]
        }));
    let other = cli(&["ui", "--instance", &second_id, "state"]);
    assert_eq!(other["modal"], serde_json::Value::Null);
    let since = initial["revision"].as_u64().unwrap().to_string();
    let events = cli(&[
        "ui",
        "--instance",
        &first_id,
        "watch",
        "--since",
        &since,
        "--once",
    ]);
    assert!(events["events"].as_array().unwrap().iter().any(|event| {
        event["kind"] == "overlay.opened"
            && event["value"]["field"] == "modal"
            && event["value"]["value"]["kind"] == "help"
    }));
    let other_events = cli(&[
        "ui",
        "--instance",
        &second_id,
        "watch",
        "--since",
        &since,
        "--once",
    ]);
    assert!(!other_events["events"]
        .as_array()
        .unwrap()
        .iter()
        .any(|event| {
            event["kind"] == "overlay.opened"
                && event["value"]["field"] == "modal"
                && event["value"]["value"]["kind"] == "help"
        }));
    let resync = cli(&[
        "ui",
        "--instance",
        &first_id,
        "watch",
        "--since",
        "0",
        "--once",
    ]);
    assert_eq!(resync["kind"], "resync_required");
    assert_eq!(resync["state"]["modal"]["kind"], "help");
    assert!(first.quit().success());
    assert!(second.quit().success());
    let _ = watcher.kill();
    let _ = watcher.wait();
}

#[test]
fn ui_state_reads_plugin_local_changes_without_store_writes() {
    let interface = interface_plus(
        "91_counter.lua",
        r#"local count = 0
return {
  name = "counter",
  slot = "sessions",
  render = function()
    count = count + 1
    return { type = "text", text = "" }
  end,
  ui_state = function() return { count = count } end,
}"#,
    );
    let profile = Profile::new();
    let mut tui = Tui::spawn_with(&profile, 40, 120, |cmd| {
        cmd.env("THURBOX_UI_DIR", interface.path());
    });
    tui.wait_for("No sessions yet");
    let state = || {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_thurbox-cli"));
        profile.apply(&mut cmd);
        let output = cmd.args(["--json", "ui", "state"]).output().expect("state");
        assert!(output.status.success());
        serde_json::from_slice::<serde_json::Value>(&output.stdout).expect("JSON")
    };
    let first = state();
    std::thread::sleep(Duration::from_millis(700));
    let second = state();
    assert!(
        second["plugin_state"]["plugins/91_counter.lua"]["count"]
            .as_u64()
            .unwrap()
            > first["plugin_state"]["plugins/91_counter.lua"]["count"]
                .as_u64()
                .unwrap()
    );
    assert!(tui.quit().success());
}

fn float_focus_tui(opted_in: bool) -> (tempfile::TempDir, Profile, Tui) {
    let fixture = include_str!("fixtures/float_focus/91_cards.lua");
    let body = if opted_in {
        fixture.to_string()
    } else {
        fixture.replace("on_focus_cycle = function", "unused_cycle = function")
    };
    let interface = interface_plus("91_cards.lua", &body);
    let profile = Profile::new();
    let tui = Tui::spawn_with(&profile, 40, 120, |cmd| {
        cmd.env("THURBOX_UI_DIR", interface.path());
    });
    tui.wait_for("No sessions yet");
    (interface, profile, tui)
}

#[test]
fn float_focus_resize_repairs_hidden_pane_without_cycling_the_float() {
    let (_interface, _profile, mut tui) = float_focus_tui(true);
    tui.send(b"\x0c");
    tui.send(b"\x02");
    tui.wait_for("CARDS alpha calls=0 behind=sessions");
    tui.resize(40, 70);
    tui.wait_for("CARDS alpha calls=0 behind=agent");
    tui.send(b"\x0c");
    tui.wait_for("CARDS beta calls=1 behind=agent");
    assert!(tui.quit().success());
}

#[test]
fn float_focus_cycles_next_and_wraps_without_moving_the_panes() {
    let (_interface, _profile, mut tui) = float_focus_tui(true);
    tui.send(b"\x02");
    tui.wait_for("CARDS alpha calls=0 behind=agent");
    for (item, calls) in [("beta", 1), ("gamma", 2), ("alpha", 3)] {
        tui.send(b"\x0c");
        tui.wait_for(&format!("CARDS {item} calls={calls} behind=agent"));
    }
    assert!(tui.quit().success());
}

#[test]
fn float_focus_cycles_previous_and_wraps() {
    let (_interface, _profile, mut tui) = float_focus_tui(true);
    tui.send(b"\x02");
    tui.wait_for("CARDS alpha calls=0 behind=agent");
    for (item, calls) in [("gamma", 1), ("beta", 2), ("alpha", 3)] {
        tui.send(b"\x08");
        tui.wait_for(&format!("CARDS {item} calls={calls} behind=agent"));
    }
    assert!(tui.quit().success());
}

#[test]
fn float_focus_close_preserves_focus_and_resumes_the_normal_cycle() {
    let (_interface, _profile, mut tui) = float_focus_tui(true);
    tui.send(b"\x02");
    tui.wait_for("CARDS alpha calls=0 behind=agent");
    tui.send(b"\x0c");
    tui.wait_for("CARDS beta calls=1 behind=agent");
    tui.send(ESC);
    tui.wait_gone("CARDS");
    tui.send(b"\x0c");
    tui.send(b"\x02");
    tui.wait_for("CARDS alpha calls=0 behind=sessions");
    tui.send(ESC);
    tui.wait_gone("CARDS");
    tui.send(b"\x08");
    tui.send(b"\x02");
    tui.wait_for("CARDS alpha calls=0 behind=agent");
    assert!(tui.quit().success());
}

#[test]
fn float_focus_empty_hook_declines_to_the_normal_cycle() {
    let (_interface, _profile, mut tui) = float_focus_tui(true);
    tui.send(b"\x02");
    tui.wait_for("CARDS alpha calls=0 behind=agent");
    tui.send(b"e");
    tui.wait_for("CARDS empty calls=0 behind=agent");
    tui.send(b"\x0c");
    tui.wait_for("CARDS empty calls=1 behind=sessions");
    tui.send(b"\x08");
    tui.wait_for("CARDS empty calls=2 behind=agent");
    assert!(tui.quit().success());
}

#[test]
fn float_focus_without_opt_in_keeps_the_existing_cycle() {
    let (_interface, _profile, mut tui) = float_focus_tui(false);
    tui.send(b"\x02");
    tui.wait_for("CARDS alpha calls=0 behind=agent");
    tui.send(b"\x0c");
    tui.wait_for("CARDS alpha calls=0 behind=sessions");
    tui.send(b"\x08");
    tui.wait_for("CARDS alpha calls=0 behind=agent");
    assert!(tui.quit().success());
}

#[test]
fn float_focus_error_falls_back_and_recovery_keys_still_work() {
    let fixture = include_str!("fixtures/float_focus/91_cards.lua").replace(
        "if state.empty then",
        "if true then error('cycle failed') end; if state.empty then",
    );
    let interface = interface_plus("91_cards.lua", &fixture);
    let profile = Profile::new();
    let mut tui = Tui::spawn_with(&profile, 40, 120, |cmd| {
        cmd.env("THURBOX_UI_DIR", interface.path());
    });
    tui.wait_for("No sessions yet");
    tui.send(b"\x02");
    tui.wait_for("CARDS alpha calls=0 behind=agent");
    tui.send(b"\x0c");
    tui.wait_for("CARDS alpha calls=1 behind=sessions");
    tui.send(F12);
    tui.wait_for("rend/reuse");
    tui.send(F12);
    tui.wait_gone("rend/reuse");
    std::fs::write(
        interface.path().join("plugins/91_cards.lua"),
        include_str!("fixtures/float_focus/91_cards.lua"),
    )
    .unwrap();
    tui.send(F10);
    tui.wait_for("reload=f10");
    tui.send(b"\x0c");
    tui.wait_for("CARDS beta calls=2 behind=sessions");
    assert!(tui.quit().success());
}

#[test]
fn ui_state_reports_the_focused_switch_pane_after_it_is_drawn() {
    let interface = interface_plus(
        "21_alt.lua",
        r#"return {
  name = "alternate",
  slot = "center",
  focusable = true,
  render = function() return { type = "text", text = "ALT PANE" } end,
  keys = {
    { key = "ctrl+b", action = "alternate.focus", desc = "focus alternate", scope = "global" },
  },
  on_action = function(action)
    if action == "alternate.focus" then
      command("focus", { text = "alternate" })
      return true
    end
    return false
  end,
}"#,
    );
    let profile = Profile::new();
    let mut tui = Tui::spawn_with(&profile, 40, 120, |cmd| {
        cmd.env("THURBOX_UI_DIR", interface.path());
    });
    tui.wait_for("No sessions yet");
    let state = || {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_thurbox-cli"));
        profile.apply(&mut cmd);
        let output = cmd.args(["--json", "ui", "state"]).output().expect("state");
        assert!(output.status.success());
        serde_json::from_slice::<serde_json::Value>(&output.stdout).expect("JSON")
    };
    tui.send(b"\x02");
    let deadline = Instant::now() + WAIT;
    let after = loop {
        let current = state();
        if current["focused_pane"] == "alternate" {
            break current;
        }
        assert!(
            Instant::now() < deadline,
            "alternate pane did not take focus"
        );
    };
    assert_eq!(after["focused_pane"], "alternate");
    let center = after["slots"]
        .as_array()
        .unwrap()
        .iter()
        .find(|slot| slot["slot"] == "center")
        .expect("center slot");
    assert_eq!(
        center["visible_pane_ids"],
        serde_json::json!(["plugins/21_alt.lua"])
    );
    tui.wait_for("ALT PANE");
    assert!(tui.quit().success());
}

#[test]
fn ui_watch_exits_cleanly_when_its_reader_closes() {
    let profile = Profile::new();
    let mut tui = Tui::spawn(&profile, 40, 120);
    tui.wait_for("No sessions yet");
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_thurbox-cli"));
    profile.apply(&mut cmd);
    let mut watcher = cmd
        .args(["--json", "ui", "watch"])
        .stdout(Stdio::piped())
        .spawn()
        .expect("watch stream");
    let stdout = watcher.stdout.take().expect("watch stdout");
    let mut reader = std::io::BufReader::new(stdout);
    use std::io::BufRead;
    let mut first = String::new();
    reader.read_line(&mut first).expect("snapshot line");
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&first).unwrap()["kind"],
        "snapshot"
    );
    drop(reader);
    tui.send(F1);
    tui.wait_for("Keybindings");
    let deadline = Instant::now() + WAIT;
    let status = loop {
        if let Some(status) = watcher.try_wait().expect("watch exit") {
            break status;
        }
        assert!(
            Instant::now() < deadline,
            "watch did not stop after a closed pipe"
        );
        std::thread::sleep(Duration::from_millis(40));
    };
    assert!(
        status.success(),
        "closed reader is a normal end to the stream"
    );
    assert!(tui.quit().success());
}

#[test]
fn ui_state_reports_an_explicit_error_when_the_snapshot_exceeds_the_reply_limit() {
    let interface = interface_plus(
        "90_bulk_00.lua",
        r#"return {
  name = "bulk00", slot = "sessions",
  render = function() return { type = "text", text = "" } end,
  ui_state = function()
    local result = {}
    for i = 1, 16 do result["key" .. i] = string.rep("x", 256) end
    return result
  end,
}"#,
    );
    let source = std::fs::read_to_string(interface.path().join("plugins/90_bulk_00.lua"))
        .expect("bulk plugin");
    for index in 1..70 {
        std::fs::write(
            interface
                .path()
                .join(format!("plugins/90_bulk_{index:02}.lua")),
            source.replace("bulk00", &format!("bulk{index:02}")),
        )
        .expect("bulk plugin");
    }
    let profile = Profile::new();
    let mut tui = Tui::spawn_with(&profile, 40, 120, |cmd| {
        cmd.env("THURBOX_UI_DIR", interface.path());
    });
    tui.wait_for("No active sessions");
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_thurbox-cli"));
    profile.apply(&mut cmd);
    let output = cmd.args(["--json", "ui", "state"]).output().expect("state");
    assert!(!output.status.success(), "large state must be rejected");
    let message = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        message.contains("UI state exceeds local reply limit"),
        "{message}"
    );
    assert!(tui.quit().success());
}

#[test]
fn ui_watch_keeps_action_outcomes_behind_a_large_state_delta() {
    let interface = interface_plus(
        "90_bulk_00.lua",
        r#"local generation = 0
return {
  name = "bulk00", slot = "sessions",
  render = function() return { type = "text", text = "" } end,
  ui_state = function()
    generation = generation + 1
    local result = {}
    for i = 1, 16 do result["key" .. i] = string.rep("x", 250) .. generation end
    return result
  end,
}"#,
    );
    let source = std::fs::read_to_string(interface.path().join("plugins/90_bulk_00.lua"))
        .expect("bulk plugin");
    for index in 1..4 {
        std::fs::write(
            interface
                .path()
                .join(format!("plugins/90_bulk_{index:02}.lua")),
            source.replace("bulk00", &format!("bulk{index:02}")),
        )
        .expect("bulk plugin");
    }
    let profile = Profile::new();
    let mut tui = Tui::spawn_with(&profile, 40, 120, |cmd| {
        cmd.env("THURBOX_UI_DIR", interface.path());
    });
    tui.wait_for("No active sessions");
    let cli = |args: &[&str]| -> serde_json::Value {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_thurbox-cli"));
        profile.apply(&mut cmd);
        let output = cmd
            .args(["--json", "ui"])
            .args(args)
            .output()
            .expect("ui cli");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(&output.stdout).expect("JSON")
    };
    let initial = cli(&["state"]);
    let initial_revision = initial["revision"].as_u64().expect("initial revision");
    cli(&["action", "search.open", "--query", "bulk-review"]);
    tui.wait_for("Search bulk-review");
    let mut since = initial_revision;
    let mut found_action = false;
    for _ in 0..8 {
        let revision = since.to_string();
        let batch = cli(&["watch", "--since", &revision, "--once"]);
        assert_eq!(batch["kind"], "delta", "watch lost events: {batch}");
        found_action |= batch["events"].as_array().unwrap().iter().any(|event| {
            event["kind"] == "action.completed" && event["value"]["action"] == "search.open"
        });
        since = batch["revision"].as_u64().expect("batch revision");
        if found_action {
            break;
        }
    }
    assert!(found_action, "watch omitted the action outcome");
    assert!(tui.quit().success());
}

#[test]
fn a_paste_lands_in_the_search_strip() {
    // A paste goes where the caret is. It used to go to the terminal behind
    // the strip — or, with no terminal on screen, nowhere — because only a
    // modal or a float was offered the text before the focused terminal.
    let profile = Profile::new();
    let mut tui = Tui::spawn(&profile, 40, 120);
    tui.wait_for("No sessions yet");

    tui.send(CTRL_SLASH);
    tui.wait_for("Search");
    tui.send(b"\x1b[200~zq-pasted\x1b[201~");
    tui.wait_for("Search zq-pasted");
    assert!(tui.quit().success());
}

#[test]
fn a_paste_into_a_field_is_text_even_where_the_pane_binds_a_letter() {
    // A paste is typing, never a command: replayed through the registry, a
    // pasted `x` ran the pane's own `x` action instead of reaching its field.
    let interface = interface_plus(
        "91_field.lua",
        r#"local textinput = require("lib.textinput")
return {
  name = "field",
  slot = "sessions",
  focusable = true,
  keys = {
    { key = "ctrl+g", action = "field.focus", desc = "focus the field", scope = "global" },
    { key = "x", action = "field.x", desc = "a letter chord" },
  },
  render = function()
    local field = state.field or textinput.new("")
    return textinput.node(field, { label = state.fired and "FIRED" or "probe", focused = true })
  end,
  on_action = function(action)
    if action == "field.focus" then
      command("focus", { text = "field" })
      return true
    elseif action == "field.x" then
      state.fired = true
      return true
    end
    return false
  end,
  on_key = function(key)
    local field = state.field or textinput.new("")
    local consumed = textinput.key(field, key)
    state.field = field
    return consumed
  end,
}"#,
    );
    let profile = Profile::new();
    let mut tui = Tui::spawn_with(&profile, 40, 120, |cmd| {
        cmd.env("THURBOX_UI_DIR", interface.path());
    });
    tui.wait_for("probe");
    tui.send(b"\x07");
    wait_for_view(&tui, "Field");
    tui.send(b"ok");
    tui.wait_for("│ok");

    tui.send(b"\x1b[200~axb\x1b[201~");
    tui.wait_until("the pasted text in the field", |frame| {
        frame.contains("okaxb")
    });
    assert!(
        !tui.frame().contains("FIRED"),
        "a pasted letter ran an action"
    );
    assert!(tui.quit().success());
}

#[test]
fn the_search_field_edits_by_word_as_a_shell_line_does() {
    // Alt+b, Alt+f, Alt+d and Alt+Backspace are what a shell's line editor
    // has taught every hand; the field swallowed every Alt chord unused.
    let profile = Profile::new();
    let mut tui = Tui::spawn(&profile, 40, 120);
    tui.wait_for("No sessions yet");

    tui.send(CTRL_SLASH);
    tui.wait_for("Search");
    tui.send(b"alpha beta gamma");
    tui.wait_for("Search alpha beta gamma");

    // Alt+Backspace: the word before the caret goes.
    tui.send(b"\x1b\x7f");
    tui.wait_for("Search alpha beta ");
    tui.wait_gone("gamma");

    // Alt+b back over `beta`, and what is typed lands before it.
    tui.send(b"\x1bb");
    tui.send(b"X");
    tui.wait_for("Search alpha Xbeta");
    assert!(tui.quit().success());
}

#[test]
fn the_palette_lists_the_kernels_clipboard_actions() {
    // The one thing a unit test over a hand-assembled registry cannot show: the
    // *binary* declares copy and paste (`collect_declarations`), so they are
    // real bindings — listed, runnable by name, and rebindable — rather than the
    // literal key arms in the loop they used to be (issue #1024).
    let profile = Profile::new();
    let mut tui = Tui::spawn(&profile, 40, 120);
    tui.wait_for("No sessions yet");

    tui.send(CTRL_P);
    tui.send(b"paste");
    // The row's description, which only the registry could have supplied.
    tui.wait_for("ctrl+v");

    tui.send(ESC);
    tui.wait_gone("ctrl+v");
    assert!(tui.quit().success());
}

// --- a reflow: closing a column ---------------------------------------------

#[test]
fn hiding_the_session_column_reflows_without_ghosts_or_a_screen_clear() {
    // Two regressions live here, and they pull in opposite directions. A
    // closed column left its border behind (the cell diff cannot see a
    // glyph-width disagreement), and the fix that cleared the screen made
    // every toggle blink the whole interface. The right answer is a full
    // repaint of the new frame with no clear in between — asserted from both
    // sides: the frame has no trace of the column, and the bytes have no
    // `ED 2`.
    let profile = Profile::new();
    let mut tui = Tui::spawn(&profile, 30, 100);
    tui.wait_for("No sessions yet");
    let before = tui.raw_len();

    tui.send(F9);
    tui.wait_gone("No sessions yet");
    // Settle: the forced-redraw floor is 250 ms, so a frame later than this
    // is one that would have carried a stray clear too.
    std::thread::sleep(Duration::from_millis(400));

    let since_toggle = tui.raw_since(before);
    assert!(
        !since_toggle.contains("\x1b[2J"),
        "a column toggle must repaint, never clear the screen (the blink)"
    );
    // The column was on the left; with it gone, every pane row starts with
    // the centre pane's own border — a box-drawing glyph — and nothing in it
    // is the list's. Row 0 and the last two rows are the chrome bands.
    let (rows, _) = tui.screen.lock().unwrap().screen().size();
    let rows = rows.get();
    for y in 1..rows - 2 {
        let row = tui.row(y);
        let first = row.chars().next().unwrap_or(' ');
        assert!(
            first == ' ' || ('\u{2500}'..='\u{257F}').contains(&first),
            "row {y} does not start with the centre pane's border: {row:?}\nframe:\n{}",
            tui.frame()
        );
        assert!(
            !row.contains("Sessions") && !row.contains("No sessions yet"),
            "row {y} still shows the closed column: {row:?}\nframe:\n{}",
            tui.frame()
        );
    }

    // And it comes back.
    tui.send(F9);
    tui.wait_for("No sessions yet");
    assert!(tui.quit().success());
}

// --- sizes ------------------------------------------------------------------

#[test]
fn survives_a_resize_storm_down_to_one_cell() {
    // Resizing under the loop is where underflow lives: a one-cell pane is
    // exactly what `vt_floor` exists for, and a `resolve` that hands out a
    // rect past the edge is a paint that indexes out of the buffer. The
    // binary must keep painting through arbitrary sizes and exit cleanly
    // afterwards.
    let profile = Profile::new();
    let mut tui = Tui::spawn(&profile, 40, 120);
    tui.wait_for("No sessions yet");

    for (rows, cols) in [
        (24, 80),
        (6, 20),
        (2, 2),
        (1, 1),
        (50, 140),
        (3, 4),
        (30, 100),
    ] {
        tui.resize(rows, cols);
        std::thread::sleep(Duration::from_millis(150));
        assert!(
            tui.alive(),
            "thurbox died after a resize to {rows}x{cols}; frame:\n{}",
            tui.frame()
        );
    }

    // Proof of life after the storm: back at a usable size the loop paints
    // the interface again, not merely stays resident.
    tui.wait_for("No sessions yet");
    assert!(tui.quit().success());
}

// --- a broken interface -----------------------------------------------------

/// A copy of the repository's `ui/` with one pane replaced by `body`.
fn interface_with(broken: &str, body: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../ui");
    copy_tree(&source, dir.path());
    std::fs::write(dir.path().join(broken), body).expect("break a pane");
    dir
}

fn copy_tree(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).expect("mkdir");
    for entry in std::fs::read_dir(from).expect("read_dir") {
        let entry = entry.expect("entry");
        let target = to.join(entry.file_name());
        if entry.file_type().expect("file_type").is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), &target).expect("copy");
        }
    }
}

#[test]
fn a_pane_that_fails_to_load_is_reported_and_the_rest_of_the_interface_runs() {
    // The recovery path. A syntax error in one pane must not take the binary
    // down or leave a blank screen: the error is painted where the user can
    // read it, the kernel-owned overlays still open (they are how the pane
    // gets switched off or restored), and quitting is still clean.
    let interface = interface_with("plugins/10_sessions.lua", "return {\n");
    let profile = Profile::new();
    let mut tui = Tui::spawn_with(&profile, 40, 120, |cmd| {
        cmd.env("THURBOX_UI_DIR", interface.path());
    });

    tui.wait_for("reload failed");
    tui.wait_for("10_sessions");
    assert!(tui.alive(), "a broken pane must not take the process down");

    // The documented recovery path: settings → the Interface tab, where the
    // failed file sorts to the top with its error in the footer. Both are
    // kernel-owned, which is the point — the recovery tool is not the thing
    // that is broken.
    tui.send(F6);
    tui.wait_for("Settings");
    tui.send(b"]");
    // The file's own name, which the error panel does not print (it names
    // the plugin), so this can only be the Interface tab's row.
    tui.wait_for("10_sessions.lua");
    tui.send(ESC);
    tui.wait_gone("Interface");

    let status = tui.quit();
    assert!(status.success(), "exit must still be clean: {status:?}");
}

/// Move the Interface tab's cursor onto the row listing `path`.
///
/// By pressing `j` until the pointer is on it rather than by counting, so the
/// scenario does not depend on how many files the bundled interface has today.
fn select_file(tui: &mut Tui, path: &str) {
    for _ in 0..60 {
        let on_it = |frame: &str| {
            frame
                .lines()
                .any(|line| line.contains('▸') && line.contains(path))
        };
        if on_it(&tui.frame()) {
            return;
        }
        let before = tui.frame();
        tui.send(b"j");
        tui.wait_until("the cursor to move", |frame| frame != before);
    }
    tui.give_up(&format!("{path} was never selected"));
}

#[test]
fn the_interface_tab_explains_each_file_and_drives_every_action() {
    // The tab exists to answer "why is this pane not on screen, and what do I do
    // about it" without reading the guide. So: every file grouped, one word of
    // state per row, the selected row's reason and fix spelled out, only the
    // keys that row answers to, and the one destructive key asking first.
    let interface = interface_with("plugins/10_sessions.lua", {
        let shipped = std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../ui/plugins/10_sessions.lua"),
        )
        .expect("shipped pane");
        &format!("{shipped}\n-- tb-edit\n")
    });
    std::fs::write(
        interface.path().join("plugins/91_tbnotes.lua"),
        r#"return { name = "tbnotes", slot = "tbnotes",
  render = function() return { type = "text", text = "tb-notes" } end }"#,
    )
    .expect("an unplaced pane");
    std::fs::write(
        interface.path().join("plugins/92_tbrun.lua"),
        r#"return { name = "tbrun", slot = "tbrun", capabilities = { "run" },
  render = function() return { type = "text", text = "tb-run" } end }"#,
    )
    .expect("a pane asking to run programs");
    let profile = Profile::new();
    let mut tui = Tui::spawn_with(&profile, 40, 120, |cmd| {
        cmd.env("THURBOX_UI_DIR", interface.path());
    });
    tui.wait_for("No sessions yet");

    tui.send(F6);
    tui.wait_for("Settings");
    tui.send(b"]");
    tui.wait_for("YOURS");
    tui.wait_for("not placed");

    // Not on screen: the reason, and the line that fixes it.
    select_file(&mut tui, "91_tbnotes.lua");
    tui.wait_for(r#"{ slot = "tbnotes" }"#);

    // space: off, said so, and back on.
    tui.send(b" ");
    tui.wait_for("space turns it back on");
    tui.send(b" ");
    tui.wait_gone("space turns it back on");

    // d asks first, and moving away withdraws the question.
    select_file(&mut tui, "91_tbnotes.lua");
    tui.send(b"d");
    tui.wait_for("cannot be undone");
    tui.send(b"k");
    tui.wait_gone("cannot be undone");

    // t: what it asks for, and where it stands, before and after.
    select_file(&mut tui, "92_tbrun.lua");
    tui.wait_for("not granted");
    tui.send(b"t");
    tui.wait_for("t again to grant");
    tui.send(b"t");
    tui.wait_for("t revokes it");

    // r on an edited file asks, says what is lost, and then restores.
    select_file(&mut tui, "10_sessions.lua");
    tui.wait_for("r restore");
    tui.send(b"r");
    tui.wait_for("edits are lost");
    tui.send(b"r");
    tui.wait_for("restored plugins/10_sessions.lua");
    let restored = std::fs::read_to_string(interface.path().join("plugins/10_sessions.lua"))
        .expect("restored pane");
    assert!(!restored.contains("tb-edit"), "the shipped copy is back");

    tui.send(ESC);
    tui.wait_gone("YOURS");
    let status = tui.quit();
    assert!(status.success(), "exit must be clean: {status:?}");
}

/// A copy of the shipped interface with a mix of plugins in it: the `top`
/// example installed as a package, a pane of your own the layout does not
/// place, one that asks to run programs, and an edited shipped pane.
fn mixed_interface(profile: &Profile) -> tempfile::TempDir {
    let interface = interface_with("plugins/10_sessions.lua", {
        let shipped = std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../ui/plugins/10_sessions.lua"),
        )
        .expect("shipped pane");
        &format!("{shipped}\n-- tb-edit\n")
    });
    std::fs::write(
        interface.path().join("plugins/91_tbnotes.lua"),
        r#"return { name = "tbnotes", slot = "tbnotes",
  render = function() return { type = "text", text = "tb-notes" } end }"#,
    )
    .expect("an unplaced pane");
    std::fs::write(
        interface.path().join("plugins/92_tbrun.lua"),
        r#"return { name = "tbrun", slot = "tbrun", capabilities = { "run" },
  render = function() return { type = "text", text = "tb-run" } end }"#,
    )
    .expect("a pane asking to run programs");
    let top = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/panes/top");
    let mut install = Command::new(env!("CARGO_BIN_EXE_thurbox-cli"));
    profile.apply(&mut install);
    let output = install
        .env("THURBOX_UI_DIR", interface.path())
        .args(["plugin", "install", top.to_str().expect("utf-8 path")])
        .output()
        .expect("run thurbox-cli");
    assert!(
        output.status.success(),
        "plugin install failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    interface
}

/// Settings, then the Interface tab, then wait for its plugin groups.
fn open_interface_tab(tui: &mut Tui) {
    tui.send(F6);
    tui.wait_for("Settings");
    tui.send(b"]");
    tui.wait_for("SHIPPED");
}

/// The frame line that lists `path`.
fn line_of(tui: &Tui, path: &str) -> String {
    tui.frame()
        .lines()
        .find(|line| line.contains(path))
        .unwrap_or_else(|| tui.give_up(&format!("a line listing {path}")))
        .to_string()
}

#[test]
fn the_interface_tab_groups_plugins_by_where_they_came_from() {
    // A real interface is forty-odd files, and twenty of them are modules and
    // docs nobody acts on. The plugins are what an operator manages, so they are
    // grouped by where they came from — yours, each installed package, shipped —
    // with what each asks for on its row, and the support files stay out of the
    // way until asked for or until one of them needs attention.
    let profile = Profile::new();
    let interface = mixed_interface(&profile);
    let mut tui = Tui::spawn_with(&profile, 40, 120, |cmd| {
        cmd.env("THURBOX_UI_DIR", interface.path());
    });
    tui.wait_for("No sessions yet");
    open_interface_tab(&mut tui);
    tui.wait_for("YOURS");
    tui.wait_for("PACKAGE · top");

    let frame = tui.frame();
    let at = |needle: &str| {
        frame
            .lines()
            .position(|line| line.contains(needle))
            .unwrap_or_else(|| tui.give_up(&format!("{needle} listed")))
    };
    assert!(at("YOURS") < at("91_tbnotes.lua"), "{frame}");
    assert!(at("PACKAGE · top") < at("85_top.lua"), "{frame}");
    assert!(at("85_top.lua") < at("SHIPPED"), "{frame}");
    assert!(at("SHIPPED") < at("10_sessions.lua"), "{frame}");

    let asks = line_of(&tui, "92_tbrun.lua");
    assert!(asks.contains("run not granted"), "{asks}");
    let edited = line_of(&tui, "10_sessions.lua");
    assert!(edited.contains("edited"), "{edited}");
    // The edited layout needs attention — r restores it — so it stays; the
    // untouched modules do not.
    assert!(!frame.contains("lib/chrome.lua"), "{frame}");

    tui.send(b"a");
    tui.wait_for("SUPPORT FILES");
    tui.wait_for("AGENTS.md");
    tui.send(b"a");
    tui.wait_gone("AGENTS.md");

    tui.send(ESC);
    tui.wait_gone("SHIPPED");
    assert!(tui.quit().success());
}

#[test]
fn a_plugin_that_breaks_the_reload_is_named_and_the_others_are_not_blamed() {
    // A reload is all-or-nothing, so one broken file leaves every other file of
    // yours unloaded. Calling all of them `failed` hid the one to fix among
    // several red rows; the summary counts the culprit, its row carries the
    // error, and the rest read as held back by it.
    //
    // In the profile's own interface directory rather than `THURBOX_UI_DIR`:
    // that is where a user's edits live, and the one place a failed load swaps
    // in the bundled interface — whose host reports no error of its own, so the
    // culprit has to be named from the user's error it replaced.
    let profile = Profile::new();
    let interface = profile.path("config/ui");
    copy_tree(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("../../ui"),
        &interface,
    );
    std::fs::write(interface.join("plugins/93_tbbroken.lua"), "return {\n").expect("break");
    std::fs::write(
        interface.join("plugins/92_tbrun.lua"),
        r#"return { name = "tbrun", slot = "tbrun", capabilities = { "run" },
  render = function() return { type = "text", text = "tb-run" } end }"#,
    )
    .expect("a pane that is fine");
    let mut tui = Tui::spawn(&profile, 40, 120);
    tui.wait_for("using the bundled interface");
    open_interface_tab(&mut tui);
    tui.wait_for("1 failed");

    let broken = line_of(&tui, "93_tbbroken.lua");
    assert!(broken.contains("failed"), "{broken}");
    assert!(
        broken.contains('▸'),
        "the culprit is selected first: {broken}"
    );
    let held = line_of(&tui, "92_tbrun.lua");
    assert!(held.contains("not loaded"), "{held}");
    assert!(!held.contains("failed"), "{held}");
    // The load error itself, under the list.
    tui.wait_for("unexpected symbol");

    select_file(&mut tui, "92_tbrun.lua");
    tui.wait_for("93_tbbroken.lua failed to load");

    tui.send(ESC);
    tui.wait_gone("SHIPPED");
    assert!(tui.quit().success());
}

#[test]
fn a_broken_layout_reads_failed_with_its_error() {
    // `layout.lua` is not a pane, so the inventory calls it present whether it
    // loaded or not. Read that way, the one file to fix said `in use`, the
    // summary counted nothing, and — with the error panel kept off the open
    // modal — its error was shown nowhere.
    let profile = Profile::new();
    let interface = profile.path("config/ui");
    copy_tree(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("../../ui"),
        &interface,
    );
    std::fs::write(interface.join("layout.lua"), "return {\n").expect("break");
    let mut tui = Tui::spawn(&profile, 40, 120);
    tui.wait_for("using the bundled interface");
    open_interface_tab(&mut tui);
    tui.wait_for("1 failed");

    let layout = line_of(&tui, "layout.lua");
    assert!(layout.contains("failed"), "{layout}");
    assert!(!layout.contains("in use"), "{layout}");
    select_file(&mut tui, "layout.lua");
    tui.wait_for("unexpected symbol");

    tui.send(ESC);
    tui.wait_gone("SHIPPED");
    assert!(tui.quit().success());
}

#[test]
fn granting_a_capability_asks_first_and_revoking_does_not() {
    // `t` is the one key in the tab that hands something away: `run` lets a
    // pane start programs. It used to grant on a single press while `d`, which
    // only costs a file, asked twice. Granting now spells out what is granted
    // and waits for a second press; taking it back stays one press.
    let profile = Profile::new();
    let interface = mixed_interface(&profile);
    let mut tui = Tui::spawn_with(&profile, 40, 120, |cmd| {
        cmd.env("THURBOX_UI_DIR", interface.path());
    });
    tui.wait_for("No sessions yet");
    open_interface_tab(&mut tui);
    let store = profile.path("config/plugin-state.json");
    let granted = || {
        std::fs::read_to_string(&store)
            .unwrap_or_default()
            .contains("92_tbrun.lua")
    };

    select_file(&mut tui, "92_tbrun.lua");
    tui.wait_for("not granted; t grants it");
    tui.send(b"t");
    tui.wait_for("grant 92_tbrun.lua");
    tui.wait_for("runs programs and reads their output");
    tui.wait_for("t again to grant");
    assert!(!granted(), "the first press only asks");

    // Any other key withdraws the question.
    tui.send(b"j");
    tui.wait_gone("t again to grant");
    select_file(&mut tui, "92_tbrun.lua");
    // So does leaving the tab: `[`/`]` never reach the tab, and a question
    // still armed on return would grant on the next `t` without asking.
    tui.send(b"t");
    tui.wait_for("t again to grant");
    tui.send(b"]");
    tui.wait_gone("SHIPPED");
    tui.send(b"[");
    tui.wait_for("SHIPPED");
    tui.wait_gone("t again to grant");
    tui.send(b"t");
    tui.wait_for("t again to grant");
    assert!(!granted(), "a press after coming back asks again");
    tui.send(b"t");
    tui.wait_for("granted; t revokes it");
    assert!(granted(), "the second press grants");
    let row = line_of(&tui, "92_tbrun.lua");
    assert!(row.contains("run granted"), "{row}");

    tui.send(b"t");
    tui.wait_for("not granted; t grants it");
    assert!(!granted(), "revoking takes one press");

    tui.send(ESC);
    tui.wait_gone("SHIPPED");
    assert!(tui.quit().success());
}

#[test]
fn the_interface_tab_keeps_each_rows_state_on_a_narrow_terminal() {
    // At 60 columns the modal is the whole screen. The file name gives way
    // first, then where it came from; the state and the grant never do, and
    // the footer still offers its keys.
    let profile = Profile::new();
    let interface = mixed_interface(&profile);
    std::fs::write(
        interface
            .path()
            .join("plugins/94_a_pane_with_a_name_long_enough_to_need_cutting.lua"),
        r#"return { name = "tblong", slot = "tblong", capabilities = { "run" },
  render = function() return { type = "text", text = "tb-long" } end }"#,
    )
    .expect("a long-named pane");
    let mut tui = Tui::spawn_with(&profile, 30, 60, |cmd| {
        cmd.env("THURBOX_UI_DIR", interface.path());
    });
    tui.wait_for("thurbox");
    open_interface_tab(&mut tui);

    let long = line_of(&tui, "94_a_pane");
    assert!(long.contains('…'), "the name is what gives way: {long}");
    assert!(long.contains("not placed"), "{long}");
    assert!(long.contains("run not granted"), "{long}");
    let asks = line_of(&tui, "92_tbrun.lua");
    assert!(asks.contains("run not granted"), "{asks}");
    let footer = line_of(&tui, "j/k");
    assert!(footer.contains("Close"), "{footer}");
    // The hints stop short of the pill rather than run into it.
    assert!(!footer.contains("ta Close"), "{footer}");

    // The grant question fits too: the file name gives way, never what is
    // granted or the key that grants it.
    select_file(&mut tui, "94_a_pane");
    tui.send(b"t");
    tui.wait_for("grant 94_");
    let question = line_of(&tui, "grant 94_");
    assert!(question.contains("run?"), "{question}");
    assert!(question.contains("t again"), "{question}");
    assert!(question.contains("Close"), "{question}");
    tui.send(b"j");

    tui.send(ESC);
    tui.wait_gone("SHIPPED");
    assert!(tui.quit().success());
}

/// A pane written for this test, dropped in beside the bundled ones.
fn interface_plus(name: &str, body: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../ui");
    copy_tree(&source, dir.path());
    std::fs::write(dir.path().join("plugins").join(name), body).expect("add a pane");
    dir
}

#[test]
fn a_pane_can_speak_in_the_message_band_and_open_a_kernel_modal() {
    // The coordinator half of `command("message")` and `command("action")`.
    // Both are only reachable through the loop: one writes the status the
    // message band reads, the other runs a declared action exactly as a click
    // on it would — and neither can be seen from a `TestBackend`, because the
    // bands and the modals are drawn by the binary's own `draw`.
    //
    // `ctrl+p` is the palette's chord and reaches the pane from anywhere, so
    // the scenario needs no focus dance; `p` alone would be swallowed by
    // whatever holds the keyboard.
    let interface = interface_plus(
        "91_speaker.lua",
        r#"return {
  name = "speaker",
  slot = "sessions",
  render = function()
    return { type = "text", text = "" }
  end,
  keys = {
    { key = "ctrl+g", action = "speaker.say", desc = "say something", scope = "global" },
    { key = "ctrl+b", action = "speaker.help", desc = "open help", scope = "global" },
  },
  on_action = function(action)
    if action == "speaker.say" then
      command("message", { text = "the pane said so", level = "error" })
      return true
    elseif action == "speaker.help" then
      command("action", { text = "help.open" })
      return true
    end
    return false
  end,
}"#,
    );
    let profile = Profile::new();
    let mut tui = Tui::spawn_with(&profile, 40, 120, |cmd| {
        cmd.env("THURBOX_UI_DIR", interface.path());
    });
    tui.wait_for("No sessions yet");

    tui.send(b"\x07");
    // The band badges the level the pane asked for, which is what makes this a
    // contribution to kernel chrome rather than a string the pane painted.
    tui.wait_for("ERROR");
    tui.wait_for("the pane said so");

    tui.send(b"\x02");
    tui.wait_for("Keybindings");
    tui.send(ESC);
    tui.wait_gone("Keybindings");

    assert!(tui.quit().success());
}

impl Tui {
    /// One press and release of `button` at a 0-based cell, as SGR reports.
    ///
    /// Button 0 is the left, 2 the right — the numbers xterm sends, which is
    /// the layer this has to start at: the whole road from the escape sequence
    /// to the hook is what is being asserted, so a `MouseEvent` built in
    /// process would skip the part that was missing.
    fn press(&mut self, button: u8, (x, y): (u16, u16)) {
        let (px, py) = (x + 1, y + 1);
        self.send(format!("\x1b[<{button};{px};{py}M").as_bytes());
        self.send(format!("\x1b[<{button};{px};{py}m").as_bytes());
    }
}

/// A right press travels from the terminal to the `on_context` of the pane
/// that painted the node under it, and a left one still reaches `on_click`.
///
/// `tests/mouse.rs` calls both hooks directly, with a plugin index it picked
/// and a `Click` it built, which proves the hooks are separate and nothing
/// about the road to them. That road is entirely the binary's: the
/// mouse-reporting mode it turns on, `crossterm` reading button 2 as
/// `Down(Right)`, `on_mouse` sending it to `on_context_click` rather than down
/// the click path, and the hit under the pointer resolving to the plugin that
/// painted it. A wire that named `on_click` for both buttons would leave every
/// in-process test green while making every pane ever written act on a right
/// press — the failure this feature exists to avoid.
///
/// Two panes answer `on_context`, so a press that reached every pane, or the
/// wrong one, repaints the bystander and fails here.
#[test]
fn the_right_button_reaches_on_context_and_the_left_one_still_reaches_on_click() {
    // The `id` is what makes the row a hit target: a pane that cannot hold
    // focus records no rect of its own, so an anonymous node would leave the
    // press landing on nothing and the test passing for the wrong reason.
    let pane = |name: &str, order: u8| {
        format!(
            r#"return {{
  name = "{name}",
  slot = "sessions",
  order = {order},
  render = function()
    return {{ type = "text", text = "tb-{name}-" .. (state.said or "none"), id = "tb-{name}" }}
  end,
  on_click = function(hit)
    state.said = "left"
    return true
  end,
  on_context = function(hit)
    state.said = "right"
    return true
  end,
}}"#
        )
    };
    let interface = interface_plus("91_hook.lua", &pane("hook", 5));
    std::fs::write(
        interface.path().join("plugins/92_other.lua"),
        pane("other", 6),
    )
    .expect("add the second pane");
    let profile = Profile::new();
    let mut tui = Tui::spawn_with(&profile, 40, 120, |cmd| {
        cmd.env("THURBOX_UI_DIR", interface.path());
    });
    tui.wait_for("tb-hook-none");
    tui.wait_for("tb-other-none");

    // `wait_for` is the assertion: the pane repainted, and what it painted says
    // which hook ran. A right press routed to the click path would paint
    // `tb-hook-left` instead, and this would fail on the timeout rather than
    // pass quietly.
    tui.press(2, tui.find("tb-hook-none"));
    tui.wait_for("tb-hook-right");
    tui.find("tb-other-none");

    tui.press(2, tui.find("tb-other-none"));
    tui.wait_for("tb-other-right");
    tui.find("tb-hook-right");

    tui.press(0, tui.find("tb-hook-right"));
    tui.wait_for("tb-hook-left");
    tui.find("tb-other-right");

    let status = tui.quit();
    assert!(status.success(), "exit must be clean: {status:?}");
}

/// A float pinned where a right press landed, closed by a press anywhere else.
const PINNED: &str = r#"return {
  name = "pinned",
  slot = "float",
  order = 91,
  floats = true,
  focusable = false,
  render = function()
    if not store.pinned then
      return { type = "text", text = "" }
    end
    return { float = { at = store.pinned, cols = 12, rows = 1 }, type = "text", text = "tb-pinned" }
  end,
  on_outside = function(hit)
    store.pinned = nil
    store.outside = (store.outside or 0) + 1
    return true
  end,
}"#;

/// Opens `pinned` at its right press, and paints what reached it.
const PIN_OPENER: &str = r#"return {
  name = "opener",
  slot = "sessions",
  order = 5,
  render = function()
    return {
      type = "text",
      text = "tb-opener-" .. (state.heard or "none") .. "-" .. tostring(store.outside or 0),
      id = "tb-opener",
    }
  end,
  on_click = function(hit)
    state.heard = "left"
    return true
  end,
  on_context = function(hit)
    store.pinned = { x = hit.screen_x, y = hit.screen_y }
    return true
  end,
}"#;

/// The whole road a context menu takes, on the real binary: the screen cell
/// reaches `on_context`, the float opens on that cell, and a press that misses
/// it is told to the float and to nobody else — the pane under that press must
/// not hear it, or closing a menu would also act on whatever was beneath.
#[test]
fn a_float_opens_where_it_was_asked_and_closes_on_a_press_elsewhere() {
    let interface = interface_plus("92_opener.lua", PIN_OPENER);
    std::fs::write(interface.path().join("plugins/91_pinned.lua"), PINNED).expect("add pinned");
    let profile = Profile::new();
    let mut tui = Tui::spawn_with(&profile, 40, 120, |cmd| {
        cmd.env("THURBOX_UI_DIR", interface.path());
    });
    tui.wait_for("tb-opener-none-0");
    let (x, y) = tui.find("tb-opener-none-0");

    tui.press(2, (x + 4, y));
    tui.wait_for("tb-pinned");
    assert_eq!(
        tui.find("tb-pinned"),
        (x + 4, y),
        "the float opens on the pressed cell"
    );

    // Left of the float, on the opener itself: swallowed, and told to the float.
    tui.press(0, (x, y));
    tui.wait_gone("tb-pinned");
    tui.wait_for("tb-opener-none-1");

    let status = tui.quit();
    assert!(status.success(), "exit must be clean: {status:?}");
}

/// A press on a band's pill runs the pill — it is not a pane, and a float never
/// held it — but it is still a press that missed the float, so the float is told.
/// Without that a menu left open under the Help modal is still there when the
/// modal closes, although a press landed elsewhere.
#[test]
fn a_press_on_a_band_pill_is_told_to_the_float_it_missed() {
    let interface = interface_plus("92_opener.lua", PIN_OPENER);
    std::fs::write(interface.path().join("plugins/91_pinned.lua"), PINNED).expect("add pinned");
    let profile = Profile::new();
    let mut tui = Tui::spawn_with(&profile, 40, 120, |cmd| {
        cmd.env("THURBOX_UI_DIR", interface.path());
    });
    tui.wait_for("tb-opener-none-0");
    let (x, y) = tui.find("tb-opener-none-0");
    tui.press(2, (x + 4, y));
    tui.wait_for("tb-pinned");
    // The footer band's pill, not the agent pane's "F1 Help" hint above it.
    tui.press(0, tui.find("Help · F1"));
    tui.wait_gone("tb-pinned");
    tui.wait_for("tb-opener-none-1");

    let status = tui.quit();
    assert!(status.success(), "exit must be clean: {status:?}");
}

/// A pane in the session column that paints what it heard, and from which node.
fn listening_pane(name: &str, order: u8) -> String {
    format!(
        r#"return {{
  name = "{name}",
  slot = "sessions",
  order = {order},
  render = function()
    return {{ type = "text", text = "tb-{name}-" .. (state.heard or "none"), id = "{name}" }}
  end,
  on_click = function(hit)
    state.heard = (state.heard or "") .. "L:" .. tostring(hit.id)
    return true
  end,
  on_context = function(hit)
    state.heard = (state.heard or "") .. "R:" .. tostring(hit.id)
    return true
  end,
}}"#
    )
}

/// A press read in the same batch as a reload reaches no pane, rather than the
/// one that now sits at the index the last paint recorded (#1118).
///
/// Click targets name plugins by index, and a reload rebuilds the vector those
/// indices point into. Removing a pane that sorts before `aim` moves `near` into
/// `aim`'s old index, so a press resolved against the previous paint reaches
/// `near` carrying `aim`'s node id. The reload and both presses are one write
/// so `drain_input` handles all three before the next paint records fresh
/// targets — the same window a watcher reload leaves open between a paint and
/// the press that follows it.
#[test]
fn a_press_right_after_a_reload_never_reaches_a_pane_that_did_not_paint_it() {
    let interface = interface_plus("06_tbgone.lua", &listening_pane("gone", 6));
    let plugins = interface.path().join("plugins");
    std::fs::write(plugins.join("07_tbaim.lua"), listening_pane("aim", 7)).expect("add aim");
    std::fs::write(plugins.join("08_tbnear.lua"), listening_pane("near", 8)).expect("add near");
    let profile = Profile::new();
    let mut tui = Tui::spawn_with(&profile, 40, 120, |cmd| {
        cmd.env("THURBOX_UI_DIR", interface.path());
    });
    tui.wait_for("tb-gone-none");
    tui.wait_for("tb-near-none");
    let (x, y) = tui.find("tb-aim-none");

    // Written at once, well inside the watcher's debounce, so the reload the
    // deletion schedules cannot repaint before F10 is read.
    std::fs::remove_file(plugins.join("06_tbgone.lua")).expect("remove gone");
    let (px, py) = (x + 1, y + 1);
    let mut batch = F10.to_vec();
    for button in [2, 0] {
        batch.extend(format!("\x1b[<{button};{px};{py}M\x1b[<{button};{px};{py}m").as_bytes());
    }
    tui.send(&batch);
    tui.wait_gone("tb-gone-");

    // Events are handled in order, so once `aim` has painted this later press
    // nothing from the batch can still be on its way to `near`.
    tui.press(0, tui.find("tb-aim-"));
    tui.wait_for("tb-aim-L:aim");
    let frame = tui.frame();
    assert!(
        frame.contains("tb-near-none"),
        "a press after a reload reached a pane that did not paint the node:\n{frame}"
    );

    let status = tui.quit();
    assert!(status.success(), "exit must be clean: {status:?}");
}

/// Record a trust grant for one interface file, as the settings modal would.
///
/// Both spellings of the path are granted: trust is keyed by the absolute path
/// the binary resolved, and a tempdir reached through a symlink has two.
fn trust(profile: &Profile, interface: &Path, file: &str, contents: &str) {
    let digest = thurbox::kernel::bundled::digest(contents);
    let raw = interface.join(file);
    let canonical = raw.canonicalize().expect("canonicalize");
    std::fs::write(
        profile.path("config/ui.json"),
        format!(r#"{{"trusted": {{ {raw:?}: "{digest}", {canonical:?}: "{digest}" }}}}"#),
    )
    .expect("seed trust");
}

#[test]
fn a_sessionless_run_is_refused_before_queueing_and_local_runs_complete() {
    let body = r#"return {
  name = "run-contract", slot = "sessions", capabilities = { "run" },
  render = function()
    if not run then return { type = "text", text = "untrusted" } end
    local accepted, err = pcall(run, "missing", "echo SHOULD-NOT-RUN", { ttl = 60 })
    if not state.asked then
      run("local", "echo LOCAL-RUN-OK", { machine = "local", ttl = 60 })
      state.asked = true
    end
    local got = (thurbox.runs or {})["local"]
    return { type = "box", children = {
      { type = "text", len = 1, text = accepted and "MISSING-ACCEPTED" or tostring(err) },
      { type = "text", len = 1, text = got and (got.stdout or got.error or got.state) or "pending" },
    } }
  end,
}"#;
    let interface = interface_with("plugins/10_sessions.lua", body);
    std::fs::write(
        interface.path().join("layout.lua"),
        "return function() return { children = { { slot = 'sessions' } } } end",
    )
    .expect("layout");
    let profile = Profile::new();
    trust(&profile, interface.path(), "plugins/10_sessions.lua", body);
    let mut tui = Tui::spawn_with(&profile, 24, 140, |cmd| {
        cmd.env("THURBOX_UI_DIR", interface.path());
    });
    tui.wait_for("opts.session is required unless opts.machine is \"local\"");
    tui.wait_for("LOCAL-RUN-OK");
    assert!(!tui.frame().contains("MISSING-ACCEPTED"));
    assert!(tui.quit().success());
}

/// #1467: trust and the disabled set leave `ui.json` for a store of their own.
/// A profile written before the move must keep every decision — the first
/// start migrates it, and the second start, reading only the new store, must
/// still grant `run` to the trusted pane and keep the disabled one off.
#[test]
fn legacy_trust_and_disabled_choices_move_to_their_own_store_and_still_apply() {
    let body = r#"return {
  name = "migrated", slot = "sessions", capabilities = { "run" },
  render = function()
    if not run then return { type = "text", text = "MIGRATED-UNTRUSTED" } end
    if not state.asked then
      run("m", "printf MIGRATED-RUN-OK", { machine = "local", ttl = 60 })
      state.asked = true
    end
    local got = (thurbox.runs or {}).m
    return { type = "text", text = got and (got.stdout or got.state) or "pending" }
  end,
}"#;
    let off = r#"return {
  name = "off", slot = "off",
  render = function() return { type = "text", text = "OFF-PANE-DREW" } end,
}"#;
    let interface = interface_with("plugins/10_sessions.lua", body);
    let off_path = interface.path().join("plugins/91_off.lua");
    std::fs::write(&off_path, off).expect("off pane");
    std::fs::write(
        interface.path().join("layout.lua"),
        "return function() return { children = { { slot = 'sessions' }, { slot = 'off' } } } end",
    )
    .expect("layout");
    let profile = Profile::new();
    let digest = thurbox::kernel::bundled::digest(body);
    let raw = interface.path().join("plugins/10_sessions.lua");
    let canonical = raw.canonicalize().expect("canonicalize");
    let ui_json = profile.path("config/ui.json");
    std::fs::write(
        &ui_json,
        format!(
            r#"{{"pills": {{"sessions.create": 7}},
"trusted": {{ {raw:?}: "{digest}", {canonical:?}: "{digest}" }},
"disabled": [{off_path:?}]}}"#
        ),
    )
    .expect("seed a legacy ui.json");

    for start in ["migrating", "reading the store"] {
        let mut tui = Tui::spawn_with(&profile, 24, 140, |cmd| {
            cmd.env("THURBOX_UI_DIR", interface.path());
        });
        tui.wait_for("MIGRATED-RUN-OK");
        assert!(
            !tui.frame().contains("OFF-PANE-DREW"),
            "{start}: the disabled pane drew\n{}",
            tui.frame()
        );
        assert!(tui.quit().success());

        let store: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(profile.path("config/plugin-state.json"))
                .unwrap_or_else(|e| panic!("{start}: no plugin-state.json: {e}")),
        )
        .expect("store json");
        assert!(
            !store["trusted"][raw.to_string_lossy().as_ref()].is_null(),
            "{start}: the grant did not move: {store}"
        );
        assert_eq!(
            store["disabled"],
            serde_json::json!([off_path]),
            "{start}: the disabled set did not move"
        );
        let ui: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&ui_json).expect("ui.json"))
                .expect("ui json");
        assert!(
            ui.get("trusted").is_none() && ui.get("disabled").is_none(),
            "{start}: ui.json still carries trust or the disabled set: {ui}"
        );
        assert_eq!(ui["pills"]["sessions.create"], 7, "{start}: {ui}");
    }
}

/// Keep the real server and its output alive while closing only the control
/// client's input. A burst cannot reliably fill a queue drained on another
/// thread; a pipe with no reader reliably makes that writer fail instead.
struct CutProgramInput {
    control: std::fs::File,
    root: PathBuf,
}

impl CutProgramInput {
    fn install(profile: &Profile) -> Self {
        use std::os::unix::fs::OpenOptionsExt;

        let real = Command::new("sh")
            .args(["-c", "command -v tmux"])
            .output()
            .expect("find tmux");
        assert!(real.status.success());
        let real = PathBuf::from(String::from_utf8(real.stdout).unwrap().trim());
        let root = profile.path("input-relay");
        std::fs::create_dir(&root).expect("relay directory");
        let fifo = root.join("control");
        assert!(Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .unwrap()
            .success());
        let control = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(&fifo)
            .expect("control fifo");
        let shim = profile.bin.join("tmux");
        std::fs::write(
            &shim,
            format!(
                r#"#!/bin/sh
case " $* " in
  *" -C attach-session "*)
    cd {root} || exit 1
    : > started
    mkfifo up down || exit 1
    exec 3<>up
    # Dash redirects stdin of async commands to /dev/null before <&0.
    # Preserve the incoming pipe on a separate descriptor for the input pump.
    exec 4<&0
    up= down= mux=
    cleanup() {{
      for pid in "$up" "$down" "$mux"; do
        [ -z "$pid" ] || kill "$pid" 2>/dev/null
      done
      wait
      exec 3>&-
      : > done
    }}
    trap cleanup EXIT
    {real} "$@" <up >down 3>&- 4<&- & mux=$!
    cat <down 3>&- 4<&- & down=$!
    cat <&4 >&3 3>&- 4<&- & up=$!
    exec 4<&- 0</dev/null
    : > ready
    while IFS= read -r request; do
      case "$request" in
        cut) kill "$up"; wait "$up"; up=; : > cut ;;
        stop) break ;;
      esac
    done <control
    ;;
  *) exec {real} "$@" ;;
esac
"#,
                root = shell_word(&root),
                real = shell_word(&real),
            ),
        )
        .expect("tmux input relay");
        std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o700)).unwrap();
        Self { control, root }
    }

    fn cut(&mut self, tui: &Tui) {
        tui.wait_until("the control relay to attach", |_| {
            self.root.join("ready").exists()
        });
        self.control.write_all(b"cut\n").expect("cut input");
        tui.wait_until("the input relay to close", |_| {
            self.root.join("cut").exists()
        });
    }
}

impl Drop for CutProgramInput {
    fn drop(&mut self) {
        // The supervisor owns and reaps its pumps, including on a test panic.
        // Its held FIFO descriptor keeps server input open until this request.
        let _ = self.control.write_all(b"stop\n");
        let deadline = Instant::now() + WAIT;
        while self.root.join("started").exists() && !self.root.join("done").exists() {
            if Instant::now() >= deadline {
                eprintln!("input relay did not acknowledge teardown");
                break;
            }
            std::thread::sleep(Duration::from_millis(40));
        }
    }
}

fn configured_host_run_profile() -> Profile {
    let profile = Profile::new();
    std::fs::write(
        profile.path("config/hosts.toml"),
        r#"[[hosts]]
name = "local"
destination = "fixture-host"
share_sessions = false
ssh_opts = ["-o", "HostKeyAlias=fixture"]
"#,
    )
    .expect("hosts");
    let script = profile.bin.join("ssh");
    std::fs::write(
        &script,
        format!(
            r#"#!/bin/sh
printf '%s\n' "$*" >> {calls}
while [ "$1" = '-o' ]; do shift 2; done
[ "$1" = 'fixture-host' ] || exit 98
shift
case "$*" in
  *HOST-API-STALL*) printf '%s\n' "$$" > {pid}; exec sleep 30 ;;
  *HOST-API-OFFLINE*) printf '%s\n' 'fixture transport unavailable' >&2; exit 255 ;;
  *HOST-API-*) cd "$HOME" || exit 97; eval "exec $*" ;;
  *) exit 96 ;;
esac
"#,
            calls = shell_word(&profile.path("host-run-calls")),
            pid = shell_word(&profile.path("host-run-pid")),
        ),
    )
    .expect("transport fixture");
    std::fs::set_permissions(script, std::fs::Permissions::from_mode(0o700)).expect("chmod");
    profile
}

const CONFIGURED_HOST_RUN: &str = r#"return {
  name = "host-run", slot = "sessions", capabilities = { "run" },
  render = function()
    if not run then return { type = "text", text = "HOST-RUN-UNTRUSTED" } end
    local ok, err = pcall(function()
      run("host:local:ok", [[printf 'HOST-API-OK:%s' "$PWD"]], { machine = "host:local", ttl = 60 })
      run("host:local:offline", "echo HOST-API-OFFLINE", { machine = "host:local", ttl = 60 })
      run("host:local:stall", "echo HOST-API-STALL", { machine = "host:local", ttl = 60, timeout = 0.25 })
      run("host:local:cap", "printf HOST-API-CAP; head -c 300000 /dev/zero", { machine = "host:local", ttl = 60 })
      run("host:missing", "echo HOST-API-MUST-NOT-RUN", { machine = "host:missing", ttl = 60 })
    end)
    if not ok then return { type = "text", text = "HOST-RUN-REJECTED " .. tostring(err) } end
    local children = { { type = "text", len = 1, text = "sessions=" .. #thurbox.sessions } }
    for _, key in ipairs({ "ok", "offline", "stall", "cap" }) do
      local got = thurbox.runs["host:local:" .. key]
      local text = key .. " pending"
      if got and got.state == "done" then
        if key == "ok" then text = got.stdout
        elseif key == "offline" and got.status == 255 and got.stderr:find("fixture transport unavailable", 1, true) then text = "HOST-OFFLINE-OK"
        elseif key == "stall" and got.timed_out and not got.ok then text = "HOST-TIMEOUT-OK"
        elseif key == "cap" and got.truncated and #got.stdout <= 262144 then text = "HOST-CAP-OK"
        end
      end
      children[#children + 1] = { type = "text", len = 1, text = text }
    end
    local missing = thurbox.runs["host:missing"]
    if missing and missing.state == "failed" and missing.error then
      children[#children + 1] = { type = "text", len = 1, text = "HOST-MISSING-REFUSED" }
    end
    return { type = "box", children = children }
  end,
}"#;

#[test]
fn configured_host_runs_need_no_session_or_sharing_and_keep_execution_bounded() {
    let interface = interface_with("plugins/10_sessions.lua", CONFIGURED_HOST_RUN);
    std::fs::write(
        interface.path().join("layout.lua"),
        "return function() return { children = { { slot = 'sessions' } } } end",
    )
    .expect("layout");
    let profile = configured_host_run_profile();
    trust(
        &profile,
        interface.path(),
        "plugins/10_sessions.lua",
        CONFIGURED_HOST_RUN,
    );
    let mut tui = Tui::spawn_with(&profile, 24, 140, |cmd| {
        cmd.env("THURBOX_UI_DIR", interface.path());
    });
    tui.wait_for(&format!("HOST-API-OK:{}", profile.path("home").display()));
    tui.wait_for("sessions=0");
    for result in [
        "HOST-OFFLINE-OK",
        "HOST-TIMEOUT-OK",
        "HOST-CAP-OK",
        "HOST-MISSING-REFUSED",
    ] {
        tui.wait_for(result);
    }
    let calls = std::fs::read_to_string(profile.path("host-run-calls")).expect("calls");
    assert!(calls.contains("HostKeyAlias=fixture"), "{calls}");
    assert!(!calls.contains("HOST-API-MUST-NOT-RUN"), "{calls}");
    let pid: i32 = std::fs::read_to_string(profile.path("host-run-pid"))
        .expect("transport pid")
        .trim()
        .parse()
        .expect("pid");
    // The stand-in execs its sleeper: this pins local transport termination,
    // without claiming that killing SSH can terminate a remote command.
    assert_eq!(
        unsafe { libc::kill(pid, 0) },
        -1,
        "timed-out transport survived"
    );
    assert!(tui.quit().success());
}

#[test]
fn configured_host_runs_still_require_a_trusted_run_capability() {
    let interface = interface_with("plugins/10_sessions.lua", CONFIGURED_HOST_RUN);
    let profile = configured_host_run_profile();
    let mut tui = Tui::spawn_with(&profile, 24, 140, |cmd| {
        cmd.env("THURBOX_UI_DIR", interface.path());
    });
    tui.wait_for("HOST-RUN-UNTRUSTED");
    let calls = std::fs::read_to_string(profile.path("host-run-calls")).unwrap_or_default();
    assert!(!calls.contains("HOST-API-"), "{calls}");
    assert!(tui.quit().success());
}

#[test]
fn forgetting_retired_host_keys_bounds_answers_across_repeated_churn() {
    let body = r#"return {
  name = "retention", slot = "sessions", capabilities = { "run" },
  render = function()
    if not run_forget then return { type = "text", text = "FORGET-API-MISSING" } end
    state.round = state.round or 1
    local count = 0
    for _ in pairs(thurbox.runs) do count = count + 1 end
    if count > 1 then error("retired answers accumulated: " .. count) end
    if state.round > 32 then
      return { type = "text", text = count == 0 and "HOST-CHURN-BOUNDED" or "retiring" }
    end
    local key = "host:rotating-" .. state.round
    local got = thurbox.runs[key]
    if got and got.state == "done" and got.ok then
      run_forget(key)
      run_forget(key)
      state.round = state.round + 1
    else
      run(key, "printf HOST-API-CHURN", { machine = "host:rotating-" .. state.round, ttl = 60 })
    end
    return { type = "text", text = "host churn " .. state.round }
  end,
}"#;
    let interface = interface_with("plugins/10_sessions.lua", body);
    let profile = configured_host_run_profile();
    // The registry is fixed at startup. Rotate the consumer's desired hosts,
    // without implying that editing hosts.toml reloads the running registry.
    let mut hosts = String::new();
    for round in 1..=32 {
        hosts.push_str(&format!(
            "[[hosts]]\nname = \"rotating-{round}\"\ndestination = \"fixture-host\"\nshare_sessions = false\n"
        ));
    }
    std::fs::write(profile.path("config/hosts.toml"), hosts).expect("hosts");
    trust(&profile, interface.path(), "plugins/10_sessions.lua", body);
    let mut tui = Tui::spawn_with(&profile, 24, 140, |cmd| {
        cmd.env("THURBOX_UI_DIR", interface.path());
    });
    let deadline = Instant::now() + WAIT;
    while !tui.frame().contains("HOST-CHURN-BOUNDED") && Instant::now() < deadline {
        // Drive the input/repaint cycles a changing desired-host set causes,
        // rather than relying on an otherwise idle interface's redraw floor.
        tui.send(b"\x0c");
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(
        tui.frame().contains("HOST-CHURN-BOUNDED"),
        "{}",
        tui.frame()
    );
    assert!(tui.quit().success());
}

#[test]
fn forgetting_an_active_request_does_not_replace_a_reused_keys_new_answer() {
    let body = r#"return {
  name = "forget-race", slot = "sessions", capabilities = { "run" },
  keys = { { key = "ctrl+g", action = "reuse", desc = "Reuse forgotten key", scope = "global" } },
  on_action = function()
    run_forget("same")
    run_forget("same")
    run("same", "printf HOST-API-NEW", { machine = "host:local", ttl = 60 })
    return true
  end,
  render = function()
    if not run_forget then return { type = "text", text = "FORGET-API-MISSING" } end
    if not state.asked then
      run("same", "printf HOST-API-OLD", { machine = "host:local", ttl = 60 })
      state.asked = true
    end
    local got = thurbox.runs.same
    return { type = "text", text = got and (got.stdout or got.state) or "forgotten" }
  end,
}"#;
    let interface = interface_with("plugins/10_sessions.lua", body);
    let profile = configured_host_run_profile();
    let old_started = profile.path("old-started");
    let release_old = profile.path("release-old");
    let old_finished = profile.path("old-finished");
    let script = profile.bin.join("ssh");
    let original = std::fs::read_to_string(&script).expect("fixture");
    let gated = original.replacen(
        "case \"$*\" in",
        &format!(
            r#"case "$*" in
  *HOST-API-OLD*)
    touch {started}
    n=0
    while [ ! -f {release} ] && [ "$n" -lt 250 ]; do
      sleep 0.02
      n=$((n + 1))
    done
    printf HOST-API-OLD
    touch {finished}
    exit 0 ;;
"#,
            started = shell_word(&old_started),
            release = shell_word(&release_old),
            finished = shell_word(&old_finished),
        ),
        1,
    );
    std::fs::write(script, gated).expect("gated fixture");
    trust(&profile, interface.path(), "plugins/10_sessions.lua", body);
    let mut tui = Tui::spawn_with(&profile, 24, 140, |cmd| {
        cmd.env("THURBOX_UI_DIR", interface.path());
    });
    let deadline = Instant::now() + Duration::from_secs(5);
    while !old_started.exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(
        old_started.exists(),
        "old request never reached the transport: {}",
        tui.frame()
    );
    tui.send(b"\x07");
    tui.wait_for("HOST-API-NEW");
    std::fs::write(release_old, "release").expect("release old transport");
    let deadline = Instant::now() + Duration::from_secs(5);
    while !old_finished.exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(old_finished.exists(), "old transport never finished");
    // Let the real coordinator consume the transport's late completion. The
    // replacement must remain readable after repeated paints and polls.
    for _ in 0..10 {
        tui.send(b"\x0c");
        std::thread::sleep(Duration::from_millis(20));
        assert!(tui.frame().contains("HOST-API-NEW"), "{}", tui.frame());
    }
    assert!(tui.quit().success());
}

#[test]
fn forgetting_a_queued_host_request_prevents_its_transport_from_starting() {
    let body = r#"return {
  name = "queued-forget", slot = "sessions", capabilities = { "run" },
  render = function()
    if not run_forget then return { type = "text", text = "FORGET-API-MISSING" } end
    if not state.asked then
      for i = 1, 4 do
        run("busy:" .. i, "echo HOST-API-STALL", { machine = "host:local", ttl = 60, timeout = 0.25 })
      end
      run("discard", "echo HOST-API-QUEUED-MUST-NOT-START", { machine = "host:local", ttl = 60 })
      run_forget("discard")
      run_forget("discard")
      run("after", "printf HOST-API-QUEUE-DRAINED", { machine = "host:local", ttl = 60 })
      state.asked = true
    end
    if thurbox.runs.discard then error("forgotten queued answer survived") end
    local got = thurbox.runs.after
    return { type = "text", text = got and (got.stdout or got.state) or "pending" }
  end,
}"#;
    let interface = interface_with("plugins/10_sessions.lua", body);
    let profile = configured_host_run_profile();
    trust(&profile, interface.path(), "plugins/10_sessions.lua", body);
    let mut tui = Tui::spawn_with(&profile, 24, 140, |cmd| {
        cmd.env("THURBOX_UI_DIR", interface.path());
    });
    tui.wait_for("HOST-API-QUEUE-DRAINED");
    let calls = std::fs::read_to_string(profile.path("host-run-calls")).expect("calls");
    assert!(!calls.contains("HOST-API-QUEUED-MUST-NOT-START"), "{calls}");
    assert_eq!(calls.matches("HOST-API-STALL").count(), 4, "{calls}");
    assert!(tui.quit().success());
}

#[test]
fn forgetting_an_asked_key_preserves_another_plugins_answer_with_that_key() {
    let forgetting = r#"return {
  name = "forgetter", slot = "sessions", capabilities = { "run" },
  keys = {
    { key = "ctrl+g", action = "owner.forget", desc = "forget this owner's answer", scope = "global" },
  },
  on_action = function(action)
    if action ~= "owner.forget" then return false end
    run_forget("same")
    run_forget("same")
    state.forgot = true
    return true
  end,
  render = function()
    if not run_forget then return { type = "text", text = "FORGET-API-MISSING" } end
    if not state.asked then
      run("same", "printf FORGETTER-READY", { machine = "local", ttl = 60 })
      state.asked = true
    end
    local got = thurbox.runs.same
    local text = got and (got.stdout or got.state) or "pending"
    if state.forgot then text = got and "retiring" or "OWNER-EMPTY-AFTER-FORGET" end
    return { type = "text", text = text }
  end,
}"#;
    let keeping = r#"return {
  name = "keeper", slot = "keeper", capabilities = { "run" },
  render = function()
    if not state.asked then
      run("same", "printf x >> \"$HOME/owner-keeper-calls\"; printf OTHER-OWNER-SURVIVED", { machine = "local", ttl = 60 })
      state.asked = true
    end
    local got = thurbox.runs.same
    return { type = "text", text = got and (got.stdout or got.state) or "KEEPER-ANSWER-MISSING" }
  end,
}"#;
    let interface = interface_with("plugins/10_sessions.lua", forgetting);
    let second = "plugins/91_keeper.lua";
    std::fs::write(interface.path().join(second), keeping).expect("keeper");
    std::fs::write(
        interface.path().join("layout.lua"),
        "return function() return { children = { { slot = 'sessions' }, { slot = 'keeper' } } } end",
    )
    .expect("layout");
    let profile = Profile::new();
    trust(
        &profile,
        interface.path(),
        "plugins/10_sessions.lua",
        forgetting,
    );
    let grants = profile.path("config/ui.json");
    let mut config: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&grants).expect("trust")).expect("json");
    let path = interface.path().join(second);
    let digest = thurbox::kernel::bundled::digest(keeping);
    for path in [path.clone(), path.canonicalize().expect("canonicalize")] {
        config["trusted"][path.to_string_lossy().as_ref()] = serde_json::json!(digest);
    }
    std::fs::write(grants, serde_json::to_string(&config).expect("encode")).expect("trust keeper");
    let mut tui = Tui::spawn_with(&profile, 24, 140, |cmd| {
        cmd.env("THURBOX_UI_DIR", interface.path());
    });
    tui.wait_for("OTHER-OWNER-SURVIVED");
    tui.wait_for("FORGETTER-READY");
    tui.send(b"\x07");
    tui.wait_for("OWNER-EMPTY-AFTER-FORGET");
    assert!(
        tui.frame().contains("OTHER-OWNER-SURVIVED"),
        "{}",
        tui.frame()
    );
    assert_eq!(
        std::fs::read_to_string(profile.path("home/owner-keeper-calls")).expect("keeper calls"),
        "x",
        "the peer answer must stay cached without another execution"
    );
    assert!(tui.quit().success());
}

/// Types at a program it started, and counts what `command.failed` told it.
const TYPIST: &str = r#"return {
  name = "typist",
  slot = "sessions",
  order = 5,
  capabilities = { "program" },
  events = { "command.failed" },
  render = function()
    return {
      type = "text",
      text = "tb-typist " .. (state.started and 1 or 0)
        .. " absent=" .. (state.absent or 0)
        .. " shut=" .. (state.closed and "yes" or "no"),
    }
  end,
  keys = {
    { key = "ctrl+g", action = "typist.next", desc = "next step", scope = "global" },
    { key = "ctrl+u", action = "typist.missing", desc = "missing program", scope = "global" },
  },
  on_action = function(action)
    if action == "typist.missing" then
      command("program", { text = "gone", keys = "x" })
      return true
    end
    if action ~= "typist.next" then
      return false
    end
    if not state.started then
      state.started = true
      command("program", { text = "cat", repo = "cat" })
    else
      command("program", { text = "cat", keys = "x" })
    end
    return true
  end,
  on_event = function(_, payload)
    local error = payload.error or ""
    if error:find("no running program", 1, true) then
      state.absent = (state.absent or 0) + 1
    end
    if error:find("input channel closed", 1, true) then
      state.closed = true
    end
  end,
}"#;

/// A live program whose input failed is distinct from a missing program, and
/// both refusal reasons reach the plugin (#1119). Queue overflow itself is
/// pinned without a consumer by `input_channel_overflow_fails_fast_without_blocking`.
#[test]
fn a_refused_keystroke_reaches_the_plugin_with_the_reason_it_was_refused() {
    if !have_tmux() {
        eprintln!("skipping: tmux is not installed");
        return;
    }
    let interface = interface_plus("91_typist.lua", TYPIST);
    let profile = Profile::new();
    let mut relay = CutProgramInput::install(&profile);
    trust(&profile, interface.path(), "plugins/91_typist.lua", TYPIST);
    let mut tui = Tui::spawn_with(&profile, 40, 120, |cmd| {
        cmd.env("THURBOX_UI_DIR", interface.path());
    });
    tui.wait_for("tb-typist 0");
    tui.send(b"\x07");
    tui.wait_for("tb-typist 1");
    relay.cut(&tui);

    // The first write fails in the control writer; the next makes the pane
    // writer drop its receiver. Wait for that state, not a scheduling delay.
    let deadline = Instant::now() + WAIT;
    while !tui.frame().contains("tb-typist 1 absent=0 shut=yes") {
        assert!(
            !tui.frame().contains("no running program"),
            "a live program's input failure was reported as a missing program:\n{}",
            tui.frame()
        );
        if Instant::now() >= deadline {
            tui.give_up("the live program's closed input refusal");
        }
        tui.send(b"\x07");
        std::thread::sleep(Duration::from_millis(40));
    }
    let panes = profile.server.tmux(&[
        "list-panes",
        "-a",
        "-F",
        "#{pane_dead} #{pane_current_command}",
    ]);
    assert!(panes.status.success());
    assert!(
        String::from_utf8_lossy(&panes.stdout)
            .lines()
            .any(|line| line == "0 cat"),
        "the program must still be alive: {}",
        String::from_utf8_lossy(&panes.stdout)
    );
    assert!(
        !tui.frame().contains("no running program"),
        "a closed input channel was reported as a missing program:\n{}",
        tui.frame()
    );
    tui.send(b"\x15");
    tui.wait_for("tb-typist 1 absent=1 shut=yes");
    let reaped = relay.root.join("done");
    drop(relay);
    assert!(
        reaped.exists(),
        "the relay must reap its pumps and close its FIFO"
    );
    let status = tui.quit();
    assert!(status.success(), "exit must be clean: {status:?}");
}

// --- a live session ---------------------------------------------------------

fn git(dir: &Path, args: &[&str]) {
    let mut cmd = Command::new("git");
    for var in GIT_LOCATION_ENV {
        cmd.env_remove(var);
    }
    let ok = cmd
        .args(args)
        .current_dir(dir)
        .output()
        .expect("run git")
        .status
        .success();
    assert!(ok, "git {args:?} failed");
}

/// A repository with one commit — the least a session's cwd can be.
fn repo(under: &Path) -> PathBuf {
    named_repo(under, "repo")
}

/// [`repo`], under a directory name of the caller's choosing — which is the
/// label its repo row shows.
fn named_repo(under: &Path, name: &str) -> PathBuf {
    let dir = under.join(name);
    std::fs::create_dir_all(&dir).expect("mkdir");
    git(&dir, &["init", "-q", "-b", "main"]);
    git(&dir, &["config", "user.email", "t@example.com"]);
    git(&dir, &["config", "user.name", "thurbox-e2e"]);
    git(&dir, &["config", "commit.gpgsign", "false"]);
    std::fs::write(dir.join("README.md"), "# probe\n").expect("write");
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-qm", "init"]);
    dir
}

/// A profile with one `sh` session, and the binary attached to it with the
/// agent pane focused and its prompt painted — the ground every scenario that
/// drives a real terminal starts from. `None` where tmux is absent.
///
/// The "agent" is `sh`, declared in the profile's own agents.toml — thurbox is
/// agent-neutral, so a shell is as good an agent as any and the only one CI
/// has.
fn shell_session() -> Option<(Profile, Tui)> {
    shell_session_with(|_| {})
}

#[test]
fn the_terminal_title_stays_visible_in_a_narrow_session_list() {
    let Some((_profile, mut tui)) = shell_session() else {
        return;
    };
    tui.send(b"printf '\\033]0;Plan tests\\007'\r");
    tui.wait_until("the terminal title beside its session name", |_| {
        let Some((_, y)) = tui.session_row("probe") else {
            return false;
        };
        let row = tui.row(y);
        let body = row
            .strip_prefix('│')
            .or_else(|| row.strip_prefix('┃'))
            .unwrap();
        let sidebar = body.split(['│', '┃']).next().unwrap();
        sidebar.contains("Plan tests")
    });
    assert!(tui.quit().success());
}

#[test]
fn session_host_arrows_reach_focused_agent_pane() {
    let Some((_profile, mut tui)) = shell_session() else {
        return;
    };
    tui.send(b"stty -echo -icanon min 1 time 0; printf '\\033[2J\\033[HARROW-READY\\n'; cat -v\r");
    tui.wait_until("raw agent reader", |frame| {
        frame.contains("ARROW-READY") && !frame.contains("stty -echo")
    });
    tui.send(b"\x1b[D\x1b[C");
    tui.wait_for("^[[D^[[C");
    assert!(tui.quit().success());
}

/// The same, with the binary's environment adjusted — for the cases where what
/// is being tested is what thurbox does with the machine it thinks it is on.
fn shell_session_with(adjust: impl FnOnce(&mut Command)) -> Option<(Profile, Tui)> {
    shell_session_prepared(|_| {}, adjust)
}

/// The same, with `prepare` run over the profile after the session exists and
/// before the binary starts — for a scenario about what a start finds on disk.
fn shell_session_prepared(
    prepare: impl FnOnce(&Profile),
    adjust: impl FnOnce(&mut Command),
) -> Option<(Profile, Tui)> {
    shell_session_prepared_on_branch(None, prepare, adjust)
}

fn shell_session_prepared_on_branch(
    branch: Option<&str>,
    prepare: impl FnOnce(&Profile),
    adjust: impl FnOnce(&mut Command),
) -> Option<(Profile, Tui)> {
    if !have_tmux() {
        eprintln!("skipping: tmux is not installed");
        return None;
    }
    let profile = Profile::new();
    profile.real_fleet()?;
    let repo = repo(profile.root.path());
    if let Some(branch) = branch {
        git(&repo, &["switch", "-q", "-c", branch]);
    }
    profile.fleet_session("probe", &repo);
    prepare(&profile);

    let tui = Tui::spawn_with(&profile, 40, 120, adjust);
    tui.wait_for("probe");

    // The agent pane has focus at boot, and the action band names the focused
    // pane; the prompt is the attach. Both are waited for, because a keystroke
    // sent before either goes to the list or to nothing.
    tui.wait_until("the agent pane to be the focused one", |frame| {
        frame
            .lines()
            .last()
            .is_some_and(|band| band.trim_start().starts_with("Agent"))
    });
    tui.wait_for("$ ");
    Some((profile, tui))
}

/// thurbox keeps no session state of its own (ADR-41): the interface starts
/// on Fleet, shows Fleet's session and runs its terminal, and leaves no
/// database in its data directory.
#[test]
fn the_interface_runs_a_fleet_session_with_no_database() {
    let Some((profile, mut tui)) = shell_session() else {
        return;
    };
    tui.send(b"echo tb-fleet-\"\"marker\r");
    tui.wait_for("tb-fleet-marker");
    assert!(tui.quit().success());
    let db = profile.path("data/thurbox.db");
    assert!(!db.exists(), "the interface wrote {}", db.display());
}

#[test]
fn a_session_shows_its_terminal_and_takes_keystrokes() {
    // The product, end to end: a session created headlessly appears in the
    // list, its pane is attached and painted, and a keystroke sent to the
    // focused terminal reaches the process behind it. The "agent" is `sh`,
    // declared in the profile's own agents.toml — thurbox is agent-neutral,
    // so a shell is as good an agent as any and the only one CI has.
    let Some((_profile, mut tui)) = shell_session() else {
        return;
    };

    // Typed into the focused terminal. The echo is the assertion: the marker
    // is printed by the shell, so seeing it means the pane was attached,
    // painted and wired for input — and that the letters reached the pty
    // rather than the session list, whose single-letter chords include `r`
    // (restart) and `d` (delete). Either firing here kills the pane the
    // marker was typed into, so a routing regression cannot pass this.
    tui.send(b"echo tb-e2e-\"\"marker\r");
    tui.wait_for("tb-e2e-marker");

    let status = tui.quit();
    assert!(status.success(), "exit must be clean: {status:?}");
}
#[test]
fn configured_unavailable_multiplexer_is_named_in_the_tui_create_flow() {
    let profile = Profile::new();
    std::fs::write(
        profile.path("config/settings.toml"),
        "multiplexer = \"herdr\"\n[features]\nautomations = false\nversion_check = false\nauto_update = false\n",
    )
    .expect("set unavailable backend");
    let mut tui = Tui::spawn(&profile, 40, 120);
    tui.wait_for("No sessions yet");
    tui.send(b"\x0e");
    tui.wait_for("herdr is unavailable");
    assert!(tui.quit().success());
}

#[test]
fn search_finds_text_that_scrolled_away_and_opens_the_session_on_it() {
    // The failure search was rebuilt for: a prompt typed earlier has scrolled
    // off the screen, and searching for it found nothing, because only the
    // visible screen was searched. So the marker is printed and then pushed
    // three hundred lines up, found from the strip, and opened — and opening
    // it has to land ON it, scrolled back, not merely focus the session.
    // Colour suppression must not erase the inverse mark when the line lands.
    let Some((_profile, mut tui)) = shell_session_with(|cmd| {
        cmd.env("NO_COLOR", "1");
    }) else {
        return;
    };
    // Quoted apart on the command line, so the only line that spells the
    // marker whole is the one the shell prints.
    tui.send(b"echo tb-\"\"findme; seq 1 300\r");
    tui.wait_for("300");
    tui.wait_gone("tb-findme");

    tui.send(CTRL_SLASH);
    tui.wait_for("Search");
    tui.send(b"tb-findme");
    // A result row names its session, how far back the hit is, and the line.
    tui.wait_until("a result row for the scrolled-away line", |frame| {
        frame
            .lines()
            .any(|line| line.contains("probe") && line.contains('↑') && line.contains("tb-findme"))
    });

    tui.send(b"\r");
    // Landed: the strip is gone, the terminal is scrolled back (its title
    // carries the offset) and the printed line is back on screen — beside the
    // thick border, because opening a result hands the terminal focus.
    tui.wait_until("the session scrolled to the match", |frame| {
        !frame.contains("Search")
            && frame.contains("↑]")
            && frame.lines().any(|line| line.contains("┃tb-findme "))
    });
    tui.wait_until("the landed line to be highlighted", |frame| {
        frame
            .lines()
            .enumerate()
            .find(|(_, line)| line.contains("┃tb-findme "))
            .is_some_and(|(row, line)| {
                let byte = line.find("┃tb-findme").expect("the landed line") + "┃".len();
                let column = unicode_width::UnicodeWidthStr::width(&line[..byte]);
                tui.inverse_at(row as u16, column as u16)
            })
    });
    // And the line is marked, so a long screen of output does not leave you
    // hunting for the row you were brought to.
    let frame = tui.frame();
    let (y, line) = frame
        .lines()
        .enumerate()
        .find(|(_, line)| line.contains("┃tb-findme "))
        .expect("the landed line");
    let byte = line.find("┃tb-findme").expect("the landed line") + "┃".len();
    let x = unicode_width::UnicodeWidthStr::width(&line[..byte]);
    assert!(
        tui.inverse_at(y as u16, x as u16),
        "the landed line is not highlighted:\n{frame}"
    );
    assert!(tui.quit().success());
}

#[test]
fn ctrl_d_reaches_a_live_agent_as_its_eof() {
    // The other side of the rule above: while the agent is live the chord is
    // still its EOF, not a delete. Pressing it ends `sh` — which is what EOF
    // does — but the session stays in the list, because the keystroke went to
    // the pty and never to the list's delete. Were the dead-pane exception
    // firing on a live pane, the row would be gone instead.
    let Some((_profile, mut tui)) = shell_session() else {
        return;
    };

    tui.send(b"\x04");
    // The assertion is the negative: the row is still there, so the chord
    // reached the pty and was not spent on a delete.
    tui.wait_until_quiet();
    assert!(
        tui.session_row("probe").is_some(),
        "Ctrl+D to a live agent must not delete its session:\n{}",
        tui.frame()
    );

    let status = tui.quit();
    assert!(status.success(), "exit must be clean: {status:?}");
}

#[test]
fn ctrl_d_over_a_live_shell_reaches_it_though_the_agent_behind_it_died() {
    // The shell is a second pane, addressed `<id>#shell`, and the two panes can
    // die apart: an agent that ran `/exit` is dead while its companion shell is
    // still a live `sh`. The chord follows the surface on screen, so with the
    // shell up it must ask *the shell* whether it is dead — not the agent whose
    // suffix it shares. Judging by the agent would delete the session out from
    // under a shell the user is still typing in.
    let Some((_profile, mut tui)) = shell_session() else {
        return;
    };

    tui.send(b"exit\r");
    tui.wait_until_quiet();
    assert!(
        tui.session_row("probe").is_some(),
        "the agent must be dead and the session still listed:\n{}",
        tui.frame()
    );

    // Ctrl+T raises the companion shell — a fresh pane, so it is live even though
    // the agent it sits beside is not.
    tui.send(b"\x14");
    tui.wait_until("the shell tab to be the view", |frame| {
        frame
            .lines()
            .last()
            .is_some_and(|band| band.trim_start().starts_with("Shell"))
    });
    // The pane paints before the shell inside it has drawn its prompt, and a
    // chord sent in between would race the shell that must receive it.
    tui.wait_until_quiet();

    // 0x04 is Ctrl+D, here the live shell's EOF. Were deadness read off the
    // agent, the chord would delete the session instead; the row leaving is the
    // failure this guards.
    tui.send(b"\x04");
    tui.wait_until_quiet();
    assert!(
        tui.frame().contains("1 session(s)") && !tui.frame().contains("No sessions yet"),
        "Ctrl+D on a live shell must not delete the session behind it:\n{}",
        tui.frame()
    );

    let status = tui.quit();
    assert!(status.success(), "exit must be clean: {status:?}");
}

/// Wait until the action band names `view` as the focused pane's view.
fn wait_for_view(tui: &Tui, view: &str) {
    tui.wait_until(
        &format!("the {view} view to be the one on screen"),
        |frame| {
            frame
                .lines()
                .last()
                .is_some_and(|band| band.trim_start().starts_with(view))
        },
    );
}

// --- selection, copy, and the interrupt a shell is owed ----------------------

const OSC52: &str = "\x1b]52;c;";

/// The text an OSC 52 sequence in `out` carries, if there is one.
fn osc52_payload(out: &str) -> Option<String> {
    let start = out.find(OSC52)? + OSC52.len();
    let end = out[start..].find('\x07')? + start;
    let bytes =
        base64::Engine::decode(&base64::engine::general_purpose::STANDARD, &out[start..end])
            .expect("OSC 52 payload is base64");
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

/// A left press at a 0-based cell, `over` moves to the right and the release,
/// as one run of SGR reports — what `drain_input` reads as a single batch.
fn drag_gesture((x, y): (u16, u16), over: u16) -> Vec<u8> {
    let (px, py) = (x + 1, y + 1);
    let mut seq = format!("\x1b[<0;{px};{py}M").into_bytes();
    for cx in px + 1..=px + over {
        seq.extend_from_slice(format!("\x1b[<32;{cx};{py}M").as_bytes());
    }
    seq.extend_from_slice(format!("\x1b[<0;{};{py}m", px + over).as_bytes());
    seq
}

impl Tui {
    /// Match the session list's first framed column, excluding the selected
    /// session's name repeated in the app header and terminal title.
    fn session_row(&self, name: &str) -> Option<(u16, u16)> {
        use unicode_width::UnicodeWidthStr;

        let rows = self.screen.lock().unwrap().screen().size().0.get();
        (1..rows).find_map(|y| {
            let row = self.row(y);
            let body = row.strip_prefix('│').or_else(|| row.strip_prefix('┃'))?;
            let sidebar = body.split(['│', '┃']).next()?;
            let byte = sidebar.find(name)?;
            Some(((1 + sidebar[..byte].width()) as u16, y))
        })
    }

    /// Where `needle` is painted, as a 0-based (column, row).
    ///
    /// The column is counted in cells, not bytes: the borders to the left of
    /// a pane are multi-byte glyphs, and a byte offset lands a press several
    /// cells into the text it was aimed at.
    fn find(&self, needle: &str) -> (u16, u16) {
        let rows = self.screen.lock().unwrap().screen().size().0.get();
        (0..rows)
            .find_map(|y| {
                let row = self.row(y);
                row.find(needle)
                    .map(|byte| (row[..byte].chars().count() as u16, y))
            })
            .unwrap_or_else(|| self.give_up(&format!("{needle:?} to be on screen")))
    }

    /// A left press at a 0-based cell, dragged `over` cells to the right (none
    /// for a bare click), and released — as SGR mouse reports, which is what
    /// the binary asked the terminal for.
    fn drag(&mut self, (x, y): (u16, u16), over: u16) {
        let (px, py) = (x + 1, y + 1);
        self.send(format!("\x1b[<0;{px};{py}M").as_bytes());
        for cx in px + 1..=px + over {
            self.send(format!("\x1b[<32;{cx};{py}M").as_bytes());
        }
        self.send(format!("\x1b[<0;{};{py}m", px + over).as_bytes());
        // The frame that paints the selection is the one that reads its text.
        std::thread::sleep(Duration::from_millis(250));
    }

    /// A left drag and a chord `key`, written to the pty in one burst so they
    /// reach `drain_input` in a single batch with no paint between — the case
    /// `drag`'s trailing sleep deliberately avoids. The whole SGR gesture
    /// (press, `over` moves, release) is followed immediately by the chord
    /// byte, so a handler bound to the chord runs in the same batch as the drag
    /// that made the selection.
    fn drag_then_chord(&mut self, at: (u16, u16), over: u16, key: u8) {
        let mut seq = drag_gesture(at, over);
        seq.push(key);
        self.send(&seq);
    }

    /// `Ctrl+C`, then a marker typed straight after: the marker echoing is
    /// the shell having taken the chord as its interrupt and gone back to its
    /// prompt. What the binary wrote in between is returned for the caller to
    /// judge — an OSC 52 there is a copy that stole the chord.
    fn ctrl_c_then(&mut self, marker: &str) -> String {
        let mark = self.raw_len();
        self.send(b"\x03");
        // The interrupt has to LAND before the next keystroke is written, and
        // these two used to be back-to-back. `\x03` travels pty -> thurbox ->
        // tmux -> `sh`, and the shell answers it by abandoning the line it was
        // reading and drawing a fresh prompt; a byte that arrives while it is
        // doing that is discarded. The symptom is the command's FIRST character
        // going missing -- `sh: cho: command not found`, from a swallowed `e` --
        // so the marker never echoes and the wait below times out having
        // reported nothing about why. It only showed up on a loaded machine,
        // which is what made a race look like slowness.
        self.wait_for_output_since(mark, "the shell to answer the interrupt");
        // And then until it has finished answering. The reply is several writes
        // -- `^C`, a newline, a fresh prompt -- and a byte arriving between them
        // is discarded exactly as one arriving before the first is; the barrier
        // above only proves the reply STARTED. Waiting for the stream to stop is
        // what proves it ended, and it is the same signal for every shell.
        self.wait_until_quiet();
        self.send(format!("echo {marker}-\"\"ok\r").as_bytes());
        self.wait_for(&format!("{marker}-ok"));
        self.raw_since(mark)
    }

    /// Wait until the terminal has stopped writing.
    ///
    /// The other half of [`Self::wait_for_output_since`]: that one proves the
    /// far end started reacting, this one proves it stopped. Best-effort — a
    /// stream that never settles simply gives the time back rather than failing,
    /// because this is a barrier in front of an assertion and not the assertion.
    /// Budgeted well under [`WAIT`] for the same reason.
    fn wait_until_quiet(&self) {
        const QUIET: Duration = Duration::from_millis(150);
        let deadline = Instant::now() + Duration::from_secs(3);
        let (mut seen, mut still) = (self.raw_len(), Instant::now());
        while Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
            let now = self.raw_len();
            if now != seen {
                seen = now;
                still = Instant::now();
            } else if still.elapsed() >= QUIET {
                return;
            }
        }
    }

    /// Wait until the terminal has written *anything* since `since`.
    ///
    /// Coarser than [`Self::wait_for`] on purpose: the caller is waiting for the
    /// far end to have reacted at all, not for a particular string. What the
    /// shell emits when it takes an interrupt differs between shells and between
    /// "at an idle prompt" and "mid-command" -- `^C`, a bare newline, a fresh
    /// prompt, or some combination -- so matching on any of them would be a
    /// guess. That bytes came back is the one signal every case shares.
    ///
    /// `since` must be taken BEFORE whatever is being waited on is sent, or the
    /// echo of something already in flight satisfies it instead.
    fn wait_for_output_since(&self, since: usize, what: &str) {
        let deadline = Instant::now() + WAIT;
        while Instant::now() < deadline {
            if self.raw_len() > since {
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        self.give_up(what);
    }
}

/// A shell session whose settings turn copy-on-select off — the opt-out, under
/// which a drag only selects and `Ctrl+C` is what copies.
fn shell_session_without_copy_on_select() -> Option<(Profile, Tui)> {
    shell_session_prepared(
        |profile| {
            let settings = profile.path("config/settings.toml");
            let mut seeded = std::fs::read_to_string(&settings).expect("read settings");
            seeded.push_str("[clipboard]\ncopy_on_select = false\n");
            std::fs::write(&settings, seeded).expect("seed settings");
        },
        |_| {},
    )
}

#[test]
fn a_drag_release_copies_by_default_and_ctrl_c_stays_the_interrupt() {
    // Copy-on-select is on by default: releasing a drag is the copy, with no
    // key pressed — the one copy gesture no emulator can intercept, which is
    // what macOS needs when the terminal keeps Cmd+C for itself. `Ctrl+C`
    // after it is never a second copy; it is the shell's interrupt.
    let Some((_profile, mut tui)) = shell_session() else {
        return;
    };
    tui.send(b"echo tb-select-\"\"me\r");
    tui.wait_for("tb-select-me");
    let at = tui.find("tb-select-me");

    // A bare click copies nothing.
    let mark = tui.raw_len();
    tui.drag(at, 0);
    assert!(
        !tui.raw_since(mark).contains(OSC52),
        "a click is not a selection and must not copy"
    );

    // A drag and its release, and nothing else.
    let mark = tui.raw_len();
    tui.drag(at, 12);
    tui.wait_for("copied 1 line(s)");
    let copied = osc52_payload(&tui.raw_since(mark))
        .unwrap_or_else(|| tui.give_up("an OSC 52 sequence after the release"));
    assert_eq!(copied, "tb-select-me");

    // The selection is still on screen, and `Ctrl+C` interrupts a command
    // rather than copying it again.
    tui.send(b"sleep 30 && echo tb-not-\"\"interrupted\r");
    std::thread::sleep(Duration::from_millis(200));
    let out = tui.ctrl_c_then("tb-after-copy");
    assert!(
        !out.contains(OSC52),
        "Ctrl+C after a copy-on-select must be the interrupt; wrote:\n{out:?}"
    );
    assert!(!tui.frame().contains("tb-not-interrupted"));

    // Wide characters copy byte for byte: no space inside `漢字`. Printed
    // from octal escapes so the line editor never sees a multi-byte key.
    tui.send(b"printf 'tbw\\346\\274\\242\\345\\255\\227-\\303\\251-end\\n'\r");
    tui.wait_for("tbw漢字-é-end");
    let at = tui.find("tbw漢字-é-end");
    let mark = tui.raw_len();
    // 3 + 2×2 + 1 + 1 + 4 = 13 cells, so the release lands 12 to the right.
    tui.drag(at, 12);
    tui.wait_for_output_since(mark, "the copy of the wide line");
    tui.wait_until_quiet();
    let copied = osc52_payload(&tui.raw_since(mark))
        .unwrap_or_else(|| tui.give_up("an OSC 52 sequence for the wide line"));
    assert_eq!(copied, "tbw漢字-é-end");

    let status = tui.quit();
    assert!(status.success(), "exit must be clean: {status:?}");
}

#[test]
fn a_key_right_behind_a_release_does_not_cancel_its_copy() {
    // A terminal's selection is read off its grid, so the release can copy at
    // once. Waiting for the next paint instead lost the copy to a key queued in
    // the same batch: the key drops the selection before that paint runs.
    let Some((_profile, mut tui)) = shell_session() else {
        return;
    };
    tui.send(b"echo tb-select-\"\"me\r");
    tui.wait_for("tb-select-me");
    let at = tui.find("tb-select-me");
    let mark = tui.raw_len();
    tui.drag_then_chord(at, 11, b':');
    tui.wait_for("copied 1 line(s)");
    let copied = osc52_payload(&tui.raw_since(mark))
        .unwrap_or_else(|| tui.give_up("an OSC 52 sequence after the release"));
    assert_eq!(copied, "tb-select-me");

    let status = tui.quit();
    assert!(status.success(), "exit must be clean: {status:?}");
}

#[test]
fn a_drag_over_a_pane_with_no_grid_copies_what_it_finished_on() {
    // A pane that is not a terminal has no grid: its text is read off the
    // painted frame. A drag whose every report lands in one input batch has had
    // no paint by its release, so a copy made there would carry the text of the
    // last paint (none) instead of the selection now highlighted.
    if !have_tmux() {
        return;
    }
    let profile = Profile::new();
    let mut tui = Tui::spawn(&profile, 40, 120);
    tui.wait_for("No sessions yet");
    let at = tui.find("No sessions yet");
    let mark = tui.raw_len();
    tui.send(&drag_gesture(at, 15));
    tui.wait_for("copied 1 line(s)");
    let copied = osc52_payload(&tui.raw_since(mark))
        .unwrap_or_else(|| tui.give_up("an OSC 52 sequence after the release"));
    assert_eq!(copied, "No sessions yet");

    let status = tui.quit();
    assert!(status.success(), "exit must be clean: {status:?}");
}

#[test]
fn a_click_is_not_a_selection_so_ctrl_c_still_interrupts_the_shell() {
    // Clicking into a terminal is how it is focused, and the press used to
    // stay armed as an empty selection afterwards: every `Ctrl+C` from then on
    // was taken by the copy chord, which — finding nothing selected — pushed
    // the whole visible screen at the outer terminal as OSC 52 and never
    // reached the shell as the interrupt it was. v1's rule, restored here: a
    // press that never moved is a click, and a selection is only what was
    // dragged over.
    //
    // Run with copy-on-select turned off, which is where `Ctrl+C` is still
    // the copy chord — the opt-out has to keep that working.
    let Some((_profile, mut tui)) = shell_session_without_copy_on_select() else {
        return;
    };
    tui.send(b"echo tb-select-\"\"me\r");
    tui.wait_for("tb-select-me");
    let at = tui.find("tb-select-me");

    // A bare click, then a command to interrupt. Were the chord stolen, the
    // shell would still be in `sleep` when the marker is typed, and the
    // marker would not echo inside the wait.
    tui.drag(at, 0);
    tui.send(b"sleep 30 && echo tb-not-\"\"interrupted\r");
    std::thread::sleep(Duration::from_millis(200));
    let out = tui.ctrl_c_then("tb-click");
    assert!(
        !out.contains(OSC52),
        "a click alone must not turn Ctrl+C into a copy; wrote:\n{out:?}"
    );
    assert!(!tui.frame().contains("tb-not-interrupted"));

    // A drag is a selection, and the chord copies exactly what was dragged
    // over — as OSC 52, since a headless pty has no native clipboard.
    let released = tui.raw_len();
    tui.drag(at, 12);
    assert!(
        !tui.raw_since(released).contains(OSC52),
        "with copy_on_select = false a release must only select"
    );
    let mark = tui.raw_len();
    tui.send(b"\x03");
    tui.wait_for("copied 1 line(s)");
    let copied = osc52_payload(&tui.raw_since(mark))
        .unwrap_or_else(|| tui.give_up("an OSC 52 sequence after the copy"));
    assert_eq!(copied.trim(), "tb-select-me");

    // Any other key drops the selection and still does what it does, so the
    // next Ctrl+C is the shell's again.
    tui.drag(at, 12);
    // Wait for the key to have reached the shell and echoed back before the
    // chord follows it. Left in flight, that echo is the first thing to arrive
    // after `ctrl_c_then` takes its mark, and satisfies the barrier there in
    // place of the interrupt it is meant to be waiting for.
    let typed = tui.raw_len();
    tui.send(b":");
    tui.wait_for_output_since(typed, "the shell to echo the key that clears the selection");
    let out = tui.ctrl_c_then("tb-key");
    assert!(
        !out.contains(OSC52),
        "a key press must clear the selection; wrote:\n{out:?}"
    );

    let status = tui.quit();
    assert!(status.success(), "exit must be clean: {status:?}");
}

// --- an app's own OSC 52: the focused session writes, nobody reads ----------

/// `text` as an OSC 52 clipboard write, spelled as a `printf` format a shell
/// turns into the escape — so the line the shell echoes carries no ESC.
fn osc52_printf(target: &str, payload: &str) -> String {
    format!("printf '\\033]52;{target};{payload}\\007'")
}

fn b64(bytes: &[u8]) -> String {
    base64::Engine::encode(&base64::engine::general_purpose::STANDARD, bytes)
}

/// How many OSC 52 writes `out` carries.
fn osc52_count(out: &str) -> usize {
    out.matches(OSC52).count()
}

/// Give the binary a moment to act on output it has already read, then return
/// what it wrote since `mark`. The negative assertions read this: nothing can
/// prove a write will *never* come, only that it did not come in a window
/// several frames long.
fn settled_since(tui: &Tui, mark: usize) -> String {
    std::thread::sleep(Duration::from_millis(600));
    tui.wait_until_quiet();
    tui.raw_since(mark)
}

#[test]
fn an_app_osc52_copy_in_the_focused_session_reaches_the_outer_terminal() {
    // An agent's `/copy`, nvim's OSC 52 provider, lazygit: each writes the
    // clipboard by printing OSC 52 into its own pane. Thurbox is the process on
    // the user's machine that reads those bytes, so it is the one that has to
    // put them on the user's clipboard — tmux never hands a control-mode client
    // a selection. It was dropped: tmux kept a paste buffer and nothing reached
    // the outer terminal.
    let Some((_profile, mut tui)) = shell_session() else {
        return;
    };
    let mark = tui.raw_len();
    let line = osc52_printf("c", &b64("tb-agent-copy é".as_bytes()));
    tui.send(format!("{line}; echo tb-copied-\"\"done\r").as_bytes());
    tui.wait_for("tb-copied-done");
    tui.wait_until("the app's copy at the outer terminal", |_| {
        tui.raw_since(mark).contains(OSC52)
    });
    let out = settled_since(&tui, mark);
    assert_eq!(
        osc52_payload(&out).as_deref(),
        Some("tb-agent-copy é"),
        "the outer terminal must get the app's text, byte for byte"
    );
    // tmux stores the same write as a paste buffer and announces it with
    // `%paste-buffer-changed`; acting on that too would copy it twice.
    assert_eq!(osc52_count(&out), 1, "one copy, once; wrote:\n{out:?}");

    let status = tui.quit();
    assert!(status.success(), "exit must be clean: {status:?}");
}

#[test]
fn a_focused_apps_malformed_or_oversized_osc52_writes_change_nothing() {
    // Each of these reaches the pane's parser and none is a copy the user can
    // want: an empty write, which would wipe the clipboard; a
    // payload that is not base64, one that does not decode to UTF-8 text, one
    // larger than an OSC 52 can carry to the outer terminal whole, and a write
    // to the primary selection rather than the clipboard.
    let Some((_profile, mut tui)) = shell_session() else {
        return;
    };
    let mark = tui.raw_len();
    let writes = [
        osc52_printf("c", ""),
        osc52_printf("c", "abc"),
        osc52_printf("c", &b64(b"\xff\xfe-not-utf8")),
        osc52_printf("p", &b64(b"tb-primary-only")),
        // 75,000 bytes, past OSC52_MAX_BYTES (74,994): built by the shell so
        // the command line stays short.
        "printf '\\033]52;c;%s\\007' \"$(head -c 75000 /dev/zero | tr '\\0' a | base64 | tr -d '\\n')\""
            .to_string(),
    ];
    for write in &writes {
        tui.send(format!("{write}\r").as_bytes());
    }
    tui.send(b"echo tb-bad-writes-\"\"done\r");
    tui.wait_for("tb-bad-writes-done");
    let out = settled_since(&tui, mark);
    assert_eq!(
        osc52_count(&out),
        0,
        "no malformed write may reach the clipboard; wrote:\n{out:?}"
    );

    // And the path is live, so the silence above is a refusal and not a
    // forwarder that never ran.
    let mark = tui.raw_len();
    let line = osc52_printf("c", &b64(b"tb-good-write"));
    tui.send(format!("{line}\r").as_bytes());
    tui.wait_until("the well-formed copy at the outer terminal", |_| {
        tui.raw_since(mark).contains(OSC52)
    });
    assert_eq!(
        osc52_payload(&tui.raw_since(mark)).as_deref(),
        Some("tb-good-write")
    );

    let status = tui.quit();
    assert!(status.success(), "exit must be clean: {status:?}");
}

#[test]
fn a_valid_copy_survives_a_write_that_follows_it_and_is_refused() {
    // Two writes in one burst — the clipboard, then the primary selection
    // only — reach the forwarder in the same iteration. The refused second
    // one must not cost the first.
    let Some((_profile, mut tui)) = shell_session() else {
        return;
    };
    let mark = tui.raw_len();
    let line = format!(
        "{}; {}",
        osc52_printf("c", &b64(b"tb-kept-copy")),
        osc52_printf("p", &b64(b"tb-primary-only")),
    );
    tui.send(format!("{line}; echo tb-burst-\"\"done\r").as_bytes());
    tui.wait_for("tb-burst-done");
    tui.wait_until("the clipboard write at the outer terminal", |_| {
        tui.raw_since(mark).contains(OSC52)
    });
    let out = settled_since(&tui, mark);
    assert_eq!(osc52_payload(&out).as_deref(), Some("tb-kept-copy"));
    assert_eq!(osc52_count(&out), 1, "wrote:\n{out:?}");

    let status = tui.quit();
    assert!(status.success(), "exit must be clean: {status:?}");
}

#[test]
fn an_app_behind_a_modal_cannot_write_the_clipboard() {
    // A modal takes the keys, so the session behind it is not the one the
    // user is in, for the clipboard as for typing. Nor is its write held
    // until the modal closes.
    let Some((_profile, mut tui)) = shell_session() else {
        return;
    };
    let line = osc52_printf("c", &b64(b"tb-behind-modal"));
    tui.send(format!("sleep 1; {line}; echo tb-modal-\"\"done\r").as_bytes());
    let mark = tui.raw_len();
    tui.send(F6);
    // The modal's tab row; the footer always names "Settings".
    tui.wait_for("Interface");
    std::thread::sleep(Duration::from_millis(1500));
    tui.send(ESC);
    tui.wait_gone("Interface");
    tui.wait_for("tb-modal-done");
    let out = settled_since(&tui, mark);
    assert_eq!(
        osc52_count(&out),
        0,
        "an app behind a modal wrote the clipboard:\n{out:?}"
    );

    let status = tui.quit();
    assert!(status.success(), "exit must be clean: {status:?}");
}

/// The mouse text selection reaches a Lua pane through `thurbox.selection`.
///
/// The coordinator recomputes the selection every frame for `copy_selection`;
/// publishing it into the snapshot is what lets a pane see it at all. This is the
/// whole wire, not the module: a probe pane paints the field, and a drag over the
/// shell's own echoed line is a real selection — the copy test above proves the
/// same gesture copies exactly that text. Without the publish the field is nil
/// and the probe stays `selwire:[]`, so this fails on the timeout rather than
/// passing quietly.
///
/// The probe is deliberately NOT `pure`: `thurbox.selection` is a bare scalar, so
/// it moves no epoch and bumps no state version — a pure pane reading it live
/// would be served its cached tree until some other signal ticked. The real
/// consumer (`41_notes`) reads it in `on_key`, which is never cached; a pane that
/// wants to paint the live selection reads it every frame, which is what impure
/// means. Reading it in render here is what makes the wire observable.
#[test]
fn the_text_selection_reaches_a_pane_as_a_published_field() {
    let interface = interface_plus(
        "95_selwire.lua",
        r#"return {
  name = "selwire",
  slot = "sessions",
  order = 90,
  render = function()
    return {
      type = "text",
      text = "selwire:[" .. (thurbox.selection or "") .. "]",
      id = "selwire",
    }
  end,
}"#,
    );
    let Some((_profile, mut tui)) = shell_session_with(|cmd| {
        cmd.env("THURBOX_UI_DIR", interface.path());
    }) else {
        return;
    };

    // The field is published every frame, so it is "" before any drag — the
    // probe paints the empty selection rather than a missing field.
    tui.wait_for("selwire:[]");

    // A line the shell echoes back, aimed at by its output (the `""` keeps the
    // needle out of the command line, which still shows the quotes). The drag
    // over it is the selection.
    tui.send(b"echo tb-select-\"\"me\r");
    tui.wait_for("tb-select-me");
    let at = tui.find("tb-select-me");
    tui.drag(at, 12);

    // The assertion: the pane repainted with the dragged text, which it could
    // only have read from `thurbox.selection`.
    tui.wait_for("selwire:[tb-select-me]");

    let status = tui.quit();
    assert!(status.success(), "exit must be clean: {status:?}");
}

/// A chord that fires in the *same* input batch as the drag that made the
/// selection reads the finished selection, not the pre-drag one.
///
/// `drain_input` publishes the world once per batch and `selected_text` is
/// recomputed at paint time, so a chord queued right behind a drag — with no
/// paint between — used to read the selection as it stood before the gesture
/// (empty here). The human path (drag, see the highlight, then press) is a
/// later batch and already works; `drag_then_chord` writes the gesture and the
/// chord in one burst to pin the batch-boundary case. `on_action` echoes what
/// it read into the message band, so a stale read shows `selchord:[]` and this
/// fails on the timeout.
#[test]
fn a_chord_reads_the_selection_dragged_in_its_own_batch() {
    let interface = interface_plus(
        "95_selchord.lua",
        r#"return {
  name = "selchord",
  slot = "sessions",
  order = 90,
  render = function()
    return { type = "text", text = state.seen or "selchord-ready", id = "selchord" }
  end,
  keys = {
    { key = "ctrl+g", action = "selchord.read", desc = "read the selection", scope = "global" },
  },
  on_action = function(action)
    if action == "selchord.read" then
      state.seen = "selchord:[" .. (thurbox.selection or "") .. "]"
      return true
    end
    return false
  end,
}"#,
    );
    let Some((_profile, mut tui)) = shell_session_with(|cmd| {
        cmd.env("THURBOX_UI_DIR", interface.path());
    }) else {
        return;
    };
    tui.wait_for("selchord-ready");

    tui.send(b"echo tb-select-\"\"me\r");
    tui.wait_for("tb-select-me");
    let at = tui.find("tb-select-me");

    // Drag over the echoed line and press ctrl+g in one batch. The handler
    // reads `thurbox.selection` while it runs — which is inside this batch.
    tui.drag_then_chord(at, 12, 0x07);

    tui.wait_for("selchord:[tb-select-me]");

    let status = tui.quit();
    assert!(status.success(), "exit must be clean: {status:?}");
}

/// The top-left corners of the panes, left to right, off the first row that
/// has any: `┏` is the focused pane's frame and `╭` every other's.
fn pane_corners(frame: &str) -> String {
    frame
        .lines()
        .find(|line| line.contains('╭') || line.contains('┏'))
        .map(|line| line.chars().filter(|c| matches!(c, '╭' | '┏')).collect())
        .unwrap_or_default()
}

/// The block the agent pane paints at its terminal's cursor.
fn cursor_block_shown(frame: &str) -> bool {
    frame.contains('█')
}

#[test]
fn the_thick_frame_and_the_terminal_cursor_move_with_focus() {
    // Focus has to be readable at a glance and without colour: the focused
    // pane's frame is the thick one, and the terminal paints its cursor only
    // while it is the pane the keys go to. Both are asserted on the byte
    // stream, as characters, because that is what survives a monochrome
    // terminal.
    let Some((_profile, mut tui)) = shell_session() else {
        return;
    };

    tui.wait_until("the agent pane to wear the thick frame", |frame| {
        pane_corners(frame) == "╭┏" && cursor_block_shown(frame)
    });

    // 0x08 is Ctrl+H, the kernel's focus-cycle chord.
    tui.send(b"\x08");
    tui.wait_until("the thick frame to move to the session list", |frame| {
        pane_corners(frame) == "┏╭" && !cursor_block_shown(frame)
    });

    // 0x0c is Ctrl+L, the other direction.
    tui.send(b"\x0c");
    tui.wait_until("the thick frame to come back to the agent pane", |frame| {
        pane_corners(frame) == "╭┏" && cursor_block_shown(frame)
    });

    let status = tui.quit();
    assert!(status.success(), "exit must be clean: {status:?}");
}

/// A second occupant of the centre `switch` slot, reached only by its own key.
const CENTRE_ALTERNATE: &str = r#"return {
  name = "stand-in",
  slot = "center",
  focusable = true,
  keys = {
    { key = "ctrl+g", action = "stand-in.toggle", desc = "open the stand-in", scope = "global" },
  },
  render = function()
    return { type = "text", text = "tb-stand-in-body" }
  end,
  on_action = function(action)
    if action == "stand-in.toggle" then
      command("focus", { text = "stand-in", toggle = true })
      return true
    end
    return false
  end,
}"#;

#[test]
fn the_focus_cycle_skips_a_centre_alternate_and_its_key_still_opens_it() {
    // A pane that replaces the centre is an alternate of its switch slot, and
    // moving focus onto one is what draws it. So a Ctrl+L that stepped onto it
    // swapped the agent's terminal out from under the user on an ordinary walk
    // across the columns. The cycle stops once per column — on the slot's
    // default occupant — and the alternate is opened by its own key.
    let interface = interface_plus("91_stand_in.lua", CENTRE_ALTERNATE);
    let Some((_profile, mut tui)) = shell_session_with(|cmd| {
        cmd.env("THURBOX_UI_DIR", interface.path());
    }) else {
        return;
    };

    tui.send(b"\x08");
    wait_for_view(&tui, "Sessions");
    tui.send(b"\x0c");
    wait_for_view(&tui, "Agent");
    // Onward from the centre wraps to the list: there is no other column.
    tui.send(b"\x0c");
    wait_for_view(&tui, "Sessions");
    tui.wait_until_quiet();
    assert!(
        !tui.frame().contains("tb-stand-in-body"),
        "Ctrl+L opened the centre alternate:\n{}",
        tui.frame()
    );

    // Its own key brings it forward.
    tui.send(b"\x07");
    tui.wait_for("tb-stand-in-body");

    // And the cycle leaves it for the next column, not for its sibling …
    tui.send(b"\x08");
    wait_for_view(&tui, "Sessions");
    // … and coming back lands on the default occupant, not the alternate.
    tui.send(b"\x0c");
    wait_for_view(&tui, "Agent");
    tui.wait_until_quiet();
    let frame = tui.frame();
    assert!(
        !frame.contains("tb-stand-in-body") && cursor_block_shown(&frame),
        "Ctrl+L back into the centre reopened the alternate:\n{frame}"
    );

    let status = tui.quit();
    assert!(status.success(), "exit must be clean: {status:?}");
}

// --- the wheel over a live terminal -----------------------------------------

impl Tui {
    /// `count` wheel reports at a 0-based cell, as the SGR reports the binary
    /// asked the terminal for (xterm's wheel is buttons 64 up and 65 down).
    ///
    /// A notch is several reports and each is one line, so the count is the
    /// number of lines a real wheel would have travelled.
    fn wheel(&mut self, (x, y): (u16, u16), up: bool, count: u16) {
        let (px, py) = (x + 1, y + 1);
        let button = if up { 64 } else { 65 };
        for _ in 0..count {
            self.send(format!("\x1b[<{button};{px};{py}M").as_bytes());
        }
        std::thread::sleep(Duration::from_millis(250));
    }
}

/// Print `marker` into the focused terminal and then bury it: a hundred
/// numbered lines, which is more than any pane on a 40-row screen can show.
///
/// The marker being off screen is the precondition every scroll assertion
/// below rests on, so it is waited for rather than assumed.
fn bury_a_marker(tui: &mut Tui, marker: &str) {
    tui.send(format!("echo {marker}\r").as_bytes());
    tui.wait_for(marker);
    tui.send(b"i=1; while [ $i -le 100 ]; do echo tb-fill-$i; i=$((i+1)); done\r");
    tui.wait_for("tb-fill-100");
    tui.wait_gone(marker);
}

// Darwin's PTY input queue is smaller than the source's read buffer, so it
// cannot prequeue this burst. The vendored UnixStream regression is portable.
#[cfg(target_os = "linux")]
#[test]
fn a_prewritten_mouse_burst_is_read_without_another_wakeup() {
    const PROBE: &str = "TBX_E2E_INPUT_READINESS_PROBE";
    if std::env::var_os(PROBE).is_some() {
        use crossterm::event::{poll, read, Event, MouseEventKind};

        // The source must leave a borrowed, blocking stdin untouched.
        let flags = unsafe { libc::fcntl(0, libc::F_GETFL) };
        assert_eq!(flags & libc::O_NONBLOCK, 0);
        for index in 0..90 {
            assert!(
                poll(Duration::from_secs(1)).expect("poll input"),
                "mouse report {index} of 90 was stranded without a second write"
            );
            assert!(matches!(
                read().expect("read input"),
                Event::Mouse(mouse) if mouse.kind == MouseEventKind::ScrollUp
            ));
        }
        assert!(!poll(Duration::from_millis(20)).expect("idle poll"));
        assert_eq!(unsafe { libc::fcntl(0, libc::F_GETFL) }, flags);
        return;
    }

    let profile = Profile::new();
    let (master, slave) = openpty(40, 120);
    // Prequeue the burst before the event source exists, so a later PTY
    // delivery cannot supply a second readiness edge and hide the bug.
    let mut termios = std::mem::MaybeUninit::uninit();
    unsafe {
        assert_eq!(libc::tcgetattr(slave.as_raw_fd(), termios.as_mut_ptr()), 0);
        let mut termios = termios.assume_init();
        libc::cfmakeraw(&mut termios);
        assert_eq!(
            libc::tcsetattr(slave.as_raw_fd(), libc::TCSANOW, &termios),
            0
        );
    }
    let burst = b"\x1b[<64;50;20M".repeat(90);
    assert!(burst.len() > 1024);
    let mut writer = std::fs::File::from(master);
    assert_eq!(writer.write(&burst).expect("prewrite burst"), burst.len());
    let queued_deadline = Instant::now() + WAIT;
    loop {
        let mut queued = 0_i32;
        // A successful master write can precede delivery to the slave queue.
        assert_eq!(
            unsafe { libc::ioctl(slave.as_raw_fd(), libc::FIONREAD, &mut queued) },
            0
        );
        if queued as usize == burst.len() {
            break;
        }
        assert!(
            Instant::now() < queued_deadline,
            "prewritten burst has only {queued} bytes queued"
        );
        std::thread::sleep(Duration::from_millis(1));
    }

    let mut cmd = Command::new(std::env::current_exe().expect("test binary"));
    profile.apply(&mut cmd);
    cmd.args([
        "a_prewritten_mouse_burst_is_read_without_another_wakeup",
        "--exact",
        "--nocapture",
    ])
    .env(PROBE, "1")
    .stdin(Stdio::from(slave))
    .stdout(Stdio::piped())
    .stderr(Stdio::piped());
    let mut child = cmd.spawn().expect("input probe");
    let deadline = Instant::now() + WAIT;
    while child.try_wait().expect("probe status").is_none() {
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("input probe blocked on a drained blocking descriptor");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let output = child.wait_with_output().expect("probe output");
    assert!(
        output.status.success(),
        "input probe failed: {}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn the_wheel_scrolls_the_agents_output_back() {
    // The wheel over a terminal pane has to move that terminal's scrollback.
    // It reached the pane as a synthesized `up`/`down` keystroke, and the pane
    // that shows a live terminal is the one pane that cannot declare those —
    // they belong to the agent — so the tick resolved to nothing and the wheel
    // did nothing at all. An agent that turns on mouse tracking hid it (the
    // tick is forwarded to the pty instead), which is why it looked like it
    // only happened to some people.
    let Some((_profile, mut tui)) = shell_session() else {
        return;
    };
    bury_a_marker(&mut tui, "tb-scroll-marker");

    let at = tui.find("tb-fill-100");
    tui.wheel(at, true, 90);
    tui.wait_for("tb-scroll-marker");
    tui.wait_for("[90↑]");

    // And back down again: the wheel is not a one-way trip, and the pane
    // returns to the live bottom of the stream.
    tui.wheel(at, false, 90);
    tui.wait_for("tb-fill-100");
    tui.wait_gone("tb-scroll-marker");

    let status = tui.quit();
    assert!(status.success(), "exit must be clean: {status:?}");
}

#[test]
fn the_wheel_reaches_a_fullscreen_app_that_checks_tmux_mouse_mode() {
    let Some((_profile, mut tui)) = shell_session() else {
        return;
    };

    // Some apps suppress mouse capture when tmux reports `mouse=0`. The
    // private server must advertise mouse support so they request wheel
    // reports, which their transcript views can then handle.
    tui.send(b"stty -echo -icanon min 1 time 0; printf '\\033[?1049h'; if [ \"$(tmux display-message -p '#{mouse}')\" = 1 ]; then printf '\\033[?1000h\\033[?1006h'; fi; printf 'ALT-CODEX\\n'; cat -v\r");
    tui.wait_until("the mouse-gated alternate screen", |frame| {
        frame.contains("ALT-CODEX") && !frame.contains("stty -echo")
    });

    let at = tui.find("ALT-CODEX");
    tui.wheel(at, true, 1);
    tui.wait_for("[<64;");

    let status = tui.quit();
    assert!(status.success(), "exit must be clean: {status:?}");
}

#[test]
fn the_wheel_still_reaches_a_fullscreen_app_after_a_restart() {
    // An app turns mouse tracking on once, at startup — Codex does — and
    // never again: a repaint redraws its cells, not its modes. An interface
    // started after that adopts the pane mid-stream, so whether a wheel tick
    // is forwarded must come from the mode tmux holds for the pane, not from
    // bytes this process never read. `?1003` is the mode Codex asks for.
    let Some((profile, mut tui)) = shell_session() else {
        return;
    };
    tui.send(b"stty -echo -icanon min 1 time 0; printf '\\033[?1049h\\033[?1003h\\033[?1006hALT-CODEX\\n'; cat -v\r");
    tui.wait_until("the mouse-tracking alternate screen", |frame| {
        frame.contains("ALT-CODEX") && !frame.contains("stty -echo")
    });
    assert!(tui.quit().success());

    let mut tui = Tui::spawn(&profile, 40, 120);
    tui.wait_for("ALT-CODEX");
    tui.wait_until_quiet();
    let at = tui.find("ALT-CODEX");
    tui.wheel(at, true, 1);
    tui.wait_for("[<64;");
    assert!(tui.quit().success());
}

// --- the buttons over a tracking terminal -----------------------------------

#[test]
fn a_drag_over_a_tracking_terminal_reaches_the_program_inside() {
    // The wheel above already goes to a terminal that asked for the mouse, and
    // the buttons did not: a program that tracks the mouse and selects text
    // itself (Claude Code copies on select this way) never heard a press,
    // because thurbox spent every drag on its own selection. Once a program
    // has asked, the gesture is its: press, the moves while the button is
    // down, and the release all reach the pty, in the encoding it asked for
    // and with coordinates local to its pane.
    let Some((_profile, mut tui)) = shell_session() else {
        return;
    };

    // `cat` parks the shell so the tty's echo shows what the program is sent,
    // control bytes visibly (`ESC` as `^[`) — the only way a forwarded report
    // can be read off the screen.
    tui.send(b"echo tb-mouse-\"\"here\r");
    tui.wait_for("tb-mouse-here");
    tui.send(b"printf '\\033[?1002h\\033[?1006h'; cat\r");
    tui.wait_until_quiet();

    let at = tui.find("tb-mouse-here");
    tui.drag(at, 3);

    // SGR tells the legs apart by `Cb` and the final letter alone: 0 is the
    // left button, 32 its move flag, and only a release ends in `m`.
    tui.wait_for("[<0;");
    tui.wait_for("[<32;");
    tui.wait_until("the release to reach the program", |frame| {
        frame
            .match_indices("[<0;")
            .any(|(i, _)| frame[i..].chars().take(16).find(|c| *c == 'M' || *c == 'm') == Some('m'))
    });

    let status = tui.quit();
    assert!(status.success(), "exit must be clean: {status:?}");
}

#[test]
fn a_bare_move_reaches_a_terminal_that_asked_for_every_motion() {
    // `?1003` is the one tracking mode that wants motion with no button down
    // — hover-driven TUIs are built on it — and a bare move used to stop at
    // thurbox's own hover. With no button down there is no gesture for a
    // capture to own, so the move is routed by position, like the wheel.
    let Some((_profile, mut tui)) = shell_session() else {
        return;
    };

    tui.send(b"echo tb-hover-\"\"here\r");
    tui.wait_for("tb-hover-here");
    tui.send(b"printf '\\033[?1003h\\033[?1006h'; cat\r");
    tui.wait_until_quiet();

    // 35 is SGR's "motion, no button": 3 under the 32 move flag.
    let (x, y) = tui.find("tb-hover-here");
    tui.send(format!("\x1b[<35;{};{}M", x + 1, y + 1).as_bytes());
    tui.wait_for("[<35;");

    let status = tui.quit();
    assert!(status.success(), "exit must be clean: {status:?}");
}

fn open_repo_picker(profile: &Profile) -> Tui {
    let mut tui = Tui::spawn(profile, 40, 120);
    tui.wait_for("No sessions yet");
    tui.send(b"\x0e");
    // The multiplexer step is asked only where more than one is installed.
    tui.wait_until("the new-session wizard to be up", |frame| {
        frame.contains("Multiplexer") || frame.contains("Select Repos")
    });
    if tui.frame().contains("Multiplexer") {
        tui.send(b"\r");
    }
    tui.wait_for("Select Repos");
    tui
}

fn repo_picker_selected(frame: &str, path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap();
    frame
        .lines()
        .any(|line| line.contains("[x]") && line.contains(name))
}

fn repo_picker_fixture(profile: &Profile) {
    std::fs::create_dir(profile.path("home/plain")).expect("plain directory");
    std::fs::create_dir_all(profile.path("home/repo/subdir")).expect("repo directory");
    std::os::unix::fs::symlink("plain", profile.path("home/link")).expect("symlink");
    for args in [
        vec!["init", "-q", "home/repo"],
        vec!["init", "-q", "--bare", "home/bare"],
        vec![
            "-C",
            "home/repo",
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.com",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            "seed",
        ],
        vec![
            "-C",
            "home/repo",
            "worktree",
            "add",
            "-q",
            "-b",
            "linked",
            "../worktree",
        ],
    ] {
        let mut cmd = Command::new("git");
        profile.apply(&mut cmd);
        let output = cmd.args(&args).output().expect("git fixture");
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

fn repo_picker_accepts_directory_path_spellings(directory: &str) {
    let mut refused = Vec::new();
    for (absolute, slash) in [(false, false), (false, true), (true, false), (true, true)] {
        let profile = Profile::new();
        repo_picker_fixture(&profile);
        let path = if absolute {
            profile
                .path(&format!("home/{directory}"))
                .display()
                .to_string()
        } else {
            format!("~/{directory}")
        };
        let path = format!("{path}{}", if slash { "/" } else { "" });
        let mut tui = open_repo_picker(&profile);
        tui.send(format!("\t{path}").as_bytes());
        tui.wait_until_quiet();
        tui.send(b"\r");
        let deadline = Instant::now() + Duration::from_secs(3);
        while !repo_picker_selected(&tui.frame(), directory) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(40));
        }
        let accepted = repo_picker_selected(&tui.frame(), directory);
        eprintln!("{directory}: absolute={absolute}, slash={slash}, typed Enter: {accepted}");
        if !accepted {
            refused.push(format!(
                "{directory}, absolute={absolute}, slash={slash}: {}",
                tui.frame()
            ));
        }
        assert!(tui.quit().success());
    }
    assert!(
        refused.is_empty(),
        "refused directories:\n{}",
        refused.join("\n")
    );
}

#[test]
fn repo_picker_accepts_plain_directory_path_spellings() {
    repo_picker_accepts_directory_path_spellings("plain");
}

#[test]
fn repo_picker_accepts_git_root_path_spellings() {
    repo_picker_accepts_directory_path_spellings("repo");
}

#[test]
fn repo_picker_accepts_repo_subdirectory_path_spellings() {
    repo_picker_accepts_directory_path_spellings("repo/subdir");
}

#[test]
fn repo_picker_accepts_bare_repo_path_spellings() {
    repo_picker_accepts_directory_path_spellings("bare");
}

#[test]
fn repo_picker_accepts_worktree_path_spellings() {
    repo_picker_accepts_directory_path_spellings("worktree");
}

#[test]
fn repo_picker_accepts_symlinked_directory_path_spellings() {
    repo_picker_accepts_directory_path_spellings("link");
}

#[test]
fn repo_picker_dropdown_accepts_directory_kinds() {
    let mut refused = Vec::new();
    for directory in ["plain", "repo", "repo/subdir", "bare", "worktree", "link"] {
        let profile = Profile::new();
        repo_picker_fixture(&profile);
        let mut tui = open_repo_picker(&profile);
        tui.send(b"\t");
        tui.wait_until_quiet();
        tui.send(b"\t");
        tui.wait_for("Browse");
        tui.send(directory.as_bytes());
        tui.wait_until_quiet();
        tui.send(b"\r");
        let deadline = Instant::now() + Duration::from_secs(3);
        while !repo_picker_selected(&tui.frame(), directory) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(40));
        }
        let accepted = repo_picker_selected(&tui.frame(), directory);
        eprintln!("{directory}, dropdown Enter: {accepted}");
        if !accepted {
            refused.push(format!("{directory}: {}", tui.frame()));
        }
        assert!(tui.quit().success());
    }
    assert!(
        refused.is_empty(),
        "refused directories:\n{}",
        refused.join("\n")
    );
}

#[test]
fn repo_picker_enter_selects_a_plain_directory_from_the_dropdown() {
    let profile = Profile::new();
    std::fs::create_dir(profile.path("home/plain")).expect("plain directory");
    let mut tui = open_repo_picker(&profile);
    tui.send(b"\t");
    tui.wait_until_quiet();
    tui.send(b"\t");
    tui.wait_for("plain/");
    tui.send(b"\r");
    tui.wait_until("the plain directory to be selected", |frame| {
        repo_picker_selected(frame, "plain")
    });
    tui.send(b"\r");
    tui.wait_gone("Select Repos");
    assert!(tui.quit().success());
}

#[test]
fn repo_picker_existing_worktrees_are_collapsed_until_right_and_closed_with_left() {
    let profile = Profile::new();
    repo_picker_fixture(&profile);
    let mut tui = open_repo_picker(&profile);
    tui.send(b"\t~/repo\r");
    tui.wait_until("the repository to be selected", |frame| {
        repo_picker_selected(frame, "repo")
    });
    tui.wait_until_quiet();
    assert!(
        !tui.frame().contains("linked"),
        "existing worktrees start collapsed: {}",
        tui.frame()
    );
    tui.send(b"\x1b[Z\x1b[C");
    tui.wait_for("linked");
    tui.send(b"\x1b[B\x1b[D");
    tui.wait_gone("linked");
    tui.send(b"\x1b[C");
    tui.wait_for("linked");
    assert!(tui.quit().success());
}

#[test]
fn repo_picker_tab_completion_then_enter_selects_a_directory() {
    let profile = Profile::new();
    std::fs::create_dir(profile.path("home/plain")).expect("plain directory");
    let mut tui = open_repo_picker(&profile);
    tui.send(b"\t~/pl");
    tui.wait_until_quiet();
    tui.send(b"\t");
    tui.wait_until_quiet();
    tui.send(b"\r");
    tui.wait_until("the completed directory to be selected", |frame| {
        repo_picker_selected(frame, "plain")
    });
    assert!(tui.quit().success());
}

#[test]
fn repo_picker_shows_a_refused_path_inside_the_modal() {
    let profile = Profile::new();
    std::fs::write(profile.path("home/file"), "a regular file").expect("file");
    let mut tui = open_repo_picker(&profile);
    tui.send(b"\t~/file/");
    tui.wait_until_quiet();
    tui.send(b"\r");
    tui.wait_for("Path not found:");
    let frame = tui.frame();
    let top = frame
        .lines()
        .position(|line| line.contains("Select Repos"))
        .unwrap();
    let bottom = frame
        .lines()
        .position(|line| line.contains("[ Cancel ]"))
        .unwrap();
    let refusal = frame
        .lines()
        .position(|line| line.contains("Path not found:"))
        .unwrap();
    assert!(
        refusal > top && refusal < bottom,
        "refusal must be inside the modal:\n{frame}"
    );
    assert!(
        frame.contains("~/file/"),
        "keep the refused path for correction:\n{frame}"
    );
    assert!(tui.quit().success());
}

/// How soon a hovered affordance must light. Well under the idle frame floor
/// (250ms), because the failure it catches is a pure pane served its cached
/// tree until something unrelated moves the epoch: that still lights the
/// affordance, just late — measured at 280ms and 850ms before the fix.
const HOVER_BUDGET: Duration = Duration::from_millis(200);

#[test]
fn a_bare_move_lights_the_pill_under_it_in_a_pure_pane() {
    // The new-session flow is a `pure` pane: the kernel serves its cached tree
    // until something it reads moves. The pointer's identity is one of those
    // reads, so moving onto a pill has to move the epoch the cache is keyed on
    // — otherwise the lit pill is drawn only once something unrelated moves it,
    // and hover lags in every pure pane while every render-level test (which
    // publishes fresh each time) passes.
    let profile = Profile::new();
    let mut tui = Tui::spawn(&profile, 40, 120);
    tui.wait_for("No sessions yet");
    tui.send(b"\x0e");
    tui.wait_for("[ Cancel ]");
    tui.wait_until_quiet();

    let (x, y) = tui.find("[ Cancel ]");
    let bg = |tui: &Tui| {
        tui.screen
            .lock()
            .unwrap()
            .screen()
            .cell(y, x)
            .map(|cell| cell.bgcolor())
    };
    let resting = bg(&tui);
    // 35 is SGR's "motion, no button".
    tui.send(format!("\x1b[<35;{};{}M", x + 1, y + 1).as_bytes());
    let deadline = Instant::now() + HOVER_BUDGET;
    while bg(&tui) == resting && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_ne!(bg(&tui), resting, "the pill under the pointer must light");

    let status = tui.quit();
    assert!(status.success(), "exit must be clean: {status:?}");
}

#[test]
fn a_bare_move_lights_a_chip_on_a_docked_pure_pane() {
    // The same rule for a pane that is not a float: the agent pane is `pure`
    // and docked, so its cached tree is what the kernel paints unless the
    // pointer's identity moving is something it notices.
    let Some((_profile, mut tui)) = shell_session() else {
        return;
    };
    tui.wait_for("Shell · F8");
    tui.wait_until_quiet();
    // The chip, not the banner: the banner also says "Agent".
    let (x, y) = tui.find("Shell · F8");
    let bg = |tui: &Tui| {
        tui.screen
            .lock()
            .unwrap()
            .screen()
            .cell(y, x)
            .map(|cell| cell.bgcolor())
    };
    let resting = bg(&tui);
    tui.send(format!("\x1b[<35;{};{}M", x + 1, y + 1).as_bytes());
    let deadline = Instant::now() + HOVER_BUDGET;
    while bg(&tui) == resting && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_ne!(bg(&tui), resting, "the chip under the pointer must light");

    let status = tui.quit();
    assert!(status.success(), "exit must be clean: {status:?}");
}

#[test]
fn a_new_press_frees_a_capture_whose_release_never_came() {
    // A release is the outer terminal's to deliver, and it can fail to — a
    // focus loss mid-drag is enough in some emulators. A capture that only
    // the missing release could clear would then own every later drag, and
    // a selection made anywhere else would be typed into the old pane's pty
    // instead. The next press starts a new gesture, so it is what frees the
    // orphaned capture.
    let Some((_profile, mut tui)) = shell_session() else {
        return;
    };

    tui.send(b"echo tb-stale-\"\"here\r");
    tui.wait_for("tb-stale-here");
    tui.send(b"printf '\\033[?1002h\\033[?1006h'; cat\r");
    tui.wait_until_quiet();

    // The wait pins the capture as armed. No release follows — that absence
    // is the failure under test, not an oversight.
    let (x, y) = tui.find("tb-stale-here");
    tui.send(format!("\x1b[<0;{};{}M", x + 1, y + 1).as_bytes());
    tui.wait_for("[<0;");

    // Proving an absence needs the stream to settle: were the capture still
    // armed, the moves would echo as `[<32;`, and the quiet wait is what
    // gives them time to land before the assertion looks.
    let mark = tui.raw_len();
    let at = tui.session_row("probe").expect("the session row");
    tui.drag(at, 3);
    tui.wait_until_quiet();
    assert!(
        !tui.raw_since(mark).contains("[<32;"),
        "a drag outside the pane must not reach an orphaned capture"
    );

    let status = tui.quit();
    assert!(status.success(), "exit must be clean: {status:?}");
}

// --- the scrollbar is a control, not a decoration ---------------------------

impl Tui {
    /// The character painted at a 0-based cell, in cells rather than bytes.
    fn cell(&self, x: u16, y: u16) -> String {
        self.row(y)
            .chars()
            .nth(usize::from(x))
            .map(|c| c.to_string())
            .unwrap_or_default()
    }

    /// The first and last row a scrollbar occupies in column `x` — its caps.
    fn track_extent(&self, x: u16) -> (u16, u16) {
        let rows = self.screen.lock().unwrap().screen().size().0.get();
        let painted: Vec<u16> = (0..rows)
            .filter(|y| matches!(self.cell(x, *y).as_str(), "▲" | "▼" | "║" | "█"))
            .collect();
        match (painted.first(), painted.last()) {
            (Some(top), Some(bottom)) => (*top, *bottom),
            _ => self.give_up(&format!("a scrollbar in column {x}")),
        }
    }

    /// Press at a 0-based cell, drag straight down (or up) to `to_y`, release.
    fn drag_down(&mut self, (x, y): (u16, u16), to_y: u16) {
        let px = x + 1;
        self.send(format!("\x1b[<0;{px};{}M", y + 1).as_bytes());
        let (from, to) = (y.min(to_y), y.max(to_y));
        for cy in from..=to {
            self.send(format!("\x1b[<32;{px};{}M", cy + 1).as_bytes());
        }
        self.send(format!("\x1b[<0;{px};{}m", to_y + 1).as_bytes());
        std::thread::sleep(Duration::from_millis(250));
    }
}

// --- the WSL image probe, through the real binary ---------------------------

// --- a remote session whose link goes bad -----------------------------------

/// How long the interface is given to answer while a remote link is wedged.
///
/// A **liveness** bound, not a performance one — which is why it does not run
/// against ADR-P2's "caught by counting, not timing" or ADR-P5's refusal of a
/// startup-time gate. Neither of those excludes a wall clock as such: `WAIT`
/// above is one, and every `wait_for` in this file is a timeout. What they
/// exclude is a threshold close enough to the real value that machine variance
/// decides the verdict. This one is nowhere near: the measured answer is
/// ~270 ms and ~20 ms (ADR-P24) while the failure it catches never arrives at
/// all — measured past 30 s. The budget has to clear the first by enough to
/// survive this suite's own parallelism, which on a loaded machine delays a pty
/// test's frames by seconds (the reason `WAIT` is 20 s), and still sit far
/// below the second. It asks whether the interface answered, not how quickly.
///
/// `WAIT` itself is no use here for the opposite reason: at 20 s it is longer
/// than the `COMMAND_TIMEOUT` (10 s) bounding a single wedged round trip, so a
/// frozen interface would pass.
const RESPONSIVE: Duration = Duration::from_secs(5);

/// A path as one shell word. The profile root is a tempdir, so it is ordinary —
/// but a `TMPDIR` with a space in it would otherwise split the redirect.
fn shell_word(path: &Path) -> String {
    format!("'{}'", path.display().to_string().replace('\'', "'\\''"))
}

/// A pane that fills the plugin VM to the byte when `ctrl+g` is pressed.
///
/// The heap is held live rather than leaked, which reaches the ceiling in one
/// press. Small tables rather than one big string, because a collection's cost
/// is the objects it has to traverse.
const HOG_PANE: &str = r#"local held = {}
return {
  name = "hog",
  slot = "sessions",
  keys = {
    { key = "ctrl+g", action = "hog.fill", desc = "fill the VM", scope = "global" },
  },
  render = function(ctx)
    return { type = "text", text = (#held > 0 and "hog-full " or "hog-idle ") .. ctx.frame }
  end,
  on_action = function(action)
    if action ~= "hog.fill" then
      return false
    end
    pcall(function()
      for i = 1, 2e6 do
        held[i] = { i }
      end
    end)
    -- Then to the byte: halve the chunk whenever one no longer fits.
    local size, n = 1024 * 1024, 0
    while size >= 8 do
      n = n + 1
      local ok = pcall(function()
        held[#held + 1] = string.rep("x", size, tostring(n))
      end)
      if not ok then
        size = size // 2
      end
    end
    return true
  end,
}"#;

/// Fill the VM, then require the interface to keep answering.
///
/// The palette is drawn from the kernel's own registry, so the only thing that
/// can make it late is a loop that is not getting round. The first round trip
/// waits behind the fill itself and is not counted. Measured, eight more take
/// ~0.65 s on a healthy VM and ~14 s on a full one, every time, however long it
/// is left — `RESPONSIVE` sits far from both.
fn fill_the_vm_and_keep_answering(tui: &mut Tui) {
    tui.send(b"\x07");
    tui.send(CTRL_P);
    tui.wait_for("type to filter commands");
    tui.send(ESC);
    tui.wait_gone("type to filter commands");
    let started = Instant::now();
    for _ in 0..8 {
        tui.send(CTRL_P);
        tui.wait_for("type to filter commands");
        tui.send(ESC);
        tui.wait_gone("type to filter commands");
    }
    let took = started.elapsed();
    assert!(
        took < RESPONSIVE,
        "eight round trips through the palette took {took:?} after the plugin VM reached \
         its memory limit"
    );
}

#[test]
fn a_plugin_vm_at_its_memory_limit_does_not_freeze_the_interface() {
    // Every plugin shares one VM under the memory limit. Once its heap is at
    // the ceiling and cannot be collected below it, each allocation any pane
    // makes runs a full emergency collection and then fails anyway: every
    // frame pays one per plugin call, for as long as the process lives. That
    // was a full freeze of the interface, with nothing in the log, until it
    // was killed.
    let interface = interface_plus("91_hog.lua", HOG_PANE);
    let Some((_profile, mut tui)) = shell_session_with(|cmd| {
        cmd.env("THURBOX_UI_DIR", interface.path());
    }) else {
        return;
    };
    tui.wait_for("hog-idle");

    fill_the_vm_and_keep_answering(&mut tui);
    // On a VM of its own again, which is the only way the heap comes back.
    tui.wait_for("hog-idle");
    assert!(tui.quit().success());
}

#[test]
fn a_full_vm_is_replaced_even_when_the_interface_no_longer_loads() {
    // The rebuild reads the interface from disk, and an explicit
    // THURBOX_UI_DIR with a file that no longer loads is never swapped for the
    // bundled copy — a failed build keeps the VM that is running. When that VM
    // is the full one, keeping it is the freeze.
    let interface = interface_plus("91_hog.lua", HOG_PANE);
    let Some((_profile, mut tui)) = shell_session_with(|cmd| {
        cmd.env("THURBOX_UI_DIR", interface.path());
    }) else {
        return;
    };
    tui.wait_for("hog-idle");
    std::fs::write(
        interface.path().join("plugins").join("92_broken.lua"),
        "return {",
    )
    .expect("break the interface");
    tui.wait_for("reload failed");

    fill_the_vm_and_keep_answering(&mut tui);
    tui.wait_for("using the bundled interface");
    assert!(tui.quit().success());
}

/// A pane that holds ~230 MiB from the moment it loads — above the rebuild
/// threshold (90% of the 256 MiB ceiling) and below the ceiling itself, so it
/// works, and a rebuild cannot bring the heap down. Each VM it loads in draws
/// its own token, which is how a test sees the VM being replaced.
const RESIDENT_PANE: &str = r#"local held = {}
for i = 1, 230 do
  held[i] = string.rep(string.format("%07d|", i), 131072)
end
local token = math.random(1, 999999999)
return {
  name = "resident",
  slot = "sessions",
  render = function()
    return { type = "text", text = "resident-" .. token .. " " .. #held }
  end,
}"#;

/// The per-VM token a pane draws after `prefix`.
fn token_after(screen: &str, prefix: &str) -> Option<String> {
    let at = screen.find(prefix)? + prefix.len();
    Some(
        screen[at..]
            .chars()
            .take_while(char::is_ascii_digit)
            .collect(),
    )
}

#[test]
fn a_vm_that_a_rebuild_cannot_shrink_is_not_rebuilt_again_and_again() {
    // The memory rebuild assumes a new VM comes back small. One whose
    // interface itself holds more than the threshold comes back just as full,
    // so a rebuild on every loop iteration replaced the VM several times a
    // second, pinning a core, for as long as the process lived — while the
    // same interface ran fine below the ceiling before the rebuild existed.
    let interface = interface_plus("91_resident.lua", RESIDENT_PANE);
    let Some((_profile, mut tui)) = shell_session_with(|cmd| {
        cmd.env("THURBOX_UI_DIR", interface.path());
    }) else {
        return;
    };
    tui.wait_for("resident-");
    // Startup and at most one rebuild settle first.
    std::thread::sleep(Duration::from_secs(3));
    let settled = token_after(&tui.frame(), "resident-").expect("the resident pane is drawn");
    std::thread::sleep(Duration::from_secs(4));
    let later = token_after(&tui.frame(), "resident-").expect("the resident pane is still drawn");
    assert_eq!(
        settled,
        later,
        "the plugin VM kept being rebuilt although no rebuild could shrink it\n--- thurbox.log ---\n{}",
        tui.log_tail()
    );
    assert!(tui.quit().success());
}

/// Fill the VM to the byte from inside a plugin, holding every chunk in
/// `held`, and return how many whole MiB chunks went in first. The same
/// halving as [`HOG_PANE`]'s, as a Lua prelude several panes share.
const FILL_TO_THE_BYTE: &str = r#"local function fill(held)
  local mib = 0
  while pcall(function()
    held[#held + 1] = string.rep("m", 1024 * 1024, tostring(#held))
  end) do
    mib = mib + 1
  end
  local size, n = 512 * 1024, 0
  while size >= 8 do
    n = n + 1
    local ok = pcall(function()
      held[#held + 1] = string.rep("x", size, tostring(n))
    end)
    if not ok then
      size = size // 2
    end
  end
  return mib
end
"#;

/// A pane that loads about ten MiB under the ceiling — far above the rebuild
/// threshold — and fills the rest when `ctrl+g` is pressed. Each VM draws its
/// own token.
fn near_ceiling_pane() -> String {
    format!(
        r#"{FILL_TO_THE_BYTE}
local held = {{}}
local mib = fill(held)
-- Hand back ten of the whole MiB chunks: the VM settles ten MiB under its
-- ceiling, whatever else the interface holds.
for i = mib - 9, mib do
  held[i] = false
end
local full = false
local token = math.random(1, 999999999)
return {{
  name = "nearly",
  slot = "sessions",
  keys = {{
    {{ key = "ctrl+g", action = "nearly.fill", desc = "fill the VM", scope = "global" }},
  }},
  render = function()
    return {{ type = "text", text = (full and "nearly-full-" or "nearly-idle-") .. token }}
  end,
  on_action = function(action)
    if action ~= "nearly.fill" then
      return false
    end
    full = true
    fill(held)
    return true
  end,
}}"#
    )
}

#[test]
fn a_vm_loaded_near_its_ceiling_is_still_rebuilt_when_it_fills() {
    // An interface that loads large is left alone, but not once it is full:
    // a heap at the ceiling is the freeze, whatever size it was built at.
    // (`kernel_limits` pins the case no growth allowance can reach.)
    let interface = interface_plus("91_nearly.lua", &near_ceiling_pane());
    let Some((_profile, mut tui)) = shell_session_with(|cmd| {
        cmd.env("THURBOX_UI_DIR", interface.path());
    }) else {
        return;
    };
    tui.wait_for("nearly-idle-");
    let before = token_after(&tui.frame(), "nearly-idle-").expect("the pane is drawn");
    fill_the_vm_and_keep_answering(&mut tui);
    tui.wait_within(WAIT, "the full VM to be replaced", |frame| {
        token_after(frame, "nearly-idle-").is_some_and(|token| token != before)
    });
    assert!(tui.quit().success());
}

/// A pane that fills the VM to the byte on the first render of every VM it
/// loads in, and holds it: a rebuild cannot help, however often it runs.
fn refilling_pane() -> String {
    format!(
        r#"{FILL_TO_THE_BYTE}
local held = {{}}
return {{
  name = "refill",
  slot = "sessions",
  render = function()
    if #held == 0 then
      fill(held)
    end
    return {{ type = "text", text = "refill " .. #held }}
  end,
}}"#
    )
}

#[test]
fn an_interface_that_refills_a_fresh_vm_falls_back_to_the_bundled_one() {
    // Rebuilding a VM the interface fills again straight away is the freeze
    // with a rebuild added to every loop iteration. Once a rebuild has not
    // helped, the bundled interface runs instead, and says why.
    let interface = interface_plus("91_refill.lua", &refilling_pane());
    let Some((_profile, mut tui)) = shell_session_with(|cmd| {
        cmd.env("THURBOX_UI_DIR", interface.path());
    }) else {
        return;
    };
    tui.wait_for("using the bundled interface");
    // And it stays answerable there.
    tui.send(CTRL_P);
    tui.wait_within(
        RESPONSIVE,
        "the palette on the bundled interface",
        |frame| frame.contains("type to filter commands"),
    );
    assert!(tui.quit().success());
}

#[test]
fn an_interface_that_refills_a_fresh_vm_is_retired_when_no_fallback_can_load() {
    // The bundled interface cannot go in when its directory cannot be created.
    // Building the user's interface again instead hands it a VM to fill once
    // more, on every loop iteration: what replaces it is an empty VM.
    let interface = interface_plus("91_refill.lua", &refilling_pane());
    let profile = Profile::new();
    let blocked_temp = profile.path("blocked-temp");
    std::fs::write(&blocked_temp, "not a directory").expect("block fallback");
    let mut tui = Tui::spawn_with(&profile, 40, 120, |cmd| {
        cmd.env("TMPDIR", &blocked_temp);
        cmd.env("THURBOX_UI_DIR", interface.path());
    });
    tui.wait_for("plugin VM replaced; interface failed to load");
    std::thread::sleep(Duration::from_secs(2));
    let log = tui.log_tail();
    assert_eq!(
        log.matches("plugin VM held").count(),
        2,
        "one rebuild, then one replacement, and no more\n{log}"
    );
    tui.send(CTRL_P);
    tui.wait_within(RESPONSIVE, "the palette on the empty VM", |frame| {
        frame.contains("type to filter commands")
    });
    assert!(tui.quit().success());
}

/// A pane that fills the VM to the byte when the interface is reloaded. The
/// last of it goes in as two-byte strings made by C functions, which leave no
/// garbage: once one fails, no emergency collection can make room for the
/// next subscriber's instruction hook.
const FILLER_PANE: &str = r#"local held = {}
for i = 1, 4096 do held[i] = false end
return {
  name = "filler",
  slot = "sessions",
  events = { "interface.reloaded" },
  on_event = function()
    local n, size = 0, 1024 * 1024
    while size >= 64 do
      local made, chunk = pcall(string.rep, "x", size)
      if made then n = n + 1; held[n] = chunk else size = size // 2 end
    end
    for a = 0, 255 do
      for b = 0, 255 do
        local made, crumb = pcall(string.char, a, b)
        if not made then return end
        n = n + 1
        held[n] = crumb
      end
    end
  end,
  render = function()
    return { type = "text", text = "filler-idle" }
  end,
}"#;

/// A pane whose `interface.reloaded` handler never returns.
const SPINNER_PANE: &str = r#"return {
  name = "spinner",
  slot = "sessions",
  events = { "interface.reloaded" },
  on_event = function() while true do end end,
  render = function()
    return { type = "text", text = "spinner-idle" }
  end,
}"#;

#[test]
fn a_call_whose_instruction_budget_cannot_be_armed_does_not_freeze_the_interface() {
    // The instruction budget is a hook installed before every plugin call, and
    // installing it allocates. On a full VM that allocation fails; a call run
    // anyway has no bound, and a handler that never returns takes the render
    // loop with it. A private profile, and the harness kills the binary when a
    // wait gives up.
    let interface = interface_plus("91_filler.lua", FILLER_PANE);
    std::fs::write(
        interface.path().join("plugins").join("92_spinner.lua"),
        SPINNER_PANE,
    )
    .expect("add the spinner");
    let profile = Profile::new();
    let mut tui = Tui::spawn_with(&profile, 40, 120, |cmd| {
        cmd.env("THURBOX_UI_DIR", interface.path());
    });
    tui.wait_for("spinner-idle");

    tui.send(F10);
    // The full VM is rebuilt, filled again by the same event, and then put on
    // the bundled interface: the recovery the memory limit already had.
    tui.wait_for("using the bundled interface");
    let log = tui.log_tail();
    assert!(
        log.contains("could not be armed"),
        "the spinner's failure must say why it did not run\n{log}"
    );
    tui.send(CTRL_P);
    tui.wait_within(
        RESPONSIVE,
        "the palette on the bundled interface",
        |frame| frame.contains("type to filter commands"),
    );
    assert!(tui.quit().success());
}

/// A pane that loads small and holds ~230 MiB from its first render on, so
/// the build is small and the heap is not. Each VM draws its own token.
const LAZY_RESIDENT_PANE: &str = r#"local held = {}
local token = math.random(1, 999999999)
return {
  name = "lazy",
  slot = "sessions",
  render = function()
    if #held == 0 then
      for i = 1, 230 do
        held[i] = string.rep(string.format("%07d|", i), 131072)
      end
    end
    return { type = "text", text = "lazy-" .. token .. " " .. #held }
  end,
}"#;

#[test]
fn a_vm_that_fills_on_its_first_frame_is_rebuilt_once_then_kept() {
    // Growth since the build is what tells a leak from a large interface. One
    // that grows to its resident size on its first frame looks like a leak to
    // that test, every time: the rebuild that shows it is not one is the last.
    let interface = interface_plus("91_lazy.lua", LAZY_RESIDENT_PANE);
    let Some((_profile, mut tui)) = shell_session_with(|cmd| {
        cmd.env("THURBOX_UI_DIR", interface.path());
    }) else {
        return;
    };
    tui.wait_for("lazy-");
    std::thread::sleep(Duration::from_secs(3));
    let settled = token_after(&tui.frame(), "lazy-").expect("the lazy pane is drawn");
    std::thread::sleep(Duration::from_secs(4));
    let later = token_after(&tui.frame(), "lazy-").expect("the lazy pane is still drawn");
    assert_eq!(
        settled,
        later,
        "the plugin VM kept being rebuilt although the interface stopped growing\n--- thurbox.log ---\n{}",
        tui.log_tail()
    );
    assert!(tui.quit().success());
}

#[test]
fn the_memory_floor_keeps_the_plugins_the_user_turned_off() {
    // The floor host is built fresh and knows nothing of the disabled set, and
    // the next reload builds the user's directory with whatever it holds. A
    // plugin turned off because it no longer loads would then load again, and
    // keep the repaired interface on the floor.
    let interface = interface_plus("91_hog.lua", HOG_PANE);
    let Some((profile, tui)) = shell_session_with(|cmd| {
        cmd.env("THURBOX_UI_DIR", interface.path());
    }) else {
        return;
    };
    // Off before the binary that is tested reads it: the session above is all
    // the profile needed first.
    drop(tui);
    let off = interface.path().join("plugins").join("93_off.lua");
    std::fs::write(&off, "return {").expect("a plugin that does not load");
    std::fs::write(
        profile.path("config/ui.json"),
        format!(r#"{{"disabled": [{:?}]}}"#, off.display().to_string()),
    )
    .expect("turn the plugin off");
    let mut tui = Tui::spawn_with(&profile, 40, 120, |cmd| {
        cmd.env("THURBOX_UI_DIR", interface.path());
    });
    tui.wait_for("hog-idle");
    let broken = interface.path().join("plugins").join("92_broken.lua");
    std::fs::write(&broken, "return {").expect("break the interface");
    tui.wait_for("reload failed");

    fill_the_vm_and_keep_answering(&mut tui);
    tui.wait_for("using the bundled interface");
    std::fs::remove_file(&broken).expect("repair the interface");
    tui.wait_for("hog-idle");
    assert!(tui.quit().success());
}

#[test]
fn the_memory_floor_ends_the_programs_of_the_plugins_it_removed() {
    // A program belongs to the plugin that started it, and the floor swaps
    // every plugin out. Left running, it is a process nothing on screen can
    // reach again.
    let body = r#"local held = {}
return {
  name = "hog",
  slot = "sessions",
  capabilities = { "program" },
  keys = {
    { key = "ctrl+g", action = "hog.fill", desc = "fill the VM", scope = "global" },
  },
  render = function(ctx)
    if not state.started then
      command("program", { text = "keeper", repo = "sleep", args = { "31337421" } })
      state.started = true
    end
    return { type = "text", text = (#held > 0 and "hog-full " or "hog-idle ") .. ctx.frame }
  end,
  on_action = function(action)
    if action ~= "hog.fill" then
      return false
    end
    pcall(function()
      for i = 1, 2e6 do
        held[i] = { i }
      end
    end)
    local size, n = 1024 * 1024, 0
    while size >= 8 do
      n = n + 1
      local ok = pcall(function()
        held[#held + 1] = string.rep("x", size, tostring(n))
      end)
      if not ok then
        size = size // 2
      end
    end
    return true
  end,
}"#;
    let interface = interface_plus("91_hog.lua", body);
    let Some((profile, tui)) = shell_session_with(|cmd| {
        cmd.env("THURBOX_UI_DIR", interface.path());
    }) else {
        return;
    };
    drop(tui);
    trust(&profile, interface.path(), "plugins/91_hog.lua", body);
    let mut tui = Tui::spawn_with(&profile, 40, 120, |cmd| {
        cmd.env("THURBOX_UI_DIR", interface.path());
    });
    tui.wait_for("hog-idle");
    let running = || {
        Command::new("pgrep")
            .args(["-f", "sleep 31337421"])
            .output()
            .is_ok_and(|out| out.status.success())
    };
    let deadline = Instant::now() + WAIT;
    while !running() {
        assert!(Instant::now() < deadline, "the program never started");
        std::thread::sleep(Duration::from_millis(100));
    }
    std::fs::write(
        interface.path().join("plugins").join("92_broken.lua"),
        "return {",
    )
    .expect("break the interface");
    tui.wait_for("reload failed");

    fill_the_vm_and_keep_answering(&mut tui);
    tui.wait_for("using the bundled interface");
    let deadline = Instant::now() + WAIT;
    while running() {
        assert!(
            Instant::now() < deadline,
            "the program outlived the plugin that started it"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(tui.quit().success());
}

#[test]
fn a_full_vm_with_an_unbuildable_explicit_interface_recovers_without_false_retries() {
    full_vm_with_unbuildable_interface(true);
}

#[test]
fn a_full_vm_recovers_even_when_the_bundled_fallback_cannot_be_created() {
    full_vm_with_unbuildable_interface(false);
}

fn full_vm_with_unbuildable_interface(explicit: bool) {
    let interface = interface_plus("91_hog.lua", HOG_PANE);
    let profile = Profile::new();
    let ui = if explicit {
        interface.path().to_path_buf()
    } else {
        let ui = profile.path("config/ui");
        copy_tree(interface.path(), &ui);
        ui
    };
    // No fallback can be created beneath a regular file. This is scoped to
    // the child; test scratch and the operator's interface remain untouched.
    let blocked_temp = profile.path("blocked-temp");
    std::fs::write(&blocked_temp, "not a directory").expect("block fallback");
    let mut tui = Tui::spawn_with(&profile, 40, 120, |cmd| {
        cmd.env("TMPDIR", &blocked_temp);
        if explicit {
            cmd.env("THURBOX_UI_DIR", &ui);
        } else {
            cmd.env_remove("THURBOX_UI_DIR");
        }
    });
    tui.wait_for("hog-idle");
    let layout = ui.join("layout.lua");
    let valid_layout = std::fs::read_to_string(&layout).expect("read layout");
    for cycle in 0..3 {
        std::fs::write(&layout, "error('edited layout cannot build')").expect("break layout");
        tui.send(F10);
        tui.wait_for("edited layout cannot build");
        // Ordinary edit reloads keep the live pane available even when the new
        // layout is broken. Its key must still be able to fill the current VM.
        tui.wait_for("hog-idle");
        tui.send(b"\x07");
        tui.wait_until("memory recovery to run", |_| {
            tui.log_tail().matches("plugin VM held").count() > cycle
        });
        tui.wait_for("plugin VM replaced; interface failed to load");
        tui.wait_gone("hog-idle");
        tui.wait_gone("hog-full");

        let started = Instant::now();
        for _ in 0..8 {
            tui.send(CTRL_P);
            tui.wait_for("type to filter commands");
            tui.send(ESC);
            tui.wait_gone("type to filter commands");
        }
        assert!(
            started.elapsed() < RESPONSIVE,
            "the exhausted VM still stalls input"
        );
        let log = tui.log_tail();
        assert_eq!(
            log.matches("plugin VM replaced; interface failed to load")
                .count(),
            cycle + 1,
            "{log}"
        );
        assert!(
            !log.contains("rebuilt the interface"),
            "false rebuild success: {log}"
        );
        assert!(
            std::fs::read(&blocked_temp).is_ok(),
            "fallback blocker was changed"
        );
        assert_eq!(
            std::fs::read_to_string(&layout).unwrap(),
            "error('edited layout cannot build')"
        );

        std::fs::write(&layout, &valid_layout).expect("repair layout");
        tui.send(F10);
        tui.wait_for("hog-idle");
        tui.wait_gone("reload failed");
    }
    assert!(tui.quit().success());
}

#[test]
fn memory_fallback_keeps_disabled_bundled_plugins_disabled() {
    let interface = interface_plus("91_hog.lua", HOG_PANE);
    let profile = Profile::new();
    std::fs::write(
        profile.path("config/ui.json"),
        serde_json::json!({
            "disabled": [interface.path().join("plugins/20_agent.lua")]
        })
        .to_string(),
    )
    .expect("disable agent pane");
    let mut tui = Tui::spawn_with(&profile, 40, 120, |cmd| {
        cmd.env("THURBOX_UI_DIR", interface.path());
    });
    tui.wait_for("hog-idle");
    // The palette proves the declaration is absent, independently of whether
    // the layout happens to place this pane on screen.
    tui.send(CTRL_P);
    tui.wait_for("type to filter commands");
    tui.send(b"focus the agent terminal");
    tui.wait_for("no matching command");
    tui.send(ESC);
    tui.wait_gone("no matching command");

    std::fs::write(
        interface.path().join("layout.lua"),
        "error('broken layout')",
    )
    .expect("break layout");
    tui.send(F10);
    tui.wait_for("broken layout");
    tui.send(b"\x07");
    tui.wait_for("using the bundled interface");
    tui.send(CTRL_P);
    tui.wait_for("type to filter commands");
    tui.send(b"focus the agent terminal");
    tui.wait_for("no matching command");
    tui.send(ESC);
    assert!(tui.quit().success());
}

#[test]
fn memory_fallback_reaps_programs_owned_by_panes_it_removed() {
    if !have_tmux() {
        eprintln!("skipping: tmux is not installed");
        return;
    }
    let interface = interface_plus("91_hog.lua", HOG_PANE);
    let program = r#"return {
  name = "owned-program", slot = "sessions", capabilities = { "program" },
  render = function()
    if not state.started then
      command("program", { text = "owned", repo = "cat" })
      state.started = true
    end
    return { type = "text", text = "owned-program-ready" }
  end,
}"#;
    std::fs::write(interface.path().join("plugins/92_owned.lua"), program).expect("program pane");
    let profile = Profile::new();
    trust(&profile, interface.path(), "plugins/92_owned.lua", program);
    let mut tui = Tui::spawn_with(&profile, 40, 120, |cmd| {
        cmd.env("THURBOX_UI_DIR", interface.path());
    });
    tui.wait_for("hog-idle");
    let program_windows = || {
        let output = profile
            .server
            .tmux(&["list-windows", "-a", "-F", "#{window_name}"]);
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter(|name| name.starts_with("tbp-"))
            .count()
    };
    tui.wait_until("the owned program to start", |_| program_windows() == 1);
    std::fs::write(
        interface.path().join("layout.lua"),
        "error('broken layout')",
    )
    .expect("break layout");
    tui.send(F10);
    tui.wait_for("broken layout");
    assert_eq!(
        program_windows(),
        1,
        "ordinary failed reload must keep the live pane"
    );
    tui.send(b"\x07");
    tui.wait_for("using the bundled interface");
    tui.wait_until("the removed pane's program window to be reaped", |_| {
        program_windows() == 0
    });
    tui.send(CTRL_P);
    tui.wait_for("type to filter commands");
    tui.send(ESC);
    assert!(tui.quit().success());
}

// --- links handed back to the terminal thurbox itself runs in ----------------

impl Tui {
    /// A `Ctrl`-modified press and release at a 0-based cell. SGR adds 16 to
    /// the button number for Control, which is what a terminal sends for the
    /// chord thurbox answers as a link open.
    fn ctrl_press(&mut self, (x, y): (u16, u16)) {
        let (px, py) = (x + 1, y + 1);
        self.send(format!("\x1b[<16;{px};{py}M").as_bytes());
        self.send(format!("\x1b[<16;{px};{py}m").as_bytes());
        // The frame that answers the press is the one that raises the message.
        std::thread::sleep(Duration::from_millis(500));
    }

    /// Poll the bytes written from `since` on until `needle` is among them.
    ///
    /// The frame assertions elsewhere cannot serve here: an OSC 8 wrapper
    /// changes no glyph, so it exists only in the raw stream.
    fn wait_for_raw(&self, since: usize, needle: &str) {
        let deadline = Instant::now() + WAIT;
        while Instant::now() < deadline {
            if self.raw_since(since).contains(needle) {
                return;
            }
            std::thread::sleep(Duration::from_millis(40));
        }
        self.give_up(&format!("{needle:?} to be written to the terminal"));
    }

    /// The message band: the row above the action band.
    fn message_band(&self) -> String {
        let lines: Vec<String> = self.frame().lines().map(str::to_string).collect();
        lines
            .get(lines.len().wrapping_sub(2))
            .cloned()
            .unwrap_or_default()
            .trim()
            .to_string()
    }
}

/// On a host with no browser, both kinds of link are handed to the outer
/// terminal — and the chord thurbox keeps for itself says what it did instead.
///
/// The escape is the only route to a browser for a thurbox reached over ssh:
/// the machine it runs on has none, so the terminal the user is sitting at has
/// to be told the cells are a link. That worked for an agent's OSC 8 runs and
/// not for the bare URLs agents print far more often, which left the common
/// case with nothing for the local terminal to open.
///
/// It has to be asserted out here. `hyperlink_paints` can be handed a link list
/// in process and answer perfectly while the coordinator passes it none — the
/// bytes on the pty are the only place the wiring shows.
#[test]
fn both_kinds_of_link_reach_the_outer_terminal_on_a_host_with_no_browser() {
    let Some((_profile, mut tui)) = shell_session_with(|cmd| {
        // A bare remote: no display and no BROWSER, so `open_url` refuses and
        // the outer terminal is the only leg left.
        cmd.env_remove("DISPLAY");
        cmd.env_remove("WAYLAND_DISPLAY");
        cmd.env_remove("BROWSER");
    }) else {
        return;
    };

    // Both kinds, printed by the "agent". The label and the host go through
    // shell variables so the line the shell ECHOES back does not carry the text
    // the presses below are aimed at — a press landing on the echo would be
    // resolving the command, not its output.
    tui.send(b"L=RICH; H=example.test; printf \"rich \\033]8;;https://$H/rich\\007${L}LINK\\033]8;;\\007 bare https://$H/bare\\n\"\r");
    tui.wait_for("RICHLINK");
    tui.wait_for("https://example.test/bare");

    let mark = tui.raw_len();
    tui.wait_for_raw(mark, "\x1b]8;;https://example.test/bare");

    // 1. Both runs go out wrapped in OSC 8, so the user's own terminal can open
    //    either one.
    let out = tui.raw_since(mark);
    assert!(
        out.contains("\x1b]8;;https://example.test/rich"),
        "the OSC 8 run must be re-printed for the outer terminal"
    );
    assert!(
        out.contains("\x1b]8;;https://example.test/bare"),
        "the bare URL must be re-printed for the outer terminal too"
    );

    // 2. The chord thurbox does answer is never silent: it cannot open a
    //    browser here, so it carries the URL back over OSC 52 and says so.
    for (needle, offset, url) in [
        ("RICHLINK", 0, "https://example.test/rich"),
        ("https://example.test/bare", 4, "https://example.test/bare"),
    ] {
        let (x, y) = tui.find(needle);
        let mark = tui.raw_len();
        tui.ctrl_press((x + offset, y));
        assert_eq!(
            osc52_payload(&tui.raw_since(mark)).as_deref(),
            Some(url),
            "{needle}: the URL must reach the user's clipboard"
        );
        let band = tui.message_band();
        assert!(
            band.contains("No display to open a browser on") && band.contains(url),
            "{needle}: the band must say what happened instead, got {band:?}"
        );
    }

    assert!(tui.quit().success());
}

// --- what a paste looks like to the program it lands in ---------------------

/// A terminal's own paste — Cmd+V, Ctrl+Shift+V — as the outer terminal sends
/// it to thurbox, which turned bracketed paste on for itself.
fn terminal_paste(text: &str) -> Vec<u8> {
    [b"\x1b[200~", text.as_bytes(), b"\x1b[201~"].concat()
}

/// Start `command` in the shell and wait for it to be reading: the echo of the
/// command line, then a quiet stream.
fn run_in_shell(tui: &mut Tui, command: &str, shown: &str) {
    tui.send(format!("{command}\r").as_bytes());
    tui.wait_for(shown);
    tui.wait_until_quiet();
}

#[test]
fn a_paste_into_an_app_without_bracketed_paste_has_no_markers() {
    // The pane never asked for bracketed paste, so the markers are noise to
    // it: `cat -v` showed `^[[200~tb-pasted^[[201~`, as does any `read`
    // prompt, `dash` or REPL without readline. A multiplexer brackets a paste
    // only for a pane that enabled mode 2004 — tmux's `paste-buffer -p`.
    let Some((_profile, mut tui)) = shell_session() else {
        return;
    };
    run_in_shell(&mut tui, "cat -v", "cat -v");
    tui.send(&terminal_paste("tb-pasted\ntb-line2"));
    tui.send(b"\r");
    tui.wait_for("tb-line2");
    tui.wait_until_quiet();
    let frame = tui.frame();
    assert!(
        frame.contains("tb-pasted") && !frame.contains("[200~") && !frame.contains("[201~"),
        "a paste into an app that never enabled bracketed paste carried the markers:\n{frame}"
    );
    assert!(tui.quit().success());
}

#[test]
fn a_paste_into_an_app_with_bracketed_paste_is_one_frame_even_after_a_restart() {
    // The twin: an app that enabled mode 2004 gets exactly one frame. Then the
    // interface restarts under the running app — which turned the mode on
    // before this process ever read its output — and the next paste must
    // still be framed. Whatever decides must know the mode the pane is in,
    // not only what this interface happened to see.
    let Some((profile, mut tui)) = shell_session() else {
        return;
    };
    run_in_shell(&mut tui, "printf '\\033[?2004h'; cat -v", "cat -v");
    tui.send(&terminal_paste("tb-framed"));
    tui.send(b"\r");
    tui.wait_for("^[[200~tb-framed^[[201~");
    tui.wait_until_quiet();
    assert!(
        !tui.frame().contains("^[[200~^[[200~"),
        "the paste was framed twice:\n{}",
        tui.frame()
    );
    assert!(tui.quit().success());

    let mut tui = Tui::spawn(&profile, 40, 120);
    tui.wait_until("the agent pane to be the focused one", |frame| {
        frame
            .lines()
            .last()
            .is_some_and(|band| band.trim_start().starts_with("Agent"))
    });
    tui.wait_for("tb-framed");
    tui.wait_until_quiet();
    tui.send(&terminal_paste("tb-again"));
    tui.send(b"\r");
    tui.wait_for("^[[200~tb-again^[[201~");
    assert!(tui.quit().success());
}

/// A private X server, killed on drop. `None` where there is no `Xvfb`.
struct Xvfb {
    child: Child,
    display: String,
}

/// The first line of `child`'s stdout that `wanted` accepts, read for at most
/// [`WAIT`]; the child is killed when none comes, so a helper that hangs
/// fails the test instead of stalling it.
fn first_line(child: &mut Child, what: &str, wanted: fn(&str) -> bool) -> String {
    let stdout = child.stdout.take().expect("piped stdout");
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let line = std::io::BufRead::lines(std::io::BufReader::new(stdout))
            .map_while(Result::ok)
            .find(|line| wanted(line));
        let _ = tx.send(line);
    });
    match rx.recv_timeout(WAIT) {
        Ok(Some(line)) => line,
        outcome => {
            let _ = child.kill();
            let _ = child.wait();
            panic!("{what}: {outcome:?}");
        }
    }
}

impl Xvfb {
    fn start() -> Option<Self> {
        // `-displayfd 1`: the server picks a free display and writes its
        // number to stdout once it is accepting connections.
        let mut child = Command::new("Xvfb")
            .args([
                "-displayfd",
                "1",
                "-nolisten",
                "tcp",
                "-screen",
                "0",
                "640x480x24",
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .ok()?;
        let number = first_line(&mut child, "Xvfb never named its display", |line| {
            !line.trim().is_empty()
        });
        Some(Self {
            child,
            display: format!(":{}", number.trim()),
        })
    }
}

impl Drop for Xvfb {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// The environment variable [`clipboard_owner`] reads its text from.
const SEED_CLIPBOARD: &str = "TBX_E2E_SEED_CLIPBOARD";

/// Not a test: the process that owns the X clipboard for
/// [`a_pasted_end_marker_cannot_submit_a_line`]. An X selection lives only as
/// long as a client serves it, and the native read thurbox makes is the one
/// route a terminal does not sanitise first, so this holds it from a process
/// of its own — this test binary, re-run on this one test — until it is
/// killed. Without the variable it does nothing, so `--ignored` runs are safe.
#[test]
#[ignore = "a helper process, started by a_pasted_end_marker_cannot_submit_a_line"]
fn clipboard_owner() {
    let Ok(text) = std::env::var(SEED_CLIPBOARD) else {
        return;
    };
    let mut clipboard = arboard::Clipboard::new().expect("connect to the X server");
    clipboard.set_text(text).expect("own the clipboard");
    println!("tbx-clipboard-owned");
    loop {
        std::thread::sleep(Duration::from_secs(60));
    }
}

/// [`clipboard_owner`], running on `display` with `text`, killed on drop.
struct ClipboardOwner(Child);

impl ClipboardOwner {
    fn hold(display: &str, text: &str) -> Self {
        let mut child = Command::new(std::env::current_exe().expect("test binary"))
            .args(["clipboard_owner", "--exact", "--ignored", "--nocapture"])
            .env("DISPLAY", display)
            .env_remove("WAYLAND_DISPLAY")
            .env(SEED_CLIPBOARD, text)
            .stdout(Stdio::piped())
            .spawn()
            .expect("start the clipboard owner");
        first_line(
            &mut child,
            "the clipboard owner never took the clipboard",
            |line| line.contains("tbx-clipboard-owned"),
        );
        Self(child)
    }
}

impl Drop for ClipboardOwner {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn a_pasted_end_marker_cannot_submit_a_line() {
    // Security, not formatting. A native clipboard holding `ESC[201~` ended
    // the bracketed paste early, and the CR after it reached bash as Enter:
    // the probe ran `echo tb-pasted-onlyecho tb-INJECTED-ran`. Ctrl+V reads
    // the clipboard itself, so no terminal stands in between to defang it.
    // The `""` keeps the command's own echo from matching what running it
    // would print.
    let Some(x) = Xvfb::start() else {
        eprintln!("skipping: Xvfb is not installed");
        return;
    };
    let _owner = ClipboardOwner::hold(
        &x.display,
        "echo tb-pasted-only é漢\x1b[201~echo tb-INJ\"\"ECTED-ran\r",
    );
    let display = x.display.clone();
    let Some((_profile, mut tui)) = shell_session_with(|cmd| {
        cmd.env("DISPLAY", display);
        cmd.env_remove("WAYLAND_DISPLAY");
    }) else {
        return;
    };
    // bash enables bracketed paste at its prompt (5.1 and later), which is
    // what makes an early end marker the difference between text and Enter.
    run_in_shell(
        &mut tui,
        "exec bash --norc --noprofile -i",
        "exec bash --norc --noprofile -i",
    );
    tui.send(&[0x16]);
    tui.wait_for("tb-pasted-only é漢");
    tui.wait_until_quiet();
    let frame = tui.frame();
    assert!(
        !frame.contains("tb-INJECTED-ran"),
        "a pasted end marker let the line after it run:\n{frame}"
    );
    assert!(tui.quit().success());
}

// --- a WSL session whose launcher shares the interface's terminal ----------

/// Lua that throws on every other quarter second of the pane's own clock: the
/// intermittent failure a pane whose data is half there produces, tick after
/// tick.
const FLAKY: &str =
    "if math.floor((ctx.elapsed or 0) * 4) % 2 == 0 then error('flaky failure') end";

/// Put a `flaky` pane in a column beside the centre: `guard` is the Lua that
/// decides whether this render throws, and `flaky pane ok` is what it draws
/// when it does not.
fn flaky_column(ui: &Path, guard: &str) {
    std::fs::write(
        ui.join("plugins/31_flaky.lua"),
        format!(
            "return {{ name = 'flaky', slot = 'flaky', pure = false, render = function(ctx)\n\
             {guard}\n\
             return {{ type = 'box', frame = {{ title = 'flaky' }}, children = {{\n\
               {{ type = 'text', len = 1, text = 'flaky pane ok' }} }} }}\n\
             end }}\n"
        ),
    )
    .expect("flaky pane");
    let layout = ui.join("layout.lua");
    let arrangement = std::fs::read_to_string(&layout).expect("layout");
    let center = "    columns[#columns + 1] = { slot = \"center\" }\n";
    assert!(arrangement.contains(center), "the bundled layout moved");
    std::fs::write(
        &layout,
        arrangement.replacen(
            center,
            &format!("{center}    columns[#columns + 1] = {{ slot = \"flaky\", pct = 30 }}\n"),
            1,
        ),
    )
    .expect("place the flaky pane");
}

/// A pane that fails intermittently must settle into one state rather than
/// alternate with every tick — neither a float covering most of the screen
/// on the frames it renders and gone on the frames it throws, nor a side pane
/// swapping its content for an error panel and back.
#[test]
fn an_intermittently_failing_pane_does_not_flicker() {
    let profile = Profile::new();
    let ui = profile.path("config/ui");
    let report = thurbox::kernel::bundled::materialize(&ui);
    assert!(report.errors.is_empty(), "{:?}", report.errors);
    flaky_column(&ui, FLAKY);
    std::fs::write(
        ui.join("plugins/33_flakyfloat.lua"),
        format!(
            "return {{ name = 'flakyfloat', slot = 'float', floats = true, pure = false,\n\
             render = function(ctx)\n\
             {FLAKY}\n\
             return {{ type = 'box', float = {{ width = 60, height = 60 }},\n\
               frame = {{ title = 'float' }}, children = {{\n\
               {{ type = 'text', len = 1, text = 'flaky float ok' }} }} }}\n\
             end }}\n"
        ),
    )
    .expect("flaky float");
    let mut tui = Tui::spawn(&profile, 40, 140);
    // A float has no rect to fail in, so its failure is said in the band.
    tui.wait_for("flakyfloat failed");
    tui.wait_for("flaky failure");
    // Long enough for several failing and succeeding quarter seconds each.
    let frames: Vec<String> = (0..40)
        .map(|_| {
            std::thread::sleep(Duration::from_millis(50));
            tui.frame()
        })
        .collect();
    let float_shown = frames
        .iter()
        .filter(|f| f.contains("flaky float ok"))
        .count();
    let pane_shown = frames
        .iter()
        .filter(|f| f.contains("flaky pane ok"))
        .count();
    let summary = || format!("first frame:\n{}", frames[0]);
    assert_eq!(
        float_shown,
        0,
        "a failing float must not keep taking the screen ({float_shown}/40 frames); {}",
        summary()
    );
    assert_eq!(
        pane_shown,
        0,
        "a failing pane must hold its error, not alternate with its content \
         ({pane_shown}/40 frames); {}",
        summary()
    );
    assert!(
        frames.iter().all(|f| f.contains("No sessions yet")),
        "the session list must stay visible on every frame; {}",
        summary()
    );
    assert!(tui.quit().success());
}

/// The other half of holding a failure: a pane that stops throwing comes back
/// by itself once the hold runs out, with no key pressed to repaint it.
#[test]
fn a_failing_pane_that_recovers_comes_back_after_the_hold() {
    let profile = Profile::new();
    let ui = profile.path("config/ui");
    let report = thurbox::kernel::bundled::materialize(&ui);
    assert!(report.errors.is_empty(), "{:?}", report.errors);
    // Timed from the pane's own first render: `ctx.elapsed` counts from
    // process start, which a cold start can put past the window already.
    flaky_column(
        &ui,
        "state.first = state.first or ctx.elapsed\n\
         if ctx.elapsed - state.first < 1 then error('flaky failure') end",
    );

    let mut tui = Tui::spawn(&profile, 40, 140);
    tui.wait_for("flaky failure");
    let failed_at = Instant::now();
    tui.wait_for("flaky pane ok");
    // It stopped throwing within a second of starting, and is held five
    // seconds past its last throw: well past three since it was first seen.
    let held = failed_at.elapsed();
    assert!(
        held >= Duration::from_secs(3),
        "a recovered pane must be held, not shown on its first clean render \
         ({held:?} after the failure was seen)"
    );
    assert!(!tui.frame().contains("flaky failure"));
    assert!(tui.quit().success());
}

/// The perf HUD's `failures` counts renders that threw. A pane held in its
/// failed state after it stopped throwing renders cleanly on every one of
/// those frames, and they must not be counted as failures.
#[test]
fn a_held_failure_is_not_counted_as_a_failure_on_every_frame() {
    let profile = Profile::new();
    let ui = profile.path("config/ui");
    let report = thurbox::kernel::bundled::materialize(&ui);
    assert!(report.errors.is_empty(), "{:?}", report.errors);
    flaky_column(
        &ui,
        "state.first = state.first or ctx.elapsed\n\
         if ctx.elapsed - state.first < 1 then error('flaky failure') end",
    );

    let mut tui = Tui::spawn(&profile, 40, 140);
    tui.wait_for("flaky failure");
    let failed_at = Instant::now();
    // The HUD keeps the loop painting, so a held pane renders every frame.
    tui.send(F12);
    let failures = |frame: &str| -> u64 {
        frame
            .lines()
            .find_map(|line| line.split("failures").nth(1))
            .and_then(|rest| rest.split_whitespace().next())
            .and_then(|n| n.parse().ok())
            .unwrap_or_else(|| panic!("no failures counter in the HUD:\n{frame}"))
    };
    tui.wait_for("failures");
    // Two readings, both after the pane stopped throwing (one second in) and
    // both well inside the five-second hold.
    std::thread::sleep(Duration::from_millis(2500).saturating_sub(failed_at.elapsed()));
    let before = failures(&tui.frame());
    std::thread::sleep(Duration::from_millis(1500));
    let after = failures(&tui.frame());
    assert!(
        failed_at.elapsed() < Duration::from_secs(5),
        "the readings must fall inside the hold"
    );
    assert_eq!(
        before, after,
        "a pane that stopped throwing must stop adding failures while it is held"
    );
    assert!(tui.quit().success());
}

// --- key releases -----------------------------------------------------------

/// What the `hold` probe pane has been handed: its event log, how often the
/// press-only chord fired, and what the kernel says about key releases.
fn hold_state(profile: &Profile) -> (String, u64, String) {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_thurbox-cli"));
    profile.apply(&mut cmd);
    let output = cmd.args(["--json", "ui", "state"]).output().expect("state");
    assert!(output.status.success(), "ui state failed");
    let state: serde_json::Value = serde_json::from_slice(&output.stdout).expect("JSON");
    let pane = &state["plugin_state"]["plugins/91_hold.lua"];
    (
        pane["events"].as_str().unwrap_or_default().to_string(),
        pane["plain"].as_u64().unwrap_or(0),
        pane["releases"].as_str().unwrap_or_default().to_string(),
    )
}

fn wait_hold(profile: &Profile, tui: &Tui, events: &str, releases: &str) {
    let deadline = Instant::now() + WAIT;
    let mut last = (String::new(), 0, String::new());
    while Instant::now() < deadline {
        last = hold_state(profile);
        if last.0 == events && last.2 == releases {
            return;
        }
        std::thread::sleep(Duration::from_millis(60));
    }
    tui.give_up(&format!(
        "hold events {events:?} with releases {releases:?}; last was {last:?}"
    ));
}

fn hold_interface() -> tempfile::TempDir {
    interface_plus(
        "91_hold.lua",
        include_str!("fixtures/key_release/91_hold.lua"),
    )
}

/// The kitty encodings of `ctrl+space`: press, repeat, release, and a release
/// arriving after `ctrl` was already let go.
const CTRL_SPACE_PRESS: &[u8] = b"\x1b[32;5u";
const CTRL_SPACE_REPEAT: &[u8] = b"\x1b[32;5:2u";
const CTRL_SPACE_RELEASE: &[u8] = b"\x1b[32;5:3u";
const SPACE_RELEASE_CTRL_UP: &[u8] = b"\x1b[32;1:3u";

#[test]
fn a_release_binding_gets_one_press_no_repeats_and_its_release() {
    let interface = hold_interface();
    let profile = Profile::new();
    let mut tui = Tui::spawn_kitty(&profile, 40, 120, |cmd| {
        cmd.env("THURBOX_UI_DIR", interface.path());
    });
    tui.wait_for("No sessions yet");
    wait_hold(&profile, &tui, "", "negotiated");

    tui.send(CTRL_SPACE_PRESS);
    for _ in 0..3 {
        tui.send(CTRL_SPACE_REPEAT);
    }
    wait_hold(&profile, &tui, "press", "negotiated");
    tui.send(CTRL_SPACE_RELEASE);
    wait_hold(&profile, &tui, "press,release", "reported");

    // The modifier is often let go first; the release still belongs to the
    // chord that was pressed.
    tui.send(CTRL_SPACE_PRESS);
    tui.send(SPACE_RELEASE_CTRL_UP);
    wait_hold(&profile, &tui, "press,release,press,release", "reported");

    // A release nobody is holding goes nowhere.
    tui.send(CTRL_SPACE_RELEASE);

    // A press-only chord is untouched: no argument, and a repeat fires again
    // exactly as an auto-repeated press always did — but its release does not.
    tui.send(b"\x1b[18~");
    tui.send(b"\x1b[18;1:2~");
    tui.send(b"\x1b[18;1:3~");
    let deadline = Instant::now() + WAIT;
    while hold_state(&profile).1 < 2 && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(60));
    }
    std::thread::sleep(Duration::from_millis(300));
    let (events, plain, _) = hold_state(&profile);
    assert_eq!(
        plain, 2,
        "a press and its repeat, and nothing for the release"
    );
    assert_eq!(events, "press,release,press,release");
    assert!(tui.quit().success());
    tui.assert_keyboard_flags_popped();
}

/// kitty sends a text key's press and repeat as plain text even with event
/// types reported, so once `ctrl` is let go a held `ctrl+space` repeats as a
/// bare space. Until its release that is still the hold, and types nothing.
#[test]
fn a_held_chord_repeating_as_text_after_its_modifier_lifts_types_nothing() {
    let interface = hold_interface();
    let profile = Profile::new();
    let mut tui = Tui::spawn_kitty(&profile, 40, 120, |cmd| {
        cmd.env("THURBOX_UI_DIR", interface.path());
    });
    tui.wait_for("No sessions yet");
    tui.send(CTRL_SPACE_PRESS);
    tui.send(CTRL_SPACE_RELEASE);
    wait_hold(&profile, &tui, "press,release", "reported");

    tui.send(CTRL_SPACE_PRESS);
    wait_hold(&profile, &tui, "press,release,press", "reported");
    for _ in 0..3 {
        tui.send(b" ");
    }
    tui.send(SPACE_RELEASE_CTRL_UP);
    wait_hold(&profile, &tui, "press,release,press,release", "reported");
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(spaces(&profile), 0, "the hold's repeats reached a binding");

    // Once let go, a space is a space again.
    tui.send(b" ");
    wait_spaces(&profile, &tui, 1);
    assert!(tui.quit().success());
}

/// A terminal that accepted the request but sends no releases never ends a
/// hold, so there a held chord's key must keep typing.
#[test]
fn a_terminal_that_sends_no_releases_keeps_a_held_chords_key_typing() {
    let interface = hold_interface();
    let profile = Profile::new();
    let mut tui = Tui::spawn_kitty(&profile, 40, 120, |cmd| {
        cmd.env("THURBOX_UI_DIR", interface.path());
    });
    tui.wait_for("No sessions yet");
    tui.send(CTRL_SPACE_PRESS);
    wait_hold(&profile, &tui, "press", "negotiated");
    tui.send(b" ");
    wait_spaces(&profile, &tui, 1);
    assert!(tui.quit().success());
}

fn wait_spaces(profile: &Profile, tui: &Tui, want: u64) {
    let deadline = Instant::now() + WAIT;
    while spaces(profile) < want && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(60));
    }
    let got = spaces(profile);
    if got != want {
        tui.give_up(&format!("{want} spaces reached the binding, saw {got}"));
    }
}

/// How many plain spaces reached the `hold` probe pane's `space` binding.
fn spaces(profile: &Profile) -> u64 {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_thurbox-cli"));
    profile.apply(&mut cmd);
    let output = cmd.args(["--json", "ui", "state"]).output().expect("state");
    let state: serde_json::Value = serde_json::from_slice(&output.stdout).expect("JSON");
    state["plugin_state"]["plugins/91_hold.lua"]["spaces"]
        .as_u64()
        .unwrap_or(0)
}

#[test]
fn a_release_never_types_into_a_field_and_a_modal_keeps_its_press() {
    let interface = hold_interface();
    let profile = Profile::new();
    let mut tui = Tui::spawn_kitty(&profile, 40, 120, |cmd| {
        cmd.env("THURBOX_UI_DIR", interface.path());
    });
    tui.wait_for("No sessions yet");

    // Pressed while help is open: the modal takes the press, so the release
    // that follows has no holder and reaches nobody.
    tui.send(F1);
    tui.wait_for("Keybindings");
    tui.send(CTRL_SPACE_PRESS);
    tui.send(ESC);
    tui.wait_gone("Keybindings");
    tui.send(CTRL_SPACE_RELEASE);

    // Typing with releases reported: each key's release must not type it a
    // second time.
    tui.send(b"\x1b[47;5u");
    tui.wait_for("Search");
    for &key in b"qz" {
        tui.send(&[key]);
        tui.send(format!("\x1b[{key};1:3u").as_bytes());
    }
    tui.wait_for("Search qz");
    std::thread::sleep(Duration::from_millis(300));
    assert!(tui.frame().contains("Search qz "), "{}", tui.frame());
    let (events, _, releases) = hold_state(&profile);
    assert_eq!(events, "", "the modal's press is not the pane's to release");
    assert_eq!(releases, "reported");
    assert!(tui.quit().success());
    tui.assert_keyboard_flags_popped();
}

#[test]
fn terminal_query_replies_survive_fragmented_output() {
    for kitty in [false, true] {
        let mut terminal = TestTerminal {
            kitty,
            ..TestTerminal::default()
        };
        let mut output = b"plain text\x1b".to_vec();
        assert!(terminal.feed(&output).is_empty());
        output.extend_from_slice(b"[?u\x1b");
        let expected: &[u8] = if kitty { b"\x1b[?0u" } else { b"" };
        assert_eq!(terminal.feed(&output), expected);
        output.extend_from_slice(b"[c");
        assert_eq!(terminal.feed(&output), b"\x1b[?62;22c");
        assert!(
            terminal.feed(&output).is_empty(),
            "each query is answered once"
        );
    }
}

#[test]
fn a_silent_terminal_keeps_press_only_delivery_after_negotiation_times_out() {
    let interface = hold_interface();
    let profile = Profile::new();
    let mut tui = Tui::spawn_on(
        &profile,
        40,
        120,
        |cmd| {
            cmd.env("THURBOX_UI_DIR", interface.path());
        },
        TerminalMode::Silent,
    );
    tui.wait_for("No sessions yet");
    wait_hold(&profile, &tui, "", "unsupported");
    assert!(tui.raw_since(0).contains("\x1b[?u\x1b[c"));
    assert!(tui.replies.lock().unwrap().is_empty());
    tui.send(b"\x00");
    wait_hold(&profile, &tui, "press", "unsupported");
    tui.send(b"\x00");
    wait_hold(&profile, &tui, "press,press", "unsupported");
    assert!(tui.quit().success());
}

#[test]
fn a_legacy_terminal_reports_no_releases_and_keeps_press_only_delivery() {
    let interface = hold_interface();
    let profile = Profile::new();
    let mut tui = Tui::spawn_with(&profile, 40, 120, |cmd| {
        cmd.env("THURBOX_UI_DIR", interface.path());
    });
    tui.wait_for("No sessions yet");
    wait_hold(&profile, &tui, "", "unsupported");
    // A legacy terminal rejects kitty support by answering the following DA
    // query, rather than leaving capability detection to its timeout.
    assert!(tui.raw_since(0).contains("\x1b[?u\x1b[c"));
    assert_eq!(
        *tui.replies.lock().unwrap(),
        b"\x1b[?62;22c",
        "a legacy terminal must answer DA without advertising kitty flags"
    );
    // `ctrl+space` in a legacy terminal is NUL. Every press is delivered, as
    // a press, because nothing can say the key was ever let go.
    tui.send(b"\x00");
    wait_hold(&profile, &tui, "press", "unsupported");
    tui.send(b"\x00");
    wait_hold(&profile, &tui, "press,press", "unsupported");
    assert!(tui.quit().success());
}

/// The loop's `frames` counter from the first perf snapshot captured after
/// `after` (its `captured_at`, whole seconds since the epoch), with that
/// snapshot's own `captured_at`.
fn frames_after(profile: &Profile, tui: &Tui, after: u64) -> (u64, u64) {
    let deadline = Instant::now() + WAIT;
    while Instant::now() < deadline {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_thurbox-cli"));
        profile.apply(&mut cmd);
        let out = cmd.args(["perf", "--json"]).output().expect("perf");
        if let Ok(json) = serde_json::from_slice::<serde_json::Value>(&out.stdout) {
            let at = json["captured_at"].as_u64().unwrap_or(0);
            if at > after {
                return (json["counters"]["frames"].as_u64().unwrap_or(0), at);
            }
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    tui.give_up("a fresh perf snapshot");
}

fn epoch_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_secs()
}

#[test]
fn a_release_nobody_holds_draws_no_frame() {
    let interface = hold_interface();
    let profile = Profile::new();
    let mut tui = Tui::spawn_kitty(&profile, 40, 120, |cmd| {
        cmd.env("THURBOX_UI_DIR", interface.path());
        cmd.env("THURBOX_PERF_LOG", "1");
    });
    tui.wait_for("No sessions yet");
    // Snapshots are published every few seconds. The idle rate comes from two
    // consecutive ones after the first (whose window holds the boot's frames),
    // and the window the releases are counted in starts at the second and ends
    // at the first snapshot captured after the last release was written — so
    // it holds every send, whatever the cadence.
    let (_, booted_at) = frames_after(&profile, &tui, 0);
    let (first, first_at) = frames_after(&profile, &tui, booted_at);
    let (start, start_at) = frames_after(&profile, &tui, first_at);
    let idle_per_sec = (start - first) as f64 / (start_at - first_at).max(1) as f64;
    // Releases of a key nothing pressed, each in its own read so none share
    // a batch.
    const RELEASES: u64 = 200;
    for _ in 0..RELEASES {
        tui.send(b"\x1b[120;1:3u");
        std::thread::sleep(Duration::from_millis(5));
    }
    let sent_by = epoch_secs();
    let (end, end_at) = frames_after(&profile, &tui, sent_by);
    let idle = idle_per_sec * (end_at - start_at) as f64;
    let drawn = (end - start) as f64 - idle;
    assert!(
        drawn < (RELEASES / 10) as f64,
        "{drawn:.0} frames beyond the idle rate ({idle_per_sec:.1}/s) drawn for {RELEASES} \
         releases nobody holds"
    );
    assert!(tui.quit().success());
    tui.assert_keyboard_flags_popped();
}

/// Write an executable script.
fn write_script(path: &Path, body: &str) {
    std::fs::create_dir_all(path.parent().expect("a parent")).expect("mkdir");
    std::fs::write(path, body).expect("write script");
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
}

// --- the session features thurbox keeps on Fleet -----------------------------

/// Leave a focused terminal for the session list: 0x08 is Ctrl+H, the
/// reserved way out of a terminal and onto the list beside it.
fn focus_the_list(tui: &mut Tui) {
    tui.send(b"\x08");
    tui.wait_until("the session list to be the focused one", |frame| {
        frame
            .lines()
            .last()
            .is_some_and(|band| band.trim_start().starts_with("Sessions"))
    });
}

#[test]
fn ctrl_e_renames_the_selected_session_and_says_why_a_name_is_refused() {
    // `Ctrl+E` opens a field holding the current name. A refused name is
    // explained in the field; a good one lands in the list and in the Task's
    // name label, where Fleet keeps a session's display name.
    let Some((profile, mut tui)) = shell_session() else {
        return;
    };
    focus_the_list(&mut tui);

    // 0x05 is Ctrl+E; 0x15 is Ctrl+U, which clears the prefilled name.
    tui.send(b"\x05");
    tui.wait_for("Rename session");
    tui.send(b"\x15");
    tui.send(b"bad/name\r");
    tui.wait_for("Name contains invalid characters");

    tui.send(b"\x15");
    tui.send(b"renamed\r");
    tui.wait_gone("Rename session");
    tui.wait_until("the new name in the list", |_| {
        tui.session_row("renamed").is_some() && tui.session_row("probe").is_none()
    });
    let task = profile.fleet_task("probe");
    assert_eq!(
        task["metadata"]["labels"]["thurbox.dev/name"], "renamed",
        "the name lives on the Task: {task}"
    );
    assert!(tui.quit().success());
}

/// The rows `first` and `second` are painted on, in that order.
fn rows_of(tui: &Tui, first: &str, second: &str) -> Option<(u16, u16)> {
    Some((tui.session_row(first)?.1, tui.session_row(second)?.1))
}

#[test]
fn a_session_moved_up_stays_there_across_a_restart() {
    // `K` moves the selected session above its neighbour. The order is the
    // interface's own (Fleet has no order of Tasks), so it is kept in a file
    // of thurbox's and a restarted interface paints it the same way.
    let Some((profile, mut tui)) = shell_session_prepared(
        |profile| profile.fleet_session("second", &profile.path("repo")),
        |_| {},
    ) else {
        return;
    };
    tui.wait_until("both sessions, in name order", |_| {
        rows_of(&tui, "probe", "second").is_some_and(|(a, b)| a < b)
    });
    focus_the_list(&mut tui);
    tui.send(b"j");
    tui.send(b"K");
    tui.wait_until("the moved session above the other", |_| {
        rows_of(&tui, "probe", "second").is_some_and(|(a, b)| a > b)
    });
    assert!(tui.quit().success());

    let tui = Tui::spawn(&profile, 40, 120);
    tui.wait_until("the order kept across the restart", |_| {
        rows_of(&tui, "probe", "second").is_some_and(|(a, b)| a > b)
    });
}

/// The companion shell is the user's `$SHELL`, and a zsh started in the
/// profile's empty HOME opens its first-run wizard instead of a prompt.
fn plain_shell(cmd: &mut Command) {
    cmd.env("SHELL", "/bin/sh");
}

/// Raise the Shell tab (Ctrl+T) and prove a live shell is behind it:
/// `command` runs there and `marker` is what it prints.
fn in_the_shell(tui: &mut Tui, command: &str, marker: &str) {
    tui.send(b"\x14");
    wait_for_view(tui, "Shell");
    tui.wait_for("$ ");
    tui.wait_until_quiet();
    tui.send(format!("{command}\r").as_bytes());
    tui.wait_for(marker);
}

/// The id of each companion shell window on `profile`'s server.
fn shell_windows(profile: &Profile) -> Vec<String> {
    let windows = profile
        .server
        .tmux(&["list-windows", "-a", "-F", "#{window_id} #{window_name}"]);
    String::from_utf8_lossy(&windows.stdout)
        .lines()
        .filter(|line| line.contains(" tbs-"))
        .filter_map(|line| line.split_whitespace().next().map(str::to_string))
        .collect()
}

/// A restarted interface on `profile`, its agent pane attached.
fn restart(profile: &Profile) -> Tui {
    let tui = Tui::spawn_with(profile, 40, 120, plain_shell);
    tui.wait_until("the agent pane to be the focused one", |frame| {
        frame
            .lines()
            .last()
            .is_some_and(|band| band.trim_start().starts_with("Agent"))
    });
    tui.wait_for("$ ");
    tui
}

#[test]
fn the_companion_shell_is_readopted_after_a_restart() {
    // The shell outlives the interface in its own window. A restarted
    // interface finds it again from the file it recorded it in: the variable
    // the first interface's shell set is still set, and no second shell
    // window is made.
    let Some((profile, mut tui)) = shell_session_with(plain_shell) else {
        return;
    };
    in_the_shell(&mut tui, "TB_KEPT=kept; echo set-\"\"$TB_KEPT", "set-kept");
    assert!(tui.quit().success());

    let mut tui = restart(&profile);
    in_the_shell(&mut tui, "echo again-\"\"$TB_KEPT", "again-kept");
    assert_eq!(shell_windows(&profile).len(), 1, "one shell, re-adopted");
    assert!(tui.quit().success());
}

#[test]
fn a_shell_whose_window_went_while_thurbox_was_closed_is_replaced() {
    // A window that went while the interface was closed (a tmux server
    // restart, a kill from outside) must not be adopted as a shell that never
    // prints: raising the tab gives a working one.
    let Some((profile, mut tui)) = shell_session_with(plain_shell) else {
        return;
    };
    in_the_shell(&mut tui, "echo tb-first-\"\"shell", "tb-first-shell");
    assert!(tui.quit().success());
    let shells = shell_windows(&profile);
    assert_eq!(shells.len(), 1, "one shell window to close");
    profile.server.tmux(&["kill-window", "-t", &shells[0]]);

    let mut tui = restart(&profile);
    in_the_shell(&mut tui, "echo tb-second-\"\"shell", "tb-second-shell");
    assert!(tui.quit().success());
}
