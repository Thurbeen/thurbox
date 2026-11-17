//! Board `fleet` end to end: `fleet init` ensures it, and TaskController
//! publishes `fleet.task.phase`, `fleet.task.declared` and
//! `fleet.task.result` to it, as the controller, after the write that
//! caused each, through the real `fleet` binary on a throwaway store and
//! the fake host. A publish that fails keeps its intent in the Task's
//! status, and a retry appends each event once.

mod support;

use serde_json::{Value, json};
use support::{Env, task};

const CONTROLLER: &str = "controller/task-controller";

fn fleet_env(name: &str) -> Env {
    let mut env = Env::new(name);
    env.set_var("USER", "tester");
    env.fleet(&["init"]).ok();
    env
}

fn briefed(env: &Env, name: &str) {
    env.apply(task(name, true)).ok();
    env.reconcile().ok();
    assert_eq!(env.status(name)["phase"], "Briefed");
}

fn fleet_events(env: &Env, args: &[&str]) -> Vec<Value> {
    let mut all = vec!["event", "list", "--board", "fleet", "--json"];
    all.extend_from_slice(args);
    env.fleet(&all)
        .ok()
        .json()
        .as_array()
        .expect("an array")
        .clone()
}

fn of_type(env: &Env, event_type: &str) -> Vec<Value> {
    fleet_events(env, &["--type", event_type])
}

fn record_result(env: &Env, name: &str, summary: &str) {
    env.fleet(&[
        "result",
        name,
        "--generation",
        "1",
        "--outcome",
        "shipped",
        "--summary",
        summary,
        "--output",
        "prUrl=\"https://example.test/pr/1\"",
    ])
    .ok();
}

fn segments(env: &Env, board: &str) -> Vec<std::path::PathBuf> {
    fn walk(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
        for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, out);
            } else if path.extension().is_some_and(|e| e == "jsonl") {
                out.push(path);
            }
        }
    }
    let mut found = Vec::new();
    walk(&env.data.join("boards").join(board), &mut found);
    found.sort();
    found
}

#[test]
fn init_ensures_the_fleet_board_beside_the_shared_one() {
    let env = fleet_env("fleet-board-init");
    let board = env.fleet(&["get", "board", "fleet"]).ok().json();
    assert_eq!(
        board["spec"],
        json!({
            "retention": {"maxAgeSeconds": 604_800, "maxCount": 10_000},
            "maxDataBytes": 16_384
        })
    );
    env.fleet(&["get", "board", "shared"]).ok();
    env.fleet(&["init"]).ok();
    env.fleet(&["get", "board", "fleet"]).ok();
}

#[test]
fn each_phase_change_is_one_fleet_task_phase_event_from_the_controller() {
    let env = fleet_env("fleet-phase");
    briefed(&env, "a");
    let uid = env.get("a")["metadata"]["uid"]
        .as_str()
        .unwrap()
        .to_string();

    let events = of_type(&env, "fleet.task.*");
    assert!(!events.is_empty(), "no fleet.task events");
    for e in &events {
        assert_eq!(e["type"], "fleet.task.phase", "{e:#}");
        assert_eq!(e["source"], CONTROLLER, "{e:#}");
        assert_eq!(e["subject"], "a", "{e:#}");
        assert!(e["data"]["reason"].is_string(), "{e:#}");
    }
    assert_eq!(events[0]["data"]["from"], "Pending", "{events:#?}");
    assert_eq!(events.last().unwrap()["data"]["to"], "Briefed");
    for pair in events.windows(2) {
        assert_eq!(pair[0]["data"]["to"], pair[1]["data"]["from"], "{pair:#?}");
    }
    let keys: Vec<&str> = events.iter().map(|e| e["key"].as_str().unwrap()).collect();
    for (i, key) in keys.iter().enumerate() {
        assert_eq!(*key, format!("task/a/{uid}/phase/{}", i + 1));
    }

    let again = env.reconcile().ok().json();
    assert_eq!(again["statusWrites"], 0, "{again:#}");
    assert_eq!(of_type(&env, "fleet.task.*"), events, "nothing new");
    assert_eq!(fleet_events(&env, &["--source", CONTROLLER]), events);
    let st = env.get("a")["status"].clone();
    assert!(st["fleetEvents"]["pending"].is_null(), "{st:#}");
}

#[test]
fn a_declaration_is_one_fleet_task_declared_event_once_the_controller_sees_it() {
    let env = fleet_env("fleet-declared");
    briefed(&env, "a");
    let note = json!({"kind": "note", "text": "found a flaky test"});
    env.fleet(&["declare", "a", "--json", &note.to_string()])
        .ok();
    env.fleet(&["declare", "a", "--json", &note.to_string()])
        .ok();
    assert!(
        of_type(&env, "fleet.task.declared").is_empty(),
        "not seen yet"
    );

    env.reconcile().ok();
    let events = of_type(&env, "fleet.task.declared");
    assert_eq!(events.len(), 1, "{events:#?}");
    let e = &events[0];
    assert_eq!(e["source"], CONTROLLER);
    assert_eq!(e["subject"], "a");
    assert_eq!(e["data"], json!({"index": 0, "declaration": note}));

    let big = json!({"kind": "dump", "text": "x".repeat(20_000)});
    env.fleet(&["declare", "a", "--json", &big.to_string()])
        .ok();
    env.reconcile().ok();
    let events = of_type(&env, "fleet.task.declared");
    assert_eq!(events.len(), 2, "{events:#?}");
    assert_eq!(
        events[1]["data"],
        json!({"index": 1, "declaration": {"kind": "dump"}, "truncated": true})
    );
    env.reconcile().ok();
    assert_eq!(of_type(&env, "fleet.task.declared").len(), 2);
}

#[test]
fn a_recorded_result_is_one_fleet_task_result_event_with_its_summary_cut_short() {
    let env = fleet_env("fleet-result");
    briefed(&env, "a");
    let summary = "s".repeat(5_000);
    record_result(&env, "a", &summary);
    env.reconcile().ok();

    let events = of_type(&env, "fleet.task.result");
    assert_eq!(events.len(), 1, "{events:#?}");
    let e = &events[0];
    assert_eq!(e["source"], CONTROLLER);
    assert_eq!(e["subject"], "a");
    let data = &e["data"];
    assert_eq!(data["generation"], 1, "{data:#}");
    assert_eq!(data["outcome"], "shipped", "{data:#}");
    assert_eq!(data["summaryTruncated"], true, "{data:#}");
    let cut = data["summary"].as_str().unwrap();
    assert!(
        cut.len() < summary.len() && summary.starts_with(cut),
        "{cut}"
    );
    assert!(data.get("outputs").is_none(), "{data:#}");

    let phases = of_type(&env, "fleet.task.phase");
    let last = phases.last().unwrap();
    assert_eq!(last["data"]["from"], "Briefed", "{last:#}");
    assert_eq!(last["data"]["to"], "Concluded", "{last:#}");
    assert_eq!(last["data"]["reason"], "ResultRecorded", "{last:#}");

    env.reconcile().ok();
    assert_eq!(of_type(&env, "fleet.task.result").len(), 1);
}

#[test]
fn a_subscription_to_fleet_task_result_hears_of_another_task_s_result() {
    let env = fleet_env("fleet-subscribed");
    briefed(&env, "watcher");
    briefed(&env, "a");
    env.fleet(&[
        "event",
        "subscribe",
        "watcher",
        "--board",
        "fleet",
        "--type",
        "fleet.task.result",
    ])
    .ok();
    env.reconcile().ok();

    record_result(&env, "a", "done");
    env.fleet(&["reconcile", "--json", "--max-passes", "20"])
        .ok();
    let texts = env
        .host()
        .session(&env.session_key("watcher"))
        .unwrap()
        .expect("a session")
        .texts;
    let notices: Vec<&String> = texts
        .iter()
        .filter(|t| t.contains("fleet event list"))
        .collect();
    assert_eq!(notices.len(), 1, "{texts:#?}");
    assert!(notices[0].contains("Board fleet"), "{}", notices[0]);
    assert!(notices[0].contains("1 new event "), "{}", notices[0]);
}

#[test]
fn a_failed_publish_keeps_its_intent_and_a_retry_appends_each_event_once() {
    let env = fleet_env("fleet-retry");
    briefed(&env, "a");
    let before = of_type(&env, "fleet.task.phase");
    let segment = segments(&env, "fleet").pop().expect("a segment");
    let good = std::fs::read(&segment).unwrap();
    std::fs::write(&segment, b"not an event\n").unwrap();

    record_result(&env, "a", "done");
    env.reconcile().ok();
    let st = env.get("a")["status"].clone();
    assert_eq!(st["phase"], "Concluded", "{st:#}");
    let pending = st["fleetEvents"]["pending"].as_array().cloned();
    assert_eq!(
        pending.as_ref().map(Vec::len),
        Some(1),
        "the phase change waits: {st:#}"
    );

    std::fs::write(&segment, &good).unwrap();
    let path = env.object_path("Task", "a");
    let unpublished = std::fs::read(&path).unwrap();
    env.reconcile().ok();
    let after = fleet_events(&env, &[]);
    assert_eq!(after.len(), before.len() + 2, "{after:#?}");
    assert!(
        env.get("a")["status"]["fleetEvents"]["pending"].is_null(),
        "published"
    );

    std::fs::write(&path, &unpublished).unwrap();
    env.reconcile().ok();
    assert_eq!(fleet_events(&env, &[]), after, "the keys hold: {after:#?}");
}

#[test]
fn the_cli_publishes_to_board_fleet_but_never_under_the_fleet_prefix() {
    let env = fleet_env("fleet-refused");
    env.fleet(&[
        "event",
        "publish",
        "--board",
        "fleet",
        "--type",
        "fleet.task.phase",
        "--data",
        "{}",
    ])
    .code(1)
    .stderr_has("fleet.");
    env.fleet(&[
        "event", "publish", "--board", "fleet", "--type", "note", "--data", "{}",
    ])
    .ok();
    let events = fleet_events(&env, &[]);
    assert_eq!(events.len(), 1, "{events:#?}");
    assert_eq!(events[0]["source"], "cli/tester");
    assert!(fleet_events(&env, &["--source", CONTROLLER]).is_empty());
}

#[test]
fn a_task_from_before_board_fleet_publishes_none_of_its_past() {
    let env = fleet_env("fleet-upgrade");
    briefed(&env, "a");
    let path = env.object_path("Task", "a");
    let mut obj: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    obj["status"].as_object_mut().unwrap().remove("fleetEvents");
    std::fs::write(&path, obj.to_string()).unwrap();
    let before = fleet_events(&env, &[]);
    let note = json!({"kind": "note", "text": "old"});
    env.fleet(&["declare", "a", "--json", &note.to_string()])
        .ok();
    let mut old = obj.clone();
    old["status"] = env.get("a")["status"].clone();
    old["status"]["result"] =
        json!({"outcome": "shipped", "generation": 1, "outputs": {"prUrl": "u"}});
    old["status"]["phase"] = json!("Concluded");
    old["metadata"] = env.get("a")["metadata"].clone();
    std::fs::write(&path, old.to_string()).unwrap();

    env.reconcile().ok();
    assert_eq!(fleet_events(&env, &[]), before, "no backlog");
    let record = env.get("a")["status"]["fleetEvents"].clone();
    assert_eq!(record["declared"], 1, "{record:#}");
    assert_eq!(record["result"], 1, "{record:#}");

    let fresh = json!({"kind": "note", "text": "new"});
    env.fleet(&["declare", "a", "--json", &fresh.to_string()])
        .ok();
    env.reconcile().ok();
    let declared = of_type(&env, "fleet.task.declared");
    assert_eq!(declared.len(), 1, "{declared:#?}");
    assert_eq!(declared[0]["data"]["index"], 1);
}

#[test]
fn a_task_released_in_one_pass_still_publishes_its_last_phase_change() {
    let env = fleet_env("fleet-released");
    env.apply(task("idle", false)).ok();
    env.reconcile().ok();
    env.fleet(&["delete", "task", "idle"]).ok();
    env.reconcile().ok();
    env.get_absent("idle");
    let events = of_type(&env, "fleet.task.phase");
    let last = events.last().expect("a phase event");
    assert_eq!(last["subject"], "idle", "{events:#?}");
    assert_eq!(last["data"]["to"], "Releasing", "{last:#}");
}

#[test]
fn without_board_fleet_the_events_are_skipped_and_counted_and_nothing_fails() {
    let env = fleet_env("fleet-no-board");
    env.fleet(&["delete", "board", "fleet"]).ok();
    briefed(&env, "a");
    let record = env.get("a")["status"]["fleetEvents"].clone();
    assert!(record["pending"].is_null(), "{record:#}");
    assert!(record["skipped"].as_u64().unwrap() > 0, "{record:#}");
    assert!(
        record["lastSkipped"].as_str().unwrap().contains("/phase/"),
        "{record:#}"
    );
}
