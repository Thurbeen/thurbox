use ratatui::{
    layout::{Constraint, Direction, Layout},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use super::centered_fixed_height_rect;
use super::render_modal_frame_danger;
use super::theme::Theme;

pub struct ConfirmDeleteState<'a> {
    pub session_name: &'a str,
}

/// Render the hard-delete confirmation prompt. Shown only when the
/// `soft_delete` feature flag is off, where a delete is irreversible (the tmux
/// window + worktrees are torn down with no Ctrl+Z undo), so it warns before
/// acting.
pub fn render_confirm_delete_modal(frame: &mut Frame, state: &ConfirmDeleteState<'_>) {
    let area = centered_fixed_height_rect(60, 9, frame.area());

    let inner = render_modal_frame_danger(frame, area, "Delete session");

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // Prompt
            Constraint::Length(1), // Spacer
            Constraint::Length(1), // Warning
            Constraint::Min(1),    // Footer
        ])
        .split(inner);

    let prompt = Line::from(vec![
        Span::raw("Permanently delete "),
        Span::styled(
            format!("'{}'", state.session_name),
            Style::default().add_modifier(Modifier::BOLD),
        ),
        Span::raw("?"),
    ]);
    frame.render_widget(Paragraph::new(prompt), chunks[0]);

    let warning = Line::from(Span::styled(
        "This kills the tmux window and removes its worktrees. Cannot be undone.",
        Style::default().fg(Theme::danger()),
    ));
    frame.render_widget(Paragraph::new(warning), chunks[2]);

    let footer = Line::from(vec![
        Span::styled("Enter/y", Theme::keybind()),
        Span::styled(" delete  ", Theme::keybind_desc()),
        Span::styled("Esc/n", Theme::keybind()),
        Span::styled(" cancel", Theme::keybind_desc()),
    ]);
    frame.render_widget(Paragraph::new(footer), chunks[3]);
}
