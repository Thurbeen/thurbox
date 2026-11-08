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

use thurbox::cli::sessions::{run, Action};
use thurbox::kernel::snapshot::SnapshotStore;
use thurbox::session::background_work::WorkEdge;
use thurbox::session::SessionId;
use thurbox::session::SharedSession;
use thurbox::storage::Database;

#[path = "support/recording_backend.rs"]
mod recording_backend;

const PANE: &str = "%7";

/// What the backend serving the rows reports as: the qualified route. The
/// rows themselves are stored as a legacy `ssh:devbox`, which settles to it.
const SERVED_BY: &str = "ssh:devbox:tmux";

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
        key: None,
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
    let store = SnapshotStore::with_database(
        database(&home),
        &thurbox::backend::wiring::configured(&Default::default()).0,
        &Default::default(),
    );
    (home, store, row.id)
}

fn status_of(store: &SnapshotStore, id: SessionId) -> String {
    store
        .current()
        .sessions
        .iter()
        .find(|row| row.id == id.to_string())
        .map(|row| row.status.as_str().to_string())
        .unwrap_or_else(|| "<missing>".to_string())
}

#[test]
fn a_remote_agents_report_reaches_its_session() {
    let (_home, mut store, id) = store_with_remote_session();
    // Not `idle`: nothing has reported for this row, and claude is wired to
    // report — so the honest word before the first hook is `unreported`.
    assert_eq!(status_of(&store, id), "unreported");

    let applied = store.apply_hook_states(
        vec![(
            SERVED_BY.to_string(),
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
    assert_eq!(status_of(&store, id), "unreported");
}

#[test]
fn a_state_nobody_defines_is_ignored() {
    // The value is free text written on a machine we do not control.
    let (_home, mut store, id) = store_with_remote_session();
    let applied = store.apply_hook_states(
        vec![(
            SERVED_BY.to_string(),
            PANE.to_string(),
            "'; DROP TABLE sessions; --".to_string(),
        )],
        Instant::now(),
    );
    assert_eq!(applied, 0);
    assert_eq!(status_of(&store, id), "unreported");
}

#[test]
fn re_reporting_the_same_state_writes_nothing() {
    // The subscription re-reports everything when it reconnects. Writing an
    // unchanged state would re-stamp its timestamp and, for a `done`, resurrect
    // a turn the user already acknowledged.
    let (_home, mut store, _id) = store_with_remote_session();
    let event = || {
        vec![(
            SERVED_BY.to_string(),
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
    let mut store = SnapshotStore::with_database(
        database(&home),
        &thurbox::backend::wiring::configured(&Default::default()).0,
        &Default::default(),
    );

    let early = Instant::now();
    let applied = store.apply_hook_states(
        vec![(
            SERVED_BY.to_string(),
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
    let mut store = SnapshotStore::with_database(
        database(&home),
        &thurbox::backend::wiring::configured(&Default::default()).0,
        &Default::default(),
    );

    let early = Instant::now();
    store.apply_hook_states(
        vec![(
            SERVED_BY.to_string(),
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
    assert_eq!(status_of(&store, row.id), "unreported");
}

// One hook report, two ways in: this subscription and `thurbox-cli session
// signal`. Both are the same report about the same row, so both must leave it
// in the same state — and the interface must show the state storage kept, not
// the word it was handed. Each case starts two identical databases and
// delivers the report through one path each.

/// What the row holds before the report arrives.
#[derive(Clone, Copy)]
enum Before {
    Fresh,
    /// `working`, with one piece of sub-work still running.
    WorkInFlight,
    Reported(&'static str),
    Parked,
}

fn prepared(before: Before) -> (tempfile::TempDir, SessionId) {
    let home = tempfile::tempdir().expect("tempdir");
    let db = database(&home);
    let row = session("ssh:devbox");
    db.upsert_session(&row).expect("upsert");
    match before {
        Before::Fresh => {}
        Before::WorkInFlight => {
            db.set_hook_state(row.id, "working").expect("working");
            db.signal_work_edge(row.id, WorkEdge::Start, "w1")
                .expect("work start");
        }
        Before::Reported(state) => {
            db.set_hook_state(row.id, state).expect("state");
        }
        Before::Parked => {
            db.set_hook_state(row.id, "working").expect("working");
            db.set_session_stopped(row.id, true).expect("park");
        }
    }
    (home, row.id)
}

/// This machine's route, served by a backend that runs nothing: neither path
/// needs a pane, only the row.
fn backends() -> thurbox::backend::BackendRegistry {
    let route =
        thurbox::session::Route::local(Some(thurbox::session::Multiplexer::platform_default()));
    let backend = recording_backend::RecordingBackend::new(&route);
    thurbox::backend::BackendRegistry::new(route, backend)
}

fn store_over(home: &tempfile::TempDir) -> SnapshotStore {
    SnapshotStore::with_database(database(home), &backends(), &Default::default())
}

fn shown(store: &SnapshotStore, id: SessionId) -> String {
    store
        .current()
        .session(&id.to_string())
        .map(|row| row.status.as_str().to_string())
        .unwrap_or_else(|| "<missing>".into())
}

fn stored(home: &tempfile::TempDir, id: SessionId) -> Option<String> {
    database(home)
        .load_hook_state(id)
        .expect("read")
        .and_then(|row| row.state)
}

/// What one path left behind: the stored word, and what the interface shows.
#[derive(Debug, PartialEq)]
struct After {
    stored: Option<String>,
    shown: String,
}

fn through_the_subscription(before: Before, state: &str) -> After {
    let (home, id) = prepared(before);
    let mut store = store_over(&home);
    store.apply_hook_states(
        vec![(SERVED_BY.into(), PANE.into(), state.into())],
        Instant::now(),
    );
    store.settle();
    // The delivering store must show what a store reading the row afresh does.
    assert_eq!(
        shown(&store, id),
        shown(&store_over(&home), id),
        "the interface shows a state storage did not keep ({state} after {})",
        name(before)
    );
    After {
        stored: stored(&home, id),
        shown: shown(&store, id),
    }
}

fn through_session_signal(before: Before, state: &str) -> After {
    let (home, id) = prepared(before);
    let result = run(
        Action::Signal {
            state: Some(state.into()),
            work_start: None,
            work_end: None,
            work: None,
            turn_start: false,
            stop_payload: None,
            idle: false,
            session: Some(id.to_string()),
        },
        &database(&home),
        &thurbox::cli::Backends::ready(backends()),
        &Default::default(),
    );
    // A report the row cannot take is refused out loud, each for its reason.
    match (before, thurbox::session_ops::signals::is_state(state)) {
        (_, false) => {
            let err = result.expect_err("a word that is no state is refused");
            assert!(err.contains("is not a state"), "{err}");
        }
        (Before::Parked, true) => {
            let err = result.expect_err("a parked session is refused");
            assert!(err.contains("parked"), "{err}");
        }
        _ => {
            result.unwrap_or_else(|e| panic!("{state} after {}: {e}", name(before)));
        }
    }
    After {
        stored: stored(&home, id),
        shown: shown(&store_over(&home), id),
    }
}

fn name(before: Before) -> &'static str {
    match before {
        Before::Fresh => "nothing",
        Before::WorkInFlight => "working with sub-work running",
        Before::Reported(state) => state,
        Before::Parked => "a park",
    }
}

#[test]
fn both_paths_store_the_same_state() {
    let cases: [(Before, &str, Option<&str>); 6] = [
        (Before::Fresh, "working", Some("working")),
        // A turn that ends with sub-work still running rests as `background`.
        (Before::WorkInFlight, "done", Some("background")),
        (Before::Reported("blocked"), "blocked", Some("blocked")),
        (Before::Reported("done"), "done", Some("done")),
        // A parked session has no process to be in any state.
        (Before::Parked, "working", None),
        // Free text from the pane is never a state.
        (Before::Fresh, "'; DROP TABLE sessions; --", None),
    ];
    for (before, state, expected) in cases {
        let subscription = through_the_subscription(before, state);
        let signal = through_session_signal(before, state);
        assert_eq!(
            subscription.stored.as_deref(),
            expected,
            "subscription: {state} after {}",
            name(before)
        );
        assert_eq!(
            subscription,
            signal,
            "{state} after {}: the subscription and session signal disagree",
            name(before)
        );
    }
}

#[test]
fn the_last_of_several_queued_reports_is_shown_as_stored() {
    // Two reports in one drain, both corrected in place before either is
    // answered. The first answer must not stand in for the second: with work
    // still running, the `done` that came last rests as `background`.
    let (home, id) = prepared(Before::WorkInFlight);
    // A report stamped ahead of this clock (another machine's), so storage
    // stamps the next one after it rather than at the store's own now: each
    // answer then names a stamp the store did not give.
    rusqlite::Connection::open(home.path().join("thurbox.db"))
        .expect("open")
        .execute(
            "UPDATE sessions SET hook_state_at = hook_state_at + 60000 WHERE id = ?1",
            [id.to_string()],
        )
        .expect("stamp ahead");
    let mut store = store_over(&home);
    store.apply_hook_states(
        vec![
            (SERVED_BY.into(), PANE.into(), "blocked".into()),
            (SERVED_BY.into(), PANE.into(), "done".into()),
        ],
        Instant::now(),
    );
    store.settle();
    assert_eq!(stored(&home, id).as_deref(), Some("background"));
    assert_eq!(shown(&store, id), shown(&store_over(&home), id));
}

#[test]
fn an_unchanged_report_through_the_subscription_keeps_its_stamp() {
    // The subscription re-reports on every reconnect; re-stamping an
    // acknowledged `done` would make it unseen again.
    let (home, id) = prepared(Before::Reported("done"));
    let at = database(&home)
        .load_hook_state(id)
        .unwrap()
        .unwrap()
        .state_at;
    let mut store = store_over(&home);
    store.apply_hook_states(
        vec![(SERVED_BY.into(), PANE.into(), "done".into())],
        Instant::now(),
    );
    store.settle();
    assert_eq!(
        database(&home)
            .load_hook_state(id)
            .unwrap()
            .unwrap()
            .state_at,
        at
    );
}
