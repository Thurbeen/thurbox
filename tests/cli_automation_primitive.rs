//! The automation primitive an orchestrator drives headlessly (issues #1468,
//! #1507, #1508, #1515, #1517), through the real binary: a bounded Exec with
//! its own cwd, env and deadline; a run row that says how it ended; a keyed,
//! idempotent create and references by id, key or name; a synchronous
//! `run --wait`; and a tick that leaves a trace when it fails.

use std::path::PathBuf;
use std::process::{Command, Output};
use std::time::{Duration, Instant};

use serde_json::Value;

/// A throwaway thurbox instance with the heartbeat off: nothing fires here
/// except what a test runs itself.
struct Env {
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
        Self { root }
    }

    fn path(&self, sub: &str) -> PathBuf {
        self.root.path().join(sub)
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_thurbox-cli"));
        cmd.arg("--json").args(args);
        cmd.env("HOME", self.path("home"));
        cmd.env("USERPROFILE", self.path("home"));
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
