//! The stream pane: a session's pane over a terminal stream
//! (docs/runtime/README.md#the-terminal-stream), a process thurbox starts
//! and speaks frames with over its stdin and stdout, instead of the
//! in-process control-mode connection. The frames decode into the same
//! [`PaneChunk`](crate::backend::PaneChunk)s, so the grid, the parser and
//! the render path are the ones every pane uses. Behind `stream_panes` until it measures as fast
//! (docs/thurbox/performance.md).

use std::io::{self, Write};
use std::process::{Child, ChildStdin, Command, Stdio};

use term_stream::{Down, Size, Up};

/// What a pane is viewed through: the `runtime-mux` program, the socket of
/// this instance's own server, and the history a snapshot carries
/// (`settings.toml`'s `scrollback_lines`).
#[derive(Debug, Clone)]
pub struct View {
    pub program: std::path::PathBuf,
    pub socket: String,
    pub scrollback: usize,
}

impl View {
    /// The stream that views `pane`.
    pub fn argv(&self, pane: &str) -> Vec<String> {
        vec![
            self.program.to_string_lossy().into_owned(),
            "view".into(),
            "--socket".into(),
            self.socket.clone(),
            "--scrollback".into(),
            self.scrollback.to_string(),
            "--pane".into(),
            pane.into(),
        ]
    }
}

/// Whether a session on `backend` (its row's route, `local:tmux` or a legacy
/// unqualified `tmux`) is viewed through a stream: a local tmux one only.
pub fn views(backend: &str) -> bool {
    crate::session::route::Route::parse(backend).is_ok_and(|r| r.is_local_tmux())
}

/// The `runtime-mux` program: beside thurbox's own binary, else on `PATH`.
pub fn view_program() -> Option<std::path::PathBuf> {
    let name = if cfg!(windows) {
        "runtime-mux.exe"
    } else {
        "runtime-mux"
    };
    std::env::current_exe()
        .ok()
        .and_then(|exe| Some(exe.parent()?.join(name)))
        .filter(|p| p.is_file())
        .or_else(|| crate::paths::resolve_on_path("runtime-mux"))
}

/// Open the stream `argv` names at `rows` × `cols`: its output as chunks,
/// and input that goes up as `input` frames. The process ends when the
/// input is dropped, as a detach.
pub fn adopt(argv: &[String], rows: u16, cols: u16) -> io::Result<crate::backend::AdoptedSession> {
    let (program, args) = argv
        .split_first()
        .ok_or_else(|| io::Error::other("the stream's argv names no program"))?;
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    let (Some(mut stdin), Some(mut stdout)) = (child.stdin.take(), child.stdout.take()) else {
        return Err(io::Error::other("the stream has no pipes"));
    };
    match term_stream::read::<Down>(&mut stdout)? {
        Some(Down::Hello(hello)) if hello.version == term_stream::VERSION => {}
        Some(Down::Hello(hello)) => {
            return Err(io::Error::other(format!(
                "the stream speaks version {}, thurbox {}",
                hello.version,
                term_stream::VERSION
            )))
        }
        other => {
            return Err(io::Error::other(format!(
                "the stream did not start with hello: {other:?}"
            )))
        }
    }
    term_stream::write(&mut stdin, &Up::Resize(Size { rows, cols }))?;
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        while let Ok(Some(frame)) = term_stream::read::<Down>(&mut stdout) {
            let chunk = match frame {
                Down::Snapshot(s) => crate::backend::PaneChunk::Snapshot(Box::new(snapshot(s))),
                Down::Resync(s) => crate::backend::PaneChunk::Resync(Box::new(snapshot(s))),
                Down::Output(bytes) => crate::backend::PaneChunk::Output(bytes),
                Down::Resized(Size { rows, cols }) => {
                    crate::backend::PaneChunk::Resized { rows, cols }
                }
                Down::SizedElsewhere(elsewhere) => {
                    crate::backend::PaneChunk::SizedElsewhere(elsewhere)
                }
                Down::State(bytes) => crate::backend::PaneChunk::State(bytes),
                Down::Hello(_) => continue,
                Down::Exit(_) => break,
            };
            if tx.send(chunk).is_err() {
                break;
            }
        }
        reap(child);
    });
    Ok(crate::backend::AdoptedSession {
        output: Box::new(rx.into_iter()),
        input: Box::new(Input(stdin)),
        seed_len: 0,
        size: None,
    })
}

fn snapshot(s: term_stream::Snapshot) -> crate::backend::contract::PaneSnapshot {
    crate::backend::contract::PaneSnapshot {
        cols: s.cols,
        rows: s.rows,
        cursor: s.cursor,
        normal: s.normal,
        alternate: s.alternate,
    }
}

/// The stream's process once its output ended: it is told nothing more, and
/// waited for so it leaves no zombie.
fn reap(mut child: Child) {
    let _ = child.kill();
    let _ = child.wait();
}

/// Bytes typed into the pane, each write one `input` frame.
struct Input(ChildStdin);

impl Write for Input {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        term_stream::write(&mut self.0, &Up::Input(buf.to_vec()))?;
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.0.flush()
    }
}

impl Drop for Input {
    fn drop(&mut self) {
        let _ = term_stream::write(&mut self.0, &Up::Detach);
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_view_carries_the_scrollback_setting() {
        let view = super::View {
            program: "runtime-mux".into(),
            socket: "s".into(),
            scrollback: 5000,
        };
        let argv = view.argv("%3");
        assert!(
            argv.windows(2).any(|w| w == ["--scrollback", "5000"]),
            "{argv:?}"
        );
    }

    #[test]
    fn a_local_tmux_session_is_viewed_through_a_stream_and_no_other() {
        assert!(super::views("local:tmux"));
        // An unqualified row is on the platform's default multiplexer:
        // `Route::parse` reads `local-tmux`, `tmux` and the empty route so.
        for unqualified in ["local-tmux", "tmux", ""] {
            assert_eq!(super::views(unqualified), !cfg!(windows), "{unqualified:?}");
        }
        for other in [
            "local:rmux",
            "local-rmux",
            "local:psmux",
            "local-psmux",
            "psmux",
            "ssh:box:tmux",
            "ssh:box",
            "wsl:Ubuntu:tmux",
            "not-a-route",
        ] {
            assert!(!super::views(other), "{other}");
        }
    }
}
