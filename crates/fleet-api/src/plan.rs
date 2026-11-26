use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::forge::{ChangeRef, ChangeState, Expected};
use crate::meta::{Millis, Object};
use crate::observation::Observation;
use crate::task::SessionRef;

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Plan {
    pub status: Option<Value>,
    pub effects: Vec<Effect>,
    pub remove_finalizer: Option<String>,
    pub delete: bool,
    pub delivery: Option<crate::inbox::DeliveryReceipt>,
    pub appends: Vec<crate::inbox::InboxEntry>,
    pub creates: Vec<Create>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Create {
    pub key: String,
    pub object: Object,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exclusive: Option<Exclusive>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Exclusive {
    pub workspace: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Effect {
    pub key: String,
    pub action: Action,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "type")]
pub enum Action {
    Materialize {
        host: String,
        key: String,
        urls: Vec<String>,
    },
    CleanWorkspace {
        workspace: crate::workspace::WorkspaceRef,
    },
    Spawn(SpawnSpec),
    Brief {
        session: SessionRef,
        text: String,
        inbox: Option<Vec<String>>,
    },
    Release {
        session: SessionRef,
    },
    Revive {
        session: SessionRef,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        conversation: Option<String>,
        launch: SpawnSpec,
    },
    Pause {
        session: SessionRef,
    },
    Resume {
        session: SessionRef,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        conversation: Option<String>,
        launch: SpawnSpec,
    },
    Restart {
        session: SessionRef,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        conversation: Option<String>,
        launch: SpawnSpec,
    },
    MarkReady {
        change: ChangeRef,
        expect: Expected,
    },
    Post {
        account: String,
        conversation: crate::gateway::ConversationRef,
        text: String,
    },
    Open {
        account: String,
        route: String,
        channel: crate::gateway::ConversationRef,
        text: String,
    },
    Log {
        account: String,
        channel: crate::gateway::ConversationRef,
        text: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        thread: Option<crate::gateway::LogThread>,
    },
    React {
        account: String,
        conversation: crate::gateway::ConversationRef,
        message: String,
        reaction: String,
        on: bool,
    },
    DeleteTask {
        name: String,
        uid: String,
    },
    RetireScheduled {
        name: String,
        uid: String,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        running: bool,
    },
    RemoveDir {
        host: String,
        path: String,
    },
    Receive {
        account: String,
        route: String,
        delivery: crate::gateway::Delivery,
    },
}

impl Action {
    pub fn launch_mut(&mut self) -> Option<&mut SpawnSpec> {
        match self {
            Action::Spawn(spec)
            | Action::Revive { launch: spec, .. }
            | Action::Resume { launch: spec, .. }
            | Action::Restart { launch: spec, .. } => Some(spec),
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
            Action::Restart { .. } => "restart",
            Action::MarkReady { .. } => "markReady",
            Action::Post { .. } => "post",
            Action::Open { .. } => "open",
            Action::Receive { .. } => "receive",
            Action::Log { .. } => "log",
            Action::React { .. } => "react",
            Action::DeleteTask { .. } => "deleteTask",
            Action::RemoveDir { .. } => "removeDir",
            Action::RetireScheduled { .. } => "retireScheduled",
        }
    }
}

pub const CREATE_ACTION: &str = "create";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SpawnSpec {
    pub host: String,
    pub key: String,
    pub harness: String,
    pub workspace: crate::task::Workspace,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ready: Option<crate::workspace::WorkspaceRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub env: std::collections::BTreeMap<String, String>,
    #[serde(default)]
    pub environment_frozen: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub telemetry_args: Vec<String>,
}

impl SpawnSpec {
    pub fn launch_argv(&self) -> Result<Vec<String>, String> {
        let (extra, root) = match &self.ready {
            Some(r) => (r.extra_dirs().to_vec(), r.dir()),
            None => (Vec::new(), None),
        };
        let mut argv = crate::harness::profile(&self.harness).launch(&extra, root)?;
        argv.extend(self.telemetry_args.iter().cloned());
        Ok(argv)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Intent {
    pub action: String,
    pub at: Millis,
    pub generation: u64,
}

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
    Spawned {
        session: SessionRef,
        adopted: bool,
    },
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
    Queued {
        why: String,
    },
    Released,
    Revived,
    Paused,
    Resumed,
    Restarted,
    RestartUnsupported {
        why: String,
    },
    MarkedReady {
        already: bool,
    },
    Moved {
        head: String,
        state: ChangeState,
    },
    Opened {
        conversation: crate::gateway::ConversationRef,
        message: String,
        at: Millis,
    },
    Posted {
        message: String,
        at: Millis,
    },
    Reacted {
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        already: bool,
    },
    Received {
        conversation: String,
        uid: String,
    },
    Created {
        uid: String,
        adopted: bool,
    },
    Deleted,
    WorkspaceBusy {
        task: String,
    },
    Failed {
        why: String,
    },
    Materialized {
        workspace: Box<crate::workspace::WorkspaceRef>,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        adopted: bool,
    },
    Unsupported {
        url: String,
    },
    TrustUnmapped {
        harness: String,
    },
}

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
    WorkspaceTrust {
        session: SessionRef,
        workspace: crate::task::Workspace,
        ready: Option<crate::workspace::WorkspaceRef>,
    },
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
    Lost {
        session: SessionRef,
    },
    Paused {
        session: SessionRef,
    },
    Activity {
        session: SessionRef,
    },
    Screen {
        session: SessionRef,
    },
    Change {
        change: ChangeRef,
    },
    Object {
        kind: String,
        name: String,
    },
    List {
        kind: String,
        labels: std::collections::BTreeMap<String, String>,
    },
    Whoami {
        host: String,
    },
    History {
        account: String,
        conversation: crate::gateway::ConversationRef,
        query: crate::gateway::HistoryQuery,
    },
    Capabilities {
        account: String,
    },
    GatewayWhoami {
        account: String,
    },
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Observations {
    pub host: Option<Observation<bool>>,
    pub find: Option<Observation<Option<SessionRef>>>,
    pub ready: Option<Observation<bool>>,
    pub workspace_trust: Option<Observation<crate::runtime::WorkspaceTrust>>,
    pub brief_delivered: Option<Observation<bool>>,
    pub at_rest: Option<Observation<bool>>,
    pub lost: Option<Observation<bool>>,
    pub paused: Option<Observation<bool>>,
    pub activity: Option<Observation<crate::runtime::Activity>>,
    pub signals: Option<Observation<crate::signal::Reported>>,
    pub screen: Option<Observation<Option<crate::screen::Screen>>>,
    pub workspace: Option<Observation<Option<crate::workspace::WorkspaceRef>>>,
    pub outbox_drained: Option<Observation<bool>>,
}

pub fn effect_key(kind: &str, name: &str, uid: &str, action: &str, attempt: Option<u32>) -> String {
    let kind = kind.to_ascii_lowercase();
    match attempt {
        Some(n) => format!("{kind}/{name}/{uid}/{action}/{n}"),
        None => format!("{kind}/{name}/{uid}/{action}"),
    }
}

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
