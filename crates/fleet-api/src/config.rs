//! Passive execution defaults and the immutable resolved launch snapshot.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const ENV_PROVIDER: &str = "EnvProvider";
pub const SOURCE_CAP: usize = 64 * 1024;
pub const TASK_CONFIG: &str = "TaskConfig";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ConfigRef {
    pub name: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TaskConfigSpec {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub harness: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub env_from: Vec<ConfigRef>,
    /// Pause a Task's session once its agent was idle this long. Absent
    /// means never.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pause_after_ms: Option<crate::Millis>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ConfigSource {
    pub kind: String,
    pub name: String,
    pub uid: String,
    pub generation: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Execution {
    pub harness: String,
    pub host: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sources: Vec<ConfigSource>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pause_after_ms: Option<crate::Millis>,
}

/// Later sources override earlier spelling variants consistently on every host.
pub fn merge_env(target: &mut BTreeMap<String, String>, source: &BTreeMap<String, String>) {
    for (key, value) in source {
        target.retain(|k, _| !k.eq_ignore_ascii_case(key));
        target.insert(key.clone(), value.clone());
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EnvProviderSpec {
    #[serde(default)]
    pub data: BTreeMap<String, String>,
    #[serde(default)]
    pub sources: Vec<EnvSource>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase", deny_unknown_fields)]
pub enum EnvSource {
    File {
        path: String,
    },
    Command {
        program: String,
        #[serde(default)]
        args: Vec<String>,
        cwd: String,
        #[serde(rename = "timeoutMs")]
        timeout_ms: u64,
    },
}

/// Public identity only; the resolved map belongs to the private store.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExecutionIdentity {
    pub harness: String,
    pub host: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    pub sources: Vec<ConfigSource>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pause_after_ms: Option<crate::Millis>,
}

impl Execution {
    pub fn identity(&self) -> ExecutionIdentity {
        ExecutionIdentity {
            harness: self.harness.clone(),
            host: self.host.clone(),
            model: self.model.clone(),
            sources: self.sources.clone(),
            pause_after_ms: self.pause_after_ms,
        }
    }
}
