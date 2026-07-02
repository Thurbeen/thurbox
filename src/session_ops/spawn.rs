//! Headless session spawn — creates a local-tmux session without requiring
//! the TUI event loop.

use std::path::PathBuf;

use crate::session::{ExtraRepo, HostDef, SessionConfig, SessionId};
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
    crate::paths::validate_safe_name(&req.name)?;
    validate_parent_session(db, req.parent_session_id)?;

    // Resolve the agent definition once; `agent_name` is derived from it so the
    // persisted name always matches the def that's actually launched.
    let mut agent_def = super::resolve_agent_def(req.agent.as_deref());
    let agent_name = agent_def.name.clone();

    // Resolve the optional remote host. `backend_type` is `local-tmux` or
    // `ssh:<host>`; `host` is the matching HostDef for remote git/tmux ops.
    let (backend_type, host) = resolve_host(req.host.as_deref())?;

    // The def's `args` may reference thurbox-managed config files by their
    // *local* absolute path (e.g. claude's hooks `--settings <config>/hooks/
    // claude.json`), which the agent errors on when the path doesn't exist on
    // the host ("Settings file not found" → the pane dies instantly). Rewrite
    // them for the host: materialize the file remotely (translating a
    // home-anchored path to the remote home) or, when that's impossible, strip
    // the flag so the agent at least launches.
    if let Some(h) = host.as_ref() {
        agent_def.args = adapt_agent_args_for_remote(h, agent_def.args);
    }
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
        host.as_ref(),
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

    let (command, args) = super::build_agent_invocation(&agent_def, &config);

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

    // Record the worktree's fork point so the code-review view can scope its
    // diff to `<base>..HEAD`. Only meaningful for worktree sessions; a bare-repo
    // session leaves it NULL (review falls back to the repo's default branch).
    if req.worktree_branch.is_some() {
        let base = req.base_branch.as_deref().unwrap_or(DEFAULT_BASE_BRANCH);
        if let Err(e) = db.set_session_base_branch(session_id, base) {
            tracing::warn!("Failed to record session base branch: {e}");
        }
    }

    // No spawn-time status seed: a fresh session is `Idle` (the hooks-driven
    // default) until the agent's hooks report otherwise — e.g. claude's
    // SessionStart → idle on boot, then working/blocked/done through the turn.
    // Seeding `working` here made an idle, just-booted agent look stuck working.

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
///
/// Shared with [`super::restart`] so the headless restart launches a multi-repo
/// session in its symlink workspace, not the primary repo.
pub(crate) fn resolve_launch_cwd(
    agent_session_id: &str,
    primary_cwd: &std::path::Path,
    worktrees: &[SharedWorktree],
    additional_dirs: &[PathBuf],
    host: Option<&HostDef>,
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
    // A remote session's workspace must be built on the *remote* host — a local
    // symlink dir wouldn't exist there. Local sessions use the local builder.
    let built = match host {
        Some(h) => crate::git::ensure_remote_workspace(h, agent_session_id, &members),
        None => crate::workspace::ensure_workspace(agent_session_id, &members)
            .map_err(anyhow::Error::from),
    };
    match built {
        Ok(ws) => ws,
        Err(e) => {
            tracing::error!("Failed to build multi-repo workspace: {e}");
            primary_cwd.to_path_buf()
        }
    }
}

/// Adapt agent `args` that reference thurbox-managed config files (by their
/// *local* absolute path) for a spawn on the remote `host`, returning the args
/// to actually launch with. An agent handed a path that doesn't exist on the
/// host errors out and the pane dies instantly (claude: "Settings file not
/// found"), so an unresolvable path must never reach the remote launch:
///
/// - **Translate + materialize** (POSIX remotes): rewrite a home-anchored
///   config path onto the *remote* home (identity when the homes agree), copy
///   the local file to that remote path, and substitute the rewritten arg.
/// - **Strip as fallback**: on a `psmux` host (native Windows — no POSIX side
///   to copy to and hook commands are `sh` syntax anyway), a non-POSIX local
///   config root, a config path outside the local home that a home-translation
///   can't map, or a failed remote copy/home lookup, drop the path **and its
///   preceding flag** (e.g. the whole `--settings <path>` pair) with a warning
///   so the agent launches clean instead of dead.
///
/// Scope is deliberately narrow: only paths under the **thurbox config dir**
/// are touched (and only existing local files are copied), so an arbitrary
/// path in the agent's own args — a repo path, a user file — is never
/// rewritten or shipped. Each shipped file also has its thurbox-managed hook
/// commands rewritten for the host
/// ([`super::builtin_hooks::rewrite_hook_signals_for_remote`]): the local
/// `thurbox-cli session signal` can't work there, but a tmux pane user option
/// can — the local TUI receives it over its control-mode subscription, so
/// remote sessions get live hooks-driven status.
///
/// Shared by the headless spawn and the TUI (`App::build_spawn_inputs`) so
/// both paths launch a remote session with the same args.
pub(crate) fn adapt_agent_args_for_remote(host: &HostDef, args: Vec<String>) -> Vec<String> {
    let Some(config_root) = crate::paths::config_file()
        .and_then(|p| p.parent().map(|d| d.to_string_lossy().into_owned()))
    else {
        return args;
    };
    // Resolve the translation target lazily (one ssh round-trip) and at most
    // once; `None` = strip mode.
    let mut remote_root: Option<Option<String>> = None;
    rewrite_config_path_args(args, &config_root, |local_path| {
        let root = remote_root
            .get_or_insert_with(|| remote_config_root(host, &config_root))
            .clone()?;
        let remote_path = format!("{root}{}", &local_path[config_root.len()..]);
        // Read failure (missing/unreadable/non-file) → strip, as before.
        let contents = std::fs::read_to_string(local_path).ok()?;
        let contents = super::builtin_hooks::rewrite_hook_signals_for_remote(&contents);
        match crate::git::copy_bytes_to_remote(host, contents.as_bytes(), &remote_path) {
            Ok(()) => Some(remote_path),
            Err(e) => {
                tracing::warn!(
                    "failed to materialize agent config {local_path} on host '{}': {e:#}",
                    host.name
                );
                None
            }
        }
    })
}

/// Where the local thurbox config root lands on `host`, or `None` when no
/// remote location can hold it (→ strip the args instead):
/// - a non-POSIX (Windows `C:\…`) local root or a `psmux` host can't take a
///   POSIX copy at all;
/// - a home-anchored root translates onto the **remote** home (identity when
///   local and remote `$HOME` agree — the common same-user WSL/devbox case);
/// - a root outside the local home is mirrored at the same absolute path.
fn remote_config_root(host: &HostDef, config_root: &str) -> Option<String> {
    if !config_root.starts_with('/') || host.mux() == "psmux" {
        tracing::warn!(
            "stripping local agent-config args for host '{}': no POSIX path for the \
             thurbox config dir there",
            host.name
        );
        return None;
    }
    let Some(local_home) = crate::paths::home_dir() else {
        return Some(config_root.to_string());
    };
    let local_home = local_home.to_string_lossy().into_owned();
    let Some(suffix) = config_root.strip_prefix(&local_home) else {
        return Some(config_root.to_string());
    };
    match crate::git::remote_home(host) {
        Ok(remote_home) => Some(format!("{remote_home}{suffix}")),
        Err(e) => {
            tracing::warn!(
                "stripping local agent-config args for host '{}': cannot resolve remote \
                 home: {e:#}",
                host.name
            );
            None
        }
    }
}

/// Pure arg-rewriting core of [`adapt_agent_args_for_remote`]: every arg (or
/// `--flag=value` value) under `config_root` is passed to `map`; `Some(new)`
/// substitutes the path, `None` drops the arg **and** its preceding token when
/// that token is a flag (so a `--settings <path>` pair vanishes together).
fn rewrite_config_path_args(
    args: Vec<String>,
    config_root: &str,
    mut map: impl FnMut(&str) -> Option<String>,
) -> Vec<String> {
    let mut out: Vec<String> = Vec::with_capacity(args.len());
    for arg in args {
        if arg.starts_with(config_root) {
            match map(&arg) {
                Some(new) => out.push(new),
                None => {
                    tracing::warn!("dropping agent arg for remote spawn: {arg}");
                    if out.last().is_some_and(|prev| prev.starts_with('-')) {
                        out.pop();
                    }
                }
            }
            continue;
        }
        // `--flag=<path>` form: rewrite the value in place, or drop the whole
        // token (it is self-contained — nothing precedes it to pop).
        if let Some((flag, value)) = arg.split_once('=') {
            if flag.starts_with('-') && value.starts_with(config_root) {
                match map(value) {
                    Some(new) => out.push(format!("{flag}={new}")),
                    None => tracing::warn!("dropping agent arg for remote spawn: {arg}"),
                }
                continue;
            }
        }
        out.push(arg);
    }
    out
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
/// `None`/empty → the local backend. A named host must exist in `hosts.toml`
/// **or** be an auto-discovered local WSL distro, otherwise an error is
/// returned listing the available hosts.
fn resolve_host(host_name: Option<&str>) -> Result<(String, Option<HostDef>), String> {
    let Some(name) = host_name.filter(|n| !n.is_empty()) else {
        return Ok((LOCAL_TMUX_BACKEND_TYPE.to_string(), None));
    };
    let registry = crate::agent::host_config::load_all();
    match registry.get(name) {
        Some(h) => Ok((h.backend_name(), Some(h.clone()))),
        None => {
            let available = registry.names().join(", ");
            Err(format!(
                "Unknown host '{name}'. Configure it in hosts.toml (or check the \
                 WSL distro name). Available: [{available}]"
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::DEFAULT_AGENT_NAME;
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
    fn adapt_agent_args_is_identity_without_config_paths() {
        // No arg references the thurbox config dir → args pass through
        // untouched and, because the remote root is resolved lazily, no ssh
        // round-trip is attempted (the host here doesn't exist).
        let temp = tempfile::TempDir::new().unwrap();
        let _guard = crate::paths::TestPathGuard::new(temp.path());
        let host = HostDef {
            name: "nonexistent-host".into(),
            destination: "user@nonexistent-host".into(),
            ..Default::default()
        };
        let args: Vec<String> = ["--session-id", "abc", "--model", "opus"]
            .map(String::from)
            .into();
        assert_eq!(adapt_agent_args_for_remote(&host, args.clone()), args);
    }

    #[test]
    fn rewrite_config_args_substitutes_translated_path() {
        let args: Vec<String> = ["--settings", "/home/a/.config/thurbox/hooks/claude.json"]
            .map(String::from)
            .into();
        let out = rewrite_config_path_args(args, "/home/a/.config/thurbox", |p| {
            Some(p.replace("/home/a/", "/home/b/"))
        });
        assert_eq!(
            out,
            ["--settings", "/home/b/.config/thurbox/hooks/claude.json"].map(String::from)
        );
    }

    #[test]
    fn rewrite_config_args_strips_flag_and_path_pair() {
        // When no remote path can work, the path AND its `--settings` flag must
        // both vanish — leaving a dangling flag would eat the next arg.
        let args: Vec<String> = [
            "--verbose",
            "--settings",
            "/home/a/.config/thurbox/hooks/claude.json",
            "--session-id",
            "x",
        ]
        .map(String::from)
        .into();
        let out = rewrite_config_path_args(args, "/home/a/.config/thurbox", |_| None);
        assert_eq!(out, ["--verbose", "--session-id", "x"].map(String::from));
    }

    #[test]
    fn rewrite_config_args_handles_equals_form_and_positional() {
        // `--flag=<path>` rewrites in place / drops as one token; a positional
        // config path (no preceding flag) drops alone.
        let args: Vec<String> = ["--settings=/cfg/hooks/x.json", "/cfg/seed.toml"]
            .map(String::from)
            .into();
        let rewritten =
            rewrite_config_path_args(args.clone(), "/cfg", |p| Some(format!("/rem{p}")));
        assert_eq!(
            rewritten,
            ["--settings=/rem/cfg/hooks/x.json", "/rem/cfg/seed.toml"].map(String::from)
        );
        let stripped = rewrite_config_path_args(args, "/cfg", |_| None);
        assert!(stripped.is_empty());
    }

    #[test]
    fn rewrite_config_args_leaves_unrelated_args_untouched() {
        let args: Vec<String> = ["--model", "opus", "--add-dir", "/home/a/repo"]
            .map(String::from)
            .into();
        let out = rewrite_config_path_args(args.clone(), "/home/a/.config/thurbox", |_| {
            panic!("map must not be called for non-config args")
        });
        assert_eq!(out, args);
    }

    #[test]
    fn resolve_agent_def_derives_name_with_fallback() {
        let temp = tempfile::TempDir::new().unwrap();
        let _guard = crate::paths::TestPathGuard::new(temp.path());
        // An explicit, seeded agent wins and its name round-trips.
        assert_eq!(super::super::resolve_agent_def(Some("codex")).name, "codex");
        // Empty/None/unknown fall back to the registry default.
        assert_eq!(
            super::super::resolve_agent_def(Some("")).name,
            DEFAULT_AGENT_NAME
        );
        assert_eq!(
            super::super::resolve_agent_def(None).name,
            DEFAULT_AGENT_NAME
        );
        assert_eq!(
            super::super::resolve_agent_def(Some("no-such-agent")).name,
            DEFAULT_AGENT_NAME
        );
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
        let got = resolve_launch_cwd("sid-1", &primary, &[], &[], None);
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
        let got = resolve_launch_cwd("sid-multi", &primary, &[], &[extra], None);
        // Two members → a symlink workspace, not the primary itself.
        assert_ne!(got, primary);
        assert!(got.join("primary").exists() || got.exists());
    }
}
