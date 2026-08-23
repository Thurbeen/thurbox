//! `control_mode`'s tests, kept together (the `git/tests.rs` pattern): a
//! sibling module of `mod.rs`, so private items stay reachable.

use std::sync::mpsc::sync_channel;

use super::*;

// --- diff_polled_hook_states tests ---

#[test]
fn poll_diff_reports_first_seen_and_changes_only() {
    let mut last = std::collections::HashMap::new();
    // First poll: every non-empty value reports (arm-time catch-up parity).
    let events = diff_polled_hook_states(&mut last, "%1 working\n%2 \n%3 done");
    assert_eq!(
        events,
        vec![
            ("%1".to_string(), "working".to_string()),
            ("%3".to_string(), "done".to_string())
        ]
    );
    // Steady state stays silent.
    assert!(diff_polled_hook_states(&mut last, "%1 working\n%3 done").is_empty());
    // Only the changed pane reports.
    let events = diff_polled_hook_states(&mut last, "%1 done\n%3 done");
    assert_eq!(events, vec![("%1".to_string(), "done".to_string())]);
}

#[test]
fn poll_diff_clears_vanished_and_unset_panes_so_they_rereport() {
    let mut last = std::collections::HashMap::new();
    diff_polled_hook_states(&mut last, "%1 working\n%2 blocked");
    // %1 vanishes (respawn), %2's option is unset.
    assert!(diff_polled_hook_states(&mut last, "%2").is_empty());
    // Both re-report when they come back with a value.
    let events = diff_polled_hook_states(&mut last, "%1 working\n%2 blocked");
    assert_eq!(events.len(), 2);
}

#[test]
fn poll_diff_skips_malformed_lines_and_trims_values() {
    let mut last = std::collections::HashMap::new();
    let events = diff_polled_hook_states(
        &mut last,
        "not-a-pane working\n%x done\n\n%7  done  \n%8 two words",
    );
    // Invalid pane tokens are dropped; values are trimmed; a multi-word
    // value passes through (the app-side allow-list rejects it there).
    assert_eq!(
        events,
        vec![
            ("%7".to_string(), "done".to_string()),
            ("%8".to_string(), "two words".to_string())
        ]
    );
}

// --- parse_pane_pids tests ---

#[test]
fn pane_pids_parse_skips_malformed_lines() {
    let map = parse_pane_pids("%1 4321\n%2 not-a-pid\nnot-a-pane 7\n\n %3 8 \n%4");
    assert_eq!(map.len(), 2);
    assert_eq!(map.get("%1"), Some(&4321));
    assert_eq!(map.get("%3"), Some(&8));
}

// --- is_valid_pane_id tests ---

#[test]
fn pane_id_accepts_percent_digits() {
    assert!(is_valid_pane_id("%0"));
    assert!(is_valid_pane_id("%42"));
    assert!(is_valid_pane_id("%123456"));
}

#[test]
fn pane_id_rejects_everything_else() {
    assert!(!is_valid_pane_id(""));
    assert!(!is_valid_pane_id("%"));
    assert!(!is_valid_pane_id("42"));
    assert!(!is_valid_pane_id("%4a"));
    assert!(!is_valid_pane_id("% 42"));
    assert!(!is_valid_pane_id("%-1"));
    assert!(!is_valid_pane_id("%42; kill-server"));
    assert!(!is_valid_pane_id("%42\nkill-server"));
}

// --- shell_escape tests ---

#[test]
fn shell_escape_empty() {
    assert_eq!(shell_escape(""), "''");
}

#[test]
fn shell_escape_simple() {
    assert_eq!(shell_escape("hello"), "hello");
}

#[test]
fn shell_escape_path() {
    assert_eq!(shell_escape("/home/user/repos/app"), "/home/user/repos/app");
}

#[test]
fn shell_escape_with_spaces() {
    assert_eq!(shell_escape("hello world"), "'hello world'");
}

#[test]
fn shell_escape_with_quotes() {
    assert_eq!(shell_escape("it's"), "'it'\\''s'");
}

#[test]
fn shell_escape_flag_value() {
    assert_eq!(shell_escape("--permission-mode"), "--permission-mode");
}

#[test]
fn shell_escape_tool_pattern() {
    assert_eq!(shell_escape("Read Bash(git:*)"), "'Read Bash(git:*)'");
}

#[test]
fn shell_escape_allows_equals_comma() {
    assert_eq!(shell_escape("key=val,other"), "key=val,other");
}

#[test]
fn shell_escape_replaces_newlines() {
    // Newlines in tmux control mode commands would split the command,
    // corrupting the protocol. They must be replaced with spaces.
    assert_eq!(
        shell_escape("line one\nline two\nline three"),
        "'line one line two line three'"
    );
}

#[test]
fn shell_escape_newline_only() {
    assert_eq!(shell_escape("\n"), "' '");
}

// --- decode_octal tests ---

#[test]
fn decode_octal_esc() {
    assert_eq!(decode_octal("\\033"), vec![27]);
}

#[test]
fn decode_octal_backslash() {
    assert_eq!(decode_octal("\\134"), vec![b'\\']);
}

#[test]
fn decode_octal_newline() {
    assert_eq!(decode_octal("\\012"), vec![b'\n']);
}

#[test]
fn decode_octal_passthrough() {
    assert_eq!(decode_octal("hello"), b"hello");
}

#[test]
fn decode_octal_incomplete() {
    assert_eq!(decode_octal("\\01"), b"\\01");
}

#[test]
fn decode_octal_non_octal_digits() {
    assert_eq!(decode_octal("\\089"), b"\\089");
}

#[test]
fn decode_octal_mixed() {
    assert_eq!(
        decode_octal("A\\033[1mB"),
        vec![b'A', 27, b'[', b'1', b'm', b'B']
    );
}

#[test]
fn decode_octal_consecutive() {
    assert_eq!(decode_octal("\\033\\033"), vec![27, 27]);
}

#[test]
fn decode_octal_empty() {
    assert_eq!(decode_octal(""), b"");
}

#[test]
fn decode_octal_trailing_backslash() {
    assert_eq!(decode_octal("a\\"), b"a\\");
}

#[test]
fn decode_octal_max_value() {
    assert_eq!(decode_octal("\\377"), vec![0xFF]);
}

#[test]
fn decode_octal_overflow_wraps() {
    assert_eq!(decode_octal("\\400"), vec![0u8]);
}

// --- parse_notification tests ---

#[test]
fn parse_output_notification() {
    let n = parse_notification("%output %42 hello\\033[1m");
    assert_eq!(
        n,
        Notification::Output {
            pane_id: "%42".to_string(),
            data: vec![b'h', b'e', b'l', b'l', b'o', 27, b'[', b'1', b'm'],
        }
    );
}

#[test]
fn parse_extended_output_notification() {
    let n = parse_notification("%extended-output %2 0 : \\033[?2026hA\\033[?2026l");
    assert_eq!(
        n,
        Notification::Output {
            pane_id: "%2".to_string(),
            data: vec![
                27, b'[', b'?', b'2', b'0', b'2', b'6', b'h', b'A', 27, b'[', b'?', b'2', b'0',
                b'2', b'6', b'l'
            ],
        }
    );
}

#[test]
fn parse_begin_notification() {
    assert_eq!(
        parse_notification("%begin 1234567890 7 0"),
        Notification::Begin
    );
}

#[test]
fn parse_end_notification() {
    assert_eq!(parse_notification("%end 1234567890 7 0"), Notification::End);
}

#[test]
fn parse_error_notification() {
    assert_eq!(
        parse_notification("%error 1234567890 3 0"),
        Notification::Error
    );
}

#[test]
fn parse_pause_notification() {
    assert_eq!(
        parse_notification("%pause %42"),
        Notification::Pause {
            pane_id: "%42".to_string()
        }
    );
}

#[test]
fn parse_other_notification() {
    assert_eq!(
        parse_notification("some random line"),
        Notification::Other("some random line".to_string())
    );
}

#[test]
fn parse_output_no_data() {
    assert_eq!(
        parse_notification("%output %42"),
        Notification::Other("%output %42".to_string())
    );
}

#[test]
fn parse_extended_output_no_colon_separator() {
    assert_eq!(
        parse_notification("%extended-output %2 0 data"),
        Notification::Other("%extended-output %2 0 data".to_string())
    );
}

#[test]
fn parse_output_empty_data() {
    let n = parse_notification("%output %42 ");
    assert_eq!(
        n,
        Notification::Output {
            pane_id: "%42".to_string(),
            data: vec![],
        }
    );
}

#[test]
fn parse_pause_notification_with_leading_percent() {
    assert_eq!(
        parse_notification("%pause %123"),
        Notification::Pause {
            pane_id: "%123".to_string()
        }
    );
}

#[test]
fn parse_extended_output_missing_pane_space() {
    assert_eq!(
        parse_notification("%extended-output %2 : data"),
        Notification::Other("%extended-output %2 : data".to_string())
    );
}

// --- %subscription-changed parsing tests ---
// Wire shape (tmux man page): `%subscription-changed name session-id
// window-id window-index pane-id ... : value` — args between pane-id and
// the ':' are documented future-use; the value may be empty or contain
// spaces/colons.

#[test]
fn subscription_changed_parses_canonical_line() {
    assert_eq!(
        parse_notification("%subscription-changed thurbox-status $1 @5 2 %7 : done"),
        Notification::SubscriptionChanged {
            name: "thurbox-status".into(),
            pane_id: "%7".into(),
            value: "done".into(),
        }
    );
}

#[test]
fn subscription_changed_ignores_future_use_args() {
    assert_eq!(
        parse_notification("%subscription-changed thurbox-status $1 @5 2 %7 extra stuff : working"),
        Notification::SubscriptionChanged {
            name: "thurbox-status".into(),
            pane_id: "%7".into(),
            value: "working".into(),
        }
    );
}

#[test]
fn subscription_changed_empty_value_variants() {
    for line in [
        "%subscription-changed s $1 @5 2 %7 :",
        "%subscription-changed s $1 @5 2 %7 : ",
        "%subscription-changed s $1 @5 2 %7 future :",
    ] {
        match parse_notification(line) {
            Notification::SubscriptionChanged { value, .. } => {
                assert_eq!(value, "", "line: {line}")
            }
            other => panic!("expected SubscriptionChanged for {line}, got {other:?}"),
        }
    }
}

#[test]
fn subscription_changed_value_keeps_spaces_and_colons() {
    assert_eq!(
        parse_notification("%subscription-changed s $1 @5 2 %7 : a b : c"),
        Notification::SubscriptionChanged {
            name: "s".into(),
            pane_id: "%7".into(),
            value: "a b : c".into(),
        }
    );
}

#[test]
fn subscription_changed_malformed_falls_to_other() {
    // Invalid pane token / missing separator / truncated — wire data must
    // never panic, it degrades to Other.
    for line in [
        "%subscription-changed s $1 @5 2 pane7 : done",
        "%subscription-changed s $1 @5 2 %7 done",
        "%subscription-changed s $1 @5",
        "%subscription-changed",
    ] {
        assert!(
            matches!(parse_notification(line), Notification::Other(_)),
            "line: {line}"
        );
    }
}

// --- format_send_keys tests ---

#[test]
fn format_send_keys_single_byte() {
    assert_eq!(format_send_keys("%42", b"A"), "send-keys -t %42 -H 41\n");
}

#[test]
fn format_send_keys_multi_byte() {
    assert_eq!(
        format_send_keys("%42", b"ABC"),
        "send-keys -t %42 -H 41 42 43\n"
    );
}

#[test]
fn format_send_keys_empty() {
    assert_eq!(format_send_keys("%42", &[]), "send-keys -t %42 -H\n");
}

#[test]
fn format_send_keys_escape_sequence() {
    assert_eq!(
        format_send_keys("%1", &[0x1b, b'[', b'A']),
        "send-keys -t %1 -H 1b 5b 41\n"
    );
}

// --- send_keys_commands chunking tests ---

#[test]
fn send_keys_commands_short_input_is_one_command() {
    let cmds = send_keys_commands("%1", b"ABC", false);
    assert_eq!(cmds, vec!["send-keys -t %1 -H 41 42 43\n".to_string()]);
}

#[test]
fn send_keys_commands_empty_input_is_no_commands() {
    assert!(send_keys_commands("%1", &[], false).is_empty());
    assert!(send_keys_commands("%1", &[], true).is_empty());
}

/// A large paste is split into multiple bounded `send-keys` commands whose
/// concatenated bytes equal the original input — the property that keeps a
/// big paste from being truncated by tmux's per-command line limit.
#[test]
fn send_keys_commands_chunks_large_input_losslessly() {
    // 5 KB of bracketed-paste-wrapped content, like `send_paste_to_session`.
    let mut input = b"\x1b[200~".to_vec();
    input.extend((0..5000u32).map(|i| (i % 256) as u8));
    input.extend_from_slice(b"\x1b[201~");

    let cmds = send_keys_commands("%1", &input, false);

    assert!(
        cmds.len() > 1,
        "expected the large input to span multiple commands, got {}",
        cmds.len()
    );

    // Parse each `send-keys -t %1 -H XX XX …\n` back into its bytes.
    let decode = |cmd: &str| -> Vec<u8> {
        cmd.trim_end()
            .strip_prefix("send-keys -t %1 -H")
            .expect("send-keys prefix")
            .split_whitespace()
            .map(|h| u8::from_str_radix(h, 16).expect("hex byte"))
            .collect()
    };

    let mut reassembled = Vec::new();
    for cmd in &cmds {
        let bytes = decode(cmd);
        assert!(
            bytes.len() <= SEND_KEYS_CHUNK_BYTES,
            "chunk encodes {} bytes, exceeds bound {SEND_KEYS_CHUNK_BYTES}",
            bytes.len()
        );
        reassembled.extend(bytes);
    }
    assert_eq!(reassembled, input);
}

// --- psmux send-keys encoding tests ---
//
// Regression: psmux has no `send-keys -H`, so on Windows the hex path
// injected the literal text "62" when the user typed `b` (0x62), and Enter /
// Backspace did nothing. The psmux encoding must use `-l` literals + key-names.

#[test]
fn psmux_printable_char_uses_literal_not_hex() {
    // Typing `b` must inject `b`, not the literal text "62".
    assert_eq!(
        send_keys_commands("%1", b"b", true),
        vec!["send-keys -t %1 -l -N 1 \"b\"\n".to_string()]
    );
}

#[test]
fn psmux_printable_run_is_one_literal_command() {
    assert_eq!(
        send_keys_commands("%1", b"hello world", true),
        vec!["send-keys -t %1 -l -N 1 \"hello world\"\n".to_string()]
    );
}

#[test]
fn psmux_enter_backspace_tab_escape_use_key_names() {
    assert_eq!(
        send_keys_commands("%1", b"\r", true),
        vec!["send-keys -t %1 Enter\n".to_string()]
    );
    assert_eq!(
        send_keys_commands("%1", &[0x7f], true),
        vec!["send-keys -t %1 BSpace\n".to_string()]
    );
    assert_eq!(
        send_keys_commands("%1", b"\t", true),
        vec!["send-keys -t %1 Tab\n".to_string()]
    );
    assert_eq!(
        send_keys_commands("%1", &[0x1b], true),
        vec!["send-keys -t %1 Escape\n".to_string()]
    );
}

#[test]
fn psmux_ctrl_letters_map_to_c_prefix() {
    assert_eq!(
        send_keys_commands("%1", &[0x03], true), // Ctrl+C
        vec!["send-keys -t %1 C-c\n".to_string()]
    );
    assert_eq!(
        send_keys_commands("%1", &[0x01], true), // Ctrl+A
        vec!["send-keys -t %1 C-a\n".to_string()]
    );
    assert_eq!(
        send_keys_commands("%1", &[0x1a], true), // Ctrl+Z
        vec!["send-keys -t %1 C-z\n".to_string()]
    );
    assert_eq!(
        send_keys_commands("%1", &[0x0a], true), // LF → Ctrl+J
        vec!["send-keys -t %1 C-j\n".to_string()]
    );
}

#[test]
fn psmux_arrow_sequence_splits_escape_then_literal() {
    // An arrow key arrives as `\x1b[A`; psmux reconstructs the same bytes
    // from `Escape` + literal `[A`.
    assert_eq!(
        send_keys_commands("%1", b"\x1b[A", true),
        vec![
            "send-keys -t %1 Escape\n".to_string(),
            "send-keys -t %1 -l -N 1 \"[A\"\n".to_string(),
        ]
    );
}

#[test]
fn psmux_literal_single_quote_survives() {
    // Regression: psmux's send-coalescing re-quoted literals with the
    // POSIX `'\''` escape its own parser can't read back, so `it's` was
    // typed into the pane as `it\s`. The `-N 1` opts out of coalescing and
    // the double-quote framing passes `'` through untouched.
    assert_eq!(
        send_keys_commands("%1", b"it's", true),
        vec!["send-keys -t %1 -l -N 1 \"it's\"\n".to_string()]
    );
}

#[test]
fn psmux_literal_escapes_backslash_and_double_quote() {
    // psmux's double-quote tokenizer reads exactly `\"` and `\\`; both
    // must be escaped so Windows paths and quoted text round-trip.
    assert_eq!(
        send_keys_commands("%1", br#"say "hi" C:\p"#, true),
        vec!["send-keys -t %1 -l -N 1 \"say \\\"hi\\\" C:\\\\p\"\n".to_string()]
    );
}

#[test]
fn psmux_literal_escapes_a_leading_hyphen() {
    // Regression (#920): psmux classifies arguments after tokenizing, so
    // the quotes are gone by the time it drops everything starting with
    // `-` as a flag — a typed `-` was silently swallowed. It comes back as
    // psmux's own `0xNN` codepoint form, which decodes to the same char.
    assert_eq!(
        send_keys_commands("%1", b"-", true),
        vec!["send-keys -t %1 -l -N 1 0x2d\n".to_string()]
    );
    // Only the leading hyphens need escaping; the rest stays one literal.
    assert_eq!(
        send_keys_commands("%1", b"--flag=a-b", true),
        vec!["send-keys -t %1 -l -N 1 0x2d 0x2d \"flag=a-b\"\n".to_string()]
    );
}

#[test]
fn psmux_literal_escapes_a_hex_codepoint_lookalike() {
    // psmux rewrites a `0xNN` argument into the character it names, so a
    // run literally spelling `0x41` would have arrived as `A`.
    assert_eq!(
        send_keys_commands("%1", b"0x41", true),
        vec!["send-keys -t %1 -l -N 1 0x30 \"x41\"\n".to_string()]
    );
    // A hyphen ahead of one still leaves a lookalike behind it.
    assert_eq!(
        send_keys_commands("%1", b"-0x41", true),
        vec!["send-keys -t %1 -l -N 1 0x2d 0x30 \"x41\"\n".to_string()]
    );
    // Not a lookalike: text past the hex digits is ordinary literal text.
    assert_eq!(
        send_keys_commands("%1", b"0x41z", true),
        vec!["send-keys -t %1 -l -N 1 \"0x41z\"\n".to_string()]
    );
}

#[test]
fn psmux_literal_args_escapes_an_all_hyphen_run() {
    assert_eq!(psmux_literal_args("---"), "0x2d 0x2d 0x2d");
    assert_eq!(psmux_literal_args(""), "");
}

#[test]
fn psmux_mixed_text_then_enter() {
    // The common "type a command and submit" path.
    assert_eq!(
        send_keys_commands("%1", b"ls\r", true),
        vec![
            "send-keys -t %1 -l -N 1 \"ls\"\n".to_string(),
            "send-keys -t %1 Enter\n".to_string(),
        ]
    );
}

#[test]
fn psmux_bracketed_paste_splits_markers_from_text() {
    // A paste arrives wrapped in `\x1b[200~ … \x1b[201~`; the ESC bytes
    // become `Escape`, the rest stays literal — reconstructing the wrapper.
    // This encoding is only the *fallback* for a psmux pane (the split ESC
    // reaches the pane as a bare Escape keypress, so the marker is lost);
    // the live path is `PsmuxPaste`.
    assert_eq!(
        send_keys_commands("%1", b"\x1b[200~hi\x1b[201~", true),
        vec![
            "send-keys -t %1 Escape\n".to_string(),
            "send-keys -t %1 -l -N 1 \"[200~hi\"\n".to_string(),
            "send-keys -t %1 Escape\n".to_string(),
            "send-keys -t %1 -l -N 1 \"[201~\"\n".to_string(),
        ]
    );
}

// --- psmux out-of-band paste (`PsmuxPaste`) ---

#[test]
fn bracketed_paste_text_unwraps_a_whole_payload() {
    assert_eq!(
        bracketed_paste_text(b"\x1b[200~line one\nline two\r\x1b[201~"),
        Some("line one\nline two\r")
    );
    // Empty paste is still a paste.
    assert_eq!(bracketed_paste_text(b"\x1b[200~\x1b[201~"), Some(""));
}

#[test]
fn bracketed_paste_text_rejects_non_paste_input() {
    // Ordinary keystrokes, and each half of a payload on its own.
    assert_eq!(bracketed_paste_text(b"ls\r"), None);
    assert_eq!(bracketed_paste_text(b"\x1b[200~partial"), None);
    assert_eq!(bracketed_paste_text(b"tail\x1b[201~"), None);
    // Trailing keystroke past the closing marker: not one clean paste.
    assert_eq!(bracketed_paste_text(b"\x1b[200~hi\x1b[201~\r"), None);
}

#[test]
fn bracketed_paste_text_rejects_nested_markers() {
    // Two coalesced pastes, or pasted marker text: the key encoding keeps
    // the bytes verbatim rather than flattening them into one paste.
    assert_eq!(
        bracketed_paste_text(b"\x1b[200~a\x1b[201~\x1b[200~b\x1b[201~"),
        None
    );
    assert_eq!(bracketed_paste_text(b"\x1b[200~a\x1b[200~b\x1b[201~"), None);
}

#[test]
fn bracketed_paste_text_rejects_invalid_utf8() {
    // psmux's `send-paste` payload is text; a byte run that is not UTF-8
    // (e.g. a paste chunked mid-character) falls back to the key encoding.
    assert_eq!(bracketed_paste_text(b"\x1b[200~\xff\x1b[201~"), None);
}

#[test]
fn paste_chunks_keeps_an_ordinary_paste_whole() {
    assert_eq!(paste_chunks("one\ntwo"), vec!["one\ntwo"]);
    let exact = "x".repeat(PASTE_CHUNK_BYTES);
    assert_eq!(paste_chunks(&exact), vec![exact.as_str()]);
}

/// A huge paste is split so no single command line exceeds Windows' ~32 KB
/// cap — losslessly, and never mid-character (psmux drops a payload that
/// isn't valid UTF-8).
#[test]
fn paste_chunks_splits_large_input_on_char_boundaries() {
    // Multi-byte chars straddling the cut: 2 bytes each, odd-sized prefix.
    let text = format!("{}{}", "a", "é".repeat(PASTE_CHUNK_BYTES));
    let chunks = paste_chunks(&text);
    assert!(chunks.len() > 1);
    assert!(chunks.iter().all(|c| c.len() <= PASTE_CHUNK_BYTES));
    assert_eq!(chunks.concat(), text);
}

#[test]
fn send_paste_args_targets_the_pane_with_a_base64_payload() {
    assert_eq!(
        psmux_send_paste_args("%7", "hi\nthere"),
        vec!["send-paste", "-t", "%7", "aGkKdGhlcmU="]
    );
}

/// The payload must never put a raw CR/LF (or a quote) on psmux's
/// line-oriented command wire — base64 is what keeps it off (psmux #560).
#[test]
fn send_paste_args_payload_is_wire_safe() {
    let args = psmux_send_paste_args("%1", "first\r\nsecond \"quoted\" \\ '");
    let payload = args.last().unwrap();
    assert!(!payload
        .bytes()
        .any(|b| matches!(b, b'\r' | b'\n' | b'"' | b'\\' | b'\'' | b' ')));
    assert_eq!(
        base64::engine::general_purpose::STANDARD
            .decode(payload)
            .unwrap(),
        b"first\r\nsecond \"quoted\" \\ '"
    );
}

#[test]
fn psmux_utf8_char_goes_to_literal() {
    assert_eq!(
        send_keys_commands("%1", "é".as_bytes(), true),
        vec!["send-keys -t %1 -l -N 1 \"é\"\n".to_string()]
    );
}

#[test]
fn psmux_long_run_splits_on_char_boundary() {
    let input = "é".repeat(400); // 800 bytes, each char 2 bytes
    let cmds = send_keys_commands("%1", input.as_bytes(), true);
    assert!(cmds.len() > 1, "expected a long run to span >1 command");
    // Reassemble the quoted literals back into the original text.
    let mut text = String::new();
    for cmd in &cmds {
        let inner = cmd
            .trim_end()
            .strip_prefix("send-keys -t %1 -l -N 1 \"")
            .and_then(|s| s.strip_suffix('"'))
            .expect("literal command shape");
        text.push_str(inner);
    }
    assert_eq!(text, input);
}

// --- ControlModeReader tests ---

#[test]
fn control_mode_reader_data_delivery() {
    let (tx, rx) = sync_channel(16);
    let mut reader = ControlModeReader::new(rx);

    tx.send(b"hello".to_vec()).unwrap();
    let mut buf = [0u8; 16];
    let n = reader.read(&mut buf).unwrap();
    assert_eq!(&buf[..n], b"hello");
}

#[test]
fn control_mode_reader_eof_on_sender_drop() {
    let (tx, rx) = sync_channel(16);
    let mut reader = ControlModeReader::new(rx);

    drop(tx);
    let mut buf = [0u8; 16];
    let n = reader.read(&mut buf).unwrap();
    assert_eq!(n, 0);
}

#[test]
fn control_mode_reader_partial_reads() {
    let (tx, rx) = sync_channel(16);
    let mut reader = ControlModeReader::new(rx);

    tx.send(b"hello world".to_vec()).unwrap();

    let mut buf = [0u8; 5];
    let n = reader.read(&mut buf).unwrap();
    assert_eq!(&buf[..n], b"hello");

    let n = reader.read(&mut buf).unwrap();
    assert_eq!(&buf[..n], b" worl");

    let n = reader.read(&mut buf).unwrap();
    assert_eq!(&buf[..n], b"d");
}

#[test]
fn control_mode_reader_multiple_sends() {
    let (tx, rx) = sync_channel(16);
    let mut reader = ControlModeReader::new(rx);

    tx.send(b"aaa".to_vec()).unwrap();
    tx.send(b"bbb".to_vec()).unwrap();

    let mut buf = [0u8; 16];
    let n = reader.read(&mut buf).unwrap();
    assert_eq!(&buf[..n], b"aaa");

    let n = reader.read(&mut buf).unwrap();
    assert_eq!(&buf[..n], b"bbb");
}

#[test]
fn control_mode_writer_is_send() {
    fn assert_send<T: Send>() {}
    assert_send::<ControlModeWriter>();
}

#[test]
fn control_mode_reader_is_send() {
    fn assert_send<T: Send>() {}
    assert_send::<ControlModeReader>();
}

#[test]
fn control_mode_reader_exact_size_buffer() {
    let (tx, rx) = sync_channel(16);
    let mut reader = ControlModeReader::new(rx);

    tx.send(b"abc".to_vec()).unwrap();
    let mut buf = [0u8; 3];
    let n = reader.read(&mut buf).unwrap();
    assert_eq!(n, 3);
    assert_eq!(&buf[..n], b"abc");
}

#[test]
fn try_send_drops_when_channel_full() {
    let (tx, _rx) = sync_channel::<Vec<u8>>(1);

    tx.send(b"first".to_vec()).unwrap();

    match tx.try_send(b"second".to_vec()) {
        Err(std::sync::mpsc::TrySendError::Full(_)) => {}
        other => panic!("Expected TrySendError::Full, got: {other:?}"),
    }
}

// Compile-time check: channel capacity must be large enough to buffer heavy output.
const _: () = assert!(PANE_CHANNEL_CAPACITY >= 1024);

/// Property/fuzz tests proving the tmux control-mode **transport** is byte
/// transparent: whatever the agent writes is exactly what comes out of
/// [`decode_octal`] + [`ControlModeReader`], regardless of how tmux escapes it
/// or how the byte stream is chunked. If these stay green, thurbox's transport
/// layer cannot be the source of glitched/stray characters in the rendered pane.
mod transport_proptests {
    use std::fmt::Write as _;
    use std::io::Read;
    use std::sync::mpsc::channel;

    use proptest::prelude::*;

    use crate::agent::control_mode::{
        decode_octal, format_send_keys, parse_notification, ControlModeReader, Notification,
    };

    /// Reference encoder mirroring tmux's control-mode `%output` escaping:
    /// printable ASCII passes through, backslash becomes `\134`, and every other
    /// byte is emitted as a 3-digit `\ooo` octal escape. Because backslash is
    /// always escaped, a bare `\` never appears in the payload except as the
    /// start of a complete octal escape — exactly the input shape `decode_octal`
    /// is meant to invert.
    fn tmux_octal_encode(bytes: &[u8]) -> String {
        let mut out = String::with_capacity(bytes.len());
        for &b in bytes {
            if b == b'\\' {
                out.push_str("\\134");
            } else if (0x20..=0x7e).contains(&b) {
                out.push(b as char);
            } else {
                write!(out, "\\{b:03o}").unwrap();
            }
        }
        out
    }

    /// Split `bytes` into contiguous, non-empty chunks at the given (wrapped)
    /// offsets — models tmux emitting output across several `%output` lines.
    fn chunk_bytes(bytes: &[u8], split_points: &[usize]) -> Vec<Vec<u8>> {
        if bytes.is_empty() {
            return Vec::new();
        }
        let mut points: Vec<usize> = split_points
            .iter()
            .map(|&p| p % (bytes.len() + 1))
            .collect();
        points.push(0);
        points.push(bytes.len());
        points.sort_unstable();
        points.dedup();
        points
            .windows(2)
            .map(|w| bytes[w[0]..w[1]].to_vec())
            .filter(|c| !c.is_empty())
            .collect()
    }

    /// Read a `ControlModeReader` to EOF using a cycling sequence of buffer
    /// sizes, so reassembly is exercised across arbitrary read boundaries.
    fn drain_reader(reader: &mut ControlModeReader, buf_sizes: &[usize]) -> Vec<u8> {
        let mut out = Vec::new();
        let mut i = 0;
        loop {
            let sz = buf_sizes[i % buf_sizes.len()].max(1);
            let mut buf = vec![0u8; sz];
            match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => out.extend_from_slice(&buf[..n]),
                Err(_) => break,
            }
            i += 1;
        }
        out
    }

    /// Parse our own `send-keys -t %1 -H XX XX …` command back into the bytes it
    /// encodes, to confirm the *input* (typed/pasted) path is lossless too.
    fn parse_send_keys_hex(cmd: &str) -> Vec<u8> {
        let cmd = cmd.strip_suffix('\n').expect("trailing newline");
        let rest = cmd
            .strip_prefix("send-keys -t %1 -H")
            .expect("send-keys prefix");
        rest.split_whitespace()
            .map(|h| u8::from_str_radix(h, 16).expect("hex byte"))
            .collect()
    }

    proptest! {
        /// `decode_octal` is the exact inverse of tmux's octal escaping for any
        /// byte sequence (all 256 values, escapes, and digit runs that merely
        /// look like octal).
        #[test]
        fn decode_octal_inverts_tmux_encoding(bytes in prop::collection::vec(any::<u8>(), 0..512)) {
            let encoded = tmux_octal_encode(&bytes);
            prop_assert_eq!(decode_octal(&encoded), bytes);
        }

        /// `ControlModeReader` reassembles a chunked byte stream identically,
        /// regardless of how the stream is chunked or what read buffer sizes the
        /// consumer uses.
        #[test]
        fn control_mode_reader_reassembles_losslessly(
            chunks in prop::collection::vec(prop::collection::vec(any::<u8>(), 1..64), 0..32),
            buf_sizes in prop::collection::vec(1usize..40, 1..16),
        ) {
            let expected: Vec<u8> = chunks.iter().flatten().copied().collect();
            let (tx, rx) = channel();
            for c in &chunks {
                tx.send(c.clone()).unwrap();
            }
            drop(tx);
            let mut reader = ControlModeReader::new(rx);
            let got = drain_reader(&mut reader, &buf_sizes);
            prop_assert_eq!(got, expected);
        }

        /// The full transport — agent bytes → tmux octal `%output` lines →
        /// `parse_notification` → `decode_octal` → mpsc channel →
        /// `ControlModeReader` — is the identity function on the byte stream,
        /// for arbitrary bytes split across arbitrary `%output` boundaries and
        /// drained with arbitrary read sizes.
        #[test]
        fn full_transport_is_byte_identity(
            bytes in prop::collection::vec(any::<u8>(), 0..512),
            split_points in prop::collection::vec(any::<usize>(), 0..16),
            buf_sizes in prop::collection::vec(1usize..40, 1..16),
        ) {
            let chunks = chunk_bytes(&bytes, &split_points);
            let (tx, rx) = channel();
            for chunk in &chunks {
                let line = format!("%output %1 {}", tmux_octal_encode(chunk));
                match parse_notification(&line) {
                    Notification::Output { pane_id, data } => {
                        prop_assert_eq!(pane_id, "%1");
                        if !data.is_empty() {
                            tx.send(data).unwrap();
                        }
                    }
                    other => prop_assert!(false, "expected Output, got {:?}", other),
                }
            }
            drop(tx);
            let mut reader = ControlModeReader::new(rx);
            let got = drain_reader(&mut reader, &buf_sizes);
            prop_assert_eq!(got, bytes);
        }

        /// `format_send_keys` (the `send-keys -H` hex encoding used for every
        /// typed/pasted byte) round-trips losslessly.
        #[test]
        fn format_send_keys_round_trips(bytes in prop::collection::vec(any::<u8>(), 0..256)) {
            let cmd = format_send_keys("%1", &bytes);
            prop_assert_eq!(parse_send_keys_hex(&cmd), bytes);
        }
    }
}
