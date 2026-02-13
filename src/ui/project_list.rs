use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{List, ListItem, ListState, Paragraph},
    Frame,
};

use super::focused_block;
use crate::session::SessionInfo;

pub struct ProjectEntry<'a> {
    pub name: &'a str,
    pub session_count: usize,
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
    pub focus: LeftPanelFocus,
    pub panel_focused: bool,
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
        state.panel_focused && state.focus == LeftPanelFocus::Projects,
    );

    render_session_section(
        frame,
        chunks[1],
        state.sessions,
        state.active_session,
        state.panel_focused && state.focus == LeftPanelFocus::Sessions,
    );
}

fn render_project_section(
    frame: &mut Frame,
    area: Rect,
    projects: &[ProjectEntry<'_>],
    active_index: usize,
    focused: bool,
) {
    let block = focused_block(" Projects ", focused);

    let items: Vec<ListItem> = projects
        .iter()
        .enumerate()
        .map(|(i, project)| {
            let style = if i == active_index {
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::White)
            };

            let indicator = if i == active_index { "▸" } else { " " };

            let line = Line::from(vec![
                Span::styled(format!("{indicator} "), style),
                Span::styled(project.name, style),
                Span::styled(
                    format!(" ({})", project.session_count),
                    Style::default().fg(Color::DarkGray),
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
    focused: bool,
) {
    let block = focused_block(" Sessions ", focused);

    if sessions.is_empty() {
        let text = Paragraph::new("Ctrl+N to create session")
            .block(block)
            .style(Style::default().fg(Color::DarkGray));
        frame.render_widget(text, area);
        return;
    }

    let items: Vec<ListItem> = sessions
        .iter()
        .enumerate()
        .map(|(i, info)| {
            let style = if i == active_index {
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::White)
            };

            let mut spans = vec![
                Span::styled(
                    format!("{} ", info.status.icon()),
                    Style::default().fg(super::status_color(info.status)),
                ),
                Span::styled(&info.name, style),
            ];

            if let Some(wt) = &info.worktree {
                spans.push(Span::styled(
                    format!(" [{}]", wt.branch),
                    Style::default().fg(Color::Green),
                ));
            }

            ListItem::new(Line::from(spans))
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
