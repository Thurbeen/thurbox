//! TaskController: spawns one agent session for one brief, briefs it once,
//! delivers what is queued in the Task's inbox (amendments to the brief
//! among it), records the worker's answer, pauses the session while its
//! agent is idle and resumes it when work arrives, and releases the session
//! when the Task is deleted.
//!
//! Pure: it reads the Task and what the probes saw, and returns a Plan. The
//! manager performs the plan.

use fleet_api::condition::{
    ConditionStatus, condition, is_true, set_condition, settle_transitions,
};
use fleet_api::inbox::{DeliveryAttempt, DeliveryReceipt, InboxEntry, MAX_INBOX};
use fleet_api::meta::{Millis, Object};
use fleet_api::plan::{
    Action, Effect, EffectOutcome, Intent, Observations, Plan, Probe, Receipt, SpawnSpec,
    effect_key,
};
use fleet_api::runtime::{Activity, WorkspaceTrust};
use fleet_api::store::FieldManager;
use fleet_api::task::{self, BriefRecord, Phase, SessionRef, SessionState, TaskSpec, TaskStatus};
use fleet_api::validate::typed;
use fleet_api::{Observation, StoreError};

/// What one reconcile of one Task reads. Every outside fact arrives here as
/// a value; nothing is read from the world inside the controller.
#[derive(Debug, Clone)]
pub struct TaskInput<'a> {
    pub task: &'a Object,
    pub observations: Observations,
    pub now: Millis,
}

/// How many times a spawn or a brief send is tried before the Task stalls,
/// and how many unreadable delivery checks or failed releases raise
/// NeedsAttention (those two keep being retried: neither can do harm).
pub const MAX_ATTEMPTS: u32 = 3;

/// The wait after a failed or unknown attempt before the next one, persisted
/// as `status.notBefore` so a new manager run honours it too.
pub const RETRY_DELAY_MS: Millis = 30_000;

/// How often a Concluded Task at rest has its session looked up again, so
/// a session deleted out of band is reported. Recorded as
/// `status.sessionCheckedAt`.
pub const SESSION_RECHECK_MS: Millis = 300_000;

#[derive(Debug, Clone, Copy, Default)]
pub struct TaskController;

/// Condition types TaskController writes.
pub mod conditions {
    pub const HOST_REACHABLE: &str = "HostReachable";
    pub const SPAWNED: &str = "Spawned";
    pub const BRIEFED: &str = "Briefed";
    pub const STALLED: &str = "Stalled";
    pub const WORKER_GONE: &str = "WorkerGone";
    pub const CONCLUDED: &str = "Concluded";
    pub const RELEASED: &str = "Released";
    /// Whether the session's agent trusts its workspace; the brief waits
    /// until it does.
    pub const WORKSPACE_TRUSTED: &str = "WorkspaceTrusted";
    /// The runtime holds the session but its agent's window is gone, as
    /// after a host reboot.
    pub const SESSION_LOST: &str = "SessionLost";
    /// Something only a person can decide: a stall, a vanished worker, a
    /// release that kept failing.
    pub const NEEDS_ATTENTION: &str = "NeedsAttention";
    /// The runtime holds the session with its agent stopped: Fleet paused
    /// it while it was idle, or someone else stopped it.
    pub const PAUSED: &str = "Paused";
}
use conditions::*;

const SPAWN: &str = "spawn";
/// The reason a spawn waits for the runtime to map a repository.
const UNMAPPED: &str = "RepositoryUnmapped";
/// The reason a spawn waits for the runtime to map the harness's trust.
const TRUST_UNMAPPED: &str = "TrustUnmapped";
/// The NeedsAttention reason while a known trust gap holds the brief.
const WORKSPACE_UNTRUSTED: &str = "WorkspaceUntrusted";
const BRIEF: &str = "brief";
/// The runtime only queued the brief, where the agent does not read it.
const BRIEF_UNDELIVERED: &str = "BriefUndelivered";
const RELEASE: &str = "release";
const REVIVE: &str = "revive";
const PAUSE: &str = "pause";
const RESUME: &str = "resume";
/// The note owed to a revived or resumed session, before its inbox.
const CATCH_UP: &str = "catchup";
/// The NeedsAttention reason once revives keep leaving the session lost.
const REVIVE_FAILED: &str = "ReviveFailed";
/// The NeedsAttention reason once resumes keep leaving the session stopped.
const RESUME_FAILED: &str = "ResumeFailed";

/// What one keyed send came to on this pass.
enum Sent {
    /// On the session: `recovered` when the journal recovered it, `via` the
    /// runtime's route when the send named one, `may_hold` when that route
    /// can hold it behind an approval dialog.
    Delivered {
        recovered: bool,
        via: Option<String>,
        may_hold: bool,
    },
    /// Sent, waiting, or not sendable yet.
    Pending,
    /// Its sends ran out.
    GivenUp,
}

impl TaskController {
    /// The kind this controller reconciles and the field manager it writes as.
    pub const KIND: &'static str = task::KIND;
    pub const MANAGER: FieldManager = FieldManager::TaskController;

    /// What the next `reconcile` needs observed. Only these are asked.
    pub fn probes(&self, input: &TaskInput<'_>) -> Result<Vec<Probe>, StoreError> {
        let (spec, st) = decode(input.task)?;
        let host = st
            .session
            .as_ref()
            .map_or_else(|| spec.host.clone(), |s| s.host.clone());
        let find = Probe::Find {
            host: host.clone(),
            key: session_key(input.task),
        };
        if input.task.is_deleting() {
            let mut probes = vec![find];
            if let Some(s) = &st.session {
                probes.push(Probe::AtRest { session: s.clone() });
            }
            return Ok(probes);
        }
        Ok(match st.phase {
            Phase::Pending if spec.dispatch => vec![Probe::Host { host }],
            Phase::Spawning => vec![find],
            Phase::Spawned => {
                let mut probes = vec![find];
                if let Some(s) = &st.session {
                    probes.push(Probe::Lost { session: s.clone() });
                    probes.push(Probe::WorkspaceTrust {
                        session: s.clone(),
                        workspace: spec.workspace.clone(),
                    });
                    probes.push(Probe::Ready { session: s.clone() });
                    let bk = brief_key(input.task);
                    if st.intents.contains_key(&bk) && !known_receipt(&st, &bk) {
                        probes.push(Probe::BriefDelivered {
                            session: s.clone(),
                            key: bk,
                        });
                    } else {
                        // The brief is not in flight: the inbox may ask.
                        inbox_probes(&st, s, &mut probes);
                    }
                }
                probes
            }
            Phase::Briefed if !result_current(&st) => {
                let mut probes = vec![find];
                if let Some(s) = &st.session {
                    probes.push(Probe::Lost { session: s.clone() });
                    probes.push(Probe::Paused { session: s.clone() });
                    delivery_probes(&st, s, &mut probes);
                    activity_probe(input, &spec, &st, s, &mut probes);
                }
                probes
            }
            // A Concluded Task is due for delivery, and at rest once per
            // recheck interval; its session is looked up, so a vanished
            // worker is reported, never respawned. Only a delivery revives a
            // lost session: a concluded worker at rest stays at rest. A
            // session that may be paused has its activity watched.
            Phase::Concluded
                if next_entry(&st).is_some()
                    || session_recheck_due(&st, input.now)
                    || watched(input.task, &spec, &st) =>
            {
                let mut probes = Vec::new();
                if next_entry(&st).is_some() || session_recheck_due(&st, input.now) {
                    probes.push(find);
                }
                if let Some(s) = &st.session {
                    let delivering = next_entry(&st).is_some() || st.catch_up.is_some();
                    if delivering || watched(input.task, &spec, &st) {
                        probes.push(Probe::Lost { session: s.clone() });
                        probes.push(Probe::Paused { session: s.clone() });
                    }
                    if delivering {
                        delivery_probes(&st, s, &mut probes);
                    }
                    activity_probe(input, &spec, &st, s, &mut probes);
                }
                probes
            }
            _ => Vec::new(),
        })
    }

    pub fn reconcile(&self, input: &TaskInput<'_>) -> Result<Plan, StoreError> {
        let (spec, before) = decode(input.task)?;
        let mut r = Pass {
            input,
            spec: &spec,
            st: before.clone(),
            generation: input.task.metadata.generation,
            effects: Vec::new(),
            remove_finalizer: None,
            delivery: None,
        };
        r.st.observed_generation = r.generation;
        if input.task.is_deleting() {
            r.release();
        } else {
            r.advance();
        }
        r.observe_session();
        settle_transitions(&mut r.st.conditions, &before.conditions);
        // A delivery is a status write even when nothing else moved: the
        // store takes the status along with the receipt.
        let status = if r.st == before && r.delivery.is_none() {
            None
        } else {
            Some(
                serde_json::to_value(&r.st)
                    .map_err(|e| StoreError::Invalid(format!("Task status: {e}")))?,
            )
        };
        Ok(Plan {
            status,
            effects: r.effects,
            remove_finalizer: r.remove_finalizer,
            delete: false,
            delivery: r.delivery,
            appends: Vec::new(),
            creates: Vec::new(),
        })
    }
}

/// One reconcile in progress.
struct Pass<'a, 'b> {
    input: &'a TaskInput<'b>,
    spec: &'a TaskSpec,
    st: TaskStatus,
    generation: u64,
    effects: Vec<Effect>,
    remove_finalizer: Option<String>,
    delivery: Option<DeliveryReceipt>,
}

impl Pass<'_, '_> {
    fn now(&self) -> Millis {
        self.input.now
    }

    fn cond(&mut self, t: &str, s: ConditionStatus, reason: &str, message: impl Into<String>) {
        let (generation, now) = (self.generation, self.now());
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

    /// Record what this pass's answers proved about the session, for views.
    /// Nothing decides on it; a pass that proved nothing keeps the last state.
    fn observe_session(&mut self) {
        let Some(session) = &self.st.session else {
            self.st.session_state = None;
            return;
        };
        if let Some(state) = SessionState::observe(session, &self.input.observations) {
            self.st.session_state = Some(state);
        }
    }

    /// Wait [`RETRY_DELAY_MS`] from `from` before the next attempt.
    fn wait_from(&mut self, from: Millis) -> bool {
        let until = from.saturating_add(RETRY_DELAY_MS);
        if until > self.now() {
            self.st.not_before = Some(until);
            true
        } else {
            self.st.not_before = None;
            false
        }
    }

    fn intend(&mut self, key: String, action: Action) {
        let intent = Intent {
            action: action.name().to_string(),
            at: self.now(),
            generation: self.generation,
        };
        self.st.intents.insert(key.clone(), intent);
        self.effects.push(Effect { key, action });
    }

    fn needs_attention(&mut self, reason: &str, message: impl Into<String>) {
        self.cond(NEEDS_ATTENTION, ConditionStatus::True, reason, message);
    }

    /// Clear NeedsAttention when the cause `reason` named has gone.
    fn settle_attention(&mut self, reason: &str) {
        if condition(&self.st.conditions, NEEDS_ATTENTION)
            .is_some_and(|c| c.status == ConditionStatus::True && c.reason == reason)
        {
            self.cond(NEEDS_ATTENTION, ConditionStatus::False, "Resolved", "");
        }
    }

    fn advance(&mut self) {
        match self.st.phase {
            // Stalled has no session to deliver to; nothing moves.
            Phase::Stalled => return,
            // Terminal, except that its inbox is still delivered.
            Phase::Concluded => return self.concluded(),
            _ => {}
        }
        // A result for the generation the worker was last given ends the
        // run: a result is only accepted once the brief has a receipt, so
        // this is Briefed in practice. One for an older generation answers
        // less than the worker now has, and the run goes on.
        if let Some(result) = self.st.result.as_ref().filter(|_| result_current(&self.st)) {
            let outcome = format!("{:?}", result.outcome);
            self.st.phase = Phase::Concluded;
            // Reporting a result is work: the idle time starts again.
            self.st.idle_since = None;
            self.cond(
                CONCLUDED,
                ConditionStatus::True,
                "ResultRecorded",
                format!("outcome {outcome}"),
            );
            return;
        }
        match self.st.phase {
            Phase::Pending => self.pending(),
            Phase::Spawning => self.spawning(),
            Phase::Spawned => self.spawned(),
            Phase::Briefed => self.briefed(),
            Phase::Stalled | Phase::Concluded | Phase::Releasing => {}
        }
    }

    fn pending(&mut self) {
        if self.st.not_before.is_some_and(|t| t > self.now()) {
            return;
        }
        if !self.spec.dispatch {
            self.cond(
                SPAWNED,
                ConditionStatus::False,
                "AwaitingDispatch",
                "spec.dispatch is false",
            );
            return;
        }
        match self.input.observations.host.clone() {
            Some(Observation::Known(true)) => {
                self.cond(HOST_REACHABLE, ConditionStatus::True, "Reachable", "");
            }
            Some(Observation::Known(false)) => {
                self.cond(HOST_REACHABLE, ConditionStatus::False, "Unreachable", "");
                return;
            }
            Some(Observation::Unknown { why }) => {
                self.cond(HOST_REACHABLE, ConditionStatus::Unknown, "ProbeFailed", why);
                return;
            }
            None => {
                self.cond(HOST_REACHABLE, ConditionStatus::Unknown, "NotProbed", "");
                return;
            }
        }
        let attempt = self.st.spawn_attempts + 1;
        let key = self.key(SPAWN, Some(attempt));
        self.st.spawn_attempts = attempt;
        self.st.not_before = None;
        // An attempt given back after an unmapped repository reuses its key.
        self.st.receipts.remove(&key);
        self.st.phase = Phase::Spawning;
        self.cond(
            SPAWNED,
            ConditionStatus::Unknown,
            "Spawning",
            format!("attempt {attempt}"),
        );
        let spec = SpawnSpec {
            host: self.spec.host.clone(),
            key: session_key(self.input.task),
            harness: self.spec.harness.clone(),
            workspace: self.spec.workspace.clone(),
            model: self.spec.model.clone(),
            env: self.spec.env.clone(),
            environment_frozen: false,
        };
        self.intend(key, Action::Spawn(spec));
    }

    fn spawning(&mut self) {
        let key = self.key(SPAWN, Some(self.st.spawn_attempts));
        match self.st.receipts.get(&key).map(|r| r.outcome.clone()) {
            Some(Observation::Known(EffectOutcome::Spawned { session, .. })) => {
                self.adopt(session, "Spawned")
            }
            Some(Observation::Known(EffectOutcome::Failed { why })) => self.spawn_retry(&why),
            Some(Observation::Known(EffectOutcome::Unmapped { url })) => self.spawn_unmapped(
                UNMAPPED,
                format!("repository {url} is not in runtime.repositories"),
            ),
            Some(Observation::Known(EffectOutcome::TrustUnmapped { harness })) => self
                .spawn_unmapped(
                    TRUST_UNMAPPED,
                    format!(
                        "harness {harness} is not in runtime.workspaceTrust; set runtime.workspaceTrust.{harness} to codex, claude or none"
                    ),
                ),
            Some(Observation::Known(other)) => {
                self.spawn_retry(&format!("unexpected outcome {other:?}"))
            }
            // Unknown outcome, or an intent with no receipt (a crash between the
            // effect and its receipt): the key decides, never a blind respawn.
            Some(Observation::Unknown { .. }) | None => {
                match self.input.observations.find.clone() {
                    Some(Observation::Known(Some(session))) => {
                        let receipt = Receipt {
                            action: SPAWN.into(),
                            at: self.now(),
                            outcome: Observation::Known(EffectOutcome::Spawned {
                                session: session.clone(),
                                adopted: true,
                            }),
                        };
                        self.st.receipts.insert(key, receipt);
                        self.adopt(session, "Adopted");
                    }
                    Some(Observation::Known(None)) => self.spawn_retry("no session holds the key"),
                    Some(Observation::Unknown { why }) => {
                        self.cond(
                            SPAWNED,
                            ConditionStatus::Unknown,
                            "SessionListUnreadable",
                            why,
                        );
                    }
                    None => {}
                }
            }
        }
    }

    fn adopt(&mut self, session: SessionRef, reason: &str) {
        self.st.session = Some(session);
        self.st.phase = Phase::Spawned;
        self.cond(SPAWNED, ConditionStatus::True, reason, "");
        self.settle_spawn_gaps();
    }

    /// A repository or harness the runtime does not map is a configuration
    /// gap, not a failed attempt: nothing started, so the attempt is given
    /// back. The spawn is tried again after [`RETRY_DELAY_MS`], without
    /// limit, until the runtime maps it.
    fn spawn_unmapped(&mut self, reason: &str, why: String) {
        self.st.spawn_attempts = self.st.spawn_attempts.saturating_sub(1);
        self.st.phase = Phase::Pending;
        self.wait_from(self.now());
        self.cond(SPAWNED, ConditionStatus::False, reason, why.clone());
        self.needs_attention(reason, why);
    }

    fn settle_spawn_gaps(&mut self) {
        self.settle_attention(UNMAPPED);
        self.settle_attention(TRUST_UNMAPPED);
    }

    /// A spawn proven not to have happened is tried again after
    /// [`RETRY_DELAY_MS`], at most [`MAX_ATTEMPTS`] times in all.
    fn spawn_retry(&mut self, why: &str) {
        self.settle_spawn_gaps();
        let attempts = self.st.spawn_attempts;
        if attempts >= MAX_ATTEMPTS {
            self.st.phase = Phase::Stalled;
            self.cond(
                SPAWNED,
                ConditionStatus::False,
                "SpawnFailed",
                why.to_string(),
            );
            self.cond(
                STALLED,
                ConditionStatus::True,
                "SpawnAttemptsExhausted",
                format!("{attempts} attempts: {why}"),
            );
            self.needs_attention("Stalled", "spawn attempts exhausted");
            return;
        }
        self.st.phase = Phase::Pending;
        self.wait_from(self.now());
        self.cond(
            SPAWNED,
            ConditionStatus::False,
            "SpawnRetry",
            why.to_string(),
        );
    }

    fn spawned(&mut self) {
        let Some(session) = self.st.session.clone() else {
            // A Spawned phase without a session is not a state this controller
            // writes; start over rather than brief nobody.
            self.st.phase = Phase::Pending;
            return;
        };
        if self.worker_gone() || self.lost(&session) {
            return;
        }
        if self.st.not_before.is_some_and(|t| t > self.now()) {
            return;
        }
        self.brief_step(session.clone());
        // The brief comes first: an entry goes out only once it was sent, in
        // a pass that sent nothing and has no brief send unconfirmed. A brief
        // held for trust or readiness holds the inbox with it.
        let sent = self.st.intents.contains_key(&brief_key(self.input.task));
        if matches!(self.st.phase, Phase::Spawned | Phase::Briefed)
            && sent
            && !self.brief_pending()
            && !self.brief_queued()
        {
            self.deliver(session);
        }
    }

    /// Whether the runtime only queued the brief, where the agent does not
    /// read it.
    fn brief_queued(&self) -> bool {
        matches!(
            self.st
                .receipts
                .get(&brief_key(self.input.task))
                .map(|r| &r.outcome),
            Some(Observation::Known(EffectOutcome::Queued { .. }))
        )
    }

    /// Whether the brief was sent and its receipt is not Known yet.
    fn brief_pending(&self) -> bool {
        let key = brief_key(self.input.task);
        self.st.phase == Phase::Spawned
            && self.st.intents.contains_key(&key)
            && !known_receipt(&self.st, &key)
    }

    fn brief_step(&mut self, session: SessionRef) {
        let key = brief_key(self.input.task);
        match self.st.receipts.get(&key).map(|r| r.outcome.clone()) {
            Some(Observation::Known(EffectOutcome::Delivered { .. })) => {
                return self.briefed_now(key, false);
            }
            // The text waits where the agent does not read it. It is not
            // sent again: a person moves it on, and the inbox waits too.
            Some(Observation::Known(EffectOutcome::Queued { why })) => {
                self.cond(
                    BRIEFED,
                    ConditionStatus::False,
                    BRIEF_UNDELIVERED,
                    why.clone(),
                );
                self.needs_attention(BRIEF_UNDELIVERED, why);
                return;
            }
            Some(Observation::Known(EffectOutcome::Failed { why })) => {
                self.cond(BRIEFED, ConditionStatus::False, "BriefRefused", why);
                if self.st.brief_sends < MAX_ATTEMPTS && self.st.not_before.is_none() {
                    let at = self.st.receipts[&key].at;
                    if self.wait_from(at) {
                        return;
                    }
                }
                return self.send_brief(session, key);
            }
            _ => {}
        }
        if self.st.intents.contains_key(&key) {
            // Sent, but no Known receipt: ask the session. Known yes is a
            // recovered receipt, Known no a send, Unknown a read tried again
            // after a wait: a read can do no harm, so it never stalls the Task.
            match self.input.observations.brief_delivered.clone() {
                Some(Observation::Known(true)) => {
                    let receipt = Receipt {
                        action: BRIEF.into(),
                        at: self.now(),
                        outcome: Observation::Known(EffectOutcome::Delivered {
                            already: true,
                            via: None,
                            may_hold: false,
                        }),
                    };
                    self.st.receipts.insert(key.clone(), receipt);
                    self.briefed_now(key, true);
                }
                Some(Observation::Known(false)) => self.send_brief(session, key),
                Some(Observation::Unknown { why }) => {
                    self.st.brief_probe_failures += 1;
                    self.cond(
                        BRIEFED,
                        ConditionStatus::Unknown,
                        "BriefUnconfirmed",
                        why.clone(),
                    );
                    self.wait_from(self.now());
                    if self.st.brief_probe_failures >= MAX_ATTEMPTS {
                        self.needs_attention(
                            "BriefUnconfirmed",
                            format!("the brief's delivery could not be read: {why}"),
                        );
                    }
                }
                None => {}
            }
            return;
        }
        self.send_brief(session, key);
    }

    /// Send under the one brief key. Each send is counted and bounded; the
    /// session refuses a second delivery of the same key, so a send after a
    /// Known "not delivered" cannot brief twice.
    fn send_brief(&mut self, session: SessionRef, key: String) {
        if self.st.brief_sends >= MAX_ATTEMPTS {
            self.st.phase = Phase::Stalled;
            let msg = format!("{} sends", self.st.brief_sends);
            self.cond(
                STALLED,
                ConditionStatus::True,
                "BriefAttemptsExhausted",
                msg,
            );
            self.needs_attention("Stalled", "brief attempts exhausted");
            return;
        }
        // Every send, including retries proven not delivered, needs fresh
        // trust and readiness. Receipt recovery above sends nothing.
        if !self.workspace_trusted() {
            return;
        }
        match self.input.observations.ready.clone() {
            Some(Observation::Known(true)) => {}
            Some(Observation::Known(false)) => {
                self.cond(BRIEFED, ConditionStatus::False, "SessionNotReady", "");
                return;
            }
            Some(Observation::Unknown { why }) => {
                self.cond(BRIEFED, ConditionStatus::Unknown, "ReadyUnknown", why);
                return;
            }
            None => return,
        }
        self.st.brief_sends += 1;
        self.st.brief_probe_failures = 0;
        self.st.not_before = None;
        self.st.receipts.remove(&key);
        let msg = format!("send {}", self.st.brief_sends);
        self.cond(BRIEFED, ConditionStatus::Unknown, "Sending", msg);
        // The brief and every amendment so far, as one message: an
        // amendment queued before the brief went out is then already given.
        let text = self
            .spec
            .assignment(&self.input.task.metadata.name, self.generation);
        self.intend(key, Action::Brief { session, text });
    }

    /// Whether the session's agent trusts its workspace on this pass. A
    /// known gap asks a person and is looked at again after a wait. Unknown
    /// proves nothing either way: like readiness, it is asked again on the
    /// next pass, so a starting agent is not held back.
    fn workspace_trusted(&mut self) -> bool {
        match self.input.observations.workspace_trust.clone() {
            Some(Observation::Known(WorkspaceTrust::Trusted { how })) => {
                self.cond(WORKSPACE_TRUSTED, ConditionStatus::True, "Trusted", how);
                self.settle_attention(WORKSPACE_UNTRUSTED);
                true
            }
            Some(Observation::Known(WorkspaceTrust::Untrusted { gap, why })) => {
                self.cond(
                    WORKSPACE_TRUSTED,
                    ConditionStatus::False,
                    gap.reason(),
                    why.clone(),
                );
                self.needs_attention(WORKSPACE_UNTRUSTED, why);
                self.wait_from(self.now());
                false
            }
            Some(Observation::Unknown { why }) => {
                self.cond(
                    WORKSPACE_TRUSTED,
                    ConditionStatus::Unknown,
                    "TrustUnknown",
                    why,
                );
                false
            }
            None => false,
        }
    }

    fn briefed_now(&mut self, key: String, recovered: bool) {
        self.st.brief_probe_failures = 0;
        self.st.not_before = None;
        self.settle_attention("BriefUnconfirmed");
        // The generation the brief was composed for, which an amendment
        // since may have moved past.
        let generation = self
            .st
            .intents
            .get(&key)
            .map_or(self.generation, |i| i.generation);
        self.st.brief = Some(BriefRecord {
            key,
            generation,
            recovered,
        });
        self.st.phase = Phase::Briefed;
        let reason = if recovered {
            "ReceiptRecovered"
        } else {
            "ReceiptWritten"
        };
        self.cond(BRIEFED, ConditionStatus::True, reason, "");
        self.cond(CONCLUDED, ConditionStatus::False, "NoResultYet", "");
    }

    fn briefed(&mut self) {
        let Some(session) = self.st.session.clone() else {
            return;
        };
        if self.worker_gone()
            || self.lost(&session)
            || self.pause_state(&session)
            || self.catch_up(session.clone())
        {
            return;
        }
        self.deliver(session.clone());
        self.pause_step(session);
    }

    /// Concluded is terminal, and delivery goes on: a message for a worker
    /// that has reported is still a message. A readable list that no longer
    /// shows the session is WorkerGone and attention, never a respawn.
    fn concluded(&mut self) {
        if next_entry(&self.st).is_some() {
            // A delivery breaks the idle time: it starts again at rest.
            self.st.idle_since = None;
        }
        if next_entry(&self.st).is_none() {
            // At rest: a recheck. Only an answered lookup counts as one.
            if self.input.observations.find.is_some() {
                self.st.session_checked_at = Some(self.now());
                if self.worker_gone() {
                    return;
                }
            }
            // A worker at rest is never revived, but it may be paused. A
            // pass that does not watch it breaks its idle time.
            match self.st.session.clone() {
                Some(session) if watched(self.input.task, self.spec, &self.st) => {
                    if !self.pause_state(&session) {
                        self.pause_step(session);
                    }
                }
                _ => self.st.idle_since = None,
            }
            return;
        }
        let Some(session) = self.st.session.clone() else {
            return;
        };
        if self.worker_gone() || self.lost(&session) || self.pause_state(&session) {
            return;
        }
        if self.st.not_before.is_some_and(|t| t > self.now()) {
            return;
        }
        if !self.catch_up(session.clone()) {
            self.deliver(session);
        }
    }

    /// Deliver the oldest inbox entry not given up on: the brief protocol
    /// under the entry's key. One entry per pass; nothing else of the Task
    /// changes, apart from the attention a failed delivery asks for.
    fn deliver(&mut self, session: SessionRef) {
        if self.st.inbox.len() > MAX_INBOX {
            self.needs_attention(
                "InboxFull",
                format!(
                    "{} undelivered entries; appends are refused past {MAX_INBOX}",
                    self.st.inbox.len()
                ),
            );
        } else {
            self.settle_attention("InboxFull");
        }
        // Not in a pass that sent something, is waiting something out, or
        // has lost sight of the session.
        if !self.effects.is_empty()
            || self.st.not_before.is_some_and(|t| t > self.now())
            || condition(&self.st.conditions, WORKER_GONE)
                .is_some_and(|c| c.status == ConditionStatus::True)
        {
            return;
        }
        let Some(entry) = next_entry(&self.st).cloned() else {
            return;
        };
        let what = format!("inbox entry {} from {}", entry.key, entry.from);
        let Some(generation) = entry.amends else {
            if let Sent::Delivered {
                recovered,
                via,
                may_hold,
            } = self.keyed_send(session, &entry.key, entry.text.clone(), &what)
            {
                self.record_delivery(&entry, recovered, via, may_hold);
            }
            return;
        };
        let Some(given) = self.st.brief.as_ref().map(|b| b.generation) else {
            return;
        };
        // The worker has it already, or the newer amendment queued behind
        // it, never sent yet, will carry it: recorded without a send.
        let newer = self.st.inbox.iter().any(|e| {
            e.amends.is_some_and(|g| g > generation)
                && !self
                    .st
                    .delivery_attempts
                    .get(&e.key)
                    .is_some_and(|a| a.failed)
        });
        if generation <= given || (newer && !self.st.intents.contains_key(&entry.key)) {
            self.record_delivery(&entry, false, None, false);
            return;
        }
        let text = self
            .spec
            .amendment_message(&self.input.task.metadata.name, given, generation);
        if let Sent::Delivered {
            recovered,
            via,
            may_hold,
        } = self.keyed_send(session, &entry.key, text, &what)
        {
            self.record_delivery(&entry, recovered, via, may_hold);
            self.amended(entry.key, generation, recovered);
        }
    }

    /// The worker was given the amendment that created `generation`: a
    /// result must now name it, and a Concluded Task is open again, its old
    /// result kept until the worker replaces it.
    fn amended(&mut self, key: String, generation: u64, recovered: bool) {
        self.st.brief = Some(BriefRecord {
            key,
            generation,
            recovered,
        });
        if self.st.phase == Phase::Concluded {
            self.st.phase = Phase::Briefed;
            self.cond(
                CONCLUDED,
                ConditionStatus::False,
                "Amended",
                format!("the amendment for generation {generation} was delivered; a result must cover it"),
            );
        }
    }

    /// The brief protocol under one key, for an inbox entry or a catch-up
    /// note: a Known receipt decides, else the session's journal does after
    /// a send; a send needs the session observed ready on this pass, at most
    /// [`MAX_ATTEMPTS`] times. `what` names it in attention messages.
    fn keyed_send(&mut self, session: SessionRef, key: &str, text: String, what: &str) -> Sent {
        match self.st.receipts.get(key).map(|r| r.outcome.clone()) {
            Some(Observation::Known(EffectOutcome::Delivered { via, may_hold, .. })) => {
                return Sent::Delivered {
                    recovered: false,
                    via,
                    may_hold,
                };
            }
            // Queued where the agent does not read it: given up, never sent
            // again, so that later entries go out past it.
            Some(Observation::Known(EffectOutcome::Queued { why })) => {
                self.st
                    .delivery_attempts
                    .entry(key.to_string())
                    .or_default()
                    .failed = true;
                self.st.not_before = None;
                self.needs_attention(
                    "DeliveryQueued",
                    format!(
                        "{what}: {why}. It is not sent again; it stays in status.inbox and later entries are delivered past it"
                    ),
                );
                return Sent::GivenUp;
            }
            Some(Observation::Known(EffectOutcome::Failed { .. })) => {
                let attempt = self
                    .st
                    .delivery_attempts
                    .get(key)
                    .cloned()
                    .unwrap_or_default();
                if attempt.sends < MAX_ATTEMPTS && self.st.not_before.is_none() {
                    let at = self.st.receipts[key].at;
                    if self.wait_from(at) {
                        return Sent::Pending;
                    }
                }
                return self.send_keyed(session, key, text, what);
            }
            Some(Observation::Known(_)) => {
                return self.send_keyed(session, key, text, what);
            }
            _ => {}
        }
        if self.st.intents.contains_key(key) {
            // Sent, but no Known receipt: the session's journal decides,
            // exactly as for the brief. Unknown is read again after a wait.
            match self.input.observations.brief_delivered.clone() {
                Some(Observation::Known(true)) => {
                    return Sent::Delivered {
                        recovered: true,
                        via: None,
                        may_hold: false,
                    };
                }
                Some(Observation::Known(false)) => {
                    return self.send_keyed(session, key, text, what);
                }
                Some(Observation::Unknown { why }) => {
                    let attempt = self
                        .st
                        .delivery_attempts
                        .entry(key.to_string())
                        .or_default();
                    attempt.probe_failures += 1;
                    let failures = attempt.probe_failures;
                    if failures >= MAX_ATTEMPTS {
                        // Unknown is not absence, so the entry is never sent
                        // again; it is given up so that later entries go out.
                        attempt.failed = true;
                        self.st.not_before = None;
                        self.needs_attention(
                            "DeliveryUnconfirmed",
                            format!(
                                "{what}: its delivery could not be read: {why}. It is not sent again; \
                                 it stays in status.inbox and later entries are delivered past it"
                            ),
                        );
                        return Sent::GivenUp;
                    }
                    self.wait_from(self.now());
                }
                None => {}
            }
            return Sent::Pending;
        }
        self.send_keyed(session, key, text, what)
    }

    /// Send under `key`, to a session observed ready on this pass, at most
    /// [`MAX_ATTEMPTS`] times; past that it is given up on, and whatever
    /// comes next goes out past it.
    fn send_keyed(&mut self, session: SessionRef, key: &str, text: String, what: &str) -> Sent {
        let sends = self.st.delivery_attempts.get(key).map_or(0, |a| a.sends);
        if sends >= MAX_ATTEMPTS {
            self.st
                .delivery_attempts
                .entry(key.to_string())
                .or_default()
                .failed = true;
            self.st.not_before = None;
            let why = match self.st.receipts.get(key).map(|r| &r.outcome) {
                Some(Observation::Known(EffectOutcome::Failed { why })) => format!(": {why}"),
                _ => String::new(),
            };
            self.needs_attention(
                "DeliveryFailed",
                format!("{what}: {sends} sends refused{why}"),
            );
            return Sent::GivenUp;
        }
        if !matches!(
            self.input.observations.ready,
            Some(Observation::Known(true))
        ) {
            return Sent::Pending;
        }
        let attempt = self
            .st
            .delivery_attempts
            .entry(key.to_string())
            .or_default();
        attempt.sends += 1;
        attempt.probe_failures = 0;
        self.st.not_before = None;
        self.st.receipts.remove(key);
        self.intend(key.to_string(), Action::Brief { session, text });
        Sent::Pending
    }

    /// The entry is on the session: its receipt goes to `status.deliveries`
    /// and the entry leaves the inbox, in one write the manager performs.
    fn record_delivery(
        &mut self,
        entry: &InboxEntry,
        recovered: bool,
        delivered_via: Option<String>,
        may_hold: bool,
    ) {
        let key = entry.key.clone();
        self.st.delivery_attempts.remove(&key);
        self.st.intents.remove(&key);
        self.st.receipts.remove(&key);
        self.st.not_before = None;
        // A given-up entry keeps its attention while it stays queued.
        if !self.st.delivery_attempts.values().any(|a| a.failed) {
            self.settle_attention("DeliveryUnconfirmed");
        }
        self.delivery = Some(DeliveryReceipt {
            key,
            from: entry.from.clone(),
            at: self.now(),
            recovered,
            delivered_via,
            may_hold,
        });
    }

    /// A session a readable list no longer shows raises WorkerGone and
    /// NeedsAttention; nothing abandons the Task. An unreadable list is never
    /// read as gone. Returns true once WorkerGone is set.
    fn worker_gone(&mut self) -> bool {
        let same_session = |s: &SessionRef| self.st.session.as_ref() == Some(s);
        let present = match &self.input.observations.find {
            Some(Observation::Known(found)) => found.as_ref().is_some_and(same_session),
            _ => return false,
        };
        if present {
            if condition(&self.st.conditions, WORKER_GONE)
                .is_some_and(|c| c.status == ConditionStatus::True)
            {
                self.cond(
                    WORKER_GONE,
                    ConditionStatus::False,
                    "SessionPresent",
                    "the session list shows the session again",
                );
            }
            self.settle_attention("WorkerGone");
            return false;
        }
        self.cond(
            WORKER_GONE,
            ConditionStatus::True,
            "SessionGone",
            "the session list no longer shows the session holding its key",
        );
        self.needs_attention("WorkerGone", "the session list no longer shows the session");
        true
    }

    /// A session the runtime still holds but whose agent's window is gone,
    /// as after a host reboot, is revived in its own workspace under the
    /// same identity. Nothing is sent to it meanwhile. Revives wait
    /// [`RETRY_DELAY_MS`] between attempts and raise NeedsAttention after
    /// [`MAX_ATTEMPTS`], but go on: a revive of a running session does
    /// nothing. A held Task (`dispatch: false`) is not revived. Returns true
    /// while the session is lost.
    fn lost(&mut self, session: &SessionRef) -> bool {
        match self.input.observations.lost {
            Some(Observation::Known(true)) => {}
            Some(Observation::Known(false)) => {
                self.found_again();
                return false;
            }
            // Unknown proves nothing either way.
            _ => return false,
        }
        self.cond(
            SESSION_LOST,
            ConditionStatus::True,
            "WindowGone",
            "the runtime holds the session, but its agent's window is gone",
        );
        if !self.spec.dispatch {
            return true;
        }
        if self.st.revive_attempts > 0 {
            let last = self.key(REVIVE, Some(self.st.revivals));
            match self.st.not_before {
                Some(t) if t > self.now() => return true,
                Some(_) => self.st.not_before = None,
                None => {
                    let at = self
                        .st
                        .receipts
                        .get(&last)
                        .map(|r| r.at)
                        .or_else(|| self.st.intents.get(&last).map(|i| i.at))
                        .unwrap_or(0);
                    if self.wait_from(at) {
                        return true;
                    }
                }
            }
            if self.st.revive_attempts >= MAX_ATTEMPTS {
                self.needs_attention(
                    REVIVE_FAILED,
                    format!(
                        "{} revives left the session without a window",
                        self.st.revive_attempts
                    ),
                );
            }
        }
        self.st.revivals += 1;
        self.st.revive_attempts += 1;
        let key = self.key(REVIVE, Some(self.st.revivals));
        self.intend(
            key,
            Action::Revive {
                session: session.clone(),
            },
        );
        true
    }

    /// The session's window is there. After it was lost, a briefed worker
    /// is owed a catch-up note, sent before anything else.
    fn found_again(&mut self) {
        self.st.revive_attempts = 0;
        let was_lost = condition(&self.st.conditions, SESSION_LOST)
            .is_some_and(|c| c.status == ConditionStatus::True);
        if !was_lost {
            return;
        }
        self.st.not_before = None;
        self.cond(
            SESSION_LOST,
            ConditionStatus::False,
            "Revived",
            "the session's window is back",
        );
        self.settle_attention(REVIVE_FAILED);
        self.owe_catch_up();
    }

    /// A briefed worker whose agent was started again is owed a catch-up
    /// note, once, before anything else is delivered.
    fn owe_catch_up(&mut self) {
        if self.st.brief.is_some() && self.st.catch_up.is_none() {
            self.st.catch_ups += 1;
            self.st.catch_up = Some(self.key(CATCH_UP, Some(self.st.catch_ups)));
        }
    }

    /// Send the catch-up note owed to a revived or resumed session, under
    /// its own key, as an inbox entry is sent. Returns true while it is
    /// owed: nothing else is delivered before it. A note a record kept
    /// under an older `resume/<n>` key is sent under that key.
    fn catch_up(&mut self, session: SessionRef) -> bool {
        let Some(key) = self.st.catch_up.clone() else {
            return false;
        };
        if !self.effects.is_empty() || self.st.not_before.is_some_and(|t| t > self.now()) {
            return true;
        }
        let what = format!("catch-up note {key}");
        match self.keyed_send(session, &key, self.catch_up_text(), &what) {
            Sent::Pending => true,
            Sent::Delivered { .. } | Sent::GivenUp => {
                self.st.catch_up = None;
                self.st.intents.remove(&key);
                self.st.receipts.remove(&key);
                self.st.delivery_attempts.remove(&key);
                false
            }
        }
    }

    /// What a revived or resumed worker is told. A worker still on its
    /// brief is given the brief again, under the generation it was briefed
    /// for.
    fn catch_up_text(&self) -> String {
        let generation = self
            .st
            .brief
            .as_ref()
            .map_or(self.generation, |b| b.generation);
        let head = "Fleet: your session was restarted, after its host went down or after \
                    Fleet paused it while it was idle, and your conversation was resumed. \
                    Background shells, Monitors and loops you started did not survive; start \
                    again any you still need. Work you did not commit, push or declare may \
                    be lost. Re-read your worktree, the changes you declared and your inbox";
        if self.st.phase == Phase::Concluded {
            format!(
                "{head}, then act on the messages that follow. Your result for generation \
                 {generation} is already recorded."
            )
        } else {
            format!(
                "{head}, then continue your brief, generation {generation}, repeated \
                 below.\n\n{}",
                self.spec
                    .assignment(&self.input.task.metadata.name, generation)
            )
        }
    }

    /// Where the session stands on pausing, from what is in flight and what
    /// the runtime shows:
    ///
    /// - a pause in flight is settled by its receipt, else by the session;
    /// - a resume in flight likewise, and is tried again while the session
    ///   is proven stopped;
    /// - a session found stopped is Paused, `External` when Fleet did not
    ///   stop it, and an entry to deliver resumes it;
    /// - a paused session found running again is resumed.
    ///
    /// Returns true while the session is paused or a pause is in flight:
    /// nothing is sent to it meanwhile.
    fn pause_state(&mut self, session: &SessionRef) -> bool {
        let observed = self.input.observations.paused.clone();
        let pause = self.key(PAUSE, Some(self.st.pauses));
        if effect_in_flight(&self.st, &pause, PAUSE) {
            match self.st.receipts.get(&pause).map(|r| r.outcome.clone()) {
                Some(Observation::Known(EffectOutcome::Paused)) => self.paused_by_fleet(&pause),
                Some(Observation::Known(other)) => {
                    let why = match other {
                        EffectOutcome::Failed { why } => why,
                        other => format!("unexpected outcome {other:?}"),
                    };
                    self.pause_refused(&pause, why);
                    return false;
                }
                // No Known receipt: the session decides, never a second stop.
                Some(Observation::Unknown { .. }) | None => match observed {
                    Some(Observation::Known(true)) => self.paused_by_fleet(&pause),
                    Some(Observation::Known(false)) => {
                        self.pause_refused(&pause, "the session is still running".into());
                        return false;
                    }
                    _ => return true,
                },
            }
            return true;
        }
        let resume = self.key(RESUME, Some(self.st.resumes));
        if effect_in_flight(&self.st, &resume, RESUME) {
            match self.st.receipts.get(&resume).map(|r| r.outcome.clone()) {
                Some(Observation::Known(EffectOutcome::Resumed)) => {
                    self.resumed(Some(&resume));
                    return false;
                }
                // Refused: tried again after a wait.
                Some(Observation::Known(_)) => {
                    self.resume_again(&resume);
                    return true;
                }
                // No Known receipt: a running session is resumed; one still
                // stopped is resumed again after a wait, which is harmless.
                Some(Observation::Unknown { .. }) | None => match observed {
                    Some(Observation::Known(false)) => {
                        self.resumed(Some(&resume));
                        return false;
                    }
                    Some(Observation::Known(true)) => {
                        self.resume_again(&resume);
                        return true;
                    }
                    _ => return true,
                },
            }
        }
        let paused = is_true(&self.st.conditions, PAUSED);
        match observed {
            Some(Observation::Known(true)) => {
                self.st.idle_since = None;
                if !paused {
                    self.cond(
                        PAUSED,
                        ConditionStatus::True,
                        "External",
                        "the session was found stopped, and Fleet did not stop it",
                    );
                }
                if next_entry(&self.st).is_some() && self.spec.dispatch {
                    self.resume_now(session);
                }
                true
            }
            Some(Observation::Known(false)) => {
                if paused {
                    self.resumed(None);
                }
                false
            }
            // Unknown proves nothing: a paused session stays paused.
            _ => paused,
        }
    }

    fn paused_by_fleet(&mut self, key: &str) {
        self.st.intents.remove(key);
        self.st.receipts.remove(key);
        self.st.idle_since = None;
        self.cond(
            PAUSED,
            ConditionStatus::True,
            "Idle",
            "Fleet paused the session while its agent was idle",
        );
    }

    /// The pause did not happen. The idle time starts again from the next
    /// Idle observation.
    fn pause_refused(&mut self, key: &str, why: String) {
        self.st.intents.remove(key);
        self.st.receipts.remove(key);
        self.st.idle_since = None;
        self.cond(PAUSED, ConditionStatus::False, "PauseFailed", why);
    }

    /// Plan one resume. A held, stalled or deleting Task never comes here.
    /// Resumes wait [`RETRY_DELAY_MS`] between attempts and raise
    /// NeedsAttention after [`MAX_ATTEMPTS`], but go on: a resume of a
    /// running session does nothing.
    fn resume_now(&mut self, session: &SessionRef) {
        if !self.effects.is_empty() || self.st.not_before.is_some_and(|t| t > self.now()) {
            return;
        }
        self.st.not_before = None;
        if self.st.resume_attempts >= MAX_ATTEMPTS {
            self.needs_attention(
                RESUME_FAILED,
                format!(
                    "{} resumes left the session stopped",
                    self.st.resume_attempts
                ),
            );
        }
        self.st.resumes += 1;
        self.st.resume_attempts += 1;
        let key = self.key(RESUME, Some(self.st.resumes));
        self.intend(
            key,
            Action::Resume {
                session: session.clone(),
            },
        );
    }

    /// The resume under `key` left the session stopped: drop it, and wait
    /// from its last word before the next one.
    fn resume_again(&mut self, key: &str) {
        let at = self
            .st
            .receipts
            .get(key)
            .map(|r| r.at)
            .or_else(|| self.st.intents.get(key).map(|i| i.at))
            .unwrap_or(0);
        self.st.intents.remove(key);
        self.st.receipts.remove(key);
        self.wait_from(at);
    }

    /// The session runs again, after Fleet's resume under `key` or after
    /// someone else started it. The worker is owed a catch-up note.
    fn resumed(&mut self, key: Option<&str>) {
        if let Some(key) = key {
            self.st.intents.remove(key);
            self.st.receipts.remove(key);
        }
        self.st.resume_attempts = 0;
        self.st.idle_since = None;
        self.cond(
            PAUSED,
            ConditionStatus::False,
            "Resumed",
            "the session's agent runs again",
        );
        self.settle_attention(RESUME_FAILED);
        self.owe_catch_up();
    }

    /// Pause the session once its agent was seen Idle, without a break,
    /// for `pauseAfterMs`. The pass that plans the pause read the activity
    /// fresh, and proved the session running and not lost. Any
    /// other answer, or a pass that sent something, starts the idle time
    /// again.
    fn pause_step(&mut self, session: SessionRef) {
        let o = &self.input.observations;
        let idle = pause_candidate(self.spec, &self.st)
            && self.effects.is_empty()
            && self.delivery.is_none()
            && !self.st.not_before.is_some_and(|t| t > self.now())
            && o.activity == Some(Observation::Known(Activity::Idle))
            && o.lost != Some(Observation::Known(true))
            && o.paused == Some(Observation::Known(false));
        if !idle {
            self.st.idle_since = None;
            return;
        }
        let since = *self.st.idle_since.get_or_insert(self.input.now);
        let after = self.spec.pause_after_ms.unwrap_or(Millis::MAX);
        if since.saturating_add(after) > self.now() {
            return;
        }
        self.st.pauses += 1;
        let key = self.key(PAUSE, Some(self.st.pauses));
        self.intend(key, Action::Pause { session });
    }

    /// The finalizer: the session is deleted only once it is proven at rest
    /// or gone. A failed release is tried again after a wait; past
    /// [`MAX_ATTEMPTS`] it also raises NeedsAttention, but the finalizer is
    /// never given up on, so the record cannot be stranded.
    fn release(&mut self) {
        self.st.phase = Phase::Releasing;
        // Nothing was ever spawned: there is no session to release.
        if self.st.session.is_none() && self.st.spawn_attempts == 0 {
            self.cond(RELEASED, ConditionStatus::True, "NothingToRelease", "");
            self.remove_finalizer = Some(task::RELEASE_FINALIZER.to_string());
            return;
        }
        let pending_spawn = self.st.session.is_none() && self.st.spawn_attempts > 0;
        if pending_spawn {
            match self.input.observations.find.clone() {
                Some(Observation::Known(Some(s))) => self.st.session = Some(s),
                Some(Observation::Known(None)) => {}
                Some(Observation::Unknown { why }) => {
                    self.cond(
                        RELEASED,
                        ConditionStatus::Unknown,
                        "SessionListUnreadable",
                        why,
                    );
                    return;
                }
                None => return,
            }
        }
        let Some(session) = self.st.session.clone() else {
            self.cond(RELEASED, ConditionStatus::True, "NothingToRelease", "");
            self.remove_finalizer = Some(task::RELEASE_FINALIZER.to_string());
            return;
        };
        if self.st.release_attempts > 0 {
            let last = self.key(RELEASE, Some(self.st.release_attempts));
            match self.st.receipts.get(&last).cloned() {
                Some(Receipt {
                    outcome: Observation::Known(EffectOutcome::Released),
                    ..
                }) => {
                    self.cond(RELEASED, ConditionStatus::True, "Released", "");
                    self.remove_finalizer = Some(task::RELEASE_FINALIZER.to_string());
                    return;
                }
                receipt => {
                    let why = match &receipt {
                        Some(Receipt {
                            outcome: Observation::Known(EffectOutcome::Failed { why }),
                            ..
                        }) => why.clone(),
                        Some(Receipt {
                            outcome: Observation::Unknown { why },
                            ..
                        }) => format!("outcome unknown: {why}"),
                        Some(_) => "unexpected outcome".to_string(),
                        None => "no receipt (interrupted)".to_string(),
                    };
                    if self.st.release_attempts >= MAX_ATTEMPTS {
                        self.cond(RELEASED, ConditionStatus::False, "ReleaseFailed", why);
                        self.needs_attention(
                            "ReleaseFailed",
                            format!("{} release attempts failed", self.st.release_attempts),
                        );
                    } else {
                        self.cond(RELEASED, ConditionStatus::False, "ReleaseRetry", why);
                    }
                    // Wait once per failed attempt, from that attempt: its
                    // receipt, or its intent when it was interrupted before
                    // the receipt. A wait that has run out lets it retry.
                    match self.st.not_before {
                        Some(t) if t > self.now() => return,
                        Some(_) => self.st.not_before = None,
                        None => {
                            let at = receipt
                                .map(|r| r.at)
                                .or_else(|| self.st.intents.get(&last).map(|i| i.at))
                                .unwrap_or(0);
                            if self.wait_from(at) {
                                return;
                            }
                        }
                    }
                }
            }
        }
        let gone = matches!(self.input.observations.find, Some(Observation::Known(None)));
        if !gone {
            match self.input.observations.at_rest.clone() {
                Some(Observation::Known(true)) => {}
                Some(Observation::Known(false)) => {
                    self.cond(
                        RELEASED,
                        ConditionStatus::False,
                        "SessionBusy",
                        "waiting for the session to be at rest",
                    );
                    return;
                }
                Some(Observation::Unknown { why }) => {
                    self.cond(RELEASED, ConditionStatus::Unknown, "AtRestUnknown", why);
                    return;
                }
                None => return,
            }
        }
        let attempt = self.st.release_attempts + 1;
        self.st.release_attempts = attempt;
        let key = self.key(RELEASE, Some(attempt));
        self.cond(
            RELEASED,
            ConditionStatus::Unknown,
            "Releasing",
            format!("attempt {attempt}"),
        );
        self.intend(key, Action::Release { session });
    }

    fn key(&self, action: &str, attempt: Option<u32>) -> String {
        let m = &self.input.task.metadata;
        effect_key(task::KIND, &m.name, &m.uid, action, attempt)
    }
}

fn decode(obj: &Object) -> Result<(TaskSpec, TaskStatus), StoreError> {
    let mut spec: TaskSpec = typed(task::KIND, &obj.spec)?;
    let st: TaskStatus = if obj.status.is_null() {
        TaskStatus::default()
    } else {
        serde_json::from_value(obj.status.clone())
            .map_err(|e| StoreError::Invalid(format!("Task status: {e}")))?
    };
    if let Some(execution) = &st.execution {
        spec.harness = execution.harness.clone();
        spec.host = execution.host.clone();
        spec.model = execution.model.clone();
        spec.pause_after_ms = execution.pause_after_ms;
    }
    Ok((spec, st))
}

/// Whether the session may be paused, apart from its activity: a pause
/// setting, dispatch on, Briefed or Concluded, and nothing in flight that a
/// pause would cut short.
fn pause_candidate(spec: &TaskSpec, st: &TaskStatus) -> bool {
    spec.pause_after_ms.is_some()
        && spec.dispatch
        && (st.phase == Phase::Concluded || (st.phase == Phase::Briefed && !result_current(st)))
        && st.session.is_some()
        && st.brief.is_some()
        && next_entry(st).is_none()
        && st.catch_up.is_none()
        && st.revive_attempts == 0
        && st.resume_attempts == 0
        && ![PAUSED, SESSION_LOST, WORKER_GONE]
            .iter()
            .any(|c| is_true(&st.conditions, c))
}

/// Whether a pause or a resume waits for its outcome.
fn in_flight(obj: &Object, st: &TaskStatus) -> bool {
    let m = &obj.metadata;
    let key = |action, n| effect_key(task::KIND, &m.name, &m.uid, action, Some(n));
    effect_in_flight(st, &key(PAUSE, st.pauses), PAUSE)
        || effect_in_flight(st, &key(RESUME, st.resumes), RESUME)
}

/// Whether `key` holds an intent of the effect `action`. A record from
/// before catch-up notes keeps its note's Brief intent under a
/// `resume/<n>` key: that is a note, never a resume effect.
fn effect_in_flight(st: &TaskStatus, key: &str, action: &str) -> bool {
    st.intents.get(key).is_some_and(|i| i.action == action)
}

fn watched(obj: &Object, spec: &TaskSpec, st: &TaskStatus) -> bool {
    !obj.is_deleting() && (pause_candidate(spec, st) || in_flight(obj, st))
}

/// Whether a Task's session activity is watched: it may be paused, or a
/// pause or resume waits for its outcome. The manager reconciles such a
/// Concluded Task on every sweep. An undecodable Task is not watched.
pub fn watches_activity(obj: &Object) -> bool {
    decode(obj).is_ok_and(|(spec, st)| watched(obj, &spec, &st))
}

/// The activity probe, asked only of a session that may be paused.
fn activity_probe(
    input: &TaskInput<'_>,
    spec: &TaskSpec,
    st: &TaskStatus,
    session: &SessionRef,
    probes: &mut Vec<Probe>,
) {
    if pause_candidate(spec, st)
        && !input.task.is_deleting()
        && !st.not_before.is_some_and(|t| t > input.now)
    {
        probes.push(Probe::Activity {
            session: session.clone(),
        });
    }
}

fn session_key(obj: &Object) -> String {
    task::session_key(&obj.metadata.name, &obj.metadata.uid)
}

/// The brief has one key: every send of it reuses the key, so the session can
/// refuse a duplicate.
fn brief_key(obj: &Object) -> String {
    let m = &obj.metadata;
    effect_key(task::KIND, &m.name, &m.uid, BRIEF, None)
}

/// Whether the recorded result answers the generation the worker was last
/// given.
fn result_current(st: &TaskStatus) -> bool {
    match (&st.result, &st.brief) {
        (Some(r), Some(b)) => r.generation == b.generation,
        (Some(_), None) => true,
        (None, _) => false,
    }
}

fn known_receipt(st: &TaskStatus, key: &str) -> bool {
    st.receipts
        .get(key)
        .is_some_and(|r| !r.outcome.is_unknown())
}

/// The oldest inbox entry still to deliver: an entry given up on after its
/// sends ran out stays queued but blocks nothing.
fn next_entry(st: &TaskStatus) -> Option<&InboxEntry> {
    st.inbox.iter().find(|e| {
        !st.delivery_attempts
            .get(&e.key)
            .is_some_and(|a: &DeliveryAttempt| a.failed)
    })
}

/// What delivering the next entry needs observed: readiness, and the
/// journal when the entry was sent without a Known receipt.
/// Whether a Concluded Task at rest is due to have its session looked up
/// again: never looked up yet, or [`SESSION_RECHECK_MS`] since the last time.
pub fn session_recheck_due(st: &TaskStatus, now: Millis) -> bool {
    st.session_checked_at
        .is_none_or(|t| t.saturating_add(SESSION_RECHECK_MS) <= now)
}

/// What the next delivery needs observed: the catch-up note's, while one
/// is owed, and the inbox's otherwise.
fn delivery_probes(st: &TaskStatus, session: &SessionRef, probes: &mut Vec<Probe>) {
    let Some(key) = &st.catch_up else {
        return inbox_probes(st, session, probes);
    };
    if !probes.iter().any(|p| matches!(p, Probe::Ready { .. })) {
        probes.push(Probe::Ready {
            session: session.clone(),
        });
    }
    if st.intents.contains_key(key) && !known_receipt(st, key) {
        probes.push(Probe::BriefDelivered {
            session: session.clone(),
            key: key.clone(),
        });
    }
}

fn inbox_probes(st: &TaskStatus, session: &SessionRef, probes: &mut Vec<Probe>) {
    let Some(entry) = next_entry(st) else {
        return;
    };
    if !probes.iter().any(|p| matches!(p, Probe::Ready { .. })) {
        probes.push(Probe::Ready {
            session: session.clone(),
        });
    }
    if st.intents.contains_key(&entry.key) && !known_receipt(st, &entry.key) {
        probes.push(Probe::BriefDelivered {
            session: session.clone(),
            key: entry.key.clone(),
        });
    }
}
