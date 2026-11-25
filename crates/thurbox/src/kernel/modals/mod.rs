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

pub const OWNER: &str = "kernel";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModalKind {
    Help,
    Settings,
    Theme,
    Palette,
}

impl ModalKind {
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

pub fn bindings() -> Vec<Binding> {
    [
        ("f1", ModalKind::Help, "open keybindings help"),
        ("ctrl+g", ModalKind::Help, "open keybindings help"),
        ("f6", ModalKind::Settings, "open settings"),
        ("ctrl+,", ModalKind::Settings, "open settings"),
        ("f4", ModalKind::Theme, "open theme picker"),
        ("ctrl+y", ModalKind::Theme, "open theme picker"),
        // Taken deliberately from the list of chords held for v1's panes
        // (`tests/keymap.rs`). No F-key alternate — every
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

pub fn pills() -> Vec<Pill> {
    [
        (ModalKind::Help, "Help", 90),
        (ModalKind::Theme, "Theme", 70),
        (ModalKind::Settings, "Settings", 60),
    ]
    .into_iter()
    .map(|(kind, label, priority)| Pill {
        owner: OWNER.to_string(),
        action: kind.action().to_string(),
        label: label.to_string(),
        priority,
    })
    .collect()
}

pub struct World<'a> {
    pub registry: &'a mut Registry,
    pub themes: &'a mut Themes,
    pub settings_on_disk: &'a crate::session::settings::Settings,
    pub save_settings: &'a mut Option<crate::session::settings::Settings>,
    pub inventory: &'a [crate::kernel::inventory::Row],
    pub interface_edit: &'a mut Option<interface::Edit>,
    pub run_action: &'a mut Option<palette::Dispatch>,
}

enum Open {
    Help(help::HelpModal),
    // Boxed: it holds a whole `Settings`, several times the next variant.
    Settings(Box<settings::SettingsModal>),
    Theme(theme::ThemeModal),
    Palette(palette::PaletteModal),
}

#[derive(Default)]
pub struct Modals {
    open: Option<Open>,
    pointer: Option<Position>,
    hovered: Hover,
}

impl Modals {
    pub fn selection(&self) -> Option<usize> {
        match &self.open {
            Some(Open::Help(modal)) => Some(modal.selected()),
            Some(Open::Settings(modal)) => Some(modal.selected()),
            Some(Open::Theme(modal)) => Some(modal.selected()),
            Some(Open::Palette(modal)) => Some(modal.selected()),
            None => None,
        }
    }
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

    pub fn palette_query(&self) -> Option<&str> {
        match &self.open {
            Some(Open::Palette(modal)) => Some(modal.query()),
            _ => None,
        }
    }

    pub fn captures_everything(&self) -> bool {
        matches!(&self.open, Some(Open::Help(help)) if help.capturing())
    }

    pub fn on_drag(&mut self, y: u16) -> bool {
        match &mut self.open {
            Some(Open::Help(help)) => help.on_drag(y),
            _ => false,
        }
    }

    pub fn on_wheel(&mut self, up: bool) -> bool {
        match &mut self.open {
            Some(Open::Help(help)) => {
                help.on_wheel(up);
                true
            }
            _ => false,
        }
    }

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
            ModalKind::Settings => Open::Settings(Box::default()),
            ModalKind::Theme => Open::Theme(theme::ThemeModal::default()),
            ModalKind::Palette => Open::Palette(palette::PaletteModal::default()),
        });
    }

    pub fn close(&mut self) {
        self.open = None;
        self.hovered = Hover::Nothing;
    }

    pub fn abandon(&mut self, world: &mut World<'_>) {
        if let Some(Open::Theme(modal)) = self.open.as_mut() {
            modal.revert(world.themes);
        }
    }

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
            Open::Theme(modal) => match modal.on_key(key, world.themes) {
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

    pub fn caret(&self) -> Option<Position> {
        None
    }

    fn hits(&self) -> Option<&Hits> {
        Some(match self.open.as_ref()? {
            Open::Help(modal) => modal.hits(),
            Open::Settings(modal) => modal.hits(),
            Open::Theme(modal) => modal.hits(),
            Open::Palette(modal) => modal.hits(),
        })
    }

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
