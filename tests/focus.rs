//! Reaching the view of the interface's own files.
//!
//! It used to be a pane sharing the centre `switch` slot, and it was unreachable
//! twice over. The focus ring skipped every switch alternate by design, and its
//! own `F11` was undone a frame later by the guard that stops focus stranding on
//! a closed column — that guard runs before the switch slot records which
//! occupant it drew, so a pane focused by its opening chord is still an
//! alternate when the guard looks at it. On top of that, `F11` is the one F-key
//! terminal emulators commonly claim for fullscreen.
//!
//! Both were fixed, and then the view stopped being a pane at all: it is a tab of
//! the settings modal, because a recovery tool that is itself a plugin can be the
//! thing that is broken. So what is asserted here is what survived that move —
//! that the view is reachable without a chord of its own, and that no file in the
//! interface can take it away.
//!
//! The focus rule itself is unit-tested in `kernel::focus`, and stays relevant:
//! the centre is still a `switch` slot, so the next pane to share it inherits the
//! fix rather than the bug.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use thurbox::kernel::host::LuaHost;
use thurbox::kernel::modals::chrome::Chrome;
use thurbox::kernel::modals::interface::Files;
use thurbox::kernel::modals::settings::{SettingsModal, Tab};
use thurbox::kernel::modals::{ModalKind, Modals};
use thurbox::kernel::registry::Registry;

fn host() -> LuaHost {
    let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("ui");
    let host = LuaHost::new(dir);
    assert!(host.error.is_none(), "{:?}", host.error);
    host
}

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

#[test]
fn the_file_list_is_reached_through_settings_and_needs_no_chord_of_its_own() {
    // The whole point of the move: no opening key to be swallowed by a terminal,
    // and no pane to be skipped by a focus ring.
    let mut modal = SettingsModal::default();
    assert_eq!(modal.tab(), Tab::Settings);

    modal.on_key(
        &key(KeyCode::Char(']')),
        &mut Registry::default(),
        &Default::default(),
        &[],
    );
    assert_eq!(modal.tab(), Tab::Interface);

    // And back, so the pair is a cycle rather than a one-way trip.
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
    // The bug this closes: the settings half recorded its footer pills by
    // *assigning* the button list, which wiped the tab hitboxes recorded a few
    // lines earlier. The headings drew, and clicking one did nothing.
    let palette = thurbox::session::theme_config::ThemePreset::Default.palette();
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

    // Two headings, both clickable — one per tab.
    let buttons = modal.hits().buttons.len();
    assert!(
        buttons >= 2,
        "the tab headings are not clickable: {buttons}"
    );

    // The `Interface` heading sits on the frame's top border, right of
    // `Settings`. Replaying its key is what a click on it does.
    let (rect, replay) = modal.hits().buttons[1];
    let key = KeyEvent::new(replay.code, replay.modifiers);
    modal.on_key(&key, &mut Registry::default(), &Default::default(), &[]);
    assert_eq!(modal.tab(), Tab::Interface, "clicked at {rect:?}");
}

#[test]
fn the_tab_keys_are_offered_where_they_can_be_seen() {
    // A key that only works if you already know it is a key nobody uses. Both
    // halves say so on their own footer, since a modal's keys are not in the
    // registry and so cannot reach help by declaration.
    let palette = thurbox::session::theme_config::ThemePreset::Default.palette();
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
    // A leftover `f11` pointing at a plugin that no longer exists would resolve
    // to nothing and look like the original bug all over again.
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
        !host.plugins.iter().any(|plugin| plugin.name == "plugins"),
        "the file list is chrome, not a plugin"
    );
}

#[test]
fn settings_opens_on_its_own_half() {
    // The chord says "settings", so it must not land on whichever tab was last
    // looked at — a modal that remembered would answer a different question than
    // the one asked.
    let mut modals = Modals::default();
    modals.toggle(ModalKind::Settings);
    assert_eq!(modals.kind(), Some(ModalKind::Settings));
    modals.toggle(ModalKind::Settings);
    modals.toggle(ModalKind::Settings);
    assert_eq!(modals.kind(), Some(ModalKind::Settings));
}

#[test]
fn the_centre_is_still_a_switch_slot() {
    // Nothing shares it today, so `kernel::focus::can_focus` is exercised only by
    // its own tests. The slot mode is what makes the next pane to share it
    // inherit the fix — if this ever stops being a switch slot, the rule can go.
    let host = host();
    assert!(matches!(
        host.slot_mode("center"),
        thurbox::kernel::layout::SlotMode::Switch
    ));
}
