//! Headless session operations — spawn and restart sessions without the TUI.
//!
//! Callers (MCP, CLI) use these helpers to drive the same local-tmux-backed
//! sessions the TUI manages, without requiring the TUI event loop. All
//! operations are synchronous against the SQLite database and the `tmux -L
//! thurbox` server.

pub mod builtin;
pub mod builtin_hooks;
pub mod builtin_ui_skill;
pub mod delete;
pub mod extensions;
pub mod host_cli;
pub mod lifecycle_hooks;
pub mod mirror;
pub mod remote_hooks;
pub mod restart;
pub mod restore;
#[cfg(test)]
mod shared_tests;
pub mod spawn;

pub use builtin::{builtin_extension, ensure_builtin_extensions, Builtin};
pub use builtin_hooks::hooks_enabled;
pub use delete::{delete_session_headless, reap_soft_deleted, ForceDeleteReport};
pub use extensions::{
    activate_extension, deactivate_extension, ensure_extension, extension_health,
    heal_active_extensions, install_extension, reinstall_extension, uninstall_extension,
    update_all_extensions, update_extension, DeactivateReport, EnsureReport, ExtensionHealth,
    InstallReport, ReinstallReport, UninstallReport, UpdateReport,
};
pub use lifecycle_hooks::{fire_post, fire_pre};
pub use restart::{restart_session_headless, RestartReport};
pub use restore::{restore_refusal, restore_session_headless, RestoreReport};
pub use spawn::{spawn_session_headless, SpawnRequest, SpawnResult};

use std::collections::{BTreeMap, HashMap};

use crate::session::agent_def::ID_PLACEHOLDER;
use crate::session::{AutomationRunStatus, SessionConfig};

/// Run an `Exec` automation's shell command headlessly (`sh -c`, or `cmd /C` on
/// Windows) and report its outcome for the run history. No session/agent is
/// involved — this is the deterministic-scheduled-job path shared by the TUI and
/// the headless `automation tick`. stdout+stderr are tail-truncated so a chatty
/// command can't bloat the history.
pub fn run_exec_command(command: &str) -> (AutomationRunStatus, String) {
    let out = match platform_shell(command).output() {
        Ok(out) => out,
        Err(e) => return (AutomationRunStatus::Error, format!("spawn failed: {e}")),
    };
    let mut detail = exec_tail(&out.stdout);
    let stderr = exec_tail(&out.stderr);
    if !stderr.is_empty() {
        if !detail.is_empty() {
            detail.push('\n');
        }
        detail.push_str(&stderr);
    }
    if out.status.success() {
        let msg = match detail.is_empty() {
            true => "ok".to_string(),
            false => detail,
        };
        return (AutomationRunStatus::Success, msg);
    }
    let code = out.status.code().map_or("signal".into(), |c| c.to_string());
    let msg = match detail.is_empty() {
        true => format!("exit {code}"),
        false => format!("exit {code}: {detail}"),
    };
    (AutomationRunStatus::Error, msg)
}

/// `command` as the platform shell runs it: `sh -c` or, on Windows, `cmd /C`.
/// Shared by `Exec` automations and lifecycle hooks so the two spell a
/// command line the same way.
pub(crate) fn platform_shell(command: &str) -> std::process::Command {
    if cfg!(windows) {
        let mut cmd = std::process::Command::new("cmd");
        cmd.args(["/C", command]);
        cmd
    } else {
        let mut cmd = std::process::Command::new("sh");
        cmd.args(["-c", command]);
        cmd
    }
}

/// The trailing 500 chars of one captured stream, trimmed. Single pass via a
/// capped ring so a huge stream isn't walked twice (count + skip) or
/// materialized in full.
pub(crate) fn exec_tail(stream: &[u8]) -> String {
    const TAIL_CHARS: usize = 500;
    let text = String::from_utf8_lossy(stream);
    let mut ring: std::collections::VecDeque<char> =
        std::collections::VecDeque::with_capacity(TAIL_CHARS + 1);
    for c in text.trim().chars() {
        if ring.len() == TAIL_CHARS {
            ring.pop_front();
        }
        ring.push_back(c);
    }
    ring.into_iter().collect()
}

/// Where this process resolved thurbox's state, as the env vars `thurbox-cli`
/// honours: the config/data dirs (`THURBOX_CONFIG_DIR` / `THURBOX_DATA_DIR`,
/// derived from the resolved file paths' parents, so a dev build, a sandbox and
/// a `THURBOX_*_DIR` override all hand the same answer on) and the multiplexer
/// socket those sessions live on (`THURBOX_SOCKET`). One definition, shared by
/// the agent's environment and a lifecycle hook — "a `thurbox-cli` inside hits
/// the right DB, and finds the right server" is one property, not two.
///
/// The socket is passed rather than left to be re-derived: the child would
/// otherwise recompute it (`agent::tmux::socket_for`) from an environment that
/// need not match this one — a tmux server carries the env it was started with,
/// which is the same reason the dirs are pinned here at all.
pub(crate) fn thurbox_env_overrides() -> Vec<(String, String)> {
    let mut vars = Vec::with_capacity(3);
    if let Some(dir) = crate::paths::config_file().and_then(|p| p.parent().map(|d| d.to_path_buf()))
    {
        vars.push((
            crate::paths::CONFIG_DIR_OVERRIDE_ENV.into(),
            dir.to_string_lossy().into(),
        ));
    }
    if let Some(dir) =
        crate::paths::database_file().and_then(|p| p.parent().map(|d| d.to_path_buf()))
    {
        vars.push((
            crate::paths::DATA_DIR_OVERRIDE_ENV.into(),
            dir.to_string_lossy().into(),
        ));
    }
    vars.push((
        crate::agent::tmux::SOCKET_OVERRIDE_ENV.into(),
        crate::agent::tmux::local_socket_name(),
    ));
    // Which instance that socket belongs to. A pane's `thurbox-cli` inherits
    // both, and they agree — but a child that relocates itself out of this
    // instance (a sandbox, `tests/`, an agent exporting its own
    // `THURBOX_DATA_DIR`) must not keep a socket naming *this* server. Pairing
    // the two is what lets `agent::tmux::socket_for` tell them apart.
    if let Some(dir) = crate::paths::data_directory() {
        vars.push((
            crate::agent::tmux::SOCKET_OWNER_ENV.into(),
            dir.to_string_lossy().into(),
        ));
    }
    vars
}

/// Decide whether to pass the agent's resume group vs starting fresh when
/// (re)spawning. Returns `Some(id.clone())` only when a Claude transcript for
/// that id already exists on disk under `CLAUDE_CONFIG_DIR`/`~/.claude`.
///
/// Claude-specific: only the `claude` agent persists resumable transcripts.
/// For agents without an on-disk transcript this returns `None`, which makes
/// restart start a fresh conversation (the live tmux process is what provides
/// cross-restart persistence in the general case).
///
/// Shared by [`restart::restart_session_headless`] and
/// `App::restart_active_session` so the headless and TUI paths agree.
pub(crate) fn resume_id_if_transcript_exists(
    agent_session_id: &str,
    env: &HashMap<String, String>,
) -> Option<String> {
    let config_dir_override = env.get("CLAUDE_CONFIG_DIR").map(std::path::PathBuf::from);
    crate::paths::claude_transcript_exists(agent_session_id, config_dir_override.as_deref())
        .then(|| agent_session_id.to_string())
}

/// Decide the `resume_session_id` to use when restarting a session, given the
/// agent's definition.
///
/// - Agents that resume "the latest session in the launch directory"
///   ([`AgentDef::resumes_latest`]) get the session id back as a non-`None`
///   *trigger*: their `resume_args` are id-less (no `{id}` token), so the value
///   itself is ignored — its presence is what makes [`AgentDef::build_args`]
///   emit the resume group. Restart always reuses the session's directory, so
///   the agent's own "last in cwd" resolution targets the right conversation.
/// - Everyone else (claude) falls back to the transcript check, which returns
///   the pinned id only when a resumable transcript exists on disk.
///
/// Shared by the headless restart path and `App`'s restart/restore paths so
/// they agree on when to resume vs. start fresh.
pub(crate) fn resume_trigger_for(
    def: &crate::session::AgentDef,
    agent_session_id: &str,
    env: &HashMap<String, String>,
) -> Option<String> {
    if def.resumes_latest() {
        return Some(agent_session_id.to_string());
    }
    // A path-pinned agent (omp) records its conversation at a deterministic file
    // thurbox chose — `--session {home}/.../thurbox-{id}.jsonl`. If that file
    // exists, resume it; the `resume_args` (`--resume <same path>`) reopen it
    // exactly. This must run before the claude-transcript check, which only
    // knows claude's `~/.claude` layout. The check is inherently *local* — it
    // stats the local FS — so `{home}` resolves to the local home even for a
    // caller that hasn't expanded it. A remote session's file can't be stat'd
    // here, so it starts fresh (documented remote-omp fallback); its launch args
    // are still expanded per host in `spawn::adapt_def_for_launch`, which both
    // a fresh spawn and `restart` go through.
    if let Some(path) = session_file_template(def) {
        let expanded = match crate::paths::home_dir() {
            Some(home) => path.replace(HOME_PLACEHOLDER, &home.to_string_lossy()),
            None => path,
        };
        let file = expanded.replace(ID_PLACEHOLDER, agent_session_id);
        if std::path::Path::new(&file).exists() {
            return Some(agent_session_id.to_string());
        }
        // The template exists but the file doesn't (never launched, remote, or
        // gone) → a fresh session, not the claude fallback (which would
        // false-positive on a coincident claude transcript for the same id).
        return None;
    }
    resume_id_if_transcript_exists(agent_session_id, env)
}

/// The session *file* template of a path-pinned agent — a `new_session_args`
/// token that both names a filesystem path (has a separator) and carries the
/// `{id}` placeholder (e.g. omp's `{home}/.omp/.../thurbox-{id}.jsonl`).
/// `None` for id-only agents (claude/pi pass a bare `{id}`, not a path). Used to
/// decide resume-vs-fresh by that file's existence. Agent-neutral: it reads the
/// def's own args rather than matching an agent name.
fn session_file_template(def: &crate::session::AgentDef) -> Option<String> {
    def.new_session_args
        .iter()
        .find(|t| t.contains(ID_PLACEHOLDER) && (t.contains('/') || t.contains('\\')))
        .cloned()
}

/// Resolve the [`AgentDef`](crate::session::AgentDef) for a requested agent name
/// from the on-disk registry. The single source of truth for the agent fallback
/// chain (requested name → registry default → built-in default), so spawn and
/// restart agree on both the launched def *and* its `.name` without re-running
/// the seed. `None`/empty falls straight through to the registry default.
/// The launcher command for an off-local host — `ssh <opts> <dest>` or
/// `wsl.exe -d <distro>`.
///
/// `session` is a pure-data leaf, so the `HostDef` → launcher conversion cannot
/// live on the type; `git::command` carries the same one-liner for its own
/// side of the boundary.
pub fn host_launcher(host: &crate::session::HostDef) -> crate::shell::HostLauncher<'_> {
    if host.is_wsl() {
        crate::shell::HostLauncher::Wsl {
            distro: host.distro_name(),
        }
    } else {
        crate::shell::HostLauncher::Ssh {
            destination: &host.destination,
            ssh_opts: &host.ssh_opts,
        }
    }
}

/// Run a command in `cwd`, on `host` when the session lives off this machine,
/// under `env`.
///
/// The remote form is one `sh -c` because `cd` and the command have to share a
/// shell, and every word is POSIX-quoted so the host's login shell re-splitting
/// cannot reinterpret an argument containing a space. Lives here rather than in
/// `cli` because knowing how to reach a host — and how to quote for one — is
/// `session_ops`' job; `cli` may not reach into `shell` at all.
///
/// `env` is layered onto the inherited environment, and every inherited
/// `THURBOX_*` variable is dropped first. That scrub is the substance, not
/// hygiene: this runs *in a session's context*, and the caller invoking it is
/// itself usually inside a different session, whose `THURBOX_SESSION` the child
/// would otherwise inherit — so a `thurbox-cli session signal` run through here
/// would record state against the caller, silently and with exit 0. Anything
/// the target session should carry is in `env`, which is passed explicitly for
/// exactly this reason. Nothing crosses an SSH connection, so the remote form
/// has nothing to scrub and only has to set: `env` there is one `env K=V …`
/// prefix, whose arguments carry names a shell assignment prefix could not.
pub fn exec_in_dir(
    host: Option<&crate::session::HostDef>,
    cwd: &std::path::Path,
    program: &str,
    args: &[String],
    env: &BTreeMap<String, String>,
) -> std::io::Result<std::process::Output> {
    match host {
        None => {
            let mut cmd = std::process::Command::new(program);
            cmd.args(args).current_dir(cwd);
            for key in std::env::vars_os()
                .map(|(k, _)| k)
                .filter(|k| k.to_string_lossy().starts_with(THURBOX_ENV_PREFIX))
            {
                cmd.env_remove(key);
            }
            cmd.envs(env).output()
        }
        Some(host) => {
            let assignments = env
                .iter()
                .map(|(k, v)| crate::shell::posix_quote(&format!("{k}={v}")));
            let script = format!(
                "cd {} && exec env {}",
                crate::shell::posix_quote(&cwd.to_string_lossy()),
                assignments
                    .chain(
                        std::iter::once(program)
                            .chain(args.iter().map(String::as_str))
                            .map(crate::shell::posix_quote)
                    )
                    .collect::<Vec<_>>()
                    .join(" ")
            );
            host_launcher(host).shell_c(&script).output()
        }
    }
}

/// The prefix every variable thurbox injects into a session's processes shares.
const THURBOX_ENV_PREFIX: &str = "THURBOX_";

/// How thurbox would launch a registered agent: the executable, its arguments,
/// and the environment around them.
///
/// The point of reporting it is the arguments. An agent's status hooks are
/// installed by *appending to its `args`* in `agents.toml` (claude's
/// `--settings <hooks>.json`), so they only fire when thurbox builds the
/// command line. A driver that launches the agent itself — the documented
/// `--command` path, or typing into a shell session — got no hooks, and so an
/// empty `state` and a `watch` stream that never mentioned that session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchPlan {
    /// The registry name the plan was resolved from.
    pub agent: String,
    /// The executable, resolved on `PATH` at launch exactly as thurbox leaves
    /// it to the multiplexer.
    pub command: String,
    pub args: Vec<String>,
    /// The environment to launch under. Without a session this is only the
    /// pointers to *this* thurbox instance (config dir, data dir, multiplexer
    /// socket); with one it also carries that session's identity, which is what
    /// makes the agent's `session signal` land on the right row.
    pub env: BTreeMap<String, String>,
    /// Why hooks-driven status will be degraded or absent on this host, when it
    /// will be. `None` means nothing is known to be wrong.
    pub degraded: Option<String>,
    /// Whether the built-in `hooks` extension is active. It is what puts the
    /// status wiring into an agent's `args` in the first place, so with it off
    /// these args are simply the agent's own — and an empty `args` is then an
    /// answer rather than a surprise.
    pub hooks_enabled: bool,
}

/// Resolve the [`LaunchPlan`] for a registered agent, optionally in the context
/// of an existing session.
///
/// With `session`, the plan is the one that session's *next fresh launch* would
/// use: its `agent_session_id` pins the conversation, its host adapts the args,
/// and its identity is in the environment. Resuming an existing conversation is
/// `session start`/`restart`'s job and deliberately not offered here — a driver
/// asking what to run is starting something, not continuing it.
pub fn agent_launch_plan(
    db: &crate::storage::Database,
    agent: &str,
    session: Option<&crate::sync::SharedSession>,
) -> Result<LaunchPlan, String> {
    let registry = crate::agent::agent_config::load_or_seed();
    let def = registry.get(agent).cloned().ok_or_else(|| {
        let known: Vec<&str> = registry.agents.iter().map(|a| a.name.as_str()).collect();
        format!(
            "no agent named '{agent}' in agents.toml; known agents: {}",
            known.join(", ")
        )
    })?;
    let host = match session {
        Some(s) => resolve_host(&s.backend_type).ok_or_else(|| {
            format!(
                "session '{}' runs on backend '{}', which is not in hosts.toml",
                s.name, s.backend_type
            )
        })?,
        None => None,
    };
    let mut config = SessionConfig {
        session_id: session.map(|s| s.id),
        agent_session_id: session.and_then(|s| s.agent_session_id.clone()),
        agent: def.name.clone(),
        cwd: session.and_then(|s| s.cwd.clone()),
        backend: session.map(|s| s.backend_type.clone()),
        ..SessionConfig::default()
    };
    match session {
        Some(s) => config.env.extend(session_process_env(db, s)),
        // No session to take an identity from, but the instance is still
        // knowable — and a child `thurbox-cli` that resolves a different data
        // dir or socket would report into a database nothing here reads.
        None => config.env.extend(thurbox_env_overrides()),
    }
    let hooks = hooks_enabled(db);
    let (def, degraded) = spawn::adapt_def_for_launch(def, host.as_ref(), hooks);
    let (command, args) = build_agent_invocation(&def, &config);
    Ok(LaunchPlan {
        agent: def.name,
        command,
        args,
        env: config.env.into_iter().collect(),
        degraded,
        hooks_enabled: hooks,
    })
}

/// The environment a process run **in a session's context** must carry: the
/// session's own recorded `--env`, then the identity variables its pane has.
///
/// The order is the spawn's, and for the same reason: thurbox's identity wins,
/// so a session cannot rename its own `THURBOX_SESSION` and report another
/// session's state. Built from `inject_thurbox_env` rather than beside it, so
/// what `session exec` hands a child and what the pane holds cannot drift —
/// the surprise this exists to remove is precisely that the two disagreed.
///
/// A session with no `agent_session_id` gets no `THURBOX_SESSION_ID`: an empty
/// one would read as a conversation id rather than as the absence of one.
pub fn session_process_env(
    db: &crate::storage::Database,
    session: &crate::sync::SharedSession,
) -> BTreeMap<String, String> {
    let mut config = SessionConfig {
        session_id: Some(session.id),
        agent_session_id: session.agent_session_id.clone(),
        agent: session.agent.clone(),
        backend: Some(session.backend_type.clone()),
        ..SessionConfig::default()
    };
    config
        .env
        .extend(db.load_launch_env(session.id).unwrap_or_default());
    inject_thurbox_env(
        &mut config,
        session.agent_session_id.as_deref().unwrap_or_default(),
        None,
    );
    if session.agent_session_id.is_none() {
        config.env.remove("THURBOX_SESSION_ID");
    }
    config.env.into_iter().collect()
}

/// Fork a session: a new one beside it, on the same directory and branch, with
/// the agent asked to continue the parent's conversation into a fresh one.
///
/// What makes it a fork rather than a second session in the same place is
/// `fork_session_id`, which drives the agent's `fork_args`. An agent that
/// declares none — or a command session, which has no conversation at all —
/// still gets a working session here; it simply starts empty, and the caller is
/// told so by the returned session's own `agent_session_id` differing with no
/// parent conversation behind it.
///
/// Shared by the interface's fork command and `thurbox-cli session fork`.
pub fn fork_session_headless(
    db: &crate::storage::Database,
    id: crate::session::SessionId,
    name: &str,
) -> Result<spawn::SpawnResult, String> {
    let source = db
        .get_session_by_id(id)
        .map_err(|e| format!("get session: {e}"))?
        .ok_or_else(|| format!("session not found: {id}"))?;

    // The parent's *working directory* — its worktree for a worktree session —
    // not the repository root. A cwd-scoped agent (`codex resume --last`,
    // `opencode --continue`) resolves "the last session here" from it, so the
    // repo root would find nothing to continue.
    let repo_path = source
        .cwd
        .clone()
        .or_else(|| {
            source
                .worktrees
                .first()
                .map(|worktree| worktree.worktree_path.clone())
        })
        .ok_or("the source session has no directory to fork in")?;

    let name = if name.is_empty() {
        format!("{}-fork", source.name)
    } else {
        name.to_string()
    };

    // A command session has no registry entry, so the fork has to carry the
    // recipe rather than the agent name — otherwise it would look up an agent
    // called `bash` and find nothing.
    let recipe = db.load_launch_recipe(id).unwrap_or_default();
    // Read separately from the recipe: a registry-agent session has no
    // recipe at all, but its `--env` is still recorded in the same column and
    // there is nowhere else to re-resolve it from for the fork.
    let env = db.load_launch_env(id).unwrap_or_default();

    let request = spawn::SpawnRequest {
        name,
        repo_path,
        // No new worktree (the defaults): a fork works beside its parent, on
        // the same branch.
        agent: recipe.is_none().then(|| source.agent.clone()),
        command: recipe.as_ref().map(|r| r.command.clone()),
        args: recipe.as_ref().map(|r| r.args.clone()).unwrap_or_default(),
        env,
        // A fork of a remote session belongs on that session's host, or it would
        // silently become a local session pointed at a path that is not here.
        host: resolve_host(&source.backend_type)
            .flatten()
            .map(|host| host.name),
        parent_session_id: Some(source.id),
        // What actually makes it a fork: the agent resumes the parent's
        // conversation into a new one (`fork_args`).
        fork_session_id: source.agent_session_id.clone(),
        // Shared, not created — so the fork shows its branch and can be synced.
        inherit_worktrees: source.worktrees.clone(),
        ..Default::default()
    };
    spawn::spawn_session_headless(db, request)
}

/// The [`LaunchRecipe`](crate::session::LaunchRecipe) a spawn request carries,
/// or `None` when it names a registry agent instead.
///
/// One place decides "is this a command session", so the spawn path, the
/// restart path and the persistence all agree on the same discriminant: a
/// non-empty `command`.
pub(crate) fn launch_recipe(req: &spawn::SpawnRequest) -> Option<crate::session::LaunchRecipe> {
    let command = req.command.as_deref().filter(|c| !c.is_empty())?;
    Some(crate::session::LaunchRecipe {
        command: command.to_string(),
        args: req.args.clone(),
        env: req.env.clone(),
    })
}

/// Present a raw command as an [`AgentDef`](crate::session::AgentDef), so every
/// launch path downstream is the one that already exists.
///
/// Modelling a command session as a definition rather than a second kind of
/// launch is what keeps this small: `build_agent_invocation`, the remote
/// adaptation and the restart plan all take an `AgentDef` and none of them
/// learns a new shape. The empty arg groups are the substance, not filler —
/// they *are* the statement that this session has no conversation to resume or
/// fork, which is why `session resume`/`fork` decline it rather than launching
/// something that silently starts fresh.
///
/// The name is the command's file stem so the session reads as `bash` rather
/// than as a path. It is not looked up anywhere: hook coverage keys on known
/// agent names, and an unknown one correctly reports uncovered.
pub(crate) fn recipe_agent_def(recipe: &crate::session::LaunchRecipe) -> crate::session::AgentDef {
    let name = std::path::Path::new(&recipe.command)
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| recipe.command.clone());
    crate::session::AgentDef {
        name,
        command: recipe.command.clone(),
        args: recipe.args.clone(),
        ..crate::session::AgentDef::default()
    }
}

pub(crate) fn resolve_agent_def(requested: Option<&str>) -> crate::session::AgentDef {
    let registry = crate::agent::agent_config::load_or_seed();
    requested
        .filter(|n| !n.is_empty())
        .and_then(|n| registry.get(n))
        .or_else(|| registry.default_agent())
        .cloned()
        .unwrap_or_else(|| {
            crate::agent::agent_config::builtin_registry()
                .default_agent()
                .cloned()
                .expect("built-in registry always has a default agent")
        })
}

/// Placeholder in an [`AgentDef`](crate::session::AgentDef)'s arg groups,
/// expanded to the resolved home dir at spawn time. Unlike `{id}` (a bare
/// UUID substituted in `build_args`), `{home}` needs a filesystem side effect
/// — the home dir — so it is resolved here in `session_ops`, not in the pure
/// `session` layer. Used by agents that want a session *file path* rather than
/// a bare id (e.g. omp's `--session {home}/.omp/…/thurbox-{id}.jsonl`); the
/// path can't rely on the shell to expand `~`, since args are POSIX-quoted.
pub(crate) const HOME_PLACEHOLDER: &str = "{home}";

/// Rewrite every `{home}` token in `def`'s arg groups (`args`, `resume_args`,
/// `fork_args`, `new_session_args`) to `home`, in place.
///
/// `home` is the target's home dir: the *local* home for a local launch/restart
/// (via [`crate::paths::home_dir`]), or the *remote* home for an SSH/WSL host
/// (resolved by [`crate::git::remote_home`] in
/// [`spawn::adapt_def_for_launch`]). A def with no `{home}` token (every
/// built-in but `omp`) is left byte-identical, so this is a no-op for them.
pub(crate) fn expand_home_in_def(def: &mut crate::session::AgentDef, home: &str) {
    let subst = |tokens: &mut Vec<String>| {
        for t in tokens.iter_mut() {
            if t.contains(HOME_PLACEHOLDER) {
                *t = t.replace(HOME_PLACEHOLDER, home);
            }
        }
    };
    subst(&mut def.args);
    subst(&mut def.resume_args);
    subst(&mut def.fork_args);
    subst(&mut def.new_session_args);
}

/// Build the `(command, args)` invocation for an already-resolved [`AgentDef`].
///
/// Centralised here so headless spawn and restart agree on the args, and so the
/// `AgentDef` is resolved exactly once per operation (callers pass the def they
/// already resolved rather than re-running [`resolve_agent_def`]).
pub(crate) fn build_agent_invocation(
    def: &crate::session::AgentDef,
    config: &SessionConfig,
) -> (String, Vec<String>) {
    let provider = crate::agent::GenericProvider::new(def.clone());
    // Reach the provider trait methods via fully-qualified call syntax so this
    // module imports nothing from the agent module (architecture rule:
    // session_ops must stay free of agent-layer imports).
    let command =
        <crate::agent::GenericProvider as crate::agent::AgentProvider>::command(&provider)
            .to_string();
    let args = <crate::agent::GenericProvider as crate::agent::AgentProvider>::build_args(
        &provider, config,
    );
    (command, args)
}

/// The machine a session runs on: `Some(None)` for local, `Some(Some(host))` for
/// a remote backend we can resolve, and `None` when the backend names a host
/// `hosts.toml` no longer describes.
///
/// The distinction is the whole point. Anything that drives a session's window
/// or its worktrees has to do it *where they are*, and doing it locally instead
/// is not a degraded version of that — it acts on the wrong machine. So an
/// unresolvable host is a refusal, never a fallback to local.
pub fn resolve_host(backend_type: &str) -> Option<Option<crate::session::HostDef>> {
    if !crate::session::is_remote_backend(backend_type) {
        return Some(None);
    }
    // The cached registry: this runs on the UI thread per diff request, and a
    // fresh load walks $PATH for WSL discovery every time.
    let (registry, _warnings) = crate::agent::host_config::cached_registry();
    registry.get_by_backend(backend_type).cloned().map(Some)
}

/// Inject the standard thurbox env hints into a session config so a
/// `thurbox-cli` call running *inside* the session can prove its own identity
/// without scraping panes or names:
///
/// - `THURBOX_SESSION` — the thurbox [`SessionId`] (the registry key). Read by
///   the mailbox CLI to auto-stamp provenance and default the inbox to "me".
///   Requires `config.session_id` to be set before calling.
/// - `THURBOX_SESSION_ID` — the *agent's* conversation id (`agent_session_id`),
///   consumed by the metrics statusline. Distinct from `THURBOX_SESSION`.
/// - `THURBOX_TASK` — the originating task id, when this session was spawned for
///   a task (so messages auto-tag `from_task_id`). Headless `task run` only; the
///   TUI task-spawn path tracks the link in-memory instead.
/// - `THURBOX_METRICS_DIR` — metrics output dir.
/// - `THURBOX_CONFIG_DIR` / `THURBOX_DATA_DIR` — the resolved config/data dirs,
///   so the agent's `thurbox-cli` (its status hook) targets the same DB the TUI
///   reads regardless of XDG / PATH / a stale tmux-server env.
/// - `THURBOX_SOCKET` — the multiplexer socket this instance's sessions live
///   on, so an in-session `thurbox-cli` reaches the same server rather than
///   re-deriving one from an environment that need not match.
///
/// The four *location* vars are **local-only**: a remote (SSH/WSL) session skips
/// them — the local dirs don't exist on the host, its socket is the host's own,
/// and a remote `thurbox-cli` pinned to them would resolve garbage instead of
/// its own defaults. The
/// identity vars (`THURBOX_SESSION`/`THURBOX_SESSION_ID`/`THURBOX_TASK`) are
/// opaque and travel everywhere.
///
/// Kept in sync with `App::build_spawn_inputs` so headless and TUI sessions look
/// identical to the spawned process (modulo `THURBOX_TASK` as noted above).
///
/// Shared by the headless spawn/restart paths and the TUI `Ctrl+R` restart
/// (`App::restart_active_session`), so a restarted session keeps the same
/// identity env a fresh spawn would have had.
pub(crate) fn inject_thurbox_env(
    config: &mut SessionConfig,
    agent_session_id: &str,
    task_id: Option<i64>,
) {
    config
        .env
        .insert("THURBOX_SESSION_ID".into(), agent_session_id.into());
    if let Some(id) = config.session_id {
        config.env.insert("THURBOX_SESSION".into(), id.to_string());
    }
    if let Some(task_id) = task_id {
        config
            .env
            .insert("THURBOX_TASK".into(), task_id.to_string());
    }
    if config
        .backend
        .as_deref()
        .is_some_and(crate::session::is_remote_backend)
    {
        return;
    }
    if let Some(dir) = crate::paths::metrics_directory() {
        config
            .env
            .insert("THURBOX_METRICS_DIR".into(), dir.to_string_lossy().into());
    }
    // Pin the agent's `thurbox-cli` (its status hook) to the *same* config/data
    // dirs and multiplexer socket this thurbox resolved, so a status `signal`
    // always lands in the DB the TUI reads and a `session send` reaches the
    // server the session is actually on — independent of XDG, which
    // `thurbox-cli` is on PATH, or a stale tmux-server env.
    config.env.extend(thurbox_env_overrides());
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::SessionId;

    #[cfg(unix)]
    #[test]
    fn run_exec_command_reports_success_and_failure() {
        let (status, detail) = run_exec_command("printf hello");
        assert_eq!(status, AutomationRunStatus::Success);
        assert!(detail.contains("hello"), "got {detail}");

        let (status, detail) = run_exec_command("exit 3");
        assert_eq!(status, AutomationRunStatus::Error);
        assert!(detail.contains('3'), "got {detail}");

        // No output on success collapses to a friendly "ok".
        let (status, detail) = run_exec_command("true");
        assert_eq!(status, AutomationRunStatus::Success);
        assert_eq!(detail, "ok");
    }

    #[test]
    fn inject_env_sets_identity_and_task() {
        let sid = SessionId::default();
        let mut config = SessionConfig {
            session_id: Some(sid),
            ..SessionConfig::default()
        };
        inject_thurbox_env(&mut config, "agent-conv-uuid", Some(42));
        // The thurbox session key and the agent conversation id are distinct.
        assert_eq!(config.env.get("THURBOX_SESSION"), Some(&sid.to_string()));
        assert_eq!(
            config.env.get("THURBOX_SESSION_ID"),
            Some(&"agent-conv-uuid".to_string())
        );
        assert_eq!(config.env.get("THURBOX_TASK"), Some(&"42".to_string()));
    }

    #[test]
    fn inject_env_pins_config_data_dirs_and_socket() {
        // The agent's status hook must target the same DB the TUI reads and the
        // same multiplexer server its session lives on, so both are injected
        // for `thurbox-cli` to honour rather than resolve for itself.
        let tmp = tempfile::tempdir().unwrap();
        let _guard = crate::paths::TestPathGuard::new(tmp.path());
        let mut config = SessionConfig {
            session_id: Some(SessionId::default()),
            ..SessionConfig::default()
        };
        inject_thurbox_env(&mut config, "agent-conv-uuid", None);

        let cfg_dir = config
            .env
            .get(crate::paths::CONFIG_DIR_OVERRIDE_ENV)
            .expect("config dir injected");
        let data_dir = config
            .env
            .get(crate::paths::DATA_DIR_OVERRIDE_ENV)
            .expect("data dir injected");
        assert_eq!(
            Some(std::path::Path::new(cfg_dir)),
            crate::paths::config_file()
                .as_deref()
                .and_then(|p| p.parent())
        );
        assert_eq!(
            Some(std::path::Path::new(data_dir)),
            crate::paths::database_file()
                .as_deref()
                .and_then(|p| p.parent())
        );
        assert_eq!(
            config.env.get(crate::agent::tmux::SOCKET_OVERRIDE_ENV),
            Some(&crate::agent::tmux::local_socket_name()),
            "the session is told which server it is on"
        );
    }

    #[test]
    fn inject_env_skips_local_path_dirs_for_remote_backend() {
        // The metrics/config/data dirs are *local* paths — meaningless on an
        // SSH/WSL host, and a remote `thurbox-cli` pinned to them would resolve
        // garbage. Identity vars still travel.
        let tmp = tempfile::tempdir().unwrap();
        let _guard = crate::paths::TestPathGuard::new(tmp.path());
        let mut config = SessionConfig {
            session_id: Some(SessionId::default()),
            backend: Some("ssh:devbox".into()),
            ..SessionConfig::default()
        };
        inject_thurbox_env(&mut config, "agent-conv-uuid", None);
        assert!(config.env.contains_key("THURBOX_SESSION"));
        assert!(config.env.contains_key("THURBOX_SESSION_ID"));
        assert!(!config.env.contains_key("THURBOX_METRICS_DIR"));
        assert!(!config
            .env
            .contains_key(crate::paths::CONFIG_DIR_OVERRIDE_ENV));
        assert!(!config.env.contains_key(crate::paths::DATA_DIR_OVERRIDE_ENV));
        // The socket is a local name too: the host's sessions live on the host's
        // own server, which `hosts.toml` (or the host's CLI) names.
        assert!(!config
            .env
            .contains_key(crate::agent::tmux::SOCKET_OVERRIDE_ENV));
    }

    #[test]
    fn inject_env_omits_task_when_absent() {
        let mut config = SessionConfig {
            session_id: Some(SessionId::default()),
            ..SessionConfig::default()
        };
        inject_thurbox_env(&mut config, "agent-conv-uuid", None);
        assert!(config.env.contains_key("THURBOX_SESSION"));
        assert!(!config.env.contains_key("THURBOX_TASK"));
    }

    #[test]
    fn resume_id_is_none_when_no_transcript() {
        let tmp = tempfile::tempdir().unwrap();
        let mut env = HashMap::new();
        env.insert("CLAUDE_CONFIG_DIR".into(), tmp.path().display().to_string());
        assert_eq!(resume_id_if_transcript_exists("some-uuid", &env), None);
    }

    #[test]
    fn resume_id_is_some_when_transcript_exists() {
        let tmp = tempfile::tempdir().unwrap();
        let sid = "77777777-8888-9999-aaaa-bbbbbbbbbbbb";
        let proj = tmp.path().join("projects").join("-slug");
        std::fs::create_dir_all(&proj).unwrap();
        std::fs::write(proj.join(format!("{sid}.jsonl")), b"").unwrap();

        let mut env = HashMap::new();
        env.insert("CLAUDE_CONFIG_DIR".into(), tmp.path().display().to_string());
        assert_eq!(
            resume_id_if_transcript_exists(sid, &env),
            Some(sid.to_string())
        );
    }

    #[test]
    fn resume_trigger_latest_agent_always_triggers() {
        // A resume_latest agent (codex) triggers resume regardless of any
        // on-disk claude transcript; the returned id is just the trigger.
        let codex = crate::agent::agent_config::builtin_registry()
            .get("codex")
            .unwrap()
            .clone();
        assert!(codex.resumes_latest());
        let env = HashMap::new();
        assert_eq!(
            resume_trigger_for(&codex, "thurbox-uuid", &env),
            Some("thurbox-uuid".to_string())
        );
    }

    #[test]
    fn resume_trigger_claude_defers_to_transcript() {
        // claude is not resume_latest, so it only resumes when a transcript
        // exists — same behaviour as resume_id_if_transcript_exists.
        let claude = crate::agent::agent_config::builtin_registry()
            .get("claude")
            .unwrap()
            .clone();
        assert!(!claude.resumes_latest());

        let tmp = tempfile::tempdir().unwrap();
        let mut env = HashMap::new();
        env.insert("CLAUDE_CONFIG_DIR".into(), tmp.path().display().to_string());
        assert_eq!(resume_trigger_for(&claude, "missing", &env), None);

        let sid = "11111111-2222-3333-4444-555555555555";
        let proj = tmp.path().join("projects").join("-slug");
        std::fs::create_dir_all(&proj).unwrap();
        std::fs::write(proj.join(format!("{sid}.jsonl")), b"").unwrap();
        assert_eq!(
            resume_trigger_for(&claude, sid, &env),
            Some(sid.to_string())
        );
    }

    #[test]
    fn expand_home_rewrites_only_home_token() {
        let mut omp = crate::agent::agent_config::builtin_registry()
            .get("omp")
            .expect("omp is a built-in")
            .clone();
        expand_home_in_def(&mut omp, "/home/me");
        assert_eq!(
            omp.new_session_args,
            vec![
                "--session".to_string(),
                "/home/me/.omp/agent/sessions/thurbox-{id}.jsonl".to_string()
            ]
        );
        assert_eq!(
            omp.resume_args,
            vec![
                "--resume".to_string(),
                "/home/me/.omp/agent/sessions/thurbox-{id}.jsonl".to_string()
            ]
        );
        // `{id}` is left for build_args; only `{home}` was touched.
        assert!(omp.new_session_args.iter().any(|a| a.contains("{id}")));

        // A def with no `{home}` (claude) is byte-identical after expansion.
        let claude = crate::agent::agent_config::builtin_registry()
            .get("claude")
            .unwrap()
            .clone();
        let mut expanded = claude.clone();
        expand_home_in_def(&mut expanded, "/home/me");
        assert_eq!(expanded, claude);
    }

    #[test]
    fn session_file_template_only_for_path_pinned_agents() {
        let reg = crate::agent::agent_config::builtin_registry();
        // omp pins a session file path (has a separator + `{id}`).
        let omp = reg.get("omp").unwrap();
        assert_eq!(
            session_file_template(omp).as_deref(),
            Some("{home}/.omp/agent/sessions/thurbox-{id}.jsonl")
        );
        // claude/pi pass a bare `{id}`, not a path → no template.
        assert_eq!(session_file_template(reg.get("claude").unwrap()), None);
        assert_eq!(session_file_template(reg.get("pi").unwrap()), None);
    }

    #[test]
    fn resume_trigger_omp_resumes_only_when_session_file_exists() {
        // omp is path-pinned: it resumes iff its deterministic JSONL exists.
        let tmp = tempfile::tempdir().unwrap();
        let sid = "99999999-aaaa-bbbb-cccc-dddddddddddd";
        let mut omp = crate::agent::agent_config::builtin_registry()
            .get("omp")
            .unwrap()
            .clone();
        // The caller expands `{home}` before the trigger check; here `{home}` is
        // the tempdir so the check is hermetic (never touches the real ~/.omp).
        expand_home_in_def(&mut omp, &tmp.path().display().to_string());
        let env = HashMap::new();

        // No file yet → fresh session.
        assert_eq!(resume_trigger_for(&omp, sid, &env), None);

        // Create the exact JSONL thurbox would launch against → resume triggers.
        let file = tmp
            .path()
            .join(".omp/agent/sessions")
            .join(format!("thurbox-{sid}.jsonl"));
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, b"").unwrap();
        assert_eq!(resume_trigger_for(&omp, sid, &env), Some(sid.to_string()));
    }

    #[test]
    fn omp_is_a_builtin_and_pi_is_unchanged() {
        let reg = crate::agent::agent_config::builtin_registry();
        // omp exists alongside pi (not replacing it).
        let omp = reg.get("omp").expect("omp seeded");
        assert_eq!(omp.command, "omp");
        assert!(omp.fork_args.is_empty(), "omp has no native fork target");
        // pi keeps its id-pinned resume/fork, untouched by the omp addition.
        let pi = reg.get("pi").expect("pi still present");
        assert_eq!(pi.resume_args, vec!["--session-id", "{id}"]);
        assert_eq!(pi.fork_args, vec!["--fork", "{id}"]);
    }
}
