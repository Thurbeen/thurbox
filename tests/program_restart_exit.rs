//! A program that is restarted still reports that it ended.
//!
//! `program.exited` is derived by comparing the program panes held now against
//! the ones held at the previous look: a key whose pane reports `has_exited` and
//! was seen running before is an ending. The loop, though, applies the commands
//! plugins enqueued **before** it derives — and a plugin that asks for its
//! program on every frame (the documented pattern, which is what makes
//! `start_program` idempotent) asks again on the very frame after its program
//! died. `start_program` then replaces the finished slot, and the derivation
//! that runs a few lines later is handed a *live* pane under the same key. No
//! transition, no event: the plugin that restarted the program is never told the
//! old one finished, which is exactly the plugin that most needs to know.
//!
//! So the ending is recorded where the slot is overwritten, and drained where
//! the transition is derived. Asserted through the real restart path on a real
//! pane, because the claim is that the replacement *reaches* the recording —
//! setting the flag by hand would have passed before the fix too.
//!
//! Skipped when tmux is absent: a missing multiplexer is an environment fact.

#![cfg(unix)]

use std::process::Command;
use std::time::{Duration, Instant};

use thurbox::kernel::terminal::{ProgramKey, Terminals};

const SOCKET: &str = "thurbox-program-restart-e2e";

/// Generous next to the exit itself: the budget is for a loaded machine starting
/// a tmux server, not for the notification.
const DEADLINE: Duration = Duration::from_secs(10);

fn have_tmux() -> bool {
    Command::new("tmux")
        .arg("-V")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn cleanup() {
    let _ = Command::new("tmux")
        .args(["-L", SOCKET, "kill-server"])
        .output();
}

#[tokio::test(flavor = "multi_thread")]
async fn restarting_a_finished_program_still_reports_the_ending() {
    if !have_tmux() {
        eprintln!("skipping: tmux is not installed");
        return;
    }

    let dir = tempfile::tempdir().expect("tempdir");
    // nextest runs one process per test, so process-wide env is safe here.
    std::env::set_var("TMUX_TMPDIR", dir.path());
    std::env::set_var(thurbox::agent::tmux::SOCKET_OVERRIDE_ENV, SOCKET);
    std::env::remove_var(thurbox::agent::tmux::SOCKET_OWNER_ENV);
    thurbox::paths::set_test_dir(dir.path());

    cleanup();

    let key = ProgramKey::new("plugins/90_files.lua", "editor_opts");
    let mut terminals = Terminals::new();

    // Lives for a moment, then ends on its own — an editor being quit. Not
    // instant: a program that exits before tmux has sized the window takes the
    // pane with it and the spawn fails outright, which would turn this into a
    // skip that proves nothing.
    let short = ["-c".to_string(), "printf started; sleep 1".to_string()];
    if let Err(e) = terminals.start_program(&key, "sh", &short, Some(dir.path()), 24, 80) {
        cleanup();
        eprintln!("skipping: tmux would not start a program pane: {e}");
        return;
    }

    let exited = |terminals: &Terminals| {
        terminals
            .program_liveness()
            .iter()
            .any(|(k, _, exited)| k == &key && *exited)
    };
    let deadline = Instant::now() + DEADLINE;
    while !exited(&terminals) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    if !exited(&terminals) {
        cleanup();
        panic!("the program never reported that it ended; nothing to restart over");
    }

    // Nothing drained it in between: the ending and the restart happen inside
    // one iteration of the loop, which is the whole point.
    assert!(
        terminals.take_replaced_program_exits().is_empty(),
        "an ending was reported before anything replaced it"
    );

    // The plugin asks again, as it does on every frame.
    let long = ["-c".to_string(), "sleep 300".to_string()];
    let restarted = terminals.start_program(&key, "sh", &long, Some(dir.path()), 24, 80);
    let replaced = terminals.take_replaced_program_exits();
    let live_again = terminals
        .program_state(&key)
        .map(|(_, exited)| !exited)
        .unwrap_or(false);
    cleanup();

    assert!(restarted.is_ok(), "the restart failed: {restarted:?}");
    assert!(
        live_again,
        "the restart left no live pane, so this asserts nothing about a \
         replacement"
    );
    assert_eq!(
        replaced.iter().map(|(k, _)| k).collect::<Vec<_>>(),
        vec![&key],
        "a program that was replaced while finished reported no ending; the \
         plugin that restarted it is never told the old one stopped"
    );
}
