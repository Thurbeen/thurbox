//! Global search — a non-modal bottom strip (`Ctrl+/` by default) that
//! searches across every scope at once: session metadata + live buffer
//! **content**, automation names, task titles, and the active session's file
//! tree.
//!
//! The state lives here; building results and dispatching a selection live on
//! `App` (they touch `self.sessions`/vt100/caches). The renderer is
//! [`crate::ui::global_search`].

use std::path::PathBuf;
use std::time::{Duration, Instant};

use super::modals::TextInput;
use super::{clock, App, InputFocus};
use crossterm::event::{KeyCode, KeyModifiers};

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

impl App {
    // ---- Global search (Ctrl+/ bottom strip) -----------------------------

    /// Open the global-search strip: snapshot the current UI state (so cancel
    /// can restore it), clear the query, focus the strip, and seed the (cheap)
    /// metadata results.
    pub(crate) fn open_global_search(&mut self) {
        self.global_search.snapshot = Some(SearchSnapshot {
            focus: self.focus,
            active_index: self.active_index,
            task_panel_index: self.task_ui.task_panel_index,
            automation_panel_index: self.automation_ui.automation_panel_index,
            show_tasks_panel: self.show_tasks_panel,
            show_file_viewer: self.show_file_viewer,
        });
        self.global_search.active = true;
        self.global_search.query.clear();
        self.global_search.results.clear();
        self.global_search.selected = 0;
        self.global_search.query_changed_at = None;
        self.global_search.content_dirty = false;
        self.focus = InputFocus::GlobalSearch;
        self.recompute_global_search_metadata();
        self.resize_sessions_to_content_area();
    }

    /// Cancel the strip: restore the exact UI state captured at open time
    /// (selections, focus, and panel visibility the live preview may have
    /// changed). Bound to `Esc`.
    pub(crate) fn close_global_search(&mut self) {
        if let Some(snap) = self.global_search.snapshot.take() {
            self.active_index = snap.active_index.min(self.sessions.len().saturating_sub(1));
            self.task_ui.task_panel_index = snap.task_panel_index;
            self.automation_ui.automation_panel_index = snap.automation_panel_index;
            self.show_tasks_panel = snap.show_tasks_panel;
            self.show_file_viewer = snap.show_file_viewer;
            self.focus = snap.focus;
        }
        self.global_search.active = false;
        self.global_search.results.clear();
        self.global_search.query.clear();
        // The snapshot predates any feature flag flipped while the strip was
        // open (settings live-reload): re-enforce so the restore can't
        // resurrect a panel/focus whose feature was just disabled. Runs after
        // `active = false`, so its own close-search branch is a no-op.
        self.enforce_feature_visibility();
        self.resize_sessions_to_content_area();
    }

    /// Note that the query changed: recompute the cheap metadata results now,
    /// live-preview the new top result, and flag the expensive content scan to
    /// run after the debounce settles.
    pub(crate) fn on_global_search_query_changed(&mut self) {
        self.recompute_global_search_metadata();
        self.preview_global_search_result();
        self.global_search.content_dirty = true;
        self.global_search.query_changed_at = Some(clock::now());
    }

    /// Rebuild the metadata results (sessions/tasks/automations/files) — fast
    /// enough to run on every keystroke. Session buffer **content** matches are
    /// added separately by [`Self::recompute_global_search_content`].
    pub(crate) fn recompute_global_search_metadata(&mut self) {
        let query = self.global_search.query.value().to_string();
        let results = self.build_global_search_results(&query, false);
        self.global_search.results = results;
        self.global_search.clamp_selection();
    }

    /// Rebuild results including the debounced per-session buffer content scan.
    pub(crate) fn recompute_global_search_content(&mut self) {
        let query = self.global_search.query.value().to_string();
        let results = self.build_global_search_results(&query, true);
        self.global_search.results = results;
        self.global_search.clamp_selection();
        // The result set may have grown (content matches) — keep the preview in
        // sync with whatever is now selected.
        self.preview_global_search_result();
    }

    /// Assemble the grouped result list. `with_content` adds session buffer
    /// matches (the heavy path). Empty query → no results.
    fn build_global_search_results(
        &self,
        query: &str,
        with_content: bool,
    ) -> Vec<GlobalSearchResult> {
        if query.trim().is_empty() {
            return Vec::new();
        }
        let query_lc = query.to_lowercase();
        // Group order: Sessions → Tasks → Automations → Files. Disabled
        // features contribute no results, so a selection can never preview or
        // jump into a pane the feature flags hide.
        let mut out = self.search_sessions(query, &query_lc, with_content);
        if self.features.tasks {
            out.extend(self.search_tasks(query, &query_lc));
        }
        if self.features.automations {
            out.extend(self.search_automations(query));
        }
        if self.features.file_viewer {
            out.extend(self.search_files(&query_lc));
        }
        out
    }

    /// Session results: fuzzy metadata (name / agent / branch) plus, on the
    /// debounced heavy path, a buffer-content scan (skipping metadata matches).
    fn search_sessions(
        &self,
        query: &str,
        query_lc: &str,
        with_content: bool,
    ) -> Vec<GlobalSearchResult> {
        let mut sessions: Vec<GlobalSearchResult> = Vec::new();
        for (i, session) in self.sessions.iter().enumerate() {
            if sessions.len() >= MAX_PER_GROUP {
                break;
            }
            let info = &session.info;
            let branch = info.worktrees.first().map(|w| w.branch.as_str());
            let meta_hit = crate::fuzzy::fuzzy_match(query, &info.name).is_some()
                || crate::fuzzy::fuzzy_match(query, &info.agent).is_some()
                || branch.is_some_and(|b| crate::fuzzy::fuzzy_match(query, b).is_some());
            if meta_hit {
                sessions.push(GlobalSearchResult {
                    kind: SearchKind::Session,
                    label: info.name.clone(),
                    snippet: None,
                    target: SearchTarget::Session { index: i },
                });
            }
        }
        if with_content {
            self.push_session_content_matches(query_lc, &mut sessions);
        }
        sessions
    }

    /// Append vt100 buffer-content matches to `out`, skipping sessions already
    /// present (matched on metadata) and respecting the per-group cap.
    fn push_session_content_matches(&self, query_lc: &str, out: &mut Vec<GlobalSearchResult>) {
        let already: std::collections::HashSet<usize> = out
            .iter()
            .filter_map(|r| match r.target {
                SearchTarget::Session { index } => Some(index),
                _ => None,
            })
            .collect();
        for i in 0..self.sessions.len() {
            if out.len() >= MAX_PER_GROUP {
                break;
            }
            if already.contains(&i) {
                continue;
            }
            if let Some(snippet) = self.session_content_match(query_lc, i) {
                out.push(GlobalSearchResult {
                    kind: SearchKind::Session,
                    label: self.sessions[i].info.name.clone(),
                    snippet: Some(snippet),
                    target: SearchTarget::Session { index: i },
                });
            }
        }
    }

    /// Task results: fuzzy title, falling back to a fuzzy description match with
    /// a context snippet.
    fn search_tasks(&self, query: &str, query_lc: &str) -> Vec<GlobalSearchResult> {
        let mut tasks: Vec<GlobalSearchResult> = Vec::new();
        for task in &self.task_ui.cached_tasks {
            if tasks.len() >= MAX_PER_GROUP {
                break;
            }
            let title_hit = crate::fuzzy::fuzzy_match(query, &task.title).is_some();
            // Title missed — match the description with the same fuzzy matcher
            // used for titles (so gapped queries hit too).
            let desc_hit = !title_hit
                && task
                    .description
                    .as_deref()
                    .is_some_and(|d| crate::fuzzy::fuzzy_match(query, d).is_some());
            if !title_hit && !desc_hit {
                continue;
            }
            // Snippet (description hits only): prefer a line containing the query
            // verbatim, else the first non-empty line, for useful context.
            let snippet = desc_hit.then(|| {
                let desc = task.description.as_deref().unwrap_or("");
                desc.lines()
                    .find(|l| l.to_lowercase().contains(query_lc))
                    .or_else(|| desc.lines().find(|l| !l.trim().is_empty()))
                    .map(|l| l.trim().chars().take(120).collect::<String>())
                    .unwrap_or_default()
            });
            tasks.push(GlobalSearchResult {
                kind: SearchKind::Task,
                label: task.title.clone(),
                snippet,
                target: SearchTarget::Task { id: task.id },
            });
        }
        tasks
    }

    /// Automation results: fuzzy name.
    fn search_automations(&self, query: &str) -> Vec<GlobalSearchResult> {
        let mut automations: Vec<GlobalSearchResult> = Vec::new();
        for auto in &self.automation_ui.cached_automations {
            if automations.len() >= MAX_PER_GROUP {
                break;
            }
            if crate::fuzzy::fuzzy_match(query, &auto.name).is_some() {
                automations.push(GlobalSearchResult {
                    kind: SearchKind::Automation,
                    label: auto.name.clone(),
                    snippet: None,
                    target: SearchTarget::Automation { id: auto.id },
                });
            }
        }
        automations
    }

    /// File results: case-insensitive substring over the active session's tree.
    fn search_files(&self, query_lc: &str) -> Vec<GlobalSearchResult> {
        let mut files: Vec<GlobalSearchResult> = Vec::new();
        let Some(info) = self.sessions.get(self.active_index).map(|s| &s.info) else {
            return files;
        };
        for (root, path, name) in crate::ui::file_viewer::enumerate_paths(info) {
            if files.len() >= MAX_PER_GROUP {
                break;
            }
            if name.to_lowercase().contains(query_lc) {
                files.push(GlobalSearchResult {
                    kind: SearchKind::File,
                    label: name,
                    snippet: None,
                    target: SearchTarget::File { root, path },
                });
            }
        }
        files
    }

    /// Search a session's visible buffer for `query_lc`, returning the first
    /// matching (trimmed) line as a snippet. Scans only the last
    /// [`CONTENT_LINE_CAP`] lines and tolerates a poisoned lock.
    pub(super) fn session_content_match(&self, query_lc: &str, idx: usize) -> Option<String> {
        if query_lc.trim().is_empty() {
            return None;
        }
        let session = self.sessions.get(idx)?;
        let parser = session.parser.lock().ok()?;
        let contents = parser.screen().contents();
        drop(parser);
        let lines: Vec<&str> = contents.lines().collect();
        let start = lines.len().saturating_sub(CONTENT_LINE_CAP);
        for line in &lines[start..] {
            if line.to_lowercase().contains(query_lc) {
                let trimmed = line.trim();
                if !trimmed.is_empty() {
                    return Some(trimmed.chars().take(120).collect());
                }
            }
        }
        None
    }

    /// The active global-search query for live in-panel highlighting: `Some`
    /// when the strip is open with a non-empty query, else `None` (panels render
    /// normally). Used by the view to highlight matched rows and dim the rest.
    pub(crate) fn global_search_query(&self) -> Option<&str> {
        if !self.global_search.active {
            return None;
        }
        let q = self.global_search.query.value();
        (!q.trim().is_empty()).then_some(q)
    }

    /// The scope of the currently selected global-search result, while the strip
    /// is active. Lets the view force-show the selected (previewed) row in the
    /// owning panel even though focus stays in the search box.
    pub(crate) fn global_search_preview_kind(&self) -> Option<SearchKind> {
        if !self.global_search.active {
            return None;
        }
        self.global_search
            .results
            .get(self.global_search.selected)
            .map(|r| r.kind)
    }

    /// Live-preview the selected result without leaving the search box: move the
    /// matching panel's cursor (active session / task row / automation row) so
    /// the user sees where `Enter` would land. Files are *not* previewed (opening
    /// the file viewer per keystroke is heavy) — they only act on `Enter`.
    /// Cancelling restores all of this from the snapshot.
    pub(crate) fn preview_global_search_result(&mut self) {
        let Some(result) = self
            .global_search
            .results
            .get(self.global_search.selected)
            .cloned()
        else {
            return;
        };
        match result.target {
            SearchTarget::Session { index } => {
                if index < self.sessions.len() {
                    self.active_index = index;
                }
            }
            SearchTarget::Task { id } => {
                self.show_tasks_panel = true;
                self.refresh_tasks();
                if let Some(pos) = self
                    .task_ui
                    .filtered_task_indices
                    .iter()
                    .position(|&i| self.task_ui.cached_tasks.get(i).map(|t| t.id) == Some(id))
                {
                    self.task_ui.task_panel_index = pos;
                }
            }
            SearchTarget::Automation { id } => {
                if let Some(pos) = self
                    .automation_ui
                    .cached_automations
                    .iter()
                    .position(|a| a.id == id)
                {
                    self.automation_ui.automation_panel_index = pos;
                }
            }
            // Files aren't previewed live — they only open on `Enter`.
            SearchTarget::File { .. } => {}
        }
    }

    /// Jump to the selected search result's target, then close the strip.
    pub(crate) fn activate_global_search_result(&mut self) {
        let Some(result) = self
            .global_search
            .results
            .get(self.global_search.selected)
            .cloned()
        else {
            self.close_global_search();
            return;
        };
        // Commit: discard the snapshot (we keep the jump, don't restore) and
        // tear the strip down, then apply the jump target. Capture the pre-search
        // focus first, as the fallback when a stale target can't be opened.
        let fallback_focus = self
            .global_search
            .snapshot
            .as_ref()
            .map(|s| s.focus)
            .unwrap_or(InputFocus::SessionList);
        self.global_search.active = false;
        self.global_search.results.clear();
        self.global_search.query.clear();
        self.global_search.snapshot = None;
        match result.target {
            SearchTarget::Session { index } => {
                if index < self.sessions.len() {
                    self.active_index = index;
                    self.focus = InputFocus::Terminal;
                } else {
                    self.focus = fallback_focus;
                }
            }
            SearchTarget::Task { id } => {
                self.show_tasks_panel = true;
                self.refresh_tasks();
                if let Some(pos) = self
                    .task_ui
                    .filtered_task_indices
                    .iter()
                    .position(|&i| self.task_ui.cached_tasks.get(i).map(|t| t.id) == Some(id))
                {
                    self.task_ui.task_panel_index = pos;
                }
                self.focus = InputFocus::TaskList;
            }
            SearchTarget::Automation { id } => {
                if let Some(pos) = self
                    .automation_ui
                    .cached_automations
                    .iter()
                    .position(|a| a.id == id)
                {
                    self.automation_ui.automation_panel_index = pos;
                }
                self.focus = InputFocus::Automations;
                self.refresh_automation_view();
            }
            SearchTarget::File { root: _, path } => {
                self.show_file_viewer = true;
                self.rebuild_file_viewer_for_active();
                self.file_viewer.reveal_path(&path);
                self.focus = InputFocus::FileViewer;
            }
        }
        self.resize_sessions_to_content_area();
    }

    /// Run the debounced global-search content scan once the query has been
    /// settled for the debounce window (Instant-based, since tick cadence is
    /// event-load-dependent).
    pub(super) fn tick_global_search_content(&mut self) {
        if !(self.global_search.active && self.global_search.content_dirty) {
            return;
        }
        let settled = self
            .global_search
            .query_changed_at
            .map(|t| clock::elapsed_since(t) >= Duration::from_millis(CONTENT_DEBOUNCE_MS))
            .unwrap_or(false);
        if settled {
            self.recompute_global_search_content();
            self.global_search.content_dirty = false;
        }
    }

    /// Handle keys while the global-search strip is focused. Typed characters
    /// edit the query (so plain `j`/`k` insert, like the other search inputs);
    /// `Up`/`Down` and `Ctrl+P`/`Ctrl+N` move the selection; `Enter` activates
    /// the selected result; `Esc` closes the strip.
    pub(super) fn handle_global_search_key(&mut self, code: KeyCode, mods: KeyModifiers) {
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
