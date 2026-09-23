//! Links on a terminal surface: detection, click resolution, OSC 8 re-print.
//!
//! The pure half — a vt100 screen in, positions and URLs out — lived in
//! `session::links` while v1's mouse layer and the kernel both needed it; v1 is
//! gone, this file is its only consumer, so the two halves live together now.
//! Positions are display-width based, not byte- or char-based, so they map
//! directly onto vt100 cell columns even on rows containing wide (CJK/emoji)
//! glyphs, which each span two cells.

use std::rc::Rc;

use ratatui::buffer::Buffer;
use ratatui::layout::{Position, Rect};
use ratatui::style::Style;
use ratatui::widgets::Clear;
use ratatui::Frame;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use super::Terminals;

pub struct DetectedLink {
    pub row: usize,
    pub start_col: usize,
    pub end_col: usize,
    pub url: String,
}

/// URL schemes we linkify, matching the old `(?:https?|file)://` prefix.
const SCHEMES: [&str; 3] = ["https://", "http://", "file://"];

/// Scanned links one surface may hand to the outer terminal in a frame.
///
/// The mirror of `session::hyperlink::VISIBLE_SCAN_LIMIT`, which bounds the OSC
/// 8 leg, and owed for the same reason — except that this one counts what goes
/// **out**, not what comes in. A screen of nothing but URLs really does scan as
/// hundreds of them, and every one accepted is re-printed over the ssh link
/// this whole pass exists to serve. Counting the input instead would misfire on
/// the ordinary case: `Terminals::links` lists the OSC 8 runs first and every
/// one of them is declined here, so a prefix cap would spend the budget on
/// entries that emit nothing and drop the bare URLs behind them. A screenful
/// cannot show more than this many links worth reading anyway.
const SCANNED_LINK_LIMIT: usize = 128;

/// A URL run stops at whitespace or any of these terminators (the old
/// character class `[^\s<>"'\x60)\]]`).
fn is_url_terminator(c: char) -> bool {
    c.is_whitespace() || matches!(c, '<' | '>' | '"' | '\'' | '`' | ')' | ']')
}

/// Leftmost, non-overlapping scan for `scheme://…` runs — the hand-rolled
/// equivalent of the former URL regex. Returns each match's byte offset in
/// `row` and the matched slice, so callers keep using byte-based slicing for
/// display-width column math.
fn find_url_runs(row: &str) -> Vec<(usize, &str)> {
    let bytes = row.as_bytes();
    let mut matches = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let rest = &row[i..];
        if let Some(scheme) = SCHEMES.iter().find(|s| rest.starts_with(**s)) {
            let run_len = rest[scheme.len()..]
                .find(is_url_terminator)
                .map(|end| scheme.len() + end)
                .unwrap_or(rest.len());
            matches.push((i, &row[i..i + run_len]));
            i += run_len;
        } else {
            // Advance one full char so byte offsets stay UTF-8 aligned.
            i += rest.chars().next().map_or(1, char::len_utf8);
        }
    }
    matches
}

/// Extract visible rows from a vt100 screen, one string per row.
///
/// Emits one char per *glyph*, not per cell: a wide (CJK/emoji) glyph occupies
/// two cells, where vt100 stores the glyph in the lead cell and marks the next
/// cell as a continuation. Those continuation cells are skipped so the string
/// holds exactly the printed characters, letting [`detect_urls`] recover screen
/// cell columns via display width rather than a per-cell placeholder.
pub fn extract_screen_rows(screen: &vt100::Screen) -> Vec<String> {
    let (rows, cols) = screen.size();
    (0..rows)
        .map(|row| {
            let mut line = String::new();
            for col in 0..cols {
                match screen.cell(row, col) {
                    Some(c) if c.is_wide_continuation() => continue,
                    Some(c) => line.push(c.contents().chars().next().unwrap_or(' ')),
                    None => line.push(' '),
                }
            }
            line
        })
        .collect()
}

/// Detect URLs in screen rows, stripping trailing punctuation.
///
/// Positions are display-width based (not byte- or char-based) so they map
/// directly to vt100 screen cell columns even on rows containing wide
/// (CJK/emoji) glyphs, which each span two cells.
pub fn detect_urls(screen_rows: &[String]) -> Vec<DetectedLink> {
    let mut links = Vec::new();
    for (row_idx, row) in screen_rows.iter().enumerate() {
        for (start, matched) in find_url_runs(row) {
            let mut url = matched;
            while url.ends_with(['.', ',', ';', ':', ')', ']']) {
                url = &url[..url.len() - 1];
            }
            // Skip bare scheme-only matches like "http://" with no host
            if url.len() > "https://".len() {
                // Convert the match's byte offset to cell columns using display
                // width: a wide glyph before the URL shifts it two cells, not one.
                let start_col = UnicodeWidthStr::width(&row[..start]);
                let end_col = start_col + UnicodeWidthStr::width(url);
                links.push(DetectedLink {
                    row: row_idx,
                    start_col,
                    end_col,
                    url: url.to_string(),
                });
            }
        }
    }
    links
}

/// Find the URL at a given screen position, if any.
pub fn url_at_position(links: &[DetectedLink], row: usize, col: usize) -> Option<&str> {
    links
        .iter()
        .find(|link| link.row == row && col >= link.start_col && col < link.end_col)
        .map(|link| link.url.as_str())
}

impl Terminals {
    /// The extracted rows of a surface's screen, shared per output stamp.
    ///
    /// See [`Terminals::rows_cache`]. The stamp is read while the caller
    /// already holds the parser lock, so a cached answer and the grid it was
    /// read from cannot disagree.
    fn cached_rows(&self, surface: &str, parser: &crate::agent::SessionParser) -> Rc<Vec<String>> {
        let stamp = self.output_stamp(surface).unwrap_or(0);
        if let Some((at, rows)) = self.rows_cache.borrow().get(surface) {
            if *at == stamp {
                return rows.clone();
            }
        }
        let rows = Rc::new(extract_screen_rows(parser.screen()));
        self.rows_cache
            .borrow_mut()
            .insert(surface.to_string(), (stamp, rows.clone()));
        rows
    }

    /// Links visible in a session's terminal.
    ///
    /// Read kernel-side because a terminal is a *surface*: its text is in no
    /// tree, so a decorator could never walk it. That is also where OSC 8
    /// rich-text links live — those print only their label, so a plain-text
    /// scan alone would miss them entirely.
    ///
    /// OSC 8 runs come first and a plain-text match on a cell one of them
    /// already covers is dropped, mirroring the precedence v1's `url_at_click`
    /// applies: where a rich-text escape wraps a bare URL, the escape's target
    /// is what a terminal honours, so listing both would offer the same link
    /// twice.
    pub fn links(&self, session: &str) -> Vec<(String, usize, usize)> {
        let Some((_, parser)) = self.surface_parser(session) else {
            return Vec::new();
        };
        let Ok(parser) = parser.lock() else {
            return Vec::new();
        };
        let rows = self.cached_rows(session, &parser);
        let table = parser.callbacks().hyperlinks();

        let mut found: Vec<(String, usize, usize)> = table
            .visible_runs(&rows)
            .into_iter()
            .map(|run| (run.url.to_string(), run.row, run.col))
            .collect();
        found.extend(
            detect_urls(&rows)
                .into_iter()
                .filter(|link| {
                    rows.get(link.row)
                        .and_then(|row| table.resolve(row, link.start_col))
                        .is_none()
                })
                .map(|link| (link.url, link.row, link.start_col)),
        );
        found
    }

    /// The URL under one cell of a session's grid, if any.
    ///
    /// v1's `url_at_click` (`src/app/mod.rs`), coordinates already converted to
    /// the grid: the OSC 8 table is asked first and the plain-text scan runs
    /// only when it declines, because for a rich-text link the escape is the
    /// *only* place the target exists — the screen holds nothing but the label.
    pub fn url_at(&self, session: &str, row: usize, col: usize) -> Option<String> {
        let (_, parser) = self.surface_parser(session)?;
        let parser = parser.lock().ok()?;
        let rows = self.cached_rows(session, &parser);
        rows.get(row)
            .and_then(|text| parser.callbacks().hyperlinks().resolve(text, col))
            .map(str::to_string)
            .or_else(|| {
                let detected = detect_urls(&rows);
                url_at_position(&detected, row, col).map(str::to_string)
            })
    }

    /// Every visible link on a surface, as cells already drawn into `buf`.
    ///
    /// The outer terminal is the only thing that can open a link when thurbox
    /// runs over ssh, and it knows nothing of ratatui's buffer — so v1 learned
    /// to re-print the runs it just painted wrapped in the escape
    /// ([`paint_hyperlinks`]). Reading the glyphs back out of the frame rather
    /// than off the vt100 grid is what makes a covering modal, a scrolled pane
    /// or a repainted row emit nothing instead of a link over cells that no
    /// longer show it.
    ///
    /// Both kinds of link ride it. OSC 8 runs are read from the table the
    /// parser callbacks keep, which costs nothing to consult. **Plain-text
    /// URLs** — what an agent prints far more often than it emits an escape —
    /// come in as `scanned`, the list the caller already maintains for
    /// `thurbox.links`: it is the same `Self::links` walk, paced by its own
    /// stamp and age, so serving this leg from it adds no scan to the frame.
    /// Handing them over matters as much as the runs do: with only the runs
    /// linked, a remote agent that printed a bare URL left the local terminal
    /// nothing to open, and `Ctrl+Click` — which thurbox itself answers — can
    /// only copy on a host with no browser.
    ///
    /// `scanned` is not fresh. Its age is bounded by the link-scan interval
    /// only while the agent is printing — `refresh_links` is gated on the
    /// session's **output** stamp, and scrolling the pane moves every row
    /// without producing output, so a scrolled screen can hold its pre-scroll
    /// positions for as long as the agent stays quiet. Checking each position
    /// against the cells the frame actually drew is therefore not a nicety, it
    /// is the whole of the correctness, and matching the URL's glyphs is not
    /// enough on its own — see `drawn_url_cells`, which is what makes a stale
    /// entry drop out rather than link the wrong target.
    pub fn hyperlink_paints(
        &self,
        session: &str,
        buf: &Buffer,
        scanned: &[(String, usize, usize)],
    ) -> Vec<HyperlinkPaint> {
        let Some((painted, parser)) = self.surface_parser(session) else {
            return Vec::new();
        };
        let Ok(parser) = parser.lock() else {
            return Vec::new();
        };
        // A session showing no link of either kind pays one bool check and one
        // slice length per frame, and nothing else.
        let runs_empty = parser.callbacks().hyperlinks().is_empty();
        if runs_empty && scanned.is_empty() {
            return Vec::new();
        }
        // This surface's own rect: the runs are re-printed over the cells that
        // drew them, which is only the right place while the rect belongs to
        // the pane whose grid they were read from.
        let inner = painted.rect.get();
        if inner.width == 0 || inner.height == 0 {
            return Vec::new();
        }

        let mut paints = Vec::new();
        if !runs_empty {
            let rows = self.cached_rows(session, &parser);
            for run in parser.callbacks().hyperlinks().visible_runs(&rows) {
                if run.row >= usize::from(inner.height) || run.col >= usize::from(inner.width) {
                    continue;
                }
                let x = inner.x.saturating_add(run.col as u16);
                let y = inner.y.saturating_add(run.row as u16);
                if let Some(cells) = drawn_label_cells(buf, inner, x, y, run.label) {
                    paints.push(HyperlinkPaint {
                        x,
                        y,
                        url: run.url.to_string(),
                        cells,
                    });
                }
            }
        }
        // The runs claim their cells first, and the plain leg is checked
        // against exactly those: two scanned URLs cannot collide with each
        // other (`Self::links` gives each row's runs disjoint columns and drops
        // a plain match underneath a visible OSC 8 one), so nothing is gained
        // by re-scanning what this loop itself appends.
        let claimed = paints.len();
        for (url, row, col) in scanned {
            // Counted on what goes OUT, not on what is read: `Self::links`
            // lists the OSC 8 runs first, and every one of them is declined a
            // few lines below, so a prefix cap would spend the whole budget on
            // entries that emit nothing and drop the bare URLs behind them.
            if paints.len() - claimed >= SCANNED_LINK_LIMIT {
                break;
            }
            if *row >= usize::from(inner.height) || *col >= usize::from(inner.width) {
                continue;
            }
            let x = inner.x.saturating_add(*col as u16);
            let y = inner.y.saturating_add(*row as u16);
            // Overlap, not an equal start. A run that opened PART-WAY through
            // printed URL text leaves the bare text in `scanned` starting to
            // its left, so comparing only the first cell let the plain leg
            // print over a run's cells — and, printed second, win. Whichever
            // link the escape declared is the one thurbox's own click resolves,
            // so the outer terminal must not be told a different one.
            if paints[..claimed].iter().any(|paint| {
                paint.y == y && paint.overlaps(x, UnicodeWidthStr::width(url.as_str()))
            }) {
                continue;
            }
            // A plain-text link's label is the URL itself: it is on the screen
            // as text, which is how it was found. An OSC 8 entry in `scanned`
            // whose label is NOT its target fails this and is skipped, having
            // been served by the loop above; one whose label IS its target — an
            // autolink — is what the overlap check above catches instead.
            if let Some(cells) = drawn_url_cells(buf, inner, x, y, url, || {
                parser.screen().row_wrapped(*row as u16)
            }) {
                paints.push(HyperlinkPaint {
                    x,
                    y,
                    url: url.clone(),
                    cells,
                });
            }
        }
        paints
    }

    /// Text inside a selection, read from the session's own vt100 grid.
    ///
    /// Reading the grid rather than the painted frame is what makes a selection
    /// made after scrolling copy the history you are looking at, and what
    /// rejoins a soft-wrapped line so a wrapped URL pastes intact.
    pub fn selected_text(
        &self,
        session: &str,
        selection: &crate::kernel::selection::Selection,
        pane_origin: (u16, u16),
    ) -> Option<String> {
        let (_, parser) = self.surface_parser(session)?;
        let parser = parser.lock().ok()?;
        Some(crate::kernel::selection::extract_text_from_screen(
            parser.screen(),
            selection,
            pane_origin,
        ))
    }
}

/// One run of already-drawn cells to re-print wrapped in an OSC 8 hyperlink.
///
/// The cells are carried rather than re-derived so the re-print reproduces the
/// frame exactly — same glyphs, same colours — and reads as a link only to the
/// terminal, never as a repaint to the eye.
#[derive(Debug, Clone, PartialEq)]
pub struct HyperlinkPaint {
    pub x: u16,
    pub y: u16,
    pub url: String,
    pub cells: Vec<(String, Style)>,
}

impl HyperlinkPaint {
    /// Cells this paint covers, which is not `cells.len()`: a wide glyph is one
    /// entry and two columns.
    fn width(&self) -> usize {
        self.cells
            .iter()
            .map(|(symbol, _)| UnicodeWidthStr::width(symbol.as_str()).max(1))
            .sum()
    }

    /// Whether this paint shares a column with a `width`-wide run starting at
    /// `x`. The caller has already matched the row.
    fn overlaps(&self, x: u16, width: usize) -> bool {
        let end = x.saturating_add(width as u16);
        let mine_end = self.x.saturating_add(self.width() as u16);
        x < mine_end && self.x < end
    }
}

/// The cells a plain-text URL occupies in the drawn frame, or `None` if what
/// the frame prints there is not that URL and only that URL.
///
/// [`drawn_label_cells`] is not enough on its own, because it walks the label
/// and stops — it never asks what comes *after*. For an OSC 8 run that is
/// right: the label is whatever the agent chose to print and abutting text is
/// none of its business. For a URL it is the whole question, since the run was
/// delimited by where the URL *ended*, and two ways of ending it are wrong:
///
/// * **The frame carries on.** The scanned list may be up to one link-scan
///   interval old, and an agent that has since appended to a URL leaves the
///   stale target a PREFIX of the printed one — `…/build/1` under cells now
///   reading `…/build/12`. That matches glyph for glyph, so the terminal would
///   have been handed a link to the wrong build. `wrapped` is consulted only
///   when the run reaches the pane's edge, so this costs nothing on the
///   ordinary row.
/// * **The row wrapped.** [`detect_urls`] scans a row at a time and a URL too
///   long for the pane is therefore found as its first half, the continuation
///   carrying no scheme to be found by. Linking that half offers a truncated
///   target, which is worse than the nothing it replaced — so a run that
///   reaches the edge of a row the screen soft-wrapped is dropped. A URL that
///   merely ends flush with the edge is kept, which is what `wrapped` is asked
///   to tell apart.
fn drawn_url_cells(
    buf: &Buffer,
    inner: Rect,
    x: u16,
    y: u16,
    url: &str,
    wrapped: impl FnOnce() -> bool,
) -> Option<Vec<(String, Style)>> {
    let cells = drawn_label_cells(buf, inner, x, y, url)?;
    let end = x.saturating_add(UnicodeWidthStr::width(url) as u16);
    // Clipped: `drawn_label_cells` stops at the pane's edge, so fewer cells
    // than the URL needs means the rest of it is not on this row.
    if end > inner.right() {
        return None;
    }
    if end == inner.right() {
        return (!wrapped()).then_some(cells);
    }
    let next = buf.cell(Position::new(end, y))?;
    let carries_on = next
        .symbol()
        .chars()
        .next()
        .is_some_and(|ch| !is_url_terminator(ch));
    (!carries_on).then_some(cells)
}

/// The cells `label` occupies in the drawn frame, or `None` if the frame no
/// longer prints it there.
///
/// v1's `helpers::drawn_label_cells`, including its asymmetry: a run clipped by
/// the pane's right edge is linked as far as it is visible (`break`), while a
/// glyph that does not match is the frame having moved on and drops the run
/// whole. Advancing by display width skips ratatui's filler cell after a wide
/// glyph, which is what re-printing that glyph does to the cursor.
fn drawn_label_cells(
    buf: &Buffer,
    inner: Rect,
    x: u16,
    y: u16,
    label: &str,
) -> Option<Vec<(String, Style)>> {
    let mut cells = Vec::new();
    let mut cx = x;
    for ch in label.chars() {
        let width = UnicodeWidthChar::width(ch).unwrap_or(1).max(1) as u16;
        if cx.saturating_add(width) > inner.right() {
            break;
        }
        let cell = buf.cell(Position::new(cx, y))?;
        if !cell.symbol().starts_with(ch) {
            return None;
        }
        cells.push((cell.symbol().to_string(), cell_style(cell)));
        cx = cx.saturating_add(width);
    }
    (!cells.is_empty()).then_some(cells)
}

/// The style a drawn cell carries, as re-printing it needs it back.
fn cell_style(cell: &ratatui::buffer::Cell) -> Style {
    Style::new()
        .fg(cell.fg)
        .bg(cell.bg)
        .add_modifier(cell.modifier)
}

/// One paint per drawn row of `rect`, so a node a plugin painted can be a link
/// the outer terminal opens. Every row names the same url, which is how a
/// wrapped link is spelled in OSC 8 anyway.
///
/// The pane counterpart of `drawn_label_cells`, and it differs in the one way
/// a pane differs from a vt100 grid: there is no label to match the glyphs
/// against, because the text lives in the plugin's tree and has already been
/// through wrapping, alignment and scroll by the time it reaches the frame. So
/// the frame is the source of truth outright — whatever the node's rect
/// actually shows is what gets linked, which keeps the property that a node
/// clipped by its pane or drawn nowhere contributes nothing.
///
/// Blank cells are trimmed from either end because a pane indents its text: the
/// rect a node was given is wider than the glyphs in it, and linking the
/// padding would put the underline (and the click target the emulator draws)
/// across the whole row. Interior blanks stay — they are inside the label.
///
/// The walk advances by each cell's DISPLAY WIDTH, for the reason
/// `drawn_label_cells` does: re-printing a wide glyph moves the cursor over both
/// its columns itself, so the filler ratatui leaves beside it must not be
/// printed as well. Reading that filler as "the cell is empty" is not enough —
/// ratatui writes a BLANK there, which is indistinguishable from a space inside
/// the label — and a printed row one column longer than the cells it came from
/// shifts every glyph after the first wide one. Out here that damage is
/// permanent: this print is outside the frame diff, so the next frame repaints
/// only the cells it thinks moved and the shifted glyphs stay.
pub fn drawn_link_paints(buf: &Buffer, rect: Rect, url: &str) -> Vec<HyperlinkPaint> {
    let mut paints = Vec::new();
    for y in rect.top()..rect.bottom() {
        // The first glyph's column, and `None` while the row has shown none:
        // only a non-blank cell sets it, so a row the plugin left blank yields
        // no link at all rather than a link with no text in it.
        let mut start = None;
        let mut cells: Vec<(String, Style)> = Vec::new();
        let mut x = rect.left();
        while x < rect.right() {
            // Outside the frame entirely: nothing further along this row is
            // drawn either.
            let Some(cell) = buf.cell(Position::new(x, y)) else {
                break;
            };
            let symbol = cell.symbol();
            // A width of zero is a cell a wide glyph already covered, so it
            // cannot advance the walk on its own.
            let width = UnicodeWidthStr::width(symbol).max(1) as u16;
            x = x.saturating_add(width);
            if symbol.is_empty() {
                continue;
            }
            if start.is_none() && symbol.trim().is_empty() {
                continue;
            }
            start.get_or_insert(x.saturating_sub(width));
            cells.push((symbol.to_string(), cell_style(cell)));
        }
        // Trailing padding, trimmed the way the leading padding was skipped.
        // It cannot empty `cells`: the cell that set `start` is not blank.
        while cells
            .last()
            .is_some_and(|(symbol, _)| symbol.trim().is_empty())
        {
            cells.pop();
        }
        if let Some(x) = start {
            paints.push(HyperlinkPaint {
                x,
                y,
                url: url.to_string(),
                cells,
            });
        }
    }
    paints
}

/// Re-print each run wrapped in OSC 8, so the terminal thurbox itself runs in
/// can open it.
///
/// Written straight to stdout *after* the backend has flushed the frame, so it
/// cannot interleave with ratatui's own output.
///
/// Bracketed in DECSC/DECRC, because the frame has already placed the caret and
/// this walks it away. `draw` positions the caret last and it stays *shown*, so
/// a focused text field's caret was left sitting wherever the final link run
/// ended — and, since the loop repaints on the forced-redraw floor, it jumped
/// back to the field and away again several times a second. That reads as a
/// cursor blinking in the wrong place rather than as one that moved, which is
/// why it looked like a rendering fault. Restoring here rather than leaving it
/// to the next `draw` keeps the two independent: any number of frames may pass
/// before the next one, and every one of them is a frame with a stray caret.
pub fn paint_hyperlinks(paints: &[HyperlinkPaint]) -> std::io::Result<()> {
    use crossterm::cursor::{MoveTo, RestorePosition, SavePosition};
    use crossterm::queue;
    use crossterm::style::{Print, PrintStyledContent, ResetColor};
    use ratatui::backend::IntoCrossterm;
    use std::io::Write;

    let mut out = std::io::stdout();
    queue!(out, SavePosition)?;
    for paint in paints {
        queue!(
            out,
            MoveTo(paint.x, paint.y),
            Print(crate::session::hyperlink::osc8_open(&paint.url))
        )?;
        for (symbol, style) in &paint.cells {
            let content = (*style).into_crossterm();
            queue!(out, PrintStyledContent(content.apply(symbol.as_str())))?;
        }
        queue!(
            out,
            Print(crate::session::hyperlink::OSC8_CLOSE),
            ResetColor
        )?;
    }
    queue!(out, RestorePosition)?;
    out.flush()
}

/// Blank the rect only where the grid will not reach it.
///
/// The frame buffer is reset by `swap_buffers` before every draw, so there
/// is nothing stale to erase across frames — a `Clear` over the whole rect
/// was a second full-grid write for cells the terminal widget is about to
/// overwrite anyway. It is still owed when the grid is *smaller* than its
/// rect, which happens for a frame after a resize while the pane catches up.
pub(super) fn clear_uncovered(frame: &mut Frame, area: Rect, screen: &vt100::Screen) {
    let (rows, cols) = screen.size();
    if rows < area.height || cols < area.width {
        frame.render_widget(Clear, area);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    #[test]
    fn detect_https_url() {
        let rows = vec!["Visit https://example.com for info".to_string()];
        let links = detect_urls(&rows);
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].url, "https://example.com");
        assert_eq!(links[0].row, 0);
        assert_eq!(links[0].start_col, 6);
        assert_eq!(links[0].end_col, 25);
    }

    #[test]
    fn detect_http_url() {
        let rows = vec!["http://example.org/path?q=1".to_string()];
        let links = detect_urls(&rows);
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].url, "http://example.org/path?q=1");
    }

    #[test]
    fn strip_trailing_punctuation() {
        let rows = vec!["See https://example.com/page.".to_string()];
        let links = detect_urls(&rows);
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].url, "https://example.com/page");
    }

    #[test]
    fn strip_trailing_paren() {
        let rows = vec!["(https://example.com)".to_string()];
        let links = detect_urls(&rows);
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].url, "https://example.com");
    }

    #[test]
    fn multiple_urls_on_one_row() {
        let rows = vec!["https://a.com and https://b.com here".to_string()];
        let links = detect_urls(&rows);
        assert_eq!(links.len(), 2);
        assert_eq!(links[0].url, "https://a.com");
        assert_eq!(links[1].url, "https://b.com");
    }

    #[test]
    fn no_urls() {
        let rows = vec!["no links here".to_string()];
        let links = detect_urls(&rows);
        assert!(links.is_empty());
    }

    #[test]
    fn url_at_position_hit() {
        let rows = vec!["Visit https://example.com for info".to_string()];
        let links = detect_urls(&rows);
        assert_eq!(url_at_position(&links, 0, 6), Some("https://example.com"));
        // last char of URL (end_col is exclusive)
        assert_eq!(url_at_position(&links, 0, 24), Some("https://example.com"));
    }

    #[test]
    fn url_at_position_miss() {
        let rows = vec!["Visit https://example.com for info".to_string()];
        let links = detect_urls(&rows);
        assert_eq!(url_at_position(&links, 0, 0), None);
        assert_eq!(url_at_position(&links, 0, 25), None);
        assert_eq!(url_at_position(&links, 1, 10), None);
    }

    #[test]
    fn file_url_detected() {
        let rows = vec!["file:///home/user/doc.txt".to_string()];
        let links = detect_urls(&rows);
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].url, "file:///home/user/doc.txt");
    }

    #[test]
    fn preserves_path_components() {
        let rows = vec!["https://example.com/a/b/c?x=1&y=2#frag".to_string()];
        let links = detect_urls(&rows);
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].url, "https://example.com/a/b/c?x=1&y=2#frag");
    }

    #[test]
    fn empty_rows() {
        let links = detect_urls(&[]);
        assert!(links.is_empty());
    }

    #[test]
    fn url_at_position_empty_links() {
        assert_eq!(url_at_position(&[], 0, 0), None);
    }

    #[test]
    fn strip_multiple_trailing_punctuation() {
        let rows = vec!["https://example.com);;".to_string()];
        let links = detect_urls(&rows);
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].url, "https://example.com");
    }

    #[test]
    fn url_at_start_of_row() {
        let rows = vec!["https://example.com".to_string()];
        let links = detect_urls(&rows);
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].start_col, 0);
    }

    #[test]
    fn urls_across_multiple_rows() {
        let rows = vec![
            "first https://a.com here".to_string(),
            "no url line".to_string(),
            "last https://b.com end".to_string(),
        ];
        let links = detect_urls(&rows);
        assert_eq!(links.len(), 2);
        assert_eq!(links[0].row, 0);
        assert_eq!(links[0].url, "https://a.com");
        assert_eq!(links[1].row, 2);
        assert_eq!(links[1].url, "https://b.com");
    }

    #[test]
    fn bare_scheme_rejected() {
        let rows = vec!["see https:// here".to_string()];
        let links = detect_urls(&rows);
        assert!(links.is_empty());
    }

    #[test]
    fn strip_trailing_colon_and_bracket() {
        let rows = vec!["[https://example.com/path]:".to_string()];
        let links = detect_urls(&rows);
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].url, "https://example.com/path");
    }

    #[test]
    fn non_ascii_before_url_uses_char_offsets() {
        // '•' is 3 bytes in UTF-8 but 1 screen cell — byte offset would be 4, char offset is 2
        let rows = vec!["• https://example.com".to_string()];
        let links = detect_urls(&rows);
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].start_col, 2);
        assert_eq!(links[0].end_col, 21);
        assert_eq!(url_at_position(&links, 0, 2), Some("https://example.com"));
        assert_eq!(url_at_position(&links, 0, 1), None);
    }

    #[test]
    fn wide_chars_before_url_use_display_width() {
        // Each CJK glyph spans two cells; the URL therefore starts at column 5
        // (漢=2 + 字=2 + space=1), not column 3 as a per-char count would give.
        let rows = vec!["漢字 https://example.com".to_string()];
        let links = detect_urls(&rows);
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].url, "https://example.com");
        assert_eq!(links[0].start_col, 5);
        assert_eq!(links[0].end_col, 5 + "https://example.com".len());
        assert_eq!(url_at_position(&links, 0, 5), Some("https://example.com"));
        // The trailing cell of 字 (column 4) is not part of the URL.
        assert_eq!(url_at_position(&links, 0, 4), None);
    }

    #[test]
    fn emoji_before_url_uses_display_width() {
        // 🚀 is one char but two cells wide; URL starts at column 3 (🚀=2 + space).
        let rows = vec!["🚀 https://example.com".to_string()];
        let links = detect_urls(&rows);
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].start_col, 3);
        assert_eq!(url_at_position(&links, 0, 3), Some("https://example.com"));
    }

    #[test]
    fn wide_chars_through_parser_match_cell_columns() {
        // End-to-end over the real screen path: extract_screen_rows must agree
        // with the cell columns that a mouse click reports.
        let mut parser = vt100::Parser::new(1, 40, 0);
        parser.process("漢字 https://example.com".as_bytes());
        let rows = extract_screen_rows(parser.screen());
        let links = detect_urls(&rows);
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].start_col, 5);
        assert_eq!(url_at_position(&links, 0, 5), Some("https://example.com"));
        assert_eq!(url_at_position(&links, 0, 4), None);
    }

    #[test]
    fn extract_screen_rows_from_parser() {
        let mut parser = vt100::Parser::new(2, 10, 0);
        parser.process(b"hello");
        let rows = extract_screen_rows(parser.screen());
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0], "hello     ");
        assert_eq!(rows[1], "          ");
    }

    /// Paint `x` into the top-left cell, run `clear_uncovered` over the whole
    /// rect for a grid of `grid`, and report whether the cell survived.
    fn survives_clear(rect: (u16, u16), grid: (u16, u16)) -> bool {
        let (cols, rows) = rect;
        let mut terminal = Terminal::new(TestBackend::new(cols, rows)).expect("terminal");
        let parser = vt100::Parser::new(grid.1, grid.0, 0);
        terminal
            .draw(|frame| {
                frame.buffer_mut()[(0, 0)].set_symbol("x");
                clear_uncovered(frame, Rect::new(0, 0, cols, rows), parser.screen());
            })
            .expect("draw");
        terminal.backend().buffer()[(0, 0)].symbol() == "x"
    }

    /// The optimisation itself: a grid that covers its rect is about to
    /// overwrite every cell, so clearing first was a second full-grid write for
    /// nothing (ADR-P17). Asserted by leaving a mark the clear would erase.
    #[test]
    fn a_grid_that_covers_its_rect_is_not_cleared() {
        assert!(
            survives_clear((10, 4), (10, 4)),
            "a covering grid still triggered a clear"
        );
    }

    /// And the case that keeps it correct. A pane lags a resize by a frame, so
    /// the grid can be smaller than the rect it is painted into; without the
    /// clear the rows it never reaches would show whatever the layout left
    /// underneath.
    #[test]
    fn a_grid_shorter_than_its_rect_still_clears() {
        assert!(
            !survives_clear((10, 4), (10, 2)),
            "a grid with fewer rows than its rect left the uncovered part unclear"
        );
    }

    #[test]
    fn a_grid_narrower_than_its_rect_still_clears() {
        assert!(
            !survives_clear((10, 4), (6, 4)),
            "a grid with fewer columns than its rect left the uncovered part unclear"
        );
    }

    /// A buffer with `text` written into row `y` starting at column 0, the rest
    /// left as the blanks a pane's padding is.
    fn buffer_with(width: u16, height: u16, rows: &[&str]) -> Buffer {
        let mut buf = Buffer::empty(Rect::new(0, 0, width, height));
        for (y, text) in rows.iter().enumerate() {
            buf.set_string(0, y as u16, text, Style::default());
        }
        buf
    }

    /// [`drawn_url_cells`] over a one-or-two-row pane, with `wrapped` standing
    /// in for what the vt100 screen would answer for that row.
    fn url_cells(
        buf: &Buffer,
        width: u16,
        x: u16,
        url: &str,
        wrapped: bool,
    ) -> Option<Vec<(String, Style)>> {
        drawn_url_cells(
            buf,
            Rect::new(0, 0, width, buf.area.height),
            x,
            0,
            url,
            || wrapped,
        )
    }

    /// The glyphs, and only the glyphs: a pane indents its text, and linking a
    /// node's whole rect would underline the padding either side of it.
    #[test]
    fn a_nodes_drawn_cells_are_the_link_and_its_padding_is_not() {
        let buf = buffer_with(30, 1, &["  https://example.test/a   "]);
        let paints = drawn_link_paints(&buf, Rect::new(0, 0, 30, 1), "https://example.test/a");

        assert_eq!(paints.len(), 1);
        let paint = &paints[0];
        assert_eq!((paint.x, paint.y), (2, 0));
        assert_eq!(paint.url, "https://example.test/a");
        let printed: String = paint
            .cells
            .iter()
            .map(|(symbol, _)| symbol.as_str())
            .collect();
        assert_eq!(printed, "https://example.test/a");
    }

    /// The plain leg's whole basis: a plain-text URL is its own label, so
    /// matching the URL string against the cells the frame drew at the column
    /// the scan reported is a real check — those cells ARE the URL, it was
    /// found by reading them.
    #[test]
    fn a_plain_url_matches_the_cells_it_was_read_from() {
        let buf = buffer_with(40, 1, &["see https://example.test/a now"]);
        let cells = url_cells(&buf, 40, 4, "https://example.test/a", false)
            .expect("the frame still prints the URL there");
        let printed: String = cells.iter().map(|(symbol, _)| symbol.as_str()).collect();
        assert_eq!(printed, "https://example.test/a");
    }

    /// A row the agent has since repainted yields nothing rather than a link
    /// over whatever text now sits at that column.
    #[test]
    fn a_url_the_frame_no_longer_prints_yields_no_cells() {
        let buf = buffer_with(40, 1, &["see something else entirely now"]);
        assert!(url_cells(&buf, 40, 4, "https://example.test/a", false).is_none());
    }

    /// The stale case that a glyph-for-glyph match does NOT catch, and the
    /// reason the check looks one cell past the end. The scan caught a URL
    /// mid-print; by the time this frame is drawn the agent has appended to it,
    /// leaving the recorded target a prefix of what the cells now read. Every
    /// glyph of the prefix still matches, so without the boundary the terminal
    /// is handed a link to the wrong build.
    #[test]
    fn a_url_the_frame_has_grown_past_yields_no_cells() {
        let buf = buffer_with(40, 1, &["see https://ci.test/build/12 now"]);
        assert!(url_cells(&buf, 40, 4, "https://ci.test/build/1", false).is_none());
    }

    /// And the other end of that boundary: ending on a terminator is how a URL
    /// run was delimited in the first place, so a `)` or a space after it is
    /// the run ending, not the frame carrying on.
    #[test]
    fn a_url_followed_by_a_terminator_still_matches() {
        let buf = buffer_with(40, 1, &["see (https://example.test/a) now"]);
        assert!(url_cells(&buf, 40, 5, "https://example.test/a", false).is_some());
    }

    /// A URL too long for the pane is found by `detect_urls` as its first half
    /// — the scan reads one row at a time and the continuation carries no
    /// scheme. Linking that half would offer the outer terminal a TRUNCATED
    /// target, which is worse than the nothing it replaced, so a run reaching
    /// the edge of a soft-wrapped row is dropped.
    #[test]
    fn a_url_the_row_wrapped_under_yields_no_cells() {
        let buf = buffer_with(24, 2, &["see https://example.test", "/a/long/path"]);
        assert!(url_cells(&buf, 24, 4, "https://example.test", true).is_none());
    }

    /// The case that distinguishes: the same run at the same edge, on a row
    /// that did not wrap, is a URL that merely ends flush with the pane and is
    /// linked in full.
    #[test]
    fn a_url_ending_flush_with_the_edge_still_matches() {
        let buf = buffer_with(24, 2, &["see https://example.test", ""]);
        assert!(url_cells(&buf, 24, 4, "https://example.test", false).is_some());
    }

    /// An OSC 8 run reaches the plain-text leg too — `Terminals::links` lists
    /// both kinds as `(url, row, col)` with no tag saying which. The leg needs
    /// no tag: a run's label is not its target, so matching the target against
    /// the drawn cells declines it, and the run keeps the paint the OSC 8 leg
    /// already made for it.
    #[test]
    fn a_run_whose_label_is_not_its_target_declines_the_plain_leg() {
        let buf = buffer_with(40, 1, &["see Github now"]);
        assert!(url_cells(&buf, 40, 4, "https://github.com", false).is_none());
    }

    /// The run that check does NOT decline, and what stops it printing twice.
    /// An autolink — `OSC 8 ; ; <url>` around the URL itself, which is how
    /// agents render a bare link in markdown — has a label equal to its target,
    /// so the plain leg would match it happily. Overlap with the cells the OSC
    /// 8 leg already claimed is what skips it, and it is an overlap rather than
    /// an equal start because a run that opened part-way through printed URL
    /// text starts to the right of the bare text under it.
    #[test]
    fn a_paint_knows_the_columns_it_covers() {
        let buf = buffer_with(40, 1, &["see https://example.test/a now"]);
        let bare = (4u16, UnicodeWidthStr::width("https://example.test/a"));
        let paint = |x: u16, label: &str| HyperlinkPaint {
            x,
            y: 0,
            url: "https://tracker.test/1".to_string(),
            cells: drawn_label_cells(&buf, Rect::new(0, 0, 40, 1), x, 0, label).expect("cells"),
        };

        // The autolink — label equal to target, so the glyph match accepts it
        // and only this covers it. An equal-start check would have caught this
        // one too.
        assert!(paint(bare.0, "https://example.test/a").overlaps(bare.0, bare.1));
        // The run opened PART-WAY through the printed URL, which an equal-start
        // check would not have: it claims from column 12 while the bare text
        // the scan found starts at 4.
        assert!(paint(12, "example.test/a").overlaps(bare.0, bare.1));
        // And the neighbours either side, which genuinely do not touch it.
        assert!(!paint(27, "now").overlaps(bare.0, bare.1));
        assert!(!paint(0, "see").overlaps(bare.0, bare.1));
    }

    /// Interior blanks are inside the label, so a linked button keeps its own
    /// spacing — only the ends are trimmed.
    #[test]
    fn interior_blanks_stay_inside_the_label() {
        let buf = buffer_with(20, 1, &[" Open MR !123 "]);
        let paints = drawn_link_paints(&buf, Rect::new(0, 0, 20, 1), "https://example.test/1");

        assert_eq!(paints.len(), 1);
        assert_eq!(paints[0].x, 1);
        let printed: String = paints[0]
            .cells
            .iter()
            .map(|(symbol, _)| symbol.as_str())
            .collect();
        assert_eq!(printed, "Open MR !123");
    }

    /// A row the plugin left blank is not a link with no text in it — it is not
    /// a link. Same for a rect the frame is too small to hold.
    #[test]
    fn a_blank_row_yields_no_link() {
        let buf = buffer_with(20, 2, &["   ", "  x"]);
        // Row 0 is blank: only row 1 contributes.
        let paints = drawn_link_paints(&buf, Rect::new(0, 0, 20, 2), "https://example.test");
        assert_eq!(paints.len(), 1);
        assert_eq!((paints[0].x, paints[0].y), (2, 1));

        assert!(drawn_link_paints(&buf, Rect::new(0, 0, 20, 0), "u").is_empty());
        assert!(drawn_link_paints(&buf, Rect::new(0, 5, 20, 1), "u").is_empty());
    }

    /// A wide glyph is one cell and two columns, and ratatui leaves a BLANK in
    /// the second. Re-printing that blank puts a space inside the label and
    /// shifts every glyph after it one column right — the row this print lands
    /// on is then permanently out of step with the frame the diff believes it
    /// painted.
    #[test]
    fn a_wide_glyph_does_not_re_print_the_filler_beside_it() {
        let buf = buffer_with(20, 1, &["docs 漢字 x"]);
        let paints = drawn_link_paints(&buf, Rect::new(0, 0, 20, 1), "https://example.test/w");

        assert_eq!(paints.len(), 1);
        assert_eq!(paints[0].x, 0);
        let printed: String = paints[0]
            .cells
            .iter()
            .map(|(symbol, _)| symbol.as_str())
            .collect();
        assert_eq!(printed, "docs 漢字 x");
        // The genuine space between the glyphs and `x` survives: only the cells
        // a wide glyph already covers are skipped.
        assert_eq!(paints[0].cells.len(), "docs 漢字 x".chars().count());
    }

    /// One paint per row, all naming the same url: that is how a wrapped link is
    /// spelled in OSC 8, and a multi-row node is the same shape.
    #[test]
    fn every_drawn_row_of_a_node_carries_the_same_url() {
        let buf = buffer_with(20, 2, &["  first", "  second"]);
        let paints = drawn_link_paints(&buf, Rect::new(0, 0, 20, 2), "https://example.test");

        assert_eq!(paints.len(), 2);
        assert!(paints.iter().all(|p| p.url == "https://example.test"));
        assert_eq!(paints[0].y, 0);
        assert_eq!(paints[1].y, 1);
    }
}
