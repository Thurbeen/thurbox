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
    /// Optional model id to pass via `claude --model`. `None` = CLI default.
    pub model: Option<String>,
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
        model: req.model.clone(),
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
        // No pane_id is available without control mode. Leave `backend_id`
        // empty so `App::find_matching_discovered` (and its MCP/TUI callers)
        // fall back to matching by the sanitized window name — otherwise the
        // TUI would see a stored `tb-<name>` that never matches a real
        // tmux pane-id (`%N`), soft-delete this row, and respawn under a
        // fresh UUID.
        backend_id: String::new(),
        backend_type: LOCAL_TMUX_BACKEND_TYPE.to_string(),
        agent_session_id: Some(agent_session_id.clone()),
        cwd: Some(cwd.clone()),
        additional_dirs: Vec::new(),
        worktrees: worktrees.clone(),
        shell_backend_id: None,
        tombstone: false,
        tombstone_at: None,
        model: req.model.clone(),
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

/// Validate a session name. Delegates to the shared `paths::validate_safe_name`
/// so MCP and CLI reject the same inputs.
fn validate_session_name(name: &str) -> Result<(), String> {
    crate::paths::validate_safe_name(name)
}

/// Resolve a role name to its concrete permissions.
///
/// Falls back to the single configured role (if exactly one exists) or to the
/// seeded developer role otherwise — same precedence as the TUI.
fn resolve_role(
    db: &Database,
    requested: Option<&str>,
) -> Result<(String, RolePermissions), String> {
    // Explicit role: search effective roles so plugin-contributed roles can be
    // opted into. Plugin roles are NEVER picked up by the unscoped auto-fallback
    // below — that only consults the registry, so installing a plugin can't
    // silently change the default permissions of new sessions.
    if let Some(name) = requested.filter(|n| !n.is_empty()) {
        let effective_roles = db
            .list_effective_roles()
            .map_err(|e| format!("Failed to load roles: {e}"))?;
        let perms = effective_roles
            .iter()
            .find(|(r, _src)| r.name == name)
            .map(|(r, _src)| r.permissions.clone())
            .unwrap_or_else(|| default_permissions_for(name));
        return Ok((name.to_string(), perms));
    }

    let global_roles = db
        .list_global_roles()
        .map_err(|e| format!("Failed to load roles: {e}"))?;
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
///
/// Unknown names are rejected at create-time (rather than silently
/// dropped) so users discover typos before the session spawns and
/// Claude's staging step fails with a less useful error.
fn resolve_attachments(
    db: &Database,
    wanted_servers: &[String],
    wanted_skills: &[String],
) -> Result<(Vec<McpServerConfig>, Vec<SkillConfig>), String> {
    let available_servers = db
        .list_global_mcp_servers()
        .map_err(|e| format!("Failed to load MCP servers: {e}"))?;
    let missing_servers: Vec<&str> = wanted_servers
        .iter()
        .map(String::as_str)
        .filter(|n| !available_servers.iter().any(|s| s.name == *n))
        .collect();
    if !missing_servers.is_empty() {
        return Err(format!(
            "Unknown MCP server(s): {}. Known: {}",
            missing_servers.join(", "),
            available_servers
                .iter()
                .map(|s| s.name.as_str())
                .collect::<Vec<_>>()
                .join(", "),
        ));
    }
    let mcp_servers = available_servers
        .into_iter()
        .filter(|s| wanted_servers.iter().any(|n| n == &s.name))
        .collect();

    let available_skills: Vec<SkillConfig> = db
        .list_effective_skills()
        .map_err(|e| format!("Failed to load skills: {e}"))?
        .into_iter()
        .map(|(s, _source)| s)
        .collect();
    let missing_skills: Vec<&str> = wanted_skills
        .iter()
        .map(String::as_str)
        .filter(|n| !available_skills.iter().any(|s| s.name == *n))
        .collect();
    if !missing_skills.is_empty() {
        return Err(format!(
            "Unknown skill(s): {}. Known: {}",
            missing_skills.join(", "),
            if available_skills.is_empty() {
                "(none registered)".to_string()
            } else {
                available_skills
                    .iter()
                    .map(|s| s.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            },
        ));
    }
    let skills = available_skills
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
            model: None,
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
                model: None,
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

    /// Regression: plugin-contributed roles are reachable via `--role <name>`.
    #[test]
    fn resolve_role_matches_plugin_contributed_role_by_name() {
        let temp = tempfile::TempDir::new().unwrap();
        let _guard = crate::paths::TestPathGuard::new(temp.path());
        let plugins_dir = crate::paths::plugins_directory().unwrap();
        seed_plugin_with_role(
            &plugins_dir,
            "orch-bundle",
            "orchestrator",
            "Write,Edit,MultiEdit,NotebookEdit",
        );

        let db = empty_db();
        let (name, perms) = resolve_role(&db, Some("orchestrator")).unwrap();
        assert_eq!(name, "orchestrator");
        assert_eq!(
            perms.disallowed_tools,
            vec![
                "Write".to_string(),
                "Edit".to_string(),
                "MultiEdit".to_string(),
                "NotebookEdit".to_string(),
            ]
        );
    }

    /// Regression: a plugin-contributed role must NOT be silently auto-picked
    /// when no role is requested — installing a plugin can't change the
    /// default permissions of new sessions.
    #[test]
    fn resolve_role_ignores_plugin_role_when_unscoped() {
        let temp = tempfile::TempDir::new().unwrap();
        let _guard = crate::paths::TestPathGuard::new(temp.path());
        let plugins_dir = crate::paths::plugins_directory().unwrap();
        seed_plugin_with_role(&plugins_dir, "orch-bundle", "orchestrator", "Write");

        let db = empty_db();
        let (name, perms) = resolve_role(&db, None).unwrap();
        assert_eq!(name, DEFAULT_ROLE_NAME);
        assert_eq!(
            perms.permission_mode,
            default_developer_permissions().permission_mode
        );
    }

    fn seed_plugin_with_role(
        plugins_dir: &std::path::Path,
        plugin_name: &str,
        role_name: &str,
        disallowed_csv: &str,
    ) -> PathBuf {
        let p = plugins_dir.join(plugin_name);
        std::fs::create_dir_all(p.join("roles")).unwrap();
        let disallowed_toml = disallowed_csv
            .split(',')
            .map(|t| format!("\"{t}\""))
            .collect::<Vec<_>>()
            .join(", ");
        std::fs::write(
            p.join("roles/role.toml"),
            format!(
                "name = \"{role_name}\"\n\
                 description = \"test\"\n\
                 disallowed_tools = [{disallowed_toml}]\n"
            ),
        )
        .unwrap();
        std::fs::write(
            p.join("thurbox-plugin.toml"),
            format!(
                "name = \"{plugin_name}\"\nversion = \"0.1.0\"\nthurbox_plugin_api = 1\n\
                 [[contributes.roles]]\nname = \"{role_name}\"\npath = \"roles/role.toml\"\n"
            ),
        )
        .unwrap();
        p
    }
}
