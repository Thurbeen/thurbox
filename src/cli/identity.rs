//! Resolve the thurbox session a CLI invocation is running *inside*, from the
//! env vars thurbox injects at spawn. Shared by `message` (provenance) and
//! `session signal` (the hook callback's identity).

use crate::session::SessionId;
use crate::storage::Database;
use crate::sync::SharedSession;

/// The calling session, resolved from the `THURBOX_SESSION` env var thurbox
/// injects at spawn (the registry key). `None` when not running inside a thurbox
/// session, or the id no longer maps to a live row. DB errors are swallowed to
/// `None` (provenance is best-effort).
pub(crate) fn calling_session(db: &Database) -> Option<SharedSession> {
    let raw = std::env::var("THURBOX_SESSION").ok()?;
    let id: SessionId = raw.parse().ok()?;
    db.get_session_by_id(id).ok().flatten()
}

/// The calling session resolved from `$THURBOX_SESSION`, falling back to a lookup
/// by the agent conversation id in `$THURBOX_SESSION_ID` (the env fallback for
/// agents whose hooks don't inherit `$THURBOX_SESSION`). `Ok(None)` when neither
/// resolves. Unlike [`calling_session`], DB errors are surfaced.
pub(crate) fn calling_session_or_by_agent_id(
    db: &Database,
) -> Result<Option<SharedSession>, String> {
    if let Ok(raw) = std::env::var("THURBOX_SESSION") {
        if let Ok(id) = raw.parse::<SessionId>() {
            if let Some(s) = db
                .get_session_by_id(id)
                .map_err(|e| format!("get_session_by_id: {e}"))?
            {
                return Ok(Some(s));
            }
        }
    }
    if let Ok(agent_sid) = std::env::var("THURBOX_SESSION_ID") {
        if let Some(s) = db
            .get_session_by_agent_session_id(&agent_sid)
            .map_err(|e| format!("get_session_by_agent_session_id: {e}"))?
        {
            return Ok(Some(s));
        }
    }
    Ok(None)
}
