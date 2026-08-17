//! Headless session spawn — creates a local-tmux session without requiring
//! the TUI event loop.

use std::path::PathBuf;

use crate::session::{psmux_hook_rewrite_supported, ExtraRepo, HostDef, SessionConfig, SessionId};
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
    /// The agent conversation this session forks from.
    ///
    /// Set only by a fork: it drives the agent's `fork_args`
    /// (`--resume {id} --fork-session`), which is what makes the new session
    /// start from the parent's conversation rather than an empty one. Without
    /// it a "fork" is just a second session in the same directory.
    pub fork_session_id: Option<String>,
    /// Worktrees the new session **shares** with another rather than creating.
    ///
    /// A fork launches in its parent's worktree, so no checkout is made — but the
    /// row still has to list it, or the fork shows no branch, offers nothing to
    /// sync, and looks like a session with no work in it. v1 carries the parent's
    /// worktree list onto the fork for the same reason.
    pub inherit_worktrees: Vec<SharedWorktree>,
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
/// A stage of the spawn pipeline, for callers that want to render progress.
///
/// Creating a session runs for tens of seconds on a large repository, and the
/// slow part is not always the same one — a stalled fetch and a stalled ssh
/// connect look identical from outside. Naming the stage is what lets a caller
/// say *which*.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpawnPhase {
    /// Resolving the agent definition and the host.
    Resolving,
    /// Creating or attaching worktrees — the `git fetch` and checkout.
    Worktrees,
    /// Readying the backend: for a remote host, the ssh connect.
    Backend,
    /// Launching the agent process.
    Launching,
    /// Persisting the session row.
    Persisting,
}

impl SpawnPhase {
    pub fn as_str(self) -> &'static str {
        match self {
            SpawnPhase::Resolving => "resolving",
            SpawnPhase::Worktrees => "worktrees",
            SpawnPhase::Backend => "backend",
            SpawnPhase::Launching => "launching",
            SpawnPhase::Persisting => "persisting",
        }
    }
}

/// Told which stage the pipeline has reached. Called on the spawning thread.
pub type ProgressFn<'a> = &'a (dyn Fn(SpawnPhase) + Send + Sync);

pub fn spawn_session_headless(db: &Database, req: SpawnRequest) -> Result<SpawnResult, String> {
    spawn_session_headless_with_progress(db, req, None)
}

/// [`spawn_session_headless`], reporting each stage as it is reached.
///
/// The reporting form is separate so every existing caller — `thurbox-cli`, the
/// automation runner — is untouched and keeps its signature.
pub fn spawn_session_headless_with_progress(
    db: &Database,
    req: SpawnRequest,
    progress: Option<ProgressFn<'_>>,
) -> Result<SpawnResult, String> {
    let report = |phase: SpawnPhase| {
        if let Some(progress) = progress {
            progress(phase);
        }
    };
    report(SpawnPhase::Resolving);
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
    // them for the host — materialize the file remotely (translating a
    // home-anchored path to the remote home) or, when that's impossible, strip
    // the flag so the agent at least launches — and provision the agent's
    // config-dir hook files on the host so it reports status remotely.
    // Degradation is best-effort-logged (headless has no info panel).
    let hooks_enabled = !db.builtin_hooks_opted_out().unwrap_or(false);
    let (adapted, hook_degraded) = adapt_def_for_launch(agent_def, host.as_ref(), hooks_enabled);
    agent_def = adapted;
    if let Some(reason) = hook_degraded {
        tracing::warn!("remote hook wiring degraded for '{}': {reason}", req.name);
    }
    report(SpawnPhase::Worktrees);
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
        // What makes a fork a fork: the agent is told to resume the parent's
        // conversation into a new one.
        fork_session_id: req.fork_session_id.clone(),
        ..SessionConfig::default()
    };
    super::inject_thurbox_env(&mut config, &agent_session_id, req.task_id);

    report(SpawnPhase::Backend);
    let (command, args) = super::build_agent_invocation(&agent_def, &config);

    report(SpawnPhase::Launching);
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

    report(SpawnPhase::Persisting);
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

    // Shared, not created: a fork's worktree already exists and belongs to its
    // parent. Appended last so the primary stays first.
    for inherited in &req.inherit_worktrees {
        if !worktrees
            .iter()
            .any(|existing| existing.worktree_path == inherited.worktree_path)
        {
            worktrees.push(inherited.clone());
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
    build_multi_repo_workspace(host, agent_session_id, &members)
        .unwrap_or_else(|| primary_cwd.to_path_buf())
}

/// Build the per-session multi-repo symlink workspace — on the *remote* host
/// when one is given (a local symlink dir wouldn't exist there), else with the
/// local builder. `None` (error already logged) tells the caller to fall back
/// to its primary cwd. Single home for the host branching + fallback policy,
/// shared by [`resolve_launch_cwd`] and the TUI's `App::resolve_process_cwd`.
pub(crate) fn build_multi_repo_workspace(
    host: Option<&HostDef>,
    agent_session_id: &str,
    members: &[(String, PathBuf)],
) -> Option<PathBuf> {
    let built = match host {
        Some(h) => crate::git::ensure_remote_workspace(h, agent_session_id, members),
        None => crate::workspace::ensure_workspace(agent_session_id, members)
            .map_err(anyhow::Error::from),
    };
    match built {
        Ok(ws) => Some(ws),
        Err(e) => {
            tracing::error!("Failed to build multi-repo workspace: {e}");
            None
        }
    }
}

/// Where an **already-running** session's processes live: the same answer
/// `resolve_launch_cwd` gives, without building anything.
///
/// The non-building half of the pair, and the distinction is load-bearing: the
/// agent is *in* the workspace, and the ensure-style rebuild would `rm -rf` its
/// cwd out from under it. So this only ever names the directory. Wanted when
/// something joins a session that is already running — opening its companion
/// shell, which is `kernel::terminal`'s caller. v1 answers the same question in
/// `App::session_process_cwd_existing`; folding that one in here is left for
/// when `src/app/` is next opened, which parity has not reached yet.
///
/// `members` is the count, not the list, because every caller has already built
/// the member set for its own reasons and only the ≥2 test is wanted here.
/// Falls back to `primary_cwd` whenever the workspace cannot be named.
pub fn existing_launch_cwd(
    agent_session_id: Option<&str>,
    primary_cwd: Option<&std::path::Path>,
    members: usize,
    host: Option<&HostDef>,
) -> Option<PathBuf> {
    let primary = || primary_cwd.map(std::path::Path::to_path_buf);
    if members < 2 {
        return primary();
    }
    let Some(id) = agent_session_id else {
        return primary();
    };
    let named = match host {
        // Pure string work unless the host has no `worktrees_dir`, and then it
        // is the `$HOME` lookup every other remote path already caches.
        Some(host) => crate::git::remote_workspace_dir(host, id)
            .map(PathBuf::from)
            .map_err(|e| tracing::error!("could not name the remote workspace: {e:#}")),
        None => crate::workspace::workspace_path(id)
            .map_err(|e| tracing::error!("could not name the workspace: {e}")),
    };
    named.ok().or_else(primary)
}

/// Adapt agent `args` that reference thurbox-managed config files (by their
/// *local* absolute path) for a spawn on the remote `host`, returning the args
/// to actually launch with. An agent handed a path that doesn't exist on the
/// host errors out and the pane dies instantly (claude: "Settings file not
/// found"), so an unresolvable path must never reach the remote launch:
///
/// - **Translate + materialize** (POSIX remotes): rewrite the config path onto
///   a location the host can hold ([`remote_config_root`] — the *remote* home
///   for a home-anchored POSIX root, `$HOME/.config/<root-name>` for a Windows
///   one), copy the local file there, and substitute the rewritten arg.
/// - **Strip as fallback**: on a `psmux` host (native Windows, while
///   [`psmux_hook_rewrite_supported`] stays off), a POSIX config path outside
///   the local home that a home-translation can't map, or a failed remote
///   copy/home lookup, drop the path **and its preceding flag** (e.g. the whole
///   `--settings <path>` pair) with a warning so the agent launches clean
///   instead of dead.
///
/// Scope is deliberately narrow: only paths under the **thurbox config dir**
/// are touched (and only existing local files are copied), so an arbitrary
/// path in the agent's own args — a repo path, a user file — is never
/// rewritten or shipped. Each shipped file also has its thurbox-managed hook
/// commands rewritten for the host
/// ([`super::builtin_hooks::rewrite_hook_signals_for_target`]): the local
/// `thurbox-cli session signal` can't work there, but a tmux pane user option
/// can — the local TUI receives it over its control-mode subscription (tmux)
/// or pane-option polling (psmux), so remote sessions get live hooks-driven
/// status. The same rewrite maps over every **literal** arg too, so a hook
/// command carried directly in the args (aider's
/// `--notifications-command "thurbox-cli session signal --state blocked"`)
/// also reports remotely instead of invoking a CLI that isn't there — it is
/// marker-keyed and idempotent, so non-matching args pass through
/// byte-identical.
///
/// Shared by the headless spawn and the TUI (`App::build_spawn_inputs`) so
/// both paths launch a remote session with the same args.
pub(crate) fn adapt_agent_args_for_remote(host: &HostDef, args: Vec<String>) -> Vec<String> {
    adapt_agent_args_for_remote_with_report(host, args).0
}

/// [`adapt_agent_args_for_remote`] plus the list of local config paths that
/// were **stripped** (no remote location could hold them) — the caller
/// surfaces those as a hook-wiring degradation, since a stripped hooks config
/// means the session reports no status.
pub(crate) fn adapt_agent_args_for_remote_with_report(
    host: &HostDef,
    args: Vec<String>,
) -> (Vec<String>, Vec<String>) {
    let target = super::builtin_hooks::remote_signal_target(host);
    let rewrite_literals = |args: Vec<String>| -> Vec<String> {
        args.into_iter()
            .map(|a| super::builtin_hooks::rewrite_hook_signals_for_target(&a, &target))
            .collect()
    };
    let Some(config_root) = crate::paths::config_file()
        .and_then(|p| p.parent().map(|d| d.to_string_lossy().into_owned()))
    else {
        return (rewrite_literals(args), Vec::new());
    };
    // Resolve the translation target lazily (one ssh round-trip) and at most
    // once; `None` = strip mode.
    let mut remote_root: Option<Option<String>> = None;
    let mut stripped: Vec<String> = Vec::new();
    let args = rewrite_config_path_args(args, &config_root, |local_path| {
        let materialized = (|| {
            let root = remote_root
                .get_or_insert_with(|| remote_config_root(host, &config_root))
                .clone()?;
            let remote_path = remote_config_path(&root, &config_root, local_path);
            // Read failure (missing/unreadable/non-file) → strip, as before.
            let contents = std::fs::read_to_string(local_path).ok()?;
            let contents =
                super::builtin_hooks::rewrite_hook_signals_for_target(&contents, &target);
            // A psmux host is native Windows — no `sh`/`cat` for the POSIX
            // stream copy, so the payload goes via the PowerShell variant.
            let copied = if host.mux() == "psmux" {
                crate::git::copy_bytes_to_remote_windows(host, contents.as_bytes(), &remote_path)
            } else {
                crate::git::copy_bytes_to_remote(host, contents.as_bytes(), &remote_path)
            };
            match copied {
                Ok(()) => Some(remote_path),
                Err(e) => {
                    tracing::warn!(
                        "failed to materialize agent config {local_path} on host '{}': {e:#}",
                        host.name
                    );
                    None
                }
            }
        })();
        if materialized.is_none() {
            stripped.push(local_path.to_string());
        }
        materialized
    });
    (rewrite_literals(args), stripped)
}

/// Adapt an agent def for launch on an optional remote host: rewrite/ship its
/// args ([`adapt_agent_args_for_remote_with_report`]) and provision the
/// agent's config-dir hook files there
/// ([`super::remote_hooks::provision_agent_hooks_on_host`]). Returns the
/// adapted def plus a human-readable note when remote hooks-driven status
/// will be degraded/absent. Performs ssh round-trips for a remote host — call
/// on a worker, never the UI thread (ADR-P12). Identity for a local launch.
pub(crate) fn adapt_def_for_launch(
    mut def: crate::session::AgentDef,
    host: Option<&HostDef>,
    hooks_enabled: bool,
) -> (
    crate::session::AgentDef,
    super::remote_hooks::HookDegradation,
) {
    // Expand `{home}` in every arg group before anything else so a session-path
    // agent (omp) launches against a concrete, quote-safe absolute path — the
    // local home for a local launch, the resolved remote home for an SSH/WSL
    // host. A def with no `{home}` (every built-in but omp) is untouched.
    let Some(h) = host else {
        if let Some(home) = crate::paths::home_dir() {
            super::expand_home_in_def(&mut def, &home.to_string_lossy());
        }
        return (def, None);
    };
    if let Some(home) = resolve_launch_home(h) {
        super::expand_home_in_def(&mut def, &home);
    } else if def_references_home(&def) {
        // A session-path arg still carries a literal `{home}` — the agent would
        // create a file named "{home}" in the cwd. Better to surface it.
        tracing::warn!(
            "could not resolve remote home for host '{}'; agent '{}' may launch \
             with an unexpanded {{home}} in its session path",
            h.name,
            def.name
        );
    }
    let (args, stripped) = adapt_agent_args_for_remote_with_report(h, def.args);
    def.args = args;
    // A stripped config path outranks a provisioning note: it means the
    // agent's own hooks file never reached the host at all.
    let degraded = if stripped.is_empty() {
        super::remote_hooks::provision_agent_hooks_on_host(h, &def.name, hooks_enabled)
    } else {
        Some(format!(
            "hooks config stripped for host '{}' (no status): {}",
            h.name,
            stripped.join(", ")
        ))
    };
    (def, degraded)
}

/// The home dir to expand `{home}` against for a launch on `host`: the remote
/// `$HOME` for a POSIX SSH/WSL host, or the Windows home for a psmux host.
/// `None` when it can't be resolved (host down, no client) — the caller then
/// leaves the token unexpanded and warns.
pub(crate) fn resolve_launch_home(host: &HostDef) -> Option<String> {
    let result = if host.mux() == "psmux" {
        crate::git::remote_home_windows(host)
    } else {
        crate::git::remote_home(host)
    };
    match result {
        Ok(home) => Some(home),
        Err(e) => {
            tracing::warn!("cannot resolve home for host '{}': {e:#}", host.name);
            None
        }
    }
}

/// Whether any of `def`'s arg groups still carries an unexpanded `{home}`.
fn def_references_home(def: &crate::session::AgentDef) -> bool {
    let has = |v: &[String]| v.iter().any(|t| t.contains(super::HOME_PLACEHOLDER));
    has(&def.args) || has(&def.resume_args) || has(&def.fork_args) || has(&def.new_session_args)
}

/// Where the local thurbox config root lands on `host`, or `None` when no
/// remote location can hold it (→ strip the args instead):
/// - a `psmux` (native-Windows) host maps onto
///   `%USERPROFILE%/.config/<root-name>` — dev/release isolation carries over
///   via the root's final component — but only once
///   [`psmux_hook_rewrite_supported`] is flipped;
/// - a **Windows-local** root (`C:\Users\me\AppData\Roaming\thurbox`, a Windows
///   TUI driving a POSIX host) has no absolute counterpart to mirror, so it
///   maps onto `$HOME/.config/<root-name>` there — same shape as the psmux
///   branch, and the case a WSL session on Windows always hits;
/// - a home-anchored POSIX root translates onto the **remote** home (identity
///   when local and remote `$HOME` agree — the common same-user WSL/devbox
///   case);
/// - a POSIX root outside the local home is mirrored at the same absolute path.
fn remote_config_root(host: &HostDef, config_root: &str) -> Option<String> {
    if host.mux() == "psmux" {
        if !psmux_hook_rewrite_supported() {
            tracing::warn!(
                "stripping local agent-config args for host '{}': psmux hook rewrite \
                 is not enabled",
                host.name
            );
            return None;
        }
        return match crate::git::remote_home_windows(host) {
            Ok(home) => Some(format!("{home}/.config/{}", root_leaf_name(config_root))),
            Err(e) => {
                tracing::warn!(
                    "stripping local agent-config args for host '{}': cannot resolve \
                     Windows home: {e:#}",
                    host.name
                );
                None
            }
        };
    }
    // A Windows local root on a POSIX host: nothing to mirror or home-translate
    // (`C:\…` shares no prefix with the local home), so land it under the remote
    // home the way the psmux branch does. Before this, such a root was stripped —
    // and, because `path_under_root` only knew `/`, the `--settings` arg wasn't
    // even recognized as a config path, so claude launched inside WSL against a
    // literal `C:\…` path and died on "Settings file not found" (issue #933).
    if !is_posix_root(config_root) {
        return match crate::git::remote_home(host) {
            Ok(home) => Some(format!("{home}/.config/{}", root_leaf_name(config_root))),
            Err(e) => {
                tracing::warn!(
                    "stripping local agent-config args for host '{}': cannot resolve \
                     remote home for the Windows config dir: {e:#}",
                    host.name
                );
                None
            }
        };
    }
    let Some(local_home) = crate::paths::home_dir() else {
        return Some(config_root.to_string());
    };
    let local_home = local_home.to_string_lossy().into_owned();
    // Boundary-checked: home `/home/a` must not claim `/home/abc/…` (the
    // stripped suffix would splice a garbage sibling path onto the remote
    // home). Such a root is mirrored at its absolute path instead.
    let Some(suffix) = config_root
        .strip_prefix(&local_home)
        .filter(|s| s.starts_with('/'))
    else {
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

/// Whether the *local* config root is a POSIX absolute path. A root that isn't
/// (a Windows `C:\…` one) separates components with `\` as well as `/`
/// ([`path_under_root`], [`remote_config_path`]) and has no absolute
/// counterpart on a POSIX host ([`remote_config_root`]).
fn is_posix_root(root: &str) -> bool {
    root.starts_with('/')
}

/// The final component of a local config root (`thurbox` / `thurbox-dev`, which
/// carries dev/release isolation), split on either separator so a Windows root
/// resolves the same when the code runs on a POSIX host (tests) as on Windows —
/// `Path::file_name` would return the whole `C:\…` string there.
fn root_leaf_name(root: &str) -> &str {
    root.trim_end_matches(['/', '\\'])
        .rsplit(['/', '\\'])
        .next()
        .filter(|n| !n.is_empty())
        .unwrap_or("thurbox")
}

/// True when `path` is `root` itself or a descendant — a plain
/// `starts_with` would also claim sibling dirs sharing the prefix
/// (`…/thurbox-backup` under root `…/thurbox`). A Windows root also accepts `\`
/// as the boundary (thurbox's own args mix them: `{home}/claude.json` appended
/// to a `C:\…` home); on a POSIX root it can't, since `\` is a legal filename
/// character there.
fn path_under_root(path: &str, root: &str) -> bool {
    let windows_root = !is_posix_root(root);
    path.strip_prefix(root).is_some_and(|rest| {
        rest.is_empty() || rest.starts_with('/') || (windows_root && rest.starts_with('\\'))
    })
}

/// Splice `local_path`'s tail (relative to the local `config_root`) onto the
/// resolved `remote_root`. A Windows local root's `\` separators are normalized
/// to `/` — the tail has to be a path on the *host* (POSIX, or PowerShell,
/// which accepts `/`); a POSIX root's tail is passed through byte-identical
/// (a `\` there is part of a filename).
fn remote_config_path(remote_root: &str, config_root: &str, local_path: &str) -> String {
    // Callers reach here only for a path [`path_under_root`] accepted, so the
    // prefix always strips; `unwrap_or_default` keeps it panic-free either way.
    let tail = local_path.strip_prefix(config_root).unwrap_or_default();
    if is_posix_root(config_root) {
        return format!("{remote_root}{tail}");
    }
    format!("{remote_root}{}", tail.replace('\\', "/"))
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
        if path_under_root(&arg, config_root) {
            match map(&arg) {
                Some(new) => out.push(new),
                None => {
                    tracing::warn!("dropping agent arg for remote spawn: {arg}");
                    // A `--flag=value` token is self-contained — never a
                    // dangling flag for the dropped path — so popping it would
                    // eat an unrelated (possibly already-rewritten) arg.
                    if out
                        .last()
                        .is_some_and(|prev| prev.starts_with('-') && !prev.contains('='))
                    {
                        out.pop();
                    }
                }
            }
            continue;
        }
        // `--flag=<path>` form: rewrite the value in place, or drop the whole
        // token (it is self-contained — nothing precedes it to pop).
        if let Some((flag, value)) = arg.split_once('=') {
            if flag.starts_with('-') && path_under_root(value, config_root) {
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
            fork_session_id: None,
            inherit_worktrees: Vec::new(),
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
    fn adapt_agent_args_rewrites_literal_signal_commands() {
        // aider carries its hook as a literal arg (`--notifications-command
        // "thurbox-cli session signal --state blocked"`), not a config-file
        // path — the remote adaptation must rewrite it to the pane-option form
        // or the host invokes a CLI that isn't there.
        let temp = tempfile::TempDir::new().unwrap();
        let _guard = crate::paths::TestPathGuard::new(temp.path());
        let host = HostDef {
            name: "devbox".into(),
            destination: "user@devbox".into(),
            ..Default::default()
        };
        let args: Vec<String> = [
            "--notifications",
            "--notifications-command",
            "thurbox-cli session signal --state blocked",
        ]
        .map(String::from)
        .into();
        let out = adapt_agent_args_for_remote(&host, args.clone());
        assert_eq!(
            out,
            [
                "--notifications",
                "--notifications-command",
                "tmux set-option -p @thurbox_state blocked",
            ]
            .map(String::from)
        );

        // A psmux host gets the socket-explicit psmux form (psmux has no
        // in-pane `$TMUX` socket resolution).
        let psmux_host = HostDef {
            name: "winbox".into(),
            destination: "user@winbox".into(),
            multiplexer: Some("psmux".into()),
            socket: Some("tb".into()),
            ..Default::default()
        };
        let out = adapt_agent_args_for_remote(&psmux_host, args);
        assert_eq!(out[2], "psmux -L tb set-option -p @thurbox_state blocked");
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
    fn rewrite_config_args_never_pops_a_self_contained_equals_token() {
        // A `--flag=value` token before a stripped positional path is complete
        // on its own — even when it was itself just rewritten — and must
        // survive the pop that removes a dangling value-taking flag.
        let args: Vec<String> = ["--settings=/cfg/a.json", "/cfg/b.json"]
            .map(String::from)
            .into();
        let out = rewrite_config_path_args(args, "/cfg", |p| {
            (p == "/cfg/a.json").then(|| format!("/rem{p}"))
        });
        assert_eq!(out, ["--settings=/rem/cfg/a.json"].map(String::from));
    }

    #[test]
    fn rewrite_config_args_ignores_sibling_prefixed_paths() {
        // `/cfg-backup` shares the `/cfg` string prefix but is not under the
        // config root — it must pass through untouched, not be shipped/stripped.
        let args: Vec<String> = ["--settings", "/cfg-backup/notes.md"]
            .map(String::from)
            .into();
        let out = rewrite_config_path_args(args.clone(), "/cfg", |_| {
            panic!("map must not be called for a sibling-prefixed path")
        });
        assert_eq!(out, args);
    }

    #[test]
    fn windows_config_root_arg_is_recognized_and_normalized() {
        // Regression, issue #933: a Windows TUI spawning a WSL/SSH session. The
        // hooks patch appends `{home}/claude.json` to a `C:\…` extension home,
        // so the arg mixes separators — it must still be seen as a config path
        // (it was not, so the literal `C:\…` reached claude inside the distro
        // and the pane died on "Settings file not found") and the shipped
        // remote path must come out fully POSIX.
        let root = r"C:\Users\me\AppData\Roaming\thurbox";
        let local = r"C:\Users\me\AppData\Roaming\thurbox\hooks/claude.json";
        assert!(path_under_root(local, root));
        assert_eq!(
            remote_config_path("/home/me/.config/thurbox", root, local),
            "/home/me/.config/thurbox/hooks/claude.json"
        );

        let args: Vec<String> = ["--settings", local].map(String::from).into();
        let out = rewrite_config_path_args(args, root, |p| {
            Some(remote_config_path("/home/me/.config/thurbox", root, p))
        });
        assert_eq!(
            out,
            ["--settings", "/home/me/.config/thurbox/hooks/claude.json"].map(String::from)
        );
    }

    #[test]
    fn windows_config_root_ignores_sibling_and_keeps_dev_isolation() {
        let root = r"C:\Users\me\AppData\Roaming\thurbox";
        // Sibling dirs sharing the string prefix are not under the root.
        assert!(!path_under_root(
            r"C:\Users\me\AppData\Roaming\thurbox-backup\x.json",
            root
        ));
        // The leaf name carries dev/release isolation into the remote root, on
        // either separator (and whatever platform the test runs on).
        assert_eq!(root_leaf_name(root), "thurbox");
        assert_eq!(
            root_leaf_name(r"C:\Users\me\AppData\Roaming\thurbox-dev"),
            "thurbox-dev"
        );
        assert_eq!(
            root_leaf_name("/home/me/.config/thurbox-dev"),
            "thurbox-dev"
        );
    }

    #[test]
    fn posix_root_treats_backslash_as_a_filename_char() {
        // `\` is legal in a POSIX filename, so it must not act as a boundary
        // (that would ship/strip an unrelated sibling file) nor be rewritten in
        // a translated path.
        assert!(!path_under_root(r"/cfg\backup/x.json", "/cfg"));
        assert_eq!(
            remote_config_path("/rem/cfg", "/cfg", r"/cfg/we\ird.json"),
            r"/rem/cfg/we\ird.json"
        );
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

    #[test]
    fn existing_launch_cwd_names_the_workspace_without_building_it() {
        // The property that separates it from its twin: the agent is already in
        // that directory, so naming it must not touch the filesystem.
        let temp = tempfile::TempDir::new().unwrap();
        let _guard = crate::paths::TestPathGuard::new(temp.path());
        let primary = temp.path().join("primary");

        let got = existing_launch_cwd(Some("sid-multi"), Some(&primary), 2, None)
            .expect("a directory to launch in");
        assert_ne!(
            got, primary,
            "a multi-repo session launches in its workspace"
        );
        assert!(
            !got.exists(),
            "naming it must not create it: {}",
            got.display()
        );
    }

    #[test]
    fn existing_launch_cwd_falls_back_when_the_workspace_cannot_be_named() {
        // No agent session id, so there is no workspace to name — and a shell in
        // the primary repository still beats one wherever the multiplexer was.
        let primary = PathBuf::from("/tmp/primary");
        let got = existing_launch_cwd(None, Some(&primary), 2, None);
        assert_eq!(got, Some(primary));
    }

    #[test]
    fn existing_launch_cwd_for_one_repository_is_that_repository() {
        let primary = PathBuf::from("/tmp/primary");
        let got = existing_launch_cwd(Some("sid-1"), Some(&primary), 1, None);
        assert_eq!(got, Some(primary));
    }
}
