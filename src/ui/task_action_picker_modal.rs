//! Trigger-time action picker for a task (`r` on the tasks panel).
//!
//! Lists one **Send →** entry per running session plus a trailing
//! **Spawn new session…**. The chosen action runs immediately — nothing is
//! stored on the task. Modeled on [`theme_picker_modal`](super::theme_picker_modal).

use ratatui::{
    layout::{Constraint, Direction, Layout},
    style::Style,
    text::{Line, Span},
    widgets::{List, ListItem, Paragraph},
    Frame,
};

use super::{
    centered_fixed_height_rect, render_modal_frame, selector_list_item, selector_nav_footer,
    theme::Theme, truncate_ellipsis,
};
use crate::app::modals::TaskActionPickerModal;

pub fn render_task_action_picker_modal(frame: &mut Frame, state: &TaskActionPickerModal) {
    let height = (state.choices.len() as u16).max(2) + 6;
    let area = centered_fixed_height_rect(60, height, frame.area());
    let inner = render_modal_frame(frame, area, "Run task");

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(1),
            Constraint::Length(1),
        ])
        .split(inner);

    // Which task is being triggered.
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            format!(
                "  {}",
                truncate_ellipsis(&state.title, area.width.saturating_sub(4) as usize)
            ),
            Style::default().fg(Theme::text_muted()),
        ))),
        chunks[0],
    );

    let items: Vec<ListItem<'_>> = state
        .choices
        .iter()
        .enumerate()
        .map(|(i, choice)| selector_list_item(&choice.label(), i == state.selected))
        .collect();
    frame.render_widget(List::new(items), chunks[1]);

    frame.render_widget(Paragraph::new(selector_nav_footer()), chunks[2]);
}
