//! A tombstone on a persisted mux route survives while its adapter is absent.
//! This binary has its own host-registry cache under ordinary cargo test.

use thurbox::session::SessionId;
use thurbox::session_ops::mirror::{apply, HostRow};
use thurbox::storage::Database;
use thurbox::sync::SharedSession;

#[test]
fn mirror_preserves_a_deleted_session_on_a_suffixed_route() {
    let home = tempfile::tempdir().unwrap();
    thurbox::paths::set_test_dir(home.path());
    let id = SessionId::default();
    let host_row = HostRow {
        session: SharedSession {
            id,
            name: "probe".into(),
            agent: "probe".into(),
            backend_id: "opaque-pane".into(),
            backend_type: "ssh:example:herdr".into(),
            agent_session_id: None,
            cwd: None,
            additional_dirs: Vec::new(),
            worktrees: Vec::new(),
            shell_backend_id: None,
            parent_session_id: None,
            display_order: None,
            tombstone: false,
            tombstone_at: None,
        },
        hook_state: None,
        base_branch: None,
        updated_at: None,
        transitive: false,
    };
    let db = Database::open_in_memory().unwrap();
    db.upsert_session(&host_row.session).unwrap();
    db.soft_delete_session(id).unwrap();

    let report = apply(&db, "ssh:example", &[host_row], &[]);
    assert_eq!(report.tombstoned, vec![id]);
    assert!(db.get_session_by_id(id).unwrap().is_none());
}
