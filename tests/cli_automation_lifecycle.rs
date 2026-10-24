//! What the automation verbs leave on the row and in the run history, driven
//! through the real binary.
//!
//! The first group pins places where a verb changed more, or less, of an
//! automation than it says it does: a delete that left a send armed against a
//! session that is gone, a run that switched a disabled automation back on, a
//! run row that only appeared once the run was over, and an edit that wrote
//! the whole row back from what it had read. The rest pin the primitive an
//! orchestrator drives headlessly (issues #1468, #1507, #1508, #1515, #1517): a
//! bounded exec with its own cwd, env and deadline; a run row that says how it
//! ended; a keyed, idempotent create and references by id, key or name; a
//! synchronous `run --wait`; and a tick that leaves a trace when it fails.

use std::path::PathBuf;
use std::process::{Command, Output};
#[cfg(unix)]
use std::time::{Duration, Instant};

use serde_json::Value;
use thurbox::session::SessionId;
use thurbox::sync::SharedSession;

#[path = "support/tmux_server.rs"]
mod tmux_server;

/// A throwaway thurbox instance with the heartbeat off: nothing fires here
/// except what a test runs itself.
struct Env {
    server: tmux_server::TmuxServer,
    root: tempfile::TempDir,
}

impl Env {
    fn new() -> Self {
        let root = tempfile::TempDir::new().expect("tempdir");
        for sub in ["home", "config", "data", "tmux", "work"] {
            std::fs::create_dir_all(root.path().join(sub)).expect("mkdir");
        }
        std::fs::write(
            root.path().join("config/settings.toml"),
            "[features]\nautomations = false\nversion_check = false\nauto_update = false\n",
        )
        .expect("settings");
        std::fs::write(root.path().join("config/agents.toml"),
            "default = \"shell\"\n[[agents]]\nname = \"shell\"\ncommand = \"sleep\"\nargs = [\"60\"]\n").unwrap();
        Self {
            server: tmux_server::TmuxServer::private("automation-lifecycle"),
            root,
        }
    }

    fn path(&self, sub: &str) -> PathBuf {
        self.root.path().join(sub)
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_thurbox-cli"));
        cmd.arg("--json").args(args);
        cmd.env("HOME", self.path("home"));
        cmd.env("USERPROFILE", self.path("home"));
        for variable in [
            "XDG_CONFIG_HOME",
            "XDG_DATA_HOME",
            "XDG_CACHE_HOME",
            "XDG_RUNTIME_DIR",
        ] {
            cmd.env(variable, self.path("home").join(variable));
        }
        cmd.env("THURBOX_CONFIG_DIR", self.path("config"));
        cmd.env("THURBOX_DATA_DIR", self.path("data"));
        cmd.env("TMUX_TMPDIR", self.path("tmux"));
        for var in [
            "THURBOX_SOCKET",
            "THURBOX_SOCKET_FOR",
            "THURBOX_SESSION",
            "THURBOX_SESSION_ID",
            "THURBOX_AUTOMATION_ID",
            "THURBOX_AUTOMATION_NAME",
        ] {
            cmd.env_remove(var);
        }
        self.server.scope(&mut cmd);
        cmd
    }

    fn run(&self, args: &[&str]) -> Output {
        self.command(args).output().expect("run thurbox-cli")
    }

    /// Run `args`, assert it succeeded, and return its JSON document.
    fn ok(&self, args: &[&str]) -> Value {
        let out = self.run(args);
        assert!(
            out.status.success(),
            "{args:?} failed:\nstdout: {}\nstderr: {}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        serde_json::from_slice(&out.stdout).expect("JSON")
    }

    /// Run `args`, assert it exited `code`, and return its JSON document.
    fn exits(&self, code: i32, args: &[&str]) -> Value {
        let out = self.run(args);
        assert_eq!(
            out.status.code(),
            Some(code),
            "{args:?}:\nstdout: {}\nstderr: {}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        serde_json::from_slice(&out.stdout).expect("JSON")
    }

    fn exec(&self, name: &str, command: &str, extra: &[&str]) -> String {
        let mut args = vec![
            "automation",
            "create",
            "--name",
            name,
            "--trigger",
            "hourly",
            "--command",
            command,
        ];
        args.extend_from_slice(extra);
        id_of(&self.ok(&args))
    }

    fn runs(&self, id: &str) -> Vec<Value> {
        self.ok(&["automation", "runs", id])
            .as_array()
            .expect("a list")
            .clone()
    }

    fn automation(&self, id: &str) -> Value {
        self.ok(&["automation", "show", id])
    }

    /// A session row with no window: the verbs under test act on rows.
    fn seed_session(&self, name: &str) -> String {
        let db = self.db();
        let row = SharedSession {
            id: SessionId::default(),
            name: name.into(),
            agent: "shell".into(),
            backend_id: String::new(),
            backend_type: "local-tmux".into(),
            agent_session_id: Some(SessionId::default().to_string()),
            cwd: None,
            additional_dirs: Vec::new(),
            worktrees: Vec::new(),
            shell_backend_id: None,
            parent_session_id: None,
            display_order: None,
            tombstone: false,
            tombstone_at: None,
            key: None,
        };
        db.upsert_session(&row).expect("persist");
        row.id.to_string()
    }

    fn db(&self) -> thurbox::storage::Database {
        thurbox::storage::Database::open(&self.path("data").join("thurbox.db"))
            .expect("open the instance database")
    }
}

fn strs(args: &[String]) -> Vec<&str> {
    args.iter().map(String::as_str).collect()
}

fn id_of(doc: &Value) -> String {
    doc["id"].as_i64().expect("an automation id").to_string()
}

#[test]
#[cfg(unix)]
fn owned_automations_keep_their_identity_and_enabled_state_across_undo() {
    if !Command::new("tmux")
        .arg("-V")
        .output()
        .is_ok_and(|o| o.status.success())
    {
        return;
    }
    let env = Env::new();
    let owner = env.seed_session("owner");
    let auto = env.exec("owned", "true", &["--owner-session", &owner]);
    let disabled = env.exec(
        "disabled",
        "true",
        &["--owner-session", &owner, "--disabled"],
    );
    let target = env.seed_session("other");
    let send = id_of(&env.ok(&[
        "automation",
        "create",
        "--name",
        "owned-send",
        "--trigger",
        "hourly",
        "--prompt",
        "continue",
        "--session",
        &target,
        "--owner-session",
        &owner,
    ]));
    let spawn = id_of(&env.ok(&[
        "automation",
        "create",
        "--name",
        "owned-spawn",
        "--trigger",
        "hourly",
        "--prompt",
        "continue",
        "--repo",
        env.path("work").to_str().unwrap(),
        "--owner-session",
        &owner,
    ]));
    assert_eq!(env.automation(&auto)["owner_session_id"], owner);
    env.ok(&["automation", "run", &auto, "--wait"]);
    let history = env.runs(&auto);
    env.ok(&["session", "delete", &owner, "--soft"]);
    env.ok(&[
        "automation",
        "edit",
        &auto,
        "--name",
        "renamed while suspended",
    ]);
    for id in [&auto, &send, &spawn] {
        assert_eq!(env.automation(id)["enabled"], false);
    }
    env.ok(&["session", "restore", &owner]);
    for id in [&auto, &send, &spawn] {
        assert_eq!(env.automation(id)["enabled"], true);
    }
    assert_eq!(env.automation(&disabled)["enabled"], false);
    assert_eq!(env.runs(&auto), history);
    env.ok(&["session", "delete", &owner, "--force"]);
    for id in [&auto, &send, &spawn, &disabled] {
        env.exits(1, &["automation", "show", id]);
    }
}

#[test]
#[cfg(unix)]
fn restoring_a_target_reenables_only_the_sends_its_delete_disabled() {
    if !Command::new("tmux")
        .arg("-V")
        .output()
        .is_ok_and(|o| o.status.success())
    {
        return;
    }
    let env = Env::new();
    let target = env.seed_session("target");
    let mut ids = Vec::new();
    for disabled in [false, true] {
        let mut args = vec![
            "automation",
            "create",
            "--name",
            "send",
            "--trigger",
            "hourly",
            "--prompt",
            "continue",
            "--session",
            &target,
        ];
        if disabled {
            args.push("--disabled");
        }
        ids.push(id_of(&env.ok(&args)));
    }
    env.ok(&["session", "delete", &target, "--soft"]);
    env.ok(&["session", "restore", &target]);
    assert_eq!(env.automation(&ids[0])["enabled"], true);
    assert_eq!(env.automation(&ids[1])["enabled"], false);
}

#[test]
#[cfg(unix)]
fn an_owner_and_a_target_must_both_be_restored_before_a_send_is_enabled() {
    if !Command::new("tmux")
        .arg("-V")
        .output()
        .is_ok_and(|o| o.status.success())
    {
        return;
    }
    for owner_first in [true, false] {
        let env = Env::new();
        let owner = env.seed_session("owner");
        let target = env.seed_session("target");
        let auto = id_of(&env.ok(&[
            "automation",
            "create",
            "--name",
            "send",
            "--trigger",
            "hourly",
            "--prompt",
            "continue",
            "--session",
            &target,
            "--owner-session",
            &owner,
        ]));
        env.ok(&["session", "delete", &owner, "--soft"]);
        env.ok(&["session", "delete", &target, "--soft"]);
        let (first, second) = if owner_first {
            (&owner, &target)
        } else {
            (&target, &owner)
        };
        env.ok(&["session", "restore", first]);
        assert_eq!(env.automation(&auto)["enabled"], false);
        env.ok(&["session", "restore", second]);
        assert_eq!(env.automation(&auto)["enabled"], true);
    }
}

#[test]
fn owner_is_explicit_validated_and_can_be_cleared_on_edit() {
    let env = Env::new();
    let owner = env.seed_session("owner");
    let auto = env.exec("unowned", "true", &[]);
    assert!(env.automation(&auto)["owner_session_id"].is_null());
    env.ok(&["automation", "edit", &auto, "--owner-session", &owner]);
    assert_eq!(env.automation(&auto)["owner_session_id"], owner);
    env.ok(&["automation", "edit", &auto, "--owner-session", "none"]);
    assert!(env.automation(&auto)["owner_session_id"].is_null());
    env.ok(&["session", "delete", &owner, "--soft"]);
    env.exits(1, &["automation", "edit", &auto, "--owner-session", &owner]);
    env.exits(
        1,
        &[
            "automation",
            "create",
            "--name",
            "invalid",
            "--trigger",
            "hourly",
            "--command",
            "true",
            "--owner-session",
            &owner,
        ],
    );
}

#[test]
fn purge_accepts_a_deleted_reference_and_refuses_an_active_one() {
    let env = Env::new();
    let session = env.seed_session("old");
    env.exits(1, &["session", "purge", &session]);
    env.ok(&["session", "delete", &session, "--soft"]);
    env.db()
        .conn_ref()
        .execute(
            "UPDATE sessions SET deleted_at = 1 WHERE id = ?1",
            [&session],
        )
        .unwrap();
    let done = env.ok(&["session", "purge", &session]);
    assert_eq!(done["purged"].as_array().unwrap().len(), 1);
    env.exits(1, &["session", "restore", &session, "--best-effort"]);
}

/// A soft delete disables the send automations aimed at the session, as a
/// force delete and a delete delegated to a shared host already did. Left
/// armed, the next fire found no session and recorded a skip every hour.
#[test]
fn a_soft_delete_disables_the_send_automations_of_its_session() {
    let env = Env::new();
    let session = env.seed_session("worker");
    let auto = id_of(&env.ok(&[
        "automation",
        "create",
        "--name",
        "nudge",
        "--trigger",
        "hourly",
        "--prompt",
        "continue",
        "--session",
        &session,
    ]));
    assert_eq!(env.automation(&auto)["enabled"], true);

    let report = env.ok(&["session", "delete", &session, "--soft"]);
    assert_eq!(report["disabled_automations"], 1, "{report}");

    let after = env.automation(&auto);
    assert_eq!(after["enabled"], false, "{after}");
    assert_eq!(after["next_run_at"], Value::Null, "{after}");
}

/// `automation run` fires an automation; it does not switch one on. Marking a
/// disabled one due used to enable it as a side effect, so it went on firing
/// on its schedule after the one run that was asked for.
#[test]
fn running_a_disabled_automation_leaves_it_disabled() {
    let env = Env::new();
    let auto = id_of(&env.ok(&[
        "automation",
        "create",
        "--name",
        "paused",
        "--trigger",
        "hourly",
        "--command",
        "true",
        "--disabled",
    ]));

    let out = env.run(&["automation", "run", &auto]);
    assert_eq!(
        out.status.code(),
        Some(1),
        "a disabled automation is refused, not fired: {}",
        String::from_utf8_lossy(&out.stdout)
    );

    let after = env.automation(&auto);
    assert_eq!(after["enabled"], false, "{after}");
    assert_eq!(after["next_run_at"], Value::Null, "{after}");
}

/// A run's row exists while the run is going, stamped with when it started.
/// It used to be inserted once the action returned, so `started_at` was the
/// finish time and a run that never returned left no row at all.
#[cfg(unix)]
#[test]
fn a_run_is_recorded_when_it_starts() {
    let env = Env::new();
    let seen = env.path("seen.json");
    // The command reads its own automation's history mid-run. A fresh
    // database numbers the first automation 1, asserted below.
    let command = format!(
        "'{}' --json automation runs 1 > '{}'",
        env!("CARGO_BIN_EXE_thurbox-cli"),
        seen.display()
    );
    let auto = id_of(&env.ok(&[
        "automation",
        "create",
        "--name",
        "probe",
        "--trigger",
        "hourly",
        "--command",
        &command,
    ]));
    assert_eq!(auto, "1");

    env.ok(&["automation", "run", &auto]);
    let before = thurbox::sync::current_time_millis();
    let tick = env.ok(&["automation", "tick"]);
    assert_eq!(tick["fired"][0]["status"], "success", "{tick}");

    let during: Value =
        serde_json::from_str(&std::fs::read_to_string(&seen).expect("the command ran"))
            .expect("JSON");
    let running = &during[0];
    assert_eq!(running["status"], "running", "{during}");

    let runs = env.ok(&["automation", "runs", &auto]);
    let runs = runs.as_array().expect("a list");
    assert_eq!(runs.len(), 1, "one run, one row: {runs:?}");
    assert_eq!(runs[0]["id"], running["id"], "the row is finished in place");
    assert_eq!(runs[0]["status"], "success");
    assert_eq!(runs[0]["started_at"], running["started_at"]);
    let started = runs[0]["started_at"].as_u64().expect("a time");
    assert!(started >= before, "{started} is before the tick began");
}

/// `automation edit` writes only what it changes, as the interface's edit
/// does: a rename leaves a pending run-now due rather than pushing it to the
/// next slot, which is what writing back the whole row as read used to do.
#[test]
fn editing_a_name_keeps_a_pending_run() {
    let env = Env::new();
    let auto = id_of(&env.ok(&[
        "automation",
        "create",
        "--name",
        "nightly",
        "--trigger",
        "daily",
        "--command",
        "true",
    ]));
    env.ok(&["automation", "run", &auto]);
    let due = env.automation(&auto)["next_run_at"]
        .as_u64()
        .expect("marked due");

    let renamed = env.ok(&["automation", "edit", &auto, "--name", "renamed"]);
    assert_eq!(renamed["name"], "renamed");
    assert_eq!(renamed["next_run_at"], due, "{renamed}");

    // A schedule change still moves it.
    let moved = env.ok(&["automation", "edit", &auto, "--trigger", "hourly"]);
    assert_ne!(moved["next_run_at"], due, "{moved}");
}

/// A hung Exec is killed at its own deadline and recorded as timed out, and it
/// holds up neither the other due automation nor the housekeeping behind the
/// due pass. It used to block the whole tick until the child exited.
#[cfg(unix)]
#[test]
fn a_hung_exec_is_killed_at_its_timeout_and_holds_nothing_up() {
    let env = Env::new();
    let marker = env.path("second-fired");
    let hung = env.exec("hung", "sleep 600", &["--timeout", "2"]);
    let second = env.exec(
        "second",
        &format!("touch '{}'", marker.display()),
        &["--timeout", "30"],
    );
    env.ok(&["automation", "run", &hung]);
    env.ok(&["automation", "run", &second]);

    let started = Instant::now();
    let tick = env.ok(&["automation", "tick"]);
    assert!(
        started.elapsed() < Duration::from_secs(30),
        "the tick waited on the hung child: {:?}",
        started.elapsed()
    );
    assert!(marker.exists(), "the second automation fired: {tick}");
    assert!(tick.get("reaped").is_some(), "housekeeping ran: {tick}");

    let runs = env.runs(&hung);
    assert_eq!(runs.len(), 1, "{runs:?}");
    assert_eq!(runs[0]["status"], "timed_out", "{runs:?}");
    assert!(
        runs[0]["detail"]
            .as_str()
            .unwrap_or_default()
            .contains("timed out after 2s"),
        "{runs:?}"
    );
    assert!(runs[0]["finished_at"].as_u64().is_some(), "{runs:?}");
    assert_eq!(env.runs(&second)[0]["status"], "success");
}

/// A run says how it ended in fields, not one free-text string: the exit code,
/// when it finished, how long it took, and each stream's own tail.
#[cfg(unix)]
#[test]
fn an_exec_run_records_its_exit_code_duration_and_both_tails() {
    let env = Env::new();
    let id = env.exec("loud", "echo to-stdout; echo to-stderr >&2; exit 3", &[]);
    env.ok(&["automation", "run", &id]);
    let tick = env.ok(&["automation", "tick"]);
    assert_eq!(tick["fired"][0]["status"], "error", "{tick}");

    let run = &env.runs(&id)[0];
    assert_eq!(run["status"], "error", "{run}");
    assert_eq!(run["exit_code"], 3, "{run}");
    assert_eq!(run["stdout_tail"], "to-stdout", "{run}");
    assert_eq!(run["stderr_tail"], "to-stderr", "{run}");
    let started = run["started_at"].as_u64().expect("started_at");
    let finished = run["finished_at"].as_u64().expect("finished_at");
    assert!(finished >= started, "{run}");
    assert_eq!(
        run["duration_ms"].as_u64(),
        Some(finished - started),
        "{run}"
    );
}

/// An Exec runs where it was told, with what it was told, and a `thurbox-cli`
/// inside it reaches the instance that fired it — whoever fired it. The
/// settings round-trip through `automation show`.
#[cfg(unix)]
#[test]
fn an_exec_runs_in_its_cwd_with_its_env_and_this_instance() {
    let env = Env::new();
    let seen = env.path("seen");
    let work = env.path("work");
    let command = format!(
        "printf '%s\\n%s\\n%s\\n%s\\n%s\\n' \"$PWD\" \"$GREETING\" \"$THURBOX_AUTOMATION_ID\" \
         \"$THURBOX_AUTOMATION_NAME\" \"$THURBOX_DATA_DIR\" > '{}'",
        seen.display()
    );
    let id = env.exec(
        "probe",
        &command,
        &[
            "--cwd",
            work.to_str().expect("utf-8"),
            "--env",
            "GREETING=hello there",
            "--timeout",
            "45",
        ],
    );
    let shown = env.ok(&["automation", "show", &id]);
    assert_eq!(shown["cwd"], work.to_str().expect("utf-8"), "{shown}");
    assert_eq!(shown["env"]["GREETING"], "hello there", "{shown}");
    assert_eq!(shown["timeout_secs"], 45, "{shown}");

    env.ok(&["automation", "run", &id]);
    env.ok(&["automation", "tick"]);
    let lines: Vec<String> = std::fs::read_to_string(&seen)
        .expect("the command ran")
        .lines()
        .map(str::to_string)
        .collect();
    assert_eq!(
        PathBuf::from(&lines[0]).canonicalize().expect("cwd"),
        work.canonicalize().expect("work")
    );
    assert_eq!(lines[1], "hello there");
    assert_eq!(lines[2], id);
    assert_eq!(lines[3], "probe");
    assert_eq!(
        PathBuf::from(&lines[4]).canonicalize().expect("data dir"),
        env.path("data").canonicalize().expect("data")
    );
}

/// A tick that dies mid-run leaves its row `running`; the next tick past the
/// run's deadline — its timeout plus a few seconds' grace for recording —
/// marks it timed out, so a crash leaves a record too.
#[cfg(unix)]
#[test]
fn a_run_orphaned_by_a_dead_tick_is_timed_out_by_the_next() {
    let env = Env::new();
    // Short: the dead tick cannot kill the process group it started, so the
    // sleep outlives the test by however long it is.
    let id = env.exec("orphan", "sleep 20", &["--timeout", "1"]);
    env.ok(&["automation", "run", &id]);
    let mut tick = env.command(&["automation", "tick"]).spawn().expect("tick");
    let deadline = Instant::now() + Duration::from_secs(20);
    while env.runs(&id).is_empty() {
        assert!(Instant::now() < deadline, "the run row never opened");
        std::thread::sleep(Duration::from_millis(50));
    }
    tick.kill().expect("kill the tick");
    let _ = tick.wait();
    assert_eq!(env.runs(&id)[0]["status"], "running");

    env.ok(&["automation", "tick"]);
    assert_eq!(
        env.runs(&id)[0]["status"],
        "running",
        "inside its deadline, the row is left alone"
    );
    std::thread::sleep(Duration::from_millis(6500));
    env.ok(&["automation", "tick"]);
    let run = &env.runs(&id)[0];
    assert_eq!(run["status"], "timed_out", "{run}");
    assert!(run["finished_at"].as_u64().is_some(), "{run}");
}

/// `create --key` is idempotent: `update` rewrites the one row in place and
/// keeps its id and history, `keep` leaves it, `fail` (the default) refuses
/// with the conflict code. `list` filters by key and by name.
#[test]
fn a_keyed_create_is_idempotent() {
    let env = Env::new();
    let create = |command: &str, policy: &str| {
        vec![
            "automation".to_string(),
            "create".into(),
            "--name".into(),
            "sync".into(),
            "--trigger".into(),
            "hourly".into(),
            "--command".into(),
            command.into(),
            "--key".into(),
            "dotfiles-sync".into(),
            "--on-existing".into(),
            policy.into(),
        ]
    };

    let first = env.ok(&strs(&create("true", "update")));
    assert_eq!(first["outcome"], "created", "{first}");
    assert_eq!(first["key"], "dotfiles-sync", "{first}");
    let again = env.ok(&strs(&create("true", "update")));
    assert_eq!(again["outcome"], "updated", "{again}");
    assert_eq!(again["id"], first["id"]);
    let changed = env.ok(&strs(&create("make sync", "update")));
    assert_eq!(changed["id"], first["id"]);
    assert_eq!(changed["action"]["command"], "make sync", "{changed}");
    let kept = env.ok(&strs(&create("other", "keep")));
    assert_eq!(kept["outcome"], "kept", "{kept}");
    assert_eq!(kept["action"]["command"], "make sync", "{kept}");
    env.exits(5, &strs(&create("other", "fail")));
    assert_eq!(
        env.ok(&["automation", "list"]).as_array().map(Vec::len),
        Some(1)
    );

    env.exec("unrelated", "true", &[]);
    let by_key = env.ok(&["automation", "list", "--key", "dotfiles-sync"]);
    assert_eq!(by_key.as_array().map(Vec::len), Some(1), "{by_key}");
    let by_name = env.ok(&["automation", "list", "--name", "unrelated"]);
    assert_eq!(by_name.as_array().map(Vec::len), Some(1), "{by_name}");
    assert_eq!(by_name[0]["name"], "unrelated");
}

/// Every verb takes a reference: an id, then a key, then a unique name. A name
/// two automations share is refused with the ambiguity code and both ids.
#[test]
fn verbs_resolve_an_id_a_key_or_a_unique_name() {
    let env = Env::new();
    let keyed = env.exec("nightly", "true", &["--key", "ops-nightly"]);
    assert_eq!(
        env.ok(&["automation", "show", "ops-nightly"])["id"].to_string(),
        keyed
    );
    assert_eq!(
        env.ok(&["automation", "show", "nightly"])["id"].to_string(),
        keyed
    );

    let a = env.exec("twin", "true", &[]);
    let b = env.exec("twin", "true", &[]);
    let refused = env.exits(3, &["automation", "show", "twin"]);
    let message = refused["error"].as_str().unwrap_or_default().to_string();
    assert!(message.contains(&a) && message.contains(&b), "{refused}");
    env.exits(1, &["automation", "show", "nobody"]);
}

/// `edit --command` changes an Exec's command in place, keeping its id and run
/// history; on a send or spawn it is refused.
#[test]
fn edit_replaces_an_exec_command_in_place() {
    let env = Env::new();
    let id = env.exec("job", "true", &[]);
    env.ok(&["automation", "run", &id]);
    env.ok(&["automation", "tick"]);
    let edited = env.ok(&["automation", "edit", &id, "--command", "make job"]);
    assert_eq!(edited["action"]["command"], "make job", "{edited}");
    assert_eq!(env.runs(&id).len(), 1, "the history stays");

    let spawn = id_of(&env.ok(&[
        "automation",
        "create",
        "--name",
        "spawner",
        "--trigger",
        "hourly",
        "--prompt",
        "go",
        "--repo",
        env.path("work").to_str().expect("utf-8"),
    ]));
    let refused = env.exits(1, &["automation", "edit", &spawn, "--command", "x"]);
    assert!(
        refused["error"]
            .as_str()
            .unwrap_or_default()
            .contains("only an exec automation runs a command"),
        "{refused}"
    );
}

/// The CLI writes through the same rules the interface does: a timezone the
/// schedule would silently ignore is refused rather than stored.
#[test]
fn a_create_refuses_a_timezone_the_schedule_would_ignore() {
    let env = Env::new();
    let refused = env.exits(
        1,
        &[
            "automation",
            "create",
            "--name",
            "x",
            "--trigger",
            "daily",
            "--timezone",
            "Mars/Olympus",
            "--command",
            "true",
        ],
    );
    assert!(
        refused["error"]
            .as_str()
            .unwrap_or_default()
            .contains("unknown timezone"),
        "{refused}"
    );
    assert_eq!(
        env.ok(&["automation", "list"]).as_array().map(Vec::len),
        Some(0)
    );
}

/// `run --wait` fires one automation now and answers with its run — the same
/// row `automation runs` shows — exiting with what the run did. A manual run
/// of an automation that was not due leaves its schedule where it was.
#[cfg(unix)]
#[test]
fn run_wait_fires_once_and_answers_with_the_run() {
    let env = Env::new();
    let id = env.exec("hello", "echo hi", &[]);
    let next = env.ok(&["automation", "show", &id])["next_run_at"].clone();

    let run = env.ok(&["automation", "run", &id, "--wait"]);
    assert_eq!(run["status"], "success", "{run}");
    assert_eq!(run["exit_code"], 0, "{run}");
    assert_eq!(run["stdout_tail"], "hi", "{run}");
    let runs = env.runs(&id);
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0]["id"], run["id"]);
    assert_eq!(
        env.ok(&["automation", "show", &id])["next_run_at"],
        next,
        "a manual run moves no schedule"
    );

    let failing = env.exec("failing", "exit 7", &[]);
    let failed = env.exits(1, &["automation", "run", &failing, "--wait"]);
    assert_eq!(failed["exit_code"], 7, "{failed}");

    let slow = env.exec("slow", "sleep 600", &[]);
    let timed_out = env.exits(4, &["automation", "run", &slow, "--wait", "--timeout", "1"]);
    assert_eq!(timed_out["status"], "timed_out", "{timed_out}");
}

/// A due automation fired by `run --wait` is claimed like a scheduled fire: its
/// next run moves on, so the heartbeat does not fire it a second time.
#[cfg(unix)]
#[test]
fn run_wait_on_a_due_automation_claims_it() {
    let env = Env::new();
    let id = env.exec("due", "true", &[]);
    env.ok(&["automation", "run", &id]);
    let due = env.ok(&["automation", "show", &id])["next_run_at"]
        .as_u64()
        .expect("marked due");
    env.ok(&["automation", "run", &id, "--wait"]);
    let after = env.ok(&["automation", "show", &id]);
    assert!(
        after["next_run_at"].as_u64().expect("scheduled") > due,
        "{after}"
    );
    let tick = env.ok(&["automation", "tick"]);
    assert_eq!(tick["fired"], serde_json::json!([]), "{tick}");
    assert_eq!(env.runs(&id).len(), 1);
}

/// While one run is in flight a second `run --wait` is refused with the
/// conflict code, and a disabled automation runs only with `--force`.
#[cfg(unix)]
#[test]
fn run_wait_refuses_a_run_in_flight_and_a_disabled_automation() {
    let env = Env::new();
    let id = env.exec("busy", "sleep 3", &[]);
    let mut first = env
        .command(&["automation", "run", &id, "--wait"])
        .spawn()
        .expect("first run");
    let deadline = Instant::now() + Duration::from_secs(20);
    while env.runs(&id).is_empty() {
        assert!(Instant::now() < deadline, "the first run never opened");
        std::thread::sleep(Duration::from_millis(50));
    }
    env.exits(5, &["automation", "run", &id, "--wait"]);
    assert!(first.wait().expect("first run").success());

    let off = env.exec("off", "true", &["--disabled"]);
    env.exits(1, &["automation", "run", &off, "--wait"]);
    let forced = env.ok(&["automation", "run", &off, "--wait", "--force"]);
    assert_eq!(forced["status"], "success", "{forced}");
    assert_eq!(env.ok(&["automation", "show", &off])["enabled"], false);
}

/// A tick that fails before it fires anything leaves a timestamped entry in
/// the tick log, and `runtime status` reports the last tick's outcome — until
/// a tick that works says so instead.
#[test]
fn a_failed_tick_is_logged_and_reported_by_runtime_status() {
    let env = Env::new();
    {
        let db = env.db();
        // A row the due scan cannot read: the tick fails before any claim.
        db.conn_ref()
            .execute(
                "INSERT INTO automations (name, enabled, schedule_kind, schedule_spec, \
                 action_kind, prompt, created_at, updated_at, next_run_at, action_command) \
                 VALUES ('broken', 1, 'bogus', '?', 'exec', '', 0, 0, 1, 'true')",
                [],
            )
            .expect("insert");
    }
    let out = env.run(&["automation", "tick"]);
    assert!(!out.status.success(), "the tick fails");

    let log =
        std::fs::read_to_string(env.path("data").join("automation-tick.log")).expect("a tick log");
    let line = log.lines().last().expect("an entry");
    assert!(line.contains("invalid schedule"), "{line}");
    assert!(line.starts_with("20"), "timestamped: {line}");

    let status = env.ok(&["runtime", "status"]);
    let last = &status["last_tick"];
    assert_eq!(last["ok"], false, "{status}");
    assert!(
        last["error"]
            .as_str()
            .unwrap_or_default()
            .contains("invalid schedule"),
        "{status}"
    );
    assert!(last["at"].as_u64().is_some(), "{status}");

    env.db()
        .conn_ref()
        .execute("DELETE FROM automations WHERE name = 'broken'", [])
        .expect("delete");
    env.ok(&["automation", "tick"]);
    let status = env.ok(&["runtime", "status"]);
    assert_eq!(status["last_tick"]["ok"], true, "{status}");
}

/// An exec fired from inside a pane does not inherit the pane's session
/// identity: the heartbeat's run of it has none, and the two must match.
#[cfg(unix)]
#[test]
fn an_exec_fired_from_a_pane_does_not_act_as_that_session() {
    let env = Env::new();
    let id = env.exec("who", "printf '[%s]' \"$THURBOX_SESSION\"", &[]);
    let out = env
        .command(&["automation", "run", &id, "--wait"])
        .env("THURBOX_SESSION", "6f1c1d8e-2a2b-4c1e-9d1f-0a1b2c3d4e5f")
        .output()
        .expect("run");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
    let run: Value = serde_json::from_slice(&out.stdout).expect("JSON");
    assert_eq!(run["stdout_tail"], "[]", "{run}");
}

/// A zero timeout is refused at the flag, as create, edit and the manifest
/// refuse it, rather than quietly read as one second.
#[test]
fn a_zero_timeout_is_refused() {
    let env = Env::new();
    let id = env.exec("job", "true", &[]);
    env.exits(2, &["automation", "run", &id, "--wait", "--timeout", "0"]);
    env.exits(2, &["automation", "edit", &id, "--timeout", "0"]);
}

/// A relative `--cwd` is stored against the directory it was given in, so the
/// heartbeat — started anywhere — runs the command where the creator meant.
#[test]
fn a_relative_cwd_is_stored_as_an_absolute_path() {
    let env = Env::new();
    let out = env
        .command(&[
            "automation",
            "create",
            "--name",
            "rel",
            "--trigger",
            "hourly",
            "--command",
            "true",
            "--cwd",
            "work",
        ])
        .current_dir(env.root.path())
        .output()
        .expect("create");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
    let created: Value = serde_json::from_slice(&out.stdout).expect("JSON");
    let stored = PathBuf::from(created["cwd"].as_str().expect("a cwd"));
    assert!(stored.is_absolute(), "{stored:?}");
    assert_eq!(
        stored.canonicalize().expect("exists"),
        env.path("work").canonicalize().expect("work")
    );
}

/// Extension jobs keep their install-owned cwd across activation, manual runs
/// and the tick used by the heartbeat, regardless of each caller's directory.
#[test]
fn an_extension_exec_keeps_its_directory_across_callers() {
    for relative_home in [false, true] {
        let env = Env::new();
        let source = env.path("extension-source");
        std::fs::create_dir_all(source.join("work")).unwrap();
        std::fs::write(source.join("work/marker"), "extension-owned").unwrap();
        let command = if cfg!(windows) {
            "type marker"
        } else {
            "cat marker"
        };
        std::fs::write(
            source.join("extension.toml"),
            format!(
                r#"name = "cwd-probe"
[[files]]
path = "work/marker"
[[automations]]
name = "cwd-probe-job"
trigger = "hourly"
command = "{command}"
cwd = "work"
"#
            ),
        )
        .unwrap();
        let callers = [env.path("caller-a"), env.path("caller-b")];
        for caller in &callers {
            std::fs::create_dir_all(caller.join("work")).unwrap();
            std::fs::write(caller.join("work/marker"), "caller-owned").unwrap();
        }
        let from = |args: &[&str], cwd: &std::path::Path| -> Value {
            let out = env.command(args).current_dir(cwd).output().unwrap();
            assert!(
                out.status.success(),
                "{args:?}: {} {}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            );
            serde_json::from_slice(&out.stdout).expect("JSON")
        };
        let home = if relative_home {
            callers[0].join("extension-home")
        } else {
            env.path("extension-home")
        };
        let home_arg = if relative_home {
            "extension-home"
        } else {
            home.to_str().unwrap()
        };
        from(
            &[
                "extension",
                "install",
                source.to_str().unwrap(),
                "--home",
                home_arg,
            ],
            &callers[0],
        );
        from(&["extension", "deactivate", "cwd-probe"], &callers[0]);
        from(&["extension", "activate", "cwd-probe"], &callers[1]);
        let id = id_of(&env.automation("cwd-probe-job"));
        for caller in &callers {
            let run = from(&["automation", "run", &id, "--wait"], caller);
            assert_eq!(run["status"], "success", "{run}");
            assert_eq!(
                run["stdout_tail"], "extension-owned",
                "caller {caller:?}: {run}"
            );
        }
        from(&["automation", "run", &id], &callers[0]);
        let tick = from(&["automation", "tick"], &callers[1]);
        assert_eq!(tick["fired"][0]["status"], "success", "{tick}");
        let runs = env.runs(&id);
        assert_eq!(runs.len(), 3, "{runs:?}");
        assert!(
            runs.iter()
                .all(|run| run["stdout_tail"] == "extension-owned"),
            "{runs:?}"
        );
        let stored = env.automation(&id);
        let cwd = PathBuf::from(stored["cwd"].as_str().unwrap());
        assert!(cwd.is_absolute(), "{stored}");
        assert_eq!(
            cwd.canonicalize().unwrap(),
            home.join("work").canonicalize().unwrap()
        );
        // A prior installation may already have persisted the unresolved path.
        env.db()
            .conn_ref()
            .execute("UPDATE automations SET cwd = 'work' WHERE id = ?1", [&id])
            .unwrap();
        let activated = from(&["extension", "activate", "cwd-probe"], &callers[1]);
        assert!(activated["automations_created"]
            .as_array()
            .unwrap()
            .is_empty());
        let run = from(&["automation", "run", &id, "--wait"], &callers[0]);
        assert_eq!(run["stdout_tail"], "extension-owned", "retained row: {run}");
        assert_eq!(env.runs(&id).len(), 4, "repair preserves run history");
    }
}

/// Hand-written discovery manifests cannot leave execution dependent on the
/// activation or heartbeat caller when no install root resolved the directory.
#[test]
fn activation_refuses_an_unresolved_relative_exec_directory() {
    let env = Env::new();
    let manifests = env.path("config/extensions");
    std::fs::create_dir_all(&manifests).unwrap();
    std::fs::write(
        manifests.join("unresolved.toml"),
        r#"name = "unresolved"
[[automations]]
name = "unresolved-job"
trigger = "hourly"
command = "echo unsafe"
cwd = "work"
"#,
    )
    .unwrap();
    let out = env.run(&["extension", "activate", "unresolved"]);
    assert!(!out.status.success(), "relative cwd was accepted: {out:?}");
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("absolute"),
        "{out:?}"
    );
    assert!(env.db().list_automations().unwrap().is_empty());
}

/// A one-shot retried after its time still supports idempotent keep/update.
#[test]
fn a_keyed_one_shot_retry_preserves_its_pending_fire() {
    let env = Env::new();
    let at = thurbox::sync::current_time_millis() + 3_600_000;
    let trigger = format!("at:{at}");
    let args = [
        "automation",
        "create",
        "--name",
        "once",
        "--trigger",
        &trigger,
        "--command",
        "true",
        "--key",
        "once",
    ];
    let id = id_of(&env.ok(&args));
    // Move the existing slot into the past without waiting for wall time.
    let past = thurbox::sync::current_time_millis() - 10_000;
    env.db()
        .conn_ref()
        .execute(
            "UPDATE automations SET schedule_spec = ?1, next_run_at = ?1 WHERE id = ?2",
            rusqlite::params![past.to_string(), id],
        )
        .unwrap();
    let past_trigger = format!("at:{past}");
    for policy in ["keep", "update"] {
        let result = env.ok(&[
            "automation",
            "create",
            "--name",
            "once",
            "--trigger",
            &past_trigger,
            "--command",
            "true",
            "--key",
            "once",
            "--on-existing",
            policy,
        ]);
        assert_eq!(id_of(&result), id);
        assert_eq!(env.automation(&id)["next_run_at"], past);
        assert!(env.runs(&id).is_empty());
    }
}

/// Claims for serial sends are opened only after the preceding fire finishes.
#[test]
fn a_tick_claims_each_inline_fire_when_it_starts() {
    let env = Env::new();
    let session = env.seed_session("absent");
    let mut ids = Vec::new();
    for name in ["first", "second"] {
        let id = id_of(&env.ok(&[
            "automation",
            "create",
            "--name",
            name,
            "--trigger",
            "hourly",
            "--session",
            &session,
            "--prompt",
            "hello",
        ]));
        env.ok(&["automation", "run", &id]);
        ids.push(id);
    }
    // A deterministic observer of claim order; no slow hook or sleep needed.
    env.db()
        .conn_ref()
        .execute_batch(
            "CREATE TRIGGER serial_claims BEFORE INSERT ON automation_runs
         WHEN EXISTS (SELECT 1 FROM automation_runs WHERE status = 'running')
         BEGIN SELECT RAISE(ABORT, 'previous inline fire still running'); END;",
        )
        .unwrap();
    let tick = env.ok(&["automation", "tick"]);
    assert_eq!(tick["fired"].as_array().unwrap().len(), 2, "{tick}");
    for id in ids {
        let runs = env.runs(&id);
        assert_eq!(runs.len(), 1, "{runs:?}");
        assert_ne!(runs[0]["status"], "running");
    }
}

/// The heartbeat can discard stdout without losing an individual claim error.
#[test]
fn a_refused_claim_is_logged_and_reported_by_runtime_status() {
    let env = Env::new();
    let id = env.exec("refused", "echo ok", &[]);
    env.ok(&["automation", "run", &id]);
    env.db()
        .conn_ref()
        .execute_batch(
            "CREATE TRIGGER refuse_run BEFORE INSERT ON automation_runs
         BEGIN SELECT RAISE(ABORT, 'claim refused'); END;",
        )
        .unwrap();
    let tick = env.ok(&["automation", "tick"]);
    assert!(
        tick["skipped"]
            .as_array()
            .unwrap()
            .iter()
            .any(|s| s["reason"] == "error"),
        "{tick}"
    );
    let status = env.ok(&["runtime", "status"]);
    let errors = status["last_tick"]["errors"]
        .as_array()
        .expect("persisted errors");
    assert!(
        errors
            .iter()
            .any(|e| e.as_str().unwrap().contains("claim refused")),
        "{status}"
    );
    let log = std::fs::read_to_string(env.path("data/automation-tick.log")).expect("a tick log");
    assert!(log.contains("claim refused"), "{log}");
    assert!(env.runs(&id).is_empty());
}

#[test]
#[cfg(unix)]
fn separate_heartbeat_processes_confirm_a_missing_pane() {
    if !Command::new("tmux")
        .arg("-V")
        .output()
        .is_ok_and(|o| o.status.success())
    {
        return;
    }
    let env = Env::new();
    let session = env.seed_session("missing");
    env.db()
        .conn_ref()
        .execute(
            "UPDATE sessions SET backend_id = '%900000' WHERE id = ?1",
            [&session],
        )
        .unwrap();
    assert_eq!(env.ok(&["automation", "tick"])["lost"], 0);
    assert!(env
        .db()
        .session_lost_at(session.parse().unwrap())
        .unwrap()
        .is_none());
    assert_eq!(env.ok(&["automation", "tick"])["lost"], 1);
    assert!(env
        .db()
        .session_lost_at(session.parse().unwrap())
        .unwrap()
        .is_some());
}
