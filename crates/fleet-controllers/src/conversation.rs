use std::collections::{BTreeMap, BTreeSet};

use fleet_api::condition::{ConditionStatus, condition, set_condition, settle_transitions};
use fleet_api::conversation::{
    self, CONVERSATION_LABEL, CONVERSATION_UID_LABEL, ConversationSpec, ConversationStatus,
    EFFECTS_CAP, EffectRecord, MESSAGE_CAP, Phase, REFUSALS_CAP, ROUTE_LABEL, Retiring,
    SEEN_EVENTS_CAP, StoredMessage, Tie, TieState, Via, busy_key, busy_notice, react_key, react_of,
    refusal_key, retire_dir_key, retire_task_key, task_key, task_name, thread_dir,
};
use fleet_api::gateway::{
    Admission, AuthorKind, Capabilities, ConversationRef, Delivery, Event, GatewayDefaults,
    HISTORY_FIRST_READ, History, HistoryObservation, HistoryQuery, Marker, Message, RouteConfig,
    RouteWorkspace, Starter, Verified, admit, earns_refusal, starts_task,
};
use fleet_api::meta::{API_VERSION, Millis, Object, ObjectMeta};
use fleet_api::plan::{
    Action, CREATE_ACTION, Create, Effect, EffectOutcome, Exclusive, Intent, Plan, Probe, Receipt,
    task_of_key,
};
use fleet_api::store::FieldManager;
use fleet_api::talk::Mark;
use fleet_api::task;
use fleet_api::validate::typed;
use fleet_api::{Observation, StoreError};
use serde_json::json;

use crate::task::{MAX_ATTEMPTS, RETRY_DELAY_MS};

#[derive(Debug, Clone)]
pub struct ConversationInput<'a> {
    pub object: &'a Object,
    pub observations: ConversationObservations,
    pub now: Millis,
    pub defaults: GatewayDefaults,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ConversationObservations {
    pub parsed: Option<Observation<ConversationRef>>,
    pub history: Option<HistoryObservation>,
    pub task: Option<Observation<Option<Object>>>,
    pub opener: Option<Observation<Option<Object>>>,
    pub capabilities: Option<Observation<Capabilities>>,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct ConversationController;

pub mod conditions {
    pub const REACHABLE: &str = "Reachable";
    pub const TIED: &str = "Tied";
    pub const NEEDS_ATTENTION: &str = "NeedsAttention";
}
use conditions::*;

const ROUTE_UNKNOWN: &str = "RouteUnknown";
const MENTIONS_ONLY: &str = "MentionsOnly";
const TASK_CREATE_FAILED: &str = "TaskCreateFailed";
const WORKSPACE_BUSY: &str = "WorkspaceBusy";
const POST_FAILED: &str = "PostFailed";
const RETIRE_FAILED: &str = "RetireFailed";
const POST_UNCONFIRMED: &str = "PostUnconfirmed";

impl ConversationController {
    pub const KIND: &'static str = conversation::KIND;
    pub const MANAGER: FieldManager = FieldManager::ConversationController;

    pub fn due(object: &Object, now: Millis) -> bool {
        if object.is_deleting() {
            return false;
        }
        let Ok((_, st)) = decode(object) else {
            return true;
        };
        !st.received.is_empty()
            || st.not_before.is_none_or(|t| t <= now)
            || create_receipt_waiting(&object.metadata.uid, &st)
            || opened_unjudged(&st)
    }

    pub fn probes(&self, input: &ConversationInput<'_>) -> Result<Vec<Probe>, StoreError> {
        if input.object.is_deleting() {
            return Ok(Vec::new());
        }
        let (spec, st) = decode(input.object)?;
        let obs = &input.observations;
        let mut probes = Vec::new();
        if let Some(tie) = &st.task
            && matches!(tie.state, TieState::Created | TieState::Pending)
            && obs.task.is_none()
        {
            probes.push(Probe::Object {
                kind: task::KIND.to_string(),
                name: tie.name.clone(),
            });
        }
        if obs.opener.is_none()
            && st.task.is_none()
            && !unjudged(&st).is_empty()
            && let Some(name) = opener_of(&st)
        {
            probes.push(Probe::Object {
                kind: task::KIND.to_string(),
                name: name.to_string(),
            });
        }
        let reference = st
            .reference
            .clone()
            .or_else(|| obs.parsed.as_ref().and_then(|p| p.known().cloned()));
        let Some(reference) = reference else {
            return Ok(probes);
        };
        if obs.capabilities.is_none() && reactions_due(&st, input) {
            probes.push(Probe::Capabilities {
                account: spec.account.clone(),
            });
        }
        let query = history_due(&spec, &st, input).or_else(|| recovery_due(&st, obs));
        let asked = probes
            .iter()
            .any(|p| matches!(p, Probe::Capabilities { .. }));
        if !asked && query.is_none() && obs.capabilities.is_none() && has_refused_post(&st) {
            probes.push(Probe::Capabilities {
                account: spec.account.clone(),
            });
        }
        if let Some(query) = query {
            match &obs.capabilities {
                None if asked => {}
                None => probes.push(Probe::Capabilities {
                    account: spec.account.clone(),
                }),
                Some(Observation::Known(caps)) if caps.history && obs.history.is_none() => {
                    probes.push(Probe::History {
                        account: spec.account.clone(),
                        conversation: reference,
                        query,
                    });
                }
                Some(_) => {}
            }
        }
        Ok(probes)
    }

    pub fn reconcile(&self, input: &ConversationInput<'_>) -> Result<Plan, StoreError> {
        if input.object.is_deleting() {
            return Ok(Plan::default());
        }
        let (spec, before) = decode(input.object)?;
        let mut pass = Pass {
            input,
            spec: &spec,
            st: before.clone(),
            attention: Attention::carried(&before),
            retry_after: None,
            wake: None,
            post_wake: false,
            delete: false,
            creates: Vec::new(),
            effects: Vec::new(),
            tied_now: false,
            started: None,
        };
        pass.advance();
        settle_transitions(&mut pass.st.conditions, &before.conditions);
        pass.schedule();
        let status = if pass.st == before {
            None
        } else {
            Some(
                serde_json::to_value(&pass.st)
                    .map_err(|e| StoreError::Invalid(format!("Conversation status: {e}")))?,
            )
        };
        Ok(Plan {
            status,
            delete: pass.delete,
            creates: pass.creates,
            effects: pass.effects,
            ..Plan::default()
        })
    }
}

fn reactions_due(st: &ConversationStatus, input: &ConversationInput<'_>) -> bool {
    if input.defaults.reactions.is_none() {
        return false;
    }
    let waiting = st
        .task
        .as_ref()
        .is_some_and(|t| matches!(t.state, TieState::Pending | TieState::Busy));
    waiting
        || st.reactions.marks.values().any(|m| *m == Mark::Queued)
        || st.effects.keys().any(|k| is_react_key(k))
}

fn is_react_key(key: &str) -> bool {
    key.contains("/react/")
}

fn history_due(
    spec: &ConversationSpec,
    st: &ConversationStatus,
    input: &ConversationInput<'_>,
) -> Option<HistoryQuery> {
    let now = input.now;
    if st.observed_at.is_none() {
        return Some(HistoryQuery::After {
            id: None,
            limit: HISTORY_FIRST_READ,
        });
    }
    if st.phase != Phase::Tied {
        return None;
    }
    let active = st
        .last_message_at
        .is_some_and(|at| now.saturating_sub(at) <= input.defaults.active_ms);
    let poll = spec.poll_ms(input.defaults.poll_ms);
    let elapsed = st
        .observed_at
        .is_some_and(|at| now.saturating_sub(at) >= poll);
    if !(active && elapsed) {
        return None;
    }
    let newest = st
        .messages
        .iter()
        .max_by(|a, b| (a.at, &a.id).cmp(&(b.at, &b.id)))
        .map(|m| m.id.clone());
    Some(HistoryQuery::After {
        id: newest,
        limit: HISTORY_FIRST_READ,
    })
}

fn recovery_due(st: &ConversationStatus, obs: &ConversationObservations) -> Option<HistoryQuery> {
    let readable = match &obs.capabilities {
        Some(Observation::Known(caps)) => caps.history && caps.marker == Marker::Metadata,
        None => true,
        Some(Observation::Unknown { .. }) => false,
    };
    if !readable {
        return None;
    }
    unconfirmed_posts(st)
        .into_iter()
        .map(|(_, record)| record.intent.at)
        .min()
        .map(|at| HistoryQuery::Since { at })
}

fn unconfirmed_posts(st: &ConversationStatus) -> Vec<(&String, &EffectRecord)> {
    st.effects
        .iter()
        .filter(|(key, record)| {
            is_post_key(key)
                && !matches!(&record.receipt, Some(r) if !r.outcome.is_unknown())
                && find_post(st, key).is_none()
        })
        .collect()
}

fn has_refused_post(st: &ConversationStatus) -> bool {
    st.effects.iter().any(|(key, record)| {
        is_post_key(key)
            && matches!(
                &record.receipt,
                Some(r) if matches!(r.outcome, Observation::Known(EffectOutcome::Failed { .. }))
            )
    })
}

fn is_post_key(key: &str) -> bool {
    key.contains("/refuse/") || key.contains("/busy/")
}

fn find_post<'a>(st: &'a ConversationStatus, key: &str) -> Option<&'a StoredMessage> {
    let split = format!("{key}/");
    st.messages.iter().find(|m| {
        m.author.kind == AuthorKind::Fleet
            && m.key
                .as_deref()
                .is_some_and(|k| k == key || k.starts_with(&split))
    })
}

fn create_receipt_waiting(uid: &str, st: &ConversationStatus) -> bool {
    st.task
        .as_ref()
        .filter(|t| t.state == TieState::Pending)
        .and_then(|t| st.effects.get(&task_key(uid, t.n)))
        .is_some_and(|r| r.receipt.is_some())
}

fn opener_of(st: &ConversationStatus) -> Option<&str> {
    let starter = st.starter.as_ref()?;
    if starter.removed {
        return None;
    }
    task_of_key(starter.key.as_deref()?).map(|(name, _)| name)
}

fn opened_unjudged(st: &ConversationStatus) -> bool {
    st.task.is_none() && opener_of(st).is_some() && !unjudged(st).is_empty()
}

fn unjudged(st: &ConversationStatus) -> Vec<&StoredMessage> {
    st.messages
        .iter()
        .filter(|m| m.first_seq > st.judged)
        .collect()
}

fn decode(obj: &Object) -> Result<(ConversationSpec, ConversationStatus), StoreError> {
    let spec: ConversationSpec = typed(conversation::KIND, &obj.spec)?;
    let st: ConversationStatus = if obj.status.is_null() {
        ConversationStatus::default()
    } else {
        serde_json::from_value(obj.status.clone())
            .map_err(|e| StoreError::Invalid(format!("Conversation status: {e}")))?
    };
    Ok((spec, st))
}

struct Attention(BTreeMap<String, String>);

impl Attention {
    fn carried(st: &ConversationStatus) -> Attention {
        let mut open = BTreeMap::new();
        if let Some(c) = condition(&st.conditions, NEEDS_ATTENTION)
            && c.status == ConditionStatus::True
        {
            for line in c.message.lines() {
                if let Some((reason, text)) = line.split_once(' ') {
                    open.insert(reason.to_string(), text.to_string());
                }
            }
        }
        Attention(open)
    }

    fn raise(&mut self, reason: &str, text: impl Into<String>) {
        let text: String = text.into();
        self.0.insert(reason.to_string(), text.replace('\n', " "));
    }

    fn clear(&mut self, reason: &str) {
        self.0.remove(reason);
    }

    fn message(&self) -> String {
        self.0
            .iter()
            .map(|(reason, text)| format!("{reason} {text}"))
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn reason(&self) -> String {
        let reasons: BTreeSet<&str> = self.0.keys().map(String::as_str).collect();
        match reasons.len() {
            1 => reasons.into_iter().next().unwrap_or_default().to_string(),
            _ => "Multiple".to_string(),
        }
    }
}

struct Pass<'a, 'b> {
    input: &'a ConversationInput<'b>,
    spec: &'a ConversationSpec,
    st: ConversationStatus,
    attention: Attention,
    retry_after: Option<Millis>,
    wake: Option<Millis>,
    post_wake: bool,
    delete: bool,
    creates: Vec<Create>,
    effects: Vec<Effect>,
    tied_now: bool,
    started: Option<String>,
}

struct Fresh {
    message: Message,
    via: Via,
    edited_at: Option<Millis>,
}

fn delivered(via: &Via) -> bool {
    matches!(via.verified, Verified::Connection | Verified::Signature)
}

fn as_message(m: &StoredMessage, starter: Option<&Starter>) -> Message {
    Message {
        id: m.id.clone(),
        author: m.author.clone(),
        mentions_fleet: m.mentions_fleet,
        at: m.at,
        text: m.text.clone(),
        key: m.key.clone(),
        starts_thread: starter.is_some_and(|s| s.id.as_deref() == Some(&m.id)),
        files: m.files.clone(),
    }
}

impl Pass<'_, '_> {
    fn now(&self) -> Millis {
        self.input.now
    }

    fn uid(&self) -> &str {
        &self.input.object.metadata.uid
    }

    fn cond(&mut self, t: &str, s: ConditionStatus, reason: &str, message: impl Into<String>) {
        let (generation, now) = (self.input.object.metadata.generation, self.now());
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

    fn wake_at(&mut self, at: Millis) {
        self.wake = Some(self.wake.map_or(at, |w| w.min(at)));
    }

    fn advance(&mut self) {
        if self.st.reference.is_none() {
            match self.input.observations.parsed.clone() {
                Some(Observation::Known(reference)) => self.st.reference = Some(reference),
                Some(Observation::Unknown { why }) => {
                    self.cond(REACHABLE, ConditionStatus::Unknown, "AddressNotParsed", why);
                }
                None => {}
            }
        }
        self.tie();
        self.route();
        self.capabilities();
        let history = self.history();
        let spooled = std::mem::take(&mut self.st.received);
        self.number(history, spooled);
        self.settle_create();
        self.settle_posts();
        self.withdraw();
        self.react();
        self.retry_busy();
        self.admit();
        self.trim_effects();
        self.phase();
        self.retirement();
        self.conditions();
    }

    fn tied_task_deleting(&self) -> bool {
        let Some(tie) = &self.st.task else {
            return false;
        };
        tie.state == TieState::Created
            && matches!(
                &self.input.observations.task,
                Some(Observation::Known(Some(t))) if t.metadata.uid == tie.uid && t.is_deleting()
            )
    }

    fn retained_until(&self) -> Millis {
        let now = self.now();
        let orphaned_at = (self.st.phase == Phase::Orphaned).then(|| {
            match condition(&self.st.conditions, TIED) {
                Some(c) if c.status == ConditionStatus::True => now,
                Some(c) => c.last_transition_time,
                None => now,
            }
        });
        let since = [
            self.st.last_message_at,
            orphaned_at,
            self.input.object.metadata.creation_timestamp,
        ]
        .into_iter()
        .flatten()
        .max()
        .unwrap_or(now);
        since.saturating_add(self.input.defaults.retain_ms)
    }

    fn retirement(&mut self) {
        let waiting = self
            .st
            .task
            .as_ref()
            .is_some_and(|t| matches!(t.state, TieState::Busy | TieState::Pending));
        if waiting {
            return;
        }
        let idle = self.retained_until() <= self.now();
        if self.st.retiring.is_some() || idle {
            self.retire(idle);
        }
    }

    fn tie(&mut self) {
        let Some(tie) = self.st.task.clone() else {
            return;
        };
        if tie.state != TieState::Created {
            return;
        }
        let gone = match &self.input.observations.task {
            Some(Observation::Known(None)) => true,
            Some(Observation::Known(Some(obj))) => obj.metadata.uid != tie.uid,
            Some(Observation::Unknown { .. }) | None => false,
        };
        if gone && let Some(tie) = self.st.task.as_mut() {
            tie.state = TieState::Gone;
        }
    }

    fn route(&mut self) {
        match (&self.input.defaults.route, &self.input.defaults.brief) {
            (Some(_), Some(_)) => self.attention.clear(ROUTE_UNKNOWN),
            (Some(_), None) => self.attention.raise(
                ROUTE_UNKNOWN,
                format!(
                    "route {:?} has no brief loaded; nothing starts from this thread",
                    self.spec.route
                ),
            ),
            (None, _) => self.attention.raise(
                ROUTE_UNKNOWN,
                format!(
                    "route {:?} is no longer under [gateway.routes]; nothing starts from this thread",
                    self.spec.route
                ),
            ),
        }
    }

    fn capabilities(&mut self) {
        if let Some(Observation::Known(caps)) = &self.input.observations.capabilities {
            if caps.unmentioned {
                self.attention.clear(MENTIONS_ONLY);
            } else {
                self.attention.raise(
                    MENTIONS_ONLY,
                    "the platform delivers only mentions to Fleet, so only mentions reach the mirror",
                );
            }
        }
    }

    fn read_was_recovery(&self) -> bool {
        self.input.observations.history.is_some()
            && history_due(self.spec, &self.input.object_status(), self.input).is_none()
    }

    fn history(&mut self) -> Option<History> {
        let recovery = self.read_was_recovery();
        match self.input.observations.history.clone()? {
            HistoryObservation::Known(h) => {
                self.cond(REACHABLE, ConditionStatus::True, "Observed", "");
                if !recovery {
                    self.st.observed_at = Some(self.now());
                }
                Some(h)
            }
            HistoryObservation::Unsupported => {
                self.cond(
                    REACHABLE,
                    ConditionStatus::False,
                    "Unsupported",
                    "bots cannot read history on this platform",
                );
                self.st.observed_at = Some(self.now());
                None
            }
            HistoryObservation::Unknown {
                kind,
                why,
                retry_after,
            } => {
                self.cond(REACHABLE, ConditionStatus::Unknown, kind.reason(), why);
                self.retry_after = retry_after;
                None
            }
        }
    }

    fn number(&mut self, history: Option<History>, spooled: Vec<Delivery>) {
        let next = self.st.revision + 1;
        let untied = self
            .st
            .task
            .as_ref()
            .is_none_or(|t| t.state == TieState::Gone);
        let mut messages = std::mem::take(&mut self.st.messages);
        let mut fresh: Vec<Fresh> = Vec::new();
        let mut changed = false;
        if let Some(h) = history {
            for m in h.messages {
                match messages.iter_mut().find(|s| s.id == m.id) {
                    Some(stored) => {
                        changed |= refresh(stored, &m, next);
                    }
                    None => fresh.push(Fresh {
                        message: m,
                        via: Via {
                            verified: Verified::History,
                            received_at: self.now(),
                        },
                        edited_at: None,
                    }),
                }
            }
            if let Some(starter) = h.starter {
                changed |= self.starter(starter);
            }
        }
        for delivery in spooled {
            if self.st.seen_events.contains(&delivery.id) {
                continue;
            }
            match delivery.event {
                Event::Message { message, .. } => {
                    match messages.iter_mut().find(|s| s.id == message.id) {
                        Some(stored) => {
                            changed |= refresh(stored, &message, next);
                            if !delivered(&stored.via) {
                                stored.via = Via {
                                    verified: delivery.verified,
                                    received_at: delivery.received_at,
                                };
                                stored.mentions_fleet |= message.mentions_fleet;
                                if untied {
                                    stored.seq = next;
                                    stored.first_seq = next;
                                    changed = true;
                                }
                            }
                        }
                        None => {
                            if let Some(f) = fresh.iter_mut().find(|f| f.message.id == message.id) {
                                if !delivered(&f.via) {
                                    f.via = Via {
                                        verified: delivery.verified,
                                        received_at: delivery.received_at,
                                    };
                                    f.message.mentions_fleet |= message.mentions_fleet;
                                }
                            } else {
                                if message.starts_thread && self.st.starter.is_none() {
                                    self.st.starter = Some(Starter::message(&message));
                                    changed = true;
                                }
                                fresh.push(Fresh {
                                    message,
                                    via: Via {
                                        verified: delivery.verified,
                                        received_at: delivery.received_at,
                                    },
                                    edited_at: None,
                                });
                            }
                        }
                    }
                }
                Event::Edited {
                    id,
                    text,
                    edited_at,
                    ..
                } => {
                    if let Some(stored) = messages.iter_mut().find(|s| s.id == id) {
                        if stored.edited_at.is_none_or(|at| at < edited_at) {
                            stored.text = text;
                            stored.edited_at = Some(edited_at);
                            stored.seq = next;
                            changed = true;
                        }
                    } else if let Some(f) = fresh.iter_mut().find(|f| f.message.id == id)
                        && f.edited_at.is_none_or(|at| at < edited_at)
                    {
                        f.message.text = text;
                        f.edited_at = Some(edited_at);
                    }
                }
                Event::Removed { id, .. } => {
                    let before = messages.len() + fresh.len();
                    messages.retain(|s| s.id != id);
                    fresh.retain(|f| f.message.id != id);
                    if messages.len() + fresh.len() != before {
                        changed = true;
                    }
                    if self
                        .st
                        .starter
                        .as_ref()
                        .is_some_and(|s| s.id.as_deref() == Some(&id))
                    {
                        self.st.starter = Some(Starter::removed());
                        changed = true;
                    }
                }
            }
            self.st.seen_events.push(delivery.id);
        }
        if self.st.seen_events.len() > SEEN_EVENTS_CAP {
            let excess = self.st.seen_events.len() - SEEN_EVENTS_CAP;
            self.st.seen_events.drain(..excess);
        }
        if !fresh.is_empty() {
            changed = true;
            fresh.sort_by(|a, b| (a.message.at, &a.message.id).cmp(&(b.message.at, &b.message.id)));
            for (position, f) in fresh.into_iter().enumerate() {
                let m = f.message;
                if let Some(key) = &m.key
                    && let Some(starter) = self.st.starter.as_mut()
                    && starter.id.as_deref() == Some(&m.id)
                    && starter.key.is_none()
                {
                    starter.key = Some(key.clone());
                }
                messages.push(StoredMessage {
                    id: m.id,
                    seq: next,
                    first_seq: next,
                    position: position as u64,
                    at: m.at,
                    edited_at: f.edited_at,
                    author: m.author,
                    mentions_fleet: m.mentions_fleet,
                    text: m.text,
                    key: m.key,
                    files: m.files,
                    via: f.via,
                });
            }
        }
        messages.sort_by(|a, b| (a.at, &a.id).cmp(&(b.at, &b.id)));
        while messages.len() > MESSAGE_CAP {
            let dropped = messages.remove(0);
            self.st.dropped_before = self.st.dropped_before.max(dropped.seq);
            changed = true;
        }
        let newest = messages.iter().map(|m| m.at).max();
        if let Some(at) = newest {
            self.st.last_message_at = Some(self.st.last_message_at.map_or(at, |l| l.max(at)));
        }
        self.st.messages = messages;
        if changed {
            self.st.revision = next;
        }
    }

    fn starter(&mut self, starter: Starter) -> bool {
        let merged = match &self.st.starter {
            Some(current) if current.removed => return false,
            Some(current) if !starter.removed => Starter {
                key: starter.key.clone().or_else(|| current.key.clone()),
                ..starter
            },
            _ => starter,
        };
        if self.st.starter.as_ref() == Some(&merged) {
            return false;
        }
        self.st.starter = Some(merged);
        true
    }

    fn intent(&self, action: &str) -> Intent {
        Intent {
            action: action.to_string(),
            at: self.now(),
            generation: self.input.object.metadata.generation,
        }
    }

    fn renew_intent(&mut self, key: &str, action: &str, counted: bool) {
        let intent = self.intent(action);
        let record = self
            .st
            .effects
            .entry(key.to_string())
            .or_insert_with(|| EffectRecord {
                intent: intent.clone(),
                receipt: None,
                attempts: 0,
                recovery: 0,
            });
        record.intent = intent;
        record.receipt = None;
        if counted {
            record.attempts += 1;
        }
    }

    fn recover_receipt(&mut self, key: &str, action: &str, outcome: EffectOutcome) {
        let receipt = Receipt {
            action: action.to_string(),
            at: self.now(),
            outcome: Observation::Known(outcome),
        };
        if let Some(record) = self.st.effects.get_mut(key) {
            record.receipt = Some(receipt);
        }
    }

    fn nonce_window(&self) -> Option<Millis> {
        match &self.input.observations.capabilities {
            Some(Observation::Known(Capabilities {
                marker: Marker::Nonce { window_ms },
                ..
            })) => Some(*window_ms),
            _ => None,
        }
    }

    fn wake_for_receipt(&mut self) {
        let other_marker = matches!(
            &self.input.observations.capabilities,
            Some(Observation::Known(caps)) if !matches!(caps.marker, Marker::Nonce { .. })
        );
        if !other_marker {
            self.post_wake = true;
        }
    }

    fn resend_post(&mut self, key: &str, text: String) {
        let Some(reference) = self.st.reference.clone() else {
            return;
        };
        if let Some(record) = self.st.effects.get_mut(key) {
            record.receipt = None;
            record.attempts += 1;
        }
        self.effects.push(Effect {
            key: key.to_string(),
            action: Action::Post {
                account: self.spec.account.clone(),
                conversation: reference,
                text,
            },
        });
        self.wake_for_receipt();
    }

    fn plan_post(&mut self, key: &str, text: String, counted: bool) {
        let Some(reference) = self.st.reference.clone() else {
            return;
        };
        self.renew_intent(key, "post", counted);
        self.effects.push(Effect {
            key: key.to_string(),
            action: Action::Post {
                account: self.spec.account.clone(),
                conversation: reference,
                text,
            },
        });
        self.wake_for_receipt();
    }

    fn next_n(&self) -> u64 {
        let recorded = self
            .st
            .effects
            .keys()
            .filter_map(|k| {
                k.rsplit_once('/')
                    .filter(|(head, _)| head.ends_with("/task") || head.ends_with("/busy"))
                    .and_then(|(_, n)| n.parse::<u64>().ok())
            })
            .max()
            .unwrap_or(0);
        self.st.task.as_ref().map_or(0, |t| t.n).max(recorded) + 1
    }

    fn workspace(&self, route: &RouteConfig) -> task::Workspace {
        match &route.workspace {
            RouteWorkspace::Base(b) => {
                task::Workspace::at(thread_dir(&b.base, &self.input.object.metadata.name))
            }
            RouteWorkspace::Urls(w) => w.clone(),
        }
    }

    fn task_object(&self, name: &str, route: &RouteConfig, brief: &str) -> Object {
        Object {
            api_version: API_VERSION.to_string(),
            kind: task::KIND.to_string(),
            metadata: ObjectMeta {
                name: name.to_string(),
                labels: BTreeMap::from([
                    (
                        CONVERSATION_LABEL.to_string(),
                        self.input.object.metadata.name.clone(),
                    ),
                    (CONVERSATION_UID_LABEL.to_string(), self.uid().to_string()),
                    (ROUTE_LABEL.to_string(), self.spec.route.clone()),
                ]),
                ..ObjectMeta::default()
            },
            spec: json!({
                "configRef": {"name": route.task_config},
                "brief": brief,
                "workspace": self.workspace(route),
                "dispatch": true,
            }),
            status: serde_json::Value::Null,
        }
    }

    fn plan_create(&mut self, counted: bool) {
        let (Some(route), Some(brief)) = (
            self.input.defaults.route.clone(),
            self.input.defaults.brief.clone(),
        ) else {
            return;
        };
        let Some(tie) = self.st.task.clone() else {
            return;
        };
        let key = task_key(self.uid(), tie.n);
        self.renew_intent(&key, CREATE_ACTION, counted);
        if let Some(t) = self.st.task.as_mut() {
            t.state = TieState::Pending;
            t.uid.clear();
        }
        let object = self.task_object(&tie.name, &route, &brief);
        self.creates.push(Create {
            key,
            object,
            exclusive: route
                .workspace
                .base_dir()
                .and(self.workspace(&route).dir())
                .map(|dir| Exclusive { workspace: dir }),
        });
        let poll = self.spec.poll_ms(self.input.defaults.poll_ms);
        self.wake_at(self.now().saturating_add(poll));
    }

    fn start(&mut self, trigger: &str) {
        let n = self.next_n();
        self.st.task = Some(Tie {
            name: task_name(&self.input.object.metadata.name, n),
            uid: String::new(),
            n,
            state: TieState::Pending,
            trigger: Some(trigger.to_string()),
        });
        self.plan_create(true);
    }

    fn settle_create(&mut self) {
        let Some(tie) = self.st.task.clone() else {
            return;
        };
        if tie.state != TieState::Pending {
            return;
        }
        let key = task_key(self.uid(), tie.n);
        let Some(record) = self.st.effects.get(&key).cloned() else {
            self.plan_create(true);
            return;
        };
        match record.receipt.as_ref().map(|r| &r.outcome) {
            Some(Observation::Known(EffectOutcome::Created { uid, .. })) => {
                let uid = uid.clone();
                self.tied(&uid);
            }
            Some(Observation::Known(EffectOutcome::WorkspaceBusy { task })) => {
                let holder = task.clone();
                if let Some(t) = self.st.task.as_mut() {
                    t.state = TieState::Busy;
                }
                let dir = self
                    .input
                    .defaults
                    .route
                    .as_ref()
                    .and_then(|r| self.workspace(r).dir())
                    .unwrap_or_default();
                self.attention.raise(
                    WORKSPACE_BUSY,
                    format!(
                        "Task/{holder} has the thread's directory {dir}; the request waits for it"
                    ),
                );
                let notice = busy_key(self.uid(), tie.n);
                if !self.st.effects.contains_key(&notice) {
                    self.plan_post(&notice, busy_notice(&holder), true);
                }
            }
            Some(Observation::Known(EffectOutcome::Failed { why })) => {
                let why = why.clone();
                let at = record.receipt.as_ref().map_or(self.now(), |r| r.at);
                self.failed_create(&key, record.attempts, at, &why);
            }
            Some(Observation::Known(other)) => {
                let why = format!("the create answered {other:?}");
                let at = record.receipt.as_ref().map_or(self.now(), |r| r.at);
                self.failed_create(&key, record.attempts, at, &why);
            }
            Some(Observation::Unknown { .. }) | None => self.recover_create(&key, record.attempts),
        }
    }

    fn failed_create(&mut self, key: &str, attempts: u32, at: Millis, why: &str) {
        if attempts >= MAX_ATTEMPTS {
            self.attention.raise(
                TASK_CREATE_FAILED,
                format!("{attempts} creates of the thread's Task were refused: {why}"),
            );
            return;
        }
        let retry_at = at.saturating_add(RETRY_DELAY_MS);
        if self.now() >= retry_at {
            let _ = key;
            self.plan_create(true);
        } else {
            self.wake_at(retry_at);
        }
    }

    fn recover_create(&mut self, key: &str, attempts: u32) {
        match self.input.observations.task.clone() {
            Some(Observation::Known(Some(obj))) => {
                if obj.metadata.labels.get(CONVERSATION_UID_LABEL) == Some(&self.uid().to_string())
                {
                    let uid = obj.metadata.uid.clone();
                    self.recover_receipt(
                        key,
                        CREATE_ACTION,
                        EffectOutcome::Created {
                            uid: uid.clone(),
                            adopted: true,
                        },
                    );
                    self.tied(&uid);
                } else {
                    let why = format!("{} exists and was not created for this thread", obj.key());
                    self.recover_receipt(
                        key,
                        CREATE_ACTION,
                        EffectOutcome::Failed { why: why.clone() },
                    );
                    self.failed_create(key, attempts, self.now(), &why);
                }
            }
            Some(Observation::Known(None)) => {
                if attempts >= MAX_ATTEMPTS {
                    self.attention.raise(
                        TASK_CREATE_FAILED,
                        format!("{attempts} creates of the thread's Task left nothing behind"),
                    );
                } else {
                    self.plan_create(true);
                }
            }
            Some(Observation::Unknown { .. }) => {
                if let Some(record) = self.st.effects.get_mut(key) {
                    record.recovery += 1;
                }
                let poll = self.spec.poll_ms(self.input.defaults.poll_ms);
                self.wake_at(self.now().saturating_add(poll));
            }
            None => {
                let poll = self.spec.poll_ms(self.input.defaults.poll_ms);
                self.wake_at(self.now().saturating_add(poll));
            }
        }
    }

    fn tied(&mut self, uid: &str) {
        if let Some(t) = self.st.task.as_mut() {
            t.uid = uid.to_string();
            t.state = TieState::Created;
            self.started = t.trigger.take();
            self.tied_now = true;
        }
        self.attention.clear(WORKSPACE_BUSY);
        self.attention.clear(TASK_CREATE_FAILED);
    }

    fn settle_posts(&mut self) {
        let keys: Vec<String> = self
            .st
            .effects
            .keys()
            .filter(|k| is_post_key(k))
            .cloned()
            .collect();
        let recovery_read = self.read_was_recovery();
        let readable = match &self.input.observations.capabilities {
            Some(Observation::Known(caps)) => Some(caps.history && caps.marker == Marker::Metadata),
            _ => None,
        };
        let nonce_window = self.nonce_window();
        let mut any_unconfirmed = false;
        for key in keys {
            if self.effects.iter().any(|e| e.key == key) {
                continue;
            }
            let record = self.st.effects[&key].clone();
            match record.receipt.as_ref().map(|r| &r.outcome) {
                Some(Observation::Known(EffectOutcome::Posted { .. })) => {}
                Some(Observation::Known(EffectOutcome::Failed { why })) => {
                    let at = record.receipt.as_ref().map_or(self.now(), |r| r.at);
                    if record.attempts >= MAX_ATTEMPTS {
                        self.attention.raise(
                            POST_FAILED,
                            format!("{} attempts at {key} were refused: {why}", record.attempts),
                        );
                        continue;
                    }
                    let retry_at = at.saturating_add(RETRY_DELAY_MS);
                    if readable.is_none() {
                        let poll = self.spec.poll_ms(self.input.defaults.poll_ms);
                        self.wake_at(
                            retry_at.max(self.now().saturating_add(poll.min(RETRY_DELAY_MS))),
                        );
                        continue;
                    }
                    if let Some(window) = nonce_window
                        && retry_at >= record.intent.at.saturating_add(window)
                    {
                        self.attention.raise(
                            POST_FAILED,
                            format!(
                                "{key} was refused and its nonce window closed before a retry: {why}"
                            ),
                        );
                        continue;
                    }
                    if self.now() >= retry_at {
                        if let Some(text) = self.post_text(&key) {
                            if nonce_window.is_some() {
                                self.resend_post(&key, text);
                            } else {
                                self.plan_post(&key, text, true);
                            }
                        }
                    } else {
                        self.wake_at(retry_at);
                    }
                }
                Some(Observation::Known(other)) => {
                    let why = format!("the post answered {other:?}");
                    self.attention.raise(POST_FAILED, format!("{key}: {why}"));
                }
                Some(Observation::Unknown { .. }) | None => {
                    if let Some(found) = find_post(&self.st, &key) {
                        let outcome = EffectOutcome::Posted {
                            message: found.id.clone(),
                            at: found.at,
                        };
                        self.recover_receipt(&key, "post", outcome);
                        continue;
                    }
                    if let Some(window) = nonce_window {
                        // Inside its window the platform answers a resent nonce with what landed.
                        let open = self.now() < record.intent.at.saturating_add(window);
                        if open
                            && record.attempts < MAX_ATTEMPTS
                            && let Some(text) = self.post_text(&key)
                        {
                            self.resend_post(&key, text);
                        } else {
                            any_unconfirmed = true;
                        }
                        continue;
                    }
                    if readable == Some(false) {
                        any_unconfirmed = true;
                        continue;
                    }
                    let poll = self.spec.poll_ms(self.input.defaults.poll_ms);
                    self.wake_at(self.now().saturating_add(poll));
                    if readable.is_none() || !recovery_read {
                        continue;
                    }
                    let read = self.input.observations.history.clone();
                    if read.as_ref().is_some_and(|h| h.known().is_some())
                        && record.intent.at >= self.recovery_since()
                    {
                        if record.attempts >= MAX_ATTEMPTS {
                            self.attention.raise(
                                POST_FAILED,
                                format!(
                                    "{} attempts at {key} left no post behind",
                                    record.attempts
                                ),
                            );
                        } else if let Some(text) = self.post_text(&key) {
                            self.plan_post(&key, text, true);
                        }
                        continue;
                    }
                    if let Some(r) = self.st.effects.get_mut(&key) {
                        r.recovery += 1;
                    }
                }
            }
        }
        if any_unconfirmed {
            self.attention.raise(
                POST_UNCONFIRMED,
                "a post's outcome is unknown and the platform cannot find it again; a person decides",
            );
        } else {
            self.attention.clear(POST_UNCONFIRMED);
        }
    }

    fn recovery_since(&self) -> Millis {
        match recovery_due(&self.input.object_status(), &self.input.observations) {
            Some(HistoryQuery::Since { at }) => at,
            _ => Millis::MAX,
        }
    }

    fn post_text(&self, key: &str) -> Option<String> {
        let route = self.input.defaults.route.as_ref()?;
        if key.contains("/refuse/") {
            return Some(route.refusal().to_string());
        }
        let n: u64 = key.rsplit_once('/')?.1.parse().ok()?;
        let holder = self
            .st
            .effects
            .get(&task_key(self.uid(), n))
            .and_then(|r| r.receipt.as_ref())
            .and_then(|r| match &r.outcome {
                Observation::Known(EffectOutcome::WorkspaceBusy { task }) => Some(task.clone()),
                _ => None,
            })
            .unwrap_or_else(|| "another".to_string());
        Some(busy_notice(&holder))
    }

    fn withdraw(&mut self) {
        let Some(tie) = self.st.task.clone() else {
            return;
        };
        if tie.state != TieState::Busy {
            return;
        }
        let present = tie
            .trigger
            .as_deref()
            .is_some_and(|id| self.st.messages.iter().any(|m| m.id == id));
        if !present {
            self.st.task = None;
            self.attention.clear(WORKSPACE_BUSY);
        }
    }

    fn react(&mut self) {
        if self.input.defaults.reactions.is_none() || self.st.retiring.is_some() {
            self.st.reactions = Default::default();
            self.st.effects.retain(|k, _| !is_react_key(k));
            return;
        }
        let held: BTreeSet<String> = self.st.messages.iter().map(|m| m.id.clone()).collect();
        match &self.input.observations.capabilities {
            Some(Observation::Known(caps)) if caps.reactions => {}
            Some(Observation::Known(_)) => {
                self.st.reactions = Default::default();
                self.st.effects.retain(|k, _| !is_react_key(k));
                return;
            }
            _ => return,
        }
        let left: Vec<String> = self
            .st
            .reactions
            .marks
            .keys()
            .filter(|id| !held.contains(*id))
            .cloned()
            .collect();
        for id in left {
            if self.st.reactions.marks.remove(&id) == Some(Mark::Queued) {
                self.plan_react(&id, Mark::Queued, false);
            }
        }
        let tie = self.st.task.clone();
        match tie.as_ref().map(|t| t.state) {
            Some(TieState::Busy) => {
                for id in self.waiting_messages() {
                    if !self.st.reactions.marks.contains_key(&id) {
                        self.mark(&id, Mark::Queued);
                    }
                }
            }
            Some(TieState::Created) if self.tied_now => {
                let queued: Vec<String> = self.marked(Mark::Queued);
                for id in queued {
                    self.mark(&id, Mark::Working);
                }
                if let Some(id) = self.started.clone()
                    && held.contains(&id)
                    && !self.st.reactions.marks.contains_key(&id)
                {
                    self.mark(&id, Mark::Working);
                }
            }
            Some(TieState::Created) => {
                for id in self.marked(Mark::Queued) {
                    self.st.reactions.marks.remove(&id);
                    self.plan_react(&id, Mark::Queued, false);
                }
            }
            Some(TieState::Pending) => {}
            None | Some(TieState::Gone) => {
                for id in self.marked(Mark::Queued) {
                    self.st.reactions.marks.remove(&id);
                    self.plan_react(&id, Mark::Queued, false);
                }
                self.st.reactions.marks.retain(|_, m| *m != Mark::Working);
            }
        }
        self.retry_reactions();
    }

    fn marked(&self, mark: Mark) -> Vec<String> {
        self.st
            .reactions
            .marks
            .iter()
            .filter(|(_, m)| **m == mark)
            .map(|(id, _)| id.clone())
            .collect()
    }

    fn waiting_messages(&self) -> Vec<String> {
        let Some(trigger) = self.st.task.as_ref().and_then(|t| t.trigger.clone()) else {
            return Vec::new();
        };
        let Some(first) = self
            .st
            .messages
            .iter()
            .find(|m| m.id == trigger)
            .map(|m| m.first_seq)
        else {
            return Vec::new();
        };
        let Some(route) = self.input.defaults.route.as_ref() else {
            return vec![trigger];
        };
        let people = &self.input.defaults.people;
        let starter = self.st.starter.as_ref();
        self.st
            .messages
            .iter()
            .filter(|m| m.id == trigger || (m.first_seq > first && delivered(&m.via)))
            .filter(|m| {
                m.id == trigger
                    || admit(route, people, &as_message(m, starter)) == Admission::Admitted
            })
            .map(|m| m.id.clone())
            .collect()
    }

    fn mark(&mut self, id: &str, mark: Mark) {
        let before = self.st.reactions.marks.insert(id.to_string(), mark);
        self.plan_react(id, mark, true);
        if before == Some(Mark::Queued) && mark != Mark::Queued {
            self.plan_react(id, Mark::Queued, false);
        }
    }

    fn plan_react(&mut self, message: &str, mark: Mark, on: bool) {
        let Some(action) = self.react_action(message, mark, on) else {
            return;
        };
        let key = react_key(self.uid(), message, mark, on);
        self.renew_intent(&key, "react", true);
        self.effects.push(Effect { key, action });
    }

    fn react_action(&self, message: &str, mark: Mark, on: bool) -> Option<Action> {
        let names = self.input.defaults.reactions.as_ref()?;
        let reference = self.st.reference.clone()?;
        let reaction = match mark {
            Mark::Queued => &names.queued,
            Mark::Working => &names.working,
            Mark::Replied => &names.replied,
        };
        Some(Action::React {
            account: self.spec.account.clone(),
            conversation: reference,
            message: message.to_string(),
            reaction: reaction.clone(),
            on,
        })
    }

    fn retry_reactions(&mut self) {
        let uid = self.uid().to_string();
        let keys: Vec<String> = self
            .st
            .effects
            .keys()
            .filter(|k| is_react_key(k))
            .cloned()
            .collect();
        for key in keys {
            if self.effects.iter().any(|e| e.key == key) {
                continue;
            }
            let record = self.st.effects[&key].clone();
            let parsed = react_of(&uid, &key);
            let pointless = match &parsed {
                None => true,
                Some((id, mark, true)) => self.st.reactions.marks.get(id) != Some(mark),
                Some((id, Mark::Queued, false)) => {
                    self.st.reactions.marks.get(id) == Some(&Mark::Queued)
                }
                Some(_) => false,
            };
            let unsettled = match record.receipt.as_ref().map(|r| &r.outcome) {
                None | Some(Observation::Unknown { .. }) => true,
                Some(Observation::Known(EffectOutcome::Failed { .. })) => true,
                Some(Observation::Known(_)) => false,
            };
            if matches!(parsed, Some((_, Mark::Working, true))) {
                if pointless {
                    self.st.effects.remove(&key);
                }
                continue;
            }
            if pointless || !unsettled || record.attempts >= MAX_ATTEMPTS {
                self.st.effects.remove(&key);
                continue;
            }
            let since = record.receipt.as_ref().map_or(record.intent.at, |r| r.at);
            let retry_at = since.saturating_add(RETRY_DELAY_MS);
            if self.now() < retry_at {
                self.wake_at(retry_at);
                continue;
            }
            let Some((id, mark, on)) = parsed else {
                continue;
            };
            self.plan_react(&id, mark, on);
        }
    }

    fn retry_busy(&mut self) {
        let Some(tie) = self.st.task.clone() else {
            return;
        };
        if tie.state != TieState::Busy {
            return;
        }
        if self
            .input
            .object_status()
            .not_before
            .is_some_and(|t| t > self.now())
        {
            return;
        }
        self.plan_create(false);
    }

    fn opener_allows(&self) -> bool {
        let Some(starter) = &self.st.starter else {
            return true;
        };
        if starter.removed {
            return true;
        }
        let Some(key) = starter.key.as_deref() else {
            return starter.kind != Some(AuthorKind::Fleet);
        };
        let Some((_, uid)) = task_of_key(key) else {
            return starter.kind != Some(AuthorKind::Fleet);
        };
        match &self.input.observations.opener {
            Some(Observation::Known(Some(obj))) => obj.metadata.uid != uid,
            Some(Observation::Known(None)) => true,
            Some(Observation::Unknown { .. }) | None => false,
        }
    }

    fn tie_by_marker(&mut self) {
        if self.st.task.is_some() {
            return;
        }
        let Some(key) = self.st.starter.as_ref().and_then(|s| s.key.clone()) else {
            return;
        };
        let Some((name, uid)) = task_of_key(&key) else {
            return;
        };
        if let Some(Observation::Known(Some(obj))) = &self.input.observations.opener
            && obj.metadata.uid == uid
            && obj.metadata.name == name
        {
            self.st.task = Some(Tie {
                name: name.to_string(),
                uid: uid.to_string(),
                n: 0,
                state: TieState::Created,
                trigger: None,
            });
        }
    }

    fn admit(&mut self) {
        let (Some(route), Some(_)) = (
            self.input.defaults.route.clone(),
            self.input.defaults.brief.as_ref(),
        ) else {
            return;
        };
        if self.st.reference.is_none() {
            return;
        }
        let pending = unjudged(&self.st).into_iter().cloned().collect::<Vec<_>>();
        if pending.is_empty() {
            return;
        }
        if self.st.observed_at.is_none() {
            match &self.input.observations.capabilities {
                Some(Observation::Known(caps)) if !caps.history => {}
                _ => return,
            }
        }
        if self.st.task.is_none()
            && opener_of(&self.st).is_some()
            && !matches!(&self.input.observations.opener, Some(Observation::Known(_)))
        {
            return;
        }
        if self.tied_task_deleting() {
            return;
        }
        self.tie_by_marker();
        let people = self.input.defaults.people.clone();
        let starter = self.st.starter.clone();
        let mut started = false;
        for m in pending {
            if !delivered(&m.via) {
                continue;
            }
            let message = as_message(&m, starter.as_ref());
            let admission = admit(&route, &people, &message);
            if earns_refusal(self.spec.direct, admission, &message) {
                self.refuse(&m.author.id, &route);
                continue;
            }
            if started || admission != Admission::Admitted {
                continue;
            }
            let may_start = match self.st.task.as_ref().map(|t| t.state) {
                None => {
                    starts_task(&route, self.spec.direct, admission, &message)
                        && self.opener_allows()
                }
                Some(TieState::Gone) => true,
                Some(TieState::Pending | TieState::Busy | TieState::Created) => false,
            };
            if may_start {
                self.start(&m.id);
                started = true;
            }
        }
        self.st.judged = self.st.revision;
    }

    fn refuse(&mut self, author: &str, route: &RouteConfig) {
        if self.st.refusals.iter().any(|a| a == author) {
            return;
        }
        if self.st.refusals.len() >= REFUSALS_CAP {
            return;
        }
        self.st.refusals.push(author.to_string());
        let key = refusal_key(self.uid(), author);
        self.plan_post(&key, route.refusal().to_string(), true);
    }

    fn trim_effects(&mut self) {
        let mut settled: Vec<(String, Millis)> = self
            .st
            .effects
            .iter()
            .filter(|(_, r)| matches!(&r.receipt, Some(rc) if !rc.outcome.is_unknown()))
            .map(|(k, r)| (k.clone(), r.intent.at))
            .collect();
        if settled.len() <= EFFECTS_CAP {
            return;
        }
        settled.sort_by(|a, b| a.1.cmp(&b.1).then_with(|| a.0.cmp(&b.0)));
        let excess = settled.len() - EFFECTS_CAP;
        for (key, _) in settled.into_iter().take(excess) {
            self.st.effects.remove(&key);
        }
    }

    fn phase(&mut self) {
        self.st.phase = match self.st.task.as_ref().map(|t| t.state) {
            Some(TieState::Created) => Phase::Tied,
            Some(TieState::Gone) => Phase::Orphaned,
            Some(TieState::Pending | TieState::Busy) => self.st.phase,
            None => Phase::Untied,
        };
    }

    fn conditions(&mut self) {
        match (self.st.phase, self.st.task.clone()) {
            (Phase::Tied, Some(tie)) => {
                self.cond(TIED, ConditionStatus::True, "TaskKnown", tie.name);
            }
            (Phase::Orphaned, Some(tie)) => self.cond(
                TIED,
                ConditionStatus::False,
                "TaskGone",
                format!(
                    "Task/{} is readably gone; the next admitted message starts a new Task",
                    tie.name
                ),
            ),
            (_, Some(tie)) if tie.state == TieState::Pending => self.cond(
                TIED,
                ConditionStatus::False,
                "Starting",
                format!("Task/{} is being created", tie.name),
            ),
            (_, Some(tie)) if tie.state == TieState::Busy => self.cond(
                TIED,
                ConditionStatus::False,
                "Waiting",
                format!("Task/{} waits for the route's workspace", tie.name),
            ),
            _ => self.cond(TIED, ConditionStatus::False, "Untied", ""),
        }
        if !self.attention.0.is_empty() {
            let (reason, message) = (self.attention.reason(), self.attention.message());
            self.cond(NEEDS_ATTENTION, ConditionStatus::True, &reason, message);
        } else if condition(&self.st.conditions, NEEDS_ATTENTION)
            .is_some_and(|c| c.status == ConditionStatus::True)
        {
            self.cond(NEEDS_ATTENTION, ConditionStatus::False, "Resolved", "");
        }
    }

    fn schedule(&mut self) {
        let now = self.now();
        let poll = self.spec.poll_ms(self.input.defaults.poll_ms);
        let unknown_read = condition(&self.st.conditions, REACHABLE)
            .is_some_and(|c| c.status == ConditionStatus::Unknown);
        let retry = self.retry_after.filter(|_| unknown_read);
        let next_read = now.saturating_add(retry.unwrap_or(poll));
        let waiting = self
            .st
            .task
            .as_ref()
            .is_some_and(|t| matches!(t.state, TieState::Busy | TieState::Pending));
        let wake = self.wake;
        if waiting {
            self.st.not_before = Some(wake.unwrap_or(next_read));
            return;
        }
        let wake = if self.post_wake {
            Some(wake.map_or(now, |w| w.min(now)))
        } else {
            wake
        };
        let at_latest = |t: Millis| wake.map_or(t, |w| w.min(t));
        if self.st.phase == Phase::Tied {
            let next = if self.st.retiring.is_some() && self.post_wake {
                now
            } else {
                next_read
            };
            self.st.not_before = Some(at_latest(next));
            return;
        }
        let orphaned_at = (self.st.phase == Phase::Orphaned)
            .then(|| condition(&self.st.conditions, TIED).map(|c| c.last_transition_time))
            .flatten();
        let since = [
            self.st.last_message_at,
            orphaned_at,
            self.input.object.metadata.creation_timestamp,
        ]
        .into_iter()
        .flatten()
        .max()
        .unwrap_or(now);
        let until = since.saturating_add(self.input.defaults.retain_ms);
        if self.st.retiring.is_some() || until <= now {
            let next = if self.post_wake {
                now
            } else {
                now.saturating_add(poll)
            };
            self.st.not_before = Some(at_latest(next));
        } else if unknown_read {
            self.st.not_before = Some(at_latest(until.min(next_read)));
        } else {
            self.st.not_before = Some(at_latest(until));
        }
    }
}

impl Pass<'_, '_> {
    fn retire(&mut self, idle: bool) {
        let now = self.now();
        if let Some(r) = self.st.retiring {
            let key = retire_dir_key(self.uid(), r.at);
            let removed = self.st.effects.get(&key).is_some_and(|rec| {
                matches!(
                    rec.receipt.as_ref().map(|rc| &rc.outcome),
                    Some(Observation::Known(EffectOutcome::Deleted))
                )
            });
            if removed {
                self.st.dir = None;
            }
        }
        if self
            .st
            .retiring
            .is_some_and(|r| r.revision != self.st.revision)
        {
            self.st.retiring = None;
            self.attention.clear(RETIRE_FAILED);
        }
        let concluded = |t: &Object| t.status["phase"].as_str() == Some("Concluded");
        let tied = self.st.phase == Phase::Tied;
        let tie_uid = self
            .st
            .task
            .as_ref()
            .map(|t| t.uid.clone())
            .unwrap_or_default();
        let task = match &self.input.observations.task {
            Some(Observation::Known(Some(t))) if t.metadata.uid == tie_uid => Some(Some(t.clone())),
            Some(Observation::Known(_)) => Some(None),
            _ => None,
        };
        if self.st.retiring.is_none() {
            let eligible =
                !tied || matches!(&task, Some(Some(t)) if concluded(t) && !t.is_deleting());
            if !idle || !eligible {
                return;
            }
            self.st.retiring = Some(Retiring {
                at: now,
                revision: self.st.revision,
            });
        }
        let Some(retiring) = self.st.retiring else {
            return;
        };
        if tied {
            let Some(Some(t)) = task else {
                return;
            };
            if t.is_deleting() {
                return;
            }
            if !concluded(&t) {
                self.st.retiring = None;
                self.attention.clear(RETIRE_FAILED);
                return;
            }
            let key = retire_task_key(self.uid(), retiring.at);
            let action = Action::DeleteTask {
                name: t.metadata.name.clone(),
                uid: t.metadata.uid.clone(),
            };
            self.retire_step(&key, action);
            return;
        }
        if let Some(dir) = self.st.dir.clone() {
            let key = retire_dir_key(self.uid(), retiring.at);
            let removed = self.st.effects.get(&key).is_some_and(|r| {
                matches!(
                    r.receipt.as_ref().map(|rc| &rc.outcome),
                    Some(Observation::Known(EffectOutcome::Deleted))
                )
            });
            if !removed {
                let action = Action::RemoveDir {
                    host: dir.host,
                    path: dir.path,
                };
                self.retire_step(&key, action);
                return;
            }
            self.st.dir = None;
        }
        self.delete = self.creates.is_empty() && self.effects.is_empty();
    }

    fn retire_step(&mut self, key: &str, action: Action) {
        let name = action.name();
        let record = self.st.effects.get(key).cloned();
        let outcome = record
            .as_ref()
            .and_then(|r| r.receipt.as_ref())
            .map(|r| (r.outcome.clone(), r.at));
        let counted = match outcome {
            Some((Observation::Known(EffectOutcome::Deleted), _)) => {
                self.attention.clear(RETIRE_FAILED);
                self.attention.clear(WORKSPACE_BUSY);
                return;
            }
            Some((Observation::Known(EffectOutcome::WorkspaceBusy { task }), at)) => {
                self.attention.raise(
                    WORKSPACE_BUSY,
                    format!("Task/{task} names the thread's directory; it is removed once free"),
                );
                let poll = self.spec.poll_ms(self.input.defaults.poll_ms);
                if self.now() < at.saturating_add(poll) {
                    self.wake_at(at.saturating_add(poll));
                    return;
                }
                false
            }
            Some((Observation::Known(other), at)) => {
                let attempts = record.as_ref().map_or(0, |r| r.attempts);
                let why = match other {
                    EffectOutcome::Failed { why } => why,
                    o => format!("{o:?}"),
                };
                if attempts >= MAX_ATTEMPTS {
                    self.attention.raise(
                        RETIRE_FAILED,
                        format!("{attempts} attempts to {name} were refused: {why}"),
                    );
                    return;
                }
                if self.now() < at.saturating_add(RETRY_DELAY_MS) {
                    self.wake_at(at.saturating_add(RETRY_DELAY_MS));
                    return;
                }
                true
            }
            Some((Observation::Unknown { .. }, at)) => {
                let poll = self.spec.poll_ms(self.input.defaults.poll_ms);
                if self.now() < at.saturating_add(poll) {
                    self.wake_at(at.saturating_add(poll));
                    return;
                }
                true
            }
            None => true,
        };
        self.renew_intent(key, name, counted);
        self.effects.push(Effect {
            key: key.to_string(),
            action,
        });
        self.post_wake = true;
    }
}

impl ConversationInput<'_> {
    fn object_status(&self) -> ConversationStatus {
        decode(self.object).map(|(_, st)| st).unwrap_or_default()
    }
}

fn refresh(stored: &mut StoredMessage, seen: &Message, next: u64) -> bool {
    let mut changed = false;
    if stored.text != seen.text {
        stored.text = seen.text.clone();
        stored.seq = next;
        changed = true;
    }
    if stored.key.is_none() && seen.key.is_some() {
        stored.key = seen.key.clone();
        stored.seq = next;
        changed = true;
    }
    if stored.files != seen.files {
        stored.files = seen.files.clone();
        stored.seq = next;
        changed = true;
    }
    if seen.mentions_fleet && !stored.mentions_fleet {
        stored.mentions_fleet = true;
        changed = true;
    }
    changed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attention_lines_round_trip_through_the_condition_message() {
        let mut a = Attention(BTreeMap::new());
        a.raise(ROUTE_UNKNOWN, "route eng left the config");
        let mut st = ConversationStatus::default();
        set_condition(
            &mut st.conditions,
            NEEDS_ATTENTION,
            ConditionStatus::True,
            &a.reason(),
            a.message(),
            1,
            1,
        );
        assert_eq!(Attention::carried(&st).message(), a.message());
        assert_eq!(a.reason(), ROUTE_UNKNOWN);
        a.raise(MENTIONS_ONLY, "only mentions");
        assert_eq!(a.reason(), "Multiple");
        a.clear(ROUTE_UNKNOWN);
        assert_eq!(a.reason(), MENTIONS_ONLY);
        a.raise(WORKSPACE_BUSY, "two\nlines");
        assert!(!a.message().contains("two\nlines"), "one line per reason");
    }

    #[test]
    fn post_keys_and_task_numbers_are_read_back_from_effect_keys() {
        assert!(is_post_key("conversation/c7/refuse/U1"));
        assert!(is_post_key("conversation/c7/busy/2"));
        assert!(!is_post_key("conversation/c7/task/2"));
        let mut st = ConversationStatus::default();
        assert!(unjudged(&st).is_empty());
        st.effects.insert(
            "conversation/c7/refuse/U1".into(),
            EffectRecord {
                intent: Intent {
                    action: "post".into(),
                    at: 1,
                    generation: 1,
                },
                receipt: None,
                attempts: 1,
                recovery: 0,
            },
        );
        assert_eq!(unconfirmed_posts(&st).len(), 1);
        assert_eq!(
            recovery_due(&st, &ConversationObservations::default()),
            Some(HistoryQuery::Since { at: 1 })
        );
    }
}
