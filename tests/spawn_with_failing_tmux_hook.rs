//! A user hook on the tmux server cannot cost thurbox a window it created —
//! an agent's, or the automation heartbeat keeper's.
//!
//! tmux hands a command-mode client the exit status of the last `run-shell` its
//! command list triggered — hooks included. An `after-new-window` hook left
//! behind by an uninstalled tmux plugin runs a script that is no longer there,
//! `/bin/sh` answers 127, and the *client* exits 127 although `new-window`
//! itself succeeded and printed the pane id. Reading that status as the verdict
//! is what reported `Failed to spawn tmux window: tmux new-window exited exit
//! status: 127` on a server that had just created the window (issue #1154), and
//! the hook's own output — printed on the same stream as the `-P` answer — is
//! what corrupts the id if stdout is taken whole.
//!
//! Asserted against a real tmux, because what is being tested is which of the
//! two answers thurbox believes.
//!
//! Its own binary and its own socket because the hook it installs is
//! **server-global**: borrowing another suite's socket would leave every spawn
//! in it answering 127 for reasons of its own.
//!
//! Each test gets its own server, not a shared one: `cargo nextest` runs every
//! test in its own process and each `TmuxServer` guard gives it a socket
//! directory of its own, so the socket *name* they share resolves to a
//! different path per test — the same shape as
//! `tests/window_remain_on_exit.rs`. Under plain `cargo test` that does not
//! hold: one process, one `TMUX_TMPDIR`, and these tests will reap each
//! other's server. Run them with nextest, which is what `.publish.yaml`, CI
//! and the pre-commit hook all use.
//!
//! Skipped when tmux is absent: a missing multiplexer is an environment fact.

#![cfg(unix)]

use std::collections::HashMap;
use std::process::Command;

/// The guard every tmux server in this file is reaped by — see its own doc.
#[path = "support/tmux_server.rs"]
mod tmux_server;

use tmux_server::TmuxServer;

const SOCKET: &str = "thurbox-failing-hook-e2e";
const SESSION_ID: &str = "11111111-1111-4111-8111-111111111111";

/// The hook an uninstalled plugin leaves behind: a script that is no longer on
/// the disk, which the server keeps calling for the rest of its life.
const DEAD_HOOK: &str = "run-shell 'thurbox-uninstalled-plugin-script'";

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

/// What `option` says for the window holding `pane`, or `"<unset>"` when the
/// window carries no value of its own.
fn window_option(pane: &str, option: &str) -> String {
    let out = tmux(&["show-options", "-w", "-t", pane, option]);
    let text = String::from_utf8_lossy(&out.stdout);
    match text.split_whitespace().nth(1) {
        Some(value) => value.to_string(),
        None => "<unset>".to_string(),
    }
}

fn window_names() -> Vec<String> {
    let out = tmux(&["list-windows", "-a", "-F", "#{window_name}"]);
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::to_string)
        .collect()
}

/// A long-lived program: one that exits before tmux finishes setting the window
/// up would turn a real failure into a passing run.
fn spawn(name: &str, cwd: &std::path::Path) -> anyhow::Result<String> {
    thurbox::agent::tmux::spawn_window(
        SESSION_ID,
        name,
        "sh",
        &["-c".to_string(), "sleep 300".to_string()],
        Some(cwd),
        &HashMap::new(),
    )
}

/// A server the hook has been installed on — and the control that says the same
/// spawn succeeds on that server without it, so a failure below is the hook and
/// nothing else. (The server has to exist before a hook can be set on it, and
/// the first spawn is what creates it.)
fn server_with_a_dead_hook(dir: &std::path::Path) -> TmuxServer {
    let server = TmuxServer::pin(SOCKET);
    thurbox::paths::set_test_dir(dir);

    if let Err(e) = spawn("clean", dir) {
        panic!("the control window could not be spawned on a clean server: {e:#}");
    }
    tmux(&["set-hook", "-g", "after-new-window", DEAD_HOOK]);
    server
}

#[test]
fn a_dead_plugin_hook_does_not_fail_a_window_that_was_created() {
    if !have_tmux() {
        eprintln!("skipping: tmux is not installed");
        return;
    }

    let dir = tempfile::tempdir().expect("tempdir");
    let _server = server_with_a_dead_hook(dir.path());

    let spawned = spawn("hostile", dir.path());
    let names = window_names();

    let pane = match spawned {
        Ok(pane) => pane,
        Err(e) => panic!(
            "a window tmux created was reported as a failed spawn because a user \
             hook exited non-zero: {e:#}"
        ),
    };
    let digits = pane.strip_prefix('%').unwrap_or_default();
    assert!(
        !digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit()),
        "the spawn must keep the pane id tmux printed and not the hook's output \
         alongside it: {pane:?}"
    );
    assert!(
        names.iter().any(|n| n == "tb-hostile"),
        "the window tmux created must still be there — it was reported as a \
         failure and torn down: {names:?}"
    );
}

/// The other half: the id the spawn kept must be the window's own.
///
/// An id that is not one is not a refusal, which is what makes it worse: every
/// later lookup simply targets nothing and the session's stamp lands nowhere,
/// so a window that is running is one thurbox can never find again.
#[test]
fn the_id_kept_from_a_hooked_spawn_still_names_the_window() {
    if !have_tmux() {
        eprintln!("skipping: tmux is not installed");
        return;
    }

    let dir = tempfile::tempdir().expect("tempdir");
    let _server = server_with_a_dead_hook(dir.path());

    let spawned = spawn("stamped", dir.path());
    let found = spawned.as_ref().ok().map(|pane| {
        let out = tmux(&["display-message", "-p", "-t", pane, "#{window_name}"]);
        let name = String::from_utf8_lossy(&out.stdout).trim().to_string();
        let stamp = window_option(pane, thurbox::agent::tmux::WINDOW_SESSION_OPTION);
        (name, stamp)
    });

    if let Err(e) = &spawned {
        panic!("the spawn the hook could not stop failed anyway: {e:#}");
    }
    let (name, stamp) = found.expect("the spawn answered with a pane id");
    assert_eq!(
        name, "tb-stamped",
        "the id the spawn answered with must resolve to the window it made"
    );
    assert_eq!(
        stamp, SESSION_ID,
        "the session stamp must have landed on that window, or nothing finds it \
         again"
    );
}

/// The heartbeat keeper is created by the same command on the same server, and
/// reads the same status.
///
/// It asks for no `-P` answer, so there is no pane id to weigh and the listing
/// is what says whether the window exists. Covered separately for that reason:
/// the pane-id tests above cannot reach this branch, so it could regress while
/// they stayed green.
#[test]
fn a_dead_plugin_hook_does_not_fail_the_heartbeat_keeper() {
    if !have_tmux() {
        eprintln!("skipping: tmux is not installed");
        return;
    }

    let dir = tempfile::tempdir().expect("tempdir");
    let _server = server_with_a_dead_hook(dir.path());

    // The keeper runs `<cli> automation tick` in a shell loop, so the loop —
    // and the window holding it — exists whether or not the path resolves.
    let armed = thurbox::agent::tmux::ensure_automation_heartbeat(&dir.path().join("thurbox-cli"));
    let names = window_names();
    let running = thurbox::agent::tmux::automation_heartbeat_running();

    if let Err(e) = armed {
        panic!("a heartbeat window tmux created was reported as a failure because a user hook exited non-zero: {e:#}");
    }
    assert!(
        names.iter().any(|n| n == "automation-heartbeat"),
        "the keeper's window must be on the server: {names:?}"
    );
    assert!(
        running,
        "the keeper must read as armed, or the next automation write arms a second one"
    );
}
