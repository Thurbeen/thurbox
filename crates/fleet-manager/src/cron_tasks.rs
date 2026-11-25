use fleet_api::cron_task::{self, CronTaskStatus};
use fleet_api::plan::{Action, CREATE_ACTION, EffectOutcome, Receipt};
use fleet_api::task;
use fleet_api::{LeaseToken, Millis, Object, Observation, StoreError};
use fleet_controllers::{CronTaskController, CronTaskInput};

use crate::{Manager, RunReport, Step, telemetry};

impl Manager<'_> {
    pub(crate) fn sweep_cron_tasks(
        &self,
        lease: &mut LeaseToken,
        ttl: Millis,
        report: &mut RunReport,
    ) -> Result<bool, StoreError> {
        let cron_tasks = match self.store.list(cron_task::KIND) {
            Observation::Known(c) => c,
            Observation::Unknown { why } => {
                report.unknowns.push(format!("list CronTask: {why}"));
                return Ok(false);
            }
        };
        if cron_tasks.is_empty() {
            return Ok(false);
        }
        let tasks = match self.store.list(task::KIND) {
            Observation::Known(t) => t,
            Observation::Unknown { why } => {
                report.unknowns.push(format!("list Task: {why}"));
                return Ok(false);
            }
        };
        let now = self.clock.now();
        let due: Vec<&Object> = cron_tasks
            .iter()
            .filter(|c| CronTaskController::due(c, &owned(c, &tasks), now))
            .collect();
        let mut busy = false;
        for obj in due {
            *lease = self.store.renew_lease(lease, ttl)?;
            let step = telemetry::reconcile(
                "CronTaskController",
                obj,
                || self.clock.now(),
                || self.reconcile_cron_task(obj, &tasks, lease, ttl, report),
            );
            busy |= self.tally(step, &obj.key(), report)?;
        }
        Ok(busy)
    }

    fn reconcile_cron_task(
        &self,
        obj: &Object,
        tasks: &[Object],
        lease: &mut LeaseToken,
        ttl: Millis,
        report: &mut RunReport,
    ) -> Result<Step, StoreError> {
        let input = CronTaskInput {
            object: obj,
            tasks: owned(obj, tasks),
            now: self.clock.now(),
        };
        let plan = CronTaskController.reconcile(&input)?;
        let by = CronTaskController::MANAGER;
        let mut step = Step::default();
        let mut current = obj.clone();
        if let Some(status) = plan.status {
            match self.store.update_status(
                cron_task::KIND,
                &obj.metadata.name,
                status,
                obj.metadata.resource_version,
                by.clone(),
                Some(&*lease),
            ) {
                Ok(o) => {
                    current = o;
                    step.wrote += 1;
                }
                Err(StoreError::Conflict { .. }) => {
                    step.conflict = true;
                    return Ok(step);
                }
                Err(e) => return Err(e),
            }
        }
        // Intent first (written above), each effect under a fresh lease,
        // receipt after, under the effect's key. A retire only marks the
        // Task deleting: under Replace the controller plans the replacement
        // only once the old Task is gone from the listing, in a later pass.
        for effect in &plan.effects {
            *lease = self.store.renew_lease(lease, ttl)?;
            let Action::RetireScheduled { name, uid, running } = &effect.action else {
                continue;
            };
            let outcome = telemetry::effect(obj, effect, || {
                self.retire_scheduled(name, uid, &obj.metadata.uid, *running, lease)
            });
            if let Observation::Unknown { why } = &outcome {
                report
                    .unknowns
                    .push(format!("{} {}: {why}", obj.key(), effect.action.name()));
            }
            step.effects += 1;
            current = self.write_cron_task_receipt(
                &current,
                &effect.key,
                effect.action.name(),
                outcome,
                lease,
            )?;
            step.wrote += 1;
        }
        for create in &plan.creates {
            *lease = self.store.renew_lease(lease, ttl)?;
            let outcome = telemetry::effect_named(obj, CREATE_ACTION, || {
                self.create_scheduled(&create.object, &obj.metadata.uid, lease)
            });
            if let Observation::Unknown { why } = &outcome {
                report
                    .unknowns
                    .push(format!("{} {CREATE_ACTION}: {why}", obj.key()));
            }
            step.effects += 1;
            current =
                self.write_cron_task_receipt(&current, &create.key, CREATE_ACTION, outcome, lease)?;
            step.wrote += 1;
        }
        Ok(step)
    }

    fn create_scheduled(
        &self,
        obj: &Object,
        cron_uid: &str,
        lease: &LeaseToken,
    ) -> Observation<EffectOutcome> {
        match self
            .store
            .create_fenced(obj.clone(), CronTaskController::MANAGER, lease)
        {
            Ok(o) => Observation::Known(EffectOutcome::Created {
                uid: o.metadata.uid,
                adopted: false,
            }),
            Err(StoreError::AlreadyExists(_)) => {
                match self.store.get(&obj.kind, &obj.metadata.name) {
                    Observation::Known(Some(existing))
                        if existing
                            .metadata
                            .labels
                            .get(cron_task::UID_LABEL)
                            .map(String::as_str)
                            == Some(cron_uid) =>
                    {
                        Observation::Known(EffectOutcome::Created {
                            uid: existing.metadata.uid,
                            adopted: true,
                        })
                    }
                    Observation::Known(Some(existing)) => {
                        Observation::Known(EffectOutcome::Failed {
                            why: format!(
                                "{} exists and was not created for this CronTask",
                                existing.key()
                            ),
                        })
                    }
                    Observation::Known(None) => Observation::unknown(format!(
                        "{} was there and is gone; look again",
                        obj.key()
                    )),
                    Observation::Unknown { why } => Observation::unknown(why),
                }
            }
            Err(StoreError::LeaseLost(why)) => Observation::unknown(format!("lease lost: {why}")),
            Err(StoreError::Unknown(why)) => Observation::unknown(why),
            Err(e @ StoreError::Full(_)) => Observation::unknown(e.to_string()),
            Err(e) => Observation::Known(EffectOutcome::Failed { why: e.to_string() }),
        }
    }

    fn retire_scheduled(
        &self,
        name: &str,
        uid: &str,
        cron_uid: &str,
        running: bool,
        lease: &LeaseToken,
    ) -> Observation<EffectOutcome> {
        match self.store.retire_scheduled_task(
            name,
            uid,
            cron_uid,
            running,
            CronTaskController::MANAGER,
            lease,
        ) {
            Ok(_) | Err(StoreError::NotFound(_)) => Observation::Known(EffectOutcome::Deleted),
            Err(StoreError::Conflict { .. }) => Observation::Known(EffectOutcome::Failed {
                why: format!("Task/{name} under that name is another Task now"),
            }),
            Err(StoreError::LeaseLost(why)) => Observation::unknown(format!("lease lost: {why}")),
            Err(StoreError::Unknown(why)) => Observation::unknown(why),
            Err(e @ StoreError::Full(_)) => Observation::unknown(e.to_string()),
            Err(e) => Observation::Known(EffectOutcome::Failed { why: e.to_string() }),
        }
    }

    fn write_cron_task_receipt(
        &self,
        current: &Object,
        key: &str,
        action: &str,
        outcome: Observation<EffectOutcome>,
        lease: &LeaseToken,
    ) -> Result<Object, StoreError> {
        let receipt = Receipt {
            action: action.to_string(),
            at: self.clock.now(),
            outcome,
        };
        let mut obj = current.clone();
        for _ in 0..16 {
            let mut st: CronTaskStatus = serde_json::from_value(obj.status.clone())
                .map_err(|e| StoreError::Invalid(format!("{}: CronTask status: {e}", obj.key())))?;
            let Some(record) = st.effects.get_mut(key) else {
                return Err(StoreError::Invalid(format!(
                    "{}: no effect record holds the intent {key}",
                    obj.key()
                )));
            };
            record.receipt = Some(receipt.clone());
            let value =
                serde_json::to_value(&st).map_err(|e| StoreError::Invalid(e.to_string()))?;
            match self.store.update_status(
                cron_task::KIND,
                &obj.metadata.name,
                value,
                obj.metadata.resource_version,
                CronTaskController::MANAGER,
                Some(lease),
            ) {
                Ok(o) => return Ok(o),
                Err(StoreError::Conflict { .. }) => {
                    obj = match self.store.get(cron_task::KIND, &obj.metadata.name) {
                        Observation::Known(Some(o)) => o,
                        Observation::Known(None) => return Err(StoreError::NotFound(obj.key())),
                        Observation::Unknown { why } => return Err(StoreError::Unknown(why)),
                    };
                }
                Err(e) => return Err(e),
            }
        }
        Err(StoreError::Busy(format!(
            "{}: receipt for {key} kept conflicting",
            obj.key()
        )))
    }
}

fn owned<'a>(parent: &Object, tasks: &'a [Object]) -> Vec<&'a Object> {
    tasks
        .iter()
        .filter(|t| t.metadata.labels.get(cron_task::UID_LABEL) == Some(&parent.metadata.uid))
        .collect()
}
