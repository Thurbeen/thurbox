//! A launch that dies before its agent reports reads `failed`, end to end.
//!
//! The incident this guards: a role wrapper printed `error: role 'debugger'
//! not found` and exited 1, `remain-on-exit` kept the dead pane, and the
//! session read `unreported` — "can report, has not yet" — for twelve hours
//! while a driver waited for its first report. These tests launch through the
//! real pipeline (agents.toml, a real tmux window, the real `thurbox-cli`) and
//! read what `session get`/`list --json` and the event log make of it.
//!
//! Skipped when tmux is absent, and scoped to a throwaway socket and a
//! temporary config, so it can never touch a real session.
#![cfg(unix)]

use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

use serde_json::Value;

/// The guard every tmux server in this file is reaped by — see its own doc.
#[path = "support/tmux_server.rs"]
mod tmux_server;

use tmux_server::TmuxServer;

const SOCKET: &str = "thurbox-launch-failure-e2e";

/// Long enough for a dead pane to be noticed on a loaded runner.
const WAIT: Duration = Duration::from_secs(20);

/// The two launches the incident stands for, plus a control. Each outlives
/// the spawn's own startup check (a pane dead within its first 50 ms is
/// refused at creation instead) and then dies the way the wrapper did.
const AGENTS: &str = r#"
default = "shell"

[[agents]]
name = "shell"
command = "sh"

# The wrapper's own failure, as a setup step: the agent must never start.
[[agents]]
name = "role-setup"
hook_schema = "claude"
command = "sh"
args = ["-c", "echo started > agent-started; sleep 60"]
vars = { role = "debugger" }
setup = [
  "[ -f scripts/missing.sh ] && sh scripts/missing.sh",
  "sleep 1; echo \"error: role '{role}' not found\" >&2; exit 3",
]

# A wrapper standing in for the agent and exiting before any hook reports.
[[agents]]
name = "claude-debugger"
hook_schema = "claude"
command = "sh"
args = ["-c", "sleep 1; echo \"error: role 'debugger' not found\"; exit 1"]

# The same exit from an agent wired to report nothing: its silence never
# meant anything, so its exit is not a failed launch.
[[agents]]
name = "quiet-tool"
command = "sh"
args = ["-c", "sleep 1; exit 1"]
"#;

#[test]
fn a_failing_setup_reads_failed_and_never_starts_the_agent() {
    let Some(env) = Env::new() else { return };
    let id = env.spawn("role-setup");

    let session = env.until_state(&id, "failed");
    let failure = &session["launch_failure"];
    assert_eq!(failure["reason"], "setup", "{session:#}");
    assert_eq!(failure["exit_code"], 3, "{session:#}");
    let output = failure["output"].as_str().expect("the pane's last lines");
    assert!(
        output.contains("error: role 'debugger' not found"),
        "{output}"
    );
    assert!(
        output.contains("thurbox: setup step 2 failed (exit 3)"),
        "{output}"
    );
    assert_eq!(failure["pane"], session["backend_id"], "{session:#}");
    assert_eq!(session["ready"], false, "{session:#}");
    assert!(
        !env.repo.path().join("agent-started").exists(),
        "the agent ran although its setup failed"
    );

    // Recorded, so a listing that does not look at panes reads it too.
    let listed = env.list();
    let row = listed
        .iter()
        .find(|row| row["id"] == id.as_str())
        .expect("listed");
    assert_eq!(row["state"], "failed", "{row:#}");
    assert_eq!(row["launch_failure"], *failure);

    let event = env.launch_failed_event(&id);
    assert_eq!(event.exit_code, Some(3));
    assert_eq!(event.to_state.as_deref(), Some("failed"));
    assert!(event.detail.unwrap_or_default().contains("not found"));
}

#[test]
fn an_agent_exiting_before_it_reports_reads_failed() {
    let Some(env) = Env::new() else { return };
    let id = env.spawn("claude-debugger");

    let session = env.until_state(&id, "failed");
    let failure = &session["launch_failure"];
    assert_eq!(failure["reason"], "exited", "{session:#}");
    assert_eq!(failure["exit_code"], 1, "{session:#}");
    assert!(
        failure["output"]
            .as_str()
            .is_some_and(|o| o.contains("error: role 'debugger' not found")),
        "{session:#}"
    );
    assert_eq!(env.launch_failed_event(&id).exit_code, Some(1));

    // Once: a second look records nothing new.
    env.get(&id);
    let db = env.db();
    let events = db
        .session_events_since(0, Some(id.parse().unwrap()), 100)
        .unwrap();
    assert_eq!(
        events
            .iter()
            .filter(|e| e.reason == "launch_failed")
            .count(),
        1
    );
}

/// psmux closes a window once its program exits, so a failed launch is
/// usually lost next. The failure outlives that loss, and so must a wait for
/// it.
#[test]
fn a_wait_for_failed_answers_after_the_failed_pane_is_lost() {
    let Some(env) = Env::new() else { return };
    // Keeps the server up once the failed window is gone.
    env.spawn("shell");
    let id = env.spawn("claude-debugger");
    let session = env.until_state(&id, "failed");
    let pane = session["backend_id"].as_str().expect("a pane").to_string();

    env._server.tmux(&["kill-window", "-t", &pane]);
    let db = env.db();
    let backends = thurbox::backend::wiring::configured().0;
    let mut sweep = thurbox::session_ops::lost::LostSweep::default();
    let deadline = Instant::now() + WAIT;
    while db.session_lost_at(id.parse().unwrap()).unwrap().is_none() {
        assert!(Instant::now() < deadline, "the loss was never recorded");
        sweep.sweep(&db, &backends);
        std::thread::sleep(Duration::from_millis(100));
    }
    assert_eq!(env.get(&id)["state"], "failed");

    let out = Command::new(env!("CARGO_BIN_EXE_thurbox-cli"))
        .args([
            "session",
            "wait",
            &id,
            "--state",
            "failed",
            "--timeout",
            "5",
        ])
        .args(["--json"])
        .output()
        .expect("run thurbox-cli");
    assert!(out.status.success(), "{out:?}");
    let answer: Value = serde_json::from_slice(&out.stdout).expect("json");
    assert_eq!(answer["state"], "failed", "{answer:#}");
}

/// A relaunch clears the failure record, but the `launch_failed` edge stays in
/// the log, and a stream resumed from before it replays it as `failed`.
#[test]
fn a_replayed_launch_failure_reads_failed_after_a_relaunch() {
    let Some(env) = Env::new() else { return };
    let cursor = env.db().latest_session_event_seq().unwrap();
    let id = env.spawn("claude-debugger");
    env.until_state(&id, "failed");

    let agents = AGENTS.replace(
        r#"args = ["-c", "sleep 1; echo \"error: role 'debugger' not found\"; exit 1"]"#,
        r#"args = ["-c", "sleep 60"]"#,
    );
    assert_ne!(agents, AGENTS);
    let config = thurbox::paths::config_file().unwrap();
    std::fs::write(config.parent().unwrap().join("agents.toml"), agents).unwrap();
    env.cli(&["session", "restart", &id, "--json"]);
    assert_eq!(env.get(&id)["launch_failure"], Value::Null);

    let out = Command::new(env!("CARGO_BIN_EXE_thurbox-cli"))
        .args(["watch", "--session", &id, "--for-secs", "1"])
        .args(["--since", &cursor.to_string(), "--json"])
        .output()
        .expect("run thurbox-cli");
    assert!(out.status.success(), "{out:?}");
    let replayed: Value = String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .find(|line| line["reason"] == "launch_failed")
        .expect("the launch_failed edge replayed");
    assert_eq!(replayed["state"], "failed", "{replayed:#}");
}

/// The other way round: an edge from before a failed relaunch is judged as of
/// when it happened, not by the failure that came after it.
#[test]
fn an_edge_replayed_from_before_a_failed_relaunch_keeps_its_state() {
    let Some(env) = Env::new() else { return };
    let config = thurbox::paths::config_file().unwrap();
    let agents = config.parent().unwrap().join("agents.toml");
    let failing = r#"args = ["-c", "sleep 1; echo \"error: role 'debugger' not found\"; exit 1"]"#;
    std::fs::write(
        &agents,
        AGENTS.replace(failing, r#"args = ["-c", "sleep 60"]"#),
    )
    .unwrap();
    let id = env.spawn("claude-debugger");
    let cursor = env.db().latest_session_event_seq().unwrap();
    env.db()
        .set_hook_state(id.parse().unwrap(), "done")
        .unwrap();

    std::fs::write(&agents, AGENTS).unwrap();
    env.cli(&["session", "restart", &id, "--json"]);
    env.until_state(&id, "failed");

    let out = Command::new(env!("CARGO_BIN_EXE_thurbox-cli"))
        .args(["watch", "--session", &id, "--for-secs", "1"])
        .args(["--since", &cursor.to_string(), "--json"])
        .output()
        .expect("run thurbox-cli");
    assert!(out.status.success(), "{out:?}");
    let lines: Vec<Value> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .collect();
    let done = lines
        .iter()
        .find(|line| line["to_state"] == "done")
        .expect("the done edge replayed");
    assert_eq!(done["state"], "done", "{done:#}");
    let failed = lines
        .iter()
        .find(|line| line["reason"] == "launch_failed")
        .expect("the launch_failed edge replayed");
    assert_eq!(failed["state"], "failed", "{failed:#}");
}

#[test]
fn an_uncovered_agents_exit_is_not_a_failed_launch() {
    let Some(env) = Env::new() else { return };
    let id = env.spawn("quiet-tool");
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        let session = env.get(&id);
        assert_ne!(session["state"], "failed", "{session:#}");
        if session["hook_corroboration"] == "dead" {
            assert_eq!(session["launch_failure"], Value::Null);
            return;
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    panic!("the pane never died");
}

struct Env {
    _server: TmuxServer,
    _home: tempfile::TempDir,
    repo: tempfile::TempDir,
}

impl Env {
    fn new() -> Option<Self> {
        if !Command::new("tmux")
            .arg("-V")
            .output()
            .is_ok_and(|o| o.status.success())
        {
            eprintln!("skipping: tmux is not installed");
            return None;
        }
        let server = TmuxServer::pin(SOCKET);
        let home = tempfile::tempdir().expect("tempdir");
        std::env::set_var("HOME", home.path());
        for variable in ["XDG_CONFIG_HOME", "XDG_DATA_HOME", "XDG_CACHE_HOME"] {
            let directory = home.path().join(variable);
            std::fs::create_dir_all(&directory).unwrap();
            std::env::set_var(variable, directory);
        }
        thurbox::paths::set_test_dir(home.path());
        std::env::set_var(thurbox::paths::CONFIG_DIR_OVERRIDE_ENV, home.path());
        std::env::set_var(thurbox::paths::DATA_DIR_OVERRIDE_ENV, home.path());
        let config = thurbox::paths::config_file()
            .expect("config path")
            .parent()
            .expect("config dir")
            .to_path_buf();
        std::fs::create_dir_all(&config).unwrap();
        std::fs::write(config.join("agents.toml"), AGENTS).unwrap();
        Some(Self {
            _server: server,
            _home: home,
            repo: repo(),
        })
    }

    fn db(&self) -> thurbox::storage::Database {
        let path = thurbox::paths::database_file().expect("db path");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        thurbox::storage::Database::open(&path).expect("open db")
    }

    fn spawn(&self, agent: &str) -> String {
        let spawned = thurbox::session_ops::spawn_session_headless(
            &self.db(),
            &thurbox::backend::wiring::configured().0,
            thurbox::session_ops::SpawnRequest {
                name: agent.into(),
                repo_path: self.repo.path().into(),
                agent: Some(agent.into()),
                ..Default::default()
            },
        )
        .unwrap_or_else(|e| panic!("spawn {agent}: {e}"));
        spawned.session_id.to_string()
    }

    fn cli(&self, args: &[&str]) -> Value {
        let out = Command::new(env!("CARGO_BIN_EXE_thurbox-cli"))
            .args(args)
            .output()
            .expect("run thurbox-cli");
        assert!(out.status.success(), "thurbox-cli {args:?}: {out:?}");
        serde_json::from_slice(&out.stdout).expect("json")
    }

    fn get(&self, id: &str) -> Value {
        self.cli(&["session", "get", id, "--json"])
    }

    fn list(&self) -> Vec<Value> {
        self.cli(&["session", "list", "--json"])
            .as_array()
            .cloned()
            .expect("an array")
    }

    fn until_state(&self, id: &str, state: &str) -> Value {
        let deadline = Instant::now() + WAIT;
        loop {
            let session = self.get(id);
            if session["state"] == state {
                return session;
            }
            assert!(
                Instant::now() < deadline,
                "never reached {state}: {session:#}"
            );
            std::thread::sleep(Duration::from_millis(250));
        }
    }

    fn launch_failed_event(&self, id: &str) -> thurbox::storage::SessionEventRow {
        self.db()
            .session_events_since(0, Some(id.parse().unwrap()), 100)
            .unwrap()
            .into_iter()
            .find(|e| e.reason == "launch_failed")
            .expect("a launch_failed event")
    }
}

fn repo() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    git(dir.path(), &["init", "-q", "-b", "main"]);
    git(dir.path(), &["config", "user.email", "t@example.com"]);
    git(dir.path(), &["config", "user.name", "thurbox-test"]);
    git(dir.path(), &["config", "commit.gpgsign", "false"]);
    std::fs::write(dir.path().join("README.md"), "# probe\n").expect("write");
    git(dir.path(), &["add", "."]);
    git(dir.path(), &["commit", "-qm", "init"]);
    dir
}

fn git(dir: &Path, args: &[&str]) {
    let mut cmd = Command::new("git");
    cmd.args(args).current_dir(dir);
    for var in [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_INDEX_FILE",
        "GIT_COMMON_DIR",
        "GIT_OBJECT_DIRECTORY",
        "GIT_ALTERNATE_OBJECT_DIRECTORIES",
        "GIT_PREFIX",
        "GIT_NAMESPACE",
    ] {
        cmd.env_remove(var);
    }
    assert!(
        cmd.output().expect("run git").status.success(),
        "git {args:?}"
    );
}
