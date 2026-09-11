//! Every window thurbox creates carries the `remain-on-exit` its role wants.
//!
//! The option is a **window** option, so `set-option -t <session>` never set it
//! for a session — tmux resolves that target down to the session's current
//! window (measured, tmux 3.2a). Which windows carried it was therefore an
//! accident of when `ensure_ready` last ran, and the two roles want opposite
//! answers:
//!
//! - an agent's window keeps its corpse, so the error it printed stays readable
//!   and the listing can still report `#{pane_dead}`;
//! - a plugin's program is read from its output *stream*, and tmux announces a
//!   pane's death only by closing its window — so a kept window is a death that
//!   is never announced, which is what "the editor hung" was.
//!
//! Asserted on a real tmux through the real spawn paths, because what is being
//! tested is the wiring: a helper returning the right string proves nothing
//! about which windows are told.
//!
//! Skipped when tmux is absent: a missing multiplexer is an environment fact.

#![cfg(unix)]

use std::collections::HashMap;
use std::process::Command;

use thurbox::kernel::terminal::{ProgramKey, Terminals};

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

fn cleanup() {
    let _ = tmux(&["kill-server"]);
}

/// What `remain-on-exit` says for the window holding `pane`, or `"<unset>"` when
/// the window carries no value of its own — which is the failure this guards:
/// an option nobody set is an option that was never inherited either.
fn remain_on_exit(pane: &str) -> String {
    let out = tmux(&["show-options", "-w", "-t", pane, "remain-on-exit"]);
    let text = String::from_utf8_lossy(&out.stdout);
    match text.split_whitespace().nth(1) {
        Some(value) => value.to_string(),
        None => "<unset>".to_string(),
    }
}

/// The pane of the one window whose name starts with `prefix`.
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
    std::env::set_var("TMUX_TMPDIR", dir.path());
    std::env::set_var(thurbox::agent::tmux::SOCKET_OVERRIDE_ENV, SOCKET);
    std::env::remove_var(thurbox::agent::tmux::SOCKET_OWNER_ENV);
    thurbox::paths::set_test_dir(dir.path());

    cleanup();

    // The agent, through the headless spawn path — which creates the session and
    // applies its options on the way, exactly as a restart does. A long-lived
    // program: one that exits before tmux finishes setting the window up turns a
    // real failure into a skip.
    let spawned = thurbox::agent::tmux::spawn_window(
        "11111111-1111-4111-8111-111111111111",
        "remain-on-exit",
        "sh",
        &["-c".to_string(), "sleep 300".to_string()],
        Some(dir.path()),
        &HashMap::new(),
    );
    let agent_pane = match spawned {
        Ok(pane) if !pane.is_empty() => pane,
        other => {
            cleanup();
            eprintln!("skipping: tmux would not spawn an agent window: {other:?}");
            return;
        }
    };

    // The plugin's program, through the control-mode path.
    let key = ProgramKey::new("plugins/90_files.lua", "editor_opts");
    let mut terminals = Terminals::new();
    if let Err(e) = terminals.start_program(
        &key,
        "sh",
        &["-c".to_string(), "sleep 300".to_string()],
        Some(dir.path()),
        24,
        80,
    ) {
        cleanup();
        eprintln!("skipping: tmux would not start a program pane: {e}");
        return;
    }
    let program_pane = pane_of("tbp-");

    let agent = remain_on_exit(&agent_pane);
    let program = program_pane.as_deref().map(remain_on_exit);
    cleanup();

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
