//! thurbox's own session metrics and session event logs over OTLP (ADR-37).
//!
//! What only thurbox knows, the same for every agent — how many sessions are
//! in each state, how long a turn takes, how long a session waits on a human,
//! how sessions come and go — read from the `session_events` log and sent from
//! `automation tick`. The log is written by every process that changes a
//! session, so the export is right with the interface closed, and the tick
//! already runs whenever sessions exist: no daemon of its own.
//!
//! Each tick is a fresh process holding no state, so the metrics are **delta**
//! temporality, and the position reached in the log is a cursor in the
//! database (`storage::telemetry_export`). The agents' own telemetry is not
//! here: ADR-36 configures their exporters, and both streams carry
//! `thurbox.session.id` so a backend joins them.
//!
//! [`diagnostic`] is the third stream: the interface's own `tracing` events,
//! opt-in, bridged to the same collector (ADR-38).

pub mod diagnostic;
mod summary;

use std::collections::{HashMap, HashSet};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use opentelemetry::logs::{AnyValue, LogRecord as _, Logger as _, LoggerProvider as _, Severity};
use opentelemetry::metrics::MeterProvider as _;
use opentelemetry::KeyValue;
use opentelemetry_otlp::{Protocol, WithExportConfig, WithHttpConfig};
use opentelemetry_sdk::logs::{BatchConfigBuilder, BatchLogProcessor, SdkLoggerProvider};
use opentelemetry_sdk::metrics::{PeriodicReader, SdkMeterProvider, Temporality};
use opentelemetry_sdk::Resource;

use crate::session::settings::TelemetrySettings;
use crate::session::Route;
use crate::storage::Database;

use summary::{Measure, Record, Summary};
pub use summary::{SessionCount, LOST};

/// How long one request may take, retries included. A tick with the
/// collector down spends this once and moves on.
const EXPORT_TIMEOUT: Duration = Duration::from_secs(5);

/// How long an exporter holds the cursor: past every request it can make, so
/// a live exporter never loses it, short enough that a crashed one's rows go
/// out with the next tick but one.
const LEASE: Duration = Duration::from_secs(90);

/// The most events one tick sends; a backlog drains over several ticks.
const BATCH: usize = 1_000;

/// Records per log request. Below the SDK batch processor's own export size,
/// so every request is one this module asked for and sees the result of.
const RECORDS_PER_REQUEST: usize = 256;

/// Upper bounds of the duration histograms, in seconds: a turn takes seconds
/// to an hour, a wait on a human seconds to a working day.
const DURATION_BOUNDS: &[f64] = &[
    1.0, 5.0, 15.0, 30.0, 60.0, 120.0, 300.0, 600.0, 1800.0, 3600.0, 7200.0, 28800.0,
];

/// The one protocol thurbox's own exporter speaks.
const HTTP_PROTOBUF: &str = "http/protobuf";

/// Who is exporting: the OTLP resource every metric and record carries.
#[derive(Debug, Clone)]
pub struct Identity {
    pub version: String,
    /// The multiplexer socket this instance runs its sessions on, which tells
    /// a sandbox's telemetry apart from the real instance's on one machine.
    pub instance: String,
}

/// What one export sent.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Report {
    /// Session events exported as log records.
    pub events: usize,
    /// Where the cursor now stands.
    pub cursor: Option<i64>,
    /// Why nothing went (another exporter held the lease, or the protocol is
    /// not one this exporter speaks), or that the rows went but another
    /// exporter had taken them over.
    pub skipped: Option<String>,
}

/// Export the session events since the last export, and the current session
/// counts, to `settings.endpoint`.
///
/// `sessions` is every live session, already assessed by the caller (the
/// state words belong to the CLI's session facts). Rows on a host named in
/// `shared_hosts` are left out, events and counts alike: that host's own tick
/// exports them, under its own `host.name` (ADR-24).
///
/// On failure the cursor stays where it was and the error is returned, so the
/// next tick sends the same rows again: at least once, a duplicate preferred
/// to a gap. Nothing here panics or reaches a user's screen.
pub fn export(
    db: &Database,
    settings: &TelemetrySettings,
    identity: &Identity,
    sessions: &[SessionCount],
    shared_hosts: &HashSet<String>,
) -> Result<Report, String> {
    if !settings.exports_thurbox() {
        return Ok(Report::default());
    }
    if settings.protocol != HTTP_PROTOBUF {
        return Ok(Report {
            skipped: Some(format!(
                "protocol '{}': thurbox exports only {HTTP_PROTOBUF}",
                settings.protocol
            )),
            ..Report::default()
        });
    }
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64;
    let Some(from) = db
        .claim_telemetry_export(now, LEASE.as_millis() as i64)
        .map_err(|e| format!("claim_telemetry_export: {e}"))?
    else {
        return Ok(Report {
            skipped: Some("another export holds the lease".into()),
            ..Report::default()
        });
    };
    let outcome = send_since(db, settings, identity, sessions, shared_hosts, from);
    match outcome {
        Ok((events, to)) => {
            let moved = db
                .advance_telemetry_cursor(from, to)
                .map_err(|e| format!("advance_telemetry_cursor: {e}"))?;
            Ok(Report {
                events,
                cursor: Some(to),
                // This export outlived its lease and another took the rows
                // over: they went twice, which at-least-once allows.
                skipped: (!moved).then(|| "the cursor moved during the export".into()),
            })
        }
        Err(e) => {
            let _ = db.release_telemetry_export();
            Err(e)
        }
    }
}

/// Read, summarize and send everything after `from`, returning how many
/// events went and the seq the cursor moves to.
fn send_since(
    db: &Database,
    settings: &TelemetrySettings,
    identity: &Identity,
    sessions: &[SessionCount],
    shared_hosts: &HashSet<String>,
    from: i64,
) -> Result<(usize, i64), String> {
    let batch = db
        .session_events_for_export(from, BATCH)
        .map_err(|e| format!("session_events_for_export: {e}"))?;
    let to = batch.last().map_or(from, |e| e.event.seq);
    let shared = |backend: &str| on_shared_host(backend, shared_hosts);
    let events: Vec<_> = batch
        .into_iter()
        .filter(|e| !e.session.as_ref().is_some_and(|s| shared(&s.backend_type)))
        .collect();
    let mut prior = HashMap::new();
    for id in events.iter().map(|e| e.event.session_id) {
        if let std::collections::hash_map::Entry::Vacant(slot) = prior.entry(id) {
            if let Some(state) = db
                .state_at_seq(id, from)
                .map_err(|e| format!("state_at_seq: {e}"))?
            {
                slot.insert(state);
            }
        }
    }
    let summary = summary::summarize(&events, &prior, settings.per_session);
    let counted: Vec<SessionCount> = sessions
        .iter()
        .filter(|s| !shared(&s.backend))
        .cloned()
        .collect();
    let resource = resource(identity);
    if settings.export_thurbox_metrics {
        let gauge = summary::gauge(&counted, settings.per_session);
        send_metrics(settings, resource.clone(), &summary, &gauge)?;
    }
    let mut sent = 0;
    if settings.export_session_events && !summary.records.is_empty() {
        send_logs(settings, resource, &summary.records)?;
        sent = summary.records.len();
    }
    Ok((sent, to))
}

/// Whether a row on `backend` is a shared host's own, mirrored here.
fn on_shared_host(backend: &str, shared_hosts: &HashSet<String>) -> bool {
    Route::parse(backend)
        .ok()
        .and_then(|r| r.host().map(str::to_string))
        .is_some_and(|h| shared_hosts.contains(&h))
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

fn send_metrics(
    settings: &TelemetrySettings,
    resource: Resource,
    summary: &Summary,
    gauge: &[(i64, Vec<KeyValue>)],
) -> Result<(), String> {
    let exporter = opentelemetry_otlp::MetricExporter::builder()
        .with_http()
        .with_protocol(Protocol::HttpBinary)
        .with_endpoint(signal_url(settings, "metrics"))
        .with_timeout(EXPORT_TIMEOUT)
        .with_headers(settings.headers.clone().into_iter().collect())
        .with_temporality(Temporality::Delta)
        .build()
        .map_err(|e| format!("metrics exporter: {e}"))?;
    // The interval never elapses in a process that lives for one tick: the
    // flush below is the export.
    let reader = PeriodicReader::builder(exporter)
        .with_interval(Duration::from_secs(3600))
        .build();
    let provider = SdkMeterProvider::builder()
        .with_resource(resource)
        .with_reader(reader)
        .build();
    let meter = provider.meter("thurbox");
    let transitions = meter
        .u64_counter("thurbox.session.transitions")
        .with_unit("{transition}")
        .with_description("Agent state changes")
        .build();
    let lifecycle = meter
        .u64_counter("thurbox.session.lifecycle")
        .with_unit("{event}")
        .with_description("Sessions created, stopped, restored, lost, deleted, ...")
        .build();
    let exits = meter
        .u64_counter("thurbox.session.exits")
        .with_unit("{exit}")
        .with_description("Agent processes that returned")
        .build();
    let duration = |name: &'static str, description: &'static str| {
        meter
            .f64_histogram(name)
            .with_unit("s")
            .with_description(description)
            .with_boundaries(DURATION_BOUNDS.to_vec())
            .build()
    };
    let turns = duration(
        "thurbox.turn.duration",
        "Time from entering working to leaving it",
    );
    let blocked = duration(
        "thurbox.blocked.duration",
        "Time spent blocked, waiting on a human",
    );
    let lifetime = duration("thurbox.session.lifetime", "Time from creation to going");
    let sessions = meter
        .i64_gauge("thurbox.sessions")
        .with_unit("{session}")
        .with_description("Live sessions by state")
        .build();
    for (measure, labels) in &summary.measures {
        match measure {
            Measure::Transition => transitions.add(1, labels),
            Measure::Lifecycle => lifecycle.add(1, labels),
            Measure::Exit => exits.add(1, labels),
            Measure::Turn(secs) => turns.record(*secs, labels),
            Measure::Blocked(secs) => blocked.record(*secs, labels),
            Measure::Lifetime(secs) => lifetime.record(*secs, labels),
        }
    }
    for (count, labels) in gauge {
        sessions.record(*count, labels);
    }
    let flushed = provider
        .force_flush()
        .map_err(|e| format!("metrics export: {e}"));
    let _ = provider.shutdown();
    flushed
}

fn send_logs(
    settings: &TelemetrySettings,
    resource: Resource,
    records: &[Record],
) -> Result<(), String> {
    let exporter = opentelemetry_otlp::LogExporter::builder()
        .with_http()
        .with_protocol(Protocol::HttpBinary)
        .with_endpoint(signal_url(settings, "logs"))
        .with_timeout(EXPORT_TIMEOUT)
        .with_headers(settings.headers.clone().into_iter().collect())
        .build()
        .map_err(|e| format!("logs exporter: {e}"))?;
    // Nothing exports on its own schedule: each flush below is one request
    // whose result is seen, which a background export's would not be.
    let processor = BatchLogProcessor::builder(exporter)
        .with_batch_config(
            BatchConfigBuilder::default()
                .with_scheduled_delay(Duration::from_secs(3600))
                .with_max_export_batch_size(RECORDS_PER_REQUEST * 2)
                .with_max_queue_size(RECORDS_PER_REQUEST * 4)
                .build(),
        )
        .build();
    let provider = SdkLoggerProvider::builder()
        .with_resource(resource)
        .with_log_processor(processor)
        .build();
    let logger = provider.logger("thurbox");
    let mut result = Ok(());
    for chunk in records.chunks(RECORDS_PER_REQUEST) {
        for record in chunk {
            let mut log = logger.create_log_record();
            log.set_event_name(record.name);
            log.set_timestamp(UNIX_EPOCH + Duration::from_millis(record.at_ms.max(0) as u64));
            log.set_observed_timestamp(SystemTime::now());
            log.set_severity_number(Severity::Info);
            log.set_body(record.name.into());
            log.add_attributes(
                record
                    .attributes
                    .iter()
                    .map(|kv| (kv.key.clone(), any_value(&kv.value))),
            );
            logger.emit(log);
        }
        if let Err(e) = provider.force_flush() {
            result = Err(format!("logs export: {e}"));
            break;
        }
    }
    let _ = provider.shutdown();
    result
}

/// A metric attribute's value as a log attribute's: the two APIs keep
/// separate value types, and the summary builds one set of attributes.
fn any_value(value: &opentelemetry::Value) -> AnyValue {
    match value {
        opentelemetry::Value::Bool(b) => AnyValue::Boolean(*b),
        opentelemetry::Value::I64(i) => AnyValue::Int(*i),
        opentelemetry::Value::F64(f) => AnyValue::Double(*f),
        other => AnyValue::String(other.to_string().into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_protocol_this_exporter_does_not_speak_is_skipped_not_failed() {
        let db = Database::open_in_memory().unwrap();
        let settings = TelemetrySettings {
            enabled: true,
            protocol: "grpc".into(),
            ..TelemetrySettings::default()
        };
        let identity = Identity {
            version: "test".into(),
            instance: "test".into(),
        };
        let report = export(&db, &settings, &identity, &[], &HashSet::new()).unwrap();
        assert!(report.skipped.unwrap().contains("grpc"));
    }

    #[test]
    fn the_signal_url_appends_the_path_once() {
        let settings = TelemetrySettings {
            endpoint: "http://collector:4318/".into(),
            ..TelemetrySettings::default()
        };
        assert_eq!(
            signal_url(&settings, "logs"),
            "http://collector:4318/v1/logs"
        );
    }

    #[test]
    fn a_shared_hosts_session_is_left_to_that_host() {
        let shared_hosts = HashSet::from(["devbox".to_string()]);
        assert!(on_shared_host("ssh:devbox", &shared_hosts));
        assert!(on_shared_host("ssh:devbox:rmux", &shared_hosts));
        assert!(!on_shared_host("ssh:private", &shared_hosts));
        assert!(!on_shared_host("local-tmux", &shared_hosts));
    }
}
