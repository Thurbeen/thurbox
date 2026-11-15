//! The manager's spans and metrics, through the OpenTelemetry API alone: a
//! no-op until the CLI installs a provider, so a run without telemetry
//! records nothing and pays almost nothing.
//!
//! Each span is current while it runs. A pass is a child of the command
//! that ran it. An object's reconcile is in the object's own trace, whose
//! id is derived from the object's uid, and links to its pass: every
//! reconcile of one Task, over every command and pass, is one trace, from
//! its spawn to its release. An effect or a forge poll is a child of its
//! reconcile, and a runtime call a child of its effect. Attributes are an object's
//! identity, names of kinds and outcomes: never a brief, a message, an
//! environment value or an error's text.

use std::sync::OnceLock;

use fleet_api::forge::{ChangeObservation, Uncertainty};
use fleet_api::plan::{Action, Effect, EffectOutcome};
use fleet_api::{Millis, Object, Observation, StoreError};
use opentelemetry::metrics::{Counter, Gauge, Histogram};
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
    inbox: Gauge<u64>,
    amendments: Counter<u64>,
    tasks: Gauge<u64>,
    sessions: Gauge<u64>,
}

/// Built on first use, after the CLI installed the providers.
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
            inbox: meter
                .u64_gauge("fleet.inbox.depth")
                .with_unit("{entry}")
                .with_description("Undelivered inbox entries over every Task, at a pass's start.")
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
        }
    })
}

/// A span current until dropped, ended on drop.
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

    /// A span in `obj`'s own trace, linked to the span current now (its
    /// pass). Without a valid current span, as when nothing is exported, it
    /// is an ordinary child, so a run without telemetry carries no context.
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

/// FNV-1a over `bytes`, 128 bits wide: stable across builds and releases,
/// which `std`'s hasher is not.
fn fnv1a(bytes: &[u8]) -> u128 {
    const OFFSET: u128 = 0x6c62_272e_07bb_0142_62b8_2175_6295_c58d;
    const PRIME: u128 = 0x0000_0000_0100_0000_0000_0000_0000_013b;
    bytes
        .iter()
        .fold(OFFSET, |h, b| (h ^ u128::from(*b)).wrapping_mul(PRIME))
}

/// The context every reconcile of `obj` starts under: a trace id and a
/// parent span id derived from its uid, which stays the object's for its
/// whole life. The parent stands for the object itself and is never
/// exported, so a trace view shows the reconciles as its top spans.
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

/// An object's identity, as every span about it carries.
fn identity(obj: &Object) -> Vec<KeyValue> {
    let m = &obj.metadata;
    vec![
        KeyValue::new("fleet.kind", obj.kind.clone()),
        KeyValue::new("fleet.name", m.name.clone()),
        KeyValue::new("fleet.uid", m.uid.clone()),
        KeyValue::new("fleet.generation", m.generation as i64),
    ]
}

/// Count one `fleet amend` by outcome: `appended`, or why it was refused.
pub(crate) fn amendment(outcome: &'static str) {
    instruments()
        .amendments
        .add(1, &[KeyValue::new("fleet.outcome", outcome)]);
}

/// Every Task phase and session state, so a gauge reports a zero rather
/// than keeping a count no Task holds any more.
const PHASES: &[&str] = &[
    "Pending",
    "Spawning",
    "Spawned",
    "Briefed",
    "Stalled",
    "Concluded",
    "Releasing",
];
const SESSION_STATES: &[&str] = &["Running", "Idle", "Paused", "Lost", "Gone"];

/// Record on `gauge` how many `tasks` hold each of `values`, as `of` reads
/// one, under the attribute `key`.
fn count<'a>(
    gauge: &Gauge<u64>,
    key: &'static str,
    values: &[&'static str],
    tasks: &'a [Object],
    of: impl Fn(&'a Object) -> Option<&'a str>,
) {
    for value in values {
        let n = tasks.iter().filter(|t| of(t) == Some(*value)).count() as u64;
        gauge.record(n, &[KeyValue::new(key, *value)]);
    }
}

/// One pass over every kind; records the Tasks' inbox depth, phases and
/// session states as it starts.
pub(crate) fn pass(number: u32, tasks: &[Object]) -> Current {
    let depth = tasks
        .iter()
        .filter_map(|t| t.status.get("inbox").and_then(|i| i.as_array()))
        .map(|i| i.len() as u64)
        .sum();
    let i = instruments();
    i.inbox.record(depth, &[]);
    // A Task with no status yet is Pending.
    count(&i.tasks, "fleet.phase", PHASES, tasks, |t| {
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
        SESSION_STATES,
        tasks,
        |t| t.status.get("sessionState").and_then(|v| v.as_str()),
    );
    Current::start(
        "reconcile pass".into(),
        vec![KeyValue::new("fleet.pass", i64::from(number))],
    )
}

/// Run one controller's reconcile of `obj` as the span `reconcile KIND`,
/// and record its duration and outcome.
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

/// The effect's kind: a Brief under the Task's brief key is the brief, any
/// other Brief delivers an inbox entry.
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

/// Perform one effect as the span `effect KIND`, and count it by outcome.
pub(crate) fn effect(
    obj: &Object,
    effect: &Effect,
    perform: impl FnOnce() -> Observation<EffectOutcome>,
) -> Observation<EffectOutcome> {
    let kind = effect_kind(effect);
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

/// Poll one change on a forge as the span `forge poll`, and count the call
/// by host and outcome. A throttle is the outcome `throttled`.
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
