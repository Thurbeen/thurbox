//! The outbox step of a sweep: TaskController ingests each live session's
//! outbox into the boards, under the lease, as the Task whose session it
//! is. The events are appended first, then the new offset is written to
//! `Task.status.outbox`: a run that stops between the two reads the same
//! lines again, and their keys append nothing.

use fleet_api::event::{self, OutboxCursor, OutboxRead};
use fleet_api::session::SessionState;
use fleet_api::task::{self, SessionRef};
use fleet_api::{LeaseToken, Millis, Object, Observation, StoreError};
use fleet_controllers::TaskController;

use crate::{Manager, RunReport};

impl Manager<'_> {
    /// Ingest the outbox of `task`'s session, if it has one that is not
    /// gone. Answers the Task as written when its cursor moved. An outbox
    /// that cannot be read concludes nothing: the cursor stays.
    pub(crate) fn ingest_outbox(
        &self,
        task: &Object,
        tasks: &[Object],
        lease: &mut LeaseToken,
        ttl: Millis,
        report: &mut RunReport,
    ) -> Result<Option<Object>, StoreError> {
        let field = |name: &str| task.status.get(name).cloned().unwrap_or_default();
        let invalid = |e: serde_json::Error| StoreError::Invalid(format!("{}: {e}", task.key()));
        let session: Option<SessionRef> =
            serde_json::from_value(field("session")).map_err(invalid)?;
        let state: Option<SessionState> =
            serde_json::from_value(field("sessionState")).map_err(invalid)?;
        let Some(session) = session.filter(|_| state != Some(SessionState::Gone)) else {
            return Ok(None);
        };
        let cursor: OutboxCursor = serde_json::from_value::<Option<OutboxCursor>>(field("outbox"))
            .map_err(invalid)?
            .unwrap_or_default();
        let Some(read) = self.read_outbox(&session, cursor.offset, task, report) else {
            return Ok(None);
        };
        let (mut next, entries) = event::ingest(&cursor, &read);
        let source =
            event::task_source(task, |name| tasks.iter().find(|t| t.metadata.name == name));
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
                    break;
                }
            }
        }
        if next == cursor {
            return Ok(None);
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
                Ok(Some(written))
            }
            Err(StoreError::Conflict { .. }) => {
                report.conflicts += 1;
                Ok(None)
            }
            Err(e) => Err(e),
        }
    }

    fn read_outbox(
        &self,
        session: &SessionRef,
        from: u64,
        task: &Object,
        report: &mut RunReport,
    ) -> Option<OutboxRead> {
        let read = match (self.runtime.outbox(session, from), self.signals) {
            (Some(read), _) => read,
            (None, Some(signals)) => signals.outbox(session, from),
            (None, None) => return None,
        };
        match read {
            Observation::Known(read) => read,
            Observation::Unknown { why } => {
                report
                    .unknowns
                    .push(format!("{} outbox: {why}", task.key()));
                None
            }
        }
    }
}
