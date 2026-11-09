use serde::{Deserialize, Serialize};

use crate::meta::Millis;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ConditionStatus {
    True,
    False,
    Unknown,
}

/// metav1.Condition's shape. An absent condition reads as Unknown (P1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Condition {
    #[serde(rename = "type")]
    pub type_: String,
    pub status: ConditionStatus,
    /// CamelCase, required.
    pub reason: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub message: String,
    pub last_transition_time: Millis,
    pub observed_generation: u64,
}

/// Set a condition, keeping `last_transition_time` when the status did not
/// change, so a resync that re-derives the same fact does not look like news.
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

/// Keep each condition's `lastTransitionTime` from `before` (the status as
/// read) when its status is the same there. A controller may set a condition
/// more than once in one pass; only a change against what was stored is a
/// transition, so a pass that ends where it began writes nothing.
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

/// True only when the condition exists and is True; absence is not True.
pub fn is_true(conditions: &[Condition], type_: &str) -> bool {
    condition(conditions, type_).is_some_and(|c| c.status == ConditionStatus::True)
}
