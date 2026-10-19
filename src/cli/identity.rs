//! Resolve the thurbox session a CLI invocation is running *inside*, from the
//! env vars thurbox injects at spawn. Shared by `message` (provenance) and
//! `session signal` (the hook callback's identity).

use crate::session::SessionId;
use crate::storage::Database;
use crate::sync::SharedSession;

/// Who is calling, as far as this database can tell. "Set but unknown here" is
/// its own case because it is a real one — a remote peer spawned the session,
/// so the host's database has no row — and reporting it as unset sends the
/// reader looking for a variable that is there.
pub(crate) enum Caller {
    Unset,
    /// The raw `THURBOX_SESSION` value, which names no row here.
    Unknown(String),
    Known(Box<SharedSession>),
}

impl Caller {
    pub(crate) fn known(self) -> Option<SharedSession> {
        match self {
            Caller::Known(s) => Some(*s),
            Caller::Unset | Caller::Unknown(_) => None,
        }
    }

    /// The calling session, or an error saying which of the other two cases
    /// this is. `unset` is the caller's own message for no session at all;
    /// `flag` is the argument that names a session explicitly.
    pub(crate) fn require(self, unset: &str, flag: &str) -> Result<SharedSession, String> {
        match self {
            Caller::Known(s) => Ok(*s),
            Caller::Unset => Err(unset.to_string()),
            Caller::Unknown(raw) if raw.parse::<SessionId>().is_err() => Err(format!(
                "THURBOX_SESSION={raw} is not a session id — pass {flag} explicitly"
            )),
            Caller::Unknown(raw) => {
                let database = crate::paths::database_file()
                    .map_or_else(|| "unresolved path".into(), |p| p.display().to_string());
                Err(format!(
                    "THURBOX_SESSION={raw} is not a session in this database ({database}); \
                     it may belong to another thurbox (a remote peer) — pass {flag} explicitly"
                ))
            }
        }
    }
}

/// The calling session from the `THURBOX_SESSION` env var thurbox injects at
/// spawn (the registry key).
pub(crate) fn caller(db: &Database) -> Result<Caller, String> {
    let raw = match std::env::var("THURBOX_SESSION") {
        Ok(raw) if !raw.is_empty() => raw,
        _ => return Ok(Caller::Unset),
    };
    let Ok(id) = raw.parse::<SessionId>() else {
        return Ok(Caller::Unknown(raw));
    };
    Ok(db
        .get_session_by_id(id)
        .map_err(|e| format!("get_session_by_id: {e}"))?
        .map_or(Caller::Unknown(raw), |s| Caller::Known(Box::new(s))))
}

/// [`caller`] for provenance, which is best-effort: `None` unless the id maps to
/// a live row, DB errors included.
pub(crate) fn calling_session(db: &Database) -> Option<SharedSession> {
    caller(db).ok().and_then(Caller::known)
}

/// [`caller`], falling back to a lookup by the agent conversation id in
/// `$THURBOX_SESSION_ID` (the env fallback for agents whose hooks don't inherit
/// `$THURBOX_SESSION`). The fallback rescues an unknown `THURBOX_SESSION` too;
/// only when it also fails is the caller reported as unknown.
pub(crate) fn calling_session_or_by_agent_id(db: &Database) -> Result<Caller, String> {
    let caller = caller(db)?;
    if let Caller::Known(_) = caller {
        return Ok(caller);
    }
    if let Ok(agent_sid) = std::env::var("THURBOX_SESSION_ID") {
        if let Some(s) = db
            .get_session_by_agent_session_id(&agent_sid)
            .map_err(|e| format!("get_session_by_agent_session_id: {e}"))?
        {
            return Ok(Caller::Known(Box::new(s)));
        }
    }
    Ok(caller)
}
