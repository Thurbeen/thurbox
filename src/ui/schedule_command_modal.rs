use ratatui::{
    layout::{Constraint, Direction, Layout},
    style::Style,
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
    Frame,
};

use crate::app::ScheduleCommandField;

use super::centered_fixed_height_rect;
use super::theme::Theme;

pub struct ScheduleCommandState<'a> {
    pub command: &'a str,
    pub command_cursor: usize,
    pub delay_minutes: &'a str,
    pub delay_cursor: usize,
    pub focused_field: ScheduleCommandField,
    pub session_name: &'a str,
}

pub fn render_schedule_command_modal(frame: &mut Frame, state: &ScheduleCommandState<'_>) {
    let area = centered_fixed_height_rect(50, 11, frame.area());

    frame.render_widget(Clear, area);

    let block = Block::default()
        .title(format!(" Schedule Command → {} ", state.session_name))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Theme::ACCENT));

    let inner = block.inner(area);
    frame.render_widget(block, area);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // Command field
            Constraint::Length(3), // Delay field
            Constraint::Min(1),    // Footer
        ])
        .split(inner);

    super::render_text_field(
        frame,
        chunks[0],
        "Command",
        state.command,
        state.command_cursor,
        state.focused_field == ScheduleCommandField::Command,
    );

    super::render_text_field(
        frame,
        chunks[1],
        "Delay (minutes)",
        state.delay_minutes,
        state.delay_cursor,
        state.focused_field == ScheduleCommandField::Delay,
    );

    let footer = Line::from(vec![
        Span::styled("Tab", Theme::keybind()),
        Span::styled(" switch  ", Theme::keybind_desc()),
        Span::styled("Enter", Theme::keybind()),
        Span::styled(" schedule  ", Theme::keybind_desc()),
        Span::styled("Esc", Theme::keybind()),
        Span::styled(" cancel", Theme::keybind_desc()),
    ]);
    frame.render_widget(Paragraph::new(footer), chunks[2]);
}
