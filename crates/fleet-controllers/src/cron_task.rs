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

pub const RECOVERY_MS: Millis = 60_000;

#[derive(Debug, Clone)]
pub struct CronTaskInput<'a> {
    pub object: &'a Object,
    pub tasks: Vec<&'a Object>,
    pub now: Millis,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct CronTaskController;

pub mod conditions {
    pub const SCHEDULED: &str = "Scheduled";
    pub const SKIPPED: &str = "Skipped";
    pub const NEEDS_ATTENTION: &str = "NeedsAttention";
}
use conditions::*;

impl CronTaskController {
    pub const KIND: &'static str = cron_task::KIND;
    pub const MANAGER: FieldManager = FieldManager::CronTaskController;

    pub fn due(obj: &Object, tasks: &[&Object], now: Millis) -> bool {
        if obj.status.is_null() {
            return true;
        }
        let seen = obj.status["observedGeneration"].as_u64().unwrap_or(0);
        seen != obj.metadata.generation
            || obj.status["notBefore"].as_u64().is_none_or(|t| t <= now)
            || left_active(obj, tasks)
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

struct Pass<'a, 'b> {
    input: &'a CronTaskInput<'b>,
    spec: &'a CronTaskSpec,
    st: CronTaskStatus,
    creates: Vec<Create>,
    effects: Vec<Effect>,
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
        // Before Forbid counts pending creates against a new time: one the
        // limits in force no longer allow must not block it.
        self.discard_stale_creates();
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
        self.attention();
        self.flush_creates();
        self.st.active = self
            .alive()
            .into_iter()
            .map(|(name, _)| name)
            .chain(self.pending_creates().into_iter().map(|(_, r)| r.task))
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
        let running = self.alive();
        let pending = self.pending_creates();
        match self.spec.concurrency_policy {
            ConcurrencyPolicy::Forbid if !running.is_empty() || !pending.is_empty() => {
                let names: Vec<&str> = running
                    .iter()
                    .map(|(n, _)| n.as_str())
                    .chain(pending.iter().map(|(_, r)| r.task.as_str()))
                    .collect();
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
                for (key, _) in pending {
                    self.give_up(&key, format!("replaced by the time {at}"));
                }
            }
            _ => {}
        }
        let key = cron_task::create_key(at);
        if self.st.effects.contains_key(&key) {
            return;
        }
        let name = cron_task::task_name(&self.input.object.metadata.name, at);
        self.write_create_intent(&key, &name, at);
        self.st.last_schedule_time = Some(at);
        self.cond(
            SKIPPED,
            ConditionStatus::False,
            "Created",
            format!("Task/{name}"),
        );
    }

    fn alive(&self) -> Vec<(String, String)> {
        self.input
            .tasks
            .iter()
            .filter(|t| !concluded(t) && !t.is_deleting())
            .map(|t| (t.metadata.name.clone(), t.metadata.uid.clone()))
            .collect()
    }

    fn pending_creates(&self) -> Vec<(String, EffectRecord)> {
        self.st
            .effects
            .iter()
            .filter(|(_, r)| r.intent.action == CREATE_ACTION && self.task(&r.task).is_none())
            .filter(|(_, r)| match r.receipt.as_ref().map(|x| &x.outcome) {
                None | Some(Observation::Unknown { .. }) => true,
                Some(Observation::Known(EffectOutcome::Failed { .. })) => {
                    r.attempts < MAX_CREATE_ATTEMPTS
                }
                Some(Observation::Known(_)) => false,
            })
            .map(|(k, r)| (k.clone(), r.clone()))
            .collect()
    }

    fn attention(&mut self) {
        let stuck: Vec<String> = self
            .pending_creates()
            .into_iter()
            .filter(|(_, r)| r.doubt() >= MAX_CREATE_ATTEMPTS)
            .map(|(_, r)| r.task)
            .collect();
        if !stuck.is_empty() {
            self.cond(
                NEEDS_ATTENTION,
                ConditionStatus::True,
                "CreateUnknown",
                format!(
                    "the store could not say whether Task {} was created; it is tried again every minute",
                    stuck.join(", ")
                ),
            );
        } else if self
            .st
            .conditions
            .iter()
            .any(|c| c.type_ == NEEDS_ATTENTION && c.status == ConditionStatus::True)
        {
            self.cond(NEEDS_ATTENTION, ConditionStatus::False, "Resolved", "");
        }
    }

    fn discard_stale_creates(&mut self) {
        for (key, record) in self.pending_creates() {
            let scheduled = record.scheduled.unwrap_or(record.intent.at);
            if self.spec.suspend {
                self.give_up(&key, "the CronTask is suspended".into());
                continue;
            }
            if self.spec.concurrency_policy == ConcurrencyPolicy::Forbid
                && let Some((other, _)) = self
                    .alive()
                    .into_iter()
                    .find(|(name, _)| *name != record.task)
            {
                self.cond(
                    SKIPPED,
                    ConditionStatus::True,
                    "Forbidden",
                    format!("Task {other} still runs"),
                );
                self.give_up(&key, format!("Forbid: Task {other} still runs"));
                continue;
            }
            if let Some(deadline) = self.spec.starting_deadline_ms()
                && self.now().saturating_sub(scheduled) > deadline
            {
                let late = (self.now() - scheduled) / 1000;
                self.cond(
                    SKIPPED,
                    ConditionStatus::True,
                    "MissedDeadline",
                    format!(
                        "the time {scheduled} came {late}s ago, past startingDeadlineSeconds {}",
                        deadline / 1000
                    ),
                );
                self.give_up(&key, "past startingDeadlineSeconds".into());
            }
        }
    }

    fn flush_creates(&mut self) {
        self.discard_stale_creates();
        for (key, record) in self.pending_creates() {
            if self.spec.concurrency_policy == ConcurrencyPolicy::Replace
                && self
                    .input
                    .tasks
                    .iter()
                    .any(|t| t.metadata.name != record.task && (!concluded(t) || t.is_deleting()))
            {
                self.cond(
                    SKIPPED,
                    ConditionStatus::False,
                    "Replacing",
                    format!(
                        "Task/{} waits for the Task it replaces to be released",
                        record.task
                    ),
                );
                self.recovering = true;
                continue;
            }
            // Look again soon, the first attempt included: should this one
            // never land, the next look is within a minute, not a period.
            self.recovering = true;
            self.plan_create(&key, &record.task);
        }
    }

    fn give_up(&mut self, key: &str, why: String) {
        self.recover(key, EffectOutcome::Failed { why });
        if let Some(record) = self.st.effects.get_mut(key) {
            record.attempts = record.attempts.max(MAX_CREATE_ATTEMPTS);
        }
    }

    fn intent(&self, action: &str) -> Intent {
        Intent {
            action: action.to_string(),
            at: self.now(),
            generation: self.generation(),
        }
    }

    fn write_create_intent(&mut self, key: &str, name: &str, at: Millis) {
        let intent = self.intent(CREATE_ACTION);
        self.st.effects.insert(
            key.to_string(),
            EffectRecord {
                intent,
                receipt: None,
                attempts: 0,
                task: name.to_string(),
                scheduled: Some(at),
                unknowns: 0,
                running: false,
            },
        );
    }

    fn plan_create(&mut self, key: &str, name: &str) {
        let intent = self.intent(CREATE_ACTION);
        if let Some(record) = self.st.effects.get_mut(key) {
            // The doubt is kept on the record: a retry clears the receipt,
            // and a Known refusal is the only answer that ends the doubt.
            let refused = matches!(
                record.receipt.as_ref().map(|r| &r.outcome),
                Some(Observation::Known(_))
            );
            record.unknowns = if refused { 0 } else { record.doubt() };
            record.intent = intent;
            record.receipt = None;
            record.attempts += 1;
        }
        self.creates.push(Create {
            key: key.to_string(),
            object: self.task_object(name),
            exclusive: None,
        });
    }

    fn plan_retire(&mut self, name: &str, uid: &str, running: bool) {
        let key = cron_task::retire_key(uid);
        let intent = self.intent("retireScheduled");
        let record = self
            .st
            .effects
            .entry(key.clone())
            .or_insert_with(|| EffectRecord {
                intent: intent.clone(),
                receipt: None,
                attempts: 0,
                task: name.to_string(),
                scheduled: None,
                unknowns: 0,
                running: false,
            });
        record.running = (record.running || running)
            && self.spec.concurrency_policy == ConcurrencyPolicy::Replace;
        record.intent = intent;
        record.receipt = None;
        record.attempts += 1;
        let action = Action::RetireScheduled {
            name: name.to_string(),
            uid: uid.to_string(),
            running: record.running,
        };
        self.effects.retain(|e| e.key != key);
        self.effects.push(Effect { key, action });
        // Look again soon: should this delete never land, it is retried
        // within a minute, not at the next scheduled time.
        self.recovering = true;
    }

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
                // Adopt a Task an earlier attempt made; an absent one is a
                // pending create, which `flush_creates` decides about.
                if let Some(uid) = found {
                    self.recover(&key, EffectOutcome::Created { uid, adopted: true });
                }
            } else if let Some(uid) = found {
                // The store refused: the Task is not this CronTask's to
                // delete any more. Nothing is tried again.
                if matches!(
                    outcome,
                    Some(Observation::Known(EffectOutcome::Failed { .. }))
                ) {
                    continue;
                }
                self.plan_retire(&record.task, &uid, record.running);
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
            // A fresh decision: none was made, or the store refused the last
            // one (the Task was reopened since). One still in flight is
            // `settle_effects`' to retry.
            let refused = self
                .st
                .effects
                .get(&cron_task::retire_key(&uid))
                .is_none_or(|r| {
                    matches!(
                        r.receipt.as_ref().map(|x| &x.outcome),
                        Some(Observation::Known(EffectOutcome::Failed { .. }))
                    )
                });
            if refused {
                self.plan_retire(&name, &uid, false);
            }
        }
    }

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

fn left_active(obj: &Object, tasks: &[&Object]) -> bool {
    let Ok(st) = serde_json::from_value::<CronTaskStatus>(obj.status.clone()) else {
        return false;
    };
    st.active.iter().any(
        |name| match tasks.iter().find(|t| t.metadata.name == *name) {
            Some(task) => concluded(task) || task.is_deleting(),
            None => !creating(&st, name),
        },
    )
}

fn creating(st: &CronTaskStatus, name: &str) -> bool {
    st.effects.values().any(|r| {
        r.intent.action == CREATE_ACTION
            && r.task == name
            && !matches!(
                r.receipt.as_ref().map(|x| &x.outcome),
                Some(Observation::Known(EffectOutcome::Created { .. }))
            )
    })
}

fn concluded(task: &Object) -> bool {
    task.status["phase"].as_str() == Some("Concluded")
}
