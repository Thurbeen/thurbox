pub mod agent_def;
pub mod automation;
pub mod extension_def;
pub mod host_def;
pub mod keybindings;
pub mod message;
pub mod settings;
pub mod task;
pub mod theme_config;

pub use agent_def::{AgentDef, AgentRegistry};
pub use automation::{
    parse_hhmm, preset_to_cron, Automation, AutomationAction, AutomationRun, AutomationRunStatus,
    AutomationSchedule, ExtraRepo, SchedulePreset,
};
pub use extension_def::{
    ExtensionAutomation, ExtensionDef, ExtensionFile, ExtensionSession, ExtensionSymlink,
};
pub use host_def::{is_ssh_backend, HostDef, HostRegistry, SSH_BACKEND_PREFIX};
pub use keybindings::{Action, KeyBindings, KeyChord, KeyContext};
pub use message::SessionMessage;
pub use task::{Task, TaskStatus, SOURCE_LOCAL};
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

/// SQLite `metadata` key used to signal "focus this session" between
/// processes. The `notifications` module writes it from the OS notification
/// click handler; the running TUI reads + clears it on each tick via
/// [`crate::storage::Database::take_pending_focus_session_id`]. Defined here
/// in the pure-data layer so both sides reference one source of truth
/// without crossing module boundaries.
pub const PENDING_FOCUS_SESSION_ID_KEY: &str = "pending_focus_session_id";

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
    /// The agent signalled it finished or needs input (bell / OSC 9 / OSC 777).
    /// Latched until the user looks at the session; sorts to the top of the list.
    Attention,
}

impl SessionStatus {
    /// A status glyph chosen for **shape** distinctiveness, not just colour, so
    /// the state survives in greyscale / for colour-blind users: filled circle
    /// (busy) vs. diamond (waiting) vs. hollow circle (idle) vs. cross (error)
    /// vs. triangle (attention).
    pub fn icon(self) -> &'static str {
        match self {
            Self::Busy => "●",
            Self::Waiting => "◆",
            Self::Idle => "○",
            Self::Error => "✗",
            Self::Attention => "▲",
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
            Self::Attention => write!(f, "Needs attention"),
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

/// Real git state for a session's worktree(s), computed by the app/git layer
/// and surfaced in the info panel. Aggregated across all of a session's
/// worktrees. Agent-neutral (derived from git, not the agent CLI).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GitStats {
    pub files_changed: usize,
    pub insertions: usize,
    pub deletions: usize,
    /// Whether the worktree has uncommitted changes.
    pub dirty: bool,
    /// Commits ahead of the upstream/base branch.
    pub ahead: usize,
    /// Commits behind the upstream/base branch.
    pub behind: usize,
}

/// One account-level rate-limit window (e.g. Claude's 5-hour or weekly), as
/// shown by an agent's `/usage` command. Agent-neutral.
#[derive(Debug, Clone, PartialEq)]
pub struct UsageWindow {
    /// Short label, e.g. "5h", "Week", or a model id.
    pub label: String,
    /// Percent of the window consumed, 0–100.
    pub used_percent: f32,
    /// Unix epoch seconds when the window resets, if known.
    pub resets_at: Option<u64>,
}

/// Account-level usage/rate-limit info for an agent, fetched from the vendor
/// (the `/usage`-equivalent). Account-global, not per-session. Agent-neutral.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AgentUsage {
    pub windows: Vec<UsageWindow>,
    /// Plan/tier label when known (e.g. "max", "pro").
    pub plan: Option<String>,
    /// Human note when no windows are available (not logged in, API error…).
    pub note: Option<String>,
}

impl AgentUsage {
    /// Nothing worth rendering (no windows and no note).
    pub fn is_empty(&self) -> bool {
        self.windows.is_empty() && self.note.is_none()
    }
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
    /// Bare host name (e.g. `devbox`) when the session runs on a remote
    /// `ssh:<host>` backend; `None` for local sessions. Drives the remote
    /// indicator in the session list. Set by the agent layer at spawn/adopt.
    pub remote_host: Option<String>,
    /// Agent metrics from the agent's statusline (Claude only).
    pub agent_metrics: Option<AgentMetrics>,
    /// Latest OSC window title the agent emitted (live activity text),
    /// captured from the terminal and refreshed each tick. Agent-neutral.
    pub agent_activity: Option<String>,
    /// Message text from the agent's latest attention notification (OSC 9/777),
    /// shown as the status when `status == SessionStatus::Attention`.
    pub notification: Option<String>,
    /// Real git state of the session's worktree(s), refreshed periodically by
    /// the app layer. `None` until first computed (or for non-git sessions).
    pub git_stats: Option<GitStats>,
    /// Cached display names for repos, resolved from git remote or directory name.
    /// Order: worktree repos first, then non-worktree additional dirs.
    /// Populated by the app layer at spawn/restore time.
    pub repo_display_names: Vec<String>,
    /// Parent session (lead/worker relationship for orchestration).
    /// `None` for top-level sessions. Purely informational.
    pub parent_session_id: Option<SessionId>,
    /// Manual position in the session list. `None` = never moved: renders
    /// after all ordered sessions, in creation order.
    pub display_order: Option<i64>,
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
            remote_host: None,
            agent_metrics: None,
            agent_activity: None,
            notification: None,
            git_stats: None,
            repo_display_names: Vec::new(),
            parent_session_id: None,
            display_order: None,
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

#[derive(Debug, Clone, Default)]
pub struct SessionConfig {
    /// Desired thurbox [`SessionId`] for the spawned session. When set, the
    /// spawn path uses this id instead of minting a fresh one — so the id is
    /// known *before* launch (to inject it into the process env as
    /// `THURBOX_SESSION`) and can be reused across a respawn so a session's
    /// identity is stable for life. `None` mints a new id at spawn.
    pub session_id: Option<SessionId>,
    /// Resume an existing agent session (process restart of a known session).
    pub resume_session_id: Option<String>,
    /// Pin a session id on a fresh spawn (agents that support it).
    pub agent_session_id: Option<String>,
    pub cwd: Option<PathBuf>,
    /// Name of the agent definition to launch (looked up in the registry).
    pub agent: String,
    /// Backend to spawn on (registry name, e.g. `ssh:devbox`). `None`/empty
    /// selects the registry default (`local-tmux`).
    pub backend: Option<String>,
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
        // Glyphs are shape-distinct (not all circles) so status reads without colour.
        assert_eq!(SessionStatus::Busy.icon(), "●");
        assert_eq!(SessionStatus::Waiting.icon(), "◆");
        assert_eq!(SessionStatus::Idle.icon(), "○");
        assert_eq!(SessionStatus::Error.icon(), "✗");
        assert_eq!(SessionStatus::Attention.icon(), "▲");
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
        assert!(info.agent_metrics.is_none());
        assert!(info.agent_activity.is_none());
        assert!(info.notification.is_none());
        assert_eq!(info.status, SessionStatus::Busy);
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
