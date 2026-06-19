//! Shared tmux control mode I/O infrastructure.
//!
//! This module contains the transport-agnostic control mode parsing and I/O types
//! used by both `LocalTmuxBackend` (local tmux) and `QemuVmBackend` (SSH into VM).
//! The tmux control mode protocol is identical whether accessed locally or over SSH.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::sync::mpsc::SyncSender;
use std::sync::{Arc, Mutex};

/// Per-pane output channel capacity. Sized large enough to buffer heavy output
/// bursts; chunks are dropped (not blocked) when full to keep the reader thread alive.
pub const PANE_CHANNEL_CAPACITY: usize = 4096;

/// Type alias for pane sender broadcast map.
/// Maps pane IDs to vectors of sync senders for multi-instance output broadcast.
pub type PaneSendersMap = HashMap<String, Vec<SyncSender<Vec<u8>>>>;
pub type PaneSendersMapShared = Arc<Mutex<PaneSendersMap>>;

/// Response from a tmux control mode command.
pub struct CommandResponse {
    pub lines: Vec<String>,
    pub is_error: bool,
}

/// Parsed notification from the tmux control mode protocol.
#[derive(Debug, PartialEq)]
pub enum Notification {
    Output { pane_id: String, data: Vec<u8> },
    Begin,
    End,
    Error,
    Pause { pane_id: String },
    Other(String),
}

/// Per-pane reader that receives output via an mpsc channel.
///
/// Implements `Read` so it plugs directly into the existing `Session::reader_loop`.
pub struct ControlModeReader {
    receiver: std::sync::mpsc::Receiver<Vec<u8>>,
    buffer: Vec<u8>,
    pos: usize,
}

impl ControlModeReader {
    pub fn new(receiver: std::sync::mpsc::Receiver<Vec<u8>>) -> Self {
        Self {
            receiver,
            buffer: Vec::new(),
            pos: 0,
        }
    }
}

impl Read for ControlModeReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        // Drain leftover buffered data first.
        if self.pos < self.buffer.len() {
            let remaining = &self.buffer[self.pos..];
            let n = remaining.len().min(buf.len());
            buf[..n].copy_from_slice(&remaining[..n]);
            self.pos += n;
            if self.pos == self.buffer.len() {
                self.buffer.clear();
                self.pos = 0;
            }
            return Ok(n);
        }

        // Block until the next chunk arrives.
        match self.receiver.recv() {
            Ok(data) => {
                let n = data.len().min(buf.len());
                buf[..n].copy_from_slice(&data[..n]);
                if n < data.len() {
                    self.buffer = data;
                    self.pos = n;
                }
                Ok(n)
            }
            Err(_) => Ok(0), // Channel closed → EOF.
        }
    }
}

/// Per-pane writer that sends input via `send-keys -H` through the shared control stdin.
pub struct ControlModeWriter {
    pub stdin: Arc<Mutex<std::process::ChildStdin>>,
    pub pane_id: String,
}

impl Write for ControlModeWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        let cmd = format_send_keys(&self.pane_id, buf);
        let mut stdin = self
            .stdin
            .lock()
            .map_err(|e| std::io::Error::other(format!("stdin lock: {e}")))?;
        stdin.write_all(cmd.as_bytes())?;
        stdin.flush()?;
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Decode tmux control mode octal escapes in `%output` data.
///
/// Scans for `\` followed by exactly 3 octal digits (0-7). Emits the decoded byte.
/// All other characters pass through unchanged.
pub fn decode_octal(input: &str) -> Vec<u8> {
    let mut result = Vec::with_capacity(input.len());
    let bytes = input.as_bytes();
    let mut i = 0;

    while i < bytes.len() {
        if bytes[i] == b'\\' && i + 3 < bytes.len() {
            let d0 = bytes[i + 1];
            let d1 = bytes[i + 2];
            let d2 = bytes[i + 3];
            if is_octal(d0) && is_octal(d1) && is_octal(d2) {
                let val = (d0 - b'0') as u16 * 64 + (d1 - b'0') as u16 * 8 + (d2 - b'0') as u16;
                result.push(val as u8);
                i += 4;
                continue;
            }
        }
        result.push(bytes[i]);
        i += 1;
    }

    result
}

fn is_octal(b: u8) -> bool {
    (b'0'..=b'7').contains(&b)
}

/// Parse a line from tmux control mode into a notification.
pub fn parse_notification(line: &str) -> Notification {
    if let Some(rest) = line.strip_prefix("%output ") {
        // Format: %output %<pane_id> <octal-encoded data>
        if let Some(space_idx) = rest.find(' ') {
            let pane_id = rest[..space_idx].to_string();
            let data = decode_octal(&rest[space_idx + 1..]);
            return Notification::Output { pane_id, data };
        }
    }

    if let Some(rest) = line.strip_prefix("%extended-output ") {
        // Format: %extended-output %<pane_id> <age> : <octal-encoded data>
        // The " : " separator divides metadata from payload.
        if let Some(colon_idx) = rest.find(" : ") {
            let meta = &rest[..colon_idx];
            let data = decode_octal(&rest[colon_idx + 3..]);
            // meta is "%<pane_id> <age>" — extract pane_id.
            if let Some(space_idx) = meta.find(' ') {
                let pane_id = meta[..space_idx].to_string();
                return Notification::Output { pane_id, data };
            }
        }
    }

    if line.starts_with("%begin ") {
        return Notification::Begin;
    }

    if line.starts_with("%end ") {
        return Notification::End;
    }

    if line.starts_with("%error ") {
        return Notification::Error;
    }

    if let Some(rest) = line.strip_prefix("%pause ") {
        // Format: %pause %<pane_id>
        return Notification::Pause {
            pane_id: rest.trim().to_string(),
        };
    }

    Notification::Other(line.to_string())
}

/// A tmux pane id is `%<digits>`. Pane ids are interpolated unquoted into
/// control-mode commands (`send-keys -t`, `kill-pane -t`, …), so anything
/// else must be rejected where ids enter the system (spawn/adopt/discover).
pub fn is_valid_pane_id(s: &str) -> bool {
    s.strip_prefix('%')
        .is_some_and(|rest| !rest.is_empty() && rest.bytes().all(|b| b.is_ascii_digit()))
}

/// Format a `send-keys -H` command for a pane.
///
/// Each byte is encoded as two hex digits.
pub fn format_send_keys(pane_id: &str, bytes: &[u8]) -> String {
    use std::fmt::Write;
    // "send-keys -t %NN -H" + " XX" per byte + "\n"
    let mut cmd = String::with_capacity(20 + pane_id.len() + bytes.len() * 3 + 1);
    write!(cmd, "send-keys -t {pane_id} -H").unwrap();
    for &b in bytes {
        write!(cmd, " {b:02x}").unwrap();
    }
    cmd.push('\n');
    cmd
}

/// Shell-escape a string for safe inclusion in a tmux control mode command.
///
/// Tmux control mode is line-delimited — each `\n` starts a new command.
/// Literal newlines in arguments (e.g. `--append-system-prompt`) would split
/// the command and corrupt the protocol, so they are replaced with spaces.
pub fn shell_escape(s: &str) -> String {
    // Strip the protocol-breaking newlines, then apply standard POSIX
    // single-quote escaping (shared with the SSH/git paths).
    crate::shell::posix_quote(&s.replace('\n', " "))
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc::sync_channel;

    use super::*;

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
        assert_eq!(decode_octal("\\033"), vec![27]); // ESC
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
        assert_eq!(n, 0); // EOF
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
            Err(std::sync::mpsc::TrySendError::Full(_)) => {} // expected
            other => panic!("Expected TrySendError::Full, got: {other:?}"),
        }
    }

    // Compile-time check: channel capacity must be large enough to buffer heavy output.
    const _: () = assert!(PANE_CHANNEL_CAPACITY >= 1024);
}

/// Property/fuzz tests proving the tmux control-mode **transport** is byte
/// transparent: whatever the agent writes is exactly what comes out of
/// [`decode_octal`] + [`ControlModeReader`], regardless of how tmux escapes it
/// or how the byte stream is chunked. If these stay green, thurbox's transport
/// layer cannot be the source of glitched/stray characters in the rendered pane.
#[cfg(test)]
mod transport_proptests {
    use std::fmt::Write as _;
    use std::io::Read;
    use std::sync::mpsc::channel;

    use proptest::prelude::*;

    use super::{
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
