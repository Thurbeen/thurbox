//! A program pane must notice that its program ended.
//!
//! The regression this guards was silent in every direction. tmux control mode
//! announces a pane's death only as `%window-close` / `%unlinked-window-close`
//! — by WINDOW — while output is streamed by PANE, and nothing related the two,
//! so the notification was parsed as `Notification::Other` and dropped. The
//! pane's channel then simply went quiet, which is indistinguishable from a
//! program with nothing to say: `has_exited` stayed false for ever.
//!
//! Everything downstream is built on that flag. `program.exited` never fired,
//! so a pane could not move on from a finished program; the surface kept
//! painting the grid it left behind; and `start_program` — idempotent by
//! design, because plugins ask on every frame — kept answering `Ok(())` to
//! every request to start it again. Quitting the editor with `:q` therefore
//! made its pane unopenable for the rest of the session, with nothing in the
//! log to say why.
//!
//! Driven through a real tmux pane because the thing that was wrong is the
//! protocol reading, not the bookkeeping around it: a test that set the flag
//! itself would have passed all along. Skipped when tmux is absent — a missing
//! multiplexer is an environment fact, not a regression.
//!
//! The session is created with **`remain-on-exit on`**, which is not decoration:
//! it is what thurbox sets for its own session (`SESSION_OPTS`), so that a dead
//! agent leaves a readable window behind. With that option a window does NOT
//! close when its program ends — the pane simply goes dead and stays — and the
//! close notification never comes. The first version of this test used tmux's
//! default (`off`), passed, and proved nothing about the machine it was written
//! for. A test of this has to stand in the session the program actually runs in.

#![cfg(unix)]

use std::collections::HashMap;
use std::process::Command;
use std::time::{Duration, Instant};

use thurbox::agent::backend::{ProgramPane, SessionBackend};
use thurbox::agent::tmux::{TmuxBackend, SOCKET_OVERRIDE_ENV, SOCKET_OWNER_ENV};

/// A throwaway socket, so this never touches the real one.
const SOCKET: &str = "thurbox-program-exit-e2e";

/// Generous next to the notification, which arrives with the exit: the budget is
/// for a loaded machine starting a tmux server, not for the signal itself.
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

/// Starts the session with **`remain-on-exit on`** (see the note at the top),
/// kept out of the async test body: a blocking `Command::output` call written
/// directly in an `async fn` blocks the executor thread it runs on.
fn start_session(dir: &std::path::Path) -> std::process::Output {
    let started = Command::new("tmux")
        .args([
            "-L",
            SOCKET,
            "new-session",
            "-d",
            "-s",
            "thurbox",
            "-x",
            "80",
            "-y",
            "24",
        ])
        .env("TMUX_TMPDIR", dir)
        .output()
        .expect("run tmux");
    let _ = Command::new("tmux")
        .args([
            "-L",
            SOCKET,
            "set-option",
            "-t",
            "thurbox",
            "remain-on-exit",
            "on",
        ])
        .env("TMUX_TMPDIR", dir)
        .output()
        .expect("run tmux");
    started
}

/// A tokio runtime is required, not decorative: wiring a pane spawns its writer
/// task, and without one the spawn panics before anything can be observed.
#[tokio::test(flavor = "multi_thread")]
async fn a_program_that_ends_reports_that_it_ended() {
    if !have_tmux() {
        eprintln!("skipping: tmux is not installed");
        return;
    }

    let dir = tempfile::tempdir().expect("tempdir");
    // nextest runs one process per test, so process-wide env is safe here.
    std::env::set_var("TMUX_TMPDIR", dir.path());
    std::env::set_var(SOCKET_OVERRIDE_ENV, SOCKET);
    // The override is dropped when it was injected for someone else's data dir
    // (see `socket_for`); this test *is* somebody typing it.
    std::env::remove_var(SOCKET_OWNER_ENV);
    thurbox::paths::set_test_dir(dir.path());

    cleanup();
    let started = start_session(dir.path());
    if !started.status.success() {
        cleanup();
        eprintln!(
            "skipping: tmux would not start a server: {}",
            String::from_utf8_lossy(&started.stderr).trim()
        );
        return;
    }

    let backend = std::sync::Arc::new(TmuxBackend::local());
    if let Err(e) = backend.ensure_ready() {
        cleanup();
        panic!("tmux control mode would not start: {e:#}");
    }

    // Prints, lives for a moment, then ends on its own — the shape of an editor
    // being quit, as opposed to a pane someone killed from outside. The moment
    // is not padding: a program that exits instantly takes its pane with it
    // before `spawn` can size the window, and the spawn fails with "can't find
    // pane" — which this test used to report as a skipped environment and pass
    // on, proving nothing at all. It also has to outlive `ProgramPane::spawn`'s
    // own `display-message` round trip (registering which window the pane's
    // death will be announced on, so the notification has somewhere to land):
    // on a loaded machine that round trip can stretch well past a second, and
    // a program already gone by the time it returns is a death nothing was
    // listening for yet, not a slow notification. 1s cut it close under a full
    // parallel `nextest` run; 3s gives that round trip real headroom.
    let pane = ProgramPane::spawn(
        std::sync::Arc::clone(&backend) as std::sync::Arc<dyn SessionBackend>,
        "tbp-test-exiting",
        "sh",
        &["-c".to_string(), "printf started; sleep 3".to_string()],
        Some(dir.path()),
        &HashMap::new(),
        24,
        80,
    );
    let pane = match pane {
        Ok(pane) => pane,
        Err(e) => {
            cleanup();
            // Not a skip. The header already records what a skip here cost
            // once: a spawn failing because the pane died too fast was read as
            // a missing environment and passed, proving nothing at all.
            panic!("the program pane could not be spawned: {e:#}");
        }
    };

    let deadline = Instant::now() + DEADLINE;
    while !pane.has_exited() && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let exited = pane.has_exited();
    cleanup();
    assert!(
        exited,
        "a program pane whose program exited still reports itself running; \
         nothing downstream can ever restart it"
    );
}
