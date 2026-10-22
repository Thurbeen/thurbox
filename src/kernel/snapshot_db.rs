//! The snapshot's database connection, on a thread of its own.
//!
//! Rule 5 of the kernel — anything touching the world runs on a worker —
//! applied to SQLite (issue #1474). Every statement the snapshot runs, read or
//! write, goes through one queue to one thread that owns the connection, so a
//! lock another process holds costs that thread its busy wait and the event
//! loop nothing.
//!
//! One thread, not a pool: the queue is the ordering. A read sent after a
//! write sees it, which is what lets [`super::snapshot::SnapshotStore`] tell a
//! read that predates its own in-place corrections from one that includes
//! them — see [`Reply::Rows`].

use std::collections::{HashMap, HashSet};
use std::sync::mpsc::{self, Receiver, Sender};

use super::snapshot::{AutomationRow, DeletedRow, RunRow, TaskRow};
use crate::session::SessionId;
use crate::storage::{Database, HookRow};
use crate::sync::SharedSession;

/// What the store asks of the worker. Answered in the order sent.
pub(crate) enum Job {
    /// Re-read whatever moved since the last read. `force` skips the
    /// `data_version` gate and reads every row.
    Read {
        force: bool,
    },
    Write(Write),
    /// Answered with [`Reply::Settled`] once everything sent before it has
    /// been answered.
    Settle(u64),
}

/// A write the store has already applied to its rows in place.
///
/// Each carries what the store saw when it decided on it, and storage checks
/// that again under the write lock: the queue can hold a write for as long as
/// another process holds that lock, and what it was decided against may have
/// moved by then. A write that no longer applies is dropped and answered
/// [`Reply::Rejected`], which reads the rows again so the store decides anew.
pub(crate) enum Write {
    /// Mark the turn stamped this unseen `done` seen. A newer turn finishing
    /// first stays unseen.
    Seen(SessionId, i64),
    /// Record a shell pane over the one the row showed.
    Shell {
        id: SessionId,
        seen: Option<String>,
        pane: Option<String>,
    },
    /// Record an adopted agent pane over the one the row showed.
    Pane {
        id: SessionId,
        seen: String,
        pane: String,
    },
    /// A remote agent's hook report: the row's id as published, the state, and
    /// the stamp the store gave it in place until this answers with storage's.
    /// Decided as a change of state, so it is re-decided against whatever
    /// state storage holds by then.
    Hook(SessionId, String, String, i64),
    /// Clear the report an Enter superseded, unless a newer one replaced it.
    /// Its stamp may still be the store's provisional one.
    Retire(SessionId, HookRow),
    Perf(String),
}

impl Write {
    /// Whether the store corrected a row in place for this write. Only these
    /// are counted against a read: see [`Reply::Rows`].
    pub(crate) fn touches_rows(&self) -> bool {
        !matches!(self, Self::Perf(_))
    }
}

pub(crate) enum Reply {
    /// Every row, read after `writes` writes had been applied — counted the
    /// way the store counts the writes it sends, so a store that has sent
    /// more knows these rows predate a correction it made in place.
    Rows {
        writes: u64,
        read: Box<RowsRead>,
    },
    /// Only hook liveness moved: the hook columns, for the store to patch in.
    Hooks {
        writes: u64,
        hooks: HashMap<SessionId, HookRow>,
    },
    /// Nothing another connection committed concerns the rows.
    Unchanged,
    /// The read failed; the previous rows stand and the next read retries.
    Failed(String),
    /// A pending focus request, claimed during a full read. Sent apart from
    /// the rows because the claim deleted it: a read discarded as stale must
    /// not take the request with it.
    Focus(String),
    /// The stamp a hook write landed with.
    Stamped(String, Option<i64>),
    /// A write that did not land, so the in-place correction made for it is
    /// wrong until the rows are read again.
    Rejected,
    Settled(u64),
}

/// Everything a full read returns. Sessions stay raw because turning them into
/// rows needs the agent registry and the pane probe, which live with the
/// store; the rest is final.
pub(crate) struct RowsRead {
    pub sessions: Vec<SharedSession>,
    pub hooks: HashMap<SessionId, HookRow>,
    pub bases: HashMap<SessionId, String>,
    pub stopped: HashSet<SessionId>,
    pub reports_as: HashMap<SessionId, String>,
    /// Each session's published meta, already cleaned and capped.
    pub meta: HashMap<SessionId, std::collections::BTreeMap<String, String>>,
    pub deleted: Vec<DeletedRow>,
    pub tasks: Vec<TaskRow>,
    pub automations: Vec<AutomationRow>,
}

/// The store's end of the queue.
pub(crate) struct DbWorker {
    jobs: Sender<Job>,
    pub(crate) replies: Receiver<Reply>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl DbWorker {
    pub(crate) fn spawn(database: Database) -> Self {
        let (jobs, inbox) = mpsc::channel();
        let (outbox, replies) = mpsc::channel();
        let mut worker = Worker {
            database,
            seen: None,
            writes: 0,
            out: outbox,
            stamped: HashMap::new(),
        };
        // Ends when the store drops its sender.
        let spawned = std::thread::Builder::new()
            .name("snapshot-db".to_string())
            .spawn(move || {
                while let Ok(job) = inbox.recv() {
                    if !worker.serve(job) {
                        return;
                    }
                }
            });
        let thread = match spawned {
            Ok(handle) => Some(handle),
            Err(e) => {
                tracing::error!("could not start the snapshot's database thread: {e}");
                None
            }
        };
        Self {
            jobs,
            replies,
            thread,
        }
    }

    pub(crate) fn send(&self, job: Job) {
        // A send fails only once the thread is gone, and then every read
        // answers with the rows already held — the same degradation as a
        // database that would not open.
        let _ = self.jobs.send(job);
    }
}

/// Waits for the writes still queued, so one sent just before a clean exit is
/// not lost with the process. The interface has given the terminal back by
/// then, so a lock held elsewhere delays the exit, not a frame.
impl Drop for DbWorker {
    fn drop(&mut self) {
        // Hanging up is what ends the thread's loop once the queue is empty.
        drop(std::mem::replace(&mut self.jobs, mpsc::channel().0));
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

struct Worker {
    database: Database,
    /// `PRAGMA data_version` and the snapshot generations as of the last read.
    /// `None` forces the next one to read everything.
    seen: Option<(i64, (i64, i64))>,
    writes: u64,
    out: Sender<Reply>,
    /// The stamp storage chose for the last hook report written per session,
    /// keyed by the provisional one the store published: a write queued
    /// before the store heard back names the report by the stamp it had.
    stamped: HashMap<SessionId, (i64, i64)>,
}

impl Worker {
    /// Returns false once the store is gone.
    fn serve(&mut self, job: Job) -> bool {
        let replies = match job {
            Job::Read { force } => self.read(force),
            Job::Write(write) => {
                if write.touches_rows() {
                    self.writes += 1;
                }
                self.write(write).into_iter().collect()
            }
            Job::Settle(token) => vec![Reply::Settled(token)],
        };
        replies
            .into_iter()
            .all(|reply| self.out.send(reply).is_ok())
    }

    /// The `data_version` gate, then whichever read it calls for.
    ///
    /// The external commits are checked against the snapshot generations:
    /// telemetry needs no row reads and liveness needs only the hook stamps
    /// (ADR-P6). A failed gate read reads everything rather than guessing.
    fn read(&mut self, force: bool) -> Vec<Reply> {
        let database = &self.database;
        // Recorded BEFORE the reads: a commit landing between the two would
        // otherwise be recorded as already-seen and never picked up.
        let version = database.data_version().ok();
        let generations = database.snapshot_versions().ok();
        if let (false, Some(version), Some(generations), Some((seen, previous))) =
            (force, version, generations, self.seen)
        {
            if version == seen || generations == previous {
                self.seen = Some((version, generations));
                return vec![Reply::Unchanged];
            }
            if generations.0 == previous.0 {
                if let Ok(hooks) = database.load_hook_states() {
                    self.seen = Some((version, generations));
                    return vec![Reply::Hooks {
                        writes: self.writes,
                        hooks,
                    }];
                }
            }
        }
        self.seen = version.zip(generations);

        let mut replies = Vec::new();
        // The claiming `DELETE … RETURNING` takes the write lock even on a
        // miss, so it rides the reads another connection's commit brings
        // round rather than running on every poll: a request can only appear
        // through such a commit.
        if let Ok(Some(id)) = database.take_pending_focus_session_id() {
            replies.push(Reply::Focus(id));
        }
        match read_rows(database) {
            Ok(read) => replies.push(Reply::Rows {
                writes: self.writes,
                read: Box::new(read),
            }),
            Err(e) => {
                self.seen = None;
                replies.push(Reply::Failed(e));
            }
        }
        replies
    }

    /// A stamp as storage holds it, for one the store may have published
    /// before hearing back.
    fn stored_stamp(&self, id: SessionId, stamp: i64) -> i64 {
        match self.stamped.get(&id) {
            Some(&(provisional, stored)) if provisional == stamp => stored,
            _ => stamp,
        }
    }

    fn write(&mut self, write: Write) -> Option<Reply> {
        let write = match write {
            Write::Seen(id, at) => Write::Seen(id, self.stored_stamp(id, at)),
            Write::Retire(id, mut report) => {
                report.state_at = report.state_at.map(|at| self.stored_stamp(id, at));
                Write::Retire(id, report)
            }
            other => other,
        };
        let database = &self.database;
        let landed = match write {
            Write::Seen(id, at) => database.mark_turn_seen(id, at).unwrap_or(false),
            Write::Shell { id, seen, pane } => {
                match database.set_session_shell_from(id, seen.as_deref(), pane.as_deref()) {
                    Ok(landed) => landed,
                    Err(e) => {
                        tracing::warn!("could not record the shell pane for {id}: {e}");
                        false
                    }
                }
            }
            Write::Pane { id, seen, pane } => {
                let claimed = database.claim_backend_id_from(id, &seen, &pane, |target, other| {
                    crate::session_ops::server_key(target) == crate::session_ops::server_key(other)
                });
                match claimed {
                    Ok(Some(crate::storage::PaneClaim::Stored)) => true,
                    // The row moved on, to a restart's pane or a new row.
                    Ok(None | Some(crate::storage::PaneClaim::RowGone)) => false,
                    Ok(Some(crate::storage::PaneClaim::Occupied)) => {
                        tracing::warn!(
                            "could not record adopted pane for {id}: pane {pane} is already \
                             claimed by another session"
                        );
                        false
                    }
                    Err(e) => {
                        tracing::warn!("could not record adopted pane for {id}: {e}");
                        false
                    }
                }
            }
            // Nothing written is a session parked after the store looked, or
            // one whose state another writer already set: refused, not failed,
            // and the row must be read again either way.
            Write::Hook(id, row, state, provisional) => {
                let stored = match database.report_hook_state(id, &state) {
                    Ok(Some(stored)) => stored,
                    Ok(None) | Err(_) => return Some(Reply::Rejected),
                };
                self.stamped.insert(id, (provisional, stored));
                return Some(Reply::Stamped(row, Some(stored)));
            }
            Write::Retire(id, report) => {
                match database.clear_hook_state_if_unchanged(id, &report) {
                    Ok(retired) => retired,
                    Err(e) => {
                        tracing::warn!("could not retire the hook report for {id}: {e}");
                        false
                    }
                }
            }
            Write::Perf(json) => {
                // Never worth a reread; the CLI simply reports the older
                // snapshot, or none.
                if let Err(e) = database.set_perf_snapshot(&json) {
                    tracing::warn!("could not publish the perf snapshot: {e}");
                }
                return None;
            }
        };
        (!landed).then_some(Reply::Rejected)
    }
}

/// Every table the snapshot shows, but the session rows left raw.
fn read_rows(database: &Database) -> Result<RowsRead, String> {
    let sessions = database
        .list_active_sessions()
        .map_err(|e| format!("list sessions: {e}"))?;
    let hooks = database.load_hook_states().unwrap_or_default();
    let bases = database.load_base_branches().unwrap_or_default();
    let stopped = database.load_stopped_sessions().unwrap_or_default();
    // What a driver declared this row runs. Read here for the same reason
    // `session get` reads it: the coverage a state is judged against belongs
    // to the declared agent, not to the shell the row was created with.
    let reports_as = database.load_reports_as().unwrap_or_default();
    // Cleaned here, on the worker, so a publish only copies strings.
    let meta = database
        .load_session_meta_with_prefix(crate::kernel::lua_api::META_PREFIX)
        .unwrap_or_default()
        .into_iter()
        .map(|(id, keys)| {
            let keys = keys
                .into_iter()
                .filter(|(key, _)| crate::kernel::lua_api::is_published_meta_key(key))
                .take(crate::kernel::lua_api::META_KEYS)
                .map(|(key, value)| {
                    let value = crate::kernel::lua_api::clean_meta_value(&value);
                    (key, value)
                })
                .collect();
            (id, keys)
        })
        .collect();

    // Deleted sessions, so a restore surface has something to list.
    let deleted = database
        .list_deleted_sessions()
        .map(|list| {
            list.into_iter()
                .map(|row| DeletedRow {
                    id: row.id.to_string(),
                    // Stats each borrowed worktree, so it is deliberately
                    // here, on the worker, rather than in a render path: a
                    // full read runs only when `data_version` moved, and the
                    // deleted list is short.
                    restore_refusal: crate::session_ops::restore_refusal(
                        &row.name,
                        row.force_deleted,
                        &row.backend_type,
                        &row.worktrees,
                    ),
                    name: row.name,
                    agent: row.agent,
                    deleted_at: row.deleted_at,
                    worktrees: row.worktrees.len(),
                    partial: row.force_deleted,
                })
                .collect()
        })
        .unwrap_or_default();

    // Tasks and automations: already fully functional headlessly, so this
    // only surfaces them.
    let tasks = database
        .list_tasks()
        .map(|list| {
            list.into_iter()
                .map(|task| TaskRow {
                    id: task.id,
                    title: task.title,
                    description: task.description,
                    status: task.status.as_str().to_string(),
                    source: task.source,
                    external_url: task.external_url,
                    created_at: task.created_at,
                    updated_at: task.updated_at,
                })
                .collect()
        })
        .unwrap_or_default();

    use crate::session::automation::{AutomationAction, AutomationSchedule};
    // Run history for the whole list in one query, not one per automation.
    let mut run_history = database.list_recent_automation_runs(10).unwrap_or_default();
    let automations = database
        .list_automations()
        .map(|list| {
            list.into_iter()
                .map(|auto| {
                    let history = run_history.remove(&auto.id).unwrap_or_default();
                    let runs: Vec<RunRow> = history
                        .iter()
                        .map(|run| RunRow {
                            id: run.id,
                            started_at: run.started_at as i64,
                            status: run.status.as_str().to_string(),
                            detail: run.detail.clone(),
                            session: run.related_session_id.map(|id| id.to_string()),
                        })
                        .collect();
                    let last = history.into_iter().next();
                    let mut target = AutomationRow::default();
                    match &auto.action {
                        AutomationAction::Send { session_id } => {
                            target.session = Some(session_id.to_string());
                        }
                        AutomationAction::Spawn {
                            repo_path,
                            worktree_branch,
                            base_branch,
                            agent,
                            extra_repos,
                        } => {
                            target.repo = Some(repo_path.to_string_lossy().to_string());
                            target.branch = worktree_branch.clone();
                            target.base = base_branch.clone();
                            target.agent = agent.clone();
                            target.extra_repos = extra_repos.len();
                        }
                        AutomationAction::Exec { command } => {
                            target.command = Some(command.clone());
                        }
                    }
                    AutomationRow {
                        trigger: auto.schedule.trigger(),
                        timezone: auto.timezone.clone(),
                        prompt: auto.prompt.clone(),
                        created_at: auto.created_at,
                        updated_at: auto.updated_at,
                        last_run_at: auto.last_run_at,
                        next_run_at: auto.next_run_at,
                        id: auto.id,
                        name: auto.name,
                        // The expression itself when it is a cron, else
                        // the kind — what a pane wants to show.
                        schedule: match &auto.schedule {
                            AutomationSchedule::Cron { expr } => expr.clone(),
                            other => other.kind().to_string(),
                        },
                        action: auto.action.kind().to_string(),
                        enabled: auto.enabled,
                        last_outcome: last.as_ref().map(|run| run.status.as_str().to_string()),
                        last_detail: last.map(|run| run.detail).filter(|d| !d.is_empty()),
                        runs,
                        ..target
                    }
                })
                .collect()
        })
        .unwrap_or_default();

    Ok(RowsRead {
        sessions,
        hooks,
        bases,
        stopped,
        reports_as,
        meta,
        deleted,
        tasks,
        automations,
    })
}
