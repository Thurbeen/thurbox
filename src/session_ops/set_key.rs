//! Giving a live session a key, or taking it away — `session set-key`.
//!
//! The migration path for sessions that predate keys (issue #1408): a driver
//! finds its session once by whatever it used before and stamps it, after which
//! `key:<key>` finds it whatever it is renamed to. One pipeline for every
//! caller, like [`super::rename`], so the uniqueness rule cannot differ.

use crate::session::SessionId;
use crate::storage::Database;

/// What a `set-key` did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetKeyReport {
    /// The key the session had.
    pub previous: Option<String>,
    /// False when the session already had the key asked for.
    pub changed: bool,
}

/// Set (`Some`) or clear (`None`) the key of an active session.
///
/// Refused when another live session on the same host already has the key,
/// under the claim every key writer takes ([`super::names::claim_key`]). A
/// session on a shareable host is re-keyed by the host, whose rows are the
/// record (ADR-24), and this row follows by mirroring — exactly as a rename is.
pub fn set_session_key_headless(
    db: &Database,
    session_id: SessionId,
    key: Option<&str>,
) -> Result<SetKeyReport, String> {
    if let Some(key) = key {
        super::names::validate_key(key)?;
    }
    let session = db
        .get_session_by_id(session_id)
        .map_err(|e| format!("Failed to load session: {e}"))?
        .ok_or_else(|| format!("Session not found: {session_id}"))?;
    let previous = session.key.clone();
    if previous.as_deref() == key {
        return Ok(SetKeyReport {
            previous,
            changed: false,
        });
    }

    let host = super::resolve_host(&session.backend_type).ok_or_else(|| {
        format!(
            "Session '{}' runs on backend '{}', which is not in hosts.toml — \
             cannot reach the machine it lives on",
            session.name, session.backend_type
        )
    })?;
    if let Some((host, cli)) = host
        .as_ref()
        .and_then(|h| super::host_cli::delegated(h).map(|cli| (h, cli)))
    {
        let id = session_id.to_string();
        let mut args = vec!["session", "set-key", id.as_str()];
        match key {
            Some(key) => args.push(key),
            None => args.push("--clear"),
        }
        super::host_cli::run(host, &cli, &args)?;
        if let Err(e) = super::mirror::mirror_host(db, host, &cli) {
            tracing::warn!("mirror of '{}' after set-key failed: {e}", host.name);
        }
        return Ok(SetKeyReport {
            previous,
            changed: true,
        });
    }

    // Held across the write: until it lands, no lookup can see this session as
    // the key's holder, and the claim is what a concurrent creator meets.
    let _claim = match key {
        Some(key) => {
            let claim = super::names::claim_key(db, key, &session.backend_type)?;
            if let Some(refusal) = super::names::key_refusal(key, &session.backend_type, &claim) {
                return Err(refusal);
            }
            Some(claim)
        }
        None => None,
    };
    match db.set_session_key(session_id, key) {
        Ok(true) => Ok(SetKeyReport {
            previous,
            changed: true,
        }),
        Ok(false) => Err(format!("Session not found: {session_id}")),
        Err(e) => Err(format!("set_session_key: {e}")),
    }
}
