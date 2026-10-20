//! Permanent deletion must retain the cleanup worklist until teardown succeeds.

use std::sync::Arc;

use thurbox::backend::{BackendRegistry, SessionBackend, WindowRole};
use thurbox::session::{Multiplexer, Route, SessionId};
use thurbox::session_ops::purge::{purge_deleted, Age};
use thurbox::storage::Database;
use thurbox::sync::SharedSession;

#[path = "support/recording_backend.rs"]
mod recording_backend;
use recording_backend::RecordingBackend;

struct Env {
    db: Database,
    backend: Arc<RecordingBackend>,
    registry: BackendRegistry,
    id: SessionId,
    _guard: thurbox::paths::TestPathGuard,
    _temp: tempfile::TempDir,
}

impl Env {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let guard = thurbox::paths::TestPathGuard::new(temp.path());
        let db = Database::open(&temp.path().join("purge.db")).unwrap();
        let route = Route::local(Some(Multiplexer::platform_default()));
        let backend = RecordingBackend::new(&route);
        let registry = BackendRegistry::new(route.clone(), backend.clone());
        let id = SessionId::default();
        let pane = backend.open("tb-old", &id.to_string(), WindowRole::Agent);
        backend.open("tbs-old", &id.to_string(), WindowRole::Shell);
        db.upsert_session(&SharedSession {
            id,
            name: "old".into(),
            agent: "codex".into(),
            backend_id: pane,
            backend_type: route.format(),
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
        })
        .unwrap();
        db.soft_delete_session(id).unwrap();
        db.conn_ref()
            .execute(
                "UPDATE sessions SET deleted_at = 1 WHERE id = ?1",
                [id.to_string()],
            )
            .unwrap();
        Self {
            _temp: temp,
            _guard: guard,
            db,
            backend,
            registry,
            id,
        }
    }

    fn assert_kept(&self) {
        let report =
            purge_deleted(&self.db, &self.registry, Age::OlderThanDays(30), false).unwrap();
        assert!(report.purged.is_empty(), "{report:?}");
        assert_eq!(report.kept.len(), 1, "{report:?}");
        assert!(self
            .db
            .get_deleted_session_by_id(self.id)
            .unwrap()
            .is_some());
    }
}

#[test]
fn purge_keeps_the_row_and_owned_windows_when_kill_is_refused() {
    let env = Env::new();
    env.backend.refuse_kill();
    env.assert_kept();
    assert!(env.backend.called("kill"));
    assert_eq!(env.backend.windows_of(&env.id.to_string()).len(), 2);
}

#[test]
fn purge_keeps_the_row_when_a_listing_fails_after_ownership_was_found() {
    let env = Env::new();
    env.backend.fail_discover_on(2);
    env.assert_kept();
}

#[test]
fn purge_preserves_a_fresh_delete_made_while_listing_windows() {
    let env = Env::new();
    let path = env._temp.path().join("purge.db");
    let id = env.id;
    env.backend.on_next_discover(move || {
        let peer = Database::open(&path).unwrap();
        peer.restore_session(id).unwrap();
        peer.soft_delete_session(id).unwrap();
    });
    env.assert_kept();
    assert!(
        !env.backend.called("kill"),
        "fresh undo window keeps its agent"
    );
    assert_eq!(env.backend.windows_of(&env.id.to_string()).len(), 2);
}

#[test]
fn purge_rechecks_the_selected_deletion_even_when_no_windows_remain() {
    let env = Env::new();
    for window in env.backend.windows() {
        env.backend.kill(&window.pane).unwrap();
    }
    let path = env._temp.path().join("purge.db");
    let id = env.id;
    env.backend.on_next_discover(move || {
        let peer = Database::open(&path).unwrap();
        peer.restore_session(id).unwrap();
        peer.soft_delete_session(id).unwrap();
    });
    env.assert_kept();
}

#[test]
fn purge_removes_both_owned_windows_before_forgetting_the_row() {
    let env = Env::new();
    let report = purge_deleted(&env.db, &env.registry, Age::OlderThanDays(30), false).unwrap();
    assert_eq!(report.purged.len(), 1, "{report:?}");
    assert!(env.backend.windows_of(&env.id.to_string()).is_empty());
    assert!(env.db.get_deleted_session_by_id(env.id).unwrap().is_none());
}

#[test]
fn purge_retains_a_force_delete_whose_window_kill_failed() {
    let env = Env::new();
    env.db.restore_session(env.id).unwrap();
    env.backend.refuse_kill();
    let deleted =
        thurbox::session_ops::delete_session_headless(&env.db, &env.registry, env.id, true)
            .unwrap();
    assert!(!deleted.killed_window);
    assert!(
        env.db
            .get_deleted_session_by_id(env.id)
            .unwrap()
            .unwrap()
            .force_deleted
    );
    let report = purge_deleted(&env.db, &env.registry, Age::All, false).unwrap();
    assert!(report.purged.is_empty(), "{report:?}");
    assert_eq!(report.kept.len(), 1, "{report:?}");
    assert!(env.db.get_deleted_session_by_id(env.id).unwrap().is_some());
    assert_eq!(env.backend.windows_of(&env.id.to_string()).len(), 2);
}

#[test]
fn purge_drops_a_force_delete_after_its_windows_are_gone() {
    let env = Env::new();
    env.db.restore_session(env.id).unwrap();
    let deleted =
        thurbox::session_ops::delete_session_headless(&env.db, &env.registry, env.id, true)
            .unwrap();
    assert!(deleted.killed_window);
    let report = purge_deleted(&env.db, &env.registry, Age::All, false).unwrap();
    assert_eq!(report.purged.len(), 1, "{report:?}");
    assert!(env.db.get_deleted_session_by_id(env.id).unwrap().is_none());
    assert!(env.backend.windows_of(&env.id.to_string()).is_empty());
}

#[test]
fn purge_keeps_ambiguous_owned_windows_on_the_worklist() {
    let env = Env::new();
    env.backend
        .open("tb-old-copy", &env.id.to_string(), WindowRole::Agent);
    let report = purge_deleted(&env.db, &env.registry, Age::OlderThanDays(30), false).unwrap();
    assert!(report.purged.is_empty(), "{report:?}");
    assert_eq!(report.kept.len(), 1, "{report:?}");
    assert!(env.db.get_deleted_session_by_id(env.id).unwrap().is_some());
    assert_eq!(env.backend.windows_of(&env.id.to_string()).len(), 3);
}
