use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use rusqlite::{params, OptionalExtension};

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
    /// The pane id (`%N`) the session held when it was deleted — what the reap
    /// kills, so a live session sharing the name is never the one torn down.
    /// Empty for rows persisted before local spawns recorded an id.
    pub backend_id: String,
    pub deleted_at: u64,
    /// Whether this row was hard-deleted (tmux window + worktrees torn down). A
    /// force-deleted session is shown in the restore list but cannot be restored
    /// — its worktrees (and any uncommitted work) are gone. See schema v37.
    pub force_deleted: bool,
    pub worktrees: Vec<SharedWorktree>,
}

impl Database {
    /// Insert or update a session.
    ///
    /// Deliberately a single atomic `INSERT … ON CONFLICT(id) DO UPDATE` and
    /// never lists the hook columns (`hook_state`/`hook_state_at`/`seen_at`), so
    /// the TUI's full-row write-back can't clobber a state a headless hook just
    /// set (see [`set_hook_state`](Self::set_hook_state)). `created_at` is set
    /// only on insert; a conflict revives a soft-deleted row (`deleted_at =
    /// NULL`). The pre-write existence check decides only the audit label and
    /// can't make the write race — the UPSERT handles both cases regardless.
    ///
    /// The whole write — row, audits, worktree replacement — is one transaction:
    /// each statement in autocommit was its own WAL commit, so a single upsert
    /// cost N+5 `data_version` bumps and re-triggered every peer process's
    /// refresh that many times. (`unchecked_transaction` because `Database`
    /// methods take `&self`; the connection is not shared across threads.)
    pub fn upsert_session(&self, session: &SharedSession) -> rusqlite::Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        let now = current_time_millis() as i64;
        let id_str = session.id.to_string();

        let existed = self
            .conn
            .query_row(
                "SELECT 1 FROM sessions WHERE id = ?1",
                params![id_str],
                |_| Ok(()),
            )
            .optional()?
            .is_some();

        self.conn.execute(
            "INSERT INTO sessions (id, name, agent, backend_id, backend_type, \
             agent_session_id, cwd, additional_dirs, shell_backend_id, \
             parent_session_id, display_order, created_at, updated_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?12) \
             ON CONFLICT(id) DO UPDATE SET \
                 name = excluded.name, agent = excluded.agent, \
                 backend_id = excluded.backend_id, \
                 backend_type = excluded.backend_type, \
                 agent_session_id = excluded.agent_session_id, \
                 cwd = excluded.cwd, additional_dirs = excluded.additional_dirs, \
                 shell_backend_id = excluded.shell_backend_id, \
                 parent_session_id = excluded.parent_session_id, \
                 display_order = excluded.display_order, \
                 updated_at = excluded.updated_at, deleted_at = NULL",
            params![
                id_str,
                session.name,
                session.agent,
                session.backend_id,
                session.backend_type,
                session.agent_session_id,
                session
                    .cwd
                    .as_ref()
                    .map(|p| p.to_string_lossy().into_owned()),
                additional_dirs_to_db(&session.additional_dirs),
                session.shell_backend_id,
                session.parent_session_id.map(|id| id.to_string()),
                session.display_order,
                now,
            ],
        )?;

        if existed {
            self.log_audit(
                EntityType::Session,
                &id_str,
                AuditAction::Updated,
                None,
                None,
                None,
            )?;
        } else {
            self.log_audit(
                EntityType::Session,
                &id_str,
                AuditAction::Created,
                None,
                None,
                Some(&session.name),
            )?;
        }

        if !session.worktrees.is_empty() {
            self.upsert_worktrees(session.id, &session.worktrees)?;
        }

        tx.commit()
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

    /// Mark a soft-deleted session as force-deleted: its tmux window + worktrees
    /// were torn down, so it can't be restored (schema v37). Safe to call after
    /// [`soft_delete_session`](Self::soft_delete_session); idempotent.
    pub fn mark_session_force_deleted(&self, id: SessionId) -> rusqlite::Result<()> {
        self.conn.execute(
            "UPDATE sessions SET force_deleted = 1 WHERE id = ?1",
            params![id.to_string()],
        )?;
        Ok(())
    }

    /// Restore a soft-deleted session.
    pub fn restore_session(&self, id: SessionId) -> rusqlite::Result<()> {
        let now = current_time_millis() as i64;
        let id_str = id.to_string();

        // Clear `force_deleted` defensively — the app layer blocks restoring a
        // force-deleted row, so this only matters if a future caller revives one.
        self.conn.execute(
            "UPDATE sessions SET deleted_at = NULL, force_deleted = 0, updated_at = ?1 WHERE id = ?2 AND deleted_at IS NOT NULL",
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

        // `prepare_cached` keys on the SQL text, and every `condition` is a
        // constant fragment — so each variant compiles once and this per-refresh
        // read (ADR-P6) skips the re-parse thereafter.
        let mut stmt = self.conn.prepare_cached(&sql)?;
        let rows = stmt.query_map(params, row_to_shared_session)?;

        // Collect rows, merging multiple worktree rows into the same session
        let mut sessions: Vec<SharedSession> = Vec::new();
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

    /// Persist the launch recipe of a **command session** — one created from a
    /// raw command rather than an `agents.toml` entry.
    ///
    /// Deliberately not part of [`upsert_session`](Self::upsert_session), for
    /// the same reason `hook_state` and `base_branch` are not: a peer mirroring
    /// this machine (ADR-24) round-trips sessions through that upsert, and a
    /// peer on an older release sends JSON with no recipe in it. A column the
    /// upsert wrote would be cleared on the next sync tick, so the ones only
    /// this instance owns are written by name, here.
    pub fn set_launch_recipe(
        &self,
        id: SessionId,
        recipe: &crate::session::LaunchRecipe,
    ) -> rusqlite::Result<()> {
        let args = serde_json::to_string(&recipe.args).unwrap_or_else(|_| "[]".into());
        let env = serde_json::to_string(&recipe.env).unwrap_or_else(|_| "{}".into());
        self.conn.execute(
            "UPDATE sessions SET launch_command = ?1, launch_args = ?2, launch_env = ?3 \
             WHERE id = ?4",
            params![recipe.command, args, env, id.to_string()],
        )?;
        Ok(())
    }

    /// Persist the extra environment a session was created with (`--env`),
    /// whatever it launches.
    ///
    /// Separate from [`set_launch_recipe`](Self::set_launch_recipe) because the
    /// two answer different questions. A recipe says *what to run* and is only
    /// meaningful for a command session — persisting one for a registry agent
    /// would freeze it at creation time. An environment says what the session's
    /// processes live in, and that is equally true of both: `--env FOO=1` on an
    /// `--agent` session used to be applied at spawn and then forgotten, so the
    /// first restart silently dropped it and nothing could report it back.
    pub fn set_launch_env(
        &self,
        id: SessionId,
        env: &std::collections::BTreeMap<String, String>,
    ) -> rusqlite::Result<()> {
        let json = serde_json::to_string(env).unwrap_or_else(|_| "{}".into());
        self.conn.execute(
            "UPDATE sessions SET launch_env = ?1 WHERE id = ?2",
            params![json, id.to_string()],
        )?;
        Ok(())
    }

    /// The extra environment a session was created with — empty when it was
    /// created with none, or when the row predates the column being written for
    /// registry agents.
    pub fn load_launch_env(
        &self,
        id: SessionId,
    ) -> rusqlite::Result<std::collections::BTreeMap<String, String>> {
        let stored: Option<Option<String>> = self
            .conn
            .query_row(
                "SELECT launch_env FROM sessions WHERE id = ?1",
                params![id.to_string()],
                |row| row.get(0),
            )
            .optional()?;
        Ok(stored
            .flatten()
            .and_then(|e| serde_json::from_str(&e).ok())
            .unwrap_or_default())
    }

    /// The persisted launch recipe, or `None` for a registry agent.
    ///
    /// `None` is the discriminant the restart path reads: no recipe means the
    /// session names an agent, which is resolved from `agents.toml` afresh on
    /// every launch so an edit there is picked up by a restart.
    pub fn load_launch_recipe(
        &self,
        id: SessionId,
    ) -> rusqlite::Result<Option<crate::session::LaunchRecipe>> {
        let row: Option<(Option<String>, Option<String>, Option<String>)> = self
            .conn
            .query_row(
                "SELECT launch_command, launch_args, launch_env FROM sessions WHERE id = ?1",
                params![id.to_string()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?;
        let Some((Some(command), args, env)) = row else {
            return Ok(None);
        };
        if command.is_empty() {
            return Ok(None);
        }
        Ok(Some(crate::session::LaunchRecipe {
            command,
            args: args
                .and_then(|a| serde_json::from_str(&a).ok())
                .unwrap_or_default(),
            env: env
                .and_then(|e| serde_json::from_str(&e).ok())
                .unwrap_or_default(),
        }))
    }

    /// Mark a session stopped (`true`) or running again (`false`).
    ///
    /// The mark is what separates "parked on purpose" from "its window died",
    /// which several subsystems otherwise repair on sight.
    pub fn set_session_stopped(&self, id: SessionId, stopped: bool) -> rusqlite::Result<()> {
        let at = stopped.then(|| current_time_millis() as i64);
        self.conn.execute(
            "UPDATE sessions SET stopped_at = ?1, updated_at = ?2 WHERE id = ?3",
            params![at, current_time_millis() as i64, id.to_string()],
        )?;
        Ok(())
    }

    /// When a session was stopped, or `None` if it is not stopped.
    pub fn session_stopped_at(&self, id: SessionId) -> rusqlite::Result<Option<u64>> {
        let at: Option<Option<i64>> = self
            .conn
            .query_row(
                "SELECT stopped_at FROM sessions WHERE id = ?1",
                params![id.to_string()],
                |row| row.get(0),
            )
            .optional()?;
        Ok(at.flatten().map(|v| v as u64))
    }

    /// Every stopped session, as a set. Loaded in one query beside
    /// [`load_hook_states`](Self::load_hook_states) by the passes that must not
    /// resurrect a parked session.
    pub fn load_stopped_sessions(&self) -> rusqlite::Result<HashSet<SessionId>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT id FROM sessions WHERE deleted_at IS NULL AND stopped_at IS NOT NULL",
        )?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
        let mut set = HashSet::new();
        for row in rows {
            if let Ok(id) = row?.parse::<SessionId>() {
                set.insert(id);
            }
        }
        Ok(set)
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
        Ok(self.find_sessions_by_name(name)?.into_iter().next())
    }

    /// Every active session with this exact name.
    ///
    /// Names are not unique — two sessions can carry one, and a mirrored host
    /// legitimately contributes rows that collide with local ones. A caller
    /// resolving a name a user typed needs to know that happened rather than
    /// silently acting on whichever row sorted first, so this returns all of
    /// them and lets the caller refuse.
    pub fn find_sessions_by_name(&self, name: &str) -> rusqlite::Result<Vec<SharedSession>> {
        self.query_sessions("s.deleted_at IS NULL AND s.name = ?1", params![name])
    }

    /// Active sessions whose id **starts with** `prefix`, for addressing a
    /// session by the first few characters of its UUID.
    pub fn find_sessions_by_id_prefix(&self, prefix: &str) -> rusqlite::Result<Vec<SharedSession>> {
        if prefix.is_empty() {
            return Ok(Vec::new());
        }
        let escaped = prefix
            .replace('\\', "\\\\")
            .replace('%', "\\%")
            .replace('_', "\\_");
        self.query_sessions(
            "s.deleted_at IS NULL AND s.id LIKE ?1 || '%' ESCAPE '\\'",
            params![escaped],
        )
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
             s.force_deleted, s.backend_id, \
             w.repo_path, w.worktree_path, w.branch \
             FROM sessions s \
             LEFT JOIN worktrees w ON s.id = w.session_id \
             WHERE {condition} \
             ORDER BY s.deleted_at DESC, w.created_at"
        );

        // Cached for the same reason as `query_sessions`: read per refresh,
        // constant SQL per condition.
        let mut stmt = self.conn.prepare_cached(&sql)?;
        let rows = stmt.query_map(params, |row| {
            let id_str: String = row.get(0)?;
            let cwd: Option<String> = row.get(4)?;
            let parent_str: Option<String> = row.get(5)?;
            let deleted_at: i64 = row.get(6)?;
            let backend_type: String = row.get(7)?;
            let force_deleted: i64 = row.get(8)?;
            let backend_id: String = row.get(9)?;
            let wt_repo: Option<String> = row.get(10)?;
            let wt_path: Option<String> = row.get(11)?;
            let wt_branch: Option<String> = row.get(12)?;

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
                    backend_id,
                    deleted_at: deleted_at as u64,
                    force_deleted: force_deleted != 0,
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

    /// Record the base branch a session's worktree was forked from. A targeted
    /// UPDATE that [`upsert_session`](Self::upsert_session) never lists (base
    /// branch is write-once at spawn), so the TUI's periodic full-row write-back
    /// can't clobber it — same pattern as the hook columns. Scopes the
    /// code-review view to `<base>..HEAD`.
    pub fn set_session_base_branch(&self, id: SessionId, base: &str) -> rusqlite::Result<()> {
        self.conn.execute(
            "UPDATE sessions SET base_branch = ?1 WHERE id = ?2",
            params![base, id.to_string()],
        )?;
        Ok(())
    }

    /// Read a session's persisted base branch (the fork point of its worktree),
    /// or `None` when never recorded (legacy rows / non-worktree sessions).
    pub fn get_session_base_branch(&self, id: SessionId) -> rusqlite::Result<Option<String>> {
        use rusqlite::OptionalExtension;
        self.conn
            .query_row(
                "SELECT base_branch FROM sessions WHERE id = ?1",
                params![id.to_string()],
                |row| row.get::<_, Option<String>>(0),
            )
            .optional()
            .map(Option::flatten)
    }

    /// Record the pane id (`%N`) of a session's agent window. A targeted UPDATE
    /// like [`set_session_shell`](Self::set_session_shell) — one column, no
    /// worktree churn. Used to refresh a stale or missing id (a legacy row the
    /// interface just resolved by window name, a restore's fresh spawn);
    /// ordinary spawns persist theirs through
    /// [`upsert_session`](Self::upsert_session). Returns whether a row matched.
    pub fn set_backend_id(&self, id: SessionId, pane: &str) -> rusqlite::Result<bool> {
        let mut stmt = self.conn.prepare_cached(
            "UPDATE sessions SET backend_id = ?1 \
             WHERE id = ?2 AND deleted_at IS NULL",
        )?;
        let updated = stmt.execute(params![pane, id.to_string()])?;
        Ok(updated > 0)
    }

    /// Record — or clear — the pane id of a session's companion shell. A
    /// targeted UPDATE like [`set_hook_state`](Self::set_hook_state): rewriting
    /// the whole row via [`upsert_session`](Self::upsert_session) to change one
    /// column also churned the worktree rows and cost a commit per statement.
    /// Returns whether a row matched, so the caller can report an unknown
    /// session.
    pub fn set_session_shell(&self, id: SessionId, pane: Option<&str>) -> rusqlite::Result<bool> {
        let mut stmt = self.conn.prepare_cached(
            "UPDATE sessions SET shell_backend_id = ?1 \
             WHERE id = ?2 AND deleted_at IS NULL",
        )?;
        let updated = stmt.execute(params![pane, id.to_string()])?;
        Ok(updated > 0)
    }

    /// Mark a session as "seen" at `at_least` (epoch ms), so a `done` session
    /// the user has looked at renders `Idle` instead of `Done`. The TUI calls
    /// this once, on the transition (when `seen_at < hook_state_at`), never
    /// every tick.
    pub fn mark_session_seen(&self, id: SessionId, at_least: i64) -> rusqlite::Result<()> {
        self.conn.execute(
            "UPDATE sessions SET seen_at = ?1 WHERE id = ?2 AND deleted_at IS NULL",
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
    /// scan, keyed by id. The TUI derives statuses from this but reloads only
    /// when `data_version` moves (see `App::refresh_session_statuses`), so it
    /// is not run on every tick.
    pub fn load_hook_states(&self) -> rusqlite::Result<HashMap<SessionId, HookRow>> {
        // `prepare_cached` keeps the compiled statement across reloads — this is
        // a hot query (the TUI's status refresh), so skipping the re-parse on
        // every reload is worthwhile.
        let mut stmt = self.conn.prepare_cached(
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

    /// One session's hook-status columns, or `None` for a missing or deleted
    /// row. The point lookup behind `SnapshotStore::acknowledge`, which asks
    /// about exactly one session and has no use for the full
    /// [`load_hook_states`](Self::load_hook_states) scan.
    pub fn load_hook_state(&self, id: SessionId) -> rusqlite::Result<Option<HookRow>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT hook_state, hook_state_at, seen_at \
             FROM sessions WHERE id = ?1 AND deleted_at IS NULL",
        )?;
        stmt.query_row(params![id.to_string()], |row| {
            Ok(HookRow {
                state: row.get(0)?,
                state_at: row.get(1)?,
                seen_at: row.get(2)?,
            })
        })
        .optional()
    }

    /// Load every active session's base branch in one indexed scan, keyed by id.
    ///
    /// The sibling of [`Self::load_hook_states`], and read on the same schedule
    /// for the same reason: a diff is taken *against* this, so a snapshot that
    /// does not carry it cannot describe what it diffed. Rows with no base are
    /// omitted rather than mapped to an empty string — "never recorded" is a
    /// real answer, and the caller shows uncommitted changes instead.
    pub fn load_base_branches(&self) -> rusqlite::Result<HashMap<SessionId, String>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT id, base_branch FROM sessions \
             WHERE deleted_at IS NULL AND base_branch IS NOT NULL",
        )?;
        let rows = stmt.query_map([], |row| {
            let id_str: String = row.get(0)?;
            let base: String = row.get(1)?;
            Ok((id_str, base))
        })?;

        let mut map = HashMap::new();
        for row in rows {
            let (id_str, base) = row?;
            if let Ok(id) = id_str.parse::<SessionId>() {
                map.insert(id, base);
            }
        }
        Ok(map)
    }
}

/// Encode a session's `additional_dirs` for the column: a JSON array of path
/// strings (empty list → `''`, satisfying the `NOT NULL` column). JSON keeps a
/// path containing a newline from round-tripping as two separate dirs, which the
/// previous `\n`-joined encoding could not.
fn additional_dirs_to_db(dirs: &[PathBuf]) -> String {
    if dirs.is_empty() {
        return String::new();
    }
    let strs: Vec<String> = dirs
        .iter()
        .map(|p| p.to_string_lossy().into_owned())
        .collect();
    // A `Vec<String>` always serializes; the fallback is unreachable.
    serde_json::to_string(&strs).unwrap_or_default()
}

/// Decode the `additional_dirs` column. New rows store a JSON array; legacy rows
/// are newline-delimited, so a failed JSON parse falls back to splitting on `\n`.
fn additional_dirs_from_db(raw: &str) -> Vec<PathBuf> {
    if raw.is_empty() {
        return Vec::new();
    }
    match serde_json::from_str::<Vec<String>>(raw) {
        Ok(list) => list.into_iter().map(PathBuf::from).collect(),
        Err(_) => raw.split('\n').map(PathBuf::from).collect(),
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

    let additional_dirs = additional_dirs_from_db(&dirs_str);

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

    /// The bulk read the snapshot uses, and the reason it omits rather than
    /// defaults: a session with no recorded base is diffed against its own
    /// uncommitted changes, and an empty string would name a branch called "".
    #[test]
    fn bases_load_in_bulk_and_omit_the_sessions_that_have_none() {
        let db = Database::open_in_memory().unwrap();
        let with_base = make_session("has-base");
        let without = make_session("no-base");
        db.upsert_session(&with_base).unwrap();
        db.upsert_session(&without).unwrap();
        db.set_session_base_branch(with_base.id, "origin/main")
            .unwrap();

        let bases = db.load_base_branches().unwrap();
        assert_eq!(
            bases.get(&with_base.id).map(String::as_str),
            Some("origin/main")
        );
        assert!(
            !bases.contains_key(&without.id),
            "a session with no base must be absent, not empty: {bases:?}"
        );
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
    fn find_by_id_prefix_treats_an_empty_prefix_as_no_match() {
        // Regression: an empty reference must never resolve to "every active
        // session" — a caller passing on a blank id (e.g. an upstream bug)
        // must get "not found", not a silent match against whatever exists.
        let db = Database::open_in_memory().unwrap();
        db.upsert_session(&make_session("only-one")).unwrap();

        let found = db.find_sessions_by_id_prefix("").unwrap();
        assert!(
            found.is_empty(),
            "an empty prefix must not match every session, got {found:?}"
        );
    }

    #[test]
    fn find_by_id_prefix_treats_wildcard_characters_literally() {
        // Regression: a prefix containing `%` or `_` must be matched as a
        // literal string, not interpreted as a SQL LIKE wildcard.
        let db = Database::open_in_memory().unwrap();
        let session = make_session("wild");
        db.upsert_session(&session).unwrap();
        let id_str = session.id.to_string();
        let real_prefix = &id_str[..8];

        // Swap the prefix's last character for `_`, a single-character LIKE
        // wildcard. Interpreted as a wildcard it still matches the real id
        // (any character stands in for the one it replaced); interpreted
        // literally — the fix — it can't, since a UUID has no underscore.
        let mut wildcard_prefix = real_prefix[..7].to_string();
        wildcard_prefix.push('_');
        assert!(
            db.find_sessions_by_id_prefix(&wildcard_prefix)
                .unwrap()
                .is_empty(),
            "a literal '_' in the prefix must not act as a single-character wildcard"
        );

        let found = db.find_sessions_by_id_prefix(real_prefix).unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].id, session.id);
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
    fn additional_dirs_with_newline_roundtrips_as_one_dir() {
        // A path containing a newline must survive as a single dir — the old
        // `\n`-joined encoding split it into two.
        let db = Database::open_in_memory().unwrap();
        let mut session = make_session("Session 1");
        let weird = PathBuf::from("/home/user/odd\nname");
        session.additional_dirs = vec![weird.clone()];

        db.upsert_session(&session).unwrap();

        let sessions = db.list_active_sessions().unwrap();
        assert_eq!(sessions[0].additional_dirs, vec![weird]);
    }

    #[test]
    fn additional_dirs_reads_legacy_newline_rows() {
        // Rows written before the JSON encoding are newline-delimited; they must
        // still decode (best-effort, no migration).
        let dirs = additional_dirs_from_db("/a\n/b\n/c");
        assert_eq!(
            dirs,
            vec![
                PathBuf::from("/a"),
                PathBuf::from("/b"),
                PathBuf::from("/c")
            ]
        );
        assert!(additional_dirs_from_db("").is_empty());
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
    fn load_hook_state_reads_one_row() {
        let db = Database::open_in_memory().unwrap();
        let session = make_session("S1");
        let sid = session.id;
        db.upsert_session(&session).unwrap();
        db.set_hook_state(sid, "working").unwrap();

        let hook = db.load_hook_state(sid).unwrap().expect("row present");
        assert_eq!(hook.state.as_deref(), Some("working"));
        assert!(hook.state_at.is_some());

        // A missing session is None, not an empty row.
        assert!(db.load_hook_state(SessionId::default()).unwrap().is_none());
        db.soft_delete_session(sid).unwrap();
        assert!(db.load_hook_state(sid).unwrap().is_none());
    }

    #[test]
    fn set_session_shell_roundtrips_without_touching_worktrees() {
        let db = Database::open_in_memory().unwrap();
        let mut session = make_session("S1");
        session.worktrees = vec![SharedWorktree {
            repo_path: PathBuf::from("/repo"),
            worktree_path: PathBuf::from("/repo/.git/wt/feat"),
            branch: "feat".to_string(),
        }];
        let sid = session.id;
        db.upsert_session(&session).unwrap();

        assert!(db.set_session_shell(sid, Some("%7")).unwrap());
        let row = db.get_session_by_id(sid).unwrap().unwrap();
        assert_eq!(row.shell_backend_id.as_deref(), Some("%7"));
        assert_eq!(row.worktrees.len(), 1, "worktree rows untouched");

        assert!(db.set_session_shell(sid, None).unwrap());
        let row = db.get_session_by_id(sid).unwrap().unwrap();
        assert_eq!(row.shell_backend_id, None);

        // An unknown session reports no match rather than succeeding silently.
        assert!(!db.set_session_shell(SessionId::default(), None).unwrap());
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
