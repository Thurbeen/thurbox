//! Key event handlers for the Thurbox TUI application.
//!
//! This module contains all keyboard input handling logic organized by context:
//! - Global keybindings (always active)
//! - Focus-based handlers (ProjectList, SessionList, Terminal)
//! - Modal handlers (AddProject, RepoSelector, BranchSelector, etc.)

use std::path::PathBuf;

use crate::session::SessionConfig;

use super::mcp_editor_modal::McpEditorField;
use super::{App, EditProjectField, InputFocus, RoleEditorView, TerminalView};
use crate::agent::input;
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
        if matches!(self.modal, super::modals::Modal::Help) {
            if code == KeyCode::Esc {
                self.modal.close();
            }
            return;
        }

        // Restore sessions modal captures all input
        if matches!(self.modal, super::modals::Modal::RestoreSessions(_)) {
            self.handle_restore_sessions_key(code);
            return;
        }

        // Repo selector modal captures all input
        if matches!(self.modal, super::modals::Modal::RepoSelector(_)) {
            self.handle_repo_selector_key(code);
            return;
        }

        // Containerfile picker modal captures all input
        if matches!(self.modal, super::modals::Modal::ContainerfilePicker(_)) {
            self.handle_containerfile_picker_key(code);
            return;
        }

        // Session mode modal captures all input
        if matches!(self.modal, super::modals::Modal::SessionMode(_)) {
            self.handle_session_mode_key(code);
            return;
        }

        // Branch selector modal captures all input
        if matches!(self.modal, super::modals::Modal::BranchSelector(_)) {
            self.handle_branch_selector_key(code);
            return;
        }

        // Worktree name modal captures all input
        if matches!(self.modal, super::modals::Modal::WorktreeName(_)) {
            self.handle_worktree_name_key(code);
            return;
        }

        // Schedule command modal captures all input
        if matches!(self.modal, super::modals::Modal::ScheduleCommand(_)) {
            self.handle_schedule_command_key(code);
            return;
        }

        // Discard confirmation overlay captures all input
        if self.show_discard_confirmation {
            match code {
                KeyCode::Char('y') | KeyCode::Char('Y') => {
                    if self.show_mcp_editor {
                        self.close_mcp_editor();
                    } else if self.show_role_editor {
                        self.close_role_editor();
                    }
                }
                KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
                    self.show_discard_confirmation = false;
                }
                _ => {}
            }
            return;
        }

        // MCP editor detail form captures all input
        if self.show_mcp_editor {
            self.handle_mcp_editor_key(code);
            return;
        }

        // Role editor detail form captures all input
        if self.show_role_editor {
            self.handle_role_editor_editor_key(code);
            return;
        }

        // Role selector modal captures all input
        if matches!(self.modal, super::modals::Modal::RoleSelector(_)) {
            self.handle_role_selector_key(code);
            return;
        }

        // Add-project modal captures all input
        if matches!(self.modal, super::modals::Modal::AddProject(_)) {
            self.handle_add_project_key(code);
            return;
        }

        // Edit-project modal captures all input
        if matches!(self.modal, super::modals::Modal::EditProject(_)) {
            self.handle_edit_project_key(code);
            return;
        }

        // Delete-project modal captures all input
        if matches!(self.modal, super::modals::Modal::DeleteProject(_)) {
            self.handle_delete_project_key(code);
            return;
        }

        // Ctrl+C: copy selection if active, otherwise forward to terminal as SIGINT
        if code == KeyCode::Char('c')
            && mods.contains(KeyModifiers::CONTROL)
            && self.text_selection.is_some()
        {
            self.copy_selection_to_clipboard();
            return;
        }

        // Ctrl+V: paste from clipboard
        if code == KeyCode::Char('v') && mods.contains(KeyModifiers::CONTROL) {
            self.text_selection = None;
            self.paste_from_clipboard();
            return;
        }

        // Any key press clears text selection (but the key still performs its action)
        self.text_selection = None;

        // Global keybindings (always active)
        if mods.contains(KeyModifiers::CONTROL) {
            match code {
                KeyCode::Char('q') => {
                    self.should_quit = true;
                    return;
                }
                KeyCode::Char('n') => {
                    if self.focus == InputFocus::ProjectList {
                        self.modal = super::modals::Modal::AddProject(
                            super::modals::AddProjectModal::default(),
                        );
                    } else {
                        self.spawn_session();
                    }
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
                    self.restart_active_session();
                    return;
                }
                KeyCode::Char('p') => {
                    self.open_schedule_command_modal();
                    return;
                }
                KeyCode::Char('s') => {
                    self.start_sync();
                    return;
                }
                KeyCode::Char('t') => {
                    self.toggle_shell_view();
                    return;
                }
                KeyCode::Char('z') => {
                    if self.pending_delete.is_some() {
                        self.undo_delete();
                    }
                    return;
                }
                KeyCode::Char('u') => {
                    self.open_restore_sessions_modal();
                    return;
                }
                // Vim navigation: h=left, j=down, k=up, l=cycle-right
                KeyCode::Char('h') => {
                    self.focus = InputFocus::ProjectList;
                    return;
                }
                KeyCode::Char('j') => {
                    if self.focus == InputFocus::ProjectList {
                        self.switch_project_forward();
                    } else {
                        self.switch_session_forward();
                    }
                    return;
                }
                KeyCode::Char('k') => {
                    if self.focus == InputFocus::ProjectList {
                        self.switch_project_backward();
                    } else {
                        self.switch_session_backward();
                    }
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
                self.modal = super::modals::Modal::Help;
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
                self.switch_project_forward();
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.switch_project_backward();
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
                let result = if let (TerminalView::Shell, Some(shell)) =
                    (self.active_terminal_view(), &session.shell_pane)
                {
                    shell.send_input(bytes)
                } else {
                    session.send_input(bytes)
                };
                if let Err(e) = result {
                    error!("Failed to send input: {e}");
                }
            }
        }
    }

    fn handle_add_project_key(&mut self, code: KeyCode) {
        let super::modals::Modal::AddProject(ref ap) = self.modal else {
            return;
        };
        match ap.field {
            super::modals::AddProjectField::Name => self.handle_add_project_name_key(code),
            super::modals::AddProjectField::Path => self.handle_add_project_path_key(code),
            super::modals::AddProjectField::RepoList => self.handle_add_project_repo_list_key(code),
        }
    }

    fn handle_add_project_name_key(&mut self, code: KeyCode) {
        let super::modals::Modal::AddProject(ref mut ap) = self.modal else {
            return;
        };
        match code {
            KeyCode::Esc => self.close_add_project_modal(),
            KeyCode::Tab => {
                ap.field = super::modals::AddProjectField::Path;
            }
            KeyCode::BackTab => {
                if !ap.repos.is_empty() {
                    ap.field = super::modals::AddProjectField::RepoList;
                } else {
                    ap.field = super::modals::AddProjectField::Path;
                }
            }
            KeyCode::Enter => self.submit_add_project(),
            KeyCode::Backspace => ap.name.backspace(),
            KeyCode::Delete => ap.name.delete(),
            KeyCode::Left => ap.name.move_left(),
            KeyCode::Right => ap.name.move_right(),
            KeyCode::Home => ap.name.home(),
            KeyCode::End => ap.name.end(),
            KeyCode::Char(c) => ap.name.insert(c),
            _ => {}
        }
    }

    fn handle_add_project_path_key(&mut self, code: KeyCode) {
        let super::modals::Modal::AddProject(ref mut ap) = self.modal else {
            return;
        };
        match code {
            KeyCode::Esc => {
                self.close_add_project_modal();
                return;
            }
            KeyCode::Tab => {
                if let Some(suggestion) = ap.path_suggestion.take() {
                    for c in suggestion.chars() {
                        ap.path.insert(c);
                    }
                } else if !ap.repos.is_empty() {
                    ap.field = super::modals::AddProjectField::RepoList;
                    ap.path_suggestion = None;
                    return;
                } else {
                    ap.field = super::modals::AddProjectField::Name;
                    ap.path_suggestion = None;
                    return;
                }
            }
            KeyCode::BackTab => {
                ap.field = super::modals::AddProjectField::Name;
                ap.path_suggestion = None;
                return;
            }
            KeyCode::Enter => {
                let path = ap.path.value().trim().to_string();
                if !path.is_empty() {
                    ap.repos.push(PathBuf::from(path));
                    ap.repo_index = ap.repos.len().saturating_sub(1);
                    ap.path.clear();
                    ap.path_suggestion = None;
                }
                return;
            }
            KeyCode::Backspace => ap.path.backspace(),
            KeyCode::Delete => ap.path.delete(),
            KeyCode::Left => ap.path.move_left(),
            KeyCode::Right => ap.path.move_right(),
            KeyCode::Home => ap.path.home(),
            KeyCode::End => ap.path.end(),
            KeyCode::Char(c) => ap.path.insert(c),
            _ => return,
        }
        self.update_path_suggestion();
    }

    fn handle_add_project_repo_list_key(&mut self, code: KeyCode) {
        let super::modals::Modal::AddProject(ref mut ap) = self.modal else {
            return;
        };
        match code {
            KeyCode::Esc => self.close_add_project_modal(),
            KeyCode::Tab => {
                ap.field = super::modals::AddProjectField::Name;
            }
            KeyCode::BackTab => {
                ap.field = super::modals::AddProjectField::Path;
            }
            KeyCode::Char('j') | KeyCode::Down => {
                if ap.repo_index + 1 < ap.repos.len() {
                    ap.repo_index += 1;
                }
            }
            KeyCode::Char('k') | KeyCode::Up => {
                ap.repo_index = ap.repo_index.saturating_sub(1);
            }
            KeyCode::Char('d') => {
                if !ap.repos.is_empty() {
                    ap.repos.remove(ap.repo_index);
                    if ap.repo_index >= ap.repos.len() && ap.repo_index > 0 {
                        ap.repo_index -= 1;
                    }
                    // If list becomes empty, switch to Path field
                    if ap.repos.is_empty() {
                        ap.field = super::modals::AddProjectField::Path;
                    }
                }
            }
            KeyCode::Enter => self.submit_add_project(),
            _ => {}
        }
    }

    /// Recompute path suggestion (fish-style: only when cursor is at end).
    fn update_path_suggestion(&mut self) {
        let super::modals::Modal::AddProject(ref mut ap) = self.modal else {
            return;
        };
        let value = ap.path.value().to_string();
        let at_end = ap.path.cursor_pos() == value.chars().count();
        if at_end && !value.is_empty() {
            ap.path_suggestion = paths::complete_directory_path(&value);
        } else {
            ap.path_suggestion = None;
        }
    }

    /// Close the add-project modal and clear all related state.
    pub(crate) fn close_add_project_modal(&mut self) {
        self.modal.close();
    }

    fn handle_edit_project_key(&mut self, code: KeyCode) {
        let super::modals::Modal::EditProject(ref ep) = self.modal else {
            return;
        };
        match ep.field {
            EditProjectField::Name => self.handle_edit_project_name_key(code),
            EditProjectField::Path => self.handle_edit_project_path_key(code),
            EditProjectField::RepoList => self.handle_edit_project_repo_list_key(code),
            EditProjectField::Roles => self.handle_edit_project_roles_key(code),
            EditProjectField::McpServers => self.handle_edit_project_mcp_servers_key(code),
        }
    }

    fn handle_edit_project_name_key(&mut self, code: KeyCode) {
        let super::modals::Modal::EditProject(ref mut ep) = self.modal else {
            return;
        };
        match code {
            KeyCode::Esc => self.close_edit_project_modal(),
            KeyCode::Tab => {
                ep.field = EditProjectField::Path;
            }
            KeyCode::BackTab => {
                ep.field = EditProjectField::McpServers;
            }
            KeyCode::Enter => self.submit_edit_project(),
            KeyCode::Backspace => {
                let super::modals::Modal::EditProject(ref mut ep) = self.modal else {
                    return;
                };
                ep.name.backspace();
            }
            KeyCode::Delete => {
                let super::modals::Modal::EditProject(ref mut ep) = self.modal else {
                    return;
                };
                ep.name.delete();
            }
            KeyCode::Left => {
                let super::modals::Modal::EditProject(ref mut ep) = self.modal else {
                    return;
                };
                ep.name.move_left();
            }
            KeyCode::Right => {
                let super::modals::Modal::EditProject(ref mut ep) = self.modal else {
                    return;
                };
                ep.name.move_right();
            }
            KeyCode::Home => {
                let super::modals::Modal::EditProject(ref mut ep) = self.modal else {
                    return;
                };
                ep.name.home();
            }
            KeyCode::End => {
                let super::modals::Modal::EditProject(ref mut ep) = self.modal else {
                    return;
                };
                ep.name.end();
            }
            KeyCode::Char(c) => {
                let super::modals::Modal::EditProject(ref mut ep) = self.modal else {
                    return;
                };
                ep.name.insert(c);
            }
            _ => {}
        }
    }

    fn handle_edit_project_path_key(&mut self, code: KeyCode) {
        let super::modals::Modal::EditProject(ref mut ep) = self.modal else {
            return;
        };
        match code {
            KeyCode::Esc => {
                self.close_edit_project_modal();
                return;
            }
            KeyCode::Tab => {
                if let Some(suggestion) = ep.path_suggestion.take() {
                    for c in suggestion.chars() {
                        ep.path.insert(c);
                    }
                } else if !ep.repos.is_empty() {
                    ep.field = EditProjectField::RepoList;
                    ep.path_suggestion = None;
                    return;
                } else {
                    ep.field = EditProjectField::Roles;
                    ep.path_suggestion = None;
                    return;
                }
            }
            KeyCode::BackTab => {
                ep.field = EditProjectField::Name;
                ep.path_suggestion = None;
                return;
            }
            KeyCode::Enter => {
                let path = ep.path.value().trim().to_string();
                if !path.is_empty() {
                    ep.repos.push(PathBuf::from(path));
                    ep.repo_index = ep.repos.len().saturating_sub(1);
                    ep.path.clear();
                    ep.path_suggestion = None;
                }
                return;
            }
            KeyCode::Backspace => ep.path.backspace(),
            KeyCode::Delete => ep.path.delete(),
            KeyCode::Left => ep.path.move_left(),
            KeyCode::Right => ep.path.move_right(),
            KeyCode::Home => ep.path.home(),
            KeyCode::End => ep.path.end(),
            KeyCode::Char(c) => ep.path.insert(c),
            _ => return,
        }
        self.update_edit_path_suggestion();
    }

    fn handle_edit_project_repo_list_key(&mut self, code: KeyCode) {
        let super::modals::Modal::EditProject(ref mut ep) = self.modal else {
            return;
        };
        match code {
            KeyCode::Esc => self.close_edit_project_modal(),
            KeyCode::Tab => {
                ep.field = EditProjectField::Roles;
            }
            KeyCode::BackTab => {
                ep.field = EditProjectField::Path;
            }
            KeyCode::Char('j') | KeyCode::Down => {
                if ep.repo_index + 1 < ep.repos.len() {
                    ep.repo_index += 1;
                }
            }
            KeyCode::Char('k') | KeyCode::Up => {
                ep.repo_index = ep.repo_index.saturating_sub(1);
            }
            KeyCode::Char('d') => {
                if !ep.repos.is_empty() {
                    ep.repos.remove(ep.repo_index);
                    if ep.repo_index >= ep.repos.len() && ep.repo_index > 0 {
                        ep.repo_index -= 1;
                    }
                    // If list becomes empty, switch to Path field
                    if ep.repos.is_empty() {
                        ep.field = EditProjectField::Path;
                    }
                }
            }
            KeyCode::Enter => self.submit_edit_project(),
            _ => {}
        }
    }

    fn handle_edit_project_roles_key(&mut self, code: KeyCode) {
        let super::modals::Modal::EditProject(ref mut ep) = self.modal else {
            return;
        };
        match code {
            KeyCode::Esc => self.submit_edit_project(),
            KeyCode::Tab => {
                ep.field = EditProjectField::McpServers;
            }
            KeyCode::BackTab => {
                if !ep.repos.is_empty() {
                    ep.field = EditProjectField::RepoList;
                } else {
                    ep.field = EditProjectField::Path;
                }
            }
            KeyCode::Char('j') | KeyCode::Down => {
                if !ep.role_editor_roles.is_empty()
                    && ep.role_editor_list_index + 1 < ep.role_editor_roles.len()
                {
                    ep.role_editor_list_index += 1;
                }
            }
            KeyCode::Char('k') | KeyCode::Up => {
                ep.role_editor_list_index = ep.role_editor_list_index.saturating_sub(1);
            }
            KeyCode::Char('a') => {
                self.prepare_new_role_editor();
                self.show_role_editor = true;
            }
            KeyCode::Char('e') | KeyCode::Enter => {
                let super::modals::Modal::EditProject(ref ep) = self.modal else {
                    return;
                };
                if !ep.role_editor_roles.is_empty() {
                    let idx = ep.role_editor_list_index;
                    self.open_role_for_editing(idx);
                    self.show_role_editor = true;
                }
            }
            KeyCode::Char('d') => {
                let super::modals::Modal::EditProject(ref mut ep) = self.modal else {
                    return;
                };
                if !ep.role_editor_roles.is_empty() {
                    ep.role_editor_roles.remove(ep.role_editor_list_index);
                    if ep.role_editor_list_index >= ep.role_editor_roles.len()
                        && ep.role_editor_list_index > 0
                    {
                        ep.role_editor_list_index -= 1;
                    }
                }
            }
            _ => {}
        }
    }

    /// Recompute path suggestion for edit-project modal.
    fn update_edit_path_suggestion(&mut self) {
        let super::modals::Modal::EditProject(ref mut ep) = self.modal else {
            return;
        };
        let value = ep.path.value().to_string();
        let at_end = ep.path.cursor_pos() == value.chars().count();
        if at_end && !value.is_empty() {
            ep.path_suggestion = paths::complete_directory_path(&value);
        } else {
            ep.path_suggestion = None;
        }
    }

    fn handle_delete_project_key(&mut self, code: KeyCode) {
        let super::modals::Modal::DeleteProject(ref mut dp) = self.modal else {
            return;
        };
        match code {
            KeyCode::Enter => {
                self.delete_active_project();
            }
            KeyCode::Esc => {
                self.modal.close();
            }
            KeyCode::Char(c) => {
                dp.confirmation.insert(c);
                dp.error = None; // Clear error on new input
            }
            KeyCode::Backspace => {
                dp.confirmation.backspace();
                dp.error = None;
            }
            KeyCode::Delete => {
                dp.confirmation.delete();
                dp.error = None;
            }
            KeyCode::Left => {
                dp.confirmation.move_left();
            }
            KeyCode::Right => {
                dp.confirmation.move_right();
            }
            KeyCode::Home => {
                dp.confirmation.home();
            }
            KeyCode::End => {
                dp.confirmation.end();
            }
            _ => {}
        }
    }

    fn handle_restore_sessions_key(&mut self, code: KeyCode) {
        let super::modals::Modal::RestoreSessions(ref mut rs) = self.modal else {
            return;
        };
        match code {
            KeyCode::Esc => {
                self.modal.close();
            }
            KeyCode::Char('j') | KeyCode::Down => {
                if !rs.list.is_empty() && rs.index + 1 < rs.list.len() {
                    rs.index += 1;
                }
            }
            KeyCode::Char('k') | KeyCode::Up => {
                rs.index = rs.index.saturating_sub(1);
            }
            KeyCode::Enter => {
                if rs.list.is_empty() {
                    return;
                }
                let deleted = rs.list.remove(rs.index);
                if rs.index >= rs.list.len() && rs.index > 0 {
                    rs.index -= 1;
                }
                self.modal.close();
                self.restore_deleted_session(deleted);
            }
            _ => {}
        }
    }

    fn handle_repo_selector_key(&mut self, code: KeyCode) {
        let repo_count = self
            .active_project()
            .map(|p| p.config.repos.len())
            .unwrap_or(0);
        let super::modals::Modal::RepoSelector(ref mut rs) = self.modal else {
            return;
        };
        match code {
            KeyCode::Esc => {
                self.modal.close();
            }
            KeyCode::Char('j') | KeyCode::Down => {
                if rs.index + 1 < repo_count {
                    rs.index += 1;
                }
            }
            KeyCode::Char('k') | KeyCode::Up => {
                rs.index = rs.index.saturating_sub(1);
            }
            KeyCode::Enter => {
                let idx = rs.index;
                let path = self
                    .active_project()
                    .and_then(|p| p.config.repos.get(idx).cloned());
                if let Some(path) = path {
                    self.pending_repo_path = Some(path);
                    self.modal =
                        super::modals::Modal::SessionMode(super::modals::SessionModeModal {
                            index: 0,
                        });
                }
            }
            _ => {}
        }
    }

    fn handle_session_mode_key(&mut self, code: KeyCode) {
        let super::modals::Modal::SessionMode(ref mut sm) = self.modal else {
            return;
        };
        // Build the dynamic mode list matching the UI modal.
        let mode_state = crate::ui::session_mode_modal::SessionModeState {
            selected_index: sm.index,
            devcontainer_available: self.backends.has("devcontainer"),
            vm_available: self.backends.has("qemu-vm"),
        };
        let modes = mode_state.mode_names();
        let max_index = modes.len().saturating_sub(1);

        match code {
            KeyCode::Esc => {
                self.modal.close();
                self.pending_repo_path = None;
                self.pending_all_repos = None;
            }
            KeyCode::Char('j') | KeyCode::Down => {
                if sm.index < max_index {
                    sm.index += 1;
                }
            }
            KeyCode::Char('k') | KeyCode::Up => {
                sm.index = sm.index.saturating_sub(1);
            }
            KeyCode::Enter => {
                let selected_mode = modes.get(sm.index).copied().unwrap_or("");
                self.modal.close();
                match selected_mode {
                    "Normal" => {
                        if let Some(all_repos) = self.pending_all_repos.take() {
                            self.pending_repo_path = None;
                            let config = SessionConfig {
                                cwd: Some(all_repos[0].clone()),
                                additional_dirs: all_repos[1..].to_vec(),
                                ..SessionConfig::default()
                            };
                            self.spawn_session_with_config(&config);
                        } else if let Some(path) = self.pending_repo_path.take() {
                            self.spawn_session_in_repo(path);
                        }
                    }
                    "Worktree" => {
                        self.start_branch_selection();
                    }
                    "Container" => {
                        // Container mode — build config for role selection after
                        // container is ready, store MCP servers for writing into container.
                        let config = if let Some(all_repos) = self.pending_all_repos.clone() {
                            SessionConfig {
                                cwd: Some(all_repos[0].clone()),
                                additional_dirs: all_repos[1..].to_vec(),
                                ..SessionConfig::default()
                            }
                        } else if let Some(ref path) = self.pending_repo_path {
                            SessionConfig {
                                cwd: Some(path.clone()),
                                ..SessionConfig::default()
                            }
                        } else {
                            SessionConfig::default()
                        };
                        self.pending_container_config = Some(config);
                        self.pending_container_mcp_servers = self
                            .active_project()
                            .map(|p| p.config.mcp_servers.clone())
                            .filter(|s| !s.is_empty());

                        // Show containerfile picker (skip if only one file)
                        let containerfiles = self.load_containerfiles();
                        if containerfiles.len() <= 1 {
                            self.pending_containerfile_name = containerfiles
                                .first()
                                .cloned()
                                .or_else(|| Some("default".to_string()));
                            self.start_container_provisioning();
                        } else {
                            self.modal = super::modals::Modal::ContainerfilePicker(
                                super::modals::ContainerfilePickerModal {
                                    index: 0,
                                    list: containerfiles,
                                },
                            );
                        }
                    }
                    "VM" => {
                        // VM mode — build config for role selection after
                        // VM is ready, store MCP servers for writing into the VM.
                        let config = if let Some(all_repos) = self.pending_all_repos.clone() {
                            SessionConfig {
                                cwd: Some(all_repos[0].clone()),
                                additional_dirs: all_repos[1..].to_vec(),
                                ..SessionConfig::default()
                            }
                        } else if let Some(ref path) = self.pending_repo_path {
                            SessionConfig {
                                cwd: Some(path.clone()),
                                ..SessionConfig::default()
                            }
                        } else {
                            SessionConfig::default()
                        };
                        self.pending_vm_config = Some(config);
                        self.pending_vm_mcp_servers = self
                            .active_project()
                            .map(|p| p.config.mcp_servers.clone())
                            .filter(|s| !s.is_empty());
                        self.start_vm_provisioning();
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }

    fn handle_containerfile_picker_key(&mut self, code: KeyCode) {
        let super::modals::Modal::ContainerfilePicker(ref mut cp) = self.modal else {
            return;
        };
        let max_index = cp.list.len().saturating_sub(1);
        match code {
            KeyCode::Esc => {
                self.modal.close();
                self.pending_container_config = None;
                self.pending_container_mcp_servers = None;
                self.pending_repo_path = None;
                self.pending_all_repos = None;
            }
            KeyCode::Char('j') | KeyCode::Down => {
                if cp.index < max_index {
                    cp.index += 1;
                }
            }
            KeyCode::Char('k') | KeyCode::Up => {
                cp.index = cp.index.saturating_sub(1);
            }
            KeyCode::Enter => {
                let name = cp
                    .list
                    .get(cp.index)
                    .cloned()
                    .unwrap_or_else(|| "default".to_string());
                self.modal.close();
                self.pending_containerfile_name = Some(name);
                self.start_container_provisioning();
            }
            _ => {}
        }
    }

    fn handle_branch_selector_key(&mut self, code: KeyCode) {
        let super::modals::Modal::BranchSelector(ref mut bs) = self.modal else {
            return;
        };
        match code {
            KeyCode::Esc => {
                self.modal.close();
                self.pending_repo_path = None;
                self.pending_all_repos = None;
            }
            KeyCode::Char('j') | KeyCode::Down => {
                if bs.index + 1 < bs.branches.len() {
                    bs.index += 1;
                }
            }
            KeyCode::Char('k') | KeyCode::Up => {
                bs.index = bs.index.saturating_sub(1);
            }
            KeyCode::Enter => {
                let base_branch = bs.branches[bs.index].clone();
                self.pending_base_branch = Some(base_branch);
                self.modal =
                    super::modals::Modal::WorktreeName(super::modals::WorktreeNameModal::default());
            }
            _ => {}
        }
    }

    fn handle_worktree_name_key(&mut self, code: KeyCode) {
        let super::modals::Modal::WorktreeName(ref mut wn) = self.modal else {
            return;
        };
        match code {
            KeyCode::Esc => {
                self.modal.close();
                self.pending_base_branch = None;
                self.pending_repo_path = None;
                self.pending_all_repos = None;
            }
            KeyCode::Enter => {
                let new_branch = wn.name.value().trim().to_string();
                if new_branch.is_empty() {
                    self.set_error("Branch name cannot be empty");
                    return;
                }
                self.modal.close();
                if let Some(base_branch) = self.pending_base_branch.take() {
                    // Use all repos for multi-repo projects, single repo otherwise
                    let repo_paths = if let Some(all_repos) = self.pending_all_repos.take() {
                        self.pending_repo_path = None;
                        all_repos
                    } else if let Some(repo_path) = self.pending_repo_path.take() {
                        vec![repo_path]
                    } else {
                        return;
                    };
                    self.spawn_worktree_session(&repo_paths, &new_branch, &base_branch);
                }
            }
            KeyCode::Backspace => wn.name.backspace(),
            KeyCode::Delete => wn.name.delete(),
            KeyCode::Left => wn.name.move_left(),
            KeyCode::Right => wn.name.move_right(),
            KeyCode::Home => wn.name.home(),
            KeyCode::End => wn.name.end(),
            KeyCode::Char(c) => wn.name.insert(c),
            _ => {}
        }
    }

    fn handle_schedule_command_key(&mut self, code: KeyCode) {
        use super::modals::ScheduleCommandField;

        let super::modals::Modal::ScheduleCommand(ref mut sc) = self.modal else {
            return;
        };
        match code {
            KeyCode::Esc => {
                self.modal.close();
            }
            KeyCode::Tab | KeyCode::BackTab => {
                sc.field = match sc.field {
                    ScheduleCommandField::Command => ScheduleCommandField::Delay,
                    ScheduleCommandField::Delay => ScheduleCommandField::Command,
                };
            }
            KeyCode::Enter => {
                self.submit_schedule_command();
            }
            KeyCode::Char(c) => {
                if sc.field == ScheduleCommandField::Delay && !c.is_ascii_digit() {
                    return;
                }
                sc.active_field_mut().insert(c);
            }
            KeyCode::Backspace => sc.active_field_mut().backspace(),
            KeyCode::Delete => sc.active_field_mut().delete(),
            KeyCode::Left => sc.active_field_mut().move_left(),
            KeyCode::Right => sc.active_field_mut().move_right(),
            KeyCode::Home => sc.active_field_mut().home(),
            KeyCode::End => sc.active_field_mut().end(),
            _ => {}
        }
    }

    fn handle_role_selector_key(&mut self, code: KeyCode) {
        let role_count = self
            .active_project()
            .map(|p| p.config.roles.len())
            .unwrap_or(0);
        let super::modals::Modal::RoleSelector(ref mut rsel) = self.modal else {
            return;
        };
        match code {
            KeyCode::Esc => {
                self.modal.close();
                self.pending_spawn_config = None;
                self.pending_spawn_worktrees.clear();
                self.pending_spawn_name = None;
                self.pending_vm_id = None;
                // Undo the counter increment from prepare_spawn()
                self.session_counter = self.session_counter.saturating_sub(1);
            }
            KeyCode::Char('j') | KeyCode::Down => {
                if rsel.index + 1 < role_count {
                    rsel.index += 1;
                }
            }
            KeyCode::Char('k') | KeyCode::Up => {
                rsel.index = rsel.index.saturating_sub(1);
            }
            KeyCode::Enter => {
                let role_index = rsel.index;
                self.modal.close();
                if let (Some(mut config), Some(name)) = (
                    self.pending_spawn_config.take(),
                    self.pending_spawn_name.take(),
                ) {
                    if let Some(project) = self.active_project() {
                        if let Some(role) = project.config.roles.get(role_index) {
                            config.role = role.name.clone();
                            config.permissions = role.permissions.clone();
                            let worktrees = std::mem::take(&mut self.pending_spawn_worktrees);
                            self.do_spawn_session(name, &config, worktrees, None);
                        }
                    }
                }
            }
            _ => {}
        }
    }

    /// Navigate the role list — used by tests to simulate inline role list actions.
    #[cfg(test)]
    pub(crate) fn handle_role_editor_list_key(&mut self, code: KeyCode) {
        let super::modals::Modal::EditProject(ref mut ep) = self.modal else {
            return;
        };
        match code {
            KeyCode::Esc => {
                // Save & close
                let roles_to_save = ep.role_editor_roles.clone();
                if let Some(project) = self.active_project_mut() {
                    project.config.roles = roles_to_save;
                    let project_clone = project.clone();
                    self.save_project_to_db(&project_clone);
                }
                self.show_role_editor = false;
            }
            KeyCode::Char('j') | KeyCode::Down => {
                if !ep.role_editor_roles.is_empty()
                    && ep.role_editor_list_index + 1 < ep.role_editor_roles.len()
                {
                    ep.role_editor_list_index += 1;
                }
            }
            KeyCode::Char('k') | KeyCode::Up => {
                ep.role_editor_list_index = ep.role_editor_list_index.saturating_sub(1);
            }
            KeyCode::Char('a') => {
                self.prepare_new_role_editor();
            }
            KeyCode::Char('e') | KeyCode::Enter => {
                let super::modals::Modal::EditProject(ref ep) = self.modal else {
                    return;
                };
                if !ep.role_editor_roles.is_empty() {
                    let idx = ep.role_editor_list_index;
                    self.open_role_for_editing(idx);
                }
            }
            KeyCode::Char('d') => {
                let super::modals::Modal::EditProject(ref mut ep) = self.modal else {
                    return;
                };
                if !ep.role_editor_roles.is_empty() {
                    ep.role_editor_roles.remove(ep.role_editor_list_index);
                    if ep.role_editor_list_index >= ep.role_editor_roles.len()
                        && ep.role_editor_list_index > 0
                    {
                        ep.role_editor_list_index -= 1;
                    }
                }
            }
            _ => {}
        }
    }

    pub(crate) fn handle_role_editor_editor_key(&mut self, code: KeyCode) {
        use crate::ui::role_editor_modal::{RoleEditorField, ToolListMode};

        match self.role_editor_field {
            RoleEditorField::AllowedTools
            | RoleEditorField::DisallowedTools
            | RoleEditorField::Env => {
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
                self.try_discard_role_editor();
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
                self.try_discard_role_editor();
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
        handle_tool_list_adding_key(self.active_tool_list_mut(), code);
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
            RoleEditorField::SystemPrompt => RoleEditorField::Env,
            RoleEditorField::Env => RoleEditorField::Name,
        }
    }

    fn prev_editor_field(
        field: crate::ui::role_editor_modal::RoleEditorField,
    ) -> crate::ui::role_editor_modal::RoleEditorField {
        use crate::ui::role_editor_modal::RoleEditorField;
        match field {
            RoleEditorField::Name => RoleEditorField::Env,
            RoleEditorField::Description => RoleEditorField::Name,
            RoleEditorField::AllowedTools => RoleEditorField::Description,
            RoleEditorField::DisallowedTools => RoleEditorField::AllowedTools,
            RoleEditorField::SystemPrompt => RoleEditorField::DisallowedTools,
            RoleEditorField::Env => RoleEditorField::SystemPrompt,
        }
    }

    /// Load roles from the active project into editor state — used by tests.
    ///
    /// This opens the EditProject modal and sets up role editing state.
    #[cfg(test)]
    pub(crate) fn open_role_editor(&mut self) {
        let Some(project) = self.active_project() else {
            return;
        };
        let roles = project.config.roles.clone();
        // Ensure the EditProject modal is open (tests may not have it open yet)
        if !matches!(self.modal, super::modals::Modal::EditProject(_)) {
            self.open_edit_project_modal();
        }
        if let super::modals::Modal::EditProject(ref mut ep) = self.modal {
            ep.role_editor_roles = roles;
            ep.role_editor_list_index = 0;
        }
        self.role_editor_view = RoleEditorView::List;
        self.show_role_editor = true;
    }

    /// Reset role editor fields to prepare for adding a new role.
    fn prepare_new_role_editor(&mut self) {
        self.role_editor_editing_index = None;
        self.role_editor_name.clear();
        self.role_editor_description.clear();
        self.role_editor_allowed_tools.reset();
        self.role_editor_disallowed_tools.reset();
        self.role_editor_system_prompt.clear();
        self.role_editor_env.reset();
        self.role_editor_field = crate::ui::role_editor_modal::RoleEditorField::Name;
        self.role_editor_view = RoleEditorView::Editor;
        self.role_editor_snapshot = Some(self.capture_role_editor_snapshot());
    }

    pub(crate) fn open_role_for_editing(&mut self, index: usize) {
        let super::modals::Modal::EditProject(ref ep) = self.modal else {
            return;
        };
        let Some(role) = ep.role_editor_roles.get(index) else {
            return;
        };
        let name = role.name.clone();
        let description = role.description.clone();
        let allowed = role.permissions.allowed_tools.clone();
        let disallowed = role.permissions.disallowed_tools.clone();
        let system_prompt = role
            .permissions
            .append_system_prompt
            .clone()
            .unwrap_or_default();
        let env_items: Vec<String> = role
            .permissions
            .env
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect();

        self.role_editor_editing_index = Some(index);
        self.role_editor_name.set(&name);
        self.role_editor_description.set(&description);
        self.role_editor_allowed_tools.load(&allowed);
        self.role_editor_disallowed_tools.load(&disallowed);
        self.role_editor_system_prompt.set(&system_prompt);
        self.role_editor_env.load(&env_items);
        self.role_editor_field = crate::ui::role_editor_modal::RoleEditorField::Name;
        self.role_editor_view = RoleEditorView::Editor;
        self.role_editor_snapshot = Some(self.capture_role_editor_snapshot());
    }

    pub(crate) fn active_tool_list_mut(&mut self) -> &mut super::ToolListState {
        match self.role_editor_field {
            crate::ui::role_editor_modal::RoleEditorField::AllowedTools => {
                &mut self.role_editor_allowed_tools
            }
            crate::ui::role_editor_modal::RoleEditorField::Env => &mut self.role_editor_env,
            _ => &mut self.role_editor_disallowed_tools,
        }
    }

    /// Attempt to discard role editor — shows confirmation if dirty.
    fn try_discard_role_editor(&mut self) {
        if self.is_role_editor_dirty() {
            self.show_discard_confirmation = true;
        } else {
            self.close_role_editor();
        }
    }

    /// Attempt to discard MCP editor — shows confirmation if dirty.
    fn try_discard_mcp_editor(&mut self) {
        if self.is_mcp_editor_dirty() {
            self.show_discard_confirmation = true;
        } else {
            self.close_mcp_editor();
        }
    }

    fn handle_edit_project_mcp_servers_key(&mut self, code: KeyCode) {
        let super::modals::Modal::EditProject(ref mut ep) = self.modal else {
            return;
        };
        match code {
            KeyCode::Esc => self.submit_edit_project(),
            KeyCode::Tab => {
                ep.field = EditProjectField::Name;
            }
            KeyCode::BackTab => {
                ep.field = EditProjectField::Roles;
            }
            KeyCode::Char('j') | KeyCode::Down => {
                if !ep.mcp_servers.is_empty() && ep.mcp_server_index + 1 < ep.mcp_servers.len() {
                    ep.mcp_server_index += 1;
                }
            }
            KeyCode::Char('k') | KeyCode::Up => {
                ep.mcp_server_index = ep.mcp_server_index.saturating_sub(1);
            }
            KeyCode::Char('a') => {
                self.prepare_new_mcp_editor();
                self.show_mcp_editor = true;
            }
            KeyCode::Char('e') | KeyCode::Enter => {
                let super::modals::Modal::EditProject(ref ep) = self.modal else {
                    return;
                };
                if !ep.mcp_servers.is_empty() {
                    let idx = ep.mcp_server_index;
                    self.open_mcp_server_for_editing(idx);
                    self.show_mcp_editor = true;
                }
            }
            KeyCode::Char('d') => {
                let super::modals::Modal::EditProject(ref mut ep) = self.modal else {
                    return;
                };
                if !ep.mcp_servers.is_empty() {
                    ep.mcp_servers.remove(ep.mcp_server_index);
                    if ep.mcp_server_index >= ep.mcp_servers.len() && ep.mcp_server_index > 0 {
                        ep.mcp_server_index -= 1;
                    }
                }
            }
            _ => {}
        }
    }

    pub(crate) fn handle_mcp_editor_key(&mut self, code: KeyCode) {
        use crate::ui::role_editor_modal::ToolListMode;

        match self.mcp_editor_field {
            McpEditorField::Args | McpEditorField::Env => {
                let tool_list = match self.mcp_editor_field {
                    McpEditorField::Args => &self.mcp_editor_args,
                    _ => &self.mcp_editor_env,
                };
                if tool_list.mode == ToolListMode::Adding {
                    self.handle_mcp_tool_adding_key(code);
                } else {
                    self.handle_mcp_tool_browse_key(code);
                }
                return;
            }
            _ => {}
        }

        // Text field handling (Name, Command).
        match code {
            KeyCode::Esc => {
                self.try_discard_mcp_editor();
            }
            KeyCode::Tab => {
                self.mcp_editor_field = Self::next_mcp_editor_field(self.mcp_editor_field);
            }
            KeyCode::BackTab => {
                self.mcp_editor_field = Self::prev_mcp_editor_field(self.mcp_editor_field);
            }
            KeyCode::Enter => {
                self.submit_mcp_editor();
            }
            _ => {
                let input = match self.mcp_editor_field {
                    McpEditorField::Name => &mut self.mcp_editor_name,
                    McpEditorField::Command => &mut self.mcp_editor_command,
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

    fn handle_mcp_tool_browse_key(&mut self, code: KeyCode) {
        match code {
            KeyCode::Esc => {
                self.try_discard_mcp_editor();
            }
            KeyCode::Tab => {
                self.mcp_editor_field = Self::next_mcp_editor_field(self.mcp_editor_field);
            }
            KeyCode::BackTab => {
                self.mcp_editor_field = Self::prev_mcp_editor_field(self.mcp_editor_field);
            }
            KeyCode::Enter => {
                self.submit_mcp_editor();
            }
            KeyCode::Char('a') => self.active_mcp_tool_list_mut().start_adding(),
            KeyCode::Char('d') => self.active_mcp_tool_list_mut().delete_selected(),
            KeyCode::Char('j') | KeyCode::Down => self.active_mcp_tool_list_mut().move_down(),
            KeyCode::Char('k') | KeyCode::Up => self.active_mcp_tool_list_mut().move_up(),
            _ => {}
        }
    }

    fn handle_mcp_tool_adding_key(&mut self, code: KeyCode) {
        handle_tool_list_adding_key(self.active_mcp_tool_list_mut(), code);
    }

    fn active_mcp_tool_list_mut(&mut self) -> &mut super::ToolListState {
        match self.mcp_editor_field {
            McpEditorField::Args => &mut self.mcp_editor_args,
            _ => &mut self.mcp_editor_env,
        }
    }

    fn next_mcp_editor_field(field: McpEditorField) -> McpEditorField {
        match field {
            McpEditorField::Name => McpEditorField::Command,
            McpEditorField::Command => McpEditorField::Args,
            McpEditorField::Args => McpEditorField::Env,
            McpEditorField::Env => McpEditorField::Name,
        }
    }

    fn prev_mcp_editor_field(field: McpEditorField) -> McpEditorField {
        match field {
            McpEditorField::Name => McpEditorField::Env,
            McpEditorField::Command => McpEditorField::Name,
            McpEditorField::Args => McpEditorField::Command,
            McpEditorField::Env => McpEditorField::Args,
        }
    }

    pub(crate) fn start_branch_selection(&mut self) {
        let Some(repo_path) = self.pending_repo_path.as_ref() else {
            return;
        };
        match crate::git::list_branches(repo_path) {
            Ok(branches) if branches.is_empty() => {
                self.set_error("No branches found in repository");
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
                self.modal =
                    super::modals::Modal::BranchSelector(super::modals::BranchSelectorModal {
                        index: 0,
                        branches,
                    });
            }
            Err(e) => {
                error!("Failed to list branches: {e}");
                self.set_error(format!("Failed to list branches: {e:#}"));
                self.pending_repo_path = None;
            }
        }
    }
}

/// Handle key input when a [`ToolListState`] is in Adding mode.
///
/// Shared between role editor and MCP editor tool list fields.
fn handle_tool_list_adding_key(list: &mut super::ToolListState, code: KeyCode) {
    match code {
        KeyCode::Esc => list.cancel_add(),
        KeyCode::Enter => list.confirm_add(),
        _ => {
            let input = &mut list.input;
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
