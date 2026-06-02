//! Focusable tasks panel — a toggleable right-side column (between the terminal
//! and the file viewer), shown/hidden with F5/`Ctrl+W` like the file viewer.
//! Renders a fuzzy-search box over a checkbox list of tasks, mirroring the
//! repo-picker search bar.

use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
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
    /// e.g. `"→ send: 1a2b3c"`, `"→ spawn: repo#branch"`, or `"local"`.
    pub action_summary: String,
    /// Byte offsets in `title` matched by the active global-search query. Empty
    /// when there's no global search (or this row didn't match).
    pub match_positions: Vec<usize>,
    /// When a global search is active, rows that don't match are dimmed.
    pub dimmed: bool,
}

pub struct TaskPaneState<'a> {
    pub entries: &'a [TaskPaneEntry],
    pub selected: usize,
    pub focus: FocusLevel,
    /// While global search previews a task here, show the selected row
    /// highlighted (not dimmed) even though the panel isn't focused.
    pub preview_selected: bool,
}

pub fn render_tasks_panel(frame: &mut Frame, area: Rect, state: &TaskPaneState<'_>) {
    // Shared focus block: highlighted title + accent/rounded border when focused,
    // matching the session list and file viewer panes.
    let block = focus_block(" Tasks ", state.focus);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if inner.height == 0 {
        return;
    }

    render_task_list(frame, inner, state);
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
            let glyph = status_glyph(e.status);
            // The glyph is its own span; the title follows so highlight byte
            // offsets (which index `title`) line up.
            let title = truncate_ellipsis(&e.title, width.saturating_sub(2));

            // Matched characters are layered *on top* of this base (see
            // `row_base_style`), the same way the session list does it — so a
            // selected/previewed row still shows its fuzzy-match highlight.
            let base = super::highlight::row_base_style(
                selected,
                e.dimmed,
                Style::default().fg(status_color(e.status)),
            );
            let mut spans = vec![Span::styled(format!("{glyph} "), base)];
            // Highlight matched chars on every non-dimmed row (including the
            // selected one); dimmed rows didn't match, so no positions.
            let positions: &[usize] = if e.dimmed { &[] } else { &e.match_positions };
            spans.extend(super::highlight::highlighted_spans_owned(
                &title, positions, base,
            ));
            Line::from(spans)
        })
        .collect();

    frame.render_widget(Paragraph::new(lines), area);

    // Action summary of the selected task on the last row, when there's space.
    if let Some(sel) = state.entries.get(state.selected) {
        if area.height > state.entries.len() as u16 {
            let summary_area =
                Rect::new(area.x, area.y + state.entries.len() as u16, area.width, 1);
            let summary = Paragraph::new(Line::from(Span::styled(
                truncate_ellipsis(&format!(" {}", sel.action_summary), width),
                Style::default()
                    .fg(Theme::text_muted())
                    .add_modifier(Modifier::ITALIC),
            )));
            frame.render_widget(summary, summary_area);
        }
    }
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
