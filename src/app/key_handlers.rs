//! Key event handlers for the Thurbox TUI application.
//!
//! This module contains all keyboard input handling logic organized by context:
//! - Global keybindings (always active)
//! - Focus-based handlers (ProjectList, SessionList, Terminal)
//! - Modal handlers (RepoPicker, BranchSelector, AgentPicker, etc.)

use crate::session::SessionConfig;

use super::{App, InputFocus, TerminalView};
use crate::agent::input;
use crate::paths;
use crossterm::event::{KeyCode, KeyModifiers};
use tracing::{error, warn};

/// Convert a session name into a git-branch-friendly name.
///
/// Lowercases, replaces spaces/underscores with hyphens, drops other
/// non-alphanumeric chars, collapses consecutive hyphens, and trims
/// leading/trailing hyphens.
fn session_name_to_branch(name: &str) -> String {
    let mut result = String::with_capacity(name.len());
    for c in name.chars() {
        if c.is_alphanumeric() {
            result.push(c.to_ascii_lowercase());
        } else if (c == ' ' || c == '-' || c == '_') && !result.ends_with('-') {
            result.push('-');
        }
    }
    result.trim_matches('-').to_string()
}

impl App {
    /// Main key handler dispatcher.
    ///
    /// Routes key events to the appropriate handler based on:
    /// 1. Modal state (highest priority)
    /// 2. Global keybindings (Ctrl+Q, Ctrl+N, etc.)
    /// 3. Focus-based handlers (ProjectList, SessionList, Terminal)
    pub(crate) fn handle_key(&mut self, code: KeyCode, mods: KeyModifiers) {
        // Help overlay + clipboard chords are routed before any modal handler.
        if self.handle_priority_key(code, mods) {
            return;
        }

        // An open modal captures all input.
        if self.handle_modal_key_if_open(code, mods) {
            return;
        }

        // The global-search strip captures all input while open (typed chars
        // edit the query; arrows/Enter/Esc navigate/activate/close).
        if self.global_search.active {
            self.handle_global_search_key(code, mods);
            return;
        }

        // Any key press clears text selection (but the key still performs its action)
        self.text_selection = None;

        // The in-pane automation editor / run-history capture input like the
        // overlay modal (see `handle_automation_pane_capture`).
        if self.handle_automation_pane_capture(code, mods) {
            return;
        }

        // Keybinding lookup, scoped to the focused pane: global actions plus
        // any scoped to the current context (file viewer, session list,
        // terminal). Some actions intentionally yield to the PTY when the
        // terminal is focused (e.g. Ctrl+R = bash reverse-search) — those are
        // handled inside `dispatch_action`.
        let context = self.focus_key_context();
        if let Some(action) = self.keybindings.lookup_in(context, code, mods) {
            if self.dispatch_action(action) {
                return;
            }
        }

        match self.focus {
            InputFocus::SessionList => self.handle_session_list_key(code),
            InputFocus::Automations => self.handle_automations_pane_key(code),
            InputFocus::AutomationEditor => self.handle_automation_editor_pane_key(code, mods),
            InputFocus::AutomationRunHistory => self.handle_automation_run_history_key(code),
            InputFocus::TaskList => self.handle_task_list_key(code),
            InputFocus::TaskEditor => self.handle_task_editor_pane_key(code, mods),
            // The global-search strip captures input earlier (before the global
            // keybinding lookup), so this arm is effectively unreachable.
            InputFocus::GlobalSearch => self.handle_global_search_key(code, mods),
            InputFocus::Terminal => self.handle_terminal_key(code, mods),
            InputFocus::FileViewer => self.handle_file_viewer_key(code, mods),
        }
    }

    /// The keybinding [`KeyContext`](crate::session::KeyContext) for the
    /// current focus, used to scope the lookup. Single-letter scoped actions
    /// (file viewer / session list) only resolve here, so the terminal keeps
    /// forwarding those keys to the PTY. While the file-viewer search field is
    /// active we fall back to `Global` so typed letters edit the query instead
    /// of navigating.
    pub(crate) fn focus_key_context(&self) -> crate::session::KeyContext {
        use crate::session::KeyContext;
        match self.focus {
            InputFocus::SessionList => KeyContext::SessionList,
            InputFocus::FileViewer if !self.file_viewer.search_active => KeyContext::FileViewer,
            InputFocus::Terminal => KeyContext::Terminal,
            _ => KeyContext::Global,
        }
    }

    /// Help-overlay dismissal and clipboard chords, routed ahead of modal
    /// handlers. Returns `true` if the key was consumed.
    fn handle_priority_key(&mut self, code: KeyCode, mods: KeyModifiers) -> bool {
        // The interactive help/keybinding editor captures all input — routed
        // ahead of the global keybinding lookup so a chord being captured
        // (e.g. ctrl+q) rebinds rather than triggering its action (quit).
        if matches!(self.modal, super::modals::Modal::Help(_)) {
            return self.handle_help_key(code, mods);
        }

        // Clipboard chords (Copy/Paste) are user-rebindable global actions but
        // routed here, ahead of modal handlers, so Paste reaches modal/terminal
        // text inputs and Copy works from inside any modal. Resolved via the
        // (global) keybindings so a user's rebind takes effect.
        match self.keybindings.lookup(code, mods) {
            // Paste always consumes — `paste_from_clipboard` knows whether a
            // modal text input is open and routes the text accordingly.
            Some(crate::session::Action::Paste) => {
                self.paste_from_clipboard();
                return true;
            }
            // Copy consumes only with an active selection; otherwise it falls
            // through to the normal handlers (e.g. SIGINT when the terminal is
            // focused — see `dispatch_action`'s `Copy` arm).
            Some(crate::session::Action::Copy) if self.text_selection.is_some() => {
                self.copy_selection_to_clipboard();
                return true;
            }
            _ => {}
        }

        false
    }

    /// Interactive F1 help / keybinding editor. Always consumes input while
    /// the help modal is open (returns `true`).
    ///
    /// Navigation mode: `j`/`k` (or arrows) move the selection, `Enter`/`r`
    /// begins capturing a new chord for the selected action, `d` resets the
    /// selected action to its default(s), `Shift+D` resets *all* actions, and
    /// `F1`/`Esc` close the overlay.
    ///
    /// Capture mode: the next keypress (any chord, including `ctrl+q` or `f1`)
    /// becomes the action's sole binding; `Esc` cancels without rebinding.
    fn handle_help_key(&mut self, code: KeyCode, mods: KeyModifiers) -> bool {
        use crate::session::{Action, KeyChord};

        let actions = Action::rebindable_in_order();
        let super::modals::Modal::Help(ref mut help) = self.modal else {
            return true;
        };

        if help.capturing {
            if code == KeyCode::Esc {
                help.capturing = false;
                return true;
            }
            let chord = KeyChord { mods, code };
            let selected = help.selected.min(actions.len().saturating_sub(1));
            help.capturing = false;
            let action = actions[selected];
            let stolen = self.keybindings.rebind(action, chord);
            self.persist_keybindings();
            if let Some(other) = stolen {
                self.set_info(format!(
                    "{} reassigned from '{}'",
                    chord.display(),
                    other.label()
                ));
            }
            return true;
        }

        match code {
            KeyCode::Esc | KeyCode::F(1) => self.modal.close(),
            KeyCode::Char('j') | KeyCode::Down => {
                help.selected = (help.selected + 1).min(actions.len().saturating_sub(1));
            }
            KeyCode::Char('k') | KeyCode::Up => {
                help.selected = help.selected.saturating_sub(1);
            }
            KeyCode::Enter | KeyCode::Char('r') => help.capturing = true,
            KeyCode::Char('d') => {
                let selected = help.selected.min(actions.len().saturating_sub(1));
                let action = actions[selected];
                self.keybindings.reset(action);
                self.persist_keybindings();
            }
            // Shift+D resets every action to its built-in default.
            KeyCode::Char('D') => self.reset_all_keybindings(),
            _ => {}
        }
        true
    }

    /// Restore every keybinding to its compiled-in default and remove the
    /// user override file so defaults remain authoritative. Surfaces failures
    /// via the status bar; the in-memory map is reset regardless.
    fn reset_all_keybindings(&mut self) {
        self.keybindings = crate::session::KeyBindings::default();
        if let Err(e) = crate::storage::keybindings::delete_keybindings_json() {
            self.set_error(format!("Failed to reset keybindings: {e}"));
        } else {
            self.set_info("All keybindings reset to defaults");
        }
    }

    /// Serialize the current keybindings and write them to
    /// `~/.config/thurbox/keybindings.json`. Surfaces failures via the status
    /// bar rather than aborting — the in-memory map is already updated.
    fn persist_keybindings(&mut self) {
        match self.keybindings.to_json() {
            Ok(json) => {
                if let Err(e) = crate::storage::keybindings::save_keybindings_json(&json) {
                    self.set_error(format!("Failed to save keybindings: {e}"));
                }
            }
            Err(e) => self.set_error(format!("Failed to serialize keybindings: {e}")),
        }
    }

    /// Route the key to the open modal's handler, if any. Returns `true` if a
    /// modal was open and consumed the key.
    fn handle_modal_key_if_open(&mut self, code: KeyCode, mods: KeyModifiers) -> bool {
        use super::modals::Modal;
        match self.modal {
            Modal::RestoreSessions(_) => self.handle_restore_sessions_key(code),
            Modal::BranchSelector(_) => self.handle_branch_selector_key(code),
            Modal::WorktreeName(_) => self.handle_worktree_name_key(code),
            Modal::SessionName(_) => self.handle_session_name_key(code),
            Modal::AutomationEditor(_) => self.handle_automation_editor_key(code, mods),
            Modal::AutomationsList(_) => self.handle_automations_list_key(code),
            Modal::AgentPicker(_) => self.handle_agent_picker_key(code),
            Modal::ThemePicker(_) => self.handle_theme_picker_key(code),
            Modal::RepoPicker(_) => self.handle_repo_picker_key(code),
            _ => return false,
        }
        true
    }

    /// The in-pane automation/task editor + run-history capture input like the
    /// overlay modal — so editor chords (e.g. `e`, `d`, Ctrl+E) reach the form
    /// instead of firing a global binding. Focus navigation (Ctrl+L/H) and quit
    /// still pass through to the global handler so you can move between panes.
    /// Returns `true` if consumed.
    fn handle_automation_pane_capture(&mut self, code: KeyCode, mods: KeyModifiers) -> bool {
        if !matches!(
            self.focus,
            InputFocus::AutomationEditor
                | InputFocus::AutomationRunHistory
                | InputFocus::TaskEditor
        ) {
            return false;
        }
        let passthrough = matches!(
            self.keybindings.lookup(code, mods),
            Some(
                crate::session::Action::FocusForward
                    | crate::session::Action::FocusBackward
                    | crate::session::Action::QuitApp
            )
        );
        if passthrough {
            return false;
        }
        match self.focus {
            InputFocus::AutomationEditor => self.handle_automation_editor_pane_key(code, mods),
            InputFocus::AutomationRunHistory => self.handle_automation_run_history_key(code),
            InputFocus::TaskEditor => self.handle_task_editor_pane_key(code, mods),
            _ => unreachable!(),
        }
        true
    }

    /// The ordered focus ring for the **current context**. `Ctrl+L`/`Ctrl+H`
    /// cycle *within* this ring; switching between the session and automation
    /// contexts is done with `j`/`k` in the left column (not the focus cycle).
    ///
    /// - Session context: `SessionList → Terminal` (+ `TaskList` then
    ///   `FileViewer` when those panels are shown — each is a cycle stop while
    ///   visible, exactly like the file viewer).
    /// - Automation context: `Automations → editor` (+ `run history` for an
    ///   existing automation).
    ///
    /// So cycling out of the automation editor/history wraps back to the
    /// **Automations** pane (returning to the selected automation, like `Esc`),
    /// never off to a session.
    fn focus_ring(&self) -> Vec<InputFocus> {
        use InputFocus::*;
        match self.focus {
            Automations | AutomationEditor | AutomationRunHistory => {
                let mut ring = vec![Automations, AutomationEditor];
                // The run-history panel exists only for an existing automation.
                if self.scoped_automation_id().is_some() {
                    ring.push(AutomationRunHistory);
                }
                ring
            }
            // The tasks panel joins the session ring so `Ctrl+L`/`Ctrl+H` move
            // in and out of it like any other pane (it lives in the right column,
            // not the left-column circular list). `Esc` still drops straight back
            // to the session list.
            SessionList | Terminal | FileViewer | TaskList => {
                // Order mirrors the on-screen columns: terminal → tasks → files.
                let mut ring = vec![SessionList, Terminal];
                if self.show_tasks_panel {
                    ring.push(TaskList);
                }
                if self.show_file_viewer {
                    ring.push(FileViewer);
                }
                ring
            }
            // While editing a task in the central pane, the ring is
            // `TaskList → editor` (like the automation editor): cycling out of
            // the editor returns to the tasks panel, never off to a session.
            TaskEditor => vec![TaskList, TaskEditor],
            // The global-search strip is entered/left only via its keybinding
            // (`Ctrl+A` by default) / `Esc`, so `Ctrl+L`/`Ctrl+H` are no-ops
            // while it's open.
            GlobalSearch => vec![GlobalSearch],
        }
    }

    /// Cycle focus forward (Ctrl+L) within the current context's ring.
    pub(crate) fn cycle_focus_forward(&self) -> InputFocus {
        let ring = self.focus_ring();
        let pos = ring.iter().position(|f| *f == self.focus).unwrap_or(0);
        ring[(pos + 1) % ring.len()]
    }

    /// Cycle focus backward (Ctrl+H) within the current context's ring.
    pub(crate) fn cycle_focus_backward(&self) -> InputFocus {
        let ring = self.focus_ring();
        let pos = ring.iter().position(|f| *f == self.focus).unwrap_or(0);
        ring[(pos + ring.len() - 1) % ring.len()]
    }

    /// Shared bookkeeping after a focus change via the `Ctrl+L`/`Ctrl+H` cycle:
    /// keep the in-pane editor + run history in sync, and start the run-history
    /// selection at the top (newest run) when entering that panel.
    fn on_focus_changed(&mut self) {
        if self.focus == InputFocus::AutomationRunHistory {
            self.automation_run_index = 0;
        }
        // Entering the tasks panel via the cycle: refresh the task list and the
        // in-pane editor preview.
        if self.focus == InputFocus::TaskList {
            self.refresh_tasks();
        }
        if matches!(self.focus, InputFocus::TaskList | InputFocus::TaskEditor) {
            self.refresh_task_view();
        }
        self.refresh_automation_view();
    }

    /// Handle keys while the automations pane is focused: navigate and drive the
    /// same toggle/run/edit/delete actions as the Ctrl+P modal. (Ctrl+N to
    /// create is handled globally in `dispatch_action`, so it works here too,
    /// including on an empty pane.)
    pub(crate) fn handle_automations_pane_key(&mut self, code: KeyCode) {
        // Creating works regardless of whether the pane has entries.
        if matches!(code, KeyCode::Char('n')) {
            self.new_automation_in_pane();
            return;
        }
        let count = self.cached_automations.len();
        // `k`/Up at the top row (or in an empty pane) flows focus back up into
        // the session list, so the left column behaves as one vertical list.
        if matches!(code, KeyCode::Char('k') | KeyCode::Up) {
            if self.automation_panel_index == 0 || count == 0 {
                self.focus = InputFocus::SessionList;
                self.select_last_session();
            } else {
                self.automation_panel_index -= 1;
            }
            self.refresh_automation_view();
            return;
        }
        // `j`/Down past the last automation (or in an empty pane) loops around to
        // the TOP of the session list, so sessions+automations form one circular
        // column.
        if matches!(code, KeyCode::Char('j') | KeyCode::Down) {
            if count == 0 || self.automation_panel_index + 1 >= count {
                self.focus = InputFocus::SessionList;
                self.select_first_session();
            } else {
                self.automation_panel_index += 1;
            }
            self.refresh_automation_view();
            return;
        }
        // `Enter`/`e` focuses the central-pane editor (like `Enter` on a session
        // focuses its terminal); on an empty pane it starts a new automation.
        if matches!(code, KeyCode::Enter | KeyCode::Char('e')) {
            if count == 0 {
                self.new_automation_in_pane();
            } else {
                self.enter_automation_editor();
            }
            self.refresh_selected_automation_runs();
            return;
        }
        if count == 0 {
            return; // empty pane: remaining nav/actions are no-ops
        }
        if self.automation_panel_index >= count {
            self.automation_panel_index = count - 1;
        }
        let id = self.cached_automations[self.automation_panel_index].id;
        match code {
            KeyCode::Char(' ') => {
                self.toggle_automation_by_id(id);
                self.sync_automation_editor();
            }
            KeyCode::Char('r') => self.run_automation_by_id(id),
            KeyCode::Char('d') => {
                self.delete_automation_by_id(id);
                let new_count = self.cached_automations.len();
                if new_count > 0 && self.automation_panel_index >= new_count {
                    self.automation_panel_index = new_count - 1;
                }
                self.refresh_automation_view();
            }
            _ => {}
        }
    }

    /// Handle keys while editing the scoped automation in the central pane.
    /// `Enter` saves (and returns to the list), `Esc` discards; field navigation
    /// is shared with the overlay editor. `Ctrl+L`/`Ctrl+H` are handled earlier
    /// as global focus actions, so they move focus out of / back into the editor.
    fn handle_automation_editor_pane_key(&mut self, code: KeyCode, mods: KeyModifiers) {
        // No editor yet (e.g. focused an empty pane): allow create / leave only.
        let Some(editor) = self.automation_editor.as_mut() else {
            match code {
                KeyCode::Char('n') => self.new_automation_in_pane(),
                KeyCode::Esc => self.focus = InputFocus::Automations,
                _ => {}
            }
            return;
        };
        match editor.handle_key(code, mods) {
            super::modals::EditorOutcome::Continue => {}
            super::modals::EditorOutcome::Save => {
                let Some(editor) = self.automation_editor.clone() else {
                    return;
                };
                if self.save_automation(&editor) {
                    // A brand-new automation lands at the top of the list.
                    if editor.editing_id.is_none() {
                        self.automation_panel_index = 0;
                    }
                    self.focus = InputFocus::Automations;
                    self.refresh_automation_view();
                }
            }
            super::modals::EditorOutcome::Cancel => {
                // Discard edits and restore the preview for the selection.
                self.focus = InputFocus::Automations;
                self.refresh_automation_view();
            }
        }
    }

    /// Handle keys while the run-history panel is focused: `j`/`k` move the
    /// selected run, `r` triggers a fresh run of the scoped automation, and
    /// `Esc` returns to the editor.
    fn handle_automation_run_history_key(&mut self, code: KeyCode) {
        match code {
            KeyCode::Char('j') | KeyCode::Down => {
                let len = self.cached_automation_runs.len();
                if len > 0 && self.automation_run_index + 1 < len {
                    self.automation_run_index += 1;
                }
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.automation_run_index = self.automation_run_index.saturating_sub(1);
            }
            // Trigger a fresh run of the scoped automation now. (`run_automation_by_id`
            // refreshes the caches; the new run record lands on a later tick.)
            KeyCode::Char('r') => {
                if let Some(id) = self.scoped_automation_id() {
                    self.run_automation_by_id(id);
                }
            }
            // Jump to the session this run touched (the send target / spawned
            // session), when it's still open.
            KeyCode::Enter => {
                self.open_run_related_session();
            }
            KeyCode::Esc => self.focus = InputFocus::AutomationEditor,
            _ => {}
        }
    }

    /// Handle keys while editing the scoped task in the central pane. `Enter`
    /// saves and returns to the tasks panel; `Esc` discards and returns. Field
    /// navigation is the `TaskEditorModal`'s own. Mirrors
    /// `handle_automation_editor_pane_key`.
    fn handle_task_editor_pane_key(&mut self, code: KeyCode, mods: KeyModifiers) {
        let Some(editor) = self.task_editor.as_mut() else {
            match code {
                KeyCode::Char('n') => self.new_task_in_pane(),
                KeyCode::Esc => self.focus = InputFocus::TaskList,
                _ => {}
            }
            return;
        };
        match editor.handle_key(code, mods) {
            super::modals::EditorOutcome::Continue => {}
            super::modals::EditorOutcome::Save => {
                let Some(editor) = self.task_editor.clone() else {
                    return;
                };
                if self.save_task(&editor) {
                    // A brand-new task lands at the top of the list.
                    if editor.editing_id.is_none() {
                        self.task_panel_index = 0;
                    }
                    self.focus = InputFocus::TaskList;
                    self.refresh_task_view();
                }
            }
            super::modals::EditorOutcome::Cancel => {
                // Discard edits and restore the preview for the selection.
                self.focus = InputFocus::TaskList;
                self.refresh_task_view();
            }
        }
    }

    /// Handle keys while the tasks panel is focused: `j`/`k` select (and preview
    /// the selected task in the central pane), `n` create, `e`/`Enter` open the
    /// central-pane editor, `Space` cycles status, `r` triggers the agent action,
    /// `d` deletes, `Esc` leaves. Searching is handled by the global `Ctrl+A`.
    pub(crate) fn handle_task_list_key(&mut self, code: KeyCode) {
        // Creating works regardless of whether the panel has entries.
        if matches!(code, KeyCode::Char('n')) {
            self.new_task_in_pane();
            return;
        }

        let count = self.filtered_task_indices.len();
        match code {
            KeyCode::Char('j') | KeyCode::Down
                if count > 0 && self.task_panel_index + 1 < count =>
            {
                self.task_panel_index += 1;
                self.refresh_task_view();
            }
            KeyCode::Char('j') | KeyCode::Down => {}
            KeyCode::Char('k') | KeyCode::Up => {
                self.task_panel_index = self.task_panel_index.saturating_sub(1);
                self.refresh_task_view();
            }
            KeyCode::Esc => {
                self.focus = InputFocus::SessionList;
                self.task_editor = None;
            }
            // Open the central-pane editor for the selected task. On an empty
            // panel, start a new task instead.
            KeyCode::Char('e') | KeyCode::Enter => {
                if count == 0 {
                    self.new_task_in_pane();
                } else {
                    self.enter_task_editor();
                }
            }
            KeyCode::Char(' ') => {
                if let Some(task) = self.selected_task().cloned() {
                    if let Err(e) = self.db.set_task_status(task.id, task.status.cycle()) {
                        error!("Failed to cycle task {} status: {e}", task.id);
                    }
                    self.refresh_tasks();
                    self.sync_task_editor();
                }
            }
            KeyCode::Char('r') => {
                if let Some(task) = self.selected_task().cloned() {
                    self.trigger_task(&task);
                }
            }
            KeyCode::Char('d') => {
                if let Some(id) = self.selected_task().map(|t| t.id) {
                    if let Err(e) = self.db.soft_delete_task(id) {
                        error!("Failed to delete task {id}: {e}");
                    }
                    self.refresh_tasks();
                    let new_count = self.filtered_task_indices.len();
                    if new_count > 0 && self.task_panel_index >= new_count {
                        self.task_panel_index = new_count - 1;
                    }
                    self.refresh_task_view();
                }
            }
            _ => {}
        }
    }

    /// Handle keys while the global-search strip is focused. Typed characters
    /// edit the query (so plain `j`/`k` insert, like the other search inputs);
    /// `Up`/`Down` and `Ctrl+P`/`Ctrl+N` move the selection; `Enter` activates
    /// the selected result; `Esc` closes the strip.
    fn handle_global_search_key(&mut self, code: KeyCode, mods: KeyModifiers) {
        let ctrl = mods.contains(KeyModifiers::CONTROL);
        match code {
            KeyCode::Esc => self.close_global_search(),
            KeyCode::Enter => self.activate_global_search_result(),
            KeyCode::Down => self.move_global_search_selection(1),
            KeyCode::Up => self.move_global_search_selection(-1),
            KeyCode::Char('n') if ctrl => self.move_global_search_selection(1),
            KeyCode::Char('p') if ctrl => self.move_global_search_selection(-1),
            KeyCode::Backspace => {
                self.global_search.query.backspace();
                self.on_global_search_query_changed();
            }
            KeyCode::Delete => {
                self.global_search.query.delete();
                self.on_global_search_query_changed();
            }
            KeyCode::Left => self.global_search.query.move_left(),
            KeyCode::Right => self.global_search.query.move_right(),
            KeyCode::Home => self.global_search.query.home(),
            KeyCode::End => self.global_search.query.end(),
            // Plain chars edit the query; ignore other Ctrl-chords.
            KeyCode::Char(c) if !ctrl => {
                self.global_search.query.insert(c);
                self.on_global_search_query_changed();
            }
            _ => {}
        }
    }

    /// Move the global-search selection by `delta`, clamped to the result range,
    /// and live-preview the newly selected result.
    fn move_global_search_selection(&mut self, delta: i32) {
        let len = self.global_search.results.len();
        if len == 0 {
            self.global_search.selected = 0;
            return;
        }
        let next = (self.global_search.selected as i32 + delta).clamp(0, len as i32 - 1);
        self.global_search.selected = next as usize;
        self.preview_global_search_result();
    }

    fn handle_file_viewer_key(&mut self, code: KeyCode, mods: KeyModifiers) {
        if self.file_viewer.search_active {
            self.handle_file_viewer_search_key(code, mods);
        } else {
            self.handle_file_viewer_nav_key(code);
        }
    }

    fn handle_file_viewer_search_key(&mut self, code: KeyCode, mods: KeyModifiers) {
        let ctrl = mods.contains(KeyModifiers::CONTROL);
        match code {
            KeyCode::Esc => self.file_viewer.end_search(),
            // Enter/Down cycle to next match and stay in search mode.
            // Tab commits and exits search mode.
            KeyCode::Enter | KeyCode::Down => self.file_viewer.next_match(),
            KeyCode::Up => self.file_viewer.prev_match(),
            KeyCode::Tab => self.file_viewer.search_active = false,
            KeyCode::Char('n') if ctrl => self.file_viewer.next_match(),
            KeyCode::Char('p') if ctrl => self.file_viewer.prev_match(),
            KeyCode::Backspace => self.file_viewer.search_pop(),
            KeyCode::Char(c) if !ctrl => self.file_viewer.search_push(c),
            _ => {}
        }
    }

    fn handle_file_viewer_nav_key(&mut self, code: KeyCode) {
        // Navigation/search/expand keys are rebindable `FileViewer`-scoped
        // actions, resolved by the context lookup in `handle_key` before this
        // runs. Only the fixed "Esc clears an active query" shortcut remains.
        if matches!(code, KeyCode::Esc) && !self.file_viewer.search_query.is_empty() {
            self.file_viewer.end_search();
        }
    }

    fn open_file_in_editor(&mut self, root: std::path::PathBuf, file: std::path::PathBuf) {
        let Some(editor) = super::helpers::resolve_editor_command(&self.db) else {
            self.set_status(
                super::StatusLevel::Error,
                "No editor configured — set `editor_command` via MCP or $VISUAL / $EDITOR.",
            );
            return;
        };
        if let Err(e) = super::helpers::open_in_editor(&[root, file], &editor) {
            warn!("file viewer: failed to open editor: {e}");
        }
    }

    /// Session-list keys are all rebindable `SessionList`-scoped actions
    /// (`SessionListNext`/`Prev`/`Open`), resolved by the context lookup in
    /// `handle_key` before this runs — so nothing remains to handle here.
    pub(crate) fn handle_session_list_key(&mut self, _code: KeyCode) {}

    fn handle_terminal_key(&mut self, code: KeyCode, mods: KeyModifiers) {
        // Terminal scroll is handled by the rebindable `TerminalScroll*`
        // actions in `handle_key` before this runs; everything else snaps to
        // the bottom and is forwarded to the PTY.

        // Snap to bottom on any non-scroll key when scrolled up
        self.with_active_parser(|parser| {
            if parser.screen().scrollback() > 0 {
                parser.screen_mut().set_scrollback(0);
            }
        });

        if let Some(session) = self.sessions.get(self.active_index) {
            if let Some(bytes) = input::key_to_bytes(code, mods) {
                let result = if let (TerminalView::Shell, Some(shell)) =
                    (self.active_terminal_view(), &session.shell_pane)
                {
                    shell.send_input(bytes)
                } else {
                    session.send_input(bytes)
                };
                if let Err(e) = result {
                    error!("Failed to send input: {e}");
                }
            }
        }
    }

    fn handle_restore_sessions_key(&mut self, code: KeyCode) {
        let super::modals::Modal::RestoreSessions(ref mut rs) = self.modal else {
            return;
        };
        match code {
            KeyCode::Esc => {
                self.modal.close();
            }
            KeyCode::Char('j') | KeyCode::Down
                if !rs.list.is_empty() && rs.index + 1 < rs.list.len() =>
            {
                rs.index += 1;
            }
            KeyCode::Char('k') | KeyCode::Up => {
                rs.index = rs.index.saturating_sub(1);
            }
            KeyCode::Enter => {
                if rs.list.is_empty() {
                    return;
                }
                let deleted = rs.list.remove(rs.index);
                if rs.index >= rs.list.len() && rs.index > 0 {
                    rs.index -= 1;
                }
                self.modal.close();
                self.restore_deleted_session(deleted);
            }
            _ => {}
        }
    }

    fn handle_branch_selector_key(&mut self, code: KeyCode) {
        let super::modals::Modal::BranchSelector(ref mut bs) = self.modal else {
            return;
        };
        match code {
            KeyCode::Esc => {
                self.modal.close();
                self.pending_repo_path = None;
                self.pending_all_repos = None;
                self.pending_normal_repos.clear();
            }
            KeyCode::Char('j') | KeyCode::Down if bs.index + 1 < bs.branches.len() => {
                bs.index += 1;
            }
            KeyCode::Char('k') | KeyCode::Up => {
                bs.index = bs.index.saturating_sub(1);
            }
            KeyCode::Enter => {
                let base_branch = bs.branches[bs.index].clone();
                self.pending_base_branch = Some(base_branch);
                self.modal =
                    super::modals::Modal::SessionName(super::modals::SessionNameModal::default());
            }
            _ => {}
        }
    }

    fn handle_worktree_name_key(&mut self, code: KeyCode) {
        let super::modals::Modal::WorktreeName(ref mut wn) = self.modal else {
            return;
        };
        match code {
            KeyCode::Esc => {
                self.modal.close();
                self.pending_base_branch = None;
                self.pending_repo_path = None;
                self.pending_all_repos = None;
                self.pending_normal_repos.clear();
                self.pending_session_name = None;
            }
            KeyCode::Enter => {
                let new_branch = wn.name.value().trim().to_string();
                if new_branch.is_empty() {
                    self.set_error("Branch name cannot be empty");
                    return;
                }
                self.modal.close();
                if let Some(base_branch) = self.pending_base_branch.take() {
                    // Use all repos for multi-repo projects, single repo otherwise
                    let repo_paths = if let Some(all_repos) = self.pending_all_repos.take() {
                        self.pending_repo_path = None;
                        all_repos
                    } else if let Some(repo_path) = self.pending_repo_path.take() {
                        vec![repo_path]
                    } else {
                        return;
                    };
                    let session_name = self.pending_session_name.take();
                    self.spawn_worktree_session(
                        &repo_paths,
                        &new_branch,
                        &base_branch,
                        session_name,
                    );
                }
            }
            KeyCode::Backspace => wn.name.backspace(),
            KeyCode::Delete => wn.name.delete(),
            KeyCode::Left => wn.name.move_left(),
            KeyCode::Right => wn.name.move_right(),
            KeyCode::Home => wn.name.home(),
            KeyCode::End => wn.name.end(),
            KeyCode::Char(c) => wn.name.insert(c),
            _ => {}
        }
    }

    fn handle_session_name_key(&mut self, code: KeyCode) {
        let super::modals::Modal::SessionName(ref mut sn) = self.modal else {
            return;
        };
        match code {
            KeyCode::Esc => {
                self.modal.close();
                if self.pending_base_branch.is_some() {
                    // Worktree flow — clean up worktree-specific pending state.
                    self.pending_base_branch = None;
                    self.pending_repo_path = None;
                    self.pending_all_repos = None;
                    self.pending_normal_repos.clear();
                } else {
                    // Normal flow — clean up spawn state.
                    self.pending_spawn_config = None;
                    self.pending_spawn_worktrees.clear();
                    self.pending_fork = false;
                }
            }
            KeyCode::Enter => {
                let name = sn.name.value().trim().to_string();
                if name.is_empty() {
                    self.set_error("Session name cannot be empty");
                    return;
                }
                self.modal.close();
                if self.pending_base_branch.is_some() {
                    // Worktree flow — proceed to branch name input.
                    let branch = session_name_to_branch(&name);
                    self.pending_session_name = Some(name);
                    let mut modal = super::modals::WorktreeNameModal::default();
                    modal.name.set(&branch);
                    self.modal = super::modals::Modal::WorktreeName(modal);
                } else if let Some(config) = self.pending_spawn_config.take() {
                    let worktrees = std::mem::take(&mut self.pending_spawn_worktrees);
                    if self.pending_fork {
                        // Fork flow — role already set, spawn directly.
                        self.pending_fork = false;
                        self.do_spawn_session(name, &config, worktrees, false);
                    } else {
                        // Normal flow — proceed to role selection / spawn.
                        self.finish_prepare_spawn(name, config, worktrees);
                    }
                }
            }
            KeyCode::Backspace => sn.name.backspace(),
            KeyCode::Delete => sn.name.delete(),
            KeyCode::Left => sn.name.move_left(),
            KeyCode::Right => sn.name.move_right(),
            KeyCode::Home => sn.name.home(),
            KeyCode::End => sn.name.end(),
            KeyCode::Char(c) => sn.name.insert(c),
            _ => {}
        }
    }

    /// Drive the centered-overlay automation editor (the Ctrl+P list path).
    /// Field navigation is shared with the in-pane editor via
    /// [`AutomationEditorModal::handle_key`].
    fn handle_automation_editor_key(&mut self, code: KeyCode, mods: KeyModifiers) {
        let super::modals::Modal::AutomationEditor(ref mut m) = self.modal else {
            return;
        };
        match m.handle_key(code, mods) {
            super::modals::EditorOutcome::Continue => {}
            super::modals::EditorOutcome::Save => self.submit_automation_editor(),
            super::modals::EditorOutcome::Cancel => self.modal.close(),
        }
    }

    fn handle_automations_list_key(&mut self, code: KeyCode) {
        if let super::modals::Modal::AutomationsList(ref mut al) = self.modal {
            match code {
                KeyCode::Esc => {
                    self.modal.close();
                    return;
                }
                KeyCode::Char('j') | KeyCode::Down => {
                    if al.index + 1 < al.entries.len() {
                        al.index += 1;
                    }
                    return;
                }
                KeyCode::Char('k') | KeyCode::Up => {
                    al.index = al.index.saturating_sub(1);
                    return;
                }
                _ => {}
            }
        }

        match code {
            KeyCode::Char('n') => {
                self.modal.close();
                self.open_automation_editor();
            }
            KeyCode::Char('e') | KeyCode::Enter => {
                if let Some(id) = self.selected_automation_id() {
                    self.modal.close();
                    self.open_edit_automation(id);
                }
            }
            KeyCode::Char(' ') => {
                if let Some(id) = self.selected_automation_id() {
                    self.toggle_automation_by_id(id);
                    self.refresh_automations_list_modal();
                }
            }
            KeyCode::Char('r') => {
                if let Some(id) = self.selected_automation_id() {
                    self.run_automation_by_id(id);
                }
            }
            KeyCode::Char('d') => {
                if let Some(id) = self.selected_automation_id() {
                    self.delete_automation_by_id(id);
                    self.refresh_automations_list_modal();
                }
            }
            _ => {}
        }
    }

    /// The id of the selected automation in the list modal, if any.
    fn selected_automation_id(&self) -> Option<i64> {
        let super::modals::Modal::AutomationsList(ref al) = self.modal else {
            return None;
        };
        al.entries.get(al.index).map(|e| e.id)
    }

    /// Rebuild the list modal entries after a mutation, preserving selection.
    fn refresh_automations_list_modal(&mut self) {
        let index = match self.modal {
            super::modals::Modal::AutomationsList(ref al) => al.index,
            _ => return,
        };
        self.open_automations_list();
        if let super::modals::Modal::AutomationsList(ref mut al) = self.modal {
            al.index = index.min(al.entries.len().saturating_sub(1));
            if al.entries.is_empty() {
                self.modal.close();
            }
        }
    }

    fn handle_agent_picker_key(&mut self, code: KeyCode) {
        let super::modals::Modal::AgentPicker(ref mut ap) = self.modal else {
            return;
        };
        let choice_count = ap.choices.len();
        match code {
            KeyCode::Esc => {
                self.modal.close();
                self.pending_spawn_config = None;
                self.pending_spawn_worktrees.clear();
                self.pending_spawn_name = None;
            }
            KeyCode::Char('j') | KeyCode::Down if ap.selected_index + 1 < choice_count => {
                ap.selected_index += 1;
            }
            KeyCode::Char('k') | KeyCode::Up => {
                ap.selected_index = ap.selected_index.saturating_sub(1);
            }
            KeyCode::Enter => {
                let chosen = ap.choices.get(ap.selected_index).map(|c| c.name.clone());
                self.modal.close();
                if let (Some(mut config), Some(name), Some(agent)) = (
                    self.pending_spawn_config.take(),
                    self.pending_spawn_name.take(),
                    chosen,
                ) {
                    config.agent = agent;
                    let worktrees = std::mem::take(&mut self.pending_spawn_worktrees);
                    self.do_spawn_session(name, &config, worktrees, false);
                }
            }
            _ => {}
        }
    }

    fn dispatch_action(&mut self, action: crate::session::Action) -> bool {
        use crate::session::Action;
        match action {
            Action::QuitApp => {
                self.should_quit = true;
                true
            }
            Action::NewSession => {
                // In the automations context, Ctrl+N creates a new automation
                // (mirrors `n`); elsewhere it starts the new-session wizard.
                if matches!(
                    self.focus,
                    InputFocus::Automations | InputFocus::AutomationEditor
                ) {
                    self.new_automation_in_pane();
                } else {
                    self.open_repo_picker();
                }
                true
            }
            Action::DeleteSession => match self.focus {
                InputFocus::SessionList | InputFocus::FileViewer => {
                    self.close_active_session();
                    true
                }
                // On the automations pane, Ctrl+D deletes the selected
                // automation (mirrors the pane's `d` key).
                InputFocus::Automations => {
                    self.handle_automations_pane_key(KeyCode::Char('d'));
                    true
                }
                // On the tasks panel, Ctrl+D deletes the selected task (mirrors
                // the panel's `d` key).
                InputFocus::TaskList => {
                    self.handle_task_list_key(KeyCode::Char('d'));
                    true
                }
                // The editors / run-history / global search capture their own
                // keys earlier, so this is unreachable for them; yield just in
                // case.
                InputFocus::AutomationEditor
                | InputFocus::AutomationRunHistory
                | InputFocus::TaskEditor
                | InputFocus::GlobalSearch => false,
                InputFocus::Terminal => false, // forward to PTY
            },
            Action::OpenInEditor => match self.focus {
                InputFocus::Terminal => false, // forward to PTY
                _ => {
                    self.open_active_in_editor();
                    true
                }
            },
            Action::OpenAutomations => {
                self.open_automations_list();
                true
            }
            Action::StartSync => {
                self.start_sync();
                true
            }
            Action::ToggleShell => {
                self.toggle_shell_view();
                true
            }
            Action::ForkSession => match self.focus {
                InputFocus::Terminal => false, // PTY: shell forward-search
                _ => {
                    self.fork_active_session();
                    true
                }
            },
            Action::RestartSession => match self.focus {
                InputFocus::Terminal => false, // PTY: bash reverse-search
                _ => {
                    self.restart_active_session();
                    true
                }
            },
            Action::UndoDelete => {
                if self.pending_delete.is_some() {
                    self.undo_delete();
                }
                true
            }
            Action::OpenRestoreSessions => {
                self.open_restore_sessions_modal();
                true
            }
            Action::OpenThemePicker => {
                self.open_theme_picker();
                true
            }
            Action::FocusBackward => {
                self.focus = self.cycle_focus_backward();
                self.on_focus_changed();
                true
            }
            Action::FocusForward => {
                self.focus = self.cycle_focus_forward();
                self.on_focus_changed();
                true
            }
            Action::NextSession => {
                self.switch_session_forward();
                true
            }
            Action::PreviousSession => {
                self.switch_session_backward();
                true
            }
            Action::ToggleHelp => {
                self.modal = super::modals::Modal::Help(super::modals::HelpModal::default());
                true
            }
            Action::ToggleInfoPanel => {
                self.show_info_panel = !self.show_info_panel;
                self.resize_sessions_to_content_area();
                true
            }
            Action::FocusTasks => {
                // Toggle the tasks panel column, mirroring the file viewer
                // (`ToggleFileViewer`): showing it also focuses it; hiding it
                // drops focus back to the session list.
                self.show_tasks_panel = !self.show_tasks_panel;
                if self.show_tasks_panel {
                    self.refresh_tasks();
                    self.task_panel_index = 0;
                    self.focus = InputFocus::TaskList;
                } else if self.focus == InputFocus::TaskList {
                    self.focus = InputFocus::SessionList;
                }
                self.resize_sessions_to_content_area();
                true
            }
            Action::ToggleFileViewer => {
                self.show_file_viewer = !self.show_file_viewer;
                if self.show_file_viewer {
                    self.rebuild_file_viewer_for_active();
                } else if self.focus == InputFocus::FileViewer {
                    self.focus = InputFocus::SessionList;
                }
                self.resize_sessions_to_content_area();
                true
            }
            Action::GlobalSearch => {
                self.open_global_search();
                true
            }

            // ── Clipboard (global) ──────────────────────────────────────
            // Copy only consumes the key when there's a selection; otherwise
            // it yields (false) so the terminal can send SIGINT. Paste is
            // normally intercepted earlier (so it reaches modal text inputs);
            // this arm covers the plain-terminal path.
            Action::Copy => {
                if self.text_selection.is_some() {
                    self.copy_selection_to_clipboard();
                    true
                } else {
                    false
                }
            }
            Action::Paste => {
                self.paste_from_clipboard();
                true
            }

            // ── Session list (scoped) ───────────────────────────────────
            Action::SessionListNext => {
                // Past the last session, flow into the automations pane so the
                // left column reads as one continuous list.
                if self.active_is_last_in_order() {
                    self.focus = InputFocus::Automations;
                    self.automation_panel_index = 0;
                    self.refresh_automation_view();
                } else {
                    self.switch_session_forward();
                }
                true
            }
            Action::SessionListPrev => {
                if self.active_is_first_in_order() {
                    self.focus = InputFocus::Automations;
                    self.automation_panel_index = self.cached_automations.len().saturating_sub(1);
                    self.refresh_automation_view();
                } else {
                    self.switch_session_backward();
                }
                true
            }
            Action::SessionListOpen => {
                self.focus = InputFocus::Terminal;
                true
            }

            // ── File viewer (scoped) ────────────────────────────────────
            Action::FileViewerDown => {
                self.file_viewer.move_selection(1);
                true
            }
            Action::FileViewerUp => {
                self.file_viewer.move_selection(-1);
                true
            }
            Action::FileViewerCollapse => {
                self.file_viewer.collapse();
                true
            }
            Action::FileViewerExpand => {
                self.file_viewer_expand();
                true
            }
            Action::FileViewerSearch => {
                self.file_viewer.start_search();
                true
            }
            Action::FileViewerNextMatch => {
                self.file_viewer.next_match();
                true
            }
            Action::FileViewerPrevMatch => {
                self.file_viewer.prev_match();
                true
            }

            // ── Terminal scroll (scoped) ────────────────────────────────
            Action::TerminalScrollUp => {
                self.scroll_terminal_up(1);
                true
            }
            Action::TerminalScrollDown => {
                self.scroll_terminal_down(1);
                true
            }
            Action::TerminalPageUp => {
                let amount = self.page_scroll_amount();
                self.scroll_terminal_up(amount);
                true
            }
            Action::TerminalPageDown => {
                let amount = self.page_scroll_amount();
                self.scroll_terminal_down(amount);
                true
            }
        }
    }

    /// Expand the selected file-viewer node, opening it in the editor when it's
    /// a file (dirs just toggle). Shared by the `FileViewerExpand` action.
    fn file_viewer_expand(&mut self) {
        use crate::ui::file_viewer::Activation;
        // Capture root+file before activate() (activate only opens files, not dirs).
        let file_with_root = self.file_viewer.selected_file_with_root();
        if matches!(self.file_viewer.activate(), Activation::Open(_)) {
            if let Some((file, root)) = file_with_root {
                self.open_file_in_editor(root, file);
            }
        }
    }

    fn handle_theme_picker_key(&mut self, code: KeyCode) {
        let presets = crate::session::ThemePreset::all();
        let preset_count = presets.len();
        let super::modals::Modal::ThemePicker(ref mut tp) = self.modal else {
            return;
        };
        match code {
            KeyCode::Esc => {
                self.modal.close();
            }
            KeyCode::Char('j') | KeyCode::Down if tp.index + 1 < preset_count => {
                tp.index += 1;
                let preset = presets[tp.index];
                crate::ui::theme::set_active(preset.palette());
            }
            KeyCode::Char('k') | KeyCode::Up if tp.index > 0 => {
                tp.index -= 1;
                let preset = presets[tp.index];
                crate::ui::theme::set_active(preset.palette());
            }
            KeyCode::Enter => {
                let idx = tp.index;
                self.modal.close();
                if let Some(preset) = presets.get(idx) {
                    crate::ui::theme::set_active(preset.palette());
                    self.active_theme = *preset;
                    if let Err(e) = self.db.set_active_theme(preset.as_str()) {
                        tracing::error!("Failed to persist active theme: {e}");
                    }
                }
            }
            _ => {}
        }
    }

    pub(crate) fn start_branch_selection(&mut self) {
        let Some(repo_path) = self.pending_repo_path.clone() else {
            return;
        };

        Self::fetch_pending_repos(&repo_path, self.pending_all_repos.as_ref());

        match crate::git::list_branches(&repo_path) {
            Ok(branches) if branches.is_empty() => {
                self.set_error("No branches found in repository");
                self.pending_repo_path = None;
            }
            Ok(branches) => {
                let branches = Self::ordered_branch_list(&repo_path, branches);
                self.modal =
                    super::modals::Modal::BranchSelector(super::modals::BranchSelectorModal {
                        index: 0,
                        branches,
                    });
            }
            Err(e) => {
                error!("Failed to list branches: {e}");
                self.set_error(format!("Failed to list branches: {e:#}"));
                self.pending_repo_path = None;
            }
        }
    }

    /// Fetch origin for the primary repo and any extra worktree repos so
    /// branch lists are up-to-date. Failures are non-fatal (logged only).
    fn fetch_pending_repos(
        repo_path: &std::path::Path,
        all_repos: Option<&Vec<std::path::PathBuf>>,
    ) {
        if let Err(e) = crate::git::git_fetch(repo_path) {
            warn!("git fetch origin failed (continuing): {e:#}");
        }
        let Some(all_repos) = all_repos else {
            return;
        };
        for extra_repo in all_repos.iter().skip(1) {
            if let Err(e) = crate::git::git_fetch(extra_repo) {
                warn!(
                    "git fetch origin failed for {} (continuing): {e:#}",
                    extra_repo.display()
                );
            }
        }
    }

    /// Order a branch list for the selector: the local default branch first,
    /// then `origin/<default>` (remote-based branching) pinned at the very top.
    fn ordered_branch_list(repo_path: &std::path::Path, mut branches: Vec<String>) -> Vec<String> {
        // Move the default branch to front so it's pre-selected.
        if let Some(default) = crate::git::default_branch(repo_path, &branches) {
            if let Some(pos) = branches.iter().position(|b| b == &default) {
                let branch = branches.remove(pos);
                branches.insert(0, branch);
            }
        }

        // Insert origin/<default> at position 0 for remote-based branching.
        let remote_ref = crate::git::default_branch_from_remote(repo_path)
            .map(|name| format!("origin/{name}"))
            .or_else(|| {
                for candidate in ["origin/main", "origin/master"] {
                    if crate::git::branch_exists(repo_path, candidate) {
                        return Some(candidate.to_string());
                    }
                }
                None
            });
        if let Some(ref remote) = remote_ref {
            if !branches.contains(remote) {
                branches.insert(0, remote.clone());
            }
        }

        branches
    }

    // ── Repo Picker Modal ────────────────────────────────────────────────

    fn handle_repo_picker_key(&mut self, code: KeyCode) {
        let super::modals::Modal::RepoPicker(ref rp) = self.modal else {
            return;
        };
        match rp.focus {
            super::modals::RepoPickerFocus::List => self.handle_repo_picker_list_key(code),
            super::modals::RepoPickerFocus::Input => self.handle_repo_picker_input_key(code),
            super::modals::RepoPickerFocus::Search => self.handle_repo_picker_search_key(code),
        }
    }

    fn handle_repo_picker_list_key(&mut self, code: KeyCode) {
        let super::modals::Modal::RepoPicker(ref mut rp) = self.modal else {
            return;
        };
        match code {
            KeyCode::Esc => {
                self.modal.close();
            }
            KeyCode::Tab => {
                rp.focus = super::modals::RepoPickerFocus::Input;
            }
            KeyCode::Char('/') => {
                rp.clear_search();
                rp.focus = super::modals::RepoPickerFocus::Search;
            }
            KeyCode::Char('j') | KeyCode::Down if rp.list_index + 1 < rp.filtered_indices.len() => {
                rp.list_index += 1;
            }
            KeyCode::Char('k') | KeyCode::Up => {
                rp.list_index = rp.list_index.saturating_sub(1);
            }
            KeyCode::Char(' ') => self.repo_picker_toggle_selected(),
            KeyCode::Char('w') => self.repo_picker_toggle_worktree(),
            KeyCode::Char('d') => self.repo_picker_delete_bookmark(),
            KeyCode::Enter => {
                self.submit_repo_picker();
            }
            _ => {}
        }
    }

    /// Toggle the selected flag of the repo under the list cursor.
    fn repo_picker_toggle_selected(&mut self) {
        let super::modals::Modal::RepoPicker(ref mut rp) = self.modal else {
            return;
        };
        let Some(&real_idx) = rp.filtered_indices.get(rp.list_index) else {
            return;
        };
        if let Some(sel) = rp.selected.get_mut(real_idx) {
            *sel = !*sel;
        }
    }

    /// Toggle the worktree flag of the repo under the cursor, auto-selecting it
    /// when worktree mode is turned on.
    fn repo_picker_toggle_worktree(&mut self) {
        let super::modals::Modal::RepoPicker(ref mut rp) = self.modal else {
            return;
        };
        let Some(&real_idx) = rp.filtered_indices.get(rp.list_index) else {
            return;
        };
        let Some(wt) = rp.worktree.get_mut(real_idx) else {
            return;
        };
        *wt = !*wt;
        // Auto-select when toggling worktree on
        if *wt {
            if let Some(sel) = rp.selected.get_mut(real_idx) {
                *sel = true;
            }
        }
    }

    /// Delete the bookmark under the cursor from the DB and the modal lists.
    fn repo_picker_delete_bookmark(&mut self) {
        let super::modals::Modal::RepoPicker(ref mut rp) = self.modal else {
            return;
        };
        let Some(&real_idx) = rp.filtered_indices.get(rp.list_index) else {
            return;
        };
        let path = rp.bookmarks[real_idx].clone();
        if let Err(e) = self.db.delete_repo_bookmark(&path) {
            error!("Failed to delete repo bookmark: {e}");
        }
        let super::modals::Modal::RepoPicker(ref mut rp) = self.modal else {
            return;
        };
        rp.bookmarks.remove(real_idx);
        rp.selected.remove(real_idx);
        rp.worktree.remove(real_idx);
        self.recompute_repo_filter();
    }

    fn handle_repo_picker_input_key(&mut self, code: KeyCode) {
        let super::modals::Modal::RepoPicker(ref mut rp) = self.modal else {
            return;
        };
        match code {
            KeyCode::Esc => {
                self.modal.close();
                return;
            }
            KeyCode::Tab => {
                if let Some(suggestion) = rp.path_suggestion.take() {
                    for c in suggestion.chars() {
                        rp.path_input.insert(c);
                    }
                } else {
                    rp.focus = super::modals::RepoPickerFocus::List;
                    rp.path_suggestion = None;
                    return;
                }
            }
            KeyCode::BackTab => {
                rp.focus = super::modals::RepoPickerFocus::List;
                rp.path_suggestion = None;
                return;
            }
            KeyCode::Enter => {
                self.repo_picker_commit_path_input();
                return;
            }
            KeyCode::Backspace => rp.path_input.backspace(),
            KeyCode::Delete => rp.path_input.delete(),
            KeyCode::Left => rp.path_input.move_left(),
            KeyCode::Right => rp.path_input.move_right(),
            KeyCode::Home => rp.path_input.home(),
            KeyCode::End => rp.path_input.end(),
            KeyCode::Char(c) => rp.path_input.insert(c),
            _ => return,
        }
        self.update_repo_picker_path_suggestion();
    }

    /// Commit the typed path in the repo-picker input: add or re-select the
    /// bookmark, persist it, clear the input, and refresh the filter.
    fn repo_picker_commit_path_input(&mut self) {
        let super::modals::Modal::RepoPicker(ref mut rp) = self.modal else {
            return;
        };
        let path = rp.path_input.value().trim().to_string();
        if path.is_empty() {
            self.recompute_repo_filter();
            return;
        }
        let expanded = paths::expand_tilde(&path);
        // Add to bookmarks list if not already present
        if !rp.bookmarks.contains(&expanded) {
            rp.bookmarks.push(expanded.clone());
            rp.selected.push(true); // auto-select newly added
            rp.worktree.push(false);
        } else if let Some(idx) = rp.bookmarks.iter().position(|p| p == &expanded) {
            // Already in list — just select it
            rp.selected[idx] = true;
        }
        // Persist as bookmark
        if let Err(e) = self.db.upsert_repo_bookmark(&expanded) {
            error!("Failed to save repo bookmark: {e}");
        }
        let super::modals::Modal::RepoPicker(ref mut rp) = self.modal else {
            return;
        };
        rp.path_input.clear();
        rp.path_suggestion = None;
        self.recompute_repo_filter();
    }

    fn update_repo_picker_path_suggestion(&mut self) {
        let super::modals::Modal::RepoPicker(ref mut rp) = self.modal else {
            return;
        };
        let value = rp.path_input.value().to_string();
        let at_end = rp.path_input.cursor_pos() == value.chars().count();
        if at_end && !value.is_empty() {
            rp.path_suggestion = paths::complete_directory_path(&value);
        } else {
            rp.path_suggestion = None;
        }
    }

    fn handle_repo_picker_search_key(&mut self, code: KeyCode) {
        let super::modals::Modal::RepoPicker(ref mut rp) = self.modal else {
            return;
        };
        match code {
            KeyCode::Esc => {
                rp.clear_search();
                rp.focus = super::modals::RepoPickerFocus::List;
            }
            KeyCode::Enter => {
                rp.focus = super::modals::RepoPickerFocus::List;
            }
            KeyCode::Backspace => {
                rp.search_input.backspace();
                self.recompute_repo_filter();
            }
            KeyCode::Delete => {
                rp.search_input.delete();
                self.recompute_repo_filter();
            }
            KeyCode::Left => rp.search_input.move_left(),
            KeyCode::Right => rp.search_input.move_right(),
            KeyCode::Home => rp.search_input.home(),
            KeyCode::End => rp.search_input.end(),
            KeyCode::Char(c) => {
                rp.search_input.insert(c);
                self.recompute_repo_filter();
            }
            _ => {}
        }
    }

    fn recompute_repo_filter(&mut self) {
        let super::modals::Modal::RepoPicker(ref mut rp) = self.modal else {
            return;
        };
        let query = rp.search_input.value().to_string();
        if query.is_empty() {
            rp.filtered_indices = (0..rp.bookmarks.len()).collect();
        } else {
            rp.filtered_indices = rp
                .bookmarks
                .iter()
                .enumerate()
                .filter(|(_, path)| {
                    crate::fuzzy::fuzzy_match(&query, &path.display().to_string()).is_some()
                })
                .map(|(i, _)| i)
                .collect();
        }
        if rp.filtered_indices.is_empty() {
            rp.list_index = 0;
        } else {
            rp.list_index = rp.list_index.min(rp.filtered_indices.len() - 1);
        }
    }

    fn submit_repo_picker(&mut self) {
        let super::modals::Modal::RepoPicker(ref rp) = self.modal else {
            return;
        };

        let (worktree_repos, normal_repos) = Self::partition_selected_repos(rp);

        // Touch all selected bookmarks so they stay sorted by recency.
        for repo in worktree_repos.iter().chain(normal_repos.iter()) {
            if let Err(e) = self.db.upsert_repo_bookmark(repo) {
                error!("Failed to touch repo bookmark: {e}");
            }
        }

        self.modal.close();

        if worktree_repos.is_empty() && normal_repos.is_empty() {
            // No repos selected — spawn with HOME as cwd
            let mut config = SessionConfig::default();
            if let Some(home) = std::env::var_os("HOME") {
                config.cwd = Some(std::path::PathBuf::from(home));
            }
            self.spawn_session_with_config(&config);
        } else if !worktree_repos.is_empty() {
            // Has worktree repos — go to branch selection.
            // Store normal repos for inclusion after worktree creation.
            self.pending_repo_path = Some(worktree_repos[0].clone());
            self.pending_all_repos = if worktree_repos.len() > 1 {
                Some(worktree_repos)
            } else {
                None
            };
            self.pending_normal_repos = normal_repos;
            self.start_branch_selection();
        } else {
            // All normal repos — spawn directly (local-tmux), going straight
            // to the agent picker chain.
            self.pending_additional_dirs = normal_repos[1..].to_vec();
            let config = SessionConfig {
                cwd: Some(normal_repos[0].clone()),
                ..SessionConfig::default()
            };
            self.spawn_session_with_config(&config);
        }
    }

    /// Split the selected bookmarks into (worktree repos, normal repos).
    fn partition_selected_repos(
        rp: &super::modals::RepoPickerModal,
    ) -> (Vec<std::path::PathBuf>, Vec<std::path::PathBuf>) {
        let mut worktree_repos: Vec<std::path::PathBuf> = Vec::new();
        let mut normal_repos: Vec<std::path::PathBuf> = Vec::new();
        for (i, path) in rp.bookmarks.iter().enumerate() {
            if !rp.selected.get(i).copied().unwrap_or(false) {
                continue;
            }
            if rp.worktree.get(i).copied().unwrap_or(false) {
                worktree_repos.push(path.clone());
            } else {
                normal_repos.push(path.clone());
            }
        }
        (worktree_repos, normal_repos)
    }
}

#[cfg(test)]
mod tests {
    use super::session_name_to_branch;

    #[test]
    fn basic_conversion() {
        assert_eq!(session_name_to_branch("My Feature"), "my-feature");
    }

    #[test]
    fn multiple_spaces() {
        assert_eq!(session_name_to_branch("hello   world"), "hello-world");
    }

    #[test]
    fn uppercase() {
        assert_eq!(session_name_to_branch("FOO"), "foo");
    }

    #[test]
    fn consecutive_hyphens() {
        assert_eq!(session_name_to_branch("a--b"), "a-b");
    }

    #[test]
    fn trims_hyphens() {
        assert_eq!(session_name_to_branch(" -trim- "), "trim");
    }

    #[test]
    fn underscores_become_hyphens() {
        assert_eq!(session_name_to_branch("foo_bar"), "foo-bar");
    }

    #[test]
    fn strips_special_chars() {
        assert_eq!(session_name_to_branch("foo@bar!baz"), "foobarbaz");
    }

    #[test]
    fn empty_string() {
        assert_eq!(session_name_to_branch(""), "");
    }

    #[test]
    fn mixed_separators() {
        assert_eq!(session_name_to_branch("a - b _ c"), "a-b-c");
    }

    #[test]
    fn only_special_chars() {
        assert_eq!(session_name_to_branch("@#$%"), "");
    }

    #[test]
    fn unicode_alphanumeric() {
        assert_eq!(session_name_to_branch("café"), "café");
    }
}
