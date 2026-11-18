//! Acceptance: with logs on, each `warning: …` and `error: …` line `fleet`
//! prints is also one OTLP log record, with the same text and a WARN or
//! ERROR severity, under `service.name=fleet`. Logs are opt-in: an endpoint
//! alone sends none. No record carries a brief, a message or an
//! environment value.

mod support;

use std::fs;
use std::time::{Duration, Instant};

use serde_json::json;
use support::otlp::{Collector, Log};
use support::{Env, base_config, task};

const BRIEF_SECRET: &str = "brief-sentinel-log-5b1e";
const ENV_SECRET: &str = "env-sentinel-log-a0d4";
const MESSAGE_SECRET: &str = "message-sentinel-log-77c9";

fn with_telemetry(env: &Env, table: &str) {
    let mut text = fs::read_to_string(&env.config).expect("config");
    text.push_str("\n[telemetry]\n");
    text.push_str(table);
    fs::write(&env.config, text).expect("config");
}

fn wait_for(what: &str, mut ready: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while !ready() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn error_line(stderr: &str) -> String {
    stderr
        .lines()
        .find_map(|l| l.strip_prefix("error: "))
        .unwrap_or_else(|| panic!("no `error:` line in {stderr}"))
        .to_string()
}

fn errors(collector: &Collector) -> Vec<Log> {
    collector
        .logs()
        .into_iter()
        .filter(|l| l.severity == "ERROR")
        .collect()
}

#[test]
fn a_failing_command_exports_its_error_line_when_the_config_turns_logs_on() {
    let collector = Collector::start();
    let env = Env::new("otel-logs-file");
    with_telemetry(
        &env,
        &format!("endpoint = \"{}\"\nlogs = true\n", collector.endpoint),
    );

    let run = env.fleet(&["get", "Task", "absent"]);
    run.code(3);
    let text = error_line(&run.stderr);

    wait_for("an ERROR log record", || !errors(&collector).is_empty());
    let logs = errors(&collector);
    assert_eq!(logs.len(), 1, "{logs:#?}");
    assert_eq!(logs[0].body, text, "{logs:#?}");
    assert_eq!(logs[0].resource["service.name"], "fleet", "{logs:#?}");
}

#[test]
fn otel_logs_exporter_turns_logs_on_without_the_config() {
    let collector = Collector::start();
    let mut env = Env::new("otel-logs-env");
    env.set_var("OTEL_EXPORTER_OTLP_ENDPOINT", &collector.endpoint);
    env.set_var("OTEL_LOGS_EXPORTER", "otlp");

    let run = env.fleet(&["get", "Task", "absent"]);
    run.code(3);
    let text = error_line(&run.stderr);

    wait_for("an ERROR log record", || !errors(&collector).is_empty());
    assert_eq!(errors(&collector)[0].body, text);
}

#[test]
fn an_endpoint_alone_exports_no_log_record() {
    let collector = Collector::start();
    let mut env = Env::new("otel-logs-off");
    env.set_var("OTEL_EXPORTER_OTLP_ENDPOINT", &collector.endpoint);

    env.fleet(&["get", "Task", "absent"]).code(3);

    wait_for("the command's span", || {
        collector.spans().iter().any(|s| s.name == "fleet get")
    });
    assert!(
        !collector.paths().iter().any(|p| p == "/v1/logs"),
        "{:?}",
        collector.paths()
    );
}

#[test]
fn no_log_record_carries_a_brief_a_message_or_an_environment_value() {
    let collector = Collector::start();
    let mut env = Env::new("otel-logs-secrets");
    env.set_var("OTEL_EXPORTER_OTLP_ENDPOINT", &collector.endpoint);
    env.set_var("OTEL_LOGS_EXPORTER", "otlp");
    env.apply(base_config()).ok();
    let mut t = task("t1", true);
    t["spec"]["brief"] = json!(format!("Do the work. {BRIEF_SECRET}"));
    t["spec"]["env"] = json!({"TOKEN": ENV_SECRET});
    env.apply(t).ok();
    env.reconcile().ok();

    env.fleet(&[
        "amend",
        "t1",
        "--text",
        MESSAGE_SECRET,
        "--if-generation",
        "99",
    ])
    .code(5);
    env.fleet(&["send", "absent", MESSAGE_SECRET]).code(3);
    let mut bad = task("t1", true);
    bad["spec"]["brief"] = json!(format!("Do the work. {BRIEF_SECRET}"));
    bad["spec"]["env"] = json!({"TOKEN": ENV_SECRET});
    bad["spec"]["unknownField"] = json!(BRIEF_SECRET);
    let refused = env.apply(bad);
    assert_ne!(refused.code, Some(0), "{refused:?}");

    wait_for("three ERROR log records", || errors(&collector).len() >= 3);
    for log in collector.logs() {
        for secret in [BRIEF_SECRET, ENV_SECRET, MESSAGE_SECRET] {
            assert!(!log.body.contains(secret), "{secret} in {log:#?}");
        }
    }
}
