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
use crate::session::RoleConfig;

pub struct RoleSelectorState<'a> {
    pub roles: &'a [RoleConfig],
    pub selected_index: usize,
}

pub fn render_role_selector_modal(frame: &mut Frame, state: &RoleSelectorState<'_>) {
    let height = (state.roles.len() as u16) + 4;
    let area = centered_fixed_height_rect(50, height, frame.area());

    let inner = render_modal_frame(frame, area, "Session Role");

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(1),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .split(inner);

    let items: Vec<ListItem<'_>> = state
        .roles
        .iter()
        .enumerate()
        .map(|(i, role)| selector_list_item(&role.name, i == state.selected_index))
        .collect();

    frame.render_widget(List::new(items), chunks[0]);

    if let Some(role) = state.roles.get(state.selected_index) {
        let desc = Line::from(Span::styled(
            &role.description,
            Style::default().fg(Theme::text_muted()),
        ));
        frame.render_widget(Paragraph::new(desc), chunks[1]);
    }

    frame.render_widget(Paragraph::new(selector_nav_footer()), chunks[2]);
}
