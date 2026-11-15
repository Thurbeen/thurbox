//! CronTaskController: creates a Task from a CronTask's template at each
//! scheduled time, as a Kubernetes CronJob creates a Job.
//!
//! Pure: it reads the CronTask, the Tasks that carry its uid label and the
//! time, and returns a Plan. The status it plans carries the intent of each
//! create and retire under its key; the manager writes that status first,
//! then performs the effect, then writes the receipt under the same key.
//! It never writes a Task's status: the TaskController runs each Task it
//! creates like any other.

use std::collections::BTreeMap;

use fleet_api::condition::{ConditionStatus, set_condition, settle_transitions};
use fleet_api::cron_task::{
    self, ConcurrencyPolicy, CronTaskSpec, CronTaskStatus, EFFECTS_KEPT, EffectRecord,
    MAX_CREATE_ATTEMPTS, NAME_LABEL, Phase, Schedule, UID_LABEL,
};
use fleet_api::meta::{API_VERSION, Millis, Object, ObjectMeta};
use fleet_api::plan::{
    Action, CREATE_ACTION, Create, Effect, EffectOutcome, Intent, Plan, Receipt,
};
use fleet_api::store::FieldManager;
use fleet_api::task;
use fleet_api::validate::typed;
use fleet_api::{Observation, StoreError};

/// How long an effect with no receipt waits before it is looked at again.
pub const RECOVERY_MS: Millis = 60_000;

/// What one reconcile of one CronTask reads.
#[derive(Debug, Clone)]
pub struct CronTaskInput<'a> {
    pub object: &'a Object,
    /// The Tasks whose `crontask.fleet.dev/uid` label is this CronTask's uid,
    /// from the sweep's one Task listing.
    pub tasks: Vec<&'a Object>,
    pub now: Millis,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct CronTaskController;

/// Condition types CronTaskController writes.
pub mod conditions {
    /// The schedule parses and the CronTask is not suspended.
    pub const SCHEDULED: &str = "Scheduled";
    /// Whether the last scheduled time was skipped, and why.
    pub const SKIPPED: &str = "Skipped";
}
use conditions::*;

impl CronTaskController {
    pub const KIND: &'static str = cron_task::KIND;
    pub const MANAGER: FieldManager = FieldManager::CronTaskController;

    /// Whether the manager should reconcile `obj` at `now`: never seen, its
    /// spec changed, or its next time came.
    pub fn due(obj: &Object, now: Millis) -> bool {
        if obj.status.is_null() {
            return true;
        }
        let seen = obj.status["observedGeneration"].as_u64().unwrap_or(0);
        seen != obj.metadata.generation || obj.status["notBefore"].as_u64().is_none_or(|t| t <= now)
    }

    pub fn reconcile(&self, input: &CronTaskInput<'_>) -> Result<Plan, StoreError> {
        let obj = input.object;
        if obj.is_deleting() {
            // No finalizer: the store removes it. Its Tasks stay.
            return Ok(Plan::default());
        }
        let spec: CronTaskSpec = typed(cron_task::KIND, &obj.spec)?;
        let before: CronTaskStatus = if obj.status.is_null() {
            CronTaskStatus::default()
        } else {
            serde_json::from_value(obj.status.clone())
                .map_err(|e| StoreError::Invalid(format!("{}: CronTask status: {e}", obj.key())))?
        };
        let mut pass = Pass {
            input,
            spec: &spec,
            st: before.clone(),
            creates: Vec::new(),
            effects: Vec::new(),
            recovering: false,
        };
        pass.advance();
        settle_transitions(&mut pass.st.conditions, &before.conditions);
        let status = (pass.st != before)
            .then(|| serde_json::to_value(&pass.st))
            .transpose()
            .map_err(|e| StoreError::Invalid(format!("CronTask status: {e}")))?;
        Ok(Plan {
            status,
            creates: pass.creates,
            effects: pass.effects,
            ..Plan::default()
        })
    }
}

/// One reconcile in progress.
struct Pass<'a, 'b> {
    input: &'a CronTaskInput<'b>,
    spec: &'a CronTaskSpec,
    st: CronTaskStatus,
    creates: Vec<Create>,
    effects: Vec<Effect>,
    /// Some effect still has no receipt: look again soon.
    recovering: bool,
}

impl Pass<'_, '_> {
    fn now(&self) -> Millis {
        self.input.now
    }

    fn generation(&self) -> u64 {
        self.input.object.metadata.generation
    }

    fn cond(&mut self, t: &str, s: ConditionStatus, reason: &str, message: impl Into<String>) {
        let (generation, now) = (self.generation(), self.now());
        set_condition(
            &mut self.st.conditions,
            t,
            s,
            reason,
            message,
            generation,
            now,
        );
    }

    fn task(&self, name: &str) -> Option<&Object> {
        self.input
            .tasks
            .iter()
            .copied()
            .find(|t| t.metadata.name == name)
    }

    fn advance(&mut self) {
        self.st.observed_generation = self.generation();
        let schedule = match Schedule::parse(&self.spec.schedule) {
            Ok(s) => s,
            Err(why) => {
                self.cond(SCHEDULED, ConditionStatus::False, "InvalidSchedule", why);
                self.st.not_before = None;
                self.st.next_schedule_time = None;
                return;
            }
        };
        self.settle_effects();
        self.prune_history();
        if self.spec.suspend {
            self.st.phase = Phase::Suspended;
            self.cond(
                SCHEDULED,
                ConditionStatus::False,
                "Suspended",
                "spec.suspend is true",
            );
        } else {
            self.st.phase = Phase::Scheduled;
            self.cond(SCHEDULED, ConditionStatus::True, "Scheduled", "");
            self.decide(&schedule);
        }
        self.st.active = self
            .input
            .tasks
            .iter()
            .filter(|t| !concluded(t) && !t.is_deleting())
            .map(|t| t.metadata.name.clone())
            .chain(self.creates.iter().map(|c| c.object.metadata.name.clone()))
            .collect();
        self.st.next_schedule_time = schedule.first_after(self.now());
        self.st.not_before = if self.recovering {
            let soon = self.now().saturating_add(RECOVERY_MS);
            Some(self.st.next_schedule_time.map_or(soon, |n| n.min(soon)))
        } else {
            self.st.next_schedule_time
        };
        self.prune_records();
    }

    /// The latest scheduled time not yet decided on: create its Task, or
    /// skip it and say why. Earlier missed times are dropped.
    fn decide(&mut self, schedule: &Schedule) {
        let base = self
            .st
            .last_considered
            .or(self.input.object.metadata.creation_timestamp)
            .unwrap_or(self.now());
        let Some(at) = schedule
            .latest_at_or_before(self.now())
            .filter(|&t| t > base)
        else {
            return;
        };
        self.st.last_considered = Some(at);
        if let Some(deadline) = self.spec.starting_deadline_ms()
            && self.now().saturating_sub(at) > deadline
        {
            let late = (self.now() - at) / 1000;
            self.cond(
                SKIPPED,
                ConditionStatus::True,
                "MissedDeadline",
                format!(
                    "the time {at} came {late}s ago, past startingDeadlineSeconds {}",
                    deadline / 1000
                ),
            );
            return;
        }
        let running: Vec<(String, String)> = self
            .input
            .tasks
            .iter()
            .filter(|t| !concluded(t) && !t.is_deleting())
            .map(|t| (t.metadata.name.clone(), t.metadata.uid.clone()))
            .collect();
        match self.spec.concurrency_policy {
            ConcurrencyPolicy::Forbid if !running.is_empty() => {
                let names: Vec<&str> = running.iter().map(|(n, _)| n.as_str()).collect();
                self.cond(
                    SKIPPED,
                    ConditionStatus::True,
                    "Forbidden",
                    format!("Task {} still runs", names.join(", ")),
                );
                return;
            }
            ConcurrencyPolicy::Replace => {
                for (name, uid) in running {
                    self.plan_retire(&name, &uid, true);
                }
            }
            _ => {}
        }
        let key = cron_task::create_key(at);
        if self.st.effects.contains_key(&key) {
            return;
        }
        let name = cron_task::task_name(&self.input.object.metadata.name, at);
        self.plan_create(&key, &name);
        self.st.last_schedule_time = Some(at);
        self.cond(
            SKIPPED,
            ConditionStatus::False,
            "Created",
            format!("Task/{name}"),
        );
    }

    fn intent(&self, action: &str) -> Intent {
        Intent {
            action: action.to_string(),
            at: self.now(),
            generation: self.generation(),
        }
    }

    /// Write the create's intent under `key` and ask the manager for it.
    fn plan_create(&mut self, key: &str, name: &str) {
        let intent = self.intent(CREATE_ACTION);
        let record = self
            .st
            .effects
            .entry(key.to_string())
            .or_insert_with(|| EffectRecord {
                intent: intent.clone(),
                receipt: None,
                attempts: 0,
                task: name.to_string(),
            });
        record.intent = intent;
        record.receipt = None;
        record.attempts += 1;
        self.creates.push(Create {
            key: key.to_string(),
            object: self.task_object(name),
            exclusive: None,
        });
    }

    /// Write the retire's intent and ask the manager to delete the Task.
    fn plan_retire(&mut self, name: &str, uid: &str, running: bool) {
        let key = cron_task::retire_key(uid);
        let action = Action::RetireScheduled {
            name: name.to_string(),
            uid: uid.to_string(),
            running,
        };
        let intent = self.intent(action.name());
        let record = self
            .st
            .effects
            .entry(key.clone())
            .or_insert_with(|| EffectRecord {
                intent: intent.clone(),
                receipt: None,
                attempts: 0,
                task: name.to_string(),
            });
        record.intent = intent;
        record.receipt = None;
        record.attempts += 1;
        self.effects.push(Effect { key, action });
    }

    /// The Task a scheduled time starts: the template's spec, its labels and
    /// this CronTask's two.
    fn task_object(&self, name: &str) -> Object {
        let meta = &self.input.object.metadata;
        let mut labels = self.spec.template.metadata.labels.clone();
        labels.insert(NAME_LABEL.to_string(), meta.name.clone());
        labels.insert(UID_LABEL.to_string(), meta.uid.clone());
        Object {
            api_version: API_VERSION.to_string(),
            kind: task::KIND.to_string(),
            metadata: ObjectMeta {
                name: name.to_string(),
                labels,
                ..ObjectMeta::default()
            },
            spec: self.spec.template.spec.clone(),
            status: serde_json::Value::Null,
        }
    }

    /// Each effect whose receipt is missing or says to try again. A missing
    /// or Unknown receipt is answered by looking for the Task in the
    /// listing, never by creating it twice: the name is the same for every
    /// attempt, and only a Task carrying this uid is taken as its own.
    fn settle_effects(&mut self) {
        let records: Vec<(String, EffectRecord)> = self
            .st
            .effects
            .iter()
            .map(|(k, r)| (k.clone(), r.clone()))
            .collect();
        for (key, record) in records {
            let creating = record.intent.action == CREATE_ACTION;
            let outcome = record.receipt.as_ref().map(|r| &r.outcome);
            let settled = matches!(
                outcome,
                Some(Observation::Known(
                    EffectOutcome::Created { .. } | EffectOutcome::Deleted
                ))
            );
            if settled {
                continue;
            }
            let found = self.task(&record.task).map(|t| t.metadata.uid.clone());
            if creating {
                if let Some(uid) = found {
                    self.recover(&key, EffectOutcome::Created { uid, adopted: true });
                    continue;
                }
                if record.attempts >= MAX_CREATE_ATTEMPTS {
                    if outcome.is_none() || matches!(outcome, Some(Observation::Unknown { .. })) {
                        self.recover(
                            &key,
                            EffectOutcome::Failed {
                                why: format!("{MAX_CREATE_ATTEMPTS} attempts made no Task"),
                            },
                        );
                    }
                    continue;
                }
                self.plan_create(&key, &record.task);
                self.recovering = true;
            } else if let Some(uid) = found {
                // The store refused: the Task is not this CronTask's to
                // delete any more. Nothing is tried again.
                if matches!(
                    outcome,
                    Some(Observation::Known(EffectOutcome::Failed { .. }))
                ) {
                    continue;
                }
                let running = !self.task(&record.task).is_some_and(concluded);
                self.plan_retire(&record.task, &uid, running);
                self.recovering = true;
            } else {
                self.recover(&key, EffectOutcome::Deleted);
            }
        }
    }

    fn recover(&mut self, key: &str, outcome: EffectOutcome) {
        let receipt = Receipt {
            action: self
                .st
                .effects
                .get(key)
                .map(|r| r.intent.action.clone())
                .unwrap_or_default(),
            at: self.now(),
            outcome: Observation::Known(outcome),
        };
        if let Some(record) = self.st.effects.get_mut(key) {
            record.receipt = Some(receipt);
        }
    }

    /// Keep the newest `historyLimit` Concluded Tasks; retire the rest.
    fn prune_history(&mut self) {
        let mut done: Vec<&Object> = self
            .input
            .tasks
            .iter()
            .copied()
            .filter(|t| concluded(t) && !t.is_deleting())
            .collect();
        done.sort_by_key(|t| {
            std::cmp::Reverse((t.metadata.creation_timestamp, t.metadata.name.clone()))
        });
        let old: Vec<(String, String)> = done
            .iter()
            .skip(self.spec.history_limit())
            .map(|t| (t.metadata.name.clone(), t.metadata.uid.clone()))
            .collect();
        for (name, uid) in old {
            if !self.st.effects.contains_key(&cron_task::retire_key(&uid)) {
                self.plan_retire(&name, &uid, false);
            }
        }
    }

    /// Keep every record still waiting, and the newest settled ones.
    fn prune_records(&mut self) {
        let mut settled: Vec<(Millis, String)> = self
            .st
            .effects
            .iter()
            .filter(|(_, r)| r.receipt.is_some())
            .map(|(k, r)| (r.intent.at, k.clone()))
            .collect();
        settled.sort();
        let excess = settled.len().saturating_sub(EFFECTS_KEPT);
        let drop: BTreeMap<String, ()> = settled
            .into_iter()
            .take(excess)
            .map(|(_, k)| (k, ()))
            .collect();
        self.st.effects.retain(|k, _| !drop.contains_key(k));
    }
}

fn concluded(task: &Object) -> bool {
    task.status["phase"].as_str() == Some("Concluded")
}
