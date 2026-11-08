//! Fleet's manager: the only code that performs a controller's plan.
//!
//! It holds the one manager lease, walks the Tasks, asks TaskController what
//! to probe, probes through the runtime port, asks it to decide, then writes
//! in the order the contract requires: the status carrying each effect's
//! intent first (compare-and-swap, fenced by the lease), then the effect,
//! then its receipt. It also hosts the spec and worker verbs, so the CLI only
//! parses and prints.
//!
//! Each Task has a second controller, ForgeController, with its own due
//! gate: it runs after TaskController, from a fresh read of the Task, asks
//! for the ChangeRequests the worker declared (answered from the store),
//! Fleet's login on their hosts (through the Forge port, cached for the
//! sweep) and, while a mark-ready is being evaluated, whether the session
//! is at rest (through the runtime), and its plan is one typed write:
//! `status.forge` replaced and the relay's inbox entries appended together,
//! carrying any mark-ready intent. The one effect, `MarkReady`, is then
//! performed through the Forge port after a lease renewal, and its receipt
//! written onto the entry.
//!
//! A third controller, GatewayController, runs after the Forge one on each
//! Task it is due for: the Conversation its labels name is answered from
//! the store, its children from the sweep's one Task listing, and its plan
//! is the same typed write, `status.gateway` replaced and the relay's
//! entries appended together. Whether a tied Task is due is judged from
//! the sweep's Conversation listing, so a quiet tied Task costs nothing.
//!
//! The same sweep walks the ChangeRequests: each due one is observed through
//! the Forge port and decided by ChangeRequestController, whose plan is one
//! status write and, after retention, one fenced delete. There are no effects
//! and no receipts on that side.
//!
//! It then walks the Conversations and the Listeners of the `[gateway]`
//! section: a Conversation is answered from the store (its tied Task) and
//! the Gateway port (the account's capabilities, cached for the sweep, and
//! one history read), decided by ConversationController, and written
//! through `record_received`, which drains the spool with the status; a
//! Listener is answered from the port (capabilities and identity, cached)
//! and decided by ListenerController. Each retires its own object by a
//! fenced delete. A Conversation's plan may also create the Task its thread
//! starts, under a key whose intent the status carries: the manager checks
//! the create's `exclusive: {workspace}` precondition against the store's
//! Tasks under the lease, makes the Task's `dir` workspace on its host
//! through the runtime port, then performs the fenced create, and writes
//! the receipt back under the key; and it may post the thread's refusal or
//! busy notice through the Gateway port, intent before and receipt after.
//! The receive verb that fills the spool and records beats lives in
//! [`verbs`] and needs no lease.

mod execution;
pub mod plugins;
mod telemetry;
pub mod verbs;

use std::collections::BTreeMap;

use fleet_api::change_request::{self, ChangeRequestDefaults, ChangeRequestSpec};
use fleet_api::conversation::{
    self, CONVERSATION_UID_LABEL, ConversationSpec, ConversationStatus, ThreadDir,
};
use fleet_api::follow::{ForgeDefaults, ForgeStatus};
use fleet_api::forge::{ChangeObservation, Forge, Uncertainty};
use fleet_api::gateway::{
    Capabilities, Gateway, GatewayConfig, HistoryObservation, Identity, ListenerAccount,
};
use fleet_api::listener::{self, ListenerSpec};
use fleet_api::plan::{
    Action, CREATE_ACTION, Create, Effect, EffectOutcome, Observations, Plan, Probe, Receipt,
};
use fleet_api::runtime::Runtime;
use fleet_api::talk::{GatewayStatus, PARENT_LABEL, PARENT_UID_LABEL};
use fleet_api::task;
use fleet_api::{Clock, LeaseToken, Millis, Object, Observation, Store, StoreError};
use fleet_controllers::{
    ChangeRequestController, ChangeRequestInput, ChangeRequestObservations, ConversationController,
    ConversationInput, ConversationObservations, ForgeController, ForgeInput, ForgeObservations,
    GatewayController, GatewayInput, GatewayObservations, ListenerController, ListenerInput,
    ListenerObservations, SESSION_RECHECK_MS, TaskController, TaskInput,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub use verbs::{
    AmendRequest, Amended, ApplyOutcome, DeclareOutcome, Declared, Received, Refusal,
    ResultOutcome, amend, apply, apply_change_request, apply_conversation, beat, conversation_name,
    declare, declare_change_request, delete, derived_name, ensure_listeners, receive,
    record_result, send,
};

/// How a bounded reconcile run ends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RunOptions {
    /// Stop after this many passes even if work remains.
    pub max_passes: u32,
    /// The lease's time to live; renewed before every object and effect.
    pub lease_ttl: Millis,
}

/// What a run did, for `fleet reconcile`'s output.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunReport {
    pub holder: String,
    pub lease_token: u64,
    pub passes: u32,
    /// True when the last pass changed nothing: everything is waiting.
    pub idle: bool,
    pub status_writes: u32,
    pub effects: u32,
    pub conflicts: u32,
    /// Objects that could not be reconciled, with why. Never silently skipped.
    pub errors: Vec<String>,
    /// Observations that were Unknown during the run, with why.
    pub unknowns: Vec<String>,
}

pub struct Manager<'a> {
    pub store: &'a dyn Store,
    pub runtime: &'a dyn Runtime,
    pub clock: &'a dyn Clock,
    pub holder: String,
    /// The configured forges; `None` when the config has no `[forge]`, in
    /// which case a ChangeRequest is reported as an error, never polled.
    pub plugins: Option<(
        &'a dyn fleet_api::plugin::Plugins,
        &'a [fleet_api::plugin::Registration],
    )>,
    pub forge: Option<&'a dyn Forge>,
    /// The workspace port; `None` when the config has no `[workspaces]`, in
    /// which case the runtime makes every workspace itself.
    pub workspaces: Option<&'a dyn fleet_api::workspace::Workspaces>,
    /// The `[forge]` defaults a ChangeRequest spec may override.
    pub change_request_defaults: ChangeRequestDefaults,
    /// The `[forge]` relay defaults a Task's `spec.forge` may override, and
    /// the ready policies it may select.
    pub forge_defaults: ForgeDefaults,
    /// The `[gateway]` section with its port; `None` when the config has
    /// none, in which case a Conversation or Listener is reported as an
    /// error, never reconciled.
    pub gateway: Option<GatewaySide<'a>>,
}

/// The chat side of a run: the port the CLI wired, the section it was
/// wired from, and each account's digest, which the CLI computed.
pub struct GatewaySide<'a> {
    pub port: &'a dyn Gateway,
    pub config: GatewayConfig,
    /// Per account, the digest `fleet gateway account NAME --json` prints.
    pub digests: BTreeMap<String, String>,
    /// Per route, its brief as the CLI read the file when it loaded the
    /// config: what each Task the route starts carries.
    pub briefs: BTreeMap<String, String>,
}

impl std::fmt::Debug for GatewaySide<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GatewaySide")
            .field("accounts", &self.config.accounts.keys().collect::<Vec<_>>())
            .finish_non_exhaustive()
    }
}

/// One sweep's answers from the Gateway port, asked at most once per
/// account, and the accounts a throttled history read deferred.
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

/// The outcome of one object's reconcile.
#[derive(Debug, Default)]
struct Step {
    wrote: u32,
    effects: u32,
    conflict: bool,
}

/// A forge's answer to slow down, handed to every later ChangeRequest of the
/// same host in the sweep instead of asking again.
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

/// The forge host a ChangeRequest is on, once its controller recorded the
/// ref; empty before that, which sorts such objects first.
fn host_of(obj: &Object) -> String {
    obj.status["ref"]["host"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

impl Manager<'_> {
    /// Keep the single manager lease for the entire foreground service.
    /// Every sweep visits all due Tasks, independently of agent activity.
    /// `wait` supplies the clock wait and shutdown policy at the CLI edge.
    pub fn serve(
        &self,
        opts: RunOptions,
        mut wait: impl FnMut(&RunReport) -> bool,
    ) -> Result<(), StoreError> {
        let mut lease = self.store.acquire_lease(&self.holder, opts.lease_ttl)?;
        let outcome = (|| {
            loop {
                lease = self.store.renew_lease(&lease, opts.lease_ttl)?;
                let mut report = RunReport {
                    holder: self.holder.clone(),
                    lease_token: lease.token,
                    ..RunReport::default()
                };
                self.passes(&mut lease, opts, &mut report)?;
                // The last bounded operation may have consumed most of
                // its lease. Give the subsequent poll wait a fresh lease.
                lease = self.store.renew_lease(&lease, opts.lease_ttl)?;
                if !wait(&report) {
                    return Ok(());
                }
            }
        })();
        if !matches!(outcome, Err(StoreError::LeaseLost(_))) {
            self.store.release_lease(&lease)?;
        }
        outcome
    }

    /// Take the lease, run passes until idle or `max_passes`, give the lease
    /// back. A lease that is held elsewhere or lost mid-run ends the run with
    /// that error: no write happens without it.
    pub fn run(&self, opts: RunOptions) -> Result<RunReport, StoreError> {
        let mut lease = self.store.acquire_lease(&self.holder, opts.lease_ttl)?;
        let mut report = RunReport {
            holder: self.holder.clone(),
            lease_token: lease.token,
            ..RunReport::default()
        };
        let outcome = self.passes(&mut lease, opts, &mut report);
        // A lost lease is not ours to release; anything else is.
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
    ) -> Result<(), StoreError> {
        let mut passes = 0;
        if let Some(gateway) = &self.gateway {
            // One Listener per account, as the config loads.
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
            // Fleet's login per forge host, asked once per sweep.
            let mut whoami: BTreeMap<String, Observation<String>> = BTreeMap::new();
            let workers: BTreeMap<String, String> = tasks
                .iter()
                .map(|t| (t.metadata.uid.clone(), t.metadata.name.clone()))
                .collect();
            // What the Gateway controller's due test needs besides the
            // Task: the Conversations as they stand, and each Task's
            // children, from this one listing.
            let conversations: Observation<BTreeMap<String, Object>> =
                self.store.list(conversation::KIND).map(|list| {
                    list.into_iter()
                        .map(|c| (c.metadata.name.clone(), c))
                        .collect()
                });
            if let Some(why) = conversations.why() {
                report.unknowns.push(format!("list Conversation: {why}"));
            }
            // What a Task's change news is judged against, listed only when
            // some Task follows a change.
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
            for obj in &tasks {
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
                // Each later controller decides from the Task as it is now,
                // after the earlier one's write, so the plans do not
                // conflict on every sweep.
                let mut moved = false;
                if task_due {
                    let step = telemetry::reconcile(
                        "TaskController",
                        obj,
                        || self.clock.now(),
                        || self.reconcile_task(obj, lease, opts.lease_ttl, report),
                    );
                    busy |= self.tally(step, &obj.key(), report)?;
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
                                &mut whoami,
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
            busy |= self.sweep_change_requests(lease, opts.lease_ttl, report)?;
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

    /// Every due ChangeRequest, one host at a time in a stable order, so that
    /// a `retryAfter` from a forge defers all of that host's together: the
    /// ones after it in the order are not asked, they take the same answer.
    /// Returns whether anything changed.
    fn sweep_change_requests(
        &self,
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
        let mut due: Vec<&Object> = changes.iter().filter(|c| self.due(c)).collect();
        due.sort_by_cached_key(|o| (host_of(o), o.metadata.name.clone()));
        let mut deferred: BTreeMap<String, Deferral> = BTreeMap::new();
        let mut busy = false;
        for obj in due {
            *lease = self.store.renew_lease(lease, ttl)?;
            let step = telemetry::reconcile(
                "ChangeRequestController",
                obj,
                || self.clock.now(),
                || self.reconcile_change_request(obj, lease, ttl, report, &mut deferred),
            );
            busy |= self.tally(step, &obj.key(), report)?;
        }
        Ok(busy)
    }

    /// One ChangeRequest: parse its URL when its ref is not yet recorded,
    /// observe the change through the Forge port, decide, then write the
    /// status (compare-and-swap, under the lease) and, when the plan says so,
    /// delete the object by its uid and resourceVersion.
    fn reconcile_change_request(
        &self,
        obj: &Object,
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
        };
        if obj.status.get("ref").is_none_or(Value::is_null) {
            // The probe names the ref, so the first poll needs it parsed first.
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
                // Recreated or rewritten since the decision: nothing removed.
                Err(StoreError::Conflict { .. }) => step.conflict = true,
                Err(e) => return Err(e),
            }
        }
        Ok(step)
    }

    /// Every due Conversation, one account at a time in a stable order, so
    /// that a `retryAfter` from a platform defers all of that account's
    /// history reads together. Returns whether anything changed.
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

    /// One Conversation: parse its address when its ref is not yet
    /// recorded, answer its `Object` probe from the store and its
    /// `Capabilities` and `History` probes through the Gateway port, decide,
    /// then write the status through `record_received` (the spool drained
    /// with it, compare-and-swap, under the lease) and, when the plan says
    /// so, delete the object by its uid and resourceVersion.
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
        // Each round answers what the last one asked; the controller asks
        // only for what it lacks, so this ends. The bound is a guard.
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
                        // The tie's Task, or the Task a Fleet-opened
                        // thread's marker names.
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
                    // Not this controller's.
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
        // Intent first (written above), each effect under a fresh lease,
        // receipt after, under the effect's key.
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
                // Not this controller's effect: never performed from here.
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
                // Recreated or rewritten since the decision: nothing removed.
                Err(StoreError::Conflict { .. }) => step.conflict = true,
                Err(e) => return Err(e),
            }
        }
        Ok(step)
    }

    /// One keyed create of a Conversation's plan: the precondition is
    /// checked against the store's Tasks under the lease, then the Task's
    /// `dir` workspace is made on its host, then the fenced create runs. A
    /// directory the runtime cannot make is a refused create, and one it
    /// cannot tell about is Unknown: no Task names a missing directory. A
    /// directory this call made is returned, to be recorded with the
    /// receipt.
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
                t.spec["workspace"]["dir"]
                    .as_str()
                    .is_some_and(|d| fleet_api::task::normalise_dir(d) == wanted)
            }) {
                let busy = EffectOutcome::WorkspaceBusy {
                    task: holder.metadata.name.clone(),
                };
                return (Observation::Known(busy), None);
            }
        }
        // The directory first, recorded when this call made it, whatever
        // the create then answers: Fleet removes it when the thread retires.
        let mut made = None;
        if let Some(dir) = obj.spec["workspace"]["dir"].as_str() {
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

    /// The fenced create of a conversation Task: an object already under
    /// the name is adopted when it carries this Conversation's uid, since an
    /// earlier attempt's receipt may have been lost; one with another uid,
    /// or none, is a refusal. The store's doubt is Unknown, never a refusal.
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
            Err(e) => Observation::Known(EffectOutcome::Failed { why: e.to_string() }),
        }
    }

    /// The host a Task about to be created runs on: its own `host`, else
    /// its TaskConfig's, else the local host. An absent TaskConfig is a
    /// refusal: the directory's host is not known, and a directory made on
    /// a guessed host would leave the Task naming a missing one. A
    /// TaskConfig that cannot be read is Unknown.
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

    /// A retiring thread's Task, deleted through the store, fenced on its
    /// uid: gone already is done; one that is not Concluded, or not this
    /// thread's, is a refusal; the store's doubt is Unknown.
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
            Err(e) => Observation::Known(EffectOutcome::Failed { why: e.to_string() }),
        }
    }

    /// A retiring thread's directory, removed through the runtime port:
    /// only the directory the Conversation records the manager making, and
    /// only while no Task names it. A Task that does is `WorkspaceBusy`,
    /// and the store's doubt is Unknown, never absence.
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
            t.spec["workspace"]["dir"]
                .as_str()
                .is_some_and(|d| fleet_api::task::normalise_dir(d) == wanted)
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

    /// A Conversation's effect happened (or may have): its receipt must
    /// land under its key even if the object moved meanwhile, so this
    /// re-reads and retries on a conflict, through the controller's typed
    /// write with the spool as it is.
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

    /// Every due Listener. Returns whether anything changed.
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

    /// One Listener: answer its `Capabilities` and `GatewayWhoami` probes
    /// through the Gateway port, from the sweep's cache, decide, then write
    /// the status (compare-and-swap, under the lease) and, when its account
    /// left the config, delete the object by its uid and resourceVersion.
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

    /// An account's capabilities, asked once per sweep.
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

    /// ForgeController on one Task: answer its `Object` probes from the store
    /// (no lease renewal: no adapter is asked), then its `Whoami` probes
    /// through the Forge port and its `AtRest` probe through the runtime,
    /// each after a renewal, asking again until the controller wants
    /// nothing more; decide; write `status.forge` and the relay's entries in
    /// one typed compare-and-swap write, the mark-ready intent with them;
    /// then perform each `MarkReady` effect through the Forge port after a
    /// renewal and write its receipt onto the entry.
    fn reconcile_forge(
        &self,
        obj: &Object,
        lease: &mut LeaseToken,
        ttl: Millis,
        report: &mut RunReport,
        whoami: &mut BTreeMap<String, Observation<String>>,
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
        // Each round answers what the last one asked; the controller asks
        // only for what it lacks, so this ends. The bound is a guard.
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
                        let answer = match whoami.get(&host) {
                            Some(a) => a.clone(),
                            None => {
                                let Some(forge) = self.forge else {
                                    return Err(StoreError::Invalid(
                                        "no [forge] section is configured; a declared change cannot be followed"
                                            .into(),
                                    ));
                                };
                                *lease = self.store.renew_lease(lease, ttl)?;
                                let a = forge.whoami(&host);
                                whoami.insert(host.clone(), a.clone());
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
                    // Not this controller's.
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
            // Someone wrote first, or the key already landed: decide again
            // from fresh state on the next pass. Nothing ran yet.
            Err(StoreError::Conflict { .. }) | Err(StoreError::AlreadyExists(_)) => {
                step.conflict = true;
                return Ok(step);
            }
            Err(e) => return Err(e),
        };
        for effect in &plan.effects {
            let Action::MarkReady { change, expect } = &effect.action else {
                // Not this controller's effect: never performed from here.
                continue;
            };
            let Some(forge) = self.forge else {
                return Err(StoreError::Invalid(
                    "no [forge] section is configured; a change cannot be marked ready".into(),
                ));
            };
            // Intent first (written above), effect under a fresh lease,
            // receipt after.
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

    /// GatewayController on one Task: answer its `Object` probe for the
    /// Conversation from the store and its `List` probe for the children
    /// from the sweep's Task listing (neither renews the lease: no adapter
    /// is asked), asking again until the controller wants nothing more;
    /// decide; write `status.gateway` and the relay's entries in one typed
    /// compare-and-swap write, the intents of its `Open`, `Receive`, `Post`,
    /// `Log` and `React` actions with them; then perform each, receipt after.
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
                        // A read of the log channel recovers a log post; any
                        // other, a post in the thread.
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
                    // Not this controller's.
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
            // Someone wrote first, or the key already landed: decide again
            // from fresh state on the next pass, the intents unwritten and
            // so nothing performed.
            Err(StoreError::Conflict { .. }) | Err(StoreError::AlreadyExists(_)) => {
                step.conflict = true;
                return Ok(step);
            }
            Err(e) => return Err(e),
        }
        // Intent first (written above), each effect under a fresh lease,
        // receipt after, under the effect's key.
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

    /// One of GatewayController's effects: a post or an open through the
    /// Gateway port, or Fleet's opening post added to its thread's
    /// Conversation through the store.
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

    /// The effect happened (or may have): its receipt lands on the record
    /// that holds its intent under `status.gateway.posts`, even if the Task
    /// moved meanwhile, so this re-reads and retries on a conflict.
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

    /// The mark-ready effect happened (or may have): its receipt must land
    /// on the entry that holds its intent, even if the Task moved meanwhile,
    /// so this re-reads and retries on a conflict. The write replaces
    /// `status.forge` through the Forge controller's typed mutation, with
    /// nothing appended.
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

    /// A Task waiting out `notBefore` after a failed or unknown attempt is
    /// not looked at again until then, in this run or the next. A Concluded
    /// Task is at rest, and due again while its inbox holds something to
    /// deliver, once it is being deleted, or when its session is due to be
    /// looked up again. A Concluded Task whose session may be paused is
    /// due on every sweep, so that its activity is watched.
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
            .is_none_or(Vec::is_empty);
        let recheck_due = obj
            .status
            .get("sessionCheckedAt")
            .and_then(Value::as_u64)
            .is_none_or(|t| t.saturating_add(SESSION_RECHECK_MS) <= self.clock.now());
        !(concluded
            && inbox_empty
            && !obj.is_deleting()
            && !recheck_due
            && !fleet_controllers::task::watches_activity(obj))
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
            Err(e @ StoreError::LeaseLost(_)) => Err(e),
            Err(e) => {
                report.errors.push(format!("{key}: {e}"));
                Ok(false)
            }
        }
    }

    fn reconcile_task(
        &self,
        obj: &Object,
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
        let needs_launch = spec.config_ref.is_some() || !spec.env.is_empty();
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
            observations: Observations::default(),
            now: self.clock.now(),
            materialize: self.workspaces.is_some(),
        };
        let ctl = TaskController;
        for probe in ctl.probes(&input)? {
            // Every probe is bounded but not instant: renew first, so a
            // Task's probes together never outlive the lease.
            *lease = self.store.renew_lease(lease, ttl)?;
            self.observe(&probe, obj, &mut input.observations, report);
        }
        let mut plan = ctl.reconcile(&input)?;
        if let Some(execution) = effective {
            for effect in &mut plan.effects {
                if let Action::Spawn(spawn) = &mut effect.action {
                    spawn.harness = execution.harness.clone();
                    // A workspace Fleet made decides the host it runs on.
                    spawn.host = spawn
                        .ready
                        .as_ref()
                        .map_or_else(|| execution.host.clone(), |w| w.host.clone());
                    spawn.model = execution.model.clone();
                    spawn.environment_frozen = true;
                    spawn.env = execution.env.clone();
                }
            }
        }
        self.perform_plan(obj, plan, lease, ttl, report)
    }

    /// Write the status, run the effects with their receipts, then remove the
    /// finalizer: intent before effect, receipt after.
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
        // A delivery receipt is written with the status, as one mutation:
        // the receipt into status.deliveries, its entry out of status.inbox.
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
                // Someone wrote first (the worker, a person): nothing ran yet,
                // so drop this plan and decide again from what is there now.
                Err(StoreError::Conflict { .. }) => {
                    step.conflict = true;
                    return Ok(step);
                }
                Err(e) => return Err(e),
            }
        }
        for effect in &plan.effects {
            // Fence the effect itself, not only the writes around it: a
            // manager that lost the lease since its intent write stops here.
            // (The runtime cannot be fenced from fleet's side; an effect
            // already started when the lease changed hands is adopted or
            // refused by its key.)
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
            Probe::FindWorkspace { host, key: k } => {
                let o = match self.workspaces {
                    Some(w) => w.find(host, k),
                    None => Observation::unknown("no [workspaces] adapter is configured"),
                };
                note(&format!("findWorkspace on {host}"), o.why());
                obs.workspace = Some(o);
            }
            // A runtime that cannot pause never answers Idle, so Fleet
            // never pauses on it.
            Probe::Activity { session } => {
                let o = if rt.can_pause() {
                    rt.activity(session)
                } else {
                    Observation::unknown("the runtime cannot pause a session")
                };
                note("activity", o.why());
                obs.activity = Some(o);
            }
            // Not TaskController's probes: it never asks them.
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
        let no_port = || {
            Observation::Known(EffectOutcome::Failed {
                why: "no [workspaces] adapter is configured".into(),
            })
        };
        match &effect.action {
            Action::Materialize {
                host,
                key,
                repositories,
            } => self
                .workspaces
                .map_or_else(no_port, |w| w.materialize(host, key, repositories)),
            Action::CleanWorkspace { workspace } => {
                self.workspaces.map_or_else(no_port, |w| w.clean(workspace))
            }
            Action::Spawn(spec) => self.runtime.spawn(spec),
            Action::Brief { session, text } => self.runtime.brief(session, text, &effect.key),
            Action::Release { session } => self.runtime.release(session),
            Action::Revive { session } => self.runtime.revive(session),
            Action::Pause { session } => self.runtime.pause(session),
            Action::Resume { session } => self.runtime.resume(session),
            // The Forge, Conversation and Gateway controllers' effects,
            // performed by their own steps; TaskController never plans them.
            Action::MarkReady { .. }
            | Action::Post { .. }
            | Action::Open { .. }
            | Action::Receive { .. }
            | Action::Log { .. }
            | Action::React { .. }
            | Action::DeleteTask { .. }
            | Action::RemoveDir { .. } => Observation::Known(EffectOutcome::Failed {
                why: format!("{} is not a Task controller effect", effect.action.name()),
            }),
        }
    }

    /// The effect happened (or may have): its receipt must land even if the
    /// object moved meanwhile, so this re-reads and retries on a conflict.
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
