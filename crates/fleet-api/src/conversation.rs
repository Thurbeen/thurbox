use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::condition::Condition;
use crate::gateway::{Attachment, Author, ConversationRef, Delivery, Starter, Verified};
use crate::meta::Millis;
use crate::plan::{Intent, Receipt};
use crate::talk::{Mark, react_parse, react_suffix};

pub const KIND: &str = "Conversation";

pub const MESSAGE_CAP: usize = 200;
pub const RECEIVED_CAP: usize = 100;
pub const SEEN_EVENTS_CAP: usize = 500;
pub const REFUSALS_CAP: usize = 200;
pub const EFFECTS_CAP: usize = 100;

pub const CONVERSATION_LABEL: &str = "gateway.fleet.dev/conversation";
pub const CONVERSATION_UID_LABEL: &str = "gateway.fleet.dev/conversation-uid";
pub const ROUTE_LABEL: &str = "gateway.fleet.dev/route";

pub fn is_controller_label(label: &str) -> bool {
    label == CONVERSATION_LABEL || label == CONVERSATION_UID_LABEL
}

pub fn task_key(conversation_uid: &str, n: u64) -> String {
    format!("conversation/{conversation_uid}/task/{n}")
}

pub fn retire_task_key(conversation_uid: &str, started: Millis) -> String {
    format!("conversation/{conversation_uid}/retire/{started}/task")
}

pub fn retire_dir_key(conversation_uid: &str, started: Millis) -> String {
    format!("conversation/{conversation_uid}/retire/{started}/dir")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Retiring {
    pub at: Millis,
    pub revision: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ThreadDir {
    pub path: String,
    pub host: String,
}

pub fn busy_key(conversation_uid: &str, n: u64) -> String {
    format!("conversation/{conversation_uid}/busy/{n}")
}

pub fn react_key(conversation_uid: &str, message: &str, mark: Mark, on: bool) -> String {
    format!(
        "conversation/{conversation_uid}/{}",
        react_suffix(message, mark, on)
    )
}

pub fn react_of(conversation_uid: &str, key: &str) -> Option<(String, Mark, bool)> {
    react_parse(key.strip_prefix(&format!("conversation/{conversation_uid}/react/"))?)
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ConversationReactions {
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub marks: BTreeMap<String, Mark>,
}

impl ConversationReactions {
    pub fn is_empty(&self) -> bool {
        self.marks.is_empty()
    }
}

pub fn refusal_key(conversation_uid: &str, author: &str) -> String {
    format!("conversation/{conversation_uid}/refuse/{author}")
}

pub fn task_name(conversation: &str, n: u64) -> String {
    format!("{conversation}-{n}")
}

pub fn busy_notice(task: &str) -> String {
    format!(
        "Your request is queued: this thread's directory is in use by Task/{task}. It starts when the directory is free."
    )
}

pub fn thread_dir(base: &str, conversation: &str) -> String {
    let base = base.trim_end_matches(['/', '\\']);
    format!("{base}/{conversation}")
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ConversationSpec {
    pub account: String,
    pub address: String,
    pub route: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub direct: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub poll_ms: Option<Millis>,
}

impl ConversationSpec {
    pub fn poll_ms(&self, default: Millis) -> Millis {
        self.poll_ms.unwrap_or(default).max(1)
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Phase {
    #[default]
    Untied,
    Tied,
    Orphaned,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TieState {
    Pending,
    Busy,
    Created,
    Gone,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Tie {
    pub name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub uid: String,
    pub n: u64,
    pub state: TieState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trigger: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Via {
    pub verified: Verified,
    pub received_at: Millis,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StoredMessage {
    pub id: String,
    pub seq: u64,
    pub first_seq: u64,
    pub position: u64,
    pub at: Millis,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub edited_at: Option<Millis>,
    pub author: Author,
    #[serde(default)]
    pub mentions_fleet: bool,
    #[serde(default)]
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<Attachment>,
    pub via: Via,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EffectRecord {
    pub intent: Intent,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub receipt: Option<Receipt>,
    #[serde(default)]
    pub attempts: u32,
    #[serde(default)]
    pub recovery: u32,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ConversationStatus {
    #[serde(default)]
    pub phase: Phase,
    #[serde(rename = "ref", default, skip_serializing_if = "Option::is_none")]
    pub reference: Option<ConversationRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task: Option<Tie>,
    #[serde(default)]
    pub revision: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub starter: Option<Starter>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub messages: Vec<StoredMessage>,
    #[serde(default)]
    pub dropped_before: u64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub received: Vec<Delivery>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub seen_events: Vec<String>,
    #[serde(default)]
    pub judged: u64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub refusals: Vec<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub effects: BTreeMap<String, EffectRecord>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_at: Option<Millis>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_message_at: Option<Millis>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub not_before: Option<Millis>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dir: Option<ThreadDir>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retiring: Option<Retiring>,
    #[serde(default, skip_serializing_if = "ConversationReactions::is_empty")]
    pub reactions: ConversationReactions,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub conditions: Vec<Condition>,
}

impl ConversationStatus {
    pub fn has_seen(&self, delivery_id: &str) -> bool {
        self.received.iter().any(|d| d.id == delivery_id)
            || self.seen_events.iter().any(|id| id == delivery_id)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Spooled {
    Written,
    Duplicate,
}

pub const UNOWNED_FIELDS: &[&str] = &["received"];

pub fn check_drained(before: &[Delivery], after: &[Delivery]) -> Result<(), String> {
    for d in after {
        if !before.iter().any(|b| b == d) {
            return Err(format!(
                "status.received may only be drained by its controller; delivery {} was not spooled",
                d.id
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gateway::{AuthorKind, Event, Message};
    use serde_json::json;

    #[test]
    fn a_threads_directory_is_the_base_and_the_conversation_name() {
        for base in [
            "/work/conversations",
            "/work/conversations/",
            "/work/conversations//",
        ] {
            assert_eq!(
                thread_dir(base, "conv-3f9a1c0d2b7e"),
                "/work/conversations/conv-3f9a1c0d2b7e"
            );
        }
        assert_eq!(
            thread_dir("C:\\work\\conversations\\", "conv-3f9a1c0d2b7e"),
            "C:\\work\\conversations/conv-3f9a1c0d2b7e"
        );
    }

    fn delivery(id: &str) -> Delivery {
        Delivery {
            id: id.into(),
            verified: Verified::Connection,
            received_at: 5,
            direct: false,
            event: Event::Message {
                conversation: ConversationRef::split("fake:C1:t1").unwrap(),
                message: Message {
                    id: "m1".into(),
                    author: Author {
                        id: "U1".into(),
                        name: String::new(),
                        kind: AuthorKind::Person,
                    },
                    mentions_fleet: true,
                    at: 1,
                    text: "hi".into(),
                    key: None,
                    starts_thread: false,
                    files: Vec::new(),
                },
            },
        }
    }

    #[test]
    fn a_spec_is_the_resource_pages_shape_and_refuses_the_rest() {
        let spec: ConversationSpec = serde_json::from_value(json!({
            "account": "slack-main", "address": "slack:C0123ABCD:1730000000.000100", "route": "eng"
        }))
        .unwrap();
        assert_eq!(spec.poll_ms(7), 7);
        assert!(
            serde_json::from_value::<ConversationSpec>(json!({
                "account": "a", "address": "x:y", "route": "r", "retainMs": 1
            }))
            .is_err()
        );
    }

    #[test]
    fn missing_status_is_untied_at_revision_zero() {
        let st = ConversationStatus::default();
        assert_eq!(st.phase, Phase::Untied);
        assert_eq!(
            serde_json::to_value(&st).unwrap(),
            json!({"phase": "Untied", "revision": 0, "droppedBefore": 0, "judged": 0})
        );
        assert_eq!(serde_json::to_value(TieState::Gone).unwrap(), "gone");
    }

    #[test]
    fn a_stored_message_round_trips_in_the_pages_words() {
        let m = StoredMessage {
            id: "1".into(),
            seq: 2,
            first_seq: 1,
            position: 0,
            at: 10,
            edited_at: Some(11),
            author: Author {
                id: "U1".into(),
                name: "Alex".into(),
                kind: AuthorKind::Person,
            },
            mentions_fleet: true,
            text: "t".into(),
            key: None,
            files: Vec::new(),
            via: Via {
                verified: Verified::History,
                received_at: 12,
            },
        };
        let v = serde_json::to_value(&m).unwrap();
        assert_eq!(v["firstSeq"], 1);
        assert_eq!(v["via"], json!({"verified": "history", "receivedAt": 12}));
        assert_eq!(serde_json::from_value::<StoredMessage>(v).unwrap(), m);
    }

    #[test]
    fn the_spool_is_drained_never_grown_by_its_controller() {
        let st = ConversationStatus {
            received: vec![delivery("Ev1")],
            seen_events: vec!["Ev0".into()],
            ..ConversationStatus::default()
        };
        assert!(st.has_seen("Ev0") && st.has_seen("Ev1") && !st.has_seen("Ev2"));
        assert!(check_drained(&st.received, &[]).is_ok());
        assert!(check_drained(&st.received, &[delivery("Ev1")]).is_ok());
        assert!(check_drained(&st.received, &[delivery("Ev2")]).is_err());
        let mut changed = delivery("Ev1");
        changed.received_at = 6;
        assert!(check_drained(&st.received, &[changed]).is_err());
    }
}
