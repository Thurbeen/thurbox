//! Modal that lets the user pick a theme (built-in preset or custom).
//!
//! Renders a filterable preset list on the left and a live colour swatch for
//! the highlighted preset on the right, so users can preview the palette
//! before committing.
//!
//! The list is long — 30+ built-ins plus any custom themes from `themes.toml`
//! — so the modal is sized to most of the frame, groups entries under `Dark` /
//! `Light` headers, and can be narrowed by a query. The query lives behind `/`
//! (a sub-mode, like the file viewer's find) so `j`/`k` keep selecting as in
//! every other picker; `filter: None` is navigation mode. Headers are
//! *rendered* decoration only: they are drawn inside the same rows the entries
//! occupy, so selection indices, hitboxes, and the scrollbar all stay in plain
//! entry space (see `build_rows`).

use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::Style,
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use super::{
    centered_fixed_height_rect, render_modal_frame, scrollbar, selector_line, theme::Theme,
    RowHitbox, SelectorHits,
};
use crate::session::theme_config::ThemeEntry;

pub struct ThemePickerState<'a> {
    /// Every theme, in list order. Indices in `matches` point into this.
    pub entries: &'a [ThemeEntry],
    /// Indices of `entries` surviving the filter, in list order.
    pub matches: &'a [usize],
    /// Cursor position within `matches` (not within `entries`).
    pub selected_index: usize,
    /// The open filter query, or `None` in normal navigation mode. The two are
    /// rendered differently: a live `/`-prefixed query vs. a hint advertising
    /// the key that opens it.
    pub filter: Option<&'a str>,
}

/// A visible list row: an entry to draw, optionally preceded by a section
/// header. Keeping the header attached to its entry (rather than making it its
/// own row) is what lets the whole list stay indexed in entry space.
struct Row {
    /// Index into `matches` — i.e. the value the selection cursor takes.
    match_index: usize,
    /// Index into `entries`.
    entry_index: usize,
    /// Section header to draw on the line above this entry, if it opens a
    /// section.
    header: Option<&'static str>,
}

/// Pair each filtered entry with the section header that precedes it. A header
/// is emitted at the first entry of each `Dark`/`Light` run, so a filter that
/// removes every light theme also removes the `Light` header.
fn build_rows(entries: &[ThemeEntry], matches: &[usize]) -> Vec<Row> {
    let mut rows = Vec::with_capacity(matches.len());
    let mut current: Option<bool> = None;
    for (match_index, &entry_index) in matches.iter().enumerate() {
        let is_light = entries[entry_index].is_light;
        let header = (current != Some(is_light)).then_some(if is_light { "Light" } else { "Dark" });
        current = Some(is_light);
        rows.push(Row {
            match_index,
            entry_index,
            header,
        });
    }
    rows
}

/// Screen lines a row occupies: its own line, plus one for a section header
/// when it opens a section.
fn row_height(row: &Row) -> u16 {
    1 + u16::from(row.header.is_some())
}

/// Total screen lines `rows` would occupy if drawn in full. Saturating: a
/// `themes.toml` with thousands of entries must clamp, not wrap a `u16`.
fn total_height(rows: &[Row]) -> u16 {
    rows.iter()
        .map(row_height)
        .fold(0u16, |acc, h| acc.saturating_add(h))
}

/// Choose the first visible row so the selected row stays on screen, keeping a
/// few rows of context above it where possible. Returns an index into `rows`.
///
/// Row heights vary (a section-opening row costs an extra line for its
/// header), so this walks rows rather than doing the flat index arithmetic
/// [`super::file_viewer::visible_window`] can get away with.
fn scroll_start(rows: &[Row], selected: usize, height: u16) -> usize {
    /// Rows of context to keep above the selection while scrolling, matching
    /// the other selectors' feel.
    const MAX_MARGIN: u16 = 3;

    let selected = selected.min(rows.len().saturating_sub(1));
    // Back off from the selection far enough to show the margin above it...
    let margin = (height / 4).min(MAX_MARGIN);
    let mut start = selected;
    let mut used = 0u16;
    while start > 0 && used + row_height(&rows[start - 1]) <= margin {
        used += row_height(&rows[start - 1]);
        start -= 1;
    }
    // ...then, if that leaves blank space at the bottom (we're near the end of
    // the list), pull the top back up to fill the pane.
    let mut total = total_height(&rows[start..]);
    while start > 0 && total + row_height(&rows[start - 1]) <= height {
        start -= 1;
        total += row_height(&rows[start]);
    }
    start
}

/// Draw the (header-annotated) entry rows into `area`, windowed around the
/// selection, with a scrollbar when they overflow. Hitboxes are returned in
/// *match* space so a click maps straight back to the selection cursor.
fn render_rows(
    frame: &mut Frame,
    area: Rect,
    entries: &[ThemeEntry],
    rows: &[Row],
    selected: usize,
) -> SelectorHits {
    if rows.is_empty() || area.height == 0 || area.width == 0 {
        let empty = Line::from(Span::styled(
            "  no matching theme",
            Style::default().fg(Theme::text_muted()),
        ));
        frame.render_widget(Paragraph::new(vec![empty]), area);
        return (Vec::new(), None);
    }

    // Reserve the scrollbar track up front (against the full un-windowed
    // height) so the rows column keeps one width as the window scrolls.
    let (rows_area, track) =
        scrollbar::reserve_track(area, total_height(rows) as usize, area.height as usize);

    let start = scroll_start(rows, selected, rows_area.height);
    let mut lines: Vec<Line<'_>> = Vec::new();
    let mut hitboxes: Vec<RowHitbox> = Vec::new();
    for row in &rows[start..] {
        let remaining = rows_area.height.saturating_sub(lines.len() as u16);
        if remaining < row_height(row) {
            break;
        }
        if let Some(header) = row.header {
            lines.push(Line::from(Span::styled(
                format!(" {header}"),
                Style::default()
                    .fg(Theme::text_muted())
                    .add_modifier(ratatui::style::Modifier::BOLD),
            )));
        }
        let entry = &entries[row.entry_index];
        hitboxes.push(RowHitbox {
            rect: Rect::new(
                rows_area.x,
                rows_area.y + lines.len() as u16,
                rows_area.width,
                1,
            ),
            index: row.match_index,
        });
        lines.push(selector_line(
            &entry.display_name,
            row.match_index == selected,
        ));
    }
    frame.render_widget(Paragraph::new(lines), rows_area);

    // The scrollbar tracks rows, not lines, so its thumb matches what the
    // selection cursor moves through (headers don't get their own stop).
    let geom = track.and_then(|t| {
        scrollbar::render_into(frame, t, rows.len(), rows_area.height as usize, selected)
    });
    (hitboxes, geom)
}

/// Colour swatch + identity block for the highlighted theme.
fn render_preview(frame: &mut Frame, area: Rect, entry: &ThemeEntry) {
    let palette = &entry.palette;
    let label = |text: &'static str| Span::styled(text, Style::default().fg(Theme::text_muted()));
    let swatch = vec![
        Line::from(vec![
            label("  Accent      "),
            Span::styled("█████", Style::default().fg(palette.accent)),
            Span::raw(" "),
            Span::styled("█████", Style::default().fg(palette.accent_bright)),
        ]),
        Line::from(vec![
            label("  Status      "),
            Span::styled("◐", Style::default().fg(palette.status_working)),
            Span::raw(" "),
            Span::styled("◆", Style::default().fg(palette.status_blocked)),
            Span::raw(" "),
            Span::styled("●", Style::default().fg(palette.status_done)),
            Span::raw(" "),
            Span::styled("○", Style::default().fg(palette.status_idle)),
            Span::raw(" "),
            Span::styled("✗", Style::default().fg(palette.status_error)),
        ]),
        Line::from(vec![
            label("  Text        "),
            Span::styled("Aa", Style::default().fg(palette.text_primary)),
            Span::raw(" "),
            Span::styled("Aa", Style::default().fg(palette.text_secondary)),
            Span::raw(" "),
            Span::styled("Aa", Style::default().fg(palette.text_muted)),
        ]),
        Line::from(vec![
            label("  Diff        "),
            Span::styled(
                " + ",
                Style::default()
                    .fg(palette.diff_added)
                    .bg(palette.diff_added_bg),
            ),
            Span::raw(" "),
            Span::styled(
                " - ",
                Style::default()
                    .fg(palette.diff_removed)
                    .bg(palette.diff_removed_bg),
            ),
        ]),
        Line::from(vec![
            label("  Border      "),
            Span::styled("╭─────╮", Style::default().fg(palette.border_focused)),
        ]),
        Line::from(vec![
            label("  Modal bg    "),
            Span::styled(
                "         ",
                Style::default()
                    .bg(palette.modal_bg)
                    .fg(palette.text_primary),
            ),
        ]),
    ];
    frame.render_widget(Paragraph::new(swatch), area);
}

/// Render the picker. Returns the usual [`super::ModalRender`] plus the number
/// of entry rows the list showed this frame, which the app stores as its
/// page-scroll step.
pub fn render_theme_picker_modal(
    frame: &mut Frame,
    state: &ThemePickerState<'_>,
) -> (super::ModalRender, usize) {
    let rows = build_rows(state.entries, state.matches);

    // Grow with the content, but cap at a share of the frame so the app chrome
    // stays visible; the list then scroll-windows inside.
    /// Non-list rows the modal always spends: top+bottom border, filter line,
    /// theme-id line, footer, and one spacer.
    const CHROME: u16 = 6;
    /// Tallest the modal may grow, as a percent of the frame height.
    const MAX_FRAME_PCT: u16 = 85;
    /// Floor so the modal stays usable on a very short terminal.
    const MIN_HEIGHT: u16 = 10;
    let frame_h = frame.area().height;
    let desired = total_height(&rows).saturating_add(CHROME).max(MIN_HEIGHT);
    let cap = (frame_h * MAX_FRAME_PCT / 100).max(MIN_HEIGHT);
    let height = desired.min(cap).min(frame_h);
    let area = centered_fixed_height_rect(64, height, frame.area());

    let inner = render_modal_frame(frame, area, "Theme");

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // filter query
            Constraint::Min(1),    // list + preview
            Constraint::Length(1), // theme id
            Constraint::Length(1), // footer buttons
        ])
        .split(inner);

    // Header line: while filtering, the live `/`-prefixed query (with a block
    // cursor, so the sub-mode is unmistakably focused); otherwise a hint
    // advertising the key that opens it. Either way the match count sits at the
    // right, so a query that narrows to nothing is obvious rather than an
    // unexplained empty list.
    /// Blank column kept right of the count so it doesn't touch the border.
    const RIGHT_GUTTER: usize = 1;
    let count = format!("{}/{} themes", state.matches.len(), state.entries.len());
    let head: Vec<Span<'_>> = match state.filter {
        Some(query) => vec![
            Span::styled(" / ", Style::default().fg(Theme::accent())),
            Span::styled(
                query.to_string(),
                Style::default().fg(Theme::text_primary()),
            ),
            Span::styled("█", Style::default().fg(Theme::accent())),
        ],
        None => vec![
            Span::styled(" / ", Style::default().fg(Theme::accent())),
            Span::styled("filter themes", Style::default().fg(Theme::text_muted())),
        ],
    };
    // Right-align the count against the line's right edge (not the query's
    // width) so it holds one column as the query grows and shrinks.
    let used: usize = head.iter().map(|s| s.content.chars().count()).sum();
    let pad =
        (chunks[0].width as usize).saturating_sub(used + count.chars().count() + RIGHT_GUTTER);
    let mut header = head;
    header.push(Span::raw(" ".repeat(pad)));
    header.push(Span::styled(
        count,
        Style::default().fg(Theme::text_muted()),
    ));
    frame.render_widget(Paragraph::new(Line::from(header)), chunks[0]);

    let columns = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(45), Constraint::Percentage(55)])
        .split(chunks[1]);

    let hits = render_rows(
        frame,
        columns[0],
        state.entries,
        &rows,
        state.selected_index,
    );

    let selected_entry = state
        .matches
        .get(state.selected_index)
        .and_then(|&i| state.entries.get(i));
    if let Some(entry) = selected_entry {
        render_preview(frame, columns[1], entry);
        let id = Line::from(Span::styled(
            format!("  theme id: {}", entry.name),
            Style::default().fg(Theme::text_muted()),
        ));
        frame.render_widget(Paragraph::new(id), chunks[2]);
    }

    // Not the shared `render_selector_footer`: its fixed `j/k navigate` hint
    // can't show the extra keys, and while filtering `j`/`k` are query text.
    let hint = Line::from(if state.filter.is_some() {
        vec![
            Span::styled("↑/↓", Theme::keybind()),
            Span::styled(" navigate  ", Theme::keybind_desc()),
            Span::styled("type", Theme::keybind()),
            Span::styled(" filter  ", Theme::keybind_desc()),
            Span::styled("Esc", Theme::keybind()),
            Span::styled(" clear filter", Theme::keybind_desc()),
        ]
    } else {
        vec![
            Span::styled("j/k", Theme::keybind()),
            Span::styled(" navigate  ", Theme::keybind_desc()),
            Span::styled("PgUp/PgDn", Theme::keybind()),
            Span::styled(" page  ", Theme::keybind_desc()),
            Span::styled("/", Theme::keybind()),
            Span::styled(" filter", Theme::keybind_desc()),
        ]
    });
    let buttons = super::render_hint_action_footer(
        frame,
        chunks[3],
        hint,
        (
            "Select",
            crossterm::event::KeyCode::Enter,
            crossterm::event::KeyModifiers::NONE,
        ),
        "Cancel",
    );
    // One hitbox per visible entry row, so its count *is* the page step.
    let visible_rows = hits.0.len();
    // Rows live in the left column only; the swatch column isn't clickable.
    ((hits, buttons), visible_rows)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::theme_config::ThemePreset;

    fn entries() -> Vec<ThemeEntry> {
        ThemePreset::all()
            .iter()
            .map(|p| ThemeEntry::from_preset(*p))
            .collect()
    }

    #[test]
    fn build_rows_emits_one_header_per_section() {
        let entries = entries();
        let matches: Vec<usize> = (0..entries.len()).collect();
        let rows = build_rows(&entries, &matches);
        assert_eq!(rows.len(), entries.len());
        let headers: Vec<&str> = rows.iter().filter_map(|r| r.header).collect();
        // `all()` is dark-then-light, so exactly two sections in that order.
        assert_eq!(headers, vec!["Dark", "Light"]);
    }

    #[test]
    fn build_rows_drops_a_header_whose_section_is_filtered_out() {
        let entries = entries();
        // Only dark entries survive.
        let matches: Vec<usize> = entries
            .iter()
            .enumerate()
            .filter(|(_, e)| !e.is_light)
            .map(|(i, _)| i)
            .collect();
        let rows = build_rows(&entries, &matches);
        let headers: Vec<&str> = rows.iter().filter_map(|r| r.header).collect();
        assert_eq!(headers, vec!["Dark"]);
    }

    #[test]
    fn build_rows_indexes_in_match_space() {
        let entries = entries();
        // A sparse filter: every third entry.
        let matches: Vec<usize> = (0..entries.len()).step_by(3).collect();
        let rows = build_rows(&entries, &matches);
        for (i, row) in rows.iter().enumerate() {
            assert_eq!(row.match_index, i, "match_index must be filtered-space");
            assert_eq!(row.entry_index, matches[i]);
        }
    }

    #[test]
    fn scroll_start_keeps_selection_visible() {
        let entries = entries();
        let matches: Vec<usize> = (0..entries.len()).collect();
        let rows = build_rows(&entries, &matches);
        let height = 10u16;
        for selected in 0..rows.len() {
            let start = scroll_start(&rows, selected, height);
            assert!(start <= selected, "selection scrolled off the top");
            // Walk the window and confirm the selected row fits inside it.
            let mut used = 0u16;
            let mut visible = false;
            for row in &rows[start..] {
                if used + row_height(row) > height {
                    break;
                }
                used += row_height(row);
                if row.match_index == selected {
                    visible = true;
                    break;
                }
            }
            assert!(
                visible,
                "selection {selected} not visible from start {start}"
            );
        }
    }

    #[test]
    fn scroll_start_is_zero_when_everything_fits() {
        let entries = entries();
        let matches: Vec<usize> = (0..3).collect();
        let rows = build_rows(&entries, &matches);
        assert_eq!(scroll_start(&rows, 2, 40), 0);
    }
}
