//! The interface's own telemetry (ADR-38), configured by the standard OTEL_*
//! variables through `otel-env`, as every program of the repository is
//! (docs/observability.md): its WARN-and-above `tracing` events as OTLP
//! logs, and a span around each Fleet command it runs, carried to `fleet` as
//! `TRACEPARENT`.
//!
//! Built only by the interface: it is long-running, so the batch processors'
//! background threads have time to export. A short-lived `thurbox-cli`
//! command — above all `session signal`, which every agent hook runs — never
//! builds it, or each hook would wait on a network round-trip, and on a
//! timeout while the collector is down.

use std::sync::OnceLock;

use opentelemetry_appender_tracing::layer::OpenTelemetryTracingBridge;
use otel_env::{KeyValue, Telemetry};
use tracing_subscriber::filter::{LevelFilter, Targets};
use tracing_subscriber::Layer;

/// The installed providers, kept for [`shutdown`]: the log layer itself is
/// moved into the subscriber, which is global and never dropped.
static TELEMETRY: OnceLock<std::sync::Mutex<Option<Telemetry>>> = OnceLock::new();

/// Install the providers the OTEL_* variables ask for, as `service.name`
/// `thurbox` at `version`, and answer the log bridge layer when the logs
/// signal is exported. `instance` is the multiplexer socket this instance
/// runs on, which tells a sandbox's telemetry apart from the real
/// instance's on one machine.
///
/// The layer has its own filter: WARN and above, from thurbox's own code only
/// (its multiplexer runtime, `runtime_mux`, included). The file's
/// `thurbox=debug` is far too much to ship, and the exporter's own HTTP stack
/// logs through `tracing` too, so letting other targets through would feed
/// each export back into the next.
///
/// An error is the reason nothing is exported, for the caller to log once
/// the subscriber is up.
pub fn init<S>(version: &str, instance: &str) -> Result<Option<impl Layer<S>>, String>
where
    S: tracing::Subscriber + for<'a> tracing_subscriber::registry::LookupSpan<'a>,
{
    let telemetry = otel_env::init(
        "thurbox",
        version,
        &[KeyValue::new("thurbox.instance", instance.to_string())],
    )?;
    let bridge = telemetry.logs().map(|provider| {
        let filter = Targets::new()
            .with_target("thurbox", LevelFilter::WARN)
            .with_target("runtime_mux", LevelFilter::WARN);
        OpenTelemetryTracingBridge::new(provider).with_filter(filter)
    });
    let _ = TELEMETRY.set(std::sync::Mutex::new(Some(telemetry)));
    Ok(bridge)
}

/// Send what the batch processors still hold and stop them, within
/// `otel_env::SHUTDOWN_BOUND`. Called on the interface's clean exit; a
/// process killed by a signal loses at most the last batch interval's
/// records, which the file log still has.
pub fn shutdown() {
    let taken = TELEMETRY
        .get()
        .and_then(|t| t.lock().ok().and_then(|mut t| t.take()));
    if let Some(telemetry) = taken {
        telemetry.shutdown();
    }
}
