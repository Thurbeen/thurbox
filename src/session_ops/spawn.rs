//! Headless session spawn — creates a local-tmux session without requiring
//! the TUI event loop.

use std::path::PathBuf;

use crate::session::{
    default_developer_permissions, merge_role_permissions, McpServerConfig, ProfileConfig,
    RolePermissions, SessionConfig, SessionId, SkillConfig, DEFAULT_ROLE_NAME,
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
    /// Optional profile name — resolved via the `profiles` table and
    /// applied as a preset for roles/MCP/skills. Any explicit `role`,
    /// `mcp_servers`, or `skills` set on this request overrides the
    /// profile's contribution for that field.
    pub profile: Option<String>,
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

    let profile = resolve_profile(db, req.profile.as_deref())?;
    let (role_name, permissions) = resolve_role_with_profile(db, req.role.as_deref(), &profile)?;
    let (mcp_server_names, skill_names) = resolve_attachment_names(&req, &profile);
    let (mcp_servers, skills) = resolve_attachments(db, &mcp_server_names, &skill_names)?;
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

/// Load the profile named by the caller, if any.
///
/// Empty string is treated as "no profile". Unknown profile names are a
/// hard error so typos surface immediately.
fn resolve_profile(
    db: &Database,
    requested: Option<&str>,
) -> Result<Option<ProfileConfig>, String> {
    let Some(name) = requested.filter(|n| !n.is_empty()) else {
        return Ok(None);
    };
    match db
        .get_global_profile(name)
        .map_err(|e| format!("Failed to load profile '{name}': {e}"))?
    {
        Some(p) => Ok(Some(p)),
        None => Err(format!("Unknown profile: {name}")),
    }
}

/// Resolve the display role name and merged permissions.
///
/// Precedence for the role list:
/// 1. Explicit `requested` role wins if set.
/// 2. Otherwise, the profile's `roles` list (if non-empty).
/// 3. Otherwise, the single configured role (if exactly one exists) or
///    the seeded developer role.
///
/// The returned display name is:
/// - `"profile:<name>"` when a profile was applied (lets the TUI
///   distinguish preset-driven sessions),
/// - the single role name otherwise, or `DEFAULT_ROLE_NAME` on fallback.
fn resolve_role_with_profile(
    db: &Database,
    requested: Option<&str>,
    profile: &Option<ProfileConfig>,
) -> Result<(String, RolePermissions), String> {
    let explicit = requested.filter(|n| !n.is_empty());
    let profile_roles_opted_in = profile.as_ref().is_some_and(|p| !p.roles.is_empty());
    let role_names = resolve_role_names(db, explicit, profile)?;

    // Named roles (explicit or profile-driven) resolve against the effective
    // set so plugin-contributed roles can be opted into. The unscoped
    // auto-fallback only consults the registry, preserving the guarantee
    // that installing a plugin can't silently change the default permissions
    // of new sessions.
    let uses_effective = explicit.is_some() || profile_roles_opted_in;

    let parts: Vec<RolePermissions> = if role_names.is_empty() {
        Vec::new()
    } else if uses_effective {
        let effective = db
            .list_effective_roles()
            .map_err(|e| format!("Failed to load roles: {e}"))?;
        role_names
            .iter()
            .map(|name| {
                effective
                    .iter()
                    .find(|(r, _)| r.name == *name)
                    .map(|(r, _)| r.permissions.clone())
                    .unwrap_or_else(|| default_permissions_for(name))
            })
            .collect()
    } else {
        let global = db
            .list_global_roles()
            .map_err(|e| format!("Failed to load roles: {e}"))?;
        role_names
            .iter()
            .map(|name| {
                global
                    .iter()
                    .find(|r| r.name == *name)
                    .map(|r| r.permissions.clone())
                    .unwrap_or_else(|| default_permissions_for(name))
            })
            .collect()
    };

    // Single-role case is a pass-through: preserves insertion order of
    // tool lists and avoids the BTreeSet sort performed by the merge
    // function. Empty falls back to the hardcoded default developer perms.
    let merged = match parts.as_slice() {
        [] => default_developer_permissions(),
        [only] => only.clone(),
        _ => merge_role_permissions(&parts),
    };

    let display = match (profile.as_ref(), role_names.as_slice()) {
        (Some(p), _) => format!("profile:{}", p.name),
        (None, [only]) => only.clone(),
        (None, []) => DEFAULT_ROLE_NAME.to_string(),
        (None, many) => many.join("+"),
    };

    Ok((display, merged))
}

/// Determine which role names contribute to the merged permissions.
///
/// Explicit caller-supplied role always wins (even over a profile). If
/// no explicit role and no profile is given, falls back to the TUI's
/// previous behaviour: single existing role, or seeded developer.
fn resolve_role_names(
    db: &Database,
    explicit: Option<&str>,
    profile: &Option<ProfileConfig>,
) -> Result<Vec<String>, String> {
    if let Some(name) = explicit {
        return Ok(vec![name.to_string()]);
    }
    if let Some(p) = profile.as_ref() {
        if !p.roles.is_empty() {
            return Ok(p.roles.clone());
        }
    }
    let global_roles = db
        .list_global_roles()
        .map_err(|e| format!("Failed to load roles: {e}"))?;
    Ok(match global_roles.as_slice() {
        [only] => vec![only.name.clone()],
        _ => vec![DEFAULT_ROLE_NAME.to_string()],
    })
}

/// Caller-supplied MCP/skill lists override the profile's contributions.
///
/// Empty caller list + non-empty profile list → take from profile. Empty
/// both → empty.
fn resolve_attachment_names(
    req: &SpawnRequest,
    profile: &Option<ProfileConfig>,
) -> (Vec<String>, Vec<String>) {
    let mcp = if !req.mcp_servers.is_empty() {
        req.mcp_servers.clone()
    } else {
        profile
            .as_ref()
            .map(|p| p.mcp_servers.clone())
            .unwrap_or_default()
    };
    let skills = if !req.skills.is_empty() {
        req.skills.clone()
    } else {
        profile
            .as_ref()
            .map(|p| p.skills.clone())
            .unwrap_or_default()
    };
    (mcp, skills)
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
            profile: None,
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
                profile: None,
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
        let (name, perms) = resolve_role_with_profile(&db, None, &None).unwrap();
        assert_eq!(name, DEFAULT_ROLE_NAME);
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
        let (name, _) = resolve_role_with_profile(&db, None, &None).unwrap();
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
        let (name, perms) = resolve_role_with_profile(&db, Some("alpha"), &None).unwrap();
        assert_eq!(name, "alpha");
        assert_eq!(perms.permission_mode.as_deref(), Some("plan"));
    }

    #[test]
    fn resolve_profile_unknown_name_errors() {
        let db = empty_db();
        let err = resolve_profile(&db, Some("ghost")).unwrap_err();
        assert!(err.contains("ghost"), "got {err}");
    }

    #[test]
    fn resolve_profile_empty_string_is_none() {
        let db = empty_db();
        assert!(resolve_profile(&db, Some("")).unwrap().is_none());
        assert!(resolve_profile(&db, None).unwrap().is_none());
    }

    #[test]
    fn profile_roles_merge_when_no_explicit_role() {
        let db = empty_db();
        let reader = RoleConfig {
            name: "reader".into(),
            description: String::new(),
            permissions: RolePermissions {
                allowed_tools: vec!["Read".to_string()],
                permission_mode: Some("plan".into()),
                ..RolePermissions::default()
            },
        };
        let writer = RoleConfig {
            name: "writer".into(),
            description: String::new(),
            permissions: RolePermissions {
                allowed_tools: vec!["Edit".to_string()],
                permission_mode: Some("acceptEdits".into()),
                ..RolePermissions::default()
            },
        };
        db.replace_global_roles(&[reader, writer]).unwrap();
        let profile = ProfileConfig {
            name: "combo".into(),
            description: String::new(),
            roles: vec!["reader".into(), "writer".into()],
            mcp_servers: Vec::new(),
            skills: Vec::new(),
        };
        let (name, perms) = resolve_role_with_profile(&db, None, &Some(profile)).unwrap();
        assert_eq!(name, "profile:combo");
        assert!(perms.allowed_tools.contains(&"Read".to_string()));
        assert!(perms.allowed_tools.contains(&"Edit".to_string()));
        // Most permissive mode wins.
        assert_eq!(perms.permission_mode.as_deref(), Some("acceptEdits"));
    }

    #[test]
    fn explicit_role_overrides_profile_roles() {
        let db = empty_db();
        let reader = RoleConfig {
            name: "reader".into(),
            description: String::new(),
            permissions: RolePermissions {
                allowed_tools: vec!["Read".into()],
                ..RolePermissions::default()
            },
        };
        let writer = RoleConfig {
            name: "writer".into(),
            description: String::new(),
            permissions: RolePermissions {
                allowed_tools: vec!["Edit".into()],
                ..RolePermissions::default()
            },
        };
        db.replace_global_roles(&[reader, writer]).unwrap();
        let profile = ProfileConfig {
            name: "combo".into(),
            description: String::new(),
            roles: vec!["reader".into(), "writer".into()],
            mcp_servers: Vec::new(),
            skills: Vec::new(),
        };
        let (name, perms) = resolve_role_with_profile(&db, Some("reader"), &Some(profile)).unwrap();
        // Display name uses the profile prefix because a profile was applied,
        // but only the explicit role contributes permissions.
        assert_eq!(name, "profile:combo");
        assert_eq!(perms.allowed_tools, vec!["Read".to_string()]);
    }

    #[test]
    fn caller_mcp_and_skills_override_profile() {
        let profile = ProfileConfig {
            name: "p".into(),
            description: String::new(),
            roles: Vec::new(),
            mcp_servers: vec!["from-profile".into()],
            skills: vec!["from-profile-skill".into()],
        };
        let req = SpawnRequest {
            name: "t".into(),
            repo_path: PathBuf::from("/tmp"),
            worktree_branch: None,
            base_branch: None,
            role: None,
            profile: Some("p".into()),
            mcp_servers: vec!["caller".into()],
            skills: vec!["caller-skill".into()],
            agent_session_id: None,
        };
        let (mcp, skills) = resolve_attachment_names(&req, &Some(profile));
        assert_eq!(mcp, vec!["caller".to_string()]);
        assert_eq!(skills, vec!["caller-skill".to_string()]);
    }

    #[test]
    fn profile_mcp_and_skills_apply_when_caller_empty() {
        let profile = ProfileConfig {
            name: "p".into(),
            description: String::new(),
            roles: Vec::new(),
            mcp_servers: vec!["from-profile".into()],
            skills: vec!["from-profile-skill".into()],
        };
        let req = SpawnRequest {
            name: "t".into(),
            repo_path: PathBuf::from("/tmp"),
            worktree_branch: None,
            base_branch: None,
            role: None,
            profile: Some("p".into()),
            mcp_servers: Vec::new(),
            skills: Vec::new(),
            agent_session_id: None,
        };
        let (mcp, skills) = resolve_attachment_names(&req, &Some(profile));
        assert_eq!(mcp, vec!["from-profile".to_string()]);
        assert_eq!(skills, vec!["from-profile-skill".to_string()]);
    }
}
