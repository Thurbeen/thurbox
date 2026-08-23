//! Shared helpers for the `task` and `automation` CLI surfaces, which both build
//! an `AutomationAction` from `--session`/`--repo`/`--command` flags, label and
//! serialize it, and spawn-or-reuse a headless session to deliver a prompt. Each
//! command keeps only its type-specific wrappers (different return shapes and
//! reuse-matching) on top of these primitives.
//!
//! tmux/spawn helpers are reached via fully-qualified `crate::agent::…` paths (no
//! `use`) to keep the cli module free of an `agent` import — see
//! tests/architecture_rules.rs::cli_module_isolation.

use serde_json::{json, Value};

use crate::session::{AutomationAction, SessionId};
use crate::session_ops::SpawnRequest;
use crate::storage::Database;

/// Seconds to wait after a headless spawn before delivering the prompt, giving
/// the agent CLI time to start.
pub(crate) const BOOT_DELAY_SECS: u64 = 3;

/// The send/spawn/exec flags both action-bearing commands accept, gathered so
/// their resolution lives once. `task` passes no `--command` (Exec is
/// automation-only) and accepts an action-less record; `automation` requires
/// one — both differences sit at the call sites, not here.
pub(crate) struct ActionFlags {
    pub session: Option<String>,
    pub repo: Option<String>,
    pub worktree: Option<String>,
    pub base: Option<String>,
    pub agent: Option<String>,
    pub extra_repos: Vec<crate::session::ExtraRepo>,
    pub command: Option<String>,
}

/// Resolve an action from the flags: at most one of `--session` (send),
/// `--repo` (spawn) or `--command` (exec); `--add-repo`/`--add-dir` only make
/// sense for a spawn. `Ok(None)` when no action flag was given.
pub(crate) fn resolve_action(
    flags: ActionFlags,
    db: &Database,
) -> Result<Option<AutomationAction>, String> {
    let ActionFlags {
        session,
        repo,
        worktree,
        base,
        agent,
        extra_repos,
        command,
    } = flags;
    if let Some(cmd) = command {
        if session.is_some() || repo.is_some() {
            return Err("specify only one of --session, --repo, or --command".into());
        }
        if !extra_repos.is_empty() {
            return Err("--add-repo/--add-dir require --repo (a spawn action)".into());
        }
        if cmd.trim().is_empty() {
            return Err("--command must not be empty".into());
        }
        return Ok(Some(AutomationAction::Exec { command: cmd }));
    }
    match (session, repo) {
        (Some(_), Some(_)) => {
            Err("specify either --session (send) or --repo (spawn), not both".into())
        }
        (None, None) => {
            if !extra_repos.is_empty() {
                return Err("--add-repo/--add-dir require --repo (a spawn action)".into());
            }
            Ok(None)
        }
        (Some(_), None) if !extra_repos.is_empty() => {
            Err("--add-repo/--add-dir apply to --repo (spawn), not --session (send)".into())
        }
        (Some(s), None) => Ok(Some(AutomationAction::Send {
            session_id: resolve_send_target(db, &s)?,
        })),
        (None, Some(r)) => Ok(Some(AutomationAction::Spawn {
            repo_path: r.into(),
            worktree_branch: worktree,
            base_branch: base,
            agent,
            extra_repos,
        })),
    }
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

/// Parse + validate a `--session` UUID for a Send action: the session must
/// exist. Shared by the task/automation `resolve_action` wrappers.
pub(crate) fn resolve_send_target(db: &Database, s: &str) -> Result<SessionId, String> {
    let session_id: SessionId = s
        .parse()
        .map_err(|_| format!("invalid session UUID: {s}"))?;
    db.get_session_by_id(session_id)
        .map_err(|e| format!("get_session_by_id: {e}"))?
        .ok_or_else(|| format!("Session not found: {s}"))?;
    Ok(session_id)
}

/// Why a [`spawn_and_deliver`] attempt didn't fully succeed. The split lets each
/// caller preserve its own behavior: a spawn failure leaves no session, while a
/// delivery failure still created one (callers that record a session id keep it).
pub(crate) enum SpawnDeliverError {
    /// The spawn itself failed; no session was created.
    Spawn(String),
    /// The session spawned but the delayed prompt delivery failed.
    Deliver {
        session_id: SessionId,
        message: String,
    },
}

/// Spawn a fresh headless session for `req` and deliver `prompt` once the agent
/// boots (after [`BOOT_DELAY_SECS`]). Returns the new session id on success.
pub(crate) fn spawn_and_deliver(
    db: &Database,
    name: &str,
    req: SpawnRequest,
    prompt: &str,
) -> Result<SessionId, SpawnDeliverError> {
    let session_id = crate::session_ops::spawn_session_headless(db, req)
        .map_err(SpawnDeliverError::Spawn)?
        .session_id;
    crate::agent::tmux::send_prompt_after_delay(name, prompt, BOOT_DELAY_SECS).map_err(|e| {
        SpawnDeliverError::Deliver {
            session_id,
            message: format!("spawned {name} but prompt delivery failed: {e}"),
        }
    })?;
    Ok(session_id)
}
