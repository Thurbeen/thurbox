//! OpenTelemetry export from the standard OTEL_* variables, the same in
//! every program the repository ships (`fleet`, `runtime-mux`,
//! `workspace-git`, the thurbox interface): the SDK and its OTLP exporter,
//! installed as the process's global providers when the variables ask for
//! them, and nothing at all otherwise. docs/observability.md is the
//! interface this crate implements.
//!
//! Every other crate records through the `opentelemetry` API alone, which
//! does nothing until this crate installs a provider. A signal is exported
//! when its endpoint (`OTEL_EXPORTER_OTLP_ENDPOINT`, or the signal's own
//! `OTEL_EXPORTER_OTLP_{TRACES,METRICS,LOGS}_ENDPOINT`) is set, its
//! `OTEL_{TRACES,METRICS,LOGS}_EXPORTER` is unset or `otlp`, and
//! `OTEL_SDK_DISABLED` is not `true`. The transport is OTLP over HTTP with
//! protobuf; the exporter itself reads the headers, timeout and compression
//! variables, and the resource reads `OTEL_SERVICE_NAME` and
//! `OTEL_RESOURCE_ATTRIBUTES`.
//!
//! Trace context crosses a process boundary as the W3C `TRACEPARENT`
//! environment variable: [`traceparent`] writes it for a child, and
//! [`root`] starts a program's root span under the one it was given.
//!
//! Export never decides anything: spans, points and records are batched on
//! the SDK's own threads into bounded queues that drop when full, a failed
//! export is dropped, and shutdown waits at most [`SHUTDOWN_BOUND`].

use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use opentelemetry::trace::{
    SpanContext, SpanId, Status, TraceContextExt, TraceFlags, TraceId, TraceState, Tracer,
};
use opentelemetry::{global, Context, ContextGuard, Key};

/// The attribute type every caller passes, so a program needs no
/// `opentelemetry` dependency of its own to name one.
pub use opentelemetry::KeyValue;
use opentelemetry_otlp::{MetricExporter, SpanExporter};
use opentelemetry_sdk::metrics::SdkMeterProvider;
use opentelemetry_sdk::trace::SdkTracerProvider;
use opentelemetry_sdk::Resource;

#[cfg(feature = "logs")]
pub use opentelemetry_sdk::logs::SdkLoggerProvider;

/// The longest a program waits at exit for its last batches to leave.
pub const SHUTDOWN_BOUND: Duration = Duration::from_secs(2);

/// The environment variable that carries trace context to a child process
/// (W3C Trace Context, as the OpenTelemetry environment carrier spells it).
pub const TRACEPARENT: &str = "TRACEPARENT";

/// The one protocol the exporter speaks.
const PROTOCOL: &str = "http/protobuf";

/// The installed providers, flushed by [`Telemetry::shutdown`]; all `None`
/// when nothing is exported.
#[derive(Debug, Default)]
pub struct Telemetry {
    traces: Option<SdkTracerProvider>,
    metrics: Option<SdkMeterProvider>,
    #[cfg(feature = "logs")]
    logs: Option<SdkLoggerProvider>,
}

/// Which signals the environment asks to export.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct Signals {
    traces: bool,
    metrics: bool,
    logs: bool,
}

/// The signals a program built with this crate can export: logs only with
/// the `logs` feature, so a program that records none reads no logs
/// variable.
fn kinds() -> &'static [&'static str] {
    if cfg!(feature = "logs") {
        &["traces", "metrics", "logs"]
    } else {
        &["traces", "metrics"]
    }
}

/// Read the signals from the variables `var` answers. An unsupported
/// protocol or exporter is an error, so a misconfiguration is reported
/// rather than silently exporting nothing.
fn signals(var: impl Fn(&str) -> Option<String>) -> Result<Signals, String> {
    let set = |name: &str| var(name).filter(|v| !v.trim().is_empty());
    if set("OTEL_SDK_DISABLED").is_some_and(|v| v.trim().eq_ignore_ascii_case("true")) {
        return Ok(Signals::default());
    }
    let signal = |name: &str| -> Result<bool, String> {
        if !kinds().contains(&name) {
            return Ok(false);
        }
        let upper = name.to_ascii_uppercase();
        let exporter_var = format!("OTEL_{upper}_EXPORTER");
        match set(&exporter_var).as_deref().map(str::trim) {
            None | Some("otlp") => {}
            Some("none") => return Ok(false),
            Some(other) => {
                return Err(format!(
                    "{exporter_var}={other} is not supported; use otlp or none"
                ));
            }
        }
        let endpoint = set(&format!("OTEL_EXPORTER_OTLP_{upper}_ENDPOINT"))
            .or_else(|| set("OTEL_EXPORTER_OTLP_ENDPOINT"));
        if endpoint.is_none() {
            return Ok(false);
        }
        for protocol_var in [
            format!("OTEL_EXPORTER_OTLP_{upper}_PROTOCOL"),
            "OTEL_EXPORTER_OTLP_PROTOCOL".to_string(),
        ] {
            if let Some(p) = set(&protocol_var) {
                if p.trim() != PROTOCOL {
                    return Err(format!(
                        "{protocol_var}={p} is not supported; the exporter speaks {PROTOCOL}"
                    ));
                }
                break;
            }
        }
        Ok(true)
    };
    Ok(Signals {
        traces: signal("traces")?,
        metrics: signal("metrics")?,
        logs: signal("logs")?,
    })
}

/// Install the providers the environment asks for, under the resource
/// `service` (unless `OTEL_SERVICE_NAME` names another) at `version`, with
/// the `attributes` the environment does not set itself. On an error
/// nothing is installed, and the caller reports it and runs on without
/// telemetry.
pub fn init(service: &str, version: &str, attributes: &[KeyValue]) -> Result<Telemetry, String> {
    let wanted = signals(|name| std::env::var(name).ok())?;
    if wanted == Signals::default() {
        return Ok(Telemetry::default());
    }
    // Every exporter first: one that cannot be built leaves nothing
    // installed, never one signal exporting under a warning that says off.
    let spans = wanted
        .traces
        .then(|| SpanExporter::builder().with_http().build())
        .transpose()
        .map_err(|e| format!("trace exporter: {e}"))?;
    let metrics = wanted
        .metrics
        .then(|| MetricExporter::builder().with_http().build())
        .transpose()
        .map_err(|e| format!("metric exporter: {e}"))?;
    #[cfg(feature = "logs")]
    let logs = wanted
        .logs
        .then(|| {
            opentelemetry_otlp::LogExporter::builder()
                .with_http()
                .build()
        })
        .transpose()
        .map_err(|e| format!("log exporter: {e}"))?;
    let resource = resource(service, version, attributes);
    let mut telemetry = Telemetry::default();
    if let Some(exporter) = spans {
        let provider = SdkTracerProvider::builder()
            .with_batch_exporter(exporter)
            .with_resource(resource.clone())
            .build();
        global::set_tracer_provider(provider.clone());
        telemetry.traces = Some(provider);
    }
    if let Some(exporter) = metrics {
        let provider = SdkMeterProvider::builder()
            .with_periodic_exporter(exporter)
            .with_resource(resource.clone())
            .build();
        global::set_meter_provider(provider.clone());
        telemetry.metrics = Some(provider);
    }
    #[cfg(feature = "logs")]
    if let Some(exporter) = logs {
        telemetry.logs = Some(
            SdkLoggerProvider::builder()
                .with_batch_exporter(exporter)
                .with_resource(resource)
                .build(),
        );
    }
    Ok(telemetry)
}

/// The environment's resource, named `service` unless it names itself, with
/// `version` and `attributes` unless it sets them.
fn resource(service: &str, version: &str, attributes: &[KeyValue]) -> Resource {
    let detected = Resource::builder().build();
    let mut builder = Resource::builder();
    let named = detected
        .get(&Key::new("service.name"))
        .is_some_and(|n| !n.as_str().starts_with("unknown_service"));
    if !named {
        builder = builder.with_service_name(service.to_string());
    }
    let version = KeyValue::new("service.version", version.to_string());
    for kv in std::iter::once(&version).chain(attributes) {
        if detected.get(&kv.key).is_none() {
            builder = builder.with_attribute(kv.clone());
        }
    }
    builder.build()
}

/// The context `TRACEPARENT` names, or the empty context when it is unset
/// or not a valid version `00` traceparent.
fn inherited() -> Context {
    std::env::var(TRACEPARENT)
        .ok()
        .and_then(|v| parse(&v))
        .map_or_else(Context::new, |sc| {
            Context::new().with_remote_span_context(sc)
        })
}

/// A W3C `traceparent` value: `00-<trace id>-<span id>-<flags>`.
fn parse(value: &str) -> Option<SpanContext> {
    let mut parts = value.trim().split('-');
    let (version, trace, span, flags) =
        (parts.next()?, parts.next()?, parts.next()?, parts.next()?);
    if version != "00"
        || parts.next().is_some()
        || trace.len() != 32
        || span.len() != 16
        || flags.len() != 2
    {
        return None;
    }
    let flags = u8::from_str_radix(flags, 16).ok()?;
    let sc = SpanContext::new(
        TraceId::from_hex(trace).ok()?,
        SpanId::from_hex(span).ok()?,
        TraceFlags::new(flags),
        true,
        TraceState::default(),
    );
    sc.is_valid().then_some(sc)
}

/// The current span's context as a `TRACEPARENT` value, for a child
/// process; `None` when no span is current, as when nothing is exported.
pub fn traceparent() -> Option<String> {
    let cx = Context::current();
    let sc = cx.span().span_context().clone();
    sc.is_valid().then(|| {
        format!(
            "00-{}-{}-{:02x}",
            sc.trace_id(),
            sc.span_id(),
            sc.trace_flags().to_u8()
        )
    })
}

/// A program's root span, current until dropped and then ended. Its parent
/// is the span `TRACEPARENT` names, so a program another one runs is part of
/// its caller's trace.
#[derive(Debug)]
pub struct Root {
    cx: Context,
    _guard: ContextGuard,
}

/// Start the root span `name` with `attributes`, recorded under `scope`,
/// the instrumentation scope of the program's own spans.
pub fn root(scope: &'static str, name: String, attributes: Vec<KeyValue>) -> Root {
    let tracer = global::tracer(scope);
    let parent = inherited();
    let span = tracer
        .span_builder(name)
        .with_attributes(attributes)
        .start_with_context(&tracer, &parent);
    let cx = parent.with_span(span);
    Root {
        _guard: cx.clone().attach(),
        cx,
    }
}

impl Root {
    pub fn set(&self, kv: KeyValue) {
        self.cx.span().set_attribute(kv);
    }

    /// Mark the span failed with `why`: a kind or an exit code, never an
    /// error's text.
    pub fn fail(&self, why: String) {
        self.cx.span().set_status(Status::error(why));
    }
}

impl Drop for Root {
    fn drop(&mut self) {
        self.cx.span().end();
    }
}

impl Telemetry {
    /// The logger provider, when the logs signal is exported.
    #[cfg(feature = "logs")]
    pub fn logs(&self) -> Option<&SdkLoggerProvider> {
        self.logs.as_ref()
    }

    /// Flush and stop every provider, together, for at most
    /// [`SHUTDOWN_BOUND`]: an unreachable collector delays exit by no more.
    pub fn shutdown(self) {
        let mut stops: Vec<Box<dyn FnOnce() + Send>> = Vec::new();
        if let Some(traces) = self.traces {
            stops.push(Box::new(move || {
                let _ = traces.shutdown_with_timeout(SHUTDOWN_BOUND);
            }));
        }
        if let Some(metrics) = self.metrics {
            stops.push(Box::new(move || {
                let _ = metrics.shutdown_with_timeout(SHUTDOWN_BOUND);
            }));
        }
        #[cfg(feature = "logs")]
        if let Some(logs) = self.logs {
            stops.push(Box::new(move || {
                let _ = logs.shutdown_with_timeout(SHUTDOWN_BOUND);
            }));
        }
        let (done, finished) = mpsc::channel();
        let pending = stops.len();
        for stop in stops {
            let done = done.clone();
            thread::spawn(move || {
                stop();
                let _ = done.send(());
            });
        }
        let deadline = Instant::now() + SHUTDOWN_BOUND;
        for _ in 0..pending {
            let left = deadline.saturating_duration_since(Instant::now());
            if finished.recv_timeout(left).is_err() {
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read(vars: &[(&str, &str)]) -> Result<Signals, String> {
        signals(|name| {
            vars.iter()
                .find(|(k, _)| *k == name)
                .map(|(_, v)| v.to_string())
        })
    }

    const ALL: Signals = Signals {
        traces: true,
        metrics: true,
        logs: cfg!(feature = "logs"),
    };

    #[test]
    fn nothing_is_exported_without_an_endpoint() {
        assert_eq!(read(&[]), Ok(Signals::default()));
        assert_eq!(
            read(&[
                ("OTEL_SERVICE_NAME", "x"),
                ("OTEL_TRACES_EXPORTER", "otlp"),
                ("OTEL_EXPORTER_OTLP_ENDPOINT", " ")
            ]),
            Ok(Signals::default())
        );
    }

    #[test]
    fn an_endpoint_turns_every_signal_on_unless_the_sdk_is_disabled() {
        let endpoint = ("OTEL_EXPORTER_OTLP_ENDPOINT", "http://collector:4318");
        assert_eq!(read(&[endpoint]), Ok(ALL));
        assert_eq!(
            read(&[endpoint, ("OTEL_SDK_DISABLED", "TRUE")]),
            Ok(Signals::default())
        );
        assert_eq!(read(&[endpoint, ("OTEL_SDK_DISABLED", "false")]), Ok(ALL));
    }

    #[test]
    fn a_signal_follows_its_own_endpoint_and_exporter() {
        assert_eq!(
            read(&[("OTEL_EXPORTER_OTLP_METRICS_ENDPOINT", "http://c/v1/metrics")]),
            Ok(Signals {
                metrics: true,
                ..Signals::default()
            })
        );
        assert_eq!(
            read(&[
                ("OTEL_EXPORTER_OTLP_ENDPOINT", "http://c"),
                ("OTEL_TRACES_EXPORTER", "none")
            ]),
            Ok(Signals {
                traces: false,
                ..ALL
            })
        );
    }

    #[test]
    fn an_unsupported_protocol_or_exporter_is_reported() {
        let endpoint = ("OTEL_EXPORTER_OTLP_ENDPOINT", "http://c");
        assert_eq!(
            read(&[endpoint, ("OTEL_EXPORTER_OTLP_PROTOCOL", "http/protobuf")]),
            Ok(ALL)
        );
        let grpc = read(&[endpoint, ("OTEL_EXPORTER_OTLP_PROTOCOL", "grpc")]).unwrap_err();
        assert!(grpc.contains("OTEL_EXPORTER_OTLP_PROTOCOL=grpc"), "{grpc}");
        // The signal's own protocol wins over the general one.
        assert_eq!(
            read(&[
                endpoint,
                ("OTEL_EXPORTER_OTLP_PROTOCOL", "grpc"),
                ("OTEL_EXPORTER_OTLP_TRACES_PROTOCOL", "http/protobuf"),
                ("OTEL_EXPORTER_OTLP_LOGS_PROTOCOL", "http/protobuf"),
                ("OTEL_METRICS_EXPORTER", "none")
            ]),
            Ok(Signals {
                metrics: false,
                ..ALL
            })
        );
        let console = read(&[endpoint, ("OTEL_METRICS_EXPORTER", "console")]).unwrap_err();
        assert!(
            console.contains("OTEL_METRICS_EXPORTER=console"),
            "{console}"
        );
    }

    #[test]
    fn a_traceparent_round_trips_and_a_malformed_one_is_ignored() {
        let value = "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01";
        let sc = parse(value).expect("valid");
        assert_eq!(
            sc.trace_id().to_string(),
            "4bf92f3577b34da6a3ce929d0e0e4736"
        );
        assert_eq!(sc.span_id().to_string(), "00f067aa0ba902b7");
        assert!(sc.is_sampled() && sc.is_remote());
        let cx = Context::new().with_remote_span_context(sc);
        let _guard = cx.attach();
        assert_eq!(traceparent().as_deref(), Some(value));
        for bad in [
            "",
            "01-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01",
            "00-00000000000000000000000000000000-00f067aa0ba902b7-01",
            "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7",
            "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-1",
        ] {
            assert!(parse(bad).is_none(), "{bad}");
        }
    }

    #[test]
    fn no_traceparent_without_a_span() {
        assert_eq!(traceparent(), None);
    }
}
