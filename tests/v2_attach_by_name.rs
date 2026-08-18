//! Attaching to a session whose pane id the database does not carry.
//!
//! This is the gap that made the creation flow useless: a **local** spawn leaves
//! `backend_id` empty on purpose — "for the TUI to resolve by name", which is
//! what v1 does when it adopts windows at restore — so a session created by v2
//! had a running agent, a real tmux window, and an interface insisting it had
//! "no pane yet".
//!
//! Driven against a real tmux server on a private socket, because the thing under
//! test is precisely the round trip: a window exists, its name is derivable from
//! the session's name, and the pane behind it has to be found. Skipped where tmux
//! is not installed, like `tests/create_e2e.rs`.

use std::process::Command;

use thurbox::kernel::snapshot::{SessionRow, Snapshot};
use thurbox::kernel::terminal::Terminals;

/// A socket of this test's own, so it can never see — or kill — a real session.
const SOCKET: &str = "thurbox-attach-test";

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

/// The tmux session name the local backend groups its windows under. Mirrors
/// `agent::tmux::TMUX_SESSION`, which is private — and is `thurbox-dev` here,
/// because a test build carries the same `dev_build` marker a dev binary does.
const SESSION: &str = "thurbox-dev";

fn row(name: &str) -> SessionRow {
    SessionRow {
        id: "11111111-1111-1111-1111-111111111111".into(),
        name: name.into(),
        agent: "claude".into(),
        status: "idle".into(),
        cwd: None,
        repo: None,
        repos: Vec::new(),
        branch: None,
        base_branch: None,
        backend: "local-tmux".into(),
        // The whole point: the database has no pane id for it.
        backend_id: None,
        remote_host: None,
        agent_session_id: None,
        parent_id: None,
        display_order: None,
        worktree_count: 0,
        git: None,
        hook_state: None,
        shell_backend_id: None,
        member_dirs: Vec::new(),
    }
}

fn snapshot(rows: Vec<SessionRow>) -> Snapshot {
    Snapshot {
        sessions: rows,
        ..Snapshot::default()
    }
}

/// A tokio runtime is required, not decorative: adopting a pane spawns its reader
/// on `spawn_blocking`.
#[tokio::test(flavor = "multi_thread")]
async fn a_session_with_no_pane_id_is_found_by_its_window_name() {
    if !have_tmux() {
        eprintln!("skipping: tmux is not installed");
        return;
    }
    // Private socket directory as well as a private socket name: the sandbox
    // pattern, so nothing here can reach a real thurbox server.
    let home = tempfile::tempdir().expect("tempdir");
    std::env::set_var("TMUX_TMPDIR", home.path());
    std::env::set_var(thurbox::agent::tmux::SOCKET_OVERRIDE_ENV, SOCKET);

    tmux(&["new-session", "-d", "-s", SESSION, "-n", "bash", "sh"]);
    // The window a session named `demo` produces: `tb-demo`.
    tmux(&[
        "new-window",
        "-t",
        SESSION,
        "-n",
        "tb-demo",
        "sh -c 'while :; do sleep 1; done'",
    ]);

    let mut terminals = Terminals::new();
    let rows = snapshot(vec![row("demo")]);
    // Attaching runs on a worker, so it lands on a later sync rather than this
    // one — which is the point: the loop keeps painting while a host is being
    // reached.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    let mut attached = false;
    while std::time::Instant::now() < deadline && !attached {
        terminals.sync(&rows, 24, 80);
        attached = terminals.is_attached("11111111-1111-1111-1111-111111111111");
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    let failure = terminals
        .failure("11111111-1111-1111-1111-111111111111")
        .unwrap_or_default()
        .to_string();
    tmux(&["kill-server"]);

    assert!(
        attached,
        "a session whose window exists must be attached, not reported as paneless: {failure}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_window_that_appears_later_is_still_picked_up() {
    // The second half of the bug: the failure used to be memoised forever, so a
    // session created *while the interface was running* — which is every session
    // the flow creates — never attached even once its window existed.
    if !have_tmux() {
        eprintln!("skipping: tmux is not installed");
        return;
    }
    let home = tempfile::tempdir().expect("tempdir");
    std::env::set_var("TMUX_TMPDIR", home.path());
    std::env::set_var(thurbox::agent::tmux::SOCKET_OVERRIDE_ENV, SOCKET);

    tmux(&["new-session", "-d", "-s", SESSION, "-n", "bash", "sh"]);

    let mut terminals = Terminals::new();
    let rows = snapshot(vec![row("demo")]);
    terminals.sync(&rows, 24, 80);
    assert!(
        !terminals.is_attached("11111111-1111-1111-1111-111111111111"),
        "nothing to attach to yet"
    );

    tmux(&[
        "new-window",
        "-t",
        SESSION,
        "-n",
        "tb-demo",
        "sh -c 'while :; do sleep 1; done'",
    ]);

    // Discovery is throttled, so the next sync that may resolve it is one
    // interval away — the loop calls this every frame either way.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while std::time::Instant::now() < deadline {
        terminals.sync(&rows, 24, 80);
        if terminals.is_attached("11111111-1111-1111-1111-111111111111") {
            tmux(&["kill-server"]);
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    let failure = terminals
        .failure("11111111-1111-1111-1111-111111111111")
        .unwrap_or_default()
        .to_string();
    tmux(&["kill-server"]);
    panic!("the window appeared but was never attached: {failure}");
}

#[test]
fn a_remote_row_is_not_resolved_by_name() {
    // A remote spawn records its real pane id, so a remote row without one cannot
    // be fixed by discovery — and readying a remote backend to try would put an
    // ssh connect on the render thread.
    let mut terminals = Terminals::new();
    let mut remote = row("demo");
    remote.backend = "ssh:nowhere".into();
    let started = std::time::Instant::now();
    terminals.sync(&snapshot(vec![remote]), 24, 80);
    assert!(
        started.elapsed() < std::time::Duration::from_secs(2),
        "syncing must not wait on a host"
    );
    assert_eq!(
        terminals.failure("11111111-1111-1111-1111-111111111111"),
        Some("session has no pane yet")
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn two_windows_of_the_same_name_are_refused_rather_than_guessed() {
    // Sending keystrokes to the wrong agent is worse than a pane that explains
    // itself, so an ambiguous name resolves to nothing.
    if !have_tmux() {
        eprintln!("skipping: tmux is not installed");
        return;
    }
    let home = tempfile::tempdir().expect("tempdir");
    std::env::set_var("TMUX_TMPDIR", home.path());
    std::env::set_var(thurbox::agent::tmux::SOCKET_OVERRIDE_ENV, SOCKET);

    tmux(&["new-session", "-d", "-s", SESSION, "-n", "bash", "sh"]);
    for _ in 0..2 {
        tmux(&[
            "new-window",
            "-t",
            SESSION,
            "-n",
            "tb-demo",
            "sh -c 'while :; do sleep 1; done'",
        ]);
    }

    let mut terminals = Terminals::new();
    terminals.sync(&snapshot(vec![row("demo")]), 24, 80);
    let attached = terminals.is_attached("11111111-1111-1111-1111-111111111111");
    tmux(&["kill-server"]);
    assert!(!attached, "an ambiguous window name must not be guessed at");
}
