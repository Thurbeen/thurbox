//! Shared sessions through the public surface: the host-facing CLI verbs a
//! peer drives (`list --deleted`, `register`, `sync`), the one JSON shape both
//! directions use, the mirror's reconciliation rules over a real database, and
//! the `if_missing` relaunch the loop asks for. The ssh half is a scripted
//! runner in the unit tests beside `session_ops::host_cli`; here nothing is
//! faked, so the tests stop where a host would be needed.

use std::path::PathBuf;
use std::process::Command as ProcessCommand;

use serde_json::{json, Value};
use thurbox::cli::sessions::{run, Action};
use thurbox::kernel::command::{Args, Command};
use thurbox::session::{HostDef, SessionId};
use thurbox::session_ops::mirror::{self, HostDeletedRow, MirrorReport, Transitive};
use thurbox::storage::Database;
use thurbox::sync::{SharedSession, SharedWorktree};

const BACKEND: &str = "ssh:devbox";

/// `register` shells out to list windows on this machine's tmux server, so
/// the refusal it gives only exercises "no live window" where a `tmux`
/// binary exists to ask; on a runner without one (Windows CI) the call fails
/// before it gets that far, the same gate other tmux-backed tests use.
fn have_tmux() -> bool {
    ProcessCommand::new("tmux")
        .arg("-V")
        .output()
        .map(|out| out.status.success())
        .unwrap_or(false)
}

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
            verify: false,
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
            verify: false,
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
    let printed = mirror::session_to_json(&s, Some("done"), Some("main"), Some(1));
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
    if !have_tmux() {
        eprintln!("skipping: tmux is not installed");
        return;
    }
    let db = Database::open_in_memory().unwrap();
    let id = SessionId::default();
    let body = mirror::session_to_json(&row(id, "elsewhere", "local-tmux"), None, None, None);
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
    let same_id = mirror::session_to_json(&row(id, "other", "local-tmux"), None, None, None);
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
    // A low `updated_at` for the adopt: the restore leg below needs a *later*
    // reading of the host's own clock to outrank the local tombstone (schema
    // v45 orders a restore against the host's last-known value, not against
    // this machine's `deleted_at` — see `mirror::apply`).
    let host_rows = vec![mirror::session_from_json(
        &mirror::session_to_json(
            &row(theirs, "theirs", "local-tmux"),
            Some("working"),
            None,
            Some(1),
        ),
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

    // The host restores it; so does the peer. A restore only outranks the
    // tombstone when the host's own clock reads past its last-known value, so
    // this leg needs a fresh, later `updated_at`.
    let host_rows = vec![mirror::session_from_json(
        &mirror::session_to_json(
            &row(theirs, "theirs", "local-tmux"),
            Some("working"),
            None,
            Some(2),
        ),
        BACKEND,
    )
    .unwrap()];
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

// Mirroring across more than one hop: A mirrors B, B mirrors C, and A may also
// mirror C directly. Each instance is a real database, and every listing
// crosses as the JSON `session list [--deleted] --json` prints - the bytes
// `mirror::mirror_host` reads over ssh - so only the ssh hop is not real. A
// session keeps its id across every hop, which is what the mirror dedupes on.

const LOCAL: &str = "local-tmux";
/// Each instance's name for the others. Made up, and deliberately not the
/// same spelling everywhere: B's name for C need not be A's.
const A_TO_B: &str = "ssh:bravo";
const A_TO_C: &str = "ssh:charlie";
const B_TO_C: &str = "ssh:c-from-bravo";
const B_TO_A: &str = "ssh:alpha";

fn own(db: &Database, name: &str) -> SessionId {
    let id = SessionId::default();
    db.upsert_session(&SharedSession {
        id,
        name: name.into(),
        agent: "claude".into(),
        backend_id: "%1".into(),
        backend_type: LOCAL.into(),
        agent_session_id: None,
        cwd: Some(PathBuf::from("/srv/repo")),
        additional_dirs: Vec::new(),
        worktrees: vec![SharedWorktree {
            repo_path: PathBuf::from("/srv/repo"),
            worktree_path: PathBuf::from(format!("/srv/worktrees/{name}")),
            branch: name.into(),
            created_by_thurbox: true,
        }],
        shell_backend_id: None,
        parent_session_id: None,
        display_order: None,
        tombstone: false,
        tombstone_at: None,
    })
    .unwrap();
    id
}

/// What `session list --json` and `session list --deleted --json` print.
fn listing(db: &Database, deleted: bool) -> Value {
    run(
        Action::List {
            parent: None,
            deleted,
            verify: false,
        },
        db,
    )
    .unwrap()
    .json
}

/// One mirror pass of `host` into `observer`, on the observer's name for it.
fn pass(observer: &Database, backend: &str, host: &Database) -> MirrorReport {
    mirror::reconcile_with(
        observer,
        backend,
        &listing(host, false),
        &listing(host, true),
        Transitive::Show,
    )
}

/// [`pass`] with `[remote] transitive_sessions = false`.
fn pass_hiding(observer: &Database, backend: &str, host: &Database) -> MirrorReport {
    mirror::reconcile_with(
        observer,
        backend,
        &listing(host, false),
        &listing(host, true),
        Transitive::Hide,
    )
}

/// Where the observer's `session list --json` puts `id`: one backend per
/// appearance, so a session listed twice shows up as two entries.
fn listed_on(observer: &Database, id: SessionId) -> Vec<String> {
    listing(observer, false)
        .as_array()
        .unwrap()
        .iter()
        .filter(|row| row["id"] == id.to_string())
        .map(|row| row["backend_type"].as_str().unwrap().to_string())
        .collect()
}

/// B holds its own session and a mirror of C's; C holds its own.
struct Chain {
    a: Database,
    b: Database,
    c: Database,
    on_b: SessionId,
    on_c: SessionId,
}

fn chain() -> Chain {
    let (a, b, c) = (
        Database::open_in_memory().unwrap(),
        Database::open_in_memory().unwrap(),
        Database::open_in_memory().unwrap(),
    );
    let on_b = own(&b, "bravo-own");
    let on_c = own(&c, "charlie-own");
    pass(&b, B_TO_C, &c);
    assert_eq!(listed_on(&b, on_c), vec![B_TO_C], "B mirrors C");
    Chain {
        a,
        b,
        c,
        on_b,
        on_c,
    }
}

#[test]
fn a_session_reached_directly_and_through_a_host_is_listed_once_on_its_owner() {
    let Chain {
        a,
        b,
        c,
        on_b,
        on_c,
        ..
    } = chain();

    // Whichever order the passes run in, and however many of them, C's
    // session is C's: A reaches it directly, so that is the path it keeps.
    pass(&a, A_TO_B, &b);
    pass(&a, A_TO_C, &c);
    for _ in 0..2 {
        let through_b = pass(&a, A_TO_B, &b);
        assert!(
            !through_b.adopted.contains(&on_c) && !through_b.updated.contains(&on_c),
            "the pass through B moved C's session: {through_b:?}"
        );
        pass(&a, A_TO_C, &c);
        assert_eq!(listed_on(&a, on_c), vec![A_TO_C]);
    }
    assert_eq!(listed_on(&a, on_b), vec![A_TO_B], "B's own is still B's");
}

#[test]
fn a_session_only_reachable_through_a_host_is_listed_through_it() {
    let Chain {
        a, b, on_b, on_c, ..
    } = chain();
    for _ in 0..2 {
        pass(&a, A_TO_B, &b);
        assert_eq!(listed_on(&a, on_b), vec![A_TO_B]);
        assert_eq!(listed_on(&a, on_c), vec![A_TO_B]);

        // B's own row names B's pane and B's checkout, and both are B's.
        let bs = a.get_session_by_id(on_b).unwrap().unwrap();
        assert_eq!(bs.backend_id, "%1");
        assert!(bs.worktrees[0].created_by_thurbox);
        // C's does not: `%1` there is a pane on C's server, and on B's it is
        // B's own agent — attaching to it would type into the wrong session.
        // Its checkout is C's, so nothing run on B may remove it.
        let cs = a.get_session_by_id(on_c).unwrap().unwrap();
        assert_eq!(cs.backend_id, "", "the second pass keeps it clear too");
        assert!(!cs.worktrees[0].created_by_thurbox);
        assert_eq!(cs.worktrees[0].branch, "charlie-own", "still described");
    }
}

#[test]
fn a_host_that_mirrors_us_back_never_relabels_our_own_session() {
    let (a, b) = (
        Database::open_in_memory().unwrap(),
        Database::open_in_memory().unwrap(),
    );
    let mine = own(&a, "alpha-own");
    pass(&b, B_TO_A, &a);
    pass(&a, A_TO_B, &b);
    assert_eq!(listed_on(&a, mine), vec![LOCAL]);
}

#[test]
fn a_session_deleted_on_its_direct_path_is_not_revived_through_another() {
    let Chain { a, b, c, on_c, .. } = chain();
    pass(&a, A_TO_C, &c);
    // Deleted here; B has not mirrored C since, so it still lists it active.
    a.soft_delete_session(on_c).unwrap();
    let through_b = pass(&a, A_TO_B, &b);
    assert!(listed_on(&a, on_c).is_empty(), "{through_b:?}");
    assert!(
        through_b.tombstoned.is_empty(),
        "nothing to push to B: the delete is C's to hear, on C's path"
    );
}

#[test]
fn hiding_transitive_sessions_lists_only_each_hosts_own() {
    let Chain {
        a, b, on_b, on_c, ..
    } = chain();
    let report = pass_hiding(&a, A_TO_B, &b);
    assert_eq!(report.adopted, vec![on_b]);
    assert!(listed_on(&a, on_c).is_empty());
}

#[test]
fn hiding_forgets_what_was_taken_on_without_touching_it_and_showing_brings_it_back() {
    let Chain { a, b, c, on_c, .. } = chain();
    pass(&a, A_TO_B, &b);
    assert_eq!(listed_on(&a, on_c), vec![A_TO_B]);

    let report = pass_hiding(&a, A_TO_B, &b);
    assert_eq!(report.forgotten, vec![on_c]);
    assert!(report.tombstoned.is_empty() && report.unknown_local.is_empty());
    assert!(listed_on(&a, on_c).is_empty());
    let deleted_here = listing(&a, true);
    assert_eq!(
        deleted_here.as_array().map(Vec::len),
        Some(0),
        "forgotten, not deleted: {deleted_here}"
    );
    assert!(
        pass_hiding(&a, A_TO_B, &b).tombstoned.is_empty(),
        "and nothing is pushed to B on the next pass"
    );
    assert_eq!(listed_on(&b, on_c), vec![B_TO_C], "B's mirror is B's");
    assert_eq!(listed_on(&c, on_c), vec![LOCAL], "the session lives on");

    let report = pass(&a, A_TO_B, &b);
    assert_eq!(report.adopted, vec![on_c]);
    assert_eq!(listed_on(&a, on_c), vec![A_TO_B]);
}

#[test]
fn hiding_leaves_a_session_reached_directly_where_it_is() {
    let Chain { a, b, c, on_c, .. } = chain();
    pass(&a, A_TO_C, &c);
    let report = pass_hiding(&a, A_TO_B, &b);
    assert!(report.forgotten.is_empty(), "{report:?}");
    assert_eq!(listed_on(&a, on_c), vec![A_TO_C]);
}
