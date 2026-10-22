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
    // a latched state could answer for a process that is gone.
    if db.session_lost_at(session.id).map_err(failed)?.is_some() {
        return Err(Error::Gone);
    }
    let mut stopped = db.session_stopped_at(session.id).map_err(failed)?.is_some();
    // An explicit cursor is the next edge, never the current latched state.
    if since.is_none() {
        if let Some(value) = baseline(db, &registry, session.id, seq, &conditions.states)? {
            return Ok(value);
        }
    }
    if stopped {
        return Err(Error::Gone);
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
                let hook = super::events::assess(
                    &registry,
                    facts,
                    &HookRow {
                        state: event.to_state.clone(),
                        state_at: Some(event.at_ms),
                        seen_at: facts.seen_at,
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
                // the stop it was waiting for rather than ended by it.
                if stopped {
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
