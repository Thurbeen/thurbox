#![cfg(unix)]

use std::collections::HashMap;
use std::process::Command;
use std::time::{Duration, Instant};
use thurbox::backend::WindowLifecycle;

use thurbox::backend::pane::ProgramPane;
use thurbox::backend::tmux::TmuxBackend;
use thurbox::backend::SessionBackend;

#[path = "../../runtime-mux/tests/support/tmux_server.rs"]
mod tmux_server;

use tmux_server::TmuxServer;

const SOCKET: &str = "thurbox-program-exit-e2e";

const DEADLINE: Duration = Duration::from_secs(10);

fn have_tmux() -> bool {
    Command::new("tmux")
        .arg("-V")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

// The blocking `Command::output` calls are safe here because this test uses
// `#[tokio::test(flavor = "multi_thread")]`: the body runs on its own thread
// (`block_on(body)`), while spawned tasks run on worker threads.
fn start_session(server: &TmuxServer) -> std::process::Output {
    let started = server.tmux(&["new-session", "-d", "-s", "thurbox", "-x", "80", "-y", "24"]);
    let _ = server.tmux(&["set-option", "-t", "thurbox", "remain-on-exit", "on"]);
    started
}

#[tokio::test(flavor = "multi_thread")]
async fn a_program_that_ends_reports_that_it_ended() {
    if !have_tmux() {
        eprintln!("skipping: tmux is not installed");
        return;
    }

    let dir = tempfile::tempdir().expect("tempdir");
    let server = TmuxServer::pin(SOCKET);
    thurbox::paths::set_test_dir(dir.path());

    let started = start_session(&server);
    if !started.status.success() {
        eprintln!(
            "skipping: tmux would not start a server: {}",
            String::from_utf8_lossy(&started.stderr).trim()
        );
        return;
    }

    let backend = std::sync::Arc::new(TmuxBackend::local(&thurbox::backend::mux_settings(
        &Default::default(),
    )));
    if let Err(e) = backend.ensure_ready() {
        panic!("tmux control mode would not start: {e:#}");
    }

    // Prints, lives for a moment, then ends on its own — the shape of an editor
    // being quit, as opposed to a pane someone killed from outside. The moment
    // is not padding: a program that exits instantly takes its pane with it
    // before `spawn` can size the window, and the spawn fails with "can't find
    // pane" — which this test used to report as a skipped environment and pass
    // on, proving nothing at all.
    //
    // A second, longer than that: the program had to outlive whatever stood
    // between its window existing and `pane_windows` knowing about it, because
    // a death inside that gap is announced once, to nobody, and never again.
    // That gap used to be a serialized `display-message` round trip, which is
    // why this budget kept being raised — 1s, then 3s, then 8s — without ever
    // being enough. `new-window` now answers with the window id itself, so the
    // gap is local work, and a second is a second again.
    let pane = ProgramPane::spawn(
        std::sync::Arc::clone(&backend) as std::sync::Arc<dyn SessionBackend>,
        "tbp-test-exiting",
        "sh",
        &["-c".to_string(), "printf started; sleep 1".to_string()],
        Some(dir.path()),
        &HashMap::new(),
        24,
        80,
        thurbox::session::settings::Settings::default().scrollback_lines,
    );
    let pane = match pane {
        Ok(pane) => pane,
        Err(e) => {
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
    assert!(
        exited,
        "a program pane whose program exited still reports itself running; \
         nothing downstream can ever restart it"
    );
}
