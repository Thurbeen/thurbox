//! Renaming a session: its row, and the windows named after it.
//!
//! One pipeline for the CLI and the interface, as the other lifecycle
//! operations are, so a name `session rename` refuses is refused by the
//! interface for the same reason and in the same words.

use crate::session::SessionId;
use crate::storage::Database;

/// What a rename did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenameReport {
    /// The name the session had.
    pub previous: String,
    /// False when the session already had the name asked for.
    pub renamed: bool,
}

/// Rename a session, refusing what `session create` would refuse.
///
/// The rules are create's: [`crate::paths::validate_safe_name`] for the name
/// itself, and `--on-existing fail`'s for a collision — another active session
/// of that name on the same backend. Create *allows* a namesake by default, but
/// a name matching several sessions is then refused wherever one is typed, and
/// a rename is a name chosen on purpose. Other backends are not consulted, for
/// the reason create gives: a mirrored host's `build` is not this machine's.
///
/// The windows are renamed before the row: a window with no stamp is found by
/// the name the row still has, so the other order would lose it.
pub fn rename_session_headless(
    db: &Database,
    session_id: SessionId,
    name: &str,
) -> Result<RenameReport, String> {
    crate::paths::validate_safe_name(name)?;
    let session = db
        .get_session_by_id(session_id)
        .map_err(|e| format!("Failed to load session: {e}"))?
        .ok_or_else(|| format!("Session not found: {session_id}"))?;
    if session.name == name {
        return Ok(RenameReport {
            previous: session.name,
            renamed: false,
        });
    }

    let taken: Vec<String> = db
        .find_sessions_by_name(name)
        .map_err(|e| format!("find_sessions_by_name: {e}"))?
        .into_iter()
        .filter(|s| s.id != session_id && s.backend_type == session.backend_type)
        .map(|s| s.id.to_string())
        .collect();
    if !taken.is_empty() {
        return Err(format!(
            "'{name}' is already the name of another session on {} ({}). Pick another name",
            session.backend_type,
            taken.join(", ")
        ));
    }

    let host = super::resolve_host(&session.backend_type).ok_or_else(|| {
        format!(
            "Session '{}' runs on backend '{}', which is not in hosts.toml — \
             cannot reach the machine it lives on",
            session.name, session.backend_type
        )
    })?;

    // A shareable host's database is the record (ADR-24): the host renames its
    // own row and windows, and this row follows it by mirroring.
    if let Some((host, cli)) = host
        .as_ref()
        .and_then(|h| super::host_cli::delegated(h).map(|cli| (h, cli)))
    {
        let id = session_id.to_string();
        super::host_cli::run(host, &cli, &["session", "rename", &id, name])?;
        if let Err(e) = super::mirror::mirror_host(db, host, &cli) {
            tracing::warn!("mirror of '{}' after rename failed: {e}", host.name);
        }
        return Ok(RenameReport {
            previous: session.name,
            renamed: true,
        });
    }

    crate::agent::tmux::rename_session_windows(
        host.as_ref(),
        &session_id.to_string(),
        &session.name,
        name,
    )
    .map_err(|e| format!("could not rename the windows of '{}': {e:#}", session.name))?;
    db.rename_session(session_id, name)
        .map_err(|e| format!("rename_session: {e}"))?;
    Ok(RenameReport {
        previous: session.name,
        renamed: true,
    })
}
