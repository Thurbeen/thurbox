//! System modals: help, settings and the theme picker.
//!
//! These three are not panes and not plugins. A pane shows
//! your work and belongs in the layout; these are **system chrome** — they are
//! about thurbox itself, they overlay, they are modal, and they are the same in
//! every install. Making them plugins bought no extensibility (nobody replaces
//! the help pane wholesale) while costing the layout, the focus ring, and a
//! contribution mechanism nobody could reach.
//!
//! So the kernel draws them, in Rust, above the arrangement and above plugin
//! floats — the same place the perf HUD and the error panel already draw. The
//! consequences are the reasons:
//!
//! - **Not in the layout.** No slot, so a modal can neither shrink a pane nor
//!   be shrunk by one.
//! - **Not in the focus ring.** `focusable()` never contains one, so `Tab`
//!   keeps its short ring.
//! - **Captures input.** While open, keys reach the modal and no plugin is
//!   offered them. `Esc` closes; the opening chord toggles.
//! - **One at a time.** Opening one closes another, so there is no z-order
//!   question because there is no stack.
//!
//! Modularity lives one level down: plugins contribute **data** —
//! `Registry::bindings` for help, `Registry::settings` for settings — and the
//! kernel renders it. The theme picker deliberately takes no contribution;
//! there is nothing a plugin could add to a list of palettes.

pub mod chrome;
pub mod help;
pub mod interface;
pub mod palette;
pub mod settings;
pub mod theme;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::{Position, Rect};
use ratatui::Frame;

use self::chrome::{Chrome, Hits, Hover};
use super::registry::{binding_from, Binding, Pill, Registry};
use super::theme::Themes;
use crate::storage::Database;

/// The declaring "plugin" of a modal's chord.
///
/// The bindings go through the ordinary registry so the modals appear in help
/// and can be rebound like anything else — but no Lua plugin owns them, so the
/// loop routes this name to the modal layer instead of to `host.on_action`.
pub const OWNER: &str = "kernel";

/// Which modal is open, and what its chords are called.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModalKind {
    Help,
    Settings,
    Theme,
    /// The command palette — every action in the registry, one query away.
    Palette,
}

impl ModalKind {
    /// The registry action that toggles this modal.
    ///
    /// Named as the plugins that used to own these panes named them, because
    /// the footer pills and v1's own key table both reach them by action id —
    /// moving the code should not move the vocabulary.
    pub fn action(self) -> &'static str {
        match self {
            ModalKind::Help => "help.open",
            ModalKind::Settings => "settings.open",
            ModalKind::Theme => "themes.open",
            ModalKind::Palette => "palette.open",
        }
    }

    pub fn from_action(action: &str) -> Option<Self> {
        [
            ModalKind::Help,
            ModalKind::Settings,
            ModalKind::Theme,
            ModalKind::Palette,
        ]
        .into_iter()
        .find(|kind| kind.action() == action)
    }
}

/// Every chord that opens a system modal, as a registry declaration.
///
/// Collected alongside the plugins' own declarations, so help lists them, the
/// conflict detector sees them, and the editor can rebind them. Each keeps v1's
/// pair — the `Ctrl` chord plus an F-key alternate, which exists because a
/// focused terminal passes a bare `Ctrl+<letter>` through to the agent.
pub fn bindings() -> Vec<Binding> {
    [
        ("f1", ModalKind::Help, "open keybindings help"),
        ("ctrl+g", ModalKind::Help, "open keybindings help"),
        ("f6", ModalKind::Settings, "open settings"),
        ("ctrl+,", ModalKind::Settings, "open settings"),
        ("f4", ModalKind::Theme, "open theme picker"),
        ("ctrl+y", ModalKind::Theme, "open theme picker"),
        // Taken deliberately from the list of chords held for v1's panes
        // (`tests/v2_keymap.rs`): the automations pane it was held for is one
        // query away in the palette once it returns. No F-key alternate — every
        // one is bound or held, and this chord is never deferred to the agent
        // (readline's previous-history is also the up arrow).
        ("ctrl+p", ModalKind::Palette, "open the command palette"),
    ]
    .into_iter()
    .map(|(chord, kind, description)| {
        binding_from(
            OWNER,
            chord,
            kind.action(),
            description,
            Some("global"),
            // Never deferred to the agent: v1 leaves the readline chords to the
            // pty, and none of these three is one of them.
            false,
            Some("Panels"),
        )
    })
    .collect()
}

/// The action-band entries the system modals contribute.
///
/// The kernel declares these the same way a plugin declares its own: the band
/// resolves the chord from the registry, so an entry cannot advertise a chord
/// the keyboard does not honour, and rebinding one moves its entry with it.
///
/// v1's footer also carries `Quit`, and this list deliberately does not: quit is
/// not a declared action — `ctrl+q` is handled before the registry is consulted,
/// so it has no binding for an entry to resolve against, and giving it one would
/// make it rebindable, which is exactly what the reserved chord exists to
/// prevent. The band carries that entry itself instead (`bands::quit_entry`).
pub fn pills() -> Vec<Pill> {
    [
        (ModalKind::Help, "Help", 90),
        (ModalKind::Theme, "Theme", 70),
        (ModalKind::Settings, "Settings", 60),
    ]
    .into_iter()
    .map(|(kind, label, priority)| Pill {
        plugin: OWNER.to_string(),
        action: kind.action().to_string(),
        label: label.to_string(),
        priority,
    })
    .collect()
}

/// Everything a modal may write to. Assembled per keystroke by the loop, which
/// owns all three.
pub struct World<'a> {
    pub registry: &'a mut Registry,
    pub themes: &'a mut Themes,
    /// The settings **as the file has them**, which the settings modal shows
    /// beside what plugins declared and seeds its draft from.
    ///
    /// Deliberately not the settings in force — the panel edits the file, and the
    /// two documents differ by exactly the restart-only half: see
    /// [`crate::kernel::config::Config::on_disk`].
    pub settings_on_disk: &'a crate::session::settings::Settings,
    /// Where a saved draft is left for the loop: it has to write the file (off
    /// the render path) and take the live half into force, neither of which is a
    /// modal's business. `None` on the way in, `Some` when a save was asked for.
    pub save_settings: &'a mut Option<crate::session::settings::Settings>,
    /// Every file the interface is made of, as of the last painted frame — what
    /// the settings modal's Interface tab lists.
    pub inventory: &'a [crate::kernel::inventory::Row],
    /// Where a restore or a remove is left for the loop, which owns the one code
    /// path that writes an interface file and asks for the reload. Same shape as
    /// `save_settings`, for the same reason.
    pub interface_edit: &'a mut Option<interface::Edit>,
    /// Where the palette's choice is left for the loop, which owns the one path
    /// that runs an action. Same shape as `save_settings`, for the same reason
    /// — and the modal has closed itself by the time the loop reads it.
    pub run_action: &'a mut Option<palette::Dispatch>,
    /// Where the theme choice is persisted. `None` when the database could not
    /// be opened — the choice then applies for this run only.
    pub db: Option<&'a Database>,
}

enum Open {
    Help(help::HelpModal),
    Settings(settings::SettingsModal),
    Theme(theme::ThemeModal),
    Palette(palette::PaletteModal),
}

/// The modal layer: at most one modal, above everything.
#[derive(Default)]
pub struct Modals {
    open: Option<Open>,
    /// Where the pointer is, handed to the chrome each frame so a pill or a row
    /// under it lights up.
    pointer: Option<Position>,
    /// What that pointer last resolved to. Kept so a move *within* one row or
    /// pill is known to change nothing — see [`Modals::on_hover`].
    hovered: Hover,
}

impl Modals {
    pub fn is_open(&self) -> bool {
        self.open.is_some()
    }

    pub fn kind(&self) -> Option<ModalKind> {
        match &self.open {
            Some(Open::Help(_)) => Some(ModalKind::Help),
            Some(Open::Settings(_)) => Some(ModalKind::Settings),
            Some(Open::Theme(_)) => Some(ModalKind::Theme),
            Some(Open::Palette(_)) => Some(ModalKind::Palette),
            None => None,
        }
    }

    /// The palette's query as typed so far, for the loop's tests and for a
    /// caller that wants to know whether typing is going anywhere.
    pub fn palette_query(&self) -> Option<&str> {
        match &self.open {
            Some(Open::Palette(modal)) => Some(modal.query()),
            _ => None,
        }
    }

    /// Whether the open modal is swallowing *everything*, the kernel's reserved
    /// chords included.
    ///
    /// True only while help is capturing a chord: binding
    /// `ctrl+q` has to be possible, and it is possible precisely because the
    /// kernel knows it is capturing and for exactly one keystroke. A plugin
    /// could never be trusted with that.
    pub fn captures_everything(&self) -> bool {
        matches!(&self.open, Some(Open::Help(help)) if help.capturing())
    }

    /// Open `kind`, or close it when it is already the open one.
    ///
    /// Opening replaces whatever was open, which is what "one at a time" means:
    /// there is no stack to unwind.
    pub fn toggle(&mut self, kind: ModalKind) {
        // The resolved hover indexes the hitboxes of the modal that recorded
        // them, so it cannot outlive that modal: a row index means nothing to
        // its successor.
        self.hovered = Hover::Nothing;
        if self.kind() == Some(kind) {
            self.open = None;
            return;
        }
        self.open = Some(match kind {
            ModalKind::Help => Open::Help(help::HelpModal::default()),
            ModalKind::Settings => Open::Settings(settings::SettingsModal::default()),
            ModalKind::Theme => Open::Theme(theme::ThemeModal::default()),
            ModalKind::Palette => Open::Palette(palette::PaletteModal::default()),
        });
    }

    pub fn close(&mut self) {
        self.open = None;
        self.hovered = Hover::Nothing;
    }

    /// Undo anything the open modal applied on the way to being closed without a
    /// choice.
    ///
    /// Only the theme picker has any: it previews on every move, so leaving it
    /// has to restore what was active when it opened. Help and settings apply
    /// nothing until asked, so they have nothing to take back.
    pub fn abandon(&mut self, world: &mut World<'_>) {
        if let Some(Open::Theme(modal)) = self.open.as_mut() {
            modal.revert(world.themes);
        }
    }

    /// Take a key while a modal is open. Returns a message to report.
    ///
    /// `chord` is the canonical spelling of the same keystroke — the help
    /// editor stores what the registry will later match, rather than what this
    /// particular terminal happened to send.
    pub fn on_key(&mut self, key: &KeyEvent, chord: &str, world: &mut World<'_>) -> Option<String> {
        // `Esc` closes, but only once the modal's own sub-mode has had it: a
        // filter or a text edit is what `Esc` cancels first, exactly as v1's
        // pickers behave.
        if key.code == KeyCode::Esc && !self.sub_mode_open() {
            // A picker that previews live has something to undo: closing without
            // choosing must put the palette back, since every cursor move
            // changed it.
            self.abandon(world);
            self.close();
            return None;
        }
        match self.open.as_mut()? {
            Open::Help(modal) => modal.on_key(key, chord, world.registry),
            Open::Settings(modal) => {
                match modal.on_key(key, world.registry, world.settings_on_disk, world.inventory) {
                    settings::Outcome::Stay(message) => message,
                    // Left for the loop, and the modal stays open — v1's panel does
                    // too, so a second change does not mean re-opening it.
                    settings::Outcome::Save(draft) => {
                        *world.save_settings = Some(*draft);
                        Some("settings saved".to_string())
                    }
                    // Likewise left for the loop: the modal stays open so the row it
                    // just changed can be seen changing.
                    settings::Outcome::Interface(edit) => {
                        *world.interface_edit = Some(edit);
                        None
                    }
                }
            }
            Open::Theme(modal) => match modal.on_key(key, world.themes, world.db) {
                theme::Outcome::Stay(message) => message,
                // Closed on apply, so the palette that was just chosen is
                // visible rather than hidden behind the picker.
                theme::Outcome::Applied(message) => {
                    self.close();
                    Some(message)
                }
            },
            // Closed BEFORE the loop runs the choice, so the action sees the
            // focus state a key press would have — and cannot re-enter the modal
            // it was chosen from.
            Open::Palette(modal) => match modal.on_key(key, world.registry) {
                palette::Outcome::Stay(message) => message,
                palette::Outcome::Run(dispatch) => {
                    self.close();
                    *world.run_action = Some(dispatch);
                    None
                }
            },
        }
    }

    /// Whether the open modal has a sub-mode that owns `Esc` this keystroke.
    fn sub_mode_open(&self) -> bool {
        match &self.open {
            Some(Open::Help(modal)) => modal.capturing(),
            Some(Open::Settings(modal)) => modal.editing(),
            Some(Open::Theme(modal)) => modal.filtering(),
            // Typing is the palette's only mode, so `Esc` always closes it.
            Some(Open::Palette(_)) => false,
            None => false,
        }
    }

    /// Take a click while a modal is open.
    ///
    /// Every click is swallowed — the mouse half of capturing input — so a
    /// press that misses a row cannot reach the pane the modal covers.
    pub fn on_click(&mut self, x: u16, y: u16, world: &mut World<'_>) -> Option<String> {
        // A footer pill re-enters through `on_key`, the very handler the
        // keyboard uses (v1's `try_modal_button_click`, and the same rule as
        // `ClickVerb::Key`). So ` Select ` cannot come to mean something
        // `Enter` does not — including the layer's own `Esc`, which is how
        // ` Close ` closes.
        if let Some(replay) = self.hits().and_then(|hits| hits.button_at(x, y)) {
            let key = KeyEvent::new(replay.code, replay.modifiers);
            return self.on_key(&key, replay.chord, world);
        }
        match self.open.as_mut()? {
            Open::Help(modal) => {
                modal.on_click(x, y);
                None
            }
            Open::Settings(modal) => modal.on_click(x, y, world.registry, world.settings_on_disk),
            Open::Theme(modal) => {
                // A click is the same act as moving the cursor there, so it
                // previews too — `ClickVerb::Key`'s rule, one level up.
                modal.on_click(x, y);
                modal.preview_selected(world.themes);
                None
            }
            Open::Palette(modal) => {
                modal.on_click(x, y);
                None
            }
        }
    }

    /// Track the pointer while a modal is open. `true` when the frame must be
    /// repainted.
    ///
    /// Resolved against last frame's hitboxes rather than compared as a raw
    /// cell, because motion reporting delivers one event per cell crossed: a
    /// pointer travelling the width of a row would otherwise repaint the whole
    /// interface a few dozen times to arrive at the same picture. Same rule as
    /// `App::hover`, which compares the identity under the pointer.
    pub fn on_hover(&mut self, x: u16, y: u16) -> bool {
        self.pointer = Some(Position::new(x, y));
        let hovered = self
            .hits()
            .map_or(Hover::Nothing, |hits| hits.hover_at(x, y));
        if hovered == self.hovered {
            return false;
        }
        self.hovered = hovered;
        true
    }

    /// Where the open modal wants the terminal's caret, if anywhere.
    ///
    /// A modal captures input, so while one is up the caret belongs to *it* or
    /// to nobody. It must not be left wherever the pane underneath put it: a
    /// frame either shows the cursor at a position or hides it, so a pane that
    /// claimed it keeps a caret blinking behind the modal — which reads as the
    /// screen refreshing wrongly rather than as a misplaced cursor.
    ///
    /// `None` for all three, and correctly so: the two that take typing paint
    /// their own caret as a **glyph** (a block appended to the text) rather than
    /// asking for the terminal's cursor, and help takes a single chord with no
    /// text to put a caret in. A painted caret survives being drawn over, needs
    /// no arbitration with the panes, and can be themed.
    ///
    /// The seam is kept because the *decision* has to live somewhere: something
    /// must claim the caret while a modal is up, even if the claim is "nobody".
    /// A modal that later wants the real cursor returns it here.
    pub fn caret(&self) -> Option<Position> {
        None
    }

    /// The open modal's hitboxes, as its last frame recorded them.
    fn hits(&self) -> Option<&Hits> {
        Some(match self.open.as_ref()? {
            Open::Help(modal) => modal.hits(),
            Open::Settings(modal) => modal.hits(),
            Open::Theme(modal) => modal.hits(),
            Open::Palette(modal) => modal.hits(),
        })
    }

    /// Draw the open modal over `area`. Nothing is drawn when none is open.
    pub fn render(
        &mut self,
        frame: &mut Frame,
        area: Rect,
        registry: &Registry,
        themes: &Themes,
        on_disk: &crate::session::settings::Settings,
        files: interface::Files<'_>,
    ) {
        let palette = &themes.active().palette;
        // The dim is confined to the area this was handed, which excludes the
        // chrome bands — see `Chrome::within`.
        let chrome = Chrome::new(palette).at(self.pointer).within(area);
        match self.open.as_mut() {
            Some(Open::Help(modal)) => modal.render(frame, area, registry, chrome),
            Some(Open::Settings(modal)) => {
                modal.render(frame, area, registry, on_disk, files, chrome)
            }
            Some(Open::Theme(modal)) => modal.render(frame, area, themes, chrome),
            Some(Open::Palette(modal)) => modal.render(frame, area, registry, chrome),
            None => return,
        }
        // One place for all three: the row band is painted over cells the modal
        // has already drawn, so it can only run once the modal is done.
        if let Some(hits) = self.hits() {
            chrome::paint_row_hover(frame, hits, chrome);
        }
    }
}

/// The chords the loop must keep working even while a modal is open.
///
/// A modal that could trap you is a bug, so quit, reload and the perf HUD stay
/// reachable. Everything else belongs to the modal — including `Tab`, which the
/// settings editor uses to move between rows and which would otherwise cycle a
/// focus ring nobody can see.
pub fn escapes(key: &KeyEvent) -> bool {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    matches!(key.code, KeyCode::Char('q') if ctrl)
        || matches!(key.code, KeyCode::F(10) | KeyCode::F(12))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kernel::registry::RESERVED;

    fn press(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    /// A world over default settings, with a save slot nothing reads: these tests
    /// are about the layer, not about what a saved draft does.
    fn world<'a>(
        registry: &'a mut Registry,
        themes: &'a mut Themes,
        settings_on_disk: &'a crate::session::settings::Settings,
        saved: &'a mut Option<crate::session::settings::Settings>,
    ) -> World<'a> {
        World {
            registry,
            themes,
            settings_on_disk,
            save_settings: saved,
            inventory: &[],
            interface_edit: Box::leak(Box::new(None)),
            run_action: Box::leak(Box::new(None)),
            db: None,
        }
    }

    #[test]
    fn opening_one_modal_closes_another() {
        let mut modals = Modals::default();
        modals.toggle(ModalKind::Help);
        assert_eq!(modals.kind(), Some(ModalKind::Help));
        modals.toggle(ModalKind::Theme);
        assert_eq!(modals.kind(), Some(ModalKind::Theme));
        // The opening chord toggles.
        modals.toggle(ModalKind::Theme);
        assert!(!modals.is_open());
    }

    #[test]
    fn esc_closes_the_open_modal() {
        let mut registry = Registry::default();
        let mut themes = Themes::load(None);
        let settings = crate::session::settings::Settings::default();
        let mut saved = None;
        let mut modals = Modals::default();
        modals.toggle(ModalKind::Help);
        modals.on_key(
            &press(KeyCode::Esc),
            "esc",
            &mut world(&mut registry, &mut themes, &settings, &mut saved),
        );
        assert!(!modals.is_open());
    }

    #[test]
    fn a_capture_swallows_even_the_chords_the_kernel_reserves() {
        // The reason the modal layer is the kernel's: while capturing, ctrl+q
        // is data. Nothing else in the product may take that chord.
        let mut registry = Registry::default();
        let mut themes = Themes::load(None);
        let settings = crate::session::settings::Settings::default();
        let mut saved = None;
        let mut modals = Modals::default();
        modals.toggle(ModalKind::Help);
        assert!(!modals.captures_everything());
        modals.on_key(
            &press(KeyCode::Char('r')),
            "r",
            &mut world(&mut registry, &mut themes, &settings, &mut saved),
        );
        // With nothing declared there is no action to capture onto, so the
        // registry drives this: declare one and the capture arms.
        registry.declare(bindings(), Vec::new());
        modals.on_key(
            &press(KeyCode::Char('r')),
            "r",
            &mut world(&mut registry, &mut themes, &settings, &mut saved),
        );
        assert!(modals.captures_everything());
    }

    #[test]
    fn every_modal_chord_v1_binds_is_declared() {
        let declared: Vec<String> = bindings().into_iter().map(|b| b.chord).collect();
        for chord in ["f1", "ctrl+g", "f4", "ctrl+y", "f6", "ctrl+,", "ctrl+p"] {
            assert!(
                declared.iter().any(|declared| declared == chord),
                "{chord} opens nothing"
            );
        }
        // A modal chord must not shadow the escape route.
        for binding in bindings() {
            assert!(!RESERVED.contains(&binding.chord.as_str()), "{binding:?}");
        }
    }

    #[test]
    fn the_escape_route_survives_an_open_modal() {
        assert!(escapes(&KeyEvent::new(
            KeyCode::Char('q'),
            KeyModifiers::CONTROL
        )));
        assert!(escapes(&press(KeyCode::F(10))));
        assert!(escapes(&press(KeyCode::F(12))));
        // Tab belongs to the modal: settings moves between rows with it.
        assert!(!escapes(&press(KeyCode::Tab)));
        assert!(!escapes(&press(KeyCode::Char('q'))));
    }
}
