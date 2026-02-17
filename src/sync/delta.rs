use std::collections::HashMap;

use crate::project::ProjectId;
use crate::session::SessionId;

use super::state::{SharedProject, SharedSession, SharedState};

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

    /// Projects that were created by other instances.
    pub added_projects: Vec<SharedProject>,

    /// Project IDs that were deleted by other instances.
    pub removed_projects: Vec<ProjectId>,

    /// Projects that were updated (metadata changed).
    pub updated_projects: Vec<SharedProject>,

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

        // Build project lookup maps
        let old_project_map: HashMap<ProjectId, &SharedProject> =
            old.projects.iter().map(|p| (p.id, p)).collect();

        let new_project_map: HashMap<ProjectId, &SharedProject> =
            new.projects.iter().map(|p| (p.id, p)).collect();

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

        // Projects: Added (in new but not in old)
        for (id, project) in &new_project_map {
            if !old_project_map.contains_key(id) {
                delta.added_projects.push((*project).clone());
            }
        }

        // Projects: Removed (in old but not in new)
        for id in old_project_map.keys() {
            if !new_project_map.contains_key(id) {
                delta.removed_projects.push(*id);
            }
        }

        // Projects: Updated (in both but key fields changed)
        for (id, new_project) in &new_project_map {
            if let Some(old_project) = old_project_map.get(id) {
                if project_changed(old_project, new_project) {
                    delta.updated_projects.push((*new_project).clone());
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
            && self.added_projects.is_empty()
            && self.removed_projects.is_empty()
            && self.updated_projects.is_empty()
    }
}

/// Check if a session's key metadata changed.
/// Ignores tombstone state since delta computation filters those out.
fn session_changed(old: &SharedSession, new: &SharedSession) -> bool {
    old.name != new.name
        || old.project_id != new.project_id
        || old.role != new.role
        || old.backend_id != new.backend_id
        || old.backend_type != new.backend_type
        || old.claude_session_id != new.claude_session_id
        || old.cwd != new.cwd
        || old.additional_dirs != new.additional_dirs
        || old.worktree != new.worktree
}

/// Check if a project's key metadata changed.
fn project_changed(old: &SharedProject, new: &SharedProject) -> bool {
    old.name != new.name
        || old.repos != new.repos
        || old.roles != new.roles
        || old.mcp_servers != new.mcp_servers
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::ProjectId;
    use std::path::PathBuf;

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
        let session = SharedSession {
            id: SessionId::default(),
            name: "New Session".to_string(),
            project_id: ProjectId::default(),
            role: "developer".to_string(),
            backend_id: "thurbox:@0".to_string(),
            backend_type: "tmux".to_string(),
            claude_session_id: None,
            cwd: None,
            additional_dirs: Vec::new(),
            worktree: None,
            tombstone: false,
            tombstone_at: None,
        };
        new_state.sessions.push(session.clone());

        let delta = StateDelta::compute(&old_state, &new_state);

        assert_eq!(delta.added_sessions.len(), 1);
        assert_eq!(delta.added_sessions[0].name, "New Session");
        assert!(delta.removed_sessions.is_empty());
        assert!(delta.updated_sessions.is_empty());
    }

    #[test]
    fn removed_sessions_detected() {
        let mut old_state = SharedState::new();
        let session_id = SessionId::default();
        let session = SharedSession {
            id: session_id,
            name: "Old Session".to_string(),
            project_id: ProjectId::default(),
            role: "developer".to_string(),
            backend_id: "thurbox:@0".to_string(),
            backend_type: "tmux".to_string(),
            claude_session_id: None,
            cwd: None,
            additional_dirs: Vec::new(),
            worktree: None,
            tombstone: false,
            tombstone_at: None,
        };
        old_state.sessions.push(session);

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
        let old_session = SharedSession {
            id: session_id,
            name: "Session A".to_string(),
            project_id: ProjectId::default(),
            role: "developer".to_string(),
            backend_id: "thurbox:@0".to_string(),
            backend_type: "tmux".to_string(),
            claude_session_id: None,
            cwd: None,
            additional_dirs: Vec::new(),
            worktree: None,
            tombstone: false,
            tombstone_at: None,
        };
        old_state.sessions.push(old_session);

        let mut new_state = SharedState::new();
        let new_session = SharedSession {
            id: session_id,
            name: "Session A (renamed)".to_string(), // Changed
            project_id: ProjectId::default(),
            role: "developer".to_string(),
            backend_id: "thurbox:@0".to_string(),
            backend_type: "tmux".to_string(),
            claude_session_id: None,
            cwd: None,
            additional_dirs: Vec::new(),
            worktree: None,
            tombstone: false,
            tombstone_at: None,
        };
        new_state.sessions.push(new_session.clone());

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
        let old_session = SharedSession {
            id: session_id,
            name: "Session".to_string(),
            project_id: ProjectId::default(),
            role: "developer".to_string(),
            backend_id: "thurbox:@0".to_string(),
            backend_type: "tmux".to_string(),
            claude_session_id: None,
            cwd: None,
            additional_dirs: Vec::new(),
            worktree: None,
            tombstone: false,
            tombstone_at: None,
        };
        old_state.sessions.push(old_session);

        let mut new_state = SharedState::new();
        let new_session = SharedSession {
            id: session_id,
            name: "Session".to_string(),
            project_id: ProjectId::default(),
            role: "developer".to_string(),
            backend_id: "thurbox:@0".to_string(),
            backend_type: "tmux".to_string(),
            claude_session_id: None,
            cwd: None,
            additional_dirs: Vec::new(),
            worktree: None,
            tombstone: true, // Marked as deleted
            tombstone_at: Some(0),
        };
        new_state.sessions.push(new_session);

        let delta = StateDelta::compute(&old_state, &new_state);

        // Tombstone is treated as removal
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
    fn multiple_changes_in_single_delta() {
        let session1_id = SessionId::default();
        let session2_id = SessionId::default();
        let session3_id = SessionId::default();
        let project_id = ProjectId::default();

        let mut old_state = SharedState::new();
        old_state.session_counter = 2;
        old_state.sessions.push(SharedSession {
            id: session1_id,
            name: "Session 1".to_string(),
            project_id,
            role: "developer".to_string(),
            backend_id: "thurbox:@0".to_string(),
            backend_type: "tmux".to_string(),
            claude_session_id: None,
            cwd: None,
            additional_dirs: Vec::new(),
            worktree: None,
            tombstone: false,
            tombstone_at: None,
        });
        old_state.sessions.push(SharedSession {
            id: session2_id,
            name: "Session 2".to_string(),
            project_id,
            role: "developer".to_string(),
            backend_id: "thurbox:@0".to_string(),
            backend_type: "tmux".to_string(),
            claude_session_id: None,
            cwd: None,
            additional_dirs: Vec::new(),
            worktree: None,
            tombstone: false,
            tombstone_at: None,
        });

        let mut new_state = SharedState::new();
        new_state.session_counter = 4;
        // Session 1: kept
        new_state.sessions.push(SharedSession {
            id: session1_id,
            name: "Session 1".to_string(),
            project_id,
            role: "developer".to_string(),
            backend_id: "thurbox:@0".to_string(),
            backend_type: "tmux".to_string(),
            claude_session_id: None,
            cwd: None,
            additional_dirs: Vec::new(),
            worktree: None,
            tombstone: false,
            tombstone_at: None,
        });
        // Session 2: removed (tombstoned)
        new_state.sessions.push(SharedSession {
            id: session2_id,
            name: "Session 2".to_string(),
            project_id,
            role: "developer".to_string(),
            backend_id: "thurbox:@0".to_string(),
            backend_type: "tmux".to_string(),
            claude_session_id: None,
            cwd: None,
            additional_dirs: Vec::new(),
            worktree: None,
            tombstone: true,
            tombstone_at: Some(0),
        });
        // Session 3: added
        new_state.sessions.push(SharedSession {
            id: session3_id,
            name: "Session 3".to_string(),
            project_id,
            role: "developer".to_string(),
            backend_id: "thurbox:@0".to_string(),
            backend_type: "tmux".to_string(),
            claude_session_id: None,
            cwd: None,
            additional_dirs: Vec::new(),
            worktree: None,
            tombstone: false,
            tombstone_at: None,
        });

        let delta = StateDelta::compute(&old_state, &new_state);

        assert_eq!(delta.added_sessions.len(), 1);
        assert_eq!(delta.removed_sessions.len(), 1);
        assert!(delta.updated_sessions.is_empty());
        assert_eq!(delta.counter_increment, 4);
    }

    #[test]
    fn session_changed_detects_role_change() {
        let session_id = SessionId::default();
        let project_id = ProjectId::default();

        let mut old_state = SharedState::new();
        let old_session = SharedSession {
            id: session_id,
            name: "Session".to_string(),
            project_id,
            role: "developer".to_string(),
            backend_id: "thurbox:@0".to_string(),
            backend_type: "tmux".to_string(),
            claude_session_id: None,
            cwd: None,
            additional_dirs: Vec::new(),
            worktree: None,
            tombstone: false,
            tombstone_at: None,
        };
        old_state.sessions.push(old_session);

        let mut new_state = SharedState::new();
        let new_session = SharedSession {
            id: session_id,
            name: "Session".to_string(),
            project_id,
            role: "reviewer".to_string(), // Changed
            backend_id: "thurbox:@0".to_string(),
            backend_type: "tmux".to_string(),
            claude_session_id: None,
            cwd: None,
            additional_dirs: Vec::new(),
            worktree: None,
            tombstone: false,
            tombstone_at: None,
        };
        new_state.sessions.push(new_session);

        let delta = StateDelta::compute(&old_state, &new_state);

        assert!(delta.added_sessions.is_empty());
        assert!(delta.removed_sessions.is_empty());
        assert_eq!(delta.updated_sessions.len(), 1);
        assert_eq!(delta.updated_sessions[0].role, "reviewer");
    }

    #[test]
    fn session_changed_detects_claude_session_id_change() {
        let session_id = SessionId::default();
        let project_id = ProjectId::default();

        let mut old_state = SharedState::new();
        let old_session = SharedSession {
            id: session_id,
            name: "Session".to_string(),
            project_id,
            role: "developer".to_string(),
            backend_id: "thurbox:@0".to_string(),
            backend_type: "tmux".to_string(),
            claude_session_id: Some("claude-v1".to_string()),
            cwd: None,
            additional_dirs: Vec::new(),
            worktree: None,
            tombstone: false,
            tombstone_at: None,
        };
        old_state.sessions.push(old_session);

        let mut new_state = SharedState::new();
        let new_session = SharedSession {
            id: session_id,
            name: "Session".to_string(),
            project_id,
            role: "developer".to_string(),
            backend_id: "thurbox:@0".to_string(),
            backend_type: "tmux".to_string(),
            claude_session_id: Some("claude-v2".to_string()), // Changed
            cwd: None,
            additional_dirs: Vec::new(),
            worktree: None,
            tombstone: false,
            tombstone_at: None,
        };
        new_state.sessions.push(new_session);

        let delta = StateDelta::compute(&old_state, &new_state);

        assert_eq!(delta.updated_sessions.len(), 1);
    }

    #[test]
    fn session_changed_detects_cwd_change() {
        let session_id = SessionId::default();
        let project_id = ProjectId::default();

        let mut old_state = SharedState::new();
        let old_session = SharedSession {
            id: session_id,
            name: "Session".to_string(),
            project_id,
            role: "developer".to_string(),
            backend_id: "thurbox:@0".to_string(),
            backend_type: "tmux".to_string(),
            claude_session_id: None,
            cwd: Some(PathBuf::from("/home/user")),
            additional_dirs: Vec::new(),
            worktree: None,
            tombstone: false,
            tombstone_at: None,
        };
        old_state.sessions.push(old_session);

        let mut new_state = SharedState::new();
        let new_session = SharedSession {
            id: session_id,
            name: "Session".to_string(),
            project_id,
            role: "developer".to_string(),
            backend_id: "thurbox:@0".to_string(),
            backend_type: "tmux".to_string(),
            claude_session_id: None,
            cwd: Some(PathBuf::from("/home/user/project")), // Changed
            additional_dirs: Vec::new(),
            worktree: None,
            tombstone: false,
            tombstone_at: None,
        };
        new_state.sessions.push(new_session);

        let delta = StateDelta::compute(&old_state, &new_state);

        assert_eq!(delta.updated_sessions.len(), 1);
    }

    #[test]
    fn session_changed_detects_worktree_change() {
        use crate::sync::SharedWorktree;

        let session_id = SessionId::default();
        let project_id = ProjectId::default();

        let mut old_state = SharedState::new();
        let old_session = SharedSession {
            id: session_id,
            name: "Session".to_string(),
            project_id,
            role: "developer".to_string(),
            backend_id: "thurbox:@0".to_string(),
            backend_type: "tmux".to_string(),
            claude_session_id: None,
            cwd: None,
            additional_dirs: Vec::new(),
            worktree: Some(SharedWorktree {
                repo_path: PathBuf::from("/repo"),
                worktree_path: PathBuf::from("/repo/.git/worktrees/old"),
                branch: "old-branch".to_string(),
            }),
            tombstone: false,
            tombstone_at: None,
        };
        old_state.sessions.push(old_session);

        let mut new_state = SharedState::new();
        let new_session = SharedSession {
            id: session_id,
            name: "Session".to_string(),
            project_id,
            role: "developer".to_string(),
            backend_id: "thurbox:@0".to_string(),
            backend_type: "tmux".to_string(),
            claude_session_id: None,
            cwd: None,
            additional_dirs: Vec::new(),
            worktree: Some(SharedWorktree {
                repo_path: PathBuf::from("/repo"),
                worktree_path: PathBuf::from("/repo/.git/worktrees/new"),
                branch: "new-branch".to_string(),
            }),
            tombstone: false,
            tombstone_at: None,
        };
        new_state.sessions.push(new_session);

        let delta = StateDelta::compute(&old_state, &new_state);

        assert_eq!(delta.updated_sessions.len(), 1);
    }

    #[test]
    fn session_changed_detects_backend_type_change() {
        let session_id = SessionId::default();
        let project_id = ProjectId::default();

        let mut old_state = SharedState::new();
        let old_session = SharedSession {
            id: session_id,
            name: "Session".to_string(),
            project_id,
            role: "developer".to_string(),
            backend_id: "thurbox:@0".to_string(),
            backend_type: "tmux".to_string(),
            claude_session_id: None,
            cwd: None,
            additional_dirs: Vec::new(),
            worktree: None,
            tombstone: false,
            tombstone_at: None,
        };
        old_state.sessions.push(old_session);

        let mut new_state = SharedState::new();
        let new_session = SharedSession {
            id: session_id,
            name: "Session".to_string(),
            project_id,
            role: "developer".to_string(),
            backend_id: "thurbox:@0".to_string(),
            backend_type: "ssh".to_string(), // Changed
            claude_session_id: None,
            cwd: None,
            additional_dirs: Vec::new(),
            worktree: None,
            tombstone: false,
            tombstone_at: None,
        };
        new_state.sessions.push(new_session);

        let delta = StateDelta::compute(&old_state, &new_state);

        assert_eq!(delta.updated_sessions.len(), 1);
    }

    #[test]
    fn no_update_when_session_unchanged() {
        let session_id = SessionId::default();
        let project_id = ProjectId::default();

        let mut old_state = SharedState::new();
        let old_session = SharedSession {
            id: session_id,
            name: "Session".to_string(),
            project_id,
            role: "developer".to_string(),
            backend_id: "thurbox:@0".to_string(),
            backend_type: "tmux".to_string(),
            claude_session_id: Some("claude-123".to_string()),
            cwd: Some(PathBuf::from("/home/user")),
            additional_dirs: Vec::new(),
            worktree: None,
            tombstone: false,
            tombstone_at: None,
        };
        old_state.sessions.push(old_session);

        let mut new_state = SharedState::new();
        let new_session = SharedSession {
            id: session_id,
            name: "Session".to_string(),
            project_id,
            role: "developer".to_string(),
            backend_id: "thurbox:@0".to_string(),
            backend_type: "tmux".to_string(),
            claude_session_id: Some("claude-123".to_string()),
            cwd: Some(PathBuf::from("/home/user")),
            additional_dirs: Vec::new(),
            worktree: None,
            tombstone: false,
            tombstone_at: None,
        };
        new_state.sessions.push(new_session);

        let delta = StateDelta::compute(&old_state, &new_state);

        assert!(delta.is_empty());
        assert_eq!(delta.updated_sessions.len(), 0);
    }

    #[test]
    fn session_changed_detects_additional_dirs_change() {
        let session_id = SessionId::default();
        let project_id = ProjectId::default();

        let mut old_state = SharedState::new();
        old_state.sessions.push(SharedSession {
            id: session_id,
            name: "S".to_string(),
            project_id,
            role: "developer".to_string(),
            backend_id: "thurbox:@0".to_string(),
            backend_type: "tmux".to_string(),
            claude_session_id: None,
            cwd: Some(PathBuf::from("/repo1")),
            additional_dirs: vec![PathBuf::from("/repo2")],
            worktree: None,
            tombstone: false,
            tombstone_at: None,
        });

        let mut new_state = SharedState::new();
        new_state.sessions.push(SharedSession {
            id: session_id,
            name: "S".to_string(),
            project_id,
            role: "developer".to_string(),
            backend_id: "thurbox:@0".to_string(),
            backend_type: "tmux".to_string(),
            claude_session_id: None,
            cwd: Some(PathBuf::from("/repo1")),
            additional_dirs: vec![PathBuf::from("/repo2"), PathBuf::from("/repo3")],
            worktree: None,
            tombstone: false,
            tombstone_at: None,
        });

        let delta = StateDelta::compute(&old_state, &new_state);

        assert!(!delta.is_empty());
        assert_eq!(delta.updated_sessions.len(), 1);
    }

    #[test]
    fn project_changed_detects_mcp_servers_change() {
        use crate::session::McpServerConfig;

        let pid = ProjectId::default();

        let mut old_state = SharedState::new();
        old_state.projects.push(SharedProject {
            id: pid,
            name: "proj".to_string(),
            repos: vec![PathBuf::from("/repo")],
            roles: vec![],
            mcp_servers: vec![],
        });

        let mut new_state = SharedState::new();
        new_state.projects.push(SharedProject {
            id: pid,
            name: "proj".to_string(),
            repos: vec![PathBuf::from("/repo")],
            roles: vec![],
            mcp_servers: vec![McpServerConfig {
                name: "fs".to_string(),
                command: "npx".to_string(),
                args: vec![],
                env: std::collections::HashMap::new(),
            }],
        });

        let delta = StateDelta::compute(&old_state, &new_state);

        assert!(!delta.is_empty());
        assert_eq!(delta.updated_projects.len(), 1);
    }
}
