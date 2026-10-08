//! `session reparent` — moving a worker under another lead without SQL.
//!
//! After a lead migration, the workers' `parent_session_id` still named the old
//! lead, and the only way to move them was an `UPDATE` against the database by
//! hand. These pin the verb that replaces it, against a real database file.

use thurbox::cli::sessions::{run, Action};
use thurbox::session::SessionId;
use thurbox::storage::Database;
use thurbox::sync::SharedSession;

fn db() -> (tempfile::TempDir, Database) {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let db = Database::open(&dir.path().join("thurbox.db")).expect("open the database");
    (dir, db)
}

fn seed(db: &Database, name: &str, parent: Option<SessionId>) -> SessionId {
    let row = SharedSession {
        id: SessionId::default(),
        name: name.into(),
        agent: "claude".into(),
        backend_id: "%1".into(),
        backend_type: "local-tmux".into(),
        agent_session_id: None,
        cwd: None,
        additional_dirs: Vec::new(),
        worktrees: Vec::new(),
        shell_backend_id: None,
        parent_session_id: parent,
        display_order: None,
        tombstone: false,
        tombstone_at: None,
    };
    db.upsert_session(&row).expect("seed a session row");
    row.id
}

fn parent_of(db: &Database, id: SessionId) -> Option<SessionId> {
    db.get_session_by_id(id)
        .expect("read the row")
        .expect("the row exists")
        .parent_session_id
}

fn reparent(db: &Database, session: &str, parent: Option<&str>) -> Result<(), String> {
    run(
        Action::Reparent {
            session: session.into(),
            parent: parent.map(str::to_string),
            clear: parent.is_none(),
        },
        db,
        &thurbox::cli::Backends::ready(thurbox::backend::wiring::configured().0),
    )
    .map(|_| ())
    .map_err(|e| e.message.clone())
}

#[test]
fn a_worker_moves_from_a_deleted_lead_to_the_new_one() {
    let (_dir, db) = db();
    let old_lead = seed(&db, "old-lead", None);
    let new_lead = seed(&db, "new-lead", None);
    let worker = seed(&db, "worker", Some(old_lead));
    db.soft_delete_session(old_lead)
        .expect("retire the old lead");

    reparent(&db, "worker", Some("new-lead")).expect("reparent by name");

    assert_eq!(parent_of(&db, worker), Some(new_lead));
}

#[test]
fn clear_makes_a_session_top_level() {
    let (_dir, db) = db();
    let lead = seed(&db, "lead", None);
    let worker = seed(&db, "worker", Some(lead));

    reparent(&db, &worker.to_string(), None).expect("clear the parent");

    assert_eq!(parent_of(&db, worker), None);
}

#[test]
fn a_parent_that_does_not_exist_is_refused_and_nothing_moves() {
    let (_dir, db) = db();
    let lead = seed(&db, "lead", None);
    let worker = seed(&db, "worker", Some(lead));

    let err = reparent(&db, "worker", Some("nobody")).expect_err("no such parent");

    assert!(
        err.contains("nobody"),
        "the error names the reference: {err}"
    );
    assert_eq!(parent_of(&db, worker), Some(lead));
}

#[test]
fn a_cycle_is_refused() {
    let (_dir, db) = db();
    let lead = seed(&db, "lead", None);
    let worker = seed(&db, "worker", Some(lead));
    let grandchild = seed(&db, "grandchild", Some(worker));

    let own = reparent(&db, "lead", Some("lead")).expect_err("its own parent");
    assert!(own.contains("own parent"), "{own}");
    let under = reparent(&db, "lead", Some("grandchild")).expect_err("under its descendant");
    assert!(under.contains("descendant"), "{under}");

    assert_eq!(parent_of(&db, lead), None);
    assert_eq!(parent_of(&db, grandchild), Some(worker));
}
