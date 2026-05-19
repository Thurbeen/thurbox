use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use super::theme::Theme;
use crate::app::{StatusLevel, StatusMessage};

fn brand_style() -> Style {
    Style::default()
        .fg(Theme::accent())
        .add_modifier(Modifier::BOLD)
}

const SPINNER_CHARS: &[char] = &['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];

pub fn render_header(frame: &mut Frame, area: Rect, badge: Option<HeaderBadge<'_>>) {
    if area.height == 0 {
        return;
    }
    let header = Paragraph::new(Line::from(vec![
        Span::styled(" thurbox", brand_style()),
        Span::styled(
            "  Multi-Session Agent Orchestrator",
            Style::default().fg(Theme::text_secondary()),
        ),
        Span::styled(
            concat!("  v", env!("THURBOX_VERSION")),
            Style::default().fg(Theme::text_muted()),
        ),
    ]));
    frame.render_widget(header, area);

    if let Some(badge) = badge {
        let mut spans: Vec<Span<'_>> = Vec::new();
        if let Some(name) = badge.active_session {
            spans.push(Span::styled(
                name.to_string(),
                Style::default().fg(Theme::text_primary()),
            ));
            spans.push(Span::raw("  "));
        }
        spans.push(Span::styled(
            format!("◐ {} ", badge.theme_label),
            Style::default().fg(Theme::accent()),
        ));
        let right = Line::from(spans).alignment(ratatui::layout::Alignment::Right);
        frame.render_widget(Paragraph::new(right), area);
    }
}

/// Right-aligned overlay rendered on top of the header.
pub struct HeaderBadge<'a> {
    pub active_session: Option<&'a str>,
    pub theme_label: &'a str,
}

/// State needed to render the footer bar.
pub struct FooterState<'a> {
    pub session_count: usize,
    pub status: Option<&'a StatusMessage>,
    pub focus_label: &'a str,
    pub sync_in_progress: bool,
    pub tick_count: u64,
    pub pending_scheduled_count: usize,
    pub file_viewer_open: bool,
}

pub fn render_footer(frame: &mut Frame, area: Rect, state: &FooterState<'_>) {
    let mut spans = vec![Span::styled(
        format!(" {} ", state.focus_label),
        Theme::focused_title(),
    )];
    push_status_section(&mut spans, state);
    push_shortcut_hints(&mut spans);

    frame.render_widget(Paragraph::new(Line::from(spans)), area);

    if state.file_viewer_open {
        let right =
            Line::from(file_viewer_shortcut_spans()).alignment(ratatui::layout::Alignment::Right);
        frame.render_widget(Paragraph::new(right), area);
    }
}

fn push_status_section<'a>(spans: &mut Vec<Span<'a>>, state: &'a FooterState<'a>) {
    if state.sync_in_progress {
        push_spinner_badge(spans, state.tick_count, "SYNC");
        let text = state
            .status
            .map_or("Syncing...".to_string(), |s| s.text.clone());
        spans.push(Span::styled(
            format!(" {text} "),
            Style::default().fg(Theme::accent()),
        ));
    } else if let Some(msg) = state.status {
        push_status_message(spans, msg);
    } else {
        push_idle_counts(spans, state);
    }
}

fn push_status_message<'a>(spans: &mut Vec<Span<'a>>, msg: &'a StatusMessage) {
    let (badge_text, badge_bg, text_color) = match msg.level {
        StatusLevel::Info => (" INFO ", Theme::accent(), Theme::text_secondary()),
        StatusLevel::Success => (" ✓ SYNC ", Theme::status_busy(), Theme::status_busy()),
        StatusLevel::Error => (" ERROR ", Theme::status_error(), Theme::status_error()),
    };
    spans.push(Span::styled(
        badge_text,
        Style::default().fg(Theme::text_primary()).bg(badge_bg),
    ));
    spans.push(Span::styled(
        format!(" {} ", msg.text),
        Style::default().fg(text_color),
    ));
}

fn push_idle_counts<'a>(spans: &mut Vec<Span<'a>>, state: &FooterState<'a>) {
    spans.push(Span::styled(
        format!(" {} session(s) ", state.session_count),
        Style::default().fg(Theme::text_secondary()),
    ));
    if state.pending_scheduled_count > 0 {
        spans.push(Span::styled(
            format!(" {} scheduled ", state.pending_scheduled_count),
            Style::default()
                .fg(Theme::text_primary())
                .bg(Theme::accent()),
        ));
    }
}

fn push_shortcut_hints(spans: &mut Vec<Span<'_>>) {
    let bold_key = Theme::keybind().add_modifier(Modifier::BOLD);
    let desc = Theme::keybind_desc();
    spans.extend([
        Span::styled(" ^H", bold_key),
        Span::styled("/", desc),
        Span::styled("^L", bold_key),
        Span::styled(" Focus  ", desc),
        Span::styled("F1", bold_key),
        Span::styled(" Help  ", desc),
        Span::styled("^Q", Theme::keybind()),
        Span::styled(" Quit ", desc),
    ]);
}

fn file_viewer_shortcut_spans() -> Vec<Span<'static>> {
    let bold_key = Theme::keybind().add_modifier(Modifier::BOLD);
    let desc = Theme::keybind_desc();
    vec![
        Span::styled("j/k", bold_key),
        Span::styled(" Move  ", desc),
        Span::styled("h/l", bold_key),
        Span::styled(" Collapse/Expand  ", desc),
        Span::styled("\u{23CE}", bold_key),
        Span::styled(" Open  ", desc),
        Span::styled("/", bold_key),
        Span::styled(" Search  ", desc),
        Span::styled("n/N", bold_key),
        Span::styled(" Next/Prev ", desc),
    ]
}

fn push_spinner_badge<'a>(spans: &mut Vec<Span<'a>>, tick_count: u64, label: &'a str) {
    let idx = (tick_count as usize / 10) % SPINNER_CHARS.len();
    let spinner = SPINNER_CHARS[idx];
    spans.push(Span::styled(
        format!(" {spinner} {label} "),
        Style::default()
            .fg(Theme::text_primary())
            .bg(Theme::accent()),
    ));
}
