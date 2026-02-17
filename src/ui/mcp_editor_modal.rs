use ratatui::{
    layout::{Constraint, Direction, Layout},
    style::{Color, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
    Frame,
};

use super::{centered_fixed_height_rect, render_text_field};
use crate::app::mcp_editor_modal::McpEditorField;
use crate::ui::role_editor_modal::ToolListMode;

pub struct McpEditorState<'a> {
    pub name: &'a str,
    pub name_cursor: usize,
    pub command: &'a str,
    pub command_cursor: usize,
    pub args: &'a [String],
    pub args_index: usize,
    pub args_mode: ToolListMode,
    pub args_input: &'a str,
    pub args_input_cursor: usize,
    pub env: &'a [String],
    pub env_index: usize,
    pub env_mode: ToolListMode,
    pub env_input: &'a str,
    pub env_input_cursor: usize,
    pub focused_field: McpEditorField,
}

pub fn render_mcp_editor_modal(frame: &mut Frame, state: &McpEditorState<'_>) {
    let args_rows = super::role_editor_modal::tool_list_height(
        state.args,
        state.args_mode,
        state.focused_field == McpEditorField::Args,
    );
    let env_rows = super::role_editor_modal::tool_list_height(
        state.env,
        state.env_mode,
        state.focused_field == McpEditorField::Env,
    );

    let content_height = 3 + 3 + args_rows + env_rows + 1;
    let max_height = frame.area().height.saturating_sub(4);
    let height = (content_height + 2).min(max_height);
    let area = centered_fixed_height_rect(60, height, frame.area());

    frame.render_widget(Clear, area);

    let block = Block::default()
        .title(" Edit MCP Server ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan));

    let inner = block.inner(area);
    frame.render_widget(block, area);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),         // Name
            Constraint::Length(3),         // Command
            Constraint::Length(args_rows), // Args
            Constraint::Length(env_rows),  // Env
            Constraint::Length(1),         // Footer
        ])
        .split(inner);

    render_text_field(
        frame,
        chunks[0],
        "Name",
        state.name,
        state.name_cursor,
        state.focused_field == McpEditorField::Name,
    );

    render_text_field(
        frame,
        chunks[1],
        "Command",
        state.command,
        state.command_cursor,
        state.focused_field == McpEditorField::Command,
    );

    super::role_editor_modal::render_tool_list(
        frame,
        chunks[2],
        "Args",
        state.args,
        state.args_index,
        state.args_mode,
        state.args_input,
        state.args_input_cursor,
        state.focused_field == McpEditorField::Args,
    );

    super::role_editor_modal::render_tool_list(
        frame,
        chunks[3],
        "Env (KEY=VALUE)",
        state.env,
        state.env_index,
        state.env_mode,
        state.env_input,
        state.env_input_cursor,
        state.focused_field == McpEditorField::Env,
    );

    // Footer — context-sensitive
    let is_list_field = matches!(
        state.focused_field,
        McpEditorField::Args | McpEditorField::Env
    );
    let list_mode = match state.focused_field {
        McpEditorField::Args => state.args_mode,
        McpEditorField::Env => state.env_mode,
        _ => ToolListMode::Browse,
    };

    let footer = if is_list_field && list_mode == ToolListMode::Adding {
        Line::from(vec![
            Span::styled("Enter", Style::default().fg(Color::Yellow)),
            Span::styled(" confirm  ", Style::default().fg(Color::DarkGray)),
            Span::styled("Esc", Style::default().fg(Color::Yellow)),
            Span::styled(" cancel", Style::default().fg(Color::DarkGray)),
        ])
    } else if is_list_field {
        Line::from(vec![
            Span::styled("a", Style::default().fg(Color::Yellow)),
            Span::styled(" add  ", Style::default().fg(Color::DarkGray)),
            Span::styled("d", Style::default().fg(Color::Yellow)),
            Span::styled(" delete  ", Style::default().fg(Color::DarkGray)),
            Span::styled("Tab", Style::default().fg(Color::Yellow)),
            Span::styled(" next  ", Style::default().fg(Color::DarkGray)),
            Span::styled("Enter", Style::default().fg(Color::Yellow)),
            Span::styled(" save  ", Style::default().fg(Color::DarkGray)),
            Span::styled("Esc", Style::default().fg(Color::Yellow)),
            Span::styled(" discard", Style::default().fg(Color::DarkGray)),
        ])
    } else {
        Line::from(vec![
            Span::styled("Tab", Style::default().fg(Color::Yellow)),
            Span::styled(" next  ", Style::default().fg(Color::DarkGray)),
            Span::styled("Enter", Style::default().fg(Color::Yellow)),
            Span::styled(" save  ", Style::default().fg(Color::DarkGray)),
            Span::styled("Esc", Style::default().fg(Color::Yellow)),
            Span::styled(" discard", Style::default().fg(Color::DarkGray)),
        ])
    };
    frame.render_widget(Paragraph::new(footer), chunks[4]);
}
