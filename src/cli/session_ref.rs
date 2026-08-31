//! Resolving the thing a user typed to the session they meant.
//!
//! Every session verb takes a reference, and until this module there were two
//! answers to what one *is*: `message send --to` accepted a name or a UUID,
//! while everything else accepted a UUID alone. A driver therefore had to keep
//! its own name-to-id map and refresh it, for no reason other than which
//! subcommand it was calling.
//!
//! One resolver, three spellings, in the order that cannot surprise:
//!
//! 1. a full UUID — unambiguous by construction, so it is tried first and an
//!    id that parses is never also looked up as a name;
//! 2. an exact name;
//! 3. a UUID prefix, for typing the first few characters of an id.
//!
//! **Ambiguity is an error, never a guess.** Names are not unique (thurbox does
//! not enforce it, and a mirrored host contributes rows that legitimately
//! collide), so a reference matching more than one session is refused with
//! every candidate named. Acting on whichever row sorted first is the one
//! behaviour that would make a driver corrupt the wrong session silently.

use crate::session::SessionId;
use crate::storage::Database;
use crate::sync::SharedSession;

/// What a reference resolved to, or why it could not.
pub enum Ambiguity {
    /// No active session matches.
    NotFound,
    /// More than one does — the candidates, for the message.
    Many(Vec<SharedSession>),
}

/// Resolve a reference to exactly one active session.
///
/// `Err` carries the rendered explanation; callers that need to distinguish
/// "none" from "several" use [`resolve_detailed`].
pub fn resolve(db: &Database, reference: &str) -> Result<SharedSession, String> {
    resolve_detailed(db, reference).map_err(|(reason, _)| reason)
}

/// [`resolve`], also returning which kind of failure it was so a caller can
/// pick an exit code (AXI principle 6 asks a usage error and a missing thing to
/// exit differently).
pub fn resolve_detailed(
    db: &Database,
    reference: &str,
) -> Result<SharedSession, (String, Ambiguity)> {
    if let Ok(id) = reference.parse::<SessionId>() {
        if let Some(found) = db
            .get_session_by_id(id)
            .map_err(|e| (format!("get_session_by_id: {e}"), Ambiguity::NotFound))?
        {
            return Ok(found);
        }
        // A well-formed id that matches nothing is not worth trying as a name:
        // no session is called that.
        return Err((
            format!("Session not found: {reference}"),
            Ambiguity::NotFound,
        ));
    }

    let by_name = db
        .find_sessions_by_name(reference)
        .map_err(|e| (format!("find_sessions_by_name: {e}"), Ambiguity::NotFound))?;
    match by_name.len() {
        1 => return Ok(by_name.into_iter().next().expect("length checked")),
        n if n > 1 => return Err((ambiguous(reference, &by_name), Ambiguity::Many(by_name))),
        _ => {}
    }

    // Only now a prefix: an exact name always beats a partial id, so a session
    // literally called `ab` is reachable even while an id starts with `ab`.
    let by_prefix = db.find_sessions_by_id_prefix(reference).map_err(|e| {
        (
            format!("find_sessions_by_id_prefix: {e}"),
            Ambiguity::NotFound,
        )
    })?;
    match by_prefix.len() {
        1 => Ok(by_prefix.into_iter().next().expect("length checked")),
        0 => Err((
            format!(
                "Session not found: {reference} (tried it as a UUID, a name, and an id prefix)"
            ),
            Ambiguity::NotFound,
        )),
        _ => Err((ambiguous(reference, &by_prefix), Ambiguity::Many(by_prefix))),
    }
}

/// The refusal for a reference that matches several sessions: every candidate,
/// spelled as the id that would resolve it, so the fix is copy-pasteable.
fn ambiguous(reference: &str, found: &[SharedSession]) -> String {
    let list = found
        .iter()
        .map(|s| format!("  {}  {}", s.id, s.name))
        .collect::<Vec<_>>()
        .join("\n");
    format!("'{reference}' matches {} sessions:\n{list}", found.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync::SharedSession;

    fn row(name: &str) -> SharedSession {
        SharedSession {
            id: SessionId::default(),
            name: name.to_string(),
            agent: "claude".into(),
            backend_id: String::new(),
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
        }
    }

    fn db() -> Database {
        Database::open_in_memory().expect("in-memory db")
    }

    #[test]
    fn a_name_resolves_the_way_a_uuid_does() {
        let db = db();
        let s = row("fix-ci");
        db.upsert_session(&s).unwrap();
        assert_eq!(resolve(&db, "fix-ci").unwrap().id, s.id);
        assert_eq!(resolve(&db, &s.id.to_string()).unwrap().id, s.id);
    }

    #[test]
    fn an_id_prefix_resolves_but_never_beats_an_exact_name() {
        let db = db();
        let s = row("worker");
        db.upsert_session(&s).unwrap();
        let prefix = &s.id.to_string()[..8];
        assert_eq!(resolve(&db, prefix).unwrap().id, s.id);

        // A session *named* like another's id prefix still wins on its name.
        let mut named_like_prefix = row(prefix);
        named_like_prefix.id = SessionId::default();
        db.upsert_session(&named_like_prefix).unwrap();
        assert_eq!(resolve(&db, prefix).unwrap().id, named_like_prefix.id);
    }

    #[test]
    fn a_duplicated_name_is_refused_with_both_candidates() {
        let db = db();
        let a = row("twin");
        let b = row("twin");
        db.upsert_session(&a).unwrap();
        db.upsert_session(&b).unwrap();
        let (message, kind) = resolve_detailed(&db, "twin").unwrap_err();
        assert!(matches!(kind, Ambiguity::Many(found) if found.len() == 2));
        // The fix has to be in the message: both ids, ready to paste.
        assert!(message.contains(&a.id.to_string()));
        assert!(message.contains(&b.id.to_string()));
    }

    #[test]
    fn a_reference_that_matches_nothing_says_what_it_tried() {
        let db = db();
        let err = resolve(&db, "nope").unwrap_err();
        assert!(err.contains("nope"));
        assert!(err.contains("prefix"), "the message names every spelling");
    }
}
