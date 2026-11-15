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
