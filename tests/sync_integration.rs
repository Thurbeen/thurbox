/// Integration tests for multi-instance session synchronization via SQLite.
///
/// These tests simulate two instances running concurrently against the same
/// database file and verify that session changes are properly shared.
use std::path::PathBuf;

use thurbox::project::{ProjectConfig, ProjectId};
use thurbox::session::{RoleConfig, RolePermissions, SessionId};
use thurbox::storage::Database;
use thurbox::sync::{self, SharedSession, SharedState, SharedWorktree};

/// Helper to create a test session.
fn make_session(id: SessionId, name: &str, project_id: ProjectId) -> SharedSession {
    SharedSession {
        id,
        name: name.to_string(),
        project_id,
        role: "developer".to_string(),
        backend_id: "thurbox:@0".to_string(),
        backend_type: "tmux".to_string(),
        claude_session_id: Some(format!("claude-{name}")),
        cwd: None,
        worktree: None,
        tombstone: false,
        tombstone_at: None,
    }
}

fn test_project_id(name: &str) -> ProjectId {
    let config = ProjectConfig {
        name: name.to_string(),
        repos: vec![],
        roles: vec![],
        id: None,
    };
    config.deterministic_id()
}

// ============================================================================
// Pure delta computation tests (no DB or file I/O)
// ============================================================================

#[test]
fn delta_detects_new_session_from_other_instance() {
    let project_id = ProjectId::default();
    let session_a_id = SessionId::default();
    let session_b_id = SessionId::default();

    // Instance A's view: has session A
    let mut state_a = SharedState::new();
    state_a
        .sessions
        .push(make_session(session_a_id, "Session A", project_id));

    // Shared state: has sessions A and B
    let mut state_shared = SharedState::new();
    state_shared
        .sessions
        .push(make_session(session_a_id, "Session A", project_id));
    state_shared
        .sessions
        .push(make_session(session_b_id, "Session B", project_id));

    let delta = sync::StateDelta::compute(&state_a, &state_shared);

    assert_eq!(
        delta.added_sessions.len(),
        1,
        "Delta should show session B as added"
    );
    assert_eq!(delta.added_sessions[0].id, session_b_id);
    assert_eq!(delta.removed_sessions.len(), 0);
    assert_eq!(delta.updated_sessions.len(), 0);
}

#[test]
fn delta_detects_deleted_session() {
    let project_id = ProjectId::default();
    let session_a_id = SessionId::default();
    let session_b_id = SessionId::default();

    // Instance A's view: has both sessions
    let mut state_a = SharedState::new();
    state_a
        .sessions
        .push(make_session(session_a_id, "Session A", project_id));
    state_a
        .sessions
        .push(make_session(session_b_id, "Session B", project_id));

    // Shared state: only A (B was deleted/tombstoned)
    let mut state_shared = SharedState::new();
    state_shared
        .sessions
        .push(make_session(session_a_id, "Session A", project_id));
    let mut session_b_tombstone = make_session(session_b_id, "Session B", project_id);
    session_b_tombstone.tombstone = true;
    state_shared.sessions.push(session_b_tombstone);

    let delta = sync::StateDelta::compute(&state_a, &state_shared);

    assert_eq!(delta.added_sessions.len(), 0);
    assert_eq!(
        delta.removed_sessions.len(),
        1,
        "Delta should show session B as removed"
    );
    assert_eq!(delta.removed_sessions[0], session_b_id);
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

    let pid = test_project_id("test");
    db_a.insert_project(pid, "test", &[], false).unwrap();

    let session = make_session(SessionId::default(), "Session from A", pid);
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

    let pid = test_project_id("test");
    db_a.insert_project(pid, "test", &[], false).unwrap();

    // Instance A creates session
    let session_a = make_session(SessionId::default(), "Session A", pid);
    let sid_a = session_a.id;
    db_a.upsert_session(&session_a).unwrap();

    // Instance B creates session
    let session_b = make_session(SessionId::default(), "Session B", pid);
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

    let pid = test_project_id("test");
    db_a.insert_project(pid, "test", &[], false).unwrap();

    let session = make_session(SessionId::default(), "Session to Delete", pid);
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
fn db_change_detection_across_instances() {
    let temp = tempfile::NamedTempFile::new().unwrap();
    let path = temp.path();

    let mut db_a = Database::open(path).unwrap();
    let db_b = Database::open(path).unwrap();

    // Initialize change tracking
    let _ = db_a.has_external_changes().unwrap();

    let pid = test_project_id("test");
    db_b.insert_project(pid, "test", &[], false).unwrap();

    // Instance A should detect external change
    assert!(db_a.has_external_changes().unwrap());

    // No further changes — should return false
    assert!(!db_a.has_external_changes().unwrap());
}

#[test]
fn db_compute_delta_detects_added_session() {
    let temp = tempfile::NamedTempFile::new().unwrap();
    let path = temp.path();

    let db = Database::open(path).unwrap();

    let pid = test_project_id("test");
    db.insert_project(pid, "test", &[], false).unwrap();

    // Local snapshot is empty
    let local = SharedState::new();

    // Add session to DB
    let session = make_session(SessionId::default(), "New Session", pid);
    db.upsert_session(&session).unwrap();

    let delta = db.compute_delta(&local).unwrap();
    assert_eq!(delta.added_sessions.len(), 1);
    assert_eq!(delta.added_sessions[0].name, "New Session");
}

#[test]
fn db_compute_delta_detects_removed_session() {
    let temp = tempfile::NamedTempFile::new().unwrap();
    let path = temp.path();

    let db = Database::open(path).unwrap();

    let pid = test_project_id("test");
    db.insert_project(pid, "test", &[], false).unwrap();

    let session = make_session(SessionId::default(), "Session to Remove", pid);
    let sid = session.id;
    db.upsert_session(&session).unwrap();

    // Local snapshot has the session
    let local = db.load_shared_state().unwrap();

    // Soft-delete it
    db.soft_delete_session(sid).unwrap();

    let delta = db.compute_delta(&local).unwrap();
    assert_eq!(delta.removed_sessions.len(), 1);
    assert_eq!(delta.removed_sessions[0], sid);
}

#[test]
fn db_project_soft_delete_propagates() {
    let temp = tempfile::NamedTempFile::new().unwrap();
    let path = temp.path();

    let db_a = Database::open(path).unwrap();
    let db_b = Database::open(path).unwrap();

    let pid = test_project_id("Shared Project");
    db_a.insert_project(pid, "Shared Project", &[PathBuf::from("/repo")], false)
        .unwrap();

    // Both see the project
    assert_eq!(db_b.list_active_projects().unwrap().len(), 1);

    // Instance A deletes the project
    db_a.soft_delete_project(pid).unwrap();

    // Instance B should no longer see it
    assert_eq!(db_b.list_active_projects().unwrap().len(), 0);
}

#[test]
fn db_audit_trail_records_operations() {
    let db = Database::open_in_memory().unwrap();

    let pid = test_project_id("test");
    db.insert_project(pid, "test", &[], false).unwrap();

    let session = make_session(SessionId::default(), "Audited Session", pid);
    db.upsert_session(&session).unwrap();

    // Check audit log has entries
    let log = db.get_audit_log(None, None, 100).unwrap();
    assert!(
        log.len() >= 2,
        "Should have at least project + session audit entries"
    );
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

    let pid = test_project_id("test");
    db.insert_project(pid, "test", &[], false).unwrap();

    let mut session = make_session(SessionId::default(), "WT Session", pid);
    session.worktree = Some(SharedWorktree {
        repo_path: PathBuf::from("/repo"),
        worktree_path: PathBuf::from("/repo/.git/wt/feat"),
        branch: "feat".to_string(),
    });
    db.upsert_session(&session).unwrap();

    let wt = db.get_worktree(session.id).unwrap();
    assert!(wt.is_some());
    let wt = wt.unwrap();
    assert_eq!(wt.branch, "feat");
    assert_eq!(wt.repo_path, PathBuf::from("/repo"));
}

#[test]
fn db_session_metadata_preserved_across_instances() {
    let temp = tempfile::NamedTempFile::new().unwrap();
    let path = temp.path();

    let db_a = Database::open(path).unwrap();
    let db_b = Database::open(path).unwrap();

    let pid = test_project_id("test");
    db_a.insert_project(pid, "test", &[], false).unwrap();

    let session_id = SessionId::default();
    let session = SharedSession {
        id: session_id,
        name: "Dev Session".to_string(),
        project_id: pid,
        role: "developer".to_string(),
        backend_id: "thurbox:@0".to_string(),
        backend_type: "tmux".to_string(),
        claude_session_id: Some("claude-123".to_string()),
        cwd: Some(PathBuf::from("/home/dev")),
        worktree: None,
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
    assert_eq!(s.role, "developer");
    assert_eq!(s.backend_id, "thurbox:@0");
    assert_eq!(s.claude_session_id, Some("claude-123".to_string()));
    assert_eq!(s.cwd, Some(PathBuf::from("/home/dev")));
}

#[test]
fn db_multiple_sessions_per_project() {
    let temp = tempfile::NamedTempFile::new().unwrap();
    let path = temp.path();

    let db_a = Database::open(path).unwrap();
    let db_b = Database::open(path).unwrap();

    let pid = test_project_id("test");
    db_a.insert_project(pid, "test", &[], false).unwrap();

    // Instance A creates 2 sessions
    let s1 = make_session(SessionId::default(), "Session 1", pid);
    let s2 = make_session(SessionId::default(), "Session 2", pid);
    let sid1 = s1.id;
    let sid2 = s2.id;
    db_a.upsert_session(&s1).unwrap();
    db_a.upsert_session(&s2).unwrap();

    // Instance B creates 1 session
    let s3 = make_session(SessionId::default(), "Session 3", pid);
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

// ============================================================================
// poll_for_changes integration tests
// ============================================================================

#[test]
fn poll_for_changes_returns_none_when_no_external_changes() {
    let temp = tempfile::NamedTempFile::new().unwrap();
    let path = temp.path();

    let mut db = Database::open(path).unwrap();
    let mut sync_state = sync::SyncState::with_interval(std::time::Duration::from_millis(0));

    // Initialize change tracking
    let _ = db.has_external_changes().unwrap();

    // No external changes — should return None
    std::thread::sleep(std::time::Duration::from_millis(1));
    let result = sync::poll_for_changes(&mut sync_state, &mut db).unwrap();
    assert!(result.is_none());
}

#[test]
fn poll_for_changes_detects_external_session_add() {
    let temp = tempfile::NamedTempFile::new().unwrap();
    let path = temp.path();

    let mut db_poller = Database::open(path).unwrap();
    let db_writer = Database::open(path).unwrap();

    let mut sync_state = sync::SyncState::with_interval(std::time::Duration::from_millis(0));

    // Initialize change tracking
    let _ = db_poller.has_external_changes().unwrap();

    // External writer adds a session
    let pid = test_project_id("test");
    db_writer.insert_project(pid, "test", &[], false).unwrap();
    let session = make_session(SessionId::default(), "External Session", pid);
    db_writer.upsert_session(&session).unwrap();

    // Poll should detect the change
    std::thread::sleep(std::time::Duration::from_millis(1));
    let result = sync::poll_for_changes(&mut sync_state, &mut db_poller).unwrap();
    assert!(result.is_some());
    let delta = result.unwrap();
    assert_eq!(delta.added_sessions.len(), 1);
    assert_eq!(delta.added_sessions[0].name, "External Session");
}

#[test]
fn poll_for_changes_respects_interval() {
    let temp = tempfile::NamedTempFile::new().unwrap();
    let path = temp.path();

    let mut db = Database::open(path).unwrap();
    // Long interval — should not poll yet
    let mut sync_state = sync::SyncState::with_interval(std::time::Duration::from_secs(60));

    let result = sync::poll_for_changes(&mut sync_state, &mut db).unwrap();
    assert!(result.is_none());
}

// ============================================================================
// Multi-instance project rename and role sync tests
// ============================================================================

#[test]
fn db_project_rename_propagates_across_instances() {
    let temp = tempfile::NamedTempFile::new().unwrap();
    let path = temp.path();

    let db_a = Database::open(path).unwrap();
    let db_b = Database::open(path).unwrap();

    let pid = test_project_id("OriginalName");
    db_a.insert_project(pid, "OriginalName", &[PathBuf::from("/repo")], false)
        .unwrap();

    // Instance A renames the project
    db_a.update_project(pid, "RenamedProject", &[PathBuf::from("/repo")])
        .unwrap();

    // Instance B should see the renamed project with same ID
    let projects = db_b.list_active_projects().unwrap();
    assert_eq!(projects.len(), 1);
    assert_eq!(projects[0].id, pid);
    assert_eq!(projects[0].name, "RenamedProject");
}

#[test]
fn db_project_rename_does_not_affect_sessions() {
    let temp = tempfile::NamedTempFile::new().unwrap();
    let path = temp.path();

    let db_a = Database::open(path).unwrap();
    let db_b = Database::open(path).unwrap();

    let pid = test_project_id("TestProject");
    db_a.insert_project(pid, "TestProject", &[PathBuf::from("/repo")], false)
        .unwrap();

    // Create a session tied to the project
    let session = make_session(SessionId::default(), "MySession", pid);
    let sid = session.id;
    db_a.upsert_session(&session).unwrap();

    // Instance A renames the project
    db_a.update_project(pid, "NewName", &[PathBuf::from("/repo")])
        .unwrap();

    // Instance B should see the session still tied to the same project ID
    let sessions = db_b.list_active_sessions().unwrap();
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].id, sid);
    assert_eq!(sessions[0].project_id, pid);
}

#[test]
fn db_role_sync_across_instances() {
    let temp = tempfile::NamedTempFile::new().unwrap();
    let path = temp.path();

    let db_a = Database::open(path).unwrap();
    let db_b = Database::open(path).unwrap();

    let pid = test_project_id("RoleProject");
    db_a.insert_project(pid, "RoleProject", &[], false).unwrap();

    // Instance A adds roles
    let roles = vec![
        RoleConfig {
            name: "developer".to_string(),
            description: "Full access".to_string(),
            permissions: RolePermissions::default(),
        },
        RoleConfig {
            name: "reviewer".to_string(),
            description: "Read only".to_string(),
            permissions: RolePermissions::default(),
        },
    ];
    db_a.replace_roles(pid, &roles).unwrap();

    // Instance B should see the roles via list_active_projects
    let projects = db_b.list_active_projects().unwrap();
    assert_eq!(projects.len(), 1);
    assert_eq!(projects[0].roles.len(), 2);
    assert!(projects[0].roles.iter().any(|r| r.name == "developer"));
    assert!(projects[0].roles.iter().any(|r| r.name == "reviewer"));
}

#[test]
fn db_delta_detects_role_change() {
    let temp = tempfile::NamedTempFile::new().unwrap();
    let path = temp.path();

    let db = Database::open(path).unwrap();

    let pid = test_project_id("DeltaRoleProject");
    db.insert_project(pid, "DeltaRoleProject", &[], false)
        .unwrap();

    // Take initial snapshot (no roles)
    let initial = db.load_shared_state().unwrap();
    assert_eq!(initial.projects.len(), 1);
    assert_eq!(initial.projects[0].roles.len(), 0);

    // Add a role
    db.replace_roles(
        pid,
        &[RoleConfig {
            name: "admin".to_string(),
            description: "Admin role".to_string(),
            permissions: RolePermissions::default(),
        }],
    )
    .unwrap();

    // Delta should detect the project as updated
    let delta = db.compute_delta(&initial).unwrap();
    assert_eq!(
        delta.updated_projects.len(),
        1,
        "Should detect role change as project update"
    );
    assert_eq!(delta.updated_projects[0].roles.len(), 1);
    assert_eq!(delta.updated_projects[0].roles[0].name, "admin");
}

#[test]
fn db_delta_detects_project_rename() {
    let temp = tempfile::NamedTempFile::new().unwrap();
    let path = temp.path();

    let db = Database::open(path).unwrap();

    let pid = test_project_id("BeforeRename");
    db.insert_project(pid, "BeforeRename", &[PathBuf::from("/repo")], false)
        .unwrap();

    // Take snapshot
    let before = db.load_shared_state().unwrap();
    assert_eq!(before.projects[0].name, "BeforeRename");

    // Rename
    db.update_project(pid, "AfterRename", &[PathBuf::from("/repo")])
        .unwrap();

    // Delta should detect the rename
    let delta = db.compute_delta(&before).unwrap();
    assert_eq!(delta.updated_projects.len(), 1);
    assert_eq!(delta.updated_projects[0].name, "AfterRename");
    assert_eq!(delta.updated_projects[0].id, pid);
}
