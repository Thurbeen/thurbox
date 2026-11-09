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

/// What the stream promises: a snapshot and its history only from a
/// backend that can take one in step with the output.
fn hello(snapshots: bool) -> Hello {
    let mut capabilities = vec![Capability::Viewers, Capability::Resize];
    if snapshots {
        capabilities.extend([Capability::Snapshot, Capability::Scrollback]);
    }
    Hello {
        version: VERSION,
        capabilities,
    }
}

/// The pane's chunks as frames. With snapshots, output before the first
/// screen is in it, and the first screen is whichever snapshot comes first,
/// a resync too: the saved title and modes follow it.
struct Frames {
    shown: bool,
    state: Vec<u8>,
}

impl Frames {
    fn new(snapshots: bool, state: Vec<u8>) -> Frames {
        Frames {
            shown: !snapshots,
            state,
        }
    }

    fn of(&mut self, chunk: PaneChunk) -> Vec<Down> {
        let frame = match chunk {
            PaneChunk::Snapshot(s) | PaneChunk::Resync(s) if !self.shown => {
                self.shown = true;
                let mut first = vec![Down::Snapshot(snapshot(*s))];
                if !self.state.is_empty() {
                    first.push(Down::State(std::mem::take(&mut self.state)));
                }
                return first;
            }
            _ if !self.shown => return Vec::new(),
            PaneChunk::Snapshot(s) | PaneChunk::Resync(s) => Down::Resync(snapshot(*s)),
            PaneChunk::Output(bytes) => Down::Output(bytes),
            PaneChunk::Resized { rows, cols } => Down::Resized(Size { rows, cols }),
            PaneChunk::SizedElsewhere(elsewhere) => Down::SizedElsewhere(elsewhere),
            PaneChunk::State(bytes) => Down::State(bytes),
        };
        vec![frame]
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
    let snapshots = backend.capabilities().supports_snapshots;
    term_stream::write(to_view, &Down::Hello(hello(snapshots))).map_err(io)?;
    let size = loop {
        match term_stream::read::<Up>(&mut from_view).map_err(io)? {
            Some(Up::Resize(size)) => break size,
            Some(Up::Detach) | None => return Ok(()),
            Some(_) => {}
        }
    };
    // With snapshots the screen comes as one, in step with the output, and
    // the title and modes it cannot carry follow it. Without, the backend
    // replays the history and that state into the output itself.
    let (seed, state) = if snapshots {
        (Some(Vec::new()), backend.state_seed(pane))
    } else {
        (None, Vec::new())
    };
    let adopted = backend
        .adopt(pane, size.rows, size.cols, seed)
        .map_err(|e| e.to_string())?;
    if snapshots {
        backend.request_snapshot(pane).map_err(|e| e.to_string())?;
    }
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
    let mut frames = Frames::new(snapshots, state);
    for chunk in adopted.output {
        for frame in frames.of(chunk) {
            if term_stream::write(to_view, &frame).is_err() {
                let _ = backend.detach(pane);
                return Ok(());
            }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn screen(text: &str) -> Box<PaneSnapshot> {
        Box::new(PaneSnapshot {
            cols: 80,
            rows: 24,
            cursor: (0, 0),
            normal: vec![text.into()],
            alternate: None,
        })
    }

    #[test]
    fn a_resync_can_be_the_first_screen() {
        let mut frames = Frames::new(true, Vec::new());
        assert!(frames.of(PaneChunk::Output(b"busy".to_vec())).is_empty());
        assert_eq!(
            frames.of(PaneChunk::Resync(screen("now"))),
            [Down::Snapshot(snapshot(*screen("now")))]
        );
        assert_eq!(
            frames.of(PaneChunk::Output(b"more".to_vec())),
            [Down::Output(b"more".to_vec())]
        );
    }

    #[test]
    fn the_saved_title_and_modes_follow_the_first_screen() {
        let state = b"\x1b]0;title\x07\x1b[?1000h".to_vec();
        let mut frames = Frames::new(true, state.clone());
        assert_eq!(
            frames.of(PaneChunk::Snapshot(screen("now"))),
            [Down::Snapshot(snapshot(*screen("now"))), Down::State(state)]
        );
    }

    #[test]
    fn a_backend_without_snapshots_streams_from_the_start_and_promises_none() {
        assert!(!hello(false).has(Capability::Snapshot));
        assert!(hello(true).has(Capability::Snapshot));
        let mut frames = Frames::new(false, Vec::new());
        assert_eq!(
            frames.of(PaneChunk::Output(b"history".to_vec())),
            [Down::Output(b"history".to_vec())]
        );
    }
}
