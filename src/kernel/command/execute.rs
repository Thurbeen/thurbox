//! The effects: what each accepted command actually does, on a worker thread
//! with its own database connection.

use std::sync::mpsc::Sender;

use super::bus::Progress;
use super::{BookmarkEdit, Command, ExtraMember};
use crate::kernel::snapshot;
use crate::session::SessionId;
use crate::storage::Database;

/// Run one command on a worker, never the UI thread.
///
/// Only the outcome travels back. A creation and a fork each mint a session id,
/// and neither reports it: a session that finished spawning is a row in the
/// list, not a reason to move the user's selection onto it.
pub(super) fn execute(
    command: &Command,
    backends: &crate::backend::BackendRegistry,
    id: u64,
    progress: &Sender<Progress>,
) -> Result<(), String> {
    // Handled by the loop before dispatch, because they mutate in-process state
    // the worker cannot reach. Reaching here means a caller bypassed that.
    if command.applied_on_ui_thread() {
        return Err("applied on the UI thread, not dispatched".to_string());
    }
    if let Command::Guarded {
        inner,
        session,
        backend_id,
        cwd,
        member_dirs,
    } = command
    {
        let session_id: SessionId = session.parse().map_err(|_| "invalid confirmation target")?;
        let path = crate::paths::database_file().ok_or("could not resolve the database path")?;
        let db = Database::open_existing(&path).map_err(|e| format!("open database: {e}"))?;
        let row = db
            .get_session_by_id(session_id)
            .map_err(|e| e.to_string())?
            .ok_or("confirmation target changed")?;
        let actual_backend = (!row.backend_id.is_empty()).then_some(row.backend_id.as_str());
        let mut actual_members: Vec<std::path::PathBuf> = if row.worktrees.is_empty() {
            row.cwd.iter().cloned().collect()
        } else {
            row.worktrees
                .iter()
                .map(|wt| wt.worktree_path.clone())
                .collect()
        };
        for dir in &row.additional_dirs {
            if !row.worktrees.iter().any(|wt| wt.worktree_path == *dir) {
                actual_members.push(dir.clone());
            }
        }
        if actual_backend != backend_id.as_deref()
            || row.cwd != *cwd
            || actual_members != *member_dirs
        {
            return Err("confirmation target changed".into());
        }
        return execute(inner, backends, id, progress);
    }

    // Creation names a repository rather than a session, so it runs before the
    // id is parsed — there is nothing to parse yet.
    if let Command::Create {
        name,
        repo,
        branch,
        base,
        worktree_path,
        agent,
        host,
        multiplexer,
        extras,
    } = command
    {
        return create(
            backends,
            name,
            repo,
            branch,
            base,
            worktree_path.as_deref(),
            agent,
            host,
            multiplexer,
            extras,
            id,
            progress,
        );
    }

    // Repository memory names a path, not a session, so it too runs before the
    // id parse.
    if let Command::Bookmark { host, path, edit } = command {
        return bookmark(host, path, edit);
    }

    // The settings file names nothing at all.
    if let Command::Configure { settings } = command {
        return crate::agent::settings_config::save_settings(settings)
            .map_err(|e| format!("write settings.toml: {e}"));
    }

    // Asked of the database rather than of a session: the sweep finds every row
    // whose undo window has closed, and every force delete still owing a
    // teardown on a host that was unreachable when it was taken.
    if matches!(command, Command::Reap) {
        let path = crate::paths::database_file().ok_or("could not resolve the database path")?;
        let db = Database::open_existing(&path).map_err(|e| format!("open database: {e}"))?;
        crate::session_ops::reconcile::tick(&db, backends);
        return Ok(());
    }

    // Keyed by nothing at all: an explicit order names every session at once.
    if let Command::Order { list } = command {
        let path = crate::paths::database_file().ok_or("could not resolve the database path")?;
        let db = Database::open_existing(&path).map_err(|e| format!("open database: {e}"))?;
        return order(&db, list);
    }

    // Tasks and automations are keyed by number, not by session id.
    if matches!(
        command,
        Command::Task { .. }
            | Command::DispatchTask { .. }
            | Command::Automation { .. }
            | Command::AutomationSave { .. }
    ) {
        let path = crate::paths::database_file().ok_or("could not resolve the database path")?;
        let db = Database::open_existing(&path).map_err(|e| format!("open database: {e}"))?;
        return match command {
            Command::Task {
                id,
                title,
                status,
                delete,
            } => task(&db, *id, title, status, *delete),
            Command::DispatchTask { task, session } => {
                dispatch_task(&db, backends, *task, session.as_deref())
            }
            Command::Automation {
                id,
                enabled,
                run_now,
                delete,
            } => automation(&db, *id, *enabled, *run_now, *delete),
            Command::AutomationSave { id, draft, .. } => {
                save_automation(&db, *id, draft, crate::sync::current_time_millis())
            }
            _ => unreachable!(),
        };
    }

    let id: SessionId = command
        .session()
        .parse()
        .map_err(|_| format!("not a session id: {}", command.session()))?;

    // Its own connection: sharing the UI thread's would mean locking against
    // the very reads this is supposed to keep instant. `open_existing` like
    // the terminal and repos workers — the schema is already there, and
    // `open` would re-run it (a `journal_mode = WAL` pragma that takes the
    // write lock, plus two prune DELETEs) on every command dispatched.
    let path = crate::paths::database_file().ok_or("could not resolve the database path")?;
    let db = Database::open_existing(&path).map_err(|e| format!("open database: {e}"))?;

    // A fork mints a session too; like a creation, the new row simply appears
    // in the list rather than pulling the selection onto itself.
    if let Command::Fork { name, .. } = command {
        return fork(&db, backends, id, name);
    }

    match command {
        Command::Delete { force, .. } => {
            crate::session_ops::delete_session_headless(&db, backends, id, *force)
                .map(|_| ())
                .map_err(|e| e.to_string())
        }
        // Restoring is the row, its worktrees and its agent — clearing the flag
        // alone gives back a session that can never attach (parity-gap #11).
        Command::Restore { best_effort, .. } => {
            crate::session_ops::restore_session_headless(&db, backends, id, *best_effort)
                .map(|report| {
                    if let Some(error) = report.respawn_error {
                        // Restored, but without its agent: worth saying, not worth
                        // undoing — `restart` will try again.
                        tracing::warn!("restored {} but could not launch it: {error}", report.name);
                    }
                })
                .map_err(|e| e.to_string())
        }
        // A failed post-restart hook is already in the log; the restart stands.
        // A relaunch held back for a crash loop does not, and fails with why.
        // Fresh is refused mid-turn: the interface never blocks a worker
        // waiting for a turn to end, and the refusal says why.
        Command::Restart { fresh: true, .. } => crate::session_ops::restart::restart_fresh(
            &db,
            backends,
            id,
            crate::session_ops::restart::WhenBusy::Refuse,
        )
        .and_then(crate::session_ops::restart::FreshReport::into_outcome)
        .map_err(|e| e.to_string()),
        Command::Restart { if_missing, .. } => {
            crate::session_ops::restart::restart_session_headless_with(
                &db,
                backends,
                id,
                *if_missing,
            )
            .and_then(crate::session_ops::restart::RestartReport::into_outcome)
            .map_err(|e| e.to_string())
        }
        Command::Send { text, .. } => {
            let session = db
                .get_session_by_id(id)
                .map_err(|e| format!("get session: {e}"))?
                .ok_or_else(|| format!("session not found: {id}"))?;
            crate::session_ops::send::prompt(&db, backends, &session, text, None)
                .map_err(|e| e.to_string())
        }
        Command::Reorder { delta, .. } => reorder(&db, id, *delta),
        // Handled above, before the session id is parsed.
        Command::Order { .. } => Ok(()),
        Command::Fork { .. } => unreachable!("handled above, where it mints its session"),

        Command::Sync { .. } => sync(&db, id),
        Command::Rename { name, .. } => {
            crate::session_ops::rename::rename_session_headless(&db, backends, id, name)
                .map(|_| ())
                .map_err(|e| e.to_string())
        }
        // Unreachable: guarded above, and kept exhaustive so adding a command
        // is a compile error here rather than a silent no-op.
        // Handled above, before the session id is parsed.
        Command::Create { .. }
        | Command::Guarded { .. }
        | Command::Bookmark { .. }
        | Command::Configure { .. }
        | Command::Task { .. }
        | Command::DispatchTask { .. }
        | Command::Reap
        | Command::Automation { .. }
        | Command::AutomationSave { .. } => unreachable!("handled before the id parse"),
        Command::Theme { .. }
        | Command::Setting { .. }
        | Command::Copy { .. }
        | Command::Diff { .. }
        | Command::OpenLink { .. }
        | Command::Shell { .. }
        | Command::Program { .. }
        | Command::Editor { .. }
        | Command::Focus { .. }
        | Command::Emit { .. }
        | Command::Action { .. }
        | Command::ActionTarget { .. }
        | Command::Message { .. }
        | Command::Plugin { .. } => unreachable!("applied on the UI thread"),
    }
}

/// Create a session.
///
/// The verb is [`crate::session_ops::create::create_or_adopt`], the one
/// `thurbox-cli session create` calls, so a creation is checked, named and
/// spawned the same way whether it came from a plugin or a script (#1468).
/// This only reads the command into a request.
#[allow(clippy::too_many_arguments)]
fn create(
    backends: &crate::backend::BackendRegistry,
    name: &str,
    repo: &str,
    branch: &Option<String>,
    base: &Option<String>,
    worktree_path: Option<&str>,
    agent: &Option<String>,
    host: &Option<String>,
    multiplexer: &Option<String>,
    extras: &[ExtraMember],
    id: u64,
    progress: &Sender<Progress>,
) -> Result<(), String> {
    let path = crate::paths::database_file().ok_or("could not resolve the database path")?;
    let db = Database::open_existing(&path).map_err(|e| format!("open database: {e}"))?;

    let request = crate::session_ops::spawn::SpawnRequest {
        name: name.to_string(),
        repo_path: crate::paths::expand_tilde(repo),
        worktree_branch: branch.clone(),
        base_branch: base.clone(),
        existing_worktree: worktree_path.map(crate::paths::expand_tilde),
        agent: agent.clone(),
        host: host.clone(),
        multiplexer: multiplexer.clone(),
        // Each extra either takes its own worktree on the shared branch — off
        // its own base, which here is the session's — or is attached as it is.
        // Two or more members is what makes the agent launch in a symlink
        // workspace, and that is `spawn`'s decision, not this one's.
        extra_repos: extras
            .iter()
            .map(|extra| crate::session::automation::ExtraRepo {
                repo_path: crate::paths::expand_tilde(&extra.path),
                worktree: extra.worktree,
                base_branch: None,
            })
            .collect(),
        ..Default::default()
    };
    // Report each stage, so a slow creation says *which* part is slow: a
    // stalled fetch and a stalled ssh connect look identical otherwise.
    let progress = progress.clone();
    let report = move |phase: crate::session_ops::spawn::SpawnPhase| {
        let _ = progress.send(Progress {
            id,
            phase: phase.as_str().to_string(),
        });
    };
    crate::session_ops::create::create_or_adopt(&db, backends, request, Some(&report))
        .map(|_| ())
        .map_err(|e| e.to_string())
}

/// Remember, forget or import a repository path.
///
/// Runs here rather than in the flow because every branch of it touches the
/// world: expanding a `~` on an ssh host is a round trip, so is establishing
/// whether a path is a repository, and scanning a folder is another. The
/// refusal of a missing path is the point — catching a typo now beats failing
/// minutes later inside `git worktree add`.
fn bookmark(host: &str, path: &str, edit: &BookmarkEdit) -> Result<(), String> {
    let db_path = crate::paths::database_file().ok_or("could not resolve the database path")?;
    let db = Database::open_existing(&db_path).map_err(|e| format!("open database: {e}"))?;

    // Removal names a path that is already remembered — an absolute one, since
    // the rows the flow offers come from the database — so it needs neither the
    // filesystem nor the host. Done BEFORE the host is resolved, which is what
    // lets a bookmark be forgotten after its host has been taken out of
    // `hosts.toml`.
    if *edit == BookmarkEdit::Remove {
        return bookmark_remove(&db, host, path);
    }

    // `""` is local; the flow's key is the same string `repo_bookmarks.host`
    // stores, so nothing is translated here.
    let remote = match host.is_empty() {
        true => None,
        false => {
            let (registry, _warnings) = crate::agent::host_config::cached_registry();
            Some(
                registry
                    .resolve(host)
                    .cloned()
                    .ok_or_else(|| format!("no such host: {host}"))?,
            )
        }
    };

    let expanded = match remote.as_ref() {
        Some(host) => std::path::PathBuf::from(
            crate::git::expand_remote_tilde(host, path).map_err(|e| format!("{e:#}"))?,
        ),
        None => crate::paths::expand_tilde(path),
    };

    let fill = match edit {
        BookmarkEdit::Parent => return bookmark_parent(&db, host, remote.as_ref(), &expanded),
        BookmarkEdit::Create => crate::git::NewRepo::Empty,
        BookmarkEdit::Init => crate::git::NewRepo::Init,
        BookmarkEdit::Clone { url } => crate::git::NewRepo::Clone { url: url.clone() },
        BookmarkEdit::Add | BookmarkEdit::Remove => {
            return bookmark_add(&db, host, remote.as_ref(), &expanded)
        }
    };
    crate::git::create_repo_dir(remote.as_ref(), &expanded, &fill).map_err(|e| format!("{e:#}"))?;
    // Remembered through the same door as a typed path, so the git-ness is
    // observed rather than assumed from which verb was asked for.
    bookmark_add(&db, host, remote.as_ref(), &expanded)
}

/// Forget a remembered path.
///
/// Removal names a path that is already remembered — an absolute one, since the
/// rows the flow offers come from the database — so it needs neither the
/// filesystem nor the host. Done BEFORE the host is resolved, which is what lets
/// a bookmark be forgotten after its host has been taken out of `hosts.toml`.
fn bookmark_remove(db: &Database, host: &str, path: &str) -> Result<(), String> {
    let removed = db
        .delete_repo_bookmark(host, &crate::paths::expand_tilde(path))
        .map_err(|e| format!("forget repo: {e}"))?;
    match removed {
        true => Ok(()),
        false => Err(format!("not a remembered repository: {path}")),
    }
}

/// Import a folder of repositories: remember the folder, then its members.
///
/// The members are not fixed here. A local folder is re-scanned on every read of
/// the bookmark list and a remote one on its own interval, so this is the first
/// scan rather than the only one — nobody has to re-import a folder to see a
/// repository they cloned into it.
fn bookmark_parent(
    db: &Database,
    host: &str,
    remote: Option<&crate::session::HostDef>,
    expanded: &std::path::Path,
) -> Result<(), String> {
    let children = match remote {
        Some(host) => crate::git::scan_child_repos_on(host, &expanded.to_string_lossy())
            .map_err(|e| format!("{e:#}"))?,
        None => {
            if !expanded.is_dir() {
                return Err(format!("Path not found: {}", expanded.display()));
            }
            crate::git::scan_child_repos(expanded)
        }
    };
    db.upsert_repo_bookmark_kind(host, expanded, true)
        .map_err(|e| format!("remember folder: {e}"))?;
    // Replace rather than merge: these members are a scan, and a scan is the
    // whole truth about the folder, so a repository that has since been deleted
    // must stop being offered. The same write is what a rescan makes later
    // (`kernel::repos::rescan_folder`) — importing is only the first one.
    db.replace_parent_children(host, expanded, &children)
        .map_err(|e| format!("remember folder contents: {e}"))?;
    if children.is_empty() {
        // Not a failure — the folder is remembered — but the user asked a
        // question and "none" is the answer, so it is reported rather than
        // looking like an import that silently did nothing.
        return Err(format!(
            "No repositories found under {}",
            expanded.display()
        ));
    }
    Ok(())
}

/// Remember one path: establish what it is, refuse what is not there, and record
/// the git-ness so the flow knows whether worktree mode is even possible.
fn bookmark_add(
    db: &Database,
    host: &str,
    remote: Option<&crate::session::HostDef>,
    expanded: &std::path::Path,
) -> Result<(), String> {
    let is_git = match remote {
        Some(host) => match crate::git::classify_path_on(host, &expanded.to_string_lossy())
            .map_err(|e| format!("{e:#}"))?
        {
            crate::git::PathClass::Git => Some(true),
            crate::git::PathClass::Dir => Some(false),
            crate::git::PathClass::Missing => {
                return Err(format!(
                    "Path not found on '{}': {}",
                    host.name,
                    expanded.display()
                ))
            }
        },
        None => {
            if !expanded.is_dir() {
                return Err(format!("Path not found: {}", expanded.display()));
            }
            Some(crate::git::is_git_repo(expanded))
        }
    };

    // Touches recency as well as adding, which is what lets the flow re-select a
    // path that was already remembered: the row it asked for is the newest one.
    db.upsert_repo_bookmark_checked(host, expanded, is_git)
        .map_err(|e| format!("remember repo: {e}"))
}

/// Fork a session: a new one on the same repository, recording its parent.
///
/// The work is [`crate::session_ops::fork_session_headless`], so the interface
/// and `thurbox-cli session fork` produce the same session rather than two
/// implementations that drift.
fn fork(
    db: &Database,
    backends: &crate::backend::BackendRegistry,
    id: SessionId,
    name: &str,
) -> Result<(), String> {
    crate::session_ops::fork_session_headless(db, backends, id, name)
        .map(|_| ())
        .map_err(|e| e.to_string())
}

/// Refuses rather than asking the user to be careful: a sync that discards
/// uncommitted work is indistinguishable from a bug at the moment it happens.
pub(super) fn sync(db: &Database, id: SessionId) -> Result<(), String> {
    let session = db
        .get_session_by_id(id)
        .map_err(|e| format!("get session: {e}"))?
        .ok_or_else(|| format!("session not found: {id}"))?;

    if session.worktrees.is_empty() {
        return Err("this session has no worktree to sync".to_string());
    }

    // A multi-repo session has one worktree per repository, all on the same
    // branch — syncing only the first left the others behind, which is worse than
    // not syncing at all: the session then spans repositories at different bases.
    //
    // On the session's own machine, too. A remote worktree's path does not exist
    // here, so the local `git` would either fail or — with an unlucky path
    // collision — rebase something else entirely.
    let host = crate::session_ops::resolve_host(&session.backend_type).ok_or_else(|| {
        format!(
            "'{}' runs on backend '{}', which is not in hosts.toml — cannot reach \
             the machine its worktrees live on",
            session.name, session.backend_type
        )
    })?;

    // `sync_worktree` stashes, rebases and pops — and on conflict aborts and
    // restores the stash. So uncommitted work is never lost, which is why this
    // does not pre-refuse a dirty worktree the way a naive rebase would have to.
    let base = db
        .get_session_base_branch(id)
        .map_err(|e| format!("read base branch: {e}"))?;

    let mut synced = 0;
    for worktree in &session.worktrees {
        match crate::git::sync_worktree_on(host.as_ref(), &worktree.worktree_path, base.as_deref())
        {
            crate::git::SyncResult::Synced => synced += 1,
            // Not an error in the "something broke" sense: the rebase was undone
            // and the worktree is exactly as it was. Reported so the user knows
            // why nothing moved — and reported for the repository it happened in,
            // since the others may have synced.
            crate::git::SyncResult::Conflict(detail) => {
                return Err(format!(
                "sync stopped in {} — conflicts, nothing changed there ({synced} synced): {detail}",
                worktree.branch
            ))
            }
            crate::git::SyncResult::Error(detail) => {
                return Err(format!(
                    "sync failed in {} ({synced} synced): {detail}",
                    worktree.branch
                ))
            }
        }
    }
    Ok(())
}

/// Create, edit, retitle or delete a task, through the verbs `thurbox-cli task`
/// shares.
fn task(
    db: &Database,
    id: Option<i64>,
    title: &Option<String>,
    status: &Option<String>,
    delete: bool,
) -> Result<(), String> {
    use crate::session_ops::tasks;

    let status: Option<crate::session::TaskStatus> =
        status.as_deref().map(str::parse).transpose()?;
    let Some(id) = id else {
        let title = title.clone().ok_or("a new task needs a title")?;
        let new = crate::storage::tasks::NewTask {
            status: status.unwrap_or_default(),
            ..crate::storage::tasks::NewTask::local(title)
        };
        return tasks::create(db, new)
            .map(|_| ())
            .map_err(|e| e.to_string());
    };
    if delete {
        return tasks::delete(db, id).map_err(|e| e.to_string());
    }
    let changes = tasks::TaskEdit {
        title: title.clone(),
        status,
        ..Default::default()
    };
    tasks::edit(db, id, changes)
        .map(|_| ())
        .map_err(|e| e.to_string())
}

/// Hand a task to an agent, through the verb `thurbox-cli task run` shares.
///
/// A skip is a failure here: a command reports nothing else, and a reader who
/// pressed a key needs to see why nothing happened.
fn dispatch_task(
    db: &Database,
    backends: &crate::backend::BackendRegistry,
    task_id: i64,
    session: Option<&str>,
) -> Result<(), String> {
    use crate::session_ops::tasks::{dispatch, Dispatched};
    let task = crate::session_ops::tasks::get(db, task_id).map_err(|e| e.to_string())?;
    let session = session
        .map(|s| {
            s.parse::<SessionId>()
                .map_err(|_| format!("not a session id: {s}"))
        })
        .transpose()?;
    match dispatch(db, backends, &task, session)? {
        Dispatched::Skipped(reason) => Err(reason.to_string()),
        _ => Ok(()),
    }
}

/// Enable, disable, run or delete an automation — through the session
/// engine's verbs, the ones `thurbox-cli automation` uses.
fn automation(
    db: &Database,
    id: i64,
    enabled: Option<bool>,
    run_now: bool,
    delete: bool,
) -> Result<(), String> {
    use crate::session_ops::automations as ops;
    if delete {
        return ops::remove(db, id).map(|_| ());
    }
    if let Some(enabled) = enabled {
        ops::set_enabled(db, id, enabled, crate::sync::current_time_millis())?;
    }
    if run_now {
        // Marks it due; the next `automation tick` — the heartbeat keeper's,
        // a cron's, or a hand-run one — executes it. The TUI runs no scheduler.
        ops::trigger(db, id)?;
    }
    Ok(())
}

/// Create an automation (`id` is `None`) or edit one, from a pane's draft.
fn save_automation(
    db: &Database,
    id: Option<i64>,
    draft: &super::AutomationDraft,
    now: u64,
) -> Result<(), String> {
    use crate::session_ops::automations as ops;
    match id {
        Some(id) => ops::edit(db, id, draft, now),
        None => ops::create(db, draft, None, ops::OnExisting::Fail, now)
            .map(|_| ())
            .map_err(|e| e.to_string()),
    }
}

/// Move a session `delta` places in the manual order and renumber densely.
///
/// Renumbering every row (rather than nudging one) is what v1 does, and it is
/// what makes the order stable: a session that has never been moved sorts last
/// by `display_order IS NULL`, and one move gives everything a definite place.
///
/// The listing and the renumbering are one transaction
/// ([`Database::reorder_sessions`]), which is what lets two moves from a held-
/// down key both land: reorder is a read-modify-write over *every* row and
/// commands each run on their own thread.
fn reorder(db: &Database, id: SessionId, delta: i64) -> Result<(), String> {
    db.reorder_sessions(|rows| {
        let mut sessions: Vec<&crate::sync::SharedSession> = rows.iter().collect();

        // The order the list renders in: manual position first, then name — the
        // same comparator ui/plugins/10_sessions.lua uses, or a move would appear
        // to jump.
        sessions.sort_by(|a, b| {
            a.display_order
                .unwrap_or(i64::MAX)
                .cmp(&b.display_order.unwrap_or(i64::MAX))
                .then_with(|| a.name.cmp(&b.name))
        });

        let at = sessions
            .iter()
            .position(|s| s.id == id)
            .ok_or_else(|| format!("session not in the active list: {id}"))?;

        // A move stays inside its repo group, because that is how the list is
        // drawn: swapping past a group edge would reorder the underlying list
        // while the screen appeared not to change at all.
        let repo_of = |s: &crate::sync::SharedSession| {
            snapshot::repo_name(&s.cwd, s.worktrees.first().map(|w| &w.repo_path))
        };
        let group = repo_of(sessions[at]);

        let step = if delta > 0 { 1i64 } else { -1 };
        let mut target = at as i64 + step;
        while target >= 0 && target < sessions.len() as i64 {
            if repo_of(sessions[target as usize]) == group {
                break;
            }
            target += step;
        }
        if target < 0 || target >= sessions.len() as i64 {
            // Already at its group's edge; not an error, just nothing to do.
            return Ok(Vec::new());
        }
        sessions.swap(at, target as usize);

        Ok(sessions
            .iter()
            .enumerate()
            .map(|(position, session)| (session.id, position as i64))
            .collect())
    })
}

/// Persist an explicit manual order and renumber densely.
///
/// `list` is the rendered order, as computed by the pane that drew it. Sessions
/// the list did not mention keep their relative order and follow the ones it
/// did — so a permutation of a filtered view cannot silently discard rows it
/// never showed.
fn order(db: &Database, list: &[String]) -> Result<(), String> {
    // Same transaction as `reorder`: two concurrent renumberings must not
    // interleave, and neither may revive a row deleted between the two halves.
    db.reorder_sessions(|sessions| {
        let rank: std::collections::HashMap<&str, usize> = list
            .iter()
            .enumerate()
            .map(|(position, id)| (id.as_str(), position))
            .collect();

        // Unmentioned rows sort after every mentioned one, keeping the order
        // they already had among themselves.
        let existing =
            |session: &crate::sync::SharedSession| session.display_order.unwrap_or(i64::MAX);
        let mut indexed: Vec<(usize, i64, usize)> = sessions
            .iter()
            .enumerate()
            .map(|(index, session)| {
                let key = rank
                    .get(session.id.to_string().as_str())
                    .copied()
                    .unwrap_or(usize::MAX);
                (key, existing(session), index)
            })
            .collect();
        indexed.sort_by(|a, b| {
            a.0.cmp(&b.0)
                .then_with(|| a.1.cmp(&b.1))
                .then_with(|| a.2.cmp(&b.2))
        });

        Ok(indexed
            .into_iter()
            .enumerate()
            .map(|(position, (_, _, index))| (sessions[index].id, position as i64))
            .collect())
    })
}

#[cfg(test)]
mod tests {
    /// The pane's automation commands write through the session engine's own
    /// verbs, so what `thurbox-cli automation` refuses the pane refuses too.
    mod automation_writes {
        use super::super::{automation, save_automation};
        use crate::kernel::command::AutomationDraft;
        use crate::storage::Database;

        // 2024-01-01 00:00:00 UTC, a Monday.
        const NOW: u64 = 1_704_067_200_000;

        #[test]
        fn a_pane_create_and_edit_go_through_the_shared_rules() {
            let db = Database::open_in_memory().expect("db");
            let draft = AutomationDraft {
                name: Some("sync".into()),
                trigger: Some("hourly".into()),
                command: Some("make sync".into()),
                timezone: Some("Mars/Olympus".into()),
                ..AutomationDraft::default()
            };
            let error = save_automation(&db, None, &draft, NOW).expect_err("bad timezone");
            assert!(error.contains("timezone"), "{error}");
            let draft = AutomationDraft {
                timezone: None,
                ..draft
            };
            save_automation(&db, None, &draft, NOW).expect("create");
            let id = db.list_automations().expect("list")[0].id;
            automation(&db, id, Some(false), false, false).expect("disable");
            assert!(!db.get_automation(id).expect("get").expect("row").enabled);
        }

        /// A run of a row that is gone says so, rather than calling it disabled.
        #[test]
        fn running_a_deleted_automation_says_it_no_longer_exists() {
            let db = Database::open_in_memory().expect("db");
            let error = automation(&db, 404, None, true, false).unwrap_err();
            assert_eq!(error, "automation #404 no longer exists");
        }
    }
}
