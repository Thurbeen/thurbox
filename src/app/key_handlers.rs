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

/// Whether a pressed chord is a bare `Ctrl+<letter>` — the namespace thurbox
/// shares with readline / shell line-editing chords. Used to gate
/// [`crate::session::Action::terminal_passthrough`] so the PTY-deferral only
/// fires for the conflicting chords; a non-`Ctrl+letter` rebind of a
/// passthrough action keeps working in the terminal.
fn is_ctrl_letter_chord(code: KeyCode, mods: KeyModifiers) -> bool {
    mods == KeyModifiers::CONTROL && matches!(code, KeyCode::Char(c) if c.is_ascii_alphabetic())
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

        // Cmd/Super chords are commands, never text: only the keybinding
        // lookup may consume them (see `handle_super_chord`).
        if mods.contains(KeyModifiers::SUPER) {
            self.handle_super_chord(code, mods);
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

        // The native code-review view captures keys (nav / comment / compose)
        // before the global lookup; focus/quit chords fall through so the user
        // can always leave.
        if self.handle_code_review_key(code, mods) {
            return;
        }

        // Keybinding lookup, scoped to the focused pane: global actions plus
        // any scoped to the current context (file viewer, session list,
        // terminal). Some readline/shell chords (Ctrl+A/E/W/U/R/D/…) defer to
        // the PTY when the terminal is focused so the inner agent CLI's
        // line editing keeps working — see `Action::terminal_passthrough`.
        // The deferral is gated on the bound chord still being a bare
        // `Ctrl+<letter>`, so a rebind to a non-conflicting key keeps the
        // thurbox command working even in the terminal.
        let context = self.focus_key_context();
        if let Some(action) = self.keybindings.lookup_in(context, code, mods) {
            let defer_to_pty = self.focus == InputFocus::Terminal
                && action.terminal_passthrough()
                && is_ctrl_letter_chord(code, mods);
            if !defer_to_pty && self.dispatch_action(action) {
                return;
            }
        }

        self.handle_focused_pane_key(code, mods);
    }

    /// Cmd/Super chords are commands, never text: only the keybinding lookup
    /// may consume them. The focus-based handlers (modal inputs, the search
    /// query, in-pane editors, list hotkeys) predate the kitty keyboard
    /// protocol and match `Char` without checking SUPER, so a chord like Cmd+J
    /// would otherwise type a bare `j`. While a modal or the search strip owns
    /// input the chord is swallowed outright, mirroring how Ctrl chords are
    /// unavailable there. (The terminal pass-through swallows SUPER on its own —
    /// `agent::input::key_to_bytes`.)
    fn handle_super_chord(&mut self, code: KeyCode, mods: KeyModifiers) {
        if self.modal.is_open() || self.global_search.active {
            return;
        }
        self.text_selection = None;
        let context = self.focus_key_context();
        if let Some(action) = self.keybindings.lookup_in(context, code, mods) {
            self.dispatch_action(action);
        }
    }

    /// Route a key (already cleared of priority/modal/global handling) to the
    /// handler for the currently focused pane.
    fn handle_focused_pane_key(&mut self, code: KeyCode, mods: KeyModifiers) {
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
            // The code-review view captures input earlier (before the global
            // keybinding lookup), so this arm is effectively unreachable.
            InputFocus::CodeReview => {}
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
    pub(super) fn handle_help_key(&mut self, code: KeyCode, mods: KeyModifiers) -> bool {
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
            // Normalize so the toast below shows the stored chord (masked
            // modifiers, canonical Shift+letter) — `rebind` normalizes again
            // for storage.
            let chord = KeyChord::normalized(mods, code);
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
        self.mark_keybindings_saved();
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
        self.mark_keybindings_saved();
    }

    /// Route the key to the open modal's handler, if any. Returns `true` if a
    /// modal was open and consumed the key.
    pub(super) fn handle_modal_key_if_open(&mut self, code: KeyCode, mods: KeyModifiers) -> bool {
        use super::modals::Modal;
        match self.modal {
            Modal::RestoreSessions(_) => self.handle_restore_sessions_key(code),
            Modal::BranchSelector(_) => self.handle_branch_selector_key(code),
            Modal::WorktreeName(_) => self.handle_worktree_name_key(code, mods),
            Modal::SessionName(_) => self.handle_session_name_key(code, mods),
            Modal::AutomationEditor(_) => self.handle_automation_editor_key(code, mods),
            Modal::AutomationsList(_) => self.handle_automations_list_key(code),
            Modal::AgentPicker(_) => self.handle_agent_picker_key(code),
            Modal::HostPicker(_) => self.handle_host_picker_key(code),
            Modal::ThemePicker(_) => self.handle_theme_picker_key(code),
            Modal::RepoPicker(_) => self.handle_repo_picker_key(code, mods),
            Modal::TaskActionPicker(_) => self.handle_task_action_picker_key(code),
            Modal::ConfirmDelete(_) => self.handle_confirm_delete_key(code),
            Modal::ConfirmRestore(_) => self.handle_confirm_restore_key(code),
            Modal::Settings(_) => self.handle_settings_key(code, mods),
            _ => return false,
        }
        true
    }

    /// Drive the Settings panel: edits a working copy, persists on `Ctrl+S`,
    /// discards on `Esc` (no live preview to revert).
    fn handle_settings_key(&mut self, code: KeyCode, mods: KeyModifiers) {
        let super::modals::Modal::Settings(ref mut m) = self.modal else {
            return;
        };
        match m.handle_key(code, mods) {
            super::modals::EditorOutcome::Continue => {}
            super::modals::EditorOutcome::Save => self.submit_settings_panel(),
            super::modals::EditorOutcome::Cancel => self.modal.close(),
        }
    }

    /// Drive the hard-delete confirmation prompt: `Enter`/`y` tears the session
    /// down, `Esc`/`n` cancels.
    fn handle_confirm_delete_key(&mut self, code: KeyCode) {
        let super::modals::Modal::ConfirmDelete(ref cd) = self.modal else {
            return;
        };
        match code {
            KeyCode::Enter | KeyCode::Char('y') | KeyCode::Char('Y') => {
                let session_id = cd.session_id;
                self.modal.close();
                self.confirm_hard_delete_session(session_id);
            }
            KeyCode::Esc | KeyCode::Char('n') | KeyCode::Char('N') => {
                self.modal.close();
            }
            _ => {}
        }
    }

    /// Drive the best-effort restore confirmation for a force-deleted session:
    /// `Enter`/`y` restores (committed branch state only), `Esc`/`n` cancels.
    fn handle_confirm_restore_key(&mut self, code: KeyCode) {
        let super::modals::Modal::ConfirmRestore(ref cr) = self.modal else {
            return;
        };
        match code {
            KeyCode::Enter | KeyCode::Char('y') | KeyCode::Char('Y') => {
                let deleted = cr.deleted.clone();
                self.modal.close();
                self.restore_deleted_session(deleted);
            }
            KeyCode::Esc | KeyCode::Char('n') | KeyCode::Char('N') => {
                self.modal.close();
            }
            _ => {}
        }
    }

    /// Drive the trigger-time task action picker: `j`/`k` (or arrows) select,
    /// `Enter` runs the chosen action, `Esc` closes.
    fn handle_task_action_picker_key(&mut self, code: KeyCode) {
        let super::modals::Modal::TaskActionPicker(ref mut p) = self.modal else {
            return;
        };
        match code {
            KeyCode::Esc => self.modal.close(),
            KeyCode::Char('j') | KeyCode::Down if p.selected + 1 < p.choices.len() => {
                p.selected += 1;
            }
            KeyCode::Char('k') | KeyCode::Up => {
                p.selected = p.selected.saturating_sub(1);
            }
            KeyCode::Enter => self.confirm_task_action_picker(),
            _ => {}
        }
    }

    /// `Enter` on the task action picker: run the selected Send/Spawn choice for
    /// the task being acted on, then close the picker.
    fn confirm_task_action_picker(&mut self) {
        use super::modals::TaskActionChoice;
        let super::modals::Modal::TaskActionPicker(ref p) = self.modal else {
            return;
        };
        let Some(choice) = p.choices.get(p.selected).cloned() else {
            self.modal.close();
            return;
        };
        let task_id = p.task_id;
        let title = p.title.clone();
        let status = self
            .task_ui
            .cached_tasks
            .iter()
            .find(|t| t.id == task_id)
            .map(|t| t.status)
            .unwrap_or_default();
        self.modal.close();
        match choice {
            TaskActionChoice::Send(session_id, _) => {
                self.send_task_to_session(task_id, &title, status, session_id);
            }
            TaskActionChoice::SpawnNew => {
                // Reuse the normal new-session flow; the prompt is
                // delivered + the task advanced when the spawn lands.
                self.task_ui.pending_task_prompt = Some((task_id, title));
                self.open_repo_picker();
            }
        }
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
            SessionList | Terminal | FileViewer | TaskList | CodeReview => {
                // Order mirrors the on-screen columns: central → tasks → files.
                // The central pane is the code review when the active session has
                // one open (persisted per session, like the shell view), else the
                // terminal — so `Ctrl+L`/`Ctrl+H` move in and out of the review
                // just like the terminal, and `Ctrl+H` to the session list keeps
                // the review open.
                let central = if self.active_review().is_some() {
                    CodeReview
                } else {
                    Terminal
                };
                let mut ring = vec![SessionList, central];
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
            // (`Ctrl+/` by default) / `Esc`, so `Ctrl+L`/`Ctrl+H` are no-ops
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
    pub(super) fn on_focus_changed(&mut self) {
        if self.focus == InputFocus::AutomationRunHistory {
            self.automation_ui.automation_run_index = 0;
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

    /// Handle keys while editing the scoped task in the central pane. `Enter`
    /// saves and returns to the tasks panel; `Esc` discards and returns. Field
    /// navigation is the `TaskEditorModal`'s own. Mirrors
    /// `handle_automation_editor_pane_key`.
    pub(crate) fn handle_task_editor_pane_key(&mut self, code: KeyCode, mods: KeyModifiers) {
        let Some(editor) = self.task_ui.task_editor.as_mut() else {
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
                let Some(editor) = self.task_ui.task_editor.clone() else {
                    return;
                };
                if self.save_task(&editor) {
                    // A brand-new task lands at the top of the list.
                    if editor.editing_id.is_none() {
                        self.task_ui.task_panel_index = 0;
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
                // A force-deleted session lost its uncommitted work; confirm the
                // best-effort recovery first. Plain soft-deletes restore directly.
                if deleted.force_deleted {
                    self.modal =
                        super::modals::Modal::ConfirmRestore(super::modals::ConfirmRestoreModal {
                            deleted,
                        });
                } else {
                    self.modal.close();
                    self.restore_deleted_session(deleted);
                }
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
                self.new_session.repo_path = None;
                self.new_session.all_repos = None;
                self.new_session.normal_repos.clear();
            }
            KeyCode::Char('j') | KeyCode::Down if bs.index + 1 < bs.branches.len() => {
                bs.index += 1;
            }
            KeyCode::Char('k') | KeyCode::Up => {
                bs.index = bs.index.saturating_sub(1);
            }
            KeyCode::Enter => {
                let base_branch = bs.branches[bs.index].clone();
                self.new_session.base_branch = Some(base_branch);
                self.modal =
                    super::modals::Modal::SessionName(super::modals::SessionNameModal::default());
            }
            _ => {}
        }
    }

    fn handle_worktree_name_key(&mut self, code: KeyCode, mods: KeyModifiers) {
        let super::modals::Modal::WorktreeName(ref mut wn) = self.modal else {
            return;
        };
        match code {
            KeyCode::Esc => {
                self.modal.close();
                self.cancel_worktree_name();
            }
            KeyCode::Enter => {
                let new_branch = wn.name.value().trim().to_string();
                if new_branch.is_empty() {
                    self.set_error("Branch name cannot be empty");
                    return;
                }
                self.modal.close();
                self.confirm_worktree_name(&new_branch);
            }
            other => {
                super::modals::apply_text_input_key(Some(&mut wn.name), other, mods);
            }
        }
    }

    /// Clear the worktree-flow pending state when the branch-name modal is cancelled.
    fn cancel_worktree_name(&mut self) {
        self.new_session.base_branch = None;
        self.new_session.repo_path = None;
        self.new_session.all_repos = None;
        self.new_session.normal_repos.clear();
        self.new_session.session_name = None;
    }

    /// Spawn the worktree session for the confirmed branch name.
    fn confirm_worktree_name(&mut self, new_branch: &str) {
        let Some(base_branch) = self.new_session.base_branch.take() else {
            return;
        };
        // Use all repos for multi-repo projects, single repo otherwise
        let repo_paths = if let Some(all_repos) = self.new_session.all_repos.take() {
            self.new_session.repo_path = None;
            all_repos
        } else if let Some(repo_path) = self.new_session.repo_path.take() {
            vec![repo_path]
        } else {
            return;
        };
        let session_name = self.new_session.session_name.take();
        self.spawn_worktree_session(&repo_paths, new_branch, &base_branch, session_name);
    }

    fn handle_session_name_key(&mut self, code: KeyCode, mods: KeyModifiers) {
        let super::modals::Modal::SessionName(ref mut sn) = self.modal else {
            return;
        };
        match code {
            KeyCode::Esc => {
                self.modal.close();
                self.cancel_session_name();
            }
            KeyCode::Enter => {
                let name = sn.name.value().trim().to_string();
                if name.is_empty() {
                    self.set_error("Session name cannot be empty");
                    return;
                }
                self.modal.close();
                self.confirm_session_name(name);
            }
            other => {
                super::modals::apply_text_input_key(Some(&mut sn.name), other, mods);
            }
        }
    }

    /// Clear pending state when the session-name modal is cancelled.
    fn cancel_session_name(&mut self) {
        if self.new_session.base_branch.is_some() {
            // Worktree flow — clean up worktree-specific pending state.
            self.new_session.base_branch = None;
            self.new_session.repo_path = None;
            self.new_session.all_repos = None;
            self.new_session.normal_repos.clear();
        } else {
            // Normal flow — clean up spawn state.
            self.new_session.spawn_config = None;
            self.new_session.spawn_worktrees.clear();
            self.new_session.fork = false;
            self.new_session.parent_session_id = None;
        }
    }

    /// Advance from the confirmed session name to the next step of the flow.
    fn confirm_session_name(&mut self, name: String) {
        if self.new_session.base_branch.is_some() {
            // Worktree flow — proceed to branch name input.
            let branch = session_name_to_branch(&name);
            self.new_session.session_name = Some(name);
            let mut modal = super::modals::WorktreeNameModal::default();
            modal.name.set(&branch);
            self.modal = super::modals::Modal::WorktreeName(modal);
        } else if let Some(config) = self.new_session.spawn_config.take() {
            let worktrees = std::mem::take(&mut self.new_session.spawn_worktrees);
            if self.new_session.fork {
                // Fork flow — role already set, spawn directly.
                self.new_session.fork = false;
                self.do_spawn_session_async(name, &config, worktrees);
            } else {
                // Normal flow — proceed to role selection / spawn.
                self.finish_prepare_spawn(name, config, worktrees);
            }
        }
    }

    fn handle_host_picker_key(&mut self, code: KeyCode) {
        let super::modals::Modal::HostPicker(ref mut hp) = self.modal else {
            return;
        };
        let choice_count = hp.choices.len();
        match code {
            KeyCode::Esc => {
                self.modal.close();
                self.new_session.backend = None;
            }
            KeyCode::Char('j') | KeyCode::Down if hp.selected_index + 1 < choice_count => {
                hp.selected_index += 1;
            }
            KeyCode::Char('k') | KeyCode::Up => {
                hp.selected_index = hp.selected_index.saturating_sub(1);
            }
            KeyCode::Enter => {
                let backend = hp
                    .choices
                    .get(hp.selected_index)
                    .map(|c| c.backend.clone())
                    .unwrap_or_default();
                self.modal.close();
                self.confirm_host_picker(backend);
            }
            _ => {}
        }
    }

    /// `Enter` on the host picker: record the chosen backend (empty == local
    /// default) and advance to the repo picker. The backend is passed in to
    /// avoid re-borrowing the modal after it's closed.
    fn confirm_host_picker(&mut self, backend: String) {
        // Empty backend == local default.
        self.new_session.backend = if backend.is_empty() {
            None
        } else {
            Some(backend)
        };
        self.open_repo_picker();
    }

    fn handle_agent_picker_key(&mut self, code: KeyCode) {
        let super::modals::Modal::AgentPicker(ref mut ap) = self.modal else {
            return;
        };
        let choice_count = ap.choices.len();
        match code {
            KeyCode::Esc => {
                self.modal.close();
                self.new_session.spawn_config = None;
                self.new_session.spawn_worktrees.clear();
                self.new_session.spawn_name = None;
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
                self.confirm_agent_picker(chosen);
            }
            _ => {}
        }
    }

    /// `Enter` on the agent picker: stamp the chosen agent onto the pending
    /// spawn config and launch the session (a no-op if any pending state is
    /// missing). The chosen agent name is passed in to avoid re-borrowing the
    /// modal after it's closed.
    fn confirm_agent_picker(&mut self, chosen: Option<String>) {
        if let (Some(mut config), Some(name), Some(agent)) = (
            self.new_session.spawn_config.take(),
            self.new_session.spawn_name.take(),
            chosen,
        ) {
            config.agent = agent;
            let worktrees = std::mem::take(&mut self.new_session.spawn_worktrees);
            self.do_spawn_session_async(name, &config, worktrees);
        }
    }

    /// Gate a feature-flagged action: returns whether the feature is enabled,
    /// surfacing a toast naming the switch when it isn't. Callers still
    /// consume the key either way (a disabled chord must not reach the PTY).
    fn feature_gate(&mut self, enabled: bool, what: &str) -> bool {
        if !enabled {
            self.set_info(format!("{what} is disabled ([features] in settings.toml)"));
        }
        enabled
    }

    /// Run a feature-gated action when its switch is enabled (toasting the
    /// switch name otherwise), and always consume the key (`true`) so a
    /// disabled chord never falls through to the PTY.
    fn gated(&mut self, enabled: bool, what: &str, act: impl FnOnce(&mut Self)) -> bool {
        if self.feature_gate(enabled, what) {
            act(self);
        }
        true
    }

    pub(super) fn dispatch_action(&mut self, action: crate::session::Action) -> bool {
        if let Some(consumed) = self.dispatch_app_action(action) {
            return consumed;
        }
        if let Some(consumed) = self.dispatch_focus_action(action) {
            return consumed;
        }
        if let Some(consumed) = self.dispatch_panel_action(action) {
            return consumed;
        }
        if let Some(consumed) = self.dispatch_clipboard_action(action) {
            return consumed;
        }
        if let Some(consumed) = self.dispatch_session_list_action(action) {
            return consumed;
        }
        self.dispatch_scoped_pane_action(action)
    }

    /// Global app-control actions (quit, new/fork/restart/delete session, sync,
    /// open editor, undo/restore, theme/settings/help). Returns `Some(consumed)`
    /// when `action` is one of these, else `None`.
    fn dispatch_app_action(&mut self, action: crate::session::Action) -> Option<bool> {
        use crate::session::Action;
        let consumed = match action {
            Action::QuitApp => {
                self.should_quit = true;
                true
            }
            Action::NewSession => {
                self.act_new_session();
                true
            }
            Action::DeleteSession => self.act_delete_session(),
            Action::OpenInEditor => {
                self.open_active_in_editor();
                true
            }
            Action::StartSync => {
                self.start_sync();
                true
            }
            Action::ForkSession => {
                self.fork_active_session();
                true
            }
            Action::RestartSession => {
                self.restart_active_session();
                true
            }
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
            Action::ToggleHelp => {
                self.modal = super::modals::Modal::Help(super::modals::HelpModal::default());
                true
            }
            Action::OpenSettings => {
                self.open_settings_panel();
                true
            }
            _ => return None,
        };
        Some(consumed)
    }

    /// Focus-cycle and inter-session navigation actions. Returns
    /// `Some(consumed)` when `action` is one of these, else `None`.
    fn dispatch_focus_action(&mut self, action: crate::session::Action) -> Option<bool> {
        use crate::session::Action;
        match action {
            Action::FocusBackward => {
                self.focus = self.cycle_focus_backward();
                self.on_focus_changed();
            }
            Action::FocusForward => {
                self.focus = self.cycle_focus_forward();
                self.on_focus_changed();
            }
            Action::NextSession => self.switch_session_forward(),
            Action::PreviousSession => self.switch_session_backward(),
            _ => return None,
        }
        Some(true)
    }

    /// Feature-gated panel/pane toggles (shell, info panel, tasks, file viewer,
    /// global search, automations). Returns `Some(consumed)` when `action` is
    /// one of these, else `None`.
    fn dispatch_panel_action(&mut self, action: crate::session::Action) -> Option<bool> {
        use crate::session::Action;
        let consumed = match action {
            Action::ToggleShell => self.gated(
                self.features.shell_pane,
                "Shell pane",
                Self::toggle_shell_view,
            ),
            Action::ToggleReview => self.gated(
                self.features.code_review,
                "Code review",
                Self::toggle_code_review,
            ),
            Action::OpenAutomations => self.gated(
                self.features.automations,
                "Automations",
                Self::open_automations_list,
            ),
            Action::ToggleInfoPanel => self.gated(self.features.info_panel, "Info panel", |s| {
                s.show_info_panel = !s.show_info_panel;
                s.resize_sessions_to_content_area();
            }),
            Action::FocusTasks => {
                self.gated(self.features.tasks, "Tasks panel", Self::act_toggle_tasks)
            }
            Action::ToggleFileViewer => self.gated(
                self.features.file_viewer,
                "File viewer",
                Self::act_toggle_file_viewer,
            ),
            Action::GlobalSearch => self.gated(
                self.features.global_search,
                "Global search",
                Self::open_global_search,
            ),
            _ => return None,
        };
        Some(consumed)
    }

    /// Clipboard actions (Copy/Paste). Copy only consumes the key when there's a
    /// selection; otherwise it yields (false) so the terminal can send SIGINT.
    /// Paste is normally intercepted earlier (so it reaches modal text inputs);
    /// this path covers the plain-terminal case. Returns `Some(consumed)` when
    /// `action` is one of these, else `None`.
    fn dispatch_clipboard_action(&mut self, action: crate::session::Action) -> Option<bool> {
        use crate::session::Action;
        let consumed = match action {
            Action::Copy => {
                if self.text_selection.is_some() {
                    self.copy_selection_to_clipboard();
                    true
                } else {
                    false // no selection → let the terminal send SIGINT
                }
            }
            Action::Paste => {
                self.paste_from_clipboard();
                true
            }
            _ => return None,
        };
        Some(consumed)
    }

    /// Session-list-scoped actions (navigation, move, open, sort). Returns
    /// `Some(consumed)` when `action` is one of these, else `None`.
    fn dispatch_session_list_action(&mut self, action: crate::session::Action) -> Option<bool> {
        use crate::session::Action;
        match action {
            Action::SessionListNext => self.act_session_list_next(),
            Action::SessionListPrev => self.act_session_list_prev(),
            Action::SessionListOpen => self.focus = InputFocus::Terminal,
            Action::SessionListMoveDown => self.move_active_session(true),
            Action::SessionListMoveUp => self.move_active_session(false),
            Action::SessionListSortAlphabetically => self.sort_sessions_alphabetically(),
            _ => return None,
        }
        Some(true)
    }

    /// File-viewer and terminal-scroll scoped actions, delegated to their
    /// sub-dispatchers. This is the final fall-through arm of `dispatch_action`.
    fn dispatch_scoped_pane_action(&mut self, action: crate::session::Action) -> bool {
        use crate::session::Action;
        match action {
            Action::FileViewerDown
            | Action::FileViewerUp
            | Action::FileViewerCollapse
            | Action::FileViewerExpand
            | Action::FileViewerSearch
            | Action::FileViewerNextMatch
            | Action::FileViewerPrevMatch => self.dispatch_file_viewer_action(action),
            Action::TerminalScrollUp
            | Action::TerminalScrollDown
            | Action::TerminalPageUp
            | Action::TerminalPageDown => self.dispatch_terminal_scroll_action(action),
            // Every other action is handled by an earlier dispatcher in
            // `dispatch_action`, so this fall-through is never reached.
            _ => unreachable!("action handled by an earlier dispatcher"),
        }
    }

    /// Run a `FileViewer`-scoped action. Always consumes the key (`true`).
    fn dispatch_file_viewer_action(&mut self, action: crate::session::Action) -> bool {
        use crate::session::Action;
        match action {
            Action::FileViewerDown => self.file_viewer.move_selection(1),
            Action::FileViewerUp => self.file_viewer.move_selection(-1),
            Action::FileViewerCollapse => self.file_viewer.collapse(),
            Action::FileViewerExpand => self.file_viewer_expand(),
            Action::FileViewerSearch => self.file_viewer.start_search(),
            Action::FileViewerNextMatch => self.file_viewer.next_match(),
            Action::FileViewerPrevMatch => self.file_viewer.prev_match(),
            _ => {}
        }
        true
    }

    /// Run a terminal-scroll action. Always consumes the key (`true`).
    fn dispatch_terminal_scroll_action(&mut self, action: crate::session::Action) -> bool {
        use crate::session::Action;
        match action {
            Action::TerminalScrollUp => self.scroll_terminal_up(1),
            Action::TerminalScrollDown => self.scroll_terminal_down(1),
            Action::TerminalPageUp => {
                let amount = self.page_scroll_amount();
                self.scroll_terminal_up(amount);
            }
            Action::TerminalPageDown => {
                let amount = self.page_scroll_amount();
                self.scroll_terminal_down(amount);
            }
            _ => {}
        }
        true
    }

    /// `Ctrl+N`: in the automations context create an automation (mirrors `n`),
    /// else start the new-session wizard (clearing any leftover task prompt).
    fn act_new_session(&mut self) {
        if matches!(
            self.focus,
            InputFocus::Automations | InputFocus::AutomationEditor
        ) {
            self.new_automation_in_pane();
        } else {
            // A manual new-session must not inherit a task prompt or fork
            // parenthood left over from a cancelled task-spawn / fork.
            self.task_ui.pending_task_prompt = None;
            self.new_session.parent_session_id = None;
            self.start_new_session();
        }
    }

    /// `Ctrl+D`: delete the focused entity (session / automation / task). Editors
    /// and search capture their own keys earlier, so they yield here.
    fn act_delete_session(&mut self) -> bool {
        match self.focus {
            InputFocus::SessionList | InputFocus::FileViewer => {
                self.close_active_session();
                true
            }
            InputFocus::Automations => {
                self.handle_automations_pane_key(KeyCode::Char('d'));
                true
            }
            InputFocus::TaskList => {
                self.handle_task_list_key(KeyCode::Char('d'));
                true
            }
            InputFocus::AutomationEditor
            | InputFocus::AutomationRunHistory
            | InputFocus::TaskEditor
            | InputFocus::CodeReview
            | InputFocus::GlobalSearch => false,
            InputFocus::Terminal => false, // forward to PTY
        }
    }

    /// Toggle the tasks panel column (`F5`/`Ctrl+W`), mirroring the file viewer:
    /// showing it also focuses it; hiding it drops focus back to the list.
    fn act_toggle_tasks(&mut self) {
        self.show_tasks_panel = !self.show_tasks_panel;
        if self.show_tasks_panel {
            self.refresh_tasks();
            self.task_ui.task_panel_index = 0;
            self.focus = InputFocus::TaskList;
            // Populate the central-pane preview for the selected task (without
            // this the workspace shows the empty hint).
            self.sync_task_editor();
        } else if self.focus == InputFocus::TaskList {
            self.focus = InputFocus::SessionList;
        }
        self.resize_sessions_to_content_area();
    }

    /// Toggle the file viewer column; showing it rebuilds it for the active
    /// session, hiding it returns focus to the session list.
    fn act_toggle_file_viewer(&mut self) {
        self.show_file_viewer = !self.show_file_viewer;
        if self.show_file_viewer {
            self.rebuild_file_viewer_for_active();
        } else if self.focus == InputFocus::FileViewer {
            self.focus = InputFocus::SessionList;
        }
        self.resize_sessions_to_content_area();
    }

    /// Session-list `Ctrl+J`: step to the next session, or flow into the
    /// automations pane past the last so the left column reads as one list.
    /// With automations disabled there is no pane to flow into, so the list
    /// wraps onto itself.
    fn act_session_list_next(&mut self) {
        if self.features.automations && self.active_is_last_in_order() {
            self.focus = InputFocus::Automations;
            self.automation_ui.automation_panel_index = 0;
            self.refresh_automation_view();
        } else {
            self.switch_session_forward();
        }
    }

    /// Session-list `Ctrl+K`: step to the previous session, or flow into the
    /// automations pane (last row) above the first.
    fn act_session_list_prev(&mut self) {
        if self.features.automations && self.active_is_first_in_order() {
            self.focus = InputFocus::Automations;
            self.automation_ui.automation_panel_index = self
                .automation_ui
                .cached_automations
                .len()
                .saturating_sub(1);
            self.refresh_automation_view();
        } else {
            self.switch_session_backward();
        }
    }

    /// Expand the selected file-viewer node, opening it in the editor when it's
    /// a file (dirs just toggle). Shared by the `FileViewerExpand` action.
    pub(super) fn file_viewer_expand(&mut self) {
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
        let entries = crate::ui::theme::all_theme_entries();
        let entry_count = entries.len();
        let super::modals::Modal::ThemePicker(ref mut tp) = self.modal else {
            return;
        };
        match code {
            KeyCode::Esc => {
                // Cancel: undo the live preview by restoring the palette that
                // was active when the picker opened (nothing is persisted).
                crate::ui::theme::set_active(tp.original.clone());
                self.modal.close();
            }
            KeyCode::Char('j') | KeyCode::Down if tp.index + 1 < entry_count => {
                tp.index += 1;
                crate::ui::theme::set_active(entries[tp.index].palette.clone());
            }
            KeyCode::Char('k') | KeyCode::Up if tp.index > 0 => {
                tp.index -= 1;
                crate::ui::theme::set_active(entries[tp.index].palette.clone());
            }
            KeyCode::Enter => {
                let idx = tp.index;
                self.modal.close();
                self.commit_theme_selection(entries, idx);
            }
            _ => {}
        }
    }

    /// `Enter` on the theme picker: activate the selected entry and persist it
    /// as the active theme.
    fn commit_theme_selection(
        &mut self,
        entries: Vec<crate::session::theme_config::ThemeEntry>,
        idx: usize,
    ) {
        let Some(entry) = entries.into_iter().nth(idx) else {
            return;
        };
        crate::ui::theme::set_active(entry.palette.clone());
        if let Err(e) = self.db.set_active_theme(&entry.name) {
            tracing::error!("Failed to persist active theme: {e}");
            self.set_error(format!("Failed to persist theme: {e}"));
        }
        self.active_theme = entry;
    }

    pub(crate) fn start_branch_selection(&mut self) {
        // Resolve the remote host (if any) so branch listing targets the
        // session's machine. Cloned so we don't hold a borrow on `self`.
        let host = self
            .host_for_backend(self.new_session.backend.as_deref())
            .cloned();
        let host = host.as_ref();

        let Some(repo_path) = self.new_session.repo_path.clone() else {
            return;
        };
        let repo_path = repo_path.as_path();

        Self::fetch_pending_repos(host, repo_path, self.new_session.all_repos.as_ref());

        match crate::git::list_branches_on(host, repo_path) {
            Ok(branches) if branches.is_empty() => {
                self.set_error("No branches found in repository");
                self.new_session.repo_path = None;
            }
            Ok(branches) => {
                let branches = Self::ordered_branch_list(host, repo_path, branches);
                self.modal =
                    super::modals::Modal::BranchSelector(super::modals::BranchSelectorModal {
                        index: 0,
                        branches,
                    });
            }
            Err(e) => {
                error!("Failed to list branches: {e}");
                self.set_error(format!("Failed to list branches: {e:#}"));
                self.new_session.repo_path = None;
            }
        }
    }

    /// Fetch origin for the primary repo and any extra worktree repos so
    /// branch lists are up-to-date. Failures are non-fatal (logged only).
    fn fetch_pending_repos(
        host: Option<&crate::session::HostDef>,
        repo_path: &std::path::Path,
        all_repos: Option<&Vec<std::path::PathBuf>>,
    ) {
        if let Err(e) = crate::git::git_fetch_on(host, repo_path) {
            warn!("git fetch origin failed (continuing): {e:#}");
        }
        let Some(all_repos) = all_repos else {
            return;
        };
        for extra_repo in all_repos.iter().skip(1) {
            if let Err(e) = crate::git::git_fetch_on(host, extra_repo) {
                warn!(
                    "git fetch origin failed for {} (continuing): {e:#}",
                    extra_repo.display()
                );
            }
        }
    }

    /// Order a branch list for the selector: the local default branch first,
    /// then `origin/<default>` (remote-based branching) pinned at the very top.
    fn ordered_branch_list(
        host: Option<&crate::session::HostDef>,
        repo_path: &std::path::Path,
        mut branches: Vec<String>,
    ) -> Vec<String> {
        // Move the default branch to front so it's pre-selected.
        if let Some(default) = crate::git::default_branch_on(host, repo_path, &branches) {
            if let Some(pos) = branches.iter().position(|b| b == &default) {
                let branch = branches.remove(pos);
                branches.insert(0, branch);
            }
        }

        // Insert origin/<default> at position 0 for remote-based branching.
        let remote_ref = crate::git::default_branch_from_remote_on(host, repo_path)
            .map(|name| format!("origin/{name}"))
            .or_else(|| {
                for candidate in ["origin/main", "origin/master"] {
                    if crate::git::branch_exists_on(host, repo_path, candidate) {
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

    fn handle_repo_picker_key(&mut self, code: KeyCode, mods: KeyModifiers) {
        let super::modals::Modal::RepoPicker(ref rp) = self.modal else {
            return;
        };
        // Ctrl+P: import the typed path as a *parent* folder whose git
        // sub-directories are re-scanned on each picker open. Works from any
        // focus (uses the path input value). Not `Ctrl+I` — that is `Tab`.
        if mods.contains(KeyModifiers::CONTROL)
            && matches!(code, KeyCode::Char('p') | KeyCode::Char('P'))
        {
            self.repo_picker_import_parent();
            return;
        }
        match rp.focus {
            super::modals::RepoPickerFocus::List => self.handle_repo_picker_list_key(code),
            super::modals::RepoPickerFocus::Input => self.handle_repo_picker_input_key(code, mods),
            super::modals::RepoPickerFocus::Search => {
                self.handle_repo_picker_search_key(code, mods)
            }
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

    /// `Space` on the row under the cursor: toggle the selected flag of a repo,
    /// or expand/collapse a parent header's child tree.
    fn repo_picker_toggle_selected(&mut self) {
        let super::modals::Modal::RepoPicker(ref mut rp) = self.modal else {
            return;
        };
        let Some(&real_idx) = rp.filtered_indices.get(rp.list_index) else {
            return;
        };
        if rp.is_header_row(real_idx) {
            rp.toggle_collapsed(real_idx);
            return;
        }
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
        if rp.is_header_row(real_idx) {
            return;
        }
        let Some(wt) = rp.worktree.get_mut(real_idx) else {
            return;
        };
        *wt = !*wt;
        if *wt {
            if let Some(sel) = rp.selected.get_mut(real_idx) {
                *sel = true;
            }
        }
    }

    /// Delete the bookmark under the cursor. A standalone repo is removed in
    /// place; a parent header drops the parent bookmark and its (ephemeral)
    /// child rows via a full re-scan; a child row has no persistent identity, so
    /// deleting it is a no-op (delete its parent header instead).
    fn repo_picker_delete_bookmark(&mut self) {
        let super::modals::Modal::RepoPicker(ref mut rp) = self.modal else {
            return;
        };
        let Some(&real_idx) = rp.filtered_indices.get(rp.list_index) else {
            return;
        };
        let path = rp.bookmarks[real_idx].clone();
        let is_header = rp.is_header_row(real_idx);
        let is_child = rp.is_child_row(real_idx);

        if is_child {
            self.set_status(
                super::StatusLevel::Info,
                "Child of a parent bookmark — delete the parent header instead",
            );
            return;
        }

        if let Err(e) = self.db.delete_repo_bookmark(&path) {
            error!("Failed to delete repo bookmark: {e}");
        }

        if is_header {
            // Rebuild so the header and all its child rows disappear together.
            self.refresh_repo_picker_rows();
            return;
        }

        let super::modals::Modal::RepoPicker(ref mut rp) = self.modal else {
            return;
        };
        rp.bookmarks.remove(real_idx);
        rp.selected.remove(real_idx);
        rp.worktree.remove(real_idx);
        rp.is_header.remove(real_idx);
        rp.is_child.remove(real_idx);
        self.recompute_repo_filter();
    }

    fn handle_repo_picker_input_key(&mut self, code: KeyCode, mods: KeyModifiers) {
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
            other => {
                if !super::modals::apply_text_input_key(Some(&mut rp.path_input), other, mods) {
                    return;
                }
            }
        }
        self.update_repo_picker_path_suggestion();
    }

    /// Commit the typed path in the repo-picker input: add or re-select the
    /// bookmark, persist it, clear the input, and refresh the filter.
    fn repo_picker_commit_path_input(&mut self) {
        // For a remote target the path is a remote path: don't expand `~`
        // against the local home, and don't persist it as a local bookmark.
        let remote = self.new_session.backend.is_some();
        let super::modals::Modal::RepoPicker(ref mut rp) = self.modal else {
            return;
        };
        let path = rp.path_input.value().trim().to_string();
        if path.is_empty() {
            self.recompute_repo_filter();
            return;
        }
        let expanded = if remote {
            std::path::PathBuf::from(&path)
        } else {
            paths::expand_tilde(&path)
        };
        let persist = Self::repo_picker_select_or_add_row(rp, &expanded);
        if persist && !remote {
            if let Err(e) = self.db.upsert_repo_bookmark(&expanded) {
                error!("Failed to save repo bookmark: {e}");
                self.set_error(format!("Failed to save repo bookmark: {e}"));
            }
        }
        let super::modals::Modal::RepoPicker(ref mut rp) = self.modal else {
            return;
        };
        rp.path_input.clear();
        rp.path_suggestion = None;
        self.recompute_repo_filter();
    }

    /// Select an already-represented bookmark row for `expanded`, or push a new
    /// auto-selected row. Returns whether the path should be persisted as a
    /// standalone bookmark: a path already shown as a parent's child (or the
    /// parent header itself) is already covered, so it is not re-persisted.
    fn repo_picker_select_or_add_row(
        rp: &mut super::modals::RepoPickerModal,
        expanded: &std::path::Path,
    ) -> bool {
        // If already represented, just select it (no duplicate row or DB entry).
        let Some(idx) = rp.bookmarks.iter().position(|p| p == expanded) else {
            rp.push_row(expanded.to_path_buf(), true, false, false);
            return true;
        };
        let is_child = rp.is_child_row(idx);
        let is_header = rp.is_header_row(idx);
        if !is_header {
            rp.selected[idx] = true;
        }
        !is_child && !is_header
    }

    /// Import the typed path as a *parent* folder: persist it as a parent
    /// bookmark, then rebuild the list (re-scanning its git sub-directories).
    /// The parent itself is not added as a selectable repo — its children are.
    fn repo_picker_import_parent(&mut self) {
        let super::modals::Modal::RepoPicker(ref mut rp) = self.modal else {
            return;
        };
        let path = rp.path_input.value().trim().to_string();
        if path.is_empty() {
            self.set_status(
                super::StatusLevel::Info,
                "Type a folder path, then Ctrl+P to import its repos as a parent",
            );
            return;
        }
        let expanded = paths::expand_tilde(&path);
        if let Err(e) = self.db.upsert_repo_bookmark_kind(&expanded, true) {
            error!("Failed to save parent bookmark: {e}");
            self.set_error(format!("Failed to save parent bookmark: {e}"));
        }
        if let super::modals::Modal::RepoPicker(ref mut rp) = self.modal {
            rp.path_input.clear();
            rp.path_suggestion = None;
            rp.focus = super::modals::RepoPickerFocus::List;
        }
        self.refresh_repo_picker_rows();
    }

    pub(super) fn update_repo_picker_path_suggestion(&mut self) {
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

    fn handle_repo_picker_search_key(&mut self, code: KeyCode, mods: KeyModifiers) {
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
            // Cursor moves don't change the filter; edits (incl. Ctrl+W/U) do.
            KeyCode::Left => rp.search_input.move_left(),
            KeyCode::Right => rp.search_input.move_right(),
            KeyCode::Home => rp.search_input.home(),
            KeyCode::End => rp.search_input.end(),
            other => {
                if super::modals::apply_text_input_key(Some(&mut rp.search_input), other, mods) {
                    self.recompute_repo_filter();
                }
            }
        }
    }

    fn recompute_repo_filter(&mut self) {
        if let super::modals::Modal::RepoPicker(ref mut rp) = self.modal {
            rp.recompute_filter();
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
            self.spawn_repo_picker_no_repos();
        } else if !worktree_repos.is_empty() {
            self.spawn_repo_picker_worktrees(worktree_repos, normal_repos);
        } else {
            self.spawn_repo_picker_normal(normal_repos);
        }
    }

    /// No repos selected — spawn with HOME as cwd. For a remote target,
    /// leave cwd unset so the remote session starts in its own default
    /// directory (local $HOME is meaningless there).
    fn spawn_repo_picker_no_repos(&mut self) {
        let mut config = SessionConfig::default();
        if self.new_session.backend.is_none() {
            if let Some(home) = crate::paths::home_dir() {
                config.cwd = Some(home);
            }
        }
        self.spawn_session_with_config(&config);
    }

    /// Has worktree repos — go to branch selection.
    /// Store normal repos for inclusion after worktree creation.
    fn spawn_repo_picker_worktrees(
        &mut self,
        worktree_repos: Vec<std::path::PathBuf>,
        normal_repos: Vec<std::path::PathBuf>,
    ) {
        self.new_session.repo_path = Some(worktree_repos[0].clone());
        self.new_session.all_repos = if worktree_repos.len() > 1 {
            Some(worktree_repos)
        } else {
            None
        };
        self.new_session.normal_repos = normal_repos;
        self.start_branch_selection();
    }

    /// All normal repos — spawn directly (local-tmux), going straight
    /// to the agent picker chain.
    fn spawn_repo_picker_normal(&mut self, normal_repos: Vec<std::path::PathBuf>) {
        self.new_session.additional_dirs = normal_repos[1..].to_vec();
        let config = SessionConfig {
            cwd: Some(normal_repos[0].clone()),
            ..SessionConfig::default()
        };
        self.spawn_session_with_config(&config);
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
    use super::{is_ctrl_letter_chord, session_name_to_branch};
    use crossterm::event::{KeyCode, KeyModifiers};

    #[test]
    fn ctrl_letter_chord_detects_readline_namespace() {
        // Bare Ctrl+letter — the readline-conflicting namespace.
        assert!(is_ctrl_letter_chord(
            KeyCode::Char('a'),
            KeyModifiers::CONTROL
        ));
        assert!(is_ctrl_letter_chord(
            KeyCode::Char('r'),
            KeyModifiers::CONTROL
        ));
        // Plain letters, F-keys, and Ctrl+<non-letter> are not in the namespace,
        // so a passthrough action bound to them keeps working in the terminal.
        assert!(!is_ctrl_letter_chord(
            KeyCode::Char('a'),
            KeyModifiers::NONE
        ));
        assert!(!is_ctrl_letter_chord(KeyCode::F(3), KeyModifiers::NONE));
        assert!(!is_ctrl_letter_chord(
            KeyCode::Char('1'),
            KeyModifiers::CONTROL
        ));
        // Extra modifiers take it out of the bare-Ctrl namespace.
        assert!(!is_ctrl_letter_chord(
            KeyCode::Char('a'),
            KeyModifiers::CONTROL | KeyModifiers::ALT
        ));
    }

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
