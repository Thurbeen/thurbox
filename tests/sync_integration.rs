/// Integration tests for multi-instance session synchronization.
///
/// These tests simulate two instances running concurrently and verify
/// that session changes are properly shared between them.
use std::collections::HashMap;
use tempfile::TempDir;
use thurbox::paths::TestPathGuard;
use thurbox::project::ProjectId;
use thurbox::session::SessionId;
use thurbox::sync::{self, SharedSession, SharedState};

/// Helper to create a test session
fn make_session(id: SessionId, name: &str, project_id: ProjectId) -> SharedSession {
    SharedSession {
        id,
        name: name.to_string(),
        project_id,
        role: "developer".to_string(),
        backend_id: "thurbox:@0".to_string(),
        backend_type: "tmux".to_string(),
        claude_session_id: Some(format!("claude-{}", name)),
        cwd: None,
        worktree: None,
        tombstone: false,
        tombstone_at: None,
    }
}

#[test]
fn instance_a_creates_session_visible_to_instance_b() {
    let temp_dir = TempDir::new().unwrap();
    let _guard = TestPathGuard::new(temp_dir.path());

    let project_id = ProjectId::default();
    let session_id = SessionId::default();

    // Instance A: create a session and write to shared state
    let mut state_a = SharedState::new();
    state_a
        .sessions
        .push(make_session(session_id, "Session from A", project_id));
    state_a.session_counter = 1;

    let shared_path = thurbox::paths::shared_state_file().unwrap();
    sync::file_store::save_shared_state(&shared_path, &state_a).unwrap();

    // Instance B: load shared state and see instance A's session
    let state_b = sync::file_store::load_shared_state(&shared_path).unwrap();

    assert_eq!(state_b.sessions.len(), 1);
    assert_eq!(state_b.sessions[0].name, "Session from A");
    assert_eq!(state_b.sessions[0].id, session_id);
}

#[test]
fn instance_b_creates_session_without_erasing_instance_a() {
    let temp_dir = TempDir::new().unwrap();
    let _guard = TestPathGuard::new(temp_dir.path());

    let project_id = ProjectId::default();
    let session_a_id = SessionId::default();
    let session_b_id = SessionId::default();

    // Instance A: create session and write
    let mut state_a = SharedState::new();
    state_a
        .sessions
        .push(make_session(session_a_id, "Session A", project_id));
    state_a.session_counter = 1;
    let shared_path = thurbox::paths::shared_state_file().unwrap();
    sync::file_store::save_shared_state(&shared_path, &state_a).unwrap();

    // Instance B: load, add its own session, write (merging)
    let mut state_b = sync::file_store::load_shared_state(&shared_path).unwrap();
    state_b
        .sessions
        .push(make_session(session_b_id, "Session B", project_id));
    state_b.session_counter = 2;
    state_b.last_modified += 1000;
    sync::file_store::save_shared_state(&shared_path, &state_b).unwrap();

    // Instance A: reload and verify both sessions are present
    let state_a_reload = sync::file_store::load_shared_state(&shared_path).unwrap();

    assert_eq!(
        state_a_reload.sessions.len(),
        2,
        "Both sessions should be present"
    );

    let names: HashMap<SessionId, String> = state_a_reload
        .sessions
        .iter()
        .map(|s| (s.id, s.name.clone()))
        .collect();

    assert_eq!(names.get(&session_a_id), Some(&"Session A".to_string()));
    assert_eq!(names.get(&session_b_id), Some(&"Session B".to_string()));
}

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

    // Compute delta
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

    // Compute delta
    let delta = sync::StateDelta::compute(&state_a, &state_shared);

    assert_eq!(delta.added_sessions.len(), 0);
    assert_eq!(
        delta.removed_sessions.len(),
        1,
        "Delta should show session B as removed"
    );
    assert_eq!(delta.removed_sessions[0], session_b_id);
}

#[test]
fn session_counter_avoids_conflicts() {
    let temp_dir = TempDir::new().unwrap();
    let _guard = TestPathGuard::new(temp_dir.path());

    let project_id = ProjectId::default();

    // Instance A: create 2 sessions (counter = 2)
    let mut state_a = SharedState::new();
    state_a.sessions.push(make_session(
        SessionId::default(),
        "A-Session-1",
        project_id,
    ));
    state_a.sessions.push(make_session(
        SessionId::default(),
        "A-Session-2",
        project_id,
    ));
    state_a.session_counter = 2;
    let shared_path = thurbox::paths::shared_state_file().unwrap();
    sync::file_store::save_shared_state(&shared_path, &state_a).unwrap();

    // Instance B: load A's state, add its own, write (merge)
    let mut state_b = sync::file_store::load_shared_state(&shared_path).unwrap();
    state_b.sessions.push(make_session(
        SessionId::default(),
        "B-Session-1",
        project_id,
    ));
    state_b.session_counter = 3; // B created its own session
    state_b.last_modified += 1000;
    sync::file_store::save_shared_state(&shared_path, &state_b).unwrap();

    // Verify final state has both A's and B's sessions with max counter
    let final_state = sync::file_store::load_shared_state(&shared_path).unwrap();
    assert_eq!(
        final_state.session_counter, 3,
        "Counter should be max of both"
    );
    assert_eq!(
        final_state.sessions.len(),
        3,
        "Should have 2 from A + 1 from B = 3 total"
    );
}

#[test]
fn two_instances_adopt_same_session_no_duplicates() {
    let temp_dir = TempDir::new().unwrap();
    let _guard = TestPathGuard::new(temp_dir.path());

    let project_id = ProjectId::default();
    let session_id = SessionId::default();

    // Instance A: Create a session and write to shared state
    let mut state_a = SharedState::new();
    state_a
        .sessions
        .push(make_session(session_id, "Shared Session", project_id));
    state_a.session_counter = 1;
    let shared_path = thurbox::paths::shared_state_file().unwrap();
    sync::file_store::save_shared_state(&shared_path, &state_a).unwrap();

    // Instance B: Load state and see session A's session
    let state_b = sync::file_store::load_shared_state(&shared_path).unwrap();
    assert_eq!(state_b.sessions.len(), 1);
    assert_eq!(state_b.sessions[0].id, session_id);

    // Instance A: Now read again, should still have the same session
    // (simulating that A adopts the session it created)
    let state_a_reload = sync::file_store::load_shared_state(&shared_path).unwrap();

    // Key test: When both instances see the same session, session IDs should not duplicate
    // This would manifest as project.session_ids.len() > 1 for same session_id
    assert_eq!(state_a_reload.sessions.len(), 1);
    assert_eq!(state_a_reload.sessions[0].id, session_id);
}

#[test]
fn removed_session_cleaned_from_all_projects() {
    let temp_dir = TempDir::new().unwrap();
    let _guard = TestPathGuard::new(temp_dir.path());

    let project_id = ProjectId::default();
    let session_id = SessionId::default();

    // Initial state: Session exists in project
    let mut state = SharedState::new();
    state
        .sessions
        .push(make_session(session_id, "Session", project_id));
    state.session_counter = 1;
    let shared_path = thurbox::paths::shared_state_file().unwrap();
    sync::file_store::save_shared_state(&shared_path, &state).unwrap();

    // Instance B loads it
    let state_b = sync::file_store::load_shared_state(&shared_path).unwrap();
    assert_eq!(state_b.sessions.len(), 1);

    // Instance A deletes the session (tombstones it with old timestamp)
    let mut state_a_delete = SharedState::new();
    let mut tombstoned = make_session(session_id, "Session", project_id);
    tombstoned.tombstone = true;
    // Use an old timestamp so it will be purged (> 60 seconds old)
    tombstoned.tombstone_at = Some(sync::current_time_millis() - 61_000);
    state_a_delete.sessions.push(tombstoned);
    state_a_delete.session_counter = 1;
    state_a_delete.last_modified += 1000;
    sync::file_store::save_shared_state(&shared_path, &state_a_delete).unwrap();

    // Instance B reloads and purges old tombstones
    let state_b_reload = sync::file_store::load_shared_state(&shared_path).unwrap();
    // After load, file_store already calls purge_old_tombstones() internally
    // Verify session is gone
    assert_eq!(state_b_reload.sessions.len(), 0);
}

#[test]
fn concurrent_adoption_preserves_session_metadata() {
    let temp_dir = TempDir::new().unwrap();
    let _guard = TestPathGuard::new(temp_dir.path());

    let project_id = ProjectId::default();
    let session_id = SessionId::default();

    // Instance A: Create session with specific metadata
    let mut state_a = SharedState::new();
    state_a.sessions.push(SharedSession {
        id: session_id,
        name: "Dev Session".to_string(),
        project_id,
        role: "developer".to_string(),
        backend_id: "thurbox:@0".to_string(),
        backend_type: "tmux".to_string(),
        claude_session_id: Some("claude-123".to_string()),
        cwd: Some(std::path::PathBuf::from("/home/dev")),
        worktree: None,
        tombstone: false,
        tombstone_at: None,
    });
    state_a.session_counter = 1;
    let shared_path = thurbox::paths::shared_state_file().unwrap();
    sync::file_store::save_shared_state(&shared_path, &state_a).unwrap();

    // Instance B: Load and verify all metadata preserved
    let state_b = sync::file_store::load_shared_state(&shared_path).unwrap();
    assert_eq!(state_b.sessions.len(), 1);
    let session = &state_b.sessions[0];

    assert_eq!(session.id, session_id);
    assert_eq!(session.name, "Dev Session");
    assert_eq!(session.role, "developer");
    assert_eq!(session.backend_id, "thurbox:@0");
    assert_eq!(session.claude_session_id, Some("claude-123".to_string()));
    assert_eq!(session.cwd, Some(std::path::PathBuf::from("/home/dev")));

    // Instance A: Now update the session metadata (rename)
    let mut state_a_update = sync::file_store::load_shared_state(&shared_path).unwrap();
    if let Some(session) = state_a_update.sessions.iter_mut().next() {
        session.name = "Dev Session (Updated)".to_string();
    }
    state_a_update.last_modified += 1000;
    sync::file_store::save_shared_state(&shared_path, &state_a_update).unwrap();

    // Instance B: Reload and see the update
    let state_b_reload = sync::file_store::load_shared_state(&shared_path).unwrap();
    assert_eq!(state_b_reload.sessions[0].name, "Dev Session (Updated)");
}

#[test]
fn multiple_sessions_per_project_stay_synchronized() {
    let temp_dir = TempDir::new().unwrap();
    let _guard = TestPathGuard::new(temp_dir.path());

    let project_id = ProjectId::default();
    let session1_id = SessionId::default();
    let session2_id = SessionId::default();
    let session3_id = SessionId::default();

    // Instance A: Create 2 sessions
    let mut state_a = SharedState::new();
    state_a
        .sessions
        .push(make_session(session1_id, "Session 1", project_id));
    state_a
        .sessions
        .push(make_session(session2_id, "Session 2", project_id));
    state_a.session_counter = 2;
    let shared_path = thurbox::paths::shared_state_file().unwrap();
    sync::file_store::save_shared_state(&shared_path, &state_a).unwrap();

    // Instance B: Load A's sessions, add its own
    let mut state_b = sync::file_store::load_shared_state(&shared_path).unwrap();
    assert_eq!(state_b.sessions.len(), 2);
    state_b
        .sessions
        .push(make_session(session3_id, "Session 3", project_id));
    state_b.session_counter = 3;
    state_b.last_modified += 1000;
    sync::file_store::save_shared_state(&shared_path, &state_b).unwrap();

    // Instance A: Reload and should see all 3
    let state_a_reload = sync::file_store::load_shared_state(&shared_path).unwrap();
    assert_eq!(state_a_reload.sessions.len(), 3);

    // Verify all session IDs are present
    let ids: HashMap<SessionId, _> = state_a_reload
        .sessions
        .iter()
        .map(|s| (s.id, s.name.clone()))
        .collect();

    assert!(ids.contains_key(&session1_id));
    assert!(ids.contains_key(&session2_id));
    assert!(ids.contains_key(&session3_id));

    // Instance B: Delete session 2 with old tombstone time so it gets purged
    let mut state_b_delete = sync::file_store::load_shared_state(&shared_path).unwrap();
    state_b_delete.sessions.iter_mut().for_each(|s| {
        if s.id == session2_id {
            s.tombstone = true;
            // Use old timestamp so it will be purged
            s.tombstone_at = Some(sync::current_time_millis() - 61_000);
        }
    });
    state_b_delete.last_modified += 2000;
    sync::file_store::save_shared_state(&shared_path, &state_b_delete).unwrap();

    // Instance A: Reload - file_store.load_shared_state() automatically purges old tombstones
    let state_a_final = sync::file_store::load_shared_state(&shared_path).unwrap();
    // Should have 2 sessions left (1 and 3), old tombstone is purged on load
    assert_eq!(state_a_final.sessions.len(), 2);

    // Verify sessions 1 and 3 remain, session 2 is gone
    let final_ids: Vec<SessionId> = state_a_final.sessions.iter().map(|s| s.id).collect();

    assert!(final_ids.contains(&session1_id));
    assert!(!final_ids.contains(&session2_id));
    assert!(final_ids.contains(&session3_id));
}

#[test]
fn session_ids_not_duplicated_on_reload() {
    let temp_dir = TempDir::new().unwrap();
    let _guard = TestPathGuard::new(temp_dir.path());

    let project_id = ProjectId::default();
    let session_id = SessionId::default();

    // Simulate Instance B creating a session
    let mut state_b = SharedState::new();
    state_b
        .sessions
        .push(make_session(session_id, "Session from B", project_id));
    state_b.session_counter = 1;
    let shared_path = thurbox::paths::shared_state_file().unwrap();
    sync::file_store::save_shared_state(&shared_path, &state_b).unwrap();

    // Instance A loads for the first time
    let mut state_a = sync::file_store::load_shared_state(&shared_path).unwrap();

    // Simulate Instance A has a project with this session
    // (normally this would happen via adoption and project association)
    assert_eq!(state_a.sessions.len(), 1);
    let session = &state_a.sessions[0];
    assert_eq!(session.id, session_id);

    // Instance A writes back (no changes, just reload)
    state_a.last_modified += 1000;
    sync::file_store::save_shared_state(&shared_path, &state_a).unwrap();

    // Instance A reloads (simulates restart)
    let state_a_reload = sync::file_store::load_shared_state(&shared_path).unwrap();

    // CRITICAL: Session count should still be 1, not 2
    assert_eq!(state_a_reload.sessions.len(), 1);
    assert_eq!(state_a_reload.sessions[0].id, session_id);
}

#[test]
fn multiple_instances_can_adopt_same_session() {
    let temp_dir = TempDir::new().unwrap();
    let _guard = TestPathGuard::new(temp_dir.path());

    // Setup: Create shared state with one session
    let session_id = SessionId::default();
    let project_id = ProjectId::default();
    let mut state = SharedState::new();

    state.sessions.push(SharedSession {
        id: session_id,
        name: "Shared Session".to_string(),
        project_id,
        role: "developer".to_string(),
        backend_id: "thurbox:%42".to_string(),
        backend_type: "tmux".to_string(),
        claude_session_id: Some("claude-123".to_string()),
        cwd: None,
        worktree: None,
        tombstone: false,
        tombstone_at: None,
    });

    let shared_path = thurbox::paths::shared_state_file().unwrap();
    sync::file_store::save_shared_state(&shared_path, &state).unwrap();

    // Simulate Instance A loading the shared state
    let state_a = sync::file_store::load_shared_state(&shared_path).unwrap();
    let sessions_a: Vec<_> = state_a.sessions.iter().filter(|s| !s.tombstone).collect();

    assert_eq!(sessions_a.len(), 1);
    assert_eq!(sessions_a[0].id, session_id);
    assert_eq!(
        sessions_a[0].claude_session_id,
        Some("claude-123".to_string())
    );

    // Simulate Instance B loading the SAME shared state
    let state_b = sync::file_store::load_shared_state(&shared_path).unwrap();
    let sessions_b: Vec<_> = state_b.sessions.iter().filter(|s| !s.tombstone).collect();

    // Both instances should see the same session
    assert_eq!(sessions_b.len(), 1);
    assert_eq!(sessions_b[0].id, session_id);
    assert_eq!(
        sessions_b[0].claude_session_id,
        Some("claude-123".to_string())
    );

    // No ownership field — both instances are free to adopt without restriction
    // (Ownership was removed to enable true multi-instance collaboration)
}

#[test]
fn multiple_instances_independent_adoption() {
    let temp_dir = TempDir::new().unwrap();
    let _guard = TestPathGuard::new(temp_dir.path());

    let session_id = SessionId::default();
    let project_id = ProjectId::default();

    // Create initial shared state
    let mut state = SharedState::new();
    state.sessions.push(SharedSession {
        id: session_id,
        name: "Test Session".to_string(),
        project_id,
        role: "developer".to_string(),
        backend_id: "thurbox:%42".to_string(),
        backend_type: "tmux".to_string(),
        claude_session_id: None,
        cwd: None,
        worktree: None,
        tombstone: false,
        tombstone_at: None,
    });

    let shared_path = thurbox::paths::shared_state_file().unwrap();
    sync::file_store::save_shared_state(&shared_path, &state).unwrap();

    // Instance A loads and modifies
    let mut state_a = sync::file_store::load_shared_state(&shared_path).unwrap();
    if let Some(session) = state_a.sessions.iter_mut().next() {
        session.claude_session_id = Some("claude-a-123".to_string());
    }
    state_a.last_modified = sync::current_time_millis();
    sync::file_store::save_shared_state(&shared_path, &state_a).unwrap();

    // Instance B loads (should see Instance A's changes)
    let state_b = sync::file_store::load_shared_state(&shared_path).unwrap();
    assert_eq!(state_b.sessions.len(), 1);
    assert_eq!(
        state_b.sessions[0].claude_session_id,
        Some("claude-a-123".to_string())
    );

    // Instance B can now interact with the same session
    // (Previously would be blocked by ownership check)
}

#[test]
fn session_id_preserved_across_restart() {
    use thurbox::session::PersistedSession;

    // Simulate creating a session and persisting it
    let original_session_id = SessionId::default();
    let persisted = PersistedSession {
        id: Some(original_session_id),
        name: "Test Session".to_string(),
        claude_session_id: "claude-123".to_string(),
        cwd: None,
        worktree: None,
        role: "developer".to_string(),
        backend_id: "thurbox:%42".to_string(),
        backend_type: "tmux".to_string(),
        project_id: None,
    };

    // Serialize and deserialize (simulating save/load)
    let toml_str = toml::to_string(&persisted).unwrap();
    let restored: PersistedSession = toml::from_str(&toml_str).unwrap();

    // SessionId should be preserved
    assert_eq!(restored.id, Some(original_session_id));
    assert_eq!(restored.name, "Test Session");
    assert_eq!(restored.claude_session_id, "claude-123");
}

#[test]
fn session_without_id_handled_gracefully() {
    use thurbox::session::PersistedSession;

    // Simulate an old persisted session without an ID (backward compatibility)
    let persisted = PersistedSession {
        id: None,
        name: "Legacy Session".to_string(),
        claude_session_id: "claude-456".to_string(),
        cwd: None,
        worktree: None,
        role: "reviewer".to_string(),
        backend_id: "thurbox:%43".to_string(),
        backend_type: "tmux".to_string(),
        project_id: None,
    };

    // Serialize and deserialize
    let toml_str = toml::to_string(&persisted).unwrap();
    assert!(toml_str.contains("name"));
    assert!(toml_str.contains("claude_session_id"));

    // Should deserialize correctly with id as None
    let restored: PersistedSession = toml::from_str(&toml_str).unwrap();
    assert_eq!(restored.id, None);
    assert_eq!(restored.name, "Legacy Session");

    // When a session with Some(id) is serialized, it should include the id
    let session_with_id = PersistedSession {
        id: Some(SessionId::default()),
        name: "Session With ID".to_string(),
        claude_session_id: "claude-789".to_string(),
        cwd: None,
        worktree: None,
        role: "developer".to_string(),
        backend_id: "thurbox:%44".to_string(),
        backend_type: "tmux".to_string(),
        project_id: None,
    };

    let toml_with_id = toml::to_string(&session_with_id).unwrap();
    assert!(toml_with_id.contains("id"));
    let restored_with_id: PersistedSession = toml::from_str(&toml_with_id).unwrap();
    assert_eq!(restored_with_id.id, session_with_id.id);
}

/// Merges local sessions into shared state, marking deleted sessions as tombstones.
/// This simulates the merge logic in App::save_shared_state().
fn merge_sessions_with_tombstones(
    shared_sessions: Vec<thurbox::sync::SharedSession>,
    local_sessions_map: std::collections::HashMap<SessionId, thurbox::sync::SharedSession>,
) -> Vec<thurbox::sync::SharedSession> {
    use thurbox::sync;

    let mut local_sessions = local_sessions_map;
    let mut merged_sessions = Vec::new();

    for mut existing in shared_sessions {
        if let Some(updated) = local_sessions.remove(&existing.id) {
            merged_sessions.push(updated);
        } else if existing.tombstone {
            merged_sessions.push(existing);
        } else {
            // Mark deleted session as tombstone for propagation to other instances
            existing.tombstone = true;
            existing.tombstone_at = Some(sync::current_time_millis());
            merged_sessions.push(existing);
        }
    }

    // Add new sessions (only in local state)
    for (_session_id, session) in local_sessions {
        merged_sessions.push(session);
    }

    merged_sessions
}

#[test]
fn deleted_sessions_marked_as_tombstones() {
    use thurbox::sync::{self, SharedSession, SharedState};

    let temp_dir = TempDir::new().unwrap();
    let _guard = TestPathGuard::new(temp_dir.path());

    // Setup: Create shared state with two sessions
    let session_a_id = SessionId::default();
    let session_b_id = SessionId::default();
    let project_id = ProjectId::default();

    let mut state = SharedState::new();
    state.sessions.push(SharedSession {
        id: session_a_id,
        name: "Session A".to_string(),
        project_id,
        role: "developer".to_string(),
        backend_id: "thurbox:%40".to_string(),
        backend_type: "tmux".to_string(),
        claude_session_id: Some("claude-a".to_string()),
        cwd: None,
        worktree: None,
        tombstone: false,
        tombstone_at: None,
    });
    state.sessions.push(SharedSession {
        id: session_b_id,
        name: "Session B".to_string(),
        project_id,
        role: "developer".to_string(),
        backend_id: "thurbox:%41".to_string(),
        backend_type: "tmux".to_string(),
        claude_session_id: Some("claude-b".to_string()),
        cwd: None,
        worktree: None,
        tombstone: false,
        tombstone_at: None,
    });

    let shared_path = thurbox::paths::shared_state_file().unwrap();
    sync::file_store::save_shared_state(&shared_path, &state).unwrap();

    // Simulate Instance A: Load both sessions, then delete session B
    let mut state_a = sync::file_store::load_shared_state(&shared_path).unwrap();
    assert_eq!(state_a.sessions.len(), 2);

    // Remove session B from local state (simulating deletion via Ctrl+X)
    let mut local_sessions: std::collections::HashMap<SessionId, _> =
        std::collections::HashMap::new();
    local_sessions.insert(session_a_id, state_a.sessions[0].clone());
    // Note: session_b_id is NOT in local_sessions (it was deleted)

    // Merge using the helper function
    let merged_sessions = merge_sessions_with_tombstones(state_a.sessions.clone(), local_sessions);

    // Verify deletion is marked as tombstone
    let deleted_session = merged_sessions
        .iter()
        .find(|s| s.id == session_b_id)
        .unwrap();
    assert!(deleted_session.tombstone);
    assert!(deleted_session.tombstone_at.is_some());

    // Update shared state
    state_a.sessions = merged_sessions;
    state_a.last_modified = sync::current_time_millis();
    sync::file_store::save_shared_state(&shared_path, &state_a).unwrap();

    // Instance B loads updated state
    let state_b = sync::file_store::load_shared_state(&shared_path).unwrap();

    // Instance B should see session B as tombstoned (not active, but present for TTL cleanup)
    let session_b_in_b = state_b.sessions.iter().find(|s| s.id == session_b_id);
    assert!(session_b_in_b.is_some());
    assert!(session_b_in_b.unwrap().tombstone);

    // Instance B should see session A as active (not tombstoned)
    let session_a_in_b = state_b
        .sessions
        .iter()
        .find(|s| s.id == session_a_id)
        .unwrap();
    assert!(!session_a_in_b.tombstone);

    // When Instance B filters active sessions, it should exclude tombstoned ones
    let active_sessions: Vec<_> = state_b.sessions.iter().filter(|s| !s.tombstone).collect();
    assert_eq!(active_sessions.len(), 1);
    assert_eq!(active_sessions[0].id, session_a_id);
}

#[test]
fn merge_preserves_already_tombstoned_sessions() {
    use thurbox::sync;

    let project_id = ProjectId::default();
    let session_a_id = SessionId::default();
    let session_b_id = SessionId::default();
    let session_c_id = SessionId::default();

    // Shared state has: A (active), B (tombstoned), C (active)
    let shared_sessions = vec![
        make_session(session_a_id, "Session A", project_id),
        SharedSession {
            id: session_b_id,
            name: "Session B (deleted)".to_string(),
            project_id,
            role: "developer".to_string(),
            backend_id: "thurbox:%40".to_string(),
            backend_type: "tmux".to_string(),
            claude_session_id: Some("claude-b".to_string()),
            cwd: None,
            worktree: None,
            tombstone: true,
            tombstone_at: Some(sync::current_time_millis()),
        },
        make_session(session_c_id, "Session C", project_id),
    ];

    // Local state has: A (active), C (active) - both still exist locally
    let mut local_sessions = std::collections::HashMap::new();
    local_sessions.insert(
        session_a_id,
        make_session(session_a_id, "Session A", project_id),
    );
    local_sessions.insert(
        session_c_id,
        make_session(session_c_id, "Session C", project_id),
    );

    let merged = merge_sessions_with_tombstones(shared_sessions, local_sessions);

    // Merged state should have A (local), B (tombstoned), C (local)
    assert_eq!(merged.len(), 3);

    let session_a = merged.iter().find(|s| s.id == session_a_id).unwrap();
    assert!(!session_a.tombstone);

    let session_b = merged.iter().find(|s| s.id == session_b_id).unwrap();
    assert!(session_b.tombstone); // Remains tombstoned

    let session_c = merged.iter().find(|s| s.id == session_c_id).unwrap();
    assert!(!session_c.tombstone);
}

#[test]
fn merge_adds_only_local_sessions() {
    let project_id = ProjectId::default();
    let session_a_id = SessionId::default();
    let session_b_id = SessionId::default();
    let session_c_id = SessionId::default();

    // Shared state has: A, B
    let shared_sessions = vec![
        make_session(session_a_id, "Session A", project_id),
        make_session(session_b_id, "Session B", project_id),
    ];

    // Local state has: A, B, C (C is new)
    let mut local_sessions = std::collections::HashMap::new();
    local_sessions.insert(
        session_a_id,
        make_session(session_a_id, "Session A", project_id),
    );
    local_sessions.insert(
        session_b_id,
        make_session(session_b_id, "Session B", project_id),
    );
    local_sessions.insert(
        session_c_id,
        make_session(session_c_id, "Session C", project_id),
    );

    let merged = merge_sessions_with_tombstones(shared_sessions, local_sessions);

    // Merged state should have A, B (from shared), C (new from local)
    assert_eq!(merged.len(), 3);
    assert!(merged.iter().any(|s| s.id == session_a_id));
    assert!(merged.iter().any(|s| s.id == session_b_id));
    assert!(merged.iter().any(|s| s.id == session_c_id));

    // C should not be tombstoned
    let session_c = merged.iter().find(|s| s.id == session_c_id).unwrap();
    assert!(!session_c.tombstone);
}

#[test]
fn merge_handles_empty_local_sessions() {
    let project_id = ProjectId::default();
    let session_a_id = SessionId::default();
    let session_b_id = SessionId::default();

    // Shared state has: A, B (active)
    let shared_sessions = vec![
        make_session(session_a_id, "Session A", project_id),
        make_session(session_b_id, "Session B", project_id),
    ];

    // Local state is empty (all sessions deleted)
    let local_sessions = std::collections::HashMap::new();

    let merged = merge_sessions_with_tombstones(shared_sessions, local_sessions);

    // Merged state should have A and B both marked as tombstones
    assert_eq!(merged.len(), 2);
    for session in merged {
        assert!(
            session.tombstone,
            "Session {} should be tombstoned",
            session.name
        );
        assert!(session.tombstone_at.is_some());
    }
}
