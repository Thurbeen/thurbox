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

pub struct PtySession {
    pub info: SessionInfo,
    pub parser: Arc<Mutex<vt100::Parser>>,
    input_tx: mpsc::UnboundedSender<Vec<u8>>,
    master: Box<dyn MasterPty + Send>,
    pub exited: Arc<AtomicBool>,
}

impl PtySession {
    pub fn spawn(name: String, rows: u16, cols: u16) -> Result<Self> {
        Self::spawn_with_config(name, rows, cols, &SessionConfig::default())
    }

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

        // Reader task: reads PTY output and feeds into vt100 parser
        let parser_clone = Arc::clone(&parser);
        let exited_clone = Arc::clone(&exited);
        tokio::task::spawn_blocking(move || {
            Self::reader_loop(reader, parser_clone, exited_clone, child);
        });

        // Writer task: receives input bytes and writes to PTY
        let (input_tx, input_rx) = mpsc::unbounded_channel();
        tokio::spawn(Self::writer_loop(writer, input_rx));

        let info = SessionInfo::new(name);
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
    ) {
        let mut buf = [0u8; 4096];
        loop {
            match reader.read(&mut buf) {
                Ok(0) => {
                    debug!("PTY reader: EOF");
                    break;
                }
                Ok(n) => {
                    if let Ok(mut p) = parser.lock() {
                        p.process(&buf[..n]);
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
}
