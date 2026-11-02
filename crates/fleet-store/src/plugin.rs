//! Atomic plugin verbs: rights and identity are rechecked under the write lock.
use super::*;
use fleet_api::plugin::{CREATOR, Mutation, TaskAccess};
use serde_json::json;

impl FileStore {
    pub(super) fn mutate_plugin(
        &self,
        mutation: Mutation,
        by: FieldManager,
        lease: &LeaseToken,
    ) -> Result<Value, StoreError> {
        let name = by
            .name()
            .strip_prefix("plugin/")
            .ok_or_else(|| StoreError::Forbidden("plugin verb requires its writer".into()))?
            .to_owned();
        let access = self
            .plugin_rights
            .get(&name)
            .ok_or_else(|| StoreError::Forbidden("unregistered plugin".into()))?;
        let _guard = self.lock()?;
        self.check_lease(lease)?;
        if let Mutation::Create(obj) = mutation {
            if obj.kind != "Task" && !self.plugins.get(&obj.kind).is_some_and(|(p, _)| p == &name) {
                return Err(StoreError::Forbidden("foreign create kind".into()));
            }
            check_conversation_labels(&obj, &by)?;
            match self.read(&obj.kind, &obj.metadata.name) {
                Observation::Known(Some(current))
                    if current.metadata.labels.get(CREATOR) == Some(&name) =>
                {
                    return Ok(json!("already"));
                }
                Observation::Known(Some(_)) => {
                    return Err(StoreError::Forbidden(
                        "name belongs to another creator".into(),
                    ));
                }
                Observation::Unknown { why } => return Err(StoreError::Unknown(why)),
                _ => {}
            }
            self.check_parent(&obj, &by)?;
            return self.insert(obj, Some(&name)).map(|_| json!("created"));
        }
        let (kind, target, uid) = match &mutation {
            Mutation::Patch(p) => (&*p.kind, &*p.name, &*p.uid),
            Mutation::Delete(d) => (&*d.kind, &*d.name, &*d.uid),
            Mutation::Append(a) => ("Task", &*a.task, &*a.uid),
            Mutation::Declare { task, uid, .. } => ("Task", &**task, &**uid),
            Mutation::Create(_) => unreachable!(),
        };
        let mut obj = self.read_existing(kind, target)?;
        if obj.metadata.uid != uid {
            return Err(StoreError::Forbidden("target uid changed".into()));
        }
        if kind == "Task" {
            if *access == TaskAccess::Own && obj.metadata.labels.get(CREATOR) != Some(&name) {
                return Err(StoreError::Forbidden("foreign Task".into()));
            }
        } else if !self.plugins.get(kind).is_some_and(|(p, _)| p == &name) {
            return Err(StoreError::Forbidden("foreign plugin kind".into()));
        }
        if let Mutation::Delete(_) = mutation {
            return self.mark_deleted(obj).map(|_| json!("deleted"));
        }
        if obj.is_deleting() {
            return Err(StoreError::Forbidden("target is deleting".into()));
        }
        match &mutation {
            Mutation::Patch(p) => {
                let patch = p
                    .spec
                    .as_object()
                    .ok_or_else(|| StoreError::Invalid("patch must be object".into()))?;
                if kind != "Task" || patch.keys().any(|k| k != "dispatch" && k != "forge") {
                    return Err(StoreError::Forbidden(
                        "only Task dispatch and forge may be patched".into(),
                    ));
                }
                let mut spec = obj.spec.clone();
                for (k, v) in patch {
                    spec[k] = v.clone();
                }
                validate_spec_update(kind, &obj.spec, &spec)?;
                if spec == obj.spec {
                    return Ok(json!("unchanged"));
                }
                obj.spec = spec;
                obj.metadata.generation = obj
                    .metadata
                    .generation
                    .checked_add(1)
                    .ok_or_else(|| StoreError::Invalid("generation exhausted".into()))?;
            }
            Mutation::Append(a) => {
                let mut status = task_status(&obj)?;
                let entry = InboxEntry {
                    key: format!("task/{}/{}/plugin/{}/{}", a.task, a.uid, name, a.key),
                    from: format!("plugin/{name}"),
                    text: a.text.clone(),
                    about: None,
                    answers: a.answers.clone(),
                    amends: None,
                    queued_at: self.now(),
                };
                if status.inbox.iter().any(|e| e.key == entry.key)
                    || status
                        .deliveries
                        .receipts
                        .iter()
                        .any(|r| r.key == entry.key)
                {
                    return Ok(json!("already"));
                }
                if entry.text.trim().is_empty() || a.key.is_empty() || a.key.len() > 512 {
                    return Err(StoreError::Invalid("invalid plugin inbox entry".into()));
                }
                push_entry(&obj, &mut status, entry)?;
                obj.status = encode_status(&status)?;
            }
            Mutation::Declare { item, .. } => {
                fleet_api::follow::validate_declaration(item).map_err(StoreError::Invalid)?;
                let mut status = task_status(&obj)?;
                if status.brief.is_none() {
                    return Err(StoreError::Forbidden("Task has no brief receipt".into()));
                }
                if status.declared.contains(item) {
                    return Ok(json!("already"));
                }
                status.declared.push(item.clone());
                obj.status = encode_status(&status)?;
            }
            _ => unreachable!(),
        }
        obj.metadata.resource_version = self.next_rv()?;
        self.write(&obj)?;
        Ok(json!("applied"))
    }
}
