use crossterm::event::{KeyCode, KeyModifiers};

/// Translate a crossterm key event into the ANSI byte sequence that should be
/// sent to the PTY. Follows xterm-style modifier encoding conventions.
pub fn key_to_bytes(code: KeyCode, modifiers: KeyModifiers) -> Option<Vec<u8>> {
    let shift = modifiers.contains(KeyModifiers::SHIFT);
    let alt = modifiers.contains(KeyModifiers::ALT);
    let ctrl = modifiers.contains(KeyModifiers::CONTROL);

    // Ctrl+letter → control character (0x01..=0x1A), optionally wrapped with ESC for Alt
    if ctrl && !shift {
        if let KeyCode::Char(c) = code {
            if c.is_ascii_alphabetic() {
                let ctrl_byte = c.to_ascii_lowercase() as u8 - b'a' + 1;
                return Some(if alt {
                    vec![0x1b, ctrl_byte]
                } else {
                    vec![ctrl_byte]
                });
            }
        }
    }

    // Shift+Enter → ESC [ 13;2u (xterm modifyOtherKeys / kitty protocol)
    if shift && code == KeyCode::Enter {
        return Some(b"\x1b[13;2u".to_vec());
    }

    // BackTab / Shift+Tab → reverse tab (CSI Z)
    if matches!(code, KeyCode::BackTab) || (shift && code == KeyCode::Tab) {
        return Some(b"\x1b[Z".to_vec());
    }

    // Cursor keys & navigation with modifiers: CSI 1;<mod> X
    let modifier_param = xterm_modifier(shift, alt, ctrl);
    if let Some(suffix) = cursor_key_suffix(code) {
        return if modifier_param > 1 {
            Some(format!("\x1b[1;{modifier_param}{suffix}").into_bytes())
        } else {
            Some(format!("\x1b[{suffix}").into_bytes())
        };
    }

    // Extended keys with modifiers: CSI <num>;<mod> ~
    if let Some(num) = extended_key_num(code) {
        return if modifier_param > 1 {
            Some(format!("\x1b[{num};{modifier_param}~").into_bytes())
        } else {
            Some(format!("\x1b[{num}~").into_bytes())
        };
    }

    // F-keys
    if let KeyCode::F(n) = code {
        return f_key_bytes(n, modifier_param);
    }

    // Alt wraps the inner byte with ESC prefix
    if alt {
        if let KeyCode::Char(c) = code {
            let mut buf = [0u8; 4];
            let s = c.encode_utf8(&mut buf);
            let mut result = vec![0x1b];
            result.extend_from_slice(s.as_bytes());
            return Some(result);
        }
        // Alt+Enter, Alt+Backspace, etc.
        let inner = unmodified_key(code)?;
        let mut result = vec![0x1b];
        result.extend(inner);
        return Some(result);
    }

    // Shift+Char: just send the character (already uppercase/shifted by crossterm)
    if shift {
        if let KeyCode::Char(c) = code {
            let mut buf = [0u8; 4];
            let s = c.encode_utf8(&mut buf);
            return Some(s.as_bytes().to_vec());
        }
    }

    unmodified_key(code)
}

/// Plain key without modifiers.
fn unmodified_key(code: KeyCode) -> Option<Vec<u8>> {
    match code {
        KeyCode::Char(c) => {
            let mut buf = [0u8; 4];
            let s = c.encode_utf8(&mut buf);
            Some(s.as_bytes().to_vec())
        }
        KeyCode::Enter => Some(vec![b'\r']),
        KeyCode::Backspace => Some(vec![0x7f]),
        KeyCode::Tab => Some(vec![b'\t']),
        KeyCode::BackTab => Some(b"\x1b[Z".to_vec()),
        KeyCode::Esc => Some(vec![0x1b]),
        _ => None,
    }
}

/// xterm modifier parameter: 1=none, 2=Shift, 3=Alt, 4=Shift+Alt, 5=Ctrl, etc.
fn xterm_modifier(shift: bool, alt: bool, ctrl: bool) -> u8 {
    1 + (shift as u8) + ((alt as u8) << 1) + ((ctrl as u8) << 2)
}

/// Single-char suffix for cursor/navigation keys (CSI <suffix> format).
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

/// Numeric parameter for extended keys (CSI <num> ~ format).
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
    // F1-F4 use SS3 format (no modifier support in SS3, fall back to CSI)
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
        // Ctrl=5, Ctrl+Shift=6
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
        // Ctrl+Alt+C = ESC + 0x03
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
        // Shift+A should produce 'A' (crossterm already delivers uppercase)
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
        // Alt modifier_param = 3, so uses CSI 1;3 A format
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
}
