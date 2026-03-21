use ratatui::{
    layout::{Constraint, Direction, Layout},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use crate::app::ScheduleCommandField;

use super::centered_fixed_height_rect;
use super::render_modal_frame;
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

    let title = format!("Schedule Command → {}", state.session_name);
    let inner = render_modal_frame(frame, area, &title);

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
