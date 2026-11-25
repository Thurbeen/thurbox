use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

const OUTPUT_CAP: usize = 256 * 1024;

pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

const MAX_TIMEOUT: Duration = Duration::from_secs(600);

pub const DEFAULT_TTL: Duration = Duration::from_secs(2);

const MIN_TTL: Duration = Duration::from_millis(250);

const MAX_CONCURRENT: usize = 4;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ask {
    pub key: String,
    pub program: String,
    pub target: Target,
    pub ttl: Duration,
    pub timeout: Duration,
    pub refresh: bool,
}

#[derive(Debug, Clone)]
pub enum Request {
    Ask(Ask),
    Forget(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    Session(String),
    Local,
    Host(String),
}

impl Ask {
    pub fn clamped(mut self) -> Self {
        self.ttl = self.ttl.max(MIN_TTL);
        self.timeout = self.timeout.min(MAX_TIMEOUT);
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Run {
    Pending,
    Done(Output),
    Failed(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Output {
    pub stdout: String,
    pub stderr: String,
    pub status: Option<i32>,
    pub truncated: bool,
    pub timed_out: bool,
}

struct Finished {
    owner: String,
    key: String,
    run: Run,
    at: Instant,
    started: Instant,
    identity: Arc<()>,
}

struct Report {
    tx: Sender<Finished>,
    owner: String,
    key: String,
    run: Option<Run>,
    started: Instant,
    identity: Arc<()>,
}

impl Drop for Report {
    fn drop(&mut self) {
        let _ = self.tx.send(Finished {
            owner: std::mem::take(&mut self.owner),
            key: std::mem::take(&mut self.key),
            run: self.run.take().unwrap_or(Run::Pending),
            at: Instant::now(),
            started: self.started,
            identity: self.identity.clone(),
        });
    }
}

struct Answer {
    run: Run,
    at: Option<Instant>,
    ttl: Duration,
    inflight: bool,
    identity: Arc<()>,
}

impl Answer {
    fn stale(&self, now: Instant) -> bool {
        match self.at {
            None => false,
            Some(at) => now.duration_since(at) >= self.ttl,
        }
    }
}

pub type Runner = dyn Fn(&Ask) -> Run + Send + Sync;

#[derive(Debug, Clone)]
pub enum RunEvent {
    Started {
        owner: String,
        at: Instant,
    },
    Finished {
        owner: String,
        started: Instant,
        took: Duration,
    },
}

#[derive(Default)]
pub struct RunStore {
    answers: HashMap<(String, String), Answer>,
    queued: VecDeque<(String, Ask, Arc<()>)>,
    running: usize,
    channel: Option<(Sender<Finished>, Receiver<Finished>)>,
    events: Vec<RunEvent>,
}

impl RunStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn request(&mut self, mod_name: &str, ask: Ask, runner: &std::sync::Arc<Runner>) -> bool {
        let ask = ask.clamped();
        let id = (mod_name.to_string(), ask.key.clone());
        let now = Instant::now();

        if let Some(answer) = self.answers.get(&id) {
            // Already queued or executing: a second ask must not start a second
            // process. This covers a refresh too, which the old answer's own
            // `Run::Pending` state cannot — see `Answer::inflight`.
            if answer.inflight {
                return false;
            }
            if !ask.refresh && !answer.stale(now) {
                return false;
            }
        }

        // The previous answer stays readable until the new one lands, so a pane
        // does not blink to "pending" every time its value refreshes — only a
        // key nobody has asked for before starts out pending.
        let identity = Arc::new(());
        let entry = self.answers.entry(id).or_insert_with(|| Answer {
            run: Run::Pending,
            at: None,
            ttl: ask.ttl,
            inflight: false,
            identity: identity.clone(),
        });
        entry.inflight = true;
        entry.identity = identity.clone();

        self.queued.push_back((mod_name.to_string(), ask, identity));
        self.drain_queue(runner);
        true
    }

    fn drain_queue(&mut self, runner: &std::sync::Arc<Runner>) {
        while self.running < MAX_CONCURRENT {
            let Some((owner, ask, identity)) = self.queued.pop_front() else {
                return;
            };
            self.running += 1;
            let started = Instant::now();
            self.events.push(RunEvent::Started {
                owner: owner.clone(),
                at: started,
            });
            let tx = self.ensure_channel();
            let runner = runner.clone();
            std::thread::spawn(move || {
                // Reported from a drop guard so a runner that panics still frees
                // its slot and clears the key's in-flight marker. Otherwise one
                // panic costs a permanent slot out of `MAX_CONCURRENT` and leaves
                // that key unable to ever run again; four would wedge every run in
                // the interface.
                let mut report = Report {
                    tx,
                    owner,
                    key: ask.key.clone(),
                    run: Some(Run::Failed(format!("{}: the run panicked", ask.program))),
                    started,
                    identity,
                };
                report.run = Some(runner(&ask));
            });
        }
    }

    fn ensure_channel(&mut self) -> Sender<Finished> {
        self.channel
            .get_or_insert_with(std::sync::mpsc::channel)
            .0
            .clone()
    }

    pub fn poll(&mut self, runner: &std::sync::Arc<Runner>) -> bool {
        let mut changed = false;
        let finished: Vec<Finished> = match &self.channel {
            Some((_, rx)) => rx.try_iter().collect(),
            None => Vec::new(),
        };
        for done in finished {
            self.running = self.running.saturating_sub(1);
            self.events.push(RunEvent::Finished {
                owner: done.owner.clone(),
                started: done.started,
                took: done.at.saturating_duration_since(done.started),
            });
            let id = (done.owner, done.key);
            if let Some(answer) = self.answers.get_mut(&id) {
                if Arc::ptr_eq(&answer.identity, &done.identity) {
                    answer.run = done.run;
                    answer.at = Some(done.at);
                    answer.inflight = false;
                }
            }
            changed = true;
        }
        if changed {
            self.drain_queue(runner);
        }
        changed
    }

    pub fn is_empty(&self) -> bool {
        self.answers.is_empty() && self.queued.is_empty() && self.running == 0
    }

    pub fn get(&self, mod_name: &str, key: &str) -> Option<&Run> {
        self.answers
            .get(&(mod_name.to_string(), key.to_string()))
            .map(|answer| &answer.run)
    }

    pub fn for_mod<'a>(&'a self, mod_name: &str) -> Vec<(&'a str, &'a Run)> {
        self.answers
            .iter()
            .filter(|((owner, _), _)| owner == mod_name)
            .map(|((_, key), answer)| (key.as_str(), &answer.run))
            .collect()
    }

    pub fn forget(&mut self, mod_name: &str, key: &str) -> bool {
        let removed = self
            .answers
            .remove(&(mod_name.to_string(), key.to_string()));
        self.queued
            .retain(|(owner, ask, _)| owner != mod_name || ask.key != key);
        removed.is_some()
    }

    pub fn retain_mods(&mut self, live: &[String]) {
        self.answers.retain(|(owner, _), _| live.contains(owner));
        self.queued.retain(|(owner, _, _)| live.contains(owner));
    }

    pub fn drain_events(&mut self) -> Vec<RunEvent> {
        std::mem::take(&mut self.events)
    }
}

pub fn capture(mut command: std::process::Command, timeout: Duration) -> Run {
    command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());

    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(e) => return Run::Failed(format!("could not start it: {e}")),
    };

    // Both streams are drained on their own threads, writing into buffers this
    // one can read at any moment. Three things force that shape:
    //
    // - A program that fills the stderr pipe while we read stdout deadlocks.
    // - A read that runs to EOF *here* would not return until the program
    //   exited, so the timeout below could never fire.
    // - On a timeout we must not WAIT for the readers either: `sh -c "sleep 30"`
    //   passes its stdout to a grandchild, so killing the shell leaves the pipe
    //   open and a join would block for the full thirty seconds — which is
    //   exactly the bug the timeout exists to prevent, reintroduced one level
    //   down. So the buffers are shared, and on a timeout we take what has
    //   arrived and abandon the readers, which end when the pipe finally closes.
    let out = Captured::draining(child.stdout.take());
    let err = Captured::draining(child.stderr.take());

    // The exit is waited for on its own thread — a blocking `wait` there and a
    // `recv_timeout` here, instead of a 10ms `try_wait` poll that woke this
    // thread a hundred times a second for the life of the program. The waiter
    // owns the `Child` (both `wait` and `kill` need `&mut`), so the timeout
    // path kills by pid instead; the waiter's `wait` then reaps the corpse, and
    // the bounded second `recv` below is so a kill that failed cannot hold this
    // worker past its own timeout.
    let pid = child.id();
    let (exit_tx, exit_rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = exit_tx.send(child.wait());
    });
    let (status, timed_out) = match exit_rx.recv_timeout(timeout) {
        Ok(Ok(status)) => (status.code(), false),
        Ok(Err(e)) => return Run::Failed(format!("waiting for it failed: {e}")),
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
            kill_by_pid(pid);
            let _ = exit_rx.recv_timeout(Duration::from_secs(2));
            (None, true)
        }
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
            return Run::Failed("waiting for it failed: the waiter thread died".to_string())
        }
    };

    // A program that exited has closed its pipes, so give the readers a moment
    // to finish rather than racing them for the last chunk. A timed-out one gets
    // no grace: whatever arrived is the answer.
    if !timed_out {
        out.settle();
        err.settle();
    }
    let (stdout, out_cut) = out.take();
    let (stderr, err_cut) = err.take();

    Run::Done(Output {
        stdout,
        stderr,
        status,
        truncated: out_cut || err_cut,
        timed_out,
    })
}

fn kill_by_pid(pid: u32) {
    #[cfg(unix)]
    let mut command = {
        let mut command = std::process::Command::new("kill");
        command.args(["-9", &pid.to_string()]);
        command
    };
    #[cfg(windows)]
    let mut command = {
        let mut command = std::process::Command::new("taskkill");
        command.args(["/F", "/PID", &pid.to_string()]);
        command
    };
    let _ = command
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
}

#[derive(Clone)]
struct Captured {
    buffer: std::sync::Arc<std::sync::Mutex<(Vec<u8>, bool)>>,
    done: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl Captured {
    fn draining<R: std::io::Read + Send + 'static>(pipe: Option<R>) -> Self {
        let captured = Self {
            buffer: std::sync::Arc::new(std::sync::Mutex::new((Vec::new(), false))),
            done: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(pipe.is_none())),
        };
        let Some(mut pipe) = pipe else {
            return captured;
        };
        let sink = captured.clone();
        std::thread::spawn(move || {
            let mut chunk = [0_u8; 8192];
            loop {
                let read = match pipe.read(&mut chunk) {
                    Ok(0) | Err(_) => break,
                    Ok(read) => read,
                };
                let Ok(mut held) = sink.buffer.lock() else {
                    break;
                };
                // Reading past the cap is not merely wasteful: it is how a
                // program that prints without end takes the interface's memory
                // with it.
                if held.0.len() + read >= OUTPUT_CAP {
                    let room = OUTPUT_CAP.saturating_sub(held.0.len());
                    held.0.extend_from_slice(&chunk[..room]);
                    held.1 = true;
                    break;
                }
                held.0.extend_from_slice(&chunk[..read]);
            }
            sink.done.store(true, std::sync::atomic::Ordering::SeqCst);
        });
        captured
    }

    fn settle(&self) {
        let deadline = Instant::now() + Duration::from_millis(200);
        while !self.done.load(std::sync::atomic::Ordering::SeqCst) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    fn take(&self) -> (String, bool) {
        match self.buffer.lock() {
            Ok(held) => (String::from_utf8_lossy(&held.0).into_owned(), held.1),
            Err(_) => (String::new(), false),
        }
    }
}

pub fn command_for(program: &str, cwd: &PathBuf) -> std::process::Command {
    let mut command = if cfg!(windows) {
        let mut c = std::process::Command::new("cmd");
        c.arg("/C");
        c
    } else {
        let mut c = std::process::Command::new("sh");
        c.arg("-c");
        c
    };
    command.arg(program).current_dir(cwd);
    command
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn ask(key: &str) -> Ask {
        Ask {
            key: key.into(),
            program: "true".into(),
            target: Target::Session("s1".into()),
            ttl: DEFAULT_TTL,
            timeout: DEFAULT_TIMEOUT,
            refresh: false,
        }
    }

    fn counting() -> (Arc<Runner>, Arc<std::sync::atomic::AtomicUsize>) {
        let count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let seen = count.clone();
        let runner: Arc<Runner> = Arc::new(move |_ask: &Ask| {
            seen.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Run::Done(Output {
                stdout: "ok\n".into(),
                stderr: String::new(),
                status: Some(0),
                truncated: false,
                timed_out: false,
            })
        });
        (runner, count)
    }

    fn run_to_completion(store: &mut RunStore, runner: &Arc<Runner>) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            store.poll(runner);
            if store.running == 0 && store.queued.is_empty() {
                return;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        panic!("runs never settled");
    }

    #[test]
    fn a_fresh_answer_is_not_run_again() {
        // The property that makes "ask on every frame" the normal way to keep a
        // value current, rather than a mistake.
        let (runner, count) = counting();
        let mut store = RunStore::new();
        store.request("p", ask("k"), &runner);
        run_to_completion(&mut store, &runner);
        for _ in 0..10 {
            store.request("p", ask("k"), &runner);
        }
        run_to_completion(&mut store, &runner);
        assert_eq!(count.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[test]
    fn a_stale_answer_is_run_again_and_the_old_one_stays_readable() {
        let (runner, count) = counting();
        let mut store = RunStore::new();
        let mut first = ask("k");
        first.ttl = MIN_TTL;
        store.request("p", first.clone(), &runner);
        run_to_completion(&mut store, &runner);
        assert!(matches!(store.get("p", "k"), Some(Run::Done(_))));

        std::thread::sleep(MIN_TTL + Duration::from_millis(20));
        store.request("p", first, &runner);
        // Readable throughout: the previous answer is not cleared to make way.
        assert!(matches!(store.get("p", "k"), Some(Run::Done(_))));
        run_to_completion(&mut store, &runner);
        assert_eq!(count.load(std::sync::atomic::Ordering::SeqCst), 2);
    }

    #[test]
    fn asking_repeatedly_while_a_refresh_is_in_flight_starts_one_run() {
        let count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let seen = count.clone();
        // Slower than its TTL, which is the case that never settles.
        let runner: Arc<Runner> = Arc::new(move |_ask: &Ask| {
            seen.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            std::thread::sleep(Duration::from_millis(300));
            Run::Done(Output {
                stdout: "ok\n".into(),
                stderr: String::new(),
                status: Some(0),
                truncated: false,
                timed_out: false,
            })
        });

        let mut store = RunStore::new();
        let mut first = ask("k");
        first.ttl = MIN_TTL;
        store.request("p", first.clone(), &runner);
        run_to_completion(&mut store, &runner);
        assert_eq!(count.load(std::sync::atomic::Ordering::SeqCst), 1);

        std::thread::sleep(MIN_TTL + Duration::from_millis(20));

        // A render-loop's worth of asks against one stale key.
        let deadline = Instant::now() + Duration::from_millis(200);
        while Instant::now() < deadline {
            store.request("p", first.clone(), &runner);
            store.poll(&runner);
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(
            count.load(std::sync::atomic::Ordering::SeqCst),
            2,
            "a stale key being asked every frame must refresh once, not per frame"
        );
        run_to_completion(&mut store, &runner);
    }

    #[test]
    fn a_run_that_panics_frees_its_slot_and_can_be_asked_again() {
        let count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let seen = count.clone();
        let runner: Arc<Runner> = Arc::new(move |_ask: &Ask| {
            seen.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            panic!("the runner fell over");
        });

        let mut store = RunStore::new();
        let mut only = ask("k");
        only.refresh = true;

        let more_than_the_pool_has_slots = MAX_CONCURRENT + 2;
        for _ in 0..more_than_the_pool_has_slots {
            store.request("p", only.clone(), &runner);
            run_to_completion(&mut store, &runner);
        }

        assert_eq!(
            count.load(std::sync::atomic::Ordering::SeqCst),
            more_than_the_pool_has_slots,
            "a panicking run must leave the key askable and the pool intact"
        );
    }

    #[test]
    fn a_refresh_overrides_freshness() {
        let (runner, count) = counting();
        let mut store = RunStore::new();
        store.request("p", ask("k"), &runner);
        run_to_completion(&mut store, &runner);
        let mut again = ask("k");
        again.refresh = true;
        store.request("p", again, &runner);
        run_to_completion(&mut store, &runner);
        assert_eq!(count.load(std::sync::atomic::Ordering::SeqCst), 2);
    }

    #[test]
    fn a_mod_cannot_ask_for_the_same_run_twice_at_once() {
        // Two asks in one frame, or an ask while the first is still going, must
        // not become two processes.
        let (runner, count) = counting();
        let mut store = RunStore::new();
        store.request("p", ask("k"), &runner);
        store.request("p", ask("k"), &runner);
        store.request("p", ask("k"), &runner);
        run_to_completion(&mut store, &runner);
        assert_eq!(count.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[test]
    fn two_mods_may_use_the_same_key() {
        let (runner, _) = counting();
        let mut store = RunStore::new();
        store.request("a", ask("status"), &runner);
        store.request("b", ask("status"), &runner);
        run_to_completion(&mut store, &runner);
        assert!(store.get("a", "status").is_some());
        assert!(store.get("b", "status").is_some());
    }

    #[test]
    fn more_asks_than_the_bound_are_queued_and_all_run() {
        let (runner, count) = counting();
        let mut store = RunStore::new();
        for n in 0..(MAX_CONCURRENT * 3) {
            store.request("p", ask(&format!("k{n}")), &runner);
        }
        run_to_completion(&mut store, &runner);
        assert_eq!(
            count.load(std::sync::atomic::Ordering::SeqCst),
            MAX_CONCURRENT * 3,
            "queued asks must run, not be dropped"
        );
    }

    #[test]
    fn a_finished_run_is_readable_while_another_is_pending() {
        // What makes a composite pane possible: the slow one does not hold the
        // fast one.
        let slow: Arc<Runner> = Arc::new(|ask: &Ask| {
            if ask.key == "slow" {
                std::thread::sleep(Duration::from_millis(300));
            }
            Run::Done(Output {
                stdout: ask.key.clone(),
                stderr: String::new(),
                status: Some(0),
                truncated: false,
                timed_out: false,
            })
        });
        let mut store = RunStore::new();
        store.request("p", ask("slow"), &slow);
        store.request("p", ask("fast"), &slow);

        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            store.poll(&slow);
            if matches!(store.get("p", "fast"), Some(Run::Done(_))) {
                assert!(
                    matches!(store.get("p", "slow"), Some(Run::Pending)),
                    "the fast one landed first, which is the point"
                );
                return;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        panic!("the fast run never landed");
    }

    #[test]
    fn a_mod_that_went_away_leaves_nothing_behind() {
        let (runner, _) = counting();
        let mut store = RunStore::new();
        store.request("gone", ask("k"), &runner);
        run_to_completion(&mut store, &runner);
        store.retain_mods(&["still-here".to_string()]);
        assert!(store.get("gone", "k").is_none());
    }

    #[test]
    fn a_retired_mods_late_completion_cannot_recreate_its_answer() {
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let release_rx = std::sync::Mutex::new(release_rx);
        let runner: Arc<Runner> = Arc::new(move |ask: &Ask| {
            if ask.key == "retired" {
                started_tx.send(()).expect("started");
                release_rx
                    .lock()
                    .expect("release lock")
                    .recv_timeout(Duration::from_secs(5))
                    .expect("release retired request");
            }
            Run::Done(Output {
                stdout: ask.key.clone(),
                stderr: String::new(),
                status: Some(0),
                truncated: false,
                timed_out: false,
            })
        });
        let mut store = RunStore::new();
        for _ in 0..64 {
            store.retain_mods(&["gone".to_string(), "keeper".to_string()]);
            store.request("gone", ask("retired"), &runner);
            started_rx
                .recv_timeout(Duration::from_secs(5))
                .expect("retired request started");
            store.retain_mods(&["keeper".to_string()]);
            store.request("keeper", ask("live"), &runner);
            release_tx.send(()).expect("release");
            run_to_completion(&mut store, &runner);

            assert!(store.get("gone", "retired").is_none());
            assert!(store.for_mod("gone").is_empty());
            assert!(matches!(store.get("keeper", "live"), Some(Run::Done(_))));
            assert_eq!(store.answers.len(), 1);
            assert!(store.queued.is_empty());
            assert_eq!(store.running, 0);
        }
    }

    #[test]
    fn forgetting_and_reusing_a_key_accepts_only_its_new_request_in_either_finish_order() {
        for old_first in [true, false] {
            let (started_tx, started_rx) = std::sync::mpsc::channel();
            let (old_tx, old_rx) = std::sync::mpsc::channel();
            let (new_tx, new_rx) = std::sync::mpsc::channel();
            let old_rx = std::sync::Mutex::new(old_rx);
            let new_rx = std::sync::Mutex::new(new_rx);
            let runner: Arc<Runner> = Arc::new(move |ask| {
                started_tx.send(()).expect("started");
                let gate = if ask.program == "old" {
                    &old_rx
                } else {
                    &new_rx
                };
                gate.lock()
                    .expect("gate")
                    .recv_timeout(Duration::from_secs(5))
                    .expect("release");
                Run::Failed(ask.program.clone())
            });
            let mut store = RunStore::new();
            let mut old = ask("same");
            old.program = "old".into();
            store.request("p", old, &runner);
            started_rx
                .recv_timeout(Duration::from_secs(5))
                .expect("old started");
            let retired = Arc::downgrade(
                &store
                    .answers
                    .get(&("p".into(), "same".into()))
                    .expect("answer")
                    .identity,
            );
            assert!(store.forget("p", "same"));
            assert!(!store.forget("p", "same"));
            assert!(
                !store.is_empty(),
                "forgotten active captures still need polling"
            );
            let mut new = ask("same");
            new.program = "new".into();
            store.request("p", new, &runner);
            started_rx
                .recv_timeout(Duration::from_secs(5))
                .expect("new started");
            let (first, last) = if old_first {
                (&old_tx, &new_tx)
            } else {
                (&new_tx, &old_tx)
            };
            first.send(()).expect("first release");
            let deadline = Instant::now() + Duration::from_secs(5);
            while store.running == 2 && Instant::now() < deadline {
                store.poll(&runner);
                std::thread::sleep(Duration::from_millis(1));
            }
            assert_eq!(store.running, 1);
            if old_first {
                assert!(matches!(store.get("p", "same"), Some(Run::Pending)));
            } else {
                assert_eq!(store.get("p", "same"), Some(&Run::Failed("new".into())));
            }
            last.send(()).expect("last release");
            run_to_completion(&mut store, &runner);
            assert_eq!(store.get("p", "same"), Some(&Run::Failed("new".into())));
            assert!(
                retired.upgrade().is_none(),
                "retired request identity leaked"
            );
            assert_eq!(store.answers.len(), 1);
            assert!(store.queued.is_empty());
        }
    }

    #[test]
    fn forgotten_key_churn_leaves_no_retained_request_state() {
        let (runner, count) = counting();
        let mut store = RunStore::new();
        store.request("keeper", ask("same"), &runner);
        run_to_completion(&mut store, &runner);
        for index in 0..128 {
            let key = format!("host:retired-{index}");
            store.request("p", ask(&key), &runner);
            run_to_completion(&mut store, &runner);
            let retired = Arc::downgrade(
                &store
                    .answers
                    .get(&("p".into(), key.clone()))
                    .expect("answer")
                    .identity,
            );
            assert!(store.forget("p", &key));
            assert!(!store.forget("p", &key));
            assert!(retired.upgrade().is_none());
            assert!(store.for_mod("p").is_empty());
            assert_eq!(store.answers.len(), 1);
            assert!(store.queued.is_empty());
            assert_eq!(store.running, 0);
            assert!(matches!(store.get("keeper", "same"), Some(Run::Done(_))));
        }
        assert_eq!(count.load(std::sync::atomic::Ordering::SeqCst), 129);
    }

    #[test]
    fn what_a_mod_asked_for_is_clamped_to_what_the_kernel_will_do() {
        let greedy = Ask {
            ttl: Duration::ZERO,
            timeout: Duration::from_secs(86_400),
            ..ask("k")
        }
        .clamped();
        assert_eq!(greedy.ttl, MIN_TTL);
        assert_eq!(greedy.timeout, MAX_TIMEOUT);
    }

    #[test]
    fn output_is_captured_with_its_status() {
        let mut command = std::process::Command::new("sh");
        command.arg("-c").arg("echo out; echo err >&2; exit 3");
        match capture(command, DEFAULT_TIMEOUT) {
            Run::Done(output) => {
                assert_eq!(output.stdout.trim(), "out");
                assert_eq!(output.stderr.trim(), "err");
                assert_eq!(output.status, Some(3));
                assert!(!output.truncated);
                assert!(!output.timed_out);
            }
            other => panic!("expected output, got {other:?}"),
        }
    }

    #[test]
    fn a_program_that_never_exits_is_given_up_on_promptly() {
        // `sh -c "sleep 30"` hands its stdout to a GRANDCHILD, so killing the
        // shell leaves the pipe open. Waiting for the reader would then block for
        // the full thirty seconds — the very hang the timeout exists to prevent,
        // one level down. So the elapsed time is asserted, not just the flag.
        let mut command = std::process::Command::new("sh");
        command.arg("-c").arg("sleep 30");
        let started = Instant::now();
        match capture(command, Duration::from_millis(200)) {
            Run::Done(output) => assert!(output.timed_out, "{output:?}"),
            other => panic!("expected a timed-out run, got {other:?}"),
        }
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "gave up after {:?}, which is not giving up",
            started.elapsed()
        );
    }

    #[test]
    fn a_program_that_does_not_exist_fails_rather_than_reporting_nothing() {
        let command = std::process::Command::new("definitely-not-a-program-xyz");
        assert!(matches!(capture(command, DEFAULT_TIMEOUT), Run::Failed(_)));
    }

    #[test]
    fn output_past_the_cap_is_truncated_and_says_so() {
        let mut command = std::process::Command::new("sh");
        // Comfortably past the cap, cheaply.
        command
            .arg("-c")
            .arg("yes abcdefghijklmnopqrstuvwxyz | head -c 400000");
        match capture(command, DEFAULT_TIMEOUT) {
            Run::Done(output) => {
                assert!(output.truncated, "the cap was not applied");
                assert!(output.stdout.len() <= OUTPUT_CAP);
            }
            other => panic!("expected output, got {other:?}"),
        }
    }
}
