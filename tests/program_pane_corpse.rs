//! A program window whose program has died is not re-adopted.
//!
//! What this guards is what "the editor hung" actually looked like. A window can
//! outlive the program in it — tmux's `remain-on-exit` keeps the frame, and
//! thurbox turns that option on for a dead agent's window to stay readable (with
//! a twist: `set-option -t <session> remain-on-exit on` lands on the session's
//! CURRENT window, measured on tmux 3.2a, so the option reaches whatever window
//! happened to be current, which can be a program's). The corpse keeps the
//! window's deterministic name, so the next run of the interface finds it and
//! re-adopts it: a pane painting a frozen last screen, swallowing every
//! keystroke, with `:q` going nowhere.
//!
//! Two runs of `Terminals` over one tmux server is the honest shape of it — the
//! second is the restart, and it must not inherit the first one's corpse.
//!
//! Skipped when tmux is absent: a missing multiplexer is an environment fact.

#![cfg(unix)]

use std::process::Command;
use std::time::{Duration, Instant};

use thurbox::kernel::terminal::{ProgramKey, Terminals};

const SOCKET: &str = "thurbox-program-corpse-e2e";
const OWNER: &str = "plugins/90_files.lua";
const PANE: &str = "editor_corpse";
const DEADLINE: Duration = Duration::from_secs(10);

fn have_tmux() -> bool {
    Command::new("tmux")
        .arg("-V")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn tmux(args: &[&str]) -> std::process::Output {
    Command::new("tmux")
        .args(["-L", SOCKET])
        .args(args)
        .output()
        .expect("run tmux")
}

fn cleanup() {
    let _ = tmux(&["kill-server"]);
}

/// Every program window on the server: `(window_id, pane_id, pane_pid,
/// pane_dead)`.
///
/// All of them, not the first — the deterministic name is supposed to address
/// exactly one window, and a test that looked at one line could not tell a
/// replaced corpse from a second window opened beside it.
fn program_windows() -> Vec<(String, String, String, String)> {
    let out = tmux(&[
        "list-windows",
        "-a",
        "-F",
        "#{window_name} #{window_id} #{pane_id} #{pane_pid} #{pane_dead}",
    ]);
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|line| line.starts_with("tbp-"))
        .map(|line| {
            let mut it = line.split_whitespace().skip(1);
            (
                it.next().unwrap_or_default().to_string(),
                it.next().unwrap_or_default().to_string(),
                it.next().unwrap_or_default().to_string(),
                it.next().unwrap_or_default().to_string(),
            )
        })
        .collect()
}

/// The one program window, when there is exactly one.
fn program_window() -> Option<(String, String, String, String)> {
    let mut all = program_windows();
    (all.len() == 1).then(|| all.remove(0))
}

#[tokio::test(flavor = "multi_thread")]
async fn a_dead_program_window_is_replaced_rather_than_adopted() {
    if !have_tmux() {
        eprintln!("skipping: tmux is not installed");
        return;
    }

    let dir = tempfile::tempdir().expect("tempdir");
    std::env::set_var("TMUX_TMPDIR", dir.path());
    std::env::set_var(thurbox::agent::tmux::SOCKET_OVERRIDE_ENV, SOCKET);
    std::env::remove_var(thurbox::agent::tmux::SOCKET_OWNER_ENV);
    thurbox::paths::set_test_dir(dir.path());

    cleanup();
    let started = tmux(&[
        "new-session",
        "-d",
        "-s",
        "thurbox-dev",
        "-x",
        "80",
        "-y",
        "24",
    ]);
    if !started.status.success() {
        cleanup();
        eprintln!(
            "skipping: tmux would not start a server: {}",
            String::from_utf8_lossy(&started.stderr).trim()
        );
        return;
    }

    let key = ProgramKey::new(OWNER, PANE);
    let mut first = Terminals::new();
    if let Err(e) = first.start_program(
        &key,
        "sh",
        &["-c".to_string(), "printf started; sleep 300".to_string()],
        Some(dir.path()),
        24,
        80,
    ) {
        cleanup();
        // Not a skip: tmux is installed, so a pane that would not start is this
        // path being broken rather than a machine without a multiplexer.
        panic!("the program pane could not be started: {e}");
    }
    let (window, _pane, pid, _) = program_window().expect("the program window should exist");

    // The corpse: the option that keeps a dead pane's frame, then the program
    // killed from outside — which is what an editor being quit looks like to
    // tmux, minus the window closing.
    let _ = tmux(&["set-window-option", "-t", &window, "remain-on-exit", "on"]);
    let _ = Command::new("kill").arg(&pid).output();

    let deadline = Instant::now() + DEADLINE;
    let mut corpse = false;
    while Instant::now() < deadline {
        if let Some((_, _, _, dead)) = program_window() {
            if dead == "1" {
                corpse = true;
                break;
            }
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    if !corpse {
        cleanup();
        eprintln!("skipping: this tmux does not keep a dead pane's frame");
        return;
    }

    // The restart: a fresh `Terminals` over the same server, finding that window
    // by its deterministic name.
    drop(first);
    let mut second = Terminals::new();
    let started_again = second.start_program(
        &key,
        "sh",
        &["-c".to_string(), "printf again; sleep 300".to_string()],
        Some(dir.path()),
        24,
        80,
    );

    let state = second
        .program_state(&key)
        .map(|(p, exited)| (p.to_string(), exited));
    let windows_now = program_windows();
    cleanup();

    started_again.expect("starting over a corpse must work");
    let (_, exited) = state.expect("the interface should hold a program pane");
    assert!(
        !exited,
        "adopted the corpse: a pane that paints a frozen screen and swallows every key"
    );
    assert_eq!(
        windows_now.len(),
        1,
        "the deterministic name must still address one window, not a fresh one \
         beside the corpse: {windows_now:?}"
    );
    let (_, _, _, dead) = &windows_now[0];
    assert_eq!(dead, "0", "the window it holds must be a live one");
}
