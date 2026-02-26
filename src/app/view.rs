//! View / rendering logic for the Thurbox TUI.
//!
//! Contains the main `App::view` method and helper functions for
//! rendering the help overlay and formatting timestamps.

use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::Style,
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
    Frame,
};

use crate::session::{SessionInfo, SessionStatus};
use crate::ui::selection;
use crate::ui::theme::Theme;
use crate::ui::{
    add_project_modal, branch_selector_modal, containerfile_picker, delete_project_modal,
    edit_project_modal, info_panel, layout, project_list, repo_selector_modal,
    restore_sessions_modal, role_editor_modal, role_selector_modal, schedule_command_modal,
    session_mode_modal, status_bar, terminal_view, worktree_name_modal,
};

use super::{App, InputFocus, TerminalView};

impl App {
    pub fn view(&mut self, frame: &mut Frame) {
        let areas = layout::compute_layout(frame.area(), self.show_info_panel);

        status_bar::render_header(frame, areas.header);

        // Left panel (projects + sessions)
        if let Some(left_area) = areas.left_panel {
            let project_entries: Vec<project_list::ProjectEntry<'_>> = self
                .projects
                .iter()
                .map(|p| {
                    let mut busy_count = 0usize;
                    let mut waiting_count = 0usize;
                    let mut error_count = 0usize;
                    for sid in &p.session_ids {
                        if let Some(s) = self.sessions.iter().find(|s| s.info.id == *sid) {
                            match s.info.status {
                                SessionStatus::Busy | SessionStatus::Provisioning => {
                                    busy_count += 1;
                                }
                                SessionStatus::Waiting => waiting_count += 1,
                                SessionStatus::Error => error_count += 1,
                                SessionStatus::Idle => {}
                            }
                        }
                    }

                    let repo_count = p.config.repos.len();
                    let repo_short = if repo_count == 1 {
                        p.config.repos[0].file_name().and_then(|n| n.to_str())
                    } else {
                        None
                    };

                    project_list::ProjectEntry {
                        name: &p.config.name,
                        is_admin: p.is_admin,
                        repo_count,
                        repo_short,
                        role_count: p.config.roles.len(),
                        busy_count,
                        waiting_count,
                        error_count,
                    }
                })
                .collect();

            let project_session_indices = self.active_project_sessions();
            let mut project_sessions: Vec<&SessionInfo> = project_session_indices
                .iter()
                .map(|&i| &self.sessions[i].info)
                .collect();
            self.session_elapsed_buf.clear();
            for &i in &project_session_indices {
                self.session_elapsed_buf
                    .push(self.sessions[i].millis_since_last_output());
            }

            // Include VM placeholder in the session list if it belongs to this project.
            if let Some(ref ph) = self.vm_placeholder {
                if let Some(project) = self.projects.get(self.active_project_index) {
                    if project.session_ids.contains(&ph.id) {
                        project_sessions.push(ph);
                        self.session_elapsed_buf.push(0);
                    }
                }
            }

            // Include container placeholder in the session list if it belongs to this project.
            if let Some(ref ph) = self.container_placeholder {
                if let Some(project) = self.projects.get(self.active_project_index) {
                    if project.session_ids.contains(&ph.id) {
                        project_sessions.push(ph);
                        self.session_elapsed_buf.push(0);
                    }
                }
            }

            let panel_focus = match self.focus {
                InputFocus::ProjectList => project_list::LeftPanelFocus::Projects,
                InputFocus::SessionList | InputFocus::Terminal => {
                    project_list::LeftPanelFocus::Sessions
                }
            };

            // Compute tri-state focus levels for each sub-panel
            use crate::ui::FocusLevel;
            let (project_focus, session_focus) = match self.focus {
                InputFocus::ProjectList => (FocusLevel::Focused, FocusLevel::Inactive),
                InputFocus::SessionList => (FocusLevel::Active, FocusLevel::Focused),
                InputFocus::Terminal => (FocusLevel::Inactive, FocusLevel::Active),
            };

            project_list::render_left_panel(
                frame,
                left_area,
                &project_list::LeftPanelState {
                    projects: &project_entries,
                    active_project: self.active_project_index,
                    sessions: &project_sessions,
                    active_session: self.active_session_in_project(),
                    session_elapsed_ms: &self.session_elapsed_buf,
                    focus: panel_focus,
                    panel_focused: self.focus != InputFocus::Terminal,
                    project_focus,
                    session_focus,
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
                info_panel::render_info_panel(
                    frame,
                    info_area,
                    info,
                    vm_details.as_ref(),
                    Some(&self.system_metrics),
                );
            }
        }

        // Terminal
        let terminal_focus = match self.focus {
            InputFocus::Terminal => crate::ui::FocusLevel::Focused,
            InputFocus::SessionList => crate::ui::FocusLevel::Active,
            InputFocus::ProjectList => crate::ui::FocusLevel::Inactive,
        };
        let is_shell_view = self.active_terminal_view() == TerminalView::Shell;
        match self.sessions.get(self.active_index) {
            Some(session) => {
                let is_admin_project = self
                    .projects
                    .get(self.active_project_index)
                    .is_some_and(|p| p.is_admin);
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
            InputFocus::ProjectList => "Projects",
            InputFocus::SessionList => "Sessions",
            InputFocus::Terminal if is_shell_view => "Shell",
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
                vm_provisioning: self.vm_provisioning,
                vm_provisioning_step: &self.vm_provisioning_step,
                container_provisioning: self.container_provisioning,
                container_provisioning_step: &self.container_provisioning_step,
                tick_count: self.tick_count,
            },
        );

        // Help overlay (rendered last, on top of everything)
        if matches!(self.modal, super::modals::Modal::Help) {
            render_help_overlay(frame);
        }

        // Add-project modal (on top of everything including help)
        if let super::modals::Modal::AddProject(ref ap) = self.modal {
            add_project_modal::render_add_project_modal(
                frame,
                &add_project_modal::AddProjectModalState {
                    name: ap.name.value(),
                    name_cursor: ap.name.cursor_pos(),
                    path: ap.path.value(),
                    path_cursor: ap.path.cursor_pos(),
                    path_suggestion: ap.path_suggestion.as_deref(),
                    repos: &ap.repos,
                    repo_index: ap.repo_index,
                    focused_field: ap.field,
                },
            );
        }

        // Edit-project modal
        if let super::modals::Modal::EditProject(ref ep) = self.modal {
            edit_project_modal::render_edit_project_modal(
                frame,
                &edit_project_modal::EditProjectModalState {
                    name: ep.name.value(),
                    name_cursor: ep.name.cursor_pos(),
                    path: ep.path.value(),
                    path_cursor: ep.path.cursor_pos(),
                    path_suggestion: ep.path_suggestion.as_deref(),
                    repos: &ep.repos,
                    repo_index: ep.repo_index,
                    roles: &ep.role_editor_roles,
                    role_index: ep.role_editor_list_index,
                    mcp_servers: &ep.mcp_servers,
                    mcp_server_index: ep.mcp_server_index,
                    focused_field: ep.field,
                },
            );
        }

        // Delete-project modal
        if let super::modals::Modal::DeleteProject(ref dp) = self.modal {
            delete_project_modal::render_delete_project_modal(
                frame,
                &delete_project_modal::DeleteProjectModalState {
                    project_name: &dp.project_name,
                    confirmation: dp.confirmation.value(),
                    confirmation_cursor: dp.confirmation.cursor_pos(),
                    error: dp.error.as_deref(),
                },
            );
        }

        // Repo selector modal
        if let super::modals::Modal::RepoSelector(ref rs) = self.modal {
            if let Some(active_project) = self.active_project() {
                repo_selector_modal::render_repo_selector_modal(
                    frame,
                    &repo_selector_modal::RepoSelectorState {
                        repos: &active_project.config.repos,
                        selected_index: rs.index,
                    },
                );
            }
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

        // Role selector modal
        if let super::modals::Modal::RoleSelector(ref rsel) = self.modal {
            if let Some(project) = self.active_project() {
                role_selector_modal::render_role_selector_modal(
                    frame,
                    &role_selector_modal::RoleSelectorState {
                        roles: &project.config.roles,
                        selected_index: rsel.index,
                    },
                );
            }
        }

        // Role editor modal (detail form, overlays edit-project modal)
        if self.show_role_editor {
            let project_name = if let super::modals::Modal::EditProject(ref ep) = self.modal {
                ep.name.value()
            } else {
                ""
            };
            role_editor_modal::render_role_editor_modal(
                frame,
                &role_editor_modal::RoleEditorState {
                    project_name,
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

        // MCP editor modal (detail form, overlays edit-project modal)
        if self.show_mcp_editor {
            let project_name = if let super::modals::Modal::EditProject(ref ep) = self.modal {
                ep.name.value()
            } else {
                ""
            };
            crate::ui::mcp_editor_modal::render_mcp_editor_modal(
                frame,
                &crate::ui::mcp_editor_modal::McpEditorState {
                    project_name,
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
            let session_name = self
                .sessions
                .get(self.active_index)
                .map(|s| s.info.name.as_str())
                .unwrap_or("?");
            schedule_command_modal::render_schedule_command_modal(
                frame,
                &schedule_command_modal::ScheduleCommandState {
                    command: sc.command.value(),
                    command_cursor: sc.command.cursor_pos(),
                    delay_minutes: sc.delay_minutes.value(),
                    delay_cursor: sc.delay_minutes.cursor_pos(),
                    focused_field: sc.field,
                    session_name,
                },
            );
        }

        // Discard confirmation overlay
        if self.show_discard_confirmation {
            let confirm_area = crate::ui::centered_fixed_height_rect(40, 5, frame.area());
            frame.render_widget(Clear, confirm_area);
            let block = Block::default()
                .title(" Unsaved Changes ")
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Theme::STATUS_ERROR));
            let inner = block.inner(confirm_area);
            frame.render_widget(block, confirm_area);
            let text = Line::from(vec![
                Span::styled(
                    " Discard changes? ",
                    Style::default().fg(Theme::TEXT_PRIMARY),
                ),
                Span::styled("y", Theme::keybind()),
                Span::styled("/", Style::default().fg(Theme::TEXT_MUTED)),
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

        // Selection highlight and text cache — runs after all rendering.
        if let Some(ref sel) = self.text_selection {
            let sel_style = Style::default()
                .bg(Theme::SELECTION_BG)
                .fg(Theme::SELECTION_FG);
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

    frame.render_widget(Clear, area);

    let block = Block::default()
        .title(" Keybindings ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Theme::ACCENT));

    let help_lines = vec![
        help_section("Navigation"),
        help_line(
            "Ctrl+H",
            "Focus previous pane (← project ← session ← terminal)",
        ),
        help_line("Ctrl+L", "Focus next pane (→ project → session → terminal)"),
        help_line("Ctrl+J", "Select next project or session"),
        help_line("Ctrl+K", "Select previous project or session"),
        Line::from(""),
        help_section("Sessions"),
        help_line(
            "Ctrl+N",
            "Create new session (or project when list focused)",
        ),
        help_line("Ctrl+D", "Delete focused session or project"),
        help_line("Ctrl+R", "Restart active session"),
        help_line("Ctrl+P", "Schedule a command for active session"),
        help_line("Ctrl+Z", "Undo last delete"),
        help_line("Ctrl+U", "Restore deleted sessions"),
        Line::from(""),
        help_section("Project"),
        help_line("Ctrl+E", "Edit active project (name, repos, roles)"),
        help_line("Ctrl+S", "Sync all worktrees with main"),
        Line::from(""),
        help_section("UI"),
        help_line("Ctrl+T", "Toggle shell pane"),
        help_line("F1", "Toggle keybindings help"),
        help_line("F2", "Toggle info panel"),
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
            Style::default().fg(Theme::TEXT_MUTED),
        )),
    ];

    let paragraph = Paragraph::new(help_lines).block(block);
    frame.render_widget(paragraph, area);
}

fn help_section(title: &str) -> Line<'_> {
    Line::from(Span::styled(title, Theme::section_header()))
}

fn help_line<'a>(key: &'a str, desc: &'a str) -> Line<'a> {
    Line::from(vec![
        Span::styled(format!("  {key:<16}"), Theme::keybind()),
        Span::styled(desc, Style::default().fg(Theme::TEXT_PRIMARY)),
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
