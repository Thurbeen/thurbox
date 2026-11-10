pub mod automation;
pub mod hook_status;
pub mod plugin_spec;
pub mod review;
pub mod settings;

// The host model a route names, the hyperlink table a pane fills and the
// identity a window is stamped with are the multiplexer runtime's; a session
// is described in its terms, so they are re-exported here.
pub use runtime_mux::SessionId;
pub use runtime_mux::{host_def, hyperlink, multiplexer, platform, route};

pub use automation::{parse_hhmm, preset_to_cron, AutomationSchedule, SchedulePreset};
pub use hook_status::{SessionState, WORKING_QUIET_MS};
pub use host_def::{HostDef, HostKind, HostRegistry, WslRepairPlan};
pub use hyperlink::{HyperlinkRun, HyperlinkTable, VisibleRun};
pub use multiplexer::{BackendChoice, Multiplexer};
pub use platform::Platform;
pub use plugin_spec::{
    LockEntry, PackageFile, PackageManifest, PluginEntry, PluginLock, PluginSpec,
};
pub use review::FileStatus;
pub use route::{host_of_key, Place, Route, RouteError, Via, SSH_PREFIX, WSL_PREFIX};

/// The session meta keys published to interface plugins as `session.meta`
/// (ADR-40).
pub const PUBLISHED_META_PREFIX: &str = "ui.";

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
    /// Whether origin's default branch already holds this worktree's work,
    /// however the forge landed it (see `git::merged_into_default`). `None`
    /// when the question was not asked (nothing ahead) or could not be
    /// answered.
    pub merged: Option<bool>,
    /// The commit these stats describe. It is what `merged` is an answer
    /// *about*, so a caller caching that answer keys it on this rather than on
    /// the worktree — see `git::worktree_stats`. `None` on an unborn branch.
    pub head: Option<String>,
}
