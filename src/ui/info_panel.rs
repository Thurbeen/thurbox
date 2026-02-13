use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
    Frame,
};

use crate::session::SessionInfo;

pub fn render_info_panel(frame: &mut Frame, area: Rect, info: &SessionInfo) {
    let block = Block::default()
        .title(" Info ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Gray));

    let lines = vec![
        Line::from(vec![
            Span::styled("Name: ", Style::default().fg(Color::DarkGray)),
            Span::styled(&info.name, Style::default().fg(Color::White)),
        ]),
        Line::from(""),
        Line::from(vec![
            Span::styled("Status: ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                format!("{} {}", info.status_icon(), info.status),
                Style::default()
                    .fg(status_color(&info.status))
                    .add_modifier(Modifier::BOLD),
            ),
        ]),
        Line::from(""),
        Line::from(vec![
            Span::styled("ID: ", Style::default().fg(Color::DarkGray)),
            Span::styled(info.id.to_string(), Style::default().fg(Color::DarkGray)),
        ]),
    ];

    let paragraph = Paragraph::new(lines).block(block);
    frame.render_widget(paragraph, area);
}

fn status_color(status: &crate::session::SessionStatus) -> Color {
    match status {
        crate::session::SessionStatus::Running => Color::Green,
        crate::session::SessionStatus::Idle => Color::Yellow,
        crate::session::SessionStatus::Error => Color::Red,
    }
}
