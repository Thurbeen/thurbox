use std::fmt;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::session::SessionId;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ProjectId(Uuid);

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
}

pub struct ProjectInfo {
    pub id: ProjectId,
    pub config: ProjectConfig,
    pub session_ids: Vec<SessionId>,
    pub is_default: bool,
}

impl ProjectInfo {
    pub fn new(config: ProjectConfig) -> Self {
        Self {
            id: ProjectId::default(),
            config,
            session_ids: Vec::new(),
            is_default: false,
        }
    }

    pub fn new_default(config: ProjectConfig) -> Self {
        Self {
            id: ProjectId::default(),
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
    let Some(path) = config_path() else {
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
    let path = config_path().ok_or_else(|| {
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

fn config_path() -> Option<PathBuf> {
    // Prefer $XDG_CONFIG_HOME, fall back to $HOME/.config
    if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME") {
        let mut p = PathBuf::from(xdg);
        p.push("thurbox");
        p.push("config.toml");
        return Some(p);
    }

    std::env::var_os("HOME").map(|h| {
        let mut p = PathBuf::from(h);
        p.push(".config");
        p.push("thurbox");
        p.push("config.toml");
        p
    })
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
            },
            ProjectConfig {
                name: "beta".to_string(),
                repos: vec![PathBuf::from("/tmp/beta")],
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
}
