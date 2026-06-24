//! Automations cluster for the Thurbox TUI application.
//!
//! Relocated from `app/mod.rs` and `app/key_handlers.rs` (ADR-22 step 2): the
//! `impl App` methods that drive scheduled automations — firing due schedules,
//! the Ctrl+P list modal, the in-pane editor + run-history panel, and the
//! create/edit/toggle/run/delete operations. The automation UI state itself
//! lives in [`AutomationUiState`](super::automation_state::AutomationUiState)
//! as `App::automation_ui`; this module only operates on it.

use super::modals;
use super::view;
use super::{App, InputFocus, StatusLevel};
use crate::session::{
    Automation, AutomationAction, AutomationRunStatus, AutomationSchedule, SessionId,
};
use crossterm::event::{KeyCode, KeyModifiers};
use tracing::{debug, error, info};

impl App {
    /// Process due scheduled commands from the database (fallback).
    ///
    /// The primary dispatch is via `tmux run-shell -b -d` timers set at
    /// scheduling time. This tick-loop catches commands whose tmux timer
    /// failed or was never set (e.g., scheduled while Thurbox was down).
    /// Throttled to once per second (~100 ticks at 10ms each).
    /// Fire any due automations. Called once per ~second from `tick()`; pass
    /// `force = true` for the one-shot startup catch-up pass (ignores cadence).
    pub(crate) fn process_automations(&mut self, force: bool) {
        // Automations fully off: the TUI neither fires schedules nor catches
        // up at startup (explicit `thurbox-cli automation` use still works).
        if !self.features.automations {
            return;
        }
        if !force && self.metrics.tick_count % 100 != 0 {
            return;
        }
        let now = crate::sync::current_time_millis();
        let due = match self.db.due_automations(now) {
            Ok(autos) => autos,
            Err(e) => {
                error!("Failed to fetch due automations: {e}");
                return;
            }
        };
        for auto in due {
            // Claim before firing so a concurrent headless `automation tick`
            // (keeper window / systemd) can't double-fire the same automation.
            // Claim advances the schedule; `None` disables a spent one-shot.
            let next = auto.schedule.next_after(now, auto.timezone.as_deref());
            match self
                .db
                .claim_due_automation(auto.id, auto.next_run_at.unwrap_or(0), next, now)
            {
                Ok(true) => {}
                Ok(false) => {
                    debug!(
                        automation_id = auto.id,
                        "automation claim lost to a concurrent firer (headless tick / other instance)"
                    );
                    continue;
                }
                Err(e) => {
                    error!("Failed to claim automation {}: {e}", auto.id);
                    continue;
                }
            }
            let (status, detail, related) = self.fire_automation(&auto);
            if let Err(e) = self
                .db
                .record_automation_run(auto.id, status, &detail, related)
            {
                error!("Failed to record run for automation {}: {e}", auto.id);
            }
        }
    }

    /// Execute a single automation's action, returning its run status, detail,
    /// and the session it sent to / spawned (when one exists).
    fn fire_automation(
        &mut self,
        auto: &Automation,
    ) -> (AutomationRunStatus, String, Option<SessionId>) {
        match &auto.action {
            AutomationAction::Send { session_id } => {
                if self.sessions.iter().any(|s| s.info.id == *session_id) {
                    self.send_prompt_to_session(*session_id, &auto.prompt, 0);
                    info!("Automation {} sent prompt to {}", auto.id, session_id);
                    (
                        AutomationRunStatus::Success,
                        format!("sent to {session_id}"),
                        Some(*session_id),
                    )
                } else {
                    (
                        AutomationRunStatus::Skipped,
                        "target session not running".to_string(),
                        None,
                    )
                }
            }
            AutomationAction::Spawn {
                repo_path,
                worktree_branch,
                base_branch,
                agent,
                extra_repos,
            } => match self.spawn_for_automation(
                auto,
                repo_path,
                worktree_branch.as_deref(),
                base_branch.as_deref(),
                agent.as_deref(),
                extra_repos,
            ) {
                Ok(session_id) => (
                    AutomationRunStatus::Success,
                    format!("session {session_id}"),
                    Some(session_id),
                ),
                Err(e) => {
                    error!("Automation {} spawn failed: {e}", auto.id);
                    (AutomationRunStatus::Error, e, None)
                }
            },
            AutomationAction::Exec { command } => {
                let (status, detail) = crate::session_ops::run_exec_command(command);
                (status, detail, None)
            }
        }
    }

    /// Spawn (or reuse) the session for a `Spawn` automation and queue its
    /// prompt. Each automation owns one session named `auto-<id>`; a recurring
    /// automation reuses that session on later fires (and after a TUI restart,
    /// where it is restored from the database by name).
    fn spawn_for_automation(
        &mut self,
        auto: &Automation,
        repo_path: &std::path::Path,
        worktree_branch: Option<&str>,
        base_branch: Option<&str>,
        agent: Option<&str>,
        extra_repos: &[crate::session::ExtraRepo],
    ) -> Result<SessionId, String> {
        self.spawn_and_prompt(
            format!("auto-{}", auto.id),
            repo_path,
            worktree_branch,
            base_branch,
            agent,
            extra_repos,
            &auto.prompt,
        )
    }

    /// Open the automations list modal.
    pub(crate) fn open_automations_list(&mut self) {
        self.refresh_automations();
        let now = crate::sync::current_time_millis();
        let entries: Vec<modals::AutomationListEntry> = self
            .automation_ui
            .cached_automations
            .iter()
            .map(|a| modals::AutomationListEntry {
                id: a.id,
                name: a.name.clone(),
                summary: format_automation_summary(a, now),
                enabled: a.enabled,
            })
            .collect();
        self.modal =
            modals::Modal::AutomationsList(modals::AutomationsListModal { index: 0, entries });
    }

    /// Open a blank automation editor as a **centered overlay** (the Ctrl+P list
    /// path). A new `Send` automation defaults to the active session. The
    /// in-pane editor uses [`new_automation_in_pane`](Self::new_automation_in_pane)
    /// instead.
    pub(crate) fn open_automation_editor(&mut self) {
        self.modal = modals::Modal::AutomationEditor(self.blank_automation_editor());
    }

    /// `(id, name)` pairs for every session, used to populate the editor's Send
    /// target selector.
    pub(crate) fn session_target_choices(&self) -> Vec<(SessionId, String)> {
        self.sessions
            .iter()
            .map(|s| (s.info.id, s.info.name.clone()))
            .collect()
    }

    /// A blank editor for a new automation, with its Send target list populated
    /// from the running sessions and defaulting to the active one.
    fn blank_automation_editor(&self) -> modals::AutomationEditorModal {
        let mut m = modals::AutomationEditorModal::default();
        let active = self.sessions.get(self.active_index).map(|s| s.info.id);
        m.set_target_sessions(self.session_target_choices(), active);
        m
    }

    /// Open the centered-overlay editor pre-filled for an existing automation
    /// (the Ctrl+P list path).
    fn open_edit_automation(&mut self, id: i64) {
        let Some(auto) = self
            .automation_ui
            .cached_automations
            .iter()
            .find(|a| a.id == id)
            .cloned()
        else {
            return;
        };
        self.modal = modals::Modal::AutomationEditor(self.build_automation_editor(&auto));
    }

    /// Build an editor pre-filled from an existing automation, with its Send
    /// target list populated from the running sessions.
    fn build_automation_editor(&self, auto: &Automation) -> modals::AutomationEditorModal {
        let mut m = modals::AutomationEditorModal::from_automation(auto);
        let selected = match &auto.action {
            AutomationAction::Send { session_id } => Some(*session_id),
            AutomationAction::Spawn { .. } | AutomationAction::Exec { .. } => None,
        };
        m.set_target_sessions(self.session_target_choices(), selected);
        m
    }

    /// Keep the in-pane automation editor (`self.automation_ui.automation_editor`) in sync with
    /// the current focus + selection:
    /// - [`InputFocus::Automations`] → mirror the selected automation (preview).
    /// - [`InputFocus::AutomationEditor`] → leave in-progress edits untouched.
    /// - anything else → drop it (we're no longer in the automation context).
    pub(crate) fn sync_automation_editor(&mut self) {
        match self.focus {
            InputFocus::Automations => {
                self.automation_ui.automation_editor = self
                    .automation_ui
                    .cached_automations
                    .get(self.automation_ui.automation_panel_index)
                    .cloned()
                    .map(|auto| self.build_automation_editor(&auto));
            }
            // Keep the editor + its run history intact while editing or
            // browsing history.
            InputFocus::AutomationEditor | InputFocus::AutomationRunHistory => {}
            _ => self.automation_ui.automation_editor = None,
        }
    }

    /// The id of the automation currently scoped in the central pane (the one
    /// being edited/previewed), if it's an existing automation.
    pub(crate) fn scoped_automation_id(&self) -> Option<i64> {
        self.automation_ui
            .automation_editor
            .as_ref()
            .and_then(|m| m.editing_id)
    }

    /// Open the session associated with the selected run-history entry.
    /// Switches to that session's terminal when it's still open, otherwise
    /// sets a status message.
    pub(crate) fn open_run_related_session(&mut self) {
        let Some(run) = self
            .automation_ui
            .cached_automation_runs
            .get(self.automation_ui.automation_run_index)
        else {
            return;
        };
        // Prefer the typed column (v28+). Pre-v28 rows only embed the id in
        // their free-text detail (e.g. "session <uuid>"), so fall back to the
        // first token that parses as a session id.
        let session_id = run.related_session_id.or_else(|| {
            run.detail
                .split_whitespace()
                .find_map(|tok| tok.parse::<SessionId>().ok())
        });
        let Some(session_id) = session_id else {
            self.set_status(StatusLevel::Info, "This run has no related session");
            return;
        };
        match self.sessions.iter().position(|s| s.info.id == session_id) {
            Some(idx) => {
                self.active_index = idx;
                self.focus = InputFocus::Terminal;
                // Leaving the automation context clears the editor/run cache.
                self.refresh_automation_view();
            }
            None => self.set_status(StatusLevel::Info, "Related session is no longer open"),
        }
    }

    /// Start a brand-new automation in the central pane and focus the editor.
    pub(crate) fn new_automation_in_pane(&mut self) {
        self.automation_ui.automation_editor = Some(self.blank_automation_editor());
        self.focus = InputFocus::AutomationEditor;
    }

    /// Re-sync the in-pane editor preview and the run-history cache after the
    /// automations selection or an automation itself changes.
    pub(crate) fn refresh_automation_view(&mut self) {
        self.sync_automation_editor();
        self.refresh_selected_automation_runs();
    }

    /// Move focus into the central-pane editor for the selected automation
    /// (mirrors `Enter` on a session focusing its terminal).
    pub(crate) fn enter_automation_editor(&mut self) {
        // Build the editor for the current selection (focus is still
        // `Automations`, so `sync` populates it), then focus it.
        self.sync_automation_editor();
        if self.automation_ui.automation_editor.is_some() {
            self.focus = InputFocus::AutomationEditor;
        }
    }

    /// Validate and submit the centered-overlay editor (create or update).
    pub(crate) fn submit_automation_editor(&mut self) {
        let modals::Modal::AutomationEditor(ref m) = self.modal else {
            return;
        };
        let m = m.clone();
        if self.save_automation(&m) {
            self.modal.close();
        }
    }

    /// Validate `m` and persist it (create or update). Returns `true` on success;
    /// on failure sets an error status and returns `false` (leaving the editor
    /// open). Refreshes the cached automations on success. Shared by the overlay
    /// and in-pane editors.
    fn save_automation(&mut self, m: &modals::AutomationEditorModal) -> bool {
        let name = m.name.value().trim().to_string();
        if name.is_empty() {
            self.set_error("Name cannot be empty");
            return false;
        }
        let prompt = m.prompt.value().trim().to_string();
        if prompt.is_empty() {
            self.set_error("Prompt cannot be empty");
            return false;
        }

        let now = crate::sync::current_time_millis();
        let schedule = match m.build_schedule(now) {
            Ok(s) => s,
            Err(e) => {
                self.set_error(e);
                return false;
            }
        };

        let timezone = m.timezone();

        let Some(action) = self.build_automation_action(m) else {
            return false;
        };

        let next_run_at = m
            .enabled
            .then(|| schedule.next_after(now, timezone.as_deref()))
            .flatten();

        let new = crate::storage::automations::NewAutomation {
            name,
            enabled: m.enabled,
            schedule,
            timezone,
            action,
            prompt,
            next_run_at,
        };
        let Some(result) = self.persist_automation(m.editing_id, new) else {
            return false;
        };

        if let Err(e) = result {
            error!("Failed to save automation: {e}");
            self.set_error("Failed to save automation");
            return false;
        }
        self.refresh_automations();
        self.set_status(StatusLevel::Success, "Automation saved");
        true
    }

    /// Build the [`AutomationAction`] from the editor's action fields. Returns
    /// `None` (after setting an error status) when a required field is missing.
    fn build_automation_action(
        &mut self,
        m: &modals::AutomationEditorModal,
    ) -> Option<AutomationAction> {
        match m.action {
            modals::AutomationActionKind::Send => {
                let Some(session_id) = m.selected_target().map(|(id, _)| *id) else {
                    self.set_error("No target session — start a session first");
                    return None;
                };
                Some(AutomationAction::Send { session_id })
            }
            modals::AutomationActionKind::Spawn => {
                let repo = m.repo.value().trim();
                if repo.is_empty() {
                    self.set_error("Repo path required for spawn action");
                    return None;
                }
                let worktree = m.worktree.value().trim();
                let agent = m.agent.value().trim();
                Some(AutomationAction::Spawn {
                    // Expand `~` so the stored path is absolute (git and the
                    // session cwd don't expand it themselves).
                    repo_path: crate::paths::expand_tilde(repo),
                    worktree_branch: (!worktree.is_empty()).then(|| worktree.to_string()),
                    base_branch: None,
                    agent: (!agent.is_empty()).then(|| agent.to_string()),
                    // The TUI automation editor is single-repo; multi-repo spawns
                    // are authored via the CLI (`--add-repo`/`--add-dir`).
                    extra_repos: Vec::new(),
                })
            }
            modals::AutomationActionKind::Exec => {
                let command = m.command.value().trim();
                if command.is_empty() {
                    self.set_error("Command required for exec action");
                    return None;
                }
                Some(AutomationAction::Exec {
                    command: command.to_string(),
                })
            }
        }
    }

    /// Persist the automation: update the existing row (`editing_id`) or create
    /// a new one. Returns the DB result, or `None` (after an error status) when
    /// editing a row that no longer exists.
    fn persist_automation(
        &mut self,
        editing_id: Option<i64>,
        new: crate::storage::automations::NewAutomation,
    ) -> Option<rusqlite::Result<()>> {
        match editing_id {
            Some(id) => match self.db.get_automation(id) {
                Ok(Some(mut auto)) => {
                    auto.name = new.name;
                    auto.prompt = new.prompt;
                    auto.schedule = new.schedule;
                    auto.timezone = new.timezone;
                    auto.action = new.action;
                    auto.enabled = new.enabled;
                    auto.next_run_at = new.next_run_at;
                    Some(self.db.update_automation(&auto))
                }
                Ok(None) => {
                    self.set_error("Automation no longer exists");
                    None
                }
                Err(e) => Some(Err(e)),
            },
            None => Some(self.db.create_automation(&new).map(|_| ())),
        }
    }

    /// Refresh the cached automations from the database.
    pub(crate) fn refresh_automations(&mut self) {
        match self.db.list_automations() {
            Ok(autos) => self.automation_ui.cached_automations = autos,
            Err(e) => error!("Failed to list automations: {e}"),
        }
        // Keep the pane selection in range. The pane stays focusable even when
        // empty (so Ctrl+N can create the first automation), so focus is left
        // where it is.
        if self.automation_ui.cached_automations.is_empty() {
            self.automation_ui.automation_panel_index = 0;
        } else if self.automation_ui.automation_panel_index
            >= self.automation_ui.cached_automations.len()
        {
            self.automation_ui.automation_panel_index =
                self.automation_ui.cached_automations.len() - 1;
        }
        // Keep the central-pane run history fresh while the pane is scoped (e.g.
        // a just-fired automation gains a new run).
        self.refresh_selected_automation_runs();
    }

    /// Load the run history for the automation currently scoped in the central
    /// pane. No-op (and clears the cache) unless the automations context is
    /// active and points at a real automation.
    pub(crate) fn refresh_selected_automation_runs(&mut self) {
        // While editing / browsing history the runs belong to the scoped
        // automation; in list preview they follow the highlighted row.
        let editing_id = self
            .automation_ui
            .automation_editor
            .as_ref()
            .and_then(|m| m.editing_id);
        let selected_id = match self.focus {
            InputFocus::AutomationEditor | InputFocus::AutomationRunHistory => editing_id,
            InputFocus::Automations => self
                .automation_ui
                .cached_automations
                .get(self.automation_ui.automation_panel_index)
                .map(|a| a.id),
            _ => None,
        };
        let Some(id) = selected_id else {
            self.automation_ui.cached_automation_runs.clear();
            self.automation_ui.cached_automation_runs_id = None;
            self.automation_ui.automation_run_index = 0;
            return;
        };
        match self.db.list_automation_runs(id, 20) {
            Ok(runs) => {
                self.automation_ui.cached_automation_runs = runs;
                self.automation_ui.cached_automation_runs_id = Some(id);
            }
            Err(e) => {
                error!("Failed to list automation runs for {id}: {e}");
                self.automation_ui.cached_automation_runs.clear();
                self.automation_ui.cached_automation_runs_id = None;
            }
        }
        // Keep the run-history selection in range.
        let len = self.automation_ui.cached_automation_runs.len();
        if len == 0 {
            self.automation_ui.automation_run_index = 0;
        } else if self.automation_ui.automation_run_index >= len {
            self.automation_ui.automation_run_index = len - 1;
        }
    }

    /// Toggle an automation's enabled state, recomputing `next_run_at` on enable.
    fn toggle_automation_by_id(&mut self, id: i64) {
        let Ok(Some(mut auto)) = self.db.get_automation(id) else {
            return;
        };
        auto.enabled = !auto.enabled;
        auto.next_run_at = if auto.enabled {
            auto.schedule
                .next_after(crate::sync::current_time_millis(), auto.timezone.as_deref())
        } else {
            None
        };
        if let Err(e) = self.db.update_automation(&auto) {
            error!("Failed to toggle automation {id}: {e}");
        }
        self.refresh_automations();
    }

    /// Mark an automation due so the next tick fires it.
    fn run_automation_by_id(&mut self, id: i64) {
        match self.db.trigger_automation_now(id) {
            Ok(true) => {
                self.refresh_automations();
                self.set_status(StatusLevel::Success, "Automation will run now");
            }
            Ok(false) => self.set_error("Automation not found"),
            Err(e) => {
                error!("Failed to trigger automation {id}: {e}");
                self.set_error("Failed to trigger automation");
            }
        }
    }

    /// Delete an automation by ID and refresh the cache.
    fn delete_automation_by_id(&mut self, id: i64) {
        match self.db.delete_automation(id) {
            Ok(true) => {
                self.refresh_automations();
                self.set_status(StatusLevel::Success, "Automation deleted");
            }
            Ok(false) => self.set_error("Automation not found"),
            Err(e) => {
                error!("Failed to delete automation {id}: {e}");
                self.set_error("Failed to delete automation");
            }
        }
    }

    /// Step the run-history selection by `delta`, clamped to the run count.
    pub(crate) fn move_run_history_selection(&mut self, delta: i32) {
        let len = self.automation_ui.cached_automation_runs.len();
        if len == 0 {
            self.automation_ui.automation_run_index = 0;
            return;
        }
        let next =
            (self.automation_ui.automation_run_index as i32 + delta).clamp(0, len as i32 - 1);
        self.automation_ui.automation_run_index = next as usize;
    }

    /// Step the automations-pane selection by `delta`, clamped, refreshing the
    /// preview.
    pub(crate) fn move_automation_selection(&mut self, delta: i32) {
        let len = self.automation_ui.cached_automations.len();
        if len == 0 {
            return;
        }
        let next =
            (self.automation_ui.automation_panel_index as i32 + delta).clamp(0, len as i32 - 1);
        let next = next as usize;
        if next != self.automation_ui.automation_panel_index {
            self.automation_ui.automation_panel_index = next;
            self.refresh_automation_view();
        }
    }

    // ---- Key handling (relocated from key_handlers.rs) -------------------

    /// The in-pane automation/task editor + run-history capture input like the
    /// overlay modal — so editor chords (e.g. `e`, `d`, Ctrl+E) reach the form
    /// instead of firing a global binding. Focus navigation (Ctrl+L/H) and quit
    /// still pass through to the global handler so you can move between panes.
    /// Returns `true` if consumed.
    pub(crate) fn handle_automation_pane_capture(
        &mut self,
        code: KeyCode,
        mods: KeyModifiers,
    ) -> bool {
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

    /// Handle keys while the automations pane is focused: navigate and drive the
    /// same toggle/run/edit/delete actions as the Ctrl+P modal. (Ctrl+N to
    /// create is handled globally in `dispatch_action`, so it works here too,
    /// including on an empty pane.)
    pub(crate) fn handle_automations_pane_key(&mut self, code: KeyCode) {
        let count = self.automation_ui.cached_automations.len();
        match code {
            // Creating works regardless of whether the pane has entries.
            KeyCode::Char('n') => self.new_automation_in_pane(),
            // `k`/Up at the top row (or empty pane) flows focus back up into the
            // session list; `j`/Down past the last loops to the top of it — so
            // the left column behaves as one circular vertical list.
            KeyCode::Char('k') | KeyCode::Up => self.automations_pane_move_up(count),
            KeyCode::Char('j') | KeyCode::Down => self.automations_pane_move_down(count),
            // `Enter`/`e` focuses the central-pane editor (like `Enter` on a
            // session focuses its terminal); on an empty pane it starts a new
            // automation.
            KeyCode::Enter | KeyCode::Char('e') => self.enter_automation_editor_in_pane(count),
            // Remaining nav/actions are no-ops on an empty pane.
            _ if count > 0 => self.dispatch_automation_pane_action(code, count),
            _ => {}
        }
    }

    /// `Enter`/`e` in the automations pane: open the editor for the selection,
    /// or start a new automation on an empty pane.
    fn enter_automation_editor_in_pane(&mut self, count: usize) {
        if count == 0 {
            self.new_automation_in_pane();
        } else {
            self.enter_automation_editor();
        }
        self.refresh_selected_automation_runs();
    }

    /// Clamp the selection and run the toggle/run/delete action for the row
    /// under the cursor (caller guarantees a non-empty pane).
    fn dispatch_automation_pane_action(&mut self, code: KeyCode, count: usize) {
        if self.automation_ui.automation_panel_index >= count {
            self.automation_ui.automation_panel_index = count - 1;
        }
        let id =
            self.automation_ui.cached_automations[self.automation_ui.automation_panel_index].id;
        self.handle_automation_pane_action(code, id);
    }

    /// `k`/Up in the automations pane: step up, or hand focus back to the
    /// session list (last row) at the top / when empty.
    fn automations_pane_move_up(&mut self, count: usize) {
        if self.automation_ui.automation_panel_index == 0 || count == 0 {
            self.focus = InputFocus::SessionList;
            self.select_last_session();
        } else {
            self.automation_ui.automation_panel_index -= 1;
        }
        self.refresh_automation_view();
    }

    /// `j`/Down in the automations pane: step down, or loop focus to the top of
    /// the session list past the last row / when empty.
    fn automations_pane_move_down(&mut self, count: usize) {
        if count == 0 || self.automation_ui.automation_panel_index + 1 >= count {
            self.focus = InputFocus::SessionList;
            self.select_first_session();
        } else {
            self.automation_ui.automation_panel_index += 1;
        }
        self.refresh_automation_view();
    }

    /// Toggle / run / delete the selected automation (`Space`/`r`/`d`).
    fn handle_automation_pane_action(&mut self, code: KeyCode, id: i64) {
        match code {
            KeyCode::Char(' ') => {
                self.toggle_automation_by_id(id);
                self.sync_automation_editor();
            }
            KeyCode::Char('r') => self.run_automation_by_id(id),
            KeyCode::Char('d') => {
                self.delete_automation_by_id(id);
                let new_count = self.automation_ui.cached_automations.len();
                if new_count > 0 && self.automation_ui.automation_panel_index >= new_count {
                    self.automation_ui.automation_panel_index = new_count - 1;
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
    pub(crate) fn handle_automation_editor_pane_key(&mut self, code: KeyCode, mods: KeyModifiers) {
        // No editor yet (e.g. focused an empty pane): allow create / leave only.
        let Some(editor) = self.automation_ui.automation_editor.as_mut() else {
            match code {
                KeyCode::Char('n') => self.new_automation_in_pane(),
                KeyCode::Esc => self.focus = InputFocus::Automations,
                _ => {}
            }
            return;
        };
        match editor.handle_key(code, mods) {
            modals::EditorOutcome::Continue => {}
            modals::EditorOutcome::Save => {
                let Some(editor) = self.automation_ui.automation_editor.clone() else {
                    return;
                };
                if self.save_automation(&editor) {
                    // A brand-new automation lands at the top of the list.
                    if editor.editing_id.is_none() {
                        self.automation_ui.automation_panel_index = 0;
                    }
                    self.focus = InputFocus::Automations;
                    self.refresh_automation_view();
                }
            }
            modals::EditorOutcome::Cancel => {
                // Discard edits and restore the preview for the selection.
                self.focus = InputFocus::Automations;
                self.refresh_automation_view();
            }
        }
    }

    /// Handle keys while the run-history panel is focused: `j`/`k` move the
    /// selected run, `r` triggers a fresh run of the scoped automation, and
    /// `Esc` returns to the editor.
    pub(crate) fn handle_automation_run_history_key(&mut self, code: KeyCode) {
        match code {
            KeyCode::Char('j') | KeyCode::Down => {
                let len = self.automation_ui.cached_automation_runs.len();
                if len > 0 && self.automation_ui.automation_run_index + 1 < len {
                    self.automation_ui.automation_run_index += 1;
                }
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.automation_ui.automation_run_index =
                    self.automation_ui.automation_run_index.saturating_sub(1);
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

    /// Drive the centered-overlay automation editor (the Ctrl+P list path).
    /// Field navigation is shared with the in-pane editor via
    /// [`AutomationEditorModal::handle_key`].
    pub(crate) fn handle_automation_editor_key(&mut self, code: KeyCode, mods: KeyModifiers) {
        let modals::Modal::AutomationEditor(ref mut m) = self.modal else {
            return;
        };
        match m.handle_key(code, mods) {
            modals::EditorOutcome::Continue => {}
            modals::EditorOutcome::Save => self.submit_automation_editor(),
            modals::EditorOutcome::Cancel => self.modal.close(),
        }
    }

    pub(crate) fn handle_automations_list_key(&mut self, code: KeyCode) {
        if self.handle_automations_list_nav(code) {
            return;
        }
        self.handle_automations_list_action(code);
    }

    /// Close/select navigation for the automations list modal. Returns `true`
    /// when the key was consumed as navigation.
    fn handle_automations_list_nav(&mut self, code: KeyCode) -> bool {
        let modals::Modal::AutomationsList(ref mut al) = self.modal else {
            return false;
        };
        match code {
            KeyCode::Esc => self.modal.close(),
            KeyCode::Char('j') | KeyCode::Down => {
                if al.index + 1 < al.entries.len() {
                    al.index += 1;
                }
            }
            KeyCode::Char('k') | KeyCode::Up => {
                al.index = al.index.saturating_sub(1);
            }
            _ => return false,
        }
        true
    }

    /// Action keys (new/edit/toggle/run/delete) for the automations list modal.
    fn handle_automations_list_action(&mut self, code: KeyCode) {
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
        let modals::Modal::AutomationsList(ref al) = self.modal else {
            return None;
        };
        al.entries.get(al.index).map(|e| e.id)
    }

    /// Rebuild the list modal entries after a mutation, preserving selection.
    fn refresh_automations_list_modal(&mut self) {
        let index = match self.modal {
            modals::Modal::AutomationsList(ref al) => al.index,
            _ => return,
        };
        self.open_automations_list();
        if let modals::Modal::AutomationsList(ref mut al) = self.modal {
            al.index = index.min(al.entries.len().saturating_sub(1));
            if al.entries.is_empty() {
                self.modal.close();
            }
        }
    }
}

/// One-line summary of an automation for the list modal:
/// `<schedule> · <action> · <when>`.
pub(crate) fn format_automation_summary(auto: &Automation, now: u64) -> String {
    let schedule = match &auto.schedule {
        AutomationSchedule::Once { .. } => "once".to_string(),
        // Show a human-readable schedule for preset cron shapes; fall back to the
        // raw expression for power-user crons that don't map to a preset.
        AutomationSchedule::Cron { expr } => {
            modals::humanize_cron(expr).unwrap_or_else(|| expr.clone())
        }
    };
    let action = auto.action.kind();
    let when = if !auto.enabled {
        "disabled".to_string()
    } else if let Some(next) = auto.next_run_at {
        // `format_countdown` already includes the "in " prefix.
        view::format_countdown(next.saturating_sub(now))
    } else {
        "—".to_string()
    };
    format!("{schedule} · {action} · {when}")
}
