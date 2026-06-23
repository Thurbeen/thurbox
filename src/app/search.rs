//! Global search — a non-modal bottom strip (`Ctrl+/` by default) that
//! searches across every scope at once: session metadata + live buffer
//! **content**, automation names, task titles, and the active session's file
//! tree.
//!
//! The state lives here; building results and dispatching a selection live on
//! `App` (they touch `self.sessions`/vt100/caches). The renderer is
//! [`crate::ui::global_search`].

use std::path::PathBuf;
use std::time::Instant;

use super::modals::TextInput;
use super::InputFocus;

/// Max results kept per group (sessions/tasks/automations/files), so a broad
/// query can't flood the strip.
pub(crate) const MAX_PER_GROUP: usize = 8;

/// How many trailing lines of a session's buffer the content scan inspects.
pub(crate) const CONTENT_LINE_CAP: usize = 500;

/// Debounce before the expensive session-content scan runs after a keystroke.
pub(crate) const CONTENT_DEBOUNCE_MS: u64 = 150;

/// What a result jumps to when activated with `Enter`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SearchTarget {
    /// Switch to this session (index into `App::sessions`) and focus its terminal.
    Session { index: usize },
    /// Focus the tasks panel and select this task.
    Task { id: i64 },
    /// Focus the automations pane and select this automation.
    Automation { id: i64 },
    /// Open the file viewer on this path.
    File { root: PathBuf, path: PathBuf },
}

/// The scope a result belongs to (drives grouping + the group header).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SearchKind {
    Session,
    Task,
    Automation,
    File,
}

/// A single match shown in the strip.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GlobalSearchResult {
    pub kind: SearchKind,
    /// Primary display text (session name, task title, file label, …).
    pub label: String,
    /// Matching line for content matches, shown dimmed beneath the label.
    pub snippet: Option<String>,
    pub target: SearchTarget,
}

/// Snapshot of the UI state taken when the strip opens, so cancelling (`Esc`)
/// restores exactly what the user had before searching — including selections,
/// focus, and which optional panels were visible. Live result previews mutate
/// these same fields, so without the snapshot a cancel would leave the cursor
/// wherever the last preview moved it.
#[derive(Clone)]
pub(crate) struct SearchSnapshot {
    pub focus: InputFocus,
    pub active_index: usize,
    pub task_panel_index: usize,
    pub automation_panel_index: usize,
    pub show_tasks_panel: bool,
    pub show_file_viewer: bool,
}

/// State for the global-search strip.
pub(crate) struct GlobalSearchState {
    pub active: bool,
    pub query: TextInput,
    pub results: Vec<GlobalSearchResult>,
    /// Selected flat index into `results`.
    pub selected: usize,
    /// UI state captured at open time (incl. the focus to restore), applied on
    /// cancel.
    pub snapshot: Option<SearchSnapshot>,
    /// When the query last changed — anchors the content-scan debounce.
    pub query_changed_at: Option<Instant>,
    /// A content scan is pending (set on edit, cleared once it runs).
    pub content_dirty: bool,
}

impl Default for GlobalSearchState {
    fn default() -> Self {
        Self {
            active: false,
            query: TextInput::new(),
            results: Vec::new(),
            selected: 0,
            snapshot: None,
            query_changed_at: None,
            content_dirty: false,
        }
    }
}

impl GlobalSearchState {
    /// Clamp `selected` into the current result range.
    pub(crate) fn clamp_selection(&mut self) {
        if self.results.is_empty() {
            self.selected = 0;
        } else if self.selected >= self.results.len() {
            self.selected = self.results.len() - 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn result(label: &str) -> GlobalSearchResult {
        GlobalSearchResult {
            kind: SearchKind::Task,
            label: label.to_string(),
            snippet: None,
            target: SearchTarget::Task { id: 1 },
        }
    }

    fn state_with(results: usize, selected: usize) -> GlobalSearchState {
        GlobalSearchState {
            results: (0..results).map(|i| result(&format!("r{i}"))).collect(),
            selected,
            ..GlobalSearchState::default()
        }
    }

    #[test]
    fn clamp_resets_to_zero_when_empty() {
        let mut s = state_with(0, 5);
        s.clamp_selection();
        assert_eq!(s.selected, 0);
    }

    #[test]
    fn clamp_pins_to_last_when_out_of_range() {
        let mut s = state_with(3, 9);
        s.clamp_selection();
        assert_eq!(s.selected, 2);
    }

    #[test]
    fn clamp_leaves_in_range_selection_untouched() {
        let mut s = state_with(3, 1);
        s.clamp_selection();
        assert_eq!(s.selected, 1);
    }
}
