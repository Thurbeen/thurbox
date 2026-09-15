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
/// A name is also taken when it names the same window as another session's:
/// a window name folds everything outside `[A-Za-z0-9_-]` to `_`, so `a:b` and
/// `a.b` share `tb-a_b`, and where a window carries no stamp (psmux) that name
/// is all that finds it.
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

    let window = crate::agent::tmux::sanitize_window_name(name);
    let taken: Vec<String> = db
        .list_active_sessions()
        .map_err(|e| format!("list_active_sessions: {e}"))?
        .into_iter()
        .filter(|s| {
            s.id != session_id
                && s.backend_type == session.backend_type
                && crate::agent::tmux::sanitize_window_name(&s.name) == window
        })
        .map(|s| s.id.to_string())
        .collect();
    if !taken.is_empty() {
        return Err(format!(
            "another session on {} ({}) already has the name '{name}', or one that names \
             the same window. Pick another name",
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

    let id = session_id.to_string();
    let written =
        crate::agent::tmux::rename_session_windows(host.as_ref(), &id, &session.name, name)
            .map_err(|e| format!("could not rename the windows of '{}': {e:#}", session.name))
            .and_then(|()| match db.rename_session(session_id, name) {
                Ok(true) => Ok(()),
                Ok(false) => Err(format!("Session not found: {session_id}")),
                Err(e) => Err(format!("rename_session: {e}")),
            });
    if let Err(error) = written {
        // Some window may now carry a name the row does not — the agent's, when
        // the shell's rename failed after it, or both, when the row could not be
        // written. A window with no stamp is found by the row's name alone: left
        // like this it reads as gone, and a relaunch would start a second agent
        // beside it. Nothing renamed is found under the new name, so putting
        // back is harmless when the first rename failed outright.
        if let Err(e) =
            crate::agent::tmux::rename_session_windows(host.as_ref(), &id, name, &session.name)
        {
            tracing::warn!(
                "could not put back the windows of '{}': {e:#}",
                session.name
            );
        }
        return Err(error);
    }
    Ok(RenameReport {
        previous: session.name,
        renamed: true,
    })
}
