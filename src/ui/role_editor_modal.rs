use ratatui::{
    layout::{Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, Paragraph},
    Frame,
};

use super::{centered_fixed_height_rect, render_text_field};
use crate::session::RoleConfig;

// ── Role List View ──────────────────────────────────────────────────────────

pub struct RoleListState<'a> {
    pub roles: &'a [RoleConfig],
    pub selected_index: usize,
}

pub fn render_role_list_modal(frame: &mut Frame, state: &RoleListState<'_>) {
    // 2 (border) + max(roles, 1) + 1 (description) + 1 (footer)
    let list_rows = if state.roles.is_empty() {
        1
    } else {
        state.roles.len() as u16
    };
    let height = list_rows + 4;
    let area = centered_fixed_height_rect(50, height, frame.area());

    frame.render_widget(Clear, area);

    let block = Block::default()
        .title(" Project Roles ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan));

    let inner = block.inner(area);
    frame.render_widget(block, area);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(1),    // Role list
            Constraint::Length(1), // Description
            Constraint::Length(1), // Footer
        ])
        .split(inner);

    if state.roles.is_empty() {
        let empty = Paragraph::new(Line::from(Span::styled(
            "  No roles defined",
            Style::default().fg(Color::DarkGray),
        )));
        frame.render_widget(empty, chunks[0]);
    } else {
        let items: Vec<ListItem<'_>> = state
            .roles
            .iter()
            .enumerate()
            .map(|(i, role)| {
                let style = if i == state.selected_index {
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(Color::White)
                };
                let prefix = if i == state.selected_index {
                    "▸ "
                } else {
                    "  "
                };
                ListItem::new(Line::from(Span::styled(
                    format!("{prefix}{}", role.name),
                    style,
                )))
            })
            .collect();

        let list = List::new(items);
        frame.render_widget(list, chunks[0]);

        // Description of selected role
        if let Some(role) = state.roles.get(state.selected_index) {
            let desc = Line::from(Span::styled(
                &role.description,
                Style::default().fg(Color::DarkGray),
            ));
            frame.render_widget(Paragraph::new(desc), chunks[1]);
        }
    }

    let footer = Line::from(vec![
        Span::styled("a", Style::default().fg(Color::Yellow)),
        Span::styled(" add  ", Style::default().fg(Color::DarkGray)),
        Span::styled("e", Style::default().fg(Color::Yellow)),
        Span::styled("/", Style::default().fg(Color::DarkGray)),
        Span::styled("Enter", Style::default().fg(Color::Yellow)),
        Span::styled(" edit  ", Style::default().fg(Color::DarkGray)),
        Span::styled("d", Style::default().fg(Color::Yellow)),
        Span::styled(" delete  ", Style::default().fg(Color::DarkGray)),
        Span::styled("Esc", Style::default().fg(Color::Yellow)),
        Span::styled(" save & close", Style::default().fg(Color::DarkGray)),
    ]);
    frame.render_widget(Paragraph::new(footer), chunks[2]);
}

// ── Role Editor View ────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoleEditorField {
    Name,
    Description,
    AllowedTools,
    DisallowedTools,
}

pub struct RoleEditorState<'a> {
    pub name: &'a str,
    pub name_cursor: usize,
    pub description: &'a str,
    pub description_cursor: usize,
    pub allowed_tools: &'a str,
    pub allowed_tools_cursor: usize,
    pub disallowed_tools: &'a str,
    pub disallowed_tools_cursor: usize,
    pub focused_field: RoleEditorField,
}

pub fn render_role_editor_modal(frame: &mut Frame, state: &RoleEditorState<'_>) {
    // 2 (border) + 4*3 (text fields) + 1 (footer) = 15
    let height = 15;
    let area = centered_fixed_height_rect(60, height, frame.area());

    frame.render_widget(Clear, area);

    let block = Block::default()
        .title(" Edit Role ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan));

    let inner = block.inner(area);
    frame.render_widget(block, area);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // Name
            Constraint::Length(3), // Description
            Constraint::Length(3), // Allowed Tools
            Constraint::Length(3), // Disallowed Tools
            Constraint::Length(1), // Footer
        ])
        .split(inner);

    render_text_field(
        frame,
        chunks[0],
        "Name",
        state.name,
        state.name_cursor,
        state.focused_field == RoleEditorField::Name,
    );

    render_text_field(
        frame,
        chunks[1],
        "Description",
        state.description,
        state.description_cursor,
        state.focused_field == RoleEditorField::Description,
    );

    render_text_field(
        frame,
        chunks[2],
        "Allowed Tools (space-separated)",
        state.allowed_tools,
        state.allowed_tools_cursor,
        state.focused_field == RoleEditorField::AllowedTools,
    );

    render_text_field(
        frame,
        chunks[3],
        "Disallowed Tools (space-separated)",
        state.disallowed_tools,
        state.disallowed_tools_cursor,
        state.focused_field == RoleEditorField::DisallowedTools,
    );

    // Footer
    let footer = Line::from(vec![
        Span::styled("Tab", Style::default().fg(Color::Yellow)),
        Span::styled(" switch  ", Style::default().fg(Color::DarkGray)),
        Span::styled("Enter", Style::default().fg(Color::Yellow)),
        Span::styled(" save  ", Style::default().fg(Color::DarkGray)),
        Span::styled("Esc", Style::default().fg(Color::Yellow)),
        Span::styled(" discard", Style::default().fg(Color::DarkGray)),
    ]);
    frame.render_widget(Paragraph::new(footer), chunks[4]);
}
