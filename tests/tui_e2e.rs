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
const F6: &[u8] = b"\x1b[17~";
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
    /// `TMUX_TMPDIR`. An AF_UNIX socket path is limited to ~104 bytes and a
    /// tempdir under a long `TMPDIR` blows through it with "File name too
    /// long", so this is its own short directory — `$XDG_RUNTIME_DIR` where
    /// there is one, the same rule `scripts/dev/lib/sandbox-env.sh` applies.
    sockets: PathBuf,
    socket: String,
}

impl Profile {
    fn new() -> Self {
        let root = tempfile::tempdir().expect("tempdir");
        for sub in ["home", "config", "data"] {
            std::fs::create_dir_all(root.path().join(sub)).expect("mkdir");
        }
        let socket = private_socket();
        let sockets = std::env::var_os("XDG_RUNTIME_DIR")
            .map(PathBuf::from)
            .filter(|dir| dir.is_dir())
            .unwrap_or_else(std::env::temp_dir)
            .join(&socket);
        std::fs::create_dir_all(&sockets).expect("mkdir sockets");
        // No update check, no version check (both reach the network), and no
        // automation heartbeat (it would arm a tmux keeper window on startup).
        std::fs::write(
            root.path().join("config/settings.toml"),
            "[features]\nautomations = false\nversion_check = false\nauto_update = false\n",
        )
        .expect("seed settings");
        Self {
            root,
            sockets,
            socket,
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
        cmd.env("TMUX_TMPDIR", &self.sockets);
        cmd.env(thurbox::agent::tmux::SOCKET_OVERRIDE_ENV, &self.socket);
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

impl Drop for Profile {
    fn drop(&mut self) {
        // Best-effort: only a scenario that spawned a session started a server.
        let _ = Command::new("tmux")
            .env("TMUX_TMPDIR", &self.sockets)
            .args(["-L", &self.socket, "kill-server"])
            .output();
        let _ = std::fs::remove_dir_all(&self.sockets);
    }
}

/// A pseudo-terminal pair at the given size.
fn openpty(rows: u16, cols: u16) -> (OwnedFd, OwnedFd) {
    let mut master = -1;
    let mut slave = -1;
    let size = libc::winsize {
        ws_row: rows,
        ws_col: cols,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    // SAFETY: openpty writes two valid descriptors into the out-params on
    // success; the name and termios pointers are allowed to be null.
    let rc = unsafe {
        libc::openpty(
            &mut master,
            &mut slave,
            std::ptr::null_mut(),
            std::ptr::null(),
            &size,
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
        let deadline = Instant::now() + WAIT;
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

    let tui = Tui::spawn(&profile, 40, 120);
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
