use std::collections::HashMap;

use crate::session::SessionId;

use super::state::{SharedSession, SharedState};

/// Represents the delta (changes) between two shared states.
///
/// Used to communicate to an instance what changed externally
/// so it can update its local view accordingly.
#[derive(Debug, Default, Clone)]
pub struct StateDelta {
    /// Sessions that were created by other instances.
    pub added_sessions: Vec<SharedSession>,

    /// Session IDs that were deleted by other instances.
    pub removed_sessions: Vec<SessionId>,

    /// Sessions that were updated (metadata changed).
    pub updated_sessions: Vec<SharedSession>,

    /// Latest session counter from external state.
    /// Should be merged using max(local, external).
    pub counter_increment: usize,
}

impl StateDelta {
    /// Compute the delta between two states.
    ///
    /// Determines which sessions were added, removed, or updated
    /// by comparing the old state (what we knew) with the new state
    /// (what other instances know).
    pub fn compute(old: &SharedState, new: &SharedState) -> Self {
        // Build lookup maps, excluding tombstoned sessions
        let old_session_map: HashMap<SessionId, &SharedSession> = old
            .sessions
            .iter()
            .filter(|s| !s.tombstone)
            .map(|s| (s.id, s))
            .collect();

        let new_session_map: HashMap<SessionId, &SharedSession> = new
            .sessions
            .iter()
            .filter(|s| !s.tombstone)
            .map(|s| (s.id, s))
            .collect();

        let mut delta = StateDelta::default();

        // Sessions: Added (in new but not in old)
        for (id, session) in &new_session_map {
            if !old_session_map.contains_key(id) {
                delta.added_sessions.push((*session).clone());
            }
        }

        // Sessions: Removed (in old but not in new or tombstoned in new)
        for id in old_session_map.keys() {
            if !new_session_map.contains_key(id) {
                delta.removed_sessions.push(*id);
            }
        }

        // Sessions: Updated (in both but key fields changed)
        for (id, new_session) in &new_session_map {
            if let Some(old_session) = old_session_map.get(id) {
                if session_changed(old_session, new_session) {
                    delta.updated_sessions.push((*new_session).clone());
                }
            }
        }

        delta.counter_increment = new.session_counter;

        delta
    }

    /// Check if this delta has any meaningful changes.
    pub fn is_empty(&self) -> bool {
        self.added_sessions.is_empty()
            && self.removed_sessions.is_empty()
            && self.updated_sessions.is_empty()
    }
}

/// Check if a session's key metadata changed.
/// Ignores tombstone state since delta computation filters those out.
fn session_changed(old: &SharedSession, new: &SharedSession) -> bool {
    old.name != new.name
        || old.role != new.role
        || old.backend_id != new.backend_id
        || old.backend_type != new.backend_type
        || old.agent_session_id != new.agent_session_id
        || old.cwd != new.cwd
        || old.additional_dirs != new.additional_dirs
        || old.worktrees != new.worktrees
        || old.model != new.model
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn make_session(id: SessionId, name: &str) -> SharedSession {
        SharedSession {
            id,
            name: name.to_string(),
            role: "developer".to_string(),
            backend_id: "thurbox:@0".to_string(),
            backend_type: "tmux".to_string(),
            agent_session_id: None,
            cwd: None,
            additional_dirs: Vec::new(),
            worktrees: Vec::new(),
            shell_backend_id: None,
            tombstone: false,
            tombstone_at: None,
            model: None,
        }
    }

    #[test]
    fn empty_delta_when_no_changes() {
        let state = SharedState::new();
        let delta = StateDelta::compute(&state, &state);
        assert!(delta.is_empty());
    }

    #[test]
    fn added_sessions_detected() {
        let old_state = SharedState::new();

        let mut new_state = SharedState::new();
        new_state
            .sessions
            .push(make_session(SessionId::default(), "New Session"));

        let delta = StateDelta::compute(&old_state, &new_state);

        assert_eq!(delta.added_sessions.len(), 1);
        assert_eq!(delta.added_sessions[0].name, "New Session");
        assert!(delta.removed_sessions.is_empty());
        assert!(delta.updated_sessions.is_empty());
    }

    #[test]
    fn removed_sessions_detected() {
        let session_id = SessionId::default();
        let mut old_state = SharedState::new();
        old_state
            .sessions
            .push(make_session(session_id, "Old Session"));

        let new_state = SharedState::new();

        let delta = StateDelta::compute(&old_state, &new_state);

        assert!(delta.added_sessions.is_empty());
        assert_eq!(delta.removed_sessions.len(), 1);
        assert_eq!(delta.removed_sessions[0], session_id);
        assert!(delta.updated_sessions.is_empty());
    }

    #[test]
    fn updated_sessions_detected() {
        let session_id = SessionId::default();

        let mut old_state = SharedState::new();
        old_state
            .sessions
            .push(make_session(session_id, "Session A"));

        let mut new_state = SharedState::new();
        new_state
            .sessions
            .push(make_session(session_id, "Session A (renamed)"));

        let delta = StateDelta::compute(&old_state, &new_state);

        assert!(delta.added_sessions.is_empty());
        assert!(delta.removed_sessions.is_empty());
        assert_eq!(delta.updated_sessions.len(), 1);
        assert_eq!(delta.updated_sessions[0].name, "Session A (renamed)");
    }

    #[test]
    fn tombstoned_sessions_ignored() {
        let session_id = SessionId::default();

        let mut old_state = SharedState::new();
        old_state.sessions.push(make_session(session_id, "Session"));

        let mut new_state = SharedState::new();
        let mut s = make_session(session_id, "Session");
        s.tombstone = true;
        s.tombstone_at = Some(0);
        new_state.sessions.push(s);

        let delta = StateDelta::compute(&old_state, &new_state);

        assert!(delta.added_sessions.is_empty());
        assert_eq!(delta.removed_sessions.len(), 1);
        assert!(delta.updated_sessions.is_empty());
    }

    #[test]
    fn counter_increment_tracked() {
        let mut old_state = SharedState::new();
        old_state.session_counter = 5;

        let mut new_state = SharedState::new();
        new_state.session_counter = 10;

        let delta = StateDelta::compute(&old_state, &new_state);

        assert_eq!(delta.counter_increment, 10);
    }

    #[test]
    fn session_changed_detects_role_change() {
        let session_id = SessionId::default();

        let mut old_state = SharedState::new();
        old_state.sessions.push(make_session(session_id, "Session"));

        let mut new_state = SharedState::new();
        let mut s = make_session(session_id, "Session");
        s.role = "reviewer".to_string();
        new_state.sessions.push(s);

        let delta = StateDelta::compute(&old_state, &new_state);

        assert_eq!(delta.updated_sessions.len(), 1);
        assert_eq!(delta.updated_sessions[0].role, "reviewer");
    }

    #[test]
    fn session_changed_detects_cwd_change() {
        let session_id = SessionId::default();

        let mut old_state = SharedState::new();
        let mut s1 = make_session(session_id, "Session");
        s1.cwd = Some(PathBuf::from("/home/user"));
        old_state.sessions.push(s1);

        let mut new_state = SharedState::new();
        let mut s2 = make_session(session_id, "Session");
        s2.cwd = Some(PathBuf::from("/home/user/project"));
        new_state.sessions.push(s2);

        let delta = StateDelta::compute(&old_state, &new_state);

        assert_eq!(delta.updated_sessions.len(), 1);
    }

    #[test]
    fn no_update_when_session_unchanged() {
        let session_id = SessionId::default();

        let mut old_state = SharedState::new();
        let mut s1 = make_session(session_id, "Session");
        s1.agent_session_id = Some("claude-123".to_string());
        s1.cwd = Some(PathBuf::from("/home/user"));
        old_state.sessions.push(s1);

        let mut new_state = SharedState::new();
        let mut s2 = make_session(session_id, "Session");
        s2.agent_session_id = Some("claude-123".to_string());
        s2.cwd = Some(PathBuf::from("/home/user"));
        new_state.sessions.push(s2);

        let delta = StateDelta::compute(&old_state, &new_state);

        assert!(delta.is_empty());
    }

    #[test]
    fn session_changed_detects_additional_dirs_change() {
        let session_id = SessionId::default();

        let mut old_state = SharedState::new();
        let mut s1 = make_session(session_id, "S");
        s1.cwd = Some(PathBuf::from("/repo1"));
        s1.additional_dirs = vec![PathBuf::from("/repo2")];
        old_state.sessions.push(s1);

        let mut new_state = SharedState::new();
        let mut s2 = make_session(session_id, "S");
        s2.cwd = Some(PathBuf::from("/repo1"));
        s2.additional_dirs = vec![PathBuf::from("/repo2"), PathBuf::from("/repo3")];
        new_state.sessions.push(s2);

        let delta = StateDelta::compute(&old_state, &new_state);

        assert!(!delta.is_empty());
        assert_eq!(delta.updated_sessions.len(), 1);
    }
}
