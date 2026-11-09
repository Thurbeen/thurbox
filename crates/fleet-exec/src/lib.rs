//! The one place fleet starts a process. Every adapter that runs a program
//! (the thurbox runtime, the gate's tools) goes through [`run`], and `crates/architecture.toml` lets no other crate name
//! `std::process::Command`.
//!
//! What a run promises:
//!
//! - **Bounded.** Past its timeout the child is killed with its whole tree:
//!   on Unix it leads its own process group and the group is killed; on
//!   Windows it runs in a job object and the job is terminated. A program
//!   that hangs costs one bound, never the caller, and leaves no grandchild.
//! - **Killed on unwind.** A caller that panics while the child runs drops
//!   the guard, which kills the tree the same way.
//! - **Values off the log.** `Debug` prints the names of `env`, never the
//!   values.
//! - **Bounded output.** stdout and stderr are capped; a chatty child cannot
//!   exhaust memory or block on a full pipe.
//!
//! A child that exits normally keeps anything it detached (a multiplexer
//! server it started in a new session): only a timeout or an unwind kills.

use std::ffi::OsString;
use std::fmt;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use process_wrap::std::{ChildWrapper, CommandWrap};

/// The most of each stream a run keeps.
pub const OUTPUT_CAP: usize = 16 * 1024 * 1024;

#[derive(Clone, Default)]
pub struct Spec {
    /// Clear inherited environment before injecting declared variables.
    pub clear_env: bool,
    /// Kill the process tree when either output stream exceeds this bound.
    pub output_limit: Option<usize>,
    pub program: OsString,
    pub args: Vec<OsString>,
    /// Removed from the inherited environment first.
    pub env_remove: Vec<String>,
    /// Added to the inherited environment.
    pub env: Vec<(String, String)>,
    pub stdin: Option<Vec<u8>>,
    pub cwd: Option<PathBuf>,
    pub timeout: Duration,
}

impl Spec {
    pub fn new(program: impl Into<OsString>, timeout: Duration) -> Spec {
        Spec {
            program: program.into(),
            timeout,
            ..Spec::default()
        }
    }

    pub fn arg(mut self, a: impl Into<OsString>) -> Spec {
        self.args.push(a.into());
        self
    }

    pub fn args<I, A>(mut self, args: I) -> Spec
    where
        I: IntoIterator<Item = A>,
        A: Into<OsString>,
    {
        self.args.extend(args.into_iter().map(Into::into));
        self
    }

    pub fn stdin(mut self, bytes: impl Into<Vec<u8>>) -> Spec {
        self.stdin = Some(bytes.into());
        self
    }
}

impl fmt::Debug for Spec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let names: Vec<&str> = self.env.iter().map(|(k, _)| k.as_str()).collect();
        f.debug_struct("Spec")
            .field("program", &self.program)
            .field("args", &self.args)
            .field("env_remove", &self.env_remove)
            .field("env", &names)
            .field("stdin", &self.stdin.as_ref().map(Vec::len))
            .field("cwd", &self.cwd)
            .field("timeout", &self.timeout)
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Exited {
        /// `None` when a signal ended it.
        code: Option<i32>,
        stdout: Vec<u8>,
        stderr: Vec<u8>,
    },
    /// Past the bound; its tree was killed.
    TimedOut { stdout: Vec<u8>, stderr: Vec<u8> },
    /// It never started: not found, not executable.
    NotStarted(String),
}

impl Outcome {
    /// The exit code 0, with stdout as text.
    pub fn success_text(&self) -> Option<String> {
        match self {
            Outcome::Exited {
                code: Some(0),
                stdout,
                ..
            } => Some(String::from_utf8_lossy(stdout).into_owned()),
            _ => None,
        }
    }

    /// One line about a run that did not succeed, without its output: a
    /// child's stderr may echo what it was given, so callers decide whether
    /// to show it.
    pub fn failure(&self) -> String {
        match self {
            Outcome::Exited { code: Some(c), .. } => format!("exited {c}"),
            Outcome::Exited { code: None, .. } => "killed by a signal".into(),
            Outcome::TimedOut { .. } => "timed out; its process tree was killed".into(),
            Outcome::NotStarted(why) => format!("could not start: {why}"),
        }
    }
}

/// Kills the tree unless the child was seen to exit.
struct Guard {
    child: Box<dyn ChildWrapper>,
    exited: bool,
}

impl Guard {
    fn kill(&mut self) {
        let _ = self.child.start_kill();
        let _ = self.child.wait();
        self.exited = true;
    }
}

impl Drop for Guard {
    fn drop(&mut self) {
        if !self.exited {
            self.kill();
        }
    }
}

type Sink = Arc<Mutex<Vec<u8>>>;

fn drain(
    pipe: Option<impl Read + Send + 'static>,
    limit: usize,
    overflow: Arc<std::sync::atomic::AtomicBool>,
) -> (Sink, thread::JoinHandle<()>) {
    let sink: Sink = Arc::default();
    let out = sink.clone();
    let handle = thread::spawn(move || {
        let Some(mut pipe) = pipe else { return };
        let mut buf = [0u8; 8192];
        loop {
            match pipe.read(&mut buf) {
                Ok(0) | Err(_) => return,
                Ok(n) => {
                    let mut v = out.lock().unwrap_or_else(|e| e.into_inner());
                    let room = limit.saturating_sub(v.len());
                    if n > room {
                        overflow.store(true, std::sync::atomic::Ordering::Relaxed);
                    }
                    v.extend_from_slice(&buf[..n.min(room)]);
                }
            }
        }
    });
    (sink, handle)
}

fn take(sink: &Sink) -> Vec<u8> {
    std::mem::take(&mut *sink.lock().unwrap_or_else(|e| e.into_inner()))
}

/// How long the output readers get once the child is gone or killed. A
/// reader still blocked past it belongs to a process that left the tree;
/// it is left behind rather than waited on.
pub const GRACE: Duration = Duration::from_secs(2);

/// The longest a [`run`] with this timeout can take: the bound, the kill,
/// and the readers' grace on either path.
pub fn worst_case(timeout: Duration) -> Duration {
    timeout + GRACE * 2
}

/// Wait for every reader until `until`; true when all finished.
fn settle(readers: &[thread::JoinHandle<()>], until: Instant) -> bool {
    loop {
        if readers.iter().all(thread::JoinHandle::is_finished) {
            return true;
        }
        if Instant::now() >= until {
            return false;
        }
        thread::sleep(Duration::from_millis(5));
    }
}

/// The one place a child's command line and environment are built: the
/// environment minus `env_remove`, plus the variables in `env`.
pub(crate) fn command(spec: &Spec) -> Command {
    let mut cmd = Command::new(&spec.program);
    cmd.args(&spec.args);
    if spec.clear_env {
        cmd.env_clear();
    }
    for k in &spec.env_remove {
        cmd.env_remove(k);
    }
    for (k, v) in &spec.env {
        cmd.env(k, v);
    }
    if let Some(d) = &spec.cwd {
        cmd.current_dir(d);
    }
    cmd
}

pub fn run(spec: &Spec) -> Outcome {
    let mut cmd = command(spec);
    cmd.stdin(if spec.stdin.is_some() {
        Stdio::piped()
    } else {
        Stdio::null()
    })
    .stdout(Stdio::piped())
    .stderr(Stdio::piped());
    let mut wrap = CommandWrap::from(cmd);
    #[cfg(unix)]
    wrap.wrap(process_wrap::std::ProcessGroup::leader());
    #[cfg(windows)]
    wrap.wrap(process_wrap::std::JobObject);
    let child = match wrap.spawn() {
        Ok(c) => c,
        Err(e) => return Outcome::NotStarted(e.to_string()),
    };
    let mut guard = Guard {
        child,
        exited: false,
    };
    let deadline = Instant::now() + spec.timeout;
    if let (Some(bytes), Some(mut pipe)) = (spec.stdin.clone(), guard.child.stdin().take()) {
        // On its own thread: a child that never reads cannot block us.
        thread::spawn(move || {
            let _ = pipe.write_all(&bytes);
        });
    }
    let overflow = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let limit = spec.output_limit.unwrap_or(OUTPUT_CAP).min(OUTPUT_CAP);
    let (out, out_h) = drain(guard.child.stdout().take(), limit, overflow.clone());
    let (err, err_h) = drain(guard.child.stderr().take(), limit, overflow.clone());
    let mut pause = Duration::from_millis(2);
    let code = loop {
        if spec.output_limit.is_some() && overflow.load(std::sync::atomic::Ordering::Relaxed) {
            guard.kill();
            return Outcome::NotStarted("output limit exceeded; process tree killed".into());
        }
        match guard.child.try_wait() {
            Ok(Some(status)) => break Some(status.code()),
            Ok(None) if Instant::now() < deadline => {
                thread::sleep(pause);
                pause = (pause * 2).min(Duration::from_millis(25));
            }
            Ok(None) => break None,
            Err(e) => {
                guard.kill();
                return Outcome::NotStarted(format!("waiting for the child: {e}"));
            }
        }
    };
    let readers = [out_h, err_h];
    let Some(code) = code else {
        guard.kill();
        settle(&readers, Instant::now() + GRACE);
        return Outcome::TimedOut {
            stdout: take(&out),
            stderr: take(&err),
        };
    };
    guard.exited = true;
    // A process the child left in its group or job may still hold the
    // pipes; give the readers until the deadline, then clean up after it.
    if !settle(&readers, deadline.max(Instant::now() + GRACE)) {
        guard.kill();
        settle(&readers, Instant::now() + GRACE);
    }
    if spec.output_limit.is_some() && overflow.load(std::sync::atomic::Ordering::Relaxed) {
        return Outcome::NotStarted("output limit exceeded".into());
    }
    Outcome::Exited {
        code,
        stdout: take(&out),
        stderr: take(&err),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_spec_never_prints_an_environment_value() {
        let mut s = Spec::new("prog", Duration::from_secs(1));
        s.env.push(("TOKEN".into(), "hunter2-value".into()));
        let shown = format!("{s:?}");
        assert!(
            shown.contains("TOKEN") && !shown.contains("hunter2-value"),
            "{shown}"
        );
    }

    #[test]
    fn a_program_that_is_not_there_never_started() {
        let o = run(&Spec::new(
            "fleet-exec-no-such-program",
            Duration::from_secs(5),
        ));
        assert!(matches!(o, Outcome::NotStarted(_)), "{o:?}");
    }
}
