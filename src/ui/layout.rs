use ratatui::layout::{Constraint, Direction, Layout, Rect};

pub struct PanelAreas {
    pub header: Rect,
    /// Session list area (top of the left column).
    pub left_panel: Option<Rect>,
    /// Automations pane, below the session list in the left column. Present
    /// (even with zero automations) as long as the automations feature is
    /// enabled and the column is tall enough to fit both lists; its height
    /// grows with the automation count.
    pub automations_panel: Option<Rect>,
    pub info_panel: Option<Rect>,
    /// Tasks panel — a toggleable column on the right, between the terminal and
    /// the file viewer (behaves like the file viewer).
    pub tasks_panel: Option<Rect>,
    pub file_viewer: Option<Rect>,
    /// Global search strip — full-width, docked along the bottom (above the
    /// footer) when active.
    pub global_search: Option<Rect>,
    /// Full-width transient band for the active status/error message (or the
    /// sync spinner), docked directly above the footer. Present only while a
    /// message is showing, so nothing is clipped by the footer pills.
    pub status_message: Option<Rect>,
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

/// Vertical bands carved from the full area: header, content region, optional
/// global-search strip, optional status-message row, and footer.
struct VerticalBands {
    header: Rect,
    content: Rect,
    global_search: Option<Rect>,
    status_message: Option<Rect>,
    footer: Rect,
}

/// Split the full area into header / content / global-search / status-message /
/// footer bands.
fn split_vertical(area: Rect, show_global_search: bool, show_status_row: bool) -> VerticalBands {
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

    // One transient row for the active status/error message, directly above the
    // footer (keeping the pills pinned to the bottom edge). Carved only while a
    // message is showing, so content shrinks by 1 only transiently.
    let status_height = if show_status_row { 1 } else { 0 };

    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(header_height),
            Constraint::Min(1),
            Constraint::Length(search_height),
            Constraint::Length(status_height),
            Constraint::Length(1),
        ])
        .split(area);

    VerticalBands {
        header: vertical[0],
        content: vertical[1],
        global_search: (search_height > 0).then_some(vertical[2]),
        status_message: (status_height > 0).then_some(vertical[3]),
        footer: vertical[4],
    }
}

/// Split a left-column rect into (session list, automations pane) honouring the
/// `show_automations_pane` flag.
fn left_column_split(
    col: Rect,
    show_automations_pane: bool,
    automation_count: usize,
) -> (Rect, Option<Rect>) {
    if show_automations_pane {
        split_left_column(col, automation_count)
    } else {
        (col, None)
    }
}

/// Build the wide (≥ three_panel_min_cols) layout with optional info / tasks /
/// file-viewer columns. Column order: list? | info? | terminal | tasks? |
/// file_viewer?. The list column is omitted entirely when
/// `show_session_list` is false (the terminal expands to fill the freed width),
/// but the right-side columns are unaffected.
#[allow(clippy::too_many_arguments)]
fn three_panel_layout(
    bands: &VerticalBands,
    content: Rect,
    show_session_list: bool,
    show_info_panel: bool,
    show_tasks_panel: bool,
    show_file_viewer: bool,
    show_automations_pane: bool,
    automation_count: usize,
) -> PanelAreas {
    let mut constraints: Vec<Constraint> = Vec::new();
    if show_session_list {
        constraints.push(Constraint::Percentage(18));
    }
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

    // Walk the split left→right. The list column (when present) is index 0,
    // followed by info; the terminal sits at `terminal_idx` regardless of
    // whether the list column was emitted.
    let mut idx = 0;
    let (left_panel, automations_panel) = if show_session_list {
        let (lp, ap) = left_column_split(horizontal[idx], show_automations_pane, automation_count);
        idx += 1;
        (Some(lp), ap)
    } else {
        (None, None)
    };
    let info_panel = show_info_panel.then(|| {
        let r = horizontal[idx];
        idx += 1;
        r
    });
    let terminal = horizontal[terminal_idx];
    // Tasks (if shown) immediately follow the terminal; the file viewer
    // follows tasks (or the terminal when tasks are hidden).
    let mut next = terminal_idx + 1;
    let tasks_panel = show_tasks_panel.then(|| {
        let r = horizontal[next];
        next += 1;
        r
    });
    let file_viewer = show_file_viewer.then(|| horizontal[next]);

    PanelAreas {
        header: bands.header,
        left_panel,
        automations_panel,
        info_panel,
        tasks_panel,
        file_viewer,
        global_search: bands.global_search,
        status_message: bands.status_message,
        terminal,
        footer: bands.footer,
    }
}

/// Build the 2-panel layout: 25% list | 75% terminal. When the session list
/// is hidden the terminal takes the full content width (no list column).
fn two_panel_layout(
    bands: &VerticalBands,
    content: Rect,
    show_session_list: bool,
    show_automations_pane: bool,
    automation_count: usize,
) -> PanelAreas {
    if !show_session_list {
        return PanelAreas {
            header: bands.header,
            left_panel: None,
            automations_panel: None,
            info_panel: None,
            tasks_panel: None,
            file_viewer: None,
            global_search: bands.global_search,
            status_message: bands.status_message,
            terminal: content,
            footer: bands.footer,
        };
    }
    let horizontal = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(25), Constraint::Percentage(75)])
        .split(content);

    let (left_panel, automations_panel) =
        left_column_split(horizontal[0], show_automations_pane, automation_count);
    PanelAreas {
        header: bands.header,
        left_panel: Some(left_panel),
        automations_panel,
        info_panel: None,
        tasks_panel: None,
        file_viewer: None,
        global_search: bands.global_search,
        status_message: bands.status_message,
        terminal: horizontal[1],
        footer: bands.footer,
    }
}

/// Compute panel layout areas based on terminal dimensions and optional
/// right-side panel visibility.
///
/// At width ≥ 120, the layout becomes
/// `list? | info? | terminal | tasks? | file_viewer?` with info (15%), tasks
/// (20%), and file_viewer (20%) appearing only when requested. The tasks panel
/// sits between the terminal and the file viewer (both right-side columns). The
/// left column is further split into a session list and an automations pane
/// beneath it (whenever the column is tall enough and `show_automations_pane`
/// is set — false when the `automations` feature flag is off);
/// `automation_count` only sizes that pane. When `show_session_list` is false
/// the whole left column (sessions + automations) is dropped and the terminal
/// expands — the right-side panels are unaffected.
///
/// `show_status_row` carves a transient full-width 1-row band directly above the
/// footer for the active status/error message (or the sync spinner), so a long
/// message is never clipped by the right-aligned footer pills. It shrinks the
/// content region by one row while shown (mirroring `show_global_search`).
#[allow(clippy::too_many_arguments)]
pub fn compute_layout(
    area: Rect,
    show_session_list: bool,
    show_info_panel: bool,
    show_tasks_panel: bool,
    show_file_viewer: bool,
    show_global_search: bool,
    show_automations_pane: bool,
    automation_count: usize,
    show_status_row: bool,
) -> PanelAreas {
    let bands = split_vertical(area, show_global_search, show_status_row);
    let content = bands.content;

    let settings = crate::session::settings::global();
    if area.width < settings.two_panel_min_cols {
        return PanelAreas {
            header: bands.header,
            left_panel: None,
            automations_panel: None,
            info_panel: None,
            tasks_panel: None,
            file_viewer: None,
            global_search: bands.global_search,
            status_message: bands.status_message,
            terminal: content,
            footer: bands.footer,
        };
    }

    // At width ≥ three_panel_min_cols (default 120), support optional info /
    // tasks / file-viewer columns.
    if area.width >= settings.three_panel_min_cols
        && (show_info_panel || show_tasks_panel || show_file_viewer)
    {
        return three_panel_layout(
            &bands,
            content,
            show_session_list,
            show_info_panel,
            show_tasks_panel,
            show_file_viewer,
            show_automations_pane,
            automation_count,
        );
    }

    two_panel_layout(
        &bands,
        content,
        show_session_list,
        show_automations_pane,
        automation_count,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn area(width: u16, height: u16) -> Rect {
        Rect::new(0, 0, width, height)
    }

    #[test]
    fn narrow_terminal_hides_left_panel() {
        let areas = compute_layout(
            area(79, 24),
            true,
            false,
            false,
            false,
            false,
            true,
            0,
            false,
        );
        assert!(areas.left_panel.is_none());
        assert!(areas.info_panel.is_none());
        assert!(areas.file_viewer.is_none());
    }

    #[test]
    fn normal_width_shows_two_panels() {
        let areas = compute_layout(
            area(100, 24),
            true,
            false,
            false,
            false,
            false,
            true,
            0,
            false,
        );
        assert!(areas.left_panel.is_some());
        assert!(areas.info_panel.is_none());
        assert!(areas.file_viewer.is_none());
    }

    #[test]
    fn wide_terminal_with_info_panel_shows_three_panels() {
        let areas = compute_layout(
            area(120, 24),
            true,
            true,
            false,
            false,
            false,
            true,
            0,
            false,
        );
        assert!(areas.left_panel.is_some());
        assert!(areas.info_panel.is_some());
        assert!(areas.file_viewer.is_none());
    }

    #[test]
    fn wide_terminal_without_info_panel_shows_two_panels() {
        let areas = compute_layout(
            area(120, 24),
            true,
            false,
            false,
            false,
            false,
            true,
            0,
            false,
        );
        assert!(areas.left_panel.is_some());
        assert!(areas.info_panel.is_none());
        assert!(areas.file_viewer.is_none());
    }

    #[test]
    fn wide_terminal_with_file_viewer_only() {
        let areas = compute_layout(
            area(160, 24),
            true,
            false,
            false,
            true,
            false,
            true,
            0,
            false,
        );
        assert!(areas.left_panel.is_some());
        assert!(areas.info_panel.is_none());
        assert!(areas.file_viewer.is_some());
    }

    #[test]
    fn wide_terminal_with_info_and_file_viewer() {
        let areas = compute_layout(
            area(160, 24),
            true,
            true,
            false,
            true,
            false,
            true,
            0,
            false,
        );
        assert!(areas.left_panel.is_some());
        assert!(areas.info_panel.is_some());
        assert!(areas.file_viewer.is_some());
        let term = areas.terminal;
        let fv = areas.file_viewer.unwrap();
        assert!(fv.x >= term.x + term.width);
    }

    #[test]
    fn wide_terminal_with_tasks_panel_only() {
        let areas = compute_layout(
            area(160, 24),
            true,
            false,
            true,
            false,
            false,
            true,
            0,
            false,
        );
        assert!(areas.left_panel.is_some());
        assert!(areas.info_panel.is_none());
        assert!(areas.tasks_panel.is_some());
        assert!(areas.file_viewer.is_none());
        let term = areas.terminal;
        let tp = areas.tasks_panel.unwrap();
        assert!(tp.x >= term.x + term.width);
    }

    #[test]
    fn tasks_panel_sits_left_of_file_viewer() {
        let areas = compute_layout(
            area(180, 24),
            true,
            false,
            true,
            true,
            false,
            true,
            0,
            false,
        );
        let term = areas.terminal;
        let tp = areas.tasks_panel.expect("tasks panel shown");
        let fv = areas.file_viewer.expect("file viewer shown");
        assert!(tp.x >= term.x + term.width, "tasks right of terminal");
        assert!(fv.x >= tp.x + tp.width, "file viewer right of tasks");
    }

    #[test]
    fn tasks_panel_ignored_below_120_cols() {
        let areas = compute_layout(
            area(119, 24),
            true,
            false,
            true,
            false,
            false,
            true,
            0,
            false,
        );
        assert!(areas.tasks_panel.is_none());
    }

    #[test]
    fn global_search_strip_absent_by_default() {
        let areas = compute_layout(
            area(120, 40),
            true,
            false,
            false,
            false,
            false,
            true,
            0,
            false,
        );
        assert!(areas.global_search.is_none());
    }

    #[test]
    fn global_search_strip_present_when_active() {
        let areas = compute_layout(
            area(120, 40),
            true,
            false,
            false,
            false,
            true,
            true,
            0,
            false,
        );
        let strip = areas.global_search.expect("strip shown when active");
        // Full width, carved directly above the footer.
        assert_eq!(strip.width, 120);
        assert_eq!(strip.x, 0);
        assert_eq!(strip.y + strip.height, areas.footer.y);
        assert_eq!(strip.height, GLOBAL_SEARCH_HEIGHT);
    }

    #[test]
    fn global_search_strip_shrinks_content() {
        let without = compute_layout(
            area(120, 40),
            true,
            false,
            false,
            false,
            false,
            true,
            0,
            false,
        )
        .terminal;
        let with = compute_layout(
            area(120, 40),
            true,
            false,
            false,
            false,
            true,
            true,
            0,
            false,
        )
        .terminal;
        // The terminal (content) region loses the strip's rows.
        assert_eq!(without.height - with.height, GLOBAL_SEARCH_HEIGHT);
    }

    #[test]
    fn status_row_absent_by_default() {
        let areas = compute_layout(
            area(120, 40),
            true,
            false,
            false,
            false,
            false,
            true,
            0,
            false,
        );
        assert!(areas.status_message.is_none());
        assert_eq!(areas.footer.height, 1);
    }

    #[test]
    fn status_row_present_when_active() {
        let areas = compute_layout(
            area(120, 40),
            true,
            false,
            false,
            false,
            false,
            true,
            0,
            true,
        );
        let row = areas
            .status_message
            .expect("row shown when a message is active");
        // Full width, one row, docked directly above the footer.
        assert_eq!(row.width, 120);
        assert_eq!(row.x, 0);
        assert_eq!(row.height, 1);
        assert_eq!(row.y + row.height, areas.footer.y);
    }

    #[test]
    fn status_row_shrinks_content_by_one() {
        let without = compute_layout(
            area(120, 40),
            true,
            false,
            false,
            false,
            false,
            true,
            0,
            false,
        )
        .terminal;
        let with = compute_layout(
            area(120, 40),
            true,
            false,
            false,
            false,
            false,
            true,
            0,
            true,
        )
        .terminal;
        assert_eq!(without.height - with.height, 1);
    }

    #[test]
    fn status_row_stacks_below_global_search() {
        // Both strips active: search on top, status row just above the footer.
        let areas = compute_layout(
            area(120, 40),
            true,
            false,
            false,
            false,
            true,
            true,
            0,
            true,
        );
        let gs = areas.global_search.expect("search strip shown");
        let sm = areas.status_message.expect("status row shown");
        assert!(
            sm.y >= gs.y + gs.height,
            "status row sits below the search strip"
        );
        assert_eq!(
            sm.y + sm.height,
            areas.footer.y,
            "status row sits above the footer"
        );
    }

    #[test]
    fn header_and_footer_are_one_line() {
        let areas = compute_layout(
            area(100, 24),
            true,
            false,
            false,
            false,
            false,
            true,
            0,
            false,
        );
        assert_eq!(areas.header.height, 1);
        assert_eq!(areas.footer.height, 1);
    }

    #[test]
    fn compact_mode_hides_header_below_20_rows() {
        let areas = compute_layout(
            area(100, 19),
            true,
            false,
            false,
            false,
            false,
            true,
            0,
            false,
        );
        assert_eq!(areas.header.height, 0);
        assert_eq!(areas.footer.height, 1);
        assert!(areas.left_panel.is_some());
    }

    #[test]
    fn header_returns_at_20_rows() {
        let areas = compute_layout(
            area(100, 20),
            true,
            false,
            false,
            false,
            false,
            true,
            0,
            false,
        );
        assert_eq!(areas.header.height, 1);
    }

    #[test]
    fn info_panel_ignored_below_120_cols() {
        let areas = compute_layout(
            area(119, 24),
            true,
            true,
            false,
            false,
            false,
            true,
            0,
            false,
        );
        assert!(areas.info_panel.is_none());
    }

    #[test]
    fn file_viewer_ignored_below_120_cols() {
        let areas = compute_layout(
            area(119, 24),
            true,
            false,
            false,
            true,
            false,
            true,
            0,
            false,
        );
        assert!(areas.file_viewer.is_none());
    }

    fn terminal_inner(width: u16, height: u16, show_info: bool) -> (u16, u16) {
        use ratatui::widgets::{Block, Borders};
        let terminal = compute_layout(
            area(width, height),
            true,
            show_info,
            false,
            false,
            false,
            true,
            0,
            false,
        )
        .terminal;
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
        let areas = compute_layout(
            area(100, 24),
            true,
            false,
            false,
            false,
            false,
            true,
            0,
            false,
        );
        assert!(areas.left_panel.is_some());
        let autos = areas.automations_panel.expect("empty pane still shown");
        assert_eq!(autos.height, AUTOMATIONS_PANE_MIN_ROWS);
    }

    #[test]
    fn automations_pane_appears_below_sessions() {
        let areas = compute_layout(
            area(100, 30),
            true,
            false,
            false,
            false,
            false,
            true,
            2,
            false,
        );
        let sessions = areas.left_panel.unwrap();
        let autos = areas.automations_panel.expect("automations pane shown");
        assert_eq!(sessions.x, autos.x);
        assert_eq!(sessions.width, autos.width);
        assert_eq!(autos.y, sessions.y + sessions.height);
        // 2 automations + 2 border rows = 4 rows tall.
        assert_eq!(autos.height, 4);
        assert!(sessions.height >= SESSIONS_MIN_ROWS);
    }

    #[test]
    fn automations_pane_height_is_capped() {
        let areas = compute_layout(
            area(100, 60),
            true,
            false,
            false,
            false,
            false,
            true,
            50,
            false,
        );
        assert_eq!(
            areas.automations_panel.unwrap().height,
            AUTOMATIONS_PANE_MAX_ROWS
        );
    }

    #[test]
    fn automations_pane_hidden_when_feature_disabled() {
        let with = compute_layout(
            area(100, 30),
            true,
            false,
            false,
            false,
            false,
            true,
            2,
            false,
        );
        let without = compute_layout(
            area(100, 30),
            true,
            false,
            false,
            false,
            false,
            false,
            2,
            false,
        );
        assert!(without.automations_panel.is_none());
        // The session list absorbs the whole left column.
        let full = without.left_panel.unwrap();
        let split = with.left_panel.unwrap();
        assert_eq!(
            full.height,
            split.height + with.automations_panel.unwrap().height
        );
    }

    #[test]
    fn automations_pane_hidden_when_column_too_short() {
        // Content height ≈ 4 rows leaves no room for both lists.
        let areas = compute_layout(
            area(100, 6),
            true,
            false,
            false,
            false,
            false,
            true,
            3,
            false,
        );
        assert!(areas.left_panel.is_some());
        assert!(areas.automations_panel.is_none());
    }

    #[test]
    fn hidden_session_list_drops_left_column_two_panel() {
        // Two-panel width: hiding the list gives the terminal the full content
        // width (no 25% list column).
        let shown = compute_layout(
            area(100, 24),
            true,
            false,
            false,
            false,
            false,
            true,
            0,
            false,
        );
        let hidden = compute_layout(
            area(100, 24),
            false,
            false,
            false,
            false,
            false,
            true,
            0,
            false,
        );
        assert!(shown.left_panel.is_some());
        assert!(hidden.left_panel.is_none());
        assert!(hidden.automations_panel.is_none());
        assert!(hidden.terminal.width > shown.terminal.width);
    }

    #[test]
    fn hidden_session_list_keeps_right_side_panels() {
        // Hiding the left column must not drop the right-side panels — the
        // terminal expands into the list's freed width while info/tasks/files
        // stay put.
        let areas = compute_layout(
            area(160, 24),
            false,
            true,
            true,
            true,
            false,
            true,
            0,
            false,
        );
        assert!(areas.left_panel.is_none(), "list column dropped");
        assert!(areas.automations_panel.is_none());
        assert!(areas.info_panel.is_some(), "info panel survives");
        assert!(areas.tasks_panel.is_some(), "tasks panel survives");
        assert!(areas.file_viewer.is_some(), "file viewer survives");
        let term = areas.terminal;
        // Terminal sits between the info column (left edge now) and tasks.
        let info = areas.info_panel.unwrap();
        let tasks = areas.tasks_panel.unwrap();
        assert!(
            term.x >= info.x + info.width,
            "terminal right of info: {term:?} vs {info:?}"
        );
        assert!(
            tasks.x >= term.x + term.width,
            "tasks right of terminal: {tasks:?} vs {term:?}"
        );
    }

    #[test]
    fn hidden_session_list_three_panel_terminal_widens() {
        // With info open and the list hidden, the terminal reclaims the 18%
        // the list would have reserved.
        let with_list = compute_layout(
            area(160, 24),
            true,
            true,
            false,
            false,
            false,
            true,
            0,
            false,
        )
        .terminal;
        let no_list = compute_layout(
            area(160, 24),
            false,
            true,
            false,
            false,
            false,
            true,
            0,
            false,
        )
        .terminal;
        assert!(
            no_list.width > with_list.width,
            "terminal widens when the list is hidden: {} vs {}",
            no_list.width,
            with_list.width
        );
    }
}
