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

use crate::storage::{Database, DeletedSessionInfo};
use crate::sync::{current_time_millis, SharedSession};

/// The floor a hold outlives its holder by, before the lifecycle hooks' own
/// configured budget is added ([`hold_ttl_ms`]).
///
/// The two directions do not cost the same, which is why this is generous
/// rather than tight. **Too short** is the expensive one: a creation still
/// running when its claim lapses can have the name taken over, and the second
/// creator finds nothing — the first has not written its row yet — and spawns,
/// which is the pair this module exists to prevent. **Too long** costs a
/// deferred heal, because a creator killed mid-spawn holds the name until the
/// claim expires, and the heartbeat is back every 60 s meanwhile.
const HOLD_TTL_FLOOR_MS: u64 = 5 * 60 * 1000;

/// How long a hold taken now should last.
///
/// The floor covers the operation itself — an extension's declared session
/// spawns in place, no `worktree_branch`, so no `git fetch` and no checkout,
/// and is a tmux window and an agent launch; a restore recreates worktrees and
/// relaunches. What the floor cannot cover on its own is the part the *user*
/// sizes: every lifecycle hook runs inside one of these operations, in file
/// order, each bounded only by its own `timeout_secs` — an `Option<u64>` with
/// no cap, and the shipped template shows `timeout_secs = 120`. A hold shorter
/// than that budget lapses while its holder is still waiting on a hook, and the
/// next creator sees a free name and an absent row.
///
/// Every hook in the file is counted rather than the events one operation will
/// reach. It is a strict over-estimate — no operation runs them all — and that
/// is the direction the two errors point in: too long costs a deferred heal,
/// too short costs the pair. It also cannot go stale, which per-event budgets
/// did: the restore's hold was sized on the *creation* hooks and a long
/// `session.pre_restore` outlived it.
fn hold_ttl_ms() -> u64 {
    // Fully-qualified per the session_ops → agent path-only architecture rule.
    let hooks = crate::agent::hooks_config::load_or_seed();
    let budget: std::time::Duration = hooks.hooks.iter().map(|h| h.timeout()).sum();
    HOLD_TTL_FLOOR_MS.saturating_add(budget.as_millis() as u64)
}

/// A held name, and the token that releases it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NameClaim {
    backend: String,
    name: String,
    expires_at: u64,
}

/// The live sessions already carrying `name` on `backend`.
///
/// More than one is possible: thurbox enforces no uniqueness on the column, so
/// this reports what is there rather than assuming what should be.
pub fn live_namesakes(
    db: &Database,
    name: &str,
    backend: &str,
) -> Result<Vec<SharedSession>, String> {
    Ok(db
        .find_sessions_by_name(name)
        .map_err(|e| format!("find_sessions_by_name: {e}"))?
        .into_iter()
        .filter(|s| s.backend_type == backend)
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
) -> Result<Option<DeletedSessionInfo>, String> {
    let window = super::UNDO_WINDOW.as_millis() as u64;
    Ok(db
        .find_deleted_sessions_by_name(name)
        .map_err(|e| format!("find_deleted_sessions_by_name: {e}"))?
        .into_iter()
        .find(|row| {
            row.backend_type == backend
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
) -> Result<Vec<SharedSession>, String> {
    // Fully-qualified per the session_ops → agent path-only architecture rule.
    let window = crate::agent::tmux::sanitize_window_name(name);
    Ok(db
        .list_active_sessions()
        .map_err(|e| format!("list_active_sessions: {e}"))?
        .into_iter()
        .filter(|s| {
            s.backend_type == backend && crate::agent::tmux::sanitize_window_name(&s.name) == window
        })
        .collect())
}

/// Take the claim [`hold`] wraps, or `None` when it is already taken.
fn claim(db: &Database, name: &str, backend: &str) -> Result<Option<NameClaim>, String> {
    let now = current_time_millis();
    let expires_at = now.saturating_add(hold_ttl_ms());
    let won = db
        .claim_session_name(backend, name, expires_at, now)
        .map_err(|e| format!("claim_session_name: {e}"))?;
    Ok(won.then(|| NameClaim {
        backend: backend.to_string(),
        name: name.to_string(),
        expires_at,
    }))
}

/// Give a claim back. Best-effort: a claim that cannot be deleted expires on
/// its own, and failing a creation that already happened over its bookkeeping
/// would be the worse answer.
fn release(db: &Database, claim: NameClaim) {
    if let Err(e) = db.release_session_name(&claim.backend, &claim.name, claim.expires_at) {
        tracing::warn!(
            "could not release the name claim on '{}' ({}): {e}",
            claim.name,
            claim.backend
        );
    }
}

/// A claim held for as long as this value lives.
///
/// The release is structural rather than a call at each exit point, for the
/// reason `tests/support/tmux_server.rs` gives about tmux servers: an operation
/// that takes a name runs through `?` and early returns, and a claim leaked by
/// the one path that did not reach its release holds the name until it expires.
pub struct HeldName<'a> {
    db: &'a Database,
    claim: Option<NameClaim>,
}

impl Drop for HeldName<'_> {
    fn drop(&mut self) {
        if let Some(claim) = self.claim.take() {
            release(self.db, claim);
        }
    }
}

/// Hold `name` on `backend` for the length of an operation that must be the
/// only one deciding it — or `None` when somebody else already holds it.
///
/// A hold outlives its holder by a margin sized against the operation and the
/// lifecycle hooks it runs, so `None` means "held", not "somebody is definitely
/// working on it": a creator killed mid-spawn leaves one behind until it
/// expires. The row is deleted when the hold ends on the ordinary path and
/// overwritten by the next hold on that name otherwise, so a leaked one is
/// bounded by the names ever held rather than accumulating.
///
/// Both operations that take one are operations nobody is watching: self-heal
/// creating a declared session, and a restore un-deleting a name. They are the
/// same claim deliberately. A restore that only looked for a *live* namesake
/// would miss a creation that has claimed the name and not yet written its row
/// — it is inside a spawn, whose `session.pre_create` hooks alone can run for
/// minutes — and would un-delete into it, which is the pair again.
pub fn hold<'a>(
    db: &'a Database,
    name: &str,
    backend: &str,
) -> Result<Option<HeldName<'a>>, String> {
    Ok(claim(db, name, backend)?.map(|claim| HeldName {
        db,
        claim: Some(claim),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::SessionId;

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

    /// A hook's `timeout_secs` has no cap, so a hold sized on the floor alone
    /// can lapse while its holder is still waiting on one — and the name is then
    /// taken over while the operation that holds it has written no row. Every
    /// configured hook counts, not the events one operation reaches: sizing the
    /// restore's hold on the *creation* hooks is exactly how a long
    /// `session.pre_restore` outlived it.
    #[test]
    fn a_hold_outlasts_the_hooks_the_operation_will_wait_on() {
        let temp = tempfile::TempDir::new().expect("tempdir");
        let _guard = crate::paths::TestPathGuard::new(temp.path());
        let path = crate::agent::hooks_config::hooks_config_path().expect("hooks path");
        std::fs::create_dir_all(path.parent().expect("config dir")).expect("mkdir");
        std::fs::write(
            &path,
            "[[hooks]]\nevent = \"session.pre_restore\"\ncommand = \"sleep 900\"\n\
             timeout_secs = 900\n",
        )
        .expect("write hooks.toml");

        let db = db();
        let held = hold(&db, "build", "local-tmux")
            .expect("hold")
            .expect("nothing holds it");
        let expires_at = held.claim.as_ref().expect("a live hold").expires_at;
        assert!(
            expires_at.saturating_sub(current_time_millis()) > HOLD_TTL_FLOOR_MS,
            "the hook's 900 s has to be inside the hold, not beyond it"
        );
    }

    #[test]
    fn a_held_name_is_given_back_when_the_hold_ends() {
        let db = db();
        {
            let held = hold(&db, "build", "local-tmux")
                .expect("hold")
                .expect("the first holder wins");
            assert!(
                hold(&db, "build", "local-tmux").expect("hold").is_none(),
                "nobody else decides this name while it is held"
            );
            drop(held);
        }
        assert!(
            hold(&db, "build", "local-tmux").expect("hold").is_some(),
            "the hold ending gives the name back, on every path out"
        );
    }

    #[test]
    fn only_one_creator_holds_a_claim_at_a_time() {
        let db = db();
        let held = claim(&db, "build", "local-tmux")
            .expect("claim")
            .expect("the first creator wins");
        assert!(
            claim(&db, "build", "local-tmux").expect("claim").is_none(),
            "the second must not spawn"
        );
        assert!(
            claim(&db, "build", "ssh:devbox").expect("claim").is_some(),
            "a claim is per backend, like every other question about a name"
        );

        release(&db, held);
        assert!(
            claim(&db, "build", "local-tmux").expect("claim").is_some(),
            "the name is available again once the creation is over"
        );
    }

    #[test]
    fn an_expired_claim_is_taken_over_rather_than_waited_on() {
        let db = db();
        let now = current_time_millis();
        assert!(db
            .claim_session_name("local-tmux", "build", now - 1, now)
            .expect("claim"));
        assert!(
            claim(&db, "build", "local-tmux").expect("claim").is_some(),
            "a creator that died mid-spawn must not hold the name forever"
        );
    }

    #[test]
    fn releasing_an_overrun_claim_leaves_its_successor_alone() {
        let db = db();
        let now = current_time_millis();
        let overrun = NameClaim {
            backend: "local-tmux".into(),
            name: "build".into(),
            expires_at: now - 1,
        };
        let successor = claim(&db, "build", "local-tmux")
            .expect("claim")
            .expect("the expired claim is taken over");

        release(&db, overrun);
        assert!(
            claim(&db, "build", "local-tmux").expect("claim").is_none(),
            "the successor still holds it"
        );
        release(&db, successor);
    }
}
