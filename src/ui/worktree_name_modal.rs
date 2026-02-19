use ratatui::{
    layout::{Constraint, Direction, Layout},
    style::Style,
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
    Frame,
};

use super::centered_fixed_height_rect;
use super::theme::Theme;

pub struct WorktreeNameState<'a> {
    pub name: &'a str,
    pub cursor: usize,
    pub base_branch: &'a str,
}

pub fn render_worktree_name_modal(frame: &mut Frame, state: &WorktreeNameState<'_>) {
    let area = centered_fixed_height_rect(50, 8, frame.area());

    frame.render_widget(Clear, area);

    let block = Block::default()
        .title(format!(" New Branch (from {}) ", state.base_branch))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Theme::ACCENT));

    let inner = block.inner(area);
    frame.render_widget(block, area);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // Name field
            Constraint::Min(1),    // Footer
        ])
        .split(inner);

    super::render_text_field(
        frame,
        chunks[0],
        "Branch Name",
        state.name,
        state.cursor,
        true,
    );

    let footer = Line::from(vec![
        Span::styled("Enter", Theme::keybind()),
        Span::styled(" confirm  ", Theme::keybind_desc()),
        Span::styled("Esc", Theme::keybind()),
        Span::styled(" cancel", Theme::keybind_desc()),
    ]);
    frame.render_widget(Paragraph::new(footer), chunks[1]);
}
