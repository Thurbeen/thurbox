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
        force_teardown(db, session_id, &session, &mut report)?;
    }

    db.soft_delete_session(session_id)
        .map_err(|e| format!("soft_delete_session: {e}"))?;

    Ok(report)
}

/// Tear down a session's runtime resources for a force-delete: kill the tmux
/// window, remove worktrees + the symlink workspace, and disable any pending
/// `Send` automations. Best-effort cleanup is recorded in `report`; only the
/// automation-disable failure (a DB error) aborts.
fn force_teardown(
    db: &Database,
    session_id: SessionId,
    session: &crate::sync::SharedSession,
    report: &mut ForceDeleteReport,
) -> Result<(), String> {
    // Capture the pane's OS pid *before* the kill so we can reap the pane's child
    // process below. Windows refuses to remove a directory that is a live
    // process's cwd, and a session's agent runs with cwd = its worktree /
    // extension home; Unix has no such restriction, so this is Windows-only.
    #[cfg(windows)]
    let pane_pid = crate::agent::tmux::window_pane_pid(&session.name)
        .ok()
        .flatten();

    match crate::agent::tmux::kill_window(&session.name) {
        Ok(()) => report.killed_window = true,
        Err(e) => tracing::warn!("kill_window({}) failed: {e}", session.name),
    }

    // `kill-window` returns before the OS reaps the pane's child process; wait
    // for it (force-terminating as a backstop) before the rmdir steps below.
    // NOTE: this only handles a handle held by the *pane child*. psmux ALSO holds
    // a server-level handle to each pane's `-c` cwd that only `kill-server`
    // releases (verified in the Windows VM) — which we can't do per-session on
    // the shared server. So removing a just-deleted session's own working dir can
    // still fail on Windows; that is a documented psmux limitation, not covered
    // here.
    #[cfg(windows)]
    if let Some(pid) = pane_pid {
        reap_pane_process(pid);
    }

    for wt in &session.worktrees {
        remove_worktree_into(wt, report);
    }

    // Tear down the multi-repo symlink workspace (if any). Only the symlinks
    // are removed — the underlying repos are untouched.
    if let Some(asid) = &session.agent_session_id {
        if let Err(e) = crate::workspace::remove_workspace(asid) {
            tracing::warn!("remove_workspace({asid}) failed: {e}");
        }
    }

    report.disabled_automations = db
        .disable_send_automations_for_session(session_id)
        .map_err(|e| format!("disable_send_automations_for_session: {e}"))?;

    Ok(())
}

/// Wait (≈5s) for a killed pane's child process to exit, force-terminating it as
/// a backstop if it outlives the grace period. Windows-only: a live process's
/// cwd is unremovable on Windows, and a session's agent runs in its worktree.
/// This reaps the *pane child* only — psmux's own server-level handle on the
/// pane's `-c` cwd is a separate, un-fixable-per-session issue (see callsite).
#[cfg(windows)]
fn reap_pane_process(pid: u32) {
    let pid = sysinfo::Pid::from_u32(pid);
    let mut sys = sysinfo::System::new();
    let kind = sysinfo::ProcessRefreshKind::nothing();
    let mut terminated = false;
    for tick in 0..50 {
        // `remove_dead_processes = true` drops exited pids, so `process(pid)`
        // going `None` means the process is truly gone.
        sys.refresh_processes_specifics(sysinfo::ProcessesToUpdate::Some(&[pid]), true, kind);
        match sys.process(pid) {
            None => return,
            Some(proc) => {
                // Give it ~2s to exit on its own, then force the kill.
                if !terminated && tick >= 20 {
                    proc.kill();
                    terminated = true;
                }
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}

/// Best-effort worktree removal, recording success/failure into `report`.
fn remove_worktree_into(wt: &crate::sync::SharedWorktree, report: &mut ForceDeleteReport) {
    match crate::git::remove_worktree(&wt.repo_path, &wt.worktree_path) {
        Ok(()) => report
            .removed_worktrees
            .push(wt.worktree_path.display().to_string()),
        Err(e) => report
            .worktree_errors
            .push(format!("{}: {e}", wt.worktree_path.display())),
    }
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
            parent_session_id: None,
            display_order: None,
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

        assert!(db.get_session_by_id(id).unwrap().is_none());
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
