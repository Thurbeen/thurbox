//! Headless session deletion — soft-delete by default, `force` also tears
//! down the tmux windows, worktrees, and pending scheduled commands so the
//! filesystem and tmux server don't leak orphans when the TUI isn't
//! running to observe the deletion.

use crate::session::SessionId;
use crate::storage::{Database, DeletedSessionInfo};

/// Outcome of a force-delete, reported to callers for their JSON payload.
#[derive(Debug, Clone, Default)]
pub struct ForceDeleteReport {
    pub killed_window: bool,
    pub removed_worktrees: Vec<String>,
    pub worktree_errors: Vec<String>,
    /// Worktrees left on disk because thurbox did not create them. Reported so
    /// the caller can say what it deliberately did *not* delete — silence here
    /// would read as "nothing to clean up".
    pub kept_worktrees: Vec<String>,
    pub disabled_automations: usize,
    /// Set when the session lived on a remote host (SSH/WSL) and its window
    /// could not be torn down there: the host is unreachable, has no
    /// `hosts.toml` entry, or the session carries no pane id. Best-effort — an
    /// unreachable host is expected (that's often *why* someone force-deletes),
    /// so this is recorded rather than aborting the delete.
    pub remote_teardown_error: Option<String>,
    /// Set when the host was asked to delete the session and answered that it
    /// has no such row — a fork minted here, a row from before ADR-24, or one
    /// a peer already deleted there. The delete is then taken from here
    /// instead of failing, and this says so: the alternative was an error that
    /// left the local row active and attached, which reads as "delete does
    /// nothing".
    pub host_unknown: Option<String>,
    /// `session.post_delete` hooks that failed. The delete stands regardless.
    pub hook_failures: Vec<String>,
}

/// Soft-delete a session and (when `force`) also tear down its runtime
/// resources: the tmux windows, on-disk worktrees, and any pending
/// scheduled commands queued against it.
///
/// Worktree and tmux cleanup are best-effort — individual failures are
/// captured in the report but do not abort the delete. The row is marked in
/// one write (`delete_row`) and, for a force delete, **before** the teardown:
/// a watcher never sees an intermediate state where a force-delete reads as
/// restorable, and a crash partway through the teardown cannot leave an active
/// row whose worktrees are already gone.
pub fn delete_session_headless(
    db: &Database,
    session_id: SessionId,
    force: bool,
) -> Result<ForceDeleteReport, String> {
    let session = db
        .get_session_by_id(session_id)
        .map_err(|e| format!("get_session_by_id: {e}"))?
        .ok_or_else(|| format!("Session not found: {session_id}"))?;

    // The user's say, before anything is torn down or marked: a refusal here
    // leaves the row exactly as it was.
    let mut hook_ctx = super::lifecycle_hooks::context_for(&session);
    hook_ctx.force = Some(force);
    super::fire_pre(crate::session::HookEvent::PreDelete, &hook_ctx)?;

    let mut report = ForceDeleteReport::default();

    // A session on a shareable host is the host's to delete: its CLI kills the
    // window and removes the worktrees where they are, and marks its own row;
    // the local row is marked from its answer. Soft or forced as asked.
    if let Some(host) = super::resolve_host(&session.backend_type).flatten() {
        if let Some(cli) = super::host_cli::delegated(&host) {
            let id = session_id.to_string();
            let mut args = vec!["session", "delete", &id];
            if force {
                args.push("--force");
            }
            let answer = match super::host_cli::run(&host, &cli, &args) {
                Ok(answer) => answer,
                // The host does not know this row, so there is nothing there to
                // delegate to — but the row here is real and the caller asked
                // for it to go. Take the delete from here instead: the teardown
                // is addressed by uuid, so it still reaches exactly this
                // session's windows on that host if they are there at all.
                Err(e) if is_unknown_session(&e) => {
                    report.host_unknown = Some(format!(
                        "'{}' does not know this session ({e}); deleted from here",
                        host.name
                    ));
                    return finish_locally(db, &session, force, report, &hook_ctx);
                }
                Err(e) => return Err(e),
            };
            report.killed_window = answer
                .get("killed_window")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false);
            report.removed_worktrees = string_list(&answer, "removed_worktrees");
            report.kept_worktrees = string_list(&answer, "kept_worktrees");
            report.worktree_errors = string_list(&answer, "worktree_errors");
            report.remote_teardown_error = answer
                .get("remote_teardown_error")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string);
            report.disabled_automations = db
                .disable_send_automations_for_session(session_id)
                .map_err(|e| format!("disable_send_automations_for_session: {e}"))?;
            delete_row(db, session_id, force)?;
            report.hook_failures =
                super::fire_post(crate::session::HookEvent::PostDelete, &hook_ctx);
            return Ok(report);
        }
    }

    finish_locally(db, &session, force, report, &hook_ctx)
}

/// The delete taken from here: mark the row, tear down what `force` asks for,
/// fire the post hooks. Shared by the plain path and by a delegated delete the
/// host could not take (see [`is_unknown_session`]).
fn finish_locally(
    db: &Database,
    session: &crate::sync::SharedSession,
    force: bool,
    report: ForceDeleteReport,
    hook_ctx: &crate::session::HookContext,
) -> Result<ForceDeleteReport, String> {
    finish_locally_with(db, session, force, report, hook_ctx, |session, report| {
        teardown_runtime_resources(session, report);
    })
}

/// [`finish_locally`] with the teardown injected, so a test can look at the row
/// from *inside* it — which is the only place the ordering below is visible.
fn finish_locally_with(
    db: &Database,
    session: &crate::sync::SharedSession,
    force: bool,
    mut report: ForceDeleteReport,
    hook_ctx: &crate::session::HookContext,
    teardown: impl FnOnce(&crate::sync::SharedSession, &mut ForceDeleteReport),
) -> Result<ForceDeleteReport, String> {
    // Marked **before** anything is torn down. The teardown kills windows and
    // removes worktrees, and a crash or a kill signal partway through used to
    // leave an active, restore-listed row whose worktrees were already gone —
    // a restore that could only hand back a session with nothing in it. The
    // mark is the one write that makes the loss survivable news.
    delete_row(db, session.id, force)?;

    if force {
        teardown(session, &mut report);
        report.disabled_automations = db
            .disable_send_automations_for_session(session.id)
            .map_err(|e| format!("disable_send_automations_for_session: {e}"))?;
    }

    report.hook_failures = super::fire_post(crate::session::HookEvent::PostDelete, hook_ctx);

    Ok(report)
}

/// Whether a host CLI's refusal means "no such session here".
///
/// The host resolves the id against its *active* rows, so it answers this for
/// a session it never knew and for one it has already deleted alike — both of
/// which are cases where the local row still has to go.
fn is_unknown_session(error: &str) -> bool {
    error.contains("Session not found")
}

/// Mark the row gone. A force delete says so in the same statement rather than
/// marking twice: the restore list needs to know the worktrees and window were
/// torn down, and a watcher must not first be told the session is restorable.
fn delete_row(db: &Database, session_id: SessionId, force: bool) -> Result<(), String> {
    if force {
        db.force_delete_session(session_id)
            .map_err(|e| format!("force_delete_session: {e}"))?;
        db.clear_session_meta(session_id)
            .map_err(|e| format!("clear_session_meta: {e}"))?;
    } else {
        db.soft_delete_session(session_id)
            .map_err(|e| format!("soft_delete_session: {e}"))?;
    }
    Ok(())
}

fn string_list(answer: &serde_json::Value, key: &str) -> Vec<String> {
    answer
        .get(key)
        .and_then(serde_json::Value::as_array)
        .map(|list| {
            list.iter()
                .filter_map(serde_json::Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// Tear down a session's slow runtime resources: kill the tmux windows
/// (agent and companion shell), remove worktrees and the symlink workspace.
/// Touches no SQLite — safe to call from a background thread after the row
/// has been soft-deleted on the UI thread, so the TUI's hard-delete
/// confirmation can close without blocking on a remote `kill-window` or a
/// `git worktree remove`. Best-effort: failures are logged into `report`
/// (or `tracing::warn`), never abort.
///
/// **Backend-aware.** The window kills and each worktree removal run on the
/// server the session actually lives on, resolved from `session.backend_type`:
/// a local backend uses the local tmux socket + local `git`; an `ssh:`/`wsl:`
/// backend kills the panes and removes the worktrees over that host's launcher.
/// The symlink workspace is always local (a spawn-time process-cwd detail under
/// the local data dir), so it is torn down regardless of backend.
pub fn teardown_runtime_resources(
    session: &crate::sync::SharedSession,
    report: &mut ForceDeleteReport,
) {
    if crate::session::is_remote_backend(&session.backend_type) {
        // Off-local session: kill the pane + remove worktrees on the host. An
        // unresolvable/unreachable host is expected — record it, never abort.
        let registry = crate::agent::host_config::load_all();
        match registry.get_by_backend(&session.backend_type) {
            Some(host) => {
                kill_remote_window(host, session, report);
                for wt in &session.worktrees {
                    remove_worktree_into(Some(host), wt, report);
                }
            }
            None => {
                let msg = format!(
                    "remote host '{}' not found in hosts.toml; \
                     left its window + worktrees in place",
                    session.backend_type
                );
                tracing::warn!("{msg}");
                report.remote_teardown_error = Some(msg);
            }
        }
    } else {
        kill_local_window(session, report);
        for wt in &session.worktrees {
            remove_worktree_into(None, wt, report);
        }
    }

    // Tear down the multi-repo symlink workspace (if any). Only the symlinks
    // are removed — the underlying repos are untouched. Always local: the
    // workspace lives under the local data dir even for a remote session.
    if let Some(asid) = &session.agent_session_id {
        if let Err(e) = crate::workspace::remove_workspace(asid) {
            tracing::warn!("remove_workspace({asid}) failed: {e}");
        }
    }
}

/// How long a soft-deleted session keeps its agent, so the delete can still be
/// undone. v1's `UNDO_TIMEOUT`, and the one place the undo window is stated:
/// the interface and the headless heartbeat both sweep against it.
pub const UNDO_WINDOW: std::time::Duration = std::time::Duration::from_secs(10);

/// Let go of the agents of every session soft-deleted longer ago than the undo
/// window, when the row still owns a window. Returns the ids reaped.
///
/// The one reaper. It asks the durable question — `deleted_at + UNDO_WINDOW` —
/// so it gives the same answer with or without an interface running (ADR-8b),
/// survives a restart of whatever is calling it, and is idempotent: a row
/// reaped once owns nothing on the next pass. It replaced an in-memory watcher
/// that inferred "was deleted" from "left the snapshot", which is a proxy for
/// deletion that anything else emptying the snapshot also armed, and whose
/// state a restart lost.
///
/// Only rows with windows of their *own* are touched, so a stale row is never
/// reported as reaped on the strength of a live namesake's window. The gate is
/// [`owned_windows_in`], the same and only ownership test the reap itself
/// applies.
///
/// A remote row is gated the same way, against its host's own listing rather
/// than this machine's: a session soft-deleted on a host has no interface there
/// to collect it, and leaving it was how every headless delete of a remote
/// session leaked its `tb-`/`tbs-` pair for good. That costs one
/// `list-windows` per host, and only for a host with a row actually overdue.
///
/// Window ownership is the whole gate, so a row that owns none is skipped
/// entirely and its metrics file and symlink workspace are left in place —
/// artifacts [`reap_soft_deleted`] would otherwise release. Accepted rather
/// than worked around: without an "already reaped" marker on the row,
/// ownership is the only idempotence proxy the sweep has, and releasing
/// artifacts on every tick forever is the worse trade.
pub fn reap_overdue_soft_deletes(db: &Database) -> Vec<String> {
    let Ok(rows) = db.list_deleted_sessions() else {
        return Vec::new();
    };
    let now = crate::sync::current_time_millis();
    let window = UNDO_WINDOW.as_millis() as u64;
    let mut reaped = Vec::new();
    let mut windows: std::collections::HashMap<String, crate::agent::tmux::WindowIndex> =
        std::collections::HashMap::new();
    for row in rows {
        if row.force_deleted || now.saturating_sub(row.deleted_at) < window {
            continue;
        }
        let index = windows
            .entry(row.backend_type.clone())
            .or_insert_with(|| window_index_on(&row.backend_type));
        if owned_windows_in(index, &row).is_empty() {
            continue;
        }
        match reap_soft_deleted(db, row.id) {
            Ok(true) => reaped.push(row.id.to_string()),
            Ok(false) => {}
            Err(e) => tracing::warn!("reap of '{}': {e}", row.name),
        }
    }
    reaped
}

/// Every thurbox window on the server `backend_type` names. An empty index for
/// a host that is unconfigured or unreachable, which reads as "owns nothing" —
/// the conservative answer a teardown gate wants.
fn window_index_on(backend_type: &str) -> crate::agent::tmux::WindowIndex {
    if !crate::session::is_remote_backend(backend_type) {
        return crate::agent::tmux::local_window_index().unwrap_or_default();
    }
    match super::resolve_host(backend_type).flatten() {
        Some(host) => crate::agent::tmux::remote_window_index(&host).unwrap_or_else(|e| {
            tracing::debug!("could not list the windows of '{}': {e:#}", host.name);
            crate::agent::tmux::WindowIndex::default()
        }),
        None => crate::agent::tmux::WindowIndex::default(),
    }
}

/// Release what a *soft*-deleted session is still holding: its agent, its
/// companion shell, its metrics file and its symlink workspace.
///
/// Deleting softly is meant to be undoable, so the row is kept and the worktrees
/// stay on disk — but the agent process is not part of what an undo restores, and
/// leaving it running means a deleted session keeps working, keeps writing, and
/// keeps its tmux windows forever. v1 killed the agent's once the undo window closed;
/// this is that, callable without a TUI. Whose turn it is to call it is
/// [`reap_overdue_soft_deletes`], the one sweep both the interface and the
/// headless heartbeat drive.
///
/// Worktrees are deliberately untouched: they are what makes the undo lossless.
///
/// A session on a remote host is reaped *there* rather than left running: a
/// host has no interface of its own to collect it, so "left alone" meant
/// forever.
///
/// Returns whether the row was processed — `false` when it came back (the user
/// undid it) or was force-deleted (already torn down), both of which are
/// ordinary races rather than failures. It is not a claim that a window came
/// down: a row that owns none (see [`owned_windows_in`]) still releases its
/// derived artifacts and reports `true`.
pub fn reap_soft_deleted(db: &Database, id: SessionId) -> Result<bool, String> {
    let Some(row) = db
        .get_deleted_session_by_id(id)
        .map_err(|e| format!("get deleted session: {e}"))?
    else {
        // Restored between the delete and the reap: the session is alive again,
        // and killing its agent now would be the bug this guard exists to avoid.
        return Ok(false);
    };
    if row.force_deleted {
        return Ok(false);
    }

    // A remote session's windows live on its host, so the reap goes there.
    // Leaving them was the old answer, and it meant every soft delete of a
    // remote session leaked a `tb-`/`tbs-` pair forever — recreating the name
    // then put a second agent beside the first.
    if crate::session::is_remote_backend(&row.backend_type) {
        reap_remote(&row);
    } else {
        // Strict: kill only the windows this row still owns. A reap must not
        // resolve a pane id or a `tb-<name>` target another row answers to —
        // that is how deleting a frozen session came to kill its replacement
        // 30-60s later, and why each delete-and-recreate made the next one die
        // sooner.
        let owned = owned_windows(&row);
        if owned.is_empty() {
            // The windows may already be gone — the agent exited, or a previous
            // reap got there first. Logged rather than silent: owning nothing is
            // also the shape a misdirected kill used to take.
            tracing::debug!(
                "reap of '{}': pane {:?} owns no window it may kill; \
                 leaving any same-named window alone",
                row.name,
                row.backend_id
            );
        }
        for target in owned {
            // Not worth failing a cleanup over if the window went away underneath.
            if let Err(e) = crate::agent::tmux::kill_window_at(&target) {
                tracing::debug!("kill_window_at({target}) during reap: {e}");
            }
        }
    }

    // Derived per-session artifacts, both rebuilt on restore.
    if let Some(asid) = &row.agent_session_id {
        if let Some(dir) = crate::paths::metrics_directory() {
            let _ = std::fs::remove_file(dir.join(format!("{asid}.json")));
        }
        if let Err(e) = crate::workspace::remove_workspace(asid) {
            tracing::warn!("remove_workspace({asid}) during reap: {e}");
        }
    }
    Ok(true)
}

/// The windows a soft-deleted row still owns — its agent and its companion
/// shell — the only targets its reap may kill, and the answer to whether it has
/// anything left to release.
///
/// The one place ownership is decided, so the reap and the headless sweep that
/// gates on it cannot drift apart. It is the window's own stamp that settles
/// it (ADR-25): the row's remembered pane id proves nothing after a tmux
/// server has reissued it, and the `tb-<name>` a namesake shares proves less.
/// The shell is included for the same reason it is included in every other
/// teardown — `shell_backend_id` is written only once the interface has opened
/// one, so the column is no guide to whether the window is there.
///
/// Conservatively owns nothing when the listing fails or cannot tell: leaking a
/// window costs a stale agent, killing the wrong one costs live work.
fn owned_windows(row: &DeletedSessionInfo) -> Vec<String> {
    match crate::agent::tmux::local_window_index() {
        Ok(index) => owned_windows_in(&index, row),
        Err(_) => Vec::new(),
    }
}

/// The windows a soft-deleted row owns, against a listing the caller already
/// holds — so a sweep over several rows pays for one `list-windows` instead of
/// one each.
///
/// A window whose pane has already exited still counts: `remain-on-exit` keeps
/// it on the server, and leaving it there is the leak the reap exists to stop.
pub fn owned_windows_in(
    index: &crate::agent::tmux::WindowIndex,
    row: &DeletedSessionInfo,
) -> Vec<String> {
    let id = row.id.to_string();
    [
        index.agent_window(&id, &row.name),
        index.shell_window(&id, &row.name),
    ]
    .into_iter()
    .filter_map(crate::agent::tmux::Located::pane)
    .collect()
}

/// Let go of a soft-deleted remote session's windows on the host they run on.
///
/// A host that runs a thurbox of its own is asked to reap the row itself
/// (`session reap`): its database holds the same row, and only the host can
/// mark it there. Otherwise the windows are killed from here, resolved by the
/// stamp exactly as the local branch does.
///
/// Best-effort and silent about the ordinary cases: an unreachable host is
/// often *why* the session was deleted, and the reap runs on a timer.
fn reap_remote(row: &DeletedSessionInfo) {
    let Some(Some(host)) = super::resolve_host(&row.backend_type) else {
        tracing::warn!(
            "'{}' was soft-deleted on {}, which is not in hosts.toml; \
             its windows are left running there",
            row.name,
            row.backend_type
        );
        return;
    };
    if let Some(cli) = super::host_cli::delegated(&host) {
        let id = row.id.to_string();
        match super::host_cli::run(&host, &cli, &["session", "reap", &id]) {
            Ok(_) => return,
            // The host has no row to reap — the delete was taken here while it
            // was unreachable, and the mirror has not pushed it yet. Its
            // windows are still there, so they come down from here.
            Err(e) if e.contains("not found") => {
                tracing::debug!("'{}' is unknown on '{}': {e}", row.name, host.name);
            }
            Err(e) => {
                tracing::warn!("reap of '{}' on '{}': {e}", row.name, host.name);
                return;
            }
        }
    }
    let panes = crate::agent::tmux::SessionPanes {
        agent: &row.backend_id,
        shell: row.shell_backend_id.as_deref().unwrap_or_default(),
    };
    if let Err(e) =
        crate::agent::tmux::kill_remote_windows(&host, &row.id.to_string(), &row.name, panes)
    {
        tracing::warn!("reap of '{}' on '{}': {e}", row.name, host.name);
    }
}

/// Kill the session's window on the local tmux server, reaping the pane's child
/// process on Windows (where a live process's cwd blocks the later rmdir).
fn kill_local_window(session: &crate::sync::SharedSession, report: &mut ForceDeleteReport) {
    // Capture the pane's OS pid *before* the kill so we can reap the pane's child
    // process below. Windows refuses to remove a directory that is a live
    // process's cwd, and a session's agent runs with cwd = its worktree /
    // extension home; Unix has no such restriction, so this is Windows-only.
    #[cfg(windows)]
    let pane_pid = crate::agent::tmux::window_pane_pid(&session.id.to_string(), &session.name)
        .ok()
        .flatten();

    match crate::agent::tmux::kill_window(&session.id.to_string(), &session.name) {
        Ok(()) => report.killed_window = true,
        Err(e) => tracing::warn!("kill_window({}) failed: {e}", session.name),
    }

    // The companion shell is the session's second window and nothing else ever
    // takes it down — leaving it is how a force-deleted session kept a live
    // `tbs-` window on the server for good.
    if let Err(e) = crate::agent::tmux::kill_shell_window(&session.id.to_string(), &session.name) {
        tracing::warn!("kill_shell_window({}) failed: {e}", session.name);
    }

    // `kill-window` returns before the OS reaps the pane's child process; wait
    // for it (force-terminating as a backstop) before the rmdir steps below.
    // NOTE: this only handles a handle held by the *pane child*. psmux ALSO holds
    // a server-level handle to each pane's `-c` cwd that only `kill-server`
    // releases (verified in the Windows VM) — which we can't do per-session on
    // the shared server. So removing a just-deleted session's own working dir can
    // still fail on Windows; that is a documented psmux limitation, not covered
    // here.
    #[cfg(windows)]
    if let Some(pid) = pane_pid {
        reap_pane_process(pid);
    }
}

/// Kill the session's windows — agent and companion shell — on a remote host,
/// resolved by the stamp each carries. Best-effort: an unreachable host, or one
/// whose socket is not known, is recorded in `report.remote_teardown_error` and
/// never aborts the delete.
fn kill_remote_window(
    host: &crate::session::HostDef,
    session: &crate::sync::SharedSession,
    report: &mut ForceDeleteReport,
) {
    let panes = crate::agent::tmux::SessionPanes {
        agent: session.backend_id.trim(),
        shell: session
            .shell_backend_id
            .as_deref()
            .map(str::trim)
            .unwrap_or_default(),
    };
    match crate::agent::tmux::kill_remote_windows(
        host,
        &session.id.to_string(),
        &session.name,
        panes,
    ) {
        Ok(true) => report.killed_window = true,
        // Nothing there the row could claim: already gone, or a window the
        // host's listing attributes to somebody else. Not an error, and not a
        // kill either.
        Ok(false) => tracing::debug!(
            "'{}' owns no window on {} to kill",
            session.name,
            host.backend_name()
        ),
        Err(e) => {
            let msg = format!(
                "could not kill the remote window of '{}' on {}: {e}",
                session.name,
                host.backend_name()
            );
            tracing::warn!("{msg}");
            report.remote_teardown_error = Some(msg);
        }
    }
}

/// Wait (≈5s) for a killed pane's child process to exit, force-terminating it as
/// a backstop if it outlives the grace period. Windows-only: a live process's
/// cwd is unremovable on Windows, and a session's agent runs in its worktree.
/// This reaps the *pane child* only — psmux's own server-level handle on the
/// pane's `-c` cwd is a separate, un-fixable-per-session issue (see callsite).
#[cfg(windows)]
fn reap_pane_process(pid: u32) {
    let pid = sysinfo::Pid::from_u32(pid);
    let mut sys = sysinfo::System::new();
    let kind = sysinfo::ProcessRefreshKind::nothing();
    let mut terminated = false;
    for tick in 0..50 {
        // `remove_dead_processes = true` drops exited pids, so `process(pid)`
        // going `None` means the process is truly gone.
        sys.refresh_processes_specifics(sysinfo::ProcessesToUpdate::Some(&[pid]), true, kind);
        match sys.process(pid) {
            None => return,
            Some(proc) => {
                // Give it ~2s to exit on its own, then force the kill.
                if !terminated && tick >= 20 {
                    proc.kill();
                    terminated = true;
                }
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}

/// Best-effort worktree removal on `host` (local when `None`), recording
/// success/failure into `report`. Removes the worktree *directory* only — the
/// git branch is deliberately left behind (local and remote alike), matching
/// force-delete's contract. A worktree thurbox did not create is skipped
/// outright and recorded in `report.kept_worktrees`.
fn remove_worktree_into(
    host: Option<&crate::session::HostDef>,
    wt: &crate::sync::SharedWorktree,
    report: &mut ForceDeleteReport,
) {
    // Only what thurbox checked out. `git worktree remove --force` deletes the
    // directory along with any uncommitted work in it — fine for a worktree
    // thurbox made for this session, never acceptable for one the user already
    // had and merely opened.
    if !wt.created_by_thurbox {
        report
            .kept_worktrees
            .push(wt.worktree_path.display().to_string());
        return;
    }

    match crate::git::remove_worktree_on(host, &wt.repo_path, &wt.worktree_path) {
        Ok(()) => report
            .removed_worktrees
            .push(wt.worktree_path.display().to_string()),
        Err(e) => report
            .worktree_errors
            .push(format!("{}: {e}", wt.worktree_path.display())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::SessionId;
    use crate::sync::SharedSession;

    fn insert_session(db: &Database, name: &str) -> SessionId {
        insert_session_on(db, name, "local-tmux", "")
    }

    /// Insert a session with an explicit backend type + pane id, so the remote
    /// teardown paths can be exercised.
    fn insert_session_on(
        db: &Database,
        name: &str,
        backend_type: &str,
        backend_id: &str,
    ) -> SessionId {
        let id = SessionId::default();
        let shared = SharedSession {
            id,
            name: name.into(),
            agent: "dev".into(),
            backend_id: backend_id.into(),
            backend_type: backend_type.into(),
            agent_session_id: Some(uuid::Uuid::new_v4().to_string()),
            cwd: None,
            additional_dirs: Vec::new(),
            worktrees: Vec::new(),
            shell_backend_id: None,
            parent_session_id: None,
            display_order: None,
            tombstone: false,
            tombstone_at: None,
        };
        db.upsert_session(&shared).unwrap();
        id
    }

    fn send_automation(db: &Database, session_id: SessionId, name: &str) -> i64 {
        use crate::session::{AutomationAction, AutomationSchedule};
        use crate::storage::automations::NewAutomation;
        db.create_automation(&NewAutomation {
            name: name.into(),
            enabled: true,
            schedule: AutomationSchedule::Once { at: u64::MAX },
            timezone: None,
            action: AutomationAction::Send { session_id },
            prompt: "noop".into(),
            next_run_at: Some(u64::MAX),
        })
        .unwrap()
    }

    #[test]
    fn soft_delete_without_force_leaves_no_side_effects() {
        let db = Database::open_in_memory().unwrap();
        let id = insert_session(&db, "demo");

        // Send automation targeting the session — should survive a soft delete.
        let auto = send_automation(&db, id, "noop");

        let report = delete_session_headless(&db, id, false).unwrap();
        assert!(!report.killed_window);
        assert!(report.removed_worktrees.is_empty());
        assert_eq!(report.disabled_automations, 0);

        assert!(db.get_session_by_id(id).unwrap().is_none());
        assert!(db.get_automation(auto).unwrap().unwrap().enabled);
    }

    #[test]
    fn force_delete_disables_send_automations() {
        let db = Database::open_in_memory().unwrap();
        let id = insert_session(&db, "demo");

        let a = send_automation(&db, id, "a");
        let b = send_automation(&db, id, "b");

        let report = delete_session_headless(&db, id, true).unwrap();
        assert_eq!(report.disabled_automations, 2);
        assert!(!db.get_automation(a).unwrap().unwrap().enabled);
        assert!(!db.get_automation(b).unwrap().unwrap().enabled);
    }

    #[test]
    fn force_delete_clears_session_meta_but_soft_delete_does_not() {
        // Regression: `clear_session_meta` is documented as being called on
        // force-delete (the row is unrestorable, so its metadata is dead
        // weight) but nothing wired the call in — a force-deleted session's
        // meta rows outlived it forever.
        let db = Database::open_in_memory().unwrap();

        let soft = insert_session(&db, "soft-meta");
        db.set_session_meta(soft, "fm.lease", "abc").unwrap();
        delete_session_headless(&db, soft, false).unwrap();
        assert_eq!(
            db.get_session_meta(soft, "fm.lease").unwrap(),
            Some("abc".to_string()),
            "a soft delete is restorable, so its metadata must survive it"
        );

        let hard = insert_session(&db, "hard-meta");
        db.set_session_meta(hard, "fm.lease", "xyz").unwrap();
        delete_session_headless(&db, hard, true).unwrap();
        assert!(
            db.get_session_meta(hard, "fm.lease").unwrap().is_none(),
            "a force delete is unrestorable, so its metadata must not outlive it"
        );
    }

    #[test]
    fn force_delete_marks_force_deleted_but_soft_does_not() {
        let db = Database::open_in_memory().unwrap();

        let soft = insert_session(&db, "soft");
        delete_session_headless(&db, soft, false).unwrap();
        assert!(
            !db.get_deleted_session_by_id(soft)
                .unwrap()
                .unwrap()
                .force_deleted,
            "a soft delete stays restorable"
        );

        let hard = insert_session(&db, "hard");
        delete_session_headless(&db, hard, true).unwrap();
        assert!(
            db.get_deleted_session_by_id(hard)
                .unwrap()
                .unwrap()
                .force_deleted,
            "a force delete is flagged as not restorable"
        );
    }

    // The resolved-remote-host kill/worktree path (a configured, reachable
    // host) is not unit-tested here: it needs a live SSH/WSL host and
    // `kill_remote_windows` would issue a real connection. The routing is thin —
    // `remove_worktree_on` / `kill_remote_windows` are exercised where they live —
    // so these tests cover the two host-resolution failure modes instead.
    // (cfg(test) sandboxes the config dir, so `load_all` sees an empty
    // `hosts.toml` and never touches the real network.)

    #[test]
    fn force_delete_remote_session_with_no_configured_host_records_error() {
        let db = Database::open_in_memory().unwrap();
        let id = insert_session_on(&db, "remote", "ssh:devbox", "%3");

        let report = delete_session_headless(&db, id, true).unwrap();

        // No matching host in (the empty test) hosts.toml → recorded, not killed.
        assert!(!report.killed_window);
        let err = report
            .remote_teardown_error
            .expect("remote teardown error recorded");
        assert!(err.contains("ssh:devbox"), "got {err}");

        // The row is still soft- + force-deleted (best-effort teardown).
        assert!(db.get_session_by_id(id).unwrap().is_none());
        assert!(
            db.get_deleted_session_by_id(id)
                .unwrap()
                .unwrap()
                .force_deleted
        );
    }

    #[test]
    fn force_delete_on_a_host_whose_socket_is_unknown_records_the_refusal() {
        // The teardown used to build a backend on this build's own socket name
        // and `ensure_ready` it, which *creates* the server and the thurbox
        // session on the host — a teardown leaving an empty server behind on
        // somebody else's machine, while the host's own thurbox ran on another
        // socket entirely. It refuses instead, and says so.
        let temp = tempfile::TempDir::new().unwrap();
        let _guard = crate::paths::TestPathGuard::new(temp.path());
        let path = crate::agent::host_config::hosts_config_path().unwrap();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            "[[hosts]]\nname = \"devbox\"\ndestination = \"me@devbox\"\n",
        )
        .unwrap();

        // Not delegated — no reachable CLI there — so the teardown is the
        // local one, aimed at the host's tmux server. Forced rather than
        // probed so the test never dials.
        crate::session_ops::host_cli::fake::force_usable(crate::session_ops::host_cli::Usable::No(
            "no cli".into(),
        ));
        let db = Database::open_in_memory().unwrap();
        let id = insert_session_on(&db, "remote", "ssh:devbox", "%3");

        let report = delete_session_headless(&db, id, true).unwrap();
        crate::session_ops::host_cli::fake::clear();

        assert!(!report.killed_window);
        let err = report
            .remote_teardown_error
            .expect("remote teardown error recorded");
        assert!(
            err.contains("socket unknown for host 'devbox'"),
            "got {err}"
        );
        assert!(db.get_session_by_id(id).unwrap().is_none());
    }

    #[test]
    fn force_delete_local_session_records_no_remote_error() {
        let db = Database::open_in_memory().unwrap();
        let id = insert_session(&db, "local");

        let report = delete_session_headless(&db, id, true).unwrap();
        assert!(report.remote_teardown_error.is_none());
    }

    #[test]
    fn teardown_leaves_a_worktree_thurbox_did_not_create_on_disk() {
        // The whole point of opening an existing worktree is that the user (or
        // their agent) made it outside thurbox. Force-deleting the session must
        // not run `git worktree remove --force` on it: that deletes the
        // directory and any uncommitted work in it. The path below does not
        // exist, so a removal *attempt* would surface as a worktree_error —
        // its absence is the proof that no attempt was made.
        let session = SharedSession {
            id: SessionId::default(),
            name: "opened".into(),
            agent: "dev".into(),
            backend_id: "%4".into(),
            backend_type: "tmux".into(),
            agent_session_id: None,
            cwd: None,
            additional_dirs: Vec::new(),
            worktrees: vec![crate::sync::SharedWorktree {
                repo_path: "/nonexistent/repo".into(),
                worktree_path: "/nonexistent/repo/.worktrees/mine".into(),
                branch: "feat/x".into(),
                created_by_thurbox: false,
            }],
            shell_backend_id: None,
            parent_session_id: None,
            display_order: None,
            tombstone: false,
            tombstone_at: None,
        };

        let mut report = ForceDeleteReport::default();
        teardown_runtime_resources(&session, &mut report);

        assert!(
            report.removed_worktrees.is_empty() && report.worktree_errors.is_empty(),
            "no removal attempted for a worktree thurbox did not create"
        );
        assert_eq!(
            report.kept_worktrees,
            vec!["/nonexistent/repo/.worktrees/mine".to_string()],
            "the skipped worktree is reported, not silently dropped"
        );
    }

    #[test]
    fn teardown_still_removes_a_worktree_thurbox_created() {
        // The counterpart to the test above: provenance must gate the removal,
        // not disable it. A thurbox-created worktree at a path that is gone
        // still reaches `git worktree remove` and reports the failure.
        let session = SharedSession {
            id: SessionId::default(),
            name: "created".into(),
            agent: "dev".into(),
            backend_id: "%5".into(),
            backend_type: "tmux".into(),
            agent_session_id: None,
            cwd: None,
            additional_dirs: Vec::new(),
            worktrees: vec![crate::sync::SharedWorktree {
                repo_path: "/nonexistent/repo".into(),
                worktree_path: "/nonexistent/repo/wt".into(),
                branch: "feat/x".into(),
                created_by_thurbox: true,
            }],
            shell_backend_id: None,
            parent_session_id: None,
            display_order: None,
            tombstone: false,
            tombstone_at: None,
        };

        let mut report = ForceDeleteReport::default();
        teardown_runtime_resources(&session, &mut report);

        assert!(report.kept_worktrees.is_empty());
        assert_eq!(report.worktree_errors.len(), 1, "removal was attempted");
    }

    #[test]
    fn remote_teardown_with_unresolved_host_leaves_worktrees_untouched() {
        // A remote session whose host isn't configured: its worktree dirs live
        // on that (now unreachable) host, so they must be left alone — NOT
        // attempted against the local `git`, which would either error out or,
        // worse, act on a same-path local directory. Exercises the routing
        // directly (no DB round-trip needed for the worktree list).
        let session = SharedSession {
            id: SessionId::default(),
            name: "remote".into(),
            agent: "dev".into(),
            backend_id: "%3".into(),
            backend_type: "wsl:Ubuntu".into(),
            // `None` so no local symlink-workspace cleanup is attempted either.
            agent_session_id: None,
            cwd: None,
            additional_dirs: Vec::new(),
            worktrees: vec![crate::sync::SharedWorktree {
                repo_path: "/nonexistent/repo".into(),
                worktree_path: "/nonexistent/repo/wt".into(),
                branch: "feat/x".into(),
                created_by_thurbox: true,
            }],
            shell_backend_id: None,
            parent_session_id: None,
            display_order: None,
            tombstone: false,
            tombstone_at: None,
        };

        let mut report = ForceDeleteReport::default();
        teardown_runtime_resources(&session, &mut report);

        assert!(
            report.remote_teardown_error.is_some(),
            "unreachable host recorded"
        );
        assert!(
            report.removed_worktrees.is_empty() && report.worktree_errors.is_empty(),
            "no local git worktree removal attempted for a remote session"
        );
        assert!(!report.killed_window);
    }

    #[test]
    fn the_overdue_reap_skips_fresh_forced_and_remote_rows() {
        let temp = tempfile::TempDir::new().unwrap();
        let _guard = crate::paths::TestPathGuard::new(temp.path());
        let db = Database::open_in_memory().unwrap();
        let fresh = crate::sync::SharedSession {
            id: SessionId::default(),
            name: "fresh".into(),
            agent: "claude".into(),
            backend_id: "%1".into(),
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
        db.upsert_session(&fresh).unwrap();
        db.soft_delete_session(fresh.id).unwrap();
        let mut remote = fresh.clone();
        remote.id = SessionId::default();
        remote.name = "remote".into();
        remote.backend_type = "ssh:devbox".into();
        db.upsert_session(&remote).unwrap();
        db.soft_delete_session(remote.id).unwrap();
        // Inside the undo window, and on another machine: neither is touched
        // (and no tmux is consulted for either).
        assert!(reap_overdue_soft_deletes(&db).is_empty());
    }

    /// The sweep's ownership gate. Its old test was `agent_window_alive`, so an
    /// overdue row whose pane had long gone was re-reported as reaped on every
    /// tick on the strength of a live namesake's window. Ownership is now the
    /// window's own stamp (ADR-25): with no window carrying this row's id — here
    /// there is no tmux server at all — the row owns nothing and the sweep
    /// leaves it alone.
    #[test]
    fn the_overdue_reap_skips_a_row_a_live_namesake_answers_for() {
        let temp = tempfile::TempDir::new().unwrap();
        let _guard = crate::paths::TestPathGuard::new(temp.path());
        let db = Database::open_in_memory().unwrap();
        let stale = crate::sync::SharedSession {
            id: SessionId::default(),
            name: "fleet".into(),
            agent: "claude".into(),
            backend_id: "%1".into(),
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
        db.upsert_session(&stale).unwrap();
        db.soft_delete_session(stale.id).unwrap();
        // Past the undo window, so only the gate can hold the sweep back.
        db.conn_ref()
            .execute(
                "UPDATE sessions SET deleted_at = 0 WHERE id = ?1",
                [stale.id.to_string()],
            )
            .unwrap();
        let mut live = stale.clone();
        live.id = SessionId::default();
        db.upsert_session(&live).unwrap();

        assert!(
            reap_overdue_soft_deletes(&db).is_empty(),
            "an overdue row whose pane and name a live namesake claims owns \
             nothing to release"
        );
        assert!(
            db.get_deleted_session_by_id(stale.id).unwrap().is_some(),
            "and is left soft-deleted rather than reported collected"
        );
    }

    /// A force delete's teardown destroys worktrees. If the row were marked
    /// afterwards, a crash between the two would leave an active,
    /// restore-listed session whose work is already gone — so the mark comes
    /// first, and the teardown is the place to prove it.
    #[test]
    fn a_force_delete_marks_the_row_before_it_tears_anything_down() {
        let db = Database::open_in_memory().unwrap();
        let id = insert_session(&db, "doomed");
        let session = db.get_session_by_id(id).unwrap().unwrap();
        let ctx = super::super::lifecycle_hooks::context_for(&session);

        let mut seen_force_deleted = false;
        finish_locally_with(
            &db,
            &session,
            true,
            ForceDeleteReport::default(),
            &ctx,
            |_, _| {
                seen_force_deleted = db
                    .get_deleted_session_by_id(id)
                    .unwrap()
                    .is_some_and(|row| row.force_deleted);
            },
        )
        .unwrap();

        assert!(
            seen_force_deleted,
            "the row must already read force-deleted while its windows and \
             worktrees are coming down"
        );
        assert!(db.get_session_by_id(id).unwrap().is_none());
    }

    #[test]
    fn missing_session_errors() {
        let db = Database::open_in_memory().unwrap();
        let err = delete_session_headless(&db, SessionId::default(), false).unwrap_err();
        assert!(err.contains("Session not found"), "got {err}");
    }
}
