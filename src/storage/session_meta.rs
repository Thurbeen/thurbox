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

    /// The meta a reader may publish, for every **active** session, in one
    /// query and bounded as it is read (issue #1523): keys starting with
    /// `prefix` and at most `key_chars` characters long, the first `keys` of
    /// them per session in key order, each value cut to `value_chars`.
    ///
    /// Bounded here rather than after the read because a value comes from
    /// whoever has the CLI and has no size limit of its own: a full snapshot
    /// read must not load what it would only throw away. The caller still
    /// decides what a key means.
    pub fn load_active_session_meta(
        &self,
        prefix: &str,
        key_chars: usize,
        keys: usize,
        value_chars: usize,
    ) -> rusqlite::Result<HashMap<SessionId, BTreeMap<String, String>>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT session_id, key, substr(value, 1, ?4) FROM ( \
                 SELECT m.session_id, m.key, m.value, ROW_NUMBER() OVER ( \
                     PARTITION BY m.session_id ORDER BY m.key) AS position \
                 FROM session_meta m JOIN sessions s ON s.id = m.session_id \
                 WHERE s.deleted_at IS NULL \
                   AND substr(m.key, 1, length(?1)) = ?1 \
                   AND length(m.key) <= ?2) \
             WHERE position <= ?3",
        )?;
        let rows = stmt.query_map(
            params![prefix, key_chars as i64, keys as i64, value_chars as i64],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            },
        )?;
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

#[cfg(test)]
mod tests {
    use crate::sync::SharedSession;

    use super::*;

    fn session(db: &Database, name: &str) -> SessionId {
        let session = SharedSession {
            id: SessionId::default(),
            name: name.to_string(),
            agent: "shell".to_string(),
            backend_id: String::new(),
            backend_type: "local-tmux".to_string(),
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
        };
        db.upsert_session(&session).unwrap();
        session.id
    }

    #[test]
    fn the_published_read_is_bounded_and_skips_deleted_sessions() {
        let db = Database::open_in_memory().unwrap();
        let live = session(&db, "live");
        let gone = session(&db, "gone");
        for key in ["ui.d", "ui.c", "ui.b", "ui.a"] {
            db.set_session_meta(live, key, "0123456789").unwrap();
        }
        db.set_session_meta(live, "ui.far-too-long-a-key", "x")
            .unwrap();
        db.set_session_meta(live, "fm.lease", "x").unwrap();
        db.set_session_meta(gone, "ui.note", "x").unwrap();
        db.soft_delete_session(gone).unwrap();

        let read = db.load_active_session_meta("ui.", 8, 3, 4).unwrap();
        assert_eq!(
            read.keys().collect::<Vec<_>>(),
            [&live],
            "no deleted session"
        );
        assert_eq!(
            read[&live].iter().collect::<Vec<_>>(),
            [
                (&"ui.a".to_string(), &"0123".to_string()),
                (&"ui.b".to_string(), &"0123".to_string()),
                (&"ui.c".to_string(), &"0123".to_string()),
            ],
            "the first three short-enough keys, each value cut"
        );
    }
}
