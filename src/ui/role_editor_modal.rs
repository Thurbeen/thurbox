use ratatui::{
    layout::{Constraint, Direction, Layout},
    style::Style,
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph},
    Frame,
};

use super::render_modal_frame;
use super::theme::Theme;
use super::{centered_fixed_height_rect, render_text_field};

// ── Role Editor View ────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoleEditorField {
    Name,
    Description,
    AllowedTools,
    DisallowedTools,
    SystemPrompt,
    Env,
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
    pub env: &'a [String],
    pub env_index: usize,
    pub env_mode: ToolListMode,
    pub env_input: &'a str,
    pub env_input_cursor: usize,
    pub focused_field: RoleEditorField,
}

pub fn render_role_editor_modal(frame: &mut Frame, state: &RoleEditorState<'_>) {
    // Dynamic height: 2 (border) + 3 (name) + 3 (desc) + tool lists + 3 (prompt) + env list + 1 (footer)
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
    let env_rows = tool_list_height(
        state.env,
        state.env_mode,
        state.focused_field == RoleEditorField::Env,
    );
    // Clamp total height so it doesn't exceed terminal.
    let content_height = 1 + 3 + 3 + allowed_rows + disallowed_rows + 3 + env_rows + 1; // +1 breadcrumb
    let max_height = frame.area().height.saturating_sub(4);
    let height = (content_height + 2).min(max_height); // +2 for border
    let area = centered_fixed_height_rect(60, height, frame.area());

    let inner = render_modal_frame(frame, area, "Edit Role");

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),               // Breadcrumb
            Constraint::Length(3),               // Name
            Constraint::Length(3),               // Description
            Constraint::Length(allowed_rows),    // Allowed Tools
            Constraint::Length(disallowed_rows), // Disallowed Tools
            Constraint::Length(3),               // System Prompt
            Constraint::Length(env_rows),        // Environment Variables
            Constraint::Length(1),               // Footer
        ])
        .split(inner);

    // Breadcrumb
    let role_label = if state.name.is_empty() {
        "New".to_string()
    } else {
        format!("\"{}\"", state.name)
    };
    let breadcrumb = Line::from(vec![
        Span::styled(" Roles", Style::default().fg(Theme::TEXT_MUTED)),
        Span::styled(" > ", Style::default().fg(Theme::TEXT_MUTED)),
        Span::styled(role_label, Style::default().fg(Theme::ACCENT)),
    ]);
    frame.render_widget(Paragraph::new(breadcrumb), chunks[0]);

    render_text_field(
        frame,
        chunks[1],
        "Name",
        state.name,
        state.name_cursor,
        state.focused_field == RoleEditorField::Name,
    );

    render_text_field(
        frame,
        chunks[2],
        "Description",
        state.description,
        state.description_cursor,
        state.focused_field == RoleEditorField::Description,
    );

    render_tool_list(
        frame,
        chunks[3],
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
        chunks[4],
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
        chunks[5],
        "System Prompt",
        state.system_prompt,
        state.system_prompt_cursor,
        state.focused_field == RoleEditorField::SystemPrompt,
    );

    render_tool_list(
        frame,
        chunks[6],
        "Env (KEY=VALUE)",
        state.env,
        state.env_index,
        state.env_mode,
        state.env_input,
        state.env_input_cursor,
        state.focused_field == RoleEditorField::Env,
    );

    // Footer — context-sensitive
    let is_list_field = matches!(
        state.focused_field,
        RoleEditorField::AllowedTools | RoleEditorField::DisallowedTools | RoleEditorField::Env
    );
    let tool_mode = match state.focused_field {
        RoleEditorField::AllowedTools => state.allowed_tools_mode,
        RoleEditorField::DisallowedTools => state.disallowed_tools_mode,
        RoleEditorField::Env => state.env_mode,
        _ => ToolListMode::Browse,
    };

    let footer = if is_list_field && tool_mode == ToolListMode::Adding {
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
    frame.render_widget(Paragraph::new(footer), chunks[7]);
}

// ── Tool list helpers ───────────────────────────────────────────────────────

/// Maximum visible items in a tool list before scrolling kicks in.
const TOOL_LIST_MAX_VISIBLE: u16 = 6;

/// Compute the height needed for a tool list section.
/// 2 (border) + min(max(items, 1 empty), MAX_VISIBLE) + optional 1 for input row.
pub fn tool_list_height(tools: &[String], mode: ToolListMode, focused: bool) -> u16 {
    let item_rows = if tools.is_empty() {
        1
    } else {
        (tools.len() as u16).min(TOOL_LIST_MAX_VISIBLE)
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
pub fn render_tool_list(
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
    let border_color = if focused {
        Theme::BORDER_FOCUSED
    } else {
        Theme::BORDER_UNFOCUSED
    };
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
            Style::default().fg(Theme::TEXT_MUTED),
        )));
        frame.render_widget(empty, parts[0]);
    } else {
        let items: Vec<ListItem<'_>> = tools
            .iter()
            .enumerate()
            .map(|(i, tool)| {
                let is_selected = focused && mode == ToolListMode::Browse && i == selected_index;
                let style = if is_selected {
                    Theme::selected_item()
                } else {
                    Theme::normal_item()
                };
                let prefix = if is_selected { "▸ " } else { "  " };
                ListItem::new(Line::from(Span::styled(format!("{prefix}{tool}"), style)))
            })
            .collect();
        // Use ListState so ratatui auto-scrolls to keep the selected item visible.
        let mut list_state = ListState::default();
        if focused && mode == ToolListMode::Browse {
            list_state.select(Some(selected_index));
        }
        frame.render_stateful_widget(List::new(items), parts[0], &mut list_state);
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
        Span::styled("+ ", Style::default().fg(Theme::TOOL_ALLOWED)),
        Span::styled(before, Style::default().fg(Theme::TEXT_PRIMARY)),
        Span::styled(cursor_char, Theme::cursor()),
        Span::styled(after, Style::default().fg(Theme::TEXT_PRIMARY)),
    ]);

    frame.render_widget(Paragraph::new(line), area);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_list_height_empty() {
        // Empty list: 1 placeholder row + 2 border = 3
        assert_eq!(tool_list_height(&[], ToolListMode::Browse, false), 3);
    }

    #[test]
    fn tool_list_height_few_items() {
        let tools: Vec<String> = (0..3).map(|i| format!("tool_{i}")).collect();
        // 3 items + 2 border = 5
        assert_eq!(tool_list_height(&tools, ToolListMode::Browse, false), 5);
    }

    #[test]
    fn tool_list_height_capped_at_max_visible() {
        let tools: Vec<String> = (0..20).map(|i| format!("tool_{i}")).collect();
        // Capped to TOOL_LIST_MAX_VISIBLE (6) + 2 border = 8
        assert_eq!(tool_list_height(&tools, ToolListMode::Browse, false), 8);
    }

    #[test]
    fn tool_list_height_exactly_max_visible() {
        let tools: Vec<String> = (0..TOOL_LIST_MAX_VISIBLE)
            .map(|i| format!("tool_{i}"))
            .collect();
        assert_eq!(
            tool_list_height(&tools, ToolListMode::Browse, false),
            TOOL_LIST_MAX_VISIBLE + 2
        );
    }

    #[test]
    fn tool_list_height_adding_mode_adds_input_row() {
        let tools: Vec<String> = (0..3).map(|i| format!("tool_{i}")).collect();
        // 3 items + 1 input + 2 border = 6
        assert_eq!(tool_list_height(&tools, ToolListMode::Adding, true), 6);
    }

    #[test]
    fn tool_list_height_adding_unfocused_no_input_row() {
        let tools: Vec<String> = (0..3).map(|i| format!("tool_{i}")).collect();
        // Input row only shown when focused
        assert_eq!(tool_list_height(&tools, ToolListMode::Adding, false), 5);
    }
}
