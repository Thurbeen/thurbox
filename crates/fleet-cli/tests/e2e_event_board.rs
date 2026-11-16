//! The event board end to end: `fleet init` ensures Board `shared`, and the
//! real `fleet event publish` and `fleet event list` write and read its log
//! on a throwaway store, with the refusals, the dedupe, the filters and the
//! retention a reader relies on.

mod support;

use fleet_api::event::{CliSource, Filter, Page, Pattern, Publish, Source};
use fleet_api::{DeleteOutcome, FieldManager, LeaseToken, Object, Observation, Store, StoreError};
use serde_json::{Value, json};
use support::Env;

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
}

impl Store for BusyBoard<'_> {
    fn read_events(&self, board: &str, after: u64, _limit: usize) -> Result<Page, StoreError> {
        self.reads.set(self.reads.get() + 1);
        if self.reads.get() > 20 {
            return Err(StoreError::Busy("the list never ended".into()));
        }
        let noise = Publish {
            event_type: "noise".into(),
            subject: None,
            key: None,
            correlation: None,
            data: json!(null),
        };
        let source = Source::Cli(CliSource::new("busy").unwrap());
        self.inner
            .append_events(board, &source, vec![noise], FieldManager::Cli)?;
        self.inner.read_events(board, after, 1)
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
    };
    let filter = Filter {
        event_type: Some(Pattern::new("absent")),
        ..Filter::default()
    };
    let page = fleet_manager::list_events(&busy, "shared", 0, &filter, 10).unwrap();
    assert_eq!(page, Page::Events(Vec::new()));
}
