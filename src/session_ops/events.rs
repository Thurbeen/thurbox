//! The bounded event-log read and JSON shape shared by watch, waits and hooks.
use crate::session::{Assessment, SessionId};
use crate::storage::{Database, SessionEventRow, SessionFacts};
use serde_json::{json, Value};

pub const BATCH: usize = 512;

pub fn drain(
    db: &Database,
    seq: i64,
    only: Option<SessionId>,
) -> rusqlite::Result<Vec<SessionEventRow>> {
    db.session_events_since(seq, only, BATCH)
}

pub fn render(
    event: &SessionEventRow,
    facts: &SessionFacts,
    hook: &Assessment,
    stopped: bool,
) -> Value {
    let mut line = base(event.session_id, facts, hook);
    line["seq"] = json!(event.seq);
    line["event"] = json!(event.event);
    line["reason"] = json!(event.reason);
    line["from_state"] = json!(event.from_state);
    line["to_state"] = json!(event.to_state);
    line["exit_code"] = json!(event.exit_code);
    line["detail"] = json!(event.detail);
    line["stopped"] = json!(stopped);
    line["at"] = json!(event.at_ms);
    line
}

pub fn assess(
    registry: &crate::session::AgentRegistry,
    facts: &SessionFacts,
    row: &crate::storage::HookRow,
    stopped: bool,
) -> Assessment {
    let hook = Assessment::from_hooks(
        registry,
        &facts.agent,
        row.state.as_deref(),
        row.state_at,
        row.seen_at,
        crate::sync::current_time_millis() as i64,
    );
    if stopped {
        hook.parked()
    } else {
        hook
    }
}

pub fn present(seq: i64, id: SessionId, facts: &SessionFacts, hook: &Assessment) -> Value {
    let mut line = base(id, facts, hook);
    line["seq"] = json!(seq);
    line["event"] = json!("present");
    line["reason"] = Value::Null;
    line["from_state"] = Value::Null;
    line["to_state"] = json!(hook.hook_state);
    line["exit_code"] = Value::Null;
    line["detail"] = Value::Null;
    line["stopped"] = json!(facts.stopped);
    line["at"] = json!(crate::sync::current_time_millis());
    line
}

/// The fields every line carries whatever produced it.
///
/// `state` is [`Assessment::state_word`] — the same one word `session get` and
/// `session list` answer with, so a driver never has to reconcile two
/// vocabularies — and the gating fields beside it are what say how much that
/// word is worth. They are on the line rather than a `session get` away
/// precisely because a driver reacting to a `blocked` needs to know whether
/// this agent's `blocked` is a text match on a notification body before it
/// acts on one.
pub fn base(id: SessionId, facts: &SessionFacts, hook: &Assessment) -> Value {
    json!({
        "gap": false,
        "last_output_at": Value::Null,
        "transcript_path": Value::Null,
        "session": id.to_string(),
        "name": facts.name,
        "agent": facts.agent,
        "backend_id": facts.backend_id,
        "state": hook.state_word(),
        "hook_state": hook.hook_state,
        "state_source": hook.state_source.map(|source| source.as_str()),
        "hook_coverage": hook.coverage.as_str(),
        "hook_blocked_is_heuristic": hook.blocked_is_heuristic(),
        "hook_state_contradicted": hook.contradicted,
        // Which agent is in the pane, when it is not the one the row was
        // created with — `--verify` only, like every other pane field.
        "detected_agent": hook.detected_agent(),
    })
}
