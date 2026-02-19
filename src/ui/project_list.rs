use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{List, ListItem, ListState, Paragraph},
    Frame,
};

use super::focus_block;
use super::theme::Theme;
use super::FocusLevel;
use crate::session::SessionInfo;

pub struct ProjectEntry<'a> {
    pub name: &'a str,
    pub session_count: usize,
    pub is_admin: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LeftPanelFocus {
    Projects,
    Sessions,
}

pub struct LeftPanelState<'a> {
    pub projects: &'a [ProjectEntry<'a>],
    pub active_project: usize,
    pub sessions: &'a [&'a SessionInfo],
    pub active_session: usize,
    /// Elapsed millis since last output, parallel to `sessions`.
    pub session_elapsed_ms: &'a [u64],
    pub focus: LeftPanelFocus,
    pub panel_focused: bool,
    /// Focus level for the project sub-section.
    pub project_focus: FocusLevel,
    /// Focus level for the session sub-section.
    pub session_focus: FocusLevel,
}

pub fn render_left_panel(frame: &mut Frame, area: Rect, state: &LeftPanelState<'_>) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Percentage(40), Constraint::Percentage(60)])
        .split(area);

    render_project_section(
        frame,
        chunks[0],
        state.projects,
        state.active_project,
        state.project_focus,
    );

    render_session_section(
        frame,
        chunks[1],
        state.sessions,
        state.active_session,
        state.session_elapsed_ms,
        state.session_focus,
    );
}

fn render_project_section(
    frame: &mut Frame,
    area: Rect,
    projects: &[ProjectEntry<'_>],
    active_index: usize,
    level: FocusLevel,
) {
    let block = focus_block(" Projects ", level);

    let items: Vec<ListItem> = projects
        .iter()
        .enumerate()
        .map(|(i, project)| {
            let base_color = if project.is_admin {
                Theme::ADMIN_BADGE
            } else if i == active_index {
                Theme::ACCENT
            } else {
                Theme::TEXT_PRIMARY
            };
            let style = if i == active_index {
                Style::default().fg(base_color).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(base_color)
            };

            let indicator = if i == active_index { "▸" } else { " " };
            let prefix = if project.is_admin { "⚙ " } else { "" };

            let line = Line::from(vec![
                Span::styled(format!("{indicator} "), style),
                Span::styled(format!("{prefix}{}", project.name), style),
                Span::styled(
                    format!(" ({})", project.session_count),
                    Style::default().fg(Theme::TEXT_MUTED),
                ),
            ]);

            ListItem::new(line)
        })
        .collect();

    let list = List::new(items).block(block).highlight_style(
        Style::default()
            .bg(Color::DarkGray)
            .add_modifier(Modifier::BOLD),
    );

    let mut state = ListState::default();
    state.select(Some(active_index));
    frame.render_stateful_widget(list, area, &mut state);
}

fn render_session_section(
    frame: &mut Frame,
    area: Rect,
    sessions: &[&SessionInfo],
    active_index: usize,
    elapsed_ms: &[u64],
    level: FocusLevel,
) {
    let block = focus_block(" Sessions ", level);

    if sessions.is_empty() {
        let text = Paragraph::new("Ctrl+N to create session")
            .block(block)
            .style(Style::default().fg(Theme::TEXT_MUTED));
        frame.render_widget(text, area);
        return;
    }

    let items: Vec<ListItem> = sessions
        .iter()
        .enumerate()
        .map(|(i, info)| {
            let is_active = i == active_index;
            let prefix = if is_active { "▸" } else { " " };

            // Line 1: status icon + #N + right-aligned status text
            let status_text = format_status_with_elapsed(info.status, elapsed_ms.get(i).copied());
            let name_style = if is_active {
                Theme::selected_item()
            } else {
                Theme::normal_item()
            };

            let line1 = Line::from(vec![
                Span::styled(
                    format!("{prefix} {} ", info.status.icon()),
                    Style::default().fg(super::status_color(info.status)),
                ),
                Span::styled(&info.name, name_style),
                Span::styled(
                    format!("  {status_text}"),
                    Style::default().fg(super::status_color(info.status)),
                ),
            ]);

            // Line 2: indented role name + optional · branch
            let mut line2_spans = vec![Span::styled(
                format!("    {}", info.role),
                Style::default().fg(Theme::ROLE_NAME),
            )];
            if let Some(wt) = info.worktrees.first() {
                line2_spans.push(Span::styled(" · ", Style::default().fg(Theme::TEXT_MUTED)));
                line2_spans.push(Span::styled(
                    &wt.branch,
                    Style::default().fg(Theme::BRANCH_NAME),
                ));
            }
            let line2 = Line::from(line2_spans);

            ListItem::new(vec![line1, line2])
        })
        .collect();

    let list = List::new(items).block(block).highlight_style(
        Style::default()
            .bg(Color::DarkGray)
            .add_modifier(Modifier::BOLD),
    );

    let mut state = ListState::default();
    state.select(Some(active_index));
    frame.render_stateful_widget(list, area, &mut state);
}

/// Format status text with elapsed time for Waiting/Idle sessions.
fn format_status_with_elapsed(
    status: crate::session::SessionStatus,
    elapsed_ms: Option<u64>,
) -> String {
    use crate::session::SessionStatus;
    match (status, elapsed_ms) {
        (SessionStatus::Waiting | SessionStatus::Idle, Some(ms)) if ms >= 60_000 => {
            let mins = ms / 60_000;
            format!("{status} {mins}m")
        }
        (SessionStatus::Waiting | SessionStatus::Idle, Some(ms)) if ms >= 10_000 => {
            let secs = ms / 1_000;
            format!("{status} {secs}s")
        }
        _ => format!("{status}"),
    }
}
