use serde::{Deserialize, Serialize};

use crate::condition::Condition;
use crate::forge::{
    ChangeRef, ChangeState, Checks, FailingCheck, Mergeable, Note, NoteKind, Review,
    ReviewDecision, Threads,
};
use crate::meta::Millis;

pub const KIND: &str = "ChangeRequest";

pub const DEFAULT_POLL_MS: Millis = 60_000;
pub const DEFAULT_CLOSED_GRACE_MS: Millis = 604_800_000;
pub const DEFAULT_SETTLED_TTL_MS: Millis = 2_592_000_000;
pub const CLOSED_POLL_FACTOR: Millis = 10;
pub const NOTE_CAP: usize = 100;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ChangeRequestSpec {
    pub url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub poll_ms: Option<Millis>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub closed_grace_ms: Option<Millis>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub settled_ttl_ms: Option<Millis>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChangeRequestDefaults {
    pub closed_grace_ms: Millis,
    pub settled_ttl_ms: Millis,
}

impl Default for ChangeRequestDefaults {
    fn default() -> Self {
        ChangeRequestDefaults {
            closed_grace_ms: DEFAULT_CLOSED_GRACE_MS,
            settled_ttl_ms: DEFAULT_SETTLED_TTL_MS,
        }
    }
}

impl ChangeRequestSpec {
    pub fn poll_ms(&self) -> Millis {
        self.poll_ms.unwrap_or(DEFAULT_POLL_MS).max(1)
    }

    pub fn closed_grace_ms(&self, defaults: &ChangeRequestDefaults) -> Millis {
        self.closed_grace_ms.unwrap_or(defaults.closed_grace_ms)
    }

    pub fn settled_ttl_ms(&self, defaults: &ChangeRequestDefaults) -> Millis {
        self.settled_ttl_ms.unwrap_or(defaults.settled_ttl_ms)
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Phase {
    #[default]
    Watching,
    Closed,
    Settled,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Sequenced<T> {
    pub value: T,
    pub seq: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Head {
    pub id: String,
    pub checks: Checks,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub failing: Vec<FailingCheck>,
    pub seq: u64,
    pub first_seen_at: Millis,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StoredNote {
    pub id: String,
    pub seq: u64,
    pub kind: NoteKind,
    pub author: String,
    #[serde(default)]
    pub bot: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<u64>,
    pub at: Millis,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub edited_at: Option<Millis>,
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub body: String,
}

impl StoredNote {
    pub fn seen(note: Note, seq: u64) -> StoredNote {
        StoredNote {
            id: note.id,
            seq,
            kind: note.kind,
            author: note.author,
            bot: note.bot,
            thread: note.thread,
            resolved: note.resolved,
            path: note.path,
            line: note.line,
            at: note.at,
            edited_at: note.edited_at,
            url: note.url,
            body: note.body,
        }
    }

    pub fn as_seen(&self) -> Note {
        Note {
            id: self.id.clone(),
            kind: self.kind,
            author: self.author.clone(),
            bot: self.bot,
            thread: self.thread.clone(),
            resolved: self.resolved,
            path: self.path.clone(),
            line: self.line,
            at: self.at,
            edited_at: self.edited_at,
            url: self.url.clone(),
            body: self.body.clone(),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ChangeRequestStatus {
    // Bookkeeping.
    #[serde(default)]
    pub phase: Phase,
    #[serde(rename = "ref", default, skip_serializing_if = "Option::is_none")]
    pub reference: Option<ChangeRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_at: Option<Millis>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub not_before: Option<Millis>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub closed_at: Option<Millis>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub settled_at: Option<Millis>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub conditions: Vec<Condition>,

    // The snapshot.
    #[serde(default)]
    pub revision: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<Sequenced<ChangeState>>,
    #[serde(default)]
    pub ever_ready: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head: Option<Head>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review_decision: Option<Sequenced<ReviewDecision>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mergeable: Option<Mergeable>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conflicts: Option<Sequenced<bool>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub threads: Option<Threads>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reviews: Vec<Review>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<StoredNote>,
    #[serde(default)]
    pub dropped_before: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_spec_takes_its_defaults_from_the_resource_and_the_config() {
        let spec: ChangeRequestSpec =
            serde_json::from_value(serde_json::json!({"url": "https://forge.example/p/1"}))
                .unwrap();
        let defaults = ChangeRequestDefaults::default();
        assert_eq!(spec.poll_ms(), DEFAULT_POLL_MS);
        assert_eq!(spec.closed_grace_ms(&defaults), DEFAULT_CLOSED_GRACE_MS);
        assert_eq!(spec.settled_ttl_ms(&defaults), DEFAULT_SETTLED_TTL_MS);
        let overridden = ChangeRequestDefaults {
            closed_grace_ms: 5,
            settled_ttl_ms: 6,
        };
        assert_eq!(spec.closed_grace_ms(&overridden), 5);
        assert_eq!(spec.settled_ttl_ms(&overridden), 6);
    }

    #[test]
    fn missing_status_is_watching_at_revision_zero() {
        let st = ChangeRequestStatus::default();
        assert_eq!(st.phase, Phase::Watching);
        assert_eq!(st.revision, 0);
        assert!(!st.ever_ready);
        let v = serde_json::to_value(&st).unwrap();
        assert_eq!(
            v,
            serde_json::json!({"phase": "Watching", "revision": 0, "everReady": false, "droppedBefore": 0})
        );
    }

    #[test]
    fn a_spec_refuses_fields_it_does_not_know() {
        let err = serde_json::from_value::<ChangeRequestSpec>(
            serde_json::json!({"url": "https://forge.example/p/1", "relayNotes": true}),
        )
        .unwrap_err();
        assert!(err.to_string().contains("unknown field"), "{err}");
    }
}
