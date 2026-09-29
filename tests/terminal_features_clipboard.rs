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

/// A tmux command that sets up the state a test is about, which must succeed:
/// a staging step that failed silently would let the assertion after it pass
/// against a state nobody staged.
fn stage(server: &TmuxServer, args: &[&str]) {
    let out = server.tmux(args);
    assert!(
        out.status.success(),
        "staging `tmux {}` failed: {}",
        args.join(" "),
        String::from_utf8_lossy(&out.stderr)
    );
}

/// A server started the way a pre-fix thurbox or the operator's own config
/// would leave it, before thurbox touches it.
fn start_bare(server: &TmuxServer) {
    stage(
        server,
        &["-f", "/dev/null", "new-session", "-d", "-s", "bare"],
    );
}

/// Every entry of the server's `terminal-features`, in order.
fn terminal_features(server: &TmuxServer) -> Vec<String> {
    let out = server.tmux(&["show-options", "-sv", "terminal-features"]);
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
    let server = TmuxServer::pin(SOCKET);
    thurbox::paths::set_test_dir(dir.path());

    spawn(1, dir.path());
    // A feature the operator added by hand, which a fix must not disturb.
    stage(
        &server,
        &[
            "set-option",
            "-as",
            "terminal-features",
            ",xterm-ghostty:extkeys",
        ],
    );
    let before = terminal_features(&server);
    assert_eq!(
        clipboard_entries(&before),
        1,
        "the first setup must add `*:clipboard`: {before:?}"
    );
    assert!(
        before.iter().any(|f| f == "xterm-ghostty:extkeys"),
        "the test could not stage the feature it is about: {before:?}"
    );

    // Each spawn re-applies the config, exactly as a restart does.
    for n in 2..=4 {
        spawn(n, dir.path());
    }

    let after = terminal_features(&server);
    assert_eq!(
        after, before,
        "re-applying the config must leave terminal-features as it was"
    );
}

/// The slot thurbox writes is one a user's `~/.tmux.conf` — which thurbox's
/// server reads — could have claimed too. Theirs wins: thurbox's entry is only
/// written into an empty slot.
#[tokio::test(flavor = "multi_thread")]
async fn a_slot_the_user_already_set_is_left_alone() {
    if !have_tmux() {
        eprintln!("skipping: tmux is not installed");
        return;
    }

    let dir = tempfile::tempdir().expect("tempdir");
    let server = TmuxServer::pin(SOCKET);
    thurbox::paths::set_test_dir(dir.path());

    start_bare(&server);
    stage(
        &server,
        &[
            "set-option",
            "-s",
            "terminal-features[100]",
            "xterm-kitty:title",
        ],
    );
    let before = terminal_features(&server);

    spawn(1, dir.path());
    spawn(2, dir.path());

    assert_eq!(terminal_features(&server), before);
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
    let server = TmuxServer::pin(SOCKET);
    thurbox::paths::set_test_dir(dir.path());

    // One appended `*:clipboard` per pre-fix run.
    start_bare(&server);
    for _ in 0..3 {
        stage(
            &server,
            &["set-option", "-as", "terminal-features", ",*:clipboard"],
        );
    }
    let staged = terminal_features(&server);
    assert_eq!(
        clipboard_entries(&staged),
        3,
        "the test could not stage the state it is about: {staged:?}"
    );

    spawn(1, dir.path());
    let first = terminal_features(&server);
    assert!(
        staged.iter().all(|f| first.contains(f)) && first.len() <= staged.len() + 1,
        "setup must keep every existing entry and add at most its own: \
         {staged:?} -> {first:?}"
    );

    spawn(2, dir.path());
    assert_eq!(
        terminal_features(&server),
        first,
        "the list must stop growing"
    );
}
