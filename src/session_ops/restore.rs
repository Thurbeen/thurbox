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
    /// `session.post_restore` hooks that failed. The restore stands regardless.
    pub hook_failures: Vec<String>,
}

/// Restore a deleted session: the row, then its worktrees, then its agent.
///
/// Ordered deliberately. The row comes first because it is the only step that
/// cannot partially succeed; the worktrees next because the agent's cwd is one
/// of them; the agent last, and its failure does not fail the restore — a
/// session whose window did not come up is still restored, and `restart` will
/// try again.
/// Whether a force-delete of these worktrees could have destroyed work.
///
/// The refusal below exists because `git worktree remove --force` takes the
/// directory and any uncommitted work in it. A session that only *opened*
/// worktrees it did not create had none of them removed, so there is nothing
/// to warn about and the refusal would scare the caller off a restore that
/// costs them nothing.
///
/// No worktrees at all still counts as lossy: that is every session predating
/// `created_by_thurbox`, and the conservative reading is the one that cannot
/// lose someone's work by being wrong.
fn force_delete_was_lossy(worktrees: &[SharedWorktree]) -> bool {
    worktrees.is_empty() || worktrees.iter().any(|w| w.created_by_thurbox)
}

/// Why this restore should stop and ask first, if it should — `None` when it
/// can simply run.
///
/// Two different promises can be broken, and saying the wrong one is worse
/// than saying nothing. A **lossy force-delete** destroyed uncommitted work, so
/// only committed work returns. A **borrowed worktree that is no longer on
/// disk** destroyed nothing — but the restore cannot deliver the session it
/// hands back: `restore_session` leaves the stored `cwd` alone and `respawn`
/// anchors on it, so the pane opens at a path that is not there. The skip in
/// [`recreate_worktrees`] keeps the *count* honest; only this keeps the restore
/// honest.
///
/// The disk check is not conditioned on `force_deleted`: a soft-deleted session
/// whose borrowed checkout the user removed afterwards lands in exactly the
/// same place. `--best-effort` (the interface's confirm) remains the way to say
/// yes to either.
///
/// It *is* conditioned on the backend being local, for the same reason the
/// `create` command only validates a worktree path when no host is named: a
/// remote session's checkout lives on its host, so stat'ing the path here
/// answers about the wrong filesystem and reads every borrowed remote worktree
/// as gone. The host's own `session restore` asks this question again against
/// the filesystem the path belongs to — [`restore_session_headless`] delegates
/// before it gets as far as recreating anything.
pub fn restore_refusal(
    name: &str,
    force_deleted: bool,
    backend_type: &str,
    worktrees: &[SharedWorktree],
) -> Option<String> {
    if force_deleted && force_delete_was_lossy(worktrees) {
        return Some(format!(
            "'{name}' was force-deleted; recovering it brings back committed work only \
             (uncommitted and untracked changes are gone)"
        ));
    }
    if crate::session::is_remote_backend(backend_type) {
        return None;
    }
    let gone = worktrees
        .iter()
        .find(|w| !w.created_by_thurbox && !w.worktree_path.is_dir())?;
    Some(format!(
        "'{name}' opened the worktree at {}, and it is no longer on disk; \
         restoring cannot bring back a directory thurbox never created",
        gone.worktree_path.display()
    ))
}

pub fn restore_session_headless(
    db: &Database,
    id: SessionId,
    best_effort: bool,
) -> Result<RestoreReport, String> {
    let deleted = db
        .get_deleted_session_by_id(id)
        .map_err(|e| format!("get deleted session: {e}"))?
        .ok_or_else(|| format!("deleted session not found: {id}"))?;

    // Recovery the caller would not want is a decision, not a discovery: they
    // have to have been told before it happens. v1's confirm modal and the
    // CLI's `--best-effort` are the two places that ask — but only when there
    // is something to warn about, which `force_deleted` alone no longer
    // answers.
    if !best_effort {
        if let Some(reason) = restore_refusal(
            &deleted.name,
            deleted.force_deleted,
            &deleted.backend_type,
            &deleted.worktrees,
        ) {
            return Err(reason);
        }
    }

    // A session on a shareable host is restored by the host's CLI — the
    // worktrees are recreated and the agent relaunched where they live — and
    // the local row is then mirrored from the host's. A remote host that cannot
    // be delegated to keeps the refusal: every helper below drives the local
    // machine, and restoring there would produce a local impostor of a remote
    // session — `restart` refuses for the same reason.
    let remote = super::resolve_host(&deleted.backend_type).flatten();
    let delegated = remote
        .as_ref()
        .and_then(|host| super::host_cli::delegated(host).map(|cli| (host.clone(), cli)));
    if crate::session::is_remote_backend(&deleted.backend_type) && delegated.is_none() {
        return Err(format!(
            "'{}' runs on remote backend '{}'; restoring it is local-only for now",
            deleted.name, deleted.backend_type
        ));
    }

    // The user's say, with both refusals above already made and the row still
    // deleted: a refusal here changes nothing.
    let primary = deleted.worktrees.first();
    let mut hook_ctx = crate::session::HookContext {
        session_id: Some(deleted.id),
        name: deleted.name.clone(),
        agent: deleted.agent.clone(),
        agent_session_id: deleted.agent_session_id.clone(),
        repo: primary
            .map(|w| w.repo_path.clone())
            .or_else(|| deleted.cwd.clone()),
        cwd: deleted.cwd.clone(),
        branch: primary.map(|w| w.branch.clone()),
        host: super::lifecycle_hooks::host_name(&deleted.backend_type),
        parent_session_id: deleted.parent_session_id,
        force_deleted: Some(deleted.force_deleted),
        worktrees: deleted
            .worktrees
            .iter()
            .map(super::lifecycle_hooks::worktree)
            .collect(),
        ..crate::session::HookContext::default()
    };
    super::fire_pre(crate::session::HookEvent::PreRestore, &hook_ctx)?;

    if let Some((host, cli)) = delegated {
        let id = deleted.id.to_string();
        let mut args = vec!["session", "restore", &id];
        if best_effort {
            args.push("--best-effort");
        }
        let answer = super::host_cli::run(&host, &cli, &args)?;
        db.restore_session(deleted.id)
            .map_err(|e| format!("restore session: {e}"))?;
        if let Err(e) = super::mirror::mirror_host(db, &host, &cli) {
            tracing::warn!("mirror of '{}' after restore failed: {e}", host.name);
        }
        let count = |key: &str| {
            answer
                .get(key)
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0) as usize
        };
        hook_ctx.backend_id = super::lifecycle_hooks::current_pane(db, deleted.id);
        let hook_failures = super::fire_post(crate::session::HookEvent::PostRestore, &hook_ctx);
        return Ok(RestoreReport {
            name: deleted.name,
            best_effort: deleted.force_deleted,
            worktrees_wanted: count("worktrees_wanted"),
            worktrees_recovered: count("worktrees_recovered"),
            respawn_error: answer
                .get("respawn_error")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string),
            hook_failures,
        });
    }

    db.restore_session(deleted.id)
        .map_err(|e| format!("restore session: {e}"))?;

    let wanted = deleted.worktrees.len();
    let recovered = recreate_worktrees(&deleted.worktrees);

    let respawn_error = respawn(db, deleted.id).err();

    // A restore whose agent did not come up is still a restore — the report
    // says so, and the hooks fire either way.
    hook_ctx.backend_id = super::lifecycle_hooks::current_pane(db, deleted.id);
    let hook_failures = super::fire_post(crate::session::HookEvent::PostRestore, &hook_ctx);

    Ok(RestoreReport {
        name: deleted.name,
        best_effort: deleted.force_deleted,
        worktrees_wanted: wanted,
        worktrees_recovered: recovered.len(),
        respawn_error,
        hook_failures,
    })
}

/// Re-attach each worktree whose branch still exists.
///
/// A worktree that cannot come back — its branch gone, or, for one thurbox only
/// borrowed, its directory gone — is skipped rather than failing the restore:
/// the others are still worth having, and the report says how many came back.
/// v1's `App::recreate_worktrees`, lifted here so both interfaces share it.
pub fn recreate_worktrees(worktrees: &[SharedWorktree]) -> Vec<WorktreeInfo> {
    let mut recovered = Vec::new();
    for worktree in worktrees {
        // Never thurbox's to re-create: the directory was the user's all along
        // and force-delete left it in place, so it is still checked out and
        // still registered with git. `add_existing_worktree` would only fail on
        // it and drop it from the restored session.
        if !worktree.created_by_thurbox {
            // The user's directory, so its continued existence is theirs to
            // decide: if they removed it, there is nothing to re-attach and
            // counting it as recovered would be a lie. Symmetric with the
            // `branch_exists` guard below — each arm checks the thing its own
            // restore depends on. This is the *report* only; what stops a
            // session being handed a cwd that is not there is
            // [`restore_refusal`], since nothing here is written back to the
            // row.
            if !worktree.worktree_path.is_dir() {
                tracing::warn!(
                    "not restoring {}: the worktree is gone",
                    worktree.worktree_path.display()
                );
                continue;
            }
            recovered.push(WorktreeInfo {
                repo_path: worktree.repo_path.clone(),
                worktree_path: worktree.worktree_path.clone(),
                branch: worktree.branch.clone(),
                created_by_thurbox: false,
            });
            continue;
        }
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
                created_by_thurbox: true,
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
    //
    // A soft-deleted session keeps its agent until the reaper lets it go; a
    // restore inside that window — or one asked for from a peer before this
    // machine's reaper ran — finds the window still alive, and a second launch
    // beside it would be two agents on one conversation.
    if let Ok(Some(pane)) = crate::agent::tmux::agent_window_pane(None, &session.name) {
        db.set_backend_id(session.id, &pane)
            .map_err(|e| format!("record the live pane: {e}"))?;
        return Ok(());
    }
    let hooks_enabled = super::hooks_enabled(db);
    let recipe = db.load_launch_recipe(session.id).unwrap_or_default();
    let env = db.load_launch_env(session.id).unwrap_or_default();
    let plan =
        super::restart::build_restart_plan(&session, None, hooks_enabled, recipe.as_ref(), &env)?;
    let pane = crate::agent::tmux::spawn_window(
        &plan.window_name,
        &plan.command,
        &plan.args,
        plan.cwd.as_deref(),
        &plan.env,
    )
    .map_err(|e| format!("re-spawn: {e}"))?;
    // The row still carries the pane the delete killed; the fresh one is what
    // every later read must target (empty on psmux — the name fallback stands).
    db.set_backend_id(session.id, &pane)
        .map_err(|e| format!("record the new pane: {e}"))?;
    Ok(())
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
            created_by_thurbox: true,
        }];
        assert!(recreate_worktrees(&worktrees).is_empty());
    }

    #[test]
    fn a_borrowed_worktree_whose_directory_is_gone_is_skipped_too() {
        // The user deleted their own checkout between the force-delete and the
        // restore. Handing the row back regardless gives the session a cwd that
        // is not there, which the thurbox arm already refuses to do via
        // `branch_exists`.
        let worktrees = vec![SharedWorktree {
            repo_path: std::path::PathBuf::from("/definitely/not/a/repo"),
            worktree_path: std::path::PathBuf::from("/definitely/not/a/worktree"),
            branch: "feat/borrowed".into(),
            created_by_thurbox: false,
        }];
        assert!(recreate_worktrees(&worktrees).is_empty());
    }

    #[test]
    fn a_borrowed_worktree_still_on_disk_comes_back_as_it_is() {
        let dir = tempfile::tempdir().expect("tempdir");
        let worktrees = vec![SharedWorktree {
            repo_path: dir.path().to_path_buf(),
            worktree_path: dir.path().to_path_buf(),
            branch: "feat/borrowed".into(),
            created_by_thurbox: false,
        }];
        let recovered = recreate_worktrees(&worktrees);
        assert_eq!(recovered.len(), 1);
        assert_eq!(recovered[0].worktree_path, dir.path());
        assert!(!recovered[0].created_by_thurbox);
    }

    /// The backend a session on this machine carries.
    const LOCAL: &str = "local-tmux";

    fn worktree(created_by_thurbox: bool) -> SharedWorktree {
        SharedWorktree {
            repo_path: std::path::PathBuf::from("/repo"),
            worktree_path: std::path::PathBuf::from("/repo/.worktrees/mine"),
            branch: "feat/x".into(),
            created_by_thurbox,
        }
    }

    #[test]
    fn a_force_delete_that_removed_nothing_is_not_lossy() {
        // The session only opened worktrees it did not create, so the teardown
        // skipped every one of them and the directories are still on disk with
        // their uncommitted work. Warning here talks the caller out of a
        // restore that costs them nothing.
        assert!(!force_delete_was_lossy(&[worktree(false)]));
        assert!(!force_delete_was_lossy(&[worktree(false), worktree(false)]));
    }

    #[test]
    fn one_worktree_thurbox_created_makes_the_whole_restore_lossy() {
        // `git worktree remove --force` ran on that one, so something was
        // destroyed even though its neighbours survived.
        assert!(force_delete_was_lossy(&[worktree(false), worktree(true)]));
        assert!(force_delete_was_lossy(&[worktree(true)]));
    }

    #[test]
    fn a_borrowed_worktree_missing_from_disk_is_refused_with_its_own_reason() {
        // The restore cannot deliver what it promises: the directory the
        // session would be anchored at is not there, and nothing downstream
        // notices — `restore_session` leaves `cwd` alone and `respawn` opens a
        // pane at it regardless. The message has to name that, not uncommitted
        // work that was never touched.
        let reason = restore_refusal("borrowed", false, LOCAL, &[worktree(false)])
            .expect("a missing borrowed worktree is a refusal");
        assert!(reason.contains("/repo/.worktrees/mine"), "{reason}");
        assert!(!reason.contains("uncommitted"), "{reason}");
    }

    #[test]
    fn a_borrowed_worktree_still_on_disk_is_not_refused() {
        let dir = tempfile::tempdir().expect("tempdir");
        let present = SharedWorktree {
            repo_path: dir.path().to_path_buf(),
            worktree_path: dir.path().to_path_buf(),
            branch: "feat/borrowed".into(),
            created_by_thurbox: false,
        };
        assert_eq!(restore_refusal("borrowed", true, LOCAL, &[present]), None);
    }

    #[test]
    fn a_remote_session_is_not_refused_over_a_path_on_the_other_machine() {
        // The borrowed worktree is on the host, so the path never existed
        // locally and `is_dir` here is answering about the wrong filesystem.
        // Refusing on it would make every remote session that opened a
        // worktree unrestorable without `--best-effort`, over a directory that
        // is in fact still there. The host's own `session restore` asks again,
        // against the filesystem the path belongs to.
        assert_eq!(
            restore_refusal("borrowed", false, "ssh:builder", &[worktree(false)]),
            None
        );
        // The lossy case is not a disk question, so it still refuses.
        let reason = restore_refusal("mine", true, "ssh:builder", &[worktree(true)])
            .expect("a lossy force-delete is a refusal wherever it ran");
        assert!(reason.contains("uncommitted"), "{reason}");
    }

    #[test]
    fn a_lossy_force_delete_keeps_the_uncommitted_work_message() {
        // Thurbox made this one, so `git worktree remove --force` took the
        // directory: the older refusal is the accurate one and wins.
        let reason = restore_refusal("mine", true, LOCAL, &[worktree(true)])
            .expect("a lossy force-delete is a refusal");
        assert!(reason.contains("uncommitted"), "{reason}");
    }

    #[test]
    fn a_session_with_no_worktrees_stays_lossy() {
        // Every row predating `created_by_thurbox` looks like this, and the
        // conservative reading is the one that cannot lose work by being wrong.
        assert!(force_delete_was_lossy(&[]));
    }
}
