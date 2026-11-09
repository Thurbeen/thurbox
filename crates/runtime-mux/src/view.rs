//! `runtime-mux view`: one pane as a terminal stream
//! (docs/runtime/README.md#the-terminal-stream), its frames on the
//! process's stdin and stdout.
//!
//! The server keeps the screen, so the stream is the backend's own control
//! mode encoded as frames: the snapshot the backend puts at its place in the
//! output, then the output, sizes and state in order. The view's first
//! `resize` sizes the pane, and the stream starts once the size is known: a
//! connection sizes the pane it attaches to.

use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use term_stream::{Capability, Down, Exit, Hello, Size, Snapshot, Up, VERSION};

use crate::contract::{PaneChunk, PaneSnapshot, SessionBackend};

fn snapshot(s: PaneSnapshot) -> Snapshot {
    Snapshot {
        cols: s.cols,
        rows: s.rows,
        cursor: s.cursor,
        normal: s.normal,
        alternate: s.alternate,
    }
}

/// Serve `pane` until the view detaches, its input ends or the pane goes.
/// `backend` is shared with the thread reading the view's frames.
pub fn serve(
    backend: Arc<dyn SessionBackend>,
    pane: &str,
    mut from_view: impl Read + Send + 'static,
    to_view: &mut impl Write,
) -> Result<(), String> {
    let io = |e: std::io::Error| format!("the view: {e}");
    term_stream::write(
        to_view,
        &Down::Hello(Hello {
            version: VERSION,
            capabilities: vec![
                Capability::Snapshot,
                Capability::Scrollback,
                Capability::Viewers,
                Capability::Resize,
            ],
        }),
    )
    .map_err(io)?;
    let size = loop {
        match term_stream::read::<Up>(&mut from_view).map_err(io)? {
            Some(Up::Resize(size)) => break size,
            Some(Up::Detach) | None => return Ok(()),
            Some(_) => {}
        }
    };
    let adopted = backend
        .adopt(pane, size.rows, size.cols, Some(Vec::new()))
        .map_err(|e| e.to_string())?;
    backend.request_snapshot(pane).map_err(|e| e.to_string())?;
    let detached = Arc::new(AtomicBool::new(false));
    {
        let backend = Arc::clone(&backend);
        let detached = Arc::clone(&detached);
        let pane = pane.to_string();
        let mut input = adopted.input;
        std::thread::spawn(move || {
            loop {
                let done = match term_stream::read::<Up>(&mut from_view) {
                    Ok(Some(Up::Input(bytes))) => input
                        .write_all(&bytes)
                        .and_then(|()| input.flush())
                        .is_err(),
                    Ok(Some(Up::Paste(text))) => backend.send_text(&pane, &text, false).is_err(),
                    Ok(Some(Up::Resize(Size { rows, cols }))) => {
                        let _ = backend.claim_size(&pane, rows, cols);
                        false
                    }
                    Ok(Some(Up::Detach) | None) | Err(_) => true,
                };
                if done {
                    break;
                }
            }
            // Letting go of the pane ends its output, and so the stream.
            detached.store(true, Ordering::SeqCst);
            let _ = backend.detach(&pane);
        });
    }
    // Output before the first snapshot is in it.
    let mut shown = false;
    for chunk in adopted.output {
        let frame = match chunk {
            PaneChunk::Snapshot(s) if !shown => {
                shown = true;
                Down::Snapshot(snapshot(*s))
            }
            _ if !shown => continue,
            PaneChunk::Snapshot(s) | PaneChunk::Resync(s) => Down::Resync(snapshot(*s)),
            PaneChunk::Output(bytes) => Down::Output(bytes),
            PaneChunk::Resized { rows, cols } => Down::Resized(Size { rows, cols }),
            PaneChunk::SizedElsewhere(elsewhere) => Down::SizedElsewhere(elsewhere),
            PaneChunk::State(bytes) => Down::State(bytes),
        };
        if term_stream::write(to_view, &frame).is_err() {
            let _ = backend.detach(pane);
            return Ok(());
        }
    }
    if detached.load(Ordering::SeqCst) {
        return Ok(());
    }
    let exit = match backend.pane_state(pane) {
        Ok(state) if state.dead == Some(true) => Exit::Known {
            code: state.exit_status,
        },
        Ok(_) => Exit::Unknown {
            why: "the pane's output ended".into(),
        },
        Err(e) => Exit::Unknown { why: e.to_string() },
    };
    let _ = term_stream::write(to_view, &Down::Exit(exit));
    Ok(())
}
