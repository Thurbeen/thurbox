//! Headless session spawn — creates a local-tmux session without requiring
//! the TUI event loop.

use std::path::PathBuf;

use crate::session::{SessionConfig, SessionId, DEFAULT_AGENT_NAME};
use crate::storage::Database;
use crate::sync::{SharedSession, SharedWorktree};

/// Default base branch for `--worktree-branch` when none is given.
const DEFAULT_BASE_BRANCH: &str = "main";

/// Backend identifier for the local-tmux backend (matches `LocalTmuxBackend`).
const LOCAL_TMUX_BACKEND_TYPE: &str = "local-tmux";

/// Request to create a new headless session.
#[derive(Debug, Clone)]
pub struct SpawnRequest {
    /// Session name (used for the tmux window `tb-<name>`).
    pub name: String,
    /// Directory the agent process should `cd` into.
    pub repo_path: PathBuf,
    /// Optional branch name — when set, a git worktree is created at
    /// `repo_path/worktrees/<name>` and used as the cwd instead of
    /// `repo_path` itself.
    pub worktree_branch: Option<String>,
    /// Base branch to create the worktree from (default `main`).
    pub base_branch: Option<String>,
    /// Optional agent name — falls back to the registry default agent.
    pub agent: Option<String>,
    /// Optional pre-generated agent session UUID. When unset one is generated
    /// so callers can return it to the user immediately.
    pub agent_session_id: Option<String>,
}

/// Result returned on successful headless spawn.
#[derive(Debug, Clone)]
pub struct SpawnResult {
    pub session_id: SessionId,
    pub name: String,
    pub agent: String,
    pub agent_session_id: String,
    pub cwd: PathBuf,
    pub worktrees: Vec<SharedWorktree>,
}

/// Spawn a new session inside `tmux -L thurbox`, persisting its state to the
/// shared SQLite database.
pub fn spawn_session_headless(_db: &Database, req: SpawnRequest) -> Result<SpawnResult, String> {
    validate_session_name(&req.name)?;

    let agent_name = resolve_agent_name(req.agent.as_deref());
    let (cwd, worktrees) = resolve_cwd(&req)?;

    let agent_session_id = req
        .agent_session_id
        .clone()
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());

    let mut config = SessionConfig {
        agent_session_id: Some(agent_session_id.clone()),
        cwd: Some(cwd.clone()),
        agent: agent_name.clone(),
        ..SessionConfig::default()
    };
    super::inject_thurbox_env(&mut config, &agent_session_id);

    let (command, args) = super::build_agent_invocation(&config);

    crate::agent::tmux::spawn_window(&req.name, &command, &args, Some(&cwd), &config.env)
        .map_err(|e| format!("Failed to spawn tmux window: {e}"))?;

    let session_id = SessionId::default();
    let shared = SharedSession {
        id: session_id,
        name: req.name.clone(),
        agent: agent_name.clone(),
        // No pane_id is available without control mode. Leave `backend_id`
        // empty so `App::find_matching_discovered` falls back to matching by
        // the sanitized window name.
        backend_id: String::new(),
        backend_type: LOCAL_TMUX_BACKEND_TYPE.to_string(),
        agent_session_id: Some(agent_session_id.clone()),
        cwd: Some(cwd.clone()),
        additional_dirs: Vec::new(),
        worktrees: worktrees.clone(),
        shell_backend_id: None,
        tombstone: false,
        tombstone_at: None,
    };
    _db.upsert_session(&shared)
        .map_err(|e| format!("Failed to persist session: {e}"))?;

    Ok(SpawnResult {
        session_id,
        name: req.name,
        agent: agent_name,
        agent_session_id,
        cwd,
        worktrees,
    })
}

/// Validate a session name. Delegates to the shared `paths::validate_safe_name`.
fn validate_session_name(name: &str) -> Result<(), String> {
    crate::paths::validate_safe_name(name)
}

/// Resolve the agent name: explicit request wins, otherwise the registry's
/// default agent, otherwise the built-in default.
fn resolve_agent_name(requested: Option<&str>) -> String {
    if let Some(name) = requested.filter(|n| !n.is_empty()) {
        return name.to_string();
    }
    let registry = crate::agent::agent_config::load_or_seed();
    let name = registry.default_name();
    if name.is_empty() {
        DEFAULT_AGENT_NAME.to_string()
    } else {
        name
    }
}

/// Resolve the working directory and worktree records.
///
/// Returns the bare repo path when no worktree branch is given; otherwise
/// creates the worktree and returns its path plus a single
/// [`SharedWorktree`] entry.
fn resolve_cwd(req: &SpawnRequest) -> Result<(PathBuf, Vec<SharedWorktree>), String> {
    let Some(branch) = req.worktree_branch.as_deref() else {
        return Ok((req.repo_path.clone(), Vec::new()));
    };
    let base_branch = req.base_branch.as_deref().unwrap_or(DEFAULT_BASE_BRANCH);
    let path = crate::git::create_worktree(&req.repo_path, branch, base_branch)
        .map_err(|e| format!("Failed to create worktree {branch} off {base_branch}: {e}"))?;
    let wt = SharedWorktree {
        repo_path: req.repo_path.clone(),
        worktree_path: path.clone(),
        branch: branch.to_string(),
    };
    Ok((path, vec![wt]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::Database;

    fn empty_db() -> Database {
        Database::open_in_memory().expect("open in-memory db")
    }

    fn req(name: &str) -> SpawnRequest {
        SpawnRequest {
            name: name.into(),
            repo_path: PathBuf::from("/tmp"),
            worktree_branch: None,
            base_branch: None,
            agent: None,
            agent_session_id: None,
        }
    }

    #[test]
    fn empty_name_is_rejected() {
        let db = empty_db();
        let err = spawn_session_headless(&db, req("")).unwrap_err();
        assert!(err.to_lowercase().contains("name"), "got {err}");
    }

    #[test]
    fn unsafe_names_are_rejected() {
        let db = empty_db();
        for bad in [".hidden", "foo/bar", "foo..bar", "foo\\bar"] {
            assert!(
                spawn_session_headless(&db, req(bad)).is_err(),
                "should reject {bad}"
            );
        }
    }

    #[test]
    fn resolve_agent_name_uses_explicit() {
        assert_eq!(resolve_agent_name(Some("codex")), "codex");
        assert_eq!(resolve_agent_name(Some("")), DEFAULT_AGENT_NAME);
    }
}
