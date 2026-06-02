//! Details panel for the scoped task.
//!
//! Shown beneath the in-pane task editor (the central pane while the tasks
//! panel / editor is focused): the task's agent linkage, status, source, and
//! timestamps. Tasks have no run history, so this read-only panel takes its
//! place. All human-formatted strings are computed by the caller (which owns
//! the time helpers); this module only lays them out.

use ratatui::{
    layout::Rect,
    style::Style,
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
    Frame,
};

use super::theme::Theme;

/// Pre-formatted task details for display.
pub struct TaskDetail<'a> {
    /// How the task connects to an agent, e.g. `"send → claude(feat/x)"`,
    /// `"spawn → repo#branch"`, or `"local todo"`.
    pub linkage: String,
    pub status: &'a str,
    pub source: &'a str,
    /// Relative created time, e.g. `"2d ago"`.
    pub created: String,
    /// Relative updated time, e.g. `"5m ago"`.
    pub updated: String,
}

/// Render the task details into `area` (a bordered, read-only panel).
pub fn render_task_detail(frame: &mut Frame, area: Rect, detail: &TaskDetail<'_>) {
    let block = Block::default()
        .title(" Details ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Theme::border_unfocused()));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.height == 0 {
        return;
    }

    let rows = [
        ("agent", detail.linkage.clone()),
        ("status", detail.status.to_string()),
        ("source", detail.source.to_string()),
        ("created", detail.created.clone()),
        ("updated", detail.updated.clone()),
    ];
    let lines: Vec<Line> = rows
        .iter()
        .map(|(label, value)| {
            Line::from(vec![
                Span::styled(format!("  {label:<9}"), Theme::label()),
                Span::styled(value.clone(), Style::default().fg(Theme::text_primary())),
            ])
        })
        .collect();
    frame.render_widget(Paragraph::new(lines), inner);
}
