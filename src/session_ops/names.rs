//! Whether a session name is free to take on a backend, and the claim that
//! stops two creators from both deciding that it is.
//!
//! The rule the CLI's `--on-existing` already applied to `session create` —
//! a name is a question about **one backend**, because a database mirroring a
//! shareable host (ADR-24) holds that host's rows beside its own and two
//! machines may legitimately each have a session called `build`. Self-heal
//! never asked it: `ensure_extension` looked the name up across every backend
//! and spawned if it was absent, so a namesake on another machine answered for
//! the local session while two local sessions of one name went unnoticed
//! (issue #1192).
//!
//! Three things can hold a name, and the first two are only visible to a
//! lookup that knows to ask:
//!
//! - a **live** session — [`live_namesakes`];
//! - a **soft-deleted** one whose undo window is still open
//!   ([`undoable_namesake`]): the delete can still be taken back, and a
//!   creation now is a pair the moment it is;
//! - another **creator**, mid-spawn ([`hold`]). A spawn runs for tens of
//!   seconds, so "look, then create" is not a claim at all.

use super::lock::{Lock, Subject};
use crate::storage::{Database, DeletedSessionInfo};
use crate::sync::SharedSession;

/// The live sessions already carrying `name` on `backend`'s server
/// ([`super::server_key`]).
///
/// More than one is possible: thurbox enforces no uniqueness on the column, so
/// this reports what is there rather than assuming what should be.
pub fn live_namesakes(
    db: &Database,
    name: &str,
    backend: &str,
) -> Result<Vec<SharedSession>, crate::session_ops::OpError> {
    let server = super::server_key(backend);
    Ok(db
        .find_sessions_by_name(name)
        .map_err(|e| format!("find_sessions_by_name: {e}"))?
        .into_iter()
        .filter(|s| super::server_key(&s.backend_type) == server)
        .collect())
}

/// A soft-deleted session of that name on that backend whose undo window is
/// still open — the delete can still be taken back, so the name is spoken for.
///
/// Bounded by [`super::UNDO_WINDOW`] and not by "is there a deleted row", which
/// is a condition that never ends: a soft-deleted row stays restorable after the
/// reaper has let go of its agent, so refusing on its account would mean an
/// extension's session, once deleted, never came back. The window is the one
/// interval in which two answers are both still true, and it is the durable
/// question the reaper already asks (`deleted_at + UNDO_WINDOW`).
pub fn undoable_namesake(
    db: &Database,
    name: &str,
    backend: &str,
    now: u64,
) -> Result<Option<DeletedSessionInfo>, crate::session_ops::OpError> {
    let window = super::UNDO_WINDOW.as_millis() as u64;
    let server = super::server_key(backend);
    Ok(db
        .find_deleted_sessions_by_name(name)
        .map_err(|e| format!("find_deleted_sessions_by_name: {e}"))?
        .into_iter()
        .find(|row| {
            super::server_key(&row.backend_type) == server
                // A force delete cannot be undone, so it frees the name.
                && !row.force_deleted
                && now.saturating_sub(row.deleted_at) < window
        }))
}

/// The live sessions on `backend` whose name would resolve to the same tmux
/// window as `name`.
///
/// Stricter than [`live_namesakes`], and the right test wherever the answer is
/// a hard refusal rather than a choice the caller makes. `sanitize_window_name`
/// folds everything outside `[A-Za-z0-9_-]` to `_` while `validate_safe_name`
/// admits `.`, `:` and spaces, so `deploy prod` and `deploy.prod` are two names
/// and one `tb-deploy_prod` — and on a multiplexer that carries no window stamp
/// (psmux) that name is all a later teardown has to go on. `rename` refuses on
/// this test for the same reason.
pub fn window_namesakes(
    db: &Database,
    name: &str,
    backend: &str,
) -> Result<Vec<SharedSession>, crate::session_ops::OpError> {
    // Fully-qualified per the session_ops → agent path-only architecture rule.
    let window = crate::backend::identity::sanitize_window_name(name);
    let server = super::server_key(backend);
    Ok(db
        .list_active_sessions()
        .map_err(|e| format!("list_active_sessions: {e}"))?
        .into_iter()
        .filter(|s| {
            super::server_key(&s.backend_type) == server
                && crate::backend::identity::sanitize_window_name(&s.name) == window
        })
        .collect())
}

/// Lock `name` on `backend` for the length of an operation that must be the
/// only one deciding it — or `None` when somebody else already holds it.
///
/// Locked per server ([`super::server_key`]), so two spellings of one server
/// contend for one name, and two machines' namesakes do not.
///
/// Every operation that decides a name unwatched or on a promise takes it:
/// self-heal creating a declared session, a restore un-deleting a name, and
/// `session create --on-existing fail|adopt|replace`. They are the same lock
/// deliberately. A restore that only looked for a *live* namesake would miss a
/// creation that has locked the name and not yet written its row — it is
/// inside a spawn, whose `session.pre_create` hooks alone can run for minutes
/// — and would un-delete into it, which is the pair again.
pub fn hold<'a>(
    db: &'a Database,
    name: &str,
    backend: &str,
    op: &str,
) -> Result<Option<Lock<'a>>, crate::session_ops::OpError> {
    let subject = Subject::Name {
        scope: super::server_key(backend),
        name: name.to_string(),
    };
    super::lock::lock(db, subject, op)
}

/// Refuse a key that is not one: held to a name's rules (1-64 bytes, no `/` or
/// `\`, no `..`, no leading `.`), so `key:<host>/<key>` stays parseable — the
/// first `/` is the host's.
pub fn validate_key(key: &str) -> Result<(), crate::session_ops::OpError> {
    Ok(crate::paths::validate_safe_name(key).map_err(|e| format!("invalid key '{key}': {e}"))?)
}

/// The scope a key is unique in: the host a session on `backend` runs on.
///
/// The host, not [`super::server_key`]: a key names a role, so the same key
/// under tmux and under psmux on one machine would be the twin the key exists
/// to rule out. `None` is this machine.
pub fn key_host(backend: &str) -> Option<String> {
    crate::session::Route::parse(backend)
        .ok()
        .and_then(|route| route.host().map(str::to_string))
}

/// The live sessions on `backend`'s host already carrying `key`.
///
/// One at most in a database that has only ever written keys through
/// [`claim_key`]; a list because a mirrored host's rows may hold a stale
/// duplicate for one pass, and because reporting what is there beats assuming
/// what should be.
pub fn live_key_holders(
    db: &Database,
    key: &str,
    backend: &str,
) -> Result<Vec<SharedSession>, crate::session_ops::OpError> {
    Ok(db
        .find_sessions_by_key(key)
        .map_err(|e| format!("find_sessions_by_key: {e}"))?
        .into_iter()
        .filter(|s| super::same_machine(&s.backend_type, backend))
        .collect())
}

/// The outcome of [`claim_key`].
pub enum KeyClaim<'a> {
    /// Nobody on the host has the key and nobody else is deciding it: give it to
    /// a row while this lives.
    Held(Lock<'a>),
    /// Another creator is mid-way through giving the key to a row.
    Busy,
    /// A live session on the host already carries it.
    Taken(Box<SharedSession>),
}

/// Claim `key` on `backend`'s host for the length of an operation that gives it
/// to a row, and say whether it is free.
///
/// The same lock [`hold`] takes for a name, in a scope of its own (`key@<host>`,
/// which no server key spells), so "look, then write" is one decision for every
/// path that writes a key: create, `set-key`, restore, `register`. A
/// soft-deleted row does not reserve its key the way [`undoable_namesake`]
/// reserves a name — otherwise an extension could not self-heal its keyed
/// session until the undo window had closed.
pub fn claim_key<'a>(
    db: &'a Database,
    key: &str,
    backend: &str,
    op: &str,
) -> Result<KeyClaim<'a>, crate::session_ops::OpError> {
    let subject = Subject::Name {
        scope: format!("key@{}", key_host(backend).unwrap_or_default()),
        name: key.to_string(),
    };
    let Some(held) = super::lock::lock(db, subject, op)? else {
        return Ok(KeyClaim::Busy);
    };
    Ok(
        match live_key_holders(db, key, backend)?.into_iter().next() {
            Some(holder) => KeyClaim::Taken(Box::new(holder)),
            None => KeyClaim::Held(held),
        },
    )
}

/// Why `key` could not be given to a session on `backend`, for a caller that
/// refuses rather than chooses.
pub fn key_refusal(key: &str, backend: &str, claim: &KeyClaim<'_>) -> Option<String> {
    let place = key_host(backend).unwrap_or_else(|| "this machine".to_string());
    match claim {
        KeyClaim::Held(_) => None,
        KeyClaim::Busy => Some(format!(
            "key '{key}' is being given to a session on {place} right now; try again once \
             that has finished"
        )),
        KeyClaim::Taken(holder) => Some(format!(
            "key '{key}' already belongs to session '{}' ({}) on {place}",
            holder.name, holder.id
        )),
    }
}

/// The key a row that is being recorded **anyway** may keep — for a writer
/// that does not refuse over a key: a restore (the session that has the key now
/// keeps it) and `session register` (the window is already running).
///
/// Returns the claim to hold until the row is written, and — when the key is
/// taken or being given out — why the row must be written without it. `None`
/// for both when there is no key to admit.
pub fn admit_key<'a>(
    db: &'a Database,
    key: Option<&str>,
    backend: &str,
    op: &str,
) -> Result<(Option<KeyClaim<'a>>, Option<String>), crate::session_ops::OpError> {
    let Some(key) = key else {
        return Ok((None, None));
    };
    let claim = claim_key(db, key, backend, op)?;
    let refusal = key_refusal(key, backend, &claim);
    Ok((Some(claim), refusal))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::SessionId;
    use crate::sync::current_time_millis;

    fn db() -> Database {
        Database::open_in_memory().expect("in-memory db")
    }

    fn insert(db: &Database, name: &str, backend: &str) -> SessionId {
        let id = SessionId::default();
        db.upsert_session(&SharedSession {
            id,
            name: name.into(),
            agent: "shell".into(),
            backend_id: String::new(),
            backend_type: backend.into(),
            agent_session_id: Some(uuid::Uuid::new_v4().to_string()),
            cwd: None,
            additional_dirs: Vec::new(),
            worktrees: Vec::new(),
            shell_backend_id: None,
            parent_session_id: None,
            display_order: None,
            tombstone: false,
            tombstone_at: None,
            key: None,
        })
        .expect("upsert");
        id
    }

    #[test]
    fn a_namesake_on_another_backend_does_not_hold_the_name() {
        let db = db();
        insert(&db, "build", "ssh:devbox");
        assert!(live_namesakes(&db, "build", "local-tmux")
            .expect("lookup")
            .is_empty());
        assert_eq!(
            live_namesakes(&db, "build", "ssh:devbox")
                .expect("lookup")
                .len(),
            1
        );
    }

    #[test]
    fn a_soft_delete_holds_the_name_only_while_it_can_be_undone() {
        let db = db();
        let id = insert(&db, "build", "local-tmux");
        db.soft_delete_session(id).expect("soft delete");
        let now = current_time_millis();
        let window = super::super::UNDO_WINDOW.as_millis() as u64;

        assert!(
            undoable_namesake(&db, "build", "local-tmux", now)
                .expect("lookup")
                .is_some(),
            "the undo is still on offer"
        );
        assert!(
            undoable_namesake(&db, "build", "local-tmux", now + window)
                .expect("lookup")
                .is_none(),
            "the window has closed; the name is free again"
        );
        assert!(
            undoable_namesake(&db, "build", "ssh:devbox", now)
                .expect("lookup")
                .is_none(),
            "the question is about one backend"
        );
    }

    #[test]
    fn a_force_delete_frees_the_name_at_once() {
        let db = db();
        let id = insert(&db, "build", "local-tmux");
        db.force_delete_session(id).expect("force delete");
        assert!(
            undoable_namesake(&db, "build", "local-tmux", current_time_millis())
                .expect("lookup")
                .is_none(),
            "a force delete cannot be undone, so nothing is waiting to come back"
        );
    }

    #[test]
    fn two_names_that_share_one_window_are_one_name_to_a_hard_refusal() {
        let db = db();
        insert(&db, "deploy prod", "local-tmux");

        assert!(
            live_namesakes(&db, "deploy.prod", "local-tmux")
                .expect("lookup")
                .is_empty(),
            "the names differ, which is all a caller offered a choice needs"
        );
        assert_eq!(
            window_namesakes(&db, "deploy.prod", "local-tmux")
                .expect("lookup")
                .len(),
            1,
            "both sanitize to tb-deploy_prod, and a stampless window is found \
             by that name alone"
        );
        assert!(
            window_namesakes(&db, "deploy.prod", "ssh:devbox")
                .expect("lookup")
                .is_empty(),
            "still a question about one backend"
        );
    }

    #[test]
    fn a_name_is_held_per_backend_until_the_hold_ends() {
        let db = db();
        let held = hold(&db, "build", "local-tmux", "heal")
            .expect("hold")
            .expect("the first holder wins");
        assert!(
            hold(&db, "build", "local-tmux", "create")
                .expect("hold")
                .is_none(),
            "nobody else decides this name while it is held"
        );
        assert!(
            hold(&db, "build", "ssh:devbox", "create")
                .expect("hold")
                .is_some(),
            "a hold is per backend, like every other question about a name"
        );
        drop(held);
        assert!(
            hold(&db, "build", "local-tmux", "create")
                .expect("hold")
                .is_some(),
            "the hold ending gives the name back, on every path out"
        );
    }

    fn insert_keyed(db: &Database, name: &str, backend: &str, key: &str) -> SessionId {
        let id = insert(db, name, backend);
        assert!(db.set_session_key(id, Some(key)).expect("set key"));
        id
    }

    fn is_held(claim: &KeyClaim<'_>) -> bool {
        matches!(claim, KeyClaim::Held(_))
    }

    #[test]
    fn a_key_is_held_to_a_names_rules_so_its_host_form_stays_parseable() {
        assert!(validate_key("fleet.acme.mission-control").is_ok());
        for bad in ["", "a/b", "a\\b", "..", ".hidden", &"k".repeat(65)] {
            assert!(validate_key(bad).is_err(), "{bad:?} must be refused");
        }
    }

    /// The scope is the host, not the server: one machine under two
    /// multiplexers is one place for a role to be twinned in.
    #[test]
    fn a_key_is_unique_per_host_whatever_multiplexer_serves_it() {
        let db = db();
        let holder = insert_keyed(&db, "lead", "local-tmux", "fleet.lead");

        match claim_key(&db, "fleet.lead", "local-psmux", "create").expect("claim") {
            KeyClaim::Taken(found) => assert_eq!(found.id, holder),
            _ => panic!("a live session on this machine has the key"),
        }
        assert!(
            is_held(&claim_key(&db, "fleet.lead", "ssh:devbox", "create").expect("claim")),
            "another host's database is the one that rules on its keys"
        );
        assert_eq!(key_host("ssh:devbox:rmux").as_deref(), Some("devbox"));
        assert_eq!(key_host("local-tmux"), None);
    }

    #[test]
    fn a_held_key_is_busy_to_everyone_else_until_the_hold_ends() {
        let db = db();
        let first = claim_key(&db, "fleet.lead", "local-tmux", "create").expect("claim");
        assert!(is_held(&first));
        assert!(matches!(
            claim_key(&db, "fleet.lead", "local-tmux", "create").expect("claim"),
            KeyClaim::Busy
        ));
        assert!(key_refusal("fleet.lead", "local-tmux", &KeyClaim::Busy)
            .is_some_and(|why| why.contains("right now")),);
        drop(first);
        assert!(is_held(
            &claim_key(&db, "fleet.lead", "local-tmux", "create").expect("claim")
        ));
    }

    /// Unlike a name, a soft-deleted row's key is not spoken for: an extension
    /// has to be able to heal its keyed session inside the undo window.
    #[test]
    fn a_deleted_row_does_not_reserve_its_key() {
        let db = db();
        let gone = insert_keyed(&db, "lead", "local-tmux", "fleet.lead");
        db.soft_delete_session(gone).expect("soft delete");
        assert!(is_held(
            &claim_key(&db, "fleet.lead", "local-tmux", "create").expect("claim")
        ));
    }

    #[test]
    fn a_row_recorded_anyway_keeps_its_key_only_when_it_is_free() {
        let db = db();
        let (claim, dropped) =
            admit_key(&db, Some("fleet.lead"), "local-tmux", "restore").expect("admit");
        assert!(claim.is_some_and(|c| is_held(&c)) && dropped.is_none());

        insert_keyed(&db, "lead", "local-tmux", "fleet.lead");
        let (_, dropped) =
            admit_key(&db, Some("fleet.lead"), "local-tmux", "restore").expect("admit");
        assert!(
            dropped.is_some_and(|why| why.contains("lead")),
            "says which session has it"
        );
        let (claim, dropped) = admit_key(&db, None, "local-tmux", "restore").expect("admit");
        assert!(claim.is_none() && dropped.is_none());
    }
}
