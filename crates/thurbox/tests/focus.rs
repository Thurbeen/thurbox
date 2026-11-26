use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use thurbox::kernel::host::LuaHost;
use thurbox::kernel::modals::chrome::Chrome;
use thurbox::kernel::modals::interface::Files;
use thurbox::kernel::modals::settings::{SettingsModal, Tab};
use thurbox::kernel::modals::{ModalKind, Modals};
use thurbox::kernel::registry::Registry;

fn host() -> LuaHost {
    let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../ui");
    let host = LuaHost::new(dir);
    assert!(host.error.is_none(), "{:?}", host.error);
    host
}

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

#[test]
fn the_file_list_is_reached_through_settings_and_needs_no_chord_of_its_own() {
    let mut modal = SettingsModal::default();
    assert_eq!(modal.tab(), Tab::Settings);

    modal.on_key(
        &key(KeyCode::Char(']')),
        &mut Registry::default(),
        &Default::default(),
        &[],
    );
    assert_eq!(modal.tab(), Tab::Interface);

    modal.on_key(
        &key(KeyCode::Char('[')),
        &mut Registry::default(),
        &Default::default(),
        &[],
    );
    assert_eq!(modal.tab(), Tab::Settings);
}

#[test]
fn clicking_a_tab_heading_switches_to_it() {
    let palette = thurbox::theme::ThemePreset::Default.palette();
    let mut modal = SettingsModal::default();
    let mut terminal =
        ratatui::Terminal::new(ratatui::backend::TestBackend::new(90, 26)).expect("terminal");
    terminal
        .draw(|frame| {
            modal.render(
                frame,
                frame.area(),
                &Registry::default(),
                &Default::default(),
                Files {
                    rows: &[],
                    dir: "ui",
                },
                Chrome::new(&palette),
            );
        })
        .expect("draw");

    let buttons = modal.hits().buttons.len();
    assert!(
        buttons >= 2,
        "the tab headings are not clickable: {buttons}"
    );

    let (rect, replay) = modal.hits().buttons[1];
    let key = KeyEvent::new(replay.code, replay.modifiers);
    modal.on_key(&key, &mut Registry::default(), &Default::default(), &[]);
    assert_eq!(modal.tab(), Tab::Interface, "clicked at {rect:?}");
}

#[test]
fn the_tab_keys_are_offered_where_they_can_be_seen() {
    let palette = thurbox::theme::ThemePreset::Default.palette();
    for tab in [Tab::Settings, Tab::Interface] {
        let mut modal = SettingsModal::default();
        if tab == Tab::Interface {
            modal.on_key(
                &key(KeyCode::Char(']')),
                &mut Registry::default(),
                &Default::default(),
                &[],
            );
        }
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(90, 26)).expect("terminal");
        terminal
            .draw(|frame| {
                modal.render(
                    frame,
                    frame.area(),
                    &Registry::default(),
                    &Default::default(),
                    Files {
                        rows: &[],
                        dir: "ui",
                    },
                    Chrome::new(&palette),
                );
            })
            .expect("draw");
        let screen: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(
            screen.contains("[/]"),
            "{tab:?} does not offer the tab keys"
        );
    }
}

#[test]
fn no_interface_file_declares_a_chord_for_it() {
    let host = host();
    let mut registry = Registry::default();
    let (bindings, settings) = host.declarations();
    registry.declare(bindings, settings);

    for binding in registry.bindings() {
        assert_ne!(
            binding.action, "plugins.open",
            "the file list has no opener; it is a settings tab"
        );
    }
    assert!(
        !host.mods.iter().any(|loaded| loaded.name == "plugins"),
        "the file list is chrome, not a plugin"
    );
}

#[test]
fn settings_opens_on_its_own_half() {
    let mut modals = Modals::default();
    modals.toggle(ModalKind::Settings);
    assert_eq!(modals.kind(), Some(ModalKind::Settings));
    modals.toggle(ModalKind::Settings);
    modals.toggle(ModalKind::Settings);
    assert_eq!(modals.kind(), Some(ModalKind::Settings));
}

#[test]
fn the_centre_is_still_a_switch_slot() {
    let host = host();
    assert!(matches!(
        host.slot_mode("center"),
        thurbox::kernel::layout::SlotMode::Switch
    ));
}

fn host_with(mods: &[(&str, &str)]) -> (tempfile::TempDir, LuaHost) {
    let home = tempfile::tempdir().expect("tempdir");
    let ui = home.path().join("ui");
    std::fs::create_dir_all(ui.join("mods")).expect("mkdir");
    for (file, body) in mods {
        std::fs::write(ui.join("mods").join(file), body).expect("write plugin");
    }
    let host = LuaHost::new(ui);
    assert!(host.error.is_none(), "{:?}", host.error);
    (home, host)
}

fn index_of(host: &LuaHost, name: &str) -> usize {
    host.mods
        .iter()
        .position(|p| p.name == name)
        .unwrap_or_else(|| panic!("{name} should have loaded"))
}

fn centre_pane(name: &str, order: u8, extra: &str) -> String {
    format!(
        r#"return {{
  name = "{name}",
  slot = "center",
  order = {order},
  focusable = true,
  {extra}
  render = function() return {{ type = "text", text = "{name}" }} end,
}}"#
    )
}

#[test]
fn a_switch_slots_cycle_stop_is_its_first_focusable_pane_never_a_float() {
    let popup = centre_pane("popup", 5, "floats = true,");
    let unfocusable =
        centre_pane("banner", 10, "").replace("focusable = true", "focusable = false");
    let main = centre_pane("main", 20, r#"slot_mode = "switch","#);
    let alternate = centre_pane("alternate", 30, "");
    let (_home, host) = host_with(&[
        ("05_popup.lua", &popup),
        ("10_banner.lua", &unfocusable),
        ("20_main.lua", &main),
        ("30_alternate.lua", &alternate),
    ]);
    assert_eq!(host.switch_default("center"), Some(index_of(&host, "main")));
    assert_eq!(host.switch_default("nowhere"), None);
}
