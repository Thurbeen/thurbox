//! The outbox step of a sweep: TaskController ingests the outbox of each
//! Task's session into the boards, under the lease, as the Task whose
//! session it is. The events are appended first, then the new offset is
//! written to `Task.status.outbox`: a run that stops between the two reads
//! the same lines again, and their keys append nothing. What the step
//! learns, whether every whole line is in, is the `outbox_drained`
//! observation, which holds a released Task until its outbox is read.

use fleet_api::event::{self, MAX_OUTBOX_READ, OutboxCursor, OutboxRead};
use fleet_api::task::{self, SessionRef};
use fleet_api::{LeaseToken, Millis, Object, Observation, StoreError};
use fleet_controllers::TaskController;

use crate::{Manager, RunReport};

/// What one ingest did: the Task as written, when its cursor moved, and
/// whether the outbox holds no whole line still to ingest.
pub(crate) struct Ingested {
    pub(crate) written: Option<Object>,
    pub(crate) drained: Observation<bool>,
}

impl Ingested {
    fn unchanged(drained: Observation<bool>) -> Ingested {
        Ingested {
            written: None,
            drained,
        }
    }
}

impl Manager<'_> {
    /// Ingest the outbox of `task`'s session, if it has one, gone or not:
    /// what a session wrote before it ended is still its. An outbox that
    /// cannot be read concludes nothing: the cursor stays, and `drained` is
    /// Unknown.
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
        let cursor: OutboxCursor = serde_json::from_value::<Option<OutboxCursor>>(field("outbox"))
            .map_err(invalid)?
            .unwrap_or_default();
        let read = match self.read_outbox(&session, cursor.offset) {
            None | Some(Observation::Known(None)) => {
                return Ok(Ingested::unchanged(Observation::Known(true)));
            }
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
        for entry in entries {
            let (board, publish) = match entry.line {
                Ok(line) => line,
                Err(why) => {
                    next.skip(entry.offset, entry.end, &why);
                    continue;
                }
            };
            *lease = self.store.renew_lease(lease, ttl)?;
            match self.store.append_events(
                &board,
                &source,
                vec![publish],
                TaskController::MANAGER,
                Some(lease),
            ) {
                Ok(_) => next.offset = entry.end,
                Err(
                    e @ (StoreError::Invalid(_)
                    | StoreError::NotFound(_)
                    | StoreError::Forbidden(_)),
                ) => {
                    next.skip(entry.offset, entry.end, &e.to_string());
                }
                Err(e @ StoreError::LeaseLost(_)) => return Err(e),
                Err(e) => {
                    report.unknowns.push(format!("{} outbox: {e}", task.key()));
                    drained = Observation::unknown(e.to_string());
                    break;
                }
            }
        }
        if next == cursor {
            return Ok(Ingested::unchanged(drained));
        }
        *lease = self.store.renew_lease(lease, ttl)?;
        let mut status = task.status.clone();
        status["outbox"] =
            serde_json::to_value(&next).map_err(|e| StoreError::Invalid(e.to_string()))?;
        match self.store.update_status(
            task::KIND,
            &task.metadata.name,
            status,
            task.metadata.resource_version,
            TaskController::MANAGER,
            Some(lease),
        ) {
            Ok(written) => {
                report.status_writes += 1;
                Ok(Ingested {
                    written: Some(written),
                    drained,
                })
            }
            Err(StoreError::Conflict { .. }) => {
                report.conflicts += 1;
                Ok(Ingested::unchanged(Observation::Known(false)))
            }
            Err(e) => Err(e),
        }
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
