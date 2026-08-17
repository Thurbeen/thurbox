//! Programs a plugin asked to be run, and what they printed.
//!
//! The eighth worker. A plugin cannot call anything that waits, so running a
//! program is an *ask* (a command) whose answer arrives as a *read* (published
//! next frame) — the split the kernel already enforces everywhere else, applied
//! to the one thing that most obviously cannot block a render: a process.
//!
//! Three properties are load-bearing and each is a bound rather than a
//! convention, because the caller is a file the user dropped in a directory and
//! the interface has to survive it being written badly:
//!
//! - **Asking is idempotent while the answer is fresh.** A pane keeps `git
//!   status` current by asking on every frame; a TTL is what makes that cost one
//!   map lookup instead of one process.
//! - **Output is capped and runs time out**, so a program that prints without
//!   end, or never exits, costs a truncated answer rather than the interface.
//! - **Concurrency is bounded and the excess queues**, so twenty asks are slow,
//!   not a fork bomb.
//!
//! Modelled on `kernel::diff` (request → worker → poll → publish) with
//! `kernel::repos`'s staleness, so there is one shape to learn rather than three.

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender};
use std::time::{Duration, Instant};

/// Most output kept per stream. Beyond this the run is truncated and says so.
const OUTPUT_CAP: usize = 256 * 1024;

/// How long a program may run before it is given up on.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

/// The longest timeout a plugin may ask for. `npm ci` legitimately takes
/// minutes; nothing legitimately takes an hour, and a plugin that asks for one
/// has made a mistake the kernel should not honour.
const MAX_TIMEOUT: Duration = Duration::from_secs(600);

/// How long an answer stays fresh when the asker does not say.
pub const DEFAULT_TTL: Duration = Duration::from_secs(2);

/// The shortest TTL the kernel will honour.
///
/// A plugin asking for zero would run a process per frame — which is not a
/// choice it should be able to make, since the cost lands on the whole
/// interface rather than on that pane.
const MIN_TTL: Duration = Duration::from_millis(250);

/// Programs running at once. The rest queue.
const MAX_CONCURRENT: usize = 4;

/// What a plugin asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ask {
    /// The plugin's own name for this run. Namespaced by the kernel, so two
    /// plugins may both call theirs `status`.
    pub key: String,
    /// The command line, run through the session's own shell.
    pub program: String,
    /// The session whose directory — and whose machine — it runs in.
    pub session: String,
    pub ttl: Duration,
    pub timeout: Duration,
    /// Run it again even if the answer is fresh.
    pub refresh: bool,
}

impl Ask {
    /// Clamp what a plugin asked for into what the kernel will do.
    ///
    /// Applied here rather than at the worker so the stored TTL is the honoured
    /// one — otherwise freshness would be judged against a number nobody used.
    pub fn clamped(mut self) -> Self {
        self.ttl = self.ttl.max(MIN_TTL);
        self.timeout = self.timeout.min(MAX_TIMEOUT);
        self
    }
}

/// What a program did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Run {
    /// Asked for, not finished. Distinct from a finished run that printed
    /// nothing, which is the distinction a pane needs to avoid drawing "no
    /// containers" while `docker` is still starting.
    Pending,
    Done(Output),
    /// It could not be run at all — no such session, a host that is not
    /// configured, a program that could not be started.
    Failed(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Output {
    pub stdout: String,
    pub stderr: String,
    /// `None` when the program was killed by a signal or timed out.
    pub status: Option<i32>,
    /// Either stream hit the cap.
    pub truncated: bool,
    /// It ran out of time rather than finishing.
    pub timed_out: bool,
}

/// One finished run, on its way back to the loop.
struct Finished {
    plugin: String,
    key: String,
    run: Run,
    at: Instant,
}

/// Sends its `Finished` when the worker thread's frame goes away, however it
/// goes away.
///
/// A plain `tx.send` after the call is skipped by an unwinding panic, and the
/// loop's bookkeeping — the concurrency slot and the key's in-flight marker — is
/// only ever cleared by a message arriving. So the message is owed on every exit
/// path, which is what `Drop` is for.
struct Report {
    tx: Sender<Finished>,
    plugin: String,
    key: String,
    run: Option<Run>,
}

impl Drop for Report {
    fn drop(&mut self) {
        let _ = self.tx.send(Finished {
            plugin: std::mem::take(&mut self.plugin),
            key: std::mem::take(&mut self.key),
            run: self.run.take().unwrap_or(Run::Pending),
            at: Instant::now(),
        });
    }
}

/// A stored answer and when it landed.
struct Answer {
    run: Run,
    /// `None` while pending — freshness is a property of an answer, and a run
    /// still going has not produced one.
    at: Option<Instant>,
    ttl: Duration,
    /// A run for this key is queued or executing.
    ///
    /// Tracked separately because `Run::Pending` cannot answer it: a refresh
    /// deliberately leaves the previous answer in place — that is what stops a
    /// pane blinking to "pending" every time its value renews — so the entry
    /// still reads `Done` while the next run is in flight. Without this marker
    /// every frame's ask started another process, and asking every frame is the
    /// documented way to use `run`.
    inflight: bool,
}

impl Answer {
    fn stale(&self, now: Instant) -> bool {
        match self.at {
            None => false,
            Some(at) => now.duration_since(at) >= self.ttl,
        }
    }
}

/// What the loop must do to serve a queued ask: it owns the session lookup and
/// the host resolution, neither of which belongs in a store.
pub type Runner = dyn Fn(&Ask) -> Run + Send + Sync;

/// Every run a plugin asked for, keyed by `(plugin, key)`.
#[derive(Default)]
pub struct RunStore {
    answers: HashMap<(String, String), Answer>,
    /// Asks waiting for a slot, in the order they arrived.
    queued: VecDeque<(String, Ask)>,
    running: usize,
    channel: Option<(Sender<Finished>, Receiver<Finished>)>,
}

impl RunStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Ask for a program, or do nothing because the answer is still fresh.
    ///
    /// Returns whether anything was started or queued, which the caller uses to
    /// decide nothing more than whether to repaint.
    pub fn request(&mut self, plugin: &str, ask: Ask, runner: &std::sync::Arc<Runner>) -> bool {
        let ask = ask.clamped();
        let id = (plugin.to_string(), ask.key.clone());
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
        let entry = self.answers.entry(id).or_insert_with(|| Answer {
            run: Run::Pending,
            at: None,
            ttl: ask.ttl,
            inflight: false,
        });
        entry.inflight = true;

        self.queued.push_back((plugin.to_string(), ask));
        self.drain_queue(runner);
        true
    }

    /// Start whatever the concurrency bound allows.
    fn drain_queue(&mut self, runner: &std::sync::Arc<Runner>) {
        while self.running < MAX_CONCURRENT {
            let Some((plugin, ask)) = self.queued.pop_front() else {
                return;
            };
            self.running += 1;
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
                    plugin,
                    key: ask.key.clone(),
                    run: Some(Run::Failed(format!("{}: the run panicked", ask.program))),
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

    /// Fold finished runs in and start any that were waiting. True when
    /// anything arrived, so the caller can repaint.
    pub fn poll(&mut self, runner: &std::sync::Arc<Runner>) -> bool {
        let mut changed = false;
        let finished: Vec<Finished> = match &self.channel {
            Some((_, rx)) => rx.try_iter().collect(),
            None => Vec::new(),
        };
        for done in finished {
            self.running = self.running.saturating_sub(1);
            let id = (done.plugin, done.key);
            let ttl = self.answers.get(&id).map(|a| a.ttl).unwrap_or(DEFAULT_TTL);
            self.answers.insert(
                id,
                Answer {
                    run: done.run,
                    at: Some(done.at),
                    ttl,
                    inflight: false,
                },
            );
            changed = true;
        }
        if changed {
            self.drain_queue(runner);
        }
        changed
    }

    /// Has anything ever been asked for?
    ///
    /// The common answer is no — most interfaces have no plugin that runs
    /// programs — and the loop checks it every iteration, so it is one bool
    /// rather than a walk.
    pub fn is_empty(&self) -> bool {
        self.answers.is_empty() && self.queued.is_empty()
    }

    pub fn get(&self, plugin: &str, key: &str) -> Option<&Run> {
        self.answers
            .get(&(plugin.to_string(), key.to_string()))
            .map(|answer| &answer.run)
    }

    /// Every answer belonging to one plugin, for publishing.
    pub fn for_plugin<'a>(&'a self, plugin: &str) -> Vec<(&'a str, &'a Run)> {
        self.answers
            .iter()
            .filter(|((owner, _), _)| owner == plugin)
            .map(|((_, key), answer)| (key.as_str(), &answer.run))
            .collect()
    }

    /// Forget everything belonging to plugins that no longer exist.
    ///
    /// Called after a reload: a plugin that was edited away, renamed or removed
    /// must not leave its answers behind to accumulate across reloads.
    pub fn retain_plugins(&mut self, live: &[String]) {
        self.answers.retain(|(plugin, _), _| live.contains(plugin));
        self.queued.retain(|(plugin, _)| live.contains(plugin));
    }
}

/// Run a command line and capture what it printed, bounded.
///
/// Shared by the local and remote paths: both end in a `Command` that has
/// already been pointed at the right machine, so the capping, the timeout and
/// the status reading happen once.
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

    let deadline = Instant::now() + timeout;
    let (status, timed_out) = loop {
        match child.try_wait() {
            Ok(Some(status)) => break (status.code(), false),
            Ok(None) if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                break (None, true);
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(10)),
            Err(e) => return Run::Failed(format!("waiting for it failed: {e}")),
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

/// One pipe being read into a buffer the caller can take at any time.
#[derive(Clone)]
struct Captured {
    buffer: std::sync::Arc<std::sync::Mutex<(Vec<u8>, bool)>>,
    done: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl Captured {
    /// Start draining `pipe` on its own thread, stopping at the cap.
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

    /// Wait briefly for the reader to finish. Bounded, because "the program
    /// exited" does not guarantee the reader has been scheduled.
    fn settle(&self) {
        let deadline = Instant::now() + Duration::from_millis(200);
        while !self.done.load(std::sync::atomic::Ordering::SeqCst) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    /// Whatever has arrived so far, and whether it was cut at the cap.
    fn take(&self) -> (String, bool) {
        match self.buffer.lock() {
            Ok(held) => (String::from_utf8_lossy(&held.0).into_owned(), held.1),
            Err(_) => (String::new(), false),
        }
    }
}

/// The command line a plugin asked for, pointed at the machine it belongs to.
///
/// Local runs go through the platform shell in the session's directory; remote
/// ones through the host launcher, which is the same path `sync` and the diff
/// take. Building it here means the *decision* about where is made once, and
/// `capture` never has to know.
pub fn command_for(
    program: &str,
    cwd: &PathBuf,
    host: Option<&crate::session::HostDef>,
) -> std::process::Command {
    match host {
        None => {
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
        Some(host) => {
            // The directory is on the host, so `cd` is part of the script rather
            // than a `current_dir` on this machine. Quoted, because a worktree
            // path can contain anything a filesystem allows.
            let script = format!(
                "cd {} && {program}",
                crate::shell::posix_quote(&cwd.to_string_lossy())
            );
            crate::git::host_shell_c(host, &script)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn ask(key: &str) -> Ask {
        Ask {
            key: key.into(),
            program: "true".into(),
            session: "s1".into(),
            ttl: DEFAULT_TTL,
            timeout: DEFAULT_TIMEOUT,
            refresh: false,
        }
    }

    /// A runner that answers immediately and counts how often it was called.
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

    /// Drive the store until everything queued has landed.
    ///
    /// Named apart from `Captured::settle`, which waits on one pipe: sharing a
    /// verb for "wait for a reader" and "wait for every run" made two very
    /// different waits read the same.
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

    /// Asking every frame is the documented way to use `run`, so asking while a
    /// run is in flight has to be free.
    ///
    /// It was not: a refresh leaves the old answer `Done` on purpose, and
    /// `Run::Pending` was the only "already going" test — so once an answer went
    /// stale, every ask started another process until the whole concurrency pool
    /// was full. A program slower than its own TTL never settled and held all four
    /// slots for good, starving every other key and plugin.
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

    /// A panicking runner must not cost a slot: the message the store's
    /// bookkeeping waits on is owed on every exit path, so it is sent from a drop
    /// guard. Four leaked slots would wedge every run in the interface.
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
        only.ttl = MIN_TTL;

        // More panicking runs than the pool has slots.
        for _ in 0..(MAX_CONCURRENT + 2) {
            store.request("p", only.clone(), &runner);
            run_to_completion(&mut store, &runner);
            std::thread::sleep(MIN_TTL + Duration::from_millis(20));
        }

        assert_eq!(
            count.load(std::sync::atomic::Ordering::SeqCst),
            MAX_CONCURRENT + 2,
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
    fn a_plugin_cannot_ask_for_the_same_run_twice_at_once() {
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
    fn two_plugins_may_use_the_same_key() {
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
    fn a_plugin_that_went_away_leaves_nothing_behind() {
        let (runner, _) = counting();
        let mut store = RunStore::new();
        store.request("gone", ask("k"), &runner);
        run_to_completion(&mut store, &runner);
        store.retain_plugins(&["still-here".to_string()]);
        assert!(store.get("gone", "k").is_none());
    }

    #[test]
    fn what_a_plugin_asked_for_is_clamped_to_what_the_kernel_will_do() {
        // A plugin choosing its own bounds could take the interface down, so the
        // numbers it offers are a preference, not a decision.
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
