use ratatui::{
    layout::{Constraint, Direction, Layout},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use super::centered_fixed_height_rect;
use super::render_modal_frame;
use super::theme::Theme;

pub struct SessionNameState<'a> {
    pub name: &'a str,
    pub cursor: usize,
}

pub fn render_session_name_modal(frame: &mut Frame, state: &SessionNameState<'_>) {
    let area = centered_fixed_height_rect(50, 8, frame.area());

    let inner = render_modal_frame(frame, area, "Session Name");

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // Name field
            Constraint::Min(1),    // Footer
        ])
        .split(inner);

    super::render_text_field(frame, chunks[0], "Name", state.name, state.cursor, true);

    let footer = Line::from(vec![
        Span::styled("Enter", Theme::keybind()),
        Span::styled(" confirm  ", Theme::keybind_desc()),
        Span::styled("Esc", Theme::keybind()),
        Span::styled(" cancel", Theme::keybind_desc()),
    ]);
    frame.render_widget(Paragraph::new(footer), chunks[1]);
}
