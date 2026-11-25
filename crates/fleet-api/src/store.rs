use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

use crate::meta::{Millis, Object};
use crate::observation::Observation;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum FieldManager {
    Apply,
    Worker,
    TaskController,
    ChangeRequestController,
    ForgeController,
    Cli,
    GatewayController,
    Plugin(String),
    ConversationController,
    ListenerController,
    CronTaskController,
    Listener(String),
}

impl FieldManager {
    pub fn name(&self) -> String {
        match self {
            FieldManager::Apply => "apply",
            FieldManager::Worker => "worker",
            FieldManager::TaskController => "task-controller",
            FieldManager::ChangeRequestController => "change-request-controller",
            FieldManager::ForgeController => "forge-controller",
            FieldManager::Cli => "cli",
            FieldManager::GatewayController => "gateway-controller",
            FieldManager::Plugin(name) => return format!("plugin/{name}"),
            FieldManager::ConversationController => "conversation-controller",
            FieldManager::ListenerController => "listener-controller",
            FieldManager::CronTaskController => "cron-task-controller",
            FieldManager::Listener(account) => return format!("listener/{account}"),
        }
        .into()
    }

    pub fn is_controller(&self) -> bool {
        matches!(
            self,
            FieldManager::TaskController
                | FieldManager::ChangeRequestController
                | FieldManager::ForgeController
                | FieldManager::GatewayController
                | FieldManager::Plugin(_)
                | FieldManager::ConversationController
                | FieldManager::ListenerController
                | FieldManager::CronTaskController
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LeaseToken {
    pub holder: String,
    pub token: u64,
    pub expires_at: Millis,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum StoreError {
    #[error("{0} not found")]
    NotFound(String),
    #[error("{0} already exists")]
    AlreadyExists(String),
    #[error("conflict on {key}: expected resourceVersion {expected}, found {actual}")]
    Conflict {
        key: String,
        expected: u64,
        actual: u64,
    },
    #[error("forbidden: {0}")]
    Forbidden(String),
    #[error("invalid: {0}")]
    Invalid(String),
    #[error("unsupported: {0}")]
    Unsupported(String),
    #[error("lease lost: {0}")]
    LeaseLost(String),
    #[error("lease held by {holder} until {expires_at}")]
    LeaseHeld { holder: String, expires_at: Millis },
    #[error("store busy: {0}")]
    Busy(String),
    #[error("conflict: {0}")]
    Precondition(String),
    #[error("could not tell: {0}")]
    Unknown(String),
    #[error("disk full at {0}")]
    Full(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DeleteOutcome {
    Marked,
    Removed,
}

pub trait Store {
    fn plugin_mutate(
        &self,
        _mutation: crate::plugin::Mutation,
        _by: FieldManager,
        _lease: &LeaseToken,
    ) -> Result<Value, StoreError> {
        Err(StoreError::Unsupported("plugin verbs".into()))
    }

    fn served_kinds(&self) -> Vec<String> {
        crate::kinds::BUILT_IN
            .iter()
            .map(|k| (*k).to_string())
            .collect()
    }
    fn validate_plugin_status(&self, _kind: &str, _status: &Value) -> Result<(), StoreError> {
        Err(StoreError::Unsupported("plugin status".into()))
    }

    fn launch(&self, _task: &Object) -> Result<Option<crate::config::Execution>, StoreError> {
        Err(StoreError::Unsupported(
            "private launches unsupported".into(),
        ))
    }
    fn freeze_launch(
        &self,
        _task: &Object,
        _execution: &crate::config::Execution,
        _lease: &LeaseToken,
    ) -> Result<crate::config::Execution, StoreError> {
        Err(StoreError::Unsupported(
            "private launches unsupported".into(),
        ))
    }

    fn get(&self, kind: &str, name: &str) -> Observation<Option<Object>>;
    fn list(&self, kind: &str) -> Observation<Vec<Object>>;
    fn create(&self, obj: Object, by: FieldManager) -> Result<Object, StoreError>;
    fn create_fenced(
        &self,
        _obj: Object,
        _by: FieldManager,
        _lease: &LeaseToken,
    ) -> Result<Object, StoreError> {
        Err(StoreError::Unsupported("create_fenced".into()))
    }
    fn retire_conversation_task(
        &self,
        _name: &str,
        _uid: &str,
        _conversation_uid: &str,
        _by: FieldManager,
        _lease: &LeaseToken,
    ) -> Result<DeleteOutcome, StoreError> {
        Err(StoreError::Unsupported("retire_conversation_task".into()))
    }
    fn retire_scheduled_task(
        &self,
        _name: &str,
        _uid: &str,
        _cron_uid: &str,
        _running: bool,
        _by: FieldManager,
        _lease: &LeaseToken,
    ) -> Result<DeleteOutcome, StoreError> {
        Err(StoreError::Unsupported("retire_scheduled_task".into()))
    }
    fn update_spec(
        &self,
        kind: &str,
        name: &str,
        spec: Value,
        labels: Option<std::collections::BTreeMap<String, String>>,
        expected: u64,
        by: FieldManager,
    ) -> Result<Object, StoreError>;
    fn update_status(
        &self,
        kind: &str,
        name: &str,
        status: Value,
        expected: u64,
        by: FieldManager,
        lease: Option<&LeaseToken>,
    ) -> Result<Object, StoreError>;
    fn delete(&self, kind: &str, name: &str, by: FieldManager)
    -> Result<DeleteOutcome, StoreError>;
    fn delete_fenced(
        &self,
        kind: &str,
        name: &str,
        uid: &str,
        expected: u64,
        by: FieldManager,
        lease: &LeaseToken,
    ) -> Result<DeleteOutcome, StoreError>;
    fn append_inbox(
        &self,
        name: &str,
        entry: crate::inbox::InboxEntry,
        expected: u64,
        by: FieldManager,
        lease: Option<&LeaseToken>,
    ) -> Result<Object, StoreError>;
    fn resolve_inbox(
        &self,
        _name: &str,
        _key: &str,
        _retry: Option<crate::inbox::Retry>,
        _expected: u64,
        _by: FieldManager,
    ) -> Result<Object, StoreError> {
        Err(StoreError::Unsupported("resolve_inbox".into()))
    }
    fn request_wake(
        &self,
        _name: &str,
        _at: crate::meta::Millis,
        _expected: u64,
        _by: FieldManager,
    ) -> Result<Object, StoreError> {
        Err(StoreError::Unsupported("request_wake".into()))
    }
    fn request_pause(
        &self,
        _name: &str,
        _at: crate::meta::Millis,
        _expected: u64,
        _by: FieldManager,
    ) -> Result<Object, StoreError> {
        Err(StoreError::Unsupported("request_pause".into()))
    }
    fn request_restart(
        &self,
        _name: &str,
        _request: crate::task::RestartRequest,
        _expected: u64,
        _by: FieldManager,
    ) -> Result<Object, StoreError> {
        Err(StoreError::Unsupported("request_restart".into()))
    }
    fn amend_task(
        &self,
        _name: &str,
        _amendment: crate::task::Amendment,
        _entry: crate::inbox::InboxEntry,
        _expected: u64,
        _by: FieldManager,
    ) -> Result<Object, StoreError> {
        Err(StoreError::Unsupported("amend_task".into()))
    }
    fn record_delivery(
        &self,
        name: &str,
        status: Value,
        receipt: crate::inbox::DeliveryReceipt,
        expected: u64,
        by: FieldManager,
        lease: &LeaseToken,
    ) -> Result<Object, StoreError>;
    fn record_relay(
        &self,
        name: &str,
        status: Value,
        appends: Vec<crate::inbox::InboxEntry>,
        expected: u64,
        by: FieldManager,
        lease: &LeaseToken,
    ) -> Result<Object, StoreError>;
    fn receive_event(
        &self,
        _name: &str,
        _delivery: crate::gateway::Delivery,
        _expected: u64,
        _by: FieldManager,
    ) -> Result<(Object, crate::conversation::Spooled), StoreError> {
        Err(StoreError::Unsupported("receive_event".into()))
    }
    fn record_received(
        &self,
        _name: &str,
        _status: Value,
        _expected: u64,
        _by: FieldManager,
        _lease: &LeaseToken,
    ) -> Result<Object, StoreError> {
        Err(StoreError::Unsupported("record_received".into()))
    }
    fn record_beat(
        &self,
        _name: &str,
        _instance: &str,
        _beat: crate::listener::Beat,
        _expected: u64,
        _by: FieldManager,
    ) -> Result<Object, StoreError> {
        Err(StoreError::Unsupported("record_beat".into()))
    }
    fn record_refusal(
        &self,
        _name: &str,
        _reason: &str,
        _fault: bool,
        _at: Millis,
        _expected: u64,
        _by: FieldManager,
    ) -> Result<Object, StoreError> {
        Err(StoreError::Unsupported("record_refusal".into()))
    }
    fn append_events(
        &self,
        _board: &str,
        _source: &crate::event::Source,
        _events: Vec<crate::event::Publish>,
        _by: FieldManager,
        _lease: Option<&LeaseToken>,
    ) -> Result<Vec<crate::event::Appended>, StoreError> {
        Err(StoreError::Unsupported("append_events".into()))
    }
    fn read_events(
        &self,
        _board: &str,
        _after: u64,
        _limit: usize,
    ) -> Result<crate::event::Page, StoreError> {
        Err(StoreError::Unsupported("read_events".into()))
    }
    fn event_head(&self, _board: &str) -> Result<u64, StoreError> {
        Err(StoreError::Unsupported("event_head".into()))
    }
    fn remove_finalizer(
        &self,
        kind: &str,
        name: &str,
        finalizer: &str,
        expected: u64,
        by: FieldManager,
        lease: &LeaseToken,
    ) -> Result<DeleteOutcome, StoreError>;
    fn validate(&self, obj: &Object) -> Result<(), StoreError> {
        crate::validate::validate_object(obj)
    }
    fn acquire_lease(&self, holder: &str, ttl: Millis) -> Result<LeaseToken, StoreError>;
    fn renew_lease(&self, lease: &LeaseToken, ttl: Millis) -> Result<LeaseToken, StoreError>;
    fn reclaim_lease(&self, lease: &LeaseToken, ttl: Millis) -> Result<LeaseToken, StoreError> {
        self.renew_lease(lease, ttl)
    }
    fn release_lease(&self, lease: &LeaseToken) -> Result<(), StoreError>;
}

pub trait Clock {
    fn now(&self) -> Millis;
}
