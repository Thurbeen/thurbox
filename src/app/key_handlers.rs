//! Key event handlers for the Thurbox TUI application.
//!
//! This module contains all keyboard input handling logic organized by context:
//! - Global keybindings (always active)
//! - Focus-based handlers (ProjectList, SessionList, Terminal)
//! - Modal handlers (AddProject, RepoSelector, BranchSelector, etc.)

use std::path::PathBuf;

use super::{AddProjectField, App, EditProjectField, InputFocus, RoleEditorView};
use crate::claude::input;
use crate::paths;
use crossterm::event::{KeyCode, KeyModifiers};
use tracing::error;

impl App {
    /// Main key handler dispatcher.
    ///
    /// Routes key events to the appropriate handler based on:
    /// 1. Modal state (highest priority)
    /// 2. Global keybindings (Ctrl+Q, Ctrl+N, etc.)
    /// 3. Focus-based handlers (ProjectList, SessionList, Terminal)
    pub(crate) fn handle_key(&mut self, code: KeyCode, mods: KeyModifiers) {
        // Dismiss help overlay with Esc
        if self.show_help {
            if code == KeyCode::Esc {
                self.show_help = false;
            }
            return;
        }

        // Repo selector modal captures all input
        if self.show_repo_selector {
            self.handle_repo_selector_key(code);
            return;
        }

        // Session mode modal captures all input
        if self.show_session_mode_modal {
            self.handle_session_mode_key(code);
            return;
        }

        // Branch selector modal captures all input
        if self.show_branch_selector {
            self.handle_branch_selector_key(code);
            return;
        }

        // Worktree name modal captures all input
        if self.show_worktree_name_modal {
            self.handle_worktree_name_key(code);
            return;
        }

        // Role editor modal captures all input
        if self.show_role_editor {
            self.handle_role_editor_key(code);
            return;
        }

        // Role selector modal captures all input
        if self.show_role_selector {
            self.handle_role_selector_key(code);
            return;
        }

        // Add-project modal captures all input
        if self.show_add_project_modal {
            self.handle_add_project_key(code);
            return;
        }

        // Edit-project modal captures all input
        if self.show_edit_project_modal {
            self.handle_edit_project_key(code);
            return;
        }

        // Delete-project modal captures all input
        if self.show_delete_project_modal_flag {
            self.handle_delete_project_key(code);
            return;
        }

        // Global keybindings (always active)
        if mods.contains(KeyModifiers::CONTROL) {
            match code {
                KeyCode::Char('q') => {
                    self.should_quit = true;
                    return;
                }
                KeyCode::Char('n') => {
                    if self.focus == InputFocus::ProjectList {
                        self.show_add_project_modal = true;
                        self.add_project_field = AddProjectField::Name;
                    } else {
                        self.spawn_session();
                    }
                    return;
                }
                KeyCode::Char('c') => {
                    self.close_active_session();
                    return;
                }
                KeyCode::Char('d') => match self.focus {
                    InputFocus::SessionList => {
                        self.close_active_session();
                        return;
                    }
                    InputFocus::ProjectList => {
                        self.show_delete_project_modal();
                        return;
                    }
                    InputFocus::Terminal => {} // forward to PTY
                },
                KeyCode::Char('e') => {
                    self.open_edit_project_modal();
                    return;
                }
                KeyCode::Char('r') => {
                    self.open_role_editor();
                    return;
                }
                // Vim navigation: h=left, j=down, k=up, l=cycle-right
                KeyCode::Char('h') => {
                    self.focus = InputFocus::ProjectList;
                    return;
                }
                KeyCode::Char('j') => {
                    self.switch_session_forward();
                    return;
                }
                KeyCode::Char('k') => {
                    self.switch_session_backward();
                    return;
                }
                KeyCode::Char('l') => {
                    self.focus = match self.focus {
                        InputFocus::ProjectList => InputFocus::SessionList,
                        InputFocus::SessionList => InputFocus::Terminal,
                        InputFocus::Terminal => InputFocus::ProjectList,
                    };
                    return;
                }
                _ => {}
            }
        }

        // Function keys (work reliably in all terminals)
        match code {
            KeyCode::F(1) => {
                self.show_help = true;
                return;
            }
            KeyCode::F(2) => {
                self.show_info_panel = !self.show_info_panel;
                return;
            }
            _ => {}
        }

        match self.focus {
            InputFocus::ProjectList => self.handle_project_list_key(code),
            InputFocus::SessionList => self.handle_session_list_key(code),
            InputFocus::Terminal => self.handle_terminal_key(code, mods),
        }
    }

    fn handle_project_list_key(&mut self, code: KeyCode) {
        match code {
            KeyCode::Char('j') | KeyCode::Down => {
                if self.active_project_index + 1 < self.projects.len() {
                    self.active_project_index += 1;
                    self.sync_active_session_to_project();
                }
            }
            KeyCode::Char('k') | KeyCode::Up => {
                if self.active_project_index > 0 {
                    self.active_project_index -= 1;
                    self.sync_active_session_to_project();
                }
            }
            KeyCode::Enter => {
                self.focus = InputFocus::SessionList;
            }
            _ => {}
        }
    }

    fn handle_session_list_key(&mut self, code: KeyCode) {
        match code {
            KeyCode::Char('j') | KeyCode::Down => {
                self.switch_session_forward();
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.switch_session_backward();
            }
            KeyCode::Enter => {
                self.focus = InputFocus::Terminal;
            }
            _ => {}
        }
    }

    fn handle_terminal_key(&mut self, code: KeyCode, mods: KeyModifiers) {
        // Scroll keybindings (Shift + navigation keys)
        if mods.contains(KeyModifiers::SHIFT) {
            match code {
                KeyCode::Up => {
                    self.scroll_terminal_up(1);
                    return;
                }
                KeyCode::Down => {
                    self.scroll_terminal_down(1);
                    return;
                }
                KeyCode::PageUp => {
                    let amount = self.page_scroll_amount();
                    self.scroll_terminal_up(amount);
                    return;
                }
                KeyCode::PageDown => {
                    let amount = self.page_scroll_amount();
                    self.scroll_terminal_down(amount);
                    return;
                }
                _ => {}
            }
        }

        // Snap to bottom on any non-scroll key when scrolled up
        self.with_active_parser(|parser| {
            if parser.screen().scrollback() > 0 {
                parser.screen_mut().set_scrollback(0);
            }
        });

        if let Some(session) = self.sessions.get(self.active_index) {
            if let Some(bytes) = input::key_to_bytes(code, mods) {
                if let Err(e) = session.send_input(bytes) {
                    error!("Failed to send input: {e}");
                }
            }
        }
    }

    fn handle_add_project_key(&mut self, code: KeyCode) {
        match self.add_project_field {
            AddProjectField::Name => self.handle_add_project_name_key(code),
            AddProjectField::Path => self.handle_add_project_path_key(code),
            AddProjectField::RepoList => self.handle_add_project_repo_list_key(code),
        }
    }

    fn handle_add_project_name_key(&mut self, code: KeyCode) {
        match code {
            KeyCode::Esc => self.close_add_project_modal(),
            KeyCode::Tab => {
                self.add_project_field = AddProjectField::Path;
            }
            KeyCode::BackTab => {
                if !self.add_project_repos.is_empty() {
                    self.add_project_field = AddProjectField::RepoList;
                } else {
                    self.add_project_field = AddProjectField::Path;
                }
            }
            KeyCode::Enter => self.submit_add_project(),
            KeyCode::Backspace => self.add_project_name.backspace(),
            KeyCode::Delete => self.add_project_name.delete(),
            KeyCode::Left => self.add_project_name.move_left(),
            KeyCode::Right => self.add_project_name.move_right(),
            KeyCode::Home => self.add_project_name.home(),
            KeyCode::End => self.add_project_name.end(),
            KeyCode::Char(c) => self.add_project_name.insert(c),
            _ => {}
        }
    }

    fn handle_add_project_path_key(&mut self, code: KeyCode) {
        match code {
            KeyCode::Esc => {
                self.close_add_project_modal();
                return;
            }
            KeyCode::Tab => {
                if let Some(suggestion) = self.add_project_path_suggestion.take() {
                    for c in suggestion.chars() {
                        self.add_project_path.insert(c);
                    }
                } else if !self.add_project_repos.is_empty() {
                    self.add_project_field = AddProjectField::RepoList;
                    self.add_project_path_suggestion = None;
                    return;
                } else {
                    self.add_project_field = AddProjectField::Name;
                    self.add_project_path_suggestion = None;
                    return;
                }
            }
            KeyCode::BackTab => {
                self.add_project_field = AddProjectField::Name;
                self.add_project_path_suggestion = None;
                return;
            }
            KeyCode::Enter => {
                let path = self.add_project_path.value().trim().to_string();
                if !path.is_empty() {
                    self.add_project_repos.push(PathBuf::from(path));
                    self.add_project_repo_index = self.add_project_repos.len().saturating_sub(1);
                    self.add_project_path.clear();
                    self.add_project_path_suggestion = None;
                }
                return;
            }
            KeyCode::Backspace => self.add_project_path.backspace(),
            KeyCode::Delete => self.add_project_path.delete(),
            KeyCode::Left => self.add_project_path.move_left(),
            KeyCode::Right => self.add_project_path.move_right(),
            KeyCode::Home => self.add_project_path.home(),
            KeyCode::End => self.add_project_path.end(),
            KeyCode::Char(c) => self.add_project_path.insert(c),
            _ => return,
        }
        self.update_path_suggestion();
    }

    fn handle_add_project_repo_list_key(&mut self, code: KeyCode) {
        match code {
            KeyCode::Esc => self.close_add_project_modal(),
            KeyCode::Tab => {
                self.add_project_field = AddProjectField::Name;
            }
            KeyCode::BackTab => {
                self.add_project_field = AddProjectField::Path;
            }
            KeyCode::Char('j') | KeyCode::Down => {
                if self.add_project_repo_index + 1 < self.add_project_repos.len() {
                    self.add_project_repo_index += 1;
                }
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.add_project_repo_index = self.add_project_repo_index.saturating_sub(1);
            }
            KeyCode::Char('d') => {
                if !self.add_project_repos.is_empty() {
                    self.add_project_repos.remove(self.add_project_repo_index);
                    if self.add_project_repo_index >= self.add_project_repos.len()
                        && self.add_project_repo_index > 0
                    {
                        self.add_project_repo_index -= 1;
                    }
                    // If list becomes empty, switch to Path field
                    if self.add_project_repos.is_empty() {
                        self.add_project_field = AddProjectField::Path;
                    }
                }
            }
            KeyCode::Enter => self.submit_add_project(),
            _ => {}
        }
    }

    /// Recompute path suggestion (fish-style: only when cursor is at end).
    fn update_path_suggestion(&mut self) {
        let value = self.add_project_path.value();
        let at_end = self.add_project_path.cursor_pos() == value.chars().count();
        if at_end && !value.is_empty() {
            self.add_project_path_suggestion = paths::complete_directory_path(value);
        } else {
            self.add_project_path_suggestion = None;
        }
    }

    /// Close the add-project modal and clear all related state.
    pub(crate) fn close_add_project_modal(&mut self) {
        self.show_add_project_modal = false;
        self.add_project_name.clear();
        self.add_project_path.clear();
        self.add_project_field = AddProjectField::Name;
        self.add_project_repos.clear();
        self.add_project_repo_index = 0;
        self.add_project_path_suggestion = None;
    }

    fn handle_edit_project_key(&mut self, code: KeyCode) {
        match self.edit_project_field {
            EditProjectField::Name => self.handle_edit_project_name_key(code),
            EditProjectField::Path => self.handle_edit_project_path_key(code),
            EditProjectField::RepoList => self.handle_edit_project_repo_list_key(code),
            EditProjectField::Roles => self.handle_edit_project_roles_key(code),
        }
    }

    fn handle_edit_project_name_key(&mut self, code: KeyCode) {
        match code {
            KeyCode::Esc => self.close_edit_project_modal(),
            KeyCode::Tab => {
                self.edit_project_field = EditProjectField::Path;
            }
            KeyCode::BackTab => {
                self.edit_project_field = EditProjectField::Roles;
            }
            KeyCode::Enter => self.submit_edit_project(),
            KeyCode::Backspace => self.edit_project_name.backspace(),
            KeyCode::Delete => self.edit_project_name.delete(),
            KeyCode::Left => self.edit_project_name.move_left(),
            KeyCode::Right => self.edit_project_name.move_right(),
            KeyCode::Home => self.edit_project_name.home(),
            KeyCode::End => self.edit_project_name.end(),
            KeyCode::Char(c) => self.edit_project_name.insert(c),
            _ => {}
        }
    }

    fn handle_edit_project_path_key(&mut self, code: KeyCode) {
        match code {
            KeyCode::Esc => {
                self.close_edit_project_modal();
                return;
            }
            KeyCode::Tab => {
                if let Some(suggestion) = self.edit_project_path_suggestion.take() {
                    for c in suggestion.chars() {
                        self.edit_project_path.insert(c);
                    }
                } else if !self.edit_project_repos.is_empty() {
                    self.edit_project_field = EditProjectField::RepoList;
                    self.edit_project_path_suggestion = None;
                    return;
                } else {
                    self.edit_project_field = EditProjectField::Roles;
                    self.edit_project_path_suggestion = None;
                    return;
                }
            }
            KeyCode::BackTab => {
                self.edit_project_field = EditProjectField::Name;
                self.edit_project_path_suggestion = None;
                return;
            }
            KeyCode::Enter => {
                let path = self.edit_project_path.value().trim().to_string();
                if !path.is_empty() {
                    self.edit_project_repos.push(PathBuf::from(path));
                    self.edit_project_repo_index = self.edit_project_repos.len().saturating_sub(1);
                    self.edit_project_path.clear();
                    self.edit_project_path_suggestion = None;
                }
                return;
            }
            KeyCode::Backspace => self.edit_project_path.backspace(),
            KeyCode::Delete => self.edit_project_path.delete(),
            KeyCode::Left => self.edit_project_path.move_left(),
            KeyCode::Right => self.edit_project_path.move_right(),
            KeyCode::Home => self.edit_project_path.home(),
            KeyCode::End => self.edit_project_path.end(),
            KeyCode::Char(c) => self.edit_project_path.insert(c),
            _ => return,
        }
        self.update_edit_path_suggestion();
    }

    fn handle_edit_project_repo_list_key(&mut self, code: KeyCode) {
        match code {
            KeyCode::Esc => self.close_edit_project_modal(),
            KeyCode::Tab => {
                self.edit_project_field = EditProjectField::Roles;
            }
            KeyCode::BackTab => {
                self.edit_project_field = EditProjectField::Path;
            }
            KeyCode::Char('j') | KeyCode::Down => {
                if self.edit_project_repo_index + 1 < self.edit_project_repos.len() {
                    self.edit_project_repo_index += 1;
                }
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.edit_project_repo_index = self.edit_project_repo_index.saturating_sub(1);
            }
            KeyCode::Char('d') => {
                if !self.edit_project_repos.is_empty() {
                    self.edit_project_repos.remove(self.edit_project_repo_index);
                    if self.edit_project_repo_index >= self.edit_project_repos.len()
                        && self.edit_project_repo_index > 0
                    {
                        self.edit_project_repo_index -= 1;
                    }
                    // If list becomes empty, switch to Path field
                    if self.edit_project_repos.is_empty() {
                        self.edit_project_field = EditProjectField::Path;
                    }
                }
            }
            KeyCode::Enter => self.submit_edit_project(),
            _ => {}
        }
    }

    fn handle_edit_project_roles_key(&mut self, code: KeyCode) {
        match code {
            KeyCode::Esc => self.close_edit_project_modal(),
            KeyCode::Tab => {
                self.edit_project_field = EditProjectField::Name;
            }
            KeyCode::BackTab => {
                if !self.edit_project_repos.is_empty() {
                    self.edit_project_field = EditProjectField::RepoList;
                } else {
                    self.edit_project_field = EditProjectField::Path;
                }
            }
            KeyCode::Enter => {
                // Save name+repo changes first, then open role editor
                self.submit_edit_project();
                // Only open role editor if submit succeeded (modal closed)
                if !self.show_edit_project_modal {
                    self.open_role_editor();
                }
            }
            _ => {}
        }
    }

    /// Recompute path suggestion for edit-project modal.
    fn update_edit_path_suggestion(&mut self) {
        let value = self.edit_project_path.value();
        let at_end = self.edit_project_path.cursor_pos() == value.chars().count();
        if at_end && !value.is_empty() {
            self.edit_project_path_suggestion = paths::complete_directory_path(value);
        } else {
            self.edit_project_path_suggestion = None;
        }
    }

    fn handle_delete_project_key(&mut self, code: KeyCode) {
        match code {
            KeyCode::Enter => {
                self.delete_active_project();
            }
            KeyCode::Esc => {
                self.show_delete_project_modal_flag = false;
                self.delete_project_confirmation.clear();
                self.delete_project_error = None;
            }
            KeyCode::Char(c) => {
                self.delete_project_confirmation.insert(c);
                self.delete_project_error = None; // Clear error on new input
            }
            KeyCode::Backspace => {
                self.delete_project_confirmation.backspace();
                self.delete_project_error = None;
            }
            KeyCode::Delete => {
                self.delete_project_confirmation.delete();
                self.delete_project_error = None;
            }
            KeyCode::Left => {
                self.delete_project_confirmation.move_left();
            }
            KeyCode::Right => {
                self.delete_project_confirmation.move_right();
            }
            KeyCode::Home => {
                self.delete_project_confirmation.home();
            }
            KeyCode::End => {
                self.delete_project_confirmation.end();
            }
            _ => {}
        }
    }

    fn handle_repo_selector_key(&mut self, code: KeyCode) {
        let Some(project) = self.active_project() else {
            return;
        };
        let repo_count = project.config.repos.len();
        match code {
            KeyCode::Esc => {
                self.show_repo_selector = false;
            }
            KeyCode::Char('j') | KeyCode::Down => {
                if self.repo_selector_index + 1 < repo_count {
                    self.repo_selector_index += 1;
                }
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.repo_selector_index = self.repo_selector_index.saturating_sub(1);
            }
            KeyCode::Enter => {
                if let Some(path) = project.config.repos.get(self.repo_selector_index).cloned() {
                    self.pending_repo_path = Some(path);
                    self.show_repo_selector = false;
                    self.session_mode_index = 0;
                    self.show_session_mode_modal = true;
                }
            }
            _ => {}
        }
    }

    fn handle_session_mode_key(&mut self, code: KeyCode) {
        match code {
            KeyCode::Esc => {
                self.show_session_mode_modal = false;
                self.pending_repo_path = None;
            }
            KeyCode::Char('j') | KeyCode::Down => {
                if self.session_mode_index == 0 {
                    self.session_mode_index = 1;
                }
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.session_mode_index = 0;
            }
            KeyCode::Enter => {
                self.show_session_mode_modal = false;
                if self.session_mode_index == 0 {
                    // Normal mode
                    if let Some(path) = self.pending_repo_path.take() {
                        self.spawn_session_in_repo(path);
                    }
                } else {
                    // Worktree mode
                    self.start_branch_selection();
                }
            }
            _ => {}
        }
    }

    fn handle_branch_selector_key(&mut self, code: KeyCode) {
        match code {
            KeyCode::Esc => {
                self.show_branch_selector = false;
                self.available_branches.clear();
                self.pending_repo_path = None;
            }
            KeyCode::Char('j') | KeyCode::Down => {
                if self.branch_selector_index + 1 < self.available_branches.len() {
                    self.branch_selector_index += 1;
                }
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.branch_selector_index = self.branch_selector_index.saturating_sub(1);
            }
            KeyCode::Enter => {
                let base_branch = self.available_branches[self.branch_selector_index].clone();
                self.show_branch_selector = false;
                self.available_branches.clear();
                self.worktree_name_input.clear();
                self.pending_base_branch = Some(base_branch);
                self.show_worktree_name_modal = true;
            }
            _ => {}
        }
    }

    fn handle_worktree_name_key(&mut self, code: KeyCode) {
        match code {
            KeyCode::Esc => {
                self.show_worktree_name_modal = false;
                self.worktree_name_input.clear();
                self.pending_base_branch = None;
                self.pending_repo_path = None;
            }
            KeyCode::Enter => {
                let new_branch = self.worktree_name_input.value().trim().to_string();
                if new_branch.is_empty() {
                    self.error_message = Some("Branch name cannot be empty".to_string());
                    return;
                }
                self.show_worktree_name_modal = false;
                if let (Some(repo_path), Some(base_branch)) = (
                    self.pending_repo_path.take(),
                    self.pending_base_branch.take(),
                ) {
                    self.worktree_name_input.clear();
                    self.spawn_worktree_session(repo_path, &new_branch, &base_branch);
                }
            }
            KeyCode::Backspace => self.worktree_name_input.backspace(),
            KeyCode::Delete => self.worktree_name_input.delete(),
            KeyCode::Left => self.worktree_name_input.move_left(),
            KeyCode::Right => self.worktree_name_input.move_right(),
            KeyCode::Home => self.worktree_name_input.home(),
            KeyCode::End => self.worktree_name_input.end(),
            KeyCode::Char(c) => self.worktree_name_input.insert(c),
            _ => {}
        }
    }

    fn handle_role_selector_key(&mut self, code: KeyCode) {
        let role_count = self
            .active_project()
            .map(|p| p.config.roles.len())
            .unwrap_or(0);
        match code {
            KeyCode::Esc => {
                self.show_role_selector = false;
                self.pending_spawn_config = None;
                self.pending_spawn_worktree = None;
                self.pending_spawn_name = None;
                // Undo the counter increment from prepare_spawn()
                self.session_counter = self.session_counter.saturating_sub(1);
            }
            KeyCode::Char('j') | KeyCode::Down => {
                if self.role_selector_index + 1 < role_count {
                    self.role_selector_index += 1;
                }
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.role_selector_index = self.role_selector_index.saturating_sub(1);
            }
            KeyCode::Enter => {
                self.show_role_selector = false;
                let role_index = self.role_selector_index;
                if let (Some(mut config), Some(name)) = (
                    self.pending_spawn_config.take(),
                    self.pending_spawn_name.take(),
                ) {
                    if let Some(project) = self.active_project() {
                        if let Some(role) = project.config.roles.get(role_index) {
                            config.role = role.name.clone();
                            config.permissions = role.permissions.clone();
                            let worktree = self.pending_spawn_worktree.take();
                            self.do_spawn_session(name, &config, worktree);
                        }
                    }
                }
            }
            _ => {}
        }
    }

    fn handle_role_editor_key(&mut self, code: KeyCode) {
        match self.role_editor_view {
            RoleEditorView::List => self.handle_role_editor_list_key(code),
            RoleEditorView::Editor => self.handle_role_editor_editor_key(code),
        }
    }

    pub(crate) fn handle_role_editor_list_key(&mut self, code: KeyCode) {
        match code {
            KeyCode::Esc => {
                // Save & close
                let roles_to_save = self.role_editor_roles.clone();
                if let Some(project) = self.active_project_mut() {
                    project.config.roles = roles_to_save;
                    let project_clone = project.clone();
                    self.save_project_to_db(&project_clone);
                }
                self.show_role_editor = false;
            }
            KeyCode::Char('j') | KeyCode::Down => {
                if !self.role_editor_roles.is_empty()
                    && self.role_editor_list_index + 1 < self.role_editor_roles.len()
                {
                    self.role_editor_list_index += 1;
                }
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.role_editor_list_index = self.role_editor_list_index.saturating_sub(1);
            }
            KeyCode::Char('a') => {
                self.role_editor_editing_index = None;
                self.role_editor_name.clear();
                self.role_editor_description.clear();
                self.role_editor_allowed_tools.reset();
                self.role_editor_disallowed_tools.reset();
                self.role_editor_system_prompt.clear();
                self.role_editor_field = crate::ui::role_editor_modal::RoleEditorField::Name;
                self.role_editor_view = RoleEditorView::Editor;
            }
            KeyCode::Char('e') | KeyCode::Enter => {
                if !self.role_editor_roles.is_empty() {
                    let idx = self.role_editor_list_index;
                    self.open_role_for_editing(idx);
                }
            }
            KeyCode::Char('d') => {
                if !self.role_editor_roles.is_empty() {
                    self.role_editor_roles.remove(self.role_editor_list_index);
                    if self.role_editor_list_index >= self.role_editor_roles.len()
                        && self.role_editor_list_index > 0
                    {
                        self.role_editor_list_index -= 1;
                    }
                }
            }
            _ => {}
        }
    }

    pub(crate) fn handle_role_editor_editor_key(&mut self, code: KeyCode) {
        use crate::ui::role_editor_modal::{RoleEditorField, ToolListMode};

        match self.role_editor_field {
            RoleEditorField::AllowedTools | RoleEditorField::DisallowedTools => {
                if self.active_tool_list_mut().mode == ToolListMode::Adding {
                    self.handle_tool_adding_key(code);
                } else {
                    self.handle_tool_browse_key(code);
                }
                return;
            }
            _ => {}
        }

        // Text field handling (Name, Description, SystemPrompt).
        match code {
            KeyCode::Esc => {
                self.role_editor_view = RoleEditorView::List;
            }
            KeyCode::Tab => {
                self.role_editor_field = Self::next_editor_field(self.role_editor_field);
            }
            KeyCode::BackTab => {
                self.role_editor_field = Self::prev_editor_field(self.role_editor_field);
            }
            KeyCode::Enter => {
                self.submit_role_editor();
            }
            _ => {
                let input = match self.role_editor_field {
                    RoleEditorField::Name => &mut self.role_editor_name,
                    RoleEditorField::Description => &mut self.role_editor_description,
                    RoleEditorField::SystemPrompt => &mut self.role_editor_system_prompt,
                    _ => return,
                };
                match code {
                    KeyCode::Backspace => input.backspace(),
                    KeyCode::Delete => input.delete(),
                    KeyCode::Left => input.move_left(),
                    KeyCode::Right => input.move_right(),
                    KeyCode::Home => input.home(),
                    KeyCode::End => input.end(),
                    KeyCode::Char(c) => input.insert(c),
                    _ => {}
                }
            }
        }
    }

    fn handle_tool_browse_key(&mut self, code: KeyCode) {
        match code {
            KeyCode::Esc => {
                self.role_editor_view = RoleEditorView::List;
            }
            KeyCode::Tab => {
                self.role_editor_field = Self::next_editor_field(self.role_editor_field);
            }
            KeyCode::BackTab => {
                self.role_editor_field = Self::prev_editor_field(self.role_editor_field);
            }
            KeyCode::Enter => {
                self.submit_role_editor();
            }
            KeyCode::Char('a') => self.active_tool_list_mut().start_adding(),
            KeyCode::Char('d') => self.active_tool_list_mut().delete_selected(),
            KeyCode::Char('j') | KeyCode::Down => self.active_tool_list_mut().move_down(),
            KeyCode::Char('k') | KeyCode::Up => self.active_tool_list_mut().move_up(),
            _ => {}
        }
    }

    fn handle_tool_adding_key(&mut self, code: KeyCode) {
        match code {
            KeyCode::Esc => self.active_tool_list_mut().cancel_add(),
            KeyCode::Enter => self.active_tool_list_mut().confirm_add(),
            _ => {
                let input = &mut self.active_tool_list_mut().input;
                match code {
                    KeyCode::Backspace => input.backspace(),
                    KeyCode::Delete => input.delete(),
                    KeyCode::Left => input.move_left(),
                    KeyCode::Right => input.move_right(),
                    KeyCode::Home => input.home(),
                    KeyCode::End => input.end(),
                    KeyCode::Char(c) => input.insert(c),
                    _ => {}
                }
            }
        }
    }

    fn next_editor_field(
        field: crate::ui::role_editor_modal::RoleEditorField,
    ) -> crate::ui::role_editor_modal::RoleEditorField {
        use crate::ui::role_editor_modal::RoleEditorField;
        match field {
            RoleEditorField::Name => RoleEditorField::Description,
            RoleEditorField::Description => RoleEditorField::AllowedTools,
            RoleEditorField::AllowedTools => RoleEditorField::DisallowedTools,
            RoleEditorField::DisallowedTools => RoleEditorField::SystemPrompt,
            RoleEditorField::SystemPrompt => RoleEditorField::Name,
        }
    }

    fn prev_editor_field(
        field: crate::ui::role_editor_modal::RoleEditorField,
    ) -> crate::ui::role_editor_modal::RoleEditorField {
        use crate::ui::role_editor_modal::RoleEditorField;
        match field {
            RoleEditorField::Name => RoleEditorField::SystemPrompt,
            RoleEditorField::Description => RoleEditorField::Name,
            RoleEditorField::AllowedTools => RoleEditorField::Description,
            RoleEditorField::DisallowedTools => RoleEditorField::AllowedTools,
            RoleEditorField::SystemPrompt => RoleEditorField::DisallowedTools,
        }
    }

    pub(crate) fn open_role_editor(&mut self) {
        let Some(project) = self.active_project() else {
            return;
        };
        self.role_editor_roles = project.config.roles.clone();
        self.role_editor_list_index = 0;
        self.role_editor_view = RoleEditorView::List;
        self.show_role_editor = true;
    }

    pub(crate) fn open_role_for_editing(&mut self, index: usize) {
        let role = &self.role_editor_roles[index];
        self.role_editor_editing_index = Some(index);
        self.role_editor_name.set(&role.name);
        self.role_editor_description.set(&role.description);
        self.role_editor_allowed_tools
            .load(&role.permissions.allowed_tools);
        self.role_editor_disallowed_tools
            .load(&role.permissions.disallowed_tools);
        self.role_editor_system_prompt.set(
            role.permissions
                .append_system_prompt
                .as_deref()
                .unwrap_or(""),
        );
        self.role_editor_field = crate::ui::role_editor_modal::RoleEditorField::Name;
        self.role_editor_view = RoleEditorView::Editor;
    }

    pub(crate) fn active_tool_list_mut(&mut self) -> &mut super::ToolListState {
        match self.role_editor_field {
            crate::ui::role_editor_modal::RoleEditorField::AllowedTools => {
                &mut self.role_editor_allowed_tools
            }
            _ => &mut self.role_editor_disallowed_tools,
        }
    }

    pub(crate) fn start_branch_selection(&mut self) {
        let Some(repo_path) = self.pending_repo_path.as_ref() else {
            return;
        };
        match crate::git::list_branches(repo_path) {
            Ok(branches) if branches.is_empty() => {
                self.error_message = Some("No branches found in repository".to_string());
                self.pending_repo_path = None;
            }
            Ok(mut branches) => {
                // Move the default branch to front so it's pre-selected.
                if let Some(default) = crate::git::default_branch(repo_path, &branches) {
                    if let Some(pos) = branches.iter().position(|b| b == &default) {
                        let branch = branches.remove(pos);
                        branches.insert(0, branch);
                    }
                }
                self.available_branches = branches;
                self.branch_selector_index = 0;
                self.show_branch_selector = true;
            }
            Err(e) => {
                error!("Failed to list branches: {e}");
                self.error_message = Some(format!("Failed to list branches: {e:#}"));
                self.pending_repo_path = None;
            }
        }
    }
}
