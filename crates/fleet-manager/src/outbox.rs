//! The outbox step of a sweep: TaskController ingests the outbox of each
//! Task's session into the boards, under the lease, as the Task whose
//! session it is. The events are appended first, then the new offset is
//! written to `Task.status.outbox`: a run that stops between the two reads
//! the same lines again, and their keys append nothing. What the step
//! learns, whether every whole line is in, is the `outbox_drained`
//! observation, which holds a released Task until its outbox is read.

use fleet_api::event::{self, MAX_OUTBOX_READ, OutboxCursor, OutboxRead};
use fleet_api::session::SessionState;
use fleet_api::task::{self, SessionRef};
use fleet_api::{LeaseToken, Millis, Object, Observation, StoreError};
use fleet_controllers::TaskController;

use crate::{Manager, RunReport};

const CURSOR_WRITE_TRIES: u32 = 5;

/// The most lines one append takes: well inside a pass, and one lock, one
/// lease renewal and one fsync per segment for them all.
const MAX_RUN: usize = 256;

/// Consecutive whole lines for one board, appended together.
#[derive(Default)]
struct Run {
    board: Option<String>,
    lines: Vec<RunLine>,
}

struct RunLine {
    offset: u64,
    end: u64,
    publish: event::Publish,
}

impl Run {
    /// Whether a line for `board` can join this run.
    fn takes(&self, board: &str) -> bool {
        self.board.as_deref().is_none_or(|b| b == board) && self.lines.len() < MAX_RUN
    }

    fn push(&mut self, (board, publish): (String, event::Publish), offset: u64, end: u64) {
        self.board.get_or_insert(board);
        self.lines.push(RunLine {
            offset,
            end,
            publish,
        });
    }
}

/// A store answer that refuses the events themselves, not one that leaves
/// them unknown: the line is skipped.
fn refused(e: &StoreError) -> bool {
    matches!(
        e,
        StoreError::Invalid(_) | StoreError::NotFound(_) | StoreError::Forbidden(_)
    )
}

/// The lease lost ends the sweep; any other error stops this ingest.
fn stopped_by(e: StoreError) -> Result<Option<StoreError>, StoreError> {
    match e {
        StoreError::LeaseLost(_) => Err(e),
        e => Ok(Some(e)),
    }
}

/// What one ingest did: the Task as written, when its cursor moved, and
/// whether the outbox holds no whole line still to ingest.
pub(crate) struct Ingested {
    pub(crate) written: Option<Object>,
    pub(crate) drained: Observation<bool>,
}

impl Ingested {
    /// A Concluded Task's outbox between its rechecks: not read, and no
    /// TaskController reconcile asks about it.
    pub(crate) fn unread() -> Ingested {
        Ingested::unchanged(Observation::Known(true))
    }

    fn unchanged(drained: Observation<bool>) -> Ingested {
        Ingested {
            written: None,
            drained,
        }
    }
}

impl Manager<'_> {
    /// Ingest the outbox of `task`'s session, if it has one. The sweep asks
    /// for a Concluded Task that is not being deleted only when it is due
    /// for its session recheck. A gone
    /// session's outbox is read until a read finds every whole line in
    /// (`status.outbox.closed`), and while its Task is being deleted. An
    /// outbox that cannot be read concludes nothing: the cursor stays, and
    /// `drained` is Unknown. The lease is renewed before each read.
    pub(crate) fn ingest_outbox(
        &self,
        task: &Object,
        tasks: &[Object],
        lease: &mut LeaseToken,
        ttl: Millis,
        report: &mut RunReport,
    ) -> Result<Ingested, StoreError> {
        let field = |name: &str| task.status.get(name).cloned().unwrap_or_default();
        let invalid = |e: serde_json::Error| StoreError::Invalid(format!("{}: {e}", task.key()));
        let session: Option<SessionRef> =
            serde_json::from_value(field("session")).map_err(invalid)?;
        let Some(session) = session else {
            return Ok(Ingested::unchanged(Observation::Known(true)));
        };
        let state: Option<SessionState> =
            serde_json::from_value(field("sessionState")).map_err(invalid)?;
        let gone = state == Some(SessionState::Gone);
        let cursor: OutboxCursor = serde_json::from_value::<Option<OutboxCursor>>(field("outbox"))
            .map_err(invalid)?
            .unwrap_or_default();
        if gone && cursor.closed && !task.is_deleting() {
            return Ok(Ingested::unchanged(Observation::Known(true)));
        }
        *lease = self.store.renew_lease(lease, ttl)?;
        let read = match self.read_outbox(&session, cursor.offset) {
            None | Some(Observation::Known(None)) => OutboxRead {
                start: cursor.offset,
                bytes: Vec::new(),
            },
            Some(Observation::Known(Some(read))) => read,
            Some(Observation::Unknown { why }) => {
                report
                    .unknowns
                    .push(format!("{} outbox: {why}", task.key()));
                return Ok(Ingested::unchanged(Observation::unknown(why)));
            }
        };
        let (mut next, entries) = event::ingest(&cursor, &read);
        let source =
            event::task_source(task, |name| tasks.iter().find(|t| t.metadata.name == name));
        let mut drained = Observation::Known((read.bytes.len() as u64) < MAX_OUTBOX_READ);
        let mut run = Run::default();
        let mut stopped = None;
        for entry in entries {
            let line = match entry.line {
                Ok(line) => line,
                Err(why) => {
                    stopped = self.append_run(&mut run, &source, &mut next, lease, ttl)?;
                    if stopped.is_some() {
                        break;
                    }
                    next.skip(entry.offset, entry.end, &why);
                    continue;
                }
            };
            if !run.takes(&line.0) {
                stopped = self.append_run(&mut run, &source, &mut next, lease, ttl)?;
                if stopped.is_some() {
                    break;
                }
            }
            run.push(line, entry.offset, entry.end);
        }
        if stopped.is_none() {
            stopped = self.append_run(&mut run, &source, &mut next, lease, ttl)?;
        }
        if let Some(e) = stopped {
            report.unknowns.push(format!("{} outbox: {e}", task.key()));
            drained = Observation::unknown(e.to_string());
        }
        next.closed = gone && drained == Observation::Known(true);
        if next == cursor {
            return Ok(Ingested::unchanged(drained));
        }
        *lease = self.store.renew_lease(lease, ttl)?;
        let Some(written) = self.write_cursor(task, &next, lease, report)? else {
            return Ok(Ingested::unchanged(Observation::Known(false)));
        };
        report.status_writes += 1;
        Ok(Ingested {
            written: Some(written),
            drained,
        })
    }

    /// Append `run` in one call, under one lock and one lease renewal, and
    /// move `next` past it. A run the store refuses whole is appended one
    /// line at a time, so a refused line is skipped and counted and the
    /// others land. Answers the error that stopped it, which leaves `next`
    /// at the last line in.
    fn append_run(
        &self,
        run: &mut Run,
        source: &event::Source,
        next: &mut OutboxCursor,
        lease: &mut LeaseToken,
        ttl: Millis,
    ) -> Result<Option<StoreError>, StoreError> {
        let Some(board) = run.board.take() else {
            return Ok(None);
        };
        let lines = std::mem::take(&mut run.lines);
        *lease = self.store.renew_lease(lease, ttl)?;
        let publishes = lines.iter().map(|l| l.publish.clone()).collect();
        match self.store.append_events(
            &board,
            source,
            publishes,
            TaskController::MANAGER,
            Some(lease),
        ) {
            Ok(_) => {
                next.offset = lines.last().map_or(next.offset, |l| l.end);
                return Ok(None);
            }
            Err(e) if !refused(&e) => return stopped_by(e),
            Err(_) => {}
        }
        for line in lines {
            *lease = self.store.renew_lease(lease, ttl)?;
            match self.store.append_events(
                &board,
                source,
                vec![line.publish],
                TaskController::MANAGER,
                Some(lease),
            ) {
                Ok(_) => next.offset = line.end,
                Err(e) if refused(&e) => next.skip(line.offset, line.end, &e.to_string()),
                Err(e) => return stopped_by(e),
            }
        }
        Ok(None)
    }

    /// Write `next` to the Task's `status.outbox`, re-reading the Task and
    /// trying again when another writer, such as the worker, wrote first:
    /// the events are already appended, and a cursor left behind would
    /// read them again after retention dropped their keys. `None` when the
    /// Task is gone, or still moving after `CURSOR_WRITE_TRIES`.
    fn write_cursor(
        &self,
        task: &Object,
        next: &OutboxCursor,
        lease: &LeaseToken,
        report: &mut RunReport,
    ) -> Result<Option<Object>, StoreError> {
        let cursor = serde_json::to_value(next).map_err(|e| StoreError::Invalid(e.to_string()))?;
        let mut current = task.clone();
        for _ in 0..CURSOR_WRITE_TRIES {
            let mut status = current.status.clone();
            status["outbox"] = cursor.clone();
            match self.store.update_status(
                task::KIND,
                &current.metadata.name,
                status,
                current.metadata.resource_version,
                TaskController::MANAGER,
                Some(lease),
            ) {
                Ok(written) => return Ok(Some(written)),
                Err(StoreError::Conflict { .. }) => {
                    report.conflicts += 1;
                    current = match self.store.get(task::KIND, &current.metadata.name) {
                        Observation::Known(Some(fresh))
                            if fresh.metadata.uid == task.metadata.uid =>
                        {
                            fresh
                        }
                        Observation::Known(_) => return Ok(None),
                        Observation::Unknown { why } => return Err(StoreError::Unknown(why)),
                    };
                }
                Err(e) => return Err(e),
            }
        }
        Ok(None)
    }

    /// The session's outbox from `from`: through the runtime when only it
    /// reaches the host, else through the signal port. `None` with neither,
    /// where no session was given an outbox.
    fn read_outbox(
        &self,
        session: &SessionRef,
        from: u64,
    ) -> Option<Observation<Option<OutboxRead>>> {
        match (self.runtime.outbox(session, from), self.signals) {
            (Some(read), _) => Some(read),
            (None, Some(signals)) => Some(signals.outbox(session, from)),
            (None, None) => None,
        }
    }
}
