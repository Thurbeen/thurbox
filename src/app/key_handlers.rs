//! Key event handlers for the Thurbox TUI application.
//!
//! This module contains all keyboard input handling logic organized by context:
//! - Global keybindings (always active)
//! - Focus-based handlers (ProjectList, SessionList, Terminal)
//! - Modal handlers (AddProject, RepoSelector, BranchSelector, etc.)

use crate::session::SessionConfig;

use super::mcp_editor_modal::McpEditorField;
use super::{App, InputFocus, RoleEditorView, TerminalView};
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
        // Dismiss help overlay with Esc or F1 (toggle)
        if matches!(self.modal, super::modals::Modal::Help) {
            if code == KeyCode::Esc || code == KeyCode::F(1) {
                self.modal.close();
            }
            return;
        }

        // Restore sessions modal captures all input
        if matches!(self.modal, super::modals::Modal::RestoreSessions(_)) {
            self.handle_restore_sessions_key(code);
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

        // Session name modal captures all input
        if matches!(self.modal, super::modals::Modal::SessionName(_)) {
            self.handle_session_name_key(code);
            return;
        }

        // Schedule command modal captures all input
        if matches!(self.modal, super::modals::Modal::ScheduleCommand(_)) {
            self.handle_schedule_command_key(code);
            return;
        }

        // Scheduled commands list modal captures all input
        if matches!(self.modal, super::modals::Modal::ScheduledCommandsList(_)) {
            self.handle_scheduled_commands_list_key(code);
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

        // Settings overlay (tabbed list of roles / MCP servers) captures all input
        if self.show_settings {
            self.handle_settings_key(code);
            return;
        }

        // Role selector modal captures all input
        if matches!(self.modal, super::modals::Modal::RoleSelector(_)) {
            self.handle_role_selector_key(code);
            return;
        }

        // Repo picker modal captures all input
        if matches!(self.modal, super::modals::Modal::RepoPicker(_)) {
            self.handle_repo_picker_key(code);
            return;
        }

        // Search input captures all keys when active
        if self.search_active {
            self.handle_search_key(code, mods);
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
                    self.open_repo_picker();
                    return;
                }
                KeyCode::Char('a') => {
                    self.spawn_admin_session();
                    return;
                }
                KeyCode::Char('d') => match self.focus {
                    InputFocus::SessionList => {
                        self.close_active_session();
                        return;
                    }
                    InputFocus::Terminal => {} // forward to PTY
                },
                KeyCode::Char('e') => {
                    self.open_settings();
                    return;
                }
                KeyCode::Char('r') => match self.focus {
                    InputFocus::Terminal => {} // forward to PTY (e.g. bash reverse search)
                    _ => {
                        self.restart_active_session();
                        return;
                    }
                },
                KeyCode::Char('p') => {
                    self.open_scheduled_commands_list();
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
                // Vim navigation: h=cycle-left, j=down, k=up, l=cycle-right
                KeyCode::Char('h') => {
                    self.clear_search();
                    self.focus = match self.focus {
                        InputFocus::SessionList => InputFocus::Terminal,
                        InputFocus::Terminal => InputFocus::SessionList,
                    };
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
                    self.clear_search();
                    self.focus = match self.focus {
                        InputFocus::SessionList => InputFocus::Terminal,
                        InputFocus::Terminal => InputFocus::SessionList,
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
            InputFocus::SessionList => self.handle_session_list_key(code),
            InputFocus::Terminal => self.handle_terminal_key(code, mods),
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
            KeyCode::Char('/') => {
                self.search_active = true;
                self.search_input.buffer.clear();
                self.search_input.cursor = 0;
                self.session_match_positions.clear();
            }
            _ => {}
        }
    }

    fn handle_search_key(&mut self, code: KeyCode, mods: KeyModifiers) {
        // Allow Ctrl+Q to quit even during search
        if code == KeyCode::Char('q') && mods.contains(KeyModifiers::CONTROL) {
            self.should_quit = true;
            return;
        }

        match code {
            KeyCode::Esc => {
                // Cancel search and clear filter
                self.clear_search();
            }
            KeyCode::Enter => {
                // Confirm search: exit input mode but keep filter active
                self.search_active = false;
            }
            KeyCode::Backspace => {
                self.search_input.backspace();
                self.recompute_search_filter();
            }
            KeyCode::Left => {
                self.search_input.move_left();
            }
            KeyCode::Right => {
                self.search_input.move_right();
            }
            KeyCode::Up => {
                self.search_navigate_backward();
            }
            KeyCode::Down => {
                self.search_navigate_forward();
            }
            KeyCode::Char('j') if mods.contains(KeyModifiers::CONTROL) => {
                self.search_navigate_forward();
            }
            KeyCode::Char('k') if mods.contains(KeyModifiers::CONTROL) => {
                self.search_navigate_backward();
            }
            KeyCode::Char(c) => {
                if !mods.contains(KeyModifiers::CONTROL) {
                    self.search_input.insert(c);
                    self.recompute_search_filter();
                }
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
                self.pending_normal_repos.clear();
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
                        self.pending_container_mcp_servers = if self.global_mcp_servers.is_empty() {
                            None
                        } else {
                            Some(self.global_mcp_servers.clone())
                        };

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
                        self.pending_vm_mcp_servers = if self.global_mcp_servers.is_empty() {
                            None
                        } else {
                            Some(self.global_mcp_servers.clone())
                        };
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
                self.pending_normal_repos.clear();
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
                self.pending_normal_repos.clear();
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

    fn handle_session_name_key(&mut self, code: KeyCode) {
        let super::modals::Modal::SessionName(ref mut sn) = self.modal else {
            return;
        };
        match code {
            KeyCode::Esc => {
                self.modal.close();
                self.pending_spawn_config = None;
                self.pending_spawn_worktrees.clear();
                self.pending_vm_id = None;
                // Undo the counter increment from prepare_spawn().
                self.session_counter = self.session_counter.saturating_sub(1);
            }
            KeyCode::Enter => {
                let name = sn.name.value().trim().to_string();
                if name.is_empty() {
                    self.set_error("Session name cannot be empty");
                    return;
                }
                self.modal.close();
                if let Some(config) = self.pending_spawn_config.take() {
                    let worktrees = std::mem::take(&mut self.pending_spawn_worktrees);
                    self.finish_prepare_spawn(name, config, worktrees);
                }
            }
            KeyCode::Backspace => sn.name.backspace(),
            KeyCode::Delete => sn.name.delete(),
            KeyCode::Left => sn.name.move_left(),
            KeyCode::Right => sn.name.move_right(),
            KeyCode::Home => sn.name.home(),
            KeyCode::End => sn.name.end(),
            KeyCode::Char(c) => sn.name.insert(c),
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

    fn handle_scheduled_commands_list_key(&mut self, code: KeyCode) {
        if let super::modals::Modal::ScheduledCommandsList(ref mut scl) = self.modal {
            match code {
                KeyCode::Esc => {
                    self.modal.close();
                    return;
                }
                KeyCode::Char('j') | KeyCode::Down => {
                    if scl.index + 1 < scl.commands.len() {
                        scl.index += 1;
                    }
                    return;
                }
                KeyCode::Char('k') | KeyCode::Up => {
                    scl.index = scl.index.saturating_sub(1);
                    return;
                }
                _ => {}
            }
        }

        match code {
            KeyCode::Enter => self.cancel_selected_scheduled_command(),
            KeyCode::Char('n') => {
                self.modal.close();
                self.open_schedule_command_modal();
            }
            KeyCode::Char('e') => self.edit_selected_scheduled_command(),
            _ => {}
        }
    }

    /// Cancel the currently selected command in the list modal.
    fn cancel_selected_scheduled_command(&mut self) {
        let id = {
            let super::modals::Modal::ScheduledCommandsList(ref mut scl) = self.modal else {
                return;
            };
            if scl.commands.is_empty() {
                return;
            }
            let entry = scl.commands.remove(scl.index);
            if scl.index >= scl.commands.len() && scl.index > 0 {
                scl.index -= 1;
            }
            entry.id
        };
        self.cancel_scheduled_command_by_id(id);
        if let super::modals::Modal::ScheduledCommandsList(ref scl) = self.modal {
            if scl.commands.is_empty() {
                self.modal.close();
            }
        }
    }

    /// Open the edit modal for the currently selected command in the list.
    fn edit_selected_scheduled_command(&mut self) {
        let entry = {
            let super::modals::Modal::ScheduledCommandsList(ref scl) = self.modal else {
                return;
            };
            if scl.commands.is_empty() {
                return;
            }
            scl.commands[scl.index].clone()
        };
        self.open_edit_scheduled_command(entry);
    }

    fn handle_role_selector_key(&mut self, code: KeyCode) {
        let role_count = self.global_roles.len();
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
                    if let Some(role) = self.global_roles.get(role_index) {
                        config.role = role.name.clone();
                        config.permissions = role.permissions.clone();
                        let worktrees = std::mem::take(&mut self.pending_spawn_worktrees);
                        self.do_spawn_session(name, &config, worktrees, false);
                    }
                }
            }
            _ => {}
        }
    }

    /// Navigate the role list — used by tests to simulate inline role list actions.
    #[cfg(test)]
    pub(crate) fn handle_role_editor_list_key(&mut self, code: KeyCode) {
        match code {
            KeyCode::Esc => {
                // Save & close
                if let Err(e) = self.db.replace_global_roles(&self.global_roles) {
                    tracing::error!("Failed to save global roles: {e}");
                }
                self.show_role_editor = false;
            }
            KeyCode::Char('j') | KeyCode::Down => {
                if !self.global_roles.is_empty()
                    && self.role_editor_list_index + 1 < self.global_roles.len()
                {
                    self.role_editor_list_index += 1;
                }
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.role_editor_list_index = self.role_editor_list_index.saturating_sub(1);
            }
            KeyCode::Char('a') => {
                self.prepare_new_role_editor();
            }
            KeyCode::Char('e') | KeyCode::Enter => {
                if !self.global_roles.is_empty() {
                    let idx = self.role_editor_list_index;
                    self.open_role_for_editing(idx);
                }
            }
            KeyCode::Char('d') => {
                if !self.global_roles.is_empty() {
                    self.global_roles.remove(self.role_editor_list_index);
                    if self.role_editor_list_index >= self.global_roles.len()
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

    /// Open the settings overlay (tabbed list of roles / MCP servers).
    pub(crate) fn open_settings(&mut self) {
        self.settings_tab = super::SettingsTab::Roles;
        self.role_editor_list_index = 0;
        self.mcp_server_list_index = 0;
        self.show_settings = true;
    }

    /// Handle input in the settings overlay (tabbed list view).
    pub(crate) fn handle_settings_key(&mut self, code: KeyCode) {
        match code {
            KeyCode::Esc => {
                // Save and close settings
                if let Err(e) = self.db.replace_global_roles(&self.global_roles) {
                    tracing::error!("Failed to save global roles: {e}");
                }
                if let Err(e) = self.db.replace_global_mcp_servers(&self.global_mcp_servers) {
                    tracing::error!("Failed to save global MCP servers: {e}");
                }
                self.show_settings = false;
            }
            KeyCode::Tab | KeyCode::BackTab => {
                self.settings_tab = match self.settings_tab {
                    super::SettingsTab::Roles => super::SettingsTab::McpServers,
                    super::SettingsTab::McpServers => super::SettingsTab::Roles,
                };
            }
            _ => match self.settings_tab {
                super::SettingsTab::Roles => self.handle_settings_roles_key(code),
                super::SettingsTab::McpServers => self.handle_settings_mcp_key(code),
            },
        }
    }

    /// Handle keys for the Roles tab in the settings overlay.
    fn handle_settings_roles_key(&mut self, code: KeyCode) {
        match code {
            KeyCode::Char('j') | KeyCode::Down => {
                if !self.global_roles.is_empty()
                    && self.role_editor_list_index + 1 < self.global_roles.len()
                {
                    self.role_editor_list_index += 1;
                }
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.role_editor_list_index = self.role_editor_list_index.saturating_sub(1);
            }
            KeyCode::Char('a') => {
                self.prepare_new_role_editor();
                self.show_role_editor = true;
            }
            KeyCode::Char('e') | KeyCode::Enter => {
                if !self.global_roles.is_empty() {
                    let idx = self.role_editor_list_index;
                    self.open_role_for_editing(idx);
                    self.show_role_editor = true;
                }
            }
            KeyCode::Char('d') => {
                if !self.global_roles.is_empty() {
                    self.global_roles.remove(self.role_editor_list_index);
                    if self.role_editor_list_index >= self.global_roles.len()
                        && self.role_editor_list_index > 0
                    {
                        self.role_editor_list_index -= 1;
                    }
                }
            }
            _ => {}
        }
    }

    /// Handle keys for the MCP Servers tab in the settings overlay.
    fn handle_settings_mcp_key(&mut self, code: KeyCode) {
        match code {
            KeyCode::Char('j') | KeyCode::Down => {
                if !self.global_mcp_servers.is_empty()
                    && self.mcp_server_list_index + 1 < self.global_mcp_servers.len()
                {
                    self.mcp_server_list_index += 1;
                }
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.mcp_server_list_index = self.mcp_server_list_index.saturating_sub(1);
            }
            KeyCode::Char('a') => {
                self.open_new_mcp_editor();
            }
            KeyCode::Char('e') | KeyCode::Enter => {
                if !self.global_mcp_servers.is_empty() {
                    let idx = self.mcp_server_list_index;
                    self.open_mcp_for_editing(idx);
                }
            }
            KeyCode::Char('d') => {
                if !self.global_mcp_servers.is_empty() {
                    self.global_mcp_servers.remove(self.mcp_server_list_index);
                    if self.mcp_server_list_index >= self.global_mcp_servers.len()
                        && self.mcp_server_list_index > 0
                    {
                        self.mcp_server_list_index -= 1;
                    }
                }
            }
            _ => {}
        }
    }

    /// Open the MCP editor for a new server.
    fn open_new_mcp_editor(&mut self) {
        self.prepare_new_mcp_editor();
        self.show_mcp_editor = true;
    }

    /// Open the MCP editor for an existing server at the given index.
    fn open_mcp_for_editing(&mut self, idx: usize) {
        self.open_mcp_server_for_editing(idx);
        self.show_mcp_editor = true;
    }

    /// Open the global role editor (list view).
    #[allow(dead_code)]
    pub(crate) fn open_role_editor(&mut self) {
        self.role_editor_list_index = 0;
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
        let Some(role) = self.global_roles.get(index) else {
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

    // ── Repo Picker Modal ────────────────────────────────────────────────

    fn handle_repo_picker_key(&mut self, code: KeyCode) {
        let super::modals::Modal::RepoPicker(ref rp) = self.modal else {
            return;
        };
        match rp.focus {
            super::modals::RepoPickerFocus::List => self.handle_repo_picker_list_key(code),
            super::modals::RepoPickerFocus::Input => self.handle_repo_picker_input_key(code),
        }
    }

    fn handle_repo_picker_list_key(&mut self, code: KeyCode) {
        let super::modals::Modal::RepoPicker(ref mut rp) = self.modal else {
            return;
        };
        match code {
            KeyCode::Esc => {
                self.modal.close();
            }
            KeyCode::Tab => {
                rp.focus = super::modals::RepoPickerFocus::Input;
            }
            KeyCode::Char('j') | KeyCode::Down => {
                if !rp.bookmarks.is_empty() && rp.list_index + 1 < rp.bookmarks.len() {
                    rp.list_index += 1;
                }
            }
            KeyCode::Char('k') | KeyCode::Up => {
                rp.list_index = rp.list_index.saturating_sub(1);
            }
            KeyCode::Char(' ') => {
                if let Some(sel) = rp.selected.get_mut(rp.list_index) {
                    *sel = !*sel;
                }
            }
            KeyCode::Char('w') => {
                let idx = rp.list_index;
                if let Some(wt) = rp.worktree.get_mut(idx) {
                    *wt = !*wt;
                    // Auto-select when toggling worktree on
                    if *wt {
                        if let Some(sel) = rp.selected.get_mut(idx) {
                            *sel = true;
                        }
                    }
                }
            }
            KeyCode::Enter => {
                self.submit_repo_picker();
            }
            _ => {}
        }
    }

    fn handle_repo_picker_input_key(&mut self, code: KeyCode) {
        let super::modals::Modal::RepoPicker(ref mut rp) = self.modal else {
            return;
        };
        match code {
            KeyCode::Esc => {
                self.modal.close();
                return;
            }
            KeyCode::Tab => {
                if let Some(suggestion) = rp.path_suggestion.take() {
                    for c in suggestion.chars() {
                        rp.path_input.insert(c);
                    }
                } else {
                    rp.focus = super::modals::RepoPickerFocus::List;
                    rp.path_suggestion = None;
                    return;
                }
            }
            KeyCode::BackTab => {
                rp.focus = super::modals::RepoPickerFocus::List;
                rp.path_suggestion = None;
                return;
            }
            KeyCode::Enter => {
                let path = rp.path_input.value().trim().to_string();
                if !path.is_empty() {
                    let expanded = paths::expand_tilde(&path);
                    // Add to bookmarks list if not already present
                    if !rp.bookmarks.contains(&expanded) {
                        rp.bookmarks.push(expanded.clone());
                        rp.selected.push(true); // auto-select newly added
                        rp.worktree.push(false);
                    } else {
                        // Already in list — just select it
                        if let Some(idx) = rp.bookmarks.iter().position(|p| p == &expanded) {
                            rp.selected[idx] = true;
                        }
                    }
                    // Persist as bookmark
                    if let Err(e) = self.db.upsert_repo_bookmark(&expanded) {
                        error!("Failed to save repo bookmark: {e}");
                    }
                    let super::modals::Modal::RepoPicker(ref mut rp) = self.modal else {
                        return;
                    };
                    rp.path_input.clear();
                    rp.path_suggestion = None;
                }
                return;
            }
            KeyCode::Backspace => rp.path_input.backspace(),
            KeyCode::Delete => rp.path_input.delete(),
            KeyCode::Left => rp.path_input.move_left(),
            KeyCode::Right => rp.path_input.move_right(),
            KeyCode::Home => rp.path_input.home(),
            KeyCode::End => rp.path_input.end(),
            KeyCode::Char(c) => rp.path_input.insert(c),
            _ => return,
        }
        self.update_repo_picker_path_suggestion();
    }

    fn update_repo_picker_path_suggestion(&mut self) {
        let super::modals::Modal::RepoPicker(ref mut rp) = self.modal else {
            return;
        };
        let value = rp.path_input.value().to_string();
        let at_end = rp.path_input.cursor_pos() == value.chars().count();
        if at_end && !value.is_empty() {
            rp.path_suggestion = paths::complete_directory_path(&value);
        } else {
            rp.path_suggestion = None;
        }
    }

    fn submit_repo_picker(&mut self) {
        let super::modals::Modal::RepoPicker(ref rp) = self.modal else {
            return;
        };

        // Partition selected repos into worktree and normal sets.
        let mut worktree_repos: Vec<std::path::PathBuf> = Vec::new();
        let mut normal_repos: Vec<std::path::PathBuf> = Vec::new();
        for (i, path) in rp.bookmarks.iter().enumerate() {
            let selected = rp.selected.get(i).copied().unwrap_or(false);
            if !selected {
                continue;
            }
            let is_worktree = rp.worktree.get(i).copied().unwrap_or(false);
            if is_worktree {
                worktree_repos.push(path.clone());
            } else {
                normal_repos.push(path.clone());
            }
        }

        // Touch all selected bookmarks so they stay sorted by recency.
        for repo in worktree_repos.iter().chain(normal_repos.iter()) {
            if let Err(e) = self.db.upsert_repo_bookmark(repo) {
                error!("Failed to touch repo bookmark: {e}");
            }
        }

        self.modal.close();

        if worktree_repos.is_empty() && normal_repos.is_empty() {
            // No repos selected — spawn with HOME as cwd
            let mut config = SessionConfig::default();
            if let Some(home) = std::env::var_os("HOME") {
                config.cwd = Some(std::path::PathBuf::from(home));
            }
            self.spawn_session_with_config(&config);
        } else if !worktree_repos.is_empty() {
            // Has worktree repos — go to branch selection.
            // Store normal repos for inclusion after worktree creation.
            self.pending_repo_path = Some(worktree_repos[0].clone());
            self.pending_all_repos = if worktree_repos.len() > 1 {
                Some(worktree_repos)
            } else {
                None
            };
            self.pending_normal_repos = normal_repos;
            self.start_branch_selection();
        } else {
            // All normal repos — go to session mode selection.
            self.pending_repo_path = Some(normal_repos[0].clone());
            self.pending_all_repos = if normal_repos.len() > 1 {
                Some(normal_repos)
            } else {
                None
            };
            self.modal =
                super::modals::Modal::SessionMode(super::modals::SessionModeModal { index: 0 });
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
