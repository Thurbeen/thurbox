use crate::sync::{SharedState, StateDelta};

use super::Database;

impl Database {
    /// Check if another instance has modified the database since our last check.
    ///
    /// Uses SQLite's `PRAGMA data_version` which increments whenever another
    /// connection modifies the database in WAL mode.
    pub fn has_external_changes(&mut self) -> rusqlite::Result<bool> {
        let current: i64 = self
            .conn
            .query_row("PRAGMA data_version", [], |row| row.get(0))?;

        if current != self.last_data_version {
            self.last_data_version = current;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    /// Build a SharedState snapshot from the current database contents.
    ///
    /// Used to compute deltas against local in-memory state.
    pub fn load_shared_state(&self) -> rusqlite::Result<SharedState> {
        let projects = self.list_active_projects()?;
        let sessions = self.list_active_sessions()?;
        let counter = self.get_session_counter()?;

        Ok(SharedState {
            version: 1,
            last_modified: crate::sync::current_time_millis(),
            session_counter: counter,
            sessions,
            projects,
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
    use std::path::PathBuf;

    use crate::project::ProjectConfig;
    use crate::session::SessionId;
    use crate::sync::{SharedSession, SharedState};

    use super::*;

    fn test_project_id(name: &str) -> crate::project::ProjectId {
        let config = ProjectConfig {
            name: name.to_string(),
            repos: vec![],
            roles: vec![],
            id: None,
        };
        config.deterministic_id()
    }

    fn make_session(name: &str, project_id: crate::project::ProjectId) -> SharedSession {
        SharedSession {
            id: SessionId::default(),
            name: name.to_string(),
            project_id,
            role: "developer".to_string(),
            backend_id: "thurbox:@0".to_string(),
            backend_type: "tmux".to_string(),
            claude_session_id: None,
            cwd: None,
            worktree: None,
            tombstone: false,
            tombstone_at: None,
        }
    }

    #[test]
    fn load_shared_state_from_db() {
        let db = Database::open_in_memory().unwrap();
        let pid = test_project_id("test");
        db.insert_project(pid, "test", &[PathBuf::from("/repo")], false)
            .unwrap();

        let session = make_session("S1", pid);
        db.upsert_session(&session).unwrap();
        db.set_session_counter(5).unwrap();

        let state = db.load_shared_state().unwrap();
        assert_eq!(state.projects.len(), 1);
        assert_eq!(state.sessions.len(), 1);
        assert_eq!(state.session_counter, 5);
    }

    #[test]
    fn compute_delta_detects_added_session() {
        let db = Database::open_in_memory().unwrap();
        let pid = test_project_id("test");
        db.insert_project(pid, "test", &[], false).unwrap();

        // Local state is empty
        let local = SharedState::new();

        // DB has a session
        let session = make_session("S1", pid);
        db.upsert_session(&session).unwrap();

        let delta = db.compute_delta(&local).unwrap();
        assert_eq!(delta.added_sessions.len(), 1);
        assert_eq!(delta.added_sessions[0].name, "S1");
    }

    #[test]
    fn compute_delta_detects_removed_session() {
        let db = Database::open_in_memory().unwrap();
        let pid = test_project_id("test");
        db.insert_project(pid, "test", &[], false).unwrap();

        // Local state has a session
        let session = make_session("S1", pid);
        let sid = session.id;
        let mut local = SharedState::new();
        local.sessions.push(session.clone());

        // DB has session soft-deleted
        db.upsert_session(&session).unwrap();
        db.soft_delete_session(sid).unwrap();

        let delta = db.compute_delta(&local).unwrap();
        assert_eq!(delta.removed_sessions.len(), 1);
    }

    #[test]
    fn has_external_changes_in_memory() {
        let mut db = Database::open_in_memory().unwrap();

        // First call always reports changes (initial data_version)
        let _ = db.has_external_changes().unwrap();

        // No changes yet for in-memory (single connection)
        let changed = db.has_external_changes().unwrap();
        assert!(!changed);
    }

    #[test]
    fn multi_connection_change_detection() {
        let temp = tempfile::NamedTempFile::new().unwrap();
        let path = temp.path();

        let mut db1 = Database::open(path).unwrap();
        let db2 = Database::open(path).unwrap();

        // Initialize db1's version tracking
        let _ = db1.has_external_changes().unwrap();

        // db2 makes a change
        let pid = test_project_id("test");
        db2.insert_project(pid, "test", &[], false).unwrap();

        // db1 should detect the external change
        let changed = db1.has_external_changes().unwrap();
        assert!(changed);

        // Second check with no new changes should return false
        let changed_again = db1.has_external_changes().unwrap();
        assert!(!changed_again);
    }
}
