#![cfg(unix)]

use std::process::Command;
use std::time::{Duration, Instant};

use thurbox::kernel::terminal::{ProgramKey, ProgramTransition, Terminals};

#[path = "../../runtime-mux/tests/support/tmux_server.rs"]
mod tmux_server;

use tmux_server::TmuxServer;

const SOCKET: &str = "thurbox-program-restart-e2e";

const DEADLINE: Duration = Duration::from_secs(10);

fn have_tmux() -> bool {
    Command::new("tmux")
        .arg("-V")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

#[tokio::test(flavor = "multi_thread")]
async fn restarting_a_finished_program_still_reports_the_ending() {
    if !have_tmux() {
        eprintln!("skipping: tmux is not installed");
        return;
    }

    let dir = tempfile::tempdir().expect("tempdir");
    let _server = TmuxServer::pin(SOCKET);
    thurbox::paths::set_test_dir(dir.path());

    let key = ProgramKey::new("mods/90_files.lua", "editor_opts");
    let mut terminals = Terminals::with_registry(
        std::sync::Arc::new(thurbox::backend::wiring::local_only(&Default::default())),
        &Default::default(),
    );

    // tmux drops the pane of a program that exits before the window is sized, so it lives a moment.
    let short = ["-c".to_string(), "printf started; sleep 1".to_string()];
    if let Err(e) = terminals.start_program(&key, "sh", &short, Some(dir.path()), 24, 80) {
        panic!("the program pane could not be started with tmux installed: {e}");
    }

    let exited = |terminals: &Terminals| {
        terminals
            .program_liveness()
            .iter()
            .any(|(k, _, exited)| k == &key && *exited)
    };
    let deadline = Instant::now() + DEADLINE;
    while !exited(&terminals) && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    if !exited(&terminals) {
        panic!("the program never reported that it ended; nothing to restart over");
    }

    assert_eq!(
        terminals.take_program_transitions(),
        vec![ProgramTransition::Started(key.clone())],
        "the spawn was the only thing that happened, and an ending was reported \
         before anything replaced it"
    );

    let long = ["-c".to_string(), "sleep 300".to_string()];
    let restarted = terminals.start_program(&key, "sh", &long, Some(dir.path()), 24, 80);
    let transitions = terminals.take_program_transitions();
    let live_again = terminals
        .program_state(&key)
        .map(|(_, exited)| !exited)
        .unwrap_or(false);

    assert!(restarted.is_ok(), "the restart failed: {restarted:?}");
    assert!(
        live_again,
        "the restart left no live pane, so this asserts nothing about a \
         replacement"
    );
    assert_eq!(
        transitions,
        vec![
            ProgramTransition::Replaced(key.clone(), "sh".to_string()),
            ProgramTransition::Started(key.clone()),
        ],
        "a program that was replaced while finished reported no ending; the \
         plugin that restarted it is never told the old one stopped"
    );
}
