//! CronTask end to end: the real `fleet` binary takes, shows and deletes
//! one, and the manager, on the file store with a test clock and the fake
//! runtime, creates its Tasks at the scheduled times and no others.

mod support;

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use fleet_api::{Clock, FieldManager, Observation, Store};
use fleet_manager::{Manager, RunOptions};
use fleet_runtime::{FakeHost, FakeRuntime};
use fleet_store::FileStore;
use serde_json::{Value, json};
use support::Env;

/// 2026-11-16T03:00:00Z, a Monday.
const MONDAY_3AM: u64 = 1_794_798_000_000;
const MINUTE: u64 = 60_000;
const DAY: u64 = 86_400_000;

#[derive(Debug)]
struct TestClock(AtomicU64);

impl Clock for TestClock {
    fn now(&self) -> u64 {
        self.0.load(Ordering::SeqCst)
    }
}

/// A store and a manager over `env`, both on one clock the test moves.
struct World {
    // The directories the store and the fake runtime live in.
    _env: Env,
    clock: Arc<TestClock>,
    store: FileStore,
    runtime: FakeRuntime,
}

impl World {
    fn new(name: &str, at: u64) -> World {
        let env = Env::new(name);
        let clock = Arc::new(TestClock(AtomicU64::new(at)));
        let store = FileStore::open_with_clock(&env.data, clock.clone()).expect("store");
        let runtime = FakeRuntime::new(FakeHost::open(&env.fake).expect("fake host"));
        World {
            _env: env,
            clock,
            store,
            runtime,
        }
    }

    fn at(&self, t: u64) {
        self.clock.0.store(t, Ordering::SeqCst);
    }

    fn reconcile(&self) {
        let manager = Manager {
            plugins: None,
            store: &self.store,
            runtime: &self.runtime,
            clock: self.clock.as_ref(),
            holder: "cron-task-test".into(),
            forge: None,
            signals: None,
            change_request_defaults: Default::default(),
            forge_defaults: Default::default(),
            gateway: None,
        };
        let report = manager
            .run(RunOptions {
                max_passes: 20,
                lease_ttl: 30_000,
            })
            .expect("a run");
        assert!(report.errors.is_empty(), "{:#?}", report.errors);
    }

    fn apply(&self, doc: Value) {
        let obj = serde_json::from_value(doc).expect("an object");
        fleet_manager::apply(&self.store, obj, &Default::default(), false).expect("applied");
    }

    fn cron_task(&self, name: &str) -> Value {
        match self.store.get("CronTask", name) {
            Observation::Known(Some(o)) => serde_json::to_value(o).unwrap(),
            other => panic!("CronTask/{name}: {other:?}"),
        }
    }

    /// The Tasks CronTask `name` created, oldest first, not being deleted.
    fn tasks_of(&self, name: &str) -> Vec<Value> {
        let Observation::Known(tasks) = self.store.list("Task") else {
            panic!("list Task");
        };
        tasks
            .into_iter()
            .filter(|t| {
                t.metadata
                    .labels
                    .get("crontask.fleet.dev/name")
                    .map(String::as_str)
                    == Some(name)
                    && !t.is_deleting()
            })
            .map(|t| serde_json::to_value(t).unwrap())
            .collect()
    }

    /// What the TaskController does when a worker's result lands: the Task
    /// is Concluded.
    fn conclude(&self, task: &str) {
        let lease = self
            .store
            .acquire_lease("test-conclude", 30_000)
            .expect("lease");
        let Observation::Known(Some(obj)) = self.store.get("Task", task) else {
            panic!("Task/{task}");
        };
        let mut status = obj.status.clone();
        status["phase"] = json!("Concluded");
        self.store
            .update_status(
                "Task",
                task,
                status,
                obj.metadata.resource_version,
                FieldManager::TaskController,
                Some(&lease),
            )
            .expect("concluded");
        self.store.release_lease(&lease).expect("release");
    }
}

fn nightly(extra: Value) -> Value {
    let mut doc = json!({
        "apiVersion": "fleet/v1alpha1",
        "kind": "CronTask",
        "metadata": {"name": "nightly"},
        "spec": {
            "schedule": "0 3 * * *",
            "template": {
                "metadata": {"labels": {"team": "review"}},
                "spec": {
                    "harness": "claude-code",
                    "host": "local",
                    "brief": "Review what landed today.",
                    "workspace": [],
                    "dispatch": true
                }
            }
        }
    });
    if let Value::Object(fields) = extra {
        for (k, v) in fields {
            doc["spec"][k] = v;
        }
    }
    doc
}

fn names(tasks: &[Value]) -> Vec<String> {
    tasks
        .iter()
        .map(|t| t["metadata"]["name"].as_str().unwrap().to_string())
        .collect()
}

#[test]
fn a_cron_task_creates_one_task_per_scheduled_time() {
    let w = World::new("cron-once", MONDAY_3AM - 10 * MINUTE);
    w.apply(nightly(json!({})));
    w.reconcile();
    assert!(w.tasks_of("nightly").is_empty(), "before its time");
    let st = &w.cron_task("nightly")["status"];
    assert_eq!(st["nextScheduleTime"], MONDAY_3AM);
    assert_eq!(st["phase"], "Scheduled");

    w.at(MONDAY_3AM + 5_000);
    w.reconcile();
    w.reconcile();
    let tasks = w.tasks_of("nightly");
    assert_eq!(
        names(&tasks),
        ["nightly-29913300"],
        "one Task, however often it is asked"
    );
    let task = &tasks[0];
    assert_eq!(task["metadata"]["labels"]["team"], "review");
    let uid = w.cron_task("nightly")["metadata"]["uid"].clone();
    assert_eq!(task["metadata"]["labels"]["crontask.fleet.dev/uid"], uid);
    assert_eq!(task["spec"]["brief"], "Review what landed today.");
    let st = &w.cron_task("nightly")["status"];
    assert_eq!(st["lastScheduleTime"], MONDAY_3AM);
    assert_eq!(st["nextScheduleTime"], MONDAY_3AM + DAY);
    assert_eq!(st["active"], json!(["nightly-29913300"]));
    let record = &st["effects"]["create/29913300"];
    assert_eq!(record["intent"]["action"], "create");
    assert_eq!(record["receipt"]["outcome"]["known"]["type"], "created");
}

#[test]
fn forbid_skips_a_time_while_a_task_runs_and_allow_does_not() {
    let w = World::new("cron-forbid", MONDAY_3AM - MINUTE);
    w.apply(nightly(json!({})));
    w.at(MONDAY_3AM);
    w.reconcile();
    assert_eq!(w.tasks_of("nightly").len(), 1);
    w.at(MONDAY_3AM + DAY);
    w.reconcile();
    assert_eq!(w.tasks_of("nightly").len(), 1, "Forbid is the default");
    let st = w.cron_task("nightly")["status"].clone();
    support::assert_condition(&st, "Skipped", "True", "Forbidden");

    let a = World::new("cron-allow", MONDAY_3AM - MINUTE);
    a.apply(nightly(json!({"concurrencyPolicy": "Allow"})));
    a.at(MONDAY_3AM);
    a.reconcile();
    a.at(MONDAY_3AM + DAY);
    a.reconcile();
    assert_eq!(a.tasks_of("nightly").len(), 2);
}

#[test]
fn replace_retires_the_running_task_before_it_creates() {
    let w = World::new("cron-replace", MONDAY_3AM - MINUTE);
    w.apply(nightly(json!({"concurrencyPolicy": "Replace"})));
    w.at(MONDAY_3AM);
    w.reconcile();
    w.at(MONDAY_3AM + DAY);
    w.reconcile();
    assert_eq!(
        names(&w.tasks_of("nightly")),
        ["nightly-29914740"],
        "only the new Task is left running"
    );
}

#[test]
fn a_time_missed_past_its_deadline_is_skipped() {
    let w = World::new("cron-deadline", MONDAY_3AM - MINUTE);
    w.apply(nightly(json!({"startingDeadlineSeconds": 600})));
    w.reconcile();
    // The manager was down from 02:59 until 03:30.
    w.at(MONDAY_3AM + 30 * MINUTE);
    w.reconcile();
    assert!(w.tasks_of("nightly").is_empty());
    let st = w.cron_task("nightly")["status"].clone();
    support::assert_condition(&st, "Skipped", "True", "MissedDeadline");
    assert_eq!(st["nextScheduleTime"], MONDAY_3AM + DAY);
}

#[test]
fn a_suspended_cron_task_creates_nothing() {
    let w = World::new("cron-suspend", MONDAY_3AM - MINUTE);
    w.apply(nightly(json!({"suspend": true})));
    w.at(MONDAY_3AM + MINUTE);
    w.reconcile();
    assert!(w.tasks_of("nightly").is_empty());
    let st = w.cron_task("nightly")["status"].clone();
    assert_eq!(st["phase"], "Suspended");
}

#[test]
fn history_keeps_the_newest_concluded_tasks() {
    let w = World::new("cron-history", MONDAY_3AM - MINUTE);
    w.apply(nightly(
        json!({"historyLimit": 1, "concurrencyPolicy": "Allow"}),
    ));
    for day in 0..3 {
        w.at(MONDAY_3AM + day * DAY);
        w.reconcile();
        let newest = names(&w.tasks_of("nightly")).pop().unwrap();
        w.conclude(&newest);
    }
    w.at(MONDAY_3AM + 3 * DAY - MINUTE);
    // Due again only at its next time; a spec change makes it due now.
    let mut doc = nightly(json!({"historyLimit": 1, "concurrencyPolicy": "Allow"}));
    doc["spec"]["template"]["metadata"]["labels"]["team"] = json!("review-2");
    w.apply(doc);
    w.reconcile();
    assert_eq!(names(&w.tasks_of("nightly")), ["nightly-29916180"]);
}

#[test]
fn an_intent_without_a_receipt_adopts_the_task_and_never_makes_a_second() {
    let w = World::new("cron-crash", MONDAY_3AM - MINUTE);
    w.apply(nightly(json!({})));
    w.at(MONDAY_3AM);
    w.reconcile();
    // As if the manager died after the create, before the receipt landed.
    let lease = w.store.acquire_lease("test-crash", 30_000).expect("lease");
    let Observation::Known(Some(obj)) = w.store.get("CronTask", "nightly") else {
        panic!("CronTask/nightly");
    };
    let mut status = obj.status.clone();
    status["effects"]["create/29913300"]
        .as_object_mut()
        .unwrap()
        .remove("receipt");
    status["notBefore"] = json!(MONDAY_3AM);
    w.store
        .update_status(
            "CronTask",
            "nightly",
            status,
            obj.metadata.resource_version,
            FieldManager::CronTaskController,
            Some(&lease),
        )
        .expect("receipt removed");
    w.store.release_lease(&lease).expect("release");

    w.at(MONDAY_3AM + MINUTE);
    w.reconcile();
    assert_eq!(w.tasks_of("nightly").len(), 1);
    let record = &w.cron_task("nightly")["status"]["effects"]["create/29913300"];
    assert_eq!(
        record["receipt"]["outcome"]["known"]["adopted"], true,
        "{record:#}"
    );
}

#[test]
fn the_cli_takes_shows_and_deletes_a_cron_task_and_refuses_a_bad_one() {
    let env = Env::new("cron-cli");
    env.apply(nightly(json!({}))).ok();
    assert_eq!(
        env.fleet(&["get", "crontask", "nightly"]).ok().json()["spec"]["schedule"],
        "0 3 * * *"
    );
    env.fleet(&["list", "crontasks"]).ok().stdout_has("nightly");

    env.apply(nightly(json!({"schedule": "0 3 * *"})))
        .code(1)
        .stderr_has("schedule");
    let mut bad = nightly(json!({}));
    bad["spec"]["template"]["spec"]["host"] = json!("");
    env.apply(bad).code(1).stderr_has("spec.template.spec");
    let mut labelled = nightly(json!({}));
    labelled["spec"]["template"]["metadata"]["labels"]["crontask.fleet.dev/uid"] = json!("x");
    env.apply(labelled)
        .code(1)
        .stderr_has("set only by a controller");
    let mut task = support::task("forged", false);
    task["metadata"]["labels"]["crontask.fleet.dev/name"] = json!("nightly");
    env.apply(task).stderr_has("crontask.fleet.dev/name");

    env.fleet(&["delete", "crontask", "nightly"])
        .ok()
        .stdout_has("CronTask/nightly deleted");
    env.fleet(&["get", "crontask", "nightly"]).code(3);
}
