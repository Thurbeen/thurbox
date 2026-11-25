
use fleet_api::condition::{
    Condition, ConditionStatus, condition, is_true, set_condition, settle_transitions,
};
use fleet_api::inbox::{self, DeliveryAttempt, DeliveryReceipt, InboxEntry, MAX_INBOX};
use fleet_api::meta::{Millis, Object};
use fleet_api::plan::{
    Action, Effect, EffectOutcome, Intent, Observations, Plan, Probe, Receipt, SpawnSpec,
    effect_key,
};
use fleet_api::runtime::{Activity, WorkspaceTrust};
use fleet_api::store::FieldManager;
use fleet_api::task::{
    self, BriefRecord, Phase, RestartRequest, SessionRef, SessionState, TaskSpec, TaskStatus,
};
use fleet_api::validate::typed;
use fleet_api::{Observation, StoreError};

#[derive(Debug, Clone)]
pub struct TaskInput<'a> {
    pub task: &'a Object,
    pub observations: Observations,
    pub now: Millis,
}

pub const MAX_ATTEMPTS: u32 = 3;

pub const RETRY_DELAY_MS: Millis = 30_000;

pub const RETRY_BACKOFF_MAX_MS: Millis = 3_600_000;

pub const SESSION_RECHECK_MS: Millis = 300_000;

const OUTBOX_UNREAD: &str = "OutboxUnread";

const HELD_BY_AN_IDLE_AGENT: &str = "the agent stayed idle and its prompt hook never reported it, so it may sit unsent in its composer";

pub const RELEASE_GRACE_MS: Millis = 300_000;

fn retry_delay_after_failures(attempts: u32) -> Millis {
    let Some(past) = attempts.checked_sub(MAX_ATTEMPTS) else {
        return RETRY_DELAY_MS;
    };
    1u64.checked_shl(past)
        .map_or(RETRY_BACKOFF_MAX_MS, |factor| {
            RETRY_DELAY_MS.saturating_mul(factor)
        })
        .min(RETRY_BACKOFF_MAX_MS)
}

#[derive(Debug, Clone, Copy, Default)]
pub struct TaskController;

pub mod conditions {
    pub const HOST_REACHABLE: &str = "HostReachable";
    pub const SPAWNED: &str = "Spawned";
    pub const BRIEFED: &str = "Briefed";
    pub const STALLED: &str = "Stalled";
    pub const WORKER_GONE: &str = "WorkerGone";
    pub const CONCLUDED: &str = "Concluded";
    pub const RELEASED: &str = "Released";
    pub const WORKSPACE_TRUSTED: &str = "WorkspaceTrusted";
    pub const SESSION_LOST: &str = "SessionLost";
    pub const NEEDS_ATTENTION: &str = "NeedsAttention";
    pub const PAUSED: &str = "Paused";
    pub const RESTARTED: &str = "Restarted";
    pub const WORKSPACE_READY: &str = "WorkspaceReady";
}
use conditions::*;

const SPAWN: &str = "spawn";
const MATERIALIZE: &str = "materialize";
const CLEAN: &str = "cleanWorkspace";
const CLEAN_FAILED: &str = "CleanFailed";
const UNSUPPORTED: &str = "WorkspaceUnsupported";
const TRUST_UNMAPPED: &str = "TrustUnmapped";
const WORKSPACE_UNTRUSTED: &str = "WorkspaceUntrusted";
const HOOK_REPORT_HINT_MS: Millis = 60_000;
const BRIEF: &str = "brief";
const INTERACTIVE: &str = "Interactive";
const BRIEF_UNDELIVERED: &str = "BriefUndelivered";
const AWAITING_PROMPT: &str = "AwaitingPrompt";
const RELEASE: &str = "release";
const REVIVE: &str = "revive";
const PAUSE: &str = "pause";
const RESUME: &str = "resume";
const CATCH_UP: &str = "catchup";
const RESTART: &str = "restart";
const REBRIEF: &str = "rebrief";
const RESTART_UNCONFIRMED: &str = "RestartUnconfirmed";
const CANNOT_RESUME: &str = "CannotResume";
const REVIVE_FAILED: &str = "ReviveFailed";
const RESUME_FAILED: &str = "ResumeFailed";

fn phase_condition(phase: Phase) -> &'static str {
    match phase {
        Phase::Pending | Phase::Spawning | Phase::Spawned => SPAWNED,
        Phase::Briefed => BRIEFED,
        Phase::Stalled => STALLED,
        Phase::Concluded => CONCLUDED,
        Phase::Releasing => RELEASED,
    }
}

enum Proof {
    NotNeeded,
    Proven,
    Awaiting(String),
    NotAsked,
}

enum Sent {
    Delivered {
        recovered: bool,
        via: Option<String>,
        may_hold: bool,
    },
    Pending,
    GivenUp,
}

impl TaskController {
    pub const KIND: &'static str = task::KIND;
    pub const MANAGER: FieldManager = FieldManager::TaskController;

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
        let find_workspace = materialize_in_flight(input.task, &st).then(|| Probe::FindWorkspace {
            host: spec.host.clone(),
            key: session_key(input.task),
            urls: spec.workspace.urls().to_vec(),
        });
        if input.task.is_deleting() {
            let mut probes = vec![find];
            if let Some(s) = &st.session {
                probes.push(Probe::AtRest { session: s.clone() });
            }
            return Ok(probes);
        }
        Ok(match st.phase {
            Phase::Pending if spec.dispatch => {
                let mut probes = vec![Probe::Host { host }];
                probes.extend(find_workspace);
                probes
            }
            Phase::Spawning => vec![find],
            Phase::Spawned => {
                let mut probes = vec![find];
                if let Some(s) = &st.session {
                    activity_probe(input, &spec, &st, s, &mut probes);
                    probes.push(Probe::Lost { session: s.clone() });
                    probes.push(Probe::WorkspaceTrust {
                        session: s.clone(),
                        workspace: spec.workspace.clone(),
                        ready: st.workspace.clone(),
                    });
                    probes.push(Probe::Ready { session: s.clone() });
                    if spec.is_interactive() {
                        probes.push(Probe::Paused { session: s.clone() });
                    }
                    let bk = brief_key(input.task);
                    if awaits_proof(&st, &bk) {
                        proof_probe(s, &mut probes);
                    } else if st.intents.contains_key(&bk) && !known_receipt(&st, &bk) {
                        probes.push(Probe::BriefDelivered {
                            session: s.clone(),
                            key: bk,
                        });
                    } else {
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
            Phase::Concluded
                if next_entry(&st).is_some()
                    || st.catch_up.is_some()
                    || session_recheck_due(&st, input.now)
                    || reviving(&st)
                    || watched(input.task, &spec, &st) =>
            {
                let mut probes = Vec::new();
                let recheck = session_recheck_due(&st, input.now);
                if next_entry(&st).is_some() || recheck {
                    probes.push(find);
                }
                if let Some(s) = &st.session {
                    let delivering = next_entry(&st).is_some() || st.catch_up.is_some();
                    let watching = delivering || watched(input.task, &spec, &st);
                    if watching || recheck || reviving(&st) {
                        probes.push(Probe::Lost { session: s.clone() });
                    }
                    if watching {
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
        r.start_outbox_unread(&before);
        r.queue_phase_event(&before);
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

    fn observe_session(&mut self) {
        let Some(session) = &self.st.session else {
            self.st.session_state = None;
            self.st.agent_state = None;
            self.st.activity = None;
            return;
        };
        if let Some(state) = SessionState::observe(session, &self.input.observations) {
            self.st.session_state = Some(state);
        }
        if matches!(
            self.st.session_state,
            Some(SessionState::Lost | SessionState::Gone)
        ) {
            self.st.agent_state = None;
            self.st.activity = None;
            return;
        }
        let profile = fleet_api::harness::profile(&self.spec.harness);
        let agentless = profile.authority == fleet_api::harness::Authority::None
            && profile.screen_rules().is_none();
        if agentless {
            self.st.activity = None;
        }
        let screen = &self.input.observations.screen;
        let seen = match screen {
            Some(Observation::Known(Some(s))) => {
                profile.screen_rules().and_then(|rules| rules.evaluate(s))
            }
            _ => None,
        };
        let unread = fleet_api::signal::Reported::default();
        let r = match &self.input.observations.signals {
            Some(Observation::Known(r)) => r,
            Some(Observation::Unknown { .. }) if seen.is_some() => &unread,
            _ => return,
        };
        if r.conversation.is_some() && r.conversation != self.st.retired_conversation {
            let fresh_start_seen =
                self.st.conversation.is_none() && self.fresh_restart_unconfirmed();
            self.st.conversation = r.conversation.clone();
            if fresh_start_seen {
                self.restarted(true);
            }
        }
        let merged = fleet_api::screen::merge(profile.authority, r, seen.as_ref());
        if agentless {
            self.st.agent_state = Some(merged.state);
            return;
        }
        let last = self.st.activity.as_ref();
        if merged.source != fleet_api::screen::Source::Hook
            && matches!(screen, Some(Observation::Unknown { .. }))
            && last.is_some_and(|a| a.source == fleet_api::screen::Source::Screen)
        {
            return;
        }
        let same = self.st.agent_state == Some(merged.state)
            && last.is_some_and(|a| a.source == merged.source && a.rule == merged.rule);
        if !same {
            self.st.agent_state = Some(merged.state);
            self.st.activity = Some(fleet_api::screen::AgentActivity {
                source: merged.source,
                rule: merged.rule,
                at: self.now(),
            });
        }
    }

    fn wait_from(&mut self, from: Millis) -> bool {
        self.wait_for(from, RETRY_DELAY_MS)
    }

    fn wait_for(&mut self, from: Millis, delay: Millis) -> bool {
        let until = from.saturating_add(delay);
        if until > self.now() {
            self.st.not_before = Some(until);
            true
        } else {
            self.st.not_before = None;
            false
        }
    }

    fn intend(&mut self, key: String, action: Action) {
        self.st.loose_prompts.remove(&key);
        let profile = fleet_api::harness::profile(&self.spec.harness);
        match &action {
            Action::Brief { text, .. } if profile.prompt_hook() => {
                self.st
                    .prompts
                    .insert(key.clone(), fleet_api::signal::prompt_digest(text));
                if let Some(tag) = profile.payload.paste.as_deref() {
                    self.st.loose_prompts.insert(
                        key.clone(),
                        fleet_api::signal::loose_prompt_digest(text, tag),
                    );
                }
            }
            _ => {
                self.st.prompts.remove(&key);
            }
        }
        let intent = Intent {
            action: action.name().to_string(),
            at: self.now(),
            generation: self.generation,
        };
        self.st.intents.insert(key.clone(), intent);
        self.effects.push(Effect { key, action });
    }

    fn proof(&mut self, key: &str) -> Proof {
        let Some(digest) = self.st.prompts.get(key) else {
            return Proof::NotNeeded;
        };
        let since = self.st.intents.get(key).map_or(0, |i| i.at);
        let loose = self.st.loose_prompts.get(key);
        match &self.input.observations.signals {
            Some(Observation::Known(r))
                if r.prompted_since(digest, since)
                    || loose.is_some_and(|l| r.prompted_since(l, since)) =>
            {
                self.st.prompts.remove(key);
                self.st.loose_prompts.remove(key);
                Proof::Proven
            }
            Some(Observation::Known(_)) => {
                Proof::Awaiting("the agent's prompt hook has not reported it yet".into())
            }
            Some(Observation::Unknown { why }) => Proof::Awaiting(why.clone()),
            None => Proof::NotAsked,
        }
    }

    fn delivered(&mut self, key: &str) -> Option<Observation<bool>> {
        match self.proof(key) {
            Proof::NotNeeded => self.input.observations.brief_delivered.clone(),
            Proof::Proven => Some(Observation::Known(true)),
            Proof::Awaiting(why) => Some(Observation::Unknown { why }),
            Proof::NotAsked => None,
        }
    }

    fn needs_attention(&mut self, reason: &str, message: impl Into<String>) {
        self.cond(NEEDS_ATTENTION, ConditionStatus::True, reason, message);
    }

    fn forget_resolved_entries(&mut self) {
        let forgotten = inbox::forgotten(&self.st.inbox, &self.st.delivery_attempts);
        if forgotten.is_empty() {
            return;
        }
        for key in &forgotten {
            self.st.delivery_attempts.remove(key);
            self.st.intents.remove(key);
            self.st.receipts.remove(key);
            self.st.prompts.remove(key);
            self.st.loose_prompts.remove(key);
        }
        let still_given_up = self
            .st
            .inbox
            .iter()
            .find(|e| {
                self.st
                    .delivery_attempts
                    .get(&e.key)
                    .is_some_and(|a| a.failed)
            })
            .map(|e| format!("inbox entry {} from {}", e.key, e.from));
        let delivery_reasons = ["DeliveryUnconfirmed", "DeliveryFailed", "DeliveryQueued"];
        match still_given_up {
            None => {
                for reason in delivery_reasons {
                    self.settle_attention(reason);
                }
            }
            Some(what) => {
                let raised = condition(&self.st.conditions, NEEDS_ATTENTION)
                    .filter(|c| {
                        c.status == ConditionStatus::True
                            && delivery_reasons.contains(&c.reason.as_str())
                    })
                    .map(|c| c.reason.clone());
                if let Some(reason) = raised {
                    self.needs_attention(
                        &reason,
                        format!(
                            "{what} is given up on and stays in status.inbox; \
                             fleet send --retry or --drop resolves it"
                        ),
                    );
                }
            }
        }
    }

    fn settle_attention(&mut self, reason: &str) {
        if condition(&self.st.conditions, NEEDS_ATTENTION)
            .is_some_and(|c| c.status == ConditionStatus::True && c.reason == reason)
        {
            self.cond(NEEDS_ATTENTION, ConditionStatus::False, "Resolved", "");
        }
    }

    fn advance(&mut self) {
        self.forget_resolved_entries();
        match self.st.phase {
            Phase::Stalled => return,
            Phase::Concluded => return self.concluded(),
            _ => {}
        }
        if let Some(result) = self.st.result.as_ref().filter(|_| result_current(&self.st)) {
            let outcome = format!("{:?}", result.outcome);
            self.st.phase = Phase::Concluded;
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
        if self.makes_workspace() && self.st.workspace.is_none() && !self.materialize() {
            return;
        }
        let attempt = self.st.spawn_attempts + 1;
        let key = self.key(SPAWN, Some(attempt));
        self.st.spawn_attempts = attempt;
        self.st.not_before = None;
        self.st.receipts.remove(&key);
        self.st.phase = Phase::Spawning;
        self.cond(
            SPAWNED,
            ConditionStatus::Unknown,
            "Spawning",
            format!("attempt {attempt}"),
        );
        let host = self
            .st
            .workspace
            .as_ref()
            .map_or_else(|| self.spec.host.clone(), |w| w.host.clone());
        let spec = self.launch(host);
        self.intend(key, Action::Spawn(spec));
    }

    fn launch(&self, host: String) -> SpawnSpec {
        SpawnSpec {
            host,
            key: session_key(self.input.task),
            harness: self.spec.harness.clone(),
            workspace: self.spec.workspace.clone(),
            ready: self.st.workspace.clone(),
            model: self.spec.model.clone(),
            env: self.spec.env.clone(),
            environment_frozen: false,
            telemetry_args: Vec::new(),
        }
    }

    fn spawning(&mut self) {
        let key = self.key(SPAWN, Some(self.st.spawn_attempts));
        match self.st.receipts.get(&key).map(|r| r.outcome.clone()) {
            Some(Observation::Known(EffectOutcome::Spawned { session, .. })) => {
                self.adopt(session, "Spawned")
            }
            Some(Observation::Known(EffectOutcome::Failed { why })) => self.spawn_retry(&why),
            Some(Observation::Known(EffectOutcome::TrustUnmapped { harness })) => self
                .spawn_unmapped(
                    TRUST_UNMAPPED,
                    format!("the runtime knows no workspace-trust interface for harness {harness}"),
                ),
            Some(Observation::Known(other)) => {
                self.spawn_retry(&format!("unexpected outcome {other:?}"))
            }
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

    fn spawn_unmapped(&mut self, reason: &str, why: String) {
        self.st.spawn_attempts = self.st.spawn_attempts.saturating_sub(1);
        self.st.phase = Phase::Pending;
        self.wait_from(self.now());
        self.cond(SPAWNED, ConditionStatus::False, reason, why.clone());
        self.needs_attention(reason, why);
    }

    fn settle_spawn_gaps(&mut self) {
        self.settle_attention(TRUST_UNMAPPED);
    }

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
            self.st.phase = Phase::Pending;
            return;
        };
        if self.worker_gone() || self.lost(&session, true) {
            return;
        }
        if self.st.not_before.is_some_and(|t| t > self.now()) {
            return;
        }
        if self.spec.is_interactive() {
            self.cond(
                BRIEFED,
                ConditionStatus::False,
                INTERACTIVE,
                "no brief: the session waits for its user",
            );
            if self.restart_step(&session) || self.pause_state(&session) {
                return;
            }
            self.deliver(session.clone());
            return self.pause_step(session);
        }
        self.brief_step(session.clone());
        let sent = self.st.intents.contains_key(&brief_key(self.input.task));
        if matches!(self.st.phase, Phase::Spawned | Phase::Briefed)
            && sent
            && !self.brief_pending()
            && !self.brief_queued()
        {
            self.deliver(session);
        }
    }

    fn brief_queued(&self) -> bool {
        matches!(
            self.st
                .receipts
                .get(&brief_key(self.input.task))
                .map(|r| &r.outcome),
            Some(Observation::Known(EffectOutcome::Queued { .. }))
        )
    }

    fn brief_pending(&self) -> bool {
        let key = brief_key(self.input.task);
        self.st.phase == Phase::Spawned
            && self.st.intents.contains_key(&key)
            && (!known_receipt(&self.st, &key) || self.st.prompts.contains_key(&key))
    }

    fn brief_step(&mut self, session: SessionRef) {
        let key = brief_key(self.input.task);
        match self.st.receipts.get(&key).map(|r| r.outcome.clone()) {
            Some(Observation::Known(EffectOutcome::Delivered { .. })) => {
                return match self.proof(&key) {
                    Proof::NotNeeded | Proof::Proven => self.briefed_now(key, false),
                    Proof::Awaiting(why) => {
                        self.cond(BRIEFED, ConditionStatus::Unknown, AWAITING_PROMPT, why)
                    }
                    Proof::NotAsked => {}
                };
            }
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
            match self.delivered(&key) {
                Some(Observation::Known(true)) => {
                    self.st
                        .receipts
                        .insert(key.clone(), recovered_receipt(self.now()));
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
                let why = self.with_hook_report_hint(BRIEFED, why);
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
        let text = self
            .spec
            .assignment(&self.input.task.metadata.name, self.generation);
        self.intend(
            key,
            Action::Brief {
                session,
                text,
                inbox: None,
            },
        );
    }

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
                let why = self.with_hook_report_hint(WORKSPACE_TRUSTED, why);
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

    fn with_hook_report_hint(&self, type_: &str, why: String) -> String {
        if self.unknown_since(type_) + HOOK_REPORT_HINT_MS > self.now() {
            return why;
        }
        format!(
            "{why}; no hook report yet for harness {}: check `fleet doctor` for its integration",
            self.spec.harness
        )
    }

    fn unknown_since(&self, type_: &str) -> Millis {
        condition(&self.st.conditions, type_)
            .filter(|c| c.status == ConditionStatus::Unknown)
            .map_or_else(|| self.now(), |c| c.last_transition_time)
    }

    fn briefed_now(&mut self, key: String, recovered: bool) {
        self.st.brief_probe_failures = 0;
        self.st.not_before = None;
        self.settle_attention("BriefUnconfirmed");
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
            || self.lost(&session, true)
            || self.restart_step(&session)
            || self.pause_state(&session)
            || self.catch_up(session.clone())
        {
            return;
        }
        self.deliver(session.clone());
        self.pause_step(session);
    }

    fn concluded(&mut self) {
        if next_entry(&self.st).is_some() {
            self.st.idle_since = None;
        }
        if next_entry(&self.st).is_none() && self.st.catch_up.is_none() {
            let asked = view_asked(&self.st, self.now());
            let revive = asked || reviving(&self.st);
            let lost_known = matches!(self.input.observations.lost, Some(Observation::Known(_)));
            if self.input.observations.find.is_some() && (lost_known || !asked) {
                self.st.session_checked_at = Some(self.now());
                if self.worker_gone() {
                    return;
                }
            }
            if let Some(session) = self.st.session.clone()
                && self.lost(&session, revive)
            {
                return;
            }
            match self.st.session.clone() {
                Some(session) if watched(self.input.task, self.spec, &self.st) => {
                    if !self.restart_step(&session) && !self.pause_state(&session) {
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
        if self.worker_gone()
            || self.lost(&session, true)
            || self.restart_step(&session)
            || self.pause_state(&session)
        {
            return;
        }
        if self.st.not_before.is_some_and(|t| t > self.now()) {
            return;
        }
        if !self.catch_up(session.clone()) {
            self.deliver(session);
        }
    }

    fn deliver(&mut self, session: SessionRef) {
        let due = inbox::due(&self.st.inbox, &self.st.delivery_attempts);
        if due > MAX_INBOX {
            self.needs_attention(
                "InboxFull",
                format!("{due} entries still to deliver; appends are refused past {MAX_INBOX}"),
            );
        } else {
            self.settle_attention("InboxFull");
        }
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
        if answered_relay(&self.st, &entry.key) {
            self.record_delivery(&entry, false, None, false);
            return;
        }
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

    fn keyed_send(&mut self, session: SessionRef, key: &str, text: String, what: &str) -> Sent {
        match self.st.receipts.get(key).map(|r| r.outcome.clone()) {
            Some(Observation::Known(EffectOutcome::Delivered { via, may_hold, .. })) => {
                return match self.proof(key) {
                    Proof::NotNeeded | Proof::Proven => Sent::Delivered {
                        recovered: false,
                        via,
                        may_hold,
                    },
                    Proof::Awaiting(why) => {
                        self.record_prompt_boundary(key);
                        if self.another_prompt_reported_since(key) {
                            self.give_up_unconfirmed(key, what, &why)
                        } else if self.held_by_an_idle_agent(key) {
                            self.give_up_unconfirmed(key, what, HELD_BY_AN_IDLE_AGENT)
                        } else {
                            Sent::Pending
                        }
                    }
                    Proof::NotAsked => Sent::Pending,
                };
            }
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
            match self.delivered(key) {
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
                    if attempt.probe_failures >= MAX_ATTEMPTS {
                        return self.give_up_unconfirmed(key, what, &why);
                    }
                    self.wait_from(self.now());
                }
                None => {}
            }
            return Sent::Pending;
        }
        self.send_keyed(session, key, text, what)
    }

    fn record_prompt_boundary(&mut self, key: &str) {
        let reported = match (self.st.prompts.get(key), &self.input.observations.signals) {
            (Some(digest), Some(Observation::Known(r))) => latest_other_prompt_seq(r, digest),
            _ => return,
        };
        let attempt = self
            .st
            .delivery_attempts
            .entry(key.to_string())
            .or_default();
        attempt.other_prompt_seq.get_or_insert(reported);
    }

    fn another_prompt_reported_since(&self, key: &str) -> bool {
        let before = self
            .st
            .delivery_attempts
            .get(key)
            .and_then(|a| a.other_prompt_seq);
        match (
            before,
            self.st.prompts.get(key),
            &self.input.observations.signals,
        ) {
            (Some(before), Some(digest), Some(Observation::Known(r))) => {
                latest_other_prompt_seq(r, digest) > before
            }
            _ => false,
        }
    }

    fn held_by_an_idle_agent(&mut self, key: &str) -> bool {
        let reports_every_turn = fleet_api::harness::profile(&self.spec.harness).authority
            == fleet_api::harness::Authority::Full;
        let idle = reports_every_turn
            && matches!(
                &self.input.observations.signals,
                Some(Observation::Known(r)) if r.activity() == Some(Activity::Idle)
            );
        let now = self.now();
        let attempt = self
            .st
            .delivery_attempts
            .entry(key.to_string())
            .or_default();
        if !idle {
            attempt.idle_since = None;
            return false;
        }
        let since = *attempt.idle_since.get_or_insert(now);
        !self.wait_from(since)
    }

    fn give_up_unconfirmed(&mut self, key: &str, what: &str, why: &str) -> Sent {
        self.st
            .delivery_attempts
            .entry(key.to_string())
            .or_default()
            .failed = true;
        self.st.not_before = None;
        self.needs_attention(
            "DeliveryUnconfirmed",
            format!(
                "{what}: its delivery could not be read: {why}. It is not sent again; \
                 it stays in status.inbox and later entries are delivered past it"
            ),
        );
        Sent::GivenUp
    }

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
        let digest = fleet_api::signal::prompt_digest(&text);
        let other_prompt_seq = match &self.input.observations.signals {
            Some(Observation::Known(r)) => Some(latest_other_prompt_seq(r, &digest)),
            _ => None,
        };
        let attempt = self
            .st
            .delivery_attempts
            .entry(key.to_string())
            .or_default();
        attempt.sends += 1;
        attempt.probe_failures = 0;
        attempt.other_prompt_seq = other_prompt_seq;
        attempt.idle_since = None;
        self.st.not_before = None;
        self.st.receipts.remove(key);
        let inbox = match &self.input.observations.signals {
            Some(Observation::Known(r)) => {
                let conversation = r
                    .conversation
                    .as_deref()
                    .filter(|c| self.st.retired_conversation.as_deref() != Some(*c));
                fleet_api::harness::profile(&self.spec.harness).inbox(
                    r.inbox.as_deref(),
                    conversation,
                    &text,
                )
            }
            _ => None,
        };
        self.intend(
            key.to_string(),
            Action::Brief {
                session,
                text,
                inbox,
            },
        );
        Sent::Pending
    }

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
        self.st.prompts.remove(&key);
        self.st.loose_prompts.remove(&key);
        self.st.not_before = None;
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

    fn lost(&mut self, session: &SessionRef, revive: bool) -> bool {
        match self.input.observations.lost {
            Some(Observation::Known(true)) => {}
            Some(Observation::Known(false)) => {
                self.found_again();
                return false;
            }
            _ => return false,
        }
        if cannot_resume(&self.st) {
            return true;
        }
        self.cond(
            SESSION_LOST,
            ConditionStatus::True,
            "WindowGone",
            "the runtime holds the session, but its agent's window is gone",
        );
        if !self.spec.dispatch || !revive {
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
                if self.started_revives_left_lost() {
                    let why = format!(
                        "{MAX_ATTEMPTS} revives started its agent and left the session without a window: its agent cannot resume it"
                    );
                    self.cond(
                        SESSION_LOST,
                        ConditionStatus::True,
                        CANNOT_RESUME,
                        why.clone(),
                    );
                    self.needs_attention(CANNOT_RESUME, why);
                    return true;
                }
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
        let launch = self.launch(session.host.clone());
        self.intend(
            key,
            Action::Revive {
                session: session.clone(),
                conversation: self.st.conversation.clone(),
                launch,
            },
        );
        true
    }

    fn started_revives_left_lost(&self) -> bool {
        (0..MAX_ATTEMPTS).all(|back| {
            let Some(n) = self.st.revivals.checked_sub(back) else {
                return false;
            };
            self.st
                .receipts
                .get(&self.key(REVIVE, Some(n)))
                .is_some_and(|r| matches!(r.outcome, Observation::Known(EffectOutcome::Revived)))
        })
    }

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
        self.settle_attention(CANNOT_RESUME);
        self.settle_attention(REVIVE_FAILED);
        if self.st.phase != Phase::Concluded {
            self.owe_catch_up();
        }
    }

    fn owe_catch_up(&mut self) {
        if self.st.brief.is_some() && self.st.catch_up.is_none() {
            self.st.catch_ups += 1;
            self.st.catch_up = Some(self.key(CATCH_UP, Some(self.st.catch_ups)));
        }
    }

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

    fn catch_up_text(&self) -> String {
        let generation = self
            .st
            .brief
            .as_ref()
            .map_or(self.generation, |b| b.generation);
        if self.st.catch_up == Some(self.key(REBRIEF, Some(self.st.catch_ups))) {
            return self.rebrief_text(generation);
        }
        let head = "Fleet: your session was lost and restarted, and your conversation \
                    was resumed. Background shells, Monitors and loops you started did not \
                    survive; start again any you still need. Work you did not commit, push or declare may \
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

    fn rebrief_text(&self, generation: u64) -> String {
        let head = "Fleet: your session was restarted on request, in a new conversation: you \
                    remember nothing of the previous one. Background shells, Monitors and loops \
                    you started did not survive. Work you did not commit, push or declare may be \
                    lost. Read your worktree, the changes you declared and your inbox";
        let next = if self.st.phase == Phase::Concluded {
            format!(
                "then act on the messages that follow. Your result for generation \
                 {generation} is already recorded; your brief is repeated below."
            )
        } else {
            format!("then continue your brief, generation {generation}, repeated below.")
        };
        let questions: Vec<String> = self
            .st
            .declared
            .iter()
            .filter(|d| {
                d.get("kind").and_then(serde_json::Value::as_str)
                    == Some(fleet_api::talk::DECLARED_QUESTION)
            })
            .filter_map(|d| {
                let id = fleet_api::talk::declared_id(d)?;
                let text = d.get("text").and_then(serde_json::Value::as_str)?;
                (!self.st.answered.contains_key(id)).then(|| format!("- {id}: {text}"))
            })
            .collect();
        let open = if questions.is_empty() {
            String::new()
        } else {
            format!(
                "\n\nYou declared these Questions, and nobody has answered them yet:\n{}",
                questions.join("\n")
            )
        };
        format!(
            "{head}, {next}{open}\n\n{}",
            self.spec
                .assignment(&self.input.task.metadata.name, generation)
        )
    }

    fn restart_step(&mut self, session: &SessionRef) -> bool {
        let key = self.key(RESTART, Some(self.st.restarts));
        if effect_in_flight(&self.st, &key, RESTART) {
            let outcome = self.st.receipts.get(&key).map(|r| r.outcome.clone());
            self.st.intents.remove(&key);
            self.st.receipts.remove(&key);
            let fresh = self.st.restart_request_answered.is_some_and(|r| r.fresh);
            let failed = ConditionStatus::False;
            match outcome {
                Some(Observation::Known(EffectOutcome::Restarted)) => self.restarted(fresh),
                Some(Observation::Known(EffectOutcome::RestartUnsupported { why })) => {
                    self.keep_the_conversation();
                    self.cond(RESTARTED, failed, "Unsupported", why);
                }
                Some(Observation::Known(EffectOutcome::Failed { why })) => {
                    self.keep_the_conversation();
                    self.cond(RESTARTED, failed, "RestartFailed", why);
                }
                Some(Observation::Known(other)) => {
                    let why = format!("unexpected outcome {other:?}");
                    self.cond(RESTARTED, failed, "RestartFailed", why);
                }
                Some(Observation::Unknown { why }) => {
                    self.cond(
                        RESTARTED,
                        ConditionStatus::Unknown,
                        RESTART_UNCONFIRMED,
                        why,
                    );
                }
                None => self.cond(
                    RESTARTED,
                    ConditionStatus::Unknown,
                    RESTART_UNCONFIRMED,
                    "the restart's answer was never recorded",
                ),
            }
            return false;
        }
        let Some(request) = restart_requested(&self.st) else {
            return false;
        };
        if !self.spec.dispatch || !self.effects.is_empty() {
            return false;
        }
        let resumable = self.st.conversation.is_some()
            && fleet_api::harness::profile(&self.spec.harness).resumes();
        let request = RestartRequest {
            fresh: request.fresh || !resumable,
            ..request
        };
        self.st.restart_request_answered = Some(request);
        self.st.restarts += 1;
        self.st.idle_since = None;
        self.st.restart_retired = false;
        let conversation = if request.fresh {
            if let Some(old) = self.st.conversation.take() {
                self.st.retired_conversation = Some(old);
                self.st.restart_retired = true;
            }
            None
        } else {
            self.st.conversation.clone()
        };
        let key = self.key(RESTART, Some(self.st.restarts));
        let launch = self.launch(session.host.clone());
        self.intend(
            key,
            Action::Restart {
                session: session.clone(),
                conversation,
                launch,
            },
        );
        true
    }

    fn restarted(&mut self, fresh: bool) {
        let (reason, message) = if fresh {
            ("Fresh", "the agent started again on a new conversation")
        } else {
            ("Resumed", "the agent started again on its conversation")
        };
        self.cond(RESTARTED, ConditionStatus::True, reason, message);
        if fresh && self.st.brief.is_some() {
            if let Some(owed) = self.st.catch_up.take() {
                self.forget_send(&owed);
            }
            self.st.catch_ups += 1;
            self.st.catch_up = Some(self.key(REBRIEF, Some(self.st.catch_ups)));
        }
    }

    fn keep_the_conversation(&mut self) {
        if std::mem::take(&mut self.st.restart_retired) {
            self.st.conversation = self.st.retired_conversation.take();
        }
    }

    fn fresh_restart_unconfirmed(&self) -> bool {
        self.st.restart_request_answered.is_some_and(|r| r.fresh)
            && condition(&self.st.conditions, RESTARTED)
                .is_some_and(|c| c.reason == RESTART_UNCONFIRMED)
    }

    fn forget_send(&mut self, key: &str) {
        self.st.intents.remove(key);
        self.st.receipts.remove(key);
        self.st.delivery_attempts.remove(key);
        self.st.prompts.remove(key);
        self.st.loose_prompts.remove(key);
    }

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
                Some(Observation::Known(_)) => {
                    self.resume_again(&resume);
                    return true;
                }
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
                if (next_entry(&self.st).is_some() || wake_requested(&self.st))
                    && self.spec.dispatch
                {
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
            _ => paused,
        }
    }

    fn paused_by_fleet(&mut self, key: &str) {
        self.st.intents.remove(key);
        self.st.receipts.remove(key);
        self.st.idle_since = None;
        let (reason, message) = if paused_on_request(&self.st) {
            (
                "Requested",
                "Fleet paused the session because fleet pause asked for it",
            )
        } else {
            ("Idle", "Fleet paused the session while its agent was idle")
        };
        self.cond(PAUSED, ConditionStatus::True, reason, message);
    }

    fn pause_refused(&mut self, key: &str, why: String) {
        self.st.intents.remove(key);
        self.st.receipts.remove(key);
        self.st.idle_since = None;
        self.cond(PAUSED, ConditionStatus::False, "PauseFailed", why);
    }

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
        let launch = self.launch(session.host.clone());
        self.intend(
            key,
            Action::Resume {
                session: session.clone(),
                conversation: self.st.conversation.clone(),
                launch,
            },
        );
    }

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
    }

    fn pause_step(&mut self, session: SessionRef) {
        let o = &self.input.observations;
        let safe = pausable(self.spec, &self.st)
            && self.effects.is_empty()
            && self.delivery.is_none()
            && !self.st.not_before.is_some_and(|t| t > self.now())
            && o.lost != Some(Observation::Known(true))
            && o.paused == Some(Observation::Known(false));
        let requested = safe && pause_requested(&self.st);
        let idle = safe
            && self.spec.pause_after_ms.is_some()
            && o.activity == Some(Observation::Known(Activity::Idle));
        if !idle {
            self.st.idle_since = None;
        }
        if requested {
            self.st.pause_request_answered = self.st.pause_requested_at;
        } else if !self.idle_long_enough(idle) {
            return;
        }
        self.st.pauses += 1;
        let key = self.key(PAUSE, Some(self.st.pauses));
        self.intend(key, Action::Pause { session });
    }

    fn idle_long_enough(&mut self, idle: bool) -> bool {
        if !idle {
            return false;
        }
        let since = *self.st.idle_since.get_or_insert(self.input.now);
        let after = self.spec.pause_after_ms.unwrap_or(Millis::MAX);
        since.saturating_add(after) <= self.now()
    }

    fn release(&mut self) {
        self.st.phase = Phase::Releasing;
        if self.st.session.is_none() && self.st.spawn_attempts == 0 {
            self.cond(RELEASED, ConditionStatus::True, "NothingToRelease", "");
            self.let_go();
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
            self.let_go();
            return;
        };
        if self.st.release_attempts > 0 {
            let last = self.key(RELEASE, Some(self.st.release_attempts));
            match self.st.receipts.get(&last).cloned() {
                Some(Receipt {
                    outcome: Observation::Known(EffectOutcome::Released),
                    ..
                }) => {
                    let Some((reason, message)) = self.outbox_settled() else {
                        return;
                    };
                    self.cond(RELEASED, ConditionStatus::True, reason, message);
                    self.let_go();
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
                    let delay = retry_delay_after_failures(self.st.release_attempts);
                    if self.st.release_attempts >= MAX_ATTEMPTS {
                        self.needs_attention(
                            "ReleaseFailed",
                            format!(
                                "{} release attempts failed, retrying every {}s: {why}",
                                self.st.release_attempts,
                                delay / 1000
                            ),
                        );
                        self.cond(RELEASED, ConditionStatus::False, "ReleaseFailed", why);
                    } else {
                        self.cond(RELEASED, ConditionStatus::False, "ReleaseRetry", why);
                    }
                    match self.st.not_before {
                        Some(t) if t > self.now() => return,
                        Some(_) => self.st.not_before = None,
                        None => {
                            let at = receipt
                                .map(|r| r.at)
                                .or_else(|| self.st.intents.get(&last).map(|i| i.at))
                                .unwrap_or(0);
                            if self.wait_for(at, delay) {
                                return;
                            }
                        }
                    }
                }
            }
        }
        let gone = matches!(self.input.observations.find, Some(Observation::Known(None)));
        let grace_over = self.release_grace_over();
        if !gone {
            match self.input.observations.at_rest.clone() {
                Some(Observation::Known(true)) => {}
                Some(Observation::Known(false) | Observation::Unknown { .. }) if grace_over => {}
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

    fn outbox_settled(&mut self) -> Option<(&'static str, String)> {
        match self.input.observations.outbox_drained.clone() {
            None | Some(Observation::Known(true)) => Some(("Released", String::new())),
            Some(Observation::Known(false)) => {
                self.cond(
                    RELEASED,
                    ConditionStatus::False,
                    "OutboxDraining",
                    "the session's outbox has lines still to ingest",
                );
                None
            }
            Some(Observation::Unknown { why })
                if self
                    .outbox_unread_since()
                    .is_some_and(|since| since.saturating_add(RELEASE_GRACE_MS) <= self.now()) =>
            {
                Some((
                    "OutboxGivenUp",
                    format!(
                        "the outbox stayed unreadable past the release grace, and its unread lines are lost: {why}"
                    ),
                ))
            }
            Some(Observation::Unknown { why }) => {
                self.cond(RELEASED, ConditionStatus::Unknown, OUTBOX_UNREAD, why);
                None
            }
        }
    }

    fn outbox_unread_since(&self) -> Option<Millis> {
        condition(&self.st.conditions, RELEASED)
            .filter(|c| c.reason == OUTBOX_UNREAD)
            .map(|c| c.last_transition_time)
    }

    fn queue_phase_event(&mut self, before: &TaskStatus) {
        let (from, to) = (before.phase, self.st.phase);
        if from == to {
            return;
        }
        let changed: Vec<&Condition> = self
            .st
            .conditions
            .iter()
            .filter(|c| {
                condition(&before.conditions, &c.type_)
                    .is_none_or(|b| b.status != c.status || b.reason != c.reason)
            })
            .collect();
        let reason = changed
            .iter()
            .find(|c| c.type_ == phase_condition(to))
            .or(changed.first())
            .map(|c| c.reason.clone());
        let meta = &self.input.task.metadata;
        self.st
            .fleet_events
            .queue_phase(&meta.name, &meta.uid, from, to, reason.as_deref());
    }

    fn start_outbox_unread(&mut self, before: &TaskStatus) {
        let was_unread =
            condition(&before.conditions, RELEASED).is_some_and(|c| c.reason == OUTBOX_UNREAD);
        let now = self.now();
        if let Some(c) = self
            .st
            .conditions
            .iter_mut()
            .find(|c| c.type_ == RELEASED && c.reason == OUTBOX_UNREAD)
            && !was_unread
        {
            c.last_transition_time = now;
        }
    }

    fn release_grace_over(&self) -> bool {
        self.input
            .task
            .metadata
            .deletion_timestamp
            .is_some_and(|deleted| deleted.saturating_add(RELEASE_GRACE_MS) <= self.now())
    }

    fn makes_workspace(&self) -> bool {
        !self.spec.workspace.is_empty()
    }

    fn materialize(&mut self) -> bool {
        let attempt = self.st.materialize_attempts;
        let key = self.key(MATERIALIZE, Some(attempt));
        if attempt > 0 && self.st.intents.contains_key(&key) {
            match self.st.receipts.get(&key).map(|r| r.outcome.clone()) {
                Some(Observation::Known(EffectOutcome::Materialized { workspace, adopted })) => {
                    return self.workspace_ready(
                        *workspace,
                        if adopted { "Adopted" } else { "Materialized" },
                    );
                }
                Some(Observation::Known(EffectOutcome::Unsupported { url })) => {
                    self.st.intents.remove(&key);
                    self.st.receipts.remove(&key);
                    self.st.materialize_attempts = attempt - 1;
                    self.wait_from(self.now());
                    let why = format!(
                        "the runtime of host {} does not handle {url}",
                        self.spec.host
                    );
                    self.cond(
                        WORKSPACE_READY,
                        ConditionStatus::False,
                        UNSUPPORTED,
                        why.clone(),
                    );
                    self.needs_attention(UNSUPPORTED, why);
                    return false;
                }
                Some(Observation::Known(EffectOutcome::Failed { why })) => {
                    self.materialize_retry(&key, &why);
                    return false;
                }
                Some(Observation::Known(other)) => {
                    self.materialize_retry(&key, &format!("unexpected outcome {other:?}"));
                    return false;
                }
                Some(Observation::Unknown { .. }) | None => {
                    match self.input.observations.workspace.clone() {
                        Some(Observation::Known(Some(workspace))) => {
                            let receipt = Receipt {
                                action: MATERIALIZE.into(),
                                at: self.now(),
                                outcome: Observation::Known(EffectOutcome::Materialized {
                                    workspace: Box::new(workspace.clone()),
                                    adopted: true,
                                }),
                            };
                            self.st.receipts.insert(key, receipt);
                            return self.workspace_ready(workspace, "Adopted");
                        }
                        Some(Observation::Known(None)) => {
                            self.materialize_retry(&key, "no whole workspace holds the key");
                        }
                        Some(Observation::Unknown { why }) => {
                            self.cond(
                                WORKSPACE_READY,
                                ConditionStatus::Unknown,
                                "WorkspaceUnreadable",
                                why,
                            );
                        }
                        None => {}
                    }
                    return false;
                }
            }
        }
        let attempt = attempt + 1;
        let key = self.key(MATERIALIZE, Some(attempt));
        self.st.materialize_attempts = attempt;
        self.st.not_before = None;
        self.cond(
            WORKSPACE_READY,
            ConditionStatus::Unknown,
            "Materializing",
            format!("attempt {attempt}"),
        );
        let action = Action::Materialize {
            host: self.spec.host.clone(),
            key: session_key(self.input.task),
            urls: self.spec.workspace.urls().to_vec(),
        };
        self.intend(key, action);
        false
    }

    fn workspace_ready(
        &mut self,
        workspace: fleet_api::workspace::WorkspaceRef,
        reason: &str,
    ) -> bool {
        self.settle_attention(UNSUPPORTED);
        self.cond(
            WORKSPACE_READY,
            ConditionStatus::True,
            reason,
            workspace.dir().unwrap_or_default().to_string(),
        );
        self.st.workspace = Some(workspace);
        true
    }

    fn materialize_retry(&mut self, key: &str, why: &str) {
        self.st.intents.remove(key);
        self.settle_attention(UNSUPPORTED);
        let attempts = self.st.materialize_attempts;
        if attempts >= MAX_ATTEMPTS {
            self.st.phase = Phase::Stalled;
            self.cond(
                WORKSPACE_READY,
                ConditionStatus::False,
                "MaterializeFailed",
                why.to_string(),
            );
            self.cond(
                STALLED,
                ConditionStatus::True,
                "MaterializeAttemptsExhausted",
                format!("{attempts} attempts: {why}"),
            );
            self.needs_attention("Stalled", "materialize attempts exhausted");
            return;
        }
        self.wait_from(self.now());
        self.cond(
            WORKSPACE_READY,
            ConditionStatus::False,
            "MaterializeRetry",
            why.to_string(),
        );
    }

    fn let_go(&mut self) {
        let finalizer = Some(task::RELEASE_FINALIZER.to_string());
        let workspace = match self.workspace_to_clean() {
            Ok(Some(w)) => w,
            Ok(None) => {
                self.remove_finalizer = finalizer;
                return;
            }
            Err(()) => return,
        };
        let attempt = self.st.clean_attempts;
        if attempt > 0 {
            let key = self.key(CLEAN, Some(attempt));
            let receipt = self.st.receipts.get(&key).cloned();
            if let Some(Receipt {
                outcome: Observation::Known(EffectOutcome::Deleted),
                ..
            }) = receipt
            {
                self.st.workspace = None;
                self.cond(WORKSPACE_READY, ConditionStatus::False, "Cleaned", "");
                self.remove_finalizer = finalizer;
                return;
            }
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
            let delay = retry_delay_after_failures(attempt);
            if attempt >= MAX_ATTEMPTS {
                self.needs_attention(
                    CLEAN_FAILED,
                    format!(
                        "{attempt} workspace cleans failed, retrying every {}s: {why}",
                        delay / 1000
                    ),
                );
            }
            self.cond(WORKSPACE_READY, ConditionStatus::Unknown, "CleanRetry", why);
            match self.st.not_before {
                Some(t) if t > self.now() => return,
                Some(_) => self.st.not_before = None,
                None => {
                    let at = receipt
                        .map(|r| r.at)
                        .or_else(|| self.st.intents.get(&key).map(|i| i.at))
                        .unwrap_or(0);
                    if self.wait_for(at, delay) {
                        return;
                    }
                }
            }
        }
        let attempt = attempt + 1;
        self.st.clean_attempts = attempt;
        let key = self.key(CLEAN, Some(attempt));
        self.cond(
            WORKSPACE_READY,
            ConditionStatus::Unknown,
            "Cleaning",
            format!("attempt {attempt}"),
        );
        self.intend(key, Action::CleanWorkspace { workspace });
    }

    fn workspace_to_clean(&mut self) -> Result<Option<fleet_api::workspace::WorkspaceRef>, ()> {
        if let Some(w) = &self.st.workspace {
            return Ok(Some(w.clone()));
        }
        let key = self.key(MATERIALIZE, Some(self.st.materialize_attempts));
        match self.st.receipts.get(&key).map(|r| &r.outcome) {
            Some(Observation::Known(EffectOutcome::Materialized { workspace, .. })) => {
                return Ok(Some((**workspace).clone()));
            }
            Some(Observation::Known(_)) => return Ok(None),
            Some(Observation::Unknown { .. }) | None => {}
        }
        if !self.st.intents.contains_key(&key) {
            return Ok(None);
        }
        Ok(Some(fleet_api::workspace::WorkspaceRef {
            key: session_key(self.input.task),
            host: self.spec.host.clone(),
            dirs: Vec::new(),
        }))
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

fn pause_candidate(spec: &TaskSpec, st: &TaskStatus) -> bool {
    spec.pause_after_ms.is_some() && pausable(spec, st)
}

fn pausable(spec: &TaskSpec, st: &TaskStatus) -> bool {
    let in_pausable_phase = if spec.is_interactive() {
        st.phase == Phase::Spawned
    } else {
        st.brief.is_some()
            && (st.phase == Phase::Concluded || (st.phase == Phase::Briefed && !result_current(st)))
    };
    st.conversation.is_some()
        && fleet_api::harness::profile(&spec.harness).resumes()
        && spec.dispatch
        && in_pausable_phase
        && st.session.is_some()
        && next_entry(st).is_none()
        && st.catch_up.is_none()
        && st.revive_attempts == 0
        && st.resume_attempts == 0
        && ![PAUSED, SESSION_LOST, WORKER_GONE]
            .iter()
            .any(|c| is_true(&st.conditions, c))
}

fn in_flight(obj: &Object, st: &TaskStatus) -> bool {
    let m = &obj.metadata;
    let key = |action, n| effect_key(task::KIND, &m.name, &m.uid, action, Some(n));
    effect_in_flight(st, &key(PAUSE, st.pauses), PAUSE)
        || effect_in_flight(st, &key(RESUME, st.resumes), RESUME)
        || effect_in_flight(st, &key(RESTART, st.restarts), RESTART)
}

fn effect_in_flight(st: &TaskStatus, key: &str, action: &str) -> bool {
    st.intents.get(key).is_some_and(|i| i.action == action)
}

fn watched(obj: &Object, spec: &TaskSpec, st: &TaskStatus) -> bool {
    !obj.is_deleting()
        && (pause_candidate(spec, st)
            || (pause_requested(st) && pausable(spec, st))
            || in_flight(obj, st)
            || (spec.dispatch && (wake_requested(st) || restart_requested(st).is_some())))
}

fn activity_probe(
    input: &TaskInput<'_>,
    spec: &TaskSpec,
    st: &TaskStatus,
    session: &SessionRef,
    probes: &mut Vec<Probe>,
) {
    if input.task.is_deleting() || st.not_before.is_some_and(|t| t > input.now) {
        return;
    }
    probes.push(Probe::Activity {
        session: session.clone(),
    });
    let profile = fleet_api::harness::profile(&spec.harness);
    let hooks_say = profile.authority == fleet_api::harness::Authority::Full
        && st
            .activity
            .as_ref()
            .is_some_and(|a| a.source == fleet_api::screen::Source::Hook);
    if profile.screen_rules().is_some() && !hooks_say {
        probes.push(Probe::Screen {
            session: session.clone(),
        });
    }
}

fn materialize_in_flight(obj: &Object, st: &TaskStatus) -> bool {
    let m = &obj.metadata;
    let key = effect_key(
        task::KIND,
        &m.name,
        &m.uid,
        MATERIALIZE,
        Some(st.materialize_attempts),
    );
    st.workspace.is_none()
        && st.materialize_attempts > 0
        && st.intents.contains_key(&key)
        && !known_receipt(st, &key)
}

fn session_key(obj: &Object) -> String {
    task::session_key(&obj.metadata.name, &obj.metadata.uid)
}

fn brief_key(obj: &Object) -> String {
    let m = &obj.metadata;
    effect_key(task::KIND, &m.name, &m.uid, BRIEF, None)
}

fn result_current(st: &TaskStatus) -> bool {
    match (&st.result, &st.brief) {
        (Some(r), Some(b)) => r.generation == b.generation,
        (Some(_), None) => true,
        (None, _) => false,
    }
}

fn awaits_proof(st: &TaskStatus, key: &str) -> bool {
    st.prompts.contains_key(key)
        && st.intents.contains_key(key)
        && st.receipts.get(key).is_none_or(|r| {
            matches!(
                r.outcome,
                Observation::Unknown { .. } | Observation::Known(EffectOutcome::Delivered { .. })
            )
        })
}

fn proof_probe(session: &SessionRef, probes: &mut Vec<Probe>) {
    if !probes.iter().any(|p| matches!(p, Probe::Activity { .. })) {
        probes.push(Probe::Activity {
            session: session.clone(),
        });
    }
}

fn recovered_receipt(at: Millis) -> Receipt {
    Receipt {
        action: BRIEF.into(),
        at,
        outcome: Observation::Known(EffectOutcome::Delivered {
            already: true,
            via: None,
            may_hold: false,
        }),
    }
}

fn known_receipt(st: &TaskStatus, key: &str) -> bool {
    st.receipts
        .get(key)
        .is_some_and(|r| !r.outcome.is_unknown())
}

fn latest_other_prompt_seq(reported: &fleet_api::signal::Reported, digest: &str) -> u64 {
    reported
        .prompt_seqs
        .iter()
        .filter(|(d, _)| *d != digest)
        .map(|(_, seq)| *seq)
        .max()
        .unwrap_or_default()
}

fn wake_requested(st: &TaskStatus) -> bool {
    let paused_at = condition(&st.conditions, PAUSED)
        .filter(|c| c.status == ConditionStatus::True)
        .map(|c| c.last_transition_time);
    st.wake_requested_at
        .zip(paused_at)
        .is_some_and(|(asked, paused)| asked >= paused)
}

fn pause_requested(st: &TaskStatus) -> bool {
    st.pause_requested_at
        .is_some_and(|asked| st.pause_request_answered.is_none_or(|a| asked > a))
        && pause_request_since_last_transition(st, st.pause_requested_at)
}

fn restart_requested(st: &TaskStatus) -> Option<RestartRequest> {
    st.restart_requested
        .filter(|asked| st.restart_request_answered.is_none_or(|a| asked.at > a.at))
}

fn paused_on_request(st: &TaskStatus) -> bool {
    pause_request_since_last_transition(st, st.pause_request_answered)
}

fn pause_request_since_last_transition(st: &TaskStatus, asked: Option<Millis>) -> bool {
    let transition = condition(&st.conditions, PAUSED).map(|c| c.last_transition_time);
    asked.is_some_and(|asked| {
        transition.is_none_or(|t| asked > t) && st.wake_requested_at.is_none_or(|w| asked > w)
    })
}

fn answered_relay(st: &TaskStatus, key: &str) -> bool {
    st.gateway.as_ref().is_some_and(|gw| {
        gw.children
            .values()
            .any(|c| c.answered.iter().any(|k| k == key))
    })
}

fn next_entry(st: &TaskStatus) -> Option<&InboxEntry> {
    st.inbox.iter().find(|e| {
        !st.delivery_attempts
            .get(&e.key)
            .is_some_and(|a: &DeliveryAttempt| a.failed)
    })
}

pub fn session_recheck_due(st: &TaskStatus, now: Millis) -> bool {
    view_asked(st, now)
        || st
            .session_checked_at
            .is_none_or(|t| t.saturating_add(SESSION_RECHECK_MS) <= now)
}

fn view_asked(st: &TaskStatus, now: Millis) -> bool {
    st.wake_requested_at.is_some_and(|asked| {
        asked.saturating_add(task::VIEW_WAIT_MS) >= now
            && st.session_checked_at.is_none_or(|checked| asked >= checked)
    })
}

fn reviving(st: &TaskStatus) -> bool {
    st.revive_attempts > 0 && !cannot_resume(st)
}

fn cannot_resume(st: &TaskStatus) -> bool {
    condition(&st.conditions, SESSION_LOST)
        .is_some_and(|c| c.status == ConditionStatus::True && c.reason == CANNOT_RESUME)
}

pub fn due_at_rest(obj: &Object, now: Millis) -> bool {
    decode(obj).map_or(true, |(spec, st)| {
        session_recheck_due(&st, now)
            || reviving(&st)
            || watched(obj, &spec, &st)
            || !inbox::forgotten(&st.inbox, &st.delivery_attempts).is_empty()
    })
}

fn delivery_probes(st: &TaskStatus, session: &SessionRef, probes: &mut Vec<Probe>) {
    let Some(key) = &st.catch_up else {
        return inbox_probes(st, session, probes);
    };
    if !probes.iter().any(|p| matches!(p, Probe::Ready { .. })) {
        probes.push(Probe::Ready {
            session: session.clone(),
        });
    }
    if awaits_proof(st, key) {
        proof_probe(session, probes);
    } else if st.intents.contains_key(key) && !known_receipt(st, key) {
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
    if awaits_proof(st, &entry.key) {
        proof_probe(session, probes);
    } else if st.intents.contains_key(&entry.key) && !known_receipt(st, &entry.key) {
        probes.push(Probe::BriefDelivered {
            session: session.clone(),
            key: entry.key.clone(),
        });
    }
}
