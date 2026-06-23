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
    let mut spans = vec![
        Span::styled(" thurbox", brand_style()),
        Span::styled(
            "  Multi-Session Agent Orchestrator",
            Style::default().fg(Theme::text_secondary()),
        ),
        Span::styled(
            concat!("  v", env!("THURBOX_VERSION")),
            Style::default().fg(Theme::text_muted()),
        ),
    ];
    if let Some(latest) = badge.as_ref().and_then(|b| b.update_latest) {
        spans.push(Span::styled(
            format!("  ⬆ v{latest} available"),
            Style::default()
                .fg(Theme::accent())
                .add_modifier(Modifier::BOLD),
        ));
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), area);

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

/// Right-aligned overlay rendered on top of the header, plus the optional
/// left-aligned "update available" version badge.
pub struct HeaderBadge<'a> {
    pub active_session: Option<&'a str>,
    pub theme_label: &'a str,
    /// When `Some`, the latest available release version (no leading `v`),
    /// rendered as a left-aligned "⬆ vX.Y.Z available" badge after the version.
    pub update_latest: Option<&'a str>,
}

/// State needed to render the footer bar.
pub struct FooterState<'a> {
    pub session_count: usize,
    pub status: Option<&'a StatusMessage>,
    pub focus_label: &'a str,
    pub sync_in_progress: bool,
    pub tick_count: u64,
    pub automation_count: usize,
    pub file_viewer_open: bool,
}

/// The clickable footer buttons, in render order. `view.rs` maps each returned
/// [`ButtonHit`](super::ButtonHit)'s index back to the matching `Action`.
pub const FOOTER_BUTTONS: [super::ButtonSpec<'static>; 4] = [
    super::ButtonSpec {
        label: "Help",
        primary: false,
    },
    super::ButtonSpec {
        label: "Settings",
        primary: false,
    },
    super::ButtonSpec {
        label: "Theme",
        primary: false,
    },
    super::ButtonSpec {
        label: "Quit",
        primary: false,
    },
];

/// Render the footer bar and return the clickable button hitboxes (Help /
/// Settings / Theme / Quit), packed against the right edge. The buttons are
/// **always** drawn; when the file viewer is open its navigation hints fill
/// the space to the *left* of the buttons (right-aligned there) so both stay
/// visible.
pub fn render_footer(
    frame: &mut Frame,
    area: Rect,
    state: &FooterState<'_>,
) -> Vec<super::ButtonHit> {
    let mut spans = vec![Span::styled(
        format!(" {} ", state.focus_label),
        Theme::focused_title(),
    )];
    push_status_section(&mut spans, state);
    push_shortcut_hints(&mut spans);

    frame.render_widget(Paragraph::new(Line::from(spans)), area);

    // Buttons always sit on the far right.
    let hits = super::render_button_bar(frame, area, &FOOTER_BUTTONS, true);

    // File-viewer hints fill whatever room is left of the buttons.
    if state.file_viewer_open {
        let buttons_left = hits
            .iter()
            .map(|h| h.rect.x)
            .min()
            .unwrap_or(area.x + area.width);
        let avail = buttons_left.saturating_sub(area.x).saturating_sub(1);
        if avail > 0 {
            let hint_area = Rect {
                width: avail,
                ..area
            };
            let right = Line::from(file_viewer_shortcut_spans())
                .alignment(ratatui::layout::Alignment::Right);
            frame.render_widget(Paragraph::new(right), hint_area);
        }
    }

    hits
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
        StatusLevel::Success => (" ✓ SYNC ", Theme::tool_allowed(), Theme::tool_allowed()),
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
    if state.automation_count > 0 {
        spans.push(Span::styled(
            format!(" {} automation(s) ", state.automation_count),
            Style::default()
                .fg(Theme::text_primary())
                .bg(Theme::accent()),
        ));
    }
}

fn push_shortcut_hints(spans: &mut Vec<Span<'_>>) {
    // Focus stays an informational hint (no single click target); Help /
    // Settings / Theme / Quit are rendered as right-aligned clickable buttons.
    let bold_key = Theme::keybind().add_modifier(Modifier::BOLD);
    let desc = Theme::keybind_desc();
    spans.extend([
        Span::styled(" ^H", bold_key),
        Span::styled("/", desc),
        Span::styled("^L", bold_key),
        Span::styled(" Focus ", desc),
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

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{backend::TestBackend, Terminal};

    /// Render the header into a 120×1 buffer (a realistic three-panel width,
    /// wide enough that the right-aligned theme overlay doesn't paint over the
    /// left badge) and return its single line.
    fn header_line(update_latest: Option<&str>) -> String {
        let backend = TestBackend::new(120, 1);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| {
                render_header(
                    f,
                    Rect::new(0, 0, 120, 1),
                    Some(HeaderBadge {
                        active_session: None,
                        theme_label: "Default",
                        update_latest,
                    }),
                );
            })
            .unwrap();
        let buffer = terminal.backend().buffer();
        (0..buffer.area.width)
            .map(|x| buffer[(x, 0)].symbol())
            .collect()
    }

    #[test]
    fn update_badge_renders_when_a_newer_release_is_available() {
        let line = header_line(Some("0.114.0"));
        assert!(
            line.contains("⬆ v0.114.0 available"),
            "badge missing from header: {line:?}"
        );
    }

    #[test]
    fn no_update_badge_without_a_newer_release() {
        let line = header_line(None);
        assert!(
            !line.contains("available"),
            "unexpected badge in header: {line:?}"
        );
    }

    fn footer_state(file_viewer_open: bool) -> FooterState<'static> {
        FooterState {
            session_count: 1,
            status: None,
            focus_label: "Files",
            sync_in_progress: false,
            tick_count: 0,
            automation_count: 0,
            file_viewer_open,
        }
    }

    /// Render the footer into a 120×1 buffer and return (button hits, line text).
    fn footer_render(file_viewer_open: bool) -> (Vec<super::super::ButtonHit>, String) {
        let backend = TestBackend::new(120, 1);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut hits = Vec::new();
        terminal
            .draw(|f| {
                hits = render_footer(f, Rect::new(0, 0, 120, 1), &footer_state(file_viewer_open));
            })
            .unwrap();
        let buffer = terminal.backend().buffer();
        let line: String = (0..buffer.area.width)
            .map(|x| buffer[(x, 0)].symbol())
            .collect();
        (hits, line)
    }

    #[test]
    fn footer_renders_four_buttons() {
        let (hits, line) = footer_render(false);
        assert_eq!(hits.len(), 4, "Help/Settings/Theme/Quit");
        for label in ["Help", "Settings", "Theme", "Quit"] {
            assert!(line.contains(label), "missing {label} in footer: {line:?}");
        }
    }

    /// The buttons stay visible with the file viewer open; its hints share the
    /// row to the left of them.
    #[test]
    fn footer_keeps_buttons_with_file_viewer_open() {
        let (hits, line) = footer_render(true);
        assert_eq!(
            hits.len(),
            4,
            "buttons must remain when the file viewer is open"
        );
        assert!(line.contains("Quit"), "buttons still rendered: {line:?}");
        assert!(
            line.contains("Open") || line.contains("Move"),
            "file-viewer hints share the row: {line:?}"
        );
        // The rightmost button ends at the footer's right edge.
        let last = hits.iter().max_by_key(|h| h.rect.x).unwrap().rect;
        assert_eq!(last.x + last.width, 120);
    }
}
