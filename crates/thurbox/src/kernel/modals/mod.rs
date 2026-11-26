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
        if key.code == KeyCode::Esc && !self.sub_mode_open() {
            self.abandon(world);
            self.close();
            return None;
        }
        match self.open.as_mut()? {
            Open::Help(modal) => modal.on_key(key, chord, world.registry),
            Open::Settings(modal) => {
                match modal.on_key(key, world.registry, world.settings_on_disk, world.inventory) {
                    settings::Outcome::Stay(message) => message,
                    settings::Outcome::Save(draft) => {
                        *world.save_settings = Some(*draft);
                        Some("settings saved".to_string())
                    }
                    settings::Outcome::Interface(edit) => {
                        *world.interface_edit = Some(edit);
                        None
                    }
                }
            }
            Open::Theme(modal) => match modal.on_key(key, world.themes) {
                theme::Outcome::Stay(message) => message,
                theme::Outcome::Applied(message) => {
                    self.close();
                    Some(message)
                }
            },
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
            Some(Open::Palette(_)) => false,
            None => false,
        }
    }

    pub fn on_click(&mut self, x: u16, y: u16, world: &mut World<'_>) -> Option<String> {
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
        assert!(!escapes(&press(KeyCode::Tab)));
        assert!(!escapes(&press(KeyCode::Char('q'))));
    }
}
