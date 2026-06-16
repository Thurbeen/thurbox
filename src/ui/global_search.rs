//! Renderer for the global-search strip (`Ctrl+/`) — a non-modal panel docked
//! full-width along the bottom. Matches also highlight **live in the panels
//! themselves** (session list, tasks, automations). The strip shows: a query
//! line, a per-scope match summary, the grouped result list (scrollable, with
//! the selected row highlighted), and key hints.

use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
    Frame,
};

use crate::app::search::{GlobalSearchResult, SearchKind};

use super::theme::Theme;
use super::truncate_ellipsis;

/// View data for the strip (built by the app layer, so the UI stays free of the
/// app's private `TextInput` internals — mirrors `tasks_panel`).
pub(crate) struct GlobalSearchView<'a> {
    pub query: &'a str,
    pub cursor: usize,
    pub results: &'a [GlobalSearchResult],
    pub selected: usize,
}

pub(crate) fn render_global_search(frame: &mut Frame, area: Rect, state: &GlobalSearchView<'_>) {
    // Same dedicated search-bar accent the session-list search uses.
    let bar_style = Style::default().fg(Theme::search_bar());
    let block = Block::default()
        .title(Line::from(Span::styled(" Search ", bar_style)))
        .borders(Borders::ALL)
        .border_style(bar_style);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.height == 0 {
        return;
    }

    // Query line, per-scope summary, the result list (flexible), then key hints.
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // query
            Constraint::Length(1), // summary (counts)
            Constraint::Min(0),    // result list
            Constraint::Length(1), // hints
        ])
        .split(inner);

    render_query_line(frame, chunks[0], state);
    if inner.height >= 2 {
        render_summary_line(frame, chunks[1], state);
    }
    if inner.height >= 4 {
        render_results(frame, chunks[2], state);
    }
    if inner.height >= 3 {
        render_hint_line(frame, chunks[3]);
    }
}

/// The grouped, scrollable result list. Each scope gets a dim header; the
/// selected row is highlighted and kept in view; content matches show a dim
/// snippet line. Mirrors the tasks-panel styling.
fn render_results(frame: &mut Frame, area: Rect, state: &GlobalSearchView<'_>) {
    if area.height == 0 || state.results.is_empty() {
        return;
    }
    let width = area.width as usize;

    // Build display lines, tracking which line carries the selection so we can
    // scroll it into view.
    let mut lines: Vec<Line> = Vec::new();
    let mut selected_line = 0usize;
    let mut last_kind: Option<SearchKind> = None;
    for (i, r) in state.results.iter().enumerate() {
        if last_kind != Some(r.kind) {
            lines.push(scope_header_line(r.kind));
            last_kind = Some(r.kind);
        }
        let selected = i == state.selected;
        if selected {
            selected_line = lines.len();
        }
        push_result_row(&mut lines, r, selected, width);
    }

    // Scroll so the selected line stays visible within the result area.
    let height = area.height as usize;
    let offset = selected_line.saturating_sub(height.saturating_sub(1));
    let visible: Vec<Line> = lines.into_iter().skip(offset).collect();
    frame.render_widget(Paragraph::new(visible), area);
}

/// A dim, bold scope header line introducing a group of results.
fn scope_header_line<'a>(kind: SearchKind) -> Line<'a> {
    Line::from(Span::styled(
        format!(" {}", scope_header(kind)),
        Style::default()
            .fg(Theme::text_muted())
            .add_modifier(Modifier::BOLD),
    ))
}

/// Push a single result's row line (plus its optional dim snippet line) into
/// `lines`, marking and highlighting it when `selected`.
fn push_result_row<'a>(
    lines: &mut Vec<Line<'a>>,
    r: &'a GlobalSearchResult,
    selected: bool,
    width: usize,
) {
    let marker = if selected { "▸ " } else { "  " };
    let row = truncate_ellipsis(&format!("{marker}{}", r.label), width);
    let style = if selected {
        Theme::selected_item()
    } else {
        Style::default().fg(Theme::text_primary())
    };
    lines.push(Line::from(Span::styled(row, style)));
    if let Some(snippet) = &r.snippet {
        lines.push(Line::from(Span::styled(
            truncate_ellipsis(&format!("      {snippet}"), width),
            Style::default()
                .fg(Theme::text_muted())
                .add_modifier(Modifier::ITALIC),
        )));
    }
}

fn render_query_line(frame: &mut Frame, area: Rect, state: &GlobalSearchView<'_>) {
    let chars: Vec<char> = state.query.chars().collect();
    let cursor = state.cursor.min(chars.len());
    let before: String = chars[..cursor].iter().collect();
    let after: String = chars[cursor..].iter().collect();
    let line = Line::from(vec![
        Span::styled("🔍 ", Style::default().fg(Theme::search_bar())),
        Span::styled(
            before,
            Style::default()
                .fg(Theme::text_primary())
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled("▏", Style::default().fg(Theme::search_bar())),
        Span::styled(
            after,
            Style::default()
                .fg(Theme::text_primary())
                .add_modifier(Modifier::BOLD),
        ),
    ]);
    frame.render_widget(Paragraph::new(line), area);
}

/// A one-line summary of how many matches each scope has, with the selected
/// result called out so the user knows what `Enter` will jump to.
fn render_summary_line(frame: &mut Frame, area: Rect, state: &GlobalSearchView<'_>) {
    if state.query.trim().is_empty() {
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                "type to search sessions · tasks · automations · files",
                Style::default().fg(Theme::text_muted()),
            ))),
            area,
        );
        return;
    }
    if state.results.is_empty() {
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                "no matches",
                Style::default().fg(Theme::text_muted()),
            ))),
            area,
        );
        return;
    }

    // Counts per scope, in display order.
    let mut spans: Vec<Span> = Vec::new();
    let order = [
        SearchKind::Session,
        SearchKind::Task,
        SearchKind::Automation,
        SearchKind::File,
    ];
    let mut first = true;
    for kind in order {
        let n = state.results.iter().filter(|r| r.kind == kind).count();
        if n == 0 {
            continue;
        }
        if !first {
            spans.push(Span::styled(
                "  ·  ",
                Style::default().fg(Theme::text_muted()),
            ));
        }
        first = false;
        spans.push(Span::styled(
            format!("{n} "),
            Style::default()
                .fg(Theme::accent())
                .add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::styled(
            scope_word(kind, n),
            Style::default().fg(Theme::text_secondary()),
        ));
    }

    // Show the 1-based position of the selection within the total.
    spans.push(Span::styled(
        format!("   [{}/{}]", state.selected + 1, state.results.len()),
        Style::default().fg(Theme::text_muted()),
    ));

    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn render_hint_line(frame: &mut Frame, area: Rect) {
    let line = Line::from(vec![
        Span::styled("↑↓", Theme::keybind()),
        Span::styled(" select  ", Theme::keybind_desc()),
        Span::styled("↵", Theme::keybind()),
        Span::styled(" jump  ", Theme::keybind_desc()),
        Span::styled("esc", Theme::keybind()),
        Span::styled(" close", Theme::keybind_desc()),
    ]);
    frame.render_widget(Paragraph::new(line), area);
}

/// The display names for a scope: `(uppercase_header, singular, plural)`. Single
/// source so the result-list header and the summary word never disagree.
fn scope_names(kind: SearchKind) -> (&'static str, &'static str, &'static str) {
    match kind {
        SearchKind::Session => ("SESSIONS", "session", "sessions"),
        SearchKind::Task => ("TASKS", "task", "tasks"),
        SearchKind::Automation => ("AUTOMATIONS", "automation", "automations"),
        SearchKind::File => ("FILES", "file", "files"),
    }
}

fn scope_header(kind: SearchKind) -> &'static str {
    scope_names(kind).0
}

fn scope_word(kind: SearchKind, n: usize) -> &'static str {
    let (_, singular, plural) = scope_names(kind);
    if n == 1 {
        singular
    } else {
        plural
    }
}
