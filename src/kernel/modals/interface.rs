//! The interface, looking at itself — as system chrome rather than as a pane.
//!
//! Every file the interface is made of, where it came from (shipped / edited /
//! yours / installed) and whether it is actually on screen — with, for the
//! selected file, why and what changes it — and restore, remove, trust and
//! switch. Three things about those files are otherwise invisible from inside
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
//! the API is exercised by every other pane, and `examples/lua/plugin.lua` is
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
use crate::kernel::inventory::{Kind, Row, State as FileState, Trust as FileTrust};

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

/// What a file is listed under.
///
/// Panes first, because they are what an operator is looking for when a pane
/// is missing; the files that only support them follow in the order an edit
/// usually reaches them. Grouping by kind rather than by package: a package's
/// files are all panes and modules, so its name is on each row instead.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Group {
    Panes,
    /// `layout.lua` and `plugins.toml`: what decides which panes exist and where.
    /// A layout-preset picker would land here, on the arrangement's row.
    Layout,
    Modules,
    Docs,
}

impl Group {
    fn of(kind: Kind) -> Self {
        match kind {
            Kind::Pane => Group::Panes,
            Kind::Arrangement | Kind::Manifest => Group::Layout,
            Kind::Module => Group::Modules,
            Kind::Doc => Group::Docs,
        }
    }

    fn title(self) -> &'static str {
        match self {
            Group::Panes => "PANES",
            Group::Layout => "LAYOUT",
            Group::Modules => "MODULES",
            Group::Docs => "DOCS",
        }
    }
}

/// How a file's state reads, and how loudly.
///
/// One word per state, and no word shared with a source or a key: `removed`
/// used to be both a source and a state, and `on/off` a key beside an `off`
/// state. The rank is the sort order within a group — what needs you comes
/// first, so a failure never sits below thirty healthy rows.
struct Reading {
    rank: u8,
    glyph: &'static str,
    word: &'static str,
    /// Whether the row is a fault, and so drawn in the danger colour.
    fault: bool,
}

fn state_of(row: &Row) -> Reading {
    let (rank, glyph, word, fault) = match row.state {
        FileState::Failed => (1, "✗", "failed", true),
        FileState::Removed => (2, "⊘", "deleted", true),
        FileState::Unplaced => (3, "◌", "not placed", false),
        // Between the faults and the healthy: it is not a problem, but it is the
        // answer to "where did my pane go", so it should not be buried.
        FileState::Disabled => (4, "◍", "off", false),
        FileState::Visible => (5, "●", "on screen", false),
        // Neither drawing nor waiting on anything: a modal at rest.
        FileState::OnDemand => (6, "◐", "on demand", false),
        FileState::Hidden => (7, "○", "hidden", false),
        // Not a state so much as what the file is for; the group header already
        // says what kind it is, so this says what it does.
        FileState::Present => (
            8,
            "·",
            match row.kind {
                Kind::Pane => "decorates",
                Kind::Module => "on require",
                Kind::Arrangement | Kind::Manifest => "in use",
                Kind::Doc => "guide",
            },
            false,
        ),
    };
    Reading {
        rank,
        glyph,
        word,
        fault,
    }
}

/// The package an installed file came from, by name: the last segment of its
/// source, the way `plugin install` names it. The whole source is a path or a
/// URL, which is what used to push the state off the row.
fn package(src: &str) -> &str {
    let src = src.trim_end_matches('/');
    let last = src.rsplit(['/', ':']).next().unwrap_or(src);
    last.strip_suffix(".git").unwrap_or(last)
}

/// Where a file came from, as the row says it. Silent for the shipped,
/// untouched default: saying `shipped` on every bundled row would bury the one
/// row that differs.
fn origin(source: &Source) -> Option<String> {
    match source {
        Source::Edited => Some("edited".to_string()),
        Source::User => Some("yours".to_string()),
        Source::Installed { src } => Some(format!("from {}", package(src))),
        Source::InstalledEdited { src } => Some(format!("from {}, edited", package(src))),
        Source::Bundled | Source::Removed => None,
    }
}

/// Where a file came from, in full — the details line, where there is room.
fn provenance(source: &Source) -> String {
    match source {
        Source::Bundled => "shipped with thurbox, unchanged".to_string(),
        Source::Edited => "shipped with thurbox, edited here".to_string(),
        Source::User => "your own file".to_string(),
        Source::Removed => "shipped with thurbox, deleted here".to_string(),
        Source::Installed { src } => format!("installed from {src}"),
        Source::InstalledEdited { src } => format!("installed from {src}, edited since"),
    }
}

/// Where a file stands on what it asks for, as the row says it.
fn trust_word(trust: FileTrust) -> Option<&'static str> {
    match trust {
        FileTrust::NotAsked => None,
        FileTrust::Untrusted => Some("untrusted"),
        FileTrust::Trusted => Some("trusted"),
        FileTrust::Drifted => Some("trusted, changed"),
    }
}

/// Why the selected file is in the state it is, and — when that is not what
/// the operator wants — the one thing that changes it.
fn reason(row: &Row) -> String {
    match row.state {
        FileState::Failed => match &row.error {
            Some(error) => error.clone(),
            None => "did not load; fix it and press F10, or space turns it off".to_string(),
        },
        FileState::Removed => "you deleted this shipped file; r puts it back".to_string(),
        FileState::Unplaced => format!(
            "layout.lua places no slot \"{0}\" at this size; add {{ slot = \"{0}\" }} to layout.lua",
            row.slot
        ),
        FileState::Disabled => {
            "turned off here, the file is untouched; space turns it back on".to_string()
        }
        FileState::Visible => "drawn on screen now".to_string(),
        FileState::OnDemand => "draws when something opens it".to_string(),
        FileState::Hidden => format!(
            "slot \"{}\" is placed, but another pane holds it or its column is closed",
            row.slot
        ),
        FileState::Present => match row.kind {
            Kind::Pane => "draws into another pane rather than a slot of its own",
            Kind::Module => "loaded when a pane requires it",
            Kind::Arrangement => "decides where every pane goes",
            Kind::Manifest => "what `thurbox-cli plugin sync` installs",
            Kind::Doc => "guidance for whoever edits this directory",
        }
        .to_string(),
    }
}

/// What the selected file asks for, and where the operator's answer stands —
/// the line that has to be read before `t`, since `t` is the one key here that
/// hands something away.
fn asks(row: &Row) -> Option<String> {
    if row.capabilities.is_empty() {
        return None;
    }
    let wants: Vec<&str> = row
        .capabilities
        .iter()
        .map(|capability| capability.describe())
        .collect();
    let standing = match row.trust {
        FileTrust::Untrusted => " — not granted; t grants it",
        FileTrust::Trusted => " — granted; t revokes it",
        FileTrust::Drifted => " — granted, but the file changed since; t revokes it",
        FileTrust::NotAsked => "",
    };
    Some(format!("asks to: {}{standing}", wants.join(", ")))
}

/// The file's kind and, for a pane, the slot it wants.
fn what(row: &Row) -> String {
    if row.kind == Kind::Pane && !row.slot.is_empty() {
        format!("pane · slot \"{}\"", row.slot)
    } else {
        row.kind.as_str().to_string()
    }
}

/// Whether `r` would change anything: an edited or deleted shipped file. A
/// restore of an untouched one writes the same bytes back, and an installed one
/// is put back by `plugin sync`, not from the binary.
fn restorable(row: &Row) -> bool {
    matches!(row.source, Source::Edited | Source::Removed)
}

/// The least of a path a narrow row keeps before it starts cutting the words
/// after it: enough to tell `plugins/85_top.lua` from its neighbours.
const PATH_FLOOR: usize = 20;

/// Lines the selected row's details take under the list.
const DETAILS: u16 = 3;

/// What a second press of the same key will do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Armed {
    /// `d`: delete the file. `ours` says whether it is the user's own, so the
    /// question can say whether this is undoable *before* it happens.
    Remove { ours: bool },
    /// `r` on an edited file: the shipped copy replaces the edit.
    Restore { layout: bool },
}

/// The Interface tab's own state: a cursor, and an armed confirmation.
#[derive(Default)]
pub struct InterfaceTab {
    selected: usize,
    /// The file a second press would act on, and which act. Cleared by moving
    /// away, because a confirmation that survives the cursor leaving it is a
    /// confirmation about the wrong file.
    armed: Option<(String, Armed)>,
    /// The file under the cursor, by path. An action reorders the list — a pane
    /// turned off sorts above the healthy ones — so an index alone would leave
    /// the cursor on whichever file slid into its place.
    following: Option<String>,
}

impl InterfaceTab {
    /// The rows, ordered: by group, trouble first within one, then by path.
    ///
    /// Sorted per call rather than cached, because the whole point of the view is
    /// that it is current — a reload changes every one of these answers.
    fn ordered(inventory: &[Row]) -> Vec<&Row> {
        let mut rows: Vec<&Row> = inventory.iter().collect();
        rows.sort_by(|a, b| {
            Group::of(a.kind)
                .cmp(&Group::of(b.kind))
                .then_with(|| state_of(a).rank.cmp(&state_of(b).rank))
                .then_with(|| a.path.cmp(&b.path))
        });
        rows
    }

    /// Put the cursor back on the file it was on, wherever that now sorts, and
    /// remember whichever file it is on from here.
    fn follow(&mut self, rows: &[&Row]) {
        if let Some(at) = self
            .following
            .as_deref()
            .and_then(|path| rows.iter().position(|row| row.path == path))
        {
            self.selected = at;
        }
        self.selected = self.selected.min(rows.len().saturating_sub(1));
        self.following = rows.get(self.selected).map(|row| row.path.clone());
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
        self.armed.as_ref().map(|(path, _)| path.as_str())
    }

    /// Take a key. Returns a message to report, and an edit for the loop.
    pub fn on_key(
        &mut self,
        key: &KeyEvent,
        inventory: &[Row],
        hits: &Hits,
    ) -> (Option<String>, Option<Edit>) {
        let ordered = Self::ordered(inventory);
        let total = ordered.len();
        if total == 0 {
            return (None, None);
        }
        self.follow(&ordered);

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
        self.following = ordered.get(self.selected).map(|row| row.path.clone());
        (None, None)
    }

    /// Whether `path` is armed for `act` — so a second press of the *same* key
    /// confirms, and a different key asks its own question instead.
    fn is_armed(&self, path: &str, act: fn(Armed) -> bool) -> bool {
        self.armed
            .as_ref()
            .is_some_and(|(armed, what)| armed == path && act(*what))
    }

    /// Put back the version thurbox ships.
    ///
    /// Covers both undo cases — a file you edited and one you deleted — because
    /// both mean "put back what we ship and forget what happened to it". Only
    /// the first loses anything, so only the first asks: nothing keeps an
    /// edited pane's contents, and `layout.lua`'s are moved aside.
    fn restore(&mut self, inventory: &[Row]) -> (Option<String>, Option<Edit>) {
        let Some(row) = self.current(inventory) else {
            return (None, None);
        };
        if let Some(src) = row.source.installed_from() {
            self.armed = None;
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
            self.armed = None;
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
        let path = row.path.clone();
        if row.source == Source::Edited
            && !self.is_armed(&path, |what| matches!(what, Armed::Restore { .. }))
        {
            let layout = row.kind == Kind::Arrangement;
            self.armed = Some((path, Armed::Restore { layout }));
            return (None, None);
        }
        self.armed = None;
        (
            None,
            Some(Edit::File {
                file: path,
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
        if !self.is_armed(&path, |what| matches!(what, Armed::Remove { .. })) {
            let ours = row.source.is_theirs();
            self.armed = Some((path, Armed::Remove { ours }));
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
            self.following = None;
        }
    }

    /// One row: pointer, state glyph, path, and the words — where it came from,
    /// its state, and where its trust stands.
    ///
    /// The words are what the row is for, so at a narrow width the path is what
    /// gives way, never them.
    fn line<'a>(row: &Row, width: usize, chrome: Chrome<'a>, selected: bool) -> Line<'a> {
        let state = state_of(row);
        let (pointer, body) = if selected {
            ("▸ ", chrome.selected_item())
        } else {
            ("  ", chrome.normal_item())
        };
        let glyph = if state.fault {
            Style::default().fg(chrome.palette.danger)
        } else {
            Style::default()
        };

        // State first: it is the word the row is for. Where it came from goes
        // last, so it is what a narrow row cuts.
        let mut words: Vec<String> = vec![state.word.to_string()];
        words.extend(trust_word(row.trust).map(str::to_string));
        words.extend(origin(&row.source));
        let tail = words.join(" · ");

        // 2 pointer + glyph + 1 space, then the path, a gap of at least 2, the
        // tail. The path gives way first, down to enough of it to recognise;
        // past that the tail is cut from its end.
        let lead = 2 + state.glyph.chars().count() + 1;
        let floor = row.path.chars().count().min(PATH_FLOOR);
        let room = width
            .saturating_sub(lead + 2 + tail.chars().count())
            .max(floor);
        let path = chrome::truncate(&row.path, room);
        let tail = chrome::truncate(&tail, width.saturating_sub(lead + path.chars().count() + 2));
        let gap = width
            .saturating_sub(lead + path.chars().count() + tail.chars().count())
            .max(1);
        let tail_style = if state.fault {
            Style::default().fg(chrome.palette.danger)
        } else {
            chrome.muted()
        };
        Line::from(vec![
            Span::styled(pointer, body),
            Span::styled(format!("{} ", state.glyph), glyph),
            Span::styled(path, body),
            Span::raw(" ".repeat(gap)),
            Span::styled(tail, tail_style),
        ])
    }

    /// The selected row, explained: what it is and where it came from, why it
    /// is in the state it is with the fix, and what it asks for.
    ///
    /// Always [`DETAILS`] lines, so the modal does not change height as the
    /// cursor moves.
    fn details<'a>(row: Option<&Row>, chrome: Chrome<'a>) -> Vec<Line<'a>> {
        let Some(row) = row else {
            return vec![Line::default(); DETAILS as usize];
        };
        let state = state_of(row);
        let reason_style = if state.fault {
            Style::default().fg(chrome.palette.danger)
        } else {
            chrome.normal_item()
        };
        vec![
            Line::from(Span::styled(
                format!(" {} · {}", what(row), provenance(&row.source)),
                chrome.muted(),
            )),
            Line::from(Span::styled(format!(" {}", reason(row)), reason_style)),
            match asks(row) {
                Some(asks) => Line::from(Span::styled(format!(" {asks}"), chrome.normal_item())),
                None => Line::default(),
            },
        ]
    }

    /// The bottom line: the armed question, or the keys the selected row
    /// answers to — only those, so a key on screen is a key that does something.
    pub fn footer<'a>(&self, inventory: &[Row], chrome: Chrome<'a>) -> Line<'a> {
        if let Some((path, armed)) = &self.armed {
            // The two removals are different acts and must not be worded alike:
            // one can be undone from the binary, the other cannot be undone at
            // all. A reflex learned on the first must not carry to the second.
            let (question, consequence) = match armed {
                Armed::Remove { ours: true } => (
                    "delete",
                    " thurbox has no copy — this cannot be undone. d again to confirm",
                ),
                Armed::Remove { ours: false } => {
                    ("delete", " restorable afterwards. d again to confirm")
                }
                Armed::Restore { layout: true } => (
                    "restore",
                    " your layout.lua is moved to layout.lua.bak. r again to confirm",
                ),
                Armed::Restore { layout: false } => (
                    "restore",
                    " your edits are lost — nothing keeps a copy. r again to confirm",
                ),
            };
            return Line::from(vec![
                Span::styled(
                    format!(" {question} {path}? "),
                    chrome
                        .button(true)
                        .add_modifier(ratatui::style::Modifier::BOLD),
                ),
                Span::styled(consequence, chrome.muted()),
            ]);
        }
        // The descriptions carry their own spacing: `hint_line` joins the spans
        // as they are, so a bare word runs into the next key.
        let mut hints: Vec<(&str, &str)> = vec![("j/k", " move  ")];
        if let Some(row) = self.current(inventory) {
            if row.kind == Kind::Pane && row.state != FileState::Removed {
                hints.push(if row.state == FileState::Disabled {
                    ("space", " turn on  ")
                } else {
                    ("space", " turn off  ")
                });
            }
            if restorable(row) {
                hints.push(("r", " restore  "));
            }
            if row.state != FileState::Removed {
                hints.push(("d", " delete  "));
            }
            match row.trust {
                FileTrust::Untrusted => hints.push(("t", " trust  ")),
                FileTrust::Trusted | FileTrust::Drifted => hints.push(("t", " revoke  ")),
                FileTrust::NotAsked => {}
            }
        }
        hints.push(("[/]", " tab"));
        chrome::hint_line(&hints, chrome)
    }

    /// Draw the file list into `area`, the selected row's details under it, and
    /// record a hitbox per row.
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
                Span::styled("  ·  add a pane: a .lua file in plugins/", chrome.muted()),
            ])),
            Rect::new(area.x, area.y, area.width, 1),
        );

        let rows = Self::ordered(inventory);
        if rows.is_empty() {
            if area.height > 1 {
                frame.render_widget(
                    Paragraph::new(Span::styled(
                        " no interface files here yet — thurbox writes the shipped ones when it starts",
                        chrome.muted(),
                    )),
                    Rect::new(area.x, area.y + 1, area.width, 1),
                );
            }
            return;
        }
        self.follow(&rows);

        // The details are pinned under the list; on a screen too short for both,
        // the list keeps its rows and the details give way.
        let details = DETAILS.min(area.height.saturating_sub(2));
        let list = Rect::new(
            area.x,
            area.y + 1,
            area.width,
            area.height.saturating_sub(1 + details),
        );
        if details > 0 {
            let below = Rect::new(area.x, list.y + list.height, area.width, details);
            frame.render_widget(
                Paragraph::new(Self::details(rows.get(self.selected).copied(), chrome)),
                below,
            );
        }
        if list.height == 0 {
            return;
        }

        // Group headers are lines of their own and never selectable, so the
        // window is built over lines and each carries the row it selects.
        let mut lines: Vec<Option<usize>> = Vec::with_capacity(rows.len() + 4);
        let mut group = None;
        for (index, row) in rows.iter().enumerate() {
            let this = Group::of(row.kind);
            if group != Some(this) {
                lines.push(None);
                group = Some(this);
            }
            lines.push(Some(index));
        }
        let at = lines
            .iter()
            .position(|line| *line == Some(self.selected))
            .unwrap_or(0);

        let viewport = list.height as usize;
        let (body, track) = chrome::reserve_track(list, lines.len(), viewport);
        // Keep the cursor on screen — and its group's header with it, when the
        // cursor is the first row under one.
        let first = at
            .saturating_sub(viewport.saturating_sub(1))
            .min(lines.len().saturating_sub(viewport));
        let first = if viewport > 1 && at > 0 && lines[at - 1].is_none() && first == at {
            at - 1
        } else {
            first
        };

        for (nth, line) in lines.iter().skip(first).take(viewport).enumerate() {
            let rect = Rect::new(body.x, body.y + nth as u16, body.width, 1);
            match line {
                Some(index) => {
                    let row = rows[*index];
                    let drawn =
                        Self::line(row, body.width as usize, chrome, *index == self.selected);
                    frame.render_widget(Paragraph::new(drawn), rect);
                    hits.rows.push((rect, *index));
                }
                None => {
                    // The header of the group the next row belongs to.
                    let next = lines[first + nth + 1..]
                        .iter()
                        .flatten()
                        .next()
                        .map(|index| Group::of(rows[*index].kind));
                    if let Some(next) = next {
                        frame.render_widget(
                            Paragraph::new(Span::styled(
                                format!(" {}", next.title()),
                                chrome.section_header(),
                            )),
                            rect,
                        );
                    }
                }
            }
        }
        if let Some(track) = track {
            chrome::scrollbar(frame, track, lines.len(), viewport, first, chrome);
        }
    }

    /// Lines the list needs: a row per file and a header per group present.
    pub fn listed(inventory: &[Row]) -> usize {
        let mut groups: Vec<Group> = inventory.iter().map(|row| Group::of(row.kind)).collect();
        groups.sort();
        groups.dedup();
        inventory.len() + groups.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kernel::host::Capability;

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

    /// A file that is not a pane, as the inventory reports one.
    fn file(path: &str, kind: Kind) -> Row {
        Row {
            kind,
            slot: String::new(),
            ..row(path, Source::Bundled, FileState::Present)
        }
    }

    /// The `top` example as `plugin install` leaves it: from a package directory
    /// with a long absolute path, asking to run programs, not yet trusted, and in
    /// a slot the shipped layout does not place.
    fn installed_top() -> Row {
        Row {
            slot: "top".into(),
            capabilities: vec![Capability::Run],
            trust: FileTrust::Untrusted,
            ..row(
                "plugins/85_top.lua",
                Source::Installed {
                    src: "/home/user/code/thurbox/examples/panes/top".into(),
                },
                FileState::Unplaced,
            )
        }
    }

    fn press(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, crossterm::event::KeyModifiers::NONE)
    }

    /// The tab as the settings modal draws it — the list with its details, then
    /// the footer on the last line — one string per screen row.
    fn paint(tab: &mut InterfaceTab, rows: &[Row], width: u16, height: u16) -> Vec<String> {
        let palette = crate::session::theme_config::ThemePreset::Default.palette();
        let chrome = Chrome::new(&palette);
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, height))
                .expect("terminal");
        let mut hits = Hits::default();
        terminal
            .draw(|frame| {
                let area = frame.area();
                let body = Rect::new(0, 0, area.width, area.height - 1);
                tab.render(
                    frame,
                    body,
                    Files {
                        rows,
                        dir: "/home/user/.config/thurbox/ui",
                    },
                    chrome,
                    &mut hits,
                );
                frame.render_widget(
                    Paragraph::new(tab.footer(rows, chrome)),
                    Rect::new(0, area.height - 1, area.width, 1),
                );
            })
            .expect("draw");
        let buffer = terminal.backend().buffer();
        (0..height)
            .map(|y| {
                (0..width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect()
    }

    fn screen(tab: &mut InterfaceTab, rows: &[Row]) -> String {
        paint(tab, rows, 90, 24).join("\n")
    }

    /// The screen row that lists `path`.
    fn row_of(lines: &[String], path: &str) -> String {
        lines
            .iter()
            .find(|line| line.contains(path))
            .unwrap_or_else(|| panic!("{path} is not listed:\n{}", lines.join("\n")))
            .clone()
    }

    /// Select the row listing `path` by pressing `j` until it is under the cursor.
    fn select(tab: &mut InterfaceTab, rows: &[Row], path: &str) {
        for _ in 0..rows.len() {
            if tab.current(rows).is_some_and(|row| row.path == path) {
                return;
            }
            tab.on_key(&press(KeyCode::Char('j')), rows, &Hits::default());
        }
        panic!("{path} is never selected");
    }

    // ── what a row says ─────────────────────────────────────────────────────

    #[test]
    fn an_installed_row_names_its_package_and_keeps_its_state_in_view() {
        // The source used to be printed whole. A package installed from a
        // directory has a long absolute path, which pushed the state and the
        // trust words off the end of the row: the one pane that asks to run
        // programs could not show that it was untrusted, nor why it drew nothing.
        let rows = [installed_top()];
        let lines = paint(&mut InterfaceTab::default(), &rows, 72, 12);
        let line = row_of(&lines, "85_top.lua");
        assert!(line.contains("from top"), "names the package: {line}");
        assert!(!line.contains("/home/user"), "not its path: {line}");
        assert!(line.contains("not placed"), "keeps its state: {line}");
        assert!(line.contains("untrusted"), "keeps its trust: {line}");
    }

    #[test]
    fn a_row_names_its_provenance_only_when_it_differs_from_what_ships() {
        let lines = |row: Row| paint(&mut InterfaceTab::default(), &[row], 90, 12);
        let edited = row_of(
            &lines(row(
                "plugins/10_sessions.lua",
                Source::Edited,
                FileState::Visible,
            )),
            "10_sessions.lua",
        );
        assert!(edited.contains("edited"), "{edited}");
        let mine = row_of(
            &lines(row("plugins/90_mine.lua", Source::User, FileState::Visible)),
            "90_mine.lua",
        );
        assert!(mine.contains("yours"), "{mine}");

        // A trusted file that changed since says both halves.
        let mut drifted = installed_top();
        drifted.source = Source::InstalledEdited {
            src: "git+https://github.com/someone/thurbox-top.git".into(),
        };
        drifted.trust = FileTrust::Drifted;
        let line = row_of(&lines(drifted), "85_top.lua");
        assert!(line.contains("from thurbox-top, edited"), "{line}");
        assert!(line.contains("trusted, changed"), "{line}");

        // Silence is the right answer for the shipped, unmodified default: a
        // word on every bundled row would bury the one row that differs.
        let plain = row_of(
            &lines(row(
                "plugins/20_agent.lua",
                Source::Bundled,
                FileState::Visible,
            )),
            "20_agent.lua",
        );
        assert!(!plain.contains("shipped"), "{plain}");
        assert!(!plain.contains("trust"), "{plain}");
    }

    #[test]
    fn a_narrow_row_cuts_the_path_and_keeps_the_state() {
        let rows = [row(
            "plugins/85_a_pane_with_a_name_long_enough_to_fill_a_narrow_row.lua",
            Source::Edited,
            FileState::Disabled,
        )];
        let lines = paint(&mut InterfaceTab::default(), &rows, 56, 12);
        let line = row_of(&lines, "plugins/85_a_pane");
        assert!(line.contains("off"), "{line}");
        assert!(line.contains('…'), "the path is what gives way: {line}");
    }

    #[test]
    fn each_state_reads_as_one_word_that_nothing_else_on_the_row_uses() {
        // `removed` was both a source and a state, `modified` and `edited` meant
        // the same thing, `on/off` was a key while `off` was a state, and
        // `no slot` said nothing about what to do. One word per state, and none
        // of the old overlapping ones.
        let states = [
            FileState::Failed,
            FileState::Removed,
            FileState::Unplaced,
            FileState::Disabled,
            FileState::Visible,
            FileState::OnDemand,
            FileState::Hidden,
        ];
        let mut words = Vec::new();
        for state in states {
            let rows = [row("plugins/50_x.lua", Source::Bundled, state)];
            let lines = paint(&mut InterfaceTab::default(), &rows, 72, 12);
            let line = row_of(&lines, "50_x.lua");
            let tail = line
                .split("50_x.lua")
                .nth(1)
                .expect("a tail")
                .trim()
                .to_string();
            for stale in ["no slot", "removed", "modified", "present"] {
                assert!(!tail.contains(stale), "{state:?} reads `{tail}`");
            }
            assert!(!tail.is_empty(), "{state:?} says nothing");
            assert!(
                !words.contains(&tail),
                "{state:?} reads like another: {tail}"
            );
            words.push(tail);
        }
    }

    #[test]
    fn the_rows_are_grouped_by_what_they_are_with_the_panes_first() {
        let rows = [
            file("AGENTS.md", Kind::Doc),
            file("layout.lua", Kind::Arrangement),
            file("lib/chrome.lua", Kind::Module),
            row(
                "plugins/10_sessions.lua",
                Source::Bundled,
                FileState::Visible,
            ),
            row("plugins/90_notes.lua", Source::User, FileState::Unplaced),
        ];
        let lines = paint(&mut InterfaceTab::default(), &rows, 72, 20);
        let at = |needle: &str| {
            lines
                .iter()
                .position(|line| line.contains(needle))
                .unwrap_or_else(|| panic!("no `{needle}`:\n{}", lines.join("\n")))
        };
        assert!(at("PANES") < at("90_notes.lua"));
        assert!(at("90_notes.lua") < at("10_sessions.lua"), "trouble first");
        assert!(at("10_sessions.lua") < at("LAYOUT"));
        assert!(at("LAYOUT") < at("layout.lua"));
        assert!(at("layout.lua") < at("MODULES"));
        assert!(at("MODULES") < at("lib/chrome.lua"));
        assert!(at("lib/chrome.lua") < at("DOCS"));
        assert!(at("DOCS") < at("AGENTS.md"));
    }

    #[test]
    fn the_selected_row_carries_the_pointer_the_settings_tab_uses() {
        let rows = [
            row(
                "plugins/10_sessions.lua",
                Source::Bundled,
                FileState::Visible,
            ),
            row("plugins/20_agent.lua", Source::Bundled, FileState::Visible),
        ];
        let mut tab = InterfaceTab::default();
        tab.on_key(&press(KeyCode::Char('j')), &rows, &Hits::default());
        let lines = paint(&mut tab, &rows, 72, 12);
        assert!(row_of(&lines, "20_agent.lua").contains('▸'));
        assert!(!row_of(&lines, "10_sessions.lua").contains('▸'));
    }

    #[test]
    fn an_empty_interface_says_so() {
        let text = screen(&mut InterfaceTab::default(), &[]);
        assert!(text.contains("no interface files"), "{text}");
    }

    // ── what the selected row explains ──────────────────────────────────────

    #[test]
    fn the_details_say_why_a_pane_is_not_on_screen_and_what_fixes_it() {
        let top = installed_top();
        let text = screen(&mut InterfaceTab::default(), std::slice::from_ref(&top));
        assert!(
            text.contains("{ slot = \"top\" }"),
            "the fix, spelled: {text}"
        );
        assert!(text.contains("layout.lua"), "and where it goes: {text}");
        assert!(
            text.contains("/home/user/code/thurbox/examples/panes/top"),
            "the full source is in the details, where there is room: {text}"
        );

        let off = [row(
            "plugins/90_notes.lua",
            Source::User,
            FileState::Disabled,
        )];
        let text = screen(&mut InterfaceTab::default(), &off);
        assert!(text.contains("space turns it back on"), "{text}");

        let gone = [row(
            "plugins/20_agent.lua",
            Source::Removed,
            FileState::Removed,
        )];
        let text = screen(&mut InterfaceTab::default(), &gone);
        assert!(text.contains("you deleted"), "{text}");
        assert!(text.contains("r puts it back"), "{text}");

        let mut broken = row("plugins/90_notes.lua", Source::User, FileState::Failed);
        broken.error = Some("90_notes: attempt to index a nil value".into());
        let text = screen(&mut InterfaceTab::default(), &[broken]);
        assert!(text.contains("attempt to index a nil value"), "{text}");
    }

    /// The list where `t` is pressed has to say what it would be granting.
    ///
    /// The two capabilities are two different decisions — one is bounded
    /// output, the other is a process held open on your keystrokes.
    #[test]
    fn the_details_say_what_the_selected_file_asks_for_and_where_it_stands() {
        let mut asks = installed_top();
        let text = screen(&mut InterfaceTab::default(), std::slice::from_ref(&asks));
        assert!(text.contains("reads their output"), "{text}");
        assert!(text.contains("not granted"), "{text}");

        asks.capabilities = vec![Capability::Program];
        asks.trust = FileTrust::Drifted;
        let text = screen(&mut InterfaceTab::default(), std::slice::from_ref(&asks));
        assert!(text.contains("interact"), "{text}");
        assert!(text.contains("changed since"), "{text}");

        let quiet = [row(
            "plugins/10_sessions.lua",
            Source::Bundled,
            FileState::Visible,
        )];
        let text = screen(&mut InterfaceTab::default(), &quiet);
        assert!(!text.contains("asks to"), "{text}");
    }

    // ── which keys are offered ──────────────────────────────────────────────

    /// The footer of a tab whose cursor is on `rows[0]`.
    fn footer(tab: &InterfaceTab, rows: &[Row]) -> String {
        let palette = crate::session::theme_config::ThemePreset::Default.palette();
        tab.footer(rows, Chrome::new(&palette))
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect()
    }

    #[test]
    fn the_footer_offers_only_the_keys_the_selected_row_answers_to() {
        let tab = InterfaceTab::default();
        let shipped = [row(
            "plugins/10_sessions.lua",
            Source::Bundled,
            FileState::Visible,
        )];
        let keys = footer(&tab, &shipped);
        assert!(keys.contains("turn off"), "{keys}");
        assert!(!keys.contains("restore"), "nothing to restore: {keys}");
        assert!(!keys.contains("trust"), "nothing to trust: {keys}");

        let edited = [row(
            "plugins/10_sessions.lua",
            Source::Edited,
            FileState::Disabled,
        )];
        let keys = footer(&tab, &edited);
        assert!(keys.contains("turn on"), "{keys}");
        assert!(keys.contains("restore"), "{keys}");

        let keys = footer(&tab, &[installed_top()]);
        assert!(keys.contains("t") && keys.contains("trust"), "{keys}");
        let mut trusted = installed_top();
        trusted.trust = FileTrust::Trusted;
        let keys = footer(&tab, &[trusted]);
        assert!(keys.contains("revoke"), "{keys}");

        let doc = [file("README.md", Kind::Doc)];
        let keys = footer(&tab, &doc);
        assert!(!keys.contains("turn"), "a doc does not draw: {keys}");
    }

    // ── confirming what cannot be taken back ────────────────────────────────

    #[test]
    fn restoring_an_edited_file_asks_first_and_says_the_edit_is_lost() {
        // A restore writes the shipped copy over the file. For a pane nothing
        // keeps the edit, so a stray `r` used to cost it without a word.
        let rows = [row(
            "plugins/10_sessions.lua",
            Source::Edited,
            FileState::Visible,
        )];
        let mut tab = InterfaceTab::default();
        let (_, edit) = tab.on_key(&press(KeyCode::Char('r')), &rows, &Hits::default());
        assert_eq!(edit, None, "the first press only asks");
        let asked = footer(&tab, &rows);
        assert!(asked.contains("edits are lost"), "{asked}");
        assert!(asked.contains("r again"), "{asked}");

        let (_, edit) = tab.on_key(&press(KeyCode::Char('r')), &rows, &Hits::default());
        assert_eq!(
            edit,
            Some(Edit::File {
                file: "plugins/10_sessions.lua".into(),
                kind: crate::kernel::command::PluginEdit::Restore,
            })
        );
    }

    #[test]
    fn restoring_the_layout_says_where_the_edit_goes() {
        let mut layout = file("layout.lua", Kind::Arrangement);
        layout.source = Source::Edited;
        let rows = [layout];
        let mut tab = InterfaceTab::default();
        tab.on_key(&press(KeyCode::Char('r')), &rows, &Hits::default());
        let asked = footer(&tab, &rows);
        assert!(asked.contains("layout.lua.bak"), "{asked}");
    }

    #[test]
    fn restoring_a_deleted_file_or_moving_away_asks_nothing() {
        let rows = [
            row(
                "plugins/10_sessions.lua",
                Source::Edited,
                FileState::Visible,
            ),
            row("plugins/20_agent.lua", Source::Removed, FileState::Removed),
        ];
        let mut tab = InterfaceTab::default();
        select(&mut tab, &rows, "plugins/10_sessions.lua");
        tab.on_key(&press(KeyCode::Char('r')), &rows, &Hits::default());
        select(&mut tab, &rows, "plugins/20_agent.lua");
        assert!(tab.armed().is_none(), "moving away disarms");
        let (_, edit) = tab.on_key(&press(KeyCode::Char('r')), &rows, &Hits::default());
        assert!(edit.is_some(), "putting back a deleted file loses nothing");
    }

    #[test]
    fn the_cursor_stays_on_its_file_when_an_action_reorders_the_list() {
        // Turning a pane off moves it within the list (trouble sorts first), so
        // a cursor kept as an index was left on whichever file slid into its
        // place — and the next key acted on that one.
        let before = [
            row(
                "plugins/10_sessions.lua",
                Source::Bundled,
                FileState::Visible,
            ),
            row("plugins/90_notes.lua", Source::User, FileState::Visible),
        ];
        let mut tab = InterfaceTab::default();
        select(&mut tab, &before, "plugins/90_notes.lua");
        tab.on_key(&press(KeyCode::Char(' ')), &before, &Hits::default());

        let after = [
            row(
                "plugins/10_sessions.lua",
                Source::Bundled,
                FileState::Visible,
            ),
            row("plugins/90_notes.lua", Source::User, FileState::Disabled),
        ];
        let lines = paint(&mut tab, &after, 72, 14);
        assert!(row_of(&lines, "90_notes.lua").contains('▸'), "{lines:#?}");
        let (_, edit) = tab.on_key(&press(KeyCode::Char(' ')), &after, &Hits::default());
        assert_eq!(
            edit,
            Some(Edit::Switch {
                file: "plugins/90_notes.lua".into(),
                off: false
            })
        );
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
        let ours = [row("plugins/90_mine.lua", Source::User, FileState::Visible)];
        let mut tab = InterfaceTab::default();
        tab.on_key(&press(KeyCode::Char('d')), &ours, &Hits::default());
        let mine = footer(&tab, &ours);
        assert!(mine.contains("cannot be undone"), "{mine}");

        let shipped = [row(
            "plugins/10_sessions.lua",
            Source::Bundled,
            FileState::Visible,
        )];
        let mut tab = InterfaceTab::default();
        tab.on_key(&press(KeyCode::Char('d')), &shipped, &Hits::default());
        let theirs = footer(&tab, &shipped);
        assert!(theirs.contains("restorable"), "{theirs}");
        assert_ne!(mine, theirs, "the two must not be worded alike");
    }
}
