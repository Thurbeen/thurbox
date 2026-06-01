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

use crate::session::{Action, Category, KeyBindings, KeyChord, SessionInfo};
use crate::ui::selection;
use crate::ui::theme::Theme;
use crate::ui::{
    agent_picker_modal, automation_editor_modal, automations_list_modal, automations_panel,
    branch_selector_modal, file_viewer, info_panel, layout, project_list, restore_sessions_modal,
    session_name_modal, status_bar, terminal_view, theme_picker_modal, worktree_name_modal,
};

use super::{App, InputFocus, TerminalView};

impl App {
    pub fn view(&mut self, frame: &mut Frame) {
        let areas = layout::compute_layout(
            frame.area(),
            self.show_info_panel,
            self.show_file_viewer,
            self.cached_automations.len(),
        );

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
            let all_sessions: Vec<&SessionInfo> = self.sessions.iter().map(|s| &s.info).collect();
            self.session_elapsed_buf.clear();
            for s in &self.sessions {
                self.session_elapsed_buf.push(s.millis_since_last_output());
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
                InputFocus::Automations | InputFocus::Terminal | InputFocus::FileViewer => {
                    FocusLevel::Active
                }
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

        // Automations pane (beneath the session list in the left column)
        if let Some(auto_area) = areas.automations_panel {
            let now = crate::sync::current_time_millis();
            let entries: Vec<automations_panel::AutomationPaneEntry> = self
                .cached_automations
                .iter()
                .map(|a| automations_panel::AutomationPaneEntry {
                    name: a.name.clone(),
                    summary: super::format_automation_summary(a, now),
                    enabled: a.enabled,
                })
                .collect();
            let focus = if self.focus == InputFocus::Automations {
                crate::ui::FocusLevel::Focused
            } else {
                crate::ui::FocusLevel::Inactive
            };
            let selected = self
                .automation_panel_index
                .min(entries.len().saturating_sub(1));
            automations_panel::render_automations_pane(
                frame,
                auto_area,
                &automations_panel::AutomationsPaneState {
                    entries: &entries,
                    selected,
                    focus,
                },
            );
        }

        // Info panel
        if let Some(info_area) = areas.info_panel {
            if let Some(info) = self.sessions.get(self.active_index).map(|s| &s.info) {
                let now = crate::sync::current_time_millis();
                let agent_usage = self.usage.get(&info.agent);
                let automation_entries: Vec<info_panel::AutomationEntry> = self
                    .cached_automations
                    .iter()
                    .filter(|a| a.enabled && a.next_run_at.is_some())
                    .map(|a| {
                        let remaining = a.next_run_at.unwrap_or(now).saturating_sub(now);
                        info_panel::AutomationEntry {
                            label: truncate_str(&a.name, 30),
                            countdown: format_countdown(remaining),
                        }
                    })
                    .collect();
                info_panel::render_info_panel(
                    frame,
                    info_area,
                    info,
                    Some(&self.system_metrics),
                    &automation_entries,
                    agent_usage,
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
            InputFocus::SessionList | InputFocus::Automations | InputFocus::FileViewer => {
                crate::ui::FocusLevel::Active
            }
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
            InputFocus::Automations => "Automations",
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
                tick_count: self.tick_count,
                automation_count: self.cached_automations.iter().filter(|a| a.enabled).count(),
                file_viewer_open: self.show_file_viewer,
            },
        );

        // Help overlay (rendered last, on top of everything)
        if matches!(self.modal, super::modals::Modal::Help) {
            render_help_overlay(frame, &self.keybindings);
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

        // Agent picker modal
        if let super::modals::Modal::AgentPicker(ref ap) = self.modal {
            agent_picker_modal::render_agent_picker_modal(frame, ap);
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

        // Restore sessions modal
        if let super::modals::Modal::RestoreSessions(ref rsm) = self.modal {
            let entries: Vec<restore_sessions_modal::DeletedSessionEntry> = rsm
                .list
                .iter()
                .map(|d| restore_sessions_modal::DeletedSessionEntry {
                    name: d.name.clone(),
                    agent: d.agent.clone(),
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

        // Automation editor modal
        if let super::modals::Modal::AutomationEditor(ref m) = self.modal {
            // Live preview of when this schedule will next fire (or the
            // validation error for the current input).
            let now = crate::sync::current_time_millis();
            let preview = match m.build_schedule(now) {
                Ok(sched) => match sched.next_after(now, m.timezone().as_deref()) {
                    Some(next) => format!("in {}", format_countdown(next.saturating_sub(now))),
                    None => "never (check schedule)".to_string(),
                },
                Err(e) => e,
            };
            automation_editor_modal::render_automation_editor_modal(
                frame,
                &automation_editor_modal::AutomationEditorState {
                    editing: m.editing_id.is_some(),
                    field: m.field,
                    trigger_kind: m.trigger_kind,
                    action: m.action,
                    enabled: m.enabled,
                    name: m.name.value(),
                    delay: m.delay.value(),
                    weekday: m.weekday,
                    hour: m.hour,
                    minute: m.minute,
                    cron_expr: m.cron_expr.value(),
                    timezone: m.timezone.value(),
                    repo: m.repo.value(),
                    worktree: m.worktree.value(),
                    agent: m.agent.value(),
                    prompt: m.prompt.value(),
                    target_session: m.target_session.as_ref().map(|(_, name)| name.as_str()),
                    preview: &preview,
                },
            );
        }

        // Automations list modal
        if let super::modals::Modal::AutomationsList(ref al) = self.modal {
            let entries: Vec<automations_list_modal::AutomationsListEntry> = al
                .entries
                .iter()
                .map(|e| automations_list_modal::AutomationsListEntry {
                    name: e.name.clone(),
                    summary: e.summary.clone(),
                    enabled: e.enabled,
                })
                .collect();
            automations_list_modal::render_automations_list_modal(
                frame,
                &automations_list_modal::AutomationsListState {
                    entries: &entries,
                    selected_index: al.index,
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

fn render_help_overlay(frame: &mut Frame, keybindings: &KeyBindings) {
    let area = centered_rect(60, 70, frame.area());

    let inner = crate::ui::render_modal_frame(frame, area, "Keybindings");

    let mut help_lines: Vec<Line<'static>> = Vec::new();

    // Action-driven sections — every variant of `Action` shows up here
    // automatically. Adding a new variant is a compile error in
    // `Action::category()` and `Action::default_chords()`.
    for category in Category::all() {
        help_lines.push(help_section(category.title()));
        for action in Action::all().iter().filter(|a| a.category() == *category) {
            let key = chords_display(keybindings.chords_for(*action));
            help_lines.push(help_line(key, action.label()));
        }
        help_lines.push(Line::from(""));
    }

    // Non-Action keys (modal-internal, terminal forwarding, file viewer).
    // These are not user-rebindable and don't drift with the `Action` enum.
    help_lines.push(help_section("List Navigation (when focused)"));
    help_lines.push(help_line("j / Down".into(), "Select next item"));
    help_lines.push(help_line("k / Up".into(), "Select previous item"));
    help_lines.push(help_line("Enter".into(), "Focus next pane"));
    help_lines.push(Line::from(""));

    help_lines.push(help_section("File Viewer (when focused)"));
    help_lines.push(help_line("j/k".into(), "Move down/up"));
    help_lines.push(help_line("h".into(), "Collapse / parent"));
    help_lines.push(help_line(
        "l / Enter".into(),
        "Expand dir / open file in editor",
    ));
    help_lines.push(help_line("/".into(), "Start search"));
    help_lines.push(help_line(
        "Enter / \u{2193}".into(),
        "In search: jump to next match (stays in search)",
    ));
    help_lines.push(help_line("\u{2191}".into(), "In search: previous match"));
    help_lines.push(help_line(
        "Tab".into(),
        "In search: commit & exit search mode",
    ));
    help_lines.push(help_line("Esc".into(), "In search: cancel and clear query"));
    help_lines.push(help_line(
        "n / N".into(),
        "After search: next / previous match",
    ));
    help_lines.push(Line::from(""));

    help_lines.push(help_section("Terminal (when focused)"));
    help_lines.push(help_line(
        "Ctrl+C".into(),
        "Copy selection, or send SIGINT if none",
    ));
    help_lines.push(help_line("Ctrl+V".into(), "Paste from clipboard"));
    help_lines.push(help_line(
        "Shift+\u{2191}/\u{2193}".into(),
        "Scroll up/down one line",
    ));
    help_lines.push(help_line(
        "Shift+PgUp/PgDn".into(),
        "Scroll up/down half page",
    ));
    help_lines.push(help_line(
        "Mouse wheel".into(),
        "Scroll up/down three lines",
    ));
    help_lines.push(help_line("Click+drag".into(), "Select text"));
    help_lines.push(help_line("*".into(), "All other keys forwarded to session"));
    help_lines.push(Line::from(""));

    help_lines.push(Line::from(Span::styled(
        "Press F1 or Esc to close",
        Style::default().fg(Theme::text_muted()),
    )));

    frame.render_widget(Paragraph::new(help_lines), inner);
}

/// Format a slice of chords as the F1-help key column, e.g.
/// `"ctrl+y / f4"`. Empty input renders as `"<unbound>"` — should not
/// occur for built-in actions, but keeps the overlay legible if a user
/// override drops every chord.
fn chords_display(chords: &[KeyChord]) -> String {
    if chords.is_empty() {
        return "<unbound>".into();
    }
    chords
        .iter()
        .map(KeyChord::display)
        .collect::<Vec<_>>()
        .join(" / ")
}

fn help_section(title: &'static str) -> Line<'static> {
    Line::from(Span::styled(title, Theme::section_header()))
}

fn help_line(key: String, desc: &'static str) -> Line<'static> {
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
