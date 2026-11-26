use std::collections::BTreeMap;
use std::sync::{Mutex, OnceLock};

use fleet_api::forge::{ChangeObservation, Uncertainty};
use fleet_api::plan::{Action, Effect, EffectOutcome};
use fleet_api::session::SessionState;
use fleet_api::task::Phase;
use fleet_api::{Millis, Object, Observation, StoreError};
use opentelemetry::metrics::{AsyncInstrument, Counter, Gauge, Histogram, ObservableGauge};
use opentelemetry::trace::{
    Link, SpanContext, SpanId, Status, TraceContextExt, TraceFlags, TraceId, TraceState, Tracer,
};
use opentelemetry::{Context, ContextGuard, KeyValue, global};

use crate::Step;

const SCOPE: &str = "fleet";

struct Instruments {
    reconcile: Histogram<f64>,
    effects: Counter<u64>,
    forge_calls: Counter<u64>,
    _inbox: ObservableGauge<u64>,
    amendments: Counter<u64>,
    tasks: Gauge<u64>,
    sessions: Gauge<u64>,
    manager: Gauge<u64>,
}

fn instruments() -> &'static Instruments {
    static INSTRUMENTS: OnceLock<Instruments> = OnceLock::new();
    INSTRUMENTS.get_or_init(|| {
        let meter = global::meter(SCOPE);
        Instruments {
            reconcile: meter
                .f64_histogram("fleet.reconcile.duration")
                .with_unit("s")
                .with_description("One object's reconcile: probes, decision, writes and effects.")
                .build(),
            effects: meter
                .u64_counter("fleet.effects")
                .with_unit("{effect}")
                .with_description("Effects performed, by kind and outcome.")
                .build(),
            forge_calls: meter
                .u64_counter("fleet.forge.calls")
                .with_unit("{call}")
                .with_description("Change polls sent to a forge, by host and outcome.")
                .build(),
            _inbox: meter
                .u64_observable_gauge("fleet.inbox.depth")
                .with_unit("{entry}")
                .with_description(
                    "Each Task's undelivered inbox entries, pending or given up, at a pass's start.",
                )
                .with_callback(observe_inbox)
                .build(),
            amendments: meter
                .u64_counter("fleet.task.amendments")
                .with_unit("{amendment}")
                .with_description("fleet amend calls, by outcome.")
                .build(),
            tasks: meter
                .u64_gauge("fleet.tasks")
                .with_unit("{task}")
                .with_description("Tasks in each phase, at a pass's start.")
                .build(),
            sessions: meter
                .u64_gauge("fleet.sessions")
                .with_unit("{session}")
                .with_description("Tasks' sessions in each state, at a pass's start.")
                .build(),
            manager: meter
                .u64_gauge("fleet.manager.info")
                .with_unit("{manager}")
                .with_description("This manager process, and whether it holds the manager lease.")
                .build(),
        }
    })
}

pub(crate) struct Current {
    _guard: ContextGuard,
}

impl Current {
    fn start(name: String, attributes: Vec<KeyValue>) -> Current {
        let tracer = global::tracer(SCOPE);
        let span = tracer
            .span_builder(name)
            .with_attributes(attributes)
            .start(&tracer);
        Current {
            _guard: Context::current_with_span(span).attach(),
        }
    }

    fn start_in_trace_of(obj: &Object, name: String, attributes: Vec<KeyValue>) -> Current {
        let current = Context::current();
        let pass = current.span().span_context().clone();
        if !pass.is_valid() {
            return Current::start(name, attributes);
        }
        let parent = Context::new().with_remote_span_context(object_context(obj));
        let tracer = global::tracer(SCOPE);
        let span = tracer
            .span_builder(name)
            .with_attributes(attributes)
            .with_links(vec![Link::with_context(pass)])
            .start_with_context(&tracer, &parent);
        Current {
            _guard: parent.with_span(span).attach(),
        }
    }

    fn set(&self, kv: KeyValue) {
        Context::current().span().set_attribute(kv);
    }
}

impl Drop for Current {
    fn drop(&mut self) {
        Context::current().span().end();
    }
}

fn fnv1a(bytes: &[u8]) -> u128 {
    const OFFSET: u128 = 0x6c62_272e_07bb_0142_62b8_2175_6295_c58d;
    const PRIME: u128 = 0x0000_0000_0100_0000_0000_0000_0000_013b;
    bytes
        .iter()
        .fold(OFFSET, |h, b| (h ^ u128::from(*b)).wrapping_mul(PRIME))
}

fn object_context(obj: &Object) -> SpanContext {
    let hash = fnv1a(obj.metadata.uid.as_bytes()).max(1);
    let span = (hash as u64).max(1);
    SpanContext::new(
        TraceId::from_bytes(hash.to_be_bytes()),
        SpanId::from_bytes(span.to_be_bytes()),
        TraceFlags::SAMPLED,
        true,
        TraceState::default(),
    )
}

fn identity(obj: &Object) -> Vec<KeyValue> {
    let m = &obj.metadata;
    vec![
        KeyValue::new("fleet.kind", obj.kind.clone()),
        KeyValue::new("fleet.name", m.name.clone()),
        KeyValue::new("fleet.uid", m.uid.clone()),
        KeyValue::new("fleet.generation", m.generation as i64),
    ]
}

pub(crate) fn amendment(outcome: &'static str) {
    instruments()
        .amendments
        .add(1, &[KeyValue::new("fleet.outcome", outcome)]);
}

fn names<T: serde::Serialize>(values: &[T]) -> Vec<String> {
    values
        .iter()
        .filter_map(|v| serde_json::to_value(v).ok())
        .filter_map(|v| v.as_str().map(str::to_owned))
        .collect()
}

fn phases() -> &'static [String] {
    static PHASES: OnceLock<Vec<String>> = OnceLock::new();
    PHASES.get_or_init(|| names(Phase::ALL))
}

fn session_states() -> &'static [String] {
    static STATES: OnceLock<Vec<String>> = OnceLock::new();
    STATES.get_or_init(|| names(SessionState::ALL))
}

fn count<'a>(
    gauge: &Gauge<u64>,
    key: &'static str,
    values: &[String],
    tasks: &'a [Object],
    of: impl Fn(&'a Object) -> Option<&'a str>,
) {
    for value in values {
        let n = tasks
            .iter()
            .filter(|t| of(t) == Some(value.as_str()))
            .count() as u64;
        gauge.record(n, &[KeyValue::new(key, value.clone())]);
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Engine {
    pub version: String,
    pub host: String,
}

static ENGINE: OnceLock<Engine> = OnceLock::new();

pub fn identify(engine: Engine) {
    let _ = ENGINE.set(engine);
}

pub fn manager_lease(held: bool) {
    let Some(engine) = ENGINE.get() else {
        return;
    };
    for lease in [held, !held] {
        instruments().manager.record(
            u64::from(lease == held),
            &[
                KeyValue::new("fleet.manager.version", engine.version.clone()),
                KeyValue::new("process.pid", i64::from(std::process::id())),
                KeyValue::new("host.name", engine.host.clone()),
                KeyValue::new("fleet.manager.lease", lease),
            ],
        );
    }
}

fn inbox_states(task: &Object) -> (u64, u64) {
    let given_up = |key: &serde_json::Value| {
        key.as_str()
            .and_then(|k| task.status["deliveryAttempts"].get(k))
            .is_some_and(|a| a["failed"] == true)
    };
    let (failed, pending): (Vec<_>, Vec<_>) = task.status["inbox"]
        .as_array()
        .into_iter()
        .flatten()
        .partition(|entry| given_up(&entry["key"]));
    (pending.len() as u64, failed.len() as u64)
}

#[derive(Debug, Clone, Copy, Default)]
struct Backlog {
    pending: u64,
    failed: u64,
    gone: bool,
}

static BACKLOGS: Mutex<BTreeMap<String, Backlog>> = Mutex::new(BTreeMap::new());

fn backlogs() -> std::sync::MutexGuard<'static, BTreeMap<String, Backlog>> {
    BACKLOGS.lock().unwrap_or_else(|e| e.into_inner())
}

fn inbox_depth(tasks: &[Object]) {
    let mut backlogs = backlogs();
    for backlog in backlogs.values_mut() {
        *backlog = Backlog {
            gone: true,
            ..Backlog::default()
        };
    }
    for task in tasks {
        let (pending, failed) = inbox_states(task);
        backlogs.insert(
            task.metadata.name.clone(),
            Backlog {
                pending,
                failed,
                gone: false,
            },
        );
    }
}

fn observe_inbox(observer: &dyn AsyncInstrument<u64>) {
    let mut backlogs = backlogs();
    for (name, backlog) in backlogs.iter() {
        for (state, n) in [("pending", backlog.pending), ("failed", backlog.failed)] {
            observer.observe(
                n,
                &[
                    KeyValue::new("fleet.name", name.clone()),
                    KeyValue::new("fleet.inbox.state", state),
                ],
            );
        }
    }
    backlogs.retain(|_, backlog| !backlog.gone);
}

pub(crate) fn pass(number: u32, tasks: &[Object]) -> Current {
    let i = instruments();
    inbox_depth(tasks);
    count(&i.tasks, "fleet.phase", phases(), tasks, |t| {
        Some(
            t.status
                .get("phase")
                .and_then(|v| v.as_str())
                .unwrap_or("Pending"),
        )
    });
    count(
        &i.sessions,
        "fleet.session.state",
        session_states(),
        tasks,
        |t| t.status.get("sessionState").and_then(|v| v.as_str()),
    );
    Current::start(
        "reconcile pass".into(),
        vec![KeyValue::new("fleet.pass", i64::from(number))],
    )
}

pub(crate) fn reconcile(
    controller: &'static str,
    obj: &Object,
    now: impl Fn() -> Millis,
    run: impl FnOnce() -> Result<Step, StoreError>,
) -> Result<Step, StoreError> {
    let mut attributes = identity(obj);
    attributes.push(KeyValue::new("fleet.controller", controller));
    let span = Current::start_in_trace_of(obj, format!("reconcile {}", obj.kind), attributes);
    let started = now();
    let step = run();
    let outcome = match &step {
        Ok(s) if s.conflict => "conflict",
        Ok(s) if s.wrote > 0 || s.effects > 0 => "changed",
        Ok(_) => "unchanged",
        Err(StoreError::LeaseLost(_)) => "leaseLost",
        Err(_) => "error",
    };
    span.set(KeyValue::new("fleet.outcome", outcome));
    if step.is_err() {
        Context::current().span().set_status(Status::error(outcome));
    }
    let seconds = now().saturating_sub(started) as f64 / 1000.0;
    instruments().reconcile.record(
        seconds,
        &[
            KeyValue::new("fleet.controller", controller),
            KeyValue::new("fleet.outcome", outcome),
        ],
    );
    step
}

fn effect_kind(effect: &Effect) -> &'static str {
    match &effect.action {
        Action::Brief { .. } if !effect.key.ends_with("/brief") => "deliver",
        action => action.name(),
    }
}

fn effect_outcome(outcome: &Observation<EffectOutcome>) -> &'static str {
    match outcome {
        Observation::Unknown { .. } => "unknown",
        Observation::Known(o) => match o {
            EffectOutcome::Spawned { adopted: true, .. } => "adopted",
            EffectOutcome::Spawned { .. } => "spawned",
            EffectOutcome::Delivered { already: true, .. }
            | EffectOutcome::MarkedReady { already: true }
            | EffectOutcome::Reacted { already: true } => "already",
            EffectOutcome::Delivered { .. } => "delivered",
            EffectOutcome::Queued { .. } => "queued",
            EffectOutcome::Released => "released",
            EffectOutcome::Revived => "revived",
            EffectOutcome::Paused => "paused",
            EffectOutcome::Resumed => "resumed",
            EffectOutcome::Restarted => "restarted",
            EffectOutcome::RestartUnsupported { .. } => "restartUnsupported",
            EffectOutcome::MarkedReady { .. } => "markedReady",
            EffectOutcome::Moved { .. } => "moved",
            EffectOutcome::Opened { .. } => "opened",
            EffectOutcome::Posted { .. } => "posted",
            EffectOutcome::Reacted { .. } => "reacted",
            EffectOutcome::Received { .. } => "received",
            EffectOutcome::Created { adopted: true, .. }
            | EffectOutcome::Materialized { adopted: true, .. } => "adopted",
            EffectOutcome::Materialized { .. } => "materialized",
            EffectOutcome::Created { .. } => "created",
            EffectOutcome::WorkspaceBusy { .. } => "workspaceBusy",
            EffectOutcome::Deleted => "deleted",
            EffectOutcome::Failed { .. } => "failed",
            EffectOutcome::Unsupported { .. } => "unsupported",
            EffectOutcome::TrustUnmapped { .. } => "trustUnmapped",
        },
    }
}

pub(crate) fn effect(
    obj: &Object,
    effect: &Effect,
    perform: impl FnOnce() -> Observation<EffectOutcome>,
) -> Observation<EffectOutcome> {
    effect_named(obj, effect_kind(effect), perform)
}

pub(crate) fn effect_named(
    obj: &Object,
    kind: &'static str,
    perform: impl FnOnce() -> Observation<EffectOutcome>,
) -> Observation<EffectOutcome> {
    let mut attributes = identity(obj);
    attributes.push(KeyValue::new("fleet.effect", kind));
    let span = Current::start(format!("effect {kind}"), attributes);
    let outcome = perform();
    let name = effect_outcome(&outcome);
    span.set(KeyValue::new("fleet.outcome", name));
    instruments().effects.add(
        1,
        &[
            KeyValue::new("fleet.effect", kind),
            KeyValue::new("fleet.outcome", name),
        ],
    );
    outcome
}

fn uncertainty(kind: Uncertainty) -> &'static str {
    match kind {
        Uncertainty::Timeout => "timeout",
        Uncertainty::Incomplete => "incomplete",
        Uncertainty::Throttled => "throttled",
        Uncertainty::Unreadable => "unreadable",
        Uncertainty::NotLoggedIn => "notLoggedIn",
        Uncertainty::Refused => "refused",
        Uncertainty::Unavailable => "unavailable",
    }
}

pub(crate) fn forge_poll(
    obj: &Object,
    host: &str,
    poll: impl FnOnce() -> ChangeObservation,
) -> ChangeObservation {
    let mut attributes = identity(obj);
    attributes.push(KeyValue::new("fleet.forge.host", host.to_string()));
    let span = Current::start("forge poll".into(), attributes);
    let answer = poll();
    let outcome = match &answer {
        ChangeObservation::Known(Some(_)) => "known",
        ChangeObservation::Known(None) => "absent",
        ChangeObservation::Unknown { kind, .. } => uncertainty(*kind),
    };
    span.set(KeyValue::new("fleet.outcome", outcome));
    instruments().forge_calls.add(
        1,
        &[
            KeyValue::new("fleet.forge.host", host.to_string()),
            KeyValue::new("fleet.outcome", outcome),
        ],
    );
    answer
}
