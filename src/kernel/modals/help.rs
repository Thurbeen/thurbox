//! The keybindings modal: everything declared, and an editor for it.
//!
//! It knows no plugin. It renders [`Registry::bindings`], which is whatever the
//! loaded plugins declared plus the kernel's own — so a plugin added tomorrow
//! appears here with nothing else edited.
//!
//! Grouped by FUNCTION rather than by owner, matching v1's `help_sections()`:
//! you look for "how do I delete a session", not "what does the session list
//! offer". A plugin declares which group each key belongs to, so a group appears,
//! grows and disappears with the plugins that name it — including one v1 never
//! had, which is listed after v1's own.
//!
//! **Why this is an editor**: v1's F1 panel rebinds live, and
//! `Registry::rebind` already carries conflict detection and persistence. While
//! capturing, *every* chord is data — including `ctrl+q`, which no plugin could
//! ever be allowed to swallow. That is the reason the modal layer is the
//! kernel's rather than Lua's.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use super::chrome::{self, Chrome, Hits, Pill, Replay};
use crate::kernel::registry::{Binding, Registry, HELP_SECTIONS, QUIT_CHORD};

/// Narrowest the chord column gets, from v1's `help_line` (`{key:<16}`).
///
/// A *floor*, not a fixed width: v1 pads to 16 and then prints the description,
/// so a chord string longer than that runs straight into it with no gap. v1 never
/// hits it because each of its actions carries one or two short chords; v2 folds
/// an action's alternates onto one row, so `j / down / ctrl+j` is 17 columns and
/// collided. See [`key_column`].
const KEY_WIDTH: usize = 16;

/// The chords the kernel keeps for itself, with what they do.
///
/// **`tab` is deliberately absent.** It used to be listed here as moving focus,
/// which was simply untrue — `RESERVED` does not contain it and nothing binds it, on
/// purpose: every coding agent uses Tab for completion, and a full-screen program in
/// a pane needs it too (`doom`'s automap, vim, a pager). Listing it told users the
/// interface would eat a key it forwards, and a downstream plugin author read that
/// and concluded their pane could never receive it. Focus moves on `ctrl+h`/`ctrl+l`.
///
/// Listed because they are real and cannot be rebound, which is exactly what
/// someone reading help wants to know. Kept in step with
/// [`crate::kernel::registry::RESERVED`] by a test below; the settings tabs are
/// here too because they belong to a modal rather than to the registry — a key
/// nobody can discover is a key nobody uses.
///
/// **Copy and paste used to be listed here** and no longer are: they are
/// declared bindings now ([`crate::kernel::clipboard`]), so they appear in the
/// editable rows above like anything else and can be moved — onto `Cmd+C` on a
/// Mac, where `Ctrl+C` is spent on interrupt.
const RESERVED_ROWS: [(&str, &str); 5] = [
    (QUIT_CHORD, "Quit"),
    ("f10", "Reload plugins"),
    ("ctrl+h / ctrl+l", "Focus previous / next pane"),
    ("f12", "Perf counters"),
    ("[ / ]", "Previous / next tab, in settings"),
];

/// One editable row: an action, its chords, and what it does.
struct Row {
    action: String,
    /// Every chord bound to the action, joined as v1's `chords_display` does.
    chords: String,
    description: String,
    /// True when any of the action's chords is a user override.
    rebound: bool,
}

/// The rows, grouped and ordered the way help is read.
///
/// Rebuilt on every render rather than cached: the registry is the source of
/// truth and a rebind must show up on the very next frame.
fn rows(registry: &Registry) -> Vec<(String, Vec<Row>)> {
    let mut groups: Vec<(String, Vec<Row>)> = Vec::new();
    for binding in registry.bindings() {
        let group = binding.group.clone();
        let position = match groups.iter().position(|(name, _)| *name == group) {
            Some(position) => position,
            None => {
                groups.push((group, Vec::new()));
                groups.len() - 1
            }
        };
        push_binding(&mut groups[position].1, binding);
    }
    // v1's sections first, in its order; anything else follows alphabetically,
    // so a third-party plugin's group appears without editing this list.
    groups.sort_by_key(|(name, _)| {
        (
            HELP_SECTIONS
                .iter()
                .position(|section| section == name)
                .unwrap_or(HELP_SECTIONS.len()),
            name.clone(),
        )
    });
    groups
}

/// Fold a binding into the group's rows: one row per ACTION, its chords joined
/// with " / " (v1's `chords_display`), so `ctrl+b / f2` reads as one binding
/// with an alternate rather than two unrelated rows.
fn push_binding(rows: &mut Vec<Row>, binding: &Binding) {
    if let Some(row) = rows.iter_mut().find(|row| row.action == binding.action) {
        // An override is keyed by action, so an action that *declared* two
        // chords holds the same one twice once it is rebound. Listing `f7 / f7`
        // would report the registry's shape rather than the user's binding.
        if !row.chords.split(" / ").any(|chord| chord == binding.chord) {
            row.chords.push_str(" / ");
            row.chords.push_str(&binding.chord);
        }
        row.rebound = row.rebound || binding.overridden;
        return;
    }
    rows.push(Row {
        action: binding.action.clone(),
        chords: binding.chord.clone(),
        description: binding.description.clone(),
        rebound: binding.overridden,
    });
}

/// Every editable action, in the order the modal lists them.
fn actions(registry: &Registry) -> Vec<String> {
    rows(registry)
        .into_iter()
        .flat_map(|(_, rows)| rows.into_iter().map(|row| row.action))
        .collect()
}

#[derive(Default)]
pub struct HelpModal {
    /// Index into [`actions`] — an action, never a screen line, so a group
    /// header can never be selected.
    selected: usize,
    /// Waiting for the next physical keypress to bind onto the selection.
    capturing: bool,
    /// Where each action row and footer pill was drawn, so a click reaches the
    /// one under it.
    hits: Hits,
}

impl HelpModal {
    pub fn capturing(&self) -> bool {
        self.capturing
    }

    pub fn hits(&self) -> &Hits {
        &self.hits
    }

    /// Take a key. Returns a message to report, if anything happened worth
    /// saying.
    ///
    /// `chord` is the canonical spelling of the same keystroke, folded by the
    /// registry — so a capture stores the chord the registry will later match,
    /// whichever encoding this terminal used to send it.
    ///
    /// `Esc` and the opening chord are handled by the layer above; everything
    /// else lands here, and while capturing that includes chords the kernel
    /// otherwise reserves.
    pub fn on_key(
        &mut self,
        key: &KeyEvent,
        chord: &str,
        registry: &mut Registry,
    ) -> Option<String> {
        let total = actions(registry).len();
        if self.capturing {
            return self.capture(key, chord, registry);
        }
        match key.code {
            KeyCode::Char('j') | KeyCode::Down => {
                self.selected = (self.selected + 1).min(total.saturating_sub(1));
            }
            KeyCode::Char('k') | KeyCode::Up => self.selected = self.selected.saturating_sub(1),
            // A screenful is however many rows the last frame fitted, which is
            // exactly the number of row hitboxes it recorded.
            KeyCode::PageDown => {
                self.selected = (self.selected + self.page()).min(total.saturating_sub(1));
            }
            KeyCode::PageUp => self.selected = self.selected.saturating_sub(self.page()),
            KeyCode::Home | KeyCode::Char('g') => self.selected = 0,
            KeyCode::End | KeyCode::Char('G') => self.selected = total.saturating_sub(1),
            KeyCode::Enter | KeyCode::Char('r') => self.capturing = total > 0,
            // Lower-case resets one, capital resets everything — v1's `d` /
            // `Shift+D` pair.
            KeyCode::Char('d') => return self.reset_selected(registry),
            KeyCode::Char('D') => return Some(reset_all(registry)),
            _ => {}
        }
        None
    }

    /// Bind the captured chord onto the selected action.
    fn capture(&mut self, key: &KeyEvent, chord: &str, registry: &mut Registry) -> Option<String> {
        self.capturing = false;
        if key.code == KeyCode::Esc {
            return Some("rebinding cancelled".to_string());
        }
        let actions = actions(registry);
        let action = actions.get(self.selected)?.clone();

        // Who holds it now, so the report can say what moved. Read before the
        // rebind, because afterwards the chord belongs to the new action.
        let previous: Option<String> = registry
            .bindings()
            .iter()
            .find(|binding| binding.chord == chord && binding.action != action)
            .map(|binding| binding.action.clone());

        match registry.rebind(&action, Some(chord)) {
            // The registry resolves an override ahead of a declaration that
            // merely defaults to the same chord, so the reassignment is real
            // rather than silently shadowed.
            Ok(()) => Some(match previous {
                Some(previous) => format!("{chord} moved from {previous} to {action}"),
                None => format!("{action} is now {chord}"),
            }),
            Err(e) => Some(e),
        }
    }

    fn reset_selected(&self, registry: &mut Registry) -> Option<String> {
        let action = actions(registry).get(self.selected)?.clone();
        Some(match registry.rebind(&action, None) {
            Ok(()) => format!("{action} reset to its default"),
            Err(e) => e,
        })
    }

    /// How far `PageUp`/`PageDown` step: one screenful of rows, never zero.
    fn page(&self) -> usize {
        self.hits.rows.len().max(1)
    }

    /// A click selects the row under it — the same act as `j`/`k`.
    pub fn on_click(&mut self, x: u16, y: u16) {
        if let Some(index) = self.hits.row_at(x, y) {
            self.selected = index;
        }
    }

    pub fn render(&mut self, frame: &mut Frame, area: Rect, registry: &Registry, chrome: Chrome) {
        self.hits.clear();
        let modal = chrome::centered_rect(60, 70, area);
        let inner = chrome::modal_frame(frame, modal, "Keybindings", chrome);
        let [body, footer] = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(0), Constraint::Length(1)])
            .areas(inner);

        // Screen lines, plus where each action row landed among them: the two
        // are built together so the scroll window and the hitboxes cannot
        // disagree about which line a row is on.
        let key_width = key_column(registry);
        let mut lines: Vec<Line> = Vec::new();
        let mut action_lines: Vec<(usize, usize)> = Vec::new();
        let mut index = 0usize;
        let mut selected_line = 0usize;
        for (group, rows) in rows(registry) {
            lines.push(Line::from(Span::styled(group, chrome.section_header())));
            for row in rows {
                let selected = index == self.selected;
                if selected {
                    selected_line = lines.len();
                }
                action_lines.push((lines.len(), index));
                let (chords, marker) = if selected && self.capturing {
                    ("Press the new shortcut… (Esc cancels)".to_string(), None)
                } else {
                    // v1 marks nothing; the Lua port marked a changed row, and
                    // that is worth keeping — it is the only way to tell a
                    // default from a choice you made months ago.
                    (row.chords, row.rebound.then_some(" (rebound)"))
                };
                lines.push(entry_line(
                    key_width,
                    &chords,
                    &row.description,
                    marker,
                    selected,
                    chrome,
                ));
                index += 1;
            }
            lines.push(Line::default());
        }

        // Never selectable, and last: the escape route out of any pane, which a
        // reader needs to know exists but can never change.
        lines.push(Line::from(Span::styled(
            "Fixed (not rebindable)",
            chrome.section_header(),
        )));
        for (chord, description) in RESERVED_ROWS {
            lines.push(entry_line(
                key_width,
                chord,
                description,
                None,
                false,
                chrome,
            ));
        }

        // The events a plugin may subscribe to, from the same table the loader
        // validates against — so what help shows is what `events = { … }` may
        // name, and a plugin author need not leave the interface to find out.
        lines.push(Line::default());
        lines.push(Line::from(Span::styled(
            "Events (for plugins: events = { … } + on_event)",
            chrome.section_header(),
        )));
        let event_width = crate::kernel::events::KERNEL_EVENTS
            .iter()
            .map(|spec| spec.name.chars().count())
            .max()
            .unwrap_or(0)
            + 2;
        for spec in crate::kernel::events::KERNEL_EVENTS {
            lines.push(entry_line(
                event_width,
                spec.name,
                &format!("{} — {}", spec.fields.join(", "), spec.when),
                None,
                false,
                chrome,
            ));
        }
        lines.push(entry_line(
            event_width,
            &format!("{}<name>", crate::kernel::events::USER_PREFIX),
            "source, … — a plugin ran command(\"emit\", { text = \"<name>\", … })",
            None,
            false,
            chrome,
        ));

        let total = lines.len();
        let height = usize::from(body.height);
        let scroll = scroll_for(total, height, selected_line);
        let (rows_area, track) = chrome::reserve_track(body, total, height);
        frame.render_widget(Paragraph::new(lines).scroll((scroll as u16, 0)), rows_area);

        for (line, action) in action_lines {
            let Some(on_screen) = line.checked_sub(scroll) else {
                continue;
            };
            if on_screen >= height {
                continue;
            }
            self.hits.rows.push((
                Rect::new(
                    rows_area.x,
                    rows_area.y + on_screen as u16,
                    rows_area.width,
                    1,
                ),
                action,
            ));
        }

        // The bar tracks ACTIONS, not screen lines, so its thumb matches what
        // the selection cursor moves through — v1's rule, and why the viewport
        // is the number of rows that survived the window above.
        if let Some(track) = track {
            chrome::scrollbar(
                frame,
                track,
                index,
                self.hits.rows.len().max(1),
                self.selected,
                chrome,
            );
        }

        frame.render_widget(
            Paragraph::new(chrome::hint_line(
                &[
                    ("j/k", " move  "),
                    ("Enter/r", " rebind  "),
                    ("d", " reset  "),
                    ("D", " reset all"),
                ],
                chrome,
            )),
            footer,
        );
        // v1's pair and the keys it replays them as: ` Reset all ` is `Shift+D`,
        // ` Close ` the `Esc` the layer above turns into a close.
        self.hits.buttons = chrome::button_bar(
            frame,
            footer,
            &[
                Pill {
                    label: "Reset all",
                    primary: false,
                    key: Replay::new(KeyCode::Char('D'), KeyModifiers::SHIFT, "shift+d"),
                },
                Pill {
                    label: "Close",
                    primary: true,
                    key: Replay::ESC,
                },
            ],
            chrome,
        );
    }
}

/// How wide the chord column has to be for every row to keep a gap.
///
/// Measured across the whole list — the editable rows and the fixed ones — so one
/// long chord string widens the column rather than overlapping its own
/// description, and every row stays aligned. Identical to v1's fixed 16 whenever
/// nothing exceeds it, which is the common case.
fn key_column(registry: &Registry) -> usize {
    let widest = rows(registry)
        .iter()
        .flat_map(|(_, rows)| rows.iter().map(|row| row.chords.chars().count()))
        .chain(RESERVED_ROWS.iter().map(|(chord, _)| chord.chars().count()))
        .max()
        .unwrap_or(0);
    // Two columns of separation. One is enough to stop the collision, but the
    // chord and the description then read as a single run; v1's short chords
    // leave four or five columns of air, and this keeps the widest row legible
    // without pushing every other row out.
    KEY_WIDTH.max(widest + 2)
}

/// One row of the key column and its description. v1's `help_line` /
/// `help_row`: a `›` marker and the active style when selected.
fn entry_line<'a>(
    key_width: usize,
    chords: &str,
    description: &str,
    marker: Option<&str>,
    selected: bool,
    chrome: Chrome,
) -> Line<'a> {
    let key = chrome::pad(chords, key_width);
    let (key_style, desc_style) = if selected {
        (chrome.selected_item(), chrome.selected_item())
    } else {
        (chrome.keybind(), chrome.normal_item())
    };
    Line::from(vec![
        Span::styled(
            format!("{} {key}", if selected { "›" } else { " " }),
            key_style,
        ),
        Span::styled(description.to_string(), desc_style),
        Span::styled(marker.unwrap_or("").to_string(), chrome.keybind()),
    ])
}

/// Keep the selected row visible, roughly centred. v1's `help_body_scroll`.
fn scroll_for(total: usize, height: usize, selected_line: usize) -> usize {
    if total <= height || height == 0 {
        return 0;
    }
    selected_line.saturating_sub(height / 2).min(total - height)
}

/// Clear every override at once. v1's `Shift+D`, which deletes the override
/// file so the defaults are authoritative again.
fn reset_all(registry: &mut Registry) -> String {
    // By ACTION, not by binding: an override is keyed by action, so an action
    // that declared two chords would otherwise be reset — and counted — twice.
    let mut overridden: Vec<String> = registry
        .bindings()
        .iter()
        .filter(|binding| binding.overridden)
        .map(|binding| binding.action.clone())
        .collect();
    overridden.sort();
    overridden.dedup();
    if overridden.is_empty() {
        return "nothing to reset — every key is at its default".to_string();
    }
    let count = overridden.len();
    for action in overridden {
        if let Err(e) = registry.rebind(&action, None) {
            return e;
        }
    }
    format!("reset {count} keybinding(s) to their defaults")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kernel::registry::{binding_from, Setting, RESERVED};
    use crossterm::event::KeyModifiers;

    fn registry_with(bindings: Vec<Binding>) -> Registry {
        let mut registry = Registry::default();
        registry.declare(bindings, Vec::<Setting>::new());
        registry
    }

    fn binding(plugin: &str, chord: &str, action: &str, group: &str) -> Binding {
        binding_from(
            plugin,
            chord,
            action,
            "does a thing",
            None,
            false,
            Some(group),
        )
    }

    /// The rows may not claim a chord the kernel does not actually take.
    ///
    /// The existing check below is one-directional — every `RESERVED` chord appears
    /// here — so an extra row went unnoticed for as long as it took a plugin author
    /// to trust it. `tab` was listed as moving focus while being forwarded to the
    /// pane, and this is the direction that catches the next one.
    #[test]
    fn no_row_claims_a_chord_the_kernel_does_not_take() {
        use crate::kernel::registry::RESERVED;
        // Rows that are not registry chords are documented exceptions, each of which
        // the loop handles before any binding is consulted, or a modal owns.
        const HANDLED_ELSEWHERE: [&str; 1] = ["[ / ]"];
        for (chord, description) in RESERVED_ROWS {
            if HANDLED_ELSEWHERE.contains(&chord) {
                continue;
            }
            // Split the alternates a row may fold together.
            for one in chord.split(" / ") {
                assert!(
                    RESERVED.contains(&one),
                    "help lists {one:?} as {description:?}, but the kernel does not \
                     reserve it — so it is forwarded to the focused pane and this row \
                     is a false claim"
                );
            }
        }
    }

    #[test]
    fn every_reserved_chord_is_listed_as_fixed() {
        // The two lists are written separately because one carries prose; a
        // chord the kernel takes and help does not mention would be a chord
        // nobody can discover.
        let listed = RESERVED_ROWS
            .iter()
            .flat_map(|(chords, _)| chords.split(" / "))
            .collect::<Vec<_>>();
        for reserved in RESERVED {
            assert!(listed.contains(&reserved), "{reserved} is not listed");
        }
    }

    #[test]
    fn an_action_with_two_chords_is_one_row() {
        // v1 lists one row per action and joins its chords, so `ctrl+g / f1`
        // reads as a binding with an alternate rather than two bindings.
        let registry = registry_with(vec![
            binding("kernel", "ctrl+g", "help.open", "Panels"),
            binding("kernel", "f1", "help.open", "Panels"),
        ]);
        let groups = rows(&registry);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].1.len(), 1);
        assert_eq!(groups[0].1[0].chords, "ctrl+g / f1");
    }

    #[test]
    fn known_sections_lead_and_the_rest_follow_alphabetically() {
        let registry = registry_with(vec![
            binding("z", "z", "z.go", "Zebra"),
            binding("a", "a", "a.go", "Apple"),
            binding("s", "s", "s.go", "Sessions"),
            binding("n", "n", "n.go", "Navigation"),
        ]);
        let order: Vec<String> = rows(&registry).into_iter().map(|(name, _)| name).collect();
        assert_eq!(order, ["Navigation", "Sessions", "Apple", "Zebra"]);
    }

    #[test]
    fn capture_binds_the_chord_onto_the_selected_action() {
        // A rebind persists, so the config directory is redirected: a unit test
        // must never write the developer's own `ui.json`.
        let dir = tempfile::tempdir().expect("temp dir");
        let _guard = crate::paths::TestPathGuard::new(dir.path());
        let mut registry = registry_with(vec![
            binding("sessions", "d", "sessions.delete", "Sessions"),
            binding("sessions", "r", "sessions.restart", "Sessions"),
        ]);
        let mut help = HelpModal::default();
        let press = |code| KeyEvent::new(code, KeyModifiers::NONE);
        help.on_key(&press(KeyCode::Char('j')), "j", &mut registry);
        help.on_key(&press(KeyCode::Char('r')), "r", &mut registry);
        assert!(help.capturing());
        help.on_key(&press(KeyCode::F(7)), "f7", &mut registry);
        assert!(!help.capturing());
        let restart = registry
            .bindings()
            .iter()
            .find(|b| b.action == "sessions.restart")
            .expect("declared");
        assert_eq!(restart.chord, "f7");
        assert!(restart.overridden);
    }

    #[test]
    fn scrolling_keeps_the_selection_on_screen() {
        for selected in 0..40 {
            let scroll = scroll_for(40, 10, selected);
            assert!(scroll <= selected, "row {selected} scrolled off the top");
            assert!(
                selected < scroll + 10,
                "row {selected} scrolled off the bottom"
            );
        }
    }
}
