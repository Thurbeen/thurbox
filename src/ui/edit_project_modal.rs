use std::path::PathBuf;

use ratatui::{
    layout::{Constraint, Direction, Layout},
    style::Style,
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, Paragraph},
    Frame,
};

use super::theme::Theme;
use super::{centered_fixed_height_rect, render_text_field, render_text_field_with_suggestion};
use crate::app::EditProjectField;
use crate::session::{McpServerConfig, RoleConfig};

pub struct EditProjectModalState<'a> {
    pub name: &'a str,
    pub name_cursor: usize,
    pub path: &'a str,
    pub path_cursor: usize,
    pub path_suggestion: Option<&'a str>,
    pub repos: &'a [PathBuf],
    pub repo_index: usize,
    pub roles: &'a [RoleConfig],
    pub role_index: usize,
    pub mcp_servers: &'a [McpServerConfig],
    pub mcp_server_index: usize,
    pub focused_field: EditProjectField,
}

pub fn render_edit_project_modal(frame: &mut Frame, state: &EditProjectModalState<'_>) {
    // Dynamic height: name(3) + path(3) + repo_list + roles_list + footer(1) + outer border(2)
    let repo_list_inner = if state.repos.is_empty() {
        1
    } else {
        state.repos.len().min(6)
    };
    let repo_list_height = repo_list_inner as u16 + 2; // +2 for borders

    let roles_list_inner = if state.roles.is_empty() {
        1
    } else {
        state.roles.len().min(6)
    };
    let roles_list_height = roles_list_inner as u16 + 2; // +2 for borders

    let mcp_list_inner = if state.mcp_servers.is_empty() {
        1
    } else {
        state.mcp_servers.len().min(6)
    };
    let mcp_list_height = mcp_list_inner as u16 + 2; // +2 for borders

    let total_height = 3 + 3 + repo_list_height + roles_list_height + mcp_list_height + 1 + 2;

    let area = centered_fixed_height_rect(50, total_height, frame.area());

    frame.render_widget(Clear, area);

    let block = Block::default()
        .title(" Edit Project ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Theme::ACCENT));

    let inner = block.inner(area);
    frame.render_widget(block, area);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),                 // Name field
            Constraint::Length(3),                 // Path field
            Constraint::Length(repo_list_height),  // Repo list
            Constraint::Length(roles_list_height), // Roles list
            Constraint::Length(mcp_list_height),   // MCP servers list
            Constraint::Min(1),                    // Footer
        ])
        .split(inner);

    render_text_field(
        frame,
        chunks[0],
        "Name",
        state.name,
        state.name_cursor,
        state.focused_field == EditProjectField::Name,
    );

    render_text_field_with_suggestion(
        frame,
        chunks[1],
        "Add Repo Path",
        state.path,
        state.path_cursor,
        state.focused_field == EditProjectField::Path,
        state.path_suggestion,
    );

    // Repo list
    render_item_list(
        frame,
        chunks[2],
        "Repos",
        &state
            .repos
            .iter()
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>(),
        state.repo_index,
        state.focused_field == EditProjectField::RepoList,
        "(none — add via Path field above)",
    );

    // Roles list
    render_item_list(
        frame,
        chunks[3],
        "Roles",
        &state
            .roles
            .iter()
            .map(|r| r.name.clone())
            .collect::<Vec<_>>(),
        state.role_index,
        state.focused_field == EditProjectField::Roles,
        "  No roles defined",
    );

    // MCP servers list
    render_item_list(
        frame,
        chunks[4],
        "MCP Servers",
        &state
            .mcp_servers
            .iter()
            .map(|s| s.name.clone())
            .collect::<Vec<_>>(),
        state.mcp_server_index,
        state.focused_field == EditProjectField::McpServers,
        "  No MCP servers defined",
    );

    // Context-sensitive footer
    let footer = match state.focused_field {
        EditProjectField::Name => Line::from(vec![
            Span::styled("Tab", Theme::keybind()),
            Span::styled(" next  ", Theme::keybind_desc()),
            Span::styled("Enter", Theme::keybind()),
            Span::styled(" save  ", Theme::keybind_desc()),
            Span::styled("Esc", Theme::keybind()),
            Span::styled(" cancel", Theme::keybind_desc()),
        ]),
        EditProjectField::Path => {
            let tab_hint = if state.path_suggestion.is_some() {
                " complete  "
            } else {
                " next  "
            };
            Line::from(vec![
                Span::styled("Tab", Theme::keybind()),
                Span::styled(tab_hint, Theme::keybind_desc()),
                Span::styled("Enter", Theme::keybind()),
                Span::styled(" add repo  ", Theme::keybind_desc()),
                Span::styled("Esc", Theme::keybind()),
                Span::styled(" cancel", Theme::keybind_desc()),
            ])
        }
        EditProjectField::RepoList => Line::from(vec![
            Span::styled("j/k", Theme::keybind()),
            Span::styled(" navigate  ", Theme::keybind_desc()),
            Span::styled("d", Theme::keybind()),
            Span::styled(" delete  ", Theme::keybind_desc()),
            Span::styled("Enter", Theme::keybind()),
            Span::styled(" save  ", Theme::keybind_desc()),
            Span::styled("Esc", Theme::keybind()),
            Span::styled(" cancel", Theme::keybind_desc()),
        ]),
        EditProjectField::Roles | EditProjectField::McpServers => Line::from(vec![
            Span::styled("j/k", Theme::keybind()),
            Span::styled(" navigate  ", Theme::keybind_desc()),
            Span::styled("a", Theme::keybind()),
            Span::styled(" add  ", Theme::keybind_desc()),
            Span::styled("e", Theme::keybind()),
            Span::styled(" edit  ", Theme::keybind_desc()),
            Span::styled("d", Theme::keybind()),
            Span::styled(" delete  ", Theme::keybind_desc()),
            Span::styled("Esc", Theme::keybind()),
            Span::styled(" save", Theme::keybind_desc()),
        ]),
    };
    frame.render_widget(Paragraph::new(footer), chunks[5]);
}

/// Render a bordered item list with selection highlighting.
fn render_item_list(
    frame: &mut Frame,
    area: ratatui::layout::Rect,
    label: &str,
    items: &[String],
    selected_index: usize,
    focused: bool,
    empty_text: &str,
) {
    let border_color = if focused {
        Theme::BORDER_FOCUSED
    } else {
        Theme::BORDER_UNFOCUSED
    };

    let block = Block::default()
        .title(format!(" {label} ({}) ", items.len()))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(border_color));

    let inner = block.inner(area);
    frame.render_widget(block, area);

    if items.is_empty() {
        let placeholder = Paragraph::new(Line::from(Span::styled(
            empty_text,
            Style::default().fg(Theme::TEXT_MUTED),
        )));
        frame.render_widget(placeholder, inner);
    } else {
        let visible_count = inner.height as usize;
        let scroll_offset = if selected_index >= visible_count {
            selected_index - visible_count + 1
        } else {
            0
        };

        let list_items: Vec<ListItem<'_>> = items
            .iter()
            .enumerate()
            .skip(scroll_offset)
            .take(visible_count)
            .map(|(i, item)| {
                let is_selected = i == selected_index && focused;
                let style = if is_selected {
                    Theme::selected_item()
                } else {
                    Theme::normal_item()
                };
                let prefix = if is_selected { "▸ " } else { "  " };
                ListItem::new(Line::from(Span::styled(format!("{prefix}{item}"), style)))
            })
            .collect();

        frame.render_widget(List::new(list_items), inner);
    }
}
