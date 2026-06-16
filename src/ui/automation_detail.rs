//! Run-history panel for the scoped automation.
//!
//! Shown beneath the in-pane automation editor (the central pane while the
//! automations pane / editor is focused): the recent fires of the automation
//! being viewed. All human-formatted strings are computed by the caller (which
//! owns the time helpers); this module only lays them out.

use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
    Frame,
};

use crate::session::AutomationRunStatus;

use super::scrollbar::{self, ScrollbarGeom};
use super::theme::Theme;
use super::FocusLevel;

/// One recorded run, pre-formatted for display.
pub struct AutomationRunRow<'a> {
    pub status: AutomationRunStatus,
    /// Absolute local clock time, e.g. `"06-01 14:03"`.
    pub at: String,
    /// Relative time, e.g. `"2h ago"`.
    pub when: String,
    /// Free-text detail (spawned session id, error, or skip reason).
    pub detail: &'a str,
}

/// Render the run history for an automation into `area` (a bordered panel).
/// When `focus` is [`FocusLevel::Focused`] the panel highlights `selected` and
/// shows a footer with the run-now / navigation shortcuts.
pub fn render_run_history(
    frame: &mut Frame,
    area: Rect,
    runs: &[AutomationRunRow<'_>],
    selected: usize,
    focus: FocusLevel,
) -> Option<ScrollbarGeom> {
    let focused = matches!(focus, FocusLevel::Focused);
    let border_color = if focused {
        Theme::border_focused()
    } else {
        Theme::border_unfocused()
    };
    let block = Block::default()
        .title(" Run history ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(border_color));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    // Reserve the bottom row for the shortcut hint while focused.
    let (list_area, hint_area) = if focused && inner.height >= 2 {
        (
            Rect {
                height: inner.height - 1,
                ..inner
            },
            Some(Rect {
                y: inner.y + inner.height - 1,
                height: 1,
                ..inner
            }),
        )
    } else {
        (inner, None)
    };

    if runs.is_empty() {
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                "no runs yet — press r to run now",
                Style::default().fg(Theme::text_muted()),
            ))),
            list_area,
        );
        return None;
    }

    // Window the runs so the selected row stays visible, reserving the rightmost
    // column for a scrollbar when the history overflows.
    let height = list_area.height as usize;
    let (rows_area, track) = scrollbar::reserve_track(list_area, runs.len(), height);
    let (start, end) = super::file_viewer::visible_window(runs.len(), selected, height.max(1));

    let lines: Vec<Line> = runs[start..end]
        .iter()
        .enumerate()
        .map(|(offset, run)| run_line(run, focused && start + offset == selected))
        .collect();

    frame.render_widget(Paragraph::new(lines), rows_area);

    if let Some(hint_area) = hint_area {
        let hint = Line::from(vec![
            Span::styled("r", Theme::keybind()),
            Span::styled(" run now  ", Theme::keybind_desc()),
            Span::styled("Enter", Theme::keybind()),
            Span::styled(" open session  ", Theme::keybind_desc()),
            Span::styled("j/k", Theme::keybind()),
            Span::styled(" select  ", Theme::keybind_desc()),
            Span::styled("Esc", Theme::keybind()),
            Span::styled(" back", Theme::keybind_desc()),
        ]);
        frame.render_widget(Paragraph::new(hint), hint_area);
    }

    track.and_then(|track| scrollbar::render_into(frame, track, runs.len(), height, selected))
}

/// Build one run-history row: a coloured status badge, the absolute clock time,
/// the relative age, and any free-text detail. Highlighted when `is_selected`.
fn run_line<'a>(run: &AutomationRunRow<'_>, is_selected: bool) -> Line<'a> {
    let (glyph, word, color) = match run.status {
        AutomationRunStatus::Success => ("✓", "ok", Theme::status_idle()),
        AutomationRunStatus::Error => ("✗", "error", Theme::status_error()),
        AutomationRunStatus::Skipped => ("–", "skipped", Theme::status_waiting()),
    };
    let pointer = if is_selected { "▸" } else { " " };
    let mut spans = vec![
        // Status — colour + bold so each outcome stands out.
        Span::styled(
            format!("{pointer}{glyph} {word:<7}"),
            Style::default().fg(color).add_modifier(Modifier::BOLD),
        ),
        // Absolute clock time, then the relative age.
        Span::styled(
            format!("{:<11} ", run.at),
            Style::default().fg(Theme::text_secondary()),
        ),
        Span::styled(
            format!("{:<9} ", run.when),
            Style::default().fg(Theme::text_muted()),
        ),
    ];
    if !run.detail.is_empty() {
        spans.push(Span::styled(
            run.detail.to_string(),
            Style::default().fg(Theme::text_primary()),
        ));
    }
    let line = Line::from(spans);
    if is_selected {
        line.style(Style::default().bg(Theme::selection_bg()))
    } else {
        line
    }
}
