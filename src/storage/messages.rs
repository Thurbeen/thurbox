//! Persistence + delivery for [`SessionMessage`]s — the inter-session mailbox.
//!
//! A message is addressed **to** a session (the recipient drains its inbox) and
//! optionally carries provenance (`from_session_id`, `from_task_id`). Delivery is
//! **exactly-once**: [`claim_messages`](Database::claim_messages) selects the
//! unread tail and marks it read in a single transaction, so the TUI and a cron
//! tick can race without double-processing or losing a message. A native send
//! into the agent's own inbox holds a short lease
//! ([`lease_message_delivery`](Database::lease_message_delivery)) that `claim`
//! skips, so a drain cannot hand over a body that is mid-send either. The lease
//! lapses rather than marking the row read, so a sender killed mid-send delays
//! a message but never hides it. Growth is bounded by a per-recipient unread
//! cap on enqueue plus the time-based
//! [`prune_messages`](Database::prune_messages) retention sweep.
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

/// How long a native send may hold a message out of `claim` (see
/// [`Database::lease_message_delivery`]). Comfortably above the slowest send
/// (a `codex queue` is given 20 s), so a live send is never overtaken by a
/// drain, and short enough that a sender killed mid-send delays the message
/// by a minute rather than hiding it.
pub const DELIVERY_LEASE_MS: i64 = 60_000;

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
    /// per-recipient unread cap atomically as part of the insert.
    ///
    /// The cap check and the insert are a **single** `INSERT … SELECT … WHERE`
    /// statement: the row is written only if the recipient's unread count is
    /// still below [`MAX_UNREAD_PER_RECIPIENT`] *at insert time*. SQLite
    /// serializes writers, so two concurrent senders can't both pass the cap and
    /// both insert (the TOCTOU a separate count-then-insert would allow). A zero
    /// row-count means the guard rejected it → [`EnqueueError::InboxFull`].
    pub fn enqueue_message(&self, new: &NewMessage) -> Result<i64, EnqueueError> {
        validate_kind_body(&new.kind, &new.body).map_err(EnqueueError::Invalid)?;

        let now = current_time_millis() as i64;
        let inserted = self.conn.execute(
            "INSERT INTO session_messages
                (to_session_id, from_session_id, from_task_id, kind, body, created_at, read_at)
             SELECT ?1, ?2, ?3, ?4, ?5, ?6, NULL
             WHERE (
                 SELECT count(*) FROM session_messages
                 WHERE to_session_id = ?1 AND read_at IS NULL
             ) < ?7",
            params![
                new.to_session_id.to_string(),
                new.from_session_id.map(|id| id.to_string()),
                new.from_task_id,
                new.kind,
                new.body,
                now,
                MAX_UNREAD_PER_RECIPIENT as i64,
            ],
        )?;
        if inserted == 0 {
            return Err(EnqueueError::InboxFull {
                cap: MAX_UNREAD_PER_RECIPIENT,
            });
        }
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

    /// Fetch a single message by id (read or unread), if it exists. Used by the
    /// `reply` path to look up the original sender.
    pub fn get_message(&self, id: i64) -> rusqlite::Result<Option<SessionMessage>> {
        let sql = format!("SELECT {COLS} FROM session_messages WHERE id = ?1");
        let mut stmt = self.conn.prepare(&sql)?;
        let mut rows = stmt.query_map(params![id], map_message)?;
        rows.next().transpose()
    }

    /// Atomically claim (drain) up to `limit` unread messages for a recipient:
    /// mark the oldest unread read and return exactly those, in a **single**
    /// `UPDATE … RETURNING` statement. SQLite serializes writers, so the
    /// `read_at IS NULL` sub-select can never hand the same row to two concurrent
    /// claimers — exactly-once delivery across the TUI and a cron tick. A
    /// second claim returns the next batch (or nothing), never a repeat.
    pub fn claim_messages(
        &self,
        for_session: SessionId,
        limit: Option<usize>,
    ) -> rusqlite::Result<Vec<SessionMessage>> {
        let limit = limit.unwrap_or(DEFAULT_INBOX_LIMIT) as i64;
        let now = current_time_millis() as i64;
        // Claiming a row whose lease lapsed also voids that lease, so its
        // stalled holder cannot record a delivery of a row a drain now owns.
        let sql = format!(
            "UPDATE session_messages \
             SET read_at = ?3, delivering_at = NULL, delivery_lease = NULL \
             WHERE id IN ( \
                SELECT id FROM session_messages \
                WHERE to_session_id = ?1 AND read_at IS NULL \
                  AND (delivering_at IS NULL OR delivering_at < ?4) \
                ORDER BY id ASC LIMIT ?2 \
             ) \
             RETURNING {COLS}"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let lapsed = now - DELIVERY_LEASE_MS;
        let rows = stmt.query_map(
            params![for_session.to_string(), limit, now, lapsed],
            map_message,
        )?;
        let mut claimed: Vec<SessionMessage> = rows.collect::<rusqlite::Result<_>>()?;
        // RETURNING does not guarantee row order; restore oldest-first.
        claimed.sort_by_key(|m| m.id);
        Ok(claimed)
    }

    /// Take the delivery lease on an unread message before a native send, so a
    /// concurrent drain (`inbox --claim`) skips it rather than handing the same
    /// body over while the send is in flight. Returns the lease's token, or
    /// `None` when the message is no longer deliverable: already read, or
    /// another sender holds a live lease.
    ///
    /// A lease, not a read mark: a sender killed mid-send cannot release it,
    /// and a read mark would then hide a message that never arrived. The lease
    /// lapses after [`DELIVERY_LEASE_MS`] and the row is an ordinary unread one
    /// again. Every later call names the token, so a sender whose lease lapsed
    /// and was taken over cannot complete or release someone else's.
    pub fn lease_message_delivery(&self, id: i64) -> rusqlite::Result<Option<String>> {
        let token = uuid::Uuid::new_v4().to_string();
        let now = current_time_millis() as i64;
        let changed = self.conn.execute(
            "UPDATE session_messages SET delivering_at = ?2, delivery_lease = ?3 \
             WHERE id = ?1 AND read_at IS NULL \
               AND (delivering_at IS NULL OR delivering_at < ?4)",
            params![id, now, token, now - DELIVERY_LEASE_MS],
        )?;
        Ok((changed == 1).then_some(token))
    }

    /// Restart the lease clock before each send attempt, so every attempt
    /// begins with a full [`DELIVERY_LEASE_MS`]. `false` means the lease lapsed
    /// and was taken, or the row was read: the caller must not send.
    pub fn renew_message_delivery(&self, id: i64, token: &str) -> rusqlite::Result<bool> {
        let changed = self.conn.execute(
            "UPDATE session_messages SET delivering_at = ?3 \
             WHERE id = ?1 AND delivery_lease = ?2 AND read_at IS NULL",
            params![id, token, current_time_millis() as i64],
        )?;
        Ok(changed == 1)
    }

    /// Close the lease after the native send succeeded: mark the row read with
    /// the inbox that carried it, so no later drain repeats the body. `false`
    /// when `token` no longer holds the lease.
    pub fn complete_message_delivery(
        &self,
        id: i64,
        token: &str,
        via: &str,
    ) -> rusqlite::Result<bool> {
        let changed = self.conn.execute(
            "UPDATE session_messages \
             SET read_at = COALESCE(read_at, ?3), delivered_via = ?4, \
                 delivering_at = NULL, delivery_lease = NULL \
             WHERE id = ?1 AND delivery_lease = ?2",
            params![id, token, current_time_millis() as i64, via],
        )?;
        Ok(changed == 1)
    }

    /// Drop the lease after the native send failed: the row is an ordinary
    /// unread message again, for the next drain. A no-op for a lease `token`
    /// no longer holds.
    pub fn release_message_delivery(&self, id: i64, token: &str) -> rusqlite::Result<()> {
        self.conn.execute(
            "UPDATE session_messages SET delivering_at = NULL, delivery_lease = NULL \
             WHERE id = ?1 AND delivery_lease = ?2",
            params![id, token],
        )?;
        Ok(())
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
    created_at, read_at, delivered_via";

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
        delivered_via: row.get(8)?,
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
    fn concurrent_enqueue_never_exceeds_cap() {
        // Several Database connections (file-backed shared store) hammer
        // enqueue_message past the cap from N threads. The atomic
        // INSERT … SELECT … WHERE guard must keep total inserts ≤ cap — a
        // separate count-then-insert would let racing writers overshoot it.
        use std::sync::Arc;

        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("messages.db");
        // Materialize the schema before the threads race on it.
        let writer = Database::open(&path).unwrap();
        let to = SessionId::default();

        const THREADS: usize = 8;
        // Each thread attempts more than its share of the cap, so collectively
        // they greatly overshoot and the guard has to reject the excess.
        const PER_THREAD: usize = MAX_UNREAD_PER_RECIPIENT / 4;

        let path = Arc::new(path);
        let mut handles = Vec::new();
        for _ in 0..THREADS {
            let path = Arc::clone(&path);
            handles.push(std::thread::spawn(move || {
                let db = Database::open(path.as_ref()).unwrap();
                let mut ok = 0usize;
                for i in 0..PER_THREAD {
                    match db.enqueue_message(&new_msg(to, "note", &format!("m{i}"))) {
                        Ok(_) => ok += 1,
                        Err(EnqueueError::InboxFull { .. }) => {}
                        Err(e) => panic!("unexpected enqueue error: {e}"),
                    }
                }
                ok
            }));
        }
        let total_ok: usize = handles.into_iter().map(|h| h.join().unwrap()).sum();

        // The cap is the ceiling: never exceeded, regardless of races.
        assert_eq!(
            writer.count_unread_messages(to).unwrap(),
            MAX_UNREAD_PER_RECIPIENT,
            "inbox filled exactly to the cap"
        );
        // Every successful enqueue is accounted for by a real unread row.
        assert_eq!(
            total_ok, MAX_UNREAD_PER_RECIPIENT,
            "successful enqueues equal the cap — none lost, none overshot"
        );
    }

    /// One claimer in [`concurrent_claims_deliver_each_message_exactly_once`]:
    /// claim until the inbox is drained, recording every id and counting any
    /// claimed twice.
    fn drain_inbox(
        path: &std::path::Path,
        to: SessionId,
        seen: &std::sync::Mutex<std::collections::HashSet<i64>>,
        dupes: &std::sync::Mutex<usize>,
    ) {
        let db = Database::open(path).unwrap();
        loop {
            let batch = db.claim_messages(to, Some(3)).unwrap();
            if batch.is_empty() {
                // Could be a transient empty between other threads' batches;
                // confirm the inbox is actually drained before giving up.
                if db.count_unread_messages(to).unwrap() == 0 {
                    return;
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
                drain_inbox(&path, to, &seen, &dupes);
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
    fn a_completed_delivery_is_read_and_never_drained() {
        let db = Database::open_in_memory().unwrap();
        let to = SessionId::default();
        let id = db.enqueue_message(&new_msg(to, "note", "hi")).unwrap();

        let lease = db.lease_message_delivery(id).unwrap().unwrap();
        assert!(db.renew_message_delivery(id, &lease).unwrap());
        assert!(db
            .complete_message_delivery(id, &lease, "claude-socket")
            .unwrap());
        let m = db.get_message(id).unwrap().unwrap();
        assert!(!m.is_unread());
        assert_eq!(m.delivered_via.as_deref(), Some("claude-socket"));
        assert!(db.claim_messages(to, None).unwrap().is_empty());
        // Already delivered: not leasable a second time.
        assert_eq!(db.lease_message_delivery(id).unwrap(), None);
    }

    #[test]
    fn a_drain_skips_a_message_mid_send_and_gets_it_back_on_release() {
        let db = Database::open_in_memory().unwrap();
        let to = SessionId::default();
        let id = db.enqueue_message(&new_msg(to, "note", "hi")).unwrap();
        let other = db.enqueue_message(&new_msg(to, "note", "other")).unwrap();

        let lease = db.lease_message_delivery(id).unwrap().unwrap();
        assert_eq!(
            db.lease_message_delivery(id).unwrap(),
            None,
            "one sender at a time"
        );
        // Mid-send: a drain takes only what is not in flight.
        let claimed = db.claim_messages(to, None).unwrap();
        assert_eq!(
            claimed.iter().map(|m| m.id).collect::<Vec<_>>(),
            vec![other]
        );
        // Still unread, so it is counted and shown while in flight.
        assert_eq!(db.count_unread_messages(to).unwrap(), 1);

        db.release_message_delivery(id, &lease).unwrap();
        let claimed = db.claim_messages(to, None).unwrap();
        assert_eq!(claimed.iter().map(|m| m.id).collect::<Vec<_>>(), vec![id]);
        assert_eq!(claimed[0].delivered_via, None);
    }

    /// Age a lease past [`DELIVERY_LEASE_MS`], as a sender that stalled or was
    /// killed would leave it.
    fn lapse(db: &Database, id: i64) {
        let stale = current_time_millis() as i64 - DELIVERY_LEASE_MS - 1;
        db.conn_ref()
            .execute(
                "UPDATE session_messages SET delivering_at = ?2 WHERE id = ?1",
                params![id, stale],
            )
            .unwrap();
    }

    #[test]
    fn a_lapsed_lease_is_drained_like_any_unread_message() {
        let db = Database::open_in_memory().unwrap();
        let to = SessionId::default();
        let id = db.enqueue_message(&new_msg(to, "note", "hi")).unwrap();
        db.lease_message_delivery(id).unwrap().unwrap();
        lapse(&db, id);
        assert_eq!(db.claim_messages(to, None).unwrap().len(), 1);
    }

    #[test]
    fn a_sender_whose_lease_was_taken_over_cannot_touch_the_new_one() {
        let db = Database::open_in_memory().unwrap();
        let to = SessionId::default();
        let id = db.enqueue_message(&new_msg(to, "note", "hi")).unwrap();
        let stalled = db.lease_message_delivery(id).unwrap().unwrap();
        lapse(&db, id);
        let current = db.lease_message_delivery(id).unwrap().unwrap();

        // The stalled sender may neither send again, finish, nor let go of
        // the lease it no longer holds.
        assert!(!db.renew_message_delivery(id, &stalled).unwrap());
        assert!(!db
            .complete_message_delivery(id, &stalled, "codex-queue")
            .unwrap());
        db.release_message_delivery(id, &stalled).unwrap();
        assert!(db.get_message(id).unwrap().unwrap().is_unread());
        assert!(
            db.claim_messages(to, None).unwrap().is_empty(),
            "still leased"
        );

        assert!(db
            .complete_message_delivery(id, &current, "claude-socket")
            .unwrap());
        let m = db.get_message(id).unwrap().unwrap();
        assert_eq!(m.delivered_via.as_deref(), Some("claude-socket"));
    }

    #[test]
    fn a_claimed_message_cannot_be_leased_or_renewed() {
        let db = Database::open_in_memory().unwrap();
        let to = SessionId::default();
        let id = db.enqueue_message(&new_msg(to, "note", "hi")).unwrap();
        let lease = db.lease_message_delivery(id).unwrap().unwrap();
        lapse(&db, id);
        db.claim_messages(to, None).unwrap();
        assert!(!db.renew_message_delivery(id, &lease).unwrap());
        assert_eq!(db.lease_message_delivery(id).unwrap(), None);
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
