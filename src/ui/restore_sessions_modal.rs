use ratatui::{
    layout::{Alignment, Constraint, Direction, Layout},
    style::Style,
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
    Frame,
};

use super::centered_fixed_height_rect;
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
    let list_height = state.entries.len().max(1) as u16;
    // 2 (borders) + list_height + 1 (footer) + 2 (padding)
    let total_height = (list_height + 5).min(20);
    let area = centered_fixed_height_rect(60, total_height, frame.area());

    frame.render_widget(Clear, area);

    let block = Block::default()
        .title(" Restore Deleted Sessions ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Theme::ACCENT));

    let inner = block.inner(area);
    frame.render_widget(block, area);

    if state.entries.is_empty() {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(1), Constraint::Length(1)])
            .split(inner);

        let empty = Paragraph::new(Line::from(Span::styled(
            "No deleted sessions",
            Style::default().fg(Theme::TEXT_MUTED),
        )))
        .alignment(Alignment::Center);
        frame.render_widget(empty, chunks[0]);

        let help = Line::from(vec![
            Span::styled("Esc", Theme::keybind()),
            Span::raw(" close"),
        ]);
        frame.render_widget(Paragraph::new(help), chunks[1]);
        return;
    }

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(1)])
        .split(inner);

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
                    Style::default().fg(Theme::TEXT_SECONDARY),
                ))
            }
        })
        .collect();

    frame.render_widget(Paragraph::new(lines), chunks[0]);

    // Footer
    let help = Line::from(vec![
        Span::styled("Enter", Theme::keybind()),
        Span::raw(" restore  "),
        Span::styled("Esc", Theme::keybind()),
        Span::raw(" close"),
    ]);
    frame.render_widget(Paragraph::new(help), chunks[1]);
}
