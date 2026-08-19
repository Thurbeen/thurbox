//! The theme picker: the presets plus `themes.toml`, and nothing else.
//!
//! Deliberately asymmetric with the other two (design.md D1): there is nothing
//! a plugin could usefully add to a list of palettes, so this modal declares no
//! extension point at all rather than an unused one.
//!
//! The layout is v1's `ui::theme_picker_modal`, kept detail for detail: a
//! 64 %-wide, content-height frame; the `/` filter line with a right-aligned
//! `matched/total themes` count; a 45/55 split of rows and preview swatch;
//! `Dark`/`Light` headers drawn *inside* the row that opens each section, so a
//! header costs a screen line but never a selection index; the `  theme id:`
//! line; and hints with right-packed ` Select ` / ` Cancel ` pills.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use super::chrome::{self, Chrome, Hits, Pill, Replay};
use crate::kernel::theme::{ThemeChoice, Themes};
use crate::session::theme_config::ThemePalette;
use crate::storage::Database;

/// Non-list rows the modal always spends: both borders, the filter line, the
/// theme-id line, the footer, and one spacer. v1's `CHROME`.
const CHROME: u16 = 6;
/// Tallest the modal may grow, as a percent of the screen.
const MAX_FRAME_PCT: u16 = 85;
/// Floor, so it stays usable on a very short terminal.
const MIN_HEIGHT: u16 = 10;
/// Share of the width the modal takes, centred.
const WIDTH_PCT: u16 = 64;
/// Rows of context kept above the selection while scrolling.
const MAX_MARGIN: u16 = 3;
/// Blank column kept right of the count so it does not touch the border.
const RIGHT_GUTTER: usize = 1;

/// A visible row: the choice to draw, plus the section header that precedes it
/// when it opens one. Keeping the header attached to its row is what lets the
/// whole list stay indexed in match space.
struct Row {
    /// Index into the *matches*, which is what the cursor moves through.
    match_index: usize,
    /// Index into the full choice list.
    choice_index: usize,
    header: Option<&'static str>,
}

fn row_height(row: &Row) -> u16 {
    1 + u16::from(row.header.is_some())
}

fn total_height(rows: &[Row]) -> u16 {
    rows.iter().map(row_height).fold(0u16, u16::saturating_add)
}

/// Pair each surviving choice with its header. A header is emitted at the first
/// choice of each `Dark`/`Light` run, so a filter that removes every light
/// theme removes the `Light` header with it.
fn build_rows(choices: &[ThemeChoice], matches: &[usize]) -> Vec<Row> {
    let mut rows = Vec::with_capacity(matches.len());
    let mut current: Option<bool> = None;
    for (match_index, &choice_index) in matches.iter().enumerate() {
        let is_light = choices[choice_index].is_light;
        let header = (current != Some(is_light)).then_some(if is_light { "Light" } else { "Dark" });
        current = Some(is_light);
        rows.push(Row {
            match_index,
            choice_index,
            header,
        });
    }
    rows
}

/// Case-insensitive substring match over a theme's display name *and* its
/// stable id, so both "rose" and "rose-pine-dawn" find the same entry. v1's
/// `ThemePickerModal::is_match`.
fn is_match(choice: &ThemeChoice, needle: &str) -> bool {
    choice.display_name.to_lowercase().contains(needle)
        || choice.name.to_lowercase().contains(needle)
}

fn compute_matches(choices: &[ThemeChoice], filter: &str) -> Vec<usize> {
    let needle = filter.trim().to_lowercase();
    choices
        .iter()
        .enumerate()
        .filter(|(_, choice)| needle.is_empty() || is_match(choice, &needle))
        .map(|(index, _)| index)
        .collect()
}

/// v1's `scroll_start`. Row heights vary, so this WALKS rows rather than doing
/// flat arithmetic: back off far enough to show the margin above the selection,
/// then pull the top up to fill the pane.
fn scroll_start(rows: &[Row], selected: usize, height: u16) -> usize {
    if rows.is_empty() {
        return 0;
    }
    let selected = selected.min(rows.len() - 1);
    // Show the top whenever the top would have fitted. The margin walk below
    // backs off at most `MAX_MARGIN` rows, which is right for a list scrolled
    // into its middle and wrong at the start of one: the picker opens on the
    // active theme, and with `doom` fifth the walk stopped one row short of the
    // beginning — because the first row carries the `Dark` section header and is
    // two rows tall. So `Default` and the header that groups everything under it
    // sat off the top of a pane with twenty-six rows to spare, which read as the
    // grouping being absent rather than scrolled.
    if total_height(&rows[..=selected]) <= height {
        return 0;
    }
    let margin = (height / 4).min(MAX_MARGIN);
    let mut start = selected;
    let mut used = 0u16;
    while start > 0 && used + row_height(&rows[start - 1]) <= margin {
        used += row_height(&rows[start - 1]);
        start -= 1;
    }
    let mut total = total_height(&rows[start..]);
    while start > 0 && total + row_height(&rows[start - 1]) <= height {
        start -= 1;
        total += row_height(&rows[start]);
    }
    start
}

#[derive(Default)]
pub struct ThemeModal {
    /// Cursor within the *matches*, not within the full list.
    selected: usize,
    /// The open query, or `None` in navigation mode — v1 puts the filter behind
    /// `/` (a sub-mode, like the file viewer's find) so `j`/`k` keep selecting
    /// as they do in every other picker.
    filter: Option<String>,
    /// How many rows the list showed last frame, so `PgUp`/`PgDn` step by
    /// exactly one screenful.
    page: usize,
    /// Where the cursor should start: the active theme, resolved once.
    settled: bool,
    /// The theme that was active when the picker opened, so `Esc` can put it
    /// back. Every cursor move previews, and a preview is not a choice — v1
    /// keeps the same value in `ThemePickerModal::original` for the same reason.
    original: Option<String>,
    hits: Hits,
}

impl ThemeModal {
    /// Whether keystrokes are being typed into the filter rather than driving
    /// the list.
    pub fn filtering(&self) -> bool {
        self.filter.is_some()
    }

    pub fn hits(&self) -> &Hits {
        &self.hits
    }

    /// Take a key. `Some(message)` is reported by the loop; `None` is silent.
    ///
    /// Returns `Close` when the modal is done with itself — an `Esc` that had
    /// no filter to close first.
    pub fn on_key(
        &mut self,
        key: &KeyEvent,
        themes: &mut Themes,
        db: Option<&Database>,
    ) -> Outcome {
        let choices = themes.choices();
        self.settle(choices, themes.active_name());
        let matches = compute_matches(choices, self.filter.as_deref().unwrap_or(""));
        let total = matches.len();

        // The theme under the cursor *now*, so a query edit below can carry the
        // cursor to wherever that theme lands in the new match list.
        let highlighted = matches.get(self.selected).copied();

        // While the filter is open, a bare letter is query text — so the
        // navigation keys narrow to the ones that cannot be typed.
        if let Some(query) = &mut self.filter {
            let edited = match key.code {
                KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                    query.push(c);
                    true
                }
                KeyCode::Backspace => {
                    query.pop();
                    true
                }
                // v1 closes the filter first, restoring the full list without
                // moving the cursor off the theme it was on; a second Esc then
                // closes the picker.
                KeyCode::Esc => {
                    self.filter = None;
                    true
                }
                _ => false,
            };
            if edited {
                self.keep_highlighted(choices, highlighted);
                // Narrowing can move the cursor onto a different theme, so the
                // preview follows it — otherwise the list would highlight one
                // palette while the screen showed another.
                self.preview_selected(themes);
                return Outcome::Stay(None);
            }
        }

        match key.code {
            KeyCode::Char('j') | KeyCode::Down => self.move_by(1, total),
            KeyCode::Char('k') | KeyCode::Up => self.move_by(-1, total),
            KeyCode::Char('n') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.move_by(1, total)
            }
            KeyCode::Char('p') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.move_by(-1, total)
            }
            KeyCode::PageDown => self.move_by(self.page.max(1) as isize, total),
            KeyCode::PageUp => self.move_by(-(self.page.max(1) as isize), total),
            KeyCode::Char('g') | KeyCode::Home => self.selected = 0,
            KeyCode::Char('G') | KeyCode::End => self.selected = total.saturating_sub(1),
            KeyCode::Char('/') => {
                // Idempotent: re-pressing `/` keeps the query rather than
                // clearing it.
                self.filter.get_or_insert_with(String::new);
            }
            KeyCode::Enter => {
                let Some(name) = matches
                    .get(self.selected)
                    .and_then(|index| choices.get(*index))
                    .map(|choice| choice.name.clone())
                else {
                    return Outcome::Stay(None);
                };
                // Applied and persisted where v1 persists it —
                // `metadata.active_theme` — so every other thurbox process
                // picks it up on its next `data_version` poll.
                // Chosen, so there is nothing to put back — clearing this is
                // what stops the close below from reverting the choice.
                self.original = None;
                return match themes.select(&name, db) {
                    Ok(()) => Outcome::Applied(format!("theme: {name}")),
                    Err(e) => Outcome::Stay(Some(e)),
                };
            }
            _ => {}
        }
        // Every arm above either moved the cursor or did nothing, so previewing
        // here covers all of them — v1 does exactly this at the end of its own
        // handler rather than repeating the call per key.
        self.preview_selected(themes);
        Outcome::Stay(None)
    }

    /// Apply whatever the cursor points at, without persisting it.
    ///
    /// An empty match list leaves the previous preview alone: there is nothing to
    /// show, and blanking back to the original mid-search would read as the
    /// picker forgetting what you were looking at.
    pub(super) fn preview_selected(&self, themes: &mut Themes) {
        let choices = themes.choices();
        let matches = compute_matches(choices, self.filter.as_deref().unwrap_or(""));
        let Some(name) = matches
            .get(self.selected)
            .and_then(|index| choices.get(*index))
            .map(|choice| choice.name.clone())
        else {
            return;
        };
        // A palette that will not resolve is not worth reporting from a preview:
        // the list only offers names that came from `choices`.
        let _ = themes.preview(&name);
    }

    /// Put back the palette that was active when the picker opened.
    ///
    /// Called when the picker closes without a choice. Nothing was persisted, so
    /// this is the whole of undoing a preview.
    pub fn revert(&mut self, themes: &mut Themes) {
        if let Some(original) = self.original.take() {
            let _ = themes.preview(&original);
        }
    }

    fn move_by(&mut self, step: isize, total: usize) {
        if total == 0 {
            self.selected = 0;
            return;
        }
        let next = self.selected as isize + step;
        self.selected = next.clamp(0, total as isize - 1) as usize;
    }

    /// Start on the theme that is actually in force, so the first `j` moves
    /// from the highlighted row rather than from row one.
    fn settle(&mut self, choices: &[ThemeChoice], active: &str) {
        if self.settled {
            return;
        }
        self.settled = true;
        // Captured before any preview can move `active` off it.
        self.original = Some(active.to_string());
        if let Some(index) = choices.iter().position(|choice| choice.name == active) {
            self.selected = index;
        }
    }

    /// Put the cursor back on `highlighted` after a query edit, when that theme
    /// survived the new filter.
    ///
    /// v1's `refilter`, and the reason for it: the cursor indexes *matches*, so
    /// holding the index across an edit would silently preview — and then apply
    /// — a different palette than the one that was highlighted.
    fn keep_highlighted(&mut self, choices: &[ThemeChoice], highlighted: Option<usize>) {
        let matches = compute_matches(choices, self.filter.as_deref().unwrap_or(""));
        self.selected = highlighted
            .and_then(|choice| matches.iter().position(|index| *index == choice))
            .unwrap_or(0);
    }

    /// A click selects the theme row under it — the same act as `j`/`k`, with
    /// `Enter` still the thing that applies, so a stray click cannot repaint
    /// the whole interface.
    pub fn on_click(&mut self, x: u16, y: u16) {
        if let Some(index) = self.hits.row_at(x, y) {
            self.selected = index;
        }
    }

    pub fn render(&mut self, frame: &mut Frame, area: Rect, themes: &Themes, chrome: Chrome) {
        self.hits.clear();
        let choices = themes.choices();
        self.settle(choices, themes.active_name());
        let matches = compute_matches(choices, self.filter.as_deref().unwrap_or(""));
        self.selected = self.selected.min(matches.len().saturating_sub(1));
        let rows = build_rows(choices, &matches);

        // Grow with the content, but cap at a share of the screen so the
        // interface around it stays visible; the list scroll-windows inside.
        let desired = total_height(&rows).saturating_add(CHROME).max(MIN_HEIGHT);
        let cap = (area.height * MAX_FRAME_PCT / 100).max(MIN_HEIGHT);
        let height = desired.min(cap).min(area.height);
        let modal = chrome::centered_fixed_height_rect(WIDTH_PCT, height, area);
        let inner = chrome::modal_frame(frame, modal, "Theme", chrome);
        if inner.width == 0 || inner.height == 0 {
            return;
        }

        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(1), // filter query + count
                Constraint::Min(1),    // rows + preview
                Constraint::Length(1), // theme id
                Constraint::Length(1), // hints + buttons
            ])
            .split(inner);

        self.render_head(frame, chunks[0], matches.len(), choices.len(), chrome);

        let columns = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(45), Constraint::Percentage(55)])
            .split(chunks[1]);
        self.render_rows(frame, columns[0], choices, &rows, chrome);

        if let Some(choice) = matches
            .get(self.selected)
            .and_then(|index| choices.get(*index))
        {
            // v1's swatch previews the HIGHLIGHTED row, not the active theme,
            // which is the whole point of a preview.
            if let Some(entry) = themes.entry(&choice.name) {
                render_preview(frame, columns[1], &entry.palette, chrome);
            }
            frame.render_widget(
                Paragraph::new(Line::from(Span::styled(
                    format!("  theme id: {}", choice.name),
                    chrome.muted(),
                ))),
                chunks[2],
            );
        }

        // The hint is clipped to the space left of the pills, so a narrow modal
        // elides it rather than painting underneath them.
        let hints: &[(&str, &str)] = if self.filter.is_some() {
            &[
                ("↑/↓", " navigate  "),
                ("type", " filter  "),
                ("Esc", " clear filter"),
            ]
        } else {
            &[
                ("j/k", " navigate  "),
                ("PgUp/PgDn", " page  "),
                ("/", " filter"),
            ]
        };
        // v1's pair, with the keys it replays them as: ` Select ` is `Enter`,
        // ` Cancel ` the `Esc` the layer above turns into a close.
        self.hits.buttons = chrome::button_bar(
            frame,
            chunks[3],
            &[
                Pill {
                    label: "Select",
                    primary: true,
                    key: Replay::ENTER,
                },
                Pill {
                    label: "Cancel",
                    primary: false,
                    key: Replay::ESC,
                },
            ],
            chrome,
        );
        let hint_width = self
            .hits
            .buttons
            .first()
            .map_or(chunks[3].width, |(rect, _)| {
                rect.x.saturating_sub(chunks[3].x + 1)
            });
        frame.render_widget(
            Paragraph::new(chrome::hint_line(hints, chrome)),
            Rect {
                width: hint_width,
                ..chunks[3]
            },
        );
    }

    /// The filter line: the live query or the hint that opens it, with the
    /// match count right-aligned so a query that narrows to nothing is obvious
    /// rather than an unexplained empty list.
    fn render_head(
        &self,
        frame: &mut Frame,
        area: Rect,
        matched: usize,
        total: usize,
        chrome: Chrome,
    ) {
        let count = format!("{matched}/{total} themes");
        let mut head = vec![Span::styled(
            " / ",
            Style::default().fg(chrome.palette.accent),
        )];
        match &self.filter {
            Some(query) => {
                head.push(Span::styled(query.clone(), chrome.normal_item()));
                head.push(Span::styled(
                    "█",
                    Style::default().fg(chrome.palette.accent),
                ));
            }
            None => head.push(Span::styled("filter themes", chrome.muted())),
        }
        let used: usize = head.iter().map(|span| span.content.chars().count()).sum();
        let pad =
            usize::from(area.width).saturating_sub(used + count.chars().count() + RIGHT_GUTTER);
        head.push(Span::raw(" ".repeat(pad)));
        head.push(Span::styled(count, chrome.muted()));
        frame.render_widget(Paragraph::new(Line::from(head)), area);
    }

    fn render_rows(
        &mut self,
        frame: &mut Frame,
        area: Rect,
        choices: &[ThemeChoice],
        rows: &[Row],
        chrome: Chrome,
    ) {
        if rows.is_empty() || area.height == 0 || area.width == 0 {
            frame.render_widget(
                Paragraph::new(Line::from(Span::styled(
                    "  no matching theme",
                    chrome.muted(),
                ))),
                area,
            );
            self.page = 0;
            return;
        }
        // Reserved against the full un-windowed height, so the rows column
        // keeps one width as the window scrolls.
        let (rows_area, track) = chrome::reserve_track(
            area,
            usize::from(total_height(rows)),
            usize::from(area.height),
        );

        let start = scroll_start(rows, self.selected, rows_area.height);
        let mut lines: Vec<Line> = Vec::new();
        for row in &rows[start..] {
            let remaining = rows_area.height.saturating_sub(lines.len() as u16);
            if remaining < row_height(row) {
                break;
            }
            if let Some(header) = row.header {
                lines.push(Line::from(Span::styled(
                    format!(" {header}"),
                    chrome.muted().add_modifier(Modifier::BOLD),
                )));
            }
            self.hits.rows.push((
                Rect::new(
                    rows_area.x,
                    rows_area.y + lines.len() as u16,
                    rows_area.width,
                    1,
                ),
                row.match_index,
            ));
            let selected = row.match_index == self.selected;
            let label = &choices[row.choice_index].display_name;
            lines.push(Line::from(Span::styled(
                format!("{}{label}", if selected { "▸ " } else { "  " }),
                if selected {
                    chrome.selected_item()
                } else {
                    chrome.normal_item()
                },
            )));
        }
        // One hitbox per visible row, so its count *is* the page step.
        self.page = self.hits.rows.len();
        frame.render_widget(Paragraph::new(lines), rows_area);

        // The bar tracks ROWS, not lines, so its thumb matches what the cursor
        // moves through — headers get no stop of their own.
        if let Some(track) = track {
            chrome::scrollbar(
                frame,
                track,
                rows.len(),
                usize::from(rows_area.height),
                self.selected,
                chrome,
            );
        }
    }
}

/// What a key did to the modal.
pub enum Outcome {
    /// Stay open, optionally reporting something.
    Stay(Option<String>),
    /// A theme was applied; the layer closes the modal so the result is
    /// visible, as v1's picker does.
    Applied(String),
}

/// Six lines of 14-column muted labels and their chips, from v1's
/// `render_preview`. The palette is the highlighted theme's, not the active
/// one's.
fn render_preview(frame: &mut Frame, area: Rect, palette: &ThemePalette, chrome: Chrome) {
    use ratatui::style::Color;
    // Secondary, not muted: these name the swatches beside them, and a label
    // dimmer than the thing it labels inverts the reading order.
    let label =
        |text: &'static str| Span::styled(text, Style::default().fg(chrome.palette.text_secondary));
    let chip = |text: &'static str, fg: Color, bg: Option<Color>| {
        let mut style = Style::default().fg(fg);
        if let Some(bg) = bg {
            style = style.bg(bg);
        }
        Span::styled(text, style)
    };
    let space = || Span::raw(" ");
    let swatch = vec![
        Line::from(vec![
            label("  Accent      "),
            chip("█████", palette.accent, None),
            space(),
            chip("█████", palette.accent_bright, None),
        ]),
        Line::from(vec![
            label("  Status      "),
            chip("◐", palette.status_working, None),
            space(),
            chip("◆", palette.status_blocked, None),
            space(),
            chip("●", palette.status_done, None),
            space(),
            chip("○", palette.status_idle, None),
            space(),
            chip("✗", palette.status_error, None),
        ]),
        Line::from(vec![
            label("  Text        "),
            chip("Aa", palette.text_primary, None),
            space(),
            chip("Aa", palette.text_secondary, None),
            space(),
            chip("Aa", palette.text_muted, None),
        ]),
        Line::from(vec![
            label("  Diff        "),
            chip(" + ", palette.diff_added, Some(palette.diff_added_bg)),
            space(),
            chip(" - ", palette.diff_removed, Some(palette.diff_removed_bg)),
        ]),
        // Blocks, like every row above. A hairline `╭─────╮` in the border colour
        // was close to invisible, and it showed only one of the two border roles.
        Line::from(vec![
            label("  Border      "),
            chip("█████", palette.border_unfocused, None),
            space(),
            chip("█████", palette.border_focused, None),
        ]),
        // Shown against a CONTRASTING ground, which is the only way this row can
        // work. It was spaces whose background was `modal_bg`, drawn onto the modal
        // whose background is `modal_bg` — invisible by construction rather than by
        // bad luck. Painting it as ink instead is no better: the ink is the same
        // colour as what is behind it. So the swatch sits on the palette's own text
        // colour, where a dark ground reads as dark and a light one as light.
        Line::from(vec![
            label("  Modal bg    "),
            chip("█████", palette.modal_bg, Some(palette.text_primary)),
        ]),
    ];
    frame.render_widget(Paragraph::new(swatch), area);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn choices() -> Vec<ThemeChoice> {
        Themes::load(None).choices().to_vec()
    }

    #[test]
    fn opening_on_an_early_theme_does_not_scroll_the_first_ones_away() {
        // The picker opens on the ACTIVE theme. `doom` is the fifth preset, and the
        // first row is two tall because it carries the `Dark` header — so the
        // margin walk, which backs off at most MAX_MARGIN rows, stopped one row
        // short of the top and hid `Default` and the header with it. In a pane with
        // twenty-six rows to spare, that read as the dark grouping not existing.
        let choices = choices();
        let matches: Vec<usize> = (0..choices.len()).collect();
        let rows = build_rows(&choices, &matches);
        let doom = choices
            .iter()
            .position(|choice| choice.name == "doom")
            .expect("doom is a preset");
        assert!(
            doom > MAX_MARGIN as usize,
            "or the walk would reach 0 anyway"
        );
        assert_eq!(
            scroll_start(&rows, doom, 26),
            0,
            "everything from the top through the selection fits, so show the top"
        );

        // Still scrolls when it has to, and the selection stays inside the window
        // it chose.
        let last = rows.len() - 1;
        let start = scroll_start(&rows, last, 26);
        assert!(start > 0, "a selection past the pane still scrolls");
        assert!(
            total_height(&rows[start..=last]) <= 26,
            "the selected row must be visible from the offset returned"
        );
    }

    #[test]
    fn an_empty_list_has_somewhere_to_start() {
        // Reachable with a filter that matches nothing; indexing `rows[..=0]` on an
        // empty slice would panic.
        assert_eq!(scroll_start(&[], 0, 20), 0);
    }

    #[test]
    fn one_header_opens_each_section() {
        let choices = choices();
        let matches: Vec<usize> = (0..choices.len()).collect();
        let rows = build_rows(&choices, &matches);
        let headers: Vec<&str> = rows.iter().filter_map(|row| row.header).collect();
        // The presets are dark-then-light, so exactly two sections in order.
        assert_eq!(headers, vec!["Dark", "Light"]);
    }

    #[test]
    fn a_header_disappears_with_the_section_it_opened() {
        let choices = choices();
        let matches: Vec<usize> = choices
            .iter()
            .enumerate()
            .filter(|(_, choice)| !choice.is_light)
            .map(|(index, _)| index)
            .collect();
        let rows = build_rows(&choices, &matches);
        let headers: Vec<&str> = rows.iter().filter_map(|row| row.header).collect();
        assert_eq!(headers, vec!["Dark"]);
    }

    #[test]
    fn the_filter_matches_a_display_name_and_an_id() {
        let choices = choices();
        // "Tokyo Night" is the display name; "tokyo-night" the persisted id.
        let by_name = compute_matches(&choices, "Tokyo");
        let by_id = compute_matches(&choices, "tokyo-night");
        assert!(!by_name.is_empty(), "no theme matched a display name");
        assert!(!by_id.is_empty(), "no theme matched an id");
        assert!(compute_matches(&choices, "").len() == choices.len());
        assert!(compute_matches(&choices, "zzzz").is_empty());
    }

    #[test]
    fn the_selection_stays_on_screen_at_every_position() {
        let choices = choices();
        let matches: Vec<usize> = (0..choices.len()).collect();
        let rows = build_rows(&choices, &matches);
        for selected in 0..rows.len() {
            let start = scroll_start(&rows, selected, 10);
            assert!(start <= selected, "selection scrolled off the top");
            let mut used = 0u16;
            let mut visible = false;
            for row in &rows[start..] {
                if used + row_height(row) > 10 {
                    break;
                }
                used += row_height(row);
                if row.match_index == selected {
                    visible = true;
                    break;
                }
            }
            assert!(visible, "row {selected} was not visible from {start}");
        }
    }

    #[test]
    fn enter_applies_and_persists_the_highlighted_theme() {
        let db = Database::open_in_memory().expect("in-memory database");
        let mut themes = Themes::load(Some(&db));
        let mut modal = ThemeModal::default();
        let press = |code| KeyEvent::new(code, KeyModifiers::NONE);
        modal.on_key(&press(KeyCode::Char('j')), &mut themes, Some(&db));
        let wanted = themes.choices()[1].name.clone();
        match modal.on_key(&press(KeyCode::Enter), &mut themes, Some(&db)) {
            Outcome::Applied(message) => assert!(message.contains(&wanted), "{message}"),
            Outcome::Stay(message) => panic!("apply did not happen: {message:?}"),
        }
        assert_eq!(themes.active_name(), wanted);
        // Where v1 persists it, so a restart and every other process agree.
        assert_eq!(db.get_active_theme().expect("read"), Some(wanted));
    }

    #[test]
    fn an_open_filter_paints_its_own_caret() {
        // Same decision as the settings edit: the caret is a glyph, so it
        // survives the modal being drawn over and needs no arbitration with the
        // panes underneath.
        let palette = crate::session::theme_config::ThemePreset::Default.palette();
        let mut themes = Themes::load(None);
        let mut modal = ThemeModal::default();
        modal.on_key(
            &KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE),
            &mut themes,
            None,
        );
        assert!(modal.filtering(), "`/` opens the filter");

        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(90, 30)).expect("terminal");
        terminal
            .draw(|frame| {
                modal.render(frame, frame.area(), &themes, Chrome::new(&palette));
            })
            .expect("draw");
        let screen: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(
            screen.contains('\u{2588}'),
            "an open filter must show where the keys go"
        );
    }
}
