//! A process-local event cursor; no keeper, UI, or subscription to supervise.
use crate::{
    session::{AgentRegistry, SessionId},
    storage::{Database, HookRow},
    sync::SharedSession,
};
use serde_json::{json, Value};
use std::time::{Duration, Instant};

pub const POLL: Duration = Duration::from_millis(250);
const CAPTURE_INTERVAL: Duration = Duration::from_secs(1);

pub struct Conditions {
    pub states: Vec<String>,
    pub pane: Option<regex::Regex>,
    pub timeout: Option<Duration>,
}

pub enum Error {
    Timeout,
    Gone,
    Failed(String),
}

/// Take the head before delivery for a send, or before the baseline for a wait.
/// Every edge after that head remains observable even if two share one poll.
pub fn run(
    db: &Database,
    backends: &crate::backend::BackendRegistry,
    session: &SharedSession,
    conditions: Conditions,
    since: Option<i64>,
) -> Result<Value, Error> {
    let started = Instant::now();
    let mut seq = since
        .map(Ok)
        .unwrap_or_else(|| db.latest_session_event_seq())
        .map_err(failed)?;
    let registry = crate::agent::agent_config::load_or_seed();
    // A session already marked lost has no pane to reach any state, and its
    // `lost` edge predates the cursor, so the mark itself ends the wait — before
    // a latched state could answer for a process that is gone. An explicit
    // cursor (`send --wait-turn`'s, taken before delivery) may have edges
    // queued after it, a turn ahead of the loss among them: those are read in
    // order first, and the loop's idle check ends the wait on the mark after.
    if since.is_none() && db.session_lost_at(session.id).map_err(failed)?.is_some() {
        // Except a launch failure, which outlives the loss of its pane (psmux
        // closes the window once its program exits): it still answers a wait
        // for `failed`, which nothing else can match on a lost row.
        let failure = crate::session::SessionState::Failed.as_str();
        if conditions.states.iter().any(|state| state == failure) {
            if let Some(value) = baseline(db, &registry, session.id, seq, &[failure.into()])? {
                return Ok(value);
            }
        }
        return Err(Error::Gone);
    }
    // With an explicit cursor the park is replayed from the edges like
    // everything else: a turn recorded before it still answers, and a park
    // that predates the cursor ends the wait at the loop's idle check.
    let mut stopped =
        since.is_none() && db.session_stopped_at(session.id).map_err(failed)?.is_some();
    // An explicit cursor is the next edge, never the current latched state.
    if since.is_none() {
        if let Some(value) = baseline(db, &registry, session.id, seq, &conditions.states)? {
            return Ok(value);
        }
    }
    if stopped {
        return Err(Error::Gone);
    }
    // A launch that already failed, and was not what the caller waits for
    // (the baseline above answers `--state failed`), will reach no state.
    if since.is_none() {
        super::launch_failure::check(db, backends, session.id);
        let facts = db.load_session_facts_for(&[session.id]).map_err(failed)?;
        if facts
            .get(&session.id)
            .is_some_and(|f| f.launch_failure.is_some())
        {
            if let Some(value) = baseline(db, &registry, session.id, seq, &conditions.states)? {
                return Ok(value);
            }
            return Err(Error::Gone);
        }
    }
    let mut version = db.data_version().map_err(failed)?;
    let mut dirty = true;
    let mut lost = super::lost::LostSweep::default();
    let mut swept = Instant::now();
    let mut captured: Option<Instant> = None;
    let mut gap = false;
    loop {
        if dirty {
            let batch = super::events::drain(db, seq, Some(session.id)).map_err(failed)?;
            let full = batch.len() == super::events::BATCH;
            gap |= db.session_event_gap(seq).map_err(failed)?;
            let facts = db.load_session_facts_for(&[session.id]).map_err(failed)?;
            for event in batch {
                seq = event.seq;
                match event.reason.as_str() {
                    "stopped" => stopped = true,
                    "started" => stopped = false,
                    _ => {}
                }
                let ended =
                    event.event == "gone" || ["lost", "exited"].contains(&event.reason.as_str());
                if ended {
                    return Err(Error::Gone);
                }
                let Some(facts) = facts.get(&session.id) else {
                    return Err(Error::Gone);
                };
                let facts = &*super::events::as_of(&event, facts);
                let hook = super::events::assess(
                    &registry,
                    facts,
                    // An edge is judged as of the moment it happened, when
                    // nobody had seen it yet: the row's later acknowledgement
                    // would turn a `done` edge into `idle` and hang a wait
                    // for `done`.
                    &HookRow {
                        state: event.to_state.clone(),
                        state_at: Some(event.at_ms),
                        seen_at: None,
                    },
                    stopped,
                );
                if conditions
                    .states
                    .iter()
                    .any(|state| state == hook.state_word())
                {
                    let mut value = super::events::render(&event, facts, &hook, stopped);
                    value["gap"] = json!(gap);
                    return Ok(value);
                }
                // Checked after the match, so `--state stopped` is answered by
                // the stop it was waiting for rather than ended by it — and
                // `--state failed` by the failure.
                if stopped || event.reason == "launch_failed" {
                    return Err(Error::Gone);
                }
            }
            if full {
                if conditions
                    .timeout
                    .is_some_and(|timeout| started.elapsed() >= timeout)
                {
                    return Err(Error::Timeout);
                }
                continue;
            }
            dirty = false;
            if db.get_session_by_id(session.id).map_err(failed)?.is_none()
                || db.session_stopped_at(session.id).map_err(failed)?.is_some()
                || db.session_lost_at(session.id).map_err(failed)?.is_some()
            {
                return Err(Error::Gone);
            }
        }
        if let Some(pattern) = &conditions.pane {
            if captured.map_or(true, |at| at.elapsed() >= CAPTURE_INTERVAL) {
                captured = Some(Instant::now());
                let row = db
                    .get_session_by_id(session.id)
                    .map_err(failed)?
                    .ok_or(Error::Gone)?;
                let (backend, pane) =
                    super::windows::require_agent_pane(backends, &row).map_err(failed)?;
                let text = backend.capture(&pane, 1000, false).map_err(failed)?;
                if pattern.is_match(&text) {
                    let facts = db.load_session_facts_for(&[session.id]).map_err(failed)?;
                    let facts = facts.get(&session.id).ok_or(Error::Gone)?;
                    let row_state = db
                        .load_hook_state(session.id)
                        .map_err(failed)?
                        .unwrap_or_default();
                    let hook = super::events::assess(&registry, facts, &row_state, false);
                    let mut value = super::events::present(seq, row.id, facts, &hook);
                    value["gap"] = json!(gap);
                    value["event"] = json!("pane_match");
                    value["detail"] = json!(text);
                    return Ok(value);
                }
            }
        }
        let nap = match conditions.timeout {
            Some(timeout) => timeout
                .checked_sub(started.elapsed())
                .filter(|d| !d.is_zero())
                .ok_or(Error::Timeout)?
                .min(POLL),
            None => POLL,
        };
        std::thread::sleep(nap);
        if swept.elapsed() >= Duration::from_secs(3) {
            dirty |= lost.sweep(db, backends) > 0;
            dirty |= super::launch_failure::check(db, backends, session.id);
            swept = Instant::now();
        }
        let next = db.data_version().map_err(failed)?;
        dirty |= next != version;
        version = next;
    }
}

fn failed(e: impl std::fmt::Display) -> Error {
    Error::Failed(e.to_string())
}
fn baseline(
    db: &Database,
    registry: &AgentRegistry,
    id: SessionId,
    seq: i64,
    states: &[String],
) -> Result<Option<Value>, Error> {
    let facts = db.load_session_facts_for(&[id]).map_err(failed)?;
    let facts = facts.get(&id).ok_or(Error::Gone)?;
    let row = db.load_hook_state(id).map_err(failed)?.unwrap_or_default();
    let hook = super::events::assess(registry, facts, &row, facts.stopped);
    Ok(states
        .iter()
        .any(|s| s == hook.state_word())
        .then(|| super::events::present(seq, id, facts, &hook)))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A local row with a pane to lose.
    fn row() -> SharedSession {
        SharedSession {
            id: SessionId::default(),
            name: "worker".into(),
            agent: "shell".into(),
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
            key: None,
        }
    }

    fn turn(db: &Database, session: &SharedSession, cursor: i64) -> Result<Value, Error> {
        run(
            db,
            &crate::backend::registry::inert(),
            session,
            Conditions {
                states: vec!["working".into()],
                pane: None,
                timeout: Some(Duration::from_secs(2)),
            },
            Some(cursor),
        )
    }

    /// The same ordering as a loss: a turn recorded after the cursor answers
    /// the wait even when the session was parked right after it.
    #[test]
    fn an_explicit_cursor_reads_a_turn_recorded_before_a_stop() {
        let temp = tempfile::TempDir::new().unwrap();
        let _guard = crate::paths::TestPathGuard::new(temp.path());
        let db = Database::open_in_memory().unwrap();
        let session = row();
        db.upsert_session(&session).unwrap();
        let cursor = db.latest_session_event_seq().unwrap();
        db.set_hook_state(session.id, "working").unwrap();
        db.set_session_stopped(session.id, true).unwrap();

        let Ok(event) = turn(&db, &session, cursor) else {
            panic!("the recorded turn went unconfirmed");
        };
        assert_eq!(event["to_state"], "working");
    }

    /// `send --wait-turn` takes its cursor before delivery. A turn recorded
    /// after that cursor answers the wait even when a loss was recorded after
    /// the turn and before the wait started reading: the queued edges are read
    /// in order before the lost mark can end the wait.
    #[test]
    fn an_explicit_cursor_reads_a_turn_recorded_before_a_loss() {
        let temp = tempfile::TempDir::new().unwrap();
        let _guard = crate::paths::TestPathGuard::new(temp.path());
        let db = Database::open_in_memory().unwrap();
        let session = row();
        db.upsert_session(&session).unwrap();
        let cursor = db.latest_session_event_seq().unwrap();
        db.set_hook_state(session.id, "working").unwrap();
        assert!(db.record_session_lost(session.id, "%1").unwrap());

        let Ok(event) = turn(&db, &session, cursor) else {
            panic!("the recorded turn went unconfirmed");
        };
        assert_eq!(event["to_state"], "working");
    }
}
