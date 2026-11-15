//! CronTaskController decisions that the end-to-end tests cannot stage: a
//! pass whose plan the manager never finished, fed back as the next input.

use fleet_api::meta::Object;
use fleet_controllers::{CronTaskController, CronTaskInput};
use serde_json::{Value, json};

/// 2026-11-16T03:00:00Z.
const T: u64 = 1_794_798_000_000;
const MINUTE: u64 = 60_000;

fn cron_task(status: Value) -> Object {
    cron_task_with(json!({}), status)
}

/// A CronTask whose spec has `extra` merged in.
fn cron_task_with(extra: Value, status: Value) -> Object {
    let mut obj: Value = json!({
        "apiVersion": "fleet/v1alpha1",
        "kind": "CronTask",
        "metadata": {"name": "nightly", "uid": "u1", "generation": 1,
                     "resourceVersion": 1, "creationTimestamp": T - MINUTE},
        "spec": {
            "schedule": "0 3 * * *",
            "template": {"spec": {"harness": "claude-code", "brief": "Review.", "workspace": []}}
        },
        "status": status
    });
    for (k, v) in extra.as_object().expect("an object") {
        obj["spec"][k] = v.clone();
    }
    serde_json::from_value(obj).expect("a CronTask")
}

/// A Task of this CronTask, in `phase`.
fn task(name: &str, uid: &str, phase: &str) -> Object {
    serde_json::from_value(json!({
        "apiVersion": "fleet/v1alpha1",
        "kind": "Task",
        "metadata": {"name": name, "uid": uid, "generation": 1, "creationTimestamp": T,
                     "labels": {"crontask.fleet.dev/uid": "u1"}},
        "spec": {"harness": "claude-code", "brief": "Review.", "workspace": []},
        "status": {"phase": phase}
    }))
    .expect("a Task")
}

/// The retires a plan asks for, as (uid, running).
fn retires(plan: &fleet_api::plan::Plan) -> Vec<(String, bool)> {
    plan.effects
        .iter()
        .filter_map(|e| match &e.action {
            fleet_api::plan::Action::RetireScheduled { uid, running, .. } => {
                Some((uid.clone(), *running))
            }
            _ => None,
        })
        .collect()
}

fn plan(obj: &Object, tasks: Vec<&Object>, now: u64) -> fleet_api::plan::Plan {
    CronTaskController
        .reconcile(&CronTaskInput {
            object: obj,
            tasks,
            now,
        })
        .expect("a plan")
}

/// A history delete still waiting for its receipt, on a Task a person
/// reopened.
fn pending_history_delete() -> Value {
    json!({
        "observedGeneration": 1,
        "lastConsidered": T,
        "notBefore": T + MINUTE,
        "effects": {"retire/u-old": {
            "intent": {"action": "retireScheduled", "at": T, "generation": 1},
            "attempts": 1,
            "task": "nightly-29911860"
        }}
    })
}

/// A newer Replace decision widens a stored history delete: the Task it
/// replaces is deleted though running, and asked for once.
#[test]
fn a_replace_decision_widens_a_pending_history_delete() {
    let obj = cron_task_with(
        json!({"concurrencyPolicy": "Replace"}),
        pending_history_delete(),
    );
    let reopened = task("nightly-29911860", "u-old", "Briefed");
    let plan = plan(&obj, vec![&reopened], T + 86_400_000);
    assert_eq!(retires(&plan), [("u-old".to_string(), true)]);
}

/// A retry narrows when the spec no longer says Replace: a delete decided
/// under Replace never removes a running Task once the policy changed.
#[test]
fn a_retried_replace_delete_narrows_when_the_policy_changed() {
    let mut status = pending_history_delete();
    status["effects"]["retire/u-old"]["running"] = json!(true);
    let obj = cron_task_with(json!({"concurrencyPolicy": "Allow"}), status);
    let running = task("nightly-29911860", "u-old", "Briefed");
    let plan = plan(&obj, vec![&running], T + MINUTE);
    assert_eq!(retires(&plan), [("u-old".to_string(), false)]);
}

/// A history delete the store refused (the Task was reopened) is decided
/// again once the Task is Concluded again and still past the limit.
#[test]
fn a_refused_history_delete_is_decided_again() {
    let mut status = pending_history_delete();
    status["effects"]["retire/u-old"]["receipt"] = json!({
        "action": "retireScheduled", "at": T,
        "outcome": {"known": {"type": "failed", "why": "not Concluded"}}
    });
    let obj = cron_task_with(json!({"historyLimit": 0}), status);
    let done = task("nightly-29911860", "u-old", "Concluded");
    let plan = plan(&obj, vec![&done], T + MINUTE);
    assert_eq!(retires(&plan), [("u-old".to_string(), false)]);
}

/// A first delete looks again within a minute, as a first create does.
#[test]
fn a_first_history_delete_is_looked_at_again_within_a_minute() {
    let obj = cron_task_with(
        json!({"historyLimit": 0}),
        json!({"observedGeneration": 1, "lastConsidered": T, "notBefore": T + MINUTE}),
    );
    let done = task("nightly-29911860", "u-old", "Concluded");
    let plan = plan(&obj, vec![&done], T + MINUTE);
    assert_eq!(retires(&plan), [("u-old".to_string(), false)]);
    let status = plan.status.expect("a status");
    assert_eq!(status["notBefore"], T + 2 * MINUTE, "{status:#}");
}

fn reconcile(obj: &Object, now: u64) -> Value {
    let plan = CronTaskController
        .reconcile(&CronTaskInput {
            object: obj,
            tasks: Vec::new(),
            now,
        })
        .expect("a plan");
    plan.status.unwrap_or_else(|| obj.status.clone())
}

fn attention(status: &Value) -> (String, String) {
    let c = status["conditions"]
        .as_array()
        .and_then(|cs| cs.iter().find(|c| c["type"] == "NeedsAttention"))
        .unwrap_or_else(|| panic!("no NeedsAttention in {status:#}"));
    (
        c["status"].as_str().unwrap().to_string(),
        c["reason"].as_str().unwrap().to_string(),
    )
}

/// The doubt about a create lives on the pending create: a retry whose
/// receipt never landed (the manager stopped again) keeps it in doubt,
/// rather than reading as resolved because the latest receipt is gone.
#[test]
fn a_create_stays_in_doubt_when_its_retry_has_no_receipt() {
    let first = cron_task(json!({
        "observedGeneration": 1,
        "lastConsidered": T,
        "notBefore": T,
        "effects": {"create/29913300": {
            "intent": {"action": "create", "at": T, "generation": 1},
            "receipt": {"action": "create", "at": T,
                        "outcome": {"unknown": {"why": "the store could not tell"}}},
            "attempts": 3,
            "unknowns": 2,
            "task": "nightly-29913300",
            "scheduled": T
        }}
    }));
    let planned = reconcile(&first, T + MINUTE);
    assert_eq!(attention(&planned), ("True".into(), "CreateUnknown".into()));

    // The manager wrote the intent, then stopped before the receipt.
    let second = cron_task(planned);
    let next = reconcile(&second, T + 2 * MINUTE);
    assert_eq!(
        attention(&next),
        ("True".into(), "CreateUnknown".into()),
        "still in doubt: {next:#}"
    );
}

/// The doubt grows across passes: three attempts that each read Unknown,
/// fed back as the manager would write them, raise `CreateUnknown`, though
/// each retry clears the latest receipt.
#[test]
fn three_unknown_attempts_raise_create_unknown() {
    let unknown = json!({"action": "create", "at": T,
                         "outcome": {"unknown": {"why": "the store could not tell"}}});
    let mut status = json!({
        "observedGeneration": 1,
        "lastConsidered": T,
        "notBefore": T,
        "effects": {"create/29913300": {
            "intent": {"action": "create", "at": T, "generation": 1},
            "receipt": unknown,
            "attempts": 1,
            "task": "nightly-29913300",
            "scheduled": T
        }}
    });
    for pass in 1..=3 {
        status = reconcile(&cron_task(status), T + pass * MINUTE);
        let record = &mut status["effects"]["create/29913300"];
        assert!(
            record["receipt"].is_null(),
            "pass {pass} retried: {record:#}"
        );
        if pass < 3 {
            assert_ne!(
                attention_of(&status),
                Some(("True".into(), "CreateUnknown".into())),
                "pass {pass}: too early"
            );
        }
        // The manager's receipt for the retry: Unknown again.
        status["effects"]["create/29913300"]["receipt"] = unknown.clone();
    }
    let status = reconcile(&cron_task(status), T + 4 * MINUTE);
    assert_eq!(attention(&status), ("True".into(), "CreateUnknown".into()));
}

fn attention_of(status: &Value) -> Option<(String, String)> {
    status["conditions"]
        .as_array()
        .and_then(|cs| cs.iter().find(|c| c["type"] == "NeedsAttention"))
        .map(|c| {
            (
                c["status"].as_str().unwrap().to_string(),
                c["reason"].as_str().unwrap().to_string(),
            )
        })
}

/// The first create looks again soon, as a retry does: should it never land
/// (the manager stops, or the store answers Unknown or no), the next look
/// is within a minute, not at the next scheduled time.
#[test]
fn a_first_create_is_looked_at_again_within_a_minute() {
    let obj = cron_task(json!({"observedGeneration": 1, "notBefore": T}));
    let planned = reconcile(&obj, T);
    assert!(
        planned["effects"]["create/29913300"].is_object(),
        "{planned:#}"
    );
    assert_eq!(planned["notBefore"], T + MINUTE, "{planned:#}");
}

/// A history delete keeps the permission it was decided with: a Task that a
/// person reopened since is never deleted as if Replace had asked.
#[test]
fn a_history_delete_retried_keeps_its_concluded_only_permission() {
    let obj = cron_task(json!({
        "observedGeneration": 1,
        "lastConsidered": T,
        "notBefore": T + MINUTE,
        "effects": {"retire/u-old": {
            "intent": {"action": "retireScheduled", "at": T, "generation": 1},
            "attempts": 1,
            "task": "nightly-29911860"
        }}
    }));
    let reopened: Object = serde_json::from_value(json!({
        "apiVersion": "fleet/v1alpha1",
        "kind": "Task",
        "metadata": {"name": "nightly-29911860", "uid": "u-old", "generation": 2,
                     "labels": {"crontask.fleet.dev/uid": "u1"}},
        "spec": {"harness": "claude-code", "brief": "Review.", "workspace": []},
        "status": {"phase": "Briefed"}
    }))
    .expect("a Task");
    let plan = CronTaskController
        .reconcile(&CronTaskInput {
            object: &obj,
            tasks: vec![&reopened],
            now: T + MINUTE,
        })
        .expect("a plan");
    for effect in &plan.effects {
        if let fleet_api::plan::Action::RetireScheduled { running, .. } = &effect.action {
            assert!(
                !running,
                "a history delete never asks to delete a running Task"
            );
        }
    }
}

/// A create decided under Allow, retried after the policy became Forbid
/// while another Task of this CronTask runs, is given up: a retry obeys the
/// policy in force, as a fresh decision would.
#[test]
fn a_retried_create_obeys_forbid_in_force() {
    let obj = cron_task_with(
        json!({"concurrencyPolicy": "Forbid"}),
        json!({
            "observedGeneration": 1,
            "lastConsidered": T,
            "notBefore": T + MINUTE,
            "effects": {"create/29913300": {
                "intent": {"action": "create", "at": T, "generation": 1},
                "attempts": 1,
                "task": "nightly-29913300",
                "scheduled": T
            }}
        }),
    );
    let earlier = task("nightly-29911860", "u-old", "Briefed");
    let plan = plan(&obj, vec![&earlier], T + MINUTE);
    assert!(plan.creates.is_empty(), "{:#?}", plan.creates);
    let status = plan.status.expect("a status");
    assert_eq!(
        status["effects"]["create/29913300"]["receipt"]["outcome"]["known"]["type"], "failed",
        "{status:#}"
    );
}
