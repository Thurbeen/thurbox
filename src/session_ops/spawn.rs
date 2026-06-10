//! Headless session spawn — creates a local-tmux session without requiring
//! the TUI event loop.

use std::path::PathBuf;

use crate::session::{HostDef, SessionConfig, SessionId, DEFAULT_AGENT_NAME};
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
    /// Optional remote host name (from `hosts.toml`). When set, the session is
    /// created on that host over SSH (worktree + tmux window live remotely).
    pub host: Option<String>,
    /// Optional parent session (lead/worker relationship for orchestration).
    /// Must reference an existing active session.
    pub parent_session_id: Option<SessionId>,
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
    pub parent_session_id: Option<SessionId>,
}

/// Spawn a new session inside `tmux -L thurbox`, persisting its state to the
/// shared SQLite database.
pub fn spawn_session_headless(db: &Database, req: SpawnRequest) -> Result<SpawnResult, String> {
    validate_session_name(&req.name)?;
    validate_parent_session(db, req.parent_session_id)?;

    let agent_name = resolve_agent_name(req.agent.as_deref());

    // Resolve the optional remote host. `backend_type` is `local-tmux` or
    // `ssh:<host>`; `host` is the matching HostDef for remote git/tmux ops.
    let (backend_type, host) = resolve_host(req.host.as_deref())?;
    let (cwd, worktrees) = resolve_cwd(&req, host.as_ref())?;

    let agent_session_id = req
        .agent_session_id
        .clone()
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());

    let mut config = SessionConfig {
        agent_session_id: Some(agent_session_id.clone()),
        cwd: Some(cwd.clone()),
        agent: agent_name.clone(),
        backend: (backend_type != LOCAL_TMUX_BACKEND_TYPE).then(|| backend_type.clone()),
        ..SessionConfig::default()
    };
    super::inject_thurbox_env(&mut config, &agent_session_id);

    let (command, args) = super::build_agent_invocation(&config);

    // Remote spawns drive the SSH backend's control mode to learn the real pane
    // id; local spawns leave `backend_id` empty for the TUI to resolve by name.
    let backend_id = match host.as_ref() {
        Some(h) => crate::agent::tmux::spawn_window_remote(
            h,
            &req.name,
            &command,
            &args,
            Some(&cwd),
            &config.env,
        )
        .map_err(|e| format!("Failed to spawn remote tmux window: {e:#}"))?,
        None => {
            crate::agent::tmux::spawn_window(&req.name, &command, &args, Some(&cwd), &config.env)
                .map_err(|e| format!("Failed to spawn tmux window: {e}"))?;
            String::new()
        }
    };

    let session_id = SessionId::default();
    let shared = SharedSession {
        id: session_id,
        name: req.name.clone(),
        agent: agent_name.clone(),
        backend_id: backend_id.clone(),
        backend_type,
        agent_session_id: Some(agent_session_id.clone()),
        cwd: Some(cwd.clone()),
        additional_dirs: Vec::new(),
        worktrees: worktrees.clone(),
        shell_backend_id: None,
        parent_session_id: req.parent_session_id,
        tombstone: false,
        tombstone_at: None,
    };
    // The tmux window is already live. If the DB upsert fails now, no row exists
    // for the TUI to adopt and the window would be orphaned — untrackable and
    // unkillable from the UI. Best-effort tear it down before surfacing the
    // error so we don't leak a window.
    if let Err(e) = db.upsert_session(&shared) {
        tracing::error!(
            "spawn race: DB upsert failed after the tmux window for '{}' spawned; \
             tearing down the orphaned window: {e}",
            req.name
        );
        let cleanup = match host.as_ref() {
            Some(h) => crate::agent::tmux::kill_pane_remote(h, &backend_id),
            None => crate::agent::tmux::kill_window(&req.name),
        };
        if let Err(kill_err) = cleanup {
            tracing::error!(
                "failed to tear down orphaned window for '{}': {kill_err}",
                req.name
            );
        }
        return Err(format!("Failed to persist session: {e}"));
    }

    Ok(SpawnResult {
        session_id,
        name: req.name,
        agent: agent_name,
        agent_session_id,
        cwd,
        worktrees,
        parent_session_id: req.parent_session_id,
    })
}

/// Validate a session name. Delegates to the shared `paths::validate_safe_name`.
fn validate_session_name(name: &str) -> Result<(), String> {
    crate::paths::validate_safe_name(name)
}

/// Validate that the requested parent session, if any, exists and is active.
/// Runs before any side effects (worktree creation, tmux spawn).
fn validate_parent_session(db: &Database, parent: Option<SessionId>) -> Result<(), String> {
    let Some(parent) = parent else {
        return Ok(());
    };
    match db.get_session_by_id(parent) {
        Ok(Some(_)) => Ok(()),
        Ok(None) => Err(format!("Parent session not found: {parent}")),
        Err(e) => Err(format!("get_session_by_id: {e}")),
    }
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
fn resolve_cwd(
    req: &SpawnRequest,
    host: Option<&HostDef>,
) -> Result<(PathBuf, Vec<SharedWorktree>), String> {
    let Some(branch) = req.worktree_branch.as_deref() else {
        return Ok((req.repo_path.clone(), Vec::new()));
    };
    let base_branch = req.base_branch.as_deref().unwrap_or(DEFAULT_BASE_BRANCH);
    let path = crate::git::create_worktree_on(host, &req.repo_path, branch, base_branch)
        .map_err(|e| format!("Failed to create worktree {branch} off {base_branch}: {e}"))?;
    let wt = SharedWorktree {
        repo_path: req.repo_path.clone(),
        worktree_path: path.clone(),
        branch: branch.to_string(),
    };
    Ok((path, vec![wt]))
}

/// Resolve `--host` to `(backend_type, host)`.
///
/// `None`/empty → the local backend. A named host must exist in `hosts.toml`,
/// otherwise an error is returned listing the available hosts.
fn resolve_host(host_name: Option<&str>) -> Result<(String, Option<HostDef>), String> {
    let Some(name) = host_name.filter(|n| !n.is_empty()) else {
        return Ok((LOCAL_TMUX_BACKEND_TYPE.to_string(), None));
    };
    let registry = crate::agent::host_config::load_or_seed();
    match registry.get(name) {
        Some(h) => Ok((h.backend_name(), Some(h.clone()))),
        None => {
            let available = registry.names().join(", ");
            Err(format!(
                "Unknown host '{name}'. Configure it in hosts.toml. Available: [{available}]"
            ))
        }
    }
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
            host: None,
            parent_session_id: None,
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
    fn unknown_parent_session_is_rejected_before_spawn() {
        let db = empty_db();
        let mut r = req("worker");
        r.parent_session_id = Some(SessionId::default());
        let err = spawn_session_headless(&db, r).unwrap_err();
        assert!(err.contains("Parent session not found"), "got {err}");
    }

    #[test]
    fn validate_parent_session_accepts_existing_and_none() {
        let db = empty_db();
        assert!(validate_parent_session(&db, None).is_ok());

        let parent = crate::sync::SharedSession {
            id: SessionId::default(),
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
            tombstone: false,
            tombstone_at: None,
        };
        db.upsert_session(&parent).unwrap();
        assert!(validate_parent_session(&db, Some(parent.id)).is_ok());
    }

    #[test]
    fn resolve_agent_name_uses_explicit() {
        assert_eq!(resolve_agent_name(Some("codex")), "codex");
        assert_eq!(resolve_agent_name(Some("")), DEFAULT_AGENT_NAME);
    }

    #[test]
    fn resolve_host_none_is_local() {
        let (backend_type, host) = resolve_host(None).unwrap();
        assert_eq!(backend_type, LOCAL_TMUX_BACKEND_TYPE);
        assert!(host.is_none());
        // Empty string is treated the same as None.
        let (backend_type, host) = resolve_host(Some("")).unwrap();
        assert_eq!(backend_type, LOCAL_TMUX_BACKEND_TYPE);
        assert!(host.is_none());
    }

    #[test]
    fn resolve_host_unknown_errors_with_guidance() {
        let temp = tempfile::TempDir::new().unwrap();
        let _guard = crate::paths::TestPathGuard::new(temp.path());
        let err = resolve_host(Some("nope")).unwrap_err();
        assert!(err.contains("Unknown host 'nope'"), "got: {err}");
        assert!(err.contains("hosts.toml"), "got: {err}");
    }

    #[test]
    fn resolve_host_reads_configured_host() {
        let temp = tempfile::TempDir::new().unwrap();
        let _guard = crate::paths::TestPathGuard::new(temp.path());
        let path = crate::agent::host_config::hosts_config_path().unwrap();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            "[[hosts]]\nname = \"devbox\"\ndestination = \"me@devbox\"\n",
        )
        .unwrap();

        let (backend_type, host) = resolve_host(Some("devbox")).unwrap();
        assert_eq!(backend_type, "ssh:devbox");
        assert_eq!(host.unwrap().destination, "me@devbox");
    }
}
