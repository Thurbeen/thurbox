use ratatui::layout::{Constraint, Direction, Layout, Rect};

pub struct PanelAreas {
    pub header: Rect,
    pub left_panel: Option<Rect>,
    pub info_panel: Option<Rect>,
    pub file_viewer: Option<Rect>,
    pub terminal: Rect,
    pub footer: Rect,
}

/// Compute panel layout areas based on terminal dimensions and optional
/// right-side panel visibility.
///
/// At width ≥ 120, the layout becomes `list | info? | terminal | file_viewer?`
/// with info (15%) and file_viewer (20%) appearing only when requested.
pub fn compute_layout(area: Rect, show_info_panel: bool, show_file_viewer: bool) -> PanelAreas {
    // Compact mode: when the terminal is shorter than 20 rows, drop the
    // header line entirely so the content + footer get every row available.
    let header_height = if area.height < 20 { 0 } else { 1 };

    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(header_height),
            Constraint::Min(1),
            Constraint::Length(1),
        ])
        .split(area);

    let header = vertical[0];
    let content = vertical[1];
    let footer = vertical[2];

    if area.width < 80 {
        return PanelAreas {
            header,
            left_panel: None,
            info_panel: None,
            file_viewer: None,
            terminal: content,
            footer,
        };
    }

    // At width ≥ 120, support optional info and file-viewer columns.
    if area.width >= 120 && (show_info_panel || show_file_viewer) {
        let mut constraints: Vec<Constraint> = vec![Constraint::Percentage(18)]; // list
        if show_info_panel {
            constraints.push(Constraint::Percentage(15));
        }
        // terminal takes the remainder
        let terminal_idx = constraints.len();
        constraints.push(Constraint::Min(0));
        if show_file_viewer {
            constraints.push(Constraint::Percentage(20));
        }

        let horizontal = Layout::default()
            .direction(Direction::Horizontal)
            .constraints(constraints)
            .split(content);

        let left_panel = Some(horizontal[0]);
        let mut idx = 1;
        let info_panel = if show_info_panel {
            let r = horizontal[idx];
            idx += 1;
            Some(r)
        } else {
            None
        };
        let terminal = horizontal[terminal_idx];
        let _ = idx;
        let file_viewer = if show_file_viewer {
            Some(horizontal[terminal_idx + 1])
        } else {
            None
        };

        return PanelAreas {
            header,
            left_panel,
            info_panel,
            file_viewer,
            terminal,
            footer,
        };
    }

    // 2-panel mode: 25% list | 75% terminal
    let horizontal = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(25), Constraint::Percentage(75)])
        .split(content);

    PanelAreas {
        header,
        left_panel: Some(horizontal[0]),
        info_panel: None,
        file_viewer: None,
        terminal: horizontal[1],
        footer,
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
        let areas = compute_layout(area(79, 24), false, false);
        assert!(areas.left_panel.is_none());
        assert!(areas.info_panel.is_none());
        assert!(areas.file_viewer.is_none());
    }

    #[test]
    fn normal_width_shows_two_panels() {
        let areas = compute_layout(area(100, 24), false, false);
        assert!(areas.left_panel.is_some());
        assert!(areas.info_panel.is_none());
        assert!(areas.file_viewer.is_none());
    }

    #[test]
    fn wide_terminal_with_info_panel_shows_three_panels() {
        let areas = compute_layout(area(120, 24), true, false);
        assert!(areas.left_panel.is_some());
        assert!(areas.info_panel.is_some());
        assert!(areas.file_viewer.is_none());
    }

    #[test]
    fn wide_terminal_without_info_panel_shows_two_panels() {
        let areas = compute_layout(area(120, 24), false, false);
        assert!(areas.left_panel.is_some());
        assert!(areas.info_panel.is_none());
        assert!(areas.file_viewer.is_none());
    }

    #[test]
    fn wide_terminal_with_file_viewer_only() {
        let areas = compute_layout(area(160, 24), false, true);
        assert!(areas.left_panel.is_some());
        assert!(areas.info_panel.is_none());
        assert!(areas.file_viewer.is_some());
    }

    #[test]
    fn wide_terminal_with_info_and_file_viewer() {
        let areas = compute_layout(area(160, 24), true, true);
        assert!(areas.left_panel.is_some());
        assert!(areas.info_panel.is_some());
        assert!(areas.file_viewer.is_some());
        // file viewer should be to the right of terminal
        let term = areas.terminal;
        let fv = areas.file_viewer.unwrap();
        assert!(fv.x >= term.x + term.width);
    }

    #[test]
    fn header_and_footer_are_one_line() {
        let areas = compute_layout(area(100, 24), false, false);
        assert_eq!(areas.header.height, 1);
        assert_eq!(areas.footer.height, 1);
    }

    #[test]
    fn compact_mode_hides_header_below_20_rows() {
        let areas = compute_layout(area(100, 19), false, false);
        assert_eq!(areas.header.height, 0);
        assert_eq!(areas.footer.height, 1);
        assert!(areas.left_panel.is_some());
    }

    #[test]
    fn header_returns_at_20_rows() {
        let areas = compute_layout(area(100, 20), false, false);
        assert_eq!(areas.header.height, 1);
    }

    #[test]
    fn info_panel_ignored_below_120_cols() {
        let areas = compute_layout(area(119, 24), true, false);
        assert!(areas.info_panel.is_none());
    }

    #[test]
    fn file_viewer_ignored_below_120_cols() {
        let areas = compute_layout(area(119, 24), false, true);
        assert!(areas.file_viewer.is_none());
    }

    fn terminal_inner(width: u16, height: u16, show_info: bool) -> (u16, u16) {
        use ratatui::widgets::{Block, Borders};
        let terminal = compute_layout(area(width, height), show_info, false).terminal;
        let inner = Block::default().borders(Borders::ALL).inner(terminal);
        (inner.height, inner.width)
    }

    #[test]
    fn two_panel_terminal_width_at_160_cols() {
        let (rows, cols) = terminal_inner(160, 40, false);
        assert_eq!(cols, 118);
        assert_eq!(rows, 36);
    }

    #[test]
    fn two_panel_terminal_width_at_80_cols() {
        let (rows, cols) = terminal_inner(80, 24, false);
        assert_eq!(cols, 58);
        assert_eq!(rows, 20);
    }

    #[test]
    fn three_panel_terminal_width_at_160_cols() {
        // 160 cols, list(18%)+info(15%)=33% reserved, terminal ≈ 67% (107) - 2 borders
        let (rows, cols) = terminal_inner(160, 40, true);
        assert!((100..=110).contains(&cols));
        assert_eq!(rows, 36);
    }

    #[test]
    fn narrow_terminal_uses_full_width() {
        let (rows, cols) = terminal_inner(60, 24, false);
        assert_eq!(cols, 58);
        assert_eq!(rows, 20);
    }
}
