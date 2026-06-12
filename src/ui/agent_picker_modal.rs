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

/// One selectable agent row: display name + the CLI command it launches.
#[derive(Debug, Clone, Default)]
pub struct AgentChoice {
    pub name: String,
    pub command: String,
}

#[derive(Debug, Clone, Default)]
pub struct AgentPickerState {
    pub choices: Vec<AgentChoice>,
    pub selected_index: usize,
}

pub fn render_agent_picker_modal(
    frame: &mut Frame,
    state: &AgentPickerState,
) -> super::SelectorHits {
    let height = (state.choices.len() as u16) + 3;
    let area = centered_fixed_height_rect(50, height, frame.area());

    let inner = render_modal_frame(frame, area, "Coding Agent");

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(1)])
        .split(inner);

    let lines: Vec<Line<'_>> = state
        .choices
        .iter()
        .enumerate()
        .map(|(i, c)| {
            let label = if c.name == c.command {
                c.name.clone()
            } else {
                format!("{}  ({})", c.name, c.command)
            };
            selector_line(&label, i == state.selected_index)
        })
        .collect();

    frame.render_widget(Paragraph::new(selector_nav_footer()), chunks[1]);
    render_selector_rows(frame, chunks[0], lines, state.selected_index)
}
