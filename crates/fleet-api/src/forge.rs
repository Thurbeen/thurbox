use serde::{Deserialize, Serialize};

use crate::meta::Millis;
use crate::observation::Observation;
use crate::plan::EffectOutcome;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ChangeRef {
    pub kind: String,
    pub host: String,
    pub project: String,
    pub id: String,
}

impl ChangeRef {
    pub fn canonical(&self) -> String {
        format!("{}/{}/{}/{}", self.kind, self.host, self.project, self.id).to_lowercase()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ChangeState {
    Draft,
    Open,
    Closed,
    Merged,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Checks {
    None,
    Pending,
    Passed,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FailingCheck {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HeadSeen {
    pub id: String,
    pub checks: Checks,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub failing: Vec<FailingCheck>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ReviewDecision {
    Approved,
    ChangesRequested,
    None,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Mergeable {
    pub value: Option<bool>,
    pub reason: Option<String>,
}

pub const CONFLICTS: &str = "conflicts";

impl Mergeable {
    pub fn conflicts(&self) -> Option<bool> {
        self.value
            .map(|v| !v && self.reason.as_deref() == Some(CONFLICTS))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Threads {
    pub unresolved: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ReviewState {
    Approved,
    ChangesRequested,
    Commented,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Review {
    pub id: String,
    pub author: String,
    #[serde(default)]
    pub bot: bool,
    pub state: ReviewState,
    pub head: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum NoteKind {
    Comment,
    Review,
    Inline,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Note {
    pub id: String,
    pub kind: NoteKind,
    pub author: String,
    #[serde(default)]
    pub bot: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<u64>,
    pub at: Millis,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub edited_at: Option<Millis>,
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub body: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ChangeSnapshot {
    pub state: ChangeState,
    pub head: HeadSeen,
    pub review_decision: ReviewDecision,
    pub mergeable: Mergeable,
    pub threads: Threads,
    #[serde(default)]
    pub reviews: Vec<Review>,
    #[serde(default)]
    pub notes: Vec<Note>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Uncertainty {
    Timeout,
    Incomplete,
    Throttled,
    Unreadable,
    NotLoggedIn,
    Refused,
    Unavailable,
}

impl Uncertainty {
    pub fn reason(self) -> &'static str {
        match self {
            Uncertainty::Timeout => "Timeout",
            Uncertainty::Incomplete => "Incomplete",
            Uncertainty::Throttled => "Throttled",
            Uncertainty::Unreadable => "Unreadable",
            Uncertainty::NotLoggedIn => "NotLoggedIn",
            Uncertainty::Refused => "Refused",
            Uncertainty::Unavailable => "CliUnavailable",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChangeObservation {
    Known(Option<ChangeSnapshot>),
    Unknown {
        kind: Uncertainty,
        why: String,
        retry_after: Option<Millis>,
    },
}

impl ChangeObservation {
    pub fn unknown(kind: Uncertainty, why: impl Into<String>) -> ChangeObservation {
        ChangeObservation::Unknown {
            kind,
            why: why.into(),
            retry_after: None,
        }
    }

    pub fn is_unknown(&self) -> bool {
        matches!(self, ChangeObservation::Unknown { .. })
    }

    pub fn why(&self) -> Option<&str> {
        match self {
            ChangeObservation::Known(_) => None,
            ChangeObservation::Unknown { why, .. } => Some(why),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Expected {
    pub head: String,
    pub state: ChangeState,
}

pub trait Forge {
    fn operation_bound(&self) -> Millis;
    fn parse(&self, url: &str) -> Observation<ChangeRef>;
    fn whoami(&self, host: &str) -> Observation<String>;
    fn change(&self, change: &ChangeRef) -> ChangeObservation;
    fn mark_ready(
        &self,
        change: &ChangeRef,
        expect: &Expected,
        intent_key: &str,
    ) -> Observation<EffectOutcome>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_canonical_ref_is_lowercase_and_slash_joined() {
        let r = ChangeRef {
            kind: "github".into(),
            host: "GitHub.com".into(),
            project: "Acme/Widgets".into(),
            id: "7".into(),
        };
        assert_eq!(r.canonical(), "github/github.com/acme/widgets/7");
    }

    #[test]
    fn the_vocabulary_serialises_in_the_resource_pages_words() {
        assert_eq!(
            serde_json::to_value(ReviewDecision::ChangesRequested).unwrap(),
            "changes-requested"
        );
        assert_eq!(serde_json::to_value(ChangeState::Draft).unwrap(), "draft");
        assert_eq!(serde_json::to_value(Checks::None).unwrap(), "none");
        assert_eq!(serde_json::to_value(NoteKind::Inline).unwrap(), "inline");
    }
}
