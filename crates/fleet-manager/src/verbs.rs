//! The verbs people and workers use: spec writes (`apply`, `delete`), a
//! person's message (`send`) and the worker's answers (`result`, `declare`).
//! The CLI calls these and nothing below them, so every rule lives here
//! once.

use std::collections::{BTreeMap, BTreeSet};

use fleet_api::change_request::{self, ChangeRequestSpec};
use fleet_api::conversation::{
    self, CONVERSATION_LABEL, CONVERSATION_UID_LABEL, ConversationSpec, Spooled,
};
use fleet_api::follow::{DECLARED_CHANGE_REQUEST, ForgeSpec, validate_declaration};
use fleet_api::forge::{ChangeRef, Forge};
use fleet_api::gateway::{
    AuthorKind, ConversationRef, Envelope, Event, Gateway, GatewayConfig, admit, earns_refusal,
    starts_task,
};
use fleet_api::inbox::{CLI_SOURCE_PREFIX, InboxEntry, amend_key, send_key};
use fleet_api::listener::{self, Beat, Heartbeat, listener_name};
use fleet_api::meta::API_VERSION;
use fleet_api::plan::EffectOutcome;
use fleet_api::result::{OutputDecl, TaskResult, validate_outputs};
use fleet_api::task::{self, Amendment, Phase, TaskSpec, TaskStatus};
use fleet_api::validate::typed;
use fleet_api::{Clock, DeleteOutcome, FieldManager, Object, Observation, Store, StoreError};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ApplyOutcome {
    Created,
    Configured,
    Unchanged,
}

/// Bounded retries for a compare-and-swap that keeps losing to another
/// writer; past it the conflict is reported, never forced.
const CAS_TRIES: u32 = 8;

/// Start a bootstrap Task once, retaining its immutable inputs and identity
/// on reconnect. A changed template never rewrites an already running Task.
pub fn ensure_task(store: &dyn Store, obj: Object) -> Result<Object, StoreError> {
    store.validate(&obj)?;
    for _ in 0..CAS_TRIES {
        match store.get(&obj.kind, &obj.metadata.name) {
            Observation::Known(Some(current)) => return Ok(current),
            Observation::Known(None) => match store.create(obj.clone(), FieldManager::Apply) {
                Ok(created) => return Ok(created),
                Err(StoreError::AlreadyExists(_)) => continue,
                Err(e) => return Err(e),
            },
            Observation::Unknown { why } => return Err(StoreError::Unknown(why)),
        }
    }
    Err(StoreError::Busy(
        "bootstrap Task creation kept conflicting".into(),
    ))
}

/// Create the object, or replace its spec and labels when it exists; a Task
/// manifest without `amendments` keeps the stored ones. Status in an
/// applied object is refused: status belongs to controllers. A
/// Task whose `spec.forge.draft.policy` names a policy not in
/// `ready_policies`, the names under `[forge.readyPolicies]`, is refused:
/// a policy the config does not define would never mark anything ready.
/// With `create_only`, an object whose name is taken is refused as
/// `AlreadyExists` and left as it is: the store's create is exclusive, so two
/// clients applying one name at once get one object and one refusal.
pub fn apply(
    store: &dyn Store,
    obj: Object,
    ready_policies: &BTreeSet<String>,
    create_only: bool,
) -> Result<(ApplyOutcome, Object), StoreError> {
    store.validate(&obj)?;
    if obj.kind == task::KIND {
        let spec: TaskSpec = typed(task::KIND, &obj.spec)?;
        if let Some(name) = spec.forge.as_ref().and_then(ForgeSpec::named_policy)
            && !ready_policies.contains(name)
        {
            return Err(StoreError::Invalid(format!(
                "{}: spec.forge.draft.policy {name:?} is not defined under [forge.readyPolicies]",
                obj.key()
            )));
        }
        for label in [CONVERSATION_LABEL, CONVERSATION_UID_LABEL] {
            if obj.metadata.labels.contains_key(label) {
                return Err(StoreError::Invalid(format!(
                    "{}: the label {label} is set only by the Conversation's controller when it creates the Task",
                    obj.key()
                )));
            }
        }
    }
    if obj.kind == listener::KIND {
        return Err(StoreError::Invalid(format!(
            "{}: a Listener is created by the manager, one per [gateway.accounts] entry; the config is the only source",
            obj.key()
        )));
    }
    if obj.kind == conversation::KIND {
        return Err(StoreError::Invalid(format!(
            "{}: a Conversation manifest is applied through the gateway, which derives its name",
            obj.key()
        )));
    }
    if !obj.status.is_null() {
        return Err(StoreError::Invalid(format!(
            "{}: status is written by controllers; remove it from the applied object",
            obj.key()
        )));
    }
    let owners = obj.metadata.owner_references.clone();
    let key = obj.key();
    upsert(store, obj, create_only, |current| {
        if current.metadata.owner_references != owners {
            return Err(StoreError::Invalid(format!(
                "{key}: ownerReferences cannot change"
            )));
        }
        Ok(())
    })
}

/// Create `obj`, or replace its spec and labels when it exists and
/// `existing` accepts the object there; a Task manifest without
/// `amendments` keeps the stored ones. With `create_only` an object whose
/// name is taken is refused as `AlreadyExists` and left as it is: the
/// store's create is exclusive, so two clients applying one name at once
/// get one object and one refusal. Every kind `fleet apply` takes is
/// written here.
fn upsert(
    store: &dyn Store,
    obj: Object,
    create_only: bool,
    existing: impl Fn(&Object) -> Result<(), StoreError>,
) -> Result<(ApplyOutcome, Object), StoreError> {
    let name = obj.metadata.name.clone();
    for _ in 0..CAS_TRIES {
        match store.get(&obj.kind, &name) {
            Observation::Known(None) => match store.create(obj.clone(), FieldManager::Apply) {
                Ok(o) => return Ok((ApplyOutcome::Created, o)),
                // Another writer created it between our read and our create.
                Err(StoreError::AlreadyExists(why)) if create_only => {
                    return Err(StoreError::AlreadyExists(why));
                }
                Err(StoreError::AlreadyExists(_)) => continue,
                Err(e) => return Err(e),
            },
            Observation::Known(Some(_)) if create_only => {
                return Err(StoreError::AlreadyExists(format!("{}/{name}", obj.kind)));
            }
            Observation::Known(Some(current)) => {
                existing(&current)?;
                let rv = current.metadata.resource_version;
                let labels = Some(obj.metadata.labels.clone());
                let mut spec = obj.spec.clone();
                // A manifest carries no amendments: re-applying it keeps the
                // stored ones, which only the store's amend mutation writes.
                if obj.kind == task::KIND
                    && let (Some(fields), Some(kept)) =
                        (spec.as_object_mut(), current.spec.get("amendments"))
                {
                    fields.entry("amendments").or_insert_with(|| kept.clone());
                }
                match store.update_spec(&obj.kind, &name, spec, labels, rv, FieldManager::Apply) {
                    Ok(o) if o.metadata.resource_version == rv => {
                        return Ok((ApplyOutcome::Unchanged, o));
                    }
                    Ok(o) => return Ok((ApplyOutcome::Configured, o)),
                    Err(StoreError::Conflict { .. }) => continue,
                    Err(e) => return Err(e),
                }
            }
            Observation::Unknown { why } => return Err(StoreError::Unknown(why)),
        }
    }
    Err(StoreError::Busy(format!(
        "{}/{name}: kept conflicting with other writers",
        obj.kind
    )))
}

/// The labels only Fleet writes: the Conversation's controller when it
/// creates a Task, and a plugin's host. `fleet label` refuses them.
const PROTECTED_LABELS: [&str; 5] = [
    CONVERSATION_LABEL,
    CONVERSATION_UID_LABEL,
    fleet_api::talk::PARENT_LABEL,
    fleet_api::talk::PARENT_UID_LABEL,
    fleet_api::plugin::CREATOR,
];

/// Set `labels` on the Task `name` and change nothing else of it: its spec,
/// generation and other labels stay. A label only Fleet writes is refused,
/// and the ones the Task carries are kept, which `apply` cannot do for a
/// Task a Conversation made.
pub fn label(
    store: &dyn Store,
    name: &str,
    labels: &BTreeMap<String, String>,
) -> Result<Object, StoreError> {
    if let Some(key) = labels
        .keys()
        .find(|key| PROTECTED_LABELS.contains(&key.as_str()))
    {
        return Err(StoreError::Invalid(format!(
            "the label {key} is written by Fleet alone"
        )));
    }
    for _ in 0..CAS_TRIES {
        let current = match store.get(task::KIND, name) {
            Observation::Known(Some(o)) => o,
            Observation::Known(None) => {
                return Err(StoreError::NotFound(format!("{}/{name}", task::KIND)));
            }
            Observation::Unknown { why } => return Err(StoreError::Unknown(why)),
        };
        let mut merged = current.metadata.labels.clone();
        merged.extend(labels.iter().map(|(k, v)| (k.clone(), v.clone())));
        match store.update_spec(
            task::KIND,
            name,
            current.spec.clone(),
            Some(merged),
            current.metadata.resource_version,
            FieldManager::Apply,
        ) {
            Err(StoreError::Conflict { .. }) => continue,
            done => return done,
        }
    }
    Err(StoreError::Busy(format!(
        "{}/{name}: kept conflicting with other writers",
        task::KIND
    )))
}

pub fn delete(store: &dyn Store, kind: &str, name: &str) -> Result<DeleteOutcome, StoreError> {
    store.delete(kind, name, FieldManager::Apply)
}

/// `cr-` and the first twelve hex digits of the SHA-256 of the canonical
/// ref: one change names one object, two projects that flatten alike cannot
/// collide, and long project paths fit.
pub fn derived_name(reference: &ChangeRef) -> String {
    let digest = Sha256::digest(reference.canonical().as_bytes());
    let hex: String = digest.iter().take(6).map(|b| format!("{b:02x}")).collect();
    format!("cr-{hex}")
}

/// `conv-` and the first twelve hex digits of the SHA-256 of
/// `<account>/<address>`, the address as the adapter normalised it: one
/// thread names one Conversation per account, and the account, not the
/// address, carries the platform workspace.
pub fn conversation_name(account: &str, conversation: &ConversationRef) -> String {
    let digest = Sha256::digest(conversation.canonical(account).as_bytes());
    let hex: String = digest.iter().take(6).map(|b| format!("{b:02x}")).collect();
    format!("conv-{hex}")
}

/// Start mirroring a change. The URL is parsed by the forge adapter for its
/// host (a host not configured is refused), the name is derived from the
/// canonical ref when the manifest gives none, and an existing object under
/// the name is reused only if it mirrors the same change: its recorded
/// `status.ref` when the controller wrote one, else its own URL parsed.
pub fn apply_change_request(
    store: &dyn Store,
    forge: &dyn Forge,
    mut obj: Object,
    create_only: bool,
) -> Result<(ApplyOutcome, Object), StoreError> {
    if obj.kind != change_request::KIND {
        return Err(StoreError::Invalid(format!(
            "{}: not a ChangeRequest",
            obj.key()
        )));
    }
    let spec: ChangeRequestSpec = typed(change_request::KIND, &obj.spec)?;
    let reference = parse_url(forge, &spec.url, &obj.metadata.name)?;
    if obj.metadata.name.is_empty() {
        obj.metadata.name = derived_name(&reference);
    }
    store.validate(&obj)?;
    if !obj.status.is_null() {
        return Err(StoreError::Invalid(format!(
            "{}: status is written by controllers; remove it from the applied object",
            obj.key()
        )));
    }
    upsert(store, obj, create_only, |current| {
        verify_mirrors(forge, current, &reference)
    })
}

/// An existing ChangeRequest is reused only when it mirrors `reference`:
/// its recorded `status.ref` once the controller wrote one, else its own
/// URL parsed again.
fn verify_mirrors(
    forge: &dyn Forge,
    current: &Object,
    reference: &ChangeRef,
) -> Result<(), StoreError> {
    let recorded: Option<ChangeRef> = match current.status.get("ref") {
        Some(v) if !v.is_null() => Some(
            serde_json::from_value(v.clone())
                .map_err(|e| StoreError::Unknown(format!("{}: status.ref: {e}", current.key())))?,
        ),
        _ => None,
    };
    let mirrors = match recorded {
        Some(r) => r,
        None => {
            let existing: ChangeRequestSpec = typed(change_request::KIND, &current.spec)?;
            parse_url(forge, &existing.url, &current.metadata.name)?
        }
    };
    if mirrors != *reference {
        return Err(StoreError::Forbidden(format!(
            "{} mirrors {}, not {}: a change names one object, so give this one another name",
            current.key(),
            mirrors.canonical(),
            reference.canonical()
        )));
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DeclareOutcome {
    Declared,
    /// The same fact was already declared: a retry of `fleet declare`.
    Unchanged,
}

/// What `fleet declare --change-request` did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Declared {
    pub outcome: DeclareOutcome,
    /// The declaration as appended (or found).
    pub item: Value,
    /// The ChangeRequest's name, and whether this verb created it.
    pub change_request: String,
    pub created: bool,
}

/// `fleet declare TASK --change-request URL`: parse the URL through the
/// Forge port (a host not configured is refused), ensure one ChangeRequest
/// exists for it under its derived name (an existing one is reused after
/// its `status.ref` is verified), then append `{kind, url, name}` to the
/// Task's `status.declared` as field manager `worker`. A repeated URL is a
/// no-op. The two steps are not atomic: a crash between them leaves a
/// ChangeRequest nobody follows, which the next `declare` of that URL finds.
pub fn declare_change_request(
    store: &dyn Store,
    forge: &dyn Forge,
    task_name: &str,
    url: &str,
) -> Result<Declared, StoreError> {
    // Eligibility first, so an ineligible Task creates no orphan.
    eligible_for_declare(store, task_name)?;
    let url = url.trim();
    let (name, created) = ensure_change_request(store, forge, url)?;
    let item = json!({"kind": DECLARED_CHANGE_REQUEST, "url": url, "name": name});
    let outcome = append_declaration(store, task_name, item.clone(), |existing| {
        existing.get("kind").and_then(Value::as_str) == Some(DECLARED_CHANGE_REQUEST)
            && existing.get("name").and_then(Value::as_str) == Some(name.as_str())
    })?;
    Ok(Declared {
        outcome,
        item,
        change_request: name,
        created,
    })
}

/// Ensure a verified mirror without applying over its operator-owned spec or labels.
pub(crate) fn ensure_change_request(
    store: &dyn Store,
    forge: &dyn Forge,
    url: &str,
) -> Result<(String, bool), StoreError> {
    let url = url.trim();
    let reference = match forge.parse(url) {
        Observation::Known(r) => r,
        Observation::Unknown { why } => {
            return Err(StoreError::Invalid(format!("change URL {url:?}: {why}")));
        }
    };
    let name = derived_name(&reference);
    for _ in 0..CAS_TRIES {
        match store.get(change_request::KIND, &name) {
            Observation::Known(None) => {
                let manifest = Object {
                    api_version: API_VERSION.to_string(),
                    kind: change_request::KIND.to_string(),
                    metadata: fleet_api::ObjectMeta {
                        name: name.clone(),
                        ..fleet_api::ObjectMeta::default()
                    },
                    spec: json!({"url": url}),
                    status: Value::Null,
                };
                store.validate(&manifest)?;
                match store.create(manifest, FieldManager::Apply) {
                    Ok(_) => return Ok((name, true)),
                    Err(StoreError::AlreadyExists(_)) => continue,
                    Err(e) => return Err(e),
                }
            }
            Observation::Known(Some(current)) => {
                verify_mirrors(forge, &current, &reference)?;
                return Ok((name, false));
            }
            Observation::Unknown { why } => return Err(StoreError::Unknown(why)),
        }
    }
    Err(StoreError::Busy(
        "ChangeRequest creation kept conflicting".into(),
    ))
}

/// `fleet declare TASK --json OBJECT`: append any JSON object with a string
/// `kind` to `status.declared`; Fleet validates nothing else. The same
/// object again is a no-op.
pub fn declare(
    store: &dyn Store,
    task_name: &str,
    item: Value,
) -> Result<DeclareOutcome, StoreError> {
    validate_declaration(&item).map_err(StoreError::Invalid)?;
    append_declaration(store, task_name, item.clone(), |existing| *existing == item)
}

/// Both forms are accepted after the brief has a receipt and before
/// deletion, and conclude nothing.
fn eligible_for_declare(store: &dyn Store, name: &str) -> Result<(Object, TaskStatus), StoreError> {
    let obj = match store.get(task::KIND, name) {
        Observation::Known(Some(o)) => o,
        Observation::Known(None) => {
            return Err(StoreError::NotFound(format!("{}/{name}", task::KIND)));
        }
        Observation::Unknown { why } => return Err(StoreError::Unknown(why)),
    };
    if obj.is_deleting() {
        return Err(StoreError::Forbidden(format!(
            "{} is being deleted",
            obj.key()
        )));
    }
    let st: TaskStatus = if obj.status.is_null() {
        TaskStatus::default()
    } else {
        typed(task::KIND, &obj.status)?
    };
    if st.brief.is_none() {
        return Err(StoreError::Forbidden(format!(
            "{}: no brief has a receipt yet; a declaration comes from a briefed worker",
            obj.key()
        )));
    }
    Ok((obj, st))
}

fn append_declaration(
    store: &dyn Store,
    name: &str,
    item: Value,
    same: impl Fn(&Value) -> bool,
) -> Result<DeclareOutcome, StoreError> {
    for _ in 0..CAS_TRIES {
        let (obj, mut st) = eligible_for_declare(store, name)?;
        if st.declared.iter().any(&same) {
            return Ok(DeclareOutcome::Unchanged);
        }
        st.declared.push(item.clone());
        let status = serde_json::to_value(&st).map_err(|e| StoreError::Invalid(e.to_string()))?;
        match store.update_status(
            task::KIND,
            name,
            status,
            obj.metadata.resource_version,
            FieldManager::Worker,
            None,
        ) {
            Ok(_) => return Ok(DeclareOutcome::Declared),
            Err(StoreError::Conflict { .. }) => continue,
            Err(e) => return Err(e),
        }
    }
    Err(StoreError::Busy(format!(
        "{}/{name}: kept conflicting with other writers",
        task::KIND
    )))
}

fn parse_url(forge: &dyn Forge, url: &str, name: &str) -> Result<ChangeRef, StoreError> {
    match forge.parse(url) {
        Observation::Known(r) => Ok(r),
        Observation::Unknown { why } => Err(StoreError::Invalid(format!(
            "ChangeRequest/{name}: spec.url {url:?}: {why}"
        ))),
    }
}

/// `fleet send`: queue a message for the Task's session as `cli/<user>`,
/// under the key `task/<name>/<uid>/send/<n>`, through the store's typed
/// append. `n` is the resourceVersion the append is made against, which the
/// store hands out once for its whole life, so a key never repeats even
/// after its receipt has rotated out of `status.deliveries`. `answers`
/// names the declared Questions the message answers (`--answers ID`): the
/// store records each in `status.answered` in the same write, first answer
/// wins, and refuses an id that names no declared Question. The entry is
/// returned as appended; delivery is the controller's, on its next pass.
pub fn send(
    store: &dyn Store,
    clock: &dyn Clock,
    name: &str,
    user: &str,
    text: &str,
    answers: &[String],
) -> Result<InboxEntry, StoreError> {
    let user = user.trim();
    if user.is_empty() || user.contains(['/', '\0']) {
        return Err(StoreError::Invalid(format!(
            "sender {user:?}: a user name without '/'"
        )));
    }
    if text.trim().is_empty() {
        return Err(StoreError::Invalid(
            "nothing to send: the text is empty".into(),
        ));
    }
    for _ in 0..CAS_TRIES {
        let obj = match store.get(task::KIND, name) {
            Observation::Known(Some(o)) => o,
            Observation::Known(None) => {
                return Err(StoreError::NotFound(format!("{}/{name}", task::KIND)));
            }
            Observation::Unknown { why } => return Err(StoreError::Unknown(why)),
        };
        if obj.is_deleting() {
            return Err(StoreError::Forbidden(format!(
                "{} is being deleted",
                obj.key()
            )));
        }
        let m = &obj.metadata;
        let entry = InboxEntry {
            key: send_key(&m.name, &m.uid, m.resource_version),
            from: format!("{CLI_SOURCE_PREFIX}{user}"),
            text: text.to_string(),
            about: None,
            answers: answers.iter().map(|a| a.trim().to_string()).collect(),
            amends: None,
            queued_at: clock.now(),
        };
        match store.append_inbox(
            name,
            entry.clone(),
            m.resource_version,
            FieldManager::Cli,
            None,
        ) {
            Ok(_) => return Ok(entry),
            Err(StoreError::Conflict { .. }) => continue,
            Err(e) => return Err(e),
        }
    }
    Err(StoreError::Busy(format!(
        "{}/{name}: kept conflicting with other writers",
        task::KIND
    )))
}

/// What `fleet amend` asks for.
#[derive(Debug, Clone, Default)]
pub struct AmendRequest {
    pub text: String,
    pub reason: Option<String>,
    pub outputs: std::collections::BTreeMap<String, OutputDecl>,
    /// Refuse unless the Task is at this generation.
    pub if_generation: Option<u64>,
}

/// What `fleet amend` did.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Amended {
    pub task: String,
    /// 1 for the Task's first amendment.
    pub amendment: usize,
    pub previous_generation: u64,
    pub generation: u64,
    /// The inbox entry that delivers it.
    pub key: String,
    pub from: String,
}

/// Add to a Task's brief: append an amendment to `spec.amendments`, which
/// bumps the generation, and queue its delivery to the same session, in one
/// write. The caller never needs a generation; `if_generation` is an
/// optional precondition for one that must not land after another write.
pub fn amend(
    store: &dyn Store,
    clock: &dyn Clock,
    name: &str,
    user: &str,
    request: &AmendRequest,
) -> Result<Amended, StoreError> {
    let outcome = amend_once(store, clock, name, user, request);
    crate::telemetry::amendment(match &outcome {
        Ok(_) => "appended",
        Err(StoreError::Precondition(_)) => "precondition",
        Err(StoreError::Conflict { .. } | StoreError::Busy(_)) => "conflict",
        Err(StoreError::NotFound(_)) => "notFound",
        Err(StoreError::Forbidden(_)) => "forbidden",
        Err(StoreError::Invalid(_)) => "invalid",
        Err(_) => "error",
    });
    outcome
}

fn amend_once(
    store: &dyn Store,
    clock: &dyn Clock,
    name: &str,
    user: &str,
    request: &AmendRequest,
) -> Result<Amended, StoreError> {
    let user = user.trim();
    if user.is_empty() || user.contains(['/', '\0']) {
        return Err(StoreError::Invalid(format!(
            "sender {user:?}: a user name without '/'"
        )));
    }
    if request.text.trim().is_empty() {
        return Err(StoreError::Invalid(
            "nothing to amend: the text is empty".into(),
        ));
    }
    let reason = request
        .reason
        .as_deref()
        .map(str::trim)
        .filter(|r| !r.is_empty())
        .map(str::to_string);
    for _ in 0..CAS_TRIES {
        let obj = match store.get(task::KIND, name) {
            Observation::Known(Some(o)) => o,
            Observation::Known(None) => {
                return Err(StoreError::NotFound(format!("{}/{name}", task::KIND)));
            }
            Observation::Unknown { why } => return Err(StoreError::Unknown(why)),
        };
        if obj.is_deleting() {
            return Err(StoreError::Forbidden(format!(
                "{} is being deleted; nothing would deliver the amendment",
                obj.key()
            )));
        }
        let m = &obj.metadata;
        if let Some(want) = request.if_generation
            && want != m.generation
        {
            return Err(StoreError::Precondition(format!(
                "{} is at generation {}, not {want}",
                obj.key(),
                m.generation
            )));
        }
        let spec: TaskSpec = typed(task::KIND, &obj.spec)?;
        let st: TaskStatus = if obj.status.is_null() {
            TaskStatus::default()
        } else {
            typed(task::KIND, &obj.status)?
        };
        if st.phase == Phase::Stalled {
            return Err(StoreError::Forbidden(format!(
                "{} is Stalled; nothing would deliver the amendment, so create a new Task",
                obj.key()
            )));
        }
        let declared = spec.outputs_at(u64::MAX);
        if let Some(name) = request.outputs.keys().find(|n| declared.contains_key(*n)) {
            return Err(StoreError::Invalid(format!(
                "{}: output {name:?} is already declared; an amendment only adds outputs",
                obj.key()
            )));
        }
        let generation = m.generation + 1;
        let from = format!("{CLI_SOURCE_PREFIX}{user}");
        let at = clock.now();
        let amendment = Amendment {
            text: request.text.clone(),
            reason: reason.clone(),
            outputs: request.outputs.clone(),
            from: from.clone(),
            at,
            generation,
        };
        let key = amend_key(&m.name, &m.uid, generation);
        let entry = InboxEntry {
            key: key.clone(),
            from: from.clone(),
            text: request.text.clone(),
            about: None,
            answers: Vec::new(),
            amends: Some(generation),
            queued_at: at,
        };
        match store.amend_task(
            name,
            amendment,
            entry,
            m.resource_version,
            FieldManager::Cli,
        ) {
            Ok(_) => {
                return Ok(Amended {
                    task: name.to_string(),
                    amendment: spec.amendments.len() + 1,
                    previous_generation: m.generation,
                    generation,
                    key,
                    from,
                });
            }
            Err(StoreError::Conflict { .. }) => continue,
            Err(e) => return Err(e),
        }
    }
    Err(StoreError::Busy(format!(
        "{}/{name}: kept conflicting with other writers",
        task::KIND
    )))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ResultOutcome {
    Recorded,
    /// The same result was already recorded: a retry of `fleet result`.
    Unchanged,
}

/// The worker's answer: written as field manager `worker` into
/// `status.result`, only once the brief it answers has a receipt, and never
/// while an amendment waits to be delivered. It names `status.brief`'s
/// generation and carries the outputs declared up to it.
pub fn record_result(
    store: &dyn Store,
    name: &str,
    result: TaskResult,
) -> Result<ResultOutcome, StoreError> {
    for _ in 0..CAS_TRIES {
        let obj = match store.get(task::KIND, name) {
            Observation::Known(Some(o)) => o,
            Observation::Known(None) => {
                return Err(StoreError::NotFound(format!("{}/{name}", task::KIND)));
            }
            Observation::Unknown { why } => return Err(StoreError::Unknown(why)),
        };
        if obj.is_deleting() {
            return Err(StoreError::Forbidden(format!(
                "{} is being deleted",
                obj.key()
            )));
        }
        let spec: TaskSpec = typed(task::KIND, &obj.spec)?;
        let mut st: TaskStatus = if obj.status.is_null() {
            TaskStatus::default()
        } else {
            typed(task::KIND, &obj.status)?
        };
        let Some(brief) = &st.brief else {
            return Err(StoreError::Forbidden(format!(
                "{}: no brief has a receipt yet; a result answers a delivered brief",
                obj.key()
            )));
        };
        // An amendment still to deliver is owed an answer first; one given
        // up on never reached the worker, so it holds nothing.
        if let Some(pending) = st.inbox.iter().find(|e| {
            e.amends.is_some_and(|g| g > brief.generation)
                && !st.delivery_attempts.get(&e.key).is_some_and(|a| a.failed)
        }) {
            return Err(StoreError::Forbidden(format!(
                "{}: the amendment for generation {} is not delivered yet; record a result once it arrives, naming its generation",
                obj.key(),
                pending.amends.unwrap_or_default()
            )));
        }
        if result.generation != brief.generation {
            return Err(StoreError::Invalid(format!(
                "{}: stale generation {}: the brief was delivered for generation {}",
                obj.key(),
                result.generation,
                brief.generation
            )));
        }
        let problems = validate_outputs(&spec.outputs_at(brief.generation), &result.outputs);
        if !problems.is_empty() {
            return Err(StoreError::Invalid(format!(
                "{}: result does not match spec.outputs: {}",
                obj.key(),
                problems.join("; ")
            )));
        }
        // Write-once within a generation; a result for an amended brief
        // replaces the one for an older generation.
        match &st.result {
            Some(existing) if *existing == result => return Ok(ResultOutcome::Unchanged),
            Some(existing) if existing.generation == result.generation => {
                return Err(StoreError::Forbidden(format!(
                    "{}: a different result is already recorded for generation {}",
                    obj.key(),
                    result.generation
                )));
            }
            Some(_) | None => {}
        }
        st.result = Some(result.clone());
        let status = serde_json::to_value(&st).map_err(|e| StoreError::Invalid(e.to_string()))?;
        match store.update_status(
            task::KIND,
            name,
            status,
            obj.metadata.resource_version,
            FieldManager::Worker,
            None,
        ) {
            Ok(_) => return Ok(ResultOutcome::Recorded),
            Err(StoreError::Conflict { .. }) => continue,
            Err(e) => return Err(e),
        }
    }
    Err(StoreError::Busy(format!(
        "{}/{name}: kept conflicting with other writers",
        task::KIND
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_conversation_is_named_from_its_account_and_normalised_address() {
        let thread = ConversationRef::split("slack:C0123ABCD:1730000000.000100").unwrap();
        let name = conversation_name("slack-main", &thread);
        assert!(name.starts_with("conv-") && name.len() == 5 + 12, "{name}");
        assert!(name[5..].chars().all(|c| c.is_ascii_hexdigit()));
        assert_eq!(
            name,
            conversation_name("slack-main", &thread),
            "deterministic"
        );
        assert_ne!(
            name,
            conversation_name("slack-other", &thread),
            "per account"
        );
        assert_ne!(
            name,
            conversation_name("slack-main", &thread.channel()),
            "per thread"
        );
        // The canonical string is `<account>/<address>`, and the hash is of exactly that.
        use sha2::{Digest, Sha256};
        let hex: String = Sha256::digest(b"slack-main/slack:C0123ABCD:1730000000.000100")
            .iter()
            .take(6)
            .map(|b| format!("{b:02x}"))
            .collect();
        assert_eq!(name, format!("conv-{hex}"));
        assert_ne!(
            name,
            derived_name(&ChangeRef {
                kind: "github".into(),
                host: "h".into(),
                project: "p".into(),
                id: "1".into()
            })
        );
    }
}

/// Start mirroring a thread by manifest. The adapter of the account parses
/// the address (an account or route not configured is refused), the name
/// derives from the account and the normalised address when the manifest
/// gives none, and a manifest giving one thread's derived name with
/// another's address is refused: one thread names one object.
pub fn apply_conversation(
    store: &dyn Store,
    gateway: &dyn Gateway,
    config: &GatewayConfig,
    mut obj: Object,
    create_only: bool,
) -> Result<(ApplyOutcome, Object), StoreError> {
    if obj.kind != conversation::KIND {
        return Err(StoreError::Invalid(format!(
            "{}: not a Conversation",
            obj.key()
        )));
    }
    let spec: ConversationSpec = typed(conversation::KIND, &obj.spec)?;
    if !config.accounts.contains_key(&spec.account) {
        return Err(StoreError::Invalid(format!(
            "{}: spec.account {:?} is not under [gateway.accounts]",
            obj.key(),
            spec.account
        )));
    }
    match config.routes.get(&spec.route) {
        Some(route) if route.account == spec.account => {}
        Some(_) => {
            return Err(StoreError::Invalid(format!(
                "{}: route {:?} is not a route of account {:?}",
                obj.key(),
                spec.route,
                spec.account
            )));
        }
        None => {
            return Err(StoreError::Invalid(format!(
                "{}: route {:?} is not under [gateway.routes]",
                obj.key(),
                spec.route
            )));
        }
    }
    let reference = match gateway.parse(&spec.account, &spec.address) {
        Observation::Known(r) => r,
        Observation::Unknown { why } => {
            return Err(StoreError::Invalid(format!(
                "{}: spec.address {:?}: {why}",
                obj.key(),
                spec.address
            )));
        }
    };
    let derived = conversation_name(&spec.account, &reference);
    if obj.metadata.name.is_empty() {
        obj.metadata.name = derived.clone();
    } else if obj.metadata.name != derived {
        return Err(StoreError::Forbidden(format!(
            "{}: a thread names one object, {derived}, derived from its account and address; give this one no name or that one",
            obj.key()
        )));
    }
    // The stored address is the adapter's normalised one.
    obj.spec["address"] = json!(reference.address());
    store.validate(&obj)?;
    if !obj.status.is_null() {
        return Err(StoreError::Invalid(format!(
            "{}: status is written by controllers; remove it from the applied object",
            obj.key()
        )));
    }
    upsert(store, obj, create_only, |_| Ok(()))
}

/// What `fleet gateway receive` did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "lowercase")]
pub enum Received {
    /// The delivery is spooled on the thread's Conversation.
    Written {
        conversation: String,
        delivery: String,
    },
    /// The delivery id was spooled or numbered before: nothing changed.
    Duplicate {
        conversation: String,
        delivery: String,
    },
    /// Nothing the Gateway mirrors.
    Ignored { why: String },
    /// A heartbeat was recorded on the account's Listener.
    Beat { listener: String, instance: String },
}

/// Why the verb refused or could not read an envelope, as counted on the
/// Listener. `fault` marks the listener's own: not an envelope, or the
/// wrong `verified`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    NotEnvelope,
    WrongVerified,
    UnreadablePayload,
    SpoolFull,
    Deleting,
    UnknownRecord,
}

impl Refusal {
    pub fn reason(self) -> &'static str {
        match self {
            Refusal::NotEnvelope => "NotEnvelope",
            Refusal::WrongVerified => "WrongVerified",
            Refusal::UnreadablePayload => "UnreadablePayload",
            Refusal::SpoolFull => "SpoolFull",
            Refusal::Deleting => "Deleting",
            Refusal::UnknownRecord => "UnknownRecord",
        }
    }

    pub fn fault(self) -> bool {
        matches!(self, Refusal::NotEnvelope | Refusal::WrongVerified)
    }
}

fn not_configured(account: &str) -> StoreError {
    StoreError::Invalid(format!(
        "account {account:?} is not configured under [gateway.accounts]"
    ))
}

/// `fleet gateway receive --account NAME`: read one envelope and add its
/// event to the thread's Conversation, as `listener/<account>`, without the
/// lease. An event in a channel no route covers is ignored; one in a thread
/// Fleet does not mirror yet creates the Conversation only when it could
/// start a Task, earn a refusal, or is Fleet's own keyed post; a
/// redelivered event changes nothing. Every envelope refused or unreadable
/// is counted on the account's Listener, which the verb creates if the
/// manager has not. The error maps to the verb's exit code: refused is 1,
/// Unknown is 4, a conflict the retries did not resolve is 5.
pub fn receive(
    store: &dyn Store,
    clock: &dyn Clock,
    gateway: &dyn Gateway,
    config: &GatewayConfig,
    account: &str,
    text: &str,
) -> Result<Received, StoreError> {
    let Some(account_config) = config.accounts.get(account) else {
        return Err(not_configured(account));
    };
    let refuse = |why: Refusal, err: StoreError| -> StoreError {
        record_refusal(store, clock, account, why);
        err
    };
    let envelope: Envelope = match serde_json::from_str(text) {
        Ok(e) => e,
        Err(e) => {
            return Err(refuse(
                Refusal::NotEnvelope,
                StoreError::Unknown(format!("the envelope could not be read: {e}")),
            ));
        }
    };
    let expected = account_config.inbound.verified();
    if envelope.verified != expected {
        return Err(refuse(
            Refusal::WrongVerified,
            StoreError::Forbidden(format!(
                "verified {:?} is not how account {account}'s listener delivers; it delivers by {:?}",
                envelope.verified.name(),
                expected.name()
            )),
        ));
    }
    let delivery = match gateway.event(account_config, &envelope) {
        Observation::Known(Some(d)) => d,
        Observation::Known(None) => {
            return Ok(Received::Ignored {
                why: "the payload carries nothing the Gateway reads".into(),
            });
        }
        Observation::Unknown { why } => {
            return Err(refuse(
                Refusal::UnreadablePayload,
                StoreError::Unknown(format!("the payload could not be read: {why}")),
            ));
        }
    };
    let reference = delivery.event.conversation().clone();
    let name = conversation_name(account, &reference);
    let direct = delivery.direct
        || match mirrored_as_dm(store, config, account, &name, &delivery.event) {
            Ok(direct) => direct,
            Err(why) => return Err(refuse(Refusal::UnknownRecord, StoreError::Unknown(why))),
        };
    let Some(covered) = config.route_for(account, &reference.chat, direct) else {
        return Ok(Received::Ignored {
            why: format!("no route covers channel {}", reference.chat),
        });
    };
    let (route_name, route) = (covered.name, covered.route);
    let by = FieldManager::Listener(account.to_string());
    for _ in 0..CAS_TRIES {
        let current = match store.get(conversation::KIND, &name) {
            Observation::Known(Some(o)) => o,
            Observation::Known(None) => {
                if !worth_mirroring(route, covered.direct, config, &delivery.event) {
                    return Ok(Received::Ignored {
                        why: "chatter in a thread Fleet does not mirror".into(),
                    });
                }
                let mut spec = json!({
                    "account": account,
                    "address": reference.address(),
                    "route": route_name,
                });
                if covered.direct {
                    spec["direct"] = json!(true);
                }
                let manifest = Object {
                    api_version: API_VERSION.to_string(),
                    kind: conversation::KIND.to_string(),
                    metadata: fleet_api::ObjectMeta {
                        name: name.clone(),
                        ..fleet_api::ObjectMeta::default()
                    },
                    spec,
                    status: Value::Null,
                };
                store.validate(&manifest)?;
                match store.create(manifest, by.clone()) {
                    Ok(o) => o,
                    Err(StoreError::AlreadyExists(_)) => continue,
                    Err(e) => return Err(e),
                }
            }
            Observation::Unknown { why } => {
                return Err(refuse(Refusal::UnknownRecord, StoreError::Unknown(why)));
            }
        };
        if current.is_deleting() {
            return Err(refuse(
                Refusal::Deleting,
                StoreError::Forbidden(format!("{} is being deleted", current.key())),
            ));
        }
        match store.receive_event(
            &name,
            delivery.clone(),
            current.metadata.resource_version,
            by.clone(),
        ) {
            Ok((_, Spooled::Written)) => {
                return Ok(Received::Written {
                    conversation: name,
                    delivery: delivery.id,
                });
            }
            Ok((_, Spooled::Duplicate)) => {
                return Ok(Received::Duplicate {
                    conversation: name,
                    delivery: delivery.id,
                });
            }
            Err(StoreError::Conflict { .. }) => continue,
            Err(e @ StoreError::Forbidden(_)) => {
                let why = if e.to_string().contains("unread events") {
                    Refusal::SpoolFull
                } else {
                    Refusal::Deleting
                };
                return Err(refuse(why, e));
            }
            Err(e @ StoreError::Unknown(_)) => return Err(refuse(Refusal::UnknownRecord, e)),
            Err(e) => return Err(e),
        }
    }
    Err(StoreError::Busy(format!(
        "{}/{name}: kept conflicting with other writers",
        conversation::KIND
    )))
}

/// The Gateway controller's `Receive` action: add Fleet's opening post in a
/// thread it opened to the thread's Conversation, as the receive verb would,
/// creating the Conversation under the name its account and address derive.
/// Written as `by`, the Gateway controller, under the lease; a delivery already
/// spooled or numbered is a duplicate. The store's doubt is Unknown, never
/// a refusal.
pub fn receive_opened(
    store: &dyn Store,
    account: &str,
    route: &str,
    delivery: fleet_api::gateway::Delivery,
    by: FieldManager,
    lease: &fleet_api::LeaseToken,
) -> Observation<EffectOutcome> {
    let reference = delivery.event.conversation().clone();
    let name = conversation_name(account, &reference);
    for _ in 0..CAS_TRIES {
        let current = match store.get(conversation::KIND, &name) {
            Observation::Known(Some(o)) => o,
            Observation::Known(None) => {
                let manifest = Object {
                    api_version: API_VERSION.to_string(),
                    kind: conversation::KIND.to_string(),
                    metadata: fleet_api::ObjectMeta {
                        name: name.clone(),
                        ..fleet_api::ObjectMeta::default()
                    },
                    spec: json!({
                        "account": account,
                        "address": reference.address(),
                        "route": route,
                    }),
                    status: Value::Null,
                };
                match store.create_fenced(manifest, by.clone(), lease) {
                    Ok(o) => o,
                    Err(StoreError::AlreadyExists(_)) => continue,
                    Err(StoreError::Unknown(why)) => return Observation::unknown(why),
                    Err(e) => {
                        return Observation::Known(EffectOutcome::Failed { why: e.to_string() });
                    }
                }
            }
            Observation::Unknown { why } => return Observation::unknown(why),
        };
        let uid = current.metadata.uid.clone();
        match store.receive_event(
            &name,
            delivery.clone(),
            current.metadata.resource_version,
            by.clone(),
        ) {
            Ok(_) => {
                return Observation::Known(EffectOutcome::Received {
                    conversation: name,
                    uid,
                });
            }
            Err(StoreError::Conflict { .. }) => continue,
            Err(StoreError::Unknown(why)) => return Observation::unknown(why),
            Err(e) => return Observation::Known(EffectOutcome::Failed { why: e.to_string() }),
        }
    }
    Observation::unknown(format!(
        "{}/{name}: kept conflicting with other writers",
        conversation::KIND
    ))
}

/// Whether an edit or a removal that does not say it is in a DM is in a
/// thread Fleet mirrors as one. A platform may leave the chat's type out of
/// such an event, and the thread's route must still cover it. Only an
/// account with a DM route reads the store for this; a missing
/// Conversation is no DM, and an unknown one is the store's doubt.
fn mirrored_as_dm(
    store: &dyn Store,
    config: &GatewayConfig,
    account: &str,
    name: &str,
    event: &Event,
) -> Result<bool, String> {
    if matches!(event, Event::Message { .. }) || !config.has_dm_route(account) {
        return Ok(false);
    }
    match store.get(conversation::KIND, name) {
        Observation::Known(Some(o)) => Ok(o.spec["direct"] == json!(true)),
        Observation::Known(None) => Ok(false),
        Observation::Unknown { why } => Err(why),
    }
}

/// Whether an event in a thread Fleet does not mirror yet is worth a
/// Conversation: it could start a Task, earn a refusal, or is Fleet's own
/// keyed post. Other chatter costs no object.
fn worth_mirroring(
    route: &fleet_api::gateway::RouteConfig,
    direct: bool,
    config: &GatewayConfig,
    event: &Event,
) -> bool {
    let Event::Message { message, .. } = event else {
        return false;
    };
    let admission = admit(route, &config.people, message);
    starts_task(route, direct, admission, message)
        || earns_refusal(direct, admission, message)
        || (message.author.kind == AuthorKind::Fleet && message.key.is_some())
}

/// `fleet gateway receive --account NAME --beat`: record one heartbeat on
/// the account's Listener, stamped by this clock, creating the Listener if
/// the manager has not yet.
pub fn beat(
    store: &dyn Store,
    clock: &dyn Clock,
    config: &GatewayConfig,
    account: &str,
    text: &str,
) -> Result<Received, StoreError> {
    if !config.accounts.contains_key(account) {
        return Err(not_configured(account));
    }
    let heartbeat: Heartbeat = match serde_json::from_str(text) {
        Ok(b) => b,
        Err(e) => {
            record_refusal(store, clock, account, Refusal::NotEnvelope);
            return Err(StoreError::Unknown(format!(
                "the heartbeat could not be read: {e}"
            )));
        }
    };
    let name = listener_name(account);
    let by = FieldManager::Listener(account.to_string());
    for _ in 0..CAS_TRIES {
        let current = ensure_listener(store, account)?;
        let beat = Beat {
            state: heartbeat.state,
            digest: heartbeat.digest.clone(),
            at: clock.now(),
            sent_at: heartbeat.sent_at,
            last_delivery_at: heartbeat.last_delivery_at,
        };
        match store.record_beat(
            &name,
            &heartbeat.instance,
            beat,
            current.metadata.resource_version,
            by.clone(),
        ) {
            Ok(_) => {
                return Ok(Received::Beat {
                    listener: name,
                    instance: heartbeat.instance,
                });
            }
            Err(StoreError::Conflict { .. }) => continue,
            Err(e) => return Err(e),
        }
    }
    Err(StoreError::Busy(format!(
        "{}/{name}: kept conflicting with other writers",
        listener::KIND
    )))
}

/// Count a refused or unreadable envelope on the account's Listener. Best
/// effort: the refusal is already decided, and a Listener the store cannot
/// write changes nothing about it.
fn record_refusal(store: &dyn Store, clock: &dyn Clock, account: &str, why: Refusal) {
    let name = listener_name(account);
    let by = FieldManager::Listener(account.to_string());
    for _ in 0..CAS_TRIES {
        let Ok(current) = ensure_listener(store, account) else {
            return;
        };
        match store.record_refusal(
            &name,
            why.reason(),
            why.fault(),
            clock.now(),
            current.metadata.resource_version,
            by.clone(),
        ) {
            Err(StoreError::Conflict { .. }) => continue,
            _ => return,
        }
    }
}

/// The account's Listener, created by exclusive creation when absent.
fn ensure_listener(store: &dyn Store, account: &str) -> Result<Object, StoreError> {
    let name = listener_name(account);
    for _ in 0..CAS_TRIES {
        match store.get(listener::KIND, &name) {
            Observation::Known(Some(o)) => return Ok(o),
            Observation::Known(None) => {
                let manifest = Object {
                    api_version: API_VERSION.to_string(),
                    kind: listener::KIND.to_string(),
                    metadata: fleet_api::ObjectMeta {
                        name: name.clone(),
                        ..fleet_api::ObjectMeta::default()
                    },
                    spec: json!({"account": account}),
                    status: Value::Null,
                };
                store.validate(&manifest)?;
                match store.create(manifest, FieldManager::Listener(account.to_string())) {
                    Ok(o) => return Ok(o),
                    Err(StoreError::AlreadyExists(_)) => continue,
                    Err(e) => return Err(e),
                }
            }
            Observation::Unknown { why } => return Err(StoreError::Unknown(why)),
        }
    }
    Err(StoreError::Busy(
        "Listener creation kept conflicting".into(),
    ))
}

/// One Listener per configured account, as the manager loads the config:
/// each created by exclusive creation when absent. Returns what could not
/// be ensured, by account, so a run reports it without stopping.
pub fn ensure_listeners(store: &dyn Store, config: &GatewayConfig) -> Vec<String> {
    let mut problems = Vec::new();
    for account in config.accounts.keys() {
        if let Err(e) = ensure_listener(store, account) {
            problems.push(format!("Listener/{}: {e}", listener_name(account)));
        }
    }
    problems
}
