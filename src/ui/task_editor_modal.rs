//! In-pane editor for creating/editing a task, rendered full-screen in the
//! central pane when the task editor is focused. A task is just a title, a
//! multi-line markdown description, and a status — the agent action is chosen
//! at trigger time, not authored here.

use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use super::theme::Theme;
use crate::app::TaskField;
use crate::session::TaskStatus;

pub struct TaskEditorState<'a> {
    pub editing: bool,
    pub field: TaskField,
    pub status: TaskStatus,
    pub title: &'a str,
    /// Caret position within the **active** text field, for drawing the in-text
    /// cursor where editing is happening. Meaningful only when [`Self::field`]
    /// is a typeable (non-selector) field and the editor is [`Self::focused`].
    pub active_cursor: usize,
    /// Multi-line markdown description text.
    pub description: &'a str,
    /// Caret `(line, col)` within the description, for drawing the block cursor
    /// while the `Description` field is active.
    pub description_cursor: (usize, usize),
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
            title: m.title.value(),
            active_cursor: m.active_field().map(|f| f.cursor_pos()).unwrap_or(0),
            description: m.description.value(),
            description_cursor: m.description.cursor_line_col(),
            focused,
        }
    }
}

/// Render the editor inline into `area`, framed by a border whose style reflects
/// [`TaskEditorState::focused`] (mirrors `render_automation_editor_into`).
/// Returns one click hitbox per field (Title row, the whole Description block,
/// Status row), indexed by position in the task's visible field order
/// (Title=0, Description=1, Status=2). The caller records them as `PaneField`.
pub fn render_task_editor_into(
    frame: &mut Frame,
    area: Rect,
    state: &TaskEditorState<'_>,
) -> Vec<super::RowHitbox> {
    let inner = super::render_editor_frame(frame, area, editor_title(state), state.focused);
    frame.render_widget(Paragraph::new(editor_lines(state)), inner);

    // Row layout (matches `editor_lines`): Title=1 row, Description=1 header +
    // one row per newline-separated line, Status=1 row. Build a hitbox per
    // field, clipped to the inner area.
    let desc_rows = 1 + state.description.split('\n').count() as u16;
    let mut y = 0u16;
    let mut hits = Vec::new();
    let mut push = |y: &mut u16, height: u16, index: usize| {
        if *y < inner.height {
            let h = height.min(inner.height - *y);
            hits.push(super::RowHitbox {
                rect: Rect::new(inner.x, inner.y + *y, inner.width, h),
                index,
            });
        }
        *y = y.saturating_add(height);
    };
    push(&mut y, 1, 0); // Title
    push(&mut y, desc_rows, 1); // Description (header + text rows)
    push(&mut y, 1, 2); // Status
    hits
}

fn editor_title(state: &TaskEditorState<'_>) -> &'static str {
    if state.editing {
        "Edit Task"
    } else {
        "New Task"
    }
}

fn editor_lines<'a>(state: &TaskEditorState<'a>) -> Vec<Line<'a>> {
    let mut lines: Vec<Line> = Vec::new();
    for f in [TaskField::Title, TaskField::Description, TaskField::Status] {
        let active = state.focused && f == state.field;
        // Only mark the active field when the editor itself is focused; an
        // unfocused in-pane preview renders every field plainly. The
        // multi-line description expands into several rows.
        if f == TaskField::Description {
            lines.extend(description_lines(state, active));
        } else {
            lines.push(field_line(f, state, active));
        }
    }

    lines.push(super::key_hint_line(&[
        ("Tab/↑↓", " move  "),
        ("←→", " adjust  "),
        ("Enter", " save  "),
        ("^S", " save  "),
        ("Esc", " cancel"),
    ]));

    lines
}

/// Render the description as a `▸ description` label row followed by its text
/// rows (indented), drawing a block cursor at `state.description_cursor` when
/// the field is active. An empty description shows a dimmed `(none)`.
fn description_lines<'a>(state: &TaskEditorState<'a>, active: bool) -> Vec<Line<'a>> {
    let prefix = if active { "▸ " } else { "  " };
    let mut lines = vec![Line::from(Span::styled(
        format!("{prefix}{:<9}", "description"),
        Theme::label(),
    ))];

    let text_style = if active {
        Style::default()
            .fg(Theme::border_focused())
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Theme::text_primary())
    };

    if state.description.is_empty() && !active {
        lines.push(Line::from(Span::styled(
            "    (none)",
            Style::default().fg(Theme::text_secondary()),
        )));
        return lines;
    }

    lines.extend(description_text_rows(state, active, text_style));
    lines
}

/// Build the indented text rows of the description, drawing a block cursor on
/// the cursor's row when `active`.
fn description_text_rows<'a>(
    state: &TaskEditorState<'a>,
    active: bool,
    text_style: Style,
) -> Vec<Line<'a>> {
    let (cur_line, cur_col) = state.description_cursor;
    state
        .description
        .split('\n')
        .enumerate()
        .map(|(li, text)| {
            if active && li == cur_line {
                description_cursor_line(text, cur_col, text_style)
            } else {
                description_plain_line(text, text_style)
            }
        })
        .collect()
}

/// Render one description row with a block cursor drawn at `cur_col`.
fn description_cursor_line<'a>(text: &str, cur_col: usize, text_style: Style) -> Line<'a> {
    let chars: Vec<char> = text.chars().collect();
    let mut spans = vec![Span::raw("    ")];
    super::push_block_cursor(&mut spans, &chars, cur_col, text_style);
    Line::from(spans)
}

/// Render one description row without a cursor (indented plain text).
fn description_plain_line<'a>(text: &str, text_style: Style) -> Line<'a> {
    let mut spans = vec![Span::raw("    ")];
    if !text.is_empty() {
        spans.push(Span::styled(text.to_string(), text_style));
    }
    Line::from(spans)
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
        // Description renders via `description_lines`, never as a one-row field.
        TaskField::Description => ("description", state.description.to_string(), false),
    };

    // Every non-selector field is a typeable text input, so feed the active
    // field's caret through to draw the block cursor where editing happens.
    let cursor = (is_active_field && !selector).then_some(state.active_cursor);
    super::editor_field_line_with_cursor(label, value, selector, is_active_field, cursor)
}
