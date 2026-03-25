use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
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
            " Multi-Session Agent Orchestrator",
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
    pub status: Option<&'a StatusMessage>,
    pub focus_label: &'a str,
    pub sync_in_progress: bool,
    pub vm_provisioning: bool,
    pub vm_provisioning_step: &'a str,
    pub container_provisioning: bool,
    pub container_provisioning_step: &'a str,
    pub tick_count: u64,
    pub pending_scheduled_count: usize,
}

pub fn render_footer(frame: &mut Frame, area: Rect, state: &FooterState<'_>) {
    let focus_badge = Span::styled(format!(" {} ", state.focus_label), Theme::focused_title());

    let mut spans = vec![focus_badge];

    if state.vm_provisioning {
        push_spinner_badge(&mut spans, state.tick_count, "VM");
        let step = if state.vm_provisioning_step.is_empty() {
            "Starting VM..."
        } else {
            state.vm_provisioning_step
        };
        spans.push(Span::styled(
            format!(" {step} "),
            Style::default().fg(Theme::ACCENT),
        ));
    } else if state.container_provisioning {
        push_spinner_badge(&mut spans, state.tick_count, "CONTAINER");
        let step = if state.container_provisioning_step.is_empty() {
            "Starting container..."
        } else {
            state.container_provisioning_step
        };
        spans.push(Span::styled(
            format!(" {step} "),
            Style::default().fg(Theme::ACCENT),
        ));
    } else if state.sync_in_progress {
        push_spinner_badge(&mut spans, state.tick_count, "SYNC");
        let text = state
            .status
            .map_or("Syncing...".to_string(), |s| s.text.clone());
        spans.push(Span::styled(
            format!(" {text} "),
            Style::default().fg(Theme::ACCENT),
        ));
    } else if let Some(msg) = state.status {
        let (badge_text, badge_bg, text_color) = match msg.level {
            StatusLevel::Info => (" INFO ", Theme::ACCENT, Theme::TEXT_SECONDARY),
            StatusLevel::Success => (" ✓ SYNC ", Theme::STATUS_BUSY, Theme::STATUS_BUSY),
            StatusLevel::Error => (" ERROR ", Theme::STATUS_ERROR, Theme::STATUS_ERROR),
        };
        spans.push(Span::styled(
            badge_text,
            Style::default().fg(Theme::TEXT_PRIMARY).bg(badge_bg),
        ));
        spans.push(Span::styled(
            format!(" {} ", msg.text),
            Style::default().fg(text_color),
        ));
    } else {
        let counts = format!(" {} session(s) ", state.session_count);
        spans.push(Span::styled(
            counts,
            Style::default().fg(Theme::TEXT_SECONDARY),
        ));
        if state.pending_scheduled_count > 0 {
            spans.push(Span::styled(
                format!(" {} scheduled ", state.pending_scheduled_count),
                Style::default().fg(Theme::TEXT_PRIMARY).bg(Theme::ACCENT),
            ));
        }
    }

    // Always-visible shortcut hints
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

    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn push_spinner_badge<'a>(spans: &mut Vec<Span<'a>>, tick_count: u64, label: &'a str) {
    let idx = (tick_count as usize / 10) % SPINNER_CHARS.len();
    let spinner = SPINNER_CHARS[idx];
    spans.push(Span::styled(
        format!(" {spinner} {label} "),
        Style::default().fg(Theme::TEXT_PRIMARY).bg(Theme::ACCENT),
    ));
}
