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
    pub disabled_automations: usize,
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

        report.disabled_automations = db
            .disable_send_automations_for_session(session_id)
            .map_err(|e| format!("disable_send_automations_for_session: {e}"))?;
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
            agent: "dev".into(),
            backend_id: String::new(),
            backend_type: "local-tmux".into(),
            agent_session_id: Some(uuid::Uuid::new_v4().to_string()),
            cwd: None,
            additional_dirs: Vec::new(),
            worktrees: Vec::new(),
            shell_backend_id: None,
            tombstone: false,
            tombstone_at: None,
        };
        db.upsert_session(&shared).unwrap();
        id
    }

    fn send_automation(db: &Database, session_id: SessionId, name: &str) -> i64 {
        use crate::session::{AutomationAction, AutomationSchedule};
        use crate::storage::automations::NewAutomation;
        db.create_automation(&NewAutomation {
            name: name.into(),
            enabled: true,
            schedule: AutomationSchedule::Once { at: u64::MAX },
            timezone: None,
            action: AutomationAction::Send { session_id },
            prompt: "noop".into(),
            next_run_at: Some(u64::MAX),
        })
        .unwrap()
    }

    #[test]
    fn soft_delete_without_force_leaves_no_side_effects() {
        let db = Database::open_in_memory().unwrap();
        let id = insert_session(&db, "demo");

        // Send automation targeting the session — should survive a soft delete.
        let auto = send_automation(&db, id, "noop");

        let report = delete_session_headless(&db, id, false).unwrap();
        assert!(!report.killed_window);
        assert!(report.removed_worktrees.is_empty());
        assert_eq!(report.disabled_automations, 0);

        // Row is soft-deleted.
        assert!(db.get_session_by_id(id).unwrap().is_none());
        // The automation is still enabled.
        assert!(db.get_automation(auto).unwrap().unwrap().enabled);
    }

    #[test]
    fn force_delete_disables_send_automations() {
        let db = Database::open_in_memory().unwrap();
        let id = insert_session(&db, "demo");

        let a = send_automation(&db, id, "a");
        let b = send_automation(&db, id, "b");

        let report = delete_session_headless(&db, id, true).unwrap();
        assert_eq!(report.disabled_automations, 2);
        assert!(!db.get_automation(a).unwrap().unwrap().enabled);
        assert!(!db.get_automation(b).unwrap().unwrap().enabled);
    }

    #[test]
    fn missing_session_errors() {
        let db = Database::open_in_memory().unwrap();
        let err = delete_session_headless(&db, SessionId::default(), false).unwrap_err();
        assert!(err.contains("Session not found"), "got {err}");
    }
}
