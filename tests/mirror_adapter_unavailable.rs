//! A host-reported mux route survives a temporarily unavailable adapter.
//! This binary owns its host-registry cache, including under ordinary cargo test.

use thurbox::session::SessionId;
use thurbox::storage::Database;

#[test]
fn mirror_keeps_an_active_sessions_recorded_suffix_when_adapter_is_unavailable() {
    let home = tempfile::tempdir().unwrap();
    thurbox::paths::set_test_dir(home.path());
    let config = thurbox::paths::config_file()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf();
    std::fs::create_dir_all(&config).unwrap();
    std::fs::write(
        config.join("hosts.toml"),
        "[[hosts]]\nname = 'example'\ndestination = 'unused'\n",
    )
    .unwrap();
    let id = SessionId::default();
    let db = Database::open_in_memory().unwrap();
    let host_row = thurbox::session_ops::mirror::session_from_json(
        &serde_json::json!({
            "id": id.to_string(), "name": "probe", "backend_type": "local-herdr",
            "backend_id": "opaque-pane"
        }),
        "ssh:example",
    )
    .unwrap();
    assert_eq!(host_row.session.backend_type, "ssh:example:herdr");
    let mut local = host_row.session.clone();
    local.backend_type = "ssh:example:herdr".into();
    db.upsert_session(&local).unwrap();

    thurbox::session_ops::mirror::apply(&db, "ssh:example", &[host_row], &[]);
    assert_eq!(
        db.get_session_by_id(id).unwrap().unwrap().backend_type,
        "ssh:example:herdr"
    );
}
