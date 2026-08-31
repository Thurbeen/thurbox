//! Task CRUD subcommands for `thurbox-cli`.
//!
//! Tasks are persisted to the shared database; the TUI's right-side panel reads
//! them. `run` triggers a task's agent action headlessly (Send into a live tmux
//! window, or Spawn a fresh session named `<title> · #<id>` seeded with
//! the title).

use clap::Subcommand;
use serde_json::{json, Value};

use crate::cli::action::{self, SpawnDeliverError};
use crate::cli::output::{self, CommandOutput};
use crate::session::{AutomationAction, Task, TaskStatus, SOURCE_LOCAL};
use crate::session_ops::SpawnRequest;
use crate::storage::tasks::NewTask;
use crate::storage::Database;

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
        /// Spawn action: additional repo to span (repeatable). `PATH` or
        /// `PATH@BASE` — each gets its own isolated worktree on `--worktree`
        /// off `BASE` (default `--base`). Makes a multi-repo task session.
        #[arg(long = "add-repo")]
        add_repo: Vec<String>,
        /// Spawn action: additional directory to span (repeatable), attached
        /// as-is (no worktree / branch). Makes a multi-repo task session.
        #[arg(long = "add-dir")]
        add_dir: Vec<String>,
        /// Origin tracker (default `local`); set by sync extensions to e.g.
        /// Any tag your importer chooses; `local` for a task made here.
        #[arg(long)]
        source: Option<String>,
        /// Identifier in the external tracker (the dedup/upsert key).
        #[arg(long = "external-id")]
        external_id: Option<String>,
        /// Link to the item in the external tracker.
        #[arg(long = "external-url")]
        external_url: Option<String>,
    },
    /// List all active tasks.
    List,
    /// Show one task by id.
    #[command(alias = "get")]
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
        /// Origin tracker tag; unchanged when omitted.
        #[arg(long)]
        source: Option<String>,
        /// External tracker id; `--external-id ""` clears it.
        #[arg(long = "external-id")]
        external_id: Option<String>,
        /// External tracker link; `--external-url ""` clears it.
        #[arg(long = "external-url")]
        external_url: Option<String>,
    },
    /// Remove a task (soft delete).
    #[command(alias = "delete")]
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
            add_repo,
            add_dir,
            source,
            external_id,
            external_url,
        } => {
            if title.trim().is_empty() {
                return Err("title must not be empty".into());
            }
            let extra_repos = super::parse_extra_repos(&add_repo, &add_dir);
            let new = NewTask {
                title,
                description: blank_to_none(description),
                status: parse_status(status.as_deref())?,
                action: resolve_action(session, repo, worktree, base, agent, extra_repos, db)?,
                source: source
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .unwrap_or_else(|| SOURCE_LOCAL.to_string()),
                external_id: blank_to_none(external_id),
                external_url: blank_to_none(external_url),
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
            Ok(CommandOutput::new(json, render_task_list(&tasks))
                .list("tasks", &["id", "title", "status", "source"])
                .empty("0 tasks — the todo list is empty")
                .help([
                    "thurbox-cli task show <id>   the full description",
                    "thurbox-cli task run <id>   hand it to an agent",
                    "thurbox-cli task create --title <title>   add one",
                ]))
        }
        Action::Show { id } => {
            let task = load(db, id)?;
            Ok(
                CommandOutput::new(task_to_json(&task), render_task_detail(&task))
                    .truncate(2000)
                    .help([
                        "thurbox-cli task edit <id> --status done   close it",
                        "thurbox-cli task run <id>   hand it to an agent",
                    ]),
            )
        }
        Action::Edit {
            id,
            title,
            description,
            status,
            source,
            external_id,
            external_url,
        } => {
            let mut task = load(db, id)?;
            apply_edits(
                &mut task,
                title,
                description,
                status,
                source,
                external_id,
                external_url,
            )?;
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

/// Fold the `edit` flags that were passed into `task`.
///
/// Every field follows the same rule: passing the flag sets it. `--description`
/// and the two `external_*` clear to NULL when passed empty; a blank `--source`
/// is ignored, since the column is NOT NULL.
#[allow(clippy::too_many_arguments)]
fn apply_edits(
    task: &mut Task,
    title: Option<String>,
    description: Option<String>,
    status: Option<String>,
    source: Option<String>,
    external_id: Option<String>,
    external_url: Option<String>,
) -> Result<(), String> {
    if let Some(t) = title {
        if t.trim().is_empty() {
            return Err("title must not be empty".into());
        }
        task.title = t;
    }
    if let Some(d) = description {
        task.description = blank_to_none(Some(d));
    }
    if let Some(s) = status {
        task.status = parse_status(Some(&s))?;
    }
    if let Some(s) = source
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
    {
        task.source = s;
    }
    if let Some(e) = external_id {
        task.external_id = blank_to_none(Some(e));
    }
    if let Some(u) = external_url {
        task.external_url = blank_to_none(Some(u));
    }
    Ok(())
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
                action::action_label(t.action.as_ref()),
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
        ("action", action::action_label(t.action.as_ref())),
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
            // The full row, not just the name: the persisted pane id is what
            // disambiguates when another session shares the name.
            let target = db
                .get_session_by_id(*session_id)
                .map_err(|e| format!("get_session_by_id: {e}"))?
                .ok_or_else(|| format!("Target session not found: {session_id}"))?;
            if !crate::agent::tmux::window_exists(&target.name, &target.backend_id) {
                return Err("target session not running".into());
            }
            crate::agent::tmux::send_prompt_now(&target.name, &target.backend_id, &prompt)
                .map_err(|e| format!("send_prompt_now: {e}"))?;
            mark_in_progress(db, task)?;
            Ok(json!({ "sent": true, "id": task.id, "session_id": session_id.to_string() }))
        }
        Some(AutomationAction::Spawn {
            repo_path,
            worktree_branch,
            base_branch,
            agent,
            extra_repos,
        }) => {
            let name = task.spawn_session_name();
            // Reuse an existing session window (re-trigger / restored session).
            // Match by the `· #<id>` tag rather than the exact name so a
            // since-edited title (and legacy `task-<id>` sessions) are found too.
            let existing = db
                .list_active_sessions()
                .map_err(|e| format!("list_active_sessions: {e}"))?
                .into_iter()
                .find(|s| {
                    task.matches_spawn_session(&s.name)
                        && crate::agent::tmux::window_exists(&s.name, &s.backend_id)
                });
            if let Some(session) = existing {
                crate::agent::tmux::send_prompt_now(&session.name, &session.backend_id, &prompt)
                    .map_err(|e| format!("send_prompt_now: {e}"))?;
                mark_in_progress(db, task)?;
                return Ok(json!({ "reused": session.name, "id": task.id }));
            }
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
            action::spawn_and_deliver(db, &name, req, &prompt).map_err(|e| match e {
                SpawnDeliverError::Spawn(msg) | SpawnDeliverError::Deliver { message: msg, .. } => {
                    msg
                }
            })?;
            mark_in_progress(db, task)?;
            Ok(json!({ "spawned": name, "id": task.id }))
        }
        // Exec is an automation-only action; a task never carries one.
        Some(AutomationAction::Exec { .. }) => Ok(json!({
            "skipped": "exec action is not supported for tasks",
            "id": task.id,
        })),
    }
}

/// Advance a freshly-triggered task `Todo → InProgress` (no-op for other
/// states), mirroring what the kernel's task-run command does
/// (`kernel::command`, `set_task_status`) so the headless `task run` path keeps
/// status in sync too.
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

/// Trim an optional CLI string, mapping blank input to `None` (cleared). Shared
/// by the blank-able task fields: `--description`, `--external-id`, and
/// `--external-url` (passing `""` clears each).
fn blank_to_none(s: Option<String>) -> Option<String> {
    s.and_then(|s| {
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
#[allow(clippy::too_many_arguments)]
/// Tasks carry no `--command`: an Exec action is automation-only, so the flag
/// is pinned to `None` before the shared resolution runs.
fn resolve_action(
    session: Option<String>,
    repo: Option<String>,
    worktree: Option<String>,
    base: Option<String>,
    agent: Option<String>,
    extra_repos: Vec<crate::session::ExtraRepo>,
    db: &Database,
) -> Result<Option<AutomationAction>, String> {
    action::resolve_action(
        action::ActionFlags {
            session,
            repo,
            worktree,
            base,
            agent,
            extra_repos,
            command: None,
        },
        db,
    )
}

fn task_to_json(t: &Task) -> Value {
    let action = action::action_to_json(t.action.as_ref());
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
    use crate::session::SessionId;

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
        assert_eq!(action::action_label(None), "-");
        assert_eq!(
            action::action_label(Some(&AutomationAction::Send {
                session_id: SessionId::default(),
            })),
            "send"
        );
        assert_eq!(
            action::action_label(Some(&AutomationAction::Spawn {
                repo_path: "/x".into(),
                worktree_branch: None,
                base_branch: None,
                agent: None,
                extra_repos: Vec::new(),
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
    fn create_and_edit_reject_blank_title() {
        let db = Database::open_in_memory().unwrap();
        let err = run(
            Action::Create {
                title: "   ".into(),
                description: None,
                status: None,
                session: None,
                repo: None,
                worktree: None,
                base: None,
                agent: None,
                add_repo: Vec::new(),
                add_dir: Vec::new(),
                source: None,
                external_id: None,
                external_url: None,
            },
            &db,
        )
        .unwrap_err();
        assert!(err.contains("title"), "got {err}");

        // A real task cannot have its title cleared to blank via edit.
        let created = run(
            Action::Create {
                title: "Real".into(),
                description: None,
                status: None,
                session: None,
                repo: None,
                worktree: None,
                base: None,
                agent: None,
                add_repo: Vec::new(),
                add_dir: Vec::new(),
                source: None,
                external_id: None,
                external_url: None,
            },
            &db,
        )
        .unwrap();
        let id = created["id"].as_i64().unwrap();
        let err = run(
            Action::Edit {
                id,
                title: Some("  ".into()),
                description: None,
                status: None,
                source: None,
                external_id: None,
                external_url: None,
            },
            &db,
        )
        .unwrap_err();
        assert!(err.contains("title"), "got {err}");
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

    /// A local-todo create with all the spawn knobs defaulted, parameterized on
    /// just the external-sync fields under test.
    fn create_action(
        title: &str,
        source: Option<&str>,
        external_id: Option<&str>,
        external_url: Option<&str>,
    ) -> Action {
        Action::Create {
            title: title.into(),
            description: None,
            status: None,
            session: None,
            repo: None,
            worktree: None,
            base: None,
            agent: None,
            add_repo: Vec::new(),
            add_dir: Vec::new(),
            source: source.map(Into::into),
            external_id: external_id.map(Into::into),
            external_url: external_url.map(Into::into),
        }
    }

    #[test]
    fn create_with_external_fields_persists() {
        let db = Database::open_in_memory().unwrap();
        let out = run(
            create_action(
                "Imported from Linear",
                Some("linear"),
                Some("ENG-7"),
                Some("https://linear.app/x/issue/ENG-7"),
            ),
            &db,
        )
        .unwrap();
        assert_eq!(out["source"], "linear");
        assert_eq!(out["external_id"], "ENG-7");
        assert_eq!(out["external_url"], "https://linear.app/x/issue/ENG-7");

        // The (source, external_id) pair is now resolvable for dedup.
        let id = out["id"].as_i64().unwrap();
        assert_eq!(
            db.get_task_by_external_id("linear", "ENG-7")
                .unwrap()
                .map(|t| t.id),
            Some(id)
        );
    }

    #[test]
    fn create_without_source_defaults_to_local() {
        let db = Database::open_in_memory().unwrap();
        let out = run(create_action("plain", None, None, None), &db).unwrap();
        assert_eq!(out["source"], SOURCE_LOCAL);
        assert!(out["external_id"].is_null());
        assert!(out["external_url"].is_null());
    }

    #[test]
    fn create_blank_source_falls_back_to_local() {
        let db = Database::open_in_memory().unwrap();
        let out = run(create_action("plain", Some("   "), None, None), &db).unwrap();
        assert_eq!(out["source"], SOURCE_LOCAL);
    }

    #[test]
    fn edit_can_change_source_but_ignores_blank() {
        let db = Database::open_in_memory().unwrap();
        let id = run(create_action("t", Some("github"), Some("1"), None), &db).unwrap()["id"]
            .as_i64()
            .unwrap();
        let edit = |source: Option<&str>| {
            run(
                Action::Edit {
                    id,
                    title: None,
                    description: None,
                    status: None,
                    source: source.map(Into::into),
                    external_id: None,
                    external_url: None,
                },
                &db,
            )
            .unwrap()
        };
        assert_eq!(edit(Some("gitlab"))["source"], "gitlab");
        // A blank value is ignored (source is NOT NULL) — left unchanged.
        assert_eq!(edit(Some("  "))["source"], "gitlab");
    }

    #[test]
    fn edit_updates_and_clears_external_fields() {
        let db = Database::open_in_memory().unwrap();
        let created = run(
            create_action(
                "issue",
                Some("github"),
                Some("42"),
                Some("https://example.com/issues/42"),
            ),
            &db,
        )
        .unwrap();
        let id = created["id"].as_i64().unwrap();

        let edited = run(
            Action::Edit {
                id,
                title: None,
                description: None,
                status: Some("done".into()),
                source: None,
                external_id: None,
                external_url: Some("https://example.com/issues/42#closed".into()),
            },
            &db,
        )
        .unwrap();
        assert_eq!(edited["status"], "done");
        assert_eq!(edited["source"], "github"); // untouched (flag omitted)
        assert_eq!(edited["external_id"], "42");
        assert_eq!(
            edited["external_url"],
            "https://example.com/issues/42#closed"
        );

        let cleared = run(
            Action::Edit {
                id,
                title: None,
                description: None,
                status: None,
                source: None,
                external_id: None,
                external_url: Some(String::new()),
            },
            &db,
        )
        .unwrap();
        assert!(cleared["external_url"].is_null());
    }
}
