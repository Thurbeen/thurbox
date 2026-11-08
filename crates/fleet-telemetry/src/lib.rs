//! OpenTelemetry export for fleet: the SDK and its OTLP exporter, installed
//! as the process's global providers when the standard OTEL_* variables ask
//! for them, and nothing at all otherwise.
//!
//! Every other crate records through the `opentelemetry` API alone, which
//! does nothing until this crate installs a provider. A signal is exported
//! when its endpoint (`OTEL_EXPORTER_OTLP_ENDPOINT`, or the signal's own
//! `OTEL_EXPORTER_OTLP_{TRACES,METRICS}_ENDPOINT`) is set, its
//! `OTEL_{TRACES,METRICS}_EXPORTER` is unset or `otlp`, and
//! `OTEL_SDK_DISABLED` is not `true`. The transport is OTLP over HTTP with
//! protobuf; the exporter itself reads the headers, timeout and compression
//! variables, and the resource reads `OTEL_SERVICE_NAME` and
//! `OTEL_RESOURCE_ATTRIBUTES`.
//!
//! Export never decides anything: spans and points are batched on the SDK's
//! own threads into bounded queues that drop when full, a failed export is
//! dropped, and shutdown waits at most [`SHUTDOWN_BOUND`].

use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use opentelemetry::trace::{Span, Status, TraceContextExt, Tracer};
use opentelemetry::{Context, Key, KeyValue, global};
use opentelemetry_otlp::{MetricExporter, SpanExporter};
use opentelemetry_sdk::Resource;
use opentelemetry_sdk::metrics::SdkMeterProvider;
use opentelemetry_sdk::trace::SdkTracerProvider;

/// The instrumentation scope of every span and instrument fleet records.
pub const SCOPE: &str = "fleet";

/// The longest a command waits at exit for its last batches to leave.
pub const SHUTDOWN_BOUND: Duration = Duration::from_secs(2);

/// The one protocol fleet's exporter speaks.
const PROTOCOL: &str = "http/protobuf";

/// The installed providers, flushed by [`Telemetry::shutdown`]; both `None`
/// when nothing is exported.
#[derive(Debug, Default)]
pub struct Telemetry {
    traces: Option<SdkTracerProvider>,
    metrics: Option<SdkMeterProvider>,
}

/// Which signals the environment asks to export.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct Signals {
    traces: bool,
    metrics: bool,
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
                        "{protocol_var}={p} is not supported; fleet exports {PROTOCOL}"
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
    })
}

/// Install the providers the environment asks for. On an error nothing is
/// installed, and the caller reports it and runs on without telemetry.
pub fn init(version: &str) -> Result<Telemetry, String> {
    let wanted = signals(|name| std::env::var(name).ok())?;
    if wanted == Signals::default() {
        return Ok(Telemetry::default());
    }
    // Both exporters first: one that cannot be built leaves nothing
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
    let resource = resource(version);
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
            .with_resource(resource)
            .build();
        global::set_meter_provider(provider.clone());
        telemetry.metrics = Some(provider);
    }
    Ok(telemetry)
}

/// The environment's resource, named `fleet` unless it names itself, with
/// this build's version unless it sets one.
fn resource(version: &str) -> Resource {
    let detected = Resource::builder().build();
    let mut builder = Resource::builder();
    let named = detected
        .get(&Key::new("service.name"))
        .is_some_and(|n| !n.as_str().starts_with("unknown_service"));
    if !named {
        builder = builder.with_service_name(SCOPE);
    }
    if detected.get(&Key::new("service.version")).is_none() {
        builder = builder.with_attribute(KeyValue::new("service.version", version.to_string()));
    }
    builder.build()
}

impl Telemetry {
    /// Run one command as the root span `fleet NAME`, current while it runs,
    /// with its exit code; a non-zero exit is the span's error status.
    pub fn command(&self, name: &str, run: impl FnOnce() -> u8) -> u8 {
        let tracer = global::tracer(SCOPE);
        let mut span = tracer.start(format!("fleet {name}"));
        span.set_attribute(KeyValue::new("fleet.command", name.to_string()));
        let cx = Context::current_with_span(span);
        let code = {
            let _current = cx.clone().attach();
            run()
        };
        let span = cx.span();
        span.set_attribute(KeyValue::new("process.exit.code", i64::from(code)));
        if code != 0 {
            span.set_status(Status::error(format!("exit {code}")));
        }
        span.end();
        code
    }

    /// Flush and stop both providers, together, for at most
    /// [`SHUTDOWN_BOUND`]: an unreachable collector delays exit by no more.
    pub fn shutdown(self) {
        let (done, finished) = mpsc::channel();
        let mut pending = 0;
        if let Some(traces) = self.traces {
            let done = done.clone();
            pending += 1;
            thread::spawn(move || {
                let _ = traces.shutdown_with_timeout(SHUTDOWN_BOUND);
                let _ = done.send(());
            });
        }
        if let Some(metrics) = self.metrics {
            pending += 1;
            thread::spawn(move || {
                let _ = metrics.shutdown_with_timeout(SHUTDOWN_BOUND);
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

    const BOTH: Signals = Signals {
        traces: true,
        metrics: true,
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
    fn an_endpoint_turns_both_signals_on_unless_the_sdk_is_disabled() {
        let endpoint = ("OTEL_EXPORTER_OTLP_ENDPOINT", "http://collector:4318");
        assert_eq!(read(&[endpoint]), Ok(BOTH));
        assert_eq!(
            read(&[endpoint, ("OTEL_SDK_DISABLED", "TRUE")]),
            Ok(Signals::default())
        );
        assert_eq!(read(&[endpoint, ("OTEL_SDK_DISABLED", "false")]), Ok(BOTH));
    }

    #[test]
    fn a_signal_follows_its_own_endpoint_and_exporter() {
        assert_eq!(
            read(&[("OTEL_EXPORTER_OTLP_METRICS_ENDPOINT", "http://c/v1/metrics")]),
            Ok(Signals {
                traces: false,
                metrics: true
            })
        );
        assert_eq!(
            read(&[
                ("OTEL_EXPORTER_OTLP_ENDPOINT", "http://c"),
                ("OTEL_TRACES_EXPORTER", "none")
            ]),
            Ok(Signals {
                traces: false,
                metrics: true
            })
        );
    }

    #[test]
    fn an_unsupported_protocol_or_exporter_is_reported() {
        let endpoint = ("OTEL_EXPORTER_OTLP_ENDPOINT", "http://c");
        assert_eq!(
            read(&[endpoint, ("OTEL_EXPORTER_OTLP_PROTOCOL", "http/protobuf")]),
            Ok(BOTH)
        );
        let grpc = read(&[endpoint, ("OTEL_EXPORTER_OTLP_PROTOCOL", "grpc")]).unwrap_err();
        assert!(grpc.contains("OTEL_EXPORTER_OTLP_PROTOCOL=grpc"), "{grpc}");
        // The signal's own protocol wins over the general one.
        assert_eq!(
            read(&[
                endpoint,
                ("OTEL_EXPORTER_OTLP_PROTOCOL", "grpc"),
                ("OTEL_EXPORTER_OTLP_TRACES_PROTOCOL", "http/protobuf"),
                ("OTEL_METRICS_EXPORTER", "none")
            ]),
            Ok(Signals {
                traces: true,
                metrics: false
            })
        );
        let console = read(&[endpoint, ("OTEL_METRICS_EXPORTER", "console")]).unwrap_err();
        assert!(
            console.contains("OTEL_METRICS_EXPORTER=console"),
            "{console}"
        );
    }
}
