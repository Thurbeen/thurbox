use ratatui::{
    layout::{Constraint, Direction, Layout},
    Frame,
};

use super::centered_fixed_height_rect;
use super::render_modal_frame;

pub struct SessionNameState<'a> {
    pub name: &'a str,
    pub cursor: usize,
}

pub fn render_session_name_modal(
    frame: &mut Frame,
    state: &SessionNameState<'_>,
) -> super::ModalButtons {
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

    let hits = super::render_button_bar(
        frame,
        chunks[1],
        &[
            super::ButtonSpec::primary("Confirm"),
            super::ButtonSpec::secondary("Cancel"),
        ],
        true,
    );
    super::modal_button_keys(
        hits,
        &[
            (
                crossterm::event::KeyCode::Enter,
                crossterm::event::KeyModifiers::NONE,
            ),
            (
                crossterm::event::KeyCode::Esc,
                crossterm::event::KeyModifiers::NONE,
            ),
        ],
    )
}
