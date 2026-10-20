//! Purging deleted sessions: dropping the row for good, so the restore list,
//! the reaper's sweep and every mirroring peer stop carrying it.
//!
//! A deleted row is not always inert. A soft-deleted one may still own its
//! agent's windows until the reaper takes them, and one whose teardown never
//! reached its host is the owed-teardown sweep's worklist. A purge leaves both
//! kinds alone rather than forgetting what they still hold: it releases a
//! soft row's windows the way the reaper would, and keeps any row whose
//! windows it could not look at, or whose teardown is owed.
//!
//! Worktrees stay on disk. They are what made a soft delete undoable, may hold
//! uncommitted work, and removing a directory is not what dropping a row asks
//! for — the report names them instead.

use std::path::PathBuf;

use crate::session::SessionId;
use crate::storage::{Database, DeletedSessionInfo};

const MS_PER_DAY: u64 = 24 * 60 * 60 * 1000;

/// Which deleted rows a purge is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Age {
    /// Every deleted row, however recent.
    All,
    /// Rows deleted at least this many days ago.
    OlderThanDays(u64),
}

/// A row a purge dropped — or, on a dry run, would drop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Purged {
    pub id: SessionId,
    pub name: String,
    pub backend_type: String,
    pub deleted_at: u64,
    /// Worktrees thurbox checked out for it that a soft delete left in place.
    /// On the host the row lived on, for a remote one.
    pub left_on_disk: Vec<PathBuf>,
}

/// A row a purge was asked for and kept, with why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Kept {
    pub id: SessionId,
    pub name: String,
    pub reason: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PurgeReport {
    pub purged: Vec<Purged>,
    pub kept: Vec<Kept>,
}

/// Purge the deleted rows `age` selects. A `dry_run` reaps and drops nothing,
/// but still lists each server's windows — which is how it can say which rows
/// would be kept, and which stamps a remote server's listing backoff as the
/// sweep's own listing does.
pub fn purge_deleted(
    db: &Database,
    backends: &crate::backend::BackendRegistry,
    age: Age,
    dry_run: bool,
) -> Result<PurgeReport, String> {
    let now = crate::sync::current_time_millis();
    let selected = |row: &DeletedSessionInfo| match age {
        Age::All => true,
        Age::OlderThanDays(days) => {
            now.saturating_sub(row.deleted_at) >= days.saturating_mul(MS_PER_DAY)
        }
    };
    let rows = db
        .list_deleted_sessions()
        .map_err(|e| format!("list_deleted_sessions: {e}"))?;
    let mut windows: std::collections::HashMap<
        String,
        Option<crate::backend::identity::WindowIndex>,
    > = std::collections::HashMap::new();
    let mut report = PurgeReport::default();
    for row in rows.into_iter().filter(selected) {
        let kept = |reason: &str| Kept {
            id: row.id,
            name: row.name.clone(),
            reason: reason.to_string(),
        };
        if row.teardown_owed {
            report
                .kept
                .push(kept("its teardown is still owed on its host"));
            continue;
        }
        if !row.force_deleted {
            if now.saturating_sub(row.deleted_at) < super::UNDO_WINDOW.as_millis() as u64 {
                report.kept.push(kept("it is inside its undo window"));
                continue;
            }
            let index = windows.entry(row.backend_type.clone()).or_insert_with(|| {
                super::delete::listed_windows_on(db, backends, &row.backend_type)
            });
            let Some(index) = index else {
                report
                    .kept
                    .push(kept("its server's windows could not be listed"));
                continue;
            };
            let owns = !super::delete::owned_windows_in(
                index,
                &row,
                super::delete::has_live_namesake(db, &row),
            )
            .is_empty();
            if owns && !dry_run {
                if let Err(e) = super::reap_soft_deleted(db, backends, row.id) {
                    report
                        .kept
                        .push(kept(&format!("its windows did not come down: {e}")));
                    continue;
                }
            }
        }
        if !dry_run {
            match db.purge_deleted_session(row.id) {
                Ok(true) => {}
                Ok(false) => {
                    report.kept.push(kept(
                        "it was restored, or began owing a teardown, meanwhile",
                    ));
                    continue;
                }
                Err(e) => {
                    report
                        .kept
                        .push(kept(&format!("the database refused: {e}")));
                    continue;
                }
            }
        }
        report.purged.push(Purged {
            left_on_disk: if row.force_deleted {
                Vec::new()
            } else {
                row.worktrees
                    .iter()
                    .filter(|w| w.created_by_thurbox)
                    .map(|w| w.worktree_path.clone())
                    .collect()
            },
            id: row.id,
            name: row.name,
            backend_type: row.backend_type,
            deleted_at: row.deleted_at,
        });
    }
    Ok(report)
}

/// The `deleted_retention_days` sweep: purge what has been deleted for longer
/// than the setting allows. Off at `0`, and one indexed probe on a pass with
/// nothing that old, so it can ride the reaper's five-second cadence.
pub fn purge_expired(db: &Database, backends: &crate::backend::BackendRegistry) -> PurgeReport {
    let days = crate::session::settings::global().deleted_retention_days;
    if days == 0 {
        return PurgeReport::default();
    }
    let cutoff = crate::sync::current_time_millis().saturating_sub(days.saturating_mul(MS_PER_DAY));
    if !db.any_deleted_before(cutoff).unwrap_or(false) {
        return PurgeReport::default();
    }
    match purge_deleted(db, backends, Age::OlderThanDays(days), false) {
        Ok(report) => {
            if !report.purged.is_empty() {
                tracing::info!(
                    "purged {} session(s) deleted over {days} day(s) ago",
                    report.purged.len()
                );
            }
            report
        }
        Err(e) => {
            tracing::warn!("retention purge: {e}");
            PurgeReport::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync::{SharedSession, SharedWorktree};

    fn deleted(db: &Database, name: &str, backend_type: &str, days_ago: u64) -> SessionId {
        let row = SharedSession {
            id: SessionId::default(),
            name: name.into(),
            agent: "claude".into(),
            backend_id: "%1".into(),
            backend_type: backend_type.into(),
            agent_session_id: None,
            cwd: None,
            additional_dirs: Vec::new(),
            worktrees: vec![SharedWorktree {
                repo_path: "/srv/repo".into(),
                worktree_path: format!("/srv/repo/.worktrees/{name}").into(),
                branch: name.into(),
                created_by_thurbox: true,
            }],
            shell_backend_id: None,
            parent_session_id: None,
            display_order: None,
            tombstone: false,
            tombstone_at: None,
            key: None,
        };
        db.upsert_session(&row).unwrap();
        db.soft_delete_session(row.id).unwrap();
        let at = crate::sync::current_time_millis().saturating_sub(days_ago * MS_PER_DAY);
        db.conn_ref()
            .execute(
                "UPDATE sessions SET deleted_at = ?1 WHERE id = ?2",
                rusqlite::params![at as i64, row.id.to_string()],
            )
            .unwrap();
        row.id
    }

    /// Sorted: rows deleted in the same millisecond list in no fixed order.
    fn names(report: &PurgeReport) -> (Vec<&str>, Vec<&str>) {
        let mut purged: Vec<&str> = report.purged.iter().map(|p| p.name.as_str()).collect();
        let mut kept: Vec<&str> = report.kept.iter().map(|k| k.name.as_str()).collect();
        purged.sort_unstable();
        kept.sort_unstable();
        (purged, kept)
    }

    #[test]
    fn a_purge_drops_old_rows_and_keeps_what_still_holds_something() {
        let temp = tempfile::TempDir::new().unwrap();
        let _guard = crate::paths::TestPathGuard::new(temp.path());
        let db = Database::open_in_memory().unwrap();
        let backends = crate::backend::registry::inert();
        let old = deleted(&db, "old", "local-tmux", 40);
        let recent = deleted(&db, "recent", "local-tmux", 1);
        let owed = deleted(&db, "owed", "local-tmux", 40);
        db.mark_session_force_deleted(owed).unwrap();
        db.set_teardown_owed(owed, true).unwrap();
        // Its host serves nothing here, so its windows cannot be looked at.
        let unlisted = deleted(&db, "unlisted", "ssh:devbox", 40);

        let dry = purge_deleted(&db, &backends, Age::OlderThanDays(30), true).unwrap();
        assert_eq!(names(&dry), (vec!["old"], vec!["owed", "unlisted"]));
        assert!(
            db.get_deleted_session_by_id(old).unwrap().is_some(),
            "a dry run writes nothing"
        );

        let done = purge_deleted(&db, &backends, Age::OlderThanDays(30), false).unwrap();
        assert_eq!(names(&done), (vec!["old"], vec!["owed", "unlisted"]));
        assert_eq!(
            done.purged[0].left_on_disk,
            vec![PathBuf::from("/srv/repo/.worktrees/old")]
        );
        assert!(db.get_deleted_session_by_id(old).unwrap().is_none());
        for kept in [recent, owed, unlisted] {
            assert!(db.get_deleted_session_by_id(kept).unwrap().is_some());
        }
    }

    #[test]
    fn purging_everything_still_leaves_a_delete_inside_its_undo_window() {
        let temp = tempfile::TempDir::new().unwrap();
        let _guard = crate::paths::TestPathGuard::new(temp.path());
        let db = Database::open_in_memory().unwrap();
        let backends = crate::backend::registry::inert();
        deleted(&db, "yesterday", "local-tmux", 1);
        deleted(&db, "just-now", "local-tmux", 0);

        let report = purge_deleted(&db, &backends, Age::All, false).unwrap();
        assert_eq!(names(&report), (vec!["yesterday"], vec!["just-now"]));
    }

    #[test]
    fn a_row_restored_before_the_write_is_not_purged() {
        let db = Database::open_in_memory().unwrap();
        let id = deleted(&db, "back", "local-tmux", 40);
        db.restore_session(id).unwrap();
        assert!(!db.purge_deleted_session(id).unwrap());
        assert!(db.get_session_by_id(id).unwrap().is_some());
    }
}
