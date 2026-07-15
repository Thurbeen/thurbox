use std::collections::HashSet;
use std::path::PathBuf;

use ratatui::{
    layout::{Constraint, Direction, Layout},
    style::Style,
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, Paragraph},
    Frame,
};

use super::render_modal_frame;
use super::theme::Theme;
use super::{centered_fixed_height_rect, render_text_field, render_text_field_with_suggestion};
use crate::app::modals::{BrowseEntry, RepoPickerFocus};

/// The path-browser dropdown's render view: the browsed dir, its
/// prefix-filtered entries, and the fetch state.
pub struct BrowserView<'a> {
    pub dir: &'a str,
    /// Already prefix-filtered by the app (indices resolved from
    /// `PathBrowser::filtered`).
    pub entries: Vec<&'a BrowseEntry>,
    pub index: usize,
    pub loading: bool,
    pub error: Option<&'a str>,
}

pub struct RepoPickerState<'a> {
    pub bookmarks: &'a [PathBuf],
    pub selected: &'a [bool],
    pub worktree: &'a [bool],
    /// Whether each row is a parent header (non-selectable group title).
    pub is_header: &'a [bool],
    /// Whether each row is a child repo nested under a parent (drives indentation).
    pub is_child: &'a [bool],
    /// Parent folders whose child tree is collapsed (drives the ▸/▾ glyph).
    pub collapsed: &'a HashSet<PathBuf>,
    pub list_index: usize,
    pub path_input: &'a str,
    pub path_cursor: usize,
    pub path_suggestion: Option<&'a str>,
    pub focus: RepoPickerFocus,
    pub search_query: &'a str,
    pub search_cursor: usize,
    pub search_active: bool,
    pub filtered_indices: &'a [usize],
    /// The target host's name for an off-local session (`None` = local).
    /// Shown in the list title so it's unambiguous whose filesystem the
    /// repos (and the typed path) belong to.
    pub host: Option<&'a str>,
    /// Per-row git-ness parallel to `bookmarks` (`Some(false)` renders a dim
    /// `(dir)` suffix — selectable, but not worktree-capable).
    pub is_git: &'a [Option<bool>],
    /// The path-browser dropdown (`None` = closed).
    pub browser: Option<BrowserView<'a>>,
    /// Whether an Enter-commit is being validated on the host (renders a
    /// spinner on the path-input label).
    pub pending_commit: bool,
    /// The current spinner frame (`ui::SPINNER_FRAMES[app.spinner_frame()]`).
    pub spinner: &'a str,
}

/// The clickable sub-areas of the repo picker that focus an editable field:
/// the always-present path input and the optional search bar.
pub struct RepoFocusAreas {
    pub input: ratatui::layout::Rect,
    pub search: Option<ratatui::layout::Rect>,
}

pub fn render_repo_picker_modal(
    frame: &mut Frame,
    state: &RepoPickerState<'_>,
) -> (super::ModalRender, RepoFocusAreas) {
    let visible_count = if state.filtered_indices.is_empty() {
        1
    } else {
        state.filtered_indices.len().min(10)
    };
    let list_height = visible_count as u16 + 2; // +2 for borders

    let search_height: u16 = if state.search_active { 3 } else { 0 };

    // Dropdown chunk under the path input: entry rows capped at 8, floored at
    // 1 (the loading/empty/error line), +2 borders.
    let browser_height: u16 = match &state.browser {
        Some(b) => b.entries.len().clamp(1, 8) as u16 + 2,
        None => 0,
    };

    // Layout: search(optional 3) + list + path input(3) + browser(optional)
    // + footer(1) + outer border(2)
    let total_height = search_height + list_height + 3 + browser_height + 1 + 2;

    let area = centered_fixed_height_rect(60, total_height, frame.area());

    let inner = render_modal_frame(frame, area, "Select Repos");

    let mut constraints = Vec::new();
    if state.search_active {
        constraints.push(Constraint::Length(3));
    }
    constraints.push(Constraint::Length(list_height));
    constraints.push(Constraint::Length(3));
    if state.browser.is_some() {
        constraints.push(Constraint::Length(browser_height));
    }
    constraints.push(Constraint::Min(1));

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints(constraints)
        .split(inner);

    let mut chunk = chunks.iter().copied();
    let search_area = state.search_active.then(|| chunk.next()).flatten();
    let list_area = chunk.next().unwrap_or_default();
    let input_area = chunk.next().unwrap_or_default();
    let browser_area = state.browser.is_some().then(|| chunk.next()).flatten();
    let footer_area = chunk.next().unwrap_or_default();

    if let Some(area) = search_area {
        render_search_bar(frame, area, state);
    }

    let hitboxes = render_bookmark_list(frame, list_area, state);

    let input_label = if state.pending_commit {
        format!("Add Repo Path {} checking…", state.spinner)
    } else {
        "Add Repo Path".to_string()
    };
    render_text_field_with_suggestion(
        frame,
        input_area,
        &input_label,
        state.path_input,
        state.path_cursor,
        state.focus == RepoPickerFocus::Input,
        state.path_suggestion,
    );

    if let (Some(browser), Some(area)) = (&state.browser, browser_area) {
        render_path_browser(frame, area, browser, state.spinner);
    }

    // Footer: focus-dependent key hints on the left, clickable `[ Done ]`
    // (Enter) / `[ Cancel ]` (Esc) buttons on the right. The hint is clipped to
    // the space left of the pills so a wide hint row can't render underneath
    // them (see `render_hint_action_footer`).
    let buttons = super::render_hint_action_footer(
        frame,
        footer_area,
        footer_line(state),
        (
            "Done",
            crossterm::event::KeyCode::Enter,
            crossterm::event::KeyModifiers::NONE,
        ),
        "Cancel",
    );
    (
        (hitboxes, buttons),
        RepoFocusAreas {
            input: input_area,
            search: search_area,
        },
    )
}

/// Render the search bar at the top of the modal (only shown when search is active).
fn render_search_bar(frame: &mut Frame, area: ratatui::layout::Rect, state: &RepoPickerState<'_>) {
    let match_label = format!(
        "Search ({}/{})",
        state.filtered_indices.len(),
        state.bookmarks.len()
    );
    render_text_field(
        frame,
        area,
        &match_label,
        state.search_query,
        state.search_cursor,
        state.focus == RepoPickerFocus::Search,
    );
}

/// Render the path-browser dropdown under the path input: the browsed dir as
/// the block title, then a spinner / error / empty line or the entry rows
/// (git repos marked with an accent `●git`; the cursor row highlighted).
fn render_path_browser(
    frame: &mut Frame,
    area: ratatui::layout::Rect,
    browser: &BrowserView<'_>,
    spinner: &str,
) {
    // Elide the dir from the left so the tail (the part being browsed) stays
    // visible in a narrow modal.
    let max_title = area.width.saturating_sub(6) as usize;
    let dir = if browser.dir.chars().count() > max_title {
        let tail: String = browser
            .dir
            .chars()
            .rev()
            .take(max_title.saturating_sub(1))
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        format!("…{tail}")
    } else {
        browser.dir.to_string()
    };
    let block = Block::default()
        .title(format!(" Browse {dir} "))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Theme::border_focused()));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if browser.loading {
        let line = Line::from(Span::styled(
            format!(" {spinner} listing…"),
            Style::default().fg(Theme::text_muted()),
        ));
        frame.render_widget(Paragraph::new(line), inner);
        return;
    }
    if let Some(err) = browser.error {
        let line = Line::from(Span::styled(
            format!(" {err}"),
            Style::default().fg(Theme::danger()),
        ));
        frame.render_widget(Paragraph::new(line), inner);
        return;
    }
    if browser.entries.is_empty() {
        let line = Line::from(Span::styled(
            " (no subdirectories)",
            Style::default().fg(Theme::text_muted()),
        ));
        frame.render_widget(Paragraph::new(line), inner);
        return;
    }

    let visible = inner.height as usize;
    let scroll_offset = browser.index.saturating_sub(visible.saturating_sub(1));
    let items: Vec<ListItem<'_>> = browser
        .entries
        .iter()
        .enumerate()
        .skip(scroll_offset)
        .take(visible)
        .map(|(i, entry)| {
            let style = if i == browser.index {
                Theme::selected_item()
            } else {
                Theme::normal_item()
            };
            let mut spans = vec![Span::styled(format!(" {}/", entry.name), style)];
            if entry.is_git {
                spans.push(Span::styled(" ●git", Style::default().fg(Theme::accent())));
            }
            ListItem::new(Line::from(spans))
        })
        .collect();
    frame.render_widget(List::new(items), inner);
}

/// Render the bookmark list with checkboxes, fuzzy highlighting, and scrolling.
fn render_bookmark_list(
    frame: &mut Frame,
    list_area: ratatui::layout::Rect,
    state: &RepoPickerState<'_>,
) -> super::SelectorHits {
    let list_focused = state.focus == RepoPickerFocus::List;
    let border_color = if list_focused {
        Theme::border_focused()
    } else {
        Theme::border_unfocused()
    };

    let title = match state.host {
        Some(host) => format!(" Repos on {host} ({}) ", state.bookmarks.len()),
        None => format!(" Repos ({}) ", state.bookmarks.len()),
    };

    let list_block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_style(Style::default().fg(border_color));

    let list_inner_area = list_block.inner(list_area);
    frame.render_widget(list_block, list_area);

    if state.filtered_indices.is_empty() {
        let msg = if state.search_query.is_empty() {
            "  No bookmarks — add via path input below"
        } else {
            "  No matches"
        };
        let placeholder = Paragraph::new(Line::from(Span::styled(
            msg,
            Style::default().fg(Theme::text_muted()),
        )));
        frame.render_widget(placeholder, list_inner_area);
        return (Vec::new(), None);
    }

    let total = state.filtered_indices.len();
    let visible_count = list_inner_area.height as usize;
    let scroll_offset = if state.list_index >= visible_count {
        state.list_index - visible_count + 1
    } else {
        0
    };

    // Reserve the rightmost column for a scrollbar when the list overflows.
    let (rows_area, track) = super::scrollbar::reserve_track(list_inner_area, total, visible_count);

    let items: Vec<ListItem<'_>> = state
        .filtered_indices
        .iter()
        .enumerate()
        .skip(scroll_offset)
        .take(visible_count)
        .map(|(vi, &real_idx)| bookmark_item(state, vi, real_idx, list_focused))
        .collect();

    frame.render_widget(List::new(items), rows_area);

    // One hitbox per visible row, indexed by position in `filtered_indices`
    // (the same space as `list_index`).
    let hitboxes = (scroll_offset..total.min(scroll_offset + visible_count))
        .enumerate()
        .map(|(line, vi)| super::RowHitbox {
            rect: ratatui::layout::Rect::new(
                rows_area.x,
                rows_area.y + line as u16,
                rows_area.width,
                1,
            ),
            index: vi,
        })
        .collect();

    let geom = track.and_then(|t| {
        super::scrollbar::render_into(frame, t, total, visible_count, state.list_index)
    });
    (hitboxes, geom)
}

/// Build a single bookmark list item (checkbox + path + optional `[wt]` marker).
fn bookmark_item<'a>(
    state: &RepoPickerState<'a>,
    visible_index: usize,
    real_idx: usize,
    list_focused: bool,
) -> ListItem<'a> {
    let path = &state.bookmarks[real_idx];
    let is_header = state.is_header.get(real_idx).copied().unwrap_or(false);
    let is_cursor = visible_index == state.list_index && list_focused;

    let style = if is_cursor {
        Theme::selected_item()
    } else {
        Theme::normal_item()
    };

    // Parent header row: no checkbox, a collapse glyph + basename + dim marker.
    if is_header {
        return header_item(state, path, style);
    }

    child_item(state, real_idx, style)
}

/// Build a parent header row: a collapse glyph + basename + dim `(parent)` marker.
fn header_item<'a>(
    state: &RepoPickerState<'a>,
    path: &std::path::Path,
    style: Style,
) -> ListItem<'a> {
    let glyph = if state.collapsed.contains(path) {
        "▸ "
    } else {
        "▾ "
    };
    ListItem::new(Line::from(vec![
        Span::styled(glyph, style),
        Span::styled(crate::paths::display_path(path), style),
        Span::styled(" (parent)", Style::default().fg(Theme::text_muted())),
    ]))
}

/// Build a (possibly indented) child bookmark row: checkbox + path + optional
/// `[wt]` marker, with the search query highlighted when present.
fn child_item<'a>(state: &RepoPickerState<'a>, real_idx: usize, style: Style) -> ListItem<'a> {
    let path = &state.bookmarks[real_idx];
    let checked = state.selected[real_idx];
    let is_wt = state.worktree[real_idx];
    let is_child = state.is_child.get(real_idx).copied().unwrap_or(false);

    let indent = if is_child { "  " } else { "" };
    let check = if checked { "[x] " } else { "[ ] " };
    let display = crate::paths::display_path(path);

    let mut spans = vec![Span::styled(indent, style)];
    if state.search_query.is_empty() {
        spans.push(Span::styled(format!("{check}{display}"), style));
    } else {
        spans.extend(highlighted_spans(
            state.search_query,
            check,
            &display,
            style,
        ));
    }

    if checked && is_wt {
        spans.push(Span::styled(" [wt]", Style::default().fg(Theme::accent())));
    }
    // A known non-repo reads as a plain dir — selectable, but `w` won't take.
    if state.is_git.get(real_idx).copied().flatten() == Some(false) {
        spans.push(Span::styled(
            " (dir)",
            Style::default().fg(Theme::text_muted()),
        ));
    }
    ListItem::new(Line::from(spans))
}

/// Build spans for a bookmark with fuzzy-match positions highlighted in the accent color.
fn highlighted_spans(query: &str, check: &str, display: &str, style: Style) -> Vec<Span<'static>> {
    let positions = crate::fuzzy::fuzzy_match(query, display)
        .map(|m| m.positions)
        .unwrap_or_default();
    let mut result = vec![Span::styled(check.to_string(), style)];
    let mut last = 0;
    for &pos in &positions {
        if pos > last {
            result.push(Span::styled(display[last..pos].to_string(), style));
        }
        let end = display[pos..]
            .chars()
            .next()
            .map(|c| pos + c.len_utf8())
            .unwrap_or(pos + 1);
        result.push(Span::styled(
            display[pos..end].to_string(),
            Style::default().fg(Theme::accent()),
        ));
        last = end;
    }
    if last < display.len() {
        result.push(Span::styled(display[last..].to_string(), style));
    }
    result
}

/// Build the footer hint line for the current focus.
fn footer_line(state: &RepoPickerState<'_>) -> Line<'static> {
    match state.focus {
        RepoPickerFocus::List => Line::from(vec![
            Span::styled("j/k", Theme::keybind()),
            Span::styled(" nav  ", Theme::keybind_desc()),
            Span::styled("Space", Theme::keybind()),
            Span::styled(" toggle/fold  ", Theme::keybind_desc()),
            Span::styled("w", Theme::keybind()),
            Span::styled(" worktree  ", Theme::keybind_desc()),
            Span::styled("/", Theme::keybind()),
            Span::styled(" search  ", Theme::keybind_desc()),
            Span::styled("d", Theme::keybind()),
            Span::styled(" delete  ", Theme::keybind_desc()),
            Span::styled("Tab", Theme::keybind()),
            Span::styled(" input  ", Theme::keybind_desc()),
            Span::styled("Enter", Theme::keybind()),
            Span::styled(" ok", Theme::keybind_desc()),
        ]),
        RepoPickerFocus::Input if state.browser.is_some() => Line::from(vec![
            Span::styled("↑/↓", Theme::keybind()),
            Span::styled(" select  ", Theme::keybind_desc()),
            Span::styled("Enter", Theme::keybind()),
            Span::styled(" open/pick  ", Theme::keybind_desc()),
            Span::styled("Tab", Theme::keybind()),
            Span::styled(" refresh  ", Theme::keybind_desc()),
            Span::styled("Esc", Theme::keybind()),
            Span::styled(" close", Theme::keybind_desc()),
        ]),
        RepoPickerFocus::Input => {
            let tab_hint = if state.path_suggestion.is_some() {
                " complete  "
            } else {
                " browse  "
            };
            Line::from(vec![
                Span::styled("Tab", Theme::keybind()),
                Span::styled(tab_hint, Theme::keybind_desc()),
                Span::styled("Enter", Theme::keybind()),
                Span::styled(" add repo  ", Theme::keybind_desc()),
                Span::styled("Ctrl+P", Theme::keybind()),
                Span::styled(" import parent  ", Theme::keybind_desc()),
                Span::styled("Esc", Theme::keybind()),
                Span::styled(" cancel", Theme::keybind_desc()),
            ])
        }
        RepoPickerFocus::Search => Line::from(vec![
            Span::styled("Enter", Theme::keybind()),
            Span::styled(" keep filter  ", Theme::keybind_desc()),
            Span::styled("Esc", Theme::keybind()),
            Span::styled(" clear  ", Theme::keybind_desc()),
        ]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn span_text(spans: &[Span<'_>]) -> String {
        spans.iter().map(|s| s.content.as_ref()).collect()
    }

    fn accent_chars(spans: &[Span<'_>]) -> String {
        spans
            .iter()
            .filter(|s| s.style.fg == Some(Theme::accent()))
            .map(|s| s.content.as_ref())
            .collect()
    }

    #[test]
    fn highlighted_spans_no_match_keeps_display_in_base_style() {
        let base = Style::default();
        // "zzz" can't be a subsequence of "hello" → no highlights.
        let spans = highlighted_spans("zzz", "[x] ", "hello", base);
        assert_eq!(span_text(&spans), "[x] hello");
        assert!(
            accent_chars(&spans).is_empty(),
            "no character should be accented on a miss"
        );
    }

    #[test]
    fn highlighted_spans_empty_query_highlights_nothing() {
        let base = Style::default();
        let spans = highlighted_spans("", "", "hello", base);
        assert_eq!(span_text(&spans), "hello");
        assert!(accent_chars(&spans).is_empty());
    }

    #[test]
    fn highlighted_spans_accents_matched_characters() {
        let base = Style::default();
        let spans = highlighted_spans("ll", "", "hello", base);
        assert_eq!(span_text(&spans), "hello");
        assert_eq!(accent_chars(&spans), "ll");
    }

    #[test]
    fn highlighted_spans_slices_multibyte_chars_correctly() {
        let base = Style::default();
        // 'é' is two bytes; the position slicing must use char width, not +1,
        // or this would panic / corrupt the text (regression target).
        let spans = highlighted_spans("é", "", "héllo", base);
        assert_eq!(span_text(&spans), "héllo");
        assert_eq!(accent_chars(&spans), "é");
    }

    fn picker_state(
        focus: RepoPickerFocus,
        suggestion: Option<&'static str>,
    ) -> RepoPickerState<'static> {
        static EMPTY_PATHS: &[PathBuf] = &[];
        static EMPTY_BOOLS: &[bool] = &[];
        static EMPTY_IDX: &[usize] = &[];
        static EMPTY_GIT: &[Option<bool>] = &[];
        // Leaked once so the borrow is 'static — fine for a test fixture.
        let collapsed: &'static HashSet<PathBuf> = Box::leak(Box::new(HashSet::new()));
        RepoPickerState {
            bookmarks: EMPTY_PATHS,
            selected: EMPTY_BOOLS,
            worktree: EMPTY_BOOLS,
            is_header: EMPTY_BOOLS,
            is_child: EMPTY_BOOLS,
            collapsed,
            list_index: 0,
            path_input: "",
            path_cursor: 0,
            path_suggestion: suggestion,
            focus,
            search_query: "",
            search_cursor: 0,
            search_active: false,
            filtered_indices: EMPTY_IDX,
            host: None,
            is_git: EMPTY_GIT,
            browser: None,
            pending_commit: false,
            spinner: "⠋",
        }
    }

    #[test]
    fn footer_line_list_focus_shows_navigation_hints() {
        let s = picker_state(RepoPickerFocus::List, None);
        let text = span_text(&footer_line(&s).spans);
        assert!(text.contains("toggle/fold"));
        assert!(text.contains("worktree"));
    }

    #[test]
    fn footer_line_input_focus_tab_hint_depends_on_suggestion() {
        let with = picker_state(RepoPickerFocus::Input, Some("/home/me/proj"));
        let with_text = span_text(&footer_line(&with).spans);
        assert!(with_text.contains("complete"));
        assert!(with_text.contains("add repo"));

        let without = picker_state(RepoPickerFocus::Input, None);
        let without_text = span_text(&footer_line(&without).spans);
        assert!(without_text.contains("browse"));
        assert!(!without_text.contains("complete"));
    }

    #[test]
    fn footer_line_open_browser_shows_dropdown_hints() {
        let mut s = picker_state(RepoPickerFocus::Input, None);
        s.browser = Some(BrowserView {
            dir: "~/projects",
            entries: Vec::new(),
            index: 0,
            loading: false,
            error: None,
        });
        let text = span_text(&footer_line(&s).spans);
        assert!(text.contains("open/pick"));
        assert!(text.contains("refresh"));
        assert!(text.contains("close"));
    }

    #[test]
    fn footer_line_search_focus_shows_filter_hints() {
        let s = picker_state(RepoPickerFocus::Search, None);
        let text = span_text(&footer_line(&s).spans);
        assert!(text.contains("keep filter"));
        assert!(text.contains("clear"));
    }
}
