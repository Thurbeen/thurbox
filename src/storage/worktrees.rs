use rusqlite::params;

use crate::session::SessionId;
use crate::sync::{current_time_millis, SharedWorktree};

use super::audit::{AuditAction, EntityType};
use super::Database;

impl Database {
    /// Replace all worktrees for a session.
    ///
    /// Deletes existing active rows for the session, then inserts all new rows.
    pub fn upsert_worktrees(
        &self,
        session_id: SessionId,
        worktrees: &[SharedWorktree],
    ) -> rusqlite::Result<()> {
        let now = current_time_millis() as i64;
        let sid = session_id.to_string();

        self.conn.execute(
            "DELETE FROM worktrees WHERE session_id = ?1 AND deleted_at IS NULL",
            params![sid],
        )?;

        for wt in worktrees {
            self.conn.execute(
                "INSERT INTO worktrees (session_id, repo_path, worktree_path, branch, created_at, deleted_at, created_by_thurbox) \
                 VALUES (?1, ?2, ?3, ?4, ?5, NULL, ?6)",
                params![
                    sid,
                    wt.repo_path.display().to_string(),
                    wt.worktree_path.display().to_string(),
                    wt.branch,
                    now,
                    wt.created_by_thurbox,
                ],
            )?;
        }

        if !worktrees.is_empty() {
            self.log_audit(
                EntityType::Worktree,
                &sid,
                AuditAction::Created,
                None,
                None,
                Some(&worktrees[0].branch),
            )?;
        }

        Ok(())
    }

    /// Soft-delete all worktrees for a session.
    pub fn soft_delete_worktrees(&self, session_id: SessionId) -> rusqlite::Result<()> {
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

    /// Permanently delete all worktree rows for a session.
    pub fn hard_delete_worktrees(&self, session_id: SessionId) -> rusqlite::Result<()> {
        let sid = session_id.to_string();

        self.conn
            .execute("DELETE FROM worktrees WHERE session_id = ?1", params![sid])?;

        Ok(())
    }

    /// Get all active worktrees for a session.
    pub fn get_worktrees(&self, session_id: SessionId) -> rusqlite::Result<Vec<SharedWorktree>> {
        let sid = session_id.to_string();

        // Cached: read on the refresh schedule, like the session queries.
        let mut stmt = self.conn.prepare_cached(
            "SELECT repo_path, worktree_path, branch, created_by_thurbox FROM worktrees \
             WHERE session_id = ?1 AND deleted_at IS NULL \
             ORDER BY created_at",
        )?;

        let rows = stmt.query_map(params![sid], |row| {
            let repo: String = row.get(0)?;
            let wt_path: String = row.get(1)?;
            let branch: String = row.get(2)?;
            Ok(SharedWorktree {
                repo_path: std::path::PathBuf::from(repo),
                worktree_path: std::path::PathBuf::from(wt_path),
                branch,
                created_by_thurbox: row.get(3)?,
            })
        })?;

        rows.collect()
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use crate::sync::SharedSession;

    use super::*;

    fn setup_db_with_session() -> (Database, SessionId) {
        let db = Database::open_in_memory().unwrap();

        let session = SharedSession {
            id: SessionId::default(),
            name: "S1".to_string(),
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
        };
        let sid = session.id;
        db.upsert_session(&session).unwrap();
        (db, sid)
    }

    #[test]
    fn upsert_and_get_worktrees() {
        let (db, sid) = setup_db_with_session();
        let wt = SharedWorktree {
            repo_path: PathBuf::from("/repo"),
            worktree_path: PathBuf::from("/repo/.git/wt/feat"),
            branch: "feat".to_string(),
            created_by_thurbox: true,
        };

        db.upsert_worktrees(sid, &[wt]).unwrap();

        let result = db.get_worktrees(sid).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].branch, "feat");
        assert_eq!(result[0].repo_path, PathBuf::from("/repo"));
    }

    #[test]
    fn provenance_survives_the_round_trip() {
        // Force-delete decides whether to run `git worktree remove` from this
        // flag, and it decides that after a restart — so a worktree thurbox
        // merely opened must still read as "not mine" when it comes back off
        // disk. A flag that defaulted to true on read would delete the user's
        // directory on the very restart it was meant to survive.
        let (db, sid) = setup_db_with_session();
        let opened = SharedWorktree {
            repo_path: PathBuf::from("/repo"),
            worktree_path: PathBuf::from("/repo/.worktrees/mine"),
            branch: "feat/mine".to_string(),
            created_by_thurbox: false,
        };

        db.upsert_worktrees(sid, &[opened]).unwrap();

        let result = db.get_worktrees(sid).unwrap();
        assert_eq!(result.len(), 1);
        assert!(!result[0].created_by_thurbox);
    }

    #[test]
    fn upsert_multiple_worktrees() {
        let (db, sid) = setup_db_with_session();
        let wts = vec![
            SharedWorktree {
                repo_path: PathBuf::from("/repo1"),
                worktree_path: PathBuf::from("/repo1/.git/wt/feat"),
                branch: "feat".to_string(),
                created_by_thurbox: true,
            },
            SharedWorktree {
                repo_path: PathBuf::from("/repo2"),
                worktree_path: PathBuf::from("/repo2/.git/wt/feat"),
                branch: "feat".to_string(),
                created_by_thurbox: true,
            },
        ];

        db.upsert_worktrees(sid, &wts).unwrap();

        let result = db.get_worktrees(sid).unwrap();
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].repo_path, PathBuf::from("/repo1"));
        assert_eq!(result[1].repo_path, PathBuf::from("/repo2"));
    }

    #[test]
    fn soft_delete_worktrees() {
        let (db, sid) = setup_db_with_session();
        let wt = SharedWorktree {
            repo_path: PathBuf::from("/repo"),
            worktree_path: PathBuf::from("/repo/.git/wt/feat"),
            branch: "feat".to_string(),
            created_by_thurbox: true,
        };

        db.upsert_worktrees(sid, &[wt]).unwrap();
        db.soft_delete_worktrees(sid).unwrap();

        assert!(db.get_worktrees(sid).unwrap().is_empty());
    }

    #[test]
    fn hard_delete_worktrees() {
        let (db, sid) = setup_db_with_session();
        let wt = SharedWorktree {
            repo_path: PathBuf::from("/repo"),
            worktree_path: PathBuf::from("/repo/.git/wt/feat"),
            branch: "feat".to_string(),
            created_by_thurbox: true,
        };

        db.upsert_worktrees(sid, &[wt]).unwrap();
        db.hard_delete_worktrees(sid).unwrap();

        assert!(db.get_worktrees(sid).unwrap().is_empty());
    }

    #[test]
    fn no_worktrees_returns_empty_vec() {
        let (db, sid) = setup_db_with_session();
        assert!(db.get_worktrees(sid).unwrap().is_empty());
    }

    #[test]
    fn upsert_empty_worktrees_is_noop() {
        let (db, sid) = setup_db_with_session();
        db.upsert_worktrees(sid, &[]).unwrap();
        assert!(db.get_worktrees(sid).unwrap().is_empty());
    }

    #[test]
    fn upsert_replaces_worktrees() {
        let (db, sid) = setup_db_with_session();

        let wt1 = SharedWorktree {
            repo_path: PathBuf::from("/repo"),
            worktree_path: PathBuf::from("/repo/.git/wt/old"),
            branch: "old".to_string(),
            created_by_thurbox: true,
        };
        db.upsert_worktrees(sid, &[wt1]).unwrap();

        let wt2 = SharedWorktree {
            repo_path: PathBuf::from("/repo"),
            worktree_path: PathBuf::from("/repo/.git/wt/new"),
            branch: "new".to_string(),
            created_by_thurbox: true,
        };
        db.upsert_worktrees(sid, &[wt2]).unwrap();

        let got = db.get_worktrees(sid).unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].branch, "new");
    }
}
