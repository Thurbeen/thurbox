use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

pub fn render_header(frame: &mut Frame, area: Rect) {
    let header = Paragraph::new(Line::from(vec![
        Span::styled(
            " thurbox ",
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            " Multi-Session Claude Code Orchestrator",
            Style::default().fg(Color::Gray),
        ),
    ]));
    frame.render_widget(header, area);
}

pub fn render_footer(frame: &mut Frame, area: Rect, session_count: usize, error: Option<&str>) {
    let status = if let Some(err) = error {
        Line::from(vec![
            Span::styled(" ERROR ", Style::default().fg(Color::White).bg(Color::Red)),
            Span::styled(format!(" {err}"), Style::default().fg(Color::Red)),
        ])
    } else {
        Line::from(vec![
            Span::styled(
                format!(" {session_count} session(s) "),
                Style::default().fg(Color::Gray),
            ),
            Span::styled(
                " Ctrl+N: New  Ctrl+X: Close  Ctrl+L: Switch Focus  Ctrl+Q: Quit ",
                Style::default().fg(Color::DarkGray),
            ),
        ])
    };

    frame.render_widget(Paragraph::new(status), area);
}
