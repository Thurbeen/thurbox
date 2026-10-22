//! `session delete` saying what it did (issue #1520): the help names what a
//! soft delete keeps, the JSON alone tells a caller whether and until when it
//! can undo, and a flagless `--json` delete — soft for legacy peers — says so
//! on stderr.
//!
//! Driven through the real `thurbox-cli` against an isolated data dir.

use std::path::Path;
use std::process::{Command, Output};

use serde_json::Value;
use thurbox::session::SessionId;
use thurbox::storage::Database;
use thurbox::sync::SharedSession;

/// The database a `thurbox-cli` pointed at `home` (see [`cli`]) uses. Nothing
/// process-wide is set, so tests sharing a process under `cargo test`'s
/// threads keep their own instance each.
fn isolate(home: &Path) -> Database {
    Database::open(&home.join("thurbox.db")).expect("open db")
}

fn seed(db: &Database, name: &str) -> SessionId {
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
        parent_session_id: None,
        display_order: None,
        tombstone: false,
        tombstone_at: None,
        key: None,
    };
    db.upsert_session(&row).expect("seed a session row");
    row.id
}

fn cli(home: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_thurbox-cli"))
        .args(args)
        .env(thurbox::paths::CONFIG_DIR_OVERRIDE_ENV, home)
        .env(thurbox::paths::DATA_DIR_OVERRIDE_ENV, home)
        .env_remove("THURBOX_SESSION")
        .env_remove("THURBOX_SOCKET")
        .env_remove("THURBOX_SOCKET_FOR")
        .output()
        .expect("run thurbox-cli")
}

fn delete(home: &Path, id: SessionId, flags: &[&str]) -> (Value, String) {
    let id = id.to_string();
    let mut args = vec!["session", "delete", id.as_str(), "--json"];
    args.extend_from_slice(flags);
    let out = cli(home, &args);
    assert!(
        out.status.success(),
        "delete failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    (
        serde_json::from_slice(&out.stdout).expect("delete --json"),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn the_help_says_a_soft_delete_reaps_its_windows() {
    let home = tempfile::tempdir().expect("tempdir");
    isolate(home.path());
    let out = cli(home.path(), &["session", "delete", "--help"]);
    let help = String::from_utf8_lossy(&out.stdout);
    let help = help.split_whitespace().collect::<Vec<_>>().join(" ");

    assert!(
        !help.contains("keep windows and worktrees"),
        "the windows are reaped after the undo window, not kept: {help}"
    );
    assert!(help.contains("windows become reapable"), "{help}");
    assert!(help.contains("session restore"), "{help}");
}

#[test]
fn a_soft_delete_reports_its_undo_window_and_the_row_it_left() {
    let home = tempfile::tempdir().expect("tempdir");
    let db = isolate(home.path());
    let id = seed(&db, "worker");

    let (report, stderr) = delete(home.path(), id, &["--soft"]);

    assert_eq!(report["soft"], true, "{report}");
    let row = &report["session"];
    assert_eq!(row["id"], id.to_string(), "{report}");
    assert_eq!(row["force_deleted"], false);
    let deleted_at = row["deleted_at"].as_u64().expect("deleted_at");
    let window = thurbox::session_ops::UNDO_WINDOW.as_millis() as u64;
    assert_eq!(report["reapable_at"].as_u64(), Some(deleted_at + window));
    // No retention configured: nothing purges the row, so restoring has no
    // deadline — the undo window ends the agent, not the restore.
    assert!(report["undo_until"].is_null(), "{report}");
    assert!(
        !stderr.contains("warning"),
        "--soft was asked for: {stderr}"
    );

    // The same shape `session list --deleted --json` prints for the row.
    let listed: Value = serde_json::from_slice(
        &cli(home.path(), &["session", "list", "--deleted", "--json"]).stdout,
    )
    .expect("list --deleted --json");
    assert_eq!(&listed[0], row);
}

#[test]
fn with_retention_a_soft_delete_says_when_it_stops_being_restorable() {
    let home = tempfile::tempdir().expect("tempdir");
    let db = isolate(home.path());
    std::fs::write(
        home.path().join("settings.toml"),
        "deleted_retention_days = 2\n",
    )
    .expect("write settings.toml");
    let id = seed(&db, "worker");

    let (report, _) = delete(home.path(), id, &["--soft"]);

    let deleted_at = report["session"]["deleted_at"]
        .as_u64()
        .expect("deleted_at");
    assert_eq!(
        report["undo_until"].as_u64(),
        Some(deleted_at + 2 * 24 * 60 * 60 * 1000),
        "{report}"
    );
}

#[test]
fn a_flagless_json_delete_is_soft_and_says_so_on_stderr_only() {
    let home = tempfile::tempdir().expect("tempdir");
    let db = isolate(home.path());
    let id = seed(&db, "worker");

    let (report, stderr) = delete(home.path(), id, &[]);

    assert_eq!(report["soft"], true, "{report}");
    assert_eq!(report["session"]["force_deleted"], false, "{report}");
    let warning: Vec<&str> = stderr.lines().filter(|l| l.contains("warning")).collect();
    assert_eq!(warning.len(), 1, "one line: {stderr}");
    assert!(warning[0].contains("soft"), "{stderr}");
    assert!(warning[0].contains("--force"), "{stderr}");
}

#[test]
fn a_forced_delete_reports_no_undo() {
    let home = tempfile::tempdir().expect("tempdir");
    let db = isolate(home.path());
    let id = seed(&db, "worker");

    let (report, stderr) = delete(home.path(), id, &["--force"]);

    assert_eq!(report["soft"], false, "{report}");
    assert_eq!(report["forced"], true);
    assert!(report["undo_until"].is_null());
    assert!(report["reapable_at"].is_null());
    assert_eq!(report["session"]["force_deleted"], true, "{report}");
    assert!(
        !stderr.contains("warning"),
        "--force was asked for: {stderr}"
    );
}
