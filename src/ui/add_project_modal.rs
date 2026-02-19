use std::path::PathBuf;

use ratatui::{
    layout::{Constraint, Direction, Layout},
    style::Style,
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
    Frame,
};

use super::theme::Theme;
use super::{centered_fixed_height_rect, render_text_field, render_text_field_with_suggestion};
use crate::app::AddProjectField;

pub struct AddProjectModalState<'a> {
    pub name: &'a str,
    pub name_cursor: usize,
    pub path: &'a str,
    pub path_cursor: usize,
    pub path_suggestion: Option<&'a str>,
    pub repos: &'a [PathBuf],
    pub repo_index: usize,
    pub focused_field: AddProjectField,
}

pub fn render_add_project_modal(frame: &mut Frame, state: &AddProjectModalState<'_>) {
    // Dynamic height: name(3) + path(3) + repo_list(max 6 items + 2 border) + footer(1) + outer border(2)
    let repo_list_inner = if state.repos.is_empty() {
        1
    } else {
        state.repos.len().min(6)
    };
    let repo_list_height = repo_list_inner as u16 + 2; // +2 for borders
    let total_height = 3 + 3 + repo_list_height + 1 + 2; // name + path + repos + footer + outer

    let area = centered_fixed_height_rect(50, total_height, frame.area());

    frame.render_widget(Clear, area);

    let block = Block::default()
        .title(" Add Project ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Theme::ACCENT));

    let inner = block.inner(area);
    frame.render_widget(block, area);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),                // Name field
            Constraint::Length(3),                // Path field
            Constraint::Length(repo_list_height), // Repo list
            Constraint::Min(1),                   // Footer
        ])
        .split(inner);

    render_text_field(
        frame,
        chunks[0],
        "Name",
        state.name,
        state.name_cursor,
        state.focused_field == AddProjectField::Name,
    );

    render_text_field_with_suggestion(
        frame,
        chunks[1],
        "Repo Path",
        state.path,
        state.path_cursor,
        state.focused_field == AddProjectField::Path,
        state.path_suggestion,
    );

    // Repo list
    let list_focused = state.focused_field == AddProjectField::RepoList;
    let list_border_color = if list_focused {
        Theme::BORDER_FOCUSED
    } else {
        Theme::BORDER_UNFOCUSED
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
            Style::default().fg(Theme::TEXT_MUTED),
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
                    (Theme::ACCENT, Theme::TEXT_PRIMARY)
                } else {
                    (Theme::TEXT_MUTED, Theme::TEXT_SECONDARY)
                };
                Line::from(vec![
                    Span::styled(marker, Style::default().fg(marker_color)),
                    Span::styled(path_str, Style::default().fg(path_color)),
                ])
            })
            .collect();

        frame.render_widget(Paragraph::new(lines), list_inner);
    }

    // Context-sensitive footer
    let footer = match state.focused_field {
        AddProjectField::Name => Line::from(vec![
            Span::styled("Tab", Theme::keybind()),
            Span::styled(" next  ", Theme::keybind_desc()),
            Span::styled("Enter", Theme::keybind()),
            Span::styled(" submit  ", Theme::keybind_desc()),
            Span::styled("Esc", Theme::keybind()),
            Span::styled(" cancel", Theme::keybind_desc()),
        ]),
        AddProjectField::Path => {
            let tab_hint = if state.path_suggestion.is_some() {
                " complete  "
            } else {
                " next  "
            };
            Line::from(vec![
                Span::styled("Tab", Theme::keybind()),
                Span::styled(tab_hint, Theme::keybind_desc()),
                Span::styled("Enter", Theme::keybind()),
                Span::styled(" add repo  ", Theme::keybind_desc()),
                Span::styled("Esc", Theme::keybind()),
                Span::styled(" cancel", Theme::keybind_desc()),
            ])
        }
        AddProjectField::RepoList => Line::from(vec![
            Span::styled("j/k", Theme::keybind()),
            Span::styled(" navigate  ", Theme::keybind_desc()),
            Span::styled("d", Theme::keybind()),
            Span::styled(" delete  ", Theme::keybind_desc()),
            Span::styled("Enter", Theme::keybind()),
            Span::styled(" submit  ", Theme::keybind_desc()),
            Span::styled("Esc", Theme::keybind()),
            Span::styled(" cancel", Theme::keybind_desc()),
        ]),
    };
    frame.render_widget(Paragraph::new(footer), chunks[3]);
}
