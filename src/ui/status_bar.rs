use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use super::theme::Theme;
use crate::app::{StatusLevel, StatusMessage};
use crate::session::{Action, KeyBindings};

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
    /// Feature flags gating the panel buttons (hidden when off).
    pub tasks_enabled: bool,
    pub file_viewer_enabled: bool,
    pub shell_pane_enabled: bool,
    pub code_review_enabled: bool,
    /// Live keybindings, so each footer pill can show its (rebindable) shortcut.
    pub keybindings: &'a KeyBindings,
}

/// The clickable footer buttons, in render order, paired with the `Action` each
/// dispatches. `render_footer` filters this by feature flags and returns the
/// surviving `(ButtonHit, Action)` pairs so the click map can't drift from the
/// render.
const FOOTER_BUTTONS: &[(&str, Action)] = &[
    ("Help", Action::ToggleHelp),
    ("Tasks", Action::FocusTasks),
    ("Files", Action::ToggleFileViewer),
    ("Shell", Action::ToggleShell),
    ("Review", Action::ToggleReview),
    ("Settings", Action::OpenSettings),
    ("Theme", Action::OpenThemePicker),
    ("Quit", Action::QuitApp),
];

/// The footer buttons that survive the current feature flags, in render order.
/// The panel/pane toggles (Tasks/Files/Shell/Review) are dropped when their
/// feature is off (clicking a button that just toasts "disabled" would be
/// noise).
fn footer_entries(flags: &FooterState<'_>) -> Vec<(&'static str, Action)> {
    FOOTER_BUTTONS
        .iter()
        .copied()
        .filter(|(_, action)| match action {
            Action::FocusTasks => flags.tasks_enabled,
            Action::ToggleFileViewer => flags.file_viewer_enabled,
            Action::ToggleShell => flags.shell_pane_enabled,
            Action::ToggleReview => flags.code_review_enabled,
            _ => true,
        })
        .collect()
}

/// Total width of the footer pill block: each pill is ` label ` (the label plus
/// the two padding spaces) and pills are joined by a single-space separator.
/// Mirrors `render_button_bar`'s packing so the responsive trim below agrees
/// with what actually renders.
fn pill_block_width(labels: &[String]) -> u16 {
    if labels.is_empty() {
        return 0;
    }
    let pills: u16 = labels.iter().map(|l| l.chars().count() as u16 + 2).sum();
    pills + labels.len() as u16 - 1
}

/// Render the footer bar and return each clickable button's hitbox paired with
/// the `Action` it dispatches, packed against the right edge. Tasks/Files are
/// gated by their feature flags. The view-toggle pills (Shell/Review) are
/// *optional*: when the full set would overflow the footer they're dropped
/// first, so the essential Settings/Theme/Quit pills never fall off a narrow
/// terminal. When the file viewer is open its navigation hints fill the space
/// to the *left* of the buttons (right-aligned there) so both stay visible.
pub fn render_footer(
    frame: &mut Frame,
    area: Rect,
    state: &FooterState<'_>,
) -> Vec<(super::ButtonHit, Action)> {
    let mut spans = vec![Span::styled(
        format!(" {} ", state.focus_label),
        Theme::focused_title(),
    )];
    push_status_section(&mut spans, state);
    push_shortcut_hints(&mut spans);

    frame.render_widget(Paragraph::new(Line::from(spans)), area);

    let mut entries = footer_entries(state);
    // Suffix each pill with its live shortcut (`Help · F1`); own the strings so
    // the borrowed `ButtonSpec`s can reference them. Kept index-aligned with
    // `entries` through the responsive trim below.
    let mut labels: Vec<String> = entries
        .iter()
        .map(|(label, action)| {
            match crate::session::compact_shortcut(state.keybindings.chords_for(*action)) {
                Some(sc) => format!("{label} · {sc}"),
                None => label.to_string(),
            }
        })
        .collect();
    // When the full set won't fit, drop *both* optional view-toggle pills
    // together (so it's all-or-nothing, never a lone Shell or Review) rather
    // than letting `render_button_bar` shed the rightmost essential pill.
    if pill_block_width(&labels) > area.width {
        let optional = |a: &Action| matches!(a, Action::ToggleShell | Action::ToggleReview);
        labels = entries
            .iter()
            .zip(&labels)
            .filter(|((_, a), _)| !optional(a))
            .map(|(_, l)| l.clone())
            .collect();
        entries.retain(|(_, a)| !optional(a));
    }
    let specs: Vec<super::ButtonSpec<'_>> = labels
        .iter()
        .map(|l| super::ButtonSpec::secondary(l))
        .collect();
    let hits = super::render_button_bar(frame, area, &specs, true);

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

    // Each placed hit keeps its index into `entries`, so the click→action map
    // follows the same feature-filtered list that was rendered.
    hits.into_iter()
        .map(|hit| {
            let action = entries[hit.index].1;
            (hit, action)
        })
        .collect()
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
    // Focus stays an informational hint (no single click target); the footer
    // buttons (Help / Tasks / Files / Settings / Theme / Quit) are rendered as
    // right-aligned clickable pills.
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

    fn test_keybindings() -> &'static KeyBindings {
        static KB: std::sync::OnceLock<KeyBindings> = std::sync::OnceLock::new();
        KB.get_or_init(KeyBindings::default)
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
            tasks_enabled: true,
            file_viewer_enabled: true,
            shell_pane_enabled: true,
            code_review_enabled: true,
            keybindings: test_keybindings(),
        }
    }

    /// Render the given state into a 120×1 buffer and return (button+action
    /// hits, line text).
    fn render_footer_state(
        state: &FooterState<'_>,
    ) -> (Vec<(super::super::ButtonHit, Action)>, String) {
        let backend = TestBackend::new(120, 1);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut hits = Vec::new();
        terminal
            .draw(|f| hits = render_footer(f, Rect::new(0, 0, 120, 1), state))
            .unwrap();
        let buffer = terminal.backend().buffer();
        let line: String = (0..buffer.area.width)
            .map(|x| buffer[(x, 0)].symbol())
            .collect();
        (hits, line)
    }

    /// Render a default footer (all features on) with the given file-viewer
    /// visibility.
    fn footer_render(file_viewer_open: bool) -> (Vec<(super::super::ButtonHit, Action)>, String) {
        render_footer_state(&footer_state(file_viewer_open))
    }

    fn hit_actions(hits: &[(super::super::ButtonHit, Action)]) -> Vec<Action> {
        hits.iter().map(|(_, a)| *a).collect()
    }

    #[test]
    fn footer_renders_all_buttons() {
        let (hits, line) = footer_render(false);
        for label in [
            "Help", "Tasks", "Files", "Shell", "Review", "Settings", "Theme", "Quit",
        ] {
            assert!(line.contains(label), "missing {label} in footer: {line:?}");
        }
        // Each pill carries its live shortcut: an F-key alternate where one
        // exists (Help · F1, Review · F7), else the caret-ctrl chord (Quit · ^Q).
        for shortcut in ["F1", "F7", "F8", "^Q"] {
            assert!(
                line.contains(shortcut),
                "missing shortcut {shortcut} in footer: {line:?}"
            );
        }
        // Every button maps back to its Action, in render order — guards the
        // index→action remapping in `render_footer`.
        assert_eq!(
            hit_actions(&hits),
            vec![
                Action::ToggleHelp,
                Action::FocusTasks,
                Action::ToggleFileViewer,
                Action::ToggleShell,
                Action::ToggleReview,
                Action::OpenSettings,
                Action::OpenThemePicker,
                Action::QuitApp,
            ]
        );
    }

    /// The panel/pane-toggle buttons are dropped when their feature is off.
    #[test]
    fn footer_hides_panel_buttons_when_features_off() {
        let mut state = footer_state(false);
        state.tasks_enabled = false;
        state.file_viewer_enabled = false;
        state.shell_pane_enabled = false;
        state.code_review_enabled = false;
        let (hits, _) = render_footer_state(&state);
        assert_eq!(
            hit_actions(&hits),
            vec![
                Action::ToggleHelp,
                Action::OpenSettings,
                Action::OpenThemePicker,
                Action::QuitApp,
            ],
            "only the non-panel buttons remain"
        );
    }

    /// On a narrow footer the optional view-toggle pills (Shell/Review) drop as
    /// a pair, but the essential Settings/Theme/Quit pills always survive.
    #[test]
    fn footer_drops_view_toggles_when_narrow() {
        let state = footer_state(false);
        let backend = TestBackend::new(100, 1);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut hits = Vec::new();
        terminal
            .draw(|f| hits = render_footer(f, Rect::new(0, 0, 100, 1), &state))
            .unwrap();
        let actions = hit_actions(&hits);
        assert!(
            !actions.contains(&Action::ToggleShell) && !actions.contains(&Action::ToggleReview),
            "view-toggle pills drop together when the footer is too narrow: {actions:?}"
        );
        for essential in [
            Action::OpenSettings,
            Action::OpenThemePicker,
            Action::QuitApp,
        ] {
            assert!(
                actions.contains(&essential),
                "essential pill {essential:?} must survive a narrow footer: {actions:?}"
            );
        }
    }

    /// The two panel buttons gate independently — a swapped flag in
    /// `footer_entries` would surface here.
    #[test]
    fn footer_panel_buttons_gate_independently() {
        let mut state = footer_state(false);
        state.tasks_enabled = false;
        state.file_viewer_enabled = true;
        let actions = hit_actions(&render_footer_state(&state).0);
        assert!(!actions.contains(&Action::FocusTasks), "Tasks gated off");
        assert!(
            actions.contains(&Action::ToggleFileViewer),
            "Files still shown"
        );
    }

    /// The buttons stay visible with the file viewer open; its hints share the
    /// row to the left of them.
    #[test]
    fn footer_keeps_buttons_with_file_viewer_open() {
        let (hits, line) = footer_render(true);
        assert_eq!(
            hits.len(),
            8,
            "buttons must remain when the file viewer is open"
        );
        assert!(line.contains("Quit"), "buttons still rendered: {line:?}");
        assert!(
            line.contains("Open") || line.contains("Move"),
            "file-viewer hints share the row: {line:?}"
        );
        // The rightmost button ends at the footer's right edge.
        let last = hits.iter().max_by_key(|(h, _)| h.rect.x).unwrap().0.rect;
        assert_eq!(last.x + last.width, 120);
    }
}
