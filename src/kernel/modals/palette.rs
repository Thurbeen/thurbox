//! The command palette: every action the registry knows, one query away.
//!
//! An action used to be reachable only by its chord, in a `Ctrl+<letter>`
//! namespace that readline, the agents and v1's muscle memory have already
//! spent; help could *list* an action but not run it. The palette lists every
//! plugin's keys, every chord-less `commands` declaration and the kernel's own,
//! filters them as you type, and runs the chosen one through the same
//! `on_action` path a key press takes — so a plugin cannot tell the two apart.
//!
//! Chrome like help and settings: plugins contribute rows by declaring data,
//! the kernel draws and dispatches.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use super::chrome::{self, Chrome, Hits, Pill, Replay};
use super::{ModalKind, OWNER};
use crate::kernel::registry::{PaletteRow, Registry, QUIT_CHORD};

/// Non-list rows the modal always spends: both borders, the query line, the
/// footer and one spacer.
const CHROME: u16 = 5;
const MAX_FRAME_PCT: u16 = 80;
const MIN_HEIGHT: u16 = 8;
const WIDTH_PCT: u16 = 60;
const RIGHT_GUTTER: usize = 1;

/// Two actions the kernel handles before the registry is consulted, and so has
/// no binding for — listed here so the palette can still run them.
pub const RELOAD_ACTION: &str = "kernel.reload";
pub const QUIT_ACTION: &str = "kernel.quit";

/// What `Enter` chose: the owner and the action id, exactly as a resolved key
/// carries them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Dispatch {
    pub plugin: String,
    pub action: String,
}

pub enum Outcome {
    Stay(Option<String>),
    /// An action was chosen; the layer closes the modal *before* the loop runs
    /// it, so the action sees the focus state a key press would have.
    Run(Dispatch),
}

/// Every row the palette offers, in listing order.
///
/// The registry's rows, minus the palette's own chord (opening it from inside
/// itself is not an action anyone means), plus the two reserved chords that no
/// binding backs.
pub fn rows(registry: &Registry) -> Vec<PaletteRow> {
    let mut rows: Vec<PaletteRow> = registry
        .palette_rows()
        .into_iter()
        .filter(|row| row.action != ModalKind::Palette.action())
        .collect();
    rows.push(PaletteRow {
        plugin: OWNER.to_string(),
        action: RELOAD_ACTION.to_string(),
        description: "reload the interface from disk".to_string(),
        chords: Some("f10".to_string()),
    });
    rows.push(PaletteRow {
        plugin: OWNER.to_string(),
        action: QUIT_ACTION.to_string(),
        description: "quit (sessions keep running)".to_string(),
        chords: Some(QUIT_CHORD.to_string()),
    });
    rows
}

/// Subsequence match, case-folded: the query's characters in order, not
/// necessarily together. The same rule `ui/lib/fuzzy.lua` applies to the
/// session list and the search strip, so the three cannot disagree about what
/// `thm` finds.
fn subsequence(query: &[char], haystack: &str) -> bool {
    if query.is_empty() {
        return true;
    }
    let mut wanted = 0;
    for c in haystack.chars().flat_map(char::to_lowercase) {
        if c == query[wanted] {
            wanted += 1;
            if wanted == query.len() {
                return true;
            }
        }
    }
    false
}

/// Indices of the rows a query keeps, in listing order.
pub fn matches(rows: &[PaletteRow], query: &str) -> Vec<usize> {
    let needle: Vec<char> = query.chars().flat_map(char::to_lowercase).collect();
    rows.iter()
        .enumerate()
        .filter(|(_, row)| {
            subsequence(&needle, &row.description)
                || subsequence(&needle, &row.action)
                || subsequence(&needle, &row.plugin)
        })
        .map(|(index, _)| index)
        .collect()
}

#[derive(Default)]
pub struct PaletteModal {
    query: String,
    /// Cursor within the *matches*, not the full list.
    selected: usize,
    /// Rows the list showed last frame, so `PgUp`/`PgDn` step a screenful.
    page: usize,
    hits: Hits,
}

impl PaletteModal {
    pub fn hits(&self) -> &Hits {
        &self.hits
    }

    pub fn query(&self) -> &str {
        &self.query
    }

    pub fn on_key(&mut self, key: &KeyEvent, registry: &Registry) -> Outcome {
        let rows = rows(registry);
        let matched = matches(&rows, &self.query);
        let total = matched.len();
        let highlighted = matched.get(self.selected).copied();

        match key.code {
            // Typing is the primary mode, so a bare letter is query text and
            // navigation lives on the keys that cannot be typed.
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.query.push(c);
                self.keep_highlighted(&rows, highlighted);
            }
            KeyCode::Backspace => {
                self.query.pop();
                self.keep_highlighted(&rows, highlighted);
            }
            KeyCode::Down => self.move_by(1, total),
            KeyCode::Up => self.move_by(-1, total),
            KeyCode::Char('n') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.move_by(1, total)
            }
            KeyCode::PageDown => self.move_by(self.page.max(1) as isize, total),
            KeyCode::PageUp => self.move_by(-(self.page.max(1) as isize), total),
            KeyCode::Home => self.selected = 0,
            KeyCode::End => self.selected = total.saturating_sub(1),
            KeyCode::Enter => {
                let Some(row) = matched.get(self.selected).and_then(|i| rows.get(*i)) else {
                    return Outcome::Stay(None);
                };
                return Outcome::Run(Dispatch {
                    plugin: row.plugin.clone(),
                    action: row.action.clone(),
                });
            }
            _ => {}
        }
        Outcome::Stay(None)
    }

    fn move_by(&mut self, step: isize, total: usize) {
        if total == 0 {
            self.selected = 0;
            return;
        }
        let next = self.selected as isize + step;
        self.selected = next.clamp(0, total as isize - 1) as usize;
    }

    /// Keep the cursor on the same action across a query edit, when it
    /// survived — the cursor indexes matches, so holding the index would run a
    /// different action than the one that was highlighted.
    fn keep_highlighted(&mut self, rows: &[PaletteRow], highlighted: Option<usize>) {
        let matched = matches(rows, &self.query);
        self.selected = highlighted
            .and_then(|row| matched.iter().position(|index| *index == row))
            .unwrap_or(0);
    }

    /// A click selects the row under it; `Enter` still runs.
    pub fn on_click(&mut self, x: u16, y: u16) {
        if let Some(index) = self.hits.row_at(x, y) {
            self.selected = index;
        }
    }

    pub fn render(&mut self, frame: &mut Frame, area: Rect, registry: &Registry, chrome: Chrome) {
        self.hits.clear();
        let rows = rows(registry);
        let matched = matches(&rows, &self.query);
        self.selected = self.selected.min(matched.len().saturating_sub(1));

        let desired = (matched.len() as u16)
            .saturating_add(CHROME)
            .max(MIN_HEIGHT);
        let cap = (area.height * MAX_FRAME_PCT / 100).max(MIN_HEIGHT);
        let height = desired.min(cap).min(area.height);
        let modal = chrome::centered_fixed_height_rect(WIDTH_PCT, height, area);
        let inner = chrome::modal_frame(frame, modal, "Commands", chrome);
        if inner.width == 0 || inner.height == 0 {
            return;
        }

        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(1),
                Constraint::Min(1),
                Constraint::Length(1),
            ])
            .split(inner);

        self.render_head(frame, chunks[0], matched.len(), rows.len(), chrome);
        self.render_rows(frame, chunks[1], &rows, &matched, chrome);

        self.hits.buttons = chrome::button_bar(
            frame,
            chunks[2],
            &[
                Pill {
                    label: "Run",
                    primary: true,
                    key: Replay::ENTER,
                },
                Pill {
                    label: "Close",
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
            .map_or(chunks[2].width, |(rect, _)| {
                rect.x.saturating_sub(chunks[2].x + 1)
            });
        frame.render_widget(
            Paragraph::new(chrome::hint_line(
                &[("type", " filter  "), ("↑/↓", " move  "), ("Enter", " run")],
                chrome,
            )),
            Rect {
                width: hint_width,
                ..chunks[2]
            },
        );
    }

    fn render_head(
        &self,
        frame: &mut Frame,
        area: Rect,
        matched: usize,
        total: usize,
        chrome: Chrome,
    ) {
        let count = format!("{matched}/{total}");
        let mut head = vec![Span::styled(
            " > ",
            Style::default().fg(chrome.palette.accent),
        )];
        if self.query.is_empty() {
            head.push(Span::styled("type to filter commands", chrome.muted()));
        } else {
            head.push(Span::styled(self.query.clone(), chrome.normal_item()));
        }
        head.push(Span::styled(
            "█",
            Style::default().fg(chrome.palette.accent),
        ));
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
        rows: &[PaletteRow],
        matched: &[usize],
        chrome: Chrome,
    ) {
        if matched.is_empty() || area.height == 0 || area.width == 0 {
            frame.render_widget(
                Paragraph::new(Line::from(Span::styled(
                    "  no matching command",
                    chrome.muted(),
                ))),
                area,
            );
            self.page = 0;
            return;
        }
        let (rows_area, track) =
            chrome::reserve_track(area, matched.len(), usize::from(area.height));
        let height = usize::from(rows_area.height);
        let start = scroll_for(matched.len(), height, self.selected);
        let width = usize::from(rows_area.width);
        // The owner column is as wide as the widest owner on screen, so the
        // descriptions line up; the chord is right-aligned against the border.
        let owner_width = matched
            .iter()
            .filter_map(|index| rows.get(*index))
            .map(|row| row.plugin.chars().count())
            .max()
            .unwrap_or(0);

        let mut lines: Vec<Line> = Vec::new();
        for (on_screen, match_index) in (start..matched.len().min(start + height)).enumerate() {
            let row = &rows[matched[match_index]];
            let selected = match_index == self.selected;
            self.hits.rows.push((
                Rect::new(
                    rows_area.x,
                    rows_area.y + on_screen as u16,
                    rows_area.width,
                    1,
                ),
                match_index,
            ));
            let marker = if selected { " ▸ " } else { "   " };
            let owner = chrome::pad(&row.plugin, owner_width + 1);
            let chord = row.chords.clone().unwrap_or_default();
            let description = if row.description.is_empty() {
                row.action.clone()
            } else {
                row.description.clone()
            };
            // Two columns of air before the chord, so a truncated description
            // never runs into it.
            let fixed = marker.chars().count() + owner.chars().count() + chord.chars().count() + 3;
            let room = width.saturating_sub(fixed);
            let description = chrome::truncate(&description, room);
            let gap = room.saturating_sub(description.chars().count());
            let (owner_style, text_style) = if selected {
                (chrome.selected_item(), chrome.selected_item())
            } else {
                (chrome.muted(), chrome.normal_item())
            };
            lines.push(Line::from(vec![
                Span::styled(marker.to_string(), text_style),
                Span::styled(owner, owner_style),
                Span::styled(description, text_style),
                Span::raw(" ".repeat(gap + 2)),
                Span::styled(chord, chrome.keybind()),
            ]));
        }
        self.page = self.hits.rows.len();
        frame.render_widget(Paragraph::new(lines), rows_area);

        if let Some(track) = track {
            chrome::scrollbar(frame, track, matched.len(), height, self.selected, chrome);
        }
    }
}

/// Keep the selection visible, roughly centred.
fn scroll_for(total: usize, height: usize, selected: usize) -> usize {
    if total <= height || height == 0 {
        return 0;
    }
    selected.saturating_sub(height / 2).min(total - height)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kernel::registry::{binding_from, CommandDecl, Setting};

    fn registry() -> Registry {
        let mut registry = Registry::default();
        registry.declare(
            vec![
                binding_from(
                    "sessions",
                    "ctrl+d",
                    "sessions.delete",
                    "delete the selected session",
                    Some("global"),
                    false,
                    None,
                ),
                binding_from(
                    "agent",
                    "f8",
                    "shell.open",
                    "open a shell",
                    None,
                    false,
                    None,
                ),
            ],
            Vec::<Setting>::new(),
        );
        registry.declare_commands(vec![CommandDecl {
            plugin: "mine".into(),
            action: "mine.export".into(),
            description: "export the list".into(),
        }]);
        registry
    }

    #[test]
    fn every_kind_of_action_is_a_row() {
        let rows = rows(&registry());
        let actions: Vec<&str> = rows.iter().map(|row| row.action.as_str()).collect();
        assert!(actions.contains(&"sessions.delete"));
        assert!(actions.contains(&"shell.open"));
        assert!(actions.contains(&"mine.export"));
        assert!(actions.contains(&RELOAD_ACTION));
        assert!(actions.contains(&QUIT_ACTION));
        let export = rows.iter().find(|row| row.action == "mine.export").unwrap();
        assert_eq!(export.chords, None, "a chord-less command shows no chord");
        let delete = rows
            .iter()
            .find(|row| row.action == "sessions.delete")
            .unwrap();
        assert_eq!(delete.chords.as_deref(), Some("ctrl+d"));
    }

    #[test]
    fn the_filter_is_a_subsequence_over_description_id_and_owner() {
        let rows = rows(&registry());
        let by_description = matches(&rows, "dls");
        assert!(by_description
            .iter()
            .any(|i| rows[*i].action == "sessions.delete"));
        let by_id = matches(&rows, "mine.exp");
        assert_eq!(by_id.len(), 1);
        let by_owner = matches(&rows, "agent");
        assert!(by_owner.iter().any(|i| rows[*i].action == "shell.open"));
        assert!(matches(&rows, "zzzz").is_empty());
        assert_eq!(matches(&rows, "").len(), rows.len());
    }

    #[test]
    fn enter_runs_the_highlighted_row_and_refining_keeps_it() {
        let registry = registry();
        let mut modal = PaletteModal::default();
        let press = |code| KeyEvent::new(code, KeyModifiers::NONE);
        for c in "export".chars() {
            modal.on_key(&press(KeyCode::Char(c)), &registry);
        }
        match modal.on_key(&press(KeyCode::Enter), &registry) {
            Outcome::Run(dispatch) => {
                assert_eq!(dispatch.plugin, "mine");
                assert_eq!(dispatch.action, "mine.export");
            }
            Outcome::Stay(_) => panic!("Enter must run the selection"),
        }
    }

    #[test]
    fn refining_the_query_keeps_the_cursor_on_the_same_action() {
        let mut registry = Registry::default();
        registry.declare(
            vec![
                binding_from("a", "1", "a.one", "delete session", None, false, None),
                binding_from("a", "2", "a.two", "delete all", None, false, None),
            ],
            Vec::<Setting>::new(),
        );
        let mut modal = PaletteModal::default();
        let press = |code| KeyEvent::new(code, KeyModifiers::NONE);
        for c in "delete".chars() {
            modal.on_key(&press(KeyCode::Char(c)), &registry);
        }
        modal.on_key(&press(KeyCode::Down), &registry);
        // Both still match; the cursor must stay on "delete all" rather than
        // snapping back to the first match.
        for c in " a".chars() {
            modal.on_key(&press(KeyCode::Char(c)), &registry);
        }
        match modal.on_key(&press(KeyCode::Enter), &registry) {
            Outcome::Run(dispatch) => assert_eq!(dispatch.action, "a.two"),
            Outcome::Stay(_) => panic!("Enter must run the selection"),
        }
    }

    #[test]
    fn nothing_matching_means_enter_does_nothing() {
        let registry = registry();
        let mut modal = PaletteModal::default();
        let press = |code| KeyEvent::new(code, KeyModifiers::NONE);
        for c in "zzzz".chars() {
            modal.on_key(&press(KeyCode::Char(c)), &registry);
        }
        assert!(matches!(
            modal.on_key(&press(KeyCode::Enter), &registry),
            Outcome::Stay(None)
        ));
    }
}
