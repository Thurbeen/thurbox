use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use crate::app::{StatusLevel, StatusMessage};

const SPINNER_CHARS: &[char] = &['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];

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
        Span::styled(
            concat!("  v", env!("THURBOX_VERSION")),
            Style::default().fg(Color::DarkGray),
        ),
    ]));
    frame.render_widget(header, area);
}

/// State needed to render the footer bar.
pub struct FooterState<'a> {
    pub session_count: usize,
    pub project_count: usize,
    pub status: Option<&'a StatusMessage>,
    pub focus_label: &'a str,
    pub sync_in_progress: bool,
    pub tick_count: u64,
}

pub fn render_footer(frame: &mut Frame, area: Rect, state: &FooterState<'_>) {
    let focus_badge = Span::styled(
        format!(" {} ", state.focus_label),
        Style::default()
            .fg(Color::Black)
            .bg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    );

    let line = if state.sync_in_progress {
        let idx = (state.tick_count as usize / 10) % SPINNER_CHARS.len();
        let spinner = SPINNER_CHARS[idx];
        let text = state
            .status
            .map_or("Syncing...".to_string(), |s| s.text.clone());
        Line::from(vec![
            focus_badge,
            Span::styled(
                format!(" {spinner} SYNC "),
                Style::default()
                    .fg(Color::White)
                    .bg(Color::Blue)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(format!(" {text}"), Style::default().fg(Color::Blue)),
        ])
    } else if let Some(msg) = state.status {
        let (badge_text, badge_bg, text_color) = match msg.level {
            StatusLevel::Info => (" INFO ", Color::Blue, Color::Blue),
            StatusLevel::Success => (" ✓ SYNC ", Color::Green, Color::Green),
            StatusLevel::Error => (" ERROR ", Color::Red, Color::Red),
        };
        Line::from(vec![
            focus_badge,
            Span::styled(badge_text, Style::default().fg(Color::White).bg(badge_bg)),
            Span::styled(format!(" {}", msg.text), Style::default().fg(text_color)),
        ])
    } else {
        let counts = if state.project_count > 0 {
            format!(
                " {} project(s) | {} session(s) ",
                state.project_count, state.session_count
            )
        } else {
            format!(" {} session(s) ", state.session_count)
        };
        Line::from(vec![
            focus_badge,
            Span::styled(counts, Style::default().fg(Color::Gray)),
            Span::styled(
                " ^N New  ^C Close  ^D Delete  ^E Edit  ^R Restart  ^S Sync  ^H/J/K/L Nav  F1 Help  F2 Info  ^Q Quit ",
                Style::default().fg(Color::DarkGray),
            ),
        ])
    };

    frame.render_widget(Paragraph::new(line), area);
}
