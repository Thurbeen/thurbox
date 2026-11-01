//! Handing a task to an agent: the one implementation the interface's
//! `dispatch` command and `thurbox-cli task run` both call (#1468), so a task
//! gets the same session, reuse and refusal whichever of them hands it over.

use crate::session::{AutomationAction, SessionId, Task, TaskStatus};
use crate::storage::Database;
use crate::sync::SharedSession;

use super::deliver::{spawn_and_deliver, SpawnDeliverError};
use super::SpawnRequest;

/// What [`dispatch`] did with a task.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Dispatched {
    /// Typed into a running session.
    Sent { session_id: SessionId },
    /// Typed into the running session the task spawned earlier.
    Reused { name: String },
    /// A new session spawned for the task, handed the prompt once it booted.
    Spawned { name: String },
    /// Nothing to hand it to; the reason is the text the caller shows.
    Skipped(&'static str),
}

/// Why a task without an action is not dispatched.
pub const NOT_CONNECTED: &str = "task is not connected to an agent";
/// Why a task carrying an Exec action is not dispatched.
pub const EXEC_UNSUPPORTED: &str = "exec action is not supported for tasks";

/// Hand `task` to an agent and advance it `Todo → InProgress`.
///
/// `session` overrides the task's own action with a send to that session, which
/// is what the interface's `dispatch` passes when a pane names one. Otherwise a
/// Send action types into its session, and a Spawn action reuses the running
/// session tagged with the task or spawns one named
/// [`Task::spawn_session_name`].
pub fn dispatch(
    db: &Database,
    backends: &crate::backend::BackendRegistry,
    task: &Task,
    session: Option<SessionId>,
) -> Result<Dispatched, String> {
    let session = session.or(match task.action {
        Some(AutomationAction::Send { session_id }) => Some(session_id),
        _ => None,
    });
    let dispatched = match (session, &task.action) {
        (Some(session_id), _) => send_to(db, backends, task, session_id)?,
        (
            None,
            Some(AutomationAction::Spawn {
                repo_path,
                worktree_branch,
                base_branch,
                agent,
                extra_repos,
            }),
        ) => {
            if let Some(running) = running_task_session(db, backends, task)? {
                super::send_text_with_status(db, backends, &running, &task.agent_prompt(), true)
                    .map_err(|e| format!("send: {e:#}"))?;
                Dispatched::Reused { name: running.name }
            } else {
                let name = task.spawn_session_name();
                let req = SpawnRequest {
                    name: name.clone(),
                    repo_path: repo_path.clone(),
                    worktree_branch: worktree_branch.clone(),
                    base_branch: base_branch.clone(),
                    agent: agent.clone(),
                    task_id: Some(task.id),
                    extra_repos: extra_repos.clone(),
                    ..Default::default()
                };
                spawn_and_deliver(db, backends, &name, req, &task.agent_prompt()).map_err(|e| {
                    match e {
                        SpawnDeliverError::Spawn(error) => error.to_string(),
                        SpawnDeliverError::Deliver { message, .. } => message,
                    }
                })?;
                Dispatched::Spawned { name }
            }
        }
        (None, None | Some(AutomationAction::Send { .. })) => {
            return Ok(Dispatched::Skipped(NOT_CONNECTED))
        }
        (None, Some(AutomationAction::Exec { .. })) => {
            return Ok(Dispatched::Skipped(EXEC_UNSUPPORTED))
        }
    };
    if task.status == TaskStatus::Todo {
        db.set_task_status(task.id, TaskStatus::InProgress)
            .map_err(|e| format!("set_task_status: {e}"))?;
    }
    Ok(dispatched)
}

fn send_to(
    db: &Database,
    backends: &crate::backend::BackendRegistry,
    task: &Task,
    session_id: SessionId,
) -> Result<Dispatched, String> {
    // The full row, not just the name: the row id is what the window is
    // stamped with, and so what disambiguates a shared name.
    let target = db
        .get_session_by_id(session_id)
        .map_err(|e| format!("get_session_by_id: {e}"))?
        .ok_or_else(|| format!("Target session not found: {session_id}"))?;
    if super::windows::agent_pane(backends, &target)
        .map_err(|e| e.to_string())?
        .is_none()
    {
        return Err("target session not running".into());
    }
    super::send_text_with_status(db, backends, &target, &task.agent_prompt(), true)
        .map_err(|e| format!("send: {e:#}"))?;
    Ok(Dispatched::Sent { session_id })
}

/// The running session `task` spawned before, matched by its `· #<id>` tag
/// rather than the exact name so a since-edited title (and legacy `task-<id>`
/// sessions) are found too.
fn running_task_session(
    db: &Database,
    backends: &crate::backend::BackendRegistry,
    task: &Task,
) -> Result<Option<SharedSession>, String> {
    let mut running = Vec::new();
    for session in db
        .list_active_sessions()
        .map_err(|e| format!("list_active_sessions: {e}"))?
        .into_iter()
        .filter(|s| task.matches_spawn_session(&s.name))
    {
        // A task spawns on this machine, so a same-named row on a host is
        // another machine's, and a row on a route nothing here serves is not
        // one to reuse; not knowing whether a served one runs must not launch
        // a second one.
        if crate::session::Route::is_remote_key(&session.backend_type)
            || super::windows::backend_for(backends, &session.backend_type).is_err()
        {
            continue;
        }
        if super::windows::agent_pane(backends, &session)
            .map_err(|e| e.to_string())?
            .is_some()
        {
            running.push(session);
        }
    }
    if running.len() > 1 {
        return Err(format!(
            "{} running sessions carry this task's tag, so there is no telling which one it \
             meant",
            running.len()
        ));
    }
    Ok(running.pop())
}
