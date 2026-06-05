pub mod agent_picker_modal;
pub mod automation_detail;
pub mod automation_editor_modal;
pub mod automations_list_modal;
pub mod automations_panel;
pub mod branch_selector_modal;
pub mod file_viewer;
pub mod global_search;
pub mod highlight;
pub mod info_panel;
pub mod layout;
pub mod links;
pub mod markdown;
pub mod project_list;
pub mod repo_picker_modal;
pub mod restore_sessions_modal;
pub mod selection;
pub mod session_name_modal;
pub mod status_bar;
pub mod task_action_picker_modal;
pub mod task_detail;
pub mod task_editor_modal;
pub mod tasks_panel;
pub mod terminal_view;
pub mod theme;
pub mod theme_picker_modal;
pub mod worktree_name_modal;

use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Clear, Paragraph},
    Frame,
};

use crate::session::SessionStatus;
use theme::Theme;

pub fn status_color(status: SessionStatus) -> Color {
    match status {
        SessionStatus::Busy => Theme::status_busy(),
        SessionStatus::Waiting => Theme::status_waiting(),
        SessionStatus::Idle => Theme::status_idle(),
        SessionStatus::Error => Theme::status_error(),
        SessionStatus::Attention => Theme::accent_bright(),
    }
}

/// Truncate `s` to at most `max` display columns, appending `…` when cut.
///
/// Counts by `char` (not bytes), reserving one column for the ellipsis.
/// Returns an empty string when `max` is too small to show anything useful
/// (`max <= 1`), since a lone `…` carries no information.
pub fn truncate_ellipsis(s: &str, max: usize) -> String {
    let count = s.chars().count();
    if count <= max {
        return s.to_string();
    }
    if max <= 1 {
        return String::new();
    }
    let kept: String = s.chars().take(max - 1).collect();
    format!("{kept}…")
}

/// Render a titled, bordered editor frame and return its inner area. Uses the
/// shared [`focus_block`] chrome so a focused editor is highlighted exactly like
/// the session list / tasks panel (bright accent border + highlighted title
/// badge); unfocused it reads as a muted preview. Shared by the automation and
/// task in-pane editors so their chrome stays identical.
pub fn render_editor_frame(frame: &mut Frame, area: Rect, title: &str, focused: bool) -> Rect {
    let level = if focused {
        FocusLevel::Focused
    } else {
        FocusLevel::Inactive
    };
    let title = format!(" {title} ");
    let block = focus_block(&title, level);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    inner
}

/// Render one editor field row: a left-aligned `label`, then its `value`. When
/// `active` the row is prefixed with `▸` and bolded; `selector` values (adjusted
/// with ←/→) are wrapped in `‹ ›`, while an active text value gets a block
/// cursor. Shared by the automation and task editor field renderers.
///
/// This convenience form draws the cursor at the end of the value; use
/// [`editor_field_line_with_cursor`] to place it at a specific caret position.
pub fn editor_field_line<'a>(label: &str, value: String, selector: bool, active: bool) -> Line<'a> {
    editor_field_line_with_cursor(label, value, selector, active, None)
}

/// Cursor-aware variant of [`editor_field_line`]. See its docs for `cursor`.
pub fn editor_field_line_with_cursor<'a>(
    label: &str,
    value: String,
    selector: bool,
    active: bool,
    cursor: Option<usize>,
) -> Line<'a> {
    let prefix = if active { "▸ " } else { "  " };
    let value_style = if active {
        Style::default()
            .fg(Theme::border_focused())
            .add_modifier(ratatui::style::Modifier::BOLD)
    } else {
        Style::default().fg(Theme::text_primary())
    };

    let mut spans = vec![Span::styled(format!("{prefix}{label:<9}"), Theme::label())];

    if selector {
        spans.push(Span::styled(format!("‹ {value} ›"), value_style));
    } else if active {
        // Draw a real block cursor at the caret position so horizontal movement
        // inside the text is visible. Fall back to a trailing block when no
        // cursor is supplied (preserves the prior end-of-line affordance).
        let chars: Vec<char> = value.chars().collect();
        let caret = cursor.unwrap_or(chars.len()).min(chars.len());

        let before: String = chars[..caret].iter().collect();
        if !before.is_empty() {
            spans.push(Span::styled(before, value_style));
        }
        let cursor_char = chars
            .get(caret)
            .map(|c| c.to_string())
            .unwrap_or_else(|| " ".to_string());
        spans.push(Span::styled(cursor_char, Theme::cursor()));
        if caret < chars.len() {
            let after: String = chars[caret + 1..].iter().collect();
            if !after.is_empty() {
                spans.push(Span::styled(after, value_style));
            }
        }
    } else {
        spans.push(Span::styled(value, value_style));
    }

    Line::from(spans)
}

/// Build a footer/hint [`Line`] from `(key, description)` pairs, styling keys
/// with [`Theme::keybind`] and descriptions with [`Theme::keybind_desc`]. Shared
/// by the editor footers so the keybind chrome reads identically everywhere.
pub fn key_hint_line<'a>(pairs: &[(&'a str, &'a str)]) -> Line<'a> {
    let mut spans = Vec::with_capacity(pairs.len() * 2);
    for (key, desc) in pairs {
        spans.push(Span::styled(*key, Theme::keybind()));
        spans.push(Span::styled(*desc, Theme::keybind_desc()));
    }
    Line::from(spans)
}

/// Tri-state focus level for panels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FocusLevel {
    /// Receiving input: thick accent border + badge title.
    Focused,
    /// Contextually relevant: plain accent border + accent title text.
    Active,
    /// Background: plain dark-gray border + dark-gray title.
    Inactive,
}

/// Build a [`Block`] with tri-state focus styling.
///
/// Focus is communicated by colour (bright accent vs plain accent vs gray)
/// rather than border weight — every level uses rounded borders for a
/// softer, opencode-style chrome.
pub fn focus_block(title_text: &str, level: FocusLevel) -> Block<'_> {
    match level {
        FocusLevel::Focused => Block::default()
            .title(Line::from(Span::styled(title_text, Theme::focused_title())))
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(Theme::accent_bright())),
        FocusLevel::Active => Block::default()
            .title(Line::from(Span::styled(
                title_text,
                Style::default().fg(Theme::accent()),
            )))
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(Theme::accent())),
        FocusLevel::Inactive => Block::default()
            .title(Line::from(Span::styled(
                title_text,
                Theme::unfocused_title(),
            )))
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(Theme::border_unfocused())),
    }
}

/// Build a [`Block`] with focused or unfocused styling (backward compat).
///
/// Focused: thick borders in accent color with a highlighted title badge.
/// Unfocused: plain borders in gray with a dimmed title.
pub fn focused_block(title_text: &str, focused: bool) -> Block<'_> {
    focus_block(
        title_text,
        if focused {
            FocusLevel::Focused
        } else {
            FocusLevel::Inactive
        },
    )
}

/// Create a centered rectangle with a fixed width percentage and a fixed height in lines.
pub fn centered_fixed_height_rect(percent_x: u16, height: u16, area: Rect) -> Rect {
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(0),
            Constraint::Length(height),
            Constraint::Min(0),
        ])
        .split(area);

    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(vertical[1])[1]
}

/// Render a full-screen dim overlay to visually separate a modal from the background.
pub fn render_dim_overlay(frame: &mut Frame) {
    let dim = Block::default().style(Style::default().bg(Theme::modal_dim_bg()));
    frame.render_widget(dim, frame.area());
}

/// Build a modal [`Block`] with the given title style and border color.
fn build_modal_block(title: &str, title_style: Style, border_color: Color) -> Block<'_> {
    Block::default()
        .title(Line::from(Span::styled(format!(" {title} "), title_style)))
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(border_color))
        .style(Style::default().bg(Theme::modal_bg()))
}

/// Build a styled modal [`Block`] with rounded borders and an explicit background.
pub fn modal_block(title: &str) -> Block<'_> {
    build_modal_block(title, Theme::modal_title(), Theme::modal_border())
}

/// Build a danger-styled modal [`Block`] with red borders and background.
pub fn modal_block_danger(title: &str) -> Block<'_> {
    build_modal_block(title, Theme::modal_title_danger(), Theme::danger())
}

/// Dim the background, clear the modal region, render a styled block, and return the inner area.
pub fn render_modal_frame(frame: &mut Frame, area: Rect, title: &str) -> Rect {
    render_dim_overlay(frame);
    frame.render_widget(Clear, area);
    let block = modal_block(title);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    inner
}

/// Dim the background, clear the modal region, render a danger-styled block, and return the inner area.
pub fn render_modal_frame_danger(frame: &mut Frame, area: Rect, title: &str) -> Rect {
    render_dim_overlay(frame);
    frame.render_widget(Clear, area);
    let block = modal_block_danger(title);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    inner
}

/// Set up a list modal with dim overlay, styled border, and list + footer split.
///
/// If `entry_count` is 0 and `empty_message` is `Some`, renders the empty state
/// with the given message and footer keybinds, then returns `None`.
/// Otherwise returns `Some([list_area, footer_area])`.
pub fn render_list_modal_frame<'a>(
    frame: &mut Frame,
    percent_width: u16,
    title: &str,
    entry_count: usize,
    empty_message: Option<&str>,
    empty_footer: Option<Line<'a>>,
) -> Option<[Rect; 2]> {
    let list_height = entry_count.max(1) as u16;
    let total_height = (list_height + 5).min(20);
    let area = centered_fixed_height_rect(percent_width, total_height, frame.area());
    let inner = render_modal_frame(frame, area, title);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(1)])
        .split(inner);

    if entry_count == 0 {
        if let Some(msg) = empty_message {
            let empty = Paragraph::new(Line::from(Span::styled(
                msg,
                Style::default().fg(Theme::text_muted()),
            )))
            .alignment(ratatui::layout::Alignment::Center);
            frame.render_widget(empty, chunks[0]);

            if let Some(footer) = empty_footer {
                frame.render_widget(Paragraph::new(footer), chunks[1]);
            }
        }
        return None;
    }

    Some([chunks[0], chunks[1]])
}

/// Standard "j/k navigate · Enter select · Esc cancel" footer used by selector
/// modals.
pub fn selector_nav_footer() -> Line<'static> {
    Line::from(vec![
        Span::styled("j/k", Theme::keybind()),
        Span::styled(" navigate  ", Theme::keybind_desc()),
        Span::styled("Enter", Theme::keybind()),
        Span::styled(" select  ", Theme::keybind_desc()),
        Span::styled("Esc", Theme::keybind()),
        Span::styled(" cancel", Theme::keybind_desc()),
    ])
}

/// Build a selector list item with the standard "▸ " selected prefix and
/// selected/normal theme styles.
pub fn selector_list_item<'a>(label: &str, selected: bool) -> ratatui::widgets::ListItem<'a> {
    let style = if selected {
        Theme::selected_item()
    } else {
        Theme::normal_item()
    };
    let prefix = if selected { "▸ " } else { "  " };
    ratatui::widgets::ListItem::new(Line::from(Span::styled(format!("{prefix}{label}"), style)))
}

/// Render a labeled text input field with cursor visualization and horizontal
/// viewport scrolling.
///
/// When `focused` is true, a block cursor is shown at the current position.
/// If the text exceeds the visible width, the viewport scrolls to keep the
/// cursor visible and overflow indicators (`◀` / `▶`) are shown at the edges.
/// When unfocused, the value is displayed as plain text with a dimmed border.
pub fn render_text_field(
    frame: &mut Frame,
    area: Rect,
    label: &str,
    value: &str,
    cursor: usize,
    focused: bool,
) {
    render_text_field_with_suggestion(frame, area, label, value, cursor, focused, None);
}

/// Render a text field with an optional inline suggestion (fish-style).
///
/// When `focused`, cursor at end, and `suggestion` is `Some`, the suggestion
/// text is rendered in dark gray after the cursor block. Pass `None` for a
/// plain text field (identical to [`render_text_field`]).
pub fn render_text_field_with_suggestion(
    frame: &mut Frame,
    area: Rect,
    label: &str,
    value: &str,
    cursor: usize,
    focused: bool,
    suggestion: Option<&str>,
) {
    let border_color = if focused {
        Theme::border_focused()
    } else {
        Theme::border_unfocused()
    };

    let block = Block::default()
        .title(format!(" {label} "))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(border_color));

    let inner = block.inner(area);
    let width = inner.width as usize;

    let chars: Vec<char> = value.chars().collect();
    let cursor = cursor.min(chars.len());

    let display = if focused && width > 0 {
        let suggestion_text = if cursor == chars.len() {
            suggestion.unwrap_or("")
        } else {
            ""
        };
        render_focused_field_line(&chars, cursor, width, suggestion_text)
    } else if width > 0 {
        render_unfocused_field_line(value, &chars, width)
    } else {
        Line::from("")
    };

    frame.render_widget(block, area);
    frame.render_widget(Paragraph::new(display), inner);
}

/// Computed scroll viewport for a focused text field.
struct Viewport {
    /// First character index of the scrolled-in viewport (before overflow trim).
    start: usize,
    has_left_overflow: bool,
    has_right_overflow: bool,
}

/// Compute the scroll viewport (and overflow indicators) for a focused field.
fn compute_viewport(chars_len: usize, width: usize, cursor: usize) -> Viewport {
    if chars_len < width {
        return Viewport {
            start: 0,
            has_left_overflow: false,
            has_right_overflow: false,
        };
    }
    let usable = width.saturating_sub(1);
    let start = if cursor < usable {
        0
    } else {
        cursor - usable + 1
    };
    Viewport {
        start,
        has_left_overflow: start > 0,
        has_right_overflow: start + width < chars_len + 1,
    }
}

/// Build the rendered line for a focused text field (with cursor block and
/// optional inline suggestion / overflow indicators).
fn render_focused_field_line(
    chars: &[char],
    cursor: usize,
    width: usize,
    suggestion_text: &str,
) -> Line<'static> {
    let vp = compute_viewport(chars.len(), width, cursor);

    let content_start = if vp.has_left_overflow {
        vp.start + 1
    } else {
        vp.start
    };
    let content_width = width
        - if vp.has_left_overflow { 1 } else { 0 }
        - if vp.has_right_overflow { 1 } else { 0 };

    let mut spans = Vec::new();

    if vp.has_left_overflow {
        spans.push(Span::styled("◀", Style::default().fg(Theme::text_muted())));
    }

    let visible_end = (content_start + content_width).min(chars.len());

    if cursor >= content_start && cursor <= visible_end {
        push_cursor_spans(
            &mut spans,
            chars,
            content_start,
            cursor,
            visible_end,
            content_width,
            vp.has_left_overflow,
            suggestion_text,
        );
    } else {
        let visible: String = chars[content_start..visible_end].iter().collect();
        spans.push(Span::styled(
            visible,
            Style::default().fg(Theme::text_primary()),
        ));
    }

    if vp.has_right_overflow {
        spans.push(Span::styled("▶", Style::default().fg(Theme::text_muted())));
    }

    Line::from(spans)
}

/// Push the before/cursor/after text spans and an optional trailing suggestion
/// for the segment of the field that contains the cursor.
#[allow(clippy::too_many_arguments)]
fn push_cursor_spans(
    spans: &mut Vec<Span<'static>>,
    chars: &[char],
    content_start: usize,
    cursor: usize,
    visible_end: usize,
    content_width: usize,
    has_left_overflow: bool,
    suggestion_text: &str,
) {
    let before: String = chars[content_start..cursor].iter().collect();
    let cursor_char = if cursor < chars.len() {
        chars[cursor].to_string()
    } else {
        " ".to_string()
    };
    let after_start = (cursor + 1).min(chars.len());
    let after_end = visible_end.min(chars.len());
    let after: String = chars[after_start..after_end].iter().collect();
    let after_len = after.len();

    if !before.is_empty() {
        spans.push(Span::styled(
            before,
            Style::default().fg(Theme::text_primary()),
        ));
    }
    spans.push(Span::styled(cursor_char, Theme::cursor()));
    if !after.is_empty() {
        spans.push(Span::styled(
            after,
            Style::default().fg(Theme::text_primary()),
        ));
    }

    if suggestion_text.is_empty() {
        return;
    }
    let used = if has_left_overflow { 1 } else { 0 }
        + (cursor - content_start)
        + 1 // cursor block
        + after_len;
    let remaining = content_width.saturating_sub(used);
    if remaining == 0 {
        return;
    }
    let sug: String = suggestion_text.chars().take(remaining).collect();
    if !sug.is_empty() {
        spans.push(Span::styled(sug, Style::default().fg(Theme::text_muted())));
    }
}

/// Build the rendered line for an unfocused text field (plain text, truncated
/// with an ellipsis when it exceeds the visible width).
fn render_unfocused_field_line(value: &str, chars: &[char], width: usize) -> Line<'static> {
    if chars.len() > width {
        let truncated: String = chars[..width - 1].iter().collect();
        return Line::from(vec![
            Span::styled(truncated, Style::default().fg(Theme::text_primary())),
            Span::styled("…", Style::default().fg(Theme::text_muted())),
        ]);
    }
    Line::from(Span::styled(
        value.to_string(),
        Style::default().fg(Theme::text_primary()),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn area(width: u16, height: u16) -> Rect {
        Rect::new(0, 0, width, height)
    }

    #[test]
    fn truncate_ellipsis_keeps_short_strings_intact() {
        assert_eq!(truncate_ellipsis("hello", 10), "hello");
        assert_eq!(truncate_ellipsis("hello", 5), "hello");
    }

    fn line_text(line: &Line) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    #[test]
    fn key_hint_line_alternates_key_and_desc_spans() {
        let line = key_hint_line(&[("Enter", " save  "), ("Esc", " cancel")]);
        assert_eq!(line.spans.len(), 4);
        assert_eq!(line_text(&line), "Enter save  Esc cancel");
    }

    #[test]
    fn editor_field_line_marks_active_and_wraps_selectors() {
        // Inactive plain text: no cursor, no marker.
        assert_eq!(
            line_text(&editor_field_line("repo", "x".into(), false, false)),
            "  repo     x"
        );
        // Active text field gets a "▸" prefix and a block cursor at the caret.
        // With no cursor supplied the caret sits past the end, drawn as a
        // trailing space-block.
        assert_eq!(
            line_text(&editor_field_line("repo", "x".into(), false, true)),
            "▸ repo     x "
        );
        // Selector values are wrapped in guillemets (no cursor even when active).
        assert_eq!(
            line_text(&editor_field_line("status", "todo".into(), true, true)),
            "▸ status   ‹ todo ›"
        );
    }

    #[test]
    fn editor_field_line_with_cursor_draws_block_at_caret() {
        // Caret in the middle: the character under the cursor is its own span,
        // so the visible text is unchanged but split before/cursor/after.
        let line = editor_field_line_with_cursor("title", "hello".into(), false, true, Some(2));
        assert_eq!(line_text(&line), "▸ title    hello");
        // Spans: label, "he", cursor "l", "lo".
        assert_eq!(line.spans.len(), 4);
        assert_eq!(line.spans[2].content.as_ref(), "l");

        // Caret at end: a trailing space-block is appended after the value.
        let line = editor_field_line_with_cursor("title", "hi".into(), false, true, Some(2));
        assert_eq!(line_text(&line), "▸ title    hi ");

        // Caret at start: cursor is the first character.
        let line = editor_field_line_with_cursor("title", "ab".into(), false, true, Some(0));
        assert_eq!(line.spans[1].content.as_ref(), "a");
    }

    #[test]
    fn truncate_ellipsis_cuts_and_appends_ellipsis() {
        assert_eq!(truncate_ellipsis("hello world", 5), "hell…");
    }

    #[test]
    fn truncate_ellipsis_returns_empty_when_too_narrow() {
        assert_eq!(truncate_ellipsis("hello", 1), "");
        assert_eq!(truncate_ellipsis("hello", 0), "");
    }

    #[test]
    fn truncate_ellipsis_counts_by_char_not_byte() {
        // Multi-byte chars count as one column each.
        assert_eq!(truncate_ellipsis("héllo wörld", 5), "héll…");
    }

    #[test]
    fn centered_rect_has_exact_height() {
        let rect = centered_fixed_height_rect(50, 10, area(100, 40));
        assert_eq!(rect.height, 10);
    }

    #[test]
    fn centered_rect_is_horizontally_centered() {
        let rect = centered_fixed_height_rect(50, 10, area(100, 40));
        assert_eq!(rect.x, 25);
        assert_eq!(rect.width, 50);
    }

    #[test]
    fn centered_rect_is_vertically_centered() {
        let rect = centered_fixed_height_rect(50, 10, area(100, 40));
        // With Min(0) / Length(10) / Min(0), the 10 lines should be centered
        // in 40 rows: (40 - 10) / 2 = 15
        assert_eq!(rect.y, 15);
    }

    #[test]
    fn centered_rect_clamps_to_area_height() {
        let rect = centered_fixed_height_rect(50, 50, area(100, 20));
        // Height is clamped to available area
        assert!(rect.height <= 20);
    }

    #[test]
    fn status_color_maps_all_variants() {
        assert_eq!(status_color(SessionStatus::Busy), Color::Green);
        assert_eq!(status_color(SessionStatus::Waiting), Color::Yellow);
        assert_eq!(status_color(SessionStatus::Idle), Color::DarkGray);
        assert_eq!(status_color(SessionStatus::Error), Color::Red);
        // Attention reuses the bright accent (distinct from the four above).
        assert_eq!(
            status_color(SessionStatus::Attention),
            Theme::accent_bright()
        );
    }

    #[test]
    fn focused_block_returns_block_for_both_states() {
        let focused = focused_block(" Test ", true);
        let unfocused = focused_block(" Test ", false);
        // Verify both produce valid blocks that can compute inner area
        let test_area = area(40, 10);
        let inner_focused = focused.inner(test_area);
        let inner_unfocused = unfocused.inner(test_area);
        // Both should produce inner areas smaller than the outer area (borders consume space)
        assert!(inner_focused.width < test_area.width);
        assert!(inner_focused.height < test_area.height);
        assert!(inner_unfocused.width < test_area.width);
        assert!(inner_unfocused.height < test_area.height);
    }

    #[test]
    fn modal_block_produces_valid_block_with_borders() {
        let test_area = area(40, 10);
        let block = modal_block("Test Modal");
        let inner = block.inner(test_area);
        assert!(inner.width < test_area.width);
        assert!(inner.height < test_area.height);
    }

    #[test]
    fn modal_block_danger_produces_valid_block_with_borders() {
        let test_area = area(40, 10);
        let block = modal_block_danger("Delete");
        let inner = block.inner(test_area);
        assert!(inner.width < test_area.width);
        assert!(inner.height < test_area.height);
    }

    #[test]
    fn modal_title_matches_focused_title() {
        assert_eq!(Theme::modal_title(), Theme::focused_title());
    }

    #[test]
    fn modal_title_danger_uses_danger_color() {
        let style = Theme::modal_title_danger();
        assert_eq!(style.bg, Some(Theme::danger()));
    }
}
