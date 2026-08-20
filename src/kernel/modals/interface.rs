//! The interface, looking at itself — as system chrome rather than as a pane.
//!
//! Every file the interface is made of, where it came from (bundled / edited /
//! yours / removed) and whether it is actually on screen, with restore and
//! remove. Three things about those files are otherwise invisible from inside
//! thurbox: a pane whose slot `layout.lua` does not place is dropped in silence,
//! a file that fails to load leaves the **last good version** running (so the
//! screen looks right while the file on disk is not what is on it), and a
//! bundled file you deleted looks exactly like one that was never shipped.
//!
//! **Why this is not a plugin.** It used to be one, deliberately: a pane that
//! lists panes is an honest test of whether the plugin API is enough to build a
//! pane with. But it is a *recovery* tool, and a recovery tool that is itself a
//! plugin can be the thing that is broken — a bad edit to it takes away the view
//! you would use to undo the edit. That is the same argument that made help,
//! settings and the theme picker kernel-owned, and it outranks the API test:
//! the API is exercised by every other pane, and `docs/examples/plugin.lua` is
//! the thing an author copies.
//!
//! Being chrome also removes the reachability problem that made it useless in
//! practice. As a centre-slot pane it needed an opening chord of its own, and
//! its chord was `F11` — the one F-key terminals commonly claim for fullscreen.
//! A modal is reached the way every modal is reached.

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use super::chrome::{self, Chrome, Hits};
use crate::kernel::bundled::Source;
use crate::kernel::inventory::{Row, State as FileState, Trust as FileTrust};

/// The interface's files and where they live, as one argument.
///
/// The two always travel together and neither means much alone: a path is
/// relative to the directory, and the directory is the answer to "why did my
/// edit do nothing".
#[derive(Clone, Copy)]
pub struct Files<'a> {
    pub rows: &'a [Row],
    pub dir: &'a str,
}

/// What a restore, a remove, a trust decision or a switch asks the loop to do.
///
/// Named rather than done here for the same reason the settings tab leaves its
/// draft behind: writing a file — or writing the user's decision about one — is
/// not a modal's business, and the loop already owns the one code path that
/// writes one and asks for the reload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Edit {
    File {
        file: String,
        kind: crate::kernel::command::PluginEdit,
    },
    /// Grant or withdraw this file's declared capabilities.
    ///
    /// Applied by the loop, which then reloads: revoking has to make the
    /// capability *absent* on the next frame rather than present-and-refusing,
    /// and the environment is rebuilt at load (design D7).
    Trust { file: String, trusted: bool },
    /// Turn this file off, or back on. The file is untouched either way — which
    /// is the entire reason this exists beside `File`.
    Switch { file: String, off: bool },
}

/// How a file's state reads, and how loudly.
///
/// The order is the sort order: what needs you comes first, so a failure never
/// sits below thirty healthy rows. `visible`/`hidden` are not faults and are
/// styled as the plain text they are.
struct Reading {
    rank: u8,
    glyph: &'static str,
    label: &'static str,
}

fn state_of(row: &Row) -> Reading {
    let (rank, glyph, label) = match row.state {
        FileState::Failed => (1, "✗", "failed"),
        FileState::Removed => (2, "⊘", "removed"),
        FileState::Unplaced => (3, "◌", "no slot"),
        // Between the faults and the healthy: it is not a problem, but it is the
        // answer to "where did my pane go", so it should not be buried.
        FileState::Disabled => (4, "◍", "off"),
        FileState::Visible => (5, "●", "on screen"),
        // Neither drawing nor waiting on anything: a modal at rest.
        FileState::OnDemand => (6, "◐", "on demand"),
        FileState::Hidden => (7, "○", "hidden"),
        FileState::Present => (8, "·", ""),
    };
    Reading { rank, glyph, label }
}

/// The Interface tab's own state: a cursor, and an armed removal.
#[derive(Default)]
pub struct InterfaceTab {
    selected: usize,
    /// The file a second `d` would remove. Cleared by moving away, because a
    /// confirmation that survives the cursor leaving it is a confirmation about
    /// the wrong file.
    armed: Option<String>,
    /// Whether that file is the user's own — so the confirmation can say whether
    /// this is undoable *before* it happens, rather than reporting the asymmetry
    /// afterwards as `restore` does.
    armed_is_ours: bool,
}

impl InterfaceTab {
    /// The rows, ordered: trouble first, then by path.
    ///
    /// Sorted per call rather than cached, because the whole point of the view is
    /// that it is current — a reload changes every one of these answers.
    fn ordered(inventory: &[Row]) -> Vec<&Row> {
        let mut rows: Vec<&Row> = inventory.iter().collect();
        rows.sort_by(|a, b| {
            state_of(a)
                .rank
                .cmp(&state_of(b).rank)
                .then_with(|| a.path.cmp(&b.path))
        });
        rows
    }

    fn current<'a>(&self, inventory: &'a [Row]) -> Option<&'a Row> {
        let rows = Self::ordered(inventory);
        rows.get(self.selected.min(rows.len().saturating_sub(1)))
            .copied()
    }

    /// Rows the last frame drew, so `PageUp`/`PageDown` move by a screenful.
    fn page(&self, hits: &Hits) -> usize {
        hits.rows.len().max(1)
    }

    pub fn armed(&self) -> Option<&str> {
        self.armed.as_deref()
    }

    /// Take a key. Returns a message to report, and an edit for the loop.
    pub fn on_key(
        &mut self,
        key: &KeyEvent,
        inventory: &[Row],
        hits: &Hits,
    ) -> (Option<String>, Option<Edit>) {
        let total = Self::ordered(inventory).len();
        if total == 0 {
            return (None, None);
        }
        self.selected = self.selected.min(total - 1);

        match key.code {
            KeyCode::Char('j') | KeyCode::Down | KeyCode::Tab => {
                self.selected = (self.selected + 1) % total;
                self.armed = None;
            }
            KeyCode::Char('k') | KeyCode::Up | KeyCode::BackTab => {
                self.selected = (self.selected + total - 1) % total;
                self.armed = None;
            }
            KeyCode::PageDown => {
                self.selected = (self.selected + self.page(hits)).min(total - 1);
                self.armed = None;
            }
            KeyCode::PageUp => {
                self.selected = self.selected.saturating_sub(self.page(hits));
                self.armed = None;
            }
            KeyCode::Home => {
                self.selected = 0;
                self.armed = None;
            }
            KeyCode::End => {
                self.selected = total - 1;
                self.armed = None;
            }
            KeyCode::Char('r') => return self.restore(inventory),
            KeyCode::Char('d') => return self.remove(inventory),
            KeyCode::Char('t') => return self.trust(inventory),
            KeyCode::Char(' ') => return self.switch(inventory),
            _ => {}
        }
        (None, None)
    }

    /// Put back the version thurbox ships.
    ///
    /// Covers both undo cases — a file you edited and one you deleted — because
    /// both mean "put back what we ship and forget what happened to it".
    fn restore(&mut self, inventory: &[Row]) -> (Option<String>, Option<Edit>) {
        self.armed = None;
        let Some(row) = self.current(inventory) else {
            return (None, None);
        };
        if let Some(src) = row.source.installed_from() {
            // Nothing to restore *to* either, and the fix is a different command:
            // an installed file is put back by the manager that put it there.
            return (
                Some(format!(
                    "{} came from {src}; `thurbox-cli plugin sync` puts it back",
                    row.path
                )),
                None,
            );
        }
        if row.source.is_theirs() {
            // Nothing to restore *to*: we never shipped it, so there is no
            // shipped version to put back.
            return (
                Some(format!(
                    "{} is yours; thurbox ships no version of it",
                    row.path
                )),
                None,
            );
        }
        (
            None,
            Some(Edit::File {
                file: row.path.clone(),
                kind: crate::kernel::command::PluginEdit::Restore,
            }),
        )
    }

    /// Grant or withdraw trust in the file under the cursor.
    ///
    /// Only meaningful for a file that asks for something. Pressing it on one
    /// that asks for nothing says so rather than silently recording a decision
    /// about a capability it never wanted — a trust list full of files that
    /// need no trust is a list nobody can read.
    fn trust(&mut self, inventory: &[Row]) -> (Option<String>, Option<Edit>) {
        self.armed = None;
        let Some(row) = self.current(inventory) else {
            return (None, None);
        };
        if row.trust == FileTrust::NotAsked {
            return (
                Some(format!("{} asks for nothing to trust it with", row.path)),
                None,
            );
        }
        let granting = row.trust == FileTrust::Untrusted;
        (
            None,
            Some(Edit::Trust {
                file: row.path.clone(),
                trusted: granting,
            }),
        )
    }

    /// Turn the selected file off, or back on.
    ///
    /// No confirmation, deliberately: nothing is at risk, and confirming a
    /// reversible action is what teaches people to confirm without reading —
    /// which is exactly what makes the irreversible one below dangerous.
    fn switch(&mut self, inventory: &[Row]) -> (Option<String>, Option<Edit>) {
        self.armed = None;
        let Some(row) = self.current(inventory) else {
            return (None, None);
        };
        (
            None,
            Some(Edit::Switch {
                file: row.path.clone(),
                off: row.state != FileState::Disabled,
            }),
        )
    }

    /// Delete a file, on the second press.
    ///
    /// Asked twice because it is not undoable in the ordinary sense: removing a
    /// **bundled** file records the removal, so delivery leaves it alone from
    /// then on. That is what makes a bundled pane replaceable by a differently
    /// named one of your own — and what makes an accidental `d` cost you a pane
    /// until you come back here and restore it.
    fn remove(&mut self, inventory: &[Row]) -> (Option<String>, Option<Edit>) {
        let Some(row) = self.current(inventory) else {
            return (None, None);
        };
        let path = row.path.clone();
        if self.armed.as_deref() != Some(path.as_str()) {
            self.armed = Some(path);
            self.armed_is_ours = row.source.is_theirs();
            return (None, None);
        }
        self.armed = None;
        (
            None,
            Some(Edit::File {
                file: path,
                kind: crate::kernel::command::PluginEdit::Remove,
            }),
        )
    }

    pub fn on_click(&mut self, x: u16, y: u16, hits: &Hits) {
        if let Some(index) = hits.row_at(x, y) {
            if index != self.selected {
                self.armed = None;
            }
            self.selected = index;
        }
    }

    /// One row: state, path, and where it came from.
    fn line<'a>(row: &Row, width: usize, chrome: Chrome<'a>, selected: bool) -> Line<'a> {
        let state = state_of(row);
        let body = if selected {
            chrome.selected_item()
        } else {
            chrome.normal_item()
        };
        let mut spans = vec![
            Span::styled(format!("{} ", state.glyph), Style::default()),
            Span::styled(row.path.clone(), body),
        ];

        // Where it came from, and — for a file the user did not write — WHO from.
        // A capability is granted per file, so the source is the fact that has to
        // be legible before anyone presses `t` at it.
        let origin = match &row.source {
            Source::Edited => Some("edited".to_string()),
            Source::User => Some("yours".to_string()),
            Source::Installed { src } => Some(format!("from {src}")),
            Source::InstalledEdited { src } => Some(format!("from {src} · modified")),
            Source::Bundled | Source::Removed => None,
        };
        let mut tail: Vec<&str> = Vec::new();
        if let Some(origin) = &origin {
            tail.push(origin.as_str());
        }
        if !state.label.is_empty() {
            tail.push(state.label);
        }
        // A file that asks to run programs says so here, and says where it
        // stands — this is the whole of the security surface, so it has to be
        // legible in the list rather than discoverable by pressing keys at it.
        if row.trust != FileTrust::NotAsked {
            tail.push(row.trust.as_str());
        }
        if !tail.is_empty() {
            let text = tail.join(" · ");
            let used = row.path.chars().count() + 2;
            let gap = width.saturating_sub(used + text.chars().count()).max(1);
            spans.push(Span::styled(
                format!("{}{text}", " ".repeat(gap)),
                chrome.muted(),
            ));
        }
        Line::from(spans)
    }

    /// The bottom line: the armed removal, the selected row's error, or the keys.
    pub fn footer<'a>(&self, inventory: &[Row], chrome: Chrome<'a>) -> Line<'a> {
        if let Some(path) = &self.armed {
            // The two removals are different acts and must not be worded alike:
            // one can be undone from the binary, the other cannot be undone at
            // all. A reflex learned on the first must not carry to the second.
            let consequence = if self.armed_is_ours {
                " thurbox has no copy — this cannot be undone. d again to confirm"
            } else {
                " restorable afterwards. d again to confirm"
            };
            return Line::from(vec![
                Span::styled(
                    format!(" remove {path}? "),
                    chrome
                        .button(true)
                        .add_modifier(ratatui::style::Modifier::BOLD),
                ),
                Span::styled(consequence, chrome.muted()),
            ]);
        }
        if let Some(error) = self.current(inventory).and_then(|row| row.error.as_deref()) {
            return Line::from(Span::styled(
                format!(" {error}"),
                Style::default().fg(chrome.row_hovered_bg()),
            ));
        }
        // What the selected file is asking for, in words, because `t` is the one
        // key here that hands something away. The row's tail says whether trust was
        // granted; it has no room to say what *for*, and "runs programs" would
        // describe both capabilities while hiding the difference that matters —
        // whether it also takes your keystrokes and stays running.
        if let Some(row) = self.current(inventory) {
            if !row.capabilities.is_empty() {
                let asks: Vec<&str> = row
                    .capabilities
                    .iter()
                    .map(|capability| capability.describe())
                    .collect();
                return Line::from(vec![
                    Span::styled(" asks to: ", chrome.muted()),
                    Span::styled(asks.join(", "), chrome.normal_item()),
                    Span::styled("   t to decide", chrome.muted()),
                ]);
            }
        }
        chrome::hint_line(
            // The descriptions carry their own spacing: `hint_line` joins the
            // spans as they are, so a bare word runs into the next key.
            &[
                ("j/k", " move  "),
                ("space", " on/off  "),
                ("r", " restore  "),
                ("d", " remove  "),
                ("t", " trust  "),
                ("[/]", " tab"),
            ],
            chrome,
        )
    }

    /// Draw the file list into `area`, recording a hitbox per row.
    pub fn render(
        &mut self,
        frame: &mut Frame,
        area: Rect,
        files: Files<'_>,
        chrome: Chrome<'_>,
        hits: &mut Hits,
    ) {
        let (inventory, ui_dir) = (files.rows, files.dir);
        if area.height == 0 || area.width == 0 {
            return;
        }
        // Where the files are, and — said out loud — that adding one is putting a
        // file there. Both answer confusions nothing else can: `THURBOX_UI_DIR`
        // and a dev build each move the live directory, so edits that "did
        // nothing" are usually edits to a file that is not the one running; and
        // "how do I add a pane" otherwise lives only in the guide.
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(format!(" {ui_dir}"), chrome.muted()),
                Span::styled("  ·  add a .lua file here", chrome.muted()),
            ])),
            Rect::new(area.x, area.y, area.width, 1),
        );

        let list = Rect::new(
            area.x,
            area.y + 1,
            area.width,
            area.height.saturating_sub(1),
        );
        if list.height == 0 {
            return;
        }
        let rows = Self::ordered(inventory);
        self.selected = self.selected.min(rows.len().saturating_sub(1));

        let viewport = list.height as usize;
        let (body, track) = chrome::reserve_track(list, rows.len(), viewport);
        // Keep the cursor on screen: the window follows it rather than the
        // other way round.
        let first = self.selected.saturating_sub(viewport.saturating_sub(1));

        for (nth, row) in rows.iter().skip(first).take(viewport).enumerate() {
            let index = first + nth;
            let rect = Rect::new(body.x, body.y + nth as u16, body.width, 1);
            let line = Self::line(row, body.width as usize, chrome, index == self.selected);
            frame.render_widget(Paragraph::new(line), rect);
            hits.rows.push((rect, index));
        }
        if let Some(track) = track {
            chrome::scrollbar(frame, track, rows.len(), viewport, first, chrome);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kernel::inventory::Kind;

    fn row(path: &str, source: Source, state: FileState) -> Row {
        Row {
            path: path.into(),
            name: path.into(),
            kind: Kind::Pane,
            slot: "sessions".into(),
            source,
            state,
            error: None,
            capabilities: Vec::new(),
            trust: FileTrust::NotAsked,
        }
    }

    /// Every cell of one rendered row, as a string.
    fn rendered(row: &Row) -> String {
        let palette = crate::session::theme_config::ThemePreset::Default.palette();
        InterfaceTab::line(row, 78, Chrome::new(&palette), false)
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect()
    }

    /// The list where `t` is pressed has to say what it would be granting.
    ///
    /// The row's tail says whether trust was given; it has no room to say what
    /// *for*, and the two capabilities are two different decisions — one is
    /// bounded output, the other is a process held open on your keystrokes.
    #[test]
    fn the_footer_says_what_the_selected_file_is_asking_for() {
        let palette = crate::session::theme_config::ThemePreset::Default.palette();
        let footer = |rows: &[Row]| -> String {
            let tab = InterfaceTab {
                selected: 0,
                ..Default::default()
            };
            tab.footer(rows, Chrome::new(&palette))
                .spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect()
        };

        let mut asks = row("plugins/85_top.lua", Source::User, FileState::Visible);
        asks.capabilities = vec![crate::kernel::host::Capability::Run];
        let line = footer(std::slice::from_ref(&asks));
        assert!(line.contains("asks to"), "{line}");
        assert!(
            line.contains("output"),
            "the bounded one is named as such: {line}"
        );

        asks.capabilities = vec![crate::kernel::host::Capability::Program];
        let line = footer(std::slice::from_ref(&asks));
        assert!(
            line.contains("interact"),
            "and the interactive one is distinguishable: {line}"
        );

        // A file asking for nothing gets the keys back, not an empty claim.
        let quiet = row(
            "plugins/10_sessions.lua",
            Source::Bundled,
            FileState::Visible,
        );
        let line = footer(std::slice::from_ref(&quiet));
        assert!(!line.contains("asks to"), "{line}");
        assert!(
            line.contains("trust"),
            "the key hints are shown instead: {line}"
        );
    }

    #[test]
    fn a_row_names_its_provenance_and_where_its_trust_stands() {
        // Both are on the row already; what they are NOT is stated on every row.
        // A UI review read a default install — fourteen bundled files, none asking
        // for a capability — and concluded the columns were missing, so this pins
        // that they appear when there is something to say.
        let mut edited = row(
            "plugins/10_sessions.lua",
            Source::Edited,
            FileState::Visible,
        );
        assert!(
            rendered(&edited).contains("edited"),
            "{}",
            rendered(&edited)
        );

        let mine = row("plugins/90_mine.lua", Source::User, FileState::Visible);
        assert!(rendered(&mine).contains("yours"), "{}", rendered(&mine));

        // A pane somebody else wrote is NOT "yours". `run` is granted per file, so
        // the source is the fact that has to be legible before anyone presses `t`
        // at the row — and until this existed, a third-party pane and one you wrote
        // yourself read identically.
        let installed = row(
            "plugins/85_top.lua",
            Source::Installed { src: "top".into() },
            FileState::Visible,
        );
        let line = rendered(&installed);
        assert!(line.contains("from top"), "names the source: {line}");
        assert!(!line.contains("yours"), "{line}");

        let tampered = row(
            "plugins/85_top.lua",
            Source::InstalledEdited { src: "top".into() },
            FileState::Visible,
        );
        let line = rendered(&tampered);
        assert!(
            line.contains("from top") && line.contains("modified"),
            "and says when the contents are no longer that version's: {line}"
        );

        // A file asking to run programs is the whole security surface, so where it
        // stands has to be legible in the list rather than found by pressing keys.
        edited.capabilities = vec![crate::kernel::host::Capability::Run];
        edited.trust = FileTrust::Untrusted;
        assert!(
            rendered(&edited).contains("untrusted"),
            "{}",
            rendered(&edited)
        );
        edited.trust = FileTrust::Drifted;
        let drifted = rendered(&edited);
        assert!(
            drifted.contains("trusted") && drifted.contains("modified"),
            "a trusted file that changed says both: {drifted}"
        );

        // And silence is the right answer for the shipped, unmodified default:
        // saying `bundled · asks nothing` on all fourteen rows would bury the one
        // row that differs.
        let plain = row("plugins/20_agent.lua", Source::Bundled, FileState::Visible);
        let text = rendered(&plain);
        assert!(!text.contains("bundled"), "{text}");
        assert!(!text.contains("trust"), "{text}");
    }

    fn press(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, crossterm::event::KeyModifiers::NONE)
    }

    #[test]
    fn space_turns_a_plugin_off_and_on_again() {
        let mut tab = InterfaceTab::default();
        let on = [row("plugins/90_mine.lua", Source::User, FileState::Visible)];
        let (_, edit) = tab.on_key(&press(KeyCode::Char(' ')), &on, &Hits::default());
        assert_eq!(
            edit,
            Some(Edit::Switch {
                file: "plugins/90_mine.lua".into(),
                off: true
            })
        );

        let off = [row(
            "plugins/90_mine.lua",
            Source::User,
            FileState::Disabled,
        )];
        let (_, edit) = tab.on_key(&press(KeyCode::Char(' ')), &off, &Hits::default());
        assert_eq!(
            edit,
            Some(Edit::Switch {
                file: "plugins/90_mine.lua".into(),
                off: false
            }),
            "the same key must turn it back on"
        );
    }

    #[test]
    fn turning_a_plugin_off_asks_nothing() {
        // Nothing is at risk, and a confirmation on a reversible action is what
        // teaches people to confirm without reading.
        let mut tab = InterfaceTab::default();
        let rows = [row("plugins/90_mine.lua", Source::User, FileState::Visible)];
        let (_, edit) = tab.on_key(&press(KeyCode::Char(' ')), &rows, &Hits::default());
        assert!(edit.is_some(), "it happens on the first press");
        assert!(tab.armed().is_none(), "and arms no confirmation");
    }

    #[test]
    fn deleting_says_whether_it_can_be_undone() {
        // The two removals are different acts. A reflex learned on the
        // restorable one must not carry to the permanent one.
        let palette = crate::session::theme_config::ThemePreset::Default.palette();
        let chrome = Chrome::new(&palette);

        let ours = [row("plugins/90_mine.lua", Source::User, FileState::Visible)];
        let mut tab = InterfaceTab::default();
        tab.on_key(&press(KeyCode::Char('d')), &ours, &Hits::default());
        let mine: String = tab
            .footer(&ours, chrome)
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect();
        assert!(mine.contains("cannot be undone"), "{mine}");

        let shipped = [row(
            "plugins/10_sessions.lua",
            Source::Bundled,
            FileState::Visible,
        )];
        let mut tab = InterfaceTab::default();
        tab.on_key(&press(KeyCode::Char('d')), &shipped, &Hits::default());
        let theirs: String = tab
            .footer(&shipped, chrome)
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect();
        assert!(theirs.contains("restorable"), "{theirs}");
        assert_ne!(mine, theirs, "the two must not be worded alike");
    }

    #[test]
    fn the_keys_are_offered_where_they_can_be_seen() {
        let palette = crate::session::theme_config::ThemePreset::Default.palette();
        let tab = InterfaceTab::default();
        let hints: String = tab
            .footer(&[], Chrome::new(&palette))
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect();
        assert!(
            hints.contains("space"),
            "the reversible key must be offered"
        );
        assert!(hints.contains("on/off"), "{hints}");
    }
}
