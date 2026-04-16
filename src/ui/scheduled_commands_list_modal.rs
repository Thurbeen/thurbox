use ratatui::{
    style::Style,
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use super::render_list_modal_frame;
use super::theme::Theme;

/// View-only entry for the scheduled commands list modal.
pub struct ScheduledCommandsListEntry {
    pub session_name: String,
    pub command_preview: String,
    pub countdown: String,
}

pub struct ScheduledCommandsListState<'a> {
    pub entries: &'a [ScheduledCommandsListEntry],
    pub selected_index: usize,
}

pub fn render_scheduled_commands_list_modal(
    frame: &mut Frame,
    state: &ScheduledCommandsListState<'_>,
) {
    let empty_footer = Line::from(vec![
        Span::styled("n", Theme::keybind()),
        Span::styled(" new  ", Theme::keybind_desc()),
        Span::styled("Esc", Theme::keybind()),
        Span::styled(" close", Theme::keybind_desc()),
    ]);

    let Some([list_area, footer_area]) = render_list_modal_frame(
        frame,
        70,
        "Scheduled Commands",
        state.entries.len(),
        Some("No pending scheduled commands"),
        Some(empty_footer),
    ) else {
        return;
    };

    let inner_width = list_area.width as usize;

    let lines: Vec<Line<'_>> = state
        .entries
        .iter()
        .enumerate()
        .map(|(i, entry)| {
            let selected = i == state.selected_index;
            let prefix = format!("[{}] ", entry.session_name);
            let suffix = format!(" {}", entry.countdown);
            let cmd_max = inner_width
                .saturating_sub(prefix.chars().count())
                .saturating_sub(suffix.chars().count())
                .saturating_sub(2);
            let cmd = if entry.command_preview.chars().count() > cmd_max {
                let truncated: String = entry
                    .command_preview
                    .chars()
                    .take(cmd_max.saturating_sub(3))
                    .collect();
                format!("{truncated}...")
            } else {
                entry.command_preview.clone()
            };
            let text = format!(" {prefix}{cmd}{suffix} ");
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

    let help = Line::from(vec![
        Span::styled("n", Theme::keybind()),
        Span::styled(" new  ", Theme::keybind_desc()),
        Span::styled("e", Theme::keybind()),
        Span::styled(" edit  ", Theme::keybind_desc()),
        Span::styled("Enter", Theme::keybind()),
        Span::styled(" cancel  ", Theme::keybind_desc()),
        Span::styled("Esc", Theme::keybind()),
        Span::styled(" close", Theme::keybind_desc()),
    ]);
    frame.render_widget(Paragraph::new(help), footer_area);
}
