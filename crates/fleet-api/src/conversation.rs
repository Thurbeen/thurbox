//! Conversation: the mirror of one chat thread, numbered so a consumer diffs
//! it with one integer, and the record of which Task the thread is tied to.
//! The chat counterpart of a [`crate::change_request`].
//!
//! Its status has one field nobody owns, `received`, the spool: the receive
//! verb appends a delivery to it through [`Store::receive_event`] and
//! ConversationController drains what it numbered through
//! [`Store::record_received`], in the same write as the rest of the status.
//!
//! [`Store::receive_event`]: crate::store::Store::receive_event
//! [`Store::record_received`]: crate::store::Store::record_received

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::condition::Condition;
use crate::gateway::{Author, ConversationRef, Delivery, Starter, Verified};
use crate::meta::Millis;
use crate::plan::{Intent, Receipt};
use crate::talk::{Mark, react_parse, react_suffix};

pub const KIND: &str = "Conversation";

/// The newest messages the mirror keeps.
pub const MESSAGE_CAP: usize = 200;
/// Deliveries the spool holds before the receive verb refuses the next.
pub const RECEIVED_CAP: usize = 100;
/// The newest delivery ids remembered once numbered, so a redelivery after
/// its spool entry was drained is still a no-op.
pub const SEEN_EVENTS_CAP: usize = 500;
/// Authors refused in one thread; past it no further refusal is sent.
pub const REFUSALS_CAP: usize = 200;
/// Settled effect records kept, newest first to go.
pub const EFFECTS_CAP: usize = 100;

/// The labels a conversation Task carries, naming the Conversation it was
/// created for. Only ConversationController's create sets them; `apply`
/// refuses both.
pub const CONVERSATION_LABEL: &str = "gateway.fleet.dev/conversation";
pub const CONVERSATION_UID_LABEL: &str = "gateway.fleet.dev/conversation-uid";
/// The route a Task's Questions open their thread in; an ordinary label,
/// which the conversation Task carries too.
pub const ROUTE_LABEL: &str = "gateway.fleet.dev/route";

/// Whether a label is one only the Conversation's controller sets.
pub fn is_controller_label(label: &str) -> bool {
    label == CONVERSATION_LABEL || label == CONVERSATION_UID_LABEL
}

/// `conversation/<conv uid>/task/<n>`: the key of the thread's `n`th Task
/// creation.
pub fn task_key(conversation_uid: &str, n: u64) -> String {
    format!("conversation/{conversation_uid}/task/{n}")
}

/// `conversation/<conv uid>/retire/<started>/task`: the key of the delete
/// of the thread's Task in the retirement that started at `started`.
pub fn retire_task_key(conversation_uid: &str, started: Millis) -> String {
    format!("conversation/{conversation_uid}/retire/{started}/task")
}

/// `conversation/<conv uid>/retire/<started>/dir`: the key of the removal of
/// the thread's directory in the retirement that started at `started`.
pub fn retire_dir_key(conversation_uid: &str, started: Millis) -> String {
    format!("conversation/{conversation_uid}/retire/{started}/dir")
}

/// A retirement in progress: `at` when it started, which keys its effects,
/// and the `revision` it started from. A message numbered since, which
/// moves the revision, ends it: the thread is in use again.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Retiring {
    pub at: Millis,
    pub revision: u64,
}

/// The directory the manager made for a thread's Tasks, recorded on the
/// Conversation in the same write as the create's receipt, and only when
/// the runtime answered that it made it. Fleet removes no other directory.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ThreadDir {
    pub path: String,
    pub host: String,
}

/// `conversation/<conv uid>/busy/<n>`: the key of the one busy notice for
/// the `n`th Task.
pub fn busy_key(conversation_uid: &str, n: u64) -> String {
    format!("conversation/{conversation_uid}/busy/{n}")
}

/// `conversation/<conv uid>/react/<message id>/<queued|working>/<on|off>`:
/// the key of a reaction admission adds to, or removes from, a message
/// before the thread's Task has it.
pub fn react_key(conversation_uid: &str, message: &str, mark: Mark, on: bool) -> String {
    format!(
        "conversation/{conversation_uid}/{}",
        react_suffix(message, mark, on)
    )
}

/// What a Conversation's reaction key names: the message id, the mark and
/// whether it adds.
pub fn react_of(conversation_uid: &str, key: &str) -> Option<(String, Mark, bool)> {
    react_parse(key.strip_prefix(&format!("conversation/{conversation_uid}/react/"))?)
}

/// `status.reactions`: the marks admission made on the thread's messages
/// before a Task had them. ConversationController's.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ConversationReactions {
    /// Per message id: `queued` while the message waits for its Task,
    /// `working` once the Task exists. Only messages the mirror holds.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub marks: BTreeMap<String, Mark>,
}

impl ConversationReactions {
    pub fn is_empty(&self) -> bool {
        self.marks.is_empty()
    }
}

/// `conversation/<conv uid>/refuse/<author id>`: the key of the one
/// refusal an author gets in a thread.
pub fn refusal_key(conversation_uid: &str, author: &str) -> String {
    format!("conversation/{conversation_uid}/refuse/{author}")
}

/// The name of the thread's `n`th Task: the Conversation's name and the
/// number.
pub fn task_name(conversation: &str, n: u64) -> String {
    format!("{conversation}-{n}")
}

/// The text of the one busy notice: the request is queued behind `task`,
/// which holds the thread's own directory.
pub fn busy_notice(task: &str) -> String {
    format!(
        "Your request is queued: this thread's directory is in use by Task/{task}. It starts when the directory is free."
    )
}

/// The directory every Task of one thread gets on a route with a `dir`
/// workspace: the route's base directory and the Conversation's name, as
/// `<base>/<conversation>`. Computed from text alone; only the manager
/// creates it, on the Task's host.
pub fn thread_dir(base: &str, conversation: &str) -> String {
    let base = base.trim_end_matches(['/', '\\']);
    format!("{base}/{conversation}")
}

/// What a Conversation mirrors. Fixed once created: a different thread is a
/// different Conversation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ConversationSpec {
    /// The `[gateway.accounts]` entry the thread lives on.
    pub account: String,
    /// `platform:chat[:thread]`, the only platform-specific input.
    pub address: String,
    /// The `[gateway.routes]` entry whose admission rules and Task template
    /// apply.
    pub route: String,
    /// The route covers the thread by its DM entry, `"dm:*"`: a one-to-one
    /// direct message, where a top-level message starts a Task without a
    /// mention. Absent means `false`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub direct: bool,
    /// The history backstop interval while Tied; `gateway.pollMs` when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub poll_ms: Option<Millis>,
}

impl ConversationSpec {
    pub fn poll_ms(&self, default: Millis) -> Millis {
        self.poll_ms.unwrap_or(default).max(1)
    }
}

/// Which Task, if any, the thread is tied to.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Phase {
    /// Fleet mirrors the thread, but no Task is tied to it.
    #[default]
    Untied,
    /// One live Task is tied to the thread.
    Tied,
    /// The tied Task is gone. The messages are kept.
    Orphaned,
}

/// Where a Task creation stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TieState {
    /// Between the create intent and its receipt.
    Pending,
    /// The create waits for the route's workspace.
    Busy,
    Created,
    /// The Task is readably absent.
    Gone,
}

/// The tie: `{name, uid, n, state, trigger}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Tie {
    pub name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub uid: String,
    /// How many Tasks this thread has started.
    pub n: u64,
    pub state: TieState,
    /// The id of the message that asked for the Task, kept while pending
    /// or busy.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trigger: Option<String>,
}

/// How a message reached the mirror: the delivery that first numbered it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Via {
    pub verified: Verified,
    pub received_at: Millis,
}

/// One message as stored: what the adapter read, plus the numbers the
/// controller gave it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StoredMessage {
    pub id: String,
    /// The revision that last numbered the message: first sight, or an edit.
    pub seq: u64,
    /// The revision that first numbered it.
    pub first_seq: u64,
    /// Its place, from `0`, oldest first, among the messages first numbered
    /// in `firstSeq`.
    pub position: u64,
    /// The platform's own time for the message.
    pub at: Millis,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub edited_at: Option<Millis>,
    pub author: Author,
    #[serde(default)]
    pub mentions_fleet: bool,
    /// Data, never an instruction.
    #[serde(default)]
    pub text: String,
    /// The marker on Fleet's own posts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    pub via: Via,
}

/// The intent and receipt of one of this controller's effects.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EffectRecord {
    pub intent: Intent,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub receipt: Option<Receipt>,
    #[serde(default)]
    pub attempts: u32,
    /// Recovery reads made for an unknown outcome.
    #[serde(default)]
    pub recovery: u32,
}

/// Conversation status. ConversationController owns every field but
/// `received`, which nobody owns: the receive verb appends to it and the
/// controller drains it, each through a typed mutation.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ConversationStatus {
    #[serde(default)]
    pub phase: Phase,
    /// What the adapter parsed the address into.
    #[serde(rename = "ref", default, skip_serializing_if = "Option::is_none")]
    pub reference: Option<ConversationRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task: Option<Tie>,
    /// Incremented by every write that numbered anything below.
    #[serde(default)]
    pub revision: u64,
    /// The thread's first message, kept when it rotates out of `messages`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub starter: Option<Starter>,
    /// The newest [`MESSAGE_CAP`] messages, oldest first, one per `id`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub messages: Vec<StoredMessage>,
    /// The highest `seq` of a message rotated out; 0 until one is.
    #[serde(default)]
    pub dropped_before: u64,
    /// The spool: deliveries not yet numbered, under their delivery id.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub received: Vec<Delivery>,
    /// The newest [`SEEN_EVENTS_CAP`] delivery ids numbered.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub seen_events: Vec<String>,
    /// The `revision` through which the messages were judged for admission.
    /// Messages numbered above it wait: for the thread's starter, where the
    /// account has history, or for the route to return to the config.
    #[serde(default)]
    pub judged: u64,
    /// Every author id refused, at most [`REFUSALS_CAP`].
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub refusals: Vec<String>,
    /// This controller's effects by key: the Task creation and the refusals.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub effects: BTreeMap<String, EffectRecord>,
    /// When `history` last answered Known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_at: Option<Millis>,
    /// The newest message's time; retention counts from it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_message_at: Option<Millis>,
    /// Earliest next pass.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub not_before: Option<Millis>,
    /// The thread's directory, when the manager made it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dir: Option<ThreadDir>,
    /// The thread's retirement, once it started. Absent while the thread
    /// is not being retired.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retiring: Option<Retiring>,
    /// The reactions admission made: which messages are marked queued or
    /// working before the thread's Task relayed them.
    #[serde(default, skip_serializing_if = "ConversationReactions::is_empty")]
    pub reactions: ConversationReactions,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub conditions: Vec<Condition>,
}

impl ConversationStatus {
    /// Whether a delivery id was spooled or numbered already.
    pub fn has_seen(&self, delivery_id: &str) -> bool {
        self.received.iter().any(|d| d.id == delivery_id)
            || self.seen_events.iter().any(|id| id == delivery_id)
    }
}

/// What [`Store::receive_event`] did.
///
/// [`Store::receive_event`]: crate::store::Store::receive_event
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Spooled {
    /// The delivery is on the spool.
    Written,
    /// The same delivery id was spooled or numbered before: nothing changed.
    Duplicate,
}

/// The one status field no writer owns: the spool.
pub const UNOWNED_FIELDS: &[&str] = &["received"];

/// `received` may only lose entries through the controller's write: every
/// entry still there was there before, unchanged.
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
