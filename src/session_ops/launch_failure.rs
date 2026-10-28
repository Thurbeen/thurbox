//! Noticing a launch that died before its agent said anything.
//!
//! The pane is kept (`remain-on-exit`), so the death is an observation away:
//! a session no hook has reported for whose pane is dead. What it finds is
//! recorded once ([`Database::record_launch_failure`]) and every surface then
//! reads the session as `failed` — see [`crate::session::launch_failure`].
//!
//! Looked for wherever a loss is (`reconcile::tick` — the interface worker and
//! the headless heartbeat — and the `wait`/`watch` loops), and by
//! `session get` for the one session it reads, so a driver polling it learns
//! of the failure on its next poll rather than the next heartbeat.
//!
//! Local sessions only, like [`super::lost`]: a session on a `--host` lives on
//! that host's server, and a host that shares its sessions records its own.

use crate::session::{LaunchFailure, Route, SessionId};
use crate::storage::Database;

/// Look at every session that could have failed to launch and record each one
/// that did. Returns how many it recorded.
pub fn sweep(db: &Database, backends: &crate::backend::BackendRegistry) -> usize {
    sweep_where(db, backends, None)
}

/// [`sweep`] for one session.
pub fn check(db: &Database, backends: &crate::backend::BackendRegistry, id: SessionId) -> bool {
    sweep_where(db, backends, Some(id)) > 0
}

fn sweep_where(
    db: &Database,
    backends: &crate::backend::BackendRegistry,
    only: Option<SessionId>,
) -> usize {
    let rows = match db.list_active_sessions() {
        Ok(rows) => rows,
        Err(e) => {
            tracing::warn!("could not list sessions to check their launches: {e}");
            return 0;
        }
    };
    let hooks = db.load_hook_states().unwrap_or_default();
    let stopped = db.load_stopped_sessions().unwrap_or_default();
    let lost = db.load_lost_sessions().unwrap_or_default();
    let failed = db.load_launch_failures().unwrap_or_default();
    // Never reported, and so still owed a first word — the only rows a dead
    // pane can make `failed`. Cheap columns first: on a quiet machine every
    // row has reported, and nothing below runs at all.
    let candidates: Vec<_> = rows
        .iter()
        .filter(|row| only.map_or(true, |id| row.id == id))
        .filter(|row| {
            !Route::is_remote_key(&row.backend_type)
                && !row.backend_id.is_empty()
                && !stopped.contains(&row.id)
                && !lost.contains_key(&row.id)
                && !failed.contains_key(&row.id)
                && hooks.get(&row.id).map_or(true, |h| h.state.is_none())
        })
        .collect();
    // A session that never reports (an uncovered agent, a `--command`) stays
    // a candidate for as long as it lives, so the panes are looked at first
    // and agents.toml is read only once one of them is found dead.
    let dead: Vec<_> = candidates
        .into_iter()
        .filter_map(|row| observe(backends, row).map(|failure| (row, failure)))
        .collect();
    if dead.is_empty() {
        return 0;
    }
    let expects = Expectations::load(db);
    dead.into_iter()
        .filter(|(row, failure)| expects.judges(row, failure))
        .filter(|(row, failure)| {
            let recorded = db
                .record_launch_failure(row.id, failure)
                .map_err(|e| {
                    tracing::warn!("could not record the launch failure of {}: {e}", row.id)
                })
                .unwrap_or(false);
            if recorded {
                tracing::warn!(
                    "the launch of '{}' failed ({}, exit {})",
                    row.name,
                    failure.reason.as_str(),
                    failure
                        .exit_code
                        .map_or_else(|| "unknown".to_string(), |c| c.to_string())
                );
            }
            recorded
        })
        .count()
}

/// Record a launch failure for an agent that reported its own exit from
/// inside its pane (`session exited`, on a multiplexer that closes the window
/// with its program, where no sweep would find a dead pane to read). Judged
/// like a swept one: only a session no hook ever reported for.
pub fn record_exited(
    db: &Database,
    session: &crate::sync::SharedSession,
    exit_code: Option<i64>,
    output: Option<String>,
) -> bool {
    let reported = db
        .load_hook_state(session.id)
        .ok()
        .flatten()
        .is_some_and(|h| h.state.is_some());
    if reported {
        return false;
    }
    let failure = LaunchFailure::observed(
        &session.backend_id,
        exit_code,
        output,
        crate::sync::current_time_millis() as i64,
    );
    Expectations::load(db).judges(session, &failure)
        && db
            .record_launch_failure(session.id, &failure)
            .map_err(|e| {
                tracing::warn!("could not record the launch failure of {}: {e}", session.id)
            })
            .unwrap_or(false)
}

/// The pane's death, if it is dead: its exit status and last lines.
fn observe(
    backends: &crate::backend::BackendRegistry,
    row: &crate::sync::SharedSession,
) -> Option<LaunchFailure> {
    let (backend, pane) = super::windows::agent_pane(backends, row).ok()??;
    let state = backend.pane_state(&pane).ok()?;
    if state.dead != Some(true) {
        return None;
    }
    let output = backend
        .capture(
            &pane,
            crate::session::launch_failure::LAUNCH_FAILURE_TAIL_LINES,
            false,
        )
        .map_err(|e| tracing::debug!("could not capture the dead pane {pane}: {e:#}"))
        .ok();
    Some(LaunchFailure::observed(
        &pane,
        state.exit_status,
        output,
        crate::sync::current_time_millis() as i64,
    ))
}

/// What decides whether a dead, unreported pane is a *failed* launch.
///
/// A failed `setup` always is: the agent never ran. An agent that exited is
/// one only where its silence meant something — it is wired to report
/// (thurbox's hooks are on and its agent has hook coverage), so it would have
/// said something on boot. An uncovered agent, a plain `--command` session or
/// a session with hooks off never reports at all, and a shell the user
/// `exit`ed after an hour is not a launch that failed.
struct Expectations {
    registry: crate::session::AgentRegistry,
    hooks_enabled: bool,
    command_sessions: std::collections::HashSet<SessionId>,
    reports_as: std::collections::HashMap<SessionId, String>,
}

impl Expectations {
    fn load(db: &Database) -> Self {
        Self {
            registry: crate::agent::agent_config::load_for_launch(),
            hooks_enabled: super::hooks_enabled(db),
            command_sessions: db.load_command_sessions().unwrap_or_default(),
            reports_as: db.load_reports_as().unwrap_or_default(),
        }
    }

    fn judges(&self, row: &crate::sync::SharedSession, failure: &LaunchFailure) -> bool {
        if failure.reason == crate::session::LaunchFailureReason::Setup {
            return true;
        }
        let agent = self.reports_as.get(&row.id).unwrap_or(&row.agent);
        let declared =
            !self.command_sessions.contains(&row.id) || self.reports_as.contains_key(&row.id);
        self.hooks_enabled
            && declared
            && crate::session::coverage_for(&self.registry, agent).is_some()
    }
}
