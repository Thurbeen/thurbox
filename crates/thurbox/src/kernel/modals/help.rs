use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::{Constraint, Direction, Layout, Position, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use super::chrome::{self, Chrome, Hits, Pill, Replay};
use crate::kernel::registry::{Binding, Registry, HELP_SECTIONS, QUIT_CHORD};

const KEY_WIDTH: usize = 16;

const RESERVED_ROWS: [(&str, &str); 5] = [
    (QUIT_CHORD, "Quit"),
    ("f10", "Reload mods"),
    ("ctrl+h / ctrl+l", "Focus previous / next pane"),
    ("f12", "Perf counters"),
    ("[ / ]", "Previous / next tab, in settings"),
];

struct Row {
    action: String,
    chords: String,
    description: String,
    rebound: bool,
}

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

fn push_binding(rows: &mut Vec<Row>, binding: &Binding) {
    if let Some(row) = rows.iter_mut().find(|row| row.action == binding.action) {
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

fn actions(registry: &Registry) -> Vec<String> {
    rows(registry)
        .into_iter()
        .flat_map(|(_, rows)| rows.into_iter().map(|row| row.action))
        .collect()
}

#[derive(Default)]
pub struct HelpModal {
    selected: usize,
    capturing: bool,
    hits: Hits,
    scroll: usize,
    wheeled: bool,
    track: Option<Rect>,
    max_scroll: usize,
    holding_bar: bool,
}

impl HelpModal {
    pub fn selected(&self) -> usize {
        self.selected
    }
    pub fn capturing(&self) -> bool {
        self.capturing
    }

    pub fn hits(&self) -> &Hits {
        &self.hits
    }

    pub fn on_key(
        &mut self,
        key: &KeyEvent,
        chord: &str,
        registry: &mut Registry,
    ) -> Option<String> {
        let total = actions(registry).len();
        self.wheeled = false;
        if self.capturing {
            return self.capture(key, chord, registry);
        }
        match key.code {
            KeyCode::Char('j') | KeyCode::Down => {
                self.selected = (self.selected + 1).min(total.saturating_sub(1));
            }
            KeyCode::Char('k') | KeyCode::Up => self.selected = self.selected.saturating_sub(1),
            KeyCode::PageDown => {
                self.selected = (self.selected + self.page()).min(total.saturating_sub(1));
            }
            KeyCode::PageUp => self.selected = self.selected.saturating_sub(self.page()),
            KeyCode::Home | KeyCode::Char('g') => self.selected = 0,
            KeyCode::End | KeyCode::Char('G') => self.selected = total.saturating_sub(1),
            KeyCode::Enter | KeyCode::Char('r') => self.capturing = total > 0,
            KeyCode::Char('d') => return self.reset_selected(registry),
            KeyCode::Char('D') => return Some(reset_all(registry)),
            _ => {}
        }
        None
    }

    fn capture(&mut self, key: &KeyEvent, chord: &str, registry: &mut Registry) -> Option<String> {
        self.capturing = false;
        if key.code == KeyCode::Esc {
            return Some("rebinding cancelled".to_string());
        }
        let actions = actions(registry);
        let action = actions.get(self.selected)?.clone();

        let previous: Option<String> = registry
            .bindings()
            .iter()
            .find(|binding| binding.chord == chord && binding.action != action)
            .map(|binding| binding.action.clone());

        match registry.rebind(&action, Some(chord)) {
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

    fn page(&self) -> usize {
        self.hits.rows.len().max(1)
    }

    pub fn on_wheel(&mut self, up: bool) {
        self.wheeled = true;
        self.scroll = if up {
            self.scroll.saturating_sub(1)
        } else {
            (self.scroll + 1).min(self.max_scroll)
        };
    }

    pub fn on_click(&mut self, x: u16, y: u16) {
        self.holding_bar = self
            .track
            .is_some_and(|track| track.contains(Position::new(x, y)));
        if self.holding_bar {
            self.scroll_to(y);
        } else if let Some(index) = self.hits.row_at(x, y) {
            self.selected = index;
        }
    }

    pub fn on_drag(&mut self, y: u16) -> bool {
        if self.holding_bar {
            self.scroll_to(y);
        }
        self.holding_bar
    }

    fn scroll_to(&mut self, y: u16) {
        let Some(track) = self.track else {
            return;
        };
        let last = usize::from(track.height.saturating_sub(1)).max(1);
        let row = usize::from(y.saturating_sub(track.y)).min(last);
        self.scroll = (row * self.max_scroll + last / 2) / last;
        self.wheeled = true;
    }

    pub fn render(&mut self, frame: &mut Frame, area: Rect, registry: &Registry, chrome: Chrome) {
        self.hits.clear();
        let modal = chrome::centered_rect(60, 70, area);
        let inner = chrome::modal_frame(frame, modal, "Keybindings", chrome);
        let [body, footer] = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(0), Constraint::Length(1)])
            .areas(inner);

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
        push_reference_lines(&mut lines, key_width, chrome);

        let total = lines.len();
        let height = usize::from(body.height);
        let scroll = if self.wheeled {
            self.scroll.min(total.saturating_sub(height))
        } else {
            scroll_for(total, height, selected_line)
        };
        self.scroll = scroll;
        self.max_scroll = total.saturating_sub(height);
        let (rows_area, track) = chrome::reserve_track(body, total, height);
        self.track = track;
        frame.render_widget(Paragraph::new(lines).scroll((scroll as u16, 0)), rows_area);
        record_row_hits(
            &mut self.hits.rows,
            &action_lines,
            scroll,
            height,
            rows_area,
        );

        if let Some(track) = track {
            let (len, viewport, position) = if self.wheeled {
                (total - height + 1, height, scroll)
            } else {
                (index, self.hits.rows.len().max(1), self.selected)
            };
            chrome::scrollbar(frame, track, len, viewport, position, chrome);
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

fn push_reference_lines(lines: &mut Vec<Line<'_>>, key_width: usize, chrome: Chrome) {
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

    lines.push(Line::default());
    lines.push(Line::from(Span::styled(
        "Events (for mods: events = { … } + on_event)",
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
        "source, … — a mod ran command(\"emit\", { text = \"<name>\", … })",
        None,
        false,
        chrome,
    ));
}

fn record_row_hits(
    rows: &mut Vec<(Rect, usize)>,
    action_lines: &[(usize, usize)],
    scroll: usize,
    height: usize,
    rows_area: Rect,
) {
    for &(line, action) in action_lines {
        let Some(on_screen) = line.checked_sub(scroll) else {
            continue;
        };
        if on_screen >= height {
            continue;
        }
        rows.push((
            Rect::new(
                rows_area.x,
                rows_area.y + on_screen as u16,
                rows_area.width,
                1,
            ),
            action,
        ));
    }
}

fn key_column(registry: &Registry) -> usize {
    let widest = rows(registry)
        .iter()
        .flat_map(|(_, rows)| rows.iter().map(|row| row.chords.chars().count()))
        .chain(RESERVED_ROWS.iter().map(|(chord, _)| chord.chars().count()))
        .max()
        .unwrap_or(0);
    KEY_WIDTH.max(widest + 2)
}

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

fn scroll_for(total: usize, height: usize, selected_line: usize) -> usize {
    if total <= height || height == 0 {
        return 0;
    }
    selected_line.saturating_sub(height / 2).min(total - height)
}

fn reset_all(registry: &mut Registry) -> String {
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

    fn binding(mod_name: &str, chord: &str, action: &str, group: &str) -> Binding {
        binding_from(
            mod_name,
            chord,
            action,
            "does a thing",
            None,
            false,
            Some(group),
        )
    }

    #[test]
    fn no_row_claims_a_chord_the_kernel_does_not_take() {
        use crate::kernel::registry::RESERVED;
        const HANDLED_ELSEWHERE: [&str; 1] = ["[ / ]"];
        for (chord, description) in RESERVED_ROWS {
            if HANDLED_ELSEWHERE.contains(&chord) {
                continue;
            }
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
    fn the_wheel_turns_at_the_end_without_losing_a_line() {
        let mut help = HelpModal {
            scroll: 5,
            max_scroll: 5,
            ..HelpModal::default()
        };
        help.on_wheel(false);
        help.on_wheel(true);
        assert_eq!(help.scroll, 4);
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
