use ratatui::layout::{Constraint, Direction, Layout, Rect};

pub struct PanelAreas {
    pub header: Rect,
    /// Session list area (top of the left column).
    pub left_panel: Option<Rect>,
    /// Automations pane, below the session list in the left column. Always
    /// present (even with zero automations) as long as the column is tall
    /// enough to fit both lists; its height grows with the automation count.
    pub automations_panel: Option<Rect>,
    pub info_panel: Option<Rect>,
    /// Tasks panel — a toggleable column on the right, between the terminal and
    /// the file viewer (behaves like the file viewer).
    pub tasks_panel: Option<Rect>,
    pub file_viewer: Option<Rect>,
    /// Global search strip — full-width, docked along the bottom (above the
    /// footer) when active.
    pub global_search: Option<Rect>,
    pub terminal: Rect,
    pub footer: Rect,
}

/// Rows the global-search strip occupies: a 2-row border around a query line, a
/// per-scope match summary, a scrollable result list (~7 rows), and a key-hint
/// line. Matches also highlight live in the panels behind the strip.
const GLOBAL_SEARCH_HEIGHT: u16 = 12;

/// Max rows (including borders) the automations pane may occupy.
const AUTOMATIONS_PANE_MAX_ROWS: u16 = 10;
/// Minimum rows the automations pane occupies (border + one content row), so it
/// stays visible even with zero automations.
const AUTOMATIONS_PANE_MIN_ROWS: u16 = 3;
/// Minimum rows the session list keeps when the automations pane is shown.
const SESSIONS_MIN_ROWS: u16 = 3;

/// Split a left-column rect into (sessions, automations). The automations pane
/// is always present (its height grows with `automation_count`, with a minimum
/// so an empty pane still shows) unless the column is too short for both lists.
fn split_left_column(col: Rect, automation_count: usize) -> (Rect, Option<Rect>) {
    let desired =
        (automation_count as u16 + 2).clamp(AUTOMATIONS_PANE_MIN_ROWS, AUTOMATIONS_PANE_MAX_ROWS);
    let auto_h = desired.min(col.height.saturating_sub(SESSIONS_MIN_ROWS));
    if auto_h < AUTOMATIONS_PANE_MIN_ROWS {
        return (col, None); // not enough vertical room — keep sessions only
    }
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(SESSIONS_MIN_ROWS),
            Constraint::Length(auto_h),
        ])
        .split(col);
    (rows[0], Some(rows[1]))
}

/// Compute panel layout areas based on terminal dimensions and optional
/// right-side panel visibility.
///
/// At width ≥ 120, the layout becomes
/// `list | info? | terminal | tasks? | file_viewer?` with info (15%), tasks
/// (20%), and file_viewer (20%) appearing only when requested. The tasks panel
/// sits between the terminal and the file viewer (both right-side columns). The
/// left column is further split into a session list and an always-present
/// automations pane beneath it (whenever the column is tall enough);
/// `automation_count` only sizes that pane.
pub fn compute_layout(
    area: Rect,
    show_info_panel: bool,
    show_tasks_panel: bool,
    show_file_viewer: bool,
    show_global_search: bool,
    automation_count: usize,
) -> PanelAreas {
    // Compact mode: when the terminal is shorter than 20 rows, drop the
    // header line entirely so the content + footer get every row available.
    let header_height = if area.height < 20 { 0 } else { 1 };

    // The global-search strip is carved from the bottom of the content region
    // (full width, above the footer) so every column shrinks to make room — the
    // same way the optional right-side panels share the content width.
    let search_height = if show_global_search {
        GLOBAL_SEARCH_HEIGHT.min(area.height.saturating_sub(header_height + 1))
    } else {
        0
    };

    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(header_height),
            Constraint::Min(1),
            Constraint::Length(search_height),
            Constraint::Length(1),
        ])
        .split(area);

    let header = vertical[0];
    let content = vertical[1];
    let global_search = (search_height > 0).then_some(vertical[2]);
    let footer = vertical[3];

    if area.width < 80 {
        return PanelAreas {
            header,
            left_panel: None,
            automations_panel: None,
            info_panel: None,
            tasks_panel: None,
            file_viewer: None,
            global_search,
            terminal: content,
            footer,
        };
    }

    // At width ≥ 120, support optional info / tasks / file-viewer columns.
    // Column order: list | info? | terminal | tasks? | file_viewer?.
    if area.width >= 120 && (show_info_panel || show_tasks_panel || show_file_viewer) {
        let mut constraints: Vec<Constraint> = vec![Constraint::Percentage(18)]; // list
        if show_info_panel {
            constraints.push(Constraint::Percentage(15));
        }
        // terminal takes the remainder
        let terminal_idx = constraints.len();
        constraints.push(Constraint::Min(0));
        if show_tasks_panel {
            constraints.push(Constraint::Percentage(20));
        }
        if show_file_viewer {
            constraints.push(Constraint::Percentage(20));
        }

        let horizontal = Layout::default()
            .direction(Direction::Horizontal)
            .constraints(constraints)
            .split(content);

        let info_panel = if show_info_panel {
            Some(horizontal[1])
        } else {
            None
        };
        let terminal = horizontal[terminal_idx];
        // Tasks (if shown) immediately follow the terminal; the file viewer
        // follows tasks (or the terminal when tasks are hidden).
        let mut next = terminal_idx + 1;
        let tasks_panel = if show_tasks_panel {
            let r = horizontal[next];
            next += 1;
            Some(r)
        } else {
            None
        };
        let file_viewer = if show_file_viewer {
            Some(horizontal[next])
        } else {
            None
        };

        let (left_panel, automations_panel) = split_left_column(horizontal[0], automation_count);
        return PanelAreas {
            header,
            left_panel: Some(left_panel),
            automations_panel,
            info_panel,
            tasks_panel,
            file_viewer,
            global_search,
            terminal,
            footer,
        };
    }

    // 2-panel mode: 25% list | 75% terminal
    let horizontal = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(25), Constraint::Percentage(75)])
        .split(content);

    let (left_panel, automations_panel) = split_left_column(horizontal[0], automation_count);
    PanelAreas {
        header,
        left_panel: Some(left_panel),
        automations_panel,
        info_panel: None,
        tasks_panel: None,
        file_viewer: None,
        global_search,
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
        let areas = compute_layout(area(79, 24), false, false, false, false, 0);
        assert!(areas.left_panel.is_none());
        assert!(areas.info_panel.is_none());
        assert!(areas.file_viewer.is_none());
    }

    #[test]
    fn normal_width_shows_two_panels() {
        let areas = compute_layout(area(100, 24), false, false, false, false, 0);
        assert!(areas.left_panel.is_some());
        assert!(areas.info_panel.is_none());
        assert!(areas.file_viewer.is_none());
    }

    #[test]
    fn wide_terminal_with_info_panel_shows_three_panels() {
        let areas = compute_layout(area(120, 24), true, false, false, false, 0);
        assert!(areas.left_panel.is_some());
        assert!(areas.info_panel.is_some());
        assert!(areas.file_viewer.is_none());
    }

    #[test]
    fn wide_terminal_without_info_panel_shows_two_panels() {
        let areas = compute_layout(area(120, 24), false, false, false, false, 0);
        assert!(areas.left_panel.is_some());
        assert!(areas.info_panel.is_none());
        assert!(areas.file_viewer.is_none());
    }

    #[test]
    fn wide_terminal_with_file_viewer_only() {
        let areas = compute_layout(area(160, 24), false, false, true, false, 0);
        assert!(areas.left_panel.is_some());
        assert!(areas.info_panel.is_none());
        assert!(areas.file_viewer.is_some());
    }

    #[test]
    fn wide_terminal_with_info_and_file_viewer() {
        let areas = compute_layout(area(160, 24), true, false, true, false, 0);
        assert!(areas.left_panel.is_some());
        assert!(areas.info_panel.is_some());
        assert!(areas.file_viewer.is_some());
        // file viewer should be to the right of terminal
        let term = areas.terminal;
        let fv = areas.file_viewer.unwrap();
        assert!(fv.x >= term.x + term.width);
    }

    #[test]
    fn wide_terminal_with_tasks_panel_only() {
        let areas = compute_layout(area(160, 24), false, true, false, false, 0);
        assert!(areas.left_panel.is_some());
        assert!(areas.info_panel.is_none());
        assert!(areas.tasks_panel.is_some());
        assert!(areas.file_viewer.is_none());
        // Tasks panel sits to the right of the terminal.
        let term = areas.terminal;
        let tp = areas.tasks_panel.unwrap();
        assert!(tp.x >= term.x + term.width);
    }

    #[test]
    fn tasks_panel_sits_left_of_file_viewer() {
        let areas = compute_layout(area(180, 24), false, true, true, false, 0);
        let term = areas.terminal;
        let tp = areas.tasks_panel.expect("tasks panel shown");
        let fv = areas.file_viewer.expect("file viewer shown");
        // Order: terminal → tasks → file viewer (both on the right).
        assert!(tp.x >= term.x + term.width, "tasks right of terminal");
        assert!(fv.x >= tp.x + tp.width, "file viewer right of tasks");
    }

    #[test]
    fn tasks_panel_ignored_below_120_cols() {
        let areas = compute_layout(area(119, 24), false, true, false, false, 0);
        assert!(areas.tasks_panel.is_none());
    }

    #[test]
    fn global_search_strip_absent_by_default() {
        let areas = compute_layout(area(120, 40), false, false, false, false, 0);
        assert!(areas.global_search.is_none());
    }

    #[test]
    fn global_search_strip_present_when_active() {
        let areas = compute_layout(area(120, 40), false, false, false, true, 0);
        let strip = areas.global_search.expect("strip shown when active");
        // Full width, carved directly above the footer.
        assert_eq!(strip.width, 120);
        assert_eq!(strip.x, 0);
        assert_eq!(strip.y + strip.height, areas.footer.y);
        assert_eq!(strip.height, GLOBAL_SEARCH_HEIGHT);
    }

    #[test]
    fn global_search_strip_shrinks_content() {
        let without = compute_layout(area(120, 40), false, false, false, false, 0).terminal;
        let with = compute_layout(area(120, 40), false, false, false, true, 0).terminal;
        // The terminal (content) region loses the strip's rows.
        assert_eq!(without.height - with.height, GLOBAL_SEARCH_HEIGHT);
    }

    #[test]
    fn header_and_footer_are_one_line() {
        let areas = compute_layout(area(100, 24), false, false, false, false, 0);
        assert_eq!(areas.header.height, 1);
        assert_eq!(areas.footer.height, 1);
    }

    #[test]
    fn compact_mode_hides_header_below_20_rows() {
        let areas = compute_layout(area(100, 19), false, false, false, false, 0);
        assert_eq!(areas.header.height, 0);
        assert_eq!(areas.footer.height, 1);
        assert!(areas.left_panel.is_some());
    }

    #[test]
    fn header_returns_at_20_rows() {
        let areas = compute_layout(area(100, 20), false, false, false, false, 0);
        assert_eq!(areas.header.height, 1);
    }

    #[test]
    fn info_panel_ignored_below_120_cols() {
        let areas = compute_layout(area(119, 24), true, false, false, false, 0);
        assert!(areas.info_panel.is_none());
    }

    #[test]
    fn file_viewer_ignored_below_120_cols() {
        let areas = compute_layout(area(119, 24), false, false, true, false, 0);
        assert!(areas.file_viewer.is_none());
    }

    fn terminal_inner(width: u16, height: u16, show_info: bool) -> (u16, u16) {
        use ratatui::widgets::{Block, Borders};
        let terminal =
            compute_layout(area(width, height), show_info, false, false, false, 0).terminal;
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

    #[test]
    fn automations_pane_present_even_when_empty() {
        // Zero automations still get a minimum-height pane (so it's discoverable).
        let areas = compute_layout(area(100, 24), false, false, false, false, 0);
        assert!(areas.left_panel.is_some());
        let autos = areas.automations_panel.expect("empty pane still shown");
        assert_eq!(autos.height, AUTOMATIONS_PANE_MIN_ROWS);
    }

    #[test]
    fn automations_pane_appears_below_sessions() {
        let areas = compute_layout(area(100, 30), false, false, false, false, 2);
        let sessions = areas.left_panel.unwrap();
        let autos = areas.automations_panel.expect("automations pane shown");
        // Same column, automations stacked beneath the session list.
        assert_eq!(sessions.x, autos.x);
        assert_eq!(sessions.width, autos.width);
        assert_eq!(autos.y, sessions.y + sessions.height);
        // 2 automations + 2 border rows = 4 rows tall.
        assert_eq!(autos.height, 4);
        assert!(sessions.height >= SESSIONS_MIN_ROWS);
    }

    #[test]
    fn automations_pane_height_is_capped() {
        let areas = compute_layout(area(100, 60), false, false, false, false, 50);
        assert_eq!(
            areas.automations_panel.unwrap().height,
            AUTOMATIONS_PANE_MAX_ROWS
        );
    }

    #[test]
    fn automations_pane_hidden_when_column_too_short() {
        // Content height ≈ 4 rows leaves no room for both lists.
        let areas = compute_layout(area(100, 6), false, false, false, false, 3);
        assert!(areas.left_panel.is_some());
        assert!(areas.automations_panel.is_none());
    }
}
