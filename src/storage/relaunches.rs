//! The crash-loop guard's state: a session's recent automatic relaunches, and
//! whether it is held back from more (`session_ops::restart::crash_looping`).
//!
//! Kept apart from [`super::session_events`] on purpose. That log is a stream
//! pruned on the audit retention, every time a database is opened — and a
//! retention of zero is valid — so a guard counted from it forgets the loop it
//! was holding back. Here a relaunch is dropped only once it has aged out of
//! the window that counts it, and a hold only when a pane is recorded for the
//! session again ([`Database::claim_backend_id`]).

use rusqlite::{params, OptionalExtension};

use crate::session::SessionId;
use crate::sync::current_time_millis;

use super::session_events::{EventReason, SessionEventKind};
use super::Database;

impl Database {
    /// Record an automatic relaunch of `id`, and the `changed`/`relaunched`
    /// event that reports it. Relaunches of any session older than
    /// `forget_before_ms` are dropped: nothing counts them any more.
    pub fn record_relaunch(&self, id: SessionId, forget_before_ms: i64) -> rusqlite::Result<()> {
        let tx = self.write_transaction()?;
        self.conn.execute(
            "DELETE FROM session_relaunches WHERE at_ms < ?1",
            params![forget_before_ms],
        )?;
        self.conn.execute(
            "INSERT INTO session_relaunches (session_id, at_ms) VALUES (?1, ?2)",
            params![id.to_string(), current_time_millis() as i64],
        )?;
        self.record_session_event(
            id,
            SessionEventKind::Changed,
            EventReason::Relaunched,
            None,
            None,
        )?;
        tx.commit()
    }

    /// How many automatic relaunches `id` has had since `since_ms`.
    pub fn relaunches_since(&self, id: SessionId, since_ms: i64) -> rusqlite::Result<usize> {
        self.conn
            .query_row(
                "SELECT COUNT(*) FROM session_relaunches WHERE session_id = ?1 AND at_ms >= ?2",
                params![id.to_string(), since_ms],
                |r| r.get::<_, i64>(0),
            )
            .map(|n| n as usize)
    }

    /// Hold `id` back from automatic relaunches until a pane is recorded for
    /// it again. Holding a held session again keeps the first mark.
    pub fn hold_relaunches(&self, id: SessionId) -> rusqlite::Result<()> {
        self.conn.execute(
            "UPDATE sessions SET relaunch_held_at = ?1 \
             WHERE id = ?2 AND relaunch_held_at IS NULL",
            params![current_time_millis() as i64, id.to_string()],
        )?;
        Ok(())
    }

    /// Whether `id` is held back — see [`Self::hold_relaunches`].
    pub fn relaunches_held(&self, id: SessionId) -> rusqlite::Result<bool> {
        let at: Option<Option<i64>> = self
            .conn
            .query_row(
                "SELECT relaunch_held_at FROM sessions WHERE id = ?1",
                params![id.to_string()],
                |r| r.get(0),
            )
            .optional()?;
        Ok(at.flatten().is_some())
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
            backend_type: "local:psmux".into(),
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
    fn relaunches_are_counted_per_session_and_logged() {
        let db = Database::open_in_memory().unwrap();
        let (mine, theirs) = (row("mine"), row("theirs"));
        db.upsert_session(&mine).unwrap();
        db.upsert_session(&theirs).unwrap();
        let head = db.latest_session_event_seq().unwrap();
        db.record_relaunch(mine.id, 0).unwrap();
        db.record_relaunch(mine.id, 0).unwrap();
        db.record_relaunch(theirs.id, 0).unwrap();

        assert_eq!(db.relaunches_since(mine.id, 0).unwrap(), 2);
        assert_eq!(db.relaunches_since(theirs.id, 0).unwrap(), 1);
        assert_eq!(db.relaunches_since(mine.id, i64::MAX).unwrap(), 0);
        let events = db.session_events_since(head, Some(mine.id), 10).unwrap();
        assert!(events.iter().all(|e| e.reason == "relaunched"));
        assert_eq!(events.len(), 2);
    }

    /// The reason this is not counted from the event log: pruning that log
    /// (a zero retention prunes it on every open) must not forget a loop.
    #[test]
    fn pruning_the_event_log_forgets_no_relaunch() {
        let db = Database::open_in_memory().unwrap();
        let session = row("worker");
        db.upsert_session(&session).unwrap();
        db.record_relaunch(session.id, 0).unwrap();
        db.conn_ref()
            .execute("DELETE FROM session_events", [])
            .unwrap();
        assert_eq!(db.relaunches_since(session.id, 0).unwrap(), 1);
    }

    #[test]
    fn a_relaunch_out_of_the_window_is_forgotten() {
        let db = Database::open_in_memory().unwrap();
        let session = row("worker");
        db.upsert_session(&session).unwrap();
        db.record_relaunch(session.id, 0).unwrap();
        let later = current_time_millis() as i64 + 1;
        db.record_relaunch(session.id, later).unwrap();
        let stored: i64 = db
            .conn_ref()
            .query_row("SELECT COUNT(*) FROM session_relaunches", [], |r| r.get(0))
            .unwrap();
        assert_eq!(stored, 1, "only the one inside the window is kept");
    }

    #[test]
    fn a_hold_lasts_until_a_pane_is_recorded() {
        let db = Database::open_in_memory().unwrap();
        let session = row("worker");
        db.upsert_session(&session).unwrap();
        assert!(!db.relaunches_held(session.id).unwrap());

        db.hold_relaunches(session.id).unwrap();
        db.conn_ref()
            .execute("DELETE FROM session_events", [])
            .unwrap();
        assert!(db.relaunches_held(session.id).unwrap());

        db.claim_backend_id(session.id, "%2", |_, _| false).unwrap();
        assert!(!db.relaunches_held(session.id).unwrap());
    }
}
