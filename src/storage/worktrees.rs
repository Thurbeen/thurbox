use rusqlite::params;

use crate::session::SessionId;
use crate::sync::{current_time_millis, SharedWorktree};

use super::audit::{AuditAction, EntityType};
use super::Database;

impl Database {
    /// Insert or update a worktree for a session.
    pub fn upsert_worktree(
        &self,
        session_id: SessionId,
        worktree: &SharedWorktree,
    ) -> rusqlite::Result<()> {
        let now = current_time_millis() as i64;
        let sid = session_id.to_string();

        // Use INSERT OR REPLACE to handle both insert and update
        self.conn.execute(
            "INSERT OR REPLACE INTO worktrees (session_id, repo_path, worktree_path, branch, created_at, deleted_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, NULL)",
            params![
                sid,
                worktree.repo_path.display().to_string(),
                worktree.worktree_path.display().to_string(),
                worktree.branch,
                now,
            ],
        )?;

        self.log_audit(
            EntityType::Worktree,
            &sid,
            AuditAction::Created,
            None,
            None,
            Some(&worktree.branch),
        )?;

        Ok(())
    }

    /// Soft-delete a worktree.
    pub fn soft_delete_worktree(&self, session_id: SessionId) -> rusqlite::Result<()> {
        let now = current_time_millis() as i64;
        let sid = session_id.to_string();

        self.conn.execute(
            "UPDATE worktrees SET deleted_at = ?1 WHERE session_id = ?2 AND deleted_at IS NULL",
            params![now, sid],
        )?;

        self.log_audit(
            EntityType::Worktree,
            &sid,
            AuditAction::Deleted,
            None,
            None,
            None,
        )?;

        Ok(())
    }

    /// Permanently delete a worktree row.
    pub fn hard_delete_worktree(&self, session_id: SessionId) -> rusqlite::Result<()> {
        let sid = session_id.to_string();

        self.conn
            .execute("DELETE FROM worktrees WHERE session_id = ?1", params![sid])?;

        Ok(())
    }

    /// Get a worktree for a session (active only).
    pub fn get_worktree(&self, session_id: SessionId) -> rusqlite::Result<Option<SharedWorktree>> {
        let sid = session_id.to_string();

        let result = self.conn.query_row(
            "SELECT repo_path, worktree_path, branch FROM worktrees \
             WHERE session_id = ?1 AND deleted_at IS NULL",
            params![sid],
            |row| {
                let repo: String = row.get(0)?;
                let wt_path: String = row.get(1)?;
                let branch: String = row.get(2)?;
                Ok(SharedWorktree {
                    repo_path: std::path::PathBuf::from(repo),
                    worktree_path: std::path::PathBuf::from(wt_path),
                    branch,
                })
            },
        );

        match result {
            Ok(wt) => Ok(Some(wt)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use crate::project::ProjectConfig;
    use crate::sync::SharedSession;

    use super::*;

    fn test_project_id() -> crate::project::ProjectId {
        let config = ProjectConfig {
            name: "test".to_string(),
            repos: vec![],
            roles: vec![],
        };
        config.deterministic_id()
    }

    fn setup_db_with_session() -> (Database, SessionId) {
        let db = Database::open_in_memory().unwrap();
        let pid = test_project_id();
        db.insert_project(pid, "test", &[], false).unwrap();

        let session = SharedSession {
            id: SessionId::default(),
            name: "S1".to_string(),
            project_id: pid,
            role: "developer".to_string(),
            backend_id: "thurbox:@0".to_string(),
            backend_type: "tmux".to_string(),
            claude_session_id: None,
            cwd: None,
            worktree: None,
            tombstone: false,
            tombstone_at: None,
        };
        let sid = session.id;
        db.upsert_session(&session).unwrap();
        (db, sid)
    }

    #[test]
    fn upsert_and_get_worktree() {
        let (db, sid) = setup_db_with_session();
        let wt = SharedWorktree {
            repo_path: PathBuf::from("/repo"),
            worktree_path: PathBuf::from("/repo/.git/wt/feat"),
            branch: "feat".to_string(),
        };

        db.upsert_worktree(sid, &wt).unwrap();

        let result = db.get_worktree(sid).unwrap();
        assert!(result.is_some());
        let got = result.unwrap();
        assert_eq!(got.branch, "feat");
        assert_eq!(got.repo_path, PathBuf::from("/repo"));
    }

    #[test]
    fn soft_delete_worktree() {
        let (db, sid) = setup_db_with_session();
        let wt = SharedWorktree {
            repo_path: PathBuf::from("/repo"),
            worktree_path: PathBuf::from("/repo/.git/wt/feat"),
            branch: "feat".to_string(),
        };

        db.upsert_worktree(sid, &wt).unwrap();
        db.soft_delete_worktree(sid).unwrap();

        assert!(db.get_worktree(sid).unwrap().is_none());
    }

    #[test]
    fn hard_delete_worktree() {
        let (db, sid) = setup_db_with_session();
        let wt = SharedWorktree {
            repo_path: PathBuf::from("/repo"),
            worktree_path: PathBuf::from("/repo/.git/wt/feat"),
            branch: "feat".to_string(),
        };

        db.upsert_worktree(sid, &wt).unwrap();
        db.hard_delete_worktree(sid).unwrap();

        // Hard delete permanently removes the row
        assert!(db.get_worktree(sid).unwrap().is_none());
    }

    #[test]
    fn no_worktree_returns_none() {
        let (db, sid) = setup_db_with_session();
        assert!(db.get_worktree(sid).unwrap().is_none());
    }

    #[test]
    fn upsert_replaces_worktree() {
        let (db, sid) = setup_db_with_session();

        let wt1 = SharedWorktree {
            repo_path: PathBuf::from("/repo"),
            worktree_path: PathBuf::from("/repo/.git/wt/old"),
            branch: "old".to_string(),
        };
        db.upsert_worktree(sid, &wt1).unwrap();

        let wt2 = SharedWorktree {
            repo_path: PathBuf::from("/repo"),
            worktree_path: PathBuf::from("/repo/.git/wt/new"),
            branch: "new".to_string(),
        };
        db.upsert_worktree(sid, &wt2).unwrap();

        let got = db.get_worktree(sid).unwrap().unwrap();
        assert_eq!(got.branch, "new");
    }
}
