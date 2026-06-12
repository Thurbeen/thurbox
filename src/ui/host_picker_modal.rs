use ratatui::{
    layout::{Constraint, Direction, Layout},
    text::Line,
    widgets::Paragraph,
    Frame,
};

use super::{
    centered_fixed_height_rect, render_modal_frame, render_selector_rows, selector_line,
    selector_nav_footer,
};

/// One selectable host row: display label + the backend name it maps to
/// (`local-tmux` or `ssh:<host>`).
#[derive(Debug, Clone, Default)]
pub struct HostChoice {
    /// Display label (e.g. `local` or `devbox  (me@devbox)`).
    pub label: String,
    /// Backend name spawned on. Empty string means the local default backend.
    pub backend: String,
}

#[derive(Debug, Clone, Default)]
pub struct HostPickerState {
    pub choices: Vec<HostChoice>,
    pub selected_index: usize,
}

pub fn render_host_picker_modal(frame: &mut Frame, state: &HostPickerState) -> super::SelectorHits {
    let height = (state.choices.len() as u16) + 3;
    let area = centered_fixed_height_rect(50, height, frame.area());

    let inner = render_modal_frame(frame, area, "Run On");

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(1)])
        .split(inner);

    let lines: Vec<Line<'_>> = state
        .choices
        .iter()
        .enumerate()
        .map(|(i, c)| selector_line(&c.label, i == state.selected_index))
        .collect();

    frame.render_widget(Paragraph::new(selector_nav_footer()), chunks[1]);
    render_selector_rows(frame, chunks[0], lines, state.selected_index)
}
