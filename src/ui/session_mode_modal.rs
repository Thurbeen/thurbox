use ratatui::{
    layout::{Constraint, Direction, Layout},
    style::Style,
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, Paragraph},
    Frame,
};

use super::centered_fixed_height_rect;
use super::theme::Theme;

const MODES: [&str; 2] = ["Normal", "Worktree"];

pub struct SessionModeState {
    pub selected_index: usize,
}

pub fn render_session_mode_modal(frame: &mut Frame, state: &SessionModeState) {
    let area = centered_fixed_height_rect(50, 6, frame.area());

    frame.render_widget(Clear, area);

    let block = Block::default()
        .title(" Session Mode ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Theme::ACCENT));

    let inner = block.inner(area);
    frame.render_widget(block, area);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(1),    // Mode list
            Constraint::Length(1), // Footer
        ])
        .split(inner);

    let items: Vec<ListItem<'_>> = MODES
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
