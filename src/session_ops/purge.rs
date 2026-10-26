//! Irreversible cleanup with a retained tombstone for mirroring peers.
//!
//! A failed cleanup keeps its durable worklist; only confirmed teardown can
//! commit `purged_at`. Opened user worktrees are never removed.

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

/// A completed purge, or its dry-run plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Purged {
    pub id: SessionId,
    pub name: String,
    pub backend_type: String,
    pub deleted_at: u64,
    /// Legacy report field, empty after cleanup. Opened user worktrees are retained.
    pub left_on_disk: Vec<PathBuf>,
    /// Branches the teardown could not delete. Best-effort, so the row is
    /// still purged, but the leftover branch is said rather than silent.
    pub branch_errors: Vec<String>,
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
) -> Result<PurgeReport, crate::session_ops::OpError> {
    purge_selected(db, backends, age, dry_run, None)
}

pub fn purge_selected(
    db: &Database,
    backends: &crate::backend::BackendRegistry,
    age: Age,
    dry_run: bool,
    selected_id: Option<SessionId>,
) -> Result<PurgeReport, crate::session_ops::OpError> {
    purge_if_generation(db, backends, age, dry_run, selected_id, None)
}

pub fn purge_if_generation(
    db: &Database,
    backends: &crate::backend::BackendRegistry,
    age: Age,
    dry_run: bool,
    selected_id: Option<SessionId>,
    expected: Option<u64>,
) -> Result<PurgeReport, super::OpError> {
    purge_selected_limited(db, backends, age, dry_run, selected_id, None, expected)
}

fn purge_selected_limited(
    db: &Database,
    backends: &crate::backend::BackendRegistry,
    age: Age,
    dry_run: bool,
    selected_id: Option<SessionId>,
    limit: Option<usize>,
    expected: Option<u64>,
) -> Result<PurgeReport, crate::session_ops::OpError> {
    if let Some(expected) = expected {
        let id = selected_id.ok_or("conditional purge requires one session")?;
        db.require_lifecycle_generation(id, Some(expected))
            .map_err(|_| super::OpError::Conflict("session lifecycle changed".into()))?;
    }
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
    let mut attempts = 0;
    let mut silent = std::collections::HashSet::new();
    for row in rows
        .into_iter()
        .filter(|row| selected_id.map_or(true, |id| id == row.id))
        .filter(selected)
    {
        let _lease = if !dry_run {
            match super::lock::lock(db, super::lock::Subject::Session(row.id), "purge")? {
                Some(lease) => Some(lease),
                None => continue,
            }
        } else {
            None
        };
        if db
            .require_lifecycle_generation(row.id, expected.or(Some(row.lifecycle_generation)))
            .is_err()
        {
            continue;
        }
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
        if !row.force_deleted
            && now.saturating_sub(row.deleted_at) < super::UNDO_WINDOW.as_millis() as u64
        {
            report.kept.push(kept("it is inside its undo window"));
            continue;
        }
        if let Some(super::Executor::HostCli { host, cli }) =
            super::Executor::for_backend(&row.backend_type)
        {
            if silent.contains(&host.name) {
                continue;
            }
            if !cli.purged_tombstones {
                report
                    .kept
                    .push(kept("update the host's CLI before purging its sessions"));
                continue;
            }
            let Some(generation) = row
                .host_lifecycle_generation
                .filter(|_| cli.lifecycle_preconditions)
            else {
                report.kept.push(kept(
                    "host lifecycle precondition is unknown; sync the host first",
                ));
                continue;
            };
            if let Some(limit) = limit {
                if attempts >= limit {
                    break;
                }
                if !super::delete::claim_reap(db, row.id, now) {
                    continue;
                }
                attempts += 1;
            }
            let generation = generation.to_string();
            let id = row.id.to_string();
            let days = match age {
                Age::All => None,
                Age::OlderThanDays(days) => Some(days.to_string()),
            };
            let mut args = vec!["session", "purge", &id, "--if-generation", &generation];
            if let Some(days) = days.as_deref() {
                args.extend(["--older-than-days", days]);
            }
            if dry_run {
                args.push("--dry-run");
            }
            match super::host_cli::run(&host, &cli, &args) {
                Ok(answer)
                    if answer
                        .get("purged")
                        .and_then(serde_json::Value::as_array)
                        .is_some_and(|rows| {
                            rows.iter().any(|r| {
                                r.get("id").and_then(serde_json::Value::as_str) == Some(id.as_str())
                            })
                        }) =>
                {
                    if !dry_run
                        && db
                            .require_lifecycle_generation(row.id, Some(row.lifecycle_generation))
                            .is_err()
                    {
                        report
                            .kept
                            .push(kept("its local deletion changed meanwhile"));
                        continue;
                    }
                    if !dry_run
                        && !db
                            .purge_deleted_session(row.id, row.deleted_at)
                            .map_err(|e| e.to_string())?
                    {
                        report
                            .kept
                            .push(kept("its local deletion changed meanwhile"));
                        continue;
                    }
                    report.purged.push(Purged {
                        id: row.id,
                        name: row.name,
                        backend_type: row.backend_type,
                        deleted_at: row.deleted_at,
                        left_on_disk: Vec::new(),
                        branch_errors: Vec::new(),
                    });
                }
                Ok(_) => report
                    .kept
                    .push(kept("the host retained it; its cleanup may still be owed")),
                Err(e) => {
                    if !dry_run {
                        super::delete::reap_failed(db, row.id, now);
                    }
                    silent.insert(host.name.clone());
                    report.kept.push(kept(&format!("host purge failed: {e}")));
                }
            }
            continue;
        }
        if super::resolve_host(&row.backend_type)
            .flatten()
            .is_some_and(|host| host.shareable())
        {
            report.kept.push(kept(
                "shared host CLI is unavailable; direct cleanup is unsafe",
            ));
            continue;
        }
        let index = windows
            .entry(row.backend_type.clone())
            .or_insert_with(|| super::delete::listed_windows_on(db, backends, &row.backend_type));
        let Some(index) = index else {
            report
                .kept
                .push(kept("its server's windows could not be listed"));
            continue;
        };
        if ambiguous_windows(index, &row) {
            report.kept.push(kept("its window ownership is ambiguous"));
            continue;
        }
        if let Some(limit) = limit {
            if attempts >= limit {
                break;
            }
            if !super::delete::claim_reap(db, row.id, now) {
                continue;
            }
            attempts += 1;
        }
        let owns = !super::delete::owned_windows_in(
            index,
            &row,
            super::delete::has_live_namesake(db, &row),
        )
        .is_empty();
        if !dry_run {
            let current = db
                .get_deleted_session_by_id(row.id)
                .map_err(|e| format!("get deleted session: {e}"))?;
            if !current.is_some_and(|current| {
                current.lifecycle_generation == row.lifecycle_generation
                    && current.force_deleted == row.force_deleted
                    && !current.teardown_owed
            }) {
                report.kept.push(kept("its deletion changed meanwhile"));
                continue;
            }
        }
        if owns && row.force_deleted {
            report.kept.push(kept("its windows are still present"));
            continue;
        }
        let mut branch_errors = Vec::new();
        if !dry_run {
            if !row.force_deleted {
                match super::delete::reap_selected_soft_delete(db, backends, &row) {
                    Ok(true) => {}
                    Ok(false) => {
                        report.kept.push(kept("its deletion changed meanwhile"));
                        continue;
                    }
                    Err(error) => {
                        report
                            .kept
                            .push(kept(&format!("its windows could not be reaped: {error}")));
                        continue;
                    }
                }
            }
            if !db
                .begin_session_purge_generation(
                    row.id,
                    row.deleted_at,
                    Some(row.lifecycle_generation),
                )
                .map_err(|e| e.to_string())?
            {
                report.kept.push(kept("its deletion changed meanwhile"));
                continue;
            }
            let mut teardown = super::ForceDeleteReport::default();
            super::delete::teardown_runtime_resources(
                db,
                backends,
                &super::delete::shared_deleted(&row),
                &mut teardown,
            );
            if teardown.teardown_error.is_some() || !teardown.worktree_errors.is_empty() {
                report
                    .kept
                    .push(kept("its teardown is still owed; cleanup will retry"));
                continue;
            }
            if let Err(reason) = confirm_teardown(db, backends, &row) {
                report.kept.push(kept(&reason));
                continue;
            }
            db.complete_session_teardown(row.id)
                .map_err(|e| e.to_string())?;
            branch_errors = teardown.branch_errors;
        }
        report.purged.push(Purged {
            left_on_disk: Vec::new(),
            branch_errors,
            id: row.id,
            name: row.name,
            backend_type: row.backend_type,
            deleted_at: row.deleted_at,
        });
    }
    Ok(report)
}

fn confirm_teardown(
    db: &Database,
    backends: &crate::backend::BackendRegistry,
    row: &DeletedSessionInfo,
) -> Result<(), String> {
    let _ = db;
    // Reaping is best effort. Permanent deletion needs a fresh
    // listing proving that no owned window remains.
    let confirmed = super::delete::listed_windows_on(db, backends, &row.backend_type)
        .ok_or_else(|| "its window teardown could not be confirmed".to_string())?;
    if ambiguous_windows(&confirmed, row) {
        return Err("its window ownership is ambiguous".to_string());
    }
    if !super::delete::owned_windows_in(&confirmed, row, super::delete::has_live_namesake(db, row))
        .is_empty()
    {
        return Err("its windows are still present".to_string());
    }
    Ok(())
}

fn ambiguous_windows(
    index: &crate::backend::identity::WindowIndex,
    row: &DeletedSessionInfo,
) -> bool {
    let id = row.id.to_string();
    [
        index.agent_window(&id, &row.name),
        index.shell_window(&id, &row.name),
    ]
    .into_iter()
    .any(|window| matches!(window, crate::backend::identity::Located::Unknown))
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
    match purge_selected_limited(
        db,
        backends,
        Age::OlderThanDays(days),
        false,
        None,
        Some(128),
        None,
    ) {
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
    use crate::sync::SharedSession;

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
            worktrees: Vec::new(),
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
        assert!(done.purged[0].left_on_disk.is_empty());
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
        let deleted_at = db
            .get_deleted_session_by_id(id)
            .unwrap()
            .unwrap()
            .deleted_at;
        db.restore_session(id).unwrap();
        assert!(!db.purge_deleted_session(id, deleted_at).unwrap());
        assert!(db.get_session_by_id(id).unwrap().is_some());
        db.soft_delete_session(id).unwrap();
        assert!(!db.purge_deleted_session(id, deleted_at).unwrap());
        assert!(db.get_deleted_session_by_id(id).unwrap().is_some());
    }
}

#[cfg(test)]
mod delegated_purge_tests {
    use super::*;
    use crate::session_ops::host_cli::{fake, Usable};
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    };

    #[test]
    fn a_delegated_purge_never_force_deletes_a_host_row_restored_after_selection() {
        let temp = tempfile::tempdir().unwrap();
        let _guard = crate::paths::TestPathGuard::new(temp.path());
        let hosts = crate::agent::host_config::hosts_config_path().unwrap();
        std::fs::create_dir_all(hosts.parent().unwrap()).unwrap();
        std::fs::write(
            hosts,
            "[[hosts]]\nname = \"peer\"\ndestination = \"unused\"\nshare_sessions = true\n",
        )
        .unwrap();
        let db = Database::open_in_memory().unwrap();
        let id = crate::session::SessionId::default();
        db.conn_ref().execute("INSERT INTO sessions(id, name, backend_type, created_at, updated_at, deleted_at) VALUES (?1, 'old', 'ssh:peer', 1, 1, 1)", [id.to_string()]).unwrap();
        let active = Arc::new(AtomicBool::new(true));
        let observed = active.clone();
        let mut cli = fake::cli();
        cli.purged_tombstones = true;
        cli.lifecycle_preconditions = true;
        db.set_host_lifecycle_generation(id, 7).unwrap();
        fake::force_usable(Usable::Yes(cli));
        fake::install_runner(Box::new(move |_, args| {
            assert!(args.windows(2).any(|pair| pair == ["--if-generation", "7"]));
            if args.iter().any(|arg| arg == "--force") {
                observed.store(false, Ordering::SeqCst);
                Ok(serde_json::json!({"purged": [{"id": id.to_string()}]}))
            } else {
                Err(fake::answered(
                    "active session requires an explicit force delete",
                ))
            }
        }));
        let result =
            purge_deleted(&db, &crate::backend::registry::inert(), Age::All, false).unwrap();
        fake::clear();
        assert!(
            active.load(Ordering::SeqCst),
            "delegated purge destroyed a restored host session"
        );
        assert!(result.purged.is_empty());
    }
    #[test]
    fn a_retention_purge_checks_the_hosts_own_deletion_age() {
        let temp = tempfile::tempdir().unwrap();
        let _guard = crate::paths::TestPathGuard::new(temp.path());
        let hosts = crate::agent::host_config::hosts_config_path().unwrap();
        std::fs::create_dir_all(hosts.parent().unwrap()).unwrap();
        std::fs::write(
            hosts,
            "[[hosts]]\nname = 'peer'\ndestination = 'unused'\nshare_sessions = true\n",
        )
        .unwrap();
        let db = Database::open_in_memory().unwrap();
        let id = crate::session::SessionId::default();
        db.conn_ref().execute("INSERT INTO sessions(id, name, backend_type, created_at, updated_at, deleted_at) VALUES (?1, 'old-here', 'ssh:peer', 1, 1, 1)", [id.to_string()]).unwrap();
        db.set_host_lifecycle_generation(id, 7).unwrap();
        let purged_on_host = Arc::new(AtomicBool::new(false));
        let observed = purged_on_host.clone();
        let mut cli = fake::cli();
        cli.purged_tombstones = true;
        cli.lifecycle_preconditions = true;
        fake::force_usable(Usable::Yes(cli));
        fake::install_runner(Box::new(move |_, args| {
            if args
                .windows(2)
                .any(|pair| pair == ["--older-than-days", "30"])
            {
                Ok(
                    serde_json::json!({"purged": [], "kept": [{"id": id.to_string(), "reason": "recent deletion on host"}]}),
                )
            } else {
                observed.store(true, Ordering::SeqCst);
                Ok(serde_json::json!({"purged": [{"id": id.to_string()}]}))
            }
        }));
        let report = purge_deleted(
            &db,
            &crate::backend::registry::inert(),
            Age::OlderThanDays(30),
            false,
        )
        .unwrap();
        fake::clear();
        assert!(
            !purged_on_host.load(Ordering::SeqCst),
            "retention purged a recent host deletion using the observers older timestamp"
        );
        assert!(report.purged.is_empty());
        assert!(db.get_deleted_session_by_id(id).unwrap().is_some());
    }
}
