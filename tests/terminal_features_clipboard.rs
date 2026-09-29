//! Configuring thurbox's tmux server leaves exactly one `*:clipboard` entry in
//! `terminal-features`, however often it runs (issue #1278).
//!
//! The session config is applied on every spawn and every startup, and the
//! server outlives thurbox, so an unconditional `set -as` grew the server-wide
//! list by one entry per run, without bound.
//!
//! Driven through the real headless spawn path on a throwaway socket, because
//! what is under test is what tmux ends up holding, not the string thurbox
//! would send.
//!
//! Skipped when tmux is absent: a missing multiplexer is an environment fact.

#![cfg(unix)]

use std::collections::HashMap;
use std::process::Command;

#[path = "support/tmux_server.rs"]
mod tmux_server;

use tmux_server::TmuxServer;

const SOCKET: &str = "thurbox-terminal-features-e2e";

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

/// Every entry of the server's `terminal-features`, in order.
fn terminal_features() -> Vec<String> {
    let out = tmux(&["show-options", "-sv", "terminal-features"]);
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::to_string)
        .collect()
}

fn clipboard_entries(features: &[String]) -> usize {
    features.iter().filter(|f| *f == "*:clipboard").count()
}

fn spawn(n: usize, dir: &std::path::Path) {
    let id = format!("11111111-1111-4111-8111-{n:012}");
    let spawned = thurbox::agent::tmux::spawn_window(
        &id,
        &format!("features-{n}"),
        "sh",
        &["-c".to_string(), "sleep 300".to_string()],
        Some(dir),
        &HashMap::new(),
    );
    match spawned {
        Ok(pane) if !pane.is_empty() => {}
        other => panic!("spawn {n} produced no pane: {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn repeated_setup_adds_clipboard_once_and_keeps_every_other_feature() {
    if !have_tmux() {
        eprintln!("skipping: tmux is not installed");
        return;
    }

    let dir = tempfile::tempdir().expect("tempdir");
    let _server = TmuxServer::pin(SOCKET);
    thurbox::paths::set_test_dir(dir.path());

    spawn(1, dir.path());
    // A feature the operator added by hand, which a fix must not disturb.
    tmux(&[
        "set-option",
        "-as",
        "terminal-features",
        ",xterm-ghostty:extkeys",
    ]);
    let before = terminal_features();
    assert_eq!(
        clipboard_entries(&before),
        1,
        "the first setup must add `*:clipboard`: {before:?}"
    );

    // Each spawn re-applies the config, exactly as a restart does.
    for n in 2..=4 {
        spawn(n, dir.path());
    }

    let after = terminal_features();
    assert_eq!(
        after, before,
        "re-applying the config must leave terminal-features as it was"
    );
}

/// A server a pre-fix thurbox already filled with duplicates is left as found:
/// the entries are identical and harmless, and the list is shared server state
/// thurbox does not own — so it stops growing rather than being rewritten.
#[tokio::test(flavor = "multi_thread")]
async fn existing_duplicates_stop_growing_and_are_not_removed() {
    if !have_tmux() {
        eprintln!("skipping: tmux is not installed");
        return;
    }

    let dir = tempfile::tempdir().expect("tempdir");
    let _server = TmuxServer::pin(SOCKET);
    thurbox::paths::set_test_dir(dir.path());

    // What a pre-fix thurbox left: the server started with tmux's defaults,
    // then one appended `*:clipboard` per run.
    tmux(&[
        "-f",
        "/dev/null",
        "start-server",
        ";",
        "new-session",
        "-d",
        "-s",
        "legacy",
    ]);
    for _ in 0..3 {
        tmux(&["set-option", "-as", "terminal-features", ",*:clipboard"]);
    }
    let staged = terminal_features();
    assert_eq!(
        clipboard_entries(&staged),
        3,
        "the test could not stage the state it is about: {staged:?}"
    );

    spawn(1, dir.path());
    let first = terminal_features();
    assert!(
        staged.iter().all(|f| first.contains(f)) && first.len() <= staged.len() + 1,
        "setup must keep every existing entry and add at most its own: \
         {staged:?} -> {first:?}"
    );

    spawn(2, dir.path());
    assert_eq!(terminal_features(), first, "the list must stop growing");
}
