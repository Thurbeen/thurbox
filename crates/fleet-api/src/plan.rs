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
    /// Make the Task's workspace of `urls` under `key` on `host`, through
    /// the runtime's `workspace` call, before the first start.
    Materialize {
        host: String,
        key: String,
        urls: Vec<String>,
    },
    /// Remove the workspace the runtime made, through its `stop` with
    /// `release`, once its session is released.
    CleanWorkspace {
        workspace: crate::workspace::WorkspaceRef,
    },
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
    /// `conversation`: the harness's own id its hooks last reported, which
    /// the harness profile's resume argv names.
    Revive {
        session: SessionRef,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        conversation: Option<String>,
        /// What a start of the agent needs, as its spawn had it: a runtime
        /// that starts a program starts the resume argv from it.
        launch: SpawnSpec,
    },
    /// Stop the agent of an idle session and keep its row, its workspace
    /// and its conversation. A stopped session is left alone.
    Pause {
        session: SessionRef,
    },
    /// Start the agent of a stopped session again, in its own workspace and
    /// conversation, under the same identity. A running session is left
    /// alone.
    Resume {
        session: SessionRef,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        conversation: Option<String>,
        /// What a start of the agent needs, as its spawn had it: a runtime
        /// that starts a program starts the resume argv from it.
        launch: SpawnSpec,
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
    /// Open a thread in `channel` on `account`, through the Gateway port,
    /// whose first post is `text` and carries the effect's key as its
    /// marker. GatewayController asks, for a Question from an untied Task;
    /// `route` is the route whose channel it is.
    Open {
        account: String,
        route: String,
        channel: crate::gateway::ConversationRef,
        text: String,
    },
    /// Post `text` at the top level of the log `channel` on `account`,
    /// through the Gateway port's `post` to a channel address, carrying the
    /// effect's key as its marker: no thread is opened and no Conversation
    /// is created. `thread` is the tied thread the post names; the manager
    /// adds it as a link where its adapter can make one, else by address.
    /// GatewayController asks, for a Task's result and change news.
    Log {
        account: String,
        channel: crate::gateway::ConversationRef,
        text: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        thread: Option<crate::gateway::LogThread>,
    },
    /// Add (`on`) or remove Fleet's own `reaction` on `message` in
    /// `conversation` on `account`, through the Gateway port. A reaction is
    /// set, not counted, so a repeat under the same key changes nothing.
    /// GatewayController asks, to mark a person's message working or
    /// replied.
    React {
        account: String,
        conversation: crate::gateway::ConversationRef,
        message: String,
        reaction: String,
        on: bool,
    },
    /// Delete the conversation Task a retiring thread created, through the
    /// store, fenced on its `uid`: only a Task carrying the Conversation's
    /// uid label, and only while it is Concluded. Its finalizer then
    /// releases its session. ConversationController asks.
    DeleteTask {
        name: String,
        uid: String,
    },
    /// Remove the thread's directory `path` on `host`, through the runtime
    /// port: only the directory the manager recorded making for the thread,
    /// and only while no Task names it. ConversationController asks, as a
    /// thread retires.
    RemoveDir {
        host: String,
        path: String,
    },
    /// Add Fleet's opening post to the opened thread's Conversation, as the
    /// receive verb would, creating the Conversation under the name its
    /// account and address derive: through the store, never a platform.
    Receive {
        account: String,
        route: String,
        delivery: crate::gateway::Delivery,
    },
}

impl Action {
    /// The start this action makes: a spawn's, or a revive's or a resume's
    /// again. The manager completes each with the environment it froze.
    pub fn launch_mut(&mut self) -> Option<&mut SpawnSpec> {
        match self {
            Action::Spawn(spec)
            | Action::Revive { launch: spec, .. }
            | Action::Resume { launch: spec, .. } => Some(spec),
            _ => None,
        }
    }

    pub fn name(&self) -> &'static str {
        match self {
            Action::Materialize { .. } => "materialize",
            Action::CleanWorkspace { .. } => "cleanWorkspace",
            Action::Spawn(_) => "spawn",
            Action::Brief { .. } => "brief",
            Action::Release { .. } => "release",
            Action::Revive { .. } => "revive",
            Action::Pause { .. } => "pause",
            Action::Resume { .. } => "resume",
            Action::MarkReady { .. } => "markReady",
            Action::Post { .. } => "post",
            Action::Open { .. } => "open",
            Action::Receive { .. } => "receive",
            Action::Log { .. } => "log",
            Action::React { .. } => "react",
            Action::DeleteTask { .. } => "deleteTask",
            Action::RemoveDir { .. } => "removeDir",
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
    /// What the Task named.
    pub workspace: crate::task::Workspace,
    /// The workspace the runtime made for this session: the session runs in
    /// its first directory and spans the others. `None` for an empty list:
    /// the session runs in the runtime's default directory.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ready: Option<crate::workspace::WorkspaceRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub env: std::collections::BTreeMap<String, String>,
    /// The manager already froze runtime model-variable selection with env.
    #[serde(default)]
    pub environment_frozen: bool,
}

impl SpawnSpec {
    /// The harness profile's launch argv: its hooks, the trust of the
    /// directory the session starts in and each other directory it spans.
    pub fn launch_argv(&self) -> Result<Vec<String>, String> {
        let (extra, root) = match &self.ready {
            Some(r) => (r.extra_dirs().to_vec(), r.dir()),
            None => (Vec::new(), None),
        };
        crate::harness::profile(&self.harness).launch(&extra, root)
    }
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
    /// second send of the same key. `via` names the runtime's route when it
    /// said; `may_hold` when that route can hold the text behind a dialog.
    Delivered {
        already: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        via: Option<String>,
        #[serde(
            default,
            rename = "mayHold",
            skip_serializing_if = "std::ops::Not::not"
        )]
        may_hold: bool,
    },
    /// The runtime kept the text in a queue that its agent does not read:
    /// not a delivery. The text is there already, so it is never sent again.
    Queued {
        why: String,
    },
    Released,
    /// The runtime started the session's agent again, or found it running.
    Revived,
    /// The runtime stopped the session's agent, or found it stopped.
    Paused,
    /// The runtime started the stopped session's agent again, or found it
    /// running.
    Resumed,
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
    /// The reaction is added or removed, through the Gateway port;
    /// `already` when it was already there, or already absent.
    Reacted {
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        already: bool,
    },
    /// Fleet's opening post is on the opened thread's Conversation, now or
    /// before (a duplicate delivery): its name and `uid`.
    Received {
        conversation: String,
        uid: String,
    },
    /// The object exists under the create's name with this `uid`; `adopted`
    /// when it was there already, created by an earlier attempt whose
    /// receipt was lost.
    Created {
        uid: String,
        adopted: bool,
    },
    /// The Task is deleted or being deleted, or the directory is gone: now
    /// or before.
    Deleted,
    /// The create's `exclusive: {workspace}` precondition did not hold, or a
    /// directory to remove is still named: `task` has the directory. Nothing was created, and this is not an
    /// attempt against the key.
    WorkspaceBusy {
        task: String,
    },
    /// The adapter answered, and the answer is no.
    Failed {
        why: String,
    },
    /// The workspace is whole under the effect's key; `adopted` when a whole
    /// one was there already.
    Materialized {
        workspace: Box<crate::workspace::WorkspaceRef>,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        adopted: bool,
    },
    /// The host's runtime does not handle a URL the workspace names: its
    /// manifest lists no `workspace` verb, or not the URL's scheme, or it
    /// answered so. Nothing was made: a runtime that handles the URL, not a
    /// retry, is the fix.
    Unsupported {
        url: String,
    },
    /// The runtime knows no workspace-trust interface for the harness.
    /// Nothing was started, for the same reason as `Unsupported`.
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
    /// Whether the session's agent trusts the workspace the runtime made for
    /// it (`ready`), or its default directory.
    WorkspaceTrust {
        session: SessionRef,
        workspace: crate::task::Workspace,
        ready: Option<crate::workspace::WorkspaceRef>,
    },
    /// The whole workspace of `urls` under `key` on `host`: the runtime's
    /// `workspace` call again, which is idempotent by key.
    FindWorkspace {
        host: String,
        key: String,
        urls: Vec<String>,
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
    /// Whether the session's agent is stopped, its row kept.
    Paused {
        session: SessionRef,
    },
    /// What the session's agent reported through `fleet signal`: whether it
    /// is working or idle, the prompts it was given, its conversation.
    Activity {
        session: SessionRef,
    },
    /// The session's screen, through the runtime's `capture`, for the
    /// screen rules of a harness whose hooks do not say its state now.
    Screen {
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
    pub paused: Option<Observation<bool>>,
    /// The agent's activity, as its hooks last reported it.
    pub activity: Option<Observation<crate::runtime::Activity>>,
    /// Everything the agent's hooks reported, folded.
    pub signals: Option<Observation<crate::signal::Reported>>,
    /// The session's screen; `Known(None)` when the runtime holds no
    /// process under the key.
    pub screen: Option<Observation<Option<crate::screen::Screen>>>,
    /// `Known(None)` means the runtime proved it made no workspace under the
    /// key.
    pub workspace: Option<Observation<Option<crate::workspace::WorkspaceRef>>>,
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
