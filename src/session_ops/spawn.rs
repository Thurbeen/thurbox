//! Headless session spawn — creates a local-tmux session without requiring
//! the TUI event loop.

use std::path::PathBuf;

use crate::session::{ExtraRepo, HostDef, SessionConfig, SessionId, DEFAULT_AGENT_NAME};
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
    /// Optional originating task id. When set it is injected as `THURBOX_TASK`
    /// so the session's outgoing messages auto-tag `from_task_id` without the
    /// agent passing any id by hand.
    pub task_id: Option<i64>,
    /// Additional repositories this session spans (empty = single-repo, the
    /// unchanged common case). Each either gets its own isolated worktree on
    /// the shared `worktree_branch` (off its own `base_branch`) or is attached
    /// as-is as an additional directory. When any extra is non-empty the agent
    /// launches in a per-session symlink workspace gathering every member.
    pub extra_repos: Vec<ExtraRepo>,
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
    let (primary_cwd, worktrees, additional_dirs) = resolve_dirs(&req, host.as_ref())?;

    let agent_session_id = req
        .agent_session_id
        .clone()
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());

    // For a multi-repo session, launch the agent in a per-session symlink
    // workspace gathering every member dir (so each repo is a visible subdir,
    // agent-neutral). `info.cwd` keeps the *primary* repo. Single-repo sessions
    // launch directly in the primary cwd, unchanged. Mirrors the TUI's
    // `App::resolve_process_cwd`.
    let launch_cwd = resolve_launch_cwd(
        &agent_session_id,
        &primary_cwd,
        &worktrees,
        &additional_dirs,
    );

    // Mint the thurbox SessionId up front so it can be injected into the
    // process env (`THURBOX_SESSION`) before the agent launches.
    let session_id = SessionId::default();

    let mut config = SessionConfig {
        session_id: Some(session_id),
        agent_session_id: Some(agent_session_id.clone()),
        cwd: Some(launch_cwd.clone()),
        agent: agent_name.clone(),
        backend: (backend_type != LOCAL_TMUX_BACKEND_TYPE).then(|| backend_type.clone()),
        ..SessionConfig::default()
    };
    super::inject_thurbox_env(&mut config, &agent_session_id, req.task_id);

    let (command, args) = super::build_agent_invocation(&config);

    // Remote spawns drive the SSH backend's control mode to learn the real pane
    // id; local spawns leave `backend_id` empty for the TUI to resolve by name.
    let backend_id = match host.as_ref() {
        Some(h) => crate::agent::tmux::spawn_window_remote(
            h,
            &req.name,
            &command,
            &args,
            Some(&launch_cwd),
            &config.env,
        )
        .map_err(|e| format!("Failed to spawn remote tmux window: {e:#}"))?,
        None => {
            crate::agent::tmux::spawn_window(
                &req.name,
                &command,
                &args,
                Some(&launch_cwd),
                &config.env,
            )
            .map_err(|e| format!("Failed to spawn tmux window: {e}"))?;
            String::new()
        }
    };

    let shared = SharedSession {
        id: session_id,
        name: req.name.clone(),
        agent: agent_name.clone(),
        backend_id: backend_id.clone(),
        backend_type,
        agent_session_id: Some(agent_session_id.clone()),
        // `cwd` is the *primary* repo (for display / git context); the workspace
        // is a spawn-time launch detail, re-derived idempotently on every launch.
        cwd: Some(primary_cwd.clone()),
        additional_dirs: additional_dirs.clone(),
        worktrees: worktrees.clone(),
        shell_backend_id: None,
        parent_session_id: req.parent_session_id,
        display_order: None,
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
        cwd: primary_cwd,
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

/// Resolve the primary working directory, all worktree records, and the
/// non-worktree additional directories for a (possibly multi-repo) spawn.
///
/// Single-repo (no `extra_repos`): returns the bare repo path when no worktree
/// branch is given, otherwise the primary worktree path plus one
/// [`SharedWorktree`] — byte-identical to the pre-multi-repo behavior.
///
/// Multi-repo: the primary is resolved as above; each [`ExtraRepo`] either gets
/// its own worktree on the **shared** `worktree_branch` (off its own
/// `base_branch`, falling back to the primary's base) appended to `worktrees`,
/// or — when `worktree == false` — is attached as-is in `additional_dirs`. The
/// member set (worktrees + additional dirs) is what the symlink workspace
/// gathers; see [`crate::workspace::ensure_workspace`].
fn resolve_dirs(
    req: &SpawnRequest,
    host: Option<&HostDef>,
) -> Result<(PathBuf, Vec<SharedWorktree>, Vec<PathBuf>), String> {
    let mut worktrees: Vec<SharedWorktree> = Vec::new();
    let mut additional_dirs: Vec<PathBuf> = Vec::new();

    // Primary repo: worktree when a branch is set, otherwise the repo root.
    let primary_cwd = match req.worktree_branch.as_deref() {
        None => req.repo_path.clone(),
        Some(branch) => {
            let base = req.base_branch.as_deref().unwrap_or(DEFAULT_BASE_BRANCH);
            let path = create_worktree(host, &req.repo_path, branch, base)?;
            worktrees.push(SharedWorktree {
                repo_path: req.repo_path.clone(),
                worktree_path: path.clone(),
                branch: branch.to_string(),
            });
            path
        }
    };

    // Extra repos: each its own isolated worktree on the shared branch, or a
    // plain additional directory.
    for extra in &req.extra_repos {
        if extra.worktree {
            let branch = req.worktree_branch.as_deref().ok_or_else(|| {
                "a worktree extra-repo requires --worktree-branch (the shared branch)".to_string()
            })?;
            let base = extra
                .base_branch
                .as_deref()
                .or(req.base_branch.as_deref())
                .unwrap_or(DEFAULT_BASE_BRANCH);
            let path = create_worktree(host, &extra.repo_path, branch, base)?;
            worktrees.push(SharedWorktree {
                repo_path: extra.repo_path.clone(),
                worktree_path: path.clone(),
                branch: branch.to_string(),
            });
        } else {
            additional_dirs.push(extra.repo_path.clone());
        }
    }

    Ok((primary_cwd, worktrees, additional_dirs))
}

/// Create a worktree, wrapping the error with the branch/base for context.
fn create_worktree(
    host: Option<&HostDef>,
    repo: &std::path::Path,
    branch: &str,
    base: &str,
) -> Result<PathBuf, String> {
    crate::git::create_worktree_on(host, repo, branch, base)
        .map_err(|e| format!("Failed to create worktree {branch} off {base} in {repo:?}: {e}"))
}

/// The directory the agent process should launch in: a per-session symlink
/// workspace for a multi-repo session (≥2 members), else the primary cwd.
///
/// Mirrors the TUI's `App::resolve_process_cwd`: members are the worktree repos
/// (labeled by their original repo name) followed by the non-worktree
/// additional dirs; a workspace build failure falls back to the primary cwd.
fn resolve_launch_cwd(
    agent_session_id: &str,
    primary_cwd: &std::path::Path,
    worktrees: &[SharedWorktree],
    additional_dirs: &[PathBuf],
) -> PathBuf {
    let mut members: Vec<(String, PathBuf)> = Vec::new();
    if worktrees.is_empty() {
        members.push((dir_label(primary_cwd), primary_cwd.to_path_buf()));
    } else {
        for wt in worktrees {
            members.push((dir_label(&wt.repo_path), wt.worktree_path.clone()));
        }
    }
    for dir in additional_dirs {
        members.push((dir_label(dir), dir.clone()));
    }

    if members.len() < 2 {
        return primary_cwd.to_path_buf();
    }
    match crate::workspace::ensure_workspace(agent_session_id, &members) {
        Ok(ws) => ws,
        Err(e) => {
            tracing::error!("Failed to build multi-repo workspace: {e}");
            primary_cwd.to_path_buf()
        }
    }
}

/// A human-friendly label for a member directory in the symlink workspace:
/// the git repo display name, falling back to the final path component.
fn dir_label(path: &std::path::Path) -> String {
    crate::git::repo_display_name(path)
        .or_else(|| path.file_name().and_then(|s| s.to_str()).map(String::from))
        .unwrap_or_else(|| "repo".to_string())
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
            task_id: None,
            extra_repos: Vec::new(),
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
            display_order: None,
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

    #[test]
    fn resolve_dirs_single_repo_no_worktree_is_unchanged() {
        let mut r = req("s");
        r.repo_path = PathBuf::from("/tmp/primary");
        let (cwd, worktrees, additional) = resolve_dirs(&r, None).unwrap();
        assert_eq!(cwd, PathBuf::from("/tmp/primary"));
        assert!(worktrees.is_empty());
        assert!(additional.is_empty());
    }

    #[test]
    fn resolve_dirs_attaches_dir_extras_without_git() {
        // dir-only extras (worktree == false) never touch git, so this is
        // hermetic. The primary has no worktree branch either.
        let mut r = req("s");
        r.repo_path = PathBuf::from("/tmp/primary");
        r.extra_repos = vec![
            ExtraRepo {
                repo_path: PathBuf::from("/tmp/extra-a"),
                worktree: false,
                base_branch: None,
            },
            ExtraRepo {
                repo_path: PathBuf::from("/tmp/extra-b"),
                worktree: false,
                base_branch: None,
            },
        ];
        let (cwd, worktrees, additional) = resolve_dirs(&r, None).unwrap();
        assert_eq!(cwd, PathBuf::from("/tmp/primary"));
        assert!(worktrees.is_empty());
        assert_eq!(
            additional,
            vec![PathBuf::from("/tmp/extra-a"), PathBuf::from("/tmp/extra-b")]
        );
    }

    #[test]
    fn resolve_dirs_worktree_extra_without_shared_branch_errors() {
        let mut r = req("s");
        r.repo_path = PathBuf::from("/tmp/primary");
        // No worktree_branch on the primary, but an extra wants a worktree.
        r.extra_repos = vec![ExtraRepo {
            repo_path: PathBuf::from("/tmp/extra"),
            worktree: true,
            base_branch: None,
        }];
        let err = resolve_dirs(&r, None).unwrap_err();
        assert!(err.contains("worktree-branch"), "got: {err}");
    }

    #[test]
    fn resolve_launch_cwd_single_member_is_primary() {
        let primary = PathBuf::from("/tmp/primary");
        // No worktrees, no extra dirs → 1 member → primary cwd, no workspace.
        let got = resolve_launch_cwd("sid-1", &primary, &[], &[]);
        assert_eq!(got, primary);
    }

    #[test]
    fn resolve_launch_cwd_multi_member_builds_workspace() {
        let temp = tempfile::TempDir::new().unwrap();
        let _guard = crate::paths::TestPathGuard::new(temp.path());
        let primary = temp.path().join("primary");
        std::fs::create_dir_all(&primary).unwrap();
        let extra = temp.path().join("extra");
        std::fs::create_dir_all(&extra).unwrap();
        let got = resolve_launch_cwd("sid-multi", &primary, &[], &[extra]);
        // Two members → a symlink workspace, not the primary itself.
        assert_ne!(got, primary);
        assert!(got.join("primary").exists() || got.exists());
    }
}
