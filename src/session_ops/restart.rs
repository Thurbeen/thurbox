//! Headless session restart — tears down the tmux windows (agent + companion
//! shell) and re-launches the agent CLI, resuming the existing conversation
//! when the agent supports it and a transcript exists, starting fresh
//! otherwise.
//!
//! And its two halves on their own: [`stop_session_headless`] kills the
//! windows and leaves everything else standing, [`start_session_headless`]
//! puts the agent's window back. A restart is those two in a row, which is
//! why they live here.

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
    db: &Database,
    session: &SharedSession,
    host: Option<&crate::session::HostDef>,
    signal: Option<&str>,
    hooks_enabled: bool,
    recipe: Option<&crate::session::LaunchRecipe>,
    env: &std::collections::BTreeMap<String, String>,
) -> Result<RestartPlan, crate::session_ops::OpError> {
    let agent_session_id = session.agent_session_id.clone().ok_or_else(|| {
        format!(
            "Cannot restart session {} without agent_session_id",
            session.id
        )
    })?;

    let mut config = SessionConfig {
        // Keep the same identity across a restart so `THURBOX_SESSION` is stable.
        session_id: Some(session.id),
        telemetry_attrs: db
            .load_telemetry_attrs(session.id)
            .map_err(|e| format!("read telemetry attributes: {e}"))?,
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
    // Both agent defaults and caller overrides were frozen at creation;
    // re-reading registry defaults here could move the account on restart.
    config
        .env
        .extend(env.iter().map(|(k, v)| (k.clone(), v.clone())));
    config.env.remove(super::CODEX_PICKER_ENV);
    super::inject_thurbox_env(&mut config, &agent_session_id, None, session.key.as_deref());
    // Where a restart gets what to run. A registry agent is resolved by name
    // *now* rather than replayed, so an `agents.toml` edit takes effect on the
    // next restart. A command session has no entry to resolve, so its persisted
    // recipe is replayed verbatim — the reason the recipe is stored at all.
    // Unlike a create, a restart keeps the default for an agent since removed
    // from the file: the session exists, and an agent beats none.
    let platform = super::spawn::platform_of(host);
    let def = match recipe {
        Some(r) => super::recipe_agent_def(r),
        None => super::resolve_agent_def(Some(&config.agent), platform).unwrap_or_else(|e| {
            tracing::warn!(
                "restart of '{}': {e}; launching the default agent",
                session.name
            );
            super::resolve_agent_def(None, platform).expect("the default always resolves")
        }),
    };
    // The same adaptation a spawn does, so a restart lands the agent exactly
    // where the spawn did: `{home}` resolved against the machine it runs on
    // (omp's `--session-dir {home}/…` must reopen the same directory) and hook configs
    // shipped to the host rather than pointing at local paths that do not exist
    // there. Identity for a local restart.
    let (def, degraded) = super::spawn::adapt_def_for_launch(def, host, signal, hooks_enabled);
    if let Some(note) = degraded {
        tracing::warn!("restart of '{}': {note}", session.name);
    }
    let telemetry_args = super::telemetry::apply(
        &mut config,
        &def,
        &session.name,
        host,
        &crate::agent::settings_config::load_quiet().telemetry,
    );
    let codex_builtin = def.is_family("codex") && def.resume_args == ["resume", "{id}"];
    let codex_id = if codex_builtin {
        db.get_session_meta(session.id, "thurbox.codex_conversation_id")
            .map_err(|e| format!("read Codex conversation id: {e}"))?
            .filter(|id| uuid::Uuid::parse_str(id).is_ok())
    } else {
        None
    };
    config.resume_session_id = if codex_builtin {
        codex_id.clone()
    } else if super::conversation_started(db, session.id, &agent_session_id) {
        Some(agent_session_id.clone())
    } else {
        super::resume_trigger_for(&def, &agent_session_id, &config.env)
    };

    // A multi-repo session (≥2 members) launches in its per-session symlink
    // workspace, gathering every member dir; a single-repo session keeps the
    // primary repo. Only resolve when there is a primary cwd to anchor on.
    if let Some(primary) = session.cwd.clone() {
        if session.worktrees.len().max(1) + session.additional_dirs.len() > 1 {
            super::reconcile::workspace_unclaimed(
                db,
                session.id,
                &session.backend_type,
                &agent_session_id,
            )?;
        }
        config.cwd = Some(super::spawn::resolve_launch_cwd(
            &agent_session_id,
            &primary,
            &session.worktrees,
            &session.additional_dirs,
            host,
        ));
    }

    let member_dirs = session.cwd.as_ref().map_or_else(Vec::new, |primary| {
        super::spawn::launch_member_dirs(
            primary,
            &session.worktrees,
            &session.additional_dirs,
            host,
        )
    });
    let (command, mut args) = super::build_agent_invocation(&def, &config, &member_dirs);
    if codex_builtin && codex_id.is_none() {
        // An old row has only Thurbox's generated id. The interactive picker
        // can recover its real conversation without guessing from the CWD.
        args = def.place_session_group(vec!["resume".to_string()]);
        args.extend(def.build_directory_args(&member_dirs));
        config
            .env
            .insert(super::CODEX_PICKER_ENV.into(), "1".into());
    }
    args.extend(telemetry_args);
    args.extend(
        db.load_agent_args(session.id)
            .map_err(|e| format!("read the agent arguments: {e}"))?,
    );

    let (command, args) = super::with_setup(&def, host, command, args)?;
    Ok(RestartPlan {
        window_name: session.name.clone(),
        command,
        args,
        cwd: config.cwd,
        env: config.env,
    })
}

/// The row and its recorded pane hints, which cannot prove ownership after
/// a server restart (ADR-25).
fn owner_of<'a>(session: &'a SharedSession, id: &'a str) -> crate::backend::Owner<'a> {
    crate::backend::Owner::new(id, &session.name).remembering(
        &session.backend_id,
        session.shell_backend_id.as_deref().unwrap_or_default(),
    )
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
pub fn stop_session_headless(
    db: &Database,
    backends: &crate::backend::BackendRegistry,
    session_id: SessionId,
) -> Result<bool, crate::session_ops::OpError> {
    let session = db
        .get_session_by_id(session_id)
        .map_err(|e| format!("Failed to load session: {e}"))?
        .ok_or_else(|| {
            crate::session_ops::OpError::NotFound(format!("Session not found: {session_id}"))
        })?;

    let remote = crate::session::Route::is_remote_key(&session.backend_type);
    // Asked before the mark: a row whose route no backend here serves — on
    // its host or on this machine — would read as parked while the window that
    // could not be killed keeps running. A host `hosts.toml` no longer
    // describes is different — see below.
    let backend = if !remote || super::resolve_host(&session.backend_type).is_some() {
        Some(
            super::windows::backend_for(backends, &session.backend_type)
                .map_err(|e| format!("cannot stop '{}': {e}", session.name))?,
        )
    } else {
        None
    };

    db.set_session_stopped(session_id, true)
        .map_err(|e| format!("Failed to mark the session stopped: {e}"))?;

    let id = session.id.to_string();
    let killed = match backend {
        // An unreachable or unconfigured host is not a reason to refuse: the
        // mark is what makes the stop stick, and the pane is reclaimed by the
        // next teardown that can reach it.
        Some(backend) => super::windows::kill_owned(backend.as_ref(), owner_of(&session, &id))
            .unwrap_or_else(|e| {
                tracing::debug!("could not kill the windows of '{}': {e:#}", session.name);
                false
            }),
        None => false,
    };

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
    backends: &crate::backend::BackendRegistry,
    session_id: SessionId,
) -> Result<RestartReport, crate::session_ops::OpError> {
    // Refused before the mark comes off: a stop cleared for a session nothing
    // here can relaunch reads as a session that should be running.
    let session = db
        .get_session_by_id(session_id)
        .map_err(|e| format!("Failed to load session: {e}"))?
        .ok_or_else(|| {
            crate::session_ops::OpError::NotFound(format!("Session not found: {session_id}"))
        })?;
    if !matches!(
        super::Executor::for_backend(&session.backend_type),
        Some(super::Executor::HostCli { .. })
    ) {
        super::windows::backend_for(backends, &session.backend_type)
            .map_err(|e| format!("cannot start '{}': {e}", session.name))?;
    }
    db.set_session_stopped(session_id, false)
        .map_err(|e| format!("Failed to clear the stopped mark: {e}"))?;
    restart_for(db, backends, session_id, Relaunch::Unparking)
}

/// Refuse a restart of a row that has been deleted since it was loaded.
///
/// The row was active when `restart_session_headless_with` read it, but a
/// `pre_restart` hook is the user's own program and takes as long as it takes.
/// Asked again with nothing yet killed: relaunching a session somebody deleted
/// in between puts an agent on a row whose windows the sweep is on its way to
/// collect, and the pane write that follows would refuse anyway
/// (see [`record_pane`]) — after the spawn rather than before it.
fn refuse_if_deleted(
    db: &Database,
    session: &crate::sync::SharedSession,
) -> Result<(), crate::session_ops::OpError> {
    match db.get_deleted_session_by_id(session.id) {
        Ok(None) => Ok(()),
        Ok(Some(_)) => Err(format!(
            "'{}' was deleted while it was preparing to restart",
            session.name
        )
        .into()),
        Err(e) => Err(format!("could not re-read '{}': {e}", session.name).into()),
    }
}

/// What recording a restart's new pane on its row came to.
///
/// Two ways of not succeeding, and the caller must not treat them alike: only
/// one of them means the window it just spawned is nobody's.
enum Recorded {
    /// The row took the new pane id.
    Stored,
    /// The row is **gone** — deleted while the restart was in flight. Nothing
    /// will ever attach to the window just spawned.
    RowGone,
    /// A live row on the same server already names this pane.
    Occupied,
    /// The write itself failed, which says nothing about the row. The window
    /// stays up; see [`record_pane`].
    WriteFailed(String),
}

impl Recorded {
    /// What a caller reports when the row turned out to be gone. The window it
    /// just spawned is killed on the way out, on both transports.
    fn deleted_mid_restart(name: &str) -> String {
        format!("'{name}' was deleted while it was restarting; its new window is being killed")
    }
}

/// Persist the pane a restart just spawned.
///
/// A targeted [`Database::claim_backend_id`] rather than the full-row
/// `upsert_session` this used to be, and the difference is `deleted_at = NULL`:
/// the full write revived a row deleted between the session being loaded and
/// the new window being recorded, so a delete inside its own undo window came
/// back attached to a freshly spawned agent. The targeted write refuses a
/// deleted row instead — but a *force*-deleted row is never reaped (both
/// [`reap_overdue_soft_deletes`](crate::session_ops::reap_overdue_soft_deletes)
/// and [`reap_soft_deleted`](crate::session_ops::reap_soft_deleted) skip
/// `force_deleted` rows unconditionally), so [`Recorded::RowGone`] is not on
/// its own proof the window will ever be collected. The caller kills it, rather
/// than leaning on a sweep that only covers the soft-delete case.
///
/// [`Recorded::WriteFailed`] is the case that must **not** kill anything. The
/// storage error it carries — a peer holding the write lock, a full disk — is
/// about the write, not about the row, and the agent that was just launched is
/// a live process with a conversation in it. Left alone, the window is found
/// again on its own: `create_window` stamps it with this session's id, so the
/// interface resolves it by stamp when the row's stale pane id is contradicted
/// by a listing (`Terminals::sync` → `pane_by_name`) and writes the id back
/// (`drain_adopted_panes`). Killing the agent to tidy up after a transient
/// failure is the worse of the two mistakes, and it is the one that left a
/// session with no window at all.
fn record_pane(db: &Database, session: &crate::sync::SharedSession, pane: &str) -> Recorded {
    match db.claim_backend_id(session.id, pane, |target, other| {
        super::server_key(target) == super::server_key(other)
    }) {
        Ok(crate::storage::PaneClaim::Stored) => Recorded::Stored,
        Ok(crate::storage::PaneClaim::RowGone) => Recorded::RowGone,
        Ok(crate::storage::PaneClaim::Occupied) => Recorded::Occupied,
        Err(e) => Recorded::WriteFailed(format!("Failed to record the new pane: {e}")),
    }
}

/// What a restart has to say beyond having happened.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RestartReport {
    /// `session.post_restart` hooks that failed. The restart stands regardless.
    pub hook_failures: Vec<String>,
    /// A relaunch refused because the agent is crash-looping
    /// ([`CRASH_LOOP_RELAUNCHES`]); the session is marked lost instead.
    pub held_back: bool,
}

impl RestartReport {
    /// The restart as a command outcome: a relaunch held back is a failure,
    /// with the reason, since no agent came back. The interface reports a
    /// command that succeeds as "restarted" and lets go of the old pane.
    pub fn into_outcome(self) -> Result<(), crate::session_ops::OpError> {
        match self.held_back {
            true => Err(held_back_reason().into()),
            false => Ok(()),
        }
    }
}

/// Why a relaunch was held back, as the CLI and the interface both say it.
pub fn held_back_reason() -> String {
    format!(
        "it keeps exiting ({CRASH_LOOP_RELAUNCHES} relaunches within {} minutes), \
         so it was marked lost and is not relaunched until it is restarted by hand",
        CRASH_LOOP_WINDOW_MS / 60_000
    )
}

/// How many repairer relaunches within [`CRASH_LOOP_WINDOW_MS`] make a crash
/// loop. Three is past anything a reboot or one bad turn produces, and few
/// enough that a broken agent is not re-run for long.
pub const CRASH_LOOP_RELAUNCHES: usize = 3;

/// The window [`CRASH_LOOP_RELAUNCHES`] is counted over: 10 minutes.
pub const CRASH_LOOP_WINDOW_MS: i64 = 10 * 60 * 1000;

/// Whether `session` is held back from automatic relaunches: it already was,
/// or it has been relaunched [`CRASH_LOOP_RELAUNCHES`] times within
/// [`CRASH_LOOP_WINDOW_MS`] and is held from now on.
///
/// Both are the database's (`storage::relaunches`), which every interface and
/// every `restart --if-missing` writes to — so the limit holds across all of
/// them, where the interface's own `respawned` set only ever stopped one
/// process. Neither is read from the event log, which is pruned.
///
/// The hold is sticky: it outlasts the window, and is lifted only by a pane
/// recorded for the session again — an operator's `session restart` or
/// `session start`, or an adoption. The relaunches that tripped it stay
/// counted until they age out, so an agent that dies again straight after
/// the operator's restart is held again rather than given three more tries.
fn crash_looping(
    db: &Database,
    session: &SharedSession,
) -> Result<bool, crate::session_ops::OpError> {
    let read =
        |e: rusqlite::Error| format!("could not read the relaunches of '{}': {e}", session.name);
    if db.relaunches_held(session.id).map_err(read)? {
        return Ok(true);
    }
    let since = crate::sync::current_time_millis() as i64 - CRASH_LOOP_WINDOW_MS;
    if db.relaunches_since(session.id, since).map_err(read)? < CRASH_LOOP_RELAUNCHES {
        return Ok(false);
    }
    db.hold_relaunches(session.id)
        .map_err(|e| format!("could not hold back '{}': {e}", session.name))?;
    Ok(true)
}

/// Restart an existing session in-place — kills its tmux windows (agent +
/// companion shell) and re-spawns the agent CLI.
///
/// For the `claude` agent, uses its resume group when a transcript for the
/// session id exists on disk, otherwise pins the same id for a fresh start.
/// For `resume_latest` agents (codex, opencode, antigravity, aider, copilot) it
/// resumes the latest session in the (unchanged) launch directory. Other agents
/// degrade to "start fresh" (the live tmux process is what carries state across
/// restarts).
pub fn restart_session_headless(
    db: &Database,
    backends: &crate::backend::BackendRegistry,
    session_id: SessionId,
) -> Result<RestartReport, crate::session_ops::OpError> {
    restart_session_headless_with(db, backends, session_id, false)
}

/// [`restart_session_headless`], or — with `if_missing` — a **relaunch**: the
/// agent is started only when the session has no live window, and a session
/// that is running is left exactly as it is. This is what "the agent is gone"
/// asks for after a reboot, and what makes two observers asking for the same
/// session produce one launch: the second finds the window the first made.
pub fn restart_session_headless_with(
    db: &Database,
    backends: &crate::backend::BackendRegistry,
    session_id: SessionId,
    if_missing: bool,
) -> Result<RestartReport, crate::session_ops::OpError> {
    restart_for(
        db,
        backends,
        session_id,
        match if_missing {
            true => Relaunch::IfMissing,
            false => Relaunch::Asked,
        },
    )
}

/// Why a window is being put back — which is the whole of what a hold on the
/// row means to this caller ([`hold_restart`]).
#[derive(Clone, Copy, PartialEq, Eq)]
enum Relaunch {
    /// The caller asked for this restart by name. It replaces the window
    /// whatever is there, and a hold does not stop it.
    Asked,
    /// A repairer: the interface's respawn of surveyed rows, a peer's
    /// `restart --if-missing`, extension self-heal. It puts a window back only
    /// when one is gone — and a row somebody else is already restarting is a
    /// row whose window is *about to be back*, which is the only thing that
    /// tells the two apart (issue #1207).
    IfMissing,
    /// `session start`. The window is gone because `stop` killed it and the
    /// operator has asked for it back, so it is asked for rather than
    /// repaired: a hold outlives its holder by minutes by design, and one left
    /// behind by a restart killed mid-flight would otherwise make `start`
    /// report success and leave the session with no window at all. It still
    /// asks whether a window is already there, so an unpark of a session that
    /// has one does not spawn a second — locally. Delegated to a host that
    /// restarts its own sessions it is sent as a plain restart, because the
    /// one flag there carries both halves; see the delegation below.
    Unparking,
}

impl Relaunch {
    /// Whether a window that is already there means there is nothing to do.
    fn only_if_missing(self) -> bool {
        self != Self::Asked
    }

    /// Whether this is a **repairer** — which is both what standing down for a
    /// hold is for and what `--if-missing` says on the wire to a host that
    /// restarts its own sessions.
    fn is_a_repairer(self) -> bool {
        self == Self::IfMissing
    }
}

/// Every restart, with [`Relaunch`] saying which caller's rules apply.
fn restart_for(
    db: &Database,
    backends: &crate::backend::BackendRegistry,
    session_id: SessionId,
    why: Relaunch,
) -> Result<RestartReport, crate::session_ops::OpError> {
    let session = db
        .get_session_by_id(session_id)
        .map_err(|e| format!("Failed to load session: {e}"))?
        .ok_or_else(|| {
            crate::session_ops::OpError::NotFound(format!("Session not found: {session_id}"))
        })?;

    // A remote session's window lives on its host, so both halves have to go
    // there — restarting it locally would leave the real window running and add
    // a stray local one beside it. Refusing is only right when we cannot tell
    // *which* host, which is what a missing `hosts.toml` entry means.
    // A session on a shareable host is restarted by the host's CLI, which
    // kills and relaunches where the agent runs, with the host's own
    // configuration; the local row is then mirrored from the host's.
    if let super::Executor::HostCli { host, cli } = super::Executor::for_session(&session)? {
        // `--if-missing` is "I am a repairer", and the host reads both halves
        // out of it: put a window back only if one is gone, and stand down for
        // a hold somebody there already has. An unpark is neither — the
        // operator asked for it, and `stop` killed the window on the host
        // already, so the flag saves nothing and a hold left behind there
        // would silently no-op a `session start` that has cleared the parked
        // mark here.
        return restart_delegated(db, &session, &host, &cli, why.is_a_repairer());
    }
    // Known to `hosts.toml` (`for_session` refused otherwise): the host the
    // plan below launches on, or none for a local session.
    let host = super::resolve_host(&session.backend_type).flatten();

    // Taken before the liveness question below, because that question's answer
    // is honest and wrong: a window killed by a restart that has not yet
    // spawned its replacement really is gone. The hold is what tells "gone"
    // apart from "being replaced", and it lives until this function returns.
    // Who declines on it is `Relaunch`'s to say; what keeps two callers that
    // do not decline to one window is the retirement in `backend::tmux`.
    let held = hold_restart(db, session_id);
    if why.is_a_repairer() && held.is_none() {
        tracing::debug!(
            "'{}' is already being restarted; not relaunching",
            session.name
        );
        return Ok(RestartReport::default());
    }

    // The backend the row's route names: what the windows are killed, listed
    // and spawned with. Refused here, before any of that, when none serves it.
    let backend = super::windows::backend_for(backends, &session.backend_type)
        .map_err(|e| format!("cannot restart '{}': {e}", session.name))?;

    if why.only_if_missing() && !relaunch_is_owed(db, backend.as_ref(), &session)? {
        return Ok(RestartReport::default());
    }

    // Where an exit takes its window with it, "missing" is also what an agent
    // that keeps dying looks like, so a repairer's relaunches are counted.
    let counted = why.is_a_repairer() && !backend.keeps_exited_windows();
    if counted && crash_looping(db, &session)? {
        // The hold has to go first: a loss is not recorded while a restart
        // holds the row, since that gap is normally on purpose.
        drop(held);
        tracing::warn!(
            "'{}' keeps exiting ({CRASH_LOOP_RELAUNCHES} relaunches within {} minutes); \
             not relaunching it until it is restarted by hand",
            session.name,
            CRASH_LOOP_WINDOW_MS / 60_000
        );
        if let Err(e) = db.record_session_lost(session.id, &session.backend_id) {
            tracing::warn!("could not record the loss of '{}': {e}", session.name);
        }
        return Ok(RestartReport {
            held_back: true,
            ..RestartReport::default()
        });
    }

    let hooks_enabled = super::hooks_enabled(db);
    let _workspace = super::lock::workspace(
        db,
        &session.backend_type,
        session.agent_session_id.as_deref(),
        "restart workspace",
    )?;
    // Strict, both of them: a `--command` session's recipe read as absent makes
    // `build_restart_plan` treat it as a registry agent, find no `agents.toml`
    // entry for a command like `bash`, and launch the *default* coding agent in
    // its place. Refusing to restart is the honest answer to a read that failed.
    let recipe = db.load_launch_recipe(session_id).map_err(|e| {
        format!(
            "could not read the launch recipe of '{}': {e}",
            session.name
        )
    })?;
    let env = db
        .load_launch_env(session_id)
        .map_err(|e| format!("could not read the launch env of '{}': {e}", session.name))?;
    let plan = build_restart_plan(
        db,
        &session,
        host.as_ref(),
        backend.hook_signal_command().as_deref(),
        hooks_enabled,
        recipe.as_ref(),
        &env,
    )?;

    // The user's say, with the plan built and nothing yet killed: a refusal
    // leaves the running window running.
    let mut hook_ctx = super::lifecycle_hooks::context_for(&session);
    super::fire_pre(crate::session::HookEvent::PreRestart, &hook_ctx)?;

    refuse_if_deleted(db, &session)?;

    respawn(db, backend.as_ref(), &session, &plan)?;
    if counted {
        if let Err(e) = db.record_relaunch(
            session.id,
            crate::sync::current_time_millis() as i64 - CRASH_LOOP_WINDOW_MS,
        ) {
            tracing::warn!("could not record the relaunch of '{}': {e}", session.name);
        }
    }

    // The agent was re-spawned fresh; clear any stale hook-driven status so it
    // doesn't show a leftover Blocked/Working/Done until the agent re-reports
    // (a resumed agent may not re-fire its boot hook). A failure here leaves
    // the old status on a new process, which `session doctor` then reports as
    // a live signal — worth a line in the log rather than a discarded Result.
    if let Err(e) = db.clear_hook_state(session_id) {
        tracing::warn!("could not clear the hook state of '{}': {e}", session.name);
    }

    // The new pane is what the row now points at, so that is what the
    // post-restart hooks are told.
    hook_ctx.backend_id = super::lifecycle_hooks::current_pane(db, session_id);
    let hook_failures = super::fire_post(crate::session::HookEvent::PostRestart, &hook_ctx);

    Ok(RestartReport {
        hook_failures,
        ..RestartReport::default()
    })
}

/// Restart through the host's own CLI, then mirror the row back from it.
fn restart_delegated(
    db: &Database,
    session: &SharedSession,
    host: &crate::session::HostDef,
    cli: &super::host_cli::CliInfo,
    if_missing: bool,
) -> Result<RestartReport, crate::session_ops::OpError> {
    let mut hook_ctx = super::lifecycle_hooks::context_for(session);
    super::fire_pre(crate::session::HookEvent::PreRestart, &hook_ctx)?;
    let id = session.id.to_string();
    let mut args = vec!["session", "restart", &id];
    if if_missing {
        args.push("--if-missing");
    }
    let result = super::host_cli::run(host, cli, &args)?;
    if let Err(e) = super::mirror::mirror_host(db, host, cli) {
        tracing::warn!("mirror of '{}' after restart failed: {e}", host.name);
    }
    if result["held_back"].as_bool().unwrap_or(false) {
        return Ok(RestartReport {
            held_back: true,
            ..RestartReport::default()
        });
    }
    hook_ctx.backend_id = super::lifecycle_hooks::current_pane(db, session.id);
    let hook_failures = super::fire_post(crate::session::HookEvent::PostRestart, &hook_ctx);
    Ok(RestartReport {
        hook_failures,
        ..RestartReport::default()
    })
}

/// Hold the right to replace `session_id`'s window — or `None` when somebody
/// else already holds it.
///
/// A restart is kill-then-spawn, and between those two steps the session is
/// indistinguishable from one whose agent died. That is exactly what a
/// repairer relaunches — the interface's `respawn_missing_agents`, a peer's
/// `restart --if-missing`, extension self-heal — and both then spawn and stamp,
/// leaving one session id on two windows (issue #1207). The `respawned` guard
/// in `respawn_missing_agents` only ever closed the interface racing itself;
/// `thurbox-cli session restart` is a separate process, and the database is the
/// only thing the two share.
///
/// The row's [`super::lock`], so it is sized against every configured
/// lifecycle hook — a `pre_restart` that takes minutes is inside the hold rather
/// than beyond it — and expiring is what keeps a restart killed mid-flight from
/// holding its row forever.
fn hold_restart(db: &Database, session_id: SessionId) -> Option<super::lock::Lock<'_>> {
    match super::lock::lock(db, super::lock::Subject::Session(session_id), "restart") {
        Ok(held) => held,
        // A write that failed says nothing about who holds the row, and the
        // caller only ever declines a *relaunch* on a `None` — which is the
        // safe direction: the next heartbeat asks again.
        Err(e) => {
            tracing::warn!("could not claim the restart of '{session_id}': {e}");
            None
        }
    }
}

/// Whether a relaunch is owed: never for a parked session, and only when a
/// listing positively says the window is gone.
///
/// Deliberately not the whole question. "Is the window gone" is answered
/// honestly and wrongly in the middle of a restart, and [`hold_restart`] is
/// what covers that — asked by the caller rather than here, so the hold lives
/// for the restart instead of for this question.
fn relaunch_is_owed(
    db: &Database,
    backend: &dyn crate::backend::SessionBackend,
    session: &SharedSession,
) -> Result<bool, crate::session_ops::OpError> {
    // "Relaunch what is missing" is what a peer asks after a reboot, and a
    // parked session is missing on purpose. Only `session start` clears the
    // mark, so this is the one place that has to check it — refusing here
    // is what makes `stop` outlive the next sync tick.
    // Read strictly: a failed read is not "not stopped". Falling open here
    // relaunched a deliberately parked session under exactly the DB
    // contention that caused the failure.
    if db
        .session_stopped_at(session.id)
        .map_err(|e| format!("could not read the stopped mark of '{}': {e}", session.name))?
        .is_some()
    {
        tracing::debug!("'{}' is stopped; not relaunching", session.name);
        return Ok(false);
    }
    // Only a listing that positively says the window is *gone* relaunches.
    // An ambiguous name — several windows, none stamped — reads as running
    // here on purpose: launching another agent is the expensive mistake.
    let id = session.id.to_string();
    let alive = super::windows::agent_running(backend, owner_of(session, &id)).map_err(|e| {
        crate::session_ops::OpError::from(e)
            .context(format!("could not list windows for '{}'", session.name))
    })?;
    if alive {
        tracing::debug!("'{}' is running; nothing to relaunch", session.name);
        return Ok(false);
    }
    Ok(true)
}

/// Replace a session's window with the plan's on its backend, and point the
/// row at the new pane.
fn respawn(
    db: &Database,
    backend: &dyn crate::backend::SessionBackend,
    session: &SharedSession,
    plan: &RestartPlan,
) -> Result<(), crate::session_ops::OpError> {
    let id = session.id.to_string();
    // By the windows' own stamps, agent and companion shell together: the
    // window *name* is not unique, and killing by name with a duplicate around
    // tears down an arbitrary one of them. The shell goes because it was opened
    // beside the agent this restart replaces; the interface reopens one on
    // demand. A window that is already gone is not an error — it is what makes
    // this the *respawn* path too: a session whose server died is restarted by
    // asking for exactly this.
    //
    // A backend that cannot say what is there is a refusal, before anything is
    // spawned: launching beside windows nobody could see — or on a socket the
    // host has not vouched for — puts a second agent on the conversation.
    super::windows::kill_owned(backend, owner_of(session, &id)).map_err(|e| {
        crate::session_ops::OpError::from(e).context(format!(
            "cannot restart '{}': could not take down its windows on {}",
            session.name,
            backend.name()
        ))
    })?;
    // A successful kill reply need not mean the old window went away. In
    // particular, a mux can report an unresolved target with status zero.
    // Starting another agent then puts two processes on one conversation.
    if super::windows::agent_running(backend, owner_of(session, &id)).map_err(|e| {
        crate::session_ops::OpError::from(e)
            .context(format!("cannot verify teardown of '{}'", session.name))
    })? {
        return Err(format!(
            "cannot restart '{}': its agent window is still present after teardown; \
             no replacement was launched",
            session.name
        )
        .into());
    }

    let pane = backend
        .create_window(&crate::backend::WindowSpec {
            owner: crate::backend::Owner::new(&id, &plan.window_name),
            role: crate::backend::WindowRole::Agent,
            command: &plan.command,
            args: &plan.args,
            cwd: plan.cwd.as_deref(),
            env: &plan.env,
        })
        .map_err(|e| {
            crate::session_ops::OpError::from(e)
                .context(format!("Failed to spawn a window on {}", backend.name()))
        })?;

    // The new pane is a different one, and the id is how every later read
    // finds it — leaving the old one persisted would point the interface at a
    // pane that no longer exists.
    match record_pane(db, session, &pane) {
        Recorded::Stored => {}
        Recorded::RowGone => {
            // The row lost the race (deleted between the load above and here),
            // so nothing will ever attach to this window — a force-deleted row
            // is never reaped. Kill what was just spawned rather than leave it
            // running forever.
            let spawned = crate::backend::Owner::new(&id, &plan.window_name).remembering(&pane, "");
            if let Err(kill_err) = super::windows::kill_owned(backend, spawned) {
                tracing::warn!(
                    "could not kill orphaned restart window for '{}' on {}: {kill_err:#}",
                    session.name,
                    backend.name()
                );
            }
            return Err(crate::session_ops::OpError::Gone(
                Recorded::deleted_mid_restart(&session.name),
            ));
        }
        Recorded::Occupied => {
            return Err(crate::session_ops::OpError::Conflict(format!(
                "Failed to record the new pane for '{}': another live session already claims it",
                session.name
            )));
        }
        // The agent is running; only the row lags. Reported, not killed.
        Recorded::WriteFailed(e) => return Err(e.into()),
    }
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
            key: None,
        }
    }

    /// A hold is taken against a config dir of the test's own: sizing one
    /// reads `hooks.toml`, and seeding the operator's is not this suite's to do.
    fn hooks_isolated() -> (tempfile::TempDir, crate::paths::TestPathGuard) {
        let temp = tempfile::TempDir::new().expect("tempdir");
        let guard = crate::paths::TestPathGuard::new(temp.path());
        (temp, guard)
    }

    /// The race #1207 reports, at the only point the two processes meet. A
    /// restart is kill-then-spawn, and a repairer asked in between is told —
    /// honestly — that the window is gone. Nothing in tmux can tell that apart
    /// from an agent that died, so the row itself has to say so.
    #[test]
    fn a_relaunch_declines_a_row_that_is_already_being_restarted() {
        let (_temp, _guard) = hooks_isolated();
        let db = Database::open_in_memory().unwrap();
        let row = session(Some("conversation"), None);
        db.upsert_session(&row).unwrap();

        let held = hold_restart(&db, row.id).expect("the restart wins the row");

        // `--if-missing` is a repairer, and it must not put a second agent on a
        // window somebody is in the middle of replacing. Answered before tmux
        // is asked anything, so this needs no server.
        assert_eq!(
            restart_session_headless_with(&db, &crate::backend::registry::inert(), row.id, true),
            Ok(RestartReport::default()),
            "a relaunch of a row being restarted is not owed"
        );

        drop(held);
        assert!(
            hold_restart(&db, row.id).is_some(),
            "the restart ending gives the row back"
        );
    }

    /// A session on `mux`, with a conversation to resume, served by a stub.
    fn on(
        mux: crate::session::Multiplexer,
    ) -> (Database, crate::backend::BackendRegistry, SharedSession) {
        let route = crate::session::Route::local(Some(mux));
        let backends = crate::backend::registry::inert_serving(std::slice::from_ref(&route));
        let db = Database::open_in_memory().unwrap();
        let mut row = session(Some("conversation"), None);
        row.backend_type = route.format();
        db.upsert_session(&row).unwrap();
        (db, backends, row)
    }

    /// The agent exits, and its window goes with it — what psmux does.
    fn exit(db: &Database, backends: &crate::backend::BackendRegistry, row: &SharedSession) {
        let pane = db.get_session_by_id(row.id).unwrap().unwrap().backend_id;
        let backend = super::super::windows::backend_for(backends, &row.backend_type).unwrap();
        backend.kill(&pane).unwrap();
    }

    fn relaunches(db: &Database, row: &SharedSession) -> usize {
        db.relaunches_since(row.id, 0).unwrap()
    }

    /// Issue: on psmux every agent exit read as a vanished window, which each
    /// interface and every `restart --if-missing` relaunched — forever, and
    /// with nothing left behind to say it had happened.
    #[test]
    fn a_crash_loop_on_psmux_is_held_back_and_marked_lost() {
        let (_temp, _guard) = hooks_isolated();
        let (db, backends, row) = on(crate::session::Multiplexer::Psmux);
        let relaunch = || restart_session_headless_with(&db, &backends, row.id, true).unwrap();

        for n in 1..=CRASH_LOOP_RELAUNCHES {
            exit(&db, &backends, &row);
            assert!(!relaunch().held_back, "relaunch {n} is owed");
            assert_eq!(relaunches(&db, &row), n, "each relaunch is recorded");
        }

        exit(&db, &backends, &row);
        assert!(relaunch().held_back, "the next one is a crash loop");
        assert_eq!(relaunches(&db, &row), CRASH_LOOP_RELAUNCHES);
        assert!(db.load_lost_sessions().unwrap().contains_key(&row.id));
        // Nothing was launched, and the dead pane's id — free for the server
        // to reissue — is no longer the row's.
        assert_eq!(
            db.get_session_by_id(row.id).unwrap().unwrap().backend_id,
            "",
            "a held-back row names no pane"
        );
        assert!(
            relaunch().held_back,
            "a second repairer is held back as well"
        );

        // The operator's own restart is never held back, and is what lifts it.
        let asked = restart_session_headless_with(&db, &backends, row.id, false).unwrap();
        assert!(!asked.held_back);
        assert!(!db.load_lost_sessions().unwrap().contains_key(&row.id));
    }

    /// Review of #1390: the guard was counted from the event log, which every
    /// database open prunes on the audit retention — zero included — and a
    /// hold anchored there was released once its relaunches aged out of it.
    #[test]
    fn a_hold_survives_a_pruned_event_log_and_an_aged_window() {
        let (_temp, _guard) = hooks_isolated();
        let (db, backends, row) = on(crate::session::Multiplexer::Psmux);
        let relaunch = || restart_session_headless_with(&db, &backends, row.id, true).unwrap();

        // A zero retention prunes the log after every relaunch.
        for _ in 0..CRASH_LOOP_RELAUNCHES {
            exit(&db, &backends, &row);
            assert!(!relaunch().held_back);
            db.conn_ref()
                .execute("DELETE FROM session_events", [])
                .unwrap();
        }
        exit(&db, &backends, &row);
        assert!(relaunch().held_back, "the loop is still counted");

        // Long after the window: the relaunches have aged out, the hold has not.
        db.conn_ref()
            .execute("UPDATE session_relaunches SET at_ms = 0", [])
            .unwrap();
        assert!(relaunch().held_back, "the hold outlives its window");

        // Only a pane recorded again lifts it.
        restart_session_headless_with(&db, &backends, row.id, false).unwrap();
        exit(&db, &backends, &row);
        assert!(
            !relaunch().held_back,
            "after the operator's restart, and with the old loop aged out, a relaunch is owed"
        );
    }

    /// Review of #1390: the interface reported a held-back relaunch as
    /// "restarted" (and let go of a pane), because the kernel dropped the
    /// report. As a command outcome it is a failure that says why.
    #[test]
    fn a_held_back_relaunch_is_a_failed_command_with_its_reason() {
        let held = RestartReport {
            held_back: true,
            ..RestartReport::default()
        };
        let error = held.into_outcome().unwrap_err();
        assert!(error.contains("marked lost"), "{error}");
        assert_eq!(RestartReport::default().into_outcome(), Ok(()));
        let hook_failed = RestartReport {
            hook_failures: vec!["post".into()],
            ..RestartReport::default()
        };
        assert_eq!(
            hook_failed.into_outcome(),
            Ok(()),
            "a failed post-restart hook still leaves the restart standing"
        );
    }

    /// A loss with no relaunches behind it — a reboot — is relaunched as
    /// before, however long ago it was recorded.
    #[test]
    fn a_lost_session_with_no_crash_loop_is_relaunched() {
        let (_temp, _guard) = hooks_isolated();
        let (db, backends, row) = on(crate::session::Multiplexer::Psmux);
        exit(&db, &backends, &row);
        db.record_session_lost(row.id, &row.backend_id).unwrap();
        let report = restart_session_headless_with(&db, &backends, row.id, true).unwrap();
        assert!(!report.held_back);
        assert_eq!(relaunches(&db, &row), 1);
    }

    /// tmux keeps an exited agent's window, so "missing" there is never an
    /// exit: its relaunches are neither recorded nor counted.
    #[test]
    fn a_multiplexer_that_keeps_exited_windows_is_never_held_back() {
        let (_temp, _guard) = hooks_isolated();
        let (db, backends, row) = on(crate::session::Multiplexer::Tmux);
        for _ in 0..=CRASH_LOOP_RELAUNCHES {
            exit(&db, &backends, &row);
            let report = restart_session_headless_with(&db, &backends, row.id, true).unwrap();
            assert!(!report.held_back);
        }
        assert_eq!(relaunches(&db, &row), 0);
    }

    /// `session start` is the caller that looks like a repairer and is not. It
    /// asks `if_missing` so an unpark of a session that already has a window
    /// does not spawn a second — but a hold left behind by a restart killed
    /// mid-flight outlives it by minutes, and standing down for one would make
    /// `start` clear the parked mark, report success and leave the session with
    /// no window at all until the hold expired. The second predicate is what a
    /// delegated restart puts on the wire too, so the same unpark must not
    /// reach a host as `--if-missing` — see
    /// `shared_tests::an_unpark_reaches_the_host_as_a_plain_restart`.
    #[test]
    fn only_a_repairer_stands_down_for_a_hold() {
        assert!(!Relaunch::Asked.only_if_missing());
        assert!(!Relaunch::Asked.is_a_repairer());

        assert!(Relaunch::IfMissing.only_if_missing());
        assert!(Relaunch::IfMissing.is_a_repairer());

        assert!(Relaunch::Unparking.only_if_missing());
        assert!(!Relaunch::Unparking.is_a_repairer());
    }

    /// A hold is per row, and given back on every path out of the restart that
    /// took it — the reason it is a guard rather than a call at each return.
    #[test]
    fn only_one_restart_holds_a_row_at_a_time() {
        let (_temp, _guard) = hooks_isolated();
        let db = Database::open_in_memory().unwrap();
        let mine = session(None, None);
        let other = session(None, None);

        let held = hold_restart(&db, mine.id).expect("the first restart wins");
        assert!(
            hold_restart(&db, mine.id).is_none(),
            "the second must not start killing windows"
        );
        assert!(
            hold_restart(&db, other.id).is_some(),
            "a hold says nothing about any other session"
        );

        drop(held);
        assert!(
            hold_restart(&db, mine.id).is_some(),
            "the row is free again once the restart is over"
        );
    }

    /// A restart killed mid-flight — between its kill and its spawn is exactly
    /// where a `SIGKILL` hurts — must not lock its own row out forever. The
    /// expiry is what bounds that, and it is the claim's token too, so the
    /// holder that overran cannot release its successor's.
    #[test]
    fn a_restart_hold_left_behind_expires_rather_than_sticking() {
        let (_temp, _guard) = hooks_isolated();
        let db = Database::open_in_memory().unwrap();
        let row = session(None, None);
        let held = crate::storage::event_cursors::LockSubject::Session(row.id);
        let now = crate::sync::current_time_millis();

        assert!(db.take_lease(&held, now - 1, now).unwrap());
        let successor = hold_restart(&db, row.id).expect("an expired hold is taken over");

        db.release_lease(&held, now - 1).unwrap();
        assert!(
            hold_restart(&db, row.id).is_none(),
            "releasing the dead hold must leave the successor's alone"
        );
        drop(successor);
    }

    /// A `pre_restart` hook runs the user's own program between the row being
    /// read and the window being killed, so "it was active a moment ago" is not
    /// an answer. A restart that went ahead here spawned an agent for a row the
    /// sweep was on its way to collect — and, when it still wrote the whole row
    /// back, revived the delete outright.
    #[test]
    fn a_restart_refuses_a_row_deleted_since_it_was_loaded() {
        let db = Database::open_in_memory().unwrap();
        let row = session(None, None);
        db.upsert_session(&row).unwrap();

        refuse_if_deleted(&db, &row).expect("an active row restarts");

        db.soft_delete_session(row.id).unwrap();
        let error = refuse_if_deleted(&db, &row).unwrap_err();
        assert!(
            error.contains("was deleted while it was preparing to restart"),
            "got {error}"
        );
    }

    /// [`record_pane`] must tell a row that is genuinely gone apart from a write
    /// that merely failed — only the former means the window it just spawned is
    /// nobody's. The peer-lock trick the storage tests use to force `SQLITE_BUSY`
    /// no longer reaches this path: `claim_backend_id` takes the write lock up
    /// front and waits it out. Dropping the table out from under the connection
    /// forces a real, non-deletion storage error instead.
    #[test]
    fn record_pane_distinguishes_a_deleted_row_from_a_failed_write() {
        let db = Database::open_in_memory().unwrap();

        let stored = session(None, None);
        db.upsert_session(&stored).unwrap();
        assert!(matches!(record_pane(&db, &stored, "%1"), Recorded::Stored));

        let gone = session(None, None);
        db.upsert_session(&gone).unwrap();
        db.soft_delete_session(gone.id).unwrap();
        assert!(matches!(record_pane(&db, &gone, "%2"), Recorded::RowGone));

        let live = session(None, None);
        db.upsert_session(&live).unwrap();
        db.conn_ref().execute_batch("DROP TABLE sessions").unwrap();
        assert!(matches!(
            record_pane(&db, &live, "%3"),
            Recorded::WriteFailed(_)
        ));
    }

    #[test]
    fn restart_plan_requires_agent_session_id() {
        let temp = tempfile::TempDir::new().unwrap();
        let _guard = crate::paths::TestPathGuard::new(temp.path());
        let db = Database::open_in_memory().unwrap();
        let err = build_restart_plan(
            &db,
            &session(None, None),
            None,
            None,
            true,
            None,
            &Default::default(),
        )
        .unwrap_err();
        assert!(err.contains("agent_session_id"), "got: {err}");
    }

    #[test]
    fn restart_plan_injects_identity_env() {
        let temp = tempfile::TempDir::new().unwrap();
        let _guard = crate::paths::TestPathGuard::new(temp.path());
        let db = Database::open_in_memory().unwrap();
        let sess = session(Some("agent-conv-uuid"), Some(PathBuf::from("/tmp/repo")));
        let plan =
            build_restart_plan(&db, &sess, None, None, true, None, &Default::default()).unwrap();

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
        let db = Database::open_in_memory().unwrap();
        let primary = temp.path().join("primary");
        std::fs::create_dir_all(&primary).unwrap();
        let plan = build_restart_plan(
            &db,
            &session(Some("sid"), Some(primary.clone())),
            None,
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
        let db = Database::open_in_memory().unwrap();
        let primary = temp.path().join("primary");
        std::fs::create_dir_all(&primary).unwrap();
        let extra = temp.path().join("extra");
        std::fs::create_dir_all(&extra).unwrap();

        let mut sess = session(Some("sid-multi"), Some(primary.clone()));
        sess.additional_dirs = vec![extra];

        let plan =
            build_restart_plan(&db, &sess, None, None, true, None, &Default::default()).unwrap();
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
        let db = Database::open_in_memory().unwrap();
        let mut sess = session(Some("agent-conv-uuid"), Some(PathBuf::from("/srv/repo")));
        sess.backend_type = "ssh:devbox".into();

        let plan =
            build_restart_plan(&db, &sess, None, None, true, None, &Default::default()).unwrap();
        assert_eq!(plan.env.get("THURBOX_SESSION"), Some(&sess.id.to_string()));
        assert!(!plan.env.contains_key(crate::paths::CONFIG_DIR_OVERRIDE_ENV));
        assert!(!plan.env.contains_key("THURBOX_METRICS_DIR"));
    }
}
