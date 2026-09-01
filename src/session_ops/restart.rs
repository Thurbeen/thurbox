//! Headless session restart — tears down the tmux window and re-launches
//! the agent CLI, resuming the existing conversation when the agent supports
//! it and a transcript exists, starting fresh otherwise.
//!
//! And its two halves on their own: [`stop_session_headless`] kills the window
//! and leaves everything else standing, [`start_session_headless`] puts a
//! window back. A restart is those two in a row, which is why they live here.

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
/// replay the session's recorded `--env`, inject the standard `THURBOX_*` env,
/// decide the resume trigger from the agent definition, and resolve the process
/// cwd (the symlink workspace for a multi-repo session, else the primary repo —
/// mirroring the TUI's `App::resolve_process_cwd`).
pub(crate) fn build_restart_plan(
    session: &SharedSession,
    host: Option<&crate::session::HostDef>,
    hooks_enabled: bool,
    recipe: Option<&crate::session::LaunchRecipe>,
    env: &std::collections::BTreeMap<String, String>,
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
    // The session's own env is part of what it *is*, so it goes on before
    // thurbox's identity vars — which must still win — exactly as at spawn.
    // Recorded for a registry agent as much as for a command session: `--env`
    // is the caller's, not the registry's, and there is nowhere else to
    // re-resolve it from.
    config
        .env
        .extend(env.iter().map(|(k, v)| (k.clone(), v.clone())));
    super::inject_thurbox_env(&mut config, &agent_session_id, None);
    // Where a restart gets what to run. A registry agent is resolved by name
    // *now* rather than replayed, so an `agents.toml` edit takes effect on the
    // next restart. A command session has no entry to resolve, so its persisted
    // recipe is replayed verbatim — the reason the recipe is stored at all.
    let def = match recipe {
        Some(r) => super::recipe_agent_def(r),
        None => super::resolve_agent_def(Some(&config.agent)),
    };
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

/// Park a session: kill its pane, keep everything else.
///
/// The row, the checkout, the branch, the agent's own conversation on disk all
/// survive — only the process and its window go. That is the difference from a
/// delete, and it is the operation that was missing: until now the only way to
/// reclaim a heavy agent's pane headlessly was to delete the session, which
/// also removed its worktrees and cancelled its scheduled commands.
///
/// The mark is written **before** the kill. Three subsystems repair a session
/// that has no pane (the interface's respawn of surveyed rows, a peer's
/// `restart --if-missing`, extension self-heal), and the window between killing
/// and recording is exactly when one of them would put it back.
pub fn stop_session_headless(db: &Database, session_id: SessionId) -> Result<bool, String> {
    let session = db
        .get_session_by_id(session_id)
        .map_err(|e| format!("Failed to load session: {e}"))?
        .ok_or_else(|| format!("Session not found: {session_id}"))?;

    db.set_session_stopped(session_id, true)
        .map_err(|e| format!("Failed to mark the session stopped: {e}"))?;

    let killed = if crate::session::is_remote_backend(&session.backend_type) {
        match super::resolve_host(&session.backend_type).flatten() {
            Some(host) if !session.backend_id.is_empty() => {
                crate::agent::tmux::kill_pane_remote(&host, &session.backend_id).is_ok()
            }
            // An unreachable host is not a reason to refuse: the mark is what
            // makes the stop stick, and the pane is reclaimed by the next
            // teardown that can reach it.
            _ => false,
        }
    } else {
        crate::agent::tmux::kill_window(&session.name, &session.backend_id).is_ok()
    };

    // A stopped session reports nothing, so a leftover `working` would sit on
    // the status line for as long as it stayed parked — and the quiescence pass
    // that would normally correct it has no terminal to measure.
    let _ = db.clear_hook_state(session_id);
    Ok(killed)
}

/// Un-park a session: put a window back, resuming the conversation the same way
/// a restart does.
///
/// Clearing the mark first is what makes this work at all — the relaunch below
/// refuses a stopped session on purpose, so that a peer's `restart --if-missing`
/// cannot resurrect one. `start` is the one caller allowed to say otherwise.
pub fn start_session_headless(
    db: &Database,
    session_id: SessionId,
) -> Result<RestartReport, String> {
    db.set_session_stopped(session_id, false)
        .map_err(|e| format!("Failed to clear the stopped mark: {e}"))?;
    restart_session_headless_with(db, session_id, true)
}

/// What a restart has to say beyond having happened.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RestartReport {
    /// `session.post_restart` hooks that failed. The restart stands regardless.
    pub hook_failures: Vec<String>,
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
pub fn restart_session_headless(
    db: &Database,
    session_id: SessionId,
) -> Result<RestartReport, String> {
    restart_session_headless_with(db, session_id, false)
}

/// [`restart_session_headless`], or — with `if_missing` — a **relaunch**: the
/// agent is started only when the session has no live window, and a session
/// that is running is left exactly as it is. This is what "the agent is gone"
/// asks for after a reboot, and what makes two observers asking for the same
/// session produce one launch: the second finds the window the first made.
pub fn restart_session_headless_with(
    db: &Database,
    session_id: SessionId,
    if_missing: bool,
) -> Result<RestartReport, String> {
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

    // A session on a shareable host is restarted by the host's CLI, which
    // kills and relaunches where the agent runs, with the host's own
    // configuration; the local row is then mirrored from the host's.
    if let Some((host, cli)) = host
        .as_ref()
        .and_then(|h| super::host_cli::delegated(h).map(|cli| (h, cli)))
    {
        let mut hook_ctx = super::lifecycle_hooks::context_for(&session);
        super::fire_pre(crate::session::HookEvent::PreRestart, &hook_ctx)?;
        let id = session_id.to_string();
        let mut args = vec!["session", "restart", &id];
        if if_missing {
            args.push("--if-missing");
        }
        super::host_cli::run(host, &cli, &args)?;
        if let Err(e) = super::mirror::mirror_host(db, host, &cli) {
            tracing::warn!("mirror of '{}' after restart failed: {e}", host.name);
        }
        hook_ctx.backend_id = super::lifecycle_hooks::current_pane(db, session_id);
        let hook_failures = super::fire_post(crate::session::HookEvent::PostRestart, &hook_ctx);
        return Ok(RestartReport { hook_failures });
    }

    if if_missing {
        // "Relaunch what is missing" is what a peer asks after a reboot, and a
        // parked session is missing on purpose. Only `session start` clears the
        // mark, so this is the one place that has to check it — refusing here
        // is what makes `stop` outlive the next sync tick.
        if db
            .session_stopped_at(session_id)
            .unwrap_or_default()
            .is_some()
        {
            tracing::debug!("'{}' is stopped; not relaunching", session.name);
            return Ok(RestartReport::default());
        }
        let alive = crate::agent::tmux::agent_window_alive(host.as_ref(), &session.name)
            .map_err(|e| format!("could not list windows for '{}': {e:#}", session.name))?;
        if alive {
            tracing::debug!("'{}' is running; nothing to relaunch", session.name);
            return Ok(RestartReport::default());
        }
    }

    let hooks_enabled = super::hooks_enabled(db);
    let recipe = db.load_launch_recipe(session_id).unwrap_or_default();
    let env = db.load_launch_env(session_id).unwrap_or_default();
    let plan = build_restart_plan(
        &session,
        host.as_ref(),
        hooks_enabled,
        recipe.as_ref(),
        &env,
    )?;

    // The user's say, with the plan built and nothing yet killed: a refusal
    // leaves the running window running.
    let mut hook_ctx = super::lifecycle_hooks::context_for(&session);
    super::fire_pre(crate::session::HookEvent::PreRestart, &hook_ctx)?;

    match host.as_ref() {
        None => {
            // A window that is already gone is not an error — the same rule the
            // remote branch follows. It is also what makes this the *respawn*
            // path: a session whose tmux server died is restarted by asking for
            // exactly this, and refusing because there was nothing to kill would
            // make the one case that needs it the one case that fails.
            // Killed by the persisted pane id when one is usable: the window
            // *name* is not unique, and killing by name with a duplicate around
            // tears down an arbitrary one of them.
            if let Err(e) = crate::agent::tmux::kill_window(&plan.window_name, &session.backend_id)
            {
                tracing::debug!("no window to kill for '{}': {e:#}", plan.window_name);
            }
            let pane = crate::agent::tmux::spawn_window(
                &plan.window_name,
                &plan.command,
                &plan.args,
                plan.cwd.as_deref(),
                &plan.env,
            )
            .map_err(|e| format!("Failed to re-spawn tmux window: {e}"))?;

            // The new pane is a different one, and the id is how every later
            // read finds it — leaving the old one persisted would point the
            // interface at a pane that no longer exists. (Empty on psmux, where
            // the spawn can't report an id; the interface then resolves by
            // window name as before.)
            let mut session = session.clone();
            session.backend_id = pane;
            db.upsert_session(&session)
                .map_err(|e| format!("Failed to record the new pane: {e}"))?;
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

    // The new pane is what the row now points at, so that is what the
    // post-restart hooks are told.
    hook_ctx.backend_id = super::lifecycle_hooks::current_pane(db, session_id);
    let hook_failures = super::fire_post(crate::session::HookEvent::PostRestart, &hook_ctx);

    Ok(RestartReport { hook_failures })
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
        let err = build_restart_plan(&session(None, None), None, true, None, &Default::default())
            .unwrap_err();
        assert!(err.contains("agent_session_id"), "got: {err}");
    }

    #[test]
    fn restart_plan_injects_identity_env() {
        let temp = tempfile::TempDir::new().unwrap();
        let _guard = crate::paths::TestPathGuard::new(temp.path());
        let sess = session(Some("agent-conv-uuid"), Some(PathBuf::from("/tmp/repo")));
        let plan = build_restart_plan(&sess, None, true, None, &Default::default()).unwrap();

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
        let plan = build_restart_plan(
            &session(Some("sid"), Some(primary.clone())),
            None,
            true,
            None,
            &Default::default(),
        )
        .unwrap();
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

        let plan = build_restart_plan(&sess, None, true, None, &Default::default()).unwrap();
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

        let plan = build_restart_plan(&sess, None, true, None, &Default::default()).unwrap();
        assert_eq!(plan.env.get("THURBOX_SESSION"), Some(&sess.id.to_string()));
        assert!(!plan.env.contains_key(crate::paths::CONFIG_DIR_OVERRIDE_ENV));
        assert!(!plan.env.contains_key("THURBOX_METRICS_DIR"));
    }
}
