mod helpers;
mod key_handlers;
pub(crate) mod mcp_editor_modal;
pub(crate) mod modals;
mod provisioning;
mod state;
mod view;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{mpsc, Arc};

use crossterm::event::{KeyCode, KeyModifiers};
use ratatui::{
    layout::{Position, Rect},
    widgets::{Block, Borders},
};
use tracing::{error, info, warn};

use crate::agent::{BackendRegistry, Session, SessionBackend};
use crate::git;
use crate::project::{ProjectConfig, ProjectId, ProjectInfo};
use crate::session::{
    default_developer_permissions, default_developer_role, RoleConfig, RolePermissions,
    SessionCommand, SessionConfig, SessionId, SessionInfo, SessionStatus, WorktreeInfo,
    DEFAULT_ROLE_NAME,
};
use crate::storage::Database;
use crate::storage::DeletedSessionInfo;
use crate::sync::{self, SharedWorktree, StateDelta, SyncState};
use crate::ui::selection::{PaneBounds, Selection, TermPos};
use crate::ui::{info_panel, layout, role_editor_modal};

const MOUSE_SCROLL_LINES: usize = 3;

/// How long the user has to press Ctrl+Z to undo a session delete.
const UNDO_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// If no output for this many milliseconds, consider session "Waiting".
const ACTIVITY_TIMEOUT_MS: u64 = 1000;

/// Prompt sent to Claude sessions when a worktree rebase has conflicts.
const SYNC_CONFLICT_PROMPT: &str = "Please sync this worktree with main. Run: git fetch origin && git rebase origin/main -- if there are conflicts, resolve them and continue the rebase with git rebase --continue.";

/// Tick delay before sending Enter after pasting text into a session.
/// At ~10ms per tick, 10 ticks ≈ 100ms — enough for the app to process the pasted text.
const DEFERRED_INPUT_DELAY_TICKS: u64 = 10;

/// How often to refresh system metrics (in ticks). At ~10ms per tick, 100 ≈ 1 second.
const METRICS_REFRESH_TICKS: u64 = 100;

/// MCP tool names auto-allowed in the admin session so Claude can manage
/// Thurbox without repeated permission prompts.
const ADMIN_MCP_TOOLS: &[&str] = &[
    "mcp__thurbox__list_projects",
    "mcp__thurbox__get_project",
    "mcp__thurbox__create_project",
    "mcp__thurbox__update_project",
    "mcp__thurbox__delete_project",
    "mcp__thurbox__list_roles",
    "mcp__thurbox__set_roles",
    "mcp__thurbox__list_mcp_servers",
    "mcp__thurbox__set_mcp_servers",
    "mcp__thurbox__list_sessions",
    "mcp__thurbox__get_session",
    "mcp__thurbox__delete_session",
    "mcp__thurbox__restart_session",
    "mcp__thurbox__restore_session",
    "mcp__thurbox__list_vms",
    "mcp__thurbox__get_vm",
    "mcp__thurbox__configure_project_vm",
    "mcp__thurbox__list_containerfile_templates",
    "mcp__thurbox__get_containerfile_template",
    "mcp__thurbox__set_containerfile_template",
    "mcp__thurbox__delete_containerfile_template",
    "mcp__thurbox__configure_project_container",
    "mcp__thurbox__get_project_container_config",
    "mcp__thurbox__list_vm_images",
    "mcp__thurbox__download_vm_image",
    "mcp__thurbox__delete_vm_image",
    "mcp__thurbox__schedule_command",
    "mcp__thurbox__list_scheduled_commands",
    "mcp__thurbox__get_scheduled_command",
    "mcp__thurbox__cancel_scheduled_command",
];

/// System prompt appended to the admin session to give Claude context about its
/// role as the Thurbox management assistant.
const ADMIN_SYSTEM_PROMPT: &str = "\
You are the Thurbox admin assistant. Thurbox is a multi-session Claude Code TUI \
orchestrator. Your role is to help the user manage their Thurbox setup using the \
thurbox MCP tools available to you.

You can:
- List, create, update, and delete projects (each project groups related sessions)
- Configure roles for projects (named permission presets applied to sessions)
- Configure MCP servers for projects
- List, inspect, delete, restart, and restore sessions
- List and inspect VMs; configure per-project VM defaults
- Create and manage Containerfile templates for container-based sessions
- Configure per-project container defaults (image, cpus, memory, firewall, template)
- List, download, and delete VM images for sandbox sessions

Containerfile templates live in ~/.local/share/thurbox/containerfiles/. Each \
template is a folder containing a Containerfile and any support files (e.g. \
init-firewall.sh). The default/ template includes Node.js LTS, tmux, git, \
iptables, and claude-code. Use the containerfile template tools to list, read, \
create, update, and delete templates — no need to edit files directly.

VM images live in ~/.local/share/thurbox/images/. Use the VM image tools to \
list cached images, download new ones from HTTPS URLs, or delete old ones.

When the user asks you to manage projects, roles, sessions, or MCP servers, use \
the appropriate thurbox MCP tool. Always list existing resources before making \
changes so you have current state.

Important: delete operations are soft-deletes (recoverable via undo in the TUI). \
Role changes via set_roles are atomic replacements — include all desired roles, \
not just new ones.";

/// Parse agent metrics from a Claude CLI statusline JSON value.
fn parse_agent_metrics(raw: &serde_json::Value) -> crate::session::AgentMetrics {
    use crate::session::AgentMetrics;
    AgentMetrics {
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

/// Build `RolePermissions` with all admin MCP tools pre-allowed.
fn admin_mcp_permissions() -> RolePermissions {
    RolePermissions {
        allowed_tools: ADMIN_MCP_TOOLS.iter().map(|s| s.to_string()).collect(),
        append_system_prompt: Some(ADMIN_SYSTEM_PROMPT.to_string()),
        ..RolePermissions::default()
    }
}

pub use modals::RoleEditorView;

pub use modals::AddProjectField;

pub use modals::EditProjectField;

pub use modals::ScheduleCommandField;

/// State for an editable list of tool names (allowed or disallowed).
pub(crate) struct ToolListState {
    pub(crate) items: Vec<String>,
    pub(crate) selected: usize,
    pub(crate) mode: role_editor_modal::ToolListMode,
    pub(crate) input: TextInput,
}

impl ToolListState {
    fn new() -> Self {
        Self {
            items: Vec::new(),
            selected: 0,
            mode: role_editor_modal::ToolListMode::Browse,
            input: TextInput::new(),
        }
    }

    fn reset(&mut self) {
        self.items.clear();
        self.selected = 0;
        self.mode = role_editor_modal::ToolListMode::Browse;
        self.input.clear();
    }

    fn load(&mut self, tools: &[String]) {
        self.items = tools.to_vec();
        self.selected = 0;
        self.mode = role_editor_modal::ToolListMode::Browse;
        self.input.clear();
    }

    fn start_adding(&mut self) {
        self.mode = role_editor_modal::ToolListMode::Adding;
        self.input.clear();
    }

    fn confirm_add(&mut self) {
        let val = self.input.value().trim().to_string();
        if !val.is_empty() {
            self.items.push(val);
            self.selected = self.items.len() - 1;
        }
        self.mode = role_editor_modal::ToolListMode::Browse;
    }

    fn cancel_add(&mut self) {
        self.mode = role_editor_modal::ToolListMode::Browse;
    }

    fn delete_selected(&mut self) {
        if !self.items.is_empty() {
            self.items.remove(self.selected);
            if self.selected >= self.items.len() && self.selected > 0 {
                self.selected -= 1;
            }
        }
    }

    fn move_down(&mut self) {
        if !self.items.is_empty() && self.selected + 1 < self.items.len() {
            self.selected += 1;
        }
    }

    fn move_up(&mut self) {
        self.selected = self.selected.saturating_sub(1);
    }
}

pub(crate) struct TextInput {
    pub(crate) buffer: String,
    pub(crate) cursor: usize,
}

impl TextInput {
    fn new() -> Self {
        Self {
            buffer: String::new(),
            cursor: 0,
        }
    }

    fn insert(&mut self, c: char) {
        let byte_pos = self.byte_offset();
        self.buffer.insert(byte_pos, c);
        self.cursor += 1;
    }

    fn backspace(&mut self) {
        if self.cursor > 0 {
            self.cursor -= 1;
            let byte_pos = self.byte_offset();
            self.buffer.remove(byte_pos);
        }
    }

    fn delete(&mut self) {
        let byte_pos = self.byte_offset();
        if byte_pos < self.buffer.len() {
            self.buffer.remove(byte_pos);
        }
    }

    fn move_left(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }

    fn move_right(&mut self) {
        let char_count = self.buffer.chars().count();
        if self.cursor < char_count {
            self.cursor += 1;
        }
    }

    fn home(&mut self) {
        self.cursor = 0;
    }

    fn end(&mut self) {
        self.cursor = self.buffer.chars().count();
    }

    fn clear(&mut self) {
        self.buffer.clear();
        self.cursor = 0;
    }

    fn set(&mut self, value: &str) {
        self.buffer = value.to_string();
        self.cursor = value.chars().count();
    }

    fn value(&self) -> &str {
        &self.buffer
    }

    fn cursor_pos(&self) -> usize {
        self.cursor
    }

    /// Convert char-based cursor position to byte offset.
    fn byte_offset(&self) -> usize {
        self.buffer
            .char_indices()
            .nth(self.cursor)
            .map(|(i, _)| i)
            .unwrap_or(self.buffer.len())
    }
}

pub enum AppMessage {
    KeyPress(KeyCode, KeyModifiers),
    MouseScrollUp,
    MouseScrollDown,
    MouseClick {
        x: u16,
        y: u16,
        modifiers: KeyModifiers,
    },
    MouseDrag {
        x: u16,
        y: u16,
    },
    MouseUp {
        x: u16,
        y: u16,
    },
    Resize(u16, u16),
    ExternalStateChange(StateDelta),
    /// A VM has finished provisioning and is ready for a session.
    VmReady {
        vm_id: String,
    },
    /// VM provisioning failed.
    VmFailed {
        error: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusLevel {
    Info,
    Success,
    Error,
}

#[derive(Debug, Clone)]
pub struct StatusMessage {
    pub text: String,
    pub level: StatusLevel,
    pub created_at: std::time::Instant,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputFocus {
    ProjectList,
    SessionList,
    Terminal,
}

/// Which pane the terminal view is showing for a given session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TerminalView {
    Claude,
    Shell,
}

/// Holds a recently deleted session for undo (Ctrl+Z) support.
struct PendingDelete {
    session: Session,
    session_id: SessionId,
    project_id: ProjectId,
    created_at: std::time::Instant,
}

/// Result sent by a background container/VM restore thread on completion.
struct RestoreResult {
    /// Sessions discovered on the restored backend (for adopt matching).
    discovered: Vec<crate::agent::backend::DiscoveredSession>,
}

/// A background container/VM restoration task being polled by `tick()`.
struct PendingRestore {
    /// Placeholder session ID shown with `Provisioning` status.
    session_id: SessionId,
    /// Completion channel — receives `Ok(RestoreResult)` or `Err(message)`.
    rx: mpsc::Receiver<Result<RestoreResult, String>>,
    /// Progress step channel — latest message shown as `provisioning_step`.
    step_rx: mpsc::Receiver<String>,
    /// Original session data needed for adopt/respawn once the background work finishes.
    shared: sync::SharedSession,
    /// Project index for this session.
    project_index: usize,
}

pub struct App {
    pub(crate) projects: Vec<ProjectInfo>,
    pub(crate) active_project_index: usize,
    pub(crate) sessions: Vec<Session>,
    pub(crate) active_index: usize,
    backends: BackendRegistry,
    provider: Arc<dyn crate::agent::AgentProvider>,
    pub(crate) db: Database,
    pub(crate) focus: InputFocus,
    pub(crate) should_quit: bool,
    pub(crate) status_message: Option<StatusMessage>,
    terminal_rows: u16,
    pub(crate) terminal_cols: u16,
    session_counter: usize,
    pub(crate) show_info_panel: bool,
    pub(crate) modal: modals::Modal,
    // (Delete project, add project, edit project modal state is now in self.modal)
    pub(crate) pending_repo_path: Option<PathBuf>,
    pub(crate) pending_all_repos: Option<Vec<PathBuf>>,
    /// Repo paths collected for rsync into the VM (set during provisioning).
    pub(crate) pending_vm_repo_paths: Option<Vec<PathBuf>>,
    pub(crate) pending_base_branch: Option<String>,
    pub(crate) pending_spawn_config: Option<SessionConfig>,
    pub(crate) pending_spawn_worktrees: Vec<WorktreeInfo>,
    pub(crate) pending_spawn_name: Option<String>,
    pub(crate) show_role_editor: bool,
    pub(crate) role_editor_view: RoleEditorView,
    pub(crate) role_editor_field: role_editor_modal::RoleEditorField,
    pub(crate) role_editor_name: TextInput,
    pub(crate) role_editor_description: TextInput,
    pub(crate) role_editor_allowed_tools: ToolListState,
    pub(crate) role_editor_disallowed_tools: ToolListState,
    pub(crate) role_editor_system_prompt: TextInput,
    pub(crate) role_editor_env: ToolListState,
    pub(crate) role_editor_editing_index: Option<usize>,
    pub(crate) show_mcp_editor: bool,
    pub(crate) mcp_editor_field: mcp_editor_modal::McpEditorField,
    pub(crate) mcp_editor_name: TextInput,
    pub(crate) mcp_editor_command: TextInput,
    pub(crate) mcp_editor_args: ToolListState,
    pub(crate) mcp_editor_env: ToolListState,
    pub(crate) mcp_editor_editing_index: Option<usize>,
    /// Snapshot of role editor fields at open time for dirty detection.
    pub(crate) role_editor_snapshot: Option<EditorSnapshot>,
    /// Snapshot of MCP editor fields at open time for dirty detection.
    pub(crate) mcp_editor_snapshot: Option<EditorSnapshot>,
    /// Whether the "Discard unsaved changes?" confirmation is showing.
    pub(crate) show_discard_confirmation: bool,
    /// Inter-instance DB sync (polls for changes from other thurbox instances).
    sync_state: SyncState,
    /// Worktree-to-main git sync (Ctrl+S).
    worktree_sync_in_progress: bool,
    worktree_sync_rx: Option<mpsc::Receiver<(SessionId, git::SyncResult)>>,
    worktree_sync_pending: usize,
    worktree_sync_completed: Vec<(SessionId, git::SyncResult)>,
    tick_count: u64,
    /// System info collector for CPU/RAM metrics.
    sys: sysinfo::System,
    /// Cached system metrics for the info panel.
    system_metrics: crate::ui::info_panel::SystemMetrics,
    /// Deferred inputs: `(session_id, data, tick_at_which_to_send)`.
    /// Used to introduce a small delay between pasting text and pressing Enter.
    deferred_inputs: Vec<(SessionId, Vec<u8>, u64)>,
    /// Per-session terminal view state (Claude vs Shell). Defaults to Claude.
    session_terminal_views: HashMap<SessionId, TerminalView>,
    /// Recently deleted session awaiting finalization or undo (Ctrl+Z).
    pending_delete: Option<PendingDelete>,
    // (Restore sessions modal state is now in self.modal)
    /// VM provisioning in progress — stores the pending session config.
    vm_provisioning: bool,
    /// VM ID currently being provisioned.
    vm_provisioning_id: Option<String>,
    /// VM lifecycle manager (shared with background provisioning thread).
    vm_manager: Option<Arc<std::sync::Mutex<crate::agent::VmManager>>>,
    /// Receiver for VM provisioning results from background thread.
    vm_provision_rx: Option<mpsc::Receiver<Result<String, String>>>,
    /// Receiver for VM provisioning step updates (progress messages).
    vm_provision_step_rx: Option<mpsc::Receiver<String>>,
    /// Current VM provisioning step description shown in the status bar.
    vm_provisioning_step: String,
    /// Placeholder session shown in the session list during VM provisioning.
    vm_placeholder: Option<SessionInfo>,
    /// Session config preserved during VM provisioning for role selection after ready.
    pending_vm_config: Option<SessionConfig>,
    /// VM ID stored after provisioning completes, consumed by `do_spawn_session`.
    pending_vm_id: Option<String>,
    /// MCP servers to write into the VM working directory before spawning.
    pending_vm_mcp_servers: Option<Vec<crate::session::McpServerConfig>>,
    /// Active text selection (click+drag), uses screen-absolute coordinates.
    pub(crate) text_selection: Option<Selection>,
    /// Cached text extracted from the frame buffer for the current selection.
    selected_text_cache: Option<String>,
    /// Persistent clipboard handle to avoid "dropped too quickly" warnings on Linux.
    clipboard: Option<arboard::Clipboard>,
    /// Containerfile name selected by the user (consumed during provisioning).
    pub(crate) pending_containerfile_name: Option<String>,
    /// Container provisioning in progress.
    container_provisioning: bool,
    /// Container ID currently being provisioned.
    container_provisioning_id: Option<String>,
    /// Container lifecycle manager (shared with background provisioning thread).
    container_manager: Option<Arc<std::sync::Mutex<crate::agent::ContainerManager>>>,
    /// Receiver for container provisioning results from background thread.
    container_provision_rx: Option<mpsc::Receiver<Result<String, String>>>,
    /// Receiver for container provisioning step updates (progress messages).
    container_provision_step_rx: Option<mpsc::Receiver<String>>,
    /// Current container provisioning step description.
    container_provisioning_step: String,
    /// Placeholder session shown during container provisioning.
    container_placeholder: Option<SessionInfo>,
    /// Session config preserved during container provisioning for role selection after ready.
    pending_container_config: Option<SessionConfig>,
    /// Container ID stored after provisioning completes, consumed by `do_spawn_session`.
    pending_container_id: Option<String>,
    /// MCP servers to write into the container working directory before spawning.
    pending_container_mcp_servers: Option<Vec<crate::session::McpServerConfig>>,
    /// Background container/VM restoration tasks polled by `tick()`.
    pending_restores: Vec<PendingRestore>,
    /// Reusable buffer for session elapsed-ms in the view (avoids per-frame allocation).
    pub(crate) session_elapsed_buf: Vec<u64>,
}

/// Snapshot of editor field values for dirty detection.
#[derive(Clone, PartialEq)]
pub(crate) struct EditorSnapshot {
    pub fields: Vec<String>,
}

/// Convert a SharedProject to ProjectInfo, preserving the shared state ID.
fn shared_project_to_info(sp: sync::SharedProject) -> ProjectInfo {
    let config = ProjectConfig {
        name: sp.name,
        repos: sp.repos,
        roles: sp.roles,
        mcp_servers: sp.mcp_servers,
        id: Some(sp.id.to_string()),
    };
    let mut info = ProjectInfo::new(config);
    info.id = sp.id;
    info
}

/// One-time migration: import roles from config.toml into the database.
///
/// If config.toml exists and has projects with roles, and the DB has no roles yet,
/// import them. After successful import, rename config.toml → config.toml.bak.
fn migrate_config_toml_roles(db: &Database) {
    // Check if migration already done (DB has roles)
    if let Ok(roles_map) = db.list_all_roles() {
        if !roles_map.is_empty() {
            return;
        }
    }

    // Check migration metadata flag
    let migrated: bool = db
        .conn_ref()
        .query_row(
            "SELECT value FROM metadata WHERE key = 'config_toml_migrated'",
            [],
            |row| {
                let v: String = row.get(0)?;
                Ok(v == "true")
            },
        )
        .unwrap_or(false);
    if migrated {
        return;
    }

    let Some(config_path) = crate::paths::config_file() else {
        return;
    };
    let Ok(contents) = std::fs::read_to_string(&config_path) else {
        return;
    };

    // Inline TOML parsing for legacy config format
    #[derive(serde::Deserialize)]
    struct LegacyConfigFile {
        #[serde(default)]
        projects: Vec<LegacyProjectConfig>,
    }
    #[derive(serde::Deserialize)]
    struct LegacyProjectConfig {
        name: String,
        #[serde(default)]
        repos: Vec<std::path::PathBuf>,
        #[serde(default)]
        roles: Vec<crate::session::RoleConfig>,
        #[serde(default)]
        id: Option<String>,
    }

    let Ok(legacy) = toml::from_str::<LegacyConfigFile>(&contents) else {
        return;
    };

    let mut had_roles = false;
    for lp in &legacy.projects {
        if lp.roles.is_empty() {
            continue;
        }

        // Find matching project in DB
        let db_projects = db.list_active_projects().unwrap_or_default();
        let config_id = lp.id.as_ref().and_then(|s| {
            s.parse::<uuid::Uuid>()
                .ok()
                .map(crate::project::ProjectId::from_uuid)
        });
        let det_id = {
            let c = ProjectConfig {
                name: lp.name.clone(),
                repos: lp.repos.clone(),
                roles: Vec::new(),
                mcp_servers: Vec::new(),
                id: None,
            };
            c.deterministic_id()
        };

        if let Some(db_proj) = db_projects
            .iter()
            .find(|p| Some(p.id) == config_id || p.id == det_id || p.name == lp.name)
        {
            if let Err(e) = db.replace_roles(db_proj.id, &lp.roles) {
                tracing::warn!("Failed to migrate roles for {}: {e}", lp.name);
            } else {
                had_roles = true;
            }
        }
    }

    // Mark migration as done
    let _ = db.conn_ref().execute(
        "INSERT OR REPLACE INTO metadata (key, value) VALUES ('config_toml_migrated', 'true')",
        [],
    );

    // Rename config.toml to .bak if we migrated roles
    if had_roles {
        let bak = config_path.with_extension("toml.bak");
        if let Err(e) = std::fs::rename(&config_path, &bak) {
            tracing::warn!("Failed to rename {} to .bak: {e}", config_path.display());
        } else {
            tracing::info!(
                "Migrated roles from config.toml to SQLite; backed up to {}",
                bak.display()
            );
        }
    }
}

/// Load projects from the database.
///
/// The DB is the single source of truth for all project data including roles.
/// Non-admin projects with no roles are seeded with the default developer role
/// and persisted back, so the migration is transparent on subsequent loads.
/// Returns an empty vec if the database has no projects.
fn load_projects_from_db(db: &Database) -> Vec<ProjectInfo> {
    let mut projects: Vec<ProjectInfo> = db
        .list_active_projects()
        .unwrap_or_default()
        .into_iter()
        .map(shared_project_to_info)
        .collect();

    // Seed existing projects that have no roles with the default developer role.
    for project in &mut projects {
        if !project.is_admin && project.config.roles.is_empty() {
            project.config.roles = vec![default_developer_role()];
            let _ = db.replace_roles(project.id, &project.config.roles);
        }
    }

    projects
}

impl App {
    pub fn new(
        rows: u16,
        cols: u16,
        backends: BackendRegistry,
        provider: Arc<dyn crate::agent::AgentProvider>,
        db: Database,
        vm_manager: Option<Arc<std::sync::Mutex<crate::agent::VmManager>>>,
        container_manager: Option<Arc<std::sync::Mutex<crate::agent::ContainerManager>>>,
    ) -> Self {
        // Migrate roles from config.toml on first run after upgrade
        migrate_config_toml_roles(&db);

        let projects = load_projects_from_db(&db);

        // Load session counter from DB
        let session_counter = db.get_session_counter().unwrap_or(0);

        let mut sync_state = SyncState::new();

        // Initialize the sync snapshot from the current DB state so the first
        // poll doesn't produce a false delta treating everything as "added".
        if let Ok(initial_state) = db.load_shared_state() {
            sync_state.set_initial_snapshot(initial_state);
        }

        Self {
            projects,
            active_project_index: 0,
            sessions: Vec::new(),
            active_index: 0,
            backends,
            provider,
            db,
            focus: InputFocus::ProjectList,
            should_quit: false,
            status_message: None,
            terminal_rows: rows,
            terminal_cols: cols,
            session_counter,
            show_info_panel: false,
            modal: modals::Modal::None,
            pending_repo_path: None,
            pending_all_repos: None,
            pending_vm_repo_paths: None,
            pending_base_branch: None,
            pending_spawn_config: None,
            pending_spawn_worktrees: Vec::new(),
            pending_spawn_name: None,
            show_role_editor: false,
            role_editor_view: RoleEditorView::List,
            role_editor_field: role_editor_modal::RoleEditorField::Name,
            role_editor_name: TextInput::new(),
            role_editor_description: TextInput::new(),
            role_editor_allowed_tools: ToolListState::new(),
            role_editor_disallowed_tools: ToolListState::new(),
            role_editor_system_prompt: TextInput::new(),
            role_editor_env: ToolListState::new(),
            role_editor_editing_index: None,
            show_mcp_editor: false,
            mcp_editor_field: mcp_editor_modal::McpEditorField::Name,
            mcp_editor_name: TextInput::new(),
            mcp_editor_command: TextInput::new(),
            mcp_editor_args: ToolListState::new(),
            mcp_editor_env: ToolListState::new(),
            mcp_editor_editing_index: None,
            role_editor_snapshot: None,
            mcp_editor_snapshot: None,
            show_discard_confirmation: false,
            sync_state,
            worktree_sync_in_progress: false,
            worktree_sync_rx: None,
            worktree_sync_pending: 0,
            worktree_sync_completed: Vec::new(),
            tick_count: 0,
            sys: sysinfo::System::new(),
            system_metrics: info_panel::SystemMetrics {
                cpu_percent: 0.0,
                memory_used: 0,
                memory_total: 0,
                session_cpu_percent: 0.0,
                session_memory_bytes: 0,
            },
            deferred_inputs: Vec::new(),
            session_terminal_views: HashMap::new(),
            pending_delete: None,
            vm_provisioning: false,
            vm_provisioning_id: None,
            vm_manager,
            vm_provision_rx: None,
            vm_provision_step_rx: None,
            vm_provisioning_step: String::new(),
            vm_placeholder: None,
            pending_vm_config: None,
            pending_vm_id: None,
            pending_vm_mcp_servers: None,
            text_selection: None,
            selected_text_cache: None,
            clipboard: arboard::Clipboard::new().ok(),
            pending_containerfile_name: None,
            container_provisioning: false,
            container_provisioning_id: None,
            container_manager,
            container_provision_rx: None,
            container_provision_step_rx: None,
            container_provisioning_step: String::new(),
            container_placeholder: None,
            pending_container_config: None,
            pending_container_id: None,
            pending_container_mcp_servers: None,
            pending_restores: Vec::new(),
            session_elapsed_buf: Vec::new(),
        }
    }

    /// Ensure the containerfiles template directory exists and is seeded with defaults.
    ///
    /// Creates `~/.local/share/thurbox/containerfiles/default/` containing:
    /// - `Containerfile` — the default container image definition
    /// - `init-firewall.sh` — the firewall script referenced by the Containerfile
    ///
    /// Each template is a folder used as the build context.
    pub fn ensure_containerfiles_dir(&self) {
        let Some(dir) = crate::paths::containerfiles_directory() else {
            return;
        };
        let default_dir = dir.join("default");
        if let Err(e) = std::fs::create_dir_all(&default_dir) {
            tracing::warn!("Failed to create default containerfile directory: {e}");
            return;
        }
        // Always overwrite the "default" template to keep it in sync with the
        // built-in version. Users who want a custom template should create a
        // new named template instead of modifying "default".
        if let Err(e) = std::fs::write(
            default_dir.join("Containerfile"),
            crate::agent::DEFAULT_CONTAINERFILE,
        ) {
            tracing::warn!("Failed to write default Containerfile: {e}");
        }
        if let Err(e) = std::fs::write(
            default_dir.join("init-firewall.sh"),
            crate::agent::INIT_FIREWALL_SH,
        ) {
            tracing::warn!("Failed to write init-firewall.sh: {e}");
        }
        if let Err(e) = std::fs::write(
            default_dir.join("allowlist.conf"),
            crate::agent::DEFAULT_ALLOWLIST,
        ) {
            tracing::warn!("Failed to write allowlist.conf: {e}");
        }
    }

    /// Load available containerfile template names from the templates directory.
    ///
    /// Each template is a subdirectory containing a `Containerfile`. Returns
    /// sorted directory names, excluding hidden directories and directories
    /// without a `Containerfile`.
    pub fn load_containerfiles(&self) -> Vec<String> {
        let Some(dir) = crate::paths::containerfiles_directory() else {
            return vec!["default".to_string()];
        };
        let entries = match std::fs::read_dir(&dir) {
            Ok(e) => e,
            Err(_) => return vec!["default".to_string()],
        };
        let mut names: Vec<String> = entries
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().map(|ft| ft.is_dir()).unwrap_or(false))
            .filter_map(|e| {
                let name = e.file_name().to_string_lossy().to_string();
                if name.starts_with('.') {
                    return None;
                }
                // Only include if the directory contains a Containerfile
                if e.path().join("Containerfile").exists() {
                    Some(name)
                } else {
                    None
                }
            })
            .collect();
        names.sort();
        if names.is_empty() {
            names.push("default".to_string());
        }
        names
    }

    /// Ensure the global admin project and `.mcp.json` exist.
    ///
    /// Creates a dedicated admin directory with a `.mcp.json` pointing to the
    /// `thurbox-mcp` binary and an "Admin" pseudo-project pinned at index 0.
    /// Does not auto-spawn any session — the user creates sessions explicitly.
    /// The `.mcp.json` is rewritten on every startup to pick up binary path
    /// changes after upgrades.
    pub fn ensure_admin_setup(&mut self) {
        let Some(admin_dir) = crate::paths::admin_directory() else {
            tracing::warn!("Could not resolve admin directory path");
            return;
        };

        if let Err(e) = std::fs::create_dir_all(&admin_dir) {
            tracing::warn!("Failed to create admin directory: {e}");
            return;
        }

        self.write_mcp_json(&admin_dir);
        self.ensure_admin_project(&admin_dir);
    }

    /// Set up the statusline script and `~/.claude/settings.json` for agent metrics.
    ///
    /// Creates a shell script that the Claude CLI pipes statusline JSON into,
    /// which writes metrics to per-session files. Also configures the Claude CLI
    /// global settings to use this script as the statusline handler.
    pub fn ensure_statusline_setup(&self) {
        let Some(data_dir) = crate::paths::log_directory() else {
            return;
        };
        let Some(metrics_dir) = crate::paths::metrics_directory() else {
            return;
        };

        // Ensure metrics directory exists.
        if let Err(e) = std::fs::create_dir_all(&metrics_dir) {
            tracing::warn!("Failed to create metrics directory: {e}");
            return;
        }

        // Write statusline shell script.
        // The script captures stdin JSON from the Claude CLI and saves it
        // using the THURBOX_SESSION_ID env var as filename. If the env var
        // is missing (e.g. sessions spawned before this feature), it falls
        // back to extracting `session_id` from the JSON itself.
        let script_path = data_dir.join("statusline.sh");
        let script = format!(
            "#!/bin/sh\n\
             METRICS_DIR=\"${{THURBOX_METRICS_DIR:-{metrics_dir}}}\"\n\
             mkdir -p \"$METRICS_DIR\"\n\
             INPUT=$(cat)\n\
             SID=\"$THURBOX_SESSION_ID\"\n\
             if [ -z \"$SID\" ]; then\n\
             \tSID=$(printf '%s' \"$INPUT\" | grep -o '\"session_id\"[[:space:]]*:[[:space:]]*\"[^\"]*\"' \
             | head -1 | sed 's/.*\"\\([^\"]*\\)\"$/\\1/')\n\
             fi\n\
             if [ -n \"$SID\" ]; then\n\
             \tprintf '%s' \"$INPUT\" > \"$METRICS_DIR/$SID.json\"\n\
             fi\n",
            metrics_dir = metrics_dir.display()
        );
        if let Err(e) = std::fs::write(&script_path, &script) {
            tracing::warn!("Failed to write statusline script: {e}");
            return;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&script_path, std::fs::Permissions::from_mode(0o755));
        }

        // Configure ~/.claude/settings.json with the statusline command.
        let home = match std::env::var_os("HOME") {
            Some(h) => std::path::PathBuf::from(h),
            None => return,
        };
        let settings_path = home.join(".claude").join("settings.json");
        if let Err(e) = std::fs::create_dir_all(settings_path.parent().unwrap()) {
            tracing::warn!("Failed to create ~/.claude directory: {e}");
            return;
        }

        let mut settings: serde_json::Value = std::fs::read_to_string(&settings_path)
            .ok()
            .and_then(|c| serde_json::from_str(&c).ok())
            .unwrap_or_else(|| serde_json::json!({}));

        // Only write statusLine if not already configured by the user.
        if settings.get("statusLine").is_none() {
            settings["statusLine"] = serde_json::json!({
                "type": "command",
                "command": script_path.display().to_string()
            });
            if let Err(e) = std::fs::write(
                &settings_path,
                serde_json::to_string_pretty(&settings).unwrap(),
            ) {
                tracing::warn!("Failed to write ~/.claude/settings.json: {e}");
            }
        }

        // Clean up stale metrics files (sessions that no longer exist).
        if let Ok(entries) = std::fs::read_dir(&metrics_dir) {
            let active_sids: std::collections::HashSet<String> = self
                .sessions
                .iter()
                .filter_map(|s| s.info.agent_session_id.clone())
                .collect();
            for entry in entries.flatten() {
                if let Some(stem) = entry.path().file_stem().and_then(|s| s.to_str()) {
                    if !active_sids.contains(stem) {
                        let _ = std::fs::remove_file(entry.path());
                    }
                }
            }
        }
    }

    /// Write `.mcp.json` into the admin directory.
    ///
    /// Rewritten on every startup to pick up binary path changes after upgrades.
    fn write_mcp_json(&self, admin_dir: &std::path::Path) {
        let mcp_binary = crate::paths::thurbox_mcp_binary();
        let mcp_json = serde_json::json!({
            "mcpServers": {
                "thurbox": {
                    "command": mcp_binary,
                    "args": []
                }
            }
        })
        .to_string();
        if let Err(e) = std::fs::write(admin_dir.join(".mcp.json"), &mcp_json) {
            tracing::warn!("Failed to write .mcp.json: {e}");
        }
    }

    /// Ensure the Admin project exists at index 0.
    fn ensure_admin_project(&mut self, admin_dir: &std::path::Path) {
        let admin_config = ProjectConfig {
            name: "Admin".to_string(),
            repos: vec![admin_dir.to_path_buf()],
            roles: Vec::new(),
            mcp_servers: Vec::new(),
            id: None,
        };
        let admin_id = admin_config.effective_id();

        if let Some(pos) = self.projects.iter().position(|p| p.id == admin_id) {
            // Mark existing project as admin and move to index 0
            self.projects[pos].is_admin = true;
            if pos != 0 {
                let project = self.projects.remove(pos);
                self.projects.insert(0, project);
                if self.active_project_index == pos {
                    self.active_project_index = 0;
                } else if self.active_project_index < pos {
                    self.active_project_index += 1;
                }
            }
        } else {
            let had_projects = !self.projects.is_empty();
            let info = ProjectInfo::new_admin(admin_config);
            self.projects.insert(0, info);
            if had_projects {
                self.active_project_index += 1;
            }
            self.save_project_to_db(&self.projects[0].clone());
        }
    }

    pub fn spawn_session(&mut self) {
        let Some(project) = self.active_project() else {
            return;
        };

        let repos = project.config.repos.clone();
        match repos.len() {
            0 => {
                let mut config = SessionConfig::default();
                if let Some(home) = std::env::var_os("HOME") {
                    config.cwd = Some(PathBuf::from(home));
                }
                self.spawn_session_with_config(&config);
            }
            _ => {
                // 1+ repos: show session mode modal (Normal vs Worktree)
                self.pending_repo_path = Some(repos[0].clone());
                self.pending_all_repos = if repos.len() > 1 { Some(repos) } else { None };
                self.modal = modals::Modal::SessionMode(modals::SessionModeModal { index: 0 });
            }
        }
    }

    pub(crate) fn spawn_session_in_repo(&mut self, repo_path: PathBuf) {
        let config = SessionConfig {
            cwd: Some(repo_path),
            ..SessionConfig::default()
        };
        self.spawn_session_with_config(&config);
    }

    fn next_session_name(&mut self) -> String {
        self.session_counter += 1;
        self.session_counter.to_string()
    }

    pub(crate) fn spawn_session_with_config(&mut self, config: &SessionConfig) {
        self.prepare_spawn(config.clone(), Vec::new());
    }

    /// Route session creation through role selection.
    ///
    /// Assigns a session name, then spawns immediately if no roles or exactly
    /// one role is configured, or shows the role selector modal for 2+ roles.
    pub(crate) fn prepare_spawn(
        &mut self,
        mut config: SessionConfig,
        worktrees: Vec<WorktreeInfo>,
    ) {
        let raw_name = self.next_session_name();
        let Some(project) = self.active_project() else {
            return;
        };
        let name = if project.is_admin {
            format!("admin-{raw_name}")
        } else {
            raw_name
        };
        let roles = &project.config.roles;

        match roles.len() {
            0 => {
                // No roles configured — spawn with default developer permissions.
                config.role = DEFAULT_ROLE_NAME.to_string();
                config.permissions = default_developer_permissions();
                self.do_spawn_session(name, &config, worktrees, None);
            }
            1 => {
                // Exactly one role — auto-assign it.
                config.role = roles[0].name.clone();
                config.permissions = roles[0].permissions.clone();
                self.do_spawn_session(name, &config, worktrees, None);
            }
            _ => {
                // 2+ roles — show the role selector.
                self.pending_spawn_name = Some(name);
                self.pending_spawn_config = Some(config);
                self.pending_spawn_worktrees = worktrees;
                self.modal = modals::Modal::RoleSelector(modals::RoleSelectorModal::default());
            }
        }
    }

    fn restart_active_session(&mut self) {
        let Some(session) = self.sessions.get(self.active_index) else {
            return;
        };
        let Some(agent_session_id) = session.info.agent_session_id.clone() else {
            return;
        };

        let role = session.info.role.clone();
        let cwd = session.info.cwd.clone();
        let additional_dirs = session.info.additional_dirs.clone();

        let permissions = self.resolve_role_permissions(&role);
        let config = SessionConfig {
            resume_session_id: Some(agent_session_id.clone()),
            agent_session_id: Some(agent_session_id),
            cwd,
            additional_dirs,
            role,
            permissions,
            vm_id: session.info.vm_id.clone(),
            container_id: session.info.container_id.clone(),
        };

        let (rows, cols) = self.content_area_size();
        let session = &mut self.sessions[self.active_index];
        match session.restart(&config, rows, cols) {
            Ok(()) => {
                self.save_state();
                self.set_status(StatusLevel::Info, "Session restarted");
            }
            Err(e) => {
                error!("Failed to restart session: {e}");
                self.set_error(format!("Failed to restart session: {e:#}"));
            }
        }
    }

    fn close_active_session(&mut self) {
        if self.sessions.is_empty() {
            return;
        }

        let Some(session) = self.sessions.get(self.active_index) else {
            return;
        };

        let session_id = session.info.id;

        // Find the project this session belongs to
        let project_id = self
            .projects
            .iter()
            .find(|p| p.session_ids.contains(&session_id))
            .map(|p| p.id)
            .unwrap_or_default();

        // Soft-delete in DB
        if let Err(e) = self.db.soft_delete_session(session_id) {
            error!("Failed to soft-delete session in DB: {e}");
        }

        // Remove from the session list (do NOT kill backend or remove worktrees yet)
        let removed_session = self.sessions.remove(self.active_index);
        let session_name = removed_session.info.name.clone();

        // Clean up terminal view state
        self.session_terminal_views.remove(&session_id);

        // Remove session from its project
        for project in &mut self.projects {
            project.session_ids.retain(|id| *id != session_id);
        }

        if self.sessions.is_empty() {
            self.active_index = 0;
        } else if self.active_index >= self.sessions.len() {
            self.active_index = self.sessions.len() - 1;
        }

        // Finalize any existing pending delete before storing the new one
        self.finalize_pending_delete();

        self.pending_delete = Some(PendingDelete {
            session: removed_session,
            session_id,
            project_id,
            created_at: std::time::Instant::now(),
        });

        self.set_status(
            StatusLevel::Info,
            format!("Deleted '{session_name}'. Ctrl+Z to undo"),
        );

        // Sync to shared state for other instances
        self.save_state();
    }

    /// Recreate git worktrees from shared worktree metadata.
    ///
    /// For each worktree whose branch still exists, runs `git worktree add`
    /// to restore it. Returns the successfully recreated worktrees.
    fn recreate_worktrees(worktrees: &[SharedWorktree]) -> Vec<WorktreeInfo> {
        let mut infos = Vec::new();
        for wt in worktrees {
            if git::branch_exists(&wt.repo_path, &wt.branch) {
                match git::add_existing_worktree(&wt.repo_path, &wt.branch) {
                    Ok(wt_path) => {
                        infos.push(WorktreeInfo {
                            repo_path: wt.repo_path.clone(),
                            worktree_path: wt_path,
                            branch: wt.branch.clone(),
                        });
                    }
                    Err(e) => {
                        error!("Failed to recreate worktree for {}: {e}", wt.branch);
                    }
                }
            }
        }
        infos
    }

    /// Finalize a pending delete — kill the backend session.
    ///
    /// Worktrees, containers, and VMs are intentionally preserved on disk
    /// so that restored sessions (Ctrl+U) can reuse them without re-cloning.
    fn finalize_pending_delete(&mut self) {
        if let Some(pending) = self.pending_delete.take() {
            // Clean up agent metrics file.
            if let Some(ref sid) = pending.session.info.agent_session_id {
                if let Some(metrics_dir) = crate::paths::metrics_directory() {
                    let _ = std::fs::remove_file(metrics_dir.join(format!("{sid}.json")));
                }
            }
            pending.session.kill();
        }
    }

    /// Undo the most recent session delete (Ctrl+Z).
    fn undo_delete(&mut self) {
        let Some(pending) = self.pending_delete.take() else {
            return;
        };

        if let Err(e) = self.db.restore_session(pending.session_id) {
            error!("Failed to restore session in DB: {e}");
            self.set_error("Failed to undo delete");
            return;
        }

        let session_name = pending.session.info.name.clone();
        self.sessions.push(pending.session);
        self.active_index = self.sessions.len() - 1;
        self.associate_session_with_project(pending.session_id, pending.project_id);
        self.save_state();

        self.set_status(StatusLevel::Success, format!("Restored '{session_name}'"));
    }

    /// Open the restore deleted sessions modal (Ctrl+U).
    fn open_restore_sessions_modal(&mut self) {
        let Some(project) = self.active_project() else {
            return;
        };
        let project_id = project.id;
        match self.db.list_deleted_sessions_for_project(project_id) {
            Ok(list) => {
                self.modal =
                    modals::Modal::RestoreSessions(modals::RestoreSessionsModal { list, index: 0 });
            }
            Err(e) => {
                error!("Failed to list deleted sessions: {e}");
                self.set_error("Failed to list deleted sessions");
            }
        }
    }

    /// Restore a soft-deleted session: un-delete in DB, recreate worktrees, and spawn.
    fn restore_deleted_session(&mut self, deleted: DeletedSessionInfo) {
        if let Err(e) = self.db.restore_session(deleted.id) {
            error!("Failed to restore session in DB: {e}");
            self.set_error("Failed to restore session");
            return;
        }

        let worktree_infos = Self::recreate_worktrees(&deleted.worktrees);
        let cwd = worktree_infos
            .first()
            .map(|wt| wt.worktree_path.clone())
            .or(deleted.cwd.clone());

        let permissions = self.resolve_role_permissions(&deleted.role);
        let config = SessionConfig {
            resume_session_id: deleted.agent_session_id.clone(),
            agent_session_id: deleted.agent_session_id,
            cwd,
            additional_dirs: Vec::new(),
            role: deleted.role,
            permissions,
            vm_id: None,
            container_id: None,
        };

        let session_name = deleted.name.clone();
        let (rows, cols) = self.content_area_size();

        match Session::spawn(
            session_name.clone(),
            rows,
            cols,
            &config,
            self.backends.default_backend(),
            &self.provider,
        ) {
            Ok(mut session) => {
                session.info.id = deleted.id;
                session.info.worktrees = worktree_infos;
                let session_id = session.info.id;
                self.sessions.push(session);
                self.active_index = self.sessions.len() - 1;
                self.focus = InputFocus::Terminal;

                self.associate_session_with_project(session_id, deleted.project_id);
                self.save_state();

                self.set_status(StatusLevel::Success, format!("Restored '{session_name}'"));
            }
            Err(e) => {
                error!("Failed to spawn restored session: {e}");
                self.set_error(format!("Failed to restore session: {e:#}"));
            }
        }
    }

    /// Get sessions belonging to the active project.
    pub(crate) fn active_project_sessions(&self) -> Vec<usize> {
        match self.active_project() {
            Some(project) => self
                .sessions
                .iter()
                .enumerate()
                .filter(|(_, s)| project.session_ids.contains(&s.info.id))
                .map(|(i, _)| i)
                .collect(),
            None => Vec::new(),
        }
    }

    /// Get the active session's index within the active project's session list.
    pub(crate) fn active_session_in_project(&self) -> usize {
        let project_sessions = self.active_project_sessions();
        project_sessions
            .iter()
            .position(|&i| i == self.active_index)
            .unwrap_or(0)
    }

    /// Ensure a session is associated with a project.
    /// Tries to add to the session's original project, falling back to the first project.
    fn associate_session_with_project(&mut self, session_id: SessionId, project_id: ProjectId) {
        let mut found_project = false;
        if let Some(project) = self.projects.iter_mut().find(|p| p.id == project_id) {
            if !project.session_ids.contains(&session_id) {
                project.session_ids.push(session_id);
            }
            found_project = true;
        }

        // If session's project doesn't exist in this instance, add to the first project
        if !found_project {
            if let Some(project) = self.projects.first_mut() {
                if !project.session_ids.contains(&session_id) {
                    project.session_ids.push(session_id);
                }
            }
        }
    }

    /// Apply shared session metadata to a local session info.
    /// Used when updating or adopting sessions from shared state.
    fn apply_shared_session_metadata(session: &mut Session, shared: &sync::SharedSession) {
        session.info.name = shared.name.clone();
        session.info.role = shared.role.clone();
        session.info.cwd = shared.cwd.clone();
        session.info.additional_dirs = shared.additional_dirs.clone();
        session.info.agent_session_id = shared.agent_session_id.clone();
        session.info.worktrees = shared.worktrees.iter().cloned().map(Into::into).collect();
    }

    pub fn update(&mut self, msg: AppMessage) {
        match msg {
            AppMessage::KeyPress(code, mods) => self.handle_key(code, mods),
            AppMessage::MouseScrollUp => self.scroll_terminal_up(MOUSE_SCROLL_LINES),
            AppMessage::MouseScrollDown => self.scroll_terminal_down(MOUSE_SCROLL_LINES),
            AppMessage::MouseClick { x, y, modifiers } => self.handle_mouse_click(x, y, modifiers),
            AppMessage::MouseDrag { x, y } => self.handle_mouse_drag(x, y),
            AppMessage::MouseUp { x, y } => self.handle_mouse_up(x, y),
            AppMessage::Resize(cols, rows) => self.handle_resize(cols, rows),
            AppMessage::ExternalStateChange(delta) => self.handle_external_state_change(delta),
            AppMessage::VmReady { vm_id } => self.handle_vm_ready(&vm_id),
            AppMessage::VmFailed { error } => self.handle_vm_failed(&error),
        }
    }

    /// Get the current terminal view for the active session.
    pub(crate) fn active_terminal_view(&self) -> TerminalView {
        self.sessions
            .get(self.active_index)
            .and_then(|s| self.session_terminal_views.get(&s.info.id))
            .copied()
            .unwrap_or(TerminalView::Claude)
    }

    pub(crate) fn with_active_parser(&self, f: impl FnOnce(&mut vt100::Parser)) {
        if let Some(session) = self.sessions.get(self.active_index) {
            let parser_arc = if self.active_terminal_view() == TerminalView::Shell {
                session.shell_pane.as_ref().map(|sp| &sp.parser)
            } else {
                None
            }
            .unwrap_or(&session.parser);
            if let Ok(mut parser) = parser_arc.lock() {
                f(&mut parser);
            }
        }
    }

    /// Toggle the terminal view between Claude and Shell for the active session.
    /// Lazily spawns the shell pane on first toggle.
    pub(crate) fn toggle_shell_view(&mut self) {
        let Some(session) = self.sessions.get(self.active_index) else {
            return;
        };
        let session_id = session.info.id;
        let needs_shell = session.shell_pane.is_none();

        let current = self
            .session_terminal_views
            .get(&session_id)
            .copied()
            .unwrap_or(TerminalView::Claude);

        match current {
            TerminalView::Claude => {
                // Lazily create the shell pane if it doesn't exist
                if needs_shell {
                    let (rows, cols) = self.content_area_size();
                    let session = &mut self.sessions[self.active_index];
                    if let Err(e) = session.ensure_shell_pane(rows, cols) {
                        error!("Failed to create shell pane: {e}");
                        self.set_error(format!("Failed to create shell: {e:#}"));
                        return;
                    }
                    self.save_state();
                }
                self.session_terminal_views
                    .insert(session_id, TerminalView::Shell);
            }
            TerminalView::Shell => {
                self.session_terminal_views
                    .insert(session_id, TerminalView::Claude);
            }
        }
    }

    pub(crate) fn scroll_terminal_up(&mut self, lines: usize) {
        self.text_selection = None;
        self.with_active_parser(|parser| {
            let current = parser.screen().scrollback();
            parser.screen_mut().set_scrollback(current + lines);
        });
    }

    pub(crate) fn scroll_terminal_down(&mut self, lines: usize) {
        self.text_selection = None;
        self.with_active_parser(|parser| {
            let current = parser.screen().scrollback();
            parser
                .screen_mut()
                .set_scrollback(current.saturating_sub(lines));
        });
    }

    pub(crate) fn page_scroll_amount(&self) -> usize {
        let (rows, _) = self.content_area_size();
        (rows as usize) / 2
    }

    fn handle_mouse_click(&mut self, x: u16, y: u16, modifiers: KeyModifiers) {
        use crate::ui::links;

        let area = Rect::new(0, 0, self.terminal_cols, self.terminal_rows);
        let areas = layout::compute_layout(area, self.show_info_panel);
        let border_block = Block::default().borders(Borders::ALL);

        // Ctrl+Click: URL opening (terminal-relative, existing behavior)
        if modifiers.contains(KeyModifiers::CONTROL) {
            self.text_selection = None;
            let inner = border_block.inner(areas.terminal);

            if inner.contains(Position::new(x, y)) {
                let screen_col = (x - inner.x) as usize;
                let screen_row = (y - inner.y) as usize;
                self.with_active_parser(|parser| {
                    let rows = links::extract_screen_rows(parser.screen());
                    let detected = links::detect_urls(&rows);
                    if let Some(url) = links::url_at_position(&detected, screen_row, screen_col) {
                        helpers::open_url(url);
                    }
                });
            }
            return;
        }

        // Find which pane was clicked; use inner area (excluding borders).
        let pos = Position::new(x, y);
        let pane_rects = [Some(areas.terminal), areas.left_panel, areas.info_panel];
        let pane_inner = pane_rects
            .into_iter()
            .flatten()
            .find(|r| r.contains(pos))
            .map(|r| border_block.inner(r));

        let Some(inner) = pane_inner else {
            self.text_selection = None;
            return;
        };

        let pane = PaneBounds::from_rect(inner);
        let anchor = TermPos {
            row: y as usize,
            col: x as usize,
        };
        self.text_selection = Some(Selection::new(anchor, pane));
    }

    fn handle_mouse_drag(&mut self, x: u16, y: u16) {
        if let Some(ref mut sel) = self.text_selection {
            let (cx, cy) = sel.pane.clamp(x, y);
            sel.cursor = TermPos {
                row: cy as usize,
                col: cx as usize,
            };
        }
    }

    fn handle_mouse_up(&mut self, x: u16, y: u16) {
        self.handle_mouse_drag(x, y);

        if let Some(ref mut sel) = self.text_selection {
            sel.dragging = false;

            // If anchor == cursor, it was just a click (no drag) — clear selection
            if sel.anchor == sel.cursor {
                self.text_selection = None;
            }
        }
    }

    fn copy_selection_to_clipboard(&mut self) {
        let text = match &self.selected_text_cache {
            Some(t) if !t.is_empty() => t.clone(),
            _ => return,
        };

        let Some(clipboard) = &mut self.clipboard else {
            self.set_error("Clipboard not available");
            return;
        };

        if let Err(e) = clipboard.set_text(&text) {
            self.set_error(format!("Clipboard write failed: {e}"));
            return;
        }

        self.text_selection = None;
        self.selected_text_cache = None;
        self.set_status(StatusLevel::Info, "Copied to clipboard");
    }

    fn paste_from_clipboard(&mut self) {
        let Some(clipboard) = &mut self.clipboard else {
            self.set_error("Clipboard not available");
            return;
        };

        let text = match clipboard.get_text() {
            Ok(t) => t,
            Err(e) => {
                self.set_error(format!("Clipboard read failed: {e}"));
                return;
            }
        };

        if text.is_empty() {
            return;
        }

        if let Some(session) = self.sessions.get(self.active_index) {
            let bytes = text.into_bytes();
            let result = if let (TerminalView::Shell, Some(shell)) =
                (self.active_terminal_view(), &session.shell_pane)
            {
                shell.send_input(bytes)
            } else {
                session.send_input(bytes)
            };
            if let Err(e) = result {
                error!("Failed to send pasted input: {e}");
            }
        }
    }

    pub(crate) fn submit_role_editor(&mut self) {
        let name = self.role_editor_name.value().trim().to_string();
        if name.is_empty() {
            self.set_error("Role name cannot be empty");
            return;
        }

        let modals::Modal::EditProject(ref ep) = self.modal else {
            return;
        };

        // Check uniqueness (exclude the role being edited)
        let duplicate = ep
            .role_editor_roles
            .iter()
            .enumerate()
            .any(|(i, r)| r.name == name && Some(i) != self.role_editor_editing_index);
        if duplicate {
            self.set_error(format!("Role name '{name}' already exists"));
            return;
        }

        let allowed_tools = self.role_editor_allowed_tools.items.clone();
        let disallowed_tools = self.role_editor_disallowed_tools.items.clone();

        let system_prompt = self.role_editor_system_prompt.value().trim().to_string();
        let append_system_prompt = if system_prompt.is_empty() {
            None
        } else {
            Some(system_prompt)
        };

        // Parse env KEY=VALUE items into a HashMap
        let env: HashMap<String, String> = self
            .role_editor_env
            .items
            .iter()
            .filter_map(|item| {
                let (k, v) = item.split_once('=')?;
                let k = k.trim();
                if k.is_empty() {
                    return None;
                }
                Some((k.to_string(), v.to_string()))
            })
            .collect();

        // Preserve fields not exposed in the editor (permission_mode, tools)
        let base_permissions = self
            .role_editor_editing_index
            .and_then(|idx| ep.role_editor_roles.get(idx))
            .map(|r| &r.permissions);

        let role = RoleConfig {
            name,
            description: self.role_editor_description.value().trim().to_string(),
            permissions: RolePermissions {
                permission_mode: base_permissions.and_then(|p| p.permission_mode.clone()),
                allowed_tools,
                disallowed_tools,
                tools: base_permissions.and_then(|p| p.tools.clone()),
                append_system_prompt,
                env,
            },
        };

        let modals::Modal::EditProject(ref mut ep) = self.modal else {
            return;
        };

        match self.role_editor_editing_index {
            Some(idx) => {
                ep.role_editor_roles[idx] = role;
            }
            None => {
                ep.role_editor_roles.push(role);
                ep.role_editor_list_index = ep.role_editor_roles.len() - 1;
            }
        }

        self.set_status(StatusLevel::Success, "Role saved");
        // Return to edit-project modal (roles field) instead of role list
        self.show_role_editor = false;
        self.role_editor_snapshot = None;
        if let modals::Modal::EditProject(ref mut ep) = self.modal {
            ep.field = EditProjectField::Roles;
        }
    }

    pub(crate) fn spawn_worktree_session(
        &mut self,
        repo_paths: &[PathBuf],
        new_branch: &str,
        base_branch: &str,
    ) {
        let mut worktree_infos = Vec::new();
        let mut worktree_paths = Vec::new();

        for repo_path in repo_paths {
            match git::create_worktree(repo_path, new_branch, base_branch) {
                Ok(worktree_path) => {
                    worktree_infos.push(WorktreeInfo {
                        repo_path: repo_path.clone(),
                        worktree_path: worktree_path.clone(),
                        branch: new_branch.to_string(),
                    });
                    worktree_paths.push(worktree_path);
                }
                Err(e) => {
                    // Roll back already-created worktrees
                    for info in &worktree_infos {
                        if let Err(re) = git::remove_worktree(&info.repo_path, &info.worktree_path)
                        {
                            error!("Failed to roll back worktree: {re}");
                        }
                    }
                    error!("Failed to create worktree in {}: {e}", repo_path.display());
                    self.set_error(format!("Failed to create worktree: {e:#}"));
                    return;
                }
            }
        }

        let config = SessionConfig {
            cwd: Some(worktree_paths[0].clone()),
            additional_dirs: worktree_paths[1..].to_vec(),
            ..SessionConfig::default()
        };
        self.prepare_spawn(config, worktree_infos);
    }

    pub(crate) fn do_spawn_session(
        &mut self,
        name: String,
        config: &SessionConfig,
        worktrees: Vec<WorktreeInfo>,
        target_project_index: Option<usize>,
    ) {
        let (rows, cols) = self.content_area_size();

        let mut config = config.clone();
        if config.agent_session_id.is_none() {
            config.agent_session_id = Some(uuid::Uuid::new_v4().to_string());
        }

        // Inject statusline env vars so the metrics script knows which session this is.
        if let Some(ref sid) = config.agent_session_id {
            config
                .permissions
                .env
                .insert("THURBOX_SESSION_ID".into(), sid.clone());
        }
        if let Some(metrics_dir) = crate::paths::metrics_directory() {
            config.permissions.env.insert(
                "THURBOX_METRICS_DIR".into(),
                metrics_dir.to_string_lossy().into(),
            );
        }

        // When a VM was just provisioned, use the VM backend and take the
        // placeholder's name instead of generating a new one.
        let vm_id = self.pending_vm_id.take();
        let container_id = self.pending_container_id.take();
        let placeholder = if vm_id.is_some() {
            self.vm_placeholder.take()
        } else if container_id.is_some() {
            self.container_placeholder.take()
        } else {
            None
        };
        let placeholder_id = placeholder.as_ref().map(|ph| ph.id);
        // Tie the session to its specific VM or container.
        if vm_id.is_some() {
            config.vm_id = vm_id.clone();
        }
        if container_id.is_some() {
            config.container_id = container_id.clone();
        }
        let (backend, spawn_name): (Arc<dyn SessionBackend>, String) = if vm_id.is_some() {
            let vm_name = placeholder
                .as_ref()
                .map(|ph| ph.name.clone())
                .unwrap_or_else(|| name.clone());
            match self.backends.get("qemu-vm") {
                Some(b) => (Arc::clone(b), vm_name),
                None => {
                    self.set_error("QEMU VM backend disappeared".to_string());
                    return;
                }
            }
        } else if container_id.is_some() {
            let dc_name = placeholder
                .as_ref()
                .map(|ph| ph.name.clone())
                .unwrap_or_else(|| name.clone());
            match self.backends.get("devcontainer") {
                Some(b) => (Arc::clone(b), dc_name),
                None => {
                    self.set_error("Container backend disappeared".to_string());
                    return;
                }
            }
        } else {
            (Arc::clone(self.backends.default_backend()), name)
        };

        match Session::spawn(spawn_name, rows, cols, &config, &backend, &self.provider) {
            Ok(mut session) => {
                session.info.worktrees = worktrees;
                let session_id = session.info.id;

                if let Some(ref vid) = vm_id {
                    session.info.vm_id = Some(vid.clone());

                    // Replace the placeholder ID with the real session ID.
                    if let Some(ph_id) = placeholder_id {
                        if let Some(project) = self.projects.get_mut(self.active_project_index) {
                            project.session_ids.retain(|id| *id != ph_id);
                        }
                    }

                    // Persist SSH port and QEMU PID from the running VM instance.
                    let qemu_pid = if let Some(ref mgr) = self.vm_manager {
                        if let Ok(mgr) = mgr.lock() {
                            if let Some(inst) = mgr.get_instance(vid) {
                                if let Err(e) = self.db.update_vm_ssh_port(vid, inst.ssh_port) {
                                    error!(vm_id = %vid, "Failed to persist VM SSH port: {e}");
                                }
                            }
                            mgr.qemu_pid(vid)
                        } else {
                            None
                        }
                    } else {
                        None
                    };

                    if let Err(e) = self.db.update_vm_state(
                        vid,
                        &crate::session::VmState::Ready,
                        qemu_pid,
                        None,
                    ) {
                        error!(vm_id = %vid, "Failed to update VM state to Ready: {e}");
                    }
                }

                if let Some(ref cid) = container_id {
                    session.info.container_id = Some(cid.clone());

                    // Replace the placeholder ID with the real session ID.
                    if let Some(ph_id) = placeholder_id {
                        if let Some(project) = self.projects.get_mut(self.active_project_index) {
                            project.session_ids.retain(|id| *id != ph_id);
                        }
                    }

                    // Update container state to Ready with docker ID.
                    let docker_id = if let Some(ref mgr) = self.container_manager {
                        if let Ok(mgr) = mgr.lock() {
                            mgr.get_instance(cid)
                                .and_then(|i| i.docker_container_id.clone())
                        } else {
                            None
                        }
                    } else {
                        None
                    };

                    if let Err(e) = self.db.update_container_state(
                        cid,
                        &crate::session::ContainerState::Ready,
                        docker_id.as_deref(),
                        None,
                    ) {
                        error!(container_id = %cid, "Failed to update container state to Ready: {e}");
                    }
                }

                self.sessions.push(session);
                self.active_index = self.sessions.len() - 1;
                self.focus = InputFocus::Terminal;
                self.status_message = None;

                // Only add to project if not already there
                let project_index = target_project_index.unwrap_or(self.active_project_index);
                if let Some(project) = self.projects.get_mut(project_index) {
                    if !project.session_ids.contains(&session_id) {
                        project.session_ids.push(session_id);
                    }
                }

                // Sync to shared state for other instances — this upserts
                // the session into the DB (required before FK references).
                self.save_state();

                // Link the VM record to this session. This must happen AFTER
                // save_state() because vms.session_id has a FK constraint
                // referencing sessions(id), so the session row must exist first.
                if let Some(ref vid) = vm_id {
                    let sid_str = session_id.to_string();
                    if let Err(e) = self.db.update_vm_session(vid, &sid_str) {
                        error!(
                            vm_id = %vid,
                            session_id = %sid_str,
                            "Failed to link VM record to session: {e}"
                        );
                    }
                }

                // Link the container record to this session (same FK constraint).
                if let Some(ref cid) = container_id {
                    let sid_str = session_id.to_string();
                    if let Err(e) = self.db.update_container_session(cid, &sid_str) {
                        error!(
                            container_id = %cid,
                            session_id = %sid_str,
                            "Failed to link container record to session: {e}"
                        );
                    }
                }
            }
            Err(e) => {
                error!("Failed to spawn session: {e}");
                self.set_error(format!("Failed to start claude: {e:#}"));
            }
        }
    }

    pub(crate) fn submit_add_project(&mut self) {
        let modals::Modal::AddProject(ref mut ap) = self.modal else {
            return;
        };

        let name = ap.name.value().trim().to_string();

        // If the path field has content, treat it as an un-added repo
        let pending_path = ap.path.value().trim().to_string();
        if !pending_path.is_empty() {
            ap.repos.push(PathBuf::from(pending_path));
        }

        if name.is_empty() || ap.repos.is_empty() {
            self.set_error("Project name and at least one repo are required");
            return;
        }

        let config = ProjectConfig {
            name,
            repos: ap.repos.clone(),
            roles: vec![default_developer_role()],
            mcp_servers: Vec::new(),
            id: None,
        };
        let info = ProjectInfo::new(config);
        self.projects.push(info);
        self.active_project_index = self.projects.len() - 1;

        // Persist project to DB at point of change
        self.save_project_to_db(&self.projects[self.active_project_index].clone());

        // Close modal and clear inputs
        self.close_add_project_modal();
        self.set_status(StatusLevel::Info, "Project created");
    }

    pub(crate) fn open_edit_project_modal(&mut self) {
        let Some(project) = self.active_project() else {
            return;
        };
        if project.is_admin {
            self.set_error("Cannot edit admin project");
            return;
        }

        let name = project.config.name.clone();
        let repos = project.config.repos.clone();
        let roles = project.config.roles.clone();
        let mcp_servers = project.config.mcp_servers.clone();
        let id = project.id;

        let mut ep = modals::EditProjectModal::default();
        ep.name.set(&name);
        ep.field = EditProjectField::Name;
        ep.repos = repos;
        ep.original_id = Some(id);
        ep.role_editor_roles = roles;
        ep.mcp_servers = mcp_servers;
        self.modal = modals::Modal::EditProject(Box::new(ep));
    }

    pub(crate) fn submit_edit_project(&mut self) {
        let modals::Modal::EditProject(ref mut ep) = self.modal else {
            return;
        };

        let name = ep.name.value().trim().to_string();

        // If the path field has content, treat it as an un-added repo
        let pending_path = ep.path.value().trim().to_string();
        if !pending_path.is_empty() {
            ep.repos.push(PathBuf::from(pending_path));
        }

        if name.is_empty() || ep.repos.is_empty() {
            self.set_error("Project name and at least one repo are required");
            return;
        }

        let Some(original_id) = ep.original_id else {
            return;
        };

        let repos = ep.repos.clone();
        let roles = ep.role_editor_roles.clone();
        let mcp_servers = ep.mcp_servers.clone();

        // Find project by original ID (stable across renames)
        let Some(project) = self.projects.iter_mut().find(|p| p.id == original_id) else {
            self.set_error("Project not found");
            return;
        };

        // Update config without regenerating ID
        project.config.name = name;
        project.config.repos = repos;
        project.config.roles = roles;
        project.config.mcp_servers = mcp_servers;

        // Persist project to DB at point of change
        let project_clone = project.clone();
        self.save_project_to_db(&project_clone);
        self.status_message = None;

        self.close_edit_project_modal();
        self.set_status(StatusLevel::Info, "Project saved");
    }

    pub(crate) fn close_edit_project_modal(&mut self) {
        self.modal.close();
        self.show_role_editor = false;
        self.show_mcp_editor = false;
    }

    pub(crate) fn show_delete_project_modal(&mut self) {
        let Some(project) = self.active_project() else {
            return;
        };

        // Safety checks
        if project.is_admin {
            self.set_error("Cannot delete admin project");
            return;
        }
        if self.projects.len() <= 1 {
            self.set_error("Cannot delete last project");
            return;
        }

        // Copy project name before borrowing self
        let project_name = project.config.name.clone();

        self.modal = modals::Modal::DeleteProject(modals::DeleteProjectModal {
            project_name,
            confirmation: modals::TextInput::new(),
            error: None,
        });
    }

    pub(crate) fn delete_active_project(&mut self) {
        // Validate confirmation
        if let modals::Modal::DeleteProject(ref mut dp) = self.modal {
            if dp.confirmation.value() != dp.project_name {
                dp.error = Some("Project name doesn't match".to_string());
                return;
            }
        } else {
            return;
        }

        // Get project session IDs, ID, and name before removal
        let Some(project) = self.active_project() else {
            self.modal.close();
            return;
        };

        let session_ids_to_close: Vec<_> = project.session_ids.clone();
        let project_name = project.config.name.clone();
        let project_id = project.id;

        // Soft-delete sessions in DB
        for session_id in &session_ids_to_close {
            if let Err(e) = self.db.soft_delete_session(*session_id) {
                error!("Failed to soft-delete session in DB: {e}");
            }
        }

        // Soft-delete project in DB
        if let Err(e) = self.db.soft_delete_project(project_id) {
            error!("Failed to soft-delete project in DB: {e}");
        }

        // Close all sessions belonging to this project
        for session_id in session_ids_to_close {
            if let Some(session_pos) = self.sessions.iter().position(|s| s.info.id == session_id) {
                // Clean up agent metrics file.
                if let Some(ref sid) = self.sessions[session_pos].info.agent_session_id {
                    if let Some(metrics_dir) = crate::paths::metrics_directory() {
                        let _ = std::fs::remove_file(metrics_dir.join(format!("{sid}.json")));
                    }
                }
                self.sessions[session_pos].kill();
                self.sessions.remove(session_pos);
            }
        }

        // Remove project from list
        self.projects.remove(self.active_project_index);

        // Adjust active index
        if self.active_project_index >= self.projects.len() {
            self.active_project_index = self.projects.len().saturating_sub(1);
        }

        // Close modal and show success
        self.modal.close();
        self.set_status(
            StatusLevel::Success,
            format!("Deleted project '{project_name}'"),
        );
    }

    /// When switching projects, select the first session of the new project.
    pub(crate) fn sync_active_session_to_project(&mut self) {
        let project_sessions = self.active_project_sessions();
        if let Some(&first) = project_sessions.first() {
            self.active_index = first;
        }
    }

    /// Switch to the next project (wraps around to first).
    pub(crate) fn switch_project_forward(&mut self) {
        if self.projects.is_empty() {
            return;
        }
        if self.active_project_index + 1 < self.projects.len() {
            self.active_project_index += 1;
        } else {
            self.active_project_index = 0;
        }
        self.sync_active_session_to_project();
    }

    /// Switch to the previous project (wraps around to last).
    pub(crate) fn switch_project_backward(&mut self) {
        if self.projects.is_empty() {
            return;
        }
        if self.active_project_index > 0 {
            self.active_project_index -= 1;
        } else {
            self.active_project_index = self.projects.len() - 1;
        }
        self.sync_active_session_to_project();
    }

    /// Switch to the next session within the active project.
    pub(crate) fn switch_session_forward(&mut self) {
        self.switch_session_by_offset(1);
    }

    /// Switch to the previous session within the active project.
    pub(crate) fn switch_session_backward(&mut self) {
        self.switch_session_by_offset(-1);
    }

    /// Move the active session by `offset` positions within the active project's session list.
    fn switch_session_by_offset(&mut self, offset: isize) {
        let project_sessions = self.active_project_sessions();
        let current_pos = project_sessions
            .iter()
            .position(|&i| i == self.active_index)
            .unwrap_or(0);
        let new_pos = current_pos as isize + offset;
        if new_pos >= 0 && (new_pos as usize) < project_sessions.len() {
            self.active_index = project_sessions[new_pos as usize];
        }
    }

    fn handle_resize(&mut self, cols: u16, rows: u16) {
        self.terminal_cols = cols;
        self.terminal_rows = rows;

        // Collapse info panel if terminal gets too narrow
        if cols < 120 {
            self.show_info_panel = false;
        }

        let (r, c) = self.content_area_size();
        for session in &self.sessions {
            session.resize(r, c);
        }
    }

    pub fn tick(&mut self) {
        self.tick_count = self.tick_count.wrapping_add(1);

        for session in &mut self.sessions {
            session.info.status = if session.has_exited() {
                SessionStatus::Idle
            } else if session.millis_since_last_output() > ACTIVITY_TIMEOUT_MS {
                SessionStatus::Waiting
            } else {
                SessionStatus::Busy
            };
        }

        // Poll for sync results from background worktree sync threads
        self.poll_sync_results();

        // Send deferred inputs whose delay has elapsed
        self.drain_deferred_inputs();

        // Finalize pending delete after undo timeout
        if let Some(ref pending) = self.pending_delete {
            if pending.created_at.elapsed() >= UNDO_TIMEOUT {
                self.finalize_pending_delete();
            }
        }

        // Poll for external state changes from other thurbox instances (DB-based)
        if let Ok(Some(delta)) = sync::poll_for_changes(&mut self.sync_state, &mut self.db) {
            self.handle_external_state_change(delta);
        }

        // Poll for VM provisioning results from background thread
        self.poll_vm_provision();

        // Poll for container provisioning results from background thread
        self.poll_container_provision();

        // Poll for background session restore threads (container/VM startup on restart)
        self.poll_session_restores();

        // Process queued session commands from MCP
        self.process_session_commands();

        // Process scheduled commands (once per second)
        self.process_scheduled_commands();

        // Refresh system metrics periodically
        if self.tick_count % METRICS_REFRESH_TICKS == 0 {
            self.refresh_system_metrics();
        }
    }

    /// Collect CPU/RAM metrics from sysinfo and poll agent metrics files.
    fn refresh_system_metrics(&mut self) {
        self.sys.refresh_cpu_all();
        self.sys.refresh_memory();

        let cpu_percent = self.sys.global_cpu_usage();
        let memory_used = self.sys.used_memory();
        let memory_total = self.sys.total_memory();

        // Refresh only the active session's root process for CPU/RAM.
        let (session_cpu, session_mem) = self.active_session_metrics();

        self.system_metrics = info_panel::SystemMetrics {
            cpu_percent,
            memory_used,
            memory_total,
            session_cpu_percent: session_cpu,
            session_memory_bytes: session_mem,
        };

        // Poll agent metrics files written by the statusline script.
        if let Some(metrics_dir) = crate::paths::metrics_directory() {
            for session in &mut self.sessions {
                if let Some(ref agent_sid) = session.info.agent_session_id {
                    let path = metrics_dir.join(format!("{agent_sid}.json"));
                    if let Ok(content) = std::fs::read_to_string(&path) {
                        if let Ok(raw) = serde_json::from_str::<serde_json::Value>(&content) {
                            session.info.agent_metrics = Some(parse_agent_metrics(&raw));
                        }
                    }
                }
            }
        }
    }

    /// Compute CPU% and memory (bytes) for the active session's root process.
    fn active_session_metrics(&mut self) -> (f32, u64) {
        let active = match self.sessions.get(self.active_index) {
            Some(s) => s,
            None => return (0.0, 0),
        };

        let root_pid = match active.pane_pid() {
            Ok(Some(pid)) => sysinfo::Pid::from_u32(pid),
            _ => return (0.0, 0),
        };

        let refresh_kind = sysinfo::ProcessRefreshKind::nothing()
            .with_memory()
            .with_cpu();
        self.sys.refresh_processes_specifics(
            sysinfo::ProcessesToUpdate::Some(&[root_pid]),
            false,
            refresh_kind,
        );

        if let Some(proc_info) = self.sys.process(root_pid) {
            (proc_info.cpu_usage(), proc_info.memory())
        } else {
            (0.0, 0)
        }
    }

    /// Send deferred inputs whose scheduled tick has arrived.
    fn drain_deferred_inputs(&mut self) {
        let tick = self.tick_count;
        // Partition: send the ones that are ready, keep the rest.
        let mut remaining = Vec::new();
        for (session_id, data, send_at) in std::mem::take(&mut self.deferred_inputs) {
            if tick >= send_at {
                if let Some(session) = self.sessions.iter().find(|s| s.info.id == session_id) {
                    if let Err(e) = session.send_input(data) {
                        error!("Failed to send deferred input: {e}");
                    }
                }
            } else {
                remaining.push((session_id, data, send_at));
            }
        }
        self.deferred_inputs = remaining;
    }

    /// Poll for completed worktree sync results and handle them.
    fn poll_sync_results(&mut self) {
        if let Some(rx) = &self.worktree_sync_rx {
            while let Ok((session_id, result)) = rx.try_recv() {
                self.worktree_sync_completed.push((session_id, result));
            }

            if self.worktree_sync_completed.len() >= self.worktree_sync_pending {
                self.worktree_sync_in_progress = false;
                self.worktree_sync_rx = None;
                self.finish_sync();
            }
        }
    }

    /// Finalize sync: compose status message and send conflict prompts.
    fn finish_sync(&mut self) {
        let results = std::mem::take(&mut self.worktree_sync_completed);
        let mut synced = 0usize;
        let mut conflicts = 0usize;
        let mut errors = Vec::new();

        for (session_id, result) in results {
            match result {
                git::SyncResult::Synced => synced += 1,
                git::SyncResult::Conflict(_) => {
                    conflicts += 1;
                    self.send_conflict_prompt(session_id);
                }
                git::SyncResult::Error(msg) => errors.push(msg),
            }
        }

        if !errors.is_empty() {
            self.set_error(format!("Sync failed: {}", errors.join(", ")));
        } else if conflicts > 0 {
            self.set_status(
                StatusLevel::Info,
                format!("{synced} synced, {conflicts} conflict(s) (sent to Claude)"),
            );
        } else {
            self.set_status(StatusLevel::Success, format!("{synced} worktree(s) synced"));
        }
    }

    /// Send a conflict resolution prompt to a session via bracketed paste,
    /// with a deferred Enter so the app processes the text first.
    fn send_conflict_prompt(&mut self, session_id: SessionId) {
        if let Some(session) = self.sessions.iter().find(|s| s.info.id == session_id) {
            let mut paste = b"\x1b[200~".to_vec();
            paste.extend_from_slice(SYNC_CONFLICT_PROMPT.as_bytes());
            paste.extend_from_slice(b"\x1b[201~");
            if let Err(e) = session.send_input(paste) {
                error!("Failed to send sync prompt to session: {e}");
            } else {
                self.deferred_inputs.push((
                    session_id,
                    b"\r".to_vec(),
                    self.tick_count + DEFERRED_INPUT_DELAY_TICKS,
                ));
            }
        }
    }

    /// Start syncing all worktree sessions with origin/main.
    ///
    /// Worktrees sharing the same parent repo are synced sequentially (to avoid
    /// concurrent `index.lock` contention), while different repos sync in parallel.
    pub(crate) fn start_sync(&mut self) {
        if self.worktree_sync_in_progress {
            return;
        }

        let Some(project) = self.active_project() else {
            self.set_status(StatusLevel::Info, "No active project");
            return;
        };
        let session_ids: std::collections::HashSet<SessionId> =
            project.session_ids.iter().copied().collect();

        let worktree_sessions: Vec<_> = self
            .sessions
            .iter()
            .filter(|s| session_ids.contains(&s.info.id))
            .flat_map(|s| {
                s.info
                    .worktrees
                    .iter()
                    .map(move |wt| (s.info.id, wt.worktree_path.clone(), wt.repo_path.clone()))
            })
            .collect();

        if worktree_sessions.is_empty() {
            self.set_status(StatusLevel::Info, "No worktrees to sync in active project");
            return;
        }

        let count = worktree_sessions.len();
        let (tx, rx) = mpsc::channel();

        // Group worktrees by repo so those sharing a repo sync sequentially.
        let mut by_repo = std::collections::HashMap::<PathBuf, Vec<(SessionId, PathBuf)>>::new();
        for (session_id, worktree_path, repo_path) in worktree_sessions {
            by_repo
                .entry(repo_path)
                .or_default()
                .push((session_id, worktree_path));
        }

        for worktrees in by_repo.into_values() {
            let tx = tx.clone();
            std::thread::spawn(move || {
                for (session_id, worktree_path) in worktrees {
                    let result = git::sync_worktree(&worktree_path);
                    let _ = tx.send((session_id, result));
                }
            });
        }

        self.worktree_sync_in_progress = true;
        self.worktree_sync_rx = Some(rx);
        self.worktree_sync_pending = count;
        self.worktree_sync_completed.clear();
        self.set_status(StatusLevel::Info, format!("Syncing {count} worktree(s)..."));
    }

    /// Handle external state changes detected from other instances.
    fn handle_external_state_change(&mut self, delta: StateDelta) {
        // Update session counter to avoid conflicts
        self.session_counter = self.session_counter.max(delta.counter_increment);

        let has_project_changes = !delta.removed_projects.is_empty()
            || !delta.added_projects.is_empty()
            || !delta.updated_projects.is_empty();

        // Handle removed projects (deleted by other instances)
        for project_id in delta.removed_projects {
            if let Some(pos) = self.projects.iter().position(|p| p.id == project_id) {
                self.projects.remove(pos);
                // Adjust active_project_index if it's out of bounds
                if self.active_project_index >= self.projects.len() {
                    self.active_project_index = self.projects.len().saturating_sub(1);
                }
                tracing::debug!("Removed project {} from external state", project_id);
            }
        }

        // Handle added projects from other instances
        for shared_project in delta.added_projects {
            // Skip if we already have this project
            if self.projects.iter().any(|p| p.id == shared_project.id) {
                continue;
            }

            // Create ProjectInfo from SharedProject
            let project_name = shared_project.name.clone();
            let project = shared_project_to_info(shared_project);

            self.projects.push(project);
            tracing::debug!("Adopted project {} from another instance", project_name);
        }

        // Handle updated projects (metadata changed by other instances)
        for shared_project in delta.updated_projects {
            if let Some(project) = self.projects.iter_mut().find(|p| p.id == shared_project.id) {
                let project_name = shared_project.name.clone();
                project.config.name = shared_project.name;
                project.config.repos = shared_project.repos;
                project.config.roles = shared_project.roles;
                project.config.mcp_servers = shared_project.mcp_servers;
                tracing::debug!("Updated project {} from external state", project_name);
            }
        }

        // Note: no config.toml sync needed — DB is the single source of truth.
        let _ = has_project_changes;

        // Handle removed sessions (deleted by other instances)
        for session_id in delta.removed_sessions {
            if let Some(pos) = self.sessions.iter().position(|s| s.info.id == session_id) {
                self.sessions.remove(pos);
                if self.active_index >= self.sessions.len() && self.active_index > 0 {
                    self.active_index -= 1;
                }

                // Remove session from all projects (cleanup project.session_ids)
                for project in &mut self.projects {
                    project.session_ids.retain(|id| *id != session_id);
                }
            }
        }

        // Handle updated sessions (metadata changed by other instances)
        for shared_session in delta.updated_sessions {
            if let Some(session) = self
                .sessions
                .iter_mut()
                .find(|s| s.info.id == shared_session.id)
            {
                Self::apply_shared_session_metadata(session, &shared_session);
            }
        }

        // Handle added sessions from other instances
        // Try to adopt them from the backend using their backend_id
        for shared_session in delta.added_sessions {
            // Skip if we already have this session
            if self.sessions.iter().any(|s| s.info.id == shared_session.id) {
                continue;
            }

            // Try to adopt from backend
            let (rows, cols) = self.content_area_size();
            let env = self.resolve_role_permissions(&shared_session.role).env;
            match Session::adopt(
                shared_session.name.clone(),
                rows,
                cols,
                &shared_session.backend_id,
                self.backends.default_backend(),
                &self.provider,
                env,
            ) {
                Ok(mut adopted_session) => {
                    // Preserve the original session ID from shared state
                    // (Session::adopt creates a new one, but we need the consistent ID)
                    adopted_session.info.id = shared_session.id;

                    // Update with metadata from shared state
                    Self::apply_shared_session_metadata(&mut adopted_session, &shared_session);

                    // Add to sessions
                    let session_id = adopted_session.info.id;
                    self.sessions.push(adopted_session);

                    // Associate with project
                    self.associate_session_with_project(session_id, shared_session.project_id);

                    tracing::debug!(
                        "Adopted session {} from another instance",
                        shared_session.name
                    );
                }
                Err(e) => {
                    tracing::debug!(
                        "Failed to adopt session {} from backend: {}",
                        shared_session.name,
                        e
                    );

                    // If adopt failed but session has an agent_session_id,
                    // try spawning with --resume (e.g. restored via MCP).
                    if let Some(ref agent_sid) = shared_session.agent_session_id {
                        let worktree_infos = Self::recreate_worktrees(&shared_session.worktrees);
                        let cwd = worktree_infos
                            .first()
                            .map(|wt| wt.worktree_path.clone())
                            .or(shared_session.cwd.clone());

                        let permissions = self.resolve_role_permissions(&shared_session.role);
                        let config = SessionConfig {
                            resume_session_id: Some(agent_sid.clone()),
                            agent_session_id: Some(agent_sid.clone()),
                            cwd,
                            additional_dirs: shared_session.additional_dirs.clone(),
                            role: shared_session.role.clone(),
                            permissions,
                            vm_id: None,
                            container_id: None,
                        };

                        let (rows, cols) = self.content_area_size();
                        if let Ok(mut spawned) = Session::spawn(
                            shared_session.name.clone(),
                            rows,
                            cols,
                            &config,
                            self.backends.default_backend(),
                            &self.provider,
                        ) {
                            spawned.info.id = shared_session.id;
                            spawned.info.worktrees = worktree_infos;
                            let session_id = spawned.info.id;
                            self.sessions.push(spawned);
                            self.associate_session_with_project(
                                session_id,
                                shared_session.project_id,
                            );
                            self.save_state();
                            tracing::debug!(
                                "Spawned restored session {} with --resume",
                                shared_session.name
                            );
                        }
                    }
                }
            }
        }
    }

    pub fn should_quit(&self) -> bool {
        self.should_quit
    }

    pub fn shutdown(mut self) {
        // Finalize any pending delete before shutting down
        self.finalize_pending_delete();
        self.save_state();
        // Do NOT remove worktrees — they persist for resume.
        // Detach from backend sessions without killing them — they persist in tmux.
        for session in self.sessions {
            session.detach();
        }
    }

    /// Set status bar message with the given severity level.
    fn set_status(&mut self, level: StatusLevel, text: impl Into<String>) {
        self.status_message = Some(StatusMessage {
            text: text.into(),
            level,
            created_at: std::time::Instant::now(),
        });
    }

    fn set_error(&mut self, text: impl Into<String>) {
        self.set_status(StatusLevel::Error, text.into());
    }

    fn set_info(&mut self, text: impl Into<String>) {
        self.set_status(StatusLevel::Info, text.into());
    }

    /// Capture current role editor field values as a snapshot for dirty detection.
    fn capture_role_editor_snapshot(&self) -> EditorSnapshot {
        EditorSnapshot {
            fields: vec![
                self.role_editor_name.value().to_string(),
                self.role_editor_description.value().to_string(),
                self.role_editor_allowed_tools.items.join("\n"),
                self.role_editor_disallowed_tools.items.join("\n"),
                self.role_editor_system_prompt.value().to_string(),
                self.role_editor_env.items.join("\n"),
            ],
        }
    }

    /// Capture current MCP editor field values as a snapshot for dirty detection.
    fn capture_mcp_editor_snapshot(&self) -> EditorSnapshot {
        EditorSnapshot {
            fields: vec![
                self.mcp_editor_name.value().to_string(),
                self.mcp_editor_command.value().to_string(),
                self.mcp_editor_args.items.join("\n"),
                self.mcp_editor_env.items.join("\n"),
            ],
        }
    }

    /// Check if the role editor has unsaved changes compared to its snapshot.
    fn is_role_editor_dirty(&self) -> bool {
        match &self.role_editor_snapshot {
            Some(snapshot) => *snapshot != self.capture_role_editor_snapshot(),
            None => false,
        }
    }

    /// Check if the MCP editor has unsaved changes compared to its snapshot.
    fn is_mcp_editor_dirty(&self) -> bool {
        match &self.mcp_editor_snapshot {
            Some(snapshot) => *snapshot != self.capture_mcp_editor_snapshot(),
            None => false,
        }
    }

    /// Close the role editor, clearing snapshot and confirmation state.
    fn close_role_editor(&mut self) {
        self.show_role_editor = false;
        self.role_editor_snapshot = None;
        self.show_discard_confirmation = false;
        if let modals::Modal::EditProject(ref mut ep) = self.modal {
            ep.field = EditProjectField::Roles;
        }
    }

    /// Close the MCP editor, clearing snapshot and confirmation state.
    fn close_mcp_editor(&mut self) {
        self.show_mcp_editor = false;
        self.mcp_editor_snapshot = None;
        self.show_discard_confirmation = false;
        self.mcp_editor_field = crate::app::mcp_editor_modal::McpEditorField::Name;
    }

    /// Persist session state to the SQLite database.
    ///
    /// Only writes sessions and the session counter. Project mutations
    /// (add/edit/delete) write to the DB at their point of change, avoiding
    /// race conditions where a blanket re-write overwrites another instance's edits.
    fn save_state(&self) {
        // Sync session counter
        if let Err(e) = self.db.set_session_counter(self.session_counter) {
            error!("Failed to save session counter to DB: {e}");
        }

        // Upsert all sessions
        for session in &self.sessions {
            let shared_session = self.session_to_shared(session);
            if let Err(e) = self.db.upsert_session(&shared_session) {
                error!("Failed to upsert session to DB: {e}");
            }
        }
    }

    /// Persist a single project to the DB (insert or update).
    ///
    /// Handles the edge case where a project with the same ID was previously
    /// soft-deleted: the INSERT fails on the PK, so we restore and update instead.
    fn save_project_to_db(&self, project: &ProjectInfo) {
        let id = project.id;
        let name = &project.config.name;
        let repos = &project.config.repos;

        if self.db.project_exists(id).unwrap_or(false) {
            if let Err(e) = self.db.update_project(id, name, repos) {
                error!("Failed to update project in DB: {e}");
            }
        } else if self.db.insert_project(id, name, repos).is_err() {
            // PK conflict from a soft-deleted row — restore then update.
            if let Err(e) = self
                .db
                .restore_project(id)
                .and_then(|()| self.db.update_project(id, name, repos))
            {
                error!("Failed to restore/update soft-deleted project {id}: {e}");
            }
        }

        if let Err(e) = self.db.replace_roles(id, &project.config.roles) {
            error!("Failed to save project roles to DB: {e}");
        }

        if let Err(e) = self.db.replace_mcp_servers(id, &project.config.mcp_servers) {
            error!("Failed to save project MCP servers to DB: {e}");
        }
    }

    /// Build a SharedSession from a local Session.
    fn session_to_shared(&self, session: &Session) -> sync::SharedSession {
        sync::SharedSession {
            id: session.info.id,
            name: session.info.name.clone(),
            project_id: self
                .projects
                .iter()
                .find(|p| p.session_ids.contains(&session.info.id))
                .map(|p| p.id)
                .unwrap_or_default(),
            role: session.info.role.clone(),
            backend_id: session.backend_id().to_string(),
            backend_type: session.backend_name().to_string(),
            agent_session_id: session.info.agent_session_id.clone(),
            cwd: session.info.cwd.clone(),
            additional_dirs: session.info.additional_dirs.clone(),
            worktrees: session
                .info
                .worktrees
                .iter()
                .cloned()
                .map(Into::into)
                .collect(),
            shell_backend_id: session.info.shell_backend_id.clone(),
            tombstone: false,
            tombstone_at: None,
        }
    }

    /// Load persisted session state from the database.
    ///
    /// Returns `Some(sessions, counter)` if there are active sessions in the DB,
    /// or `None` if no sessions exist (indicating a fresh start or first run).
    pub fn load_persisted_state_from_db(&self) -> Option<(Vec<sync::SharedSession>, usize)> {
        let sessions = self.db.list_active_sessions().ok()?;
        if sessions.is_empty() {
            return None;
        }

        // Only restore sessions that have a agent_session_id (resumable)
        let resumable: Vec<sync::SharedSession> = sessions
            .into_iter()
            .filter(|s| s.agent_session_id.is_some())
            .collect();

        if resumable.is_empty() {
            return None;
        }

        let counter = self.db.get_session_counter().unwrap_or(0);
        Some((resumable, counter))
    }

    /// Restore sessions from the database on startup.
    ///
    /// Local-tmux sessions are restored synchronously (fast — just tmux queries).
    /// Container and VM sessions are restored asynchronously: a placeholder session
    /// with `Provisioning` status is shown immediately, and a background thread
    /// handles the expensive container inspect/start + control mode setup.
    /// `poll_session_restores()` in `tick()` finishes the adopt/respawn once ready.
    pub fn restore_sessions(&mut self, sessions: Vec<sync::SharedSession>, session_counter: usize) {
        self.session_counter = session_counter;

        // Partition sessions by backend type.
        let mut local_sessions = Vec::new();
        let mut async_sessions = Vec::new();
        for shared in sessions {
            if shared.agent_session_id.is_none() {
                continue; // Skip sessions without a claude session ID
            }
            match shared.backend_type.as_str() {
                "devcontainer" | "qemu-vm" => async_sessions.push(shared),
                _ => local_sessions.push(shared),
            }
        }

        // --- Async sessions: create placeholders + spawn background threads ---
        for shared in async_sessions {
            let session_id = shared.id;
            let target_project_index =
                self.find_project_index_for_session(session_id, &shared.project_id);

            // Skip admin sessions — they always start fresh.
            let is_admin = self
                .projects
                .get(target_project_index)
                .is_some_and(|p| p.is_admin);
            if is_admin {
                if let Err(e) = self.db.soft_delete_session(session_id) {
                    error!("Failed to soft-delete old admin session {session_id}: {e}");
                }
                continue;
            }

            // Create a placeholder session with Provisioning status.
            let mut placeholder = SessionInfo::new(shared.name.clone());
            placeholder.id = session_id;
            placeholder.status = SessionStatus::Provisioning;
            if shared.backend_type == "devcontainer" {
                placeholder.container_id = self
                    .db
                    .get_container_by_session(&session_id.to_string())
                    .ok()
                    .flatten()
                    .map(|r| r.id);
            } else if shared.backend_type == "qemu-vm" {
                placeholder.vm_id = self
                    .db
                    .get_vm_by_session(&session_id.to_string())
                    .ok()
                    .flatten()
                    .map(|r| r.id);
            }
            let step_label = if shared.backend_type == "devcontainer" {
                "Restoring container..."
            } else {
                "Restoring VM..."
            };
            placeholder.provisioning_step = Some(step_label.to_string());

            // Add placeholder to the project's session list so it renders.
            if let Some(project) = self.projects.get_mut(target_project_index) {
                if !project.session_ids.contains(&session_id) {
                    project.session_ids.push(session_id);
                }
            }
            self.sessions.push(Session::placeholder(placeholder));

            // Gather data needed by the background thread (DB lookups are fast).
            let (tx, rx) = mpsc::channel();
            let (step_tx, step_rx) = mpsc::channel();

            if shared.backend_type == "devcontainer" {
                self.spawn_container_restore_thread(&shared, tx, step_tx);
            } else {
                self.spawn_vm_restore_thread(&shared, tx, step_tx);
            }

            self.pending_restores.push(PendingRestore {
                session_id,
                rx,
                step_rx,
                shared,
                project_index: target_project_index,
            });
        }

        // --- Local-tmux sessions: restore synchronously (fast) ---
        // Discover existing sessions from the default (local-tmux) backend only.
        let mut discovered = Vec::new();
        let default_backend = self.backends.default_backend().clone();
        match default_backend.discover() {
            Ok(disc) => discovered.extend(disc),
            Err(e) => {
                warn!(
                    backend = default_backend.name(),
                    "Failed to discover sessions from backend: {e}"
                );
            }
        }

        for shared in local_sessions {
            self.restore_single_session(shared, &discovered);
        }

        // Claim ownership of restored sessions in the shared state
        self.save_state();
    }

    /// Spawn a background thread to restore a container for a session.
    fn spawn_container_restore_thread(
        &self,
        shared: &sync::SharedSession,
        tx: mpsc::Sender<Result<RestoreResult, String>>,
        step_tx: mpsc::Sender<String>,
    ) {
        let session_id_str = shared.id.to_string();

        // Look up container record from DB (fast).
        let container_record = match self.db.get_container_by_session(&session_id_str) {
            Ok(Some(rec)) => rec,
            Ok(None) => {
                warn!(session = %shared.id, "No container record found for devcontainer session");
                let _ = tx.send(Err("No container record found".to_string()));
                return;
            }
            Err(e) => {
                warn!(session = %shared.id, "Failed to look up container record: {e}");
                let _ = tx.send(Err(format!("DB lookup failed: {e}")));
                return;
            }
        };

        let docker_id = match container_record.docker_container_id {
            Some(ref id) => id.clone(),
            None => {
                let _ = tx.send(Err(
                    "Container record has no docker container ID".to_string()
                ));
                return;
            }
        };

        let manager = match self.container_manager {
            Some(ref m) => Arc::clone(m),
            None => {
                let _ = tx.send(Err("No ContainerManager available".to_string()));
                return;
            }
        };

        let backend = match self.backends.get("devcontainer") {
            Some(b) => Arc::clone(b),
            None => {
                let _ = tx.send(Err("devcontainer backend not available".to_string()));
                return;
            }
        };

        let config = crate::session::ContainerConfig {
            image: container_record.image.clone(),
            cpus: container_record.cpus,
            memory_mb: container_record.memory_mb,
            firewall_enabled: container_record.firewall_enabled,
            containerfile: container_record.containerfile.clone(),
        };

        let workspace_dir = shared
            .cwd
            .clone()
            .unwrap_or_else(|| std::path::PathBuf::from("/workspaces"));
        let container_id = container_record.id.clone();
        let session_id = shared.id;
        let tx_fallback = tx.clone();

        let spawn_result = std::thread::Builder::new()
            .name(format!(
                "dc-restore-{}",
                &container_id[..8.min(container_id.len())]
            ))
            .spawn(move || {
                let _ = step_tx.send("Starting container...".to_string());

                // Restore container (inspect/start/wait for readiness).
                let mgr = match manager.lock() {
                    Ok(m) => m,
                    Err(e) => {
                        let _ = tx.send(Err(format!("ContainerManager lock poisoned: {e}")));
                        return;
                    }
                };
                if let Err(e) =
                    mgr.restore_container(&container_id, &docker_id, &config, &workspace_dir)
                {
                    let _ = tx.send(Err(format!("Container not reachable: {e:#}")));
                    return;
                }
                drop(mgr);

                let _ = step_tx.send("Connecting to container...".to_string());

                // Establish docker exec control mode connection.
                if let Err(e) = backend.prepare_vm(&container_id) {
                    let _ = tx.send(Err(format!("Control mode failed: {e:#}")));
                    return;
                }

                // Discover sessions on this backend.
                let discovered = match backend.discover() {
                    Ok(d) => d,
                    Err(e) => {
                        warn!(
                            container_id = %container_id,
                            session = %session_id,
                            "Discover failed after container restore: {e}"
                        );
                        Vec::new()
                    }
                };

                info!(
                    container_id = %container_id,
                    session = %session_id,
                    discovered = discovered.len(),
                    "Container restored and control mode established"
                );
                let _ = tx.send(Ok(RestoreResult { discovered }));
            });

        if let Err(e) = spawn_result {
            error!("Failed to spawn container restore thread: {e}");
            let _ = tx_fallback.send(Err(format!("Thread spawn failed: {e}")));
        }
    }

    /// Spawn a background thread to restore a VM for a session.
    fn spawn_vm_restore_thread(
        &self,
        shared: &sync::SharedSession,
        tx: mpsc::Sender<Result<RestoreResult, String>>,
        step_tx: mpsc::Sender<String>,
    ) {
        let session_id_str = shared.id.to_string();

        // Look up VM record from DB (fast).
        let vm_record = match self.db.get_vm_by_session(&session_id_str) {
            Ok(Some(rec)) => rec,
            Ok(None) => {
                warn!(session = %shared.id, "No VM record found for VM session");
                let _ = tx.send(Err("No VM record found".to_string()));
                return;
            }
            Err(e) => {
                warn!(session = %shared.id, "Failed to look up VM record: {e}");
                let _ = tx.send(Err(format!("DB lookup failed: {e}")));
                return;
            }
        };

        let manager = match self.vm_manager {
            Some(ref m) => Arc::clone(m),
            None => {
                let _ = tx.send(Err("No VmManager available".to_string()));
                return;
            }
        };

        let backend = match self.backends.get("qemu-vm") {
            Some(b) => Arc::clone(b),
            None => {
                let _ = tx.send(Err("qemu-vm backend not available".to_string()));
                return;
            }
        };

        let vm_id = vm_record.id.clone();
        let session_id = shared.id;
        let tx_fallback = tx.clone();

        let spawn_result = std::thread::Builder::new()
            .name(format!("vm-restore-{}", &vm_id[..8.min(vm_id.len())]))
            .spawn(move || {
                let _ = step_tx.send("Restoring VM...".to_string());

                // Restore VM (verify SSH reachable).
                let mgr = match manager.lock() {
                    Ok(m) => m,
                    Err(e) => {
                        let _ = tx.send(Err(format!("VmManager lock poisoned: {e}")));
                        return;
                    }
                };
                if let Err(e) = mgr.restore_vm(&vm_record) {
                    let _ = tx.send(Err(format!("VM not reachable: {e:#}")));
                    return;
                }
                drop(mgr);

                let _ = step_tx.send("Connecting to VM...".to_string());

                // Establish SSH control mode connection.
                if let Err(e) = backend.prepare_vm(&vm_id) {
                    let _ = tx.send(Err(format!("SSH control mode failed: {e:#}")));
                    return;
                }

                // Discover sessions on this backend.
                let discovered = match backend.discover() {
                    Ok(d) => d,
                    Err(e) => {
                        warn!(
                            vm_id = %vm_id,
                            session = %session_id,
                            "Discover failed after VM restore: {e}"
                        );
                        Vec::new()
                    }
                };

                info!(
                    vm_id = %vm_id,
                    session = %session_id,
                    discovered = discovered.len(),
                    "VM restored and control mode established"
                );
                let _ = tx.send(Ok(RestoreResult { discovered }));
            });

        if let Err(e) = spawn_result {
            error!("Failed to spawn VM restore thread: {e}");
            let _ = tx_fallback.send(Err(format!("Thread spawn failed: {e}")));
        }
    }

    /// Restore a single local-tmux session synchronously (used during startup).
    fn restore_single_session(
        &mut self,
        shared: sync::SharedSession,
        discovered: &[crate::agent::backend::DiscoveredSession],
    ) {
        let name = shared.name.clone();
        let session_id = shared.id;

        let role = if shared.role.is_empty() {
            DEFAULT_ROLE_NAME.to_string()
        } else {
            shared.role.clone()
        };

        let worktrees: Vec<WorktreeInfo> =
            shared.worktrees.iter().cloned().map(Into::into).collect();

        let agent_session_id = match shared.agent_session_id {
            Some(ref id) => id.clone(),
            None => return,
        };

        let matching_discovered = Self::find_matching_discovered(&shared, discovered);

        // Select the correct backend based on the persisted backend_type.
        let backend = self
            .backends
            .get(&shared.backend_type)
            .cloned()
            .unwrap_or_else(|| self.backends.default_backend().clone());

        let target_project_index =
            self.find_project_index_for_session(session_id, &shared.project_id);
        let is_admin = self
            .projects
            .get(target_project_index)
            .is_some_and(|p| p.is_admin);

        if is_admin {
            if let Some(disc) = matching_discovered {
                if let Err(e) = backend.kill(&disc.backend_id) {
                    tracing::warn!("Failed to kill old admin tmux window: {e}");
                }
            }
            if let Err(e) = self.db.soft_delete_session(session_id) {
                error!("Failed to soft-delete old admin session {session_id}: {e}");
            }
            return;
        }

        // Try to adopt the existing backend session.
        let env = self.resolve_role_permissions(&role).env;
        let adopted = matching_discovered.and_then(|disc| {
            let (rows, cols) = self.content_area_size();
            match Session::adopt(
                name.clone(),
                rows,
                cols,
                &disc.backend_id,
                &backend,
                &self.provider,
                env.clone(),
            ) {
                Ok(session) => Some(session),
                Err(e) => {
                    error!("Failed to adopt session '{name}': {e}");
                    None
                }
            }
        });

        if let Some(mut session) = adopted {
            session.info.id = session_id;
            session.info.agent_session_id = Some(agent_session_id.clone());
            session.info.cwd = shared.cwd.clone();
            session.info.additional_dirs = shared.additional_dirs.clone();
            session.info.role = role;
            session.info.worktrees = worktrees.clone();

            // Re-adopt shell pane if one was persisted
            if let Some(shell_bid) = &shared.shell_backend_id {
                if discovered
                    .iter()
                    .any(|d| d.backend_id == *shell_bid && d.is_alive)
                {
                    let (rows, cols) = self.content_area_size();
                    if let Err(e) = session.adopt_shell_pane(shell_bid, rows, cols) {
                        tracing::warn!("Failed to re-adopt shell pane: {e}");
                    }
                }
            }

            self.sessions.push(session);
            self.active_index = self.sessions.len() - 1;
            self.focus = InputFocus::Terminal;

            if let Some(project) = self.projects.get_mut(target_project_index) {
                if !project.session_ids.contains(&session_id) {
                    project.session_ids.push(session_id);
                }
            }
        } else {
            // No matching backend session or adopt failed — spawn new with --resume.
            if let Err(e) = self.db.soft_delete_session(session_id) {
                error!("Failed to soft-delete stale session {session_id}: {e}");
            }

            let permissions =
                self.resolve_role_permissions_for_project(&role, target_project_index);

            let config = SessionConfig {
                resume_session_id: Some(agent_session_id.clone()),
                agent_session_id: Some(agent_session_id),
                cwd: shared.cwd,
                additional_dirs: shared.additional_dirs,
                role,
                permissions,
                vm_id: None,
                container_id: None,
            };
            self.do_spawn_session(name, &config, worktrees, Some(target_project_index));
        }
    }

    /// Poll background session restore threads for completion.
    ///
    /// Called from `tick()`. Drains step updates and checks for completion.
    /// When a restore completes, the placeholder session is replaced with a
    /// real session (adopted or respawned with `--resume`).
    fn poll_session_restores(&mut self) {
        if self.pending_restores.is_empty() {
            return;
        }

        // Drain step updates — update placeholder provisioning_step.
        for pending in &self.pending_restores {
            let mut latest_step = None;
            while let Ok(step) = pending.step_rx.try_recv() {
                latest_step = Some(step);
            }
            if let Some(step) = latest_step {
                // Find the placeholder session and update its provisioning step.
                if let Some(session) = self
                    .sessions
                    .iter_mut()
                    .find(|s| s.info.id == pending.session_id)
                {
                    session.info.provisioning_step = Some(step);
                }
            }
        }

        // Check for completed restores (drain finished entries).
        let mut completed_indices = Vec::new();
        for (i, pending) in self.pending_restores.iter().enumerate() {
            match pending.rx.try_recv() {
                Ok(result) => {
                    completed_indices.push((i, result));
                }
                Err(mpsc::TryRecvError::Empty) => {} // Still in progress
                Err(mpsc::TryRecvError::Disconnected) => {
                    completed_indices
                        .push((i, Err("Restore thread terminated unexpectedly".to_string())));
                }
            }
        }

        // Process completed restores in reverse order so indices stay valid.
        for (i, result) in completed_indices.into_iter().rev() {
            let pending = self.pending_restores.remove(i);
            match result {
                Ok(restore_result) => {
                    self.handle_restore_complete(pending, restore_result);
                }
                Err(error) => {
                    self.handle_restore_failed(pending, &error);
                }
            }
        }
    }

    /// Handle successful background restore: adopt or respawn the session.
    fn handle_restore_complete(&mut self, pending: PendingRestore, result: RestoreResult) {
        let shared = pending.shared;
        let session_id = shared.id;
        let name = shared.name.clone();

        let role = if shared.role.is_empty() {
            DEFAULT_ROLE_NAME.to_string()
        } else {
            shared.role.clone()
        };

        let worktrees: Vec<WorktreeInfo> =
            shared.worktrees.iter().cloned().map(Into::into).collect();

        let agent_session_id = match shared.agent_session_id {
            Some(ref id) => id.clone(),
            None => return,
        };

        // Remove the placeholder session.
        self.sessions.retain(|s| s.info.id != session_id);

        let matching_discovered = Self::find_matching_discovered(&shared, &result.discovered);

        let backend = self
            .backends
            .get(&shared.backend_type)
            .cloned()
            .unwrap_or_else(|| self.backends.default_backend().clone());

        // Try to adopt the existing backend session.
        let env = self.resolve_role_permissions(&role).env;
        let adopted = matching_discovered.and_then(|disc| {
            let (rows, cols) = self.content_area_size();
            match Session::adopt(
                name.clone(),
                rows,
                cols,
                &disc.backend_id,
                &backend,
                &self.provider,
                env.clone(),
            ) {
                Ok(session) => Some(session),
                Err(e) => {
                    error!("Failed to adopt session '{name}': {e}");
                    None
                }
            }
        });

        if let Some(mut session) = adopted {
            session.info.id = session_id;
            session.info.agent_session_id = Some(agent_session_id.clone());
            session.info.cwd = shared.cwd.clone();
            session.info.additional_dirs = shared.additional_dirs.clone();
            session.info.role = role;
            session.info.worktrees = worktrees;

            // Restore vm_id on adopted VM sessions.
            if shared.backend_type == "qemu-vm" {
                if let Ok(Some(vm_record)) = self.db.get_vm_by_session(&session_id.to_string()) {
                    session.info.vm_id = Some(vm_record.id);
                }
            }

            // Restore container_id on adopted devcontainer sessions.
            if shared.backend_type == "devcontainer" {
                if let Ok(Some(container_record)) =
                    self.db.get_container_by_session(&session_id.to_string())
                {
                    session.info.container_id = Some(container_record.id);
                }
            }

            // Re-adopt shell pane if one was persisted.
            if let Some(shell_bid) = &shared.shell_backend_id {
                if result
                    .discovered
                    .iter()
                    .any(|d| d.backend_id == *shell_bid && d.is_alive)
                {
                    let (rows, cols) = self.content_area_size();
                    if let Err(e) = session.adopt_shell_pane(shell_bid, rows, cols) {
                        tracing::warn!("Failed to re-adopt shell pane: {e}");
                    }
                }
            }

            self.sessions.push(session);
            info!(session = %session_id, name = %name, "Session restored (adopted)");
        } else {
            // No matching backend session or adopt failed — spawn new with --resume.
            if let Err(e) = self.db.soft_delete_session(session_id) {
                error!("Failed to soft-delete stale session {session_id}: {e}");
            }

            let permissions =
                self.resolve_role_permissions_for_project(&role, pending.project_index);

            // Check if the backend resource is alive for respawn routing.
            let mut resolved_vm_id = None;
            let mut resolved_container_id = None;

            if shared.backend_type == "qemu-vm" {
                if let Ok(Some(vm_record)) = self.db.get_vm_by_session(&session_id.to_string()) {
                    let vm_alive = self.vm_manager.as_ref().is_some_and(|mgr| {
                        mgr.lock()
                            .ok()
                            .and_then(|m| m.vm_state(&vm_record.id))
                            .is_some_and(|s| s == crate::session::VmState::Ready)
                    });
                    if vm_alive {
                        resolved_vm_id = Some(vm_record.id);
                    } else {
                        warn!(
                            session = %session_id,
                            vm_id = %vm_record.id,
                            "VM died — session will be re-spawned with --resume on local-tmux"
                        );
                        let _ = self.db.update_vm_state(
                            &vm_record.id,
                            &crate::session::VmState::Stopped,
                            None,
                            Some("VM not reachable after restart"),
                        );
                    }
                }
            } else if shared.backend_type == "devcontainer" {
                if let Ok(Some(container_record)) =
                    self.db.get_container_by_session(&session_id.to_string())
                {
                    let container_alive = self.container_manager.as_ref().is_some_and(|mgr| {
                        mgr.lock()
                            .ok()
                            .and_then(|m| m.get_instance(&container_record.id))
                            .is_some_and(|inst| inst.state == crate::session::ContainerState::Ready)
                    });
                    if container_alive {
                        info!(
                            session = %session_id,
                            container_id = %container_record.id,
                            "Container alive — re-spawning session inside container"
                        );
                        resolved_container_id = Some(container_record.id.clone());
                        self.pending_container_id = Some(container_record.id);
                    } else {
                        warn!(
                            session = %session_id,
                            container_id = %container_record.id,
                            "Container not alive — session will be re-spawned on local-tmux"
                        );
                        let _ = self.db.update_container_state(
                            &container_record.id,
                            &crate::session::ContainerState::Stopped,
                            None,
                            Some("Container not reachable after restart"),
                        );
                    }
                }
            }

            let config = SessionConfig {
                resume_session_id: Some(agent_session_id.clone()),
                agent_session_id: Some(agent_session_id),
                cwd: shared.cwd,
                additional_dirs: shared.additional_dirs,
                role,
                permissions,
                vm_id: resolved_vm_id,
                container_id: resolved_container_id,
            };
            self.do_spawn_session(name, &config, worktrees, Some(pending.project_index));
            info!(session = %session_id, "Session restored (respawned with --resume)");
        }

        // Ensure the project still has the session registered.
        if let Some(project) = self.projects.get_mut(pending.project_index) {
            if !project.session_ids.contains(&session_id) {
                project.session_ids.push(session_id);
            }
        }

        self.save_state();
    }

    /// Handle a failed background restore: remove placeholder, fall back to local-tmux.
    fn handle_restore_failed(&mut self, pending: PendingRestore, error: &str) {
        let session_id = pending.session_id;
        warn!(session = %session_id, "Background restore failed: {error}");

        // Try to respawn on local-tmux as a fallback.
        let shared = pending.shared;
        let name = shared.name.clone();
        let role = if shared.role.is_empty() {
            DEFAULT_ROLE_NAME.to_string()
        } else {
            shared.role.clone()
        };
        let worktrees: Vec<WorktreeInfo> =
            shared.worktrees.iter().cloned().map(Into::into).collect();

        // Remove the placeholder session.
        self.sessions.retain(|s| s.info.id != session_id);

        if let Some(ref agent_session_id) = shared.agent_session_id {
            // Soft-delete the stale entry and respawn on local-tmux with --resume.
            if let Err(e) = self.db.soft_delete_session(session_id) {
                error!("Failed to soft-delete stale session {session_id}: {e}");
            }

            // Mark the container/VM as stopped in the DB.
            if shared.backend_type == "devcontainer" {
                if let Ok(Some(rec)) = self.db.get_container_by_session(&session_id.to_string()) {
                    let _ = self.db.update_container_state(
                        &rec.id,
                        &crate::session::ContainerState::Stopped,
                        None,
                        Some(error),
                    );
                }
            } else if shared.backend_type == "qemu-vm" {
                if let Ok(Some(rec)) = self.db.get_vm_by_session(&session_id.to_string()) {
                    let _ = self.db.update_vm_state(
                        &rec.id,
                        &crate::session::VmState::Stopped,
                        None,
                        Some(error),
                    );
                }
            }

            let permissions =
                self.resolve_role_permissions_for_project(&role, pending.project_index);

            let config = SessionConfig {
                resume_session_id: Some(agent_session_id.clone()),
                agent_session_id: Some(agent_session_id.clone()),
                cwd: shared.cwd,
                additional_dirs: shared.additional_dirs,
                role,
                permissions,
                vm_id: None,
                container_id: None,
            };

            warn!(
                session = %session_id,
                "Falling back to local-tmux for session after restore failure"
            );
            self.do_spawn_session(name, &config, worktrees, Some(pending.project_index));
        }

        self.save_state();
    }

    /// Find a discovered backend session matching a shared session.
    ///
    /// Tries to match by `backend_id` first, falls back to window name (`tb-<name>`).
    fn find_matching_discovered<'a>(
        shared: &sync::SharedSession,
        discovered: &'a [crate::agent::backend::DiscoveredSession],
    ) -> Option<&'a crate::agent::backend::DiscoveredSession> {
        if !shared.backend_id.is_empty() {
            discovered
                .iter()
                .find(|d| d.backend_id == shared.backend_id && d.is_alive)
        } else {
            let expected_name = format!("tb-{}", shared.name);
            discovered
                .iter()
                .find(|d| d.name == expected_name && d.is_alive)
        }
    }

    /// Find the project index that owns a session, falling back to `active_project_index`.
    fn find_project_index_for_session(
        &self,
        session_id: SessionId,
        project_id: &ProjectId,
    ) -> usize {
        let proj_uuid = project_id.as_uuid();
        self.projects
            .iter()
            .position(|p| p.id.as_uuid() == proj_uuid)
            .unwrap_or_else(|| {
                tracing::warn!(
                    session = %session_id,
                    project_uuid = %proj_uuid,
                    fallback_index = self.active_project_index,
                    "Session project not found, falling back to active project"
                );
                self.active_project_index
            })
    }

    /// Resolve a role name to its permissions for a specific project.
    /// Admin projects always get the hardcoded MCP tool permissions.
    fn resolve_role_permissions_for_project(
        &self,
        role_name: &str,
        project_index: usize,
    ) -> RolePermissions {
        let project = self.projects.get(project_index);
        if project.is_some_and(|p| p.is_admin) {
            return admin_mcp_permissions();
        }
        project
            .and_then(|project| {
                project
                    .config
                    .roles
                    .iter()
                    .find(|r| r.name == role_name)
                    .map(|r| r.permissions.clone())
            })
            .unwrap_or_else(|| {
                if role_name == DEFAULT_ROLE_NAME {
                    default_developer_permissions()
                } else {
                    RolePermissions::default()
                }
            })
    }

    /// Resolve a role name to its permissions using the active project's role config.
    fn resolve_role_permissions(&self, role_name: &str) -> RolePermissions {
        self.resolve_role_permissions_for_project(role_name, self.active_project_index)
    }

    /// Process pending session commands from the MCP command queue.
    fn process_session_commands(&mut self) {
        let commands = match self.db.pending_session_commands() {
            Ok(cmds) => cmds,
            Err(e) => {
                error!("Failed to fetch session commands: {e}");
                return;
            }
        };

        for cmd in commands {
            match cmd.command.as_str() {
                "restart" => self.handle_restart_command(&cmd),
                other => error!("Unknown session command: {other}"),
            }

            if let Err(e) = self.db.mark_command_processed(cmd.id) {
                error!("Failed to mark command {} as processed: {e}", cmd.id);
            }
        }
    }

    /// Handle a restart command from the session command queue.
    fn handle_restart_command(&mut self, cmd: &SessionCommand) {
        let Some(session_idx) = self
            .sessions
            .iter()
            .position(|s| s.info.id == cmd.session_id)
        else {
            error!("Restart command for unknown session: {}", cmd.session_id);
            return;
        };

        let session = &self.sessions[session_idx];
        let Some(agent_session_id) = session.info.agent_session_id.clone() else {
            error!(
                "Cannot restart session {} without agent_session_id",
                cmd.session_id
            );
            return;
        };

        let role = session.info.role.clone();
        let cwd = session.info.cwd.clone();
        let additional_dirs = session.info.additional_dirs.clone();

        // Find the project that owns this session (may not be the active project)
        let project_index = self
            .projects
            .iter()
            .position(|p| p.session_ids.contains(&cmd.session_id))
            .unwrap_or(self.active_project_index);
        let permissions = self.resolve_role_permissions_for_project(&role, project_index);

        let config = SessionConfig {
            resume_session_id: Some(agent_session_id.clone()),
            agent_session_id: Some(agent_session_id),
            cwd,
            additional_dirs,
            role,
            permissions,
            vm_id: session.info.vm_id.clone(),
            container_id: session.info.container_id.clone(),
        };

        let (rows, cols) = self.content_area_size();
        let session = &mut self.sessions[session_idx];
        match session.restart(&config, rows, cols) {
            Ok(()) => {
                self.save_state();
            }
            Err(e) => {
                error!(
                    "Failed to restart session {} via command: {e}",
                    cmd.session_id
                );
            }
        }
    }

    /// Process due scheduled commands from the database (fallback).
    ///
    /// The primary dispatch is via `tmux run-shell -b -d` timers set at
    /// scheduling time. This tick-loop catches commands whose tmux timer
    /// failed or was never set (e.g., scheduled while Thurbox was down).
    /// Throttled to once per second (~100 ticks at 10ms each).
    fn process_scheduled_commands(&mut self) {
        if self.tick_count % 100 != 0 {
            return;
        }

        let commands = match self.db.due_scheduled_commands() {
            Ok(cmds) => cmds,
            Err(e) => {
                error!("Failed to fetch due scheduled commands: {e}");
                return;
            }
        };

        for cmd in commands {
            // Skip if we don't have this session — the tmux timer can handle it independently.
            let Some(session) = self.sessions.iter().find(|s| s.info.id == cmd.session_id) else {
                continue;
            };

            let mut paste = b"\x1b[200~".to_vec();
            paste.extend_from_slice(cmd.command_text.as_bytes());
            paste.extend_from_slice(b"\x1b[201~");
            if let Err(e) = session.send_input(paste) {
                error!(
                    "Failed to send scheduled command {} to session {}: {e}",
                    cmd.id, cmd.session_id
                );
            } else {
                self.deferred_inputs.push((
                    cmd.session_id,
                    b"\r".to_vec(),
                    self.tick_count + DEFERRED_INPUT_DELAY_TICKS,
                ));
                info!(
                    "Executed scheduled command {} for session {}",
                    cmd.id, cmd.session_id
                );
            }

            // Mark as executed to prevent the tmux timer from duplicating
            if let Err(e) = self.db.mark_scheduled_command_executed(cmd.id) {
                error!(
                    "Failed to mark scheduled command {} as executed: {e}",
                    cmd.id
                );
            }
        }
    }

    /// Open the schedule-command modal for the active session.
    fn open_schedule_command_modal(&mut self) {
        if self.sessions.is_empty() {
            self.set_error("No active session to schedule a command for");
            return;
        }
        self.modal = modals::Modal::ScheduleCommand(modals::ScheduleCommandModal::default());
    }

    /// Validate and submit the schedule-command modal.
    fn submit_schedule_command(&mut self) {
        let modals::Modal::ScheduleCommand(ref sc) = self.modal else {
            return;
        };
        let command_text = sc.command.value().trim().to_string();
        if command_text.is_empty() {
            self.set_error("Command cannot be empty");
            return;
        }
        let delay_minutes: u64 = match sc.delay_minutes.value().trim().parse() {
            Ok(v) if v > 0 => v,
            _ => {
                self.set_error("Delay must be a positive number of minutes");
                return;
            }
        };

        let session = match self.sessions.get(self.active_index) {
            Some(s) => s,
            None => {
                self.set_error("No active session");
                return;
            }
        };
        let session_id = session.info.id;
        let session_name = session.info.name.clone();

        let delay_seconds = delay_minutes * 60;
        let now = crate::sync::current_time_millis();
        let scheduled_at = now + delay_seconds * 1000;

        match self
            .db
            .create_scheduled_command(session_id, &command_text, scheduled_at)
        {
            Ok(id) => {
                // Set up a tmux timer for external dispatch
                if let Some(db_path) = crate::paths::database_file() {
                    if let Err(e) = crate::agent::tmux::schedule_tmux_command(
                        &session_name,
                        &command_text,
                        delay_seconds,
                        id,
                        &db_path,
                    ) {
                        warn!("Failed to set tmux timer for command {id}: {e}");
                    }
                }

                self.modal.close();
                self.set_status(
                    StatusLevel::Success,
                    format!("Command scheduled for {} in {delay_minutes}m", session_name),
                );
            }
            Err(e) => {
                error!("Failed to create scheduled command: {e}");
                self.set_error("Failed to schedule command");
            }
        }
    }

    pub(crate) fn content_area_size(&self) -> (u16, u16) {
        let area = Rect::new(0, 0, self.terminal_cols, self.terminal_rows);
        let terminal = layout::compute_layout(area, self.show_info_panel).terminal;
        let inner = Block::default().borders(Borders::ALL).inner(terminal);
        (inner.height, inner.width)
    }
}

// Test-only helper accessors for modal state.
//
// These provide ergonomic access to fields that moved into the Modal enum,
// so tests don't need to destructure the modal everywhere.
#[cfg(test)]
impl App {
    fn is_edit_project_open(&self) -> bool {
        matches!(self.modal, modals::Modal::EditProject(_))
    }

    fn is_add_project_open(&self) -> bool {
        matches!(self.modal, modals::Modal::AddProject(_))
    }

    fn edit_project(&self) -> Option<&modals::EditProjectModal> {
        if let modals::Modal::EditProject(ref ep) = self.modal {
            Some(ep)
        } else {
            None
        }
    }

    fn edit_project_mut(&mut self) -> Option<&mut modals::EditProjectModal> {
        if let modals::Modal::EditProject(ref mut ep) = self.modal {
            Some(ep)
        } else {
            None
        }
    }

    /// Set up an empty EditProject modal with the role editor open.
    /// Used by tests that need the role editor without requiring an active project.
    fn open_empty_role_editor(&mut self) {
        self.modal = modals::Modal::EditProject(Box::default());
        self.role_editor_view = RoleEditorView::List;
        self.show_role_editor = true;
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::sync::Arc;

    use super::*;
    use crate::agent::SessionBackend;

    // --- TextInput tests ---

    #[test]
    fn text_input_new_is_empty() {
        let input = TextInput::new();
        assert_eq!(input.value(), "");
        assert_eq!(input.cursor_pos(), 0);
    }

    #[test]
    fn text_input_insert_appends_chars() {
        let mut input = TextInput::new();
        input.insert('a');
        input.insert('b');
        input.insert('c');
        assert_eq!(input.value(), "abc");
        assert_eq!(input.cursor_pos(), 3);
    }

    #[test]
    fn text_input_insert_at_middle() {
        let mut input = TextInput::new();
        input.insert('a');
        input.insert('c');
        input.move_left();
        input.insert('b');
        assert_eq!(input.value(), "abc");
        assert_eq!(input.cursor_pos(), 2);
    }

    #[test]
    fn text_input_backspace_removes_before_cursor() {
        let mut input = TextInput::new();
        input.insert('a');
        input.insert('b');
        input.insert('c');
        input.backspace();
        assert_eq!(input.value(), "ab");
        assert_eq!(input.cursor_pos(), 2);
    }

    #[test]
    fn text_input_backspace_at_start_is_noop() {
        let mut input = TextInput::new();
        input.insert('a');
        input.home();
        input.backspace();
        assert_eq!(input.value(), "a");
        assert_eq!(input.cursor_pos(), 0);
    }

    #[test]
    fn text_input_delete_removes_at_cursor() {
        let mut input = TextInput::new();
        input.insert('a');
        input.insert('b');
        input.insert('c');
        input.home();
        input.delete();
        assert_eq!(input.value(), "bc");
        assert_eq!(input.cursor_pos(), 0);
    }

    #[test]
    fn text_input_delete_at_end_is_noop() {
        let mut input = TextInput::new();
        input.insert('a');
        input.delete();
        assert_eq!(input.value(), "a");
        assert_eq!(input.cursor_pos(), 1);
    }

    #[test]
    fn text_input_move_left_and_right() {
        let mut input = TextInput::new();
        input.insert('a');
        input.insert('b');
        assert_eq!(input.cursor_pos(), 2);

        input.move_left();
        assert_eq!(input.cursor_pos(), 1);

        input.move_right();
        assert_eq!(input.cursor_pos(), 2);
    }

    #[test]
    fn text_input_move_left_at_zero_is_noop() {
        let mut input = TextInput::new();
        input.move_left();
        assert_eq!(input.cursor_pos(), 0);
    }

    #[test]
    fn text_input_move_right_at_end_is_noop() {
        let mut input = TextInput::new();
        input.insert('a');
        input.move_right();
        assert_eq!(input.cursor_pos(), 1);
    }

    #[test]
    fn text_input_home_and_end() {
        let mut input = TextInput::new();
        input.insert('a');
        input.insert('b');
        input.insert('c');

        input.home();
        assert_eq!(input.cursor_pos(), 0);

        input.end();
        assert_eq!(input.cursor_pos(), 3);
    }

    #[test]
    fn text_input_clear_resets_buffer_and_cursor() {
        let mut input = TextInput::new();
        input.insert('a');
        input.insert('b');
        input.clear();
        assert_eq!(input.value(), "");
        assert_eq!(input.cursor_pos(), 0);
    }

    #[test]
    fn text_input_multibyte_chars() {
        let mut input = TextInput::new();
        input.insert('é');
        input.insert('ñ');
        assert_eq!(input.value(), "éñ");
        assert_eq!(input.cursor_pos(), 2);

        input.move_left();
        input.insert('ü');
        assert_eq!(input.value(), "éüñ");
        assert_eq!(input.cursor_pos(), 2);
    }

    #[test]
    fn text_input_backspace_multibyte() {
        let mut input = TextInput::new();
        input.insert('a');
        input.insert('é');
        input.insert('b');
        input.backspace();
        assert_eq!(input.value(), "aé");

        input.backspace();
        assert_eq!(input.value(), "a");
    }

    #[test]
    fn text_input_delete_multibyte() {
        let mut input = TextInput::new();
        input.insert('é');
        input.insert('b');
        input.home();
        input.delete();
        assert_eq!(input.value(), "b");
        assert_eq!(input.cursor_pos(), 0);
    }

    // --- Session switching tests ---

    /// Stub backend that does nothing — for unit tests only.
    struct StubBackend;
    impl SessionBackend for StubBackend {
        fn name(&self) -> &str {
            "stub"
        }
        fn check_available(&self) -> anyhow::Result<()> {
            Ok(())
        }
        fn ensure_ready(&self) -> anyhow::Result<()> {
            Ok(())
        }
        fn spawn(
            &self,
            _: &str,
            _: &str,
            _: &[String],
            _: Option<&Path>,
            _: &std::collections::HashMap<String, String>,
            _: u16,
            _: u16,
        ) -> anyhow::Result<crate::agent::backend::SpawnedSession> {
            anyhow::bail!("stub backend does not spawn")
        }
        fn adopt(
            &self,
            _: &str,
            _: u16,
            _: u16,
        ) -> anyhow::Result<crate::agent::backend::AdoptedSession> {
            anyhow::bail!("stub backend does not adopt")
        }
        fn discover(&self) -> anyhow::Result<Vec<crate::agent::backend::DiscoveredSession>> {
            Ok(vec![])
        }
        fn resize(&self, _: &str, _: u16, _: u16) -> anyhow::Result<()> {
            Ok(())
        }
        fn is_dead(&self, _: &str) -> anyhow::Result<bool> {
            Ok(false)
        }
        fn kill(&self, _: &str) -> anyhow::Result<()> {
            Ok(())
        }
        fn detach(&self, _: &str) -> anyhow::Result<()> {
            Ok(())
        }
        fn pane_pid(&self, _: &str) -> anyhow::Result<Option<u32>> {
            Ok(None)
        }
    }

    fn stub_backend_arc() -> Arc<dyn SessionBackend> {
        Arc::new(StubBackend)
    }

    fn stub_provider() -> Arc<dyn crate::agent::AgentProvider> {
        Arc::new(crate::agent::claude::ClaudeProvider)
    }

    fn stub_backend() -> BackendRegistry {
        BackendRegistry::new(stub_backend_arc())
    }

    fn test_db() -> Database {
        Database::open_in_memory().unwrap()
    }

    /// Create a basic test project config.
    fn test_project_config() -> ProjectConfig {
        ProjectConfig {
            name: "Test".to_string(),
            repos: vec![PathBuf::from("/test")],
            roles: Vec::new(),
            mcp_servers: Vec::new(),
            id: None,
        }
    }

    /// Create a test DB with a project pre-inserted.
    fn test_db_with_project(config: &ProjectConfig) -> Database {
        let db = test_db();
        let id = config.effective_id();
        db.insert_project(id, &config.name, &config.repos).unwrap();
        if !config.roles.is_empty() {
            db.replace_roles(id, &config.roles).unwrap();
        }
        db
    }

    /// Create an App with a test project and N stub sessions bound to it.
    fn app_with_sessions(count: usize) -> App {
        let backend_arc = stub_backend_arc();
        let provider = stub_provider();
        let mut app = App::new(
            24,
            120,
            BackendRegistry::new(backend_arc.clone()),
            provider.clone(),
            test_db_with_project(&test_project_config()),
            None,
            None,
        );
        for _i in 0..count {
            let session = Session::stub("test-session", &backend_arc, &provider);
            let session_id = session.info.id;
            app.sessions.push(session);
            app.projects[0].session_ids.push(session_id);
        }
        if !app.sessions.is_empty() {
            app.active_index = 0;
        }
        app
    }

    #[test]
    fn switch_forward_advances_to_next_session() {
        let mut app = app_with_sessions(3);
        app.active_index = 0;
        app.switch_session_forward();
        assert_eq!(app.active_index, 1);
    }

    #[test]
    fn switch_forward_at_last_session_is_noop() {
        let mut app = app_with_sessions(3);
        app.active_index = 2;
        app.switch_session_forward();
        assert_eq!(app.active_index, 2);
    }

    #[test]
    fn switch_backward_moves_to_previous_session() {
        let mut app = app_with_sessions(3);
        app.active_index = 2;
        app.switch_session_backward();
        assert_eq!(app.active_index, 1);
    }

    #[test]
    fn switch_backward_at_first_session_is_noop() {
        let mut app = app_with_sessions(3);
        app.active_index = 0;
        app.switch_session_backward();
        assert_eq!(app.active_index, 0);
    }

    #[test]
    fn switch_with_no_sessions_is_noop() {
        let mut app = app_with_sessions(0);
        app.switch_session_forward();
        assert_eq!(app.active_index, 0);
        app.switch_session_backward();
        assert_eq!(app.active_index, 0);
    }

    #[test]
    fn switch_with_single_session_is_noop() {
        let mut app = app_with_sessions(1);
        app.active_index = 0;
        app.switch_session_forward();
        assert_eq!(app.active_index, 0);
        app.switch_session_backward();
        assert_eq!(app.active_index, 0);
    }

    // --- Scroll tests ---

    fn parser_with_scrollback() -> vt100::Parser {
        let mut parser = vt100::Parser::new(24, 80, 100);
        // Fill screen and scrollback by writing many lines
        for i in 0..50 {
            parser.process(format!("line {i}\r\n").as_bytes());
        }
        parser
    }

    #[test]
    fn scrollback_starts_at_zero() {
        let parser = parser_with_scrollback();
        assert_eq!(parser.screen().scrollback(), 0);
    }

    #[test]
    fn scrollback_increments() {
        let mut parser = parser_with_scrollback();
        parser.screen_mut().set_scrollback(5);
        assert_eq!(parser.screen().scrollback(), 5);
    }

    #[test]
    fn scrollback_clamps_to_max() {
        let mut parser = parser_with_scrollback();
        parser.screen_mut().set_scrollback(usize::MAX);
        let max = parser.screen().scrollback();
        // Should be clamped to the actual scrollback content, not usize::MAX
        assert!(max < usize::MAX);
        assert!(max > 0);
    }

    #[test]
    fn scrollback_restores_after_probe() {
        let mut parser = parser_with_scrollback();
        parser.screen_mut().set_scrollback(3);

        // Probe total scrollback (same technique as render_terminal)
        let saved = parser.screen().scrollback();
        parser.screen_mut().set_scrollback(usize::MAX);
        let _total = parser.screen().scrollback();
        parser.screen_mut().set_scrollback(saved);

        assert_eq!(parser.screen().scrollback(), 3);
    }

    #[test]
    fn scrollback_zero_stays_at_bottom() {
        let mut parser = parser_with_scrollback();
        assert_eq!(parser.screen().scrollback(), 0);

        // New output while at bottom keeps offset at 0
        parser.process(b"new line\r\n");
        assert_eq!(parser.screen().scrollback(), 0);
    }

    #[test]
    fn page_scroll_amount_is_half_content_height() {
        let app = App::new(
            50,
            100,
            stub_backend(),
            stub_provider(),
            test_db(),
            None,
            None,
        );
        // rows = 50 - 4 = 46, half = 23
        assert_eq!(app.page_scroll_amount(), 23);
    }

    #[test]
    fn page_scroll_amount_small_terminal() {
        let app = App::new(
            6,
            80,
            stub_backend(),
            stub_provider(),
            test_db(),
            None,
            None,
        );
        // rows = 6 - 4 = 2, half = 1
        assert_eq!(app.page_scroll_amount(), 1);
    }

    #[test]
    fn mouse_scroll_lines_constant() {
        assert_eq!(MOUSE_SCROLL_LINES, 3);
    }

    // --- Session naming tests ---

    #[test]
    fn next_session_name_starts_at_one() {
        let mut app = App::new(
            24,
            80,
            stub_backend(),
            stub_provider(),
            test_db(),
            None,
            None,
        );
        assert_eq!(app.next_session_name(), "1");
    }

    #[test]
    fn next_session_name_increments() {
        let mut app = App::new(
            24,
            80,
            stub_backend(),
            stub_provider(),
            test_db(),
            None,
            None,
        );
        assert_eq!(app.next_session_name(), "1");
        assert_eq!(app.next_session_name(), "2");
        assert_eq!(app.next_session_name(), "3");
    }

    #[test]
    fn next_session_name_continues_from_restored_counter() {
        let mut app = App::new(
            24,
            80,
            stub_backend(),
            stub_provider(),
            test_db(),
            None,
            None,
        );
        app.session_counter = 5;
        assert_eq!(app.next_session_name(), "6");
    }

    // --- Role editor tests ---

    #[test]
    fn open_role_editor_has_seeded_developer_role() {
        let mut app = App::new(
            24,
            120,
            stub_backend(),
            stub_provider(),
            test_db_with_project(&test_project_config()),
            None,
            None,
        );
        app.open_role_editor();
        assert!(app.show_role_editor);
        let ep = app.edit_project().unwrap();
        assert_eq!(ep.role_editor_roles.len(), 1);
        assert_eq!(ep.role_editor_roles[0].name, "developer");
        assert_eq!(app.role_editor_view, RoleEditorView::List);
    }

    #[test]
    fn open_role_editor_clones_existing_roles() {
        use crate::session::{RoleConfig, RolePermissions};

        let config = ProjectConfig {
            name: "test".to_string(),
            repos: vec![],
            roles: vec![RoleConfig {
                name: "ops".to_string(),
                description: "Operations".to_string(),
                permissions: RolePermissions::default(),
            }],
            mcp_servers: vec![],
            id: None,
        };
        let mut app = App::new(
            24,
            120,
            stub_backend(),
            stub_provider(),
            test_db_with_project(&config),
            None,
            None,
        );
        app.open_role_editor();
        let ep = app.edit_project().unwrap();
        assert_eq!(ep.role_editor_roles.len(), 1);
        assert_eq!(ep.role_editor_roles[0].name, "ops");
    }

    #[test]
    fn role_editor_submit_uses_allowed_tools_list() {
        let mut app = App::new(
            24,
            120,
            stub_backend(),
            stub_provider(),
            test_db(),
            None,
            None,
        );
        app.open_empty_role_editor();
        app.handle_role_editor_list_key(KeyCode::Char('a'));
        for c in "reviewer".chars() {
            app.role_editor_name.insert(c);
        }
        app.role_editor_allowed_tools.items.push("Read".to_string());
        app.role_editor_allowed_tools
            .items
            .push("Bash(git:*)".to_string());
        app.submit_role_editor();
        let ep = app.edit_project().unwrap();
        assert_eq!(ep.role_editor_roles.len(), 1);
        assert_eq!(
            ep.role_editor_roles[0].permissions.allowed_tools,
            vec!["Read".to_string(), "Bash(git:*)".to_string()]
        );
    }

    #[test]
    fn role_editor_submit_uses_disallowed_tools_list() {
        let mut app = App::new(
            24,
            120,
            stub_backend(),
            stub_provider(),
            test_db(),
            None,
            None,
        );
        app.open_empty_role_editor();
        app.handle_role_editor_list_key(KeyCode::Char('a'));
        for c in "restricted".chars() {
            app.role_editor_name.insert(c);
        }
        app.role_editor_disallowed_tools
            .items
            .push("Edit".to_string());
        app.role_editor_disallowed_tools
            .items
            .push("Write".to_string());
        app.submit_role_editor();
        let ep = app.edit_project().unwrap();
        assert_eq!(ep.role_editor_roles.len(), 1);
        assert_eq!(
            ep.role_editor_roles[0].permissions.disallowed_tools,
            vec!["Edit".to_string(), "Write".to_string()]
        );
    }

    #[test]
    fn spawn_with_two_roles_shows_selector() {
        use crate::session::{RoleConfig, RolePermissions};

        let config = ProjectConfig {
            name: "test".to_string(),
            repos: vec![],
            roles: vec![
                RoleConfig {
                    name: "dev".to_string(),
                    description: "Developer".to_string(),
                    permissions: RolePermissions::default(),
                },
                RoleConfig {
                    name: "reviewer".to_string(),
                    description: "Read-only".to_string(),
                    permissions: RolePermissions {
                        permission_mode: Some("plan".to_string()),
                        ..RolePermissions::default()
                    },
                },
            ],
            mcp_servers: vec![],
            id: None,
        };
        let mut app = App::new(
            24,
            120,
            stub_backend(),
            stub_provider(),
            test_db_with_project(&config),
            None,
            None,
        );
        let session_config = SessionConfig::default();
        app.prepare_spawn(session_config, Vec::new());
        assert!(matches!(app.modal, modals::Modal::RoleSelector(_)));
    }

    #[test]
    fn spawn_with_no_roles_has_no_pending_selector() {
        let config = ProjectConfig {
            name: "test".to_string(),
            repos: vec![],
            roles: vec![],
            mcp_servers: vec![],
            id: None,
        };
        let app = App::new(
            24,
            120,
            stub_backend(),
            stub_provider(),
            test_db_with_project(&config),
            None,
            None,
        );
        // With no roles, the selector should never be set
        assert!(!matches!(app.modal, modals::Modal::RoleSelector(_)));
    }

    #[test]
    fn role_editor_name_validation_rejects_empty() {
        let mut app = App::new(
            24,
            120,
            stub_backend(),
            stub_provider(),
            test_db(),
            None,
            None,
        );
        app.open_empty_role_editor();
        app.handle_role_editor_list_key(KeyCode::Char('a'));
        // Try to submit with empty name
        app.submit_role_editor();
        assert!(app.status_message.is_some());
        // Should still be in editor view
        assert_eq!(app.role_editor_view, RoleEditorView::Editor);
    }

    #[test]
    fn text_input_set_replaces_content_and_moves_cursor_to_end() {
        let mut input = TextInput::new();
        input.insert('x');
        input.set("hello");
        assert_eq!(input.value(), "hello");
        assert_eq!(input.cursor_pos(), 5);
    }

    #[test]
    fn text_input_set_empty_clears() {
        let mut input = TextInput::new();
        input.insert('a');
        input.insert('b');
        input.set("");
        assert_eq!(input.value(), "");
        assert_eq!(input.cursor_pos(), 0);
    }

    #[test]
    fn role_editor_name_validation_rejects_duplicate() {
        let mut app = App::new(
            24,
            120,
            stub_backend(),
            stub_provider(),
            test_db(),
            None,
            None,
        );
        app.open_empty_role_editor();
        // Add first role
        app.handle_role_editor_list_key(KeyCode::Char('a'));
        app.role_editor_name.set("dev");
        app.submit_role_editor();
        assert_eq!(app.edit_project().unwrap().role_editor_roles.len(), 1);

        // Try to add a second role with the same name
        app.handle_role_editor_list_key(KeyCode::Char('a'));
        app.role_editor_name.set("dev");
        app.submit_role_editor();
        assert!(app.status_message.is_some());
        assert!(app
            .status_message
            .as_ref()
            .unwrap()
            .text
            .contains("already exists"));
        // Should still be in editor view, role count unchanged
        assert_eq!(app.role_editor_view, RoleEditorView::Editor);
        assert_eq!(app.edit_project().unwrap().role_editor_roles.len(), 1);
    }

    #[test]
    fn role_editor_edit_preserves_permission_mode_and_tools() {
        use crate::session::{RoleConfig, RolePermissions};

        let config = ProjectConfig {
            name: "test".to_string(),
            repos: vec![],
            roles: vec![RoleConfig {
                name: "custom".to_string(),
                description: "Custom role".to_string(),
                permissions: RolePermissions {
                    permission_mode: Some("plan".to_string()),
                    allowed_tools: vec!["Read".to_string()],
                    disallowed_tools: vec![],
                    tools: Some("default".to_string()),
                    append_system_prompt: Some("Be careful".to_string()),
                    env: HashMap::new(),
                },
            }],
            mcp_servers: vec![],
            id: None,
        };
        let mut app = App::new(
            24,
            120,
            stub_backend(),
            stub_provider(),
            test_db_with_project(&config),
            None,
            None,
        );
        app.open_role_editor();
        app.open_role_for_editing(0);

        // Modify the name and submit
        app.role_editor_name.set("custom-v2");
        app.submit_role_editor();

        let role = &app.edit_project().unwrap().role_editor_roles[0];
        assert_eq!(role.name, "custom-v2");
        // permission_mode and tools are not exposed in the editor
        assert_eq!(role.permissions.permission_mode, Some("plan".to_string()));
        assert_eq!(role.permissions.tools, Some("default".to_string()));
        // system prompt is loaded and re-saved unchanged
        assert_eq!(
            role.permissions.append_system_prompt,
            Some("Be careful".to_string())
        );
    }

    #[test]
    fn role_editor_new_role_has_no_extra_fields() {
        let mut app = App::new(
            24,
            120,
            stub_backend(),
            stub_provider(),
            test_db(),
            None,
            None,
        );
        app.open_empty_role_editor();
        app.handle_role_editor_list_key(KeyCode::Char('a'));
        app.role_editor_name.set("new-role");
        app.submit_role_editor();

        let role = &app.edit_project().unwrap().role_editor_roles[0];
        assert!(role.permissions.permission_mode.is_none());
        assert!(role.permissions.tools.is_none());
        assert!(role.permissions.append_system_prompt.is_none());
    }

    #[test]
    fn open_role_for_editing_populates_fields() {
        use crate::session::{RoleConfig, RolePermissions};

        let config = ProjectConfig {
            name: "test".to_string(),
            repos: vec![],
            roles: vec![RoleConfig {
                name: "reviewer".to_string(),
                description: "Read-only".to_string(),
                permissions: RolePermissions {
                    permission_mode: Some("plan".to_string()),
                    allowed_tools: vec!["Read".to_string(), "Bash(git:*)".to_string()],
                    disallowed_tools: vec!["Edit".to_string()],
                    ..RolePermissions::default()
                },
            }],
            mcp_servers: vec![],
            id: None,
        };
        let mut app = App::new(
            24,
            120,
            stub_backend(),
            stub_provider(),
            test_db_with_project(&config),
            None,
            None,
        );
        app.open_role_editor();
        app.open_role_for_editing(0);

        assert_eq!(app.role_editor_name.value(), "reviewer");
        assert_eq!(app.role_editor_description.value(), "Read-only");
        assert_eq!(
            app.role_editor_allowed_tools.items,
            vec!["Read".to_string(), "Bash(git:*)".to_string()]
        );
        assert_eq!(
            app.role_editor_disallowed_tools.items,
            vec!["Edit".to_string()]
        );
        assert_eq!(app.role_editor_editing_index, Some(0));
    }

    #[test]
    fn role_editor_tab_cycles_fields_forward() {
        use role_editor_modal::RoleEditorField;
        let mut app = App::new(
            24,
            120,
            stub_backend(),
            stub_provider(),
            test_db(),
            None,
            None,
        );
        app.open_role_editor();
        app.handle_role_editor_list_key(KeyCode::Char('a'));

        assert_eq!(app.role_editor_field, RoleEditorField::Name);
        app.handle_role_editor_editor_key(KeyCode::Tab);
        assert_eq!(app.role_editor_field, RoleEditorField::Description);
        app.handle_role_editor_editor_key(KeyCode::Tab);
        assert_eq!(app.role_editor_field, RoleEditorField::AllowedTools);
        app.handle_role_editor_editor_key(KeyCode::Tab);
        assert_eq!(app.role_editor_field, RoleEditorField::DisallowedTools);
        app.handle_role_editor_editor_key(KeyCode::Tab);
        assert_eq!(app.role_editor_field, RoleEditorField::SystemPrompt);
        app.handle_role_editor_editor_key(KeyCode::Tab);
        assert_eq!(app.role_editor_field, RoleEditorField::Env);
        app.handle_role_editor_editor_key(KeyCode::Tab);
        assert_eq!(app.role_editor_field, RoleEditorField::Name);
    }

    #[test]
    fn role_editor_backtab_cycles_fields_backward() {
        use role_editor_modal::RoleEditorField;
        let mut app = App::new(
            24,
            120,
            stub_backend(),
            stub_provider(),
            test_db(),
            None,
            None,
        );
        app.open_role_editor();
        app.handle_role_editor_list_key(KeyCode::Char('a'));

        assert_eq!(app.role_editor_field, RoleEditorField::Name);
        app.handle_role_editor_editor_key(KeyCode::BackTab);
        assert_eq!(app.role_editor_field, RoleEditorField::Env);
        app.handle_role_editor_editor_key(KeyCode::BackTab);
        assert_eq!(app.role_editor_field, RoleEditorField::SystemPrompt);
        app.handle_role_editor_editor_key(KeyCode::BackTab);
        assert_eq!(app.role_editor_field, RoleEditorField::DisallowedTools);
        app.handle_role_editor_editor_key(KeyCode::BackTab);
        assert_eq!(app.role_editor_field, RoleEditorField::AllowedTools);
        app.handle_role_editor_editor_key(KeyCode::BackTab);
        assert_eq!(app.role_editor_field, RoleEditorField::Description);
        app.handle_role_editor_editor_key(KeyCode::BackTab);
        assert_eq!(app.role_editor_field, RoleEditorField::Name);
    }

    #[test]
    fn role_editor_esc_returns_to_edit_project() {
        let mut app = App::new(
            24,
            120,
            stub_backend(),
            stub_provider(),
            test_db(),
            None,
            None,
        );
        app.open_empty_role_editor();
        app.handle_role_editor_list_key(KeyCode::Char('a'));
        assert_eq!(app.role_editor_view, RoleEditorView::Editor);

        app.handle_role_editor_editor_key(KeyCode::Esc);
        // Esc now closes the role editor overlay, returning to edit-project
        assert!(!app.show_role_editor);
        assert_eq!(app.edit_project().unwrap().field, EditProjectField::Roles);
    }

    #[test]
    fn role_editor_delete_adjusts_list_index() {
        use crate::session::{RoleConfig, RolePermissions};

        let config = ProjectConfig {
            name: "test".to_string(),
            repos: vec![],
            roles: vec![
                RoleConfig {
                    name: "a".to_string(),
                    description: String::new(),
                    permissions: RolePermissions::default(),
                },
                RoleConfig {
                    name: "b".to_string(),
                    description: String::new(),
                    permissions: RolePermissions::default(),
                },
            ],
            mcp_servers: vec![],
            id: None,
        };
        let mut app = App::new(
            24,
            120,
            stub_backend(),
            stub_provider(),
            test_db_with_project(&config),
            None,
            None,
        );
        app.open_role_editor();
        // Select the last role
        app.edit_project_mut().unwrap().role_editor_list_index = 1;
        // Delete it
        app.handle_role_editor_list_key(KeyCode::Char('d'));
        assert_eq!(app.edit_project().unwrap().role_editor_roles.len(), 1);
        assert_eq!(app.edit_project().unwrap().role_editor_list_index, 0);
    }

    #[test]
    fn role_editor_submit_clears_error_on_success() {
        let mut app = App::new(
            24,
            120,
            stub_backend(),
            stub_provider(),
            test_db(),
            None,
            None,
        );
        app.open_empty_role_editor();
        app.handle_role_editor_list_key(KeyCode::Char('a'));

        // Trigger an error by submitting with empty name
        app.submit_role_editor();
        assert!(app.status_message.is_some());

        // Now provide a valid name and submit again
        app.role_editor_name.set("valid-role");
        app.submit_role_editor();
        assert!(app
            .status_message
            .as_ref()
            .map_or(true, |m| m.level != StatusLevel::Error));
        assert_eq!(app.edit_project().unwrap().role_editor_roles.len(), 1);
    }

    #[test]
    fn tool_list_state_add_and_confirm() {
        let mut tls = ToolListState::new();
        assert!(tls.items.is_empty());

        tls.start_adding();
        assert_eq!(tls.mode, role_editor_modal::ToolListMode::Adding);

        for c in "Bash(git:*)".chars() {
            tls.input.insert(c);
        }
        tls.confirm_add();

        assert_eq!(tls.items, vec!["Bash(git:*)".to_string()]);
        assert_eq!(tls.selected, 0);
        assert_eq!(tls.mode, role_editor_modal::ToolListMode::Browse);
    }

    #[test]
    fn tool_list_state_add_and_cancel() {
        let mut tls = ToolListState::new();
        tls.start_adding();
        for c in "Read".chars() {
            tls.input.insert(c);
        }
        tls.cancel_add();

        assert!(tls.items.is_empty());
        assert_eq!(tls.mode, role_editor_modal::ToolListMode::Browse);
    }

    #[test]
    fn tool_list_state_confirm_empty_input_is_no_op() {
        let mut tls = ToolListState::new();
        tls.start_adding();
        tls.confirm_add();
        assert!(tls.items.is_empty());
    }

    #[test]
    fn tool_list_state_confirm_whitespace_input_is_no_op() {
        let mut tls = ToolListState::new();
        tls.start_adding();
        tls.input.insert(' ');
        tls.input.insert(' ');
        tls.confirm_add();
        assert!(tls.items.is_empty());
    }

    #[test]
    fn tool_list_state_delete_adjusts_index() {
        let mut tls = ToolListState::new();
        tls.items = vec!["A".into(), "B".into(), "C".into()];
        tls.selected = 2;
        tls.delete_selected();
        assert_eq!(tls.items, vec!["A".to_string(), "B".to_string()]);
        assert_eq!(tls.selected, 1);
    }

    #[test]
    fn tool_list_state_delete_from_empty_is_no_op() {
        let mut tls = ToolListState::new();
        tls.delete_selected();
        assert!(tls.items.is_empty());
    }

    #[test]
    fn tool_list_state_navigation() {
        let mut tls = ToolListState::new();
        tls.items = vec!["A".into(), "B".into(), "C".into()];
        assert_eq!(tls.selected, 0);

        tls.move_down();
        assert_eq!(tls.selected, 1);
        tls.move_down();
        assert_eq!(tls.selected, 2);
        tls.move_down(); // at end, should not advance
        assert_eq!(tls.selected, 2);

        tls.move_up();
        assert_eq!(tls.selected, 1);
        tls.move_up();
        assert_eq!(tls.selected, 0);
        tls.move_up(); // at start, should not go negative
        assert_eq!(tls.selected, 0);
    }

    #[test]
    fn tool_list_state_load_resets_state() {
        let mut tls = ToolListState::new();
        tls.items = vec!["old".into()];
        tls.selected = 0;
        tls.mode = role_editor_modal::ToolListMode::Adding;
        tls.input.insert('x');

        tls.load(&["new1".to_string(), "new2".to_string()]);
        assert_eq!(tls.items, vec!["new1".to_string(), "new2".to_string()]);
        assert_eq!(tls.selected, 0);
        assert_eq!(tls.mode, role_editor_modal::ToolListMode::Browse);
        assert_eq!(tls.input.value(), "");
    }

    #[test]
    fn tool_browse_add_via_key_handler() {
        use role_editor_modal::RoleEditorField;
        let mut app = App::new(
            24,
            120,
            stub_backend(),
            stub_provider(),
            test_db(),
            None,
            None,
        );
        app.open_role_editor();
        app.handle_role_editor_list_key(KeyCode::Char('a'));

        // Navigate to AllowedTools
        app.role_editor_field = RoleEditorField::AllowedTools;

        // Press 'a' to start adding
        app.handle_role_editor_editor_key(KeyCode::Char('a'));
        assert_eq!(
            app.role_editor_allowed_tools.mode,
            role_editor_modal::ToolListMode::Adding
        );

        // Type "Read" and confirm
        for c in "Read".chars() {
            app.handle_role_editor_editor_key(KeyCode::Char(c));
        }
        app.handle_role_editor_editor_key(KeyCode::Enter);

        assert_eq!(
            app.role_editor_allowed_tools.items,
            vec!["Read".to_string()]
        );
        assert_eq!(
            app.role_editor_allowed_tools.mode,
            role_editor_modal::ToolListMode::Browse
        );
    }

    #[test]
    fn tool_browse_delete_via_key_handler() {
        use role_editor_modal::RoleEditorField;
        let mut app = App::new(
            24,
            120,
            stub_backend(),
            stub_provider(),
            test_db(),
            None,
            None,
        );
        app.open_role_editor();
        app.handle_role_editor_list_key(KeyCode::Char('a'));
        app.role_editor_field = RoleEditorField::AllowedTools;
        app.role_editor_allowed_tools.items = vec!["Read".into(), "Write".into()];
        app.role_editor_allowed_tools.selected = 0;

        app.handle_role_editor_editor_key(KeyCode::Char('d'));
        assert_eq!(
            app.role_editor_allowed_tools.items,
            vec!["Write".to_string()]
        );
    }

    #[test]
    fn tool_adding_esc_cancels() {
        use role_editor_modal::RoleEditorField;
        let mut app = App::new(
            24,
            120,
            stub_backend(),
            stub_provider(),
            test_db(),
            None,
            None,
        );
        app.open_role_editor();
        app.handle_role_editor_list_key(KeyCode::Char('a'));
        app.role_editor_field = RoleEditorField::DisallowedTools;

        // Start adding, type something, then cancel
        app.handle_role_editor_editor_key(KeyCode::Char('a'));
        app.handle_role_editor_editor_key(KeyCode::Char('X'));
        app.handle_role_editor_editor_key(KeyCode::Esc);

        assert!(app.role_editor_disallowed_tools.items.is_empty());
        assert_eq!(
            app.role_editor_disallowed_tools.mode,
            role_editor_modal::ToolListMode::Browse
        );
    }

    #[test]
    fn system_prompt_loaded_and_saved() {
        use crate::session::{RoleConfig, RolePermissions};

        let config = ProjectConfig {
            name: "test".to_string(),
            repos: vec![],
            roles: vec![RoleConfig {
                name: "dev".to_string(),
                description: String::new(),
                permissions: RolePermissions {
                    append_system_prompt: Some("Be safe".to_string()),
                    ..RolePermissions::default()
                },
            }],
            mcp_servers: vec![],
            id: None,
        };
        let mut app = App::new(
            24,
            120,
            stub_backend(),
            stub_provider(),
            test_db_with_project(&config),
            None,
            None,
        );
        app.open_role_editor();
        app.open_role_for_editing(0);

        // Verify it was loaded
        assert_eq!(app.role_editor_system_prompt.value(), "Be safe");

        // Modify and submit
        app.role_editor_system_prompt.set("Be very safe");
        app.submit_role_editor();

        assert_eq!(
            app.edit_project().unwrap().role_editor_roles[0]
                .permissions
                .append_system_prompt,
            Some("Be very safe".to_string())
        );
    }

    #[test]
    fn system_prompt_empty_saves_as_none() {
        let config = ProjectConfig {
            name: "test".to_string(),
            repos: vec![],
            roles: vec![],
            mcp_servers: vec![],
            id: None,
        };
        let mut app = App::new(
            24,
            120,
            stub_backend(),
            stub_provider(),
            test_db_with_project(&config),
            None,
            None,
        );
        app.open_role_editor();
        app.handle_role_editor_list_key(KeyCode::Char('a'));
        app.role_editor_name.set("test");
        app.role_editor_system_prompt.set("");
        app.submit_role_editor();

        assert!(app.edit_project().unwrap().role_editor_roles[0]
            .permissions
            .append_system_prompt
            .is_none());
    }

    #[test]
    fn spawn_with_one_role_auto_assigns() {
        use crate::session::{RoleConfig, RolePermissions};
        let config = ProjectConfig {
            name: "test".to_string(),
            repos: vec![],
            roles: vec![RoleConfig {
                name: "only-role".to_string(),
                description: "The only role".to_string(),
                permissions: RolePermissions {
                    permission_mode: Some("plan".to_string()),
                    ..RolePermissions::default()
                },
            }],
            mcp_servers: vec![],
            id: None,
        };
        let app = App::new(
            24,
            120,
            stub_backend(),
            stub_provider(),
            test_db_with_project(&config),
            None,
            None,
        );
        // With exactly 1 role, prepare_spawn should not show selector
        // (it would try to spawn, which needs a runtime — just verify no selector)
        assert!(!matches!(app.modal, modals::Modal::RoleSelector(_)));
    }

    // --- Project loading helper tests ---

    #[test]
    fn shared_project_to_info_preserves_id() {
        let proj_config = ProjectConfig {
            name: "Test Project".to_string(),
            repos: vec![PathBuf::from("/path/to/repo")],
            roles: Vec::new(),
            mcp_servers: Vec::new(),
            id: None,
        };
        let proj_id = proj_config.deterministic_id();

        let shared_proj = sync::SharedProject {
            id: proj_id,
            name: "Test Project".to_string(),
            repos: vec![PathBuf::from("/path/to/repo")],
            roles: Vec::new(),
            mcp_servers: Vec::new(),
        };

        let info = shared_project_to_info(shared_proj.clone());

        assert_eq!(info.id, shared_proj.id);
        assert_eq!(info.config.name, "Test Project");
        assert_eq!(info.config.repos, vec![PathBuf::from("/path/to/repo")]);
        assert!(info.config.roles.is_empty());
    }

    #[test]
    fn shared_project_to_info_multiple_repos() {
        let proj_config = ProjectConfig {
            name: "Multi Repo".to_string(),
            repos: vec![
                PathBuf::from("/repo1"),
                PathBuf::from("/repo2"),
                PathBuf::from("/repo3"),
            ],
            roles: Vec::new(),
            mcp_servers: Vec::new(),
            id: None,
        };
        let proj_id = proj_config.deterministic_id();

        let shared_proj = sync::SharedProject {
            id: proj_id,
            name: "Multi Repo".to_string(),
            repos: vec![
                PathBuf::from("/repo1"),
                PathBuf::from("/repo2"),
                PathBuf::from("/repo3"),
            ],
            roles: Vec::new(),
            mcp_servers: Vec::new(),
        };

        let info = shared_project_to_info(shared_proj.clone());

        assert_eq!(info.config.repos.len(), 3);
        assert_eq!(info.config.repos[0], PathBuf::from("/repo1"));
        assert_eq!(info.config.repos[1], PathBuf::from("/repo2"));
        assert_eq!(info.config.repos[2], PathBuf::from("/repo3"));
    }

    #[test]
    fn load_projects_from_db_returns_db_project() {
        let db = test_db();
        let proj_config = ProjectConfig {
            name: "DB Project".to_string(),
            repos: vec![PathBuf::from("/db/repo")],
            roles: Vec::new(),
            mcp_servers: Vec::new(),
            id: None,
        };
        let proj_id = proj_config.deterministic_id();
        db.insert_project(proj_id, "DB Project", &[PathBuf::from("/db/repo")])
            .unwrap();

        let projects = load_projects_from_db(&db);

        assert_eq!(projects.len(), 1);
        assert_eq!(projects[0].config.name, "DB Project");
        assert_eq!(projects[0].id, proj_id);
    }

    #[test]
    fn load_projects_from_db_empty_returns_empty() {
        let db = test_db();

        let projects = load_projects_from_db(&db);

        assert!(projects.is_empty());
    }

    #[test]
    fn empty_db_app_has_valid_active_project_index() {
        let app = App::new(
            24,
            120,
            stub_backend(),
            stub_provider(),
            test_db(),
            None,
            None,
        );
        // With an empty DB, the project list is empty, but the index should be valid
        assert!(
            app.projects.is_empty() || app.active_project_index < app.projects.len(),
            "active_project_index {} is out of bounds for {} projects",
            app.active_project_index,
            app.projects.len()
        );
    }

    #[test]
    fn load_projects_from_db_loads_roles() {
        let db = test_db();
        let proj_config = ProjectConfig {
            name: "Test".to_string(),
            repos: vec![PathBuf::from("/repo")],
            roles: Vec::new(),
            mcp_servers: Vec::new(),
            id: None,
        };
        let proj_id = proj_config.deterministic_id();
        db.insert_project(proj_id, "Test", &[PathBuf::from("/repo")])
            .unwrap();

        let role = crate::session::RoleConfig {
            name: "reviewer".to_string(),
            description: "Code reviewer".to_string(),
            permissions: crate::session::RolePermissions::default(),
        };
        db.replace_roles(proj_id, &[role]).unwrap();

        let projects = load_projects_from_db(&db);

        assert_eq!(projects.len(), 1);
        assert_eq!(projects[0].config.roles.len(), 1);
        assert_eq!(projects[0].config.roles[0].name, "reviewer");
    }

    #[test]
    fn load_projects_from_db_seeds_developer_role_for_roleless_project() {
        let db = test_db();
        let proj_config = ProjectConfig {
            name: "NoRoles".to_string(),
            repos: vec![PathBuf::from("/repo")],
            roles: Vec::new(),
            mcp_servers: Vec::new(),
            id: None,
        };
        let proj_id = proj_config.deterministic_id();
        db.insert_project(proj_id, "NoRoles", &[PathBuf::from("/repo")])
            .unwrap();

        let projects = load_projects_from_db(&db);

        assert_eq!(projects.len(), 1);
        assert_eq!(projects[0].config.roles.len(), 1);
        assert_eq!(projects[0].config.roles[0].name, "developer");
        assert_eq!(
            projects[0].config.roles[0].permissions.permission_mode,
            Some("acceptEdits".to_string())
        );

        // Verify the role was persisted to DB (subsequent load finds it).
        let reloaded = load_projects_from_db(&db);
        assert_eq!(reloaded[0].config.roles.len(), 1);
        assert_eq!(reloaded[0].config.roles[0].name, "developer");
    }

    #[test]
    fn load_projects_from_db_skips_seeding_project_with_existing_roles() {
        let db = test_db();
        let proj_config = ProjectConfig {
            name: "HasRoles".to_string(),
            repos: vec![PathBuf::from("/repo")],
            roles: Vec::new(),
            mcp_servers: Vec::new(),
            id: None,
        };
        let proj_id = proj_config.deterministic_id();
        db.insert_project(proj_id, "HasRoles", &[PathBuf::from("/repo")])
            .unwrap();
        db.replace_roles(
            proj_id,
            &[crate::session::RoleConfig {
                name: "custom".to_string(),
                description: String::new(),
                permissions: crate::session::RolePermissions::default(),
            }],
        )
        .unwrap();

        let projects = load_projects_from_db(&db);

        assert_eq!(projects[0].config.roles.len(), 1);
        assert_eq!(projects[0].config.roles[0].name, "custom");
    }

    #[test]
    fn load_projects_from_db_multiple_projects() {
        let db = test_db();

        let config_a = ProjectConfig {
            name: "ProjectA".to_string(),
            repos: vec![PathBuf::from("/a")],
            roles: Vec::new(),
            mcp_servers: Vec::new(),
            id: None,
        };
        let config_b = ProjectConfig {
            name: "ProjectB".to_string(),
            repos: vec![PathBuf::from("/b")],
            roles: Vec::new(),
            mcp_servers: Vec::new(),
            id: None,
        };
        db.insert_project(
            config_a.deterministic_id(),
            "ProjectA",
            &[PathBuf::from("/a")],
        )
        .unwrap();
        db.insert_project(
            config_b.deterministic_id(),
            "ProjectB",
            &[PathBuf::from("/b")],
        )
        .unwrap();

        let projects = load_projects_from_db(&db);

        assert_eq!(projects.len(), 2);
        assert!(projects.iter().any(|p| p.config.name == "ProjectA"));
        assert!(projects.iter().any(|p| p.config.name == "ProjectB"));
    }

    #[test]
    fn save_project_to_db_restores_soft_deleted_project() {
        let backend = stub_backend();
        let provider = stub_provider();
        let db = test_db();
        let config = ProjectConfig {
            name: "TestProject".to_string(),
            repos: vec![PathBuf::from("/repo")],
            roles: Vec::new(),
            mcp_servers: Vec::new(),
            id: None,
        };
        let id = config.deterministic_id();

        // Insert then soft-delete to create the PK conflict scenario
        db.insert_project(id, "TestProject", &[PathBuf::from("/repo")])
            .unwrap();
        db.soft_delete_project(id).unwrap();
        assert!(!db.project_exists(id).unwrap());

        let app = App::new(24, 120, backend, provider, db, None, None);

        // Create a project with the same deterministic ID
        let project = ProjectInfo::new(config);
        app.save_project_to_db(&project);

        // The project should be restored and visible
        assert!(app.db.project_exists(id).unwrap());
        let projects = app.db.list_active_projects().unwrap();
        let found = projects.iter().find(|p| p.id == id);
        assert!(found.is_some());
        assert_eq!(found.unwrap().name, "TestProject");
    }

    // --- Global keybinding tests ---

    #[test]
    fn ctrl_h_focuses_project_list_from_terminal() {
        let mut app = app_with_sessions(1);
        app.focus = InputFocus::Terminal;
        app.handle_key(KeyCode::Char('h'), KeyModifiers::CONTROL);
        assert_eq!(app.focus, InputFocus::ProjectList);
    }

    #[test]
    fn ctrl_h_focuses_project_list_from_session_list() {
        let mut app = app_with_sessions(1);
        app.focus = InputFocus::SessionList;
        app.handle_key(KeyCode::Char('h'), KeyModifiers::CONTROL);
        assert_eq!(app.focus, InputFocus::ProjectList);
    }

    #[test]
    fn ctrl_c_copies_selection_or_falls_through() {
        let mut app = app_with_sessions(1);
        let initial_count = app.sessions.len();
        // With no selection, Ctrl+C should NOT close session (it falls through to terminal)
        app.handle_key(KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert_eq!(app.sessions.len(), initial_count);
    }

    #[test]
    fn any_key_clears_text_selection() {
        let mut app = app_with_sessions(1);
        // Set up a fake selection
        app.text_selection = Some(Selection::new(
            TermPos { row: 0, col: 0 },
            PaneBounds::from_rect(ratatui::layout::Rect::new(0, 0, 80, 24)),
        ));
        assert!(app.text_selection.is_some());

        // Any non-copy key should clear the selection
        app.handle_key(KeyCode::Char('a'), KeyModifiers::empty());
        assert!(app.text_selection.is_none());
    }

    #[test]
    fn ctrl_v_clears_selection() {
        let mut app = app_with_sessions(1);
        app.text_selection = Some(Selection::new(
            TermPos { row: 0, col: 0 },
            PaneBounds::from_rect(ratatui::layout::Rect::new(0, 0, 80, 24)),
        ));

        // Ctrl+V should clear selection (paste)
        app.handle_key(KeyCode::Char('v'), KeyModifiers::CONTROL);
        assert!(app.text_selection.is_none());
    }

    #[test]
    fn scroll_clears_selection() {
        let mut app = app_with_sessions(1);
        app.text_selection = Some(Selection::new(
            TermPos { row: 0, col: 0 },
            PaneBounds::from_rect(ratatui::layout::Rect::new(0, 0, 80, 24)),
        ));

        app.scroll_terminal_up(1);
        assert!(app.text_selection.is_none());
    }

    #[test]
    fn ctrl_d_deletes_session_from_session_list() {
        let mut app = app_with_sessions(2);
        app.focus = InputFocus::SessionList;
        let initial_count = app.sessions.len();
        app.handle_key(KeyCode::Char('d'), KeyModifiers::CONTROL);
        assert!(app.sessions.len() < initial_count);
    }

    #[test]
    fn ctrl_d_shows_delete_project_modal_from_project_list() {
        let mut app = app_with_sessions(0);
        // Need at least 2 projects (can't delete if only 1).
        app.projects.push(ProjectInfo {
            id: ProjectId::default(),
            config: ProjectConfig {
                name: "Extra".into(),
                repos: vec![],
                roles: vec![],
                mcp_servers: vec![],
                id: None,
            },
            session_ids: vec![],
            is_admin: false,
        });
        app.active_project_index = 1;
        app.focus = InputFocus::ProjectList;
        app.handle_key(KeyCode::Char('d'), KeyModifiers::CONTROL);
        assert!(matches!(app.modal, modals::Modal::DeleteProject(_)));
    }

    #[test]
    fn ctrl_d_forwards_to_pty_from_terminal() {
        let mut app = app_with_sessions(1);
        app.focus = InputFocus::Terminal;
        app.handle_key(KeyCode::Char('d'), KeyModifiers::CONTROL);
        // Should NOT show delete modal — Ctrl+D is forwarded to PTY
        assert!(!matches!(app.modal, modals::Modal::DeleteProject(_)));
        assert_eq!(app.sessions.len(), 1); // session not closed either
    }

    #[test]
    fn ctrl_r_no_crash_without_sessions() {
        let mut app = app_with_sessions(0);
        app.focus = InputFocus::Terminal;
        // Should not crash when there are no sessions
        app.handle_key(KeyCode::Char('r'), KeyModifiers::CONTROL);
        assert!(app.status_message.is_none());
    }

    #[test]
    fn f1_shows_help_from_any_context() {
        let mut app = app_with_sessions(0);
        for focus in [
            InputFocus::ProjectList,
            InputFocus::SessionList,
            InputFocus::Terminal,
        ] {
            app.modal = modals::Modal::None;
            app.focus = focus;
            app.handle_key(KeyCode::F(1), KeyModifiers::NONE);
            assert!(
                matches!(app.modal, modals::Modal::Help),
                "F1 should show help from {focus:?}"
            );
        }
    }

    #[test]
    fn f1_does_not_activate_during_modal() {
        let mut app = app_with_sessions(0);
        app.modal = modals::Modal::RepoSelector(modals::RepoSelectorModal::default());
        app.handle_key(KeyCode::F(1), KeyModifiers::NONE);
        assert!(!matches!(app.modal, modals::Modal::Help));
    }

    #[test]
    fn f2_toggles_info_panel() {
        let mut app = app_with_sessions(0);
        assert!(!app.show_info_panel);
        app.handle_key(KeyCode::F(2), KeyModifiers::NONE);
        assert!(app.show_info_panel);
        app.handle_key(KeyCode::F(2), KeyModifiers::NONE);
        assert!(!app.show_info_panel);
    }

    #[test]
    fn ctrl_l_cycles_focus() {
        let mut app = app_with_sessions(1);
        app.focus = InputFocus::ProjectList;
        app.handle_key(KeyCode::Char('l'), KeyModifiers::CONTROL);
        assert_eq!(app.focus, InputFocus::SessionList);
        app.handle_key(KeyCode::Char('l'), KeyModifiers::CONTROL);
        assert_eq!(app.focus, InputFocus::Terminal);
        app.handle_key(KeyCode::Char('l'), KeyModifiers::CONTROL);
        assert_eq!(app.focus, InputFocus::ProjectList);
    }

    // --- Context-sensitive Ctrl+J/K tests ---

    fn app_with_projects(count: usize) -> App {
        let mut app = app_with_sessions(0);
        // app already has one default project at index 0
        for i in 1..count {
            app.projects.push(ProjectInfo {
                id: ProjectId::default(),
                config: ProjectConfig {
                    name: format!("Project {}", i + 1),
                    repos: vec![],
                    roles: vec![],
                    mcp_servers: vec![],
                    id: None,
                },
                session_ids: vec![],
                is_admin: false,
            });
        }
        app
    }

    #[test]
    fn ctrl_j_moves_project_forward_when_project_list_focused() {
        let mut app = app_with_projects(3);
        app.focus = InputFocus::ProjectList;
        app.active_project_index = 0;
        app.handle_key(KeyCode::Char('j'), KeyModifiers::CONTROL);
        assert_eq!(app.active_project_index, 1);
    }

    #[test]
    fn ctrl_k_moves_project_backward_when_project_list_focused() {
        let mut app = app_with_projects(3);
        app.focus = InputFocus::ProjectList;
        app.active_project_index = 2;
        app.handle_key(KeyCode::Char('k'), KeyModifiers::CONTROL);
        assert_eq!(app.active_project_index, 1);
    }

    #[test]
    fn ctrl_j_switches_session_when_session_list_focused() {
        let mut app = app_with_sessions(3);
        app.focus = InputFocus::SessionList;
        app.active_index = 0;
        app.handle_key(KeyCode::Char('j'), KeyModifiers::CONTROL);
        assert_eq!(app.active_index, 1);
    }

    #[test]
    fn ctrl_j_switches_session_when_terminal_focused() {
        let mut app = app_with_sessions(3);
        app.focus = InputFocus::Terminal;
        app.active_index = 0;
        app.handle_key(KeyCode::Char('j'), KeyModifiers::CONTROL);
        assert_eq!(app.active_index, 1);
    }

    #[test]
    fn ctrl_j_at_last_project_wraps_to_first() {
        let mut app = app_with_projects(3);
        app.focus = InputFocus::ProjectList;
        app.active_project_index = 2;
        app.handle_key(KeyCode::Char('j'), KeyModifiers::CONTROL);
        assert_eq!(app.active_project_index, 0);
    }

    #[test]
    fn ctrl_k_at_first_project_wraps_to_last() {
        let mut app = app_with_projects(3);
        app.focus = InputFocus::ProjectList;
        app.active_project_index = 0;
        app.handle_key(KeyCode::Char('k'), KeyModifiers::CONTROL);
        assert_eq!(app.active_project_index, 2);
    }

    // --- DB persistence tests ---

    #[test]
    fn load_persisted_state_empty_db_returns_none() {
        let app = App::new(
            24,
            80,
            stub_backend(),
            stub_provider(),
            test_db(),
            None,
            None,
        );
        assert!(app.load_persisted_state_from_db().is_none());
    }

    #[test]
    fn load_persisted_state_sessions_without_claude_id_returns_none() {
        let db = test_db();
        let proj_config = ProjectConfig {
            name: "test".to_string(),
            repos: vec![],
            roles: Vec::new(),
            mcp_servers: Vec::new(),
            id: None,
        };
        let pid = proj_config.deterministic_id();
        db.insert_project(pid, "test", &[]).unwrap();

        // Session without agent_session_id — not resumable
        let session = sync::SharedSession {
            id: SessionId::default(),
            name: "1".to_string(),
            project_id: pid,
            role: "developer".to_string(),
            backend_id: "thurbox:@0".to_string(),
            backend_type: "tmux".to_string(),
            agent_session_id: None,
            cwd: None,
            additional_dirs: Vec::new(),
            worktrees: Vec::new(),
            shell_backend_id: None,
            tombstone: false,
            tombstone_at: None,
        };
        db.upsert_session(&session).unwrap();

        let app = App::new(24, 80, stub_backend(), stub_provider(), db, None, None);
        assert!(app.load_persisted_state_from_db().is_none());
    }

    #[test]
    fn load_persisted_state_filters_to_resumable_only() {
        let db = test_db();
        let proj_config = ProjectConfig {
            name: "test".to_string(),
            repos: vec![],
            roles: Vec::new(),
            mcp_servers: Vec::new(),
            id: None,
        };
        let pid = proj_config.deterministic_id();
        db.insert_project(pid, "test", &[]).unwrap();

        // Non-resumable session
        let s1 = sync::SharedSession {
            id: SessionId::default(),
            name: "1".to_string(),
            project_id: pid,
            role: "developer".to_string(),
            backend_id: "thurbox:@0".to_string(),
            backend_type: "tmux".to_string(),
            agent_session_id: None,
            cwd: None,
            additional_dirs: Vec::new(),
            worktrees: Vec::new(),
            shell_backend_id: None,
            tombstone: false,
            tombstone_at: None,
        };
        db.upsert_session(&s1).unwrap();

        // Resumable session
        let s2 = sync::SharedSession {
            id: SessionId::default(),
            name: "2".to_string(),
            project_id: pid,
            role: "developer".to_string(),
            backend_id: "thurbox:@1".to_string(),
            backend_type: "tmux".to_string(),
            agent_session_id: Some("claude-abc".to_string()),
            cwd: None,
            additional_dirs: Vec::new(),
            worktrees: Vec::new(),
            shell_backend_id: None,
            tombstone: false,
            tombstone_at: None,
        };
        db.upsert_session(&s2).unwrap();
        db.set_session_counter(7).unwrap();

        let app = App::new(24, 80, stub_backend(), stub_provider(), db, None, None);
        let (sessions, counter) = app.load_persisted_state_from_db().unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].name, "2");
        assert_eq!(counter, 7);
    }

    #[test]
    fn save_state_roundtrips_sessions() {
        let backend_arc = stub_backend_arc();
        let provider = stub_provider();
        let mut app = App::new(
            24,
            120,
            BackendRegistry::new(backend_arc.clone()),
            provider.clone(),
            test_db_with_project(&test_project_config()),
            None,
            None,
        );

        // Add a session
        let session = Session::stub("test-session", &backend_arc, &provider);
        let sid = session.info.id;
        app.sessions.push(session);
        app.projects[0].session_ids.push(sid);

        // Save to DB (only persists sessions + counter, not projects)
        app.save_state();

        // Verify session in DB
        let sessions = app.db.list_active_sessions().unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].name, "test-session");
    }

    #[test]
    fn save_state_persists_session_counter() {
        let mut app = App::new(
            24,
            120,
            stub_backend(),
            stub_provider(),
            test_db(),
            None,
            None,
        );
        app.session_counter = 42;

        app.save_state();

        let counter = app.db.get_session_counter().unwrap();
        assert_eq!(counter, 42);
    }

    #[test]
    fn session_to_shared_converts_correctly() {
        let backend_arc = stub_backend_arc();
        let provider = stub_provider();
        let mut app = App::new(
            24,
            120,
            BackendRegistry::new(backend_arc.clone()),
            provider.clone(),
            test_db_with_project(&test_project_config()),
            None,
            None,
        );

        let mut session = Session::stub("test-session", &backend_arc, &provider);
        session.info.role = "reviewer".to_string();
        session.info.cwd = Some(PathBuf::from("/home/user"));
        session.info.agent_session_id = Some("claude-xyz".to_string());

        let sid = session.info.id;
        app.sessions.push(session);
        app.projects[0].session_ids.push(sid);

        let shared = app.session_to_shared(&app.sessions[0]);
        assert_eq!(shared.id, sid);
        assert_eq!(shared.name, "test-session");
        assert_eq!(shared.role, "reviewer");
        assert_eq!(shared.cwd, Some(PathBuf::from("/home/user")));
        assert_eq!(shared.agent_session_id, Some("claude-xyz".to_string()));
        assert!(!shared.tombstone);
        assert!(shared.tombstone_at.is_none());
    }

    // --- Edit-project modal tests ---

    /// Create an App with a single project for edit-project tests.
    fn app_with_project(name: &str, repos: Vec<PathBuf>) -> App {
        let config = ProjectConfig {
            name: name.to_string(),
            repos,
            roles: Vec::new(),
            mcp_servers: Vec::new(),
            id: None,
        };
        App::new(
            24,
            120,
            stub_backend(),
            stub_provider(),
            test_db_with_project(&config),
            None,
            None,
        )
    }

    #[test]
    fn open_edit_project_populates_fields() {
        let mut app = app_with_project("my-proj", vec![PathBuf::from("/repo/a")]);
        app.open_edit_project_modal();
        assert!(app.is_edit_project_open());
        assert_eq!(app.edit_project().unwrap().name.value(), "my-proj");
        assert_eq!(
            app.edit_project().unwrap().repos,
            vec![PathBuf::from("/repo/a")]
        );
        assert_eq!(app.edit_project().unwrap().field, EditProjectField::Name);
        assert!(app.edit_project().unwrap().original_id.is_some());
    }

    #[test]
    fn submit_edit_project_updates_name_and_repos() {
        let mut app = app_with_project("old-name", vec![PathBuf::from("/repo/a")]);
        let original_id = app.projects[0].id;

        app.open_edit_project_modal();
        app.edit_project_mut().unwrap().name.clear();
        app.edit_project_mut().unwrap().name.set("new-name");
        app.edit_project_mut().unwrap().repos =
            vec![PathBuf::from("/repo/b"), PathBuf::from("/repo/c")];
        app.submit_edit_project();

        assert!(!app.is_edit_project_open());
        assert_eq!(app.projects[0].config.name, "new-name");
        assert_eq!(app.projects[0].config.repos.len(), 2);
        // ID must stay stable (no UUID regeneration)
        assert_eq!(app.projects[0].id, original_id);
    }

    #[test]
    fn submit_edit_project_rejects_empty_name() {
        let mut app = app_with_project("test", vec![PathBuf::from("/repo")]);
        app.open_edit_project_modal();
        app.edit_project_mut().unwrap().name.clear();
        app.submit_edit_project();

        // Modal should still be open
        assert!(app.is_edit_project_open());
        assert!(app.status_message.is_some());
    }

    #[test]
    fn submit_edit_project_rejects_empty_repos() {
        let mut app = app_with_project("test", vec![PathBuf::from("/repo")]);
        app.open_edit_project_modal();
        app.edit_project_mut().unwrap().repos.clear();
        app.submit_edit_project();

        assert!(app.is_edit_project_open());
        assert!(app.status_message.is_some());
    }

    #[test]
    fn submit_edit_project_auto_adds_pending_path() {
        let mut app = app_with_project("test", vec![PathBuf::from("/repo/a")]);
        app.open_edit_project_modal();
        app.edit_project_mut().unwrap().path.set("/repo/b");
        app.submit_edit_project();

        assert!(!app.is_edit_project_open());
        assert_eq!(app.projects[0].config.repos.len(), 2);
        assert_eq!(app.projects[0].config.repos[1], PathBuf::from("/repo/b"));
    }

    #[test]
    fn close_edit_project_clears_all_state() {
        let mut app = app_with_project("test", vec![PathBuf::from("/repo")]);
        app.open_edit_project_modal();
        assert!(app.is_edit_project_open());

        app.close_edit_project_modal();
        assert!(!app.is_edit_project_open());
        // After closing, the EditProject modal is gone entirely
        assert!(app.edit_project().is_none());
    }

    #[test]
    fn edit_project_tab_cycles_through_all_fields() {
        let mut app = app_with_project("test", vec![PathBuf::from("/repo")]);
        app.open_edit_project_modal();

        // Name -> Path
        assert_eq!(app.edit_project().unwrap().field, EditProjectField::Name);
        app.handle_key(KeyCode::Tab, KeyModifiers::NONE);
        assert_eq!(app.edit_project().unwrap().field, EditProjectField::Path);

        // Path -> RepoList (repos not empty, no suggestion)
        app.handle_key(KeyCode::Tab, KeyModifiers::NONE);
        assert_eq!(
            app.edit_project().unwrap().field,
            EditProjectField::RepoList
        );

        // RepoList -> Roles
        app.handle_key(KeyCode::Tab, KeyModifiers::NONE);
        assert_eq!(app.edit_project().unwrap().field, EditProjectField::Roles);

        // Roles -> McpServers
        app.handle_key(KeyCode::Tab, KeyModifiers::NONE);
        assert_eq!(
            app.edit_project().unwrap().field,
            EditProjectField::McpServers
        );

        // McpServers -> Name
        app.handle_key(KeyCode::Tab, KeyModifiers::NONE);
        assert_eq!(app.edit_project().unwrap().field, EditProjectField::Name);
    }

    #[test]
    fn edit_project_tab_skips_repo_list_when_empty() {
        let mut app = app_with_project("test", vec![PathBuf::from("/repo")]);
        app.open_edit_project_modal();
        app.edit_project_mut().unwrap().repos.clear();

        // Name -> Path
        app.handle_key(KeyCode::Tab, KeyModifiers::NONE);
        assert_eq!(app.edit_project().unwrap().field, EditProjectField::Path);

        // Path -> Roles (skip empty RepoList)
        app.handle_key(KeyCode::Tab, KeyModifiers::NONE);
        assert_eq!(app.edit_project().unwrap().field, EditProjectField::Roles);
    }

    #[test]
    fn edit_project_esc_closes_modal() {
        let mut app = app_with_project("test", vec![PathBuf::from("/repo")]);
        app.open_edit_project_modal();
        app.handle_key(KeyCode::Esc, KeyModifiers::NONE);
        assert!(!app.is_edit_project_open());
    }

    #[test]
    fn edit_project_repo_list_delete() {
        let mut app = app_with_project(
            "test",
            vec![PathBuf::from("/repo/a"), PathBuf::from("/repo/b")],
        );
        app.open_edit_project_modal();
        app.edit_project_mut().unwrap().field = EditProjectField::RepoList;
        app.edit_project_mut().unwrap().repo_index = 0;

        // Delete first repo
        app.handle_key(KeyCode::Char('d'), KeyModifiers::NONE);
        assert_eq!(app.edit_project().unwrap().repos.len(), 1);
        assert_eq!(
            app.edit_project().unwrap().repos[0],
            PathBuf::from("/repo/b")
        );
    }

    #[test]
    fn edit_project_repo_list_empty_after_delete_switches_to_path() {
        let mut app = app_with_project("test", vec![PathBuf::from("/repo/a")]);
        app.open_edit_project_modal();
        app.edit_project_mut().unwrap().field = EditProjectField::RepoList;

        app.handle_key(KeyCode::Char('d'), KeyModifiers::NONE);
        assert!(app.edit_project().unwrap().repos.is_empty());
        assert_eq!(app.edit_project().unwrap().field, EditProjectField::Path);
    }

    #[test]
    fn edit_project_id_stable_on_rename() {
        let mut app = app_with_project("alpha", vec![PathBuf::from("/repo")]);
        let id_before = app.projects[0].id;

        app.open_edit_project_modal();
        app.edit_project_mut().unwrap().name.clear();
        app.edit_project_mut().unwrap().name.set("beta");
        app.submit_edit_project();

        assert_eq!(app.projects[0].config.name, "beta");
        assert_eq!(app.projects[0].id, id_before);
    }

    #[test]
    fn renamed_project_loads_with_roles_from_db() {
        // DB has a project that was renamed, with roles stored in project_roles table.
        let db = test_db();
        let old_config = ProjectConfig {
            name: "old-name".to_string(),
            repos: vec![PathBuf::from("/repo")],
            roles: Vec::new(),
            mcp_servers: Vec::new(),
            id: None,
        };
        let det_id = old_config.deterministic_id();

        // Insert project with old name's ID but renamed
        db.insert_project(det_id, "renamed-proj", &[PathBuf::from("/repo")])
            .unwrap();

        // Store roles in DB
        use crate::session::{RoleConfig, RolePermissions};
        db.replace_roles(
            det_id,
            &[RoleConfig {
                name: "dev".to_string(),
                description: String::new(),
                permissions: RolePermissions::default(),
            }],
        )
        .unwrap();

        let projects = load_projects_from_db(&db);
        let proj = projects.iter().find(|p| p.id == det_id).unwrap();
        assert_eq!(proj.config.name, "renamed-proj");
        assert_eq!(proj.config.roles.len(), 1);
        assert_eq!(proj.config.roles[0].name, "dev");
    }

    #[test]
    fn rename_project_full_lifecycle() {
        // Full lifecycle test: create app, rename project, shutdown, create new app (restart).
        // Verifies no duplicate projects and sessions stay associated.
        let db = test_db();
        let backend_arc = stub_backend_arc();
        let provider = stub_provider();

        // Step 1: Start app with project "TestA"
        let config = ProjectConfig {
            name: "TestA".to_string(),
            repos: vec![PathBuf::from("/repo")],
            roles: Vec::new(),
            mcp_servers: Vec::new(),
            id: None,
        };
        let original_id = config.deterministic_id();
        let id = config.effective_id();
        db.insert_project(id, &config.name, &config.repos).unwrap();
        let mut app = App::new(
            24,
            120,
            BackendRegistry::new(backend_arc.clone()),
            provider.clone(),
            db,
            None,
            None,
        );

        // Verify initial state: 1 project named "TestA"
        assert_eq!(app.projects.len(), 1);
        assert_eq!(app.projects[0].config.name, "TestA");
        assert_eq!(app.projects[0].id, original_id);

        // Step 2: Create a session for TestA
        app.active_project_index = app
            .projects
            .iter()
            .position(|p| p.config.name == "TestA")
            .unwrap();
        let session = Session::stub("1", &backend_arc, &provider);
        let session_id = session.info.id;
        app.sessions.push(session);
        app.projects[app.active_project_index]
            .session_ids
            .push(session_id);

        // Step 3: Rename "TestA" → "TestB" via edit modal
        app.open_edit_project_modal();
        assert!(app.is_edit_project_open());
        app.edit_project_mut().unwrap().name.set("TestB");
        // Repos stay the same (pre-populated from open_edit_project_modal)
        app.submit_edit_project();
        assert!(!app.is_edit_project_open(), "Modal should close on success");

        // Verify: project renamed, ID stable
        let renamed_project = app.projects.iter().find(|p| p.config.name == "TestB");
        assert!(renamed_project.is_some(), "Should have project TestB");
        assert_eq!(
            renamed_project.unwrap().id,
            original_id,
            "ID should be stable"
        );
        assert!(
            app.projects.iter().all(|p| p.config.name != "TestA"),
            "TestA should no longer exist"
        );

        // Step 4: Save state (simulates shutdown)
        app.save_state();

        // Step 5: Simulate restart with the same DB (project already persisted from edit)
        let app2 = App::new(
            24,
            120,
            BackendRegistry::new(backend_arc.clone()),
            provider.clone(),
            app.db,
            None,
            None,
        );

        // Verify: only 1 project, named "TestB"
        assert_eq!(
            app2.projects.len(),
            1,
            "Expected 1 project, got {}: {:?}",
            app2.projects.len(),
            app2.projects
                .iter()
                .map(|p| &p.config.name)
                .collect::<Vec<_>>()
        );
        assert_eq!(app2.projects[0].config.name, "TestB");
        assert_eq!(app2.projects[0].id, original_id);

        // Step 6: Restore sessions
        if let Some((sessions, _counter)) = app2.load_persisted_state_from_db() {
            // Verify session has correct project_id
            assert_eq!(sessions.len(), 1);
            assert_eq!(
                sessions[0].project_id, original_id,
                "Session should reference original project ID"
            );
        }
    }

    #[test]
    fn rename_project_survives_restart_db_only() {
        // After a rename, the DB is the single source of truth.
        // On restart, load_projects_from_db returns the renamed project with stable ID.
        let db = test_db();

        let original_config = ProjectConfig {
            name: "TestA".to_string(),
            repos: vec![PathBuf::from("/repo")],
            roles: Vec::new(),
            mcp_servers: Vec::new(),
            id: None,
        };
        let original_id = original_config.deterministic_id();
        db.insert_project(original_id, "TestA", &[PathBuf::from("/repo")])
            .unwrap();

        // Rename in DB (as submit_edit_project does)
        db.update_project(original_id, "TestB", &[PathBuf::from("/repo")])
            .unwrap();

        // Simulate restart: load from DB only
        let projects = load_projects_from_db(&db);

        assert_eq!(projects.len(), 1);
        assert_eq!(projects[0].config.name, "TestB");
        assert_eq!(projects[0].id, original_id);
    }

    #[test]
    fn rename_project_sessions_survive_restart() {
        // Simulate: project "TestA" has a session, renamed to "TestB", then restart.
        // The session should remain associated with the renamed project via stable ID.
        let db = test_db();

        // Step 1: Insert original project "TestA" into DB
        let original_config = ProjectConfig {
            name: "TestA".to_string(),
            repos: vec![PathBuf::from("/repo")],
            roles: Vec::new(),
            mcp_servers: Vec::new(),
            id: None,
        };
        let original_id = original_config.deterministic_id();
        db.insert_project(original_id, "TestA", &[PathBuf::from("/repo")])
            .unwrap();

        // Step 2: Create a session associated with "TestA"
        let session_id = SessionId::default();
        let shared_session = sync::SharedSession {
            id: session_id,
            name: "Session 1".to_string(),
            project_id: original_id,
            role: "developer".to_string(),
            backend_id: "thurbox:@0".to_string(),
            backend_type: "tmux".to_string(),
            agent_session_id: Some("claude-abc".to_string()),
            cwd: None,
            additional_dirs: Vec::new(),
            worktrees: Vec::new(),
            shell_backend_id: None,
            tombstone: false,
            tombstone_at: None,
        };
        db.upsert_session(&shared_session).unwrap();

        // Step 3: Rename in DB (as submit_edit_project does)
        db.update_project(original_id, "TestB", &[PathBuf::from("/repo")])
            .unwrap();

        // Step 4: Simulate restart — load from DB only
        let projects = load_projects_from_db(&db);
        assert_eq!(projects.len(), 1);
        assert_eq!(projects[0].id, original_id);
        assert_eq!(projects[0].config.name, "TestB");

        // Check session still references the correct project
        let sessions = db.list_active_sessions().unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].project_id, original_id);
    }

    #[test]
    fn session_to_shared_maps_worktree() {
        let backend_arc = stub_backend_arc();
        let provider = stub_provider();
        let mut app = App::new(
            24,
            120,
            BackendRegistry::new(backend_arc.clone()),
            provider.clone(),
            test_db_with_project(&test_project_config()),
            None,
            None,
        );

        let mut session = Session::stub("test-session", &backend_arc, &provider);
        session.info.worktrees = vec![WorktreeInfo {
            repo_path: PathBuf::from("/repo"),
            worktree_path: PathBuf::from("/repo/.git/wt/feat"),
            branch: "feat".to_string(),
        }];

        let sid = session.info.id;
        app.sessions.push(session);
        app.projects[0].session_ids.push(sid);

        let shared = app.session_to_shared(&app.sessions[0]);
        assert_eq!(shared.worktrees.len(), 1);
        let wt = &shared.worktrees[0];
        assert_eq!(wt.branch, "feat");
        assert_eq!(wt.repo_path, PathBuf::from("/repo"));
    }

    // --- Edit-project inline roles tests ---

    fn app_with_roles(roles: Vec<crate::session::RoleConfig>) -> App {
        let config = ProjectConfig {
            name: "test".to_string(),
            repos: vec![PathBuf::from("/repo")],
            roles,
            mcp_servers: vec![],
            id: None,
        };
        App::new(
            24,
            120,
            stub_backend(),
            stub_provider(),
            test_db_with_project(&config),
            None,
            None,
        )
    }

    #[test]
    fn open_edit_project_loads_roles() {
        use crate::session::{RoleConfig, RolePermissions};
        let mut app = app_with_roles(vec![RoleConfig {
            name: "dev".to_string(),
            description: "Developer".to_string(),
            permissions: RolePermissions::default(),
        }]);
        app.open_edit_project_modal();
        assert_eq!(app.edit_project().unwrap().role_editor_roles.len(), 1);
        assert_eq!(app.edit_project().unwrap().role_editor_roles[0].name, "dev");
        assert_eq!(app.edit_project().unwrap().role_editor_list_index, 0);
    }

    #[test]
    fn submit_edit_project_saves_roles() {
        use crate::session::{RoleConfig, RolePermissions};
        let mut app = app_with_roles(vec![]);
        app.open_edit_project_modal();
        // Add a role to the editor state (developer role was seeded on load)
        app.edit_project_mut()
            .unwrap()
            .role_editor_roles
            .push(RoleConfig {
                name: "new-role".to_string(),
                description: String::new(),
                permissions: RolePermissions::default(),
            });
        app.submit_edit_project();
        // Verify the project has both the seeded developer role and the new one
        let project = app
            .projects
            .iter()
            .find(|p| p.config.name == "test")
            .unwrap();
        assert_eq!(project.config.roles.len(), 2);
        assert_eq!(project.config.roles[0].name, "developer");
        assert_eq!(project.config.roles[1].name, "new-role");
    }

    #[test]
    fn close_edit_project_clears_role_editor() {
        use crate::session::{RoleConfig, RolePermissions};
        let mut app = app_with_roles(vec![RoleConfig {
            name: "dev".to_string(),
            description: String::new(),
            permissions: RolePermissions::default(),
        }]);
        app.open_edit_project_modal();
        app.show_role_editor = true; // Simulate role editor being open
        app.close_edit_project_modal();
        assert!(!app.is_edit_project_open());
        assert!(!app.show_role_editor);
        // After closing, the EditProject modal (and its state) is gone
        assert!(app.edit_project().is_none());
    }

    #[test]
    fn edit_project_roles_navigate_and_delete() {
        use crate::session::{RoleConfig, RolePermissions};
        let mut app = app_with_roles(vec![
            RoleConfig {
                name: "a".to_string(),
                description: String::new(),
                permissions: RolePermissions::default(),
            },
            RoleConfig {
                name: "b".to_string(),
                description: String::new(),
                permissions: RolePermissions::default(),
            },
        ]);
        app.open_edit_project_modal();
        app.edit_project_mut().unwrap().field = EditProjectField::Roles;
        assert_eq!(app.edit_project().unwrap().role_editor_list_index, 0);

        // Navigate down
        app.handle_key(KeyCode::Char('j'), KeyModifiers::NONE);
        assert_eq!(app.edit_project().unwrap().role_editor_list_index, 1);

        // Navigate up
        app.handle_key(KeyCode::Char('k'), KeyModifiers::NONE);
        assert_eq!(app.edit_project().unwrap().role_editor_list_index, 0);

        // Delete first role
        app.handle_key(KeyCode::Char('d'), KeyModifiers::NONE);
        assert_eq!(app.edit_project().unwrap().role_editor_roles.len(), 1);
        assert_eq!(app.edit_project().unwrap().role_editor_roles[0].name, "b");
    }

    #[test]
    fn edit_project_roles_add_opens_role_editor() {
        let mut app = app_with_roles(vec![]);
        app.open_edit_project_modal();
        app.edit_project_mut().unwrap().field = EditProjectField::Roles;
        app.handle_key(KeyCode::Char('a'), KeyModifiers::NONE);
        assert!(app.show_role_editor);
        assert_eq!(app.role_editor_view, RoleEditorView::Editor);
        assert!(app.role_editor_editing_index.is_none());
    }

    #[test]
    fn edit_project_roles_edit_opens_role_editor() {
        use crate::session::{RoleConfig, RolePermissions};
        let mut app = app_with_roles(vec![RoleConfig {
            name: "dev".to_string(),
            description: "Developer".to_string(),
            permissions: RolePermissions::default(),
        }]);
        app.open_edit_project_modal();
        app.edit_project_mut().unwrap().field = EditProjectField::Roles;
        app.handle_key(KeyCode::Char('e'), KeyModifiers::NONE);
        assert!(app.show_role_editor);
        assert_eq!(app.role_editor_view, RoleEditorView::Editor);
        assert_eq!(app.role_editor_editing_index, Some(0));
        assert_eq!(app.role_editor_name.value(), "dev");
    }

    #[test]
    fn edit_project_roles_esc_saves_and_closes() {
        use crate::session::{RoleConfig, RolePermissions};
        let mut app = app_with_roles(vec![]);
        app.open_edit_project_modal();
        app.edit_project_mut().unwrap().field = EditProjectField::Roles;
        // Add a role directly to the editor state (developer role was seeded on load)
        app.edit_project_mut()
            .unwrap()
            .role_editor_roles
            .push(RoleConfig {
                name: "added".to_string(),
                description: String::new(),
                permissions: RolePermissions::default(),
            });
        // Esc from Roles field triggers submit_edit_project (saves)
        app.handle_key(KeyCode::Esc, KeyModifiers::NONE);
        assert!(!app.is_edit_project_open());
        let project = app
            .projects
            .iter()
            .find(|p| p.config.name == "test")
            .unwrap();
        assert_eq!(project.config.roles.len(), 2);
        assert_eq!(project.config.roles[0].name, "developer");
        assert_eq!(project.config.roles[1].name, "added");
    }

    #[test]
    fn ctrl_r_no_op_without_agent_session_id() {
        let mut app = app_with_sessions(1);
        // Session exists but has no agent_session_id
        app.sessions[0].info.agent_session_id = None;
        app.focus = InputFocus::Terminal;
        app.handle_key(KeyCode::Char('r'), KeyModifiers::CONTROL);
        // Should be a no-op (no error, no crash)
        assert!(app.status_message.is_none());
    }

    #[test]
    fn session_to_shared_maps_additional_dirs() {
        let backend_arc = stub_backend_arc();
        let provider = stub_provider();
        let mut app = App::new(
            24,
            120,
            BackendRegistry::new(backend_arc.clone()),
            provider.clone(),
            test_db_with_project(&test_project_config()),
            None,
            None,
        );

        let mut session = Session::stub("test-session", &backend_arc, &provider);
        session.info.additional_dirs = vec![PathBuf::from("/repo2"), PathBuf::from("/repo3")];

        let sid = session.info.id;
        app.sessions.push(session);
        app.projects[0].session_ids.push(sid);

        let shared = app.session_to_shared(&app.sessions[0]);
        assert_eq!(shared.additional_dirs.len(), 2);
        assert_eq!(shared.additional_dirs[0], PathBuf::from("/repo2"));
        assert_eq!(shared.additional_dirs[1], PathBuf::from("/repo3"));
    }

    #[test]
    fn prepare_spawn_prefixes_admin_session_name() {
        let backend_arc = stub_backend_arc();
        let provider = stub_provider();
        let admin_config = ProjectConfig {
            name: "Admin".to_string(),
            repos: vec![PathBuf::from("/admin")],
            roles: vec![
                RoleConfig {
                    name: "role-a".to_string(),
                    description: String::new(),
                    permissions: RolePermissions::default(),
                },
                RoleConfig {
                    name: "role-b".to_string(),
                    description: String::new(),
                    permissions: RolePermissions::default(),
                },
            ],
            mcp_servers: vec![],
            id: None,
        };
        let mut app = App::new(
            24,
            120,
            BackendRegistry::new(backend_arc),
            provider,
            test_db_with_project(&admin_config),
            None,
            None,
        );
        app.projects[0].is_admin = true;

        app.prepare_spawn(SessionConfig::default(), Vec::new());

        // With 2+ roles the name is stored in pending_spawn_name
        let name = app.pending_spawn_name.as_deref().unwrap();
        assert!(
            name.starts_with("admin-"),
            "expected admin- prefix, got: {name}"
        );
    }

    #[test]
    fn cannot_edit_admin_project() {
        let mut app = App::new(
            24,
            120,
            stub_backend(),
            stub_provider(),
            test_db(),
            None,
            None,
        );

        // Add an admin project and select it
        let admin_project = ProjectInfo::new_admin(ProjectConfig {
            name: "Admin".to_string(),
            repos: vec![],
            roles: Vec::new(),
            mcp_servers: Vec::new(),
            id: None,
        });
        app.projects.push(admin_project);
        app.active_project_index = app.projects.len() - 1;

        app.open_edit_project_modal();
        assert!(!app.is_edit_project_open());
        assert_eq!(
            app.status_message.as_ref().map(|m| m.text.as_str()),
            Some("Cannot edit admin project")
        );
    }

    #[test]
    fn cannot_delete_admin_project() {
        let mut app = App::new(
            24,
            120,
            stub_backend(),
            stub_provider(),
            test_db(),
            None,
            None,
        );

        // Add an admin project and select it
        let admin_project = ProjectInfo::new_admin(ProjectConfig {
            name: "Admin".to_string(),
            repos: vec![],
            roles: Vec::new(),
            mcp_servers: Vec::new(),
            id: None,
        });
        app.projects.push(admin_project);
        app.active_project_index = app.projects.len() - 1;

        app.show_delete_project_modal();
        assert!(!matches!(app.modal, modals::Modal::DeleteProject(_)));
        assert_eq!(
            app.status_message.as_ref().map(|m| m.text.as_str()),
            Some("Cannot delete admin project")
        );
    }

    #[test]
    fn can_close_admin_session() {
        let backend_arc = stub_backend_arc();
        let provider = stub_provider();
        let mut app = App::new(
            24,
            120,
            BackendRegistry::new(backend_arc.clone()),
            provider.clone(),
            test_db(),
            None,
            None,
        );

        // Add an admin project with a session and select it
        let mut admin_project = ProjectInfo::new_admin(ProjectConfig {
            name: "Admin".to_string(),
            repos: vec![],
            roles: Vec::new(),
            mcp_servers: Vec::new(),
            id: None,
        });
        let session = Session::stub("admin-1", &backend_arc, &provider);
        let sid = session.info.id;
        app.sessions.push(session);
        admin_project.session_ids.push(sid);
        app.projects.push(admin_project);
        app.active_project_index = app.projects.len() - 1;
        app.active_index = 0;

        // Ctrl+D from session list closes admin session
        app.focus = InputFocus::SessionList;
        app.handle_key(KeyCode::Char('d'), KeyModifiers::CONTROL);
        assert_eq!(app.sessions.len(), 0); // Session closed
                                           // No error — status message is the "Deleted ... Ctrl+Z to undo" info
        assert_ne!(
            app.status_message.as_ref().map(|m| m.level),
            Some(StatusLevel::Error)
        );
    }

    // --- StatusMessage / set_error / set_status tests ---

    #[test]
    fn set_error_creates_error_status() {
        let mut app = App::new(
            24,
            80,
            stub_backend(),
            stub_provider(),
            test_db(),
            None,
            None,
        );
        app.set_error("something failed");
        let msg = app.status_message.as_ref().unwrap();
        assert_eq!(msg.level, StatusLevel::Error);
        assert_eq!(msg.text, "something failed");
    }

    #[test]
    fn set_status_creates_typed_status() {
        let mut app = App::new(
            24,
            80,
            stub_backend(),
            stub_provider(),
            test_db(),
            None,
            None,
        );
        app.set_status(StatusLevel::Success, "all good");
        let msg = app.status_message.as_ref().unwrap();
        assert_eq!(msg.level, StatusLevel::Success);
        assert_eq!(msg.text, "all good");
    }

    #[test]
    fn set_status_replaces_previous() {
        let mut app = App::new(
            24,
            80,
            stub_backend(),
            stub_provider(),
            test_db(),
            None,
            None,
        );
        app.set_error("old error");
        app.set_status(StatusLevel::Info, "new info");
        let msg = app.status_message.as_ref().unwrap();
        assert_eq!(msg.level, StatusLevel::Info);
        assert_eq!(msg.text, "new info");
    }

    // --- Worktree sync tests ---

    #[test]
    fn start_sync_with_no_active_project_shows_info() {
        let mut app = App::new(
            24,
            80,
            stub_backend(),
            stub_provider(),
            test_db(),
            None,
            None,
        );
        app.start_sync();
        assert!(!app.worktree_sync_in_progress);
        let msg = app.status_message.as_ref().unwrap();
        assert_eq!(msg.level, StatusLevel::Info);
        assert_eq!(msg.text, "No active project");
    }

    #[test]
    fn start_sync_ignores_if_already_in_progress() {
        let mut app = App::new(
            24,
            80,
            stub_backend(),
            stub_provider(),
            test_db(),
            None,
            None,
        );
        app.worktree_sync_in_progress = true;
        app.status_message = None;
        app.start_sync();
        // Should not set any new status message
        assert!(app.status_message.is_none());
    }

    #[test]
    fn ctrl_s_triggers_start_sync() {
        let mut app = App::new(
            24,
            80,
            stub_backend(),
            stub_provider(),
            test_db(),
            None,
            None,
        );
        app.handle_key(KeyCode::Char('s'), KeyModifiers::CONTROL);
        // No active project → info message
        let msg = app.status_message.as_ref().unwrap();
        assert_eq!(msg.text, "No active project");
    }

    #[test]
    fn start_sync_with_worktree_sessions_sets_in_progress() {
        let mut app = app_with_sessions(1);
        app.sessions[0].info.worktrees = vec![WorktreeInfo {
            repo_path: PathBuf::from("/tmp/nonexistent-repo"),
            worktree_path: PathBuf::from("/tmp/nonexistent-wt"),
            branch: "test-branch".to_string(),
        }];

        app.start_sync();
        assert!(app.worktree_sync_in_progress);
        assert_eq!(app.worktree_sync_pending, 1);
        let msg = app.status_message.as_ref().unwrap();
        assert_eq!(msg.level, StatusLevel::Info);
        assert!(msg.text.contains("Syncing 1 worktree"));
    }

    #[test]
    fn start_sync_with_no_worktrees_in_active_project_shows_info() {
        let mut app = app_with_sessions(1);
        // Session has no worktrees
        assert!(app.sessions[0].info.worktrees.is_empty());
        app.start_sync();
        assert!(!app.worktree_sync_in_progress);
        let msg = app.status_message.as_ref().unwrap();
        assert_eq!(msg.level, StatusLevel::Info);
        assert_eq!(msg.text, "No worktrees to sync in active project");
    }

    #[test]
    fn start_sync_ignores_sessions_outside_active_project() {
        let mut app = app_with_sessions(1);
        // Add a second session NOT in the active project, with a worktree
        let backend_arc = stub_backend_arc();
        let provider = stub_provider();
        let mut orphan = Session::stub("orphan-session", &backend_arc, &provider);
        orphan.info.worktrees = vec![WorktreeInfo {
            repo_path: PathBuf::from("/tmp/orphan-repo"),
            worktree_path: PathBuf::from("/tmp/orphan-wt"),
            branch: "orphan-branch".to_string(),
        }];
        app.sessions.push(orphan);
        // Do NOT add orphan to projects[0].session_ids

        app.start_sync();
        // The active project's session has no worktrees, so sync should not start
        assert!(!app.worktree_sync_in_progress);
        let msg = app.status_message.as_ref().unwrap();
        assert_eq!(msg.text, "No worktrees to sync in active project");
    }

    #[test]
    fn tick_increments_tick_count() {
        let mut app = App::new(
            24,
            80,
            stub_backend(),
            stub_provider(),
            test_db(),
            None,
            None,
        );
        assert_eq!(app.tick_count, 0);
        app.tick();
        assert_eq!(app.tick_count, 1);
        app.tick();
        assert_eq!(app.tick_count, 2);
    }

    #[test]
    fn finish_sync_all_synced_shows_success() {
        let mut app = App::new(
            24,
            80,
            stub_backend(),
            stub_provider(),
            test_db(),
            None,
            None,
        );
        let id = SessionId::default();
        app.worktree_sync_completed = vec![
            (id, git::SyncResult::Synced),
            (SessionId::default(), git::SyncResult::Synced),
        ];
        app.finish_sync();
        let msg = app.status_message.as_ref().unwrap();
        assert_eq!(msg.level, StatusLevel::Success);
        assert!(msg.text.contains("2 worktree(s) synced"));
    }

    #[test]
    fn finish_sync_with_errors_shows_error() {
        let mut app = App::new(
            24,
            80,
            stub_backend(),
            stub_provider(),
            test_db(),
            None,
            None,
        );
        app.worktree_sync_completed = vec![(
            SessionId::default(),
            git::SyncResult::Error("fetch failed".into()),
        )];
        app.finish_sync();
        let msg = app.status_message.as_ref().unwrap();
        assert_eq!(msg.level, StatusLevel::Error);
        assert!(msg.text.contains("Sync failed"));
        assert!(msg.text.contains("fetch failed"));
    }

    #[test]
    fn finish_sync_with_conflicts_shows_info() {
        let mut app = App::new(
            24,
            80,
            stub_backend(),
            stub_provider(),
            test_db(),
            None,
            None,
        );
        app.worktree_sync_completed = vec![
            (SessionId::default(), git::SyncResult::Synced),
            (
                SessionId::default(),
                git::SyncResult::Conflict("merge conflict".into()),
            ),
        ];
        app.finish_sync();
        let msg = app.status_message.as_ref().unwrap();
        assert_eq!(msg.level, StatusLevel::Info);
        assert!(msg.text.contains("1 synced"));
        assert!(msg.text.contains("1 conflict"));
    }

    #[test]
    fn finish_sync_errors_take_priority_over_conflicts() {
        let mut app = App::new(
            24,
            80,
            stub_backend(),
            stub_provider(),
            test_db(),
            None,
            None,
        );
        app.worktree_sync_completed = vec![
            (
                SessionId::default(),
                git::SyncResult::Conflict("merge conflict".into()),
            ),
            (
                SessionId::default(),
                git::SyncResult::Error("network error".into()),
            ),
        ];
        app.finish_sync();
        let msg = app.status_message.as_ref().unwrap();
        assert_eq!(msg.level, StatusLevel::Error);
        assert!(msg.text.contains("network error"));
    }

    #[test]
    fn drain_deferred_inputs_sends_at_correct_tick() {
        let mut app = App::new(
            24,
            80,
            stub_backend(),
            stub_provider(),
            test_db(),
            None,
            None,
        );
        let id = SessionId::default();
        app.deferred_inputs.push((id, b"hello".to_vec(), 5));

        // Before target tick: nothing drained
        app.tick_count = 4;
        app.drain_deferred_inputs();
        assert_eq!(app.deferred_inputs.len(), 1);

        // At target tick: drained (no matching session, but entry is removed)
        app.tick_count = 5;
        app.drain_deferred_inputs();
        assert!(app.deferred_inputs.is_empty());
    }

    #[test]
    fn drain_deferred_inputs_retains_future_items() {
        let mut app = App::new(
            24,
            80,
            stub_backend(),
            stub_provider(),
            test_db(),
            None,
            None,
        );
        let id = SessionId::default();
        app.deferred_inputs.push((id, b"early".to_vec(), 5));
        app.deferred_inputs.push((id, b"late".to_vec(), 20));

        app.tick_count = 5;
        app.drain_deferred_inputs();
        assert_eq!(app.deferred_inputs.len(), 1);
        assert_eq!(app.deferred_inputs[0].2, 20);
    }

    #[test]
    fn send_conflict_prompt_noop_for_unknown_session() {
        let mut app = App::new(
            24,
            80,
            stub_backend(),
            stub_provider(),
            test_db(),
            None,
            None,
        );
        app.send_conflict_prompt(SessionId::default());
        assert!(app.deferred_inputs.is_empty());
    }

    #[test]
    fn send_conflict_prompt_no_deferred_when_send_fails() {
        let mut app = app_with_sessions(1);
        let sid = app.sessions[0].info.id;

        // Stub's channel rx is dropped, so send_input fails.
        // No deferred input should be created.
        app.send_conflict_prompt(sid);
        assert!(app.deferred_inputs.is_empty());
    }

    #[test]
    fn poll_sync_results_triggers_finish_when_all_received() {
        let mut app = App::new(
            24,
            80,
            stub_backend(),
            stub_provider(),
            test_db(),
            None,
            None,
        );
        let (tx, rx) = mpsc::channel();
        let id = SessionId::default();

        tx.send((id, git::SyncResult::Synced)).unwrap();
        drop(tx);

        app.worktree_sync_in_progress = true;
        app.worktree_sync_rx = Some(rx);
        app.worktree_sync_pending = 1;

        app.poll_sync_results();

        assert!(!app.worktree_sync_in_progress);
        assert!(app.worktree_sync_rx.is_none());
        let msg = app.status_message.as_ref().unwrap();
        assert_eq!(msg.level, StatusLevel::Success);
    }

    #[test]
    fn poll_sync_results_waits_for_all_pending() {
        let mut app = App::new(
            24,
            80,
            stub_backend(),
            stub_provider(),
            test_db(),
            None,
            None,
        );
        let (tx, rx) = mpsc::channel();

        tx.send((SessionId::default(), git::SyncResult::Synced))
            .unwrap();
        // Don't drop tx — second result hasn't arrived yet

        app.worktree_sync_in_progress = true;
        app.worktree_sync_rx = Some(rx);
        app.worktree_sync_pending = 2;

        app.poll_sync_results();

        // Still in progress — only 1 of 2 received
        assert!(app.worktree_sync_in_progress);
        assert!(app.worktree_sync_rx.is_some());
        assert_eq!(app.worktree_sync_completed.len(), 1);
    }

    // --- find_project_index_for_session tests ---

    #[test]
    fn find_project_index_finds_matching_project() {
        let backend = stub_backend();
        let provider = stub_provider();
        let config_b = ProjectConfig {
            name: "Other".to_string(),
            repos: vec![PathBuf::from("/other")],
            ..test_project_config()
        };
        let mut app = App::new(24, 120, backend, provider, test_db(), None, None);
        app.projects.push(ProjectInfo::new(test_project_config()));
        let project_b = ProjectInfo::new(config_b);
        let id_b = project_b.id;
        app.projects.push(project_b);
        app.active_project_index = 0;

        let index = app.find_project_index_for_session(SessionId::default(), &id_b);
        assert_eq!(index, 1);
    }

    #[test]
    fn find_project_index_falls_back_to_active_project() {
        let backend = stub_backend();
        let provider = stub_provider();
        let mut app = App::new(24, 120, backend, provider, test_db(), None, None);
        app.projects.push(ProjectInfo::new(test_project_config()));
        app.active_project_index = 0;

        let index = app.find_project_index_for_session(SessionId::default(), &ProjectId::default());
        assert_eq!(index, 0);
    }

    // --- resolve_role_permissions_for_project tests ---

    #[test]
    fn resolve_role_permissions_for_specific_project() {
        use crate::session::{RoleConfig, RolePermissions};
        let backend = stub_backend();
        let provider = stub_provider();
        let config_with_roles = ProjectConfig {
            roles: vec![RoleConfig {
                name: "reviewer".to_string(),
                description: String::new(),
                permissions: RolePermissions {
                    permission_mode: Some("plan".to_string()),
                    ..RolePermissions::default()
                },
            }],
            ..test_project_config()
        };
        let mut app = App::new(24, 120, backend, provider, test_db(), None, None);
        app.projects.push(ProjectInfo::new(test_project_config()));
        app.projects.push(ProjectInfo::new(config_with_roles));
        app.active_project_index = 0;

        // Resolve from project at index 1 (not the active project)
        let perms = app.resolve_role_permissions_for_project("reviewer", 1);
        assert_eq!(perms.permission_mode, Some("plan".to_string()));

        // Resolve from project at index 0 — role doesn't exist there
        let perms = app.resolve_role_permissions_for_project("reviewer", 0);
        assert_eq!(perms, RolePermissions::default());
    }

    #[test]
    fn resolve_role_permissions_returns_default_for_missing_role() {
        use crate::session::RolePermissions;
        let backend = stub_backend();
        let provider = stub_provider();
        let mut app = App::new(24, 120, backend, provider, test_db(), None, None);
        app.projects.push(ProjectInfo::new(test_project_config()));
        app.active_project_index = 0;

        let perms = app.resolve_role_permissions_for_project("nonexistent", 0);
        assert_eq!(perms, RolePermissions::default());
    }

    #[test]
    fn resolve_role_permissions_returns_default_for_invalid_index() {
        use crate::session::RolePermissions;
        let backend = stub_backend();
        let provider = stub_provider();
        let app = App::new(24, 120, backend, provider, test_db(), None, None);

        let perms = app.resolve_role_permissions_for_project("any-role", 999);
        assert_eq!(perms, RolePermissions::default());
    }

    #[test]
    fn admin_mcp_permissions_contains_all_tools() {
        let perms = super::admin_mcp_permissions();
        assert_eq!(perms.allowed_tools.len(), 30);
        assert!(perms
            .allowed_tools
            .iter()
            .all(|t| t.starts_with("mcp__thurbox__")));
        assert!(perms.permission_mode.is_none());
        assert!(perms.disallowed_tools.is_empty());
        assert!(perms.append_system_prompt.is_some());
    }

    #[test]
    fn resolve_role_permissions_returns_admin_tools_for_admin_project() {
        let backend = stub_backend();
        let mut app = App::new(24, 120, backend, stub_provider(), test_db(), None, None);
        let admin_project = ProjectInfo::new_admin(ProjectConfig {
            name: "Admin".to_string(),
            repos: vec![],
            roles: vec![],
            mcp_servers: vec![],
            id: None,
        });
        app.projects.push(admin_project);

        let perms = app.resolve_role_permissions_for_project("developer", 0);
        assert_eq!(perms, super::admin_mcp_permissions());
    }

    // --- format_time_ago tests ---

    #[test]
    fn format_time_ago_seconds() {
        let now = crate::sync::current_time_millis();
        assert_eq!(super::view::format_time_ago(now - 5_000), "5s ago");
        assert_eq!(super::view::format_time_ago(now - 30_000), "30s ago");
    }

    #[test]
    fn format_time_ago_minutes() {
        let now = crate::sync::current_time_millis();
        assert_eq!(super::view::format_time_ago(now - 120_000), "2m ago");
        assert_eq!(super::view::format_time_ago(now - 3_540_000), "59m ago");
    }

    #[test]
    fn format_time_ago_hours() {
        let now = crate::sync::current_time_millis();
        assert_eq!(super::view::format_time_ago(now - 3_600_000), "1h ago");
        assert_eq!(super::view::format_time_ago(now - 7_200_000), "2h ago");
    }

    #[test]
    fn format_time_ago_days() {
        let now = crate::sync::current_time_millis();
        assert_eq!(super::view::format_time_ago(now - 86_400_000), "1d ago");
        assert_eq!(super::view::format_time_ago(now - 259_200_000), "3d ago");
    }

    #[test]
    fn format_time_ago_future_timestamp() {
        let now = crate::sync::current_time_millis();
        // Future timestamp should saturate to 0s
        assert_eq!(super::view::format_time_ago(now + 10_000), "0s ago");
    }

    // --- parse_agent_metrics tests ---

    #[test]
    fn parse_agent_metrics_full_json() {
        let json = serde_json::json!({
            "version": "2.1.58",
            "model": { "id": "claude-opus-4-6", "display_name": "Opus 4.6" },
            "cost": {
                "total_cost_usd": 0.0123,
                "total_duration_ms": 5000,
                "total_api_duration_ms": 3000,
                "total_lines_added": 156,
                "total_lines_removed": 23,
            },
            "context_window": {
                "total_input_tokens": 15200,
                "total_output_tokens": 4500,
                "context_window_size": 200000,
                "used_percentage": 8,
                "current_usage": {
                    "input_tokens": 1200,
                    "output_tokens": 300,
                    "cache_creation_input_tokens": 5000,
                    "cache_read_input_tokens": 2000,
                }
            }
        });
        let m = super::parse_agent_metrics(&json);
        assert_eq!(m.model_id.as_deref(), Some("claude-opus-4-6"));
        assert_eq!(m.model_display_name.as_deref(), Some("Opus 4.6"));
        assert!((m.total_cost_usd.unwrap() - 0.0123).abs() < 1e-6);
        assert_eq!(m.total_input_tokens, Some(15200));
        assert_eq!(m.total_output_tokens, Some(4500));
        assert_eq!(m.context_window_size, Some(200000));
        assert_eq!(m.used_percentage, Some(8));
        assert_eq!(m.total_lines_added, Some(156));
        assert_eq!(m.total_lines_removed, Some(23));
        assert_eq!(m.cache_read_input_tokens, Some(2000));
        assert_eq!(m.cache_creation_input_tokens, Some(5000));
        assert_eq!(m.cli_version.as_deref(), Some("2.1.58"));
    }

    #[test]
    fn parse_agent_metrics_empty_json() {
        let json = serde_json::json!({});
        let m = super::parse_agent_metrics(&json);
        assert!(m.model_id.is_none());
        assert!(m.total_cost_usd.is_none());
        assert!(m.used_percentage.is_none());
    }

    #[test]
    fn parse_agent_metrics_partial_json() {
        let json = serde_json::json!({
            "model": { "display_name": "Sonnet" },
            "cost": { "total_cost_usd": 0.05 }
        });
        let m = super::parse_agent_metrics(&json);
        assert_eq!(m.model_display_name.as_deref(), Some("Sonnet"));
        assert!(m.model_id.is_none());
        assert!((m.total_cost_usd.unwrap() - 0.05).abs() < 1e-6);
        assert!(m.total_input_tokens.is_none());
    }

    // --- find_matching_discovered tests ---

    fn make_shared_session(backend_id: &str, name: &str) -> sync::SharedSession {
        sync::SharedSession {
            id: crate::session::SessionId::default(),
            name: name.to_string(),
            project_id: crate::project::ProjectId::from_uuid(uuid::Uuid::nil()),
            role: String::new(),
            backend_id: backend_id.to_string(),
            backend_type: "tmux".to_string(),
            agent_session_id: Some("agent-123".to_string()),
            cwd: None,
            additional_dirs: Vec::new(),
            worktrees: Vec::new(),
            shell_backend_id: None,
            tombstone: false,
            tombstone_at: None,
        }
    }

    fn make_discovered(
        backend_id: &str,
        name: &str,
        is_alive: bool,
    ) -> crate::agent::backend::DiscoveredSession {
        crate::agent::backend::DiscoveredSession {
            backend_id: backend_id.to_string(),
            name: name.to_string(),
            is_alive,
        }
    }

    #[test]
    fn find_matching_discovered_by_backend_id() {
        let shared = make_shared_session("thurbox:@0", "1");
        let discovered = vec![
            make_discovered("thurbox:@0", "tb-1", true),
            make_discovered("thurbox:@1", "tb-2", true),
        ];
        let result = App::find_matching_discovered(&shared, &discovered);
        assert!(result.is_some());
        assert_eq!(result.unwrap().backend_id, "thurbox:@0");
    }

    #[test]
    fn find_matching_discovered_by_name_fallback() {
        let shared = make_shared_session("", "1");
        let discovered = vec![
            make_discovered("thurbox:@5", "tb-1", true),
            make_discovered("thurbox:@6", "tb-2", true),
        ];
        let result = App::find_matching_discovered(&shared, &discovered);
        assert!(result.is_some());
        assert_eq!(result.unwrap().backend_id, "thurbox:@5");
    }

    #[test]
    fn find_matching_discovered_skips_dead() {
        let shared = make_shared_session("thurbox:@0", "1");
        let discovered = vec![make_discovered("thurbox:@0", "tb-1", false)];
        let result = App::find_matching_discovered(&shared, &discovered);
        assert!(result.is_none());
    }

    #[test]
    fn find_matching_discovered_no_match() {
        let shared = make_shared_session("thurbox:@99", "99");
        let discovered = vec![make_discovered("thurbox:@0", "tb-1", true)];
        let result = App::find_matching_discovered(&shared, &discovered);
        assert!(result.is_none());
    }

    #[test]
    fn find_matching_discovered_empty_list() {
        let shared = make_shared_session("thurbox:@0", "1");
        let result = App::find_matching_discovered(&shared, &[]);
        assert!(result.is_none());
    }

    // =========================================================================
    // Phase 5: State transition tests
    // =========================================================================

    /// Create an App with a single project "test-project" and no sessions.
    /// Convenience wrapper used by Phase 5 tests.
    fn make_test_app() -> App {
        app_with_project("test-project", vec![PathBuf::from("/tmp/test-repo")])
    }

    // --- 5a. Modal flow tests ---

    #[test]
    fn f1_opens_help() {
        let mut app = make_test_app();
        assert!(!matches!(app.modal, modals::Modal::Help));
        app.handle_key(KeyCode::F(1), KeyModifiers::NONE);
        assert!(matches!(app.modal, modals::Modal::Help));
    }

    #[test]
    fn esc_closes_help() {
        let mut app = make_test_app();
        app.modal = modals::Modal::Help;
        app.handle_key(KeyCode::Esc, KeyModifiers::NONE);
        assert!(!matches!(app.modal, modals::Modal::Help));
    }

    #[test]
    fn ctrl_n_project_list_opens_add_project() {
        let mut app = make_test_app();
        app.focus = InputFocus::ProjectList;
        app.handle_key(KeyCode::Char('n'), KeyModifiers::CONTROL);
        assert!(app.is_add_project_open());
    }

    #[test]
    fn esc_closes_add_project_modal() {
        let mut app = make_test_app();
        app.modal = modals::Modal::AddProject(modals::AddProjectModal::default());
        app.handle_key(KeyCode::Esc, KeyModifiers::NONE);
        assert!(!app.is_add_project_open());
    }

    #[test]
    fn opening_new_modal_after_closing_previous() {
        let mut app = make_test_app();
        // Open help first
        app.modal = modals::Modal::Help;
        // Close help
        app.handle_key(KeyCode::Esc, KeyModifiers::NONE);
        assert!(!matches!(app.modal, modals::Modal::Help));
        // Now open add project
        app.focus = InputFocus::ProjectList;
        app.handle_key(KeyCode::Char('n'), KeyModifiers::CONTROL);
        assert!(app.is_add_project_open());
    }

    #[test]
    fn ctrl_d_project_list_opens_delete_modal() {
        let mut app = make_test_app();
        // Need at least 2 projects (can't delete the only project)
        app.projects.push(ProjectInfo {
            id: ProjectId::default(),
            config: ProjectConfig {
                name: "Extra".into(),
                repos: vec![PathBuf::from("/tmp/extra")],
                roles: vec![],
                mcp_servers: vec![],
                id: None,
            },
            session_ids: vec![],
            is_admin: false,
        });
        app.active_project_index = 1;
        app.focus = InputFocus::ProjectList;
        app.handle_key(KeyCode::Char('d'), KeyModifiers::CONTROL);
        assert!(matches!(app.modal, modals::Modal::DeleteProject(_)));
    }

    #[test]
    fn ctrl_e_opens_edit_project_modal() {
        let mut app = make_test_app();
        app.handle_key(KeyCode::Char('e'), KeyModifiers::CONTROL);
        assert!(app.is_edit_project_open());
    }

    #[test]
    fn help_modal_blocks_other_keys() {
        let mut app = make_test_app();
        app.modal = modals::Modal::Help;
        let focus_before = app.focus;
        // Ctrl+H should not change focus while help is open
        app.handle_key(KeyCode::Char('h'), KeyModifiers::CONTROL);
        assert_eq!(app.focus, focus_before);
        assert!(matches!(app.modal, modals::Modal::Help));
    }

    #[test]
    fn esc_closes_delete_project_modal() {
        let mut app = make_test_app();
        app.projects.push(ProjectInfo {
            id: ProjectId::default(),
            config: ProjectConfig {
                name: "Extra".into(),
                repos: vec![PathBuf::from("/tmp/extra")],
                roles: vec![],
                mcp_servers: vec![],
                id: None,
            },
            session_ids: vec![],
            is_admin: false,
        });
        app.active_project_index = 1;
        app.focus = InputFocus::ProjectList;
        app.handle_key(KeyCode::Char('d'), KeyModifiers::CONTROL);
        assert!(matches!(app.modal, modals::Modal::DeleteProject(_)));
        app.handle_key(KeyCode::Esc, KeyModifiers::NONE);
        assert!(!matches!(app.modal, modals::Modal::DeleteProject(_)));
    }

    #[test]
    fn ctrl_u_opens_restore_sessions_modal() {
        let mut app = make_test_app();
        app.handle_key(KeyCode::Char('u'), KeyModifiers::CONTROL);
        // The modal opens only if there are deleted sessions;
        // with no deleted sessions the DB query returns empty list,
        // so the modal opens with an empty list.
        assert!(matches!(app.modal, modals::Modal::RestoreSessions(_)));
    }

    #[test]
    fn esc_closes_restore_sessions_modal() {
        let mut app = make_test_app();
        app.modal = modals::Modal::RestoreSessions(modals::RestoreSessionsModal::default());
        app.handle_key(KeyCode::Esc, KeyModifiers::NONE);
        assert!(!matches!(app.modal, modals::Modal::RestoreSessions(_)));
    }

    #[test]
    fn add_project_modal_blocks_global_keys() {
        let mut app = make_test_app();
        app.modal = modals::Modal::AddProject(modals::AddProjectModal::default());
        let focus_before = app.focus;
        // Ctrl+Q should NOT quit while add-project modal is open
        app.handle_key(KeyCode::Char('q'), KeyModifiers::CONTROL);
        assert!(!app.should_quit);
        assert_eq!(app.focus, focus_before);
    }

    // --- 5b. Focus management tests ---

    #[test]
    fn enter_in_project_list_focuses_session_list() {
        let mut app = make_test_app();
        app.focus = InputFocus::ProjectList;
        app.handle_key(KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(app.focus, InputFocus::SessionList);
    }

    #[test]
    fn enter_in_session_list_focuses_terminal() {
        let mut app = make_test_app();
        app.focus = InputFocus::SessionList;
        app.handle_key(KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(app.focus, InputFocus::Terminal);
    }

    #[test]
    fn ctrl_q_sets_should_quit() {
        let mut app = make_test_app();
        app.handle_key(KeyCode::Char('q'), KeyModifiers::CONTROL);
        assert!(app.should_quit);
    }

    #[test]
    fn ctrl_l_cycles_focus_back_to_project_list() {
        let mut app = app_with_sessions(1);
        app.focus = InputFocus::Terminal;
        app.handle_key(KeyCode::Char('l'), KeyModifiers::CONTROL);
        assert_eq!(app.focus, InputFocus::ProjectList);
    }

    #[test]
    fn ctrl_h_always_focuses_project_list() {
        let mut app = make_test_app();
        for initial_focus in [InputFocus::SessionList, InputFocus::Terminal] {
            app.focus = initial_focus;
            app.handle_key(KeyCode::Char('h'), KeyModifiers::CONTROL);
            assert_eq!(
                app.focus,
                InputFocus::ProjectList,
                "Ctrl+H should focus project list from {initial_focus:?}"
            );
        }
    }

    // --- 5c. Tick behavior tests ---

    #[test]
    fn status_message_persists_within_timeout() {
        let mut app = make_test_app();
        app.set_info("test message");
        assert!(app.status_message.is_some());
        app.tick();
        // Should still be there (just created, no auto-expire in tick)
        assert!(app.status_message.is_some());
    }

    #[test]
    fn multiple_ticks_accumulate() {
        let mut app = make_test_app();
        let initial = app.tick_count;
        app.tick();
        app.tick();
        app.tick();
        assert_eq!(app.tick_count, initial + 3);
    }

    #[test]
    fn tick_count_starts_at_zero() {
        let app = make_test_app();
        assert_eq!(app.tick_count, 0);
    }

    #[test]
    fn tick_wraps_on_overflow() {
        let mut app = make_test_app();
        app.tick_count = u64::MAX;
        app.tick();
        // wrapping_add(1) from MAX wraps to 0
        assert_eq!(app.tick_count, 0);
    }

    #[test]
    fn set_info_replaces_previous_status() {
        let mut app = make_test_app();
        app.set_info("first");
        app.set_info("second");
        let msg = app.status_message.as_ref().unwrap();
        assert_eq!(msg.text, "second");
        assert_eq!(msg.level, StatusLevel::Info);
    }

    // --- 5d. External sync delta tests ---

    #[test]
    fn external_project_added_appears_in_list() {
        let mut app = make_test_app();
        let initial_count = app.projects.len();
        let delta = StateDelta {
            added_projects: vec![sync::SharedProject {
                id: ProjectId::from_uuid(uuid::Uuid::new_v4()),
                name: "external-project".to_string(),
                repos: vec![PathBuf::from("/tmp/ext")],
                roles: vec![],
                mcp_servers: vec![],
            }],
            ..StateDelta::default()
        };
        app.update(AppMessage::ExternalStateChange(delta));
        assert_eq!(app.projects.len(), initial_count + 1);
        assert!(app
            .projects
            .iter()
            .any(|p| p.config.name == "external-project"));
    }

    #[test]
    fn external_project_removed_disappears() {
        let mut app = make_test_app();
        let project_id = app.projects[0].id;
        let delta = StateDelta {
            removed_projects: vec![project_id],
            ..StateDelta::default()
        };
        app.update(AppMessage::ExternalStateChange(delta));
        assert!(!app.projects.iter().any(|p| p.id == project_id));
    }

    #[test]
    fn external_project_update_modifies_name() {
        let mut app = make_test_app();
        let project_id = app.projects[0].id;
        let delta = StateDelta {
            updated_projects: vec![sync::SharedProject {
                id: project_id,
                name: "renamed-project".to_string(),
                repos: vec![PathBuf::from("/tmp/repo")],
                roles: vec![],
                mcp_servers: vec![],
            }],
            ..StateDelta::default()
        };
        app.update(AppMessage::ExternalStateChange(delta));
        assert_eq!(
            app.projects
                .iter()
                .find(|p| p.id == project_id)
                .unwrap()
                .config
                .name,
            "renamed-project"
        );
    }

    #[test]
    fn active_project_index_adjusts_on_removal() {
        let mut app = make_test_app();
        // Add a second project
        let config2 = ProjectConfig {
            name: "second".to_string(),
            repos: vec![PathBuf::from("/tmp/second")],
            roles: vec![],
            mcp_servers: vec![],
            id: None,
        };
        let info2 = ProjectInfo::new(config2);
        app.projects.push(info2);
        app.active_project_index = 1; // Select second project

        // Remove first project
        let first_id = app.projects[0].id;
        let delta = StateDelta {
            removed_projects: vec![first_id],
            ..StateDelta::default()
        };
        app.update(AppMessage::ExternalStateChange(delta));

        // Active index should be valid
        assert!(app.active_project_index < app.projects.len());
    }

    #[test]
    fn duplicate_external_project_add_ignored() {
        let mut app = make_test_app();
        let project_id = app.projects[0].id;
        let initial_count = app.projects.len();
        let delta = StateDelta {
            added_projects: vec![sync::SharedProject {
                id: project_id, // Same ID as existing
                name: "duplicate".to_string(),
                repos: vec![],
                roles: vec![],
                mcp_servers: vec![],
            }],
            ..StateDelta::default()
        };
        app.update(AppMessage::ExternalStateChange(delta));
        assert_eq!(app.projects.len(), initial_count); // No duplicates
    }

    #[test]
    fn external_project_removal_with_empty_list_stays_at_zero() {
        let mut app = make_test_app();
        let project_id = app.projects[0].id;
        app.active_project_index = 0;
        let delta = StateDelta {
            removed_projects: vec![project_id],
            ..StateDelta::default()
        };
        app.update(AppMessage::ExternalStateChange(delta));
        // With 0 projects, active_project_index should be 0 (saturating_sub(1))
        assert_eq!(app.active_project_index, 0);
    }

    #[test]
    fn external_update_preserves_project_count() {
        let mut app = make_test_app();
        let initial_count = app.projects.len();
        let project_id = app.projects[0].id;
        let delta = StateDelta {
            updated_projects: vec![sync::SharedProject {
                id: project_id,
                name: "updated-name".to_string(),
                repos: vec![PathBuf::from("/tmp/new-repo")],
                roles: vec![],
                mcp_servers: vec![],
            }],
            ..StateDelta::default()
        };
        app.update(AppMessage::ExternalStateChange(delta));
        assert_eq!(app.projects.len(), initial_count);
    }

    #[test]
    fn external_update_for_nonexistent_project_is_noop() {
        let mut app = make_test_app();
        let initial_count = app.projects.len();
        let fake_id = ProjectId::from_uuid(uuid::Uuid::new_v4());
        let delta = StateDelta {
            updated_projects: vec![sync::SharedProject {
                id: fake_id,
                name: "ghost".to_string(),
                repos: vec![],
                roles: vec![],
                mcp_servers: vec![],
            }],
            ..StateDelta::default()
        };
        app.update(AppMessage::ExternalStateChange(delta));
        // No project was added or removed
        assert_eq!(app.projects.len(), initial_count);
    }

    #[test]
    fn external_removal_of_nonexistent_project_is_noop() {
        let mut app = make_test_app();
        let initial_count = app.projects.len();
        let fake_id = ProjectId::from_uuid(uuid::Uuid::new_v4());
        let delta = StateDelta {
            removed_projects: vec![fake_id],
            ..StateDelta::default()
        };
        app.update(AppMessage::ExternalStateChange(delta));
        assert_eq!(app.projects.len(), initial_count);
    }

    #[test]
    fn external_delta_counter_increment_merges_with_max() {
        let mut app = make_test_app();
        app.session_counter = 5;
        let delta = StateDelta {
            counter_increment: 10,
            ..StateDelta::default()
        };
        app.update(AppMessage::ExternalStateChange(delta));
        assert_eq!(app.session_counter, 10);

        // If local is higher, it stays
        let delta2 = StateDelta {
            counter_increment: 3,
            ..StateDelta::default()
        };
        app.update(AppMessage::ExternalStateChange(delta2));
        assert_eq!(app.session_counter, 10);
    }
}
