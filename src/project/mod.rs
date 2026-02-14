use std::fmt;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::session::{PersistedState, RoleConfig, SessionId};

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
    #[serde(default)]
    pub roles: Vec<RoleConfig>,
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

fn data_dir() -> Option<PathBuf> {
    // Prefer $XDG_DATA_HOME, fall back to $HOME/.local/share
    if let Some(xdg) = std::env::var_os("XDG_DATA_HOME") {
        let mut p = PathBuf::from(xdg);
        p.push("thurbox");
        return Some(p);
    }

    std::env::var_os("HOME").map(|h| {
        let mut p = PathBuf::from(h);
        p.push(".local");
        p.push("share");
        p.push("thurbox");
        p
    })
}

fn state_path() -> Option<PathBuf> {
    data_dir().map(|mut p| {
        p.push("state.toml");
        p
    })
}

/// Load persisted session state from `$XDG_DATA_HOME/thurbox/state.toml`.
/// Returns default (empty) state if the file doesn't exist or can't be parsed.
pub fn load_session_state() -> PersistedState {
    let Some(path) = state_path() else {
        return PersistedState::default();
    };

    let contents = match std::fs::read_to_string(&path) {
        Ok(c) => c,
        Err(_) => return PersistedState::default(),
    };

    match toml::from_str::<PersistedState>(&contents) {
        Ok(state) => state,
        Err(e) => {
            tracing::warn!("Failed to parse state at {}: {e}", path.display());
            PersistedState::default()
        }
    }
}

/// Save persisted session state to `$XDG_DATA_HOME/thurbox/state.toml`.
pub fn save_session_state(state: &PersistedState) -> std::io::Result<()> {
    let path = state_path().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "Could not determine state path",
        )
    })?;

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let contents = toml::to_string_pretty(state)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;

    std::fs::write(&path, contents)
}

/// Remove the persisted state file after successful restore.
pub fn clear_session_state() -> std::io::Result<()> {
    let Some(path) = state_path() else {
        return Ok(());
    };
    match std::fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
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
    fn persisted_state_roundtrip() {
        use crate::session::{PersistedSession, PersistedState, PersistedWorktree};

        let state = PersistedState {
            sessions: vec![
                PersistedSession {
                    name: "Session 1".to_string(),
                    claude_session_id: "abc-123".to_string(),
                    cwd: Some(PathBuf::from("/tmp/repo")),
                    worktree: None,
                    role: "developer".to_string(),
                    backend_id: String::new(),
                    backend_type: String::new(),
                },
                PersistedSession {
                    name: "Session 2".to_string(),
                    claude_session_id: "def-456".to_string(),
                    cwd: Some(PathBuf::from("/tmp/wt")),
                    worktree: Some(PersistedWorktree {
                        repo_path: PathBuf::from("/tmp/repo"),
                        worktree_path: PathBuf::from("/tmp/wt"),
                        branch: "feat".to_string(),
                    }),
                    role: "reviewer".to_string(),
                    backend_id: String::new(),
                    backend_type: String::new(),
                },
            ],
            session_counter: 2,
        };

        let serialized = toml::to_string_pretty(&state).unwrap();
        let deserialized: PersistedState = toml::from_str(&serialized).unwrap();

        assert_eq!(deserialized.sessions.len(), 2);
        assert_eq!(deserialized.session_counter, 2);
        assert_eq!(deserialized.sessions[0].name, "Session 1");
        assert_eq!(deserialized.sessions[0].claude_session_id, "abc-123");
        assert!(deserialized.sessions[0].worktree.is_none());
        assert_eq!(deserialized.sessions[1].name, "Session 2");
        assert!(deserialized.sessions[1].worktree.is_some());
        let wt = deserialized.sessions[1].worktree.as_ref().unwrap();
        assert_eq!(wt.branch, "feat");
    }

    #[test]
    fn persisted_state_empty_deserializes() {
        let state: PersistedState = toml::from_str("").unwrap();
        assert!(state.sessions.is_empty());
        assert_eq!(state.session_counter, 0);
    }

    #[test]
    fn persisted_state_session_without_cwd() {
        use crate::session::{PersistedSession, PersistedState};

        let state = PersistedState {
            sessions: vec![PersistedSession {
                name: "Session 1".to_string(),
                claude_session_id: "abc-123".to_string(),
                cwd: None,
                worktree: None,
                role: "developer".to_string(),
                backend_id: String::new(),
                backend_type: String::new(),
            }],
            session_counter: 1,
        };

        let serialized = toml::to_string_pretty(&state).unwrap();
        let deserialized: PersistedState = toml::from_str(&serialized).unwrap();

        assert_eq!(deserialized.sessions.len(), 1);
        assert!(deserialized.sessions[0].cwd.is_none());
        assert!(deserialized.sessions[0].worktree.is_none());
    }

    #[test]
    fn persisted_state_deserializes_from_manual_toml() {
        use crate::session::PersistedState;

        let toml_str = r#"
session_counter = 3

[[sessions]]
name = "Session 1"
claude_session_id = "abc-123-def-456"
cwd = "/home/user/repos/app"

[[sessions]]
name = "Session 2"
claude_session_id = "ghi-789-jkl-012"
cwd = "/home/user/repos/app/.git/thurbox-worktrees/feat-login"

[sessions.worktree]
repo_path = "/home/user/repos/app"
worktree_path = "/home/user/repos/app/.git/thurbox-worktrees/feat-login"
branch = "feat-login"
"#;
        let state: PersistedState = toml::from_str(toml_str).unwrap();
        assert_eq!(state.session_counter, 3);
        assert_eq!(state.sessions.len(), 2);
        assert_eq!(state.sessions[0].name, "Session 1");
        assert_eq!(
            state.sessions[0].cwd,
            Some(PathBuf::from("/home/user/repos/app"))
        );
        assert!(state.sessions[0].worktree.is_none());
        assert_eq!(state.sessions[1].name, "Session 2");
        let wt = state.sessions[1].worktree.as_ref().unwrap();
        assert_eq!(wt.branch, "feat-login");
        assert_eq!(wt.repo_path, PathBuf::from("/home/user/repos/app"));
    }

    #[test]
    fn persisted_state_missing_counter_defaults_to_zero() {
        use crate::session::PersistedState;

        let toml_str = r#"
[[sessions]]
name = "Session 1"
claude_session_id = "abc-123"
"#;
        let state: PersistedState = toml::from_str(toml_str).unwrap();
        assert_eq!(state.session_counter, 0);
        assert_eq!(state.sessions.len(), 1);
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
}
