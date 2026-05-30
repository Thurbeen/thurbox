pub mod agent_def;
pub mod keybindings;
pub mod theme_config;

pub use agent_def::{AgentDef, AgentRegistry};
pub use keybindings::{Action, Category, KeyBindings, KeyChord};
pub use theme_config::{ThemePalette, ThemePreset};

use std::collections::HashMap;
use std::fmt;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Default agent name used when none is configured. Matches the `default`
/// entry of the seeded `agents.toml`; the live default comes from the loaded
/// [`AgentRegistry`].
pub const DEFAULT_AGENT_NAME: &str = "claude";

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

/// Agent metrics collected from the Claude CLI statusline mechanism.
#[derive(Debug, Clone, Default)]
pub struct AgentMetrics {
    pub model_id: Option<String>,
    pub model_display_name: Option<String>,
    pub total_cost_usd: Option<f64>,
    pub total_duration_ms: Option<u64>,
    pub total_api_duration_ms: Option<u64>,
    pub total_lines_added: Option<u64>,
    pub total_lines_removed: Option<u64>,
    pub total_input_tokens: Option<u64>,
    pub total_output_tokens: Option<u64>,
    pub context_window_size: Option<u64>,
    pub used_percentage: Option<u8>,
    pub current_input_tokens: Option<u64>,
    pub current_output_tokens: Option<u64>,
    pub cache_creation_input_tokens: Option<u64>,
    pub cache_read_input_tokens: Option<u64>,
    pub cli_version: Option<String>,
}

pub struct SessionInfo {
    pub id: SessionId,
    pub name: String,
    pub status: SessionStatus,
    /// Name of the coding agent driving this session (e.g. `"claude"`).
    pub agent: String,
    pub worktrees: Vec<WorktreeInfo>,
    pub agent_session_id: Option<String>,
    pub cwd: Option<PathBuf>,
    pub additional_dirs: Vec<PathBuf>,
    pub backend_id: Option<String>,
    pub shell_backend_id: Option<String>,
    /// Agent metrics from the agent's statusline (Claude only).
    pub agent_metrics: Option<AgentMetrics>,
    /// Whether this is the admin session (internal, never user-visible).
    pub is_admin: bool,
    /// Cached display names for repos, resolved from git remote or directory name.
    /// Order: worktree repos first, then non-worktree additional dirs.
    /// Populated by the app layer at spawn/restore time.
    pub repo_display_names: Vec<String>,
}

impl SessionInfo {
    pub fn new(name: String) -> Self {
        Self {
            id: SessionId::default(),
            name,
            status: SessionStatus::Busy,
            agent: DEFAULT_AGENT_NAME.to_string(),
            worktrees: Vec::new(),
            agent_session_id: None,
            cwd: None,
            additional_dirs: Vec::new(),
            backend_id: None,
            shell_backend_id: None,
            agent_metrics: None,
            is_admin: false,
            repo_display_names: Vec::new(),
        }
    }
}

/// A queued command for a session, inserted by MCP and processed by the TUI.
#[derive(Debug, Clone)]
pub struct SessionCommand {
    pub id: i64,
    pub session_id: SessionId,
    pub command: String,
    pub created_at: u64,
}

/// A time-scheduled command for a session, inserted by MCP and executed by the TUI tick loop.
#[derive(Debug, Clone)]
pub struct ScheduledCommand {
    pub id: i64,
    pub session_id: SessionId,
    pub command_text: String,
    pub scheduled_at: u64,
    pub created_at: u64,
    pub executed_at: Option<u64>,
    pub cancelled_at: Option<u64>,
}

#[derive(Debug, Clone, Default)]
pub struct SessionConfig {
    /// Resume an existing agent session (process restart of a known session).
    pub resume_session_id: Option<String>,
    /// Pin a session id on a fresh spawn (agents that support it).
    pub agent_session_id: Option<String>,
    pub cwd: Option<PathBuf>,
    /// Name of the agent definition to launch (looked up in the registry).
    pub agent: String,
    /// Fork from an existing session's conversation (agents that support it).
    pub fork_session_id: Option<String>,
    /// Environment variables injected into the spawned session process
    /// (thurbox-internal: session id, metrics dir, etc.).
    pub env: HashMap<String, String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_id_display_is_uuid_format() {
        let id = SessionId::default();
        let display = id.to_string();
        assert_eq!(display.len(), 36);
        assert_eq!(display.chars().filter(|&c| c == '-').count(), 4);
    }

    #[test]
    fn session_id_default_is_unique() {
        assert_ne!(SessionId::default(), SessionId::default());
    }

    #[test]
    fn session_status_display_and_icon() {
        assert_eq!(SessionStatus::Busy.to_string(), "Busy");
        assert_eq!(SessionStatus::Idle.icon(), "○");
        assert_eq!(SessionStatus::Error.icon(), "✗");
    }

    #[test]
    fn session_info_new_defaults() {
        let info = SessionInfo::new("Test".to_string());
        assert_eq!(info.name, "Test");
        assert_eq!(info.status, SessionStatus::Busy);
        assert_eq!(info.agent, DEFAULT_AGENT_NAME);
        assert!(info.worktrees.is_empty());
        assert!(info.agent_session_id.is_none());
        assert!(info.cwd.is_none());
        assert!(info.backend_id.is_none());
    }

    #[test]
    fn default_agent_name_is_claude() {
        assert_eq!(DEFAULT_AGENT_NAME, "claude");
    }

    #[test]
    fn session_config_default_is_empty() {
        let config = SessionConfig::default();
        assert!(config.resume_session_id.is_none());
        assert!(config.agent_session_id.is_none());
        assert!(config.cwd.is_none());
        assert_eq!(config.agent, "");
        assert!(config.env.is_empty());
    }

    #[test]
    fn worktree_info_stores_fields() {
        let wt = WorktreeInfo {
            repo_path: PathBuf::from("/repo"),
            worktree_path: PathBuf::from("/repo/.git/thurbox-worktrees/feat"),
            branch: "feat".to_string(),
        };
        assert_eq!(wt.repo_path, PathBuf::from("/repo"));
        assert_eq!(wt.branch, "feat");
    }
}
