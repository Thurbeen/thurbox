//! The event board end to end: `fleet init` ensures Board `shared`, and the
//! real `fleet event publish` and `fleet event list` write and read its log
//! on a throwaway store, with the refusals, the dedupe, the filters and the
//! retention a reader relies on. Inside a session, `fleet event publish`
//! appends to the session's outbox and opens no store, and TaskController
//! ingests the outbox under the lease, as the Task whose outbox it is.

mod support;

use std::io::Write;

use fleet_api::event::{CliSource, Filter, Page, Pattern, Publish, Source};
use fleet_api::{DeleteOutcome, FieldManager, LeaseToken, Object, Observation, Store, StoreError};
use serde_json::{Value, json};
use support::{Env, wait};

fn board_env(name: &str) -> Env {
    let mut env = Env::new(name);
    env.set_var("USER", "tester");
    env.fleet(&["init"]).ok();
    env
}

fn publish(env: &Env, args: &[&str]) -> Value {
    let mut all = vec!["event", "publish"];
    all.extend_from_slice(args);
    all.push("--json");
    env.fleet(&all).ok().json()
}

fn list(env: &Env, args: &[&str]) -> Vec<Value> {
    let mut all = vec!["event", "list"];
    all.extend_from_slice(args);
    all.push("--json");
    env.fleet(&all)
        .ok()
        .json()
        .as_array()
        .expect("an array")
        .clone()
}

fn seqs(events: &[Value]) -> Vec<u64> {
    events.iter().map(|e| e["seq"].as_u64().unwrap()).collect()
}

#[test]
fn init_ensures_the_shared_board_with_its_defaults_and_keeps_it() {
    let env = board_env("board-init");
    let board = env.fleet(&["get", "board", "shared"]).ok().json();
    assert_eq!(
        board["spec"],
        json!({
            "retention": {"maxAgeSeconds": 604_800, "maxCount": 10_000},
            "maxDataBytes": 16_384
        })
    );
    publish(&env, &["--type", "note", "--data", "{}"]);
    env.fleet(&["init"]).ok();
    let board = env.fleet(&["get", "board", "shared"]).ok().json();
    assert_eq!(board["status"]["head"], 1, "{board}");
    assert_eq!(board["status"]["oldest"], 1, "{board}");
}

#[test]
fn a_published_event_is_listed_with_its_seq_and_cli_source() {
    let env = board_env("board-publish");
    let event = publish(
        &env,
        &[
            "--type",
            "git.push",
            "--subject",
            "repo/app",
            "--correlation",
            "run-7",
            "--data",
            r#"{"branch":"main"}"#,
        ],
    );
    assert_eq!(event["board"], "shared");
    assert_eq!(event["seq"], 1);
    assert_eq!(event["source"], "cli/tester");
    assert_eq!(event["type"], "git.push");
    assert!(event["time"].as_u64().unwrap() > 0);

    let events = list(&env, &[]);
    assert_eq!(events, vec![event.clone()]);

    env.fleet(&["event", "publish", "--type", "git.tag", "--data", "1"])
        .ok()
        .stdout_has("shared/2");
    env.fleet(&["event", "list"])
        .ok()
        .stdout_has("git.push")
        .stdout_has("cli/tester")
        .stdout_has("git.tag");
}

#[test]
fn the_same_key_from_the_same_source_keeps_the_first_seq() {
    let env = board_env("board-dedupe");
    let first = publish(&env, &["--type", "a", "--key", "k1", "--data", "1"]);
    let again = publish(&env, &["--type", "a", "--key", "k1", "--data", "2"]);
    assert_eq!(again, first);
    publish(&env, &["--type", "a", "--key", "k2", "--data", "3"]);
    assert_eq!(seqs(&list(&env, &[])), vec![1, 2]);

    let mut other = board_env("board-dedupe-other");
    publish(&other, &["--type", "a", "--key", "k1", "--data", "1"]);
    other.set_var("USER", "someone");
    let theirs = publish(&other, &["--type", "a", "--key", "k1", "--data", "1"]);
    assert_eq!(theirs["seq"], 2, "another source's key is its own");
}

#[test]
fn a_publish_breaking_the_board_rules_is_refused_and_appends_nothing() {
    let env = board_env("board-refusals");
    env.fleet(&[
        "event",
        "publish",
        "--type",
        "fleet.task.phase",
        "--data",
        "{}",
    ])
    .code(1)
    .stderr_has("fleet.");
    for bad in ["Git.Push", "git..push", ".git", "git.", "git push"] {
        env.fleet(&["event", "publish", "--type", bad, "--data", "{}"])
            .code(1)
            .stderr_has("type");
    }
    let long = "a".repeat(129);
    env.fleet(&["event", "publish", "--type", &long, "--data", "{}"])
        .code(1);
    let subject = "s".repeat(257);
    env.fleet(&[
        "event",
        "publish",
        "--type",
        "a",
        "--subject",
        &subject,
        "--data",
        "{}",
    ])
    .code(1)
    .stderr_has("subject");
    let big = serde_json::to_string(&"x".repeat(16_384)).unwrap();
    env.fleet(&["event", "publish", "--type", "a", "--data", &big])
        .code(1)
        .stderr_has("16384");
    env.fleet(&["event", "publish", "--type", "a", "--data", "{not json"])
        .code(1);
    env.fleet(&[
        "event", "publish", "--board", "nowhere", "--type", "a", "--data", "1",
    ])
    .code(3);
    assert!(list(&env, &[]).is_empty());
}

#[test]
fn list_filters_by_cursor_type_subject_source_and_correlation() {
    let env = board_env("board-filters");
    publish(
        &env,
        &["--type", "git.push", "--subject", "repo/a", "--data", "1"],
    );
    publish(
        &env,
        &[
            "--type",
            "git.tag",
            "--subject",
            "repo/b",
            "--correlation",
            "c1",
            "--data",
            "2",
        ],
    );
    publish(
        &env,
        &["--type", "ci.done", "--subject", "repo/a", "--data", "3"],
    );

    assert_eq!(seqs(&list(&env, &["--after", "1"])), vec![2, 3]);
    assert_eq!(seqs(&list(&env, &["--type", "git.*"])), vec![1, 2]);
    assert_eq!(seqs(&list(&env, &["--type", "git.tag"])), vec![2]);
    assert_eq!(seqs(&list(&env, &["--type", "git"])), Vec::<u64>::new());
    assert_eq!(seqs(&list(&env, &["--subject", "repo/a"])), vec![1, 3]);
    assert_eq!(seqs(&list(&env, &["--subject", "repo/*"])), vec![1, 2, 3]);
    assert_eq!(seqs(&list(&env, &["--correlation", "c1"])), vec![2]);
    assert_eq!(
        seqs(&list(&env, &["--source", "cli/tester"])),
        vec![1, 2, 3]
    );
    assert!(list(&env, &["--source", "cli/other"]).is_empty());
    assert_eq!(seqs(&list(&env, &["--limit", "2"])), vec![1, 2]);
    assert_eq!(
        seqs(&list(&env, &["--type", "git.*", "--after", "1"])),
        vec![2]
    );
    assert!(list(&env, &["--after", "3"]).is_empty());
}

#[test]
fn retention_by_count_expires_an_old_cursor_and_names_the_oldest() {
    let env = board_env("board-retention");
    env.apply(json!({
        "apiVersion": "fleet/v1alpha1",
        "kind": "Board",
        "metadata": {"name": "tiny"},
        "spec": {"retention": {"maxCount": 2}}
    }))
    .ok();
    for n in 1..=5 {
        let event = publish(
            &env,
            &[
                "--board",
                "tiny",
                "--type",
                "tick",
                "--data",
                &n.to_string(),
            ],
        );
        assert_eq!(event["seq"], n);
    }
    let board = env.fleet(&["get", "board", "tiny"]).ok().json();
    assert_eq!(board["status"], json!({"head": 5, "oldest": 4}));

    assert_eq!(
        seqs(&list(&env, &["--board", "tiny", "--after", "3"])),
        vec![4, 5]
    );
    env.fleet(&["event", "list", "--board", "tiny", "--after", "2"])
        .code(5)
        .stderr_has("oldest 4");
    let expired = env.fleet(&["event", "list", "--board", "tiny", "--json"]);
    expired.code(5);
    assert_eq!(expired.json(), json!({"expired": {"oldest": 4}}));
}

#[test]
fn a_deleted_board_takes_its_log_with_it() {
    let env = board_env("board-delete");
    let board = json!({
        "apiVersion": "fleet/v1alpha1",
        "kind": "Board",
        "metadata": {"name": "scratch"},
        "spec": {}
    });
    env.apply(board.clone()).ok();
    publish(&env, &["--board", "scratch", "--type", "a", "--data", "1"]);
    env.fleet(&["delete", "board", "scratch"]).ok();
    env.fleet(&["event", "list", "--board", "scratch"]).code(3);
    env.apply(board).ok();
    assert!(list(&env, &["--board", "scratch"]).is_empty());
    let event = publish(&env, &["--board", "scratch", "--type", "a", "--data", "1"]);
    assert_eq!(event["seq"], 1);
}

struct BusyBoard<'a> {
    inner: &'a fleet_store::FileStore,
    reads: std::cell::Cell<u32>,
    published_type: &'static str,
}

impl Store for BusyBoard<'_> {
    fn read_events(&self, board: &str, after: u64, limit: usize) -> Result<Page, StoreError> {
        self.reads.set(self.reads.get() + 1);
        if self.reads.get() > 20 {
            return Err(StoreError::Busy("the list never ended".into()));
        }
        let noise = Publish {
            event_type: self.published_type.into(),
            subject: None,
            key: None,
            correlation: None,
            data: json!(null),
        };
        let source = Source::Cli(CliSource::new("busy").unwrap());
        self.inner
            .append_events(board, &source, vec![noise], FieldManager::Cli, None)?;
        self.inner.read_events(board, after, limit)
    }
    fn event_head(&self, board: &str) -> Result<u64, StoreError> {
        self.inner.event_head(board)
    }
    fn get(&self, kind: &str, name: &str) -> Observation<Option<Object>> {
        self.inner.get(kind, name)
    }
    fn list(&self, kind: &str) -> Observation<Vec<Object>> {
        self.inner.list(kind)
    }
    fn create(&self, _: Object, _: FieldManager) -> Result<Object, StoreError> {
        unreachable!()
    }
    fn update_spec(
        &self,
        _: &str,
        _: &str,
        _: Value,
        _: Option<std::collections::BTreeMap<String, String>>,
        _: u64,
        _: FieldManager,
    ) -> Result<Object, StoreError> {
        unreachable!()
    }
    fn update_status(
        &self,
        _: &str,
        _: &str,
        _: Value,
        _: u64,
        _: FieldManager,
        _: Option<&LeaseToken>,
    ) -> Result<Object, StoreError> {
        unreachable!()
    }
    fn delete(&self, _: &str, _: &str, _: FieldManager) -> Result<DeleteOutcome, StoreError> {
        unreachable!()
    }
    fn delete_fenced(
        &self,
        _: &str,
        _: &str,
        _: &str,
        _: u64,
        _: FieldManager,
        _: &LeaseToken,
    ) -> Result<DeleteOutcome, StoreError> {
        unreachable!()
    }
    fn append_inbox(
        &self,
        _: &str,
        _: fleet_api::inbox::InboxEntry,
        _: u64,
        _: FieldManager,
        _: Option<&LeaseToken>,
    ) -> Result<Object, StoreError> {
        unreachable!()
    }
    fn record_delivery(
        &self,
        _: &str,
        _: Value,
        _: fleet_api::inbox::DeliveryReceipt,
        _: u64,
        _: FieldManager,
        _: &LeaseToken,
    ) -> Result<Object, StoreError> {
        unreachable!()
    }
    fn record_relay(
        &self,
        _: &str,
        _: Value,
        _: Vec<fleet_api::inbox::InboxEntry>,
        _: u64,
        _: FieldManager,
        _: &LeaseToken,
    ) -> Result<Object, StoreError> {
        unreachable!()
    }
    fn remove_finalizer(
        &self,
        _: &str,
        _: &str,
        _: &str,
        _: u64,
        _: FieldManager,
        _: &LeaseToken,
    ) -> Result<DeleteOutcome, StoreError> {
        unreachable!()
    }
    fn acquire_lease(&self, _: &str, _: u64) -> Result<LeaseToken, StoreError> {
        unreachable!()
    }
    fn renew_lease(&self, _: &LeaseToken, _: u64) -> Result<LeaseToken, StoreError> {
        unreachable!()
    }
    fn release_lease(&self, _: &LeaseToken) -> Result<(), StoreError> {
        unreachable!()
    }
}

#[test]
fn a_list_ends_at_the_head_it_started_from_while_others_publish() {
    let env = board_env("board-busy");
    publish(&env, &["--type", "wanted", "--data", "1"]);
    let store = fleet_store::FileStore::open(&env.data).unwrap();
    let busy = BusyBoard {
        inner: &store,
        reads: std::cell::Cell::new(0),
        published_type: "noise",
    };
    let filter = Filter {
        event_type: Some(Pattern::new("absent")),
        ..Filter::default()
    };
    let page = fleet_manager::list_events(&busy, "shared", 0, &filter, 10).unwrap();
    assert_eq!(page, Page::Events(Vec::new()));
}

#[test]
fn a_list_keeps_no_matching_event_published_after_it_started() {
    let env = board_env("board-busy-match");
    publish(&env, &["--type", "wanted", "--data", "1"]);
    let store = fleet_store::FileStore::open(&env.data).unwrap();
    let busy = BusyBoard {
        inner: &store,
        reads: std::cell::Cell::new(0),
        published_type: "wanted",
    };
    let filter = Filter {
        event_type: Some(Pattern::new("wanted")),
        ..Filter::default()
    };
    let Page::Events(events) = fleet_manager::list_events(&busy, "shared", 0, &filter, 10).unwrap()
    else {
        panic!("the list expired");
    };
    let seqs: Vec<u64> = events.iter().map(|e| e.seq).collect();
    assert_eq!(seqs, vec![1]);
}

#[test]
fn a_list_shows_events_a_crash_left_past_the_board_status() {
    let env = board_env("board-crash-list");
    publish(&env, &["--type", "a", "--data", "1"]);
    let path = env.data.join("objects/Board/shared.json");
    let mut obj: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    obj.as_object_mut().unwrap().remove("status");
    std::fs::write(&path, serde_json::to_vec(&obj).unwrap()).unwrap();
    let seqs: Vec<u64> = list(&env, &[])
        .iter()
        .map(|e| e["seq"].as_u64().unwrap())
        .collect();
    assert_eq!(seqs, vec![1]);
}

fn session_env(name: &str) -> Env {
    let mut env = Env::new(name);
    env.set_var("USER", "tester");
    env.fleet(&["init"]).ok();
    env.apply(support::task("a", true)).ok();
    env.reconcile().ok();
    env
}

fn session_var(env: &Env, var: &str) -> String {
    let session = env
        .host()
        .session(&env.session_key("a"))
        .unwrap()
        .expect("a session");
    session
        .env
        .get(var)
        .cloned()
        .unwrap_or_else(|| panic!("{var} in the session's environment"))
}

/// `fleet event publish <args>` as the session runs it: with the variables
/// Fleet passed it.
fn publish_inside(env: &Env, vars: &[(&str, &str)], args: &[&str]) -> support::Run {
    let mut cmd = env.command(&[&["event", "publish"], args].concat());
    for (k, v) in vars {
        cmd.env(k, v);
    }
    wait(cmd.spawn().expect("fleet event publish"))
}

fn session_vars(env: &Env) -> Vec<(&'static str, String)> {
    vec![
        ("FLEET_SIGNAL_FILE", session_var(env, "FLEET_SIGNAL_FILE")),
        ("FLEET_EVENT_FILE", session_var(env, "FLEET_EVENT_FILE")),
    ]
}

fn borrowed<'a>(vars: &'a [(&'static str, String)]) -> Vec<(&'static str, &'a str)> {
    vars.iter().map(|(k, v)| (*k, v.as_str())).collect()
}

fn append_raw(file: &str, bytes: &str) {
    std::fs::OpenOptions::new()
        .append(true)
        .create(true)
        .open(file)
        .expect("the outbox")
        .write_all(bytes.as_bytes())
        .expect("append");
}

#[test]
fn a_session_publishes_through_its_outbox_and_the_event_reads_as_its_task() {
    let env = session_env("event-session");
    let vars = session_vars(&env);
    let uid = env.get("a")["metadata"]["uid"].clone();

    let config = std::fs::read(&env.config).unwrap();
    std::fs::write(&env.config, "not a config").unwrap();
    publish_inside(
        &env,
        &borrowed(&vars),
        &[
            "--type",
            "git.push",
            "--subject",
            "repo/app",
            "--data",
            r#"{"branch":"main"}"#,
        ],
    )
    .ok()
    .stdout_has("offset 0");
    std::fs::write(&env.config, config).unwrap();
    assert!(
        list(&env, &[]).is_empty(),
        "nothing on the board before ingest"
    );

    env.reconcile().ok();
    let events = list(&env, &[]);
    assert_eq!(events.len(), 1, "{events:#?}");
    let e = &events[0];
    assert_eq!(e["type"], "git.push");
    assert_eq!(e["subject"], "repo/app");
    assert_eq!(e["data"], json!({"branch": "main"}));
    assert_eq!(
        e["source"],
        json!({"task": "a", "uid": uid, "lineage": []}),
        "{e}"
    );
    env.fleet(&["event", "list", "--source", "task/a"])
        .ok()
        .stdout_has("git.push")
        .stdout_has("task/a");

    let outbox = &vars[1].1;
    append_raw(
        outbox,
        "{\"board\":\"shared\",\"type\":\"spoof\",\"data\":1,\"source\":\"cli/evil\",\"task\":\"b\",\"uid\":\"x\"}\n",
    );
    env.reconcile().ok();
    let events = list(&env, &[]);
    assert_eq!(events.len(), 2, "{events:#?}");
    assert_eq!(events[1]["type"], "spoof");
    assert_eq!(events[1]["source"]["task"], "a", "{}", events[1]);
    assert_eq!(events[1]["source"]["uid"], uid);
}

#[test]
fn a_reserved_type_is_refused_at_publish_and_a_bad_line_is_skipped_and_counted() {
    let env = session_env("event-session-refused");
    let vars = session_vars(&env);
    let outbox = vars[1].1.clone();
    publish_inside(
        &env,
        &borrowed(&vars),
        &["--type", "fleet.task.phase", "--data", "{}"],
    )
    .code(1)
    .stderr_has("fleet.");
    publish_inside(
        &env,
        &borrowed(&vars),
        &["--board", "Not_A_Name", "--type", "a", "--data", "{}"],
    )
    .code(1)
    .stderr_has("name");
    assert!(
        std::fs::read(&outbox).map_or(true, |b| b.is_empty()),
        "a refused publish writes nothing"
    );

    append_raw(
        &outbox,
        "{\"board\":\"shared\",\"type\":\"fleet.x\",\"data\":1}\n",
    );
    append_raw(&outbox, "not json\n");
    publish_inside(&env, &borrowed(&vars), &["--type", "ok", "--data", "1"]).ok();
    env.reconcile().ok();
    let events = list(&env, &[]);
    assert_eq!(events.len(), 1, "{events:#?}");
    assert_eq!(events[0]["type"], "ok");
    let cursor = &env.get("a")["status"]["outbox"];
    assert_eq!(cursor["skipped"], 2, "{cursor}");
    assert!(cursor["lastSkipped"].is_string(), "{cursor}");
    assert_eq!(
        cursor["offset"],
        std::fs::metadata(&outbox).unwrap().len(),
        "{cursor}"
    );
}

#[test]
fn a_retried_ingest_appends_each_line_once() {
    let env = session_env("event-session-retry");
    let vars = session_vars(&env);
    publish_inside(&env, &borrowed(&vars), &["--type", "a", "--data", "1"]).ok();
    publish_inside(
        &env,
        &borrowed(&vars),
        &["--type", "b", "--key", "mine", "--data", "2"],
    )
    .ok();
    env.reconcile().ok();
    assert_eq!(list(&env, &[]).len(), 2);

    let path = env.object_path("Task", "a");
    let mut obj: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert!(obj["status"]["outbox"]["offset"].as_u64().unwrap() > 0);
    obj["status"]["outbox"]["offset"] = json!(0);
    std::fs::write(&path, obj.to_string()).unwrap();
    env.reconcile().ok();
    let events = list(&env, &[]);
    let seqs: Vec<u64> = events.iter().map(|e| e["seq"].as_u64().unwrap()).collect();
    assert_eq!(seqs, vec![1, 2], "{events:#?}");
    assert_eq!(events[1]["key"], "mine");
}

#[test]
fn a_torn_last_line_waits_for_its_newline() {
    let env = session_env("event-session-torn");
    let vars = session_vars(&env);
    let outbox = vars[1].1.clone();
    append_raw(&outbox, "{\"board\":\"shared\",\"type\":\"torn\",");
    env.reconcile().ok();
    assert!(list(&env, &[]).is_empty());
    assert_eq!(env.get("a")["status"]["outbox"]["skipped"], Value::Null);
    append_raw(&outbox, "\"data\":1}\n");
    env.reconcile().ok();
    let events = list(&env, &[]);
    assert_eq!(events.len(), 1, "{events:#?}");
    assert_eq!(events[0]["type"], "torn");
}

#[test]
fn a_session_without_an_outbox_refuses_to_publish() {
    let env = session_env("event-session-no-outbox");
    let signal = session_var(&env, "FLEET_SIGNAL_FILE");
    publish_inside(
        &env,
        &[("FLEET_SIGNAL_FILE", signal.as_str())],
        &["--type", "a", "--data", "1"],
    )
    .code(1)
    .stderr_has("FLEET_EVENT_FILE");
    env.reconcile().ok();
    assert!(list(&env, &[]).is_empty(), "no fallback to cli/USER");
}

#[test]
fn a_gone_session_is_read_until_a_read_finds_it_drained_and_then_no_more() {
    let env = session_env("event-session-gone");
    let outbox = session_var(&env, "FLEET_EVENT_FILE");
    env.host().vanish(&env.session_key("a")).unwrap();
    env.fleet(&["reconcile", "--max-passes", "1", "--json"])
        .ok();
    assert_eq!(env.get("a")["status"]["sessionState"], "Gone");
    append_raw(
        &outbox,
        "{\"board\":\"shared\",\"type\":\"last.words\",\"data\":1}\n",
    );
    env.reconcile().ok();
    let events = list(&env, &[]);
    assert_eq!(events.len(), 1, "{events:#?}");
    assert_eq!(events[0]["type"], "last.words");
    assert_eq!(env.get("a")["status"]["outbox"]["closed"], true);

    append_raw(
        &outbox,
        "{\"board\":\"shared\",\"type\":\"too.late\",\"data\":1}\n",
    );
    env.reconcile().ok();
    assert_eq!(
        list(&env, &[]).len(),
        1,
        "a drained gone session is read no more"
    );
}

#[test]
fn a_released_session_keeps_its_task_until_its_outbox_is_read() {
    let env = session_env("event-session-release");
    let vars = session_vars(&env);
    let outbox = vars[1].1.clone();
    publish_inside(&env, &borrowed(&vars), &["--type", "first", "--data", "1"]).ok();
    env.reconcile().ok();
    append_raw(
        &outbox,
        "{\"board\":\"shared\",\"type\":\"before.release\",\"data\":1}\n",
    );
    let held = std::fs::read(&outbox).unwrap();
    std::fs::remove_file(&outbox).unwrap();
    std::fs::create_dir(&outbox).unwrap();
    env.host().set_at_rest(&env.session_key("a"), true).unwrap();
    env.fleet(&["delete", "task", "a"]).ok();
    env.reconcile().ok();
    env.reconcile().ok();
    let task = env.get("a");
    assert!(
        task["status"]["conditions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["type"] == "Released" && c["reason"] == "OutboxUnread"),
        "an unread outbox holds the Task: {:#}",
        task["status"]
    );
    std::fs::remove_dir(&outbox).unwrap();
    std::fs::write(&outbox, held).unwrap();
    env.reconcile().ok();
    env.get_absent("a");
    let types: Vec<Value> = list(&env, &[]).iter().map(|e| e["type"].clone()).collect();
    assert_eq!(types, vec![json!("first"), json!("before.release")]);
}

#[test]
fn an_outbox_that_stays_unreadable_is_given_up_after_the_release_grace() {
    let env = session_env("event-session-unreadable");
    let outbox = session_var(&env, "FLEET_EVENT_FILE");
    std::fs::create_dir(&outbox).unwrap();
    env.host().set_at_rest(&env.session_key("a"), true).unwrap();
    env.fleet(&["delete", "task", "a"]).ok();
    env.reconcile().ok();
    env.reconcile().ok();
    assert_eq!(env.get("a")["status"]["phase"], "Releasing");

    let path = env.object_path("Task", "a");
    let mut obj: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    let released = obj["status"]["conditions"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|c| c["type"] == "Released")
        .expect("a Released condition");
    assert_eq!(released["reason"], "OutboxUnread", "{released}");
    released["lastTransitionTime"] = json!(0);
    std::fs::write(&path, obj.to_string()).unwrap();
    env.reconcile().ok();
    env.get_absent("a");
}

#[test]
fn a_concluded_task_reads_its_outbox_only_when_its_recheck_is_due() {
    let env = Env::new("event-session-concluded");
    env.apply(json!({"apiVersion": "fleet/v1alpha1", "kind": "Board", "metadata": {"name": "shared"}, "spec": {}}))
        .ok();
    env.apply(support::task("a", true)).ok();
    env.reconcile().ok();
    let vars = session_vars(&env);
    env.fleet(&[
        "result",
        "a",
        "--generation",
        "1",
        "--outcome",
        "shipped",
        "--summary",
        "done",
        "--output",
        "prUrl=\"https://example.test/pr/1\"",
    ])
    .ok();
    env.reconcile().ok();
    assert_eq!(env.get("a")["status"]["phase"], "Concluded");

    publish_inside(
        &env,
        &borrowed(&vars),
        &["--type", "after.result", "--data", "1"],
    )
    .ok();
    env.reconcile().ok();
    assert!(
        list(&env, &[]).is_empty(),
        "not due: the outbox is not read: {:#}",
        env.get("a")["status"]
    );

    env.expire_session_check("a");
    env.reconcile().ok();
    let events = list(&env, &[]);
    assert_eq!(events.len(), 1, "{events:#?}");
    assert_eq!(events[0]["type"], "after.result");
}

struct WorkerWritesDuringIngest {
    inner: fleet_store::FileStore,
    task: &'static str,
    fired: std::cell::Cell<bool>,
}

impl WorkerWritesDuringIngest {
    fn worker_writes_task_status(&self) {
        let Observation::Known(Some(task)) = self.inner.get("Task", self.task) else {
            panic!("the Task");
        };
        let mut status = task.status.clone();
        status["declared"] = json!([{"kind": "note"}]);
        self.inner
            .update_status(
                "Task",
                self.task,
                status,
                task.metadata.resource_version,
                FieldManager::Worker,
                None,
            )
            .expect("the worker's write");
    }
}

impl Store for WorkerWritesDuringIngest {
    fn plugin_mutate(
        &self,
        mutation: fleet_api::plugin::Mutation,
        by: FieldManager,
        lease: &LeaseToken,
    ) -> Result<Value, StoreError> {
        self.inner.plugin_mutate(mutation, by, lease)
    }
    fn served_kinds(&self) -> Vec<String> {
        self.inner.served_kinds()
    }
    fn validate_plugin_status(&self, kind: &str, status: &Value) -> Result<(), StoreError> {
        self.inner.validate_plugin_status(kind, status)
    }
    fn launch(&self, task: &Object) -> Result<Option<fleet_api::config::Execution>, StoreError> {
        self.inner.launch(task)
    }
    fn freeze_launch(
        &self,
        task: &Object,
        execution: &fleet_api::config::Execution,
        lease: &LeaseToken,
    ) -> Result<fleet_api::config::Execution, StoreError> {
        self.inner.freeze_launch(task, execution, lease)
    }
    fn get(&self, kind: &str, name: &str) -> Observation<Option<Object>> {
        self.inner.get(kind, name)
    }
    fn list(&self, kind: &str) -> Observation<Vec<Object>> {
        self.inner.list(kind)
    }
    fn create(&self, obj: Object, by: FieldManager) -> Result<Object, StoreError> {
        self.inner.create(obj, by)
    }
    fn create_fenced(
        &self,
        obj: Object,
        by: FieldManager,
        lease: &LeaseToken,
    ) -> Result<Object, StoreError> {
        self.inner.create_fenced(obj, by, lease)
    }
    fn retire_conversation_task(
        &self,
        name: &str,
        uid: &str,
        conversation_uid: &str,
        by: FieldManager,
        lease: &LeaseToken,
    ) -> Result<DeleteOutcome, StoreError> {
        self.inner
            .retire_conversation_task(name, uid, conversation_uid, by, lease)
    }
    fn retire_scheduled_task(
        &self,
        name: &str,
        uid: &str,
        cron_uid: &str,
        running: bool,
        by: FieldManager,
        lease: &LeaseToken,
    ) -> Result<DeleteOutcome, StoreError> {
        self.inner
            .retire_scheduled_task(name, uid, cron_uid, running, by, lease)
    }
    fn update_spec(
        &self,
        kind: &str,
        name: &str,
        spec: Value,
        labels: Option<std::collections::BTreeMap<String, String>>,
        expected: u64,
        by: FieldManager,
    ) -> Result<Object, StoreError> {
        self.inner
            .update_spec(kind, name, spec, labels, expected, by)
    }
    fn update_status(
        &self,
        kind: &str,
        name: &str,
        status: Value,
        expected: u64,
        by: FieldManager,
        lease: Option<&LeaseToken>,
    ) -> Result<Object, StoreError> {
        self.inner
            .update_status(kind, name, status, expected, by, lease)
    }
    fn delete(
        &self,
        kind: &str,
        name: &str,
        by: FieldManager,
    ) -> Result<DeleteOutcome, StoreError> {
        self.inner.delete(kind, name, by)
    }
    fn delete_fenced(
        &self,
        kind: &str,
        name: &str,
        uid: &str,
        expected: u64,
        by: FieldManager,
        lease: &LeaseToken,
    ) -> Result<DeleteOutcome, StoreError> {
        self.inner
            .delete_fenced(kind, name, uid, expected, by, lease)
    }
    fn append_inbox(
        &self,
        name: &str,
        entry: fleet_api::inbox::InboxEntry,
        expected: u64,
        by: FieldManager,
        lease: Option<&LeaseToken>,
    ) -> Result<Object, StoreError> {
        self.inner.append_inbox(name, entry, expected, by, lease)
    }
    fn amend_task(
        &self,
        name: &str,
        amendment: fleet_api::task::Amendment,
        entry: fleet_api::inbox::InboxEntry,
        expected: u64,
        by: FieldManager,
    ) -> Result<Object, StoreError> {
        self.inner.amend_task(name, amendment, entry, expected, by)
    }
    fn record_delivery(
        &self,
        name: &str,
        status: Value,
        receipt: fleet_api::inbox::DeliveryReceipt,
        expected: u64,
        by: FieldManager,
        lease: &LeaseToken,
    ) -> Result<Object, StoreError> {
        self.inner
            .record_delivery(name, status, receipt, expected, by, lease)
    }
    fn record_relay(
        &self,
        name: &str,
        status: Value,
        appends: Vec<fleet_api::inbox::InboxEntry>,
        expected: u64,
        by: FieldManager,
        lease: &LeaseToken,
    ) -> Result<Object, StoreError> {
        self.inner
            .record_relay(name, status, appends, expected, by, lease)
    }
    fn receive_event(
        &self,
        name: &str,
        delivery: fleet_api::gateway::Delivery,
        expected: u64,
        by: FieldManager,
    ) -> Result<(Object, fleet_api::conversation::Spooled), StoreError> {
        self.inner.receive_event(name, delivery, expected, by)
    }
    fn record_received(
        &self,
        name: &str,
        status: Value,
        expected: u64,
        by: FieldManager,
        lease: &LeaseToken,
    ) -> Result<Object, StoreError> {
        self.inner
            .record_received(name, status, expected, by, lease)
    }
    fn record_beat(
        &self,
        name: &str,
        instance: &str,
        beat: fleet_api::listener::Beat,
        expected: u64,
        by: FieldManager,
    ) -> Result<Object, StoreError> {
        self.inner.record_beat(name, instance, beat, expected, by)
    }
    fn record_refusal(
        &self,
        name: &str,
        reason: &str,
        fault: bool,
        at: fleet_api::Millis,
        expected: u64,
        by: FieldManager,
    ) -> Result<Object, StoreError> {
        self.inner
            .record_refusal(name, reason, fault, at, expected, by)
    }
    fn append_events(
        &self,
        board: &str,
        source: &fleet_api::event::Source,
        events: Vec<fleet_api::event::Publish>,
        by: FieldManager,
        lease: Option<&LeaseToken>,
    ) -> Result<Vec<fleet_api::event::Appended>, StoreError> {
        let appended = self.inner.append_events(board, source, events, by, lease)?;
        if !self.fired.replace(true) {
            self.worker_writes_task_status();
        }
        Ok(appended)
    }
    fn read_events(
        &self,
        board: &str,
        after: u64,
        limit: usize,
    ) -> Result<fleet_api::event::Page, StoreError> {
        self.inner.read_events(board, after, limit)
    }
    fn event_head(&self, board: &str) -> Result<u64, StoreError> {
        self.inner.event_head(board)
    }
    fn remove_finalizer(
        &self,
        kind: &str,
        name: &str,
        finalizer: &str,
        expected: u64,
        by: FieldManager,
        lease: &LeaseToken,
    ) -> Result<DeleteOutcome, StoreError> {
        self.inner
            .remove_finalizer(kind, name, finalizer, expected, by, lease)
    }
    fn validate(&self, obj: &Object) -> Result<(), StoreError> {
        self.inner.validate(obj)
    }
    fn acquire_lease(
        &self,
        holder: &str,
        ttl: fleet_api::Millis,
    ) -> Result<LeaseToken, StoreError> {
        self.inner.acquire_lease(holder, ttl)
    }
    fn renew_lease(
        &self,
        lease: &LeaseToken,
        ttl: fleet_api::Millis,
    ) -> Result<LeaseToken, StoreError> {
        self.inner.renew_lease(lease, ttl)
    }
    fn release_lease(&self, lease: &LeaseToken) -> Result<(), StoreError> {
        self.inner.release_lease(lease)
    }
}

#[test]
fn a_cursor_write_that_conflicts_is_retried_so_retention_never_duplicates_an_event() {
    let env = session_env("event-session-conflict");
    env.apply(json!({
        "apiVersion": "fleet/v1alpha1",
        "kind": "Board",
        "metadata": {"name": "tiny"},
        "spec": {"retention": {"maxCount": 1}}
    }))
    .ok();
    let vars = session_vars(&env);
    for t in ["first", "second"] {
        publish_inside(
            &env,
            &borrowed(&vars),
            &["--board", "tiny", "--type", t, "--data", "1"],
        )
        .ok();
    }

    let store = WorkerWritesDuringIngest {
        inner: fleet_store::FileStore::open(&env.data).unwrap(),
        task: "a",
        fired: std::cell::Cell::new(false),
    };
    let runtime =
        fleet_runtime::FakeRuntime::new(fleet_runtime::FakeHost::open(&env.fake).unwrap());
    let signals = fleet_runtime::FileSignals::open(env.data.join("signals")).unwrap();
    let manager = fleet_manager::Manager {
        store: &store,
        runtime: &runtime,
        clock: &fleet_store::SystemClock,
        holder: "conflict-test".into(),
        plugins: None,
        forge: None,
        signals: Some(&signals),
        agent_telemetry: None,
        change_request_defaults: Default::default(),
        forge_defaults: Default::default(),
        gateway: None,
    };
    manager
        .run(fleet_manager::RunOptions {
            max_passes: 5,
            lease_ttl: 30_000,
        })
        .unwrap();
    assert!(store.fired.get(), "the worker wrote during the ingest");
    env.reconcile().ok();
    env.reconcile().ok();

    let board = env.fleet(&["get", "board", "tiny"]).ok().json();
    assert_eq!(board["status"]["head"], 2, "each event once: {board}");
}

const BULK_LINES: usize = 50;

fn more_than_one_read() -> String {
    let line = format!(
        "{{\"board\":\"shared\",\"type\":\"bulk\",\"data\":\"{}\"}}\n",
        "x".repeat(15_000)
    );
    let text = line.repeat(BULK_LINES);
    assert!(text.len() as u64 > fleet_api::event::MAX_OUTBOX_READ);
    text
}

fn bulk_events(env: &Env) -> usize {
    list(env, &["--type", "bulk", "--limit", "10000"]).len()
}

fn one_pass(env: &Env) {
    env.fleet(&["reconcile", "--max-passes", "1", "--json"])
        .ok();
}

#[test]
fn a_released_task_waits_for_an_outbox_longer_than_one_read() {
    let env = session_env("event-session-draining");
    let outbox = session_var(&env, "FLEET_EVENT_FILE");
    env.host().set_at_rest(&env.session_key("a"), true).unwrap();
    env.fleet(&["delete", "task", "a"]).ok();
    one_pass(&env);
    append_raw(&outbox, &more_than_one_read());

    one_pass(&env);
    let st = env.get("a")["status"].clone();
    let released = st["conditions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["type"] == "Released")
        .cloned()
        .expect("a Released condition");
    assert_eq!(released["reason"], "OutboxDraining", "{st:#}");
    assert_eq!(released["status"], "False", "{st:#}");
    assert!(bulk_events(&env) < BULK_LINES);

    env.reconcile().ok();
    env.get_absent("a");
    assert_eq!(bulk_events(&env), BULK_LINES);
}

#[test]
fn a_gone_session_is_closed_only_once_a_read_drains_its_outbox() {
    let env = session_env("event-session-gone-bulk");
    let outbox = session_var(&env, "FLEET_EVENT_FILE");
    env.host().vanish(&env.session_key("a")).unwrap();
    one_pass(&env);
    assert_eq!(env.get("a")["status"]["sessionState"], "Gone");
    append_raw(&outbox, &more_than_one_read());

    one_pass(&env);
    assert!(bulk_events(&env) < BULK_LINES);
    assert_eq!(env.get("a")["status"]["outbox"]["closed"], Value::Null);

    env.reconcile().ok();
    assert_eq!(bulk_events(&env), BULK_LINES);
    assert_eq!(env.get("a")["status"]["outbox"]["closed"], true);
}
