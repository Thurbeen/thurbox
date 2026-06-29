use std::path::Path;

use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::Style,
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, Paragraph},
    Frame,
};

use super::render_modal_frame;
use super::theme::Theme;
use super::{centered_fixed_height_rect, render_text_field_with_suggestion};
use crate::app::modals::{BasketEntry, BrowseEntry, BrowseKind, RepoPickerFocus};

/// Borrowed view of the two-pane repo picker for rendering. The filesystem
/// listing (`browse_entries`) and `is_repo` flags are precomputed in the `app`
/// layer (the `ui` layer may not touch `crate::git` or the filesystem), so
/// rendering is a pure read.
pub struct RepoPickerState<'a> {
    pub focus: RepoPickerFocus,
    /// Whether the new session targets a remote host (browsing is local-only).
    pub remote: bool,
    // Left pane — filesystem browser.
    pub browse_dir: &'a Path,
    pub browse_entries: &'a [BrowseEntry],
    pub browse_filtered: &'a [usize],
    pub browse_index: usize,
    pub filter_query: &'a str,
    pub filter_active: bool,
    // Right pane — basket of chosen repos.
    pub basket: &'a [BasketEntry],
    pub basket_index: usize,
    // Path text-input (remote add / local go-to).
    pub path_input: &'a str,
    pub path_cursor: usize,
    pub path_suggestion: Option<&'a str>,
}

/// A clickable basket row: `body` toggles the repo's worktree flag, `remove`
/// (the trailing `✕`) drops it from the basket.
pub struct BasketHit {
    pub index: usize,
    pub body: Rect,
    pub remove: Rect,
}

/// The `[+]` add-button on a plain-folder browser row (`index` is its filtered
/// row index).
pub struct BrowseAddHit {
    pub index: usize,
    pub rect: Rect,
}

/// The clickable sub-areas of the repo picker recorded for the mouse handler:
/// the path text-input row, the basket rows, and the browser `[+]` buttons.
pub struct RepoFocusAreas {
    pub input: Rect,
    pub basket: Vec<BasketHit>,
    pub browse_add: Vec<BrowseAddHit>,
}

/// Modal width (cols) — wide enough for the two panes side by side.
const MODAL_WIDTH: u16 = 76;
/// Max visible rows in either pane before it scrolls.
const MAX_ROWS: usize = 10;
/// Width (cols) of an inline row action button (`[✕]` remove, `[+]` add) — a
/// comfortably clickable target rather than a single glyph.
const ACTION_BTN_W: u16 = 3;

pub fn render_repo_picker_modal(
    frame: &mut Frame,
    state: &RepoPickerState<'_>,
) -> (super::ModalRender, RepoFocusAreas) {
    let rows = state
        .browse_filtered
        .len()
        .max(state.basket.len())
        .clamp(1, MAX_ROWS);
    let panes_height = rows as u16 + 2; // +2 borders
                                        // panes + input(3) + footer(1) + outer border(2)
    let total_height = panes_height + 3 + 1 + 2;

    let area = centered_fixed_height_rect(MODAL_WIDTH, total_height, frame.area());
    let inner = render_modal_frame(frame, area, "New Session · repos");

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(panes_height),
            Constraint::Length(3), // path input
            Constraint::Min(1),    // footer
        ])
        .split(inner);
    let (panes_area, input_area, footer_area) = (chunks[0], chunks[1], chunks[2]);

    let panes = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(58), Constraint::Percentage(42)])
        .split(panes_area);
    let (left_area, right_area) = (panes[0], panes[1]);

    // Left pane: the browser (local) or a placeholder (remote — type below).
    let (hitboxes, browse_add) = if state.remote {
        render_remote_left(frame, left_area);
        ((Vec::new(), None), Vec::new())
    } else {
        render_browse_pane(frame, left_area, state)
    };

    let basket_hits = render_basket_pane(frame, right_area, state);

    let input_label = if state.remote {
        "Add remote path"
    } else {
        "Go to path"
    };
    render_text_field_with_suggestion(
        frame,
        input_area,
        input_label,
        state.path_input,
        state.path_cursor,
        state.focus == RepoPickerFocus::PathInput,
        state.path_suggestion,
    );

    frame.render_widget(Paragraph::new(footer_line(state)), footer_area);
    // Done replays Ctrl+Enter so it confirms from any pane — a plain Enter in
    // the browser opens a folder (see `handle_repo_picker_key`).
    let buttons = super::render_action_footer(
        frame,
        footer_area,
        (
            "Done",
            crossterm::event::KeyCode::Enter,
            crossterm::event::KeyModifiers::CONTROL,
        ),
        "Cancel",
    );
    (
        (hitboxes, buttons),
        RepoFocusAreas {
            input: input_area,
            basket: basket_hits,
            browse_add,
        },
    )
}

/// Left pane for a remote target: there's no local filesystem to browse.
fn render_remote_left(frame: &mut Frame, area: Rect) {
    let block = Block::default()
        .title(" Browse ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Theme::border_unfocused()));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            "  remote — type a path below",
            Style::default().fg(Theme::text_muted()),
        ))),
        inner,
    );
}

/// Render the filesystem browser (left pane), returning the row hitboxes and
/// the `[+]` add-buttons drawn on plain-folder rows.
fn render_browse_pane(
    frame: &mut Frame,
    area: Rect,
    state: &RepoPickerState<'_>,
) -> (super::SelectorHits, Vec<BrowseAddHit>) {
    let focused = state.focus == RepoPickerFocus::Browse;
    let border = if focused {
        Theme::border_focused()
    } else {
        Theme::border_unfocused()
    };
    // While filtering, the title shows the live query; otherwise the breadcrumb.
    let title = if state.filter_active || !state.filter_query.is_empty() {
        format!(" filter: {} ", state.filter_query)
    } else {
        format!(" {} ", state.browse_dir.display())
    };
    let block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_style(Style::default().fg(border));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if state.browse_filtered.is_empty() {
        let msg = if state.filter_query.is_empty() {
            "  (no sub-directories)"
        } else {
            "  (no matches)"
        };
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                msg,
                Style::default().fg(Theme::text_muted()),
            ))),
            inner,
        );
        return ((Vec::new(), None), Vec::new());
    }

    let total = state.browse_filtered.len();
    let visible = inner.height as usize;
    let offset = state.browse_index.saturating_sub(visible.saturating_sub(1));
    let (rows_area, track) = super::scrollbar::reserve_track(inner, total, visible);

    let items: Vec<ListItem<'_>> = state
        .browse_filtered
        .iter()
        .enumerate()
        .skip(offset)
        .take(visible)
        .filter_map(|(vi, &real)| {
            let entry = state.browse_entries.get(real)?;
            Some(browse_item(entry, vi == state.browse_index && focused))
        })
        .collect();
    frame.render_widget(List::new(items), rows_area);

    let mut hitboxes = Vec::new();
    let mut add_hits = Vec::new();
    for (line, vi) in (offset..total.min(offset + visible)).enumerate() {
        let y = rows_area.y + line as u16;
        hitboxes.push(super::RowHitbox {
            rect: Rect::new(rows_area.x, y, rows_area.width, 1),
            index: vi,
        });
        // A plain folder (clicking its name navigates) gets a trailing `[+]` so
        // it can be added as an attached dir; checked before the row hitbox.
        let is_plain_dir = state
            .browse_filtered
            .get(vi)
            .and_then(|&real| state.browse_entries.get(real))
            .is_some_and(|e| e.addable() && !e.is_repo);
        if is_plain_dir && rows_area.width >= ACTION_BTN_W {
            let rect = Rect::new(
                rows_area.x + rows_area.width - ACTION_BTN_W,
                y,
                ACTION_BTN_W,
                1,
            );
            frame.render_widget(
                Paragraph::new(Span::styled("[+]", Style::default().fg(Theme::accent()))),
                rect,
            );
            add_hits.push(BrowseAddHit { index: vi, rect });
        }
    }

    let geom = track
        .and_then(|t| super::scrollbar::render_into(frame, t, total, visible, state.browse_index));
    ((hitboxes, geom), add_hits)
}

/// Build one browser row. The glyph encodes the row kind: `..` (up), a pinned
/// favorite (★), a git repo (●, addable), or a plain folder (▸).
fn browse_item(entry: &BrowseEntry, is_cursor: bool) -> ListItem<'static> {
    let style = if is_cursor {
        Theme::selected_item()
    } else {
        Theme::normal_item()
    };
    let muted = Style::default().fg(Theme::text_muted());
    let accent = Style::default().fg(Theme::accent());
    match entry.kind {
        BrowseKind::Up => ListItem::new(Line::from(vec![
            Span::styled("⮤ ", muted),
            Span::styled("..", style),
        ])),
        BrowseKind::Favorite => ListItem::new(Line::from(vec![
            Span::styled("★ ", accent),
            Span::styled(entry.name.clone(), style),
            Span::styled(" (favorite)", muted),
        ])),
        BrowseKind::Dir if entry.is_repo => ListItem::new(Line::from(vec![
            Span::styled("● ", accent),
            Span::styled(entry.name.clone(), style),
            Span::styled(" (repo)", muted),
        ])),
        BrowseKind::Dir => ListItem::new(Line::from(vec![
            Span::styled("▸ ", muted),
            Span::styled(entry.name.clone(), style),
        ])),
    }
}

/// Render the basket of chosen repos (right pane), returning per-row click
/// hitboxes (body = worktree toggle, trailing `✕` = remove).
fn render_basket_pane(
    frame: &mut Frame,
    area: Rect,
    state: &RepoPickerState<'_>,
) -> Vec<BasketHit> {
    let focused = state.focus == RepoPickerFocus::Basket;
    let border = if focused {
        Theme::border_focused()
    } else {
        Theme::border_unfocused()
    };
    let block = Block::default()
        .title(format!(" Selected ({}) ", state.basket.len()))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(border));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if state.basket.is_empty() {
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                "  a / Space to add",
                Style::default().fg(Theme::text_muted()),
            ))),
            inner,
        );
        return Vec::new();
    }

    let total = state.basket.len();
    let visible = inner.height as usize;
    let offset = state.basket_index.saturating_sub(visible.saturating_sub(1));
    let (rows_area, track) = super::scrollbar::reserve_track(inner, total, visible);

    // Reserve a `[✕]` remove button at the end of each row.
    let body_w = rows_area.width.saturating_sub(ACTION_BTN_W);
    let body_area = Rect {
        width: body_w,
        ..rows_area
    };
    let items: Vec<ListItem<'_>> = state
        .basket
        .iter()
        .enumerate()
        .skip(offset)
        .take(visible)
        .map(|(i, repo)| basket_item(repo, i == state.basket_index && focused))
        .collect();
    frame.render_widget(List::new(items), body_area);

    let x_start = rows_area.x + body_w;
    let mut hits = Vec::new();
    for (line, i) in (offset..total.min(offset + visible)).enumerate() {
        let y = rows_area.y + line as u16;
        let remove = Rect::new(x_start, y, ACTION_BTN_W, 1);
        frame.render_widget(
            Paragraph::new(Span::styled(
                "[✕]",
                Style::default().fg(Theme::text_muted()),
            )),
            remove,
        );
        hits.push(BasketHit {
            index: i,
            body: Rect::new(rows_area.x, y, body_w, 1),
            remove,
        });
    }

    if let Some(t) = track {
        super::scrollbar::render_into(frame, t, total, visible, state.basket_index);
    }
    hits
}

/// Build one basket row: name + a `[wt]` marker for worktree repos, or a muted
/// `(dir)` tag for a plain directory attached as-is.
fn basket_item(entry: &BasketEntry, is_cursor: bool) -> ListItem<'static> {
    let style = if is_cursor {
        Theme::selected_item()
    } else {
        Theme::normal_item()
    };
    let mut spans = vec![Span::styled(entry.name.clone(), style)];
    if !entry.is_repo {
        spans.push(Span::styled(
            " (dir)",
            Style::default().fg(Theme::text_muted()),
        ));
    } else if entry.worktree {
        spans.push(Span::styled(" [wt]", Style::default().fg(Theme::accent())));
    }
    ListItem::new(Line::from(spans))
}

/// Build the footer hint line for the current focus.
fn footer_line(state: &RepoPickerState<'_>) -> Line<'static> {
    let kb = |s: &'static str| Span::styled(s, Theme::keybind());
    let de = |s: &'static str| Span::styled(s, Theme::keybind_desc());
    match state.focus {
        RepoPickerFocus::Browse if state.filter_active => Line::from(vec![
            kb("type"),
            de(" filter  "),
            kb("Enter"),
            de(" keep  "),
            kb("Esc"),
            de(" clear"),
        ]),
        RepoPickerFocus::Browse => Line::from(vec![
            kb("j/k"),
            de(" nav  "),
            kb("Enter"),
            de(" pick  "),
            kb("l"),
            de(" open  "),
            kb("Bksp"),
            de(" up  "),
            kb("a/Spc"),
            de(" add  "),
            kb("/"),
            de(" filter  "),
            kb("Tab"),
            de(" panes"),
        ]),
        RepoPickerFocus::Basket => Line::from(vec![
            kb("j/k"),
            de(" nav  "),
            kb("w"),
            de(" worktree  "),
            kb("x"),
            de(" remove  "),
            kb("Tab"),
            de(" browse  "),
            kb("Enter"),
            de(" done"),
        ]),
        RepoPickerFocus::PathInput => {
            let (enter, esc) = if state.remote {
                (" add  ", " cancel")
            } else {
                (" go  ", " back")
            };
            let tab = if state.path_suggestion.is_some() {
                " complete  "
            } else {
                " switch  "
            };
            Line::from(vec![
                kb("Tab"),
                de(tab),
                kb("Enter"),
                de(enter),
                kb("Esc"),
                de(esc),
            ])
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn span_text(spans: &[Span<'_>]) -> String {
        spans.iter().map(|s| s.content.as_ref()).collect()
    }

    fn entry(name: &str, is_repo: bool) -> BrowseEntry {
        BrowseEntry::dir(PathBuf::from("/p").join(name), name.to_string(), is_repo)
    }

    #[test]
    fn browse_item_marks_rows_by_kind() {
        // Can't read ListItem spans directly, so assert via the debug repr.
        let repo = format!("{:?}", browse_item(&entry("alpha", true), false));
        assert!(repo.contains('●') && repo.contains("(repo)"));
        let plain = format!("{:?}", browse_item(&entry("tools", false), false));
        assert!(plain.contains('▸') && !plain.contains("(repo)"));

        let up = BrowseEntry {
            path: "/".into(),
            name: "..".into(),
            is_repo: false,
            kind: BrowseKind::Up,
        };
        assert!(format!("{:?}", browse_item(&up, false)).contains(".."));

        let fav = BrowseEntry {
            path: "/p/fav".into(),
            name: "fav".into(),
            is_repo: true,
            kind: BrowseKind::Favorite,
        };
        assert!(format!("{:?}", browse_item(&fav, false)).contains("favorite"));
    }

    fn picker_state(focus: RepoPickerFocus) -> RepoPickerState<'static> {
        static EMPTY_ENTRIES: &[BrowseEntry] = &[];
        static EMPTY_IDX: &[usize] = &[];
        static EMPTY_BASKET: &[BasketEntry] = &[];
        RepoPickerState {
            focus,
            remote: false,
            browse_dir: Path::new("/p"),
            browse_entries: EMPTY_ENTRIES,
            browse_filtered: EMPTY_IDX,
            browse_index: 0,
            filter_query: "",
            filter_active: false,
            basket: EMPTY_BASKET,
            basket_index: 0,
            path_input: "",
            path_cursor: 0,
            path_suggestion: None,
        }
    }

    #[test]
    fn footer_browse_shows_core_hints() {
        let text = span_text(&footer_line(&picker_state(RepoPickerFocus::Browse)).spans);
        assert!(text.contains("open"));
        assert!(text.contains("add"));
        assert!(text.contains("panes"));
    }

    #[test]
    fn footer_basket_shows_worktree_and_remove() {
        let text = span_text(&footer_line(&picker_state(RepoPickerFocus::Basket)).spans);
        assert!(text.contains("worktree"));
        assert!(text.contains("remove"));
    }
}
