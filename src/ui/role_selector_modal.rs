use ratatui::{
    layout::{Constraint, Direction, Layout},
    style::Style,
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, Paragraph},
    Frame,
};

use super::centered_fixed_height_rect;
use super::theme::Theme;
use crate::session::RoleConfig;

pub struct RoleSelectorState<'a> {
    pub roles: &'a [RoleConfig],
    pub selected_index: usize,
}

pub fn render_role_selector_modal(frame: &mut Frame, state: &RoleSelectorState<'_>) {
    // 2 (border) + roles count + 1 (description) + 1 (footer)
    let height = (state.roles.len() as u16) + 4;
    let area = centered_fixed_height_rect(50, height, frame.area());

    frame.render_widget(Clear, area);

    let block = Block::default()
        .title(" Session Role ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Theme::ACCENT));

    let inner = block.inner(area);
    frame.render_widget(block, area);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(1),    // Role list
            Constraint::Length(1), // Description
            Constraint::Length(1), // Footer
        ])
        .split(inner);

    let items: Vec<ListItem<'_>> = state
        .roles
        .iter()
        .enumerate()
        .map(|(i, role)| {
            let style = if i == state.selected_index {
                Theme::selected_item()
            } else {
                Theme::normal_item()
            };
            let prefix = if i == state.selected_index {
                "▸ "
            } else {
                "  "
            };
            ListItem::new(Line::from(Span::styled(
                format!("{prefix}{}", role.name),
                style,
            )))
        })
        .collect();

    let list = List::new(items);
    frame.render_widget(list, chunks[0]);

    // Description of selected role
    if let Some(role) = state.roles.get(state.selected_index) {
        let desc = Line::from(Span::styled(
            &role.description,
            Style::default().fg(Theme::TEXT_MUTED),
        ));
        frame.render_widget(Paragraph::new(desc), chunks[1]);
    }

    let footer = Line::from(vec![
        Span::styled("j/k", Theme::keybind()),
        Span::styled(" navigate  ", Theme::keybind_desc()),
        Span::styled("Enter", Theme::keybind()),
        Span::styled(" select  ", Theme::keybind_desc()),
        Span::styled("Esc", Theme::keybind()),
        Span::styled(" cancel", Theme::keybind_desc()),
    ]);
    frame.render_widget(Paragraph::new(footer), chunks[2]);
}
