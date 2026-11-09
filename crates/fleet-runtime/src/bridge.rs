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
    let pty = fleet_exec::pty(argv, size.rows, size.cols)
        .map_err(|e| format!("cannot start {argv:?}: {e}"))?;
    let mut output = pty.reader().map_err(|e| e.to_string())?;
    let mut input = pty.writer().map_err(|e| e.to_string())?;
    send(&Down::Resized(size))?;
    let pty = Arc::new(Mutex::new(pty));
    {
        let pty = Arc::clone(&pty);
        let to_view = Arc::clone(&to_view);
        std::thread::spawn(move || {
            loop {
                let wrote = match term_stream::read::<Up>(&mut from_view) {
                    Ok(Some(Up::Input(bytes))) => input.write_all(&bytes),
                    Ok(Some(Up::Paste(text))) => input.write_all(text.as_bytes()),
                    Ok(Some(Up::Resize(Size { rows, cols }))) => {
                        let resized = pty
                            .lock()
                            .map_err(|_| std::io::Error::other("poisoned"))
                            .and_then(|p| p.resize(rows, cols));
                        if resized.is_ok()
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
            // Ending the command is how a bridge lets go.
            if let Ok(mut p) = pty.lock() {
                p.kill();
            }
        });
    }
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        match output.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => send(&Down::Output(buf[..n].to_vec()))?,
        }
    }
    let exit = match pty.lock().map(|mut p| p.wait()) {
        Ok(Ok(code)) => Exit::Known {
            code: Some(i64::from(code)),
        },
        Ok(Err(e)) => Exit::Unknown { why: e.to_string() },
        Err(_) => Exit::Unknown {
            why: "the command's handle is poisoned".into(),
        },
    };
    let _ = send(&Down::Exit(exit));
    Ok(())
}
