//! Session CRUD and orchestration subcommands.

use std::path::PathBuf;

use clap::Subcommand;
use serde_json::{json, Value};

use crate::session::SessionId;
use crate::storage::Database;
use crate::sync::SharedSession;

#[derive(Subcommand, Debug)]
pub enum Action {
    /// List all active sessions.
    List {
        /// Only list children of this parent session UUID.
        #[arg(long)]
        parent: Option<String>,
    },
    /// Get a session by UUID.
    Get {
        /// Session UUID.
        uuid: String,
    },
    /// Create a new session (runs synchronously — tmux window live on return).
    Create {
        /// Session name (1-64 chars, no slashes or leading '.').
        #[arg(long)]
        name: String,
        /// Absolute path to the repository or working directory.
        #[arg(long)]
        repo_path: PathBuf,
        /// Coding agent to launch (e.g. "claude", "codex"). Falls back to the
        /// default agent from `agents.toml`.
        #[arg(long)]
        agent: Option<String>,
        /// If set, create a git worktree on this branch off --base-branch.
        #[arg(long)]
        worktree_branch: Option<String>,
        /// Base branch for the worktree (default: main).
        #[arg(long)]
        base_branch: Option<String>,
        /// Remote host to run the session on (name from `hosts.toml`). The
        /// worktree and tmux window are created on that host over SSH.
        #[arg(long)]
        host: Option<String>,
        /// Parent session UUID (lead/worker relationship for orchestration).
        /// Must reference an existing active session.
        #[arg(long)]
        parent: Option<String>,
    },
    /// Soft-delete a session.
    ///
    /// By default only the DB row is soft-deleted (the TUI cleans up the
    /// tmux window and worktree on next sync). Pass `--force` to also
    /// kill the tmux window, remove worktrees, and cancel pending
    /// scheduled commands — useful for headless cleanup when the TUI
    /// isn't running.
    Delete {
        /// Session UUID.
        uuid: String,
        /// Also kill the tmux window, remove worktrees, and cancel
        /// pending scheduled commands for this session.
        #[arg(long)]
        force: bool,
    },
    /// Restore a soft-deleted session.
    Restore {
        /// Session UUID.
        uuid: String,
    },
    /// Restart a session in-place (kills the window, re-spawns with --resume).
    Restart {
        /// Session UUID.
        uuid: String,
    },
    /// Type text into a session's terminal, followed by Enter.
    Send {
        /// Session UUID.
        uuid: String,
        /// Text to send.
        text: String,
    },
    /// Capture rendered pane contents as text.
    Capture {
        /// Session UUID.
        uuid: String,
        /// Scrollback lines to include (default 200, max 10000).
        #[arg(long, default_value_t = 200)]
        lines: u32,
    },
}

pub fn run(action: Action, db: &Database) -> Result<Value, String> {
    match action {
        Action::List { parent } => {
            let parent_id = parent.as_deref().map(parse_session_id).transpose()?;
            let sessions = db
                .list_active_sessions()
                .map_err(|e| format!("list_active_sessions: {e}"))?;
            Ok(Value::Array(
                sessions
                    .iter()
                    .filter(|s| parent_id.is_none() || s.parent_session_id == parent_id)
                    .map(shared_session_to_json)
                    .collect(),
            ))
        }
        Action::Get { uuid } => {
            let session = resolve(db, &uuid)?;
            Ok(shared_session_to_json(&session))
        }
        Action::Create {
            name,
            repo_path,
            agent,
            worktree_branch,
            base_branch,
            host,
            parent,
        } => {
            let parent_session_id = parent.as_deref().map(parse_session_id).transpose()?;
            let req = crate::session_ops::SpawnRequest {
                name,
                repo_path,
                worktree_branch,
                base_branch,
                agent,
                agent_session_id: None,
                host,
                parent_session_id,
            };
            let res = crate::session_ops::spawn_session_headless(db, req)?;
            Ok(json!({
                "id": res.session_id.to_string(),
                "name": res.name,
                "agent": res.agent,
                "agent_session_id": res.agent_session_id,
                "cwd": res.cwd.display().to_string(),
                "parent_session_id": res.parent_session_id.map(|id| id.to_string()),
            }))
        }
        Action::Delete { uuid, force } => {
            let session = resolve(db, &uuid)?;
            let report = crate::session_ops::delete_session_headless(db, session.id, force)?;
            Ok(json!({
                "deleted": true,
                "id": session.id.to_string(),
                "name": session.name,
                "forced": force,
                "killed_window": report.killed_window,
                "removed_worktrees": report.removed_worktrees,
                "worktree_errors": report.worktree_errors,
                "disabled_automations": report.disabled_automations,
            }))
        }
        Action::Restore { uuid } => {
            let id: SessionId = uuid
                .parse()
                .map_err(|_| format!("Invalid session UUID: {uuid}"))?;
            let deleted = db
                .get_deleted_session_by_id(id)
                .map_err(|e| format!("get_deleted_session_by_id: {e}"))?
                .ok_or_else(|| format!("Deleted session not found: {uuid}"))?;
            db.restore_session(deleted.id)
                .map_err(|e| format!("restore_session: {e}"))?;
            Ok(json!({
                "restored": true,
                "id": deleted.id.to_string(),
                "name": deleted.name,
            }))
        }
        Action::Restart { uuid } => {
            let session = resolve(db, &uuid)?;
            crate::session_ops::restart_session_headless(db, session.id)?;
            Ok(json!({
                "restarted": true,
                "session_id": session.id.to_string(),
                "session_name": session.name,
            }))
        }
        Action::Send { uuid, text } => {
            let session = resolve(db, &uuid)?;
            if text.is_empty() {
                return Err("text must not be empty".into());
            }
            crate::agent::tmux::send_prompt_now(&session.name, &text)
                .map_err(|e| format!("send_prompt_now: {e}"))?;
            Ok(json!({
                "sent": true,
                "session_id": session.id.to_string(),
                "session_name": session.name,
            }))
        }
        Action::Capture { uuid, lines } => {
            let session = resolve(db, &uuid)?;
            let output = crate::agent::tmux::capture_pane_text(&session.name, lines)
                .map_err(|e| format!("capture_pane_text: {e}"))?;
            Ok(json!({
                "session_id": session.id.to_string(),
                "session_name": session.name,
                "lines": lines,
                "output": output,
            }))
        }
    }
}

fn parse_session_id(uuid: &str) -> Result<SessionId, String> {
    uuid.parse()
        .map_err(|_| format!("Invalid session UUID: {uuid}"))
}

fn resolve(db: &Database, uuid: &str) -> Result<SharedSession, String> {
    let id = parse_session_id(uuid)?;
    db.get_session_by_id(id)
        .map_err(|e| format!("get_session_by_id: {e}"))?
        .ok_or_else(|| format!("Session not found: {uuid}"))
}

fn shared_session_to_json(s: &SharedSession) -> Value {
    json!({
        "id": s.id.to_string(),
        "name": s.name,
        "agent": s.agent,
        "backend_type": s.backend_type,
        "agent_session_id": s.agent_session_id,
        "cwd": s.cwd.as_ref().map(|p| p.display().to_string()),
        "parent_session_id": s.parent_session_id.map(|id| id.to_string()),
        "display_order": s.display_order,
        "worktrees": s.worktrees.iter().map(|w| json!({
            "repo_path": w.repo_path.display().to_string(),
            "worktree_path": w.worktree_path.display().to_string(),
            "branch": w.branch,
        })).collect::<Vec<_>>(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Database {
        Database::open_in_memory().unwrap()
    }

    #[test]
    fn list_empty_returns_array() {
        let db = db();
        let v = run(Action::List { parent: None }, &db).unwrap();
        assert!(v.is_array(), "got {v}");
        assert_eq!(v.as_array().unwrap().len(), 0);
    }

    #[test]
    fn list_returns_session_with_expected_shape() {
        let db = db();
        let id = SessionId::default();
        let shared = SharedSession {
            id,
            name: "demo".into(),
            agent: "dev".into(),
            backend_id: "tb-demo".into(),
            backend_type: "local-tmux".into(),
            agent_session_id: Some("agent-1".into()),
            cwd: Some(std::path::PathBuf::from("/tmp/repo")),
            additional_dirs: Vec::new(),
            worktrees: Vec::new(),
            shell_backend_id: None,
            parent_session_id: None,
            display_order: None,
            tombstone: false,
            tombstone_at: None,
        };
        db.upsert_session(&shared).unwrap();

        let v = run(Action::List { parent: None }, &db).unwrap();
        let arr = v.as_array().unwrap();
        assert_eq!(arr.len(), 1);
        let s = &arr[0];
        assert_eq!(s["id"].as_str(), Some(id.to_string().as_str()));
        assert_eq!(s["name"].as_str(), Some("demo"));
        assert_eq!(s["agent"].as_str(), Some("dev"));
        assert_eq!(s["backend_type"].as_str(), Some("local-tmux"));
        assert_eq!(s["agent_session_id"].as_str(), Some("agent-1"));
        assert_eq!(s["cwd"].as_str(), Some("/tmp/repo"));
        assert!(s["parent_session_id"].is_null());
        assert!(s["display_order"].is_null());
        assert!(s["worktrees"].is_array());
    }

    #[test]
    fn list_emits_parent_session_id_and_filters_by_parent() {
        let db = db();
        let parent_id = SessionId::default();
        let parent = SharedSession {
            id: parent_id,
            name: "lead".into(),
            agent: "dev".into(),
            backend_id: String::new(),
            backend_type: "local-tmux".into(),
            agent_session_id: None,
            cwd: None,
            additional_dirs: Vec::new(),
            worktrees: Vec::new(),
            shell_backend_id: None,
            parent_session_id: None,
            display_order: None,
            tombstone: false,
            tombstone_at: None,
        };
        db.upsert_session(&parent).unwrap();
        let mut child = parent.clone();
        child.id = SessionId::default();
        child.name = "worker".into();
        child.parent_session_id = Some(parent_id);
        db.upsert_session(&child).unwrap();

        // Unfiltered list carries the field on both rows.
        let v = run(Action::List { parent: None }, &db).unwrap();
        let arr = v.as_array().unwrap();
        assert_eq!(arr.len(), 2);
        let worker = arr.iter().find(|s| s["name"] == "worker").unwrap();
        assert_eq!(
            worker["parent_session_id"].as_str(),
            Some(parent_id.to_string().as_str())
        );

        // --parent filters to direct children only.
        let v = run(
            Action::List {
                parent: Some(parent_id.to_string()),
            },
            &db,
        )
        .unwrap();
        let arr = v.as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["name"].as_str(), Some("worker"));

        // Malformed --parent uuid errors.
        let err = run(
            Action::List {
                parent: Some("not-a-uuid".into()),
            },
            &db,
        )
        .unwrap_err();
        assert!(err.contains("Invalid session UUID"), "got {err}");
    }

    #[test]
    fn get_unknown_uuid_errors() {
        let db = db();
        let err = run(
            Action::Get {
                uuid: "not-a-uuid".into(),
            },
            &db,
        )
        .unwrap_err();
        assert!(err.contains("Invalid session UUID"), "got {err}");
    }

    #[test]
    fn send_rejects_empty_text() {
        let db = db();
        let id = SessionId::default();
        let shared = SharedSession {
            id,
            name: "demo".into(),
            agent: "dev".into(),
            backend_id: "tb-demo".into(),
            backend_type: "local-tmux".into(),
            agent_session_id: None,
            cwd: None,
            additional_dirs: Vec::new(),
            worktrees: Vec::new(),
            shell_backend_id: None,
            parent_session_id: None,
            display_order: None,
            tombstone: false,
            tombstone_at: None,
        };
        db.upsert_session(&shared).unwrap();
        let err = run(
            Action::Send {
                uuid: id.to_string(),
                text: String::new(),
            },
            &db,
        )
        .unwrap_err();
        assert!(err.contains("text"), "got {err}");
    }

    #[test]
    fn soft_delete_leaves_session_recoverable() {
        // Bug #3: `session delete` without --force only soft-deletes the
        // DB row, leaving pending scheduled commands and the metadata
        // restorable.
        let db = db();
        let id = SessionId::default();
        let shared = SharedSession {
            id,
            name: "Foo Bar".into(), // exercise bug #1 sanitization path too
            agent: "dev".into(),
            backend_id: String::new(),
            backend_type: "local-tmux".into(),
            agent_session_id: None,
            cwd: None,
            additional_dirs: Vec::new(),
            worktrees: Vec::new(),
            shell_backend_id: None,
            parent_session_id: None,
            display_order: None,
            tombstone: false,
            tombstone_at: None,
        };
        db.upsert_session(&shared).unwrap();
        let auto = db
            .create_automation(&crate::storage::automations::NewAutomation {
                name: "noop".into(),
                enabled: true,
                schedule: crate::session::AutomationSchedule::Once { at: u64::MAX },
                timezone: None,
                action: crate::session::AutomationAction::Send { session_id: id },
                prompt: "noop".into(),
                next_run_at: Some(u64::MAX),
            })
            .unwrap();

        let v = run(
            Action::Delete {
                uuid: id.to_string(),
                force: false,
            },
            &db,
        )
        .unwrap();
        assert_eq!(v["deleted"], true);
        assert_eq!(v["forced"], false);
        assert_eq!(v["disabled_automations"], 0);

        // Row is soft-deleted but recoverable; the automation is untouched.
        assert!(db.get_session_by_id(id).unwrap().is_none());
        assert!(db.get_automation(auto).unwrap().unwrap().enabled);
        let restored = run(
            Action::Restore {
                uuid: id.to_string(),
            },
            &db,
        )
        .unwrap();
        assert_eq!(restored["restored"], true);
        assert!(db.get_session_by_id(id).unwrap().is_some());
    }
}
