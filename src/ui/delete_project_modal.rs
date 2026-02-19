use ratatui::{
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
    Frame,
};

use super::centered_fixed_height_rect;
use super::theme::Theme;

pub struct DeleteProjectModalState<'a> {
    pub project_name: &'a str,
    pub confirmation: &'a str,
    pub confirmation_cursor: usize,
    pub error: Option<&'a str>,
}

pub fn render_delete_project_modal(frame: &mut Frame, state: &DeleteProjectModalState<'_>) {
    let area = centered_fixed_height_rect(60, 13, frame.area());

    frame.render_widget(Clear, area);

    let block = Block::default()
        .title(" Delete Project ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Theme::DANGER));

    let inner = block.inner(area);
    frame.render_widget(block, area);

    // Layout: warning, spacer, confirmation input, error message, help
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // Warning text
            Constraint::Length(1), // Spacer
            Constraint::Length(3), // Confirmation input
            Constraint::Length(2), // Error message
            Constraint::Min(1),    // Help text
        ])
        .split(inner);

    // Warning text
    let warning = Paragraph::new(vec![
        Line::from(Span::styled(
            "⚠ Delete Project",
            Style::default()
                .fg(Theme::DANGER)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(vec![
            Span::raw("Type "),
            Span::styled(
                state.project_name,
                Style::default()
                    .fg(Theme::ACCENT)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(" to confirm"),
        ]),
    ])
    .alignment(Alignment::Left);
    frame.render_widget(warning, chunks[0]);

    // Confirmation input
    render_confirmation_input(frame, chunks[2], state);

    // Error message
    if let Some(error) = state.error {
        let error_msg = Paragraph::new(error)
            .style(Style::default().fg(Theme::DANGER))
            .alignment(Alignment::Left);
        frame.render_widget(error_msg, chunks[3]);
    }

    // Help text
    let help = Line::from(vec![
        Span::styled("Enter", Theme::keybind()),
        Span::raw(" confirm  "),
        Span::styled("Esc", Theme::keybind()),
        Span::raw(" cancel"),
    ]);
    let help_paragraph = Paragraph::new(help).alignment(Alignment::Left);
    frame.render_widget(help_paragraph, chunks[4]);
}

fn render_confirmation_input(frame: &mut Frame, area: Rect, state: &DeleteProjectModalState<'_>) {
    let block = Block::default()
        .title(" Confirmation ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Theme::TEXT_PRIMARY));

    let inner = block.inner(area);
    frame.render_widget(block, area);

    // Build the text with cursor
    let chars: Vec<char> = state.confirmation.chars().collect();
    let cursor = state.confirmation_cursor.min(chars.len());

    let mut spans = Vec::new();

    if inner.width > 0 {
        // Text before cursor
        if cursor > 0 {
            let before: String = chars[..cursor].iter().collect();
            spans.push(Span::styled(
                before,
                Style::default().fg(Theme::TEXT_PRIMARY),
            ));
        }

        // Cursor character or space
        let cursor_char = if cursor < chars.len() {
            chars[cursor].to_string()
        } else {
            " ".to_string()
        };
        spans.push(Span::styled(cursor_char, Theme::cursor()));

        // Text after cursor
        if cursor + 1 < chars.len() {
            let after: String = chars[cursor + 1..].iter().collect();
            spans.push(Span::styled(
                after,
                Style::default().fg(Theme::TEXT_PRIMARY),
            ));
        }
    }

    let line = Line::from(spans);
    frame.render_widget(Paragraph::new(line), inner);
}
