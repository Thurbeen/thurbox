//! The worker's answer (Q5, R6): validated when written, never trusted as the
//! outcome of anything beyond its own Task.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Outcome {
    Shipped,
    Stuck,
    Failed,
    NotApplicable,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TaskResult {
    pub outcome: Outcome,
    /// The brief generation this answer is for (generation fencing).
    pub generation: u64,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub summary: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifacts: Vec<String>,
    /// Structured outputs a parent can read, checked against `spec.outputs`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub outputs: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum OutputType {
    String,
    Integer,
    Number,
    Boolean,
    Object,
    Array,
}

/// One declared output.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OutputDecl {
    #[serde(rename = "type")]
    pub type_: OutputType,
    #[serde(default)]
    pub required: bool,
}

/// Every reason a result fails its Task's declared outputs, in name order.
pub fn validate_outputs(
    declared: &BTreeMap<String, OutputDecl>,
    outputs: &BTreeMap<String, Value>,
) -> Vec<String> {
    let mut problems = Vec::new();
    for (name, decl) in declared {
        match outputs.get(name) {
            None if decl.required => problems.push(format!("output {name:?} is required")),
            None => {}
            Some(v) if !matches_type(decl.type_, v) => problems.push(format!(
                "output {name:?} must be {:?}, got {}",
                decl.type_,
                type_name(v)
            )),
            Some(_) => {}
        }
    }
    for name in outputs.keys() {
        if !declared.contains_key(name) {
            problems.push(format!("output {name:?} is not declared in spec.outputs"));
        }
    }
    problems
}

fn matches_type(t: OutputType, v: &Value) -> bool {
    match t {
        OutputType::String => v.is_string(),
        OutputType::Integer => v.is_i64() || v.is_u64(),
        OutputType::Number => v.is_number(),
        OutputType::Boolean => v.is_boolean(),
        OutputType::Object => v.is_object(),
        OutputType::Array => v.is_array(),
    }
}

fn type_name(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}
