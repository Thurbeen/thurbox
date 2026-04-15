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
    List,
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
        /// Optional role name (falls back to the default developer role).
        #[arg(long)]
        role: Option<String>,
        /// If set, create a git worktree on this branch off --base-branch.
        #[arg(long)]
        worktree_branch: Option<String>,
        /// Base branch for the worktree (default: main).
        #[arg(long)]
        base_branch: Option<String>,
        /// Attach a global MCP server by name (repeatable).
        #[arg(long = "mcp-server")]
        mcp_servers: Vec<String>,
        /// Stage a global skill by name (repeatable).
        #[arg(long = "skill")]
        skills: Vec<String>,
    },
    /// Soft-delete a session.
    Delete {
        /// Session UUID.
        uuid: String,
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
        Action::List => {
            let sessions = db
                .list_active_sessions()
                .map_err(|e| format!("list_active_sessions: {e}"))?;
            Ok(Value::Array(
                sessions.iter().map(shared_session_to_json).collect(),
            ))
        }
        Action::Get { uuid } => {
            let session = resolve(db, &uuid)?;
            Ok(shared_session_to_json(&session))
        }
        Action::Create {
            name,
            repo_path,
            role,
            worktree_branch,
            base_branch,
            mcp_servers,
            skills,
        } => {
            let req = crate::session_ops::SpawnRequest {
                name,
                repo_path,
                worktree_branch,
                base_branch,
                role,
                mcp_servers,
                skills,
                agent_session_id: None,
            };
            let res = crate::session_ops::spawn_session_headless(db, req)?;
            Ok(json!({
                "id": res.session_id.to_string(),
                "name": res.name,
                "role": res.role,
                "agent_session_id": res.agent_session_id,
                "cwd": res.cwd.display().to_string(),
            }))
        }
        Action::Delete { uuid } => {
            let session = resolve(db, &uuid)?;
            db.soft_delete_session(session.id)
                .map_err(|e| format!("soft_delete_session: {e}"))?;
            Ok(json!({
                "deleted": true,
                "id": session.id.to_string(),
                "name": session.name,
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

fn resolve(db: &Database, uuid: &str) -> Result<SharedSession, String> {
    let id: SessionId = uuid
        .parse()
        .map_err(|_| format!("Invalid session UUID: {uuid}"))?;
    db.get_session_by_id(id)
        .map_err(|e| format!("get_session_by_id: {e}"))?
        .ok_or_else(|| format!("Session not found: {uuid}"))
}

fn shared_session_to_json(s: &SharedSession) -> Value {
    json!({
        "id": s.id.to_string(),
        "name": s.name,
        "role": s.role,
        "backend_type": s.backend_type,
        "agent_session_id": s.agent_session_id,
        "cwd": s.cwd.as_ref().map(|p| p.display().to_string()),
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
        let v = run(Action::List, &db).unwrap();
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
            role: "dev".into(),
            backend_id: "tb-demo".into(),
            backend_type: "local-tmux".into(),
            agent_session_id: Some("agent-1".into()),
            cwd: Some(std::path::PathBuf::from("/tmp/repo")),
            additional_dirs: Vec::new(),
            worktrees: Vec::new(),
            shell_backend_id: None,
            tombstone: false,
            tombstone_at: None,
        };
        db.upsert_session(&shared).unwrap();

        let v = run(Action::List, &db).unwrap();
        let arr = v.as_array().unwrap();
        assert_eq!(arr.len(), 1);
        let s = &arr[0];
        assert_eq!(s["id"].as_str(), Some(id.to_string().as_str()));
        assert_eq!(s["name"].as_str(), Some("demo"));
        assert_eq!(s["role"].as_str(), Some("dev"));
        assert_eq!(s["backend_type"].as_str(), Some("local-tmux"));
        assert_eq!(s["agent_session_id"].as_str(), Some("agent-1"));
        assert_eq!(s["cwd"].as_str(), Some("/tmp/repo"));
        assert!(s["worktrees"].is_array());
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
            role: "dev".into(),
            backend_id: "tb-demo".into(),
            backend_type: "local-tmux".into(),
            agent_session_id: None,
            cwd: None,
            additional_dirs: Vec::new(),
            worktrees: Vec::new(),
            shell_backend_id: None,
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
}
