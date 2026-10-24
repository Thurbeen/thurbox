//! The pure half of the export: session events and session counts in, the
//! measurements and log records to send out. No clock, no network, no
//! database — what [`super::export`] hands the SDK is decided here alone.

use std::collections::{BTreeMap, HashMap};

use opentelemetry::KeyValue;

use crate::session::{SessionId, SessionState};
use crate::storage::ExportEvent;

/// One measurement of thurbox's own instruments.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Measure {
    /// `thurbox.session.transitions`: one agent state change.
    Transition,
    /// `thurbox.session.lifecycle`: one event that is not a state change.
    Lifecycle,
    /// `thurbox.session.exits`: the agent's process returned.
    Exit,
    /// `thurbox.turn.duration`, seconds in `working`.
    Turn(f64),
    /// `thurbox.blocked.duration`, seconds in `blocked` — waiting on a human.
    Blocked(f64),
    /// `thurbox.session.lifetime`, seconds from creation to going.
    Lifetime(f64),
}

/// One session event as an OTLP log record.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Record {
    pub name: &'static str,
    /// When the event happened — the record's timestamp, not the export's.
    pub at_ms: i64,
    pub attributes: Vec<KeyValue>,
}

#[derive(Debug, Default, PartialEq)]
pub(crate) struct Summary {
    pub measures: Vec<(Measure, Vec<KeyValue>)>,
    pub records: Vec<Record>,
}

/// One live session as `thurbox.sessions` counts it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionCount {
    pub id: SessionId,
    /// The word every surface shows ([`crate::session::Assessment::state`]),
    /// or `lost` for a row whose pane went with nobody asking.
    pub state: String,
    pub agent: String,
    pub backend: String,
}

/// The word `thurbox.sessions` uses for a session marked lost.
pub const LOST: &str = "lost";

/// Turn `events` into measurements and records.
///
/// `prior` is the state each session was in when the batch starts, and since
/// when (`Database::state_at_seq`): a turn that began before this batch is
/// measured from its real start rather than dropped. A duration is measured
/// only between two state changes, so a stop, a loss or a delete in between
/// ends it unmeasured — the agent did not finish, it went.
pub(crate) fn summarize(
    events: &[ExportEvent],
    prior: &HashMap<SessionId, (String, i64)>,
    per_session: bool,
    attributes: &HashMap<String, BTreeMap<String, String>>,
) -> Summary {
    let mut since = prior.clone();
    let mut out = Summary::default();
    for export in events {
        let e = &export.event;
        let agent = export
            .session
            .as_ref()
            .map_or("unknown", |s| s.agent.as_str())
            .to_string();
        let custom = attributes.get(&e.session_id.to_string());
        let mut labels = if per_session {
            custom_labels(custom)
        } else {
            Vec::new()
        };
        labels.push(KeyValue::new("agent", agent.clone()));
        if per_session {
            labels.push(KeyValue::new(
                "thurbox.session.id",
                e.session_id.to_string(),
            ));
        }
        let mut detail = Vec::new();
        let name = match (e.event.as_str(), e.reason.as_str()) {
            ("changed", "state") => {
                let mut moved = labels.clone();
                if let Some(from) = &e.from_state {
                    moved.push(KeyValue::new("from", from.clone()));
                    detail.push(KeyValue::new("from", from.clone()));
                }
                if let Some(to) = &e.to_state {
                    moved.push(KeyValue::new("to", to.clone()));
                    detail.push(KeyValue::new("to", to.clone()));
                }
                out.measures.push((Measure::Transition, moved));
                if let Some((state, at)) = since.remove(&e.session_id) {
                    let secs = (e.at_ms - at).max(0) as f64 / 1000.0;
                    match state.as_str() {
                        "working" => {
                            let mut turn = labels.clone();
                            let outcome = e.to_state.clone().unwrap_or_default();
                            turn.push(KeyValue::new("outcome", outcome));
                            out.measures.push((Measure::Turn(secs), turn));
                        }
                        "blocked" => out.measures.push((Measure::Blocked(secs), labels.clone())),
                        _ => {}
                    }
                }
                if let Some(to) = &e.to_state {
                    since.insert(e.session_id, (to.clone(), e.at_ms));
                }
                "thurbox.session.state_changed"
            }
            (event, reason) => {
                let mut cause = labels.clone();
                cause.push(KeyValue::new("reason", reason.to_string()));
                out.measures.push((Measure::Lifecycle, cause.clone()));
                detail.push(KeyValue::new("reason", reason.to_string()));
                if reason == "exited" {
                    let mut exit = labels.clone();
                    if let Some(code) = e.exit_code {
                        exit.push(KeyValue::new("exit_code", code));
                        detail.push(KeyValue::new("exit_code", code));
                    }
                    out.measures.push((Measure::Exit, exit));
                }
                if reason != "updated" {
                    since.remove(&e.session_id);
                }
                match event {
                    "created" => "thurbox.session.created",
                    "gone" => {
                        if let Some(session) = &export.session {
                            let secs = (e.at_ms - session.created_at).max(0) as f64 / 1000.0;
                            out.measures.push((Measure::Lifetime(secs), cause));
                        }
                        "thurbox.session.gone"
                    }
                    _ => "thurbox.session.changed",
                }
            }
        };
        let mut attributes = custom_labels(custom);
        attributes.extend([
            KeyValue::new("thurbox.session.id", e.session_id.to_string()),
            KeyValue::new(
                "thurbox.session.name",
                export
                    .session
                    .as_ref()
                    .map(|s| s.name.clone())
                    .unwrap_or_default(),
            ),
            KeyValue::new("agent", agent),
        ]);
        attributes.extend(detail);
        out.records.push(Record {
            name,
            at_ms: e.at_ms,
            attributes,
        });
    }
    out
}

// User labels cannot replace the fields that give a Thurbox measurement its meaning.
fn custom_labels(attributes: Option<&BTreeMap<String, String>>) -> Vec<KeyValue> {
    attributes
        .into_iter()
        .flat_map(BTreeMap::iter)
        .filter(|(key, _)| {
            !key.starts_with("thurbox.")
                && !matches!(
                    key.as_str(),
                    "agent"
                        | "backend"
                        | "state"
                        | "from"
                        | "to"
                        | "reason"
                        | "outcome"
                        | "exit_code"
                )
        })
        .map(|(key, value)| KeyValue::new(key.clone(), value.clone()))
        .collect()
}

/// `thurbox.sessions`: how many sessions are in each state, per agent and
/// backend.
///
/// Every state word is reported for each agent and backend seen, at zero where
/// no session is in it. Each tick's gauge is a fresh reading, and a state left
/// out would leave the last non-zero reading standing in the backend until it
/// goes stale — a session that finished would still count as `working`.
pub(crate) fn gauge(
    sessions: &[SessionCount],
    per_session: bool,
    attributes: &HashMap<String, BTreeMap<String, String>>,
) -> Vec<(i64, Vec<KeyValue>)> {
    let mut groups: BTreeMap<(String, String, Option<String>), BTreeMap<String, i64>> =
        BTreeMap::new();
    for s in sessions {
        let key = (
            s.agent.clone(),
            s.backend.clone(),
            per_session.then(|| s.id.to_string()),
        );
        *groups
            .entry(key)
            .or_default()
            .entry(s.state.clone())
            .or_default() += 1;
    }
    let words = SessionState::ALL
        .iter()
        .map(|s| s.as_str())
        .chain(std::iter::once(LOST));
    let mut out = Vec::new();
    for ((agent, backend, id), counts) in groups {
        let mut states: BTreeMap<String, i64> = words.clone().map(|w| (w.to_string(), 0)).collect();
        states.extend(counts);
        for (state, count) in states {
            let mut labels = id
                .as_ref()
                .map_or_else(Vec::new, |id| custom_labels(attributes.get(id)));
            labels.extend([
                KeyValue::new("state", state),
                KeyValue::new("agent", agent.clone()),
                KeyValue::new("backend", backend.clone()),
            ]);
            if let Some(id) = &id {
                labels.push(KeyValue::new("thurbox.session.id", id.clone()));
            }
            out.push((count, labels));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::{ExportSession, SessionEventRow};

    fn event(id: SessionId, seq: i64, event: &str, reason: &str, at_ms: i64) -> ExportEvent {
        ExportEvent {
            event: SessionEventRow {
                seq,
                session_id: id,
                event: event.into(),
                reason: reason.into(),
                from_state: None,
                to_state: None,
                at_ms,
                exit_code: None,
                detail: None,
            },
            session: Some(ExportSession {
                name: "worker".into(),
                agent: "claude".into(),
                backend_type: "local-tmux".into(),
                created_at: 1_000,
            }),
        }
    }

    fn state(id: SessionId, seq: i64, from: Option<&str>, to: &str, at_ms: i64) -> ExportEvent {
        let mut e = event(id, seq, "changed", "state", at_ms);
        e.event.from_state = from.map(str::to_string);
        e.event.to_state = Some(to.to_string());
        e
    }

    fn label(labels: &[KeyValue], key: &str) -> Option<String> {
        labels
            .iter()
            .find(|kv| kv.key.as_str() == key)
            .map(|kv| kv.value.to_string())
    }

    #[test]
    fn a_turn_and_a_wait_are_measured_between_state_changes() {
        let id = SessionId::default();
        let events = [
            state(id, 1, None, "working", 10_000),
            state(id, 2, Some("working"), "blocked", 13_000),
            state(id, 3, Some("blocked"), "working", 20_000),
            state(id, 4, Some("working"), "done", 21_500),
        ];
        let summary = summarize(&events, &HashMap::new(), false, &HashMap::new());
        let durations: Vec<(Measure, Option<String>)> = summary
            .measures
            .iter()
            .filter(|(m, _)| matches!(m, Measure::Turn(_) | Measure::Blocked(_)))
            .map(|(m, labels)| (m.clone(), label(labels, "outcome")))
            .collect();
        assert_eq!(
            durations,
            vec![
                (Measure::Turn(3.0), Some("blocked".into())),
                (Measure::Blocked(7.0), None),
                (Measure::Turn(1.5), Some("done".into())),
            ]
        );
    }

    #[test]
    fn a_turn_begun_before_the_batch_is_measured_from_its_start() {
        let id = SessionId::default();
        let prior = HashMap::from([(id, ("working".to_string(), 4_000))]);
        let events = [state(id, 9, Some("working"), "done", 10_000)];
        let summary = summarize(&events, &prior, false, &HashMap::new());
        assert!(summary
            .measures
            .iter()
            .any(|(m, _)| *m == Measure::Turn(6.0)));
    }

    #[test]
    fn a_stop_ends_a_turn_unmeasured() {
        let id = SessionId::default();
        let events = [
            state(id, 1, None, "working", 1_000),
            event(id, 2, "changed", "stopped", 2_000),
            state(id, 3, None, "done", 3_000),
        ];
        let summary = summarize(&events, &HashMap::new(), false, &HashMap::new());
        assert!(!summary
            .measures
            .iter()
            .any(|(m, _)| matches!(m, Measure::Turn(_))));
    }

    #[test]
    fn going_records_the_lifetime_and_the_reason() {
        let id = SessionId::default();
        let events = [event(id, 1, "gone", "soft_deleted", 61_000)];
        let summary = summarize(&events, &HashMap::new(), false, &HashMap::new());
        let (measure, labels) = summary
            .measures
            .iter()
            .find(|(m, _)| matches!(m, Measure::Lifetime(_)))
            .expect("a lifetime");
        assert_eq!(*measure, Measure::Lifetime(60.0));
        assert_eq!(label(labels, "reason").as_deref(), Some("soft_deleted"));
        assert_eq!(summary.records[0].name, "thurbox.session.gone");
        assert_eq!(summary.records[0].at_ms, 61_000);
    }

    #[test]
    fn an_exit_is_counted_with_its_code() {
        let id = SessionId::default();
        let mut exited = event(id, 1, "changed", "exited", 5_000);
        exited.event.exit_code = Some(2);
        let summary = summarize(&[exited], &HashMap::new(), false, &HashMap::new());
        let exit = summary
            .measures
            .iter()
            .find(|(m, _)| *m == Measure::Exit)
            .expect("an exit");
        assert_eq!(label(&exit.1, "exit_code").as_deref(), Some("2"));
        assert_eq!(summary.records[0].name, "thurbox.session.changed");
    }

    #[test]
    fn the_session_id_labels_metrics_only_per_session() {
        let id = SessionId::default();
        let events = [state(id, 1, None, "working", 1_000)];
        let pooled = summarize(&events, &HashMap::new(), false, &HashMap::new());
        assert_eq!(label(&pooled.measures[0].1, "thurbox.session.id"), None);
        assert_eq!(
            label(&pooled.records[0].attributes, "thurbox.session.id"),
            Some(id.to_string()),
            "records always carry it"
        );
        let split = summarize(&events, &HashMap::new(), true, &HashMap::new());
        assert_eq!(
            label(&split.measures[0].1, "thurbox.session.id"),
            Some(id.to_string())
        );
    }

    #[test]
    fn the_gauge_reports_every_state_word_down_to_zero() {
        let sessions = [
            SessionCount {
                id: SessionId::default(),
                state: "working".into(),
                agent: "claude".into(),
                backend: "local-tmux".into(),
            },
            SessionCount {
                id: SessionId::default(),
                state: "working".into(),
                agent: "claude".into(),
                backend: "local-tmux".into(),
            },
        ];
        let points = gauge(&sessions, false, &HashMap::new());
        assert_eq!(points.len(), SessionState::ALL.len() + 1);
        let of = |word: &str| {
            points
                .iter()
                .find(|(_, l)| label(l, "state").as_deref() == Some(word))
                .map(|(v, _)| *v)
        };
        assert_eq!(of("working"), Some(2));
        assert_eq!(of("done"), Some(0));
        assert_eq!(of(LOST), Some(0));
    }
}
