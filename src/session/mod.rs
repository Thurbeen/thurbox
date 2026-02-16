use std::fmt;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Default role name assigned when no explicit role is configured.
pub const DEFAULT_ROLE_NAME: &str = "developer";

/// Validated role name type that prevents invalid states.
/// Role names must be non-empty and at most 64 characters.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct RoleName(String);

impl RoleName {
    /// Create a new role name with validation.
    ///
    /// # Errors
    ///
    /// Returns `RoleNameError` if:
    /// - The name is empty or only whitespace
    /// - The name exceeds 64 characters
    pub fn new(name: impl Into<String>) -> Result<Self, RoleNameError> {
        let name = name.into();
        let trimmed = name.trim();

        if trimmed.is_empty() {
            return Err(RoleNameError::Empty);
        }

        if trimmed.len() > 64 {
            return Err(RoleNameError::TooLong);
        }

        Ok(Self(trimmed.to_string()))
    }

    /// Get the role name as a string slice.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Consume the RoleName and return the inner String.
    pub fn into_string(self) -> String {
        self.0
    }
}

impl fmt::Display for RoleName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::str::FromStr for RoleName {
    type Err = RoleNameError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::new(s)
    }
}

/// Error type for invalid role names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RoleNameError {
    /// Role name is empty or only whitespace.
    Empty,
    /// Role name exceeds 64 characters.
    TooLong,
}

impl fmt::Display for RoleNameError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => write!(f, "Role name cannot be empty"),
            Self::TooLong => write!(f, "Role name cannot exceed 64 characters"),
        }
    }
}

impl std::error::Error for RoleNameError {}

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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
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

impl std::str::FromStr for SessionId {
    type Err = uuid::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(Self(Uuid::parse_str(s)?))
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
    pub backend_id: Option<String>,
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
            backend_id: None,
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    #[test]
    fn role_name_valid() {
        let name = RoleName::new("developer").unwrap();
        assert_eq!(name.as_str(), "developer");
    }

    #[test]
    fn role_name_with_spaces_trimmed() {
        let name = RoleName::new("  admin  ").unwrap();
        assert_eq!(name.as_str(), "admin");
    }

    #[test]
    fn role_name_empty_rejected() {
        assert_eq!(RoleName::new(""), Err(RoleNameError::Empty));
        assert_eq!(RoleName::new("   "), Err(RoleNameError::Empty));
    }

    #[test]
    fn role_name_too_long_rejected() {
        let long_name = "a".repeat(65);
        assert_eq!(RoleName::new(long_name), Err(RoleNameError::TooLong));
    }

    #[test]
    fn role_name_max_length_accepted() {
        let max_name = "a".repeat(64);
        let name = RoleName::new(max_name.clone()).unwrap();
        assert_eq!(name.as_str(), max_name);
    }

    #[test]
    fn role_name_display() {
        let name = RoleName::new("test_role").unwrap();
        assert_eq!(name.to_string(), "test_role");
    }

    #[test]
    fn role_name_from_str() {
        let name = RoleName::from_str("editor").unwrap();
        assert_eq!(name.as_str(), "editor");

        let err = RoleName::from_str("");
        assert_eq!(err, Err(RoleNameError::Empty));
    }

    #[test]
    fn role_name_into_string() {
        let name = RoleName::new("maintainer").unwrap();
        let owned = name.into_string();
        assert_eq!(owned, "maintainer");
    }

    #[test]
    fn role_name_hash() {
        use std::collections::HashSet;
        let name1 = RoleName::new("reviewer").unwrap();
        let name2 = RoleName::new("reviewer").unwrap();

        let mut set = HashSet::new();
        set.insert(name1);
        // Same value should not increase size due to hash equality
        set.insert(name2);
        assert_eq!(set.len(), 1);
    }

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
    fn session_info_new_has_no_backend_id() {
        let info = SessionInfo::new("Test".to_string());
        assert!(info.backend_id.is_none());
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
}
