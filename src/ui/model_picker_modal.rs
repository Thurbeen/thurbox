use ratatui::{
    layout::{Constraint, Direction, Layout},
    widgets::{List, ListItem, Paragraph},
    Frame,
};

use super::{
    centered_fixed_height_rect, render_modal_frame, selector_list_item, selector_nav_footer,
};
use crate::session::MODEL_CHOICES;

pub struct ModelPickerState {
    pub selected_index: usize,
}

pub fn render_model_picker_modal(frame: &mut Frame, state: &ModelPickerState) {
    let height = (MODEL_CHOICES.len() as u16) + 3;
    let area = centered_fixed_height_rect(50, height, frame.area());

    let inner = render_modal_frame(frame, area, "Session Model");

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(1)])
        .split(inner);

    let items: Vec<ListItem<'_>> = MODEL_CHOICES
        .iter()
        .enumerate()
        .map(|(i, (_, name))| selector_list_item(name, i == state.selected_index))
        .collect();

    frame.render_widget(List::new(items), chunks[0]);
    frame.render_widget(Paragraph::new(selector_nav_footer()), chunks[1]);
}
