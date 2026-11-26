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

// Blocking calls are safe: tokio multi_thread runs the test body on its own thread.
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

    // tmux drops the pane of a program that exits before the window is sized, so it lives a moment.
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
            panic!("the program pane could not be spawned with tmux installed: {e:#}");
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
