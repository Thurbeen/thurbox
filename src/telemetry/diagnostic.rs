//! The interface's own diagnostic logs over OTLP (ADR-38).
//!
//! One more `tracing` layer beside the file appender, built only by the
//! interface: it is long-running, so the batch processor's background thread
//! has time to export. A short-lived `thurbox-cli` command — above all
//! `session signal`, which every agent hook runs — never builds it, or each
//! hook would wait on a network round-trip, and on a timeout while the
//! collector is down.

use std::sync::OnceLock;

use opentelemetry_appender_tracing::layer::OpenTelemetryTracingBridge;
use opentelemetry_otlp::{Protocol, WithExportConfig, WithHttpConfig};
use opentelemetry_sdk::logs::{BatchLogProcessor, SdkLoggerProvider};
use tracing_subscriber::filter::{LevelFilter, Targets};
use tracing_subscriber::Layer;

use super::{resource, signal_url, Identity, EXPORT_TIMEOUT, HTTP_PROTOBUF};
use crate::session::settings::TelemetrySettings;

/// The provider behind the layer, kept for [`shutdown`]: the layer itself
/// is moved into the subscriber, which is global and never dropped.
static PROVIDER: OnceLock<SdkLoggerProvider> = OnceLock::new();

/// The bridge layer, when `export_diagnostic_logs` is on.
///
/// Its filter is its own: WARN and above, from thurbox's own code only. The
/// file's `thurbox=debug` is far too much to ship, and the exporter's own
/// HTTP stack logs through `tracing` too, so letting other targets through
/// would feed each export back into the next.
///
/// `None` when the setting is off, when the protocol is one this exporter
/// does not speak, or when the exporter cannot be built — the last two
/// with the reason, for the caller to log once the subscriber is up.
pub fn layer<S>(
    settings: &TelemetrySettings,
    identity: &Identity,
) -> Result<Option<impl Layer<S>>, String>
where
    S: tracing::Subscriber + for<'a> tracing_subscriber::registry::LookupSpan<'a>,
{
    if !settings.exports_diagnostic_logs() {
        return Ok(None);
    }
    if settings.protocol != HTTP_PROTOBUF {
        return Err(format!(
            "diagnostic logs: protocol '{}': thurbox exports only {HTTP_PROTOBUF}",
            settings.protocol
        ));
    }
    let exporter = opentelemetry_otlp::LogExporter::builder()
        .with_http()
        .with_protocol(Protocol::HttpBinary)
        .with_endpoint(signal_url(settings, "logs"))
        .with_timeout(EXPORT_TIMEOUT)
        .with_headers(settings.headers.clone().into_iter().collect())
        .build()
        .map_err(|e| format!("diagnostic logs exporter: {e}"))?;
    let provider = SdkLoggerProvider::builder()
        .with_resource(resource(identity))
        .with_log_processor(BatchLogProcessor::builder(exporter).build())
        .build();
    let bridge = OpenTelemetryTracingBridge::new(&provider);
    let _ = PROVIDER.set(provider);
    let filter = Targets::new().with_target("thurbox", LevelFilter::WARN);
    Ok(Some(bridge.with_filter(filter)))
}

/// Send what the batch processor still holds and stop it. Called on the
/// interface's clean exit; a process killed by a signal loses at most the
/// last batch interval's records, which the file log still has.
pub fn shutdown() {
    if let Some(provider) = PROVIDER.get() {
        let _ = provider.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tracing_subscriber::Registry;

    fn identity() -> Identity {
        Identity {
            version: "test".into(),
            instance: "test".into(),
        }
    }

    #[test]
    fn no_bridge_unless_telemetry_and_the_switch_are_both_on() {
        let switch_only = TelemetrySettings {
            export_diagnostic_logs: true,
            ..TelemetrySettings::default()
        };
        assert!(layer::<Registry>(&switch_only, &identity())
            .unwrap()
            .is_none());
        let telemetry_only = TelemetrySettings {
            enabled: true,
            ..TelemetrySettings::default()
        };
        assert!(layer::<Registry>(&telemetry_only, &identity())
            .unwrap()
            .is_none());
    }

    #[test]
    fn a_protocol_this_exporter_does_not_speak_is_reported() {
        let settings = TelemetrySettings {
            enabled: true,
            export_diagnostic_logs: true,
            protocol: "grpc".into(),
            ..TelemetrySettings::default()
        };
        let error = layer::<Registry>(&settings, &identity()).err().unwrap();
        assert!(error.contains("grpc"), "{error}");
    }
}
