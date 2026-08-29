//! What makes a session's dot say `working`, and what makes it stop.
//!
//! The regression these guard: the fallback that rescues a stuck `working`
//! state was keyed on the age of the hook row rather than on terminal output,
//! so every turn reported itself finished ten seconds in and started again at
//! the agent's next hook — a spinner that stopped early and restarted while the
//! agent was still visibly printing. Hooks only mark the edges of a turn; only
//! the terminal says whether one is still running.
//!
//! Driven through `SnapshotStore` rather than the pure fold, because the part
//! that can be wrong is that the pass runs every tick and reverses itself: a
//! session that goes quiet and then prints again has to be `working` once more
//! without waiting for a hook that is never coming.

use thurbox::kernel::snapshot::SnapshotStore;
use thurbox::session::SessionId;
use thurbox::storage::Database;
use thurbox::sync::SharedSession;

/// Longer than the fallback's bound, whatever it is set to.
const QUIET: u64 = 60_000;

/// Every session is printing right now.
fn printing(_id: &str) -> Option<u64> {
    Some(0)
}

/// Every session has a live pane that has said nothing for a long time.
fn silent(_id: &str) -> Option<u64> {
    Some(QUIET)
}

/// No session has a live pane at all.
fn detached(_id: &str) -> Option<u64> {
    None
}

fn store_with_working_session() -> (tempfile::TempDir, SnapshotStore, String) {
    let home = tempfile::tempdir().expect("tempdir");
    let path = home.path().join("thurbox.db");
    let row = SharedSession {
        id: SessionId::default(),
        name: "long-turn".into(),
        agent: "claude".into(),
        backend_id: "%3".into(),
        backend_type: "local-tmux".into(),
        agent_session_id: Some("sid".into()),
        cwd: Some(std::path::PathBuf::from("/srv/repo")),
        additional_dirs: Vec::new(),
        worktrees: Vec::new(),
        shell_backend_id: None,
        parent_session_id: None,
        display_order: None,
        tombstone: false,
        tombstone_at: None,
    };
    let database = Database::open(&path).expect("db");
    database.upsert_session(&row).expect("upsert");
    database.set_hook_state(row.id, "working").expect("signal");
    let mut store = SnapshotStore::with_database(Database::open(&path).expect("db"));
    store.refresh();
    (home, store, row.id.to_string())
}

fn status_of(store: &SnapshotStore, id: &str) -> String {
    store
        .current()
        .sessions
        .iter()
        .find(|row| row.id == id)
        .map(|row| row.status.clone())
        .unwrap_or_else(|| "<missing>".to_string())
}

/// A hook fired long ago and never followed up, with the agent printing the
/// whole time. This is an ordinary turn that outlasts the fallback's bound.
#[test]
fn a_long_turn_keeps_working_while_its_agent_prints() {
    let (_home, mut store, id) = store_with_working_session();
    assert_eq!(status_of(&store, &id), "working");

    for _ in 0..3 {
        assert_eq!(store.apply_output_quiescence(printing), 0);
        assert_eq!(status_of(&store, &id), "working");
    }
}

/// The turn was interrupted, which fires no hook at all: the row still says
/// `working` and nothing is left to print.
#[test]
fn a_turn_that_went_quiet_is_reported_idle() {
    let (_home, mut store, id) = store_with_working_session();

    assert_eq!(store.apply_output_quiescence(silent), 1);
    assert_eq!(status_of(&store, &id), "idle");
    // Idempotent: the second pass has nothing left to change.
    assert_eq!(store.apply_output_quiescence(silent), 0);
}

/// A session absent from the map has no live pane, so nothing can be producing
/// output — where the old exited → `Idle` branch lands.
#[test]
fn a_working_session_with_no_live_pane_is_reported_idle() {
    let (_home, mut store, id) = store_with_working_session();
    assert_eq!(store.apply_output_quiescence(detached), 1);
    assert_eq!(status_of(&store, &id), "idle");
}

/// The half a one-way fallback gets wrong. An agent that goes quiet mid-turn
/// and then resumes fires no hook to say so, so the pass has to re-derive from
/// `hook_state` rather than adjust the status it wrote last time.
#[test]
fn a_session_that_prints_again_goes_back_to_working() {
    let (_home, mut store, id) = store_with_working_session();

    assert_eq!(store.apply_output_quiescence(silent), 1);
    assert_eq!(status_of(&store, &id), "idle");

    assert_eq!(store.apply_output_quiescence(printing), 1);
    assert_eq!(status_of(&store, &id), "working");
}

/// A session waiting on you produces no output while it waits, and saying so is
/// the whole point of the dot. Only `working` is time-gated.
#[test]
fn a_blocked_session_is_never_time_gated() {
    let (home, mut store, id) = store_with_working_session();
    let database = Database::open(&home.path().join("thurbox.db")).expect("db");
    let parsed: SessionId = id.parse().expect("id");
    database.set_hook_state(parsed, "blocked").expect("signal");
    store.refresh();
    assert_eq!(status_of(&store, &id), "blocked");

    assert_eq!(store.apply_output_quiescence(silent), 0);
    assert_eq!(status_of(&store, &id), "blocked");
}
