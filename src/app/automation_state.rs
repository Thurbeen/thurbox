//! Automations-pane UI state (the focusable list beneath the session list).
//!
//! Grouped out of the [`App`](super::App) god object. Fields are `pub(crate)`
//! so call-sites keep direct access (`self.automation_ui.cached_automations`).

use super::modals;
use crate::session::{Automation, AutomationRun};

/// UI state backing the automations pane: the cached list, panel selection, the
/// scoped run-history cache, the live preview/edit editor, and the run-history
/// selection.
#[derive(Default)]
pub(crate) struct AutomationUiState {
    /// Cached automations for the UI, refreshed every ~1 second.
    pub(crate) cached_automations: Vec<Automation>,
    /// Selected row in the focusable automations pane.
    pub(crate) automation_panel_index: usize,
    /// Run history for the currently scoped automation, shown in the central
    /// pane. Refreshed when the automations pane is focused / its selection
    /// changes (see
    /// [`App::refresh_selected_automation_runs`](super::App::refresh_selected_automation_runs)).
    pub(crate) cached_automation_runs: Vec<AutomationRun>,
    /// Which automation `cached_automation_runs` belongs to, so the cache can be
    /// invalidated when the selection moves.
    pub(crate) cached_automation_runs_id: Option<i64>,
    /// The editor for the automation currently scoped in the central pane. While
    /// the automations pane is focused this mirrors the selected automation (a
    /// live preview); while
    /// [`InputFocus::AutomationEditor`](super::InputFocus::AutomationEditor) is
    /// focused it holds the in-progress edits. `None` when no automation is
    /// scoped.
    pub(crate) automation_editor: Option<modals::AutomationEditorModal>,
    /// Selected row in the run-history panel while
    /// [`InputFocus::AutomationRunHistory`](super::InputFocus::AutomationRunHistory)
    /// is focused. Indexes [`Self::cached_automation_runs`].
    pub(crate) automation_run_index: usize,
}
