use ratatui::layout::{Constraint, Direction, Layout, Rect};

pub struct PanelAreas {
    pub header: Rect,
    pub left_panel: Option<Rect>,
    pub info_panel: Option<Rect>,
    pub terminal: Rect,
    pub footer: Rect,
}

/// Compute panel layout areas based on terminal dimensions and info panel visibility.
pub fn compute_layout(area: Rect, show_info_panel: bool) -> PanelAreas {
    // Vertical split: header | content | footer
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(1),
            Constraint::Length(1),
        ])
        .split(area);

    let header = vertical[0];
    let content = vertical[1];
    let footer = vertical[2];

    // If terminal is too narrow, show terminal only
    if area.width < 80 {
        return PanelAreas {
            header,
            left_panel: None,
            info_panel: None,
            terminal: content,
            footer,
        };
    }

    if show_info_panel && area.width >= 120 {
        // 3-panel mode: 18% left panel | 15% info | 67% terminal
        let horizontal = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Percentage(18),
                Constraint::Percentage(15),
                Constraint::Percentage(67),
            ])
            .split(content);

        PanelAreas {
            header,
            left_panel: Some(horizontal[0]),
            info_panel: Some(horizontal[1]),
            terminal: horizontal[2],
            footer,
        }
    } else {
        // 2-panel mode: 25% left panel | 75% terminal
        let horizontal = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(25), Constraint::Percentage(75)])
            .split(content);

        PanelAreas {
            header,
            left_panel: Some(horizontal[0]),
            info_panel: None,
            terminal: horizontal[1],
            footer,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn area(width: u16, height: u16) -> Rect {
        Rect::new(0, 0, width, height)
    }

    #[test]
    fn narrow_terminal_hides_left_panel() {
        let areas = compute_layout(area(79, 24), false);
        assert!(areas.left_panel.is_none());
        assert!(areas.info_panel.is_none());
    }

    #[test]
    fn normal_width_shows_two_panels() {
        let areas = compute_layout(area(100, 24), false);
        assert!(areas.left_panel.is_some());
        assert!(areas.info_panel.is_none());
    }

    #[test]
    fn wide_terminal_with_info_panel_shows_three_panels() {
        let areas = compute_layout(area(120, 24), true);
        assert!(areas.left_panel.is_some());
        assert!(areas.info_panel.is_some());
    }

    #[test]
    fn wide_terminal_without_info_panel_shows_two_panels() {
        let areas = compute_layout(area(120, 24), false);
        assert!(areas.left_panel.is_some());
        assert!(areas.info_panel.is_none());
    }

    #[test]
    fn header_and_footer_are_one_line() {
        let areas = compute_layout(area(100, 24), false);
        assert_eq!(areas.header.height, 1);
        assert_eq!(areas.footer.height, 1);
    }

    #[test]
    fn info_panel_ignored_below_120_cols() {
        let areas = compute_layout(area(119, 24), true);
        assert!(areas.info_panel.is_none());
    }

    /// Inner dimensions after removing the 1-cell border on all sides,
    /// matching what `content_area_size()` computes for tmux/vt100 sizing.
    fn terminal_inner(width: u16, height: u16, show_info: bool) -> (u16, u16) {
        use ratatui::widgets::{Block, Borders};
        let terminal = compute_layout(area(width, height), show_info).terminal;
        let inner = Block::default().borders(Borders::ALL).inner(terminal);
        (inner.height, inner.width)
    }

    #[test]
    fn two_panel_terminal_width_at_160_cols() {
        // 160 * 75% = 120 cols for terminal panel, minus 2 for borders = 118 inner
        let (rows, cols) = terminal_inner(160, 40, false);
        assert_eq!(cols, 118);
        // 40 - header(1) - footer(1) - top/bottom border(2) = 36
        assert_eq!(rows, 36);
    }

    #[test]
    fn two_panel_terminal_width_at_80_cols() {
        let (rows, cols) = terminal_inner(80, 24, false);
        assert_eq!(cols, 58); // 80 * 75% = 60, minus 2 borders
        assert_eq!(rows, 20); // 24 - 1 - 1 - 2
    }

    #[test]
    fn three_panel_terminal_width_at_160_cols() {
        let (rows, cols) = terminal_inner(160, 40, true);
        // 160 * 67% = 107 cols for terminal, minus 2 borders = 105 inner
        assert_eq!(cols, 105);
        assert_eq!(rows, 36);
    }

    #[test]
    fn narrow_terminal_uses_full_width() {
        let (rows, cols) = terminal_inner(60, 24, false);
        // No panels, full width minus 2 borders = 58
        assert_eq!(cols, 58);
        assert_eq!(rows, 20);
    }
}
