//! Remote agents reporting their status back to v2.
//!
//! A remote agent cannot call `thurbox-cli session signal`: there is no CLI on
//! the host, and running one there would write the host's own database. So its
//! hooks set a tmux pane option, which the local control-mode connection pushes
//! back — and the whole of remote status is whether anything drains that queue
//! into the columns a local signal writes. v2 armed the subscription and threw
//! the events away, so every remote session sat at whatever state it was last
//! left in, forever.
//!
//! These tests drive `SnapshotStore::apply_hook_states` directly, because the
//! part that can be wrong is the matching and the de-duplication, not the tmux
//! round trip: pane ids collide across hosts, the value is remote-controlled free
//! text, and the subscription re-reports everything on reconnect.

use std::time::{Duration, Instant};

use thurbox::kernel::snapshot::SnapshotStore;
use thurbox::session::SessionId;
use thurbox::storage::Database;
use thurbox::sync::SharedSession;

const PANE: &str = "%7";

fn session(backend_type: &str) -> SharedSession {
    SharedSession {
        id: SessionId::default(),
        name: "remote-work".into(),
        agent: "claude".into(),
        backend_id: PANE.into(),
        backend_type: backend_type.into(),
        agent_session_id: Some("sid".into()),
        cwd: Some(std::path::PathBuf::from("/srv/repo")),
        additional_dirs: Vec::new(),
        worktrees: Vec::new(),
        shell_backend_id: None,
        parent_session_id: None,
        display_order: None,
        tombstone: false,
        tombstone_at: None,
    }
}

/// A database on disk, so the test can hold a second handle and change what the
/// store will read on its next refresh — which is what an attach landing a pane
/// id looks like from here.
fn database(home: &tempfile::TempDir) -> Database {
    Database::open(&home.path().join("thurbox.db")).expect("db")
}

/// A store over a database holding one session on `ssh:devbox`.
fn store_with_remote_session() -> (tempfile::TempDir, SnapshotStore, SessionId) {
    let home = tempfile::tempdir().expect("tempdir");
    let row = session("ssh:devbox");
    database(&home).upsert_session(&row).expect("upsert");
    let store = SnapshotStore::with_database(database(&home));
    (home, store, row.id)
}

fn status_of(store: &SnapshotStore, id: SessionId) -> String {
    store
        .current()
        .sessions
        .iter()
        .find(|row| row.id == id.to_string())
        .map(|row| row.status.clone())
        .unwrap_or_else(|| "<missing>".to_string())
}

#[test]
fn a_remote_agents_report_reaches_its_session() {
    let (_home, mut store, id) = store_with_remote_session();
    assert_eq!(status_of(&store, id), "idle");

    let applied = store.apply_hook_states(
        vec![(
            "ssh:devbox".to_string(),
            PANE.to_string(),
            "working".to_string(),
        )],
        Instant::now(),
    );

    assert_eq!(applied, 1);
    assert_eq!(status_of(&store, id), "working");
}

#[test]
fn an_event_from_another_host_is_not_applied_here() {
    // Pane ids are per-server, so `%7` names a different pane on every host.
    // Matching on the pane alone would drive this session from someone else's.
    let (_home, mut store, id) = store_with_remote_session();
    let applied = store.apply_hook_states(
        vec![(
            "ssh:other".to_string(),
            PANE.to_string(),
            "blocked".to_string(),
        )],
        Instant::now(),
    );
    assert_eq!(applied, 0);
    assert_eq!(status_of(&store, id), "idle");
}

#[test]
fn a_state_nobody_defines_is_ignored() {
    // The value is free text written on a machine we do not control.
    let (_home, mut store, id) = store_with_remote_session();
    let applied = store.apply_hook_states(
        vec![(
            "ssh:devbox".to_string(),
            PANE.to_string(),
            "'; DROP TABLE sessions; --".to_string(),
        )],
        Instant::now(),
    );
    assert_eq!(applied, 0);
    assert_eq!(status_of(&store, id), "idle");
}

#[test]
fn re_reporting_the_same_state_writes_nothing() {
    // The subscription re-reports everything when it reconnects. Writing an
    // unchanged state would re-stamp its timestamp and, for a `done`, resurrect
    // a turn the user already acknowledged.
    let (_home, mut store, _id) = store_with_remote_session();
    let event = || {
        vec![(
            "ssh:devbox".to_string(),
            PANE.to_string(),
            "blocked".to_string(),
        )]
    };
    assert_eq!(store.apply_hook_states(event(), Instant::now()), 1);
    assert_eq!(store.apply_hook_states(event(), Instant::now()), 0);
}

#[test]
fn an_event_for_a_pane_nobody_claims_yet_is_kept_until_it_appears() {
    // The subscription's first report routinely lands before the pane has been
    // attached and the row carries its id. Dropping it there is how a session
    // ends up showing `idle` through an entire turn.
    let home = tempfile::tempdir().expect("tempdir");
    let mut row = session("ssh:devbox");
    row.backend_id = String::new();
    database(&home).upsert_session(&row).expect("upsert");
    let mut store = SnapshotStore::with_database(database(&home));

    let early = Instant::now();
    let applied = store.apply_hook_states(
        vec![(
            "ssh:devbox".to_string(),
            PANE.to_string(),
            "working".to_string(),
        )],
        early,
    );
    assert_eq!(applied, 0, "nothing to write it to yet");

    row.backend_id = PANE.into();
    database(&home).upsert_session(&row).expect("upsert");
    store.refresh();

    // No new events: the parked one is what has to land.
    let applied = store.apply_hook_states(Vec::new(), early + Duration::from_secs(1));
    assert_eq!(applied, 1);
    assert_eq!(status_of(&store, row.id), "working");
}

#[test]
fn a_parked_event_is_eventually_given_up_on() {
    // Another instance's panes would otherwise accumulate here for the life of
    // the process.
    let home = tempfile::tempdir().expect("tempdir");
    let mut row = session("ssh:devbox");
    row.backend_id = String::new();
    database(&home).upsert_session(&row).expect("upsert");
    let mut store = SnapshotStore::with_database(database(&home));

    let early = Instant::now();
    store.apply_hook_states(
        vec![(
            "ssh:devbox".to_string(),
            PANE.to_string(),
            "working".to_string(),
        )],
        early,
    );

    // A drain long past the retry window lets it go.
    store.apply_hook_states(Vec::new(), early + Duration::from_secs(600));

    // So the pane appearing afterwards finds nothing waiting for it.
    row.backend_id = PANE.into();
    database(&home).upsert_session(&row).expect("upsert");
    store.refresh();
    let applied = store.apply_hook_states(Vec::new(), early + Duration::from_secs(601));
    assert_eq!(applied, 0);
    assert_eq!(status_of(&store, row.id), "idle");
}
