use std::collections::BTreeSet;

use super::snapshot::{SessionRow, Snapshot};

#[derive(Debug, Clone, PartialEq)]
pub enum Field {
    Text(String),
    Bool(bool),
    Number(f64),
    List(Vec<String>),
}

impl From<&str> for Field {
    fn from(text: &str) -> Self {
        Field::Text(text.to_string())
    }
}

impl From<String> for Field {
    fn from(text: String) -> Self {
        Field::Text(text)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Event {
    pub name: String,
    pub payload: Vec<(String, Field)>,
    pub depth: u8,
    pub only: Option<String>,
}

impl Event {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            payload: Vec::new(),
            depth: 0,
            only: None,
        }
    }

    pub fn to(mut self, mod_name: impl Into<String>) -> Self {
        self.only = Some(mod_name.into());
        self
    }

    pub fn with(mut self, key: &str, value: Option<impl Into<Field>>) -> Self {
        if let Some(value) = value {
            self.payload.push((key.to_string(), value.into()));
        }
        self
    }

    pub fn text(&self, key: &str) -> Option<&str> {
        self.payload.iter().find_map(|(k, v)| match v {
            Field::Text(text) if k == key => Some(text.as_str()),
            _ => None,
        })
    }
}

pub const MAX_DEPTH: u8 = 4;

pub const USER_PREFIX: &str = "user.";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EventSpec {
    pub name: &'static str,
    pub when: &'static str,
    pub fields: &'static [&'static str],
}

pub const KERNEL_EVENTS: &[EventSpec] = &[
    EventSpec {
        name: "session.created",
        when: "a session row appears in the snapshot, whoever made it",
        fields: &["session", "name", "agent", "repo"],
    },
    EventSpec {
        name: "session.deleted",
        when: "a session row leaves the snapshot",
        fields: &["session", "name"],
    },
    EventSpec {
        name: "session.status",
        when: "a session's derived status changes",
        fields: &["session", "name", "from", "to"],
    },
    EventSpec {
        name: "session.changed",
        when: "a session's name, branch, repositories or parent change",
        fields: &["session", "name", "fields"],
    },
    EventSpec {
        name: "session.post_create",
        when: "this interface finished creating (or forking) a session",
        fields: &[
            "session", "name", "agent", "repo", "cwd", "branch", "parent",
        ],
    },
    EventSpec {
        name: "session.post_delete",
        when: "this interface finished deleting a session",
        fields: &["session", "name", "force"],
    },
    EventSpec {
        name: "focus.session",
        when: "the selected session changes",
        fields: &["from", "to"],
    },
    EventSpec {
        name: "focus.pane",
        when: "focus moves to another mod",
        fields: &["from", "to"],
    },
    EventSpec {
        name: "command.done",
        when: "a command a mod issued completed",
        fields: &["kind", "session", "subject"],
    },
    EventSpec {
        name: "command.failed",
        when: "a command a mod issued failed",
        fields: &["kind", "session", "subject", "error"],
    },
    EventSpec {
        name: "program.exited",
        when: "a program THIS mod started ended, however it ended",
        fields: &["name", "program"],
    },
    EventSpec {
        name: "interface.reloaded",
        when: "the interface was rebuilt from disk",
        fields: &["reason"],
    },
];

pub fn is_kernel_event(name: &str) -> bool {
    KERNEL_EVENTS.iter().any(|spec| spec.name == name)
}

fn is_identifier(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
}

pub fn validate_subscription(name: &str) -> Result<(), String> {
    if is_kernel_event(name) {
        return Ok(());
    }
    if let Some(rest) = name.strip_prefix(USER_PREFIX) {
        if is_identifier(rest) {
            return Ok(());
        }
        return Err(format!(
            "{name:?} is not a valid user event — the part after \"user.\" must be a name"
        ));
    }
    let known: Vec<&str> = KERNEL_EVENTS.iter().map(|spec| spec.name).collect();
    Err(format!(
        "no event named {name:?} (available: {}, or user.<name>)",
        known.join(", ")
    ))
}

pub fn user_event_name(name: &str) -> Result<String, String> {
    if is_kernel_event(name) {
        return Err(format!(
            "command \"emit\": {name:?} is a kernel event and cannot be emitted by a mod"
        ));
    }
    if name.starts_with(USER_PREFIX) {
        return Err(format!(
            "command \"emit\": name {name:?} without the \"user.\" prefix — it is added for you"
        ));
    }
    if !is_identifier(name) {
        return Err(format!(
            "command \"emit\": {name:?} is not a valid event name (letters, digits, _ - .)"
        ));
    }
    Ok(format!("{USER_PREFIX}{name}"))
}

#[derive(Debug, Clone, PartialEq)]
struct Facts {
    name: String,
    agent: String,
    repo: Option<String>,
    repos: Vec<String>,
    status: crate::session::SessionState,
    branch: Option<String>,
    parent: Option<String>,
}

impl Facts {
    fn of(row: &SessionRow) -> Self {
        Self {
            name: row.name.clone(),
            agent: row.agent.clone(),
            repo: row.repo.clone(),
            repos: row.repos.clone(),
            status: row.status,
            branch: row.branch.clone(),
            parent: row.parent_id.clone(),
        }
    }

    fn changed_since(&self, before: &Facts) -> Vec<String> {
        let mut fields = Vec::new();
        if before.name != self.name {
            fields.push("name".to_string());
        }
        if before.branch != self.branch {
            fields.push("branch".to_string());
        }
        if before.repo != self.repo || before.repos != self.repos {
            fields.push("repos".to_string());
        }
        if before.parent != self.parent {
            fields.push("parent".to_string());
        }
        fields
    }
}

#[derive(Debug, Default)]
pub struct Deriver {
    rows: Vec<(String, Facts)>,
    version: Option<u64>,
}

impl Deriver {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn observe(&mut self, snapshot: &Snapshot, version: u64) -> Vec<Event> {
        if self.version == Some(version) {
            return Vec::new();
        }
        let seeded = self.version.is_some();
        self.version = Some(version);
        let next: Vec<(String, Facts)> = snapshot
            .sessions
            .iter()
            .map(|row| (row.id.clone(), Facts::of(row)))
            .collect();
        if !seeded {
            self.rows = next;
            return Vec::new();
        }

        let mut events = Vec::new();
        push_departures(&mut events, &self.rows, &next);
        let old: std::collections::HashMap<&str, &Facts> = self
            .rows
            .iter()
            .map(|(id, facts)| (id.as_str(), facts))
            .collect();
        push_arrivals(&mut events, &old, &next);
        push_changes(&mut events, &old, &next);
        self.rows = next;
        events
    }

    pub fn reset(&mut self) {
        self.rows.clear();
        self.version = None;
    }
}

fn push_departures(events: &mut Vec<Event>, before: &[(String, Facts)], after: &[(String, Facts)]) {
    let remaining: BTreeSet<&str> = after.iter().map(|(id, _)| id.as_str()).collect();
    for (id, facts) in before {
        if !remaining.contains(id.as_str()) {
            events.push(
                Event::new("session.deleted")
                    .with("session", Some(id.as_str()))
                    .with("name", Some(facts.name.as_str())),
            );
        }
    }
}

fn push_arrivals(
    events: &mut Vec<Event>,
    old: &std::collections::HashMap<&str, &Facts>,
    after: &[(String, Facts)],
) {
    for (id, facts) in after {
        if !old.contains_key(id.as_str()) {
            events.push(
                Event::new("session.created")
                    .with("session", Some(id.as_str()))
                    .with("name", Some(facts.name.as_str()))
                    .with("agent", Some(facts.agent.as_str()))
                    .with("repo", facts.repo.as_deref()),
            );
        }
    }
}

fn push_changes(
    events: &mut Vec<Event>,
    old: &std::collections::HashMap<&str, &Facts>,
    after: &[(String, Facts)],
) {
    let mut changes = Vec::new();
    for (id, facts) in after {
        let Some(before) = old.get(id.as_str()) else {
            continue;
        };
        if before.status != facts.status {
            events.push(
                Event::new("session.status")
                    .with("session", Some(id.as_str()))
                    .with("name", Some(facts.name.as_str()))
                    .with("from", Some(before.status.as_str()))
                    .with("to", Some(facts.status.as_str())),
            );
        }
        let fields = facts.changed_since(before);
        if !fields.is_empty() {
            changes.push(
                Event::new("session.changed")
                    .with("session", Some(id.as_str()))
                    .with("name", Some(facts.name.as_str()))
                    .with("fields", Some(Field::List(fields))),
            );
        }
    }
    events.extend(changes);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::SessionState;
    use std::path::PathBuf;

    #[test]
    fn a_program_can_report_that_it_ended() {
        assert!(is_kernel_event("program.exited"));
        let spec = KERNEL_EVENTS
            .iter()
            .find(|spec| spec.name == "program.exited")
            .expect("the spec is what `mod events` and `F1` print");
        assert_eq!(spec.fields, &["name", "program"]);
        assert!(Event::new("program.exited").only.is_none());
        assert_eq!(
            Event::new("program.exited")
                .to("mods/50_editor.lua")
                .only
                .as_deref(),
            Some("mods/50_editor.lua")
        );
    }

    fn row(id: &str, status: SessionState) -> SessionRow {
        SessionRow {
            id: id.to_string(),
            name: format!("name-{id}"),
            agent: "claude".into(),
            status,
            cwd: Some(PathBuf::from("/src/thurbox")),
            repo: Some("thurbox".into()),
            repos: vec!["thurbox".into()],
            branch: Some(format!("feat/{id}")),
            base_branch: None,
            backend: "local:tmux".into(),
            backend_id: None,
            remote_host: None,
            place: None,
            agent_session_id: None,
            parent_id: None,
            display_order: None,
            worktree_count: 1,
            git: None,
            stopped: false,
            hook_state: None,
            key: None,
            reports_as: None,
            detected_agent: None,
            shell_backend_id: None,
            member_dirs: Vec::new(),
            meta: Default::default(),
            deleting: None,
            state_why: None,
            status_source: None,
        }
    }

    fn snapshot(rows: Vec<SessionRow>) -> Snapshot {
        Snapshot {
            sessions: rows,
            ..Snapshot::default()
        }
    }

    #[test]
    fn the_first_snapshot_seeds_silently() {
        let mut deriver = Deriver::new();
        let events = deriver.observe(
            &snapshot(vec![
                row("a", SessionState::Idle),
                row("b", SessionState::Working),
            ]),
            1,
        );
        assert!(events.is_empty(), "{events:?}");
    }

    #[test]
    fn an_unchanged_version_costs_nothing_and_says_nothing() {
        let mut deriver = Deriver::new();
        deriver.observe(&snapshot(vec![row("a", SessionState::Idle)]), 1);
        assert!(deriver.observe(&snapshot(vec![]), 1).is_empty());
    }

    #[test]
    fn arrivals_departures_and_changes_each_fire_once_in_order() {
        let mut deriver = Deriver::new();
        deriver.observe(
            &snapshot(vec![
                row("a", SessionState::Idle),
                row("b", SessionState::Working),
            ]),
            1,
        );
        let mut renamed = row("a", SessionState::Blocked);
        renamed.name = "renamed".into();
        renamed.branch = Some("other".into());
        let events = deriver.observe(&snapshot(vec![renamed, row("c", SessionState::Idle)]), 2);
        let names: Vec<&str> = events.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "session.deleted",
                "session.created",
                "session.status",
                "session.changed"
            ]
        );
        assert_eq!(events[0].text("session"), Some("b"));
        assert_eq!(events[1].text("session"), Some("c"));
        assert_eq!(events[1].text("repo"), Some("thurbox"));
        assert_eq!(events[2].text("from"), Some("idle"));
        assert_eq!(events[2].text("to"), Some("blocked"));
        assert_eq!(
            events[3]
                .payload
                .iter()
                .find(|(k, _)| k == "fields")
                .map(|(_, v)| v),
            Some(&Field::List(vec!["name".into(), "branch".into()]))
        );
        // And nothing fires again for the same version.
        assert!(deriver.observe(&snapshot(vec![]), 2).is_empty());
    }

    #[test]
    fn a_change_names_each_field_that_moved_and_a_status_alone_is_not_one() {
        let mut deriver = Deriver::new();
        let ids = ["a", "b", "c", "d"];
        deriver.observe(
            &snapshot(ids.iter().map(|id| row(id, SessionState::Idle)).collect()),
            1,
        );
        let mut repos_grew = row("a", SessionState::Idle);
        repos_grew.repos.push("fleet".into());
        let mut reparented = row("b", SessionState::Idle);
        reparented.parent_id = Some("a".into());
        let working = row("c", SessionState::Working);
        let mut repo_moved = row("d", SessionState::Idle);
        repo_moved.repo = Some("elsewhere".into());
        let events = deriver.observe(
            &snapshot(vec![repos_grew, reparented, working, repo_moved]),
            2,
        );
        let fields = |event: &Event| {
            event
                .payload
                .iter()
                .find(|(k, _)| k == "fields")
                .map(|(_, v)| v.clone())
        };
        let summary: Vec<(&str, Option<&str>, Option<Field>)> = events
            .iter()
            .map(|e| (e.name.as_str(), e.text("session"), fields(e)))
            .collect();
        let list =
            |names: &[&str]| Some(Field::List(names.iter().map(|n| n.to_string()).collect()));
        assert_eq!(
            summary,
            [
                ("session.status", Some("c"), None),
                ("session.changed", Some("a"), list(&["repos"])),
                ("session.changed", Some("b"), list(&["parent"])),
                ("session.changed", Some("d"), list(&["repos"])),
            ]
        );
    }

    #[test]
    fn a_reset_makes_the_next_snapshot_seed_again() {
        let mut deriver = Deriver::new();
        deriver.observe(&snapshot(vec![row("a", SessionState::Idle)]), 1);
        deriver.reset();
        assert!(deriver
            .observe(&snapshot(vec![row("b", SessionState::Idle)]), 2)
            .is_empty());
    }

    #[test]
    fn a_subscription_is_a_kernel_name_or_a_user_name() {
        for spec in KERNEL_EVENTS {
            validate_subscription(spec.name).expect(spec.name);
        }
        validate_subscription("user.refresh").expect("user event");
        let error = validate_subscription("sesion.status").unwrap_err();
        assert!(error.contains("sesion.status"), "{error}");
        assert!(error.contains("session.status"), "{error}");
        assert!(validate_subscription("user.").is_err());
        assert!(validate_subscription("user.with space").is_err());
    }

    #[test]
    fn an_emit_is_prefixed_and_a_kernel_name_is_refused() {
        assert_eq!(user_event_name("refresh").unwrap(), "user.refresh");
        assert!(user_event_name("session.created").is_err());
        assert!(user_event_name("user.refresh").is_err());
        assert!(user_event_name("").is_err());
    }

    #[test]
    fn every_kernel_event_documents_its_payload() {
        let mut names = BTreeSet::new();
        for spec in KERNEL_EVENTS {
            assert!(!spec.when.is_empty(), "{}", spec.name);
            assert!(!spec.fields.is_empty(), "{}", spec.name);
            assert!(names.insert(spec.name), "{} listed twice", spec.name);
        }
    }
}
