mod cron_tasks;
mod execution;
mod fleet_events;
mod outbox;
pub mod plugins;
mod subscriptions;
mod telemetry;
pub mod verbs;

pub use telemetry::{Engine, identify, manager_lease};

use std::collections::{BTreeMap, BTreeSet};

use fleet_api::change_request::{self, ChangeRequestDefaults, ChangeRequestSpec};
use fleet_api::conversation::{
    self, CONVERSATION_UID_LABEL, ConversationSpec, ConversationStatus, ThreadDir,
};
use fleet_api::follow::{ForgeDefaults, ForgeStatus, declared_changes};
use fleet_api::forge::{ChangeObservation, Forge, Uncertainty};
use fleet_api::gateway::{
    Capabilities, Gateway, GatewayConfig, HistoryObservation, Identity, ListenerAccount,
};
use fleet_api::listener::{self, ListenerSpec};
use fleet_api::plan::{
    Action, CREATE_ACTION, Create, Effect, EffectOutcome, Observations, Plan, Probe, Receipt,
};
use fleet_api::runtime::Runtime;
use fleet_api::session::SessionRef;
use fleet_api::talk::{GatewayStatus, PARENT_LABEL, PARENT_UID_LABEL};
use fleet_api::task;
use fleet_api::{Clock, LeaseToken, Millis, Object, Observation, Store, StoreError};
use fleet_controllers::{
    ChangeRequestController, ChangeRequestInput, ChangeRequestObservations, ConversationController,
    ConversationInput, ConversationObservations, ForgeController, ForgeInput, ForgeObservations,
    GatewayController, GatewayInput, GatewayObservations, ListenerController, ListenerInput,
    ListenerObservations, TaskController, TaskInput,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub use verbs::{
    AmendRequest, Amended, ApplyOutcome, DeclareOutcome, Declared, Received, Refusal, Resolution,
    ResultOutcome, Sent, amend, apply, apply_change_request, apply_conversation, beat,
    conversation_name, declare, declare_change_request, delete, derived_name, ensure_listeners,
    fetch_file, label, list_events, pause, publish_event, receive, record_result, resolve_inbox,
    restart, send, wake,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RunOptions {
    pub max_passes: u32,
    pub lease_ttl: Millis,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunReport {
    pub holder: String,
    pub lease_token: u64,
    pub passes: u32,
    pub idle: bool,
    pub status_writes: u32,
    pub effects: u32,
    pub conflicts: u32,
    pub errors: Vec<String>,
    pub unknowns: Vec<String>,
    #[serde(skip)]
    pub waiting: Vec<(SessionRef, u64)>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub paused: Option<Paused>,
}

const PAUSE_FIRST_RETRY: Millis = 5_000;
const PAUSE_LAST_RETRY: Millis = 60_000;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Paused {
    pub why: String,
    pub retry_after: Millis,
}

struct WaitingOnAgent {
    name: String,
    session: SessionRef,
    mark: u64,
    outbox_drained: Observation<bool>,
}

pub struct Manager<'a> {
    pub store: &'a dyn Store,
    pub runtime: &'a dyn Runtime,
    pub clock: &'a dyn Clock,
    pub holder: String,
    pub plugins: Option<(
        &'a dyn fleet_api::plugin::Plugins,
        &'a [fleet_api::plugin::Registration],
    )>,
    pub forge: Option<&'a dyn Forge>,
    pub signals: Option<&'a dyn fleet_api::signal::Signals>,
    pub agent_telemetry: Option<fleet_api::harness::Collector>,
    pub change_request_defaults: ChangeRequestDefaults,
    pub forge_defaults: ForgeDefaults,
    pub gateway: Option<GatewaySide<'a>>,
}

pub struct GatewaySide<'a> {
    pub port: &'a dyn Gateway,
    pub config: GatewayConfig,
    pub digests: BTreeMap<String, String>,
    pub briefs: BTreeMap<String, String>,
}

impl std::fmt::Debug for GatewaySide<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GatewaySide")
            .field("accounts", &self.config.accounts.keys().collect::<Vec<_>>())
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Default)]
struct GatewayCache {
    capabilities: BTreeMap<String, Observation<Capabilities>>,
    whoami: BTreeMap<String, Observation<Identity>>,
    deferred: BTreeMap<String, HistoryObservation>,
}

impl std::fmt::Debug for Manager<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Manager")
            .field("holder", &self.holder)
            .finish_non_exhaustive()
    }
}

const FORGE_LOGIN_KEPT_MS: Millis = 10 * 60_000;
const CHANGE_POLLS_PER_PASS: std::time::Duration = std::time::Duration::from_secs(1);

#[derive(Debug, Default)]
struct ForgeLogins(BTreeMap<String, (Millis, Observation<String>)>);

impl ForgeLogins {
    fn forget_unknown_and_old(&mut self, now: Millis) {
        self.0.retain(|_, (asked_at, login)| {
            login.known().is_some() && now.saturating_sub(*asked_at) < FORGE_LOGIN_KEPT_MS
        });
    }

    fn get(&self, host: &str) -> Option<Observation<String>> {
        self.0.get(host).map(|(_, login)| login.clone())
    }

    fn insert(&mut self, host: String, asked_at: Millis, login: Observation<String>) {
        self.0.insert(host, (asked_at, login));
    }
}

#[derive(Debug, Default)]
struct Step {
    wrote: u32,
    effects: u32,
    conflict: bool,
}

#[derive(Debug, Clone)]
struct Deferral {
    kind: Uncertainty,
    why: String,
    retry_after: Millis,
}

impl Deferral {
    fn observation(&self) -> ChangeObservation {
        ChangeObservation::Unknown {
            kind: self.kind,
            why: format!("deferred with the host: {}", self.why),
            retry_after: Some(self.retry_after),
        }
    }
}

fn change_request_followers(tasks: &[Object]) -> BTreeSet<String> {
    tasks
        .iter()
        .filter(|t| !t.is_deleting())
        .filter_map(|t| t.status.get("declared")?.as_array())
        .flat_map(|declared| declared_changes(declared))
        .map(|d| d.name)
        .collect()
}

fn host_of(obj: &Object) -> String {
    obj.status["ref"]["host"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

const STARTUP_STEPS_IN_A_ROW: usize = 16;

const RELIST_AFTER_MS: Millis = 250;

struct NewStarts {
    seen: BTreeSet<(String, u64)>,
    started_here: BTreeSet<String>,
    listed_at: Millis,
}

fn generation_of(obj: &Object) -> (String, u64) {
    (obj.metadata.uid.clone(), obj.metadata.generation)
}

fn starting(obj: &Object) -> bool {
    let Ok(spec) = serde_json::from_value::<task::TaskSpec>(obj.spec.clone()) else {
        return false;
    };
    let phase = match obj.status.get("phase") {
        None => task::Phase::Pending,
        Some(phase) => match serde_json::from_value::<task::Phase>(phase.clone()) {
            Ok(phase) => phase,
            Err(_) => return false,
        },
    };
    !obj.is_deleting()
        && spec.dispatch
        && match phase {
            task::Phase::Pending | task::Phase::Spawning => true,
            task::Phase::Spawned => !spec.is_interactive(),
            _ => false,
        }
}

impl Manager<'_> {
    pub fn serve(
        &self,
        opts: RunOptions,
        mut wait: impl FnMut(&RunReport) -> bool,
    ) -> Result<(), StoreError> {
        let mut pauses = 0;
        let mut lease = loop {
            match self.store.acquire_lease(&self.holder, opts.lease_ttl) {
                Err(StoreError::Full(at)) => {
                    let report = self.paused(0, &mut pauses, at);
                    if !wait(&report) {
                        return Ok(());
                    }
                }
                acquired => break acquired?,
            }
        };
        telemetry::manager_lease(true);
        let mut logins = ForgeLogins::default();
        let one_pass = RunOptions {
            max_passes: 1,
            ..opts
        };
        let mut sweep_opts = one_pass;
        let outcome = (|| {
            loop {
                let report = match self.sweep(&mut lease, sweep_opts, pauses > 0, &mut logins) {
                    Ok(report) => {
                        pauses = 0;
                        sweep_opts = opts;
                        report
                    }
                    Err(StoreError::Full(at)) => {
                        sweep_opts = one_pass;
                        self.paused(lease.token, &mut pauses, at)
                    }
                    Err(e) => return Err(e),
                };
                if !wait(&report) {
                    return Ok(());
                }
            }
        })();
        if !matches!(outcome, Err(StoreError::LeaseLost(_))) {
            match self.store.release_lease(&lease) {
                Err(StoreError::Full(_)) => {}
                released => released?,
            }
        }
        telemetry::manager_lease(false);
        outcome
    }

    fn sweep(
        &self,
        lease: &mut LeaseToken,
        opts: RunOptions,
        resuming: bool,
        logins: &mut ForgeLogins,
    ) -> Result<RunReport, StoreError> {
        *lease = if resuming {
            self.store.reclaim_lease(lease, opts.lease_ttl)?
        } else {
            self.store.renew_lease(lease, opts.lease_ttl)?
        };
        let mut report = RunReport {
            holder: self.holder.clone(),
            lease_token: lease.token,
            ..RunReport::default()
        };
        self.passes(lease, opts, &mut report, logins)?;
        *lease = self.store.renew_lease(lease, opts.lease_ttl)?;
        Ok(report)
    }

    fn paused(&self, lease_token: u64, pauses: &mut u32, at: String) -> RunReport {
        let retry_after = PAUSE_FIRST_RETRY
            .saturating_mul(1 << (*pauses).min(4))
            .min(PAUSE_LAST_RETRY);
        *pauses += 1;
        RunReport {
            holder: self.holder.clone(),
            lease_token,
            paused: Some(Paused {
                why: StoreError::Full(at).to_string(),
                retry_after,
            }),
            ..RunReport::default()
        }
    }

    pub fn run(&self, opts: RunOptions) -> Result<RunReport, StoreError> {
        let mut lease = self.store.acquire_lease(&self.holder, opts.lease_ttl)?;
        let mut report = RunReport {
            holder: self.holder.clone(),
            lease_token: lease.token,
            ..RunReport::default()
        };
        let outcome = self.passes(&mut lease, opts, &mut report, &mut ForgeLogins::default());
        if !matches!(outcome, Err(StoreError::LeaseLost(_))) {
            self.store.release_lease(&lease)?;
        }
        outcome.map(|()| report)
    }

    fn passes(
        &self,
        lease: &mut LeaseToken,
        opts: RunOptions,
        report: &mut RunReport,
        logins: &mut ForgeLogins,
    ) -> Result<(), StoreError> {
        let mut passes = 0;
        if let Some(gateway) = &self.gateway {
            for problem in verbs::ensure_listeners(self.store, &gateway.config) {
                if !report.errors.contains(&problem) {
                    report.errors.push(problem);
                }
            }
        }
        while passes < opts.max_passes {
            passes += 1;
            report.passes += 1;
            let tasks = match self.store.list(task::KIND) {
                Observation::Known(t) => t,
                Observation::Unknown { why } => {
                    report.unknowns.push(format!("list Task: {why}"));
                    report.idle = false;
                    return Ok(());
                }
            };
            let _pass = telemetry::pass(passes, &tasks);
            let mut busy = false;
            logins.forget_unknown_and_old(self.clock.now());
            let workers: BTreeMap<String, String> = tasks
                .iter()
                .map(|t| (t.metadata.uid.clone(), t.metadata.name.clone()))
                .collect();
            let conversations: Observation<BTreeMap<String, Object>> =
                self.store.list(conversation::KIND).map(|list| {
                    list.into_iter()
                        .map(|c| (c.metadata.name.clone(), c))
                        .collect()
                });
            if let Some(why) = conversations.why() {
                report.unknowns.push(format!("list Conversation: {why}"));
            }
            let follows = tasks.iter().any(|t| {
                t.status["forge"]["changes"]
                    .as_object()
                    .is_some_and(|c| !c.is_empty())
            });
            let change_requests: BTreeMap<String, Object> = if !follows {
                BTreeMap::new()
            } else {
                match self.store.list(change_request::KIND) {
                    Observation::Known(list) => list
                        .into_iter()
                        .map(|c| (c.metadata.name.clone(), c))
                        .collect(),
                    Observation::Unknown { why } => {
                        report.unknowns.push(format!("list ChangeRequest: {why}"));
                        BTreeMap::new()
                    }
                }
            };
            let mut children_of: BTreeMap<&str, Vec<&Object>> = BTreeMap::new();
            for t in &tasks {
                if let (Some(parent), Some(_)) = (
                    t.metadata.labels.get(PARENT_LABEL),
                    t.metadata.labels.get(PARENT_UID_LABEL),
                ) {
                    children_of.entry(parent.as_str()).or_default().push(t);
                }
            }
            let now = self.clock.now();
            let mut order: Vec<&Object> = tasks.iter().collect();
            order.sort_by_key(|t| !starting(t));
            let mut waiting: Vec<WaitingOnAgent> = Vec::new();
            let mut new_starts = NewStarts {
                seen: tasks.iter().map(generation_of).collect(),
                started_here: BTreeSet::new(),
                listed_at: self.clock.now(),
            };
            for obj in order {
                busy |=
                    self.reconcile_reported_starts(&mut waiting, lease, opts.lease_ttl, report)?;
                busy |= self.reconcile_new_starts(
                    &mut new_starts,
                    &mut waiting,
                    lease,
                    opts.lease_ttl,
                    report,
                )?;
                if new_starts.started_here.contains(&obj.metadata.uid) {
                    continue;
                }
                let published = match self.publish_fleet_events(obj, lease, opts.lease_ttl, report)
                {
                    Ok(written) => written,
                    Err(e @ StoreError::LeaseLost(_)) => return Err(e),
                    Err(e) => {
                        report
                            .errors
                            .push(format!("{} fleet events: {e}", obj.key()));
                        None
                    }
                };
                if published.is_some() {
                    report.status_writes += 1;
                    busy = true;
                }
                let obj = published.as_ref().unwrap_or(obj);
                let concluded =
                    obj.status.get("phase").and_then(Value::as_str) == Some("Concluded");
                let read_outbox = !concluded || obj.is_deleting() || self.due(obj);
                let read = if read_outbox {
                    self.ingest_outbox(obj, &tasks, lease, opts.lease_ttl, report)
                } else {
                    Ok(outbox::Ingested::unread())
                };
                let ingested = match read {
                    Ok(ingested) => ingested,
                    Err(e @ (StoreError::LeaseLost(_) | StoreError::Full(_))) => return Err(e),
                    Err(e) => {
                        report.errors.push(format!("{} outbox: {e}", obj.key()));
                        outbox::Ingested {
                            written: None,
                            drained: Observation::unknown(e.to_string()),
                        }
                    }
                };
                busy |= ingested.written.is_some();
                let outbox_drained = ingested.drained;
                let obj = ingested.written.as_ref().unwrap_or(obj);
                let notified = match self.notify_subscriptions(obj, lease, opts.lease_ttl, report) {
                    Ok(written) => written,
                    Err(e @ (StoreError::LeaseLost(_) | StoreError::Full(_))) => return Err(e),
                    Err(e) => {
                        report
                            .errors
                            .push(format!("{} subscriptions: {e}", obj.key()));
                        None
                    }
                };
                busy |= notified.is_some();
                let obj = notified.as_ref().unwrap_or(obj);
                let task_due = self.due(obj);
                let forge_due = ForgeController::due(obj, now);
                let gateway_due = {
                    let conversation = match &conversations {
                        Observation::Known(map) => Observation::Known(
                            GatewayController::conversation_of(obj).and_then(|name| map.get(&name)),
                        ),
                        Observation::Unknown { why } => Observation::Unknown { why: why.clone() },
                    };
                    let children = children_of
                        .get(obj.metadata.name.as_str())
                        .map(Vec::as_slice)
                        .unwrap_or_default();
                    GatewayController::due_with(obj, now, &conversation, children, &change_requests)
                };
                if !(task_due || forge_due || gateway_due) {
                    continue;
                }
                *lease = self.store.renew_lease(lease, opts.lease_ttl)?;
                let mut moved = false;
                if task_due {
                    let (stepped, rest) = self.reconcile_while_starting(
                        obj,
                        &outbox_drained,
                        lease,
                        opts.lease_ttl,
                        report,
                    )?;
                    busy |= stepped;
                    waiting.extend(rest);
                    moved = true;
                }
                let fresh = |moved: bool, report: &mut RunReport| -> Option<Object> {
                    if !moved {
                        return Some(obj.clone());
                    }
                    match self.store.get(task::KIND, &obj.metadata.name) {
                        Observation::Known(Some(o)) => Some(o),
                        Observation::Known(None) => None,
                        Observation::Unknown { why } => {
                            report.unknowns.push(format!("{} reread: {why}", obj.key()));
                            None
                        }
                    }
                };
                if forge_due {
                    let Some(current) = fresh(moved, report) else {
                        continue;
                    };
                    let step = telemetry::reconcile(
                        "ForgeController",
                        &current,
                        || self.clock.now(),
                        || {
                            self.reconcile_forge(
                                &current,
                                lease,
                                opts.lease_ttl,
                                report,
                                logins,
                                &workers,
                            )
                        },
                    );
                    busy |= self.tally(step, &obj.key(), report)?;
                    moved = true;
                }
                if gateway_due {
                    let Some(current) = fresh(moved, report) else {
                        continue;
                    };
                    let step = telemetry::reconcile(
                        "GatewayController",
                        &current,
                        || self.clock.now(),
                        || self.reconcile_gateway(&current, lease, opts.lease_ttl, report, &tasks),
                    );
                    busy |= self.tally(step, &obj.key(), report)?;
                }
            }
            busy |= self.reconcile_reported_starts(&mut waiting, lease, opts.lease_ttl, report)?;
            report.waiting = waiting.into_iter().map(|w| (w.session, w.mark)).collect();
            busy |= self.sweep_change_requests(&tasks, lease, opts.lease_ttl, report)?;
            busy |= self.sweep_cron_tasks(lease, opts.lease_ttl, report)?;
            let mut cache = GatewayCache::default();
            busy |= self.sweep_conversations(lease, opts.lease_ttl, report, &mut cache)?;
            busy |= self.sweep_listeners(lease, opts.lease_ttl, report, &mut cache)?;
            busy |= self.sweep_plugins(lease, opts.lease_ttl, report)?;
            if !busy {
                report.idle = true;
                return Ok(());
            }
        }
        report.idle = false;
        Ok(())
    }

    fn sweep_change_requests(
        &self,
        tasks: &[Object],
        lease: &mut LeaseToken,
        ttl: Millis,
        report: &mut RunReport,
    ) -> Result<bool, StoreError> {
        let changes = match self.store.list(change_request::KIND) {
            Observation::Known(c) => c,
            Observation::Unknown { why } => {
                report.unknowns.push(format!("list ChangeRequest: {why}"));
                return Ok(false);
            }
        };
        let followers = change_request_followers(tasks);
        let followed = |c: &Object| followers.contains(&c.metadata.name);
        let now = self.clock.now();
        let mut due: Vec<&Object> = changes
            .iter()
            .filter(|c| {
                ChangeRequestController
                    .due(c, followed(c), now)
                    .unwrap_or_else(|_| self.due(c))
            })
            .collect();
        due.sort_by_cached_key(|o| {
            (
                o.status["notBefore"].as_u64().unwrap_or(0),
                host_of(o),
                o.metadata.name.clone(),
            )
        });
        let mut deferred: BTreeMap<String, Deferral> = BTreeMap::new();
        let mut busy = false;
        let started = std::time::Instant::now();
        for obj in due {
            if started.elapsed() >= CHANGE_POLLS_PER_PASS && !deferred.contains_key(&host_of(obj)) {
                busy = true;
                continue;
            }
            *lease = self.store.renew_lease(lease, ttl)?;
            let step = telemetry::reconcile(
                "ChangeRequestController",
                obj,
                || self.clock.now(),
                || {
                    self.reconcile_change_request(
                        obj,
                        followed(obj),
                        lease,
                        ttl,
                        report,
                        &mut deferred,
                    )
                },
            );
            busy |= self.tally(step, &obj.key(), report)?;
        }
        Ok(busy)
    }

    fn reconcile_change_request(
        &self,
        obj: &Object,
        followed: bool,
        lease: &mut LeaseToken,
        ttl: Millis,
        report: &mut RunReport,
        deferred: &mut BTreeMap<String, Deferral>,
    ) -> Result<Step, StoreError> {
        let Some(forge) = self.forge else {
            return Err(StoreError::Invalid(
                "no [forge] section is configured; a ChangeRequest cannot be polled".into(),
            ));
        };
        let spec: ChangeRequestSpec = fleet_api::validate::typed(change_request::KIND, &obj.spec)?;
        let mut input = ChangeRequestInput {
            object: obj,
            observations: ChangeRequestObservations::default(),
            now: self.clock.now(),
            defaults: self.change_request_defaults,
            followed,
        };
        if obj.status.get("ref").is_none_or(Value::is_null) {
            let parsed = forge.parse(&spec.url);
            if let Some(why) = parsed.why() {
                report.unknowns.push(format!("{} parse: {why}", obj.key()));
            }
            input.observations.parsed = Some(parsed);
        }
        let ctl = ChangeRequestController;
        for probe in ctl.probes(&input)? {
            let Probe::Change { change } = &probe else {
                continue;
            };
            *lease = self.store.renew_lease(lease, ttl)?;
            let answer = match deferred.get(&change.host) {
                Some(d) => d.observation(),
                None => telemetry::forge_poll(obj, &change.host, || forge.change(change)),
            };
            if let ChangeObservation::Unknown {
                kind,
                why,
                retry_after: Some(wait),
            } = &answer
            {
                deferred.entry(change.host.clone()).or_insert(Deferral {
                    kind: *kind,
                    why: why.clone(),
                    retry_after: *wait,
                });
            }
            if let Some(why) = answer.why() {
                report.unknowns.push(format!("{} change: {why}", obj.key()));
            }
            input.observations.change = Some(answer);
        }
        let plan = ctl.reconcile(&input)?;
        let by = ChangeRequestController::MANAGER;
        let name = &obj.metadata.name;
        let mut step = Step::default();
        let mut rv = obj.metadata.resource_version;
        if let Some(status) = plan.status {
            match self.store.update_status(
                change_request::KIND,
                name,
                status,
                rv,
                by.clone(),
                Some(&*lease),
            ) {
                Ok(o) => {
                    rv = o.metadata.resource_version;
                    step.wrote += 1;
                }
                Err(StoreError::Conflict { .. }) => {
                    step.conflict = true;
                    return Ok(step);
                }
                Err(e) => return Err(e),
            }
        }
        if plan.delete {
            *lease = self.store.renew_lease(lease, ttl)?;
            match self.store.delete_fenced(
                change_request::KIND,
                name,
                &obj.metadata.uid,
                rv,
                by.clone(),
                lease,
            ) {
                Ok(_) => step.wrote += 1,
                Err(StoreError::Conflict { .. }) => step.conflict = true,
                Err(e) => return Err(e),
            }
        }
        Ok(step)
    }

    fn sweep_conversations(
        &self,
        lease: &mut LeaseToken,
        ttl: Millis,
        report: &mut RunReport,
        cache: &mut GatewayCache,
    ) -> Result<bool, StoreError> {
        let objects = match self.store.list(conversation::KIND) {
            Observation::Known(c) => c,
            Observation::Unknown { why } => {
                report.unknowns.push(format!("list Conversation: {why}"));
                return Ok(false);
            }
        };
        let now = self.clock.now();
        let mut due: Vec<&Object> = objects
            .iter()
            .filter(|o| ConversationController::due(o, now))
            .collect();
        due.sort_by_cached_key(|o| {
            (
                o.spec["account"].as_str().unwrap_or_default().to_string(),
                o.metadata.name.clone(),
            )
        });
        let mut busy = false;
        for obj in due {
            *lease = self.store.renew_lease(lease, ttl)?;
            let step = telemetry::reconcile(
                "ConversationController",
                obj,
                || self.clock.now(),
                || self.reconcile_conversation(obj, lease, ttl, report, cache),
            );
            busy |= self.tally(step, &obj.key(), report)?;
        }
        Ok(busy)
    }

    fn reconcile_conversation(
        &self,
        obj: &Object,
        lease: &mut LeaseToken,
        ttl: Millis,
        report: &mut RunReport,
        cache: &mut GatewayCache,
    ) -> Result<Step, StoreError> {
        let Some(gateway) = &self.gateway else {
            return Err(StoreError::Invalid(
                "no [gateway] section is configured; a Conversation cannot be reconciled".into(),
            ));
        };
        let spec: ConversationSpec = fleet_api::validate::typed(conversation::KIND, &obj.spec)?;
        let mut input = ConversationInput {
            object: obj,
            observations: ConversationObservations::default(),
            now: self.clock.now(),
            defaults: gateway.config.defaults().for_route(
                gateway.config.routes.get(&spec.route).cloned(),
                gateway.briefs.get(&spec.route).cloned(),
            ),
        };
        if obj.status.get("ref").is_none_or(Value::is_null) {
            let parsed = gateway.port.parse(&spec.account, &spec.address);
            if let Some(why) = parsed.why() {
                report.unknowns.push(format!("{} parse: {why}", obj.key()));
            }
            input.observations.parsed = Some(parsed);
        }
        let ctl = ConversationController;
        for _ in 0..4 {
            let probes = ctl.probes(&input)?;
            if probes.is_empty() {
                break;
            }
            for probe in probes {
                match probe {
                    Probe::Object { kind, name } => {
                        let o = self.store.get(&kind, &name);
                        if let Some(why) = o.why() {
                            report
                                .unknowns
                                .push(format!("{} object {kind}/{name}: {why}", obj.key()));
                        }
                        if obj.status["task"]["name"].as_str() == Some(name.as_str()) {
                            input.observations.task = Some(o);
                        } else {
                            input.observations.opener = Some(o);
                        }
                    }
                    Probe::Capabilities { account } => {
                        let answer = self.capabilities(gateway, &account, lease, ttl, cache)?;
                        if let Some(why) = answer.why() {
                            report
                                .unknowns
                                .push(format!("{} capabilities {account}: {why}", obj.key()));
                        }
                        input.observations.capabilities = Some(answer);
                    }
                    Probe::History {
                        account,
                        conversation,
                        query,
                    } => {
                        let answer = match cache.deferred.get(&account) {
                            Some(d) => d.clone(),
                            None => {
                                *lease = self.store.renew_lease(lease, ttl)?;
                                gateway.port.history(&account, &conversation, &query)
                            }
                        };
                        if let HistoryObservation::Unknown {
                            kind,
                            why,
                            retry_after: Some(wait),
                        } = &answer
                        {
                            cache.deferred.entry(account.clone()).or_insert(
                                HistoryObservation::Unknown {
                                    kind: *kind,
                                    why: format!("deferred with the account: {why}"),
                                    retry_after: Some(*wait),
                                },
                            );
                        }
                        if let HistoryObservation::Unknown { why, .. } = &answer {
                            report
                                .unknowns
                                .push(format!("{} history: {why}", obj.key()));
                        }
                        input.observations.history = Some(answer);
                    }
                    _ => {}
                }
            }
        }
        let plan = ctl.reconcile(&input)?;
        let by = ConversationController::MANAGER;
        let name = &obj.metadata.name;
        let mut step = Step::default();
        let mut rv = obj.metadata.resource_version;
        let mut current = obj.clone();
        if let Some(status) = plan.status {
            match self
                .store
                .record_received(name, status, rv, by.clone(), lease)
            {
                Ok(o) => {
                    rv = o.metadata.resource_version;
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
        for create in &plan.creates {
            *lease = self.store.renew_lease(lease, ttl)?;
            let (outcome, made) = self.perform_create(create, lease);
            if let Observation::Unknown { why } = &outcome {
                report
                    .unknowns
                    .push(format!("{} {CREATE_ACTION}: {why}", obj.key()));
            }
            step.effects += 1;
            current = self.write_conversation_receipt(
                &current,
                &create.key,
                CREATE_ACTION,
                outcome,
                made.as_ref(),
                lease,
            )?;
            rv = current.metadata.resource_version;
            step.wrote += 1;
        }
        for effect in &plan.effects {
            *lease = self.store.renew_lease(lease, ttl)?;
            let outcome = match &effect.action {
                Action::Post {
                    account,
                    conversation: thread,
                    text,
                } => gateway.port.post(account, thread, text, &effect.key),
                Action::React {
                    account,
                    conversation: thread,
                    message,
                    reaction,
                    on,
                } => gateway.port.react(account, thread, message, reaction, *on),
                Action::DeleteTask { name: task, uid } => {
                    self.retire_task(task, uid, &obj.metadata.uid, lease)
                }
                Action::RemoveDir { host, path } => self.remove_thread_dir(obj, host, path),
                _ => continue,
            };
            if let Observation::Unknown { why } = &outcome {
                report
                    .unknowns
                    .push(format!("{} {}: {why}", obj.key(), effect.action.name()));
            }
            step.effects += 1;
            current = self.write_conversation_receipt(
                &current,
                &effect.key,
                effect.action.name(),
                outcome,
                None,
                lease,
            )?;
            rv = current.metadata.resource_version;
            step.wrote += 1;
        }
        if plan.delete {
            *lease = self.store.renew_lease(lease, ttl)?;
            match self.store.delete_fenced(
                conversation::KIND,
                name,
                &obj.metadata.uid,
                rv,
                by,
                lease,
            ) {
                Ok(_) => step.wrote += 1,
                Err(StoreError::Conflict { .. }) => step.conflict = true,
                Err(e) => return Err(e),
            }
        }
        Ok(step)
    }

    fn perform_create(
        &self,
        create: &Create,
        lease: &LeaseToken,
    ) -> (Observation<EffectOutcome>, Option<ThreadDir>) {
        let obj = &create.object;
        if let Some(exclusive) = &create.exclusive {
            let wanted = fleet_api::task::normalise_dir(&exclusive.workspace);
            let tasks = match self.store.list(task::KIND) {
                Observation::Known(t) => t,
                Observation::Unknown { why } => {
                    return (Observation::unknown(format!("list Task: {why}")), None);
                }
            };
            if let Some(holder) = tasks.iter().find(|t| {
                fleet_api::task::spec_dir(&t.spec)
                    .is_some_and(|d| fleet_api::task::normalise_dir(&d) == wanted)
            }) {
                let busy = EffectOutcome::WorkspaceBusy {
                    task: holder.metadata.name.clone(),
                };
                return (Observation::Known(busy), None);
            }
        }
        let mut made = None;
        if let Some(dir) = create.exclusive.as_ref().map(|e| e.workspace.as_str()) {
            let host = match self.task_host(obj) {
                Observation::Known(Ok(h)) => h,
                Observation::Known(Err(why)) => {
                    return (Observation::Known(EffectOutcome::Failed { why }), None);
                }
                Observation::Unknown { why } => return (Observation::unknown(why), None),
            };
            match self.runtime.create_dir(&host, dir) {
                Observation::Known(Ok(true)) => {
                    made = Some(ThreadDir {
                        path: dir.to_string(),
                        host,
                    });
                }
                Observation::Known(Ok(false)) => {}
                Observation::Known(Err(why)) => {
                    let failed = EffectOutcome::Failed {
                        why: format!("the thread's directory: {why}"),
                    };
                    return (Observation::Known(failed), None);
                }
                Observation::Unknown { why } => {
                    let why = format!("the thread's directory: {why}");
                    return (Observation::unknown(why), None);
                }
            }
        }
        (self.create_task(obj, lease), made)
    }

    fn create_task(&self, obj: &Object, lease: &LeaseToken) -> Observation<EffectOutcome> {
        let by = ConversationController::MANAGER;
        match self.store.create_fenced(obj.clone(), by, lease) {
            Ok(o) => Observation::Known(EffectOutcome::Created {
                uid: o.metadata.uid,
                adopted: false,
            }),
            Err(StoreError::AlreadyExists(_)) => {
                match self.store.get(&obj.kind, &obj.metadata.name) {
                    Observation::Known(Some(existing))
                        if existing.metadata.labels.get(CONVERSATION_UID_LABEL)
                            == obj.metadata.labels.get(CONVERSATION_UID_LABEL) =>
                    {
                        Observation::Known(EffectOutcome::Created {
                            uid: existing.metadata.uid,
                            adopted: true,
                        })
                    }
                    Observation::Known(Some(existing)) => {
                        Observation::Known(EffectOutcome::Failed {
                            why: format!(
                                "{} exists and was not created for this thread",
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

    fn task_host(&self, obj: &Object) -> Observation<Result<String, String>> {
        if let Some(h) = obj.spec["host"].as_str() {
            return Observation::Known(Ok(h.to_string()));
        }
        let Some(name) = obj.spec["configRef"]["name"].as_str() else {
            return Observation::Known(Ok(task::LOCAL_HOST.to_string()));
        };
        match self.store.get(fleet_api::config::TASK_CONFIG, name) {
            Observation::Known(Some(cfg)) if !cfg.is_deleting() => {
                let host = cfg.spec["host"].as_str().unwrap_or(task::LOCAL_HOST);
                Observation::Known(Ok(host.to_string()))
            }
            Observation::Known(_) => Observation::Known(Err(format!(
                "TaskConfig/{name} is absent or deleting, so the host for the thread's directory is not known"
            ))),
            Observation::Unknown { why } => {
                Observation::unknown(format!("get TaskConfig/{name}: {why}"))
            }
        }
    }

    fn retire_task(
        &self,
        name: &str,
        uid: &str,
        conversation_uid: &str,
        lease: &LeaseToken,
    ) -> Observation<EffectOutcome> {
        let by = ConversationController::MANAGER;
        match self
            .store
            .retire_conversation_task(name, uid, conversation_uid, by, lease)
        {
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

    fn remove_thread_dir(
        &self,
        conv: &Object,
        host: &str,
        path: &str,
    ) -> Observation<EffectOutcome> {
        let recorded = conv.status["dir"]["path"].as_str() == Some(path)
            && conv.status["dir"]["host"].as_str() == Some(host);
        if !recorded {
            return Observation::Known(EffectOutcome::Failed {
                why: format!("{path} is not the directory this thread recorded making"),
            });
        }
        let wanted = fleet_api::task::normalise_dir(path);
        let tasks = match self.store.list(task::KIND) {
            Observation::Known(t) => t,
            Observation::Unknown { why } => {
                return Observation::unknown(format!("list Task: {why}"));
            }
        };
        if let Some(holder) = tasks.iter().find(|t| {
            fleet_api::task::spec_dir(&t.spec)
                .is_some_and(|d| fleet_api::task::normalise_dir(&d) == wanted)
        }) {
            return Observation::Known(EffectOutcome::WorkspaceBusy {
                task: holder.metadata.name.clone(),
            });
        }
        match self.runtime.remove_dir(host, path) {
            Observation::Known(Ok(())) => Observation::Known(EffectOutcome::Deleted),
            Observation::Known(Err(why)) => Observation::Known(EffectOutcome::Failed { why }),
            Observation::Unknown { why } => Observation::Unknown { why },
        }
    }

    fn write_conversation_receipt(
        &self,
        current: &Object,
        key: &str,
        action: &str,
        outcome: Observation<EffectOutcome>,
        dir: Option<&ThreadDir>,
        lease: &LeaseToken,
    ) -> Result<Object, StoreError> {
        let receipt = Receipt {
            action: action.to_string(),
            at: self.clock.now(),
            outcome,
        };
        let mut obj = current.clone();
        for _ in 0..16 {
            let mut st: ConversationStatus = if obj.status.is_null() {
                ConversationStatus::default()
            } else {
                serde_json::from_value(obj.status.clone()).map_err(|e| {
                    StoreError::Invalid(format!("{}: Conversation status: {e}", obj.key()))
                })?
            };
            let Some(record) = st.effects.get_mut(key) else {
                return Err(StoreError::Invalid(format!(
                    "{}: no effect record holds the intent {key}",
                    obj.key()
                )));
            };
            record.receipt = Some(receipt.clone());
            if let Some(dir) = dir {
                st.dir = Some(dir.clone());
            }
            let value =
                serde_json::to_value(&st).map_err(|e| StoreError::Invalid(e.to_string()))?;
            let rv = obj.metadata.resource_version;
            match self.store.record_received(
                &obj.metadata.name,
                value,
                rv,
                ConversationController::MANAGER,
                lease,
            ) {
                Ok(o) => return Ok(o),
                Err(StoreError::Conflict { .. }) => {
                    obj = match self.store.get(conversation::KIND, &obj.metadata.name) {
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

    fn sweep_listeners(
        &self,
        lease: &mut LeaseToken,
        ttl: Millis,
        report: &mut RunReport,
        cache: &mut GatewayCache,
    ) -> Result<bool, StoreError> {
        let objects = match self.store.list(listener::KIND) {
            Observation::Known(l) => l,
            Observation::Unknown { why } => {
                report.unknowns.push(format!("list Listener: {why}"));
                return Ok(false);
            }
        };
        let now = self.clock.now();
        let mut busy = false;
        for obj in objects.iter().filter(|o| ListenerController::due(o, now)) {
            *lease = self.store.renew_lease(lease, ttl)?;
            let step = telemetry::reconcile(
                "ListenerController",
                obj,
                || self.clock.now(),
                || self.reconcile_listener(obj, lease, ttl, report, cache),
            );
            busy |= self.tally(step, &obj.key(), report)?;
        }
        Ok(busy)
    }

    fn reconcile_listener(
        &self,
        obj: &Object,
        lease: &mut LeaseToken,
        ttl: Millis,
        report: &mut RunReport,
        cache: &mut GatewayCache,
    ) -> Result<Step, StoreError> {
        let Some(gateway) = &self.gateway else {
            return Err(StoreError::Invalid(
                "no [gateway] section is configured; a Listener cannot be reconciled".into(),
            ));
        };
        let spec: ListenerSpec = fleet_api::validate::typed(listener::KIND, &obj.spec)?;
        let account = gateway
            .config
            .accounts
            .get(&spec.account)
            .map(|config| ListenerAccount {
                config: config.clone(),
                digest: gateway
                    .digests
                    .get(&spec.account)
                    .cloned()
                    .unwrap_or_default(),
                routed: gateway.config.is_routed(&spec.account),
            });
        let mut input = ListenerInput {
            object: obj,
            observations: ListenerObservations::default(),
            now: self.clock.now(),
            defaults: gateway.config.defaults().for_account(account),
        };
        let ctl = ListenerController;
        for _ in 0..4 {
            let probes = ctl.probes(&input)?;
            if probes.is_empty() {
                break;
            }
            for probe in probes {
                match probe {
                    Probe::Capabilities { account } => {
                        let answer = self.capabilities(gateway, &account, lease, ttl, cache)?;
                        if let Some(why) = answer.why() {
                            report
                                .unknowns
                                .push(format!("{} capabilities {account}: {why}", obj.key()));
                        }
                        input.observations.capabilities = Some(answer);
                    }
                    Probe::GatewayWhoami { account } => {
                        let answer = match cache.whoami.get(&account) {
                            Some(a) => a.clone(),
                            None => {
                                *lease = self.store.renew_lease(lease, ttl)?;
                                let a = gateway.port.whoami(&account);
                                cache.whoami.insert(account.clone(), a.clone());
                                a
                            }
                        };
                        if let Some(why) = answer.why() {
                            report
                                .unknowns
                                .push(format!("{} whoami {account}: {why}", obj.key()));
                        }
                        input.observations.whoami = Some(answer);
                    }
                    _ => {}
                }
            }
        }
        let plan = ctl.reconcile(&input)?;
        let by = ListenerController::MANAGER;
        let name = &obj.metadata.name;
        let mut step = Step::default();
        let mut rv = obj.metadata.resource_version;
        if let Some(status) = plan.status {
            match self.store.update_status(
                listener::KIND,
                name,
                status,
                rv,
                by.clone(),
                Some(&*lease),
            ) {
                Ok(o) => {
                    rv = o.metadata.resource_version;
                    step.wrote += 1;
                }
                Err(StoreError::Conflict { .. }) => {
                    step.conflict = true;
                    return Ok(step);
                }
                Err(e) => return Err(e),
            }
        }
        if plan.delete {
            *lease = self.store.renew_lease(lease, ttl)?;
            match self
                .store
                .delete_fenced(listener::KIND, name, &obj.metadata.uid, rv, by, lease)
            {
                Ok(_) => step.wrote += 1,
                Err(StoreError::Conflict { .. }) => step.conflict = true,
                Err(e) => return Err(e),
            }
        }
        Ok(step)
    }

    fn capabilities(
        &self,
        gateway: &GatewaySide<'_>,
        account: &str,
        lease: &mut LeaseToken,
        ttl: Millis,
        cache: &mut GatewayCache,
    ) -> Result<Observation<Capabilities>, StoreError> {
        if let Some(a) = cache.capabilities.get(account) {
            return Ok(a.clone());
        }
        *lease = self.store.renew_lease(lease, ttl)?;
        let a = gateway.port.capabilities(account);
        cache.capabilities.insert(account.to_string(), a.clone());
        Ok(a)
    }

    fn reconcile_forge(
        &self,
        obj: &Object,
        lease: &mut LeaseToken,
        ttl: Millis,
        report: &mut RunReport,
        logins: &mut ForgeLogins,
        workers: &BTreeMap<String, String>,
    ) -> Result<Step, StoreError> {
        let ctl = ForgeController;
        let mut input = ForgeInput {
            task: obj,
            observations: ForgeObservations {
                workers: workers.clone(),
                ..ForgeObservations::default()
            },
            now: self.clock.now(),
            defaults: self.forge_defaults.clone(),
        };
        for _ in 0..4 {
            let probes = ctl.probes(&input)?;
            if probes.is_empty() {
                break;
            }
            for probe in probes {
                match probe {
                    Probe::Object { kind, name } => {
                        let o = self.store.get(&kind, &name);
                        if let Some(why) = o.why() {
                            report
                                .unknowns
                                .push(format!("{} object {kind}/{name}: {why}", obj.key()));
                        }
                        input.observations.changes.insert(name, o);
                    }
                    Probe::Whoami { host } => {
                        let answer = match logins.get(&host) {
                            Some(a) => a,
                            None => {
                                let Some(forge) = self.forge else {
                                    return Err(StoreError::Invalid(
                                        "no [forge] section is configured; a declared change cannot be followed"
                                            .into(),
                                    ));
                                };
                                *lease = self.store.renew_lease(lease, ttl)?;
                                let a = forge.whoami(&host);
                                logins.insert(host.clone(), self.clock.now(), a.clone());
                                a
                            }
                        };
                        if let Some(why) = answer.why() {
                            report
                                .unknowns
                                .push(format!("{} whoami {host}: {why}", obj.key()));
                        }
                        input.observations.whoami.insert(host, answer);
                    }
                    Probe::AtRest { session } => {
                        *lease = self.store.renew_lease(lease, ttl)?;
                        let o = self.runtime.at_rest(&session);
                        if let Some(why) = o.why() {
                            report.unknowns.push(format!("{} atRest: {why}", obj.key()));
                        }
                        input.observations.at_rest = Some(o);
                    }
                    _ => {}
                }
            }
        }
        let plan = ctl.reconcile(&input)?;
        let mut step = Step::default();
        if plan.status.is_none() && plan.appends.is_empty() {
            return Ok(step);
        }
        let forge = plan
            .status
            .unwrap_or_else(|| obj.status.get("forge").cloned().unwrap_or(Value::Null));
        let mut current = match self.store.record_relay(
            &obj.metadata.name,
            forge,
            plan.appends,
            obj.metadata.resource_version,
            ForgeController::MANAGER,
            lease,
        ) {
            Ok(o) => {
                step.wrote += 1;
                o
            }
            Err(StoreError::Conflict { .. }) | Err(StoreError::AlreadyExists(_)) => {
                step.conflict = true;
                return Ok(step);
            }
            Err(e) => return Err(e),
        };
        for effect in &plan.effects {
            let Action::MarkReady { change, expect } = &effect.action else {
                continue;
            };
            let Some(forge) = self.forge else {
                return Err(StoreError::Invalid(
                    "no [forge] section is configured; a change cannot be marked ready".into(),
                ));
            };
            *lease = self.store.renew_lease(lease, ttl)?;
            let outcome = telemetry::effect(obj, effect, || {
                forge.mark_ready(change, expect, &effect.key)
            });
            if let Observation::Unknown { why } = &outcome {
                report
                    .unknowns
                    .push(format!("{} {}: {why}", obj.key(), effect.action.name()));
            }
            step.effects += 1;
            current = self.write_forge_receipt(&current, effect, outcome, lease)?;
            step.wrote += 1;
        }
        Ok(step)
    }

    fn reconcile_gateway(
        &self,
        obj: &Object,
        lease: &mut LeaseToken,
        ttl: Millis,
        report: &mut RunReport,
        tasks: &[Object],
    ) -> Result<Step, StoreError> {
        let ctl = GatewayController;
        let mut cache = GatewayCache::default();
        let mut input = GatewayInput {
            task: obj,
            observations: GatewayObservations::default(),
            now: self.clock.now(),
            defaults: self
                .gateway
                .as_ref()
                .map(|g| g.config.defaults())
                .unwrap_or_default(),
        };
        for _ in 0..4 {
            let probes = ctl.probes(&input)?;
            if probes.is_empty() {
                break;
            }
            for probe in probes {
                match probe {
                    Probe::Object { kind, name } if kind == conversation::KIND => {
                        let o = self.store.get(&kind, &name);
                        if let Some(why) = o.why() {
                            report
                                .unknowns
                                .push(format!("{} object {kind}/{name}: {why}", obj.key()));
                        }
                        input.observations.conversation = Some(o);
                    }
                    Probe::Object { kind, name } => {
                        let o = self.store.get(&kind, &name);
                        if let Some(why) = o.why() {
                            report
                                .unknowns
                                .push(format!("{} object {kind}/{name}: {why}", obj.key()));
                        }
                        if kind == task::KIND {
                            input.observations.parent = Some(o);
                        } else {
                            input.observations.changes.insert(name, o);
                        }
                    }
                    Probe::Capabilities { account } => {
                        let answer = match &self.gateway {
                            Some(gateway) => {
                                self.capabilities(gateway, &account, lease, ttl, &mut cache)?
                            }
                            None => Observation::unknown("no [gateway] section is configured"),
                        };
                        if let Some(why) = answer.why() {
                            report
                                .unknowns
                                .push(format!("{} capabilities {account}: {why}", obj.key()));
                        }
                        input.observations.capabilities.insert(account, answer);
                    }
                    Probe::History {
                        account,
                        conversation,
                        query,
                    } => {
                        let logs = input
                            .defaults
                            .logs
                            .as_ref()
                            .is_some_and(|l| l.account == account && l.channel == conversation);
                        let answer = match &self.gateway {
                            Some(gateway) => {
                                *lease = self.store.renew_lease(lease, ttl)?;
                                gateway.port.history(&account, &conversation, &query)
                            }
                            None => HistoryObservation::unknown(
                                fleet_api::gateway::Uncertainty::Refused,
                                "no [gateway] section is configured",
                            ),
                        };
                        if let HistoryObservation::Unknown { why, .. } = &answer {
                            report
                                .unknowns
                                .push(format!("{} history: {why}", obj.key()));
                        }
                        if logs {
                            input.observations.log_history = Some(answer);
                        } else {
                            input.observations.history = Some(answer);
                        }
                    }
                    Probe::List { kind, labels } => {
                        let selected = |list: &[Object]| -> Vec<Object> {
                            list.iter()
                                .filter(|o| {
                                    labels
                                        .iter()
                                        .all(|(k, v)| o.metadata.labels.get(k) == Some(v))
                                })
                                .cloned()
                                .collect()
                        };
                        let answer = if kind == task::KIND {
                            Observation::Known(selected(tasks))
                        } else {
                            match self.store.list(&kind) {
                                Observation::Known(list) => Observation::Known(selected(&list)),
                                Observation::Unknown { why } => {
                                    report
                                        .unknowns
                                        .push(format!("{} list {kind}: {why}", obj.key()));
                                    Observation::Unknown { why }
                                }
                            }
                        };
                        input.observations.children = Some(answer);
                    }
                    _ => {}
                }
            }
        }
        let plan = ctl.reconcile(&input)?;
        let mut step = Step::default();
        if plan.status.is_none() && plan.appends.is_empty() {
            return Ok(step);
        }
        let mut current;
        let gateway = plan
            .status
            .unwrap_or_else(|| obj.status.get("gateway").cloned().unwrap_or(Value::Null));
        *lease = self.store.renew_lease(lease, ttl)?;
        match self.store.record_relay(
            &obj.metadata.name,
            gateway,
            plan.appends,
            obj.metadata.resource_version,
            GatewayController::MANAGER,
            lease,
        ) {
            Ok(o) => {
                current = o;
                step.wrote += 1;
            }
            Err(StoreError::Conflict { .. }) | Err(StoreError::AlreadyExists(_)) => {
                step.conflict = true;
                return Ok(step);
            }
            Err(e) => return Err(e),
        }
        for effect in &plan.effects {
            *lease = self.store.renew_lease(lease, ttl)?;
            let outcome = self.perform_gateway(effect, lease);
            if let Observation::Unknown { why } = &outcome {
                report
                    .unknowns
                    .push(format!("{} {}: {why}", obj.key(), effect.action.name()));
            }
            step.effects += 1;
            current = self.write_gateway_receipt(&current, effect, outcome, lease)?;
            step.wrote += 1;
        }
        Ok(step)
    }

    fn perform_gateway(&self, effect: &Effect, lease: &LeaseToken) -> Observation<EffectOutcome> {
        let Some(gateway) = &self.gateway else {
            return Observation::Known(EffectOutcome::Failed {
                why: "no [gateway] section is configured".into(),
            });
        };
        match &effect.action {
            Action::Post {
                account,
                conversation: thread,
                text,
            } => gateway.port.post(account, thread, text, &effect.key),
            Action::Open {
                account,
                channel,
                text,
                ..
            } => gateway.port.open(account, channel, text, &effect.key),
            Action::Log {
                account,
                channel,
                text,
                thread,
            } => {
                let text = match thread {
                    Some(t) => {
                        let at = gateway
                            .port
                            .link(&t.account, &t.conversation)
                            .unwrap_or_else(|| t.conversation.address());
                        format!("{text}\nThread: {at}")
                    }
                    None => text.clone(),
                };
                gateway.port.post(account, channel, &text, &effect.key)
            }
            Action::React {
                account,
                conversation,
                message,
                reaction,
                on,
            } => gateway
                .port
                .react(account, conversation, message, reaction, *on),
            Action::Receive {
                account,
                route,
                delivery,
            } => verbs::receive_opened(
                self.store,
                account,
                route,
                delivery.clone(),
                GatewayController::MANAGER,
                lease,
            ),
            other => Observation::Known(EffectOutcome::Failed {
                why: format!("{} is not a Gateway controller effect", other.name()),
            }),
        }
    }

    fn write_gateway_receipt(
        &self,
        current: &Object,
        effect: &Effect,
        outcome: Observation<EffectOutcome>,
        lease: &LeaseToken,
    ) -> Result<Object, StoreError> {
        let receipt = Receipt {
            action: effect.action.name().to_string(),
            at: self.clock.now(),
            outcome,
        };
        let mut obj = current.clone();
        for _ in 0..16 {
            let mut gw: GatewayStatus = match obj.status.get("gateway") {
                Some(v) if !v.is_null() => serde_json::from_value(v.clone()).map_err(|e| {
                    StoreError::Invalid(format!("{}: status.gateway: {e}", obj.key()))
                })?,
                _ => GatewayStatus::default(),
            };
            let Some(record) = gw.posts.get_mut(&effect.key) else {
                return Err(StoreError::Invalid(format!(
                    "{}: no post record holds the intent {}",
                    obj.key(),
                    effect.key
                )));
            };
            record.receipt = Some(receipt.clone());
            let value =
                serde_json::to_value(&gw).map_err(|e| StoreError::Invalid(e.to_string()))?;
            match self.store.record_relay(
                &obj.metadata.name,
                value,
                Vec::new(),
                obj.metadata.resource_version,
                GatewayController::MANAGER,
                lease,
            ) {
                Ok(o) => return Ok(o),
                Err(StoreError::Conflict { .. }) => {
                    obj = match self.store.get(task::KIND, &obj.metadata.name) {
                        Observation::Known(Some(o)) => o,
                        Observation::Known(None) => return Err(StoreError::NotFound(obj.key())),
                        Observation::Unknown { why } => return Err(StoreError::Unknown(why)),
                    };
                }
                Err(e) => return Err(e),
            }
        }
        Err(StoreError::Busy(format!(
            "{}: receipt for {} kept conflicting",
            obj.key(),
            effect.key
        )))
    }

    fn write_forge_receipt(
        &self,
        current: &Object,
        effect: &Effect,
        outcome: Observation<EffectOutcome>,
        lease: &LeaseToken,
    ) -> Result<Object, StoreError> {
        let receipt = Receipt {
            action: effect.action.name().to_string(),
            at: self.clock.now(),
            outcome,
        };
        let mut obj = current.clone();
        for _ in 0..16 {
            let mut forge: ForgeStatus = match obj.status.get("forge") {
                Some(v) if !v.is_null() => serde_json::from_value(v.clone()).map_err(|e| {
                    StoreError::Invalid(format!("{}: status.forge: {e}", obj.key()))
                })?,
                _ => ForgeStatus::default(),
            };
            let Some(entry) = forge
                .changes
                .values_mut()
                .find(|e| e.mark_ready.intents.contains_key(&effect.key))
            else {
                return Err(StoreError::Invalid(format!(
                    "{}: no entry holds the intent {}",
                    obj.key(),
                    effect.key
                )));
            };
            entry
                .mark_ready
                .receipts
                .insert(effect.key.clone(), receipt.clone());
            let value =
                serde_json::to_value(&forge).map_err(|e| StoreError::Invalid(e.to_string()))?;
            let rv = obj.metadata.resource_version;
            match self.store.record_relay(
                &obj.metadata.name,
                value,
                Vec::new(),
                rv,
                ForgeController::MANAGER,
                lease,
            ) {
                Ok(o) => return Ok(o),
                Err(StoreError::Conflict { .. }) => {
                    obj = match self.store.get(task::KIND, &obj.metadata.name) {
                        Observation::Known(Some(o)) => o,
                        Observation::Known(None) => return Err(StoreError::NotFound(obj.key())),
                        Observation::Unknown { why } => return Err(StoreError::Unknown(why)),
                    };
                }
                Err(e) => return Err(e),
            }
        }
        Err(StoreError::Busy(format!(
            "{}: receipt for {} kept conflicting",
            obj.key(),
            effect.key
        )))
    }

    fn due(&self, obj: &Object) -> bool {
        let waiting = obj
            .status
            .get("notBefore")
            .and_then(Value::as_u64)
            .is_some_and(|t| t > self.clock.now());
        if waiting {
            return false;
        }
        let concluded = obj.status.get("phase").and_then(Value::as_str) == Some("Concluded");
        let inbox_empty = obj
            .status
            .get("inbox")
            .and_then(Value::as_array)
            .is_none_or(Vec::is_empty)
            && obj.status.get("catchUp").is_none_or(Value::is_null);
        !(concluded
            && inbox_empty
            && !obj.is_deleting()
            && !fleet_controllers::task::due_at_rest(obj, self.clock.now()))
    }

    fn reconcile_while_starting(
        &self,
        obj: &Object,
        outbox_drained: &Observation<bool>,
        lease: &mut LeaseToken,
        ttl: Millis,
        report: &mut RunReport,
    ) -> Result<(bool, Option<WaitingOnAgent>), StoreError> {
        let mut current = obj.clone();
        let mut busy = false;
        for _ in 0..STARTUP_STEPS_IN_A_ROW {
            let before = self.waiting_on_agent(&current, outbox_drained.clone());
            let step = telemetry::reconcile(
                "TaskController",
                &current,
                || self.clock.now(),
                || self.reconcile_task(&current, outbox_drained.clone(), lease, ttl, report),
            );
            let stepped = self.tally(step, &obj.key(), report)?;
            busy |= stepped;
            if !starting(&current) {
                return Ok((busy, None));
            }
            if !stepped {
                return Ok((busy, before));
            }
            match self.store.get(task::KIND, &obj.metadata.name) {
                Observation::Known(Some(next)) if starting(&next) && self.due(&next) => {
                    current = next;
                }
                _ => return Ok((busy, None)),
            }
            *lease = self.store.renew_lease(lease, ttl)?;
        }
        Ok((busy, None))
    }

    fn waiting_on_agent(
        &self,
        obj: &Object,
        outbox_drained: Observation<bool>,
    ) -> Option<WaitingOnAgent> {
        let session: SessionRef =
            serde_json::from_value(obj.status.get("session")?.clone()).ok()?;
        let mark = self.signals?.written(&session)?;
        Some(WaitingOnAgent {
            name: obj.metadata.name.clone(),
            session,
            mark,
            outbox_drained,
        })
    }

    fn reconcile_new_starts(
        &self,
        new_starts: &mut NewStarts,
        waiting: &mut Vec<WaitingOnAgent>,
        lease: &mut LeaseToken,
        ttl: Millis,
        report: &mut RunReport,
    ) -> Result<bool, StoreError> {
        let now = self.clock.now();
        if now < new_starts.listed_at.saturating_add(RELIST_AFTER_MS) {
            return Ok(false);
        }
        new_starts.listed_at = now;
        let Observation::Known(tasks) = self.store.list(task::KIND) else {
            return Ok(false);
        };
        let mut busy = false;
        for obj in tasks {
            if !new_starts.seen.insert(generation_of(&obj)) || !starting(&obj) || !self.due(&obj) {
                continue;
            }
            new_starts.started_here.insert(obj.metadata.uid.clone());
            *lease = self.store.renew_lease(lease, ttl)?;
            let unread = Observation::unknown("the outbox is read on the next pass");
            let (stepped, rest) =
                self.reconcile_while_starting(&obj, &unread, lease, ttl, report)?;
            busy |= stepped;
            waiting.extend(rest);
        }
        Ok(busy)
    }

    fn reconcile_reported_starts(
        &self,
        waiting: &mut Vec<WaitingOnAgent>,
        lease: &mut LeaseToken,
        ttl: Millis,
        report: &mut RunReport,
    ) -> Result<bool, StoreError> {
        let mut busy = false;
        let mut still = Vec::new();
        for w in std::mem::take(waiting) {
            if !self.reported_since(&w.session, w.mark) {
                still.push(w);
                continue;
            }
            let Observation::Known(Some(obj)) = self.store.get(task::KIND, &w.name) else {
                continue;
            };
            if !starting(&obj) || !self.due(&obj) {
                continue;
            }
            *lease = self.store.renew_lease(lease, ttl)?;
            let (stepped, rest) =
                self.reconcile_while_starting(&obj, &w.outbox_drained, lease, ttl, report)?;
            busy |= stepped;
            still.extend(rest);
        }
        *waiting = still;
        Ok(busy)
    }

    fn reported_since(&self, session: &SessionRef, mark: u64) -> bool {
        self.signals
            .and_then(|s| s.written(session))
            .is_some_and(|now| now != mark)
    }

    pub fn reported(&self, report: &RunReport) -> bool {
        report
            .waiting
            .iter()
            .any(|(session, mark)| self.reported_since(session, *mark))
    }

    fn tally(
        &self,
        step: Result<Step, StoreError>,
        key: &str,
        report: &mut RunReport,
    ) -> Result<bool, StoreError> {
        match step {
            Ok(step) => {
                report.status_writes += step.wrote;
                report.effects += step.effects;
                report.conflicts += u32::from(step.conflict);
                Ok(step.wrote > 0 || step.effects > 0 || step.conflict)
            }
            Err(e @ (StoreError::LeaseLost(_) | StoreError::Full(_))) => Err(e),
            Err(e) => {
                report.errors.push(format!("{key}: {e}"));
                Ok(false)
            }
        }
    }

    fn launch_saved(&self, obj: &Object) -> Result<bool, StoreError> {
        match self.store.launch(obj) {
            Ok(launch) => Ok(launch.is_some()),
            Err(StoreError::Unsupported(_)) => Ok(false),
            Err(e) => Err(e),
        }
    }

    fn reconcile_task(
        &self,
        obj: &Object,
        outbox_drained: Observation<bool>,
        lease: &mut LeaseToken,
        ttl: Millis,
        report: &mut RunReport,
    ) -> Result<Step, StoreError> {
        let spec: task::TaskSpec = fleet_api::validate::typed(task::KIND, &obj.spec)?;
        let st: task::TaskStatus = if obj.status.is_null() {
            task::TaskStatus::default()
        } else {
            fleet_api::validate::typed(task::KIND, &obj.status)?
        };
        let needs_launch = spec.config_ref.is_some()
            || !spec.env.is_empty()
            || obj.spec.get("harness").is_none()
            || st.execution.is_some()
            || (!obj.is_deleting() && spec.dispatch && self.launch_saved(obj)?)
            || (st.session.is_none()
                && st.intents.is_empty()
                && !obj.is_deleting()
                && spec.dispatch
                && execution::default_config(self.store)?);
        let mut effective = None;
        if needs_launch && !obj.is_deleting() && spec.dispatch {
            let snapshot = match self.store.launch(obj)? {
                Some(e) => e,
                None if st.execution.is_some()
                    || !st.intents.is_empty()
                    || st.session.is_some() =>
                {
                    return Err(StoreError::Unknown(
                        "private launch missing; refusing to re-resolve an existing execution"
                            .into(),
                    ));
                }
                None => {
                    *lease = self.store.renew_lease(lease, ttl)?;
                    let e = execution::resolve(self.store, self.runtime, obj, lease, ttl)?;
                    self.store.freeze_launch(obj, &e, lease)?
                }
            };
            if st.execution.is_none() {
                let mut status = st;
                status.execution = Some(snapshot.identity());
                let plan = Plan {
                    status: Some(serde_json::to_value(status).map_err(|_| {
                        StoreError::Invalid("execution status encoding failed".into())
                    })?),
                    ..Plan::default()
                };
                return self.perform_plan(obj, plan, lease, ttl, report);
            }
            if st.execution.as_ref() != Some(&snapshot.identity()) {
                return Err(StoreError::Unknown(
                    "private launch identity mismatch".into(),
                ));
            }
            effective = Some(snapshot);
        }
        let mut input = TaskInput {
            task: obj,
            observations: Observations {
                outbox_drained: Some(outbox_drained),
                ..Observations::default()
            },
            now: self.clock.now(),
        };
        let ctl = TaskController;
        for probe in ctl.probes(&input)? {
            *lease = self.store.renew_lease(lease, ttl)?;
            self.observe(&probe, obj, &mut input.observations, report);
        }
        let mut plan = ctl.reconcile(&input)?;
        let telemetry_on = effective
            .as_ref()
            .and_then(|e| e.agent_telemetry)
            .or(spec.agent_telemetry)
            .unwrap_or(true);
        if let Some(execution) = effective {
            for effect in &mut plan.effects {
                let first = matches!(effect.action, Action::Spawn(_));
                if let Some(spawn) = effect.action.launch_mut() {
                    spawn.harness = execution.harness.clone();
                    if first {
                        spawn.host = spawn
                            .ready
                            .as_ref()
                            .map_or_else(|| execution.host.clone(), |w| w.host.clone());
                    }
                    spawn.model = execution.model.clone();
                    spawn.environment_frozen = true;
                    spawn.env = execution.env.clone();
                }
            }
        }
        let profiles = fleet_api::harness::installed();
        for effect in &mut plan.effects {
            let Some(spawn) = effect.action.launch_mut() else {
                continue;
            };
            if !spawn.environment_frozen {
                let mut env = profiles.get(&spawn.harness).launch.env;
                fleet_api::config::merge_env(&mut env, &spawn.env);
                spawn.env = env;
            }
            if let Some(collector) = &self.agent_telemetry
                && telemetry_on
                && !fleet_api::harness::names_collector(&spawn.env)
            {
                let t = profiles.get(&spawn.harness).telemetry(
                    collector,
                    &obj.metadata.name,
                    &obj.metadata.uid,
                );
                let mut env = t.env;
                fleet_api::config::merge_env(&mut env, &spawn.env);
                spawn.env = env;
                spawn.telemetry_args = t.args;
            }
            if let Some(file) = self.signals.and_then(|s| s.file(&spawn.host, &spawn.key)) {
                spawn.env.insert(
                    fleet_api::event::EVENT_FILE_ENV.to_string(),
                    fleet_api::event::outbox_file(&file),
                );
                spawn
                    .env
                    .insert(fleet_api::signal::SIGNAL_FILE_ENV.to_string(), file);
                spawn.env.insert(
                    fleet_api::harness::HARNESS_ENV.to_string(),
                    spawn.harness.clone(),
                );
                if let Some(dir) = profiles.dir() {
                    spawn.env.insert(
                        fleet_api::harness::HARNESS_DIR_ENV.to_string(),
                        dir.to_string(),
                    );
                }
                if let Some(dir) = profiles.assets() {
                    spawn.env.insert(
                        fleet_api::harness::ASSET_DIR_ENV.to_string(),
                        dir.to_string(),
                    );
                }
            }
        }
        self.perform_plan(obj, plan, lease, ttl, report)
    }

    fn perform_plan(
        &self,
        obj: &Object,
        plan: Plan,
        lease: &mut LeaseToken,
        ttl: Millis,
        report: &mut RunReport,
    ) -> Result<Step, StoreError> {
        let by = TaskController::MANAGER;
        let mut step = Step::default();
        let mut current = obj.clone();
        let status = match (plan.status, &plan.delivery) {
            (Some(status), _) => Some(status),
            (None, Some(_)) => Some(current.status.clone()),
            (None, None) => None,
        };
        if let Some(status) = status {
            let rv = current.metadata.resource_version;
            let written = match plan.delivery {
                Some(receipt) => self.store.record_delivery(
                    &obj.metadata.name,
                    status,
                    receipt,
                    rv,
                    by.clone(),
                    lease,
                ),
                None => self.store.update_status(
                    task::KIND,
                    &obj.metadata.name,
                    status,
                    rv,
                    by.clone(),
                    Some(&*lease),
                ),
            };
            match written {
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
            match self.publish_fleet_events(&current, lease, ttl, report) {
                Ok(Some(o)) => {
                    current = o;
                    step.wrote += 1;
                }
                Ok(None) => {}
                Err(e @ StoreError::LeaseLost(_)) => return Err(e),
                Err(e) => report
                    .errors
                    .push(format!("{} fleet events: {e}", obj.key())),
            }
        }
        for effect in &plan.effects {
            *lease = self.store.renew_lease(lease, ttl)?;
            let outcome = telemetry::effect(obj, effect, || self.perform(effect));
            if let Observation::Unknown { why } = &outcome {
                report
                    .unknowns
                    .push(format!("{} {}: {why}", obj.key(), effect.action.name()));
            }
            step.effects += 1;
            current = self.write_receipt(&current, effect, outcome, lease)?;
            step.wrote += 1;
        }
        if let Some(finalizer) = plan.remove_finalizer {
            let rv = current.metadata.resource_version;
            self.store.remove_finalizer(
                task::KIND,
                &obj.metadata.name,
                &finalizer,
                rv,
                by.clone(),
                lease,
            )?;
            step.wrote += 1;
        }
        Ok(step)
    }

    fn observe(
        &self,
        probe: &Probe,
        task: &Object,
        obs: &mut Observations,
        report: &mut RunReport,
    ) {
        let rt = self.runtime;
        let key = task.key();
        let mut note = |what: &str, why: Option<&str>| {
            if let Some(why) = why {
                report.unknowns.push(format!("{key} {what}: {why}"));
            }
        };
        match probe {
            Probe::Host { host } => {
                let o = rt.host(host);
                note(&format!("host {host}"), o.why());
                obs.host = Some(o);
            }
            Probe::Find { host, key: k } => {
                let o = rt.find(host, k);
                note(&format!("find on {host}"), o.why());
                obs.find = Some(o);
            }
            Probe::Ready { session } => {
                let o = rt.ready(session);
                note("ready", o.why());
                obs.ready = Some(o);
            }
            Probe::WorkspaceTrust {
                session,
                workspace,
                ready,
            } => {
                let o = rt.workspace_trust(session, workspace, ready.as_ref());
                note("workspaceTrust", o.why());
                obs.workspace_trust = Some(o);
            }
            Probe::BriefDelivered { session, key: k } => {
                let o = rt.brief_delivered(session, k);
                note("briefDelivered", o.why());
                obs.brief_delivered = Some(o);
            }
            Probe::AtRest { session } => {
                let o = rt.at_rest(session);
                note("atRest", o.why());
                obs.at_rest = Some(o);
            }
            Probe::Lost { session } => {
                let o = rt.lost(session);
                note("lost", o.why());
                obs.lost = Some(o);
            }
            Probe::Paused { session } => {
                let o = rt.paused(session);
                note("paused", o.why());
                obs.paused = Some(o);
            }
            Probe::FindWorkspace { host, key: k, urls } => {
                let o = rt.find_workspace(host, k, urls);
                note(&format!("findWorkspace on {host}"), o.why());
                obs.workspace = Some(o);
            }
            Probe::Activity { session } => {
                let o = match (rt.signals(session), self.signals) {
                    (Some(o), _) => o,
                    (None, Some(s)) => s.read(session),
                    (None, None) => Observation::unknown("no signal port is configured"),
                };
                note("signals", o.why());
                obs.activity = Some(match &o {
                    Observation::Known(r) => match r.activity() {
                        Some(a) => Observation::Known(a),
                        None => Observation::unknown("the agent reported no state yet"),
                    },
                    Observation::Unknown { why } => Observation::unknown(why.clone()),
                });
                obs.signals = Some(o);
            }
            Probe::Screen { session } => obs.screen = Some(rt.screen(session)),
            Probe::Change { .. }
            | Probe::Object { .. }
            | Probe::List { .. }
            | Probe::Whoami { .. }
            | Probe::History { .. }
            | Probe::Capabilities { .. }
            | Probe::GatewayWhoami { .. } => {}
        }
    }

    fn perform(&self, effect: &Effect) -> Observation<EffectOutcome> {
        match &effect.action {
            Action::Materialize { host, key, urls } => self.runtime.workspace(host, key, urls),
            Action::CleanWorkspace { workspace } => self.runtime.release_workspace(workspace),
            Action::Spawn(spec) => self.runtime.spawn(spec),
            Action::Brief {
                session,
                text,
                inbox: None,
            } => self.runtime.brief(session, text, &effect.key),
            Action::Brief {
                session,
                text,
                inbox: Some(argv),
            } => self.runtime.deliver(session, text, &effect.key, argv),
            Action::Release { session } => self.runtime.release(session),
            Action::Revive {
                session,
                conversation,
                launch,
            } => self
                .runtime
                .revive(session, conversation.as_deref(), launch),
            Action::Pause { session } => self.runtime.pause(session),
            Action::Resume {
                session,
                conversation,
                launch,
            } => self
                .runtime
                .resume(session, conversation.as_deref(), launch),
            Action::Restart {
                session,
                conversation,
                launch,
            } => self
                .runtime
                .restart(session, conversation.as_deref(), launch),
            Action::MarkReady { .. }
            | Action::Post { .. }
            | Action::Open { .. }
            | Action::Receive { .. }
            | Action::Log { .. }
            | Action::React { .. }
            | Action::DeleteTask { .. }
            | Action::RemoveDir { .. }
            | Action::RetireScheduled { .. } => Observation::Known(EffectOutcome::Failed {
                why: format!("{} is not a Task controller effect", effect.action.name()),
            }),
        }
    }

    fn write_receipt(
        &self,
        current: &Object,
        effect: &Effect,
        outcome: Observation<EffectOutcome>,
        lease: &LeaseToken,
    ) -> Result<Object, StoreError> {
        let receipt = Receipt {
            action: effect.action.name().to_string(),
            at: self.clock.now(),
            outcome,
        };
        let receipt =
            serde_json::to_value(&receipt).map_err(|e| StoreError::Invalid(e.to_string()))?;
        let mut obj = current.clone();
        for _ in 0..16 {
            let mut status = if obj.status.is_object() {
                obj.status.clone()
            } else {
                Value::Object(serde_json::Map::new())
            };
            let receipts = status.as_object_mut().map(|m| {
                m.entry("receipts")
                    .or_insert_with(|| Value::Object(serde_json::Map::new()))
            });
            match receipts.and_then(Value::as_object_mut) {
                Some(r) => {
                    r.insert(effect.key.clone(), receipt.clone());
                }
                None => {
                    return Err(StoreError::Invalid(format!(
                        "{}: status.receipts is not a map",
                        obj.key()
                    )));
                }
            }
            let rv = obj.metadata.resource_version;
            match self.store.update_status(
                task::KIND,
                &obj.metadata.name,
                status,
                rv,
                TaskController::MANAGER,
                Some(lease),
            ) {
                Ok(o) => return Ok(o),
                Err(StoreError::Conflict { .. }) => {
                    obj = match self.store.get(task::KIND, &obj.metadata.name) {
                        Observation::Known(Some(o)) => o,
                        Observation::Known(None) => return Err(StoreError::NotFound(obj.key())),
                        Observation::Unknown { why } => return Err(StoreError::Unknown(why)),
                    };
                }
                Err(e) => return Err(e),
            }
        }
        Err(StoreError::Busy(format!(
            "{}: receipt for {} kept conflicting",
            obj.key(),
            effect.key
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_known_login_is_kept_for_ten_minutes_and_an_unknown_one_for_the_pass() {
        let mut logins = ForgeLogins::default();
        logins.insert(
            "known.test".into(),
            0,
            Observation::Known("fleet-bot".into()),
        );
        logins.insert("unknown.test".into(), 0, Observation::unknown("timed out"));

        logins.forget_unknown_and_old(FORGE_LOGIN_KEPT_MS - 1);
        assert_eq!(
            logins.get("known.test"),
            Some(Observation::Known("fleet-bot".into()))
        );
        assert_eq!(logins.get("unknown.test"), None);

        logins.forget_unknown_and_old(FORGE_LOGIN_KEPT_MS);
        assert_eq!(logins.get("known.test"), None);
    }
}
