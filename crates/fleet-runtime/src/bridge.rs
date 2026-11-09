//! `fleet term-bridge -- <argv>`: a runtime's raw interactive command (a
//! `tty` answer to `attach`) as a terminal stream
//! (docs/runtime/README.md#the-terminal-stream), so a view only ever speaks
//! the stream.
//!
//! The command runs on a pseudo-terminal of its own through `fleet-exec`.
//! The bridge relays: it keeps no screen, so it has no history from before
//! it started and `hello` names no `snapshot`. The view's first `resize`
//! sizes the terminal the command starts on, and a program redraws on a
//! resize, which is how a view gets a screen. A `detach` ends the command
//! (a `docker attach` client, a console), never the process it reaches.

use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use term_stream::{Capability, Down, Exit, Hello, Size, Up, VERSION};

/// Serve `argv` to the view until it detaches, its input ends or the
/// command exits.
pub fn serve(
    argv: &[String],
    mut from_view: impl Read + Send + 'static,
    to_view: impl Write + Send + 'static,
) -> Result<(), String> {
    let to_view = Arc::new(Mutex::new(to_view));
    let send = |frame: &Down| -> Result<(), String> {
        let mut w = to_view
            .lock()
            .map_err(|_| "the view's writer is poisoned")?;
        term_stream::write(&mut *w, frame).map_err(|e| format!("the view: {e}"))
    };
    send(&Down::Hello(Hello {
        version: VERSION,
        capabilities: vec![Capability::Resize],
    }))?;
    let size = loop {
        match term_stream::read::<Up>(&mut from_view).map_err(|e| format!("the view: {e}"))? {
            Some(Up::Resize(size)) => break size,
            Some(Up::Detach) | None => return Ok(()),
            Some(_) => {}
        }
    };
    let pty = Arc::new(
        fleet_exec::pty(argv, size.rows, size.cols)
            .map_err(|e| format!("cannot start {argv:?}: {e}"))?,
    );
    let mut output = pty.reader().map_err(|e| e.to_string())?;
    let mut input = pty.writer().map_err(|e| e.to_string())?;
    let mut killer = pty.killer().map_err(|e| e.to_string())?;
    let bracketed = Arc::new(AtomicBool::new(false));
    send(&Down::Resized(size))?;
    {
        let pty = Arc::clone(&pty);
        let to_view = Arc::clone(&to_view);
        let bracketed = Arc::clone(&bracketed);
        std::thread::spawn(move || {
            loop {
                let wrote = match term_stream::read::<Up>(&mut from_view) {
                    Ok(Some(Up::Input(bytes))) => input.write_all(&bytes),
                    Ok(Some(Up::Paste(text))) if bracketed.load(Ordering::SeqCst) => {
                        input.write_all(format!("\x1b[200~{text}\x1b[201~").as_bytes())
                    }
                    Ok(Some(Up::Paste(text))) => input.write_all(text.as_bytes()),
                    Ok(Some(Up::Resize(Size { rows, cols }))) => {
                        if pty.resize(rows, cols).is_ok()
                            && let Ok(mut w) = to_view.lock()
                        {
                            let _ =
                                term_stream::write(&mut *w, &Down::Resized(Size { rows, cols }));
                        }
                        Ok(())
                    }
                    Ok(Some(Up::Detach) | None) | Err(_) => break,
                };
                if wrote.and_then(|()| input.flush()).is_err() {
                    break;
                }
            }
            // Ending the command is how a bridge lets go; the killer needs no
            // lock, so a wait for the command does not hold it up.
            killer.kill();
        });
    }
    let mut modes = Modes::default();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        match output.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                modes.read(&buf[..n], &bracketed);
                send(&Down::Output(buf[..n].to_vec()))?;
            }
        }
    }
    let exit = match pty.wait() {
        Ok(code) => Exit::Known {
            code: Some(i64::from(code)),
        },
        Err(e) => Exit::Unknown { why: e.to_string() },
    };
    let _ = send(&Down::Exit(exit));
    Ok(())
}

/// Whether the program asked for bracketed paste (`CSI ? 2004 h`, off with
/// `l`), read off its output: the last switch wins, and a switch split
/// across two reads is still seen.
#[derive(Debug, Default)]
struct Modes {
    tail: Vec<u8>,
}

impl Modes {
    const ON: &[u8] = b"\x1b[?2004h";
    const OFF: &[u8] = b"\x1b[?2004l";

    fn read(&mut self, bytes: &[u8], bracketed: &AtomicBool) {
        let mut seen = std::mem::take(&mut self.tail);
        seen.extend_from_slice(bytes);
        let last = |needle: &[u8]| seen.windows(needle.len()).rposition(|w| w == needle);
        match (last(Self::ON), last(Self::OFF)) {
            (Some(on), Some(off)) => bracketed.store(on > off, Ordering::SeqCst),
            (Some(_), None) => bracketed.store(true, Ordering::SeqCst),
            (None, Some(_)) => bracketed.store(false, Ordering::SeqCst),
            (None, None) => {}
        }
        let keep = seen.len().saturating_sub(Self::ON.len() - 1);
        self.tail = seen.split_off(keep);
    }
}
