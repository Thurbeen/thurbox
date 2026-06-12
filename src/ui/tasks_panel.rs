//! Focusable tasks panel — a toggleable right-side column (between the terminal
//! and the file viewer), shown/hidden with F5/`Ctrl+W` like the file viewer.
//! Renders a checkbox list of task titles (☐/◐/☑), with global-search matches
//! highlighted; while focused it shows a compact action footer. Filtering is
//! handled by the global `Ctrl+A` search, not a per-pane box.

use ratatui::{
    layout::Rect,
    style::Style,
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use crate::session::TaskStatus;

use super::theme::Theme;
use super::{focus_block, truncate_ellipsis, FocusLevel};

/// One row in the tasks panel (view data built by the app layer).
pub struct TaskPaneEntry {
    pub title: String,
    pub status: TaskStatus,
    /// Byte offsets in `title` matched by the active global-search query. Empty
    /// when there's no global search (or this row didn't match).
    pub match_positions: Vec<usize>,
    /// When a global search is active, rows that don't match are dimmed.
    pub dimmed: bool,
    /// The task has at least one currently-open related session (a spawned
    /// `task-<id>-<slug>` window or a Send target). Drawn as a trailing `⇄`
    /// marker so a live task is glanceable in the list; press `o` to jump to it.
    pub linked: bool,
}

/// Trailing marker shown on rows whose task has an open related session.
const LINKED_MARKER: &str = " ⇄";

pub struct TaskPaneState<'a> {
    pub entries: &'a [TaskPaneEntry],
    pub selected: usize,
    pub focus: FocusLevel,
    /// While global search previews a task here, show the selected row
    /// highlighted (not dimmed) even though the panel isn't focused.
    pub preview_selected: bool,
}

pub fn render_tasks_panel(
    frame: &mut Frame,
    area: Rect,
    state: &TaskPaneState<'_>,
) -> Vec<super::RowHitbox> {
    // Shared focus block: highlighted title + accent/rounded border when focused,
    // matching the session list and file viewer panes.
    let block = focus_block(" Tasks ", state.focus);
    let mut inner = block.inner(area);
    frame.render_widget(block, area);

    if inner.height == 0 {
        return Vec::new();
    }

    // While focused, reserve the bottom row for a compact action footer that
    // mirrors the full-screen preview's hints (so `r run` is discoverable here
    // too, not only in the central pane).
    if matches!(state.focus, FocusLevel::Focused) && inner.height > 2 {
        let hint_area = Rect::new(inner.x, inner.y + inner.height - 1, inner.width, 1);
        frame.render_widget(
            // Same relative order as the central preview footer (e · r · n).
            Paragraph::new(super::key_hint_line(&[
                ("e", " edit "),
                ("r", " run "),
                ("n", " new "),
            ])),
            hint_area,
        );
        inner.height -= 1;
    }

    render_task_list(frame, inner, state);
    // One single-line hitbox per visible row (the hint footer, when present,
    // was already subtracted from `inner`).
    super::single_line_row_hitboxes(inner, state.entries.len())
}

fn render_task_list(frame: &mut Frame, area: Rect, state: &TaskPaneState<'_>) {
    if state.entries.is_empty() {
        let text = if matches!(state.focus, FocusLevel::Focused) {
            "no tasks — n to add"
        } else {
            "no tasks"
        };
        let hint = Paragraph::new(Line::from(Span::styled(
            text,
            Style::default().fg(Theme::text_muted()),
        )));
        frame.render_widget(hint, area);
        return;
    }

    let width = area.width as usize;
    let focused = matches!(state.focus, FocusLevel::Focused);

    let lines: Vec<Line> = state
        .entries
        .iter()
        .enumerate()
        .map(|(i, e)| {
            // Highlight the selected row when the panel is focused, OR when a
            // global-search preview points here (so the moving cursor is visible
            // even though focus is in the search box).
            let selected = (focused || state.preview_selected) && i == state.selected;
            task_row_line(e, selected, width)
        })
        .collect();

    frame.render_widget(Paragraph::new(lines), area);
}

/// Build one task row: `<glyph> <title>` with global-search highlighting, plus a
/// trailing `⇄` marker when the task has a live related session.
fn task_row_line(e: &TaskPaneEntry, selected: bool, width: usize) -> Line<'_> {
    let glyph = status_glyph(e.status);
    // The glyph is its own span; the title follows so highlight byte offsets
    // (which index `title`) line up. Reserve room for the trailing link marker
    // so it never pushes the title off-row.
    let reserved = if e.linked {
        2 + LINKED_MARKER.chars().count()
    } else {
        2
    };
    let title = truncate_ellipsis(&e.title, width.saturating_sub(reserved));

    // Matched characters are layered *on top* of this base (see `row_base_style`),
    // the same way the session list does it — so a selected/previewed row still
    // shows its fuzzy-match highlight.
    let base = super::highlight::row_base_style(
        selected,
        e.dimmed,
        Style::default().fg(status_color(e.status)),
    );
    let mut spans = vec![Span::styled(format!("{glyph} "), base)];
    // Highlight matched chars on every non-dimmed row (including the selected
    // one); dimmed rows didn't match, so no positions.
    let positions: &[usize] = if e.dimmed { &[] } else { &e.match_positions };
    spans.extend(super::highlight::highlighted_spans_owned(
        &title, positions, base,
    ));
    // Trailing accent marker for tasks with a live session. Dimmed rows
    // (non-matches during a search) keep the dim tone for consistency.
    if e.linked {
        let marker_style = if e.dimmed {
            base
        } else {
            Style::default().fg(Theme::accent())
        };
        spans.push(Span::styled(LINKED_MARKER, marker_style));
    }
    Line::from(spans)
}

fn status_glyph(status: TaskStatus) -> &'static str {
    match status {
        TaskStatus::Todo => "☐",
        TaskStatus::InProgress => "◐",
        TaskStatus::Done => "☑",
    }
}

fn status_color(status: TaskStatus) -> ratatui::style::Color {
    match status {
        TaskStatus::Todo => Theme::text_primary(),
        TaskStatus::InProgress => Theme::accent(),
        TaskStatus::Done => Theme::text_muted(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{backend::TestBackend, Terminal};

    fn entry(title: &str) -> TaskPaneEntry {
        TaskPaneEntry {
            title: title.into(),
            status: TaskStatus::Todo,
            match_positions: vec![],
            dimmed: false,
            linked: false,
        }
    }

    fn hitboxes(focus: FocusLevel, count: usize) -> Vec<super::super::RowHitbox> {
        let backend = TestBackend::new(20, 6);
        let mut terminal = Terminal::new(backend).unwrap();
        let entries: Vec<TaskPaneEntry> = (0..count).map(|i| entry(&format!("t{i}"))).collect();
        let mut rows = Vec::new();
        terminal
            .draw(|f| {
                rows = render_tasks_panel(
                    f,
                    Rect::new(0, 0, 20, 6),
                    &TaskPaneState {
                        entries: &entries,
                        selected: 0,
                        focus,
                        preview_selected: false,
                    },
                );
            })
            .unwrap();
        rows
    }

    #[test]
    fn row_hitboxes_start_below_border() {
        let rows = hitboxes(FocusLevel::Inactive, 2);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].rect, Rect::new(1, 1, 18, 1));
        assert_eq!(rows[1].rect, Rect::new(1, 2, 18, 1));
        assert_eq!(rows[1].index, 1);
    }

    #[test]
    fn focused_panel_hitboxes_exclude_hint_footer() {
        // 6 outer rows → 4 inner; while focused the bottom inner row is the
        // action footer, so only 3 rows are clickable.
        let rows = hitboxes(FocusLevel::Focused, 10);
        assert_eq!(rows.len(), 3);
        assert_eq!(rows.last().unwrap().rect.y, 3);
    }
}
