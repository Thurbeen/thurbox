//! In-pane editor for creating/editing a task, rendered in the central pane
//! (like the automation editor). Mirrors
//! [`automation_editor_modal`](super::automation_editor_modal) but simpler — a
//! task has no schedule, just a title, status, and optional agent action.

use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
    Frame,
};

use crate::app::{TaskActionKind, TaskField};
use crate::session::TaskStatus;

use super::theme::Theme;

pub struct TaskEditorState<'a> {
    pub editing: bool,
    pub field: TaskField,
    pub status: TaskStatus,
    pub action: TaskActionKind,
    pub title: &'a str,
    pub repo: &'a str,
    pub worktree: &'a str,
    pub base: &'a str,
    pub agent: &'a str,
    /// Display name of the Send target session, if any.
    pub target_session: Option<&'a str>,
    /// Whether the editor currently has keyboard focus. When `false` (an in-pane
    /// preview while the tasks panel is focused), the active-field cursor/
    /// highlight is suppressed and the border is drawn unfocused.
    pub focused: bool,
}

/// Fields shown for the current action kind, in order. Thin re-export of
/// [`TaskActionKind::visible_fields`] so the renderer and the app's field
/// navigation stay in lockstep.
pub fn visible_fields(action: TaskActionKind) -> Vec<TaskField> {
    action.visible_fields()
}

/// Render the editor inline into `area`, framed by a border whose style reflects
/// [`TaskEditorState::focused`] (mirrors `render_automation_editor_into`).
pub fn render_task_editor_into(frame: &mut Frame, area: Rect, state: &TaskEditorState<'_>) {
    let border = if state.focused {
        Theme::border_focused()
    } else {
        Theme::border_unfocused()
    };
    let block = Block::default()
        .title(format!(" {} ", editor_title(state)))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(border));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    frame.render_widget(Paragraph::new(editor_lines(state)), inner);
}

fn editor_title(state: &TaskEditorState<'_>) -> &'static str {
    if state.editing {
        "Edit Task"
    } else {
        "New Task"
    }
}

fn editor_lines<'a>(state: &TaskEditorState<'a>) -> Vec<Line<'a>> {
    let fields = visible_fields(state.action);
    let mut lines: Vec<Line> = fields
        .iter()
        // Only mark the active field when the editor itself is focused; an
        // unfocused in-pane preview renders every field plainly.
        .map(|f| field_line(*f, state, state.focused && *f == state.field))
        .collect();

    lines.push(Line::from(vec![
        Span::styled("Tab/↑↓", Theme::keybind()),
        Span::styled(" move  ", Theme::keybind_desc()),
        Span::styled("←→", Theme::keybind()),
        Span::styled(" adjust  ", Theme::keybind_desc()),
        Span::styled("Enter", Theme::keybind()),
        Span::styled(" save  ", Theme::keybind_desc()),
        Span::styled("Esc", Theme::keybind()),
        Span::styled(" cancel", Theme::keybind_desc()),
    ]));

    lines
}

fn field_line<'a>(
    field: TaskField,
    state: &TaskEditorState<'a>,
    is_active_field: bool,
) -> Line<'a> {
    // (label, value, is_selector). Selector values are wrapped in ‹ › guillemets
    // to signal they are adjusted with ←/→ rather than typed.
    let (label, value, selector): (&str, String, bool) = match field {
        TaskField::Title => ("title", state.title.to_string(), false),
        TaskField::Status => ("status", state.status.label().to_string(), true),
        TaskField::Action => ("action", state.action.label().to_string(), true),
        TaskField::Target => ("target", target_display(state), true),
        TaskField::Repo => ("repo", state.repo.to_string(), false),
        TaskField::Worktree => ("worktree", optional_display(state.worktree), false),
        TaskField::Base => ("base", optional_display(state.base), false),
        TaskField::Agent => ("agent", optional_display(state.agent), false),
    };

    let prefix = if is_active_field { "▸ " } else { "  " };
    let value_style = if is_active_field {
        Style::default()
            .fg(Theme::border_focused())
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Theme::text_primary())
    };

    let display = if selector {
        format!("‹ {value} ›")
    } else if is_active_field {
        format!("{value}_")
    } else {
        value
    };

    Line::from(vec![
        Span::styled(format!("{prefix}{label:<9}"), Theme::label()),
        Span::styled(display, value_style),
    ])
}

fn target_display(state: &TaskEditorState<'_>) -> String {
    state
        .target_session
        .map(|s| s.to_string())
        .unwrap_or_else(|| "(no sessions)".to_string())
}

fn optional_display(value: &str) -> String {
    if value.is_empty() {
        "(none)".to_string()
    } else {
        value.to_string()
    }
}
