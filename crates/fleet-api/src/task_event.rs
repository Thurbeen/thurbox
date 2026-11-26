use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::event::Publish;
use crate::result::TaskResult;
use crate::task::Phase;

pub const PHASE: &str = "fleet.task.phase";
pub const DECLARED: &str = "fleet.task.declared";
pub const RESULT: &str = "fleet.task.result";

pub const MAX_PENDING: usize = 64;
pub const MAX_SUMMARY_CHARS: usize = 1024;
pub const MAX_DECLARATION_BYTES: usize = 4096;
pub const MAX_KIND_CHARS: usize = 256;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FleetEvents {
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub started: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pending: Vec<Publish>,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub phases: u64,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub declared: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<u64>,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub skipped: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_skipped: Option<String>,
}

fn is_zero(n: &u64) -> bool {
    *n == 0
}

pub fn key(uid: &str, series: &str, n: u64) -> String {
    format!("task/{uid}/{series}/{n}")
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

    pub fn queue_phase(
        &mut self,
        task: &str,
        uid: &str,
        from: Phase,
        to: Phase,
        reason: Option<&str>,
    ) {
        self.started = true;
        self.phases += 1;
        let mut data = json!({"from": from, "to": to});
        if let Some(reason) = reason {
            data["reason"] = json!(reason);
        }
        self.pending
            .push(publish(PHASE, task, key(uid, "phase", self.phases), data));
        if self.pending.len() > MAX_PENDING {
            self.pending.remove(0);
        }
    }

    pub fn owed(
        &self,
        task: &str,
        uid: &str,
        declared: &[Value],
        result: Option<&TaskResult>,
    ) -> (Vec<Publish>, FleetEvents) {
        if self.is_empty() {
            let baseline = FleetEvents {
                started: true,
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
                key(uid, "declared", n),
                declaration_data(n, item),
            ));
        }
        next.declared = next.declared.max(declared.len() as u64);
        if let Some(r) = result.filter(|r| self.result != Some(r.generation)) {
            events.push(publish(
                RESULT,
                task,
                key(uid, "result", r.generation),
                result_data(r),
            ));
            next.result = Some(r.generation);
        }
        (events, next)
    }

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
        "declaration": {"kind": cut_kind(item.get("kind"))},
        "truncated": true,
    })
}

fn cut_kind(kind: Option<&Value>) -> Value {
    match kind {
        Some(Value::String(k)) => json!(k.chars().take(MAX_KIND_CHARS).collect::<String>()),
        _ => Value::Null,
    }
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
        assert_eq!(keys, ["task/u1/phase/1", "task/u1/phase/2"]);
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
        assert_eq!(f.pending[0].key.as_deref(), Some("task/u/phase/3"));
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
                "task/u/phase/1",
                "task/u/declared/0",
                "task/u/declared/1",
                "task/u/result/2"
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
        assert!(next.started);
        let (none, started) = FleetEvents::default().owed("a", "u", &[], None);
        assert!(none.is_empty());
        assert!(started.started && !started.is_empty(), "{started:?}");
        let (later, _) = started.owed("a", "u", &declared, None);
        assert_eq!(later.len(), 1, "{later:?}");
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
        let huge = json!({"kind": "k".repeat(20_000)});
        assert_eq!(
            declaration_data(0, &huge)["declaration"]["kind"],
            json!("k".repeat(MAX_KIND_CHARS))
        );
        let key = key(&"u".repeat(64), "declared", u64::MAX);
        assert!(key.len() <= crate::event::MAX_FIELD_CHARS, "{key}");
    }
}
