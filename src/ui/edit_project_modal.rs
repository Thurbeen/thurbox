use std::path::PathBuf;

use ratatui::{
    layout::{Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, Paragraph},
    Frame,
};

use super::{centered_fixed_height_rect, render_text_field, render_text_field_with_suggestion};
use crate::app::EditProjectField;
use crate::session::RoleConfig;

pub struct EditProjectModalState<'a> {
    pub name: &'a str,
    pub name_cursor: usize,
    pub path: &'a str,
    pub path_cursor: usize,
    pub path_suggestion: Option<&'a str>,
    pub repos: &'a [PathBuf],
    pub repo_index: usize,
    pub roles: &'a [RoleConfig],
    pub role_index: usize,
    pub focused_field: EditProjectField,
}

pub fn render_edit_project_modal(frame: &mut Frame, state: &EditProjectModalState<'_>) {
    // Dynamic height: name(3) + path(3) + repo_list + roles_list + footer(1) + outer border(2)
    let repo_list_inner = if state.repos.is_empty() {
        1
    } else {
        state.repos.len().min(6)
    };
    let repo_list_height = repo_list_inner as u16 + 2; // +2 for borders

    let roles_list_inner = if state.roles.is_empty() {
        1
    } else {
        state.roles.len().min(6)
    };
    let roles_list_height = roles_list_inner as u16 + 2; // +2 for borders

    let total_height = 3 + 3 + repo_list_height + roles_list_height + 1 + 2;

    let area = centered_fixed_height_rect(50, total_height, frame.area());

    frame.render_widget(Clear, area);

    let block = Block::default()
        .title(" Edit Project ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan));

    let inner = block.inner(area);
    frame.render_widget(block, area);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),                 // Name field
            Constraint::Length(3),                 // Path field
            Constraint::Length(repo_list_height),  // Repo list
            Constraint::Length(roles_list_height), // Roles list
            Constraint::Min(1),                    // Footer
        ])
        .split(inner);

    render_text_field(
        frame,
        chunks[0],
        "Name",
        state.name,
        state.name_cursor,
        state.focused_field == EditProjectField::Name,
    );

    render_text_field_with_suggestion(
        frame,
        chunks[1],
        "Add Repo Path",
        state.path,
        state.path_cursor,
        state.focused_field == EditProjectField::Path,
        state.path_suggestion,
    );

    // Repo list
    let list_focused = state.focused_field == EditProjectField::RepoList;
    let list_border_color = if list_focused {
        Color::Cyan
    } else {
        Color::Gray
    };

    let list_block = Block::default()
        .title(format!(" Repos ({}) ", state.repos.len()))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(list_border_color));

    let list_inner = list_block.inner(chunks[2]);
    frame.render_widget(list_block, chunks[2]);

    if state.repos.is_empty() {
        let placeholder = Paragraph::new(Line::from(Span::styled(
            "(none — add via Path field above)",
            Style::default().fg(Color::DarkGray),
        )));
        frame.render_widget(placeholder, list_inner);
    } else {
        let visible_count = list_inner.height as usize;
        // Scroll so that selected item is always visible
        let scroll_offset = if state.repo_index >= visible_count {
            state.repo_index - visible_count + 1
        } else {
            0
        };

        let lines: Vec<Line> = state
            .repos
            .iter()
            .enumerate()
            .skip(scroll_offset)
            .take(visible_count)
            .map(|(i, path)| {
                let selected = i == state.repo_index && list_focused;
                let marker = if selected { "▸ " } else { "  " };
                let path_str = path.display().to_string();
                let (marker_color, path_color) = if selected {
                    (Color::Cyan, Color::White)
                } else {
                    (Color::DarkGray, Color::Gray)
                };
                Line::from(vec![
                    Span::styled(marker, Style::default().fg(marker_color)),
                    Span::styled(path_str, Style::default().fg(path_color)),
                ])
            })
            .collect();

        frame.render_widget(Paragraph::new(lines), list_inner);
    }

    // Roles list (inline, with j/k/a/e/d navigation when focused)
    let roles_focused = state.focused_field == EditProjectField::Roles;
    let roles_border_color = if roles_focused {
        Color::Cyan
    } else {
        Color::Gray
    };

    let roles_block = Block::default()
        .title(format!(" Roles ({}) ", state.roles.len()))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(roles_border_color));

    let roles_inner = roles_block.inner(chunks[3]);
    frame.render_widget(roles_block, chunks[3]);

    if state.roles.is_empty() {
        let placeholder = Paragraph::new(Line::from(Span::styled(
            "  No roles defined",
            Style::default().fg(Color::DarkGray),
        )));
        frame.render_widget(placeholder, roles_inner);
    } else {
        let visible_count = roles_inner.height as usize;
        let scroll_offset = if state.role_index >= visible_count {
            state.role_index - visible_count + 1
        } else {
            0
        };

        let items: Vec<ListItem<'_>> = state
            .roles
            .iter()
            .enumerate()
            .skip(scroll_offset)
            .take(visible_count)
            .map(|(i, role)| {
                let is_selected = i == state.role_index && roles_focused;
                let style = if is_selected {
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(Color::White)
                };
                let prefix = if is_selected { "▸ " } else { "  " };
                ListItem::new(Line::from(Span::styled(
                    format!("{prefix}{}", role.name),
                    style,
                )))
            })
            .collect();

        frame.render_widget(List::new(items), roles_inner);
    }

    // Context-sensitive footer
    let footer = match state.focused_field {
        EditProjectField::Name => Line::from(vec![
            Span::styled("Tab", Style::default().fg(Color::Yellow)),
            Span::styled(" next  ", Style::default().fg(Color::DarkGray)),
            Span::styled("Enter", Style::default().fg(Color::Yellow)),
            Span::styled(" save  ", Style::default().fg(Color::DarkGray)),
            Span::styled("Esc", Style::default().fg(Color::Yellow)),
            Span::styled(" cancel", Style::default().fg(Color::DarkGray)),
        ]),
        EditProjectField::Path => {
            let tab_hint = if state.path_suggestion.is_some() {
                " complete  "
            } else {
                " next  "
            };
            Line::from(vec![
                Span::styled("Tab", Style::default().fg(Color::Yellow)),
                Span::styled(tab_hint, Style::default().fg(Color::DarkGray)),
                Span::styled("Enter", Style::default().fg(Color::Yellow)),
                Span::styled(" add repo  ", Style::default().fg(Color::DarkGray)),
                Span::styled("Esc", Style::default().fg(Color::Yellow)),
                Span::styled(" cancel", Style::default().fg(Color::DarkGray)),
            ])
        }
        EditProjectField::RepoList => Line::from(vec![
            Span::styled("j/k", Style::default().fg(Color::Yellow)),
            Span::styled(" navigate  ", Style::default().fg(Color::DarkGray)),
            Span::styled("d", Style::default().fg(Color::Yellow)),
            Span::styled(" delete  ", Style::default().fg(Color::DarkGray)),
            Span::styled("Enter", Style::default().fg(Color::Yellow)),
            Span::styled(" save  ", Style::default().fg(Color::DarkGray)),
            Span::styled("Esc", Style::default().fg(Color::Yellow)),
            Span::styled(" cancel", Style::default().fg(Color::DarkGray)),
        ]),
        EditProjectField::Roles => Line::from(vec![
            Span::styled("j/k", Style::default().fg(Color::Yellow)),
            Span::styled(" navigate  ", Style::default().fg(Color::DarkGray)),
            Span::styled("a", Style::default().fg(Color::Yellow)),
            Span::styled(" add  ", Style::default().fg(Color::DarkGray)),
            Span::styled("e", Style::default().fg(Color::Yellow)),
            Span::styled(" edit  ", Style::default().fg(Color::DarkGray)),
            Span::styled("d", Style::default().fg(Color::Yellow)),
            Span::styled(" delete  ", Style::default().fg(Color::DarkGray)),
            Span::styled("Esc", Style::default().fg(Color::Yellow)),
            Span::styled(" save", Style::default().fg(Color::DarkGray)),
        ]),
    };
    frame.render_widget(Paragraph::new(footer), chunks[4]);
}
