use std::ffi::OsString;
use std::fmt;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use process_wrap::std::{ChildWrapper, CommandWrap};

pub mod children;

pub const OUTPUT_CAP: usize = 16 * 1024 * 1024;

#[derive(Clone, Default)]
pub struct Spec {
    pub clear_env: bool,
    pub output_limit: Option<usize>,
    pub program: OsString,
    pub args: Vec<OsString>,
    pub env_remove: Vec<String>,
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
        code: Option<i32>,
        stdout: Vec<u8>,
        stderr: Vec<u8>,
    },
    TimedOut {
        stdout: Vec<u8>,
        stderr: Vec<u8>,
    },
    NotStarted(String),
}

impl Outcome {
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

    pub fn failure(&self) -> String {
        match self {
            Outcome::Exited { code: Some(c), .. } => format!("exited {c}"),
            Outcome::Exited { code: None, .. } => "killed by a signal".into(),
            Outcome::TimedOut { .. } => "timed out; its process tree was killed".into(),
            Outcome::NotStarted(why) => format!("could not start: {why}"),
        }
    }
}

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

pub const GRACE: Duration = Duration::from_secs(2);

pub fn worst_case(timeout: Duration) -> Duration {
    timeout + GRACE * 2
}

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

fn feed_without_blocking_the_caller(mut pipe: impl Write + Send + 'static, bytes: Vec<u8>) {
    thread::spawn(move || {
        let _ = pipe.write_all(&bytes);
    });
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
    let record = match children::Record::intend() {
        Ok(record) => record,
        Err(e) => return Outcome::NotStarted(format!("could not record the run: {e}")),
    };
    let child = match wrap.spawn() {
        Ok(c) => c,
        Err(e) => return Outcome::NotStarted(e.to_string()),
    };
    let mut guard = Guard {
        child,
        exited: false,
    };
    if let Some(record) = &record
        && let Err(e) = record.started(guard.child.id())
    {
        guard.kill();
        return Outcome::NotStarted(format!("could not record the run: {e}"));
    }
    let deadline = Instant::now() + spec.timeout;
    if let (Some(bytes), Some(pipe)) = (spec.stdin.clone(), guard.child.stdin().take()) {
        feed_without_blocking_the_caller(pipe, bytes);
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
    // A grandchild left in the process group or job can hold the pipes open.
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

pub struct Pty {
    master: Mutex<Box<dyn portable_pty::MasterPty + Send>>,
    child: Mutex<Box<dyn portable_pty::Child + Send + Sync>>,
    exited: std::sync::atomic::AtomicBool,
}

impl fmt::Debug for Pty {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Pty").finish_non_exhaustive()
    }
}

pub struct PtyKiller(Box<dyn portable_pty::ChildKiller + Send + Sync>);

impl fmt::Debug for PtyKiller {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PtyKiller").finish_non_exhaustive()
    }
}

impl PtyKiller {
    pub fn kill(&mut self) {
        let _ = self.0.kill();
    }
}

pub fn pty(argv: &[String], rows: u16, cols: u16) -> std::io::Result<Pty> {
    let (program, args) = argv
        .split_first()
        .ok_or_else(|| std::io::Error::other("argv names no program"))?;
    let pair = portable_pty::native_pty_system()
        .openpty(portable_pty::PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        })
        .map_err(std::io::Error::other)?;
    let mut cmd = portable_pty::CommandBuilder::new(program);
    cmd.args(args);
    if let Ok(dir) = std::env::current_dir() {
        cmd.cwd(dir);
    }
    let child = pair
        .slave
        .spawn_command(cmd)
        .map_err(std::io::Error::other)?;
    Ok(Pty {
        master: Mutex::new(pair.master),
        child: Mutex::new(child),
        exited: std::sync::atomic::AtomicBool::new(false),
    })
}

fn poisoned<T>(_: T) -> std::io::Error {
    std::io::Error::other("the terminal's lock is poisoned")
}

impl Pty {
    pub fn reader(&self) -> std::io::Result<Box<dyn Read + Send>> {
        self.master
            .lock()
            .map_err(poisoned)?
            .try_clone_reader()
            .map_err(std::io::Error::other)
    }

    pub fn writer(&self) -> std::io::Result<Box<dyn Write + Send>> {
        self.master
            .lock()
            .map_err(poisoned)?
            .take_writer()
            .map_err(std::io::Error::other)
    }

    pub fn resize(&self, rows: u16, cols: u16) -> std::io::Result<()> {
        self.master
            .lock()
            .map_err(poisoned)?
            .resize(portable_pty::PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(std::io::Error::other)
    }

    pub fn killer(&self) -> std::io::Result<PtyKiller> {
        Ok(PtyKiller(
            self.child.lock().map_err(poisoned)?.clone_killer(),
        ))
    }

    pub fn wait(&self) -> std::io::Result<u32> {
        let status = self.child.lock().map_err(poisoned)?.wait()?;
        self.exited.store(true, std::sync::atomic::Ordering::SeqCst);
        Ok(status.exit_code())
    }
}

impl Drop for Pty {
    fn drop(&mut self) {
        if !self.exited.load(std::sync::atomic::Ordering::SeqCst)
            && let Ok(child) = self.child.get_mut()
        {
            let _ = child.kill();
            let _ = child.wait();
        }
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
