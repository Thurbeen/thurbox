//! Bringing a deleted session back — the row, its worktrees, and its agent.
//!
//! Restoring is not one step, and treating it as one is what made v2's restore
//! useless: clearing `deleted_at` returns a row to the list, but its worktree
//! directory was removed with the delete and its tmux window died with it, so
//! what came back was a session that could never attach. v1 does all three
//! (`App::restore_deleted_session`); this is that, headless, so the interface and
//! the command line cannot disagree about what restoring means.
//!
//! What can and cannot be recovered is the whole subtlety. A soft delete leaves
//! the worktree on disk, so restoring is exact. A **force** delete removed the
//! directory — the branch survives, so committed work comes back and
//! uncommitted work does not. That is lossy, so it is refused unless the caller
//! says it knows.

use crate::session::{SessionId, WorktreeInfo};
use crate::storage::Database;
use crate::sync::SharedWorktree;

/// What a restore managed to bring back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestoreReport {
    pub name: String,
    /// True when the session had been force-deleted, so only committed work
    /// returned.
    pub best_effort: bool,
    /// Worktrees the session had, and how many came back — they differ when a
    /// branch was deleted after the session was.
    pub worktrees_wanted: usize,
    pub worktrees_recovered: usize,
    /// Set when the row and its worktrees returned but the agent did not.
    pub respawn_error: Option<String>,
}

/// Restore a deleted session: the row, then its worktrees, then its agent.
///
/// Ordered deliberately. The row comes first because it is the only step that
/// cannot partially succeed; the worktrees next because the agent's cwd is one
/// of them; the agent last, and its failure does not fail the restore — a
/// session whose window did not come up is still restored, and `restart` will
/// try again.
pub fn restore_session_headless(
    db: &Database,
    id: SessionId,
    best_effort: bool,
) -> Result<RestoreReport, String> {
    let deleted = db
        .get_deleted_session_by_id(id)
        .map_err(|e| format!("get deleted session: {e}"))?
        .ok_or_else(|| format!("deleted session not found: {id}"))?;

    // Lossy recovery is a decision, not a discovery: the caller has to have been
    // told before it happens. v1's confirm modal and the CLI's `--best-effort`
    // are the two places that ask.
    if deleted.force_deleted && !best_effort {
        return Err(format!(
            "'{}' was force-deleted; recovering it brings back committed work only \
             (uncommitted and untracked changes are gone)",
            deleted.name
        ));
    }

    // A remote session's worktrees and window live on its host, and every helper
    // below drives the local machine. Silently restoring it here would produce a
    // local impostor of a remote session — `restart` refuses for the same reason.
    if crate::session::is_remote_backend(&deleted.backend_type) {
        return Err(format!(
            "'{}' runs on remote backend '{}'; restoring it is local-only for now",
            deleted.name, deleted.backend_type
        ));
    }

    db.restore_session(deleted.id)
        .map_err(|e| format!("restore session: {e}"))?;

    let wanted = deleted.worktrees.len();
    let recovered = recreate_worktrees(&deleted.worktrees);

    let respawn_error = respawn(db, deleted.id).err();

    Ok(RestoreReport {
        name: deleted.name,
        best_effort: deleted.force_deleted,
        worktrees_wanted: wanted,
        worktrees_recovered: recovered.len(),
        respawn_error,
    })
}

/// Re-attach each worktree whose branch still exists.
///
/// A branch that is gone is skipped rather than failing the restore: the other
/// worktrees are still worth having, and the report says how many came back. v1's
/// `App::recreate_worktrees`, lifted here so both interfaces share it.
pub fn recreate_worktrees(worktrees: &[SharedWorktree]) -> Vec<WorktreeInfo> {
    let mut recovered = Vec::new();
    for worktree in worktrees {
        if !crate::git::branch_exists(&worktree.repo_path, &worktree.branch) {
            tracing::warn!(
                "not restoring {}: its branch is gone",
                worktree.worktree_path.display()
            );
            continue;
        }
        match crate::git::add_existing_worktree(&worktree.repo_path, &worktree.branch) {
            Ok(path) => recovered.push(WorktreeInfo {
                repo_path: worktree.repo_path.clone(),
                worktree_path: path,
                branch: worktree.branch.clone(),
            }),
            Err(e) => tracing::warn!("could not recreate worktree {}: {e}", worktree.branch),
        }
    }
    recovered
}

/// Launch the agent again, under the session's own identity.
///
/// The window is gone (the delete killed it), so this spawns rather than
/// restarts — but through the same plan a restart builds, so a restored session
/// resumes its conversation exactly as a restarted one does.
fn respawn(db: &Database, id: SessionId) -> Result<(), String> {
    let session = db
        .get_session_by_id(id)
        .map_err(|e| format!("load restored session: {e}"))?
        .ok_or_else(|| format!("restored session not found: {id}"))?;
    // Local by design: `restore_session_headless` refuses a remote session
    // above, since its worktrees cannot be recreated from here.
    let hooks_enabled = !db.builtin_hooks_opted_out().unwrap_or(false);
    let plan = super::restart::build_restart_plan(&session, None, hooks_enabled)?;
    crate::agent::tmux::spawn_window(
        &plan.window_name,
        &plan.command,
        &plan.args,
        plan.cwd.as_deref(),
        &plan.env,
    )
    .map_err(|e| format!("re-spawn: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restoring_something_that_was_never_deleted_says_so() {
        let db = Database::open_in_memory().expect("db");
        let error = restore_session_headless(&db, SessionId::default(), false).unwrap_err();
        assert!(error.contains("not found"), "{error}");
    }

    #[test]
    fn a_branch_that_is_gone_is_skipped_rather_than_failing_the_restore() {
        // Nothing exists at these paths, so every worktree is unrecoverable — and
        // that is a report, not an error: the row itself still comes back.
        let worktrees = vec![SharedWorktree {
            repo_path: std::path::PathBuf::from("/definitely/not/a/repo"),
            worktree_path: std::path::PathBuf::from("/definitely/not/a/worktree"),
            branch: "feat/gone".into(),
        }];
        assert!(recreate_worktrees(&worktrees).is_empty());
    }
}
