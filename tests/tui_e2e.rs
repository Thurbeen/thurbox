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

use thurbox::backend::tmux_compat::server::TmuxCompatible;

/// The guard every tmux server in this file is reaped by — see its own doc.
#[path = "support/tmux_server.rs"]
mod tmux_server;

use tmux_server::TmuxServer;

/// How long a frame is given to show something before the test gives up.
/// Generous because a cold CI runner pays for the first paint with the Lua
/// interface load and the SQLite open.
const WAIT: Duration = Duration::from_secs(20);

/// The bytes a terminal sends for the chords the scenarios press.
const CTRL_P: &[u8] = b"\x10";
const CTRL_Q: &[u8] = b"\x11";
const CTRL_Y: &[u8] = b"\x19";
/// Readline's end-of-line, and so one of the chords the interface defers to a
/// focused agent (`passthrough` in `ui/plugins/10_sessions.lua`).
const CTRL_E: &[u8] = b"\x05";
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

fn have_rmux() -> bool {
    Command::new("rmux").arg("-V").output().is_ok_and(|output| {
        output.status.success()
            && thurbox::backend::rmux::Rmux::check_banner(
                &String::from_utf8_lossy(&output.stdout),
                "test",
            )
            .is_ok()
    })
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
        Self {
            root,
            bin,
            server: TmuxServer::private(&private_socket()),
        }
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

    /// Run `thurbox-cli` in this profile; it must succeed.
    fn cli(&self, args: &[&str]) {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_thurbox-cli"));
        self.apply(&mut cmd);
        let output = cmd.args(args).output().expect("run thurbox-cli");
        assert!(
            output.status.success(),
            "thurbox-cli {args:?} failed:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
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

/// The binary, running on a pty, with everything it has written so far.
struct Tui {
    child: Child,
    master: OwnedFd,
    /// Every byte the binary wrote, verbatim — the escape-sequence record.
    raw: Arc<Mutex<Vec<u8>>>,
    /// The same bytes through vt100, for asserting on the visible frame.
    screen: Arc<Mutex<vt100::Parser>>,
    /// The exit status, once seen: `try_wait` reaps, so it is read once.
    exited: Option<ExitStatus>,
    /// The binary's own log, quoted when a wait times out.
    log: PathBuf,
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
        let screen = Arc::new(Mutex::new(vt100::Parser::new(rows, cols, 0)));
        let mut reader = std::fs::File::from(master.try_clone().expect("dup master"));
        {
            let raw = Arc::clone(&raw);
            let screen = Arc::clone(&screen);
            // Reads until EIO, which is how a pty reports the child gone.
            std::thread::spawn(move || {
                let mut buf = [0u8; 4096];
                while let Ok(n) = reader.read(&mut buf) {
                    if n == 0 {
                        break;
                    }
                    raw.lock().unwrap().extend_from_slice(&buf[..n]);
                    screen.lock().unwrap().process(&buf[..n]);
                }
            });
        }
        Self {
            child,
            master,
            raw,
            screen,
            exited: None,
            log: profile.path("data/thurbox.log"),
        }
    }

    /// The frame as vt100 reconstructs it, rows trimmed of trailing blanks.
    fn frame(&self) -> String {
        self.screen.lock().unwrap().screen().contents()
    }

    /// One row of the frame, untrimmed, so a column position means something.
    fn row(&self, y: u16) -> String {
        let screen = self.screen.lock().unwrap();
        let screen = screen.screen();
        (0..screen.size().1)
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
            .set_size(rows, cols);
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
        // The appender rolls daily, so the file carries a date suffix.
        let dir = self.log.parent().expect("log dir");
        let stem = self
            .log
            .file_name()
            .expect("log name")
            .to_string_lossy()
            .into_owned();
        let text = std::fs::read_dir(dir)
            .ok()
            .into_iter()
            .flatten()
            .flatten()
            .filter(|entry| entry.file_name().to_string_lossy().starts_with(&stem))
            .filter_map(|entry| std::fs::read_to_string(entry.path()).ok())
            .collect::<String>();
        let lines: Vec<&str> = text.lines().collect();
        lines[lines.len().saturating_sub(30)..].join("\n")
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
    let mut saw_control_notice = false;
    let mut saw_other_status = false;
    for _ in 0..16 {
        tui.send(b"\x1a");
        std::thread::sleep(Duration::from_millis(500));
        let frame = tui.frame();
        saw_control_notice |= frame.contains("local UI control unavailable");
        saw_other_status |= frame.contains("nothing to undo");
    }
    assert!(saw_other_status, "status traffic did not reach the TUI");
    assert!(
        saw_control_notice,
        "other status messages hid the control failure"
    );
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
fn session_focus_refuses_a_tui_without_an_agent_pane() {
    let Some((profile, mut first)) = shell_session() else {
        return;
    };
    let agent = profile.path("config/ui/plugins/20_agent.lua");
    std::fs::write(
        profile.path("config/ui.json"),
        serde_json::json!({"disabled": [agent]}).to_string(),
    )
    .expect("disable agent pane");
    let mut tui = Tui::spawn(&profile, 40, 120);
    tui.wait_for("probe");
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_thurbox-cli"));
    profile.apply(&mut cmd);
    let sessions = cmd
        .args(["--json", "session", "list"])
        .output()
        .expect("sessions");
    let rows: serde_json::Value = serde_json::from_slice(&sessions.stdout).expect("JSON");
    let session = rows[0]["id"].as_str().expect("session id");
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_thurbox-cli"));
    profile.apply(&mut cmd);
    let instances = cmd
        .args(["--json", "ui", "instances"])
        .output()
        .expect("instances");
    let listed: serde_json::Value = serde_json::from_slice(&instances.stdout).expect("JSON");
    let target = listed["instances"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["pid"] == tui.child.id())
        .and_then(|entry| entry["id"].as_str())
        .expect("target instance");
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_thurbox-cli"));
    profile.apply(&mut cmd);
    let output = cmd
        .args([
            "--json",
            "ui",
            "--instance",
            target,
            "action",
            "session.focus",
            "--session",
            session,
        ])
        .output()
        .expect("focus request");
    assert!(
        !output.status.success(),
        "focus must refuse a missing agent pane"
    );
    assert!(tui.quit().success());
    assert!(first.quit().success());
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
fn local_ui_control_targets_one_of_two_live_instances() {
    let Some((profile, mut first)) = shell_session() else {
        return;
    };
    let mut second = Tui::spawn(&profile, 40, 120);
    second.wait_for("probe");
    let cli = |args: &[&str]| {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_thurbox-cli"));
        profile.apply(&mut cmd);
        let output = cmd.args(["--json"]).args(args).output().expect("run cli");
        (
            output.status,
            serde_json::from_slice::<serde_json::Value>(&output.stdout).expect("JSON"),
        )
    };
    let deadline = Instant::now() + WAIT;
    let ids = loop {
        let (status, value) = cli(&["ui", "instances"]);
        if status.success() {
            let entries = value["instances"].as_array().expect("instances array");
            if entries.len() == 2 {
                let id_for = |pid| {
                    entries
                        .iter()
                        .find(|row| row["pid"] == pid)
                        .and_then(|row| row["id"].as_str())
                        .expect("instance PID")
                        .to_owned()
                };
                break vec![id_for(first.child.id()), id_for(second.child.id())];
            }
        }
        assert!(
            Instant::now() < deadline,
            "two live instances were not discovered"
        );
        std::thread::sleep(Duration::from_millis(40));
    };
    let (status, ambiguous) = cli(&["ui", "state"]);
    assert!(!status.success());
    assert!(ambiguous["error"].to_string().contains("ambiguous"));
    let directory = profile.path("data/ui-control");
    assert_eq!(
        std::fs::metadata(&directory).unwrap().permissions().mode() & 0o777,
        0o700
    );
    for id in &ids {
        let socket = directory.join(format!("{id}.sock"));
        assert_eq!(
            std::fs::metadata(socket).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
    let (status, absent) = cli(&[
        "ui",
        "--instance",
        &ids[1],
        "action",
        "session.focus",
        "--session",
        "00000000-0000-0000-0000-000000000000",
    ]);
    assert!(!status.success());
    assert!(absent["result"]["error"]["message"]
        .to_string()
        .contains("not in this interface"));
    let (status, invalid) = cli(&[
        "ui",
        "--instance",
        &ids[1],
        "action",
        "session.focus",
        "--session",
        "not-a-uuid",
    ]);
    assert!(!status.success());
    assert!(invalid["result"]["error"]["message"]
        .to_string()
        .contains("must be a UUID"));

    let (status, receipt) = cli(&[
        "ui",
        "--instance",
        &ids[0],
        "action",
        "search.open",
        "--query",
        "one",
    ]);
    assert!(status.success());
    assert_eq!(receipt["instance_id"], ids[0]);
    assert!(receipt["request_id"].as_str().is_some());
    assert_eq!(receipt["result"]["ok"], true);
    assert!(
        receipt["revision"].as_u64().unwrap()
            >= receipt["result"]["state"]["revision"].as_u64().unwrap()
    );
    first.wait_for("Search one");
    assert!(!second.frame().contains("Search one"));
    let (status, state) = cli(&["ui", "--instance", &ids[0], "state"]);
    assert!(status.success());
    assert_eq!(state["search_query"], "one");
    assert_eq!(state["focused_pane"], "search");

    let (status, _) = cli(&[
        "ui",
        "--instance",
        &ids[1],
        "action",
        "search.open",
        "--query",
        "two",
    ]);
    assert!(status.success());
    second.wait_for("Search two");
    let (status, state) = cli(&["ui", "--instance", &ids[1], "state"]);
    assert!(status.success());
    assert_eq!(state["search_query"], "two");
    let revision = state["revision"].clone();
    let (status, _) = cli(&[
        "ui",
        "--instance",
        &ids[1],
        "action",
        "search.open",
        "--query",
        "two",
    ]);
    assert!(status.success());
    second.wait_for("Search two");
    let (status, state) = cli(&["ui", "--instance", &ids[1], "state"]);
    assert!(status.success());
    assert_eq!(state["search_query"], "two");
    assert!(state["revision"].as_u64().unwrap() > revision.as_u64().unwrap());
    let since = revision.as_u64().unwrap().to_string();
    let (status, outcome) = cli(&[
        "ui",
        "--instance",
        &ids[1],
        "watch",
        "--since",
        &since,
        "--once",
    ]);
    assert!(status.success());
    assert!(outcome["events"].as_array().unwrap().iter().any(|event| {
        event["kind"] == "action.completed" && event["value"]["action"] == "search.open"
    }));

    let (status, sessions) = cli(&["session", "list"]);
    assert!(status.success());
    let session = sessions[0]["id"].as_str().expect("session id");
    let (status, _) = cli(&[
        "ui",
        "--instance",
        &ids[1],
        "action",
        "session.focus",
        "--session",
        session,
    ]);
    assert!(status.success());
    let (status, state) = cli(&["ui", "--instance", &ids[1], "state"]);
    assert!(status.success());
    assert_eq!(state["selected_session"], session);
    assert_eq!(state["focused_pane"], "agent");
    let (status, first_state) = cli(&["ui", "--instance", &ids[0], "state"]);
    assert!(status.success());
    assert_eq!(first_state["search_query"], "one");

    assert!(second.quit().success());
    let (status, stale) = cli(&["ui", "--instance", &ids[1], "state"]);
    assert!(!status.success());
    assert!(stale["error"].to_string().contains("instance"));
    assert!(first.quit().success());
}

#[test]
fn destructive_ui_action_needs_a_single_use_instance_bound_confirmation() {
    let Some((profile, mut tui)) = shell_session() else {
        return;
    };
    let mut discover = Command::new(env!("CARGO_BIN_EXE_thurbox-cli"));
    profile.apply(&mut discover);
    let output = discover
        .args(["--json", "ui", "instances"])
        .output()
        .expect("discover UI");
    assert!(output.status.success());
    let discovery: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("instance JSON");
    let instance: thurbox::ui_control::Instance =
        serde_json::from_value(discovery["instances"][0].clone()).expect("running interface");
    let list = || {
        let mut command = Command::new(env!("CARGO_BIN_EXE_thurbox-cli"));
        profile.apply(&mut command);
        let output = command
            .args(["--json", "session", "list"])
            .output()
            .expect("list sessions");
        assert!(output.status.success());
        serde_json::from_slice::<serde_json::Value>(&output.stdout).expect("session JSON")
    };
    let sessions = list();
    let session = sessions[0]["id"].as_str().expect("session id");
    tui.send(b"\x08");
    let addressed = thurbox::ui_control::send(
        &instance,
        &thurbox::ui_control::Request::Input {
            target: "sessions".into(),
            input: thurbox::ui_control::InputOperation::Key { chord: "D".into() },
        },
    )
    .expect("addressed key reply");
    assert_eq!(
        addressed.result["error"]["code"], "confirmation_required",
        "{}",
        addressed.result
    );
    assert!(list()
        .as_array()
        .unwrap()
        .iter()
        .any(|row| row["id"] == session));
    let requested = thurbox::ui_control::send(
        &instance,
        &thurbox::ui_control::Request::Action {
            name: "sessions.force_delete".into(),
            args: serde_json::json!({"session_id": session}),
        },
    )
    .expect("action reply");
    assert_eq!(requested.result["error"]["code"], "confirmation_required");
    let ticket = requested.result["error"]["ticket"]
        .as_str()
        .expect("confirmation ticket");
    assert!(!ticket.is_empty());
    let audit = profile.path("data/ui-control/audit.jsonl");
    assert_eq!(
        std::fs::metadata(&audit).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let recorded = std::fs::read_to_string(&audit).expect("audit record");
    assert!(recorded.contains("sessions.force_delete"));
    assert!(recorded.contains(session));
    assert_eq!(
        recorded
            .lines()
            .filter(|line| line.contains("\"outcome\":\"confirmation_required\""))
            .count(),
        1,
        "the decision is recorded once"
    );
    let still_present = list();
    assert!(still_present
        .as_array()
        .unwrap()
        .iter()
        .any(|row| row["id"] == session));

    let confirm = |ticket: &str| {
        let mut command = Command::new(env!("CARGO_BIN_EXE_thurbox-cli"));
        profile.apply(&mut command);
        let output = command
            .args([
                "--json",
                "ui",
                "--instance",
                &instance.id,
                "confirm",
                ticket,
            ])
            .output()
            .expect("confirm command");
        (
            output.status,
            serde_json::from_slice::<serde_json::Value>(&output.stdout).expect("confirmation JSON"),
        )
    };
    let (status, confirmed) = confirm(ticket);
    assert!(status.success(), "confirmation: {confirmed}");
    let (status, replay) = confirm(ticket);
    assert!(!status.success());
    assert_eq!(replay["result"]["error"]["code"], "invalid_ticket");
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
fn destructive_ui_action_fails_closed_when_the_audit_file_is_not_private() {
    let Some((profile, mut tui)) = shell_session() else {
        return;
    };
    let mut command = Command::new(env!("CARGO_BIN_EXE_thurbox-cli"));
    profile.apply(&mut command);
    let output = command
        .args(["--json", "ui", "instances"])
        .output()
        .expect("instances");
    let instances: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let instance: thurbox::ui_control::Instance =
        serde_json::from_value(instances["instances"][0].clone()).unwrap();
    let mut command = Command::new(env!("CARGO_BIN_EXE_thurbox-cli"));
    profile.apply(&mut command);
    let output = command
        .args(["--json", "session", "list"])
        .output()
        .expect("sessions");
    let sessions: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let session = sessions[0]["id"].as_str().unwrap();
    let audit = profile.path("data/ui-control/audit.jsonl");
    std::fs::write(&audit, "").expect("audit file");
    std::fs::set_permissions(&audit, std::fs::Permissions::from_mode(0o644))
        .expect("weaken audit permissions");
    let reply = thurbox::ui_control::send(
        &instance,
        &thurbox::ui_control::Request::Action {
            name: "sessions.force_delete".into(),
            args: serde_json::json!({"session_id": session}),
        },
    )
    .expect("action reply");
    assert_eq!(reply.result["error"]["code"], "audit_unavailable");
    let mut command = Command::new(env!("CARGO_BIN_EXE_thurbox-cli"));
    profile.apply(&mut command);
    let output = command
        .args(["--json", "session", "list"])
        .output()
        .expect("sessions");
    let still_present: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(still_present
        .as_array()
        .unwrap()
        .iter()
        .any(|row| row["id"] == session));
    assert!(tui.quit().success());
}

#[test]
fn confirmation_rejects_another_instance_and_a_removed_target() {
    let Some((profile, mut first)) = shell_session() else {
        return;
    };
    let mut second = Tui::spawn(&profile, 40, 120);
    second.wait_for("probe");
    let cli = |args: &[&str]| {
        let mut command = Command::new(env!("CARGO_BIN_EXE_thurbox-cli"));
        profile.apply(&mut command);
        let output = command.args(["--json"]).args(args).output().expect("CLI");
        (
            output.status,
            serde_json::from_slice::<serde_json::Value>(&output.stdout).expect("JSON"),
        )
    };
    let (_, instances) = cli(&["ui", "instances"]);
    let rows = instances["instances"].as_array().expect("instances");
    let first_id = rows
        .iter()
        .find(|row| row["pid"] == first.child.id())
        .unwrap()["id"]
        .as_str()
        .unwrap();
    let second_id = rows
        .iter()
        .find(|row| row["pid"] == second.child.id())
        .unwrap()["id"]
        .as_str()
        .unwrap();
    let (_, sessions) = cli(&["session", "list"]);
    let session = sessions[0]["id"].as_str().unwrap();
    let (status, request) = cli(&[
        "ui",
        "--instance",
        first_id,
        "action",
        "sessions.force_delete",
        "--session",
        session,
    ]);
    assert!(!status.success());
    let ticket = request["result"]["error"]["ticket"]
        .as_str()
        .expect("ticket");
    let (status, cross) = cli(&["ui", "--instance", second_id, "confirm", ticket]);
    assert!(!status.success());
    assert_eq!(cross["result"]["error"]["code"], "invalid_ticket");
    let (status, _) = cli(&["session", "delete", session]);
    assert!(status.success());
    first.wait_gone("probe");
    let (status, stale) = cli(&["ui", "--instance", first_id, "confirm", ticket]);
    assert!(!status.success());
    assert_eq!(stale["result"]["error"]["code"], "stale_target");
    assert!(second.quit().success());
    assert!(first.quit().success());
}

#[test]
fn keyboard_force_delete_uses_the_shared_confirmation_float() {
    let Some((profile, mut tui)) = shell_session() else {
        return;
    };
    tui.send(b"\x08");
    tui.send(b"D");
    tui.wait_for("Confirm");
    let mut list = Command::new(env!("CARGO_BIN_EXE_thurbox-cli"));
    profile.apply(&mut list);
    let output = list
        .args(["--json", "session", "list"])
        .output()
        .expect("sessions");
    let sessions: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(sessions.as_array().unwrap().len(), 1);
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
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("ui");
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
            Path::new(env!("CARGO_MANIFEST_DIR")).join("ui/plugins/10_sessions.lua"),
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
    tui.wait_for("PANES");
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
    tui.wait_gone("PANES");
    let status = tui.quit();
    assert!(status.success(), "exit must be clean: {status:?}");
}

/// A pane written for this test, dropped in beside the bundled ones.
fn interface_plus(name: &str, body: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("ui");
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
      text = "tb-typist " .. (state.step or 0)
        .. " absent=" .. (state.absent or 0)
        .. " full=" .. (state.full and "yes" or "no"),
    }
  end,
  keys = {
    { key = "ctrl+g", action = "typist.next", desc = "next step", scope = "global" },
  },
  on_action = function(action)
    if action ~= "typist.next" then
      return false
    end
    state.step = (state.step or 0) + 1
    if state.step == 1 then
      command("program", { text = "cat", repo = "cat" })
      for _ = 1, 5000 do
        command("program", { text = "cat", keys = "x" })
      end
    else
      command("program", { text = "gone", keys = "x" })
    end
    return true
  end,
  on_event = function(_, payload)
    local error = payload.error or ""
    if error:find("no running program", 1, true) then
      state.absent = (state.absent or 0) + 1
    end
    if error:find("full", 1, true) then
      state.full = true
    end
  end,
}"#;

/// Every refused `keys` send reaches the plugin's `command.failed`, with the
/// reason it was refused (#1119).
///
/// The burst is the case that was misreported: one batch holds more sends than
/// a pane's input channel, so the tail is refused while `cat` is plainly
/// running — and was reported as "no running program", to the band only. The
/// second step is the honest version of that message, which also never reached
/// the plugin.
#[test]
fn a_refused_keystroke_reaches_the_plugin_with_the_reason_it_was_refused() {
    if !have_tmux() {
        eprintln!("skipping: tmux is not installed");
        return;
    }
    let interface = interface_plus("91_typist.lua", TYPIST);
    let profile = Profile::new();
    trust(&profile, interface.path(), "plugins/91_typist.lua", TYPIST);
    let mut tui = Tui::spawn_with(&profile, 40, 120, |cmd| {
        cmd.env("THURBOX_UI_DIR", interface.path());
    });
    tui.wait_for("tb-typist 0");

    tui.send(b"\x07");
    tui.wait_for("tb-typist 1 absent=0 full=yes");
    assert!(
        !tui.frame().contains("no running program"),
        "a full input channel was reported as a missing program:\n{}",
        tui.frame()
    );

    tui.send(b"\x07");
    tui.wait_for("tb-typist 2 absent=1");

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

/// A local shell beside a stored SSH session, with no live remote connection.
/// The session list is the subject; no test needs to attach to the remote pane.
fn hosted_session_list() -> Option<(Profile, Tui)> {
    shell_session_prepared(
        |profile| {
            let second = profile.path("second");
            std::fs::create_dir_all(&second).expect("second root");
            let local_repo = repo(&second);
            profile.cli(&[
                "session",
                "create",
                "--name",
                "local-row",
                "--repo-path",
                local_repo.to_str().expect("utf-8 path"),
                "--agent",
                "shell",
            ]);
            std::fs::write(
                profile.path("config/hosts.toml"),
                "[[hosts]]\nname = \"example-ssh\"\ndestination = \"invalid.example\"\n",
            )
            .expect("hosts");
            let db = rusqlite::Connection::open(profile.path("data/thurbox.db")).expect("database");
            db.execute(
                "UPDATE sessions SET backend_type = 'ssh:example-ssh' WHERE name = 'probe'",
                [],
            )
            .expect("put probe on host");
        },
        |_| {},
    )
}

#[test]
fn ssh_host_labels_do_not_depend_on_platform() {
    let Some((_profile, mut tui)) = shell_session_prepared(
        |profile| {
            for name in ["linux-session", "windows-session"] {
                profile.cli(&[
                    "session",
                    "create",
                    "--name",
                    name,
                    "--repo-path",
                    profile.path("repo").to_str().unwrap(),
                    "--agent",
                    "shell",
                ]);
            }
            std::fs::write(
                profile.path("config/hosts.toml"),
                "[[hosts]]\nname = \"build-box\"\ndestination = \"invalid.example\"\nplatform = \"posix\"\n\n[[hosts]]\nname = \"win-desk\"\ndestination = \"invalid.example\"\nplatform = \"windows\"\n",
            ).expect("hosts");
            let db = rusqlite::Connection::open(profile.path("data/thurbox.db")).unwrap();
            for (name, backend) in [
                ("linux-session", "ssh:build-box:tmux"),
                ("windows-session", "ssh:win-desk:psmux"),
            ] {
                db.execute(
                    "UPDATE sessions SET backend_type = ?1 WHERE name = ?2",
                    [backend, name],
                )
                .unwrap();
            }
        },
        |_| {},
    ) else {
        return;
    };
    tui.wait_for("ssh build-box");
    tui.wait_for("ssh win-desk");
    tui.press(0, tui.find("ssh win-desk"));
    tui.wait_gone("windows-session");
    assert!(tui.frame().contains("ssh win-desk"));
    assert!(tui.quit().success());
}

fn selected_session(profile: &Profile) -> Option<String> {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_thurbox-cli"));
    profile.apply(&mut cmd);
    let output = cmd
        .args(["--json", "ui", "state"])
        .output()
        .expect("ui state");
    assert!(output.status.success());
    let state: serde_json::Value = serde_json::from_slice(&output.stdout).expect("state JSON");
    state["selected_session"].as_str().map(str::to_owned)
}

fn select_expanded_host(tui: &mut Tui) {
    tui.press(0, tui.find("example-ssh"));
    tui.wait_gone("probe");
    tui.send(b"l");
    tui.wait_for("probe");
}

#[test]
fn session_host_left_arrow_collapses_expanded_host() {
    let Some((_profile, mut tui)) = hosted_session_list() else {
        return;
    };
    select_expanded_host(&mut tui);
    tui.send(b"\x1b[D");
    tui.wait_gone("probe");
    assert!(tui.quit().success());
}

#[test]
fn session_host_right_arrow_expands_collapsed_host() {
    let Some((_profile, mut tui)) = hosted_session_list() else {
        return;
    };
    tui.press(0, tui.find("example-ssh"));
    tui.wait_gone("probe");
    tui.send(b"\x1b[C");
    tui.wait_for("probe");
    assert!(tui.quit().success());
}

#[test]
fn session_host_left_arrow_moves_to_parent_before_collapsing() {
    let Some((profile, mut tui)) = hosted_session_list() else {
        return;
    };
    tui.press(0, tui.find("probe"));
    tui.send(b"\x1b[D");
    tui.wait_until("host selection with child still visible", |frame| {
        frame.contains("probe") && selected_session(&profile).is_none()
    });
    tui.send(b"\x1b[D");
    tui.wait_gone("probe");
    assert!(tui.quit().success());
}

#[test]
fn session_host_right_arrow_moves_to_first_session() {
    let Some((profile, mut tui)) = hosted_session_list() else {
        return;
    };
    let id = thurbox::storage::Database::open(&profile.path("data/thurbox.db"))
        .expect("database")
        .get_session_by_name("probe")
        .expect("read")
        .expect("probe")
        .id
        .to_string();
    select_expanded_host(&mut tui);
    // Host, then its repo row, then the repo's first session.
    tui.send(b"\x1b[C\x1b[C");
    tui.wait_until("first session selected", |_| {
        selected_session(&profile).as_deref() == Some(&id)
    });
    tui.send(b"\x1b[C");
    tui.send(b"r");
    tui.wait_for("Restart probe?");
    tui.send(ESC);
    assert!(tui.quit().success());
}

#[test]
fn session_host_right_click_toggles_without_opening_menu() {
    let Some((_profile, mut tui)) = hosted_session_list() else {
        return;
    };
    tui.press(2, tui.find("example-ssh"));
    tui.wait_gone("probe");
    assert!(!tui.frame().contains("Restore deleted"));
    tui.press(2, tui.find("example-ssh"));
    tui.wait_for("probe");
    assert!(!tui.frame().contains("Restore deleted"));
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

#[test]
fn session_host_enter_toggles_the_selected_host() {
    let Some((_profile, mut tui)) = hosted_session_list() else {
        return;
    };
    select_expanded_host(&mut tui);
    tui.send(b"\r");
    tui.wait_gone("probe");
    tui.send(b"\r");
    tui.wait_for("probe");
    assert!(tui.quit().success());
}

#[test]
fn session_host_double_click_toggles_once() {
    let Some((_profile, mut tui)) = hosted_session_list() else {
        return;
    };
    let point = tui.find("example-ssh");
    tui.press(0, point);
    tui.press(0, point);
    tui.wait_gone("probe");
    tui.send(b"r");
    tui.send(F1);
    tui.wait_for("Keybindings");
    assert!(!tui.frame().contains("probe"));
    tui.send(ESC);
    assert!(tui.quit().success());
}

#[test]
fn session_context_menus_reuse_bulk_fold_actions() {
    let Some((_profile, mut tui)) = hosted_session_list() else {
        return;
    };
    tui.press(2, tui.find("◌ local-row"));
    tui.wait_for("Delete + worktree");
    tui.wait_for("Collapse all");
    tui.press(0, tui.find("Collapse all"));
    tui.wait_gone("Collapse all");
    tui.wait_gone("local-row");
    tui.wait_gone("probe");

    tui.press(2, (3, 20));
    tui.wait_for("Expand all");
    tui.press(0, tui.find("Expand all"));
    tui.wait_gone("Expand all");
    tui.wait_for("local-row");
    tui.wait_for("probe");
    assert!(tui.quit().success());
}

#[test]
fn session_host_bulk_keys_and_boundary_navigation_skip_folded_children() {
    let Some((_profile, mut tui)) = hosted_session_list() else {
        return;
    };
    tui.send(b"\x08H");
    tui.wait_gone("probe");
    tui.wait_gone("local-row");
    tui.send(b"\x1b[H\x1b[6~\r\x1b[C\x1b[C");
    tui.wait_for("probe");
    assert!(!tui.frame().contains("local-row"));
    tui.send(b"H\x1b[F\x1b[5~\r\x1b[C\x1b[C");
    tui.wait_for("local-row");
    assert!(!tui.frame().contains("probe"));
    tui.send(b"L");
    tui.wait_for("probe");
    assert!(tui.quit().success());
}

#[test]
fn session_host_jump_keys_visit_host_handles() {
    let Some((_profile, mut tui)) = hosted_session_list() else {
        return;
    };
    tui.press(0, tui.find("◌ local-row"));
    tui.send(b"]h");
    tui.wait_gone("probe");
    tui.send(b"[h");
    tui.wait_gone("local-row");
    tui.send(b"L");
    tui.wait_for("local-row");
    tui.wait_for("probe");
    assert!(tui.quit().success());
}

#[test]
fn session_host_control_api_addresses_folds_and_reports_state() {
    let Some((profile, mut tui)) = hosted_session_list() else {
        return;
    };
    let mut catalog = Command::new(env!("CARGO_BIN_EXE_thurbox-cli"));
    profile.apply(&mut catalog);
    let output = catalog
        .args(["--json", "ui", "actions"])
        .output()
        .expect("action catalog");
    assert!(output.status.success());
    let catalog: serde_json::Value = serde_json::from_slice(&output.stdout).expect("catalog JSON");
    let actions = catalog["actions"].as_array().expect("actions");
    for name in [
        "sessions.collapse_host",
        "sessions.expand_host",
        "sessions.toggle_host",
        "sessions.parent_host",
        "sessions.first_child",
        "sessions.collapse_all",
        "sessions.expand_all",
        "sessions.next_host",
        "sessions.previous_host",
        "sessions.next_attention",
        "sessions.first",
        "sessions.last",
        "sessions.page_up",
        "sessions.page_down",
    ] {
        assert_eq!(
            actions
                .iter()
                .filter(|action| action["name"] == name)
                .count(),
            1,
            "catalog should declare {name} once"
        );
    }
    profile.cli(&[
        "ui",
        "action",
        "sessions.collapse_host",
        "--arg",
        "host=example-ssh",
    ]);
    tui.wait_gone("probe");
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_thurbox-cli"));
    profile.apply(&mut cmd);
    let output = cmd
        .args(["--json", "ui", "state"])
        .output()
        .expect("ui state");
    assert!(output.status.success());
    let state: serde_json::Value = serde_json::from_slice(&output.stdout).expect("state JSON");
    let pane = state["plugin_state"]
        .as_object()
        .expect("plugin state")
        .values()
        .find(|pane| pane.get("selected_host").is_some())
        .expect("session fold projection");
    assert_eq!(pane["selected_host"], "example-ssh");
    assert_eq!(pane["host_collapsed"], true);
    assert_eq!(pane["folded_host_count"], 1);
    profile.cli(&[
        "ui",
        "action",
        "sessions.expand_host",
        "--arg",
        "host=example-ssh",
    ]);
    tui.wait_for("probe");
    assert!(tui.quit().success());
}

#[test]
fn session_host_row_folds_by_key_reveals_search_hits_and_survives_restart() {
    let Some((profile, mut tui)) = hosted_session_list() else {
        return;
    };
    tui.wait_for("example-ssh");
    tui.wait_for("probe");
    tui.send(b"\x08"); // Ctrl+H focuses the session list.
    tui.send(b"j");
    tui.send(b"h");
    tui.wait_until("the host to fold", |frame| {
        frame.contains("example-ssh") && !frame.contains("probe")
    });
    tui.send(b"jjj");
    let local_id = thurbox::storage::Database::open(&profile.path("data/thurbox.db"))
        .expect("database")
        .get_session_by_name("local-row")
        .expect("read local session")
        .expect("local session")
        .id
        .to_string();
    tui.wait_until("navigation to skip the folded child", |_| {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_thurbox-cli"));
        profile.apply(&mut cmd);
        let output = cmd
            .args(["--json", "ui", "state"])
            .output()
            .expect("ui state");
        output.status.success()
            && serde_json::from_slice::<serde_json::Value>(&output.stdout)
                .ok()
                .and_then(|state| state["selected_session"].as_str().map(str::to_string))
                .as_deref()
                == Some(local_id.as_str())
    });
    assert!(
        !tui.frame().contains("probe"),
        "navigation should skip folded children:\n{}",
        tui.frame()
    );
    tui.send(CTRL_SLASH);
    tui.wait_for("Search");
    tui.send(b"probe");
    tui.wait_until("the search hit inside the folded host", |frame| {
        frame.contains("probe") && frame.contains("example-ssh")
    });
    tui.send(ESC);
    tui.wait_gone("Search");
    tui.wait_until("the host to fold again after search", |frame| {
        frame.contains("example-ssh") && !frame.contains("probe")
    });
    assert!(tui.quit().success());

    let mut reopened = Tui::spawn(&profile, 40, 120);
    reopened.wait_for("example-ssh");
    assert!(
        !reopened.frame().contains("probe"),
        "fold state should survive restart:\n{}",
        reopened.frame()
    );
    reopened.send(b"\x08");
    reopened.send(b"j");
    reopened.send(b"l");
    reopened.wait_for("probe");
    reopened.press(0, reopened.find("example-ssh"));
    reopened.wait_until("a mouse press to fold the host", |frame| {
        frame.contains("example-ssh") && !frame.contains("probe")
    });
    std::thread::sleep(Duration::from_millis(500));
    reopened.press(0, reopened.find("example-ssh"));
    reopened.wait_for("probe");
    assert!(reopened.quit().success());
}

#[test]
fn activating_a_search_hit_inside_a_folded_host_keeps_that_session_selected() {
    let Some((profile, mut tui)) = hosted_session_list() else {
        return;
    };
    tui.wait_for("example-ssh");
    tui.wait_for("probe");
    tui.send(b"\x08");
    tui.send(b"j");
    tui.send(b"h");
    tui.wait_until("the host to fold", |frame| {
        frame.contains("example-ssh") && !frame.contains("probe")
    });
    tui.send(CTRL_SLASH);
    tui.wait_for("Search");
    tui.send(b"probe");
    tui.wait_until("the folded child to appear in search", |frame| {
        frame.contains("probe") && frame.contains("example-ssh")
    });
    tui.send(b"\r");
    tui.wait_gone("Search");
    let probe_id = thurbox::storage::Database::open(&profile.path("data/thurbox.db"))
        .expect("database")
        .get_session_by_name("probe")
        .expect("read probe")
        .expect("probe")
        .id
        .to_string();
    tui.wait_until("the accepted remote session to stay selected", |frame| {
        if !frame.contains("⇅ probe") {
            return false;
        }
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_thurbox-cli"));
        profile.apply(&mut cmd);
        let output = cmd
            .args(["--json", "ui", "state"])
            .output()
            .expect("ui state");
        output.status.success()
            && serde_json::from_slice::<serde_json::Value>(&output.stdout)
                .ok()
                .and_then(|state| state["selected_session"].as_str().map(str::to_string))
                .as_deref()
                == Some(probe_id.as_str())
    });
    assert!(tui.quit().success());
}

/// `probe` in `repo` beside `sibling` in `other-repo`, both local: the local
/// host row holds two repo rows, each holding one session. `prepare` runs
/// before the binary starts, for a scenario that changes a setting.
fn nested_repo_list_with(prepare: impl FnOnce(&Profile)) -> Option<(Profile, Tui)> {
    let found = shell_session_prepared(
        |profile| {
            let second = profile.path("second");
            std::fs::create_dir_all(&second).expect("second root");
            let other = named_repo(&second, "other-repo");
            profile.cli(&[
                "session",
                "create",
                "--name",
                "sibling",
                "--repo-path",
                other.to_str().expect("utf-8 path"),
                "--agent",
                "shell",
            ]);
            prepare(profile);
        },
        |_| {},
    );
    if let Some((_, tui)) = &found {
        tui.wait_for("sibling");
    }
    found
}

fn nested_repo_list() -> Option<(Profile, Tui)> {
    nested_repo_list_with(|_| {})
}

const PROBE_REPO: &str = "repo:\0local\u{1}repo";
const LOCAL_HOST: &str = "host:\0local";

/// The row the session list's cursor is on — a session id, or the target of
/// a host or repo row, which `selected_session` never reports.
fn selected_row(profile: &Profile) -> Option<String> {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_thurbox-cli"));
    profile.apply(&mut cmd);
    let output = cmd
        .args(["--json", "ui", "state"])
        .output()
        .expect("ui state");
    assert!(output.status.success());
    let state: serde_json::Value = serde_json::from_slice(&output.stdout).expect("state JSON");
    state["plugin_state"]
        .as_object()?
        .values()
        .find(|pane| pane.get("folded_host_count").is_some())?["selected_row"]
        .as_str()
        .map(str::to_owned)
}

fn wait_selected(tui: &Tui, profile: &Profile, row: &str) {
    tui.wait_until(&format!("{row:?} to be selected"), |_| {
        selected_row(profile).as_deref() == Some(row)
    });
}

fn session_id(profile: &Profile, name: &str) -> String {
    thurbox::storage::Database::open(&profile.path("data/thurbox.db"))
        .expect("database")
        .get_session_by_name(name)
        .expect("read")
        .expect("session")
        .id
        .to_string()
}

/// A click folds a repo row and selects it; `l` unfolds it again.
fn select_expanded_repo(tui: &mut Tui) {
    tui.press(0, tui.find("▾ repo"));
    tui.wait_gone("probe");
    tui.send(b"l");
    tui.wait_for("probe");
}

#[test]
fn session_repo_left_arrow_collapses_expanded_repo() {
    let Some((profile, mut tui)) = nested_repo_list() else {
        return;
    };
    select_expanded_repo(&mut tui);
    tui.send(b"\x1b[D");
    tui.wait_gone("probe");
    assert!(tui.frame().contains("sibling"), "only the repo folds");
    wait_selected(&tui, &profile, PROBE_REPO);
    assert!(tui.quit().success());

    let mut reopened = Tui::spawn(&profile, 40, 120);
    reopened.wait_for("sibling");
    assert!(
        !reopened.frame().contains("probe"),
        "the repo fold should survive a restart:\n{}",
        reopened.frame()
    );
    assert!(reopened.quit().success());
}

#[test]
fn session_repo_right_arrow_expands_collapsed_repo() {
    let Some((profile, mut tui)) = nested_repo_list() else {
        return;
    };
    tui.press(0, tui.find("▾ repo"));
    tui.wait_gone("probe");
    tui.send(b"\x1b[C");
    tui.wait_for("probe");
    wait_selected(&tui, &profile, PROBE_REPO);
    assert!(tui.quit().success());
}

#[test]
fn session_repo_left_arrow_walks_from_session_to_repo_to_host() {
    let Some((profile, mut tui)) = nested_repo_list() else {
        return;
    };
    tui.press(0, tui.find("probe"));
    tui.send(b"\x1b[D");
    wait_selected(&tui, &profile, PROBE_REPO);
    assert!(
        tui.frame().contains("probe"),
        "selecting the repo folds nothing"
    );
    tui.send(b"\x1b[D");
    tui.wait_gone("probe");
    tui.send(b"\x1b[D");
    wait_selected(&tui, &profile, LOCAL_HOST);
    assert!(
        tui.frame().contains("sibling"),
        "selecting the host folds nothing"
    );
    tui.send(b"\x1b[D");
    tui.wait_gone("sibling");
    assert!(tui.quit().success());
}

#[test]
fn session_repo_right_arrow_walks_from_host_to_repo_to_session() {
    let Some((profile, mut tui)) = nested_repo_list() else {
        return;
    };
    let sibling = session_id(&profile, "sibling");
    tui.press(0, tui.find("⌂ local"));
    tui.wait_gone("sibling");
    tui.send(b"\x1b[C");
    tui.wait_for("sibling");
    // `other-repo` is the host's first repo row.
    tui.send(b"\x1b[C");
    wait_selected(&tui, &profile, "repo:\0local\u{1}other-repo");
    tui.press(0, tui.find("▾ other-repo"));
    tui.wait_gone("sibling");
    tui.send(b"\x1b[C");
    tui.wait_for("sibling");
    tui.send(b"\x1b[C");
    wait_selected(&tui, &profile, &sibling);
    assert!(tui.quit().success());
}

#[test]
fn session_repo_arrows_without_host_rows() {
    let Some((profile, mut tui)) = nested_repo_list_with(|profile| {
        std::fs::write(
            profile.path("config/ui.json"),
            r#"{"settings":{"sessions.group_by_host":false}}"#,
        )
        .expect("ui.json");
    }) else {
        return;
    };
    let repo = "repo:\u{1}repo";
    let probe = session_id(&profile, "probe");
    tui.press(0, tui.find("probe"));
    tui.send(b"\x1b[D");
    wait_selected(&tui, &profile, repo);
    tui.send(b"\x1b[D");
    tui.wait_gone("probe");
    // Nothing above a repo row: Left there stays put.
    tui.send(b"\x1b[D");
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(selected_row(&profile).as_deref(), Some(repo));
    tui.send(b"\x1b[C");
    tui.wait_for("probe");
    tui.send(b"\x1b[C");
    wait_selected(&tui, &profile, &probe);
    assert!(tui.quit().success());
}

#[test]
fn session_repo_enter_toggles_the_selected_repo() {
    let Some((_profile, mut tui)) = nested_repo_list() else {
        return;
    };
    select_expanded_repo(&mut tui);
    tui.send(b"\r");
    tui.wait_gone("probe");
    tui.send(b"\r");
    tui.wait_for("probe");
    assert!(tui.quit().success());
}

#[test]
fn session_repo_double_click_toggles_once() {
    let Some((_profile, mut tui)) = nested_repo_list() else {
        return;
    };
    let point = tui.find("▾ repo");
    tui.press(0, point);
    tui.press(0, point);
    tui.wait_gone("probe");
    tui.send(F1);
    tui.wait_for("Keybindings");
    assert!(!tui.frame().contains("probe"));
    tui.send(ESC);
    assert!(tui.quit().success());
}

#[test]
fn session_repo_right_click_toggles_without_opening_menu() {
    let Some((_profile, mut tui)) = nested_repo_list() else {
        return;
    };
    tui.press(2, tui.find("▾ repo"));
    tui.wait_gone("probe");
    assert!(!tui.frame().contains("Restore deleted"));
    tui.press(2, tui.find("▸ repo"));
    tui.wait_for("probe");
    assert!(!tui.frame().contains("Restore deleted"));
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
    std::fs::write(
        profile.path("config/agents.toml"),
        "default = \"shell\"\n\n[[agents]]\nname = \"shell\"\ncommand = \"sh\"\nargs = []\n",
    )
    .expect("seed agents");
    let repo = repo(profile.root.path());

    let mut create = vec![
        "session",
        "create",
        "--name",
        "probe",
        "--repo-path",
        repo.to_str().expect("utf-8 path"),
        "--agent",
        "shell",
    ];
    if let Some(branch) = branch {
        create.extend(["--worktree-branch", branch]);
    }
    profile.cli(&create);
    // A database with session history is a v1 profile as far as the one-time
    // gate can tell, and a gate on a pty is a real prompt; this is the
    // headless answer to it.
    profile.cli(&["config", "accept-interface"]);
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
fn deleting_an_agent_window_while_the_tui_is_open_relaunches_it_once() {
    let Some((profile, mut tui)) =
        shell_session_prepared_on_branch(Some("test/relaunch-same-worktree"), |_| {}, |_| {})
    else {
        return;
    };

    let db = thurbox::storage::Database::open(&profile.path("data/thurbox.db"))
        .expect("open profile database");
    let original = db
        .get_session_by_name("probe")
        .expect("read session")
        .expect("probe session");
    assert_eq!(original.worktrees.len(), 1);

    let old_pane = original.backend_id.as_str();
    assert!(!old_pane.is_empty());

    let mut kill = Command::new("tmux");
    profile.apply(&mut kill);
    let killed = kill
        .args(["-L", profile.server.socket(), "kill-pane", "-t", old_pane])
        .output()
        .expect("kill pane");
    assert!(
        killed.status.success(),
        "{}",
        String::from_utf8_lossy(&killed.stderr)
    );

    let deadline = Instant::now() + WAIT;
    while Instant::now() < deadline
        && db
            .get_session_by_name("probe")
            .expect("read session during relaunch")
            .is_some_and(|row| row.backend_id == old_pane)
    {
        std::thread::sleep(Duration::from_millis(20));
    }
    tui.wait_for("$ ");
    tui.send(b"echo tb-relaunched-once\r");
    tui.wait_for("tb-relaunched-once");

    let mut list = Command::new("tmux");
    profile.apply(&mut list);
    let after = list
        .args(["-L", profile.server.socket()])
        .args(["list-panes", "-a", "-F", "#{pane_id} #{window_name}"])
        .output()
        .expect("list panes after relaunch");
    assert!(after.status.success());
    let panes = String::from_utf8(after.stdout).expect("pane ids");
    let agents: Vec<_> = panes
        .lines()
        .filter(|line| line.ends_with(" tb-probe"))
        .collect();
    assert_eq!(agents.len(), 1, "duplicate relaunch: {panes}");
    assert_ne!(agents[0].split(' ').next(), Some(old_pane));
    let relaunched = db
        .get_session_by_name("probe")
        .expect("read relaunched session")
        .expect("same session row");
    assert_eq!(relaunched.id, original.id);
    assert_ne!(relaunched.backend_id, original.backend_id);
    assert_eq!(relaunched.agent, original.agent);
    assert_eq!(relaunched.worktrees, original.worktrees);
    assert!(tui.quit().success());
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
fn the_tui_picker_creates_a_session_on_rmux() {
    if !have_rmux() {
        eprintln!("skipping: rmux is not installed");
        return;
    }
    let profile = Profile::new();
    struct RmuxCleanup<'a>(&'a Profile);
    impl Drop for RmuxCleanup<'_> {
        fn drop(&mut self) {
            let mut command = Command::new("rmux");
            self.0.apply(&mut command);
            let _ = command
                .args(["-L", self.0.server.socket(), "kill-server"])
                .output();
        }
    }
    let _cleanup = RmuxCleanup(&profile);
    std::fs::write(
        profile.path("config/agents.toml"),
        "default = \"shell\"\n\n[[agents]]\nname = \"shell\"\ncommand = \"sh\"\nargs = []\n",
    )
    .expect("seed agent");
    let repo = repo(profile.root.path());
    let mut tui = Tui::spawn(&profile, 40, 120);
    tui.wait_for("No sessions yet");
    tui.send(b"\x0e");
    tui.wait_for("Multiplexer");
    tui.wait_for("tmux");
    tui.wait_for("rmux");
    tui.send(b"\x1b[B");
    tui.wait_for("▸ rmux");
    tui.send(b"\r");
    tui.wait_for("Select Repos");
    tui.send(b"\t");
    tui.wait_for("Add Repo Path");
    tui.send(repo.to_str().expect("utf-8 repo path").as_bytes());
    tui.send(b"\r");
    tui.wait_until("the new repository to be selected", |frame| {
        frame
            .lines()
            .any(|line| line.contains("[x]") && line.contains("/repo"))
    });
    tui.send(b"\x1b[Z");
    tui.send(b"\r");
    tui.wait_for("Session Name");
    tui.send(b"rmux-picker\r");
    tui.wait_for("rmux-picker");
    let db = thurbox::storage::Database::open(&profile.path("data/thurbox.db"))
        .expect("open profile database");
    let row = db
        .get_session_by_name("rmux-picker")
        .expect("read session")
        .expect("created session");
    assert_eq!(row.backend_type, "local:rmux");
    tui.wait_for("$ ");
    let old_pane = row.backend_id;
    let mut kill = Command::new("rmux");
    profile.apply(&mut kill);
    let killed = kill
        .args(["-L", profile.server.socket(), "kill-pane", "-t", &old_pane])
        .output()
        .expect("kill RMUX pane while TUI is open");
    assert!(killed.status.success(), "{killed:?}");
    let deadline = Instant::now() + WAIT;
    while Instant::now() < deadline
        && db
            .get_session_by_name("rmux-picker")
            .expect("read session during relaunch")
            .is_some_and(|row| row.backend_id == old_pane)
    {
        std::thread::sleep(Duration::from_millis(20));
    }
    let relaunched = db
        .get_session_by_name("rmux-picker")
        .expect("read relaunched session")
        .expect("same session row");
    assert_ne!(relaunched.backend_id, old_pane);
    tui.wait_for("$ ");
    tui.send(b"echo rmux-relaunched-live\r");
    tui.wait_for("rmux-relaunched-live");
    assert!(tui.quit().success());
}

#[test]
fn explicit_cli_choice_overrides_an_unavailable_local_preference() {
    if !have_tmux() {
        return;
    }
    let profile = Profile::new();
    std::fs::write(
        profile.path("config/settings.toml"),
        "multiplexer = \"herdr\"\n[features]\nautomations = false\nversion_check = false\nauto_update = false\n",
    )
    .expect("set unavailable preference");
    let repo = repo(profile.root.path());

    let mut unavailable = Command::new(env!("CARGO_BIN_EXE_thurbox-cli"));
    profile.apply(&mut unavailable);
    let unavailable = unavailable
        .args([
            "session",
            "create",
            "--name",
            "unavailable",
            "--command",
            "sh",
        ])
        .arg("--repo-path")
        .arg(&repo)
        .output()
        .expect("create with preference");
    assert!(!unavailable.status.success());
    assert!(
        String::from_utf8_lossy(&unavailable.stdout).contains("herdr is unavailable"),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&unavailable.stdout),
        String::from_utf8_lossy(&unavailable.stderr)
    );

    let mut override_create = Command::new(env!("CARGO_BIN_EXE_thurbox-cli"));
    profile.apply(&mut override_create);
    let created = override_create
        .args([
            "session",
            "create",
            "--name",
            "overridden",
            "--command",
            "sh",
            "--multiplexer",
            "tmux",
        ])
        .arg("--repo-path")
        .arg(&repo)
        .output()
        .expect("create with override");
    assert!(
        created.status.success(),
        "{}",
        String::from_utf8_lossy(&created.stderr)
    );
    let db = thurbox::storage::Database::open(&profile.path("data/thurbox.db"))
        .expect("open profile database");
    let row = db
        .get_session_by_name("overridden")
        .expect("read session")
        .expect("created session");
    // Written qualified: an explicit tmux, not the legacy platform default.
    assert_eq!(row.backend_type, "local:tmux");
    assert!(db.get_session_by_name("unavailable").unwrap().is_none());
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
fn ctrl_d_asks_to_delete_a_session_whose_agent_has_exited() {
    // `Ctrl+D` is a passthrough chord: while a terminal has focus it is the
    // agent's EOF, and the delete it also means is left to the session list.
    // But an agent that ran `/exit` leaves a dead pane the window keeps
    // (remain-on-exit), and tmux still accepts `send-keys` into it — so the
    // chord was delivered to a pane no one reads and the session it should
    // have deleted hung in the list. A dead pane is doing no line editing, so
    // the delete prompt is what the chord means there.
    let Some((_profile, mut tui)) = shell_session() else {
        return;
    };

    // The row's presence is the precondition the delete acts on.
    tui.send(b"exit\r");
    tui.wait_until_quiet();
    assert!(
        tui.frame().contains("no status hooks"),
        "the session must still be listed after its agent exits:\n{}",
        tui.frame()
    );

    // 0x04 is Ctrl+D; focus never left the agent pane, so this is the
    // passthrough path, not the list's own binding.
    tui.send(b"\x04");
    tui.wait_for("Confirm");
    tui.send(b"y");
    tui.wait_gone("no status hooks");

    let status = tui.quit();
    assert!(status.success(), "exit must be clean: {status:?}");
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
        tui.frame().contains("no status hooks"),
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
        tui.frame().contains("no status hooks"),
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

/// The companion shell is the user's `$SHELL`, and a zsh started in the
/// profile's empty HOME opens its first-run wizard instead of a prompt.
fn plain_shell(cmd: &mut Command) {
    cmd.env("SHELL", "/bin/sh");
}

/// Ctrl+T there and back twice, typing into each side: the shell is its own
/// live terminal, and what it printed is still there after the agent has had
/// the pane.
fn exercise_the_shell_tab(tui: &mut Tui) {
    tui.send(b"\x14");
    wait_for_view(tui, "Shell");
    // The pane paints before the shell inside it has drawn a prompt, and a
    // keystroke sent in between is lost.
    tui.wait_until_quiet();
    tui.send(b"echo tb-in-\"\"shell\r");
    tui.wait_for("tb-in-shell");

    tui.send(b"\x14");
    wait_for_view(tui, "Agent");
    tui.wait_gone("tb-in-shell");
    tui.send(b"echo tb-in-\"\"agent\r");
    tui.wait_for("tb-in-agent");

    tui.send(b"\x14");
    wait_for_view(tui, "Shell");
    tui.wait_for("tb-in-shell");
    assert!(
        !tui.frame().contains("tb-in-agent"),
        "the Shell tab must show the shell, not the agent's terminal:\n{}",
        tui.frame()
    );
    tui.wait_until_quiet();
    tui.send(b"echo tb-still-\"\"live\r");
    tui.wait_for("tb-still-live");
}

#[test]
fn the_shell_tab_shows_switches_and_holds_a_working_shell() {
    // The classic arrangement's companion shell: a tab of the agent pane that
    // Ctrl+T raises and lowers. #1227 swapped it for a pane of its own under a
    // split layout; this pins the tab its rollback brings back.
    let Some((_profile, mut tui)) = shell_session_with(plain_shell) else {
        return;
    };
    exercise_the_shell_tab(&mut tui);

    let status = tui.quit();
    assert!(status.success(), "exit must be clean: {status:?}");
}

/// A shell session whose panes lose their grid after a second off screen.
fn shell_session_dropping_hidden_grids() -> Option<(Profile, Tui)> {
    shell_session_prepared(
        |profile| {
            let settings = profile.path("config/settings.toml");
            let seeded = std::fs::read_to_string(&settings).expect("read settings");
            std::fs::write(&settings, format!("hidden_terminal_secs = 1\n{seeded}"))
                .expect("seed settings");
        },
        plain_shell,
    )
}

/// Raise the Shell tab and prove a live shell is behind it: `marker`, typed
/// into it, is echoed back.
fn raise_a_working_shell(tui: &mut Tui, marker: &str) {
    tui.send(b"\x14");
    wait_for_view(tui, "Shell");
    tui.wait_until_quiet();
    let (head, tail) = marker.split_at(marker.len() / 2);
    tui.send(format!("echo {head}\"\"{tail}\r").as_bytes());
    tui.wait_for(marker);
}

#[test]
fn a_shell_that_exited_behind_the_agent_is_replaced_when_raised() {
    // The long-lived session's shell, left for later: it ends while the agent
    // has the pane (here `exit`; a window closed from outside is the same),
    // stays off screen long enough for its grid to be dropped, and is raised
    // again. The shell tab must hold a working shell, not the dead one's
    // blank grid — which no snapshot can rebuild, since the pane is gone.
    let Some((_profile, mut tui)) = shell_session_dropping_hidden_grids() else {
        return;
    };
    raise_a_working_shell(&mut tui, "tb-first-shell");
    tui.send(b"exit\r");
    tui.wait_until_quiet();
    tui.send(b"\x14");
    wait_for_view(&tui, "Agent");
    // Past the setting, and past the snapshot tick that does the dropping.
    std::thread::sleep(Duration::from_secs(4));

    raise_a_working_shell(&mut tui, "tb-second-shell");

    let status = tui.quit();
    assert!(status.success(), "exit must be clean: {status:?}");
}

#[test]
fn a_shell_whose_window_went_while_thurbox_was_closed_is_replaced() {
    // The shell's pane id outlives the interface in the session's row, so a
    // restart re-adopts it. A window that went in the meantime (a tmux server
    // restart, a kill from outside) must not be adopted as a shell that never
    // prints: raising the tab has to give a working one.
    let Some((profile, mut tui)) = shell_session_with(plain_shell) else {
        return;
    };
    raise_a_working_shell(&mut tui, "tb-first-shell");
    let status = tui.quit();
    assert!(status.success(), "exit must be clean: {status:?}");

    let windows = profile
        .server
        .tmux(&["list-windows", "-a", "-F", "#{window_id} #{window_name}"]);
    let shells: Vec<String> = String::from_utf8_lossy(&windows.stdout)
        .lines()
        .filter(|line| line.contains(" tbs-"))
        .filter_map(|line| line.split_whitespace().next().map(str::to_string))
        .collect();
    assert_eq!(shells.len(), 1, "one shell window to close: {windows:?}");
    profile.server.tmux(&["kill-window", "-t", &shells[0]]);

    let mut tui = Tui::spawn_with(&profile, 40, 120, plain_shell);
    tui.wait_until("the agent pane to be the focused one", |frame| {
        frame
            .lines()
            .last()
            .is_some_and(|band| band.trim_start().starts_with("Agent"))
    });
    tui.wait_for("$ ");
    raise_a_working_shell(&mut tui, "tb-second-shell");

    let status = tui.quit();
    assert!(status.success(), "exit must be clean: {status:?}");
}

/// Leave `profile` as v2.32.0 left someone who chose the `split-shell` preset:
/// that release's layout, shell pane, agent pane and `lib/panels.lua` on disk,
/// the delivery manifest recording them as written, and `layout` in
/// settings.toml. `edited` adds a line of the user's own to the layout and to
/// the shell pane, so delivery must keep both rather than refresh them.
fn as_v2_32_0_split_shell(profile: &Profile, edited: bool) {
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/v2_32_0_split_shell");
    let ui = profile.path("config/ui");
    let report = thurbox::kernel::bundled::materialize(&ui);
    assert!(report.errors.is_empty(), "{:?}", report.errors);

    let manifest_path = ui.join(".bundled.json");
    let mut manifest: serde_json::Map<String, serde_json::Value> =
        serde_json::from_str(&std::fs::read_to_string(&manifest_path).expect("manifest"))
            .expect("manifest is a map");
    for relative in [
        "layout.lua",
        "plugins/25_shell.lua",
        "plugins/20_agent.lua",
        "lib/panels.lua",
    ] {
        let shipped = std::fs::read_to_string(fixture.join(relative)).expect("fixture");
        manifest.insert(
            relative.to_string(),
            thurbox::kernel::bundled::digest(&shipped).into(),
        );
        let on_disk = if edited && matches!(relative, "layout.lua" | "plugins/25_shell.lua") {
            format!("{shipped}-- my own line\n")
        } else {
            shipped
        };
        std::fs::write(ui.join(relative), on_disk).expect("write fixture");
    }
    std::fs::write(
        &manifest_path,
        serde_json::to_string(&manifest).expect("manifest"),
    )
    .expect("write manifest");

    let settings = profile.path("config/settings.toml");
    let body = std::fs::read_to_string(&settings).expect("settings");
    std::fs::write(&settings, format!("layout = \"split-shell\"\n{body}")).expect("settings");
}

/// What every v2.32.0 split-shell profile must come back to after upgrading:
/// the one-time note, the classic Shell tab working, the shell pane taken back
/// and the settings key gone — and, started again, nothing more said.
fn assert_back_to_classic(profile: &Profile, mut tui: Tui) {
    tui.wait_for("layout presets");
    exercise_the_shell_tab(&mut tui);
    let status = tui.quit();
    assert!(status.success(), "exit must be clean: {status:?}");

    let ui = profile.path("config/ui");
    assert!(
        !ui.join("plugins/25_shell.lua").exists(),
        "the shell pane must be taken back"
    );
    let shipped = |relative: &str| {
        std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("ui")
                .join(relative),
        )
        .expect("shipped")
    };
    for relative in ["plugins/20_agent.lua", "lib/panels.lua"] {
        assert_eq!(
            std::fs::read_to_string(ui.join(relative)).expect("delivered"),
            shipped(relative),
            "{relative} must be refreshed to this release's copy"
        );
    }
    let settings: toml::Table = std::fs::read_to_string(profile.path("config/settings.toml"))
        .expect("settings")
        .parse()
        .expect("settings.toml still parses");
    assert!(
        !settings.contains_key("layout"),
        "the withdrawn key must be gone"
    );
    assert!(
        settings.contains_key("features"),
        "the rest of settings.toml must survive"
    );

    let tui = Tui::spawn(profile, 40, 120);
    tui.wait_for("probe");
    tui.wait_until_quiet();
    assert!(
        !tui.frame().contains("layout presets"),
        "the note is said once:\n{}",
        tui.frame()
    );
}

#[test]
fn a_v2_32_0_split_shell_profile_upgrades_to_the_classic_layout() {
    // Layout presets shipped in v2.32.0 and were rolled back. Someone who picked
    // `split-shell` has its layout.lua, its shell pane and the agent pane that
    // dropped its Shell tab for it — all untouched, so all refreshed or retired.
    let Some((profile, tui)) =
        shell_session_prepared(|p| as_v2_32_0_split_shell(p, false), plain_shell)
    else {
        return;
    };
    assert_back_to_classic(&profile, tui);
    assert_eq!(
        std::fs::read_to_string(profile.path("config/ui/layout.lua")).expect("layout"),
        include_str!("../ui/layout.lua"),
        "an untouched split-shell layout is refreshed to classic"
    );
}

#[test]
fn a_layout_edited_from_the_split_shell_preset_is_kept_and_still_works() {
    // Their edits are theirs and are kept. The layout still names a `shell`
    // slot, which nothing fills once the shell pane is set aside — so the arrangement's own
    // `filled` check leaves it out and the Shell tab is where the shell is.
    let Some((profile, tui)) =
        shell_session_prepared(|p| as_v2_32_0_split_shell(p, true), plain_shell)
    else {
        return;
    };
    assert_back_to_classic(&profile, tui);
    assert!(
        std::fs::read_to_string(profile.path("config/ui/layout.lua"))
            .expect("layout")
            .ends_with("-- my own line\n"),
        "an edited layout is never overwritten"
    );
    // The edited shell pane is kept too, but aside: loaded, it would be a pane
    // with a slot no arrangement places, which `plugin check` rejects.
    assert!(
        std::fs::read_to_string(profile.path("config/ui/plugins/25_shell.lua.bak"))
            .expect("the edit is kept beside the pane")
            .ends_with("-- my own line\n")
    );
    profile.cli(&["plugin", "check"]);
}

#[test]
fn ctrl_e_renames_the_selected_session_and_says_why_a_name_is_refused() {
    // Issue #1141: a session could be created and deleted from the keyboard, and
    // renamed by nothing at all. `Ctrl+E` is a readline chord like the list's
    // others, so it is pressed with the list focused; the field it opens holds
    // the current name, a refused name is explained in the field rather than
    // lost, and a good one lands in the list.
    let Some((_profile, mut tui)) = shell_session() else {
        return;
    };

    // 0x08 is Ctrl+H, the reserved way out of a focused terminal and onto the
    // list beside it.
    tui.send(b"\x08");
    tui.wait_until("the session list to be the focused one", |frame| {
        frame
            .lines()
            .last()
            .is_some_and(|band| band.trim_start().starts_with("Sessions"))
    });

    // 0x05 is Ctrl+E; 0x15 is Ctrl+U, which clears the prefilled name.
    tui.send(b"\x05");
    tui.wait_for("Rename session");
    tui.send(b"\x15");
    tui.send(b"bad/name\r");
    tui.wait_for("Name contains invalid characters");

    tui.send(b"\x15");
    tui.send(b"renamed\r");
    tui.wait_gone("Rename session");
    tui.wait_gone("probe");
    assert!(
        tui.frame().contains("renamed"),
        "the list must show the new name:\n{}",
        tui.frame()
    );

    let status = tui.quit();
    assert!(status.success(), "exit must be clean: {status:?}");
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
    /// Where `needle` is painted, as a 0-based (column, row).
    ///
    /// The column is counted in cells, not bytes: the borders to the left of
    /// a pane are multi-byte glyphs, and a byte offset lands a press several
    /// cells into the text it was aimed at.
    fn find(&self, needle: &str) -> (u16, u16) {
        let rows = self.screen.lock().unwrap().screen().size().0;
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

/// What the second session's app copies, once the test lets it.
const BACKGROUND_COPY: &str = "tb-background-secret";

/// The shell session, plus a second one — `copier` — that prints an OSC 52
/// write of [`BACKGROUND_COPY`] when `copy-now` appears in the profile, says
/// so by creating `copied`, and is then a shell. `probe` holds the focus;
/// `copier` is off screen.
fn shell_session_beside_a_copier() -> Option<(Profile, Tui)> {
    let (profile, mut tui) = shell_session_prepared(
        |profile| {
            let script = format!(
                "echo tb-copier-ready; while [ ! -e '{}' ]; do sleep 0.05; done; {}; : > '{}'; \
                 exec sh",
                profile.path("copy-now").display(),
                osc52_printf("c", &b64(BACKGROUND_COPY.as_bytes())),
                profile.path("copied").display(),
            );
            profile.cli(&[
                "session",
                "create",
                "--name",
                "copier",
                "--repo-path",
                profile.path("repo").to_str().expect("utf-8 path"),
                "--command",
                "sh",
                "--arg",
                "-c",
                "--arg",
                &script,
            ]);
        },
        plain_shell,
    )?;
    // Which row is selected at start is the list's business; this test needs
    // `probe` in front, and says so.
    profile.cli(&["session", "focus", "probe"]);
    tui.send(b"echo tb-probe-\"\"focused\r");
    tui.wait_for("tb-probe-focused");
    assert!(
        !tui.frame().contains("tb-copier-ready"),
        "the copier must be off screen"
    );
    Some((profile, tui))
}

/// Let the copier copy, and wait until it has written the sequence to its
/// pane.
fn let_the_copier_copy(profile: &Profile, tui: &Tui) {
    std::fs::write(profile.path("copy-now"), "").expect("touch trigger");
    let deadline = Instant::now() + WAIT;
    while !profile.path("copied").exists() {
        if Instant::now() > deadline {
            tui.give_up("the copier to print its copy");
        }
        std::thread::sleep(Duration::from_millis(40));
    }
}

#[test]
fn a_background_sessions_osc52_copy_never_reaches_the_clipboard() {
    // The operator's rule: an app writes the clipboard only from the session
    // the user is in. An agent working off screen — a local one or one on a
    // remote host — must not replace what the user just copied, and must not
    // have its write held until the user happens to look at it.
    let Some((profile, mut tui)) = shell_session_beside_a_copier() else {
        return;
    };
    let mark = tui.raw_len();
    let_the_copier_copy(&profile, &tui);
    // Output the binary reads after the copier's, so the copier's has been
    // parsed by the time the window below closes.
    tui.send(b"echo tb-after-\"\"copy\r");
    tui.wait_for("tb-after-copy");
    let out = settled_since(&tui, mark);
    assert_eq!(
        osc52_count(&out),
        0,
        "a background copy reached the outer terminal:\n{out:?}"
    );

    // Bringing it forward must not release a write it made while hidden.
    let mark = tui.raw_len();
    profile.cli(&["session", "focus", "copier"]);
    tui.wait_for("tb-copier-ready");
    let out = settled_since(&tui, mark);
    assert_eq!(
        osc52_count(&out),
        0,
        "focusing the copier released its hidden copy:\n{out:?}"
    );

    // Now in front, it copies like any focused app — so the silence above
    // was the focus rule and not a pane whose output was never read.
    tui.wait_until_quiet();
    let mark = tui.raw_len();
    let line = osc52_printf("c", &b64(b"tb-copier-focused"));
    tui.send(format!("{line}\r").as_bytes());
    tui.wait_until("the focused copier's copy", |_| {
        tui.raw_since(mark).contains(OSC52)
    });
    assert_eq!(
        osc52_payload(&tui.raw_since(mark)).as_deref(),
        Some("tb-copier-focused")
    );

    let status = tui.quit();
    assert!(status.success(), "exit must be clean: {status:?}");
}

#[test]
fn an_app_cannot_read_back_another_sessions_copy() {
    // An OSC 52 `?` asks the terminal for the clipboard. Under `set-clipboard
    // on`, tmux answers it from its newest paste buffer, which that setting
    // fills with every app's copy on the server: an app in one session could
    // read what an app in another had copied. No reply at all is the answer
    // every peer gives — whatever buffers the server holds, so one is put
    // there by hand as well.
    let Some((profile, mut tui)) = shell_session_beside_a_copier() else {
        return;
    };
    let_the_copier_copy(&profile, &tui);
    let seeded = profile.server.tmux(&["set-buffer", "tb-buffered-secret"]);
    assert!(seeded.status.success(), "seed a paste buffer");

    // `min 0 time 10`: a read returns whatever came within a second, or end of
    // file, so `cat` ends by itself — and stays in the foreground, where it can
    // read the terminal at all.
    let reply = profile.path("reply.bin");
    let reader = profile.path("read-clipboard.sh");
    std::fs::write(
        &reader,
        format!(
            "stty raw -echo min 0 time 10\nprintf '\\033]52;c;?\\007'\ncat > '{}'\nstty sane\n",
            reply.display()
        ),
    )
    .expect("write reader");
    tui.send(format!("sh '{}'; echo tb-read-\"\"done\r", reader.display()).as_bytes());
    tui.wait_for("tb-read-done");
    let answered = std::fs::read(&reply).expect("read reply");
    assert!(
        answered.is_empty(),
        "an app was answered a clipboard read: {:?}",
        String::from_utf8_lossy(&answered)
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

#[test]
fn a_single_click_selects_a_session_row_and_a_double_click_opens_it() {
    // Pointing at a session and opening it are two gestures. A single click
    // selects the row and leaves the keyboard in the column, so Ctrl+D and the
    // other list chords act on the session just pointed at; a double-click is
    // Enter, and hands focus to the agent pane. The whole road is asserted
    // here — SGR reports in, `ClickTrain` counting the two presses, the pane
    // reading `hit.clicks` — because a wire that dropped the count anywhere
    // along it would leave every in-process test green and open on one click.
    let Some((_profile, mut tui)) = shell_session_prepared(
        |profile| {
            let repo = profile.root.path().join("repo");
            profile.cli(&[
                "session",
                "create",
                "--name",
                "second",
                "--repo-path",
                repo.to_str().expect("utf-8 path"),
                "--agent",
                "shell",
            ]);
        },
        |_| {},
    ) else {
        return;
    };
    let badge_reads = |frame: &str, pane: &str| {
        frame
            .lines()
            .last()
            .is_some_and(|band| band.trim_start().starts_with(pane))
    };

    // 0x08 is Ctrl+H, the kernel's focus-cycle chord.
    tui.send(b"\x08");
    tui.wait_until("the sessions pane to be the focused one", |frame| {
        badge_reads(frame, "Sessions")
    });

    // "second" is on screen only as its row: the chrome and the agent pane's
    // title both name the selected session, which is still "probe".
    let at = tui.find("second");
    tui.press(0, at);
    tui.wait_until("the click to select the second session", |frame| {
        frame.contains("second (shell)")
    });
    tui.wait_until_quiet();
    assert!(
        badge_reads(&tui.frame(), "Sessions"),
        "a single click must leave the keyboard in the column:\n{}",
        tui.frame()
    );

    // Now "probe" is the row that is NOT selected, and its row is the only
    // "probe" on screen. The first press selects it, and the second is held
    // back until the repaint has marked the row selected — the case a
    // double-click exists for, and the one a count keyed on anything but the
    // node's id reads as two singles. Two presses sent back to back land in
    // one frame and would miss that transition. The wait is budgeted well
    // inside the 400 ms window, so a slow repaint fails here, by name, rather
    // than as a double-click that did not open.
    let at = tui.find("probe");
    tui.press(0, at);
    tui.wait_within(
        Duration::from_millis(300),
        "the first press to select probe and repaint its row",
        |frame| frame.contains("probe (shell)"),
    );
    tui.press(0, at);
    tui.wait_until(
        "the double-click to hand focus to the agent pane",
        |frame| badge_reads(frame, "Agent"),
    );

    let status = tui.quit();
    assert!(status.success(), "exit must be clean: {status:?}");
}

/// A press of any button is a press. Two quick left clicks on a row with a
/// middle press between them are not a double-click, however fast: the
/// gesture was interrupted, and the row must only be selected, not opened.
#[test]
fn a_press_of_another_button_between_two_clicks_keeps_them_two_clicks() {
    let Some((_profile, mut tui)) = shell_session_prepared(
        |profile| {
            let repo = profile.root.path().join("repo");
            profile.cli(&[
                "session",
                "create",
                "--name",
                "second",
                "--repo-path",
                repo.to_str().expect("utf-8 path"),
                "--agent",
                "shell",
            ]);
        },
        |_| {},
    ) else {
        return;
    };
    let badge_reads = |frame: &str, pane: &str| {
        frame
            .lines()
            .last()
            .is_some_and(|band| band.trim_start().starts_with(pane))
    };

    tui.send(b"\x08");
    tui.wait_until("the sessions pane to be the focused one", |frame| {
        badge_reads(frame, "Sessions")
    });
    // Select "second" so that "probe" is the row that is not selected, and
    // the only "probe" on screen.
    tui.press(0, tui.find("second"));
    tui.wait_until("the click to select the second session", |frame| {
        frame.contains("second (shell)")
    });
    tui.wait_until_quiet();

    // Left, middle (button 1), left — back to back, well inside the window.
    let at = tui.find("probe");
    tui.press(0, at);
    tui.press(1, at);
    tui.press(0, at);
    tui.wait_until("the presses to select probe", |frame| {
        frame.contains("probe (shell)")
    });
    tui.wait_until_quiet();
    assert!(
        badge_reads(&tui.frame(), "Sessions"),
        "an interrupted pair of clicks must not open the session:\n{}",
        tui.frame()
    );

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

#[test]
fn the_wheel_scrolls_the_companion_shell_too() {
    // The shell is a second surface over the same primitive, and it was the
    // half that never honoured a scroll offset: the pane refused to hold one
    // for it and the kernel never set it on the shell's parser, so the wheel
    // over an open shell moved nothing.
    let Some((_profile, mut tui)) = shell_session() else {
        return;
    };

    // Ctrl+T opens the companion shell in the same pane. The focus badge names
    // the view, so it is what tells us the shell is the one on screen.
    tui.send(b"\x14");
    tui.wait_until("the shell tab to be the view", |frame| {
        frame
            .lines()
            .last()
            .is_some_and(|band| band.trim_start().starts_with("Shell"))
    });
    // The pane paints before the shell inside it has drawn a prompt, and a
    // keystroke sent in between is lost.
    tui.wait_until_quiet();
    bury_a_marker(&mut tui, "tb-shell-marker");

    let at = tui.find("tb-fill-100");
    tui.wheel(at, true, 90);
    tui.wait_for("tb-shell-marker");

    let status = tui.quit();
    assert!(status.success(), "exit must be clean: {status:?}");
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
    tui.wait_for("Multiplexer");
    tui.send(b"\r");
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
    let at = tui.find("no status hooks");
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
        let rows = self.screen.lock().unwrap().screen().size().0;
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

#[test]
fn the_scrollbar_can_be_pressed_and_dragged() {
    // The bar was drawn and could not be touched: the border column carried no
    // identity, so a press on it armed a text selection, and a drag only ever
    // meant "extend the selection" — there was no route from the pointer to the
    // pane that owns the offset.
    //
    // Driven on the SHELL tab, which is where it was reported and the harder of
    // the two: the shell is a second surface over the same primitive.
    let Some((_profile, mut tui)) = shell_session() else {
        return;
    };
    tui.send(b"\x14");
    tui.wait_until("the shell tab to be the view", |frame| {
        frame
            .lines()
            .last()
            .is_some_and(|band| band.trim_start().starts_with("Shell"))
    });
    tui.wait_until_quiet();
    bury_a_marker(&mut tui, "tb-bar-marker");

    // A wheel scroll is what gives the bar a depth to be scaled against, and
    // leaves the thumb at the top of its track.
    let at = tui.find("tb-fill-100");
    tui.wheel(at, true, 90);
    tui.wait_for("tb-bar-marker");
    let (column, _) = tui.find("█");
    let (top, bottom) = tui.track_extent(column);

    // Drag the thumb down the track: that is a return to the live bottom of the
    // stream, the same place the wheel would have brought us back to.
    tui.drag_down((column, top + 1), bottom - 1);
    tui.wait_for("tb-fill-100");
    tui.wait_gone("tb-bar-marker");

    // And a press on the track alone is a jump, with no drag behind it.
    tui.drag_down((column, top + 1), top + 1);
    tui.wait_for("tb-bar-marker");

    let status = tui.quit();
    assert!(status.success(), "exit must be clean: {status:?}");
}

// --- the WSL image probe, through the real binary ---------------------------

/// A stand-in for `powershell.exe` that records every call and answers `answer`.
///
/// The probe resolves PowerShell through `PATH` (`clipboard::POWERSHELL`), so a
/// directory in front of the real one is the whole trick — no Windows, and no
/// need for a real clipboard to be in any particular state.
fn stub_powershell(dir: &Path, answer: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let bin = dir.join("winbin");
    std::fs::create_dir_all(&bin).expect("mkdir winbin");
    let script = bin.join("powershell.exe");
    std::fs::write(
        &script,
        format!("#!/bin/sh\necho call >> \"$(dirname \"$0\")/asked\"\necho {answer}\n"),
    )
    .expect("write the stub");
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).expect("chmod");
    bin
}

/// How many times the stub has been asked.
fn asked(bin: &Path) -> usize {
    std::fs::read_to_string(bin.join("asked"))
        .map(|log| log.lines().count())
        .unwrap_or(0)
}

/// Waits for the stub to have been asked `n` times, and says so if it never is.
fn wait_for_asks(bin: &Path, n: usize, tui: &Tui) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while asked(bin) < n && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    assert_eq!(
        asked(bin),
        n,
        "Windows was asked {} times, not {n}\n--- frame ---\n{}",
        asked(bin),
        tui.frame()
    );
}

/// Inside WSL every `Ctrl+V` asks Windows — one question per press, and none at
/// all while a float is holding the keyboard.
///
/// The whole route, through the real binary: the press is claimed by the
/// clipboard stage, the question goes out on a worker, the answer is polled
/// back on the loop and spent, and only then can the next question be asked.
/// Nothing below the binary is stubbed except Windows itself — a directory in
/// front of `PATH` holding a `powershell.exe` that counts its calls.
///
/// Two failures are covered that unit tests cannot reach:
///
/// - **the answer is never polled.** Deleting `poll_image_probe` from the loop
///   leaves every unit test green, because `ImageProbe` is perfectly happy
///   never to be asked again — but the interface then swallows every paste for
///   the rest of the session. Here the second press has to reach Windows, which
///   it can only do after the first answer was taken.
/// - **a float's paste is swallowed.** The clipboard stage runs *before*
///   `dispatch_grabbed`, so asking Windows while the new-session wizard is up
///   claims a press that the float should have had — and the answer, arriving
///   0.42 s later, finds the float still there and drops it. Pasting a path
///   into the wizard did nothing at all under WSL.
#[test]
fn a_paste_under_wsl_asks_windows_once_per_press_and_never_from_a_float() {
    let windows = tempfile::tempdir().expect("tempdir");
    // "image" so the answer is spent on forwarding the chord to the shell,
    // which leaves the message band clean for the float's own report below.
    let bin = stub_powershell(windows.path(), "image");
    let path = format!(
        "{}:{}",
        bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let Some((_profile, mut tui)) = shell_session_with(|cmd| {
        cmd.env("WSL_DISTRO_NAME", "Ubuntu");
        cmd.env("PATH", path);
        // No X clipboard, so the float below reports the one thing it can
        // report rather than pasting whatever this machine happens to hold.
        cmd.env_remove("DISPLAY");
        cmd.env_remove("WAYLAND_DISPLAY");
    }) else {
        return;
    };

    const CTRL_V: &[u8] = &[0x16];
    const CTRL_N: &[u8] = &[0x0e];

    tui.send(CTRL_V);
    wait_for_asks(&bin, 1, &tui);
    // The second press is the assertion: it can only reach Windows if the first
    // answer came back, was polled on the loop, and freed the question.
    tui.send(CTRL_V);
    wait_for_asks(&bin, 2, &tui);

    // Now the wizard, which floats and therefore holds the keyboard.
    tui.send(CTRL_N);
    // Its first question is "Run On" where the machine has hosts and
    // "Multiplexer" where it has none — this one has sibling WSL distros, so which it
    // is depends on the machine and neither is the point.
    tui.wait_until("the new-session wizard to be up", |frame| {
        frame.contains("Run On") || frame.contains("Multiplexer")
    });
    tui.send(CTRL_V);
    // It has nothing to paste from — that is what the missing X clipboard
    // buys — and saying so is proof the press was handled here rather than
    // spent on a question the float could never use the answer to.
    tui.wait_for("No local clipboard");
    assert_eq!(
        asked(&bin),
        2,
        "a press made into a float asked Windows about an image a name field \
         could not take"
    );

    let status = tui.quit();
    assert!(status.success(), "exit must be clean: {status:?}");
}

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

/// The remote session's name. Long enough that a narrow terminal cannot show
/// it, which is what lets a test tell a repaint from leftover glyphs.
const REMOTE_NAME: &str = "afar-on-bad-link";

/// A stand-in for `ssh` that runs the "remote" command on this machine.
///
/// The point is not to imitate a network. It is to reproduce the one thing a
/// real `ssh` puts between thurbox and the multiplexer: **a process that copies
/// the bytes**, which a test can then stop.
///
/// That relay has to be built rather than inherited, because a local tmux has
/// none. `tmux -C attach-session` hands its stdin and stdout *file descriptors*
/// to the tmux server and then only shepherds; the server does the I/O on those
/// inherited fds. So stopping the client changes nothing — it is not on the
/// data path — while stopping `ssh` wedges the link exactly as a bad network
/// does. The control connection therefore runs through a pair of `cat` pumps
/// over fifos, one per direction, and those are the pids recorded.
///
/// Every *other* call `exec`s straight through: they are short round trips
/// whose **exit status is load-bearing** (`has-session` answering "no" is how
/// `ensure_ready` decides to create a session, and the git probes read theirs),
/// and only the long-lived connection ever needs to be wedged. Real ssh joins
/// the command words with spaces and hands them to the host's login shell,
/// which is what `eval` reads them as here — the same re-splitting
/// `posix_quote` is written against.
fn fake_ssh(profile: &Profile) -> Link {
    use std::os::unix::fs::PermissionsExt;
    let pids = profile.root.path().join("ssh-pids");
    let script = profile.bin.join("ssh");
    std::fs::write(
        &script,
        format!(
            "#!/bin/sh\n\
             while [ \"$#\" -gt 0 ]; do\n\
             \x20 case \"$1\" in\n\
             \x20   -o) shift; shift ;;\n\
             \x20   -*) shift ;;\n\
             \x20   *) break ;;\n\
             \x20 esac\n\
             done\n\
             [ \"$#\" -gt 0 ] && shift\n\
             [ \"$#\" -eq 0 ] && exit 0\n\
             printf '%s\\n' \"$$\" >> {pids}\n\
             case \" $* \" in\n\
             \x20 *\" -C attach-session \"*)\n\
             \x20   d=$(mktemp -d {root}/link.XXXXXX) || exit 1\n\
             \x20   mkfifo \"$d/up\" \"$d/down\" || exit 1\n\
             \x20   eval \"$* \" < \"$d/up\" > \"$d/down\" &\n\
             \x20   cat < \"$d/down\" &\n\
             \x20   printf '%s\\n' \"$!\" >> {pids}\n\
             \x20   exec cat > \"$d/up\"\n\
             \x20   ;;\n\
             esac\n\
             eval \"exec $*\"\n",
            pids = shell_word(&pids),
            root = shell_word(profile.root.path()),
        ),
    )
    .expect("write the ssh stand-in");
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).expect("chmod");
    Link { pids }
}

/// A path as one shell word. The profile root is a tempdir, so it is ordinary —
/// but a `TMPDIR` with a space in it would otherwise split the redirect.
fn shell_word(path: &Path) -> String {
    format!("'{}'", path.display().to_string().replace('\'', "'\\''"))
}

/// The link the stand-in carries, and the switch that takes it away.
struct Link {
    pids: PathBuf,
}

impl Link {
    /// Every process the stand-in recorded that is still running — in practice
    /// the control connection's two pumps, the short calls having exited.
    fn live(&self) -> Vec<libc::pid_t> {
        std::fs::read_to_string(&self.pids)
            .unwrap_or_default()
            .lines()
            .filter_map(|line| line.trim().parse::<libc::pid_t>().ok())
            // SAFETY: signal 0 delivers nothing; it only reports whether the
            // process exists.
            .filter(|pid| unsafe { libc::kill(*pid, 0) } == 0)
            .collect()
    }

    fn signal(&self, sig: libc::c_int) -> usize {
        let live = self.live();
        for pid in &live {
            // SAFETY: a pid this process started, and a plain signal number.
            unsafe { libc::kill(*pid, sig) };
        }
        live.len()
    }

    /// Wedge the link: up, and carrying nothing.
    ///
    /// Stopped pumps carry nothing in either direction while every pipe stays
    /// open, which is what a link that has gone bad looks like from this end —
    /// and is the case no timeout in the ssh option set reaches, because the
    /// connection never fails, it just stops working. Killing them instead
    /// would exercise the broken-pipe path, which already works.
    fn wedge(&self) {
        assert_eq!(
            self.signal(libc::SIGSTOP),
            2,
            "a wedge needs both of the control connection's pumps; the session \
             cannot have attached over the stand-in"
        );
    }

    fn heal(&self) {
        self.signal(libc::SIGCONT);
    }
}

impl Drop for Link {
    /// A wedged process would otherwise outlive a panicking test — it cannot
    /// even act on the `kill-server` the profile's own drop sends.
    fn drop(&mut self) {
        self.heal();
    }
}

/// A profile with one `sh` session on a *remote* host, attached and painted.
///
/// The host is this machine reached through `fake_ssh`, so everything below
/// the launcher is real: the ssh `HostLauncher` behind `TmuxTransport`, the POSIX quoting, the
/// control-mode protocol, the attach worker. `share_sessions = false` because
/// the host's database would be this database (the ADR-24 loopback), and the
/// socket is named outright for the same reason the profile names it — a
/// default would put the "remote" server on the developer's own.
fn remote_shell_session() -> Option<(Profile, Link, Tui)> {
    if !have_tmux() {
        eprintln!("skipping: tmux is not installed");
        return None;
    }
    let profile = Profile::new();
    let link = fake_ssh(&profile);
    std::fs::write(
        profile.path("config/agents.toml"),
        "default = \"shell\"\n\n[[agents]]\nname = \"shell\"\ncommand = \"sh\"\nargs = []\n",
    )
    .expect("seed agents");
    std::fs::write(
        profile.path("config/hosts.toml"),
        format!(
            "[[hosts]]\n\
             name = \"devbox\"\n\
             destination = \"e2e@localhost\"\n\
             socket = \"{socket}\"\n\
             share_sessions = false\n\
             worktrees_dir = \"{worktrees}\"\n",
            socket = profile.server.socket(),
            worktrees = profile.path("worktrees").display(),
        ),
    )
    .expect("seed hosts");
    let repo = repo(profile.root.path());

    profile.cli(&[
        "session",
        "create",
        "--name",
        REMOTE_NAME,
        "--repo-path",
        repo.to_str().expect("utf-8 path"),
        "--agent",
        "shell",
        "--host",
        "devbox",
    ]);
    profile.cli(&["config", "accept-interface"]);

    let tui = Tui::spawn(&profile, 40, 120);
    tui.wait_for(REMOTE_NAME);
    tui.wait_until("the agent pane to be the focused one", |frame| {
        frame
            .lines()
            .last()
            .is_some_and(|band| band.trim_start().starts_with("Agent"))
    });
    tui.wait_for("$ ");
    Some((profile, link, tui))
}

#[test]
fn a_chord_is_answered_while_a_remote_sessions_link_is_wedged() {
    // The interface must stay the user's while the network is not.
    //
    // A *passthrough* chord is the one that goes over the wire. Before it can
    // be left to the agent, `coordinator::input`'s gate asks whether the
    // focused pane is dead — `focused_terminal_is_dead` -> `Terminals::is_dead`
    // -> `tmux_compat::Server::is_dead`, a control-mode round trip made on the loop
    // itself. With the link wedged that runs out `COMMAND_TIMEOUT`, reconnects,
    // and runs out again, and nothing else is handled meanwhile.
    //
    // So `ctrl+e` is the press under test and `ctrl+p` is the assertion: the
    // palette is drawn entirely from the kernel's own registry and needs
    // nothing from the host, so it can only be late if the press before it
    // stopped the loop. Two presses rather than one because a live pane's
    // passthrough chord has no visible outcome of its own — it is forwarded,
    // which is the whole point of it.
    //
    // The answer is already here, without asking: the pane's reader thread sets
    // `WiredPane::exited` on EOF, and `has_exited` reads it with an atomic load.
    let Some((_profile, link, mut tui)) = remote_shell_session() else {
        return;
    };

    link.wedge();

    tui.send(CTRL_E);
    tui.send(CTRL_P);
    tui.wait_within(
        RESPONSIVE,
        "the palette to open on a wedged link",
        |frame| frame.contains("type to filter commands"),
    );

    // Healed before the exit: quitting detaches every backend, and a wedged one
    // would hold that up for reasons this test has already made its point about.
    link.heal();
    tui.send(ESC);
    tui.wait_gone("type to filter commands");
    assert!(tui.quit().success());
}

#[test]
fn a_resize_is_not_paid_for_on_the_render_thread_when_the_link_is_wedged() {
    // The other half, on the thread that owns the screen. `render_session`
    // matches the pane to the rect it is painted into, and `Session::resize`
    // does that by asking the backend — a control-mode round trip, mid-frame.
    // The placeholder branch right above it already refuses to (*"would issue a
    // blocking ssh resize on the UI thread — the freeze we're avoiding"*); the
    // live branch is the one this pins.
    //
    // Nothing on screen needs that answer: the pane is resized so the *agent*
    // wraps correctly, which is a message to the host, not an input to the
    // frame. It belongs on the queue the keystrokes already go out on.
    //
    // Same shape as the chord test, and for the same reason — the palette is
    // the only thing asserted on, because it is drawn from the kernel's own
    // registry and owes the host nothing. The session list is deliberately not
    // used: it comes back on a snapshot tick, which is seconds even on a
    // healthy link, so it cannot tell a frozen interface from a patient one.
    let Some((_profile, link, mut tui)) = remote_shell_session() else {
        return;
    };

    link.wedge();

    // The rect changes, so the next frame re-sizes the pane behind it — and
    // that frame is the assertion. Narrowing to 100 columns cuts the header's
    // right-hand end out of the grid, so `Default` can only be back once the
    // interface has painted a whole frame at the new width. Blocked mid-paint,
    // it never does.
    //
    // Asserted on the repaint rather than on a chord sent after it: a press
    // made before the reflow lands can be refused (focus may only rest on a
    // slot the last painted frame placed, which is what
    // `a_press_right_after_a_reload_never_reaches_a_pane_that_did_not_paint_it`
    // pins), so pressing here tested the race and not the resize.
    tui.resize(30, 100);
    tui.wait_within(
        RESPONSIVE,
        "the interface to repaint at the new width on a wedged link",
        |frame| frame.contains("Default"),
    );

    link.heal();
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

/// A stand-in agent that echoes each key it reads as `[k]`, redrawn in place,
/// a few milliseconds after reading it.
///
/// The delay is what makes the echo scenarios mean something. An echo that
/// arrives before the interface has started painting the keystroke's own frame
/// rides in that frame for free, however the loop paces output; one that
/// arrives *after* it — which is the case for any agent slower than a frame to
/// answer, real agents included — used to be paced by the output floor.
const DELAYED_ECHO: &str =
    "printf 'ready> '; while IFS= read -rs -n1 c; do sleep 0.005; printf '\\r[%s]' \"$c\"; done";

/// Type `keys` letters into the focused [`DELAYED_ECHO`] session at a person's
/// pace, each one only after the last one's echo is on screen.
fn type_and_see_echoes(tui: &mut Tui, keys: usize) {
    for i in 0..keys {
        let key = b'a' + (i % 26) as u8;
        let token = format!("[{}]", key as char);
        tui.send(&[key]);
        tui.wait_within(
            Duration::from_secs(5),
            &format!("the echo {token}"),
            |frame| frame.contains(&token),
        );
        // 60–100 ms between keys, varied so the keys cannot lock onto the
        // loop's own clock — the benchmark's typing rate.
        std::thread::sleep(Duration::from_millis(60 + (i as u64 * 37) % 41));
    }
}

/// The loop's `(echoes, echo_frames)` counters, once its published perf
/// snapshot has counted at least `at_least` echoes — or as they stand when it
/// gives up. Published every few seconds while `THURBOX_PERF_LOG` is set.
fn echo_counters(profile: &Profile, at_least: u64) -> (u64, u64) {
    let deadline = Instant::now() + WAIT;
    let mut seen = (0, 0);
    while Instant::now() < deadline {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_thurbox-cli"));
        profile.apply(&mut cmd);
        let out = cmd
            .args(["perf", "--json"])
            .output()
            .expect("thurbox-cli perf");
        if let Ok(json) = serde_json::from_slice::<serde_json::Value>(&out.stdout) {
            let counter = |name: &str| json["counters"][name].as_u64().unwrap_or(0);
            seen = (counter("echoes"), counter("echo_frames"));
            if seen.0 >= at_least {
                break;
            }
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    seen
}

/// A profile with a session named `echo` running `command` (plus whatever
/// `extra` creates after it), attached and focused, with the loop counting.
fn echo_session(command: &str, extra: impl FnOnce(&Profile, &Path)) -> Option<(Profile, Tui)> {
    if !have_tmux() {
        eprintln!("skipping: tmux is not installed");
        return None;
    }
    let profile = Profile::new();
    let repo = repo(profile.root.path());
    let repo_path = repo.to_str().expect("utf-8 path");
    profile.cli(&[
        "session",
        "create",
        "--name",
        "echo",
        "--repo-path",
        repo_path,
        "--command",
        "bash",
        "--arg",
        "-c",
        "--arg",
        command,
    ]);
    // After, so `echo` is the first row, which is the one selected at boot.
    extra(&profile, &repo);
    profile.cli(&["config", "accept-interface"]);
    let tui = Tui::spawn_with(&profile, 40, 120, |cmd| {
        cmd.env("THURBOX_PERF_LOG", "1");
    });
    tui.wait_for("ready> ");
    tui.wait_until("the agent pane to be the focused one", |frame| {
        frame
            .lines()
            .last()
            .is_some_and(|band| band.trim_start().starts_with("Agent"))
    });
    Some((profile, tui))
}

/// Keys typed in the two scenarios below.
const ECHO_KEYS: u64 = 20;

/// The most floor-free paints one key can buy: its first answer, then
/// `ECHO_TAIL_FRAMES` (4) more of its pane's output.
const ECHO_PAINTS_PER_KEY: u64 = 5;

/// What both scenarios assert: every key's echo was painted with no frame
/// floor, and all but the first as a frame that redrew only the pane — the
/// first key after a pause has no kept frame to redraw over (see
/// `KEEP_FRAME_WHILE_TYPING`).
///
/// Asserted on the loop's counters rather than on the clock (ADR-P5): the
/// benchmark measures how long an echo takes (docs/BENCHMARK-MULTIPLEXERS.md),
/// and this pins that nothing puts it back on a floor. A range, not a count: an
/// echo that reaches the interface as two chunks earns its second a floor-free
/// frame too (`ECHO_TAIL_FRAMES`), and how the pty splits a write is not the
/// test's to decide.
fn assert_every_echo_painted_at_once(profile: &Profile) {
    let (echoes, echo_frames) = echo_counters(profile, ECHO_KEYS);
    assert!(
        (ECHO_KEYS..=ECHO_KEYS * ECHO_PAINTS_PER_KEY).contains(&echoes),
        "{echoes} floor-free paints for {ECHO_KEYS} keys: every keystroke's echo is painted with \
         no floor ({echo_frames} of them as echo frames)"
    );
    assert!(
        echo_frames >= ECHO_KEYS - 1,
        "{echo_frames} of {echoes} echoes were painted by redrawing only the pane"
    );
}

#[test]
fn a_keystrokes_echo_is_painted_without_waiting_for_the_output_floor() {
    // Output is paced at 30 frames a second (ADR-P17) because nobody reads a
    // scrolling log faster. The echo of a key is output too, and pacing it
    // put 25–48 ms on every keystroke (docs/BENCHMARK-MULTIPLEXERS.md): the
    // keystroke's own frame painted at once, the echo landed just after it and
    // then waited out the floor from that frame (ADR-P28).
    // After the 20 ordinary keys, answer the first key of a two-key batch at
    // once and the second 75 ms later. That keeps the two output chunks well
    // inside the echo window but far enough apart that tmux cannot coalesce
    // them into one read.
    const DELAY_SECOND_BATCH_ECHO: &str = "printf 'ready> '; i=0; while IFS= read -rs -n1 c; do i=$((i + 1)); if [ \"$i\" -le 20 ]; then sleep 0.005; elif [ \"$i\" -eq 22 ]; then sleep 0.075; fi; printf '\\r[%s]' \"$c\"; done";
    let Some((profile, mut tui)) = echo_session(DELAY_SECOND_BATCH_ECHO, |_, _| {}) else {
        return;
    };
    type_and_see_echoes(&mut tui, ECHO_KEYS as usize);
    assert_every_echo_painted_at_once(&profile);

    // Crossterm can hand one input poll several rapid or repeated keys. The
    // agent answers them one at a time, so each send must keep its own wait
    // instead of replacing the wait the previous key left behind.
    const BATCHED_KEYS: u64 = 2;
    let keys: Vec<u8> = (0..BATCHED_KEYS).map(|i| b'a' + (i % 26) as u8).collect();
    let last = format!("[{}]", keys.last().copied().expect("a key") as char);
    tui.send(&keys);
    tui.wait_within(Duration::from_secs(5), "the last batched echo", |frame| {
        frame.contains(&last)
    });
    let expected = ECHO_KEYS + BATCHED_KEYS;
    let (echoes, echo_frames) = echo_counters(&profile, expected);
    assert!(
        (expected..=expected * ECHO_PAINTS_PER_KEY).contains(&echoes),
        "{echoes} floor-free paints for {expected} keys: every batched key retained its echo wait"
    );
    // Reading the first counter snapshot can outlive KEEP_FRAME_WHILE_TYPING,
    // so the batch's first echo may need a full frame; its second must reuse
    // that frame.
    assert!(
        echo_frames >= expected - 2,
        "{echo_frames} of {echoes} batched echoes redrew only the pane"
    );
    assert!(tui.quit().success());
}

#[test]
fn a_keystrokes_echo_is_painted_at_once_while_another_session_prints() {
    // The same, with a second session printing the whole time. Its output
    // keeps the loop painting at the output floor, which is exactly the clock
    // an echo must not be put on — and it must not be counted as an echo.
    let Some((profile, mut tui)) = echo_session(DELAYED_ECHO, |profile, repo| {
        profile.cli(&[
            "session",
            "create",
            "--name",
            "busy",
            "--repo-path",
            repo.to_str().expect("utf-8 path"),
            "--command",
            "bash",
            "--arg",
            "-c",
            "--arg",
            "while :; do echo \"busy $RANDOM $RANDOM $RANDOM\"; sleep 0.002; done",
        ]);
    }) else {
        return;
    };
    type_and_see_echoes(&mut tui, ECHO_KEYS as usize);
    assert_every_echo_painted_at_once(&profile);
    assert!(tui.quit().success());
}

/// A stand-in for an agent that redraws the way Codex does: on each key, a
/// synchronized frame that only moves the cursor, then 20 ms later the glyph,
/// as three separate writes inside a second synchronized update.
///
/// tmux forwards each write as its own output line, so the cursor-only frame is
/// the first output after the key and the glyph is not. The 15 ms before the
/// glyph is longer than the loop's input poll, which pins the race that stalled
/// real keys: a frame is painted between the chunks, and a glyph not treated as
/// the key's answer then waits out the output floor from that frame.
const SPLIT_ECHO: &str = "printf 'ready> '; while IFS= read -rs -n1 c; do \
     printf '\\e[?2026h\\e[9G\\e[?25h\\e[?2026l'; sleep 0.02; \
     printf '\\e[?2026h\\e[?25l'; sleep 0.015; \
     printf '\\r[%s]' \"$c\"; sleep 0.002; \
     printf '\\e[?25h\\e[?2026l'; done";

#[test]
fn a_glyph_that_follows_a_cursor_only_frame_is_painted_as_the_keys_echo() {
    // Treating the first chunk after a key as the whole echo spent the key's
    // floor-free frame on a frame that showed nothing, and the glyph was
    // painted on the output floor: 8–35 % of Codex keys took ~65 ms under
    // thurbox against ~25 ms under raw tmux. Each key's pane stays owed until
    // the echo window closes, so the glyph gets a floor-free frame of its own.
    let Some((profile, mut tui)) = echo_session(SPLIT_ECHO, |_, _| {}) else {
        return;
    };
    type_and_see_echoes(&mut tui, ECHO_KEYS as usize);
    // At least the cursor-only frame and the glyph for every key. Without the
    // fix this can never exceed one per key, whatever the timing.
    let (echoes, echo_frames) = echo_counters(&profile, 2 * ECHO_KEYS);
    assert!(
        echoes >= 2 * ECHO_KEYS,
        "{echoes} floor-free echo paints for {ECHO_KEYS} keys: the glyph after a \
         cursor-only frame was left to the output floor ({echo_frames} echo frames)"
    );
    assert!(tui.quit().success());
}

#[test]
fn a_pane_printing_while_you_type_into_it_is_not_painted_per_chunk() {
    // The pane typed into rewrites a status line every couple of
    // milliseconds. Its output after each key is owed floor-free frames, but
    // only a few per key (`ECHO_TAIL_FRAMES`, 4): per chunk, it would be
    // painted at the poll rate for the whole echo window.
    const TYPED_AND_BUSY: &str = "printf 'ready> '; while :; do \
         if IFS= read -rs -n1 -t 0.002 c; then printf '\\r[%s]' \"$c\"; fi; \
         printf '\\e[3;1Hbusy %05d\\e[1;8H' $RANDOM; done";
    let Some((profile, mut tui)) = echo_session(TYPED_AND_BUSY, |_, _| {}) else {
        return;
    };
    type_and_see_echoes(&mut tui, ECHO_KEYS as usize);
    // A snapshot published after the last key: they come every five seconds.
    std::thread::sleep(Duration::from_secs(6));
    let (echoes, _) = echo_counters(&profile, ECHO_KEYS);
    assert!(
        (ECHO_KEYS..=ECHO_KEYS * ECHO_PAINTS_PER_KEY).contains(&echoes),
        "{echoes} floor-free paints for {ECHO_KEYS} keys into a printing pane \
         (bounded at 1 + 4 a key)"
    );
    assert!(tui.quit().success());
}

#[test]
fn creating_a_session_runs_a_handful_of_tmux_processes() {
    // `session create` re-applies the server's options on every call, and did
    // it as one `tmux set-option` process each: 27 processes and ~92 ms a
    // session, most of the gap to Herdr and raw tmux (#1243). What is counted
    // here is the processes, not the milliseconds — a count is the same on
    // every machine.
    if !have_tmux() {
        eprintln!("skipping: tmux is not installed");
        return;
    }
    let profile = Profile::new();
    let repo = repo(profile.root.path());
    let real = String::from_utf8(
        Command::new("sh")
            .args(["-c", "command -v tmux"])
            .output()
            .expect("find tmux")
            .stdout,
    )
    .expect("utf-8 path");
    let log = profile.path("tmux-calls.log");
    let shim = profile.bin.join("tmux");
    std::fs::write(
        &shim,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\nexec '{}' \"$@\"\n",
            log.display(),
            real.trim()
        ),
    )
    .expect("write tmux shim");
    let mut perms = std::fs::metadata(&shim).expect("shim").permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o755);
    std::fs::set_permissions(&shim, perms).expect("chmod shim");

    let create = |name: &str| {
        profile.cli(&[
            "session",
            "create",
            "--name",
            name,
            "--repo-path",
            repo.to_str().expect("utf-8 path"),
            "--command",
            "sleep",
            "--arg",
            "600",
        ]);
    };
    // The first one starts the server, which is a cost paid once.
    create("first");
    std::fs::write(&log, "").expect("reset log");
    create("second");

    let calls = std::fs::read_to_string(&log).expect("read log");
    let count = calls.lines().count();
    assert!(
        count <= 3,
        "one `session create` on a running server ran {count} tmux processes (budget 3: \
         configure, create, check for a duplicate):\n{calls}"
    );
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

/// A session whose agent is `script`, run by `sh` — an agent-like pane that
/// prints a transcript, then waits for the test to touch `<profile>/redraw`
/// before changing what it shows, the way a working agent prints or a TUI
/// repaints while somebody is looking at a search result in it.
fn scripted_agent_session(script: impl FnOnce(&Path) -> String) -> Option<(Profile, Tui)> {
    if !have_tmux() {
        eprintln!("skipping: tmux is not installed");
        return None;
    }
    let profile = Profile::new();
    let path = profile.root.path().join("agent.sh");
    std::fs::write(&path, script(profile.root.path())).expect("write agent script");
    std::fs::write(
        profile.path("config/agents.toml"),
        format!(
            "default = \"scripted\"\n\n[[agents]]\nname = \"scripted\"\ncommand = \"sh\"\nargs = [{:?}]\n",
            path.to_str().expect("utf-8 path")
        ),
    )
    .expect("seed agents");
    let repo = repo(profile.root.path());
    profile.cli(&[
        "session",
        "create",
        "--name",
        "probe",
        "--repo-path",
        repo.to_str().expect("utf-8 path"),
        "--agent",
        "scripted",
    ]);
    profile.cli(&["config", "accept-interface"]);
    let tui = Tui::spawn_with(&profile, 40, 120, |cmd| {
        cmd.env("NO_COLOR", "1");
    });
    tui.wait_for("probe");
    tui.wait_for("tb-ready");
    Some((profile, tui))
}

/// The tail every scripted agent ends with: wait for the test's go, run
/// `then`, and stay alive.
fn after_redraw(profile_root: &Path, then: &str) -> String {
    format!(
        "while [ ! -e '{root}/redraw' ]; do sleep 0.05; done\n{then}\nexec sleep 1000\n",
        root = profile_root.display()
    )
}

/// The agent pane's left border column: where its top corner sits beside the
/// session list's.
fn agent_border(tui: &Tui) -> u16 {
    let top = tui.row(1);
    top.chars()
        .enumerate()
        .skip(1)
        .find(|(_, c)| matches!(c, '╭' | '┏'))
        .map(|(x, _)| x as u16)
        .unwrap_or_else(|| tui.give_up("the agent pane's top border"))
}

/// The rows of the agent pane: those whose border column is drawn and whose
/// left neighbour is the session list's border — which rules out the search
/// strip's rows, which repeat the same text further down.
fn agent_rows(tui: &Tui) -> Vec<(u16, String)> {
    let x = usize::from(agent_border(tui));
    let rows = tui.screen.lock().unwrap().screen().size().0;
    (2..rows)
        .map(|y| (y, tui.row(y)))
        .filter(|(_, row)| {
            let mut chars = row.chars().skip(x - 1);
            chars.next() == Some('│') && matches!(chars.next(), Some('│' | '┃'))
        })
        .collect()
}

/// The agent pane's rows the kernel has marked — reversed from the first cell
/// inside the border.
fn marked_rows(tui: &Tui) -> Vec<u16> {
    let x = agent_border(tui) + 1;
    agent_rows(tui)
        .into_iter()
        .map(|(y, _)| y)
        .filter(|&y| tui.inverse_at(y, x))
        .collect()
}

/// Whether the agent pane shows `needle`, and the one row it marks is the row
/// `needle` is on.
fn marks_exactly(tui: &Tui, needle: &str) -> bool {
    let on = agent_rows(tui)
        .into_iter()
        .find(|(_, row)| row.contains(needle))
        .map(|(y, _)| y);
    on.is_some() && marked_rows(tui) == on.into_iter().collect::<Vec<_>>()
}

/// Wait for `marks_exactly`, failing with the rows that were marked instead.
fn wait_for_mark_on(tui: &Tui, needle: &str, what: &str) {
    let deadline = Instant::now() + WAIT;
    while Instant::now() < deadline {
        if marks_exactly(tui, needle) {
            return;
        }
        std::thread::sleep(Duration::from_millis(40));
    }
    let lines: Vec<String> = marked_rows(tui).into_iter().map(|y| tui.row(y)).collect();
    tui.give_up(&format!(
        "{what}: the row holding {needle:?} to be the one marked; marked instead: {lines:#?}"
    ));
}

#[test]
fn a_search_result_is_revealed_as_soon_as_it_is_the_selected_one() {
    // Typing a query lands the cursor on its first result, and the session
    // list already follows it — but the terminal stayed where it was until an
    // arrow key or Enter, so the "preview" showed the right session at the
    // wrong place. The selected result is revealed the moment it is selected,
    // without a key, and Enter stays what lands the focus in it.
    let Some((_profile, mut tui)) = scripted_agent_session(|_| {
        "seq 1 200; echo tb-\"\"findme; seq 1 300; echo tb-ready\nexec sleep 1000\n".into()
    }) else {
        return;
    };
    tui.wait_gone("tb-findme");
    tui.send(CTRL_SLASH);
    tui.wait_for("Search");
    tui.send(b"tb-findme");
    tui.wait_until("a result row for the scrolled-away line", |frame| {
        frame
            .lines()
            .any(|line| line.contains("probe") && line.contains('↑') && line.contains("tb-findme"))
    });
    wait_for_mark_on(&tui, "tb-findme", "the selected result, before any key");
    assert!(
        tui.frame().contains("Search"),
        "the strip must still be open"
    );

    tui.send(b"\r");
    tui.wait_until("the terminal to take focus", |frame| {
        !frame.contains("Search")
    });
    wait_for_mark_on(&tui, "tb-findme", "the opened result");
    assert!(tui.quit().success());
}

/// What the search strip's status line says it searched: the `in N lines` of
/// it, so a test can tell a fresh answer from the one before it.
fn searched_lines(frame: &str) -> Option<String> {
    frame.lines().find_map(|line| {
        let at = line.find(" lines of ")?;
        let from = line[..at].rfind(" in ")? + " in ".len();
        Some(line[from..at].to_string())
    })
}

#[test]
fn the_search_mark_stays_on_a_wrapped_scrollback_line_while_the_agent_prints() {
    // The bug as reported: in an agent session the strip marked the wrong
    // rows, yet Enter landed on the right one. A hit is a position — how far
    // back its line is — and a working agent keeps printing under it, so the
    // position goes stale. The kernel re-runs the search and the answer
    // moves, but the strip only told the terminal where to scroll on a key,
    // so the view kept the old offset and the mark sat on whatever had moved
    // under it; Enter re-sent the fresh position and so landed correctly.
    // The line is a wrapped one with the match on its second row, in the
    // scrollback.
    let Some((profile, mut tui)) = scripted_agent_session(|root| {
        format!(
            "seq 1 200\nprintf '%0100d tb-wrapped\\n' 0\nseq 1 300\necho tb-ready\n{}",
            after_redraw(root, "seq 1 7")
        )
    }) else {
        return;
    };
    tui.wait_gone("tb-wrapped");
    tui.send(CTRL_SLASH);
    tui.wait_for("Search");
    tui.send(b"tb-wrapped");
    // The result row's snippet is cut to the strip's width, so it is the
    // count that says the line was found.
    tui.wait_until("a result for the wrapped line", |frame| {
        frame.contains("text 1 in")
            && frame
                .lines()
                .any(|l| l.contains("probe") && l.contains('↑'))
    });
    // Revealed by a key, so this holds whether or not selecting a result
    // reveals it by itself.
    tui.send(b"\x1b[B");
    wait_for_mark_on(&tui, "tb-wrapped", "the previewed result");

    let before = searched_lines(&tui.frame());
    std::fs::write(profile.root.path().join("redraw"), "").expect("signal the agent");
    tui.wait_until(
        "the search to re-run over what the agent printed",
        |frame| searched_lines(frame).is_some_and(|now| Some(&now) != before.as_ref()),
    );
    wait_for_mark_on(
        &tui,
        "tb-wrapped",
        "the previewed result after the agent printed",
    );

    tui.send(b"\r");
    tui.wait_until("the terminal to take focus", |frame| {
        !frame.contains("Search")
    });
    wait_for_mark_on(&tui, "tb-wrapped", "the opened result");
    assert!(tui.quit().success());
}

#[test]
fn the_search_mark_follows_a_redraw_on_the_alternate_screen() {
    // Codex and Claude draw on the alternate screen, which has no scrollback:
    // a hit there is a screen row, and the program repaints it wherever it
    // likes. A repaint that moves the line moves the hit, and the mark has to
    // move with it rather than stay on the row the line used to be on.
    let Some((profile, mut tui)) = scripted_agent_session(|root| {
        format!(
            "printf '\\033[?1049h\\033[H\\033[2J'\n\
             for i in 1 2 3 4 5; do echo \"header $i\"; done\n\
             echo 'tb-alt v1'\necho tb-ready\n{}",
            after_redraw(
                root,
                "printf '\\033[H\\033[2J\\n\\n\\n\\n'\n\
                 for i in 1 2 3 4 5; do echo \"header $i\"; done\n\
                 echo 'tb-alt v2'\necho tb-ready"
            )
        )
    }) else {
        return;
    };
    tui.send(CTRL_SLASH);
    tui.wait_for("Search");
    tui.send(b"tb-alt");
    tui.wait_until("a result row for the on-screen line", |frame| {
        frame.lines().any(|line| {
            line.contains("probe") && line.contains("on screen") && line.contains("tb-alt v1")
        })
    });
    tui.send(b"\x1b[B");
    wait_for_mark_on(&tui, "tb-alt v1", "the previewed result");

    std::fs::write(profile.root.path().join("redraw"), "").expect("signal the agent");
    tui.wait_until("the search to find the repainted line", |frame| {
        frame
            .lines()
            .any(|line| line.contains("probe") && line.contains("tb-alt v2"))
    });
    wait_for_mark_on(&tui, "tb-alt v2", "the previewed result after the repaint");

    tui.send(b"\r");
    tui.wait_until("the terminal to take focus", |frame| {
        !frame.contains("Search")
    });
    wait_for_mark_on(&tui, "tb-alt v2", "the opened result");
    assert!(tui.quit().success());
}

// --- a WSL session whose launcher shares the interface's terminal ----------

/// The WSL session's name.
const WSL_NAME: &str = "in-the-distro";

/// A stand-in for `wsl.exe` that runs the "distro" command on this machine and,
/// like the real one, reads the terminal it was started under for as long as it
/// runs.
///
/// That second half is the whole point. On Windows 11 a `wsl.exe` child whose
/// stdin, stdout and stderr are all redirected still attaches to its parent's
/// console and takes that console's input: a console reader received 0 of 8
/// keys while `wsl.exe -d <distro> sleep 40` ran beside it, and 8 of 8 once the
/// same child was started with `CREATE_NO_WINDOW`. A control-mode connection is
/// such a child for as long as a session on the distro is attached, so the
/// interface stopped answering the keyboard the moment one connected.
///
/// A pty has no console. What a child that was not kept off the interface's
/// terminal can reach here is the controlling terminal, `/dev/tty`, so that is
/// what the stand-in reads — and the read is only possible for a child that
/// still has one. Everything else is `fake_ssh`'s shape: `-l` lists one distro,
/// `-d`/`--cd` are dropped, `--exec` runs argv as given and anything else goes
/// through the shell, which is how `wsl.exe` forwards whitespace-free tokens.
fn fake_wsl(profile: &Profile, distro: &str) {
    use std::os::unix::fs::PermissionsExt;
    let script = profile.bin.join("wsl.exe");
    std::fs::write(
        &script,
        format!(
            "#!/bin/sh\n\
             if [ \"$1\" = -l ]; then printf '%s\\n' {distro}; exit 0; fi\n\
             direct=\n\
             while [ \"$#\" -gt 0 ]; do\n\
             \x20 case \"$1\" in\n\
             \x20   -d|--cd) shift; shift ;;\n\
             \x20   -e|--exec) shift; direct=1; break ;;\n\
             \x20   *) break ;;\n\
             \x20 esac\n\
             done\n\
             relay=\n\
             if (: < /dev/tty) 2>/dev/null; then\n\
             \x20 cat < /dev/tty > /dev/null &\n\
             \x20 relay=$!\n\
             fi\n\
             if [ -n \"$direct\" ]; then \"$@\"; else eval \"$*\"; fi\n\
             status=$?\n\
             [ -n \"$relay\" ] && kill \"$relay\"\n\
             exit \"$status\"\n",
        ),
    )
    .expect("write the wsl.exe stand-in");
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).expect("chmod");
}

#[test]
fn the_keyboard_is_still_the_interfaces_while_a_wsl_session_is_attached() {
    // The freeze reported against native Windows: create or switch to a
    // session on a WSL distro and the interface stops answering. It is still
    // painting — the loop is not blocked — but no key reaches it, because the
    // `wsl.exe` behind the session's control-mode connection is reading them.
    //
    // A printable run typed into the palette's filter is the assertion rather
    // than one chord: with two readers on a terminal each key goes to
    // whichever is woken first, so a single key could slip through by luck,
    // while twenty-six in order cannot.
    if !have_tmux() {
        eprintln!("skipping: tmux is not installed");
        return;
    }
    let distro = "e2e-distro";
    let profile = Profile::new();
    fake_wsl(&profile, distro);
    std::fs::write(
        profile.path("config/agents.toml"),
        "default = \"shell\"\n\n[[agents]]\nname = \"shell\"\ncommand = \"sh\"\nargs = []\n",
    )
    .expect("seed agents");
    // Configured rather than left to discovery so the socket can be named — the
    // "distro" is this machine, and a default would land on the developer's own
    // server — and sharing is off because its database would be this one.
    std::fs::write(
        profile.path("config/hosts.toml"),
        format!(
            "[[hosts]]\n\
             name = \"{distro}\"\n\
             kind = \"wsl\"\n\
             socket = \"{socket}\"\n\
             share_sessions = false\n\
             worktrees_dir = \"{worktrees}\"\n",
            socket = profile.server.socket(),
            worktrees = profile.path("worktrees").display(),
        ),
    )
    .expect("seed hosts");
    let repo = repo(profile.root.path());
    profile.cli(&[
        "session",
        "create",
        "--name",
        WSL_NAME,
        "--repo-path",
        repo.to_str().expect("utf-8 path"),
        "--agent",
        "shell",
        "--host",
        distro,
    ]);
    profile.cli(&["config", "accept-interface"]);

    let mut tui = Tui::spawn(&profile, 40, 120);
    tui.wait_for(WSL_NAME);
    // Its prompt is painted, so the control-mode connection is up — and with it
    // the `wsl.exe` that has to stay off this terminal.
    tui.wait_for("$ ");

    tui.send(CTRL_P);
    tui.wait_within(
        RESPONSIVE,
        "the palette to open while a WSL session is attached",
        |frame| frame.contains("type to filter commands"),
    );
    let typed = "qwertyuiopasdfghjklzxcvbnm";
    tui.send(typed.as_bytes());
    tui.wait_within(
        RESPONSIVE,
        "every key typed into the palette to reach it",
        |frame| frame.contains(&format!("> {typed}")),
    );

    tui.send(ESC);
    tui.wait_gone("type to filter commands");
    assert!(tui.quit().success());
}
