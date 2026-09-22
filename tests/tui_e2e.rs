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
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

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
const F8: &[u8] = b"\x1b[19~";

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
    let dir = under.join("repo");
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

/// The same, with the binary's environment adjusted — for the cases where what
/// is being tested is what thurbox does with the machine it thinks it is on.
fn shell_session_with(adjust: impl FnOnce(&mut Command)) -> Option<(Profile, Tui)> {
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

    profile.cli(&[
        "session",
        "create",
        "--name",
        "probe",
        "--repo-path",
        repo.to_str().expect("utf-8 path"),
        "--agent",
        "shell",
    ]);
    // A database with session history is a v1 profile as far as the one-time
    // gate can tell, and a gate on a pty is a real prompt; this is the
    // headless answer to it.
    profile.cli(&["config", "accept-interface"]);

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

/// Whether the action band names `pane` as the focused one.
fn band_names(frame: &str, pane: &str) -> bool {
    frame
        .lines()
        .last()
        .is_some_and(|band| band.trim_start().starts_with(pane))
}

/// A `probe` session whose agent is `sh`, under the layout preset `layout`
/// chosen the way an install chooses it, on the real binary at 120×40.
fn session_under_layout(layout: &str) -> Option<(Profile, Tui)> {
    if !have_tmux() {
        eprintln!("skipping: tmux is not installed");
        return None;
    }
    let profile = Profile::new();
    std::fs::write(
        profile.path("config/agents.toml"),
        "default = \"probe-agent\"\n\n[[agents]]\nname = \"probe-agent\"\ncommand = \"sh\"\nargs = []\n",
    )
    .expect("seed agents");
    let repo = repo(profile.root.path());
    // The companion shell is the user's `$SHELL`, which under this profile's
    // empty HOME can be an interactive first-run wizard that eats keystrokes.
    // Pinned for every process here, since whichever starts the multiplexer
    // server hands it the default shell.
    let cli = |args: &[&str]| {
        let mut command = Command::new(env!("CARGO_BIN_EXE_thurbox-cli"));
        profile.apply(&mut command);
        command.env("SHELL", "/bin/sh");
        let output = command.args(args).output().expect("run thurbox-cli");
        assert!(
            output.status.success(),
            "thurbox-cli {args:?} failed:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
    };
    cli(&[
        "session",
        "create",
        "--name",
        "probe",
        "--repo-path",
        repo.to_str().expect("utf-8 path"),
        "--agent",
        "probe-agent",
    ]);
    cli(&["config", "accept-interface"]);
    cli(&["layout", "set", layout]);

    let tui = Tui::spawn_with(&profile, 40, 120, |command| {
        command.env("SHELL", "/bin/sh");
    });
    tui.wait_for("(probe-agent)");
    Some((profile, tui))
}

#[test]
fn split_shell_shows_the_agent_and_its_shell_at_once_and_f8_moves_between_them() {
    // The `split-shell` preset, chosen the way an install chooses it, on the real
    // binary: the agent and the same session's shell are both painted, the
    // agent pane no longer offers a Shell tab, and F8 walks focus into the shell
    // pane — where keystrokes reach the shell — and back out.
    let Some((_profile, mut tui)) = session_under_layout("split-shell") else {
        return;
    };
    tui.wait_for("(probe-agent)");
    tui.wait_for("probe (shell)");
    tui.wait_until("the agent pane to be the focused one", |frame| {
        band_names(frame, "Agent")
    });
    let (_, agent_title) = tui.find("(probe-agent)");
    let (_, shell_title) = tui.find("probe (shell)");
    assert!(
        shell_title > agent_title,
        "the shell pane sits below the agent:\n{}",
        tui.frame()
    );
    assert!(
        !tui.frame().contains("Shell ·"),
        "the agent pane still offers a Shell tab:\n{}",
        tui.frame()
    );

    tui.send(F8);
    tui.wait_until("the shell pane to take focus", |frame| {
        band_names(frame, "Shell")
    });
    tui.wait_until_quiet();
    tui.send(b"echo tb-split-\"\"marker\r");
    tui.wait_for("tb-split-marker");
    let (_, marker) = tui.find("tb-split-marker");
    assert!(
        marker > shell_title,
        "typed into the shell pane, not the agent:\n{}",
        tui.frame()
    );

    tui.send(F8);
    tui.wait_until("focus to return to the agent", |frame| {
        band_names(frame, "Agent")
    });

    let status = tui.quit();
    assert!(status.success(), "exit must be clean: {status:?}");
}

#[test]
fn search_finds_text_that_scrolled_away_and_opens_the_session_on_it() {
    // The failure search was rebuilt for: a prompt typed earlier has scrolled
    // off the screen, and searching for it found nothing, because only the
    // visible screen was searched. So the marker is printed and then pushed
    // three hundred lines up, found from the strip, and opened — and opening
    // it has to land ON it, scrolled back, not merely focus the session.
    let Some((_profile, mut tui)) = shell_session() else {
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
    // And the line is marked, so a long screen of output does not leave you
    // hunting for the row you were brought to.
    let frame = tui.frame();
    let (y, line) = frame
        .lines()
        .enumerate()
        .find(|(_, line)| line.contains("┃tb-findme "))
        .expect("the landed line");
    let byte = line.find("┃tb-findme").expect("the landed line") + "┃".len();
    let x = line[..byte].chars().count();
    assert!(
        tui.inverse_at(y as u16, x as u16),
        "the landed line is not highlighted:\n{frame}"
    );
    assert!(tui.quit().success());

#[test]
fn focus_shows_the_agent_alone_and_f9_brings_the_session_list_back() {
    // `focus`, on the real binary: the agent pane has the whole width and the
    // list is hidden, and F9 — the same toggle as everywhere else — brings it
    // back on its first press and hides it again on the second.
    let Some((_profile, mut tui)) = session_under_layout("focus") else {
        return;
    };
    tui.wait_until("the agent pane to be the focused one", |frame| {
        band_names(frame, "Agent")
    });
    assert!(
        !tui.frame().contains("Sessions"),
        "the list starts hidden:\n{}",
        tui.frame()
    );
    // Still a tab of the agent pane: this preset places no shell pane.
    assert!(tui.frame().contains("Shell ·"), "{}", tui.frame());

    tui.send(F9);
    tui.wait_for("Sessions");
    let (list_col, _) = tui.find("Sessions");
    let (agent_col, _) = tui.find("(probe-agent)");
    assert!(
        list_col < agent_col,
        "the list opens on the left:\n{}",
        tui.frame()
    );

    tui.send(F9);
    tui.wait_until("the list to hide again", |frame| {
        !frame.contains("Sessions")
    });

    let status = tui.quit();
    assert!(status.success(), "exit must be clean: {status:?}");
}

#[test]
fn ide_shows_the_list_left_and_the_shell_along_the_bottom() {
    // `ide`, on the real binary: the list on the left, the same session's shell
    // as a panel under the agent, and — with no plugin filling a right-hand
    // slot — no right column reserved for nothing.
    let Some((_profile, mut tui)) = session_under_layout("ide") else {
        return;
    };
    tui.wait_for("probe (shell)");
    tui.wait_for("Sessions");
    let (list_col, _) = tui.find("Sessions");
    let (agent_col, agent_row) = tui.find("(probe-agent)");
    let (shell_col, shell_row) = tui.find("probe (shell)");
    let frame = tui.frame();
    assert!(list_col < agent_col, "the list is on the left:\n{frame}");
    assert!(
        shell_row > agent_row,
        "the shell sits below the agent:\n{frame}"
    );
    assert!(
        shell_col > list_col,
        "beside the list, not under it:\n{frame}"
    );
    assert!(
        !frame.contains("Shell ·"),
        "the agent pane still offers a Shell tab:\n{frame}"
    );
    // The agent's frame runs to the last column: its title row ends in the
    // top-right corner at the screen's edge.
    let title_line = frame
        .lines()
        .nth(usize::from(agent_row))
        .expect("title row");
    assert!(
        title_line.trim_end().ends_with('╮') || title_line.trim_end().ends_with('┐'),
        "no right column is reserved when nothing fills it:\n{frame}"
    );
    assert_eq!(
        title_line.trim_end().chars().count(),
        120,
        "the agent reaches the right edge:\n{frame}"
    );

    let status = tui.quit();
    assert!(status.success(), "exit must be clean: {status:?}");
}

#[test]
fn ide_f8_moves_focus_into_the_shell_panel_and_back() {
    let Some((_profile, mut tui)) = session_under_layout("ide") else {
        return;
    };
    tui.wait_for("probe (shell)");
    tui.wait_until("the agent pane to be the focused one", |frame| {
        band_names(frame, "Agent")
    });
    tui.send(F8);
    tui.wait_until("the shell panel to take focus", |frame| {
        band_names(frame, "Shell")
    });
    tui.wait_until_quiet();
    tui.send(b"echo tb-ide-\"\"marker\r");
    tui.wait_for("tb-ide-marker");
    let (_, shell_title) = tui.find("probe (shell)");
    let (_, marker) = tui.find("tb-ide-marker");
    assert!(
        marker > shell_title,
        "typed into the panel:\n{}",
        tui.frame()
    );
    tui.send(F8);
    tui.wait_until("focus to return to the agent", |frame| {
        band_names(frame, "Agent")
    });

    let status = tui.quit();
    assert!(status.success(), "exit must be clean: {status:?}");
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

fn wait_for_view(tui: &Tui, view: &str) {
    tui.wait_until(
        &format!("the {view} view to be the one on screen"),
        |frame| band_names(frame, view),
    );
}

#[test]
fn classic_keeps_the_shell_a_tab_that_switches_and_holds_a_working_shell() {
    // `classic` is the unchanged default: the companion shell is a tab of the
    // agent pane that Ctrl+T raises and lowers, and no shell pane appears.
    let Some((_profile, mut tui)) = session_under_layout("classic") else {
        return;
    };
    assert!(
        !tui.frame().contains("probe (shell)"),
        "classic places no shell pane:\n{}",
        tui.frame()
    );
    exercise_the_shell_tab(&mut tui);

    let status = tui.quit();
    assert!(status.success(), "exit must be clean: {status:?}");
}

#[test]
fn an_exited_shell_comes_back_in_the_shell_pane() {
    // `exit` typed into the shell pane used to leave its last screen frozen
    // there for good: keystrokes went nowhere, and neither F8 nor a restart of
    // the pane's focus brought a shell back.
    let Some((_profile, mut tui)) = session_under_layout("split-shell") else {
        return;
    };
    tui.wait_for("probe (shell)");
    tui.send(F8);
    wait_for_view(&tui, "Shell");
    tui.wait_until_quiet();
    tui.send(b"echo tb-first-\"\"shell; exit\r");
    tui.wait_for("tb-first-shell");
    tui.wait_gone("tb-first-shell");
    tui.wait_until_quiet();
    tui.send(b"echo tb-second-\"\"shell\r");
    tui.wait_for("tb-second-shell");
    let (_, shell_title) = tui.find("probe (shell)");
    let (_, marker) = tui.find("tb-second-shell");
    assert!(marker > shell_title, "in the shell pane:\n{}", tui.frame());

    let status = tui.quit();
    assert!(status.success(), "exit must be clean: {status:?}");
}

#[test]
fn an_exited_shell_tab_opens_a_new_shell() {
    // The same dead shell under `classic`: its tab kept painting the frozen
    // screen and Ctrl+T toggled between that and the agent forever.
    let Some((_profile, mut tui)) = session_under_layout("classic") else {
        return;
    };
    tui.send(b"\x14");
    wait_for_view(&tui, "Shell");
    tui.wait_until_quiet();
    tui.send(b"echo tb-first-\"\"shell; exit\r");
    tui.wait_for("tb-first-shell");
    tui.wait_gone("tb-first-shell");
    tui.wait_until_quiet();
    tui.send(b"echo tb-second-\"\"shell\r");
    tui.wait_for("tb-second-shell");
    assert!(tui.frame().contains("probe (shell)"), "{}", tui.frame());

    let status = tui.quit();
    assert!(status.success(), "exit must be clean: {status:?}");
}

#[test]
fn the_shell_keeps_the_keyboard_when_the_screen_narrows_and_widens() {
    // Typing in the shell pane, then narrowing the terminal, took the shell
    // pane off screen and dropped focus onto the agent pane showing the AGENT:
    // the next line typed went to the agent. And the other way round: on the
    // agent's Shell tab, widening gave the agent back its own view.
    let Some((_profile, mut tui)) = session_under_layout("split-shell") else {
        return;
    };
    tui.wait_for("probe (shell)");
    tui.send(F8);
    wait_for_view(&tui, "Shell");
    tui.wait_until_quiet();

    tui.resize(40, 70);
    tui.wait_until("the agent pane to show the shell", |frame| {
        frame.contains("probe (shell)") && !frame.contains("(probe-agent)")
    });
    wait_for_view(&tui, "Shell");
    tui.wait_until_quiet();
    tui.send(b"echo tb-narrow-\"\"marker\r");
    tui.wait_for("tb-narrow-marker");

    tui.resize(40, 120);
    tui.wait_until("both panes to be back", |frame| {
        frame.contains("probe (shell)") && frame.contains("(probe-agent)")
    });
    wait_for_view(&tui, "Shell");
    tui.wait_until_quiet();
    tui.send(b"echo tb-wide-\"\"marker\r");
    tui.wait_for("tb-wide-marker");
    let (_, shell_title) = tui.find("probe (shell)");
    let (_, narrow) = tui.find("tb-narrow-marker");
    let (_, wide) = tui.find("tb-wide-marker");
    assert!(
        narrow > shell_title && wide > shell_title,
        "both lines went to the shell, none to the agent:\n{}",
        tui.frame()
    );

    let status = tui.quit();
    assert!(status.success(), "exit must be clean: {status:?}");
}

#[test]
fn ctrl_d_deletes_a_session_whose_agent_has_exited() {
    // `Ctrl+D` is a passthrough chord: while a terminal has focus it is the
    // agent's EOF, and the delete it also means is left to the session list.
    // But an agent that ran `/exit` leaves a dead pane the window keeps
    // (remain-on-exit), and tmux still accepts `send-keys` into it — so the
    // chord was delivered to a pane no one reads and the session it should
    // have deleted hung in the list. A dead pane is doing no line editing, so
    // the delete is what the chord means there.
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
    fn drag_then_chord(&mut self, (x, y): (u16, u16), over: u16, key: u8) {
        let (px, py) = (x + 1, y + 1);
        let mut seq = format!("\x1b[<0;{px};{py}M").into_bytes();
        for cx in px + 1..=px + over {
            seq.extend_from_slice(format!("\x1b[<32;{cx};{py}M").as_bytes());
        }
        seq.extend_from_slice(format!("\x1b[<0;{};{py}m", px + over).as_bytes());
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

#[test]
fn a_click_is_not_a_selection_so_ctrl_c_still_interrupts_the_shell() {
    // Clicking into a terminal is how it is focused, and the press used to
    // stay armed as an empty selection afterwards: every `Ctrl+C` from then on
    // was taken by the copy chord, which — finding nothing selected — pushed
    // the whole visible screen at the outer terminal as OSC 52 and never
    // reached the shell as the interrupt it was. v1's rule, restored here: a
    // press that never moved is a click, and a selection is only what was
    // dragged over.
    let Some((_profile, mut tui)) = shell_session() else {
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
    tui.drag(at, 12);
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
    // Its first question is "Run On" where the machine has hosts and "Select
    // Repos" where it has none — this one has sibling WSL distros, so which it
    // is depends on the machine and neither is the point.
    tui.wait_until("the new-session wizard to be up", |frame| {
        frame.contains("Run On") || frame.contains("Select Repos")
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
/// the launcher is real: the `TmuxTransport::Ssh` arm, the POSIX quoting, the
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
    // -> `TmuxBackend::is_dead`, a control-mode round trip made on the loop
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
