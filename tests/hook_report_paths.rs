//! One hook report, two ways in: the interface's subscription to a backend's
//! status channel (`SnapshotStore::apply_hook_states`) and `thurbox-cli
//! session signal`. Both are the same report about the same row, so both must
//! leave the row in the same state — and the interface must show the state it
//! stored, not the word it was handed.
//!
//! Each case starts two identical databases, delivers the report through one
//! path each, and compares what storage holds afterwards, and what a store
//! reading that database from scratch shows against what the delivering store
//! shows.

use std::time::Instant;

use thurbox::cli::sessions::{run, Action};
use thurbox::kernel::snapshot::SnapshotStore;
use thurbox::session::background_work::WorkEdge;
use thurbox::session::SessionId;
use thurbox::storage::Database;
use thurbox::sync::SharedSession;

#[path = "support/recording_backend.rs"]
mod recording_backend;

const PANE: &str = "%7";
const STORED_AS: &str = "ssh:devbox";
/// The qualified route the backend serving [`STORED_AS`] reports under.
const SERVED_BY: &str = "ssh:devbox:tmux";

/// What the row holds before the report arrives.
#[derive(Clone, Copy)]
enum Before {
    Fresh,
    /// `working`, with one piece of sub-work still running.
    WorkInFlight,
    Reported(&'static str),
    Parked,
}

fn row() -> SharedSession {
    SharedSession {
        id: SessionId::default(),
        name: "worker".into(),
        agent: "claude".into(),
        backend_id: PANE.into(),
        backend_type: STORED_AS.into(),
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

fn open(home: &tempfile::TempDir) -> Database {
    Database::open(&home.path().join("thurbox.db")).expect("db")
}

fn prepared(before: Before) -> (tempfile::TempDir, SessionId) {
    let home = tempfile::tempdir().expect("tempdir");
    let db = open(&home);
    let row = row();
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

fn store(home: &tempfile::TempDir) -> SnapshotStore {
    SnapshotStore::with_database(open(home), &backends())
}

fn shown(store: &SnapshotStore, id: SessionId) -> String {
    store
        .current()
        .session(&id.to_string())
        .map(|row| row.status.as_str().to_string())
        .unwrap_or_else(|| "<missing>".into())
}

fn stored(home: &tempfile::TempDir, id: SessionId) -> Option<String> {
    open(home)
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
    let mut store = store(&home);
    store.apply_hook_states(
        vec![(SERVED_BY.into(), PANE.into(), state.into())],
        Instant::now(),
    );
    store.settle();
    // The delivering store must show what a store reading the row afresh does.
    assert_eq!(
        shown(&store, id),
        shown(&self::store(&home), id),
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
    let _ = run(
        Action::Signal {
            state: Some(state.into()),
            work_start: None,
            work_end: None,
            work: None,
            turn_start: false,
            session: Some(id.to_string()),
        },
        &open(&home),
        &thurbox::cli::Backends::ready(backends()),
    );
    After {
        stored: stored(&home, id),
        shown: shown(&store(&home), id),
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
fn an_unchanged_report_through_the_subscription_keeps_its_stamp() {
    // The subscription re-reports on every reconnect; re-stamping an
    // acknowledged `done` would make it unseen again.
    let (home, id) = prepared(Before::Reported("done"));
    let at = open(&home).load_hook_state(id).unwrap().unwrap().state_at;
    let mut store = store(&home);
    store.apply_hook_states(
        vec![(SERVED_BY.into(), PANE.into(), "done".into())],
        Instant::now(),
    );
    store.settle();
    assert_eq!(
        open(&home).load_hook_state(id).unwrap().unwrap().state_at,
        at
    );
}
