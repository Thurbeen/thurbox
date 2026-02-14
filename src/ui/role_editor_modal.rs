use ratatui::{
    layout::{Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, Paragraph},
    Frame,
};

use super::{centered_fixed_height_rect, render_text_field};
use crate::session::RoleConfig;

// ── Role List View ──────────────────────────────────────────────────────────

pub struct RoleListState<'a> {
    pub roles: &'a [RoleConfig],
    pub selected_index: usize,
}

pub fn render_role_list_modal(frame: &mut Frame, state: &RoleListState<'_>) {
    // 2 (border) + max(roles, 1) + 1 (description) + 1 (footer)
    let list_rows = if state.roles.is_empty() {
        1
    } else {
        state.roles.len() as u16
    };
    let height = list_rows + 4;
    let area = centered_fixed_height_rect(50, height, frame.area());

    frame.render_widget(Clear, area);

    let block = Block::default()
        .title(" Project Roles ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan));

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

    if state.roles.is_empty() {
        let empty = Paragraph::new(Line::from(Span::styled(
            "  No roles defined",
            Style::default().fg(Color::DarkGray),
        )));
        frame.render_widget(empty, chunks[0]);
    } else {
        let items: Vec<ListItem<'_>> = state
            .roles
            .iter()
            .enumerate()
            .map(|(i, role)| {
                let style = if i == state.selected_index {
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(Color::White)
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
                Style::default().fg(Color::DarkGray),
            ));
            frame.render_widget(Paragraph::new(desc), chunks[1]);
        }
    }

    let footer = Line::from(vec![
        Span::styled("a", Style::default().fg(Color::Yellow)),
        Span::styled(" add  ", Style::default().fg(Color::DarkGray)),
        Span::styled("e", Style::default().fg(Color::Yellow)),
        Span::styled("/", Style::default().fg(Color::DarkGray)),
        Span::styled("Enter", Style::default().fg(Color::Yellow)),
        Span::styled(" edit  ", Style::default().fg(Color::DarkGray)),
        Span::styled("d", Style::default().fg(Color::Yellow)),
        Span::styled(" delete  ", Style::default().fg(Color::DarkGray)),
        Span::styled("Esc", Style::default().fg(Color::Yellow)),
        Span::styled(" save & close", Style::default().fg(Color::DarkGray)),
    ]);
    frame.render_widget(Paragraph::new(footer), chunks[2]);
}

// ── Role Editor View ────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoleEditorField {
    Name,
    Description,
    AllowedTools,
    DisallowedTools,
    SystemPrompt,
}

/// Sub-state for a tool list field: either browsing items or typing a new one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolListMode {
    /// Navigating the list with j/k, delete with d, add with a.
    Browse,
    /// Typing a new tool name into the inline input.
    Adding,
}

pub struct RoleEditorState<'a> {
    pub name: &'a str,
    pub name_cursor: usize,
    pub description: &'a str,
    pub description_cursor: usize,
    pub allowed_tools: &'a [String],
    pub allowed_tools_index: usize,
    pub allowed_tools_mode: ToolListMode,
    pub allowed_tools_input: &'a str,
    pub allowed_tools_input_cursor: usize,
    pub disallowed_tools: &'a [String],
    pub disallowed_tools_index: usize,
    pub disallowed_tools_mode: ToolListMode,
    pub disallowed_tools_input: &'a str,
    pub disallowed_tools_input_cursor: usize,
    pub system_prompt: &'a str,
    pub system_prompt_cursor: usize,
    pub focused_field: RoleEditorField,
}

pub fn render_role_editor_modal(frame: &mut Frame, state: &RoleEditorState<'_>) {
    // Dynamic height: 2 (border) + 3 (name) + 3 (desc) + tool lists + 3 (prompt) + 1 (footer)
    let allowed_rows = tool_list_height(
        state.allowed_tools,
        state.allowed_tools_mode,
        state.focused_field == RoleEditorField::AllowedTools,
    );
    let disallowed_rows = tool_list_height(
        state.disallowed_tools,
        state.disallowed_tools_mode,
        state.focused_field == RoleEditorField::DisallowedTools,
    );
    // Clamp total height so it doesn't exceed terminal.
    let content_height = 3 + 3 + allowed_rows + disallowed_rows + 3 + 1;
    let max_height = frame.area().height.saturating_sub(4);
    let height = (content_height + 2).min(max_height); // +2 for border
    let area = centered_fixed_height_rect(60, height, frame.area());

    frame.render_widget(Clear, area);

    let block = Block::default()
        .title(" Edit Role ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan));

    let inner = block.inner(area);
    frame.render_widget(block, area);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),               // Name
            Constraint::Length(3),               // Description
            Constraint::Length(allowed_rows),    // Allowed Tools
            Constraint::Length(disallowed_rows), // Disallowed Tools
            Constraint::Length(3),               // System Prompt
            Constraint::Length(1),               // Footer
        ])
        .split(inner);

    render_text_field(
        frame,
        chunks[0],
        "Name",
        state.name,
        state.name_cursor,
        state.focused_field == RoleEditorField::Name,
    );

    render_text_field(
        frame,
        chunks[1],
        "Description",
        state.description,
        state.description_cursor,
        state.focused_field == RoleEditorField::Description,
    );

    render_tool_list(
        frame,
        chunks[2],
        "Allowed Tools",
        state.allowed_tools,
        state.allowed_tools_index,
        state.allowed_tools_mode,
        state.allowed_tools_input,
        state.allowed_tools_input_cursor,
        state.focused_field == RoleEditorField::AllowedTools,
    );

    render_tool_list(
        frame,
        chunks[3],
        "Disallowed Tools",
        state.disallowed_tools,
        state.disallowed_tools_index,
        state.disallowed_tools_mode,
        state.disallowed_tools_input,
        state.disallowed_tools_input_cursor,
        state.focused_field == RoleEditorField::DisallowedTools,
    );

    render_text_field(
        frame,
        chunks[4],
        "System Prompt",
        state.system_prompt,
        state.system_prompt_cursor,
        state.focused_field == RoleEditorField::SystemPrompt,
    );

    // Footer — context-sensitive
    let is_tool_field = matches!(
        state.focused_field,
        RoleEditorField::AllowedTools | RoleEditorField::DisallowedTools
    );
    let tool_mode = match state.focused_field {
        RoleEditorField::AllowedTools => state.allowed_tools_mode,
        RoleEditorField::DisallowedTools => state.disallowed_tools_mode,
        _ => ToolListMode::Browse,
    };

    let footer = if is_tool_field && tool_mode == ToolListMode::Adding {
        Line::from(vec![
            Span::styled("Enter", Style::default().fg(Color::Yellow)),
            Span::styled(" confirm  ", Style::default().fg(Color::DarkGray)),
            Span::styled("Esc", Style::default().fg(Color::Yellow)),
            Span::styled(" cancel", Style::default().fg(Color::DarkGray)),
        ])
    } else if is_tool_field {
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
    frame.render_widget(Paragraph::new(footer), chunks[5]);
}

// ── Tool list helpers ───────────────────────────────────────────────────────

/// Compute the height needed for a tool list section.
/// 2 (border) + max(items, 1 empty) + optional 1 for input row.
fn tool_list_height(tools: &[String], mode: ToolListMode, focused: bool) -> u16 {
    let item_rows = if tools.is_empty() {
        1
    } else {
        tools.len() as u16
    };
    let input_row = if focused && mode == ToolListMode::Adding {
        1
    } else {
        0
    };
    item_rows + input_row + 2 // +2 for borders
}

/// Render a bordered tool list with optional inline add-input.
#[allow(clippy::too_many_arguments)]
fn render_tool_list(
    frame: &mut Frame,
    area: ratatui::layout::Rect,
    label: &str,
    tools: &[String],
    selected_index: usize,
    mode: ToolListMode,
    input_value: &str,
    input_cursor: usize,
    focused: bool,
) {
    let border_color = if focused { Color::Cyan } else { Color::Gray };
    let block = Block::default()
        .title(format!(" {label} "))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(border_color));

    let inner = block.inner(area);
    frame.render_widget(block, area);

    // Split inner into list rows + optional input row.
    let has_input = focused && mode == ToolListMode::Adding;
    let constraints = if has_input {
        vec![Constraint::Min(0), Constraint::Length(1)]
    } else {
        vec![Constraint::Min(0)]
    };
    let parts = Layout::default()
        .direction(Direction::Vertical)
        .constraints(constraints)
        .split(inner);

    // Render tool items (or placeholder).
    if tools.is_empty() {
        let empty = Paragraph::new(Line::from(Span::styled(
            "  (none)",
            Style::default().fg(Color::DarkGray),
        )));
        frame.render_widget(empty, parts[0]);
    } else {
        let items: Vec<ListItem<'_>> = tools
            .iter()
            .enumerate()
            .map(|(i, tool)| {
                let is_selected = focused && mode == ToolListMode::Browse && i == selected_index;
                let style = if is_selected {
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(Color::White)
                };
                let prefix = if is_selected { "▸ " } else { "  " };
                ListItem::new(Line::from(Span::styled(format!("{prefix}{tool}"), style)))
            })
            .collect();
        frame.render_widget(List::new(items), parts[0]);
    }

    // Render inline input row when adding.
    if has_input {
        render_inline_input(frame, parts[1], input_value, input_cursor);
    }
}

/// Render a single-line inline text input (no border, just cursor + text).
fn render_inline_input(frame: &mut Frame, area: ratatui::layout::Rect, value: &str, cursor: usize) {
    let chars: Vec<char> = value.chars().collect();
    let cursor = cursor.min(chars.len());

    let before: String = chars[..cursor].iter().collect();
    let cursor_char = if cursor < chars.len() {
        chars[cursor].to_string()
    } else {
        " ".to_string()
    };
    let after: String = if cursor < chars.len() {
        chars[cursor + 1..].iter().collect()
    } else {
        String::new()
    };

    let line = Line::from(vec![
        Span::styled("+ ", Style::default().fg(Color::Green)),
        Span::styled(before, Style::default().fg(Color::White)),
        Span::styled(
            cursor_char,
            Style::default().fg(Color::Black).bg(Color::White),
        ),
        Span::styled(after, Style::default().fg(Color::White)),
    ]);

    frame.render_widget(Paragraph::new(line), area);
}
