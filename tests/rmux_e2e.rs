//! RMUX sessions through the real registry and session lifecycle.

#![cfg(unix)]

use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

use thurbox::backend::wiring;
use thurbox::session::{Multiplexer, Route};
use thurbox::session_ops::{delete, rename, restart, restore, spawn};
use thurbox::storage::Database;

#[path = "support/tmux_server.rs"]
mod tmux_server;

struct RmuxServer {
    socket: String,
    _scope: tmux_server::TmuxServer,
}

impl RmuxServer {
    fn new() -> Self {
        let socket = format!("thurbox-rmux-e2e-{}", std::process::id());
        let scope = tmux_server::TmuxServer::pin(&socket);
        Self {
            socket,
            _scope: scope,
        }
    }

    fn command(&self, args: &[&str]) -> std::process::Output {
        Command::new("rmux")
            .args(["-L", &self.socket])
            .args(args)
            .output()
            .expect("rmux command")
    }
}

impl Drop for RmuxServer {
    fn drop(&mut self) {
        let _ = self.command(&["kill-server"]);
    }
}

fn git(repo: &Path, args: &[&str]) {
    let output = Command::new("git")
        .args(args)
        .current_dir(repo)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .output()
        .expect("git command");
    assert!(output.status.success(), "git {args:?}: {output:?}");
}

#[test]
fn rmux_create_restart_rename_restore_relaunch_and_delete() {
    if !Command::new("rmux")
        .arg("-V")
        .output()
        .is_ok_and(|out| out.status.success())
    {
        eprintln!("skipping: rmux is not installed");
        return;
    }
    let root = tempfile::tempdir().expect("isolated paths");
    let repo = root.path().join("repo");
    std::fs::create_dir(&repo).expect("repo dir");
    git(&repo, &["init", "-q", "-b", "main"]);
    git(&repo, &["config", "user.email", "test@example.invalid"]);
    git(&repo, &["config", "user.name", "Test"]);
    git(&repo, &["config", "commit.gpgsign", "false"]);
    std::fs::write(repo.join("README.md"), "test\n").expect("README");
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-qm", "init"]);

    thurbox::paths::set_test_dir(root.path());
    let config = thurbox::paths::config_file()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf();
    std::fs::create_dir_all(&config).expect("config dir");
    std::fs::write(
        config.join("hosts.toml"),
        "[[hosts]]\nname = \"down\"\ndestination = \"probe.invalid\"\nmultiplexer = \"rmux\"\nshare_sessions = false\n",
    )
    .expect("hosts config");
    let bin = root.path().join("bin");
    std::fs::create_dir(&bin).expect("bin dir");
    let ssh = bin.join("ssh");
    std::fs::write(&ssh, "#!/bin/sh\nexit 255\n").expect("ssh stand-in");
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&ssh, std::fs::Permissions::from_mode(0o700)).expect("chmod");
    let path = std::env::join_paths(std::iter::once(bin).chain(std::env::split_paths(
        &std::env::var_os("PATH").unwrap_or_default(),
    )))
    .expect("PATH");
    std::env::set_var("PATH", path);
    let server = RmuxServer::new();
    let db = Database::open_in_memory().expect("db");
    let backends = wiring::configured().0;
    let rmux_route = Route::local(Some(Multiplexer::Rmux));
    assert!(backends.supports(&rmux_route));
    assert!(backends.supports(&Route::local(Some(Multiplexer::Tmux))));
    assert!(backends.supports(&Route::local(Some(Multiplexer::Psmux))));
    assert!(backends.supports(&Route::remote(
        thurbox::session::Via::Ssh,
        "down",
        Some(Multiplexer::Rmux),
    )));
    assert_eq!(
        backends.default_route(),
        &Route::local(Some(Multiplexer::Tmux))
    );

    let created = spawn::spawn_session_headless(
        &db,
        &backends,
        spawn::SpawnRequest {
            name: "rmux-probe".into(),
            repo_path: repo,
            command: Some("cat".into()),
            multiplexer: Some("rmux".into()),
            ..Default::default()
        },
    )
    .expect("create on RMUX");
    assert_eq!(created.backend_type, "local:rmux");
    let id = created.session_id;
    let original = created.backend_id;

    let backend = backends.get(&rmux_route).expect("RMUX backend");
    let hook = backend.hook_signal_command().expect("RMUX hook command");
    assert!(
        hook.contains("$RMUX_PANE"),
        "the hook must target an RMUX pane"
    );
    let hook_pane = backend
        .create_window(&thurbox::backend::WindowSpec {
            owner: thurbox::backend::Owner::new(
                "00000000-0000-4000-8000-0000000000ee",
                "hook-probe",
            ),
            role: thurbox::backend::WindowRole::Agent,
            command: "sh",
            args: &["-c".into(), format!("{hook}blocked; sleep 3")],
            cwd: None,
            env: &Default::default(),
        })
        .expect("hook pane");
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline
        && !backend
            .hook_states()
            .expect("hook states")
            .contains(&(hook_pane.clone(), "blocked".into()))
    {
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(
        backend
            .hook_states()
            .expect("hook states")
            .contains(&(hook_pane.clone(), "blocked".into())),
        "the in-pane RMUX hook did not report its state"
    );
    backend.kill(&hook_pane).expect("kill hook probe");
    backend
        .send_text(&original, "rmux-input-probe", true)
        .expect("input");
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline
        && !String::from_utf8_lossy(&backend.capture_history(&original).expect("capture"))
            .contains("rmux-input-probe")
    {
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(
        String::from_utf8_lossy(&backend.capture_history(&original).expect("capture"))
            .contains("rmux-input-probe"),
        "input was not captured from the RMUX pane"
    );

    restart::restart_session_headless(&db, &backends, id).expect("restart");
    let restarted = db.get_session_by_id(id).unwrap().unwrap().backend_id;
    assert_ne!(original, restarted);

    rename::rename_session_headless(&db, &backends, id, "renamed").expect("rename");
    let named = server.command(&["list-windows", "-a", "-F", "#{window_name}"]);
    assert!(named.status.success());
    assert!(String::from_utf8_lossy(&named.stdout)
        .lines()
        .any(|line| line == "tb-renamed"));

    delete::delete_session_headless(&db, &backends, id, false).expect("soft delete");
    restore::restore_session_headless(&db, &backends, id, false).expect("restore");
    assert_eq!(
        db.get_session_by_id(id).unwrap().unwrap().backend_id,
        restarted
    );

    let killed = server.command(&["kill-window", "-t", &restarted]);
    assert!(killed.status.success(), "kill pane: {killed:?}");
    restart::restart_session_headless_with(&db, &backends, id, true).expect("relaunch missing");
    let relaunched = db.get_session_by_id(id).unwrap().unwrap().backend_id;
    assert_ne!(relaunched, restarted);
    restart::restart_session_headless_with(&db, &backends, id, true).expect("relaunch once");
    assert_eq!(
        db.get_session_by_id(id).unwrap().unwrap().backend_id,
        relaunched
    );

    db.conn_ref()
        .execute(
            "UPDATE sessions SET backend_type = 'ssh:down:rmux' WHERE id = ?1",
            [id.to_string()],
        )
        .expect("move row to unreachable route");
    assert!(restart::restart_session_headless_with(&db, &backends, id, true).is_err());
    assert_eq!(
        db.get_session_by_id(id).unwrap().unwrap().backend_id,
        relaunched
    );
    let still_local = server.command(&["list-panes", "-a", "-F", "#{pane_id}"]);
    assert!(String::from_utf8_lossy(&still_local.stdout)
        .lines()
        .any(|p| p == relaunched));
    db.conn_ref()
        .execute(
            "UPDATE sessions SET backend_type = 'local:rmux' WHERE id = ?1",
            [id.to_string()],
        )
        .expect("restore local route");

    delete::delete_session_headless(&db, &backends, id, true).expect("force delete");
    let windows = server.command(&["list-windows", "-a", "-F", "#{window_name}"]);
    let names = String::from_utf8_lossy(&windows.stdout);
    assert!(!names.lines().any(|line| line == "tb-renamed"), "{names}");
}
