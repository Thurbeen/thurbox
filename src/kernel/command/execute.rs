//! The effects: what each accepted command actually does, on its own thread
//! with its own database connection.

use std::sync::mpsc::Sender;
use std::sync::{Mutex, PoisonError};

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
    id: u64,
    progress: &Sender<Progress>,
) -> Result<(), String> {
    // Handled by the loop before dispatch, because they mutate in-process state
    // the worker cannot reach. Reaching here means a caller bypassed that.
    if command.applied_on_ui_thread() {
        return Err("applied on the UI thread, not dispatched".to_string());
    }

    // Creation names a repository rather than a session, so it runs before the
    // id is parsed — there is nothing to parse yet.
    if let Command::Create {
        name,
        repo,
        branch,
        base,
        agent,
        host,
        extras,
    } = command
    {
        return create(name, repo, branch, base, agent, host, extras, id, progress);
    }

    // Repository memory names a path, not a session, so it too runs before the
    // id parse.
    if let Command::Bookmark { host, path, edit } = command {
        return bookmark(host, path, *edit);
    }

    // The settings file names nothing at all.
    if let Command::Configure { settings } = command {
        return crate::agent::settings_config::save_settings(settings)
            .map_err(|e| format!("write settings.toml: {e}"));
    }

    // Keyed by nothing at all: an explicit order names every session at once.
    if let Command::Order { list } = command {
        let path = crate::paths::database_file().ok_or("could not resolve the database path")?;
        let db = Database::open(&path).map_err(|e| format!("open database: {e}"))?;
        return order(&db, list);
    }

    // Tasks and automations are keyed by number, not by session id.
    if matches!(
        command,
        Command::Task { .. } | Command::DispatchTask { .. } | Command::Automation { .. }
    ) {
        let path = crate::paths::database_file().ok_or("could not resolve the database path")?;
        let db = Database::open(&path).map_err(|e| format!("open database: {e}"))?;
        return match command {
            Command::Task {
                id,
                title,
                status,
                delete,
            } => task(&db, *id, title, status, *delete),
            Command::DispatchTask { task, session } => {
                dispatch_task(&db, *task, session.as_deref())
            }
            Command::Automation {
                id,
                enabled,
                run_now,
                delete,
            } => automation(&db, *id, *enabled, *run_now, *delete),
            _ => unreachable!(),
        };
    }

    let id: SessionId = command
        .session()
        .parse()
        .map_err(|_| format!("not a session id: {}", command.session()))?;

    // Its own connection: sharing the UI thread's would mean locking against
    // the very reads this is supposed to keep instant.
    let path = crate::paths::database_file().ok_or("could not resolve the database path")?;
    let db = Database::open(&path).map_err(|e| format!("open database: {e}"))?;

    // A fork mints a session too; like a creation, the new row simply appears
    // in the list rather than pulling the selection onto itself.
    if let Command::Fork { name, .. } = command {
        return fork(&db, id, name);
    }

    match command {
        Command::Delete { force, .. } => {
            crate::session_ops::delete_session_headless(&db, id, *force).map(|_| ())
        }
        // Restoring is the row, its worktrees and its agent — clearing the flag
        // alone gives back a session that can never attach (parity-gap #11).
        Command::Restore { best_effort, .. } => {
            crate::session_ops::restore_session_headless(&db, id, *best_effort).map(|report| {
                if let Some(error) = report.respawn_error {
                    // Restored, but without its agent: worth saying, not worth
                    // undoing — `restart` will try again.
                    tracing::warn!("restored {} but could not launch it: {error}", report.name);
                }
            })
        }
        Command::Restart { .. } => crate::session_ops::restart_session_headless(&db, id),
        Command::Reap { .. } => crate::session_ops::reap_soft_deleted(&db, id).map(|_| ()),
        Command::Send { text, .. } => {
            let session = db
                .get_session_by_id(id)
                .map_err(|e| format!("get session: {e}"))?
                .ok_or_else(|| format!("session not found: {id}"))?;
            crate::agent::tmux::send_prompt_now(&session.name, text)
                .map_err(|e| format!("send: {e}"))
        }
        Command::Reorder { delta, .. } => reorder(&db, id, *delta),
        // Handled above, before the session id is parsed.
        Command::Order { .. } => Ok(()),
        Command::Fork { .. } => unreachable!("handled above, where it mints its session"),

        Command::Sync { .. } => sync(&db, id),
        // Unreachable: guarded above, and kept exhaustive so adding a command
        // is a compile error here rather than a silent no-op.
        // Handled above, before the session id is parsed.
        Command::Create { .. }
        | Command::Bookmark { .. }
        | Command::Configure { .. }
        | Command::Task { .. }
        | Command::DispatchTask { .. }
        | Command::Automation { .. } => unreachable!("handled before the id parse"),
        Command::Theme { .. }
        | Command::Setting { .. }
        | Command::Copy { .. }
        | Command::Diff { .. }
        | Command::OpenLink { .. }
        | Command::Shell { .. }
        | Command::Program { .. }
        | Command::Editor { .. }
        | Command::Focus { .. }
        | Command::Plugin { .. } => unreachable!("applied on the UI thread"),
    }
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
    name: &str,
    repo: &str,
    branch: &Option<String>,
    base: &Option<String>,
    agent: &Option<String>,
    host: &Option<String>,
    extras: &[ExtraMember],
    id: u64,
    progress: &Sender<Progress>,
) -> Result<(), String> {
    let path = crate::paths::database_file().ok_or("could not resolve the database path")?;
    let db = Database::open(&path).map_err(|e| format!("open database: {e}"))?;

    let repo_path = crate::paths::expand_tilde(repo);
    // Only a local target can be checked here. Statting a *remote* path on this
    // machine is worse than not checking at all: it refuses a perfectly good
    // repository whenever the two filesystems disagree, which for a WSL distro
    // or an ssh host is always. The flow already validated it on the host when
    // the path was remembered, and the spawn reports the truth either way.
    if host.is_none() && !repo_path.is_dir() {
        return Err(format!("not a directory: {}", repo_path.display()));
    }

    // An unnamed session takes the branch, else the repo directory — the same
    // defaults the CLI applies, so a plugin need not invent one.
    let name = if name.is_empty() {
        branch
            .clone()
            .or_else(|| {
                repo_path
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
            })
            .unwrap_or_else(|| "session".to_string())
    } else {
        name.to_string()
    };

    let request = crate::session_ops::spawn::SpawnRequest {
        name,
        repo_path,
        worktree_branch: branch.clone(),
        base_branch: base.clone(),
        agent: agent.clone(),
        host: host.clone(),
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
    crate::session_ops::spawn::spawn_session_headless_with_progress(&db, request, Some(&report))
        .map(|_| ())
}

/// Remember, forget or import a repository path.
///
/// Runs here rather than in the flow because every branch of it touches the
/// world: expanding a `~` on an ssh host is a round trip, so is establishing
/// whether a path is a repository, and scanning a folder is another. The
/// refusal of a missing path is the point — catching a typo now beats failing
/// minutes later inside `git worktree add`.
fn bookmark(host: &str, path: &str, edit: BookmarkEdit) -> Result<(), String> {
    let db_path = crate::paths::database_file().ok_or("could not resolve the database path")?;
    let db = Database::open(&db_path).map_err(|e| format!("open database: {e}"))?;

    // Removal names a path that is already remembered — an absolute one, since
    // the rows the flow offers come from the database — so it needs neither the
    // filesystem nor the host. Done BEFORE the host is resolved, which is what
    // lets a bookmark be forgotten after its host has been taken out of
    // `hosts.toml`.
    if edit == BookmarkEdit::Remove {
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

    match edit {
        BookmarkEdit::Parent => bookmark_parent(&db, host, remote.as_ref(), &expanded),
        _ => bookmark_add(&db, host, remote.as_ref(), &expanded),
    }
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
    // Replace rather than merge: re-importing is how a folder is refreshed, so a
    // repository that has since been deleted must stop being offered.
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
    // path that was already remembered: the row it asked for is the newest one
    // (design.md D2).
    db.upsert_repo_bookmark_checked(host, expanded, is_git)
        .map_err(|e| format!("remember repo: {e}"))
}

/// Fork a session: a new one on the same repository, recording its parent.
fn fork(db: &Database, id: SessionId, name: &str) -> Result<(), String> {
    let source = db
        .get_session_by_id(id)
        .map_err(|e| format!("get session: {e}"))?
        .ok_or_else(|| format!("session not found: {id}"))?;

    // The parent's *working directory* — its worktree for a worktree session —
    // not the repository root. A cwd-scoped agent (`codex resume --last`,
    // `opencode --continue`) resolves "the last session here" from it, so the
    // repo root would find nothing to continue.
    let repo_path = source
        .cwd
        .clone()
        .or_else(|| {
            source
                .worktrees
                .first()
                .map(|worktree| worktree.worktree_path.clone())
        })
        .ok_or("the source session has no directory to fork in")?;

    let name = if name.is_empty() {
        format!("{}-fork", source.name)
    } else {
        name.to_string()
    };

    let request = crate::session_ops::spawn::SpawnRequest {
        name,
        repo_path,
        // No new worktree (the defaults): a fork works beside its parent, on
        // the same branch.
        agent: Some(source.agent.clone()),
        // A fork of a remote session belongs on that session's host, or it would
        // silently become a local session pointed at a path that is not here.
        host: host_name_for(&source.backend_type),
        parent_session_id: Some(source.id),
        // What actually makes it a fork: the agent resumes the parent's
        // conversation into a new one (`fork_args`).
        fork_session_id: source.agent_session_id.clone(),
        // Shared, not created — so the fork shows its branch and can be synced.
        inherit_worktrees: source.worktrees.clone(),
        ..Default::default()
    };
    crate::session_ops::spawn::spawn_session_headless(db, request).map(|_| ())
}

/// Bring a session's worktree up to date with the branch it came from.
///
/// The bare host name behind a persisted backend (`ssh:devbox` → `devbox`), or
/// `None` for a local session.
///
/// `SpawnRequest.host` names a host as `hosts.toml` does, while a session row
/// stores the backend it runs on — the registry is what maps between them, and
/// a name it does not know is treated as local rather than guessed at.
fn host_name_for(backend_type: &str) -> Option<String> {
    crate::session_ops::resolve_host(backend_type)
        .flatten()
        .map(|host| host.name)
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
fn dispatch_task(db: &Database, task_id: i64, session: Option<&str>) -> Result<(), String> {
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
            crate::agent::tmux::send_prompt_now(&target.name, &prompt)
                .map_err(|e| format!("send: {e}"))?;
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
            let spawned = crate::session_ops::spawn::spawn_session_headless(db, request)?;
            // The agent needs a moment to be ready for input; sending into a
            // shell that has not drawn its prompt loses the text.
            crate::agent::tmux::send_prompt_after_delay(&spawned.name, &prompt, 3)
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
    if let Some(enabled) = enabled {
        db.set_automation_enabled(id, enabled)
            .map_err(|e| format!("set enabled: {e}"))?;
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

/// Serializes reordering.
///
/// Reorder is a read-modify-write over *every* row, and commands run on
/// independent threads — so holding a key down would otherwise have two moves
/// read the same order, swap the same pair, and land as one. Ordering is the
/// only command with this shape; the rest touch a single row and need no lock.
static ORDER_LOCK: Mutex<()> = Mutex::new(());

/// Move a session `delta` places in the manual order and renumber densely.
///
/// Renumbering every row (rather than nudging one) is what v1 does, and it is
/// what makes the order stable: a session that has never been moved sorts last
/// by `display_order IS NULL`, and one move gives everything a definite place.
fn reorder(db: &Database, id: SessionId, delta: i64) -> Result<(), String> {
    // Poisoning only means an earlier reorder panicked; the order is still
    // consistent on disk, so recovering is better than refusing every future
    // move.
    let _guard = ORDER_LOCK.lock().unwrap_or_else(PoisonError::into_inner);

    let mut sessions = db
        .list_active_sessions()
        .map_err(|e| format!("list sessions: {e}"))?;

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
    let group = repo_of(&sessions[at]);

    let step = if delta > 0 { 1i64 } else { -1 };
    let mut target = at as i64 + step;
    while target >= 0 && target < sessions.len() as i64 {
        if repo_of(&sessions[target as usize]) == group {
            break;
        }
        target += step;
    }
    if target < 0 || target >= sessions.len() as i64 {
        // Already at its group's edge; not an error, just nothing to do.
        return Ok(());
    }
    sessions.swap(at, target as usize);

    for (position, session) in sessions.iter_mut().enumerate() {
        session.display_order = Some(position as i64);
        db.upsert_session(session)
            .map_err(|e| format!("persist order: {e}"))?;
    }
    Ok(())
}

/// Persist an explicit manual order and renumber densely.
///
/// `list` is the rendered order, as computed by the pane that drew it. Sessions
/// the list did not mention keep their relative order and follow the ones it
/// did — so a permutation of a filtered view cannot silently discard rows it
/// never showed.
fn order(db: &Database, list: &[String]) -> Result<(), String> {
    // Same lock as `reorder`: two concurrent renumberings must not interleave.
    let _guard = ORDER_LOCK.lock().unwrap_or_else(PoisonError::into_inner);

    let mut sessions = db
        .list_active_sessions()
        .map_err(|e| format!("list sessions: {e}"))?;

    let rank: std::collections::HashMap<&str, usize> = list
        .iter()
        .enumerate()
        .map(|(position, id)| (id.as_str(), position))
        .collect();

    // Unmentioned rows sort after every mentioned one, keeping the order they
    // already had among themselves.
    let existing = |session: &crate::sync::SharedSession| session.display_order.unwrap_or(i64::MAX);
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

    let permutation: Vec<usize> = indexed.into_iter().map(|(_, _, index)| index).collect();
    for (position, index) in permutation.into_iter().enumerate() {
        sessions[index].display_order = Some(position as i64);
    }
    for session in sessions.iter_mut() {
        db.upsert_session(session)
            .map_err(|e| format!("persist order: {e}"))?;
    }
    Ok(())
}
