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

pub fn render_automations_list_modal(
    frame: &mut Frame,
    state: &AutomationsListState<'_>,
) -> super::ModalRender {
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
        return ((Vec::new(), None), Vec::new());
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

    let hits = super::render_selector_rows(frame, list_area, lines, state.selected_index);

    let help = Line::from(vec![
        Span::styled("n", Theme::keybind()),
        Span::styled(" new  ", Theme::keybind_desc()),
        Span::styled("Spc", Theme::keybind()),
        Span::styled(" toggle  ", Theme::keybind_desc()),
        Span::styled("r", Theme::keybind()),
        Span::styled(" run  ", Theme::keybind_desc()),
        Span::styled("d", Theme::keybind()),
        Span::styled(" delete", Theme::keybind_desc()),
    ]);
    frame.render_widget(Paragraph::new(help), footer_area);
    let buttons = super::render_action_footer(
        frame,
        footer_area,
        (
            "Edit",
            crossterm::event::KeyCode::Enter,
            crossterm::event::KeyModifiers::NONE,
        ),
        "Close",
    );
    (hits, buttons)
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() > max {
        let t: String = s.chars().take(max.saturating_sub(3)).collect();
        format!("{t}...")
    } else {
        s.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_leaves_short_strings_untouched() {
        assert_eq!(truncate("", 10), "");
        assert_eq!(truncate("hello", 10), "hello");
    }

    #[test]
    fn truncate_keeps_string_at_exact_width() {
        assert_eq!(truncate("hello", 5), "hello");
    }

    #[test]
    fn truncate_adds_ellipsis_when_over_width() {
        // 7 chars, max 6 → keep 3 chars + "..." = 6 chars total.
        let out = truncate("abcdefg", 6);
        assert_eq!(out, "abc...");
        assert_eq!(out.chars().count(), 6);
    }

    #[test]
    fn truncate_counts_chars_not_bytes() {
        // Multi-byte chars must not be split mid-byte.
        let out = truncate("héllo wörld", 6);
        assert_eq!(out, "hél...");
        assert_eq!(out.chars().count(), 6);
    }
}
