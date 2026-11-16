//! Events on a [Board](crate::board): the envelope, its validation, and what
//! a reader gets back. Field names follow CloudEvents.
//!
//! The publisher sets `type`, `subject`, `key`, `correlation` and `data`.
//! Fleet sets `board`, `seq` (one total order per board, from 1), `time` and
//! `source`. A second event with the `(source, key)` of one the board still
//! holds is not appended: the publisher gets the first one back.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::meta::Millis;

/// Event types under this prefix are Fleet's own: only a controller
/// publishes them.
pub const RESERVED_PREFIX: &str = "fleet.";

pub const MAX_TYPE_LEN: usize = 128;
/// The most characters of `subject`, `key` and `correlation` each.
pub const MAX_FIELD_CHARS: usize = 256;

/// Who published an event. Fleet sets it; a publisher never names it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Source {
    /// `cli/<user>`: a person or a script outside any session, through the
    /// manager's publish verb.
    Cli(CliSource),
}

impl Source {
    /// The source as `fleet event list --source` matches it.
    pub fn name(&self) -> &str {
        match self {
            Source::Cli(c) => &c.0,
        }
    }
}

/// `cli/<user>`, `user` non-empty and without `/`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct CliSource(String);

impl CliSource {
    pub fn new(user: &str) -> Result<CliSource, String> {
        CliSource::try_from(format!(
            "{}{}",
            crate::inbox::CLI_SOURCE_PREFIX,
            user.trim()
        ))
    }
}

impl TryFrom<String> for CliSource {
    type Error = String;

    fn try_from(s: String) -> Result<CliSource, String> {
        match s.strip_prefix(crate::inbox::CLI_SOURCE_PREFIX) {
            Some(user) if !user.is_empty() && !user.contains(['/', '\0']) => Ok(CliSource(s)),
            _ => Err(format!(
                "source {s:?}: use cli/<user>, a user name without '/'"
            )),
        }
    }
}

impl From<CliSource> for String {
    fn from(c: CliSource) -> String {
        c.0
    }
}

/// What a publisher sets.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Publish {
    #[serde(rename = "type")]
    pub event_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
    /// With `source`, what makes a retried publish the same event.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    /// Ties events of one piece of work together.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub correlation: Option<String>,
    pub data: Value,
}

/// One entry of a board's log.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Event {
    pub board: String,
    pub seq: u64,
    pub time: Millis,
    pub source: Source,
    #[serde(rename = "type")]
    pub event_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub correlation: Option<String>,
    pub data: Value,
}

impl Event {
    pub fn new(board: &str, seq: u64, time: Millis, source: Source, publish: Publish) -> Event {
        Event {
            board: board.to_string(),
            seq,
            time,
            source,
            event_type: publish.event_type,
            subject: publish.subject,
            key: publish.key,
            correlation: publish.correlation,
            data: publish.data,
        }
    }

    /// Whether `publish` from `source` is this event again.
    pub fn same_key(&self, source: &Source, publish: &Publish) -> bool {
        publish.key.is_some() && self.key == publish.key && self.source == *source
    }
}

/// What an append answers for one publish: the event the board holds under
/// it, and whether it was there already.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Appended {
    pub event: Event,
    pub duplicate: bool,
}

/// What a read answers.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Page {
    /// The events after the cursor, oldest first.
    Events(Vec<Event>),
    /// The cursor is older than the log: the events up to `oldest - 1` are
    /// gone. A reader resumes from `oldest - 1`, knowing it missed some.
    Expired { oldest: u64 },
}

/// Dot-separated segments of `a-z`, `0-9`, `_` and `-`, none empty, at most
/// [`MAX_TYPE_LEN`] characters in all.
pub fn validate_type(t: &str) -> Result<(), String> {
    let segment_ok = |s: &str| {
        !s.is_empty()
            && s.chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
    };
    if t.len() > MAX_TYPE_LEN || !t.split('.').all(segment_ok) {
        return Err(format!(
            "type {t:?}: use dot-separated segments of a-z, 0-9, '_' and '-', at most {MAX_TYPE_LEN} characters"
        ));
    }
    Ok(())
}

fn validate_field(name: &str, value: Option<&str>) -> Result<(), String> {
    match value {
        Some(v) if v.is_empty() || v.chars().count() > MAX_FIELD_CHARS => Err(format!(
            "{name}: use 1 to {MAX_FIELD_CHARS} characters, or leave it out"
        )),
        _ => Ok(()),
    }
}

/// Check a publish against the envelope's rules and the board's
/// `maxDataBytes`. The [`RESERVED_PREFIX`] is refused: no publisher in this
/// release is a controller.
pub fn validate_publish(p: &Publish, max_data_bytes: u64) -> Result<(), String> {
    validate_type(&p.event_type)?;
    if p.event_type.starts_with(RESERVED_PREFIX) {
        return Err(format!(
            "type {:?}: the {RESERVED_PREFIX} prefix is reserved for Fleet's controllers",
            p.event_type
        ));
    }
    validate_field("subject", p.subject.as_deref())?;
    validate_field("key", p.key.as_deref())?;
    validate_field("correlation", p.correlation.as_deref())?;
    let size = serde_json::to_vec(&p.data).map_or(usize::MAX, |b| b.len());
    if size as u64 > max_data_bytes {
        return Err(format!(
            "data is {size} bytes; this board takes at most {max_data_bytes}"
        ));
    }
    Ok(())
}

/// A filter value: exact, or a prefix when it ends in `*`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pattern(String);

impl Pattern {
    pub fn new(p: &str) -> Pattern {
        Pattern(p.to_string())
    }

    pub fn matches(&self, value: &str) -> bool {
        match self.0.strip_suffix('*') {
            Some(prefix) => value.starts_with(prefix),
            None => value == self.0,
        }
    }
}

/// What `fleet event list` keeps; an empty filter keeps every event.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Filter {
    pub event_type: Option<Pattern>,
    pub subject: Option<Pattern>,
    pub source: Option<String>,
    pub correlation: Option<String>,
}

impl Filter {
    pub fn matches(&self, e: &Event) -> bool {
        self.event_type
            .as_ref()
            .is_none_or(|p| p.matches(&e.event_type))
            && self
                .subject
                .as_ref()
                .is_none_or(|p| e.subject.as_deref().is_some_and(|s| p.matches(s)))
            && self.source.as_ref().is_none_or(|s| e.source.name() == s)
            && self
                .correlation
                .as_ref()
                .is_none_or(|c| e.correlation.as_ref() == Some(c))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn publish(t: &str) -> Publish {
        Publish {
            event_type: t.into(),
            subject: None,
            key: None,
            correlation: None,
            data: json!({}),
        }
    }

    #[test]
    fn the_type_grammar_takes_dotted_lowercase_segments() {
        for ok in [
            "a",
            "git.push",
            "ci.job-done",
            "x_1.y-2.z",
            &"a".repeat(128),
        ] {
            assert!(validate_type(ok).is_ok(), "{ok}");
        }
        for bad in [
            "",
            ".a",
            "a.",
            "a..b",
            "Git",
            "a b",
            "a/b",
            "é",
            &"a".repeat(129),
        ] {
            assert!(validate_type(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn the_reserved_prefix_is_refused() {
        assert!(validate_publish(&publish("fleet.task.phase"), 16).is_err());
        assert!(validate_publish(&publish("fleetish.x"), 16).is_ok());
    }

    #[test]
    fn data_is_bounded_by_its_serialized_size() {
        let mut p = publish("a");
        p.data = json!("xx");
        assert!(validate_publish(&p, 4).is_ok());
        assert!(validate_publish(&p, 3).is_err());
    }

    #[test]
    fn subject_key_and_correlation_are_bounded_in_characters() {
        let mut p = publish("a");
        p.subject = Some("é".repeat(256));
        assert!(validate_publish(&p, 16).is_ok());
        p.subject = Some("é".repeat(257));
        assert!(validate_publish(&p, 16).is_err());
        p.subject = None;
        p.key = Some(String::new());
        assert!(validate_publish(&p, 16).is_err());
    }

    #[test]
    fn a_cli_source_is_a_string_on_the_wire() {
        let source = Source::Cli(CliSource::new("alice").unwrap());
        assert_eq!(serde_json::to_value(&source).unwrap(), json!("cli/alice"));
        let back: Source = serde_json::from_value(json!("cli/alice")).unwrap();
        assert_eq!(back, source);
        assert!(serde_json::from_value::<Source>(json!("forge/x")).is_err());
        assert!(CliSource::new("a/b").is_err());
        assert!(CliSource::new(" ").is_err());
    }

    #[test]
    fn a_pattern_ending_in_a_star_is_a_prefix() {
        assert!(Pattern::new("git.*").matches("git.push"));
        assert!(!Pattern::new("git.*").matches("gitx"));
        assert!(Pattern::new("git").matches("git"));
        assert!(!Pattern::new("git").matches("git.push"));
    }
}
