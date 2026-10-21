//! Session keys (issue #1408) through the pipelines that give a row one —
//! create, `set-key`, restore — on this machine, against the in-memory backend.
//! Every one of them goes through `names::claim_key`, so none can make a twin.

use std::path::Path;

use super::spawn::SpawnRequest;
use crate::session::SessionId;
use crate::storage::Database;

struct Rig {
    _temp: tempfile::TempDir,
    _guard: crate::paths::TestPathGuard,
    backends: crate::backend::BackendRegistry,
    db: Database,
    repo: std::path::PathBuf,
}

fn rig() -> Rig {
    let temp = tempfile::TempDir::new().unwrap();
    let guard = crate::paths::TestPathGuard::new(temp.path());
    let config = crate::paths::config_file()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf();
    std::fs::create_dir_all(&config).unwrap();
    std::fs::write(
        config.join("agents.toml"),
        "default = \"shell\"\n\n[[agents]]\nname = \"shell\"\ncommand = \"sh\"\nargs = []\n",
    )
    .unwrap();
    let repo = temp.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    Rig {
        _temp: temp,
        _guard: guard,
        backends: crate::backend::registry::inert(),
        db: Database::open_in_memory().unwrap(),
        repo,
    }
}

fn request(repo: &Path, name: &str, key: Option<&str>) -> SpawnRequest {
    SpawnRequest {
        name: name.into(),
        repo_path: repo.to_path_buf(),
        agent: Some("shell".into()),
        key: key.map(str::to_string),
        ..Default::default()
    }
}

fn create(
    rig: &Rig,
    name: &str,
    key: Option<&str>,
) -> Result<SessionId, crate::session_ops::OpError> {
    super::spawn_session_headless(&rig.db, &rig.backends, request(&rig.repo, name, key))
        .map(|res| res.session_id)
}

fn key_of(db: &Database, id: SessionId) -> Option<String> {
    db.get_session_by_id(id).unwrap().unwrap().key
}

#[test]
fn a_create_with_a_taken_key_is_refused_before_anything_is_spawned() {
    let rig = rig();
    let lead = create(&rig, "lead", Some("fleet.lead")).unwrap();
    assert_eq!(key_of(&rig.db, lead).as_deref(), Some("fleet.lead"));

    let err = create(&rig, "another", Some("fleet.lead")).unwrap_err();
    assert!(err.contains("fleet.lead") && err.contains("lead"), "{err}");
    assert_eq!(rig.db.list_active_sessions().unwrap().len(), 1);

    // A name is still no identity: a second session of that name is allowed
    // (the default `--on-existing allow`) — under its own key.
    create(&rig, "lead", Some("fleet.lead.2")).unwrap();
    assert!(create(&rig, "x", Some("bad/key")).is_err(), "validated");
}

#[test]
fn a_key_is_in_the_panes_environment() {
    let rig = rig();
    let id = create(&rig, "lead", Some("fleet.lead")).unwrap();
    let env = super::session_process_env(&rig.db, &rig.db.get_session_by_id(id).unwrap().unwrap())
        .unwrap();
    assert_eq!(
        env.get("THURBOX_SESSION_KEY").map(String::as_str),
        Some("fleet.lead")
    );
}

#[test]
fn a_fork_does_not_copy_the_key() {
    let rig = rig();
    let lead = create(&rig, "lead", Some("fleet.lead")).unwrap();
    let fork = super::fork_session_headless(&rig.db, &rig.backends, lead, "").unwrap();
    assert_eq!(fork.key, None);
    assert_eq!(key_of(&rig.db, fork.session_id), None);
}

#[test]
fn set_key_gives_changes_and_clears_a_key_but_never_makes_a_twin() {
    let rig = rig();
    let a = create(&rig, "a", None).unwrap();
    let b = create(&rig, "b", Some("fleet.b")).unwrap();

    let report = super::set_key::set_session_key_headless(&rig.db, a, Some("fleet.a")).unwrap();
    assert!(report.changed && report.previous.is_none());
    assert_eq!(key_of(&rig.db, a).as_deref(), Some("fleet.a"));
    assert!(
        !super::set_key::set_session_key_headless(&rig.db, a, Some("fleet.a"))
            .unwrap()
            .changed
    );

    let err = super::set_key::set_session_key_headless(&rig.db, a, Some("fleet.b")).unwrap_err();
    assert!(err.contains("fleet.b"), "{err}");
    assert_eq!(key_of(&rig.db, a).as_deref(), Some("fleet.a"), "unchanged");

    super::set_key::set_session_key_headless(&rig.db, b, None).unwrap();
    assert_eq!(key_of(&rig.db, b), None);
    super::set_key::set_session_key_headless(&rig.db, a, Some("fleet.b")).unwrap();
    assert_eq!(key_of(&rig.db, a).as_deref(), Some("fleet.b"));
}

/// The undo-after-self-heal case. A keyed session is deleted; self-heal gives
/// its key to a new session at once (a deleted row does not reserve it); the
/// delete is then undone. The original comes back — without the key, and says
/// so — and the key still names exactly one session.
#[test]
fn an_undo_after_the_key_was_taken_restores_the_session_without_it() {
    let rig = rig();
    let original = create(&rig, "mission-control", Some("fleet.lead")).unwrap();
    super::delete_session_headless(&rig.db, &rig.backends, original, false).unwrap();
    let healed = create(&rig, "mission-control-2", Some("fleet.lead")).unwrap();

    let report = super::restore_session_headless(&rig.db, &rig.backends, original, true).unwrap();

    let why = report
        .key_dropped
        .expect("the restore says it dropped the key");
    assert!(why.contains("fleet.lead"), "{why}");
    assert_eq!(key_of(&rig.db, original), None);
    assert_eq!(key_of(&rig.db, healed).as_deref(), Some("fleet.lead"));
    assert_eq!(rig.db.find_sessions_by_key("fleet.lead").unwrap().len(), 1);
}

#[test]
fn an_undo_with_the_key_still_free_gives_it_back() {
    let rig = rig();
    let original = create(&rig, "lead", Some("fleet.lead")).unwrap();
    super::delete_session_headless(&rig.db, &rig.backends, original, false).unwrap();

    let report = super::restore_session_headless(&rig.db, &rig.backends, original, true).unwrap();

    assert_eq!(report.key_dropped, None);
    assert_eq!(key_of(&rig.db, original).as_deref(), Some("fleet.lead"));
}
