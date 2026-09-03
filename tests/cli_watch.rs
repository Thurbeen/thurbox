//! What `thurbox-cli watch` streams, and what it can no longer lose.
//!
//! The command used to sample every session's state on a 250 ms timer and diff
//! the samples, which is exactly wrong for the thing a driver watches for: two
//! transitions inside one sample cancelled out. `working → blocked → working`
//! around an auto-answered permission arrived as nothing at all, and the driver
//! never learned the permission had been asked. These tests pin the replacement
//! — an append-only log written by each writer in its own transaction — by
//! driving the real binary against a real database file, because the property
//! under test is the cross-process one: the writer is a different connection.
//!
//! No tmux and no agent anywhere here. Every event these tests care about is a
//! row in the database, written by the storage layer a spawn would have gone
//! through.

use std::io::{BufRead, BufReader, Read};
use std::path::PathBuf;
use std::process::{Child, ChildStdout, Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use serde_json::Value;
use thurbox::session::SessionId;
use thurbox::storage::Database;
use thurbox::sync::SharedSession;

/// How long a test waits for a line that should already be on its way.
const WAIT: Duration = Duration::from_secs(15);

/// The agent registry these tests read coverage from: `claude` reports every
/// state and its `blocked` is a text match, which is what the gating fields on
/// each event are asserted against.
const AGENTS_TOML: &str = r#"
config_version = 1
default = "claude"

[[agents]]
name = "claude"
command = "claude"
"#;

/// A throwaway thurbox instance: its own config, data, home and multiplexer
/// socket, so nothing here reads or writes the operator's.
struct Env {
    root: tempfile::TempDir,
}

impl Env {
    fn new() -> Self {
        let root = tempfile::TempDir::new().expect("tempdir");
        for sub in ["home", "config", "data"] {
            std::fs::create_dir_all(root.path().join(sub)).expect("mkdir");
        }
        let agents = root.path().join("config").join("agents.toml");
        std::fs::write(agents, AGENTS_TOML).expect("write agents.toml");
        Self { root }
    }

    fn path(&self, sub: &str) -> PathBuf {
        self.root.path().join(sub)
    }

    fn db(&self) -> Database {
        Database::open(&self.path("data").join("thurbox.db")).expect("open the instance database")
    }

    /// Start `watch` with the given flags, its stdout on a pipe.
    fn watch(&self, args: &[&str]) -> Watch {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_thurbox-cli"));
        cmd.arg("watch").args(args);
        cmd.env("HOME", self.path("home"));
        cmd.env("USERPROFILE", self.path("home"));
        cmd.env("XDG_DATA_HOME", self.path("home").join("xdg-data"));
        cmd.env("XDG_CONFIG_HOME", self.path("home").join("xdg-config"));
        cmd.env("THURBOX_CONFIG_DIR", self.path("config"));
        cmd.env("THURBOX_DATA_DIR", self.path("data"));
        // Named outright: a relocated data dir derives a socket of its own, and
        // nothing here may reach the operator's server even by accident.
        cmd.env("THURBOX_SOCKET", "thurbox-watch-test");
        cmd.env_remove("THURBOX_SOCKET_FOR");
        cmd.env_remove("THURBOX_SESSION");
        cmd.env_remove("THURBOX_SESSION_ID");
        let mut child = cmd
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn watch");
        let stdout = child.stdout.take().expect("piped stdout");
        Watch {
            child,
            lines: reader(stdout),
        }
    }
}

/// A running `watch`, killed when the test drops it so a failure cannot leave a
/// process behind.
struct Watch {
    child: Child,
    lines: mpsc::Receiver<String>,
}

impl Watch {
    /// The next line, or a failure naming what was being waited for.
    fn line(&self, what: &str) -> String {
        self.lines
            .recv_timeout(WAIT)
            .unwrap_or_else(|_| panic!("watch produced no line while waiting for {what}"))
    }

    fn event(&self, what: &str) -> Value {
        serde_json::from_str(&self.line(what)).expect("one JSON object per line")
    }

    /// Assert nothing more arrives within `grace` — used where the point is an
    /// event that must *not* be emitted.
    fn silent_for(&self, grace: Duration) {
        if let Ok(line) = self.lines.recv_timeout(grace) {
            panic!("watch emitted an event it should not have: {line}");
        }
    }
}

impl Drop for Watch {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Pump a child's stdout into a channel, so a test can wait on a line with a
/// timeout instead of blocking forever on a read.
fn reader(stdout: ChildStdout) -> mpsc::Receiver<String> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            let Ok(line) = line else { return };
            if tx.send(line).is_err() {
                return;
            }
        }
    });
    rx
}

fn seed(db: &Database, name: &str) -> SessionId {
    let row = SharedSession {
        id: SessionId::default(),
        name: name.into(),
        agent: "claude".into(),
        backend_id: "%1".into(),
        backend_type: "local-tmux".into(),
        agent_session_id: None,
        cwd: None,
        additional_dirs: Vec::new(),
        worktrees: Vec::new(),
        shell_backend_id: None,
        parent_session_id: None,
        display_order: None,
        tombstone: false,
        tombstone_at: None,
    };
    db.upsert_session(&row).expect("seed a session row");
    row.id
}

/// Let the watcher take its baseline before the test changes anything, so a
/// write below is unambiguously a change rather than part of the initial state.
fn settle() {
    std::thread::sleep(Duration::from_millis(700));
}

fn event_of(line: &Value) -> (&str, &str) {
    (
        line["event"].as_str().unwrap_or_default(),
        line["reason"].as_str().unwrap_or_default(),
    )
}

/// Two writes inside the same sample window are two events, in order.
///
/// This is the bug the event log exists for: `working → blocked → working` is
/// how an auto-answered permission looks, and the old whole-state diff reported
/// it as nothing at all because both writes landed between two samples.
#[test]
fn two_writes_in_one_sample_are_two_events_in_order() {
    let env = Env::new();
    let db = env.db();
    let id = seed(&db, "worker");
    db.set_hook_state(id, "working").expect("first state");

    let watch = env.watch(&["--json", "--for-secs", "30"]);
    settle();

    // Both inside a single 250 ms gate window, and far tighter than one.
    db.set_hook_state(id, "blocked").expect("blocked");
    std::thread::sleep(Duration::from_millis(10));
    db.set_hook_state(id, "working").expect("working");

    let blocked = watch.event("the blocked transition");
    let working = watch.event("the working transition");

    assert_eq!(event_of(&blocked), ("changed", "state"));
    assert_eq!(blocked["from_state"], Value::String("working".into()));
    assert_eq!(blocked["to_state"], Value::String("blocked".into()));
    assert_eq!(event_of(&working), ("changed", "state"));
    assert_eq!(working["from_state"], Value::String("blocked".into()));
    assert_eq!(working["to_state"], Value::String("working".into()));
    assert!(
        blocked["seq"].as_i64() < working["seq"].as_i64(),
        "seq must order the two writes"
    );
}

/// Every event carries what the reported state is *worth*, so reacting to a
/// `blocked` needs no follow-up `session get`.
#[test]
fn each_event_carries_the_gating_fields() {
    let env = Env::new();
    let db = env.db();
    let id = seed(&db, "worker");

    let watch = env.watch(&["--json", "--for-secs", "30"]);
    settle();
    db.set_hook_state(id, "blocked").expect("blocked");

    let event = watch.event("the blocked transition");
    assert_eq!(event["state"], Value::String("blocked".into()));
    assert_eq!(event["state_source"], Value::String("hook".into()));
    assert_eq!(event["hook_coverage"], Value::String("full".into()));
    // claude's `blocked` is a text match on a notification body, which is
    // exactly what a driver about to act on one needs told.
    assert_eq!(event["hook_blocked_is_heuristic"], Value::Bool(true));
    // Not checked rather than checked-and-consistent: the pane probe is
    // `--verify`, off by default here as it is on `session list`.
    assert_eq!(event["hook_state_contradicted"], Value::Null);
}

/// A soft delete and a force delete are not the same news: one is restorable.
#[test]
fn gone_says_which_delete_it_was() {
    let env = Env::new();
    let db = env.db();
    let soft = seed(&db, "soft");
    let hard = seed(&db, "hard");

    let watch = env.watch(&["--json", "--for-secs", "30"]);
    settle();
    db.soft_delete_session(soft).expect("soft delete");
    db.force_delete_session(hard).expect("force delete");

    let first = watch.event("the soft delete");
    let second = watch.event("the force delete");
    assert_eq!(event_of(&first), ("gone", "soft_deleted"));
    assert_eq!(first["name"], Value::String("soft".into()));
    assert_eq!(event_of(&second), ("gone", "force_deleted"));
    assert_eq!(second["name"], Value::String("hard".into()));
}

/// `created` says where the session came from — thurbox launched it, it was
/// adopted while already running, or it came back from the deleted list.
#[test]
fn created_says_where_the_session_came_from() {
    let env = Env::new();
    let db = env.db();

    let watch = env.watch(&["--json", "--for-secs", "30"]);
    settle();
    let spawned = seed(&db, "spawned");
    db.soft_delete_session(spawned).expect("delete");
    db.restore_session(spawned).expect("restore");

    let created = watch.event("the spawn");
    assert_eq!(event_of(&created), ("created", "spawned"));
    assert_eq!(
        event_of(&watch.event("the delete")),
        ("gone", "soft_deleted")
    );
    assert_eq!(
        event_of(&watch.event("the restore")),
        ("created", "restored")
    );
}

/// A driver that persisted the last seq it handled resumes with exactly what it
/// missed, and nothing it already had.
#[test]
fn since_resumes_without_replaying() {
    let env = Env::new();
    let db = env.db();
    let id = seed(&db, "worker");
    db.set_hook_state(id, "working").expect("working");
    let head = db.latest_session_event_seq().expect("head");
    db.set_hook_state(id, "done").expect("done");

    let watch = env.watch(&["--json", "--since", &head.to_string(), "--for-secs", "30"]);

    // The missed event, immediately — a resume does not wait for the next
    // commit by somebody else.
    let missed = watch.event("the event missed while away");
    assert_eq!(missed["to_state"], Value::String("done".into()));
    assert!(missed["seq"].as_i64() > Some(head));
    watch.silent_for(Duration::from_secs(1));
}

/// A parked session takes no hook state, so a heartbeat's pane poll — or a
/// mirror pass carrying a host's last word — cannot report a turn on a session
/// that has no process to be in one.
#[test]
fn a_parked_session_gets_no_hook_events() {
    let env = Env::new();
    let db = env.db();
    let id = seed(&db, "parked");
    db.set_hook_state(id, "working").expect("working");

    let watch = env.watch(&["--json", "--for-secs", "30"]);
    settle();
    db.set_session_stopped(id, true).expect("park");

    let parked = watch.event("the park");
    assert_eq!(event_of(&parked), ("changed", "stopped"));
    assert_eq!(parked["stopped"], Value::Bool(true));
    assert_eq!(parked["state"], Value::String("stopped".into()));
    // The park cleared the latched state; the word came from thurbox, not the
    // agent, so nothing may launder it back into the column.
    assert_eq!(parked["from_state"], Value::String("working".into()));

    assert!(
        !db.set_hook_state(id, "working").expect("write"),
        "a parked row must refuse a hook state"
    );
    watch.silent_for(Duration::from_secs(1));

    db.set_session_stopped(id, false).expect("un-park");
    assert_eq!(
        event_of(&watch.event("the un-park")),
        ("changed", "started")
    );
}

/// The stream ends when its reader does, instead of sitting out the whole of
/// its `--for-secs`.
#[test]
fn the_stream_ends_when_the_reader_closes() {
    let env = Env::new();
    let db = env.db();
    let id = seed(&db, "worker");

    // Read directly rather than through the pump thread: this test has to *drop*
    // the pipe, which the pump would keep open.
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_thurbox-cli"));
    cmd.args(["watch", "--json", "--initial", "--for-secs", "120"]);
    cmd.env("HOME", env.path("home"));
    cmd.env("USERPROFILE", env.path("home"));
    cmd.env("XDG_DATA_HOME", env.path("home").join("xdg-data"));
    cmd.env("XDG_CONFIG_HOME", env.path("home").join("xdg-config"));
    cmd.env("THURBOX_CONFIG_DIR", env.path("config"));
    cmd.env("THURBOX_DATA_DIR", env.path("data"));
    cmd.env("THURBOX_SOCKET", "thurbox-watch-test");
    cmd.env_remove("THURBOX_SOCKET_FOR");
    let mut child = cmd
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn watch");

    let mut stdout = child.stdout.take().expect("piped stdout");
    let mut baseline = [0u8; 1];
    stdout.read_exact(&mut baseline).expect("the initial line");
    drop(stdout);

    // Something to write into the closed pipe: the exit is on the write, not on
    // a timer.
    settle();
    db.set_hook_state(id, "working").expect("working");

    let status = wait_for_exit(&mut child, Duration::from_secs(20));
    assert!(
        status,
        "watch must exit when its reader is gone, not run out its --for-secs"
    );
}

/// Poll a child for up to `limit`, returning whether it exited.
fn wait_for_exit(child: &mut Child, limit: Duration) -> bool {
    let deadline = std::time::Instant::now() + limit;
    while std::time::Instant::now() < deadline {
        match child.try_wait() {
            Ok(Some(_)) => return true,
            Ok(None) => std::thread::sleep(Duration::from_millis(50)),
            Err(_) => return false,
        }
    }
    let _ = child.kill();
    let _ = child.wait();
    false
}

/// `--initial` is the baseline plus the changes after it in one stream, and it
/// obeys `--text` like every other rendering.
#[test]
fn initial_emits_the_baseline_in_the_asked_for_format() {
    let env = Env::new();
    let db = env.db();
    let id = seed(&db, "worker");
    db.set_hook_state(id, "working").expect("working");

    let watch = env.watch(&["--text", "--initial", "--for-secs", "30"]);
    let baseline = watch.line("the baseline row");
    assert!(
        baseline.contains("present") && baseline.contains("worker") && baseline.contains("working"),
        "a --text baseline reads as a line, not JSON: {baseline}"
    );
    assert!(
        !baseline.trim_start().starts_with('{'),
        "--text must not emit JSON: {baseline}"
    );

    settle();
    db.set_hook_state(id, "done").expect("done");
    let changed = watch.line("the transition");
    assert!(
        changed.contains("changed") && changed.contains("working → done"),
        "a --text event names its transition: {changed}"
    );
}

/// TOON declares its columns once and then writes rows — the shape a stream
/// wants, and the format a piped reader gets by default.
#[test]
fn toon_declares_its_columns_once() {
    let env = Env::new();
    let db = env.db();
    let id = seed(&db, "worker");

    let watch = env.watch(&["--toon", "--for-secs", "30"]);
    settle();
    db.set_hook_state(id, "working").expect("working");
    db.set_hook_state(id, "done").expect("done");

    let header = watch.line("the TOON header");
    assert!(
        header.starts_with("events{") && header.ends_with("}:"),
        "the header names the fields: {header}"
    );
    for expected in ["working", "done"] {
        let row = watch.line("a TOON row");
        assert!(row.starts_with("  "), "a row is indented under the header");
        assert!(
            row.contains(expected),
            "row {row} should mention {expected}"
        );
        assert!(
            !row.contains("events{"),
            "the header is written once, not per row: {row}"
        );
    }
}

/// A `state` transition and a park landing in the same drained batch must each
/// report their own point-in-time `stopped`, not the batch's final row.
///
/// This is the same class of loss the event log exists to prevent, reintroduced
/// for the derived `state`/`stopped` fields: reading the row once per batch
/// would make an earlier event in the batch inherit a park that happened after
/// it, mislabeling a live transition as a park.
#[test]
fn a_state_event_sharing_a_batch_with_a_park_reports_its_own_state() {
    let env = Env::new();
    let db = env.db();
    let id = seed(&db, "worker");
    db.set_hook_state(id, "working").expect("working");

    let watch = env.watch(&["--json", "--for-secs", "30"]);
    settle();

    // Both inside a single gate window, so `drain` reads them as one batch.
    db.set_hook_state(id, "blocked").expect("blocked");
    std::thread::sleep(Duration::from_millis(10));
    db.set_session_stopped(id, true).expect("park");

    let transition = watch.event("the state transition");
    let park = watch.event("the park");

    assert_eq!(event_of(&transition), ("changed", "state"));
    assert_eq!(transition["to_state"], Value::String("blocked".into()));
    assert_eq!(
        transition["stopped"],
        Value::Bool(false),
        "the transition happened before the park: {transition}"
    );
    assert_eq!(
        transition["state"],
        Value::String("blocked".into()),
        "a later park in the same batch must not relabel this event: {transition}"
    );

    assert_eq!(event_of(&park), ("changed", "stopped"));
    assert_eq!(park["stopped"], Value::Bool(true));
    assert_eq!(park["state"], Value::String("stopped".into()));
}

/// A minimal §7.1-aware split, just enough to prove a quoted TOON row decodes
/// back to the fields the header promises rather than shifting on an embedded
/// delimiter.
fn split_toon_row(row: &str) -> Vec<String> {
    let mut fields = Vec::new();
    let mut chars = row.chars().peekable();
    loop {
        let mut field = String::new();
        if chars.peek() == Some(&'"') {
            chars.next();
            while let Some(c) = chars.next() {
                match c {
                    '"' => break,
                    '\\' => {
                        if let Some(next) = chars.next() {
                            field.push(match next {
                                'n' => '\n',
                                'r' => '\r',
                                't' => '\t',
                                other => other,
                            });
                        }
                    }
                    c => field.push(c),
                }
            }
        } else {
            while let Some(&c) = chars.peek() {
                if c == ',' {
                    break;
                }
                field.push(c);
                chars.next();
            }
        }
        fields.push(field);
        match chars.next() {
            Some(',') => continue,
            _ => break,
        }
    }
    fields
}

/// A session name that itself contains the row delimiter and a colon must not
/// shift every column after it — the TOON row is quoted and escaped per §7.2
/// by the shared encoder, not hand-joined.
#[test]
fn toon_row_escapes_a_name_that_contains_the_delimiter() {
    let env = Env::new();
    let db = env.db();
    let id = seed(&db, "fix: foo, bar");

    let watch = env.watch(&["--toon", "--for-secs", "30"]);
    settle();
    db.set_hook_state(id, "blocked").expect("blocked");

    let _header = watch.line("the TOON header");
    let row = watch.line("the event row");
    let fields = split_toon_row(row.trim_start());

    assert_eq!(
        fields.len(),
        8,
        "the comma and colon inside the session name must not add a column: {row}"
    );
    assert_eq!(
        fields[3], "fix: foo, bar",
        "the name must decode back exactly: {row}"
    );
    assert_eq!(
        fields[7],
        id.to_string(),
        "the session column must still be the id, not a shifted fragment: {row}"
    );
}

/// `--session` narrows the stream to one session, and the log is filtered
/// rather than the output.
#[test]
fn session_filter_reports_only_that_session() {
    let env = Env::new();
    let db = env.db();
    let mine = seed(&db, "mine");
    let theirs = seed(&db, "theirs");

    let watch = env.watch(&["--json", "--session", &mine.to_string(), "--for-secs", "30"]);
    settle();
    db.set_hook_state(theirs, "working").expect("theirs");
    db.set_hook_state(mine, "blocked").expect("mine");

    let event = watch.event("the filtered transition");
    assert_eq!(event["session"], Value::String(mine.to_string()));
    assert_eq!(event["to_state"], Value::String("blocked".into()));
    watch.silent_for(Duration::from_secs(1));
}
