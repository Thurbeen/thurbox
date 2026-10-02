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

#[test]
fn pane_ids_parse_keeps_every_listed_pane_whatever_its_pid() {
    let ids = parse_pane_ids("%1\n%2 \n not-a-pane\n\n %3 \n%\n");
    let mut ids: Vec<_> = ids.into_iter().collect();
    ids.sort();
    assert_eq!(ids, ["%1", "%2", "%3"]);
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
    assert_eq!(decode_octal(b"\\033"), vec![27]);
}

#[test]
fn decode_octal_backslash() {
    assert_eq!(decode_octal(b"\\134"), vec![b'\\']);
}

#[test]
fn decode_octal_newline() {
    assert_eq!(decode_octal(b"\\012"), vec![b'\n']);
}

#[test]
fn decode_octal_passthrough() {
    assert_eq!(decode_octal(b"hello"), b"hello");
}

#[test]
fn decode_octal_incomplete() {
    assert_eq!(decode_octal(b"\\01"), b"\\01");
}

#[test]
fn decode_octal_non_octal_digits() {
    assert_eq!(decode_octal(b"\\089"), b"\\089");
}

#[test]
fn decode_octal_mixed() {
    assert_eq!(
        decode_octal(b"A\\033[1mB"),
        vec![b'A', 27, b'[', b'1', b'm', b'B']
    );
}

#[test]
fn decode_octal_consecutive() {
    assert_eq!(decode_octal(b"\\033\\033"), vec![27, 27]);
}

#[test]
fn decode_octal_empty() {
    assert_eq!(decode_octal(b""), b"");
}

#[test]
fn decode_octal_trailing_backslash() {
    assert_eq!(decode_octal(b"a\\"), b"a\\");
}

#[test]
fn decode_octal_max_value() {
    assert_eq!(decode_octal(b"\\377"), vec![0xFF]);
}

#[test]
fn decode_octal_overflow_wraps() {
    assert_eq!(decode_octal(b"\\400"), vec![0u8]);
}

// --- window close → reader EOF ---

/// The point of the notification: a closed window must give the readers of its
/// panes an EOF, because EOF is the only thing that sets `exited`, and `exited`
/// is what `program.exited` and `start_program`'s "replace a finished slot"
/// both stand on.
#[test]
fn window_close_gives_its_panes_eof() {
    let senders: PaneSendersMapShared = Arc::new(Mutex::new(HashMap::new()));
    let windows: PaneWindowsMapShared = Arc::new(Mutex::new(HashMap::new()));
    let (tx, rx) = sync_channel(4);
    senders.lock().unwrap().insert("%7".to_string(), vec![tx]);
    windows
        .lock()
        .unwrap()
        .insert("%7".to_string(), "@3".to_string());
    let mut reader = ControlModeReader::new(rx);

    ControlMode::close_window_panes(&senders, &windows, "@3");

    let mut buf = [0u8; 16];
    assert_eq!(reader.read(&mut buf).unwrap(), 0, "reader should see EOF");
    assert!(senders.lock().unwrap().is_empty());
    assert!(windows.lock().unwrap().is_empty());
}

/// Somebody else's window closing must not take our panes with it — on a shared
/// tmux server most closes are not ours.
#[test]
fn window_close_leaves_other_windows_alone() {
    let senders: PaneSendersMapShared = Arc::new(Mutex::new(HashMap::new()));
    let windows: PaneWindowsMapShared = Arc::new(Mutex::new(HashMap::new()));
    let (tx, rx) = sync_channel(4);
    senders.lock().unwrap().insert("%7".to_string(), vec![tx]);
    windows
        .lock()
        .unwrap()
        .insert("%7".to_string(), "@3".to_string());
    let mut reader = ControlModeReader::new(rx);

    ControlMode::close_window_panes(&senders, &windows, "@9");

    ControlMode::dispatch_output(&senders, "%7", b"still here".to_vec());
    let mut buf = [0u8; 16];
    assert_eq!(reader.read(&mut buf).unwrap(), 10);
    assert_eq!(&buf[..10], b"still here");
}

// --- layout change → the reader's grid size ---

#[test]
fn layout_change_reads_the_window_and_its_size() {
    assert_eq!(
        parse_notification("%layout-change @1 a87e,100x30,0,0,1 a87e,100x30,0,0,1 *"),
        Notification::LayoutChange {
            window_id: "@1".to_string(),
            rows: 30,
            cols: 100,
        }
    );
    // A multi-pane layout nests, but its first size is still the window's.
    assert_eq!(
        parse_notification("%layout-change @4 d0a3,160x45,0,0{80x45,0,0,7,79x45,81,0,8} x"),
        Notification::LayoutChange {
            window_id: "@4".to_string(),
            rows: 45,
            cols: 160,
        }
    );
    for garbled in [
        "%layout-change ",
        "%layout-change @1",
        "%layout-change 1 a87e,100x30,0,0,1",
        "%layout-change @1 a87e",
        "%layout-change @1 a87e,100by30,0,0,1",
    ] {
        assert!(
            matches!(parse_notification(garbled), Notification::Other(_)),
            "{garbled}"
        );
    }
}

/// A size reaches the readers of the window's panes as an interrupted read of
/// its own, after the output queued before it — and only those readers.
#[test]
fn a_window_resize_interrupts_its_panes_readers_in_order() {
    let senders: PaneSendersMapShared = Arc::new(Mutex::new(HashMap::new()));
    let windows: PaneWindowsMapShared = Arc::new(Mutex::new(HashMap::new()));
    let (tx, rx) = sync_channel(8);
    let (other_tx, other_rx) = sync_channel(8);
    senders.lock().unwrap().insert("%7".to_string(), vec![tx]);
    senders
        .lock()
        .unwrap()
        .insert("%8".to_string(), vec![other_tx]);
    windows
        .lock()
        .unwrap()
        .insert("%7".to_string(), "@3".to_string());
    windows
        .lock()
        .unwrap()
        .insert("%8".to_string(), "@4".to_string());
    let mut reader = ControlModeReader::new(rx);
    let sizes: PaneSizesMapShared = Arc::default();

    ControlMode::dispatch_output(&senders, "%7", b"old".to_vec());
    ControlMode::dispatch_resize(&senders, &windows, &sizes, "@3", 30, 100);
    ControlMode::dispatch_output(&senders, "%7", b"new".to_vec());

    let mut buf = [0u8; 16];
    assert_eq!(reader.read(&mut buf).unwrap(), 3);
    assert_eq!(&buf[..3], b"old");
    let err = reader.read(&mut buf).unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::Interrupted);
    assert_eq!(reader.read(&mut buf).unwrap(), 3);
    assert_eq!(&buf[..3], b"new");
    assert!(
        other_rx.try_recv().is_err(),
        "another window's pane was told"
    );
}

/// A size is never dropped. A pane whose channel is full has its size put
/// straight where its reader applies sizes, at the next read boundary — a
/// little early for the bytes still queued, where dropping it left a resident
/// grid at the old size with nothing to correct it.
#[test]
fn a_resize_into_a_full_channel_still_reaches_the_reader() {
    let senders: PaneSendersMapShared = Arc::new(Mutex::new(HashMap::new()));
    let windows: PaneWindowsMapShared = Arc::new(Mutex::new(HashMap::new()));
    let (tx, rx) = sync_channel(1);
    senders.lock().unwrap().insert("%7".to_string(), vec![tx]);
    windows
        .lock()
        .unwrap()
        .insert("%7".to_string(), "@3".to_string());
    let reader = ControlModeReader::new(rx);
    let sizes: PaneSizesMapShared = Arc::default();
    sizes
        .lock()
        .unwrap()
        .insert("%7".to_string(), reader.size());

    ControlMode::dispatch_output(&senders, "%7", b"fills it".to_vec());
    ControlMode::dispatch_resize(&senders, &windows, &sizes, "@3", 30, 100);

    assert_eq!(reader.size().last_reported(), Some((30, 100)));
}

/// Who sizes a pane is a hint, not a point in the stream: it is noted and the
/// read carries on to the next output.
#[test]
fn a_sizer_change_does_not_interrupt_the_read() {
    let (tx, rx) = sync_channel(8);
    let mut reader = ControlModeReader::new(rx);
    let size = reader.size();
    tx.send(PaneChunk::SizedElsewhere(true)).unwrap();
    tx.send(PaneChunk::Output(b"x".to_vec())).unwrap();
    let mut buf = [0u8; 4];
    assert_eq!(reader.read(&mut buf).unwrap(), 1);
    assert!(size.sized_elsewhere());
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

/// tmux cuts a pane's output wherever its read ended, here inside the `и` of
/// `мир` (`d0 b8`): each half must come out as the raw byte it was, so the two
/// rejoin downstream, not as the U+FFFD a text decode would make of each.
#[test]
fn parse_output_keeps_a_character_split_across_two_lines() {
    let halves = [
        &b"%output %0 \\015\\012\xd0\xbc\xd0"[..],
        b"%output %0 \xb8\xd1\x80",
    ];
    let data: Vec<u8> = halves
        .iter()
        .flat_map(|line| match parse_output(line) {
            Some(Notification::Output { pane_id, data }) => {
                assert_eq!(pane_id, "%0");
                data
            }
            other => panic!("not output: {other:?}"),
        })
        .collect();
    assert_eq!(std::str::from_utf8(&data).unwrap(), "\r\nмир");
    match parse_output(b"%extended-output %3 12 : \xd1\x80\\033") {
        Some(Notification::Output { pane_id, data }) => {
            assert_eq!(pane_id, "%3");
            assert_eq!(data, b"\xd1\x80\x1b");
        }
        other => panic!("not output: {other:?}"),
    }
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

/// Both spellings of a window's death mean the same thing, and the one that
/// actually arrives when a program exits on its own is the UNLINKED one
/// (measured against tmux control mode, 2026-09-11). Parsing only
/// `%window-close` would leave every ordinary exit unnoticed.
#[test]
fn parse_window_close_both_spellings() {
    assert_eq!(
        parse_notification("%window-close @3"),
        Notification::WindowClose {
            window_id: "@3".to_string()
        }
    );
    assert_eq!(
        parse_notification("%unlinked-window-close @3"),
        Notification::WindowClose {
            window_id: "@3".to_string()
        }
    );
}

/// Some tmux versions append a layout to the close line; the id is the first
/// token either way, and the rest is not ours to interpret.
#[test]
fn parse_window_close_ignores_trailing_fields() {
    assert_eq!(
        parse_notification("%window-close @7 80x24,0,0,1"),
        Notification::WindowClose {
            window_id: "@7".to_string()
        }
    );
}

/// A close with no id is not a close: it names no window, so acting on it
/// would mean guessing which one died.
#[test]
fn parse_window_close_without_an_id_is_other() {
    assert_eq!(
        parse_notification("%window-close "),
        Notification::Other("%window-close ".to_string())
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

// --- hex_send_keys_commands chunking tests ---

#[test]
fn hex_send_keys_commands_short_input_is_one_command() {
    let cmds = hex_send_keys_commands("%1", b"ABC");
    assert_eq!(cmds, vec!["send-keys -t %1 -H 41 42 43\n".to_string()]);
}

#[test]
fn hex_send_keys_commands_empty_input_is_no_commands() {
    assert!(hex_send_keys_commands("%1", &[]).is_empty());
}

/// A large paste is split into multiple bounded `send-keys` commands whose
/// concatenated bytes equal the original input — the property that keeps a
/// big paste from being truncated by tmux's per-command line limit.
#[test]
fn hex_send_keys_commands_chunks_large_input_losslessly() {
    // 5 KB of bracketed-paste-wrapped content, like `send_paste_to_session`.
    let mut input = b"\x1b[200~".to_vec();
    input.extend((0..5000u32).map(|i| (i % 256) as u8));
    input.extend_from_slice(b"\x1b[201~");

    let cmds = hex_send_keys_commands("%1", &input);

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

// --- bracketed paste payloads ---

#[test]
fn tmux_quote_keeps_a_line_on_one_line() {
    assert_eq!(
        tmux_quote("a\nb\r\tc \"d\" $HOME ~ \\ é\x1b\u{9b}"),
        "\"a\\nb\\r\\tc \\\"d\\\" \\$HOME \\~ \\\\ é\\033\\u009b\""
    );
}

/// The buffer a paste's commands name, which every one of them must agree on.
fn paste_buffer_of(cmds: &[String]) -> &str {
    let last = cmds.last().expect("a paste-buffer line");
    let name = last
        .split(" -b ")
        .nth(1)
        .unwrap()
        .split(' ')
        .next()
        .unwrap();
    assert!(cmds.iter().all(|c| c.contains(&format!(" -b {name} "))));
    name
}

#[test]
fn a_tmux_paste_lets_tmux_decide_the_markers() {
    let cmds = tmux_paste_commands("%7", "-a\nb");
    let buffer = paste_buffer_of(&cmds).to_string();
    assert_eq!(
        cmds,
        vec![
            format!("set-buffer -b {buffer} -- \"-a\\nb\"\n"),
            format!("paste-buffer -d -p -r -b {buffer} -t %7\n"),
        ]
    );
}

#[test]
fn two_tmux_pastes_never_share_a_buffer() {
    // Two interfaces can paste into one pane over two connections, and their
    // `set-buffer -a` and `paste-buffer -d` lines then interleave on the
    // server: a buffer named after the pane alone mixed one paste into the
    // other, or was deleted before the second could paste it.
    let first = tmux_paste_commands("%7", "one");
    let second = tmux_paste_commands("%7", "two");
    assert_ne!(paste_buffer_of(&first), paste_buffer_of(&second));
}

#[test]
fn a_long_tmux_paste_is_appended_on_char_boundaries() {
    let text = "é".repeat(SET_BUFFER_CHUNK_BYTES);
    let cmds = tmux_paste_commands("%1", &text);
    assert!(cmds.len() > 2);
    assert!(cmds[0].starts_with("set-buffer -b "));
    assert!(cmds[1..cmds.len() - 1]
        .iter()
        .all(|c| c.starts_with("set-buffer -a -b ")));
    let joined: String = cmds[..cmds.len() - 1]
        .iter()
        .map(|c| c.split_once("-- \"").unwrap().1.trim_end_matches("\"\n"))
        .collect();
    assert_eq!(joined, text);
}

/// A server's input as a test sees it: keystrokes typed out one line per byte
/// run, CR spelt `Enter` the way psmux's key-names spell it, and a paste
/// answered with `paste`.
#[cfg(unix)]
struct FakeInput(Option<fn() -> Result<()>>);

#[cfg(unix)]
impl PaneInput for FakeInput {
    fn send_keys(&self, pane_id: &str, buf: &[u8]) -> Vec<String> {
        String::from_utf8_lossy(buf)
            .split_inclusive('\r')
            .map(|run| format!("send-keys -t {pane_id} {}\n", run.replace('\r', " Enter")))
            .collect()
    }

    fn paste(&self, _: &str, _: &str) -> Option<Result<()>> {
        self.0.map(|deliver| deliver())
    }
}

/// What a [`ControlModeWriter`] wrote to its control connection for `buf` —
/// the connection a `cat` standing in for the server copies to a file.
#[cfg(unix)]
fn written_by(input: FakeInput, buf: &[u8]) -> String {
    let dir = tempfile::tempdir().expect("tempdir");
    let out = dir.path().join("control");
    let mut cat = std::process::Command::new("cat")
        .stdin(std::process::Stdio::piped())
        .stdout(std::fs::File::create(&out).expect("create"))
        .spawn()
        .expect("spawn cat");
    let stdin = cat.stdin.take().expect("cat stdin");
    {
        let mut writer = ControlModeWriter {
            stdin: Arc::new(Mutex::new(stdin)),
            pane_id: "%1".to_string(),
            input: Arc::new(input),
        };
        writer.write_all(buf).expect("write");
    }
    cat.wait().expect("cat exits once its stdin closes");
    std::fs::read_to_string(&out).expect("read")
}

#[cfg(unix)]
#[test]
fn a_tmux_paste_goes_through_a_buffer_never_send_keys() {
    let sent = written_by(FakeInput(None), b"\x1b[200~echo a\recho b\x1b[201~");
    assert!(!sent.contains("send-keys"), "{sent}");
    assert!(sent.contains("paste-buffer -d -p -r"), "{sent}");
}

#[cfg(unix)]
#[test]
fn a_paste_that_cannot_go_out_of_band_presses_no_key() {
    // The key encoding is what the out-of-band channel exists to avoid: every
    // CR in it is `Enter`. Falling back to it when psmux's `send-paste` failed
    // ran each pasted line as it went.
    let failing = FakeInput(Some(|| Err(anyhow::anyhow!("send-paste failed"))));
    let sent = written_by(
        failing,
        b"\x1b[200~echo tb-pasted\recho tb-INJECTED\r\x1b[201~",
    );
    assert!(
        !sent.contains("Enter") && !sent.contains("tb-INJECTED"),
        "a failed out-of-band paste was typed out key by key:\n{sent}"
    );
}

#[cfg(unix)]
#[test]
fn a_paste_with_a_marker_inside_presses_no_key() {
    // A frame holding a second marker is not one paste, and was typed out
    // through the key encoding — where the CR after the early end marker is
    // `Enter`. Whatever the coordinator sanitised, the writer is the last
    // place that can refuse it.
    for input in [FakeInput(None), FakeInput(Some(|| Ok(())))] {
        let sent = written_by(
            input,
            b"\x1b[200~echo a\x1b[201~echo tb-INJECTED\r\x1b[201~",
        );
        assert!(
            !sent.contains("Enter") && !sent.contains("tb-INJECTED"),
            "a paste with an embedded end marker was typed out key by key:\n{sent}"
        );
    }
}

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

// --- ControlModeReader tests ---

#[test]
fn control_mode_reader_data_delivery() {
    let (tx, rx) = sync_channel(16);
    let mut reader = ControlModeReader::new(rx);

    tx.send(b"hello".to_vec().into()).unwrap();
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

    tx.send(b"hello world".to_vec().into()).unwrap();

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

    tx.send(b"aaa".to_vec().into()).unwrap();
    tx.send(b"bbb".to_vec().into()).unwrap();

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

    tx.send(b"abc".to_vec().into()).unwrap();
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

    use crate::backend::tmux_compat::control_mode::{
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
            prop_assert_eq!(decode_octal(encoded.as_bytes()), bytes);
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
                tx.send(c.clone().into()).unwrap();
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
                            tx.send(data.into()).unwrap();
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

// --- command lists on a real tmux ---

/// A tmux server on a throwaway socket, killed on drop. Real tmux because what
/// is pinned is how the server answers a command list — one `%begin`/`%end`
/// block per command that runs — not the reader's bookkeeping.
#[cfg(unix)]
struct ThrowawayServer {
    socket: String,
}

#[cfg(unix)]
impl ThrowawayServer {
    const SESSION: &str = "lists";

    /// `None` when tmux is absent or will not start a server: an environment
    /// fact, not a regression.
    fn start(name: &str) -> Option<Self> {
        let socket = format!("thurbox-cm-{name}-{}", std::process::id());
        let started = TmuxTransport::local("tmux")
            .tmux_command(
                &socket,
                &[
                    "new-session",
                    "-d",
                    "-s",
                    Self::SESSION,
                    "-x",
                    "80",
                    "-y",
                    "24",
                ],
            )
            .output();
        match started {
            Ok(out) if out.status.success() => Some(Self { socket }),
            _ => {
                eprintln!("skipping: tmux would not start a server");
                None
            }
        }
    }

    fn control(&self) -> ControlMode {
        ControlMode::start(
            &TmuxTransport::local("tmux"),
            &self.socket,
            Self::SESSION,
            "tests",
            &ControlPolicy {
                flow_control_command: Some("refresh-client -f pause-after=5"),
                implicit_attach_reply: true,
                tagged_blocks: true,
                subscriptions: true,
                status_poll: None,
            },
        )
        .expect("control mode starts")
    }
}

#[cfg(unix)]
impl Drop for ThrowawayServer {
    fn drop(&mut self) {
        let _ = TmuxTransport::local("tmux")
            .tmux_command(&self.socket, &["kill-server"])
            .output();
        // tmux does not unlink its socket when the server exits, so a killed
        // server still leaves a dead socket file in the shared socket
        // directory — one per test process, kept for good. The path is the
        // rule tmux itself applies (`$TMUX_TMPDIR` or `/tmp`, then
        // `tmux-<uid>/<name>`); this test cannot point `TMUX_TMPDIR`
        // somewhere private instead, because the lib's unit tests share one
        // process and the variable is process-wide.
        // Empty is not a directory, and tmux itself only honours the variable
        // when it is non-empty — matching that is what keeps this pointing at
        // the file tmux actually made.
        let tmpdir = std::env::var_os("TMUX_TMPDIR")
            .filter(|dir| !dir.is_empty())
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| std::path::PathBuf::from("/tmp"));
        // SAFETY: `getuid` is always successful and takes no arguments.
        let uid = unsafe { libc::getuid() };
        let _ = std::fs::remove_file(tmpdir.join(format!("tmux-{uid}")).join(&self.socket));
    }
}

/// The blocks after a list's first belong to the list. Here the middle one is
/// held open by `run-shell`, so a reader that answered the list at its first
/// `%end` has already let the next command queue up behind it — and that
/// command would be answered with the `run-shell` block's empty body (#1120).
#[cfg(unix)]
#[test]
fn a_command_list_is_answered_once_all_its_blocks_are_in() {
    let Some(server) = ThrowawayServer::start("list-blocks") else {
        return;
    };
    let ctrl = server.control();

    let list = ctrl
        .send_command_list(&[
            "display-message -p first",
            "run-shell 'sleep 0.5'",
            "display-message -p third",
        ])
        .expect("the list runs");
    let next = ctrl
        .send_command("display-message -p second")
        .expect("the next command runs");

    assert_eq!(
        next, "second",
        "the next command got another command's answer"
    );
    assert_eq!(list, "first\nthird");
}

/// tmux drops the rest of a list at its first error, so the list answers with
/// fewer blocks than it has commands. The error is the list's, and the command
/// after it still gets its own answer.
#[cfg(unix)]
#[test]
fn a_command_list_cut_short_by_an_error_fails_and_keeps_later_answers_in_place() {
    let Some(server) = ThrowawayServer::start("list-error") else {
        return;
    };
    let ctrl = server.control();

    let failed = ctrl.send_command_list(&[
        "display-message -p a",
        "set-window-option nosuchoption on",
        "display-message -p c",
    ]);
    let next = ctrl
        .send_command("display-message -p next")
        .expect("the next command runs");

    let err = failed.expect_err("a failing command fails its list");
    assert!(
        format!("{err:#}").contains("nosuchoption"),
        "the error is the failing command's: {err:#}"
    );
    assert_eq!(next, "next");
}

// --- the implicit attach response (ADR-13, issue #1168) ---

/// The drain takes the implicit block and **only** it: the bytes after `%end`
/// are the connection's, and eating them would lose the first notification.
#[test]
fn the_drain_stops_at_the_end_of_the_implicit_block() {
    let stream = "\
%begin 1789657328 1 1
%end 1789657328 1 1
%window-add @1
";
    let mut reader = std::io::Cursor::new(stream.as_bytes());
    ControlMode::drain_implicit_attach_response(&mut reader).expect("the block is consumed");

    let mut rest = String::new();
    reader.read_line(&mut rest).expect("read");
    assert_eq!(rest.trim_end(), "%window-add @1");
}

/// What psmux actually puts on the wire after an attach: a blank line, then
/// nothing until something is sent. Against a real pipe this is where the
/// drain blocks forever; a closed one is the same answer arriving as an error,
/// which is the "control mode closed before sending its implicit attach
/// response" seen on Windows. Either way it must not be asked of psmux.
#[test]
fn a_psmux_shaped_stream_never_satisfies_the_drain() {
    let mut reader = std::io::Cursor::new(b"\n".as_slice());
    let err = ControlMode::drain_implicit_attach_response(&mut reader)
        .expect_err("psmux sends no implicit block");
    assert!(
        format!("{err:#}").contains("before sending its implicit attach response"),
        "{err:#}"
    );
}

// --- pane snapshots ---

/// A command's output is written raw, so a pane showing a line that reads like
/// the protocol — `%end …`, `%output …` — hands that line to whatever captures
/// it. Inside a block it is the block's content: it must neither end the block
/// early (every later answer would go to the wrong waiter) nor be dispatched
/// as some pane's output.
#[cfg(unix)]
#[test]
fn a_captured_line_that_reads_like_the_protocol_is_only_content() {
    let Some(server) = ThrowawayServer::start("snap-lines") else {
        return;
    };
    let dir = tempfile::tempdir().expect("tempdir");
    let script = dir.path().join("lines");
    std::fs::write(
        &script,
        "%end 1789657328 7 1\n%output %1 hijacked\n%error 1789657328 8 1\nplain\n",
    )
    .expect("write");
    let out = TmuxTransport::local("tmux")
        .tmux_command(
            &server.socket,
            &[
                "new-window",
                "-P",
                "-F",
                "#{pane_id}",
                "-t",
                ThrowawayServer::SESSION,
                &format!("sh -c 'cat {}; exec sleep 100'", script.display()),
            ],
        )
        .output()
        .expect("new-window");
    let pane = String::from_utf8_lossy(&out.stdout).trim().to_string();
    let ctrl = server.control();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    let snapshot = loop {
        let snapshot = ctrl
            .ask_snapshot(&pane, 100)
            .and_then(PendingSnapshot::wait)
            .expect("the snapshot is answered");
        if snapshot.normal.iter().any(|line| line.contains("plain"))
            || std::time::Instant::now() > deadline
        {
            break snapshot;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    };
    let next = ctrl
        .send_command("display-message -p after")
        .expect("the next command runs");

    for line in ["%end 1789657328 7 1", "%output %1 hijacked", "plain"] {
        assert!(
            snapshot.normal.iter().any(|l| l.contains(line)),
            "{line:?} is in the capture: {:?}",
            snapshot.normal
        );
    }
    assert_eq!(next, "after", "the next command got its own answer");
}

#[test]
fn a_snapshot_is_read_from_its_three_blocks() {
    let blocks = |alternate: &str| {
        vec![
            vec![format!("80 24 5 3 {alternate}")],
            vec!["current".to_string(), "rows".to_string()],
            vec!["saved".to_string()],
        ]
    };
    let normal = parse_snapshot(blocks("0")).expect("a normal screen");
    assert_eq!((normal.cols, normal.rows, normal.cursor), (80, 24, (5, 3)));
    assert_eq!(normal.normal, vec!["current", "rows"]);
    assert_eq!(normal.alternate, None);

    // With the alternate screen up, the capture without `-a` is that screen and
    // the one with it is the normal screen behind.
    let alternate = parse_snapshot(blocks("1")).expect("an alternate screen");
    assert_eq!(alternate.normal, vec!["saved"]);
    assert_eq!(
        alternate.alternate,
        Some(vec!["current".to_string(), "rows".to_string()])
    );
}

#[test]
fn anything_but_a_snapshot_answer_is_not_read_as_one() {
    assert_eq!(parse_snapshot(Vec::new()), None);
    assert_eq!(
        parse_snapshot(vec![vec!["80 24".into()], vec![], vec![]]),
        None,
        "too few fields"
    );
    assert_eq!(
        parse_snapshot(vec![vec!["0 24 0 0 0".into()], vec![], vec![]]),
        None,
        "no width"
    );
    assert_eq!(
        parse_snapshot(vec![vec!["80 24 0 0 0".into()], vec![]]),
        None,
        "a block short"
    );
}

/// A snapshot reaches a pane's reader in the pane's own stream, between the
/// bytes before it and the bytes after, and comes out of `read` as an
/// interruption carrying it — so a reader that knows nothing of snapshots just
/// reads on.
#[test]
fn a_snapshot_comes_out_of_the_reader_between_the_bytes_around_it() {
    let (tx, rx) = sync_channel(16);
    let mut reader = ControlModeReader::new(rx);
    let snapshot = PaneSnapshot {
        cols: 80,
        rows: 24,
        cursor: (0, 0),
        normal: vec!["x".into()],
        alternate: None,
    };
    tx.send(b"before".to_vec().into()).unwrap();
    tx.send(PaneChunk::Snapshot(Box::new(snapshot.clone())))
        .unwrap();
    tx.send(b"after".to_vec().into()).unwrap();

    let mut buf = [0u8; 16];
    let n = reader.read(&mut buf).unwrap();
    assert_eq!(&buf[..n], b"before");
    let err = reader.read(&mut buf).expect_err("the snapshot interrupts");
    assert_eq!(err.kind(), std::io::ErrorKind::Interrupted);
    assert_eq!(SnapshotArrived::take(err).as_deref(), Some(&snapshot));
    let n = reader.read(&mut buf).unwrap();
    assert_eq!(&buf[..n], b"after");
}
