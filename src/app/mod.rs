mod key_handlers;
mod modals;
mod state;

use std::path::PathBuf;
use std::sync::Arc;

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
use crate::project::{self, ProjectConfig, ProjectId, ProjectInfo};
use crate::session::{
    PersistedSession, PersistedState, PersistedWorktree, RoleConfig, RolePermissions,
    SessionConfig, SessionId, SessionInfo, SessionStatus, WorktreeInfo, DEFAULT_ROLE_NAME,
};
use crate::sync::{self, StateDelta, SyncState};
use crate::ui::{
    add_project_modal, branch_selector_modal, delete_project_modal, info_panel, layout,
    project_list, repo_selector_modal, role_editor_modal, role_selector_modal, session_mode_modal,
    status_bar, terminal_view, worktree_name_modal,
};

const MOUSE_SCROLL_LINES: usize = 3;

/// If no output for this many milliseconds, consider session "Waiting".
const ACTIVITY_TIMEOUT_MS: u64 = 1000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoleEditorView {
    List,
    Editor,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AddProjectField {
    Name,
    Path,
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
    pub(crate) focus: InputFocus,
    pub(crate) should_quit: bool,
    pub(crate) error_message: Option<String>,
    terminal_rows: u16,
    pub(crate) terminal_cols: u16,
    session_counter: usize,
    pub(crate) show_info_panel: bool,
    pub(crate) show_help: bool,
    pub(crate) show_add_project_modal: bool,
    pub(crate) add_project_name: TextInput,
    pub(crate) add_project_path: TextInput,
    pub(crate) add_project_field: AddProjectField,
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
    sync_state: SyncState,
}

/// Convert a SharedProject to ProjectInfo, preserving the shared state ID.
fn shared_project_to_info(sp: sync::SharedProject) -> ProjectInfo {
    let config = ProjectConfig {
        name: sp.name,
        repos: sp.repos,
        roles: Vec::new(),
    };
    let mut info = ProjectInfo::new(config);
    info.id = sp.id;
    info
}

/// Load and merge projects from shared state and config.
/// Shared state is the source of truth; config provides roles.
fn load_and_merge_projects(
    sync_state_path: &std::path::Path,
    project_configs: Vec<ProjectConfig>,
) -> Vec<ProjectInfo> {
    let shared_projects = sync::file_store::load_shared_state(sync_state_path)
        .map(|state| state.projects)
        .unwrap_or_default();

    if !shared_projects.is_empty() {
        let mut result: Vec<ProjectInfo> = shared_projects
            .into_iter()
            .map(shared_project_to_info)
            .collect();

        for config in &project_configs {
            if let Some(project) = result.iter_mut().find(|p| p.config.name == config.name) {
                project.config.roles = config.roles.clone();
            } else {
                result.push(ProjectInfo::new(config.clone()));
            }
        }
        result
    } else if !project_configs.is_empty() {
        project_configs.into_iter().map(ProjectInfo::new).collect()
    } else {
        vec![ProjectInfo::new_default(project::create_default_project())]
    }
}

impl App {
    pub fn new(
        rows: u16,
        cols: u16,
        project_configs: Vec<ProjectConfig>,
        backend: Arc<dyn SessionBackend>,
    ) -> Self {
        // Initialize sync state first so we can load projects from it
        let sync_state_path = sync::shared_state_path().unwrap_or_else(|_| {
            let mut p = PathBuf::from(std::env::var_os("HOME").unwrap_or_default());
            p.push(".local");
            p.push("share");
            p.push("thurbox");
            p.push("shared_state.toml");
            p
        });

        let mut projects = load_and_merge_projects(&sync_state_path, project_configs);

        // Ensure projects is never empty (fallback to default if needed)
        if projects.is_empty() {
            projects = vec![ProjectInfo::new_default(project::create_default_project())];
        }

        // Create sync state using the path we determined above
        let sync_state = SyncState::new(sync_state_path);

        Self {
            projects,
            active_project_index: 0,
            sessions: Vec::new(),
            active_index: 0,
            backend,
            focus: InputFocus::ProjectList,
            should_quit: false,
            error_message: None,
            terminal_rows: rows,
            terminal_cols: cols,
            session_counter: 0,
            show_info_panel: false,
            show_help: false,
            show_add_project_modal: false,
            add_project_name: TextInput::new(),
            add_project_path: TextInput::new(),
            add_project_field: AddProjectField::Name,
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
            sync_state,
        }
    }

    /// Load and merge projects from shared state at startup.
    /// This ensures all projects synced by other instances are available locally.
    pub fn merge_projects_from_shared_state(&mut self) {
        let shared_state = match sync::file_store::load_shared_state(self.sync_state.path()) {
            Ok(state) => state,
            Err(_) => return, // No shared state file yet, that's fine
        };

        // Build map of existing projects by ID
        let existing_ids: std::collections::HashMap<ProjectId, usize> = self
            .projects
            .iter()
            .enumerate()
            .map(|(i, p)| (p.id, i))
            .collect();

        // Add projects from shared state that aren't already loaded
        for shared_project in shared_state.projects {
            if !existing_ids.contains_key(&shared_project.id) {
                let project = shared_project_to_info(shared_project.clone());
                self.projects.push(project);

                tracing::debug!(
                    "Loaded project {} from shared state at startup",
                    shared_project.id
                );
            }
        }
    }

    pub fn spawn_session(&mut self) {
        let Some(project) = self.active_project() else {
            return;
        };
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
                self.repo_selector_index = 0;
                self.show_repo_selector = true;
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
                self.do_spawn_session(name, &config, worktree);
            }
            1 => {
                // Exactly one role — auto-assign it.
                config.role = roles[0].name.clone();
                config.permissions = roles[0].permissions.clone();
                self.do_spawn_session(name, &config, worktree);
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

    fn close_active_session(&mut self) {
        if self.sessions.is_empty() {
            return;
        }

        let Some(session) = self.sessions.get(self.active_index) else {
            return;
        };

        let session_id = session.info.id;

        // Clean up worktree if present
        if let Some(wt) = &session.info.worktree {
            if let Err(e) = git::remove_worktree(&wt.repo_path, &wt.worktree_path) {
                error!("Failed to remove worktree: {e}");
            }
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
        self.save_shared_state();
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
    /// Tries to add to the session's original project, falling back to the default project.
    fn associate_session_with_project(&mut self, session_id: SessionId, project_id: ProjectId) {
        let mut found_project = false;
        if let Some(project) = self.projects.iter_mut().find(|p| p.id == project_id) {
            if !project.session_ids.contains(&session_id) {
                project.session_ids.push(session_id);
            }
            found_project = true;
        }

        // If session's project doesn't exist in this instance, add to the default/first project
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
            self.error_message = Some("Role name cannot be empty".to_string());
            return;
        }

        // Check uniqueness (exclude the role being edited)
        let duplicate = self
            .role_editor_roles
            .iter()
            .enumerate()
            .any(|(i, r)| r.name == name && Some(i) != self.role_editor_editing_index);
        if duplicate {
            self.error_message = Some(format!("Role name '{name}' already exists"));
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

        self.error_message = None;
        self.role_editor_view = RoleEditorView::List;
    }

    pub(crate) fn save_project_configs_to_disk(&self) {
        let configs: Vec<ProjectConfig> = self
            .projects
            .iter()
            .filter(|p| !p.is_default)
            .map(|p| p.config.clone())
            .collect();
        if let Err(e) = project::save_project_configs(&configs) {
            error!("Failed to save config: {e}");
        }
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
                self.error_message = Some(format!("Failed to create worktree: {e:#}"));
            }
        }
    }

    pub(crate) fn do_spawn_session(
        &mut self,
        name: String,
        config: &SessionConfig,
        worktree: Option<WorktreeInfo>,
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
                self.error_message = None;

                // Only add to project if not already there
                if let Some(project) = self.projects.get_mut(self.active_project_index) {
                    if !project.session_ids.contains(&session_id) {
                        project.session_ids.push(session_id);
                    }
                }

                // Sync to shared state for other instances
                self.save_shared_state();
            }
            Err(e) => {
                error!("Failed to spawn session: {e}");
                self.error_message = Some(format!("Failed to start claude: {e:#}"));
            }
        }
    }

    pub(crate) fn submit_add_project(&mut self) {
        let name = self.add_project_name.value().trim().to_string();
        let path = self.add_project_path.value().trim().to_string();

        if name.is_empty() || path.is_empty() {
            self.error_message = Some("Project name and path cannot be empty".to_string());
            return;
        }

        let config = ProjectConfig {
            name,
            repos: vec![PathBuf::from(path)],
            roles: Vec::new(),
        };
        let info = ProjectInfo::new(config);
        self.projects.push(info);
        self.active_project_index = self.projects.len() - 1;

        // Persist to config file (exclude default projects)
        let configs: Vec<ProjectConfig> = self
            .projects
            .iter()
            .filter(|p| !p.is_default)
            .map(|p| p.config.clone())
            .collect();
        if let Err(e) = project::save_project_configs(&configs) {
            error!("Failed to save config: {e}");
            self.error_message = Some(format!("Project added but failed to save config: {e}"));
        } else {
            self.error_message = None;
            // Sync new project to shared state for multi-instance visibility
            self.save_shared_state();
        }

        // Close modal and clear inputs
        self.show_add_project_modal = false;
        self.add_project_name.clear();
        self.add_project_path.clear();
    }

    pub(crate) fn show_delete_project_modal(&mut self) {
        let Some(project) = self.active_project() else {
            return;
        };

        // Safety checks
        if project.is_default {
            self.error_message = Some("Cannot delete default project".into());
            return;
        }
        if self.projects.len() <= 1 {
            self.error_message = Some("Cannot delete last project".into());
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

        // Get project session IDs and name before removal
        let Some(project) = self.active_project() else {
            self.show_delete_project_modal_flag = false;
            return;
        };

        let session_ids_to_close: Vec<_> = project.session_ids.clone();
        let project_name = project.config.name.clone();

        // Close all sessions belonging to this project
        for session_id in session_ids_to_close {
            if let Some(session_pos) = self.sessions.iter().position(|s| s.info.id == session_id) {
                // Kill backend session
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

        // Persist changes
        self.save_project_configs_to_disk();

        // Sync deletion to other instances
        self.save_shared_state();

        // Close modal and show success
        self.show_delete_project_modal_flag = false;
        self.error_message = Some(format!("Deleted project '{}'", project_name));
    }

    /// When switching projects, select the first session of the new project.
    pub(crate) fn sync_active_session_to_project(&mut self) {
        let project_sessions = self.active_project_sessions();
        if let Some(&first) = project_sessions.first() {
            self.active_index = first;
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
        for session in &mut self.sessions {
            session.info.status = if session.has_exited() {
                SessionStatus::Idle
            } else if session.millis_since_last_output() > ACTIVITY_TIMEOUT_MS {
                SessionStatus::Waiting
            } else {
                SessionStatus::Busy
            };
        }

        // Poll for external state changes from other thurbox instances
        if let Ok(Some(delta)) = sync::poll_for_changes(&mut self.sync_state) {
            self.handle_external_state_change(delta);
        }
    }

    /// Handle external state changes detected from other instances.
    fn handle_external_state_change(&mut self, delta: StateDelta) {
        // Update session counter to avoid conflicts
        self.session_counter = self.session_counter.max(delta.counter_increment);

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
                tracing::debug!("Updated project {} from external state", project_name);
            }
        }

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
                info_panel::render_info_panel(
                    frame,
                    info_area,
                    &session.info,
                    active_project.map(|p| &p.config),
                );
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
            self.sessions.len(),
            self.projects.len(),
            self.error_message.as_deref(),
            focus_label,
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
                    focused_field: self.add_project_field,
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

        // Role editor modal
        if self.show_role_editor {
            match self.role_editor_view {
                RoleEditorView::List => {
                    role_editor_modal::render_role_list_modal(
                        frame,
                        &role_editor_modal::RoleListState {
                            roles: &self.role_editor_roles,
                            selected_index: self.role_editor_list_index,
                        },
                    );
                }
                RoleEditorView::Editor => {
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
                            allowed_tools_input_cursor: self
                                .role_editor_allowed_tools
                                .input
                                .cursor_pos(),
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
            }
        }
    }

    pub fn should_quit(&self) -> bool {
        self.should_quit
    }

    pub fn shutdown(self) {
        self.save_state();
        // Also save shared state for multi-instance sync
        self.save_shared_state();
        // Do NOT remove worktrees — they persist for resume.
        // Detach from backend sessions without killing them — they persist in tmux.
        for session in self.sessions {
            session.detach();
        }
    }

    fn save_state(&self) {
        let sessions: Vec<PersistedSession> = self
            .sessions
            .iter()
            .filter_map(|s| {
                let claude_session_id = s.info.claude_session_id.as_ref()?;
                // Find which project this session belongs to
                let project_id = self
                    .projects
                    .iter()
                    .find(|p| p.session_ids.contains(&s.info.id))
                    .map(|p| p.id.as_uuid()); // Extract UUID from ProjectId
                Some(PersistedSession {
                    id: Some(s.info.id),
                    name: s.info.name.clone(),
                    claude_session_id: claude_session_id.clone(),
                    cwd: s.info.cwd.clone(),
                    worktree: s.info.worktree.as_ref().map(|wt| PersistedWorktree {
                        repo_path: wt.repo_path.clone(),
                        worktree_path: wt.worktree_path.clone(),
                        branch: wt.branch.clone(),
                    }),
                    role: s.info.role.clone(),
                    backend_id: s.backend_id().to_string(),
                    backend_type: s.backend_name().to_string(),
                    project_id,
                })
            })
            .collect();

        let state = PersistedState {
            sessions,
            session_counter: self.session_counter,
        };

        if let Err(e) = project::save_session_state(&state) {
            error!("Failed to save session state: {e}");
        }
    }

    /// Save current sessions to the shared state file for multi-instance sync.
    ///
    /// Merges local sessions into the existing shared state instead of overwriting it.
    /// This preserves sessions from other instances.
    fn save_shared_state(&self) {
        // Load existing shared state (or create new if it doesn't exist)
        let mut shared_state = sync::file_store::load_shared_state(self.sync_state.path())
            .unwrap_or_else(|_| sync::SharedState::new());

        // Build map of our local sessions for quick lookup
        let mut local_sessions = std::collections::HashMap::new();
        for session in &self.sessions {
            let shared_session = sync::SharedSession {
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
            };
            local_sessions.insert(session.info.id, shared_session);
        }

        // Merge: update existing sessions or add new ones
        let mut merged_sessions = Vec::new();
        for mut existing in shared_state.sessions {
            if let Some(updated) = local_sessions.remove(&existing.id) {
                merged_sessions.push(updated);
            } else if existing.tombstone {
                merged_sessions.push(existing);
            } else {
                // Session was deleted locally. Mark as tombstone for propagation to other instances.
                existing.tombstone = true;
                existing.tombstone_at = Some(sync::current_time_millis());
                merged_sessions.push(existing);
            }
        }

        // Add new sessions (only in local state, not yet in shared state)
        for (_session_id, session) in local_sessions {
            merged_sessions.push(session);
        }

        // Sync projects: create SharedProject for each local project
        let mut local_projects = std::collections::HashMap::new();
        for project in &self.projects {
            let shared_project = sync::SharedProject {
                id: project.id,
                name: project.config.name.clone(),
                repos: project.config.repos.clone(),
            };
            local_projects.insert(project.id, shared_project);
        }

        // Merge projects: only include projects that exist in local state.
        // Deleted projects are simply removed from shared state (not kept from other instances).
        // This ensures deletions propagate across all instances.
        let mut merged_projects = Vec::new();
        for existing in shared_state.projects {
            if let Some(updated) = local_projects.remove(&existing.id) {
                merged_projects.push(updated);
            }
            // Projects not in local_projects are treated as deleted and omitted
        }

        // Add new projects (only in local state, not yet in shared state)
        for (_project_id, project) in local_projects {
            merged_projects.push(project);
        }

        // Update state with merged sessions, projects and our counter
        shared_state.sessions = merged_sessions;
        shared_state.projects = merged_projects;
        shared_state.session_counter = self.session_counter.max(shared_state.session_counter);
        shared_state.last_modified = sync::current_time_millis();

        if let Err(e) = sync::file_store::save_shared_state(self.sync_state.path(), &shared_state) {
            error!("Failed to save shared state: {e}");
        }
    }

    pub fn restore_sessions(&mut self, state: PersistedState) {
        self.session_counter = state.session_counter;

        // Load shared state to fill in missing project_id for backward compat
        // (old persisted sessions don't have project_id field)
        let shared_state = sync::file_store::load_shared_state(self.sync_state.path())
            .unwrap_or_else(|_| sync::SharedState::default());

        // Discover existing sessions from the backend.
        let discovered = self.backend.discover().unwrap_or_default();

        for persisted in state.sessions {
            let name = persisted.name;

            let role = if persisted.role.is_empty() {
                DEFAULT_ROLE_NAME.to_string()
            } else {
                persisted.role
            };

            let worktree = persisted.worktree.map(|wt| WorktreeInfo {
                repo_path: wt.repo_path,
                worktree_path: wt.worktree_path,
                branch: wt.branch,
            });

            // Try to match a discovered backend session by backend_id.
            let matching_discovered = if !persisted.backend_id.is_empty() {
                discovered
                    .iter()
                    .find(|d| d.backend_id == persisted.backend_id && d.is_alive)
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
                // Restore the persisted SessionId if available (preserves identity across restarts)
                if let Some(persisted_id) = persisted.id {
                    session.info.id = persisted_id;
                }

                session.info.claude_session_id = Some(persisted.claude_session_id.clone());
                session.info.cwd = persisted.cwd;
                session.info.role = role;
                session.info.worktree = worktree;
                let session_id = session.info.id;
                self.sessions.push(session);
                self.active_index = self.sessions.len() - 1;
                self.focus = InputFocus::Terminal;

                // Associate with the original project (restored sessions preserve their project)
                // Use persisted project_id if available, otherwise look in shared state for backward compat
                let project_id_to_lookup = if let Some(persisted_proj_id) = persisted.project_id {
                    Some(persisted_proj_id)
                } else {
                    // No project_id in persisted session (old session), look in shared state
                    shared_state
                        .sessions
                        .iter()
                        .find(|s| s.id == session_id)
                        .map(|s| s.project_id.as_uuid())
                };

                let target_project_index = if let Some(proj_uuid) = project_id_to_lookup {
                    // Try to find the project by UUID
                    let found_index = self
                        .projects
                        .iter()
                        .position(|p| p.id.as_uuid() == proj_uuid);

                    if let Some(idx) = found_index {
                        if let Some(proj) = self.projects.get(idx) {
                            tracing::debug!(
                                session = %session_id,
                                project_uuid = %proj_uuid,
                                project_index = idx,
                                project_name = %proj.config.name,
                                "Restored session to original project"
                            );
                        }
                        idx
                    } else {
                        tracing::warn!(
                            session = %session_id,
                            project_uuid = %proj_uuid,
                            available_projects = ?self.projects.iter().map(|p| (p.id.as_uuid(), p.config.name.clone())).collect::<Vec<_>>(),
                            fallback_index = self.active_project_index,
                            "Session project_id not found, falling back to active project"
                        );
                        self.active_project_index
                    }
                } else {
                    tracing::debug!(
                        session = %session_id,
                        "No project_id persisted or in shared state for session, using active project"
                    );
                    // Fallback to active project if no project_id found anywhere
                    self.active_project_index
                };

                if let Some(project) = self.projects.get_mut(target_project_index) {
                    if !project.session_ids.contains(&session_id) {
                        project.session_ids.push(session_id);
                    }
                }
            } else {
                // No matching backend session or adopt failed — spawn new with --resume.
                let permissions = self.resolve_role_permissions(&role);
                let config = SessionConfig {
                    resume_session_id: Some(persisted.claude_session_id.clone()),
                    claude_session_id: Some(persisted.claude_session_id),
                    cwd: persisted.cwd,
                    role,
                    permissions,
                };
                self.do_spawn_session(name, &config, worktree);
            }
        }

        if let Err(e) = project::clear_session_state() {
            error!("Failed to clear session state after restore: {e}");
        }

        // Claim ownership of restored sessions in the shared state
        // This ensures sessions stay visible to other instances and persist across restarts
        self.save_shared_state();
    }

    /// Resolve a role name to its permissions using the active project's role config.
    fn resolve_role_permissions(&self, role_name: &str) -> RolePermissions {
        self.active_project()
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
        Line::from(Span::styled(
            "Global",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )),
        help_line("Ctrl+Q", "Quit"),
        help_line(
            "Ctrl+N",
            "New project (project focus) / session (normal or worktree)",
        ),
        help_line("Ctrl+J / Ctrl+K", "Next / previous session"),
        help_line("Ctrl+X", "Close active session"),
        help_line("Ctrl+L", "Cycle focus (project / session / terminal)"),
        help_line("Ctrl+I", "Toggle info panel (width >= 120)"),
        help_line("?", "Show this help (list focus only)"),
        Line::from(""),
        Line::from(Span::styled(
            "Project List (when focused)",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )),
        help_line("j / Down", "Next project"),
        help_line("k / Up", "Previous project"),
        help_line("d", "Delete selected project"),
        help_line("r", "Edit project roles"),
        help_line("Enter", "Focus session list"),
        Line::from(""),
        Line::from(Span::styled(
            "Session List (when focused)",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )),
        help_line("j / Down", "Next session"),
        help_line("k / Up", "Previous session"),
        help_line("Enter", "Focus terminal"),
        Line::from(""),
        Line::from(Span::styled(
            "Terminal (when focused)",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )),
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

    /// Clear the test's shared state file to avoid stale projects from previous runs.
    /// This is needed because load_and_merge_projects() prefers shared state over config.
    fn clear_test_shared_state() {
        if let Ok(path) = sync::shared_state_path() {
            let _ = std::fs::remove_file(path);
        }
    }

    /// Create an App with N stub sessions bound to the default project.
    fn app_with_sessions(count: usize) -> App {
        let backend = stub_backend();
        let mut app = App::new(24, 120, vec![], backend.clone());
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
        let app = App::new(50, 100, vec![], stub_backend());
        // rows = 50 - 4 = 46, half = 23
        assert_eq!(app.page_scroll_amount(), 23);
    }

    #[test]
    fn page_scroll_amount_small_terminal() {
        let app = App::new(6, 80, vec![], stub_backend());
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
        let mut app = App::new(24, 80, vec![], stub_backend());
        assert_eq!(app.next_session_name(), "1");
    }

    #[test]
    fn next_session_name_increments() {
        let mut app = App::new(24, 80, vec![], stub_backend());
        assert_eq!(app.next_session_name(), "1");
        assert_eq!(app.next_session_name(), "2");
        assert_eq!(app.next_session_name(), "3");
    }

    #[test]
    fn next_session_name_continues_from_restored_counter() {
        let mut app = App::new(24, 80, vec![], stub_backend());
        app.session_counter = 5;
        assert_eq!(app.next_session_name(), "6");
    }

    // --- Role editor tests ---

    #[test]
    fn open_role_editor_starts_empty_for_no_custom_roles() {
        let mut app = App::new(24, 120, vec![], stub_backend());
        app.open_role_editor();
        assert!(app.show_role_editor);
        assert!(app.role_editor_roles.is_empty());
        assert_eq!(app.role_editor_view, RoleEditorView::List);
    }

    #[test]
    fn open_role_editor_clones_existing_roles() {
        use crate::session::{RoleConfig, RolePermissions};
        clear_test_shared_state();
        let config = ProjectConfig {
            name: "test".to_string(),
            repos: vec![],
            roles: vec![RoleConfig {
                name: "ops".to_string(),
                description: "Operations".to_string(),
                permissions: RolePermissions::default(),
            }],
        };
        let mut app = App::new(24, 120, vec![config], stub_backend());
        app.open_role_editor();
        assert_eq!(app.role_editor_roles.len(), 1);
        assert_eq!(app.role_editor_roles[0].name, "ops");
    }

    #[test]
    fn role_editor_submit_uses_allowed_tools_list() {
        let mut app = App::new(24, 120, vec![], stub_backend());
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
        let mut app = App::new(24, 120, vec![], stub_backend());
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
        clear_test_shared_state();
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
        };
        let mut app = App::new(24, 120, vec![config], stub_backend());
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
        };
        let app = App::new(24, 120, vec![config], stub_backend());
        // With no roles, the selector should never be set
        assert!(!app.show_role_selector);
    }

    #[test]
    fn role_editor_name_validation_rejects_empty() {
        let mut app = App::new(24, 120, vec![], stub_backend());
        app.open_role_editor();
        app.handle_role_editor_list_key(KeyCode::Char('a'));
        // Try to submit with empty name
        app.submit_role_editor();
        assert!(app.error_message.is_some());
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
        let mut app = App::new(24, 120, vec![], stub_backend());
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
        assert!(app.error_message.is_some());
        assert!(app
            .error_message
            .as_ref()
            .unwrap()
            .contains("already exists"));
        // Should still be in editor view, role count unchanged
        assert_eq!(app.role_editor_view, RoleEditorView::Editor);
        assert_eq!(app.role_editor_roles.len(), 1);
    }

    #[test]
    fn role_editor_edit_preserves_permission_mode_and_tools() {
        use crate::session::{RoleConfig, RolePermissions};
        clear_test_shared_state();
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
        };
        let mut app = App::new(24, 120, vec![config], stub_backend());
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
        let mut app = App::new(24, 120, vec![], stub_backend());
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
        clear_test_shared_state();
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
        };
        let mut app = App::new(24, 120, vec![config], stub_backend());
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
        let mut app = App::new(24, 120, vec![], stub_backend());
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
        let mut app = App::new(24, 120, vec![], stub_backend());
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
    fn role_editor_esc_discards_and_returns_to_list() {
        let mut app = App::new(24, 120, vec![], stub_backend());
        app.open_role_editor();
        app.handle_role_editor_list_key(KeyCode::Char('a'));
        assert_eq!(app.role_editor_view, RoleEditorView::Editor);

        app.handle_role_editor_editor_key(KeyCode::Esc);
        assert_eq!(app.role_editor_view, RoleEditorView::List);
    }

    #[test]
    fn role_editor_delete_adjusts_list_index() {
        use crate::session::{RoleConfig, RolePermissions};
        clear_test_shared_state();
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
        };
        let mut app = App::new(24, 120, vec![config], stub_backend());
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
        let mut app = App::new(24, 120, vec![], stub_backend());
        app.open_role_editor();
        app.handle_role_editor_list_key(KeyCode::Char('a'));

        // Trigger an error by submitting with empty name
        app.submit_role_editor();
        assert!(app.error_message.is_some());

        // Now provide a valid name and submit again
        app.role_editor_name.set("valid-role");
        app.submit_role_editor();
        assert!(app.error_message.is_none());
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
        let mut app = App::new(24, 120, vec![], stub_backend());
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
        let mut app = App::new(24, 120, vec![], stub_backend());
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
        let mut app = App::new(24, 120, vec![], stub_backend());
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
        clear_test_shared_state();
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
        };
        let mut app = App::new(24, 120, vec![config], stub_backend());
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
        let mut app = App::new(24, 120, vec![], stub_backend());
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
        };
        let app = App::new(24, 120, vec![config], stub_backend());
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
        };
        let proj_id = proj_config.deterministic_id();

        let shared_proj = sync::SharedProject {
            id: proj_id,
            name: "Test Project".to_string(),
            repos: vec![PathBuf::from("/path/to/repo")],
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
        };

        let info = shared_project_to_info(shared_proj.clone());

        assert_eq!(info.config.repos.len(), 3);
        assert_eq!(info.config.repos[0], PathBuf::from("/repo1"));
        assert_eq!(info.config.repos[1], PathBuf::from("/repo2"));
        assert_eq!(info.config.repos[2], PathBuf::from("/repo3"));
    }

    #[test]
    fn load_and_merge_projects_from_shared_state_only() {
        // Create a temporary shared state file with one project
        let temp_dir = tempfile::tempdir().unwrap();
        let state_path = temp_dir.path().join("shared_state.toml");

        let mut shared_state = sync::state::SharedState::new();
        let proj_config = ProjectConfig {
            name: "Shared Project".to_string(),
            repos: vec![PathBuf::from("/shared/repo")],
            roles: Vec::new(),
        };
        let proj_id = proj_config.deterministic_id();
        shared_state.projects = vec![sync::SharedProject {
            id: proj_id,
            name: "Shared Project".to_string(),
            repos: vec![PathBuf::from("/shared/repo")],
        }];

        sync::file_store::save_shared_state(&state_path, &shared_state).unwrap();

        let projects = load_and_merge_projects(&state_path, vec![]);

        assert_eq!(projects.len(), 1);
        assert_eq!(projects[0].config.name, "Shared Project");
        assert_eq!(projects[0].id, proj_id);
    }

    #[test]
    fn load_and_merge_projects_from_config_only() {
        let temp_dir = tempfile::tempdir().unwrap();
        let state_path = temp_dir.path().join("nonexistent.toml");

        let config = ProjectConfig {
            name: "Config Project".to_string(),
            repos: vec![PathBuf::from("/config/repo")],
            roles: vec![],
        };

        let projects = load_and_merge_projects(&state_path, vec![config]);

        assert_eq!(projects.len(), 1);
        assert_eq!(projects[0].config.name, "Config Project");
    }

    #[test]
    fn load_and_merge_projects_shared_takes_precedence() {
        let temp_dir = tempfile::tempdir().unwrap();
        let state_path = temp_dir.path().join("shared_state.toml");

        // Shared state has the project
        let mut shared_state = sync::state::SharedState::new();
        let proj_config = ProjectConfig {
            name: "Test".to_string(),
            repos: vec![PathBuf::from("/shared/repo")],
            roles: Vec::new(),
        };
        let proj_id = proj_config.deterministic_id();
        shared_state.projects = vec![sync::SharedProject {
            id: proj_id,
            name: "Test".to_string(),
            repos: vec![PathBuf::from("/shared/repo")],
        }];

        sync::file_store::save_shared_state(&state_path, &shared_state).unwrap();

        // Config also has a project with same name
        let config = ProjectConfig {
            name: "Test".to_string(),
            repos: vec![PathBuf::from("/config/repo")],
            roles: vec![],
        };

        let projects = load_and_merge_projects(&state_path, vec![config]);

        assert_eq!(projects.len(), 1);
        // Shared state ID should be preserved
        // ID should match the deterministic ID of the config name
        let expected_id = ProjectConfig {
            name: "Test".to_string(),
            repos: vec![],
            roles: Vec::new(),
        }
        .deterministic_id();
        assert_eq!(projects[0].id, expected_id);
        // Config repos should not override shared repos (only roles are merged)
        assert_eq!(
            projects[0].config.repos,
            vec![PathBuf::from("/shared/repo")]
        );
    }

    #[test]
    fn load_and_merge_projects_merges_roles_from_config() {
        let temp_dir = tempfile::tempdir().unwrap();
        let state_path = temp_dir.path().join("shared_state.toml");

        let mut shared_state = sync::state::SharedState::new();
        let proj_config = ProjectConfig {
            name: "Test".to_string(),
            repos: vec![PathBuf::from("/repo")],
            roles: Vec::new(),
        };
        let proj_id = proj_config.deterministic_id();
        shared_state.projects = vec![sync::SharedProject {
            id: proj_id,
            name: "Test".to_string(),
            repos: vec![PathBuf::from("/repo")],
        }];

        sync::file_store::save_shared_state(&state_path, &shared_state).unwrap();

        let role = crate::session::RoleConfig {
            name: "reviewer".to_string(),
            description: "Code reviewer".to_string(),
            permissions: crate::session::RolePermissions::default(),
        };

        let config = ProjectConfig {
            name: "Test".to_string(),
            repos: vec![PathBuf::from("/repo")],
            roles: vec![role.clone()],
        };

        let projects = load_and_merge_projects(&state_path, vec![config]);

        assert_eq!(projects.len(), 1);
        assert_eq!(projects[0].config.roles.len(), 1);
        assert_eq!(projects[0].config.roles[0].name, "reviewer");
    }

    #[test]
    fn load_and_merge_projects_empty_state_uses_default() {
        let temp_dir = tempfile::tempdir().unwrap();
        let state_path = temp_dir.path().join("nonexistent.toml");

        let projects = load_and_merge_projects(&state_path, vec![]);

        assert_eq!(projects.len(), 1);
        assert!(projects[0].is_default);
    }

    #[test]
    fn load_and_merge_projects_adds_config_only_projects() {
        let temp_dir = tempfile::tempdir().unwrap();
        let state_path = temp_dir.path().join("shared_state.toml");

        let mut shared_state = sync::state::SharedState::new();
        let proj_config = ProjectConfig {
            name: "Shared".to_string(),
            repos: vec![PathBuf::from("/shared/repo")],
            roles: Vec::new(),
        };
        let proj_id = proj_config.deterministic_id();
        shared_state.projects = vec![sync::SharedProject {
            id: proj_id,
            name: "Shared".to_string(),
            repos: vec![PathBuf::from("/shared/repo")],
        }];

        sync::file_store::save_shared_state(&state_path, &shared_state).unwrap();

        let config_only = ProjectConfig {
            name: "ConfigOnly".to_string(),
            repos: vec![PathBuf::from("/config/repo")],
            roles: vec![],
        };

        let projects = load_and_merge_projects(&state_path, vec![config_only]);

        assert_eq!(projects.len(), 2);
        assert!(projects.iter().any(|p| p.config.name == "Shared"));
        assert!(projects.iter().any(|p| p.config.name == "ConfigOnly"));
    }
}
