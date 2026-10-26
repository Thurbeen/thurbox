//! `event_cursors`: one row per consumer of the session event log, and the
//! leases that keep two processes from acting on one subject at once (schema
//! v55, issue #1475).
//!
//! A **cursor** row is a consumer's position in [`super::session_events`] and
//! the short lease one instance of it holds while it reads past that position:
//! claim, read, then advance by compare-and-swap. A failed pass moves nothing,
//! so its rows go out with the next one — at-least-once, a duplicate preferred
//! to a gap. `telemetry` is the first consumer (ADR-37).
//!
//! A **lease** row is the same lease with no position: `session_ops::lock`
//! holds one on a name, a session row or an automation's run for the length of
//! the operation. The expiry is the lease's token, so a holder that overran
//! cannot release its successor's.
//!
//! A table of its own rather than `metadata` keys: the snapshot triggers
//! (schema v49) bump `rows_version` on every `metadata` write, and an advance
//! every minute, or a lock taken and given back, would rebuild the interface's
//! snapshot for nothing.

use rusqlite::params;

use crate::session::SessionId;

use super::Database;

/// The consumer row the telemetry export reads the event log through.
pub const TELEMETRY_CONSUMER: &str = "telemetry";

/// The consumer prefix of every [`LockSubject::Notification`] row.
const NOTIFICATION_PREFIX: &str = "lock:notify:";

/// What a lease is held on — every subject `session_ops::lock` takes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LockSubject {
    /// A name being given to a session within `scope`: a server for a session
    /// name, `key@<host>` for a key.
    Name { scope: String, name: String },
    /// One session row, whatever its name — what a restart holds while it
    /// replaces the row's window.
    Session(SessionId),
    /// One automation's run, held while a fire is in flight so a second fire
    /// of the same automation waits for it.
    AutomationRun(i64),
    /// A `notify send --key`, held for its `--min-interval` after a delivery
    /// so a repeat inside it is dropped. Never released on success: the
    /// expiry is the interval, and a lapsed one is swept by
    /// [`Database::prune_notification_leases`].
    Notification(String),
}

impl LockSubject {
    /// The row's `consumer`. Prefixed so no lock can spell a cursor's name;
    /// schema v55 rewrites the `metadata` claims that came before this table
    /// into these spellings.
    fn consumer(&self) -> String {
        match self {
            LockSubject::Name { scope, name } => format!("lock:name:{scope}:{name}"),
            LockSubject::Session(id) => format!("lock:session:{id}"),
            LockSubject::AutomationRun(id) => format!("lock:run:{id}"),
            LockSubject::Notification(key) => format!("{NOTIFICATION_PREFIX}{key}"),
        }
    }
}

impl Database {
    /// Take `consumer`'s lease until `now_ms + lease_ms`, returning the cursor
    /// to read from — `None` while another instance holds the lease.
    ///
    /// The first claim starts the cursor at the head of the log, so a consumer
    /// turned on today does not replay the history from before it was on.
    pub fn claim_cursor(
        &self,
        consumer: &str,
        now_ms: i64,
        lease_ms: i64,
    ) -> rusqlite::Result<Option<i64>> {
        let tx = self.write_transaction()?;
        self.conn.execute(
            &format!(
                "INSERT OR IGNORE INTO event_cursors (consumer, cursor, lease_until) \
                 VALUES (?1, ({}), 0)",
                super::session_events::LATEST_SEQ
            ),
            [consumer],
        )?;
        let cursor = self
            .conn
            .query_row(
                "UPDATE event_cursors SET lease_until = ?2 \
                 WHERE consumer = ?1 AND lease_until <= ?3 RETURNING cursor",
                params![consumer, now_ms + lease_ms, now_ms],
                |row| row.get(0),
            )
            .map(Some)
            .or_else(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => Ok(None),
                e => Err(e),
            })?;
        tx.commit()?;
        Ok(cursor)
    }

    /// Move `consumer`'s cursor from `from` to `to` and give the lease up.
    /// `false` when the cursor was no longer at `from` — an instance whose
    /// lease ran out lost the rows to the one that took it over.
    pub fn advance_cursor(&self, consumer: &str, from: i64, to: i64) -> rusqlite::Result<bool> {
        let moved = self.conn.execute(
            "UPDATE event_cursors SET cursor = ?3, lease_until = 0 \
             WHERE consumer = ?1 AND cursor = ?2",
            params![consumer, from, to],
        )?;
        Ok(moved == 1)
    }

    /// Give `consumer`'s lease up without moving the cursor, after a failed
    /// pass.
    pub fn release_cursor(&self, consumer: &str) -> rusqlite::Result<()> {
        self.conn.execute(
            "UPDATE event_cursors SET lease_until = 0 WHERE consumer = ?1",
            [consumer],
        )?;
        Ok(())
    }

    /// Take `subject` until `expires_at`. `true` for the single winner; a
    /// lease still live at `now` is refused.
    ///
    /// One conditional statement, which is what makes it a claim rather than a
    /// look followed by a write. An expired lease is taken over rather than
    /// waited on forever, so a holder that died mid-operation is recoverable.
    pub fn take_lease(
        &self,
        subject: &LockSubject,
        expires_at: u64,
        now: u64,
    ) -> rusqlite::Result<bool> {
        let taken = self.conn.execute(
            "INSERT INTO event_cursors (consumer, cursor, lease_until) VALUES (?1, 0, ?2) \
             ON CONFLICT(consumer) DO UPDATE SET lease_until = ?2 \
             WHERE event_cursors.lease_until <= ?3",
            params![subject.consumer(), expires_at as i64, now as i64],
        )?;
        Ok(taken == 1)
    }

    /// Claim a directory only when no live lease in its scope overlaps it.
    /// The write transaction keeps the overlap check and claim atomic.
    pub fn take_directory_lease(
        &self,
        scope: &str,
        path: &std::path::Path,
        expires_at: u64,
        now: u64,
    ) -> rusqlite::Result<bool> {
        let tx = self.write_transaction()?;
        let prefix = format!("lock:name:{scope}:");
        let mut stmt = self.conn.prepare(
            "SELECT substr(consumer, length(?1) + 1) FROM event_cursors \
             WHERE substr(consumer, 1, length(?1)) = ?1 AND lease_until > ?2",
        )?;
        let held = stmt.query_map(params![prefix, now as i64], |row| row.get::<_, String>(0))?;
        for held in held {
            let held = std::path::PathBuf::from(held?);
            if path.starts_with(&held) || held.starts_with(path) {
                return Ok(false);
            }
        }
        drop(stmt);
        let taken = self.take_lease(
            &LockSubject::Name {
                scope: scope.into(),
                name: path.display().to_string(),
            },
            expires_at,
            now,
        )?;
        tx.commit()?;
        Ok(taken)
    }

    pub fn owns_lease(
        &self,
        subject: &LockSubject,
        token: u64,
        now: u64,
    ) -> rusqlite::Result<bool> {
        self.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM event_cursors WHERE consumer = ?1 AND lease_until = ?2 AND lease_until > ?3)",
            params![subject.consumer(), token as i64, now as i64], |r| r.get(0))
    }

    /// Give back the lease taken with `expires_at`, and only that one. A lock
    /// row is deleted rather than zeroed, so the table is bounded by the leases
    /// held, not by every name ever locked.
    pub fn release_lease(&self, subject: &LockSubject, expires_at: u64) -> rusqlite::Result<()> {
        self.conn.execute(
            "DELETE FROM event_cursors WHERE consumer = ?1 AND lease_until = ?2",
            params![subject.consumer(), expires_at as i64],
        )?;
        Ok(())
    }

    /// Give `subject` up whoever holds it: a notification whose delivery
    /// failed, or a failing streak that ended.
    pub fn drop_lease(&self, subject: &LockSubject) -> rusqlite::Result<()> {
        self.conn.execute(
            "DELETE FROM event_cursors WHERE consumer = ?1",
            [subject.consumer()],
        )?;
        Ok(())
    }

    /// Delete the notification leases that lapsed by `now`, so the table is
    /// bounded by the keys still inside their interval rather than by every
    /// key ever sent. A range on the primary key, not a scan.
    pub fn prune_notification_leases(&self, now: u64) -> rusqlite::Result<usize> {
        self.conn.execute(
            "DELETE FROM event_cursors \
             WHERE consumer >= ?1 AND consumer < ?2 AND lease_until <= ?3",
            params![NOTIFICATION_PREFIX, "lock:notify;", now as i64],
        )
    }

    /// Whether somebody holds `subject` at `now`.
    pub fn lease_held(&self, subject: &LockSubject, now: u64) -> rusqlite::Result<bool> {
        self.conn.query_row(
            "SELECT EXISTS (SELECT 1 FROM event_cursors \
             WHERE consumer = ?1 AND lease_until > ?2)",
            params![subject.consumer(), now as i64],
            |row| row.get(0),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync::SharedSession;

    fn rows_version(db: &Database) -> i64 {
        db.conn
            .query_row("SELECT rows_version FROM snapshot_versions", [], |r| {
                r.get(0)
            })
            .unwrap()
    }

    fn name(name: &str) -> LockSubject {
        LockSubject::Name {
            scope: "local:tmux".into(),
            name: name.into(),
        }
    }

    #[test]
    fn the_first_claim_starts_at_the_head_of_the_log() {
        let db = Database::open_in_memory().unwrap();
        db.upsert_session(&SharedSession {
            id: SessionId::default(),
            name: "worker".into(),
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
            key: None,
        })
        .unwrap();
        let head = db.latest_session_event_seq().unwrap();
        assert!(head > 0);
        assert_eq!(db.claim_cursor("hooks", 1_000, 30_000).unwrap(), Some(head));
    }

    #[test]
    fn a_held_lease_turns_a_second_instance_away_until_it_runs_out() {
        let db = Database::open_in_memory().unwrap();
        assert_eq!(db.claim_cursor("hooks", 1_000, 30_000).unwrap(), Some(0));
        assert_eq!(db.claim_cursor("hooks", 2_000, 30_000).unwrap(), None);
        assert_eq!(
            db.claim_cursor(TELEMETRY_CONSUMER, 2_000, 30_000).unwrap(),
            Some(0),
            "each consumer has a lease of its own"
        );
        assert_eq!(db.claim_cursor("hooks", 31_000, 30_000).unwrap(), Some(0));
    }

    #[test]
    fn the_cursor_moves_only_from_where_it_was() {
        let db = Database::open_in_memory().unwrap();
        db.claim_cursor("hooks", 1_000, 30_000).unwrap();
        assert!(db.advance_cursor("hooks", 0, 5).unwrap());
        assert!(!db.advance_cursor("hooks", 0, 9).unwrap());
        assert_eq!(db.claim_cursor("hooks", 2_000, 30_000).unwrap(), Some(5));
    }

    #[test]
    fn a_release_keeps_the_cursor_and_frees_the_lease() {
        let db = Database::open_in_memory().unwrap();
        db.claim_cursor("hooks", 1_000, 30_000).unwrap();
        db.release_cursor("hooks").unwrap();
        assert_eq!(db.claim_cursor("hooks", 2_000, 30_000).unwrap(), Some(0));
    }

    #[test]
    fn a_lease_has_one_holder_until_it_expires_or_is_released() {
        let db = Database::open_in_memory().unwrap();
        assert!(db.take_lease(&name("build"), 100, 10).unwrap());
        assert!(!db.take_lease(&name("build"), 200, 10).unwrap());
        assert!(db.take_lease(&name("other"), 200, 10).unwrap());
        assert!(db.lease_held(&name("build"), 99).unwrap());
        assert!(!db.lease_held(&name("build"), 100).unwrap());

        assert!(db.take_lease(&name("build"), 300, 100).unwrap());
        db.release_lease(&name("build"), 100).unwrap();
        assert!(
            db.lease_held(&name("build"), 150).unwrap(),
            "the overrun holder's release leaves its successor's lease alone"
        );
        db.release_lease(&name("build"), 300).unwrap();
        assert!(!db.lease_held(&name("build"), 150).unwrap());
    }

    #[test]
    fn a_name_a_row_and_a_run_are_different_subjects() {
        let db = Database::open_in_memory().unwrap();
        let id = SessionId::default();
        assert!(db.take_lease(&LockSubject::Session(id), 100, 10).unwrap());
        assert!(db.take_lease(&name(&id.to_string()), 100, 10).unwrap());
        assert!(db
            .take_lease(&LockSubject::AutomationRun(7), 100, 10)
            .unwrap());
        assert!(!db
            .take_lease(&LockSubject::AutomationRun(7), 100, 10)
            .unwrap());
    }

    #[test]
    fn a_notification_lease_is_dropped_by_anyone_and_swept_once_lapsed() {
        let db = Database::open_in_memory().unwrap();
        let key = |k: &str| LockSubject::Notification(k.into());
        assert!(db.take_lease(&key("a"), 100, 10).unwrap());
        assert!(!db.take_lease(&key("a"), 100, 10).unwrap());
        db.drop_lease(&key("a")).unwrap();
        assert!(db.take_lease(&key("a"), 100, 10).unwrap());

        assert!(db.take_lease(&key("b"), 50, 10).unwrap());
        assert!(db.take_lease(&name("build"), 50, 10).unwrap());
        assert_eq!(db.prune_notification_leases(60).unwrap(), 1);
        assert!(db.lease_held(&key("a"), 60).unwrap(), "inside its interval");
        assert!(
            db.lease_held(&name("build"), 40).unwrap(),
            "the sweep leaves other leases alone"
        );
    }

    #[test]
    fn cursors_and_leases_rebuild_nothing() {
        let db = Database::open_in_memory().unwrap();
        let before = rows_version(&db);
        db.claim_cursor(TELEMETRY_CONSUMER, 1_000, 30_000).unwrap();
        db.advance_cursor(TELEMETRY_CONSUMER, 0, 1).unwrap();
        assert!(db.take_lease(&name("build"), 100, 10).unwrap());
        db.release_lease(&name("build"), 100).unwrap();
        assert_eq!(rows_version(&db), before);
    }
}
