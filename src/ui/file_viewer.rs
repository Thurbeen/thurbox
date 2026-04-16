//! Right-side file viewer panel.
//!
//! Displays an expandable tree of every worktree and additional directory
//! associated with the active session. Selection is flat (one visible row
//! at a time), and callers drive navigation through [`FileViewerState`]
//! helpers. File I/O (reading directory entries) is performed lazily when a
//! folder is expanded.

use std::path::{Path, PathBuf};

use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use super::{focus_block, theme::Theme, FocusLevel};
use crate::session::SessionInfo;

/// One node in the tree. `children = None` means "not yet expanded"; an
/// empty `Some(vec![])` means "expanded but empty".
pub struct FileNode {
    pub path: PathBuf,
    pub is_dir: bool,
    pub expanded: bool,
    pub children: Option<Vec<FileNode>>,
}

impl FileNode {
    fn new_dir(path: PathBuf) -> Self {
        Self {
            path,
            is_dir: true,
            expanded: false,
            children: None,
        }
    }
}

/// Paths that should appear as roots for the given session: every worktree,
/// then every additional dir, falling back to `cwd` if both are empty.
fn expected_root_paths(info: &SessionInfo) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = info
        .worktrees
        .iter()
        .map(|w| w.worktree_path.clone())
        .chain(info.additional_dirs.iter().cloned())
        .collect();
    if out.is_empty() {
        if let Some(cwd) = &info.cwd {
            out.push(cwd.clone());
        }
    }
    out
}

/// Result of activating the currently-selected row.
pub enum Activation {
    /// A file was activated — caller should open it in the editor.
    Open(PathBuf),
    /// A directory was toggled (expanded or collapsed).
    Toggled,
    /// Nothing was done (empty tree, out-of-bounds, etc.).
    NoOp,
}

/// One flattened visible row. Depth drives indentation; `index_path` is the
/// traversal path into `roots` (sequence of child indices).
struct FlatRow {
    index_path: Vec<usize>,
    depth: usize,
    label: String,
    is_dir: bool,
    expanded: bool,
}

pub struct FileViewerState {
    roots: Vec<FileNode>,
    selected: usize,
    pub search_active: bool,
    pub search_query: String,
    pub search_cursor: usize,
}

/// Maximum nodes traversed per search to keep typing responsive.
const SEARCH_NODE_LIMIT: usize = 5000;
/// Maximum directory depth traversed per search.
const SEARCH_DEPTH_LIMIT: usize = 6;

impl FileViewerState {
    pub fn new() -> Self {
        Self {
            roots: Vec::new(),
            selected: 0,
            search_active: false,
            search_query: String::new(),
            search_cursor: 0,
        }
    }

    /// Enter search mode. Preserves current selection.
    pub fn start_search(&mut self) {
        self.search_active = true;
        self.search_query.clear();
        self.search_cursor = 0;
    }

    /// Exit search mode, keeping selection.
    pub fn end_search(&mut self) {
        self.search_active = false;
        self.search_query.clear();
        self.search_cursor = 0;
    }

    /// Append a char to the query and jump selection to the first match.
    pub fn search_push(&mut self, c: char) {
        self.search_query.push(c);
        self.search_cursor = self.search_query.chars().count();
        self.expand_for_search();
        self.jump_to_first_match();
    }

    /// Backspace in the query.
    pub fn search_pop(&mut self) {
        self.search_query.pop();
        self.search_cursor = self.search_query.chars().count();
        if !self.search_query.is_empty() {
            self.expand_for_search();
        }
        self.jump_to_first_match();
    }

    /// Count flat rows currently matching the query.
    pub fn match_count(&self) -> usize {
        if self.search_query.is_empty() {
            return 0;
        }
        let q = self.search_query.to_lowercase();
        self.flatten()
            .iter()
            .filter(|r| r.label.to_lowercase().contains(&q))
            .count()
    }

    /// 1-based index of the currently selected row among matches, or 0 if
    /// selection is not on a match or query is empty.
    pub fn current_match_index(&self) -> usize {
        if self.search_query.is_empty() {
            return 0;
        }
        let q = self.search_query.to_lowercase();
        let rows = self.flatten();
        let mut idx = 0;
        for (i, row) in rows.iter().enumerate() {
            if row.label.to_lowercase().contains(&q) {
                idx += 1;
                if i == self.selected {
                    return idx;
                }
            }
        }
        0
    }

    /// Walk roots (bounded) and auto-expand ancestors of any node whose name
    /// matches the current query. Reads directories lazily.
    fn expand_for_search(&mut self) {
        if self.search_query.is_empty() {
            return;
        }
        let q = self.search_query.to_lowercase();
        let mut budget = SEARCH_NODE_LIMIT;
        for root in &mut self.roots {
            expand_matches(root, &q, 0, &mut budget);
            if budget == 0 {
                break;
            }
        }
    }

    /// Cycle to the next match (wrapping), starting after the current selection.
    pub fn next_match(&mut self) {
        self.step_match(true);
    }

    /// Cycle to the previous match (wrapping), starting before the current selection.
    pub fn prev_match(&mut self) {
        self.step_match(false);
    }

    fn step_match(&mut self, forward: bool) {
        if self.search_query.is_empty() {
            return;
        }
        let rows = self.flatten();
        if rows.is_empty() {
            return;
        }
        let q = self.search_query.to_lowercase();
        let n = rows.len();
        let start = self.selected;
        for offset in 1..=n {
            let i = if forward {
                (start + offset) % n
            } else {
                (start + n - offset) % n
            };
            if rows[i].label.to_lowercase().contains(&q) {
                self.selected = i;
                return;
            }
        }
    }

    fn jump_to_first_match(&mut self) {
        if self.search_query.is_empty() {
            return;
        }
        let rows = self.flatten();
        let q = self.search_query.to_lowercase();
        if let Some((i, _)) = rows
            .iter()
            .enumerate()
            .find(|(_, r)| r.label.to_lowercase().contains(&q))
        {
            self.selected = i;
        }
    }

    /// Return the selected file path along with its root worktree path.
    /// Returns `None` if selection is a directory or out of bounds.
    pub fn selected_file_with_root(&self) -> Option<(std::path::PathBuf, std::path::PathBuf)> {
        let rows = self.flatten();
        let row = rows.get(self.selected)?;
        let index_path = &row.index_path;
        let root_idx = *index_path.first()?;
        let root = self.roots.get(root_idx)?;
        let mut node = root;
        for idx in &index_path[1..] {
            node = node.children.as_ref()?.get(*idx)?;
        }
        if node.is_dir {
            return None;
        }
        Some((node.path.clone(), root.path.clone()))
    }

    /// Rebuild roots from the active session's worktrees + additional_dirs.
    /// Selection is reset to 0.
    pub fn rebuild_from_session(&mut self, info: &SessionInfo) {
        self.roots = expected_root_paths(info)
            .into_iter()
            .map(FileNode::new_dir)
            .collect();
        self.selected = 0;
    }

    pub fn clear(&mut self) {
        self.roots.clear();
        self.selected = 0;
    }

    /// Return true if the current roots don't match the session's expected roots.
    /// Used by the render layer to rebuild lazily when the active session changes.
    pub fn needs_rebuild_for(&self, info: &SessionInfo) -> bool {
        let expected = expected_root_paths(info);
        if self.roots.len() != expected.len() {
            return true;
        }
        self.roots
            .iter()
            .zip(expected.iter())
            .any(|(root, exp)| root.path != *exp)
    }

    fn flatten(&self) -> Vec<FlatRow> {
        let mut out = Vec::new();
        for (i, root) in self.roots.iter().enumerate() {
            push_flat(root, vec![i], 0, &mut out, true);
        }
        out
    }

    pub fn move_selection(&mut self, delta: i32) {
        let len = self.flatten().len();
        if len == 0 {
            self.selected = 0;
            return;
        }
        let next = (self.selected as i32 + delta).clamp(0, len as i32 - 1);
        self.selected = next as usize;
    }

    /// Activate the current row: toggle directory expansion or return a file to open.
    pub fn activate(&mut self) -> Activation {
        let rows = self.flatten();
        let Some(row) = rows.get(self.selected) else {
            return Activation::NoOp;
        };
        let index_path = row.index_path.clone();
        let Some(node) = traverse_mut(&mut self.roots, &index_path) else {
            return Activation::NoOp;
        };
        if node.is_dir {
            if node.expanded {
                node.expanded = false;
            } else {
                if node.children.is_none() {
                    node.children = Some(read_dir_sorted(&node.path));
                }
                node.expanded = true;
            }
            Activation::Toggled
        } else {
            Activation::Open(node.path.clone())
        }
    }

    /// Collapse the selected directory (or jump up to parent if selection is a file or a closed dir).
    pub fn collapse(&mut self) {
        let rows = self.flatten();
        let Some(row) = rows.get(self.selected) else {
            return;
        };
        let index_path = row.index_path.clone();
        if let Some(node) = traverse_mut(&mut self.roots, &index_path) {
            if node.is_dir && node.expanded {
                node.expanded = false;
                return;
            }
        }
        // Else: move selection up one level (parent)
        if index_path.len() > 1 {
            let parent_path = &index_path[..index_path.len() - 1];
            let new_rows = self.flatten();
            if let Some((i, _)) = new_rows
                .iter()
                .enumerate()
                .find(|(_, r)| r.index_path.as_slice() == parent_path)
            {
                self.selected = i;
            }
        }
    }
}

impl Default for FileViewerState {
    fn default() -> Self {
        Self::new()
    }
}

fn push_flat(
    node: &FileNode,
    index_path: Vec<usize>,
    depth: usize,
    out: &mut Vec<FlatRow>,
    is_root: bool,
) {
    let label = if is_root {
        // Show a shortened path for roots (last 2 components)
        short_root_label(&node.path)
    } else {
        node.path
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| node.path.to_string_lossy().into_owned())
    };
    out.push(FlatRow {
        index_path: index_path.clone(),
        depth,
        label,
        is_dir: node.is_dir,
        expanded: node.expanded,
    });
    if node.is_dir && node.expanded {
        if let Some(children) = &node.children {
            for (i, child) in children.iter().enumerate() {
                let mut ip = index_path.clone();
                ip.push(i);
                push_flat(child, ip, depth + 1, out, false);
            }
        }
    }
}

fn traverse_mut<'a>(roots: &'a mut [FileNode], index_path: &[usize]) -> Option<&'a mut FileNode> {
    let (first, rest) = index_path.split_first()?;
    let mut node = roots.get_mut(*first)?;
    for idx in rest {
        let children = node.children.as_mut()?;
        node = children.get_mut(*idx)?;
    }
    Some(node)
}

/// Recursively walk `node` and auto-expand directories that contain any
/// descendant whose name matches `q_lc`. Returns true if a match was found at
/// or below `node`. Reads child directories on demand.
fn expand_matches(node: &mut FileNode, q_lc: &str, depth: usize, budget: &mut usize) -> bool {
    if *budget == 0 {
        return false;
    }
    *budget -= 1;

    let self_matches = node
        .path
        .file_name()
        .map(|n| n.to_string_lossy().to_lowercase().contains(q_lc))
        .unwrap_or(false);

    if !node.is_dir {
        return self_matches;
    }

    if depth >= SEARCH_DEPTH_LIMIT {
        return self_matches;
    }

    // Lazily load children on first search traversal.
    if node.children.is_none() {
        node.children = Some(read_dir_sorted(&node.path));
    }

    let mut child_match = false;
    if let Some(children) = node.children.as_mut() {
        for child in children.iter_mut() {
            if expand_matches(child, q_lc, depth + 1, budget) {
                child_match = true;
            }
            if *budget == 0 {
                break;
            }
        }
    }

    if child_match {
        node.expanded = true;
    }
    self_matches || child_match
}

fn short_root_label(path: &Path) -> String {
    path.file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string_lossy().into_owned())
}

fn read_dir_sorted(path: &Path) -> Vec<FileNode> {
    let mut entries: Vec<(PathBuf, bool, String)> = match std::fs::read_dir(path) {
        Ok(iter) => iter
            .filter_map(|e| e.ok())
            .filter_map(|e| {
                let name = e.file_name().to_string_lossy().into_owned();
                if name.starts_with('.') {
                    return None;
                }
                let is_dir = e.file_type().map(|t| t.is_dir()).unwrap_or(false);
                Some((e.path(), is_dir, name))
            })
            .collect(),
        Err(_) => return Vec::new(),
    };
    // Dirs first, then files; each sorted by name.
    entries.sort_by(|a, b| match (a.1, b.1) {
        (true, false) => std::cmp::Ordering::Less,
        (false, true) => std::cmp::Ordering::Greater,
        _ => a.2.to_lowercase().cmp(&b.2.to_lowercase()),
    });
    entries
        .into_iter()
        .map(|(p, is_dir, _)| FileNode {
            path: p,
            is_dir,
            expanded: false,
            children: None,
        })
        .collect()
}

pub fn render_file_viewer(
    frame: &mut Frame,
    area: Rect,
    state: &FileViewerState,
    focus: FocusLevel,
) {
    use ratatui::layout::{Constraint, Direction, Layout};

    let search_visible = state.search_active || !state.search_query.is_empty();
    let constraints: Vec<Constraint> = if search_visible {
        vec![Constraint::Min(0), Constraint::Length(3)]
    } else {
        vec![Constraint::Min(0)]
    };
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints(constraints)
        .split(area);
    let list_outer = chunks[0];
    let search_outer = if search_visible {
        Some(chunks[1])
    } else {
        None
    };

    let block = focus_block(" Files ", focus);
    let inner = block.inner(list_outer);
    frame.render_widget(block, list_outer);

    if let Some(sa) = search_outer {
        render_search_bar(
            frame,
            sa,
            &state.search_query,
            state.search_active,
            state.search_cursor,
            state.current_match_index(),
            state.match_count(),
        );
    }

    if inner.height == 0 || inner.width == 0 {
        return;
    }

    let list_area = inner;

    if state.roots.is_empty() {
        let p = Paragraph::new(Line::from(Span::styled(
            "No folders",
            Style::default().fg(Theme::text_muted()),
        )));
        frame.render_widget(p, list_area);
        return;
    }

    let rows = state.flatten();
    let height = list_area.height as usize;
    if height == 0 {
        return;
    }

    let query_lc = if state.search_active && !state.search_query.is_empty() {
        Some(state.search_query.to_lowercase())
    } else {
        None
    };

    let (start, end) = visible_window(rows.len(), state.selected, height);

    let lines: Vec<Line> = rows[start..end]
        .iter()
        .enumerate()
        .map(|(i, row)| build_row_line(row, start + i == state.selected, query_lc.as_deref()))
        .collect();
    frame.render_widget(Paragraph::new(lines), list_area);
}

fn row_marker(row: &FlatRow) -> &'static str {
    let nerd = super::theme::current().nerd_font_enabled;
    match (row.is_dir, row.expanded, nerd) {
        (true, true, false) => "▾ ",
        (true, false, false) => "▸ ",
        (false, _, false) => "  ",
        (true, true, true) => "\u{f07c} ",
        (true, false, true) => "\u{f07b} ",
        (false, _, true) => "\u{f15b} ",
    }
}

fn row_label_color(is_match: bool, is_dir: bool) -> ratatui::style::Color {
    if !is_match {
        Theme::text_muted()
    } else if is_dir {
        Theme::accent()
    } else {
        Theme::text_primary()
    }
}

fn build_row_line(row: &FlatRow, selected: bool, query_lc: Option<&str>) -> Line<'static> {
    let indent = "  ".repeat(row.depth);
    let marker = row_marker(row);
    let is_match = query_lc
        .map(|q| row.label.to_lowercase().contains(q))
        .unwrap_or(true);

    let label_style = if selected {
        Style::default()
            .bg(Theme::selection_bg())
            .fg(Theme::selection_fg())
            .add_modifier(Modifier::BOLD)
    } else {
        let mut s = Style::default().fg(row_label_color(is_match, row.is_dir));
        if row.is_dir && is_match {
            s = s.add_modifier(Modifier::BOLD);
        }
        s
    };
    let prefix_style = if selected {
        Style::default()
            .bg(Theme::selection_bg())
            .fg(Theme::selection_fg())
    } else {
        Style::default().fg(Theme::text_muted())
    };

    Line::from(vec![
        Span::styled(format!("{indent}{marker}"), prefix_style),
        Span::styled(row.label.clone(), label_style),
    ])
}

fn render_search_bar(
    frame: &mut Frame,
    area: Rect,
    query: &str,
    is_active: bool,
    cursor: usize,
    current: usize,
    total: usize,
) {
    use ratatui::widgets::{Block, Borders};

    let style = if is_active {
        Style::default().fg(Theme::search_bar())
    } else {
        Style::default().fg(Theme::text_muted())
    };

    let block = Block::default()
        .title(Line::from(Span::styled(
            search_title(query, current, total),
            style,
        )))
        .borders(Borders::ALL)
        .border_style(style);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let max_width = inner.width as usize;
    if max_width == 0 || inner.height == 0 {
        return;
    }

    let prefix = "/ ";
    let display_query = truncate_left(query, max_width.saturating_sub(prefix.len()));
    let (before, after) = split_at_cursor(display_query, cursor);

    let mut spans = vec![Span::styled(prefix, style), Span::styled(before, style)];
    append_cursor_spans(&mut spans, after, is_active, style);

    frame.render_widget(Paragraph::new(Line::from(spans)), inner);
}

fn search_title(query: &str, current: usize, total: usize) -> String {
    if query.is_empty() {
        " Search ".to_string()
    } else if total == 0 {
        " Search (no matches) ".to_string()
    } else {
        format!(" Search ({current}/{total}) ")
    }
}

fn truncate_left(query: &str, budget: usize) -> &str {
    if query.len() <= budget {
        query
    } else {
        &query[query.len().saturating_sub(budget)..]
    }
}

fn split_at_cursor(text: &str, cursor: usize) -> (&str, &str) {
    if cursor > text.chars().count() {
        return (text, "");
    }
    let byte_pos = text
        .char_indices()
        .nth(cursor)
        .map(|(i, _)| i)
        .unwrap_or(text.len());
    (&text[..byte_pos], &text[byte_pos..])
}

fn append_cursor_spans<'a>(
    spans: &mut Vec<Span<'a>>,
    after: &'a str,
    is_active: bool,
    style: Style,
) {
    if !is_active {
        spans.push(Span::styled(after, style));
        return;
    }
    let first_len = after.chars().next().map_or(0, |c| c.len_utf8());
    let cursor_char = if first_len == 0 {
        " "
    } else {
        &after[..first_len]
    };
    spans.push(Span::styled(cursor_char, Theme::cursor()));
    let rest = &after[first_len..];
    if !rest.is_empty() {
        spans.push(Span::styled(rest, style));
    }
}

fn visible_window(total: usize, selected: usize, height: usize) -> (usize, usize) {
    if total <= height {
        return (0, total);
    }
    // Center-ish: keep selected in view with a small margin.
    let margin = (height / 4).min(3);
    let start = selected.saturating_sub(margin);
    let start = start.min(total.saturating_sub(height));
    let end = (start + height).min(total);
    (start, end)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::{SessionInfo, WorktreeInfo};
    use std::path::PathBuf;

    fn sample_session() -> SessionInfo {
        let mut info = SessionInfo::new("t".into());
        info.worktrees.push(WorktreeInfo {
            repo_path: PathBuf::from("/tmp/a"),
            worktree_path: PathBuf::from("/tmp/a/wt"),
            branch: "main".into(),
        });
        info.additional_dirs.push(PathBuf::from("/tmp/b"));
        info
    }

    #[test]
    fn rebuild_from_session_collects_worktrees_and_additional_dirs() {
        let mut st = FileViewerState::new();
        st.rebuild_from_session(&sample_session());
        assert_eq!(st.roots.len(), 2);
        assert_eq!(st.roots[0].path, PathBuf::from("/tmp/a/wt"));
        assert_eq!(st.roots[1].path, PathBuf::from("/tmp/b"));
    }

    #[test]
    fn move_selection_is_bounded() {
        let mut st = FileViewerState::new();
        st.rebuild_from_session(&sample_session());
        st.move_selection(-5);
        assert_eq!(st.selected, 0);
        st.move_selection(100);
        assert_eq!(st.selected, 1);
    }

    #[test]
    fn activate_on_empty_is_noop() {
        let mut st = FileViewerState::new();
        assert!(matches!(st.activate(), Activation::NoOp));
    }

    #[test]
    fn activate_on_missing_dir_toggles() {
        // /nonexistent path: read_dir returns empty, but we still mark it expanded.
        let mut info = SessionInfo::new("t".into());
        info.additional_dirs
            .push(PathBuf::from("/this-path-does-not-exist-xyz"));
        let mut st = FileViewerState::new();
        st.rebuild_from_session(&info);
        match st.activate() {
            Activation::Toggled => {}
            _ => panic!("expected Toggled"),
        }
        assert!(st.roots[0].expanded);
    }

    #[test]
    fn visible_window_fits_all_when_shorter_than_height() {
        assert_eq!(visible_window(5, 0, 10), (0, 5));
    }

    #[test]
    fn visible_window_scrolls_when_overflow() {
        let (s, e) = visible_window(100, 50, 10);
        assert!(s <= 50 && e > 50);
        assert_eq!(e - s, 10);
    }

    #[test]
    fn expected_root_paths_falls_back_to_cwd_when_empty() {
        let mut info = SessionInfo::new("t".into());
        info.cwd = Some(PathBuf::from("/tmp/only-cwd"));
        let roots = expected_root_paths(&info);
        assert_eq!(roots, vec![PathBuf::from("/tmp/only-cwd")]);
    }

    #[test]
    fn expected_root_paths_ignores_cwd_when_worktrees_present() {
        let mut info = sample_session();
        info.cwd = Some(PathBuf::from("/tmp/cwd"));
        let roots = expected_root_paths(&info);
        assert_eq!(roots.len(), 2);
        assert!(!roots.contains(&PathBuf::from("/tmp/cwd")));
    }

    #[test]
    fn needs_rebuild_detects_root_changes() {
        let mut st = FileViewerState::new();
        st.rebuild_from_session(&sample_session());
        assert!(!st.needs_rebuild_for(&sample_session()));

        let mut other = SessionInfo::new("t".into());
        other.additional_dirs.push(PathBuf::from("/tmp/different"));
        assert!(st.needs_rebuild_for(&other));
    }

    #[test]
    fn search_push_updates_cursor_and_query() {
        let mut st = FileViewerState::new();
        st.rebuild_from_session(&sample_session());
        st.start_search();
        st.search_push('a');
        st.search_push('b');
        assert_eq!(st.search_query, "ab");
        assert_eq!(st.search_cursor, 2);
    }

    #[test]
    fn end_search_clears_query_and_cursor() {
        let mut st = FileViewerState::new();
        st.start_search();
        st.search_push('x');
        st.end_search();
        assert!(!st.search_active);
        assert_eq!(st.search_query, "");
        assert_eq!(st.search_cursor, 0);
    }

    #[test]
    fn match_count_and_current_index_on_flat_roots() {
        let mut info = SessionInfo::new("t".into());
        info.additional_dirs.push(PathBuf::from("/tmp/alpha"));
        info.additional_dirs.push(PathBuf::from("/tmp/beta"));
        info.additional_dirs.push(PathBuf::from("/tmp/alphabet"));
        let mut st = FileViewerState::new();
        st.rebuild_from_session(&info);
        st.start_search();
        st.search_push('a');
        st.search_push('l');
        // "alpha" and "alphabet" contain "al".
        assert_eq!(st.match_count(), 2);
        assert_eq!(st.current_match_index(), 1);
        st.next_match();
        assert_eq!(st.current_match_index(), 2);
        st.next_match();
        // Wraps back to first.
        assert_eq!(st.current_match_index(), 1);
        st.prev_match();
        assert_eq!(st.current_match_index(), 2);
    }

    #[test]
    fn next_match_noop_when_query_empty() {
        let mut st = FileViewerState::new();
        st.rebuild_from_session(&sample_session());
        let before = st.selected;
        st.next_match();
        assert_eq!(st.selected, before);
    }

    #[test]
    fn clear_resets_state() {
        let mut st = FileViewerState::new();
        st.rebuild_from_session(&sample_session());
        assert_eq!(st.roots.len(), 2);
        st.clear();
        assert_eq!(st.roots.len(), 0);
        assert_eq!(st.selected, 0);
    }
}
