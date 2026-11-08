//! The task verbs the interface's `task` and `dispatch` commands and
//! `thurbox-cli task` share (#1468, #1637): creating, editing, deleting and
//! handing a task to an agent each have one implementation here, so a task
//! gets the same row, the same session and the same refusal whichever of them
//! asked. The callers keep only parsing and output.

use crate::session::SharedSession;
use crate::session::{AutomationAction, SessionId, Task, TaskStatus, SOURCE_LOCAL};
use crate::storage::tasks::NewTask;
use crate::storage::Database;

use super::deliver::{spawn_and_deliver, SpawnDeliverError};
use super::{OpError, SpawnRequest};

/// The one answer for a task id that names no active task.
pub fn not_found(id: i64) -> OpError {
    OpError::NotFound(format!("Task not found: {id}"))
}

/// The active task `id`.
pub fn get(db: &Database, id: i64) -> Result<Task, OpError> {
    db.get_task(id)
        .map_err(|e| OpError::Other(format!("get task: {e}")))?
        .ok_or_else(|| not_found(id))
}

/// Store a new task and return the row as stored.
///
/// A blank title is refused. A blank description or external field is stored
/// as none, and a blank source as [`SOURCE_LOCAL`], since that column is NOT
/// NULL.
pub fn create(db: &Database, mut new: NewTask) -> Result<Task, OpError> {
    if new.title.trim().is_empty() {
        return Err(blank_title());
    }
    new.description = blank_to_none(new.description);
    new.source = blank_to_none(Some(new.source)).unwrap_or_else(|| SOURCE_LOCAL.to_string());
    new.external_id = blank_to_none(new.external_id);
    new.external_url = blank_to_none(new.external_url);
    let id = db
        .create_task(&new)
        .map_err(|e| OpError::Other(format!("create task: {e}")))?;
    get(db, id)
}

/// The fields an [`edit`] changes; `None` leaves a field as it is.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TaskEdit {
    /// Refused when blank.
    pub title: Option<String>,
    /// Cleared when blank.
    pub description: Option<String>,
    pub status: Option<TaskStatus>,
    /// Ignored when blank, since the column is NOT NULL.
    pub source: Option<String>,
    /// Cleared when blank.
    pub external_id: Option<String>,
    /// Cleared when blank.
    pub external_url: Option<String>,
}

/// Apply `changes` to task `id` in one write, returning the row as stored.
///
/// The row is read and written under one write lock, so a field this edit
/// does not name keeps whatever another caller saved meanwhile.
pub fn edit(db: &Database, id: i64, changes: TaskEdit) -> Result<Task, OpError> {
    if changes
        .title
        .as_deref()
        .is_some_and(|t| t.trim().is_empty())
    {
        return Err(blank_title());
    }
    let found = db
        .modify_task(id, |task| {
            if let Some(title) = changes.title {
                task.title = title;
            }
            if let Some(description) = changes.description {
                task.description = blank_to_none(Some(description));
            }
            if let Some(status) = changes.status {
                task.status = status;
            }
            if let Some(source) = blank_to_none(changes.source) {
                task.source = source;
            }
            if let Some(external_id) = changes.external_id {
                task.external_id = blank_to_none(Some(external_id));
            }
            if let Some(external_url) = changes.external_url {
                task.external_url = blank_to_none(Some(external_url));
            }
        })
        .map_err(|e| OpError::Other(format!("update task: {e}")))?;
    if !found {
        return Err(not_found(id));
    }
    get(db, id)
}

/// Soft-delete task `id`; one that is not there is [`not_found`], not success.
pub fn delete(db: &Database, id: i64) -> Result<(), OpError> {
    match db.soft_delete_task(id) {
        Ok(true) => Ok(()),
        Ok(false) => Err(not_found(id)),
        Err(e) => Err(OpError::Other(format!("delete task: {e}"))),
    }
}

fn blank_title() -> OpError {
    OpError::Usage("title must not be empty".into())
}

/// Trim, mapping blank to `None`.
fn blank_to_none(s: Option<String>) -> Option<String> {
    s.and_then(|s| {
        let t = s.trim();
        (!t.is_empty()).then(|| t.to_string())
    })
}

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
                super::send::prompt(db, backends, &running, &task.agent_prompt(), Some(task.id))
                    .map_err(|e| e.to_string())?;
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
    super::send::prompt(db, backends, &target, &task.agent_prompt(), Some(task.id))
        .map_err(|e| e.to_string())?;
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

#[cfg(test)]
mod tests {
    use super::*;

    /// An edit reads the row it rewrites under the write lock, so a peer's
    /// change committed while the edit waited for that lock survives it. A
    /// read taken before the lock would hold the old title and write it back.
    #[test]
    fn an_edit_keeps_what_a_peer_committed_while_it_waited() {
        use std::sync::mpsc;
        use std::time::{Duration, Instant};

        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("thurbox.db");
        let db = Database::open(&path).unwrap();
        let id = create(&db, NewTask::local("before")).unwrap().id;

        let peer = rusqlite::Connection::open(&path).unwrap();
        peer.execute_batch("BEGIN IMMEDIATE").unwrap();

        let (started, call_started) = mpsc::channel();
        let (done, call_done) = mpsc::channel();
        let edit_path = path.clone();
        let editor = std::thread::spawn(move || {
            let db = Database::open_existing(&edit_path).unwrap();
            started.send(()).unwrap();
            let changes = TaskEdit {
                status: Some(TaskStatus::Done),
                ..Default::default()
            };
            let outcome = edit(&db, id, changes);
            let _ = done.send(());
            outcome
        });

        call_started.recv().unwrap();
        let deadline = Instant::now() + Duration::from_millis(300);
        while Instant::now() < deadline {
            assert!(
                call_done.try_recv().is_err(),
                "the edit returned while the peer held the lock, so it never blocked"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        peer.execute(
            "UPDATE tasks SET title = 'renamed by the peer' WHERE id = ?1",
            rusqlite::params![id],
        )
        .unwrap();
        peer.execute_batch("COMMIT").unwrap();

        let edited = editor.join().unwrap().expect("the edit");
        assert_eq!(
            (edited.title.as_str(), edited.status),
            ("renamed by the peer", TaskStatus::Done)
        );
    }
}
