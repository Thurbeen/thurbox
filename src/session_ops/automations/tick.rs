//! Firing what is due, and firing one automation now.
//!
//! **The run row.** A fire opens its row as `running` in the same transaction
//! as the claim that wins it, with a deadline: the automation's timeout plus
//! [`DEADLINE_GRACE`]. The fire writes the outcome over it. A row still
//! `running` past its deadline is marked `timed_out` by the next tick, so a
//! firer that hung and one that died both leave a record (issue #1508).
//!
//! **The run lock.** Every fire holds `lock:run:<id>` for its length — the
//! tick's and `run --wait`'s alike — so a manual run never overlaps a
//! scheduled one of the same automation, and the second asker is told rather
//! than queued.
//!
//! **The wall time.** Due execs run on worker threads, at most
//! [`EXEC_CONCURRENCY`] at once, each with its own deadline, while this thread
//! fires the sends and spawns and then runs the caller's housekeeping. A tick
//! takes as long as its longest exec, never the sum, and a hung exec delays
//! nothing but its own record (issue #1507).

use std::collections::VecDeque;
use std::sync::{mpsc, Mutex};
use std::time::Duration;

use crate::session::{Automation, AutomationRun, AutomationRunStatus};
use crate::storage::automations::RunRecord;
use crate::storage::Database;
use crate::sync::current_time_millis;

use super::super::lock::{self, Lock, Subject};
use super::fire::{fire, ExecJob};

/// Due execs that run at once. Small: a heartbeat on a laptop should not fork
/// a dozen jobs in the same second, and four covers every shipped extension's
/// schedule colliding at the top of an hour.
pub const EXEC_CONCURRENCY: usize = 4;

/// How long past its timeout a run row stays `running` before a tick calls it
/// timed out. An exec is killed at its timeout, so this only has to cover
/// recording it. A firer that records later than that — a tick still in its
/// housekeeping, a spawn that ran long — writes its real outcome over the
/// `timed_out` anyway ([`Database::finish_automation_run`]).
pub const DEADLINE_GRACE: Duration = Duration::from_secs(5);

/// One automation the tick fired.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fired {
    pub automation_id: i64,
    pub run_id: i64,
    pub status: AutomationRunStatus,
    pub detail: String,
    /// Set when the run's outcome could not be written — the row then still
    /// reads `running`, and this says why.
    pub history_error: Option<String>,
}

/// One due automation the tick did not fire, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Skipped {
    pub automation_id: i64,
    /// `claim-lost` (another firer claimed it), `run-in-flight` (a run of it
    /// is still going) or `error` (its lock or claim could not be written;
    /// `detail` says why, and it is due again next tick).
    pub reason: &'static str,
    pub detail: Option<String>,
}

impl Skipped {
    fn new(automation_id: i64, reason: &'static str) -> Self {
        Self {
            automation_id,
            reason,
            detail: None,
        }
    }
}

/// What the due pass did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DuePass {
    pub fired: Vec<Fired>,
    pub skipped: Vec<Skipped>,
    /// Rows found still `running` past their deadline and marked timed out.
    pub timed_out: usize,
}

impl DuePass {
    /// Fires that did not succeed.
    pub fn failed(&self) -> usize {
        self.fired
            .iter()
            .filter(|f| {
                matches!(
                    f.status,
                    AutomationRunStatus::Error | AutomationRunStatus::TimedOut
                )
            })
            .count()
    }
}

/// When a run claimed at `now`, that may take `budget`, counts as timed out.
fn deadline(budget: Duration, now: u64) -> u64 {
    now.saturating_add((budget + DEADLINE_GRACE).as_millis() as u64)
}

/// Take automation `id`'s run lock for a run that may take `budget`.
fn run_lock<'a>(
    db: &'a Database,
    id: i64,
    budget: Duration,
    op: &str,
) -> Result<Option<Lock<'a>>, String> {
    lock::lock_for(db, Subject::AutomationRun(id), op, budget + DEADLINE_GRACE)
}

/// Fire every automation due at `now`, and run `housekeeping` while the execs
/// run.
///
/// `Err` is a tick that could not look for what is due — the storage failure
/// the tick log records. A fire that fails is a run row saying so, not an
/// error here.
pub fn tick<T>(
    db: &Database,
    backends: &crate::backend::BackendRegistry,
    now: u64,
    housekeeping: impl FnOnce() -> T,
) -> Result<(DuePass, T), String> {
    let mut pass = DuePass {
        timed_out: db
            .time_out_stale_runs(now)
            .map_err(|e| format!("time out stale runs: {e}"))?,
        ..DuePass::default()
    };
    let due = db
        .due_automations(now)
        .map_err(|e| format!("due_automations: {e}"))?;

    // Claimed fires, split by what they need: an exec needs only a process,
    // a send or spawn needs this thread's database and backends.
    let mut locks: Vec<(i64, Lock<'_>)> = Vec::new();
    let mut execs: VecDeque<(i64, ExecJob)> = VecDeque::new();
    let mut inline: Vec<(i64, Automation)> = Vec::new();
    // Execs start in waves of EXEC_CONCURRENCY, so a queued one may wait for
    // the waves before it — each as long as the longest exec — before its own
    // timeout even starts. Its lock and its row's deadline cover that wait,
    // or a slow queue would see its last runs called timed out, and fired a
    // second time, while they are still running.
    let longest_exec = due
        .iter()
        .filter(|a| ExecJob::of(a).is_some())
        .map(|a| a.run.timeout())
        .max()
        .unwrap_or_default();
    for auto in due {
        let job = ExecJob::of(&auto);
        let budget = match &job {
            Some(_) => longest_exec * (execs.len() / EXEC_CONCURRENCY) as u32 + auto.run.timeout(),
            None => auto.run.timeout(),
        };
        // One automation's lock or claim that cannot be written skips that
        // automation, not the tick: the ones already claimed have rows saying
        // `running`, and leaving them unfired would read as a hang.
        let held = match run_lock(db, auto.id, budget, "tick") {
            Ok(Some(held)) => held,
            Ok(None) => {
                pass.skipped.push(Skipped::new(auto.id, "run-in-flight"));
                continue;
            }
            Err(e) => {
                pass.skipped.push(Skipped {
                    detail: Some(e),
                    ..Skipped::new(auto.id, "error")
                });
                continue;
            }
        };
        let next = auto.schedule.next_after(now, auto.timezone.as_deref());
        let claimed = db.claim_and_open_run(
            auto.id,
            auto.next_run_at.unwrap_or(0),
            next,
            now,
            deadline(budget, now),
        );
        let run_id = match claimed {
            Ok(Some(run_id)) => run_id,
            Ok(None) => {
                // Another firer won the claim. This logs at debug! (invisible
                // at the default level), so it is reported in the pass too.
                tracing::debug!(
                    automation_id = auto.id,
                    "automation claim lost to a concurrent firer"
                );
                pass.skipped.push(Skipped::new(auto.id, "claim-lost"));
                continue;
            }
            Err(e) => {
                pass.skipped.push(Skipped {
                    detail: Some(format!("claim_due_automation: {e}")),
                    ..Skipped::new(auto.id, "error")
                });
                continue;
            }
        };
        locks.push((run_id, held));
        match job {
            Some(job) => execs.push_back((run_id, job)),
            None => inline.push((run_id, auto)),
        }
    }

    let queue = Mutex::new(execs);
    let workers = queue
        .lock()
        .map(|q| q.len().min(EXEC_CONCURRENCY))
        .unwrap_or(0);
    let (tx, rx) = mpsc::channel::<(i64, i64, RunRecord)>();
    let kept = std::thread::scope(|scope| {
        for _ in 0..workers {
            let tx = tx.clone();
            let queue = &queue;
            scope.spawn(move || loop {
                let next = queue.lock().ok().and_then(|mut q| q.pop_front());
                let Some((run_id, job)) = next else {
                    break;
                };
                let automation_id = job.automation_id;
                let _ = tx.send((run_id, automation_id, job.run()));
            });
        }
        drop(tx);

        for (run_id, auto) in &inline {
            let record = fire(db, backends, auto);
            pass.fired.push(finish(db, auto.id, *run_id, &record));
            release(&mut locks, *run_id);
        }
        let kept = housekeeping();
        for (run_id, automation_id, record) in rx {
            pass.fired.push(finish(db, automation_id, run_id, &record));
            release(&mut locks, run_id);
        }
        kept
    });
    Ok((pass, kept))
}

/// Write a fire's outcome over its run row, reporting a write that failed with
/// the fire rather than dropping it: a row left `running` by it must not be
/// read as an action that never returned.
fn finish(db: &Database, automation_id: i64, run_id: i64, record: &RunRecord) -> Fired {
    let history_error = db.finish_automation_run(run_id, record).err().map(|e| {
        tracing::warn!(automation_id, "run history not recorded: {e}");
        e.to_string()
    });
    Fired {
        automation_id,
        run_id,
        status: record.status,
        detail: record.detail.clone(),
        history_error,
    }
}

fn release(locks: &mut Vec<(i64, Lock<'_>)>, run_id: i64) {
    locks.retain(|(held, _)| *held != run_id);
}

/// Why `run --wait` fired nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunNowError {
    /// The automation is disabled and the caller did not force it.
    Disabled(String),
    /// A run of it is in flight.
    InFlight(String),
    /// It was deleted before or while it ran.
    Gone(String),
    /// The database could not be asked or written.
    Storage(String),
}

impl std::fmt::Display for RunNowError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RunNowError::Disabled(m)
            | RunNowError::InFlight(m)
            | RunNowError::Gone(m)
            | RunNowError::Storage(m) => f.write_str(m),
        }
    }
}

/// Fire automation `id` now, on this thread, and answer with its run — what
/// `automation run --wait` does (issue #1517).
///
/// Only this one automation: no other due row, no self-heal, sync, reap or
/// export. It holds the run lock, so a run already in flight is refused. A
/// due automation is claimed exactly as the tick claims it, so its next run
/// moves on and the heartbeat does not fire it again; one that was not due
/// keeps its schedule, so a manual run never shifts it. `timeout` replaces an
/// exec's own deadline for this run.
pub fn run_now(
    db: &Database,
    backends: &crate::backend::BackendRegistry,
    id: i64,
    force: bool,
    timeout: Option<Duration>,
) -> Result<AutomationRun, RunNowError> {
    let storage = |e: String| RunNowError::Storage(e);
    let read = |db: &Database| match db.get_automation(id) {
        Ok(Some(auto)) => Ok(auto),
        Ok(None) => Err(RunNowError::Gone(format!(
            "automation #{id} no longer exists"
        ))),
        Err(e) => Err(RunNowError::Storage(format!("get automation: {e}"))),
    };
    let mut auto = read(db)?;
    if let Some(timeout) = timeout {
        if timeout.is_zero() {
            return Err(storage("the timeout must be at least one second".into()));
        }
        auto.run.timeout_secs = Some(timeout.as_secs().max(1));
    }
    if !auto.enabled && !force {
        return Err(RunNowError::Disabled(format!(
            "automation #{id} is disabled; pass --force to run it once, or enable it with \
             `automation edit {id} --enabled`"
        )));
    }
    let budget = auto.run.timeout();
    let Some(_held) = run_lock(db, id, budget, "run").map_err(storage)? else {
        return Err(in_flight(id));
    };
    // Read again under the lock: a tick that finished just before it may have
    // moved the schedule, and a delete may have taken it.
    let fresh = read(db)?;
    auto.next_run_at = fresh.next_run_at;
    auto.enabled = fresh.enabled;
    let now = current_time_millis();
    let due = auto.enabled && auto.next_run_at.is_some_and(|at| at <= now);
    let run_id = if due {
        let next = auto.schedule.next_after(now, auto.timezone.as_deref());
        db.claim_and_open_run(
            id,
            auto.next_run_at.unwrap_or(0),
            next,
            now,
            deadline(budget, now),
        )
        .map_err(|e| storage(format!("claim_due_automation: {e}")))?
        // Lost to a firer that takes no lock — an older thurbox's tick.
        .ok_or_else(|| in_flight(id))?
    } else {
        db.start_automation_run(id, now, Some(deadline(budget, now)))
            .map_err(|e| storage(format!("start_automation_run: {e}")))?
    };
    let record = match ExecJob::of(&auto) {
        Some(job) => job.run(),
        None => fire(db, backends, &auto),
    };
    db.finish_automation_run(run_id, &record)
        .map_err(|e| storage(format!("record the run: {e}")))?;
    // A delete during the run took its history with it.
    db.get_automation_run(run_id)
        .map_err(|e| storage(format!("read the run back: {e}")))?
        .ok_or_else(|| {
            RunNowError::Gone(format!(
                "automation #{id} was deleted while it ran ({}: {})",
                record.status.as_str(),
                record.detail
            ))
        })
}

fn in_flight(id: i64) -> RunNowError {
    RunNowError::InFlight(format!(
        "a run of automation #{id} is in flight; wait for it to finish"
    ))
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::session::{AutomationAction, AutomationSchedule, RunSettings};
    use crate::storage::automations::NewAutomation;

    fn exec(db: &Database, name: &str, timeout_secs: u64) -> i64 {
        db.create_automation(&NewAutomation {
            name: name.into(),
            enabled: true,
            schedule: AutomationSchedule::Cron {
                expr: "0 * * * *".into(),
            },
            timezone: None,
            action: AutomationAction::Exec {
                command: "true".into(),
            },
            prompt: String::new(),
            next_run_at: Some(1),
            key: None,
            run: RunSettings {
                timeout_secs: Some(timeout_secs),
                ..RunSettings::default()
            },
        })
        .expect("insert")
    }

    fn deadline_of(db: &Database, automation_id: i64) -> u64 {
        db.conn_ref()
            .query_row(
                "SELECT deadline_at FROM automation_runs WHERE automation_id = ?1",
                [automation_id],
                |row| row.get::<_, i64>(0),
            )
            .expect("a run row") as u64
    }

    /// The fifth exec waits for a slot behind four others, so its lock and its
    /// row's deadline cover that wait as well as its own timeout.
    #[test]
    fn a_queued_exec_s_deadline_covers_its_wait_for_a_slot() {
        let temp = tempfile::TempDir::new().unwrap();
        let _guard = crate::paths::TestPathGuard::new(temp.path());
        let db = Database::open_in_memory().expect("db");
        let ids: Vec<i64> = (0..=EXEC_CONCURRENCY)
            .map(|n| exec(&db, &format!("e{n}"), 100))
            .collect();
        let now = 1_000;
        let (pass, ()) = tick(&db, &crate::backend::registry::inert(), now, || ()).expect("tick");
        assert_eq!(pass.fired.len(), ids.len(), "{pass:?}");
        let grace = DEADLINE_GRACE.as_millis() as u64;
        assert_eq!(deadline_of(&db, ids[0]), now + 100_000 + grace);
        assert_eq!(
            deadline_of(&db, ids[EXEC_CONCURRENCY]),
            now + 200_000 + grace,
            "one wave of the longest exec, then its own timeout"
        );
    }

    /// A claim that cannot be written skips that automation; the others are
    /// still fired, rather than claimed and abandoned.
    #[test]
    fn one_automation_s_failed_claim_does_not_strand_the_others() {
        let temp = tempfile::TempDir::new().unwrap();
        let _guard = crate::paths::TestPathGuard::new(temp.path());
        let db = Database::open_in_memory().expect("db");
        let first = exec(&db, "first", 30);
        let broken = exec(&db, "broken", 30);
        let last = exec(&db, "last", 30);
        db.conn_ref()
            .execute_batch(&format!(
                "CREATE TRIGGER refuse_run BEFORE INSERT ON automation_runs \
                 WHEN NEW.automation_id = {broken} \
                 BEGIN SELECT RAISE(ABORT, 'refused'); END;"
            ))
            .expect("trigger");
        let (pass, ()) = tick(&db, &crate::backend::registry::inert(), 1_000, || ()).expect("tick");
        let mut fired: Vec<i64> = pass.fired.iter().map(|f| f.automation_id).collect();
        fired.sort_unstable();
        assert_eq!(fired, [first, last]);
        assert_eq!(pass.skipped.len(), 1, "{pass:?}");
        assert_eq!(pass.skipped[0].automation_id, broken);
        assert_eq!(pass.skipped[0].reason, "error");
        assert!(
            pass.skipped[0]
                .detail
                .as_deref()
                .is_some_and(|d| d.contains("refused")),
            "{pass:?}"
        );
    }

    #[test]
    fn running_an_automation_that_is_gone_says_so() {
        let temp = tempfile::TempDir::new().unwrap();
        let _guard = crate::paths::TestPathGuard::new(temp.path());
        let db = Database::open_in_memory().expect("db");
        let error = run_now(&db, &crate::backend::registry::inert(), 404, false, None)
            .expect_err("nothing to run");
        assert!(matches!(error, RunNowError::Gone(_)), "{error:?}");
    }
}
