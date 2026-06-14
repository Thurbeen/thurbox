//! Persistence + delivery for [`SessionMessage`]s — the inter-session mailbox.
//!
//! A message is addressed **to** a session (the recipient drains its inbox) and
//! optionally carries provenance (`from_session_id`, `from_task_id`). Delivery is
//! **exactly-once**: [`claim_messages`](Database::claim_messages) selects the
//! unread tail and marks it read in a single transaction, so the TUI, a cron
//! tick, and a sender's wake-nudge can race without double-processing or losing
//! a message. Growth is bounded by a per-recipient unread cap on enqueue plus the
//! time-based [`prune_messages`](Database::prune_messages) retention sweep.
//!
//! The table is agent-neutral and reusable by any extension; `flow` is its first
//! consumer. Unlike high-value entities, mailbox traffic is **not** audited (it
//! is high-churn and ephemeral).

use rusqlite::params;

use crate::session::message::validate_kind_body;
use crate::session::{SessionId, SessionMessage};
use crate::sync::current_time_millis;

use super::Database;

/// Hard cap on the number of *unread* messages a single recipient may hold.
/// Enqueue is rejected past this, so one sender plus a stuck (never-draining)
/// recipient can't grow the table without bound — backpressure, not silent loss.
pub const MAX_UNREAD_PER_RECIPIENT: usize = 500;

/// Default cap on how many messages a single `list`/`claim` returns when the
/// caller doesn't specify one. Keeps a drain bounded even with a deep backlog.
pub const DEFAULT_INBOX_LIMIT: usize = 100;

/// Retention used by the automatic prune sweep ([`prune_old_messages`](Database::prune_old_messages)):
/// already-read messages older than this are deleted. Unread are kept regardless.
pub const DEFAULT_RETENTION_DAYS: u64 = 14;

const MS_PER_DAY: u64 = 24 * 60 * 60 * 1000;

/// Fields needed to enqueue a message.
pub struct NewMessage {
    pub to_session_id: SessionId,
    pub from_session_id: Option<SessionId>,
    pub from_task_id: Option<i64>,
    pub kind: String,
    pub body: String,
}

/// Why an [`enqueue_message`](Database::enqueue_message) was rejected.
#[derive(Debug)]
pub enum EnqueueError {
    /// `kind`/`body` failed the length/non-empty bounds (carries the reason).
    Invalid(String),
    /// The recipient already holds [`MAX_UNREAD_PER_RECIPIENT`] unread messages.
    InboxFull { cap: usize },
    /// Underlying database error.
    Db(rusqlite::Error),
}

impl std::fmt::Display for EnqueueError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Invalid(why) => write!(f, "{why}"),
            Self::InboxFull { cap } => write!(
                f,
                "recipient inbox is full ({cap} unread); drain it before sending more"
            ),
            Self::Db(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for EnqueueError {}

impl From<rusqlite::Error> for EnqueueError {
    fn from(e: rusqlite::Error) -> Self {
        Self::Db(e)
    }
}

impl Database {
    /// Enqueue a message, returning its row id.
    ///
    /// Validates `kind`/`body` (see
    /// [`MAX_KIND_LEN`](crate::session::message::MAX_KIND_LEN) /
    /// [`MAX_BODY_LEN`](crate::session::message::MAX_BODY_LEN)) and enforces the
    /// per-recipient unread cap before inserting.
    pub fn enqueue_message(&self, new: &NewMessage) -> Result<i64, EnqueueError> {
        validate_kind_body(&new.kind, &new.body).map_err(EnqueueError::Invalid)?;

        let unread = self.count_unread_messages(new.to_session_id)?;
        if unread >= MAX_UNREAD_PER_RECIPIENT {
            return Err(EnqueueError::InboxFull {
                cap: MAX_UNREAD_PER_RECIPIENT,
            });
        }

        let now = current_time_millis() as i64;
        self.conn.execute(
            "INSERT INTO session_messages
                (to_session_id, from_session_id, from_task_id, kind, body, created_at, read_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, NULL)",
            params![
                new.to_session_id.to_string(),
                new.from_session_id.map(|id| id.to_string()),
                new.from_task_id,
                new.kind,
                new.body,
                now,
            ],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Number of unread messages addressed to `for_session`.
    pub fn count_unread_messages(&self, for_session: SessionId) -> rusqlite::Result<usize> {
        let n: i64 = self.conn.query_row(
            "SELECT count(*) FROM session_messages \
             WHERE to_session_id = ?1 AND read_at IS NULL",
            params![for_session.to_string()],
            |row| row.get(0),
        )?;
        Ok(n as usize)
    }

    /// Peek at a recipient's inbox **without** marking anything read. Oldest
    /// first, capped at `limit` (falls back to [`DEFAULT_INBOX_LIMIT`]).
    pub fn list_messages(
        &self,
        for_session: SessionId,
        unread_only: bool,
        limit: Option<usize>,
    ) -> rusqlite::Result<Vec<SessionMessage>> {
        // `condition` is a trusted constant fragment; values are bound params.
        let condition = if unread_only {
            "to_session_id = ?1 AND read_at IS NULL"
        } else {
            "to_session_id = ?1"
        };
        let limit = limit.unwrap_or(DEFAULT_INBOX_LIMIT) as i64;
        let sql = format!(
            "SELECT {COLS} FROM session_messages WHERE {condition} ORDER BY id ASC LIMIT ?2"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(params![for_session.to_string(), limit], map_message)?;
        rows.collect()
    }

    /// Atomically claim (drain) up to `limit` unread messages for a recipient:
    /// mark the oldest unread read and return exactly those, in a **single**
    /// `UPDATE … RETURNING` statement. SQLite serializes writers, so the
    /// `read_at IS NULL` sub-select can never hand the same row to two concurrent
    /// claimers — exactly-once delivery across the TUI, a cron tick, and a wake
    /// nudge. A second claim returns the next batch (or nothing), never a repeat.
    pub fn claim_messages(
        &self,
        for_session: SessionId,
        limit: Option<usize>,
    ) -> rusqlite::Result<Vec<SessionMessage>> {
        let limit = limit.unwrap_or(DEFAULT_INBOX_LIMIT) as i64;
        let now = current_time_millis() as i64;
        let sql = format!(
            "UPDATE session_messages SET read_at = ?3 \
             WHERE id IN ( \
                SELECT id FROM session_messages \
                WHERE to_session_id = ?1 AND read_at IS NULL \
                ORDER BY id ASC LIMIT ?2 \
             ) \
             RETURNING {COLS}"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(params![for_session.to_string(), limit, now], map_message)?;
        let mut claimed: Vec<SessionMessage> = rows.collect::<rusqlite::Result<_>>()?;
        // RETURNING does not guarantee row order; restore oldest-first.
        claimed.sort_by_key(|m| m.id);
        Ok(claimed)
    }

    /// Retention sweep: delete messages older than `older_than_millis`. When
    /// `read_only` is set, only already-read messages are removed (unread stay
    /// regardless of age). Returns the number of rows deleted. Cheap thanks to
    /// `idx_session_messages_created`.
    pub fn prune_messages(
        &self,
        older_than_millis: u64,
        read_only: bool,
    ) -> rusqlite::Result<usize> {
        let cutoff = older_than_millis as i64;
        let sql = if read_only {
            "DELETE FROM session_messages WHERE created_at < ?1 AND read_at IS NOT NULL"
        } else {
            "DELETE FROM session_messages WHERE created_at < ?1"
        };
        self.conn.execute(sql, params![cutoff])
    }

    /// Best-effort retention sweep with the default policy: delete already-read
    /// messages older than [`DEFAULT_RETENTION_DAYS`] (unread are kept). Called
    /// at startup and on each automation tick, mirroring audit-log pruning, so
    /// the table self-bounds without any operator action.
    pub fn prune_old_messages(&self) -> rusqlite::Result<usize> {
        let cutoff = current_time_millis().saturating_sub(DEFAULT_RETENTION_DAYS * MS_PER_DAY);
        self.prune_messages(cutoff, true)
    }
}

/// Column list for message SELECTs (keep in sync with [`map_message`]).
const COLS: &str = "id, to_session_id, from_session_id, from_task_id, kind, body, \
    created_at, read_at";

fn map_message(row: &rusqlite::Row) -> rusqlite::Result<SessionMessage> {
    let to: String = row.get(1)?;
    let from: Option<String> = row.get(2)?;
    Ok(SessionMessage {
        id: row.get(0)?,
        to_session_id: to.parse().unwrap_or_default(),
        from_session_id: from.and_then(|s| s.parse().ok()),
        from_task_id: row.get(3)?,
        kind: row.get(4)?,
        body: row.get(5)?,
        created_at: row.get::<_, i64>(6)? as u64,
        read_at: row.get::<_, Option<i64>>(7)?.map(|v| v as u64),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn new_msg(to: SessionId, kind: &str, body: &str) -> NewMessage {
        NewMessage {
            to_session_id: to,
            from_session_id: None,
            from_task_id: None,
            kind: kind.into(),
            body: body.into(),
        }
    }

    #[test]
    fn enqueue_peek_claim_round_trip() {
        let db = Database::open_in_memory().unwrap();
        let to = SessionId::default();
        let id = db
            .enqueue_message(&new_msg(to, "questions", "q1?"))
            .unwrap();
        assert!(id > 0);

        // Peek does not consume.
        let peek = db.list_messages(to, true, None).unwrap();
        assert_eq!(peek.len(), 1);
        assert_eq!(peek[0].kind, "questions");
        assert!(peek[0].is_unread());
        assert_eq!(db.count_unread_messages(to).unwrap(), 1);

        // Claim consumes exactly once.
        let claimed = db.claim_messages(to, None).unwrap();
        assert_eq!(claimed.len(), 1);
        assert!(!claimed[0].is_unread());
        assert_eq!(db.count_unread_messages(to).unwrap(), 0);
        assert!(db.claim_messages(to, None).unwrap().is_empty());
    }

    #[test]
    fn claim_orders_oldest_first_and_respects_limit() {
        let db = Database::open_in_memory().unwrap();
        let to = SessionId::default();
        for i in 0..5 {
            db.enqueue_message(&new_msg(to, "note", &format!("m{i}")))
                .unwrap();
        }
        let first_two = db.claim_messages(to, Some(2)).unwrap();
        assert_eq!(
            first_two
                .iter()
                .map(|m| m.body.as_str())
                .collect::<Vec<_>>(),
            vec!["m0", "m1"]
        );
        let rest = db.claim_messages(to, Some(10)).unwrap();
        assert_eq!(rest.len(), 3);
        assert_eq!(rest[0].body, "m2");
    }

    #[test]
    fn provenance_round_trips() {
        let db = Database::open_in_memory().unwrap();
        let to = SessionId::default();
        let from = SessionId::default();
        db.enqueue_message(&NewMessage {
            to_session_id: to,
            from_session_id: Some(from),
            from_task_id: Some(7),
            kind: "result".into(),
            body: "{\"status\":\"ok\"}".into(),
        })
        .unwrap();
        let got = db.list_messages(to, false, None).unwrap();
        assert_eq!(got[0].from_session_id, Some(from));
        assert_eq!(got[0].from_task_id, Some(7));
    }

    #[test]
    fn enqueue_validates_and_caps() {
        let db = Database::open_in_memory().unwrap();
        let to = SessionId::default();
        assert!(matches!(
            db.enqueue_message(&new_msg(to, "", "body")),
            Err(EnqueueError::Invalid(_))
        ));
        assert!(matches!(
            db.enqueue_message(&new_msg(to, "k", "")),
            Err(EnqueueError::Invalid(_))
        ));
    }

    #[test]
    fn inbox_full_rejects_past_cap() {
        let db = Database::open_in_memory().unwrap();
        let to = SessionId::default();
        // Insert exactly the cap of unread rows directly (cheap; avoids churning
        // the enqueue path MAX times).
        let now = current_time_millis() as i64;
        for _ in 0..MAX_UNREAD_PER_RECIPIENT {
            db.conn_ref()
                .execute(
                    "INSERT INTO session_messages \
                     (to_session_id, kind, body, created_at) VALUES (?1, 'note', 'x', ?2)",
                    params![to.to_string(), now],
                )
                .unwrap();
        }
        assert!(matches!(
            db.enqueue_message(&new_msg(to, "note", "one too many")),
            Err(EnqueueError::InboxFull { .. })
        ));
        // A different recipient is unaffected.
        assert!(db
            .enqueue_message(&new_msg(SessionId::default(), "note", "ok"))
            .is_ok());
    }

    #[test]
    fn concurrent_claims_deliver_each_message_exactly_once() {
        // A file-based DB so several Database connections share one store, then
        // hammer claim_messages from N threads: every message must land with
        // exactly one claimer — no duplicates, no drops.
        use std::collections::HashSet;
        use std::sync::{Arc, Mutex};

        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("messages.db");

        let writer = Database::open(&path).unwrap();
        let to = SessionId::default();
        const TOTAL: usize = 200;
        for i in 0..TOTAL {
            writer
                .enqueue_message(&new_msg(to, "note", &format!("m{i}")))
                .unwrap();
        }

        let seen = Arc::new(Mutex::new(HashSet::<i64>::new()));
        let dupes = Arc::new(Mutex::new(0usize));
        let mut handles = Vec::new();
        for _ in 0..6 {
            let path = path.clone();
            let seen = Arc::clone(&seen);
            let dupes = Arc::clone(&dupes);
            handles.push(std::thread::spawn(move || {
                let db = Database::open(&path).unwrap();
                loop {
                    let batch = db.claim_messages(to, Some(3)).unwrap();
                    if batch.is_empty() {
                        // Could be a transient empty between other threads' batches;
                        // confirm the inbox is actually drained before giving up.
                        if db.count_unread_messages(to).unwrap() == 0 {
                            break;
                        }
                        continue;
                    }
                    let mut seen = seen.lock().unwrap();
                    let mut dupes = dupes.lock().unwrap();
                    for m in batch {
                        if !seen.insert(m.id) {
                            *dupes += 1;
                        }
                    }
                }
            }));
        }
        for h in handles {
            h.join().unwrap();
        }

        assert_eq!(*dupes.lock().unwrap(), 0, "no message claimed twice");
        assert_eq!(
            seen.lock().unwrap().len(),
            TOTAL,
            "every message claimed exactly once"
        );
        assert_eq!(writer.count_unread_messages(to).unwrap(), 0);
    }

    #[test]
    fn prune_respects_age_and_read_only() {
        let db = Database::open_in_memory().unwrap();
        let to = SessionId::default();
        let now = current_time_millis();
        let insert = |created: u64, read: Option<u64>| {
            db.conn_ref()
                .execute(
                    "INSERT INTO session_messages \
                     (to_session_id, kind, body, created_at, read_at) VALUES (?1, 'note', 'x', ?2, ?3)",
                    params![to.to_string(), created as i64, read.map(|v| v as i64)],
                )
                .unwrap();
        };
        insert(now - 1_000_000, Some(now)); // old + read  → prunable
        insert(now - 1_000_000, None); // old + unread → kept when read_only
        insert(now, Some(now)); // fresh + read → kept (age)

        // read_only: only the old+read row goes.
        assert_eq!(db.prune_messages(now - 500_000, true).unwrap(), 1);
        // The old unread row survives.
        assert_eq!(db.count_unread_messages(to).unwrap(), 1);
        // Non-read_only prune of everything old removes the unread one too.
        assert_eq!(db.prune_messages(now - 500_000, false).unwrap(), 1);
    }
}
