use ratatui::{
    text::{Line, Span},
    widgets::{List, ListItem, Paragraph},
    Frame,
};

use super::render_list_modal_frame;
use super::theme::Theme;
use crate::session::McpServerConfig;

pub struct McpServerPickerState<'a> {
    pub servers: &'a [McpServerConfig],
    pub selected: &'a [bool],
    pub index: usize,
}

pub fn render_mcp_server_picker_modal(frame: &mut Frame, state: &McpServerPickerState<'_>) {
    let Some(chunks) = render_list_modal_frame(
        frame,
        50,
        "MCP Servers",
        state.servers.len(),
        Some("No MCP servers configured"),
        Some(Line::from(vec![
            Span::styled("Esc", Theme::keybind()),
            Span::styled(" close", Theme::keybind_desc()),
        ])),
    ) else {
        return;
    };

    let items: Vec<ListItem<'_>> = state
        .servers
        .iter()
        .enumerate()
        .map(|(i, server)| {
            let style = if i == state.index {
                Theme::selected_item()
            } else {
                Theme::normal_item()
            };
            let checked = state.selected.get(i).copied().unwrap_or(false);
            let checkbox = if checked { "[x]" } else { "[ ]" };
            let prefix = if i == state.index { "▸ " } else { "  " };
            ListItem::new(Line::from(Span::styled(
                format!("{prefix}{checkbox} {}", server.name),
                style,
            )))
        })
        .collect();

    frame.render_widget(List::new(items), chunks[0]);

    let footer = Line::from(vec![
        Span::styled("j/k", Theme::keybind()),
        Span::styled(" navigate  ", Theme::keybind_desc()),
        Span::styled("Space", Theme::keybind()),
        Span::styled(" toggle  ", Theme::keybind_desc()),
        Span::styled("Enter", Theme::keybind()),
        Span::styled(" confirm  ", Theme::keybind_desc()),
        Span::styled("Esc", Theme::keybind()),
        Span::styled(" cancel", Theme::keybind_desc()),
    ]);
    frame.render_widget(Paragraph::new(footer), chunks[1]);
}
