pub mod add_project_modal;
pub mod branch_selector_modal;
pub mod info_panel;
pub mod layout;
pub mod project_list;
pub mod repo_selector_modal;
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

/// Render a labeled text input field with cursor visualization.
///
/// When `focused` is true, a block cursor is shown at the current position.
/// When unfocused, the value is displayed as plain text with a dimmed border.
pub fn render_text_field(
    frame: &mut Frame,
    area: Rect,
    label: &str,
    value: &str,
    cursor: usize,
    focused: bool,
) {
    let border_color = if focused { Color::Cyan } else { Color::Gray };

    let block = Block::default()
        .title(format!(" {label} "))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(border_color));

    let inner = block.inner(area);

    let display = if focused {
        let chars: Vec<char> = value.chars().collect();
        let (before, after) = if cursor <= chars.len() {
            let before: String = chars[..cursor].iter().collect();
            let after: String = chars[cursor..].iter().collect();
            (before, after)
        } else {
            (value.to_string(), String::new())
        };

        let cursor_char = if after.is_empty() {
            " ".to_string()
        } else {
            after.chars().next().unwrap().to_string()
        };

        let rest = if after.len() > cursor_char.len() {
            &after[cursor_char.len()..]
        } else {
            ""
        };

        Line::from(vec![
            Span::styled(before, Style::default().fg(Color::White)),
            Span::styled(
                cursor_char,
                Style::default().fg(Color::Black).bg(Color::White),
            ),
            Span::styled(rest.to_string(), Style::default().fg(Color::White)),
        ])
    } else {
        Line::from(Span::styled(value, Style::default().fg(Color::White)))
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
