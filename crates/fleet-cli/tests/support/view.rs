//! A view's end of a terminal stream: frames up to the process, frames down
//! from it on a thread, each wait bounded.

use std::io::Write;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

use term_stream::{Down, Up};

const BOUND: Duration = Duration::from_secs(30);

pub struct View {
    pub child: Child,
    stdin: Option<ChildStdin>,
    frames: Receiver<Down>,
    /// Every output byte read so far, as text.
    pub text: String,
}

impl View {
    pub fn open(mut cmd: Command) -> View {
        cmd.stdin(Stdio::piped()).stdout(Stdio::piped());
        let mut child = cmd.spawn().expect("the stream");
        let stdin = child.stdin.take();
        let mut stdout = child.stdout.take().expect("stdout");
        let (tx, frames) = mpsc::channel();
        std::thread::spawn(move || {
            while let Ok(Some(frame)) = term_stream::read::<Down>(&mut stdout) {
                if tx.send(frame).is_err() {
                    break;
                }
            }
        });
        View {
            child,
            stdin,
            frames,
            text: String::new(),
        }
    }

    pub fn argv(argv: &[String]) -> View {
        let mut cmd = Command::new(&argv[0]);
        cmd.args(&argv[1..]);
        View::open(cmd)
    }

    pub fn send(&mut self, frame: &Up) {
        let stdin = self.stdin.as_mut().expect("stdin open");
        term_stream::write(stdin, frame).expect("send a frame");
        stdin.flush().expect("flush");
    }

    /// The next frame, within the bound.
    pub fn next(&mut self) -> Down {
        let frame = self.frames.recv_timeout(BOUND).unwrap_or_else(|e| {
            panic!(
                "no frame within {BOUND:?} ({e}); text so far: {}",
                self.text
            )
        });
        match &frame {
            Down::Output(bytes) => self.text.push_str(&String::from_utf8_lossy(bytes)),
            Down::Snapshot(s) | Down::Resync(s) => {
                for line in &s.normal {
                    self.text.push_str(line);
                    self.text.push('\n');
                }
            }
            _ => {}
        }
        frame
    }

    /// Frames until one `done` accepts, which it returns.
    pub fn until(&mut self, what: &str, done: impl Fn(&Down) -> bool) -> Down {
        let deadline = Instant::now() + BOUND;
        loop {
            assert!(
                Instant::now() < deadline,
                "{what}: text so far: {}",
                self.text
            );
            let frame = self.next();
            if done(&frame) {
                return frame;
            }
        }
    }

    /// Frames until the text read so far holds `needle`.
    pub fn until_text(&mut self, needle: &str) {
        let deadline = Instant::now() + BOUND;
        while !self.text.contains(needle) {
            assert!(Instant::now() < deadline, "no {needle:?} in: {}", self.text);
            self.next();
        }
    }

    /// Close the view's side and wait for the process, within the bound.
    pub fn close(mut self) -> std::process::ExitStatus {
        drop(self.stdin.take());
        let deadline = Instant::now() + BOUND;
        loop {
            if let Some(status) = self.child.try_wait().expect("wait") {
                return status;
            }
            assert!(Instant::now() < deadline, "the stream did not end");
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

impl Drop for View {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
