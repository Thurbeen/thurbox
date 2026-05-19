use ratatui::{
    layout::{Constraint, Direction, Layout},
    text::{Line, Span},
    widgets::{List, ListItem, Paragraph},
    Frame,
};

use super::centered_fixed_height_rect;
use super::render_modal_frame;
use super::theme::Theme;

const MODES: [&str; 2] = ["Normal", "Worktree"];

pub struct SessionModeState {
    pub selected_index: usize,
}

impl SessionModeState {
    pub fn mode_names(&self) -> Vec<&'static str> {
        MODES.to_vec()
    }

    pub fn mode_count(&self) -> usize {
        MODES.len()
    }
}

pub fn render_session_mode_modal(frame: &mut Frame, state: &SessionModeState) {
    let mode_count = state.mode_count();
    // Height = modes + 2 (border) + 1 (footer)
    let height = mode_count as u16 + 3;
    let area = centered_fixed_height_rect(50, height, frame.area());

    let inner = render_modal_frame(frame, area, "Session Mode");

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(1),    // Mode list
            Constraint::Length(1), // Footer
        ])
        .split(inner);

    let modes = state.mode_names();

    let items: Vec<ListItem<'_>> = modes
        .iter()
        .enumerate()
        .map(|(i, mode)| {
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
            ListItem::new(Line::from(Span::styled(format!("{prefix}{mode}"), style)))
        })
        .collect();

    let list = List::new(items);
    frame.render_widget(list, chunks[0]);

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
