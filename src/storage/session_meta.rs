//! Per-session key/value metadata — the driver's own scratch space.
//!
//! Whoever is driving thurbox has identity of its own to keep beside a session:
//! a task id, a lease, a correlation key. Without somewhere to put it, that
//! identity ends up encoded in the session *name*, which then has to be parsed,
//! kept unique and kept inside the name-length limit — a shape thurbox should
//! not force on anyone.
//!
//! Keys are namespaced by convention (`fm.*`, `gc.*`, `you.*`) so two drivers
//! against one database do not collide. Nothing here interprets a key or a
//! value: this table is storage, and that is the whole contract.

use std::collections::BTreeMap;

use rusqlite::params;

use crate::session::SessionId;
use crate::sync::current_time_millis;

use super::Database;

impl Database {
    /// Set one metadata key, replacing any previous value.
    pub fn set_session_meta(&self, id: SessionId, key: &str, value: &str) -> rusqlite::Result<()> {
        self.conn.execute(
            "INSERT INTO session_meta (session_id, key, value, updated_at) \
             VALUES (?1, ?2, ?3, ?4) \
             ON CONFLICT(session_id, key) DO UPDATE SET \
                 value = excluded.value, updated_at = excluded.updated_at",
            params![id.to_string(), key, value, current_time_millis() as i64],
        )?;
        Ok(())
    }

    /// One metadata value, or `None` when the key is unset.
    pub fn get_session_meta(&self, id: SessionId, key: &str) -> rusqlite::Result<Option<String>> {
        use rusqlite::OptionalExtension;
        self.conn
            .query_row(
                "SELECT value FROM session_meta WHERE session_id = ?1 AND key = ?2",
                params![id.to_string(), key],
                |row| row.get(0),
            )
            .optional()
    }

    /// Every key set on a session, ordered by key so a listing is stable.
    pub fn list_session_meta(&self, id: SessionId) -> rusqlite::Result<BTreeMap<String, String>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT key, value FROM session_meta WHERE session_id = ?1 ORDER BY key",
        )?;
        let rows = stmt.query_map(params![id.to_string()], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        rows.collect()
    }

    /// Remove one key. Returns whether it was there — so an `unset` can say
    /// "nothing to do" rather than claiming a removal it did not make.
    pub fn unset_session_meta(&self, id: SessionId, key: &str) -> rusqlite::Result<bool> {
        let removed = self.conn.execute(
            "DELETE FROM session_meta WHERE session_id = ?1 AND key = ?2",
            params![id.to_string(), key],
        )?;
        Ok(removed > 0)
    }

    /// Drop every key of a session. Called when a session is force-deleted:
    /// the row is unrestorable, so its metadata is dead weight that would
    /// otherwise outlive it and be inherited by nothing.
    pub fn clear_session_meta(&self, id: SessionId) -> rusqlite::Result<()> {
        self.conn.execute(
            "DELETE FROM session_meta WHERE session_id = ?1",
            params![id.to_string()],
        )?;
        Ok(())
    }
}
