use std::fmt;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Default role name assigned when no explicit role is configured.
pub const DEFAULT_ROLE_NAME: &str = "developer";

/// Permission flags passed to the Claude CLI when spawning a session.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct RolePermissions {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub permission_mode: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allowed_tools: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub disallowed_tools: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub append_system_prompt: Option<String>,
}

/// A named role definition.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RoleConfig {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(flatten)]
    pub permissions: RolePermissions,
}

#[derive(Debug, Clone)]
pub struct WorktreeInfo {
    pub repo_path: PathBuf,
    pub worktree_path: PathBuf,
    pub branch: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SessionId(Uuid);

impl Default for SessionId {
    fn default() -> Self {
        Self(Uuid::new_v4())
    }
}

impl fmt::Display for SessionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionStatus {
    Busy,
    Waiting,
    Idle,
    Error,
}

impl SessionStatus {
    pub fn icon(self) -> &'static str {
        match self {
            Self::Busy => "●",
            Self::Waiting => "◉",
            Self::Idle => "○",
            Self::Error => "✗",
        }
    }
}

impl fmt::Display for SessionStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Busy => write!(f, "Busy"),
            Self::Waiting => write!(f, "Waiting"),
            Self::Idle => write!(f, "Idle"),
            Self::Error => write!(f, "Error"),
        }
    }
}

pub struct SessionInfo {
    pub id: SessionId,
    pub name: String,
    pub status: SessionStatus,
    pub role: String,
    pub worktree: Option<WorktreeInfo>,
    pub claude_session_id: Option<String>,
    pub cwd: Option<PathBuf>,
}

impl SessionInfo {
    pub fn new(name: String) -> Self {
        Self {
            id: SessionId::default(),
            name,
            status: SessionStatus::Busy,
            role: DEFAULT_ROLE_NAME.to_string(),
            worktree: None,
            claude_session_id: None,
            cwd: None,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct SessionConfig {
    pub resume_session_id: Option<String>,
    pub claude_session_id: Option<String>,
    pub cwd: Option<PathBuf>,
    pub role: String,
    pub permissions: RolePermissions,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersistedSession {
    pub name: String,
    pub claude_session_id: String,
    pub cwd: Option<PathBuf>,
    pub worktree: Option<PersistedWorktree>,
    #[serde(default)]
    pub role: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersistedWorktree {
    pub repo_path: PathBuf,
    pub worktree_path: PathBuf,
    pub branch: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PersistedState {
    /// Legacy single-instance fields (kept for backward compatibility).
    #[serde(default)]
    pub sessions: Vec<PersistedSession>,
    #[serde(default)]
    pub session_counter: usize,
    /// Multi-instance state: each running instance has its own entry.
    #[serde(default)]
    pub instances: Vec<PersistedInstance>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersistedInstance {
    pub instance_id: String,
    #[serde(default)]
    pub session_counter: usize,
    #[serde(default)]
    pub sessions: Vec<PersistedSession>,
}

impl PersistedState {
    /// Migrate legacy single-instance format into the multi-instance format.
    /// If the legacy `sessions` field is populated and `instances` is empty,
    /// move the legacy sessions into a single instance entry.
    pub fn migrate_legacy(&mut self, instance_id: &str) {
        if !self.sessions.is_empty() && self.instances.is_empty() {
            self.instances.push(PersistedInstance {
                instance_id: instance_id.to_string(),
                session_counter: self.session_counter,
                sessions: std::mem::take(&mut self.sessions),
            });
            self.session_counter = 0;
        }
    }

    /// Get sessions for a specific instance.
    pub fn instance_sessions(&self, instance_id: &str) -> &[PersistedSession] {
        self.instances
            .iter()
            .find(|i| i.instance_id == instance_id)
            .map(|i| i.sessions.as_slice())
            .unwrap_or(&[])
    }

    /// Get sessions from all *other* instances.
    pub fn other_instances_sessions(&self, my_instance_id: &str) -> Vec<&PersistedSession> {
        self.instances
            .iter()
            .filter(|i| i.instance_id != my_instance_id)
            .flat_map(|i| &i.sessions)
            .collect()
    }

    /// Update (or insert) the entry for a specific instance.
    pub fn upsert_instance(&mut self, instance: PersistedInstance) {
        if let Some(existing) = self
            .instances
            .iter_mut()
            .find(|i| i.instance_id == instance.instance_id)
        {
            *existing = instance;
        } else {
            self.instances.push(instance);
        }
    }

    /// Remove the entry for a specific instance (e.g., on clean shutdown).
    pub fn remove_instance(&mut self, instance_id: &str) {
        self.instances.retain(|i| i.instance_id != instance_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_id_display_is_uuid_format() {
        let id = SessionId::default();
        let display = id.to_string();
        // UUID v4 format: 8-4-4-4-12 hex chars
        assert_eq!(display.len(), 36);
        assert_eq!(display.chars().filter(|&c| c == '-').count(), 4);
    }

    #[test]
    fn session_id_default_is_unique() {
        let id1 = SessionId::default();
        let id2 = SessionId::default();
        assert_ne!(id1, id2);
    }

    #[test]
    fn session_status_display() {
        assert_eq!(SessionStatus::Busy.to_string(), "Busy");
        assert_eq!(SessionStatus::Waiting.to_string(), "Waiting");
        assert_eq!(SessionStatus::Idle.to_string(), "Idle");
        assert_eq!(SessionStatus::Error.to_string(), "Error");
    }

    #[test]
    fn session_status_icon() {
        assert_eq!(SessionStatus::Busy.icon(), "●");
        assert_eq!(SessionStatus::Waiting.icon(), "◉");
        assert_eq!(SessionStatus::Idle.icon(), "○");
        assert_eq!(SessionStatus::Error.icon(), "✗");
    }

    #[test]
    fn session_info_new_starts_busy() {
        let info = SessionInfo::new("Test".to_string());
        assert_eq!(info.name, "Test");
        assert_eq!(info.status, SessionStatus::Busy);
    }

    #[test]
    fn session_info_new_has_no_worktree() {
        let info = SessionInfo::new("Test".to_string());
        assert!(info.worktree.is_none());
    }

    #[test]
    fn session_info_new_has_no_claude_session_id() {
        let info = SessionInfo::new("Test".to_string());
        assert!(info.claude_session_id.is_none());
    }

    #[test]
    fn session_info_new_has_no_cwd() {
        let info = SessionInfo::new("Test".to_string());
        assert!(info.cwd.is_none());
    }

    #[test]
    fn session_info_new_has_developer_role() {
        let info = SessionInfo::new("Test".to_string());
        assert_eq!(info.role, DEFAULT_ROLE_NAME);
    }

    #[test]
    fn default_role_name_is_developer() {
        assert_eq!(DEFAULT_ROLE_NAME, "developer");
    }

    #[test]
    fn session_config_default_has_all_none() {
        let config = SessionConfig::default();
        assert!(config.resume_session_id.is_none());
        assert!(config.claude_session_id.is_none());
        assert!(config.cwd.is_none());
        assert_eq!(config.role, "");
        assert_eq!(config.permissions, RolePermissions::default());
    }

    #[test]
    fn role_permissions_default_is_empty() {
        let perms = RolePermissions::default();
        assert!(perms.permission_mode.is_none());
        assert!(perms.allowed_tools.is_empty());
        assert!(perms.disallowed_tools.is_empty());
        assert!(perms.tools.is_none());
        assert!(perms.append_system_prompt.is_none());
    }

    #[test]
    fn role_permissions_serde_roundtrip() {
        let perms = RolePermissions {
            permission_mode: Some("plan".to_string()),
            allowed_tools: vec!["Read".to_string(), "Bash(git:*)".to_string()],
            disallowed_tools: vec![],
            tools: None,
            append_system_prompt: Some("Be careful".to_string()),
        };
        let serialized = toml::to_string_pretty(&perms).unwrap();
        let deserialized: RolePermissions = toml::from_str(&serialized).unwrap();
        assert_eq!(perms, deserialized);
    }

    #[test]
    fn role_permissions_all_fields_serde_roundtrip() {
        let perms = RolePermissions {
            permission_mode: Some("plan".to_string()),
            allowed_tools: vec!["Read".to_string(), "Bash(git:*)".to_string()],
            disallowed_tools: vec!["Edit".to_string()],
            tools: Some("default".to_string()),
            append_system_prompt: Some("Be careful".to_string()),
        };
        let serialized = toml::to_string_pretty(&perms).unwrap();
        let deserialized: RolePermissions = toml::from_str(&serialized).unwrap();
        assert_eq!(perms, deserialized);
    }

    #[test]
    fn role_config_serde_roundtrip() {
        let role = RoleConfig {
            name: "reviewer".to_string(),
            description: "Read-only code review".to_string(),
            permissions: RolePermissions {
                permission_mode: Some("plan".to_string()),
                allowed_tools: vec!["Read".to_string()],
                ..RolePermissions::default()
            },
        };
        let serialized = toml::to_string_pretty(&role).unwrap();
        let deserialized: RoleConfig = toml::from_str(&serialized).unwrap();
        assert_eq!(role, deserialized);
    }

    #[test]
    fn role_config_flatten_includes_permission_fields() {
        let role = RoleConfig {
            name: "test".to_string(),
            description: String::new(),
            permissions: RolePermissions {
                permission_mode: Some("plan".to_string()),
                ..RolePermissions::default()
            },
        };
        let serialized = toml::to_string_pretty(&role).unwrap();
        assert!(serialized.contains("permission_mode"));
        assert!(serialized.contains("plan"));
    }

    #[test]
    fn persisted_session_with_role() {
        let toml_str = r#"
name = "Session 1"
claude_session_id = "abc-123"
role = "reviewer"
"#;
        let session: PersistedSession = toml::from_str(toml_str).unwrap();
        assert_eq!(session.role, "reviewer");
    }

    #[test]
    fn persisted_session_backward_compat_no_role() {
        let toml_str = r#"
name = "Session 1"
claude_session_id = "abc-123"
"#;
        let session: PersistedSession = toml::from_str(toml_str).unwrap();
        assert_eq!(session.name, "Session 1");
        assert_eq!(session.role, "");
    }

    #[test]
    fn worktree_info_stores_fields() {
        let wt = WorktreeInfo {
            repo_path: PathBuf::from("/repo"),
            worktree_path: PathBuf::from("/repo/.git/thurbox-worktrees/feat"),
            branch: "feat".to_string(),
        };
        assert_eq!(wt.repo_path, PathBuf::from("/repo"));
        assert_eq!(
            wt.worktree_path,
            PathBuf::from("/repo/.git/thurbox-worktrees/feat")
        );
        assert_eq!(wt.branch, "feat");
    }

    // --- Multi-instance PersistedState tests ---

    #[test]
    fn persisted_state_upsert_instance_adds_new() {
        let mut state = PersistedState::default();
        let instance = PersistedInstance {
            instance_id: "inst-1".to_string(),
            session_counter: 3,
            sessions: vec![PersistedSession {
                name: "S1".to_string(),
                claude_session_id: "abc".to_string(),
                cwd: None,
                worktree: None,
                role: "developer".to_string(),
            }],
        };
        state.upsert_instance(instance);
        assert_eq!(state.instances.len(), 1);
        assert_eq!(state.instances[0].instance_id, "inst-1");
        assert_eq!(state.instances[0].sessions.len(), 1);
    }

    #[test]
    fn persisted_state_upsert_instance_updates_existing() {
        let mut state = PersistedState::default();
        state.upsert_instance(PersistedInstance {
            instance_id: "inst-1".to_string(),
            session_counter: 1,
            sessions: vec![],
        });
        state.upsert_instance(PersistedInstance {
            instance_id: "inst-1".to_string(),
            session_counter: 5,
            sessions: vec![PersistedSession {
                name: "Updated".to_string(),
                claude_session_id: "xyz".to_string(),
                cwd: None,
                worktree: None,
                role: String::new(),
            }],
        });
        assert_eq!(state.instances.len(), 1);
        assert_eq!(state.instances[0].session_counter, 5);
        assert_eq!(state.instances[0].sessions[0].name, "Updated");
    }

    #[test]
    fn persisted_state_remove_instance() {
        let mut state = PersistedState::default();
        state.upsert_instance(PersistedInstance {
            instance_id: "inst-1".to_string(),
            session_counter: 1,
            sessions: vec![],
        });
        state.upsert_instance(PersistedInstance {
            instance_id: "inst-2".to_string(),
            session_counter: 2,
            sessions: vec![],
        });
        state.remove_instance("inst-1");
        assert_eq!(state.instances.len(), 1);
        assert_eq!(state.instances[0].instance_id, "inst-2");
    }

    #[test]
    fn persisted_state_remove_nonexistent_instance_is_noop() {
        let mut state = PersistedState::default();
        state.remove_instance("nonexistent");
        assert!(state.instances.is_empty());
    }

    #[test]
    fn persisted_state_migrate_legacy() {
        let mut state = PersistedState {
            sessions: vec![PersistedSession {
                name: "Legacy".to_string(),
                claude_session_id: "legacy-id".to_string(),
                cwd: None,
                worktree: None,
                role: "developer".to_string(),
            }],
            session_counter: 3,
            ..PersistedState::default()
        };
        state.migrate_legacy("new-instance");
        assert!(state.sessions.is_empty());
        assert_eq!(state.session_counter, 0);
        assert_eq!(state.instances.len(), 1);
        assert_eq!(state.instances[0].instance_id, "new-instance");
        assert_eq!(state.instances[0].session_counter, 3);
        assert_eq!(state.instances[0].sessions[0].name, "Legacy");
    }

    #[test]
    fn persisted_state_migrate_legacy_noop_when_already_multi_instance() {
        let mut state = PersistedState::default();
        state.upsert_instance(PersistedInstance {
            instance_id: "existing".to_string(),
            session_counter: 1,
            sessions: vec![],
        });
        state.sessions.push(PersistedSession {
            name: "Stale".to_string(),
            claude_session_id: "stale".to_string(),
            cwd: None,
            worktree: None,
            role: String::new(),
        });
        state.migrate_legacy("new");
        // Should not migrate because instances is already populated.
        assert_eq!(state.instances.len(), 1);
        assert_eq!(state.instances[0].instance_id, "existing");
    }

    #[test]
    fn persisted_state_other_instances_sessions() {
        let mut state = PersistedState::default();
        state.upsert_instance(PersistedInstance {
            instance_id: "me".to_string(),
            session_counter: 1,
            sessions: vec![PersistedSession {
                name: "My Session".to_string(),
                claude_session_id: "mine".to_string(),
                cwd: None,
                worktree: None,
                role: String::new(),
            }],
        });
        state.upsert_instance(PersistedInstance {
            instance_id: "other".to_string(),
            session_counter: 2,
            sessions: vec![PersistedSession {
                name: "Other Session".to_string(),
                claude_session_id: "theirs".to_string(),
                cwd: None,
                worktree: None,
                role: String::new(),
            }],
        });

        let others = state.other_instances_sessions("me");
        assert_eq!(others.len(), 1);
        assert_eq!(others[0].name, "Other Session");
    }

    #[test]
    fn persisted_state_instance_sessions() {
        let mut state = PersistedState::default();
        state.upsert_instance(PersistedInstance {
            instance_id: "inst-1".to_string(),
            session_counter: 1,
            sessions: vec![PersistedSession {
                name: "S1".to_string(),
                claude_session_id: "id1".to_string(),
                cwd: None,
                worktree: None,
                role: String::new(),
            }],
        });

        assert_eq!(state.instance_sessions("inst-1").len(), 1);
        assert_eq!(state.instance_sessions("inst-1")[0].name, "S1");
        assert!(state.instance_sessions("nonexistent").is_empty());
    }

    #[test]
    fn persisted_state_multi_instance_serde_roundtrip() {
        let mut state = PersistedState::default();
        state.upsert_instance(PersistedInstance {
            instance_id: "inst-a".to_string(),
            session_counter: 2,
            sessions: vec![PersistedSession {
                name: "A1".to_string(),
                claude_session_id: "a1".to_string(),
                cwd: Some(PathBuf::from("/tmp/a")),
                worktree: None,
                role: "developer".to_string(),
            }],
        });
        state.upsert_instance(PersistedInstance {
            instance_id: "inst-b".to_string(),
            session_counter: 1,
            sessions: vec![],
        });

        let serialized = toml::to_string_pretty(&state).unwrap();
        let deserialized: PersistedState = toml::from_str(&serialized).unwrap();
        assert_eq!(deserialized.instances.len(), 2);
        assert_eq!(deserialized.instances[0].instance_id, "inst-a");
        assert_eq!(deserialized.instances[0].sessions.len(), 1);
        assert_eq!(deserialized.instances[1].instance_id, "inst-b");
        assert!(deserialized.instances[1].sessions.is_empty());
    }
}
