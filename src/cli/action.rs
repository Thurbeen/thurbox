//! Shared helpers for the `task` and `automation` CLI surfaces, which both
//! label and serialize an `AutomationAction`; `task` also reads one from its
//! `--session`/`--repo` flags. Spawning and delivering a prompt is
//! `session_ops::deliver`'s.
//!
//! tmux/spawn helpers are reached via fully-qualified `crate::agent::…` paths (no
//! `use`) to keep the cli module free of an `agent` import — see
//! tests/architecture_rules.rs::cli_module_isolation.

use serde_json::{json, Value};

use crate::session::AutomationAction;
use crate::storage::Database;

/// Resolve a task's action from its flags: at most one of `--session` (send)
/// or `--repo` (spawn); `--add-repo`/`--add-dir` only make sense for a spawn.
/// `Ok(None)` when no action flag was given. The parsing is
/// [`crate::session_ops::action::parse`], which an automation's target shares.
pub(crate) fn resolve_action(
    target: crate::session_ops::action::Target,
    db: &Database,
) -> Result<Option<AutomationAction>, String> {
    crate::session_ops::action::parse(db, target).map_err(|e| e.to_string())
}

/// Human label for an action — `-` when there is none (a plain local todo).
pub(crate) fn action_label(action: Option<&AutomationAction>) -> String {
    match action {
        None => "-".to_string(),
        Some(AutomationAction::Send { .. }) => "send".to_string(),
        Some(AutomationAction::Spawn { .. }) => "spawn".to_string(),
        Some(AutomationAction::Exec { .. }) => "exec".to_string(),
    }
}

/// Serialize an action to its JSON object — `Value::Null` when there is none.
pub(crate) fn action_to_json(action: Option<&AutomationAction>) -> Value {
    match action {
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
            extra_repos,
        }) => json!({
            "kind": "spawn",
            "repo_path": repo_path.to_string_lossy(),
            "worktree_branch": worktree_branch,
            "base_branch": base_branch,
            "agent": agent,
            "extra_repos": serde_json::to_value(extra_repos).unwrap_or(Value::Null),
        }),
        Some(AutomationAction::Exec { command }) => json!({
            "kind": "exec",
            "command": command,
        }),
    }
}
