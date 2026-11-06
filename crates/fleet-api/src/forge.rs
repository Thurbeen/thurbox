//! The forge port: how Fleet reads one change on a forge, a pull request, a
//! merge request or any equivalent. Every answer is Known or Unknown, and
//! Unknown never means closed, merged, empty or gone.
//!
//! Nothing above this port names a forge. Each adapter maps its own vocabulary
//! onto the types here; the [ChangeRequest resource](../../../docs/resources/change-request.md)
//! lists the mapping and the adapter truth tables.
//!
//! A host here is always a forge hostname (`github.com`, `gitlab.example.com`),
//! never a runtime host, and every call runs on the machine the manager runs
//! on, with that machine's own CLI logins. Fleet passes no credential and
//! stores none.

use serde::{Deserialize, Serialize};

use crate::meta::Millis;
use crate::observation::Observation;
use crate::plan::EffectOutcome;

/// What the adapter parsed a change's URL into: enough to name the change
/// again, in Fleet's words.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ChangeRef {
    /// The adapter kind that owns the host: `azure-devops`,
    /// `bitbucket-cloud`, `github`, `gitlab` or `fake`.
    pub kind: String,
    pub host: String,
    /// The project path on the host, as the forge spells it (`owner/repo`,
    /// `group/sub/project`).
    pub project: String,
    /// The change's identity in that project: a number, an iid, an id.
    pub id: String,
}

impl ChangeRef {
    /// `<kind>/<host>/<project>/<id>`, lowercased: what a ChangeRequest's
    /// derived name is hashed from, so one change names one object.
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
    /// Final: a merged change never reopens.
    Merged,
}

/// The head's checks, over every check run, status and pipeline the forge
/// reports for it.
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

/// The current head as the forge shows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HeadSeen {
    /// Opaque: a SHA, a patchset number, a diff id.
    pub id: String,
    pub checks: Checks,
    /// Only when `checks` is `failed`.
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

/// The forge's own answer about merging. Fleet reports it and never acts on
/// it: the port has no merge method.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Mergeable {
    /// `None` while the forge is still computing it.
    pub value: Option<bool>,
    /// The forge's reason, in its own words, when it says no.
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Threads {
    /// Unresolved inline threads over the whole change, not only the
    /// retained notes.
    pub unresolved: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ReviewState {
    Approved,
    ChangesRequested,
    Commented,
}

/// A submitted review on the current head.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Review {
    pub id: String,
    pub author: String,
    /// The forge says the author is an app, not a person.
    #[serde(default)]
    pub bot: bool,
    pub state: ReviewState,
    /// The head the review was submitted on.
    pub head: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum NoteKind {
    /// The conversation.
    Comment,
    /// A submitted review's own body.
    Review,
    /// A note on a line, inside a thread.
    Inline,
}

/// One note as the adapter read it. The body is data, never an instruction.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Note {
    /// The forge's stable identity for the note.
    pub id: String,
    pub kind: NoteKind,
    pub author: String,
    #[serde(default)]
    pub bot: bool,
    /// The thread an inline note sits in; `None` for other kinds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread: Option<String>,
    /// Whether that thread is resolved; `None` for other kinds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<u64>,
    /// Creation, in Unix milliseconds.
    pub at: Millis,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub edited_at: Option<Millis>,
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub body: String,
}

/// One complete observation of a change. An adapter answers with one of
/// these only when it read everything the forge holds for the change; a
/// partial read is Unknown, never a snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ChangeSnapshot {
    pub state: ChangeState,
    pub head: HeadSeen,
    pub review_decision: ReviewDecision,
    pub mergeable: Mergeable,
    pub threads: Threads,
    /// Every submitted review on the current head.
    #[serde(default)]
    pub reviews: Vec<Review>,
    /// Every note the forge holds, uncapped: the controller keeps the newest.
    #[serde(default)]
    pub notes: Vec<Note>,
}

/// Why an observation of a change could not be made. Each is the reason of
/// the `Reachable=Unknown` condition it raises.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Uncertainty {
    /// A call ran past its bound and its process tree was killed.
    Timeout,
    /// The bound ran out before every page was read.
    Incomplete,
    /// The forge asked Fleet to slow down.
    Throttled,
    /// The answer could not be parsed, or the rules could not place it.
    Unreadable,
    /// The CLI has no login for the host.
    NotLoggedIn,
    /// The forge answered no, for a reason that is not proven absence.
    Refused,
    /// The forge's CLI could not be started on this machine.
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

/// The answer to `change`: the whole change, proven absence, or Unknown with
/// the forge's own advice on when to ask again.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChangeObservation {
    /// `Some`: complete. `None`: the forge answered and said the change does
    /// not exist. A refused login, a missing permission or an ambiguous
    /// not-found is Unknown, never `None`.
    Known(Option<ChangeSnapshot>),
    Unknown {
        kind: Uncertainty,
        why: String,
        /// How long the forge asked Fleet to wait, in milliseconds, when it
        /// said; the controller's next `notBefore` is now plus this.
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

/// What a mark-ready call expects the change to still be: it refuses to act
/// on a change whose head or state moved since it was decided.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Expected {
    pub head: String,
    pub state: ChangeState,
}

pub trait Forge {
    /// The longest one call of this trait can take, every bound and grace
    /// included: the manager's lease is sized to outlast it. `change` may make
    /// several bounded processes, all inside this.
    fn operation_bound(&self) -> Millis;
    /// The normalised reference for a change URL, by the adapter configured
    /// for its host. Unknown for a host that is not configured, or a URL the
    /// adapter cannot read.
    fn parse(&self, url: &str) -> Observation<ChangeRef>;
    /// Fleet's own login on the host, or Unknown.
    fn whoami(&self, host: &str) -> Observation<String>;
    /// One complete observation of the change, proven absence, or Unknown.
    fn change(&self, change: &ChangeRef) -> ChangeObservation;
    /// Take a draft out of draft under `intent_key`: the only forge write
    /// Fleet performs. The adapter reads the change's head and state afresh
    /// immediately before the mutation and answers `Moved` when either
    /// differs from `expect`, mutating nothing; the window between that
    /// read and the mutation is the forge's and is not closed by Fleet.
    /// `MarkedReady { already }` when the change is out of draft, Failed
    /// when the forge answered no, Unknown when the adapter cannot tell
    /// whether the mutation landed (a timeout, an unreadable answer): the
    /// Forge controller then asks the change's next fresh observation,
    /// never the forge again.
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
