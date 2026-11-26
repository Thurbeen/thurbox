use fleet_api::event::{self, MAX_OUTBOX_READ, OutboxCursor, OutboxRead};
use fleet_api::session::SessionState;
use fleet_api::task::{self, SessionRef};
use fleet_api::{LeaseToken, Millis, Object, Observation, StoreError};
use fleet_controllers::TaskController;

use crate::{Manager, RunReport};

const CURSOR_WRITE_TRIES: u32 = 5;

const MAX_RUN: usize = 256;

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

pub(crate) fn refused(e: &StoreError) -> bool {
    matches!(
        e,
        StoreError::Invalid(_) | StoreError::NotFound(_) | StoreError::Forbidden(_)
    )
}

fn stopped_by(e: StoreError) -> Result<Option<StoreError>, StoreError> {
    match e {
        StoreError::LeaseLost(_) | StoreError::Full(_) => Err(e),
        e => Ok(Some(e)),
    }
}

pub(crate) struct Ingested {
    pub(crate) written: Option<Object>,
    pub(crate) drained: Observation<bool>,
}

impl Ingested {
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
        if session.host != fleet_api::task::LOCAL_HOST {
            *lease = self.store.renew_lease(lease, ttl)?;
        }
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
                    next.discarding = entry.torn;
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
