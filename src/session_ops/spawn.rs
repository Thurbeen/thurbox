//! Headless session spawn — creates a local-tmux session without requiring
//! the TUI event loop.

use std::path::PathBuf;

use crate::session::{
    default_developer_permissions, McpServerConfig, RolePermissions, SessionConfig, SessionId,
    SkillConfig, DEFAULT_ROLE_NAME,
};
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
    /// Directory the claude process should `cd` into.
    pub repo_path: PathBuf,
    /// Optional branch name — when set, a git worktree is created at
    /// `repo_path/worktrees/<name>` and used as the cwd instead of
    /// `repo_path` itself.
    pub worktree_branch: Option<String>,
    /// Base branch to create the worktree from (default `main`).
    pub base_branch: Option<String>,
    /// Optional role name — falls back to the default developer role.
    pub role: Option<String>,
    /// Names of global MCP servers to attach to the session.
    pub mcp_servers: Vec<String>,
    /// Names of global skills to stage into the session.
    pub skills: Vec<String>,
    /// Optional pre-generated agent session UUID. When unset one is generated
    /// so callers can return it to the user immediately.
    pub agent_session_id: Option<String>,
}

/// Result returned on successful headless spawn.
#[derive(Debug, Clone)]
pub struct SpawnResult {
    pub session_id: SessionId,
    pub name: String,
    pub role: String,
    pub agent_session_id: String,
    pub cwd: PathBuf,
    pub worktrees: Vec<SharedWorktree>,
}

/// Spawn a new session inside `tmux -L thurbox`, persisting its state to the
/// shared SQLite database.
pub fn spawn_session_headless(db: &Database, req: SpawnRequest) -> Result<SpawnResult, String> {
    validate_session_name(&req.name)?;

    let (role_name, permissions) = resolve_role(db, req.role.as_deref())?;
    let (mcp_servers, skills) = resolve_attachments(db, &req.mcp_servers, &req.skills)?;
    let (cwd, worktrees) = resolve_cwd(&req)?;

    let agent_session_id = req
        .agent_session_id
        .clone()
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());

    let mut config = SessionConfig {
        agent_session_id: Some(agent_session_id.clone()),
        cwd: Some(cwd.clone()),
        role: role_name.clone(),
        permissions,
        mcp_servers,
        skills,
        ..SessionConfig::default()
    };
    super::inject_thurbox_env(&mut config, &agent_session_id);

    let (command, args) = super::build_claude_invocation(&config);

    crate::agent::tmux::spawn_window(
        &req.name,
        &command,
        &args,
        Some(&cwd),
        &config.permissions.env,
    )
    .map_err(|e| format!("Failed to spawn tmux window: {e}"))?;

    let session_id = SessionId::default();
    let shared = SharedSession {
        id: session_id,
        name: req.name.clone(),
        role: role_name.clone(),
        // No pane_id is available without control mode; the window-target
        // string is stored so the TUI can re-discover the live pane on its
        // next startup.
        backend_id: format!("{}{}", crate::agent::tmux::WINDOW_PREFIX, req.name),
        backend_type: LOCAL_TMUX_BACKEND_TYPE.to_string(),
        agent_session_id: Some(agent_session_id.clone()),
        cwd: Some(cwd.clone()),
        additional_dirs: Vec::new(),
        worktrees: worktrees.clone(),
        shell_backend_id: None,
        tombstone: false,
        tombstone_at: None,
    };
    db.upsert_session(&shared)
        .map_err(|e| format!("Failed to persist session: {e}"))?;

    Ok(SpawnResult {
        session_id,
        name: req.name,
        role: role_name,
        agent_session_id,
        cwd,
        worktrees,
    })
}

/// Validate a session name — same rules as the MCP `validate_safe_name`
/// guard so both code paths reject the same inputs.
fn validate_session_name(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err("Name cannot be empty".into());
    }
    if name.len() > 64 {
        return Err("Name too long (max 64 characters)".into());
    }
    if name.starts_with('.') {
        return Err("Name cannot start with '.'".into());
    }
    if name.contains('/') || name.contains('\\') || name.contains("..") {
        return Err("Name contains invalid characters".into());
    }
    Ok(())
}

/// Resolve a role name to its concrete permissions.
///
/// Falls back to the single configured role (if exactly one exists) or to the
/// seeded developer role otherwise — same precedence as the TUI.
fn resolve_role(
    db: &Database,
    requested: Option<&str>,
) -> Result<(String, RolePermissions), String> {
    let global_roles = db
        .list_global_roles()
        .map_err(|e| format!("Failed to load roles: {e}"))?;

    if let Some(name) = requested.filter(|n| !n.is_empty()) {
        let perms = global_roles
            .iter()
            .find(|r| r.name == name)
            .map(|r| r.permissions.clone())
            .unwrap_or_else(|| default_permissions_for(name));
        return Ok((name.to_string(), perms));
    }

    Ok(match global_roles.as_slice() {
        [only] => (only.name.clone(), only.permissions.clone()),
        _ => (
            DEFAULT_ROLE_NAME.to_string(),
            default_developer_permissions(),
        ),
    })
}

fn default_permissions_for(role_name: &str) -> RolePermissions {
    if role_name == DEFAULT_ROLE_NAME {
        default_developer_permissions()
    } else {
        RolePermissions::default()
    }
}

/// Filter the global MCP-server / skill lists to those the request asks for.
fn resolve_attachments(
    db: &Database,
    wanted_servers: &[String],
    wanted_skills: &[String],
) -> Result<(Vec<McpServerConfig>, Vec<SkillConfig>), String> {
    let mcp_servers = db
        .list_global_mcp_servers()
        .map_err(|e| format!("Failed to load MCP servers: {e}"))?
        .into_iter()
        .filter(|s| wanted_servers.iter().any(|n| n == &s.name))
        .collect();

    let skills = db
        .list_global_skills()
        .map_err(|e| format!("Failed to load skills: {e}"))?
        .into_iter()
        .filter(|s| wanted_skills.iter().any(|n| n == &s.name))
        .collect();

    Ok((mcp_servers, skills))
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
    use crate::session::RoleConfig;
    use crate::storage::Database;

    fn empty_db() -> Database {
        Database::open_in_memory().expect("open in-memory db")
    }

    #[test]
    fn empty_name_is_rejected() {
        let db = empty_db();
        let req = SpawnRequest {
            name: String::new(),
            repo_path: PathBuf::from("/tmp"),
            worktree_branch: None,
            base_branch: None,
            role: None,
            mcp_servers: Vec::new(),
            skills: Vec::new(),
            agent_session_id: None,
        };
        let err = spawn_session_headless(&db, req).unwrap_err();
        assert!(err.to_lowercase().contains("name"), "got {err}");
    }

    #[test]
    fn unsafe_names_are_rejected() {
        let db = empty_db();
        for bad in [".hidden", "foo/bar", "foo..bar", "foo\\bar"] {
            let req = SpawnRequest {
                name: bad.into(),
                repo_path: PathBuf::from("/tmp"),
                worktree_branch: None,
                base_branch: None,
                role: None,
                mcp_servers: Vec::new(),
                skills: Vec::new(),
                agent_session_id: None,
            };
            assert!(
                spawn_session_headless(&db, req).is_err(),
                "should reject {bad}"
            );
        }
    }

    #[test]
    fn resolve_role_falls_back_to_default_developer() {
        let db = empty_db();
        let (name, perms) = resolve_role(&db, None).unwrap();
        assert_eq!(name, DEFAULT_ROLE_NAME);
        // Default developer role has a non-default permission_mode set.
        assert_eq!(
            perms.permission_mode,
            default_developer_permissions().permission_mode
        );
    }

    #[test]
    fn resolve_role_prefers_single_existing_role() {
        let db = empty_db();
        let role = RoleConfig {
            name: "solo".into(),
            description: String::new(),
            permissions: RolePermissions::default(),
        };
        db.replace_global_roles(&[role]).unwrap();
        let (name, _) = resolve_role(&db, None).unwrap();
        assert_eq!(name, "solo");
    }

    #[test]
    fn resolve_role_uses_explicit_match() {
        let db = empty_db();
        let r1 = RoleConfig {
            name: "alpha".into(),
            description: String::new(),
            permissions: RolePermissions {
                permission_mode: Some("plan".into()),
                ..RolePermissions::default()
            },
        };
        let r2 = RoleConfig {
            name: "beta".into(),
            description: String::new(),
            permissions: RolePermissions::default(),
        };
        db.replace_global_roles(&[r1, r2]).unwrap();
        let (name, perms) = resolve_role(&db, Some("alpha")).unwrap();
        assert_eq!(name, "alpha");
        assert_eq!(perms.permission_mode.as_deref(), Some("plan"));
    }
}
