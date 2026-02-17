use std::fmt;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::session::{RoleConfig, SessionId};

// Keep serde on ProjectId for backward compat (used in session/mod.rs serialization)

/// Namespace UUID for deriving deterministic project IDs.
/// This is the namespace used for v5 UUID generation from project configs.
/// Value is SHA1(UUID("6ba7b810-9dad-11d1-80b4-00c04fd430c8"), "thurbox:projects")
const PROJECT_ID_NAMESPACE: uuid::Uuid = uuid::Uuid::from_bytes([
    0x6e, 0xb5, 0x79, 0xc4, 0xca, 0xee, 0x5c, 0xba, 0x8d, 0x4c, 0x23, 0x33, 0x26, 0x37, 0x78, 0xb9,
]);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ProjectId(Uuid);

impl ProjectId {
    /// Get the inner UUID representation.
    pub fn as_uuid(&self) -> Uuid {
        self.0
    }

    /// Create a ProjectId from a raw UUID.
    pub fn from_uuid(uuid: Uuid) -> Self {
        Self(uuid)
    }
}

impl Default for ProjectId {
    fn default() -> Self {
        Self(Uuid::new_v4())
    }
}

impl fmt::Display for ProjectId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[derive(Debug, Clone)]
pub struct ProjectConfig {
    pub name: String,
    pub repos: Vec<PathBuf>,
    pub roles: Vec<RoleConfig>,
    /// Stable project ID preserved across renames. When present, this takes
    /// precedence over the name-derived deterministic ID.
    pub id: Option<String>,
}

impl ProjectConfig {
    /// Derive a deterministic project ID from this config.
    ///
    /// Uses the project name as the basis for a v5 UUID, ensuring that
    /// the same project name always produces the same ID across instances.
    /// This is critical for multi-instance session synchronization.
    pub fn deterministic_id(&self) -> ProjectId {
        ProjectId(Uuid::new_v5(&PROJECT_ID_NAMESPACE, self.name.as_bytes()))
    }

    /// Return the effective project ID: the persisted `id` if present,
    /// otherwise the name-derived deterministic ID.
    ///
    /// After a project rename, `id` holds the original UUID so that
    /// sessions and DB entries stay correctly associated.
    pub fn effective_id(&self) -> ProjectId {
        self.id
            .as_ref()
            .and_then(|s| s.parse::<Uuid>().ok())
            .map(ProjectId::from_uuid)
            .unwrap_or_else(|| self.deterministic_id())
    }
}

#[derive(Clone)]
pub struct ProjectInfo {
    pub id: ProjectId,
    pub config: ProjectConfig,
    pub session_ids: Vec<SessionId>,
    pub is_default: bool,
}

impl ProjectInfo {
    pub fn new(config: ProjectConfig) -> Self {
        let id = config.effective_id();
        Self {
            id,
            config,
            session_ids: Vec::new(),
            is_default: false,
        }
    }

    pub fn new_default(config: ProjectConfig) -> Self {
        let id = config.effective_id();
        Self {
            id,
            config,
            session_ids: Vec::new(),
            is_default: true,
        }
    }
}

/// Create a default project config using the current working directory.
pub fn create_default_project() -> ProjectConfig {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    ProjectConfig {
        name: "Default".to_string(),
        repos: vec![cwd],
        roles: Vec::new(),
        id: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_id_display_is_uuid_format() {
        let id = ProjectId::default();
        let display = id.to_string();
        assert_eq!(display.len(), 36);
        assert_eq!(display.chars().filter(|&c| c == '-').count(), 4);
    }

    #[test]
    fn project_id_default_is_unique() {
        let id1 = ProjectId::default();
        let id2 = ProjectId::default();
        assert_ne!(id1, id2);
    }

    #[test]
    fn project_info_new_has_empty_sessions() {
        let config = ProjectConfig {
            name: "test".to_string(),
            repos: vec![PathBuf::from("/tmp/test")],
            roles: Vec::new(),
            id: None,
        };
        let info = ProjectInfo::new(config);
        assert!(info.session_ids.is_empty());
        assert_eq!(info.config.name, "test");
        assert!(!info.is_default);
    }

    #[test]
    fn project_info_new_default_sets_flag() {
        let config = create_default_project();
        let info = ProjectInfo::new_default(config);
        assert!(info.is_default);
        assert_eq!(info.config.name, "Default");
        assert!(!info.config.repos.is_empty());
    }

    #[test]
    fn project_id_is_deterministic() {
        let config = ProjectConfig {
            name: "Test Project".to_string(),
            repos: vec![PathBuf::from("/repo1"), PathBuf::from("/repo2")],
            roles: Vec::new(),
            id: None,
        };

        let id1 = config.deterministic_id();
        let id2 = config.deterministic_id();

        assert_eq!(id1, id2);
    }

    #[test]
    fn different_project_names_have_different_ids() {
        let config1 = ProjectConfig {
            name: "Project A".to_string(),
            repos: vec![PathBuf::from("/repo")],
            roles: Vec::new(),
            id: None,
        };
        let config2 = ProjectConfig {
            name: "Project B".to_string(),
            repos: vec![PathBuf::from("/repo")],
            roles: Vec::new(),
            id: None,
        };

        assert_ne!(config1.deterministic_id(), config2.deterministic_id());
    }

    #[test]
    fn project_info_uses_deterministic_id() {
        let config = ProjectConfig {
            name: "Test Project".to_string(),
            repos: vec![PathBuf::from("/repo")],
            roles: Vec::new(),
            id: None,
        };

        let info = ProjectInfo::new(config.clone());
        assert_eq!(info.id, config.deterministic_id());
    }

    #[test]
    fn multiple_instances_derive_same_project_id() {
        let config = ProjectConfig {
            name: "Shared Project".to_string(),
            repos: vec![PathBuf::from("/shared/repo")],
            roles: Vec::new(),
            id: None,
        };

        let info_a = ProjectInfo::new(config.clone());
        let info_b = ProjectInfo::new(config.clone());

        assert_eq!(info_a.id, info_b.id);
    }

    #[test]
    fn effective_id_falls_back_to_deterministic() {
        let config = ProjectConfig {
            name: "Test".to_string(),
            repos: vec![PathBuf::from("/repo")],
            roles: Vec::new(),
            id: None,
        };
        assert_eq!(config.effective_id(), config.deterministic_id());
    }

    #[test]
    fn effective_id_uses_persisted_id() {
        let original_config = ProjectConfig {
            name: "OldName".to_string(),
            repos: vec![],
            roles: Vec::new(),
            id: None,
        };
        let original_id = original_config.deterministic_id();

        let renamed_config = ProjectConfig {
            name: "NewName".to_string(),
            repos: vec![],
            roles: Vec::new(),
            id: Some(original_id.to_string()),
        };

        assert_eq!(renamed_config.effective_id(), original_id);
        assert_ne!(renamed_config.deterministic_id(), original_id);
    }
}
