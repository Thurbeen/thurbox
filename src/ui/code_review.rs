//! Native renderer for the code-review view (the diff + interleaved comments +
//! summary + an in-view compose box), modeled on the file viewer. Pure
//! rendering: it returns click/scroll hitboxes for the app layer to record.

use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph};
use ratatui::Frame;

use crate::app::code_review::{CodeReviewState, ComposeState, ReviewButton, ReviewRow};
use crate::session::review::{Classification, CommentAnchor, DiffFile, DiffLine, DiffLineKind};
use crate::ui::scrollbar::{self, ScrollbarGeom};
use crate::ui::theme::Theme;
use crate::ui::{focus_block, render_button_bar, ButtonSpec, FocusLevel, RowHitbox};

/// What the renderer hands back for the app layer to record as click/scroll
/// targets this frame.
pub(crate) struct CodeReviewHits {
    /// One hitbox per visible diff/comment row (index = row in `state.rows`).
    pub rows: Vec<RowHitbox>,
    /// Footer buttons paired with the action each triggers.
    pub buttons: Vec<(crate::ui::ButtonHit, ReviewButton)>,
    pub scrollbar: Option<ScrollbarGeom>,
}

/// Theme color for a classification badge.
fn class_color(c: Classification) -> Color {
    match c {
        Classification::Issue => Theme::danger(),
        Classification::Suggestion => Theme::accent(),
        Classification::Note => Theme::text_secondary(),
        Classification::Praise => Theme::status_done(),
    }
}

/// Theme color for a file's status glyph (M/A/D/R): added green, deleted red,
/// modified yellow, renamed accent — so the status reads at a glance.
fn status_color(s: crate::session::review::FileStatus) -> Color {
    use crate::session::review::FileStatus::*;
    match s {
        Added => Theme::diff_added(),
        Deleted => Theme::diff_removed(),
        Modified => Theme::status_working(),
        Renamed => Theme::accent(),
    }
}

pub(crate) fn render(
    frame: &mut Frame,
    area: Rect,
    state: &mut CodeReviewState,
    level: FocusLevel,
) -> CodeReviewHits {
    let (add, del) = state.totals();
    let target = state.target.label(&state.repos, &state.commits);
    let title = format!(" Code review · {target}  +{add} -{del} ");
    // Right-aligned so the app-layer central-pane tab strip (Agent/Shell/Review)
    // overlaid on the left of this top border has room.
    let block = focus_block("", level)
        .title_top(Line::from(Span::styled(title, crate::ui::title_style(level))).right_aligned());
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if inner.height == 0 || inner.width == 0 {
        return CodeReviewHits {
            rows: Vec::new(),
            buttons: Vec::new(),
            scrollbar: None,
        };
    }

    // Footer (buttons) is always the last row; the diff uses the rest. A search
    // bar, when open, takes the top row of what's left.
    let footer = Rect::new(inner.x, inner.y + inner.height - 1, inner.width, 1);
    let mut diff_area = Rect::new(inner.x, inner.y, inner.width, inner.height - 1);
    if state.search.is_some() && diff_area.height > 1 {
        let search_row = Rect::new(diff_area.x, diff_area.y, diff_area.width, 1);
        diff_area = Rect::new(
            diff_area.x,
            diff_area.y + 1,
            diff_area.width,
            diff_area.height - 1,
        );
        render_search_bar(frame, search_row, state);
    }

    let composing = state.compose.is_some();
    // The target picker, when open, replaces the diff body (keyboard-driven).
    let hits = if state.target_picker.is_some() {
        render_target_picker(frame, diff_area, state);
        (Vec::new(), None)
    } else {
        render_rows(frame, diff_area, state)
    };
    // The compose box floats inline at the selected line (its screen row from
    // the hitboxes), not pinned to the bottom — GitHub-style line comments.
    if composing {
        if let Some(comp) = state.compose.as_ref() {
            let anchor_y = hits
                .0
                .iter()
                .find(|h| h.index == state.selected)
                .map(|h| h.rect.y);
            render_compose_inline(frame, diff_area, anchor_y, comp);
        }
    }
    let buttons = render_footer(frame, footer, composing, state.side_by_side);

    CodeReviewHits {
        rows: hits.0,
        buttons,
        scrollbar: hits.1,
    }
}

/// Render the review-target picker (branch / working / per-commit) into `area`.
fn render_target_picker(frame: &mut Frame, area: Rect, state: &CodeReviewState) {
    let Some(picker) = state.target_picker.as_ref() else {
        return;
    };
    let mut lines: Vec<Line> = vec![Line::from(Span::styled(
        " Review target  (↑/↓ select · Enter · Esc)",
        Style::default().fg(Theme::text_muted()),
    ))];
    let height = area.height as usize;
    for (i, target) in picker
        .entries
        .iter()
        .enumerate()
        .take(height.saturating_sub(1))
    {
        let selected = i == picker.selected;
        let marker = if selected { "▸ " } else { "  " };
        let label = target.label(&state.repos, &state.commits);
        let style = if selected {
            Style::default()
                .fg(Theme::selection_fg())
                .bg(Theme::selection_bg())
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Theme::text_primary())
        };
        lines.push(Line::from(Span::styled(
            truncate(&format!("{marker}{label}"), area.width as usize),
            style,
        )));
    }
    frame.render_widget(Paragraph::new(lines), area);
}

/// Render the windowed diff/comment rows + scrollbar. Returns row hitboxes
/// (index in `state.rows`) and the scrollbar geometry.
fn render_rows(
    frame: &mut Frame,
    area: Rect,
    state: &mut CodeReviewState,
) -> (Vec<RowHitbox>, Option<ScrollbarGeom>) {
    let total = state.rows.len();
    let height = area.height as usize;
    if height == 0 {
        return (Vec::new(), None);
    }

    // Clamp scroll so the selection stays visible (the nav layer set a lower
    // bound; here we enforce the upper edge given the known height).
    if state.selected >= state.scroll + height {
        state.scroll = state.selected + 1 - height;
    }
    if state.scroll + height > total {
        state.scroll = total.saturating_sub(height);
    }
    if state.selected < state.scroll {
        state.scroll = state.selected;
    }
    let start = state.scroll;
    let end = (start + height).min(total);

    let (content, track) = scrollbar::reserve_track(area, total, height);

    // Line-number column width from the largest number on screen.
    let num_w = line_number_width(state);

    // The active search query (lowercased) drives the in-row match highlight.
    let query = state
        .search
        .as_ref()
        .map(|s| s.query.to_lowercase())
        .filter(|q| !q.trim().is_empty());

    let mut lines: Vec<Line> = Vec::with_capacity(end - start);
    for i in start..end {
        lines.push(row_line(
            state,
            i,
            content.width as usize,
            num_w,
            query.as_deref(),
        ));
    }
    frame.render_widget(Paragraph::new(lines), content);

    // The view is selection-primary: the thumb tracks `selected` (which reaches
    // the last row, `total - 1`), not `scroll` (which caps at `total - height`,
    // so a thumb driven by it could never reach the bottom of the track). This
    // also matches the drag mapping — `position_for_y` returns a `0..total`
    // index that `apply_scrollbar_position` feeds straight to `cr_select_row`.
    let geom = track.and_then(|t| scrollbar::render_into(frame, t, total, height, state.selected));

    let hitboxes = (start..end)
        .enumerate()
        .map(|(row, index)| RowHitbox {
            rect: Rect::new(content.x, content.y + row as u16, content.width, 1),
            index,
        })
        .collect();
    (hitboxes, geom)
}

/// Width for the diff line-number gutter (two numbers, old+new).
fn line_number_width(state: &CodeReviewState) -> usize {
    let max = state
        .files
        .iter()
        .flat_map(|f| f.hunks.iter())
        .map(|h| h.new_start as usize + h.lines.len())
        .max()
        .unwrap_or(0);
    max.to_string().len().max(2)
}

/// Build the styled `Line` for row `i`. `query` (lowercased, non-empty) is the
/// active find-in-diff search, used to highlight literal match runs in the row.
fn row_line<'a>(
    state: &CodeReviewState,
    i: usize,
    width: usize,
    num_w: usize,
    query: Option<&str>,
) -> Line<'a> {
    let selected = i == state.selected;
    let sel_style = |base: Style| {
        if selected {
            base.bg(Theme::selection_bg()).fg(Theme::selection_fg())
        } else {
            base
        }
    };

    match &state.rows[i] {
        ReviewRow::FileHeader(fi) => file_header_line(state, *fi, width, query, &sel_style),
        ReviewRow::HunkHeader(fi, hi) => {
            hunk_header_line(state, *fi, *hi, width, query, &sel_style)
        }
        ReviewRow::Line(fi, hi, li) => {
            let f = &state.files[*fi];
            let l = &f.hunks[*hi].lines[*li];
            if state.side_by_side {
                side_by_side_line(l, width, num_w, &sel_style)
            } else {
                unified_diff_line(f, l, width, num_w, selected, query, &sel_style)
            }
        }
        ReviewRow::Comment(id) | ReviewRow::Summary(id) => {
            comment_line(state, *id, width, query, sel_style)
        }
        ReviewRow::SummaryHeader => Line::from(Span::styled(
            truncate("── Review summary (s to add) ──", width),
            sel_style(
                Style::default()
                    .fg(Theme::accent())
                    .add_modifier(Modifier::BOLD),
            ),
        )),
        ReviewRow::Info(text) => Line::from(Span::styled(
            truncate(text, width),
            Style::default().fg(Theme::text_muted()),
        )),
    }
}

/// A file header: a full-width rule with the path embedded (tuicr-style file
/// separator), a fold chevron, and the status glyph + add/remove counts tinted
/// with the diff colors so the status reads at a glance.
fn file_header_line<'a>(
    state: &CodeReviewState,
    fi: usize,
    width: usize,
    query: Option<&str>,
    sel_style: &impl Fn(Style) -> Style,
) -> Line<'a> {
    let f = &state.files[fi];
    let chevron = if state.is_file_folded(&f.path) {
        "▸"
    } else {
        "▾"
    };
    let header = Style::default()
        .fg(Theme::accent_bright())
        .add_modifier(Modifier::BOLD);
    let lead = format!("{chevron} ");
    let glyph = f.status.glyph().to_string();
    let mid = format!(" {}  ", f.path);
    let adds = format!("+{}", f.added_count());
    let dels = format!(" -{}", f.deleted_count());
    let mark = if state.reviewed_files.contains(&f.path) {
        " ✓"
    } else {
        ""
    };
    let used = lead.chars().count()
        + glyph.chars().count()
        + mid.chars().count()
        + adds.chars().count()
        + dels.chars().count()
        + mark.chars().count()
        + 1; // trailing space before the rule
    let pad = if used < width {
        "─".repeat(width - used)
    } else {
        String::new()
    };
    let mut spans = vec![
        Span::styled(lead, sel_style(header)),
        Span::styled(
            glyph,
            sel_style(
                Style::default()
                    .fg(status_color(f.status))
                    .add_modifier(Modifier::BOLD),
            ),
        ),
    ];
    spans.extend(highlight_text(mid, sel_style(header), query));
    spans.extend([
        Span::styled(adds, sel_style(Style::default().fg(Theme::diff_added()))),
        Span::styled(dels, sel_style(Style::default().fg(Theme::diff_removed()))),
    ]);
    if !mark.is_empty() {
        spans.push(Span::styled(
            mark.to_string(),
            sel_style(Style::default().fg(Theme::status_done())),
        ));
    }
    spans.push(Span::styled(format!(" {pad}"), sel_style(header)));
    Line::from(spans)
}

/// A hunk header: the `@@ -a,b +c,d @@` ranges (recomputed from the lines) + the
/// section heading, with a `✓` when the hunk is marked reviewed.
fn hunk_header_line<'a>(
    state: &CodeReviewState,
    fi: usize,
    hi: usize,
    width: usize,
    query: Option<&str>,
    sel_style: &impl Fn(Style) -> Style,
) -> Line<'a> {
    let f = &state.files[fi];
    let h = &f.hunks[hi];
    let mark = if state.reviewed_hunks.contains(&(f.path.clone(), hi)) {
        " ✓"
    } else {
        ""
    };
    // Old-side span = lines present on the old side (context + deletions);
    // new-side span = lines present on the new side (context + additions).
    let old_span = h.lines.iter().filter(|l| l.old_no.is_some()).count();
    let new_span = h.lines.iter().filter(|l| l.new_no.is_some()).count();
    let text = format!(
        "  @@ -{},{} +{},{} @@ {}{}",
        h.old_start, old_span, h.new_start, new_span, h.header, mark
    );
    let base = sel_style(
        Style::default()
            .fg(Theme::accent())
            .add_modifier(Modifier::DIM),
    );
    Line::from(highlight_text(truncate(&text, width), base, query))
}

/// A unified-diff line: a `old new ±` gutter plus the syntax-highlighted body,
/// with the add/remove row tint (the gutter sign + tint carry the +/-, leaving
/// the text free for syntax colour). Truncated/padded to `width`.
fn unified_diff_line<'a>(
    f: &DiffFile,
    l: &DiffLine,
    width: usize,
    num_w: usize,
    selected: bool,
    query: Option<&str>,
    sel_style: &impl Fn(Style) -> Style,
) -> Line<'a> {
    let (sign, row_bg) = match l.kind {
        DiffLineKind::Add => ('+', Some(Theme::diff_added_bg())),
        DiffLineKind::Del => ('-', Some(Theme::diff_removed_bg())),
        DiffLineKind::Context => (' ', None),
    };
    // Row tint under everything (selection wins on the cursor row).
    let bg = |s: Style| match row_bg {
        Some(c) if !selected => s.bg(c),
        _ => s,
    };
    let old = l.old_no.map(|n| n.to_string()).unwrap_or_default();
    let new = l.new_no.map(|n| n.to_string()).unwrap_or_default();
    let gutter = format!("{old:>num_w$} {new:>num_w$} {sign} ");
    let avail = width.saturating_sub(gutter.chars().count());

    let mut spans = vec![Span::styled(
        gutter,
        sel_style(bg(Style::default().fg(Theme::text_muted()))),
    )];
    // When a search query hits this line, highlight the literal matches over the
    // plain text (search clarity wins over syntax colour on the matched line);
    // otherwise render the syntax-highlighted tokens as usual.
    let truncated: String = l.text.chars().take(avail).collect();
    let positions = query
        .map(|q| match_positions(&truncated, q))
        .unwrap_or_default();
    let mut used = 0usize;
    if !positions.is_empty() {
        used = truncated.chars().count();
        let base = sel_style(bg(Style::default().fg(Theme::text_primary())));
        spans.extend(crate::ui::highlight::highlighted_spans_owned(
            &truncated, &positions, base,
        ));
    } else {
        let lang = crate::ui::syntax::lang_for(&f.path);
        for (tok, tcolor) in crate::ui::syntax::highlight(&l.text, &lang) {
            if used >= avail {
                break;
            }
            let tok: String = tok.chars().take(avail - used).collect();
            used += tok.chars().count();
            spans.push(Span::styled(
                tok,
                sel_style(bg(Style::default().fg(tcolor))),
            ));
        }
    }
    // Pad so the row tint fills the full width.
    if used < avail {
        spans.push(Span::styled(
            " ".repeat(avail - used),
            sel_style(bg(Style::default())),
        ));
    }
    Line::from(spans)
}

fn comment_line<'a>(
    state: &CodeReviewState,
    id: i64,
    width: usize,
    query: Option<&str>,
    sel_style: impl Fn(Style) -> Style,
) -> Line<'a> {
    let Some(c) = state.comment(id) else {
        return Line::from("");
    };
    let badge = format!("  ▸ [{}] ", c.classification.label());
    let first = c.body.lines().next().unwrap_or("");
    let more = if c.body.lines().count() > 1 {
        " …"
    } else {
        ""
    };
    let body = truncate(
        &format!("{first}{more}"),
        width.saturating_sub(badge.chars().count()),
    );
    let mut spans = vec![Span::styled(
        badge,
        sel_style(
            Style::default()
                .fg(class_color(c.classification))
                .add_modifier(Modifier::BOLD),
        ),
    )];
    spans.extend(highlight_text(
        body,
        sel_style(Style::default().fg(Theme::text_secondary())),
        query,
    ));
    Line::from(spans)
}

/// Render one diff line as two side-by-side cells (old | new). One source line
/// per screen row: context appears in both cells, a deletion only on the left,
/// an addition only on the right — so selection + comment anchoring (1 row = 1
/// line) are unchanged.
fn side_by_side_line<'a>(
    l: &DiffLine,
    width: usize,
    num_w: usize,
    sel_style: &impl Fn(Style) -> Style,
) -> Line<'a> {
    let half = width.saturating_sub(1) / 2;
    let prim = || Style::default().fg(Theme::text_primary());
    let removed = || {
        Style::default()
            .fg(Theme::diff_removed())
            .bg(Theme::diff_removed_bg())
    };
    let added = || {
        Style::default()
            .fg(Theme::diff_added())
            .bg(Theme::diff_added_bg())
    };
    // (cell text, cell style) for each side; the changed side carries its tint
    // (sel_style overrides bg when the row is selected).
    let (left, lstyle, right, rstyle) = match l.kind {
        DiffLineKind::Context => (
            half_cell(l.old_no, &l.text, num_w, half),
            prim(),
            half_cell(l.new_no, &l.text, num_w, half),
            prim(),
        ),
        DiffLineKind::Del => (
            half_cell(l.old_no, &l.text, num_w, half),
            removed(),
            half_cell(None, "", num_w, half),
            prim(),
        ),
        DiffLineKind::Add => (
            half_cell(None, "", num_w, half),
            prim(),
            half_cell(l.new_no, &l.text, num_w, half),
            added(),
        ),
    };
    Line::from(vec![
        Span::styled(left, sel_style(lstyle)),
        Span::styled("│", sel_style(Style::default().fg(Theme::text_muted()))),
        Span::styled(right, sel_style(rstyle)),
    ])
}

/// A fixed-width side-by-side cell: right-aligned line number + text, padded or
/// truncated to exactly `cell_w` columns so the center separator stays aligned.
fn half_cell(num: Option<u32>, text: &str, num_w: usize, cell_w: usize) -> String {
    let n = num.map(|n| n.to_string()).unwrap_or_default();
    let raw = format!("{n:>num_w$} {text}");
    let len = raw.chars().count();
    if len > cell_w {
        raw.chars().take(cell_w).collect()
    } else {
        let mut s = raw;
        s.push_str(&" ".repeat(cell_w - len));
        s
    }
}

/// Render the changed-files list shown in the file-viewer column during a review
/// (the navigation aid; clicking a row jumps the diff to that file). Returns one
/// [`RowHitbox`] per visible file (index = diff-file index).
pub(crate) fn render_files_list(
    frame: &mut Frame,
    area: Rect,
    state: &CodeReviewState,
    level: FocusLevel,
) -> Vec<RowHitbox> {
    let block = focus_block(" Changed files ", level);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.height == 0 || inner.width == 0 || state.files.is_empty() {
        return Vec::new();
    }
    // Reserve the last row for a compact nav-key legend (the keys that aren't
    // footer buttons), so all shortcuts stay discoverable.
    let hint_row = Rect::new(inner.x, inner.y + inner.height - 1, inner.width, 1);
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            truncate(" ↑↓ move · ↵ open · r seen · / find", inner.width as usize),
            Style::default().fg(Theme::text_muted()),
        ))),
        hint_row,
    );
    let list = Rect::new(inner.x, inner.y, inner.width, inner.height - 1);
    let height = list.height as usize;
    if height == 0 {
        return Vec::new();
    }

    // Render the files as a folder tree (directories as headers, files indented
    // beneath). `current_file()` is `None` on the summary section.
    let tree = build_file_tree(&state.files);
    let total = tree.len();
    let current_opt = state.current_file();
    let anchor = current_opt
        .and_then(|ci| {
            tree.iter()
                .position(|r| matches!(r, TreeRow::File { index, .. } if *index == ci))
        })
        .unwrap_or(0);
    let start = anchor
        .saturating_sub(height.saturating_sub(1))
        .min(total.saturating_sub(height));
    let end = (start + height).min(total);
    let w = list.width as usize;

    let mut lines: Vec<Line> = Vec::with_capacity(end - start);
    let mut hitboxes: Vec<RowHitbox> = Vec::new();
    for (row, ti) in (start..end).enumerate() {
        let line = match &tree[ti] {
            TreeRow::Folder { depth, name } => folder_row_line(*depth, name, w),
            TreeRow::File { depth, index } => {
                hitboxes.push(RowHitbox {
                    rect: Rect::new(list.x, list.y + row as u16, list.width, 1),
                    index: *index,
                });
                file_row_line(state, *index, *depth, current_opt == Some(*index))
            }
        };
        lines.push(line);
    }
    frame.render_widget(Paragraph::new(lines), list);
    hitboxes
}

/// A directory header row in the changed-files tree.
fn folder_row_line<'a>(depth: usize, name: &str, width: usize) -> Line<'a> {
    let text = format!("{}{name}/", "  ".repeat(depth));
    Line::from(Span::styled(
        truncate(&text, width),
        Style::default()
            .fg(Theme::text_muted())
            .add_modifier(Modifier::BOLD),
    ))
}

/// A file row in the changed-files tree: indented name + colored status glyph
/// and `+`/`-` counts (dimmed under the selection highlight on the current row).
fn file_row_line<'a>(
    state: &CodeReviewState,
    index: usize,
    depth: usize,
    current: bool,
) -> Line<'a> {
    let f = &state.files[index];
    let mark = if state.reviewed_files.contains(&f.path) {
        "✓ "
    } else {
        "  "
    };
    let name = f.path.rsplit('/').next().unwrap_or(&f.path).to_string();
    let base = if current {
        Style::default()
            .fg(Theme::selection_fg())
            .bg(Theme::selection_bg())
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Theme::text_primary())
    };
    // Tint the glyph + counts with diff colors, except on the selected row where
    // the highlight bg owns the line.
    let tint = |c: Color| if current { base } else { base.fg(c) };
    Line::from(vec![
        Span::styled(format!("{}{mark}", "  ".repeat(depth)), base),
        Span::styled(f.status.glyph().to_string(), tint(status_color(f.status))),
        Span::styled(format!(" {name}  "), base),
        Span::styled(format!("+{}", f.added_count()), tint(Theme::diff_added())),
        Span::styled(
            format!(" -{}", f.deleted_count()),
            tint(Theme::diff_removed()),
        ),
    ])
}

/// A row in the changed-files folder tree: a directory header or a file leaf
/// (carrying its diff-file index for click→jump).
enum TreeRow {
    Folder { depth: usize, name: String },
    File { depth: usize, index: usize },
}

/// Build a folder tree from the diff files: group by directory (so files in the
/// same folder sit together under one header), preserving each file's original
/// diff-file index for hit-testing. Multi-repo paths (`<repo>/<path>`) nest the
/// repo as the top-level folder automatically.
fn build_file_tree(files: &[crate::session::review::DiffFile]) -> Vec<TreeRow> {
    let mut entries: Vec<(usize, Vec<&str>)> = files
        .iter()
        .enumerate()
        .map(|(i, f)| (i, f.path.split('/').collect()))
        .collect();
    // Sort by path segments so sibling files group under a shared directory.
    entries.sort_by(|a, b| a.1.cmp(&b.1));

    let mut rows = Vec::new();
    let mut prev_dirs: Vec<&str> = Vec::new();
    for (idx, segs) in &entries {
        let dirs = &segs[..segs.len().saturating_sub(1)];
        // Emit a folder header for each directory segment that differs from the
        // previous file's path (the standard sorted-paths → tree fold).
        let common = dirs
            .iter()
            .zip(prev_dirs.iter())
            .take_while(|(a, b)| a == b)
            .count();
        for (d, seg) in dirs.iter().enumerate().skip(common) {
            rows.push(TreeRow::Folder {
                depth: d,
                name: seg.to_string(),
            });
        }
        rows.push(TreeRow::File {
            depth: dirs.len(),
            index: *idx,
        });
        prev_dirs = dirs.to_vec();
    }
    rows
}

/// Place the compose box inline at the selected line: just below it when there's
/// room, else just above it, else pinned to the bottom (selection off-screen).
/// Clears the area first so the diff underneath doesn't bleed through.
fn render_compose_inline(
    frame: &mut Frame,
    area: Rect,
    anchor_y: Option<u16>,
    comp: &ComposeState,
) {
    let h = area.height.clamp(3, 6);
    let bottom = area.y + area.height;
    let top = match anchor_y {
        Some(ay) if ay + 1 + h <= bottom => ay + 1,
        Some(ay) if ay >= area.y + h => ay - h,
        _ => bottom.saturating_sub(h),
    };
    // Indent one column so the box reads as attached to the line above it.
    let rect = Rect::new(area.x + 1, top, area.width.saturating_sub(2).max(1), h);
    frame.render_widget(Clear, rect);
    render_compose(frame, rect, comp);
}

/// Render the in-view comment compose box.
fn render_compose(frame: &mut Frame, area: Rect, comp: &ComposeState) {
    if area.height == 0 {
        return;
    }
    let target = match &comp.anchor {
        CommentAnchor::Line { side, line, .. } => format!("line {}:{}", side.as_str(), line),
        CommentAnchor::File { file } => format!("file {file}"),
        CommentAnchor::Review => "review summary".to_string(),
    };
    let mut lines: Vec<Line> = Vec::new();
    lines.push(Line::from(vec![
        Span::styled(
            format!(" Comment on {target}  "),
            Style::default().fg(Theme::text_secondary()),
        ),
        Span::styled(
            format!("[{}]", comp.classification.label()),
            Style::default()
                .fg(class_color(comp.classification))
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            "  Tab: cycle type · Ctrl+S: save · Esc: cancel",
            Style::default().fg(Theme::text_muted()),
        ),
    ]));
    // Body with a visible cursor.
    let (cur_line, cur_col) = comp.body.cursor_line_col();
    let body = comp.body.value();
    let body_lines: Vec<&str> = if body.is_empty() {
        vec![""]
    } else {
        body.split('\n').collect()
    };
    for (li, text) in body_lines.iter().enumerate() {
        if li == cur_line {
            lines.push(cursor_line(text, cur_col));
        } else {
            lines.push(Line::from(Span::styled(
                text.to_string(),
                Style::default().fg(Theme::text_primary()),
            )));
        }
    }
    let block = crate::ui::focus_block(" Compose ", FocusLevel::Focused);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    frame.render_widget(Paragraph::new(lines), inner);
}

/// A compose body line with a reversed cell at the cursor column.
fn cursor_line<'a>(text: &str, col: usize) -> Line<'a> {
    let chars: Vec<char> = text.chars().collect();
    let before: String = chars.iter().take(col).collect();
    let at = chars.get(col).copied().unwrap_or(' ');
    let after: String = chars.iter().skip(col + 1).collect();
    Line::from(vec![
        Span::styled(before, Style::default().fg(Theme::text_primary())),
        Span::styled(
            at.to_string(),
            Style::default().add_modifier(Modifier::REVERSED),
        ),
        Span::styled(after, Style::default().fg(Theme::text_primary())),
    ])
}

/// Render the context-dependent footer button bar and pair each button with its
/// [`ReviewButton`] action.
fn render_footer(
    frame: &mut Frame,
    area: Rect,
    composing: bool,
    side_by_side: bool,
) -> Vec<(crate::ui::ButtonHit, ReviewButton)> {
    // Each button carries its keyboard shortcut as a dimmed hint (`label·key`)
    // so it stays discoverable. The non-composing bar leads with the essential
    // actions (Comment, Send→Agent, Close) so they survive a narrow footer —
    // `render_button_bar` drops overflow from the right and marks it with `…`.
    let view_label = if side_by_side { "Unified" } else { "Split" };
    let (specs, actions): (Vec<ButtonSpec>, Vec<ReviewButton>) = if composing {
        (
            vec![
                ButtonSpec::primary("Save").with_hint("·^S"),
                ButtonSpec::secondary("Cancel").with_hint("·esc"),
                ButtonSpec::secondary("Class").with_hint("·tab"),
            ],
            vec![
                ReviewButton::Save,
                ReviewButton::Cancel,
                ReviewButton::CycleClass,
            ],
        )
    } else {
        (
            vec![
                ButtonSpec::primary("Comment").with_hint("·c"),
                ButtonSpec::primary("Send→Agent").with_hint("·e"),
                ButtonSpec::secondary("Close").with_hint("·esc"),
                ButtonSpec::secondary("Find").with_hint("·/"),
                ButtonSpec::secondary("File").with_hint("·f"),
                ButtonSpec::secondary("Summary").with_hint("·s"),
                ButtonSpec::secondary("Reviewed").with_hint("·r"),
                ButtonSpec::secondary("Target").with_hint("·t"),
                ButtonSpec::secondary(view_label).with_hint("·v"),
                ButtonSpec::secondary("Copy").with_hint("·y"),
            ],
            vec![
                ReviewButton::Comment,
                ReviewButton::SendToAgent,
                ReviewButton::Close,
                ReviewButton::Find,
                ReviewButton::FileComment,
                ReviewButton::Summary,
                ReviewButton::MarkReviewed,
                ReviewButton::Target,
                ReviewButton::ToggleView,
                ReviewButton::Copy,
            ],
        )
    };
    let hits = render_button_bar(frame, area, &specs, false);
    hits.into_iter().map(|h| (h, actions[h.index])).collect()
}

/// Byte offsets of every char inside a case-insensitive occurrence of `query`
/// (already lowercased) within `text`. Non-overlapping, left-to-right. Feeds
/// [`crate::ui::highlight::highlighted_spans_owned`] so search hits get the same
/// accent+bold+underline emphasis as the fuzzy lists elsewhere in the app.
fn match_positions(text: &str, query: &str) -> Vec<usize> {
    if query.is_empty() {
        return Vec::new();
    }
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    let q: Vec<char> = query.chars().collect();
    let (n, m) = (chars.len(), q.len());
    let mut positions = Vec::new();
    let mut i = 0;
    while i + m <= n {
        let hit = (0..m).all(|k| chars[i + k].1.to_lowercase().eq(q[k].to_lowercase()));
        if hit {
            positions.extend((0..m).map(|k| chars[i + k].0));
            i += m;
        } else {
            i += 1;
        }
    }
    positions
}

/// Spans for `text` styled with `base`, with literal `query` hits highlighted
/// when `query` (lowercased, non-empty) is set and matches; otherwise a single
/// plain span. Owned so the spans can outlive a per-row `String`.
fn highlight_text(text: String, base: Style, query: Option<&str>) -> Vec<Span<'static>> {
    if let Some(q) = query {
        let positions = match_positions(&text, q);
        if !positions.is_empty() {
            return crate::ui::highlight::highlighted_spans_owned(&text, &positions, base);
        }
    }
    vec![Span::styled(text, base)]
}

/// Render the find-in-diff search bar (the top row of the diff area while a
/// search is open): the `/`-prefixed query (with a caret while typing) plus the
/// match position/count and key hints, so the shortcut stays discoverable.
fn render_search_bar(frame: &mut Frame, area: Rect, state: &CodeReviewState) {
    let Some(s) = state.search.as_ref() else {
        return;
    };
    let style = Style::default().fg(Theme::search_bar());
    let total = s.matches.len();
    // The "current" position is derived from the selection (like the file
    // viewer's `current_match_index`), so it stays correct after `n`/`N` or a
    // plain `j`/`k` move; 0 when the cursor isn't on a match.
    let current = s
        .matches
        .iter()
        .position(|&i| i == state.selected)
        .map(|p| p + 1)
        .unwrap_or(0);
    let count = if s.query.trim().is_empty() {
        String::new()
    } else if total == 0 {
        "  no matches".to_string()
    } else {
        format!("  {current}/{total}")
    };
    let caret = if s.editing { "█" } else { "" };
    let hint = if s.editing {
        "   ↵/↓ next · ↑ prev · tab done · esc cancel"
    } else {
        "   n next · N prev · esc clear"
    };
    let text = format!("/{}{caret}{count}{hint}", s.query);
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            truncate(&text, area.width as usize),
            style,
        ))),
        area,
    );
}

/// Truncate `s` to `width` display columns (char-based; good enough for the
/// diff text we render).
fn truncate(s: &str, width: usize) -> String {
    if s.chars().count() <= width {
        s.to_string()
    } else {
        s.chars().take(width.saturating_sub(1)).collect::<String>() + "…"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::code_review::CodeReviewState;
    use crate::session::review::{DiffFile, DiffHunk, DiffLine, DiffLineKind, FileStatus};
    use crate::session::SessionId;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    use std::collections::HashSet;

    fn demo_state() -> CodeReviewState {
        let file = DiffFile {
            path: "src/a/very/deep/foo.rs".into(),
            old_path: None,
            status: FileStatus::Modified,
            hunks: vec![DiffHunk {
                old_start: 1,
                new_start: 1,
                header: "fn x".into(),
                lines: vec![
                    DiffLine {
                        kind: DiffLineKind::Context,
                        old_no: Some(1),
                        new_no: Some(1),
                        text: "ctx".into(),
                    },
                    DiffLine {
                        kind: DiffLineKind::Del,
                        old_no: Some(2),
                        new_no: None,
                        text: "old".into(),
                    },
                    DiffLine {
                        kind: DiffLineKind::Add,
                        old_no: None,
                        new_no: Some(2),
                        text: "new".into(),
                    },
                ],
            }],
        };
        let mut s = CodeReviewState {
            session_id: SessionId::default(),
            repos: vec![crate::app::code_review::ReviewRepo {
                label: String::new(),
                dir: std::path::PathBuf::from("/tmp"),
                base: Some("main".into()),
            }],
            multi: false,
            files: vec![file],
            comments: Vec::new(),
            reviewed_files: HashSet::new(),
            reviewed_hunks: HashSet::new(),
            fold_override: HashSet::new(),
            rows: Vec::new(),
            selected: 0,
            scroll: 0,
            compose: None,
            side_by_side: false,
            target: crate::app::code_review::ReviewTarget::Branch,
            commits: Vec::new(),
            host: None,
            target_picker: None,
            search: None,
        };
        s.rebuild_rows();
        s
    }

    #[test]
    fn renders_unified_and_side_by_side_without_panic() {
        for sxs in [false, true] {
            let mut state = demo_state();
            state.side_by_side = sxs;
            let mut term = Terminal::new(TestBackend::new(100, 20)).unwrap();
            term.draw(|f| {
                let area = Rect::new(0, 0, 100, 20);
                let hits = render(f, area, &mut state, FocusLevel::Focused);
                assert!(!hits.rows.is_empty(), "diff rows are clickable");
                assert!(!hits.buttons.is_empty(), "footer buttons render");
            })
            .unwrap();
        }
    }

    #[test]
    fn footer_leads_with_essentials_and_marks_overflow_when_narrow() {
        // 40 cols fits exactly Comment·c (11) + Send→Agent·e (14) + Close·esc
        // (11) with separators (11+1+14+1+11 = 38); the rest overflow.
        let mut term = Terminal::new(TestBackend::new(40, 1)).unwrap();
        let mut actions = Vec::new();
        term.draw(|f| {
            actions = render_footer(f, Rect::new(0, 0, 40, 1), false, false)
                .into_iter()
                .map(|(_, a)| a)
                .collect();
        })
        .unwrap();
        assert_eq!(
            actions,
            vec![
                ReviewButton::Comment,
                ReviewButton::SendToAgent,
                ReviewButton::Close,
            ],
            "the essential actions lead and survive a narrow footer"
        );
        let buf = term.backend().buffer();
        let row: String = (0..40).map(|x| buf[(x, 0)].symbol()).collect();
        assert!(
            row.contains('…'),
            "dropped buttons are marked with an ellipsis"
        );
    }

    #[test]
    fn renders_changed_files_list_without_panic() {
        let state = demo_state();
        let mut term = Terminal::new(TestBackend::new(30, 10)).unwrap();
        term.draw(|f| {
            let area = Rect::new(0, 0, 30, 10);
            let rows = render_files_list(f, area, &state, FocusLevel::Active);
            assert_eq!(rows.len(), 1, "one changed file → one clickable row");
        })
        .unwrap();
    }

    #[test]
    fn file_tree_groups_dirs_and_keeps_indices() {
        use crate::session::review::{DiffFile, FileStatus};
        let mk = |p: &str| DiffFile {
            path: p.into(),
            old_path: None,
            status: FileStatus::Modified,
            hunks: Vec::new(),
        };
        // Out of path order on purpose — the tree sorts + groups by directory.
        let files = vec![mk("src/b.rs"), mk("top.rs"), mk("src/ui/a.rs")];
        let tree = build_file_tree(&files);
        // Folder headers appear for `src` and `src/ui`; the top-level file has no
        // folder. Each file row carries its ORIGINAL index for click→jump.
        let folders: Vec<(usize, &str)> = tree
            .iter()
            .filter_map(|r| match r {
                TreeRow::Folder { depth, name } => Some((*depth, name.as_str())),
                _ => None,
            })
            .collect();
        assert!(folders.contains(&(0, "src")));
        assert!(folders.contains(&(1, "ui")));
        let file_indices: Vec<usize> = tree
            .iter()
            .filter_map(|r| match r {
                TreeRow::File { index, .. } => Some(*index),
                _ => None,
            })
            .collect();
        // All three original indices present (order is by sorted path).
        assert_eq!(file_indices.len(), 3);
        assert!(
            file_indices.contains(&0) && file_indices.contains(&1) && file_indices.contains(&2)
        );
    }

    #[test]
    fn match_positions_finds_case_insensitive_runs() {
        // "Foo bar foo" with query "foo" → two non-overlapping matches, each
        // contributing one byte offset per char (3 chars → 3 offsets).
        let pos = match_positions("Foo bar foo", "foo");
        assert_eq!(pos, vec![0, 1, 2, 8, 9, 10]);
        // No match → empty; empty query → empty.
        assert!(match_positions("abc", "z").is_empty());
        assert!(match_positions("abc", "").is_empty());
    }

    #[test]
    fn renders_search_bar_and_highlights_match() {
        let mut state = demo_state();
        state.search = Some(crate::app::code_review::ReviewSearch {
            query: "ctx".to_string(),
            editing: true,
            matches: state.search_matches("ctx"),
        });
        let mut term = Terminal::new(TestBackend::new(80, 20)).unwrap();
        term.draw(|f| {
            let area = Rect::new(0, 0, 80, 20);
            let _ = render(f, area, &mut state, FocusLevel::Focused);
        })
        .unwrap();
        // The search bar prints the `/`-prefixed query somewhere on screen.
        let buf = term.backend().buffer();
        let mut screen = String::new();
        for y in 0..20 {
            for x in 0..80 {
                screen.push_str(buf[(x, y)].symbol());
            }
            screen.push('\n');
        }
        assert!(
            screen.contains("/ctx"),
            "search bar shows the query: {screen}"
        );
    }

    #[test]
    fn renders_inline_compose_without_panic() {
        use crate::session::review::{Classification, CommentAnchor, Side};
        let mut state = demo_state();
        state.selected = 2; // a Line row
        state.compose = Some(crate::app::code_review::ComposeState {
            anchor: CommentAnchor::Line {
                file: "src/foo.rs".into(),
                side: Side::New,
                line: 2,
            },
            classification: Classification::Issue,
            body: crate::app::modals::TextArea::new(),
            editing_id: None,
        });
        let mut term = Terminal::new(TestBackend::new(100, 20)).unwrap();
        term.draw(|f| {
            let area = Rect::new(0, 0, 100, 20);
            let hits = render(f, area, &mut state, FocusLevel::Focused);
            // Footer still renders its buttons while composing.
            assert!(!hits.buttons.is_empty());
        })
        .unwrap();
    }
}
