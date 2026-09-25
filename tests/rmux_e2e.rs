//! An RMUX session created through the public CLI, on a private daemon.

#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Duration, Instant};

#[path = "support/tmux_server.rs"]
mod tmux_server;

use tmux_server::TmuxServer;

struct RmuxInstance {
    root: tempfile::TempDir,
    server: TmuxServer,
    binary: PathBuf,
}

impl RmuxInstance {
    fn new(binary: PathBuf) -> Self {
        // Unix socket paths have a short limit, so keep this owned root shallow.
        let root = tempfile::Builder::new()
            .prefix("tbx-rmux-")
            .tempdir_in(std::env::temp_dir())
            .expect("private runtime root");
        for name in ["home", "config", "data", "repo"] {
            std::fs::create_dir(root.path().join(name)).expect("private directory");
        }
        Self {
            root,
            server: TmuxServer::pin("tbx-rmux-e2e"),
            binary,
        }
    }

    fn path(&self, name: &str) -> PathBuf {
        self.root.path().join(name)
    }

    fn scope(&self, cmd: &mut Command) {
        cmd.env("HOME", self.path("home"))
            .env("USERPROFILE", self.path("home"))
            .env("XDG_RUNTIME_DIR", self.root.path())
            .env("THURBOX_CONFIG_DIR", self.path("config"))
            .env("THURBOX_DATA_DIR", self.path("data"))
            .env_remove("TMUX")
            .env_remove("TMUX_PANE");
        self.server.scope(cmd);
        let path = std::env::var_os("PATH").unwrap_or_default();
        let paths = std::env::split_paths(&path);
        let joined = std::env::join_paths(
            std::iter::once(self.binary.parent().unwrap().to_path_buf()).chain(paths),
        )
        .expect("PATH");
        cmd.env("PATH", joined);
    }

    fn cli(&self, args: &[&str]) -> Output {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_thurbox-cli"));
        cmd.args(args);
        self.scope(&mut cmd);
        cmd.output().expect("thurbox-cli")
    }

    fn rmux(&self, args: &[&str]) -> Output {
        let mut cmd = Command::new(&self.binary);
        cmd.args(["-L", self.server.socket()]).args(args);
        self.scope(&mut cmd);
        cmd.output().expect("rmux")
    }

    fn tmux(&self, args: &[&str]) -> Output {
        let mut cmd = Command::new("tmux");
        cmd.args(["-L", self.server.socket()]).args(args);
        self.scope(&mut cmd);
        cmd.output().expect("tmux")
    }

    fn activate_for_backend_trait(&self) {
        for (key, value) in [
            ("HOME", self.path("home")),
            ("XDG_RUNTIME_DIR", self.root.path().to_path_buf()),
            ("THURBOX_CONFIG_DIR", self.path("config")),
            ("THURBOX_DATA_DIR", self.path("data")),
        ] {
            std::env::set_var(key, value);
        }
        std::env::remove_var("TMUX");
        let path = std::env::var_os("PATH").unwrap_or_default();
        let joined = std::env::join_paths(
            std::iter::once(self.binary.parent().unwrap().to_path_buf())
                .chain(std::env::split_paths(&path)),
        )
        .expect("PATH");
        std::env::set_var("PATH", joined);
    }
}

impl Drop for RmuxInstance {
    fn drop(&mut self) {
        let _ = self.rmux(&["kill-server"]);
    }
}

fn rmux_binary() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("RMUX_TEST_BIN") {
        return Some(PathBuf::from(path));
    }
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|dir| dir.join("rmux"))
        .find(|path| Path::new(path).is_file())
}

#[test]
fn unknown_persisted_local_backend_is_rejected() {
    let error = thurbox::agent::tmux::LocalMuxContext::for_backend("local-future")
        .expect_err("unknown local backend must not resolve to tmux");
    assert!(error.contains("local-future"), "{error}");
    assert!(thurbox::agent::tmux::LocalMuxContext::for_backend("ssh:remote").is_ok());
    assert!(thurbox::agent::tmux::LocalMuxContext::for_backend("").is_ok());
}

#[test]
fn missing_rmux_is_reported_before_a_session_is_created() {
    let root = tempfile::tempdir().expect("private instance");
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_thurbox-cli"));
    cmd.args([
        "session",
        "create",
        "--name",
        "missing-rmux",
        "--repo-path",
        root.path().to_str().unwrap(),
        "--command",
        "cat",
        "--multiplexer",
        "rmux",
        "--json",
    ]);
    cmd.env("HOME", root.path())
        .env("USERPROFILE", root.path())
        .env("THURBOX_CONFIG_DIR", root.path().join("config"))
        .env("THURBOX_DATA_DIR", root.path().join("data"))
        .env("PATH", "")
        .env_remove("THURBOX_SOCKET_FOR");
    let out = cmd.output().expect("thurbox-cli");
    assert!(!out.status.success());
    let answer: serde_json::Value = serde_json::from_slice(&out.stdout).expect("error JSON");
    let error = answer["error"].as_str().expect("error text");
    assert!(error.contains("rmux"), "{error}");
    assert!(
        error.contains("github.com/Helvesec/rmux/releases"),
        "{error}"
    );
    let db = thurbox::storage::Database::open(&root.path().join("data/thurbox.db"))
        .expect("instance database");
    assert!(db.list_active_sessions().expect("list sessions").is_empty());
}

#[test]
fn settings_choose_rmux_for_new_local_sessions() {
    let Some(binary) = rmux_binary() else {
        eprintln!("skipping: set RMUX_TEST_BIN to a real rmux binary");
        return;
    };
    let instance = RmuxInstance::new(binary);
    std::fs::write(
        instance.path("config/settings.toml"),
        "multiplexer = \"rmux\"\n",
    )
    .expect("RMUX preference");
    let repo = instance.path("repo");
    let created = instance.cli(&[
        "session",
        "create",
        "--name",
        "configured-rmux",
        "--repo-path",
        repo.to_str().unwrap(),
        "--command",
        "cat",
        "--json",
    ]);
    assert!(
        created.status.success(),
        "create failed: {}",
        String::from_utf8_lossy(&created.stdout)
    );
    let created: serde_json::Value = serde_json::from_slice(&created.stdout).expect("create JSON");
    assert_eq!(created["backend_type"], "local-rmux");
    let id = created["id"].as_str().expect("session id");
    let doctor = instance.cli(&["doctor", "--json"]);
    let doctor: serde_json::Value = serde_json::from_slice(&doctor.stdout).expect("doctor JSON");
    assert_eq!(doctor["multiplexer"], "rmux");

    let override_create = instance.cli(&[
        "session",
        "create",
        "--name",
        "overridden-tmux",
        "--repo-path",
        repo.to_str().unwrap(),
        "--command",
        "cat",
        "--multiplexer",
        "default",
        "--json",
    ]);
    assert!(override_create.status.success(), "default override failed");
    let override_create: serde_json::Value =
        serde_json::from_slice(&override_create.stdout).expect("override JSON");
    assert_eq!(override_create["backend_type"], "local-tmux");

    std::fs::write(
        instance.path("config/settings.toml"),
        "multiplexer = \"default\"\n",
    )
    .expect("switch back");
    let recorded = instance.cli(&["session", "get", id, "--json"]);
    let recorded: serde_json::Value = serde_json::from_slice(&recorded.stdout).expect("get JSON");
    assert_eq!(recorded["backend_type"], "local-rmux");
}

#[test]
fn cli_selects_rmux_and_records_the_backend_that_owns_the_session() {
    let Some(binary) = rmux_binary() else {
        eprintln!("skipping: set RMUX_TEST_BIN to a real rmux binary");
        return;
    };
    let instance = RmuxInstance::new(binary);
    let repo = instance.path("repo");
    let created = instance.cli(&[
        "session",
        "create",
        "--name",
        "probe",
        "--repo-path",
        repo.to_str().unwrap(),
        "--command",
        "cat",
        "--multiplexer",
        "rmux",
        "--json",
    ]);
    assert!(
        created.status.success(),
        "create failed: stdout={} stderr={}",
        String::from_utf8_lossy(&created.stdout),
        String::from_utf8_lossy(&created.stderr),
    );
    let created: serde_json::Value = serde_json::from_slice(&created.stdout).expect("create JSON");
    let id = created["id"].as_str().expect("session id");
    let pane = created["backend_id"].as_str().expect("pane id");
    let doctor = instance.cli(&["doctor", "--multiplexer", "rmux", "--json"]);
    let doctor: serde_json::Value = serde_json::from_slice(&doctor.stdout).expect("doctor JSON");
    assert_eq!(doctor["multiplexer"], "rmux");
    assert_eq!(doctor["checks"][0]["level"], "ok");
    let rows = instance.cli(&["session", "list", "--json"]);
    assert!(rows.status.success(), "session list failed");
    let rows: serde_json::Value = serde_json::from_slice(&rows.stdout).expect("list JSON");
    assert_eq!(rows[0]["backend_type"], "local-rmux");
    let verified = instance.cli(&["session", "get", id, "--json"]);
    assert!(verified.status.success(), "session get failed");
    let verified: serde_json::Value =
        serde_json::from_slice(&verified.stdout).expect("verified session JSON");
    assert_eq!(verified["backend_type"], "local-rmux");
    assert!(
        verified["foreground_process"]
            .as_str()
            .is_some_and(|process| process.ends_with("/cat")),
        "RMUX pane probe must reach the recorded server: {verified}"
    );
    let adopted_create = instance.cli(&[
        "session",
        "create",
        "--name",
        "probe",
        "--repo-path",
        repo.to_str().unwrap(),
        "--command",
        "cat",
        "--multiplexer",
        "rmux",
        "--on-existing",
        "adopt",
        "--json",
    ]);
    assert!(adopted_create.status.success(), "adopt create failed");
    let adopted_create: serde_json::Value =
        serde_json::from_slice(&adopted_create.stdout).expect("adopt create JSON");
    assert_eq!(adopted_create["created"], false);
    assert_eq!(adopted_create["id"], id);
    assert_eq!(adopted_create["backend_type"], "local-rmux");
    let windows = instance.rmux(&["list-windows", "-t", "thurbox-dev", "-F", "#{window_name}"]);
    assert!(windows.status.success(), "RMUX list-windows failed");
    assert!(String::from_utf8_lossy(&windows.stdout).contains("tb-probe"));

    let status = instance.rmux(&["set-option", "-p", "-t", pane, "@thurbox_state", "done"]);
    assert!(
        status.status.success(),
        "set RMUX pane state: {}",
        String::from_utf8_lossy(&status.stderr)
    );
    let tick = instance.cli(&["automation", "tick", "--json"]);
    assert!(
        tick.status.success(),
        "status poll: {}",
        String::from_utf8_lossy(&tick.stdout)
    );
    let db = thurbox::storage::Database::open(&instance.path("data/thurbox.db"))
        .expect("instance database");
    let states = db.load_hook_states().expect("hook states");
    let session_id = id.parse().expect("session id");
    assert_eq!(
        states.get(&session_id).and_then(|row| row.state.as_deref()),
        Some("done")
    );

    let sent = instance.cli(&["session", "send", id, "RMUX_E2E_MARKER", "--json"]);
    assert!(
        sent.status.success(),
        "send failed: {}",
        String::from_utf8_lossy(&sent.stdout)
    );
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let captured = instance.cli(&["session", "capture", id, "--json"]);
        assert!(
            captured.status.success(),
            "capture failed: {}",
            String::from_utf8_lossy(&captured.stdout)
        );
        let captured: serde_json::Value =
            serde_json::from_slice(&captured.stdout).expect("capture JSON");
        if captured["output"]
            .as_str()
            .unwrap_or_default()
            .contains("RMUX_E2E_MARKER")
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "RMUX pane never showed the sent text"
        );
        std::thread::sleep(Duration::from_millis(25));
    }

    instance.activate_for_backend_trait();
    use thurbox::agent::SessionBackend;
    let backend = thurbox::agent::tmux::TmuxBackend::local_rmux();
    backend.check_available().expect("RMUX version is usable");
    backend.ensure_ready().expect("RMUX control mode starts");
    assert!(backend
        .discover()
        .expect("discover RMUX windows")
        .iter()
        .any(|row| row.backend_id == pane));
    let adopted = backend
        .adopt(pane, 30, 100, None)
        .expect("adopt an existing RMUX pane");
    let mut input = adopted.input;
    let mut output_stream = adopted.output;
    use std::io::{Read, Write};
    let (tx, rx) = std::sync::mpsc::channel();
    let reader = std::thread::spawn(move || {
        let mut output = String::new();
        let mut chunk = [0; 4096];
        loop {
            match output_stream.read(&mut chunk) {
                Ok(0) => break,
                Ok(n) => {
                    output.push_str(&String::from_utf8_lossy(&chunk[..n]));
                    if output.contains("RMUX_STREAM_MARKER") {
                        break;
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) => panic!("RMUX output stream: {e}"),
            }
        }
        let _ = tx.send(output);
    });
    input
        .write_all(b"RMUX_STREAM_MARKER\r")
        .expect("write through backend");
    let streamed = rx
        .recv_timeout(Duration::from_secs(5))
        .expect("RMUX control stream output");
    assert!(
        streamed.contains("RMUX_STREAM_MARKER"),
        "streamed: {streamed:?}"
    );
    backend.claim_size(pane, 27, 94).expect("resize RMUX pane");
    let size = instance.rmux(&[
        "display-message",
        "-p",
        "-t",
        pane,
        "#{pane_width}x#{pane_height}",
    ]);
    assert!(size.status.success(), "query RMUX pane size");
    assert_eq!(String::from_utf8_lossy(&size.stdout).trim(), "94x27");
    backend.detach(pane).expect("detach the RMUX pane");
    reader.join().expect("reader thread");
    drop(backend);
    let backend = thurbox::agent::tmux::TmuxBackend::local_rmux();
    backend
        .ensure_ready()
        .expect("reconnect to the RMUX daemon");
    let adopted = backend
        .adopt(pane, 30, 100, None)
        .expect("adopt after reconnect");
    drop(adopted);
    backend.detach(pane).expect("detach after reconnect");
    let mux = thurbox::agent::tmux::LocalMuxContext::for_backend("local-rmux")
        .expect("recorded RMUX backend");
    mux.send_prompt_after_delay(id, "probe", "RMUX_DELAYED_MARKER", 0)
        .expect("schedule RMUX prompt");
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let captured = instance.cli(&["session", "capture", id, "--json"]);
        let captured: serde_json::Value =
            serde_json::from_slice(&captured.stdout).expect("capture JSON");
        if captured["output"]
            .as_str()
            .unwrap_or_default()
            .contains("RMUX_DELAYED_MARKER")
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "deferred RMUX prompt did not reach its pane"
        );
        std::thread::sleep(Duration::from_millis(25));
    }

    let default_id = if Command::new("tmux").arg("-V").output().is_ok() {
        let default = instance.cli(&[
            "session",
            "create",
            "--name",
            "default-probe",
            "--repo-path",
            repo.to_str().unwrap(),
            "--command",
            "cat",
            "--json",
        ]);
        assert!(
            default.status.success(),
            "default create: {}",
            String::from_utf8_lossy(&default.stdout)
        );
        let default: serde_json::Value =
            serde_json::from_slice(&default.stdout).expect("default JSON");
        let default_id = default["id"].as_str().unwrap().to_string();
        let rows = instance.cli(&["session", "list", "--json"]);
        let rows: serde_json::Value =
            serde_json::from_slice(&rows.stdout).expect("mixed list JSON");
        assert!(rows
            .as_array()
            .unwrap()
            .iter()
            .any(|row| row["id"] == default_id && row["backend_type"] == "local-tmux"));
        let windows = instance.tmux(&["list-windows", "-t", "thurbox-dev", "-F", "#{window_name}"]);
        assert!(String::from_utf8_lossy(&windows.stdout).contains("tb-default-probe"));
        let sent = instance.cli(&["session", "send", &default_id, "TMUX_E2E_MARKER", "--json"]);
        assert!(
            sent.status.success(),
            "default send: {}",
            String::from_utf8_lossy(&sent.stdout)
        );
        let sent = instance.cli(&["session", "send", id, "RMUX_AFTER_DEFAULT", "--json"]);
        assert!(
            sent.status.success(),
            "RMUX send after default: {}",
            String::from_utf8_lossy(&sent.stdout)
        );

        std::fs::write(
            instance.path("config/agents.toml"),
            "default = \"shell\"\n\n[[agents]]\nname = \"shell\"\ncommand = \"sh\"\nargs = []\n",
        )
        .expect("shell agent config");
        let db = thurbox::storage::Database::open(&instance.path("data/thurbox.db"))
            .expect("instance database");
        let auto_id = db
            .create_automation(&thurbox::storage::automations::NewAutomation {
                name: "spawn-probe".into(),
                enabled: true,
                schedule: thurbox::session::AutomationSchedule::Once { at: 1 },
                timezone: None,
                action: thurbox::session::AutomationAction::Spawn {
                    repo_path: repo.clone(),
                    worktree_branch: None,
                    base_branch: None,
                    agent: Some("shell".into()),
                    extra_repos: Vec::new(),
                },
                prompt: "AUTO_E2E_MARKER".into(),
                next_run_at: Some(1),
            })
            .expect("automation row");
        let collision_name = format!("auto-{auto_id}");
        let collision = instance.cli(&[
            "session",
            "create",
            "--name",
            &collision_name,
            "--repo-path",
            repo.to_str().unwrap(),
            "--command",
            "cat",
            "--multiplexer",
            "rmux",
            "--json",
        ]);
        assert!(collision.status.success(), "RMUX namesake create failed");
        let collision: serde_json::Value =
            serde_json::from_slice(&collision.stdout).expect("collision JSON");
        let tick = instance.cli(&["automation", "tick", "--json"]);
        assert!(tick.status.success(), "automation tick failed");
        let tick: serde_json::Value = serde_json::from_slice(&tick.stdout).expect("tick JSON");
        assert_eq!(
            tick["fired"][0]["detail"],
            format!("spawned {collision_name}")
        );
        let rows = instance.cli(&["session", "list", "--json"]);
        let rows: serde_json::Value = serde_json::from_slice(&rows.stdout).expect("list JSON");
        let auto_session = rows
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["name"] == collision_name && row["backend_type"] == "local-tmux");
        let auto_session = auto_session.expect("automation session on default backend");
        let auto_session_id = auto_session["id"].as_str().unwrap();
        assert_ne!(auto_session_id, collision["id"]);
        let deleted_auto =
            instance.cli(&["session", "delete", auto_session_id, "--force", "--json"]);
        assert!(
            deleted_auto.status.success(),
            "automation session delete failed"
        );
        let deleted_collision = instance.cli(&[
            "session",
            "delete",
            collision["id"].as_str().unwrap(),
            "--force",
            "--json",
        ]);
        assert!(
            deleted_collision.status.success(),
            "RMUX namesake delete failed"
        );

        let task = instance.cli(&[
            "task",
            "create",
            "--title",
            "legacy reuse",
            "--repo",
            repo.to_str().unwrap(),
            "--agent",
            "shell",
            "--json",
        ]);
        assert!(task.status.success(), "task create failed");
        let task: serde_json::Value = serde_json::from_slice(&task.stdout).expect("task JSON");
        let task_id = task["id"].as_i64().expect("task id");
        let legacy_name = format!("task-{task_id}");
        let legacy = instance.cli(&[
            "session",
            "create",
            "--name",
            &legacy_name,
            "--repo-path",
            repo.to_str().unwrap(),
            "--command",
            "cat",
            "--json",
        ]);
        assert!(legacy.status.success(), "legacy task session create failed");
        let legacy: serde_json::Value =
            serde_json::from_slice(&legacy.stdout).expect("legacy session JSON");
        let legacy_id = legacy["id"].as_str().expect("legacy session id");
        let mut legacy_row = db
            .get_session_by_id(legacy_id.parse().expect("session id"))
            .expect("load legacy row")
            .expect("legacy row exists");
        legacy_row.backend_type = "tmux".into();
        db.upsert_session(&legacy_row)
            .expect("persist legacy backend");
        let run = instance.cli(&["task", "run", &task_id.to_string(), "--json"]);
        assert!(
            run.status.success(),
            "task run failed: {}",
            String::from_utf8_lossy(&run.stderr)
        );
        let run: serde_json::Value = serde_json::from_slice(&run.stdout).expect("task run JSON");
        assert_eq!(
            run["reused"], legacy_name,
            "legacy tmux row should be reused: {run}"
        );
        let deleted_legacy = instance.cli(&["session", "delete", legacy_id, "--force", "--json"]);
        assert!(
            deleted_legacy.status.success(),
            "legacy task session delete failed"
        );
        Some(default_id)
    } else {
        None
    };

    let forked = instance.cli(&["session", "fork", id, "--name", "probe-fork", "--json"]);
    assert!(
        forked.status.success(),
        "fork failed: {}",
        String::from_utf8_lossy(&forked.stdout)
    );
    let forked: serde_json::Value = serde_json::from_slice(&forked.stdout).expect("fork JSON");
    let fork_id = forked["id"].as_str().expect("fork id");
    assert_eq!(forked["backend_type"], "local-rmux");
    let rows = instance.cli(&["session", "list", "--json"]);
    let rows: serde_json::Value = serde_json::from_slice(&rows.stdout).expect("fork list JSON");
    assert!(rows
        .as_array()
        .unwrap()
        .iter()
        .any(|row| { row["id"] == fork_id && row["backend_type"] == "local-rmux" }));
    let windows = instance.rmux(&["list-windows", "-t", "thurbox-dev", "-F", "#{window_name}"]);
    assert!(String::from_utf8_lossy(&windows.stdout).contains("tb-probe-fork"));
    let deleted_fork = instance.cli(&["session", "delete", fork_id, "--force", "--json"]);
    assert!(deleted_fork.status.success(), "fork delete failed");

    let stopped = instance.cli(&["session", "stop", id, "--json"]);
    assert!(
        stopped.status.success(),
        "stop failed: {}",
        String::from_utf8_lossy(&stopped.stdout)
    );
    let started = instance.cli(&["session", "start", id, "--json"]);
    assert!(
        started.status.success(),
        "start failed: {}",
        String::from_utf8_lossy(&started.stdout)
    );
    let db = thurbox::storage::Database::open(&instance.path("data/thurbox.db"))
        .expect("instance database");
    let mut row = db
        .get_session_by_id(id.parse().expect("session id"))
        .expect("load RMUX row")
        .expect("RMUX row exists");
    row.backend_type = "local-future".into();
    db.upsert_session(&row)
        .expect("persist unsupported backend");
    let message = instance.cli(&[
        "message",
        "send",
        "--to",
        id,
        "--kind",
        "probe",
        "--body",
        "queued once",
        "--json",
    ]);
    assert!(
        message.status.success(),
        "durable enqueue must succeed despite wake failure: {}",
        String::from_utf8_lossy(&message.stdout)
    );
    let message: serde_json::Value = serde_json::from_slice(&message.stdout).expect("message JSON");
    assert_eq!(message["enqueued"], true);
    assert_eq!(message["woke"], false);
    row.backend_type = "local-rmux".into();
    db.upsert_session(&row).expect("restore RMUX backend");
    let deleted = instance.cli(&["session", "delete", id, "--force", "--json"]);
    assert!(
        deleted.status.success(),
        "delete failed: {}",
        String::from_utf8_lossy(&deleted.stdout)
    );
    let windows = instance.rmux(&["list-windows", "-t", "thurbox-dev", "-F", "#{window_name}"]);
    assert!(!String::from_utf8_lossy(&windows.stdout).contains("tb-probe"));
    if let Some(default_id) = default_id {
        let windows = instance.tmux(&["list-windows", "-t", "thurbox-dev", "-F", "#{window_name}"]);
        assert!(String::from_utf8_lossy(&windows.stdout).contains("tb-default-probe"));
        let deleted = instance.cli(&["session", "delete", &default_id, "--force", "--json"]);
        assert!(
            deleted.status.success(),
            "default delete: {}",
            String::from_utf8_lossy(&deleted.stdout)
        );
    }
}
