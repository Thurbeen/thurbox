//! The Board `fleet` step: TaskController appends the `fleet.task.*` events
//! a Task is owed (`task_event::FleetEvents::owed`) to Board `fleet`, as
//! itself, under the lease, then writes the record that says they are
//! published. A stop between the two appends them again under the same
//! keys, which append nothing. A Board that cannot be written keeps every
//! event owed, for the next pass.

use fleet_api::board::FLEET;
use fleet_api::event::{ControllerSource, Source};
use fleet_api::task::{self, TaskStatus};
use fleet_api::{LeaseToken, Millis, Object, Observation, StoreError};
use fleet_controllers::TaskController;

use crate::outbox::refused;
use crate::{Manager, RunReport};

const RECORD_WRITE_TRIES: u32 = 5;

impl Manager<'_> {
    /// Publish what `task` is owed. Answers the Task as written, when its
    /// record moved.
    pub(crate) fn publish_fleet_events(
        &self,
        task: &Object,
        lease: &mut LeaseToken,
        ttl: Millis,
        report: &mut RunReport,
    ) -> Result<Option<Object>, StoreError> {
        if task.status.is_null() {
            return Ok(None);
        }
        let st: TaskStatus = fleet_api::validate::typed(task::KIND, &task.status)?;
        let meta = &task.metadata;
        let (owed, mut next) =
            st.fleet_events
                .owed(&meta.name, &meta.uid, &st.declared, st.result.as_ref());
        if owed.is_empty() && next == st.fleet_events {
            return Ok(None);
        }
        *lease = self.store.renew_lease(lease, ttl)?;
        if !owed.is_empty()
            && let Err(e) = self.append_owed(owed, &mut next, lease, ttl)?
        {
            report
                .unknowns
                .push(format!("{} Board/{FLEET}: {e}", task.key()));
            return Ok(None);
        }
        self.write_record(task, &st, &next, lease, report)
    }

    /// Append `owed` in one call, else one at a time when the Board refuses
    /// the batch, counting each refused event in `next`. The inner error
    /// is one that leaves the events unknown: nothing is recorded.
    fn append_owed(
        &self,
        owed: Vec<fleet_api::event::Publish>,
        next: &mut fleet_api::task_event::FleetEvents,
        lease: &mut LeaseToken,
        ttl: Millis,
    ) -> Result<Result<(), StoreError>, StoreError> {
        let source = Source::Controller(ControllerSource::of(&TaskController::MANAGER));
        match self.append(&source, owed.clone(), lease) {
            Ok(()) => return Ok(Ok(())),
            Err(e @ StoreError::LeaseLost(_)) => return Err(e),
            Err(e) if !refused(&e) => return Ok(Err(e)),
            Err(_) => {}
        }
        for publish in owed {
            *lease = self.store.renew_lease(lease, ttl)?;
            match self.append(&source, vec![publish.clone()], lease) {
                Ok(()) => {}
                Err(e) if refused(&e) => next.skip(&publish, &e.to_string()),
                Err(e @ StoreError::LeaseLost(_)) => return Err(e),
                Err(e) => return Ok(Err(e)),
            }
        }
        Ok(Ok(()))
    }

    fn append(
        &self,
        source: &Source,
        events: Vec<fleet_api::event::Publish>,
        lease: &LeaseToken,
    ) -> Result<(), StoreError> {
        self.store
            .append_events(FLEET, source, events, TaskController::MANAGER, Some(lease))
            .map(|_| ())
    }

    /// Write `next` to `status.fleetEvents`, re-reading the Task when
    /// another writer wrote first. `None` when the Task is gone, when its
    /// record moved under this step, or after `RECORD_WRITE_TRIES`: the
    /// next pass owes the same events, and their keys append nothing.
    fn write_record(
        &self,
        task: &Object,
        read: &TaskStatus,
        next: &fleet_api::task_event::FleetEvents,
        lease: &LeaseToken,
        report: &mut RunReport,
    ) -> Result<Option<Object>, StoreError> {
        let record = serde_json::to_value(next).map_err(|e| StoreError::Invalid(e.to_string()))?;
        let mut current = task.clone();
        for _ in 0..RECORD_WRITE_TRIES {
            let mut status = current.status.clone();
            status["fleetEvents"] = record.clone();
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
                    let fresh: TaskStatus =
                        fleet_api::validate::typed(task::KIND, &current.status)?;
                    if fresh.fleet_events != read.fleet_events {
                        return Ok(None);
                    }
                }
                Err(e) => return Err(e),
            }
        }
        Ok(None)
    }
}
