use serde::{Deserialize, Serialize};

use crate::meta::Millis;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ConditionStatus {
    True,
    False,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Condition {
    #[serde(rename = "type")]
    pub type_: String,
    pub status: ConditionStatus,
    pub reason: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub message: String,
    pub last_transition_time: Millis,
    pub observed_generation: u64,
}

pub fn set_condition(
    conditions: &mut Vec<Condition>,
    type_: &str,
    status: ConditionStatus,
    reason: &str,
    message: impl Into<String>,
    generation: u64,
    now: Millis,
) {
    let message = message.into();
    if let Some(c) = conditions.iter_mut().find(|c| c.type_ == type_) {
        if c.status != status {
            c.last_transition_time = now;
        }
        c.status = status;
        c.reason = reason.to_string();
        c.message = message;
        c.observed_generation = generation;
        return;
    }
    conditions.push(Condition {
        type_: type_.to_string(),
        status,
        reason: reason.to_string(),
        message,
        last_transition_time: now,
        observed_generation: generation,
    });
}

pub fn settle_transitions(conditions: &mut [Condition], before: &[Condition]) {
    for c in conditions {
        if let Some(b) = before.iter().find(|b| b.type_ == c.type_)
            && b.status == c.status
        {
            c.last_transition_time = b.last_transition_time;
        }
    }
}

pub fn condition<'a>(conditions: &'a [Condition], type_: &str) -> Option<&'a Condition> {
    conditions.iter().find(|c| c.type_ == type_)
}

pub fn is_true(conditions: &[Condition], type_: &str) -> bool {
    condition(conditions, type_).is_some_and(|c| c.status == ConditionStatus::True)
}
