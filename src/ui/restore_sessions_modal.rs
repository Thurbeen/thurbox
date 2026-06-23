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
            if selected {
                Line::from(Span::styled(text, Theme::selected_item()))
            } else {
                Line::from(Span::styled(
                    text,
                    Style::default().fg(Theme::text_secondary()),
                ))
            }
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
    let button_hits = super::render_button_bar(
        frame,
        footer_area,
        &[
            super::ButtonSpec::primary("Restore"),
            super::ButtonSpec::secondary("Close"),
        ],
        true,
    );
    let buttons = super::modal_button_keys(
        button_hits,
        &[
            (
                crossterm::event::KeyCode::Enter,
                crossterm::event::KeyModifiers::NONE,
            ),
            (
                crossterm::event::KeyCode::Esc,
                crossterm::event::KeyModifiers::NONE,
            ),
        ],
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
