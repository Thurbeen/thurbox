mod key_handlers;
pub(crate) mod mcp_editor_modal;
mod modals;
mod state;

use std::path::PathBuf;
use std::sync::{mpsc, Arc};

use crossterm::event::{KeyCode, KeyModifiers};
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
    Frame,
};
use tracing::error;

use crate::claude::{Session, SessionBackend};
use crate::git;
use crate::project::{ProjectConfig, ProjectId, ProjectInfo};
use crate::session::{
    RoleConfig, RolePermissions, SessionConfig, SessionId, SessionInfo, SessionStatus,
    WorktreeInfo, DEFAULT_ROLE_NAME,
};
use crate::storage::Database;
use crate::sync::{self, StateDelta, SyncState};
use crate::ui::{
    add_project_modal, branch_selector_modal, delete_project_modal, edit_project_modal, info_panel,
    layout, project_list, repo_selector_modal, role_editor_modal, role_selector_modal,
    session_mode_modal, status_bar, terminal_view, worktree_name_modal,
};

const MOUSE_SCROLL_LINES: usize = 3;

/// If no output for this many milliseconds, consider session "Waiting".
const ACTIVITY_TIMEOUT_MS: u64 = 1000;

/// Prompt sent to Claude sessions when a worktree rebase has conflicts.
const SYNC_CONFLICT_PROMPT: &str = "Please sync this worktree with main. Run: git fetch origin && git rebase origin/main -- if there are conflicts, resolve them and continue the rebase with git rebase --continue.";

/// Tick delay before sending Enter after pasting text into a session.
/// At ~10ms per tick, 10 ticks ≈ 100ms — enough for the app to process the pasted text.
const DEFERRED_INPUT_DELAY_TICKS: u64 = 10;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoleEditorView {
    List,
    Editor,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AddProjectField {
    Name,
    Path,
    RepoList,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditProjectField {
    Name,
    Path,
    RepoList,
    Roles,
    McpServers,
}

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
    Resize(u16, u16),
    ExternalStateChange(StateDelta),
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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputFocus {
    ProjectList,
    SessionList,
    Terminal,
}

pub struct App {
    pub(crate) projects: Vec<ProjectInfo>,
    pub(crate) active_project_index: usize,
    pub(crate) sessions: Vec<Session>,
    pub(crate) active_index: usize,
    backend: Arc<dyn SessionBackend>,
    pub(crate) db: Database,
    pub(crate) focus: InputFocus,
    pub(crate) should_quit: bool,
    pub(crate) status_message: Option<StatusMessage>,
    terminal_rows: u16,
    pub(crate) terminal_cols: u16,
    session_counter: usize,
    pub(crate) show_info_panel: bool,
    pub(crate) show_help: bool,
    pub(crate) show_add_project_modal: bool,
    pub(crate) add_project_name: TextInput,
    pub(crate) add_project_path: TextInput,
    pub(crate) add_project_field: AddProjectField,
    pub(crate) add_project_repos: Vec<PathBuf>,
    pub(crate) add_project_repo_index: usize,
    pub(crate) add_project_path_suggestion: Option<String>,
    pub(crate) show_edit_project_modal: bool,
    pub(crate) edit_project_name: TextInput,
    pub(crate) edit_project_path: TextInput,
    pub(crate) edit_project_field: EditProjectField,
    pub(crate) edit_project_repos: Vec<PathBuf>,
    pub(crate) edit_project_repo_index: usize,
    pub(crate) edit_project_path_suggestion: Option<String>,
    pub(crate) edit_project_original_id: Option<ProjectId>,
    pub(crate) show_delete_project_modal_flag: bool,
    pub(crate) delete_project_name: String,
    pub(crate) delete_project_confirmation: TextInput,
    pub(crate) delete_project_error: Option<String>,
    pub(crate) show_repo_selector: bool,
    pub(crate) repo_selector_index: usize,
    pub(crate) show_session_mode_modal: bool,
    pub(crate) session_mode_index: usize,
    pub(crate) show_branch_selector: bool,
    pub(crate) branch_selector_index: usize,
    pub(crate) available_branches: Vec<String>,
    pub(crate) pending_repo_path: Option<PathBuf>,
    pub(crate) show_worktree_name_modal: bool,
    pub(crate) worktree_name_input: TextInput,
    pub(crate) pending_base_branch: Option<String>,
    pub(crate) show_role_selector: bool,
    pub(crate) role_selector_index: usize,
    pub(crate) pending_spawn_config: Option<SessionConfig>,
    pub(crate) pending_spawn_worktree: Option<WorktreeInfo>,
    pub(crate) pending_spawn_name: Option<String>,
    pub(crate) show_role_editor: bool,
    pub(crate) role_editor_view: RoleEditorView,
    pub(crate) role_editor_list_index: usize,
    pub(crate) role_editor_roles: Vec<RoleConfig>,
    pub(crate) role_editor_field: role_editor_modal::RoleEditorField,
    pub(crate) role_editor_name: TextInput,
    pub(crate) role_editor_description: TextInput,
    pub(crate) role_editor_allowed_tools: ToolListState,
    pub(crate) role_editor_disallowed_tools: ToolListState,
    pub(crate) role_editor_system_prompt: TextInput,
    pub(crate) role_editor_editing_index: Option<usize>,
    pub(crate) edit_project_mcp_servers: Vec<crate::session::McpServerConfig>,
    pub(crate) edit_project_mcp_server_index: usize,
    pub(crate) show_mcp_editor: bool,
    pub(crate) mcp_editor_field: mcp_editor_modal::McpEditorField,
    pub(crate) mcp_editor_name: TextInput,
    pub(crate) mcp_editor_command: TextInput,
    pub(crate) mcp_editor_args: ToolListState,
    pub(crate) mcp_editor_env: ToolListState,
    pub(crate) mcp_editor_editing_index: Option<usize>,
    /// Inter-instance DB sync (polls for changes from other thurbox instances).
    sync_state: SyncState,
    /// Worktree-to-main git sync (Ctrl+S).
    worktree_sync_in_progress: bool,
    worktree_sync_rx: Option<mpsc::Receiver<(SessionId, git::SyncResult)>>,
    worktree_sync_pending: usize,
    worktree_sync_completed: Vec<(SessionId, git::SyncResult)>,
    tick_count: u64,
    /// Deferred inputs: `(session_id, data, tick_at_which_to_send)`.
    /// Used to introduce a small delay between pasting text and pressing Enter.
    deferred_inputs: Vec<(SessionId, Vec<u8>, u64)>,
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
/// Returns an empty vec if the database has no projects.
fn load_projects_from_db(db: &Database) -> Vec<ProjectInfo> {
    db.list_active_projects()
        .unwrap_or_default()
        .into_iter()
        .map(shared_project_to_info)
        .collect()
}

impl App {
    pub fn new(rows: u16, cols: u16, backend: Arc<dyn SessionBackend>, db: Database) -> Self {
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
            backend,
            db,
            focus: InputFocus::ProjectList,
            should_quit: false,
            status_message: None,
            terminal_rows: rows,
            terminal_cols: cols,
            session_counter,
            show_info_panel: false,
            show_help: false,
            show_add_project_modal: false,
            add_project_name: TextInput::new(),
            add_project_path: TextInput::new(),
            add_project_field: AddProjectField::Name,
            add_project_repos: Vec::new(),
            add_project_repo_index: 0,
            add_project_path_suggestion: None,
            show_edit_project_modal: false,
            edit_project_name: TextInput::new(),
            edit_project_path: TextInput::new(),
            edit_project_field: EditProjectField::Name,
            edit_project_repos: Vec::new(),
            edit_project_repo_index: 0,
            edit_project_path_suggestion: None,
            edit_project_original_id: None,
            show_delete_project_modal_flag: false,
            delete_project_name: String::new(),
            delete_project_confirmation: TextInput::new(),
            delete_project_error: None,
            show_repo_selector: false,
            repo_selector_index: 0,
            show_session_mode_modal: false,
            session_mode_index: 0,
            show_branch_selector: false,
            branch_selector_index: 0,
            available_branches: Vec::new(),
            pending_repo_path: None,
            show_worktree_name_modal: false,
            worktree_name_input: TextInput::new(),
            pending_base_branch: None,
            show_role_selector: false,
            role_selector_index: 0,
            pending_spawn_config: None,
            pending_spawn_worktree: None,
            pending_spawn_name: None,
            show_role_editor: false,
            role_editor_view: RoleEditorView::List,
            role_editor_list_index: 0,
            role_editor_roles: Vec::new(),
            role_editor_field: role_editor_modal::RoleEditorField::Name,
            role_editor_name: TextInput::new(),
            role_editor_description: TextInput::new(),
            role_editor_allowed_tools: ToolListState::new(),
            role_editor_disallowed_tools: ToolListState::new(),
            role_editor_system_prompt: TextInput::new(),
            role_editor_editing_index: None,
            edit_project_mcp_servers: Vec::new(),
            edit_project_mcp_server_index: 0,
            show_mcp_editor: false,
            mcp_editor_field: mcp_editor_modal::McpEditorField::Name,
            mcp_editor_name: TextInput::new(),
            mcp_editor_command: TextInput::new(),
            mcp_editor_args: ToolListState::new(),
            mcp_editor_env: ToolListState::new(),
            mcp_editor_editing_index: None,
            sync_state,
            worktree_sync_in_progress: false,
            worktree_sync_rx: None,
            worktree_sync_pending: 0,
            worktree_sync_completed: Vec::new(),
            tick_count: 0,
            deferred_inputs: Vec::new(),
        }
    }

    fn set_status(&mut self, level: StatusLevel, text: String) {
        self.status_message = Some(StatusMessage { level, text });
    }

    fn set_error(&mut self, text: impl Into<String>) {
        self.set_status(StatusLevel::Error, text.into());
    }

    /// Ensure the global admin session and project exist.
    ///
    /// Creates a dedicated admin directory with a `.mcp.json` pointing to the
    /// `thurbox-mcp` binary, an "Admin" pseudo-project pinned at index 0,
    /// and spawns an admin session if one doesn't already exist.
    /// The `.mcp.json` is rewritten on every startup to pick up binary path
    /// changes after upgrades.
    pub fn ensure_admin_session(&mut self) {
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

        if self.projects[0].session_ids.is_empty() {
            self.spawn_admin_session(admin_dir);
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

    /// Spawn a single admin session in the admin directory.
    fn spawn_admin_session(&mut self, admin_dir: PathBuf) {
        let config = SessionConfig {
            cwd: Some(admin_dir),
            ..SessionConfig::default()
        };
        self.do_spawn_session("admin".to_string(), &config, None, Some(0));
    }

    /// Count sessions belonging to non-admin projects.
    pub fn user_session_count(&self) -> usize {
        self.projects
            .iter()
            .filter(|p| !p.is_admin)
            .map(|p| p.session_ids.len())
            .sum()
    }

    pub fn spawn_session(&mut self) {
        let Some(project) = self.active_project() else {
            return;
        };

        // Admin project: respawn if no sessions, otherwise no-op
        if project.is_admin {
            if project.session_ids.is_empty() {
                if let Some(admin_dir) = crate::paths::admin_directory() {
                    self.spawn_admin_session(admin_dir);
                }
            }
            return;
        }

        let repos = &project.config.repos;
        match repos.len() {
            0 => {
                let mut config = SessionConfig::default();
                if let Some(home) = std::env::var_os("HOME") {
                    config.cwd = Some(PathBuf::from(home));
                }
                self.spawn_session_with_config(&config);
            }
            1 => {
                self.pending_repo_path = Some(repos[0].clone());
                self.session_mode_index = 0;
                self.show_session_mode_modal = true;
            }
            _ => {
                let config = SessionConfig {
                    cwd: Some(repos[0].clone()),
                    additional_dirs: repos[1..].to_vec(),
                    ..SessionConfig::default()
                };
                self.spawn_session_with_config(&config);
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
        self.prepare_spawn(config.clone(), None);
    }

    /// Route session creation through role selection.
    ///
    /// Assigns a session name, then spawns immediately if no roles or exactly
    /// one role is configured, or shows the role selector modal for 2+ roles.
    pub(crate) fn prepare_spawn(
        &mut self,
        mut config: SessionConfig,
        worktree: Option<WorktreeInfo>,
    ) {
        let name = self.next_session_name();
        let Some(project) = self.active_project() else {
            return;
        };
        let roles = &project.config.roles;

        match roles.len() {
            0 => {
                // No roles configured — spawn with default (empty) permissions.
                self.do_spawn_session(name, &config, worktree, None);
            }
            1 => {
                // Exactly one role — auto-assign it.
                config.role = roles[0].name.clone();
                config.permissions = roles[0].permissions.clone();
                self.do_spawn_session(name, &config, worktree, None);
            }
            _ => {
                // 2+ roles — show the role selector.
                self.pending_spawn_name = Some(name);
                self.pending_spawn_config = Some(config);
                self.pending_spawn_worktree = worktree;
                self.role_selector_index = 0;
                self.show_role_selector = true;
            }
        }
    }

    fn restart_active_session(&mut self) {
        let Some(session) = self.sessions.get(self.active_index) else {
            return;
        };
        let Some(claude_session_id) = session.info.claude_session_id.clone() else {
            return;
        };

        let role = session.info.role.clone();
        let cwd = session.info.cwd.clone();
        let additional_dirs = session.info.additional_dirs.clone();

        let permissions = self.resolve_role_permissions(&role);
        let config = SessionConfig {
            resume_session_id: Some(claude_session_id.clone()),
            claude_session_id: Some(claude_session_id),
            cwd,
            additional_dirs,
            role,
            permissions,
        };

        let (rows, cols) = self.content_area_size();
        let session = &mut self.sessions[self.active_index];
        match session.restart(&config, rows, cols) {
            Ok(()) => {
                self.save_state();
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

        // Prevent closing admin sessions
        if let Some(project) = self
            .projects
            .iter()
            .find(|p| p.session_ids.contains(&session_id))
        {
            if project.is_admin {
                self.set_error("Cannot close admin session");
                return;
            }
        }

        // Clean up worktree if present
        if let Some(wt) = &session.info.worktree {
            if let Err(e) = git::remove_worktree(&wt.repo_path, &wt.worktree_path) {
                error!("Failed to remove worktree: {e}");
            }
        }

        // Soft-delete in DB
        if let Err(e) = self.db.soft_delete_session(session_id) {
            error!("Failed to soft-delete session in DB: {e}");
        }

        // Kill the backend session before removing from the list.
        if let Some(session) = self.sessions.get_mut(self.active_index) {
            session.kill();
        }
        self.sessions.remove(self.active_index);

        // Remove session from its project
        for project in &mut self.projects {
            project.session_ids.retain(|id| *id != session_id);
        }

        if self.sessions.is_empty() {
            self.active_index = 0;
        } else if self.active_index >= self.sessions.len() {
            self.active_index = self.sessions.len() - 1;
        }

        // Sync to shared state for other instances
        self.save_state();
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
        session.info.claude_session_id = shared.claude_session_id.clone();
        if let Some(wt) = &shared.worktree {
            session.info.worktree = Some(WorktreeInfo {
                repo_path: wt.repo_path.clone(),
                worktree_path: wt.worktree_path.clone(),
                branch: wt.branch.clone(),
            });
        }
    }

    pub fn update(&mut self, msg: AppMessage) {
        match msg {
            AppMessage::KeyPress(code, mods) => self.handle_key(code, mods),
            AppMessage::MouseScrollUp => self.scroll_terminal_up(MOUSE_SCROLL_LINES),
            AppMessage::MouseScrollDown => self.scroll_terminal_down(MOUSE_SCROLL_LINES),
            AppMessage::Resize(cols, rows) => self.handle_resize(cols, rows),
            AppMessage::ExternalStateChange(delta) => self.handle_external_state_change(delta),
        }
    }

    pub(crate) fn with_active_parser(&self, f: impl FnOnce(&mut vt100::Parser)) {
        if let Some(session) = self.sessions.get(self.active_index) {
            if let Ok(mut parser) = session.parser.lock() {
                f(&mut parser);
            }
        }
    }

    pub(crate) fn scroll_terminal_up(&self, lines: usize) {
        self.with_active_parser(|parser| {
            let current = parser.screen().scrollback();
            parser.screen_mut().set_scrollback(current + lines);
        });
    }

    pub(crate) fn scroll_terminal_down(&self, lines: usize) {
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

    pub(crate) fn submit_role_editor(&mut self) {
        let name = self.role_editor_name.value().trim().to_string();
        if name.is_empty() {
            self.set_error("Role name cannot be empty");
            return;
        }

        // Check uniqueness (exclude the role being edited)
        let duplicate = self
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

        // Preserve fields not exposed in the editor (permission_mode, tools)
        let base_permissions = self
            .role_editor_editing_index
            .and_then(|idx| self.role_editor_roles.get(idx))
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
            },
        };

        match self.role_editor_editing_index {
            Some(idx) => {
                self.role_editor_roles[idx] = role;
            }
            None => {
                self.role_editor_roles.push(role);
                self.role_editor_list_index = self.role_editor_roles.len() - 1;
            }
        }

        self.status_message = None;
        // Return to edit-project modal (roles field) instead of role list
        self.show_role_editor = false;
        self.edit_project_field = EditProjectField::Roles;
    }

    pub(crate) fn spawn_worktree_session(
        &mut self,
        repo_path: PathBuf,
        new_branch: &str,
        base_branch: &str,
    ) {
        match git::create_worktree(&repo_path, new_branch, base_branch) {
            Ok(worktree_path) => {
                let worktree_info = WorktreeInfo {
                    repo_path,
                    worktree_path: worktree_path.clone(),
                    branch: new_branch.to_string(),
                };
                let config = SessionConfig {
                    cwd: Some(worktree_path),
                    ..SessionConfig::default()
                };
                self.prepare_spawn(config, Some(worktree_info));
            }
            Err(e) => {
                error!("Failed to create worktree: {e}");
                self.set_error(format!("Failed to create worktree: {e:#}"));
            }
        }
    }

    pub(crate) fn do_spawn_session(
        &mut self,
        name: String,
        config: &SessionConfig,
        worktree: Option<WorktreeInfo>,
        target_project_index: Option<usize>,
    ) {
        let (rows, cols) = self.content_area_size();

        let mut config = config.clone();
        if config.claude_session_id.is_none() {
            config.claude_session_id = Some(uuid::Uuid::new_v4().to_string());
        }

        match Session::spawn(name, rows, cols, &config, &self.backend) {
            Ok(mut session) => {
                session.info.worktree = worktree;
                let session_id = session.info.id;
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

                // Sync to shared state for other instances
                self.save_state();
            }
            Err(e) => {
                error!("Failed to spawn session: {e}");
                self.set_error(format!("Failed to start claude: {e:#}"));
            }
        }
    }

    pub(crate) fn submit_add_project(&mut self) {
        let name = self.add_project_name.value().trim().to_string();

        // If the path field has content, treat it as an un-added repo
        let pending_path = self.add_project_path.value().trim().to_string();
        if !pending_path.is_empty() {
            self.add_project_repos.push(PathBuf::from(pending_path));
        }

        if name.is_empty() || self.add_project_repos.is_empty() {
            self.set_error("Project name and at least one repo are required");
            return;
        }

        let config = ProjectConfig {
            name,
            repos: self.add_project_repos.clone(),
            roles: Vec::new(),
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

        self.edit_project_name.set(&name);
        self.edit_project_path.clear();
        self.edit_project_field = EditProjectField::Name;
        self.edit_project_repos = repos;
        self.edit_project_repo_index = 0;
        self.edit_project_path_suggestion = None;
        self.edit_project_original_id = Some(id);
        self.role_editor_roles = roles;
        self.role_editor_list_index = 0;
        self.edit_project_mcp_servers = mcp_servers;
        self.edit_project_mcp_server_index = 0;
        self.show_edit_project_modal = true;
    }

    pub(crate) fn submit_edit_project(&mut self) {
        let name = self.edit_project_name.value().trim().to_string();

        // If the path field has content, treat it as an un-added repo
        let pending_path = self.edit_project_path.value().trim().to_string();
        if !pending_path.is_empty() {
            self.edit_project_repos.push(PathBuf::from(pending_path));
        }

        if name.is_empty() || self.edit_project_repos.is_empty() {
            self.set_error("Project name and at least one repo are required");
            return;
        }

        let Some(original_id) = self.edit_project_original_id else {
            return;
        };

        // Find project by original ID (stable across renames)
        let Some(project) = self.projects.iter_mut().find(|p| p.id == original_id) else {
            self.set_error("Project not found");
            return;
        };

        // Update config without regenerating ID
        project.config.name = name;
        project.config.repos = self.edit_project_repos.clone();
        project.config.roles = self.role_editor_roles.clone();
        project.config.mcp_servers = self.edit_project_mcp_servers.clone();

        // Persist project to DB at point of change
        let project_clone = project.clone();
        self.save_project_to_db(&project_clone);
        self.status_message = None;

        self.close_edit_project_modal();
    }

    pub(crate) fn close_edit_project_modal(&mut self) {
        self.show_edit_project_modal = false;
        self.show_role_editor = false;
        self.show_mcp_editor = false;
        self.edit_project_name.clear();
        self.edit_project_path.clear();
        self.edit_project_field = EditProjectField::Name;
        self.edit_project_repos.clear();
        self.edit_project_repo_index = 0;
        self.edit_project_path_suggestion = None;
        self.edit_project_original_id = None;
        self.role_editor_roles.clear();
        self.role_editor_list_index = 0;
        self.edit_project_mcp_servers.clear();
        self.edit_project_mcp_server_index = 0;
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

        self.show_delete_project_modal_flag = true;
        self.delete_project_name = project_name;
        self.delete_project_confirmation = TextInput::new();
        self.delete_project_error = None;
    }

    pub(crate) fn delete_active_project(&mut self) {
        // Validate confirmation
        if self.delete_project_confirmation.value() != self.delete_project_name {
            self.delete_project_error = Some("Project name doesn't match".to_string());
            return;
        }

        // Get project session IDs, ID, and name before removal
        let Some(project) = self.active_project() else {
            self.show_delete_project_modal_flag = false;
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
        self.show_delete_project_modal_flag = false;
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

    /// Switch to the next project and sync the active session.
    pub(crate) fn switch_project_forward(&mut self) {
        if self.active_project_index + 1 < self.projects.len() {
            self.active_project_index += 1;
            self.sync_active_session_to_project();
        }
    }

    /// Switch to the previous project and sync the active session.
    pub(crate) fn switch_project_backward(&mut self) {
        if self.active_project_index > 0 {
            self.active_project_index -= 1;
            self.sync_active_session_to_project();
        }
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

        // Poll for external state changes from other thurbox instances (DB-based)
        if let Ok(Some(delta)) = sync::poll_for_changes(&mut self.sync_state, &mut self.db) {
            self.handle_external_state_change(delta);
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
    pub(crate) fn start_sync(&mut self) {
        if self.worktree_sync_in_progress {
            return;
        }

        let worktree_sessions: Vec<_> = self
            .sessions
            .iter()
            .filter_map(|s| {
                s.info
                    .worktree
                    .as_ref()
                    .map(|wt| (s.info.id, wt.worktree_path.clone()))
            })
            .collect();

        if worktree_sessions.is_empty() {
            self.set_status(StatusLevel::Info, "No worktrees to sync".into());
            return;
        }

        let count = worktree_sessions.len();
        let (tx, rx) = mpsc::channel();

        for (session_id, worktree_path) in worktree_sessions {
            let tx = tx.clone();
            std::thread::spawn(move || {
                let result = git::sync_worktree(&worktree_path);
                let _ = tx.send((session_id, result));
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
                // Clean up worktree if present
                if let Some(wt) = &self.sessions[pos].info.worktree {
                    if let Err(e) = git::remove_worktree(&wt.repo_path, &wt.worktree_path) {
                        error!("Failed to remove worktree for deleted session: {e}");
                    }
                }
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
            match Session::adopt(
                shared_session.name.clone(),
                rows,
                cols,
                &shared_session.backend_id,
                &self.backend,
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
                }
            }
        }
    }

    pub fn view(&self, frame: &mut Frame) {
        let areas = layout::compute_layout(frame.area(), self.show_info_panel);

        status_bar::render_header(frame, areas.header);

        // Left panel (projects + sessions)
        if let Some(left_area) = areas.left_panel {
            let project_entries: Vec<project_list::ProjectEntry<'_>> = self
                .projects
                .iter()
                .map(|p| project_list::ProjectEntry {
                    name: &p.config.name,
                    session_count: p.session_ids.len(),
                    is_admin: p.is_admin,
                })
                .collect();

            let project_session_indices = self.active_project_sessions();
            let project_sessions: Vec<&SessionInfo> = project_session_indices
                .iter()
                .map(|&i| &self.sessions[i].info)
                .collect();

            let panel_focus = match self.focus {
                InputFocus::ProjectList => project_list::LeftPanelFocus::Projects,
                InputFocus::SessionList | InputFocus::Terminal => {
                    project_list::LeftPanelFocus::Sessions
                }
            };

            project_list::render_left_panel(
                frame,
                left_area,
                &project_list::LeftPanelState {
                    projects: &project_entries,
                    active_project: self.active_project_index,
                    sessions: &project_sessions,
                    active_session: self.active_session_in_project(),
                    focus: panel_focus,
                    panel_focused: self.focus != InputFocus::Terminal,
                },
            );
        }

        // Info panel
        if let Some(info_area) = areas.info_panel {
            let active_project = self.projects.get(self.active_project_index);
            if let Some(session) = self.sessions.get(self.active_index) {
                info_panel::render_info_panel(frame, info_area, &session.info, active_project);
            }
        }

        // Terminal
        match self.sessions.get(self.active_index) {
            Some(session) => {
                if let Ok(mut parser) = session.parser.lock() {
                    terminal_view::render_terminal(
                        frame,
                        areas.terminal,
                        &mut parser,
                        &session.info,
                        self.focus == InputFocus::Terminal,
                    );
                }
            }
            None => terminal_view::render_empty_terminal(frame, areas.terminal),
        }

        let focus_label = match self.focus {
            InputFocus::ProjectList => "Projects",
            InputFocus::SessionList => "Sessions",
            InputFocus::Terminal => "Terminal",
        };
        status_bar::render_footer(
            frame,
            areas.footer,
            &status_bar::FooterState {
                session_count: self.sessions.len(),
                project_count: self.projects.len(),
                status: self.status_message.as_ref(),
                focus_label,
                sync_in_progress: self.worktree_sync_in_progress,
                tick_count: self.tick_count,
            },
        );

        // Help overlay (rendered last, on top of everything)
        if self.show_help {
            render_help_overlay(frame);
        }

        // Add-project modal (on top of everything including help)
        if self.show_add_project_modal {
            add_project_modal::render_add_project_modal(
                frame,
                &add_project_modal::AddProjectModalState {
                    name: self.add_project_name.value(),
                    name_cursor: self.add_project_name.cursor_pos(),
                    path: self.add_project_path.value(),
                    path_cursor: self.add_project_path.cursor_pos(),
                    path_suggestion: self.add_project_path_suggestion.as_deref(),
                    repos: &self.add_project_repos,
                    repo_index: self.add_project_repo_index,
                    focused_field: self.add_project_field,
                },
            );
        }

        // Edit-project modal
        if self.show_edit_project_modal {
            edit_project_modal::render_edit_project_modal(
                frame,
                &edit_project_modal::EditProjectModalState {
                    name: self.edit_project_name.value(),
                    name_cursor: self.edit_project_name.cursor_pos(),
                    path: self.edit_project_path.value(),
                    path_cursor: self.edit_project_path.cursor_pos(),
                    path_suggestion: self.edit_project_path_suggestion.as_deref(),
                    repos: &self.edit_project_repos,
                    repo_index: self.edit_project_repo_index,
                    roles: &self.role_editor_roles,
                    role_index: self.role_editor_list_index,
                    mcp_servers: &self.edit_project_mcp_servers,
                    mcp_server_index: self.edit_project_mcp_server_index,
                    focused_field: self.edit_project_field,
                },
            );
        }

        // Delete-project modal
        if self.show_delete_project_modal_flag {
            delete_project_modal::render_delete_project_modal(
                frame,
                &delete_project_modal::DeleteProjectModalState {
                    project_name: &self.delete_project_name,
                    confirmation: self.delete_project_confirmation.value(),
                    confirmation_cursor: self.delete_project_confirmation.cursor_pos(),
                    error: self.delete_project_error.as_deref(),
                },
            );
        }

        // Repo selector modal
        if self.show_repo_selector {
            if let Some(active_project) = self.active_project() {
                repo_selector_modal::render_repo_selector_modal(
                    frame,
                    &repo_selector_modal::RepoSelectorState {
                        repos: &active_project.config.repos,
                        selected_index: self.repo_selector_index,
                    },
                );
            }
        }

        // Session mode modal
        if self.show_session_mode_modal {
            session_mode_modal::render_session_mode_modal(
                frame,
                &session_mode_modal::SessionModeState {
                    selected_index: self.session_mode_index,
                },
            );
        }

        // Worktree name modal
        if self.show_worktree_name_modal {
            let base = self.pending_base_branch.as_deref().unwrap_or("");
            worktree_name_modal::render_worktree_name_modal(
                frame,
                &worktree_name_modal::WorktreeNameState {
                    name: self.worktree_name_input.value(),
                    cursor: self.worktree_name_input.cursor_pos(),
                    base_branch: base,
                },
            );
        }

        // Branch selector modal
        if self.show_branch_selector {
            branch_selector_modal::render_branch_selector_modal(
                frame,
                &branch_selector_modal::BranchSelectorState {
                    branches: &self.available_branches,
                    selected_index: self.branch_selector_index,
                },
            );
        }

        // Role selector modal
        if self.show_role_selector {
            if let Some(project) = self.active_project() {
                role_selector_modal::render_role_selector_modal(
                    frame,
                    &role_selector_modal::RoleSelectorState {
                        roles: &project.config.roles,
                        selected_index: self.role_selector_index,
                    },
                );
            }
        }

        // Role editor modal (detail form, overlays edit-project modal)
        if self.show_role_editor {
            role_editor_modal::render_role_editor_modal(
                frame,
                &role_editor_modal::RoleEditorState {
                    name: self.role_editor_name.value(),
                    name_cursor: self.role_editor_name.cursor_pos(),
                    description: self.role_editor_description.value(),
                    description_cursor: self.role_editor_description.cursor_pos(),
                    allowed_tools: &self.role_editor_allowed_tools.items,
                    allowed_tools_index: self.role_editor_allowed_tools.selected,
                    allowed_tools_mode: self.role_editor_allowed_tools.mode,
                    allowed_tools_input: self.role_editor_allowed_tools.input.value(),
                    allowed_tools_input_cursor: self.role_editor_allowed_tools.input.cursor_pos(),
                    disallowed_tools: &self.role_editor_disallowed_tools.items,
                    disallowed_tools_index: self.role_editor_disallowed_tools.selected,
                    disallowed_tools_mode: self.role_editor_disallowed_tools.mode,
                    disallowed_tools_input: self.role_editor_disallowed_tools.input.value(),
                    disallowed_tools_input_cursor: self
                        .role_editor_disallowed_tools
                        .input
                        .cursor_pos(),
                    system_prompt: self.role_editor_system_prompt.value(),
                    system_prompt_cursor: self.role_editor_system_prompt.cursor_pos(),
                    focused_field: self.role_editor_field,
                },
            );
        }

        // MCP editor modal (detail form, overlays edit-project modal)
        if self.show_mcp_editor {
            crate::ui::mcp_editor_modal::render_mcp_editor_modal(
                frame,
                &crate::ui::mcp_editor_modal::McpEditorState {
                    name: self.mcp_editor_name.value(),
                    name_cursor: self.mcp_editor_name.cursor_pos(),
                    command: self.mcp_editor_command.value(),
                    command_cursor: self.mcp_editor_command.cursor_pos(),
                    args: &self.mcp_editor_args.items,
                    args_index: self.mcp_editor_args.selected,
                    args_mode: self.mcp_editor_args.mode,
                    args_input: self.mcp_editor_args.input.value(),
                    args_input_cursor: self.mcp_editor_args.input.cursor_pos(),
                    env: &self.mcp_editor_env.items,
                    env_index: self.mcp_editor_env.selected,
                    env_mode: self.mcp_editor_env.mode,
                    env_input: self.mcp_editor_env.input.value(),
                    env_input_cursor: self.mcp_editor_env.input.cursor_pos(),
                    focused_field: self.mcp_editor_field,
                },
            );
        }
    }

    pub fn should_quit(&self) -> bool {
        self.should_quit
    }

    pub fn shutdown(self) {
        self.save_state();
        // Do NOT remove worktrees — they persist for resume.
        // Detach from backend sessions without killing them — they persist in tmux.
        for session in self.sessions {
            session.detach();
        }
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
            claude_session_id: session.info.claude_session_id.clone(),
            cwd: session.info.cwd.clone(),
            additional_dirs: session.info.additional_dirs.clone(),
            worktree: session
                .info
                .worktree
                .as_ref()
                .map(|wt| sync::SharedWorktree {
                    repo_path: wt.repo_path.clone(),
                    worktree_path: wt.worktree_path.clone(),
                    branch: wt.branch.clone(),
                }),
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

        // Only restore sessions that have a claude_session_id (resumable)
        let resumable: Vec<sync::SharedSession> = sessions
            .into_iter()
            .filter(|s| s.claude_session_id.is_some())
            .collect();

        if resumable.is_empty() {
            return None;
        }

        let counter = self.db.get_session_counter().unwrap_or(0);
        Some((resumable, counter))
    }

    /// Restore sessions from the database on startup.
    ///
    /// Tries to adopt existing backend sessions (tmux windows) or spawns new
    /// sessions with `--resume` to reconnect to the Claude session.
    pub fn restore_sessions(&mut self, sessions: Vec<sync::SharedSession>, session_counter: usize) {
        self.session_counter = session_counter;

        // Discover existing sessions from the backend.
        let discovered = self.backend.discover().unwrap_or_default();

        for shared in sessions {
            let name = shared.name;
            let session_id = shared.id;

            let role = if shared.role.is_empty() {
                DEFAULT_ROLE_NAME.to_string()
            } else {
                shared.role
            };

            let worktree = shared.worktree.map(|wt| WorktreeInfo {
                repo_path: wt.repo_path,
                worktree_path: wt.worktree_path,
                branch: wt.branch,
            });

            let claude_session_id = match shared.claude_session_id {
                Some(id) => id,
                None => continue, // Skip sessions without a claude session ID
            };

            // Try to match a discovered backend session by backend_id.
            let matching_discovered = if !shared.backend_id.is_empty() {
                discovered
                    .iter()
                    .find(|d| d.backend_id == shared.backend_id && d.is_alive)
            } else {
                // Fall back to matching by window name (tb-<name>).
                let expected_name = format!("tb-{name}");
                discovered
                    .iter()
                    .find(|d| d.name == expected_name && d.is_alive)
            };

            // Try to adopt the existing backend session.
            let adopted = matching_discovered.and_then(|disc| {
                let (rows, cols) = self.content_area_size();
                match Session::adopt(name.clone(), rows, cols, &disc.backend_id, &self.backend) {
                    Ok(session) => Some(session),
                    Err(e) => {
                        error!("Failed to adopt session '{name}': {e}");
                        None
                    }
                }
            });

            if let Some(mut session) = adopted {
                session.info.id = session_id;
                session.info.claude_session_id = Some(claude_session_id.clone());
                session.info.cwd = shared.cwd;
                session.info.additional_dirs = shared.additional_dirs;
                session.info.role = role;
                session.info.worktree = worktree;
                let sid = session.info.id;
                self.sessions.push(session);
                self.active_index = self.sessions.len() - 1;
                self.focus = InputFocus::Terminal;

                // Associate with the original project
                let target_project_index =
                    self.find_project_index_for_session(sid, &shared.project_id);

                if let Some(project) = self.projects.get_mut(target_project_index) {
                    if !project.session_ids.contains(&sid) {
                        project.session_ids.push(sid);
                    }
                }
            } else {
                // No matching backend session or adopt failed — spawn new with --resume.
                // Soft-delete the stale session entry to prevent duplication on next restart.
                if let Err(e) = self.db.soft_delete_session(session_id) {
                    error!("Failed to soft-delete stale session {session_id}: {e}");
                }

                // Look up the original project so we respawn into the correct one.
                let target_project_index =
                    self.find_project_index_for_session(session_id, &shared.project_id);

                let is_admin = self
                    .projects
                    .get(target_project_index)
                    .is_some_and(|p| p.config.name == "Admin");

                let permissions =
                    self.resolve_role_permissions_for_project(&role, target_project_index);

                // Admin sessions start fresh — --resume would fail because the
                // old Claude conversation no longer exists after a tmux restart.
                let config = SessionConfig {
                    resume_session_id: if is_admin {
                        None
                    } else {
                        Some(claude_session_id.clone())
                    },
                    claude_session_id: if is_admin {
                        None
                    } else {
                        Some(claude_session_id)
                    },
                    cwd: shared.cwd,
                    additional_dirs: shared.additional_dirs,
                    role,
                    permissions,
                };
                self.do_spawn_session(name, &config, worktree, Some(target_project_index));
            }
        }

        // Claim ownership of restored sessions in the shared state
        self.save_state();
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
    fn resolve_role_permissions_for_project(
        &self,
        role_name: &str,
        project_index: usize,
    ) -> RolePermissions {
        self.projects
            .get(project_index)
            .and_then(|project| {
                project
                    .config
                    .roles
                    .iter()
                    .find(|r| r.name == role_name)
                    .map(|r| r.permissions.clone())
            })
            .unwrap_or_default()
    }

    /// Resolve a role name to its permissions using the active project's role config.
    fn resolve_role_permissions(&self, role_name: &str) -> RolePermissions {
        self.resolve_role_permissions_for_project(role_name, self.active_project_index)
    }

    pub(crate) fn content_area_size(&self) -> (u16, u16) {
        // Header: 1 line, Footer: 1 line, Borders: 2 lines top+bottom
        let rows = self.terminal_rows.saturating_sub(4);

        let three_panel = self.show_info_panel && self.terminal_cols >= 120;

        let list_width = if self.terminal_cols >= 80 {
            if three_panel {
                self.terminal_cols * 15 / 100 // matches layout.rs 3-panel: 15%
            } else {
                self.terminal_cols * 20 / 100 // matches layout.rs 2-panel: 20%
            }
        } else {
            0
        };
        let info_width = if three_panel {
            self.terminal_cols * 15 / 100 // matches layout.rs 3-panel: 15%
        } else {
            0
        };
        let cols = self
            .terminal_cols
            .saturating_sub(list_width + info_width + 2);
        (rows, cols)
    }
}

fn render_help_overlay(frame: &mut Frame) {
    let area = centered_rect(60, 70, frame.area());

    frame.render_widget(Clear, area);

    let block = Block::default()
        .title(" Keybindings ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan));

    let help_lines = vec![
        help_section("Navigation (Vim: h/j/k/l)"),
        help_line("Ctrl+H", "Focus project list (h = left)"),
        help_line("Ctrl+J", "Next project (project focus) / session"),
        help_line("Ctrl+K", "Previous project (project focus) / session"),
        help_line("Ctrl+L", "Cycle focus (l = right/forward)"),
        Line::from(""),
        help_section("Session Management"),
        help_line("Ctrl+N", "New project (project focus) / session"),
        help_line("Ctrl+C", "Close active session"),
        help_line("Ctrl+R", "Restart active session"),
        help_line("Ctrl+S", "Sync all worktrees with main"),
        Line::from(""),
        help_section("Project Management"),
        help_line(
            "Ctrl+D",
            "Delete session (session list) / project (project list)",
        ),
        help_line("Ctrl+E", "Edit active project (name, repos, roles)"),
        Line::from(""),
        help_section("UI"),
        help_line("Ctrl+Q", "Quit Thurbox"),
        help_line("F1", "Show this help"),
        help_line("F2", "Toggle info panel"),
        Line::from(""),
        help_section("Project List (when focused)"),
        help_line("j / Down", "Next project"),
        help_line("k / Up", "Previous project"),
        help_line("Enter", "Focus session list"),
        Line::from(""),
        help_section("Session List (when focused)"),
        help_line("j / Down", "Next session"),
        help_line("k / Up", "Previous session"),
        help_line("Enter", "Focus terminal"),
        Line::from(""),
        help_section("Terminal (when focused)"),
        help_line("Shift+\u{2191}/\u{2193}", "Scroll up/down 1 line"),
        help_line("Shift+PgUp/PgDn", "Scroll up/down half page"),
        help_line("Mouse wheel", "Scroll up/down 3 lines"),
        help_line("*", "All other keys forwarded to session"),
        Line::from(""),
        Line::from(Span::styled(
            "Press Esc to close",
            Style::default().fg(Color::DarkGray),
        )),
    ];

    let paragraph = Paragraph::new(help_lines).block(block);
    frame.render_widget(paragraph, area);
}

fn help_section(title: &str) -> Line<'_> {
    Line::from(Span::styled(
        title,
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    ))
}

fn help_line<'a>(key: &'a str, desc: &'a str) -> Line<'a> {
    Line::from(vec![
        Span::styled(
            format!("  {key:<16}"),
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(desc, Style::default().fg(Color::White)),
    ])
}

/// Create a centered rectangle within the given area.
fn centered_rect(percent_x: u16, percent_y: u16, area: Rect) -> Rect {
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(area);

    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(vertical[1])[1]
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

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
            _: u16,
            _: u16,
        ) -> anyhow::Result<crate::claude::backend::SpawnedSession> {
            anyhow::bail!("stub backend does not spawn")
        }
        fn adopt(
            &self,
            _: &str,
            _: u16,
            _: u16,
        ) -> anyhow::Result<crate::claude::backend::AdoptedSession> {
            anyhow::bail!("stub backend does not adopt")
        }
        fn discover(&self) -> anyhow::Result<Vec<crate::claude::backend::DiscoveredSession>> {
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
    }

    fn stub_backend() -> Arc<dyn SessionBackend> {
        Arc::new(StubBackend)
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
        let backend = stub_backend();
        let mut app = App::new(
            24,
            120,
            backend.clone(),
            test_db_with_project(&test_project_config()),
        );
        for i in 0..count {
            let session = Session::stub(&format!("Session {}", i + 1), &backend);
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
        let app = App::new(50, 100, stub_backend(), test_db());
        // rows = 50 - 4 = 46, half = 23
        assert_eq!(app.page_scroll_amount(), 23);
    }

    #[test]
    fn page_scroll_amount_small_terminal() {
        let app = App::new(6, 80, stub_backend(), test_db());
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
        let mut app = App::new(24, 80, stub_backend(), test_db());
        assert_eq!(app.next_session_name(), "1");
    }

    #[test]
    fn next_session_name_increments() {
        let mut app = App::new(24, 80, stub_backend(), test_db());
        assert_eq!(app.next_session_name(), "1");
        assert_eq!(app.next_session_name(), "2");
        assert_eq!(app.next_session_name(), "3");
    }

    #[test]
    fn next_session_name_continues_from_restored_counter() {
        let mut app = App::new(24, 80, stub_backend(), test_db());
        app.session_counter = 5;
        assert_eq!(app.next_session_name(), "6");
    }

    // --- Role editor tests ---

    #[test]
    fn open_role_editor_starts_empty_for_no_custom_roles() {
        let mut app = App::new(
            24,
            120,
            stub_backend(),
            test_db_with_project(&test_project_config()),
        );
        app.open_role_editor();
        assert!(app.show_role_editor);
        assert!(app.role_editor_roles.is_empty());
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
        let mut app = App::new(24, 120, stub_backend(), test_db_with_project(&config));
        app.open_role_editor();
        assert_eq!(app.role_editor_roles.len(), 1);
        assert_eq!(app.role_editor_roles[0].name, "ops");
    }

    #[test]
    fn role_editor_submit_uses_allowed_tools_list() {
        let mut app = App::new(24, 120, stub_backend(), test_db());
        app.open_role_editor();
        app.handle_role_editor_list_key(KeyCode::Char('a'));
        for c in "reviewer".chars() {
            app.role_editor_name.insert(c);
        }
        app.role_editor_allowed_tools.items.push("Read".to_string());
        app.role_editor_allowed_tools
            .items
            .push("Bash(git:*)".to_string());
        app.submit_role_editor();
        assert_eq!(app.role_editor_roles.len(), 1);
        assert_eq!(
            app.role_editor_roles[0].permissions.allowed_tools,
            vec!["Read".to_string(), "Bash(git:*)".to_string()]
        );
    }

    #[test]
    fn role_editor_submit_uses_disallowed_tools_list() {
        let mut app = App::new(24, 120, stub_backend(), test_db());
        app.open_role_editor();
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
        assert_eq!(app.role_editor_roles.len(), 1);
        assert_eq!(
            app.role_editor_roles[0].permissions.disallowed_tools,
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
        let mut app = App::new(24, 120, stub_backend(), test_db_with_project(&config));
        let session_config = SessionConfig::default();
        app.prepare_spawn(session_config, None);
        assert!(app.show_role_selector);
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
        let app = App::new(24, 120, stub_backend(), test_db_with_project(&config));
        // With no roles, the selector should never be set
        assert!(!app.show_role_selector);
    }

    #[test]
    fn role_editor_name_validation_rejects_empty() {
        let mut app = App::new(24, 120, stub_backend(), test_db());
        app.open_role_editor();
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
        let mut app = App::new(24, 120, stub_backend(), test_db());
        app.open_role_editor();
        // Add first role
        app.handle_role_editor_list_key(KeyCode::Char('a'));
        app.role_editor_name.set("dev");
        app.submit_role_editor();
        assert_eq!(app.role_editor_roles.len(), 1);

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
        assert_eq!(app.role_editor_roles.len(), 1);
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
                },
            }],
            mcp_servers: vec![],
            id: None,
        };
        let mut app = App::new(24, 120, stub_backend(), test_db_with_project(&config));
        app.open_role_editor();
        app.open_role_for_editing(0);

        // Modify the name and submit
        app.role_editor_name.set("custom-v2");
        app.submit_role_editor();

        let role = &app.role_editor_roles[0];
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
        let mut app = App::new(24, 120, stub_backend(), test_db());
        app.open_role_editor();
        app.handle_role_editor_list_key(KeyCode::Char('a'));
        app.role_editor_name.set("new-role");
        app.submit_role_editor();

        let role = &app.role_editor_roles[0];
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
        let mut app = App::new(24, 120, stub_backend(), test_db_with_project(&config));
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
        let mut app = App::new(24, 120, stub_backend(), test_db());
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
        assert_eq!(app.role_editor_field, RoleEditorField::Name);
    }

    #[test]
    fn role_editor_backtab_cycles_fields_backward() {
        use role_editor_modal::RoleEditorField;
        let mut app = App::new(24, 120, stub_backend(), test_db());
        app.open_role_editor();
        app.handle_role_editor_list_key(KeyCode::Char('a'));

        assert_eq!(app.role_editor_field, RoleEditorField::Name);
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
        let mut app = App::new(24, 120, stub_backend(), test_db());
        app.open_role_editor();
        app.handle_role_editor_list_key(KeyCode::Char('a'));
        assert_eq!(app.role_editor_view, RoleEditorView::Editor);

        app.handle_role_editor_editor_key(KeyCode::Esc);
        // Esc now closes the role editor overlay, returning to edit-project
        assert!(!app.show_role_editor);
        assert_eq!(app.edit_project_field, EditProjectField::Roles);
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
        let mut app = App::new(24, 120, stub_backend(), test_db_with_project(&config));
        app.open_role_editor();
        // Select the last role
        app.role_editor_list_index = 1;
        // Delete it
        app.handle_role_editor_list_key(KeyCode::Char('d'));
        assert_eq!(app.role_editor_roles.len(), 1);
        assert_eq!(app.role_editor_list_index, 0);
    }

    #[test]
    fn role_editor_submit_clears_error_on_success() {
        let mut app = App::new(24, 120, stub_backend(), test_db());
        app.open_role_editor();
        app.handle_role_editor_list_key(KeyCode::Char('a'));

        // Trigger an error by submitting with empty name
        app.submit_role_editor();
        assert!(app.status_message.is_some());

        // Now provide a valid name and submit again
        app.role_editor_name.set("valid-role");
        app.submit_role_editor();
        assert!(app.status_message.is_none());
        assert_eq!(app.role_editor_roles.len(), 1);
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
        let mut app = App::new(24, 120, stub_backend(), test_db());
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
        let mut app = App::new(24, 120, stub_backend(), test_db());
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
        let mut app = App::new(24, 120, stub_backend(), test_db());
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
        let mut app = App::new(24, 120, stub_backend(), test_db_with_project(&config));
        app.open_role_editor();
        app.open_role_for_editing(0);

        // Verify it was loaded
        assert_eq!(app.role_editor_system_prompt.value(), "Be safe");

        // Modify and submit
        app.role_editor_system_prompt.set("Be very safe");
        app.submit_role_editor();

        assert_eq!(
            app.role_editor_roles[0].permissions.append_system_prompt,
            Some("Be very safe".to_string())
        );
    }

    #[test]
    fn system_prompt_empty_saves_as_none() {
        let mut app = App::new(24, 120, stub_backend(), test_db());
        app.open_role_editor();
        app.handle_role_editor_list_key(KeyCode::Char('a'));
        app.role_editor_name.set("test");
        app.role_editor_system_prompt.set("");
        app.submit_role_editor();

        assert!(app.role_editor_roles[0]
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
        let app = App::new(24, 120, stub_backend(), test_db_with_project(&config));
        // With exactly 1 role, prepare_spawn should not show selector
        // (it would try to spawn, which needs a runtime — just verify no selector)
        assert!(!app.show_role_selector);
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
        let app = App::new(24, 120, stub_backend(), test_db());
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

        let app = App::new(24, 120, backend, db);

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
    fn ctrl_c_closes_active_session() {
        let mut app = app_with_sessions(2);
        app.active_index = 0;
        let initial_count = app.sessions.len();
        app.handle_key(KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert!(app.sessions.len() < initial_count);
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
        assert!(app.show_delete_project_modal_flag);
    }

    #[test]
    fn ctrl_d_forwards_to_pty_from_terminal() {
        let mut app = app_with_sessions(1);
        app.focus = InputFocus::Terminal;
        app.handle_key(KeyCode::Char('d'), KeyModifiers::CONTROL);
        // Should NOT show delete modal — Ctrl+D is forwarded to PTY
        assert!(!app.show_delete_project_modal_flag);
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
            app.show_help = false;
            app.focus = focus;
            app.handle_key(KeyCode::F(1), KeyModifiers::NONE);
            assert!(app.show_help, "F1 should show help from {focus:?}");
        }
    }

    #[test]
    fn f1_does_not_activate_during_modal() {
        let mut app = app_with_sessions(0);
        app.show_repo_selector = true;
        app.handle_key(KeyCode::F(1), KeyModifiers::NONE);
        assert!(!app.show_help);
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
    fn ctrl_j_at_last_project_is_noop() {
        let mut app = app_with_projects(3);
        app.focus = InputFocus::ProjectList;
        app.active_project_index = 2;
        app.handle_key(KeyCode::Char('j'), KeyModifiers::CONTROL);
        assert_eq!(app.active_project_index, 2);
    }

    #[test]
    fn ctrl_k_at_first_project_is_noop() {
        let mut app = app_with_projects(3);
        app.focus = InputFocus::ProjectList;
        app.active_project_index = 0;
        app.handle_key(KeyCode::Char('k'), KeyModifiers::CONTROL);
        assert_eq!(app.active_project_index, 0);
    }

    // --- DB persistence tests ---

    #[test]
    fn load_persisted_state_empty_db_returns_none() {
        let app = App::new(24, 80, stub_backend(), test_db());
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

        // Session without claude_session_id — not resumable
        let session = sync::SharedSession {
            id: SessionId::default(),
            name: "1".to_string(),
            project_id: pid,
            role: "developer".to_string(),
            backend_id: "thurbox:@0".to_string(),
            backend_type: "tmux".to_string(),
            claude_session_id: None,
            cwd: None,
            additional_dirs: Vec::new(),
            worktree: None,
            tombstone: false,
            tombstone_at: None,
        };
        db.upsert_session(&session).unwrap();

        let app = App::new(24, 80, stub_backend(), db);
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
            claude_session_id: None,
            cwd: None,
            additional_dirs: Vec::new(),
            worktree: None,
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
            claude_session_id: Some("claude-abc".to_string()),
            cwd: None,
            additional_dirs: Vec::new(),
            worktree: None,
            tombstone: false,
            tombstone_at: None,
        };
        db.upsert_session(&s2).unwrap();
        db.set_session_counter(7).unwrap();

        let app = App::new(24, 80, stub_backend(), db);
        let (sessions, counter) = app.load_persisted_state_from_db().unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].name, "2");
        assert_eq!(counter, 7);
    }

    #[test]
    fn save_state_roundtrips_sessions() {
        let backend = stub_backend();
        let mut app = App::new(
            24,
            120,
            backend.clone(),
            test_db_with_project(&test_project_config()),
        );

        // Add a session
        let session = Session::stub("Session 1", &backend);
        let sid = session.info.id;
        app.sessions.push(session);
        app.projects[0].session_ids.push(sid);

        // Save to DB (only persists sessions + counter, not projects)
        app.save_state();

        // Verify session in DB
        let sessions = app.db.list_active_sessions().unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].name, "Session 1");
    }

    #[test]
    fn save_state_persists_session_counter() {
        let backend = stub_backend();
        let mut app = App::new(24, 120, backend.clone(), test_db());
        app.session_counter = 42;

        app.save_state();

        let counter = app.db.get_session_counter().unwrap();
        assert_eq!(counter, 42);
    }

    #[test]
    fn session_to_shared_converts_correctly() {
        let backend = stub_backend();
        let mut app = App::new(
            24,
            120,
            backend.clone(),
            test_db_with_project(&test_project_config()),
        );

        let mut session = Session::stub("TestSession", &backend);
        session.info.role = "reviewer".to_string();
        session.info.cwd = Some(PathBuf::from("/home/user"));
        session.info.claude_session_id = Some("claude-xyz".to_string());

        let sid = session.info.id;
        app.sessions.push(session);
        app.projects[0].session_ids.push(sid);

        let shared = app.session_to_shared(&app.sessions[0]);
        assert_eq!(shared.id, sid);
        assert_eq!(shared.name, "TestSession");
        assert_eq!(shared.role, "reviewer");
        assert_eq!(shared.cwd, Some(PathBuf::from("/home/user")));
        assert_eq!(shared.claude_session_id, Some("claude-xyz".to_string()));
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
        App::new(24, 120, stub_backend(), test_db_with_project(&config))
    }

    #[test]
    fn open_edit_project_populates_fields() {
        let mut app = app_with_project("my-proj", vec![PathBuf::from("/repo/a")]);
        app.open_edit_project_modal();
        assert!(app.show_edit_project_modal);
        assert_eq!(app.edit_project_name.value(), "my-proj");
        assert_eq!(app.edit_project_repos, vec![PathBuf::from("/repo/a")]);
        assert_eq!(app.edit_project_field, EditProjectField::Name);
        assert!(app.edit_project_original_id.is_some());
    }

    #[test]
    fn submit_edit_project_updates_name_and_repos() {
        let mut app = app_with_project("old-name", vec![PathBuf::from("/repo/a")]);
        let original_id = app.projects[0].id;

        app.open_edit_project_modal();
        app.edit_project_name.clear();
        app.edit_project_name.set("new-name");
        app.edit_project_repos = vec![PathBuf::from("/repo/b"), PathBuf::from("/repo/c")];
        app.submit_edit_project();

        assert!(!app.show_edit_project_modal);
        assert_eq!(app.projects[0].config.name, "new-name");
        assert_eq!(app.projects[0].config.repos.len(), 2);
        // ID must stay stable (no UUID regeneration)
        assert_eq!(app.projects[0].id, original_id);
    }

    #[test]
    fn submit_edit_project_rejects_empty_name() {
        let mut app = app_with_project("test", vec![PathBuf::from("/repo")]);
        app.open_edit_project_modal();
        app.edit_project_name.clear();
        app.submit_edit_project();

        // Modal should still be open
        assert!(app.show_edit_project_modal);
        assert!(app.status_message.is_some());
    }

    #[test]
    fn submit_edit_project_rejects_empty_repos() {
        let mut app = app_with_project("test", vec![PathBuf::from("/repo")]);
        app.open_edit_project_modal();
        app.edit_project_repos.clear();
        app.submit_edit_project();

        assert!(app.show_edit_project_modal);
        assert!(app.status_message.is_some());
    }

    #[test]
    fn submit_edit_project_auto_adds_pending_path() {
        let mut app = app_with_project("test", vec![PathBuf::from("/repo/a")]);
        app.open_edit_project_modal();
        app.edit_project_path.set("/repo/b");
        app.submit_edit_project();

        assert!(!app.show_edit_project_modal);
        assert_eq!(app.projects[0].config.repos.len(), 2);
        assert_eq!(app.projects[0].config.repos[1], PathBuf::from("/repo/b"));
    }

    #[test]
    fn close_edit_project_clears_all_state() {
        let mut app = app_with_project("test", vec![PathBuf::from("/repo")]);
        app.open_edit_project_modal();
        assert!(app.show_edit_project_modal);

        app.close_edit_project_modal();
        assert!(!app.show_edit_project_modal);
        assert_eq!(app.edit_project_name.value(), "");
        assert!(app.edit_project_repos.is_empty());
        assert!(app.edit_project_original_id.is_none());
    }

    #[test]
    fn edit_project_tab_cycles_through_all_fields() {
        let mut app = app_with_project("test", vec![PathBuf::from("/repo")]);
        app.open_edit_project_modal();

        // Name -> Path
        assert_eq!(app.edit_project_field, EditProjectField::Name);
        app.handle_key(KeyCode::Tab, KeyModifiers::NONE);
        assert_eq!(app.edit_project_field, EditProjectField::Path);

        // Path -> RepoList (repos not empty, no suggestion)
        app.handle_key(KeyCode::Tab, KeyModifiers::NONE);
        assert_eq!(app.edit_project_field, EditProjectField::RepoList);

        // RepoList -> Roles
        app.handle_key(KeyCode::Tab, KeyModifiers::NONE);
        assert_eq!(app.edit_project_field, EditProjectField::Roles);

        // Roles -> McpServers
        app.handle_key(KeyCode::Tab, KeyModifiers::NONE);
        assert_eq!(app.edit_project_field, EditProjectField::McpServers);

        // McpServers -> Name
        app.handle_key(KeyCode::Tab, KeyModifiers::NONE);
        assert_eq!(app.edit_project_field, EditProjectField::Name);
    }

    #[test]
    fn edit_project_tab_skips_repo_list_when_empty() {
        let mut app = app_with_project("test", vec![PathBuf::from("/repo")]);
        app.open_edit_project_modal();
        app.edit_project_repos.clear();

        // Name -> Path
        app.handle_key(KeyCode::Tab, KeyModifiers::NONE);
        assert_eq!(app.edit_project_field, EditProjectField::Path);

        // Path -> Roles (skip empty RepoList)
        app.handle_key(KeyCode::Tab, KeyModifiers::NONE);
        assert_eq!(app.edit_project_field, EditProjectField::Roles);
    }

    #[test]
    fn edit_project_esc_closes_modal() {
        let mut app = app_with_project("test", vec![PathBuf::from("/repo")]);
        app.open_edit_project_modal();
        app.handle_key(KeyCode::Esc, KeyModifiers::NONE);
        assert!(!app.show_edit_project_modal);
    }

    #[test]
    fn edit_project_repo_list_delete() {
        let mut app = app_with_project(
            "test",
            vec![PathBuf::from("/repo/a"), PathBuf::from("/repo/b")],
        );
        app.open_edit_project_modal();
        app.edit_project_field = EditProjectField::RepoList;
        app.edit_project_repo_index = 0;

        // Delete first repo
        app.handle_key(KeyCode::Char('d'), KeyModifiers::NONE);
        assert_eq!(app.edit_project_repos.len(), 1);
        assert_eq!(app.edit_project_repos[0], PathBuf::from("/repo/b"));
    }

    #[test]
    fn edit_project_repo_list_empty_after_delete_switches_to_path() {
        let mut app = app_with_project("test", vec![PathBuf::from("/repo/a")]);
        app.open_edit_project_modal();
        app.edit_project_field = EditProjectField::RepoList;

        app.handle_key(KeyCode::Char('d'), KeyModifiers::NONE);
        assert!(app.edit_project_repos.is_empty());
        assert_eq!(app.edit_project_field, EditProjectField::Path);
    }

    #[test]
    fn edit_project_id_stable_on_rename() {
        let mut app = app_with_project("alpha", vec![PathBuf::from("/repo")]);
        let id_before = app.projects[0].id;

        app.open_edit_project_modal();
        app.edit_project_name.clear();
        app.edit_project_name.set("beta");
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
        let backend = stub_backend();

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
        let mut app = App::new(24, 120, backend.clone(), db);

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
        let session = Session::stub("Session1", &backend);
        let session_id = session.info.id;
        app.sessions.push(session);
        app.projects[app.active_project_index]
            .session_ids
            .push(session_id);

        // Step 3: Rename "TestA" → "TestB" via edit modal
        app.open_edit_project_modal();
        assert!(app.show_edit_project_modal);
        app.edit_project_name.set("TestB");
        // Repos stay the same (pre-populated from open_edit_project_modal)
        app.submit_edit_project();
        assert!(
            !app.show_edit_project_modal,
            "Modal should close on success"
        );

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
        let app2 = App::new(24, 120, backend.clone(), app.db);

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
            claude_session_id: Some("claude-abc".to_string()),
            cwd: None,
            additional_dirs: Vec::new(),
            worktree: None,
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
        let backend = stub_backend();
        let mut app = App::new(
            24,
            120,
            backend.clone(),
            test_db_with_project(&test_project_config()),
        );

        let mut session = Session::stub("WTSession", &backend);
        session.info.worktree = Some(WorktreeInfo {
            repo_path: PathBuf::from("/repo"),
            worktree_path: PathBuf::from("/repo/.git/wt/feat"),
            branch: "feat".to_string(),
        });

        let sid = session.info.id;
        app.sessions.push(session);
        app.projects[0].session_ids.push(sid);

        let shared = app.session_to_shared(&app.sessions[0]);
        assert!(shared.worktree.is_some());
        let wt = shared.worktree.unwrap();
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
        App::new(24, 120, stub_backend(), test_db_with_project(&config))
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
        assert_eq!(app.role_editor_roles.len(), 1);
        assert_eq!(app.role_editor_roles[0].name, "dev");
        assert_eq!(app.role_editor_list_index, 0);
    }

    #[test]
    fn submit_edit_project_saves_roles() {
        use crate::session::{RoleConfig, RolePermissions};
        let mut app = app_with_roles(vec![]);
        app.open_edit_project_modal();
        // Add a role to the editor state
        app.role_editor_roles.push(RoleConfig {
            name: "new-role".to_string(),
            description: String::new(),
            permissions: RolePermissions::default(),
        });
        app.submit_edit_project();
        // Verify the project now has the role
        let project = app
            .projects
            .iter()
            .find(|p| p.config.name == "test")
            .unwrap();
        assert_eq!(project.config.roles.len(), 1);
        assert_eq!(project.config.roles[0].name, "new-role");
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
        assert!(!app.show_edit_project_modal);
        assert!(!app.show_role_editor);
        assert!(app.role_editor_roles.is_empty());
        assert_eq!(app.role_editor_list_index, 0);
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
        app.edit_project_field = EditProjectField::Roles;
        assert_eq!(app.role_editor_list_index, 0);

        // Navigate down
        app.handle_key(KeyCode::Char('j'), KeyModifiers::NONE);
        assert_eq!(app.role_editor_list_index, 1);

        // Navigate up
        app.handle_key(KeyCode::Char('k'), KeyModifiers::NONE);
        assert_eq!(app.role_editor_list_index, 0);

        // Delete first role
        app.handle_key(KeyCode::Char('d'), KeyModifiers::NONE);
        assert_eq!(app.role_editor_roles.len(), 1);
        assert_eq!(app.role_editor_roles[0].name, "b");
    }

    #[test]
    fn edit_project_roles_add_opens_role_editor() {
        let mut app = app_with_roles(vec![]);
        app.open_edit_project_modal();
        app.edit_project_field = EditProjectField::Roles;
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
        app.edit_project_field = EditProjectField::Roles;
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
        app.edit_project_field = EditProjectField::Roles;
        // Add a role directly to the editor state
        app.role_editor_roles.push(RoleConfig {
            name: "added".to_string(),
            description: String::new(),
            permissions: RolePermissions::default(),
        });
        // Esc from Roles field triggers submit_edit_project (saves)
        app.handle_key(KeyCode::Esc, KeyModifiers::NONE);
        assert!(!app.show_edit_project_modal);
        let project = app
            .projects
            .iter()
            .find(|p| p.config.name == "test")
            .unwrap();
        assert_eq!(project.config.roles.len(), 1);
        assert_eq!(project.config.roles[0].name, "added");
    }

    #[test]
    fn ctrl_r_no_op_without_claude_session_id() {
        let mut app = app_with_sessions(1);
        // Session exists but has no claude_session_id
        app.sessions[0].info.claude_session_id = None;
        app.focus = InputFocus::Terminal;
        app.handle_key(KeyCode::Char('r'), KeyModifiers::CONTROL);
        // Should be a no-op (no error, no crash)
        assert!(app.status_message.is_none());
    }

    #[test]
    fn session_to_shared_maps_additional_dirs() {
        let backend = stub_backend();
        let mut app = App::new(
            24,
            120,
            backend.clone(),
            test_db_with_project(&test_project_config()),
        );

        let mut session = Session::stub("MultiDir", &backend);
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
    fn user_session_count_excludes_admin_project() {
        let backend = stub_backend();
        let config = ProjectConfig {
            name: "UserProj".to_string(),
            repos: vec![PathBuf::from("/repo")],
            roles: Vec::new(),
            mcp_servers: Vec::new(),
            id: None,
        };
        let mut app = App::new(24, 120, backend.clone(), test_db_with_project(&config));

        // User project has no sessions
        assert_eq!(app.user_session_count(), 0);

        // Add a session to the user project
        let session = Session::stub("user-1", &backend);
        let sid = session.info.id;
        app.sessions.push(session);
        app.projects[0].session_ids.push(sid);
        assert_eq!(app.user_session_count(), 1);

        // Add an admin project with a session
        let admin_config = ProjectConfig {
            name: "Admin".to_string(),
            repos: vec![],
            roles: Vec::new(),
            mcp_servers: Vec::new(),
            id: None,
        };
        let mut admin_project = ProjectInfo::new_admin(admin_config);
        let admin_session = Session::stub("admin", &backend);
        let admin_sid = admin_session.info.id;
        app.sessions.push(admin_session);
        admin_project.session_ids.push(admin_sid);
        app.projects.insert(0, admin_project);

        // Admin sessions should not count
        assert_eq!(app.user_session_count(), 1);
    }

    #[test]
    fn cannot_edit_admin_project() {
        let backend = stub_backend();
        let mut app = App::new(24, 120, backend, test_db());

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
        assert!(!app.show_edit_project_modal);
        assert_eq!(
            app.status_message.as_ref().map(|m| m.text.as_str()),
            Some("Cannot edit admin project")
        );
    }

    #[test]
    fn cannot_delete_admin_project() {
        let backend = stub_backend();
        let mut app = App::new(24, 120, backend, test_db());

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
        assert!(!app.show_delete_project_modal_flag);
        assert_eq!(
            app.status_message.as_ref().map(|m| m.text.as_str()),
            Some("Cannot delete admin project")
        );
    }

    #[test]
    fn cannot_close_admin_session() {
        let backend = stub_backend();
        let mut app = App::new(24, 120, backend.clone(), test_db());

        // Add an admin project with a session and select it
        let mut admin_project = ProjectInfo::new_admin(ProjectConfig {
            name: "Admin".to_string(),
            repos: vec![],
            roles: Vec::new(),
            mcp_servers: Vec::new(),
            id: None,
        });
        let session = Session::stub("admin", &backend);
        let sid = session.info.id;
        app.sessions.push(session);
        admin_project.session_ids.push(sid);
        app.projects.push(admin_project);
        app.active_project_index = app.projects.len() - 1;
        app.active_index = 0;

        app.handle_key(KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert_eq!(app.sessions.len(), 1); // Session not closed
        assert_eq!(
            app.status_message.as_ref().map(|m| m.text.as_str()),
            Some("Cannot close admin session")
        );
    }

    // --- StatusMessage / set_error / set_status tests ---

    #[test]
    fn set_error_creates_error_status() {
        let mut app = App::new(24, 80, stub_backend(), test_db());
        app.set_error("something failed");
        let msg = app.status_message.as_ref().unwrap();
        assert_eq!(msg.level, StatusLevel::Error);
        assert_eq!(msg.text, "something failed");
    }

    #[test]
    fn set_status_creates_typed_status() {
        let mut app = App::new(24, 80, stub_backend(), test_db());
        app.set_status(StatusLevel::Success, "all good".into());
        let msg = app.status_message.as_ref().unwrap();
        assert_eq!(msg.level, StatusLevel::Success);
        assert_eq!(msg.text, "all good");
    }

    #[test]
    fn set_status_replaces_previous() {
        let mut app = App::new(24, 80, stub_backend(), test_db());
        app.set_error("old error");
        app.set_status(StatusLevel::Info, "new info".into());
        let msg = app.status_message.as_ref().unwrap();
        assert_eq!(msg.level, StatusLevel::Info);
        assert_eq!(msg.text, "new info");
    }

    // --- Worktree sync tests ---

    #[test]
    fn start_sync_with_no_worktrees_shows_info() {
        let mut app = App::new(24, 80, stub_backend(), test_db());
        app.start_sync();
        assert!(!app.worktree_sync_in_progress);
        let msg = app.status_message.as_ref().unwrap();
        assert_eq!(msg.level, StatusLevel::Info);
        assert_eq!(msg.text, "No worktrees to sync");
    }

    #[test]
    fn start_sync_ignores_if_already_in_progress() {
        let mut app = App::new(24, 80, stub_backend(), test_db());
        app.worktree_sync_in_progress = true;
        app.status_message = None;
        app.start_sync();
        // Should not set any new status message
        assert!(app.status_message.is_none());
    }

    #[test]
    fn ctrl_s_triggers_start_sync() {
        let mut app = App::new(24, 80, stub_backend(), test_db());
        app.handle_key(KeyCode::Char('s'), KeyModifiers::CONTROL);
        // No worktrees → info message
        let msg = app.status_message.as_ref().unwrap();
        assert_eq!(msg.text, "No worktrees to sync");
    }

    #[test]
    fn start_sync_with_worktree_sessions_sets_in_progress() {
        let backend = stub_backend();
        let config = test_project_config();
        let mut app = App::new(24, 120, backend.clone(), test_db_with_project(&config));
        let mut session = Session::stub("wt-session", &backend);
        session.info.worktree = Some(WorktreeInfo {
            repo_path: PathBuf::from("/tmp/nonexistent-repo"),
            worktree_path: PathBuf::from("/tmp/nonexistent-wt"),
            branch: "test-branch".to_string(),
        });
        let session_id = session.info.id;
        app.sessions.push(session);
        app.projects[0].session_ids.push(session_id);

        app.start_sync();
        assert!(app.worktree_sync_in_progress);
        assert_eq!(app.worktree_sync_pending, 1);
        let msg = app.status_message.as_ref().unwrap();
        assert_eq!(msg.level, StatusLevel::Info);
        assert!(msg.text.contains("Syncing 1 worktree"));
    }

    #[test]
    fn tick_increments_tick_count() {
        let mut app = App::new(24, 80, stub_backend(), test_db());
        assert_eq!(app.tick_count, 0);
        app.tick();
        assert_eq!(app.tick_count, 1);
        app.tick();
        assert_eq!(app.tick_count, 2);
    }

    #[test]
    fn finish_sync_all_synced_shows_success() {
        let mut app = App::new(24, 80, stub_backend(), test_db());
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
        let mut app = App::new(24, 80, stub_backend(), test_db());
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
        let mut app = App::new(24, 80, stub_backend(), test_db());
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
        let mut app = App::new(24, 80, stub_backend(), test_db());
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
        let mut app = App::new(24, 80, stub_backend(), test_db());
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
        let mut app = App::new(24, 80, stub_backend(), test_db());
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
        let mut app = App::new(24, 80, stub_backend(), test_db());
        app.send_conflict_prompt(SessionId::default());
        assert!(app.deferred_inputs.is_empty());
    }

    #[test]
    fn send_conflict_prompt_no_deferred_when_send_fails() {
        let backend = stub_backend();
        let mut app = App::new(24, 80, backend.clone(), test_db());
        let session = Session::stub("test", &backend);
        let sid = session.info.id;
        app.sessions.push(session);

        // Stub's channel rx is dropped, so send_input fails.
        // No deferred input should be created.
        app.send_conflict_prompt(sid);
        assert!(app.deferred_inputs.is_empty());
    }

    #[test]
    fn poll_sync_results_triggers_finish_when_all_received() {
        let mut app = App::new(24, 80, stub_backend(), test_db());
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
        let mut app = App::new(24, 80, stub_backend(), test_db());
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
        let config_b = ProjectConfig {
            name: "Other".to_string(),
            repos: vec![PathBuf::from("/other")],
            ..test_project_config()
        };
        let mut app = App::new(24, 120, backend, test_db());
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
        let mut app = App::new(24, 120, backend, test_db());
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
        let mut app = App::new(24, 120, backend, test_db());
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
        let mut app = App::new(24, 120, backend, test_db());
        app.projects.push(ProjectInfo::new(test_project_config()));
        app.active_project_index = 0;

        let perms = app.resolve_role_permissions_for_project("nonexistent", 0);
        assert_eq!(perms, RolePermissions::default());
    }

    #[test]
    fn resolve_role_permissions_returns_default_for_invalid_index() {
        use crate::session::RolePermissions;
        let backend = stub_backend();
        let app = App::new(24, 120, backend, test_db());

        let perms = app.resolve_role_permissions_for_project("any-role", 999);
        assert_eq!(perms, RolePermissions::default());
    }
}
