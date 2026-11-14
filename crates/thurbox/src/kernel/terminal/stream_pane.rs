//! The stream pane: a session's pane over a terminal stream
//! (docs/runtime/README.md#the-terminal-stream), a process thurbox starts
//! and speaks frames with over its stdin and stdout, instead of the
//! in-process control-mode connection. The frames decode into the same
//! [`PaneChunk`](crate::backend::PaneChunk)s, so the grid, the parser and
//! the render path are the ones every pane uses. Behind `stream_panes` until it measures as fast
//! (docs/thurbox/performance.md).

use std::io::{self, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{channel, Sender};
use std::sync::Arc;

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

/// Whether a session on `backend`, its row's route, is viewed through a
/// stream: a local tmux one only.
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
    adopt_with_upstream(command_of(argv)?, rows, cols, None).map(|(adopted, _)| adopted)
}

/// `argv` as a process on this machine.
fn command_of(argv: &[String]) -> io::Result<Command> {
    let (program, args) = argv
        .split_first()
        .ok_or_else(|| io::Error::other("the stream's argv names no program"))?;
    let mut command = Command::new(program);
    command.args(args);
    Ok(command)
}

/// [`adopt`], with input the stream refused said on `notices`, as
/// `input dropped: <label> isn't reading`: the interface's own notices,
/// never the pane's grid, which a full-screen agent paints over.
pub fn adopt_reporting(
    argv: &[String],
    rows: u16,
    cols: u16,
    notices: Sender<String>,
    label: &str,
) -> io::Result<crate::backend::AdoptedSession> {
    adopt_with_upstream(
        command_of(argv)?,
        rows,
        cols,
        Some(Notices::new(notices, label)),
    )
    .map(|(adopted, _)| adopted)
}

/// How much of a stream's standard error is kept: its end, which is where a
/// launcher's reason (ssh's `connect to host … refused`) is.
const STDERR_TAIL: usize = 2048;

/// Read `stderr` to its end on a thread of its own, so a chatty stream never
/// blocks on it, and answer its last [`STDERR_TAIL`] bytes.
fn stderr_tail(mut stderr: std::process::ChildStderr) -> std::thread::JoinHandle<String> {
    std::thread::spawn(move || {
        let mut tail: Vec<u8> = Vec::new();
        let mut buf = [0u8; 1024];
        while let Ok(n) = std::io::Read::read(&mut stderr, &mut buf) {
            if n == 0 {
                break;
            }
            tail.extend_from_slice(&buf[..n]);
            if tail.len() > STDERR_TAIL {
                tail.drain(..tail.len() - STDERR_TAIL);
            }
        }
        String::from_utf8_lossy(&tail).trim().to_string()
    })
}

/// A stream that did not start: `why`, with what the process said on its
/// standard error once it is stopped.
fn refused(
    why: String,
    child: &mut std::process::Child,
    said: Option<std::thread::JoinHandle<String>>,
) -> io::Error {
    let _ = child.kill();
    let _ = child.wait();
    let said = said.and_then(|tail| tail.join().ok()).unwrap_or_default();
    io::Error::other(if said.is_empty() {
        why
    } else {
        format!("{why}: {said}")
    })
}

/// [`adopt`] of the stream `command` starts, which may run on another
/// machine (a Fleet's link), and the stream's way up for frames other than
/// input.
pub(super) fn adopt_with_upstream(
    mut command: Command,
    rows: u16,
    cols: u16,
    notices: Option<Notices>,
) -> io::Result<(crate::backend::AdoptedSession, Upstream)> {
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let (Some(mut stdin), Some(mut stdout)) = (child.stdin.take(), child.stdout.take()) else {
        return Err(io::Error::other("the stream has no pipes"));
    };
    // Kept so a stream that never starts says why: over ssh, a refused
    // connection is a line on stderr and no frame at all.
    let said = child.stderr.take().map(stderr_tail);
    let snapshots = match term_stream::read::<Down>(&mut stdout) {
        Ok(Some(Down::Hello(hello))) if hello.version == term_stream::VERSION => {
            hello.has(term_stream::Capability::Snapshot)
        }
        Ok(Some(Down::Hello(hello))) => {
            return Err(refused(
                format!(
                    "the stream speaks version {}, thurbox {}",
                    hello.version,
                    term_stream::VERSION
                ),
                &mut child,
                said,
            ))
        }
        Ok(other) => {
            return Err(refused(
                format!("the stream did not start with hello: {other:?}"),
                &mut child,
                said,
            ))
        }
        Err(e) => return Err(refused(e.to_string(), &mut child, said)),
    };
    term_stream::write(&mut stdin, &Up::Resize(Size { rows, cols }))?;
    let upstream = Upstream::over(stdin);
    let (tx, rx) = std::sync::mpsc::channel();
    // A pane over a stream starts without a grid and builds it from the
    // first snapshot. A stream that sends none (`fleet term-bridge` has no
    // screen from before it started) starts on a blank one of its size.
    if !snapshots {
        let blank = term_stream::Snapshot {
            cols,
            rows,
            cursor: (0, 0),
            normal: Vec::new(),
            alternate: None,
        };
        let _ = tx.send(crate::backend::PaneChunk::Snapshot(Box::new(snapshot(
            blank,
        ))));
    }
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
    let adopted = crate::backend::AdoptedSession {
        output: Box::new(rx.into_iter()),
        input: Box::new(Input::new(upstream.clone(), notices)),
        seed_len: 0,
        size: None,
    };
    Ok((adopted, upstream))
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

/// The stream's way up: shared by the pane's input and whatever else sends
/// frames, such as a resize from the paint path.
///
/// Frames are queued to a writer of their own, so sending never waits on
/// the process: one that stops reading stalls its writer, not the
/// interface. The writer ends once every sender is gone, or the process
/// stops taking frames.
///
/// What is typed or pasted and waits is bounded by [`QUEUE_BYTES`]: past it
/// input is refused at once, as the pane's own input channel refuses it when
/// full. Input bigger than the bound still goes when nothing waits before
/// it, so a long paste reaches a stream that reads.
#[derive(Clone)]
pub(super) struct Upstream {
    frames: Sender<Up>,
    queued: Arc<AtomicUsize>,
}

/// The most bytes of frames waiting for a stream.
const QUEUE_BYTES: usize = 1 << 20;

/// The bytes a frame holds, as [`Upstream`] counts them.
fn weight(frame: &Up) -> usize {
    match frame {
        Up::Input(bytes) => bytes.len(),
        Up::Paste(text) => text.len(),
        _ => 16,
    }
}

impl Upstream {
    fn over(mut stdin: ChildStdin) -> Self {
        let (frames, rx) = channel::<Up>();
        let queued = Arc::new(AtomicUsize::new(0));
        let written = Arc::clone(&queued);
        std::thread::spawn(move || {
            for frame in rx {
                let sent = term_stream::write(&mut stdin, &frame).and_then(|()| stdin.flush());
                written.fetch_sub(weight(&frame), Ordering::AcqRel);
                if sent.is_err() {
                    break;
                }
            }
        });
        Self { frames, queued }
    }

    pub(super) fn send(&self, frame: Up) -> io::Result<()> {
        let weight = weight(&frame);
        let before = self.queued.fetch_add(weight, Ordering::AcqRel);
        // Only what is typed or pasted is refused: a resize or a detach is a
        // few bytes, and dropping one would leave the stream wrong.
        let typed = matches!(frame, Up::Input(_) | Up::Paste(_));
        if typed && before > 0 && before + weight > QUEUE_BYTES {
            self.queued.fetch_sub(weight, Ordering::AcqRel);
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "the stream's input is full: it has stopped reading",
            ));
        }
        self.frames.send(frame).map_err(|_| {
            self.queued.fetch_sub(weight, Ordering::AcqRel);
            io::Error::other("the stream takes no more frames")
        })
    }
}

/// Where a pane says it dropped input: the interface's notices, under the
/// pane's label.
pub(super) struct Notices {
    tx: Sender<String>,
    label: String,
}

impl Notices {
    pub(super) fn new(tx: Sender<String>, label: &str) -> Self {
        Self {
            tx,
            label: label.to_string(),
        }
    }

    fn dropped(&self) {
        let _ = self
            .tx
            .send(format!("input dropped: {} isn't reading", self.label));
    }
}

/// Bytes typed into the pane, each write one `input` frame.
struct Input {
    up: Upstream,
    notices: Option<Notices>,
    /// Whether input is being dropped, so a run of refused writes is said
    /// once.
    dropping: bool,
}

impl Input {
    fn new(up: Upstream, notices: Option<Notices>) -> Self {
        Self {
            up,
            notices,
            dropping: false,
        }
    }
}

impl Write for Input {
    /// Each write is queued whole or not at all, so a paste is never cut in
    /// its middle.
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self.up.send(Up::Input(buf.to_vec())) {
            // Dropped, not failed: the pane's writer ends at its first
            // error, and a backlog must not close the pane. Later input goes
            // once the stream reads again; the interface says what was
            // dropped, once per run of refused writes.
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                tracing::warn!("dropped {} bytes of input: {e}", buf.len());
                if !self.dropping {
                    self.dropping = true;
                    if let Some(notices) = &self.notices {
                        notices.dropped();
                    }
                }
            }
            sent => {
                sent?;
                self.dropping = false;
            }
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Drop for Input {
    fn drop(&mut self) {
        let _ = self.up.send(Up::Detach);
    }
}

#[cfg(test)]
mod tests {
    // For the tests that run a process, all of them Unix's.
    #[cfg(unix)]
    use std::process::{Command, Stdio};
    #[cfg(unix)]
    use std::time::Duration;

    #[cfg(unix)]
    use term_stream::{Size, Up};

    /// A stream that never starts says why: what its launcher wrote on
    /// standard error (ssh's refusal) is in the error, not only the missing
    /// hello.
    #[cfg(unix)]
    #[test]
    fn a_stream_that_never_starts_says_what_it_wrote_on_stderr() {
        let mut command = Command::new("sh");
        command.args([
            "-c",
            "echo 'ssh: connect to host box port 22: No route to host' >&2; exit 255",
        ]);
        let Err(why) = super::adopt_with_upstream(command, 24, 80, None) else {
            panic!("a stream with no hello was opened");
        };
        let why = why.to_string();
        assert!(why.contains("did not start with hello"), "{why}");
        assert!(why.contains("No route to host"), "{why}");
    }

    #[cfg(unix)]
    fn notices(tx: std::sync::mpsc::Sender<String>) -> Option<super::Notices> {
        Some(super::Notices::new(tx, "s1"))
    }

    /// A resize is sent from the paint path, so it never waits behind input
    /// a stream has stopped reading: here a paste bigger than the pipe, to
    /// a process that reads nothing.
    #[cfg(unix)]
    #[test]
    fn a_resize_never_waits_behind_input_the_stream_has_not_read() {
        let mut child = Command::new("sleep")
            .arg("30")
            .stdin(Stdio::piped())
            .spawn()
            .expect("sleep");
        let upstream = super::Upstream::over(child.stdin.take().expect("stdin"));
        let typing = upstream.clone();
        std::thread::spawn(move || {
            let _ = typing.send(Up::Input(vec![b'x'; 4 << 20]));
        });
        std::thread::sleep(Duration::from_millis(200));
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(upstream.send(Up::Resize(Size { rows: 1, cols: 1 })).is_ok());
        });
        let answered = rx.recv_timeout(Duration::from_secs(2));
        let _ = child.kill();
        let _ = child.wait();
        assert_eq!(answered, Ok(true), "the resize waited behind the paste");
    }

    /// What waits for a stream that stopped reading is bounded: past the
    /// bound, input is refused at once rather than queued.
    #[cfg(unix)]
    #[test]
    fn input_for_a_stream_that_stopped_reading_is_bounded() {
        let mut child = Command::new("sleep")
            .arg("30")
            .stdin(Stdio::piped())
            .spawn()
            .expect("sleep");
        let upstream = super::Upstream::over(child.stdin.take().expect("stdin"));
        let mut refused = None;
        for _ in 0..64 {
            if let Err(e) = upstream.send(Up::Input(vec![b'x'; 256 << 10])) {
                refused = Some(e.to_string());
                break;
            }
        }
        let _ = child.kill();
        let _ = child.wait();
        let refused = refused.expect("16 MiB were queued for a stream reading nothing");
        assert!(refused.contains("full"), "{refused}");
    }

    /// A full queue drops what overflows and keeps the pane's input: the
    /// pane's writer stops at its first error and detaches, so a backlog
    /// must not be one. Once the stream reads again, the next key arrives.
    #[cfg(unix)]
    #[test]
    fn a_key_after_a_full_queue_drains_arrives() {
        use std::io::Write;
        let dir = tempfile::tempdir().expect("dir");
        let out = dir.path().join("read");
        let mut child = Command::new("sh")
            .arg("-c")
            .arg(format!("sleep 1; cat > '{}'", out.display()))
            .stdin(Stdio::piped())
            .spawn()
            .expect("sh");
        let mut input = super::Input::new(
            super::Upstream::over(child.stdin.take().expect("stdin")),
            notices(std::sync::mpsc::channel().0),
        );
        for _ in 0..8 {
            input
                .write_all(&vec![b'x'; 512 << 10])
                .expect("a backlog is no error");
        }
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        loop {
            std::thread::sleep(Duration::from_millis(100));
            input.write_all(b"key-marker").expect("a key");
            let read = std::fs::read(&out).unwrap_or_default();
            if read.windows(10).any(|w| w == b"key-marker") {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "the key never arrived"
            );
        }
        drop(input);
        let _ = child.kill();
        let _ = child.wait();
    }

    /// A write is queued whole or not at all, so a paste is never cut in
    /// its middle; one refused is said in the interface's notices.
    #[cfg(unix)]
    #[test]
    fn a_paste_that_does_not_fit_is_refused_whole_and_said_so() {
        use std::io::Write;
        use std::sync::atomic::Ordering;
        let mut child = Command::new("sleep")
            .arg("30")
            .stdin(Stdio::piped())
            .spawn()
            .expect("sleep");
        let (tx, rx) = std::sync::mpsc::channel();
        let upstream = super::Upstream::over(child.stdin.take().expect("stdin"));
        let mut input = super::Input::new(upstream.clone(), notices(tx));
        // The writer takes the first frame and stalls on it; the second
        // waits in the queue, near the bound.
        input.write_all(&vec![b'a'; 100 << 10]).expect("typed");
        std::thread::sleep(Duration::from_millis(200));
        input.write_all(&vec![b'b'; 800 << 10]).expect("queued");
        let waiting = upstream.queued.load(Ordering::Acquire);
        input
            .write_all(&vec![b'c'; 300 << 10])
            .expect("refused, not failed");
        let after = upstream.queued.load(Ordering::Acquire);
        let notice = rx.recv_timeout(Duration::from_secs(2));
        let _ = child.kill();
        let _ = child.wait();
        assert_eq!(after, waiting, "none of the paste was queued");
        assert_eq!(
            notice.as_deref(),
            Ok("input dropped: s1 isn't reading"),
            "the interface is told"
        );
    }

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
        for other in [
            "local:rmux",
            "local:psmux",
            "local-tmux",
            "tmux",
            "",
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
