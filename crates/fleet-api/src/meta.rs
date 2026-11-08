use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The one apiVersion this build reads and writes (P9).
pub const API_VERSION: &str = "fleet/v1alpha1";

/// Milliseconds since the Unix epoch. Every time in a record is one of these,
/// read from the clock by an adapter and passed in, never read in pure code.
pub type Millis = u64;

/// A stored resource: the Kubernetes shape, spec and status in one file (Q6).
///
/// The store is kind-agnostic and keeps `spec` and `status` as JSON values; the
/// typed views (`TaskSpec`, `TaskStatus`, ...) convert at the edges.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Object {
    pub api_version: String,
    pub kind: String,
    pub metadata: ObjectMeta,
    #[serde(default)]
    pub spec: Value,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub status: Value,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ObjectMeta {
    /// Empty only in a manifest of a kind that derives its name (a
    /// ChangeRequest, from its change); the store never holds an empty one.
    #[serde(default)]
    pub name: String,
    /// Assigned by the store on create; unique for the life of the store.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub uid: String,
    /// Bumped by the store whenever spec changes; status says which one it saw.
    #[serde(default)]
    pub generation: u64,
    /// Store-wide, strictly increasing; every write carries the one it read.
    #[serde(default)]
    pub resource_version: u64,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub labels: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub owner_references: Vec<OwnerReference>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub finalizers: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub creation_timestamp: Option<Millis>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deletion_timestamp: Option<Millis>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OwnerReference {
    pub kind: String,
    pub name: String,
    pub uid: String,
    #[serde(default)]
    pub controller: bool,
}

impl Object {
    pub fn is_deleting(&self) -> bool {
        self.metadata.deletion_timestamp.is_some()
    }

    /// `kind/name`, the key every message and workqueue entry uses.
    pub fn key(&self) -> String {
        format!("{}/{}", self.kind, self.metadata.name)
    }
}
