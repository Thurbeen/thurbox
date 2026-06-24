//! Read-only details/preview panel for the scoped task.
//!
//! Fills the central pane while the tasks panel is focused: the task title
//! (bold header), its agent linkage, status, source, and timestamps, then the
//! markdown-rendered description, and an optional key-hint footer. All
//! human-formatted strings are computed by the caller (which owns the time
//! helpers); this module only lays them out.

use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Wrap},
    Frame,
};

use super::scrollbar::{self, ScrollbarGeom};
use super::theme::Theme;
use super::truncate_ellipsis;

/// Pre-formatted task details for display.
pub struct TaskDetail<'a> {
    /// The task title, shown as a bold header above the metadata.
    pub title: &'a str,
    /// How the task connects to an agent, e.g. `"send → claude(feat/x)"`,
    /// `"spawn → repo#branch"`, or `"local todo"`.
    pub linkage: String,
    /// The task's currently-open related session(s) — names joined with `, `, or
    /// a muted placeholder when none are open. Shown as its own metadata row so
    /// the live session behind a task is visible (and `o`-openable) at a glance.
    pub sessions: String,
    pub status: &'a str,
    pub source: &'a str,
    /// Raw markdown description; rendered beneath the metadata rows. Empty hides
    /// the section.
    pub description: &'a str,
    /// Relative created time, e.g. `"2d ago"`.
    pub created: String,
    /// Relative updated time, e.g. `"5m ago"`.
    pub updated: String,
}

/// Render the task details into `area` (a bordered, read-only panel). When the
/// task has a description it is rendered as markdown beneath the metadata rows,
/// scrolled vertically by `scroll` rows (for the full-screen preview).
///
/// `hints` are `(key, label)` pairs shown as a footer inside the border (e.g.
/// the tasks-panel actions); pass an empty slice to omit the footer.
pub fn render_task_detail(
    frame: &mut Frame,
    area: Rect,
    detail: &TaskDetail<'_>,
    scroll: u16,
    hints: &[(&str, &str)],
) -> Option<ScrollbarGeom> {
    // The task title *is* the panel heading (bold accent in the border) — no
    // separate in-content title row, so there is one clear heading.
    let title = truncate_ellipsis(detail.title, area.width.saturating_sub(4) as usize);
    let block = Block::default()
        .title(Span::styled(
            format!(" {title} "),
            Style::default()
                .fg(Theme::accent())
                .add_modifier(Modifier::BOLD),
        ))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Theme::border_unfocused()));
    let mut inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.height == 0 {
        return None;
    }

    // Reserve the bottom row for the key-hint footer when there's room.
    if !hints.is_empty() && inner.height > 2 {
        let hint_area = Rect::new(inner.x, inner.y + inner.height - 1, inner.width, 1);
        frame.render_widget(Paragraph::new(super::key_hint_line(hints)), hint_area);
        inner.height -= 1;
    }

    // Metadata labels use the mid `text_secondary` tone (not the dimmest
    // `label()`/muted) so the column stays legible.
    let label_style = Style::default().fg(Theme::text_secondary());
    let rows = [
        ("agent", detail.linkage.clone()),
        ("session", detail.sessions.clone()),
        ("status", detail.status.to_string()),
        ("source", detail.source.to_string()),
        ("created", detail.created.clone()),
        ("updated", detail.updated.clone()),
    ];
    let mut meta_lines: Vec<Line> = rows
        .iter()
        .map(|(label, value)| {
            Line::from(vec![
                Span::styled(format!("  {label:<9}"), label_style),
                Span::styled(value.clone(), Style::default().fg(Theme::text_primary())),
            ])
        })
        .collect();

    if detail.description.trim().is_empty() {
        // Fill the otherwise-blank pane with a muted call-to-action. Only invite
        // editing when the panel is actually focused (hints present); a global-
        // search preview can't edit.
        meta_lines.push(Line::default());
        let cta = if hints.is_empty() {
            "  No description."
        } else {
            "  No description — press e to add notes."
        };
        meta_lines.push(Line::from(Span::styled(
            cta,
            Style::default().fg(Theme::text_muted()),
        )));
        frame.render_widget(Paragraph::new(meta_lines), inner);
        return None;
    }

    let meta_h = (meta_lines.len() as u16 + 1).min(inner.height);
    let parts = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(meta_h), Constraint::Min(0)])
        .split(inner);
    frame.render_widget(Paragraph::new(meta_lines), parts[0]);

    let mut md = vec![Line::from(Span::styled("  description", label_style))];
    md.extend(super::markdown::render_markdown(detail.description));
    let content_len = md.len();
    let md_area = parts[1];
    let viewport = md_area.height as usize;

    // Reserve the rightmost column for the scrollbar when the description
    // overflows, so the wrapped text never renders under the thumb.
    let (text_area, track) = scrollbar::reserve_track(md_area, content_len, viewport);
    frame.render_widget(
        Paragraph::new(md)
            .wrap(Wrap { trim: false })
            .scroll((scroll, 0)),
        text_area,
    );
    track.and_then(|track| {
        scrollbar::render_into(frame, track, content_len, viewport, scroll as usize)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn detail(description: &'static str) -> TaskDetail<'static> {
        TaskDetail {
            title: "Ship the feature",
            linkage: "spawn → repo#feat/x".to_string(),
            // Muted placeholder; deliberately free of the word "open" so the
            // footer-hint assertion below can't be satisfied by this row.
            sessions: "—".to_string(),
            status: "in progress",
            source: "local",
            description,
            created: "2d ago".to_string(),
            updated: "5m ago".to_string(),
        }
    }

    fn render(detail: &TaskDetail<'_>, scroll: u16) -> (String, Option<ScrollbarGeom>) {
        let backend = ratatui::backend::TestBackend::new(80, 24);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        let mut geom = None;
        terminal
            .draw(|frame| {
                let area = frame.area();
                geom = render_task_detail(frame, area, detail, scroll, &[("o", "open")]);
            })
            .unwrap();
        let buffer = terminal.backend().buffer();
        let area = *buffer.area();
        let mut text = String::new();
        for y in 0..area.height {
            for x in 0..area.width {
                text.push_str(buffer[(x, y)].symbol());
            }
            text.push('\n');
        }
        (text, geom)
    }

    #[test]
    fn renders_title_metadata_and_hint() {
        let (text, geom) = render(&detail("a short note"), 0);
        assert!(text.contains("Ship the feature"), "title is the heading");
        assert!(text.contains("in progress"), "status row is shown");
        assert!(text.contains("repo#feat/x"), "linkage row is shown");
        assert!(text.contains("a short note"), "description is rendered");
        assert!(text.contains("open"), "footer hint is shown");
        assert!(
            geom.is_none(),
            "a short description does not overflow → no scrollbar"
        );
    }

    #[test]
    fn long_description_overflows_and_draws_a_scrollbar() {
        let long: &'static str = "line\n".repeat(80).leak();
        let (_text, geom) = render(&detail(long), 0);
        assert!(
            geom.is_some(),
            "a description taller than the viewport reserves a scrollbar track"
        );
    }
}
