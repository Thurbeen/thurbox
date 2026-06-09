//! Modal that lets the user pick a theme (built-in preset or custom).
//!
//! Renders the preset list on the left and a small live colour swatch for
//! the highlighted preset on the right, so users can preview the palette
//! before committing.

use ratatui::{
    layout::{Constraint, Direction, Layout},
    style::Style,
    text::{Line, Span},
    widgets::{List, ListItem, Paragraph},
    Frame,
};

use super::{
    centered_fixed_height_rect, render_modal_frame, selector_list_item, selector_nav_footer,
    theme::Theme,
};
use crate::session::theme_config::ThemeEntry;

pub struct ThemePickerState<'a> {
    pub entries: &'a [ThemeEntry],
    pub selected_index: usize,
}

pub fn render_theme_picker_modal(frame: &mut Frame, state: &ThemePickerState<'_>) {
    let height = (state.entries.len() as u16).max(4) + 6;
    let area = centered_fixed_height_rect(60, height, frame.area());

    let inner = render_modal_frame(frame, area, "Theme");

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(1),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .split(inner);

    let columns = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(45), Constraint::Percentage(55)])
        .split(chunks[0]);

    let items: Vec<ListItem<'_>> = state
        .entries
        .iter()
        .enumerate()
        .map(|(i, entry)| {
            let label = if entry.is_light {
                format!("{} (light)", entry.display_name)
            } else {
                entry.display_name.clone()
            };
            selector_list_item(&label, i == state.selected_index)
        })
        .collect();

    frame.render_widget(List::new(items), columns[0]);

    if let Some(entry) = state.entries.get(state.selected_index) {
        let palette = &entry.palette;
        let swatch = vec![
            Line::from(vec![
                Span::styled("  Accent      ", Style::default().fg(Theme::text_muted())),
                Span::styled("█████", Style::default().fg(palette.accent)),
                Span::styled(" ", Style::default()),
                Span::styled("█████", Style::default().fg(palette.accent_bright)),
            ]),
            Line::from(vec![
                Span::styled("  Status      ", Style::default().fg(Theme::text_muted())),
                Span::styled("●", Style::default().fg(palette.status_busy)),
                Span::styled(" ", Style::default()),
                Span::styled("◉", Style::default().fg(palette.status_waiting)),
                Span::styled(" ", Style::default()),
                Span::styled("○", Style::default().fg(palette.status_idle)),
                Span::styled(" ", Style::default()),
                Span::styled("✗", Style::default().fg(palette.status_error)),
            ]),
            Line::from(vec![
                Span::styled("  Border      ", Style::default().fg(Theme::text_muted())),
                Span::styled("╭─────╮", Style::default().fg(palette.border_focused)),
            ]),
            Line::from(vec![
                Span::styled("  Modal bg    ", Style::default().fg(Theme::text_muted())),
                Span::styled(
                    "         ",
                    Style::default()
                        .bg(palette.modal_bg)
                        .fg(palette.text_primary),
                ),
            ]),
        ];
        frame.render_widget(Paragraph::new(swatch), columns[1]);

        let id = Line::from(Span::styled(
            format!("  theme id: {}", entry.name),
            Style::default().fg(Theme::text_muted()),
        ));
        frame.render_widget(Paragraph::new(id), chunks[1]);
    }

    frame.render_widget(Paragraph::new(selector_nav_footer()), chunks[2]);
}
