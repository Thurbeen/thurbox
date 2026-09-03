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

use crate::cli::{CommandError, EXIT_AMBIGUOUS};
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
/// `Err` carries the rendered explanation and the exit code the failure earns:
/// a reference matching several sessions exits [`EXIT_AMBIGUOUS`] rather than
/// the generic failure code, because the two failures ask different things of
/// the caller — one is reconciled by creating a session, the other only by an
/// operator picking which was meant.
pub fn resolve(db: &Database, reference: &str) -> Result<SharedSession, CommandError> {
    resolve_detailed(db, reference).map_err(|(reason, kind)| match kind {
        Ambiguity::NotFound => CommandError::from(reason),
        Ambiguity::Many(_) => CommandError::with_code(reason, EXIT_AMBIGUOUS),
    })
}

/// [`resolve`], also returning which kind of failure it was — the distinction
/// [`resolve`] turns into an exit code (AXI principle 6 asks a usage error and
/// a missing thing to exit differently), and which a caller rendering its own
/// refusal document reads directly.
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

/// Resolve a reference against the **deleted** rows, the way [`resolve`] does
/// against the active ones.
///
/// `session restore` is the one verb whose subject is a row that no longer
/// exists, so it cannot share the active-session resolver — but it can share
/// its rules, and a driver that holds a name rather than a UUID has no other
/// way in. Same three spellings, same refusal to guess between two candidates,
/// same exit code for the ambiguity.
///
/// Filtered in memory rather than in SQL because the deleted set is what
/// `session list --deleted` already reads whole.
pub fn resolve_deleted(
    db: &Database,
    reference: &str,
) -> Result<crate::storage::DeletedSessionInfo, CommandError> {
    let deleted = db
        .list_deleted_sessions()
        .map_err(|e| CommandError::from(format!("list_deleted_sessions: {e}")))?;
    if let Ok(id) = reference.parse::<SessionId>() {
        return deleted
            .into_iter()
            .find(|d| d.id == id)
            .ok_or_else(|| CommandError::from(format!("Deleted session not found: {reference}")));
    }
    let by_name: Vec<_> = deleted
        .iter()
        .filter(|d| d.name == reference)
        .cloned()
        .collect();
    if let Some(one) = pick_deleted(reference, by_name)? {
        return Ok(one);
    }
    let by_prefix: Vec<_> = deleted
        .into_iter()
        .filter(|d| d.id.to_string().starts_with(reference))
        .collect();
    pick_deleted(reference, by_prefix)?.ok_or_else(|| {
        CommandError::from(format!(
            "Deleted session not found: {reference} (tried it as a UUID, a name, and an id prefix)"
        ))
    })
}

/// One candidate, none, or a refusal naming them all — the deleted half of the
/// rule [`resolve`] follows.
fn pick_deleted(
    reference: &str,
    mut found: Vec<crate::storage::DeletedSessionInfo>,
) -> Result<Option<crate::storage::DeletedSessionInfo>, CommandError> {
    match found.len() {
        0 => Ok(None),
        1 => Ok(Some(found.remove(0))),
        n => {
            let list = found
                .iter()
                .map(|d| format!("  {}  {}", d.id, d.name))
                .collect::<Vec<_>>()
                .join("\n");
            Err(CommandError::with_code(
                format!("'{reference}' matches {n} deleted sessions:\n{list}"),
                EXIT_AMBIGUOUS,
            ))
        }
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
        assert!(err.message.contains("nope"));
        assert!(
            err.message.contains("prefix"),
            "the message names every spelling"
        );
        assert_eq!(err.exit_code, crate::cli::EXIT_ERROR);
    }

    #[test]
    fn an_ambiguous_reference_exits_differently_from_a_missing_one() {
        // A driver reconciles "no such session" by creating one; only an
        // operator can settle "two sessions answer to that name". Telling them
        // apart used to mean string-matching the message.
        let db = db();
        db.upsert_session(&row("twin")).unwrap();
        db.upsert_session(&row("twin")).unwrap();
        assert_eq!(
            resolve(&db, "twin").unwrap_err().exit_code,
            crate::cli::EXIT_AMBIGUOUS
        );
        assert_eq!(
            resolve(&db, "solo").unwrap_err().exit_code,
            crate::cli::EXIT_ERROR
        );
    }
}
