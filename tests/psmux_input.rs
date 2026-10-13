use crossterm::event::{KeyCode, KeyModifiers};
use thurbox::agent::input::key_to_bytes;
use thurbox::backend::psmux::psmux_send_keys_commands;

#[test]
fn insert_and_delete_reach_psmux_as_one_key_event() {
    for (key, modifiers, name) in [
        (KeyCode::Insert, KeyModifiers::NONE, "Insert"),
        (KeyCode::Delete, KeyModifiers::NONE, "Delete"),
        (KeyCode::Insert, KeyModifiers::SHIFT, "S-Insert"),
        (KeyCode::Delete, KeyModifiers::CONTROL, "C-Delete"),
        (
            KeyCode::Delete,
            KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SHIFT,
            "C-M-S-Delete",
        ),
    ] {
        let bytes = key_to_bytes(key, modifiers).expect("key encoding");
        assert_eq!(
            psmux_send_keys_commands("%1", &bytes),
            vec![format!("send-keys -t %1 {name}\n")],
            "{key:?} {modifiers:?} encoded as {bytes:?}"
        );
    }
}

#[test]
fn printable_alt_prefixes_reach_psmux_as_one_literal_command() {
    for character in ['[', 'O'] {
        let bytes =
            key_to_bytes(KeyCode::Char(character), KeyModifiers::ALT).expect("Alt chord encoding");
        assert_eq!(
            psmux_send_keys_commands("%1", &bytes),
            vec![format!("send-keys -t %1 -l -N 1 0x1b \"{character}\"\n")],
            "Alt+{character} encoded as {bytes:?}"
        );
    }
}

#[test]
fn named_navigation_and_lone_escape_keep_psmux_key_names() {
    for (key, name) in [(KeyCode::Up, "Up"), (KeyCode::Esc, "Escape")] {
        let bytes = key_to_bytes(key, KeyModifiers::NONE).expect("key encoding");
        assert_eq!(
            psmux_send_keys_commands("%1", &bytes),
            vec![format!("send-keys -t %1 {name}\n")],
            "{key:?} encoded as {bytes:?}"
        );
    }
}
