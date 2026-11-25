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

const RESTART_MARK: &str = "*";

const MODAL_WIDTH: u16 = 66;
const MODAL_WIDTH_MAX: u16 = 108;
const MODAL_MARGIN: u16 = 8;
const VALUE_WIDTH_MAX: usize = 16;

const NUMBER_STEP: f64 = 1.0;

type Row = Option<usize>;

pub const CORE_OWNER: &str = "core";

struct CoreField {
    id: &'static str,
    description: &'static str,
    get: fn(&Settings) -> Value,
    set: fn(&mut Settings, &Value),
}

impl CoreField {
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

const CORE_FIELDS: &[CoreField] = &[
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
        id: "hidden_terminal_secs",
        description: "seconds off screen before a terminal is dropped; 0 keeps all",
        get: |s| Value::Number(s.hidden_terminal_secs as f64),
        set: |s, v| {
            if let Value::Number(n) = v {
                s.hidden_terminal_secs = n.max(0.0) as u64;
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
    CoreField {
        id: "git_poll_secs",
        description: "seconds between git stats of a session; 0 is off",
        get: |s| Value::Number(s.git_poll_secs as f64),
        // Floored at 0 and nowhere else: 0 is the off switch, and a machine
        // where every process launch is scanned is exactly where someone wants
        // it. Every other value is the operator's trade of freshness for
        // subprocesses.
        set: |s, v| {
            if let Value::Number(n) = v {
                s.git_poll_secs = n.max(0.0) as u64;
            }
        },
    },
];

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

fn core_rows(draft: &Settings) -> Vec<Setting> {
    let defaults = Settings::default();
    CORE_FIELDS
        .iter()
        .map(|field| Setting {
            owner: CORE_OWNER.to_string(),
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
            list: false,
            choices: Vec::new(),
        })
        .collect()
}

pub enum Outcome {
    Stay(Option<String>),
    Save(Box<Settings>),
    Interface(Edit),
}

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

const NEXT_TAB: Replay = Replay::new(KeyCode::Char(']'), KeyModifiers::NONE, "]");
const PREVIOUS_TAB: Replay = Replay::new(KeyCode::Char('['), KeyModifiers::NONE, "[");

const INTERFACE_MAX_ROWS: usize = 18;

#[derive(Default)]
pub struct SettingsModal {
    tab: Tab,
    interface: InterfaceTab,
    selected: usize,
    editing: Option<String>,
    draft: Option<Settings>,
    hits: Hits,
}

impl SettingsModal {
    pub fn selected(&self) -> usize {
        self.selected
    }
    pub fn editing(&self) -> bool {
        self.editing.is_some()
    }

    pub fn tab(&self) -> Tab {
        self.tab
    }

    pub fn hits(&self) -> &Hits {
        &self.hits
    }

    pub fn dirty(&self, on_disk: &Settings) -> bool {
        self.draft.as_ref().is_some_and(|draft| draft != on_disk)
    }

    fn rows(&self, registry: &Registry, on_disk: &Settings) -> Vec<Setting> {
        let draft = self.draft.clone().unwrap_or_else(|| on_disk.clone());
        let mut rows = core_rows(&draft);
        rows.extend(registry.settings().iter().cloned());
        rows
    }

    fn draft_mut(&mut self, on_disk: &Settings) -> &mut Settings {
        self.draft.get_or_insert_with(|| on_disk.clone())
    }

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
                    self.interface.disarm();
                    return Outcome::Stay(None);
                }
                KeyCode::Char('[') => {
                    self.tab = self.tab.step(false);
                    self.interface.disarm();
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

    fn current(&self, registry: &Registry, on_disk: &Settings) -> Option<Setting> {
        self.rows(registry, on_disk).get(self.selected).cloned()
    }

    fn activate(&mut self, registry: &mut Registry, on_disk: &Settings) -> Option<String> {
        let setting = self.current(registry, on_disk)?;
        match &setting.value {
            Value::Bool(on) => self.put(registry, on_disk, &setting, Value::Bool(!on)),
            Value::Number(_) => self.step(registry, on_disk, NUMBER_STEP),
            Value::Text(text) => {
                if setting.owner == CORE_OWNER || !setting.choices.is_empty() {
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
            Value::Text(name) if setting.owner == CORE_OWNER => {
                let next = next_backend(name, by >= 0.0);
                self.put(registry, on_disk, &setting, Value::Text(next))
            }
            Value::Text(name) if !setting.choices.is_empty() => {
                let next = next_choice(&setting.choices, name, by >= 0.0);
                self.put(registry, on_disk, &setting, Value::Text(next))
            }
            Value::Text(_) => None,
        }
    }

    fn reset(&mut self, registry: &mut Registry, on_disk: &Settings) -> Option<String> {
        let setting = self.current(registry, on_disk)?;
        if setting.owner == CORE_OWNER {
            // Back to what an untouched file would hold — still a draft, so the
            // file is not written until save.
            let value = setting.default.clone();
            return self.write_core(on_disk, &setting.id, value);
        }
        Some(
            match registry.set_setting(&setting.owner, &setting.id, None) {
                Ok(()) => format!("{}.{} reset to its default", setting.owner, setting.id),
                Err(e) => e,
            },
        )
    }

    fn put(
        &mut self,
        registry: &mut Registry,
        on_disk: &Settings,
        setting: &Setting,
        value: Value,
    ) -> Option<String> {
        if setting.owner == CORE_OWNER {
            return self.write_core(on_disk, &setting.id, value);
        }
        Some(write(registry, setting, value))
    }

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

    fn page(&self) -> usize {
        self.hits.rows.len().max(1)
    }

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
        let footer = self.footer(settings, self.dirty(on_disk), inner_width, chrome);
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

    fn render_interface(
        &mut self,
        frame: &mut Frame,
        area: Rect,
        width: u16,
        files: Files<'_>,
        chrome: Chrome,
    ) {
        // Sized to its content the way the settings half is: the directory line,
        // a line per file and per group header up to the cap, the selected
        // file's details, the footer, and the border. A fixed height left blank
        // rows under a short list.
        let listed = self
            .interface
            .listed(files.rows)
            .clamp(1, INTERFACE_MAX_ROWS);
        let height = u16::try_from(listed + 8)
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
        let pills = [Pill {
            label: "Close",
            primary: true,
            key: Replay::ESC,
        }];
        // The pill's label and its padding, plus a space before it.
        let room = footer_row
            .width
            .saturating_sub(pills[0].label.len() as u16 + 3);
        frame.render_widget(
            Paragraph::new(self.interface.footer(files.rows, usize::from(room), chrome)),
            footer_row,
        );
        let placed = chrome::button_bar(frame, footer_row, &pills, chrome);
        self.hits.buttons.extend(placed);
    }

    #[allow(clippy::wrong_self_convention)]
    fn build_rows<'a>(
        &self,
        settings: &[Setting],
        width: usize,
        chrome: Chrome,
    ) -> (Vec<Line<'a>>, Vec<Row>) {
        // The widest value across every row, so the value column aligns —
        // v1's `value_width` — up to the cap a long text value is cut to.
        let value_width = value_column(settings);
        let id_width = settings
            .iter()
            .map(|setting| setting.id.chars().count())
            .max()
            .unwrap_or(0);

        let mut lines = Vec::new();
        let mut rows = Vec::new();
        let mut section: Option<&str> = None;
        for (index, setting) in settings.iter().enumerate() {
            if section != Some(setting.owner.as_str()) {
                if section.is_some() {
                    lines.push(Line::default());
                    rows.push(None);
                }
                lines.push(Line::from(Span::styled(
                    format!("  {}", setting.owner.to_uppercase()),
                    chrome.section_header(),
                )));
                rows.push(None);
                section = Some(setting.owner.as_str());
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

    fn footer<'a>(
        &self,
        settings: &[Setting],
        dirty: bool,
        width: usize,
        chrome: Chrome,
    ) -> Vec<Line<'a>> {
        let mut footer = Vec::new();
        match settings.get(self.selected) {
            Some(setting) if setting.owner == CORE_OWNER => footer.push(Line::from(vec![
                Span::styled("  settings.toml  ", chrome.muted()),
                Span::styled(setting.id.clone(), chrome.muted()),
                // Said here rather than left to be inferred: the mark is the only
                // thing distinguishing a row that applies now from one that waits,
                // and an unexplained glyph carries none of that.
                Span::styled(format!("   {RESTART_MARK} needs restart"), chrome.muted()),
            ])),
            Some(setting) => {
                let key = format!("  key  {}.{}", setting.owner, setting.id);
                let used = key.chars().count();
                let mut line = vec![Span::styled(key, chrome.muted())];
                // A pick says what it picks among, since the row shows one.
                if !setting.choices.is_empty() {
                    line.push(Span::styled(
                        chrome::truncate(
                            &format!("  {}", setting.choices.join(" · ")),
                            width.saturating_sub(used),
                        ),
                        chrome.muted(),
                    ));
                }
                // The row only has room for a count, so the selected list is
                // spelled out here, cut to the line.
                if let (true, Value::Text(text)) = (setting.list, &setting.value) {
                    line.push(Span::styled(
                        chrome::truncate(
                            &format!("  {}", entries(text).join(", ")),
                            width.saturating_sub(used),
                        ),
                        chrome.muted(),
                    ));
                }
                footer.push(Line::from(line))
            }
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

fn write(registry: &mut Registry, setting: &Setting, value: Value) -> String {
    let key = format!("{}.{}", setting.owner, setting.id);
    let shown = display(&value);
    match registry.set_setting(&setting.owner, &setting.id, Some(value)) {
        Ok(()) => format!("{key} = {shown}"),
        Err(e) => e,
    }
}

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

fn value_text(value: &Value) -> String {
    match value {
        Value::Number(_) => format!("‹ {} ›", display(value)),
        _ => display(value),
    }
}

fn next_choice(choices: &[String], current: &str, forward: bool) -> String {
    let count = choices.len();
    let next = match choices.iter().position(|choice| choice == current) {
        Some(at) if forward => (at + 1) % count,
        Some(at) => (at + count - 1) % count,
        None => 0,
    };
    choices[next].clone()
}

fn shown(setting: &Setting) -> String {
    if !setting.choices.is_empty() {
        return format!("‹ {} ›", display(&setting.value));
    }
    match (setting.list, &setting.value) {
        (true, Value::Text(text)) => match entries(text).len() {
            0 => "none".to_string(),
            1 => "1 entry".to_string(),
            n => format!("{n} entries"),
        },
        (_, value) => value_text(value),
    }
}

fn entries(text: &str) -> Vec<String> {
    text.split(';')
        .filter(|entry| !entry.is_empty())
        .map(|entry| {
            let mut bytes = Vec::with_capacity(entry.len());
            let mut rest = entry.as_bytes();
            while let Some((&byte, tail)) = rest.split_first() {
                let hex = tail
                    .get(..2)
                    .and_then(|pair| std::str::from_utf8(pair).ok())
                    .and_then(|pair| u8::from_str_radix(pair, 16).ok());
                match (byte, hex) {
                    (b'%', Some(decoded)) => {
                        bytes.push(decoded);
                        rest = &tail[2..];
                    }
                    _ => {
                        bytes.push(byte);
                        rest = tail;
                    }
                }
            }
            String::from_utf8_lossy(&bytes)
                .chars()
                .map(|c| if c.is_control() { '/' } else { c })
                .collect()
        })
        .collect()
}

fn desired_width(settings: &[Setting]) -> u16 {
    let longest = |f: fn(&Setting) -> usize| settings.iter().map(f).max().unwrap_or(0);
    let id = longest(|s| s.id.chars().count());
    let value = value_column(settings);
    // The restart mark rides in the description of a restart-only row, so the
    // column has to allow for it or the mark is what pushes a description over
    // the edge.
    let description = longest(|s| s.description.chars().count()) + 2;
    // 2 frame + 2 pointer + id + 1 + description + 1 gap + value.
    u16::try_from(2 + 2 + id + 1 + description + 1 + value).unwrap_or(MODAL_WIDTH_MAX)
}

fn value_column(settings: &[Setting]) -> usize {
    settings
        .iter()
        .map(|setting| shown(setting).chars().count())
        .max()
        .unwrap_or(0)
        .clamp(3, VALUE_WIDTH_MAX)
}

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
        // A block caret, so an open edit is unmistakably where the keys go. The
        // tail is what shows when the buffer outgrows the column: that is where
        // the caret is, and an edit is the one place the whole value is reached.
        Some(buffer) => {
            let text: Vec<char> = format!("{buffer}█").chars().collect();
            if text.len() > value_width {
                let tail = &text[text.len() - (value_width - 1)..];
                std::iter::once('…').chain(tail.iter().copied()).collect()
            } else {
                text.into_iter().collect()
            }
        }
        None => chrome::truncate(&shown(setting), value_width),
    };
    let id = chrome::pad(&setting.id, id_width);
    let block = 2 + id_width + 1;
    let right = value_width;
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

    fn setting(mod_name: &str, id: &str, default: Value) -> Setting {
        Setting {
            owner: mod_name.to_string(),
            id: id.to_string(),
            description: format!("what {id} does"),
            value: default.clone(),
            default,
            list: false,
            choices: Vec::new(),
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

    #[test]
    fn a_list_value_is_counted_and_unescaped() {
        assert_eq!(
            entries("repo%3Ah%01a%00b;;100%;%zz"),
            ["repo:h/a/b", "100%", "%zz"]
        );
        let list = |value: &str| Setting {
            list: true,
            choices: Vec::new(),
            ..setting("sessions", "folded_repos", Value::Text(value.into()))
        };
        assert_eq!(shown(&list("")), "none");
        assert_eq!(shown(&list("a")), "1 entry");
        assert_eq!(shown(&list("a;b")), "2 entries");
    }

    #[test]
    fn an_edit_past_the_value_column_keeps_its_caret_in_view() {
        let palette = crate::theme::ThemePreset::Default.palette();
        let row = setting("a", "label", Value::Text(String::new()));
        let buffer = "x".repeat(40) + "end";
        let line = setting_line(
            &row,
            true,
            Some(&buffer),
            60,
            5,
            VALUE_WIDTH_MAX,
            Chrome::new(&palette),
        );
        let text: String = line
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect();
        assert_eq!(text.chars().count(), 60, "{text:?}");
        assert!(text.ends_with("…xxxxxxxxxxxend█"), "{text:?}");
    }

    fn press(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, crossterm::event::KeyModifiers::NONE)
    }

    fn ctrl(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::CONTROL)
    }

    fn message(outcome: Outcome) -> Option<String> {
        match outcome {
            Outcome::Stay(message) => message,
            Outcome::Save(_) => Some("saved".to_string()),
            Outcome::Interface(edit) => Some(format!("interface {edit:?}")),
        }
    }

    fn on_mod_row(nth: usize) -> SettingsModal {
        SettingsModal {
            selected: CORE_FIELDS.len() + nth,
            ..SettingsModal::default()
        }
    }

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
        let mut modal = on_mod_row(0);
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
        let mut modal = on_mod_row(0);
        message(modal.on_key(&press(KeyCode::Char(' ')), &mut registry, &on_disk, &[]));
        assert_eq!(registry.settings()[0].value, Value::Bool(true));
        message(modal.on_key(&press(KeyCode::Char('d')), &mut registry, &on_disk, &[]));
        assert_eq!(registry.settings()[0].value, Value::Bool(false));
    }

    // ── the core half ──────────────────────────────────────────────────────

    fn core_row(id: &str) -> usize {
        CORE_FIELDS
            .iter()
            .position(|field| field.id == id)
            .unwrap_or_else(|| panic!("no core row named {id}"))
    }

    #[test]
    fn the_core_settings_are_listed_beside_what_mods_declared() {
        let on_disk = Settings::default();
        let registry = registry_with(vec![setting("a", "flag", Value::Bool(false))]);
        let modal = SettingsModal::default();
        let rows = modal.rows(&registry, &on_disk);
        assert_eq!(rows.len(), CORE_FIELDS.len() + 1);
        assert!(rows.iter().any(|row| row.id == "features.shell_pane"));
        assert!(
            rows.last().is_some_and(|row| row.owner == "a"),
            "a mod's rows follow the core ones"
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
        let soft = &rows[core_row("features.shell_pane")];
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
        let mut modal = on_core_row("features.shell_pane");

        assert!(!modal.dirty(&on_disk));
        let reported =
            message(modal.on_key(&press(KeyCode::Char(' ')), &mut registry, &on_disk, &[]));
        assert!(
            reported.unwrap_or_default().contains("unsaved"),
            "the row says the change has not landed yet"
        );
        assert!(modal.dirty(&on_disk));
        assert!(on_disk.features.shell_pane, "and the file has not moved");

        // Saving hands the whole draft over; the file and the live half are the
        // loop's business, not the modal's.
        match modal.on_key(&ctrl(KeyCode::Char('s')), &mut registry, &on_disk, &[]) {
            Outcome::Save(draft) => assert!(!draft.features.shell_pane),
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
        let mut modal = on_core_row("git_poll_secs");
        message(modal.on_key(&press(KeyCode::Right), &mut registry, &on_disk, &[]));
        message(modal.on_key(&press(KeyCode::Char('d')), &mut registry, &on_disk, &[]));
        assert_eq!(
            modal.draft.as_ref().map(|d| d.git_poll_secs),
            Some(Settings::default().git_poll_secs)
        );
        assert!(
            !modal.dirty(&on_disk),
            "back to what the file holds is not a change"
        );
    }

    #[test]
    fn an_empty_registry_renders_rather_than_panicking() {
        let registry = registry_with(Vec::new());
        let mut modal = SettingsModal::default();
        let palette = crate::theme::ThemePreset::Default.palette();
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

    fn painted(modal: &mut SettingsModal, registry: &Registry, on_disk: &Settings) -> String {
        let palette = crate::theme::ThemePreset::Default.palette();
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
        let mut modal = on_mod_row(0);
        message(modal.on_key(&press(KeyCode::Enter), &mut registry, &on_disk, &[]));
        assert!(modal.editing(), "Enter on a text value starts an edit");

        assert!(
            painted(&mut modal, &registry, &on_disk).contains('\u{2588}'),
            "an open edit must show where the keys go"
        );
    }

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
        {
            let id = "features.shell_pane";
            assert!(!marker(id), "{id} is read every frame, so it applies live");
        }
    }
}
