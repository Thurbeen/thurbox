//! ListenerController as a pure function: a Listener with the beats and
//! refusals the verb recorded, the account's configuration, and the
//! platform's answers go in; the three conditions come out.

use std::collections::BTreeMap;

use fleet_api::condition::{Condition, ConditionStatus};
use fleet_api::gateway::{
    AccountConfig, Capabilities, GatewayDefaults, Identity, Inbound, ListenerAccount, Marker,
};
use fleet_api::listener::{Beat, BeatState, ListenerStatus, Refused};
use fleet_api::plan::{Plan, Probe};
use fleet_api::{Object, Observation};
use fleet_controllers::{ListenerController, ListenerInput, ListenerObservations};
use serde_json::json;

const NOW: u64 = 1_730_000_000_000;
const ACCOUNT: &str = "fake-main";
const DIGEST: &str = "sha256:current";
const BOT: &str = "B0FLEET001";
const BEAT_MS: u64 = 1_000;
const POLL_MS: u64 = 60_000;

fn account(routed: bool) -> ListenerAccount {
    ListenerAccount {
        config: AccountConfig {
            platform: "fake".into(),
            team: None,
            bot: BOT.into(),
            inbound: Inbound::Connection,
            message_content: None,
            cli: None,
            curl_config: None,
            dir: Some("gateway".into()),
        },
        digest: DIGEST.into(),
        routed,
    }
}

fn defaults(account: Option<ListenerAccount>) -> GatewayDefaults {
    GatewayDefaults {
        beat_ms: BEAT_MS,
        poll_ms: POLL_MS,
        ..GatewayDefaults::default()
    }
    .for_account(account)
}

fn object(status: Option<&ListenerStatus>) -> Object {
    let mut v = json!({
        "apiVersion": "fleet/v1alpha1",
        "kind": "Listener",
        "metadata": {"name": "listener-fake-main", "uid": "l1", "generation": 1, "resourceVersion": 2},
        "spec": {"account": ACCOUNT}
    });
    if let Some(st) = status {
        v["status"] = serde_json::to_value(st).unwrap();
    }
    serde_json::from_value(v).unwrap()
}

fn beat(state: BeatState, digest: &str, at: u64) -> Beat {
    Beat {
        state,
        digest: digest.into(),
        at,
        sent_at: at.saturating_sub(10),
        last_delivery_at: None,
    }
}

fn resolved() -> ListenerObservations {
    ListenerObservations {
        capabilities: Some(Observation::Known(Capabilities {
            history: true,
            marker: Marker::Metadata,
            unmentioned: true,
            threads: true,
            reactions: false,
        })),
        whoami: Some(Observation::Known(Identity {
            id: BOT.into(),
            name: Some("fleet".into()),
        })),
    }
}

fn run_with(
    obj: &Object,
    obs: ListenerObservations,
    defaults: GatewayDefaults,
    now: u64,
) -> (Plan, ListenerStatus) {
    let input = ListenerInput {
        object: obj,
        observations: obs,
        now,
        defaults,
    };
    let plan = ListenerController.reconcile(&input).unwrap();
    let st = match &plan.status {
        Some(v) => serde_json::from_value(v.clone()).unwrap(),
        None => serde_json::from_value(obj.status.clone()).unwrap_or_default(),
    };
    (plan, st)
}

fn run(obj: &Object, obs: ListenerObservations) -> (Plan, ListenerStatus) {
    run_with(obj, obs, defaults(Some(account(true))), NOW)
}

fn cond<'a>(st: &'a ListenerStatus, t: &str) -> &'a Condition {
    st.conditions
        .iter()
        .find(|c| c.type_ == t)
        .unwrap_or_else(|| panic!("no {t} in {st:#?}"))
}

fn reason(st: &ListenerStatus, t: &str) -> (ConditionStatus, String) {
    let c = cond(st, t);
    (c.status, c.reason.clone())
}

fn with_beats(beats: Vec<(&str, Beat)>) -> ListenerStatus {
    let mut st = ListenerStatus::default();
    for (instance, b) in beats {
        st.record_beat(instance, b);
    }
    st
}

#[test]
fn a_fresh_listener_asks_for_the_identity_and_the_capabilities_every_poll() {
    let obj = object(None);
    assert!(ListenerController::due(&obj, NOW));
    let input = ListenerInput {
        object: &obj,
        observations: ListenerObservations::default(),
        now: NOW,
        defaults: defaults(Some(account(true))),
    };
    assert_eq!(
        ListenerController.probes(&input).unwrap(),
        vec![
            Probe::Capabilities {
                account: ACCOUNT.into()
            },
            Probe::GatewayWhoami {
                account: ACCOUNT.into()
            }
        ]
    );
    let (_, st) = run(&obj, resolved());
    assert_eq!(st.observed_at, Some(NOW));
    let later = object(Some(&st));
    let input = ListenerInput {
        object: &later,
        observations: ListenerObservations::default(),
        now: NOW + POLL_MS - 1,
        defaults: defaults(Some(account(true))),
    };
    assert!(ListenerController.probes(&input).unwrap().is_empty());
    let input = ListenerInput {
        object: &later,
        observations: ListenerObservations::default(),
        now: NOW + POLL_MS,
        defaults: defaults(Some(account(true))),
    };
    assert_eq!(ListenerController.probes(&input).unwrap().len(), 2);
}

#[test]
fn accepted_and_resolved_follow_the_config_and_the_platforms_identity() {
    let obj = object(None);
    let (plan, st) = run(&obj, resolved());
    assert!(!plan.delete);
    assert_eq!(
        st.accepted.as_ref().map(|a| (a.digest.as_str(), a.at)),
        Some((DIGEST, NOW))
    );
    assert_eq!(
        reason(&st, "Accepted"),
        (ConditionStatus::True, "Loaded".into())
    );
    assert_eq!(
        reason(&st, "Resolved"),
        (ConditionStatus::True, "IdentityMatches".into())
    );
    assert_eq!(
        reason(&st, "InboundReady"),
        (ConditionStatus::False, "NoListener".into())
    );
    assert_eq!(st.seen, 0);
    assert_eq!(st.not_before, Some(NOW + POLL_MS));
    // The accepted time holds while the digest does; a new digest moves it.
    let (_, again) = run_with(
        &object(Some(&st)),
        ListenerObservations::default(),
        defaults(Some(account(true))),
        NOW + 5,
    );
    assert_eq!(again.accepted.as_ref().unwrap().at, NOW);
    let mut changed = account(true);
    changed.digest = "sha256:new".into();
    let (_, moved) = run_with(
        &object(Some(&st)),
        ListenerObservations::default(),
        defaults(Some(changed)),
        NOW + 5,
    );
    assert_eq!(moved.accepted.as_ref().unwrap().at, NOW + 5);
    // No route uses the account.
    let (_, st) = run_with(&obj, resolved(), defaults(Some(account(false))), NOW);
    assert_eq!(
        reason(&st, "Accepted"),
        (ConditionStatus::False, "NoRoute".into())
    );
    // The platform names another bot.
    let other = ListenerObservations {
        whoami: Some(Observation::Known(Identity {
            id: "B0OTHER".into(),
            name: None,
        })),
        ..resolved()
    };
    let (_, st) = run(&obj, other);
    assert_eq!(
        reason(&st, "Resolved"),
        (ConditionStatus::False, "IdentityMismatch".into())
    );
    assert!(cond(&st, "Resolved").message.contains("B0OTHER"));
    // Unknown answers are Unknown, and leave the last observation alone.
    let lost = ListenerObservations {
        whoami: Some(Observation::unknown("lost")),
        ..resolved()
    };
    let (_, st) = run(&obj, lost);
    assert_eq!(
        reason(&st, "Resolved"),
        (ConditionStatus::Unknown, "Unreachable".into())
    );
    assert_eq!(st.observed_at, None);
    assert_eq!(
        st.not_before,
        Some(NOW + POLL_MS),
        "an unreachable platform is asked again next poll, not next sweep"
    );
    // Not asked: what was recorded stands.
    let (_, st) = run(&obj, resolved());
    let (_, same) = run(&object(Some(&st)), ListenerObservations::default());
    assert_eq!(cond(&same, "Resolved"), cond(&st, "Resolved"));
}

#[test]
fn inbound_ready_judges_the_beats_against_the_window_and_the_digest() {
    let window = 3 * BEAT_MS;
    let fresh = with_beats(vec![("a1", beat(BeatState::Connected, DIGEST, NOW - 10))]);
    let (_, st) = run(&object(Some(&fresh)), resolved());
    assert_eq!(
        reason(&st, "InboundReady"),
        (ConditionStatus::True, "Connected".into())
    );
    assert_eq!(st.seen, 1, "the beat revision was judged");
    assert_eq!(
        st.not_before,
        Some(NOW - 10 + window),
        "the moment the freshest beat goes stale, before the next poll"
    );
    assert!(!ListenerController::due(&object(Some(&st)), NOW + 10));
    assert!(ListenerController::due(
        &object(Some(&st)),
        NOW - 10 + window
    ));
    let cases: Vec<(&str, ListenerStatus, &str)> = vec![
        (
            "stale",
            with_beats(vec![(
                "a1",
                beat(BeatState::Connected, DIGEST, NOW - window - 1),
            )]),
            "ListenerStale",
        ),
        (
            "old digest",
            with_beats(vec![(
                "a1",
                beat(BeatState::Connected, "sha256:old", NOW - 10),
            )]),
            "ListenerConfigOld",
        ),
        (
            "connecting",
            with_beats(vec![
                ("a1", beat(BeatState::Connecting, DIGEST, NOW - 10)),
                ("a2", beat(BeatState::Connected, "sha256:old", NOW - 10)),
            ]),
            "Connecting",
        ),
        (
            "closing",
            with_beats(vec![("a1", beat(BeatState::Closing, DIGEST, NOW - 10))]),
            "Closing",
        ),
        (
            "one fresh instance is enough",
            with_beats(vec![
                ("a1", beat(BeatState::Closing, DIGEST, NOW - 10)),
                ("a2", beat(BeatState::Connected, DIGEST, NOW - 20)),
            ]),
            "Connected",
        ),
    ];
    for (what, beats, want) in cases {
        let (_, st) = run(&object(Some(&beats)), resolved());
        assert_eq!(cond(&st, "InboundReady").reason, want, "{what}");
        assert_eq!(
            cond(&st, "InboundReady").status,
            if want == "Connected" {
                ConditionStatus::True
            } else {
                ConditionStatus::False
            },
            "{what}"
        );
    }
}

#[test]
fn a_refusal_for_the_listeners_own_fault_turns_inbound_off_until_it_ages_out() {
    let mut st = with_beats(vec![("a1", beat(BeatState::Connected, DIGEST, NOW - 10))]);
    st.record_refusal("WrongVerified", true, NOW - 5);
    st.record_refusal("SpoolFull", false, NOW - 1);
    let obj = object(Some(&st));
    assert!(ListenerController::due(&obj, NOW + 1));
    let (_, judged) = run(&obj, resolved());
    assert_eq!(
        reason(&judged, "InboundReady"),
        (ConditionStatus::False, "DeliveriesRefused".into())
    );
    assert_eq!(judged.seen, 3);
    // Past three beats, with a fresh beat, the fault no longer counts.
    let mut later = judged.clone();
    later.record_beat("a1", beat(BeatState::Connected, DIGEST, NOW + 5_000));
    let (_, st) = run_with(
        &object(Some(&later)),
        ListenerObservations::default(),
        defaults(Some(account(true))),
        NOW + 5_010,
    );
    assert_eq!(
        reason(&st, "InboundReady"),
        (ConditionStatus::True, "Connected".into())
    );
    // One thread's full spool was never the listener's fault.
    let mut full = with_beats(vec![("a1", beat(BeatState::Connected, DIGEST, NOW - 10))]);
    full.refused = Some(Refused {
        counts: BTreeMap::from([("SpoolFull".to_string(), 7)]),
        last_fault_at: None,
    });
    let (_, st) = run(&object(Some(&full)), resolved());
    assert_eq!(cond(&st, "InboundReady").reason, "Connected");
}

#[test]
fn a_listener_whose_account_left_the_config_is_deleted_and_a_quiet_one_writes_nothing() {
    let (_, st) = run(&object(None), resolved());
    let obj = object(Some(&st));
    let input = ListenerInput {
        object: &obj,
        observations: ListenerObservations::default(),
        now: NOW + 1,
        defaults: defaults(None),
    };
    assert!(ListenerController.probes(&input).unwrap().is_empty());
    let plan = ListenerController.reconcile(&input).unwrap();
    assert!(plan.delete);
    assert_eq!(plan.status, None);
    // Nothing new, nothing asked: no write.
    let (plan, _) = run_with(
        &obj,
        ListenerObservations::default(),
        defaults(Some(account(true))),
        NOW + 1,
    );
    assert_eq!(plan, Plan::default());
    let mut deleting = obj.clone();
    deleting.metadata.deletion_timestamp = Some(NOW);
    assert!(!ListenerController::due(&deleting, NOW + POLL_MS));
    let (plan, _) = run(&deleting, resolved());
    assert_eq!(plan, Plan::default());
    let mut bad = object(None);
    bad.status = json!({"beats": "many"});
    assert!(ListenerController::due(&bad, NOW));
    let input = ListenerInput {
        object: &bad,
        observations: ListenerObservations::default(),
        now: NOW,
        defaults: defaults(Some(account(true))),
    };
    assert!(matches!(
        ListenerController.reconcile(&input),
        Err(fleet_api::StoreError::Invalid(_))
    ));
}
