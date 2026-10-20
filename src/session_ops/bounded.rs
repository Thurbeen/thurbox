//! Running one shell command to completion or to its deadline: the runner
//! lifecycle hooks and `Exec` automations share (issue #1507).
//!
//! Synchronous and blocking, for the reason [`super::lifecycle_hooks`] gives:
//! `session_ops` has no async runtime, so the deadline is a `try_wait` poll
//! rather than a timed future. Both output pipes are drained on threads for the
//! whole run — a deadline without draining deadlocks on a full pipe, and
//! draining without a deadline hangs on a command that never exits.
//!
//! The child leads a process group of its own on Unix, and a timeout kills the
//! whole group: `sh -c 'sleep 600'` is a shell with a `sleep` under it, and
//! killing only the shell left the `sleep` running with the pipes open. On
//! Windows `taskkill /T` does the same for the tree.

use std::io::{Read, Write};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// How often the runner checks whether the child has exited. Coarse enough to
/// cost nothing, fine enough that a timeout lands within a frame of the
/// deadline.
const POLL_INTERVAL: Duration = Duration::from_millis(20);

/// Bytes of each stream the reader keeps. Only a tail is reported, but a
/// command that prints a megabyte still has to be *drained* or it blocks on a
/// full pipe — which is why the readers are threads and this is a cap rather
/// than a limit.
const STREAM_KEEP: usize = 64 * 1024;

/// How long to wait for the reader threads after the child has exited. They
/// end when the pipes close, which is at once unless the command left a
/// grandchild holding them (`sleep 100 &`); then the output is simply not
/// reported, rather than the caller waiting on a process the command forgot
/// about.
const READER_GRACE: Duration = Duration::from_millis(500);

/// How a bounded run ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Exit {
    /// It exited with this code.
    Code(i32),
    /// It was ended by a signal it did not get from the runner.
    Signal,
    /// It was still running at its deadline and was killed.
    TimedOut(Duration),
    /// It never started.
    SpawnFailed(String),
    /// It started, and then could not be waited on.
    WaitFailed(String),
}

/// What a bounded run produced: how it ended, the last 64 KiB of each stream,
/// and how long it took.
#[derive(Debug, Clone)]
pub struct Finished {
    pub exit: Exit,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub elapsed: Duration,
}

/// Run `cmd` until it exits or `timeout` passes, writing `stdin` to it (then
/// closing it) when given and handing it a closed stdin otherwise.
///
/// The caller sets the program, arguments, environment and directory; this
/// owns the three standard streams and the process group.
pub fn run(mut cmd: Command, stdin: Option<String>, timeout: Duration) -> Finished {
    cmd.stdin(if stdin.is_some() {
        Stdio::piped()
    } else {
        Stdio::null()
    })
    .stdout(Stdio::piped())
    .stderr(Stdio::piped());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    let started = Instant::now();
    let mut child = match cmd.spawn() {
        Ok(child) => child,
        Err(e) => {
            return Finished {
                exit: Exit::SpawnFailed(e.to_string()),
                stdout: Vec::new(),
                stderr: Vec::new(),
                elapsed: started.elapsed(),
            }
        }
    };

    // The payload is small, but a command that exits without reading would
    // make a synchronous write fail on a closed pipe — and one that reads
    // slowly would stall the runner before its deadline was even armed.
    if let (Some(mut pipe), Some(payload)) = (child.stdin.take(), stdin) {
        std::thread::spawn(move || {
            let _ = pipe.write_all(payload.as_bytes());
        });
    }
    let stdout = child.stdout.take().map(drain);
    let stderr = child.stderr.take().map(drain);

    let exit = loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                break status.code().map_or(Exit::Signal, Exit::Code);
            }
            Ok(None) if started.elapsed() >= timeout => {
                kill_tree(&mut child);
                let _ = child.wait();
                break Exit::TimedOut(timeout);
            }
            Ok(None) => std::thread::sleep(POLL_INTERVAL),
            Err(e) => break Exit::WaitFailed(e.to_string()),
        }
    };
    let elapsed = started.elapsed();
    Finished {
        exit,
        stdout: stdout.map(collect).unwrap_or_default(),
        stderr: stderr.map(collect).unwrap_or_default(),
        elapsed,
    }
}

/// Kill the child and everything it started.
fn kill_tree(child: &mut std::process::Child) {
    #[cfg(unix)]
    if let Ok(group) = libc::pid_t::try_from(child.id()) {
        // SAFETY: `kill` takes no pointers; a negative pid names the process
        // group the child leads, which `process_group(0)` made it the leader
        // of. A group that is already gone answers ESRCH, which is fine.
        unsafe {
            libc::kill(-group, libc::SIGKILL);
        }
    }
    #[cfg(windows)]
    {
        let _ = Command::new("taskkill")
            .args(["/T", "/F", "/PID", &child.id().to_string()])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
    let _ = child.kill();
}

/// Read a stream to its end on a thread, keeping only the last
/// [`STREAM_KEEP`] bytes, and hand the tail back over a channel — so the runner
/// can wait for it *with a deadline* ([`READER_GRACE`]).
fn drain(mut stream: impl Read + Send + 'static) -> mpsc::Receiver<Vec<u8>> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut kept: Vec<u8> = Vec::new();
        let mut chunk = [0u8; 8192];
        loop {
            match stream.read(&mut chunk) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    kept.extend_from_slice(&chunk[..n]);
                    if kept.len() > 2 * STREAM_KEEP {
                        kept.drain(..kept.len() - STREAM_KEEP);
                    }
                }
            }
        }
        let _ = tx.send(kept);
    });
    rx
}

fn collect(rx: mpsc::Receiver<Vec<u8>>) -> Vec<u8> {
    rx.recv_timeout(READER_GRACE).unwrap_or_default()
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    fn sh(command: &str) -> Command {
        super::super::platform_shell(command)
    }

    #[test]
    fn a_hung_command_and_what_it_started_are_killed_at_the_deadline() {
        let temp = tempfile::TempDir::new().unwrap();
        let pid_file = temp.path().join("pid");
        // The grandchild writes its own pid, then sleeps: the shell's `wait`
        // keeps the shell alive too, so both are in the tree at the deadline.
        let started = Instant::now();
        let done = run(
            sh(&format!(
                "sh -c 'echo $$ > {}; exec sleep 600' & wait",
                pid_file.display()
            )),
            None,
            Duration::from_secs(1),
        );
        assert_eq!(done.exit, Exit::TimedOut(Duration::from_secs(1)));
        assert!(started.elapsed() < Duration::from_secs(5));
        let pid: i32 = std::fs::read_to_string(&pid_file)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        // Reaped by init once killed; give it a moment to go.
        let deadline = Instant::now() + Duration::from_secs(5);
        // SAFETY: signal 0 only probes whether the pid exists.
        while unsafe { libc::kill(pid, 0) } == 0 {
            assert!(
                Instant::now() < deadline,
                "the grandchild outlived the kill"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    #[test]
    fn the_streams_come_back_apart_with_the_code() {
        let done = run(
            sh("echo out; echo err >&2; exit 4"),
            None,
            Duration::from_secs(10),
        );
        assert_eq!(done.exit, Exit::Code(4));
        assert_eq!(String::from_utf8_lossy(&done.stdout).trim(), "out");
        assert_eq!(String::from_utf8_lossy(&done.stderr).trim(), "err");
    }

    #[test]
    fn stdin_is_written_when_given_and_closed_otherwise() {
        let done = run(sh("cat"), Some("payload".into()), Duration::from_secs(10));
        assert_eq!(String::from_utf8_lossy(&done.stdout), "payload");
        let done = run(sh("cat"), None, Duration::from_secs(10));
        assert_eq!(done.exit, Exit::Code(0));
        assert!(done.stdout.is_empty());
    }

    #[test]
    fn a_command_that_cannot_start_says_so() {
        let done = run(
            Command::new("/nonexistent/thurbox-test-binary"),
            None,
            Duration::from_secs(1),
        );
        assert!(matches!(done.exit, Exit::SpawnFailed(_)), "{:?}", done.exit);
    }
}
