use ratatui::{
    style::Style,
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use super::render_list_modal_frame;
use super::theme::Theme;

/// View-only entry for the automations list modal.
pub struct AutomationsListEntry {
    pub name: String,
    pub summary: String,
    pub enabled: bool,
}

pub struct AutomationsListState<'a> {
    pub entries: &'a [AutomationsListEntry],
    pub selected_index: usize,
}

pub fn render_automations_list_modal(frame: &mut Frame, state: &AutomationsListState<'_>) {
    let empty_footer = Line::from(vec![
        Span::styled("n", Theme::keybind()),
        Span::styled(" new  ", Theme::keybind_desc()),
        Span::styled("Esc", Theme::keybind()),
        Span::styled(" close", Theme::keybind_desc()),
    ]);

    let Some([list_area, footer_area]) = render_list_modal_frame(
        frame,
        70,
        "Automations",
        state.entries.len(),
        Some("No automations — press n to create one"),
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
            let marker = if entry.enabled { "●" } else { "○" };
            let text = truncate(
                &format!(" {marker} {} — {} ", entry.name, entry.summary),
                inner_width,
            );
            if selected {
                Line::from(Span::styled(text, Theme::selected_item()))
            } else {
                let color = if entry.enabled {
                    Theme::text_secondary()
                } else {
                    Theme::text_muted()
                };
                Line::from(Span::styled(text, Style::default().fg(color)))
            }
        })
        .collect();

    frame.render_widget(Paragraph::new(lines), list_area);

    let help = Line::from(vec![
        Span::styled("n", Theme::keybind()),
        Span::styled(" new  ", Theme::keybind_desc()),
        Span::styled("e", Theme::keybind()),
        Span::styled(" edit  ", Theme::keybind_desc()),
        Span::styled("Spc", Theme::keybind()),
        Span::styled(" toggle  ", Theme::keybind_desc()),
        Span::styled("r", Theme::keybind()),
        Span::styled(" run  ", Theme::keybind_desc()),
        Span::styled("d", Theme::keybind()),
        Span::styled(" delete  ", Theme::keybind_desc()),
        Span::styled("Esc", Theme::keybind()),
        Span::styled(" close", Theme::keybind_desc()),
    ]);
    frame.render_widget(Paragraph::new(help), footer_area);
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() > max {
        let t: String = s.chars().take(max.saturating_sub(3)).collect();
        format!("{t}...")
    } else {
        s.to_string()
    }
}
