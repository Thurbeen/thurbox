use ratatui::{
    layout::Rect,
    style::Style,
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use super::theme::Theme;
use crate::app::{StatusLevel, StatusMessage};

const SPINNER_CHARS: &[char] = &['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];

pub fn render_header(frame: &mut Frame, area: Rect) {
    let header = Paragraph::new(Line::from(vec![
        Span::styled(" thurbox ", Theme::focused_title()),
        Span::styled(
            " Multi-Session Claude Code Orchestrator",
            Style::default().fg(Theme::TEXT_SECONDARY),
        ),
        Span::styled(
            concat!("  v", env!("THURBOX_VERSION")),
            Style::default().fg(Theme::TEXT_MUTED),
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
    let focus_badge = Span::styled(format!(" {} ", state.focus_label), Theme::focused_title());

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
                Style::default().fg(Theme::TEXT_PRIMARY).bg(Theme::ACCENT),
            ),
            Span::styled(format!(" {text}"), Style::default().fg(Theme::ACCENT)),
        ])
    } else if let Some(msg) = state.status {
        let (badge_text, badge_bg, text_color) = match msg.level {
            StatusLevel::Info => (" INFO ", Theme::ACCENT, Theme::TEXT_SECONDARY),
            StatusLevel::Success => (" ✓ SYNC ", Theme::STATUS_BUSY, Theme::STATUS_BUSY),
            StatusLevel::Error => (" ERROR ", Theme::STATUS_ERROR, Theme::STATUS_ERROR),
        };
        Line::from(vec![
            focus_badge,
            Span::styled(
                badge_text,
                Style::default().fg(Theme::TEXT_PRIMARY).bg(badge_bg),
            ),
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
            Span::styled(counts, Style::default().fg(Theme::TEXT_SECONDARY)),
            Span::styled(
                " ^N New  ^C Close  ^D Delete  ^E Edit  ^R Restart  ^S Sync  ^H/J/K/L Nav  F1 Help  F2 Info  ^Q Quit ",
                Style::default().fg(Theme::TEXT_MUTED),
            ),
        ])
    };

    frame.render_widget(Paragraph::new(line), area);
}
