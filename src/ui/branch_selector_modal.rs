use ratatui::{
    layout::{Constraint, Direction, Layout},
    text::Line,
    Frame,
};

use super::centered_fixed_height_rect;
use super::render_modal_frame;
use super::{render_selector_footer, render_selector_rows, selector_line};

pub struct BranchSelectorState<'a> {
    pub branches: &'a [String],
    pub selected_index: usize,
}

pub fn render_branch_selector_modal(
    frame: &mut Frame,
    state: &BranchSelectorState<'_>,
) -> super::ModalRender {
    let height = (state.branches.len().min(15) + 4) as u16;
    let area = centered_fixed_height_rect(50, height, frame.area());

    let inner = render_modal_frame(frame, area, "Base Branch");

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(1),    // Branch list
            Constraint::Length(1), // Footer
        ])
        .split(inner);

    // Windowed around the selection with a scrollbar when more branches than
    // fit (the modal caps at 15 visible).
    let lines: Vec<Line<'_>> = state
        .branches
        .iter()
        .enumerate()
        .map(|(i, branch)| selector_line(branch, i == state.selected_index))
        .collect();

    let buttons = render_selector_footer(frame, chunks[1]);
    let hits = render_selector_rows(frame, chunks[0], lines, state.selected_index);
    (hits, buttons)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The modal caps at 15 visible branches plus 4 lines of chrome
    /// (2 borders + 1 title line padding + 1 footer).
    #[test]
    fn modal_height_caps_at_15_branches() {
        let height = |n: usize| (n.min(15) + 4) as u16;
        assert_eq!(height(1), 5);
        assert_eq!(height(15), 19);
        assert_eq!(height(30), 19); // capped
        assert_eq!(height(0), 4);
    }

    #[test]
    fn branch_selector_state_holds_index() {
        let branches = vec!["main".to_string(), "dev".to_string(), "feature".to_string()];
        let state = BranchSelectorState {
            branches: &branches,
            selected_index: 2,
        };
        assert_eq!(state.selected_index, 2);
        assert_eq!(state.branches.len(), 3);
    }
}
