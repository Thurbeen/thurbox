use std::path::PathBuf;

use rusqlite::params;

use crate::project::ProjectId;
use crate::session::SessionId;
use crate::sync::{current_time_millis, SharedSession, SharedWorktree};

use super::audit::{AuditAction, EntityType};
use super::Database;

impl Database {
    /// Insert or update a session.
    pub fn upsert_session(&self, session: &SharedSession) -> rusqlite::Result<()> {
        let now = current_time_millis() as i64;
        let id_str = session.id.to_string();
        let project_id_str = session.project_id.to_string();

        let existing: Option<String> = self
            .conn
            .query_row(
                "SELECT id FROM sessions WHERE id = ?1",
                params![id_str],
                |row| row.get(0),
            )
            .ok();

        let additional_dirs_str: String = session
            .additional_dirs
            .iter()
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>()
            .join("\n");

        if existing.is_some() {
            self.conn.execute(
                "UPDATE sessions SET name = ?1, project_id = ?2, role = ?3, \
                 backend_id = ?4, backend_type = ?5, claude_session_id = ?6, \
                 cwd = ?7, additional_dirs = ?8, updated_at = ?9, deleted_at = NULL \
                 WHERE id = ?10",
                params![
                    session.name,
                    project_id_str,
                    session.role,
                    session.backend_id,
                    session.backend_type,
                    session.claude_session_id,
                    session.cwd.as_ref().map(|p| p.display().to_string()),
                    additional_dirs_str,
                    now,
                    id_str,
                ],
            )?;

            self.log_audit(
                EntityType::Session,
                &id_str,
                AuditAction::Updated,
                None,
                None,
                None,
            )?;
        } else {
            self.conn.execute(
                "INSERT INTO sessions (id, name, project_id, role, backend_id, backend_type, \
                 claude_session_id, cwd, additional_dirs, created_at, updated_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
                params![
                    id_str,
                    session.name,
                    project_id_str,
                    session.role,
                    session.backend_id,
                    session.backend_type,
                    session.claude_session_id,
                    session.cwd.as_ref().map(|p| p.display().to_string()),
                    additional_dirs_str,
                    now,
                    now,
                ],
            )?;

            self.log_audit(
                EntityType::Session,
                &id_str,
                AuditAction::Created,
                None,
                None,
                Some(&session.name),
            )?;
        }

        // Upsert worktree if present
        if let Some(wt) = &session.worktree {
            self.upsert_worktree(session.id, wt)?;
        }

        Ok(())
    }

    /// Soft-delete a session.
    pub fn soft_delete_session(&self, id: SessionId) -> rusqlite::Result<()> {
        let now = current_time_millis() as i64;
        let id_str = id.to_string();

        self.conn.execute(
            "UPDATE sessions SET deleted_at = ?1, updated_at = ?1 WHERE id = ?2 AND deleted_at IS NULL",
            params![now, id_str],
        )?;

        self.log_audit(
            EntityType::Session,
            &id_str,
            AuditAction::Deleted,
            None,
            None,
            None,
        )?;

        Ok(())
    }

    /// Restore a soft-deleted session.
    pub fn restore_session(&self, id: SessionId) -> rusqlite::Result<()> {
        let now = current_time_millis() as i64;
        let id_str = id.to_string();

        self.conn.execute(
            "UPDATE sessions SET deleted_at = NULL, updated_at = ?1 WHERE id = ?2 AND deleted_at IS NOT NULL",
            params![now, id_str],
        )?;

        self.log_audit(
            EntityType::Session,
            &id_str,
            AuditAction::Restored,
            None,
            None,
            None,
        )?;

        Ok(())
    }

    /// List all active (non-deleted) sessions.
    pub fn list_active_sessions(&self) -> rusqlite::Result<Vec<SharedSession>> {
        self.query_sessions("s.deleted_at IS NULL")
    }

    /// List active sessions for a specific project.
    pub fn list_sessions_for_project(
        &self,
        project_id: ProjectId,
    ) -> rusqlite::Result<Vec<SharedSession>> {
        self.query_sessions(&format!(
            "s.deleted_at IS NULL AND s.project_id = '{}'",
            project_id
        ))
    }

    fn query_sessions(&self, condition: &str) -> rusqlite::Result<Vec<SharedSession>> {
        let sql = format!(
            "SELECT s.id, s.name, s.project_id, s.role, s.backend_id, s.backend_type, \
             s.claude_session_id, s.cwd, s.additional_dirs, \
             w.repo_path, w.worktree_path, w.branch \
             FROM sessions s \
             LEFT JOIN worktrees w ON s.id = w.session_id AND w.deleted_at IS NULL \
             WHERE {condition} \
             ORDER BY s.created_at"
        );

        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map([], |row| {
            let id_str: String = row.get(0)?;
            let project_id_str: String = row.get(2)?;
            let cwd: Option<String> = row.get(7)?;
            let dirs_str: String = row.get(8)?;
            let wt_repo: Option<String> = row.get(9)?;
            let wt_path: Option<String> = row.get(10)?;
            let wt_branch: Option<String> = row.get(11)?;

            let additional_dirs: Vec<PathBuf> = if dirs_str.is_empty() {
                Vec::new()
            } else {
                dirs_str.split('\n').map(PathBuf::from).collect()
            };

            let worktree = match (wt_repo, wt_path, wt_branch) {
                (Some(repo), Some(path), Some(branch)) => Some(SharedWorktree {
                    repo_path: PathBuf::from(repo),
                    worktree_path: PathBuf::from(path),
                    branch,
                }),
                _ => None,
            };

            Ok(SharedSession {
                id: id_str.parse().unwrap_or_default(),
                name: row.get(1)?,
                project_id: project_id_str
                    .parse::<uuid::Uuid>()
                    .map(ProjectId::from_uuid)
                    .unwrap_or_default(),
                role: row.get(3)?,
                backend_id: row.get(4)?,
                backend_type: row.get(5)?,
                claude_session_id: row.get(6)?,
                cwd: cwd.map(PathBuf::from),
                additional_dirs,
                worktree,
                tombstone: false,
                tombstone_at: None,
            })
        })?;

        rows.collect()
    }

    /// Get the session counter value.
    pub fn get_session_counter(&self) -> rusqlite::Result<usize> {
        let val: String = self.conn.query_row(
            "SELECT value FROM metadata WHERE key = 'session_counter'",
            [],
            |row| row.get(0),
        )?;
        Ok(val.parse().unwrap_or(0))
    }

    /// Set the session counter to a specific value.
    pub fn set_session_counter(&self, value: usize) -> rusqlite::Result<()> {
        self.conn.execute(
            "UPDATE metadata SET value = ?1 WHERE key = 'session_counter'",
            params![value.to_string()],
        )?;
        Ok(())
    }

    /// Atomically increment session counter and return the new value.
    pub fn increment_session_counter(&self) -> rusqlite::Result<usize> {
        let current = self.get_session_counter()?;
        let next = current + 1;
        self.set_session_counter(next)?;
        Ok(next)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::ProjectConfig;

    fn test_project_id(name: &str) -> ProjectId {
        let config = ProjectConfig {
            name: name.to_string(),
            repos: vec![],
            roles: vec![],
            mcp_servers: vec![],
            id: None,
        };
        config.deterministic_id()
    }

    fn make_session(name: &str, project_id: ProjectId) -> SharedSession {
        SharedSession {
            id: SessionId::default(),
            name: name.to_string(),
            project_id,
            role: "developer".to_string(),
            backend_id: "thurbox:@0".to_string(),
            backend_type: "tmux".to_string(),
            claude_session_id: None,
            cwd: None,
            additional_dirs: Vec::new(),
            worktree: None,
            tombstone: false,
            tombstone_at: None,
        }
    }

    fn setup_db_with_project() -> (Database, ProjectId) {
        let db = Database::open_in_memory().unwrap();
        let pid = test_project_id("test");
        db.insert_project(pid, "test", &[]).unwrap();
        (db, pid)
    }

    #[test]
    fn upsert_and_list_session() {
        let (db, pid) = setup_db_with_project();
        let session = make_session("Session 1", pid);

        db.upsert_session(&session).unwrap();

        let sessions = db.list_active_sessions().unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].name, "Session 1");
        assert_eq!(sessions[0].role, "developer");
    }

    #[test]
    fn upsert_updates_existing() {
        let (db, pid) = setup_db_with_project();
        let mut session = make_session("Session 1", pid);

        db.upsert_session(&session).unwrap();

        session.name = "Renamed".to_string();
        session.role = "reviewer".to_string();
        db.upsert_session(&session).unwrap();

        let sessions = db.list_active_sessions().unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].name, "Renamed");
        assert_eq!(sessions[0].role, "reviewer");
    }

    #[test]
    fn soft_delete_session() {
        let (db, pid) = setup_db_with_project();
        let session = make_session("Session 1", pid);
        let sid = session.id;

        db.upsert_session(&session).unwrap();
        db.soft_delete_session(sid).unwrap();

        let active = db.list_active_sessions().unwrap();
        assert!(active.is_empty());
    }

    #[test]
    fn restore_session() {
        let (db, pid) = setup_db_with_project();
        let session = make_session("Session 1", pid);
        let sid = session.id;

        db.upsert_session(&session).unwrap();
        db.soft_delete_session(sid).unwrap();
        db.restore_session(sid).unwrap();

        let active = db.list_active_sessions().unwrap();
        assert_eq!(active.len(), 1);
    }

    #[test]
    fn list_sessions_for_project() {
        let db = Database::open_in_memory().unwrap();
        let pid1 = test_project_id("proj1");
        let pid2 = test_project_id("proj2");
        db.insert_project(pid1, "proj1", &[]).unwrap();
        db.insert_project(pid2, "proj2", &[]).unwrap();

        let s1 = make_session("S1", pid1);
        let s2 = make_session("S2", pid2);
        db.upsert_session(&s1).unwrap();
        db.upsert_session(&s2).unwrap();

        let proj1_sessions = db.list_sessions_for_project(pid1).unwrap();
        assert_eq!(proj1_sessions.len(), 1);
        assert_eq!(proj1_sessions[0].name, "S1");
    }

    #[test]
    fn session_with_worktree() {
        let (db, pid) = setup_db_with_project();
        let mut session = make_session("Session 1", pid);
        session.worktree = Some(SharedWorktree {
            repo_path: PathBuf::from("/repo"),
            worktree_path: PathBuf::from("/repo/.git/wt/feat"),
            branch: "feat".to_string(),
        });

        db.upsert_session(&session).unwrap();

        let sessions = db.list_active_sessions().unwrap();
        assert!(sessions[0].worktree.is_some());
        let wt = sessions[0].worktree.as_ref().unwrap();
        assert_eq!(wt.branch, "feat");
        assert_eq!(wt.repo_path, PathBuf::from("/repo"));
    }

    #[test]
    fn session_with_cwd() {
        let (db, pid) = setup_db_with_project();
        let mut session = make_session("Session 1", pid);
        session.cwd = Some(PathBuf::from("/home/user"));

        db.upsert_session(&session).unwrap();

        let sessions = db.list_active_sessions().unwrap();
        assert_eq!(sessions[0].cwd, Some(PathBuf::from("/home/user")));
    }

    #[test]
    fn session_counter_operations() {
        let db = Database::open_in_memory().unwrap();

        assert_eq!(db.get_session_counter().unwrap(), 0);

        db.set_session_counter(5).unwrap();
        assert_eq!(db.get_session_counter().unwrap(), 5);

        let next = db.increment_session_counter().unwrap();
        assert_eq!(next, 6);
        assert_eq!(db.get_session_counter().unwrap(), 6);
    }

    #[test]
    fn session_additional_dirs_preserved() {
        let (db, pid) = setup_db_with_project();
        let mut session = make_session("Session 1", pid);
        session.additional_dirs = vec![
            PathBuf::from("/home/user/repo2"),
            PathBuf::from("/home/user/repo3"),
        ];

        db.upsert_session(&session).unwrap();

        let sessions = db.list_active_sessions().unwrap();
        assert_eq!(sessions[0].additional_dirs.len(), 2);
        assert_eq!(
            sessions[0].additional_dirs[0],
            PathBuf::from("/home/user/repo2")
        );
        assert_eq!(
            sessions[0].additional_dirs[1],
            PathBuf::from("/home/user/repo3")
        );
    }

    #[test]
    fn session_empty_additional_dirs_preserved() {
        let (db, pid) = setup_db_with_project();
        let session = make_session("Session 1", pid);

        db.upsert_session(&session).unwrap();

        let sessions = db.list_active_sessions().unwrap();
        assert!(sessions[0].additional_dirs.is_empty());
    }

    #[test]
    fn upsert_updates_additional_dirs() {
        let (db, pid) = setup_db_with_project();
        let mut session = make_session("Session 1", pid);
        session.additional_dirs = vec![PathBuf::from("/repo2")];

        db.upsert_session(&session).unwrap();

        // Update additional_dirs via second upsert
        session.additional_dirs = vec![PathBuf::from("/repo2"), PathBuf::from("/repo3")];
        db.upsert_session(&session).unwrap();

        let sessions = db.list_active_sessions().unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].additional_dirs.len(), 2);
        assert_eq!(sessions[0].additional_dirs[0], PathBuf::from("/repo2"));
        assert_eq!(sessions[0].additional_dirs[1], PathBuf::from("/repo3"));
    }

    #[test]
    fn session_claude_session_id_preserved() {
        let (db, pid) = setup_db_with_project();
        let mut session = make_session("Session 1", pid);
        session.claude_session_id = Some("claude-abc-123".to_string());

        db.upsert_session(&session).unwrap();

        let sessions = db.list_active_sessions().unwrap();
        assert_eq!(
            sessions[0].claude_session_id,
            Some("claude-abc-123".to_string())
        );
    }
}
