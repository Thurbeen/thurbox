use std::io::{Read, Write};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};

use anyhow::{Context, Result};
use portable_pty::{native_pty_system, CommandBuilder, MasterPty, PtySize};
use tokio::sync::mpsc;
use tracing::{debug, error, warn};

use crate::session::{SessionConfig, SessionInfo};

// Terminal capability query sequences sent by child processes (e.g., Claude Code).
// See: https://vt100.net/docs/vt510-rm/DA1.html, DA2, and kitty keyboard protocol.
const DA1_QUERY: &[u8] = b"\x1b[c";
const DA1_QUERY_EXPLICIT: &[u8] = b"\x1b[0c";
const DA1_RESPONSE: &[u8] = b"\x1b[?62;22c"; // VT220 with color support

const DA2_QUERY: &[u8] = b"\x1b[>c";
const DA2_QUERY_EXPLICIT: &[u8] = b"\x1b[>0c";
const DA2_RESPONSE: &[u8] = b"\x1b[>1;279;0c"; // xterm version 279

const KITTY_KEYBOARD_QUERY: &[u8] = b"\x1b[?u";
const KITTY_KEYBOARD_RESPONSE: &[u8] = b"\x1b[?0u"; // flags=0, query acknowledged

pub struct PtySession {
    pub info: SessionInfo,
    pub parser: Arc<Mutex<vt100::Parser>>,
    input_tx: mpsc::UnboundedSender<Vec<u8>>,
    master: Box<dyn MasterPty + Send>,
    exited: Arc<AtomicBool>,
}

impl PtySession {
    pub fn spawn_with_config(
        name: String,
        rows: u16,
        cols: u16,
        config: &SessionConfig,
    ) -> Result<Self> {
        let pty_system = native_pty_system();
        let pair = pty_system
            .openpty(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .context("Failed to open PTY")?;

        let mut cmd = CommandBuilder::new("claude");
        if let Some(ref session_id) = config.resume_session_id {
            cmd.arg("--resume");
            cmd.arg(session_id);
        } else if let Some(ref session_id) = config.claude_session_id {
            cmd.arg("--session-id");
            cmd.arg(session_id);
        }
        if let Some(ref cwd) = config.cwd {
            cmd.cwd(cwd);
        }
        cmd.env("TERM", "xterm-256color");

        let child = pair
            .slave
            .spawn_command(cmd)
            .context("Failed to spawn claude. Is the `claude` CLI installed?")?;
        drop(pair.slave);

        let reader = pair
            .master
            .try_clone_reader()
            .context("Failed to clone PTY reader")?;
        let writer = pair
            .master
            .take_writer()
            .context("Failed to take PTY writer")?;

        let parser = Arc::new(Mutex::new(vt100::Parser::new(rows, cols, 1000)));
        let exited = Arc::new(AtomicBool::new(false));

        // Writer task: receives input bytes and writes to PTY
        let (input_tx, input_rx) = mpsc::unbounded_channel();
        tokio::spawn(Self::writer_loop(writer, input_rx));

        // Reader task: reads PTY output and feeds into vt100 parser
        let parser_clone = Arc::clone(&parser);
        let exited_clone = Arc::clone(&exited);
        let response_tx = input_tx.clone();
        tokio::task::spawn_blocking(move || {
            Self::reader_loop(reader, parser_clone, exited_clone, child, response_tx);
        });

        let mut info = SessionInfo::new(name);
        info.claude_session_id = config.claude_session_id.clone();
        info.cwd = config.cwd.clone();
        debug!(session_id = %info.id, "Spawned claude PTY session");

        Ok(Self {
            info,
            parser,
            input_tx,
            master: pair.master,
            exited,
        })
    }

    fn reader_loop(
        mut reader: Box<dyn Read + Send>,
        parser: Arc<Mutex<vt100::Parser>>,
        exited: Arc<AtomicBool>,
        mut child: Box<dyn portable_pty::Child + Send + Sync>,
        response_tx: mpsc::UnboundedSender<Vec<u8>>,
    ) {
        let mut buf = [0u8; 4096];
        loop {
            match reader.read(&mut buf) {
                Ok(0) => {
                    debug!("PTY reader: EOF");
                    break;
                }
                Ok(n) => {
                    let data = &buf[..n];
                    Self::respond_to_queries(data, &response_tx);
                    if let Ok(mut p) = parser.lock() {
                        p.process(data);
                    }
                }
                Err(e) => {
                    debug!("PTY reader error: {e}");
                    break;
                }
            }
        }

        // Wait for child to fully exit
        match child.try_wait() {
            Ok(Some(status)) => debug!("Claude process exited: {status:?}"),
            Ok(None) => {
                debug!("Claude process still running after EOF, waiting...");
                match child.wait() {
                    Ok(status) => debug!("Claude process exited: {status:?}"),
                    Err(e) => warn!("Error waiting for claude process: {e}"),
                }
            }
            Err(e) => warn!("Error checking claude process status: {e}"),
        }

        exited.store(true, Ordering::SeqCst);
    }

    /// Scan PTY output for terminal capability queries and send responses.
    ///
    /// Claude Code (via Ink) sends DA1, DA2, and kitty keyboard protocol
    /// queries to detect terminal capabilities. Without responses, it falls
    /// back to basic mode and ignores permission settings like "Accept Edits".
    fn respond_to_queries(data: &[u8], tx: &mpsc::UnboundedSender<Vec<u8>>) {
        if contains_sequence(data, DA1_QUERY) || contains_sequence(data, DA1_QUERY_EXPLICIT) {
            let _ = tx.send(DA1_RESPONSE.to_vec());
            debug!("Responded to DA1 query");
        }

        if contains_sequence(data, DA2_QUERY) || contains_sequence(data, DA2_QUERY_EXPLICIT) {
            let _ = tx.send(DA2_RESPONSE.to_vec());
            debug!("Responded to DA2 query");
        }

        if contains_sequence(data, KITTY_KEYBOARD_QUERY) {
            let _ = tx.send(KITTY_KEYBOARD_RESPONSE.to_vec());
            debug!("Responded to kitty keyboard protocol query");
        }
    }

    async fn writer_loop(
        mut writer: Box<dyn Write + Send>,
        mut input_rx: mpsc::UnboundedReceiver<Vec<u8>>,
    ) {
        while let Some(data) = input_rx.recv().await {
            if let Err(e) = writer.write_all(&data) {
                error!("PTY writer error: {e}");
                break;
            }
            if let Err(e) = writer.flush() {
                error!("PTY flush error: {e}");
                break;
            }
        }
        debug!("PTY writer task exiting");
    }

    pub fn send_input(&self, data: Vec<u8>) -> Result<()> {
        self.input_tx
            .send(data)
            .map_err(|_| anyhow::anyhow!("PTY input channel closed"))
    }

    pub fn resize(&self, rows: u16, cols: u16) {
        if let Err(e) = self.master.resize(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        }) {
            warn!("Failed to resize PTY: {e}");
            return;
        }
        if let Ok(mut parser) = self.parser.lock() {
            parser.screen_mut().set_size(rows, cols);
        }
    }

    pub fn has_exited(&self) -> bool {
        self.exited.load(Ordering::SeqCst)
    }

    /// Graceful shutdown: drop the input sender (closes the writer task)
    /// and drop the master PTY (sends SIGHUP to the child).
    pub fn shutdown(self) {
        drop(self.input_tx);
        drop(self.master);
        debug!("PTY session shut down");
    }

    /// Create a lightweight stub for unit tests (no real PTY process).
    #[cfg(test)]
    pub fn stub(name: &str) -> Self {
        let (input_tx, _input_rx) = mpsc::unbounded_channel();
        let pty_system = native_pty_system();
        let pair = pty_system
            .openpty(PtySize {
                rows: 24,
                cols: 80,
                pixel_width: 0,
                pixel_height: 0,
            })
            .expect("test: open PTY");
        drop(pair.slave);
        Self {
            info: SessionInfo::new(name.to_string()),
            parser: Arc::new(Mutex::new(vt100::Parser::new(24, 80, 0))),
            input_tx,
            master: pair.master,
            exited: Arc::new(AtomicBool::new(false)),
        }
    }
}

fn contains_sequence(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contains_sequence_exact_match() {
        assert!(contains_sequence(DA1_QUERY, DA1_QUERY));
    }

    #[test]
    fn contains_sequence_embedded() {
        assert!(contains_sequence(b"hello\x1b[cworld", DA1_QUERY));
    }

    #[test]
    fn contains_sequence_no_match() {
        assert!(!contains_sequence(b"hello", DA1_QUERY));
    }

    #[test]
    fn contains_sequence_empty_haystack() {
        assert!(!contains_sequence(b"", DA1_QUERY));
    }

    #[test]
    fn respond_to_da1_query() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        PtySession::respond_to_queries(DA1_QUERY, &tx);
        assert_eq!(rx.try_recv().unwrap(), DA1_RESPONSE);
    }

    #[test]
    fn respond_to_da1_query_explicit_zero() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        PtySession::respond_to_queries(DA1_QUERY_EXPLICIT, &tx);
        assert_eq!(rx.try_recv().unwrap(), DA1_RESPONSE);
    }

    #[test]
    fn respond_to_da2_query() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        PtySession::respond_to_queries(DA2_QUERY, &tx);
        assert_eq!(rx.try_recv().unwrap(), DA2_RESPONSE);
    }

    #[test]
    fn respond_to_kitty_keyboard_query() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        PtySession::respond_to_queries(KITTY_KEYBOARD_QUERY, &tx);
        assert_eq!(rx.try_recv().unwrap(), KITTY_KEYBOARD_RESPONSE);
    }

    #[test]
    fn no_response_for_normal_data() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        PtySession::respond_to_queries(b"hello world", &tx);
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn respond_to_da2_query_explicit_zero() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        PtySession::respond_to_queries(DA2_QUERY_EXPLICIT, &tx);
        assert_eq!(rx.try_recv().unwrap(), DA2_RESPONSE);
    }

    #[test]
    fn respond_to_query_embedded_in_output() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        PtySession::respond_to_queries(b"some text\x1b[?umore text", &tx);
        assert_eq!(rx.try_recv().unwrap(), KITTY_KEYBOARD_RESPONSE);
    }

    #[test]
    fn respond_to_multiple_queries_in_single_buffer() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        // Buffer containing both DA1 and kitty queries
        let mut data = Vec::new();
        data.extend_from_slice(DA1_QUERY);
        data.extend_from_slice(b"output");
        data.extend_from_slice(KITTY_KEYBOARD_QUERY);
        PtySession::respond_to_queries(&data, &tx);
        assert_eq!(rx.try_recv().unwrap(), DA1_RESPONSE);
        assert_eq!(rx.try_recv().unwrap(), KITTY_KEYBOARD_RESPONSE);
    }

    #[test]
    fn no_response_for_partial_escape_sequence() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        // Just ESC [ without the trailing 'c' — should not trigger DA1
        PtySession::respond_to_queries(b"\x1b[", &tx);
        assert!(rx.try_recv().is_err());
    }
}
