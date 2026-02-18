pub mod add_project_modal;
pub mod branch_selector_modal;
pub mod delete_project_modal;
pub mod edit_project_modal;
pub mod info_panel;
pub mod layout;
pub mod links;
pub mod mcp_editor_modal;
pub mod project_list;
pub mod repo_selector_modal;
pub mod role_editor_modal;
pub mod role_selector_modal;
pub mod session_mode_modal;
pub mod status_bar;
pub mod terminal_view;
pub mod worktree_name_modal;

use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Paragraph},
    Frame,
};

use crate::session::SessionStatus;

pub fn status_color(status: SessionStatus) -> Color {
    match status {
        SessionStatus::Busy => Color::Green,
        SessionStatus::Waiting => Color::Yellow,
        SessionStatus::Idle => Color::DarkGray,
        SessionStatus::Error => Color::Red,
    }
}

/// Build a [`Block`] with focused or unfocused styling.
///
/// Focused: thick borders in cyan with a highlighted title badge.
/// Unfocused: plain borders in gray with a dimmed title.
pub fn focused_block(title_text: &str, focused: bool) -> Block<'_> {
    if focused {
        Block::default()
            .title(Line::from(Span::styled(
                title_text,
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            )))
            .borders(Borders::ALL)
            .border_type(BorderType::Thick)
            .border_style(Style::default().fg(Color::Cyan))
    } else {
        Block::default()
            .title(Line::from(Span::styled(
                title_text,
                Style::default().fg(Color::Gray),
            )))
            .borders(Borders::ALL)
            .border_type(BorderType::Plain)
            .border_style(Style::default().fg(Color::Gray))
    }
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
    let border_color = if focused { Color::Cyan } else { Color::Gray };

    let block = Block::default()
        .title(format!(" {label} "))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(border_color));

    let inner = block.inner(area);
    let width = inner.width as usize;

    let chars: Vec<char> = value.chars().collect();
    let cursor = cursor.min(chars.len());

    let display = if focused && width > 0 {
        let at_end = cursor == chars.len();
        let suggestion_text = if at_end { suggestion.unwrap_or("") } else { "" };

        let has_left_overflow;
        let has_right_overflow;

        let viewport_start = if chars.len() < width {
            has_left_overflow = false;
            has_right_overflow = false;
            0
        } else {
            let usable = width.saturating_sub(1);
            let start = if cursor < usable {
                0
            } else {
                cursor - usable + 1
            };
            has_left_overflow = start > 0;
            has_right_overflow = start + width < chars.len() + 1;
            start
        };

        let content_start = if has_left_overflow {
            viewport_start + 1
        } else {
            viewport_start
        };
        let content_width =
            width - if has_left_overflow { 1 } else { 0 } - if has_right_overflow { 1 } else { 0 };

        let mut spans = Vec::new();

        if has_left_overflow {
            spans.push(Span::styled("◀", Style::default().fg(Color::DarkGray)));
        }

        let visible_end = (content_start + content_width).min(chars.len());

        if cursor >= content_start && cursor <= visible_end {
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
                spans.push(Span::styled(before, Style::default().fg(Color::White)));
            }
            spans.push(Span::styled(
                cursor_char,
                Style::default().fg(Color::Black).bg(Color::White),
            ));
            if !after.is_empty() {
                spans.push(Span::styled(after, Style::default().fg(Color::White)));
            }

            if !suggestion_text.is_empty() {
                let used = if has_left_overflow { 1 } else { 0 }
                    + (cursor - content_start)
                    + 1 // cursor block
                    + after_len;
                let remaining = content_width.saturating_sub(used);
                if remaining > 0 {
                    let sug: String = suggestion_text.chars().take(remaining).collect();
                    if !sug.is_empty() {
                        spans.push(Span::styled(sug, Style::default().fg(Color::DarkGray)));
                    }
                }
            }
        } else {
            let visible: String = chars[content_start..visible_end].iter().collect();
            spans.push(Span::styled(visible, Style::default().fg(Color::White)));
        }

        if has_right_overflow {
            spans.push(Span::styled("▶", Style::default().fg(Color::DarkGray)));
        }

        Line::from(spans)
    } else if width > 0 {
        if chars.len() > width {
            let truncated: String = chars[..width - 1].iter().collect();
            Line::from(vec![
                Span::styled(truncated, Style::default().fg(Color::White)),
                Span::styled("…", Style::default().fg(Color::DarkGray)),
            ])
        } else {
            Line::from(Span::styled(value, Style::default().fg(Color::White)))
        }
    } else {
        Line::from("")
    };

    frame.render_widget(block, area);
    frame.render_widget(Paragraph::new(display), inner);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn area(width: u16, height: u16) -> Rect {
        Rect::new(0, 0, width, height)
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
}
