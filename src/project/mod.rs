use std::fmt;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::session::{RoleConfig, SessionId};

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

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ProjectConfig {
    pub name: String,
    pub repos: Vec<PathBuf>,
    #[serde(default)]
    pub roles: Vec<RoleConfig>,
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
        let id = config.deterministic_id();
        Self {
            id,
            config,
            session_ids: Vec::new(),
            is_default: false,
        }
    }

    pub fn new_default(config: ProjectConfig) -> Self {
        let id = config.deterministic_id();
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
    }
}

#[derive(Debug, Deserialize, Serialize)]
struct ConfigFile {
    #[serde(default)]
    projects: Vec<ProjectConfig>,
}

/// Load project configurations from `~/.config/thurbox/config.toml`.
/// Returns an empty list if the file doesn't exist or can't be parsed.
pub fn load_project_configs() -> Vec<ProjectConfig> {
    let Some(path) = crate::paths::config_file() else {
        return Vec::new();
    };

    let contents = match std::fs::read_to_string(&path) {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };

    match toml::from_str::<ConfigFile>(&contents) {
        Ok(config) => config.projects,
        Err(e) => {
            tracing::warn!("Failed to parse config at {}: {e}", path.display());
            Vec::new()
        }
    }
}

/// Save project configurations to `~/.config/thurbox/config.toml`.
pub fn save_project_configs(projects: &[ProjectConfig]) -> std::io::Result<()> {
    let path = crate::paths::config_file().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "Could not determine config path",
        )
    })?;

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let config = ConfigFile {
        projects: projects.to_vec(),
    };
    let contents = toml::to_string_pretty(&config)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;

    std::fs::write(&path, contents)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::RolePermissions;

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
    fn deserialize_config_file() {
        let toml_str = r#"
[[projects]]
name = "thurbox"
repos = ["/home/user/repos/thurbox"]

[[projects]]
name = "other"
repos = ["/home/user/repos/other"]
"#;
        let config: ConfigFile = toml::from_str(toml_str).unwrap();
        assert_eq!(config.projects.len(), 2);
        assert_eq!(config.projects[0].name, "thurbox");
        assert_eq!(
            config.projects[0].repos,
            vec![PathBuf::from("/home/user/repos/thurbox")]
        );
        assert_eq!(config.projects[1].name, "other");
    }

    #[test]
    fn deserialize_empty_config() {
        let toml_str = "";
        let config: ConfigFile = toml::from_str(toml_str).unwrap();
        assert!(config.projects.is_empty());
    }

    #[test]
    fn serialize_roundtrip() {
        let configs = vec![
            ProjectConfig {
                name: "alpha".to_string(),
                repos: vec![PathBuf::from("/tmp/alpha")],
                roles: Vec::new(),
            },
            ProjectConfig {
                name: "beta".to_string(),
                repos: vec![PathBuf::from("/tmp/beta")],
                roles: Vec::new(),
            },
        ];

        let file = ConfigFile { projects: configs };
        let serialized = toml::to_string_pretty(&file).unwrap();
        let deserialized: ConfigFile = toml::from_str(&serialized).unwrap();

        assert_eq!(deserialized.projects.len(), 2);
        assert_eq!(deserialized.projects[0].name, "alpha");
        assert_eq!(
            deserialized.projects[0].repos,
            vec![PathBuf::from("/tmp/alpha")]
        );
        assert_eq!(deserialized.projects[1].name, "beta");
    }

    #[test]
    fn serialize_format_is_toml_array() {
        let configs = vec![ProjectConfig {
            name: "test".to_string(),
            repos: vec![PathBuf::from("/tmp/test")],
            roles: Vec::new(),
        }];

        let file = ConfigFile { projects: configs };
        let serialized = toml::to_string_pretty(&file).unwrap();
        assert!(serialized.contains("[[projects]]"));
        assert!(serialized.contains("name = \"test\""));
        assert!(serialized.contains("/tmp/test"));
    }

    #[test]
    fn project_info_new_has_empty_sessions() {
        let config = ProjectConfig {
            name: "test".to_string(),
            repos: vec![PathBuf::from("/tmp/test")],
            roles: Vec::new(),
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
    fn deserialize_config_with_roles() {
        let toml_str = r#"
[[projects]]
name = "myapp"
repos = ["/tmp/myapp"]

[[projects.roles]]
name = "developer"
description = "Full access"

[[projects.roles]]
name = "reviewer"
description = "Read-only"
permission_mode = "plan"
allowed_tools = ["Read", "Bash(git:*)"]
"#;
        let config: ConfigFile = toml::from_str(toml_str).unwrap();
        assert_eq!(config.projects.len(), 1);
        assert_eq!(config.projects[0].roles.len(), 2);
        assert_eq!(config.projects[0].roles[0].name, "developer");
        assert_eq!(config.projects[0].roles[1].name, "reviewer");
        assert_eq!(
            config.projects[0].roles[1].permissions.permission_mode,
            Some("plan".to_string())
        );
    }

    #[test]
    fn serialize_roundtrip_with_roles() {
        let configs = vec![ProjectConfig {
            name: "myapp".to_string(),
            repos: vec![PathBuf::from("/tmp/myapp")],
            roles: vec![
                RoleConfig {
                    name: "developer".to_string(),
                    description: "Full access".to_string(),
                    permissions: RolePermissions::default(),
                },
                RoleConfig {
                    name: "reviewer".to_string(),
                    description: "Read-only".to_string(),
                    permissions: RolePermissions {
                        permission_mode: Some("plan".to_string()),
                        allowed_tools: vec!["Read".to_string()],
                        ..RolePermissions::default()
                    },
                },
            ],
        }];

        let file = ConfigFile { projects: configs };
        let serialized = toml::to_string_pretty(&file).unwrap();
        let deserialized: ConfigFile = toml::from_str(&serialized).unwrap();

        assert_eq!(deserialized.projects.len(), 1);
        assert_eq!(deserialized.projects[0].roles.len(), 2);
        assert_eq!(deserialized.projects[0].roles[0].name, "developer");
        assert_eq!(deserialized.projects[0].roles[1].name, "reviewer");
        assert_eq!(
            deserialized.projects[0].roles[1]
                .permissions
                .permission_mode,
            Some("plan".to_string())
        );
    }

    #[test]
    fn deserialize_config_without_roles_backward_compat() {
        let toml_str = r#"
[[projects]]
name = "old-project"
repos = ["/tmp/old"]
"#;
        let config: ConfigFile = toml::from_str(toml_str).unwrap();
        assert_eq!(config.projects.len(), 1);
        assert!(config.projects[0].roles.is_empty());
    }

    #[test]
    fn project_id_is_deterministic() {
        let config = ProjectConfig {
            name: "Test Project".to_string(),
            repos: vec![PathBuf::from("/repo1"), PathBuf::from("/repo2")],
            roles: Vec::new(),
        };

        let id1 = config.deterministic_id();
        let id2 = config.deterministic_id();

        // Same config should always produce the same ID
        assert_eq!(id1, id2);
    }

    #[test]
    fn different_project_names_have_different_ids() {
        let config1 = ProjectConfig {
            name: "Project A".to_string(),
            repos: vec![PathBuf::from("/repo")],
            roles: Vec::new(),
        };
        let config2 = ProjectConfig {
            name: "Project B".to_string(),
            repos: vec![PathBuf::from("/repo")],
            roles: Vec::new(),
        };

        let id1 = config1.deterministic_id();
        let id2 = config2.deterministic_id();

        // Different names should produce different IDs
        assert_ne!(id1, id2);
    }

    #[test]
    fn project_info_uses_deterministic_id() {
        let config = ProjectConfig {
            name: "Test Project".to_string(),
            repos: vec![PathBuf::from("/repo")],
            roles: Vec::new(),
        };

        let info = ProjectInfo::new(config.clone());
        let expected_id = config.deterministic_id();

        assert_eq!(info.id, expected_id);
    }

    #[test]
    fn multiple_instances_derive_same_project_id() {
        let config = ProjectConfig {
            name: "Shared Project".to_string(),
            repos: vec![PathBuf::from("/shared/repo")],
            roles: Vec::new(),
        };

        // Simulate Instance A loading the config
        let info_a = ProjectInfo::new(config.clone());

        // Simulate Instance B loading the same config
        let info_b = ProjectInfo::new(config.clone());

        // Both instances should derive the same project ID
        assert_eq!(info_a.id, info_b.id);
    }
}
