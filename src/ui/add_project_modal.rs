use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
    Frame,
};

use super::centered_fixed_height_rect;
use crate::app::AddProjectField;

pub struct AddProjectModalState<'a> {
    pub name: &'a str,
    pub name_cursor: usize,
    pub path: &'a str,
    pub path_cursor: usize,
    pub focused_field: AddProjectField,
}

pub fn render_add_project_modal(frame: &mut Frame, state: &AddProjectModalState<'_>) {
    let area = centered_fixed_height_rect(50, 11, frame.area());

    frame.render_widget(Clear, area);

    let block = Block::default()
        .title(" Add Project ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan));

    let inner = block.inner(area);
    frame.render_widget(block, area);

    // Layout: name field, gap, path field, gap, footer
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // Name field
            Constraint::Length(3), // Path field
            Constraint::Min(1),    // Footer
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

    render_text_field(
        frame,
        chunks[1],
        "Repo",
        state.path,
        state.path_cursor,
        state.focused_field == AddProjectField::Path,
    );

    let footer = Line::from(vec![
        Span::styled("Tab", Style::default().fg(Color::Yellow)),
        Span::styled(" switch  ", Style::default().fg(Color::DarkGray)),
        Span::styled("Enter", Style::default().fg(Color::Yellow)),
        Span::styled(" submit  ", Style::default().fg(Color::DarkGray)),
        Span::styled("Esc", Style::default().fg(Color::Yellow)),
        Span::styled(" cancel", Style::default().fg(Color::DarkGray)),
    ]);
    let footer_paragraph = Paragraph::new(footer);
    frame.render_widget(footer_paragraph, chunks[2]);
}

fn render_text_field(
    frame: &mut Frame,
    area: Rect,
    label: &str,
    value: &str,
    cursor: usize,
    focused: bool,
) {
    let border_color = if focused { Color::Cyan } else { Color::Gray };

    let block = Block::default()
        .title(format!(" {label} "))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(border_color));

    let inner = block.inner(area);

    // Build display text with cursor
    let display = if focused {
        let chars: Vec<char> = value.chars().collect();
        let (before, after) = if cursor <= chars.len() {
            let before: String = chars[..cursor].iter().collect();
            let after: String = chars[cursor..].iter().collect();
            (before, after)
        } else {
            (value.to_string(), String::new())
        };

        let cursor_char = if after.is_empty() {
            " ".to_string()
        } else {
            after.chars().next().unwrap().to_string()
        };

        let rest = if after.len() > cursor_char.len() {
            &after[cursor_char.len()..]
        } else {
            ""
        };

        Line::from(vec![
            Span::styled(before, Style::default().fg(Color::White)),
            Span::styled(
                cursor_char,
                Style::default().fg(Color::Black).bg(Color::White),
            ),
            Span::styled(rest.to_string(), Style::default().fg(Color::White)),
        ])
    } else {
        Line::from(Span::styled(value, Style::default().fg(Color::White)))
    };

    frame.render_widget(block, area);
    frame.render_widget(Paragraph::new(display), inner);
}
