//! Tasks-panel UI state (the right-side todo column).
//!
//! Grouped out of the [`App`](super::App) god object. Fields are `pub(crate)`
//! so call-sites keep direct access (`self.task_ui.cached_tasks`).

use std::collections::HashMap;

use super::modals;
use crate::session::{SessionId, Task};

/// UI state backing the tasks panel: the cached task list, panel selection,
/// the live preview/edit editor, and the in-memory task→session links.
#[derive(Default)]
pub(crate) struct TaskUiState {
    /// Cached tasks for the right-side panel, refreshed every ~1 second.
    pub(crate) cached_tasks: Vec<Task>,
    /// Selected row in the tasks panel. Indexes [`Self::filtered_task_indices`].
    pub(crate) task_panel_index: usize,
    /// Indices into [`Self::cached_tasks`] shown in the panel (currently always
    /// all active tasks — filtering is done by the global `Ctrl+A` search).
    pub(crate) filtered_task_indices: Vec<usize>,
    /// The editor for the task currently scoped in the central pane. While the
    /// tasks panel is focused this mirrors the selected task (a live preview);
    /// while [`InputFocus::TaskEditor`](super::InputFocus::TaskEditor) is
    /// focused it holds the in-progress edits. `None` when no task is scoped.
    /// Mirrors `automation_editor`.
    pub(crate) task_editor: Option<modals::TaskEditorModal>,
    /// Vertical scroll offset (rows) for the full-screen task preview shown
    /// while the tasks panel is focused. Reset when the selection changes.
    pub(crate) task_preview_scroll: u16,
    /// A task-initiated spawn in flight: `(task_id, title)`. Set when the
    /// trigger-time action picker chooses "Spawn new session", consumed by the
    /// spawn flow's success tail to deliver the prompt + advance the task.
    pub(crate) pending_task_prompt: Option<(i64, String)>,
    /// Live `task id → session id` links recorded when a task is triggered from
    /// the TUI (Spawn-new or Send). TUI spawns get a user-chosen session name
    /// rather than the `<title> · #<id>` convention, so the name alone can't
    /// recover the link — this map does. Consulted by
    /// `task_related_session_indices` alongside the name convention and any
    /// persisted `Send` action. In-memory only (the spawn-name convention is
    /// what survives a restart); stale entries are harmless since lookups
    /// filter to currently-open sessions.
    pub(crate) task_session_links: HashMap<i64, SessionId>,
}
