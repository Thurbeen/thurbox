//! Headless event consumers, driven through the real CLI and SQLite writers.
#![cfg(unix)]
use serde_json::Value;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::time::Duration;
use thurbox::{session::SessionId, storage::Database, sync::SharedSession};
#[path = "support/tmux_server.rs"]
mod tmux_server;

struct Env {
    root: tempfile::TempDir,
    server: tmux_server::TmuxServer,
}
impl Env {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        for sub in ["home", "config", "data", "work"] {
            std::fs::create_dir_all(root.path().join(sub)).unwrap();
        }
        std::fs::write(
            root.path().join("config/settings.toml"),
            "[features]\nautomations = false\nversion_check = false\nauto_update = false\n",
        )
        .unwrap();
        // The shell is pinned, prompt included, as tests/send_keys_e2e.rs pins
        // it, so the shell and its prompt are the same on every machine: `sh`
        // is bash on one and dash on another.
        std::fs::write(
            root.path().join("config/agents.toml"),
            "default = 'shell'\n[[agents]]\nname = 'shell'\ncommand = 'env'\nargs = ['PS1=bash-5.3$ ', 'bash', '--norc', '--noprofile', '-i']\n[[agents]]\nname = 'claude'\ncommand = 'sh'\nhook_schema = 'claude'\n",
        )
        .unwrap();
        Self {
            root,
            server: tmux_server::TmuxServer::private("event-consumers"),
        }
    }
    fn path(&self, p: &str) -> PathBuf {
        self.root.path().join(p)
    }
    fn db(&self) -> Database {
        Database::open(&self.path("data/thurbox.db")).unwrap()
    }
    fn command(&self, args: &[&str]) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_thurbox-cli"));
        cmd.arg("--json")
            .args(args)
            .env("HOME", self.path("home"))
            .env("USERPROFILE", self.path("home"))
            .env("THURBOX_CONFIG_DIR", self.path("config"))
            .env("THURBOX_DATA_DIR", self.path("data"))
            .env_remove("THURBOX_SESSION")
            .env_remove("THURBOX_SESSION_ID")
            .env_remove("CLAUDE_CONFIG_DIR");
        for var in [
            "GIT_DIR",
            "GIT_WORK_TREE",
            "GIT_COMMON_DIR",
            "GIT_INDEX_FILE",
            "GIT_OBJECT_DIRECTORY",
            "GIT_ALTERNATE_OBJECT_DIRECTORIES",
            "GIT_PREFIX",
            "GIT_NAMESPACE",
        ] {
            cmd.env_remove(var);
        }
        self.server.scope(&mut cmd);
        cmd
    }
    fn run(&self, args: &[&str]) -> Output {
        self.command(args).output().unwrap()
    }
    fn ok(&self, args: &[&str]) -> Value {
        let out = self.run(args);
        assert!(
            out.status.success(),
            "{args:?}: {} {}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        serde_json::from_slice(&out.stdout).unwrap()
    }
    fn seed(&self) -> SessionId {
        let row = SharedSession {
            id: SessionId::default(),
            name: "worker".into(),
            agent: "claude".into(),
            backend_type: "local-tmux".into(),
            backend_id: "%999".into(),
            cwd: Some(self.path("work")),
            agent_session_id: Some("conversation".into()),
            additional_dirs: vec![],
            worktrees: vec![],
            shell_backend_id: None,
            parent_session_id: None,
            display_order: None,
            tombstone: false,
            tombstone_at: None,
            key: None,
        };
        self.db().upsert_session(&row).unwrap();
        row.id
    }
    fn hooks(&self, text: &str) {
        std::fs::write(self.path("config/hooks.toml"), text).unwrap();
    }
    /// Create the `worker` shell session and wait for its prompt: a send into
    /// a shell that has not drawn one yet cannot confirm its submission, and
    /// how soon it draws one depends on the machine.
    fn create(&self) -> Value {
        let created = self.ok(&[
            "session",
            "create",
            "--name",
            "worker",
            "--agent",
            "shell",
            "--repo-path",
            self.path("work").to_str().unwrap(),
        ]);
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        loop {
            let capture = self.ok(&["session", "capture", "worker"]);
            if capture["output"]
                .as_str()
                .unwrap_or_default()
                .contains("bash-5.3$")
            {
                return created;
            }
            assert!(std::time::Instant::now() < deadline, "no prompt: {capture}");
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

#[test]
fn wait_returns_current_state_or_exact_next_edge() {
    let env = Env::new();
    let id = env.seed();
    let db = env.db();
    db.set_hook_state(id, "working").unwrap();
    let event = env.ok(&[
        "session",
        "wait",
        "worker",
        "--state",
        "working",
        "--timeout",
        "0",
    ]);
    assert_eq!(event["event"], "present");
    assert_eq!(event["to_state"], "working");
    let mut cmd = env.command(&[
        "session",
        "wait",
        "worker",
        "--state",
        "blocked",
        "--timeout",
        "5",
    ]);
    let writer = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(700));
        let db = env.db();
        db.set_hook_state(id, "blocked").unwrap();
        db.set_hook_state(id, "done").unwrap();
        env
    });
    // A second process observes both transitions even when they share one poll.
    let out = cmd.output().unwrap();
    let _env = writer.join().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
    let event: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(event["event"], "changed");
    assert_eq!(event["to_state"], "blocked");
}

#[test]
fn wait_timeout_has_exit_four_and_no_matching_event() {
    let env = Env::new();
    env.seed();
    let out = env.run(&[
        "session",
        "wait",
        "worker",
        "--state",
        "done",
        "--timeout",
        "0",
    ]);
    assert_eq!(
        out.status.code(),
        Some(4),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert!(v.get("event").is_none());
}

#[test]
fn wait_reports_a_session_that_leaves_with_exit_six() {
    let env = Env::new();
    let id = env.seed();
    let mut cmd = env.command(&[
        "session",
        "wait",
        "worker",
        "--state",
        "done",
        "--timeout",
        "5",
    ]);
    let thread = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(700));
        env.db().soft_delete_session(id).unwrap();
        env
    });
    let out = cmd.output().unwrap();
    let _env = thread.join().unwrap();
    assert_eq!(
        out.status.code(),
        Some(6),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
}

#[test]
fn state_hooks_resume_filter_and_report_sequence_without_an_interface() {
    let env = Env::new();
    let id = env.seed();
    env.hooks(&format!(
        "[[hooks]]\nevent = 'session.state_changed'\nto = ['done']\ncommand = 'cat >> {}'\n",
        env.path("events").display()
    ));
    env.ok(&["config", "validate"]);
    env.ok(&["automation", "tick"]); // first use starts at head
    env.db().set_hook_state(id, "working").unwrap();
    env.db().set_hook_state(id, "done").unwrap();
    env.ok(&["automation", "tick"]);
    env.ok(&["automation", "tick"]);
    let text = std::fs::read_to_string(env.path("events")).unwrap();
    let events: Vec<Value> = text
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0]["from_state"], "working");
    assert_eq!(events[0]["to_state"], "done");
    assert!(events[0]["seq"].as_i64().unwrap() > 0);
}

#[test]
fn state_hook_filters_reject_unknown_states() {
    let env = Env::new();
    env.hooks("[[hooks]]\nevent = 'session.state_changed'\nto = ['imaginary']\ncommand = 'true'\n");
    let out = env.run(&["config", "validate"]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stdout).contains("imaginary"));
}

#[test]
fn pre_launch_runs_in_final_directory_and_can_refuse_the_launch() {
    let env = Env::new();
    env.hooks("[[hooks]]\nevent = 'session.pre_launch'\nagents = ['shell']\ncommand = 'test \"$PWD\" = \"$THURBOX_CWD\" && echo preparation-refused >&2; exit 1'\n");
    let out = env.run(&[
        "session",
        "create",
        "--name",
        "worker",
        "--agent",
        "shell",
        "--repo-path",
        env.path("work").to_str().unwrap(),
    ]);
    assert!(
        !out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
    assert!(String::from_utf8_lossy(&out.stdout).contains("preparation-refused"));
    // A refusal, typed as the pre_create one is, so a driver tells "a hook said
    // no" from a failure without matching the sentence.
    let answer: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(answer["error_kind"], "refused", "{answer}");
    assert!(env.db().list_active_sessions().unwrap().is_empty());
}

#[test]
fn transcript_is_a_read_field_and_unknown_agents_have_null() {
    let env = Env::new();
    env.seed();
    let dir = env.path("home/.claude/projects/project");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("conversation.jsonl");
    std::fs::write(&path, "{}\n").unwrap();
    let row = env.ok(&["session", "get", "worker", "--no-verify"]);
    assert_eq!(row["transcript_path"], path.to_str().unwrap());
    assert!(row.get("last_output_at").is_some());
    let rows = env.ok(&["session", "list"]);
    assert_eq!(rows[0]["transcript_path"], row["transcript_path"]);
    let events = env.run(&["watch", "--initial", "--for-secs", "0"]);
    let event: Value = serde_json::from_slice(&events.stdout).unwrap();
    assert_eq!(event["transcript_path"], row["transcript_path"]);
}

#[test]
fn pane_wait_matches_capture_and_output_age_works_headlessly() {
    let env = Env::new();
    if !env.server.tmux(&["-V"]).status.success() {
        return;
    }
    env.create();
    env.ok(&[
        "session",
        "send",
        "worker",
        "printf 'ready-marker\\n'",
        "--force",
    ]);
    let event = env.ok(&[
        "session",
        "wait",
        "worker",
        "--pane-matches",
        "ready-marker",
        "--timeout",
        "5",
    ]);
    assert!(event["detail"].as_str().unwrap().contains("ready-marker"));
    let row = env.ok(&["session", "get", "worker", "--no-verify"]);
    assert!(row["last_output_at"].as_u64().unwrap() > 0);
    assert!(row["transcript_path"].is_null());
}

#[test]
fn send_wait_turn_times_out_when_no_working_edge_follows() {
    let env = Env::new();
    if !env.server.tmux(&["-V"]).status.success() {
        return;
    }
    env.create();
    let out = env.run(&[
        "session",
        "send",
        "worker",
        "true",
        "--force",
        "--wait-turn",
        "--timeout",
        "1",
    ]);
    assert_eq!(
        out.status.code(),
        Some(4),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
    // The text was delivered: a driver that retried on exit 4 without seeing
    // that would type it twice.
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["sent"], true, "{v}");
    assert!(v["turn"].is_null(), "{v}");
}

#[test]
fn hooks_report_retention_gaps_once_and_do_not_replay_old_history() {
    let env = Env::new();
    let id = env.seed();
    env.db().set_hook_state(id, "done").unwrap();
    env.hooks(&format!(
        "[[hooks]]\nevent = 'session.state_changed'\ncommand = 'cat >> {}'\n",
        env.path("events").display()
    ));
    env.ok(&["automation", "tick"]);
    assert!(!env.path("events").exists());
    let db = env.db();
    db.set_hook_state(id, "working").unwrap();
    db.set_hook_state(id, "blocked").unwrap();
    db.conn_ref()
        .execute(
            "DELETE FROM session_events WHERE seq < (SELECT MAX(seq) FROM session_events)",
            [],
        )
        .unwrap();
    env.ok(&["automation", "tick"]);
    let text = std::fs::read_to_string(env.path("events")).unwrap();
    let event: Value = serde_json::from_str(text.trim()).unwrap();
    assert_eq!(event["gap"], true);
    db.set_hook_state(id, "done").unwrap();
    env.ok(&["automation", "tick"]);
    let text = std::fs::read_to_string(env.path("events")).unwrap();
    let events: Vec<Value> = text
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(events[1]["gap"], false);
}

#[test]
fn a_failed_state_hook_does_not_stop_the_other_hooks() {
    let env = Env::new();
    let id = env.seed();
    env.hooks(&format!("[[hooks]]\nevent = 'session.state_changed'\ncommand = 'sleep 10'\ntimeout_secs = 1\n[[hooks]]\nevent = 'session.state_changed'\ncommand = 'cat >> {}'\n", env.path("events").display()));
    env.ok(&["automation", "tick"]);
    env.db().set_hook_state(id, "done").unwrap();
    env.ok(&["automation", "tick"]);
    assert!(env.path("events").exists());
}

#[test]
fn pre_launch_agent_filter_does_not_run_for_other_agents() {
    let env = Env::new();
    if !env.server.tmux(&["-V"]).status.success() {
        return;
    }
    env.hooks("[[hooks]]\nevent = 'session.pre_launch'\nagents = ['claude']\ncommand = 'exit 1'\n");
    env.create();
}

#[test]
fn strict_send_accepts_a_confirmed_submission() {
    let env = Env::new();
    if !env.server.tmux(&["-V"]).status.success() {
        return;
    }
    env.create();
    let result = env.ok(&["session", "send", "worker", "true", "--force", "--strict"]);
    assert_eq!(result["submitted"], true);
}

#[test]
fn strict_send_rejects_an_unconfirmable_dialog_but_legacy_send_still_succeeds() {
    for strict in [false, true] {
        let env = Env::new();
        if !env.server.tmux(&["-V"]).status.success() {
            return;
        }
        let script = "stty -icanon -echo; printf '❯ '; while :; do c=$(dd bs=1 count=1 2>/dev/null); if [ -n \"$c\" ]; then printf '%s' \"$c\"; else printf '\\nProceed? Yes'; fi; done";
        env.ok(&[
            "session",
            "create",
            "--name",
            "worker",
            "--repo-path",
            env.path("work").to_str().unwrap(),
            "--command",
            "sh",
            "--arg=-c",
            "--arg",
            script,
        ]);
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        loop {
            let capture = env.ok(&["session", "capture", "worker"]);
            if capture["output"].as_str().unwrap().contains('❯') {
                break;
            }
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(50));
        }
        let mut args = vec!["session", "send", "worker", "hello-dialog", "--force"];
        if strict {
            args.push("--strict");
        }
        let out = env.run(&args);
        let v: Value = serde_json::from_slice(&out.stdout).unwrap();
        assert!(v["submitted"].is_null(), "{v}");
        assert_eq!(out.status.success(), !strict, "{v}");
    }
}

#[test]
fn wait_observes_stop_and_loss_edges() {
    for stopped in [true, false] {
        let env = Env::new();
        let id = env.seed();
        let mut cmd = env.command(&[
            "session",
            "wait",
            "worker",
            "--state",
            "done",
            "--timeout",
            "5",
        ]);
        let writer = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(700));
            if stopped {
                env.db().set_session_stopped(id, true).unwrap();
            } else {
                env.db().record_session_lost(id, "%999").unwrap();
            }
            env
        });
        let out = cmd.output().unwrap();
        let _env = writer.join().unwrap();
        assert_eq!(
            out.status.code(),
            Some(6),
            "{}",
            String::from_utf8_lossy(&out.stdout)
        );
    }
}

#[test]
fn transcript_follows_the_session_account_and_family() {
    let env = Env::new();
    let id = env.seed();
    let db = env.db();
    let mut row = db.get_session_by_id(id).unwrap().unwrap();
    row.agent = "custom-account".into();
    db.upsert_session(&row).unwrap();
    std::fs::write(
        env.path("config/agents.toml"),
        "[[agents]]\nname = 'custom-account'\ncommand = 'sh'\nhook_schema = 'claude'\n",
    )
    .unwrap();
    let root = env.path("account");
    let dir = root.join("projects/project");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("conversation.jsonl");
    std::fs::write(&path, "{}\n").unwrap();
    db.set_launch_env(
        id,
        &std::collections::BTreeMap::from([(
            "CLAUDE_CONFIG_DIR".into(),
            root.to_string_lossy().to_string(),
        )]),
    )
    .unwrap();
    let value = env.ok(&["session", "get", "worker", "--no-verify"]);
    assert_eq!(value["transcript_path"], path.to_str().unwrap());
    row.agent = "codex".into();
    db.upsert_session(&row).unwrap();
    let value = env.ok(&["session", "get", "worker", "--no-verify"]);
    assert!(value["transcript_path"].is_null());
}

#[test]
fn pre_launch_sees_every_worktree_and_finishes_before_the_agent_starts() {
    let env = Env::new();
    if !env.server.tmux(&["-V"]).status.success() {
        return;
    }
    for sub in ["repo-a", "repo-b"] {
        let dir = env.path(sub);
        std::fs::create_dir_all(&dir).unwrap();
        for args in [
            vec!["init", "-b", "main"],
            vec![
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@example.invalid",
                "commit",
                "--allow-empty",
                "-m",
                "initial",
            ],
        ] {
            let mut git = Command::new("git");
            git.args(args).current_dir(&dir);
            for var in [
                "GIT_DIR",
                "GIT_WORK_TREE",
                "GIT_COMMON_DIR",
                "GIT_INDEX_FILE",
                "GIT_OBJECT_DIRECTORY",
                "GIT_ALTERNATE_OBJECT_DIRECTORIES",
                "GIT_PREFIX",
                "GIT_NAMESPACE",
            ] {
                git.env_remove(var);
            }
            assert!(git.output().unwrap().status.success());
        }
    }
    std::fs::write(env.path("config/agents.toml"), "[[agents]]\nname = 'prepared'\ncommand = 'sh'\nargs = ['-c', 'test -f prepared && echo launch-after-preparation; sleep 60']\n").unwrap();
    env.hooks(&format!("[[hooks]]\nevent = 'session.pre_launch'\nagents = ['prepared']\ncommand = 'cat > {}; touch prepared'\n", env.path("payload").display()));
    env.ok(&[
        "session",
        "create",
        "--name",
        "worker",
        "--agent",
        "prepared",
        "--repo-path",
        env.path("repo-a").to_str().unwrap(),
        "--worktree-branch",
        "feature",
        "--add-repo",
        env.path("repo-b").to_str().unwrap(),
    ]);
    let payload: Value =
        serde_json::from_slice(&std::fs::read(env.path("payload")).unwrap()).unwrap();
    assert_eq!(payload["worktrees"].as_array().unwrap().len(), 2);
    for tree in payload["worktrees"].as_array().unwrap() {
        assert!(std::path::Path::new(tree["worktree_path"].as_str().unwrap()).is_dir());
    }
    assert!(std::path::Path::new(payload["cwd"].as_str().unwrap())
        .join("prepared")
        .is_file());
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        let capture = env.ok(&["session", "capture", "worker"]);
        if capture["output"]
            .as_str()
            .unwrap()
            .contains("launch-after-preparation")
        {
            break;
        }
        assert!(std::time::Instant::now() < deadline, "{capture}");
        std::thread::sleep(Duration::from_millis(50));
    }
    env.ok(&["session", "delete", "worker", "--force"]);
}

#[test]
fn an_extension_cannot_install_lifecycle_hooks_as_an_external_file() {
    let env = Env::new();
    let source = env.path("extension");
    std::fs::create_dir_all(&source).unwrap();
    std::fs::write(
        source.join("extension.toml"),
        format!(
            "name = 'hook-injection'\n[[external_files]]\npath = '{}'\nsource = 'payload.toml'\n",
            env.path("config/hooks.toml").display()
        ),
    )
    .unwrap();
    std::fs::write(
        source.join("payload.toml"),
        "# thurbox managed\n[[hooks]]\nevent = 'session.pre_launch'\ncommand = 'true'\n",
    )
    .unwrap();
    let out = env.run(&["extension", "install", source.to_str().unwrap(), "--force"]);
    assert!(
        !out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
    assert!(String::from_utf8_lossy(&out.stdout).contains("hooks.toml"));
}

#[test]
fn a_state_hook_replays_when_its_effect_precedes_a_failed_cursor_commit() {
    let env = Env::new();
    let id = env.seed();
    let db = env.db();
    env.hooks(&format!(
        "[[hooks]]\nevent = 'session.state_changed'\ncommand = 'cat >> {}'\n",
        env.path("events").display()
    ));
    env.ok(&["automation", "tick"]);
    db.set_hook_state(id, "done").unwrap();
    db.conn_ref().execute_batch("CREATE TRIGGER refuse_cursor BEFORE UPDATE OF cursor ON event_cursors WHEN OLD.consumer = 'hooks' AND NEW.cursor > OLD.cursor BEGIN SELECT RAISE(ABORT, 'injected cursor failure'); END;").unwrap();
    let report = env.ok(&["automation", "tick"]);
    assert!(report["hooks"]["error"]
        .as_str()
        .unwrap()
        .contains("injected cursor failure"));
    db.conn_ref()
        .execute_batch("DROP TRIGGER refuse_cursor")
        .unwrap();
    env.ok(&["automation", "tick"]);
    let text = std::fs::read_to_string(env.path("events")).unwrap();
    let events: Vec<Value> = text
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(events.len(), 2);
    assert_eq!(events[0]["seq"], events[1]["seq"]);
}

#[test]
fn state_hook_backlog_is_bounded_to_one_batch_per_tick() {
    let env = Env::new();
    let id = env.seed();
    let db = env.db();
    env.hooks("[[hooks]]\nevent = 'session.state_changed'\nto = ['done']\ncommand = 'true'\n");
    env.ok(&["automation", "tick"]);
    let start = db.latest_session_event_seq().unwrap();
    for n in 0..600 {
        db.set_hook_state(id, if n % 2 == 0 { "working" } else { "blocked" })
            .unwrap();
    }
    let report = env.ok(&["automation", "tick"]);
    assert_eq!(report["hooks"]["cursor"], start + 512);
    assert_eq!(report["hooks"]["attempted"], 0);
    let report = env.ok(&["automation", "tick"]);
    assert_eq!(
        report["hooks"]["cursor"],
        db.latest_session_event_seq().unwrap()
    );
}

#[test]
fn concurrent_ticks_deliver_a_state_hook_once_under_the_lease() {
    let env = Env::new();
    let id = env.seed();
    env.hooks(&format!("[[hooks]]\nevent = 'session.state_changed'\ncommand = 'cat >> {}; sleep 1'\ntimeout_secs = 3\n", env.path("events").display()));
    env.ok(&["automation", "tick"]);
    env.db().set_hook_state(id, "done").unwrap();
    let mut a = env.command(&["automation", "tick"]);
    let mut b = env.command(&["automation", "tick"]);
    std::thread::scope(|scope| {
        let a = scope.spawn(|| a.output().unwrap());
        let b = scope.spawn(|| b.output().unwrap());
        assert!(a.join().unwrap().status.success());
        assert!(b.join().unwrap().status.success());
    });
    assert_eq!(
        std::fs::read_to_string(env.path("events"))
            .unwrap()
            .lines()
            .count(),
        1
    );
}

#[test]
fn a_wait_can_match_an_already_stopped_session() {
    let env = Env::new();
    let id = env.seed();
    env.db().set_session_stopped(id, true).unwrap();
    let value = env.ok(&[
        "session",
        "wait",
        "worker",
        "--state",
        "stopped",
        "--timeout",
        "0",
    ]);
    assert_eq!(value["state"], "stopped");
    assert_eq!(value["event"], "present");
}

#[test]
fn output_time_changes_for_output_and_not_for_a_reader() {
    let env = Env::new();
    if !env.server.tmux(&["-V"]).status.success() {
        return;
    }
    env.create();
    let first = env.ok(&["session", "get", "worker", "--no-verify"])["last_output_at"]
        .as_u64()
        .unwrap();
    std::thread::sleep(Duration::from_millis(1100));
    assert_eq!(
        env.ok(&["session", "get", "worker", "--no-verify"])["last_output_at"],
        first
    );
    env.ok(&[
        "session",
        "send",
        "worker",
        "printf 'new-output\\n'",
        "--force",
    ]);
    let second = env.ok(&["session", "get", "worker", "--no-verify"])["last_output_at"]
        .as_u64()
        .unwrap();
    assert!(second > first);
}

#[test]
fn the_cli_reports_its_additive_event_features() {
    let env = Env::new();
    let value = env.ok(&["version"]);
    for field in ["session_wait", "session_state_hooks", "session_pre_launch"] {
        assert_eq!(value[field], true, "{field}: {value}");
    }
}

#[test]
fn extension_hook_protection_covers_merges_payloads_symlinks_and_dangling_aliases() {
    for kind in ["config_merges", "files", "symlinks", "external_files"] {
        let env = Env::new();
        let source = env.path("extension");
        std::fs::create_dir_all(&source).unwrap();
        let target = if kind == "external_files" {
            let alias = env.path("alias.toml");
            std::os::unix::fs::symlink(env.path("config/hooks.toml"), &alias).unwrap();
            alias.to_string_lossy().to_string()
        } else if kind == "config_merges" {
            env.path("config/hooks.toml").to_string_lossy().to_string()
        } else {
            "hooks.toml".into()
        };
        let entry = if kind == "symlinks" {
            format!("link = '{target}'\ntarget = 'payload.toml'\n")
        } else {
            format!("path = '{target}'\nsource = 'payload.toml'\n")
        };
        std::fs::write(
            source.join("extension.toml"),
            format!("name = 'hook-injection'\n[[{kind}]]\n{entry}"),
        )
        .unwrap();
        std::fs::write(source.join("payload.toml"), "{}\n").unwrap();
        let out = env.run(&[
            "extension",
            "install",
            source.to_str().unwrap(),
            "--home",
            env.path("config").to_str().unwrap(),
            "--force",
        ]);
        assert!(
            !out.status.success(),
            "{kind}: {}",
            String::from_utf8_lossy(&out.stdout)
        );
        assert!(String::from_utf8_lossy(&out.stdout).contains("hooks.toml"));
    }
}

#[test]
fn a_long_first_hook_cannot_starve_the_rest_of_its_event_forever() {
    let env = Env::new();
    let id = env.seed();
    let mut hooks =
        "[[hooks]]\nevent = 'session.state_changed'\ncommand = 'sleep 60'\ntimeout_secs = 60\n"
            .to_string();
    for _ in 0..98 {
        hooks.push_str("[[hooks]]\nevent = 'session.state_changed'\ncommand = 'true'\n");
    }
    hooks.push_str(&format!(
        "[[hooks]]\nevent = 'session.state_changed'\ncommand = 'touch {}'\n",
        env.path("last-hook").display()
    ));
    env.hooks(&hooks);
    env.ok(&["automation", "tick"]);
    env.db().set_hook_state(id, "done").unwrap();
    env.ok(&["automation", "tick"]);
    assert!(env.path("last-hook").exists());
}

#[test]
fn output_time_uses_the_session_owner_instead_of_a_reused_pane_hint() {
    let env = Env::new();
    if !env.server.tmux(&["-V"]).status.success() {
        return;
    }
    let a = env.create();
    let own_time = env.ok(&["session", "get", "worker", "--no-verify"])["last_output_at"]
        .as_u64()
        .unwrap();
    std::thread::sleep(Duration::from_millis(1100));
    let b = env.ok(&[
        "session",
        "create",
        "--name",
        "other",
        "--agent",
        "shell",
        "--repo-path",
        env.path("work").to_str().unwrap(),
    ]);
    let id: SessionId = a["id"]
        .as_str()
        .or(a["session_id"].as_str())
        .unwrap()
        .parse()
        .unwrap();
    let db = env.db();
    let mut row = db.get_session_by_id(id).unwrap().unwrap();
    row.backend_id = b["backend_id"].as_str().unwrap().into();
    db.upsert_session(&row).unwrap();
    let read = env.ok(&["session", "get", "worker", "--no-verify"]);
    assert_eq!(read["last_output_at"], own_time);
}

#[test]
fn send_wait_turn_observes_a_new_working_edge_even_when_already_working() {
    let env = Env::new();
    if !env.server.tmux(&["-V"]).status.success() {
        return;
    }
    let created = env.create();
    let id: SessionId = created["id"]
        .as_str()
        .or(created["session_id"].as_str())
        .unwrap()
        .parse()
        .unwrap();
    let db = env.db();
    db.set_hook_state(id, "working").unwrap();
    let before = db.latest_session_event_seq().unwrap();
    // The binary under test, by path: a `thurbox-cli` on the pane's PATH is
    // whatever happens to be installed on the machine, or nothing at all.
    let cli = env!("CARGO_BIN_EXE_thurbox-cli");
    let signals =
        format!("'{cli}' session signal --state blocked; '{cli}' session signal --state working");
    let result = env.ok(&[
        "session",
        "send",
        "worker",
        &signals,
        "--force",
        "--wait-turn",
        "--timeout",
        "10",
    ]);
    assert_eq!(result["turn"]["from_state"], "blocked");
    assert_eq!(result["turn"]["to_state"], "working");
    assert!(result["turn"]["seq"].as_i64().unwrap() > before);
}

/// The read fields are local evidence. A row on a host is answered with
/// `null` rather than an ssh round trip: `session list`, `get --no-verify` and
/// every `watch` batch must not wait on a host — an unreachable one least of all.
#[test]
fn read_fields_never_ask_a_host_for_a_remote_row() {
    use std::os::unix::fs::PermissionsExt;
    let env = Env::new();
    std::fs::write(
        env.path("config/hosts.toml"),
        "[[hosts]]\nname = 'farhost'\ndestination = 'e2e@farhost.invalid'\nmultiplexer = 'tmux'\nshare_sessions = false\n",
    )
    .unwrap();
    let bin = env.path("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let log = env.path("ssh.log");
    std::fs::write(
        bin.join("ssh"),
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\nexit 255\n",
            log.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(bin.join("ssh"), std::fs::Permissions::from_mode(0o700)).unwrap();
    let path = std::env::join_paths(
        std::iter::once(bin).chain(std::env::split_paths(&std::env::var_os("PATH").unwrap())),
    )
    .unwrap();
    let id = env.seed();
    let db = env.db();
    let mut row = db.get_session_by_id(id).unwrap().unwrap();
    row.backend_type = thurbox::session::Route::remote(
        thurbox::session::Via::Ssh,
        "farhost",
        Some(thurbox::session::Multiplexer::Tmux),
    )
    .format();
    db.upsert_session(&row).unwrap();
    db.set_hook_state(id, "working").unwrap();
    // `watch` prints one line per event; the last is the row's latest edge.
    let run = |args: &[&str]| -> Value {
        let out = env.command(args).env("PATH", &path).output().unwrap();
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(out.status.success(), "{args:?}: {stdout}");
        serde_json::from_str(stdout.trim().lines().last().unwrap()).unwrap()
    };

    let got = run(&["session", "get", "worker", "--no-verify"]);
    assert!(got["last_output_at"].is_null(), "{got}");
    assert!(got["transcript_path"].is_null(), "{got}");
    let listed = run(&["session", "list"]);
    assert!(listed[0]["last_output_at"].is_null(), "{listed}");
    let watched = run(&["watch", "--since", "0", "--for-secs", "0"]);
    assert!(watched["last_output_at"].is_null(), "{watched}");
    // A wait holds the full registry, which can reach the host: the remote
    // gate is what keeps its read fields local.
    let waited = run(&[
        "session",
        "wait",
        "worker",
        "--state",
        "working",
        "--timeout",
        "0",
    ]);
    assert!(waited["last_output_at"].is_null(), "{waited}");

    let asked = std::fs::read_to_string(&log).unwrap_or_default();
    assert!(asked.is_empty(), "a read field asked the host:\n{asked}");
}

#[test]
fn extension_hook_protection_resolves_relative_destinations_from_the_install_cwd() {
    for kind in ["external_files", "config_merges"] {
        let env = Env::new();
        let source = env.path("extension");
        std::fs::create_dir_all(&source).unwrap();
        let format = if kind == "config_merges" {
            "format = 'toml'\n"
        } else {
            ""
        };
        std::fs::write(source.join("extension.toml"), format!("name = 'relative-injection'\n[[{kind}]]\npath = 'hooks.toml'\nsource = 'payload.toml'\n{format}")).unwrap();
        std::fs::write(
            source.join("payload.toml"),
            "[[hooks]]\nevent = 'session.pre_launch'\ncommand = 'true'\n",
        )
        .unwrap();
        let out = env
            .command(&["extension", "install", source.to_str().unwrap(), "--force"])
            .current_dir(env.path("config"))
            .output()
            .unwrap();
        assert!(
            !out.status.success(),
            "{kind}: {}",
            String::from_utf8_lossy(&out.stdout)
        );
        assert!(String::from_utf8_lossy(&out.stdout).contains("hooks.toml"));
    }
}

#[test]
fn a_wait_matches_a_stop_that_happens_while_it_waits() {
    let env = Env::new();
    let id = env.seed();
    let mut cmd = env.command(&[
        "session",
        "wait",
        "worker",
        "--state",
        "stopped",
        "--timeout",
        "5",
    ]);
    let writer = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(700));
        env.db().set_session_stopped(id, true).unwrap();
        env
    });
    let out = cmd.output().unwrap();
    let _env = writer.join().unwrap();
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
    let event: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(event["state"], "stopped");
    assert_eq!(event["reason"], "stopped");
}

/// Only the hook words and the terminal reasons are ever an edge's
/// destination, so a filter naming a derived state would never fire.
#[test]
fn state_hook_filters_reject_states_no_edge_carries() {
    for state in ["running", "unreachable", "uncovered", "unreported"] {
        let env = Env::new();
        env.hooks(&format!(
            "[[hooks]]\nevent = 'session.state_changed'\nto = ['{state}']\ncommand = 'true'\n"
        ));
        let out = env.run(&["config", "validate"]);
        assert!(!out.status.success(), "{state} was accepted");
        assert!(String::from_utf8_lossy(&out.stdout).contains(state));
    }
}

/// An extension's name becomes its manifest's file name, so a name that
/// climbs out of the extensions directory would overwrite hooks.toml.
#[test]
fn an_extension_name_cannot_climb_out_of_the_extensions_directory() {
    let env = Env::new();
    let hooks = "config_version = 1\n[[hooks]]\nevent = 'session.pre_create'\ncommand = 'true'\n";
    env.hooks(hooks);
    let source = env.path("extension");
    std::fs::create_dir_all(&source).unwrap();
    std::fs::write(source.join("extension.toml"), "name = '../hooks'\n").unwrap();
    let out = env.run(&["extension", "install", source.to_str().unwrap(), "--force"]);
    assert!(
        !out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
    assert_eq!(
        std::fs::read_to_string(env.path("config/hooks.toml")).unwrap(),
        hooks
    );
}

/// Retention can empty the log entirely. The next edge is news, not a gap: a
/// consumer that starts on the empty log missed nothing.
#[test]
fn a_first_claim_on_an_emptied_log_reports_no_gap() {
    let env = Env::new();
    let id = env.seed();
    let db = env.db();
    db.set_hook_state(id, "working").unwrap();
    db.conn_ref()
        .execute("DELETE FROM session_events", [])
        .unwrap();
    env.hooks(&format!(
        "[[hooks]]\nevent = 'session.state_changed'\ncommand = 'cat >> {}'\n",
        env.path("events").display()
    ));
    env.ok(&["automation", "tick"]);
    db.set_hook_state(id, "done").unwrap();
    env.ok(&["automation", "tick"]);
    let text = std::fs::read_to_string(env.path("events")).unwrap();
    let event: Value = serde_json::from_str(text.trim()).unwrap();
    assert_eq!(event["gap"], false, "{event}");
}

/// A session already marked lost has no pane to reach any state: its `lost`
/// edge predates the wait's cursor, so the mark itself ends the wait.
#[test]
fn a_wait_on_an_already_lost_session_exits_six() {
    let env = Env::new();
    let id = env.seed();
    let db = env.db();
    db.set_hook_state(id, "done").unwrap();
    assert!(db.record_session_lost(id, "%999").unwrap());
    for state in ["done", "blocked"] {
        let out = env.run(&[
            "session",
            "wait",
            "worker",
            "--state",
            state,
            "--timeout",
            "2",
        ]);
        assert_eq!(
            out.status.code(),
            Some(6),
            "{state}: {}",
            String::from_utf8_lossy(&out.stdout)
        );
    }
}

/// The saved manifest is a write too. A symlink in the extensions directory
/// named like the extension must not carry that write to hooks.toml.
#[test]
fn an_extension_manifest_alias_cannot_overwrite_hooks() {
    let env = Env::new();
    let hooks = "config_version = 1\n[[hooks]]\nevent = 'session.pre_create'\ncommand = 'true'\n";
    env.hooks(hooks);
    std::fs::create_dir_all(env.path("config/extensions")).unwrap();
    std::os::unix::fs::symlink(
        env.path("config/hooks.toml"),
        env.path("config/extensions/alias.toml"),
    )
    .unwrap();
    let source = env.path("extension");
    std::fs::create_dir_all(&source).unwrap();
    std::fs::write(source.join("extension.toml"), "name = 'alias'\n").unwrap();
    let out = env.run(&["extension", "install", source.to_str().unwrap(), "--force"]);
    assert!(
        !out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
    assert!(String::from_utf8_lossy(&out.stdout).contains("hooks.toml"));
    assert_eq!(
        std::fs::read_to_string(env.path("config/hooks.toml")).unwrap(),
        hooks
    );
}

/// A `done` edge is the moment the turn finished, before anyone looked at it.
/// The user acknowledging the session before the wait reads that edge must not
/// turn it into `idle` and leave the wait for `done` hanging.
#[test]
fn a_wait_matches_a_done_edge_that_was_seen_before_it_was_read() {
    let env = Env::new();
    let id = env.seed();
    env.db().set_hook_state(id, "working").unwrap();
    let mut cmd = env.command(&[
        "session",
        "wait",
        "worker",
        "--state",
        "done",
        "--timeout",
        "5",
    ]);
    let writer = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(700));
        let db = env.db();
        db.set_hook_state(id, "done").unwrap();
        // What the interface does when the user looks: seen as of now.
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64;
        db.mark_session_seen(id, now + 1_000).unwrap();
        env
    });
    let out = cmd.output().unwrap();
    let _env = writer.join().unwrap();
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
    let event: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(event["to_state"], "done");
    assert_eq!(event["state"], "done");
}

/// A state hook's payload describes the edge as it happened: a `done` the user
/// looked at before the tick delivered it is still `done` on stdin.
#[test]
fn a_state_hook_reports_a_done_edge_seen_before_the_tick_as_done() {
    let env = Env::new();
    let id = env.seed();
    env.hooks(&format!(
        "[[hooks]]\nevent = 'session.state_changed'\ncommand = 'cat >> {}'\n",
        env.path("events").display()
    ));
    env.ok(&["automation", "tick"]);
    let db = env.db();
    db.set_hook_state(id, "done").unwrap();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64;
    db.mark_session_seen(id, now + 1_000).unwrap();
    env.ok(&["automation", "tick"]);
    let text = std::fs::read_to_string(env.path("events")).unwrap();
    let event: Value = serde_json::from_str(text.trim()).unwrap();
    assert_eq!(event["to_state"], "done", "{event}");
    assert_eq!(event["state"], "done", "{event}");
}
