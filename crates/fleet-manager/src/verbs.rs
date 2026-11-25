use std::collections::{BTreeMap, BTreeSet};

use fleet_api::change_request::{self, ChangeRequestSpec};
use fleet_api::conversation::{
    self, CONVERSATION_LABEL, CONVERSATION_UID_LABEL, ConversationSpec, ConversationStatus, Spooled,
};
use fleet_api::event::{Appended, CliSource, Event as BoardEvent, Filter, Page, Publish, Source};
use fleet_api::follow::{DECLARED_CHANGE_REQUEST, ForgeSpec, validate_declaration};
use fleet_api::forge::{ChangeRef, Forge};
use fleet_api::gateway::{
    AuthorKind, ConversationRef, Envelope, Event, Fetched, Gateway, GatewayConfig, admit,
    earns_refusal, starts_task,
};
use fleet_api::inbox::{CLI_SOURCE_PREFIX, InboxEntry, Retry, amend_key, send_key};
use fleet_api::listener::{self, Beat, Heartbeat, listener_name};
use fleet_api::meta::API_VERSION;
use fleet_api::plan::EffectOutcome;
use fleet_api::result::{OutputDecl, TaskResult, validate_outputs};
use fleet_api::subscription::Subscription;
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

const CAS_TRIES: u32 = 8;

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

const PROTECTED_LABELS: [&str; 5] = [
    CONVERSATION_LABEL,
    CONVERSATION_UID_LABEL,
    fleet_api::talk::PARENT_LABEL,
    fleet_api::talk::PARENT_UID_LABEL,
    fleet_api::plugin::CREATOR,
];

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
        merged.remove(fleet_api::plugin::CREATOR);
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

pub fn derived_name(reference: &ChangeRef) -> String {
    let digest = Sha256::digest(reference.canonical().as_bytes());
    let hex: String = digest.iter().take(6).map(|b| format!("{b:02x}")).collect();
    format!("cr-{hex}")
}

pub fn conversation_name(account: &str, conversation: &ConversationRef) -> String {
    let digest = Sha256::digest(conversation.canonical(account).as_bytes());
    let hex: String = digest.iter().take(6).map(|b| format!("{b:02x}")).collect();
    format!("conv-{hex}")
}

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
    Unchanged,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Declared {
    pub outcome: DeclareOutcome,
    pub item: Value,
    pub change_request: String,
    pub created: bool,
}

pub fn declare_change_request(
    store: &dyn Store,
    forge: &dyn Forge,
    task_name: &str,
    url: &str,
) -> Result<Declared, StoreError> {
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

pub fn declare(
    store: &dyn Store,
    task_name: &str,
    item: Value,
) -> Result<DeclareOutcome, StoreError> {
    validate_declaration(&item).map_err(StoreError::Invalid)?;
    append_declaration(store, task_name, item.clone(), |existing| *existing == item)
}

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

pub fn send(
    store: &dyn Store,
    clock: &dyn Clock,
    name: &str,
    user: &str,
    text: &str,
    answers: &[String],
) -> Result<Sent, StoreError> {
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
    let answers: Vec<String> = answers.iter().map(|a| a.trim().to_string()).collect();
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
        if !answers.is_empty() && concluded(&obj)? {
            let request = AmendRequest {
                text: text.to_string(),
                reason: Some(format!("answers {}", answers.join(", "))),
                answers: answers.clone(),
                if_generation: Some(m.generation),
                ..AmendRequest::default()
            };
            match amend(store, clock, name, user, &request) {
                Ok(done) => return Ok(Sent::Amended(done)),
                Err(StoreError::Precondition(_)) => continue,
                Err(e) => return Err(e),
            }
        }
        let entry = InboxEntry {
            key: send_key(&m.name, &m.uid, m.resource_version),
            from: format!("{CLI_SOURCE_PREFIX}{user}"),
            text: text.to_string(),
            about: None,
            answers: answers.clone(),
            amends: None,
            notice: None,
            queued_at: clock.now(),
        };
        match store.append_inbox(
            name,
            entry.clone(),
            m.resource_version,
            FieldManager::Cli,
            None,
        ) {
            Ok(_) => return Ok(Sent::Queued(entry)),
            Err(StoreError::Conflict { .. }) => continue,
            Err(e) => return Err(e),
        }
    }
    Err(StoreError::Busy(format!(
        "{}/{name}: kept conflicting with other writers",
        task::KIND
    )))
}

#[derive(Debug, Clone, PartialEq)]
pub enum Sent {
    Queued(InboxEntry),
    Amended(Amended),
}

fn concluded(obj: &Object) -> Result<bool, StoreError> {
    if obj.status.is_null() {
        return Ok(false);
    }
    let st: TaskStatus = typed(task::KIND, &obj.status)?;
    Ok(st.phase == Phase::Concluded)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Resolution {
    Retry,
    Drop,
}

pub fn resolve_inbox(
    store: &dyn Store,
    clock: &dyn Clock,
    name: &str,
    key: &str,
    resolution: Resolution,
) -> Result<Option<InboxEntry>, StoreError> {
    for _ in 0..CAS_TRIES {
        let obj = match store.get(task::KIND, name) {
            Observation::Known(Some(o)) => o,
            Observation::Known(None) => {
                return Err(StoreError::NotFound(format!("{}/{name}", task::KIND)));
            }
            Observation::Unknown { why } => return Err(StoreError::Unknown(why)),
        };
        let m = &obj.metadata;
        let retry = (resolution == Resolution::Retry).then(|| Retry {
            key: send_key(&m.name, &m.uid, m.resource_version),
            queued_at: clock.now(),
        });
        let copy_key = retry.as_ref().map(|r| r.key.clone());
        match store.resolve_inbox(name, key, retry, m.resource_version, FieldManager::Cli) {
            Ok(written) => {
                let st: TaskStatus = typed(task::KIND, &written.status)?;
                return Ok(copy_key.and_then(|k| st.inbox.into_iter().find(|e| e.key == k)));
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

pub fn publish_event(
    store: &dyn Store,
    board: &str,
    user: &str,
    publish: Publish,
) -> Result<Appended, StoreError> {
    let source = Source::Cli(CliSource::new(user).map_err(StoreError::Invalid)?);
    store
        .append_events(board, &source, vec![publish], FieldManager::Cli, None)?
        .pop()
        .ok_or_else(|| StoreError::Unknown(format!("{board}: the append answered no event")))
}

pub fn subscribe(
    store: &dyn Store,
    name: &str,
    subscription: Subscription,
) -> Result<Object, StoreError> {
    edit_subscriptions(store, name, |subs| {
        if subs
            .iter()
            .any(|s| s.board == subscription.board && s.filter == subscription.filter)
        {
            return Err(StoreError::Invalid(format!(
                "{}/{name} already subscribes to Board/{} with this filter",
                task::KIND,
                subscription.board
            )));
        }
        subs.push(subscription.clone());
        Ok(())
    })
}

pub fn unsubscribe(
    store: &dyn Store,
    name: &str,
    board: &str,
    filter: &Filter,
) -> Result<Object, StoreError> {
    edit_subscriptions(store, name, |subs| {
        let before = subs.len();
        subs.retain(|s| !(s.board == board && s.filter == *filter));
        if subs.len() == before {
            return Err(StoreError::NotFound(format!(
                "{}/{name} has no subscription to Board/{board} with this filter",
                task::KIND
            )));
        }
        Ok(())
    })
}

fn edit_subscriptions(
    store: &dyn Store,
    name: &str,
    edit: impl Fn(&mut Vec<Subscription>) -> Result<(), StoreError>,
) -> Result<Object, StoreError> {
    for _ in 0..CAS_TRIES {
        let current = match store.get(task::KIND, name) {
            Observation::Known(Some(o)) => o,
            Observation::Known(None) => {
                return Err(StoreError::NotFound(format!("{}/{name}", task::KIND)));
            }
            Observation::Unknown { why } => return Err(StoreError::Unknown(why)),
        };
        let mut spec = current.spec.clone();
        let mut subs: Vec<Subscription> = match spec.get("subscriptions") {
            None | Some(Value::Null) => Vec::new(),
            Some(v) => serde_json::from_value(v.clone())
                .map_err(|e| StoreError::Invalid(format!("{}: {e}", current.key())))?,
        };
        edit(&mut subs)?;
        let Some(fields) = spec.as_object_mut() else {
            return Err(StoreError::Invalid(format!(
                "{}: spec is not an object",
                current.key()
            )));
        };
        if subs.is_empty() {
            fields.remove("subscriptions");
        } else {
            fields.insert(
                "subscriptions".into(),
                serde_json::to_value(&subs).map_err(|e| StoreError::Invalid(e.to_string()))?,
            );
        }
        match store.update_spec(
            task::KIND,
            name,
            spec,
            None,
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

pub const MAX_EVENT_LIST: usize = 10_000;
const EVENT_READ_PAGE: usize = 1_000;

pub fn list_events(
    store: &dyn Store,
    board: &str,
    after: u64,
    filter: &Filter,
    limit: usize,
) -> Result<Page, StoreError> {
    check_event_limit(limit)?;
    let end = store.event_head(board)?;
    let mut cursor = after;
    let mut kept = Vec::new();
    loop {
        let events = match store.read_events(board, cursor, EVENT_READ_PAGE)? {
            Page::Events(events) => events,
            expired @ Page::Expired { .. } => return Ok(expired),
        };
        let Some(last) = events.last() else {
            return Ok(Page::Events(kept));
        };
        cursor = last.seq;
        kept.extend(
            events
                .into_iter()
                .filter(|e| e.seq <= end && filter.matches(e)),
        );
        if cursor >= end {
            kept.truncate(limit);
            return Ok(Page::Events(kept));
        }
        if kept.len() >= limit {
            kept.truncate(limit);
            return Ok(Page::Events(kept));
        }
    }
}

pub fn wake(store: &dyn Store, clock: &dyn Clock, name: &str) -> Result<Object, StoreError> {
    for _ in 0..CAS_TRIES {
        let obj = match store.get(task::KIND, name) {
            Observation::Known(Some(o)) => o,
            Observation::Known(None) => {
                return Err(StoreError::NotFound(format!("{}/{name}", task::KIND)));
            }
            Observation::Unknown { why } => return Err(StoreError::Unknown(why)),
        };
        match store.request_wake(
            name,
            clock.now(),
            obj.metadata.resource_version,
            FieldManager::Cli,
        ) {
            Ok(written) => return Ok(written),
            Err(StoreError::Conflict { .. }) => continue,
            Err(e) => return Err(e),
        }
    }
    Err(StoreError::Busy(format!(
        "{}/{name}: kept conflicting with other writers",
        task::KIND
    )))
}

pub fn restart(
    store: &dyn Store,
    clock: &dyn Clock,
    name: &str,
    fresh: bool,
) -> Result<Object, StoreError> {
    for _ in 0..CAS_TRIES {
        let obj = match store.get(task::KIND, name) {
            Observation::Known(Some(o)) => o,
            Observation::Known(None) => {
                return Err(StoreError::NotFound(format!("{}/{name}", task::KIND)));
            }
            Observation::Unknown { why } => return Err(StoreError::Unknown(why)),
        };
        let request = task::RestartRequest {
            at: clock.now(),
            fresh,
        };
        match store.request_restart(
            name,
            request,
            obj.metadata.resource_version,
            FieldManager::Cli,
        ) {
            Ok(written) => return Ok(written),
            Err(StoreError::Conflict { .. }) => continue,
            Err(e) => return Err(e),
        }
    }
    Err(StoreError::Busy(format!(
        "{}/{name}: kept conflicting with other writers",
        task::KIND
    )))
}

pub fn pause(store: &dyn Store, clock: &dyn Clock, name: &str) -> Result<Object, StoreError> {
    for _ in 0..CAS_TRIES {
        let obj = match store.get(task::KIND, name) {
            Observation::Known(Some(o)) => o,
            Observation::Known(None) => {
                return Err(StoreError::NotFound(format!("{}/{name}", task::KIND)));
            }
            Observation::Unknown { why } => return Err(StoreError::Unknown(why)),
        };
        match store.request_pause(
            name,
            clock.now(),
            obj.metadata.resource_version,
            FieldManager::Cli,
        ) {
            Ok(written) => return Ok(written),
            Err(StoreError::Conflict { .. }) => continue,
            Err(e) => return Err(e),
        }
    }
    Err(StoreError::Busy(format!(
        "{}/{name}: kept conflicting with other writers",
        task::KIND
    )))
}

fn check_event_limit(limit: usize) -> Result<(), StoreError> {
    if (1..=MAX_EVENT_LIST).contains(&limit) {
        Ok(())
    } else {
        Err(StoreError::Invalid(format!(
            "limit {limit}: use 1 to {MAX_EVENT_LIST}"
        )))
    }
}

#[derive(Debug, Clone)]
pub struct EventCursor {
    pub board: String,
    pub after: u64,
    pub filter: Filter,
}

pub const MAX_EVENT_PAGES: usize = 4;

impl EventCursor {
    pub fn next(&mut self, store: &dyn Store, limit: usize) -> Result<Page, StoreError> {
        check_event_limit(limit)?;
        let mut kept = Vec::new();
        for _ in 0..MAX_EVENT_PAGES {
            let read = store.read_events(&self.board, self.after, EVENT_READ_PAGE);
            let events = match read {
                Ok(Page::Events(events)) => events,
                _ if !kept.is_empty() => return Ok(Page::Events(kept)),
                expired_or_failed => return expired_or_failed,
            };
            let caught_up = events.len() < EVENT_READ_PAGE;
            for e in events {
                self.after = e.seq;
                if self.filter.matches(&e) {
                    kept.push(e);
                    if kept.len() == limit {
                        return Ok(Page::Events(kept));
                    }
                }
            }
            if caught_up {
                break;
            }
        }
        Ok(Page::Events(kept))
    }
}

pub fn is_transient(e: &StoreError) -> bool {
    matches!(e, StoreError::Busy(_) | StoreError::Unknown(_))
}

pub struct Poll<'a> {
    pub every_ms: u64,
    pub pause: &'a mut dyn FnMut(u64),
    pub warn: &'a mut dyn FnMut(&StoreError),
}

impl std::fmt::Debug for Poll<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Poll")
            .field("every_ms", &self.every_ms)
            .finish_non_exhaustive()
    }
}

impl Poll<'_> {
    fn until(&mut self, clock: &dyn Clock, deadline: fleet_api::Millis) -> bool {
        let now = clock.now();
        if now >= deadline {
            return false;
        }
        (self.pause)(self.every_ms.min(deadline - now));
        true
    }
}

pub fn wait_events(
    store: &dyn Store,
    clock: &dyn Clock,
    cursor: &mut EventCursor,
    limit: usize,
    deadline: fleet_api::Millis,
    poll: &mut Poll<'_>,
) -> Result<Option<Page>, StoreError> {
    loop {
        let from = cursor.after;
        match cursor.next(store, limit) {
            Ok(Page::Events(events)) if events.is_empty() => {}
            Ok(page) => return Ok(Some(page)),
            Err(e) if is_transient(&e) => (poll.warn)(&e),
            Err(e) => return Err(e),
        }
        let behind = cursor.after != from;
        if behind && clock.now() >= deadline {
            return Ok(None);
        }
        if !behind && !poll.until(clock, deadline) {
            return Ok(None);
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BeforeAppend {
    pub task: String,
    pub uid: String,
    pub outbox: fleet_api::event::OutboxCursor,
    pub head: u64,
    pub board_uid: Option<String>,
}

pub fn before_append(
    store: &dyn Store,
    session_key: &str,
    board: &str,
) -> Result<BeforeAppend, StoreError> {
    let tasks = match store.list(task::KIND) {
        Observation::Known(tasks) => tasks,
        Observation::Unknown { why } => return Err(StoreError::Unknown(why)),
    };
    let owner = tasks
        .iter()
        .find(|t| fleet_api::session::session_key(&t.metadata.name, &t.metadata.uid) == session_key)
        .ok_or_else(|| StoreError::NotFound(format!("no Task runs the session {session_key}")))?;
    let (head, board_uid) = board_head(store, board)?;
    Ok(BeforeAppend {
        task: owner.metadata.name.clone(),
        uid: owner.metadata.uid.clone(),
        outbox: outbox_of(owner)?,
        head,
        board_uid,
    })
}

const BOARD_HEAD_TRIES: u32 = 3;

fn board_head(store: &dyn Store, board: &str) -> Result<(u64, Option<String>), StoreError> {
    for _ in 0..BOARD_HEAD_TRIES {
        let uid = board_uid(store, board)?;
        let head = match store.event_head(board) {
            Err(StoreError::NotFound(_)) => 0,
            head => head?,
        };
        if board_uid(store, board)? == uid {
            return Ok((head, uid));
        }
    }
    Err(StoreError::Busy(format!(
        "Board/{board} was made again during each of {BOARD_HEAD_TRIES} reads of its head"
    )))
}

fn board_uid(store: &dyn Store, board: &str) -> Result<Option<String>, StoreError> {
    match store.get(fleet_api::board::KIND, board) {
        Observation::Known(found) => Ok(found.map(|b| b.metadata.uid)),
        Observation::Unknown { why } => Err(StoreError::Unknown(why)),
    }
}

fn outbox_of(task: &Object) -> Result<fleet_api::event::OutboxCursor, StoreError> {
    let status: TaskStatus = if task.status.is_null() {
        TaskStatus::default()
    } else {
        typed(task::KIND, &task.status)?
    };
    Ok(status.outbox.unwrap_or_default())
}

impl BeforeAppend {
    pub fn queued(&self, offset: u64, end: u64, board: &str, key: Option<String>) -> QueuedLine {
        let generation = if offset < self.outbox.offset {
            self.outbox.generation + 1
        } else {
            self.outbox.generation
        };
        QueuedLine {
            task: self.task.clone(),
            uid: self.uid.clone(),
            board: board.to_string(),
            key: key.unwrap_or_else(|| fleet_api::event::outbox_key(generation, offset)),
            offset,
            end,
            generation,
            head: self.head,
            board_uid: self.board_uid.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueuedLine {
    pub task: String,
    pub uid: String,
    pub board: String,
    pub key: String,
    pub offset: u64,
    pub end: u64,
    pub generation: u64,
    pub head: u64,
    pub board_uid: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Ingest {
    Pending,
    Appended {
        event: Box<BoardEvent>,
        duplicate: bool,
    },
    Skipped(String),
}

pub fn ingested(store: &dyn Store, line: &QueuedLine) -> Result<Ingest, StoreError> {
    let owner = match store.get(task::KIND, &line.task) {
        Observation::Known(Some(t)) if t.metadata.uid == line.uid => t,
        Observation::Known(_) => {
            return Err(StoreError::NotFound(format!(
                "{}/{} is gone; nothing reads its outbox",
                task::KIND,
                line.task
            )));
        }
        Observation::Unknown { why } => return Err(StoreError::Unknown(why)),
    };
    let outbox = outbox_of(&owner)?;
    if (outbox.generation, outbox.offset) < (line.generation, line.end) {
        return Ok(Ingest::Pending);
    }
    if outbox.generation == line.generation
        && let Some(why) = outbox.skipped_at(line.offset)
    {
        return Ok(Ingest::Skipped(why.to_string()));
    }
    let same_board = board_uid(store, &line.board)? == line.board_uid;
    let fresh_after = if same_board { line.head } else { 0 };
    if let Some(event) = find_published(store, line, fresh_after, u64::MAX)? {
        return Ok(Ingest::Appended {
            event: Box::new(event),
            duplicate: false,
        });
    }
    if same_board && let Some(event) = find_published(store, line, 0, line.head)? {
        return Ok(Ingest::Appended {
            event: Box::new(event),
            duplicate: true,
        });
    }
    Ok(Ingest::Skipped(format!(
        "offset {}: Board/{} holds no event for the line: Fleet skipped it, retention dropped its event, or the outbox was made again",
        line.offset, line.board
    )))
}

pub fn wait_ingested(
    store: &dyn Store,
    clock: &dyn Clock,
    line: &QueuedLine,
    deadline: fleet_api::Millis,
    poll: &mut Poll<'_>,
) -> Result<Ingest, StoreError> {
    loop {
        match ingested(store, line) {
            Ok(Ingest::Pending) => {}
            Ok(ingest) => return Ok(ingest),
            Err(e) if is_transient(&e) => (poll.warn)(&e),
            Err(e) => return Err(e),
        }
        if !poll.until(clock, deadline) {
            return Ok(Ingest::Pending);
        }
    }
}

fn find_published(
    store: &dyn Store,
    line: &QueuedLine,
    after: u64,
    through: u64,
) -> Result<Option<BoardEvent>, StoreError> {
    let by_task = |e: &BoardEvent| matches!(&e.source, Source::Task(t) if t.task == line.task && t.uid == line.uid);
    let mut cursor = EventCursor {
        board: line.board.clone(),
        after,
        filter: Filter::default(),
    };
    while cursor.after < through {
        let events = match cursor.next(store, EVENT_READ_PAGE) {
            Ok(Page::Events(events)) => events,
            Ok(Page::Expired { oldest }) => {
                cursor.after = oldest - 1;
                continue;
            }
            Err(StoreError::NotFound(_)) => return Ok(None),
            Err(e) => return Err(e),
        };
        if events.is_empty() {
            return Ok(None);
        }
        if let Some(e) = events
            .into_iter()
            .take_while(|e| e.seq <= through)
            .find(|e| e.key.as_deref() == Some(line.key.as_str()) && by_task(e))
        {
            return Ok(Some(e));
        }
    }
    Ok(None)
}

#[derive(Debug, Clone, Default)]
pub struct AmendRequest {
    pub text: String,
    pub reason: Option<String>,
    pub outputs: std::collections::BTreeMap<String, OutputDecl>,
    pub answers: Vec<String>,
    pub if_generation: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Amended {
    pub task: String,
    pub amendment: usize,
    pub previous_generation: u64,
    pub generation: u64,
    pub key: String,
    pub from: String,
}

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
            answers: request.answers.clone(),
            amends: Some(generation),
            notice: None,
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
    Unchanged,
}

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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "lowercase")]
pub enum Received {
    Written {
        conversation: String,
        delivery: String,
    },
    Duplicate {
        conversation: String,
        delivery: String,
    },
    Ignored {
        why: String,
    },
    Beat {
        listener: String,
        instance: String,
    },
}

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

pub fn fetch_file(
    store: &dyn Store,
    gateway: &dyn Gateway,
    task_name: &str,
    file: &str,
    dir: &std::path::Path,
) -> Result<Fetched, StoreError> {
    let read = |kind: &str, name: &str| match store.get(kind, name) {
        Observation::Known(Some(o)) => Ok(o),
        Observation::Known(None) => Err(StoreError::NotFound(format!("{kind}/{name}"))),
        Observation::Unknown { why } => Err(StoreError::Unknown(why)),
    };
    let obj = read(task::KIND, task_name)?;
    let st: TaskStatus = if obj.status.is_null() {
        TaskStatus::default()
    } else {
        typed(task::KIND, &obj.status)?
    };
    let Some(tie) = st.gateway.and_then(|g| g.conversation) else {
        return Err(StoreError::Forbidden(format!(
            "{}: the Task is tied to no conversation, so it has no shared files",
            obj.key()
        )));
    };
    let conv = read(conversation::KIND, &tie.name)?;
    if conv.metadata.uid != tie.uid {
        return Err(StoreError::Forbidden(format!(
            "{}: the Conversation the Task is tied to was replaced",
            conv.key()
        )));
    }
    let spec: ConversationSpec = typed(conversation::KIND, &conv.spec)?;
    let cs: ConversationStatus = if conv.status.is_null() {
        ConversationStatus::default()
    } else {
        typed(conversation::KIND, &conv.status)?
    };
    let Some(shared) = cs
        .messages
        .iter()
        .flat_map(|m| m.files.iter())
        .find(|f| f.id == file)
    else {
        return Err(StoreError::NotFound(format!(
            "file {file:?} is shared on no message {} holds",
            conv.key()
        )));
    };
    if let Some(why) = &shared.unavailable {
        return Ok(Fetched::Refused { why: why.clone() });
    }
    let reference = match cs.reference {
        Some(r) => r,
        None => match ConversationRef::split(&spec.address) {
            Some(r) => r,
            None => {
                return Err(StoreError::Invalid(format!(
                    "{}: address {:?} cannot be read",
                    conv.key(),
                    spec.address
                )));
            }
        },
    };
    match gateway.fetch(&spec.account, &reference, file, dir) {
        Observation::Known(f) => Ok(f),
        Observation::Unknown { why } => Err(StoreError::Unknown(why)),
    }
}

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

pub fn ensure_listeners(store: &dyn Store, config: &GatewayConfig) -> Vec<String> {
    let mut problems = Vec::new();
    for account in config.accounts.keys() {
        if let Err(e) = ensure_listener(store, account) {
            problems.push(format!("Listener/{}: {e}", listener_name(account)));
        }
    }
    problems
}
