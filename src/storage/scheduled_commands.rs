use rusqlite::params;

use crate::session::{ScheduledCommand, SessionId};
use crate::sync::current_time_millis;

use super::Database;

impl Database {
    /// Insert a scheduled command for a session.
    pub fn create_scheduled_command(
        &self,
        session_id: SessionId,
        command_text: &str,
        scheduled_at: u64,
    ) -> rusqlite::Result<i64> {
        let now = current_time_millis() as i64;
        self.conn.execute(
            "INSERT INTO scheduled_commands (session_id, command_text, scheduled_at, created_at) \
             VALUES (?1, ?2, ?3, ?4)",
            params![
                session_id.to_string(),
                command_text,
                scheduled_at as i64,
                now
            ],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Fetch all due scheduled commands (scheduled_at <= now, not executed or cancelled).
    pub fn due_scheduled_commands(&self) -> rusqlite::Result<Vec<ScheduledCommand>> {
        let now = current_time_millis() as i64;
        let mut stmt = self.conn.prepare(
            "SELECT id, session_id, command_text, scheduled_at, created_at, executed_at, \
             cancelled_at FROM scheduled_commands \
             WHERE scheduled_at <= ?1 AND executed_at IS NULL AND cancelled_at IS NULL \
             ORDER BY scheduled_at",
        )?;
        let rows = stmt.query_map(params![now], map_scheduled_command)?;
        rows.collect()
    }

    /// Mark a scheduled command as executed.
    pub fn mark_scheduled_command_executed(&self, id: i64) -> rusqlite::Result<()> {
        let now = current_time_millis() as i64;
        self.conn.execute(
            "UPDATE scheduled_commands SET executed_at = ?1 WHERE id = ?2",
            params![now, id],
        )?;
        Ok(())
    }

    /// Cancel a scheduled command (only if still pending). Returns true if cancelled.
    pub fn cancel_scheduled_command(&self, id: i64) -> rusqlite::Result<bool> {
        let now = current_time_millis() as i64;
        let updated = self.conn.execute(
            "UPDATE scheduled_commands SET cancelled_at = ?1 \
             WHERE id = ?2 AND executed_at IS NULL AND cancelled_at IS NULL",
            params![now, id],
        )?;
        Ok(updated > 0)
    }

    /// Cancel every pending scheduled command for a session. Returns the
    /// number of commands cancelled. Used by force-delete to stop timers
    /// that would otherwise fire against a dead tmux window.
    pub fn cancel_scheduled_commands_for_session(
        &self,
        session_id: SessionId,
    ) -> rusqlite::Result<usize> {
        let now = current_time_millis() as i64;
        let updated = self.conn.execute(
            "UPDATE scheduled_commands SET cancelled_at = ?1 \
             WHERE session_id = ?2 AND executed_at IS NULL AND cancelled_at IS NULL",
            params![now, session_id.to_string()],
        )?;
        Ok(updated)
    }

    /// List all pending scheduled commands, ordered by scheduled_at.
    pub fn list_pending_scheduled_commands(&self) -> rusqlite::Result<Vec<ScheduledCommand>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, session_id, command_text, scheduled_at, created_at, executed_at, \
             cancelled_at FROM scheduled_commands \
             WHERE executed_at IS NULL AND cancelled_at IS NULL \
             ORDER BY scheduled_at",
        )?;
        let rows = stmt.query_map([], map_scheduled_command)?;
        rows.collect()
    }

    /// Get a single scheduled command by ID.
    pub fn get_scheduled_command(&self, id: i64) -> rusqlite::Result<Option<ScheduledCommand>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, session_id, command_text, scheduled_at, created_at, executed_at, \
             cancelled_at FROM scheduled_commands WHERE id = ?1",
        )?;
        let mut rows = stmt.query_map(params![id], map_scheduled_command)?;
        match rows.next() {
            Some(row) => Ok(Some(row?)),
            None => Ok(None),
        }
    }
}

fn map_scheduled_command(row: &rusqlite::Row) -> rusqlite::Result<ScheduledCommand> {
    let id: i64 = row.get(0)?;
    let session_id_str: String = row.get(1)?;
    let command_text: String = row.get(2)?;
    let scheduled_at: i64 = row.get(3)?;
    let created_at: i64 = row.get(4)?;
    let executed_at: Option<i64> = row.get(5)?;
    let cancelled_at: Option<i64> = row.get(6)?;
    Ok(ScheduledCommand {
        id,
        session_id: session_id_str.parse().unwrap_or_default(),
        command_text,
        scheduled_at: scheduled_at as u64,
        created_at: created_at as u64,
        executed_at: executed_at.map(|v| v as u64),
        cancelled_at: cancelled_at.map(|v| v as u64),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_and_list_scheduled_commands() {
        let db = Database::open_in_memory().unwrap();
        let sid: SessionId = "test-session".parse().unwrap_or_default();

        // Schedule a command far in the future
        let future = current_time_millis() + 86_400_000;
        let id = db
            .create_scheduled_command(sid, "run tests", future)
            .unwrap();
        assert!(id > 0);

        let pending = db.list_pending_scheduled_commands().unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].command_text, "run tests");

        // Not yet due
        let due = db.due_scheduled_commands().unwrap();
        assert!(due.is_empty());
    }

    #[test]
    fn due_commands_returned_when_time_passes() {
        let db = Database::open_in_memory().unwrap();
        let sid: SessionId = "test-session".parse().unwrap_or_default();

        // Schedule in the past (immediately due)
        let past = current_time_millis().saturating_sub(1000);
        db.create_scheduled_command(sid, "hello", past).unwrap();

        let due = db.due_scheduled_commands().unwrap();
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].command_text, "hello");
    }

    #[test]
    fn mark_executed_removes_from_due() {
        let db = Database::open_in_memory().unwrap();
        let sid: SessionId = "test-session".parse().unwrap_or_default();

        let past = current_time_millis().saturating_sub(1000);
        let id = db.create_scheduled_command(sid, "cmd", past).unwrap();

        db.mark_scheduled_command_executed(id).unwrap();

        let due = db.due_scheduled_commands().unwrap();
        assert!(due.is_empty());

        let pending = db.list_pending_scheduled_commands().unwrap();
        assert!(pending.is_empty());
    }

    #[test]
    fn cancel_only_pending() {
        let db = Database::open_in_memory().unwrap();
        let sid: SessionId = "test-session".parse().unwrap_or_default();

        let future = current_time_millis() + 86_400_000;
        let id = db.create_scheduled_command(sid, "cmd", future).unwrap();

        assert!(db.cancel_scheduled_command(id).unwrap());
        // Already cancelled — second cancel returns false
        assert!(!db.cancel_scheduled_command(id).unwrap());

        let pending = db.list_pending_scheduled_commands().unwrap();
        assert!(pending.is_empty());
    }

    #[test]
    fn get_scheduled_command() {
        let db = Database::open_in_memory().unwrap();
        let sid: SessionId = "test-session".parse().unwrap_or_default();

        let future = current_time_millis() + 86_400_000;
        let id = db
            .create_scheduled_command(sid, "test cmd", future)
            .unwrap();

        let cmd = db.get_scheduled_command(id).unwrap().unwrap();
        assert_eq!(cmd.command_text, "test cmd");
        assert!(cmd.executed_at.is_none());
        assert!(cmd.cancelled_at.is_none());

        assert!(db.get_scheduled_command(9999).unwrap().is_none());
    }
}
