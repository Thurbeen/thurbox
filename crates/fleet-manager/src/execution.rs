//! Resolve passive launch configuration before the first runtime effect.
use fleet_api::config::{
    ConfigSource, ENV_PROVIDER, EnvProviderSpec, Execution, TASK_CONFIG, TaskConfigSpec, merge_env,
};
use fleet_api::runtime::Runtime;
use fleet_api::task::{TaskSpec, effective_host};
use fleet_api::validate::{typed, validate_env, validate_execution};
use fleet_api::{Object, Observation, Store, StoreError};

fn source(
    store: &dyn Store,
    kind: &str,
    name: &str,
    identities: &mut Vec<ConfigSource>,
) -> Result<Object, StoreError> {
    let obj = match store.get(kind, name) {
        Observation::Known(Some(o)) if !o.is_deleting() => o,
        Observation::Known(_) => {
            return Err(StoreError::Invalid(format!(
                "{kind}/{name} missing or deleting"
            )));
        }
        Observation::Unknown { .. } => {
            return Err(StoreError::Unknown(format!("{kind}/{name} unavailable")));
        }
    };
    identities.push(ConfigSource {
        kind: kind.into(),
        name: name.into(),
        uid: obj.metadata.uid.clone(),
        generation: obj.metadata.generation,
    });
    Ok(obj)
}

pub(crate) fn resolve(
    store: &dyn Store,
    runtime: &dyn Runtime,
    task: &Object,
    lease: &mut fleet_api::LeaseToken,
    ttl: u64,
) -> Result<Execution, StoreError> {
    let spec: TaskSpec = typed("Task", &task.spec)?;
    let mut sources = Vec::new();
    let defaults: TaskConfigSpec = if let Some(r) = &spec.config_ref {
        typed(
            TASK_CONFIG,
            &source(store, TASK_CONFIG, &r.name, &mut sources)?.spec,
        )?
    } else {
        TaskConfigSpec::default()
    };
    // Validate every reference before executing any external provider command.
    let mut providers = Vec::new();
    for r in &defaults.env_from {
        let obj = source(store, ENV_PROVIDER, &r.name, &mut sources)?;
        let provider: EnvProviderSpec = typed(ENV_PROVIDER, &obj.spec)?;
        providers.push((r.name.clone(), provider));
    }
    let harness = if task.spec.get("harness").is_some() {
        spec.harness
    } else {
        defaults.harness.unwrap_or_default()
    };
    let host = effective_host(&task.spec, defaults.host.as_deref());
    let model = if task.spec.get("model").is_some() {
        spec.model
    } else {
        defaults.model
    };
    let pause_after_ms = spec.pause_after_ms.or(defaults.pause_after_ms);
    validate_execution(Some(&harness), Some(&host), model.as_deref())?;
    // The harness profile's variables first, under every source: the
    // frozen launch keeps them as they were when it was resolved.
    let mut env = fleet_api::harness::profile(&harness).launch.env;
    for (name, provider) in providers {
        let mut data = provider.data;
        for (index, s) in provider.sources.iter().enumerate() {
            if matches!(s, fleet_api::config::EnvSource::Command { .. }) && ttl <= 20_000 {
                return Err(StoreError::Invalid(
                    "environment command requires a manager lease above 20000 ms".into(),
                ));
            }
            *lease = store.renew_lease(lease, ttl)?;
            match runtime.env_source(s) {
                Observation::Known(map) => merge_env(&mut data, &map),
                Observation::Unknown { why } => {
                    return Err(StoreError::Unknown(format!(
                        "EnvProvider/{} source {index}: {why}",
                        name
                    )));
                }
            }
            validate_env(&data)?;
        }
        merge_env(&mut env, &data);
    }
    merge_env(&mut env, &defaults.env);
    merge_env(&mut env, &spec.env);
    validate_env(&env)?;
    match runtime.prepare_execution(&Execution {
        harness,
        host,
        model,
        env,
        sources,
        pause_after_ms,
    }) {
        Observation::Known(e) => Ok(e),
        Observation::Unknown { why } => Err(StoreError::Unknown(why)),
    }
}

/// Resolve the same explicit provider sources for plugin observers, once per sweep.
pub(crate) fn resolve_providers(
    store: &dyn Store,
    runtime: &dyn Runtime,
    refs: &[fleet_api::config::ConfigRef],
    lease: &mut fleet_api::LeaseToken,
    ttl: u64,
) -> Result<std::collections::BTreeMap<String, String>, StoreError> {
    let mut identities = Vec::new();
    let mut providers = Vec::new();
    for r in refs {
        providers.push(typed::<EnvProviderSpec>(
            ENV_PROVIDER,
            &source(store, ENV_PROVIDER, &r.name, &mut identities)?.spec,
        )?);
    }
    let mut env = std::collections::BTreeMap::new();
    for provider in providers {
        let mut data = provider.data;
        for source in provider.sources {
            if matches!(source, fleet_api::config::EnvSource::Command { .. }) && ttl <= 20_000 {
                return Err(StoreError::Invalid(
                    "environment command requires lease above 20000 ms".into(),
                ));
            }
            *lease = store.renew_lease(lease, ttl)?;
            match runtime.env_source(&source) {
                Observation::Known(map) => merge_env(&mut data, &map),
                Observation::Unknown { .. } => {
                    return Err(StoreError::Unknown(
                        "plugin environment source unavailable".into(),
                    ));
                }
            }
            validate_env(&data)?;
        }
        merge_env(&mut env, &data);
    }
    validate_env(&env)?;
    Ok(env)
}
