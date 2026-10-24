//! What `thurbox-cli automation tick` exports over OTLP: thurbox's own session
//! metrics and one log record per session event (ADR-37, issue #1465 phase 2).
//!
//! Driven through the real binary against a real database file and an
//! in-process OTLP/HTTP receiver, because the properties under test cross
//! processes: the events are written by one connection and exported by a
//! short-lived `tick` process that holds nothing between runs, so the cursor
//! in the database is the only thing that stops a row going out twice or not
//! at all.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

use opentelemetry_proto::tonic::collector::metrics::v1::ExportMetricsServiceRequest;
use opentelemetry_proto::tonic::common::v1::{any_value, KeyValue};
use opentelemetry_proto::tonic::metrics::v1::{metric, number_data_point, Metric};
use prost::Message;
use thurbox::session::SessionId;
use thurbox::storage::Database;
use thurbox::sync::SharedSession;

#[path = "support/tmux_server.rs"]
mod tmux_server;

use tmux_server::TmuxServer;

#[path = "support/otlp_collector.rs"]
mod otlp_collector;

use otlp_collector::{logs, Collector, Request};

const AGENTS_TOML: &str = r#"
config_version = 1
default = "claude"

[[agents]]
name = "claude"
command = "claude"
"#;

/// A throwaway thurbox instance with `[telemetry]` pointed at a receiver.
struct Env {
    root: tempfile::TempDir,
    server: TmuxServer,
}

impl Env {
    fn new(telemetry: &str) -> Self {
        let root = tempfile::TempDir::new().expect("tempdir");
        for sub in ["home", "config", "data"] {
            std::fs::create_dir_all(root.path().join(sub)).expect("mkdir");
        }
        let config = root.path().join("config");
        std::fs::write(config.join("agents.toml"), AGENTS_TOML).expect("agents.toml");
        std::fs::write(config.join("settings.toml"), telemetry).expect("settings.toml");
        Self {
            root,
            server: TmuxServer::private("thurbox-telemetry-test"),
        }
    }

    fn path(&self, sub: &str) -> PathBuf {
        self.root.path().join(sub)
    }

    fn db(&self) -> Database {
        Database::open(&self.path("data").join("thurbox.db")).expect("open the database")
    }

    fn tick(&self) {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_thurbox-cli"));
        cmd.args(["automation", "tick"]);
        cmd.env("HOME", self.path("home"));
        cmd.env("USERPROFILE", self.path("home"));
        cmd.env("XDG_DATA_HOME", self.path("home").join("xdg-data"));
        cmd.env("XDG_CONFIG_HOME", self.path("home").join("xdg-config"));
        cmd.env("THURBOX_CONFIG_DIR", self.path("config"));
        cmd.env("THURBOX_DATA_DIR", self.path("data"));
        self.server.scope(&mut cmd);
        cmd.env_remove("THURBOX_SESSION");
        cmd.env_remove("THURBOX_SESSION_ID");
        // A suite run inside a session with [telemetry] on inherits these.
        for (key, _) in std::env::vars() {
            if key.starts_with("OTEL_") {
                cmd.env_remove(key);
            }
        }
        let out = cmd.output().expect("run automation tick");
        assert!(
            out.status.success(),
            "automation tick failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

fn settings(endpoint: &str) -> String {
    format!("[telemetry]\nenabled = true\nendpoint = \"{endpoint}\"\n")
}

fn seed(db: &Database, name: &str) -> SessionId {
    let row = SharedSession {
        id: SessionId::default(),
        name: name.into(),
        agent: "claude".into(),
        backend_id: "%1".into(),
        backend_type: "local-tmux".into(),
        agent_session_id: None,
        cwd: None,
        additional_dirs: Vec::new(),
        worktrees: Vec::new(),
        shell_backend_id: None,
        parent_session_id: None,
        display_order: None,
        tombstone: false,
        tombstone_at: None,
        key: None,
    };
    db.upsert_session(&row).expect("seed a session row");
    row.id
}

/// Move a session through `states`, far enough apart that each duration is
/// measurably non-zero.
fn walk(db: &Database, id: SessionId, states: &[&str]) {
    for state in states {
        std::thread::sleep(Duration::from_millis(20));
        db.set_hook_state(id, state).expect("record a state");
    }
}

fn metrics(requests: &[Request]) -> (Vec<KeyValue>, Vec<Metric>) {
    let mut resource = Vec::new();
    let mut out = Vec::new();
    for (path, body) in requests {
        if path != "/v1/metrics" {
            continue;
        }
        let request = ExportMetricsServiceRequest::decode(body.as_slice())
            .expect("a protobuf metrics request");
        for rm in request.resource_metrics {
            resource.extend(rm.resource.map(|r| r.attributes).unwrap_or_default());
            out.extend(rm.scope_metrics.into_iter().flat_map(|s| s.metrics));
        }
    }
    (resource, out)
}

fn attrs(kvs: &[KeyValue]) -> BTreeMap<String, String> {
    kvs.iter()
        .map(|kv| {
            let value = match kv.value.as_ref().and_then(|v| v.value.as_ref()) {
                Some(any_value::Value::StringValue(s)) => s.clone(),
                Some(any_value::Value::IntValue(i)) => i.to_string(),
                other => format!("{other:?}"),
            };
            (kv.key.clone(), value)
        })
        .collect()
}

fn find<'a>(metrics: &'a [Metric], name: &str) -> &'a Metric {
    metrics
        .iter()
        .find(|m| m.name == name)
        .unwrap_or_else(|| panic!("no {name} metric in {:?}", names(metrics)))
}

fn names(metrics: &[Metric]) -> Vec<&str> {
    metrics.iter().map(|m| m.name.as_str()).collect()
}

/// Each histogram data point as (its attributes, its count).
fn histogram(metric: &Metric) -> Vec<(BTreeMap<String, String>, u64)> {
    match &metric.data {
        Some(metric::Data::Histogram(h)) => h
            .data_points
            .iter()
            .map(|p| (attrs(&p.attributes), p.count))
            .collect(),
        other => panic!("{} is not a histogram: {other:?}", metric.name),
    }
}

fn int_points(metric: &Metric) -> Vec<(BTreeMap<String, String>, i64)> {
    let points = match &metric.data {
        Some(metric::Data::Sum(s)) => &s.data_points,
        Some(metric::Data::Gauge(g)) => &g.data_points,
        other => panic!("{} has no number points: {other:?}", metric.name),
    };
    points
        .iter()
        .map(|p| {
            let value = match p.value {
                Some(number_data_point::Value::AsInt(i)) => i,
                Some(number_data_point::Value::AsDouble(d)) => d as i64,
                None => 0,
            };
            (attrs(&p.attributes), value)
        })
        .collect()
}

#[test]
fn tick_exports_session_metrics_and_one_log_record_per_event() {
    let collector = Collector::start();
    let env = Env::new(&settings(&collector.endpoint()));
    let db = env.db();
    let id = seed(&db, "worker");
    // The first export starts from the log as it stands: history from before
    // telemetry was turned on is not replayed.
    env.tick();
    collector.take();

    walk(&db, id, &["working", "blocked", "working", "done"]);
    env.tick();
    let received = collector.take();

    let records = logs(&received);
    let seen: Vec<(String, BTreeMap<String, String>)> = records
        .iter()
        .map(|r| (r.event_name.clone(), attrs(&r.attributes)))
        .collect();
    let transitions: Vec<(&str, &str, &str)> = seen
        .iter()
        .map(|(name, a)| {
            (
                name.as_str(),
                a.get("from").map_or("", String::as_str),
                a.get("to").map_or("", String::as_str),
            )
        })
        .collect();
    assert_eq!(
        transitions,
        vec![
            ("thurbox.session.state_changed", "", "working"),
            ("thurbox.session.state_changed", "working", "blocked"),
            ("thurbox.session.state_changed", "blocked", "working"),
            ("thurbox.session.state_changed", "working", "done"),
        ],
        "one record per session event, in order"
    );
    for (_, a) in &seen {
        assert_eq!(a.get("thurbox.session.id"), Some(&id.to_string()));
        assert_eq!(
            a.get("thurbox.session.name").map(String::as_str),
            Some("worker")
        );
        assert_eq!(a.get("agent").map(String::as_str), Some("claude"));
    }
    // Stamped with when the event happened, not when it was exported.
    let logged: Vec<u64> = db
        .session_events_since(0, Some(id), 100)
        .unwrap()
        .iter()
        .filter(|e| e.reason == "state")
        .map(|e| e.at_ms as u64 * 1_000_000)
        .collect();
    let stamped: Vec<u64> = records.iter().map(|r| r.time_unix_nano).collect();
    assert_eq!(stamped, logged);

    let (resource, metrics) = metrics(&received);
    let resource = attrs(&resource);
    assert_eq!(
        resource.get("service.name").map(String::as_str),
        Some("thurbox")
    );
    assert!(resource.contains_key("thurbox.instance"), "{resource:?}");

    let moved: i64 = int_points(find(&metrics, "thurbox.session.transitions"))
        .iter()
        .map(|(_, v)| v)
        .sum();
    assert_eq!(moved, 4);

    let mut turns: Vec<(String, u64)> = histogram(find(&metrics, "thurbox.turn.duration"))
        .into_iter()
        .map(|(a, count)| (a["outcome"].clone(), count))
        .collect();
    turns.sort();
    assert_eq!(
        turns,
        vec![("blocked".to_string(), 1), ("done".to_string(), 1)]
    );
    let blocked: u64 = histogram(find(&metrics, "thurbox.blocked.duration"))
        .iter()
        .map(|(_, c)| c)
        .sum();
    assert_eq!(blocked, 1);

    let sessions = int_points(find(&metrics, "thurbox.sessions"));
    assert!(
        sessions
            .iter()
            .any(|(a, v)| a.get("state").map(String::as_str) == Some("done")
                && a.get("agent").map(String::as_str) == Some("claude")
                && *v == 1),
        "{sessions:?}"
    );

    // Nothing new happened: the next tick sends no record a second time.
    env.tick();
    assert!(logs(&collector.take()).is_empty());
}

#[test]
fn a_refused_export_leaves_its_rows_for_the_next_tick() {
    let collector = Collector::start();
    let env = Env::new(&settings(&collector.endpoint()));
    let db = env.db();
    let id = seed(&db, "worker");
    env.tick();
    collector.take();

    walk(&db, id, &["working"]);
    collector.answer_with(400);
    env.tick();
    assert!(
        !collector.take().is_empty(),
        "the tick did not try to export"
    );

    collector.answer_with(200);
    env.tick();
    let records = logs(&collector.take());
    let to: Vec<String> = records
        .iter()
        .filter_map(|r| attrs(&r.attributes).get("to").cloned())
        .collect();
    assert_eq!(to, vec!["working".to_string()]);
}

#[test]
fn nothing_is_sent_while_telemetry_is_off() {
    let collector = Collector::start();
    let env = Env::new(&format!(
        "[telemetry]\nenabled = false\nendpoint = \"{}\"\n",
        collector.endpoint()
    ));
    let db = env.db();
    let id = seed(&db, "worker");
    env.tick();
    walk(&db, id, &["working"]);
    env.tick();
    assert!(collector.take().is_empty());
}

#[test]
fn session_attributes_label_events_and_only_per_session_metrics() {
    for per_session in [false, true] {
        let collector = Collector::start();
        let env = Env::new(&format!(
            "{}per_session = {per_session}\n",
            settings(&collector.endpoint())
        ));
        let db = env.db();
        let id = seed(&db, "labelled");
        db.set_session_meta(id, "thurbox.telemetry_attributes",
            r#"{"factory.task.id":"topic/task","agent":"spoofed","state":"spoofed","thurbox.session.id":"spoofed"}"#).unwrap();
        env.tick();
        collector.take();
        walk(&db, id, &["working", "done"]);
        env.tick();
        let received = collector.take();
        let records = logs(&received);
        assert_eq!(records.len(), 2);
        for record in records {
            let labels = attrs(&record.attributes);
            assert_eq!(
                labels.get("factory.task.id").map(String::as_str),
                Some("topic/task")
            );
            assert_eq!(labels.get("thurbox.session.id"), Some(&id.to_string()));
            assert_eq!(labels.get("agent").map(String::as_str), Some("claude"));
        }
        let (_, measures) = metrics(&received);
        for name in ["thurbox.sessions", "thurbox.session.transitions"] {
            for (labels, _) in int_points(find(&measures, name)) {
                assert_eq!(
                    labels.get("factory.task.id").map(String::as_str),
                    per_session.then_some("topic/task")
                );
                assert_eq!(labels.get("agent").map(String::as_str), Some("claude"));
                assert_ne!(labels.get("state").map(String::as_str), Some("spoofed"));
            }
        }
    }
}

#[test]
fn force_delete_keeps_task_labels_on_pending_events_and_lifetime_metrics() {
    let collector = Collector::start();
    let env = Env::new(&format!(
        "{}per_session = true\n",
        settings(&collector.endpoint())
    ));
    let db = env.db();
    let id = seed(&db, "labelled-delete");
    db.set_session_meta(
        id,
        "thurbox.telemetry_attributes",
        r#"{"factory.task.id":"topic/task"}"#,
    )
    .unwrap();
    env.tick();
    collector.take();
    walk(&db, id, &["working", "done"]);
    db.force_delete_session(id).unwrap();
    env.tick();
    let received = collector.take();
    let records = logs(&received);
    assert_eq!(records.len(), 3);
    for record in records {
        assert_eq!(
            attrs(&record.attributes)
                .get("factory.task.id")
                .map(String::as_str),
            Some("topic/task")
        );
    }
    let (_, measures) = metrics(&received);
    for (labels, _) in histogram(find(&measures, "thurbox.session.lifetime")) {
        assert_eq!(
            labels.get("factory.task.id").map(String::as_str),
            Some("topic/task")
        );
    }
}
