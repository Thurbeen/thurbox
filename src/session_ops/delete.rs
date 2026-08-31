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
    /// Set when the session lived on a remote host (SSH/WSL) and its window
    /// could not be torn down there: the host is unreachable, has no
    /// `hosts.toml` entry, or the session carries no pane id. Best-effort — an
    /// unreachable host is expected (that's often *why* someone force-deletes),
    /// so this is recorded rather than aborting the delete.
    pub remote_teardown_error: Option<String>,
    /// `session.post_delete` hooks that failed. The delete stands regardless.
    pub hook_failures: Vec<String>,
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

    // The user's say, before anything is torn down or marked: a refusal here
    // leaves the row exactly as it was.
    let mut hook_ctx = super::lifecycle_hooks::context_for(&session);
    hook_ctx.force = Some(force);
    super::fire_pre(crate::session::HookEvent::PreDelete, &hook_ctx)?;

    let mut report = ForceDeleteReport::default();

    // A session on a shareable host is the host's to delete: its CLI kills the
    // window and removes the worktrees where they are, and marks its own row;
    // the local row is marked from its answer. Soft or forced as asked.
    if let Some(host) = super::resolve_host(&session.backend_type).flatten() {
        if let Some(cli) = super::host_cli::delegated(&host) {
            let id = session_id.to_string();
            let mut args = vec!["session", "delete", &id];
            if force {
                args.push("--force");
            }
            let answer = super::host_cli::run(&host, &cli, &args)?;
            report.killed_window = answer
                .get("killed_window")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false);
            report.removed_worktrees = string_list(&answer, "removed_worktrees");
            report.worktree_errors = string_list(&answer, "worktree_errors");
            report.remote_teardown_error = answer
                .get("remote_teardown_error")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string);
            report.disabled_automations = db
                .disable_send_automations_for_session(session_id)
                .map_err(|e| format!("disable_send_automations_for_session: {e}"))?;
            db.soft_delete_session(session_id)
                .map_err(|e| format!("soft_delete_session: {e}"))?;
            if force {
                db.mark_session_force_deleted(session_id)
                    .map_err(|e| format!("mark_session_force_deleted: {e}"))?;
                db.clear_session_meta(session_id)
                    .map_err(|e| format!("clear_session_meta: {e}"))?;
            }
            report.hook_failures =
                super::fire_post(crate::session::HookEvent::PostDelete, &hook_ctx);
            return Ok(report);
        }
    }

    if force {
        teardown_runtime_resources(&session, &mut report);
        report.disabled_automations = db
            .disable_send_automations_for_session(session_id)
            .map_err(|e| format!("disable_send_automations_for_session: {e}"))?;
    }

    db.soft_delete_session(session_id)
        .map_err(|e| format!("soft_delete_session: {e}"))?;

    // Record that this delete tore down the worktrees/tmux so the restore list
    // can tag + block it (a force-delete is not restorable).
    if force {
        db.mark_session_force_deleted(session_id)
            .map_err(|e| format!("mark_session_force_deleted: {e}"))?;
        db.clear_session_meta(session_id)
            .map_err(|e| format!("clear_session_meta: {e}"))?;
    }

    report.hook_failures = super::fire_post(crate::session::HookEvent::PostDelete, &hook_ctx);

    Ok(report)
}

fn string_list(answer: &serde_json::Value, key: &str) -> Vec<String> {
    answer
        .get(key)
        .and_then(serde_json::Value::as_array)
        .map(|list| {
            list.iter()
                .filter_map(serde_json::Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// Tear down a session's slow runtime resources: kill the tmux window, remove
/// worktrees + the symlink workspace. Touches no SQLite — safe to call from a
/// background thread after the row has been soft-deleted on the UI thread, so
/// the TUI's hard-delete confirmation can close without blocking on a remote
/// `kill-window` or a `git worktree remove`. Best-effort: failures are logged
/// into `report` (or `tracing::warn`), never abort.
///
/// **Backend-aware.** The window kill and each worktree removal run on the
/// server the session actually lives on, resolved from `session.backend_type`:
/// a local backend uses the local tmux socket + local `git`; an `ssh:`/`wsl:`
/// backend kills the pane and removes the worktrees over that host's launcher.
/// The symlink workspace is always local (a spawn-time process-cwd detail under
/// the local data dir), so it is torn down regardless of backend.
pub fn teardown_runtime_resources(
    session: &crate::sync::SharedSession,
    report: &mut ForceDeleteReport,
) {
    if crate::session::is_remote_backend(&session.backend_type) {
        // Off-local session: kill the pane + remove worktrees on the host. An
        // unresolvable/unreachable host is expected — record it, never abort.
        let registry = crate::agent::host_config::load_all();
        match registry.get_by_backend(&session.backend_type) {
            Some(host) => {
                kill_remote_window(host, session, report);
                for wt in &session.worktrees {
                    remove_worktree_into(Some(host), wt, report);
                }
            }
            None => {
                let msg = format!(
                    "remote host '{}' not found in hosts.toml; \
                     left its window + worktrees in place",
                    session.backend_type
                );
                tracing::warn!("{msg}");
                report.remote_teardown_error = Some(msg);
            }
        }
    } else {
        kill_local_window(session, report);
        for wt in &session.worktrees {
            remove_worktree_into(None, wt, report);
        }
    }

    // Tear down the multi-repo symlink workspace (if any). Only the symlinks
    // are removed — the underlying repos are untouched. Always local: the
    // workspace lives under the local data dir even for a remote session.
    if let Some(asid) = &session.agent_session_id {
        if let Err(e) = crate::workspace::remove_workspace(asid) {
            tracing::warn!("remove_workspace({asid}) failed: {e}");
        }
    }
}

/// Release what a *soft*-deleted session is still holding: its agent, its
/// metrics file and its symlink workspace.
///
/// Deleting softly is meant to be undoable, so the row is kept and the worktrees
/// stay on disk — but the agent process is not part of what an undo restores, and
/// leaving it running means a deleted session keeps working, keeps writing, and
/// keeps its tmux window forever. v1 killed it once the undo window closed;
/// this is that, callable without a TUI. The TUI side is now
/// `kernel::reaper::Reaper`, which watches the undo windows close.
///
/// Worktrees are deliberately untouched: they are what makes the undo lossless.
///
/// Returns whether anything was reaped — `false` when the row came back (the
/// user undid it) or was force-deleted (already torn down), both of which are
/// ordinary races rather than failures.
pub fn reap_soft_deleted(db: &Database, id: SessionId) -> Result<bool, String> {
    let Some(row) = db
        .get_deleted_session_by_id(id)
        .map_err(|e| format!("get deleted session: {e}"))?
    else {
        // Restored between the delete and the reap: the session is alive again,
        // and killing its agent now would be the bug this guard exists to avoid.
        return Ok(false);
    };
    if row.force_deleted {
        return Ok(false);
    }

    // A remote session's window lives on its host, and a best-effort reap is
    // not the place to be resolving hosts and dialing ssh. Left running and
    // reported rather than half-killed.
    if crate::session::is_remote_backend(&row.backend_type) {
        tracing::warn!(
            "'{}' was soft-deleted on {}; its remote window is left running",
            row.name,
            row.backend_type
        );
        return Ok(false);
    }

    if let Err(e) = crate::agent::tmux::kill_window(&row.name, &row.backend_id) {
        // The window may already be gone — the agent exited, or a previous reap
        // got there first. Not worth failing a cleanup over.
        tracing::debug!("kill_window({}) during reap: {e}", row.name);
    }

    // Derived per-session artifacts, both rebuilt on restore.
    if let Some(asid) = &row.agent_session_id {
        if let Some(dir) = crate::paths::metrics_directory() {
            let _ = std::fs::remove_file(dir.join(format!("{asid}.json")));
        }
        if let Err(e) = crate::workspace::remove_workspace(asid) {
            tracing::warn!("remove_workspace({asid}) during reap: {e}");
        }
    }
    Ok(true)
}

/// Kill the session's window on the local tmux server, reaping the pane's child
/// process on Windows (where a live process's cwd blocks the later rmdir).
fn kill_local_window(session: &crate::sync::SharedSession, report: &mut ForceDeleteReport) {
    // Capture the pane's OS pid *before* the kill so we can reap the pane's child
    // process below. Windows refuses to remove a directory that is a live
    // process's cwd, and a session's agent runs with cwd = its worktree /
    // extension home; Unix has no such restriction, so this is Windows-only.
    #[cfg(windows)]
    let pane_pid = crate::agent::tmux::window_pane_pid(&session.name, &session.backend_id)
        .ok()
        .flatten();

    match crate::agent::tmux::kill_window(&session.name, &session.backend_id) {
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
}

/// Kill the session's pane on a remote host by its persisted pane id (`%N`) —
/// the addressable unit remotely (there's no cheap "window by thurbox name"
/// lookup over the wire). Best-effort: a blank pane id or an unreachable host
/// is recorded in `report.remote_teardown_error`, never aborts.
fn kill_remote_window(
    host: &crate::session::HostDef,
    session: &crate::sync::SharedSession,
    report: &mut ForceDeleteReport,
) {
    let pane = session.backend_id.trim();
    if pane.is_empty() {
        let msg = format!(
            "session '{}' on {} has no pane id; could not kill its remote window",
            session.name,
            host.backend_name()
        );
        tracing::warn!("{msg}");
        report.remote_teardown_error = Some(msg);
        return;
    }
    match crate::agent::tmux::kill_pane_remote(host, pane) {
        Ok(()) => report.killed_window = true,
        Err(e) => {
            let msg = format!(
                "kill_pane_remote({}, {pane}) failed: {e}",
                host.backend_name()
            );
            tracing::warn!("{msg}");
            report.remote_teardown_error = Some(msg);
        }
    }
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

/// Best-effort worktree removal on `host` (local when `None`), recording
/// success/failure into `report`. Removes the worktree *directory* only — the
/// git branch is deliberately left behind (local and remote alike), matching
/// force-delete's contract.
fn remove_worktree_into(
    host: Option<&crate::session::HostDef>,
    wt: &crate::sync::SharedWorktree,
    report: &mut ForceDeleteReport,
) {
    match crate::git::remove_worktree_on(host, &wt.repo_path, &wt.worktree_path) {
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
        insert_session_on(db, name, "local-tmux", "")
    }

    /// Insert a session with an explicit backend type + pane id, so the remote
    /// teardown paths can be exercised.
    fn insert_session_on(
        db: &Database,
        name: &str,
        backend_type: &str,
        backend_id: &str,
    ) -> SessionId {
        let id = SessionId::default();
        let shared = SharedSession {
            id,
            name: name.into(),
            agent: "dev".into(),
            backend_id: backend_id.into(),
            backend_type: backend_type.into(),
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
    fn force_delete_clears_session_meta_but_soft_delete_does_not() {
        // Regression: `clear_session_meta` is documented as being called on
        // force-delete (the row is unrestorable, so its metadata is dead
        // weight) but nothing wired the call in — a force-deleted session's
        // meta rows outlived it forever.
        let db = Database::open_in_memory().unwrap();

        let soft = insert_session(&db, "soft-meta");
        db.set_session_meta(soft, "fm.lease", "abc").unwrap();
        delete_session_headless(&db, soft, false).unwrap();
        assert_eq!(
            db.get_session_meta(soft, "fm.lease").unwrap(),
            Some("abc".to_string()),
            "a soft delete is restorable, so its metadata must survive it"
        );

        let hard = insert_session(&db, "hard-meta");
        db.set_session_meta(hard, "fm.lease", "xyz").unwrap();
        delete_session_headless(&db, hard, true).unwrap();
        assert!(
            db.get_session_meta(hard, "fm.lease").unwrap().is_none(),
            "a force delete is unrestorable, so its metadata must not outlive it"
        );
    }

    #[test]
    fn force_delete_marks_force_deleted_but_soft_does_not() {
        let db = Database::open_in_memory().unwrap();

        let soft = insert_session(&db, "soft");
        delete_session_headless(&db, soft, false).unwrap();
        assert!(
            !db.get_deleted_session_by_id(soft)
                .unwrap()
                .unwrap()
                .force_deleted,
            "a soft delete stays restorable"
        );

        let hard = insert_session(&db, "hard");
        delete_session_headless(&db, hard, true).unwrap();
        assert!(
            db.get_deleted_session_by_id(hard)
                .unwrap()
                .unwrap()
                .force_deleted,
            "a force delete is flagged as not restorable"
        );
    }

    // The resolved-remote-host kill/worktree path (a configured, reachable
    // host) is not unit-tested here: it needs a live SSH/WSL host and
    // `kill_pane_remote` would issue a real connection. The routing is thin —
    // `remove_worktree_on` / `kill_pane_remote` are exercised where they live —
    // so these tests cover the two host-resolution failure modes instead.
    // (cfg(test) sandboxes the config dir, so `load_all` sees an empty
    // `hosts.toml` and never touches the real network.)

    #[test]
    fn force_delete_remote_session_with_no_configured_host_records_error() {
        let db = Database::open_in_memory().unwrap();
        let id = insert_session_on(&db, "remote", "ssh:devbox", "%3");

        let report = delete_session_headless(&db, id, true).unwrap();

        // No matching host in (the empty test) hosts.toml → recorded, not killed.
        assert!(!report.killed_window);
        let err = report
            .remote_teardown_error
            .expect("remote teardown error recorded");
        assert!(err.contains("ssh:devbox"), "got {err}");

        // The row is still soft- + force-deleted (best-effort teardown).
        assert!(db.get_session_by_id(id).unwrap().is_none());
        assert!(
            db.get_deleted_session_by_id(id)
                .unwrap()
                .unwrap()
                .force_deleted
        );
    }

    #[test]
    fn force_delete_local_session_records_no_remote_error() {
        let db = Database::open_in_memory().unwrap();
        let id = insert_session(&db, "local");

        let report = delete_session_headless(&db, id, true).unwrap();
        assert!(report.remote_teardown_error.is_none());
    }

    #[test]
    fn remote_teardown_with_unresolved_host_leaves_worktrees_untouched() {
        // A remote session whose host isn't configured: its worktree dirs live
        // on that (now unreachable) host, so they must be left alone — NOT
        // attempted against the local `git`, which would either error out or,
        // worse, act on a same-path local directory. Exercises the routing
        // directly (no DB round-trip needed for the worktree list).
        let session = SharedSession {
            id: SessionId::default(),
            name: "remote".into(),
            agent: "dev".into(),
            backend_id: "%3".into(),
            backend_type: "wsl:Ubuntu".into(),
            // `None` so no local symlink-workspace cleanup is attempted either.
            agent_session_id: None,
            cwd: None,
            additional_dirs: Vec::new(),
            worktrees: vec![crate::sync::SharedWorktree {
                repo_path: "/nonexistent/repo".into(),
                worktree_path: "/nonexistent/repo/wt".into(),
                branch: "feat/x".into(),
            }],
            shell_backend_id: None,
            parent_session_id: None,
            display_order: None,
            tombstone: false,
            tombstone_at: None,
        };

        let mut report = ForceDeleteReport::default();
        teardown_runtime_resources(&session, &mut report);

        assert!(
            report.remote_teardown_error.is_some(),
            "unreachable host recorded"
        );
        assert!(
            report.removed_worktrees.is_empty() && report.worktree_errors.is_empty(),
            "no local git worktree removal attempted for a remote session"
        );
        assert!(!report.killed_window);
    }

    #[test]
    fn missing_session_errors() {
        let db = Database::open_in_memory().unwrap();
        let err = delete_session_headless(&db, SessionId::default(), false).unwrap_err();
        assert!(err.contains("Session not found"), "got {err}");
    }
}
