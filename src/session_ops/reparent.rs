//! Moving a session under another parent — what a lead migration needs.
//!
//! The parent link is informational (nothing cascades through it), but drivers
//! filter on it: `session list --parent <lead>` is how a lead finds its
//! workers. When a lead is replaced, its workers have to follow it, and before
//! this verb the only way to move them was an `UPDATE` against the database.

use std::collections::HashSet;

use crate::session::SessionId;
use crate::storage::Database;

/// Point `id` at `parent`, or make it top-level with `None`.
///
/// Refuses a link that would make a cycle — the session under itself, or under
/// one of its own descendants — because the interface nests rows by walking
/// this link. A session on a shareable host is reparented by the host, whose
/// record the next mirror pass would otherwise copy back over this one.
pub fn reparent_session_headless(
    db: &Database,
    id: SessionId,
    parent: Option<SessionId>,
) -> Result<(), String> {
    let session = db
        .get_session_by_id(id)
        .map_err(|e| format!("Failed to load session: {e}"))?
        .ok_or_else(|| format!("Session not found: {id}"))?;
    if let Some(parent) = parent {
        refuse_cycle(db, &session.name, id, parent)?;
    }

    if let Some(Some(host)) = super::resolve_host(&session.backend_type) {
        if let Some(cli) = super::host_cli::delegated(&host) {
            let id = id.to_string();
            let target = parent.map_or_else(|| "--clear".to_string(), |p| p.to_string());
            super::host_cli::run(&host, &cli, &["session", "reparent", &id, &target])?;
            if let Err(e) = super::mirror::mirror_host(db, &host, &cli) {
                tracing::warn!("mirror of '{}' after reparent failed: {e}", host.name);
            }
            return Ok(());
        }
    }

    match db.set_session_parent(id, parent) {
        Ok(true) => Ok(()),
        Ok(false) => Err(format!("Session not found: {id}")),
        Err(e) => Err(format!(
            "could not set the parent of '{}': {e}",
            session.name
        )),
    }
}

/// Walk up from `parent`; reaching `id` means `parent` is `id` or below it.
fn refuse_cycle(db: &Database, name: &str, id: SessionId, parent: SessionId) -> Result<(), String> {
    if parent == id {
        return Err(format!("'{name}' cannot be its own parent"));
    }
    let mut seen = HashSet::new();
    let mut cursor = Some(parent);
    while let Some(at) = cursor.filter(|at| seen.insert(*at)) {
        if at == id {
            return Err(format!(
                "{parent} is a descendant of '{name}', so it cannot be its parent"
            ));
        }
        cursor = db
            .get_session_by_id(at)
            .map_err(|e| format!("Failed to load session: {e}"))?
            .and_then(|s| s.parent_session_id);
    }
    Ok(())
}
