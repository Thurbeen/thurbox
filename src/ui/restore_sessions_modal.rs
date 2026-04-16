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
    pub role: String,
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

    // Session list
    let lines: Vec<Line<'_>> = state
        .entries
        .iter()
        .enumerate()
        .map(|(i, entry)| {
            let selected = i == state.selected_index;
            let wt_indicator = if entry.has_worktrees { " [wt]" } else { "" };
            let text = format!(
                " {} ({}) {}{} ",
                entry.name, entry.role, entry.deleted_ago, wt_indicator
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

    frame.render_widget(Paragraph::new(lines), list_area);

    // Footer
    let help = Line::from(vec![
        Span::styled("Enter", Theme::keybind()),
        Span::raw(" restore  "),
        Span::styled("Esc", Theme::keybind()),
        Span::raw(" close"),
    ]);
    frame.render_widget(Paragraph::new(help), footer_area);
}
