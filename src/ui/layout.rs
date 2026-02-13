use ratatui::layout::{Constraint, Direction, Layout, Rect};

pub struct PanelAreas {
    pub header: Rect,
    pub session_list: Option<Rect>,
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
            session_list: None,
            info_panel: None,
            terminal: content,
            footer,
        };
    }

    if show_info_panel && area.width >= 120 {
        // 3-panel mode: 15% session list | 15% info | 70% terminal
        let horizontal = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Percentage(15),
                Constraint::Percentage(15),
                Constraint::Percentage(70),
            ])
            .split(content);

        PanelAreas {
            header,
            session_list: Some(horizontal[0]),
            info_panel: Some(horizontal[1]),
            terminal: horizontal[2],
            footer,
        }
    } else {
        // 2-panel mode: 20% session list | 80% terminal
        let horizontal = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(20), Constraint::Percentage(80)])
            .split(content);

        PanelAreas {
            header,
            session_list: Some(horizontal[0]),
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
    fn narrow_terminal_hides_session_list() {
        let areas = compute_layout(area(79, 24), false);
        assert!(areas.session_list.is_none());
        assert!(areas.info_panel.is_none());
    }

    #[test]
    fn normal_width_shows_two_panels() {
        let areas = compute_layout(area(100, 24), false);
        assert!(areas.session_list.is_some());
        assert!(areas.info_panel.is_none());
    }

    #[test]
    fn wide_terminal_with_info_panel_shows_three_panels() {
        let areas = compute_layout(area(120, 24), true);
        assert!(areas.session_list.is_some());
        assert!(areas.info_panel.is_some());
    }

    #[test]
    fn wide_terminal_without_info_panel_shows_two_panels() {
        let areas = compute_layout(area(120, 24), false);
        assert!(areas.session_list.is_some());
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
}
