//! The one lock every mutator that must be alone on a subject takes (issue
//! #1475): a creator giving out a name or a key, a restore taking a name back,
//! a restart replacing a row's window, a fire of one automation.
//!
//! An expiring lease in `event_cursors` rather than an in-process lock,
//! because the processes that race — the interface, every agent hook's
//! `thurbox-cli`, the heartbeat's self-heal — share nothing else. Two
//! operations on one subject contend whatever they are; `op` only names the
//! holder in what the lock logs.

use crate::storage::Database;
use crate::sync::current_time_millis;

pub use crate::storage::event_cursors::LockSubject as Subject;

/// The floor a lock outlives its holder by, before the lifecycle hooks' own
/// configured budget is added ([`ttl_ms`]).
///
/// The two directions do not cost the same, which is why this is generous
/// rather than tight. **Too short** is the expensive one: a creation still
/// running when its lock lapses can have the name taken over, and the second
/// creator finds nothing — the first has not written its row yet — and spawns,
/// which is the pair this lock exists to prevent. **Too long** costs a
/// deferred heal, because a creator killed mid-spawn holds the name until the
/// lock expires, and the heartbeat is back every 60 s meanwhile.
const TTL_FLOOR_MS: u64 = 5 * 60 * 1000;

/// How long a lock taken now should last.
///
/// The floor covers the operation itself — an extension's declared session
/// spawns in place, no `worktree_branch`, so no `git fetch` and no checkout,
/// and is a tmux window and an agent launch; a restore recreates worktrees and
/// relaunches; a restart kills a window and puts one back. What the floor
/// cannot cover on its own is the part the *user* sizes: every lifecycle hook
/// runs inside one of these operations, in file order, each bounded only by
/// its own `timeout_secs` — an `Option<u64>` with no cap, and the shipped
/// template shows `timeout_secs = 120`. A lock shorter than that budget lapses
/// while its holder is still waiting on a hook, and the next creator sees a
/// free name and an absent row.
///
/// Every hook in the file is counted rather than the events one operation will
/// reach. It is a strict over-estimate — no operation runs them all — and that
/// is the direction the two errors point in: too long costs a deferred heal,
/// too short costs the pair. It also cannot go stale, which per-event budgets
/// did: the restore's hold was sized on the *creation* hooks and a long
/// `session.pre_restore` outlived it.
fn ttl_ms() -> u64 {
    // Fully-qualified per the session_ops → agent path-only architecture rule.
    let hooks = crate::agent::hooks_config::load_or_seed();
    let budget: std::time::Duration = hooks.hooks.iter().map(|h| h.timeout()).sum();
    TTL_FLOOR_MS.saturating_add(budget.as_millis() as u64)
}

/// A held lock, given back when this value is dropped.
///
/// The release is structural rather than a call at each exit point, for the
/// reason `tests/support/tmux_server.rs` gives about tmux servers: an operation
/// that takes a lock runs through `?` and early returns, and a lock leaked by
/// the one path that did not reach its release holds the subject until it
/// expires.
pub struct Lock<'a> {
    db: &'a Database,
    subject: Subject,
    op: String,
    expires_at: u64,
}

impl Lock<'_> {
    pub(crate) fn expires_at(&self) -> u64 {
        self.expires_at
    }
}

impl Drop for Lock<'_> {
    fn drop(&mut self) {
        // Best-effort: a lease that cannot be deleted expires on its own, and
        // failing an operation that already happened over its bookkeeping
        // would be the worse answer.
        if let Err(e) = self.db.release_lease(&self.subject, self.expires_at) {
            tracing::warn!(
                "could not release the {} lock on {:?}: {e}",
                self.op,
                self.subject
            );
        }
    }
}

/// Lock `subject` for the length of operation `op` — or `None` when somebody
/// else holds it.
///
/// `None` means "held", not "somebody is definitely working on it": a holder
/// killed mid-operation leaves its lease behind until it expires. The row is
/// deleted when the lock is dropped and overwritten by the next lock on that
/// subject otherwise, so a leaked one is bounded by the subjects ever locked
/// rather than accumulating.
pub fn lock<'a>(
    db: &'a Database,
    subject: Subject,
    op: &str,
) -> Result<Option<Lock<'a>>, crate::session_ops::OpError> {
    lock_for(db, subject, op, std::time::Duration::ZERO)
}

/// [`lock`], held for at least `at_least` — for an operation whose own length
/// the caller knows to exceed the usual budget, such as a fire whose
/// automation allows it an hour.
pub fn lock_for<'a>(
    db: &'a Database,
    subject: Subject,
    op: &str,
    at_least: std::time::Duration,
) -> Result<Option<Lock<'a>>, crate::session_ops::OpError> {
    let now = current_time_millis();
    let ttl = ttl_ms().max(at_least.as_millis() as u64);
    let expires_at = now.saturating_add(ttl);
    let won = db
        .take_lease(&subject, expires_at, now)
        .map_err(|e| format!("lock {subject:?} for {op}: {e}"))?;
    Ok(won.then(|| Lock {
        db,
        subject,
        op: op.to_string(),
        expires_at,
    }))
}

/// Conversation aliases can name one workspace across distinct session rows.
pub(crate) fn workspace<'a>(
    db: &'a Database,
    backend_type: &str,
    id: Option<&str>,
    op: &str,
) -> Result<Option<Lock<'a>>, crate::session_ops::OpError> {
    let Some(name) = id
        .map(crate::paths::sanitize_workspace_segment)
        .filter(|id| !id.is_empty())
    else {
        return Ok(None);
    };
    lock(
        db,
        Subject::Name {
            scope: format!("workspace:{}", super::machine_key(backend_type)),
            name,
        },
        op,
    )?
    .map(Some)
    .ok_or_else(|| {
        crate::session_ops::OpError::Conflict(
            "another lifecycle operation holds this workspace".into(),
        )
    })
}

/// Local aliases name one checkout. A remote path belongs to its host's
/// filesystem and must never be resolved against the observer's directories.
pub(crate) fn worktree_key(backend_type: &str, path: &std::path::Path) -> std::path::PathBuf {
    if crate::session::Route::is_remote_key(backend_type) {
        return path.to_path_buf();
    }
    if let Ok(canonical) = std::fs::canonicalize(path) {
        return canonical;
    }
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir().unwrap_or_default().join(path)
    };
    let mut normalized = std::path::PathBuf::new();
    for part in absolute.components() {
        match part {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                normalized.pop();
            }
            part => normalized.push(part.as_os_str()),
        }
    }
    // Planned checkouts do not exist yet; resolve their existing ancestor.
    let mut ancestor = normalized.clone();
    let mut suffix = Vec::new();
    loop {
        if let Ok(mut canonical) = std::fs::canonicalize(&ancestor) {
            for part in suffix.into_iter().rev() {
                canonical.push(part);
            }
            return canonical;
        }
        let Some(name) = ancestor.file_name() else {
            return normalized;
        };
        suffix.push(name.to_os_string());
        if !ancestor.pop() {
            return normalized;
        }
    }
}

/// Serialize checkout cleanup with every directory borrower, including children.
pub(crate) fn worktrees<'a>(
    db: &'a Database,
    backend_type: &str,
    paths: impl IntoIterator<Item = std::path::PathBuf>,
    op: &str,
) -> Result<Vec<Lock<'a>>, crate::session_ops::OpError> {
    let paths: std::collections::BTreeSet<_> = paths
        .into_iter()
        .map(|path| worktree_key(backend_type, &path))
        .collect();
    let mut roots: Vec<std::path::PathBuf> = Vec::new();
    let mut held = Vec::new();
    let scope = format!("worktree:{}", super::machine_key(backend_type));
    let now = current_time_millis();
    let expires_at = now.saturating_add(ttl_ms());
    for path in paths {
        // One operation may borrow both a directory and its child.
        if roots.iter().any(|root| path.starts_with(root)) {
            continue;
        }
        if !db
            .take_directory_lease(&scope, &path, expires_at, now)
            .map_err(|e| format!("lock directory for {op}: {e}"))?
        {
            return Err(crate::session_ops::OpError::Conflict(
                "another lifecycle operation holds this worktree".into(),
            ));
        }
        held.push(Lock {
            db,
            subject: Subject::Name {
                scope: scope.clone(),
                name: path.display().to_string(),
            },
            op: op.into(),
            expires_at,
        });
        roots.push(path);
    }
    Ok(held)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::SessionId;

    fn build() -> Subject {
        Subject::Name {
            scope: "local:tmux".into(),
            name: "build".into(),
        }
    }

    /// A hook's `timeout_secs` has no cap, so a lock sized on the floor alone
    /// can lapse while its holder is still waiting on one — and the name is then
    /// taken over while the operation that holds it has written no row. Every
    /// configured hook counts, not the events one operation reaches: sizing the
    /// restore's hold on the *creation* hooks is exactly how a long
    /// `session.pre_restore` outlived it.
    #[test]
    fn a_lock_outlasts_the_hooks_the_operation_will_wait_on() {
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

        let db = Database::open_in_memory().expect("db");
        let held = lock(&db, build(), "restore")
            .expect("lock")
            .expect("nothing holds it");
        assert!(
            held.expires_at.saturating_sub(current_time_millis()) > TTL_FLOOR_MS,
            "the hook's 900 s has to be inside the lock, not beyond it"
        );
    }

    #[test]
    fn a_lock_is_given_back_when_it_is_dropped() {
        let db = Database::open_in_memory().expect("db");
        let held = lock(&db, build(), "create")
            .expect("lock")
            .expect("the first holder wins");
        assert!(
            lock(&db, build(), "heal").expect("lock").is_none(),
            "nobody else decides this subject while it is held, whatever they are doing"
        );
        drop(held);
        assert!(lock(&db, build(), "heal").expect("lock").is_some());
    }

    #[test]
    fn a_run_lock_turns_a_second_fire_of_that_automation_away() {
        let db = Database::open_in_memory().expect("db");
        let run = lock(&db, Subject::AutomationRun(7), "run")
            .expect("lock")
            .expect("free");
        assert!(lock(&db, Subject::AutomationRun(7), "tick")
            .expect("lock")
            .is_none());
        assert!(lock(&db, Subject::AutomationRun(8), "tick")
            .expect("lock")
            .is_some());
        drop(run);
        assert!(lock(&db, Subject::AutomationRun(7), "tick")
            .expect("lock")
            .is_some());
    }

    #[test]
    fn a_name_and_a_row_are_different_subjects() {
        let db = Database::open_in_memory().expect("db");
        let id = SessionId::default();
        let _row = lock(&db, Subject::Session(id), "restart")
            .expect("lock")
            .expect("free");
        let name = Subject::Name {
            scope: "local:tmux".into(),
            name: id.to_string(),
        };
        assert!(lock(&db, name, "create").expect("lock").is_some());
    }

    #[test]
    fn an_expired_lock_is_taken_over_rather_than_waited_on() {
        let db = Database::open_in_memory().expect("db");
        let now = current_time_millis();
        assert!(db.take_lease(&build(), now - 1, now).expect("take"));
        assert!(
            lock(&db, build(), "heal").expect("lock").is_some(),
            "a holder that died mid-operation must not hold the subject forever"
        );
    }
}
