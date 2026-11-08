//! Process-independent protocol and registration values for external controllers.
use crate::{Millis, Object, StoreError};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

pub const CREATOR: &str = "fleet.dev/plugin";
pub const RESERVED: [&str; 3] = ["notBefore", "effects", "host"];

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PluginConfig {
    pub program: String,
    #[serde(default)]
    pub args: Vec<String>,
    pub cwd: String,
    #[serde(default = "timeout")]
    pub call_timeout_ms: Millis,
    #[serde(default)]
    pub tasks: TaskAccess,
    #[serde(default)]
    pub pass_env: Vec<String>,
    #[serde(default)]
    pub env_from: Vec<crate::config::ConfigRef>,
}
fn timeout() -> Millis {
    15_000
}

#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum TaskAccess {
    #[default]
    Own,
    Any,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PluginKind {
    pub kind: String,
    pub poll_ms: Millis,
    #[serde(default)]
    pub finalizer: bool,
    #[serde(default)]
    pub immutable_spec: bool,
    pub spec: Value,
    pub status: Option<Value>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub protocol: u32,
    pub name: String,
    pub kinds: Vec<PluginKind>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Registration {
    pub manifest: Manifest,
    pub config: PluginConfig,
}

/// A call failure is distinct from a successful observer's Unknown answer.
pub trait Plugins {
    fn available(&self, _plugin: &str) -> bool {
        true
    }
    fn operation_bound(&self) -> Millis;
    fn call(
        &self,
        plugin: &str,
        call: &str,
        request: Value,
        env: &BTreeMap<String, String>,
    ) -> Result<Value, String>;
}

pub fn validate_manifest(manifest: &Manifest, name: &str) -> Result<(), StoreError> {
    crate::validate::validate_name(name)?;
    if manifest.protocol != 1 || manifest.name != name || manifest.kinds.is_empty() {
        return Err(StoreError::Invalid(
            "plugin manifest protocol, name or kinds".into(),
        ));
    }
    let mut names = std::collections::BTreeSet::new();
    for builtin in crate::kinds::BUILT_IN {
        for alias in kind_aliases(builtin) {
            names.insert(alias);
        }
    }
    for kind in &manifest.kinds {
        if kind.poll_ms < 1000
            || kind.kind.len() > 64
            || !kind.kind.starts_with(|c: char| c.is_ascii_uppercase())
            || !kind.kind.chars().all(|c| c.is_ascii_alphanumeric())
            || kind_aliases(&kind.kind)
                .iter()
                .any(|alias| names.contains(alias))
        {
            return Err(StoreError::Invalid(
                "invalid or duplicate plugin kind".into(),
            ));
        }
        names.extend(kind_aliases(&kind.kind));
    }
    Ok(())
}

/// Every spelling the CLI accepts, including a simple plural.
pub fn kind_aliases(kind: &str) -> [String; 2] {
    let lower = kind.to_ascii_lowercase();
    [lower.clone(), format!("{lower}s")]
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Plan {
    pub status: Option<Value>,
    pub not_before: Option<Millis>,
    #[serde(default)]
    pub creates: Vec<Object>,
    #[serde(default)]
    pub patches: Vec<Patch>,
    #[serde(default)]
    pub appends: Vec<Append>,
    #[serde(default)]
    pub amends: Vec<Amend>,
    #[serde(default)]
    pub declares: Vec<Declare>,
    #[serde(default)]
    pub deletes: Vec<Target>,
    #[serde(default)]
    pub effects: Vec<Effect>,
    #[serde(default)]
    pub delete: bool,
    #[serde(default)]
    pub remove_finalizer: bool,
}
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Target {
    pub kind: String,
    pub name: String,
    pub uid: String,
}
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Patch {
    pub kind: String,
    pub name: String,
    pub uid: String,
    pub spec: Value,
}
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Append {
    pub task: String,
    pub uid: String,
    pub key: String,
    pub text: String,
    /// The declared Questions the entry answers, recorded in the Task's
    /// `status.answered` by the store, first answer wins.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub answers: Vec<String>,
}
/// `fleet amend` on a Task's behalf, from `plugin/<name>`. `if_generation`
/// is required: a repeated or recovered pass cannot append twice.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Amend {
    pub task: String,
    pub uid: String,
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub outputs: BTreeMap<String, crate::result::OutputDecl>,
    pub if_generation: u64,
}
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Declare {
    pub task: String,
    pub uid: String,
    pub change_request: String,
}
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Effect {
    pub key: String,
    pub request: Value,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "camelCase", deny_unknown_fields)]
pub enum Probe {
    Object {
        id: String,
        kind: String,
        name: String,
    },
    List {
        id: String,
        kind: String,
        #[serde(default)]
        labels: BTreeMap<String, String>,
    },
    Plugin {
        id: String,
        request: Value,
    },
}
impl Probe {
    pub fn id(&self) -> &str {
        match self {
            Self::Object { id, .. } | Self::List { id, .. } | Self::Plugin { id, .. } => id,
        }
    }
}

/// Ownership lookup for stores; the manager uses the gate's admitted writer.
pub fn writer_for_store(name: &str) -> crate::FieldManager {
    crate::FieldManager::Plugin(name.into())
}

/// Typed verb executed atomically by a store while holding the manager lease.
#[derive(Debug, Clone)]
pub enum Mutation {
    Create(Object),
    Patch(Patch),
    Delete(Target),
    Append(Append),
    Amend(Amend),
    Declare {
        task: String,
        uid: String,
        item: Value,
    },
}
