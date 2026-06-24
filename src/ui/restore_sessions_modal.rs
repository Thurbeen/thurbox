use ratatui::{
    style::Style,
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use super::render_list_modal_frame;
use super::theme::Theme;

/// View-only entry for the restore sessions modal.
pub struct DeletedSessionEntry {
    pub name: String,
    pub agent: String,
    pub deleted_ago: String,
    pub has_worktrees: bool,
    /// Hard-deleted: worktrees + tmux were torn down, so it can't be restored.
    /// Rendered with a danger `force-deleted` tag and dimmed.
    pub force_deleted: bool,
}

pub struct RestoreSessionsModalState<'a> {
    pub entries: &'a [DeletedSessionEntry],
    pub selected_index: usize,
}

pub fn render_restore_sessions_modal(
    frame: &mut Frame,
    state: &RestoreSessionsModalState<'_>,
) -> super::ModalRender {
    let empty_footer = Line::from(vec![
        Span::styled("Esc", Theme::keybind()),
        Span::raw(" close"),
    ]);

    let Some([list_area, footer_area]) = render_list_modal_frame(
        frame,
        60,
        "Restore Deleted Sessions",
        state.entries.len(),
        Some("No deleted sessions"),
        Some(empty_footer),
    ) else {
        return ((Vec::new(), None), Vec::new());
    };

    // Session list — `render_selector_rows` windows the entries around the
    // selection and reserves the rightmost column for a scrollbar when the
    // list overflows.
    let lines: Vec<Line<'_>> = state
        .entries
        .iter()
        .enumerate()
        .map(|(i, entry)| {
            let selected = i == state.selected_index;
            let wt_indicator = if entry.has_worktrees { " [wt]" } else { "" };
            let text = format!(
                " {} ({}) {}{} ",
                entry.name, entry.agent, entry.deleted_ago, wt_indicator
            );
            // A force-deleted row is dimmed (it can't be restored) and tagged.
            let base_style = if selected {
                Theme::selected_item()
            } else if entry.force_deleted {
                Style::default().fg(Theme::text_muted())
            } else {
                Style::default().fg(Theme::text_secondary())
            };
            let mut spans = vec![Span::styled(text, base_style)];
            if entry.force_deleted {
                spans.push(Span::styled(
                    "force-deleted ",
                    Style::default().fg(Theme::danger()),
                ));
            }
            Line::from(spans)
        })
        .collect();

    let hits = super::render_selector_rows(frame, list_area, lines, state.selected_index);

    // Footer: left hint + right-aligned clickable buttons.
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("j/k", Theme::keybind()),
            Span::styled(" navigate", Theme::keybind_desc()),
        ])),
        footer_area,
    );
    let buttons = super::render_action_footer(
        frame,
        footer_area,
        (
            "Restore",
            crossterm::event::KeyCode::Enter,
            crossterm::event::KeyModifiers::NONE,
        ),
        "Close",
    );
    (hits, buttons)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entries(n: usize) -> Vec<DeletedSessionEntry> {
        (0..n)
            .map(|i| DeletedSessionEntry {
                name: format!("session-{i}"),
                agent: "claude".into(),
                deleted_ago: "1m ago".into(),
                has_worktrees: false,
                force_deleted: false,
            })
            .collect()
    }

    fn rendered_text(entries: &[DeletedSessionEntry], selected_index: usize) -> String {
        let backend = ratatui::backend::TestBackend::new(80, 24);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                render_restore_sessions_modal(
                    frame,
                    &RestoreSessionsModalState {
                        entries,
                        selected_index,
                    },
                );
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
        text
    }

    #[test]
    fn selection_beyond_first_page_is_scrolled_into_view() {
        let list = entries(40);
        let text = rendered_text(&list, 39);
        assert!(
            text.contains("session-39"),
            "selected entry should be visible after scrolling:\n{text}"
        );
        assert!(
            !text.contains("session-0 "),
            "first entry should have scrolled out of view:\n{text}"
        );
    }

    #[test]
    fn overflowing_list_reports_windowed_hitboxes_and_scrollbar() {
        let list = entries(40);
        let backend = ratatui::backend::TestBackend::new(80, 24);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        let mut hits: super::super::ModalRender = ((Vec::new(), None), Vec::new());
        terminal
            .draw(|frame| {
                hits = render_restore_sessions_modal(
                    frame,
                    &RestoreSessionsModalState {
                        entries: &list,
                        selected_index: 39,
                    },
                );
            })
            .unwrap();
        let ((rows, geom), _buttons) = hits;
        assert!(geom.is_some(), "overflowing list draws a scrollbar");
        assert!(rows.len() < 40, "only the visible window is clickable");
        assert!(
            rows.iter().any(|r| r.index == 39),
            "the selected row stays clickable"
        );
    }

    #[test]
    fn force_deleted_entry_renders_tag() {
        let list = vec![DeletedSessionEntry {
            name: "gone".into(),
            agent: "claude".into(),
            deleted_ago: "1m ago".into(),
            has_worktrees: false,
            force_deleted: true,
        }];
        let text = rendered_text(&list, 0);
        assert!(
            text.contains("force-deleted"),
            "force-deleted row should be tagged:\n{text}"
        );
    }

    #[test]
    fn short_list_renders_fully_without_scrolling() {
        let list = entries(3);
        let text = rendered_text(&list, 0);
        for entry in &list {
            assert!(
                text.contains(&entry.name),
                "missing {}:\n{text}",
                entry.name
            );
        }
    }
}
