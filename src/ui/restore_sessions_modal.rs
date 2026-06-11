use ratatui::{
    style::Style,
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use super::render_list_modal_frame;
use super::scrollbar;
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

pub fn render_restore_sessions_modal(frame: &mut Frame, state: &RestoreSessionsModalState<'_>) {
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
        return;
    };

    // Session list — window the entries so the selected row stays visible,
    // reserving the rightmost column for a scrollbar when the list overflows.
    let height = list_area.height as usize;
    let (rows_area, track) = scrollbar::reserve_track(list_area, state.entries.len(), height);
    let (start, end) = super::file_viewer::visible_window(
        state.entries.len(),
        state.selected_index,
        height.max(1),
    );

    let lines: Vec<Line<'_>> = state.entries[start..end]
        .iter()
        .enumerate()
        .map(|(offset, entry)| {
            let selected = start + offset == state.selected_index;
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

    frame.render_widget(Paragraph::new(lines), rows_area);

    if let Some(track) = track {
        scrollbar::render_into(
            frame,
            track,
            state.entries.len(),
            height,
            state.selected_index,
        );
    }

    // Footer
    let help = Line::from(vec![
        Span::styled("Enter", Theme::keybind()),
        Span::raw(" restore  "),
        Span::styled("Esc", Theme::keybind()),
        Span::raw(" close"),
    ]);
    frame.render_widget(Paragraph::new(help), footer_area);
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
