//! The stream pane end to end: thurbox opens a session's pane through
//! `runtime-mux view`, a process speaking the terminal stream, and gets the
//! chunks the control-mode pane gives — the screen as a snapshot, then its
//! output — while what is typed goes up the stream.
//!
//! thurbox does not link the view: the program is found beside this test in
//! cargo's target directory (a workspace build makes it). Without it or
//! without tmux the test says so and skips.

#![cfg(unix)]

use std::io::Write;
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use thurbox::backend::PaneChunk;

#[path = "../../runtime-mux/tests/support/tmux_server.rs"]
mod tmux_server;

fn runtime_mux() -> Option<PathBuf> {
    let found = std::env::current_exe()
        .ok()
        .and_then(|exe| Some(exe.parent()?.parent()?.join("runtime-mux")))
        .filter(|p| p.is_file());
    if found.is_none() {
        assert!(
            std::env::var_os("CI").is_none(),
            "no runtime-mux binary in CI, where the stream pane must be run"
        );
        eprintln!("skipping: no runtime-mux binary; build it with `cargo build -p runtime-mux`");
    }
    found
}

#[test]
fn a_stream_pane_shows_the_screen_and_takes_input() {
    if thurbox::paths::resolve_on_path("tmux").is_none() {
        eprintln!("skipping: tmux is not installed");
        return;
    }
    let Some(program) = runtime_mux() else {
        return;
    };
    let server = tmux_server::TmuxServer::private("thurbox-stream-pane");
    // Started the way the runtime starts a session, so the window sits where
    // the view's control-mode client reads it.
    let started = std::process::Command::new(&program)
        .arg("start")
        .env("TMUX_TMPDIR", server.tmpdir())
        .env("RUNTIME_MUX_SOCKET", server.socket())
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .and_then(|mut child| {
            let request = serde_json::json!({
                "protocol": 1,
                "key": "stream-pane",
                "argv": ["sh", "-c", "echo ready-marker; while IFS= read -r l; do echo \"pong-$l\"; done"],
            });
            child
                .stdin
                .take()
                .expect("stdin")
                .write_all(request.to_string().as_bytes())?;
            child.wait_with_output()
        })
        .expect("runtime-mux start");
    let answer: serde_json::Value = serde_json::from_slice(&started.stdout).expect("JSON");
    let pane = answer["known"]["started"]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("a started pane: {answer}"))
        .to_string();

    let argv: Vec<String> = [
        program.to_string_lossy().as_ref(),
        "view",
        "--socket",
        server.socket(),
        "--scrollback",
        "2000",
        "--tmux-tmpdir",
        &server.tmpdir().to_string_lossy(),
        "--pane",
        &pane,
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    let mut adopted =
        thurbox::kernel::terminal::stream_pane::adopt(&argv, 24, 80).expect("the stream opens");
    let output = std::mem::replace(&mut adopted.output, Box::new(std::iter::empty()));
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for chunk in output {
            if tx.send(chunk).is_err() {
                break;
            }
        }
    });
    let mut text = String::new();
    let mut snapshot = false;
    let read_until = |needle: &str, text: &mut String, snapshot: &mut bool| {
        let deadline = Instant::now() + Duration::from_secs(30);
        while !text.contains(needle) {
            let left = deadline.saturating_duration_since(Instant::now());
            match rx.recv_timeout(left) {
                Ok(PaneChunk::Snapshot(s)) => {
                    *snapshot = true;
                    text.push_str(&s.normal.join("\n"));
                }
                Ok(PaneChunk::Output(bytes)) => text.push_str(&String::from_utf8_lossy(&bytes)),
                Ok(_) => {}
                Err(e) => panic!("no {needle:?} ({e}) in: {text}"),
            }
        }
    };
    read_until("ready-marker", &mut text, &mut snapshot);
    assert!(snapshot, "the screen came as a snapshot first: {text}");
    adopted.input.write_all(b"ping\r").unwrap();
    adopted.input.flush().unwrap();
    read_until("pong-ping", &mut text, &mut snapshot);
}
