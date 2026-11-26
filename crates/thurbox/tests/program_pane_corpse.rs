#![cfg(unix)]

use std::process::Command;
use std::time::{Duration, Instant};

use thurbox::kernel::terminal::{ProgramKey, Terminals};

#[path = "../../runtime-mux/tests/support/tmux_server.rs"]
mod tmux_server;

use tmux_server::TmuxServer;

const SOCKET: &str = "thurbox-program-corpse-e2e";
const OWNER: &str = "mods/90_files.lua";
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
    let _server = TmuxServer::pin(SOCKET);
    thurbox::paths::set_test_dir(dir.path());

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
        eprintln!(
            "skipping: tmux would not start a server: {}",
            String::from_utf8_lossy(&started.stderr).trim()
        );
        return;
    }

    let key = ProgramKey::new(OWNER, PANE);
    let mut first = Terminals::with_registry(
        std::sync::Arc::new(thurbox::backend::wiring::local_only(&Default::default())),
        &Default::default(),
    );
    if let Err(e) = first.start_program(
        &key,
        "sh",
        &["-c".to_string(), "printf started; sleep 300".to_string()],
        Some(dir.path()),
        24,
        80,
    ) {
        panic!("the program pane could not be started with tmux installed: {e}");
    }
    let (window, _pane, pid, _) = program_window().expect("the program window should exist");

    let _ = tmux(&["set-window-option", "-t", &window, "remain-on-exit", "on"]);
    let _ = tokio::process::Command::new("kill")
        .arg(&pid)
        .output()
        .await;

    let deadline = Instant::now() + DEADLINE;
    let mut corpse = false;
    while Instant::now() < deadline {
        if let Some((_, _, _, dead)) = program_window() {
            if dead == "1" {
                corpse = true;
                break;
            }
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    if !corpse {
        eprintln!("skipping: this tmux does not keep a dead pane's frame");
        return;
    }

    drop(first);
    let mut second = Terminals::with_registry(
        std::sync::Arc::new(thurbox::backend::wiring::local_only(&Default::default())),
        &Default::default(),
    );
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
