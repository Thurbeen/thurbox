//! The heartbeat's at-least-once consumer of the state-edge log.
use crate::{
    session::{HookContext, HookEvent},
    storage::{Database, HookRow},
    sync::current_time_millis,
};
use serde_json::{json, Value};
use std::time::{Duration, Instant};

const CONSUMER: &str = "hooks";
const LEASE_MS: i64 = 90_000;
const BUDGET: Duration = Duration::from_secs(30);

pub fn configured() -> bool {
    !crate::agent::hooks_config::hooks_for(HookEvent::StateChanged).is_empty()
}

/// One bounded batch. The cursor advances only after all hooks for a row were
/// attempted. A failure is informational; a crash before the advance replays
/// the batch, so scripts must deduplicate external side effects by `seq`.
pub fn tick(db: &Database) -> Result<Value, String> {
    let hooks = crate::agent::hooks_config::hooks_for(HookEvent::StateChanged);
    if hooks.is_empty() {
        return Ok(Value::Null);
    }
    let Some(from) = db
        .claim_cursor(CONSUMER, current_time_millis() as i64, LEASE_MS)
        .map_err(|e| e.to_string())?
    else {
        return Ok(json!({"leased": true}));
    };
    let result = drain(db, &hooks, from);
    if result.is_err() {
        let _ = db.release_cursor(CONSUMER);
    }
    result
}

fn drain(
    db: &Database,
    hooks: &[crate::session::LifecycleHook],
    from: i64,
) -> Result<Value, String> {
    let events = super::events::drain(db, from, None).map_err(|e| e.to_string())?;
    let gap = db.session_event_gap(from).map_err(|e| e.to_string())?;
    let ids: Vec<_> = events.iter().map(|e| e.session_id).collect();
    let facts = db.load_session_facts_for(&ids).map_err(|e| e.to_string())?;
    let registry = crate::agent::agent_config::load_or_seed();
    let started = Instant::now();
    let mut cursor = from;
    let mut failures = Vec::new();
    let mut attempted = 0;
    let mut gap_pending = gap;
    for event in events {
        if started.elapsed() >= BUDGET {
            break;
        }
        // State edges, terminal lifecycle edges included. A rename or pane
        // update is not a state notification.
        if !["state", "stopped", "lost", "exited"].contains(&event.reason.as_str()) {
            cursor = event.seq;
            continue;
        }
        let Some(facts) = facts.get(&event.session_id) else {
            cursor = event.seq;
            continue;
        };
        let destination = match event.reason.as_str() {
            "state" => event.to_state.as_deref().unwrap_or_default(),
            other => other,
        };
        let matching: Vec<_> = hooks
            .iter()
            .filter(|h| {
                h.matches_agent(&facts.agent)
                    && (h.to.is_empty() || h.to.iter().any(|s| s == destination))
            })
            .collect();
        let ctx = db
            .get_session_by_id(event.session_id)
            .map_err(|e| e.to_string())?
            .map(|s| super::lifecycle_hooks::context_for(&s))
            .unwrap_or_else(|| HookContext {
                session_id: Some(event.session_id),
                name: facts.name.clone(),
                agent: facts.agent.clone(),
                ..HookContext::default()
            });
        let hook = super::events::assess(
            &registry,
            facts,
            &HookRow {
                state: event.to_state.clone(),
                state_at: Some(event.at_ms),
                seen_at: facts.seen_at,
            },
            event.reason == "stopped",
        );
        let mut payload = super::events::render(&event, facts, &hook, event.reason == "stopped");
        payload["gap"] = json!(gap_pending);
        let mut complete = true;
        for (index, hook) in matching.iter().enumerate() {
            let left = BUDGET.saturating_sub(started.elapsed());
            if left.is_zero() {
                complete = false;
                break;
            }
            attempted += 1;
            if let Err(e) = super::lifecycle_hooks::run_payload(
                hook,
                HookEvent::StateChanged,
                &ctx,
                format!("{payload}\n"),
                // Reserving a share for every remaining hook prevents a
                // single default-timeout hook consuming the entire phase on
                // every replay, forever starving the rest of this event.
                hook.timeout()
                    .min(left / (matching.len() - index).min(u32::MAX as usize) as u32),
            ) {
                tracing::warn!("{e}");
                failures.push(e);
            }
        }
        if !complete {
            break;
        }
        if !matching.is_empty() {
            gap_pending = false;
        }
        cursor = event.seq;
    }
    if !db
        .advance_cursor(CONSUMER, from, cursor)
        .map_err(|e| e.to_string())?
    {
        return Err("state-hook cursor changed during the batch".into());
    }
    Ok(json!({"cursor": cursor, "attempted": attempted, "gap": gap, "failures": failures}))
}
