//! The settings modal: every setting any plugin declared.
//!
//! Like help, it renders the registry rather than a list it maintains, grouped
//! by the plugin that declared each row — so a plugin gets a settings row by
//! declaring one table field and never learns how the row is drawn (design.md
//! D1).
//!
//! Values stay a knob rather than a document (design.md D5): `Bool`, `Number`,
//! `Text`, and nothing else. A plugin wanting structured configuration reads a
//! file through its own capability.
//!
//! Unlike v1 — whose panel edits a draft and applies on `Ctrl+S`, because its
//! knobs write `settings.toml` and some need a restart — a write here goes
//! straight through `Registry::set_setting`, which is in-process and takes
//! effect on the next frame. So there is nothing to save and no Cancel.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::Rect;
use ratatui::style::Modifier;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use super::chrome::{self, Chrome, Hits, Pill, Replay};
use super::interface::{Edit, Files, InterfaceTab};
use crate::kernel::inventory::Row as FileRow;
use crate::kernel::registry::{Registry, Setting, Value};
use crate::session::settings::{NotificationBackend, Settings};

/// v1's `settings_modal::MODAL_WIDTH` pair: the wide layout once a terminal has
/// room for the longest description, the compact default otherwise.
/// Marks a row that will not take effect until thurbox restarts.
///
/// ASCII on purpose. This was `⟳`, which rendered as a replacement box: no
/// monospace family installed here covers U+27F3, and none covers the arrows
/// (U+21BB, U+2B06) either — `fc-list ':charset=21BB'` finds none against
/// fourteen for Geometric Shapes. A marker nobody can see is worse than a plain
/// one, and the footer carries its legend.
const RESTART_MARK: &str = "*";

const MODAL_WIDTH: u16 = 66;
/// The most this modal will take, however wide the terminal or long a plugin's
/// description. Past this, lines are too long to scan as a table.
const MODAL_WIDTH_MAX: u16 = 108;
/// Columns left to the interface either side, so the modal always reads as a
/// modal rather than as the whole screen.
const MODAL_MARGIN: u16 = 8;

/// How much a `←`/`→` moves a number. v1 steps its scalars by one and clamps
/// per field; the registry carries no bounds, so the step is all there is.
const NUMBER_STEP: f64 = 1.0;

/// One rendered line and the setting it edits — `None` for a section header or
/// a separator, which are decoration and never selectable.
type Row = Option<usize>;

/// Owner the core settings are listed under, so one section header covers them
/// and a write can be told apart from a plugin's by owner alone.
pub const CORE_OWNER: &str = "core";

/// A row that edits `settings.toml` rather than the registry.
///
/// The table below is v1's `SettingsField::meta()`, ported: the same words, and
/// the same live-vs-restart classification — which is a fact about the code
/// (mouse capture is an escape already sent, the notifier is a thread already
/// started), not a preference. Only settings v2 actually honours are listed: a
/// row that gates nothing reads as broken, so a returning surface brings its own
/// row with it.
struct CoreField {
    /// Dotted path, as the file spells it. Doubles as the row's label.
    id: &'static str,
    description: &'static str,
    /// Read it out of a settings value.
    get: fn(&Settings) -> Value,
    /// Write it back, ignoring a value of the wrong shape.
    set: fn(&mut Settings, &Value),
}

impl CoreField {
    /// Whether changing this field only takes effect on the next launch.
    ///
    /// *Asked* of `Settings::restart_only_differs` rather than recorded per field.
    /// That function is what `Config::adopt` actually consults, so it is the only
    /// answer that can be right — and the hand-written second copy this replaces
    /// had already drifted from it: both panel-width scalars were marked as
    /// applying live while `adopt` froze them and reported `NeedsRestart`.
    ///
    /// Derived by changing the field and asking, so a field added later is
    /// classified correctly without being classified twice.
    fn restart_required(&self) -> bool {
        let base = Settings::default();
        let mut changed = base.clone();
        let altered = match (self.get)(&base) {
            Value::Bool(on) => Value::Bool(!on),
            Value::Number(n) => Value::Number(n + 1.0),
            // Any name but the current one, so the write is always a change.
            Value::Text(current) => {
                Value::Text(if current == "off" { "auto" } else { "off" }.to_string())
            }
        };
        (self.set)(&mut changed, &altered);
        base.restart_only_differs(&changed)
    }
}

/// Every core row, in the order the file reads.
const CORE_FIELDS: &[CoreField] = &[
    CoreField {
        id: "features.soft_delete",
        description: "delete is reversible; off deletes for good, after asking",
        get: |s| Value::Bool(s.features.soft_delete),
        set: |s, v| {
            if let Value::Bool(on) = v {
                s.features.soft_delete = *on;
            }
        },
    },
    CoreField {
        id: "features.shell_pane",
        description: "a shell beside each agent",
        get: |s| Value::Bool(s.features.shell_pane),
        set: |s, v| {
            if let Value::Bool(on) = v {
                s.features.shell_pane = *on;
            }
        },
    },
    CoreField {
        id: "features.perf_hud",
        description: "the live performance counters",
        get: |s| Value::Bool(s.features.perf_hud),
        set: |s, v| {
            if let Value::Bool(on) = v {
                s.features.perf_hud = *on;
            }
        },
    },
    CoreField {
        id: "features.mouse",
        description: "mouse clicks, drags and hover",
        get: |s| Value::Bool(s.features.mouse),
        set: |s, v| {
            if let Value::Bool(on) = v {
                s.features.mouse = *on;
            }
        },
    },
    CoreField {
        id: "features.notifications",
        description: "desktop notifications when an agent needs you",
        get: |s| Value::Bool(s.features.notifications),
        set: |s, v| {
            if let Value::Bool(on) = v {
                s.features.notifications = *on;
            }
        },
    },
    CoreField {
        id: "features.version_check",
        description: "tell me when a newer release exists",
        get: |s| Value::Bool(s.features.version_check),
        set: |s, v| {
            if let Value::Bool(on) = v {
                s.features.version_check = *on;
            }
        },
    },
    CoreField {
        id: "features.auto_update",
        description: "install a newer release on startup",
        get: |s| Value::Bool(s.features.auto_update),
        set: |s, v| {
            if let Value::Bool(on) = v {
                s.features.auto_update = *on;
            }
        },
    },
    CoreField {
        id: "notifications.also_on_waiting",
        description: "notify when a turn finishes, not only when it blocks",
        get: |s| Value::Bool(s.notifications.also_on_waiting),
        set: |s, v| {
            if let Value::Bool(on) = v {
                s.notifications.also_on_waiting = *on;
            }
        },
    },
    CoreField {
        id: "notifications.suppress_for_active",
        description: "stay quiet about the session you are looking at",
        get: |s| Value::Bool(s.notifications.suppress_for_active),
        set: |s, v| {
            if let Value::Bool(on) = v {
                s.notifications.suppress_for_active = *on;
            }
        },
    },
    CoreField {
        id: "notifications.sound",
        description: "play the system notification sound",
        get: |s| Value::Bool(s.notifications.sound),
        set: |s, v| {
            if let Value::Bool(on) = v {
                s.notifications.sound = *on;
            }
        },
    },
    CoreField {
        id: "notifications.min_interval_secs",
        description: "seconds before one session may notify again",
        get: |s| Value::Number(s.notifications.min_interval_secs as f64),
        set: |s, v| {
            if let Value::Number(n) = v {
                s.notifications.min_interval_secs = n.max(0.0) as u64;
            }
        },
    },
    CoreField {
        id: "notifications.backend",
        description: "how a notification is delivered: auto, dbus, windows, off",
        get: |s| Value::Text(backend_name(s.notifications.backend).to_string()),
        set: |s, v| {
            if let Value::Text(name) = v {
                if let Some(backend) = backend_from(name) {
                    s.notifications.backend = backend;
                }
            }
        },
    },
    CoreField {
        id: "scrollback_lines",
        description: "lines of history each terminal keeps",
        get: |s| Value::Number(s.scrollback_lines as f64),
        set: |s, v| {
            if let Value::Number(n) = v {
                s.scrollback_lines = n.max(0.0) as usize;
            }
        },
    },
    CoreField {
        id: "two_panel_min_cols",
        description: "width at which the session column appears",
        get: |s| Value::Number(s.two_panel_min_cols as f64),
        set: |s, v| {
            if let Value::Number(n) = v {
                s.two_panel_min_cols = n.max(0.0) as u16;
            }
        },
    },
    // `three_panel_min_cols` is deliberately absent. It sized v1's third column
    // (info panel / tasks / files) and the interface has no third column, so
    // offering the row promised a control that could not do anything. The field
    // is still parsed — an existing `settings.toml` must keep loading rather than
    // fail on an unknown key — it simply has no reader and no UI.
    CoreField {
        id: "audit_retention_days",
        description: "days of audit history kept",
        get: |s| Value::Number(s.audit_retention_days as f64),
        set: |s, v| {
            if let Value::Number(n) = v {
                s.audit_retention_days = n.max(1.0) as u64;
            }
        },
    },
];

/// The delivery backends, in the order `←`/`→` cycles them. An enum, so it is
/// stepped rather than typed: there are four spellings and typing a fifth would
/// be silently ignored.
const BACKENDS: [(&str, NotificationBackend); 4] = [
    ("auto", NotificationBackend::Auto),
    ("dbus", NotificationBackend::Dbus),
    ("windows", NotificationBackend::Windows),
    ("off", NotificationBackend::Off),
];

fn backend_name(backend: NotificationBackend) -> &'static str {
    BACKENDS
        .iter()
        .find(|(_, candidate)| *candidate == backend)
        .map(|(name, _)| *name)
        .unwrap_or("auto")
}

fn backend_from(name: &str) -> Option<NotificationBackend> {
    BACKENDS
        .iter()
        .find(|(candidate, _)| *candidate == name)
        .map(|(_, backend)| *backend)
}

/// The next backend after `name`, wrapping — what a horizontal step does.
fn next_backend(name: &str, forward: bool) -> String {
    let at = BACKENDS
        .iter()
        .position(|(candidate, _)| *candidate == name)
        .unwrap_or(0);
    let count = BACKENDS.len();
    let next = if forward {
        (at + 1) % count
    } else {
        (at + count - 1) % count
    };
    BACKENDS[next].0.to_string()
}

/// The core rows as the renderer wants them: ordinary [`Setting`]s owned by
/// [`CORE_OWNER`], so one row renderer draws both halves of the screen.
///
/// `default` is what the file would hold with nothing set, so the renderer's
/// "changed" highlight means "you moved this" for a core row exactly as it does
/// for a plugin's.
fn core_rows(draft: &Settings) -> Vec<Setting> {
    let defaults = Settings::default();
    CORE_FIELDS
        .iter()
        .map(|field| Setting {
            plugin: CORE_OWNER.to_string(),
            id: field.id.to_string(),
            // The marker rides in the description because a row is one line and
            // this is the same information v1 puts in its own column.
            description: if field.restart_required() {
                format!("{RESTART_MARK} {}", field.description)
            } else {
                field.description.to_string()
            },
            default: (field.get)(&defaults),
            value: (field.get)(draft),
        })
        .collect()
}

/// What a keystroke asked the layer to do.
pub enum Outcome {
    /// Stay open; report the message if there is one.
    Stay(Option<String>),
    /// Write these settings to the file, and take their live half into force.
    Save(Box<Settings>),
    /// Restore or remove one interface file. Left for the loop, which owns the
    /// one code path that writes one and asks for the reload.
    Interface(Edit),
}

/// The two halves of the settings modal.
///
/// Tabs rather than two modals because they answer one question — "how is this
/// thurbox configured" — and because the second half has no business owning a
/// chord of its own: that is what made it unreachable when it was a pane.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Tab {
    #[default]
    Settings,
    Interface,
}

impl Tab {
    pub fn label(self) -> &'static str {
        match self {
            Tab::Settings => "Settings",
            Tab::Interface => "Interface",
        }
    }

    const ALL: [Tab; 2] = [Tab::Settings, Tab::Interface];

    fn step(self, forward: bool) -> Self {
        let count = Self::ALL.len();
        let at = Self::ALL.iter().position(|tab| *tab == self).unwrap_or(0);
        let next = if forward {
            (at + 1) % count
        } else {
            (at + count - 1) % count
        };
        Self::ALL[next]
    }
}

/// `]` and `[` move between tabs.
///
/// Every other candidate is taken by the settings half itself: `Tab` and `j`/`k`
/// move rows, `←`/`→` and `h`/`l` step a value, `Enter`/`Space` activate one.
/// The bracket pair is the one idiom left that collides with nothing, and the
/// tab headings are clickable for anyone who would rather not know it.
const NEXT_TAB: Replay = Replay::new(KeyCode::Char(']'), KeyModifiers::NONE, "]");
const PREVIOUS_TAB: Replay = Replay::new(KeyCode::Char('['), KeyModifiers::NONE, "[");

/// Most rows of files the interface half will show at once. Beyond this it
/// scrolls, so a long list cannot push the modal past the screen.
const INTERFACE_MAX_ROWS: usize = 18;

#[derive(Default)]
pub struct SettingsModal {
    /// Which half is on screen.
    tab: Tab,
    /// The interface half's own cursor and armed removal.
    interface: InterfaceTab,
    /// Index into the combined row list: core rows first, then the registry's.
    selected: usize,
    /// The in-progress text edit, when a `Text` row is being typed into.
    editing: Option<String>,
    /// The core settings as edited, seeded on first touch from what the *file*
    /// holds.
    ///
    /// A draft, unlike a plugin setting: these are a *file*, and some of them
    /// cannot take effect until the next launch, so applying each keystroke
    /// would mean writing the document a dozen times and lying about half of
    /// them. Dropped with the modal, which is what makes `Esc` a discard.
    ///
    /// Seeded from the file rather than from what is in force, which is the
    /// load-bearing half: seeded from what is *running*, a draft proposes
    /// reverting every restart-only change already saved
    /// ([`crate::kernel::config::Config::on_disk`]).
    draft: Option<Settings>,
    hits: Hits,
}

impl SettingsModal {
    /// Whether a keystroke is being typed into a value rather than driving the
    /// list — the modal's own capture, and why `j` types a `j` here.
    pub fn editing(&self) -> bool {
        self.editing.is_some()
    }

    /// Which half is on screen. Read by the click path, which has to know whose
    /// rows the hitboxes belong to.
    pub fn tab(&self) -> Tab {
        self.tab
    }

    pub fn hits(&self) -> &Hits {
        &self.hits
    }

    /// Whether the core half has unsaved edits — what makes the save hint appear.
    pub fn dirty(&self, on_disk: &Settings) -> bool {
        self.draft.as_ref().is_some_and(|draft| draft != on_disk)
    }

    /// Every row on screen: the core settings, then whatever plugins declared.
    ///
    /// Rebuilt per call rather than cached: it is a dozen rows, and a cache would
    /// be one more thing that can disagree with the registry.
    fn rows(&self, registry: &Registry, on_disk: &Settings) -> Vec<Setting> {
        let draft = self.draft.clone().unwrap_or_else(|| on_disk.clone());
        let mut rows = core_rows(&draft);
        rows.extend(registry.settings().iter().cloned());
        rows
    }

    /// The draft to edit, seeded from what the file holds the first time it is
    /// touched — see [`SettingsModal::draft`] for why not from what is in force.
    fn draft_mut(&mut self, on_disk: &Settings) -> &mut Settings {
        self.draft.get_or_insert_with(|| on_disk.clone())
    }

    /// Write a core value into the draft. Nothing reaches the file until save.
    fn write_core(&mut self, on_disk: &Settings, id: &str, value: Value) -> Option<String> {
        let field = CORE_FIELDS.iter().find(|field| field.id == id)?;
        let draft = self.draft_mut(on_disk);
        (field.set)(draft, &value);
        Some(format!("{id} = {} (unsaved)", display(&value)))
    }

    pub fn on_key(
        &mut self,
        key: &KeyEvent,
        registry: &mut Registry,
        on_disk: &Settings,
        inventory: &[FileRow],
    ) -> Outcome {
        // Tab switching outranks both halves, but not a text edit: while a value
        // is being typed into, `[` is a bracket.
        if self.editing.is_none() {
            match key.code {
                KeyCode::Char(']') => {
                    self.tab = self.tab.step(true);
                    return Outcome::Stay(None);
                }
                KeyCode::Char('[') => {
                    self.tab = self.tab.step(false);
                    return Outcome::Stay(None);
                }
                _ => {}
            }
        }
        if self.tab == Tab::Interface {
            let (message, edit) = self.interface.on_key(key, inventory, &self.hits);
            return match edit {
                Some(edit) => Outcome::Interface(edit),
                None => Outcome::Stay(message),
            };
        }
        let total = self.rows(registry, on_disk).len();
        if total == 0 {
            return Outcome::Stay(None);
        }
        self.selected = self.selected.min(total - 1);
        if self.editing.is_some() {
            return Outcome::Stay(self.edit_key(key, registry, on_disk));
        }
        // The core half is a document, so it is saved rather than applied per
        // keystroke — and only when there is something to save.
        if key.code == KeyCode::Char('s') && key.modifiers.contains(KeyModifiers::CONTROL) {
            return match self.draft.clone() {
                Some(draft) if self.dirty(on_disk) => Outcome::Save(Box::new(draft)),
                _ => Outcome::Stay(Some("nothing to save".to_string())),
            };
        }
        match key.code {
            KeyCode::Char('j') | KeyCode::Down | KeyCode::Tab => {
                self.selected = (self.selected + 1) % total;
            }
            KeyCode::Char('k') | KeyCode::Up | KeyCode::BackTab => {
                self.selected = (self.selected + total - 1) % total;
            }
            // A screenful is however many rows the last frame fitted, which is
            // exactly the number of row hitboxes it recorded. These clamp
            // rather than wrap: paging past the end means "the end".
            KeyCode::PageDown => self.selected = (self.selected + self.page()).min(total - 1),
            KeyCode::PageUp => self.selected = self.selected.saturating_sub(self.page()),
            KeyCode::Home => self.selected = 0,
            KeyCode::End => self.selected = total - 1,
            KeyCode::Char(' ') | KeyCode::Enter => {
                return Outcome::Stay(self.activate(registry, on_disk))
            }
            KeyCode::Right | KeyCode::Char('l') => {
                return Outcome::Stay(self.step(registry, on_disk, NUMBER_STEP))
            }
            KeyCode::Left | KeyCode::Char('h') => {
                return Outcome::Stay(self.step(registry, on_disk, -NUMBER_STEP))
            }
            KeyCode::Char('d') => return Outcome::Stay(self.reset(registry, on_disk)),
            _ => {}
        }
        Outcome::Stay(None)
    }

    /// The row under the cursor.
    fn current(&self, registry: &Registry, on_disk: &Settings) -> Option<Setting> {
        self.rows(registry, on_disk).get(self.selected).cloned()
    }

    /// `Space`/`Enter`: flip a bool, start editing a text value, or step a
    /// number — whichever the declared type admits.
    fn activate(&mut self, registry: &mut Registry, on_disk: &Settings) -> Option<String> {
        let setting = self.current(registry, on_disk)?;
        match &setting.value {
            Value::Bool(on) => self.put(registry, on_disk, &setting, Value::Bool(!on)),
            Value::Number(_) => self.step(registry, on_disk, NUMBER_STEP),
            Value::Text(text) => {
                // An enum is stepped, not typed: there are four spellings and a
                // fifth would be silently ignored.
                if setting.plugin == CORE_OWNER {
                    return self.step(registry, on_disk, NUMBER_STEP);
                }
                self.editing = Some(text.clone());
                None
            }
        }
    }

    fn step(&mut self, registry: &mut Registry, on_disk: &Settings, by: f64) -> Option<String> {
        let setting = self.current(registry, on_disk)?;
        match &setting.value {
            Value::Number(n) => self.put(registry, on_disk, &setting, Value::Number(n + by)),
            // A bool has two states, so a horizontal step is the same act as a
            // toggle.
            Value::Bool(on) => self.put(registry, on_disk, &setting, Value::Bool(!on)),
            // A core text value is an enum, and it cycles; a plugin's is free
            // text, which only typing changes.
            Value::Text(name) if setting.plugin == CORE_OWNER => {
                let next = next_backend(name, by >= 0.0);
                self.put(registry, on_disk, &setting, Value::Text(next))
            }
            Value::Text(_) => None,
        }
    }

    fn reset(&mut self, registry: &mut Registry, on_disk: &Settings) -> Option<String> {
        let setting = self.current(registry, on_disk)?;
        if setting.plugin == CORE_OWNER {
            // Back to what an untouched file would hold — still a draft, so the
            // file is not written until save.
            let value = setting.default.clone();
            return self.write_core(on_disk, &setting.id, value);
        }
        Some(
            match registry.set_setting(&setting.plugin, &setting.id, None) {
                Ok(()) => format!("{}.{} reset to its default", setting.plugin, setting.id),
                Err(e) => e,
            },
        )
    }

    /// Route a write by owner: the draft for a core row, the registry for a
    /// plugin's. The one place the two halves differ, so it is the one place that
    /// has to know they do.
    fn put(
        &mut self,
        registry: &mut Registry,
        on_disk: &Settings,
        setting: &Setting,
        value: Value,
    ) -> Option<String> {
        if setting.plugin == CORE_OWNER {
            return self.write_core(on_disk, &setting.id, value);
        }
        Some(write(registry, setting, value))
    }

    /// Typing into a `Text` value. `Enter` commits, `Esc` abandons.
    fn edit_key(
        &mut self,
        key: &KeyEvent,
        registry: &mut Registry,
        on_disk: &Settings,
    ) -> Option<String> {
        let Some(buffer) = &mut self.editing else {
            return None;
        };
        match key.code {
            KeyCode::Char(c) => {
                buffer.push(c);
                None
            }
            KeyCode::Backspace => {
                buffer.pop();
                None
            }
            KeyCode::Enter => {
                let text = self.editing.take()?;
                let setting = self.current(registry, on_disk)?;
                self.put(registry, on_disk, &setting, Value::Text(text))
            }
            KeyCode::Esc => {
                self.editing = None;
                Some("edit discarded".to_string())
            }
            _ => None,
        }
    }

    /// How far `PageUp`/`PageDown` step: one screenful of rows, never zero.
    fn page(&self) -> usize {
        self.hits.rows.len().max(1)
    }

    /// A click selects the row under it, then acts on it — a bool toggles, as
    /// it does in v1, so the mouse can drive a checkbox in one press.
    pub fn on_click(
        &mut self,
        x: u16,
        y: u16,
        registry: &mut Registry,
        on_disk: &Settings,
    ) -> Option<String> {
        if self.tab == Tab::Interface {
            self.interface.on_click(x, y, &self.hits);
            return None;
        }
        let hit = self.hits.row_at(x, y)?;
        let reselecting = hit != self.selected;
        self.selected = hit;
        if reselecting {
            return None;
        }
        self.activate(registry, on_disk)
    }

    pub fn render(
        &mut self,
        frame: &mut Frame,
        area: Rect,
        registry: &Registry,
        on_disk: &Settings,
        files: Files<'_>,
        chrome: Chrome,
    ) {
        self.hits.clear();
        let rows_data = self.rows(registry, on_disk);
        let settings = rows_data.as_slice();
        // Sized to what the rows need, then clamped to the screen — the same rule
        // the height below already follows. Two fixed widths could not do this:
        // the description column is what is left after the widest id and the
        // widest value, so one long id starved every description of ~40 columns
        // and the wide tier was already in force when it happened. Deriving it
        // also means a plugin declaring a long description widens the modal
        // instead of being quietly cut off.
        let cap = area
            .width
            .saturating_sub(MODAL_MARGIN)
            .clamp(MODAL_WIDTH, MODAL_WIDTH_MAX);
        let width = desired_width(settings).clamp(MODAL_WIDTH, cap);
        if self.tab == Tab::Interface {
            self.render_interface(frame, area, width, files, chrome);
            return;
        }
        let inner_width = usize::from(width.saturating_sub(2));

        let (lines, rows) = self.build_rows(settings, inner_width, chrome);
        let footer = self.footer(settings, self.dirty(on_disk), chrome);
        // Sized to the real content — headers, separators and rows — then
        // clamped to the screen, exactly as v1 sizes its panel.
        let height = (lines.len() + footer.len() + 2)
            .try_into()
            .unwrap_or(u16::MAX)
            .min(area.height.saturating_sub(2))
            .max(6);
        let modal = chrome::centered_fixed_rect(width, height, area);
        let inner = self.frame_with_tabs(frame, modal, chrome);

        // The footer is pinned; the rows scroll-window around the active one.
        let visible = usize::from(inner.height).saturating_sub(footer.len());
        let active_line = rows
            .iter()
            .position(|row| *row == Some(self.selected))
            .unwrap_or(0);
        let start = if lines.len() > visible && visible > 0 {
            active_line
                .saturating_sub(visible / 2)
                .min(lines.len() - visible)
        } else {
            0
        };

        let mut painted: Vec<Line> = Vec::with_capacity(visible + footer.len());
        for (screen_row, (line, row)) in lines
            .into_iter()
            .zip(rows)
            .skip(start)
            .take(visible)
            .enumerate()
        {
            if let Some(index) = row {
                self.hits.rows.push((
                    Rect::new(inner.x, inner.y + screen_row as u16, inner.width, 1),
                    index,
                ));
            }
            painted.push(line);
        }
        painted.extend(footer);
        frame.render_widget(Paragraph::new(painted), inner);

        let footer_row = Rect::new(
            inner.x,
            inner.y + inner.height.saturating_sub(1),
            inner.width,
            1,
        );
        // ` Save ` appears only with something to save: the core half is a
        // document, while a plugin's value went through the registry the moment
        // the key was pressed. ` Close ` replays the `Esc` the layer above turns
        // into a close, which is also what discards an unsaved draft.
        let mut pills = Vec::new();
        if self.dirty(on_disk) {
            pills.push(Pill {
                label: "Save",
                primary: true,
                key: Replay::CTRL_S,
            });
        }
        pills.push(Pill {
            label: "Close",
            primary: !self.dirty(on_disk),
            key: Replay::ESC,
        });
        // Extended, never assigned: the tab headings are already in here, and
        // replacing the list would leave them drawn but unclickable.
        let placed = chrome::button_bar(frame, footer_row, &pills, chrome);
        self.hits.buttons.extend(placed);
    }

    /// Frame the modal with its tabs in the title, recording a hitbox per tab.
    ///
    /// A heading replays the key that moves *towards* it, so a click and the
    /// bracket keys are one code path — the rule the footer pills already
    /// follow. Exact for two tabs, which is what there are; a third would need a
    /// select-by-index key, since a replay is one keystroke and cannot say "two
    /// to the right".
    fn frame_with_tabs(&mut self, frame: &mut Frame, modal: Rect, chrome: Chrome) -> Rect {
        let active = Tab::ALL
            .iter()
            .position(|tab| *tab == self.tab)
            .unwrap_or(0);
        let labels: Vec<(&str, bool)> = Tab::ALL
            .iter()
            .map(|tab| (tab.label(), *tab == self.tab))
            .collect();
        let (inner, rects) = chrome::modal_frame_tabs(frame, modal, &labels, chrome);
        for (nth, rect) in rects.into_iter().enumerate() {
            let towards = if nth < active { PREVIOUS_TAB } else { NEXT_TAB };
            self.hits.buttons.push((rect, towards));
        }
        inner
    }

    /// Draw the Interface half: the file list, its footer, and the same pills.
    fn render_interface(
        &mut self,
        frame: &mut Frame,
        area: Rect,
        width: u16,
        files: Files<'_>,
        chrome: Chrome,
    ) {
        // Sized to its content the way the settings half is: the directory line,
        // a row per file up to the cap, the footer, and the border. A fixed
        // height left blank rows under a short list.
        let listed = files.rows.len().min(INTERFACE_MAX_ROWS);
        let height = u16::try_from(listed + 4)
            .unwrap_or(u16::MAX)
            .min(area.height.saturating_sub(2))
            .max(6);
        let modal = chrome::centered_fixed_rect(width, height, area);
        let inner = self.frame_with_tabs(frame, modal, chrome);
        if inner.height < 2 {
            return;
        }

        let body = Rect::new(
            inner.x,
            inner.y,
            inner.width,
            inner.height.saturating_sub(1),
        );
        self.interface
            .render(frame, body, files, chrome, &mut self.hits);

        let footer_row = Rect::new(
            inner.x,
            inner.y + inner.height.saturating_sub(1),
            inner.width,
            1,
        );
        frame.render_widget(
            Paragraph::new(self.interface.footer(files.rows, chrome)),
            footer_row,
        );
        let pills = [Pill {
            label: "Close",
            primary: true,
            key: Replay::ESC,
        }];
        let placed = chrome::button_bar(frame, footer_row, &pills, chrome);
        self.hits.buttons.extend(placed);
    }

    /// One line per section header and per setting, plus the row map the
    /// window and the hitboxes are built from.
    #[allow(clippy::wrong_self_convention)]
    fn build_rows<'a>(
        &self,
        settings: &[Setting],
        width: usize,
        chrome: Chrome,
    ) -> (Vec<Line<'a>>, Vec<Row>) {
        // The widest value across every row, so the value column aligns —
        // v1's `value_width`.
        let value_width = settings
            .iter()
            .map(|setting| value_text(&setting.value).chars().count())
            .max()
            .unwrap_or(3);
        let id_width = settings
            .iter()
            .map(|setting| setting.id.chars().count())
            .max()
            .unwrap_or(0);

        let mut lines = Vec::new();
        let mut rows = Vec::new();
        let mut section: Option<&str> = None;
        for (index, setting) in settings.iter().enumerate() {
            if section != Some(setting.plugin.as_str()) {
                if section.is_some() {
                    lines.push(Line::default());
                    rows.push(None);
                }
                lines.push(Line::from(Span::styled(
                    format!("  {}", setting.plugin.to_uppercase()),
                    chrome.section_header(),
                )));
                rows.push(None);
                section = Some(setting.plugin.as_str());
            }
            let editing = self.editing.as_deref().filter(|_| index == self.selected);
            lines.push(setting_line(
                setting,
                index == self.selected,
                editing,
                width,
                id_width,
                value_width,
                chrome,
            ));
            rows.push(Some(index));
        }
        (lines, rows)
    }

    /// The pinned footer: which setting is selected, then the key hints.
    fn footer<'a>(&self, settings: &[Setting], dirty: bool, chrome: Chrome) -> Vec<Line<'a>> {
        let mut footer = Vec::new();
        match settings.get(self.selected) {
            // A core row is named by its path in the file, which is where the
            // change will land; a plugin's by `plugin.id`.
            Some(setting) if setting.plugin == CORE_OWNER => footer.push(Line::from(vec![
                Span::styled("  settings.toml  ", chrome.muted()),
                Span::styled(setting.id.clone(), chrome.muted()),
                // Said here rather than left to be inferred: the mark is the only
                // thing distinguishing a row that applies now from one that waits,
                // and an unexplained glyph carries none of that.
                Span::styled(format!("   {RESTART_MARK} needs restart"), chrome.muted()),
            ])),
            Some(setting) => footer.push(Line::from(vec![
                Span::styled("  key  ", chrome.muted()),
                Span::styled(format!("{}.{}", setting.plugin, setting.id), chrome.muted()),
            ])),
            None => footer.push(Line::from(Span::styled(
                "  nothing to configure",
                chrome.muted(),
            ))),
        }
        let hints: &[(&str, &str)] = if self.editing.is_some() {
            &[
                ("type", " edit  "),
                ("Enter", " commit  "),
                ("Esc", " discard"),
            ]
        } else if dirty {
            // The restart mark is on the rows themselves; this says what the key is.
            &[
                ("j/k", " move  "),
                ("Space", " toggle  "),
                ("←/→", " adjust  "),
                ("Ctrl+S", " save  "),
                ("Esc", " discard"),
            ]
        } else {
            &[
                ("j/k", " move  "),
                ("Space", " toggle  "),
                ("←/→", " adjust  "),
                ("d", " reset  "),
                ("[/]", " tab"),
            ]
        };
        footer.push(chrome::hint_line(hints, chrome));
        footer
    }
}

/// Write a value back through the registry, which validates the type and
/// persists. The message is what the loop reports.
fn write(registry: &mut Registry, setting: &Setting, value: Value) -> String {
    let key = format!("{}.{}", setting.plugin, setting.id);
    let shown = display(&value);
    match registry.set_setting(&setting.plugin, &setting.id, Some(value)) {
        Ok(()) => format!("{key} = {shown}"),
        Err(e) => e,
    }
}

/// A value as the row shows it: `on`/`off` for a flag, the number or the text.
fn display(value: &Value) -> String {
    match value {
        Value::Bool(true) => "on".to_string(),
        Value::Bool(false) => "off".to_string(),
        // Whole numbers are the common case and `30` reads better than `30.0`.
        Value::Number(n) if n.fract() == 0.0 => format!("{n:.0}"),
        Value::Number(n) => format!("{n}"),
        Value::Text(text) => text.clone(),
    }
}

/// The value column, with v1's `‹ n ›` stepper around an adjustable number.
fn value_text(value: &Value) -> String {
    match value {
        Value::Number(_) => format!("‹ {} ›", display(value)),
        _ => display(value),
    }
}

/// One setting row, in fixed columns so the values line up:
/// `▸ <id (bold)> <description (dimmed)> … <value right-justified>`.
///
/// The width the rows would like: the widest id, the longest description and the
/// widest value side by side, plus the frame, the pointer and the gaps between
/// them.
///
/// Mirrors the arithmetic in [`setting_line`] rather than guessing at it, so a row
/// that fits here fits there. Kept as its own function because the caller needs
/// the answer *before* it can build a row.
fn desired_width(settings: &[Setting]) -> u16 {
    let longest = |f: fn(&Setting) -> usize| settings.iter().map(f).max().unwrap_or(0);
    let id = longest(|s| s.id.chars().count());
    let value = longest(|s| value_text(&s.value).chars().count()).max(3);
    // The restart mark rides in the description of a restart-only row, so the
    // column has to allow for it or the mark is what pushes a description over
    // the edge.
    let description = longest(|s| s.description.chars().count()) + 2;
    // 2 frame + 2 pointer + id + 1 + description + 1 gap + value.
    u16::try_from(2 + 2 + id + 1 + description + 1 + value).unwrap_or(MODAL_WIDTH_MAX)
}

/// v1's `settings_modal::field_line`, minus the restart marker — a v2 setting
/// is read live by the plugin that declared it, so none of them can require a
/// restart.
#[allow(clippy::too_many_arguments)]
fn setting_line<'a>(
    setting: &Setting,
    active: bool,
    editing: Option<&str>,
    width: usize,
    id_width: usize,
    value_width: usize,
    chrome: Chrome,
) -> Line<'a> {
    let pointer = if active { "▸ " } else { "  " };
    let id_style = if active {
        chrome.selected_item()
    } else {
        chrome
            .keybind_desc()
            .fg(chrome.palette.text_secondary)
            .add_modifier(Modifier::BOLD)
    };
    let desc_style = if active {
        chrome.selected_item().remove_modifier(Modifier::BOLD)
    } else {
        chrome.muted()
    };
    let changed = setting.value != setting.default;
    let value_style = match (&setting.value, changed) {
        // A knob you moved should be visible at a glance among thirty defaults.
        (_, true) => chrome.selected_item(),
        (Value::Bool(true), _) => chrome
            .muted()
            .fg(chrome.palette.tool_allowed)
            .add_modifier(Modifier::BOLD),
        _ => chrome.muted(),
    };

    let value = match editing {
        // A block caret, so an open edit is unmistakably where the keys go.
        Some(buffer) => format!("{buffer}█"),
        None => value_text(&setting.value),
    };
    let id = chrome::pad(&setting.id, id_width);
    let block = 2 + id_width + 1;
    let right = value.chars().count().max(value_width);
    let description = chrome::truncate(
        &setting.description,
        width.saturating_sub(block + right + 1),
    );
    let gap = width
        .saturating_sub(block + description.chars().count() + right)
        .max(1);

    // The value is right-justified in its column, as v1 justifies it, so every
    // row's value ends on the same screen column however long its id is.
    let value_pad = right.saturating_sub(value.chars().count());
    Line::from(vec![
        Span::styled(format!("{pointer}{id} "), id_style),
        Span::styled(description, desc_style),
        Span::raw(" ".repeat(gap + value_pad)),
        Span::styled(value, value_style),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setting(plugin: &str, id: &str, default: Value) -> Setting {
        Setting {
            plugin: plugin.to_string(),
            id: id.to_string(),
            description: format!("what {id} does"),
            value: default.clone(),
            default,
        }
    }

    fn registry_with(settings: Vec<Setting>) -> Registry {
        let mut registry = Registry::default();
        registry.declare(Vec::new(), settings);
        registry
    }

    #[test]
    fn a_number_renders_with_the_stepper_v1_draws() {
        assert_eq!(value_text(&Value::Number(30.0)), "‹ 30 ›");
        assert_eq!(value_text(&Value::Bool(true)), "on");
        assert_eq!(value_text(&Value::Text("hi".into())), "hi");
    }

    /// A keystroke with no modifier.
    fn press(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, crossterm::event::KeyModifiers::NONE)
    }

    fn ctrl(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::CONTROL)
    }

    /// The message a keystroke produced, ignoring a save.
    fn message(outcome: Outcome) -> Option<String> {
        match outcome {
            Outcome::Stay(message) => message,
            Outcome::Save(_) => Some("saved".to_string()),
            Outcome::Interface(edit) => Some(format!("interface {edit:?}")),
        }
    }

    /// A modal whose cursor is on the nth row a plugin declared: the core rows
    /// come first, and there is no point restating their count in every test.
    fn on_plugin_row(nth: usize) -> SettingsModal {
        SettingsModal {
            selected: CORE_FIELDS.len() + nth,
            ..SettingsModal::default()
        }
    }

    /// A modal whose cursor is on the core row named `id`.
    fn on_core_row(id: &str) -> SettingsModal {
        SettingsModal {
            selected: core_row(id),
            ..SettingsModal::default()
        }
    }

    #[test]
    fn each_declared_type_is_editable() {
        let dir = tempfile::tempdir().expect("temp dir");
        let _guard = crate::paths::TestPathGuard::new(dir.path());
        let on_disk = Settings::default();
        let mut registry = registry_with(vec![
            setting("a", "flag", Value::Bool(false)),
            setting("a", "size", Value::Number(4.0)),
            setting("b", "label", Value::Text("x".into())),
        ]);
        let mut modal = on_plugin_row(0);
        message(modal.on_key(&press(KeyCode::Char(' ')), &mut registry, &on_disk, &[]));
        assert_eq!(registry.settings()[0].value, Value::Bool(true));

        message(modal.on_key(&press(KeyCode::Char('j')), &mut registry, &on_disk, &[]));
        message(modal.on_key(&press(KeyCode::Right), &mut registry, &on_disk, &[]));
        assert_eq!(registry.settings()[1].value, Value::Number(5.0));

        message(modal.on_key(&press(KeyCode::Char('j')), &mut registry, &on_disk, &[]));
        message(modal.on_key(&press(KeyCode::Enter), &mut registry, &on_disk, &[]));
        assert!(modal.editing(), "Enter on a text value starts an edit");
        message(modal.on_key(&press(KeyCode::Char('y')), &mut registry, &on_disk, &[]));
        message(modal.on_key(&press(KeyCode::Enter), &mut registry, &on_disk, &[]));
        assert_eq!(registry.settings()[2].value, Value::Text("xy".into()));
        assert!(!modal.editing());
    }

    #[test]
    fn reset_puts_a_setting_back_to_its_default() {
        let dir = tempfile::tempdir().expect("temp dir");
        let _guard = crate::paths::TestPathGuard::new(dir.path());
        let on_disk = Settings::default();
        let mut registry = registry_with(vec![setting("a", "flag", Value::Bool(false))]);
        let mut modal = on_plugin_row(0);
        message(modal.on_key(&press(KeyCode::Char(' ')), &mut registry, &on_disk, &[]));
        assert_eq!(registry.settings()[0].value, Value::Bool(true));
        message(modal.on_key(&press(KeyCode::Char('d')), &mut registry, &on_disk, &[]));
        assert_eq!(registry.settings()[0].value, Value::Bool(false));
    }

    // ── the core half ──────────────────────────────────────────────────────

    /// The row index of a core setting by its path.
    fn core_row(id: &str) -> usize {
        CORE_FIELDS
            .iter()
            .position(|field| field.id == id)
            .unwrap_or_else(|| panic!("no core row named {id}"))
    }

    #[test]
    fn the_core_settings_are_listed_beside_what_plugins_declared() {
        let on_disk = Settings::default();
        let registry = registry_with(vec![setting("a", "flag", Value::Bool(false))]);
        let modal = SettingsModal::default();
        let rows = modal.rows(&registry, &on_disk);
        assert_eq!(rows.len(), CORE_FIELDS.len() + 1);
        assert!(rows.iter().any(|row| row.id == "features.soft_delete"));
        assert!(
            rows.last().is_some_and(|row| row.plugin == "a"),
            "a plugin's rows follow the core ones"
        );
    }

    #[test]
    fn a_restart_only_row_says_so_before_it_is_changed() {
        let rows = core_rows(&Settings::default());
        let mouse = &rows[core_row("features.mouse")];
        assert!(
            mouse.description.starts_with(RESTART_MARK),
            "{}",
            mouse.description
        );
        let soft = &rows[core_row("features.soft_delete")];
        assert!(
            !soft.description.starts_with(RESTART_MARK),
            "{}",
            soft.description
        );
    }

    #[test]
    fn a_core_edit_is_a_draft_until_it_is_saved() {
        let dir = tempfile::tempdir().expect("temp dir");
        let _guard = crate::paths::TestPathGuard::new(dir.path());
        let on_disk = Settings::default();
        let mut registry = registry_with(Vec::new());
        let mut modal = on_core_row("features.soft_delete");

        assert!(!modal.dirty(&on_disk));
        let reported =
            message(modal.on_key(&press(KeyCode::Char(' ')), &mut registry, &on_disk, &[]));
        assert!(
            reported.unwrap_or_default().contains("unsaved"),
            "the row says the change has not landed yet"
        );
        assert!(modal.dirty(&on_disk));
        assert!(on_disk.features.soft_delete, "and the file has not moved");

        // Saving hands the whole draft over; the file and the live half are the
        // loop's business, not the modal's.
        match modal.on_key(&ctrl(KeyCode::Char('s')), &mut registry, &on_disk, &[]) {
            Outcome::Save(draft) => assert!(!draft.features.soft_delete),
            other => panic!(
                "expected a save, got {}",
                message(other).unwrap_or_default()
            ),
        }
    }

    #[test]
    fn saving_with_nothing_changed_says_so() {
        let on_disk = Settings::default();
        let mut registry = registry_with(Vec::new());
        let mut modal = SettingsModal::default();
        let reported =
            message(modal.on_key(&ctrl(KeyCode::Char('s')), &mut registry, &on_disk, &[]));
        assert_eq!(reported.as_deref(), Some("nothing to save"));
    }

    #[test]
    fn a_core_number_steps_and_a_core_enum_cycles() {
        let on_disk = Settings::default();
        let mut registry = registry_with(Vec::new());
        let mut modal = on_core_row("notifications.min_interval_secs");
        message(modal.on_key(&press(KeyCode::Right), &mut registry, &on_disk, &[]));
        let draft = modal.draft.clone().expect("a draft");
        assert_eq!(
            draft.notifications.min_interval_secs,
            on_disk.notifications.min_interval_secs + 1
        );

        // The backend is an enum: typing a fifth spelling would be ignored, so it
        // is stepped through the four that exist.
        modal.selected = core_row("notifications.backend");
        message(modal.on_key(&press(KeyCode::Right), &mut registry, &on_disk, &[]));
        assert_eq!(
            modal.draft.as_ref().map(|d| d.notifications.backend),
            Some(NotificationBackend::Dbus)
        );
        message(modal.on_key(&press(KeyCode::Left), &mut registry, &on_disk, &[]));
        assert_eq!(
            modal.draft.as_ref().map(|d| d.notifications.backend),
            Some(NotificationBackend::Auto),
            "and it wraps both ways"
        );
        assert!(
            !modal.editing(),
            "an enum is never typed into, so no edit was started"
        );
    }

    #[test]
    fn resetting_a_core_row_stays_a_draft() {
        let on_disk = Settings::default();
        let mut registry = registry_with(Vec::new());
        let mut modal = on_core_row("audit_retention_days");
        message(modal.on_key(&press(KeyCode::Right), &mut registry, &on_disk, &[]));
        message(modal.on_key(&press(KeyCode::Char('d')), &mut registry, &on_disk, &[]));
        assert_eq!(
            modal.draft.as_ref().map(|d| d.audit_retention_days),
            Some(Settings::default().audit_retention_days)
        );
        assert!(
            !modal.dirty(&on_disk),
            "back to what the file holds is not a change"
        );
    }

    #[test]
    fn an_empty_registry_renders_rather_than_panicking() {
        // Nothing declared is a real state — a user who removed every plugin —
        // and the modal must say so rather than divide by an empty list.
        let registry = registry_with(Vec::new());
        let mut modal = SettingsModal::default();
        let palette = crate::session::theme_config::ThemePreset::Default.palette();
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 20)).expect("terminal");
        terminal
            .draw(|frame| {
                modal.render(
                    frame,
                    frame.area(),
                    &registry,
                    &Settings::default(),
                    Files {
                        rows: &[],
                        dir: "ui",
                    },
                    Chrome::new(&palette),
                );
            })
            .expect("draw");
    }

    /// Every cell of a rendered modal as one string.
    fn painted(modal: &mut SettingsModal, registry: &Registry, on_disk: &Settings) -> String {
        let palette = crate::session::theme_config::ThemePreset::Default.palette();
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 30)).expect("terminal");
        terminal
            .draw(|frame| {
                modal.render(
                    frame,
                    frame.area(),
                    registry,
                    on_disk,
                    Files {
                        rows: &[],
                        dir: "ui",
                    },
                    Chrome::new(&palette),
                );
            })
            .expect("draw");
        terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect()
    }

    #[test]
    fn an_open_edit_paints_its_own_caret() {
        // The caret is a GLYPH, not the terminal's cursor: a modal is drawn over
        // whatever it covers, and a painted caret survives that without having
        // to arbitrate the real cursor with the panes underneath. Asserted
        // because a field with no caret still accepts input — the loss only
        // shows up later, as "where am I typing?".
        let mut registry =
            registry_with(vec![setting("plugin", "label", Value::Text("hi".into()))]);
        let on_disk = Settings::default();
        let mut modal = on_plugin_row(0);
        message(modal.on_key(&press(KeyCode::Enter), &mut registry, &on_disk, &[]));
        assert!(modal.editing(), "Enter on a text value starts an edit");

        assert!(
            painted(&mut modal, &registry, &on_disk).contains('\u{2588}'),
            "an open edit must show where the keys go"
        );
    }

    /// The restart marker and the reload's own decision must be one fact.
    ///
    /// They were two: a hand-written `restart` flag per row, and
    /// `Settings::restart_only_differs`, which is what `Config::adopt` consults.
    /// They already disagreed about both panel-width scalars — the modal promised
    /// they applied live while `adopt` froze them and reported `NeedsRestart`.
    #[test]
    fn the_restart_marker_agrees_with_what_the_reload_does() {
        let base = Settings::default();
        for field in CORE_FIELDS {
            // What the row claims.
            let claimed = field.restart_required();

            // What a reload would actually do with that field changed.
            let mut changed = base.clone();
            let altered = match (field.get)(&base) {
                Value::Bool(on) => Value::Bool(!on),
                Value::Number(n) => Value::Number(n + 1.0),
                Value::Text(current) => {
                    Value::Text(if current == "off" { "auto" } else { "off" }.to_string())
                }
            };
            (field.set)(&mut changed, &altered);
            assert_ne!(
                (field.get)(&changed),
                (field.get)(&base),
                "{}: the probe must actually change the field",
                field.id
            );

            let mut config = crate::kernel::config::Config::detached(base.clone());
            let reloaded = config.adopt(changed);
            let needs_restart = matches!(reloaded, crate::kernel::config::Reloaded::NeedsRestart);
            assert_eq!(
                claimed, needs_restart,
                "{}: the row says restart={claimed}, the reload says {needs_restart}",
                field.id
            );
        }
    }

    /// The two rows that were actually wrong, pinned by name.
    ///
    /// The sweep above compares the marker against `adopt`, and both now ask
    /// `restart_only_differs` — so it holds the two in step but cannot say the
    /// answer is *right*. These can: a panel width is read once at startup, and a
    /// pane toggle is read every frame.
    #[test]
    fn the_panel_widths_are_restart_only_and_the_pane_toggles_are_not() {
        let marker = |id: &str| {
            CORE_FIELDS
                .iter()
                .find(|field| field.id == id)
                .unwrap_or_else(|| panic!("no core field {id}"))
                .restart_required()
        };

        assert!(
            marker("two_panel_min_cols"),
            "the panel width is frozen at startup, so it needs a restart"
        );
        // `three_panel_min_cols` is not offered at all: there is no third column
        // for it to size, and a control that cannot act is worse than none.
        assert!(
            !CORE_FIELDS
                .iter()
                .any(|field| field.id == "three_panel_min_cols"),
            "a setting for a column the interface does not have must not be offered"
        );
        for id in ["features.shell_pane", "features.soft_delete"] {
            assert!(!marker(id), "{id} is read every frame, so it applies live");
        }
    }
}
