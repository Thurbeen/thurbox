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

pub fn render_footer(
    frame: &mut Frame,
    area: Rect,
    session_count: usize,
    project_count: usize,
    error: Option<&str>,
    focus_label: &str,
) {
    let focus_badge = Span::styled(
        format!(" {focus_label} "),
        Style::default()
            .fg(Color::Black)
            .bg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    );

    let status = if let Some(err) = error {
        Line::from(vec![
            focus_badge,
            Span::styled(" ERROR ", Style::default().fg(Color::White).bg(Color::Red)),
            Span::styled(format!(" {err}"), Style::default().fg(Color::Red)),
        ])
    } else {
        let counts = if project_count > 0 {
            format!(" {project_count} project(s) | {session_count} session(s) ")
        } else {
            format!(" {session_count} session(s) ")
        };
        Line::from(vec![
            focus_badge,
            Span::styled(counts, Style::default().fg(Color::Gray)),
            Span::styled(
                " Ctrl+N: New  Ctrl+X: Close  Ctrl+J/K: Switch  Ctrl+L: Focus  Ctrl+Q: Quit ",
                Style::default().fg(Color::DarkGray),
            ),
        ])
    };

    frame.render_widget(Paragraph::new(status), area);
}
