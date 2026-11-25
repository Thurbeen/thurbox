use std::sync::OnceLock;

use opentelemetry_appender_tracing::layer::OpenTelemetryTracingBridge;
use otel_env::{KeyValue, Telemetry};
use tracing_subscriber::filter::{LevelFilter, Targets};
use tracing_subscriber::Layer;

static TELEMETRY: OnceLock<std::sync::Mutex<Option<Telemetry>>> = OnceLock::new();

pub fn init<S>(
    config: &otel_env::TelemetryConfig,
    version: &str,
    instance: &str,
) -> Result<(Option<impl Layer<S>>, Option<String>), String>
where
    S: tracing::Subscriber + for<'a> tracing_subscriber::registry::LookupSpan<'a>,
{
    otel_env::adopt(config)?;
    let telemetry = otel_env::init(
        "thurbox",
        version,
        &[KeyValue::new("thurbox.instance", instance.to_string())],
        otel_env::Logs::Environment,
    )?;
    let logs_off = telemetry
        .logs_off()
        .map(|why| format!("diagnostic logs are not exported: {why}"));
    let bridge = telemetry.logs().map(|provider| {
        let filter = Targets::new()
            .with_target("thurbox", LevelFilter::WARN)
            .with_target("runtime_mux", LevelFilter::WARN);
        OpenTelemetryTracingBridge::new(provider).with_filter(filter)
    });
    let _ = TELEMETRY.set(std::sync::Mutex::new(Some(telemetry)));
    Ok((bridge, logs_off))
}

pub fn shutdown() {
    let taken = TELEMETRY
        .get()
        .and_then(|t| t.lock().ok().and_then(|mut t| t.take()));
    if let Some(telemetry) = taken {
        telemetry.shutdown();
    }
}
