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
use crate::session::ProfileConfig;

pub struct ProfilePickerState<'a> {
    pub profiles: &'a [ProfileConfig],
    pub selected_index: usize,
}

/// Render the profile picker. Row 0 is always the synthetic "No profile"
/// entry; rows `1..` correspond to `profiles[index - 1]`. The description
/// line below the list shows what the highlighted profile bundles.
pub fn render_profile_picker_modal(frame: &mut Frame, state: &ProfilePickerState<'_>) {
    // +1 row for the synthetic "No profile"; +4 for borders, description,
    // and footer.
    let height = (state.profiles.len() as u16) + 1 + 4;
    let area = centered_fixed_height_rect(60, height, frame.area());

    let inner = render_modal_frame(frame, area, "Profile");

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(1),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .split(inner);

    let mut items: Vec<ListItem<'_>> = Vec::with_capacity(state.profiles.len() + 1);
    items.push(selector_list_item(
        "(No profile)",
        state.selected_index == 0,
    ));
    for (i, profile) in state.profiles.iter().enumerate() {
        items.push(selector_list_item(
            &profile.name,
            i + 1 == state.selected_index,
        ));
    }

    frame.render_widget(List::new(items), chunks[0]);

    // Description line: for "No profile" show a short hint; for a profile
    // show its description plus a compact "roles=… mcp=… skills=…" summary.
    let desc_text = if state.selected_index == 0 {
        "Use the standard role / MCP / skill pickers.".to_string()
    } else if let Some(profile) = state.profiles.get(state.selected_index - 1) {
        let mut parts: Vec<String> = Vec::new();
        if !profile.description.is_empty() {
            parts.push(profile.description.clone());
        }
        parts.push(format!(
            "roles=[{}] mcp=[{}] skills=[{}]",
            profile.roles.join(","),
            profile.mcp_servers.join(","),
            profile.skills.join(","),
        ));
        parts.join("  ·  ")
    } else {
        String::new()
    };
    let desc = Line::from(Span::styled(
        desc_text,
        Style::default().fg(Theme::text_muted()),
    ));
    frame.render_widget(Paragraph::new(desc), chunks[1]);

    frame.render_widget(Paragraph::new(selector_nav_footer()), chunks[2]);
}
