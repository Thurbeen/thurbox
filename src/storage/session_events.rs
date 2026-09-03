//! The append-only session event log — what `thurbox-cli watch` streams.
//!
//! Every writer that changes what a watcher would report appends one row here
//! **in the same transaction as the change it describes**. That is the whole
//! point: the previous `watch` sampled the session table every 250 ms and
//! diffed it, so two transitions inside one sample collapsed into one — a
//! `working → blocked → working` around an auto-answered permission arrived as
//! nothing at all, and a driver never learned the permission had been asked.
//! A log written by the writer cannot lose a transition, whichever process
//! made it.
//!
//! The log is a stream, not a record: it is pruned on the same retention as
//! [`crate::storage::audit`] (`settings.toml` `audit_retention_days`), and a
//! consumer that wants history reads the audit log instead.

use rusqlite::params;

use crate::session::SessionId;
use crate::sync::current_time_millis;

use super::Database;

const MS_PER_DAY: u64 = 24 * 60 * 60 * 1000;

/// What happened to a session row — the vocabulary `watch` has always emitted.
///
/// `present` is not here: it is the baseline `watch --initial` synthesises from
/// the current rows, never something that *happened*.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionEventKind {
    /// A row a watcher had not seen before: spawned, registered, or restored.
    Created,
    /// A row whose watched facts moved.
    Changed,
    /// A row that left the active set.
    Gone,
}

impl SessionEventKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Created => "created",
            Self::Changed => "changed",
            Self::Gone => "gone",
        }
    }
}

/// Why an event happened — the field that stops a driver needing a follow-up
/// `session get` to tell two identically-named events apart.
///
/// `gone` in particular used to be one word for both deletes, and the two are
/// not the same fact: a soft-deleted session can be restored, a force-deleted
/// one has had its worktrees and any uncommitted work torn down.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventReason {
    /// `created`: thurbox launched it.
    Spawned,
    /// `created`: an already-running session was adopted into this database
    /// (`session register`, or a mirror pass adopting a shared host's row).
    Registered,
    /// `created`: a soft-deleted row came back.
    Restored,
    /// `changed`: the agent's reported state moved (`from_state` → `to_state`).
    State,
    /// `changed`: parked by `session stop` — the pane is gone, the row stands.
    Stopped,
    /// `changed`: un-parked by `session start`.
    Started,
    /// `changed`: an identifying fact moved (the name, or the pane the row
    /// points at).
    Updated,
    /// `gone`: soft-deleted, and so restorable.
    SoftDeleted,
    /// `gone`: hard-deleted — worktrees and window torn down, not restorable.
    ForceDeleted,
}

impl EventReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Spawned => "spawned",
            Self::Registered => "registered",
            Self::Restored => "restored",
            Self::State => "state",
            Self::Stopped => "stopped",
            Self::Started => "started",
            Self::Updated => "updated",
            Self::SoftDeleted => "soft_deleted",
            Self::ForceDeleted => "force_deleted",
        }
    }
}

/// One row of the log, as `watch` reads it back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionEventRow {
    /// Monotonic, never reused — what `watch --since` resumes from.
    pub seq: i64,
    pub session_id: SessionId,
    pub event: String,
    pub reason: String,
    /// The agent state before and after, for a `changed`/`state` event. Both
    /// `None` where the event is not about the agent's state.
    pub from_state: Option<String>,
    pub to_state: Option<String>,
    pub at_ms: i64,
}

impl Database {
    /// Append one event. Call it from inside the writer's own transaction, so
    /// the event and the change it describes commit together or not at all.
    pub(super) fn record_session_event(
        &self,
        id: SessionId,
        event: SessionEventKind,
        reason: EventReason,
        from_state: Option<&str>,
        to_state: Option<&str>,
    ) -> rusqlite::Result<()> {
        let mut stmt = self.conn.prepare_cached(
            "INSERT INTO session_events \
             (session_id, event, reason, from_state, to_state, at_ms) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        )?;
        stmt.execute(params![
            id.to_string(),
            event.as_str(),
            reason.as_str(),
            from_state,
            to_state,
            current_time_millis() as i64,
        ])?;
        Ok(())
    }

    /// The highest seq in the log, or `0` when it is empty. The baseline a
    /// watcher starts from when it was not given a `--since`.
    pub fn latest_session_event_seq(&self) -> rusqlite::Result<i64> {
        self.conn.query_row(
            "SELECT COALESCE(MAX(seq), 0) FROM session_events",
            [],
            |r| r.get(0),
        )
    }

    /// Events after `since`, oldest first, optionally for one session.
    ///
    /// `limit` bounds one read, not the stream: a caller that gets `limit` rows
    /// back asks again from the last seq it saw.
    pub fn session_events_since(
        &self,
        since: i64,
        only: Option<SessionId>,
        limit: usize,
    ) -> rusqlite::Result<Vec<SessionEventRow>> {
        // One constant statement for both shapes — a NULL `?3` means "every
        // session" — so `prepare_cached` compiles it once and the filter is
        // always a bound value.
        let mut stmt = self.conn.prepare_cached(
            "SELECT seq, session_id, event, reason, from_state, to_state, at_ms \
             FROM session_events \
             WHERE seq > ?1 AND (?3 IS NULL OR session_id = ?3) \
             ORDER BY seq LIMIT ?2",
        )?;
        let rows = stmt.query_map(
            params![since, limit as i64, only.map(|id| id.to_string())],
            |row| {
                let id: String = row.get(1)?;
                Ok((
                    id,
                    SessionEventRow {
                        seq: row.get(0)?,
                        session_id: SessionId::default(),
                        event: row.get(2)?,
                        reason: row.get(3)?,
                        from_state: row.get(4)?,
                        to_state: row.get(5)?,
                        at_ms: row.get(6)?,
                    },
                ))
            },
        )?;
        let mut out = Vec::new();
        for row in rows {
            let (id, mut event) = row?;
            // An unparseable id is a row no consumer can act on; skipping it
            // beats failing the whole read.
            if let Ok(id) = id.parse::<SessionId>() {
                event.session_id = id;
                out.push(event);
            }
        }
        Ok(out)
    }

    /// Drop events older than the audit log's retention (`settings.toml`
    /// `audit_retention_days`). Returns the number of rows removed. Cheap
    /// thanks to `idx_session_events_at`.
    pub fn prune_session_events(&self) -> rusqlite::Result<usize> {
        let retention_days = crate::session::settings::global().audit_retention_days;
        let cutoff = current_time_millis().saturating_sub(retention_days * MS_PER_DAY);
        self.conn.execute(
            "DELETE FROM session_events WHERE at_ms < ?1",
            params![cutoff as i64],
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync::SharedSession;

    fn row(name: &str) -> SharedSession {
        SharedSession {
            id: SessionId::default(),
            name: name.into(),
            agent: "claude".into(),
            backend_id: "%1".into(),
            backend_type: "local-tmux".into(),
            agent_session_id: None,
            cwd: None,
            additional_dirs: Vec::new(),
            worktrees: Vec::new(),
            shell_backend_id: None,
            parent_session_id: None,
            display_order: None,
            tombstone: false,
            tombstone_at: None,
        }
    }

    #[test]
    fn seq_orders_two_writes_in_the_same_millisecond() {
        let db = Database::open_in_memory().unwrap();
        let session = row("worker");
        db.upsert_session(&session).unwrap();
        db.set_hook_state(session.id, "blocked").unwrap();
        db.set_hook_state(session.id, "working").unwrap();

        let events = db.session_events_since(0, None, 100).unwrap();
        let seen: Vec<(&str, &str)> = events
            .iter()
            .map(|e| (e.event.as_str(), e.reason.as_str()))
            .collect();
        assert_eq!(
            seen,
            vec![
                ("created", "spawned"),
                ("changed", "state"),
                ("changed", "state"),
            ]
        );
        assert!(events[0].seq < events[1].seq && events[1].seq < events[2].seq);
        assert_eq!(events[2].from_state.as_deref(), Some("blocked"));
        assert_eq!(events[2].to_state.as_deref(), Some("working"));
    }

    #[test]
    fn since_resumes_without_replaying() {
        let db = Database::open_in_memory().unwrap();
        let session = row("worker");
        db.upsert_session(&session).unwrap();
        let head = db.latest_session_event_seq().unwrap();
        db.set_hook_state(session.id, "done").unwrap();

        let events = db.session_events_since(head, None, 100).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].to_state.as_deref(), Some("done"));
    }

    #[test]
    fn events_filter_to_one_session() {
        let db = Database::open_in_memory().unwrap();
        let mine = row("mine");
        let theirs = row("theirs");
        db.upsert_session(&mine).unwrap();
        db.upsert_session(&theirs).unwrap();

        let events = db.session_events_since(0, Some(mine.id), 100).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].session_id, mine.id);
    }

    #[test]
    fn prune_removes_only_events_past_retention() {
        let db = Database::open_in_memory().unwrap();
        let session = row("worker");
        db.upsert_session(&session).unwrap();

        let retention_days = crate::session::settings::global().audit_retention_days;
        let stale = current_time_millis() - (retention_days + 1) * MS_PER_DAY;
        db.conn_ref()
            .execute(
                "UPDATE session_events SET at_ms = ?1",
                params![stale as i64],
            )
            .unwrap();
        db.set_hook_state(session.id, "working").unwrap();

        assert_eq!(db.prune_session_events().unwrap(), 1);
        assert_eq!(db.session_events_since(0, None, 100).unwrap().len(), 1);
    }
}
