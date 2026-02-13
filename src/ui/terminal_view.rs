use ratatui::{
    layout::Rect,
    style::{Color, Style},
    widgets::{Block, Borders},
    Frame,
};
use tui_term::widget::PseudoTerminal;

use crate::session::SessionInfo;

pub fn render_terminal(
    frame: &mut Frame,
    area: Rect,
    parser: &vt100::Parser,
    info: &SessionInfo,
    focused: bool,
) {
    let border_color = if focused { Color::Cyan } else { Color::Gray };
    let title = format!(" {} [{}] ", info.name, info.status);

    let block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_style(Style::default().fg(border_color));

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

    let text = ratatui::widgets::Paragraph::new("Press Ctrl+N to create a new session")
        .block(block)
        .style(Style::default().fg(Color::DarkGray));

    frame.render_widget(text, area);
}
