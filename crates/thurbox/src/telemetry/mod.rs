//! The interface's own `tracing` events over OTLP, opt-in (ADR-38):
//! [`diagnostic`]. Session metrics and event logs are Fleet's
//! (`fleet-telemetry`).

pub mod diagnostic;

use std::time::Duration;

use opentelemetry::KeyValue;
use opentelemetry_sdk::Resource;

use crate::session::settings::TelemetrySettings;

/// How long one request may take, retries included.
const EXPORT_TIMEOUT: Duration = Duration::from_secs(5);

/// The one protocol thurbox's own exporter speaks.
const HTTP_PROTOBUF: &str = "http/protobuf";

/// Who is exporting: the OTLP resource every record carries.
#[derive(Debug, Clone)]
pub struct Identity {
    pub version: String,
    /// The multiplexer socket this instance runs on, which tells a sandbox's
    /// telemetry apart from the real instance's on one machine.
    pub instance: String,
}

fn resource(identity: &Identity) -> Resource {
    let host = sysinfo::System::host_name().unwrap_or_default();
    Resource::builder_empty()
        .with_attributes([
            KeyValue::new("service.name", "thurbox"),
            KeyValue::new("service.version", identity.version.clone()),
            KeyValue::new("host.name", host),
            KeyValue::new("thurbox.instance", identity.instance.clone()),
        ])
        .build()
}

fn signal_url(settings: &TelemetrySettings, signal: &str) -> String {
    format!("{}/v1/{signal}", settings.endpoint.trim_end_matches('/'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_signal_url_appends_the_path_once() {
        let mut settings = TelemetrySettings::default();
        for endpoint in ["http://collector:4318", "http://collector:4318/"] {
            settings.endpoint = endpoint.into();
            assert_eq!(
                signal_url(&settings, "logs"),
                "http://collector:4318/v1/logs"
            );
        }
    }
}
