use ratatui::{
    layout::Rect,
    style::{Color, Style},
    widgets::{Block, Borders, Paragraph},
    Frame,
};
use tui_term::widget::PseudoTerminal;

use super::focused_block;
use crate::session::SessionInfo;

pub fn render_terminal(
    frame: &mut Frame,
    area: Rect,
    parser: &vt100::Parser,
    info: &SessionInfo,
    focused: bool,
) {
    let title = format!(" {} [{}] ", info.name, info.status);
    let block = focused_block(&title, focused);

    let pseudo_term = PseudoTerminal::new(parser.screen())
        .block(block)
        .style(Style::default().fg(Color::White).bg(Color::Reset));

    frame.render_widget(pseudo_term, area);
}

pub fn render_empty_terminal(frame: &mut Frame, area: Rect) {
    let block = Block::default()
        .title(" No Session ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::DarkGray));

    let text = Paragraph::new("Press Ctrl+N to create a new session")
        .block(block)
        .style(Style::default().fg(Color::DarkGray));

    frame.render_widget(text, area);
}
