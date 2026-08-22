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
pub(crate) struct RestartPlan {
    pub(crate) window_name: String,
    pub(crate) command: String,
    pub(crate) args: Vec<String>,
    pub(crate) cwd: Option<PathBuf>,
    pub(crate) env: HashMap<String, String>,
}

/// Build the [`RestartPlan`] for a persisted session: keep its identity stable,
/// inject the standard `THURBOX_*` env, decide the resume trigger from the
/// agent definition, and resolve the process cwd (the symlink workspace for a
/// multi-repo session, else the primary repo — mirroring the TUI's
/// `App::resolve_process_cwd`).
pub(crate) fn build_restart_plan(
    session: &SharedSession,
    host: Option<&crate::session::HostDef>,
    hooks_enabled: bool,
) -> Result<RestartPlan, String> {
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
        // Which machine this is for — `inject_thurbox_env` reads it to decide
        // whether the local-path hints travel (they must not, off-local).
        backend: Some(session.backend_type.clone()),
        ..SessionConfig::default()
    };
    super::inject_thurbox_env(&mut config, &agent_session_id, None);
    let def = super::resolve_agent_def(Some(&config.agent));
    // The same adaptation a spawn does, so a restart lands the agent exactly
    // where the spawn did: `{home}` resolved against the machine it runs on
    // (omp's `--resume {home}/…` must reopen the same JSONL) and hook configs
    // shipped to the host rather than pointing at local paths that do not exist
    // there. Identity for a local restart.
    let (def, degraded) = super::spawn::adapt_def_for_launch(def, host, hooks_enabled);
    if let Some(note) = degraded {
        tracing::warn!("restart of '{}': {note}", session.name);
    }
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
            host,
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

    // A remote session's window lives on its host, so both halves have to go
    // there — restarting it locally would leave the real window running and add
    // a stray local one beside it. Refusing is only right when we cannot tell
    // *which* host, which is what a missing `hosts.toml` entry means.
    let host = super::resolve_host(&session.backend_type).ok_or_else(|| {
        format!(
            "Session '{}' runs on backend '{}', which is not in hosts.toml — \
             cannot reach the machine it lives on",
            session.name, session.backend_type
        )
    })?;

    let hooks_enabled = super::hooks_enabled(db);
    let plan = build_restart_plan(&session, host.as_ref(), hooks_enabled)?;

    match host.as_ref() {
        None => {
            // A window that is already gone is not an error — the same rule the
            // remote branch follows. It is also what makes this the *respawn*
            // path: a session whose tmux server died is restarted by asking for
            // exactly this, and refusing because there was nothing to kill would
            // make the one case that needs it the one case that fails.
            if let Err(e) = crate::agent::tmux::kill_window(&plan.window_name) {
                tracing::debug!("no window to kill for '{}': {e:#}", plan.window_name);
            }
            crate::agent::tmux::spawn_window(
                &plan.window_name,
                &plan.command,
                &plan.args,
                plan.cwd.as_deref(),
                &plan.env,
            )
            .map_err(|e| format!("Failed to re-spawn tmux window: {e}"))?;

            // Forget the pane that was just killed. The local one-shot spawn
            // does not report the new pane's id — a local spawn never has, which
            // is why the interface resolves one by window name — so the row must
            // not keep pointing at the dead one. Left as the old id, the
            // interface stays attached to a pane that no longer exists and the
            // restarted session shows a frozen last frame that takes no keys.
            let mut session = session.clone();
            session.backend_id = String::new();
            db.upsert_session(&session)
                .map_err(|e| format!("Failed to clear the old pane: {e}"))?;
        }
        Some(host) => {
            // By pane id, not window name: the host's tmux server is shared with
            // whatever else runs there, and a name is not ours to claim. A pane
            // that is already gone is not an error — the restart is what the
            // caller wanted, and it can still happen.
            if !session.backend_id.is_empty() {
                if let Err(e) = crate::agent::tmux::kill_pane_remote(host, &session.backend_id) {
                    tracing::warn!("could not kill remote pane {}: {e:#}", session.backend_id);
                }
            }
            let pane = crate::agent::tmux::spawn_window_remote(
                host,
                &plan.window_name,
                &plan.command,
                &plan.args,
                plan.cwd.as_deref(),
                &plan.env,
            )
            .map_err(|e| format!("Failed to re-spawn window on '{}': {e:#}", host.name))?;

            // The new pane is a different one, and the id is how every later
            // read finds it — leaving the old one persisted would point the
            // interface at a pane that no longer exists.
            let mut session = session.clone();
            session.backend_id = pane;
            db.upsert_session(&session)
                .map_err(|e| format!("Failed to record the new pane: {e}"))?;
        }
    }

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
        let err = build_restart_plan(&session(None, None), None, true).unwrap_err();
        assert!(err.contains("agent_session_id"), "got: {err}");
    }

    #[test]
    fn restart_plan_injects_identity_env() {
        let temp = tempfile::TempDir::new().unwrap();
        let _guard = crate::paths::TestPathGuard::new(temp.path());
        let sess = session(Some("agent-conv-uuid"), Some(PathBuf::from("/tmp/repo")));
        let plan = build_restart_plan(&sess, None, true).unwrap();

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
        let plan =
            build_restart_plan(&session(Some("sid"), Some(primary.clone())), None, true).unwrap();
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

        let plan = build_restart_plan(&sess, None, true).unwrap();
        // ≥2 members → the symlink workspace, not the primary repo itself.
        assert_ne!(plan.cwd.as_deref(), Some(primary.as_path()));
        assert!(plan.cwd.is_some());
    }

    #[test]
    fn a_local_session_resolves_to_no_host() {
        assert_eq!(super::super::resolve_host("local-tmux"), Some(None));
    }

    #[test]
    fn a_backend_no_hosts_file_describes_resolves_to_nothing() {
        // Not "local": the session runs somewhere we can no longer reach, and
        // restarting it here would spawn a local impostor beside the real one.
        assert_eq!(
            super::super::resolve_host("ssh:host-that-is-not-configured"),
            None
        );
    }

    #[test]
    fn a_remote_restart_does_not_carry_local_paths_to_the_host() {
        // The identity vars are opaque and travel; the path hints name local
        // directories that do not exist on the host, so a remote `thurbox-cli`
        // pinned to them would resolve garbage instead of its own defaults.
        let temp = tempfile::TempDir::new().unwrap();
        let _guard = crate::paths::TestPathGuard::new(temp.path());
        let mut sess = session(Some("agent-conv-uuid"), Some(PathBuf::from("/srv/repo")));
        sess.backend_type = "ssh:devbox".into();

        let plan = build_restart_plan(&sess, None, true).unwrap();
        assert_eq!(plan.env.get("THURBOX_SESSION"), Some(&sess.id.to_string()));
        assert!(!plan.env.contains_key(crate::paths::CONFIG_DIR_OVERRIDE_ENV));
        assert!(!plan.env.contains_key("THURBOX_METRICS_DIR"));
    }
}
