//! Mirroring across more than one hop: A mirrors B, B mirrors C, and A may
//! also mirror C directly.
//!
//! Each instance is a real database, and every listing crosses as the JSON
//! `thurbox-cli session list [--deleted] --json` prints — the same bytes
//! `mirror::mirror_host` reads over ssh — so the only thing not real is the
//! ssh hop itself. A session keeps its id across every hop, which is what the
//! mirror dedupes on.

use std::path::PathBuf;

use serde_json::Value;
use thurbox::cli::sessions::{run, Action};
use thurbox::session::SessionId;
use thurbox::session_ops::mirror::{self, MirrorReport, Transitive};
use thurbox::storage::Database;
use thurbox::sync::{SharedSession, SharedWorktree};

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
fn mirror(observer: &Database, backend: &str, host: &Database) -> MirrorReport {
    mirror::reconcile(
        observer,
        backend,
        &listing(host, false),
        &listing(host, true),
    )
}

/// [`mirror`] with `[remote] transitive_sessions = false`.
fn mirror_hiding(observer: &Database, backend: &str, host: &Database) -> MirrorReport {
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
    mirror(&b, B_TO_C, &c);
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
    mirror(&a, A_TO_B, &b);
    mirror(&a, A_TO_C, &c);
    for _ in 0..2 {
        let through_b = mirror(&a, A_TO_B, &b);
        assert!(
            !through_b.adopted.contains(&on_c) && !through_b.updated.contains(&on_c),
            "the pass through B moved C's session: {through_b:?}"
        );
        mirror(&a, A_TO_C, &c);
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
        mirror(&a, A_TO_B, &b);
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
    mirror(&b, B_TO_A, &a);
    mirror(&a, A_TO_B, &b);
    assert_eq!(listed_on(&a, mine), vec![LOCAL]);
}

#[test]
fn a_session_deleted_on_its_direct_path_is_not_revived_through_another() {
    let Chain { a, b, c, on_c, .. } = chain();
    mirror(&a, A_TO_C, &c);
    // Deleted here; B has not mirrored C since, so it still lists it active.
    a.soft_delete_session(on_c).unwrap();
    let through_b = mirror(&a, A_TO_B, &b);
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
    let report = mirror_hiding(&a, A_TO_B, &b);
    assert_eq!(report.adopted, vec![on_b]);
    assert!(listed_on(&a, on_c).is_empty());
}

#[test]
fn hiding_forgets_what_was_taken_on_without_touching_it_and_showing_brings_it_back() {
    let Chain { a, b, c, on_c, .. } = chain();
    mirror(&a, A_TO_B, &b);
    assert_eq!(listed_on(&a, on_c), vec![A_TO_B]);

    let report = mirror_hiding(&a, A_TO_B, &b);
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
        mirror_hiding(&a, A_TO_B, &b).tombstoned.is_empty(),
        "and nothing is pushed to B on the next pass"
    );
    assert_eq!(listed_on(&b, on_c), vec![B_TO_C], "B's mirror is B's");
    assert_eq!(listed_on(&c, on_c), vec![LOCAL], "the session lives on");

    let report = mirror(&a, A_TO_B, &b);
    assert_eq!(report.adopted, vec![on_c]);
    assert_eq!(listed_on(&a, on_c), vec![A_TO_B]);
}

#[test]
fn hiding_leaves_a_session_reached_directly_where_it_is() {
    let Chain { a, b, c, on_c, .. } = chain();
    mirror(&a, A_TO_C, &c);
    let report = mirror_hiding(&a, A_TO_B, &b);
    assert!(report.forgotten.is_empty(), "{report:?}");
    assert_eq!(listed_on(&a, on_c), vec![A_TO_C]);
}
