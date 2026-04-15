use ratatui::{
    layout::{Constraint, Direction, Layout},
    text::{Line, Span},
    widgets::{List, ListItem, Paragraph},
    Frame,
};

use super::centered_fixed_height_rect;
use super::render_modal_frame;
use super::theme::Theme;
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
        .map(|(i, (_, name))| {
            let style = if i == state.selected_index {
                Theme::selected_item()
            } else {
                Theme::normal_item()
            };
            let prefix = if i == state.selected_index {
                "▸ "
            } else {
                "  "
            };
            ListItem::new(Line::from(Span::styled(format!("{prefix}{name}"), style)))
        })
        .collect();

    frame.render_widget(List::new(items), chunks[0]);

    let footer = Line::from(vec![
        Span::styled("j/k", Theme::keybind()),
        Span::styled(" navigate  ", Theme::keybind_desc()),
        Span::styled("Enter", Theme::keybind()),
        Span::styled(" select  ", Theme::keybind_desc()),
        Span::styled("Esc", Theme::keybind()),
        Span::styled(" cancel", Theme::keybind_desc()),
    ]);
    frame.render_widget(Paragraph::new(footer), chunks[1]);
}
