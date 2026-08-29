//! Shared sessions through the public surface: the host-facing CLI verbs a
//! peer drives (`list --deleted`, `register`, `sync`), the one JSON shape both
//! directions use, the mirror's reconciliation rules over a real database, and
//! the `if_missing` relaunch the loop asks for. The ssh half is a scripted
//! runner in the unit tests beside `session_ops::host_cli`; here nothing is
//! faked, so the tests stop where a host would be needed.

use std::path::PathBuf;

use serde_json::json;
use thurbox::cli::sessions::{run, Action};
use thurbox::kernel::command::{Args, Command};
use thurbox::session::{HostDef, SessionId};
use thurbox::session_ops::mirror::{self, HostDeletedRow};
use thurbox::storage::Database;
use thurbox::sync::SharedSession;

const BACKEND: &str = "ssh:devbox";

fn row(id: SessionId, name: &str, backend: &str) -> SharedSession {
    SharedSession {
        id,
        name: name.into(),
        agent: "claude".into(),
        backend_id: "%3".into(),
        backend_type: backend.into(),
        agent_session_id: Some("conv".into()),
        cwd: Some(PathBuf::from("/srv/repo")),
        additional_dirs: Vec::new(),
        worktrees: Vec::new(),
        shell_backend_id: None,
        parent_session_id: None,
        display_order: None,
        tombstone: false,
        tombstone_at: None,
    }
}

#[test]
fn list_deleted_prints_what_a_mirroring_peer_reads() {
    let db = Database::open_in_memory().unwrap();
    let kept = SessionId::default();
    db.upsert_session(&row(kept, "kept", "local-tmux")).unwrap();
    let gone = SessionId::default();
    db.upsert_session(&row(gone, "gone", "local-tmux")).unwrap();
    db.soft_delete_session(gone).unwrap();
    db.mark_session_force_deleted(gone).unwrap();

    let out = run(
        Action::List {
            parent: None,
            deleted: true,
        },
        &db,
    )
    .unwrap();
    let rows = out.as_array().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["id"], gone.to_string());
    assert_eq!(rows[0]["force_deleted"], true);
    assert_eq!(rows[0]["backend_type"], "local-tmux");
    assert!(out.human.contains("in part"), "{}", out.human);

    // The active list carries the fields the mirror needs and nothing it
    // would have to guess: the pane, the extra dirs, the base branch.
    let active = run(
        Action::List {
            parent: None,
            deleted: false,
        },
        &db,
    )
    .unwrap();
    let only = &active.as_array().unwrap()[0];
    assert_eq!(only["id"], kept.to_string());
    assert_eq!(only["backend_id"], "%3");
    assert!(only["additional_dirs"].is_array());
    assert!(only.get("base_branch").is_some());
    assert!(only.get("hook_state").is_some());
}

#[test]
fn the_json_a_host_prints_is_the_json_a_peer_reads() {
    let id = SessionId::default();
    let mut s = row(id, "shape", "local-tmux");
    s.additional_dirs = vec![PathBuf::from("/srv/extra")];
    let printed = mirror::session_to_json(&s, Some("done"), Some("main"));
    let read = mirror::session_from_json(&printed, BACKEND).unwrap();
    assert_eq!(read.session.id, id);
    assert_eq!(
        read.session.backend_type, BACKEND,
        "the observer's name for the host"
    );
    assert_eq!(read.session.additional_dirs, s.additional_dirs);
    assert_eq!(read.hook_state.as_deref(), Some("done"));
    assert_eq!(read.base_branch.as_deref(), Some("main"));
}

#[test]
fn register_records_only_a_window_that_is_running() {
    // No tmux server is reachable from a test, so the one answer `register`
    // can give is the refusal — which is the property: it records, it never
    // launches, and it will not invent a row for a window that is not there.
    let db = Database::open_in_memory().unwrap();
    let id = SessionId::default();
    let body = mirror::session_to_json(&row(id, "elsewhere", "local-tmux"), None, None);
    let err = run(
        Action::Register {
            json_row: body.to_string(),
        },
        &db,
    )
    .unwrap_err();
    assert!(err.contains("no live window"), "{err}");
    assert!(db.get_session_by_id(id).unwrap().is_none());

    let err = run(
        Action::Register {
            json_row: "{not json".into(),
        },
        &db,
    )
    .unwrap_err();
    assert!(err.contains("--json-row"), "{err}");
}

#[test]
fn register_refuses_an_id_or_a_name_already_here() {
    let db = Database::open_in_memory().unwrap();
    let id = SessionId::default();
    db.upsert_session(&row(id, "taken", "local-tmux")).unwrap();
    let same_id = mirror::session_to_json(&row(id, "other", "local-tmux"), None, None);
    let err = run(
        Action::Register {
            json_row: same_id.to_string(),
        },
        &db,
    )
    .unwrap_err();
    assert!(err.contains("already registered"), "{err}");
    let same_name = mirror::session_to_json(
        &row(SessionId::default(), "taken", "local-tmux"),
        None,
        None,
    );
    let err = run(
        Action::Register {
            json_row: same_name.to_string(),
        },
        &db,
    )
    .unwrap_err();
    assert!(err.contains("already exists"), "{err}");
}

#[test]
fn sync_with_no_shareable_host_configured_is_an_empty_report() {
    let temp = tempfile::TempDir::new().unwrap();
    let _guard = thurbox::paths::TestPathGuard::new(temp.path());
    let db = Database::open_in_memory().unwrap();
    let out = run(
        Action::Sync {
            host: None,
            adopt: false,
        },
        &db,
    )
    .unwrap();
    assert_eq!(out.as_array().map(Vec::len), Some(0));
    assert!(out.human.contains("No shareable hosts"), "{}", out.human);
}

#[test]
fn the_mirror_reconciles_a_real_database_both_ways() {
    let db = Database::open_in_memory().unwrap();
    let theirs = SessionId::default();
    let host_rows = vec![mirror::session_from_json(
        &mirror::session_to_json(&row(theirs, "theirs", "local-tmux"), Some("working"), None),
        BACKEND,
    )
    .unwrap()];
    let report = mirror::apply(&db, BACKEND, &host_rows, &[]);
    assert_eq!(report.adopted, vec![theirs]);
    let adopted = db.get_session_by_id(theirs).unwrap().unwrap();
    assert_eq!(adopted.backend_type, BACKEND);

    // The host deletes it; the peer's row follows, recoverable in part.
    let report = mirror::apply(
        &db,
        BACKEND,
        &[],
        &[HostDeletedRow {
            id: theirs,
            force_deleted: true,
        }],
    );
    assert_eq!(report.deleted, vec![theirs]);
    assert!(
        db.get_deleted_session_by_id(theirs)
            .unwrap()
            .unwrap()
            .force_deleted
    );

    // The host restores it; so does the peer.
    let report = mirror::apply(&db, BACKEND, &host_rows, &[]);
    assert_eq!(report.restored, vec![theirs]);
    assert!(db.get_session_by_id(theirs).unwrap().is_some());

    // A row on another backend is never the mirror's business.
    let local = SessionId::default();
    db.upsert_session(&row(local, "mine", "local-tmux"))
        .unwrap();
    let report = mirror::apply(&db, BACKEND, &host_rows, &[]);
    assert!(!report.changed());
    assert!(report.unknown_local.is_empty());
}

#[test]
fn a_host_entry_shares_by_default_and_can_opt_out() {
    let on: HostDef = toml::from_str("name = \"devbox\"\ndestination = \"me@devbox\"").unwrap();
    assert!(on.shareable());
    let off: HostDef =
        toml::from_str("name = \"devbox\"\ndestination = \"me@devbox\"\nshare_sessions = false")
            .unwrap();
    assert!(!off.shareable());
}

#[test]
fn a_restart_asked_by_a_plugin_is_a_full_restart_never_a_relaunch() {
    // `if_missing` is the loop's word, for the agent-is-gone case; a plugin's
    // "restart" must keep meaning kill-and-relaunch.
    let parsed = Command::parse(
        "restart",
        Args {
            session: "s".into(),
            ..Args::default()
        },
    )
    .unwrap();
    assert!(matches!(
        parsed,
        Command::Restart {
            if_missing: false,
            ..
        }
    ));
    assert_eq!(parsed.session(), "s");
    let _ = json!({});
}
