//! The effects: what each accepted command actually does, on its own thread
//! with its own database connection.

use std::sync::mpsc::Sender;

use super::bus::Progress;
use super::{BookmarkEdit, Command, ExtraMember};
use crate::kernel::snapshot;
use crate::session::SessionId;
use crate::storage::Database;

/// Run one command. Called on the command's own thread, never the UI thread.
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
        crate::session_ops::reap_overdue_soft_deletes(&db, backends);
        // The same sweep's other half: force deletes whose teardown never
        // reached the host they were owed on.
        crate::session_ops::retry_owed_remote_teardowns(&db, backends);
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
            crate::session_ops::delete_session_headless(&db, backends, id, *force).map(|_| ())
        }
        // Restoring is the row, its worktrees and its agent — clearing the flag
        // alone gives back a session that can never attach (parity-gap #11).
        Command::Restore { best_effort, .. } => {
            crate::session_ops::restore_session_headless(&db, backends, id, *best_effort).map(
                |report| {
                    if let Some(error) = report.respawn_error {
                        // Restored, but without its agent: worth saying, not worth
                        // undoing — `restart` will try again.
                        tracing::warn!("restored {} but could not launch it: {error}", report.name);
                    }
                },
            )
        }
        // A failed post-restart hook is already in the log; the restart stands.
        Command::Restart { if_missing, .. } => {
            crate::session_ops::restart::restart_session_headless_with(
                &db,
                backends,
                id,
                *if_missing,
            )
            .map(|_| ())
        }
        Command::Send { text, .. } => {
            let session = db
                .get_session_by_id(id)
                .map_err(|e| format!("get session: {e}"))?
                .ok_or_else(|| format!("session not found: {id}"))?;
            crate::session_ops::send_text_with_status(&db, backends, &session, text, true)
                .map_err(|e| format!("send: {e:#}"))
        }
        Command::RetireHook {
            state, state_at, ..
        } => db
            .clear_hook_state_if_unchanged(
                id,
                &crate::storage::HookRow {
                    state: Some(state.clone()),
                    state_at: *state_at,
                    seen_at: None,
                },
            )
            .map(|_| ())
            .map_err(|e| format!("retire hook: {e}")),
        Command::Reorder { delta, .. } => reorder(&db, id, *delta),
        // Handled above, before the session id is parsed.
        Command::Order { .. } => Ok(()),
        Command::Fork { .. } => unreachable!("handled above, where it mints its session"),

        Command::Sync { .. } => sync(&db, id),
        Command::Rename { name, .. } => {
            crate::session_ops::rename::rename_session_headless(&db, backends, id, name).map(|_| ())
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

/// The name an unnamed create gets.
///
/// Order: the **worktree directory's** own name when one is being opened, then
/// the branch, then the repository directory. The worktree directory leads
/// because that name is the one a person chose — an agent cutting a worktree
/// from an issue writes `.worktrees/dynamic-tooltips` while the branch it puts
/// there carries a disambiguating suffix
/// (`feat/dynamic-tooltips-15307729713678226529`), and naming the session after
/// the branch would put that suffix in the session list. Creating a worktree is
/// unaffected: there is no directory yet, so the branch still names it, exactly
/// as the CLI does.
fn session_name(
    given: &str,
    branch: Option<&str>,
    worktree_path: Option<&str>,
    repo_path: &std::path::Path,
) -> String {
    if !given.is_empty() {
        return given.to_string();
    }
    worktree_path
        .map(std::path::Path::new)
        .and_then(std::path::Path::file_name)
        .map(|name| name.to_string_lossy().to_string())
        .or_else(|| branch.map(str::to_string))
        .or_else(|| {
            repo_path
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
        })
        .unwrap_or_else(|| "session".to_string())
}

/// Create a session.
///
/// The whole pipeline — repo resolution, worktree checkout, multi-repo
/// workspace, agent launch — already exists as `spawn_session_headless` and is
/// what `thurbox-cli session create` uses. Reusing it unchanged means creation
/// behaves identically whether it came from a plugin or a script, and there is
/// one cleanup path for a failure rather than two.
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

    let repo_path = crate::paths::expand_tilde(repo);
    // Only a local target can be checked here. Statting a *remote* path on this
    // machine is worse than not checking at all: it refuses a perfectly good
    // repository whenever the two filesystems disagree, which for a WSL distro
    // or an ssh host is always. The flow already validated it on the host when
    // the path was remembered, and the spawn reports the truth either way.
    if host.is_none() && !repo_path.is_dir() {
        return Err(format!("not a directory: {}", repo_path.display()));
    }

    // A worktree to open names the branch checked out there. The pair travels
    // together from the flow, but `create` is reachable from any plugin, and
    // half the pair would record a session whose branch is blank.
    if worktree_path.is_some() && branch.is_none() {
        return Err("opening a worktree needs the branch checked out in it".to_string());
    }

    // Same reasoning as `repo_path` above, and the same local-only caveat: an
    // opened worktree becomes the session's cwd, so a path that isn't there
    // yields a pane that cannot start rather than a stated error. The picker
    // only offers paths `git worktree list` reported, but `thurbox-cli` and
    // plugins can name any path at all.
    let opened = worktree_path.map(crate::paths::expand_tilde);
    if let Some(worktree) = &opened {
        if host.is_none() && !worktree.is_dir() {
            return Err(format!("not a worktree: {}", worktree.display()));
        }
    }

    let name = session_name(name, branch.as_deref(), worktree_path, &repo_path);

    let request = crate::session_ops::spawn::SpawnRequest {
        name,
        repo_path,
        worktree_branch: branch.clone(),
        base_branch: base.clone(),
        existing_worktree: opened,
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
    crate::session_ops::spawn::spawn_session_headless_with_progress(
        &db,
        backends,
        request,
        Some(&report),
    )
    .map(|_| ())
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
    crate::session_ops::fork_session_headless(db, backends, id, name).map(|_| ())
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

/// Create, edit, retitle or delete a task.
fn task(
    db: &Database,
    id: Option<i64>,
    title: &Option<String>,
    status: &Option<String>,
    delete: bool,
) -> Result<(), String> {
    use crate::session::task::TaskStatus;
    use crate::storage::tasks::NewTask;

    let Some(id) = id else {
        let title = title.clone().ok_or("a new task needs a title")?;
        return db
            .create_task(&NewTask::local(title))
            .map(|_| ())
            .map_err(|e| format!("create task: {e}"));
    };

    if delete {
        return db
            .soft_delete_task(id)
            .map(|_| ())
            .map_err(|e| format!("delete task: {e}"));
    }

    if let Some(status) = status {
        // The same three names `thurbox-cli task edit --status` accepts, so a
        // plugin and a script speak one vocabulary.
        let status = match status.as_str() {
            "todo" => TaskStatus::Todo,
            "in_progress" => TaskStatus::InProgress,
            "done" => TaskStatus::Done,
            other => {
                return Err(format!(
                    "not a task status: {other:?} — try todo, in_progress or done"
                ))
            }
        };
        db.set_task_status(id, status)
            .map(|_| ())
            .map_err(|e| format!("set status: {e}"))?;
    }
    if let Some(title) = title {
        let mut existing = db
            .get_task(id)
            .map_err(|e| format!("get task: {e}"))?
            .ok_or_else(|| format!("no task #{id}"))?;
        existing.title = title.clone();
        db.update_task(&existing)
            .map_err(|e| format!("update task: {e}"))?;
    }
    Ok(())
}

/// Hand a task to an agent.
///
/// The prompt is `Task::agent_prompt()` — the same one `thurbox-cli task run`
/// builds — so an agent gets identical context however it was handed the work,
/// and there is one place to change what it is told.
fn dispatch_task(
    db: &Database,
    backends: &crate::backend::BackendRegistry,
    task_id: i64,
    session: Option<&str>,
) -> Result<(), String> {
    use crate::session::task::TaskStatus;

    let task = db
        .get_task(task_id)
        .map_err(|e| format!("get task: {e}"))?
        .ok_or_else(|| format!("no task #{task_id}"))?;
    let prompt = task.agent_prompt();

    match session {
        Some(session) => {
            let id: SessionId = session
                .parse()
                .map_err(|_| format!("not a session id: {session}"))?;
            let target = db
                .get_session_by_id(id)
                .map_err(|e| format!("get session: {e}"))?
                .ok_or_else(|| format!("session not found: {id}"))?;
            crate::session_ops::send_text_with_status(db, backends, &target, &prompt, true)
                .map_err(|e| format!("send: {e:#}"))?;
        }
        None => {
            // Create a session for the task, then hand it the prompt once its
            // pane is live. `spawn_session_headless` returns after the agent is
            // launched, so the delivery is a follow-up rather than part of it.
            let repo = task_repo(db)?;
            let request = crate::session_ops::spawn::SpawnRequest {
                name: format!("task-{task_id}"),
                repo_path: repo,
                task_id: Some(task_id),
                ..Default::default()
            };
            let spawned = crate::session_ops::spawn::spawn_session_headless(db, backends, request)?;
            // The agent needs a moment to be ready for input; sending into a
            // shell that has not drawn its prompt loses the text.
            crate::session_ops::send_text_when_booted(
                db,
                backends,
                spawned.session_id,
                &prompt,
                std::time::Duration::from_secs(3),
            )
            .map_err(|e| format!("send: {e}"))?;
        }
    }

    // Acting on a task moves it out of not-started, whichever way it was
    // dispatched.
    if task.status == TaskStatus::Todo {
        db.set_task_status(task_id, TaskStatus::InProgress)
            .map_err(|e| format!("advance task: {e}"))?;
    }
    Ok(())
}

/// Enable, disable, run or delete an automation.
fn automation(
    db: &Database,
    id: i64,
    enabled: Option<bool>,
    run_now: bool,
    delete: bool,
) -> Result<(), String> {
    if delete {
        return db
            .delete_automation(id)
            .map(|_| ())
            .map_err(|e| format!("delete automation: {e}"));
    }
    match enabled {
        // Disabling clears the next run, so enabling computes it again — as
        // `thurbox-cli automation edit --enabled` does. Setting the flag alone
        // left an enabled automation that never fired.
        Some(true) => {
            let mut auto = db
                .get_automation(id)
                .map_err(|e| format!("get automation: {e}"))?
                .ok_or_else(|| format!("automation #{id} no longer exists"))?;
            // Already on and scheduled: nothing to do, and recomputing would
            // push a fire that is due, or a run-now, to the next slot.
            if !(auto.enabled && auto.next_run_at.is_some()) {
                auto.next_run_at = Some(next_run(&auto, crate::sync::current_time_millis())?);
                auto.enabled = true;
                db.update_automation_definition(&auto)
                    .map_err(|e| format!("update automation: {e}"))?;
            }
        }
        Some(false) => {
            db.set_automation_enabled(id, false)
                .map_err(|e| format!("set enabled: {e}"))?;
        }
        None => {}
    }
    if run_now {
        // Marks it due; the next `automation tick` — the heartbeat keeper's,
        // a cron's, or a hand-run one — executes it, so there is one execution
        // path rather than a second one here. The TUI runs no scheduler.
        db.trigger_automation_now(id)
            .map_err(|e| format!("trigger: {e}"))?;
    }
    Ok(())
}

/// When an enabled automation fires next, or why it never will.
fn next_run(auto: &crate::session::Automation, now: u64) -> Result<u64, String> {
    auto.schedule.check()?;
    auto.schedule
        .next_after(now, auto.timezone.as_deref())
        .ok_or_else(|| {
            format!(
                "`{}` never fires again; give it a schedule in the future",
                auto.schedule.trigger()
            )
        })
}

/// Create an automation (`id` is `None`) or edit one, from a pane's draft.
///
/// The same rules as `thurbox-cli automation create` / `edit`, plus the two the
/// CLI leaves to a schedule that silently never fires: a cron expression must
/// parse and a timezone must exist. An edit changes what the draft names and
/// keeps the rest — the target included, extra repositories and all — and
/// recomputes the next run only when the schedule, timezone or enabled flag
/// moved.
fn save_automation(
    db: &Database,
    id: Option<i64>,
    draft: &super::AutomationDraft,
    now: u64,
) -> Result<(), String> {
    use crate::session::automation::{check_timezone, parse_trigger, AutomationAction};

    let weekday = match draft.weekday {
        Some(day) if !(0..=7).contains(&day) => {
            return Err(format!(
                "invalid weekday `{day}` (use 0=Sun..6=Sat, or 7=Sun)"
            ))
        }
        Some(day) => Some(day as u32),
        None => None,
    };
    let mut auto = match id {
        Some(id) => db
            .get_automation(id)
            .map_err(|e| format!("get automation: {e}"))?
            .ok_or_else(|| format!("automation #{id} no longer exists"))?,
        None => crate::session::Automation {
            id: 0,
            name: String::new(),
            enabled: true,
            schedule: parse_trigger(
                draft
                    .trigger
                    .as_deref()
                    .ok_or("an automation needs a trigger")?,
                draft.time.as_deref(),
                weekday,
            )?,
            timezone: None,
            action: draft_action(db, draft)?,
            prompt: String::new(),
            created_at: now,
            updated_at: now,
            last_run_at: None,
            next_run_at: None,
        },
    };

    if let Some(name) = &draft.name {
        auto.name = name.trim().to_string();
    }
    if auto.name.is_empty() {
        return Err("an automation needs a name".into());
    }
    // What a stored fire depends on, to tell whether the edit moved it.
    let before = (auto.schedule.clone(), auto.timezone.clone(), auto.enabled);
    if id.is_some() {
        // The CLI's rule: `time`/`weekday` only shape a preset, so alone they
        // would be a silent no-op. And an edit never retargets.
        if draft.trigger.is_none() && (draft.time.is_some() || weekday.is_some()) {
            return Err(
                "time and weekday only apply with a trigger (--trigger), as a preset".into(),
            );
        }
        if draft.session.is_some()
            || draft.repo.is_some()
            || draft.branch.is_some()
            || draft.base.is_some()
            || draft.agent.is_some()
        {
            return Err("an edit keeps its target; create a new automation to change it".into());
        }
        if let Some(trigger) = &draft.trigger {
            auto.schedule = parse_trigger(trigger, draft.time.as_deref(), weekday)?;
        }
        if let Some(command) = &draft.command {
            match &mut auto.action {
                AutomationAction::Exec { command: current } if !command.trim().is_empty() => {
                    *current = command.clone();
                }
                AutomationAction::Exec { .. } => return Err("the command must not be empty".into()),
                _ => return Err("only an exec automation runs a command".into()),
            }
        }
    }
    if let Some(timezone) = &draft.timezone {
        auto.timezone = (!timezone.is_empty()).then(|| timezone.clone());
    }
    if let Some(timezone) = &auto.timezone {
        check_timezone(timezone)?;
    }
    if let Some(prompt) = &draft.prompt {
        auto.prompt = prompt.clone();
    }
    if !matches!(auto.action, AutomationAction::Exec { .. }) && auto.prompt.trim().is_empty() {
        return Err("the prompt must not be empty".into());
    }
    if let Some(enabled) = draft.enabled {
        auto.enabled = enabled;
    }
    auto.schedule.check()?;
    let unmoved = id.is_some()
        && before == (auto.schedule.clone(), auto.timezone.clone(), auto.enabled)
        && auto.next_run_at.is_some();
    // An edit that leaves the schedule alone leaves its next run alone: a fire
    // that is due, or a run-now, still happens after a rename.
    if !unmoved {
        auto.next_run_at = if auto.enabled {
            Some(next_run(&auto, now)?)
        } else {
            None
        };
    }

    match id {
        Some(_) => db
            .update_automation_definition(&auto)
            .map_err(|e| format!("update automation: {e}")),
        None => db
            .create_automation(&crate::storage::automations::NewAutomation {
                name: auto.name,
                enabled: auto.enabled,
                schedule: auto.schedule,
                timezone: auto.timezone,
                action: auto.action,
                prompt: auto.prompt,
                next_run_at: auto.next_run_at,
            })
            .map(|_| ())
            .map_err(|e| format!("create automation: {e}")),
    }
}

/// The action a new automation's draft names: exactly one of a session to
/// send to, a repository to spawn in, or a command to run.
fn draft_action(
    db: &Database,
    draft: &super::AutomationDraft,
) -> Result<crate::session::automation::AutomationAction, String> {
    use crate::session::automation::AutomationAction;

    match (&draft.session, &draft.repo, &draft.command) {
        (Some(session), None, None) => {
            let session_id: SessionId = session
                .parse()
                .map_err(|_| format!("not a session id: {session}"))?;
            db.get_session_by_id(session_id)
                .map_err(|e| format!("get session: {e}"))?
                .ok_or_else(|| format!("no session {session} to send to"))?;
            Ok(AutomationAction::Send { session_id })
        }
        (None, Some(repo), None) => Ok(AutomationAction::Spawn {
            repo_path: repo.into(),
            worktree_branch: draft.branch.clone(),
            base_branch: draft.base.clone(),
            agent: draft.agent.clone(),
            extra_repos: Vec::new(),
        }),
        (None, None, Some(command)) if command.trim().is_empty() => {
            Err("the command must not be empty".into())
        }
        (None, None, Some(command)) => Ok(AutomationAction::Exec {
            command: command.clone(),
        }),
        (None, None, None) => {
            Err("an automation needs a target: a session, a repository or a command".into())
        }
        _ => Err("name only one target: a session, a repository or a command".into()),
    }
}

/// A repository to create a task's session in.
///
/// The most recently used one, because a task with no repo of its own belongs
/// wherever you are working — and asking would mean blocking, which a command
/// cannot do.
fn task_repo(db: &Database) -> Result<std::path::PathBuf, String> {
    db.list_active_sessions()
        .map_err(|e| format!("list sessions: {e}"))?
        .into_iter()
        .find_map(|session| {
            session
                .worktrees
                .first()
                .map(|w| w.repo_path.clone())
                .or(session.cwd)
        })
        .ok_or_else(|| "no repository to create a session in — start one session first".to_string())
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
    use super::*;

    #[test]
    fn an_opened_worktree_is_named_after_its_directory() {
        let name = session_name(
            "",
            Some("feat/dynamic-tooltips-15307729713678226529"),
            Some("/repo/.worktrees/dynamic-tooltips"),
            std::path::Path::new("/repo"),
        );
        assert_eq!(name, "dynamic-tooltips");
    }

    #[test]
    fn a_created_worktree_is_still_named_after_its_branch() {
        let name = session_name("", Some("fix-osc-52"), None, std::path::Path::new("/repo"));
        assert_eq!(name, "fix-osc-52");
    }

    #[test]
    fn a_plain_session_is_named_after_its_repository() {
        assert_eq!(
            session_name("", None, None, std::path::Path::new("/srv/thurbox")),
            "thurbox"
        );
    }

    #[test]
    fn a_name_that_was_given_always_wins() {
        let name = session_name(
            "chosen",
            Some("feat/x"),
            Some("/repo/.worktrees/other"),
            std::path::Path::new("/repo"),
        );
        assert_eq!(name, "chosen");
    }

    /// A pane managing automations writes through `automation` alone, so what
    /// the CLI validates has to be validated here too — and a mistake has to
    /// come back as a sentence the pane can show, not as a row that never fires.
    mod automation_writes {
        use super::super::{automation, save_automation};
        use crate::kernel::command::AutomationDraft;
        use crate::session::automation::{AutomationAction, AutomationSchedule, ExtraRepo};
        use crate::storage::automations::NewAutomation;
        use crate::storage::Database;

        // 2024-01-01 00:00:00 UTC, a Monday.
        const NOW: u64 = 1_704_067_200_000;

        fn spawn_draft() -> AutomationDraft {
            AutomationDraft {
                name: Some("nightly".into()),
                trigger: Some("daily".into()),
                time: Some("09:30".into()),
                timezone: Some("UTC".into()),
                prompt: Some("review the queue".into()),
                repo: Some("/srv/app".into()),
                branch: Some("feat/nightly".into()),
                base: Some("main".into()),
                agent: Some("claude".into()),
                ..AutomationDraft::default()
            }
        }

        fn only(db: &Database) -> crate::session::Automation {
            let mut all = db.list_automations().expect("list");
            assert_eq!(all.len(), 1, "{all:?}");
            all.remove(0)
        }

        #[test]
        fn a_created_automation_is_stored_with_its_next_run() {
            let db = Database::open_in_memory().expect("db");
            save_automation(&db, None, &spawn_draft(), NOW).expect("create");
            let auto = only(&db);
            assert_eq!(auto.name, "nightly");
            assert!(auto.enabled);
            assert_eq!(
                auto.schedule,
                AutomationSchedule::Cron {
                    expr: "30 9 * * *".into()
                }
            );
            assert_eq!(auto.timezone.as_deref(), Some("UTC"));
            assert_eq!(auto.prompt, "review the queue");
            assert_eq!(
                auto.action,
                AutomationAction::Spawn {
                    repo_path: "/srv/app".into(),
                    worktree_branch: Some("feat/nightly".into()),
                    base_branch: Some("main".into()),
                    agent: Some("claude".into()),
                    extra_repos: Vec::new(),
                }
            );
            assert_eq!(auto.next_run_at, Some(NOW + (9 * 60 + 30) * 60_000));
        }

        #[test]
        fn a_disabled_create_is_not_scheduled() {
            let db = Database::open_in_memory().expect("db");
            let draft = AutomationDraft {
                enabled: Some(false),
                ..spawn_draft()
            };
            save_automation(&db, None, &draft, NOW).expect("create");
            let auto = only(&db);
            assert!(!auto.enabled);
            assert_eq!(auto.next_run_at, None);
        }

        #[test]
        fn an_exec_automation_needs_a_command_and_no_prompt() {
            let db = Database::open_in_memory().expect("db");
            let draft = AutomationDraft {
                name: Some("sync".into()),
                trigger: Some("cron:*/15 * * * *".into()),
                command: Some("make sync".into()),
                ..AutomationDraft::default()
            };
            save_automation(&db, None, &draft, NOW).expect("create");
            assert_eq!(
                only(&db).action,
                AutomationAction::Exec {
                    command: "make sync".into()
                }
            );
        }

        #[test]
        fn a_create_names_what_is_wrong_and_stores_nothing() {
            let db = Database::open_in_memory().expect("db");
            let cases: Vec<(AutomationDraft, &str)> = vec![
                (
                    AutomationDraft {
                        name: Some("  ".into()),
                        ..spawn_draft()
                    },
                    "name",
                ),
                (
                    AutomationDraft {
                        trigger: Some("monthly".into()),
                        ..spawn_draft()
                    },
                    "unknown trigger",
                ),
                (
                    AutomationDraft {
                        trigger: Some("cron:61 * * * *".into()),
                        time: None,
                        ..spawn_draft()
                    },
                    "cron",
                ),
                (
                    AutomationDraft {
                        time: Some("25:00".into()),
                        ..spawn_draft()
                    },
                    "time",
                ),
                (
                    AutomationDraft {
                        timezone: Some("Mars/Olympus".into()),
                        ..spawn_draft()
                    },
                    "timezone",
                ),
                (
                    AutomationDraft {
                        trigger: Some(format!("at:{}", NOW - 1)),
                        time: None,
                        ..spawn_draft()
                    },
                    "never fires",
                ),
                (
                    AutomationDraft {
                        prompt: Some(String::new()),
                        ..spawn_draft()
                    },
                    "prompt",
                ),
                (
                    AutomationDraft {
                        repo: None,
                        branch: None,
                        base: None,
                        agent: None,
                        ..spawn_draft()
                    },
                    "target",
                ),
                (
                    AutomationDraft {
                        command: Some("make sync".into()),
                        ..spawn_draft()
                    },
                    "only one",
                ),
                (
                    AutomationDraft {
                        repo: None,
                        session: Some("not-a-session".into()),
                        ..spawn_draft()
                    },
                    "session",
                ),
                (
                    AutomationDraft {
                        repo: None,
                        session: Some("6f1c1d8e-2a2b-4c1e-9d1f-0a1b2c3d4e5f".into()),
                        ..spawn_draft()
                    },
                    "no session",
                ),
            ];
            for (draft, expected) in cases {
                let error = save_automation(&db, None, &draft, NOW)
                    .expect_err(&format!("{draft:?} must be refused"));
                assert!(
                    error.contains(expected),
                    "{draft:?}: {error:?} does not mention {expected:?}"
                );
            }
            assert!(db.list_automations().expect("list").is_empty());
        }

        fn stored_multi_repo(db: &Database) -> i64 {
            db.create_automation(&NewAutomation {
                name: "multi".into(),
                enabled: true,
                schedule: AutomationSchedule::Cron {
                    expr: "0 9 * * 1-5".into(),
                },
                timezone: None,
                action: AutomationAction::Spawn {
                    repo_path: "/srv/app".into(),
                    worktree_branch: Some("feat/x".into()),
                    base_branch: None,
                    agent: None,
                    extra_repos: vec![ExtraRepo {
                        repo_path: "/srv/lib".into(),
                        worktree: true,
                        base_branch: None,
                    }],
                },
                prompt: "go".into(),
                next_run_at: Some(NOW + 1),
            })
            .expect("insert")
        }

        #[test]
        fn an_edit_changes_what_it_names_and_keeps_the_target() {
            let db = Database::open_in_memory().expect("db");
            let id = stored_multi_repo(&db);
            let before = only(&db).action;
            let edit = AutomationDraft {
                name: Some("renamed".into()),
                trigger: Some("cron:0 18 * * *".into()),
                timezone: Some("Europe/Zurich".into()),
                prompt: Some("wrap up".into()),
                ..AutomationDraft::default()
            };
            save_automation(&db, Some(id), &edit, NOW).expect("edit");
            let after = only(&db);
            assert_eq!(after.name, "renamed");
            assert_eq!(after.prompt, "wrap up");
            assert_eq!(after.timezone.as_deref(), Some("Europe/Zurich"));
            assert_eq!(after.action, before, "the target and its extra repos stay");
            assert!(after.next_run_at.is_some_and(|next| next > NOW));

            // An empty timezone goes back to the system's own.
            let clear = AutomationDraft {
                timezone: Some(String::new()),
                ..AutomationDraft::default()
            };
            save_automation(&db, Some(id), &clear, NOW).expect("edit");
            assert_eq!(only(&db).timezone, None);
        }

        #[test]
        fn an_edit_of_a_deleted_automation_says_so() {
            let db = Database::open_in_memory().expect("db");
            let error = save_automation(
                &db,
                Some(41),
                &AutomationDraft {
                    name: Some("x".into()),
                    ..AutomationDraft::default()
                },
                NOW,
            )
            .expect_err("nothing to edit");
            assert!(error.contains("#41"), "{error}");
        }

        #[test]
        fn only_an_exec_automation_takes_a_new_command() {
            let db = Database::open_in_memory().expect("db");
            let id = stored_multi_repo(&db);
            let error = save_automation(
                &db,
                Some(id),
                &AutomationDraft {
                    command: Some("rm -rf /".into()),
                    ..AutomationDraft::default()
                },
                NOW,
            )
            .expect_err("a spawn has no command");
            assert!(error.contains("command"), "{error}");

            let exec = AutomationDraft {
                name: Some("sync".into()),
                trigger: Some("hourly".into()),
                command: Some("make sync".into()),
                ..AutomationDraft::default()
            };
            save_automation(&db, None, &exec, NOW).expect("create");
            let exec_id = db
                .list_automations()
                .expect("list")
                .into_iter()
                .find(|a| a.name == "sync")
                .expect("created")
                .id;
            save_automation(
                &db,
                Some(exec_id),
                &AutomationDraft {
                    command: Some("make sync-all".into()),
                    ..AutomationDraft::default()
                },
                NOW,
            )
            .expect("edit");
            let edited = db.get_automation(exec_id).expect("get").expect("row");
            assert_eq!(
                edited.action,
                AutomationAction::Exec {
                    command: "make sync-all".into()
                }
            );
        }

        /// Enabling what is already enabled changes nothing: a fire that is due,
        /// or that run-now just marked, must not be pushed to the next slot.
        #[test]
        fn enabling_an_enabled_automation_keeps_a_pending_fire() {
            let db = Database::open_in_memory().expect("db");
            let id = stored_multi_repo(&db);
            assert!(db.trigger_automation_now(id).expect("run now"));
            let due = only(&db).next_run_at;
            automation(&db, id, Some(true), false, false).expect("enable");
            assert_eq!(only(&db).next_run_at, due, "the run-now survives");
        }

        /// An edit that leaves the schedule alone leaves its next run alone, so
        /// a fire already due — a run-now, or a one-shot not yet ticked — still
        /// happens after a rename.
        #[test]
        fn an_edit_that_keeps_the_schedule_keeps_a_due_fire() {
            let db = Database::open_in_memory().expect("db");
            let id = stored_multi_repo(&db);
            assert!(db.trigger_automation_now(id).expect("run now"));
            let due = only(&db).next_run_at;
            let rename = AutomationDraft {
                name: Some("renamed".into()),
                ..AutomationDraft::default()
            };
            save_automation(&db, Some(id), &rename, NOW + 120_000).expect("edit");
            assert_eq!(only(&db).next_run_at, due);

            let once = db
                .create_automation(&NewAutomation {
                    name: "once".into(),
                    enabled: true,
                    schedule: AutomationSchedule::Once { at: NOW },
                    timezone: None,
                    action: AutomationAction::Exec {
                        command: "true".into(),
                    },
                    prompt: String::new(),
                    next_run_at: Some(NOW),
                })
                .expect("insert");
            save_automation(&db, Some(once), &rename, NOW + 1_000).expect("a due one-shot renames");
        }

        #[test]
        fn an_edit_refuses_what_it_cannot_apply() {
            let db = Database::open_in_memory().expect("db");
            let id = stored_multi_repo(&db);
            let cases: Vec<(AutomationDraft, &str)> = vec![
                (
                    AutomationDraft {
                        time: Some("18:00".into()),
                        ..AutomationDraft::default()
                    },
                    "--trigger",
                ),
                (
                    AutomationDraft {
                        weekday: Some(5),
                        ..AutomationDraft::default()
                    },
                    "--trigger",
                ),
                (
                    AutomationDraft {
                        repo: Some("/srv/other".into()),
                        ..AutomationDraft::default()
                    },
                    "target",
                ),
                (
                    AutomationDraft {
                        session: Some("6f1c1d8e-2a2b-4c1e-9d1f-0a1b2c3d4e5f".into()),
                        ..AutomationDraft::default()
                    },
                    "target",
                ),
            ];
            for (draft, expected) in cases {
                let error = save_automation(&db, Some(id), &draft, NOW)
                    .expect_err(&format!("{draft:?} must be refused"));
                assert!(error.contains(expected), "{draft:?}: {error}");
            }
        }

        #[test]
        fn a_weekday_out_of_range_is_refused_not_read_as_monday() {
            let db = Database::open_in_memory().expect("db");
            for weekday in [-1, 8] {
                let draft = AutomationDraft {
                    trigger: Some("weekly".into()),
                    weekday: Some(weekday),
                    ..spawn_draft()
                };
                let error = save_automation(&db, None, &draft, NOW).expect_err("refused");
                assert!(error.contains("weekday"), "{weekday}: {error}");
            }
        }

        /// Disabling clears the next run, so enabling has to compute it again —
        /// before this, an automation switched back on from a pane was enabled
        /// and never fired.
        #[test]
        fn enabling_from_a_pane_schedules_the_next_run_again() {
            let db = Database::open_in_memory().expect("db");
            let id = stored_multi_repo(&db);
            automation(&db, id, Some(false), false, false).expect("disable");
            assert_eq!(only(&db).next_run_at, None);
            automation(&db, id, Some(true), false, false).expect("enable");
            let auto = only(&db);
            assert!(auto.enabled);
            assert!(auto.next_run_at.is_some(), "an enabled schedule must fire");
        }

        #[test]
        fn enabling_a_spent_one_shot_is_refused_rather_than_silent() {
            let db = Database::open_in_memory().expect("db");
            let id = db
                .create_automation(&NewAutomation {
                    name: "once".into(),
                    enabled: false,
                    schedule: AutomationSchedule::Once { at: 5 },
                    timezone: None,
                    action: AutomationAction::Exec {
                        command: "true".into(),
                    },
                    prompt: String::new(),
                    next_run_at: None,
                })
                .expect("insert");
            let error = automation(&db, id, Some(true), false, false).expect_err("spent");
            assert!(error.contains("never fires"), "{error}");
            assert!(!only(&db).enabled);
        }
    }
}
