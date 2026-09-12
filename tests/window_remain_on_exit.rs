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
            // Not a skip. tmux is installed — that was checked above — so a
            // spawn that produced no pane is the spawn path being broken, which
            // is half of what this file is about. A skip here would report the
            // regression as a clean run on a machine without a multiplexer.
            panic!("the agent window could not be spawned: {other:?}");
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
        panic!("the program pane could not be started: {e}");
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

/// A window adopted from an earlier run is normalised, not taken as found.
///
/// The restart path finds a program window by its deterministic name and
/// reconnects to it. That window was created by some *earlier* interface —
/// possibly one that set `remain-on-exit` session-wide and landed it on
/// whichever window was current. Left as found, an editor window carrying `on`
/// from that era is a pane whose exit can never be announced: the corpse comes
/// straight back on the first restart after an upgrade, which is the one moment
/// this whole change exists to fix.
///
/// Driven through the real adoption path rather than by calling the helper: the
/// claim is that adopting *reaches* it.
#[tokio::test(flavor = "multi_thread")]
async fn adopting_a_program_window_normalises_what_it_finds() {
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

    let key = ProgramKey::new("plugins/90_files.lua", "editor_opts");
    let args = ["-c".to_string(), "sleep 300".to_string()];
    let mut first = Terminals::new();
    if let Err(e) = first.start_program(&key, "sh", &args, Some(dir.path()), 24, 80) {
        cleanup();
        panic!("the program pane could not be started: {e}");
    }
    let Some(pane) = pane_of("tbp-") else {
        cleanup();
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
    let mut second = Terminals::new();
    let started = second.start_program(&key, "sh", &args, Some(dir.path()), 24, 80);
    let after = remain_on_exit(&pane);
    cleanup();

    assert!(started.is_ok(), "adoption failed: {started:?}");
    assert_eq!(
        after, "off",
        "a program window adopted from an earlier run must be normalised, or its \
         exit stays unannounceable for as long as that window lives"
    );
}

/// An agent whose command exits **instantly** still leaves a window behind.
///
/// The option is not a message of its own: a window is born with the
/// server-wide default (`off`), and a command that has already exited by the
/// time a second message arrives takes its window with it — and the server too,
/// when it was the last window. That is what CI reported on this branch, twice,
/// as `tmux new-window exited exit status: 1 for window tb-flow: server exited
/// unexpectedly`, on the *second* install of a test whose agent binary does not
/// exist on the runner.
///
/// Deterministic without the fix, which is why it is worth having: five windows
/// created this way with the option sent afterwards were gone every time
/// (measured, tmux 3.2a, `no such window`). `exit 7` stands in for the real
/// cause — a missing agent binary, a supported state since #1104.
#[tokio::test(flavor = "multi_thread")]
async fn an_agent_that_dies_at_once_still_leaves_its_window() {
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

    let spawned = thurbox::agent::tmux::spawn_window(
        "22222222-2222-4222-8222-222222222222",
        "dies-at-once",
        "sh",
        &["-c".to_string(), "exit 7".to_string()],
        Some(dir.path()),
        &HashMap::new(),
    );
    let pane = match spawned {
        Ok(pane) if !pane.is_empty() => pane,
        other => {
            cleanup();
            panic!("the agent window could not be spawned: {other:?}");
        }
    };

    // Long enough for the corpse to be reaped if it was ever going to be.
    std::thread::sleep(std::time::Duration::from_millis(500));
    let listed = pane_of("tb-");
    let retention = remain_on_exit(&pane);
    cleanup();

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

/// And it still leaves it when an older window already answers to its name.
///
/// `tb-<session name>` is not unique — two sessions can share a name, which is
/// what the `@thurbox_session` stamp exists for — and tmux resolves a duplicate
/// name to the **lowest index**, which is the older window (measured, tmux
/// 3.2a). A retention chained by name would therefore land on the wrong window
/// and leave the new one with the server-wide `off`, which is the failure this
/// file is about, one collision away.
#[tokio::test(flavor = "multi_thread")]
async fn an_older_namesake_does_not_take_the_new_windows_retention() {
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

    // The older namesake, from the other session that shares the name. Spawned
    // through the same path, so it is a real one rather than a hand-made window.
    let first = thurbox::agent::tmux::spawn_window(
        "33333333-3333-4333-8333-333333333333",
        "same-name",
        "sh",
        &["-c".to_string(), "sleep 300".to_string()],
        Some(dir.path()),
        &HashMap::new(),
    );
    if !matches!(&first, Ok(pane) if !pane.is_empty()) {
        cleanup();
        panic!("the first window could not be spawned: {first:?}");
    }

    let second = thurbox::agent::tmux::spawn_window(
        "44444444-4444-4444-8444-444444444444",
        "same-name",
        "sh",
        &["-c".to_string(), "exit 7".to_string()],
        Some(dir.path()),
        &HashMap::new(),
    );
    let pane = match second {
        Ok(pane) if !pane.is_empty() => pane,
        other => {
            cleanup();
            panic!("the second window could not be spawned: {other:?}");
        }
    };

    std::thread::sleep(std::time::Duration::from_millis(500));
    let retention = remain_on_exit(&pane);
    let listed = tmux(&["list-windows", "-a", "-F", "#{pane_id}"]);
    let alive = String::from_utf8_lossy(&listed.stdout)
        .lines()
        .any(|line| line == pane);
    cleanup();

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
