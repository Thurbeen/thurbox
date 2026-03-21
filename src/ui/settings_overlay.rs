//! Settings overlay: tabbed list of global Roles and MCP Servers.

use ratatui::{
    style::Style,
    text::{Line, Span},
    widgets::{List, ListItem, Paragraph},
    Frame,
};

use super::centered_fixed_height_rect;
use super::render_modal_frame;
use super::theme::Theme;
use crate::app::SettingsTab;
use crate::session::{McpServerConfig, RoleConfig};

/// State needed to render the settings overlay.
pub struct SettingsOverlayState<'a> {
    pub tab: SettingsTab,
    pub roles: &'a [RoleConfig],
    pub role_index: usize,
    pub mcp_servers: &'a [McpServerConfig],
    pub mcp_index: usize,
}

pub fn render_settings_overlay(frame: &mut Frame, state: &SettingsOverlayState) {
    let area = centered_fixed_height_rect(60, 22, frame.area());
    let inner = render_modal_frame(frame, area, "Settings");

    if inner.height < 4 || inner.width < 10 {
        return;
    }

    // Tab bar (row 0)
    let tab_area = ratatui::layout::Rect { height: 1, ..inner };

    let roles_style = if state.tab == SettingsTab::Roles {
        Theme::focused_title()
    } else {
        Style::default().fg(Theme::TEXT_MUTED)
    };
    let mcp_style = if state.tab == SettingsTab::McpServers {
        Theme::focused_title()
    } else {
        Style::default().fg(Theme::TEXT_MUTED)
    };

    let tab_line = Line::from(vec![
        Span::styled(" [Roles] ", roles_style),
        Span::styled("  ", Style::default()),
        Span::styled("[MCP Servers] ", mcp_style),
    ]);
    frame.render_widget(Paragraph::new(tab_line), tab_area);

    // List area (rows 1..height-2)
    let list_area = ratatui::layout::Rect {
        y: inner.y + 1,
        height: inner.height.saturating_sub(2),
        ..inner
    };

    match state.tab {
        SettingsTab::Roles => {
            if state.roles.is_empty() {
                let empty = Paragraph::new(Line::from(Span::styled(
                    "  No roles configured. Press 'a' to add.",
                    Style::default().fg(Theme::TEXT_MUTED),
                )));
                frame.render_widget(empty, list_area);
            } else {
                let items: Vec<ListItem> = state
                    .roles
                    .iter()
                    .enumerate()
                    .map(|(i, role)| {
                        let style = if i == state.role_index {
                            Theme::focused_title()
                        } else {
                            Style::default().fg(Theme::TEXT_PRIMARY)
                        };
                        let prefix = if i == state.role_index { "▸ " } else { "  " };
                        ListItem::new(Line::from(Span::styled(
                            format!("{prefix}{}", role.name),
                            style,
                        )))
                    })
                    .collect();
                frame.render_widget(List::new(items), list_area);
            }
        }
        SettingsTab::McpServers => {
            if state.mcp_servers.is_empty() {
                let empty = Paragraph::new(Line::from(Span::styled(
                    "  No MCP servers configured. Press 'a' to add.",
                    Style::default().fg(Theme::TEXT_MUTED),
                )));
                frame.render_widget(empty, list_area);
            } else {
                let items: Vec<ListItem> = state
                    .mcp_servers
                    .iter()
                    .enumerate()
                    .map(|(i, server)| {
                        let style = if i == state.mcp_index {
                            Theme::focused_title()
                        } else {
                            Style::default().fg(Theme::TEXT_PRIMARY)
                        };
                        let prefix = if i == state.mcp_index { "▸ " } else { "  " };
                        ListItem::new(Line::from(Span::styled(
                            format!("{prefix}{}", server.name),
                            style,
                        )))
                    })
                    .collect();
                frame.render_widget(List::new(items), list_area);
            }
        }
    }

    // Hints bar (last row)
    let hints_area = ratatui::layout::Rect {
        y: inner.y + inner.height.saturating_sub(1),
        height: 1,
        ..inner
    };

    let hints = Line::from(vec![
        Span::styled(" Tab", Theme::keybind()),
        Span::styled(": switch  ", Style::default().fg(Theme::TEXT_MUTED)),
        Span::styled("a", Theme::keybind()),
        Span::styled(": add  ", Style::default().fg(Theme::TEXT_MUTED)),
        Span::styled("e", Theme::keybind()),
        Span::styled(": edit  ", Style::default().fg(Theme::TEXT_MUTED)),
        Span::styled("d", Theme::keybind()),
        Span::styled(": delete  ", Style::default().fg(Theme::TEXT_MUTED)),
        Span::styled("Esc", Theme::keybind()),
        Span::styled(": close", Style::default().fg(Theme::TEXT_MUTED)),
    ]);
    frame.render_widget(Paragraph::new(hints), hints_area);
}
