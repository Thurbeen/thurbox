use std::collections::HashMap;
use std::path::PathBuf;

use rusqlite::params;

use crate::session::SessionId;
use crate::sync::{current_time_millis, SharedSession, SharedWorktree};

use super::audit::{AuditAction, EntityType};
use super::Database;

/// The hooks-driven status columns of a session, read in one batch by the TUI
/// each tick to derive [`crate::session::SessionStatus`]. See schema v34.
#[derive(Debug, Clone, Default)]
pub struct HookRow {
    /// `working` / `blocked` / `done` / `idle`, or `None` when no hook has fired
    /// yet. (`idle` and unknown values render as [`crate::session::SessionStatus`]`::Idle`.)
    pub state: Option<String>,
    /// Epoch ms the state was last reported.
    pub state_at: Option<i64>,
    /// Epoch ms the user last "saw" a `done` state (drives Done → Idle).
    pub seen_at: Option<i64>,
}

/// Information about a soft-deleted session, including its worktrees.
#[derive(Debug, Clone)]
pub struct DeletedSessionInfo {
    pub id: SessionId,
    pub name: String,
    pub agent: String,
    pub agent_session_id: Option<String>,
    pub cwd: Option<PathBuf>,
    pub parent_session_id: Option<SessionId>,
    /// Persisted backend (`local-tmux` or `ssh:<host>`). Preserved on restore so
    /// a remote session re-spawns against its own host, not the local default.
    pub backend_type: String,
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
                 parent_session_id = ?9, display_order = ?10, updated_at = ?11, \
                 deleted_at = NULL \
                 WHERE id = ?12",
                params![
                    session.name,
                    session.agent,
                    session.backend_id,
                    session.backend_type,
                    session.agent_session_id,
                    session.cwd.as_ref().map(|p| p.display().to_string()),
                    additional_dirs_str,
                    session.shell_backend_id,
                    session.parent_session_id.map(|id| id.to_string()),
                    session.display_order,
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
                 parent_session_id, display_order, created_at, updated_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
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
                    session.parent_session_id.map(|id| id.to_string()),
                    session.display_order,
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
        self.query_sessions("s.deleted_at IS NULL", [])
    }

    /// `condition` must be a trusted, constant SQL fragment; any caller-supplied
    /// values belong in `params` (bound as `?1`, `?2`, …) — never interpolated
    /// into `condition`, or the query becomes SQL-injectable.
    fn query_sessions(
        &self,
        condition: &str,
        params: impl rusqlite::Params,
    ) -> rusqlite::Result<Vec<SharedSession>> {
        let sql = format!(
            "SELECT s.id, s.name, s.agent, s.backend_id, s.backend_type, \
             s.agent_session_id, s.cwd, s.additional_dirs, s.shell_backend_id, \
             s.parent_session_id, s.display_order, \
             w.repo_path, w.worktree_path, w.branch \
             FROM sessions s \
             LEFT JOIN worktrees w ON s.id = w.session_id AND w.deleted_at IS NULL \
             WHERE {condition} \
             ORDER BY s.display_order IS NULL, s.display_order, s.created_at, w.created_at"
        );

        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(params, row_to_shared_session)?;

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
        let sessions = self.query_sessions(
            "s.deleted_at IS NULL AND s.id = ?1",
            params![id.to_string()],
        )?;
        Ok(sessions.into_iter().next())
    }

    /// Get a single active (non-deleted) session by its name. Names are not
    /// enforced unique; the first match (by display/creation order) is returned,
    /// consistent with [`get_session_by_id`](Self::get_session_by_id).
    pub fn get_session_by_name(&self, name: &str) -> rusqlite::Result<Option<SharedSession>> {
        let sessions =
            self.query_sessions("s.deleted_at IS NULL AND s.name = ?1", params![name])?;
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
        self.query_deleted_sessions("s.deleted_at IS NOT NULL", [])
    }

    /// Get a single soft-deleted session by its ID.
    pub fn get_deleted_session_by_id(
        &self,
        id: SessionId,
    ) -> rusqlite::Result<Option<DeletedSessionInfo>> {
        let sessions = self.query_deleted_sessions(
            "s.deleted_at IS NOT NULL AND s.id = ?1",
            params![id.to_string()],
        )?;
        Ok(sessions.into_iter().next())
    }

    /// `condition` must be a trusted, constant SQL fragment; caller-supplied
    /// values belong in `params` (bound as `?1`, …), never in `condition`.
    fn query_deleted_sessions(
        &self,
        condition: &str,
        params: impl rusqlite::Params,
    ) -> rusqlite::Result<Vec<DeletedSessionInfo>> {
        let sql = format!(
            "SELECT s.id, s.name, s.agent, s.agent_session_id, \
             s.cwd, s.parent_session_id, s.deleted_at, s.backend_type, \
             w.repo_path, w.worktree_path, w.branch \
             FROM sessions s \
             LEFT JOIN worktrees w ON s.id = w.session_id \
             WHERE {condition} \
             ORDER BY s.deleted_at DESC, w.created_at"
        );

        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(params, |row| {
            let id_str: String = row.get(0)?;
            let cwd: Option<String> = row.get(4)?;
            let parent_str: Option<String> = row.get(5)?;
            let deleted_at: i64 = row.get(6)?;
            let backend_type: String = row.get(7)?;
            let wt_repo: Option<String> = row.get(8)?;
            let wt_path: Option<String> = row.get(9)?;
            let wt_branch: Option<String> = row.get(10)?;

            let worktree = worktree_from_cols(wt_repo, wt_path, wt_branch);

            Ok((
                DeletedSessionInfo {
                    id: id_str.parse().unwrap_or_default(),
                    name: row.get(1)?,
                    agent: row.get(2)?,
                    agent_session_id: row.get(3)?,
                    cwd: cwd.map(PathBuf::from),
                    parent_session_id: parent_str.and_then(|s| s.parse().ok()),
                    backend_type,
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

    /// Get a single active (non-deleted) session by its agent conversation id
    /// (`agent_session_id`, the value injected as `THURBOX_SESSION_ID`). Used by
    /// `session signal` as an identity fallback when `$THURBOX_SESSION` is not
    /// available to the hook process (e.g. an agent that sanitizes its env).
    pub fn get_session_by_agent_session_id(
        &self,
        agent_session_id: &str,
    ) -> rusqlite::Result<Option<SharedSession>> {
        let sessions = self.query_sessions(
            "s.deleted_at IS NULL AND s.agent_session_id = ?1",
            params![agent_session_id],
        )?;
        Ok(sessions.into_iter().next())
    }

    /// Record an agent-reported lifecycle state (`working`/`blocked`/`done`) for
    /// a session, stamping `hook_state_at` to now. Written by
    /// `thurbox-cli session signal` (and at spawn, defaulting to `working`).
    ///
    /// Deliberately a targeted UPDATE that touches only the hook columns —
    /// [`upsert_session`](Self::upsert_session) must never list them, so the
    /// TUI's full-row write-back can't clobber a state a headless hook just set.
    pub fn set_hook_state(&self, id: SessionId, state: &str) -> rusqlite::Result<()> {
        let now = current_time_millis() as i64;
        self.conn.execute(
            "UPDATE sessions SET hook_state = ?1, hook_state_at = ?2 \
             WHERE id = ?3 AND deleted_at IS NULL",
            params![state, now, id.to_string()],
        )?;
        Ok(())
    }

    /// Mark a session as "seen" at `at_least` (epoch ms), so a `done` session
    /// the user has looked at renders `Idle` instead of `Done`. The TUI calls
    /// this once, on the transition (when `seen_at < hook_state_at`), never
    /// every tick.
    pub fn mark_session_seen(&self, id: SessionId, at_least: i64) -> rusqlite::Result<()> {
        self.conn.execute(
            "UPDATE sessions SET seen_at = ?1 WHERE id = ?2",
            params![at_least, id.to_string()],
        )?;
        Ok(())
    }

    /// Clear a session's hooks-driven status (NULL all three columns), returning
    /// it to the never-reported `Idle` default. Called on **restart**: the agent
    /// is re-spawned fresh, so a stale `Blocked`/`Working`/`Done` must not linger
    /// until the agent's hooks re-report (which a resumed agent may not do).
    pub fn clear_hook_state(&self, id: SessionId) -> rusqlite::Result<()> {
        self.conn.execute(
            "UPDATE sessions SET hook_state = NULL, hook_state_at = NULL, seen_at = NULL \
             WHERE id = ?1",
            params![id.to_string()],
        )?;
        Ok(())
    }

    /// Load the hook-status columns for every active session in one indexed
    /// scan, keyed by id. Called once per tick by the TUI to derive statuses.
    pub fn load_hook_states(&self) -> rusqlite::Result<HashMap<SessionId, HookRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, hook_state, hook_state_at, seen_at \
             FROM sessions WHERE deleted_at IS NULL",
        )?;
        let rows = stmt.query_map([], |row| {
            let id_str: String = row.get(0)?;
            Ok((
                id_str,
                HookRow {
                    state: row.get(1)?,
                    state_at: row.get(2)?,
                    seen_at: row.get(3)?,
                },
            ))
        })?;

        let mut map = HashMap::new();
        for row in rows {
            let (id_str, hook) = row?;
            if let Ok(id) = id_str.parse::<SessionId>() {
                map.insert(id, hook);
            }
        }
        Ok(map)
    }
}

/// Build an optional [`SharedWorktree`] from the three nullable worktree
/// columns of a joined row. Returns `None` unless all three are present.
fn worktree_from_cols(
    repo: Option<String>,
    path: Option<String>,
    branch: Option<String>,
) -> Option<SharedWorktree> {
    match (repo, path, branch) {
        (Some(repo), Some(path), Some(branch)) => Some(SharedWorktree {
            repo_path: PathBuf::from(repo),
            worktree_path: PathBuf::from(path),
            branch,
        }),
        _ => None,
    }
}

/// Map a single joined `sessions × worktrees` row into a [`SharedSession`]
/// plus its optional worktree (see `query_sessions` for the column order).
fn row_to_shared_session(
    row: &rusqlite::Row<'_>,
) -> rusqlite::Result<(SharedSession, Option<SharedWorktree>)> {
    let id_str: String = row.get(0)?;
    let cwd: Option<String> = row.get(6)?;
    let dirs_str: String = row.get(7)?;
    let shell_backend_id: Option<String> = row.get(8)?;
    let parent_str: Option<String> = row.get(9)?;
    let display_order: Option<i64> = row.get(10)?;
    let wt_repo: Option<String> = row.get(11)?;
    let wt_path: Option<String> = row.get(12)?;
    let wt_branch: Option<String> = row.get(13)?;

    let additional_dirs: Vec<PathBuf> = if dirs_str.is_empty() {
        Vec::new()
    } else {
        dirs_str.split('\n').map(PathBuf::from).collect()
    };

    let worktree = worktree_from_cols(wt_repo, wt_path, wt_branch);

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
            parent_session_id: parent_str.and_then(|s| s.parse().ok()),
            display_order,
            tombstone: false,
            tombstone_at: None,
        },
        worktree,
    ))
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
            parent_session_id: None,
            display_order: None,
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
    fn display_order_roundtrips() {
        let db = Database::open_in_memory().unwrap();
        let mut session = make_session("Session 1");
        session.display_order = Some(3);

        db.upsert_session(&session).unwrap();
        let sessions = db.list_active_sessions().unwrap();
        assert_eq!(sessions[0].display_order, Some(3));

        session.display_order = Some(1);
        db.upsert_session(&session).unwrap();
        let sessions = db.list_active_sessions().unwrap();
        assert_eq!(sessions[0].display_order, Some(1));
    }

    #[test]
    fn list_orders_by_display_order_with_none_last() {
        let db = Database::open_in_memory().unwrap();
        let mut ordered_late = make_session("ordered-late");
        ordered_late.display_order = Some(5);
        let unordered = make_session("unordered");
        let mut ordered_early = make_session("ordered-early");
        ordered_early.display_order = Some(2);

        // Insert in an order that disagrees with display_order on purpose.
        db.upsert_session(&ordered_late).unwrap();
        db.upsert_session(&unordered).unwrap();
        db.upsert_session(&ordered_early).unwrap();

        let names: Vec<String> = db
            .list_active_sessions()
            .unwrap()
            .into_iter()
            .map(|s| s.name)
            .collect();
        assert_eq!(names, ["ordered-early", "ordered-late", "unordered"]);
    }

    #[test]
    fn get_session_by_id_binds_id_as_parameter() {
        // Regression: the id must be bound as a SQL parameter, not interpolated
        // into the WHERE clause. Round-trip a real session by id, and confirm a
        // foreign id selects nothing (a string-interpolated `'{id}'` would have
        // been injectable here).
        let db = Database::open_in_memory().unwrap();
        let session = make_session("Target");
        db.upsert_session(&session).unwrap();

        let found = db.get_session_by_id(session.id).unwrap();
        assert_eq!(found.map(|s| s.name), Some("Target".to_string()));

        let other = db.get_session_by_id(SessionId::default()).unwrap();
        assert!(other.is_none());
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
    fn respawn_reuses_id_revives_one_active_row() {
        // Models `respawn_stale_session`: re-upserting the SAME id after a
        // soft-delete must revive the single row in place (clear deleted_at), not
        // leave a tombstone behind — so a session's identity is stable for life.
        let db = Database::open_in_memory().unwrap();
        let mut session = make_session("worker");
        let sid = session.id;
        db.upsert_session(&session).unwrap();
        db.soft_delete_session(sid).unwrap();
        assert!(db.list_active_sessions().unwrap().is_empty());

        // Respawn: same id, fresh backend_id (as the new tmux pane would have).
        session.backend_id = "thurbox:@9".to_string();
        db.upsert_session(&session).unwrap();

        let active = db.list_active_sessions().unwrap();
        assert_eq!(active.len(), 1, "exactly one active row");
        assert_eq!(active[0].id, sid, "id is stable across the respawn");
        assert_eq!(active[0].backend_id, "thurbox:@9");
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
    fn session_parent_session_id_roundtrips() {
        let db = Database::open_in_memory().unwrap();
        let parent = make_session("Lead");
        let mut child = make_session("Worker");
        child.parent_session_id = Some(parent.id);

        db.upsert_session(&parent).unwrap();
        db.upsert_session(&child).unwrap();

        let found = db.get_session_by_id(child.id).unwrap().unwrap();
        assert_eq!(found.parent_session_id, Some(parent.id));
        let lead = db.get_session_by_id(parent.id).unwrap().unwrap();
        assert_eq!(lead.parent_session_id, None);

        // An update keeps the parent linkage.
        let mut renamed = child.clone();
        renamed.name = "Worker 2".into();
        db.upsert_session(&renamed).unwrap();
        let found = db.get_session_by_id(child.id).unwrap().unwrap();
        assert_eq!(found.name, "Worker 2");
        assert_eq!(found.parent_session_id, Some(parent.id));
    }

    #[test]
    fn deleted_session_preserves_parent_session_id() {
        let db = Database::open_in_memory().unwrap();
        let parent = make_session("Lead");
        let mut child = make_session("Worker");
        child.parent_session_id = Some(parent.id);
        db.upsert_session(&parent).unwrap();
        db.upsert_session(&child).unwrap();

        db.soft_delete_session(child.id).unwrap();
        let deleted = db.get_deleted_session_by_id(child.id).unwrap().unwrap();
        assert_eq!(deleted.parent_session_id, Some(parent.id));

        // Restore keeps the linkage in the active row.
        db.restore_session(child.id).unwrap();
        let restored = db.get_session_by_id(child.id).unwrap().unwrap();
        assert_eq!(restored.parent_session_id, Some(parent.id));
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
    fn deleted_session_preserves_backend_type() {
        // A remote session must restore against its own host, so the persisted
        // `backend_type` has to survive soft-delete + listing.
        let db = Database::open_in_memory().unwrap();
        let mut s = make_session("remote");
        s.backend_type = "ssh:devbox".to_string();
        let sid = s.id;
        db.upsert_session(&s).unwrap();
        db.soft_delete_session(sid).unwrap();

        let deleted = db.get_deleted_session_by_id(sid).unwrap().unwrap();
        assert_eq!(deleted.backend_type, "ssh:devbox");
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

    #[test]
    fn hook_state_roundtrips_and_defaults_null() {
        let db = Database::open_in_memory().unwrap();
        let session = make_session("S1");
        let sid = session.id;
        db.upsert_session(&session).unwrap();

        // No hook fired yet: row exists, hook state is NULL.
        let states = db.load_hook_states().unwrap();
        let hook = states.get(&sid).expect("session present in hook map");
        assert_eq!(hook.state, None);
        assert_eq!(hook.state_at, None);
        assert_eq!(hook.seen_at, None);

        db.set_hook_state(sid, "blocked").unwrap();
        let states = db.load_hook_states().unwrap();
        let hook = states.get(&sid).unwrap();
        assert_eq!(hook.state.as_deref(), Some("blocked"));
        assert!(hook.state_at.is_some());
    }

    #[test]
    fn upsert_does_not_clobber_hook_state() {
        // The TUI's full-row upsert must preserve a state a headless hook set.
        let db = Database::open_in_memory().unwrap();
        let mut session = make_session("S1");
        let sid = session.id;
        db.upsert_session(&session).unwrap();
        db.set_hook_state(sid, "done").unwrap();

        // Simulate the TUI writing the session back (e.g. a rename).
        session.name = "S1-renamed".to_string();
        db.upsert_session(&session).unwrap();

        let states = db.load_hook_states().unwrap();
        assert_eq!(states.get(&sid).unwrap().state.as_deref(), Some("done"));
    }

    #[test]
    fn mark_seen_records_timestamp() {
        let db = Database::open_in_memory().unwrap();
        let session = make_session("S1");
        let sid = session.id;
        db.upsert_session(&session).unwrap();
        db.set_hook_state(sid, "done").unwrap();

        let done_at = db.load_hook_states().unwrap().get(&sid).unwrap().state_at;
        db.mark_session_seen(sid, done_at.unwrap()).unwrap();

        let hook = db.load_hook_states().unwrap();
        let hook = hook.get(&sid).unwrap();
        assert_eq!(hook.seen_at, done_at);
        assert!(hook.seen_at >= hook.state_at);
    }

    #[test]
    fn clear_hook_state_nulls_all_columns() {
        // On restart, a stale Blocked/Working/Done must be wiped so the
        // re-spawned session falls back to the never-reported Idle default.
        let db = Database::open_in_memory().unwrap();
        let session = make_session("S1");
        let sid = session.id;
        db.upsert_session(&session).unwrap();
        db.set_hook_state(sid, "blocked").unwrap();
        let at = db.load_hook_states().unwrap().get(&sid).unwrap().state_at;
        db.mark_session_seen(sid, at.unwrap()).unwrap();

        db.clear_hook_state(sid).unwrap();

        let hook = db.load_hook_states().unwrap();
        let row = hook.get(&sid).unwrap();
        assert_eq!(row.state, None);
        assert_eq!(row.state_at, None);
        assert_eq!(row.seen_at, None);
    }

    #[test]
    fn lookup_by_agent_session_id() {
        let db = Database::open_in_memory().unwrap();
        let mut session = make_session("S1");
        session.agent_session_id = Some("conv-123".to_string());
        let sid = session.id;
        db.upsert_session(&session).unwrap();

        let found = db.get_session_by_agent_session_id("conv-123").unwrap();
        assert_eq!(found.map(|s| s.id), Some(sid));
        assert!(db
            .get_session_by_agent_session_id("nope")
            .unwrap()
            .is_none());
    }
}
