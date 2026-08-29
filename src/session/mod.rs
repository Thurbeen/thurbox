pub mod agent_def;
pub mod automation;
pub mod extension_def;
pub mod hook_def;
pub mod hook_status;
pub mod host_def;
pub mod hyperlink;
pub mod message;
pub mod plugin_spec;
pub mod review;
pub mod settings;
pub mod task;
pub mod theme_config;

pub use agent_def::{AgentDef, AgentRegistry};
pub use automation::{
    parse_hhmm, preset_to_cron, Automation, AutomationAction, AutomationRun, AutomationRunStatus,
    AutomationSchedule, ExtraRepo, SchedulePreset,
};
pub use extension_def::{
    AgentPatch, ConfigMerge, ExtensionAutomation, ExtensionDef, ExtensionFile, ExtensionSession,
    ExtensionSymlink, ExternalFile,
};
pub use hook_def::{
    HookContext, HookEvent, HookWorktree, HooksFile, LifecycleHook, DEFAULT_HOOK_TIMEOUT_SECS,
};
pub use hook_status::{
    age_secs, best_state, classify_foreground, contradicts, coverage_for, AgentHookCoverage,
    Assessment, Corroboration, Coverage, CoverageSource, HookDelivery, StateSource,
    AGENT_HOOK_COVERAGE, STATE_RUNNING, STATE_UNCOVERED, STATE_UNREPORTED,
};
pub use host_def::{
    is_remote_backend, is_ssh_backend, is_wsl_backend, HostDef, HostKind, HostRegistry,
    SSH_BACKEND_PREFIX, WSL_BACKEND_PREFIX,
};
pub use hyperlink::{HyperlinkRun, HyperlinkTable, VisibleRun};
pub use message::SessionMessage;
pub use plugin_spec::{
    LockEntry, PackageFile, PackageManifest, PluginEntry, PluginLock, PluginSpec,
};
pub use review::{
    parse_unified_diff, Classification, CommentAnchor, DiffFile, DiffHunk, DiffLine, DiffLineKind,
    FileStatus, ReviewComment, Side,
};
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

/// tmux **pane user option** a remote agent's hooks set to report status
/// (`tmux set-option -p @thurbox_state <working|blocked|done|idle>`). The
/// remote-side replacement for `thurbox-cli session signal`, which can't work
/// off-local (no CLI on the host, and it would write the host's own DB). The
/// local TUI receives changes over its control-mode connection via a format
/// subscription (see [`REMOTE_HOOK_SUBSCRIPTION`]). Defined in the pure-data
/// layer so `agent` (subscription) and `session_ops` (hook-command rewrite)
/// share one source of truth.
pub const REMOTE_HOOK_STATE_OPTION: &str = "@thurbox_state";

/// Name of the control-mode format subscription
/// (`refresh-client -B <name>:%*:#{@thurbox_state}`) that pushes
/// [`REMOTE_HOOK_STATE_OPTION`] changes as `%subscription-changed`
/// notifications for every pane of the attached session.
pub const REMOTE_HOOK_SUBSCRIPTION: &str = "thurbox-status";

/// The states `session signal` accepts — the single source for the CLI's
/// value parser, the TUI's remote-event allow-list
/// (`App::drain_remote_hook_events`), and the headless status poll
/// (`session_ops::remote_hooks`). One list, so a future state (e.g. `error`
/// once exit-code derivation lands) can't be accepted by the CLI yet silently
/// dropped by the remote channels.
pub const HOOK_STATES: [&str; 4] = ["working", "blocked", "done", "idle"];

/// Whether the remote hooks-driven status path is enabled for a **psmux**
/// (native-Windows SSH) host — both halves of it: shipping hook configs with
/// their commands rewritten to the psmux pane-option form
/// (`session_ops::spawn::remote_config_root`), and arming the 1 s pane-option
/// poller on the host's control-mode connection (`agent::tmux`). **Gate,
/// currently closed**: the path rests on behaviors not yet proven against
/// psmux 3.3.6 — in-pane `set-option -p` without `-t` (no `$TMUX_PANE`
/// guarantee), `#{@user_option}` expansion for the poller, and claude
/// accepting a forward-slash `--settings` path on Windows.
/// `scripts/dev/e2e/windows-vm.sh test` carries the probes; flip this to
/// `true` only with that evidence. Closed = exactly the old strip behavior
/// (the agent launches clean with no hooks, surfaced via
/// `SessionInfo::hook_wiring`). Defined in the pure-data layer so `agent`
/// (poller) and `session_ops` (rewrite/shipping) flip on the one switch.
pub fn psmux_hook_rewrite_supported() -> bool {
    false
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

/// A session's lifecycle state, driven by agent hooks (see
/// `thurbox-cli session signal`). Repo groups in the session list roll up to
/// their most-urgent member so the whole list scans at a glance.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionStatus {
    /// 🟡 The agent is actively running (reported by a hook).
    Working,
    /// 🔴 The agent needs user input or approval (reported by a hook).
    Blocked,
    /// 🔵 A turn just finished; shown until the user switches focus off it.
    Done,
    /// 🟢 Acknowledged (focus moved off a `Done`), at rest, or never-active.
    Idle,
    /// Reserved for a crashed agent. **Not currently derived** — process exit has
    /// no failure signal yet (a clean or crashed exit both map to `Idle`), so this
    /// variant is wired through colour/glyph/rollup but never assigned. Kept for
    /// when exit-code plumbing lands.
    Error,
    /// The session lives on a remote host that is currently unreachable (SSH
    /// down / auth failing / host offline). Assigned to placeholder rows so a
    /// remote session never silently vanishes from the list; cleared to the
    /// real hook-driven status once the host recovers and the session adopts.
    Unreachable,
}

impl SessionStatus {
    /// A status glyph chosen for **shape** distinctiveness, not just colour, so
    /// the state survives in greyscale / for colour-blind users: a spinner
    /// (working — the live session list animates it from `theme.spinner` in
    /// `ui/lib/theme.lua`)
    /// vs. diamond (blocked) vs. filled circle (done, unseen) vs. hollow circle
    /// (idle, seen) vs. cross (error). The filled/hollow pair makes
    /// done-vs-idle read at a glance.
    pub fn icon(self) -> &'static str {
        match self {
            Self::Working => "◐",
            Self::Blocked => "◆",
            Self::Done => "●",
            Self::Idle => "○",
            Self::Error => "✗",
            Self::Unreachable => "⊘",
        }
    }
}

impl fmt::Display for SessionStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Working => write!(f, "Working"),
            Self::Blocked => write!(f, "Blocked"),
            Self::Done => write!(f, "Done"),
            Self::Idle => write!(f, "Idle"),
            Self::Error => write!(f, "Error"),
            Self::Unreachable => write!(f, "Unreachable"),
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

impl AgentMetrics {
    /// Parse metrics out of a statusline JSON payload.
    ///
    /// Written by the agent's own statusline hook (Claude's shape), read by
    /// whichever front end is drawing the info panel — so it lives on the type
    /// rather than in either UI.
    pub fn from_statusline_json(raw: &serde_json::Value) -> Self {
        Self {
            model_id: raw
                .pointer("/model/id")
                .and_then(|v| v.as_str())
                .map(String::from),
            model_display_name: raw
                .pointer("/model/display_name")
                .and_then(|v| v.as_str())
                .map(String::from),
            total_cost_usd: raw.pointer("/cost/total_cost_usd").and_then(|v| v.as_f64()),
            total_duration_ms: raw
                .pointer("/cost/total_duration_ms")
                .and_then(|v| v.as_u64()),
            total_api_duration_ms: raw
                .pointer("/cost/total_api_duration_ms")
                .and_then(|v| v.as_u64()),
            total_lines_added: raw
                .pointer("/cost/total_lines_added")
                .and_then(|v| v.as_u64()),
            total_lines_removed: raw
                .pointer("/cost/total_lines_removed")
                .and_then(|v| v.as_u64()),
            total_input_tokens: raw
                .pointer("/context_window/total_input_tokens")
                .and_then(|v| v.as_u64()),
            total_output_tokens: raw
                .pointer("/context_window/total_output_tokens")
                .and_then(|v| v.as_u64()),
            context_window_size: raw
                .pointer("/context_window/context_window_size")
                .and_then(|v| v.as_u64()),
            used_percentage: raw
                .pointer("/context_window/used_percentage")
                .and_then(|v| v.as_u64())
                .map(|v| v.min(100) as u8),
            current_input_tokens: raw
                .pointer("/context_window/current_usage/input_tokens")
                .and_then(|v| v.as_u64()),
            current_output_tokens: raw
                .pointer("/context_window/current_usage/output_tokens")
                .and_then(|v| v.as_u64()),
            cache_creation_input_tokens: raw
                .pointer("/context_window/current_usage/cache_creation_input_tokens")
                .and_then(|v| v.as_u64()),
            cache_read_input_tokens: raw
                .pointer("/context_window/current_usage/cache_read_input_tokens")
                .and_then(|v| v.as_u64()),
            cli_version: raw
                .get("version")
                .and_then(|v| v.as_str())
                .map(String::from),
        }
    }
}

/// Real git state for a session's worktree(s), computed by the app/git layer
/// and surfaced in the info panel. Aggregated across all of a session's
/// worktrees. Agent-neutral (derived from git, not the agent CLI).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GitStats {
    /// Tracked files with staged/unstaged changes vs HEAD (excludes untracked).
    pub files_changed: usize,
    pub insertions: usize,
    pub deletions: usize,
    /// Untracked files (git status `??`), which a worktree removal would lose.
    pub untracked: usize,
    /// Whether the worktree has uncommitted changes (tracked or untracked).
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
/// (the `/usage`-equivalent). Account-global — but the *account* is scoped to
/// wherever the agent's credentials live, i.e. the host the session runs on
/// (local machine, SSH host, or WSL distro). Agent-neutral.
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
    /// shown as the status when `status == SessionStatus::Blocked`.
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
    /// Why hooks-driven status is degraded/absent on this (remote) session —
    /// e.g. the hooks config was stripped for the host, or provisioning
    /// failed. Set at spawn time, transient (never persisted); rendered as a
    /// hint in the info panel. `None` = healthy or local.
    pub hook_wiring: Option<String>,
}

impl SessionInfo {
    pub fn new(name: String) -> Self {
        Self {
            id: SessionId::default(),
            name,
            status: SessionStatus::Working,
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
            hook_wiring: None,
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
        assert_eq!(SessionStatus::Working.to_string(), "Working");
        // Glyphs are shape-distinct (not all circles) so status reads without colour.
        assert_eq!(SessionStatus::Working.icon(), "◐");
        assert_eq!(SessionStatus::Blocked.icon(), "◆");
        assert_eq!(SessionStatus::Done.icon(), "●");
        assert_eq!(SessionStatus::Idle.icon(), "○");
        assert_eq!(SessionStatus::Error.icon(), "✗");
    }

    #[test]
    fn session_info_new_defaults() {
        let info = SessionInfo::new("Test".to_string());
        assert_eq!(info.name, "Test");
        assert_eq!(info.status, SessionStatus::Working);
        assert_eq!(info.agent, DEFAULT_AGENT_NAME);
        assert!(info.worktrees.is_empty());
        assert!(info.agent_session_id.is_none());
        assert!(info.cwd.is_none());
        assert!(info.backend_id.is_none());
        assert!(info.agent_metrics.is_none());
        assert!(info.agent_activity.is_none());
        assert!(info.notification.is_none());
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
