//! Relaunching every session at once, onto a tmux server that is not there yet.
//!
//! This is the first thing thurbox does on a machine that has just rebooted:
//! `missing_agents` finds every session unplaced and `respawn_missing_agents`
//! hands each one to its own worker, so N spawns arrive together at a server
//! with no thurbox session on it. `ensure_session_configured` used to check
//! `session_exists()` and then create — which is not a lock, so every worker
//! saw "no session", every worker ran `new-session`, one won and the rest were
//! told `duplicate session`. A loser's whole respawn aborted: no window, and a
//! row still naming the pane id the *dead* server had given it — an id the new
//! server had by then reissued to whichever session won. That session sat with
//! no agent, and because `respawned` marks a session the moment it dispatches,
//! nothing tried again for the life of the process.
//!
//! Driven against a real tmux server on a private socket, because the race is
//! in tmux's answer and nothing in-process can stand in for it. Skipped where
//! tmux is not installed, like `tests/attach_by_name.rs`.

use std::collections::HashMap;
use std::process::Command;

/// A socket of this test's own, so it can never see — or kill — a real session.
const SOCKET: &str = "thurbox-respawn-test";

/// How many sessions come back at once. Six is the shape the defect was found
/// in and enough for every thread to reach `new-session` together; the loser
/// count does not change the assertion, which is that there are none.
const SESSIONS: usize = 6;

fn have_tmux() -> bool {
    Command::new("tmux")
        .arg("-V")
        .output()
        .map(|out| out.status.success())
        .unwrap_or(false)
}

fn tmux(args: &[&str]) -> std::process::Output {
    Command::new("tmux")
        .arg("-L")
        .arg(SOCKET)
        .args(args)
        .output()
        .expect("run tmux")
}

#[test]
fn every_session_relaunching_at_once_gets_its_own_window() {
    if !have_tmux() {
        eprintln!("skipping: tmux is not installed");
        return;
    }
    // Private socket directory as well as a private socket name: the sandbox
    // pattern, so nothing here can reach a real thurbox server.
    let home = tempfile::tempdir().expect("tempdir");
    std::env::set_var("TMUX_TMPDIR", home.path());
    std::env::set_var(thurbox::agent::tmux::SOCKET_OVERRIDE_ENV, SOCKET);
    // No server and no session: the state a reboot leaves behind, and the only
    // state in which the create races at all.
    tmux(&["kill-server"]);

    let barrier = std::sync::Arc::new(std::sync::Barrier::new(SESSIONS));
    let spawns: Vec<_> = (0..SESSIONS)
        .map(|i| {
            let barrier = std::sync::Arc::clone(&barrier);
            std::thread::spawn(move || {
                let id = format!("00000000-0000-0000-0000-00000000000{i}");
                let name = format!("relaunch-{i}");
                // Released together, so the `session_exists()` checks overlap
                // instead of being serialised by thread startup.
                barrier.wait();
                thurbox::agent::tmux::spawn_window(
                    &id,
                    &name,
                    "sh",
                    &["-c".to_string(), "while :; do sleep 1; done".to_string()],
                    None,
                    &HashMap::new(),
                )
                .map_err(|e| format!("{name}: {e:#}"))
            })
        })
        .collect();

    let outcomes: Vec<Result<String, String>> = spawns
        .into_iter()
        .map(|t| t.join().expect("join"))
        .collect();
    let windows =
        String::from_utf8_lossy(&tmux(&["list-windows", "-a", "-F", "#{window_name}"]).stdout)
            .to_string();
    tmux(&["kill-server"]);

    let refused: Vec<&String> = outcomes.iter().filter_map(|o| o.as_ref().err()).collect();
    assert!(
        refused.is_empty(),
        "{} of {SESSIONS} relaunches were refused; each is a session left with no \
         agent and a pane id the new server has given to somebody else:\n{}",
        refused.len(),
        refused
            .iter()
            .map(|e| e.as_str())
            .collect::<Vec<_>>()
            .join("\n")
    );
    for i in 0..SESSIONS {
        assert!(
            windows.lines().any(|w| w == format!("tb-relaunch-{i}")),
            "relaunch-{i} reported success but has no window; server holds:\n{windows}"
        );
    }
}
