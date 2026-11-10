//! TaskController with a workspace: the runtime makes a Task's URL list
//! before the first start and releases it after the session. Each case is
//! one transition or one Unknown of the workspace's lifecycle.

use fleet_api::condition::ConditionStatus;
use fleet_api::plan::{Action, EffectOutcome, Observations, Plan, Probe, Receipt};
use fleet_api::task::{Phase, SessionRef, TaskStatus};
use fleet_api::workspace::WorkspaceRef;
use fleet_api::{Object, Observation};
use fleet_controllers::{MAX_ATTEMPTS, RETRY_DELAY_MS, TaskController, TaskInput};
use serde_json::json;

const NOW: u64 = 1_000_000;
const MATERIALIZE_1: &str = "task/t/u7/materialize/1";

fn task(status: Option<TaskStatus>) -> Object {
    let mut o: Object = serde_json::from_value(json!({
        "apiVersion": "fleet/v1alpha1",
        "kind": "Task",
        "metadata": {"name": "t", "uid": "u7", "generation": 1, "resourceVersion": 7, "finalizers": ["fleet.dev/release"]},
        "spec": {"harness": "claude-code", "brief": "do it", "dispatch": true,
                 "workspace": ["git+https://example.test/app.git#main", "git+https://example.test/docs.git"]}
    }))
    .unwrap();
    if let Some(s) = status {
        o.status = serde_json::to_value(s).unwrap();
    }
    o
}

fn deleting(mut o: Object) -> Object {
    o.metadata.deletion_timestamp = Some(NOW - 1);
    o
}

fn input(obj: &Object, observations: Observations, now: u64) -> TaskInput<'_> {
    TaskInput {
        task: obj,
        observations,
        now,
    }
}

fn run_at(obj: &Object, obs: Observations, now: u64) -> (Plan, TaskStatus) {
    let plan = TaskController.reconcile(&input(obj, obs, now)).unwrap();
    let st = match &plan.status {
        Some(v) => serde_json::from_value(v.clone()).unwrap(),
        None => serde_json::from_value(obj.status.clone()).unwrap_or_default(),
    };
    (plan, st)
}

fn run(obj: &Object, obs: Observations) -> (Plan, TaskStatus) {
    run_at(obj, obs, NOW)
}

fn probes(obj: &Object) -> Vec<Probe> {
    TaskController
        .probes(&input(obj, Observations::default(), NOW))
        .unwrap()
}

fn host_up() -> Observations {
    Observations {
        host: Some(Observation::Known(true)),
        ..Default::default()
    }
}

fn ws() -> WorkspaceRef {
    WorkspaceRef {
        key: "t.u7".into(),
        host: "local".into(),
        dirs: vec!["/w/t.u7/app".into(), "/w/t.u7/docs".into()],
    }
}

fn cond(st: &TaskStatus, t: &str) -> (ConditionStatus, String) {
    let c = st
        .conditions
        .iter()
        .find(|c| c.type_ == t)
        .unwrap_or_else(|| panic!("no {t} in {st:#?}"));
    (c.status, c.reason.clone())
}

/// The status after the first materialize was planned, with `outcome` as
/// its receipt (None: the manager stopped before the receipt).
fn materializing(outcome: Option<Observation<EffectOutcome>>) -> Object {
    let (_, mut st) = run(&task(None), host_up());
    if let Some(outcome) = outcome {
        st.receipts.insert(
            MATERIALIZE_1.into(),
            Receipt {
                action: "materialize".into(),
                at: NOW,
                outcome,
            },
        );
    }
    task(Some(st))
}

#[test]
fn a_url_task_has_its_workspace_made_before_it_spawns() {
    let (plan, st) = run(&task(None), host_up());
    assert_eq!(st.phase, Phase::Pending);
    assert_eq!(cond(&st, "WorkspaceReady").1, "Materializing");
    assert_eq!(plan.effects.len(), 1);
    assert_eq!(plan.effects[0].key, MATERIALIZE_1);
    assert!(
        st.intents.contains_key(MATERIALIZE_1),
        "intent before effect"
    );
    let Action::Materialize { host, key, urls } = &plan.effects[0].action else {
        panic!("{:?}", plan.effects[0].action);
    };
    assert_eq!((host.as_str(), key.as_str()), ("local", "t.u7"));
    assert_eq!(
        urls,
        &vec![
            "git+https://example.test/app.git#main".to_string(),
            "git+https://example.test/docs.git".to_string()
        ]
    );
}

#[test]
fn the_spawn_runs_in_the_workspace_the_runtime_made() {
    let obj = materializing(Some(Observation::Known(EffectOutcome::Materialized {
        workspace: Box::new(ws()),
        adopted: false,
    })));
    let (plan, st) = run(&obj, host_up());
    assert_eq!(st.workspace, Some(ws()));
    assert_eq!(
        cond(&st, "WorkspaceReady"),
        (ConditionStatus::True, "Materialized".into())
    );
    assert_eq!(st.phase, Phase::Spawning);
    let Action::Spawn(spec) = &plan.effects[0].action else {
        panic!("{:?}", plan.effects);
    };
    assert_eq!(spec.ready, Some(ws()));
    assert_eq!(spec.host, "local");
    // The agent starts in the first directory and spans the others.
    let argv = spec.launch_argv().unwrap();
    assert!(argv.iter().any(|a| a == "/w/t.u7/docs"), "{argv:?}");
}

#[test]
fn a_lost_answer_is_settled_under_the_same_key_never_by_a_new_attempt() {
    for outcome in [None, Some(Observation::unknown("lost"))] {
        let obj = materializing(outcome);
        assert!(probes(&obj).contains(&Probe::FindWorkspace {
            host: "local".into(),
            key: "t.u7".into(),
            urls: vec![
                "git+https://example.test/app.git#main".into(),
                "git+https://example.test/docs.git".into()
            ],
        }));
        // Unreadable: nothing concludes, nothing is sent.
        let (plan, st) = run(
            &obj,
            Observations {
                workspace: Some(Observation::unknown("cannot read")),
                ..host_up()
            },
        );
        assert!(plan.effects.is_empty());
        assert_eq!(
            cond(&st, "WorkspaceReady"),
            (ConditionStatus::Unknown, "WorkspaceUnreadable".into())
        );
        assert_eq!(st.materialize_attempts, 1);
        // Found: adopted, and the spawn follows.
        let (plan, st) = run(
            &obj,
            Observations {
                workspace: Some(Observation::Known(Some(ws()))),
                ..host_up()
            },
        );
        assert_eq!(cond(&st, "WorkspaceReady").1, "Adopted");
        assert!(matches!(plan.effects[0].action, Action::Spawn(_)));
        // Proven absent: one more attempt, after the wait.
        let (plan, st) = run(
            &obj,
            Observations {
                workspace: Some(Observation::Known(None)),
                ..host_up()
            },
        );
        assert!(plan.effects.is_empty());
        assert_eq!(cond(&st, "WorkspaceReady").1, "MaterializeRetry");
        assert_eq!(st.not_before, Some(NOW + RETRY_DELAY_MS));
        let (plan, st) = run_at(&task(Some(st)), host_up(), NOW + RETRY_DELAY_MS);
        assert_eq!(plan.effects[0].key, "task/t/u7/materialize/2");
        assert_eq!(st.materialize_attempts, 2);
    }
}

#[test]
fn refused_materializes_are_bounded_and_stall_without_a_spawn() {
    let mut obj = materializing(Some(Observation::Known(EffectOutcome::Failed {
        why: "no".into(),
    })));
    let mut now = NOW;
    for attempt in 2..=MAX_ATTEMPTS {
        let (_, st) = run_at(&obj, host_up(), now);
        now += RETRY_DELAY_MS;
        let (plan, mut st) = run_at(&task(Some(st)), host_up(), now);
        let key = format!("task/t/u7/materialize/{attempt}");
        assert_eq!(plan.effects[0].key, key);
        st.receipts.insert(
            key,
            Receipt {
                action: "materialize".into(),
                at: now,
                outcome: Observation::Known(EffectOutcome::Failed { why: "no".into() }),
            },
        );
        obj = task(Some(st));
    }
    let (plan, st) = run_at(&obj, host_up(), now);
    assert!(plan.effects.is_empty());
    assert_eq!(st.phase, Phase::Stalled);
    assert_eq!(
        cond(&st, "WorkspaceReady"),
        (ConditionStatus::False, "MaterializeFailed".into())
    );
    assert_eq!(st.spawn_attempts, 0);
}

#[test]
fn an_unsupported_url_gives_its_attempt_back_and_spawns_nothing() {
    let obj = materializing(Some(Observation::Known(EffectOutcome::Unsupported {
        url: "git+https://example.test/docs.git".into(),
    })));
    let (plan, st) = run(&obj, host_up());
    assert!(plan.effects.is_empty(), "{:?}", plan.effects);
    assert_eq!(st.phase, Phase::Pending);
    assert_eq!(st.materialize_attempts, 0);
    assert_eq!(
        cond(&st, "WorkspaceReady"),
        (ConditionStatus::False, "WorkspaceUnsupported".into())
    );
    assert_eq!(cond(&st, "NeedsAttention").1, "WorkspaceUnsupported");
    let (plan, st) = run_at(&task(Some(st)), host_up(), NOW + RETRY_DELAY_MS);
    assert_eq!(plan.effects[0].key, MATERIALIZE_1);
    assert_eq!(st.materialize_attempts, 1);
}

#[test]
fn an_empty_workspace_plans_no_workspace_call() {
    let mut empty = task(None);
    empty.spec["workspace"] = json!([]);
    let (plan, st) = run(&empty, host_up());
    let Action::Spawn(spec) = &plan.effects[0].action else {
        panic!("{:?}", plan.effects);
    };
    assert_eq!(spec.ready, None);
    assert!(st.workspace.is_none());
}

#[test]
fn a_file_url_is_made_through_the_runtime_too() {
    let mut file = task(None);
    file.spec["workspace"] = json!(["file:///w"]);
    let (plan, _) = run(&file, host_up());
    assert_eq!(plan.effects[0].key, MATERIALIZE_1);
}

/// A released Task whose workspace the runtime made.
fn released() -> TaskStatus {
    let mut st = TaskStatus {
        phase: Phase::Releasing,
        workspace: Some(ws()),
        session: Some(SessionRef {
            id: "s1".into(),
            host: "local".into(),
            key: "t.u7".into(),
        }),
        spawn_attempts: 1,
        release_attempts: 1,
        ..Default::default()
    };
    st.receipts.insert(
        "task/t/u7/release/1".into(),
        Receipt {
            action: "release".into(),
            at: NOW - 10,
            outcome: Observation::Known(EffectOutcome::Released),
        },
    );
    st
}

#[test]
fn the_workspace_is_cleaned_after_the_release_and_only_then_let_go() {
    let obj = deleting(task(Some(released())));
    let (plan, st) = run(&obj, Observations::default());
    assert_eq!(
        plan.remove_finalizer, None,
        "held until the workspace is clean"
    );
    assert_eq!(plan.effects[0].key, "task/t/u7/cleanWorkspace/1");
    assert_eq!(
        plan.effects[0].action,
        Action::CleanWorkspace { workspace: ws() }
    );

    let mut st2 = st.clone();
    st2.receipts.insert(
        "task/t/u7/cleanWorkspace/1".into(),
        Receipt {
            action: "cleanWorkspace".into(),
            at: NOW,
            outcome: Observation::Known(EffectOutcome::Deleted),
        },
    );
    let (plan, st2) = run(&deleting(task(Some(st2))), Observations::default());
    assert_eq!(plan.remove_finalizer.as_deref(), Some("fleet.dev/release"));
    assert!(plan.effects.is_empty());
    assert_eq!(st2.workspace, None);
    assert_eq!(cond(&st2, "WorkspaceReady").1, "Cleaned");

    // A failed clean is tried again after the wait, never let go.
    let mut st3 = st;
    st3.receipts.insert(
        "task/t/u7/cleanWorkspace/1".into(),
        Receipt {
            action: "cleanWorkspace".into(),
            at: NOW,
            outcome: Observation::Known(EffectOutcome::Failed { why: "busy".into() }),
        },
    );
    let (plan, st3) = run(&deleting(task(Some(st3))), Observations::default());
    assert!(plan.effects.is_empty() && plan.remove_finalizer.is_none());
    let (plan, _) = run_at(
        &deleting(task(Some(st3))),
        Observations::default(),
        NOW + RETRY_DELAY_MS,
    );
    assert_eq!(plan.effects[0].key, "task/t/u7/cleanWorkspace/2");
}

#[test]
fn a_task_deleted_mid_workspace_call_releases_the_key_before_letting_go() {
    let obj = deleting(materializing(None));
    // Deletion never makes a workspace to learn what to release.
    assert!(
        !probes(&obj)
            .iter()
            .any(|p| matches!(p, Probe::FindWorkspace { .. })),
        "{:?}",
        probes(&obj)
    );
    let (plan, _) = run(&obj, Observations::default());
    assert!(plan.remove_finalizer.is_none());
    assert_eq!(
        plan.effects[0].action,
        Action::CleanWorkspace {
            workspace: WorkspaceRef {
                key: "t.u7".into(),
                host: "local".into(),
                dirs: Vec::new(),
            }
        }
    );
}
