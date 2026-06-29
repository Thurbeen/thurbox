//! Headless session restart — tears down the tmux window and re-launches
//! the agent CLI, resuming the existing conversation when the agent supports
//! it and a transcript exists, starting fresh otherwise.

use std::collections::HashMap;
use std::path::PathBuf;

use crate::session::{SessionConfig, SessionId};
use crate::storage::Database;
use crate::sync::SharedSession;

/// The resolved inputs for re-spawning a session's tmux window: the agent
/// command + args, the process cwd, and the identity env. Extracted from the
/// side-effecting [`restart_session_headless`] so the resolution logic (env
/// injection, resume trigger, multi-repo workspace cwd) is unit-testable
/// without driving tmux.
#[derive(Debug, Clone, PartialEq, Eq)]
struct RestartPlan {
    window_name: String,
    command: String,
    args: Vec<String>,
    cwd: Option<PathBuf>,
    env: HashMap<String, String>,
}

/// Build the [`RestartPlan`] for a persisted session: keep its identity stable,
/// inject the standard `THURBOX_*` env, decide the resume trigger from the
/// agent definition, and resolve the process cwd (the symlink workspace for a
/// multi-repo session, else the primary repo — mirroring the TUI's
/// `App::resolve_process_cwd`).
fn build_restart_plan(session: &SharedSession) -> Result<RestartPlan, String> {
    let agent_session_id = session.agent_session_id.clone().ok_or_else(|| {
        format!(
            "Cannot restart session {} without agent_session_id",
            session.id
        )
    })?;

    let mut config = SessionConfig {
        // Keep the same identity across a restart so `THURBOX_SESSION` is stable.
        session_id: Some(session.id),
        agent_session_id: Some(agent_session_id.clone()),
        cwd: session.cwd.clone(),
        agent: session.agent.clone(),
        ..SessionConfig::default()
    };
    super::inject_thurbox_env(&mut config, &agent_session_id, None);
    let def = super::resolve_agent_def(Some(&config.agent));
    config.resume_session_id = super::resume_trigger_for(&def, &agent_session_id, &config.env);

    // A multi-repo session (≥2 members) launches in its per-session symlink
    // workspace, gathering every member dir; a single-repo session keeps the
    // primary repo. Only resolve when there is a primary cwd to anchor on.
    if let Some(primary) = session.cwd.clone() {
        config.cwd = Some(super::spawn::resolve_launch_cwd(
            &agent_session_id,
            &primary,
            &session.worktrees,
            &session.additional_dirs,
        ));
    }

    let (command, args) = super::build_agent_invocation(&def, &config);

    Ok(RestartPlan {
        window_name: session.name.clone(),
        command,
        args,
        cwd: config.cwd,
        env: config.env,
    })
}

/// Restart an existing session in-place — kills its tmux window and
/// re-spawns the agent CLI.
///
/// For the `claude` agent, uses its resume group when a transcript for the
/// session id exists on disk, otherwise pins the same id for a fresh start.
/// For `resume_latest` agents (codex, opencode, antigravity, aider, copilot) it
/// resumes the latest session in the (unchanged) launch directory. Other agents
/// degrade to "start fresh" (the live tmux process is what carries state across
/// restarts).
pub fn restart_session_headless(db: &Database, session_id: SessionId) -> Result<(), String> {
    let session = db
        .get_session_by_id(session_id)
        .map_err(|e| format!("Failed to load session: {e}"))?
        .ok_or_else(|| format!("Session not found: {session_id}"))?;

    let plan = build_restart_plan(&session)?;

    crate::agent::tmux::kill_window(&plan.window_name)
        .map_err(|e| format!("Failed to kill tmux window: {e}"))?;
    crate::agent::tmux::spawn_window(
        &plan.window_name,
        &plan.command,
        &plan.args,
        plan.cwd.as_deref(),
        &plan.env,
    )
    .map_err(|e| format!("Failed to re-spawn tmux window: {e}"))?;

    // The agent was re-spawned fresh; clear any stale hook-driven status so it
    // doesn't show a leftover Blocked/Working/Done until the agent re-reports
    // (a resumed agent may not re-fire its boot hook). Best-effort.
    let _ = db.clear_hook_state(session_id);

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(agent_session_id: Option<&str>, cwd: Option<PathBuf>) -> SharedSession {
        SharedSession {
            id: SessionId::default(),
            name: "demo".into(),
            agent: "claude".into(),
            backend_id: String::new(),
            backend_type: "local-tmux".into(),
            agent_session_id: agent_session_id.map(String::from),
            cwd,
            additional_dirs: Vec::new(),
            worktrees: Vec::new(),
            shell_backend_id: None,
            parent_session_id: None,
            display_order: None,
            tombstone: false,
            tombstone_at: None,
        }
    }

    #[test]
    fn restart_plan_requires_agent_session_id() {
        let temp = tempfile::TempDir::new().unwrap();
        let _guard = crate::paths::TestPathGuard::new(temp.path());
        let err = build_restart_plan(&session(None, None)).unwrap_err();
        assert!(err.contains("agent_session_id"), "got: {err}");
    }

    #[test]
    fn restart_plan_injects_identity_env() {
        let temp = tempfile::TempDir::new().unwrap();
        let _guard = crate::paths::TestPathGuard::new(temp.path());
        let sess = session(Some("agent-conv-uuid"), Some(PathBuf::from("/tmp/repo")));
        let plan = build_restart_plan(&sess).unwrap();

        // The thurbox session key and the agent conversation id are both present
        // and distinct, exactly as a fresh spawn would inject them.
        assert_eq!(plan.env.get("THURBOX_SESSION"), Some(&sess.id.to_string()));
        assert_eq!(
            plan.env.get("THURBOX_SESSION_ID"),
            Some(&"agent-conv-uuid".to_string())
        );
    }

    #[test]
    fn restart_plan_single_repo_launches_in_primary() {
        let temp = tempfile::TempDir::new().unwrap();
        let _guard = crate::paths::TestPathGuard::new(temp.path());
        let primary = temp.path().join("primary");
        std::fs::create_dir_all(&primary).unwrap();
        let plan = build_restart_plan(&session(Some("sid"), Some(primary.clone()))).unwrap();
        assert_eq!(plan.cwd, Some(primary));
    }

    #[test]
    fn restart_plan_multi_repo_launches_in_workspace() {
        let temp = tempfile::TempDir::new().unwrap();
        let _guard = crate::paths::TestPathGuard::new(temp.path());
        let primary = temp.path().join("primary");
        std::fs::create_dir_all(&primary).unwrap();
        let extra = temp.path().join("extra");
        std::fs::create_dir_all(&extra).unwrap();

        let mut sess = session(Some("sid-multi"), Some(primary.clone()));
        sess.additional_dirs = vec![extra];

        let plan = build_restart_plan(&sess).unwrap();
        // ≥2 members → the symlink workspace, not the primary repo itself.
        assert_ne!(plan.cwd.as_deref(), Some(primary.as_path()));
        assert!(plan.cwd.is_some());
    }
}
