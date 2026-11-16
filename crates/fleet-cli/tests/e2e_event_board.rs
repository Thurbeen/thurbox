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

type ReadHook<'a> = Box<dyn Fn(&fleet_store::FileStore, &str, u32) -> Result<(), StoreError> + 'a>;

struct HookedBoard<'a> {
    inner: &'a fleet_store::FileStore,
    reads: std::cell::Cell<u32>,
    before_read: ReadHook<'a>,
}

impl<'a> HookedBoard<'a> {
    fn new(inner: &'a fleet_store::FileStore, before_read: ReadHook<'a>) -> HookedBoard<'a> {
        HookedBoard {
            inner,
            reads: std::cell::Cell::new(0),
            before_read,
        }
    }
}

fn busy_board<'a>(
    inner: &'a fleet_store::FileStore,
    published_type: &'static str,
) -> HookedBoard<'a> {
    HookedBoard::new(
        inner,
        Box::new(move |store, board, reads| {
            if reads > 20 {
                return Err(StoreError::Busy("the list never ended".into()));
            }
            let noise = Publish {
                event_type: published_type.into(),
                subject: None,
                key: None,
                correlation: None,
                data: json!(null),
            };
            let source = Source::Cli(CliSource::new("busy").unwrap());
            store.append_events(board, &source, vec![noise], FieldManager::Cli, None)?;
            Ok(())
        }),
    )
}

impl Store for HookedBoard<'_> {
    fn read_events(&self, board: &str, after: u64, limit: usize) -> Result<Page, StoreError> {
        self.reads.set(self.reads.get() + 1);
        (self.before_read)(self.inner, board, self.reads.get())?;
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
    let busy = busy_board(&store, "noise");
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
    let busy = busy_board(&store, "wanted");
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
fn the_tail_of_a_line_too_long_to_read_never_becomes_an_event() {
    let env = session_env("event-session-overlong-tail");
    let vars = session_vars(&env);
    let outbox = vars[1].1.clone();
    append_raw(&outbox, &" ".repeat(fleet_api::event::MAX_OUTBOX_LINE));
    env.reconcile().ok();
    assert_eq!(env.get("a")["status"]["outbox"]["skipped"], 1);
    append_raw(
        &outbox,
        "{\"board\":\"shared\",\"type\":\"smuggled\",\"data\":1}\n{\"board\":\"shared\",\"type\":\"after\",\"data\":1}\n",
    );
    env.reconcile().ok();
    let types: Vec<Value> = list(&env, &[]).iter().map(|e| e["type"].clone()).collect();
    assert_eq!(types, vec![Value::from("after")]);
    assert_eq!(env.get("a")["status"]["outbox"]["skipped"], 1);
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

struct ObservedStore {
    inner: fleet_store::FileStore,
    task: &'static str,
    worker_writes_once: bool,
    fired: std::cell::Cell<bool>,
    appends: std::cell::Cell<u32>,
}

impl ObservedStore {
    fn over(env: &Env, worker_writes_once: bool) -> ObservedStore {
        ObservedStore {
            inner: fleet_store::FileStore::open(&env.data).unwrap(),
            task: "a",
            worker_writes_once,
            fired: std::cell::Cell::new(false),
            appends: std::cell::Cell::new(0),
        }
    }

    fn run_passes(&self, env: &Env, max_passes: u32) {
        let runtime =
            fleet_runtime::FakeRuntime::new(fleet_runtime::FakeHost::open(&env.fake).unwrap());
        let signals = fleet_runtime::FileSignals::open(env.data.join("signals")).unwrap();
        let manager = fleet_manager::Manager {
            store: self,
            runtime: &runtime,
            clock: &fleet_store::SystemClock,
            holder: "observed-store-test".into(),
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
                max_passes,
                lease_ttl: 30_000,
            })
            .unwrap();
    }

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

impl Store for ObservedStore {
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
        self.appends.set(self.appends.get() + 1);
        if self.worker_writes_once && !self.fired.replace(true) {
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

    let store = ObservedStore::over(&env, true);
    store.run_passes(&env, 5);
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

#[test]
fn five_hundred_queued_lines_land_in_one_pass_in_a_few_appends() {
    let env = session_env("event-session-batch");
    let outbox = session_var(&env, "FLEET_EVENT_FILE");
    let lines: String = (0..500)
        .map(|n| format!("{{\"board\":\"shared\",\"type\":\"small\",\"data\":{n}}}\n"))
        .collect();
    append_raw(&outbox, &lines);

    let store = ObservedStore::over(&env, false);
    store.run_passes(&env, 1);
    let events = list(&env, &["--type", "small", "--limit", "10000"]);
    assert_eq!(events.len(), 500);
    let data: Vec<u64> = events.iter().map(|e| e["data"].as_u64().unwrap()).collect();
    assert_eq!(data, (0..500).collect::<Vec<u64>>(), "in outbox order");
    assert!(
        store.appends.get() <= 4,
        "batched: {} appends",
        store.appends.get()
    );
}

#[test]
fn a_refused_line_in_a_batch_is_skipped_and_the_rest_land() {
    let env = session_env("event-session-batch-refused");
    env.apply(json!({
        "apiVersion": "fleet/v1alpha1",
        "kind": "Board",
        "metadata": {"name": "small"},
        "spec": {"maxDataBytes": 8}
    }))
    .ok();
    let outbox = session_var(&env, "FLEET_EVENT_FILE");
    append_raw(
        &outbox,
        concat!(
            "{\"board\":\"small\",\"type\":\"ok\",\"data\":1}\n",
            "{\"board\":\"small\",\"type\":\"too.big\",\"data\":\"0123456789\"}\n",
            "{\"board\":\"small\",\"type\":\"ok\",\"data\":2}\n",
        ),
    );
    env.reconcile().ok();
    let types: Vec<Value> = list(&env, &["--board", "small"])
        .iter()
        .map(|e| e["type"].clone())
        .collect();
    assert_eq!(types, vec![json!("ok"), json!("ok")]);
    let cursor = &env.get("a")["status"]["outbox"];
    assert_eq!(cursor["skipped"], 1, "{cursor}");
    assert!(
        cursor["lastSkipped"].as_str().unwrap().contains("8"),
        "{cursor}"
    );
}

const TIMED_OUT: i32 = 7;

fn wait_json(env: &Env, args: &[&str]) -> support::Run {
    let mut all = vec!["event", "wait", "--poll-ms", "50", "--json"];
    all.extend_from_slice(args);
    env.fleet(&all)
}

fn tiny_board(env: &Env) {
    env.apply(json!({
        "apiVersion": "fleet/v1alpha1",
        "kind": "Board",
        "metadata": {"name": "tiny"},
        "spec": {"retention": {"maxCount": 2}}
    }))
    .ok();
    for n in 1..=5 {
        publish(
            env,
            &[
                "--board",
                "tiny",
                "--type",
                "tick",
                "--data",
                &n.to_string(),
            ],
        );
    }
}

fn still_running(child: &mut std::process::Child) -> bool {
    child.try_wait().expect("try_wait").is_none()
}

fn exits_within(child: &mut std::process::Child, bound: std::time::Duration) -> bool {
    let deadline = std::time::Instant::now() + bound;
    while std::time::Instant::now() < deadline {
        if !still_running(child) {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    false
}

#[test]
fn wait_answers_at_once_with_the_matches_already_on_the_board() {
    let env = board_env("board-wait-now");
    publish(&env, &["--type", "a", "--data", "1"]);
    publish(&env, &["--type", "b", "--data", "2"]);
    publish(&env, &["--type", "b", "--data", "3"]);

    let run = wait_json(
        &env,
        &["--after", "0", "--type", "b", "--timeout-ms", "60000"],
    );
    run.ok();
    assert_eq!(seqs(run.json().as_array().unwrap()), vec![2, 3]);
    let run = wait_json(
        &env,
        &["--after", "0", "--limit", "1", "--timeout-ms", "60000"],
    );
    run.ok();
    assert_eq!(seqs(run.json().as_array().unwrap()), vec![1]);
    env.fleet(&["event", "wait", "--after", "2", "--timeout-ms", "60000"])
        .ok()
        .stdout_has("SEQ")
        .stdout_has("cli/tester");
}

#[test]
fn wait_with_no_match_before_its_timeout_prints_nothing_and_exits_timed_out() {
    let env = board_env("board-wait-timeout");
    publish(&env, &["--type", "a", "--data", "1"]);
    let run = wait_json(
        &env,
        &["--after", "0", "--type", "b", "--timeout-ms", "200"],
    );
    run.code(TIMED_OUT);
    assert_eq!(run.stdout, "");
    wait_json(&env, &["--after", "1", "--timeout-ms", "200"]).code(TIMED_OUT);
}

#[test]
fn wait_started_before_a_publish_answers_with_that_event() {
    let env = board_env("board-wait-later");
    publish(&env, &["--type", "a", "--data", "1"]);
    let mut child = env.spawn(&[
        "event",
        "wait",
        "--after",
        "1",
        "--type",
        "later",
        "--poll-ms",
        "50",
        "--timeout-ms",
        "60000",
        "--json",
    ]);
    std::thread::sleep(std::time::Duration::from_millis(300));
    assert!(still_running(&mut child), "the wait blocks with no match");
    publish(&env, &["--type", "other", "--data", "2"]);
    let event = publish(&env, &["--type", "later", "--data", "3"]);
    let run = wait(child);
    run.ok();
    assert_eq!(run.json(), json!([event]));
}

#[test]
fn watch_streams_each_new_match_once_in_order_and_ends_when_its_reader_goes() {
    use std::io::BufRead;
    let env = board_env("board-watch");
    publish(&env, &["--type", "before", "--data", "0"]);
    let mut child = env.spawn(&[
        "event",
        "watch",
        "--after",
        "1",
        "--type",
        "kept.*",
        "--poll-ms",
        "50",
        "--json",
    ]);
    let stdout = child.stdout.take().unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    let reader = std::thread::spawn(move || {
        let mut lines = std::io::BufReader::new(stdout).lines();
        for _ in 0..4 {
            match lines.next() {
                Some(Ok(line)) => tx.send(line).unwrap(),
                _ => return,
            }
        }
    });
    let next = || -> Value {
        let line = rx
            .recv_timeout(std::time::Duration::from_secs(30))
            .expect("a line from the watch");
        serde_json::from_str(&line).expect("an NDJSON line")
    };

    publish(&env, &["--type", "kept.a", "--data", "1"]);
    publish(&env, &["--type", "dropped", "--data", "2"]);
    publish(&env, &["--type", "kept.b", "--data", "3"]);
    assert_eq!(next()["seq"], 2);
    assert_eq!(next()["seq"], 4);
    std::thread::sleep(std::time::Duration::from_millis(300));
    for n in 5..=6 {
        publish(&env, &["--type", "kept.c", "--data", &n.to_string()]);
    }
    let later = [next(), next()];
    assert_eq!(seqs(&later), vec![5, 6], "{later:#?}");
    assert_eq!(later[0]["type"], "kept.c");
    reader.join().unwrap();

    assert!(still_running(&mut child), "the watch runs on");
    publish(&env, &["--type", "kept.d", "--data", "7"]);
    assert!(
        exits_within(&mut child, std::time::Duration::from_secs(30)),
        "the watch ends once its stdout is closed"
    );

    let mut killed = env.spawn(&["event", "watch", "--after", "0", "--json"]);
    std::thread::sleep(std::time::Duration::from_millis(200));
    killed.kill().unwrap();
    assert!(exits_within(
        &mut killed,
        std::time::Duration::from_secs(30)
    ));
}

#[test]
fn wait_and_watch_on_an_expired_cursor_name_the_oldest_and_exit_5() {
    let env = board_env("board-wait-expired");
    tiny_board(&env);
    for verb in ["wait", "watch"] {
        let mut args = vec!["event", verb, "--board", "tiny", "--after", "1"];
        if verb == "wait" {
            args.extend(["--timeout-ms", "60000"]);
        }
        env.fleet(&args).code(5).stderr_has("oldest 4");
        args.push("--json");
        let run = env.fleet(&args);
        run.code(5);
        assert_eq!(run.json(), json!({"expired": {"oldest": 4}}), "{verb}");
    }
    let run = wait_json(
        &env,
        &["--board", "tiny", "--after", "3", "--timeout-ms", "60000"],
    );
    run.ok();
    assert_eq!(seqs(run.json().as_array().unwrap()), vec![4, 5]);
}

#[test]
fn publish_wait_outside_a_session_prints_the_seq_as_publish_does() {
    let env = board_env("board-publish-wait-cli");
    let event = publish(&env, &["--type", "a", "--data", "1", "--wait"]);
    assert_eq!(event["seq"], 1);
    assert_eq!(event["source"], "cli/tester");
}

fn reconcile_until_done(env: &Env, mut child: std::process::Child) -> support::Run {
    for _ in 0..100 {
        if !still_running(&mut child) {
            break;
        }
        env.reconcile().ok();
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    wait(child)
}

fn publish_inside_spawned(env: &Env, vars: &[(&str, &str)], args: &[&str]) -> std::process::Child {
    let mut cmd = env.command(&[&["event", "publish"], args].concat());
    for (k, v) in vars {
        cmd.env(k, v);
    }
    cmd.spawn().expect("fleet event publish")
}

#[test]
fn publish_wait_inside_a_session_prints_the_event_once_fleet_ingests_it() {
    let env = session_env("event-session-wait");
    let vars = session_vars(&env);
    let uid = env.get("a")["metadata"]["uid"].clone();
    publish_inside(&env, &borrowed(&vars), &["--type", "first", "--data", "0"]).ok();

    let child = publish_inside_spawned(
        &env,
        &borrowed(&vars),
        &["--type", "git.push", "--data", "1", "--wait", "--json"],
    );
    let run = reconcile_until_done(&env, child);
    run.ok();
    let event = run.json();
    assert_eq!(event["seq"], 2, "{event}");
    assert_eq!(event["type"], "git.push");
    assert_eq!(event["source"]["task"], "a");
    assert_eq!(event["source"]["uid"], uid);
    assert_eq!(list(&env, &["--after", "1"]), vec![event]);

    let child = publish_inside_spawned(
        &env,
        &borrowed(&vars),
        &["--type", "keyed", "--key", "k1", "--data", "2", "--wait"],
    );
    let first = reconcile_until_done(&env, child);
    first.ok().stdout_has("shared/3 keyed from task/a");
    assert!(!first.stdout.contains("already"), "{}", first.stdout);

    let child = publish_inside_spawned(
        &env,
        &borrowed(&vars),
        &["--type", "keyed", "--key", "k1", "--data", "3", "--wait"],
    );
    reconcile_until_done(&env, child)
        .ok()
        .stdout_has("shared/3 keyed from task/a (already published under this key)");
    assert_eq!(seqs(&list(&env, &[])), vec![1, 2, 3]);
}

#[test]
fn publish_wait_inside_a_session_reports_a_skipped_line_and_a_timeout() {
    let env = session_env("event-session-wait-refused");
    let vars = session_vars(&env);
    let child = publish_inside_spawned(
        &env,
        &borrowed(&vars),
        &["--board", "nowhere", "--type", "a", "--data", "1", "--wait"],
    );
    reconcile_until_done(&env, child)
        .code(1)
        .stderr_has("offset 0")
        .stderr_has("nowhere");

    let outbox_len = std::fs::metadata(&vars[1].1).unwrap().len();
    let run = publish_inside(
        &env,
        &borrowed(&vars),
        &[
            "--type",
            "b",
            "--data",
            "2",
            "--wait",
            "--timeout-ms",
            "300",
        ],
    );
    run.code(TIMED_OUT)
        .stdout_has(&format!("outbox offset {outbox_len}"));
}

fn outbox_len(outbox: &str) -> u64 {
    std::fs::metadata(outbox).map_or(0, |m| m.len())
}

fn wait_for_append(outbox: &str, past: u64) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    while outbox_len(outbox) <= past {
        assert!(
            std::time::Instant::now() < deadline,
            "the publish never appended"
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

#[test]
fn publish_wait_on_a_skipped_line_exits_1_though_a_later_skip_overwrote_the_reason() {
    let env = session_env("event-session-wait-two-skips");
    let vars = session_vars(&env);
    let outbox = vars[1].1.clone();
    let child = publish_inside_spawned(
        &env,
        &borrowed(&vars),
        &[
            "--board",
            "nowhere",
            "--type",
            "first",
            "--data",
            "1",
            "--wait",
            "--timeout-ms",
            "60000",
        ],
    );
    wait_for_append(&outbox, 0);
    append_raw(
        &outbox,
        "{\"board\":\"nowhere\",\"type\":\"second\",\"data\":2}\n",
    );
    let started = std::time::Instant::now();
    let run = reconcile_until_done(&env, child);
    run.code(1).stderr_has("skipped").stderr_has("offset 0");
    assert!(
        started.elapsed() < std::time::Duration::from_secs(30),
        "the wait ended long before its timeout"
    );
    let cursor = &env.get("a")["status"]["outbox"];
    assert_eq!(cursor["skipped"], 2, "{cursor}");
    assert!(
        !cursor["lastSkipped"]
            .as_str()
            .unwrap()
            .starts_with("offset 0:"),
        "{cursor}"
    );
}

#[test]
fn publish_wait_in_a_remade_outbox_prints_the_new_event_not_the_old_files() {
    let env = session_env("event-session-wait-remade");
    let vars = session_vars(&env);
    let outbox = vars[1].1.clone();
    let padding = format!("\"{}\"", "x".repeat(100));
    for t in ["old.a", "old.b", "old.c"] {
        publish_inside(&env, &borrowed(&vars), &["--type", t, "--data", &padding]).ok();
    }
    env.reconcile().ok();
    let cursor = env.get("a")["status"]["outbox"].clone();
    assert_eq!(cursor["generation"], Value::Null, "{cursor}");
    assert!(cursor["offset"].as_u64().unwrap() > 300, "{cursor}");
    let old = list(&env, &[]);
    assert_eq!(old[0]["key"], "outbox-0-0", "{old:#?}");

    std::fs::remove_file(&outbox).unwrap();
    let child = publish_inside_spawned(
        &env,
        &borrowed(&vars),
        &["--type", "new.a", "--data", "1", "--wait", "--json"],
    );
    std::thread::sleep(std::time::Duration::from_millis(500));
    let run = reconcile_until_done(&env, child);
    run.ok();
    let event = run.json();
    assert_eq!(event["type"], "new.a", "{event}");
    assert_eq!(event["key"], "outbox-1-0", "{event}");
    assert_eq!(event["seq"], 4, "{event}");
}

#[test]
fn watch_on_a_cursor_that_expires_while_it_runs_names_where_it_had_got_to() {
    use std::io::BufRead;
    let env = board_env("board-watch-expires");
    tiny_board(&env);
    let mut child = env.spawn(&[
        "event",
        "watch",
        "--board",
        "tiny",
        "--after",
        "3",
        "--poll-ms",
        "10000",
        "--json",
    ]);
    let mut lines = std::io::BufReader::new(child.stdout.take().unwrap()).lines();
    for want in [4, 5] {
        let line = lines.next().expect("a line").expect("a line");
        let event: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(event["seq"], want);
    }
    for n in 6..=10 {
        publish(
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
    }
    let rest: Vec<String> = lines.map_while(Result::ok).collect();
    let run = wait(child);
    run.code(5).stderr_has("after 5");
    assert_eq!(rest.len(), 1, "{rest:?}");
    let expired: Value = serde_json::from_str(&rest[0]).unwrap();
    let oldest = expired["expired"]["oldest"]
        .as_u64()
        .expect("an expired answer");
    assert!(oldest > 6, "the events after 5 are gone: {expired}");
}

struct FixedClock(u64);

impl fleet_api::Clock for FixedClock {
    fn now(&self) -> u64 {
        self.0
    }
}

fn cursor_on(
    board: &str,
    after: u64,
    event_type: Option<&str>,
) -> fleet_manager::verbs::EventCursor {
    fleet_manager::verbs::EventCursor {
        board: board.into(),
        after,
        filter: Filter {
            event_type: event_type.map(Pattern::new),
            ..Filter::default()
        },
    }
}

#[test]
fn wait_reads_again_after_a_busy_or_unknown_store_and_warns_of_each() {
    let env = board_env("board-wait-transient");
    let event = publish(&env, &["--type", "a", "--data", "1"]);
    let store = fleet_store::FileStore::open(&env.data).unwrap();
    let flaky = HookedBoard::new(
        &store,
        Box::new(|_, _, reads| match reads {
            1 => Err(StoreError::Busy("a writer holds the lock".into())),
            2 => Err(StoreError::Unknown("the log could not be read".into())),
            _ => Ok(()),
        }),
    );
    let mut warned = Vec::new();
    let mut pauses = 0;
    let mut pause = |_| pauses += 1;
    let mut warn = |e: &StoreError| warned.push(e.to_string());
    let mut poll = fleet_manager::verbs::Poll {
        every_ms: 50,
        pause: &mut pause,
        warn: &mut warn,
    };
    let mut cursor = cursor_on("shared", 0, None);
    let page = fleet_manager::verbs::wait_events(
        &flaky,
        &FixedClock(0),
        &mut cursor,
        10,
        u64::MAX,
        &mut poll,
    )
    .unwrap();
    let Some(Page::Events(events)) = page else {
        panic!("no events: {page:?}");
    };
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].seq, event["seq"].as_u64().unwrap());
    assert_eq!(warned.len(), 2, "{warned:?}");
    assert_eq!(pauses, 2);

    let refused = HookedBoard::new(
        &store,
        Box::new(|_, _, _| Err(StoreError::NotFound("Board/shared".into()))),
    );
    let mut cursor = cursor_on("shared", 0, None);
    let mut pause = |_| {};
    let mut warn = |_: &StoreError| panic!("not transient");
    let mut poll = fleet_manager::verbs::Poll {
        every_ms: 50,
        pause: &mut pause,
        warn: &mut warn,
    };
    assert!(matches!(
        fleet_manager::verbs::wait_events(
            &refused,
            &FixedClock(0),
            &mut cursor,
            10,
            u64::MAX,
            &mut poll
        ),
        Err(StoreError::NotFound(_))
    ));
}

fn publish_many(store: &fleet_store::FileStore, types: impl Iterator<Item = String>) {
    let publishes = types
        .map(|t| Publish {
            event_type: t,
            subject: None,
            key: None,
            correlation: None,
            data: json!(null),
        })
        .collect();
    let source = Source::Cli(CliSource::new("bulk").unwrap());
    store
        .append_events("shared", &source, publishes, FieldManager::Cli, None)
        .unwrap();
}

fn next_seqs(
    cursor: &mut fleet_manager::verbs::EventCursor,
    store: &dyn Store,
    limit: usize,
) -> Vec<u64> {
    match cursor.next(store, limit).unwrap() {
        Page::Events(events) => events.iter().map(|e| e.seq).collect(),
        expired => panic!("{expired:?}"),
    }
}

#[test]
fn an_event_cursor_reads_across_a_page_boundary_and_a_limit_cut_repeats_and_skips_nothing() {
    let env = board_env("board-cursor-pages");
    let store = fleet_store::FileStore::open(&env.data).unwrap();
    publish_many(
        &store,
        (1..=2005).map(|n| {
            if n % 2 == 0 {
                "even".into()
            } else {
                "odd".into()
            }
        }),
    );

    let mut all = cursor_on("shared", 0, None);
    assert_eq!(
        next_seqs(&mut all, &store, 10_000),
        (1..=2005).collect::<Vec<_>>()
    );
    assert_eq!(all.after, 2005);
    assert!(next_seqs(&mut all, &store, 10_000).is_empty());

    let mut even = cursor_on("shared", 995, Some("even"));
    assert_eq!(next_seqs(&mut even, &store, 3), vec![996, 998, 1000]);
    assert_eq!(even.after, 1000);
    assert_eq!(next_seqs(&mut even, &store, 3), vec![1002, 1004, 1006]);
    let rest = next_seqs(&mut even, &store, 10_000);
    assert_eq!(rest, (1008..=2004).step_by(2).collect::<Vec<_>>());
    assert_eq!(even.after, 2005, "past the odd last event it read");

    publish_many(&store, ["even".to_string()].into_iter());
    assert_eq!(next_seqs(&mut even, &store, 10), vec![2006]);
}
