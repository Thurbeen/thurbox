//! The store port (P2, P3, Q15): get, exclusive create, compare-and-swap
//! update and list, plus the one manager lease.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

use crate::meta::{Millis, Object};
use crate::observation::Observation;

/// Who is writing. The store checks it against the kind's ownership table
/// (`kinds::status_owner`) on every status write and finalizer change.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum FieldManager {
    /// People and scripts writing spec through `fleet apply`.
    Apply,
    /// The worker answering through `fleet result`; owns `Task.status.result`.
    Worker,
    /// The one controller for Task; owns the rest of Task status.
    TaskController,
    /// The one controller for ChangeRequest; owns all of its status.
    ChangeRequestController,
    /// The second controller over Task; owns `Task.status.forge` and
    /// appends to `Task.status.inbox`.
    ForgeController,
    /// A person at the command line, through `fleet send`: appends to
    /// `Task.status.inbox` and owns nothing.
    Cli,
    /// The third controller over Task; owns `Task.status.gateway` and
    /// appends to `Task.status.inbox`.
    GatewayController,
    Plugin(String),
    /// The one controller for Conversation; owns all of its status but the
    /// spool, `received`, which it drains.
    ConversationController,
    /// The one controller for Listener; owns its status but the beats and
    /// refusals the receive verb records.
    ListenerController,
    /// An account's listener program, through `fleet gateway receive`:
    /// creates Conversations and spools deliveries on them, and records
    /// beats and refusals on its Listener. Owns nothing and needs no lease.
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
            FieldManager::Listener(account) => return format!("listener/{account}"),
        }
        .into()
    }

    /// Controllers write only while they hold the manager lease (fencing).
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
        )
    }
}

/// Proof of the manager lease. `token` increases every time the lease changes
/// hands, so a writer that lost it is refused even if it does not know yet.
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
    /// A caller's own precondition did not hold, such as `fleet amend
    /// --if-generation`; retrying the same call cannot succeed.
    #[error("conflict: {0}")]
    Precondition(String),
    /// The store could not tell: an unreadable or corrupt record. Never read
    /// as absence.
    #[error("could not tell: {0}")]
    Unknown(String),
}

/// What the store does to a delete request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DeleteOutcome {
    /// Finalizers remain: the object is marked and its controller releases it.
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

    /// Private immutable launch data, keyed by Task UID, never a resource/status.
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

    /// `Known(None)` only when the store can prove absence.
    fn get(&self, kind: &str, name: &str) -> Observation<Option<Object>>;
    /// Every object of a kind, or Unknown as a whole: never a partial list.
    fn list(&self, kind: &str) -> Observation<Vec<Object>>;
    /// Exclusive: a second create of the same name is `AlreadyExists`. `by`
    /// is `Apply`, or `listener/<account>` for a Conversation or a Listener
    /// the receive verb creates.
    fn create(&self, obj: Object, by: FieldManager) -> Result<Object, StoreError>;
    /// A controller's exclusive create, fenced by the lease: how
    /// ConversationController's plan creates the Task a thread starts, the
    /// one create that may carry the `gateway.fleet.dev/conversation`
    /// labels. The same create as `create` otherwise.
    fn create_fenced(
        &self,
        _obj: Object,
        _by: FieldManager,
        _lease: &LeaseToken,
    ) -> Result<Object, StoreError> {
        Err(StoreError::Unsupported("create_fenced".into()))
    }
    /// Replace spec (and labels); bumps generation when spec changed.
    fn update_spec(
        &self,
        kind: &str,
        name: &str,
        spec: Value,
        labels: Option<std::collections::BTreeMap<String, String>>,
        expected: u64,
        by: FieldManager,
    ) -> Result<Object, StoreError>;
    /// Compare-and-swap on resourceVersion; `by` may change only the status
    /// fields it owns, and a controller must present the current lease.
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
    /// Delete an object its own controller decided to remove: fenced by the
    /// `uid` and `resourceVersion` the decision was made from and by the
    /// lease, so a stale plan cannot remove a recreated object.
    fn delete_fenced(
        &self,
        kind: &str,
        name: &str,
        uid: &str,
        expected: u64,
        by: FieldManager,
        lease: &LeaseToken,
    ) -> Result<DeleteOutcome, StoreError>;
    /// Append one entry to a Task's `status.inbox`, compare-and-swap on
    /// resourceVersion, as a caller `kinds::may_append_inbox` admits. Refused
    /// when the key is already in `inbox` or `deliveries`, when more than
    /// `inbox::MAX_INBOX` entries are undelivered, on a deleting Task, or
    /// when an `answers` id names no declared Question. The entry's
    /// `answers` are recorded in `status.answered` in the same write, first
    /// answer wins. No other status field changes.
    fn append_inbox(
        &self,
        name: &str,
        entry: crate::inbox::InboxEntry,
        expected: u64,
        by: FieldManager,
        lease: Option<&LeaseToken>,
    ) -> Result<Object, StoreError>;
    /// `fleet amend`'s write, compare-and-swap on resourceVersion, as
    /// `FieldManager::Cli` only: append `amendment` to `spec.amendments`,
    /// bump `metadata.generation` to the amendment's generation, and queue
    /// `entry` (whose `amends` names that generation, under
    /// `inbox::amend_key`) as `append_inbox` checks an entry, in one write.
    /// Refused on a deleting or Stalled Task, for an amendment the spec
    /// refuses, and when the inbox refuses the entry. No other field changes.
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
    /// TaskController's status write that also records one delivery: the
    /// receipt goes into `status.deliveries` and its entry leaves
    /// `status.inbox`, in the same compare-and-swap write as `status`, which
    /// is checked for ownership exactly as in `update_status` (so it cannot
    /// change `inbox` itself). Refused when no entry holds the receipt's key.
    fn record_delivery(
        &self,
        name: &str,
        status: Value,
        receipt: crate::inbox::DeliveryReceipt,
        expected: u64,
        by: FieldManager,
        lease: &LeaseToken,
    ) -> Result<Object, StoreError>;
    /// A relaying controller's status write: replace the writer's own Task
    /// status field (`kinds::relay_field`: `status.forge` for
    /// ForgeController, `status.gateway` for GatewayController) with
    /// `status` and append `appends` to `status.inbox`, each checked as
    /// `append_inbox` checks one and its `answers` recorded, in the same
    /// compare-and-swap write under the lease. The cursors and the entries
    /// land together or not at all; no other field changes.
    fn record_relay(
        &self,
        name: &str,
        status: Value,
        appends: Vec<crate::inbox::InboxEntry>,
        expected: u64,
        by: FieldManager,
        lease: &LeaseToken,
    ) -> Result<Object, StoreError>;
    /// Spool one delivery on a Conversation's `status.received`, as
    /// `listener/<account>` for the account its spec names, compare-and-swap
    /// on resourceVersion. A delivery id already spooled or numbered is
    /// `Duplicate` and changes nothing; the spool past
    /// `conversation::RECEIVED_CAP` and a deleting Conversation are refused.
    /// No other status field changes.
    fn receive_event(
        &self,
        _name: &str,
        _delivery: crate::gateway::Delivery,
        _expected: u64,
        _by: FieldManager,
    ) -> Result<(Object, crate::conversation::Spooled), StoreError> {
        Err(StoreError::Unsupported("receive_event".into()))
    }
    /// ConversationController's status write: `status` is checked for
    /// ownership as in `update_status`, except that `received`, which nobody
    /// owns, may have lost the entries the controller numbered and gained
    /// nothing, in the same compare-and-swap write under the lease.
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
    /// Record one heartbeat on a Listener as `listener/<account>` for the
    /// account its spec names: the instance's `beats` entry replaced and
    /// `beatRevision` moved, nothing else.
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
    /// Count one refused or unreadable envelope on a Listener as
    /// `listener/<account>`: `refused` and `beatRevision` move, nothing else.
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
    /// Only the kind's controller, holding the lease, removes a finalizer.
    fn remove_finalizer(
        &self,
        kind: &str,
        name: &str,
        finalizer: &str,
        expected: u64,
        by: FieldManager,
        lease: &LeaseToken,
    ) -> Result<DeleteOutcome, StoreError>;
    /// Validate an object as the store would before writing it.
    fn validate(&self, obj: &Object) -> Result<(), StoreError> {
        crate::validate::validate_object(obj)
    }
    fn acquire_lease(&self, holder: &str, ttl: Millis) -> Result<LeaseToken, StoreError>;
    fn renew_lease(&self, lease: &LeaseToken, ttl: Millis) -> Result<LeaseToken, StoreError>;
    fn release_lease(&self, lease: &LeaseToken) -> Result<(), StoreError>;
}

/// The clock port: time is an input to pure code, read here by an adapter.
pub trait Clock {
    fn now(&self) -> Millis;
}
