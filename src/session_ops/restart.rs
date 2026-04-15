//! Headless session restart — tears down the tmux window and re-launches
//! the claude CLI with `--resume <agent_session_id>`.

use crate::session::{
    default_developer_permissions, RolePermissions, SessionConfig, SessionId, DEFAULT_ROLE_NAME,
};
use crate::storage::Database;

/// Restart an existing session in-place — kills its tmux window and
/// re-spawns the claude CLI with `--resume <agent_session_id>`.
///
/// MCP servers + skills are intentionally not re-attached on restart — the
/// TUI has the same behaviour (see `App::handle_restart_command`). Delete
/// and recreate the session to change attached configuration.
pub fn restart_session_headless(db: &Database, session_id: SessionId) -> Result<(), String> {
    let session = db
        .get_session_by_id(session_id)
        .map_err(|e| format!("Failed to load session: {e}"))?
        .ok_or_else(|| format!("Session not found: {session_id}"))?;

    let agent_session_id = session
        .agent_session_id
        .clone()
        .ok_or_else(|| format!("Cannot restart session {session_id} without agent_session_id"))?;

    let permissions = lookup_role_permissions(db, &session.role)?;

    // If the Claude conversation transcript doesn't exist yet (session was
    // never interacted with), `--resume` would fail with "No conversation
    // found". Fall back to `--session-id` to start fresh with the same id.
    let config_dir_override = permissions
        .env
        .get("CLAUDE_CONFIG_DIR")
        .map(std::path::PathBuf::from);
    let resume_session_id = if crate::paths::claude_transcript_exists(
        &agent_session_id,
        config_dir_override.as_deref(),
    ) {
        Some(agent_session_id.clone())
    } else {
        None
    };

    let mut config = SessionConfig {
        resume_session_id,
        agent_session_id: Some(agent_session_id.clone()),
        cwd: session.cwd.clone(),
        additional_dirs: session.additional_dirs.clone(),
        role: session.role.clone(),
        permissions,
        mcp_servers: Vec::new(),
        skills: Vec::new(),
        ..SessionConfig::default()
    };
    super::inject_thurbox_env(&mut config, &agent_session_id);

    let (command, args) = super::build_claude_invocation(&config);

    crate::agent::tmux::kill_window(&session.name)
        .map_err(|e| format!("Failed to kill tmux window: {e}"))?;
    crate::agent::tmux::spawn_window(
        &session.name,
        &command,
        &args,
        config.cwd.as_deref(),
        &config.permissions.env,
    )
    .map_err(|e| format!("Failed to re-spawn tmux window: {e}"))?;

    Ok(())
}

/// Look up permissions for `role_name`, falling back to the seeded default
/// developer role (or empty permissions for any other unknown role).
fn lookup_role_permissions(db: &Database, role_name: &str) -> Result<RolePermissions, String> {
    let global_roles = db
        .list_global_roles()
        .map_err(|e| format!("Failed to load roles: {e}"))?;
    Ok(global_roles
        .iter()
        .find(|r| r.name == role_name)
        .map(|r| r.permissions.clone())
        .unwrap_or_else(|| {
            if role_name == DEFAULT_ROLE_NAME {
                default_developer_permissions()
            } else {
                RolePermissions::default()
            }
        }))
}
