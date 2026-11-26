#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::time::Duration;

use opentelemetry::trace::{SpanContext, SpanId, TraceContextExt, TraceFlags, TraceId, TraceState};
use thurbox::fleet_link::FleetLink;

const TRACE: &str = "4bf92f3577b34da6a3ce929d0e0e4736";
const PARENT: &str = "00f067aa0ba902b7";

#[test]
fn a_bounded_call_carries_its_own_span_as_traceparent() {
    let endpoint_nothing_listens_on = "http://127.0.0.1:9";
    std::env::set_var("OTEL_EXPORTER_OTLP_ENDPOINT", endpoint_nothing_listens_on);
    std::env::set_var("OTEL_METRICS_EXPORTER", "none");
    let telemetry = otel_env::init("thurbox", "test", &[], otel_env::Logs::Off).expect("telemetry");

    let dir = tempfile::tempdir().expect("dir");
    let fleet = dir.path().join("fleet");
    std::fs::write(&fleet, "#!/bin/sh\nprintf '%s' \"$TRACEPARENT\"\n").expect("fleet");
    std::fs::set_permissions(&fleet, std::fs::Permissions::from_mode(0o755)).expect("chmod");

    let parent = SpanContext::new(
        TraceId::from_hex(TRACE).unwrap(),
        SpanId::from_hex(PARENT).unwrap(),
        TraceFlags::SAMPLED,
        true,
        TraceState::default(),
    );
    let seen = {
        let _guard = opentelemetry::Context::new()
            .with_remote_span_context(parent)
            .attach();
        FleetLink::local(fleet.to_string_lossy())
            .bounded_output(&["version"], Duration::from_secs(5))
            .expect("answered")
    };
    let traceparent = String::from_utf8_lossy(&seen.stdout).into_owned();
    let parts: Vec<&str> = traceparent.split('-').collect();
    assert_eq!(parts.len(), 4, "a traceparent: {traceparent:?}");
    assert_eq!(parts[1], TRACE, "the caller's trace: {traceparent}");
    assert_ne!(
        parts[2], PARENT,
        "the call's own span, not its caller's: {traceparent}"
    );
    telemetry.shutdown_within(otel_env::CALL_SHUTDOWN_BOUND);
}
