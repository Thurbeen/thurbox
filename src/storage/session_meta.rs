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

use std::collections::{BTreeMap, HashMap};

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

    /// Every key starting with `prefix`, for every session, in one query — what
    /// the interface's snapshot read publishes (`ui.*`, issue #1523). The
    /// caller decides what a key means; this only selects by prefix.
    pub fn load_session_meta_with_prefix(
        &self,
        prefix: &str,
    ) -> rusqlite::Result<HashMap<SessionId, BTreeMap<String, String>>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT session_id, key, value FROM session_meta \
             WHERE substr(key, 1, length(?1)) = ?1 ORDER BY session_id, key",
        )?;
        let rows = stmt.query_map(params![prefix], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?;
        let mut all: HashMap<SessionId, BTreeMap<String, String>> = HashMap::new();
        for row in rows {
            let (id, key, value) = row?;
            // A row whose id does not parse belongs to no session a reader can
            // name, so it is skipped rather than failing every other row.
            if let Ok(id) = id.parse::<SessionId>() {
                all.entry(id).or_default().insert(key, value);
            }
        }
        Ok(all)
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
}
