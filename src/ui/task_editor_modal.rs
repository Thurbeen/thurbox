//! In-pane editor for creating/editing a task, rendered in the central pane
//! (like the automation editor). Mirrors
//! [`automation_editor_modal`](super::automation_editor_modal) but simpler — a
//! task has no schedule, just a title, status, and optional agent action.

use ratatui::{layout::Rect, text::Line, widgets::Paragraph, Frame};

use crate::app::{TaskActionKind, TaskField};
use crate::session::TaskStatus;

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

impl<'a> TaskEditorState<'a> {
    /// Borrow view data from a task-editor modal. Keeps the field projection in
    /// one place (mirrors `AutomationEditorState::from_modal`).
    pub fn from_modal(m: &'a crate::app::modals::TaskEditorModal, focused: bool) -> Self {
        Self {
            editing: m.editing_id.is_some(),
            field: m.field,
            status: m.status,
            action: m.action,
            title: m.title.value(),
            repo: m.repo.value(),
            worktree: m.worktree.value(),
            base: m.base.value(),
            agent: m.agent.value(),
            target_session: m.selected_target().map(|(_, n)| n.as_str()),
            focused,
        }
    }
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
    let inner = super::render_editor_frame(frame, area, editor_title(state), state.focused);
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

    lines.push(super::key_hint_line(&[
        ("Tab/↑↓", " move  "),
        ("←→", " adjust  "),
        ("Enter", " save  "),
        ("Esc", " cancel"),
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

    super::editor_field_line(label, value, selector, is_active_field)
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
