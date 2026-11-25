#![cfg(unix)]

#[path = "support/otlp_collector.rs"]
mod otlp_collector;

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use thurbox::fleet_link::FleetLink;

fn fleet_bin() -> Option<PathBuf> {
    let found = std::env::var_os("FLEET_BIN")
        .map(PathBuf::from)
        .or_else(|| {
            Path::new(env!("CARGO_BIN_EXE_thurbox-cli"))
                .parent()
                .map(|dir| dir.join("fleet"))
        })
        .filter(|p| p.is_file());
    if found.is_none() {
        assert!(
            std::env::var_os("CI").is_none(),
            "no fleet binary in CI, where the tests that drive Fleet must run"
        );
        eprintln!("skipping: no fleet binary; build it with `cargo build -p fleet-cli`");
    }
    found
}

fn script(path: &Path, body: &str) {
    std::fs::write(path, format!("#!/bin/sh\n{body}\n")).expect("script");
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
}

fn span_of(traceparent: &str) -> (String, String) {
    let parts: Vec<&str> = traceparent.trim().split('-').collect();
    assert_eq!(parts.len(), 4, "a traceparent: {traceparent:?}");
    (parts[1].to_string(), parts[2].to_string())
}

#[test]
fn opening_a_pane_is_a_client_span_that_fleet_and_the_stream_join() {
    let Some(fleet) = fleet_bin() else {
        return;
    };
    let collector = otlp_collector::Collector::start();
    let dir = tempfile::tempdir().expect("dir");
    std::fs::write(
        dir.path().join("config.toml"),
        "version = 1\n\n[data]\ndir = \"data\"\n",
    )
    .expect("config");
    std::env::set_var("FLEET_CONFIG", dir.path().join("config.toml"));
    std::env::set_var("OTEL_EXPORTER_OTLP_ENDPOINT", collector.endpoint());
    std::env::set_var("OTEL_METRICS_EXPORTER", "none");
    std::env::set_var("OTEL_LOGS_EXPORTER", "none");
    let telemetry = otel_env::init("thurbox", "test", &[], otel_env::Logs::Off).expect("telemetry");

    let refused = FleetLink::local(fleet.to_string_lossy()).stream("attach", "absent");
    assert!(refused.is_err(), "Fleet has no Task `absent`");

    let probe = dir.path().join("probe");
    let streamed = dir.path().join("streamed");
    let stream = dir.path().join("stream");
    script(
        &stream,
        &format!("printf %s \"$TRACEPARENT\" > '{}'", streamed.display()),
    );
    let stand_in = dir.path().join("fleet");
    script(
        &stand_in,
        &format!(
            "printf %s \"$TRACEPARENT\" > '{}'\nprintf '{{\"stream\":{{\"argv\":[\"{}\"]}}}}'",
            probe.display(),
            stream.display()
        ),
    );
    let status = FleetLink::local(stand_in.to_string_lossy())
        .stream("shell", "t2")
        .expect("the stream")
        .status()
        .expect("the stream runs");
    assert!(status.success());

    telemetry.shutdown_within(otel_env::CALL_SHUTDOWN_BOUND);
    let mut requests = collector.take();
    let deadline = Instant::now() + Duration::from_secs(10);
    let spans = loop {
        let spans = otlp_collector::spans(&requests);
        if spans.iter().any(|s| s.service == "fleet") || Instant::now() > deadline {
            break spans;
        }
        std::thread::sleep(Duration::from_millis(50));
        requests.extend(collector.take());
    };
    let ours = |verb: &str, task: &str| {
        spans
            .iter()
            .find(|s| {
                s.service == "thurbox"
                    && s.name == format!("fleet {verb}")
                    && s.attrs.get("fleet.command").map(String::as_str) == Some(verb)
                    && s.attrs.get("fleet.name").map(String::as_str) == Some(task)
            })
            .unwrap_or_else(|| panic!("no client span `fleet {verb}` for {task}: {spans:#?}"))
            .clone()
    };

    let attach = ours("attach", "absent");
    let fleets = spans
        .iter()
        .find(|s| s.service == "fleet" && s.name == "fleet attach")
        .unwrap_or_else(|| panic!("no `fleet attach` from fleet: {spans:#?}"));
    assert_eq!(fleets.trace_id, attach.trace_id, "{fleets:#?}");
    assert_eq!(fleets.parent, attach.span_id, "{fleets:#?}");

    let shell = ours("shell", "t2");
    for (what, file) in [("the probe", &probe), ("the stream", &streamed)] {
        let seen = std::fs::read_to_string(file).expect(what);
        assert_eq!(
            span_of(&seen),
            (shell.trace_id.clone(), shell.span_id.clone()),
            "{what} runs under the client span: {seen}"
        );
    }
}
