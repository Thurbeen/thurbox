#![cfg(unix)]

use std::collections::HashMap;
use std::process::Command;

use thurbox::kernel::terminal::{ProgramKey, Terminals};

#[path = "../../runtime-mux/tests/support/tmux_server.rs"]
mod tmux_server;

use tmux_server::TmuxServer;

const SOCKET: &str = "thurbox-remain-on-exit-e2e";

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

fn window_size(pane: Option<&str>) -> String {
    let out = match pane {
        Some(pane) => tmux(&["show-options", "-w", "-t", pane, "window-size"]),
        None => tmux(&["show-options", "-w", "-g", "window-size"]),
    };
    let text = String::from_utf8_lossy(&out.stdout);
    match text.split_whitespace().nth(1) {
        Some(value) => value.to_string(),
        None => "<unset>".to_string(),
    }
}

fn remain_on_exit(pane: &str) -> String {
    let out = tmux(&["show-options", "-w", "-t", pane, "remain-on-exit"]);
    let text = String::from_utf8_lossy(&out.stdout);
    match text.split_whitespace().nth(1) {
        Some(value) => value.to_string(),
        None => "<unset>".to_string(),
    }
}

fn pane_of(prefix: &str) -> Option<String> {
    let out = tmux(&["list-windows", "-a", "-F", "#{window_name} #{pane_id}"]);
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .find(|line| line.starts_with(prefix))
        .and_then(|line| line.split_whitespace().nth(1).map(str::to_string))
}

#[tokio::test(flavor = "multi_thread")]
async fn an_agent_window_keeps_its_corpse_and_a_program_window_does_not() {
    if !have_tmux() {
        eprintln!("skipping: tmux is not installed");
        return;
    }

    let dir = tempfile::tempdir().expect("tempdir");
    let _server = TmuxServer::pin(SOCKET);
    thurbox::paths::set_test_dir(dir.path());

    // The agent, through the headless spawn path — which creates the session and
    // applies its options on the way, exactly as a restart does. A long-lived
    // program: one that exits before tmux finishes setting the window up turns a
    // real failure into a skip.
    let spawned = thurbox::backend::WindowLifecycle::create_window(
        &thurbox::backend::tmux::TmuxBackend::local(&thurbox::backend::mux_settings(
            &Default::default(),
        )),
        &thurbox::backend::WindowSpec {
            owner: thurbox::backend::Owner::new(
                "11111111-1111-4111-8111-111111111111",
                "remain-on-exit",
            ),
            role: thurbox::backend::WindowRole::Agent,
            command: "sh",
            args: &["-c".to_string(), "sleep 300".to_string()],
            cwd: Some(dir.path()),
            env: &HashMap::new(),
        },
    );
    let agent_pane = match spawned {
        Ok(pane) if !pane.is_empty() => pane,
        other => {
            // Not a skip. tmux is installed — that was checked above — so a
            // spawn that produced no pane is the spawn path being broken, which
            // is half of what this file is about. A skip here would report the
            // regression as a clean run on a machine without a multiplexer.
            panic!("the agent window could not be spawned: {other:?}");
        }
    };

    let key = ProgramKey::new("mods/90_files.lua", "editor_opts");
    let mut terminals = Terminals::with_registry(
        std::sync::Arc::new(thurbox::backend::wiring::local_only(&Default::default())),
        &Default::default(),
    );
    if let Err(e) = terminals.start_program(
        &key,
        "sh",
        &["-c".to_string(), "sleep 300".to_string()],
        Some(dir.path()),
        24,
        80,
    ) {
        panic!("the program pane could not be started: {e}");
    }
    let program_pane = pane_of("tbp-");

    let agent = remain_on_exit(&agent_pane);
    let program = program_pane.as_deref().map(remain_on_exit);

    assert_eq!(
        agent, "on",
        "an agent's window must keep its corpse, so the error it died with stays \
         readable"
    );
    assert_eq!(
        program.as_deref(),
        Some("off"),
        "a program's window must close when its program ends, or the ending is \
         never announced"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn adopting_a_program_window_normalises_what_it_finds() {
    if !have_tmux() {
        eprintln!("skipping: tmux is not installed");
        return;
    }

    let dir = tempfile::tempdir().expect("tempdir");
    let _server = TmuxServer::pin(SOCKET);
    thurbox::paths::set_test_dir(dir.path());

    let key = ProgramKey::new("mods/90_files.lua", "editor_opts");
    let args = ["-c".to_string(), "sleep 300".to_string()];
    let mut first = Terminals::with_registry(
        std::sync::Arc::new(thurbox::backend::wiring::local_only(&Default::default())),
        &Default::default(),
    );
    if let Err(e) = first.start_program(&key, "sh", &args, Some(dir.path()), 24, 80) {
        panic!("the program pane could not be started: {e}");
    }
    let Some(pane) = pane_of("tbp-") else {
        panic!("the program pane was started and tmux lists no window for it");
    };

    // What an interface from before the per-window setting left behind.
    tmux(&["set-window-option", "-t", &pane, "remain-on-exit", "on"]);
    assert_eq!(
        remain_on_exit(&pane),
        "on",
        "the test could not stage the state it is about"
    );

    // A second interface over the same tmux: the window is found by name and
    // adopted, exactly as a restart does.
    drop(first);
    let mut second = Terminals::with_registry(
        std::sync::Arc::new(thurbox::backend::wiring::local_only(&Default::default())),
        &Default::default(),
    );
    let started = second.start_program(&key, "sh", &args, Some(dir.path()), 24, 80);
    let after = remain_on_exit(&pane);

    assert!(started.is_ok(), "adoption failed: {started:?}");
    assert_eq!(
        after, "off",
        "a program window adopted from an earlier run must be normalised, or its \
         exit stays unannounceable for as long as that window lives"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn an_agent_that_dies_at_once_still_leaves_its_window() {
    if !have_tmux() {
        eprintln!("skipping: tmux is not installed");
        return;
    }

    let dir = tempfile::tempdir().expect("tempdir");
    let _server = TmuxServer::pin(SOCKET);
    thurbox::paths::set_test_dir(dir.path());

    let spawned = thurbox::backend::WindowLifecycle::create_window(
        &thurbox::backend::tmux::TmuxBackend::local(&thurbox::backend::mux_settings(
            &Default::default(),
        )),
        &thurbox::backend::WindowSpec {
            owner: thurbox::backend::Owner::new(
                "22222222-2222-4222-8222-222222222222",
                "dies-at-once",
            ),
            role: thurbox::backend::WindowRole::Agent,
            command: "sh",
            args: &["-c".to_string(), "exit 7".to_string()],
            cwd: Some(dir.path()),
            env: &HashMap::new(),
        },
    );
    let pane = match spawned {
        Ok(pane) if !pane.is_empty() => pane,
        other => {
            panic!("the agent window could not be spawned: {other:?}");
        }
    };

    // Long enough for the corpse to be reaped if it was ever going to be.
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    let listed = pane_of("tb-");
    let retention = remain_on_exit(&pane);

    assert_eq!(
        listed.as_deref(),
        Some(pane.as_str()),
        "an agent window whose command exited at once must still be there — a \
         window that goes takes the error with it, and the server with it when \
         it was the last one"
    );
    assert_eq!(
        retention, "on",
        "and it must be there because it was told to keep its corpse, not by \
         luck of timing"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn an_older_namesake_does_not_take_the_new_windows_retention() {
    if !have_tmux() {
        eprintln!("skipping: tmux is not installed");
        return;
    }

    let dir = tempfile::tempdir().expect("tempdir");
    let _server = TmuxServer::pin(SOCKET);
    thurbox::paths::set_test_dir(dir.path());

    // The older namesake, from the other session that shares the name. Spawned
    // through the same path, so it is a real one rather than a hand-made window.
    let first = thurbox::backend::WindowLifecycle::create_window(
        &thurbox::backend::tmux::TmuxBackend::local(&thurbox::backend::mux_settings(
            &Default::default(),
        )),
        &thurbox::backend::WindowSpec {
            owner: thurbox::backend::Owner::new(
                "33333333-3333-4333-8333-333333333333",
                "same-name",
            ),
            role: thurbox::backend::WindowRole::Agent,
            command: "sh",
            args: &["-c".to_string(), "sleep 300".to_string()],
            cwd: Some(dir.path()),
            env: &HashMap::new(),
        },
    );
    if !matches!(&first, Ok(pane) if !pane.is_empty()) {
        panic!("the first window could not be spawned: {first:?}");
    }

    let second = thurbox::backend::WindowLifecycle::create_window(
        &thurbox::backend::tmux::TmuxBackend::local(&thurbox::backend::mux_settings(
            &Default::default(),
        )),
        &thurbox::backend::WindowSpec {
            owner: thurbox::backend::Owner::new(
                "44444444-4444-4444-8444-444444444444",
                "same-name",
            ),
            role: thurbox::backend::WindowRole::Agent,
            command: "sh",
            args: &["-c".to_string(), "exit 7".to_string()],
            cwd: Some(dir.path()),
            env: &HashMap::new(),
        },
    );
    let pane = match second {
        Ok(pane) if !pane.is_empty() => pane,
        other => {
            panic!("the second window could not be spawned: {other:?}");
        }
    };

    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    let retention = remain_on_exit(&pane);
    let listed = tmux(&["list-windows", "-a", "-F", "#{pane_id}"]);
    let alive = String::from_utf8_lossy(&listed.stdout)
        .lines()
        .any(|line| line == pane.as_str());

    assert!(
        alive,
        "the second window must survive its instant exit even though an older \
         window answers to the same name"
    );
    assert_eq!(
        retention, "on",
        "and the retention must have landed on it, not on its namesake"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_window_is_born_sized_by_hand_and_the_server_is_not() {
    if !have_tmux() {
        eprintln!("skipping: tmux is not installed");
        return;
    }

    let dir = tempfile::tempdir().expect("tempdir");
    let _server = TmuxServer::pin(SOCKET);
    thurbox::paths::set_test_dir(dir.path());

    let spawned = thurbox::backend::WindowLifecycle::create_window(
        &thurbox::backend::tmux::TmuxBackend::local(&thurbox::backend::mux_settings(
            &Default::default(),
        )),
        &thurbox::backend::WindowSpec {
            owner: thurbox::backend::Owner::new(
                "55555555-5555-4555-8555-555555555555",
                "hand-sized",
            ),
            role: thurbox::backend::WindowRole::Agent,
            command: "sh",
            args: &["-c".to_string(), "sleep 300".to_string()],
            cwd: Some(dir.path()),
            env: &HashMap::new(),
        },
    );
    let pane = match spawned {
        Ok(pane) if !pane.is_empty() => pane,
        other => {
            panic!("the agent window could not be spawned: {other:?}");
        }
    };

    let window = window_size(Some(&pane));
    let server = window_size(None);

    assert_eq!(
        window, "manual",
        "a window thurbox creates must size itself rather than follow the \
         smallest attached client"
    );
    assert_ne!(
        server, "manual",
        "and the server must not, or the next window creation from an \
         unattached client takes the whole server down (tmux 3.3 … 3.6)"
    );
}
