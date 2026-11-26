use crossterm::event::{KeyCode, KeyModifiers};

pub fn key_to_bytes(code: KeyCode, modifiers: KeyModifiers) -> Option<Vec<u8>> {
    // Cmd/Super keys have no legacy PTY encoding, and the inner app never negotiated kitty keys.
    if modifiers.contains(KeyModifiers::SUPER) {
        return None;
    }
    let shift = modifiers.contains(KeyModifiers::SHIFT);
    let alt = modifiers.contains(KeyModifiers::ALT);
    let ctrl = modifiers.contains(KeyModifiers::CONTROL);
    let modifier_param = xterm_modifier(shift, alt, ctrl);

    if let Some(bytes) = ctrl_or_shift_special_bytes(code, shift, alt, ctrl) {
        return Some(bytes);
    }

    if let Some(bytes) = csi_key_bytes(code, modifier_param) {
        return Some(bytes);
    }

    if let KeyCode::F(n) = code {
        return f_key_bytes(n, modifier_param);
    }

    if alt {
        return alt_bytes(code);
    }

    if shift {
        if let KeyCode::Char(c) = code {
            return Some(char_bytes(c));
        }
    }

    unmodified_key(code)
}

fn ctrl_or_shift_special_bytes(
    code: KeyCode,
    shift: bool,
    alt: bool,
    ctrl: bool,
) -> Option<Vec<u8>> {
    if let Some(bytes) = ctrl_letter_bytes(code, shift, alt, ctrl) {
        return Some(bytes);
    }

    // Shift+Enter has no legacy byte; xterm modifyOtherKeys and kitty spell it CSI 13;2u.
    if shift && code == KeyCode::Enter {
        return Some(b"\x1b[13;2u".to_vec());
    }

    if matches!(code, KeyCode::BackTab) || (shift && code == KeyCode::Tab) {
        return Some(b"\x1b[Z".to_vec());
    }

    None
}

fn csi_key_bytes(code: KeyCode, modifier_param: u8) -> Option<Vec<u8>> {
    if let Some(bytes) = cursor_key_bytes(code, modifier_param) {
        return Some(bytes);
    }

    extended_key_bytes(code, modifier_param)
}

fn char_bytes(c: char) -> Vec<u8> {
    let mut buf = [0u8; 4];
    c.encode_utf8(&mut buf).as_bytes().to_vec()
}

fn ctrl_letter_bytes(code: KeyCode, shift: bool, alt: bool, ctrl: bool) -> Option<Vec<u8>> {
    if !ctrl || shift {
        return None;
    }
    let KeyCode::Char(c) = code else {
        return None;
    };
    if !c.is_ascii_alphabetic() {
        return None;
    }
    let ctrl_byte = c.to_ascii_lowercase() as u8 - b'a' + 1;
    Some(if alt {
        vec![0x1b, ctrl_byte]
    } else {
        vec![ctrl_byte]
    })
}

fn cursor_key_bytes(code: KeyCode, modifier_param: u8) -> Option<Vec<u8>> {
    let suffix = cursor_key_suffix(code)?;
    Some(if modifier_param > 1 {
        format!("\x1b[1;{modifier_param}{suffix}").into_bytes()
    } else {
        format!("\x1b[{suffix}").into_bytes()
    })
}

fn extended_key_bytes(code: KeyCode, modifier_param: u8) -> Option<Vec<u8>> {
    let num = extended_key_num(code)?;
    Some(if modifier_param > 1 {
        format!("\x1b[{num};{modifier_param}~").into_bytes()
    } else {
        format!("\x1b[{num}~").into_bytes()
    })
}

fn alt_bytes(code: KeyCode) -> Option<Vec<u8>> {
    let inner = match code {
        KeyCode::Char(c) => char_bytes(c),
        _ => unmodified_key(code)?,
    };
    let mut result = vec![0x1b];
    result.extend(inner);
    Some(result)
}

fn unmodified_key(code: KeyCode) -> Option<Vec<u8>> {
    match code {
        KeyCode::Char(c) => Some(char_bytes(c)),
        KeyCode::Enter => Some(vec![b'\r']),
        KeyCode::Backspace => Some(vec![0x7f]),
        KeyCode::Tab => Some(vec![b'\t']),
        KeyCode::BackTab => Some(b"\x1b[Z".to_vec()),
        KeyCode::Esc => Some(vec![0x1b]),
        _ => None,
    }
}

fn xterm_modifier(shift: bool, alt: bool, ctrl: bool) -> u8 {
    1 + (shift as u8) + ((alt as u8) << 1) + ((ctrl as u8) << 2)
}

fn cursor_key_suffix(code: KeyCode) -> Option<char> {
    match code {
        KeyCode::Up => Some('A'),
        KeyCode::Down => Some('B'),
        KeyCode::Right => Some('C'),
        KeyCode::Left => Some('D'),
        KeyCode::Home => Some('H'),
        KeyCode::End => Some('F'),
        _ => None,
    }
}

fn extended_key_num(code: KeyCode) -> Option<u8> {
    match code {
        KeyCode::Insert => Some(2),
        KeyCode::Delete => Some(3),
        KeyCode::PageUp => Some(5),
        KeyCode::PageDown => Some(6),
        _ => None,
    }
}

fn f_key_bytes(n: u8, modifier_param: u8) -> Option<Vec<u8>> {
    // SS3 carries no modifier, so a modified F1-F4 falls back to CSI.
    if modifier_param <= 1 {
        let seq = match n {
            1 => "\x1bOP",
            2 => "\x1bOQ",
            3 => "\x1bOR",
            4 => "\x1bOS",
            _ => {
                return f_key_csi(n, modifier_param);
            }
        };
        return Some(seq.as_bytes().to_vec());
    }
    f_key_csi(n, modifier_param)
}

fn f_key_csi(n: u8, modifier_param: u8) -> Option<Vec<u8>> {
    let num = match n {
        1 => 11,
        2 => 12,
        3 => 13,
        4 => 14,
        5 => 15,
        6 => 17,
        7 => 18,
        8 => 19,
        9 => 20,
        10 => 21,
        11 => 23,
        12 => 24,
        _ => return None,
    };
    Some(if modifier_param > 1 {
        format!("\x1b[{num};{modifier_param}~").into_bytes()
    } else {
        format!("\x1b[{num}~").into_bytes()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn regular_chars_produce_utf8() {
        let bytes = key_to_bytes(KeyCode::Char('a'), KeyModifiers::NONE);
        assert_eq!(bytes, Some(vec![b'a']));
    }

    #[test]
    fn ctrl_c_produces_etx() {
        let bytes = key_to_bytes(KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert_eq!(bytes, Some(vec![0x03]));
    }

    #[test]
    fn ctrl_d_produces_eot() {
        let bytes = key_to_bytes(KeyCode::Char('d'), KeyModifiers::CONTROL);
        assert_eq!(bytes, Some(vec![0x04]));
    }

    #[test]
    fn enter_produces_cr() {
        let bytes = key_to_bytes(KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(bytes, Some(vec![b'\r']));
    }

    #[test]
    fn shift_enter_produces_modified_enter() {
        let bytes = key_to_bytes(KeyCode::Enter, KeyModifiers::SHIFT);
        assert_eq!(bytes, Some(b"\x1b[13;2u".to_vec()));
    }

    #[test]
    fn arrow_keys_produce_ansi_sequences() {
        assert_eq!(
            key_to_bytes(KeyCode::Up, KeyModifiers::NONE),
            Some(b"\x1b[A".to_vec())
        );
        assert_eq!(
            key_to_bytes(KeyCode::Down, KeyModifiers::NONE),
            Some(b"\x1b[B".to_vec())
        );
    }

    #[test]
    fn backspace_produces_del() {
        assert_eq!(
            key_to_bytes(KeyCode::Backspace, KeyModifiers::NONE),
            Some(vec![0x7f])
        );
    }

    #[test]
    fn shift_tab_produces_reverse_tab() {
        assert_eq!(
            key_to_bytes(KeyCode::BackTab, KeyModifiers::SHIFT),
            Some(b"\x1b[Z".to_vec())
        );
        assert_eq!(
            key_to_bytes(KeyCode::BackTab, KeyModifiers::NONE),
            Some(b"\x1b[Z".to_vec())
        );
    }

    #[test]
    fn shift_arrows_produce_modified_sequences() {
        assert_eq!(
            key_to_bytes(KeyCode::Up, KeyModifiers::SHIFT),
            Some(b"\x1b[1;2A".to_vec())
        );
        assert_eq!(
            key_to_bytes(KeyCode::Down, KeyModifiers::SHIFT),
            Some(b"\x1b[1;2B".to_vec())
        );
    }

    #[test]
    fn ctrl_shift_arrows() {
        assert_eq!(
            key_to_bytes(KeyCode::Up, KeyModifiers::CONTROL),
            Some(b"\x1b[1;5A".to_vec())
        );
        assert_eq!(
            key_to_bytes(KeyCode::Right, KeyModifiers::CONTROL | KeyModifiers::SHIFT),
            Some(b"\x1b[1;6C".to_vec())
        );
    }

    #[test]
    fn alt_key_wraps_with_esc() {
        assert_eq!(
            key_to_bytes(KeyCode::Char('m'), KeyModifiers::ALT),
            Some(vec![0x1b, b'm'])
        );
        assert_eq!(
            key_to_bytes(KeyCode::Char('p'), KeyModifiers::ALT),
            Some(vec![0x1b, b'p'])
        );
    }

    #[test]
    fn alt_ctrl_letter() {
        assert_eq!(
            key_to_bytes(
                KeyCode::Char('c'),
                KeyModifiers::CONTROL | KeyModifiers::ALT
            ),
            Some(vec![0x1b, 0x03])
        );
    }

    #[test]
    fn alt_enter() {
        assert_eq!(
            key_to_bytes(KeyCode::Enter, KeyModifiers::ALT),
            Some(vec![0x1b, b'\r'])
        );
    }

    #[test]
    fn delete_with_modifiers() {
        assert_eq!(
            key_to_bytes(KeyCode::Delete, KeyModifiers::NONE),
            Some(b"\x1b[3~".to_vec())
        );
        assert_eq!(
            key_to_bytes(KeyCode::Delete, KeyModifiers::SHIFT),
            Some(b"\x1b[3;2~".to_vec())
        );
    }

    #[test]
    fn f_keys_plain_and_modified() {
        assert_eq!(
            key_to_bytes(KeyCode::F(1), KeyModifiers::NONE),
            Some(b"\x1bOP".to_vec())
        );
        assert_eq!(
            key_to_bytes(KeyCode::F(5), KeyModifiers::NONE),
            Some(b"\x1b[15~".to_vec())
        );
        assert_eq!(
            key_to_bytes(KeyCode::F(1), KeyModifiers::SHIFT),
            Some(b"\x1b[11;2~".to_vec())
        );
    }

    #[test]
    fn esc_produces_escape_byte() {
        assert_eq!(
            key_to_bytes(KeyCode::Esc, KeyModifiers::NONE),
            Some(vec![0x1b])
        );
    }

    #[test]
    fn tab_produces_tab_byte() {
        assert_eq!(
            key_to_bytes(KeyCode::Tab, KeyModifiers::NONE),
            Some(vec![b'\t'])
        );
    }

    #[test]
    fn home_and_end_keys() {
        assert_eq!(
            key_to_bytes(KeyCode::Home, KeyModifiers::NONE),
            Some(b"\x1b[H".to_vec())
        );
        assert_eq!(
            key_to_bytes(KeyCode::End, KeyModifiers::NONE),
            Some(b"\x1b[F".to_vec())
        );
    }

    #[test]
    fn page_up_and_page_down() {
        assert_eq!(
            key_to_bytes(KeyCode::PageUp, KeyModifiers::NONE),
            Some(b"\x1b[5~".to_vec())
        );
        assert_eq!(
            key_to_bytes(KeyCode::PageDown, KeyModifiers::NONE),
            Some(b"\x1b[6~".to_vec())
        );
    }

    #[test]
    fn shift_char_passes_through() {
        assert_eq!(
            key_to_bytes(KeyCode::Char('A'), KeyModifiers::SHIFT),
            Some(vec![b'A'])
        );
    }

    #[test]
    fn alt_backspace() {
        assert_eq!(
            key_to_bytes(KeyCode::Backspace, KeyModifiers::ALT),
            Some(vec![0x1b, 0x7f])
        );
    }

    #[test]
    fn f13_returns_none() {
        assert_eq!(key_to_bytes(KeyCode::F(13), KeyModifiers::NONE), None);
    }

    #[test]
    fn xterm_modifier_values() {
        assert_eq!(xterm_modifier(false, false, false), 1);
        assert_eq!(xterm_modifier(true, false, false), 2);
        assert_eq!(xterm_modifier(false, true, false), 3);
        assert_eq!(xterm_modifier(true, true, false), 4);
        assert_eq!(xterm_modifier(false, false, true), 5);
        assert_eq!(xterm_modifier(true, false, true), 6);
        assert_eq!(xterm_modifier(false, true, true), 7);
        assert_eq!(xterm_modifier(true, true, true), 8);
    }

    #[test]
    fn alt_arrow_uses_modifier_param() {
        assert_eq!(
            key_to_bytes(KeyCode::Up, KeyModifiers::ALT),
            Some(b"\x1b[1;3A".to_vec())
        );
    }

    #[test]
    fn insert_key() {
        assert_eq!(
            key_to_bytes(KeyCode::Insert, KeyModifiers::NONE),
            Some(b"\x1b[2~".to_vec())
        );
    }

    #[test]
    fn utf8_char() {
        assert_eq!(
            key_to_bytes(KeyCode::Char('é'), KeyModifiers::NONE),
            Some("é".as_bytes().to_vec())
        );
    }

    #[test]
    fn ctrl_a_produces_soh() {
        assert_eq!(
            key_to_bytes(KeyCode::Char('a'), KeyModifiers::CONTROL),
            Some(vec![0x01])
        );
    }

    #[test]
    fn ctrl_z_produces_sub() {
        assert_eq!(
            key_to_bytes(KeyCode::Char('z'), KeyModifiers::CONTROL),
            Some(vec![0x1a])
        );
    }

    #[test]
    fn super_modified_keys_are_swallowed() {
        assert_eq!(key_to_bytes(KeyCode::Char('j'), KeyModifiers::SUPER), None);
        assert_eq!(
            key_to_bytes(
                KeyCode::Char('j'),
                KeyModifiers::SUPER | KeyModifiers::CONTROL
            ),
            None
        );
        assert_eq!(key_to_bytes(KeyCode::Enter, KeyModifiers::SUPER), None);
    }
}
