use ratatui::{
    layout::{Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, Paragraph},
    Frame,
};

use super::centered_fixed_height_rect;

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
        .border_style(Style::default().fg(Color::Cyan));

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
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::White)
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
        Span::styled("j/k", Style::default().fg(Color::Yellow)),
        Span::styled(" navigate  ", Style::default().fg(Color::DarkGray)),
        Span::styled("Enter", Style::default().fg(Color::Yellow)),
        Span::styled(" select  ", Style::default().fg(Color::DarkGray)),
        Span::styled("Esc", Style::default().fg(Color::Yellow)),
        Span::styled(" cancel", Style::default().fg(Color::DarkGray)),
    ]);
    frame.render_widget(Paragraph::new(footer), chunks[1]);
}
