//! Plugin registration and sweeps. All untrusted plans pass pure admission first.
use crate::{Manager, RunReport, Step};
use fleet_api::plugin::{
    Manifest, Mutation, Plan, PluginConfig, Plugins, Probe, RESERVED, Registration,
};
use fleet_api::{FieldManager, LeaseToken, Object, Observation, StoreError};
use fleet_controllers::plugin::admit;
use serde_json::{Value, json};
use std::collections::BTreeMap;

pub fn register(
    port: &dyn Plugins,
    configs: &BTreeMap<String, PluginConfig>,
) -> Result<Vec<Registration>, StoreError> {
    let mut registrations = Vec::new();
    let mut kinds = std::collections::BTreeSet::new();
    for (name, config) in configs {
        let answer = port
            .call(
                name,
                "manifest",
                json!({"protocol":1,"plugin":name,"call":"manifest","id":"manifest"}),
                &BTreeMap::new(),
            )
            .map_err(StoreError::Invalid)?;
        let manifest: Manifest = serde_json::from_value(answer["manifest"].clone())
            .map_err(|_| StoreError::Invalid("malformed plugin manifest".into()))?;
        fleet_api::plugin::validate_manifest(&manifest, name)?;
        for kind in &manifest.kinds {
            if fleet_api::plugin::kind_aliases(&kind.kind)
                .iter()
                .any(|alias| kinds.contains(alias))
            {
                return Err(StoreError::Invalid(
                    "two plugins claim the same kind".into(),
                ));
            }
        }
        for kind in &manifest.kinds {
            kinds.extend(fleet_api::plugin::kind_aliases(&kind.kind));
        }
        registrations.push(Registration {
            manifest,
            config: config.clone(),
        });
    }
    Ok(registrations)
}

fn object(store: &dyn fleet_api::Store, kind: &str, name: &str) -> Result<Object, StoreError> {
    match store.get(kind, name) {
        Observation::Known(Some(o)) => Ok(o),
        Observation::Known(None) => Err(StoreError::NotFound(format!("{kind}/{name}"))),
        Observation::Unknown { why } => Err(StoreError::Unknown(why)),
    }
}

impl Manager<'_> {
    pub(crate) fn sweep_plugins(
        &self,
        lease: &mut LeaseToken,
        ttl: u64,
        report: &mut RunReport,
    ) -> Result<bool, StoreError> {
        let registered = self
            .plugins
            .map(|(_, registrations)| registrations)
            .unwrap_or(&[]);
        for kind in self.store.served_kinds() {
            if fleet_api::kinds::is_served(&kind)
                || registered
                    .iter()
                    .any(|r| r.manifest.kinds.iter().any(|k| k.kind == kind))
            {
                continue;
            }
            match self.store.list(&kind) {
                Observation::Known(objects) if !objects.is_empty() => {
                    let message = format!(
                        "{kind}: unregistered plugin; {} objects retained without reconciliation",
                        objects.len()
                    );
                    if !report.errors.contains(&message) {
                        report.errors.push(message);
                    }
                }
                Observation::Unknown { why } => report.unknowns.push(why),
                _ => {}
            }
        }
        let Some((port, registrations)) = self.plugins else {
            return Ok(false);
        };
        let mut busy = false;
        for registration in registrations {
            if !port.available(&registration.manifest.name) {
                *lease = self.store.renew_lease(lease, ttl)?;
                let name = &registration.manifest.name;
                if port
                    .call(
                        name,
                        "manifest",
                        json!({"protocol":1,"plugin":name,"call":"manifest","id":"manifest"}),
                        &BTreeMap::new(),
                    )
                    .is_err()
                {
                    report.errors.push(format!("plugin {name} unavailable"));
                    continue;
                }
            }
            let mut env = None;
            for kind in &registration.manifest.kinds {
                let objects = match self.store.list(&kind.kind) {
                    Observation::Known(o) => o,
                    Observation::Unknown { why } => {
                        report.unknowns.push(why);
                        continue;
                    }
                };
                for obj in objects {
                    if !port.available(&registration.manifest.name) {
                        report
                            .errors
                            .push(format!("plugin {} unavailable", registration.manifest.name));
                        break;
                    }
                    let now = self.clock.now();
                    let due = obj.is_deleting()
                        || obj.status["notBefore"].as_u64().is_none_or(|t| t <= now)
                        || obj.status["observedGeneration"]
                            .as_u64()
                            .unwrap_or(0)
                            .max(obj.status["host"]["generation"].as_u64().unwrap_or(0))
                            < obj.metadata.generation;
                    if !due {
                        continue;
                    }
                    let step =
                        self.reconcile_plugin(port, registration, &obj, lease, ttl, &mut env);
                    busy |= self.tally(step, &obj.key(), report)?;
                }
            }
        }
        Ok(busy)
    }

    fn plugin_call(
        &self,
        plugin: (&dyn Plugins, &Registration),
        call: &str,
        mut input: Value,
        env: &BTreeMap<String, String>,
        lease: &mut LeaseToken,
        ttl: u64,
    ) -> Result<Value, StoreError> {
        let (port, registration) = plugin;
        *lease = self.store.renew_lease(lease, ttl)?;
        input["protocol"] = json!(1);
        input["plugin"] = json!(registration.manifest.name);
        input["call"] = json!(call);
        input["id"] = json!(call);
        port.call(&registration.manifest.name, call, input, env)
            .map_err(|_| StoreError::Unknown(format!("plugin {call} call failed")))
    }

    fn observer_env(
        &self,
        registration: &Registration,
        lease: &mut LeaseToken,
        ttl: u64,
        cache: &mut Option<BTreeMap<String, String>>,
    ) -> Result<BTreeMap<String, String>, StoreError> {
        if cache.is_none() {
            *cache = Some(crate::execution::resolve_providers(
                self.store,
                self.runtime,
                &registration.config.env_from,
                lease,
                ttl,
            )?);
        }
        Ok(cache.as_ref().expect("resolved").clone())
    }

    fn reconcile_plugin(
        &self,
        port: &dyn Plugins,
        reg: &Registration,
        obj: &Object,
        lease: &mut LeaseToken,
        ttl: u64,
        env: &mut Option<BTreeMap<String, String>>,
    ) -> Result<Step, StoreError> {
        let now = self.clock.now();
        let definition = reg
            .manifest
            .kinds
            .iter()
            .find(|k| k.kind == obj.kind)
            .expect("registered kind");
        // Lost answers become explicit Unknown receipts. No pending intent is replayed.
        let mut recovered = obj.status.clone();
        if recovered.is_null() {
            recovered = json!({});
        }
        if let Some(effects) = recovered.get_mut("effects").and_then(Value::as_object_mut) {
            for effect in effects.values_mut() {
                if effect.get("receipt").is_none()
                    && effect["intent"]["at"].as_u64().is_some_and(|at| {
                        now > at
                            .saturating_add(reg.config.call_timeout_ms)
                            .saturating_add(ttl)
                    })
                {
                    effect["receipt"] =
                        json!({"at":now,"outcome":{"unknown":{"why":"LostAnswer"}}});
                }
            }
        }
        let mut input_obj = obj.clone();
        input_obj.status = recovered;
        let prepare = (|| {
            let mut observations = serde_json::Map::new();
            let empty = BTreeMap::new();
            for round in 0..=3 {
                let input = json!({"object":input_obj,"observations":observations,"now":now,"defaults":{"pollMs":definition.poll_ms}});
                let answer = self.plugin_call((port, reg), "probes", input, &empty, lease, ttl)?;
                let probes: Vec<Probe> = serde_json::from_value(answer["probes"].clone())
                    .map_err(|_| StoreError::Invalid("malformed probes".into()))?;
                if probes.is_empty() {
                    break;
                }
                if round == 3 || probes.len() > 200 {
                    return Err(StoreError::Invalid(
                        "plugin probe rounds or count exceeded".into(),
                    ));
                }
                for probe in probes {
                    if probe.id().is_empty() || observations.contains_key(probe.id()) {
                        return Err(StoreError::Invalid("duplicate probe id".into()));
                    }
                    let value = match &probe {
                        Probe::Object { kind, name, .. } => match self.store.get(kind, name) {
                            Observation::Known(v) => json!({"known":v}),
                            Observation::Unknown { why } => json!({"unknown":{"why":why}}),
                        },
                        Probe::List { kind, labels, .. } => match self.store.list(kind) {
                            Observation::Known(objects) => {
                                let selected: Vec<_> = objects
                                    .into_iter()
                                    .filter(|o| {
                                        labels
                                            .iter()
                                            .all(|(k, v)| o.metadata.labels.get(k) == Some(v))
                                    })
                                    .collect();
                                if selected.len() > 200 {
                                    json!({"unknown":{"why":"TooMany"}})
                                } else {
                                    json!({"known":selected})
                                }
                            }
                            Observation::Unknown { why } => json!({"unknown":{"why":why}}),
                        },
                        Probe::Plugin { id, request } => {
                            let resolved = self.observer_env(reg, lease, ttl, env)?;
                            let answer = self.plugin_call(
                                (port, reg),
                                "observe",
                                json!({"probe":{"id":id,"request":request}}),
                                &resolved,
                                lease,
                                ttl,
                            )?;
                            if answer.get("known").is_some() && answer.get("unknown").is_none() {
                                json!({"known":answer["known"]})
                            } else if answer.get("known").is_none()
                                && answer["unknown"]["why"].is_string()
                            {
                                json!({"unknown":answer["unknown"]})
                            } else {
                                return Err(StoreError::Invalid("malformed observation".into()));
                            }
                        }
                    };
                    observations.insert(probe.id().into(), value);
                }
            }
            let answer=self.plugin_call((port,reg),"reconcile",json!({"object":input_obj,"observations":observations,"now":now,"defaults":{"pollMs":definition.poll_ms}}),&empty,lease,ttl)?;
            let plan: Plan = serde_json::from_value(answer["plan"].clone())
                .map_err(|_| StoreError::Invalid("malformed plugin plan".into()))?;
            let mut targets = BTreeMap::new();
            for (kind, name) in plan
                .patches
                .iter()
                .map(|p| (&*p.kind, &*p.name))
                .chain(plan.deletes.iter().map(|d| (&*d.kind, &*d.name)))
                .chain(plan.appends.iter().map(|a| ("Task", &*a.task)))
                .chain(plan.amends.iter().map(|a| ("Task", &*a.task)))
                .chain(plan.declares.iter().map(|d| ("Task", &*d.task)))
            {
                targets.insert(format!("{kind}/{name}"), object(self.store, kind, name)?);
            }
            let admitted = admit(answer["plan"].clone(), reg, obj, &targets)?;
            for create in &admitted.plan.creates {
                self.store.validate(create)?;
            }
            for patch in &admitted.plan.patches {
                let current = &targets[&format!("{}/{}", patch.kind, patch.name)];
                let mut spec = current.spec.clone();
                for (k, v) in patch.spec.as_object().expect("admitted") {
                    spec[k] = v.clone();
                }
                fleet_api::validate::validate_spec_update(&patch.kind, &current.spec, &spec)?;
            }
            if let Some(status) = &admitted.plan.status {
                self.store.validate_plugin_status(&obj.kind, status)?;
            }
            Ok(admitted)
        })();
        let admitted = match prepare {
            Ok(a) => a,
            Err(e @ StoreError::LeaseLost(_)) => return Err(e),
            Err(e) => {
                let mut status = input_obj.status.clone();
                status["host"] = json!({"observedAt":now,"generation":obj.metadata.generation,"error":e.to_string()});
                status["notBefore"] = json!(now.saturating_add(definition.poll_ms));
                // Record failures using the same admitted owner, without accepting any plugin writes.
                let by =
                    fleet_controllers::plugin::admit(json!({}), reg, obj, &BTreeMap::new())?.writer;
                self.store.update_status(
                    &obj.kind,
                    &obj.metadata.name,
                    status,
                    obj.metadata.resource_version,
                    by,
                    Some(lease),
                )?;
                return Err(e);
            }
        };
        let plan = admitted.plan;
        let by = admitted.writer;
        let mut status = plan
            .status
            .clone()
            .unwrap_or_else(|| input_obj.status.clone());
        for field in RESERVED {
            if let Some(value) = input_obj.status.get(field) {
                status[field] = value.clone();
            }
        }
        status["notBefore"] = json!(
            plan.not_before
                .unwrap_or_else(|| now.saturating_add(definition.poll_ms))
        );
        status["host"] = json!({"observedAt":now,"generation":obj.metadata.generation});
        let mut fresh = Vec::new();
        if !plan.effects.is_empty() && !status["effects"].is_object() {
            status["effects"] = json!({});
        }
        for effect in &plan.effects {
            if status["effects"].get(&effect.key).is_none() {
                status["effects"][&effect.key] =
                    json!({"intent":{"at":now,"generation":obj.metadata.generation}});
                fresh.push(effect);
            }
        }
        if let Some(effects) = status.get_mut("effects").and_then(Value::as_object_mut) {
            while effects.len() > 100 {
                let oldest = effects
                    .iter()
                    .filter(|(_, e)| e.get("receipt").is_some())
                    .min_by_key(|(_, e)| e["intent"]["at"].as_u64().unwrap_or(0))
                    .map(|(k, _)| k.clone());
                if let Some(key) = oldest {
                    effects.remove(&key);
                } else {
                    break;
                }
            }
        }
        let mut current = self.store.update_status(
            &obj.kind,
            &obj.metadata.name,
            status,
            obj.metadata.resource_version,
            by.clone(),
            Some(lease),
        )?;
        let mut step = Step {
            wrote: 1,
            ..Step::default()
        };
        let mut verbs = Vec::new();
        let mutations = plan
            .creates
            .into_iter()
            .map(Mutation::Create)
            // Before patches: `ifGeneration` names the generation the plan
            // was computed against, which a patch would move.
            .chain(plan.amends.into_iter().map(Mutation::Amend))
            .chain(plan.patches.into_iter().map(Mutation::Patch))
            .chain(plan.appends.into_iter().map(Mutation::Append));
        for mutation in mutations {
            *lease = self.store.renew_lease(lease, ttl)?;
            let result = self.store.plugin_mutate(mutation, by.clone(), lease);
            verbs.push(verb_result(result)?);
        }
        for declaration in plan.declares {
            let result = (|| {
                let forge = self
                    .forge
                    .ok_or_else(|| StoreError::Invalid("declare needs forge".into()))?;
                let task = object(self.store, "Task", &declaration.task)?;
                if task.metadata.uid != declaration.uid
                    || task.is_deleting()
                    || task.status.get("brief").is_none_or(Value::is_null)
                {
                    return Err(StoreError::Forbidden("ineligible declaration Task".into()));
                }
                *lease = self.store.renew_lease(lease, ttl)?;
                let (name, _) = crate::verbs::ensure_change_request(
                    self.store,
                    forge,
                    &declaration.change_request,
                )?;
                self.store.plugin_mutate(Mutation::Declare{task:declaration.task,uid:declaration.uid,item:json!({"kind":"ChangeRequest","name":name,"url":declaration.change_request})},by.clone(),lease)
            })();
            verbs.push(verb_result(result)?);
        }
        for delete in plan.deletes {
            verbs.push(verb_result(self.store.plugin_mutate(
                Mutation::Delete(delete),
                by.clone(),
                lease,
            ))?);
        }
        for effect in fresh {
            let resolved = self.observer_env(reg, lease, ttl, env);
            let answer=match resolved { Ok(env)=>self.plugin_call((port,reg),"perform",json!({"effect":effect,"intentKey":format!("plugin/{}/{}/{}/{}/{}",reg.manifest.name,obj.kind,obj.metadata.name,obj.metadata.uid,effect.key)}),&env,lease,ttl), Err(e)=>Err(e) };
            let outcome = match answer {
                Ok(answer) if valid_outcome(&answer["outcome"]) => answer["outcome"].clone(),
                Err(e @ StoreError::LeaseLost(_)) => return Err(e),
                _ => json!({"unknown":{"why":"LostAnswer"}}),
            };
            let receipt = json!({"at":self.clock.now(),"outcome":outcome});
            current = self.update_plugin_receipt(
                &current,
                by.clone(),
                lease,
                Some((&effect.key, receipt)),
                None,
            )?;
            step.effects += 1;
            step.wrote += 1;
        }
        current =
            self.update_plugin_receipt(&current, by.clone(), lease, None, Some(json!(verbs)))?;
        step.wrote += 1;
        if plan.delete {
            self.store.delete_fenced(
                &obj.kind,
                &obj.metadata.name,
                &obj.metadata.uid,
                current.metadata.resource_version,
                by.clone(),
                lease,
            )?;
        }
        if plan.remove_finalizer {
            self.store.remove_finalizer(
                &obj.kind,
                &obj.metadata.name,
                &format!("fleet.dev/plugin/{}", reg.manifest.name),
                current.metadata.resource_version,
                by,
                lease,
            )?;
        }
        Ok(step)
    }

    fn update_plugin_receipt(
        &self,
        obj: &Object,
        by: FieldManager,
        lease: &LeaseToken,
        receipt: Option<(&str, Value)>,
        verbs: Option<Value>,
    ) -> Result<Object, StoreError> {
        for _ in 0..8 {
            let fresh = object(self.store, &obj.kind, &obj.metadata.name)?;
            if fresh.metadata.uid != obj.metadata.uid {
                return Err(StoreError::Forbidden("plugin object uid changed".into()));
            }
            let mut status = fresh.status.clone();
            if let Some((key, value)) = &receipt {
                status["effects"][key]["receipt"] = value.clone();
            }
            if let Some(verbs) = &verbs {
                status["host"]["verbs"] = verbs.clone();
            }
            match self.store.update_status(
                &obj.kind,
                &obj.metadata.name,
                status,
                fresh.metadata.resource_version,
                by.clone(),
                Some(lease),
            ) {
                Err(StoreError::Conflict { .. }) => continue,
                other => return other,
            }
        }
        Err(StoreError::Busy("plugin receipt conflicts".into()))
    }
}
fn valid_outcome(value: &Value) -> bool {
    value.as_object().is_some_and(|v| {
        v.len() == 1
            && ["done", "failed", "unknown"]
                .iter()
                .any(|k| v.contains_key(*k))
    })
}
fn verb_result(result: Result<Value, StoreError>) -> Result<Value, StoreError> {
    match result {
        Ok(v) => Ok(json!({"ok":v})),
        Err(e @ StoreError::LeaseLost(_)) => Err(e),
        Err(e) => Ok(json!({"error":e.to_string()})),
    }
}
