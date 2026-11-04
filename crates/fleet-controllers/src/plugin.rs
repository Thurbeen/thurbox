//! Admission of untrusted plugin plans. No I/O and no execution capability.
use fleet_api::plugin::{CREATOR, Plan, RESERVED, Registration, TaskAccess};
use fleet_api::{FieldManager, Object, StoreError};
use serde_json::Value;
use std::collections::BTreeMap;

/// Amendments per plan. Each one is a new generation of a Task's brief,
/// so a plan carries a few, never a stream.
pub const MAX_AMENDS: usize = 8;

#[derive(Debug)]
pub struct Admitted {
    pub plan: Plan,
    pub writer: FieldManager,
}

pub fn admit(
    raw: Value,
    registration: &Registration,
    object: &Object,
    targets: &BTreeMap<String, Object>,
) -> Result<Admitted, StoreError> {
    let refuse = |why: &str| StoreError::Forbidden(format!("plugin plan: {why}"));
    let plan: Plan = serde_json::from_value(raw).map_err(|_| refuse("malformed plan"))?;
    if plan.creates.len() > 20
        || plan.effects.len() > 8
        || plan.amends.len() > MAX_AMENDS
        || plan.patches.len()
            + plan.appends.len()
            + plan.amends.len()
            + plan.declares.len()
            + plan.deletes.len()
            > 200
    {
        return Err(refuse("too many verbs or effects"));
    }
    let name = &registration.manifest.name;
    let owns = |kind: &str| registration.manifest.kinds.iter().any(|k| k.kind == kind);
    if !owns(&object.kind) {
        return Err(refuse("foreign controller object"));
    }
    if let Some(status) = &plan.status
        && (!status.is_object()
            || RESERVED.iter().any(|f| status.get(f).is_some())
            || serde_json::to_vec(status)
                .map_err(|_| refuse("status encoding"))?
                .len()
                > 256 * 1024)
    {
        return Err(refuse("reserved or oversized status"));
    }
    if plan.remove_finalizer
        && (!object.is_deleting()
            || !object
                .metadata
                .finalizers
                .contains(&format!("fleet.dev/plugin/{name}")))
    {
        return Err(refuse(
            "removeFinalizer on a live object or foreign finalizer",
        ));
    }
    if plan.delete && object.is_deleting() {
        return Err(refuse("delete on a deleting object"));
    }
    let mut effects = std::collections::BTreeSet::new();
    for effect in &plan.effects {
        if effect.key.is_empty() || effect.key.len() > 512 || !effects.insert(&effect.key) {
            return Err(refuse("empty or duplicate effect key"));
        }
    }
    for create in &plan.creates {
        if !(owns(&create.kind) || create.kind == "Task")
            || !create.status.is_null()
            || !create.metadata.uid.is_empty()
            || create.metadata.resource_version != 0
            || create.metadata.generation != 0
            || create.metadata.creation_timestamp.is_some()
            || create.metadata.deletion_timestamp.is_some()
            || !create.metadata.finalizers.is_empty()
            || !create.metadata.owner_references.is_empty()
            || create.metadata.labels.contains_key(CREATOR)
        {
            return Err(refuse("creates: kind, status or metadata"));
        }
        fleet_api::validate::validate_name(&create.metadata.name)?;
    }
    let target = |kind: &str, task: &str, uid: &str| -> Result<(), StoreError> {
        let obj = targets
            .get(&format!("{kind}/{task}"))
            .ok_or_else(|| refuse("target absent or Unknown"))?;
        if obj.metadata.uid != uid {
            return Err(refuse("target uid changed"));
        }
        if kind == "Task" {
            if registration.config.tasks == TaskAccess::Own
                && obj.metadata.labels.get(CREATOR) != Some(name)
            {
                return Err(refuse("foreign Task"));
            }
        } else if !owns(kind) {
            return Err(refuse("foreign target kind"));
        }
        Ok(())
    };
    for patch in &plan.patches {
        target(&patch.kind, &patch.name, &patch.uid)?;
        if patch.kind != "Task"
            || !patch.spec.is_object()
            || patch
                .spec
                .as_object()
                .is_some_and(|p| p.keys().any(|k| k != "dispatch" && k != "forge"))
        {
            return Err(refuse("patches: only Task dispatch and forge"));
        }
    }
    for append in &plan.appends {
        target("Task", &append.task, &append.uid)?;
        if append.key.is_empty() || append.key.len() > 512 || append.text.trim().is_empty() {
            return Err(refuse("appends: empty text or invalid key"));
        }
    }
    for amend in &plan.amends {
        target("Task", &amend.task, &amend.uid)?;
        if amend.text.trim().is_empty() {
            return Err(refuse("amends: empty text"));
        }
    }
    for declare in &plan.declares {
        target("Task", &declare.task, &declare.uid)?;
    }
    for delete in &plan.deletes {
        if delete.kind == object.kind && delete.name == object.metadata.name {
            return Err(refuse(
                "deletes: use delete:true to retire the controller object after its effects",
            ));
        }
        target(&delete.kind, &delete.name, &delete.uid)?;
    }
    Ok(Admitted {
        plan,
        writer: FieldManager::Plugin(name.clone()),
    })
}
