//! The manager's spans and metrics, through the OpenTelemetry API alone: a
//! no-op until the CLI installs a provider, so a run without telemetry
//! records nothing and pays almost nothing.
//!
//! Each span is current while it runs, so a pass is a child of the command
//! that ran it, an object's reconcile a child of its pass, and an effect or
//! a forge poll a child of its reconcile. Attributes are an object's
//! identity, names of kinds and outcomes: never a brief, a message, an
//! environment value or an error's text.

use std::sync::OnceLock;

use fleet_api::forge::{ChangeObservation, Uncertainty};
use fleet_api::plan::{Action, Effect, EffectOutcome};
use fleet_api::{Millis, Object, Observation, StoreError};
use opentelemetry::metrics::{Counter, Gauge, Histogram};
use opentelemetry::trace::{Status, TraceContextExt, Tracer};
use opentelemetry::{Context, ContextGuard, KeyValue, global};

use crate::Step;

const SCOPE: &str = "fleet";

struct Instruments {
    reconcile: Histogram<f64>,
    effects: Counter<u64>,
    forge_calls: Counter<u64>,
    inbox: Gauge<u64>,
    amendments: Counter<u64>,
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

    fn set(&self, kv: KeyValue) {
        Context::current().span().set_attribute(kv);
    }
}

impl Drop for Current {
    fn drop(&mut self) {
        Context::current().span().end();
    }
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

/// One pass over every kind; records the Tasks' inbox depth as it starts.
pub(crate) fn pass(number: u32, tasks: &[Object]) -> Current {
    let depth = tasks
        .iter()
        .filter_map(|t| t.status.get("inbox").and_then(|i| i.as_array()))
        .map(|i| i.len() as u64)
        .sum();
    instruments().inbox.record(depth, &[]);
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
    let span = Current::start(format!("reconcile {}", obj.kind), attributes);
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
            EffectOutcome::Delivered { already: true }
            | EffectOutcome::MarkedReady { already: true } => "already",
            EffectOutcome::Delivered { .. } => "delivered",
            EffectOutcome::Released => "released",
            EffectOutcome::Revived => "revived",
            EffectOutcome::MarkedReady { .. } => "markedReady",
            EffectOutcome::Moved { .. } => "moved",
            EffectOutcome::Opened { .. } => "opened",
            EffectOutcome::Posted { .. } => "posted",
            EffectOutcome::Received { .. } => "received",
            EffectOutcome::Created { adopted: true, .. } => "adopted",
            EffectOutcome::Created { .. } => "created",
            EffectOutcome::WorkspaceBusy { .. } => "workspaceBusy",
            EffectOutcome::Failed { .. } => "failed",
            EffectOutcome::Unmapped { .. } => "unmapped",
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
