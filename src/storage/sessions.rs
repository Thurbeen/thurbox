use std::path::PathBuf;

use rusqlite::params;

use crate::session::SessionId;
use crate::sync::{current_time_millis, SharedSession, SharedWorktree};

use super::audit::{AuditAction, EntityType};
use super::Database;

/// Information about a soft-deleted session, including its worktrees.
#[derive(Debug, Clone)]
pub struct DeletedSessionInfo {
    pub id: SessionId,
    pub name: String,
    pub agent: String,
    pub agent_session_id: Option<String>,
    pub cwd: Option<PathBuf>,
    pub deleted_at: u64,
    pub worktrees: Vec<SharedWorktree>,
}

impl Database {
    /// Insert or update a session.
    pub fn upsert_session(&self, session: &SharedSession) -> rusqlite::Result<()> {
        let now = current_time_millis() as i64;
        let id_str = session.id.to_string();

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
                "UPDATE sessions SET name = ?1, agent = ?2, \
                 backend_id = ?3, backend_type = ?4, agent_session_id = ?5, \
                 cwd = ?6, additional_dirs = ?7, shell_backend_id = ?8, \
                 updated_at = ?9, deleted_at = NULL \
                 WHERE id = ?10",
                params![
                    session.name,
                    session.agent,
                    session.backend_id,
                    session.backend_type,
                    session.agent_session_id,
                    session.cwd.as_ref().map(|p| p.display().to_string()),
                    additional_dirs_str,
                    session.shell_backend_id,
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
                "INSERT INTO sessions (id, name, agent, backend_id, backend_type, \
                 agent_session_id, cwd, additional_dirs, shell_backend_id, \
                 created_at, updated_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
                params![
                    id_str,
                    session.name,
                    session.agent,
                    session.backend_id,
                    session.backend_type,
                    session.agent_session_id,
                    session.cwd.as_ref().map(|p| p.display().to_string()),
                    additional_dirs_str,
                    session.shell_backend_id,
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

        // Upsert worktrees if present
        if !session.worktrees.is_empty() {
            self.upsert_worktrees(session.id, &session.worktrees)?;
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

    fn query_sessions(&self, condition: &str) -> rusqlite::Result<Vec<SharedSession>> {
        let sql = format!(
            "SELECT s.id, s.name, s.agent, s.backend_id, s.backend_type, \
             s.agent_session_id, s.cwd, s.additional_dirs, s.shell_backend_id, \
             w.repo_path, w.worktree_path, w.branch \
             FROM sessions s \
             LEFT JOIN worktrees w ON s.id = w.session_id AND w.deleted_at IS NULL \
             WHERE {condition} \
             ORDER BY s.created_at, w.created_at"
        );

        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map([], |row| {
            let id_str: String = row.get(0)?;
            let cwd: Option<String> = row.get(6)?;
            let dirs_str: String = row.get(7)?;
            let shell_backend_id: Option<String> = row.get(8)?;
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

            Ok((
                SharedSession {
                    id: id_str.parse().unwrap_or_default(),
                    name: row.get(1)?,
                    agent: row.get(2)?,
                    backend_id: row.get(3)?,
                    backend_type: row.get(4)?,
                    agent_session_id: row.get(5)?,
                    cwd: cwd.map(PathBuf::from),
                    additional_dirs,
                    worktrees: Vec::new(),
                    shell_backend_id,
                    tombstone: false,
                    tombstone_at: None,
                },
                worktree,
            ))
        })?;

        // Collect rows, merging multiple worktree rows into the same session
        let mut sessions: Vec<SharedSession> = Vec::new();
        for row in rows {
            let (session, worktree) = row?;
            if let Some(last) = sessions.last_mut() {
                if last.id == session.id {
                    // Same session — just append the worktree
                    if let Some(wt) = worktree {
                        last.worktrees.push(wt);
                    }
                    continue;
                }
            }
            // New session
            let mut s = session;
            if let Some(wt) = worktree {
                s.worktrees.push(wt);
            }
            sessions.push(s);
        }

        Ok(sessions)
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

    /// Get a single active (non-deleted) session by its ID.
    pub fn get_session_by_id(&self, id: SessionId) -> rusqlite::Result<Option<SharedSession>> {
        let sessions = self.query_sessions(&format!("s.deleted_at IS NULL AND s.id = '{id}'"))?;
        Ok(sessions.into_iter().next())
    }

    /// Get just the name of an active session by its ID.
    pub fn get_session_name(&self, id: SessionId) -> rusqlite::Result<Option<String>> {
        Ok(self
            .conn
            .query_row(
                "SELECT name FROM sessions WHERE id = ?1 AND deleted_at IS NULL",
                params![id.to_string()],
                |row| row.get(0),
            )
            .ok())
    }

    /// List all soft-deleted sessions, most recently deleted first.
    pub fn list_deleted_sessions(&self) -> rusqlite::Result<Vec<DeletedSessionInfo>> {
        self.query_deleted_sessions("s.deleted_at IS NOT NULL")
    }

    /// Get a single soft-deleted session by its ID.
    pub fn get_deleted_session_by_id(
        &self,
        id: SessionId,
    ) -> rusqlite::Result<Option<DeletedSessionInfo>> {
        let sessions =
            self.query_deleted_sessions(&format!("s.deleted_at IS NOT NULL AND s.id = '{id}'"))?;
        Ok(sessions.into_iter().next())
    }

    fn query_deleted_sessions(&self, condition: &str) -> rusqlite::Result<Vec<DeletedSessionInfo>> {
        let sql = format!(
            "SELECT s.id, s.name, s.agent, s.agent_session_id, \
             s.cwd, s.deleted_at, \
             w.repo_path, w.worktree_path, w.branch \
             FROM sessions s \
             LEFT JOIN worktrees w ON s.id = w.session_id \
             WHERE {condition} \
             ORDER BY s.deleted_at DESC, w.created_at"
        );

        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map([], |row| {
            let id_str: String = row.get(0)?;
            let cwd: Option<String> = row.get(4)?;
            let deleted_at: i64 = row.get(5)?;
            let wt_repo: Option<String> = row.get(6)?;
            let wt_path: Option<String> = row.get(7)?;
            let wt_branch: Option<String> = row.get(8)?;

            let worktree = match (wt_repo, wt_path, wt_branch) {
                (Some(repo), Some(path), Some(branch)) => Some(SharedWorktree {
                    repo_path: PathBuf::from(repo),
                    worktree_path: PathBuf::from(path),
                    branch,
                }),
                _ => None,
            };

            Ok((
                DeletedSessionInfo {
                    id: id_str.parse().unwrap_or_default(),
                    name: row.get(1)?,
                    agent: row.get(2)?,
                    agent_session_id: row.get(3)?,
                    cwd: cwd.map(PathBuf::from),
                    deleted_at: deleted_at as u64,
                    worktrees: Vec::new(),
                },
                worktree,
            ))
        })?;

        let mut sessions: Vec<DeletedSessionInfo> = Vec::new();
        for row in rows {
            let (session, worktree) = row?;
            if let Some(last) = sessions.last_mut() {
                if last.id == session.id {
                    if let Some(wt) = worktree {
                        last.worktrees.push(wt);
                    }
                    continue;
                }
            }
            let mut s = session;
            if let Some(wt) = worktree {
                s.worktrees.push(wt);
            }
            sessions.push(s);
        }

        Ok(sessions)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_session(name: &str) -> SharedSession {
        SharedSession {
            id: SessionId::default(),
            name: name.to_string(),
            agent: "claude".to_string(),
            backend_id: "thurbox:@0".to_string(),
            backend_type: "tmux".to_string(),
            agent_session_id: None,
            cwd: None,
            additional_dirs: Vec::new(),
            worktrees: Vec::new(),
            shell_backend_id: None,
            tombstone: false,
            tombstone_at: None,
        }
    }

    #[test]
    fn upsert_and_list_session() {
        let db = Database::open_in_memory().unwrap();
        let session = make_session("Session 1");

        db.upsert_session(&session).unwrap();

        let sessions = db.list_active_sessions().unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].name, "Session 1");
        assert_eq!(sessions[0].agent, "claude");
    }

    #[test]
    fn upsert_updates_existing() {
        let db = Database::open_in_memory().unwrap();
        let mut session = make_session("Session 1");

        db.upsert_session(&session).unwrap();

        session.name = "Renamed".to_string();
        session.agent = "codex".to_string();
        db.upsert_session(&session).unwrap();

        let sessions = db.list_active_sessions().unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].name, "Renamed");
        assert_eq!(sessions[0].agent, "codex");
    }

    #[test]
    fn soft_delete_session() {
        let db = Database::open_in_memory().unwrap();
        let session = make_session("Session 1");
        let sid = session.id;

        db.upsert_session(&session).unwrap();
        db.soft_delete_session(sid).unwrap();

        let active = db.list_active_sessions().unwrap();
        assert!(active.is_empty());
    }

    #[test]
    fn restore_session() {
        let db = Database::open_in_memory().unwrap();
        let session = make_session("Session 1");
        let sid = session.id;

        db.upsert_session(&session).unwrap();
        db.soft_delete_session(sid).unwrap();
        db.restore_session(sid).unwrap();

        let active = db.list_active_sessions().unwrap();
        assert_eq!(active.len(), 1);
    }

    #[test]
    fn session_with_worktree() {
        let db = Database::open_in_memory().unwrap();
        let mut session = make_session("Session 1");
        session.worktrees = vec![SharedWorktree {
            repo_path: PathBuf::from("/repo"),
            worktree_path: PathBuf::from("/repo/.git/wt/feat"),
            branch: "feat".to_string(),
        }];

        db.upsert_session(&session).unwrap();

        let sessions = db.list_active_sessions().unwrap();
        assert_eq!(sessions[0].worktrees.len(), 1);
        assert_eq!(sessions[0].worktrees[0].branch, "feat");
    }

    #[test]
    fn session_with_multiple_worktrees() {
        let db = Database::open_in_memory().unwrap();
        let mut session = make_session("Session 1");
        session.worktrees = vec![
            SharedWorktree {
                repo_path: PathBuf::from("/repo1"),
                worktree_path: PathBuf::from("/repo1/.git/wt/feat"),
                branch: "feat".to_string(),
            },
            SharedWorktree {
                repo_path: PathBuf::from("/repo2"),
                worktree_path: PathBuf::from("/repo2/.git/wt/feat"),
                branch: "feat".to_string(),
            },
        ];

        db.upsert_session(&session).unwrap();

        let sessions = db.list_active_sessions().unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].worktrees.len(), 2);
    }

    #[test]
    fn session_with_cwd() {
        let db = Database::open_in_memory().unwrap();
        let mut session = make_session("Session 1");
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
        let db = Database::open_in_memory().unwrap();
        let mut session = make_session("Session 1");
        session.additional_dirs = vec![
            PathBuf::from("/home/user/repo2"),
            PathBuf::from("/home/user/repo3"),
        ];

        db.upsert_session(&session).unwrap();

        let sessions = db.list_active_sessions().unwrap();
        assert_eq!(sessions[0].additional_dirs.len(), 2);
    }

    #[test]
    fn get_session_by_id_found() {
        let db = Database::open_in_memory().unwrap();
        let session = make_session("Session 1");
        let sid = session.id;
        db.upsert_session(&session).unwrap();

        let result = db.get_session_by_id(sid).unwrap();
        assert!(result.is_some());
        assert_eq!(result.unwrap().name, "Session 1");
    }

    #[test]
    fn get_session_by_id_not_found() {
        let db = Database::open_in_memory().unwrap();
        let result = db.get_session_by_id(SessionId::default()).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn get_session_by_id_excludes_deleted() {
        let db = Database::open_in_memory().unwrap();
        let session = make_session("Session 1");
        let sid = session.id;
        db.upsert_session(&session).unwrap();
        db.soft_delete_session(sid).unwrap();

        let result = db.get_session_by_id(sid).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn get_session_name_found() {
        let db = Database::open_in_memory().unwrap();
        let session = make_session("Session 1");
        let sid = session.id;
        db.upsert_session(&session).unwrap();

        let name = db.get_session_name(sid).unwrap();
        assert_eq!(name.as_deref(), Some("Session 1"));
    }

    #[test]
    fn session_agent_session_id_preserved() {
        let db = Database::open_in_memory().unwrap();
        let mut session = make_session("Session 1");
        session.agent_session_id = Some("claude-abc-123".to_string());

        db.upsert_session(&session).unwrap();

        let sessions = db.list_active_sessions().unwrap();
        assert_eq!(
            sessions[0].agent_session_id,
            Some("claude-abc-123".to_string())
        );
    }

    #[test]
    fn list_deleted_sessions() {
        let db = Database::open_in_memory().unwrap();
        let s1 = make_session("S1");
        let s2 = make_session("S2");
        let s1_id = s1.id;
        db.upsert_session(&s1).unwrap();
        db.upsert_session(&s2).unwrap();
        db.soft_delete_session(s1_id).unwrap();

        let deleted = db.list_deleted_sessions().unwrap();
        assert_eq!(deleted.len(), 1);
        assert_eq!(deleted[0].id, s1_id);
        assert_eq!(deleted[0].name, "S1");
    }

    #[test]
    fn get_deleted_session_by_id_found() {
        let db = Database::open_in_memory().unwrap();
        let session = make_session("S1");
        let sid = session.id;
        db.upsert_session(&session).unwrap();
        db.soft_delete_session(sid).unwrap();

        let result = db.get_deleted_session_by_id(sid).unwrap();
        assert!(result.is_some());
        assert_eq!(result.unwrap().name, "S1");
    }

    #[test]
    fn restore_clears_from_deleted_list() {
        let db = Database::open_in_memory().unwrap();
        let session = make_session("S1");
        let sid = session.id;
        db.upsert_session(&session).unwrap();
        db.soft_delete_session(sid).unwrap();
        db.restore_session(sid).unwrap();

        let deleted = db.list_deleted_sessions().unwrap();
        assert!(deleted.is_empty());

        let active = db.list_active_sessions().unwrap();
        assert_eq!(active.len(), 1);
    }
}
