//! Nondelegated remote teardown must preserve a deletion renewed during lookup.

use std::sync::Arc;

use thurbox::backend::{BackendRegistry, WindowRole};
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
    temp: tempfile::TempDir,
}

impl Env {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let guard = thurbox::paths::TestPathGuard::new(temp.path());
        let hosts = thurbox::agent::host_config::hosts_config_path().unwrap();
        std::fs::create_dir_all(hosts.parent().unwrap()).unwrap();
        std::fs::write(
            hosts,
            "[[hosts]]\nname = \"purge-peer\"\ndestination = \"unused\"\n\
             multiplexer = \"tmux\"\nshare_sessions = false\n",
        )
        .unwrap();
        let route = Route::parse("ssh:purge-peer:tmux").unwrap();
        let backend = RecordingBackend::new(&route);
        let local = Route::local(Some(Multiplexer::platform_default()));
        let mut registry = BackendRegistry::new(local.clone(), RecordingBackend::new(&local));
        registry.register(route.clone(), backend.clone());
        let db = Database::open(&temp.path().join("purge.db")).unwrap();
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
            db,
            backend,
            registry,
            id,
            _guard: guard,
            temp,
        }
    }

    fn renew_during_lookup(&self) {
        let path = self.temp.path().join("purge.db");
        let id = self.id;
        self.backend.on_next_locate(move || {
            let peer = Database::open(&path).unwrap();
            peer.restore_session(id).unwrap();
            peer.soft_delete_session(id).unwrap();
        });
    }

    fn assert_fresh_delete_kept(&self) {
        assert!(
            self.backend.called("locate"),
            "remote fallback must be exercised"
        );
        let row = self.db.get_deleted_session_by_id(self.id).unwrap().unwrap();
        assert!(row.deleted_at > 1, "the second deletion must be fresh");
        assert!(
            !self.backend.called("kill"),
            "remote lookup must not kill a fresh undo window"
        );
        assert_eq!(self.backend.windows_of(&self.id.to_string()).len(), 2);
    }
}

#[test]
fn purge_preserves_a_remote_delete_renewed_during_fallback_lookup() {
    let env = Env::new();
    env.renew_during_lookup();
    let report = purge_deleted(&env.db, &env.registry, Age::OlderThanDays(30), false).unwrap();
    assert!(report.purged.is_empty(), "{report:?}");
    assert_eq!(report.kept.len(), 1, "{report:?}");
    env.assert_fresh_delete_kept();
}

#[test]
fn timed_reap_preserves_a_remote_delete_renewed_during_fallback_lookup() {
    let env = Env::new();
    env.renew_during_lookup();
    let reaped = thurbox::session_ops::reap_overdue_soft_deletes(&env.db, &env.registry);
    assert!(reaped.is_empty(), "{reaped:?}");
    env.assert_fresh_delete_kept();
}

#[test]
fn purge_removes_remote_owned_windows_before_forgetting_an_unchanged_delete() {
    let env = Env::new();
    let report = purge_deleted(&env.db, &env.registry, Age::OlderThanDays(30), false).unwrap();
    assert_eq!(report.purged.len(), 1, "{report:?}");
    assert!(
        env.backend.called("locate"),
        "remote fallback must be exercised"
    );
    assert!(env.backend.windows_of(&env.id.to_string()).is_empty());
    assert!(env.db.get_deleted_session_by_id(env.id).unwrap().is_none());
}
