//! Fleet's own events about a Task, on Board [`FLEET`](crate::board::FLEET):
//! what TaskController publishes as itself, after the write that caused
//! each, and its record of what it published (`Task.status.fleetEvents`).
//!
//! A phase change queues its event in `pending` in the same write that
//! changes the phase, so the intent lands with its cause. A declaration and
//! a result are the worker's writes: an event is owed for each declaration
//! past `declared`, and for a result whose generation is not `result`. Each
//! event's key names the Task, its uid and the event's place in its series,
//! so a publish that is tried again appends nothing new.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::event::Publish;
use crate::result::TaskResult;
use crate::task::Phase;

pub const PHASE: &str = "fleet.task.phase";
pub const DECLARED: &str = "fleet.task.declared";
pub const RESULT: &str = "fleet.task.result";

/// The most phase events `pending` holds. A new one past it drops the
/// oldest, which leaves a gap in the phase keys' numbers.
pub const MAX_PENDING: usize = 64;
/// The most characters of a result's summary an event carries.
pub const MAX_SUMMARY_CHARS: usize = 1024;
/// The most bytes of a declaration, as JSON, an event carries whole.
pub const MAX_DECLARATION_BYTES: usize = 4096;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FleetEvents {
    /// Phase events written with their phase change and not appended yet.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pending: Vec<Publish>,
    /// Phase changes over the Task's life: the number of the last one's key.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub phases: u64,
    /// How many of `status.declared` have their event.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub declared: u64,
    /// The generation of the last result that has its event.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<u64>,
    /// Events Board `fleet` refused, such as when it does not exist, over
    /// the Task's life. They are not published again.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub skipped: u64,
    /// The key of the last event skipped, and why.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_skipped: Option<String>,
}

fn is_zero(n: &u64) -> bool {
    *n == 0
}

/// `task/<name>/<uid>/<series>/<n>`.
pub fn key(task: &str, uid: &str, series: &str, n: u64) -> String {
    format!("task/{task}/{uid}/{series}/{n}")
}

fn publish(event_type: &str, task: &str, key: String, data: Value) -> Publish {
    Publish {
        event_type: event_type.to_string(),
        subject: Some(task.to_string()),
        key: Some(key),
        correlation: None,
        data,
    }
}

impl FleetEvents {
    pub fn is_empty(&self) -> bool {
        *self == FleetEvents::default()
    }

    /// Queue the event of a change from `from` to `to`, for `reason`.
    pub fn queue_phase(
        &mut self,
        task: &str,
        uid: &str,
        from: Phase,
        to: Phase,
        reason: Option<&str>,
    ) {
        self.phases += 1;
        let mut data = json!({"from": from, "to": to});
        if let Some(reason) = reason {
            data["reason"] = json!(reason);
        }
        self.pending.push(publish(
            PHASE,
            task,
            key(task, uid, "phase", self.phases),
            data,
        ));
        if self.pending.len() > MAX_PENDING {
            self.pending.remove(0);
        }
    }

    /// Every event owed, oldest first: the pending ones, one per
    /// declaration past `declared`, and one for a result not published
    /// yet. With them, the record to write once all are appended.
    ///
    /// An empty record is a Task from before Board `fleet`: a Task queues
    /// its first phase change before any brief, and a declaration or a
    /// result needs a brief. Its past is owed nothing, and the record
    /// starts at what status holds.
    pub fn owed(
        &self,
        task: &str,
        uid: &str,
        declared: &[Value],
        result: Option<&TaskResult>,
    ) -> (Vec<Publish>, FleetEvents) {
        if self.is_empty() {
            let baseline = FleetEvents {
                declared: declared.len() as u64,
                result: result.map(|r| r.generation),
                ..FleetEvents::default()
            };
            return (Vec::new(), baseline);
        }
        let mut events = self.pending.clone();
        let mut next = FleetEvents {
            pending: Vec::new(),
            ..self.clone()
        };
        let seen = usize::try_from(self.declared).unwrap_or(usize::MAX);
        for (i, item) in declared.iter().enumerate().skip(seen) {
            let n = i as u64;
            events.push(publish(
                DECLARED,
                task,
                key(task, uid, "declared", n),
                declaration_data(n, item),
            ));
        }
        next.declared = next.declared.max(declared.len() as u64);
        if let Some(r) = result.filter(|r| self.result != Some(r.generation)) {
            events.push(publish(
                RESULT,
                task,
                key(task, uid, "result", r.generation),
                result_data(r),
            ));
            next.result = Some(r.generation);
        }
        (events, next)
    }

    /// Count `publish` as refused, for `why`.
    pub fn skip(&mut self, publish: &Publish, why: &str) {
        self.skipped += 1;
        self.last_skipped = Some(format!(
            "{}: {why}",
            publish.key.as_deref().unwrap_or(&publish.event_type)
        ));
    }
}

fn declaration_data(index: u64, item: &Value) -> Value {
    let size = serde_json::to_vec(item).map_or(usize::MAX, |b| b.len());
    if size <= MAX_DECLARATION_BYTES {
        return json!({"index": index, "declaration": item});
    }
    json!({
        "index": index,
        "declaration": {"kind": item.get("kind").cloned().unwrap_or(Value::Null)},
        "truncated": true,
    })
}

fn result_data(r: &TaskResult) -> Value {
    let mut data = json!({"generation": r.generation, "outcome": r.outcome});
    if !r.summary.is_empty() {
        let cut: String = r.summary.chars().take(MAX_SUMMARY_CHARS).collect();
        if cut.len() < r.summary.len() {
            data["summaryTruncated"] = json!(true);
        }
        data["summary"] = json!(cut);
    }
    data
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::result::Outcome;

    fn result(generation: u64, summary: &str) -> TaskResult {
        TaskResult {
            outcome: Outcome::Shipped,
            generation,
            summary: summary.to_string(),
            artifacts: Vec::new(),
            outputs: Default::default(),
        }
    }

    #[test]
    fn a_phase_change_is_queued_under_the_next_number() {
        let mut f = FleetEvents::default();
        f.queue_phase("a", "u1", Phase::Pending, Phase::Spawning, Some("Starting"));
        f.queue_phase("a", "u1", Phase::Spawning, Phase::Spawned, None);
        let keys: Vec<_> = f.pending.iter().map(|p| p.key.clone().unwrap()).collect();
        assert_eq!(keys, ["task/a/u1/phase/1", "task/a/u1/phase/2"]);
        assert_eq!(
            f.pending[0].data,
            json!({"from": "Pending", "to": "Spawning", "reason": "Starting"})
        );
        assert_eq!(
            f.pending[1].data,
            json!({"from": "Spawning", "to": "Spawned"})
        );
        assert_eq!(f.pending[0].subject.as_deref(), Some("a"));
    }

    #[test]
    fn pending_keeps_the_newest_phase_events() {
        let mut f = FleetEvents::default();
        for _ in 0..MAX_PENDING + 2 {
            f.queue_phase("a", "u", Phase::Briefed, Phase::Concluded, None);
        }
        assert_eq!(f.pending.len(), MAX_PENDING);
        assert_eq!(f.pending[0].key.as_deref(), Some("task/a/u/phase/3"));
    }

    #[test]
    fn owed_names_each_new_declaration_and_a_new_result_once() {
        let mut f = FleetEvents::default();
        f.queue_phase("a", "u", Phase::Briefed, Phase::Concluded, None);
        let declared = [json!({"kind": "x"}), json!({"kind": "y"})];
        let (events, next) = f.owed("a", "u", &declared, Some(&result(2, "ok")));
        let keys: Vec<_> = events.iter().map(|p| p.key.clone().unwrap()).collect();
        assert_eq!(
            keys,
            [
                "task/a/u/phase/1",
                "task/a/u/declared/0",
                "task/a/u/declared/1",
                "task/a/u/result/2"
            ]
        );
        assert_eq!(
            events[3].data,
            json!({"generation": 2, "outcome": "shipped", "summary": "ok"})
        );
        assert!(next.pending.is_empty());
        assert_eq!((next.declared, next.result, next.phases), (2, Some(2), 1));
        let (again, same) = next.owed("a", "u", &declared, Some(&result(2, "ok")));
        assert!(again.is_empty(), "{again:?}");
        assert_eq!(same, next);
    }

    #[test]
    fn an_empty_record_owes_nothing_and_starts_at_the_status() {
        let declared = [json!({"kind": "x"})];
        let (events, next) = FleetEvents::default().owed("a", "u", &declared, Some(&result(3, "")));
        assert!(events.is_empty(), "{events:?}");
        assert_eq!((next.declared, next.result, next.phases), (1, Some(3), 0));
        let (none, same) = FleetEvents::default().owed("a", "u", &[], None);
        assert!(none.is_empty());
        assert!(same.is_empty());
    }

    #[test]
    fn a_long_summary_and_a_large_declaration_are_cut() {
        let long = "é".repeat(MAX_SUMMARY_CHARS + 1);
        let data = result_data(&result(1, &long));
        assert_eq!(data["summaryTruncated"], true);
        assert_eq!(
            data["summary"].as_str().unwrap().chars().count(),
            MAX_SUMMARY_CHARS
        );
        let big = json!({"kind": "dump", "text": "x".repeat(MAX_DECLARATION_BYTES)});
        assert_eq!(
            declaration_data(3, &big),
            json!({"index": 3, "declaration": {"kind": "dump"}, "truncated": true})
        );
    }
}
