//! Tasks panel: the right-side todo list and its in-pane editor.
//!
//! Relocated from `app/mod.rs` and `app/key_handlers.rs` as ADR-22 step 1 — a
//! pure behavioral relocation of the Tasks-cluster `impl App` methods (no
//! field moves, no behavior change, identical signatures/visibility). The task
//! state itself stays in [`super::task_state::TaskUiState`] (`App::task_ui`).

use super::modals;
use super::{App, InputFocus, StatusLevel};
use crate::session::SessionId;
use crossterm::event::KeyCode;
use tracing::error;

impl App {
    /// Refresh the cached tasks from the database, keeping the filtered view and
    /// panel selection valid.
    pub(crate) fn refresh_tasks(&mut self) {
        match self.db.list_tasks() {
            Ok(tasks) => self.task_ui.cached_tasks = tasks,
            Err(e) => error!("Failed to list tasks: {e}"),
        }
        self.recompute_task_filter();
    }

    /// Rebuild [`Self::filtered_task_indices`] (all active tasks) and clamp the
    /// panel selection into range. The tasks panel shows every task now —
    /// filtering happens through the global `Ctrl+/` search.
    pub(crate) fn recompute_task_filter(&mut self) {
        self.task_ui.filtered_task_indices = (0..self.task_ui.cached_tasks.len()).collect();
        if self.task_ui.filtered_task_indices.is_empty() {
            self.task_ui.task_panel_index = 0;
        } else {
            self.task_ui.task_panel_index = self
                .task_ui
                .task_panel_index
                .min(self.task_ui.filtered_task_indices.len() - 1);
        }
    }

    /// The task currently selected in the panel (honoring the filter), if any.
    pub(crate) fn selected_task(&self) -> Option<&crate::session::Task> {
        let idx = *self
            .task_ui
            .filtered_task_indices
            .get(self.task_ui.task_panel_index)?;
        self.task_ui.cached_tasks.get(idx)
    }

    /// Indices into `self.sessions` of the **currently-open** sessions a task is
    /// related to, in display order:
    ///
    /// - the session named by the spawn convention (`<title> · #<id>`, or the
    ///   legacy `task-<id>-<slug>` / bare `task-<id>`) — used by the headless `task run`
    ///   (and what survives a restart; see [`Task::matches_spawn_session`]);
    /// - the target of a persisted `Send` action (`task.action`), when one is
    ///   set via the CLI; and
    /// - the in-memory `task_session_links` entry recorded when the task was
    ///   triggered from the TUI this run (TUI spawns get a user-chosen name,
    ///   not the spawn convention, so this is how that link is recovered).
    ///
    /// Deduplicated. Empty when nothing related is open right now. This is the
    /// single source of truth for both the details panel and the *open* key.
    pub(crate) fn task_related_session_indices(&self, task: &crate::session::Task) -> Vec<usize> {
        // Persisted `Send` action target (CLI-authored), plus the in-memory link
        // recorded when this task was triggered from the TUI this run.
        let send_target = match &task.action {
            Some(crate::session::AutomationAction::Send { session_id }) => Some(*session_id),
            _ => None,
        };
        let linked = self.task_ui.task_session_links.get(&task.id).copied();
        let mut out = Vec::new();
        for (i, s) in self.sessions.iter().enumerate() {
            let related = task.matches_spawn_session(&s.info.name)
                || send_target.is_some_and(|t| t == s.info.id)
                || linked.is_some_and(|t| t == s.info.id);
            if related && !out.contains(&i) {
                out.push(i);
            }
        }
        out
    }

    /// Jump to the task's related session terminal (the first open one). Bound
    /// to `o` in the focused tasks panel; mirrors `open_run_related_session`.
    pub(crate) fn open_task_related_session(&mut self) {
        let Some(task) = self.selected_task().cloned() else {
            return;
        };
        match self.task_related_session_indices(&task).first() {
            Some(&idx) => {
                self.active_index = idx;
                self.focus = InputFocus::Terminal;
                // Leaving the tasks context clears the editor/preview cache.
                self.refresh_task_view();
            }
            None => self.set_status(
                StatusLevel::Info,
                "Task has no open session — run it with r",
            ),
        }
    }

    /// Scroll the full-screen task preview by `delta` rows, clamped to the
    /// rendered description length (a slight over-scroll is harmless).
    pub(crate) fn scroll_task_preview(&mut self, delta: i32) {
        let max = self.task_preview_max_scroll() as i32;
        let next = (self.task_ui.task_preview_scroll as i32 + delta).clamp(0, max);
        self.task_ui.task_preview_scroll = next as u16;
    }

    /// Largest valid `task_preview_scroll` for the selected task's description.
    ///
    /// Matches the line set that `ui::task_detail::render_task_detail` scrolls:
    /// the rendered markdown plus the one-row `description` header it prepends.
    /// Shared by keyboard (`PageUp`/`PageDown`), the wheel, and the scrollbar
    /// drag so all three agree on the clamp.
    pub(crate) fn task_preview_max_scroll(&self) -> u16 {
        let body = self
            .selected_task()
            .and_then(|t| t.description.as_deref())
            .filter(|d| !d.trim().is_empty())
            .map(|d| crate::ui::markdown::render_markdown(d).len())
            .unwrap_or(0);
        // `content_len = body + 1` (the header row); max index is `content_len - 1`.
        body as u16
    }

    /// Open the trigger-time action picker for `task`: one **Send** entry per
    /// running session, plus **Spawn new session…**. The chosen
    /// action runs immediately (nothing is persisted on the task).
    pub(crate) fn open_task_action_picker(&mut self, task: &crate::session::Task) {
        use modals::{Modal, TaskActionChoice, TaskActionPickerModal};
        let mut choices: Vec<TaskActionChoice> = self
            .session_target_choices()
            .into_iter()
            .map(|(id, name)| TaskActionChoice::Send(id, name))
            .collect();
        choices.push(TaskActionChoice::SpawnNew);
        self.modal = Modal::TaskActionPicker(TaskActionPickerModal {
            task_id: task.id,
            title: task.title.clone(),
            choices,
            selected: 0,
        });
    }

    /// Send a task's prompt to an existing session and advance it to
    /// `InProgress`.
    pub(crate) fn send_task_to_session(
        &mut self,
        task_id: i64,
        title: &str,
        status: crate::session::TaskStatus,
        session_id: SessionId,
    ) {
        let Some(name) = self
            .sessions
            .iter()
            .find(|s| s.info.id == session_id)
            .map(|s| s.info.name.clone())
        else {
            self.set_error("Target session is not running");
            return;
        };
        let prompt = self.task_agent_prompt(task_id, title);
        self.send_prompt_to_session(session_id, &prompt, 0);
        self.task_ui.task_session_links.insert(task_id, session_id);
        self.advance_task_to_in_progress(task_id, status);
        self.refresh_tasks();
        self.set_status(StatusLevel::Success, format!("Sent task to {name}"));
    }

    /// Advance a task `Todo → InProgress` (no-op for other states) now that an
    /// agent is acting on it. Shared by the Send and Spawn trigger paths.
    pub(crate) fn advance_task_to_in_progress(
        &mut self,
        task_id: i64,
        status: crate::session::TaskStatus,
    ) {
        if status == crate::session::TaskStatus::Todo {
            if let Err(e) = self
                .db
                .set_task_status(task_id, crate::session::TaskStatus::InProgress)
            {
                error!("Failed to advance task {task_id} status: {e}");
            }
        }
    }

    /// A blank task editor (title + description + status only).
    fn blank_task_editor(&self) -> modals::TaskEditorModal {
        modals::TaskEditorModal::new()
    }

    /// Build an editor pre-filled from an existing task.
    fn build_task_editor(&self, task: &crate::session::Task) -> modals::TaskEditorModal {
        modals::TaskEditorModal::from_task(task)
    }

    /// Keep the in-pane task editor (`self.task_ui.task_editor`) in sync with focus +
    /// selection:
    /// - [`InputFocus::TaskList`] → mirror the selected task (live preview).
    /// - [`InputFocus::TaskEditor`] → leave in-progress edits untouched.
    /// - anything else → drop it (we left the tasks context). Mirrors
    ///   [`Self::sync_automation_editor`].
    pub(crate) fn sync_task_editor(&mut self) {
        match self.focus {
            InputFocus::TaskList => {
                // A fresh preview starts unscrolled.
                self.task_ui.task_preview_scroll = 0;
                self.task_ui.task_editor = self
                    .selected_task()
                    .cloned()
                    .map(|task| self.build_task_editor(&task));
            }
            InputFocus::TaskEditor => {}
            _ => self.task_ui.task_editor = None,
        }
    }

    /// Re-sync the in-pane task editor preview after the selection or a task
    /// itself changes.
    pub(crate) fn refresh_task_view(&mut self) {
        self.sync_task_editor();
    }

    /// Start a brand-new task in the central pane and focus the editor.
    pub(crate) fn new_task_in_pane(&mut self) {
        self.task_ui.task_editor = Some(self.blank_task_editor());
        self.focus = InputFocus::TaskEditor;
    }

    /// Move focus into the central-pane editor for the selected task (mirrors
    /// `Enter` on a session focusing its terminal).
    pub(crate) fn enter_task_editor(&mut self) {
        self.sync_task_editor();
        if self.task_ui.task_editor.is_some() {
            self.focus = InputFocus::TaskEditor;
        }
    }

    /// Handle keys while the tasks panel is focused: `j`/`k` select (and preview
    /// the selected task in the central pane), `PageUp`/`PageDown` scroll that
    /// preview, `n` create, `e`/`Enter` open the central-pane editor, `Space`
    /// cycles status, `r` opens the trigger-time action picker, `d` deletes,
    /// `Esc` leaves. Searching is handled by the global `Ctrl+/`.
    pub(crate) fn handle_task_list_key(&mut self, code: KeyCode) {
        // Creating works regardless of whether the panel has entries.
        if matches!(code, KeyCode::Char('n')) {
            self.new_task_in_pane();
            return;
        }

        let count = self.task_ui.filtered_task_indices.len();
        match code {
            KeyCode::Char('j') | KeyCode::Down
                if count > 0 && self.task_ui.task_panel_index + 1 < count =>
            {
                self.task_ui.task_panel_index += 1;
                self.refresh_task_view();
            }
            KeyCode::Char('j') | KeyCode::Down => {}
            KeyCode::Char('k') | KeyCode::Up => {
                self.task_ui.task_panel_index = self.task_ui.task_panel_index.saturating_sub(1);
                self.refresh_task_view();
            }
            KeyCode::Esc => {
                self.focus = InputFocus::SessionList;
                self.task_ui.task_editor = None;
            }
            // Open the central-pane editor for the selected task. On an empty
            // panel, start a new task instead.
            KeyCode::Char('e') | KeyCode::Enter => self.enter_task_editor_or_new(count),
            // Scroll the full-screen preview (the list itself uses j/k).
            KeyCode::PageDown => self.scroll_task_preview(5),
            KeyCode::PageUp => self.scroll_task_preview(-5),
            KeyCode::Char(' ') => self.cycle_selected_task_status(),
            // Open the trigger-time action picker (Send → session / Spawn new).
            KeyCode::Char('r') => self.open_selected_task_action_picker(),
            // Jump to the task's related session terminal (the spawned
            // `<title> · #<id>` window, or a persisted Send target).
            KeyCode::Char('o') => self.open_task_related_session(),
            KeyCode::Char('d') => self.delete_selected_task(),
            _ => {}
        }
    }

    /// `e`/`Enter` on the tasks panel: open the central-pane editor for the
    /// selection, or start a new task when the panel is empty.
    fn enter_task_editor_or_new(&mut self, count: usize) {
        if count == 0 {
            self.new_task_in_pane();
        } else {
            self.enter_task_editor();
        }
    }

    /// `r` on the tasks panel: open the trigger-time action picker for the
    /// selected task (Send → session / Spawn new).
    fn open_selected_task_action_picker(&mut self) {
        if let Some(task) = self.selected_task().cloned() {
            self.open_task_action_picker(&task);
        }
    }

    /// Cycle the selected task's status (`Todo → InProgress → Done`) and refresh.
    fn cycle_selected_task_status(&mut self) {
        let Some(task) = self.selected_task().cloned() else {
            return;
        };
        if let Err(e) = self.db.set_task_status(task.id, task.status.cycle()) {
            error!("Failed to cycle task {} status: {e}", task.id);
        }
        self.refresh_tasks();
        self.sync_task_editor();
    }

    /// Soft-delete the selected task, then clamp the selection and refresh.
    fn delete_selected_task(&mut self) {
        let Some(id) = self.selected_task().map(|t| t.id) else {
            return;
        };
        if let Err(e) = self.db.soft_delete_task(id) {
            error!("Failed to delete task {id}: {e}");
        }
        self.refresh_tasks();
        let new_count = self.task_ui.filtered_task_indices.len();
        if new_count > 0 && self.task_ui.task_panel_index >= new_count {
            self.task_ui.task_panel_index = new_count - 1;
        }
        self.refresh_task_view();
    }
}
