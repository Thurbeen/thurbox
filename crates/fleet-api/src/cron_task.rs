use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::condition::Condition;
use crate::meta::Millis;
use crate::plan::{Intent, Receipt};

pub const KIND: &str = "CronTask";

pub const NAME_LABEL: &str = "crontask.fleet.dev/name";
pub const UID_LABEL: &str = "crontask.fleet.dev/uid";

pub const DEFAULT_HISTORY_LIMIT: u32 = 3;
pub const MAX_HISTORY_LIMIT: u32 = 100;
pub const EFFECTS_KEPT: usize = 20;
pub const MAX_CREATE_ATTEMPTS: u32 = 3;

pub fn is_controller_label(label: &str) -> bool {
    label == NAME_LABEL || label == UID_LABEL
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CronTaskSpec {
    pub schedule: String,
    #[serde(default)]
    pub concurrency_policy: ConcurrencyPolicy,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub starting_deadline_seconds: Option<u64>,
    #[serde(default)]
    pub suspend: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub history_limit: Option<u32>,
    pub template: TaskTemplate,
}

impl CronTaskSpec {
    pub fn history_limit(&self) -> usize {
        self.history_limit.unwrap_or(DEFAULT_HISTORY_LIMIT) as usize
    }

    pub fn starting_deadline_ms(&self) -> Option<Millis> {
        self.starting_deadline_seconds
            .map(|s| s.saturating_mul(1000))
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ConcurrencyPolicy {
    Allow,
    #[default]
    Forbid,
    Replace,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TaskTemplate {
    #[serde(default, skip_serializing_if = "TemplateMeta::is_empty")]
    pub metadata: TemplateMeta,
    pub spec: Value,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TemplateMeta {
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub labels: BTreeMap<String, String>,
}

impl TemplateMeta {
    fn is_empty(&self) -> bool {
        self.labels.is_empty()
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Phase {
    #[default]
    Scheduled,
    Suspended,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EffectRecord {
    pub intent: Intent,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub receipt: Option<Receipt>,
    #[serde(default)]
    pub attempts: u32,
    pub task: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scheduled: Option<Millis>,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub unknowns: u32,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub running: bool,
}

fn is_zero(n: &u32) -> bool {
    *n == 0
}

impl EffectRecord {
    pub fn doubt(&self) -> u32 {
        let latest = self.attempts > 0
            && matches!(
                self.receipt.as_ref().map(|r| &r.outcome),
                None | Some(crate::Observation::Unknown { .. })
            );
        self.unknowns + u32::from(latest)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CronTaskStatus {
    #[serde(default)]
    pub phase: Phase,
    #[serde(default)]
    pub observed_generation: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_schedule_time: Option<Millis>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_considered: Option<Millis>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_schedule_time: Option<Millis>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub not_before: Option<Millis>,
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub active: BTreeSet<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub effects: BTreeMap<String, EffectRecord>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub conditions: Vec<Condition>,
}

#[derive(Debug, Clone)]
pub struct Schedule(croner::Cron);

impl Schedule {
    pub fn parse(text: &str) -> Result<Schedule, String> {
        croner::parser::CronParser::builder()
            .seconds(croner::parser::Seconds::Disallowed)
            .build()
            .parse(text.trim())
            .map(Schedule)
            .map_err(|e| format!("schedule {text:?}: {e}"))
    }

    pub fn latest_at_or_before(&self, t: Millis) -> Option<Millis> {
        let at = to_utc(t)?;
        self.0
            .find_previous_occurrence(&at, true)
            .ok()
            .and_then(|d| from_utc(&d))
    }

    pub fn first_after(&self, t: Millis) -> Option<Millis> {
        let at = to_utc(t)?;
        self.0
            .find_next_occurrence(&at, false)
            .ok()
            .and_then(|d| from_utc(&d))
    }
}

fn to_utc(t: Millis) -> Option<chrono::DateTime<chrono::Utc>> {
    chrono::DateTime::from_timestamp_millis(i64::try_from(t).ok()?)
}

fn from_utc(d: &chrono::DateTime<chrono::Utc>) -> Option<Millis> {
    u64::try_from(d.timestamp_millis()).ok()
}

pub fn task_name(cron_task: &str, at: Millis) -> String {
    format!("{cron_task}-{}", at / 60_000)
}

pub fn create_key(at: Millis) -> String {
    format!("create/{}", at / 60_000)
}

pub fn retire_key(task_uid: &str) -> String {
    format!("retire/{task_uid}")
}

#[cfg(test)]
mod tests {
    use super::*;

    const MONDAY_3AM: Millis = 1_794_798_000_000;

    #[test]
    fn a_five_field_schedule_answers_in_utc() {
        let s = Schedule::parse("0 3 * * 1-5").expect("parses");
        assert_eq!(s.latest_at_or_before(MONDAY_3AM), Some(MONDAY_3AM));
        assert_eq!(
            s.latest_at_or_before(MONDAY_3AM - 1),
            Some(MONDAY_3AM - 3 * 86_400_000),
            "the Friday before"
        );
        assert_eq!(s.first_after(MONDAY_3AM), Some(MONDAY_3AM + 86_400_000));
    }

    #[test]
    fn macros_parse_and_seconds_do_not() {
        let daily = Schedule::parse("@daily").expect("parses");
        assert_eq!(
            daily.first_after(MONDAY_3AM),
            Some(MONDAY_3AM + 21 * 3_600_000)
        );
        assert!(Schedule::parse("0 0 3 * * 1").is_err(), "six fields");
        assert!(Schedule::parse("not a schedule").is_err());
        assert!(Schedule::parse("61 * * * *").is_err());
    }

    #[test]
    fn a_scheduled_time_names_one_task_and_one_key() {
        assert_eq!(task_name("nightly", MONDAY_3AM), "nightly-29913300");
        assert_eq!(create_key(MONDAY_3AM + 59_999), "create/29913300");
    }
}
