//! Events on a [Board](crate::board): the envelope, its validation, and what
//! a reader gets back. Field names follow CloudEvents.
//!
//! The publisher sets `type`, `subject`, `key`, `correlation` and `data`.
//! Fleet sets `board`, `seq` (one total order per board, from 1), `time` and
//! `source`. A second event with the `(source, key)` of one the board still
//! holds is not appended: the publisher gets the first one back.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::meta::{Millis, Object};

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
    /// A session's Task, whose outbox TaskController ingested the event
    /// from.
    Task(TaskSource),
    /// `controller/<name>`: a Fleet controller, the only publisher of the
    /// [`RESERVED_PREFIX`].
    Controller(ControllerSource),
}

impl Source {
    /// The source as `fleet event list --source` matches it: `cli/<user>`
    /// or `task/<name>`.
    pub fn name(&self) -> String {
        match self {
            Source::Cli(c) => c.0.clone(),
            Source::Task(t) => format!("{TASK_SOURCE_PREFIX}{}", t.task),
            Source::Controller(c) => c.0.clone(),
        }
    }

    /// Whether `other` is the same publisher, as a key's dedupe compares
    /// them: a Task by its name and uid, whatever its lineage.
    pub fn same_publisher(&self, other: &Source) -> bool {
        match (self, other) {
            (Source::Cli(a), Source::Cli(b)) => a == b,
            (Source::Task(a), Source::Task(b)) => a.task == b.task && a.uid == b.uid,
            (Source::Controller(a), Source::Controller(b)) => a == b,
            _ => false,
        }
    }
}

pub const TASK_SOURCE_PREFIX: &str = "task/";

/// A Task as an event names it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskRef {
    pub task: String,
    pub uid: String,
}

/// The Task whose session published, and its ancestors, nearest first.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskSource {
    pub task: String,
    pub uid: String,
    pub lineage: Vec<TaskRef>,
}

/// The most ancestors a [`TaskSource`] names.
pub const MAX_LINEAGE: usize = 16;

/// The source of an event `task`'s session published. Its lineage follows
/// the parent labels ([`PARENT_LABEL`](crate::talk::PARENT_LABEL)) through
/// `lookup`, nearest first. It stops at a parent `lookup` does not find
/// under the labelled uid, after naming that parent.
pub fn task_source<'a>(task: &'a Object, lookup: impl Fn(&str) -> Option<&'a Object>) -> Source {
    use crate::talk::{PARENT_LABEL, PARENT_UID_LABEL};
    let mut lineage = Vec::new();
    let mut current = task;
    while lineage.len() < MAX_LINEAGE {
        let labels = &current.metadata.labels;
        let (Some(parent), Some(uid)) = (labels.get(PARENT_LABEL), labels.get(PARENT_UID_LABEL))
        else {
            break;
        };
        lineage.push(TaskRef {
            task: parent.clone(),
            uid: uid.clone(),
        });
        match lookup(parent) {
            Some(next) if next.metadata.uid == *uid => current = next,
            _ => break,
        }
    }
    Source::Task(TaskSource {
        task: task.metadata.name.clone(),
        uid: task.metadata.uid.clone(),
        lineage,
    })
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

pub const CONTROLLER_SOURCE_PREFIX: &str = "controller/";

/// `controller/<field manager>`, such as `controller/task-controller`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct ControllerSource(String);

impl ControllerSource {
    pub fn of(by: &crate::store::FieldManager) -> ControllerSource {
        ControllerSource(format!("{CONTROLLER_SOURCE_PREFIX}{}", by.name()))
    }
}

impl TryFrom<String> for ControllerSource {
    type Error = String;

    fn try_from(s: String) -> Result<ControllerSource, String> {
        match s.strip_prefix(CONTROLLER_SOURCE_PREFIX) {
            Some(name) if !name.is_empty() && !name.contains(['/', '\0']) => {
                Ok(ControllerSource(s))
            }
            _ => Err(format!("source {s:?}: use controller/<name>")),
        }
    }
}

impl From<ControllerSource> for String {
    fn from(c: ControllerSource) -> String {
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
        publish.key.is_some() && self.key == publish.key && self.source.same_publisher(source)
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
/// `maxDataBytes`. The [`RESERVED_PREFIX`] is refused: this check is for a
/// publisher that is not a controller.
pub fn validate_publish(p: &Publish, max_data_bytes: u64) -> Result<(), String> {
    if p.event_type.starts_with(RESERVED_PREFIX) {
        return Err(format!(
            "type {:?}: the {RESERVED_PREFIX} prefix is reserved for Fleet's controllers",
            p.event_type
        ));
    }
    validate_envelope(p, max_data_bytes)
}

/// [`validate_publish`] for a publish from `source`: a
/// [`Source::Controller`] may also use the [`RESERVED_PREFIX`].
pub fn validate_publish_from(
    source: &Source,
    p: &Publish,
    max_data_bytes: u64,
) -> Result<(), String> {
    match source {
        Source::Controller(_) => validate_envelope(p, max_data_bytes),
        Source::Cli(_) | Source::Task(_) => validate_publish(p, max_data_bytes),
    }
}

fn validate_envelope(p: &Publish, max_data_bytes: u64) -> Result<(), String> {
    validate_type(&p.event_type)?;
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

/// The variable that names a session's outbox: the file `fleet event
/// publish` appends to inside the session, which TaskController ingests.
pub const EVENT_FILE_ENV: &str = "FLEET_EVENT_FILE";

/// The outbox of the session whose signal file is `signal_file`: beside it,
/// on the same host, under a name no signal file has.
pub fn outbox_file(signal_file: &str) -> String {
    format!("{signal_file}.events")
}

/// The most bytes of one outbox line, its newline included.
pub const MAX_OUTBOX_LINE: usize = 64 * 1024;
/// The most bytes of an outbox one ingest reads, from the stored offset.
pub const MAX_OUTBOX_READ: u64 = 512 * 1024;

/// One line of a session's outbox: the board, and what a publisher sets.
/// A field it does not name, such as a source or a Task, is ignored:
/// Fleet names the source.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OutboxLine {
    pub board: String,
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

impl OutboxLine {
    pub fn new(board: &str, publish: Publish) -> OutboxLine {
        OutboxLine {
            board: board.to_string(),
            event_type: publish.event_type,
            subject: publish.subject,
            key: publish.key,
            correlation: publish.correlation,
            data: publish.data,
        }
    }

    /// The board and the publish, once they pass every check that needs no
    /// store: the board's name, the envelope's rules and the reserved
    /// prefix. The board's own `maxDataBytes` is checked at the append.
    pub fn checked(self) -> Result<(String, Publish), String> {
        crate::validate::validate_name(&self.board).map_err(|e| format!("board: {e}"))?;
        let publish = Publish {
            event_type: self.event_type,
            subject: self.subject,
            key: self.key,
            correlation: self.correlation,
            data: self.data,
        };
        validate_publish(&publish, u64::MAX)?;
        Ok((self.board, publish))
    }

    /// The line as `fleet event publish` appends it, newline included.
    pub fn encode(&self) -> Result<Vec<u8>, String> {
        self.clone().checked()?;
        let mut line = serde_json::to_vec(self).map_err(|e| e.to_string())?;
        line.push(b'\n');
        if line.len() > MAX_OUTBOX_LINE {
            return Err(format!(
                "the event is {} bytes as a line; a session publishes at most {MAX_OUTBOX_LINE}",
                line.len()
            ));
        }
        Ok(line)
    }
}

/// What a read of an outbox answers: the bytes from `start`, at most
/// [`MAX_OUTBOX_READ`]. `start` is the offset asked for, or 0 when the file
/// is shorter than it: the file was made again.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutboxRead {
    pub start: u64,
    pub bytes: Vec<u8>,
}

/// TaskController's place in a session's outbox (`Task.status.outbox`):
/// the offset of the first line not ingested yet, how often the file was
/// made again, and the lines it skipped.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OutboxCursor {
    pub offset: u64,
    /// Bumped when the outbox is shorter than `offset`: part of each key
    /// derived from an offset, so a new file's lines are new events.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub generation: u64,
    /// Lines that did not parse or were refused, over the Task's life.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub skipped: u64,
    /// Why the last skipped line was skipped, and where it was, in the
    /// current generation: a file made again clears it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_skipped: Option<String>,
    /// The session is gone and a read since found every whole line in:
    /// nothing reads the outbox again until the session is back.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub closed: bool,
    /// The bytes at `offset` continue a line already skipped as too long:
    /// they are passed over up to its newline, never read as a line.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub discarding: bool,
}

fn is_zero(n: &u64) -> bool {
    *n == 0
}

impl OutboxCursor {
    /// Past the line that ends at `end`, which was skipped for `why`.
    pub fn skip(&mut self, offset: u64, end: u64, why: &str) {
        self.offset = end;
        self.skipped += 1;
        self.last_skipped = Some(format!("offset {offset}: {why}"));
    }

    /// Why the line at `offset` was skipped, when it is the last line
    /// skipped.
    pub fn skipped_at(&self, offset: u64) -> Option<&str> {
        self.last_skipped
            .as_deref()
            .filter(|why| why.strip_prefix(&format!("offset {offset}: ")).is_some())
    }
}

/// One whole line of an outbox: where it starts and ends, and the board and
/// publish it carries, or why it is skipped.
#[derive(Debug, Clone, PartialEq)]
pub struct OutboxEntry {
    pub offset: u64,
    pub end: u64,
    pub line: Result<(String, Publish), String>,
    /// A line skipped before its newline was read: the cursor passes over
    /// the rest of it (`OutboxCursor::discarding`).
    pub torn: bool,
}

/// The whole lines of `read`, oldest first, and the cursor they continue:
/// `cursor`, or the start of a file made again under the next generation.
/// A last line with no newline yet waits for it, unless it is already
/// longer than [`MAX_OUTBOX_LINE`]. A line without a key gets one derived
/// from its generation and offset, so ingesting it again appends nothing.
pub fn ingest(cursor: &OutboxCursor, read: &OutboxRead) -> (OutboxCursor, Vec<OutboxEntry>) {
    let mut from = cursor.clone();
    if read.start < cursor.offset {
        from.generation += 1;
        from.offset = read.start;
        from.discarding = false;
        from.last_skipped = None;
    }
    let base = from.offset;
    let mut entries = Vec::new();
    let mut at = 0;
    if from.discarding {
        match read.bytes.iter().position(|&b| b == b'\n') {
            Some(n) => {
                at = n + 1;
                from.discarding = false;
            }
            None => at = read.bytes.len(),
        }
        from.offset = base + at as u64;
    }
    while at < read.bytes.len() {
        let rest = &read.bytes[at..];
        let offset = base + at as u64;
        let Some(n) = rest.iter().position(|&b| b == b'\n') else {
            if rest.len() >= MAX_OUTBOX_LINE {
                entries.push(OutboxEntry {
                    offset,
                    end: offset + rest.len() as u64,
                    line: Err(format!(
                        "{} bytes with no newline; a line is at most {MAX_OUTBOX_LINE}",
                        rest.len()
                    )),
                    torn: true,
                });
            }
            break;
        };
        let line = if n + 1 > MAX_OUTBOX_LINE {
            Err(format!(
                "a line of {} bytes; at most {MAX_OUTBOX_LINE}",
                n + 1
            ))
        } else {
            parse_line(&rest[..n], from.generation, offset)
        };
        entries.push(OutboxEntry {
            offset,
            end: offset + n as u64 + 1,
            line,
            torn: false,
        });
        at += n + 1;
    }
    (from, entries)
}

fn parse_line(bytes: &[u8], generation: u64, offset: u64) -> Result<(String, Publish), String> {
    let line: OutboxLine =
        serde_json::from_slice(bytes).map_err(|e| format!("not an event: {e}"))?;
    let (board, mut publish) = line.checked()?;
    if publish.key.is_none() {
        publish.key = Some(outbox_key(generation, offset));
    }
    Ok((board, publish))
}

/// The key Fleet gives the line at `offset` of an outbox at `generation`
/// when the line names none.
pub fn outbox_key(generation: u64, offset: u64) -> String {
    format!("outbox-{generation}-{offset}")
}

/// A filter value: exact, or a prefix when it ends in `*`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Pattern(String);

impl Pattern {
    pub fn new(p: &str) -> Pattern {
        Pattern(p.to_string())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn matches(&self, value: &str) -> bool {
        match self.0.strip_suffix('*') {
            Some(prefix) => value.starts_with(prefix),
            None => value == self.0,
        }
    }
}

/// What `fleet event list` keeps; an empty filter keeps every event.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Filter {
    #[serde(rename = "type", default, skip_serializing_if = "Option::is_none")]
    pub event_type: Option<Pattern>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject: Option<Pattern>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub correlation: Option<String>,
}

impl Filter {
    pub fn is_empty(&self) -> bool {
        *self == Filter::default()
    }

    /// The `fleet event list` options that keep what this filter keeps,
    /// each as `--flag=value` with the value quoted for a POSIX shell, so a
    /// value that starts with a dash stays a value, each with a leading
    /// space.
    pub fn cli_args(&self) -> String {
        let options = [
            ("--type", self.event_type.as_ref().map(Pattern::as_str)),
            ("--subject", self.subject.as_ref().map(Pattern::as_str)),
            ("--source", self.source.as_deref()),
            ("--correlation", self.correlation.as_deref()),
        ];
        options
            .into_iter()
            .filter_map(|(flag, value)| value.map(|v| format!(" {flag}={}", shell_quote(v))))
            .collect()
    }

    pub fn matches(&self, e: &Event) -> bool {
        self.event_type
            .as_ref()
            .is_none_or(|p| p.matches(&e.event_type))
            && self
                .subject
                .as_ref()
                .is_none_or(|p| e.subject.as_deref().is_some_and(|s| p.matches(s)))
            && self.source.as_ref().is_none_or(|s| e.source.name() == *s)
            && self
                .correlation
                .as_ref()
                .is_none_or(|c| e.correlation.as_ref() == Some(c))
    }
}

fn shell_quote(value: &str) -> String {
    let plain = !value.is_empty()
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_./:=@,+".contains(c));
    if plain {
        value.to_string()
    } else {
        format!("'{}'", value.replace('\'', r"'\''"))
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
    fn a_controller_source_is_a_string_on_the_wire_and_alone_takes_the_reserved_prefix() {
        let ctl = Source::Controller(ControllerSource::of(&crate::FieldManager::TaskController));
        assert_eq!(
            serde_json::to_value(&ctl).unwrap(),
            json!("controller/task-controller")
        );
        let back: Source = serde_json::from_value(json!("controller/task-controller")).unwrap();
        assert_eq!(back, ctl);
        assert_eq!(ctl.name(), "controller/task-controller");
        assert!(serde_json::from_value::<Source>(json!("controller/")).is_err());
        assert!(serde_json::from_value::<Source>(json!("controller/a/b")).is_err());

        let reserved = publish("fleet.task.phase");
        assert!(validate_publish_from(&ctl, &reserved, 16).is_ok());
        let cli = Source::Cli(CliSource::new("alice").unwrap());
        assert!(validate_publish_from(&cli, &reserved, 16).is_err());
        let mut big = publish("fleet.task.phase");
        big.data = json!("x".repeat(16));
        assert!(validate_publish_from(&ctl, &big, 16).is_err());
    }

    #[test]
    fn a_pattern_ending_in_a_star_is_a_prefix() {
        assert!(Pattern::new("git.*").matches("git.push"));
        assert!(!Pattern::new("git.*").matches("gitx"));
        assert!(Pattern::new("git").matches("git"));
        assert!(!Pattern::new("git").matches("git.push"));
    }

    fn line(board: &str, t: &str, key: Option<&str>) -> String {
        let mut p = publish(t);
        p.key = key.map(str::to_string);
        String::from_utf8(OutboxLine::new(board, p).encode().unwrap()).unwrap()
    }

    fn read(start: u64, text: &str) -> OutboxRead {
        OutboxRead {
            start,
            bytes: text.as_bytes().to_vec(),
        }
    }

    #[test]
    fn ingest_reads_whole_lines_and_derives_a_key_from_the_offset() {
        let a = line("shared", "a", None);
        let b = line("other", "b", Some("mine"));
        let text = format!("{a}{b}{{\"board\":\"shared\",");
        let (cursor, entries) = ingest(&OutboxCursor::default(), &read(0, &text));
        assert_eq!(cursor, OutboxCursor::default());
        let ends: Vec<_> = entries.iter().map(|e| (e.offset, e.end)).collect();
        let a_end = a.len() as u64;
        assert_eq!(ends, vec![(0, a_end), (a_end, a_end + b.len() as u64)]);
        let (board, p) = entries[0].line.clone().unwrap();
        assert_eq!(
            (board.as_str(), p.key.as_deref()),
            ("shared", Some("outbox-0-0"))
        );
        let (board, p) = entries[1].line.clone().unwrap();
        assert_eq!((board.as_str(), p.key.as_deref()), ("other", Some("mine")));
    }

    #[test]
    fn ingest_continues_from_the_cursor_and_names_the_offset_in_the_file() {
        let cursor = OutboxCursor {
            offset: 100,
            ..OutboxCursor::default()
        };
        let (from, entries) = ingest(&cursor, &read(100, &line("shared", "a", None)));
        assert_eq!(from.offset, 100);
        assert_eq!(entries[0].offset, 100);
        assert_eq!(
            entries[0].line.as_ref().unwrap().1.key.as_deref(),
            Some("outbox-0-100")
        );
    }

    #[test]
    fn a_file_made_again_forgets_the_last_skip_of_the_old_file() {
        let mut cursor = OutboxCursor::default();
        cursor.skip(0, 100, "Board/nowhere not found");
        let (from, _) = ingest(&cursor, &read(0, &line("shared", "a", None)));
        assert_eq!(from.skipped_at(0), None);
        assert_eq!(from.skipped, 1);
    }

    #[test]
    fn a_file_made_again_starts_a_new_generation_of_keys() {
        let cursor = OutboxCursor {
            offset: 100,
            ..OutboxCursor::default()
        };
        let (from, entries) = ingest(&cursor, &read(0, &line("shared", "a", None)));
        assert_eq!((from.offset, from.generation), (0, 1));
        assert_eq!(
            entries[0].line.as_ref().unwrap().1.key.as_deref(),
            Some("outbox-1-0")
        );
    }

    #[test]
    fn a_bad_line_is_skipped_with_why_and_the_rest_is_read() {
        let spoof = r#"{"board":"shared","type":"a","data":1,"source":"cli/x","task":"b"}"#;
        let text = format!(
            "not json\n{}\n{{\"board\":\"Bad_Name\",\"type\":\"a\",\"data\":1}}\n{spoof}\n",
            r#"{"board":"shared","type":"fleet.x","data":1}"#
        );
        let (_, entries) = ingest(&OutboxCursor::default(), &read(0, &text));
        assert_eq!(entries.len(), 4);
        assert!(
            entries[0]
                .line
                .as_ref()
                .unwrap_err()
                .contains("not an event")
        );
        assert!(entries[1].line.as_ref().unwrap_err().contains("fleet."));
        assert!(entries[2].line.as_ref().unwrap_err().contains("board"));
        assert_eq!(entries[3].line.as_ref().unwrap().1.event_type, "a");
        let mut cursor = OutboxCursor::default();
        cursor.skip(entries[0].offset, entries[0].end, "not an event");
        assert_eq!(cursor.offset, entries[0].end);
        assert_eq!(cursor.skipped, 1);
        assert_eq!(
            cursor.last_skipped.as_deref(),
            Some("offset 0: not an event")
        );
        assert_eq!(cursor.skipped_at(0), Some("offset 0: not an event"));
        assert_eq!(cursor.skipped_at(1), None);
        cursor.skip(10, 20, "offset 1: no");
        assert_eq!(cursor.skipped_at(1), None);
        assert_eq!(cursor.skipped_at(10), Some("offset 10: offset 1: no"));
    }

    #[test]
    fn an_overlong_line_is_skipped_whole_with_or_without_its_newline() {
        let long = "x".repeat(MAX_OUTBOX_LINE);
        let (_, entries) = ingest(&OutboxCursor::default(), &read(0, &format!("{long}\n")));
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].end, MAX_OUTBOX_LINE as u64 + 1);
        assert!(entries[0].line.is_err());
        let (_, entries) = ingest(&OutboxCursor::default(), &read(0, &long));
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].end, MAX_OUTBOX_LINE as u64);
        assert!(entries[0].line.is_err());
        let (_, entries) = ingest(&OutboxCursor::default(), &read(0, &long[1..]));
        assert!(entries.is_empty(), "a shorter torn line waits");
    }

    #[test]
    fn the_tail_of_a_line_skipped_as_too_long_never_becomes_an_event() {
        let head = " ".repeat(MAX_OUTBOX_LINE);
        let (from, entries) = ingest(&OutboxCursor::default(), &read(0, &head));
        assert!(entries[0].torn);
        let mut cursor = from;
        cursor.skip(entries[0].offset, entries[0].end, "too long");
        cursor.discarding = true;
        let tail = line("shared", "smuggled", None);
        let next = line("shared", "after", None);
        let at = cursor.offset;
        let (from, entries) = ingest(&cursor, &read(at, &format!("  {tail}{next}")));
        let tail_end = at + 2 + tail.len() as u64;
        assert_eq!((from.offset, from.discarding), (tail_end, false));
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].offset, tail_end);
        assert_eq!(entries[0].line.as_ref().unwrap().1.event_type, "after");
        let (from, entries) = ingest(&cursor, &read(at, "no newline yet"));
        assert!(entries.is_empty());
        assert_eq!((from.offset, from.discarding), (at + 14, true));
    }

    #[test]
    fn an_encoded_line_is_bounded_and_checked() {
        let mut p = publish("a");
        p.data = json!("x".repeat(MAX_OUTBOX_LINE));
        assert!(OutboxLine::new("shared", p).encode().is_err());
        assert!(
            OutboxLine::new("shared", publish("fleet.a"))
                .encode()
                .is_err()
        );
        assert!(OutboxLine::new("../x", publish("a")).encode().is_err());
    }

    fn task(name: &str, uid: &str, parent: Option<(&str, &str)>) -> Object {
        let mut labels = serde_json::Map::new();
        if let Some((p, u)) = parent {
            labels.insert(crate::talk::PARENT_LABEL.into(), json!(p));
            labels.insert(crate::talk::PARENT_UID_LABEL.into(), json!(u));
        }
        serde_json::from_value(json!({
            "apiVersion": crate::API_VERSION,
            "kind": "Task",
            "metadata": {"name": name, "uid": uid, "labels": labels},
        }))
        .unwrap()
    }

    #[test]
    fn a_task_source_names_its_ancestors_nearest_first() {
        let root = task("root", "u0", None);
        let mid = task("mid", "u1", Some(("root", "u0")));
        let leaf = task("leaf", "u2", Some(("mid", "u1")));
        let all = [&root, &mid, &leaf];
        let lookup = |n: &str| all.iter().copied().find(|t| t.metadata.name == n);
        let Source::Task(s) = task_source(&leaf, lookup) else {
            panic!("a task source")
        };
        assert_eq!((s.task.as_str(), s.uid.as_str()), ("leaf", "u2"));
        let names: Vec<_> = s.lineage.iter().map(|r| r.task.as_str()).collect();
        assert_eq!(names, vec!["mid", "root"]);

        let orphan = task("o", "u9", Some(("gone", "ux")));
        let Source::Task(s) = task_source(&orphan, |_| None) else {
            panic!("a task source")
        };
        assert_eq!(
            s.lineage,
            vec![TaskRef {
                task: "gone".into(),
                uid: "ux".into()
            }]
        );
        let under_a_recreated_parent = task("o2", "u8", Some(("mid", "old")));
        let Source::Task(s) = task_source(&under_a_recreated_parent, lookup) else {
            panic!("a task source")
        };
        let names: Vec<_> = s
            .lineage
            .iter()
            .map(|r| (r.task.as_str(), r.uid.as_str()))
            .collect();
        assert_eq!(
            names,
            vec![("mid", "old")],
            "a parent recreated under another uid is named, and its ancestors are not"
        );
    }

    #[test]
    fn a_task_source_is_an_object_on_the_wire_and_dedupes_by_task_and_uid() {
        let source: Source = serde_json::from_value(json!({
            "task": "a", "uid": "u", "lineage": [{"task": "p", "uid": "v"}]
        }))
        .unwrap();
        assert_eq!(source.name(), "task/a");
        let bare = Source::Task(TaskSource {
            task: "a".into(),
            uid: "u".into(),
            lineage: vec![],
        });
        assert!(source.same_publisher(&bare));
        let other = Source::Task(TaskSource {
            task: "a".into(),
            uid: "u2".into(),
            lineage: vec![],
        });
        assert!(!source.same_publisher(&other));
        assert!(!source.same_publisher(&Source::Cli(CliSource::new("a").unwrap())));
    }
}
