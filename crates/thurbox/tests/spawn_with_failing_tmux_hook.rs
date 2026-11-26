#![cfg(unix)]

use std::collections::HashMap;
use std::process::Command;

#[path = "../../runtime-mux/tests/support/tmux_server.rs"]
mod tmux_server;

use tmux_server::TmuxServer;

const SOCKET: &str = "thurbox-failing-hook-e2e";
const SESSION_ID: &str = "11111111-1111-4111-8111-111111111111";

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

fn spawn(name: &str, cwd: &std::path::Path) -> thurbox::backend::BackendResult<String> {
    thurbox::backend::WindowLifecycle::create_window(
        &thurbox::backend::tmux::TmuxBackend::local(&thurbox::backend::mux_settings(
            &Default::default(),
        )),
        &thurbox::backend::WindowSpec {
            owner: thurbox::backend::Owner::new(SESSION_ID, name),
            role: thurbox::backend::WindowRole::Agent,
            command: "sh",
            args: &["-c".to_string(), "sleep 300".to_string()],
            cwd: Some(cwd),
            env: &HashMap::new(),
        },
    )
    .map(String::from)
}

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
fn a_dead_mod_hook_does_not_fail_a_window_that_was_created() {
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
        let stamp = window_option(
            pane,
            thurbox::backend::tmux_compat::server::WINDOW_SESSION_OPTION,
        );
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
