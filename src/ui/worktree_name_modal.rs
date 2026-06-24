use ratatui::{
    layout::{Constraint, Direction, Layout},
    Frame,
};

use super::centered_fixed_height_rect;
use super::render_modal_frame;

pub struct WorktreeNameState<'a> {
    pub name: &'a str,
    pub cursor: usize,
    pub base_branch: &'a str,
}

pub fn render_worktree_name_modal(
    frame: &mut Frame,
    state: &WorktreeNameState<'_>,
) -> super::ModalButtons {
    let area = centered_fixed_height_rect(50, 8, frame.area());

    let title = format!("New Branch (from {})", state.base_branch);
    let inner = render_modal_frame(frame, area, &title);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(3), Constraint::Min(1)])
        .split(inner);

    super::render_text_field(
        frame,
        chunks[0],
        "Branch Name",
        state.name,
        state.cursor,
        true,
    );

    super::render_action_footer(
        frame,
        chunks[1],
        (
            "Confirm",
            crossterm::event::KeyCode::Enter,
            crossterm::event::KeyModifiers::NONE,
        ),
        "Cancel",
    )
}
