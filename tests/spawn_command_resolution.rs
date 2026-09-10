//! A local spawn must launch the agent **thurbox** resolved, not whatever the
//! multiplexer's own `PATH` happens to resolve.
//!
//! thurbox used to hand tmux a bare command name (`claude`) and let tmux
//! resolve it. Which resolver ran, and with which `PATH`, was not thurbox's to
//! choose: tmux copies the *client's* `PATH` into the new pane only for an
//! **unattached** client (`spawn.c`: "the session one is replaced from the
//! client ... only unattached clients"). thurbox's control-mode client is
//! attached, so its windows got the `PATH` of whatever first started the tmux
//! **server** — and a single-token command is handed to that server's
//! `default-shell` rather than `execvp`, so the resolver could be a shell
//! thurbox never chose.
//!
//! Under zsh/bash the two `PATH`s agree, because the interactive additions live
//! in `~/.zshenv` / `~/.profile`, which any shell that starts a server sources.
//! Under **fish** they do not: `fish_add_path` writes `fish_user_paths`, which
//! only fish applies — so a server started from anything else never sees them,
//! for the life of that server.
//!
//! Skipped when tmux is absent rather than failing: a missing multiplexer is an
//! environment fact, not a regression.

#![cfg(unix)]

use std::collections::HashMap;
use std::process::Command;

use thurbox::agent::backend::SessionBackend;
use thurbox::agent::tmux::{TmuxBackend, SOCKET_OVERRIDE_ENV, SOCKET_OWNER_ENV};

/// A throwaway socket, so this never touches the real one.
const SOCKET: &str = "thurbox-spawn-cmd-e2e";

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

#[test]
fn a_local_spawn_finds_the_agent_the_multiplexer_cannot() {
    if !have_tmux() {
        eprintln!("skipping: tmux is not installed");
        return;
    }

    let dir = tempfile::tempdir().expect("tempdir");
    let bin = dir.path().join("bin");
    std::fs::create_dir_all(&bin).expect("bin dir");
    let marker = dir.path().join("agent-ran");
    let agent = bin.join("tb-probe-agent");
    std::fs::write(
        &agent,
        format!(
            "#!/bin/sh\nprintf ok > {}\nsleep 30\n",
            marker.to_string_lossy()
        ),
    )
    .expect("write probe agent");
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&agent, std::fs::Permissions::from_mode(0o700)).expect("chmod");
    }

    // nextest runs one process per test, so process-wide env is safe here.
    std::env::set_var("TMUX_TMPDIR", dir.path());
    std::env::set_var(SOCKET_OVERRIDE_ENV, SOCKET);
    // The override is dropped when it was injected for someone else's data dir
    // (see `socket_for`); this test *is* somebody typing it.
    std::env::remove_var(SOCKET_OWNER_ENV);
    thurbox::paths::set_test_dir(dir.path());

    cleanup();
    // The server starts without the agent's directory on `PATH` — the shape a
    // fish user's machine is in whenever the server was started by anything
    // that is not fish.
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
        .env("PATH", "/usr/bin:/bin")
        .env("TMUX_TMPDIR", dir.path())
        .output()
        .expect("run tmux");
    if !started.status.success() {
        cleanup();
        eprintln!(
            "skipping: tmux would not start a server: {}",
            String::from_utf8_lossy(&started.stderr).trim()
        );
        return;
    }

    // thurbox's own `PATH` *does* have it: this is the user's interactive PATH,
    // the one the agent was installed onto.
    let path = format!(
        "{}:{}",
        bin.to_string_lossy(),
        std::env::var("PATH").unwrap_or_default()
    );
    std::env::set_var("PATH", &path);

    let backend = TmuxBackend::local();
    if let Err(e) = backend.ensure_ready() {
        cleanup();
        eprintln!("skipping: tmux control mode would not start: {e:#}");
        return;
    }
    let spawned = backend.spawn(
        "tb-probe",
        "tb-probe-agent",
        &[],
        Some(dir.path()),
        &HashMap::new(),
        24,
        80,
    );
    let spawned = match spawned {
        Ok(s) => s,
        Err(e) => {
            cleanup();
            panic!("the spawn itself failed: {e:#}");
        }
    };

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while std::time::Instant::now() < deadline && !marker.exists() {
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    let ran = marker.exists();
    drop(spawned);
    cleanup();

    assert!(
        ran,
        "the agent thurbox resolved on its own PATH never ran: the window command \
         was left for the multiplexer to resolve, and the multiplexer's PATH does \
         not have it"
    );
}
