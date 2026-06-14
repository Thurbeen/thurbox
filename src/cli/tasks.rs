//! Task CRUD subcommands for `thurbox-cli`.
//!
//! Tasks are persisted to the shared database; the TUI's right-side panel reads
//! them. `run` triggers a task's agent action headlessly (Send into a live tmux
//! window, or Spawn a fresh session named `<title> · #<id>` seeded with
//! the title).

use clap::Subcommand;
use serde_json::{json, Value};

use crate::cli::output::{self, CommandOutput};
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

pub fn run(action: Action, db: &Database) -> Result<CommandOutput, String> {
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
            let task = load(db, id)?;
            Ok(CommandOutput::new(
                task_to_json(&task),
                format!("Created task #{}: {}", task.id, task.title),
            ))
        }
        Action::List => {
            let tasks = db.list_tasks().map_err(|e| format!("list_tasks: {e}"))?;
            let json = Value::Array(tasks.iter().map(task_to_json).collect());
            Ok(CommandOutput::new(json, render_task_list(&tasks)))
        }
        Action::Show { id } => {
            let task = load(db, id)?;
            Ok(CommandOutput::new(
                task_to_json(&task),
                render_task_detail(&task),
            ))
        }
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
            let task = load(db, id)?;
            Ok(CommandOutput::new(
                task_to_json(&task),
                render_task_detail(&task),
            ))
        }
        Action::Remove { id } => match db.soft_delete_task(id) {
            Ok(true) => Ok(CommandOutput::new(
                json!({ "removed": true, "id": id }),
                format!("Removed task #{id}."),
            )),
            Ok(false) => Err(format!("Task not found: {id}")),
            Err(e) => Err(format!("soft_delete_task: {e}")),
        },
        Action::Run { id } => {
            let task = load(db, id)?;
            let json = run_task(db, &task)?;
            let human = render_task_run(&json);
            Ok(CommandOutput::new(json, human))
        }
    }
}

/// Render the task list as an aligned table (or a friendly empty line).
fn render_task_list(tasks: &[Task]) -> String {
    if tasks.is_empty() {
        return "No tasks.".to_string();
    }
    let rows: Vec<Vec<String>> = tasks
        .iter()
        .map(|t| {
            vec![
                t.id.to_string(),
                status_glyph(t.status),
                t.title.clone(),
                task_action_label(&t.action),
            ]
        })
        .collect();
    output::table(&["ID", "STATUS", "TITLE", "ACTION"], &rows)
}

/// Render a single task as a key/value block, with the markdown body trailing.
fn render_task_detail(t: &Task) -> String {
    let mut pairs: Vec<(&str, String)> = vec![
        ("id", t.id.to_string()),
        ("title", t.title.clone()),
        ("status", t.status.as_str().to_string()),
        ("action", task_action_label(&t.action)),
        ("source", t.source.clone()),
    ];
    if let Some(url) = &t.external_url {
        pairs.push(("url", url.clone()));
    }
    let mut block = output::kv(&pairs);
    if let Some(desc) = &t.description {
        if !desc.is_empty() {
            block.push_str("\n\n");
            block.push_str(desc);
        }
    }
    block
}

/// One-line human summary of a headless `task run` outcome.
fn render_task_run(v: &Value) -> String {
    if let Some(name) = v.get("spawned").and_then(Value::as_str) {
        format!("Spawned session '{name}' for the task.")
    } else if let Some(name) = v.get("reused").and_then(Value::as_str) {
        format!("Re-sent the task to existing session '{name}'.")
    } else if v.get("sent").and_then(Value::as_bool) == Some(true) {
        "Sent the task to its session.".to_string()
    } else if let Some(reason) = v.get("skipped").and_then(Value::as_str) {
        format!("Skipped: {reason}")
    } else {
        v.to_string()
    }
}

/// Short status glyph + word for the list table.
fn status_glyph(status: TaskStatus) -> String {
    match status {
        TaskStatus::Todo => "☐ todo".to_string(),
        TaskStatus::InProgress => "◐ in-progress".to_string(),
        TaskStatus::Done => "☑ done".to_string(),
    }
}

/// Human label for a task's optional agent action.
fn task_action_label(action: &Option<AutomationAction>) -> String {
    match action {
        None => "-".to_string(),
        Some(AutomationAction::Send { .. }) => "send".to_string(),
        Some(AutomationAction::Spawn { .. }) => "spawn".to_string(),
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
            let name = task.spawn_session_name();
            // Reuse an existing session window (re-trigger / restored session).
            // Match by the `· #<id>` tag rather than the exact name so a
            // since-edited title (and legacy `task-<id>` sessions) are found too.
            let existing = db
                .list_active_sessions()
                .map_err(|e| format!("list_active_sessions: {e}"))?
                .into_iter()
                .map(|s| s.name)
                .find(|n| task.matches_spawn_session(n) && crate::agent::tmux::window_exists(n));
            if let Some(name) = existing {
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
                parent_session_id: None,
                task_id: Some(task.id),
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Minimal local task for render tests.
    fn task(id: i64, title: &str, status: TaskStatus, action: Option<AutomationAction>) -> Task {
        Task {
            id,
            title: title.to_string(),
            description: None,
            status,
            action,
            source: SOURCE_LOCAL.to_string(),
            external_id: None,
            external_url: None,
            created_at: 0,
            updated_at: 0,
            deleted_at: None,
        }
    }

    #[test]
    fn status_glyph_covers_every_status() {
        assert_eq!(status_glyph(TaskStatus::Todo), "☐ todo");
        assert_eq!(status_glyph(TaskStatus::InProgress), "◐ in-progress");
        assert_eq!(status_glyph(TaskStatus::Done), "☑ done");
    }

    #[test]
    fn action_label_distinguishes_send_spawn_and_none() {
        assert_eq!(task_action_label(&None), "-");
        assert_eq!(
            task_action_label(&Some(AutomationAction::Send {
                session_id: SessionId::default(),
            })),
            "send"
        );
        assert_eq!(
            task_action_label(&Some(AutomationAction::Spawn {
                repo_path: "/x".into(),
                worktree_branch: None,
                base_branch: None,
                agent: None,
            })),
            "spawn"
        );
    }

    #[test]
    fn render_task_list_empty_and_rows() {
        assert_eq!(render_task_list(&[]), "No tasks.");
        let rendered = render_task_list(&[task(1, "Write docs", TaskStatus::Todo, None)]);
        assert!(rendered.contains("ID"));
        assert!(rendered.contains("Write docs"));
        assert!(rendered.contains("☐ todo"));
    }

    #[test]
    fn render_task_detail_appends_description() {
        let mut t = task(7, "Title", TaskStatus::InProgress, None);
        t.description = Some("notes here".to_string());
        let rendered = render_task_detail(&t);
        assert!(rendered.contains("id:"));
        assert!(rendered.contains("Title"));
        assert!(rendered.ends_with("notes here"));
    }

    #[test]
    fn render_task_run_summarizes_each_outcome() {
        assert_eq!(
            render_task_run(&json!({ "spawned": "demo", "id": 1 })),
            "Spawned session 'demo' for the task."
        );
        assert_eq!(
            render_task_run(&json!({ "reused": "demo", "id": 1 })),
            "Re-sent the task to existing session 'demo'."
        );
        assert_eq!(
            render_task_run(&json!({ "sent": true, "id": 1 })),
            "Sent the task to its session."
        );
        assert_eq!(
            render_task_run(&json!({ "skipped": "no agent", "id": 1 })),
            "Skipped: no agent"
        );
    }
}
