//! TaskController as a pure function: a Task and what the probes saw go in;
//! a plan comes out. Each case is one transition or one Unknown of the
//! Task's lifecycle.

use fleet_api::condition::ConditionStatus;
use fleet_api::plan::{Action, EffectOutcome, Observations, Plan, Probe, Receipt};
use fleet_api::runtime::{Activity, TrustGap, WorkspaceTrust};
use fleet_api::task::{Phase, SessionRef, SessionState, TaskStatus};
use fleet_api::{Object, Observation};
use fleet_controllers::{
    MAX_ATTEMPTS, RETRY_DELAY_MS, SESSION_RECHECK_MS, TaskController, TaskInput,
};
use serde_json::json;

const NOW: u64 = 1_000_000;

fn task(dispatch: bool, status: Option<TaskStatus>) -> Object {
    let mut o: Object = serde_json::from_value(json!({
        "apiVersion": "fleet/v1alpha1",
        "kind": "Task",
        "metadata": {"name": "t", "uid": "u7", "generation": 1, "resourceVersion": 7, "finalizers": ["fleet.dev/release"]},
        "spec": {"harness": "claude-code", "brief": "do it", "workspace": {"dir": "w"}, "dispatch": dispatch}
    }))
    .unwrap();
    if let Some(s) = status {
        o.status = serde_json::to_value(s).unwrap();
    }
    o
}

fn session() -> SessionRef {
    SessionRef {
        id: "s1".into(),
        host: "local".into(),
        key: "t.u7".into(),
    }
}

fn run_at(obj: &Object, obs: Observations, now: u64) -> (Plan, TaskStatus) {
    let input = TaskInput {
        task: obj,
        observations: obs,
        now,
        materialize: false,
    };
    let plan = TaskController.reconcile(&input).unwrap();
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
    let input = TaskInput {
        task: obj,
        observations: Observations::default(),
        now: NOW,
        materialize: false,
    };
    TaskController.probes(&input).unwrap()
}

/// What the runtime answers for a session whose agent trusts its workspace.
fn trusted() -> Option<Observation<WorkspaceTrust>> {
    Some(Observation::Known(WorkspaceTrust::Trusted {
        how: "test".into(),
    }))
}

fn cond<'a>(st: &'a TaskStatus, t: &str) -> &'a fleet_api::condition::Condition {
    st.conditions
        .iter()
        .find(|c| c.type_ == t)
        .unwrap_or_else(|| panic!("no {t} in {st:#?}"))
}

fn receipt(action: &str, outcome: Observation<EffectOutcome>) -> Receipt {
    Receipt {
        action: action.into(),
        at: NOW - 10,
        outcome,
    }
}

#[test]
fn an_undispatched_task_asks_nothing_and_does_nothing() {
    let obj = task(false, None);
    assert!(probes(&obj).is_empty());
    let (plan, st) = run(&obj, Observations::default());
    assert!(plan.effects.is_empty());
    assert_eq!(st.phase, Phase::Pending);
    assert_eq!(cond(&st, "Spawned").reason, "AwaitingDispatch");
}

#[test]
fn a_dispatched_task_records_the_spawn_intent_with_its_effect() {
    let obj = task(true, None);
    assert_eq!(
        probes(&obj),
        vec![Probe::Host {
            host: "local".into()
        }]
    );
    let obs = Observations {
        host: Some(Observation::Known(true)),
        ..Default::default()
    };
    let (plan, st) = run(&obj, obs);
    assert_eq!(st.phase, Phase::Spawning);
    assert_eq!(plan.effects.len(), 1);
    let e = &plan.effects[0];
    assert_eq!(e.key, "task/t/u7/spawn/1");
    assert!(
        st.intents.contains_key(&e.key),
        "the intent is in the status written before the effect"
    );
    match &e.action {
        Action::Spawn(s) => assert_eq!(
            (s.key.as_str(), s.host.as_str(), s.harness.as_str()),
            ("t.u7", "local", "claude-code")
        ),
        other => panic!("{other:?}"),
    }
    assert_eq!(st.observed_generation, 1);
}

#[test]
fn an_unknown_host_never_spawns() {
    let obs = Observations {
        host: Some(Observation::unknown("timeout")),
        ..Default::default()
    };
    let (plan, st) = run(&task(true, None), obs);
    assert!(plan.effects.is_empty());
    assert_eq!(
        cond(&st, "HostReachable").status,
        fleet_api::condition::ConditionStatus::Unknown
    );
}

fn spawning(receipt_outcome: Option<Observation<EffectOutcome>>) -> Object {
    let mut st = TaskStatus {
        phase: Phase::Spawning,
        spawn_attempts: 1,
        ..Default::default()
    };
    if let Some(o) = receipt_outcome {
        st.receipts
            .insert("task/t/u7/spawn/1".into(), receipt("spawn", o));
    }
    task(true, Some(st))
}

#[test]
fn an_intent_without_a_receipt_is_decided_by_the_session_key() {
    let obj = spawning(None);
    assert_eq!(
        probes(&obj),
        vec![Probe::Find {
            host: "local".into(),
            key: "t.u7".into()
        }]
    );

    let found = Observations {
        find: Some(Observation::Known(Some(session()))),
        ..Default::default()
    };
    let (plan, st) = run(&obj, found);
    assert!(plan.effects.is_empty(), "adopted, not respawned");
    assert_eq!(st.phase, Phase::Spawned);
    assert_eq!(cond(&st, "Spawned").reason, "Adopted");

    let unknown = Observations {
        find: Some(Observation::unknown("list unreadable")),
        ..Default::default()
    };
    let (plan, st) = run(&obj, unknown);
    assert!(plan.effects.is_empty(), "Unknown is not absence");
    assert_eq!(st.phase, Phase::Spawning);

    let absent = Observations {
        find: Some(Observation::Known(None)),
        ..Default::default()
    };
    let (plan, st) = run(&obj, absent);
    assert!(plan.effects.is_empty());
    assert_eq!(st.phase, Phase::Pending, "tried again on the next pass");
}

#[test]
fn spawn_attempts_are_bounded() {
    let mut st = TaskStatus {
        phase: Phase::Spawning,
        spawn_attempts: MAX_ATTEMPTS,
        ..Default::default()
    };
    st.receipts.insert(
        format!("task/t/u7/spawn/{MAX_ATTEMPTS}"),
        receipt(
            "spawn",
            Observation::Known(EffectOutcome::Failed { why: "no".into() }),
        ),
    );
    let (plan, st) = run(&task(true, Some(st)), Observations::default());
    assert!(plan.effects.is_empty());
    assert_eq!(st.phase, Phase::Stalled);
    assert_eq!(cond(&st, "NeedsAttention").reason, "Stalled");
}

/// One spawn as the manager performs it: the planned status, then the
/// effect's receipt under the planned key, then the next pass.
fn spawn_once(obj: &Object, outcome: EffectOutcome, now: u64) -> (Object, TaskStatus) {
    let host = Observations {
        host: Some(Observation::Known(true)),
        ..Default::default()
    };
    let (plan, mut st) = run_at(obj, host, now);
    assert_eq!(plan.effects.len(), 1, "{st:#?}");
    st.receipts.insert(
        plan.effects[0].key.clone(),
        receipt("spawn", Observation::Known(outcome)),
    );
    let (_, st) = run_at(&task(true, Some(st)), Observations::default(), now);
    (task(true, Some(st.clone())), st)
}

#[test]
fn an_unmapped_repository_spends_no_attempt_and_never_stalls_the_task() {
    let unmapped = || EffectOutcome::Unmapped {
        url: "https://example.invalid/app".into(),
    };
    let mut obj = task(true, None);
    let mut now = NOW;
    for _ in 0..MAX_ATTEMPTS + 2 {
        let (next, st) = spawn_once(&obj, unmapped(), now);
        assert_eq!(st.phase, Phase::Pending, "{st:#?}");
        assert_eq!(st.spawn_attempts, 0);
        assert_eq!(
            st.receipts.len(),
            1,
            "the given-back attempt reuses its key"
        );
        assert_eq!(cond(&st, "Spawned").reason, "RepositoryUnmapped");
        assert_eq!(cond(&st, "NeedsAttention").reason, "RepositoryUnmapped");
        assert_eq!(st.not_before, Some(now + RETRY_DELAY_MS));
        let (plan, _) = run_at(&next, Observations::default(), now + 1);
        assert!(plan.effects.is_empty(), "the retry waits");
        obj = next;
        now += RETRY_DELAY_MS;
    }

    // Genuine runtime failures keep their limit once the mapping is there.
    for attempt in 1..=MAX_ATTEMPTS {
        let (next, st) = spawn_once(&obj, EffectOutcome::Failed { why: "no".into() }, now);
        assert_eq!(st.spawn_attempts, attempt);
        assert_eq!(
            cond(&st, "NeedsAttention").status == ConditionStatus::True,
            attempt == MAX_ATTEMPTS,
            "{st:#?}"
        );
        obj = next;
        now += RETRY_DELAY_MS;
    }
    let st: TaskStatus = serde_json::from_value(obj.status.clone()).unwrap();
    assert_eq!(st.phase, Phase::Stalled);
    assert_eq!(cond(&st, "Stalled").reason, "SpawnAttemptsExhausted");
}

#[test]
fn an_untrusted_harness_waits_like_an_unmapped_repository() {
    let trust = || EffectOutcome::TrustUnmapped {
        harness: "codex-coder".into(),
    };
    let mut obj = task(true, None);
    let mut now = NOW;
    for _ in 0..MAX_ATTEMPTS + 2 {
        let (next, st) = spawn_once(&obj, trust(), now);
        assert_eq!(st.phase, Phase::Pending, "{st:#?}");
        assert_eq!(st.spawn_attempts, 0);
        assert_eq!(cond(&st, "Spawned").reason, "TrustUnmapped");
        let attention = cond(&st, "NeedsAttention");
        assert_eq!(attention.reason, "TrustUnmapped");
        assert!(
            attention
                .message
                .contains("runtime.workspaceTrust.codex-coder"),
            "{attention:#?}"
        );
        obj = next;
        now += RETRY_DELAY_MS;
    }
}

#[test]
fn a_mapping_added_while_waiting_spawns_and_resolves_the_attention() {
    for gap in [
        EffectOutcome::Unmapped {
            url: "https://example.invalid/app".into(),
        },
        EffectOutcome::TrustUnmapped {
            harness: "codex-coder".into(),
        },
    ] {
        let (obj, _) = spawn_once(&task(true, None), gap, NOW);
        let later = NOW + RETRY_DELAY_MS;
        let spawned = EffectOutcome::Spawned {
            session: session(),
            adopted: false,
        };
        let (_, st) = spawn_once(&obj, spawned, later);
        assert_eq!(st.phase, Phase::Spawned);
        assert_eq!(st.spawn_attempts, 1);
        assert_eq!(cond(&st, "NeedsAttention").reason, "Resolved");
    }
}

fn spawned(extra: impl FnOnce(&mut TaskStatus)) -> Object {
    let mut st = TaskStatus {
        phase: Phase::Spawned,
        spawn_attempts: 1,
        session: Some(session()),
        ..Default::default()
    };
    extra(&mut st);
    task(true, Some(st))
}

#[test]
fn the_brief_waits_for_a_ready_session() {
    let obj = spawned(|_| {});
    let live = Some(Observation::Known(Some(session())));
    let (plan, _) = run(
        &obj,
        Observations {
            find: live.clone(),
            ready: Some(Observation::Known(false)),
            ..Default::default()
        },
    );
    assert!(
        plan.effects.is_empty(),
        "never type into an unproven session (P7)"
    );
    let (plan, _) = run(
        &obj,
        Observations {
            find: live.clone(),
            ready: Some(Observation::unknown("capture failed")),
            ..Default::default()
        },
    );
    assert!(plan.effects.is_empty());
    let (plan, st) = run(
        &obj,
        Observations {
            find: live,
            ready: Some(Observation::Known(true)),
            workspace_trust: trusted(),
            ..Default::default()
        },
    );
    assert_eq!(plan.effects.len(), 1);
    assert_eq!(plan.effects[0].key, "task/t/u7/brief");
    assert_eq!(st.brief_sends, 1);
}

#[test]
fn a_brief_without_a_known_receipt_asks_the_session_and_never_resends_blind() {
    let obj = spawned(|st| {
        st.brief_sends = 1;
        st.intents.insert(
            "task/t/u7/brief".into(),
            fleet_api::plan::Intent {
                action: "brief".into(),
                at: NOW - 5,
                generation: 1,
            },
        );
    });
    assert!(probes(&obj).contains(&Probe::BriefDelivered {
        session: session(),
        key: "task/t/u7/brief".into()
    }));
    let live = Some(Observation::Known(Some(session())));

    let (plan, st) = run(
        &obj,
        Observations {
            find: live.clone(),
            brief_delivered: Some(Observation::Known(true)),
            ..Default::default()
        },
    );
    assert!(plan.effects.is_empty());
    assert_eq!(st.phase, Phase::Briefed);
    assert!(st.brief.as_ref().unwrap().recovered);

    let mut last_probe = obj.clone();
    last_probe.status["briefProbeFailures"] = serde_json::json!(MAX_ATTEMPTS - 1);
    let (plan, st) = run(
        &last_probe,
        Observations {
            find: live.clone(),
            brief_delivered: Some(Observation::unknown("?")),
            ..Default::default()
        },
    );
    assert!(
        plan.effects.is_empty(),
        "never resent on an unknown receipt"
    );
    assert_eq!(st.phase, Phase::Spawned, "a read never stalls the Task");
    assert_eq!(cond(&st, "NeedsAttention").reason, "BriefUnconfirmed");
    assert!(st.not_before.is_some(), "the next read waits");

    let (plan, st) = run(
        &obj,
        Observations {
            find: live,
            ready: Some(Observation::Known(true)),
            workspace_trust: trusted(),
            brief_delivered: Some(Observation::Known(false)),
            ..Default::default()
        },
    );
    assert_eq!(
        plan.effects.len(),
        1,
        "Known not delivered: send under the same key"
    );
    assert_eq!(plan.effects[0].key, "task/t/u7/brief");
    assert_eq!(st.brief_sends, 2);
}

#[test]
fn a_result_concludes_and_a_vanished_session_is_worker_gone() {
    let briefed = |extra: fn(&mut TaskStatus)| {
        spawned(|st| {
            st.phase = Phase::Briefed;
            extra(st);
        })
    };
    let (_, st) = run(
        &briefed(|st| {
            st.result = Some(
                serde_json::from_value(json!({"outcome": "shipped", "generation": 1})).unwrap(),
            );
        }),
        Observations::default(),
    );
    assert_eq!(st.phase, Phase::Concluded);

    let gone = Observations {
        find: Some(Observation::Known(None)),
        ..Default::default()
    };
    let (_, st) = run(&briefed(|_| {}), gone);
    assert_eq!(cond(&st, "WorkerGone").reason, "SessionGone");
    assert_eq!(cond(&st, "NeedsAttention").reason, "WorkerGone");
    assert_eq!(
        st.phase,
        Phase::Briefed,
        "Q13: never abandoned by the controller"
    );

    let unreadable = Observations {
        find: Some(Observation::unknown("ssh down")),
        ..Default::default()
    };
    let (_, st) = run(&briefed(|_| {}), unreadable);
    assert!(
        st.conditions.iter().all(|c| c.type_ != "WorkerGone"),
        "an unreadable list is never read as gone"
    );
}

fn deleting(st: TaskStatus) -> Object {
    let mut o = task(true, Some(st));
    o.metadata.deletion_timestamp = Some(NOW - 1);
    o
}

#[test]
fn release_runs_only_for_a_session_proven_at_rest_or_gone() {
    let obj = deleting(TaskStatus {
        phase: Phase::Concluded,
        session: Some(session()),
        ..Default::default()
    });
    let live = Some(Observation::Known(Some(session())));
    let (plan, st) = run(
        &obj,
        Observations {
            find: live.clone(),
            at_rest: Some(Observation::Known(false)),
            ..Default::default()
        },
    );
    assert!(plan.effects.is_empty());
    assert_eq!(st.phase, Phase::Releasing);
    let (plan, _) = run(
        &obj,
        Observations {
            find: live.clone(),
            at_rest: Some(Observation::unknown("?")),
            ..Default::default()
        },
    );
    assert!(plan.effects.is_empty());
    let (plan, _) = run(
        &obj,
        Observations {
            find: live,
            at_rest: Some(Observation::Known(true)),
            ..Default::default()
        },
    );
    assert!(matches!(plan.effects[0].action, Action::Release { .. }));
    let (plan, _) = run(
        &obj,
        Observations {
            find: Some(Observation::Known(None)),
            ..Default::default()
        },
    );
    assert_eq!(
        plan.effects.len(),
        1,
        "a gone session is released without proof of rest"
    );
}

#[test]
fn a_released_task_drops_its_finalizer_and_failed_releases_are_bounded() {
    let mut st = TaskStatus {
        phase: Phase::Releasing,
        session: Some(session()),
        release_attempts: 1,
        ..Default::default()
    };
    st.receipts.insert(
        "task/t/u7/release/1".into(),
        receipt("release", Observation::Known(EffectOutcome::Released)),
    );
    let (plan, _) = run(&deleting(st), Observations::default());
    assert_eq!(plan.remove_finalizer.as_deref(), Some("fleet.dev/release"));

    let mut st = TaskStatus {
        phase: Phase::Releasing,
        session: Some(session()),
        release_attempts: 3,
        ..Default::default()
    };
    st.receipts.insert(
        "task/t/u7/release/3".into(),
        receipt(
            "release",
            Observation::Known(EffectOutcome::Failed { why: "hook".into() }),
        ),
    );
    let (plan, st) = run(&deleting(st), Observations::default());
    assert!(plan.effects.is_empty() && plan.remove_finalizer.is_none());
    assert_eq!(cond(&st, "Released").reason, "ReleaseFailed");
    assert_eq!(cond(&st, "NeedsAttention").reason, "ReleaseFailed");
}

#[test]
fn a_task_that_never_spawned_releases_nothing() {
    let (plan, st) = run(&deleting(TaskStatus::default()), Observations::default());
    assert!(plan.effects.is_empty());
    assert_eq!(plan.remove_finalizer.as_deref(), Some("fleet.dev/release"));
    assert_eq!(cond(&st, "Released").reason, "NothingToRelease");
}

#[test]
fn reconciling_a_settled_task_writes_nothing() {
    let obs = Observations {
        host: Some(Observation::Known(true)),
        ..Default::default()
    };
    let (plan, st) = run(&task(true, None), obs.clone());
    let mut obj = task(true, None);
    obj.status = serde_json::to_value(&st).unwrap();
    obj.status["phase"] = json!("Concluded");
    let (again, _) = run(&obj, obs);
    assert!(plan.status.is_some());
    assert!(again.effects.is_empty());
}

#[test]
fn an_interrupted_release_is_retried_and_still_ends() {
    let intent = |at| fleet_api::plan::Intent {
        action: "release".into(),
        at,
        generation: 1,
    };
    let gone = || Observations {
        find: Some(Observation::Known(None)),
        ..Default::default()
    };
    let mut st = TaskStatus {
        phase: Phase::Releasing,
        session: Some(session()),
        release_attempts: 1,
        ..Default::default()
    };
    st.intents
        .insert("task/t/u7/release/1".into(), intent(NOW - RETRY_DELAY_MS));
    let (plan, _) = run(&deleting(st), gone());
    assert_eq!(
        plan.effects.len(),
        1,
        "an intent with no receipt is tried again"
    );

    let mut st = TaskStatus {
        phase: Phase::Releasing,
        session: Some(session()),
        release_attempts: MAX_ATTEMPTS,
        ..Default::default()
    };
    st.intents
        .insert(format!("task/t/u7/release/{MAX_ATTEMPTS}"), intent(NOW));
    let (plan, st) = run(&deleting(st), gone());
    assert!(plan.effects.is_empty());
    assert_eq!(
        cond(&st, "Released").reason,
        "ReleaseFailed",
        "the bound holds without a receipt too"
    );
}

#[test]
fn a_task_waiting_on_its_host_reaches_a_fixed_point() {
    // A pass that sees the same world as the last writes nothing, so the
    // manager goes idle instead of rewriting conditions every pass.
    let unreachable = || Observations {
        host: Some(Observation::Known(false)),
        ..Default::default()
    };
    let (first, st) = run_at(&task(true, None), unreachable(), NOW);
    assert_eq!(cond(&st, "HostReachable").reason, "Unreachable");
    assert!(first.effects.is_empty());
    let (again, _) = run_at(&task(true, Some(st)), unreachable(), NOW + 1_000);
    assert_eq!(again.status, None, "nothing changed, so nothing is written");
}

#[test]
fn a_session_seen_again_clears_worker_gone() {
    let gone = Observations {
        find: Some(Observation::Known(None)),
        ..Default::default()
    };
    let (_, st) = run(&spawned(|st| st.phase = Phase::Briefed), gone);
    assert_eq!(cond(&st, "WorkerGone").status, ConditionStatus::True);
    let back = Observations {
        find: Some(Observation::Known(Some(session()))),
        ..Default::default()
    };
    let (_, st) = run(&task(true, Some(st)), back);
    assert_eq!(cond(&st, "WorkerGone").status, ConditionStatus::False);
    assert_eq!(cond(&st, "NeedsAttention").status, ConditionStatus::False);
}

// --- The inbox: delivered as the brief is, under each entry's key. --------

fn entry(key: &str) -> fleet_api::inbox::InboxEntry {
    fleet_api::inbox::InboxEntry {
        key: key.into(),
        from: "cli/someone".into(),
        text: format!("message {key}"),
        about: None,
        answers: Vec::new(),
        amends: None,
        queued_at: NOW - 100,
    }
}

fn intent(at: u64) -> fleet_api::plan::Intent {
    fleet_api::plan::Intent {
        action: "brief".into(),
        at,
        generation: 1,
    }
}

/// A Briefed Task with the given inbox and extra status.
fn briefed_with(inbox: &[&str], extra: impl FnOnce(&mut TaskStatus)) -> Object {
    spawned(|st| {
        st.phase = Phase::Briefed;
        st.brief_sends = 1;
        st.inbox = inbox.iter().map(|k| entry(k)).collect();
        extra(st);
    })
}

fn live() -> Option<Observation<Option<SessionRef>>> {
    Some(Observation::Known(Some(session())))
}

#[test]
fn the_oldest_inbox_entry_is_sent_to_a_ready_session_under_its_key() {
    let obj = briefed_with(&["task/t/u7/send/1", "task/t/u7/send/2"], |_| {});
    assert_eq!(
        probes(&obj),
        vec![
            Probe::Find {
                host: "local".into(),
                key: "t.u7".into()
            },
            Probe::Lost { session: session() },
            Probe::Paused { session: session() },
            Probe::Ready { session: session() }
        ]
    );
    let (plan, st) = run(
        &obj,
        Observations {
            find: live(),
            ready: Some(Observation::Known(true)),
            workspace_trust: trusted(),
            ..Default::default()
        },
    );
    assert_eq!(plan.effects.len(), 1, "one entry per pass");
    assert_eq!(plan.effects[0].key, "task/t/u7/send/1");
    match &plan.effects[0].action {
        Action::Brief { text, .. } => assert_eq!(text, "message task/t/u7/send/1"),
        other => panic!("{other:?}"),
    }
    assert!(st.intents.contains_key("task/t/u7/send/1"));
    assert_eq!(st.delivery_attempts["task/t/u7/send/1"].sends, 1);
    assert_eq!(st.phase, Phase::Briefed, "delivery changes no phase");
    assert!(plan.delivery.is_none());
}

#[test]
fn an_inbox_entry_waits_for_a_ready_session() {
    let obj = briefed_with(&["task/t/u7/send/1"], |_| {});
    for ready in [
        Observation::Known(false),
        Observation::unknown("capture failed"),
    ] {
        let (plan, st) = run(
            &obj,
            Observations {
                find: live(),
                ready: Some(ready),
                ..Default::default()
            },
        );
        assert!(plan.effects.is_empty());
        assert!(st.delivery_attempts.is_empty());
        assert_eq!(st.phase, Phase::Briefed);
    }
}

#[test]
fn a_delivered_receipt_moves_the_entry_into_deliveries_in_one_write() {
    let obj = briefed_with(&["task/t/u7/send/1", "task/t/u7/send/2"], |st| {
        st.intents
            .insert("task/t/u7/send/1".into(), intent(NOW - 5));
        st.receipts.insert(
            "task/t/u7/send/1".into(),
            receipt(
                "brief",
                Observation::Known(EffectOutcome::Delivered {
                    already: false,
                    via: None,
                    may_hold: false,
                }),
            ),
        );
        st.delivery_attempts.insert(
            "task/t/u7/send/1".into(),
            fleet_api::inbox::DeliveryAttempt {
                sends: 1,
                ..Default::default()
            },
        );
    });
    let (plan, st) = run(
        &obj,
        Observations {
            find: live(),
            ready: Some(Observation::Known(true)),
            workspace_trust: trusted(),
            ..Default::default()
        },
    );
    let delivery = plan.delivery.expect("a delivery to record");
    assert_eq!(delivery.key, "task/t/u7/send/1");
    assert!(!delivery.recovered);
    assert!(plan.status.is_some(), "the receipt rides with the status");
    assert!(
        plan.effects.is_empty(),
        "recording a receipt is the pass's work; the next entry waits"
    );
    assert!(
        !st.intents.contains_key("task/t/u7/send/1")
            && !st.receipts.contains_key("task/t/u7/send/1")
            && !st.delivery_attempts.contains_key("task/t/u7/send/1"),
        "the effect records go with the entry: status stays bounded"
    );
    assert_eq!(
        st.inbox.len(),
        2,
        "the controller never edits the list itself"
    );
}

#[test]
fn an_inbox_send_without_a_known_receipt_is_read_back_never_resent_blind() {
    let obj = briefed_with(&["task/t/u7/send/1"], |st| {
        st.intents
            .insert("task/t/u7/send/1".into(), intent(NOW - 5));
        st.delivery_attempts.insert(
            "task/t/u7/send/1".into(),
            fleet_api::inbox::DeliveryAttempt {
                sends: 1,
                ..Default::default()
            },
        );
    });
    assert!(probes(&obj).contains(&Probe::BriefDelivered {
        session: session(),
        key: "task/t/u7/send/1".into()
    }));

    let (plan, _) = run(
        &obj,
        Observations {
            find: live(),
            ready: Some(Observation::Known(true)),
            workspace_trust: trusted(),
            brief_delivered: Some(Observation::Known(true)),
            ..Default::default()
        },
    );
    assert!(plan.effects.is_empty());
    assert!(plan.delivery.as_ref().is_some_and(|d| d.recovered));

    let (plan, st) = run(
        &obj,
        Observations {
            find: live(),
            ready: Some(Observation::Known(true)),
            workspace_trust: trusted(),
            brief_delivered: Some(Observation::Known(false)),
            ..Default::default()
        },
    );
    assert_eq!(plan.effects.len(), 1, "Known not delivered: sent again");
    assert_eq!(plan.effects[0].key, "task/t/u7/send/1");
    assert_eq!(st.delivery_attempts["task/t/u7/send/1"].sends, 2);

    let (plan, st) = run(
        &obj,
        Observations {
            find: live(),
            ready: Some(Observation::Known(true)),
            workspace_trust: trusted(),
            brief_delivered: Some(Observation::unknown("journal unreadable")),
            ..Default::default()
        },
    );
    assert!(plan.effects.is_empty(), "Unknown is never a resend");
    assert!(plan.delivery.is_none());
    assert_eq!(st.delivery_attempts["task/t/u7/send/1"].probe_failures, 1);
    assert!(st.not_before.is_some(), "read again after a wait");
}

#[test]
fn three_unreadable_delivery_probes_give_the_entry_up_unsent() {
    let obj = briefed_with(&["task/t/u7/send/1"], |st| {
        st.intents
            .insert("task/t/u7/send/1".into(), intent(NOW - 5));
        st.delivery_attempts.insert(
            "task/t/u7/send/1".into(),
            fleet_api::inbox::DeliveryAttempt {
                sends: 1,
                probe_failures: MAX_ATTEMPTS - 1,
                failed: false,
            },
        );
    });
    let (plan, st) = run(
        &obj,
        Observations {
            find: live(),
            ready: Some(Observation::Known(true)),
            workspace_trust: trusted(),
            brief_delivered: Some(Observation::unknown("?")),
            ..Default::default()
        },
    );
    assert!(plan.effects.is_empty());
    assert_eq!(cond(&st, "NeedsAttention").reason, "DeliveryUnconfirmed");
    assert_eq!(st.phase, Phase::Briefed, "a read never stalls the Task");
    let attempt = &st.delivery_attempts["task/t/u7/send/1"];
    assert!(attempt.failed, "given up, so later entries go past it");
    assert_eq!(attempt.sends, 1, "never sent again");
    assert_eq!(st.inbox.len(), 1, "the entry stays queued");
    assert!(
        !probes(&task(true, Some(st)))
            .iter()
            .any(|p| matches!(p, Probe::BriefDelivered { .. })),
        "a given-up entry's journal is not read again"
    );
}

#[test]
fn a_refused_inbox_send_waits_then_needs_fresh_readiness() {
    let obj = briefed_with(&["task/t/u7/send/1"], |st| {
        st.intents
            .insert("task/t/u7/send/1".into(), intent(NOW - 5));
        st.receipts.insert(
            "task/t/u7/send/1".into(),
            receipt(
                "brief",
                Observation::Known(EffectOutcome::Failed { why: "busy".into() }),
            ),
        );
        st.delivery_attempts.insert(
            "task/t/u7/send/1".into(),
            fleet_api::inbox::DeliveryAttempt {
                sends: 1,
                ..Default::default()
            },
        );
    });
    let obs = || Observations {
        find: live(),
        ready: Some(Observation::Known(true)),
        workspace_trust: trusted(),
        ..Default::default()
    };
    let (plan, st) = run(&obj, obs());
    assert!(plan.effects.is_empty(), "a refused send waits first");
    assert_eq!(st.not_before, Some(NOW - 10 + RETRY_DELAY_MS));
    let (plan, st) = run_at(&obj, obs(), NOW + RETRY_DELAY_MS);
    assert_eq!(plan.effects.len(), 1, "then it is sent again");
    assert_eq!(st.delivery_attempts["task/t/u7/send/1"].sends, 2);
    let (plan, _) = run_at(
        &obj,
        Observations {
            ready: Some(Observation::Known(false)),
            ..obs()
        },
        NOW + RETRY_DELAY_MS,
    );
    assert!(plan.effects.is_empty(), "a retry still needs readiness");
}

#[test]
fn an_entry_whose_sends_ran_out_raises_delivery_failed_and_later_entries_still_go() {
    let obj = briefed_with(&["task/t/u7/send/1", "task/t/u7/send/2"], |st| {
        st.intents
            .insert("task/t/u7/send/1".into(), intent(NOW - 5));
        st.receipts.insert(
            "task/t/u7/send/1".into(),
            receipt(
                "brief",
                Observation::Known(EffectOutcome::Failed { why: "no".into() }),
            ),
        );
        st.delivery_attempts.insert(
            "task/t/u7/send/1".into(),
            fleet_api::inbox::DeliveryAttempt {
                sends: MAX_ATTEMPTS,
                ..Default::default()
            },
        );
    });
    let obs = || Observations {
        find: live(),
        ready: Some(Observation::Known(true)),
        workspace_trust: trusted(),
        ..Default::default()
    };
    let (plan, st) = run(&obj, obs());
    assert!(plan.effects.is_empty(), "a fourth send is never made");
    let attention = cond(&st, "NeedsAttention");
    assert_eq!(attention.reason, "DeliveryFailed");
    assert!(
        attention.message.contains("task/t/u7/send/1"),
        "{attention:?}"
    );
    assert!(st.delivery_attempts["task/t/u7/send/1"].failed);
    assert_eq!(st.phase, Phase::Briefed, "no stall: the session is fine");
    assert_eq!(st.inbox.len(), 2, "the entry stays");

    let (plan, st) = run(&task(true, Some(st)), obs());
    assert_eq!(plan.effects.len(), 1, "the next entry goes out past it");
    assert_eq!(plan.effects[0].key, "task/t/u7/send/2");
    assert_eq!(cond(&st, "NeedsAttention").reason, "DeliveryFailed");
}

#[test]
fn a_concluded_task_delivers_its_inbox_and_concludes_nothing_else() {
    let concluded_checked = |inbox: &[&str], checked: Option<u64>| {
        briefed_with(inbox, |st| {
            st.phase = Phase::Concluded;
            st.result = Some(
                serde_json::from_value(json!({"outcome": "shipped", "generation": 1})).unwrap(),
            );
            st.session_checked_at = checked;
        })
    };
    let concluded = |inbox: &[&str]| concluded_checked(inbox, Some(NOW));
    assert!(
        probes(&concluded(&[])).is_empty(),
        "nothing queued, session just looked up: at rest"
    );
    // At rest, the session is looked up again once per recheck interval.
    for checked in [None, Some(NOW - SESSION_RECHECK_MS)] {
        assert_eq!(
            probes(&concluded_checked(&[], checked)),
            vec![Probe::Find {
                host: "local".into(),
                key: "t.u7".into()
            }],
            "{checked:?}"
        );
    }
    let obj = concluded(&["task/t/u7/send/1"]);
    assert_eq!(
        probes(&obj),
        vec![
            Probe::Find {
                host: "local".into(),
                key: "t.u7".into()
            },
            Probe::Lost { session: session() },
            Probe::Paused { session: session() },
            Probe::Ready { session: session() }
        ]
    );
    let (plan, st) = run(
        &obj,
        Observations {
            find: live(),
            ready: Some(Observation::Known(true)),
            workspace_trust: trusted(),
            ..Default::default()
        },
    );
    assert_eq!(plan.effects.len(), 1);
    assert_eq!(plan.effects[0].key, "task/t/u7/send/1");
    assert_eq!(st.phase, Phase::Concluded);
    assert_eq!(
        st.conditions.len(),
        0,
        "no condition is touched by a delivery"
    );
}

#[test]
fn a_concluded_task_whose_session_is_gone_is_worker_gone_never_respawned() {
    let obj = briefed_with(&["task/t/u7/send/1"], |st| {
        st.phase = Phase::Concluded;
    });
    let (plan, st) = run(
        &obj,
        Observations {
            find: Some(Observation::Known(None)),
            ready: Some(Observation::unknown("no such session")),
            ..Default::default()
        },
    );
    assert!(plan.effects.is_empty(), "no send, no spawn");
    assert_eq!(st.phase, Phase::Concluded);
    assert_eq!(cond(&st, "WorkerGone").reason, "SessionGone");
    assert_eq!(cond(&st, "NeedsAttention").reason, "WorkerGone");

    let (plan, st) = run(
        &obj,
        Observations {
            find: Some(Observation::unknown("list unreadable")),
            ready: Some(Observation::unknown("no such session")),
            ..Default::default()
        },
    );
    assert!(plan.effects.is_empty());
    assert!(
        st.conditions.iter().all(|c| c.type_ != "WorkerGone"),
        "an unreadable list is never read as gone"
    );
}

#[test]
fn a_stalled_or_deleting_task_delivers_nothing() {
    let stalled = briefed_with(&["task/t/u7/send/1"], |st| st.phase = Phase::Stalled);
    assert!(probes(&stalled).is_empty());
    let (plan, _) = run(
        &stalled,
        Observations {
            find: live(),
            ready: Some(Observation::Known(true)),
            workspace_trust: trusted(),
            ..Default::default()
        },
    );
    assert!(plan.effects.is_empty() && plan.delivery.is_none());

    let mut deleting = briefed_with(&["task/t/u7/send/1"], |_| {});
    deleting.metadata.deletion_timestamp = Some(NOW - 1);
    assert!(
        !probes(&deleting)
            .iter()
            .any(|p| matches!(p, Probe::Ready { .. })),
        "a deleting Task does not ask whether it could type"
    );
    let (plan, st) = run(
        &deleting,
        Observations {
            find: live(),
            ready: Some(Observation::Known(true)),
            workspace_trust: trusted(),
            at_rest: Some(Observation::Known(true)),
            ..Default::default()
        },
    );
    assert_eq!(st.phase, Phase::Releasing);
    assert!(matches!(plan.effects[0].action, Action::Release { .. }));
    assert!(plan.delivery.is_none());
    assert_eq!(
        st.inbox.len(),
        1,
        "deletion keeps precedence; the entry stays"
    );
}

#[test]
fn in_spawned_the_brief_goes_first_and_an_entry_follows_on_a_later_pass() {
    let obj = spawned(|st| st.inbox = vec![entry("task/t/u7/send/1")]);
    let obs = || Observations {
        find: live(),
        ready: Some(Observation::Known(true)),
        workspace_trust: trusted(),
        ..Default::default()
    };
    let (plan, st) = run(&obj, obs());
    assert_eq!(
        plan.effects.len(),
        1,
        "one send per pass, and it is the brief"
    );
    assert_eq!(plan.effects[0].key, "task/t/u7/brief");
    assert!(st.delivery_attempts.is_empty());
    // The brief is sent but unconfirmed: the inbox waits and asks nothing.
    let pending = task(true, Some(st));
    assert!(!probes(&pending).iter().any(
        |p| matches!(p, Probe::BriefDelivered { key, .. } if key.starts_with("task/t/u7/send/"))
    ));
    let (plan, _) = run(
        &pending,
        Observations {
            brief_delivered: Some(Observation::unknown("?")),
            ..obs()
        },
    );
    assert!(plan.effects.is_empty());
}

#[test]
fn more_than_the_bound_queued_is_attention_until_delivered_down() {
    let keys: Vec<String> = (0..=fleet_api::inbox::MAX_INBOX)
        .map(|i| format!("task/t/u7/send/{i}"))
        .collect();
    let refs: Vec<&str> = keys.iter().map(String::as_str).collect();
    let obj = briefed_with(&refs, |_| {});
    let (_, st) = run(
        &obj,
        Observations {
            find: live(),
            ready: Some(Observation::Known(false)),
            ..Default::default()
        },
    );
    assert_eq!(cond(&st, "NeedsAttention").reason, "InboxFull");
    // One delivered (the store removes it): back within the bound.
    let mut st = st;
    st.inbox.remove(0);
    let (_, st) = run(
        &task(true, Some(st)),
        Observations {
            find: live(),
            ready: Some(Observation::Known(false)),
            ..Default::default()
        },
    );
    assert_eq!(cond(&st, "NeedsAttention").status, ConditionStatus::False);
}

#[test]
fn entries_given_up_on_never_raise_inbox_full() {
    let keys: Vec<String> = (0..=fleet_api::inbox::MAX_INBOX)
        .map(|i| format!("task/t/u7/send/{i}"))
        .collect();
    let refs: Vec<&str> = keys.iter().map(String::as_str).collect();
    let obj = briefed_with(&refs, |st| {
        for k in &keys {
            st.delivery_attempts.insert(
                k.clone(),
                fleet_api::inbox::DeliveryAttempt {
                    failed: true,
                    ..Default::default()
                },
            );
        }
    });
    let (_, st) = run(
        &obj,
        Observations {
            find: live(),
            ready: Some(Observation::Known(true)),
            ..Default::default()
        },
    );
    assert!(
        st.conditions.iter().all(|c| c.reason != "InboxFull"),
        "{:#?}",
        st.conditions
    );
}

#[test]
fn the_brief_waits_for_the_agent_to_trust_its_workspace() {
    let obj = spawned(|_| {});
    assert!(probes(&obj).contains(&Probe::WorkspaceTrust {
        session: session(),
        workspace: fleet_api::task::Workspace::at("w"),
        ready: None,
    }));
    let obs = |trust| Observations {
        find: live(),
        ready: Some(Observation::Known(true)),
        workspace_trust: trust,
        ..Default::default()
    };

    // A known gap: no brief, a person is asked, and it is looked at again
    // after the wait rather than on every sweep.
    let (plan, st) = run(
        &obj,
        obs(Some(Observation::Known(WorkspaceTrust::Untrusted {
            gap: TrustGap::Unsupported,
            why: "trust it once".into(),
        }))),
    );
    assert!(plan.effects.is_empty());
    assert_eq!(cond(&st, "WorkspaceTrusted").status, ConditionStatus::False);
    assert_eq!(cond(&st, "WorkspaceTrusted").reason, "Unsupported");
    assert_eq!(cond(&st, "NeedsAttention").reason, "WorkspaceUntrusted");
    assert_eq!(st.not_before, Some(NOW + RETRY_DELAY_MS));
    assert_eq!(st.brief_sends, 0, "a held brief is not an attempt");

    // Unknown proves nothing: no brief, and no claim that a person must act.
    let (plan, st) = run(&obj, obs(Some(Observation::unknown("unreadable"))));
    assert!(plan.effects.is_empty());
    assert_eq!(
        cond(&st, "WorkspaceTrusted").status,
        ConditionStatus::Unknown
    );
    assert_eq!(st.not_before, None, "asked again on the next pass");
    assert!(st.conditions.iter().all(|c| c.type_ != "NeedsAttention"));

    // Not asked is not trusted.
    let (plan, _) = run(&obj, obs(None));
    assert!(plan.effects.is_empty());

    // Trust settles the attention it raised; the brief goes out.
    let (_, held) = run(
        &obj,
        obs(Some(Observation::Known(WorkspaceTrust::Untrusted {
            gap: TrustGap::Mismatch,
            why: "another session".into(),
        }))),
    );
    let later = task(true, Some(held));
    let (plan, st) = run_at(&later, obs(trusted()), NOW + RETRY_DELAY_MS);
    assert_eq!(plan.effects.len(), 1);
    assert_eq!(plan.effects[0].key, "task/t/u7/brief");
    assert_eq!(cond(&st, "WorkspaceTrusted").status, ConditionStatus::True);
    assert_eq!(cond(&st, "NeedsAttention").status, ConditionStatus::False);
}

#[test]
fn a_brief_held_for_trust_holds_the_inbox_with_it() {
    let obj = spawned(|st| st.inbox = vec![entry("task/t/u7/send/1")]);
    for trust in [
        None,
        Some(Observation::unknown("unreadable")),
        Some(Observation::Known(WorkspaceTrust::Untrusted {
            gap: TrustGap::Unsupported,
            why: "trust it once".into(),
        })),
    ] {
        let (plan, _) = run(
            &obj,
            Observations {
                find: live(),
                ready: Some(Observation::Known(true)),
                workspace_trust: trust.clone(),
                ..Default::default()
            },
        );
        assert!(
            plan.effects.is_empty(),
            "nothing reaches an untrusted session before its brief: {trust:?}"
        );
    }
}

/// A Briefed Task whose session the runtime still holds, as after a reboot.
fn lost_obs(lost: bool) -> Observations {
    Observations {
        find: live(),
        lost: Some(Observation::Known(lost)),
        ready: Some(Observation::Known(true)),
        ..Default::default()
    }
}

fn with_status(obj: &Object, st: &TaskStatus) -> Object {
    let mut o = obj.clone();
    o.status = serde_json::to_value(st).unwrap();
    o
}

#[test]
fn a_lost_session_is_revived_once_and_its_worker_resumes_its_brief() {
    let obj = briefed_with(&["task/t/u7/send/1"], |st| {
        st.brief = Some(fleet_api::task::BriefRecord {
            key: "task/t/u7/brief".into(),
            generation: 1,
            recovered: false,
        });
    });
    assert!(probes(&obj).contains(&Probe::Lost { session: session() }));
    let (plan, st) = run(&obj, lost_obs(true));
    assert_eq!(plan.effects.len(), 1, "{plan:#?}");
    assert_eq!(plan.effects[0].key, "task/t/u7/revive/1");
    assert_eq!(
        plan.effects[0].action,
        Action::Revive {
            session: session(),
            conversation: None
        }
    );
    assert_eq!(st.phase, Phase::Briefed, "never respawned");
    assert_eq!(cond(&st, "SessionLost").status, ConditionStatus::True);
    assert!(st.intents.contains_key("task/t/u7/revive/1"));

    // Back: the catch-up note goes before the inbox, under its own key.
    let mut st = st;
    st.receipts.insert(
        "task/t/u7/revive/1".into(),
        receipt("revive", Observation::Known(EffectOutcome::Revived)),
    );
    let (plan, st) = run(&with_status(&obj, &st), lost_obs(false));
    assert_eq!(cond(&st, "SessionLost").reason, "Revived");
    assert_eq!(st.catch_up.as_deref(), Some("task/t/u7/catchup/1"));
    assert_eq!(st.revive_attempts, 0);
    assert_eq!(plan.effects.len(), 1, "{plan:#?}");
    assert_eq!(plan.effects[0].key, "task/t/u7/catchup/1");
    let Action::Brief { text, .. } = &plan.effects[0].action else {
        panic!("{plan:#?}")
    };
    assert!(
        text.contains("restarted") && text.contains("generation 1"),
        "{text}"
    );
    assert!(text.ends_with("do it"), "the brief again: {text}");
    assert!(st.inbox.len() == 1, "the inbox waits for the note");

    // Delivered, and the agent's prompt hook reported it: the note is done
    // and the inbox goes next.
    let mut st = st;
    st.receipts.insert(
        "task/t/u7/catchup/1".into(),
        receipt(
            "brief",
            Observation::Known(EffectOutcome::Delivered {
                already: false,
                via: None,
                may_hold: false,
            }),
        ),
    );
    let mut obs = lost_obs(false);
    obs.signals = Some(Observation::Known(fleet_api::signal::Reported {
        prompts: [fleet_api::signal::prompt_digest(text)].into(),
        ..Default::default()
    }));
    let (plan, st) = run(&with_status(&obj, &st), obs);
    assert_eq!(st.catch_up, None);
    assert!(!st.receipts.contains_key("task/t/u7/catchup/1"));
    assert_eq!(plan.effects.len(), 1, "{plan:#?}");
    assert_eq!(plan.effects[0].key, "task/t/u7/send/1");
    assert_eq!(st.brief.as_ref().unwrap().generation, 1);
}

#[test]
fn a_held_task_is_not_revived_and_unknown_is_not_lost() {
    let obj = task(
        false,
        Some({
            let mut st: TaskStatus =
                serde_json::from_value(briefed_with(&[], |_| {}).status).unwrap();
            st.phase = Phase::Briefed;
            st
        }),
    );
    let (plan, st) = run(&obj, lost_obs(true));
    assert!(plan.effects.is_empty(), "{plan:#?}");
    assert_eq!(cond(&st, "SessionLost").status, ConditionStatus::True);

    let obj = briefed_with(&[], |_| {});
    let (plan, st) = run(
        &obj,
        Observations {
            find: live(),
            lost: Some(Observation::unknown("cannot tell")),
            ..Default::default()
        },
    );
    assert!(plan.effects.is_empty(), "{plan:#?}");
    assert!(st.conditions.iter().all(|c| c.type_ != "SessionLost"));
}

#[test]
fn revives_wait_between_attempts_and_ask_for_attention_but_go_on() {
    let mut obj = briefed_with(&[], |_| {});
    let mut now = NOW;
    for attempt in 1..=MAX_ATTEMPTS + 1 {
        let (plan, st) = run_at(&obj, lost_obs(true), now);
        assert_eq!(plan.effects.len(), 1, "attempt {attempt}: {plan:#?}");
        assert_eq!(plan.effects[0].key, format!("task/t/u7/revive/{attempt}"));
        let mut st = st;
        st.receipts.insert(
            plan.effects[0].key.clone(),
            Receipt {
                action: "revive".into(),
                at: now,
                outcome: Observation::Known(EffectOutcome::Revived),
            },
        );
        obj = with_status(&obj, &st);
        // Still lost: the next revive waits.
        let (plan, st) = run_at(&obj, lost_obs(true), now + 1);
        assert!(plan.effects.is_empty(), "{plan:#?}");
        assert_eq!(st.not_before, Some(now + RETRY_DELAY_MS));
        obj = with_status(&obj, &st);
        now += RETRY_DELAY_MS;
    }
    let (_, st) = run_at(&obj, lost_obs(true), now);
    assert_eq!(cond(&st, "NeedsAttention").reason, "ReviveFailed");
    let (_, st) = run_at(&with_status(&obj, &st), lost_obs(false), now);
    assert_eq!(cond(&st, "NeedsAttention").status, ConditionStatus::False);
}

#[test]
fn a_session_lost_before_its_brief_is_revived_and_briefed_once() {
    let obj = spawned(|_| {});
    let (plan, st) = run(&obj, lost_obs(true));
    assert_eq!(plan.effects.len(), 1);
    assert!(matches!(plan.effects[0].action, Action::Revive { .. }));
    let (plan, st) = run(
        &with_status(&obj, &st),
        Observations {
            workspace_trust: trusted(),
            ..lost_obs(false)
        },
    );
    assert_eq!(st.catch_up, None, "no note: the brief itself is still owed");
    assert_eq!(plan.effects.len(), 1);
    assert_eq!(plan.effects[0].key, "task/t/u7/brief");
}

// Pausing an idle session and resuming it when work arrives.

const PAUSE_AFTER: u64 = 60_000;

/// A Briefed Task whose session may be paused.
fn pausable(inbox: &[&str], extra: impl FnOnce(&mut TaskStatus)) -> Object {
    let mut obj = briefed_with(inbox, |st| {
        st.brief = Some(fleet_api::task::BriefRecord {
            key: "task/t/u7/brief".into(),
            generation: 1,
            recovered: false,
        });
        extra(st);
    });
    obj.spec["pauseAfterMs"] = json!(PAUSE_AFTER);
    obj
}

fn set_paused(st: &mut TaskStatus, reason: &str) {
    fleet_api::condition::set_condition(
        &mut st.conditions,
        "Paused",
        ConditionStatus::True,
        reason,
        "",
        1,
        NOW - 100,
    );
}

fn pause_obs(activity: Observation<Activity>, paused: Observation<bool>) -> Observations {
    Observations {
        find: live(),
        lost: Some(Observation::Known(false)),
        paused: Some(paused),
        activity: Some(activity),
        ready: Some(Observation::Known(true)),
        ..Default::default()
    }
}

fn idle() -> Observation<Activity> {
    Observation::Known(Activity::Idle)
}

fn not_asked<T>() -> Observation<T> {
    Observation::unknown("not asked")
}

fn effect_intent(action: &str) -> fleet_api::plan::Intent {
    fleet_api::plan::Intent {
        action: action.into(),
        at: NOW - 10,
        generation: 1,
    }
}

#[test]
fn only_a_known_idle_lasting_pause_after_ms_pauses() {
    let obj = pausable(&[], |_| {});
    assert!(probes(&obj).contains(&Probe::Activity { session: session() }));
    // The first Idle starts the idle time.
    let (plan, st) = run(&obj, pause_obs(idle(), Observation::Known(false)));
    assert!(plan.effects.is_empty(), "{plan:#?}");
    assert_eq!(st.idle_since, Some(NOW));

    // Unknown and Active never pause, and start the idle time again.
    for activity in [
        Observation::unknown("cannot tell"),
        Observation::Known(Activity::Active),
    ] {
        let later = NOW + PAUSE_AFTER;
        let (plan, after) = run_at(
            &with_status(&obj, &st),
            pause_obs(activity, Observation::Known(false)),
            later,
        );
        assert!(plan.effects.is_empty(), "{plan:#?}");
        assert_eq!(after.idle_since, None);
    }

    // Idle without a break for pauseAfterMs, read fresh: one pause.
    let (plan, st) = run_at(
        &with_status(&obj, &st),
        pause_obs(idle(), Observation::Known(false)),
        NOW + PAUSE_AFTER,
    );
    assert_eq!(plan.effects.len(), 1, "{plan:#?}");
    assert_eq!(plan.effects[0].key, "task/t/u7/pause/1");
    assert_eq!(plan.effects[0].action, Action::Pause { session: session() });
    assert!(st.intents.contains_key("task/t/u7/pause/1"));

    // A session not proven running is never paused.
    let (plan, _) = run(
        &pausable(&[], |st| st.idle_since = Some(0)),
        pause_obs(idle(), Observation::unknown("cannot tell")),
    );
    assert!(plan.effects.is_empty(), "{plan:#?}");
    // Nor is a Task with no pause setting.
    let plain = briefed_with(&[], |st| st.idle_since = Some(0));
    assert!(!probes(&plain).contains(&Probe::Activity { session: session() }));
    let (plan, _) = run(&plain, pause_obs(idle(), Observation::Known(false)));
    assert!(plan.effects.is_empty(), "{plan:#?}");
}

#[test]
fn a_pause_without_its_receipt_is_read_back_and_never_repeated() {
    let obj = pausable(&[], |st| {
        st.pauses = 1;
        st.intents
            .insert("task/t/u7/pause/1".into(), effect_intent("pause"));
    });

    // Unknown: wait, and do nothing.
    let (plan, st) = run(&obj, pause_obs(idle(), Observation::unknown("cannot tell")));
    assert!(plan.effects.is_empty(), "{plan:#?}");
    assert!(st.intents.contains_key("task/t/u7/pause/1"));

    // Stopped: the pause happened.
    let (plan, st) = run(&obj, pause_obs(idle(), Observation::Known(true)));
    assert!(plan.effects.is_empty(), "{plan:#?}");
    assert_eq!(cond(&st, "Paused").status, ConditionStatus::True);
    assert_eq!(cond(&st, "Paused").reason, "Idle");
    assert!(!st.intents.contains_key("task/t/u7/pause/1"));
    assert!(
        st.conditions
            .iter()
            .all(|c| c.type_ != "WorkerGone" && c.type_ != "SessionLost")
    );

    // Running: it did not, and the idle time starts again.
    let (plan, st) = run(&obj, pause_obs(idle(), Observation::Known(false)));
    assert!(plan.effects.is_empty(), "{plan:#?}");
    assert_eq!(cond(&st, "Paused").reason, "PauseFailed");
    assert!(!st.intents.contains_key("task/t/u7/pause/1"));
    assert_eq!(st.idle_since, Some(NOW), "counted again from this Idle");
}

#[test]
fn a_resume_without_its_receipt_is_read_back_and_retried_only_while_stopped() {
    let obj = pausable(&["task/t/u7/send/1"], |st| {
        set_paused(st, "Idle");
        st.resumes = 1;
        st.resume_attempts = 1;
        st.intents
            .insert("task/t/u7/resume/1".into(), effect_intent("resume"));
    });

    // Running: resumed; the catch-up note goes before the entry.
    let (plan, st) = run(&obj, pause_obs(not_asked(), Observation::Known(false)));
    assert_eq!(cond(&st, "Paused").status, ConditionStatus::False);
    assert_eq!(cond(&st, "Paused").reason, "Resumed");
    assert_eq!(st.catch_up.as_deref(), Some("task/t/u7/catchup/1"));
    assert_eq!(plan.effects.len(), 1, "{plan:#?}");
    assert_eq!(plan.effects[0].key, "task/t/u7/catchup/1");
    let Action::Brief { text, .. } = &plan.effects[0].action else {
        panic!("{plan:#?}")
    };
    assert!(text.contains("paused") && text.ends_with("do it"), "{text}");

    // Still stopped: a new resume, after a wait from the last one.
    let (plan, st) = run(&obj, pause_obs(not_asked(), Observation::Known(true)));
    assert!(plan.effects.is_empty(), "{plan:#?}");
    assert_eq!(st.not_before, Some(NOW - 10 + RETRY_DELAY_MS));
    let (plan, _) = run_at(
        &with_status(&obj, &st),
        pause_obs(not_asked(), Observation::Known(true)),
        NOW + RETRY_DELAY_MS,
    );
    assert_eq!(plan.effects.len(), 1, "{plan:#?}");
    assert_eq!(plan.effects[0].key, "task/t/u7/resume/2");
    assert_eq!(
        plan.effects[0].action,
        Action::Resume {
            session: session(),
            conversation: None
        }
    );

    // Unknown: wait, and do nothing.
    let (plan, st) = run(
        &obj,
        pause_obs(not_asked(), Observation::unknown("cannot tell")),
    );
    assert!(plan.effects.is_empty(), "{plan:#?}");
    assert!(st.intents.contains_key("task/t/u7/resume/1"));
}

#[test]
fn stalled_held_and_deleting_tasks_are_never_resumed() {
    let mut held = pausable(&["task/t/u7/send/1"], |st| set_paused(st, "Idle"));
    held.spec["dispatch"] = json!(false);
    let (plan, _) = run(&held, pause_obs(not_asked(), Observation::Known(true)));
    assert!(plan.effects.is_empty(), "{plan:#?}");

    let stalled = pausable(&["task/t/u7/send/1"], |st| {
        set_paused(st, "Idle");
        st.phase = Phase::Stalled;
    });
    let (plan, _) = run(&stalled, pause_obs(not_asked(), Observation::Known(true)));
    assert!(plan.effects.is_empty(), "{plan:#?}");

    // A stopped session is at rest: deleting the Task releases it.
    let mut deleting = pausable(&["task/t/u7/send/1"], |st| set_paused(st, "Idle"));
    deleting.metadata.deletion_timestamp = Some(NOW);
    let (plan, _) = run(
        &deleting,
        Observations {
            find: live(),
            at_rest: Some(Observation::Known(true)),
            ..Default::default()
        },
    );
    assert_eq!(plan.effects.len(), 1, "{plan:#?}");
    assert_eq!(
        plan.effects[0].action,
        Action::Release { session: session() }
    );
}

#[test]
fn a_note_kept_under_an_old_resume_key_is_still_delivered_first() {
    // A record written before the catch-up note was renamed.
    let mut obj = pausable(&["task/t/u7/send/1"], |_| {});
    obj.status["resume"] = json!("task/t/u7/resume/1");
    obj.status["resumes"] = json!(1);
    let (plan, st) = run(&obj, pause_obs(not_asked(), Observation::Known(false)));
    assert_eq!(st.catch_up.as_deref(), Some("task/t/u7/resume/1"));
    assert_eq!(plan.effects.len(), 1, "{plan:#?}");
    assert_eq!(plan.effects[0].key, "task/t/u7/resume/1");
    assert!(matches!(plan.effects[0].action, Action::Brief { .. }));
    let written = plan.status.unwrap();
    assert_eq!(written["catchUp"], "task/t/u7/resume/1");
    assert!(written.get("resume").is_none());

    // A later resume effect never reuses that note's key.
    let obj = pausable(&["task/t/u7/send/1"], |st| {
        set_paused(st, "External");
        st.resumes = 1;
    });
    let (plan, _) = run(&obj, pause_obs(not_asked(), Observation::Known(true)));
    assert_eq!(plan.effects[0].key, "task/t/u7/resume/2");
}

#[test]
fn an_old_resume_note_in_flight_is_never_taken_for_a_resume_effect() {
    // Written before the rename: the note `resume/3` was sent, and its
    // answer is not Known yet. Its intent is a Brief.
    let mut obj = pausable(&["task/t/u7/send/1"], |st| {
        st.intents
            .insert("task/t/u7/resume/3".into(), effect_intent("brief"));
        st.receipts.insert(
            "task/t/u7/resume/3".into(),
            receipt("brief", Observation::unknown("answer lost")),
        );
    });
    obj.status["resume"] = json!("task/t/u7/resume/3");
    obj.status["resumes"] = json!(3);
    assert!(probes(&obj).contains(&Probe::BriefDelivered {
        session: session(),
        key: "task/t/u7/resume/3".into()
    }));
    let mut obs = pause_obs(not_asked(), Observation::Known(false));
    obs.brief_delivered = Some(Observation::unknown("journal unreadable"));
    let (plan, st) = run(&obj, obs);
    assert!(
        plan.effects.is_empty(),
        "never sent again blindly: {plan:#?}"
    );
    assert!(st.intents.contains_key("task/t/u7/resume/3"));
    assert!(st.conditions.iter().all(|c| c.type_ != "Paused"));

    // The journal says it arrived: recorded, and the entry goes next.
    let mut obs = pause_obs(not_asked(), Observation::Known(false));
    obs.brief_delivered = Some(Observation::Known(true));
    let (plan, st) = run(&obj, obs);
    assert_eq!(st.catch_up, None);
    assert_eq!(plan.effects.len(), 1, "{plan:#?}");
    assert_eq!(plan.effects[0].key, "task/t/u7/send/1");
}

#[test]
fn a_delivery_to_a_concluded_task_starts_its_idle_time_again() {
    let obj = pausable(&["task/t/u7/send/1"], |st| {
        st.phase = Phase::Concluded;
        st.idle_since = Some(NOW - PAUSE_AFTER);
    });
    let (_, st) = run(&obj, pause_obs(idle(), Observation::Known(false)));
    assert_eq!(st.idle_since, None);
}

#[test]
fn a_concluded_task_at_rest_that_is_not_watched_loses_its_idle_time() {
    // Held at rest: unwatched passes leave no stale idle time behind, so a
    // later dispatch never pauses on a single Idle answer.
    let mut obj = pausable(&[], |st| {
        st.phase = Phase::Concluded;
        st.idle_since = Some(NOW - PAUSE_AFTER);
    });
    obj.spec["dispatch"] = json!(false);
    let (_, st) = run(
        &obj,
        Observations {
            find: live(),
            ..Default::default()
        },
    );
    assert_eq!(st.idle_since, None);
}

#[test]
fn a_recorded_result_starts_the_idle_time_again() {
    let obj = pausable(&[], |st| {
        st.idle_since = Some(NOW - PAUSE_AFTER);
        st.result = Some(
            serde_json::from_value(json!({
                "outcome": "shipped", "generation": 1, "summary": "done"
            }))
            .unwrap(),
        );
    });
    let (_, st) = run(&obj, pause_obs(idle(), Observation::Known(false)));
    assert_eq!(st.phase, Phase::Concluded);
    assert_eq!(st.idle_since, None);
}

#[test]
fn a_concluded_session_found_stopped_is_external_even_while_waiting() {
    let obj = pausable(&["task/t/u7/send/1"], |st| {
        st.phase = Phase::Concluded;
        st.not_before = Some(NOW + RETRY_DELAY_MS);
    });
    let (plan, st) = run(&obj, pause_obs(not_asked(), Observation::Known(true)));
    assert_eq!(cond(&st, "Paused").reason, "External");
    assert!(
        plan.effects.is_empty(),
        "the wait holds the resume: {plan:#?}"
    );
}

#[test]
fn the_status_reports_what_the_runtime_proved_about_the_session() {
    let st = TaskStatus {
        phase: Phase::Briefed,
        session: Some(session()),
        brief: Some(fleet_api::task::BriefRecord {
            key: "task/t/u7/brief".into(),
            generation: 1,
            recovered: false,
        }),
        ..Default::default()
    };
    let obj = task(true, Some(st.clone()));
    let obs = Observations {
        find: Some(Observation::Known(Some(session()))),
        lost: Some(Observation::Known(false)),
        paused: Some(Observation::Known(false)),
        ..Default::default()
    };
    let (_, after) = run(&obj, obs);
    assert_eq!(after.session_state, Some(SessionState::Running));

    // A doubt keeps the last state; a proof replaces it.
    let obj = task(true, Some(after));
    let obs = Observations {
        find: Some(Observation::unknown("list unreadable")),
        lost: Some(Observation::unknown("timeout")),
        ..Default::default()
    };
    let (plan, after) = run(&obj, obs);
    assert_eq!(after.session_state, Some(SessionState::Running));
    assert!(plan.status.is_none(), "nothing moved: {plan:#?}");
    let obj = task(true, Some(after));
    let obs = Observations {
        find: Some(Observation::Known(Some(session()))),
        lost: Some(Observation::Known(true)),
        ..Default::default()
    };
    let (_, after) = run(&obj, obs);
    assert_eq!(after.session_state, Some(SessionState::Lost));
    assert_eq!(cond(&after, "SessionLost").status, ConditionStatus::True);
}
