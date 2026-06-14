//! Headless session restart — tears down the tmux window and re-launches
//! the agent CLI, resuming the existing conversation when the agent supports
//! it and a transcript exists, starting fresh otherwise.

use crate::session::{SessionConfig, SessionId};
use crate::storage::Database;

/// Restart an existing session in-place — kills its tmux window and
/// re-spawns the agent CLI.
///
/// For the `claude` agent, uses its resume group when a transcript for the
/// session id exists on disk, otherwise pins the same id for a fresh start.
/// For `resume_latest` agents (codex, opencode, gemini, aider) it resumes the
/// latest session in the (unchanged) launch directory. Other agents degrade to
/// "start fresh" (the live tmux process is what carries state across restarts).
pub fn restart_session_headless(db: &Database, session_id: SessionId) -> Result<(), String> {
    let session = db
        .get_session_by_id(session_id)
        .map_err(|e| format!("Failed to load session: {e}"))?
        .ok_or_else(|| format!("Session not found: {session_id}"))?;

    let agent_session_id = session
        .agent_session_id
        .clone()
        .ok_or_else(|| format!("Cannot restart session {session_id} without agent_session_id"))?;

    let mut config = SessionConfig {
        // Keep the same identity across a restart so `THURBOX_SESSION` is stable.
        session_id: Some(session_id),
        agent_session_id: Some(agent_session_id.clone()),
        cwd: session.cwd.clone(),
        agent: session.agent.clone(),
        ..SessionConfig::default()
    };
    super::inject_thurbox_env(&mut config, &agent_session_id, None);
    let def = super::resolve_agent_def(&config.agent);
    config.resume_session_id = super::resume_trigger_for(&def, &agent_session_id, &config.env);

    let (command, args) = super::build_agent_invocation(&config);

    crate::agent::tmux::kill_window(&session.name)
        .map_err(|e| format!("Failed to kill tmux window: {e}"))?;
    crate::agent::tmux::spawn_window(
        &session.name,
        &command,
        &args,
        config.cwd.as_deref(),
        &config.env,
    )
    .map_err(|e| format!("Failed to re-spawn tmux window: {e}"))?;

    Ok(())
}
