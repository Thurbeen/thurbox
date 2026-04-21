//! View / rendering logic for the Thurbox TUI.
//!
//! Contains the main `App::view` method and helper functions for
//! rendering the help overlay and formatting timestamps.

use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::Style,
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use crate::session::SessionInfo;
use crate::ui::selection;
use crate::ui::theme::Theme;
use crate::ui::{
    branch_selector_modal, containerfile_picker, file_viewer, info_panel, layout,
    mcp_server_picker_modal, model_picker_modal, profile_picker_modal, project_list,
    restore_sessions_modal, role_editor_modal, role_selector_modal, schedule_command_modal,
    scheduled_commands_list_modal, session_mode_modal, session_name_modal, skill_picker_modal,
    status_bar, terminal_view, theme_picker_modal, worktree_name_modal,
};

use super::{App, InputFocus, TerminalView};

impl App {
    pub fn view(&mut self, frame: &mut Frame) {
        let areas =
            layout::compute_layout(frame.area(), self.show_info_panel, self.show_file_viewer);

        let active_name = self
            .sessions
            .get(self.active_index)
            .map(|s| s.info.name.as_str());
        let theme_label = self.active_theme.display_name();
        status_bar::render_header(
            frame,
            areas.header,
            Some(status_bar::HeaderBadge {
                active_session: active_name,
                theme_label,
            }),
        );

        // Left panel (flat session list)
        if let Some(left_area) = areas.left_panel {
            // Build flat session list: all sessions, with tag names from projects
            let mut all_sessions: Vec<&SessionInfo> =
                self.sessions.iter().map(|s| &s.info).collect();
            self.session_elapsed_buf.clear();
            for s in &self.sessions {
                self.session_elapsed_buf.push(s.millis_since_last_output());
            }

            // Include VM placeholder in the session list.
            if let Some(ref ph) = self.vm_placeholder {
                all_sessions.push(ph);
                self.session_elapsed_buf.push(0);
            }

            // Include container placeholder in the session list.
            if let Some(ref ph) = self.container_placeholder {
                all_sessions.push(ph);
                self.session_elapsed_buf.push(0);
            }

            let session_elapsed_buf = self.session_elapsed_buf.clone();

            // Pin admin sessions to the top of the list. All parallel arrays
            // (elapsed, match_positions) and active_index are remapped so they
            // stay aligned with the rendered order.
            let ordered = project_list::OrderedSessions::new(
                &all_sessions,
                &session_elapsed_buf,
                &self.session_match_positions,
                self.active_index,
            );

            use crate::ui::FocusLevel;
            let list_focus = match self.focus {
                InputFocus::SessionList => FocusLevel::Focused,
                InputFocus::Terminal | InputFocus::FileViewer => FocusLevel::Active,
            };

            let match_count = self
                .session_match_positions
                .iter()
                .filter(|m| m.is_some())
                .count();
            let total_count = ordered.sessions.len();

            project_list::render_left_panel(
                frame,
                left_area,
                &mut project_list::LeftPanelState {
                    sessions: &ordered.sessions,
                    active_session: ordered.active_index,
                    session_elapsed_ms: &ordered.elapsed_ms,
                    session_focus: list_focus,
                    session_list_state: &mut self.session_list_state,
                    search_query: &self.search_input.buffer,
                    search_active: self.search_active,
                    search_cursor: self.search_input.cursor,
                    session_match_positions: &ordered.match_positions,
                    session_search_active: !self.session_match_positions.is_empty(),
                    match_count,
                    total_count,
                    first_non_admin_index: ordered.first_non_admin_index,
                },
            );
        }

        // Info panel
        if let Some(info_area) = areas.info_panel {
            // Determine the session info to display: real session or VM placeholder.
            let info_session: Option<&SessionInfo> = self
                .sessions
                .get(self.active_index)
                .map(|s| &s.info)
                .or(self.vm_placeholder.as_ref())
                .or(self.container_placeholder.as_ref());

            if let Some(info) = info_session {
                let vm_details = info.vm_id.as_deref().and_then(|vm_id| {
                    self.db
                        .get_vm(vm_id)
                        .ok()
                        .flatten()
                        .map(|rec| info_panel::VmDetails {
                            state: rec.state.to_string(),
                            cpus: rec.cpus,
                            memory_mb: rec.memory_mb,
                            ssh_port: rec.ssh_port,
                            base_image: rec.base_image,
                        })
                });
                let now = crate::sync::current_time_millis();
                let scheduled_entries: Vec<info_panel::ScheduledCommandEntry> = self
                    .cached_pending_commands
                    .iter()
                    .filter(|cmd| cmd.session_id == info.id)
                    .map(|cmd| {
                        let remaining = cmd.scheduled_at.saturating_sub(now);
                        info_panel::ScheduledCommandEntry {
                            command_preview: truncate_str(&cmd.command_text, 30),
                            countdown: format_countdown(remaining),
                        }
                    })
                    .collect();
                info_panel::render_info_panel(
                    frame,
                    info_area,
                    info,
                    vm_details.as_ref(),
                    Some(&self.system_metrics),
                    &scheduled_entries,
                );
            }
        }

        // File viewer (right column)
        if let Some(fv_area) = areas.file_viewer {
            if let Some(session) = self.sessions.get(self.active_index) {
                if self.file_viewer.needs_rebuild_for(&session.info) {
                    self.file_viewer.rebuild_from_session(&session.info);
                }
            } else {
                self.file_viewer.clear();
            }
            let fv_focus = match self.focus {
                InputFocus::FileViewer => crate::ui::FocusLevel::Focused,
                _ => crate::ui::FocusLevel::Inactive,
            };
            file_viewer::render_file_viewer(frame, fv_area, &self.file_viewer, fv_focus);
        }

        // Terminal
        let terminal_focus = match self.focus {
            InputFocus::Terminal => crate::ui::FocusLevel::Focused,
            InputFocus::SessionList | InputFocus::FileViewer => crate::ui::FocusLevel::Active,
        };
        let is_shell_view = self.active_terminal_view() == TerminalView::Shell;
        match self.sessions.get(self.active_index) {
            Some(session) => {
                let is_admin_project = session.info.is_admin;
                let parser_arc = if is_shell_view {
                    session.shell_pane.as_ref().map(|sp| &sp.parser)
                } else {
                    None
                }
                .unwrap_or(&session.parser);
                if let Ok(mut parser) = parser_arc.lock() {
                    terminal_view::render_terminal(
                        frame,
                        areas.terminal,
                        &mut parser,
                        &session.info,
                        terminal_focus,
                        is_admin_project,
                        is_shell_view,
                    );
                }
            }
            None => terminal_view::render_empty_terminal(frame, areas.terminal),
        }

        let focus_label = match self.focus {
            InputFocus::SessionList => "Sessions",
            InputFocus::Terminal if is_shell_view => "Shell",
            InputFocus::Terminal => "Terminal",
            InputFocus::FileViewer => "Files",
        };
        status_bar::render_footer(
            frame,
            areas.footer,
            &status_bar::FooterState {
                session_count: self.sessions.len(),
                status: self.status_message.as_ref(),
                focus_label,
                sync_in_progress: self.worktree_sync_in_progress,
                vm_provisioning: self.vm_provisioning,
                vm_provisioning_step: &self.vm_provisioning_step,
                container_provisioning: self.container_provisioning,
                container_provisioning_step: &self.container_provisioning_step,
                tick_count: self.tick_count,
                pending_scheduled_count: self.cached_pending_commands.len(),
                file_viewer_open: self.show_file_viewer,
            },
        );

        // Help overlay (rendered last, on top of everything)
        if matches!(self.modal, super::modals::Modal::Help) {
            render_help_overlay(frame);
        }

        // Session mode modal
        if let super::modals::Modal::SessionMode(ref sm) = self.modal {
            session_mode_modal::render_session_mode_modal(
                frame,
                &session_mode_modal::SessionModeState {
                    selected_index: sm.index,
                    devcontainer_available: self.backends.has("devcontainer"),
                    vm_available: self.backends.has("qemu-vm"),
                },
            );
        }

        // Containerfile picker modal
        if let super::modals::Modal::ContainerfilePicker(ref cp) = self.modal {
            containerfile_picker::render_containerfile_picker(
                frame,
                &containerfile_picker::ContainerfilePickerState {
                    containerfiles: &cp.list,
                    selected_index: cp.index,
                },
            );
        }

        // Worktree name modal
        if let super::modals::Modal::WorktreeName(ref wn) = self.modal {
            let base = self.pending_base_branch.as_deref().unwrap_or("");
            worktree_name_modal::render_worktree_name_modal(
                frame,
                &worktree_name_modal::WorktreeNameState {
                    name: wn.name.value(),
                    cursor: wn.name.cursor_pos(),
                    base_branch: base,
                },
            );
        }

        // Session name modal
        if let super::modals::Modal::SessionName(ref sn) = self.modal {
            session_name_modal::render_session_name_modal(
                frame,
                &session_name_modal::SessionNameState {
                    name: sn.name.value(),
                    cursor: sn.name.cursor_pos(),
                },
            );
        }

        // Branch selector modal
        if let super::modals::Modal::BranchSelector(ref bs) = self.modal {
            branch_selector_modal::render_branch_selector_modal(
                frame,
                &branch_selector_modal::BranchSelectorState {
                    branches: &bs.branches,
                    selected_index: bs.index,
                },
            );
        }

        // Profile picker modal
        if let super::modals::Modal::ProfilePicker(ref pp) = self.modal {
            profile_picker_modal::render_profile_picker_modal(
                frame,
                &profile_picker_modal::ProfilePickerState {
                    profiles: &pp.profiles,
                    selected_index: pp.index,
                },
            );
        }

        // Role selector modal
        if let super::modals::Modal::RoleSelector(ref rsel) = self.modal {
            role_selector_modal::render_role_selector_modal(
                frame,
                &role_selector_modal::RoleSelectorState {
                    roles: &rsel.roles,
                    selected_index: rsel.index,
                },
            );
        }

        // Theme picker modal
        if let super::modals::Modal::ThemePicker(ref tp) = self.modal {
            theme_picker_modal::render_theme_picker_modal(
                frame,
                &theme_picker_modal::ThemePickerState {
                    presets: crate::session::ThemePreset::all(),
                    selected_index: tp.index,
                },
            );
        }

        // Model selector modal
        if let super::modals::Modal::ModelSelector(ref msel) = self.modal {
            model_picker_modal::render_model_picker_modal(
                frame,
                &model_picker_modal::ModelPickerState {
                    selected_index: msel.index,
                },
            );
        }

        // MCP server picker modal
        if let super::modals::Modal::McpServerPicker(ref msp) = self.modal {
            mcp_server_picker_modal::render_mcp_server_picker_modal(
                frame,
                &mcp_server_picker_modal::McpServerPickerState {
                    servers: &self.global_mcp_servers,
                    selected: &msp.selected,
                    index: msp.index,
                },
            );
        }

        // Skill picker modal
        if let super::modals::Modal::SkillPicker(ref sp) = self.modal {
            skill_picker_modal::render_skill_picker_modal(
                frame,
                &skill_picker_modal::SkillPickerState {
                    skills: &sp.skills,
                    selected: &sp.selected,
                    index: sp.index,
                },
            );
        }

        // Settings overlay (tabbed list of roles / MCP servers / skills / profiles)
        if self.show_settings
            && !self.show_role_editor
            && !self.show_mcp_editor
            && !self.show_skill_editor
            && !self.show_profile_editor
        {
            crate::ui::settings_overlay::render_settings_overlay(
                frame,
                &crate::ui::settings_overlay::SettingsOverlayState {
                    tab: self.settings_tab,
                    roles: &self.global_roles,
                    role_index: self.role_editor_list_index,
                    mcp_servers: &self.global_mcp_servers,
                    mcp_index: self.mcp_server_list_index,
                    skills: &self.global_skills,
                    skill_index: self.skill_list_index,
                    profiles: &self.global_profiles,
                    profile_index: self.profile_list_index,
                    plugins: &self.effective_plugins,
                    plugin_index: self.plugin_list_index,
                },
            );
        }

        // Plugin install modal (overlays the settings overlay's Plugins tab)
        if self.show_plugin_install_modal {
            use crate::ui::plugin_install_modal::{
                render_plugin_install_modal, PluginInstallModalState, PluginInstallStatusView,
            };
            let status = match &self.plugin_install_status {
                super::PluginInstallStatus::Idle => PluginInstallStatusView::Idle,
                super::PluginInstallStatus::InProgress => {
                    PluginInstallStatusView::InProgress("Installing… (cloning + copying)")
                }
                super::PluginInstallStatus::Success(msg) => {
                    PluginInstallStatusView::Success(msg.as_str())
                }
                super::PluginInstallStatus::Error(msg) => {
                    PluginInstallStatusView::Error(msg.as_str())
                }
            };
            render_plugin_install_modal(
                frame,
                &PluginInstallModalState {
                    input: self.plugin_install_input.value(),
                    cursor: self.plugin_install_input.cursor_pos(),
                    status,
                },
            );
        }

        // Plugin uninstall confirmation (overlays the Plugins tab)
        if let Some(ref name) = self.plugin_uninstall_confirm {
            let area = crate::ui::centered_fixed_height_rect(50, 5, frame.area());
            let inner = crate::ui::render_modal_frame_danger(frame, area, "Uninstall Plugin");
            let path_display = self
                .effective_plugins
                .iter()
                .find(|(p, _)| &p.name == name)
                .map(|(p, _)| p.path.display().to_string())
                .unwrap_or_default();
            let text = Line::from(vec![
                Span::styled(
                    format!(" Uninstall '{name}'? Deletes {path_display}  "),
                    Style::default().fg(Theme::text_primary()),
                ),
                Span::styled("y", Theme::keybind()),
                Span::styled("/", Style::default().fg(Theme::text_muted())),
                Span::styled("n", Theme::keybind()),
            ]);
            frame.render_widget(
                Paragraph::new(text),
                Rect {
                    y: inner.y + inner.height / 2,
                    ..inner
                },
            );
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
                    env: &self.role_editor_env.items,
                    env_index: self.role_editor_env.selected,
                    env_mode: self.role_editor_env.mode,
                    env_input: self.role_editor_env.input.value(),
                    env_input_cursor: self.role_editor_env.input.cursor_pos(),
                    focused_field: self.role_editor_field,
                },
            );
        }

        // Skill editor modal (detail form for global skills)
        if self.show_skill_editor {
            use crate::ui::{
                centered_fixed_height_rect, render_modal_frame, render_text_field,
                render_text_field_with_suggestion,
            };
            let area = centered_fixed_height_rect(50, 8, frame.area());
            let inner = render_modal_frame(frame, area, "Skill Editor");
            if inner.height >= 4 && inner.width >= 10 {
                let chunks = ratatui::layout::Layout::default()
                    .direction(ratatui::layout::Direction::Vertical)
                    .constraints([
                        ratatui::layout::Constraint::Length(3),
                        ratatui::layout::Constraint::Length(3),
                    ])
                    .split(inner);

                render_text_field(
                    frame,
                    chunks[0],
                    "Name",
                    self.skill_editor_name.value(),
                    self.skill_editor_name.cursor_pos(),
                    self.skill_editor_field == super::SkillEditorField::Name,
                );
                render_text_field_with_suggestion(
                    frame,
                    chunks[1],
                    "Path",
                    self.skill_editor_path.value(),
                    self.skill_editor_path.cursor_pos(),
                    self.skill_editor_field == super::SkillEditorField::Path,
                    self.skill_editor_path_suggestion.as_deref(),
                );
            }
        }

        // Profile editor modal (detail form for global profiles)
        if self.show_profile_editor {
            crate::ui::profile_editor_modal::render_profile_editor_modal(
                frame,
                &crate::ui::profile_editor_modal::ProfileEditorState {
                    name: self.profile_editor_name.value(),
                    name_cursor: self.profile_editor_name.cursor_pos(),
                    description: self.profile_editor_description.value(),
                    description_cursor: self.profile_editor_description.cursor_pos(),
                    roles: &self.profile_editor_roles.items,
                    roles_index: self.profile_editor_roles.selected,
                    roles_mode: self.profile_editor_roles.mode,
                    roles_input: self.profile_editor_roles.input.value(),
                    roles_input_cursor: self.profile_editor_roles.input.cursor_pos(),
                    mcp_servers: &self.profile_editor_mcp_servers.items,
                    mcp_servers_index: self.profile_editor_mcp_servers.selected,
                    mcp_servers_mode: self.profile_editor_mcp_servers.mode,
                    mcp_servers_input: self.profile_editor_mcp_servers.input.value(),
                    mcp_servers_input_cursor: self.profile_editor_mcp_servers.input.cursor_pos(),
                    skills: &self.profile_editor_skills.items,
                    skills_index: self.profile_editor_skills.selected,
                    skills_mode: self.profile_editor_skills.mode,
                    skills_input: self.profile_editor_skills.input.value(),
                    skills_input_cursor: self.profile_editor_skills.input.cursor_pos(),
                    focused_field: self.profile_editor_field,
                },
            );
        }

        // MCP editor modal (detail form for global MCP servers)
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

        // Restore sessions modal
        if let super::modals::Modal::RestoreSessions(ref rsm) = self.modal {
            let entries: Vec<restore_sessions_modal::DeletedSessionEntry> = rsm
                .list
                .iter()
                .map(|d| restore_sessions_modal::DeletedSessionEntry {
                    name: d.name.clone(),
                    role: d.role.clone(),
                    deleted_ago: format_time_ago(d.deleted_at),
                    has_worktrees: !d.worktrees.is_empty(),
                })
                .collect();
            restore_sessions_modal::render_restore_sessions_modal(
                frame,
                &restore_sessions_modal::RestoreSessionsModalState {
                    entries: &entries,
                    selected_index: rsm.index,
                },
            );
        }

        // Schedule command modal
        if let super::modals::Modal::ScheduleCommand(ref sc) = self.modal {
            let session_name = sc
                .editing
                .as_ref()
                .map(|ed| ed.session_name.as_str())
                .unwrap_or_else(|| {
                    self.sessions
                        .get(self.active_index)
                        .map(|s| s.info.name.as_str())
                        .unwrap_or("?")
                });
            schedule_command_modal::render_schedule_command_modal(
                frame,
                &schedule_command_modal::ScheduleCommandState {
                    command: sc.command.value(),
                    command_cursor: sc.command.cursor_pos(),
                    delay_minutes: sc.delay_minutes.value(),
                    delay_cursor: sc.delay_minutes.cursor_pos(),
                    focused_field: sc.field,
                    session_name,
                    editing: sc.editing.is_some(),
                },
            );
        }

        // Scheduled commands list modal
        if let super::modals::Modal::ScheduledCommandsList(ref scl) = self.modal {
            let entries: Vec<scheduled_commands_list_modal::ScheduledCommandsListEntry> = scl
                .commands
                .iter()
                .map(
                    |e| scheduled_commands_list_modal::ScheduledCommandsListEntry {
                        session_name: e.session_name.clone(),
                        command_preview: e.command_text.clone(),
                        countdown: e.countdown.clone(),
                    },
                )
                .collect();
            scheduled_commands_list_modal::render_scheduled_commands_list_modal(
                frame,
                &scheduled_commands_list_modal::ScheduledCommandsListState {
                    entries: &entries,
                    selected_index: scl.index,
                },
            );
        }

        // Repo picker modal
        if let super::modals::Modal::RepoPicker(ref rp) = self.modal {
            crate::ui::repo_picker_modal::render_repo_picker_modal(
                frame,
                &crate::ui::repo_picker_modal::RepoPickerState {
                    bookmarks: &rp.bookmarks,
                    selected: &rp.selected,
                    worktree: &rp.worktree,
                    list_index: rp.list_index,
                    path_input: rp.path_input.value(),
                    path_cursor: rp.path_input.cursor_pos(),
                    path_suggestion: rp.path_suggestion.as_deref(),
                    focus: rp.focus,
                    search_query: rp.search_input.value(),
                    search_cursor: rp.search_input.cursor_pos(),
                    search_active: rp.focus == super::modals::RepoPickerFocus::Search
                        || !rp.search_input.value().is_empty(),
                    filtered_indices: &rp.filtered_indices,
                },
            );
        }

        // Discard confirmation overlay
        if self.show_discard_confirmation {
            let confirm_area = crate::ui::centered_fixed_height_rect(40, 5, frame.area());
            let inner =
                crate::ui::render_modal_frame_danger(frame, confirm_area, "Unsaved Changes");
            let text = Line::from(vec![
                Span::styled(
                    " Discard changes? ",
                    Style::default().fg(Theme::text_primary()),
                ),
                Span::styled("y", Theme::keybind()),
                Span::styled("/", Style::default().fg(Theme::text_muted())),
                Span::styled("n", Theme::keybind()),
            ]);
            frame.render_widget(
                Paragraph::new(text),
                Rect {
                    y: inner.y + inner.height / 2,
                    ..inner
                },
            );
        }

        // Repaint cells that fell back to terminal-default colours with the
        // active theme's background and primary text. Themes whose `app_bg`
        // is `Color::Reset` (e.g. the ANSI-based Default preset) skip this
        // step so they continue to honour the user's terminal palette.
        let app_bg = Theme::app_bg();
        if app_bg != ratatui::style::Color::Reset {
            let text_primary = Theme::text_primary();
            let area = frame.area();
            let buf = frame.buffer_mut();
            for y in area.y..area.y + area.height {
                for x in area.x..area.x + area.width {
                    let pos = ratatui::layout::Position::new(x, y);
                    if let Some(cell) = buf.cell_mut(pos) {
                        if cell.bg == ratatui::style::Color::Reset {
                            cell.bg = app_bg;
                        }
                        if cell.fg == ratatui::style::Color::Reset {
                            cell.fg = text_primary;
                        }
                    }
                }
            }
        }

        // Selection highlight and text cache — runs after all rendering.
        if let Some(ref sel) = self.text_selection {
            let sel_style = Style::default()
                .bg(Theme::selection_bg())
                .fg(Theme::selection_fg());
            let sel_clone = sel.clone();

            selection::highlight_buffer(frame.buffer_mut(), &sel_clone, sel_style);

            let text = selection::extract_text_from_buffer(frame.buffer_mut(), &sel_clone);
            self.selected_text_cache = if text.is_empty() { None } else { Some(text) };
        } else {
            self.selected_text_cache = None;
        }
    }
}

fn render_help_overlay(frame: &mut Frame) {
    let area = centered_rect(60, 70, frame.area());

    let inner = crate::ui::render_modal_frame(frame, area, "Keybindings");

    let help_lines = vec![
        help_section("Navigation"),
        help_line("Ctrl+H/L", "Toggle focus between session list and terminal"),
        help_line("Ctrl+J", "Select next session"),
        help_line("Ctrl+K", "Select previous session"),
        Line::from(""),
        help_section("Sessions"),
        help_line("Ctrl+N", "Create new session"),
        help_line("Ctrl+A", "Create admin session"),
        help_line("Ctrl+D", "Delete focused session"),
        help_line("Ctrl+R", "Restart active session"),
        help_line("Ctrl+F", "Fork active session"),
        help_line("Ctrl+P", "Scheduled commands (list/cancel/new)"),
        help_line("Ctrl+Z", "Undo last delete"),
        help_line("Ctrl+U", "Restore deleted sessions"),
        Line::from(""),
        help_section("Project"),
        help_line("Ctrl+E", "Settings (roles & MCP servers)"),
        help_line("Ctrl+O", "Open active worktree in editor"),
        help_line("Ctrl+S", "Sync all worktrees with main"),
        Line::from(""),
        help_section("UI"),
        help_line("Ctrl+T", "Toggle shell pane"),
        help_line("F1", "Toggle keybindings help"),
        help_line("F2", "Toggle info panel"),
        help_line("F3", "Toggle file viewer"),
        help_line("Ctrl+L/H", "Cycle focus (includes file viewer when open)"),
        help_line("j/k", "File viewer: move down/up (when focused)"),
        help_line("h", "File viewer: collapse / parent"),
        help_line("l / Enter", "File viewer: expand dir / open file in editor"),
        help_line("/", "File viewer: start search"),
        help_line(
            "Enter / \u{2193}",
            "In search: jump to next match (stays in search)",
        ),
        help_line("\u{2191}", "In search: previous match"),
        help_line("Tab", "In search: commit & exit search mode"),
        help_line("Esc", "In search: cancel and clear query"),
        help_line("n / N", "After search: next / previous match"),
        help_line("Ctrl+O", "On file viewer: open project with file focused"),
        help_line("Ctrl+Q", "Quit"),
        Line::from(""),
        help_section("List Navigation (when focused)"),
        help_line("j / Down", "Select next item"),
        help_line("k / Up", "Select previous item"),
        help_line("Enter", "Focus next pane"),
        Line::from(""),
        help_section("Terminal (when focused)"),
        help_line("Ctrl+C", "Copy selection, or send SIGINT if none"),
        help_line("Ctrl+V", "Paste from clipboard"),
        help_line("Shift+\u{2191}/\u{2193}", "Scroll up/down one line"),
        help_line("Shift+PgUp/PgDn", "Scroll up/down half page"),
        help_line("Mouse wheel", "Scroll up/down three lines"),
        help_line("Click+drag", "Select text"),
        help_line("*", "All other keys forwarded to session"),
        Line::from(""),
        Line::from(Span::styled(
            "Press F1 or Esc to close",
            Style::default().fg(Theme::text_muted()),
        )),
    ];

    frame.render_widget(Paragraph::new(help_lines), inner);
}

fn help_section(title: &str) -> Line<'_> {
    Line::from(Span::styled(title, Theme::section_header()))
}

fn help_line<'a>(key: &'a str, desc: &'a str) -> Line<'a> {
    Line::from(vec![
        Span::styled(format!("  {key:<16}"), Theme::keybind()),
        Span::styled(desc, Style::default().fg(Theme::text_primary())),
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

/// Format a millisecond timestamp as a human-readable "time ago" string.
pub(super) fn format_time_ago(millis: u64) -> String {
    let now = crate::sync::current_time_millis();
    let elapsed_secs = now.saturating_sub(millis) / 1000;
    if elapsed_secs < 60 {
        format!("{elapsed_secs}s ago")
    } else if elapsed_secs < 3600 {
        format!("{}m ago", elapsed_secs / 60)
    } else if elapsed_secs < 86400 {
        format!("{}h ago", elapsed_secs / 3600)
    } else {
        format!("{}d ago", elapsed_secs / 86400)
    }
}

/// Format a remaining-milliseconds value as a human-readable countdown.
pub(super) fn format_countdown(remaining_ms: u64) -> String {
    let secs = remaining_ms / 1000;
    if secs == 0 {
        "due".to_string()
    } else if secs < 60 {
        format!("in {secs}s")
    } else if secs < 3600 {
        let m = secs / 60;
        let s = secs % 60;
        if s == 0 {
            format!("in {m}m")
        } else {
            format!("in {m}m {s}s")
        }
    } else {
        let h = secs / 3600;
        let m = (secs % 3600) / 60;
        if m == 0 {
            format!("in {h}h")
        } else {
            format!("in {h}h {m}m")
        }
    }
}

/// Truncate a string to `max_len` characters, appending "..." if truncated.
fn truncate_str(s: &str, max_len: usize) -> String {
    if s.chars().count() <= max_len {
        s.to_string()
    } else {
        let truncated: String = s.chars().take(max_len.saturating_sub(3)).collect();
        format!("{truncated}...")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── format_countdown tests ──

    #[test]
    fn format_countdown_zero() {
        assert_eq!(format_countdown(0), "due");
    }

    #[test]
    fn format_countdown_sub_minute() {
        assert_eq!(format_countdown(999), "due");
        assert_eq!(format_countdown(1_000), "in 1s");
        assert_eq!(format_countdown(45_000), "in 45s");
        assert_eq!(format_countdown(59_999), "in 59s");
    }

    #[test]
    fn format_countdown_minutes() {
        assert_eq!(format_countdown(60_000), "in 1m");
        assert_eq!(format_countdown(90_000), "in 1m 30s");
        assert_eq!(format_countdown(300_000), "in 5m");
        assert_eq!(format_countdown(3_599_000), "in 59m 59s");
    }

    #[test]
    fn format_countdown_hours() {
        assert_eq!(format_countdown(3_600_000), "in 1h");
        assert_eq!(format_countdown(5_400_000), "in 1h 30m");
        assert_eq!(format_countdown(7_200_000), "in 2h");
    }

    // ── truncate_str tests ──

    #[test]
    fn truncate_str_short() {
        assert_eq!(truncate_str("hello", 10), "hello");
    }

    #[test]
    fn truncate_str_exact_fit() {
        assert_eq!(truncate_str("hello", 5), "hello");
    }

    #[test]
    fn truncate_str_needs_truncation() {
        assert_eq!(truncate_str("hello world", 8), "hello...");
    }

    #[test]
    fn truncate_str_empty() {
        assert_eq!(truncate_str("", 5), "");
    }
}
