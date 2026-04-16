pub mod branch_selector_modal;
pub mod containerfile_picker;
pub mod file_viewer;
pub mod info_panel;
pub mod layout;
pub mod links;
pub mod mcp_editor_modal;
pub mod mcp_server_picker_modal;
pub mod model_picker_modal;
pub mod project_list;
pub mod repo_picker_modal;
pub mod repo_selector_modal;
pub mod restore_sessions_modal;
pub mod role_editor_modal;
pub mod role_selector_modal;
pub mod schedule_command_modal;
pub mod scheduled_commands_list_modal;
pub mod selection;
pub mod session_mode_modal;
pub mod session_name_modal;
pub mod settings_overlay;
pub mod skill_picker_modal;
pub mod status_bar;
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
        SessionStatus::Provisioning => Theme::accent(),
        SessionStatus::Busy => Theme::status_busy(),
        SessionStatus::Waiting => Theme::status_waiting(),
        SessionStatus::Idle => Theme::status_idle(),
        SessionStatus::Error => Theme::status_error(),
    }
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

/// Build a [`Block`] with yellow admin styling (tri-state focus).
///
/// Focused/Active use `ADMIN_BORDER` (yellow); Inactive falls back to
/// the standard unfocused gray, keeping the admin chrome unobtrusive
/// when the panel is in the background.
pub fn admin_block(title_text: &str, level: FocusLevel) -> Block<'_> {
    match level {
        FocusLevel::Focused => Block::default()
            .title(Line::from(Span::styled(title_text, Theme::admin_title())))
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(Theme::admin_border())),
        FocusLevel::Active => Block::default()
            .title(Line::from(Span::styled(title_text, Theme::admin_title())))
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(Theme::admin_border())),
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
            spans.push(Span::styled("◀", Style::default().fg(Theme::text_muted())));
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

            if !suggestion_text.is_empty() {
                let used = if has_left_overflow { 1 } else { 0 }
                    + (cursor - content_start)
                    + 1 // cursor block
                    + after_len;
                let remaining = content_width.saturating_sub(used);
                if remaining > 0 {
                    let sug: String = suggestion_text.chars().take(remaining).collect();
                    if !sug.is_empty() {
                        spans.push(Span::styled(sug, Style::default().fg(Theme::text_muted())));
                    }
                }
            }
        } else {
            let visible: String = chars[content_start..visible_end].iter().collect();
            spans.push(Span::styled(
                visible,
                Style::default().fg(Theme::text_primary()),
            ));
        }

        if has_right_overflow {
            spans.push(Span::styled("▶", Style::default().fg(Theme::text_muted())));
        }

        Line::from(spans)
    } else if width > 0 {
        if chars.len() > width {
            let truncated: String = chars[..width - 1].iter().collect();
            Line::from(vec![
                Span::styled(truncated, Style::default().fg(Theme::text_primary())),
                Span::styled("…", Style::default().fg(Theme::text_muted())),
            ])
        } else {
            Line::from(Span::styled(
                value,
                Style::default().fg(Theme::text_primary()),
            ))
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
        assert_eq!(status_color(SessionStatus::Provisioning), Color::Cyan);
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

    #[test]
    fn admin_block_returns_block_for_all_focus_levels() {
        let test_area = area(40, 10);
        for level in [
            FocusLevel::Focused,
            FocusLevel::Active,
            FocusLevel::Inactive,
        ] {
            let block = admin_block(" Admin ", level);
            let inner = block.inner(test_area);
            assert!(inner.width < test_area.width);
            assert!(inner.height < test_area.height);
        }
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
