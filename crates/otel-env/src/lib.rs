//! OpenTelemetry export from the standard OTEL_* variables, the same in
//! every program the repository ships (`fleet`, `runtime-mux`,
//! `workspace-git`, the thurbox interface): the SDK and its OTLP exporter,
//! installed as the process's global providers when the variables ask for
//! them, and nothing at all otherwise. docs/observability.md is the
//! interface this crate implements.
//!
//! A program with a config file gives its `[telemetry]` table to [`adopt`]
//! first: the table stands for the variables it names, and a variable set in
//! the environment wins over it ([`TelemetryConfig`]). The process
//! environment itself is never changed; [`child_env`] is what a child
//! process must be given on top of the environment it inherits.
//!
//! Every other crate records through the `opentelemetry` API alone, which
//! does nothing until this crate installs a provider. A signal is exported
//! when its endpoint (`OTEL_EXPORTER_OTLP_ENDPOINT`, or the signal's own
//! `OTEL_EXPORTER_OTLP_{TRACES,METRICS,LOGS}_ENDPOINT`) is set, its
//! `OTEL_{TRACES,METRICS}_EXPORTER` is unset or `otlp` (logs need
//! `OTEL_LOGS_EXPORTER=otlp` set explicitly), and `OTEL_SDK_DISABLED` is not
//! `true`. The transport is OTLP over HTTP with
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

mod file;

use std::sync::{mpsc, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

use opentelemetry::trace::{
    SpanContext, SpanId, Status, TraceContextExt, TraceFlags, TraceId, TraceState, Tracer,
};
use opentelemetry::{global, Context, ContextGuard, Key};

/// The attribute type every caller passes, so a program needs no
/// `opentelemetry` dependency of its own to name one.
pub use opentelemetry::KeyValue;
use opentelemetry_otlp::{MetricExporter, SpanExporter, WithExportConfig, WithHttpConfig};
use opentelemetry_sdk::metrics::SdkMeterProvider;
use opentelemetry_sdk::trace::SdkTracerProvider;
use opentelemetry_sdk::Resource;

pub use file::{HeadersSource, Source, TelemetryConfig};
pub use opentelemetry_sdk::logs::SdkLoggerProvider;

use file::{FileVars, Merged};

static FILE: OnceLock<FileVars> = OnceLock::new();

/// Take the program's `[telemetry]` table for the rest of the process,
/// before [`init`]: read its headers file and stand it for the variables
/// it names. Only the first call takes effect. On an error nothing is
/// taken, and the caller reports it.
pub fn adopt(config: &TelemetryConfig) -> Result<(), String> {
    let vars = config.vars()?;
    let _ = FILE.set(vars);
    Ok(())
}

fn merged() -> Merged<'static, impl Fn(&str) -> Option<String>> {
    static NONE: OnceLock<FileVars> = OnceLock::new();
    Merged {
        env: |name: &str| std::env::var(name).ok(),
        file: FILE
            .get()
            .unwrap_or_else(|| NONE.get_or_init(FileVars::default)),
    }
}

/// The adopted table's variables a child process must be given: those in
/// force that the environment it inherits lacks. A program another one
/// starts reads no config file of its own.
pub fn child_env() -> Vec<(String, String)> {
    merged().child_env()
}

/// The longest a program waits at exit for its last batches to leave.
pub const SHUTDOWN_BOUND: Duration = Duration::from_secs(2);

/// The same bound for a program that answers one call of another (a
/// runtime, `workspace-git`, a one-shot `fleet` command): it writes its
/// answer first, then waits no more than this, so a stalled collector stays
/// far inside its caller's deadline.
pub const CALL_SHUTDOWN_BOUND: Duration = Duration::from_millis(100);

/// The environment variable that carries trace context to a child process
/// (W3C Trace Context, as the OpenTelemetry environment carrier spells it).
pub const TRACEPARENT: &str = "TRACEPARENT";

/// The one protocol the exporter speaks.
pub(crate) const PROTOCOL: &str = "http/protobuf";

/// The installed providers, flushed by [`Telemetry::shutdown`]; all `None`
/// when nothing is exported.
#[derive(Debug, Default)]
pub struct Telemetry {
    traces: Option<SdkTracerProvider>,
    metrics: Option<SdkMeterProvider>,
    logs: Option<SdkLoggerProvider>,
    /// Why the logs signal is off although it was asked for.
    logs_off: Option<String>,
}

/// Which signals the environment asks to export.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
struct Signals {
    traces: bool,
    metrics: bool,
    logs: bool,
    /// Why logs are off although the program asked for them.
    logs_off: Option<String>,
}

/// Read the signals from the variables `var` answers; the logs signal only
/// when the program records logs (`logs`), so a program that records none
/// never reads a logs variable. An unsupported protocol or exporter of
/// traces or metrics is an error, so a misconfiguration is reported rather
/// than silently exporting nothing. One of logs turns logs off alone: a log
/// setting never changes what traces and metrics do.
fn signals(var: impl Fn(&str) -> Option<String>, logs: bool) -> Result<Signals, String> {
    let set = |name: &str| var(name).filter(|v| !v.trim().is_empty());
    if set("OTEL_SDK_DISABLED").is_some_and(|v| v.trim().eq_ignore_ascii_case("true")) {
        return Ok(Signals::default());
    }
    let signal = |name: &str| -> Result<bool, String> {
        let upper = name.to_ascii_uppercase();
        let exporter_var = format!("OTEL_{upper}_EXPORTER");
        match set(&exporter_var).as_deref().map(str::trim) {
            // Log records carry text, so logs are opt-in: an endpoint alone,
            // which an agent's own telemetry may have set, never sends them.
            None if name == "logs" => return Ok(false),
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
    let (logs, logs_off) = match logs.then(|| signal("logs")) {
        None => (false, None),
        Some(Ok(on)) => (on, None),
        Some(Err(why)) => (false, Some(why)),
    };
    Ok(Signals {
        traces: signal("traces")?,
        metrics: signal("metrics")?,
        logs,
        logs_off,
    })
}

/// Where this program's own export goes, for the agents a program starts
/// to export to as well (docs/observability.md, *Coding agents*): the base
/// endpoint `OTEL_EXPORTER_OTLP_ENDPOINT`, without a trailing `/`, and
/// `OTEL_EXPORTER_OTLP_HEADERS`. `None` unless traces or metrics are
/// exported and the base endpoint is set: a signal's own endpoint names no
/// collector for the other signals.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Collector {
    pub endpoint: String,
    /// Always `http/protobuf`: the one protocol the programs speak.
    pub protocol: &'static str,
    pub headers: Option<String>,
}

/// This process's [`Collector`], from its environment.
pub fn collector() -> Option<Collector> {
    let merged = merged();
    collector_from(|name| merged.var(name))
}

fn collector_from(var: impl Fn(&str) -> Option<String>) -> Option<Collector> {
    let wanted = signals(&var, false).ok()?;
    if !(wanted.traces || wanted.metrics) {
        return None;
    }
    let set = |name: &str| {
        var(name)
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
    };
    Some(Collector {
        endpoint: set("OTEL_EXPORTER_OTLP_ENDPOINT")?
            .trim_end_matches('/')
            .to_string(),
        protocol: PROTOCOL,
        headers: set("OTEL_EXPORTER_OTLP_HEADERS"),
    })
}

/// Install the providers the environment asks for, under the resource
/// `service` (unless `OTEL_SERVICE_NAME` names another) at `version`, with
/// the `attributes` the environment does not set itself; the logs signal
/// only when `logs` (the program records logs). On an error nothing is
/// installed, and the caller reports it and runs on without telemetry.
pub fn init(
    service: &str,
    version: &str,
    attributes: &[KeyValue],
    logs: bool,
) -> Result<Telemetry, String> {
    let merged = merged();
    let wanted = signals(|name| merged.var(name), logs)?;
    if !(wanted.traces || wanted.metrics || wanted.logs) {
        return Ok(Telemetry {
            logs_off: wanted.logs_off,
            ..Telemetry::default()
        });
    }
    // Every exporter first: one that cannot be built leaves nothing
    // installed, never one signal exporting under a warning that says off.
    let spans = wanted
        .traces
        .then(|| configured(SpanExporter::builder().with_http(), &merged, "traces").build())
        .transpose()
        .map_err(|e| format!("trace exporter: {e}"))?;
    let metrics = wanted
        .metrics
        .then(|| configured(MetricExporter::builder().with_http(), &merged, "metrics").build())
        .transpose()
        .map_err(|e| format!("metric exporter: {e}"))?;
    let logs = wanted
        .logs
        .then(|| {
            configured(
                opentelemetry_otlp::LogExporter::builder().with_http(),
                &merged,
                "logs",
            )
            .build()
        })
        .transpose()
        .map_err(|e| format!("log exporter: {e}"))?;
    let mut all: Vec<KeyValue> = merged
        .file_resource()
        .into_iter()
        .map(|(k, v)| KeyValue::new(k, v))
        .collect();
    all.extend(
        attributes
            .iter()
            .filter(|a| !all.iter().any(|f| f.key == a.key))
            .cloned()
            .collect::<Vec<_>>(),
    );
    let resource = resource(service, version, &all);
    let mut telemetry = Telemetry {
        logs_off: wanted.logs_off,
        ..Telemetry::default()
    };
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

/// The exporter of `signal` with the adopted table's endpoint and headers,
/// where they are in force; the exporter reads the variables itself.
fn configured<B: WithExportConfig + WithHttpConfig>(
    builder: B,
    merged: &Merged<'_, impl Fn(&str) -> Option<String>>,
    signal: &str,
) -> B {
    let mut builder = builder;
    let own = format!(
        "OTEL_EXPORTER_OTLP_{}_ENDPOINT",
        signal.to_ascii_uppercase()
    );
    if merged.var(&own).is_none() && merged.from_file("OTEL_EXPORTER_OTLP_ENDPOINT") {
        if let Some(base) = merged.var("OTEL_EXPORTER_OTLP_ENDPOINT") {
            builder = builder.with_endpoint(format!("{base}/v1/{signal}"));
        }
    }
    if merged.from_file("OTEL_EXPORTER_OTLP_HEADERS") {
        if let Some(headers) = merged.var("OTEL_EXPORTER_OTLP_HEADERS") {
            builder = builder.with_headers(file::header_pairs(&headers));
        }
    }
    builder
}

/// What a program with the `[telemetry]` table `config` exports under this
/// process's environment, and where each value in force comes from, for a
/// doctor: never a header's value. An error is why nothing is exported.
/// `logs` as for [`init`].
pub fn describe(config: &TelemetryConfig, logs: bool) -> Result<String, String> {
    let file = config.vars()?;
    let merged = Merged {
        env: |name: &str| std::env::var(name).ok(),
        file: &file,
    };
    let wanted = signals(|name| merged.var(name), logs)?;
    let from = |name: &str| {
        merged
            .lookup(name)
            .map_or_else(|| "default".to_string(), |(_, source)| source.to_string())
    };
    if merged
        .var("OTEL_SDK_DISABLED")
        .is_some_and(|v| v.trim().eq_ignore_ascii_case("true"))
    {
        return Ok(format!("off ({})", from("OTEL_SDK_DISABLED")));
    }
    let mut parts = vec![match merged.lookup("OTEL_EXPORTER_OTLP_ENDPOINT") {
        Some((endpoint, source)) => format!("endpoint {endpoint} ({source})"),
        None => "no endpoint".to_string(),
    }];
    let mut names = vec![("traces", wanted.traces), ("metrics", wanted.metrics)];
    if logs {
        names.push(("logs", wanted.logs));
    }
    for (signal, on) in names {
        let exporter = format!("OTEL_{}_EXPORTER", signal.to_ascii_uppercase());
        let state = if on { "on" } else { "off" };
        parts.push(format!("{signal} {state} ({})", from(&exporter)));
    }
    if let Some((_, source)) = merged.lookup("OTEL_EXPORTER_OTLP_HEADERS") {
        parts.push(format!("headers ({source})"));
    }
    if let Some((value, source)) = merged.lookup("OTEL_RESOURCE_ATTRIBUTES") {
        let keys: Vec<&str> = value
            .split(',')
            .filter_map(|kv| kv.split_once('=').map(|(k, _)| k.trim()))
            .collect();
        parts.push(format!("resource {} ({source})", keys.join(", ")));
    }
    Ok(parts.join("; "))
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
    pub fn logs(&self) -> Option<&SdkLoggerProvider> {
        self.logs.as_ref()
    }

    /// Why the logs signal is off although the program asked for it: an
    /// unsupported logs exporter or protocol.
    pub fn logs_off(&self) -> Option<&str> {
        self.logs_off.as_deref()
    }

    /// Flush and stop every provider, together, for at most
    /// [`SHUTDOWN_BOUND`]: an unreachable collector delays exit by no more.
    pub fn shutdown(self) {
        self.shutdown_within(SHUTDOWN_BOUND);
    }

    /// Flush and stop every provider, together, for at most `bound`. What
    /// is still unsent then is dropped.
    pub fn shutdown_within(self, bound: Duration) {
        let mut stops: Vec<Box<dyn FnOnce() + Send>> = Vec::new();
        if let Some(traces) = self.traces {
            stops.push(Box::new(move || {
                let _ = traces.shutdown_with_timeout(bound);
            }));
        }
        if let Some(metrics) = self.metrics {
            stops.push(Box::new(move || {
                let _ = metrics.shutdown_with_timeout(bound);
            }));
        }
        if let Some(logs) = self.logs {
            stops.push(Box::new(move || {
                let _ = logs.shutdown_with_timeout(bound);
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
        let deadline = Instant::now() + bound;
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

    fn read_for(vars: &[(&str, &str)], logs: bool) -> Result<Signals, String> {
        signals(
            |name| {
                vars.iter()
                    .find(|(k, _)| *k == name)
                    .map(|(_, v)| v.to_string())
            },
            logs,
        )
    }

    /// As a program that records logs reads them, with logs opted in.
    fn read(vars: &[(&str, &str)]) -> Result<Signals, String> {
        let mut vars = vars.to_vec();
        if !vars.iter().any(|(k, _)| *k == "OTEL_LOGS_EXPORTER") {
            vars.push(("OTEL_LOGS_EXPORTER", "otlp"));
        }
        read_for(&vars, true)
    }

    fn collector_for(vars: &[(&str, &str)]) -> Option<Collector> {
        collector_from(|name| {
            vars.iter()
                .find(|(k, _)| *k == name)
                .map(|(_, v)| v.to_string())
        })
    }

    #[test]
    fn agents_are_given_the_collector_only_while_this_program_exports() {
        let endpoint = ("OTEL_EXPORTER_OTLP_ENDPOINT", "http://c:4318/");
        assert_eq!(
            collector_for(&[endpoint, ("OTEL_EXPORTER_OTLP_HEADERS", "a=b")]),
            Some(Collector {
                endpoint: "http://c:4318".into(),
                protocol: "http/protobuf",
                headers: Some("a=b".into()),
            })
        );
        assert_eq!(collector_for(&[endpoint]).unwrap().headers, None);
        for off in [
            vec![],
            vec![endpoint, ("OTEL_SDK_DISABLED", "true")],
            vec![
                endpoint,
                ("OTEL_TRACES_EXPORTER", "none"),
                ("OTEL_METRICS_EXPORTER", "none"),
            ],
            vec![endpoint, ("OTEL_EXPORTER_OTLP_PROTOCOL", "grpc")],
            // A signal's own endpoint is no collector for the others.
            vec![(
                "OTEL_EXPORTER_OTLP_TRACES_ENDPOINT",
                "http://c:4318/v1/traces",
            )],
        ] {
            assert_eq!(collector_for(&off), None, "{off:?}");
        }
    }

    const ALL: Signals = Signals {
        traces: true,
        metrics: true,
        logs: true,
        logs_off: None,
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
                ..ALL.clone()
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
                ..ALL.clone()
            })
        );
        let console = read(&[endpoint, ("OTEL_METRICS_EXPORTER", "console")]).unwrap_err();
        assert!(
            console.contains("OTEL_METRICS_EXPORTER=console"),
            "{console}"
        );
    }

    #[test]
    fn a_log_setting_never_turns_off_traces_or_metrics() {
        let endpoint = ("OTEL_EXPORTER_OTLP_ENDPOINT", "http://c");
        for logs in [
            ("OTEL_LOGS_EXPORTER", "console"),
            ("OTEL_EXPORTER_OTLP_LOGS_PROTOCOL", "grpc"),
        ] {
            let read = read(&[endpoint, logs]).expect("traces and metrics stay on");
            assert!(read.traces && read.metrics, "{logs:?}: {read:?}");
            assert!(!read.logs, "{logs:?}: {read:?}");
            assert!(read.logs_off.is_some(), "the reason is kept: {read:?}");
        }
    }

    #[test]
    fn logs_are_exported_only_when_otel_logs_exporter_is_otlp() {
        let endpoint = ("OTEL_EXPORTER_OTLP_ENDPOINT", "http://c");
        assert_eq!(
            read_for(&[endpoint], true),
            Ok(Signals {
                logs: false,
                ..ALL.clone()
            }),
            "an endpoint alone sends no log text"
        );
        assert_eq!(
            read_for(&[endpoint, ("OTEL_LOGS_EXPORTER", "otlp")], true),
            Ok(ALL)
        );
    }

    #[test]
    fn a_program_that_records_no_logs_reads_no_logs_variable() {
        let endpoint = ("OTEL_EXPORTER_OTLP_ENDPOINT", "http://c");
        assert_eq!(
            read_for(&[endpoint, ("OTEL_LOGS_EXPORTER", "console")], false),
            Ok(Signals {
                logs: false,
                ..ALL.clone()
            })
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
