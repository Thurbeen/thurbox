use crossterm::event::{KeyCode, KeyModifiers};

/// Translate a crossterm key event into the ANSI byte sequence that should be
/// sent to the PTY.
pub fn key_to_bytes(code: KeyCode, modifiers: KeyModifiers) -> Option<Vec<u8>> {
    if modifiers.contains(KeyModifiers::CONTROL) {
        return match code {
            // Ctrl+A..=Ctrl+Z → 0x01..=0x1A
            KeyCode::Char(c) if c.is_ascii_lowercase() => Some(vec![c as u8 - b'a' + 1]),
            KeyCode::Char(c) if c.is_ascii_uppercase() => {
                Some(vec![c.to_ascii_lowercase() as u8 - b'a' + 1])
            }
            _ => None,
        };
    }

    match code {
        KeyCode::Char(c) => {
            let mut buf = [0u8; 4];
            let s = c.encode_utf8(&mut buf);
            Some(s.as_bytes().to_vec())
        }
        KeyCode::Enter => Some(vec![b'\r']),
        KeyCode::Backspace => Some(vec![0x7f]),
        KeyCode::Tab => Some(vec![b'\t']),
        KeyCode::Esc => Some(vec![0x1b]),
        KeyCode::Up => Some(b"\x1b[A".to_vec()),
        KeyCode::Down => Some(b"\x1b[B".to_vec()),
        KeyCode::Right => Some(b"\x1b[C".to_vec()),
        KeyCode::Left => Some(b"\x1b[D".to_vec()),
        KeyCode::Home => Some(b"\x1b[H".to_vec()),
        KeyCode::End => Some(b"\x1b[F".to_vec()),
        KeyCode::PageUp => Some(b"\x1b[5~".to_vec()),
        KeyCode::PageDown => Some(b"\x1b[6~".to_vec()),
        KeyCode::Delete => Some(b"\x1b[3~".to_vec()),
        KeyCode::Insert => Some(b"\x1b[2~".to_vec()),
        KeyCode::F(n) => f_key_bytes(n),
        _ => None,
    }
}

fn f_key_bytes(n: u8) -> Option<Vec<u8>> {
    let seq = match n {
        1 => "\x1bOP",
        2 => "\x1bOQ",
        3 => "\x1bOR",
        4 => "\x1bOS",
        5 => "\x1b[15~",
        6 => "\x1b[17~",
        7 => "\x1b[18~",
        8 => "\x1b[19~",
        9 => "\x1b[20~",
        10 => "\x1b[21~",
        11 => "\x1b[23~",
        12 => "\x1b[24~",
        _ => return None,
    };
    Some(seq.as_bytes().to_vec())
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
    fn enter_produces_cr() {
        let bytes = key_to_bytes(KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(bytes, Some(vec![b'\r']));
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
}
