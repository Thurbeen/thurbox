//! Persistent, focusable automations pane shown beneath the session list.
//!
//! Read-and-act: it mirrors `cached_automations` and, when focused, drives the
//! same toggle/run/edit/delete actions as the Ctrl+P modal.

use ratatui::{
    layout::Rect,
    style::Style,
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
    Frame,
};

use super::theme::Theme;
use super::{truncate_ellipsis, FocusLevel};

/// One row in the automations pane.
pub struct AutomationPaneEntry {
    pub name: String,
    /// e.g. `"daily · spawn · in 3h"`.
    pub summary: String,
    pub enabled: bool,
    /// Byte offsets in `name` matched by the active global-search query.
    pub match_positions: Vec<usize>,
    /// When a global search is active, rows that don't match are dimmed.
    pub dimmed: bool,
}

pub struct AutomationsPaneState<'a> {
    pub entries: &'a [AutomationPaneEntry],
    pub selected: usize,
    pub focus: FocusLevel,
    /// While global search previews an automation here, show the selected row
    /// highlighted (not dimmed) even though the pane isn't focused.
    pub preview_selected: bool,
}

pub fn render_automations_pane(
    frame: &mut Frame,
    area: Rect,
    state: &AutomationsPaneState<'_>,
) -> Vec<super::RowHitbox> {
    let border_color = match state.focus {
        FocusLevel::Focused => Theme::border_focused(),
        _ => Theme::border_unfocused(),
    };
    let block = Block::default()
        .title(" Automations ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(border_color));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if state.entries.is_empty() {
        render_empty(frame, inner, state.focus);
        return Vec::new();
    }

    let width = inner.width as usize;
    let focused = matches!(state.focus, FocusLevel::Focused);

    // Scroll the selected row into view, mirroring the session list: window the
    // rows around `selected` so a selection below the visible area stays
    // visible. Reuses the shared file-tree/run-history windowing helper.
    let selected = state.selected.min(state.entries.len() - 1);
    let (start, end) =
        super::file_viewer::visible_window(state.entries.len(), selected, inner.height as usize);

    let lines: Vec<Line> = state.entries[start..end]
        .iter()
        .enumerate()
        .map(|(row, e)| {
            let i = start + row;
            let is_selected = (focused || state.preview_selected) && i == state.selected;
            entry_line(e, is_selected, width)
        })
        .collect();

    frame.render_widget(Paragraph::new(lines), inner);

    // One single-line hitbox per visible row, indexed in entry space.
    super::windowed_row_hitboxes(inner, start, end)
}

/// Render the placeholder shown when there are no automations.
fn render_empty(frame: &mut Frame, inner: Rect, focus: FocusLevel) {
    let text = if matches!(focus, FocusLevel::Focused) {
        "none — Ctrl+N to add"
    } else {
        "none"
    };
    let hint = Paragraph::new(Line::from(Span::styled(
        text,
        Style::default().fg(Theme::text_muted()),
    )));
    frame.render_widget(hint, inner);
}

/// Build one automation row: an enabled marker, the (fuzzy-highlighted) name,
/// and a dim summary tail, fitted to `width`.
fn entry_line<'a>(e: &AutomationPaneEntry, selected: bool, width: usize) -> Line<'a> {
    let marker = if e.enabled { "●" } else { "○" };
    let prefix = format!(" {marker} ");
    let tail = format!(" — {} ", e.summary);
    // Reserve room for the prefix + tail so the (highlighted) name fits.
    let name_budget = width
        .saturating_sub(prefix.chars().count())
        .saturating_sub(tail.chars().count());
    let name = truncate_ellipsis(&e.name, name_budget);

    // Matched characters are layered on top of this base (see `row_base_style`),
    // so a selected/previewed row keeps its fuzzy-match highlight (mirrors the
    // session list + tasks pane).
    let normal = Style::default().fg(if e.enabled {
        Theme::text_secondary()
    } else {
        Theme::text_muted()
    });
    let base = super::highlight::row_base_style(selected, e.dimmed, normal);
    let mut spans = vec![Span::styled(prefix, base)];
    let positions: &[usize] = if e.dimmed { &[] } else { &e.match_positions };
    spans.extend(super::highlight::highlighted_spans_owned(
        &name, positions, base,
    ));
    spans.push(Span::styled(tail, base));
    Line::from(spans)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{backend::TestBackend, Terminal};

    fn entry(name: &str) -> AutomationPaneEntry {
        AutomationPaneEntry {
            name: name.into(),
            summary: "daily".into(),
            enabled: true,
            match_positions: vec![],
            dimmed: false,
        }
    }

    fn hitboxes(count: usize, selected: usize) -> Vec<super::super::RowHitbox> {
        let backend = TestBackend::new(30, 5);
        let mut terminal = Terminal::new(backend).unwrap();
        let entries: Vec<AutomationPaneEntry> =
            (0..count).map(|i| entry(&format!("a{i}"))).collect();
        let mut rows = Vec::new();
        terminal
            .draw(|f| {
                rows = render_automations_pane(
                    f,
                    Rect::new(0, 0, 30, 5),
                    &AutomationsPaneState {
                        entries: &entries,
                        selected,
                        focus: FocusLevel::Focused,
                        preview_selected: false,
                    },
                );
            })
            .unwrap();
        rows
    }

    #[test]
    fn rows_render_from_top_when_everything_fits() {
        // 5 outer rows → 3 inner; two automations fit without scrolling.
        let rows = hitboxes(2, 0);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].rect, Rect::new(1, 1, 28, 1));
        assert_eq!(rows[0].index, 0);
        assert_eq!(rows[1].index, 1);
    }

    #[test]
    fn selection_below_viewport_scrolls_into_view() {
        // 10 automations, only 3 inner rows: a selection near the bottom must
        // be windowed into view rather than clipped off-screen.
        let rows = hitboxes(10, 9);
        assert_eq!(rows.len(), 3);
        let visible: Vec<usize> = rows.iter().map(|r| r.index).collect();
        assert!(
            visible.contains(&9),
            "selected row 9 should be windowed into view, got {visible:?}"
        );
        assert!(!visible.contains(&0));
    }
}
