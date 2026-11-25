use std::sync::OnceLock;
use std::time::Instant;

use fleet_exec::Spec;
use opentelemetry::metrics::Histogram;
use opentelemetry::trace::{SpanKind, Status, TraceContextExt, Tracer};
use opentelemetry::{Context, ContextGuard, KeyValue, global};

const SCOPE: &str = "fleet";

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

pub(crate) fn carry(spec: &mut Spec) {
    spec.env_remove
        .extend(NOT_INHERITED.iter().map(|v| v.to_string()));
    spec.env.extend(otel_env::child_env());
    if let Some(parent) = otel_env::traceparent() {
        spec.env.push((otel_env::TRACEPARENT.to_string(), parent));
    }
}
