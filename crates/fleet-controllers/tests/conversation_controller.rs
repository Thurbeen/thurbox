//! ConversationController as a pure function: a Conversation, the spooled
//! deliveries, one history read and the tied Task go in; a plan comes out,
//! whose status is the whole mirror with its spool drained. Each case is
//! one transition of the mirror, or one reason nothing moves.

use fleet_api::condition::{Condition, ConditionStatus};
use fleet_api::conversation::{
    ConversationStatus, MESSAGE_CAP, Phase, Retiring, SEEN_EVENTS_CAP, StoredMessage, ThreadDir,
    Tie, TieState, Via, retire_dir_key, retire_task_key,
};
use fleet_api::gateway::{
    Author, AuthorKind, Capabilities, ConversationRef, Delivery, Event, GatewayDefaults,
    HISTORY_FIRST_READ, History, HistoryObservation, HistoryQuery, Marker, Mention, Message,
    RouteConfig, Starter, Uncertainty, Verified,
};
use fleet_api::plan::{Plan, Probe};
use fleet_api::task::Workspace;
use fleet_api::{Object, Observation};
use fleet_controllers::{ConversationController, ConversationInput, ConversationObservations};
use serde_json::{Value, json};

const NOW: u64 = 1_730_001_000_000;
const T0: u64 = 1_730_000_000_000;
const ACCOUNT: &str = "fake-main";
const NAME: &str = "conv-3f9a1c0d2b7e";
const ALEX: &str = "U0123ABCD";
const SAM: &str = "U0456EFGH";
const CREATED_AT: u64 = NOW - 5_000;
const BRIEF: &str = "Answer the thread.\n";

fn thread() -> ConversationRef {
    ConversationRef::split("fake:C0123ABCD:1730000000000").unwrap()
}

fn route() -> RouteConfig {
    RouteConfig {
        account: ACCOUNT.into(),
        channels: vec!["C0123ABCD".into()],
        mention: Mention::Required,
        allow: vec!["alex".into()],
        allow_bots: Vec::new(),
        task_config: "conversation".into(),
        brief: "/etc/fleet/briefs/conversation.md".into(),
        workspace: Workspace::at("/work/conversations"),
        refusal: None,
    }
}

fn defaults() -> GatewayDefaults {
    GatewayDefaults {
        poll_ms: 1_000,
        active_ms: 100_000,
        retain_ms: 10_000,
        people: std::collections::BTreeMap::from([(
            "alex".to_string(),
            std::collections::BTreeMap::from([(ACCOUNT.to_string(), ALEX.to_string())]),
        )]),
        ..GatewayDefaults::default()
    }
    .for_route(Some(route()), Some(BRIEF.to_string()))
}

fn person(id: &str, at: u64, text: &str) -> Message {
    Message {
        id: at.to_string(),
        author: Author {
            id: id.into(),
            name: "Someone".into(),
            kind: AuthorKind::Person,
        },
        mentions_fleet: text.contains("<@"),
        at,
        text: text.into(),
        key: None,
        starts_thread: false,
    }
}

fn delivery(id: &str, event: Event) -> Delivery {
    Delivery {
        id: id.into(),
        verified: Verified::Connection,
        received_at: NOW - 500,
        event,
    }
}

fn message(id: &str, m: Message) -> Delivery {
    delivery(
        id,
        Event::Message {
            conversation: thread(),
            message: m,
        },
    )
}

fn edited(id: &str, message_id: &str, text: &str, edited_at: u64) -> Delivery {
    delivery(
        id,
        Event::Edited {
            conversation: thread(),
            id: message_id.into(),
            text: text.into(),
            edited_at,
        },
    )
}

fn removed(id: &str, message_id: &str) -> Delivery {
    delivery(
        id,
        Event::Removed {
            conversation: thread(),
            id: message_id.into(),
        },
    )
}

/// A message as the mirror stores it, numbered in `seq`, delivered by the
/// listener.
fn stored(m: Message, seq: u64) -> StoredMessage {
    StoredMessage {
        id: m.id,
        seq,
        first_seq: seq,
        position: 0,
        at: m.at,
        edited_at: None,
        author: m.author,
        mentions_fleet: m.mentions_fleet,
        text: m.text,
        key: m.key,
        via: Via {
            verified: Verified::Connection,
            received_at: m.at + 100,
        },
    }
}

fn object(status: Option<&ConversationStatus>) -> Object {
    let mut v = json!({
        "apiVersion": "fleet/v1alpha1",
        "kind": "Conversation",
        "metadata": {"name": NAME, "uid": "c7", "generation": 1, "resourceVersion": 4,
                     "creationTimestamp": CREATED_AT},
        "spec": {"account": ACCOUNT, "address": "fake:C0123ABCD:1730000000000", "route": "eng"}
    });
    if let Some(st) = status {
        v["status"] = serde_json::to_value(st).unwrap();
    }
    serde_json::from_value(v).unwrap()
}

/// A mirror whose address is parsed, with `received` spooled.
fn mirrored(received: Vec<Delivery>) -> ConversationStatus {
    ConversationStatus {
        reference: Some(thread()),
        received,
        ..ConversationStatus::default()
    }
}

fn history(messages: Vec<Message>) -> HistoryObservation {
    let starter = messages.first().map(Starter::message);
    HistoryObservation::Known(History {
        messages,
        left_out: 0,
        starter,
    })
}

fn caps(history: bool) -> Observation<Capabilities> {
    Observation::Known(Capabilities {
        history,
        marker: Marker::Metadata,
        unmentioned: true,
        threads: true,
        reactions: false,
    })
}

fn run_with(
    obj: &Object,
    obs: ConversationObservations,
    defaults: GatewayDefaults,
    now: u64,
) -> (Plan, ConversationStatus) {
    let input = ConversationInput {
        object: obj,
        observations: obs,
        now,
        defaults,
    };
    let plan = ConversationController.reconcile(&input).unwrap();
    let st = match &plan.status {
        Some(v) => serde_json::from_value(v.clone()).unwrap(),
        None => serde_json::from_value(obj.status.clone()).unwrap_or_default(),
    };
    (plan, st)
}

fn run(obj: &Object, obs: ConversationObservations) -> (Plan, ConversationStatus) {
    run_with(obj, obs, defaults(), NOW)
}

fn probes(obj: &Object, obs: ConversationObservations) -> Vec<Probe> {
    let input = ConversationInput {
        object: obj,
        observations: obs,
        now: NOW,
        defaults: defaults(),
    };
    ConversationController.probes(&input).unwrap()
}

fn cond<'a>(st: &'a ConversationStatus, t: &str) -> &'a Condition {
    st.conditions
        .iter()
        .find(|c| c.type_ == t)
        .unwrap_or_else(|| panic!("no {t} in {st:#?}"))
}

fn seqs(st: &ConversationStatus) -> Vec<(&str, u64, u64, u64)> {
    st.messages
        .iter()
        .map(|m| (m.id.as_str(), m.seq, m.first_seq, m.position))
        .collect()
}

#[test]
fn a_fresh_conversation_wants_its_ref_then_capabilities_then_the_first_read() {
    let obj = object(None);
    assert!(ConversationController::due(&obj, NOW));
    assert!(
        probes(&obj, ConversationObservations::default()).is_empty(),
        "nothing to ask without a ref"
    );
    let parsed = ConversationObservations {
        parsed: Some(Observation::Known(thread())),
        ..ConversationObservations::default()
    };
    assert_eq!(
        probes(&obj, parsed.clone()),
        vec![Probe::Capabilities {
            account: ACCOUNT.into()
        }]
    );
    let with_caps = ConversationObservations {
        capabilities: Some(caps(true)),
        ..parsed.clone()
    };
    assert_eq!(
        probes(&obj, with_caps.clone()),
        vec![Probe::History {
            account: ACCOUNT.into(),
            conversation: thread(),
            query: HistoryQuery::After {
                id: None,
                limit: HISTORY_FIRST_READ
            }
        }]
    );
    let without_history = ConversationObservations {
        capabilities: Some(caps(false)),
        ..parsed.clone()
    };
    assert!(
        probes(&obj, without_history).is_empty(),
        "a platform without history is not read"
    );
    let unknown_caps = ConversationObservations {
        capabilities: Some(Observation::unknown("lost")),
        ..parsed
    };
    assert!(probes(&obj, unknown_caps).is_empty());
    let answered = ConversationObservations {
        history: Some(history(Vec::new())),
        ..with_caps
    };
    assert!(probes(&obj, answered).is_empty(), "everything is answered");
}

#[test]
fn an_address_the_adapter_cannot_read_is_unknown_and_tried_again() {
    let obj = object(None);
    let (plan, st) = run(
        &obj,
        ConversationObservations {
            parsed: Some(Observation::unknown("not one token")),
            ..ConversationObservations::default()
        },
    );
    assert!(plan.status.is_some());
    assert_eq!(st.reference, None);
    assert_eq!(cond(&st, "Reachable").status, ConditionStatus::Unknown);
    assert_eq!(cond(&st, "Reachable").reason, "AddressNotParsed");
    assert_eq!(st.phase, Phase::Untied);
}

#[test]
fn the_first_pass_numbers_the_history_and_the_spool_in_one_revision_oldest_first() {
    let mention = person(ALEX, T0 + 300_000, "<@B0FLEET001> can you look into it?");
    let obj = object(Some(&mirrored(vec![message("Ev01", mention.clone())])));
    let obs = ConversationObservations {
        capabilities: Some(caps(true)),
        history: Some(history(vec![
            person(SAM, T0, "The nightly export failed again."),
            person(SAM, T0 + 100_000, "Anyone?"),
        ])),
        ..ConversationObservations::default()
    };
    let (plan, st) = run(&obj, obs);
    assert!(!plan.delete);
    assert_eq!(st.revision, 1);
    assert_eq!(
        seqs(&st),
        vec![
            ("1730000000000", 1, 1, 0),
            ("1730000100000", 1, 1, 1),
            ("1730000300000", 1, 1, 2)
        ]
    );
    assert_eq!(st.messages[0].via.verified, Verified::History);
    assert_eq!(st.messages[0].via.received_at, NOW);
    assert_eq!(st.messages[2].via.verified, Verified::Connection);
    assert_eq!(st.messages[2].via.received_at, NOW - 500);
    assert!(st.messages[2].mentions_fleet);
    assert_eq!(
        st.starter,
        Some(Starter {
            id: Some("1730000000000".into()),
            kind: Some(AuthorKind::Person),
            key: None,
            removed: false
        })
    );
    assert_eq!(st.received, Vec::new(), "the spool is drained");
    assert_eq!(st.seen_events, vec!["Ev01".to_string()]);
    assert_eq!(st.last_message_at, Some(T0 + 300_000));
    assert_eq!(st.observed_at, Some(NOW));
    assert_eq!(st.dropped_before, 0);
    assert_eq!(cond(&st, "Reachable").reason, "Observed");
    // Alex's mention is admitted and meets the mention rule: the thread's
    // first Task is being created.
    assert_eq!(cond(&st, "Tied").status, ConditionStatus::False);
    assert_eq!(cond(&st, "Tied").reason, "Starting");
    assert_eq!(plan.creates.len(), 1);
    assert_eq!(st.task.as_ref().map(|t| t.state), Some(TieState::Pending));
    assert!(st.conditions.iter().all(|c| c.type_ != "NeedsAttention"));
    // Pending: the next look is one interval away, for the receipt.
    assert_eq!(st.not_before, Some(NOW + 1_000));
    // Untied with nothing to start, the next look is the end of its
    // retention, counted from the newer of its last message and its
    // creation.
    let quiet = object(Some(&mirrored(vec![message(
        "Ev01",
        person(SAM, T0 + 300_000, "<@B0FLEET001> can you look into it?"),
    )])));
    let (_, st) = run(
        &quiet,
        ConversationObservations {
            capabilities: Some(caps(true)),
            history: Some(history(vec![person(SAM, T0, "first")])),
            ..ConversationObservations::default()
        },
    );
    assert_eq!(cond(&st, "Tied").reason, "Untied");
    assert_eq!(st.not_before, Some(CREATED_AT + 10_000));
}

#[test]
fn the_spool_is_numbered_even_while_the_history_is_unknown() {
    let obj = object(Some(&mirrored(vec![message(
        "Ev01",
        person(ALEX, T0 + 300_000, "<@B0FLEET001> hi"),
    )])));
    let obs = ConversationObservations {
        capabilities: Some(caps(true)),
        history: Some(HistoryObservation::Unknown {
            kind: Uncertainty::Throttled,
            why: "slow down".into(),
            retry_after: Some(2_500),
        }),
        ..ConversationObservations::default()
    };
    let (_, st) = run(&obj, obs);
    assert_eq!(st.revision, 1);
    assert_eq!(seqs(&st), vec![("1730000300000", 1, 1, 0)]);
    assert_eq!(cond(&st, "Reachable").status, ConditionStatus::Unknown);
    assert_eq!(cond(&st, "Reachable").reason, "Throttled");
    assert_eq!(st.observed_at, None, "the first read is still owed");
    assert_eq!(
        st.not_before,
        Some(NOW + 2_500),
        "the read is retried as the platform asked, before the retention"
    );
    // The history later lands: its messages take the next revision, the
    // mention is one message still.
    let again = object(Some(&st));
    let (_, st) = run(
        &again,
        ConversationObservations {
            capabilities: Some(caps(true)),
            history: Some(history(vec![
                person(SAM, T0, "first"),
                person(ALEX, T0 + 300_000, "<@B0FLEET001> hi"),
            ])),
            ..ConversationObservations::default()
        },
    );
    assert_eq!(st.revision, 2);
    assert_eq!(
        seqs(&st),
        vec![("1730000000000", 2, 2, 0), ("1730000300000", 1, 1, 0)]
    );
    assert_eq!(st.observed_at, Some(NOW));
}

#[test]
fn a_platform_without_history_mirrors_the_events_alone() {
    let mut starter = person(ALEX, T0, "<@B0FLEET001> hi");
    starter.starts_thread = true;
    let obj = object(Some(&mirrored(vec![message("Ev01", starter)])));
    let (_, st) = run(
        &obj,
        ConversationObservations {
            capabilities: Some(caps(true)),
            history: Some(HistoryObservation::Unsupported),
            ..ConversationObservations::default()
        },
    );
    assert_eq!(st.revision, 1);
    assert_eq!(cond(&st, "Reachable").status, ConditionStatus::False);
    assert_eq!(cond(&st, "Reachable").reason, "Unsupported");
    assert_eq!(
        st.starter.as_ref().and_then(|s| s.id.clone()),
        Some("1730000000000".into()),
        "a message that starts its thread is the starter"
    );
    assert_eq!(st.observed_at, Some(NOW), "nothing is owed");
    let again = object(Some(&st));
    assert_eq!(
        probes(
            &again,
            ConversationObservations {
                capabilities: Some(caps(false)),
                ..ConversationObservations::default()
            }
        ),
        vec![Probe::Object {
            kind: "Task".into(),
            name: format!("{NAME}-1")
        }],
        "no history is read; the pending Task is asked about"
    );
}

#[test]
fn a_delivery_seen_twice_or_from_history_too_is_one_message() {
    let m = person(ALEX, T0 + 300_000, "<@B0FLEET001> hi");
    let obj = object(Some(&mirrored(vec![message("Ev01", m.clone())])));
    let (_, st) = run(
        &obj,
        ConversationObservations {
            capabilities: Some(caps(true)),
            history: Some(history(vec![person(SAM, T0, "first"), m.clone()])),
            ..ConversationObservations::default()
        },
    );
    assert_eq!(st.revision, 1);
    assert_eq!(st.messages.len(), 2);
    // Redelivered after the spool was drained, and delivered again as a
    // second delivery id: neither numbers anything.
    let mut st2 = st.clone();
    st2.received = vec![message("Ev01", m.clone()), message("Ev02", m.clone())];
    let (plan, st3) = run(&object(Some(&st2)), ConversationObservations::default());
    assert_eq!(st3.revision, 1);
    assert_eq!(st3.messages, st.messages);
    assert_eq!(st3.received, Vec::new());
    assert_eq!(
        st3.seen_events,
        vec!["Ev01".to_string(), "Ev02".to_string()]
    );
    assert!(plan.status.is_some(), "the drained spool is written");
    // A quiet pass writes nothing.
    let (plan, _) = run(&object(Some(&st3)), ConversationObservations::default());
    assert_eq!(plan, Plan::default());
}

#[test]
fn a_newer_edit_moves_the_number_an_older_one_changes_nothing_and_a_removal_leaves() {
    let obj = object(Some(&mirrored(vec![message(
        "Ev01",
        person(ALEX, T0 + 300_000, "<@B0FLEET001> hi"),
    )])));
    let (_, st) = run(
        &obj,
        ConversationObservations {
            capabilities: Some(caps(true)),
            history: Some(history(vec![person(SAM, T0, "first")])),
            ..ConversationObservations::default()
        },
    );
    let mut st = st;
    st.received = vec![edited("Ev02", "1730000300000", "hi, fixed", T0 + 400_000)];
    let (_, st) = run(&object(Some(&st)), ConversationObservations::default());
    assert_eq!(st.revision, 2);
    assert_eq!(
        seqs(&st),
        vec![("1730000000000", 1, 1, 0), ("1730000300000", 2, 1, 1)]
    );
    assert_eq!(st.messages[1].text, "hi, fixed");
    assert_eq!(st.messages[1].edited_at, Some(T0 + 400_000));
    let mut older = st.clone();
    older.received = vec![edited("Ev03", "1730000300000", "stale", T0 + 350_000)];
    let (_, st) = run(&object(Some(&older)), ConversationObservations::default());
    assert_eq!(
        st.revision, 2,
        "an older edit delivered late changes nothing"
    );
    assert_eq!(st.messages[1].text, "hi, fixed");
    let mut unknown = st.clone();
    unknown.received = vec![edited("Ev04", "nope", "x", T0 + 999_000)];
    let (_, st) = run(&object(Some(&unknown)), ConversationObservations::default());
    assert_eq!(
        st.revision, 2,
        "an edit of a message never seen is drained and ignored"
    );
    let mut removing = st.clone();
    removing.received = vec![removed("Ev05", "1730000000000")];
    let (_, st) = run(
        &object(Some(&removing)),
        ConversationObservations::default(),
    );
    assert_eq!(st.revision, 3);
    assert_eq!(seqs(&st), vec![("1730000300000", 2, 1, 1)]);
    assert_eq!(st.starter, Some(Starter::removed()));
    assert_eq!(st.last_message_at, Some(T0 + 300_000));
}

#[test]
fn an_edit_arriving_with_its_message_in_one_pass_is_applied_without_a_second_number() {
    let obj = object(Some(&mirrored(vec![
        message("Ev01", person(ALEX, T0 + 300_000, "<@B0FLEET001> hi")),
        edited("Ev02", "1730000300000", "hi there", T0 + 310_000),
        message("Ev03", person(SAM, T0 + 320_000, "me too")),
        removed("Ev04", "1730000320000"),
    ])));
    let (_, st) = run(&obj, ConversationObservations::default());
    assert_eq!(st.revision, 1);
    assert_eq!(seqs(&st), vec![("1730000300000", 1, 1, 0)]);
    assert_eq!(st.messages[0].text, "hi there");
    assert_eq!(st.messages[0].edited_at, Some(T0 + 310_000));
}

#[test]
fn the_history_marks_fleets_own_posts_and_a_changed_text_as_an_edit() {
    let mut own = person("B0FLEET001", T0 + 500_000, "Looking now.");
    own.author.kind = AuthorKind::Fleet;
    own.key = Some("task/t/u7/gateway/comment/c1".into());
    let obj = object(Some(&mirrored(vec![message(
        "Ev01",
        person(ALEX, T0, "<@B0FLEET001> hi"),
    )])));
    let (_, st) = run(
        &obj,
        ConversationObservations {
            capabilities: Some(caps(true)),
            history: Some(history(vec![
                person(ALEX, T0, "<@B0FLEET001> hi"),
                own.clone(),
            ])),
            ..ConversationObservations::default()
        },
    );
    assert_eq!(st.messages.len(), 2);
    assert_eq!(st.messages[1].author.kind, AuthorKind::Fleet);
    assert_eq!(st.messages[1].key, own.key);
    // The backstop later reads a changed text: an edit, numbered anew.
    let mut tied = st.clone();
    tied.task = Some(Tie {
        name: format!("{NAME}-1"),
        uid: "t1".into(),
        n: 1,
        state: TieState::Created,
        trigger: None,
    });
    tied.phase = Phase::Tied;
    tied.observed_at = Some(NOW - 5_000);
    tied.last_message_at = Some(NOW - 50_000);
    let later = object(Some(&tied));
    let task = Observation::Known(Some(task_object("t1")));
    let ask = probes(
        &later,
        ConversationObservations {
            task: Some(task.clone()),
            capabilities: Some(caps(true)),
            ..ConversationObservations::default()
        },
    );
    assert_eq!(
        ask,
        vec![Probe::History {
            account: ACCOUNT.into(),
            conversation: thread(),
            query: HistoryQuery::After {
                id: Some("1730000500000".into()),
                limit: HISTORY_FIRST_READ
            }
        }],
        "the backstop reads after the newest message"
    );
    let mut changed = own.clone();
    changed.text = "Looking now, fixed.".into();
    let (_, st) = run(
        &later,
        ConversationObservations {
            task: Some(task),
            capabilities: Some(caps(true)),
            history: Some(history(vec![changed])),
            ..ConversationObservations::default()
        },
    );
    assert_eq!(st.revision, 2);
    assert_eq!(st.messages[1].seq, 2);
    assert_eq!(st.messages[1].first_seq, 1);
    assert_eq!(st.messages[1].text, "Looking now, fixed.");
}

fn task_object(uid: &str) -> Object {
    serde_json::from_value(json!({
        "apiVersion": "fleet/v1alpha1",
        "kind": "Task",
        "metadata": {"name": format!("{NAME}-1"), "uid": uid, "generation": 1, "resourceVersion": 9},
        "spec": {"harness": "claude-code", "brief": "b", "workspace": {"dir": "w"}}
    }))
    .unwrap()
}

fn tied() -> ConversationStatus {
    let mut st = mirrored(Vec::new());
    st.phase = Phase::Tied;
    st.task = Some(Tie {
        name: format!("{NAME}-1"),
        uid: "t1".into(),
        n: 1,
        state: TieState::Created,
        trigger: None,
    });
    st.observed_at = Some(NOW - 100);
    st.last_message_at = Some(NOW - 50_000);
    st.revision = 1;
    st
}

#[test]
fn a_tied_thread_asks_for_its_task_and_stays_tied_while_the_task_is_there_or_unknown() {
    let obj = object(Some(&tied()));
    assert_eq!(
        probes(&obj, ConversationObservations::default()),
        vec![Probe::Object {
            kind: "Task".into(),
            name: format!("{NAME}-1")
        }]
    );
    let (plan, st) = run(
        &obj,
        ConversationObservations {
            task: Some(Observation::Known(Some(task_object("t1")))),
            ..ConversationObservations::default()
        },
    );
    assert_eq!(st.phase, Phase::Tied);
    assert_eq!(cond(&st, "Tied").status, ConditionStatus::True);
    assert_eq!(cond(&st, "Tied").reason, "TaskKnown");
    assert_eq!(cond(&st, "Tied").message, format!("{NAME}-1"));
    assert_eq!(st.not_before, Some(NOW + 1_000), "the backstop cadence");
    assert!(!plan.delete);
    let (_, st) = run(
        &obj,
        ConversationObservations {
            task: Some(Observation::unknown("corrupt record")),
            ..ConversationObservations::default()
        },
    );
    assert_eq!(st.phase, Phase::Tied, "Unknown never means gone");
    assert_eq!(st.task.as_ref().unwrap().state, TieState::Created);
}

#[test]
fn a_readably_gone_task_or_another_under_its_name_orphans_the_thread() {
    let obj = object(Some(&tied()));
    let (_, st) = run(
        &obj,
        ConversationObservations {
            task: Some(Observation::Known(None)),
            ..ConversationObservations::default()
        },
    );
    assert_eq!(st.phase, Phase::Orphaned);
    assert_eq!(st.task.as_ref().unwrap().state, TieState::Gone);
    assert_eq!(cond(&st, "Tied").status, ConditionStatus::False);
    assert_eq!(cond(&st, "Tied").reason, "TaskGone");
    assert_eq!(cond(&st, "Tied").last_transition_time, NOW);
    assert_eq!(st.messages, tied().messages, "the messages are kept");
    assert_eq!(
        st.not_before,
        Some(NOW + 10_000),
        "retention counts from the orphaning, later than the last message"
    );
    let (_, st) = run(
        &obj,
        ConversationObservations {
            task: Some(Observation::Known(Some(task_object("t2")))),
            ..ConversationObservations::default()
        },
    );
    assert_eq!(st.phase, Phase::Orphaned, "another Task holds the name");
    // Orphaned: the Task is not asked about again.
    let orphan = object(Some(&st));
    assert!(probes(&orphan, ConversationObservations::default()).is_empty());
}

#[test]
fn an_orphaned_or_untied_thread_is_deleted_once_its_retention_passed() {
    let mut st = tied();
    st.phase = Phase::Orphaned;
    st.task.as_mut().unwrap().state = TieState::Gone;
    fleet_api::condition::set_condition(
        &mut st.conditions,
        "Tied",
        ConditionStatus::False,
        "TaskGone",
        "",
        1,
        NOW - 3_000,
    );
    let obj = object(Some(&st));
    let (plan, st) = run(&obj, ConversationObservations::default());
    assert!(!plan.delete, "not yet: {st:#?}");
    assert_eq!(
        st.not_before,
        Some(NOW + 7_000),
        "from the orphaning, after the creation"
    );
    let (plan, _) = run_with(
        &obj,
        ConversationObservations::default(),
        defaults(),
        NOW + 7_000,
    );
    assert!(plan.delete);
    // Untied: from the later of the last message and the creation.
    let mut untied = mirrored(Vec::new());
    untied.observed_at = Some(NOW - 100);
    untied.last_message_at = Some(CREATED_AT + 5_000);
    let (plan, st) = run(&object(Some(&untied)), ConversationObservations::default());
    assert!(!plan.delete);
    assert_eq!(st.not_before, Some(CREATED_AT + 15_000));
    let (plan, _) = run_with(
        &object(Some(&untied)),
        ConversationObservations::default(),
        defaults(),
        CREATED_AT + 15_000,
    );
    assert!(plan.delete);
    // A thread waiting for its own directory is never retired.
    let mut waiting = untied.clone();
    waiting.messages = vec![stored(person(ALEX, T0 + 300_000, "<@B0FLEET001> hi"), 1)];
    waiting.revision = 1;
    waiting.judged = 1;
    waiting.task = Some(Tie {
        name: format!("{NAME}-1"),
        uid: String::new(),
        n: 1,
        state: TieState::Busy,
        trigger: Some("1730000300000".into()),
    });
    let (plan, st) = run_with(
        &object(Some(&waiting)),
        ConversationObservations::default(),
        defaults(),
        CREATED_AT + 15_000,
    );
    assert!(!plan.delete);
    assert_eq!(st.phase, Phase::Untied);
    assert_eq!(st.not_before, Some(CREATED_AT + 15_000 + 1_000));
}

#[test]
fn an_inactive_tied_thread_is_not_read_and_a_quiet_one_writes_nothing() {
    let mut st = tied();
    st.last_message_at = Some(NOW - 200_000);
    st.observed_at = Some(NOW - 5_000);
    let obj = object(Some(&st));
    let ask = probes(
        &obj,
        ConversationObservations {
            task: Some(Observation::Known(Some(task_object("t1")))),
            capabilities: Some(caps(true)),
            ..ConversationObservations::default()
        },
    );
    assert!(ask.is_empty(), "past activeMs the backstop stops: {ask:?}");
    let mut quiet = st.clone();
    quiet.phase = Phase::Tied;
    fleet_api::condition::set_condition(
        &mut quiet.conditions,
        "Tied",
        ConditionStatus::True,
        "TaskKnown",
        format!("{NAME}-1"),
        1,
        NOW - 50_000,
    );
    quiet.not_before = Some(NOW + 1_000);
    let (plan, _) = run(
        &object(Some(&quiet)),
        ConversationObservations {
            task: Some(Observation::Known(Some(task_object("t1")))),
            ..ConversationObservations::default()
        },
    );
    assert_eq!(plan, Plan::default(), "nothing new: no write");
    assert!(!ConversationController::due(&object(Some(&quiet)), NOW));
    let mut spooled = quiet.clone();
    spooled.received = vec![message("Ev09", person(SAM, NOW - 10, "ping"))];
    assert!(ConversationController::due(&object(Some(&spooled)), NOW));
}

#[test]
fn more_than_the_cap_rotates_the_oldest_and_raises_dropped_before() {
    let mut received: Vec<Delivery> = (0..=MESSAGE_CAP as u64)
        .map(|i| {
            message(
                &format!("Ev{i:03}"),
                person(ALEX, T0 + i * 1000, &format!("m{i}")),
            )
        })
        .collect();
    received.push(edited(
        "EvX",
        "1730000000000",
        "edited oldest",
        T0 + 999_999,
    ));
    let obj = object(Some(&mirrored(received)));
    let (_, st) = run(&obj, ConversationObservations::default());
    assert_eq!(st.messages.len(), MESSAGE_CAP);
    assert_eq!(st.messages[0].id, "1730000001000");
    assert_eq!(st.dropped_before, 1, "the rotated message's number");
    assert_eq!(st.seen_events.len(), MESSAGE_CAP + 2);
    // The seen delivery ids are bounded too.
    let mut many = st.clone();
    many.seen_events = (0..SEEN_EVENTS_CAP as u64)
        .map(|i| format!("old{i}"))
        .collect();
    many.received = vec![message("new", person(SAM, T0 + 5_000_000, "late"))];
    let (_, st) = run(&object(Some(&many)), ConversationObservations::default());
    assert_eq!(st.seen_events.len(), SEEN_EVENTS_CAP);
    assert_eq!(st.seen_events.last().map(String::as_str), Some("new"));
    assert_eq!(st.seen_events.first().map(String::as_str), Some("old1"));
}

#[test]
fn a_route_that_left_the_config_and_a_mentions_only_platform_need_attention() {
    let obj = object(Some(&mirrored(Vec::new())));
    let (_, st) = run_with(
        &obj,
        ConversationObservations::default(),
        defaults().for_route(None, None),
        NOW,
    );
    let c = cond(&st, "NeedsAttention");
    assert_eq!(
        (c.status, c.reason.as_str()),
        (ConditionStatus::True, "RouteUnknown")
    );
    let (_, st) = run(
        &object(Some(&st)),
        ConversationObservations {
            capabilities: Some(Observation::Known(Capabilities {
                unmentioned: false,
                ..caps(true).known().copied().unwrap()
            })),
            ..ConversationObservations::default()
        },
    );
    let c = cond(&st, "NeedsAttention");
    assert_eq!(
        (c.status, c.reason.as_str()),
        (ConditionStatus::True, "MentionsOnly")
    );
    assert!(c.message.contains("only mentions"));
    let (_, st) = run(
        &object(Some(&st)),
        ConversationObservations {
            capabilities: Some(caps(true)),
            ..ConversationObservations::default()
        },
    );
    let c = cond(&st, "NeedsAttention");
    assert_eq!(
        (c.status, c.reason.as_str()),
        (ConditionStatus::False, "Resolved")
    );
}

#[test]
fn a_deleting_object_asks_nothing_and_plans_nothing_and_malformed_status_is_invalid() {
    let mut obj = object(Some(&mirrored(vec![message(
        "Ev01",
        person(ALEX, T0, "<@B0FLEET001> hi"),
    )])));
    obj.metadata.deletion_timestamp = Some(NOW);
    assert!(!ConversationController::due(&obj, NOW));
    assert!(probes(&obj, ConversationObservations::default()).is_empty());
    let (plan, _) = run(&obj, ConversationObservations::default());
    assert_eq!(plan, Plan::default());
    let mut bad = object(None);
    bad.status = json!({"phase": "Sideways"});
    assert!(
        ConversationController::due(&bad, NOW),
        "reported, not skipped"
    );
    let input = ConversationInput {
        object: &bad,
        observations: ConversationObservations::default(),
        now: NOW,
        defaults: defaults(),
    };
    assert!(matches!(
        ConversationController.reconcile(&input),
        Err(fleet_api::StoreError::Invalid(_))
    ));
    let mut bad_spec = object(None);
    bad_spec.spec = json!({"account": "a"});
    let input = ConversationInput {
        object: &bad_spec,
        observations: ConversationObservations::default(),
        now: NOW,
        defaults: defaults(),
    };
    assert!(ConversationController.probes(&input).is_err());
    let _: Value = json!(null);
}

// ----------------------------------------------------------------------
// Admission, the create and its receipts, the busy tie and the refusals.
// ----------------------------------------------------------------------

use fleet_api::conversation::{EffectRecord, REFUSALS_CAP, busy_key, refusal_key, task_key};
use fleet_api::gateway::AuthorKind as Kind;
use fleet_api::plan::{Action, EffectOutcome, Intent, Receipt};

const UID: &str = "c7";

fn receipt(outcome: EffectOutcome) -> Receipt {
    Receipt {
        action: "x".into(),
        at: NOW - 1_000,
        outcome: Observation::Known(outcome),
    }
}

/// A Task under the thread's name carrying this thread's uid label, or
/// another's.
fn conversation_task(uid: &str, conv_uid: &str) -> Object {
    let mut t = task_object(uid);
    t.metadata
        .labels
        .insert("gateway.fleet.dev/conversation-uid".into(), conv_uid.into());
    t
}

/// A numbered Untied mirror holding `messages`, all judged.
fn judged(messages: Vec<StoredMessage>) -> ConversationStatus {
    let revision = messages.iter().map(|m| m.seq).max().unwrap_or(0);
    let mut st = mirrored(Vec::new());
    st.observed_at = Some(NOW - 100);
    st.last_message_at = messages.iter().map(|m| m.at).max();
    st.starter = messages.first().map(|m| Starter {
        id: Some(m.id.clone()),
        kind: Some(m.author.kind),
        key: m.key.clone(),
        removed: false,
    });
    st.messages = messages;
    st.revision = revision;
    st.judged = revision;
    st
}

fn create_key() -> String {
    task_key(UID, 1)
}

#[test]
fn a_repositories_route_starts_its_task_without_a_workspace_precondition() {
    let workspace: Workspace = serde_json::from_value(json!({"repositories": [
        {"name": "app", "url": "https://example.invalid/app", "ref": "main"}
    ]}))
    .unwrap();
    let defaults = GatewayDefaults {
        route: Some(RouteConfig {
            workspace: workspace.clone(),
            ..route()
        }),
        ..defaults()
    };
    let obj = object(Some(&mirrored(vec![message(
        "Ev01",
        person(ALEX, T0 + 300_000, "<@B0FLEET001> can you look into it?"),
    )])));
    let (plan, _) = run_with(
        &obj,
        ConversationObservations {
            capabilities: Some(caps(true)),
            history: Some(history(Vec::new())),
            ..ConversationObservations::default()
        },
        defaults,
        NOW,
    );
    assert_eq!(plan.creates.len(), 1, "{plan:#?}");
    let create = &plan.creates[0];
    assert_eq!(create.exclusive, None, "each Task gets its own workspace");
    assert_eq!(
        create.object.spec["workspace"],
        serde_json::to_value(&workspace).unwrap()
    );
}

#[test]
fn an_admitted_mention_starts_the_threads_first_task_as_the_interface_shows_it() {
    let obj = object(Some(&mirrored(vec![message(
        "Ev01",
        person(ALEX, T0 + 300_000, "<@B0FLEET001> can you look into it?"),
    )])));
    let (plan, st) = run(
        &obj,
        ConversationObservations {
            capabilities: Some(caps(true)),
            history: Some(history(vec![person(
                SAM,
                T0,
                "The nightly export failed again.",
            )])),
            ..ConversationObservations::default()
        },
    );
    assert_eq!(plan.creates.len(), 1, "{plan:#?}");
    let create = &plan.creates[0];
    assert_eq!(create.key, create_key());
    // The route's directory is a base: the thread gets its own below it,
    // and the precondition guards only that one.
    assert_eq!(
        create.exclusive.as_ref().map(|e| e.workspace.as_str()),
        Some("/work/conversations/conv-3f9a1c0d2b7e")
    );
    let expected: Object = serde_json::from_value(json!({
        "apiVersion": "fleet/v1alpha1",
        "kind": "Task",
        "metadata": {
            "name": format!("{NAME}-1"),
            "labels": {
                "gateway.fleet.dev/conversation": NAME,
                "gateway.fleet.dev/conversation-uid": UID,
                "gateway.fleet.dev/route": "eng",
            }
        },
        "spec": {
            "configRef": {"name": "conversation"},
            "brief": BRIEF,
            "workspace": {"dir": "/work/conversations/conv-3f9a1c0d2b7e"},
            "dispatch": true,
        }
    }))
    .unwrap();
    assert_eq!(create.object, expected);
    assert!(plan.effects.is_empty(), "nothing is posted");
    assert_eq!(
        st.task,
        Some(Tie {
            name: format!("{NAME}-1"),
            uid: String::new(),
            n: 1,
            state: TieState::Pending,
            trigger: Some("1730000300000".into()),
        })
    );
    let record = &st.effects[&create_key()];
    assert_eq!(record.intent.action, "create");
    assert_eq!(record.intent.at, NOW);
    assert_eq!(record.receipt, None);
    assert_eq!(record.attempts, 1);
    assert_eq!(st.judged, 1);
    assert_eq!(st.phase, Phase::Untied, "until the receipt lands");
    assert_eq!(cond(&st, "Tied").reason, "Starting");
    // With the receipt: Tied, and the trigger is let go.
    let mut with_receipt = st.clone();
    with_receipt.effects.get_mut(&create_key()).unwrap().receipt =
        Some(receipt(EffectOutcome::Created {
            uid: "t1".into(),
            adopted: false,
        }));
    let pending = object(Some(&with_receipt));
    assert!(
        ConversationController::due(&pending, NOW - 10),
        "the receipt makes it due"
    );
    assert_eq!(
        probes(&pending, ConversationObservations::default()),
        vec![
            Probe::Object {
                kind: "Task".into(),
                name: format!("{NAME}-1")
            },
            Probe::Capabilities {
                account: ACCOUNT.into()
            }
        ]
    );
    let (plan, st) = run(
        &pending,
        ConversationObservations {
            task: Some(Observation::Known(Some(task_object("t1")))),
            ..ConversationObservations::default()
        },
    );
    assert!(plan.creates.is_empty());
    assert_eq!(st.phase, Phase::Tied);
    assert_eq!(st.task.as_ref().unwrap().uid, "t1");
    assert_eq!(st.task.as_ref().unwrap().state, TieState::Created);
    assert_eq!(st.task.as_ref().unwrap().trigger, None);
    assert_eq!(cond(&st, "Tied").reason, "TaskKnown");
    // A later mention in the tied thread starts nothing more.
    let mut later = st.clone();
    later.received = vec![message(
        "Ev02",
        person(ALEX, T0 + 400_000, "<@B0FLEET001> and this?"),
    )];
    let (plan, st) = run(
        &object(Some(&later)),
        ConversationObservations {
            task: Some(Observation::Known(Some(task_object("t1")))),
            ..ConversationObservations::default()
        },
    );
    assert!(plan.creates.is_empty());
    assert_eq!(st.task.as_ref().unwrap().n, 1);
    assert_eq!(st.judged, 2);
}

#[test]
fn only_a_delivered_message_that_meets_the_mention_rule_starts_a_task() {
    // A mention read from history alone is not recovered.
    let obj = object(Some(&mirrored(Vec::new())));
    let (plan, st) = run(
        &obj,
        ConversationObservations {
            capabilities: Some(caps(true)),
            history: Some(history(vec![
                person(SAM, T0, "first"),
                person(ALEX, T0 + 300_000, "<@B0FLEET001> hi"),
            ])),
            ..ConversationObservations::default()
        },
    );
    assert!(plan.creates.is_empty(), "{plan:#?}");
    assert_eq!(st.task, None);
    assert_eq!(st.judged, 1);
    // Delivered and read from history in one pass: one message, delivered.
    let m = person(ALEX, T0 + 300_000, "<@B0FLEET001> hi");
    let obj = object(Some(&mirrored(vec![message("Ev01", m.clone())])));
    let (plan, st) = run(
        &obj,
        ConversationObservations {
            capabilities: Some(caps(true)),
            history: Some(history(vec![person(SAM, T0, "first"), m])),
            ..ConversationObservations::default()
        },
    );
    assert_eq!(plan.creates.len(), 1);
    assert_eq!(st.messages[1].via.verified, Verified::Connection);
    // An admitted message without a mention starts nothing in an Untied
    // thread; a stranger's mention starts nothing either.
    for m in [
        person(ALEX, T0 + 300_000, "no mention"),
        person(SAM, T0 + 300_000, "<@B0FLEET001> hi"),
    ] {
        let obj = object(Some(&mirrored(vec![message("Ev01", m)])));
        let (plan, st) = run(
            &obj,
            ConversationObservations {
                capabilities: Some(caps(true)),
                history: Some(history(vec![person(SAM, T0, "first")])),
                ..ConversationObservations::default()
            },
        );
        assert!(plan.creates.is_empty());
        assert_eq!(st.task, None);
    }
    // `top-level`: the message that starts the thread, mention or not.
    let mut top = route();
    top.mention = Mention::TopLevel;
    top.allow_bots = vec!["B0123ABCD".into()];
    let defaults = defaults().for_route(Some(top), Some(BRIEF.to_string()));
    let mut alert = person("B0123ABCD", T0, "disk full on db-3");
    alert.author.kind = Kind::Bot;
    alert.starts_thread = true;
    let obj = object(Some(&mirrored(vec![message("Ev01", alert.clone())])));
    let (plan, _) = run_with(
        &obj,
        ConversationObservations {
            capabilities: Some(caps(true)),
            history: Some(history(vec![alert])),
            ..ConversationObservations::default()
        },
        defaults.clone(),
        NOW,
    );
    assert_eq!(plan.creates.len(), 1, "a top-level post starts a Task");
    let obj = object(Some(&mirrored(vec![message(
        "Ev01",
        person(ALEX, T0 + 300_000, "<@B0FLEET001> reply in thread"),
    )])));
    let (plan, _) = run_with(
        &obj,
        ConversationObservations {
            capabilities: Some(caps(true)),
            history: Some(history(vec![person(SAM, T0, "first")])),
            ..ConversationObservations::default()
        },
        defaults,
        NOW,
    );
    assert!(plan.creates.is_empty(), "a reply does not, under top-level");
}

#[test]
fn judging_waits_for_the_starter_where_the_account_has_history() {
    let m = person(ALEX, T0 + 300_000, "<@B0FLEET001> hi");
    let obj = object(Some(&mirrored(vec![message("Ev01", m.clone())])));
    let (plan, st) = run(
        &obj,
        ConversationObservations {
            capabilities: Some(caps(true)),
            history: Some(HistoryObservation::Unknown {
                kind: Uncertainty::Throttled,
                why: "slow down".into(),
                retry_after: Some(2_500),
            }),
            ..ConversationObservations::default()
        },
    );
    assert!(plan.creates.is_empty(), "the starter is not known yet");
    assert_eq!(st.judged, 0);
    assert_eq!(st.revision, 1);
    // The read lands: the message is judged then.
    let (plan, st) = run(
        &object(Some(&st)),
        ConversationObservations {
            capabilities: Some(caps(true)),
            history: Some(history(vec![person(SAM, T0, "first"), m.clone()])),
            ..ConversationObservations::default()
        },
    );
    assert_eq!(plan.creates.len(), 1);
    assert_eq!(st.judged, 2);
    // Without history, nothing is owed: judged at once.
    let obj = object(Some(&mirrored(vec![message("Ev01", m)])));
    let (plan, _) = run(
        &obj,
        ConversationObservations {
            capabilities: Some(caps(false)),
            ..ConversationObservations::default()
        },
    );
    assert_eq!(plan.creates.len(), 1);
}

#[test]
fn a_strangers_mention_earns_one_refusal_per_author_up_to_the_cap() {
    let obj = object(Some(&mirrored(vec![
        message(
            "Ev01",
            person("U0999ZZZZ", T0 + 300_000, "<@B0FLEET001> hi"),
        ),
        message(
            "Ev02",
            person("U0999ZZZZ", T0 + 310_000, "<@B0FLEET001> hello?"),
        ),
        message(
            "Ev03",
            person("U0888YYYY", T0 + 320_000, "<@B0FLEET001> me too"),
        ),
        message("Ev04", person("U0777XXXX", T0 + 330_000, "no mention")),
    ])));
    let (plan, st) = run(
        &obj,
        ConversationObservations {
            capabilities: Some(caps(true)),
            history: Some(history(vec![person(SAM, T0, "first")])),
            ..ConversationObservations::default()
        },
    );
    assert!(plan.creates.is_empty());
    assert_eq!(
        st.refusals,
        vec!["U0999ZZZZ".to_string(), "U0888YYYY".to_string()]
    );
    assert_eq!(plan.effects.len(), 2, "{plan:#?}");
    let first = refusal_key(UID, "U0999ZZZZ");
    assert_eq!(plan.effects[0].key, first);
    assert_eq!(
        plan.effects[0].action,
        Action::Post {
            account: ACCOUNT.into(),
            conversation: thread(),
            text: fleet_api::gateway::DEFAULT_REFUSAL.into(),
        }
    );
    assert_eq!(st.effects[&first].intent.action, "post");
    assert_eq!(st.effects[&first].attempts, 1);
    // A bot that is not listed gets none; the route's own words are used.
    let mut worded = route();
    worded.refusal = Some("Not for you, sorry.".into());
    let mut bot = person("B0999ZZZZ", T0 + 300_000, "<@B0FLEET001> hi");
    bot.author.kind = Kind::Bot;
    let obj = object(Some(&mirrored(vec![
        message("Ev01", bot),
        message(
            "Ev02",
            person("U0999ZZZZ", T0 + 310_000, "<@B0FLEET001> hi"),
        ),
    ])));
    let (plan, st) = run_with(
        &obj,
        ConversationObservations {
            capabilities: Some(caps(true)),
            history: Some(history(vec![person(SAM, T0, "first")])),
            ..ConversationObservations::default()
        },
        defaults().for_route(Some(worded), Some(BRIEF.to_string())),
        NOW,
    );
    assert_eq!(st.refusals, vec!["U0999ZZZZ".to_string()]);
    assert!(
        matches!(&plan.effects[0].action, Action::Post { text, .. } if text == "Not for you, sorry.")
    );
    // Past the cap, no further refusal is sent or recorded.
    let mut full = judged(vec![stored(person(SAM, T0, "first"), 1)]);
    full.refusals = (0..REFUSALS_CAP).map(|i| format!("U{i:05}")).collect();
    full.received = vec![message(
        "Ev09",
        person("U0999ZZZZ", T0 + 300_000, "<@B0FLEET001> hi"),
    )];
    let (plan, st) = run(&object(Some(&full)), ConversationObservations::default());
    assert!(plan.effects.is_empty());
    assert_eq!(st.refusals.len(), REFUSALS_CAP);
    // A refused post is tried again after the delay, three times, then
    // needs attention; a Posted receipt settles it.
    let mut failed = judged(vec![stored(
        person("U0999ZZZZ", T0 + 300_000, "<@B0FLEET001> hi"),
        1,
    )]);
    failed.refusals = vec!["U0999ZZZZ".into()];
    let key = refusal_key(UID, "U0999ZZZZ");
    failed.effects.insert(
        key.clone(),
        EffectRecord {
            intent: Intent {
                action: "post".into(),
                at: NOW - 60_000,
                generation: 1,
            },
            receipt: Some(receipt(EffectOutcome::Failed { why: "no".into() })),
            attempts: 1,
            recovery: 0,
        },
    );
    let (plan, st) = run(&object(Some(&failed)), ConversationObservations::default());
    assert!(
        plan.effects.is_empty(),
        "the delay has not run out, and the marker is not known"
    );
    assert!(
        st.not_before.unwrap() <= NOW - 1_000 + fleet_controllers::RETRY_DELAY_MS,
        "looked at again by the retry: {st:#?}"
    );
    let (plan, st) = run_with(
        &object(Some(&failed)),
        ConversationObservations {
            capabilities: Some(caps(true)),
            ..ConversationObservations::default()
        },
        defaults(),
        NOW + fleet_controllers::RETRY_DELAY_MS,
    );
    assert_eq!(plan.effects.len(), 1);
    assert_eq!(st.effects[&key].attempts, 2);
    assert_eq!(st.effects[&key].receipt, None);
    let mut spent = failed.clone();
    spent.effects.get_mut(&key).unwrap().attempts = fleet_controllers::MAX_ATTEMPTS;
    let (plan, st) = run_with(
        &object(Some(&spent)),
        ConversationObservations {
            capabilities: Some(caps(true)),
            ..ConversationObservations::default()
        },
        defaults(),
        NOW + fleet_controllers::RETRY_DELAY_MS,
    );
    assert!(plan.effects.is_empty());
    assert_eq!(cond(&st, "NeedsAttention").reason, "PostFailed");
    let mut posted = failed.clone();
    posted.effects.get_mut(&key).unwrap().receipt = Some(receipt(EffectOutcome::Posted {
        message: "p1".into(),
        at: NOW - 1_000,
    }));
    let (plan, st) = run(&object(Some(&posted)), ConversationObservations::default());
    assert!(plan.effects.is_empty());
    assert!(st.conditions.iter().all(|c| c.type_ != "NeedsAttention"));
}

#[test]
fn a_post_whose_outcome_is_unknown_is_looked_for_and_never_repeated_blindly() {
    let mut unknown = judged(vec![stored(
        person("U0999ZZZZ", T0 + 300_000, "<@B0FLEET001> hi"),
        1,
    )]);
    unknown.refusals = vec!["U0999ZZZZ".into()];
    let key = refusal_key(UID, "U0999ZZZZ");
    unknown.effects.insert(
        key.clone(),
        EffectRecord {
            intent: Intent {
                action: "post".into(),
                at: NOW - 60_000,
                generation: 1,
            },
            receipt: Some(Receipt {
                action: "post".into(),
                at: NOW - 59_000,
                outcome: Observation::unknown("lost"),
            }),
            attempts: 1,
            recovery: 0,
        },
    );
    let obj = object(Some(&unknown));
    // Where the platform reads markers back: a recovery read since the intent.
    assert_eq!(
        probes(
            &obj,
            ConversationObservations {
                capabilities: Some(caps(true)),
                ..ConversationObservations::default()
            }
        ),
        vec![Probe::History {
            account: ACCOUNT.into(),
            conversation: thread(),
            query: HistoryQuery::Since { at: NOW - 60_000 },
        }]
    );
    // Found: the receipt is recovered.
    let mut own = person("B0FLEET001", T0 + 400_000, "Sorry.");
    own.author.kind = Kind::Fleet;
    own.key = Some(key.clone());
    let (plan, st) = run(
        &obj,
        ConversationObservations {
            capabilities: Some(caps(true)),
            history: Some(history(vec![own])),
            ..ConversationObservations::default()
        },
    );
    assert!(plan.effects.is_empty());
    assert_eq!(
        st.effects[&key].receipt.as_ref().unwrap().outcome,
        Observation::Known(EffectOutcome::Posted {
            message: "1730000400000".into(),
            at: T0 + 400_000
        })
    );
    assert_eq!(
        st.observed_at,
        Some(NOW - 100),
        "a recovery read is not the backstop"
    );
    // Absent from a complete read: one more attempt under the same key.
    let (plan, st) = run(
        &obj,
        ConversationObservations {
            capabilities: Some(caps(true)),
            history: Some(history(Vec::new())),
            ..ConversationObservations::default()
        },
    );
    assert_eq!(plan.effects.len(), 1);
    assert_eq!(plan.effects[0].key, key);
    assert_eq!(st.effects[&key].attempts, 2);
    // The read could not be made: counted, waited for.
    let (plan, st) = run(
        &obj,
        ConversationObservations {
            capabilities: Some(caps(true)),
            history: Some(HistoryObservation::unknown(Uncertainty::Timeout, "slow")),
            ..ConversationObservations::default()
        },
    );
    assert!(plan.effects.is_empty());
    assert_eq!(st.effects[&key].recovery, 1);
    // Where the platform cannot find it again: a person decides.
    let (plan, st) = run(
        &obj,
        ConversationObservations {
            capabilities: Some(Observation::Known(Capabilities {
                marker: Marker::None,
                ..caps(true).known().copied().unwrap()
            })),
            ..ConversationObservations::default()
        },
    );
    assert!(plan.effects.is_empty());
    assert_eq!(cond(&st, "NeedsAttention").reason, "PostUnconfirmed");
    assert!(
        probes(
            &object(Some(&st)),
            ConversationObservations {
                capabilities: Some(Observation::Known(Capabilities {
                    marker: Marker::None,
                    ..caps(true).known().copied().unwrap()
                })),
                ..ConversationObservations::default()
            }
        )
        .is_empty(),
        "no read where it cannot be found"
    );
}

#[test]
fn a_post_whose_outcome_is_unknown_is_resent_under_its_nonce_inside_the_window() {
    let nonce = |window_ms| {
        Some(Observation::Known(Capabilities {
            marker: Marker::Nonce { window_ms },
            ..caps(true).known().copied().unwrap()
        }))
    };
    let key = refusal_key(UID, "U0999ZZZZ");
    let unknown_since = |intent_at: u64, attempts: u32| {
        let mut st = judged(vec![stored(
            person("U0999ZZZZ", T0 + 300_000, "<@B0FLEET001> hi"),
            1,
        )]);
        st.refusals = vec!["U0999ZZZZ".into()];
        st.effects.insert(
            key.clone(),
            EffectRecord {
                intent: Intent {
                    action: "post".into(),
                    at: intent_at,
                    generation: 1,
                },
                receipt: Some(Receipt {
                    action: "post".into(),
                    at: intent_at + 3_000,
                    outcome: Observation::unknown("lost"),
                }),
                attempts,
                recovery: 0,
            },
        );
        object(Some(&st))
    };
    // A post with an unknown outcome asks what the platform offers, and
    // never reads history to find a nonce.
    let obj = unknown_since(NOW - 10_000, 1);
    assert_eq!(
        probes(&obj, ConversationObservations::default()),
        vec![Probe::Capabilities {
            account: ACCOUNT.into()
        }]
    );
    assert!(
        probes(
            &obj,
            ConversationObservations {
                capabilities: nonce(60_000),
                ..ConversationObservations::default()
            }
        )
        .is_empty()
    );
    // Inside the window: the same key again, the first intent kept.
    let (plan, st) = run(
        &obj,
        ConversationObservations {
            capabilities: nonce(60_000),
            ..ConversationObservations::default()
        },
    );
    assert_eq!(plan.effects.len(), 1, "{plan:?}");
    assert_eq!(plan.effects[0].key, key);
    let record = &st.effects[&key];
    assert_eq!((record.attempts, record.intent.at), (2, NOW - 10_000));
    assert!(
        st.not_before.is_some_and(|t| t <= NOW),
        "the receipt is looked at on the next pass, not the next poll: {:?}",
        st.not_before
    );
    assert!(record.receipt.is_none());
    assert!(st.conditions.iter().all(|c| c.reason != "PostUnconfirmed"));
    // Past the window, or out of attempts: a person decides.
    for obj in [
        unknown_since(NOW - 60_000, 1),
        unknown_since(NOW - 10_000, 3),
    ] {
        let (plan, st) = run(
            &obj,
            ConversationObservations {
                capabilities: nonce(60_000),
                ..ConversationObservations::default()
            },
        );
        assert!(plan.effects.is_empty(), "{plan:?}");
        assert_eq!(cond(&st, "NeedsAttention").reason, "PostUnconfirmed");
    }
    // A refused send on a nonce platform: an earlier send under the key
    // may have landed, so the retry stays inside the first send's window,
    // under the first intent, and outside it a person decides.
    let refused_since = |intent_at: u64, receipt_at: u64| {
        let mut st = judged(vec![stored(
            person("U0999ZZZZ", T0 + 300_000, "<@B0FLEET001> hi"),
            1,
        )]);
        st.refusals = vec!["U0999ZZZZ".into()];
        st.effects.insert(
            key.clone(),
            EffectRecord {
                intent: Intent {
                    action: "post".into(),
                    at: intent_at,
                    generation: 1,
                },
                receipt: Some(Receipt {
                    action: "post".into(),
                    at: receipt_at,
                    outcome: Observation::Known(EffectOutcome::Failed { why: "429".into() }),
                }),
                attempts: 1,
                recovery: 0,
            },
        );
        object(Some(&st))
    };
    let delay = fleet_controllers::RETRY_DELAY_MS;
    let inside = refused_since(NOW - delay - 1_000, NOW - delay);
    assert_eq!(
        probes(&inside, ConversationObservations::default()),
        vec![Probe::Capabilities {
            account: ACCOUNT.into()
        }],
        "a retry asks whether a nonce window binds it"
    );
    let (plan, st) = run(
        &inside,
        ConversationObservations {
            capabilities: nonce(60_000),
            ..ConversationObservations::default()
        },
    );
    assert_eq!(plan.effects.len(), 1, "{plan:?}");
    assert_eq!(st.effects[&key].intent.at, NOW - delay - 1_000);
    let outside = refused_since(NOW - 60_000, NOW - delay);
    let (plan, st) = run(
        &outside,
        ConversationObservations {
            capabilities: nonce(60_000),
            ..ConversationObservations::default()
        },
    );
    assert!(plan.effects.is_empty(), "{plan:?}");
    assert_eq!(cond(&st, "NeedsAttention").reason, "PostFailed");
    // The marker cannot be read this pass: no retry, which could slide
    // past a nonce window; a later pass decides.
    let (plan, _) = run(
        &inside,
        ConversationObservations {
            capabilities: Some(Observation::unknown("throttled")),
            ..ConversationObservations::default()
        },
    );
    assert!(plan.effects.is_empty(), "{plan:?}");
}

/// A pending tie whose create has `receipt`, with its trigger numbered.
fn pending(receipt_: Option<Receipt>, attempts: u32) -> ConversationStatus {
    let mut st = judged(vec![
        stored(person(SAM, T0, "first"), 1),
        stored(person(ALEX, T0 + 300_000, "<@B0FLEET001> hi"), 1),
    ]);
    st.task = Some(Tie {
        name: format!("{NAME}-1"),
        uid: String::new(),
        n: 1,
        state: TieState::Pending,
        trigger: Some("1730000300000".into()),
    });
    st.effects.insert(
        create_key(),
        EffectRecord {
            intent: Intent {
                action: "create".into(),
                at: NOW - 2_000,
                generation: 1,
            },
            receipt: receipt_,
            attempts,
            recovery: 0,
        },
    );
    // The pass that wrote the intent looks again one interval later; the
    // receipt landed since, in the same manager step.
    st.not_before = Some(NOW + 500);
    st
}

#[test]
fn a_busy_workspace_holds_the_tie_with_one_notice_and_the_task_starts_once_free() {
    let st = pending(
        Some(receipt(EffectOutcome::WorkspaceBusy {
            task: "holder".into(),
        })),
        1,
    );
    let (plan, st) = run(
        &object(Some(&st)),
        ConversationObservations {
            task: Some(Observation::Known(None)),
            ..ConversationObservations::default()
        },
    );
    assert!(
        plan.creates.is_empty(),
        "the receipt just landed: wait one interval"
    );
    assert_eq!(st.task.as_ref().unwrap().state, TieState::Busy);
    assert_eq!(st.phase, Phase::Untied);
    let c = cond(&st, "NeedsAttention");
    assert_eq!(
        (c.status, c.reason.as_str()),
        (ConditionStatus::True, "WorkspaceBusy")
    );
    assert!(c.message.contains("Task/holder"), "{}", c.message);
    assert_eq!(cond(&st, "Tied").reason, "Waiting");
    let notice = busy_key(UID, 1);
    assert_eq!(plan.effects.len(), 1);
    assert_eq!(plan.effects[0].key, notice);
    assert!(
        matches!(&plan.effects[0].action, Action::Post { text, .. } if text.contains("Task/holder"))
    );
    assert_eq!(
        st.not_before,
        Some(NOW + 1_000),
        "one pollMs, not the retention"
    );
    // Planned again when due, under the same key, no second notice and no
    // attempt counted.
    let mut posted = st.clone();
    posted.effects.get_mut(&notice).unwrap().receipt = Some(receipt(EffectOutcome::Posted {
        message: "p1".into(),
        at: NOW,
    }));
    let (plan, again) = run_with(
        &object(Some(&posted)),
        ConversationObservations::default(),
        defaults(),
        NOW + 1_000,
    );
    assert_eq!(plan.creates.len(), 1);
    assert_eq!(plan.creates[0].key, create_key());
    assert!(plan.effects.is_empty(), "one notice per tie number");
    assert_eq!(again.task.as_ref().unwrap().state, TieState::Pending);
    assert_eq!(again.effects[&create_key()].attempts, 1);
    assert_eq!(again.effects[&create_key()].receipt, None);
    assert_eq!(again.effects[&create_key()].intent.at, NOW + 1_000);
    // Busy again, then free: Tied, attention resolved, the notice stays one.
    let mut busy_again = again.clone();
    busy_again.effects.get_mut(&create_key()).unwrap().receipt =
        Some(receipt(EffectOutcome::WorkspaceBusy {
            task: "holder".into(),
        }));
    let (plan, st) = run_with(
        &object(Some(&busy_again)),
        ConversationObservations::default(),
        defaults(),
        NOW + 1_500,
    );
    assert!(plan.effects.is_empty());
    assert_eq!(st.task.as_ref().unwrap().state, TieState::Busy);
    let mut free = st.clone();
    free.effects.get_mut(&create_key()).unwrap().receipt = Some(receipt(EffectOutcome::Created {
        uid: "t1".into(),
        adopted: false,
    }));
    free.task.as_mut().unwrap().state = TieState::Pending;
    let (_, st) = run(
        &object(Some(&free)),
        ConversationObservations {
            task: Some(Observation::Known(Some(task_object("t1")))),
            ..ConversationObservations::default()
        },
    );
    assert_eq!(st.phase, Phase::Tied);
    assert_eq!(cond(&st, "NeedsAttention").status, ConditionStatus::False);
    // Woken early by a delivery, a busy tie is not planned before its interval.
    let mut early = posted.clone();
    early.received = vec![message("Ev05", person(SAM, T0 + 350_000, "any news?"))];
    let (plan, _) = run_with(
        &object(Some(&early)),
        ConversationObservations::default(),
        defaults(),
        NOW + 10,
    );
    assert!(plan.creates.is_empty());
}

#[test]
fn a_removed_trigger_withdraws_a_busy_tie_and_the_next_number_never_repeats() {
    let mut st = pending(
        Some(receipt(EffectOutcome::WorkspaceBusy {
            task: "holder".into(),
        })),
        1,
    );
    st.task.as_mut().unwrap().state = TieState::Busy;
    st.received = vec![removed("Ev05", "1730000300000")];
    let (plan, st) = run(&object(Some(&st)), ConversationObservations::default());
    assert!(plan.creates.is_empty());
    assert!(plan.effects.is_empty());
    assert_eq!(st.task, None, "withdrawn");
    assert_eq!(st.phase, Phase::Untied);
    assert!(
        st.conditions
            .iter()
            .all(|c| c.type_ != "NeedsAttention" || c.status == ConditionStatus::False)
    );
    assert!(st.effects.contains_key(&create_key()), "the record stays");
    // A later mention starts Task 2, never 1 again.
    let mut later = st.clone();
    later.received = vec![message(
        "Ev06",
        person(ALEX, T0 + 500_000, "<@B0FLEET001> now?"),
    )];
    let (plan, st) = run(&object(Some(&later)), ConversationObservations::default());
    assert_eq!(plan.creates.len(), 1);
    assert_eq!(plan.creates[0].key, task_key(UID, 2));
    assert_eq!(st.task.as_ref().unwrap().name, format!("{NAME}-2"));
}

#[test]
fn a_create_with_no_receipt_adopts_the_task_it_finds_and_never_creates_it_again() {
    let st = pending(None, 1);
    let obj = object(Some(&st));
    // The Task under its name, and the capabilities that decide whether
    // the request is marked with a reaction.
    assert_eq!(
        probes(&obj, ConversationObservations::default()),
        vec![
            Probe::Object {
                kind: "Task".into(),
                name: format!("{NAME}-1")
            },
            Probe::Capabilities {
                account: ACCOUNT.into()
            }
        ]
    );
    // Found with this thread's uid: adopted, the receipt recovered.
    let (plan, st) = run(
        &obj,
        ConversationObservations {
            task: Some(Observation::Known(Some(conversation_task("t1", UID)))),
            ..ConversationObservations::default()
        },
    );
    assert!(plan.creates.is_empty());
    assert_eq!(st.phase, Phase::Tied);
    assert_eq!(st.task.as_ref().unwrap().uid, "t1");
    assert_eq!(
        st.effects[&create_key()].receipt.as_ref().unwrap().outcome,
        Observation::Known(EffectOutcome::Created {
            uid: "t1".into(),
            adopted: true
        })
    );
    assert_eq!(st.effects[&create_key()].attempts, 1);
    // Another thread's Task under the name: a refusal, tried again after
    // the delay.
    let (plan, st) = run(
        &obj,
        ConversationObservations {
            task: Some(Observation::Known(Some(conversation_task("t9", "other")))),
            ..ConversationObservations::default()
        },
    );
    assert!(plan.creates.is_empty());
    assert_eq!(st.phase, Phase::Untied);
    assert!(matches!(
        &st.effects[&create_key()].receipt.as_ref().unwrap().outcome,
        Observation::Known(EffectOutcome::Failed { why }) if why.contains("not created for this thread")
    ));
    assert_eq!(st.not_before, Some(NOW + fleet_controllers::RETRY_DELAY_MS));
    // Readably absent: the create never happened, so once more under the
    // same key, counted.
    let (plan, st) = run(
        &obj,
        ConversationObservations {
            task: Some(Observation::Known(None)),
            ..ConversationObservations::default()
        },
    );
    assert_eq!(plan.creates.len(), 1);
    assert_eq!(plan.creates[0].key, create_key());
    assert_eq!(st.effects[&create_key()].attempts, 2);
    // Unknown: wait, and count the look.
    let (plan, st) = run(
        &obj,
        ConversationObservations {
            task: Some(Observation::unknown("corrupt")),
            ..ConversationObservations::default()
        },
    );
    assert!(plan.creates.is_empty());
    assert_eq!(st.effects[&create_key()].recovery, 1);
    assert_eq!(st.task.as_ref().unwrap().state, TieState::Pending);
    // Three refused creates need attention and stop.
    let spent = pending(
        Some(receipt(EffectOutcome::Failed {
            why: "invalid".into(),
        })),
        fleet_controllers::MAX_ATTEMPTS,
    );
    let (plan, st) = run_with(
        &object(Some(&spent)),
        ConversationObservations {
            task: Some(Observation::Known(None)),
            ..ConversationObservations::default()
        },
        defaults(),
        NOW + fleet_controllers::RETRY_DELAY_MS,
    );
    assert!(plan.creates.is_empty());
    assert_eq!(cond(&st, "NeedsAttention").reason, "TaskCreateFailed");
    let once = pending(
        Some(receipt(EffectOutcome::Failed {
            why: "invalid".into(),
        })),
        1,
    );
    let (plan, st) = run_with(
        &object(Some(&once)),
        ConversationObservations {
            task: Some(Observation::Known(None)),
            ..ConversationObservations::default()
        },
        defaults(),
        NOW + fleet_controllers::RETRY_DELAY_MS,
    );
    assert_eq!(plan.creates.len(), 1);
    assert_eq!(st.effects[&create_key()].attempts, 2);
}

#[test]
fn an_orphaned_thread_starts_the_next_task_on_any_admitted_message() {
    let mut st = judged(vec![
        stored(person(SAM, T0, "first"), 1),
        stored(person(ALEX, T0 + 300_000, "<@B0FLEET001> hi"), 1),
    ]);
    st.phase = Phase::Orphaned;
    st.task = Some(Tie {
        name: format!("{NAME}-1"),
        uid: "t1".into(),
        n: 1,
        state: TieState::Gone,
        trigger: None,
    });
    // A stranger's words are context: nothing starts.
    let mut context = st.clone();
    context.received = vec![message("Ev05", person("U0999ZZZZ", T0 + 400_000, "me too"))];
    let (plan, st2) = run(&object(Some(&context)), ConversationObservations::default());
    assert!(plan.creates.is_empty());
    assert_eq!(st2.phase, Phase::Orphaned);
    // An admitted message, mention or not, starts Task 2 with the thread.
    let mut admitted = st2.clone();
    admitted.received = vec![message("Ev06", person(ALEX, T0 + 500_000, "still broken"))];
    let (plan, st3) = run(
        &object(Some(&admitted)),
        ConversationObservations::default(),
    );
    assert_eq!(plan.creates.len(), 1);
    assert_eq!(plan.creates[0].key, task_key(UID, 2));
    assert_eq!(plan.creates[0].object.metadata.name, format!("{NAME}-2"));
    assert_eq!(
        plan.creates[0].object.spec["workspace"],
        json!({"dir": format!("/work/conversations/{NAME}")}),
        "the thread's next Task finds what the last one left"
    );
    assert_eq!(
        st3.task,
        Some(Tie {
            name: format!("{NAME}-2"),
            uid: String::new(),
            n: 2,
            state: TieState::Pending,
            trigger: Some("1730000500000".into()),
        })
    );
    assert_eq!(st3.phase, Phase::Orphaned, "until the receipt lands");
    assert_eq!(st3.messages.len(), 4);
}

#[test]
fn a_thread_fleet_opened_belongs_to_its_questions_task_while_that_task_exists() {
    let mut opening = person("B0FLEET001", T0, "Which database?");
    opening.author.kind = Kind::Fleet;
    opening.key = Some("task/asker/u9/gateway/question/q1".into());
    let mention = person(ALEX, T0 + 300_000, "<@B0FLEET001> start something else");
    let obj = object(Some(&mirrored(vec![message("Ev01", mention.clone())])));
    let read = ConversationObservations {
        capabilities: Some(caps(true)),
        history: Some(history(vec![opening.clone()])),
        ..ConversationObservations::default()
    };
    let (plan, st) = run(&obj, read.clone());
    assert!(
        plan.creates.is_empty(),
        "the opener was not asked about: no guess"
    );
    assert_eq!(st.judged, 0, "judged once the opener is known");
    assert_eq!(st.starter.as_ref().and_then(|s| s.key.clone()), opening.key);
    let again = object(Some(&st));
    assert_eq!(
        probes(&again, ConversationObservations::default()),
        vec![Probe::Object {
            kind: "Task".into(),
            name: "asker".into()
        }]
    );
    // The Task exists with that uid: tied to it by the marker, and the
    // mention starts nothing.
    let mut asker = task_object("u9");
    asker.metadata.name = "asker".into();
    let (plan, st2) = run(
        &again,
        ConversationObservations {
            opener: Some(Observation::Known(Some(asker.clone()))),
            ..ConversationObservations::default()
        },
    );
    assert!(plan.creates.is_empty());
    assert_eq!(st2.phase, Phase::Tied);
    assert_eq!(
        st2.task,
        Some(Tie {
            name: "asker".into(),
            uid: "u9".into(),
            n: 0,
            state: TieState::Created,
            trigger: None,
        })
    );
    assert_eq!(st2.judged, 1);
    // Unknown: no guess, mentions wait.
    let (plan, st3) = run(
        &again,
        ConversationObservations {
            opener: Some(Observation::unknown("corrupt")),
            ..ConversationObservations::default()
        },
    );
    assert!(plan.creates.is_empty());
    assert_eq!(st3.task, None);
    // Readably gone, or another uid: a mention may start a Task.
    for opener in [
        Observation::Known(None),
        Observation::Known(Some(task_object("u1"))),
    ] {
        let (plan, st4) = run(
            &again,
            ConversationObservations {
                opener: Some(opener),
                ..ConversationObservations::default()
            },
        );
        assert_eq!(plan.creates.len(), 1);
        assert_eq!(st4.task.as_ref().unwrap().n, 1);
    }
    // Fleet's opening post without a readable marker: ignore mentions.
    let mut unmarked = opening.clone();
    unmarked.key = None;
    let obj = object(Some(&mirrored(vec![message("Ev01", mention)])));
    let (plan, st5) = run(
        &obj,
        ConversationObservations {
            capabilities: Some(caps(true)),
            history: Some(history(vec![unmarked])),
            ..ConversationObservations::default()
        },
    );
    assert!(plan.creates.is_empty());
    assert_eq!(st5.task, None);
    assert_eq!(st5.judged, 1);
}

#[test]
fn nothing_starts_while_the_route_is_unknown_and_retention_waits_on_a_pending_create() {
    let obj = object(Some(&mirrored(vec![message(
        "Ev01",
        person(ALEX, T0 + 300_000, "<@B0FLEET001> hi"),
    )])));
    let (plan, st) = run_with(
        &obj,
        ConversationObservations {
            capabilities: Some(caps(true)),
            history: Some(history(vec![person(SAM, T0, "first")])),
            ..ConversationObservations::default()
        },
        defaults().for_route(None, None),
        NOW,
    );
    assert!(plan.creates.is_empty());
    assert_eq!(st.judged, 0, "judged once the route is back");
    assert_eq!(cond(&st, "NeedsAttention").reason, "RouteUnknown");
    let (plan, st) = run(&object(Some(&st)), ConversationObservations::default());
    assert_eq!(
        plan.creates.len(),
        1,
        "the route is back: the mention counts"
    );
    assert_eq!(st.judged, 1);
    // A pending create holds retention as a busy one does.
    let aged = pending(None, 1);
    let (plan, st) = run_with(
        &object(Some(&aged)),
        ConversationObservations {
            task: Some(Observation::unknown("corrupt")),
            ..ConversationObservations::default()
        },
        defaults(),
        CREATED_AT + 1_000_000,
    );
    assert!(!plan.delete);
    assert_eq!(st.not_before, Some(CREATED_AT + 1_000_000 + 1_000));
}

fn concluded_task(uid: &str, phase: &str) -> Object {
    let mut t = task_object(uid);
    t.status = json!({"phase": phase});
    t
}

fn thread_dir_record() -> ThreadDir {
    ThreadDir {
        path: format!("/work/conversations/{NAME}"),
        host: "local".into(),
    }
}

#[test]
fn an_idle_thread_whose_task_is_concluded_retires_its_task_then_its_directory_then_itself() {
    let idle = NOW + 10_000;
    let mut st = tied();
    st.dir = Some(thread_dir_record());
    let obj = object(Some(&st));
    // A running Task holds its thread: nothing retires.
    let (plan, after) = run_with(
        &obj,
        ConversationObservations {
            task: Some(Observation::Known(Some(concluded_task("t1", "Briefed")))),
            ..ConversationObservations::default()
        },
        defaults(),
        idle,
    );
    assert!(plan.effects.is_empty() && !plan.delete, "{plan:#?}");
    assert_eq!(after.retiring, None);
    assert_eq!(
        after.not_before,
        Some(idle + 1_000),
        "looked at again one interval later, never at once"
    );
    // An Unknown Task decides nothing.
    let (plan, after) = run_with(
        &obj,
        ConversationObservations {
            task: Some(Observation::unknown("store unreadable")),
            ..ConversationObservations::default()
        },
        defaults(),
        idle,
    );
    assert!(plan.effects.is_empty() && !plan.delete, "{plan:#?}");
    assert_eq!(after.retiring, None);
    // Not idle yet: nothing retires.
    let (plan, _) = run(
        &obj,
        ConversationObservations {
            task: Some(Observation::Known(Some(concluded_task("t1", "Concluded")))),
            ..ConversationObservations::default()
        },
    );
    assert!(plan.effects.is_empty() && !plan.delete, "{plan:#?}");
    // Idle and Concluded: step 1 deletes the Task, fenced on its uid.
    let (plan, after) = run_with(
        &obj,
        ConversationObservations {
            task: Some(Observation::Known(Some(concluded_task("t1", "Concluded")))),
            ..ConversationObservations::default()
        },
        defaults(),
        idle,
    );
    let retiring = after.retiring.expect("retiring");
    assert_eq!(retiring.at, idle);
    let task_key = retire_task_key(UID, idle);
    assert_eq!(plan.effects.len(), 1, "{plan:#?}");
    assert_eq!(plan.effects[0].key, task_key);
    assert_eq!(
        plan.effects[0].action,
        Action::DeleteTask {
            name: format!("{NAME}-1"),
            uid: "t1".into()
        }
    );
    assert!(!plan.delete);
    // The Task is gone: the thread orphans, and step 2 removes the
    // directory the manager recorded making, under the same retirement.
    let mut gone = after.clone();
    gone.effects.get_mut(&task_key).unwrap().receipt = Some(Receipt {
        action: "deleteTask".into(),
        at: idle,
        outcome: Observation::Known(EffectOutcome::Deleted),
    });
    let (plan, after) = run_with(
        &object(Some(&gone)),
        ConversationObservations {
            task: Some(Observation::Known(None)),
            ..ConversationObservations::default()
        },
        defaults(),
        idle + 2,
    );
    assert_eq!(after.phase, Phase::Orphaned);
    assert_eq!(
        after.retiring,
        Some(retiring),
        "the same retirement goes on"
    );
    let dir_key = retire_dir_key(UID, idle);
    assert_eq!(plan.effects.len(), 1, "{plan:#?}");
    assert_eq!(plan.effects[0].key, dir_key);
    assert_eq!(
        plan.effects[0].action,
        Action::RemoveDir {
            host: "local".into(),
            path: format!("/work/conversations/{NAME}"),
        }
    );
    assert!(
        !plan.delete,
        "the record goes with the Conversation, so it waits"
    );
    // Still named by a Task: it waits, and the attempt is not counted.
    let mut busy = after.clone();
    busy.effects.get_mut(&dir_key).unwrap().receipt = Some(Receipt {
        action: "removeDir".into(),
        at: idle + 2,
        outcome: Observation::Known(EffectOutcome::WorkspaceBusy {
            task: "by-hand".into(),
        }),
    });
    let (plan, after) = run_with(
        &object(Some(&busy)),
        ConversationObservations::default(),
        defaults(),
        idle + 3,
    );
    assert!(plan.effects.is_empty(), "one interval first: {plan:#?}");
    assert_eq!(after.not_before, Some(idle + 2 + 1_000));
    assert_eq!(cond(&after, "NeedsAttention").reason, "WorkspaceBusy");
    let (plan, after) = run_with(
        &object(Some(&busy)),
        ConversationObservations::default(),
        defaults(),
        idle + 2 + 1_000,
    );
    assert_eq!(plan.effects.len(), 1);
    assert_eq!(after.effects[&dir_key].attempts, 1);
    assert!(!plan.delete);
    // Removed: step 3 deletes the Conversation.
    let mut removed = after.clone();
    removed.effects.get_mut(&dir_key).unwrap().receipt = Some(Receipt {
        action: "removeDir".into(),
        at: idle + 3,
        outcome: Observation::Known(EffectOutcome::Deleted),
    });
    let (plan, _) = run_with(
        &object(Some(&removed)),
        ConversationObservations::default(),
        defaults(),
        idle + 2 + 1_001,
    );
    assert!(plan.effects.is_empty(), "{plan:#?}");
    assert!(plan.delete);
}

#[test]
fn a_thread_without_a_recorded_directory_retires_without_removing_one() {
    let mut st = tied();
    st.phase = Phase::Orphaned;
    st.task.as_mut().unwrap().state = TieState::Gone;
    fleet_api::condition::set_condition(
        &mut st.conditions,
        "Tied",
        ConditionStatus::False,
        "TaskGone",
        "",
        1,
        NOW - 3_000,
    );
    let (plan, _) = run_with(
        &object(Some(&st)),
        ConversationObservations::default(),
        defaults(),
        NOW + 10_000,
    );
    assert!(plan.effects.is_empty(), "{plan:#?}");
    assert!(plan.delete);
}

#[test]
fn a_message_numbered_during_a_retirement_ends_it() {
    let mut st = tied();
    st.phase = Phase::Orphaned;
    st.task.as_mut().unwrap().state = TieState::Gone;
    st.dir = Some(thread_dir_record());
    st.retiring = Some(Retiring {
        at: NOW - 1_000,
        revision: 1,
    });
    // A message numbered since moves the revision past the retirement's.
    st.revision = 2;
    st.last_message_at = Some(NOW - 10);
    let (plan, after) = run(&object(Some(&st)), ConversationObservations::default());
    assert_eq!(after.retiring, None, "{after:#?}");
    assert!(plan.effects.is_empty(), "the directory stays: {plan:#?}");
    assert!(!plan.delete);
    assert_eq!(after.dir, Some(thread_dir_record()));
}

#[test]
fn three_refused_steps_hold_the_retirement_with_a_written_condition() {
    let mut st = tied();
    st.phase = Phase::Orphaned;
    st.task.as_mut().unwrap().state = TieState::Gone;
    st.dir = Some(thread_dir_record());
    st.retiring = Some(Retiring {
        at: NOW - 100_000,
        revision: 1,
    });
    fleet_api::condition::set_condition(
        &mut st.conditions,
        "Tied",
        ConditionStatus::False,
        "TaskGone",
        "",
        1,
        NOW - 100_000,
    );
    let key = retire_dir_key(UID, NOW - 100_000);
    st.effects.insert(
        key.clone(),
        serde_json::from_value(json!({
            "intent": {"action": "removeDir", "at": NOW - 90_000, "generation": 1},
            "receipt": {"action": "removeDir", "at": NOW - 90_000,
                        "outcome": {"known": {"type": "failed", "why": "permission denied"}}},
            "attempts": 3
        }))
        .unwrap(),
    );
    let (plan, after) = run(&object(Some(&st)), ConversationObservations::default());
    assert!(plan.effects.is_empty() && !plan.delete, "{plan:#?}");
    let c = cond(&after, "NeedsAttention");
    assert_eq!(c.reason, "RetireFailed", "{after:#?}");
    assert!(c.message.contains("permission denied"), "{c:#?}");
    assert_eq!(
        after.dir,
        Some(thread_dir_record()),
        "the directory is kept"
    );
}

#[test]
fn a_message_written_while_the_tied_task_is_deleted_waits_to_be_judged() {
    let mut st = tied();
    st.messages = vec![stored(person(ALEX, T0 + 300_000, "still broken"), 2)];
    st.revision = 2;
    st.judged = 1;
    let mut deleting = concluded_task("t1", "Concluded");
    deleting.metadata.deletion_timestamp = Some(NOW - 10);
    let (_, after) = run(
        &object(Some(&st)),
        ConversationObservations {
            task: Some(Observation::Known(Some(deleting))),
            ..ConversationObservations::default()
        },
    );
    assert_eq!(
        after.judged, 1,
        "judged once the thread is Orphaned: {after:#?}"
    );
}

// ----------------------------------------------------------------------
// Reactions: admission marks a request working, or queued while it waits.
// ----------------------------------------------------------------------

mod reactions {
    use super::*;
    use fleet_api::conversation::react_key;
    use fleet_api::talk::Mark;

    const MENTION: &str = "1730000300000";

    fn reacting(on: bool) -> Observation<Capabilities> {
        Observation::Known(Capabilities {
            history: true,
            marker: Marker::Metadata,
            unmentioned: true,
            threads: true,
            reactions: on,
        })
    }

    fn with_caps(
        caps: Observation<Capabilities>,
        task: Option<Object>,
    ) -> ConversationObservations {
        ConversationObservations {
            capabilities: Some(caps),
            task: Some(Observation::Known(task)),
            ..ConversationObservations::default()
        }
    }

    /// The reactions a plan asks for: message, reaction name, on.
    fn reacts(plan: &Plan) -> Vec<(String, String, bool)> {
        plan.effects
            .iter()
            .filter_map(|e| match &e.action {
                Action::React {
                    message,
                    reaction,
                    on,
                    ..
                } => Some((message.clone(), reaction.clone(), *on)),
                _ => None,
            })
            .collect()
    }

    fn r(message: &str, reaction: &str, on: bool) -> (String, String, bool) {
        (message.into(), reaction.into(), on)
    }

    fn created() -> ConversationStatus {
        pending(
            Some(receipt(EffectOutcome::Created {
                uid: "t1".into(),
                adopted: false,
            })),
            1,
        )
    }

    fn busy() -> ConversationStatus {
        pending(
            Some(receipt(EffectOutcome::WorkspaceBusy {
                task: "holder".into(),
            })),
            1,
        )
    }

    #[test]
    fn a_created_task_marks_its_trigger_working_once_under_the_conversations_key() {
        let (plan, st) = run(
            &object(Some(&created())),
            with_caps(reacting(true), Some(task_object("t1"))),
        );
        assert_eq!(st.phase, Phase::Tied);
        assert_eq!(reacts(&plan), vec![r(MENTION, "eyes", true)]);
        let key = react_key(UID, MENTION, Mark::Working, true);
        assert_eq!(plan.effects[0].key, key);
        assert_eq!(st.effects[&key].intent.action, "react");
        assert_eq!(st.reactions.marks[MENTION], Mark::Working);
        // The message before it started nothing: the join marks it.
        assert_eq!(st.reactions.marks.len(), 1, "{:?}", st.reactions);
        // Landed: the record is kept for the join to read, and nothing is
        // added again.
        let mut landed = st.clone();
        landed.effects.get_mut(&key).unwrap().receipt =
            Some(receipt(EffectOutcome::Reacted { already: false }));
        let (plan, st) = run(
            &object(Some(&landed)),
            with_caps(reacting(true), Some(task_object("t1"))),
        );
        assert!(reacts(&plan).is_empty(), "{plan:?}");
        assert!(st.effects.contains_key(&key));
        assert_eq!(st.reactions.marks[MENTION], Mark::Working);
    }

    #[test]
    fn a_working_reaction_that_fails_is_never_tried_again_by_the_conversation() {
        // The Task may reply before a retry lands, and a late working
        // reaction would then stay beside the replied one: the join adds it
        // instead, under the Task's key.
        let (_, st) = run(
            &object(Some(&created())),
            with_caps(reacting(true), Some(task_object("t1"))),
        );
        let key = react_key(UID, MENTION, Mark::Working, true);
        for outcome in [
            Observation::Known(EffectOutcome::Failed {
                why: "rate limited".into(),
            }),
            Observation::Unknown {
                why: "timed out".into(),
            },
        ] {
            let mut failed = st.clone();
            failed.effects.get_mut(&key).unwrap().receipt = Some(Receipt {
                action: "react".into(),
                at: NOW - 1_000,
                outcome,
            });
            let (plan, after) = run_with(
                &object(Some(&failed)),
                with_caps(reacting(true), Some(task_object("t1"))),
                defaults(),
                NOW + 600_000,
            );
            assert!(reacts(&plan).is_empty(), "{plan:?}");
            assert_eq!(after.effects[&key].attempts, 1);
            assert_eq!(after.reactions.marks[MENTION], Mark::Working);
        }
    }

    #[test]
    fn queued_marks_the_tie_pass_could_not_change_only_lose_their_queued_reaction() {
        let mut st = busy();
        st.reactions.marks.insert(MENTION.into(), Mark::Queued);
        st.effects.get_mut(&create_key()).unwrap().receipt =
            Some(receipt(EffectOutcome::Created {
                uid: "t1".into(),
                adopted: false,
            }));
        // The tie pass, with the capabilities Unknown: no mark changes.
        let (plan, tied_st) = run(
            &object(Some(&st)),
            with_caps(
                Observation::Unknown {
                    why: "timed out".into(),
                },
                Some(task_object("t1")),
            ),
        );
        assert_eq!(tied_st.phase, Phase::Tied);
        assert!(reacts(&plan).is_empty());
        assert_eq!(tied_st.reactions.marks[MENTION], Mark::Queued);
        // A later pass: the queued reaction goes, and no working one is
        // added, since the join may have replied already.
        let (plan, st) = run(
            &object(Some(&tied_st)),
            with_caps(reacting(true), Some(task_object("t1"))),
        );
        assert_eq!(
            reacts(&plan),
            vec![r(MENTION, "hourglass_flowing_sand", false)]
        );
        assert!(st.reactions.marks.is_empty(), "{:?}", st.reactions);
    }

    #[test]
    fn a_queued_message_that_leaves_the_mirror_loses_its_queued_reaction() {
        let mut st = busy();
        st.task.as_mut().unwrap().state = TieState::Busy;
        st.reactions.marks.insert("gone-1".into(), Mark::Queued);
        st.reactions.marks.insert(MENTION.into(), Mark::Queued);
        let (plan, st) = run(&object(Some(&st)), with_caps(reacting(true), None));
        assert_eq!(
            reacts(&plan),
            vec![r("gone-1", "hourglass_flowing_sand", false)]
        );
        assert!(!st.reactions.marks.contains_key("gone-1"));
        assert_eq!(st.reactions.marks[MENTION], Mark::Queued);
    }

    #[test]
    fn a_waiting_create_marks_its_request_queued_then_working_once_the_task_starts() {
        let mut st = busy();
        // A later admitted message, and a stranger's, both delivered.
        st.messages
            .push(stored(person(ALEX, T0 + 400_000, "also this"), 2));
        st.messages
            .push(stored(person(SAM, T0 + 410_000, "me too"), 3));
        st.revision = 3;
        st.judged = 3;
        let (plan, st) = run(&object(Some(&st)), with_caps(reacting(true), None));
        assert_eq!(st.task.as_ref().unwrap().state, TieState::Busy);
        let later = (T0 + 400_000).to_string();
        let mut want = vec![
            r(MENTION, "hourglass_flowing_sand", true),
            r(&later, "hourglass_flowing_sand", true),
        ];
        want.sort();
        let mut got = reacts(&plan);
        got.sort();
        assert_eq!(got, want);
        assert_eq!(st.reactions.marks[MENTION], Mark::Queued);
        assert_eq!(st.reactions.marks[&later], Mark::Queued);
        assert!(!st.reactions.marks.contains_key(&T0.to_string()));
        assert!(!st.reactions.marks.contains_key(&(T0 + 410_000).to_string()));
        // The busy notice is planned beside them: a reaction holds nothing.
        assert!(
            plan.effects
                .iter()
                .any(|e| matches!(e.action, Action::Post { .. }))
        );
        // Still busy on the next pass: nothing is added again.
        let mut again = st.clone();
        for rec in again.effects.values_mut() {
            if rec.intent.action == "react" {
                rec.receipt = Some(receipt(EffectOutcome::Reacted { already: false }));
            }
        }
        again.task.as_mut().unwrap().state = TieState::Pending;
        again.effects.get_mut(&create_key()).unwrap().receipt =
            Some(receipt(EffectOutcome::WorkspaceBusy {
                task: "holder".into(),
            }));
        let (plan, busy_again) = run(&object(Some(&again)), with_caps(reacting(true), None));
        assert!(reacts(&plan).is_empty(), "{plan:?}");
        // Free: each queued message is marked working, queued removed.
        let mut free = busy_again.clone();
        free.task.as_mut().unwrap().state = TieState::Pending;
        free.effects.get_mut(&create_key()).unwrap().receipt =
            Some(receipt(EffectOutcome::Created {
                uid: "t1".into(),
                adopted: false,
            }));
        let (plan, st) = run(
            &object(Some(&free)),
            with_caps(reacting(true), Some(task_object("t1"))),
        );
        assert_eq!(st.phase, Phase::Tied);
        let mut got = reacts(&plan);
        got.sort();
        let mut want = vec![
            r(MENTION, "eyes", true),
            r(MENTION, "hourglass_flowing_sand", false),
            r(&later, "eyes", true),
            r(&later, "hourglass_flowing_sand", false),
        ];
        want.sort();
        assert_eq!(got, want);
        assert_eq!(st.reactions.marks[MENTION], Mark::Working);
        assert_eq!(st.reactions.marks[&later], Mark::Working);
    }

    #[test]
    fn a_withdrawn_wait_removes_its_queued_reaction() {
        let mut st = busy();
        st.task.as_mut().unwrap().state = TieState::Busy;
        st.messages
            .push(stored(person(ALEX, T0 + 400_000, "also this"), 2));
        st.revision = 2;
        st.judged = 2;
        let later = (T0 + 400_000).to_string();
        st.reactions.marks.insert(MENTION.into(), Mark::Queued);
        st.reactions.marks.insert(later.clone(), Mark::Queued);
        st.received = vec![removed("Ev05", MENTION)];
        let (plan, st) = run(&object(Some(&st)), with_caps(reacting(true), None));
        assert_eq!(st.task, None, "withdrawn");
        // Every queued reaction goes, the removed message's included: the
        // mirror cannot tell a removal from a rotation, and a removal of a
        // reaction is set, not counted.
        assert_eq!(
            reacts(&plan),
            vec![
                r(MENTION, "hourglass_flowing_sand", false),
                r(&later, "hourglass_flowing_sand", false)
            ]
        );
        assert!(st.reactions.marks.is_empty(), "{:?}", st.reactions);
    }

    #[test]
    fn no_mark_without_reactions_and_none_while_the_capabilities_are_unknown() {
        // The platform has no reactions.
        let (plan, st) = run(
            &object(Some(&created())),
            with_caps(reacting(false), Some(task_object("t1"))),
        );
        assert!(reacts(&plan).is_empty());
        assert!(st.reactions.marks.is_empty());
        // Unknown: the tie goes on, no mark, no reaction.
        let (plan, st) = run(
            &object(Some(&created())),
            with_caps(
                Observation::Unknown {
                    why: "timed out".into(),
                },
                Some(task_object("t1")),
            ),
        );
        assert_eq!(st.phase, Phase::Tied);
        assert!(reacts(&plan).is_empty());
        assert!(st.reactions.marks.is_empty());
        // Reactions off.
        let mut off = defaults();
        off.reactions = None;
        let (plan, st) = run_with(
            &object(Some(&busy())),
            with_caps(reacting(true), None),
            off.clone(),
            NOW,
        );
        assert!(reacts(&plan).is_empty());
        assert!(st.reactions.marks.is_empty());
        // And no capabilities are asked for them.
        let input_probes = ConversationController
            .probes(&ConversationInput {
                object: &object(Some(&pending(None, 1))),
                observations: ConversationObservations::default(),
                now: NOW,
                defaults: off,
            })
            .unwrap();
        assert!(
            !input_probes
                .iter()
                .any(|p| matches!(p, Probe::Capabilities { .. })),
            "{input_probes:?}"
        );
    }

    #[test]
    fn a_refused_reaction_is_tried_three_times_then_given_up_and_raises_nothing() {
        let (_, st) = run(&object(Some(&busy())), with_caps(reacting(true), None));
        let key = react_key(UID, MENTION, Mark::Queued, true);
        let mut failed = st.clone();
        failed.effects.get_mut(&key).unwrap().receipt = Some(receipt(EffectOutcome::Failed {
            why: "rate limited".into(),
        }));
        // Before the delay: not yet, and a wake is asked for.
        let (plan, st) = run(&object(Some(&failed)), with_caps(reacting(true), None));
        assert!(reacts(&plan).is_empty());
        assert!(st.effects.contains_key(&key));
        let reason = cond(&st, "NeedsAttention").reason.clone();
        assert_eq!(reason, "WorkspaceBusy", "the reaction raises nothing");
        // After the delay: again, under the same key.
        let later = NOW + 60_000;
        let (plan, st) = run_with(
            &object(Some(&failed)),
            with_caps(reacting(true), None),
            defaults(),
            later,
        );
        assert_eq!(
            reacts(&plan),
            vec![r(MENTION, "hourglass_flowing_sand", true)]
        );
        assert_eq!(st.effects[&key].attempts, 2);
        // Past three attempts: dropped, and the mark stays.
        let mut spent = st.clone();
        let rec = spent.effects.get_mut(&key).unwrap();
        rec.attempts = 3;
        rec.receipt = Some(receipt(EffectOutcome::Failed {
            why: "rate limited".into(),
        }));
        let (plan, st) = run_with(
            &object(Some(&spent)),
            with_caps(reacting(true), None),
            defaults(),
            later + 60_000,
        );
        assert!(reacts(&plan).is_empty());
        assert!(!st.effects.contains_key(&key));
        assert_eq!(st.reactions.marks[MENTION], Mark::Queued);
    }

    #[test]
    fn an_orphaned_thread_forgets_the_working_marks_of_its_old_task() {
        let mut st = tied();
        st.reactions.marks.insert(MENTION.into(), Mark::Working);
        st.messages
            .push(stored(person(ALEX, T0 + 300_000, "<@B0FLEET001> hi"), 1));
        st.judged = 1;
        let (plan, st) = run(&object(Some(&st)), with_caps(reacting(true), None));
        assert_eq!(st.phase, Phase::Orphaned);
        assert!(reacts(&plan).is_empty(), "nothing is removed: {plan:?}");
        assert!(st.reactions.marks.is_empty());
    }
}
