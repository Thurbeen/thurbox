/// Integration tests for multi-instance session sharing via SQLite.
///
/// These tests simulate two instances running concurrently against the same
/// database file and verify that session changes are properly shared. (v1's
/// snapshot/delta sync engine is gone; instances now re-read rows when
/// `PRAGMA data_version` moves.)
use std::path::PathBuf;

use thurbox::session::SessionId;
use thurbox::storage::Database;
use thurbox::sync::{SharedSession, SharedWorktree};

/// Helper to create a test session.
fn make_session(id: SessionId, name: &str) -> SharedSession {
    SharedSession {
        id,
        name: name.to_string(),
        agent: "developer".to_string(),
        backend_id: "thurbox:@0".to_string(),
        backend_type: "tmux".to_string(),
        agent_session_id: Some(format!("claude-{name}")),
        cwd: None,
        additional_dirs: Vec::new(),
        worktrees: Vec::new(),
        shell_backend_id: None,
        parent_session_id: None,
        display_order: None,
        tombstone: false,
        tombstone_at: None,
    }
}

// ============================================================================
// SQLite-based multi-instance sync tests
// ============================================================================

#[test]
fn db_instance_a_creates_session_visible_to_instance_b() {
    let temp = tempfile::NamedTempFile::new().unwrap();
    let path = temp.path();

    let db_a = Database::open(path).unwrap();
    let db_b = Database::open(path).unwrap();

    let session = make_session(SessionId::default(), "Session from A");
    let sid = session.id;
    db_a.upsert_session(&session).unwrap();

    // Instance B queries DB — should see the session
    let sessions = db_b.list_active_sessions().unwrap();
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].name, "Session from A");
    assert_eq!(sessions[0].id, sid);
}

#[test]
fn db_instance_b_creates_session_without_erasing_instance_a() {
    let temp = tempfile::NamedTempFile::new().unwrap();
    let path = temp.path();

    let db_a = Database::open(path).unwrap();
    let db_b = Database::open(path).unwrap();

    // Instance A creates session
    let session_a = make_session(SessionId::default(), "Session A");
    let sid_a = session_a.id;
    db_a.upsert_session(&session_a).unwrap();

    // Instance B creates session
    let session_b = make_session(SessionId::default(), "Session B");
    let sid_b = session_b.id;
    db_b.upsert_session(&session_b).unwrap();

    // Both instances should see both sessions
    let sessions_from_a = db_a.list_active_sessions().unwrap();
    assert_eq!(sessions_from_a.len(), 2);

    let sessions_from_b = db_b.list_active_sessions().unwrap();
    assert_eq!(sessions_from_b.len(), 2);

    let ids: Vec<SessionId> = sessions_from_a.iter().map(|s| s.id).collect();
    assert!(ids.contains(&sid_a));
    assert!(ids.contains(&sid_b));
}

#[test]
fn db_soft_delete_propagates_across_instances() {
    let temp = tempfile::NamedTempFile::new().unwrap();
    let path = temp.path();

    let db_a = Database::open(path).unwrap();
    let db_b = Database::open(path).unwrap();

    let session = make_session(SessionId::default(), "Session to Delete");
    let sid = session.id;
    db_a.upsert_session(&session).unwrap();

    // Verify session exists in both
    assert_eq!(db_b.list_active_sessions().unwrap().len(), 1);

    // Instance A soft-deletes
    db_a.soft_delete_session(sid).unwrap();

    // Instance B should no longer see it in active sessions
    assert_eq!(db_b.list_active_sessions().unwrap().len(), 0);
}

#[test]
fn db_audit_trail_records_operations() {
    let db = Database::open_in_memory().unwrap();

    let session = make_session(SessionId::default(), "Audited Session");
    db.upsert_session(&session).unwrap();

    // Check audit log has entries
    let log = db.get_audit_log(None, None, 100).unwrap();
    assert!(!log.is_empty(), "Should have at least session audit entry");
}

#[test]
fn db_session_counter_synchronized() {
    let temp = tempfile::NamedTempFile::new().unwrap();
    let path = temp.path();

    let db_a = Database::open(path).unwrap();
    let db_b = Database::open(path).unwrap();

    // Instance A sets counter
    db_a.set_session_counter(5).unwrap();

    // Instance B should see the same counter
    assert_eq!(db_b.get_session_counter().unwrap(), 5);

    // Instance B increments
    let new_val = db_b.increment_session_counter().unwrap();
    assert_eq!(new_val, 6);

    // Instance A should see updated counter
    assert_eq!(db_a.get_session_counter().unwrap(), 6);
}

#[test]
fn db_worktree_persisted_with_session() {
    let db = Database::open_in_memory().unwrap();

    let mut session = make_session(SessionId::default(), "WT Session");
    session.worktrees = vec![SharedWorktree {
        repo_path: PathBuf::from("/repo"),
        worktree_path: PathBuf::from("/repo/.git/wt/feat"),
        branch: "feat".to_string(),
        created_by_thurbox: true,
    }];
    db.upsert_session(&session).unwrap();

    let wts = db.get_worktrees(session.id).unwrap();
    assert_eq!(wts.len(), 1);
    assert_eq!(wts[0].branch, "feat");
    assert_eq!(wts[0].repo_path, PathBuf::from("/repo"));
}

#[test]
fn db_multi_worktree_persisted_and_loaded_via_sessions() {
    let db = Database::open_in_memory().unwrap();

    let mut session = make_session(SessionId::default(), "Multi-WT");
    session.worktrees = vec![
        SharedWorktree {
            repo_path: PathBuf::from("/repo1"),
            worktree_path: PathBuf::from("/repo1/.git/wt/feat"),
            branch: "feat".to_string(),
            created_by_thurbox: true,
        },
        SharedWorktree {
            repo_path: PathBuf::from("/repo2"),
            worktree_path: PathBuf::from("/repo2/.git/wt/feat"),
            branch: "feat".to_string(),
            created_by_thurbox: true,
        },
    ];
    db.upsert_session(&session).unwrap();

    // Verify via list_active_sessions (exercises the LEFT JOIN row merging)
    let sessions = db.list_active_sessions().unwrap();
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].worktrees.len(), 2);
    assert_eq!(sessions[0].worktrees[0].repo_path, PathBuf::from("/repo1"));
    assert_eq!(sessions[0].worktrees[1].repo_path, PathBuf::from("/repo2"));
}

#[test]
fn db_multi_worktree_propagates_across_instances() {
    let temp = tempfile::NamedTempFile::new().unwrap();
    let path = temp.path();

    let db_a = Database::open(path).unwrap();
    let db_b = Database::open(path).unwrap();

    let mut session = make_session(SessionId::default(), "Multi-WT");
    session.worktrees = vec![
        SharedWorktree {
            repo_path: PathBuf::from("/repo1"),
            worktree_path: PathBuf::from("/repo1/.git/wt/feat"),
            branch: "feat".to_string(),
            created_by_thurbox: true,
        },
        SharedWorktree {
            repo_path: PathBuf::from("/repo2"),
            worktree_path: PathBuf::from("/repo2/.git/wt/feat"),
            branch: "feat".to_string(),
            created_by_thurbox: true,
        },
    ];
    db_a.upsert_session(&session).unwrap();

    // Instance B should see both worktrees
    let sessions = db_b.list_active_sessions().unwrap();
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].worktrees.len(), 2);
}

#[test]
fn db_session_metadata_preserved_across_instances() {
    let temp = tempfile::NamedTempFile::new().unwrap();
    let path = temp.path();

    let db_a = Database::open(path).unwrap();
    let db_b = Database::open(path).unwrap();

    let session_id = SessionId::default();
    let session = SharedSession {
        id: session_id,
        name: "Dev Session".to_string(),
        agent: "developer".to_string(),
        backend_id: "thurbox:@0".to_string(),
        backend_type: "tmux".to_string(),
        agent_session_id: Some("claude-123".to_string()),
        cwd: Some(PathBuf::from("/home/dev")),
        additional_dirs: Vec::new(),
        worktrees: Vec::new(),
        shell_backend_id: None,
        parent_session_id: None,
        display_order: None,
        tombstone: false,
        tombstone_at: None,
    };
    db_a.upsert_session(&session).unwrap();

    // Instance B should see all metadata
    let sessions = db_b.list_active_sessions().unwrap();
    assert_eq!(sessions.len(), 1);
    let s = &sessions[0];
    assert_eq!(s.id, session_id);
    assert_eq!(s.name, "Dev Session");
    assert_eq!(s.agent, "developer");
    assert_eq!(s.backend_id, "thurbox:@0");
    assert_eq!(s.agent_session_id, Some("claude-123".to_string()));
    assert_eq!(s.cwd, Some(PathBuf::from("/home/dev")));
}

#[test]
fn db_multiple_sessions_created_and_deleted() {
    let temp = tempfile::NamedTempFile::new().unwrap();
    let path = temp.path();

    let db_a = Database::open(path).unwrap();
    let db_b = Database::open(path).unwrap();

    // Instance A creates 2 sessions
    let s1 = make_session(SessionId::default(), "Session 1");
    let s2 = make_session(SessionId::default(), "Session 2");
    let sid1 = s1.id;
    let sid2 = s2.id;
    db_a.upsert_session(&s1).unwrap();
    db_a.upsert_session(&s2).unwrap();

    // Instance B creates 1 session
    let s3 = make_session(SessionId::default(), "Session 3");
    let sid3 = s3.id;
    db_b.upsert_session(&s3).unwrap();

    // All instances should see 3 sessions
    let sessions = db_a.list_active_sessions().unwrap();
    assert_eq!(sessions.len(), 3);

    let ids: Vec<SessionId> = sessions.iter().map(|s| s.id).collect();
    assert!(ids.contains(&sid1));
    assert!(ids.contains(&sid2));
    assert!(ids.contains(&sid3));

    // Soft-delete session 2
    db_b.soft_delete_session(sid2).unwrap();

    // Should now see 2 sessions
    let remaining = db_a.list_active_sessions().unwrap();
    assert_eq!(remaining.len(), 2);
    let remaining_ids: Vec<SessionId> = remaining.iter().map(|s| s.id).collect();
    assert!(remaining_ids.contains(&sid1));
    assert!(!remaining_ids.contains(&sid2));
    assert!(remaining_ids.contains(&sid3));
}

/// `thurbox-cli watch` streams changes instead of making its reader poll.
///
/// Driven through the real binary against a real database file, because the
/// property under test is exactly the cross-process one: the writer is a
/// different connection, and `PRAGMA data_version` is what tells the reader
/// that. An in-process test would share a connection and never move it.
#[test]
fn watch_emits_a_line_when_another_process_changes_a_session() {
    use std::io::{BufRead, BufReader};
    use std::process::{Command, Stdio};

    let dir = tempfile::tempdir().expect("tempdir");
    let data = dir.path().join("data");
    std::fs::create_dir_all(&data).expect("mkdir");

    // A relocated instance: its own database, and its own tmux socket name,
    // so nothing here can reach the operator's server even by accident.
    let mut child = Command::new(env!("CARGO_BIN_EXE_thurbox-cli"))
        .args(["watch", "--for-secs", "20"])
        .env("HOME", dir.path())
        .env("USERPROFILE", dir.path())
        .env("XDG_DATA_HOME", dir.path().join("xdg-data"))
        .env("XDG_CONFIG_HOME", dir.path().join("xdg-config"))
        .env("THURBOX_DATA_DIR", &data)
        .env_remove("THURBOX_SOCKET")
        .env_remove("THURBOX_SOCKET_FOR")
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn watch");

    // Give the watcher its baseline read before changing anything, so the write
    // below is unambiguously a *change* rather than part of the initial state.
    std::thread::sleep(std::time::Duration::from_millis(700));

    let db = Database::open(&data.join("thurbox.db")).expect("open the same database");
    let id = SessionId::default();
    db.upsert_session(&make_session(id, "watched"))
        .expect("write");

    // Read one line, with the child killed either way so a failure cannot leave
    // a process behind.
    let stdout = child.stdout.take().expect("piped stdout");
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        if let Some(Ok(line)) = BufReader::new(stdout).lines().next() {
            let _ = tx.send(line);
        }
    });
    let line = rx.recv_timeout(std::time::Duration::from_secs(15));
    let _ = child.kill();
    let _ = child.wait();

    let line = line.expect("watch should emit a line when a session appears");
    let event: serde_json::Value = serde_json::from_str(&line).expect("one JSON object per line");
    assert_eq!(event["event"].as_str(), Some("created"));
    assert_eq!(event["name"].as_str(), Some("watched"));
    assert_eq!(event["session"].as_str(), Some(id.to_string().as_str()));
}
