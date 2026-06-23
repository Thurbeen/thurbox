use crate::sync::{SharedState, StateDelta};

use super::Database;

impl Database {
    /// Check if another instance has modified the database since our last check.
    pub fn has_external_changes(&mut self) -> rusqlite::Result<bool> {
        let current = self.data_version()?;

        if current != self.last_data_version {
            self.last_data_version = current;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    /// Read `PRAGMA data_version` without advancing the
    /// [`Self::has_external_changes`] cursor. The value changes whenever
    /// *another* connection commits (never for this connection's own writes),
    /// so a caller can cheaply gate a per-tick cache reload on it — independent
    /// of, and without disturbing, the sync poll's own change tracking. The
    /// pragma reads an in-memory counter (no table access), so it is far cheaper
    /// than re-running a query.
    pub fn data_version(&self) -> rusqlite::Result<i64> {
        self.conn
            .query_row("PRAGMA data_version", [], |row| row.get(0))
    }

    /// Build a SharedState snapshot from the current database contents.
    pub fn load_shared_state(&self) -> rusqlite::Result<SharedState> {
        let sessions = self.list_active_sessions()?;
        let counter = self.get_session_counter()?;

        Ok(SharedState {
            version: 1,
            last_modified: crate::sync::current_time_millis(),
            session_counter: counter,
            sessions,
        })
    }

    /// Compute the delta between the current database state and a local snapshot.
    pub fn compute_delta(&self, local: &SharedState) -> rusqlite::Result<StateDelta> {
        let db_state = self.load_shared_state()?;
        Ok(StateDelta::compute(local, &db_state))
    }
}

#[cfg(test)]
mod tests {
    use crate::session::SessionId;
    use crate::sync::{SharedSession, SharedState};

    use super::*;

    fn make_session(name: &str) -> SharedSession {
        SharedSession {
            id: SessionId::default(),
            name: name.to_string(),
            agent: "developer".to_string(),
            backend_id: "thurbox:@0".to_string(),
            backend_type: "tmux".to_string(),
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
    fn load_shared_state_from_db() {
        let db = Database::open_in_memory().unwrap();

        let session = make_session("S1");
        db.upsert_session(&session).unwrap();
        db.set_session_counter(5).unwrap();

        let state = db.load_shared_state().unwrap();
        assert_eq!(state.sessions.len(), 1);
        assert_eq!(state.session_counter, 5);
    }

    #[test]
    fn compute_delta_detects_added_session() {
        let db = Database::open_in_memory().unwrap();

        let local = SharedState::new();

        let session = make_session("S1");
        db.upsert_session(&session).unwrap();

        let delta = db.compute_delta(&local).unwrap();
        assert_eq!(delta.added_sessions.len(), 1);
        assert_eq!(delta.added_sessions[0].name, "S1");
    }

    #[test]
    fn compute_delta_detects_removed_session() {
        let db = Database::open_in_memory().unwrap();

        let session = make_session("S1");
        let sid = session.id;
        let mut local = SharedState::new();
        local.sessions.push(session.clone());

        db.upsert_session(&session).unwrap();
        db.soft_delete_session(sid).unwrap();

        let delta = db.compute_delta(&local).unwrap();
        assert_eq!(delta.removed_sessions.len(), 1);
    }

    #[test]
    fn has_external_changes_in_memory() {
        let mut db = Database::open_in_memory().unwrap();

        let _ = db.has_external_changes().unwrap();

        let changed = db.has_external_changes().unwrap();
        assert!(!changed);
    }

    #[test]
    fn multi_connection_change_detection() {
        let temp = tempfile::NamedTempFile::new().unwrap();
        let path = temp.path();

        let mut db1 = Database::open(path).unwrap();
        let db2 = Database::open(path).unwrap();

        let _ = db1.has_external_changes().unwrap();

        // db2 makes a change
        let session = make_session("S1");
        db2.upsert_session(&session).unwrap();

        let changed = db1.has_external_changes().unwrap();
        assert!(changed);

        let changed_again = db1.has_external_changes().unwrap();
        assert!(!changed_again);
    }
}
