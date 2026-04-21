use ratatui::{
    layout::{Constraint, Direction, Layout},
    style::Style,
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use super::role_editor_modal::{render_tool_list, tool_list_height, ToolListMode};
use super::{centered_fixed_height_rect, render_modal_frame, render_text_field, theme::Theme};

/// Which field is focused in the profile editor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ProfileEditorField {
    #[default]
    Name,
    Description,
    Roles,
    McpServers,
    Skills,
}

pub struct ProfileEditorState<'a> {
    pub name: &'a str,
    pub name_cursor: usize,
    pub description: &'a str,
    pub description_cursor: usize,
    pub roles: &'a [String],
    pub roles_index: usize,
    pub roles_mode: ToolListMode,
    pub roles_input: &'a str,
    pub roles_input_cursor: usize,
    pub mcp_servers: &'a [String],
    pub mcp_servers_index: usize,
    pub mcp_servers_mode: ToolListMode,
    pub mcp_servers_input: &'a str,
    pub mcp_servers_input_cursor: usize,
    pub skills: &'a [String],
    pub skills_index: usize,
    pub skills_mode: ToolListMode,
    pub skills_input: &'a str,
    pub skills_input_cursor: usize,
    pub focused_field: ProfileEditorField,
}

pub fn render_profile_editor_modal(frame: &mut Frame, state: &ProfileEditorState<'_>) {
    let roles_rows = tool_list_height(
        state.roles,
        state.roles_mode,
        state.focused_field == ProfileEditorField::Roles,
    );
    let mcp_rows = tool_list_height(
        state.mcp_servers,
        state.mcp_servers_mode,
        state.focused_field == ProfileEditorField::McpServers,
    );
    let skill_rows = tool_list_height(
        state.skills,
        state.skills_mode,
        state.focused_field == ProfileEditorField::Skills,
    );

    let content_height = 1 + 3 + 3 + roles_rows + mcp_rows + skill_rows + 1;
    let max_height = frame.area().height.saturating_sub(4);
    let height = (content_height + 2).min(max_height);
    let area = centered_fixed_height_rect(60, height, frame.area());

    let inner = render_modal_frame(frame, area, "Edit Profile");

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),          // Breadcrumb
            Constraint::Length(3),          // Name
            Constraint::Length(3),          // Description
            Constraint::Length(roles_rows), // Roles
            Constraint::Length(mcp_rows),   // MCP servers
            Constraint::Length(skill_rows), // Skills
            Constraint::Length(1),          // Footer
        ])
        .split(inner);

    let label = if state.name.is_empty() {
        "New".to_string()
    } else {
        format!("\"{}\"", state.name)
    };
    let breadcrumb = Line::from(vec![
        Span::styled(" Profiles", Style::default().fg(Theme::text_muted())),
        Span::styled(" > ", Style::default().fg(Theme::text_muted())),
        Span::styled(label, Style::default().fg(Theme::accent())),
    ]);
    frame.render_widget(Paragraph::new(breadcrumb), chunks[0]);

    render_text_field(
        frame,
        chunks[1],
        "Name",
        state.name,
        state.name_cursor,
        state.focused_field == ProfileEditorField::Name,
    );

    render_text_field(
        frame,
        chunks[2],
        "Description",
        state.description,
        state.description_cursor,
        state.focused_field == ProfileEditorField::Description,
    );

    render_tool_list(
        frame,
        chunks[3],
        "Roles",
        state.roles,
        state.roles_index,
        state.roles_mode,
        state.roles_input,
        state.roles_input_cursor,
        state.focused_field == ProfileEditorField::Roles,
    );

    render_tool_list(
        frame,
        chunks[4],
        "MCP Servers",
        state.mcp_servers,
        state.mcp_servers_index,
        state.mcp_servers_mode,
        state.mcp_servers_input,
        state.mcp_servers_input_cursor,
        state.focused_field == ProfileEditorField::McpServers,
    );

    render_tool_list(
        frame,
        chunks[5],
        "Skills",
        state.skills,
        state.skills_index,
        state.skills_mode,
        state.skills_input,
        state.skills_input_cursor,
        state.focused_field == ProfileEditorField::Skills,
    );

    let is_list_field = matches!(
        state.focused_field,
        ProfileEditorField::Roles | ProfileEditorField::McpServers | ProfileEditorField::Skills
    );
    let list_mode = match state.focused_field {
        ProfileEditorField::Roles => state.roles_mode,
        ProfileEditorField::McpServers => state.mcp_servers_mode,
        ProfileEditorField::Skills => state.skills_mode,
        _ => ToolListMode::Browse,
    };

    let footer = if is_list_field && list_mode == ToolListMode::Adding {
        Line::from(vec![
            Span::styled("Enter", Theme::keybind()),
            Span::styled(" confirm  ", Theme::keybind_desc()),
            Span::styled("Esc", Theme::keybind()),
            Span::styled(" cancel", Theme::keybind_desc()),
        ])
    } else if is_list_field {
        Line::from(vec![
            Span::styled("a", Theme::keybind()),
            Span::styled(" add  ", Theme::keybind_desc()),
            Span::styled("d", Theme::keybind()),
            Span::styled(" delete  ", Theme::keybind_desc()),
            Span::styled("Tab", Theme::keybind()),
            Span::styled(" next  ", Theme::keybind_desc()),
            Span::styled("Enter", Theme::keybind()),
            Span::styled(" save  ", Theme::keybind_desc()),
            Span::styled("Esc", Theme::keybind()),
            Span::styled(" discard", Theme::keybind_desc()),
        ])
    } else {
        Line::from(vec![
            Span::styled("Tab", Theme::keybind()),
            Span::styled(" next  ", Theme::keybind_desc()),
            Span::styled("Enter", Theme::keybind()),
            Span::styled(" save  ", Theme::keybind_desc()),
            Span::styled("Esc", Theme::keybind()),
            Span::styled(" discard", Theme::keybind_desc()),
        ])
    };
    frame.render_widget(Paragraph::new(footer), chunks[6]);
}
