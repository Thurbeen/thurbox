//! Task CRUD subcommands for `thurbox-cli`.
//!
//! Tasks are persisted to the shared database; the TUI's right-side panel reads
//! them. `run` triggers a task's agent action headlessly (Send into a live tmux
//! window, or Spawn a fresh session named `task-<id>` seeded with the title).

use clap::Subcommand;
use serde_json::{json, Value};

use crate::session::{AutomationAction, SessionId, Task, TaskStatus, SOURCE_LOCAL};
use crate::session_ops::SpawnRequest;
use crate::storage::tasks::NewTask;
use crate::storage::Database;

/// Seconds to wait after a headless spawn before delivering the task's title,
/// giving the agent CLI time to start.
const BOOT_DELAY_SECS: u64 = 3;

#[derive(Subcommand, Debug)]
pub enum Action {
    /// Create a task. With no `--session`/`--repo` it is a plain local todo.
    Create {
        /// The task text (also seeds the agent action when triggered).
        #[arg(long)]
        title: String,
        /// Optional markdown description.
        #[arg(long)]
        description: Option<String>,
        /// Initial status: `todo` | `in_progress` | `done` (default `todo`).
        #[arg(long)]
        status: Option<String>,
        /// Send action: connect to an existing session by UUID.
        #[arg(long)]
        session: Option<String>,
        /// Spawn action: repository path to run a new session in.
        #[arg(long)]
        repo: Option<String>,
        /// Spawn action: optional worktree branch (created if missing).
        #[arg(long)]
        worktree: Option<String>,
        /// Spawn action: base branch for a new worktree (default `main`).
        #[arg(long)]
        base: Option<String>,
        /// Agent name (spawn action; default registry agent).
        #[arg(long)]
        agent: Option<String>,
    },
    /// List all active tasks.
    List,
    /// Show one task by id.
    Show {
        /// Task id.
        id: i64,
    },
    /// Edit a task.
    Edit {
        /// Task id.
        id: i64,
        #[arg(long)]
        title: Option<String>,
        /// New markdown description (`--description ""` clears it).
        #[arg(long)]
        description: Option<String>,
        #[arg(long)]
        status: Option<String>,
    },
    /// Remove a task (soft delete).
    Remove {
        /// Task id.
        id: i64,
    },
    /// Trigger a task's agent action now (headless). Local tasks are skipped.
    Run {
        /// Task id.
        id: i64,
    },
}

pub fn run(action: Action, db: &Database) -> Result<Value, String> {
    match action {
        Action::Create {
            title,
            description,
            status,
            session,
            repo,
            worktree,
            base,
            agent,
        } => {
            if title.is_empty() {
                return Err("title must not be empty".into());
            }
            let status = parse_status(status.as_deref())?;
            let action = resolve_action(session, repo, worktree, base, agent, db)?;
            let new = NewTask {
                title,
                description: normalize_description(description),
                status,
                action,
                source: SOURCE_LOCAL.to_string(),
                external_id: None,
                external_url: None,
            };
            let id = db
                .create_task(&new)
                .map_err(|e| format!("create_task: {e}"))?;
            Ok(task_to_json(&load(db, id)?))
        }
        Action::List => {
            let tasks = db.list_tasks().map_err(|e| format!("list_tasks: {e}"))?;
            Ok(Value::Array(tasks.iter().map(task_to_json).collect()))
        }
        Action::Show { id } => Ok(task_to_json(&load(db, id)?)),
        Action::Edit {
            id,
            title,
            description,
            status,
        } => {
            let mut task = load(db, id)?;
            if let Some(t) = title {
                task.title = t;
            }
            // Passing --description always sets it (trimmed-empty → cleared).
            if let Some(d) = description {
                task.description = normalize_description(Some(d));
            }
            if let Some(s) = status {
                task.status = parse_status(Some(&s))?;
            }
            db.update_task(&task)
                .map_err(|e| format!("update_task: {e}"))?;
            Ok(task_to_json(&load(db, id)?))
        }
        Action::Remove { id } => match db.soft_delete_task(id) {
            Ok(true) => Ok(json!({ "removed": true, "id": id })),
            Ok(false) => Err(format!("Task not found: {id}")),
            Err(e) => Err(format!("soft_delete_task: {e}")),
        },
        Action::Run { id } => {
            let task = load(db, id)?;
            run_task(db, &task)
        }
    }
}

/// Execute a task's action without a TUI, returning a JSON outcome.
///
/// tmux/spawn helpers are reached via fully-qualified paths (no `use
/// crate::agent`) to keep the cli module free of an `agent` import — see
/// tests/architecture_rules.rs::cli_module_isolation.
fn run_task(db: &Database, task: &Task) -> Result<Value, String> {
    // Seed the agent with full task context (id + title + description + how to
    // read more / mark done), not just the bare title — shared with the TUI
    // dispatch path via `Task::agent_prompt`.
    let prompt = task.agent_prompt();
    match &task.action {
        None => Ok(json!({ "skipped": "task is not connected to an agent", "id": task.id })),
        Some(AutomationAction::Send { session_id }) => {
            let name = db
                .get_session_name(*session_id)
                .map_err(|e| format!("get_session_name: {e}"))?
                .ok_or_else(|| format!("Target session not found: {session_id}"))?;
            if !crate::agent::tmux::window_exists(&name) {
                return Err("target session not running".into());
            }
            crate::agent::tmux::send_prompt_now(&name, &prompt)
                .map_err(|e| format!("send_prompt_now: {e}"))?;
            mark_in_progress(db, task)?;
            Ok(json!({ "sent": true, "id": task.id, "session_id": session_id.to_string() }))
        }
        Some(AutomationAction::Spawn {
            repo_path,
            worktree_branch,
            base_branch,
            agent,
        }) => {
            let name = format!("task-{}", task.id);
            // Reuse an existing session window (re-trigger / restored session).
            if crate::agent::tmux::window_exists(&name) {
                crate::agent::tmux::send_prompt_now(&name, &prompt)
                    .map_err(|e| format!("send_prompt_now: {e}"))?;
                mark_in_progress(db, task)?;
                return Ok(json!({ "reused": name, "id": task.id }));
            }
            let req = SpawnRequest {
                name: name.clone(),
                repo_path: repo_path.clone(),
                worktree_branch: worktree_branch.clone(),
                base_branch: base_branch.clone(),
                agent: agent.clone(),
                agent_session_id: None,
                host: None,
            };
            crate::session_ops::spawn_session_headless(db, req)?;
            crate::agent::tmux::send_prompt_after_delay(&name, &prompt, BOOT_DELAY_SECS)
                .map_err(|e| format!("spawned {name} but prompt delivery failed: {e}"))?;
            mark_in_progress(db, task)?;
            Ok(json!({ "spawned": name, "id": task.id }))
        }
    }
}

/// Advance a freshly-triggered task `Todo → InProgress` (no-op for other
/// states), mirroring the TUI's `App::advance_task_to_in_progress` so the
/// headless `task run` path keeps status in sync too.
fn mark_in_progress(db: &Database, task: &Task) -> Result<(), String> {
    if task.status == TaskStatus::Todo {
        db.set_task_status(task.id, TaskStatus::InProgress)
            .map_err(|e| format!("set_task_status: {e}"))?;
    }
    Ok(())
}

fn load(db: &Database, id: i64) -> Result<Task, String> {
    db.get_task(id)
        .map_err(|e| format!("get_task: {e}"))?
        .ok_or_else(|| format!("Task not found: {id}"))
}

/// Trim a CLI-supplied description, mapping blank input to `None` (cleared).
fn normalize_description(d: Option<String>) -> Option<String> {
    d.and_then(|s| {
        let t = s.trim();
        (!t.is_empty()).then(|| t.to_string())
    })
}

fn parse_status(s: Option<&str>) -> Result<TaskStatus, String> {
    match s {
        None => Ok(TaskStatus::Todo),
        Some(v) => match v {
            "todo" => Ok(TaskStatus::Todo),
            "in_progress" => Ok(TaskStatus::InProgress),
            "done" => Ok(TaskStatus::Done),
            other => Err(format!(
                "invalid status `{other}` (use todo|in_progress|done)"
            )),
        },
    }
}

/// Resolve the optional action from the send/spawn flags. Neither flag → a plain
/// local todo (`None`), unlike automations where an action is required.
fn resolve_action(
    session: Option<String>,
    repo: Option<String>,
    worktree: Option<String>,
    base: Option<String>,
    agent: Option<String>,
    db: &Database,
) -> Result<Option<AutomationAction>, String> {
    match (session, repo) {
        (Some(_), Some(_)) => {
            Err("specify either --session (send) or --repo (spawn), not both".into())
        }
        (None, None) => Ok(None),
        (Some(s), None) => {
            let session_id: SessionId = s
                .parse()
                .map_err(|_| format!("invalid session UUID: {s}"))?;
            db.get_session_by_id(session_id)
                .map_err(|e| format!("get_session_by_id: {e}"))?
                .ok_or_else(|| format!("Session not found: {s}"))?;
            Ok(Some(AutomationAction::Send { session_id }))
        }
        (None, Some(r)) => Ok(Some(AutomationAction::Spawn {
            repo_path: r.into(),
            worktree_branch: worktree,
            base_branch: base,
            agent,
        })),
    }
}

fn task_to_json(t: &Task) -> Value {
    let action = match &t.action {
        None => Value::Null,
        Some(AutomationAction::Send { session_id }) => json!({
            "kind": "send",
            "session_id": session_id.to_string(),
        }),
        Some(AutomationAction::Spawn {
            repo_path,
            worktree_branch,
            base_branch,
            agent,
        }) => json!({
            "kind": "spawn",
            "repo_path": repo_path.to_string_lossy(),
            "worktree_branch": worktree_branch,
            "base_branch": base_branch,
            "agent": agent,
        }),
    };
    json!({
        "id": t.id,
        "title": t.title,
        "description": t.description,
        "status": t.status.as_str(),
        "action": action,
        "source": t.source,
        "external_id": t.external_id,
        "external_url": t.external_url,
        "created_at": t.created_at,
        "updated_at": t.updated_at,
    })
}
