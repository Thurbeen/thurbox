//! Headless session deletion — soft-delete by default, `force` also tears
//! down the tmux window, worktrees, and pending scheduled commands so the
//! filesystem and tmux server don't leak orphans when the TUI isn't
//! running to observe the deletion.

use crate::session::SessionId;
use crate::storage::Database;

/// Outcome of a force-delete, reported to callers for their JSON payload.
#[derive(Debug, Clone, Default)]
pub struct ForceDeleteReport {
    pub killed_window: bool,
    pub removed_worktrees: Vec<String>,
    pub worktree_errors: Vec<String>,
    pub cancelled_scheduled: usize,
}

/// Soft-delete a session and (when `force`) also tear down its runtime
/// resources: the tmux window, on-disk worktrees, and any pending
/// scheduled commands queued against it.
///
/// Worktree and tmux cleanup are best-effort — individual failures are
/// captured in the report but do not abort the delete. The DB row is
/// always soft-deleted last so `Ctrl+U` / `restore_session` can still
/// revive the metadata (the TUI will re-spawn a fresh window on restore).
pub fn delete_session_headless(
    db: &Database,
    session_id: SessionId,
    force: bool,
) -> Result<ForceDeleteReport, String> {
    let session = db
        .get_session_by_id(session_id)
        .map_err(|e| format!("get_session_by_id: {e}"))?
        .ok_or_else(|| format!("Session not found: {session_id}"))?;

    let mut report = ForceDeleteReport::default();

    if force {
        match crate::agent::tmux::kill_window(&session.name) {
            Ok(()) => report.killed_window = true,
            Err(e) => tracing::warn!("kill_window({}) failed: {e}", session.name),
        }

        for wt in &session.worktrees {
            match crate::git::remove_worktree(&wt.repo_path, &wt.worktree_path) {
                Ok(()) => report
                    .removed_worktrees
                    .push(wt.worktree_path.display().to_string()),
                Err(e) => report
                    .worktree_errors
                    .push(format!("{}: {e}", wt.worktree_path.display())),
            }
        }

        report.cancelled_scheduled = db
            .cancel_scheduled_commands_for_session(session_id)
            .map_err(|e| format!("cancel_scheduled_commands_for_session: {e}"))?;
    }

    db.soft_delete_session(session_id)
        .map_err(|e| format!("soft_delete_session: {e}"))?;

    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::SessionId;
    use crate::sync::SharedSession;

    fn insert_session(db: &Database, name: &str) -> SessionId {
        let id = SessionId::default();
        let shared = SharedSession {
            id,
            name: name.into(),
            role: "dev".into(),
            backend_id: String::new(),
            backend_type: "local-tmux".into(),
            agent_session_id: Some(uuid::Uuid::new_v4().to_string()),
            cwd: None,
            additional_dirs: Vec::new(),
            worktrees: Vec::new(),
            shell_backend_id: None,
            tombstone: false,
            tombstone_at: None,
            model: None,
        };
        db.upsert_session(&shared).unwrap();
        id
    }

    #[test]
    fn soft_delete_without_force_leaves_no_side_effects() {
        let db = Database::open_in_memory().unwrap();
        let id = insert_session(&db, "demo");

        // Queue a scheduled command — should NOT be cancelled without force.
        let future = crate::sync::current_time_millis() + 60_000;
        let cmd = db.create_scheduled_command(id, "noop", future).unwrap();

        let report = delete_session_headless(&db, id, false).unwrap();
        assert!(!report.killed_window);
        assert!(report.removed_worktrees.is_empty());
        assert_eq!(report.cancelled_scheduled, 0);

        // Row is soft-deleted.
        assert!(db.get_session_by_id(id).unwrap().is_none());
        // Scheduled command is still pending.
        let pending = db.list_pending_scheduled_commands().unwrap();
        assert!(pending.iter().any(|c| c.id == cmd));
    }

    #[test]
    fn force_delete_cancels_scheduled_commands() {
        let db = Database::open_in_memory().unwrap();
        let id = insert_session(&db, "demo");

        let future = crate::sync::current_time_millis() + 60_000;
        db.create_scheduled_command(id, "a", future).unwrap();
        db.create_scheduled_command(id, "b", future).unwrap();

        let report = delete_session_headless(&db, id, true).unwrap();
        assert_eq!(report.cancelled_scheduled, 2);
        assert!(db.list_pending_scheduled_commands().unwrap().is_empty());
    }

    #[test]
    fn missing_session_errors() {
        let db = Database::open_in_memory().unwrap();
        let err = delete_session_headless(&db, SessionId::default(), false).unwrap_err();
        assert!(err.contains("Session not found"), "got {err}");
    }
}
