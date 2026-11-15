//! Each runtime call as a span and a measured call, through the
//! OpenTelemetry API alone: a no-op until the CLI installs a provider.
//!
//! The call's span is a child of what is current (an effect, or a
//! reconcile's probe), and the runtime program receives it as
//! `TRACEPARENT`, so its own spans join the same trace. Attributes are the
//! runtime's name, the verb and whether the answer was Known: never a
//! request, an answer or an Unknown's text.

use std::sync::OnceLock;
use std::time::Instant;

use fleet_exec::Spec;
use opentelemetry::metrics::Histogram;
use opentelemetry::trace::{SpanKind, Status, TraceContextExt, Tracer};
use opentelemetry::{Context, ContextGuard, KeyValue, global};

const SCOPE: &str = "fleet";

/// The variables a runtime program never inherits from Fleet's own
/// environment: Fleet's service name is not the runtime's, and a
/// `TRACEPARENT` Fleet was started with is not the call's parent.
const NOT_INHERITED: &[&str] = &["OTEL_SERVICE_NAME", otel_env::TRACEPARENT];

fn duration() -> &'static Histogram<f64> {
    static DURATION: OnceLock<Histogram<f64>> = OnceLock::new();
    DURATION.get_or_init(|| {
        global::meter(SCOPE)
            .f64_histogram("fleet.runtime.call.duration")
            .with_unit("s")
            .with_description("One runtime protocol call, by runtime, verb and outcome.")
            .build()
    })
}

/// One call to a runtime, current until [`Call::end`].
pub(crate) struct Call {
    attributes: [KeyValue; 2],
    started: Instant,
    _guard: ContextGuard,
}

impl Call {
    pub(crate) fn start(runtime: &str, verb: &str) -> Call {
        let attributes = [
            KeyValue::new("fleet.runtime", runtime.to_string()),
            KeyValue::new("fleet.runtime.verb", verb.to_string()),
        ];
        let tracer = global::tracer(SCOPE);
        let span = tracer
            .span_builder(format!("runtime {verb}"))
            .with_kind(SpanKind::Client)
            .with_attributes(attributes.to_vec())
            .start(&tracer);
        Call {
            attributes,
            started: Instant::now(),
            _guard: Context::current_with_span(span).attach(),
        }
    }

    /// End the call: `known` is whether the runtime answered Known.
    pub(crate) fn end(self, known: bool) {
        let outcome = KeyValue::new("fleet.outcome", if known { "known" } else { "unknown" });
        let cx = Context::current();
        let span = cx.span();
        span.set_attribute(outcome.clone());
        if !known {
            span.set_status(Status::error("unknown"));
        }
        span.end();
        let [runtime, verb] = self.attributes;
        duration().record(
            self.started.elapsed().as_secs_f64(),
            &[runtime, verb, outcome],
        );
    }
}

/// Give the runtime program the current span as its `TRACEPARENT`, and none
/// of Fleet's own identity.
pub(crate) fn carry(spec: &mut Spec) {
    spec.env_remove
        .extend(NOT_INHERITED.iter().map(|v| v.to_string()));
    if let Some(parent) = otel_env::traceparent() {
        spec.env.push((otel_env::TRACEPARENT.to_string(), parent));
    }
}
