use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::condition::Condition;
use crate::conversation::EffectRecord;
use crate::meta::Millis;
use crate::plan::effect_key;
use crate::store::FieldManager;

pub const PARENT_LABEL: &str = "gateway.fleet.dev/parent";
pub const PARENT_UID_LABEL: &str = "gateway.fleet.dev/parent-uid";
pub const ROUTE_LABEL: &str = "gateway.fleet.dev/route";

pub const DECLARED_QUESTION: &str = "Question";
pub const DECLARED_COMMENT: &str = "Comment";
pub const DECLARED_DRAFT: &str = "Draft";

pub const ENTRY_CAP: usize = 64 * 1024;

pub const MAX_CHILDREN: usize = 200;

pub const POSTS_CAP: usize = 100;

pub const GATEWAY_SOURCE_PREFIX: &str = "gateway/";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TieState {
    Opening,
    Opened,
    Waiting,
    Tied,
    Abandoned,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ConversationTie {
    pub name: String,
    pub uid: String,
    pub state: TieState,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ChildEntry {
    pub uid: String,
    #[serde(default)]
    pub seen: u64,
    #[serde(default)]
    pub declared: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<u64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub answered: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PostedQuestion {
    pub conversation: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub message: String,
    pub at: Millis,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GatewayStatus {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conversation: Option<ConversationTie>,
    #[serde(default)]
    pub seen: u64,
    #[serde(default)]
    pub cursor: u64,
    #[serde(default)]
    pub declared: u64,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub logged: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<u64>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub changes: BTreeMap<String, u64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ready: Vec<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub announced: BTreeMap<String, Millis>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub children: BTreeMap<String, ChildEntry>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub posts: BTreeMap<String, EffectRecord>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub posted: Vec<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub questions: BTreeMap<String, PostedQuestion>,
    #[serde(default)]
    pub gaps: u64,
    #[serde(default, skip_serializing_if = "ReactionStatus::is_empty")]
    pub reactions: ReactionStatus,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub conditions: Vec<Condition>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mark {
    Queued,
    Working,
    Replied,
}

impl Mark {
    pub fn as_str(self) -> &'static str {
        match self {
            Mark::Queued => "queued",
            Mark::Working => "working",
            Mark::Replied => "replied",
        }
    }

    fn parse(word: &str) -> Option<Mark> {
        match word {
            "queued" => Some(Mark::Queued),
            "working" => Some(Mark::Working),
            "replied" => Some(Mark::Replied),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReactionStatus {
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub marks: BTreeMap<String, Mark>,
    #[serde(default, skip_serializing_if = "std::collections::BTreeSet::is_empty")]
    pub replies: std::collections::BTreeSet<String>,
}

impl ReactionStatus {
    pub fn is_empty(&self) -> bool {
        self.marks.is_empty() && self.replies.is_empty()
    }
}

pub fn react_suffix(message: &str, mark: Mark, on: bool) -> String {
    let on = if on { "on" } else { "off" };
    format!("react/{message}/{}/{on}", mark.as_str())
}

pub fn react_parse(rest: &str) -> Option<(String, Mark, bool)> {
    let mut parts = rest.rsplitn(3, '/');
    let on = match parts.next()? {
        "on" => true,
        "off" => false,
        _ => return None,
    };
    let mark = Mark::parse(parts.next()?)?;
    let message = parts.next().filter(|m| !m.is_empty())?;
    Some((message.to_string(), mark, on))
}

pub fn react_key(name: &str, uid: &str, message: &str, mark: Mark, on: bool) -> String {
    post_key(name, uid, &react_suffix(message, mark, on))
}

pub fn react_of(name: &str, uid: &str, key: &str) -> Option<(String, Mark, bool)> {
    react_parse(key.strip_prefix(&post_key(name, uid, "react/"))?)
}

impl GatewayStatus {
    pub fn is_empty(&self) -> bool {
        *self == GatewayStatus::default()
    }

    pub fn is_tied(&self) -> bool {
        self.conversation
            .as_ref()
            .is_some_and(|c| c.state == TieState::Tied)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Answer {
    pub key: String,
    pub from: String,
    pub at: Millis,
}

pub fn relay_key(name: &str, uid: &str, conversation_uid: &str, revision: u64) -> String {
    let base = effect_key(crate::task::KIND, name, uid, "gateway", None);
    format!("{base}/{conversation_uid}/{revision}")
}

pub fn child_key(name: &str, uid: &str, child_uid: &str, item: &str) -> String {
    let base = effect_key(crate::task::KIND, name, uid, "gateway", None);
    format!("{base}/child/{child_uid}/{item}")
}

pub fn post_key(name: &str, uid: &str, item: &str) -> String {
    let base = effect_key(crate::task::KIND, name, uid, "gateway", None);
    format!("{base}/{item}")
}

pub fn declared_id(item: &Value) -> Option<&str> {
    item.get("id")
        .and_then(Value::as_str)
        .filter(|id| !id.trim().is_empty())
}

pub fn gateway_reads(item: &Value) -> bool {
    match item.get("kind").and_then(Value::as_str) {
        Some(DECLARED_QUESTION | DECLARED_COMMENT | DECLARED_DRAFT) => declared_id(item).is_some(),
        Some(crate::follow::DECLARED_CHANGE_REQUEST) => {
            item.get("name").and_then(Value::as_str).is_some()
        }
        _ => false,
    }
}

pub fn check_answers(declared: &[Value], answers: &[String]) -> Result<(), String> {
    for id in answers {
        if id.trim().is_empty() {
            return Err("answers: an empty id names no Question".into());
        }
        let declared = declared.iter().any(|item| {
            item.get("kind").and_then(Value::as_str) == Some(DECLARED_QUESTION)
                && declared_id(item) == Some(id.as_str())
        });
        if !declared {
            return Err(format!("answers: no declared Question has the id {id:?}"));
        }
    }
    Ok(())
}

pub fn parent_of(labels: &BTreeMap<String, String>) -> Result<Option<(String, String)>, String> {
    match (labels.get(PARENT_LABEL), labels.get(PARENT_UID_LABEL)) {
        (None, None) => Ok(None),
        (Some(_), None) => Err(format!(
            "the label {PARENT_LABEL} needs {PARENT_UID_LABEL} beside it"
        )),
        (None, Some(_)) => Err(format!(
            "the label {PARENT_UID_LABEL} needs {PARENT_LABEL} beside it"
        )),
        (Some(name), Some(uid)) => {
            crate::validate::validate_name(name)
                .map_err(|e| format!("the label {PARENT_LABEL}: {e}"))?;
            if uid.trim().is_empty() {
                return Err(format!("the label {PARENT_UID_LABEL} is empty"));
            }
            Ok(Some((name.clone(), uid.clone())))
        }
    }
}

pub fn check_parent_unchanged(
    old: &BTreeMap<String, String>,
    new: &BTreeMap<String, String>,
) -> Result<(), String> {
    for label in [PARENT_LABEL, PARENT_UID_LABEL] {
        if old.get(label) != new.get(label) {
            return Err(format!(
                "the label {label} is set when a Task is created and never changes"
            ));
        }
    }
    Ok(())
}

pub fn may_name_parent(
    by: &FieldManager,
    plugin_access: Option<crate::plugin::TaskAccess>,
    parent: &crate::meta::Object,
) -> bool {
    match by {
        FieldManager::Apply => true,
        FieldManager::Plugin(name) => match plugin_access {
            Some(crate::plugin::TaskAccess::Any) => true,
            _ => parent.metadata.labels.get(crate::plugin::CREATOR) == Some(name),
        },
        _ => false,
    }
}

fn is_zero(n: &u64) -> bool {
    *n == 0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::meta::{Object, ObjectMeta};
    use crate::plugin::TaskAccess;
    use serde_json::json;

    #[test]
    fn the_keys_name_the_task_the_mirror_and_the_item() {
        assert_eq!(relay_key("t", "u7", "c3", 9), "task/t/u7/gateway/c3/9");
        assert_eq!(
            child_key("t", "u7", "k1", "result/2"),
            "task/t/u7/gateway/child/k1/result/2"
        );
        assert_eq!(
            child_key("t", "u7", "k1", "0"),
            "task/t/u7/gateway/child/k1/0"
        );
        assert_eq!(
            post_key("t", "u7", "change/cr1/4"),
            "task/t/u7/gateway/change/cr1/4"
        );
    }

    #[test]
    fn an_empty_status_is_not_stored_and_the_shape_is_the_pages() {
        let st = GatewayStatus::default();
        assert!(st.is_empty());
        assert!(!st.is_tied());
        assert_eq!(
            serde_json::to_value(&st).unwrap(),
            json!({"seen": 0, "cursor": 0, "declared": 0, "gaps": 0})
        );
        let tied = GatewayStatus {
            conversation: Some(ConversationTie {
                name: "conv-1".into(),
                uid: "c1".into(),
                state: TieState::Tied,
            }),
            ..GatewayStatus::default()
        };
        assert!(tied.is_tied() && !tied.is_empty());
        assert_eq!(
            serde_json::to_value(&tied).unwrap()["conversation"],
            json!({"name": "conv-1", "uid": "c1", "state": "tied"})
        );
        assert_eq!(
            serde_json::to_value(TieState::Abandoned).unwrap(),
            "abandoned"
        );
        let a: Answer =
            serde_json::from_value(json!({"key": "k", "from": "cli/a", "at": 1})).unwrap();
        assert_eq!(a.key, "k");
    }

    #[test]
    fn answers_must_name_declared_questions() {
        let declared = vec![
            json!({"kind": "Question", "id": "q1", "text": "?"}),
            json!({"kind": "Comment", "id": "c1", "text": "."}),
            json!({"kind": "Question", "text": "no id"}),
        ];
        assert!(check_answers(&declared, &["q1".into()]).is_ok());
        assert!(check_answers(&declared, &[]).is_ok());
        assert!(
            check_answers(&declared, &["c1".into()]).is_err(),
            "a Comment"
        );
        assert!(check_answers(&declared, &["q2".into()]).is_err());
        assert!(check_answers(&declared, &[" ".into()]).is_err());
        assert!(gateway_reads(&declared[0]) && gateway_reads(&declared[1]));
        assert!(!gateway_reads(&declared[2]), "an item without an id");
        assert!(gateway_reads(
            &json!({"kind": "ChangeRequest", "url": "u", "name": "cr-1"})
        ));
        assert!(!gateway_reads(&json!({"kind": "Note", "text": "x"})));
        assert!(gateway_reads(
            &json!({"kind": "Draft", "id": "d1", "title": "t", "url": "u"})
        ));
    }

    #[test]
    fn the_parent_labels_come_together_and_never_change() {
        let mut labels = BTreeMap::new();
        assert_eq!(parent_of(&labels), Ok(None));
        labels.insert(PARENT_LABEL.to_string(), "p".to_string());
        assert!(parent_of(&labels).unwrap_err().contains(PARENT_UID_LABEL));
        labels.insert(PARENT_UID_LABEL.to_string(), "u1".to_string());
        assert_eq!(
            parent_of(&labels),
            Ok(Some(("p".to_string(), "u1".to_string())))
        );
        let mut bad = labels.clone();
        bad.insert(PARENT_LABEL.to_string(), "Not A Name".into());
        assert!(parent_of(&bad).is_err());
        let mut only_uid = BTreeMap::new();
        only_uid.insert(PARENT_UID_LABEL.to_string(), "u1".to_string());
        assert!(parent_of(&only_uid).is_err());
        let mut empty_uid = labels.clone();
        empty_uid.insert(PARENT_UID_LABEL.to_string(), " ".into());
        assert!(parent_of(&empty_uid).is_err());
        assert!(check_parent_unchanged(&labels, &labels).is_ok());
        assert!(check_parent_unchanged(&labels, &BTreeMap::new()).is_err());
        let mut moved = labels.clone();
        moved.insert(PARENT_LABEL.to_string(), "q".into());
        assert!(check_parent_unchanged(&labels, &moved).is_err());
        let mut other = BTreeMap::new();
        other.insert("topic".to_string(), "x".to_string());
        assert!(check_parent_unchanged(&BTreeMap::new(), &other).is_ok());
    }

    #[test]
    fn only_a_writer_that_may_append_to_the_parents_inbox_may_name_it() {
        let parent = |creator: Option<&str>| Object {
            api_version: crate::meta::API_VERSION.into(),
            kind: crate::task::KIND.into(),
            metadata: ObjectMeta {
                name: "p".into(),
                labels: creator
                    .map(|c| BTreeMap::from([(crate::plugin::CREATOR.to_string(), c.to_string())]))
                    .unwrap_or_default(),
                ..ObjectMeta::default()
            },
            spec: json!({}),
            status: Value::Null,
        };
        let issues = FieldManager::Plugin("issues".into());
        assert!(may_name_parent(&FieldManager::Apply, None, &parent(None)));
        assert!(may_name_parent(
            &issues,
            Some(TaskAccess::Any),
            &parent(None)
        ));
        assert!(!may_name_parent(
            &issues,
            Some(TaskAccess::Own),
            &parent(None)
        ));
        assert!(!may_name_parent(&issues, None, &parent(Some("other"))));
        assert!(may_name_parent(
            &issues,
            Some(TaskAccess::Own),
            &parent(Some("issues"))
        ));
        assert!(!may_name_parent(&FieldManager::Cli, None, &parent(None)));
        assert!(!may_name_parent(
            &FieldManager::Listener("a".into()),
            None,
            &parent(None)
        ));
    }
}
