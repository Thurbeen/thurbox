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

/// Hold the deleted session's name for the length of the restore, refusing when
/// something else already answers to it or is in the middle of creating it.
fn refuse_a_taken_name<'a>(
    db: &'a Database,
    deleted: &crate::storage::DeletedSessionInfo,
) -> Result<super::names::HeldName<'a>, String> {
    let Some(hold) = super::names::hold(db, &deleted.name, &deleted.backend_type)? else {
        return Err(format!(
            "'{}' is being created right now on {}; restoring this one would race that \
             creation for the name. Try again once it has finished",
            deleted.name, deleted.backend_type
        ));
    };
    match super::names::window_namesakes(db, &deleted.name, &deleted.backend_type)?.first() {
        Some(live) => Err(format!(
            "'{}' is already the name of a live session on {} ('{}', {}); restoring this \
             one would leave two, and neither could be addressed by name. {}",
            deleted.name,
            deleted.backend_type,
            live.name,
            live.id,
            free_the_name_advice(db, &live.name, live.id)
        )),
        None => Ok(hold),
    }
}

/// How to free a name a live session holds, told so that following it ends
/// with the restore succeeding.
///
/// Renaming the live session is the non-destructive answer, and it is the whole
/// answer only when nothing recreates the name behind the user's back. A session
/// an **active extension declares** is exactly that: self-heal takes the name
/// again on the next heartbeat tick, so a user who renames and then restores
/// loses the race and is one renamed orphan worse off. For that one the
/// extension has to be deactivated first, and saying so is what makes the advice
/// terminate.
fn free_the_name_advice(db: &Database, live_name: &str, live_id: SessionId) -> String {
    match declaring_extension(db, live_name) {
        Some(ext) => format!(
            "'{live_name}' is a session the active extension '{ext}' declares, so self-heal \
             recreates it within a minute of any rename — `thurbox-cli extension deactivate \
             {ext}` first, then restore this one"
        ),
        None => format!(
            "`thurbox-cli session rename {live_id} <other-name>` frees the name without \
             destroying anything, and this one can then be restored"
        ),
    }
}

/// The active extension declaring a session called `name`, if one does.
///
/// Fully-qualified `agent` reference (no `use`) per the session_ops → agent
/// path-only architecture rule. Asked only on the refusal path, so a manifest
/// read costs a restore that was never going to happen.
fn declaring_extension(db: &Database, name: &str) -> Option<String> {
    db.get_active_extensions().ok()?.into_iter().find(|ext| {
        crate::agent::extension_config::load_manifest(ext)
            .is_some_and(|def| def.sessions.iter().any(|s| s.name == name))
    })
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

    // Not part of `restore_refusal`, and so not waived by `--best-effort`: that
    // flag says "I accept a lossy recovery", and this is not about loss.
    // Un-deleting a name something else now answers to is the other end of the
    // sequence that made a pair (issue #1192) — self-heal recreates the
    // extension's session once the undo window has closed, and a restore of the
    // original afterwards used to put both on the backend at once, where
    // neither can be addressed by name again.
    //
    // Local rows only, and below the delegation above rather than before it: a
    // remote session's names are arbitrated where its rows are authored. Asked
    // here it would be asked of a mirror, which is a snapshot — a namesake
    // deleted on the host but not yet mirrored would refuse a restore the host
    // itself would allow. The delegated `session restore` asks this same
    // question there, against the rows that decide it. `restore_refusal`
    // conditions its worktree check on the same thing for the same reason.
    //
    // `_hold` outlives the restore rather than the check: a creation that has
    // claimed the name has not written its row yet, so the lookup below cannot
    // see it, and holding the name is what stops one starting underneath.
    let _hold = if crate::session::is_remote_backend(&deleted.backend_type) {
        None
    } else {
        Some(refuse_a_taken_name(db, &deleted)?)
    };

    // The user's say, with every refusal above already made and the row still
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
    let mux = crate::agent::tmux::LocalMuxContext::for_backend(&session.backend_type)?;
    // Local by design: `restore_session_headless` refuses a remote session
    // above, since its worktrees cannot be recreated from here.
    //
    // A soft-deleted session keeps its agent until the reaper lets it go; a
    // restore inside that window — or one asked for from a peer before this
    // machine's reaper ran — finds the window still alive, and a second launch
    // beside it would be two agents on one conversation.
    let stamp = session.id.to_string();
    // Strictly its own window: one stamped for a live namesake is not this
    // row's to adopt, and recording it would put two rows on one pane — the
    // next kill-by-id then destroys the other session's agent.
    if let Ok(located) = crate::agent::tmux::agent_window(&mux, None, &stamp, &session.name) {
        if let Some(pane) = located.pane() {
            crate::agent::tmux::stamp_local_window(
                &mux,
                &pane,
                &stamp,
                crate::agent::tmux::WindowRole::Agent,
            );
            db.set_backend_id(session.id, &pane)
                .map_err(|e| format!("record the live pane: {e}"))?;
            return Ok(());
        }
    }
    let hooks_enabled = super::hooks_enabled(db);
    // Strict: a `--command` session whose recipe read as absent is planned as a
    // registry-agent session, and the default coding agent is launched in place
    // of the command that was recorded. See `restart_session_headless_with`.
    let recipe = db
        .load_launch_recipe(session.id)
        .map_err(|e| format!("read the launch recipe: {e}"))?;
    let env = db
        .load_launch_env(session.id)
        .map_err(|e| format!("read the launch env: {e}"))?;
    let plan =
        super::restart::build_restart_plan(&session, None, hooks_enabled, recipe.as_ref(), &env)?;
    let pane = crate::agent::tmux::spawn_window(
        &mux,
        &stamp,
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

    /// A creation that has claimed the name has not written its row yet, so a
    /// guard that only looked for a live namesake would un-delete straight into
    /// it and the spawn would land beside the restored row — the pair, from the
    /// one side the lookup cannot see.
    #[test]
    fn a_restore_waits_for_a_creation_that_is_holding_the_name() {
        let db = crate::storage::Database::open_in_memory().expect("db");
        let id = SessionId::default();
        db.upsert_session(&crate::sync::SharedSession {
            id,
            name: "build".into(),
            agent: "shell".into(),
            backend_id: String::new(),
            backend_type: LOCAL.into(),
            agent_session_id: None,
            cwd: None,
            additional_dirs: Vec::new(),
            worktrees: Vec::new(),
            shell_backend_id: None,
            parent_session_id: None,
            display_order: None,
            tombstone: false,
            tombstone_at: None,
        })
        .expect("upsert");
        db.soft_delete_session(id).expect("soft delete");

        let held = super::super::names::hold(&db, "build", LOCAL)
            .expect("hold")
            .expect("nothing holds it yet");
        let err = restore_session_headless(&db, id, true)
            .expect_err("a creation holds the name; the restore must say so");
        assert!(err.contains("being created right now"), "{err}");

        drop(held);
        // And the hold ending is all it was waiting on: no namesake is live.
        assert!(
            super::super::names::hold(&db, "build", LOCAL)
                .expect("hold")
                .is_some(),
            "the refused restore gave the name back"
        );
    }

    #[test]
    fn a_session_with_no_worktrees_stays_lossy() {
        // Every row predating `created_by_thurbox` looks like this, and the
        // conservative reading is the one that cannot lose work by being wrong.
        assert!(force_delete_was_lossy(&[]));
    }
}
