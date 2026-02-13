pub mod add_project_modal;
pub mod info_panel;
pub mod layout;
pub mod project_list;
pub mod repo_selector_modal;
pub mod status_bar;
pub mod terminal_view;

use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders},
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
