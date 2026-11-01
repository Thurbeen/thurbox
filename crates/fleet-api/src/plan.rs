//! What a controller hands the manager: the status it wants, the effects it
//! needs, and when to look again. A controller never performs any of it.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::forge::{ChangeRef, ChangeState, Expected};
use crate::meta::{Millis, Object};
use crate::observation::Observation;
use crate::task::SessionRef;

/// One reconcile's answer.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Plan {
    /// The controller-owned status to write, compare-and-swap, before any
    /// effect runs. It carries the intents of `effects` (P5). `None` = unchanged.
    pub status: Option<Value>,
    /// Effects to run, in order, once `status` is written.
    pub effects: Vec<Effect>,
    /// Remove this finalizer once `status` is written and every effect ran.
    pub remove_finalizer: Option<String>,
    /// Delete this object, by its `uid` and `resourceVersion` under the
    /// lease: how a controller retires its own object once its retention
    /// passed. ChangeRequestController, ConversationController and
    /// ListenerController ask.
    pub delete: bool,
    /// An inbox entry proven delivered: its receipt goes into
    /// `status.deliveries` and the entry leaves `status.inbox` in the same
    /// write as `status`, through the store's record-and-remove mutation.
    pub delivery: Option<crate::inbox::DeliveryReceipt>,
    /// Inbox entries to append in the same write as `status`, through the
    /// store's relay mutation. ForgeController asks, its `status` being
    /// `status.forge` alone, and GatewayController, its `status` being
    /// `status.gateway` alone.
    pub appends: Vec<crate::inbox::InboxEntry>,
    /// Objects to create once `status` is written, each a keyed effect whose
    /// intent `status` carries and whose receipt the manager writes back
    /// under the key. ConversationController asks, for the Task a thread
    /// starts; a plugin's plan carries its own `creates`.
    pub creates: Vec<Create>,
}

/// One object a plan creates, under a deterministic key: a key that has a
/// receipt is never created again, and a missing receipt is answered by
/// looking for the object, never by creating it again.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Create {
    pub key: String,
    pub object: Object,
    /// The precondition the manager checks under the lease immediately
    /// before the create; refused as `WorkspaceBusy` when it does not hold.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exclusive: Option<Exclusive>,
}

/// `exclusive: {workspace}`: the create holds only while no Task has the
/// same normalised directory.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Exclusive {
    pub workspace: String,
}

/// An effect outside the store, under a deterministic key: a key that has a
/// receipt is never run again.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Effect {
    pub key: String,
    pub action: Action,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "type")]
pub enum Action {
    Spawn(SpawnSpec),
    Brief {
        session: SessionRef,
        text: String,
    },
    /// Delete the session: its checkout and conversation go with it.
    Release {
        session: SessionRef,
    },
    /// Start the agent of a session whose window is gone again, in its own
    /// workspace and conversation, under the same identity. A session that
    /// is running is left alone.
    Revive {
        session: SessionRef,
    },
    /// Take a draft change out of draft, through the Forge port: the only
    /// forge write Fleet performs. `expect` is the head and state the
    /// decision was made against; the adapter re-reads both immediately
    /// before the mutation and answers `Moved` instead when either differs.
    MarkReady {
        change: ChangeRef,
        expect: Expected,
    },
    /// Post `text` in a thread on `account`, through the Gateway port,
    /// carrying the effect's key as its marker. ConversationController asks
    /// for its refusal and its busy notice; GatewayController posts what a
    /// Task declares.
    Post {
        account: String,
        conversation: crate::gateway::ConversationRef,
        text: String,
    },
}

impl Action {
    pub fn name(&self) -> &'static str {
        match self {
            Action::Spawn(_) => "spawn",
            Action::Brief { .. } => "brief",
            Action::Release { .. } => "release",
            Action::Revive { .. } => "revive",
            Action::MarkReady { .. } => "markReady",
            Action::Post { .. } => "post",
        }
    }
}

/// The action name a create's intent and receipt carry.
pub const CREATE_ACTION: &str = "create";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SpawnSpec {
    pub host: String,
    pub key: String,
    pub harness: String,
    /// What the Task named; the runtime decides how to make it.
    pub workspace: crate::task::Workspace,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub env: std::collections::BTreeMap<String, String>,
    /// The manager already froze runtime model-variable selection with env.
    #[serde(default)]
    pub environment_frozen: bool,
}

/// Written before the effect runs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Intent {
    pub action: String,
    pub at: Millis,
    pub generation: u64,
}

/// Written after the effect ran, with what the adapter said.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Receipt {
    pub action: String,
    pub at: Millis,
    pub outcome: Observation<EffectOutcome>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "type")]
pub enum EffectOutcome {
    /// A session exists for the key; `adopted` when it existed already.
    Spawned {
        session: SessionRef,
        adopted: bool,
    },
    /// The brief is on the session; `already` when the session refused a
    /// second send of the same key.
    Delivered {
        already: bool,
    },
    Released,
    /// The runtime started the session's agent again, or found it running.
    Revived,
    /// The change is out of draft; `already` when the forge said it was no
    /// longer a draft by the time the mutation ran, inside the window
    /// between the adapter's fresh read and the mutation.
    MarkedReady {
        already: bool,
    },
    /// The change is no longer what the mark-ready effect expected: its head
    /// or state moved between the decision and the adapter's fresh read.
    /// Nothing was mutated, and this is not an attempt against the change.
    Moved {
        head: String,
        state: ChangeState,
    },
    /// A thread is open on the platform, through the Gateway port: its
    /// address, the id of the opening post and the platform's own time for
    /// it.
    Opened {
        conversation: crate::gateway::ConversationRef,
        message: String,
        at: Millis,
    },
    /// A post landed in a thread, through the Gateway port: its id and the
    /// platform's own time for it.
    Posted {
        message: String,
        at: Millis,
    },
    /// The object exists under the create's name with this `uid`; `adopted`
    /// when it was there already, created by an earlier attempt whose
    /// receipt was lost.
    Created {
        uid: String,
        adopted: bool,
    },
    /// The create's `exclusive: {workspace}` precondition did not hold:
    /// `task` has the directory. Nothing was created, and this is not an
    /// attempt against the key.
    WorkspaceBusy {
        task: String,
    },
    /// The adapter answered, and the answer is no.
    Failed {
        why: String,
    },
    /// The runtime maps no source for a repository the workspace names.
    /// Nothing was started: the runtime's configuration lacks the mapping,
    /// and adding it, not a retry, is the fix.
    Unmapped {
        url: String,
    },
    /// The runtime knows no workspace-trust interface for the harness.
    /// Nothing was started, for the same reason as `Unmapped`.
    TrustUnmapped {
        harness: String,
    },
}

/// What the controller needs to know before it can decide. The manager asks
/// the adapters, then calls the controller again with the answers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Probe {
    Host {
        host: String,
    },
    Find {
        host: String,
        key: String,
    },
    Ready {
        session: SessionRef,
    },
    /// Whether the session's agent trusts the Task's workspace.
    WorkspaceTrust {
        session: SessionRef,
        workspace: crate::task::Workspace,
    },
    BriefDelivered {
        session: SessionRef,
        key: String,
    },
    AtRest {
        session: SessionRef,
    },
    /// Whether the session's row is there but its agent's window is gone.
    Lost {
        session: SessionRef,
    },
    /// One complete observation of a change, through the Forge port.
    Change {
        change: ChangeRef,
    },
    /// A stored object, answered by the manager from the store: `Known(Some)`,
    /// `Known(None)` for proven absence, Unknown when the record is
    /// unreadable. Needs no lease renewal and runs before any runtime or
    /// forge probe.
    Object {
        kind: String,
        name: String,
    },
    /// Every stored object of a kind carrying all of `labels`, answered by
    /// the manager from the store: Known with the list, or Unknown when the
    /// listing is. For Tasks the manager answers from one listing per
    /// sweep. Needs no lease renewal.
    List {
        kind: String,
        labels: std::collections::BTreeMap<String, String>,
    },
    /// Fleet's own login on a forge host, through the Forge port; cached by
    /// the manager for the sweep.
    Whoami {
        host: String,
    },
    /// One read of a thread's history through the Gateway port.
    History {
        account: String,
        conversation: crate::gateway::ConversationRef,
        query: crate::gateway::HistoryQuery,
    },
    /// What the platform offers on an account, through the Gateway port;
    /// cached by the manager for the sweep.
    Capabilities {
        account: String,
    },
    /// Fleet's bot identity on an account, through the Gateway port; cached
    /// by the manager for the sweep.
    GatewayWhoami {
        account: String,
    },
}

/// The answers to the probes; `None` means "not asked".
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Observations {
    pub host: Option<Observation<bool>>,
    /// `Known(None)` means a readable list showed no such session.
    pub find: Option<Observation<Option<SessionRef>>>,
    pub ready: Option<Observation<bool>>,
    pub workspace_trust: Option<Observation<crate::runtime::WorkspaceTrust>>,
    pub brief_delivered: Option<Observation<bool>>,
    pub at_rest: Option<Observation<bool>>,
    pub lost: Option<Observation<bool>>,
}

/// `task/<name>/<uid>/<action>[/<attempt>]`: deterministic, so a repeat is
/// recognised, and scoped by uid, so a Task recreated under the same name
/// starts every key fresh. A Task's spec cannot change once created (only
/// `dispatch` may), so one uid is one brief and needs no generation in the key.
/// An inbox entry's key has the same shape (`send/<n>`) and is the intent key
/// its delivery runs under.
pub fn effect_key(kind: &str, name: &str, uid: &str, action: &str, attempt: Option<u32>) -> String {
    let kind = kind.to_ascii_lowercase();
    match attempt {
        Some(n) => format!("{kind}/{name}/{uid}/{action}/{n}"),
        None => format!("{kind}/{name}/{uid}/{action}"),
    }
}

/// The Task a `task/<name>/<uid>/...` key names, as `(name, uid)`: how a
/// marker on one of Fleet's posts is read back. `None` for any other key.
pub fn task_of_key(key: &str) -> Option<(&str, &str)> {
    let mut parts = key.split('/');
    if parts.next()? != "task" {
        return None;
    }
    let name = parts.next().filter(|n| !n.is_empty())?;
    let uid = parts.next().filter(|u| !u.is_empty())?;
    parts.next()?;
    Some((name, uid))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_task_key_names_its_task_and_other_keys_do_not() {
        let key = effect_key("Task", "t", "u7", "gateway/question/q1", None);
        assert_eq!(key, "task/t/u7/gateway/question/q1");
        assert_eq!(task_of_key(&key), Some(("t", "u7")));
        assert_eq!(task_of_key("task/t/u7/spawn"), Some(("t", "u7")));
        for other in ["conversation/c7/task/1", "task/t/u7", "task//u7/x", "", "x"] {
            assert_eq!(task_of_key(other), None, "{other:?}");
        }
        assert_eq!(CREATE_ACTION, "create");
        assert_eq!(
            Action::Post {
                account: "a".into(),
                conversation: crate::gateway::ConversationRef::split("fake:C1:t1").unwrap(),
                text: "t".into(),
            }
            .name(),
            "post"
        );
        let v = serde_json::to_value(EffectOutcome::WorkspaceBusy {
            task: "holder".into(),
        })
        .unwrap();
        assert_eq!(v["type"], "workspaceBusy");
        let v = serde_json::to_value(EffectOutcome::Created {
            uid: "u1".into(),
            adopted: true,
        })
        .unwrap();
        assert_eq!(v["type"], "created");
    }
}
