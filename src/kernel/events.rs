//! Events a plugin can subscribe to, and how the kernel's own are derived.
//!
//! A plugin used to learn that the world changed only by being rendered and
//! diffing the snapshot itself — once per frame, in every pane that cared. This
//! is the push: the loop hands each subscriber one call per change, off the
//! render path (`plugin-events`).
//!
//! The kernel's events are **derived by diffing published state**, never raised
//! by the code that mutates it. A session created by `thurbox-cli`, a cron tick
//! or a second interface looks exactly like one the creation flow made, and no
//! mutation site anywhere can forget to fire — the failure mode of the other
//! design, which is also the one that would have put a kernel concern inside
//! `session_ops`, a layer the kernel may not `use`.
//!
//! The set is a closed enumeration ([`KERNEL_EVENTS`]) with one reader for each
//! of its three uses: the loader validates a subscription against it, the help
//! modal renders it, and `thurbox-cli plugin events` prints it. One list, so a
//! name a plugin can subscribe to is always one the kernel can emit.

use std::collections::BTreeSet;

use super::snapshot::{SessionRow, Snapshot};

/// A value carried in an event's payload.
///
/// Deliberately small: a payload is a handful of facts a handler can branch on,
/// not a document. A session's whole row is already readable from the published
/// tables by the id the payload names.
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

/// One thing that happened, ready to be handed to every subscriber.
#[derive(Debug, Clone, PartialEq)]
pub struct Event {
    pub name: String,
    pub payload: Vec<(String, Field)>,
    /// How many handler generations deep this event was emitted.
    ///
    /// Zero for anything the kernel derived. A user event emitted from a handler
    /// is one deeper than the event that handler was running for, which is what
    /// bounds a ping-pong between two plugins — see [`MAX_DEPTH`].
    pub depth: u8,
}

impl Event {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            payload: Vec::new(),
            depth: 0,
        }
    }

    /// Add a field. A `None` is left out rather than published as an empty
    /// string, so a handler can test for presence.
    pub fn with(mut self, key: &str, value: Option<impl Into<Field>>) -> Self {
        if let Some(value) = value {
            self.payload.push((key.to_string(), value.into()));
        }
        self
    }

    /// The text value of a field, for tests and for the loop's own reads.
    pub fn text(&self, key: &str) -> Option<&str> {
        self.payload.iter().find_map(|(k, v)| match v {
            Field::Text(text) if k == key => Some(text.as_str()),
            _ => None,
        })
    }
}

/// How many generations of user events one dispatch may cascade through.
///
/// A handler for `user.a` emits `user.b`, whose handler emits `user.a` again —
/// two plugins can do that to each other without either being wrong on its
/// own. The fifth generation is dropped and reported once, so the loop keeps
/// its frame cadence whatever two plugins agree to do.
pub const MAX_DEPTH: u8 = 4;

/// The prefix every plugin-emitted event carries.
pub const USER_PREFIX: &str = "user.";

/// One event the kernel emits: its name, when it fires, and its payload fields.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EventSpec {
    pub name: &'static str,
    pub when: &'static str,
    pub fields: &'static [&'static str],
}

/// Every event the kernel emits. **Closed**: a subscription to a name not here
/// (and not of the `user.` form) refuses to load.
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
        name: "session.post_restart",
        when: "this interface finished restarting a session",
        fields: &["session", "name", "agent", "repo", "cwd", "branch"],
    },
    EventSpec {
        name: "session.post_restore",
        when: "this interface finished restoring a session",
        fields: &["session", "name", "agent", "repo", "cwd", "branch"],
    },
    EventSpec {
        name: "focus.session",
        when: "the selected session changes",
        fields: &["from", "to"],
    },
    EventSpec {
        name: "focus.pane",
        when: "focus moves to another plugin",
        fields: &["from", "to"],
    },
    EventSpec {
        name: "command.done",
        when: "a command a plugin issued completed",
        fields: &["kind", "session", "subject"],
    },
    EventSpec {
        name: "command.failed",
        when: "a command a plugin issued failed",
        fields: &["kind", "session", "subject", "error"],
    },
    EventSpec {
        name: "interface.reloaded",
        when: "the interface was rebuilt from disk",
        fields: &["reason"],
    },
];

/// Is this a name the kernel emits?
pub fn is_kernel_event(name: &str) -> bool {
    KERNEL_EVENTS.iter().any(|spec| spec.name == name)
}

/// The characters a user event's name may be made of.
fn is_identifier(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
}

/// Check a name a plugin subscribes to.
///
/// Refused rather than ignored: a subscription to a name nothing emits is a
/// handler that never fires, which is the one failure with no symptom. The
/// message names what was available, the way an unknown capability's does.
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

/// The full name a plugin's emit is delivered under, or why it may not be.
///
/// A plugin writes `command("emit", { text = "refresh" })` and subscribers see
/// `user.refresh`. A kernel name is refused outright, and so is one already
/// spelled with the prefix — `user.user.x` would be a name nobody subscribed to.
pub fn user_event_name(name: &str) -> Result<String, String> {
    if is_kernel_event(name) {
        return Err(format!(
            "command \"emit\": {name:?} is a kernel event and cannot be emitted by a plugin"
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

/// The facts about a session the derived events are defined over.
#[derive(Debug, Clone, PartialEq)]
struct Facts {
    name: String,
    agent: String,
    repo: Option<String>,
    repos: Vec<String>,
    status: String,
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
            status: row.status.clone(),
            branch: row.branch.clone(),
            parent: row.parent_id.clone(),
        }
    }
}

/// Turns one snapshot into the events that separate it from the last.
///
/// Keyed on `SnapshotStore::version`, so asking on every iteration costs one
/// integer compare while nothing moves — and the first snapshot after a load
/// **seeds** silently: the rows that existed before a plugin did are not news.
#[derive(Debug, Default)]
pub struct Deriver {
    /// Rows as last observed, in published order.
    rows: Vec<(String, Facts)>,
    version: Option<u64>,
}

impl Deriver {
    pub fn new() -> Self {
        Self::default()
    }

    /// Everything that changed since the last call, in the order a subscriber
    /// should hear it: departures, arrivals, status changes, other changes —
    /// each in published row order.
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
        let new_ids: BTreeSet<&str> = next.iter().map(|(id, _)| id.as_str()).collect();
        for (id, facts) in &self.rows {
            if !new_ids.contains(id.as_str()) {
                events.push(
                    Event::new("session.deleted")
                        .with("session", Some(id.as_str()))
                        .with("name", Some(facts.name.as_str())),
                );
            }
        }
        let old: std::collections::HashMap<&str, &Facts> = self
            .rows
            .iter()
            .map(|(id, facts)| (id.as_str(), facts))
            .collect();
        for (id, facts) in &next {
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
        let mut changes = Vec::new();
        for (id, facts) in &next {
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
            let mut fields = Vec::new();
            if before.name != facts.name {
                fields.push("name".to_string());
            }
            if before.branch != facts.branch {
                fields.push("branch".to_string());
            }
            if before.repo != facts.repo || before.repos != facts.repos {
                fields.push("repos".to_string());
            }
            if before.parent != facts.parent {
                fields.push("parent".to_string());
            }
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
        self.rows = next;
        events
    }

    /// Forget everything, so the next snapshot seeds again.
    ///
    /// For a reload: the plugins that would have heard about the rows since the
    /// last observation no longer exist, and the ones that replaced them are
    /// told `interface.reloaded` instead.
    pub fn reset(&mut self) {
        self.rows.clear();
        self.version = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn row(id: &str, status: &str) -> SessionRow {
        SessionRow {
            id: id.to_string(),
            name: format!("name-{id}"),
            agent: "claude".into(),
            status: status.into(),
            cwd: Some(PathBuf::from("/src/thurbox")),
            repo: Some("thurbox".into()),
            repos: vec!["thurbox".into()],
            branch: Some(format!("feat/{id}")),
            base_branch: None,
            backend: "local-tmux".into(),
            backend_id: None,
            remote_host: None,
            agent_session_id: None,
            parent_id: None,
            display_order: None,
            worktree_count: 1,
            git: None,
            hook_state: None,
            shell_backend_id: None,
            member_dirs: Vec::new(),
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
        let events = deriver.observe(&snapshot(vec![row("a", "idle"), row("b", "working")]), 1);
        assert!(events.is_empty(), "{events:?}");
    }

    #[test]
    fn an_unchanged_version_costs_nothing_and_says_nothing() {
        let mut deriver = Deriver::new();
        deriver.observe(&snapshot(vec![row("a", "idle")]), 1);
        assert!(deriver.observe(&snapshot(vec![]), 1).is_empty());
    }

    #[test]
    fn arrivals_departures_and_changes_each_fire_once_in_order() {
        let mut deriver = Deriver::new();
        deriver.observe(&snapshot(vec![row("a", "idle"), row("b", "working")]), 1);
        let mut renamed = row("a", "blocked");
        renamed.name = "renamed".into();
        renamed.branch = Some("other".into());
        let events = deriver.observe(&snapshot(vec![renamed, row("c", "idle")]), 2);
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
    fn a_reset_makes_the_next_snapshot_seed_again() {
        let mut deriver = Deriver::new();
        deriver.observe(&snapshot(vec![row("a", "idle")]), 1);
        deriver.reset();
        assert!(deriver
            .observe(&snapshot(vec![row("b", "idle")]), 2)
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
