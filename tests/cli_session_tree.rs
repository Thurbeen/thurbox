//! `session list --tree` and `--parent <id> --recursive` (issue #1522): a
//! lead's whole subtree in one call, nested the way the sidebar nests it, and
//! never a dropped row — not for an orphan, not for a parent-id cycle.
//!
//! Driven through the real `thurbox-cli` against an isolated data dir.

use std::path::Path;
use std::process::Command;

use serde_json::Value;
use thurbox::session::SessionId;
use thurbox::storage::Database;
use thurbox::sync::SharedSession;

fn isolate(home: &Path) -> Database {
    thurbox::paths::set_test_dir(home);
    std::env::set_var(thurbox::paths::CONFIG_DIR_OVERRIDE_ENV, home);
    std::env::set_var(thurbox::paths::DATA_DIR_OVERRIDE_ENV, home);
    let path = thurbox::paths::database_file().expect("db path");
    std::fs::create_dir_all(path.parent().expect("data dir")).expect("mkdir");
    Database::open(&path).expect("open db")
}

fn seed(db: &Database, name: &str, parent: Option<SessionId>, order: i64) -> SessionId {
    let row = SharedSession {
        id: SessionId::default(),
        name: name.into(),
        agent: "claude".into(),
        backend_id: String::new(),
        backend_type: "local-tmux".into(),
        agent_session_id: None,
        cwd: None,
        additional_dirs: Vec::new(),
        worktrees: Vec::new(),
        shell_backend_id: None,
        parent_session_id: parent,
        display_order: Some(order),
        tombstone: false,
        tombstone_at: None,
        key: None,
    };
    db.upsert_session(&row).expect("seed a session row");
    row.id
}

fn list(args: &[&str]) -> String {
    let out = Command::new(env!("CARGO_BIN_EXE_thurbox-cli"))
        .args(["session", "list"])
        .args(args)
        .env_remove("THURBOX_SESSION")
        .env_remove("THURBOX_SOCKET")
        .env_remove("THURBOX_SOCKET_FOR")
        .output()
        .expect("run thurbox-cli");
    assert!(
        out.status.success(),
        "session list {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// `(name, depth)` per row, in output order.
fn rows(args: &[&str]) -> Vec<(String, u64)> {
    let mut args = args.to_vec();
    args.push("--json");
    let json: Value = serde_json::from_str(&list(&args)).expect("list --json");
    json.as_array()
        .expect("an array")
        .iter()
        .map(|row| {
            (
                row["name"].as_str().expect("name").to_string(),
                row["depth"].as_u64().expect("every row carries depth"),
            )
        })
        .collect()
}

fn named(rows: &[(&str, u64)]) -> Vec<(String, u64)> {
    rows.iter().map(|(n, d)| (n.to_string(), *d)).collect()
}

/// lead (0) ─ worker-b (2) ─ sub (4)
///          └ worker-a (3)
/// other (1); orphan (5) whose parent is deleted.
fn forest(db: &Database) -> SessionId {
    let lead = seed(db, "lead", None, 0);
    seed(db, "other", None, 1);
    let b = seed(db, "worker-b", Some(lead), 2);
    seed(db, "worker-a", Some(lead), 3);
    seed(db, "sub", Some(b), 4);
    let gone = seed(db, "gone", None, 6);
    seed(db, "orphan", Some(gone), 5);
    db.soft_delete_session(gone)
        .expect("delete the orphan's parent");
    lead
}

#[test]
fn tree_nests_children_under_parents_in_manual_order() {
    let home = tempfile::tempdir().expect("tempdir");
    let db = isolate(home.path());
    forest(&db);

    assert_eq!(
        rows(&["--tree"]),
        named(&[
            ("lead", 0),
            ("worker-b", 1),
            ("sub", 2),
            ("worker-a", 1),
            ("other", 0),
            ("orphan", 0),
        ])
    );
    let text = list(&["--tree", "--text"]);
    assert!(text.contains("└ worker-b"), "{text}");
    assert!(text.contains("  └ sub"), "{text}");
}

#[test]
fn the_flat_list_keeps_its_order_and_gains_depth() {
    let home = tempfile::tempdir().expect("tempdir");
    let db = isolate(home.path());
    forest(&db);

    assert_eq!(
        rows(&[]),
        named(&[
            ("lead", 0),
            ("other", 0),
            ("worker-b", 1),
            ("worker-a", 1),
            ("sub", 2),
            ("orphan", 0),
        ])
    );
}

#[test]
fn recursive_returns_every_descendant_and_parent_alone_stays_direct() {
    let home = tempfile::tempdir().expect("tempdir");
    let db = isolate(home.path());
    let lead = forest(&db).to_string();

    assert_eq!(
        rows(&["--parent", &lead]),
        named(&[("worker-b", 0), ("worker-a", 0)])
    );
    assert_eq!(
        rows(&["--parent", &lead, "--recursive"]),
        named(&[("worker-b", 0), ("worker-a", 0), ("sub", 1)])
    );
    assert_eq!(
        rows(&["--parent", &lead, "--recursive", "--tree"]),
        named(&[("worker-b", 0), ("sub", 1), ("worker-a", 0)])
    );
}

#[test]
fn a_parent_cycle_neither_hangs_nor_drops_a_row() {
    let home = tempfile::tempdir().expect("tempdir");
    let db = isolate(home.path());
    // Reparenting refuses a cycle, so one is written the way a mirrored row
    // could arrive: two upserts naming each other.
    let a = seed(&db, "a", None, 0);
    let b = seed(&db, "b", Some(a), 1);
    let mut row = db.get_session_by_id(a).expect("read").expect("row");
    row.parent_session_id = Some(b);
    db.upsert_session(&row).expect("close the cycle");

    let mut names: Vec<String> = rows(&["--tree"]).into_iter().map(|(n, _)| n).collect();
    names.sort();
    assert_eq!(names, ["a", "b"]);

    let listed = rows(&["--parent", &a.to_string(), "--recursive"]);
    assert_eq!(listed, named(&[("b", 0)]));
}
