use std::collections::BTreeMap;

use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;

use crate::condition::Condition;
use crate::meta::Millis;
use crate::plan::{Intent, Receipt, effect_key};

pub const DECLARED_CHANGE_REQUEST: &str = "ChangeRequest";

pub const WORKER_MARK_PREFIX: &str = "<!-- fleet:worker ";
pub const WORKER_MARK_SUFFIX: &str = " -->";

pub fn worker_mark(uid: &str) -> String {
    format!("{WORKER_MARK_PREFIX}{uid}{WORKER_MARK_SUFFIX}")
}

pub fn marked_by(body: &str) -> Option<&str> {
    let start = body.find(WORKER_MARK_PREFIX)? + WORKER_MARK_PREFIX.len();
    let rest = &body[start..];
    let end = rest.find(WORKER_MARK_SUFFIX)?;
    let uid = rest[..end].trim();
    (!uid.is_empty() && !uid.contains(char::is_whitespace)).then_some(uid)
}

pub const DEFAULT_NO_CHECK_GRACE_MS: Millis = 300_000;

pub const PREDICATES: &[&str] = &[
    "checksPassed",
    "botReviewsAddressed",
    "noUnresolvedThreads",
    "forgeMergeable",
];

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReadyPolicy {
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub checks_passed: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub bot_reviews_addressed: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub no_unresolved_threads: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub forge_mergeable: bool,
}

impl ReadyPolicy {
    pub fn needs_bots(&self) -> bool {
        self.bot_reviews_addressed
    }
}

pub fn shipped_policies() -> BTreeMap<String, ReadyPolicy> {
    let checks = ReadyPolicy {
        checks_passed: true,
        ..ReadyPolicy::default()
    };
    let checks_and_bots = ReadyPolicy {
        bot_reviews_addressed: true,
        ..checks
    };
    let strict = ReadyPolicy {
        no_unresolved_threads: true,
        ..checks_and_bots
    };
    BTreeMap::from([
        ("checks".to_string(), checks),
        ("checks-and-bots".to_string(), checks_and_bots),
        ("strict".to_string(), strict),
    ])
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForgeDefaults {
    pub relay_notes: bool,
    pub relay_checks: bool,
    pub relay_bots: bool,
    pub no_check_grace_ms: Millis,
    pub draft_policy: Option<String>,
    pub ready_policies: BTreeMap<String, ReadyPolicy>,
}

impl Default for ForgeDefaults {
    fn default() -> Self {
        ForgeDefaults {
            relay_notes: true,
            relay_checks: true,
            relay_bots: false,
            no_check_grace_ms: DEFAULT_NO_CHECK_GRACE_MS,
            draft_policy: None,
            ready_policies: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ForgeSpec {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub relay: Option<RelaySpec>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub draft: Option<DraftSpec>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RelaySpec {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checks: Option<bool>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DraftSpec {
    #[serde(
        default,
        deserialize_with = "explicit_null",
        skip_serializing_if = "Option::is_none"
    )]
    pub policy: Option<Option<String>>,
}

fn explicit_null<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Option<String>>, D::Error> {
    Option::<String>::deserialize(d).map(Some)
}

impl ForgeSpec {
    pub fn relay_notes(&self, defaults: &ForgeDefaults) -> bool {
        self.relay
            .as_ref()
            .and_then(|r| r.notes)
            .unwrap_or(defaults.relay_notes)
    }

    pub fn relay_checks(&self, defaults: &ForgeDefaults) -> bool {
        self.relay
            .as_ref()
            .and_then(|r| r.checks)
            .unwrap_or(defaults.relay_checks)
    }

    pub fn draft_policy(&self, defaults: &ForgeDefaults) -> Option<String> {
        match self.draft.as_ref().and_then(|d| d.policy.as_ref()) {
            None => defaults.draft_policy.clone(),
            Some(None) => None,
            Some(Some(name)) => Some(name.clone()),
        }
    }

    pub fn named_policy(&self) -> Option<&str> {
        match self.draft.as_ref().and_then(|d| d.policy.as_ref()) {
            Some(Some(name)) => Some(name.as_str()),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Declaration {
    pub url: Option<String>,
    pub name: String,
}

pub fn validate_declared(items: &[Value]) -> Result<(), String> {
    for (i, item) in items.iter().enumerate() {
        validate_declaration(item).map_err(|why| format!("status.declared[{i}]: {why}"))?;
    }
    Ok(())
}

pub fn validate_declaration(item: &Value) -> Result<(), String> {
    let Some(map) = item.as_object() else {
        return Err("a declaration is a JSON object".into());
    };
    match map.get("kind").and_then(Value::as_str) {
        Some(k) if !k.trim().is_empty() => Ok(()),
        _ => Err("a declaration needs a string `kind`".into()),
    }
}

pub fn declared_changes(items: &[Value]) -> Vec<Declaration> {
    let mut out: Vec<Declaration> = Vec::new();
    for item in items {
        if item.get("kind").and_then(Value::as_str) != Some(DECLARED_CHANGE_REQUEST) {
            continue;
        }
        let Some(name) = item.get("name").and_then(Value::as_str) else {
            continue;
        };
        if out.iter().any(|d| d.name == name) {
            continue;
        }
        out.push(Declaration {
            url: item.get("url").and_then(Value::as_str).map(str::to_string),
            name: name.to_string(),
        });
    }
    out
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ForgeStatus {
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub changes: BTreeMap<String, ChangeEntry>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub conditions: Vec<Condition>,
}

impl ForgeStatus {
    pub fn is_empty(&self) -> bool {
        self.changes.is_empty() && self.conditions.is_empty()
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EntryState {
    #[default]
    Following,
    Settled,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ChangeEntry {
    pub uid: String,
    #[serde(default)]
    pub state: EntryState,
    #[serde(default)]
    pub cursor: u64,
    #[serde(default)]
    pub gaps: u64,
    #[serde(default)]
    pub mark_ready: MarkReady,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub not_before: Option<Millis>,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub answered_at: Millis,
}

fn is_zero(n: &u64) -> bool {
    *n == 0
}

impl ChangeEntry {
    pub fn joined(uid: &str, revision: u64) -> ChangeEntry {
        ChangeEntry {
            uid: uid.to_string(),
            state: EntryState::Following,
            cursor: revision,
            gaps: 0,
            mark_ready: MarkReady::default(),
            not_before: None,
            answered_at: 0,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MarkReadyState {
    #[default]
    None,
    Pending,
    Done,
    Failed,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MarkReady {
    #[serde(default)]
    pub state: MarkReadyState,
    #[serde(default)]
    pub head: Option<String>,
    #[serde(default)]
    pub attempts: u32,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub intents: BTreeMap<String, Intent>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub receipts: BTreeMap<String, Receipt>,
}

pub fn ready_key(name: &str, uid: &str, change_uid: &str, head: &str) -> String {
    let base = effect_key(crate::task::KIND, name, uid, "ready", None);
    format!("{base}/{change_uid}/{head}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn the_mark_names_a_task_uid() {
        let mark = worker_mark("u7");
        assert_eq!(mark, "<!-- fleet:worker u7 -->");
        assert_eq!(marked_by(&format!("done\n{mark}\n")), Some("u7"));
        assert_eq!(marked_by("<!-- fleet:worker -->"), None);
        assert_eq!(marked_by("no mark"), None);
    }

    #[test]
    fn an_explicit_null_policy_is_kept_apart_from_an_absent_one() {
        let absent: ForgeSpec = serde_json::from_value(json!({"draft": {}})).unwrap();
        assert_eq!(absent.draft.unwrap().policy, None);
        let disabled: ForgeSpec =
            serde_json::from_value(json!({"draft": {"policy": null}})).unwrap();
        assert_eq!(disabled.draft.as_ref().unwrap().policy, Some(None));
        assert_eq!(
            serde_json::to_value(&disabled).unwrap(),
            json!({"draft": {"policy": null}})
        );
        let named: ForgeSpec =
            serde_json::from_value(json!({"draft": {"policy": "checks"}})).unwrap();
        assert_eq!(
            named.draft.unwrap().policy,
            Some(Some("checks".to_string()))
        );
        assert!(
            serde_json::from_value::<ForgeSpec>(json!({"draft": {"ready": true}})).is_err(),
            "unknown fields are refused"
        );
    }

    #[test]
    fn a_policy_is_a_fixed_vocabulary_and_the_shipped_three_are_as_the_page_shows() {
        let strict: ReadyPolicy = serde_json::from_value(json!({
            "checksPassed": true, "botReviewsAddressed": true, "noUnresolvedThreads": true
        }))
        .unwrap();
        assert_eq!(shipped_policies()["strict"], strict);
        assert!(shipped_policies()["checks-and-bots"].needs_bots());
        assert!(!shipped_policies()["checks"].needs_bots());
        assert_eq!(
            serde_json::to_value(shipped_policies()["checks"]).unwrap(),
            json!({"checksPassed": true}),
            "only what is on is written"
        );
        let err = serde_json::from_value::<ReadyPolicy>(json!({"workerDone": false})).unwrap_err();
        assert!(err.to_string().contains("unknown field"), "{err}");
        assert!(serde_json::from_value::<ReadyPolicy>(json!({"approved": true})).is_err());
        assert_eq!(PREDICATES.len(), 4);
    }

    #[test]
    fn the_policy_a_task_selects_is_its_own_else_the_default_and_null_is_none() {
        let d = ForgeDefaults {
            draft_policy: Some("checks".into()),
            ..ForgeDefaults::default()
        };
        let absent: ForgeSpec = serde_json::from_value(json!({})).unwrap();
        assert_eq!(absent.draft_policy(&d), Some("checks".into()));
        assert_eq!(absent.named_policy(), None);
        let disabled: ForgeSpec =
            serde_json::from_value(json!({"draft": {"policy": null}})).unwrap();
        assert_eq!(disabled.draft_policy(&d), None);
        let named: ForgeSpec =
            serde_json::from_value(json!({"draft": {"policy": "strict"}})).unwrap();
        assert_eq!(named.draft_policy(&d), Some("strict".into()));
        assert_eq!(named.named_policy(), Some("strict"));
        assert_eq!(absent.draft_policy(&ForgeDefaults::default()), None);
        assert_eq!(ready_key("t", "u7", "c1", "h1"), "task/t/u7/ready/c1/h1");
    }

    #[test]
    fn relay_fields_inherit_the_defaults_field_by_field() {
        let spec: ForgeSpec = serde_json::from_value(json!({"relay": {"notes": false}})).unwrap();
        let d = ForgeDefaults::default();
        assert!(!spec.relay_notes(&d));
        assert!(spec.relay_checks(&d));
        assert!(!ForgeSpec::default().relay_notes(&ForgeDefaults {
            relay_notes: false,
            ..d
        }));
    }

    #[test]
    fn declarations_need_a_kind_and_only_named_change_requests_are_followed() {
        assert!(validate_declared(&[json!({"kind": "ChangeRequest", "url": "u"})]).is_ok());
        assert!(validate_declared(&[json!({"kind": "Anything", "x": 1})]).is_ok());
        assert!(validate_declared(&[json!({"url": "u"})]).is_err());
        assert!(validate_declared(&[json!("ChangeRequest")]).is_err());
        assert!(validate_declared(&[json!({"kind": ""})]).is_err());
        let followed = declared_changes(&[
            json!({"kind": "Other", "name": "x"}),
            json!({"kind": "ChangeRequest", "url": "u1", "name": "cr-1"}),
            json!({"kind": "ChangeRequest", "url": "u1", "name": "cr-1"}),
            json!({"kind": "ChangeRequest", "url": "u2"}),
            json!({"kind": "ChangeRequest", "name": "cr-2"}),
        ]);
        assert_eq!(
            followed,
            vec![
                Declaration {
                    url: Some("u1".into()),
                    name: "cr-1".into()
                },
                Declaration {
                    url: None,
                    name: "cr-2".into()
                },
            ]
        );
    }

    #[test]
    fn an_entry_serialises_in_the_interface_pages_shape() {
        let e = ChangeEntry::joined("c1", 7);
        assert_eq!(
            serde_json::to_value(&e).unwrap(),
            json!({"uid": "c1", "state": "following", "cursor": 7, "gaps": 0,
                   "markReady": {"state": "none", "head": null, "attempts": 0}})
        );
        let st: ForgeStatus = serde_json::from_value(json!({})).unwrap();
        assert!(st.is_empty());
    }
}
