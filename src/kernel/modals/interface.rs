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
/// By where a plugin came from, because that is what decides what can be done
/// with it: yours are edited by hand, a package's are put back and advanced by
/// `thurbox-cli plugin`, the shipped ones are restored from the binary. Yours
/// and the packages first — they are what an operator manages; the shipped
/// panes follow, and everything that is not a pane last.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum Group {
    Yours,
    /// An installed package: its name, then its source, so two packages whose
    /// sources end in the same name are still two groups.
    Package(String, String),
    Shipped,
    /// Modules, docs, `layout.lua` and `plugins.toml`: what the panes are built
    /// from and arranged by. Listed only on `a`, or when one needs attention.
    Support,
}

impl Group {
    fn of(row: &Row) -> Self {
        if row.kind != Kind::Pane {
            return Group::Support;
        }
        match &row.source {
            Source::User => Group::Yours,
            Source::Installed { src } | Source::InstalledEdited { src } => {
                Group::Package(package(src).to_string(), src.clone())
            }
            Source::Bundled | Source::Edited | Source::Removed => Group::Shipped,
        }
    }

    fn title(&self) -> String {
        match self {
            Group::Yours => "YOURS".to_string(),
            Group::Package(name, _) => format!("PACKAGE · {name}"),
            Group::Shipped => "SHIPPED".to_string(),
            Group::Support => "SUPPORT FILES".to_string(),
        }
    }
}

/// Whether a support file is listed while support files are folded away: it
/// is the one a recovery needs — broken, deleted, or edited and restorable.
fn needs_attention(row: &Row) -> bool {
    row.error.is_some()
        || matches!(row.state, FileState::Failed | FileState::Removed)
        || matches!(row.source, Source::Edited | Source::InstalledEdited { .. })
}

/// The file whose load error stopped the last reload, when one is named.
///
/// A reload is all-or-nothing, so every other pane that is not loaded is held
/// back by this one rather than broken itself.
fn culprit(inventory: &[Row]) -> Option<&Row> {
    inventory.iter().find(|row| row.error.is_some())
}

/// Whether `row` is a pane left unloaded by another file's failure.
fn held_back(row: &Row, culprit: Option<&Row>) -> bool {
    row.state == FileState::Failed
        && row.error.is_none()
        && culprit.is_some_and(|culprit| culprit.path != row.path)
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

fn state_of(row: &Row, culprit: Option<&Row>) -> Reading {
    let (rank, glyph, word, fault) = match row.state {
        // Not its own fault: drawn plainly, so the one red row is the file to fix.
        FileState::Failed if held_back(row, culprit) => (2, "◇", "not loaded", false),
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

/// What a row calls its file. The group header already says where it came
/// from, so a plugin is listed by the part of its path that tells it from its
/// neighbours; a support file keeps its whole path, which is what tells
/// `lib/ui.lua` from `ui.lua`. The full path is in the details.
fn label(row: &Row) -> &str {
    match Group::of(row) {
        Group::Package(..) => row.path.rsplit('/').next().unwrap_or(&row.path),
        Group::Yours | Group::Shipped => row.path.strip_prefix("plugins/").unwrap_or(&row.path),
        Group::Support => &row.path,
    }
}

/// Where a file came from, as the row says it — only where the group header
/// does not already: an edit, made here to a shipped or installed file.
fn origin(source: &Source) -> Option<&'static str> {
    match source {
        Source::Edited | Source::InstalledEdited { .. } => Some("edited"),
        Source::User | Source::Installed { .. } | Source::Bundled | Source::Removed => None,
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

/// What a file asks for and whether it holds it, as the row says it.
///
/// Read from [`FileTrust::grants`], the rule the enforced grant goes through, so
/// the row cannot say "not granted" while the plugin holds `run` (#1467). Silent
/// for a shipped file, which holds what it asks for without being asked.
fn grant_word(row: &Row) -> Option<(String, bool)> {
    if row.capabilities.is_empty() || matches!(row.trust, FileTrust::NotAsked | FileTrust::Shipped)
    {
        return None;
    }
    let caps = capability_names(row);
    let changed = matches!(row.trust, FileTrust::Drifted | FileTrust::Replaced);
    let granted = row.trust.grants();
    let word = match (granted, changed) {
        (true, false) => format!("{caps} granted"),
        (true, true) => format!("{caps} granted, changed"),
        (false, false) => format!("{caps} not granted"),
        (false, true) => format!("{caps} not granted, changed"),
    };
    Some((word, granted))
}

fn capability_names(row: &Row) -> String {
    row.capabilities
        .iter()
        .map(|capability| capability.as_str())
        .collect::<Vec<_>>()
        .join("+")
}

/// Why the selected file is in the state it is, and — when that is probably
/// not what the operator wants — the one thing that changes it, as its own line.
fn reason(row: &Row, culprit: Option<&Row>) -> (String, Option<String>) {
    let why = |text: &str| text.to_string();
    match row.state {
        FileState::Failed if held_back(row, culprit) => {
            let culprit = culprit.map_or("another file", |culprit| culprit.path.as_str());
            (
                format!("not loaded: {culprit} failed to load, and a reload is all or nothing"),
                Some(format!(
                    "select {culprit} for its error; fixed, F10 reloads both"
                )),
            )
        }
        FileState::Failed => (
            row.error.clone().unwrap_or_else(|| why("did not load")),
            Some(why(
                "fix the file and press F10 to reload, or space turns it off",
            )),
        ),
        FileState::Removed => (
            why("you deleted this shipped file"),
            Some(why("r puts it back")),
        ),
        // Hedged, because the inventory cannot tell the two apart: the shipped
        // layout places the search strip's slot only while it is open, so a
        // default install reports that pane unplaced while it works as shipped.
        FileState::Unplaced => (
            format!(
                "layout.lua places no slot \"{}\" at this size, or only while it is open",
                row.slot
            ),
            Some(format!(
                "to always show it, add {{ slot = \"{}\" }} to layout.lua",
                row.slot
            )),
        ),
        FileState::Disabled => (
            why("turned off here; the file is untouched"),
            Some(why("space turns it back on")),
        ),
        FileState::Visible => (why("drawn on screen now"), None),
        FileState::OnDemand => (why("draws when something opens it"), None),
        FileState::Hidden => (
            format!(
                "slot \"{}\" is placed, but another pane holds it or its column is closed",
                row.slot
            ),
            None,
        ),
        FileState::Present => (
            why(match row.kind {
                Kind::Pane => "draws into another pane rather than a slot of its own",
                Kind::Module => "loaded when a pane requires it",
                Kind::Arrangement => "decides where every pane goes",
                Kind::Manifest => "what `thurbox-cli plugin sync` installs",
                Kind::Doc => "guidance for whoever edits this directory",
            }),
            None,
        ),
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
        FileTrust::Replaced => " — not granted: changed since trusted; t grants it",
        FileTrust::Shipped => " — granted: shipped with thurbox, unchanged",
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

/// The interface directory as the header shows it: under the home directory,
/// as `~/…`, so the line spends its width on the part that differs.
fn tilde(dir: &str) -> String {
    match std::env::var("HOME") {
        Ok(home) if !home.is_empty() && dir.starts_with(&home) => {
            format!("~{}", &dir[home.len()..])
        }
        _ => dir.to_string(),
    }
}

/// `text` cut to its last `width` characters, the cut marked at the front: a
/// directory is told apart by its end.
fn truncate_front(text: &str, width: usize) -> String {
    let count = text.chars().count();
    if count <= width {
        return text.to_string();
    }
    if width == 0 {
        return String::new();
    }
    let tail: String = text.chars().skip(count - (width - 1)).collect();
    format!("…{tail}")
}

/// `text` broken at spaces into lines of at most `width` characters, a word
/// longer than that cut. At most `max` lines; the last one marks a cut.
fn wrap(text: &str, width: usize, max: usize) -> Vec<String> {
    let width = width.max(1);
    let mut lines: Vec<String> = Vec::new();
    let mut line = String::new();
    for word in text.split(' ') {
        let len = line.chars().count();
        if len > 0 && len + 1 + word.chars().count() > width {
            lines.push(std::mem::take(&mut line));
        }
        if !line.is_empty() {
            line.push(' ');
        }
        line.push_str(word);
    }
    lines.push(line);
    if lines.len() > max {
        let rest = lines.split_off(max.saturating_sub(1)).join(" ");
        lines.push(rest);
    }
    lines
        .into_iter()
        .map(|line| chrome::truncate(&line, width))
        .collect()
}

/// The least of a name a narrow row keeps before it starts dropping the words
/// after it: enough to tell `85_top.lua` from its neighbours.
const PATH_FLOOR: usize = 20;

/// Lines the selected row's details take under the list.
const DETAILS: u16 = 4;

/// What a second press of the same key will do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Armed {
    /// `d`: delete the file. `ours` says whether it is the user's own, so the
    /// question can say whether this is undoable *before* it happens.
    Remove { ours: bool },
    /// `r` on an edited file: the shipped copy replaces the edit.
    Restore { layout: bool },
    /// `t` on a file that does not hold what it asks for: granting hands a
    /// plugin programs or sessions, which is the one act here that gives
    /// something away rather than costing a file.
    Grant,
}

/// The Interface tab's own state: a cursor, an armed confirmation, and whether
/// the support files are listed.
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
    /// `a`: list every file, not just the plugins and the files needing
    /// attention.
    all: bool,
}

impl InterfaceTab {
    /// The rows listed, ordered: by group, trouble first within one, then by
    /// path.
    ///
    /// Sorted per call rather than cached, because the whole point of the view is
    /// that it is current — a reload changes every one of these answers.
    fn ordered<'a>(&self, inventory: &'a [Row]) -> Vec<&'a Row> {
        let culprit = culprit(inventory);
        let mut rows: Vec<&Row> = inventory
            .iter()
            .filter(|row| self.all || row.kind == Kind::Pane || needs_attention(row))
            .collect();
        rows.sort_by(|a, b| {
            Group::of(a)
                .cmp(&Group::of(b))
                .then_with(|| state_of(a, culprit).rank.cmp(&state_of(b, culprit).rank))
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
        let rows = self.ordered(inventory);
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
        let ordered = self.ordered(inventory);
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
            KeyCode::Char('a') => {
                // The cursor stays on its file — `following` — when that file
                // is still listed; otherwise `follow` clamps it.
                self.all = !self.all;
                self.armed = None;
                return (None, None);
            }
            KeyCode::Char('r') => return self.restore(inventory),
            KeyCode::Char('d') => return self.remove(inventory),
            KeyCode::Char('t') => return self.trust(inventory),
            KeyCode::Char(' ') => return self.switch(inventory),
            // The armed question says any other key cancels it.
            _ => self.armed = None,
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
    ///
    /// Granting asks first, on the footer, naming what is handed over; revoking
    /// does not, because taking a capability away risks nothing.
    fn trust(&mut self, inventory: &[Row]) -> (Option<String>, Option<Edit>) {
        let Some(row) = self.current(inventory) else {
            self.armed = None;
            return (None, None);
        };
        if row.trust == FileTrust::NotAsked {
            self.armed = None;
            return (
                Some(format!("{} asks for nothing to trust it with", row.path)),
                None,
            );
        }
        if row.trust == FileTrust::Shipped {
            self.armed = None;
            return (
                Some(format!(
                    "{} is shipped with thurbox and holds what it asks for; space turns it off",
                    row.path
                )),
                None,
            );
        }
        let granting = !row.trust.grants();
        let path = row.path.clone();
        if granting && !self.is_armed(&path, |what| what == Armed::Grant) {
            self.armed = Some((path, Armed::Grant));
            return (None, None);
        }
        self.armed = None;
        (
            None,
            Some(Edit::Trust {
                file: path,
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

    /// One row: pointer, state glyph, name, and the words — its state, what it
    /// asks for and whether it holds it, and an edit.
    ///
    /// The words are what the row is for, so at a narrow width the name gives
    /// way first; past its floor the words are dropped whole from the end,
    /// never the state.
    fn line<'a>(
        row: &Row,
        culprit: Option<&Row>,
        width: usize,
        chrome: Chrome<'a>,
        selected: bool,
    ) -> Line<'a> {
        let state = state_of(row, culprit);
        let (pointer, body) = if selected {
            ("▸ ", chrome.selected_item())
        } else {
            ("  ", chrome.normal_item())
        };
        let fault = Style::default().fg(chrome.palette.danger);
        let glyph = if state.fault { fault } else { Style::default() };
        let quiet = if state.fault { fault } else { chrome.muted() };

        let mut words: Vec<(String, Style)> = vec![(state.word.to_string(), quiet)];
        if let Some((word, granted)) = grant_word(row) {
            let role = if granted {
                chrome.palette.tool_allowed
            } else {
                chrome.palette.tool_disallowed
            };
            words.push((word, Style::default().fg(role)));
        }
        if let Some(edited) = origin(&row.source) {
            words.push((edited.to_string(), quiet));
        }

        // 2 pointer + glyph + 1 space, then the name, a gap of at least 2, the
        // words. The name keeps at least its floor; the words that do not fit
        // beside that are dropped from the end, the state cut only as a last
        // resort.
        let name = label(row);
        let lead = 2 + state.glyph.chars().count() + 1;
        let floor = name.chars().count().min(PATH_FLOOR);
        let room = width.saturating_sub(lead + 2 + floor);
        let mut tail: Vec<Span<'a>> = Vec::new();
        let mut used = 0;
        for (nth, (word, style)) in words.into_iter().enumerate() {
            let sep = if nth == 0 { 0 } else { 3 };
            let len = word.chars().count();
            if nth == 0 {
                let word = chrome::truncate(&word, room);
                used = word.chars().count();
                tail.push(Span::styled(word, style));
            } else if used + sep + len <= room {
                tail.push(Span::styled(" · ", quiet));
                tail.push(Span::styled(word, style));
                used += sep + len;
            } else {
                break;
            }
        }
        let name = chrome::truncate(name, width.saturating_sub(lead + 2 + used).max(1));
        let gap = width
            .saturating_sub(lead + name.chars().count() + used)
            .max(1);
        let mut spans = vec![
            Span::styled(pointer, body),
            Span::styled(format!("{} ", state.glyph), glyph),
            Span::styled(name, body),
            Span::raw(" ".repeat(gap)),
        ];
        spans.extend(tail);
        Line::from(spans)
    }

    /// The selected row, explained: what it is, where it came from and its
    /// path, why it is in the state it is, the fix, and what it asks for.
    ///
    /// Always [`DETAILS`] lines, so the modal does not change height as the
    /// cursor moves. A load error is the one line that wraps, into the room the
    /// lines under it leave, because it is the line a broken file is fixed from.
    fn details<'a>(
        row: Option<&Row>,
        culprit: Option<&Row>,
        width: usize,
        chrome: Chrome<'a>,
    ) -> Vec<Line<'a>> {
        let Some(row) = row else {
            return vec![Line::default(); DETAILS as usize];
        };
        let state = state_of(row, culprit);
        let reason_style = if state.fault {
            Style::default().fg(chrome.palette.danger)
        } else {
            chrome.normal_item()
        };
        let (why, fix) = reason(row, culprit);
        let asks = asks(row);
        // A load error may take the fix's line too: the error is what a broken
        // file is fixed from, and the fix only says to fix it.
        let spare = DETAILS as usize - 1 - usize::from(asks.is_some());
        let text = width.saturating_sub(1);
        let mut lines = vec![Line::from(Span::styled(
            format!(
                " {} · {} · {}",
                what(row),
                provenance(&row.source),
                row.path
            ),
            chrome.muted(),
        ))];
        let why_lines = if row.error.is_some() {
            wrap(&why, text, spare)
        } else {
            vec![why]
        };
        lines.extend(
            why_lines
                .into_iter()
                .map(|why| Line::from(Span::styled(format!(" {why}"), reason_style))),
        );
        let fix = fix.filter(|_| lines.len() < 1 + spare);
        for text in [fix, asks].into_iter().flatten() {
            lines.push(Line::from(Span::styled(
                format!(" {text}"),
                chrome.normal_item(),
            )));
        }
        lines.resize(DETAILS as usize, Line::default());
        lines
    }

    /// The top line: what needs attention, counted, and where the files are.
    ///
    /// The counts are the answer to "is anything wrong" before any row is read;
    /// the directory to "why did my edit do nothing", since `THURBOX_UI_DIR` and
    /// a dev build each move the live one.
    fn summary<'a>(inventory: &[Row], dir: &str, width: usize, chrome: Chrome<'a>) -> Line<'a> {
        let culprit = culprit(inventory);
        let panes = inventory
            .iter()
            .filter(|row| row.kind == Kind::Pane && row.state != FileState::Removed)
            .count();
        let count = |pick: &dyn Fn(&Row) -> bool| inventory.iter().filter(|row| pick(row)).count();
        // Failed, not merely drawn as a fault: a deleted shipped file is red too,
        // and counting it would send the reader looking for an error.
        let failed = count(&|row| row.state == FileState::Failed && state_of(row, culprit).fault);
        let ungranted = count(&|row| grant_word(row).is_some_and(|(_, granted)| !granted));
        let off = count(&|row| row.state == FileState::Disabled);

        let noun = if panes == 1 { "plugin" } else { "plugins" };
        let mut spans = vec![Span::styled(
            format!(" {panes} {noun}"),
            chrome.normal_item(),
        )];
        let mut used = spans[0].content.chars().count();
        for (n, word, style) in [
            (failed, "failed", Style::default().fg(chrome.palette.danger)),
            (
                ungranted,
                "not granted",
                Style::default().fg(chrome.palette.tool_disallowed),
            ),
            (off, "off", chrome.muted()),
        ] {
            if n > 0 {
                let text = format!("{n} {word}");
                used += 3 + text.chars().count();
                spans.push(Span::styled(" · ", chrome.muted()));
                spans.push(Span::styled(text, style));
            }
        }
        let dir = tilde(dir);
        let room = width.saturating_sub(used + 3);
        if room >= 8 {
            let dir = truncate_front(&dir, room - 1);
            let gap = width.saturating_sub(used + dir.chars().count() + 1);
            spans.push(Span::raw(" ".repeat(gap.max(2))));
            spans.push(Span::styled(dir, chrome.muted()));
        }
        Line::from(spans)
    }

    /// The bottom line: the armed question, or the keys the selected row
    /// answers to — only those, so a key on screen is a key that does something.
    ///
    /// Within `width`: on a narrow screen the hints are dropped from the end,
    /// where the ones that act on no file are, rather than run into the pills.
    pub fn footer<'a>(&self, inventory: &[Row], width: usize, chrome: Chrome<'a>) -> Line<'a> {
        if let Some((path, armed)) = &self.armed {
            // The two removals are different acts and must not be worded alike:
            // one can be undone from the binary, the other cannot be undone at
            // all. A reflex learned on the first must not carry to the second.
            let name = path.rsplit('/').next().unwrap_or(path);
            let (question, consequence) = match armed {
                Armed::Remove { ours: true } => (
                    format!("delete {path}?"),
                    " thurbox has no copy — this cannot be undone. d again to confirm".to_string(),
                ),
                Armed::Remove { ours: false } => (
                    format!("delete {path}?"),
                    " restorable afterwards. d again to confirm".to_string(),
                ),
                Armed::Restore { layout: true } => (
                    format!("restore {path}?"),
                    " your layout.lua is moved to layout.lua.bak. r again to confirm".to_string(),
                ),
                Armed::Restore { layout: false } => (
                    format!("restore {path}?"),
                    " your edits are lost — nothing keeps a copy. r again to confirm".to_string(),
                ),
                Armed::Grant => {
                    let caps = inventory
                        .iter()
                        .find(|row| row.path == *path)
                        .map(capability_names)
                        .unwrap_or_default();
                    (
                        format!("grant {name}: {caps}?"),
                        " t again to grant · any other key cancels".to_string(),
                    )
                }
            };
            return Line::from(vec![
                Span::styled(
                    format!(" {question} "),
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
            match row.trust {
                FileTrust::Untrusted | FileTrust::Replaced => hints.push(("t", " grant  ")),
                FileTrust::Trusted | FileTrust::Drifted => hints.push(("t", " revoke  ")),
                FileTrust::NotAsked | FileTrust::Shipped => {}
            }
            if restorable(row) {
                hints.push(("r", " restore  "));
            }
            if row.state != FileState::Removed {
                hints.push(("d", " delete  "));
            }
        }
        hints.push(if self.all {
            ("a", " plugins only  ")
        } else {
            ("a", " all files  ")
        });
        hints.push(("[/]", " tab"));
        let len = |hints: &[(&str, &str)]| -> usize {
            hints
                .iter()
                .map(|(key, desc)| key.chars().count() + desc.chars().count())
                .sum()
        };
        while hints.len() > 1 && len(&hints) > width {
            hints.pop();
        }
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
        frame.render_widget(
            Paragraph::new(Self::summary(
                inventory,
                ui_dir,
                area.width as usize,
                chrome,
            )),
            Rect::new(area.x, area.y, area.width, 1),
        );

        let rows = self.ordered(inventory);
        if rows.is_empty() {
            if area.height > 1 {
                frame.render_widget(
                    Paragraph::new(vec![
                        Line::from(Span::styled(
                            " no plugins here yet — thurbox writes the shipped ones when it starts",
                            chrome.muted(),
                        )),
                        Line::from(Span::styled(
                            " add one: a .lua file in plugins/, or `thurbox-cli plugin install <source>`",
                            chrome.muted(),
                        )),
                    ]),
                    Rect::new(area.x, area.y + 1, area.width, area.height - 1),
                );
            }
            return;
        }
        self.follow(&rows);
        let culprit = culprit(inventory);

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
                Paragraph::new(Self::details(
                    rows.get(self.selected).copied(),
                    culprit,
                    area.width as usize,
                    chrome,
                )),
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
            let this = Group::of(row);
            if group.as_ref() != Some(&this) {
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
                    let drawn = Self::line(
                        row,
                        culprit,
                        body.width as usize,
                        chrome,
                        *index == self.selected,
                    );
                    frame.render_widget(Paragraph::new(drawn), rect);
                    hits.rows.push((rect, *index));
                }
                None => {
                    // The header of the group the next row belongs to.
                    let next = lines[first + nth + 1..]
                        .iter()
                        .flatten()
                        .next()
                        .map(|index| Group::of(rows[*index]));
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

    /// Lines the list needs: a row per listed file and a header per group.
    pub fn listed(&self, inventory: &[Row]) -> usize {
        let rows = self.ordered(inventory);
        let mut groups: Vec<Group> = rows.iter().map(|row| Group::of(row)).collect();
        groups.dedup();
        rows.len() + groups.len()
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
                    Paragraph::new(tab.footer(rows, width as usize, chrome)),
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
        assert!(
            lines.iter().any(|line| line.contains("PACKAGE · top")),
            "names the package: {lines:#?}"
        );
        let line = row_of(&lines, "85_top.lua");
        assert!(!line.contains("/home/user"), "not its path: {line}");
        assert!(line.contains("not placed"), "keeps its state: {line}");
        assert!(line.contains("run not granted"), "keeps its grant: {line}");
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
        // Your own file says so once, in its group's header, not on each row.
        let mine = lines(row("plugins/90_mine.lua", Source::User, FileState::Visible));
        assert!(mine.iter().any(|line| line.trim() == "YOURS"), "{mine:#?}");
        assert!(!row_of(&mine, "90_mine.lua").contains("yours"));

        // A trusted file that changed since says both halves.
        let mut drifted = installed_top();
        drifted.source = Source::InstalledEdited {
            src: "git+https://github.com/someone/thurbox-top.git".into(),
        };
        drifted.trust = FileTrust::Drifted;
        let drawn = lines(drifted);
        assert!(
            drawn
                .iter()
                .any(|line| line.contains("PACKAGE · thurbox-top")),
            "{drawn:#?}"
        );
        let line = row_of(&drawn, "85_top.lua");
        assert!(line.contains("edited"), "{line}");
        assert!(line.contains("run granted, changed"), "{line}");

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
        let line = row_of(&lines, "85_a_pane");
        assert!(line.contains("off"), "{line}");
        assert!(line.contains('…'), "the name is what gives way: {line}");
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
    fn the_plugins_are_grouped_by_where_they_came_from_and_support_files_last() {
        let mut top = installed_top();
        top.state = FileState::Visible;
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
            row("plugins/91_ok.lua", Source::User, FileState::Visible),
            top,
        ];
        let mut tab = InterfaceTab {
            all: true,
            ..InterfaceTab::default()
        };
        let lines = paint(&mut tab, &rows, 72, 24);
        let at = |needle: &str| {
            lines
                .iter()
                .position(|line| line.contains(needle))
                .unwrap_or_else(|| panic!("no `{needle}`:\n{}", lines.join("\n")))
        };
        assert!(at("YOURS") < at("90_notes.lua"));
        assert!(at("90_notes.lua") < at("91_ok.lua"), "trouble first");
        assert!(at("91_ok.lua") < at("PACKAGE · top"));
        assert!(at("PACKAGE · top") < at("85_top.lua"));
        assert!(at("85_top.lua") < at("SHIPPED"));
        assert!(at("SHIPPED") < at("10_sessions.lua"));
        assert!(at("10_sessions.lua") < at("SUPPORT FILES"));
        assert!(at("SUPPORT FILES") < at("AGENTS.md"));
        assert!(at("AGENTS.md") < at("lib/chrome.lua"));
    }

    #[test]
    fn support_files_are_listed_on_a_or_when_one_needs_attention() {
        let mut layout = file("layout.lua", Kind::Arrangement);
        layout.source = Source::Edited;
        let rows = [
            file("lib/chrome.lua", Kind::Module),
            layout,
            row("plugins/90_mine.lua", Source::User, FileState::Visible),
        ];
        let mut tab = InterfaceTab::default();
        let text = screen(&mut tab, &rows);
        assert!(text.contains("layout.lua"), "edited, so restorable: {text}");
        assert!(!text.contains("lib/chrome.lua"), "{text}");
        assert_eq!(tab.listed(&rows), 4, "two rows under two headers");

        tab.on_key(&press(KeyCode::Char('a')), &rows, &Hits::default());
        let text = screen(&mut tab, &rows);
        assert!(text.contains("lib/chrome.lua"), "{text}");
        assert!(footer(&tab, &rows).contains("plugins only"));
    }

    #[test]
    fn panes_held_back_by_a_failed_reload_are_not_called_failed() {
        // A reload is all-or-nothing: one broken file leaves every other one
        // unloaded. Only the file the error names is the fault.
        let mut broken = row("plugins/93_broken.lua", Source::User, FileState::Failed);
        broken.error = Some("93_broken: syntax error near <eof>".into());
        let rows = [
            row("plugins/92_fine.lua", Source::User, FileState::Failed),
            broken,
        ];
        let mut tab = InterfaceTab::default();
        let lines = paint(&mut tab, &rows, 90, 14);
        let culprit = row_of(&lines, "93_broken.lua");
        assert!(
            culprit.contains("failed") && culprit.contains('▸'),
            "{culprit}"
        );
        let held = row_of(&lines, "92_fine.lua");
        assert!(
            held.contains("not loaded") && !held.contains("failed"),
            "{held}"
        );
        assert!(lines[0].contains("1 failed"), "{}", lines[0]);
        assert!(lines
            .iter()
            .any(|line| line.contains("syntax error near <eof>")));

        select(&mut tab, &rows, "plugins/92_fine.lua");
        let text = paint(&mut tab, &rows, 90, 14).join("\n");
        assert!(
            text.contains("plugins/93_broken.lua failed to load"),
            "{text}"
        );

        // With nothing named, a pane that did not load is still its own fault.
        let alone = [row("plugins/92_fine.lua", Source::User, FileState::Failed)];
        let line = row_of(
            &paint(&mut InterfaceTab::default(), &alone, 90, 12),
            "92_fine",
        );
        assert!(line.contains("failed"), "{line}");
    }

    #[test]
    fn a_long_load_error_wraps_into_the_room_under_it() {
        let mut broken = row("plugins/93_broken.lua", Source::User, FileState::Failed);
        broken.error = Some(format!("93_broken: {}tail-of-error", "word ".repeat(20)));
        let text = paint(&mut InterfaceTab::default(), &[broken], 60, 12).join("\n");
        assert!(text.contains("tail-of-error"), "{text}");
    }

    #[test]
    fn granting_asks_first_and_names_what_is_granted_while_revoking_does_not() {
        let rows = [installed_top()];
        let mut tab = InterfaceTab::default();
        let (_, edit) = tab.on_key(&press(KeyCode::Char('t')), &rows, &Hits::default());
        assert_eq!(edit, None, "the first press only asks");
        let asked = footer(&tab, &rows);
        assert!(asked.contains("grant 85_top.lua: run?"), "{asked}");
        assert!(asked.contains("t again to grant"), "{asked}");

        // Any other key withdraws the question.
        tab.on_key(&press(KeyCode::Char('x')), &rows, &Hits::default());
        assert!(tab.armed().is_none());
        tab.on_key(&press(KeyCode::Char('t')), &rows, &Hits::default());
        let (_, edit) = tab.on_key(&press(KeyCode::Char('t')), &rows, &Hits::default());
        assert_eq!(
            edit,
            Some(Edit::Trust {
                file: "plugins/85_top.lua".into(),
                trusted: true,
            })
        );

        let mut trusted = installed_top();
        trusted.trust = FileTrust::Trusted;
        let (_, edit) = InterfaceTab::default().on_key(
            &press(KeyCode::Char('t')),
            &[trusted],
            &Hits::default(),
        );
        assert_eq!(
            edit,
            Some(Edit::Trust {
                file: "plugins/85_top.lua".into(),
                trusted: false,
            }),
            "revoking risks nothing, so it does not ask"
        );
    }

    #[test]
    fn the_summary_counts_what_needs_attention_and_shortens_the_directory() {
        let mut off = row("plugins/90_mine.lua", Source::User, FileState::Disabled);
        off.capabilities = vec![Capability::Run];
        off.trust = FileTrust::Untrusted;
        let rows = [off, installed_top()];
        let lines = paint(&mut InterfaceTab::default(), &rows, 90, 12);
        assert!(lines[0].contains("2 plugins"), "{}", lines[0]);
        assert!(lines[0].contains("2 not granted"), "{}", lines[0]);
        assert!(lines[0].contains("1 off"), "{}", lines[0]);
        assert!(!lines[0].contains("failed"), "{}", lines[0]);
        assert_eq!(truncate_front("/a/long/dir/ui", 8), "…/dir/ui");

        // A shipped pane you deleted is drawn as a fault, but it did not fail:
        // counting it would send the reader looking for an error that is not there.
        let gone = [row(
            "plugins/20_agent.lua",
            Source::Removed,
            FileState::Removed,
        )];
        let lines = paint(&mut InterfaceTab::default(), &gone, 90, 12);
        assert!(!lines[0].contains("failed"), "{}", lines[0]);
    }

    #[test]
    fn two_packages_sharing_a_name_stay_in_groups_of_their_own() {
        let mut ours = installed_top();
        ours.path = "plugins/85_top.lua".into();
        let mut theirs = installed_top();
        theirs.path = "other/plugins/86_top.lua".into();
        theirs.source = Source::Installed {
            src: "git+https://github.com/someone/top".into(),
        };
        let lines = paint(&mut InterfaceTab::default(), &[ours, theirs], 90, 14);
        let headers = lines
            .iter()
            .filter(|line| line.contains("PACKAGE · top"))
            .count();
        assert_eq!(headers, 2, "{lines:#?}");
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
        assert!(text.contains("no plugins here yet"), "{text}");
        assert!(text.contains("thurbox-cli plugin install"), "{text}");
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
        // The shipped search strip is placed only while it is open, so a
        // default install reports it unplaced. The reason must not claim the
        // layout never places it, or the fix reads as "edit your layout" for a
        // pane that is working as shipped.
        assert!(
            text.contains("or only while it is open"),
            "the reason allows for a pane placed on demand: {text}"
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
        tab.footer(rows, 200, Chrome::new(&palette))
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
        assert!(keys.contains("t") && keys.contains("grant"), "{keys}");
        let mut trusted = installed_top();
        trusted.trust = FileTrust::Trusted;
        let keys = footer(&tab, &[trusted]);
        assert!(keys.contains("revoke"), "{keys}");

        // Changed under its pin: it holds nothing, so `t` grants rather than revokes.
        let mut replaced = installed_top();
        replaced.trust = FileTrust::Replaced;
        let keys = footer(&tab, std::slice::from_ref(&replaced));
        assert!(keys.contains("grant") && !keys.contains("revoke"), "{keys}");
        let mut tab = InterfaceTab::default();
        assert_eq!(
            tab.trust(std::slice::from_ref(&replaced)).1,
            None,
            "asks first"
        );
        assert_eq!(
            tab.trust(&[replaced]).1,
            Some(Edit::Trust {
                file: installed_top().path,
                trusted: true,
            })
        );

        // The binary's own copy holds its grant with no record to revoke, so `t`
        // explains rather than writing a decision that would change nothing.
        let mut shipped = row(
            "plugins/60_confirm.lua",
            Source::Bundled,
            FileState::Present,
        );
        shipped.capabilities = vec![Capability::Sessions];
        shipped.trust = FileTrust::Shipped;
        let keys = footer(&tab, std::slice::from_ref(&shipped));
        assert!(
            !keys.contains("grant") && !keys.contains("revoke"),
            "{keys}"
        );
        let (said, edit) = InterfaceTab::default().trust(&[shipped]);
        assert_eq!(edit, None);
        assert!(said.is_some_and(|said| said.contains("space turns it off")));

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
