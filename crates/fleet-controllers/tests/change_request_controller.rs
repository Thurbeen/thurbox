//! ChangeRequestController as a pure function: a ChangeRequest and one
//! observation of its change go in; a plan comes out. Each case is one
//! transition or one Unknown of the mirror's lifecycle.

use fleet_api::change_request::{
    ChangeRequestDefaults, ChangeRequestStatus, DEFAULT_POLL_MS, NOTE_CAP, Phase,
};
use fleet_api::condition::ConditionStatus;
use fleet_api::forge::{
    ChangeObservation, ChangeRef, ChangeSnapshot, ChangeState, Checks, FailingCheck, HeadSeen,
    Mergeable, Note, NoteKind, Review, ReviewDecision, ReviewState, Threads, Uncertainty,
};
use fleet_api::plan::{Plan, Probe};
use fleet_api::{Object, Observation};
use fleet_controllers::{ChangeRequestController, ChangeRequestInput, ChangeRequestObservations};
use serde_json::json;

const NOW: u64 = 1_000_000;
const POLL: u64 = 5_000;
const GRACE: u64 = 100_000;
const TTL: u64 = 1_000_000;

fn change_ref() -> ChangeRef {
    ChangeRef {
        kind: "fake".into(),
        host: "forge.test".into(),
        project: "acme/widgets".into(),
        id: "7".into(),
    }
}

fn object(status: Option<&ChangeRequestStatus>) -> Object {
    let mut o: Object = serde_json::from_value(json!({
        "apiVersion": "fleet/v1alpha1",
        "kind": "ChangeRequest",
        "metadata": {"name": "cr-abc", "uid": "u3", "generation": 1, "resourceVersion": 9},
        "spec": {"url": "https://forge.test/acme/widgets/pull/7", "pollMs": POLL,
                 "closedGraceMs": GRACE, "settledTtlMs": TTL}
    }))
    .unwrap();
    if let Some(s) = status {
        o.status = serde_json::to_value(s).unwrap();
    }
    o
}

fn note(id: &str, at: u64, body: &str) -> Note {
    Note {
        id: id.into(),
        kind: NoteKind::Comment,
        author: "ann".into(),
        bot: false,
        thread: None,
        resolved: None,
        path: None,
        line: None,
        at,
        edited_at: None,
        url: format!("https://forge.test/n/{id}"),
        body: body.into(),
    }
}

fn inline(id: &str, at: u64, thread: &str, resolved: bool) -> Note {
    Note {
        kind: NoteKind::Inline,
        thread: Some(thread.into()),
        resolved: Some(resolved),
        path: Some("src/a.rs".into()),
        line: Some(3),
        ..note(id, at, "fix this")
    }
}

fn snapshot(state: ChangeState) -> ChangeSnapshot {
    ChangeSnapshot {
        state,
        head: HeadSeen {
            id: "h1".into(),
            checks: Checks::Pending,
            failing: Vec::new(),
        },
        review_decision: ReviewDecision::None,
        mergeable: Mergeable {
            value: None,
            reason: None,
        },
        threads: Threads { unresolved: 0 },
        reviews: Vec::new(),
        notes: vec![note("n1", 10, "first"), note("n2", 20, "second")],
    }
}

fn seen(s: ChangeSnapshot) -> ChangeRequestObservations {
    ChangeRequestObservations {
        parsed: Some(Observation::Known(change_ref())),
        change: Some(ChangeObservation::Known(Some(s))),
    }
}

fn run_at(obj: &Object, obs: ChangeRequestObservations, now: u64) -> (Plan, ChangeRequestStatus) {
    let input = ChangeRequestInput {
        object: obj,
        observations: obs,
        now,
        defaults: ChangeRequestDefaults::default(),
    };
    let plan = ChangeRequestController.reconcile(&input).unwrap();
    let st = match &plan.status {
        Some(v) => serde_json::from_value(v.clone()).unwrap(),
        None => serde_json::from_value(obj.status.clone()).unwrap_or_default(),
    };
    (plan, st)
}

fn run(obj: &Object, obs: ChangeRequestObservations) -> (Plan, ChangeRequestStatus) {
    run_at(obj, obs, NOW)
}

/// The status after a first complete poll of an open change at NOW.
fn watched() -> ChangeRequestStatus {
    let (_, st) = run(&object(None), seen(snapshot(ChangeState::Open)));
    st
}

fn probes_at(obj: &Object, obs: ChangeRequestObservations, now: u64) -> Vec<Probe> {
    let input = ChangeRequestInput {
        object: obj,
        observations: obs,
        now,
        defaults: ChangeRequestDefaults::default(),
    };
    ChangeRequestController.probes(&input).unwrap()
}

fn cond<'a>(st: &'a ChangeRequestStatus, t: &str) -> &'a fleet_api::condition::Condition {
    st.conditions
        .iter()
        .find(|c| c.type_ == t)
        .unwrap_or_else(|| panic!("no {t} in {st:#?}"))
}

#[test]
fn the_first_poll_needs_the_parsed_ref_and_then_asks_for_the_change() {
    let obj = object(None);
    assert!(
        probes_at(&obj, ChangeRequestObservations::default(), NOW).is_empty(),
        "no ref, nothing to ask"
    );
    let parsed = ChangeRequestObservations {
        parsed: Some(Observation::Known(change_ref())),
        ..Default::default()
    };
    assert_eq!(
        probes_at(&obj, parsed, NOW),
        vec![Probe::Change {
            change: change_ref()
        }]
    );
    let st = watched();
    assert_eq!(st.reference, Some(change_ref()), "the ref is recorded");
    let later = object(Some(&st));
    assert_eq!(
        probes_at(&later, ChangeRequestObservations::default(), NOW + POLL),
        vec![Probe::Change {
            change: change_ref()
        }],
        "from then on the recorded ref names the probe"
    );
    assert!(
        probes_at(&later, ChangeRequestObservations::default(), NOW + 1).is_empty(),
        "not due before notBefore"
    );
}

#[test]
fn a_url_the_forge_cannot_place_is_unknown_and_tried_again() {
    let obs = ChangeRequestObservations {
        parsed: Some(Observation::unknown("host x is not configured")),
        ..Default::default()
    };
    let (plan, st) = run(&object(None), obs);
    assert!(!plan.delete);
    assert_eq!(st.reference, None);
    assert_eq!(cond(&st, "Reachable").status, ConditionStatus::Unknown);
    assert_eq!(cond(&st, "Reachable").reason, "UrlNotParsed");
    assert_eq!(st.not_before, Some(NOW + POLL));
}

#[test]
fn the_first_complete_poll_numbers_everything_at_revision_one() {
    let st = watched();
    assert_eq!(st.phase, Phase::Watching);
    assert_eq!(st.revision, 1);
    assert_eq!(st.observed_at, Some(NOW));
    assert_eq!(st.not_before, Some(NOW + POLL));
    assert_eq!(cond(&st, "Reachable").status, ConditionStatus::True);
    let state = st.state.as_ref().unwrap();
    assert_eq!((state.value, state.seq), (ChangeState::Open, 1));
    assert!(st.ever_ready);
    let head = st.head.as_ref().unwrap();
    assert_eq!(
        (head.id.as_str(), head.seq, head.first_seen_at),
        ("h1", 1, NOW)
    );
    assert_eq!(st.review_decision.as_ref().unwrap().seq, 1);
    let seqs: Vec<(&str, u64)> = st.notes.iter().map(|n| (n.id.as_str(), n.seq)).collect();
    assert_eq!(seqs, vec![("n1", 1), ("n2", 1)]);
    assert_eq!(st.dropped_before, 0);
    assert_eq!(cond(&st, "Mergeable").status, ConditionStatus::Unknown);
    assert_eq!(cond(&st, "Mergeable").reason, "Computing");
}

#[test]
fn an_unchanged_poll_writes_bookkeeping_only() {
    let before = watched();
    let obj = object(Some(&before));
    let (plan, st) = run_at(&obj, seen(snapshot(ChangeState::Open)), NOW + POLL);
    assert!(plan.status.is_some(), "observedAt and notBefore move");
    assert_eq!(st.revision, 1, "the snapshot did not change");
    assert_eq!(st.observed_at, Some(NOW + POLL));
    assert_eq!(st.not_before, Some(NOW + 2 * POLL));
    let mut snapshot_only = st.clone();
    snapshot_only.observed_at = before.observed_at;
    snapshot_only.not_before = before.not_before;
    assert_eq!(snapshot_only, before, "nothing but the bookkeeping moved");
    assert_eq!(
        cond(&st, "Reachable").last_transition_time,
        cond(&before, "Reachable").last_transition_time,
        "a re-derived condition is not a transition"
    );
}

#[test]
fn a_new_note_takes_the_revision_that_first_showed_it() {
    let before = watched();
    let mut s = snapshot(ChangeState::Open);
    s.notes.push(note("n3", 30, "third"));
    let (_, st) = run_at(&object(Some(&before)), seen(s), NOW + POLL);
    assert_eq!(st.revision, 2);
    let seqs: Vec<(&str, u64)> = st.notes.iter().map(|n| (n.id.as_str(), n.seq)).collect();
    assert_eq!(seqs, vec![("n1", 1), ("n2", 1), ("n3", 2)]);
    assert_eq!(st.state.as_ref().unwrap().seq, 1, "the state did not move");
    assert_eq!(st.head.as_ref().unwrap().seq, 1, "nor the head");
}

#[test]
fn an_edit_moves_a_notes_number_and_a_deletion_just_removes_it() {
    let before = watched();
    let mut s = snapshot(ChangeState::Open);
    s.notes[0].body = "first, edited".into();
    s.notes[0].edited_at = Some(40);
    s.notes.remove(1);
    let (_, st) = run_at(&object(Some(&before)), seen(s), NOW + POLL);
    assert_eq!(st.revision, 2);
    let seqs: Vec<(&str, u64)> = st.notes.iter().map(|n| (n.id.as_str(), n.seq)).collect();
    assert_eq!(seqs, vec![("n1", 2)]);
    assert_eq!(st.notes[0].body, "first, edited");
    assert_eq!(st.dropped_before, 0, "a deleted note is not a rotated one");
}

#[test]
fn an_unresolve_moves_the_number_and_a_resolve_does_not() {
    let mut s = snapshot(ChangeState::Open);
    s.notes = vec![inline("i1", 10, "t1", false)];
    s.threads = Threads { unresolved: 1 };
    let (_, first) = run(&object(None), seen(s.clone()));
    assert_eq!(first.notes[0].seq, 1);

    s.notes[0].resolved = Some(true);
    s.threads = Threads { unresolved: 0 };
    let (_, resolved) = run_at(&object(Some(&first)), seen(s.clone()), NOW + POLL);
    assert_eq!(resolved.revision, 2, "the snapshot changed");
    assert_eq!(
        resolved.notes[0].seq, 1,
        "a resolve is not news to the author"
    );
    assert_eq!(resolved.threads, Some(Threads { unresolved: 0 }));

    s.notes[0].resolved = Some(false);
    s.threads = Threads { unresolved: 1 };
    let (_, reopened) = run_at(&object(Some(&resolved)), seen(s), NOW + 2 * POLL);
    assert_eq!(reopened.revision, 3);
    assert_eq!(reopened.notes[0].seq, 3, "an unresolve is");
}

#[test]
fn head_state_and_decision_transitions_take_the_revision() {
    let before = watched();
    let mut s = snapshot(ChangeState::Open);
    s.head.checks = Checks::Failed;
    s.head.failing = vec![FailingCheck {
        name: "ci".into(),
        url: None,
    }];
    let (_, st) = run_at(&object(Some(&before)), seen(s.clone()), NOW + POLL);
    let head = st.head.as_ref().unwrap();
    assert_eq!(
        (st.revision, head.seq, head.first_seen_at),
        (2, 2, NOW),
        "checks moved, the id did not"
    );

    s.head.id = "h2".into();
    s.head.checks = Checks::None;
    s.head.failing.clear();
    s.review_decision = ReviewDecision::Approved;
    s.reviews = vec![Review {
        id: "r1".into(),
        author: "ann".into(),
        bot: false,
        state: ReviewState::Approved,
        head: "h2".into(),
    }];
    s.mergeable = Mergeable {
        value: Some(true),
        reason: None,
    };
    let (_, st) = run_at(&object(Some(&st)), seen(s), NOW + 2 * POLL);
    let head = st.head.as_ref().unwrap();
    assert_eq!(
        (st.revision, head.id.as_str(), head.seq, head.first_seen_at),
        (3, "h2", 3, NOW + 2 * POLL)
    );
    assert_eq!(st.review_decision.as_ref().unwrap().seq, 3);
    assert_eq!(st.state.as_ref().unwrap().seq, 1, "the state never moved");
    assert_eq!(st.reviews.len(), 1);
    assert_eq!(cond(&st, "Mergeable").status, ConditionStatus::True);
}

#[test]
fn a_forge_reason_for_not_merging_is_reported_and_never_acted_on() {
    let mut s = snapshot(ChangeState::Open);
    s.mergeable = Mergeable {
        value: Some(false),
        reason: Some("ci_must_pass".into()),
    };
    let (plan, st) = run(&object(None), seen(s));
    assert!(plan.effects.is_empty(), "no effect, ever");
    let c = cond(&st, "Mergeable");
    assert_eq!(
        (c.status, c.reason.as_str(), c.message.as_str()),
        (ConditionStatus::False, "CiMustPass", "ci_must_pass")
    );
}

#[test]
fn more_than_the_cap_rotates_the_oldest_and_raises_dropped_before() {
    let mut s = snapshot(ChangeState::Open);
    s.notes = (0..NOTE_CAP as u64)
        .map(|i| note(&format!("n{i}"), 100 + i, "x"))
        .collect();
    let (_, first) = run(&object(None), seen(s.clone()));
    assert_eq!(first.notes.len(), NOTE_CAP);
    assert_eq!(first.dropped_before, 0);

    for i in 0..5u64 {
        s.notes.push(note(&format!("new{i}"), 1_000 + i, "y"));
    }
    let (_, st) = run_at(&object(Some(&first)), seen(s.clone()), NOW + POLL);
    assert_eq!(st.revision, 2);
    assert_eq!(st.notes.len(), NOTE_CAP);
    assert!(
        st.notes.iter().all(|n| n.id != "n0" && n.id != "n4"),
        "the five oldest left"
    );
    assert_eq!(
        st.dropped_before, 1,
        "a consumer below 1 knows it has a gap"
    );
    assert_eq!(st.notes.iter().filter(|n| n.seq == 2).count(), 5);

    // The same forge answer again: the rotated notes are not new, nothing moves.
    let (_, again) = run_at(&object(Some(&st)), seen(s), NOW + 2 * POLL);
    assert_eq!((again.revision, again.dropped_before), (2, 1));
}

#[test]
fn an_unknown_answer_leaves_the_snapshot_untouched_and_waits_one_interval() {
    let before = watched();
    let obs = ChangeRequestObservations {
        change: Some(ChangeObservation::unknown(
            Uncertainty::Timeout,
            "gh timed out",
        )),
        ..Default::default()
    };
    let (plan, st) = run_at(&object(Some(&before)), obs, NOW + POLL);
    assert!(plan.status.is_some());
    let c = cond(&st, "Reachable");
    assert_eq!(
        (c.status, c.reason.as_str(), c.message.as_str()),
        (ConditionStatus::Unknown, "Timeout", "gh timed out")
    );
    assert_eq!(st.not_before, Some(NOW + 2 * POLL));
    assert_eq!(st.observed_at, before.observed_at, "no Known answer");
    let (snap_before, snap_after) = (
        (before.revision, &before.state, &before.notes),
        (st.revision, &st.state, &st.notes),
    );
    assert_eq!(snap_before, snap_after);
    assert_eq!(
        st.phase,
        Phase::Watching,
        "Unknown never means closed or gone"
    );
}

#[test]
fn a_throttled_answer_waits_as_long_as_the_forge_said() {
    let before = watched();
    let obs = ChangeRequestObservations {
        change: Some(ChangeObservation::Unknown {
            kind: Uncertainty::Throttled,
            why: "secondary rate limit".into(),
            retry_after: Some(90_000),
        }),
        ..Default::default()
    };
    let (_, st) = run_at(&object(Some(&before)), obs, NOW + POLL);
    assert_eq!(cond(&st, "Reachable").reason, "Throttled");
    assert_eq!(st.not_before, Some(NOW + POLL + 90_000));
}

#[test]
fn proven_absence_settles_as_gone() {
    let before = watched();
    let obs = ChangeRequestObservations {
        change: Some(ChangeObservation::Known(None)),
        ..Default::default()
    };
    let (plan, st) = run_at(&object(Some(&before)), obs, NOW + POLL);
    assert!(!plan.delete);
    assert_eq!(st.phase, Phase::Settled);
    assert_eq!(cond(&st, "Settled").reason, "Gone");
    assert_eq!(st.settled_at, Some(NOW + POLL));
    assert_eq!(
        st.not_before,
        Some(NOW + POLL + TTL),
        "nothing asked until retention"
    );
    assert_eq!(st.revision, 1, "the last snapshot stays");
    assert!(
        probes_at(
            &object(Some(&st)),
            ChangeRequestObservations::default(),
            NOW + 2 * POLL
        )
        .is_empty()
    );
}

#[test]
fn a_merged_change_settles_with_its_final_snapshot() {
    let before = watched();
    let (_, st) = run_at(
        &object(Some(&before)),
        seen(snapshot(ChangeState::Merged)),
        NOW + POLL,
    );
    assert_eq!(st.phase, Phase::Settled);
    assert_eq!(cond(&st, "Settled").reason, "Merged");
    let state = st.state.as_ref().unwrap();
    assert_eq!(
        (state.value, state.seq, st.revision),
        (ChangeState::Merged, 2, 2)
    );
    assert_eq!(st.settled_at, Some(NOW + POLL));
}

#[test]
fn a_closed_change_is_watched_slowly_and_reopens_into_watching() {
    let before = watched();
    let (_, closed) = run_at(
        &object(Some(&before)),
        seen(snapshot(ChangeState::Closed)),
        NOW + POLL,
    );
    assert_eq!(closed.phase, Phase::Closed);
    assert_eq!(closed.closed_at, Some(NOW + POLL));
    assert_eq!(
        closed.not_before,
        Some(NOW + POLL + 10 * POLL),
        "ten times the interval"
    );
    assert_eq!(closed.revision, 2);

    // Still closed within the grace: bookkeeping and the slow cadence.
    let t2 = NOW + 11 * POLL;
    let (_, still) = run_at(
        &object(Some(&closed)),
        seen(snapshot(ChangeState::Closed)),
        t2,
    );
    assert_eq!((still.phase, still.revision), (Phase::Closed, 2));
    assert_eq!(
        still.closed_at,
        Some(NOW + POLL),
        "the first closing counts"
    );
    assert_eq!(still.not_before, Some(t2 + 10 * POLL));

    // Reopened within the grace: Watching again, numbered through the state.
    let t3 = t2 + 10 * POLL;
    let (_, reopened) = run_at(&object(Some(&still)), seen(snapshot(ChangeState::Open)), t3);
    assert_eq!(reopened.phase, Phase::Watching);
    assert_eq!(reopened.closed_at, None);
    assert_eq!(
        (reopened.revision, reopened.state.as_ref().unwrap().seq),
        (3, 3)
    );
    assert_eq!(reopened.not_before, Some(t3 + POLL));
}

#[test]
fn a_change_closed_past_the_grace_settles() {
    let before = watched();
    let (_, closed) = run_at(
        &object(Some(&before)),
        seen(snapshot(ChangeState::Closed)),
        NOW + POLL,
    );
    let late = NOW + POLL + GRACE;
    let (_, st) = run_at(
        &object(Some(&closed)),
        seen(snapshot(ChangeState::Closed)),
        late,
    );
    assert_eq!(st.phase, Phase::Settled);
    assert_eq!(cond(&st, "Settled").reason, "ClosedGraceExpired");
    assert_eq!(st.settled_at, Some(late));
}

#[test]
fn a_settled_object_is_deleted_only_once_its_retention_passed() {
    let before = watched();
    let (_, settled) = run_at(
        &object(Some(&before)),
        seen(snapshot(ChangeState::Merged)),
        NOW + POLL,
    );
    let settled_at = settled.settled_at.unwrap();
    let obj = object(Some(&settled));
    let (plan, st) = run_at(
        &obj,
        ChangeRequestObservations::default(),
        settled_at + TTL - 1,
    );
    assert!(!plan.delete, "not yet");
    assert_eq!(st.not_before, Some(settled_at + TTL));
    let (plan, _) = run_at(&obj, ChangeRequestObservations::default(), settled_at + TTL);
    assert!(
        plan.delete,
        "retention passed: the controller retires its own object"
    );
    assert!(plan.status.is_none(), "nothing else to write");
    assert!(plan.effects.is_empty());
    assert!(plan.remove_finalizer.is_none());
}

#[test]
fn a_draft_is_watched_but_never_ready_until_it_leaves_draft() {
    let (_, draft) = run(&object(None), seen(snapshot(ChangeState::Draft)));
    assert!(!draft.ever_ready);
    assert_eq!(draft.state.as_ref().unwrap().value, ChangeState::Draft);
    let (_, open) = run_at(
        &object(Some(&draft)),
        seen(snapshot(ChangeState::Open)),
        NOW + POLL,
    );
    assert!(open.ever_ready);
    assert_eq!(open.state.as_ref().unwrap().seq, 2);
    let (_, back) = run_at(
        &object(Some(&open)),
        seen(snapshot(ChangeState::Draft)),
        NOW + 2 * POLL,
    );
    assert!(back.ever_ready, "never returns to false");
}

#[test]
fn a_deleting_object_asks_nothing_and_plans_nothing() {
    let mut obj = object(Some(&watched()));
    obj.metadata.deletion_timestamp = Some(NOW);
    assert!(probes_at(&obj, ChangeRequestObservations::default(), NOW + POLL).is_empty());
    let (plan, _) = run_at(&obj, seen(snapshot(ChangeState::Open)), NOW + POLL);
    assert_eq!(plan, Plan::default());
}

#[test]
fn malformed_spec_or_status_is_invalid_not_a_guess() {
    let mut obj = object(None);
    obj.spec = json!({"url": "https://forge.test/acme/widgets/pull/7", "relayNotes": true});
    let input = ChangeRequestInput {
        object: &obj,
        observations: ChangeRequestObservations::default(),
        now: NOW,
        defaults: ChangeRequestDefaults::default(),
    };
    assert!(matches!(
        ChangeRequestController.reconcile(&input),
        Err(fleet_api::StoreError::Invalid(_))
    ));
    let mut obj = object(None);
    obj.status = json!({"phase": "Elsewhere"});
    let input = ChangeRequestInput {
        object: &obj,
        observations: ChangeRequestObservations::default(),
        now: NOW,
        defaults: ChangeRequestDefaults::default(),
    };
    assert!(matches!(
        ChangeRequestController.probes(&input),
        Err(fleet_api::StoreError::Invalid(_))
    ));
}

#[test]
fn the_configured_defaults_apply_when_the_spec_names_none() {
    let mut obj = object(None);
    obj.spec = json!({"url": "https://forge.test/acme/widgets/pull/7"});
    let defaults = ChangeRequestDefaults {
        closed_grace_ms: 7,
        settled_ttl_ms: 11,
    };
    let input = ChangeRequestInput {
        object: &obj,
        observations: seen(snapshot(ChangeState::Merged)),
        now: NOW,
        defaults,
    };
    let plan = ChangeRequestController.reconcile(&input).unwrap();
    let st: ChangeRequestStatus = serde_json::from_value(plan.status.unwrap()).unwrap();
    assert_eq!(st.not_before, Some(NOW + 11), "the configured retention");
    let mut obj = object(None);
    obj.spec = json!({"url": "https://forge.test/acme/widgets/pull/7"});
    let (_, st) = run(&obj, seen(snapshot(ChangeState::Open)));
    assert_eq!(
        st.not_before,
        Some(NOW + DEFAULT_POLL_MS),
        "the resource's poll default"
    );
}
