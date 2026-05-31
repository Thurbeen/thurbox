//! Persistent, focusable automations pane shown beneath the session list.
//!
//! Read-and-act: it mirrors `cached_automations` and, when focused, drives the
//! same toggle/run/edit/delete actions as the Ctrl+P modal.

use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
    Frame,
};

use super::theme::Theme;
use super::FocusLevel;

/// One row in the automations pane.
pub struct AutomationPaneEntry {
    pub name: String,
    /// e.g. `"daily · spawn · in 3h"`.
    pub summary: String,
    pub enabled: bool,
}

pub struct AutomationsPaneState<'a> {
    pub entries: &'a [AutomationPaneEntry],
    pub selected: usize,
    pub focus: FocusLevel,
}

pub fn render_automations_pane(frame: &mut Frame, area: Rect, state: &AutomationsPaneState<'_>) {
    let border_color = match state.focus {
        FocusLevel::Focused => Theme::border_focused(),
        _ => Theme::border_unfocused(),
    };
    let block = Block::default()
        .title(" Automations ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(border_color));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if state.entries.is_empty() {
        let text = if matches!(state.focus, FocusLevel::Focused) {
            "none — Ctrl+N to add"
        } else {
            "none"
        };
        let hint = Paragraph::new(Line::from(Span::styled(
            text,
            Style::default().fg(Theme::text_muted()),
        )));
        frame.render_widget(hint, inner);
        return;
    }

    let width = inner.width as usize;
    let focused = matches!(state.focus, FocusLevel::Focused);

    let lines: Vec<Line> = state
        .entries
        .iter()
        .enumerate()
        .map(|(i, e)| {
            let selected = focused && i == state.selected;
            let marker = if e.enabled { "●" } else { "○" };
            let text = truncate(&format!(" {marker} {} — {} ", e.name, e.summary), width);
            if selected {
                Line::from(Span::styled(text, Theme::selected_item()))
            } else {
                let mut style = Style::default().fg(if e.enabled {
                    Theme::text_secondary()
                } else {
                    Theme::text_muted()
                });
                if !focused && i == state.selected {
                    style = style.add_modifier(Modifier::DIM);
                }
                Line::from(Span::styled(text, style))
            }
        })
        .collect();

    frame.render_widget(Paragraph::new(lines), inner);
}

fn truncate(s: &str, max: usize) -> String {
    if max == 0 {
        return String::new();
    }
    if s.chars().count() > max {
        let t: String = s.chars().take(max.saturating_sub(1)).collect();
        format!("{t}…")
    } else {
        s.to_string()
    }
}
