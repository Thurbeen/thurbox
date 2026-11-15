//! CronTaskController decisions that the end-to-end tests cannot stage: a
//! pass whose plan the manager never finished, fed back as the next input.

use fleet_api::meta::Object;
use fleet_controllers::{CronTaskController, CronTaskInput};
use serde_json::{Value, json};

/// 2026-11-16T03:00:00Z.
const T: u64 = 1_794_798_000_000;
const MINUTE: u64 = 60_000;

fn cron_task(status: Value) -> Object {
    serde_json::from_value(json!({
        "apiVersion": "fleet/v1alpha1",
        "kind": "CronTask",
        "metadata": {"name": "nightly", "uid": "u1", "generation": 1,
                     "resourceVersion": 1, "creationTimestamp": T - MINUTE},
        "spec": {
            "schedule": "0 3 * * *",
            "template": {"spec": {"harness": "claude-code", "brief": "Review.", "workspace": []}}
        },
        "status": status
    }))
    .expect("a CronTask")
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
