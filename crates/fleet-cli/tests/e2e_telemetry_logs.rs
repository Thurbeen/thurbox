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

fn exports_no_log_record(env: &Env, collector: &Collector) {
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
fn an_endpoint_alone_exports_no_log_record() {
    let collector = Collector::start();
    let mut env = Env::new("otel-logs-off");
    env.set_var("OTEL_EXPORTER_OTLP_ENDPOINT", &collector.endpoint);
    exports_no_log_record(&env, &collector);
}

#[test]
fn an_agents_otel_logs_exporter_does_not_turn_fleets_logs_on() {
    let collector = Collector::start();
    let mut env = Env::new("otel-logs-agent-env");
    env.set_var("OTEL_EXPORTER_OTLP_ENDPOINT", &collector.endpoint);
    env.set_var("OTEL_LOGS_EXPORTER", "otlp");
    exports_no_log_record(&env, &collector);
}

#[test]
fn no_log_record_carries_a_brief_a_message_or_an_environment_value() {
    let collector = Collector::start();
    let env = Env::new("otel-logs-secrets");
    with_telemetry(
        &env,
        &format!("endpoint = \"{}\"\nlogs = true\n", collector.endpoint),
    );
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
    let mut mistyped = task("t2", true);
    mistyped["spec"]["env"] = json!(format!("TOKEN={ENV_SECRET}"));
    let mistyped_env = env.apply(mistyped);
    assert_ne!(mistyped_env.code, Some(0), "{mistyped_env:?}");
    let mut mistyped = task("t3", true);
    mistyped["spec"]["workspace"] = json!(BRIEF_SECRET);
    let mistyped_brief = env.apply(mistyped);
    assert_ne!(mistyped_brief.code, Some(0), "{mistyped_brief:?}");
    let result = env.root.join("result.json");
    fs::write(
        &result,
        json!({"generation": MESSAGE_SECRET, "outcome": "shipped"}).to_string(),
    )
    .expect("result");
    let mistyped_result = env.fleet(&["result", "t1", "--file", result.to_str().unwrap()]);
    assert_ne!(mistyped_result.code, Some(0), "{mistyped_result:?}");

    let flag_output = env.fleet(&[
        "result",
        "t1",
        "--generation",
        "1",
        "--outcome",
        "shipped",
        "--output",
        MESSAGE_SECRET,
    ]);
    assert_ne!(flag_output.code, Some(0), "{flag_output:?}");
    let flag_outcome = env.fleet(&["result", "t1", "--generation", "1", "--outcome", ENV_SECRET]);
    assert_ne!(flag_outcome.code, Some(0), "{flag_outcome:?}");
    let declared_output = env.fleet(&["amend", "t1", "--text", "more", "--output", BRIEF_SECRET]);
    assert_ne!(declared_output.code, Some(0), "{declared_output:?}");

    wait_for("nine ERROR log records", || errors(&collector).len() >= 9);
    for log in collector.logs() {
        for secret in [BRIEF_SECRET, ENV_SECRET, MESSAGE_SECRET] {
            assert!(!log.body.contains(secret), "{secret} in {log:#?}");
        }
    }
    for run in [
        &mistyped_env,
        &mistyped_brief,
        &mistyped_result,
        &flag_output,
        &flag_outcome,
        &declared_output,
    ] {
        for secret in [BRIEF_SECRET, ENV_SECRET, MESSAGE_SECRET] {
            assert!(!run.stderr.contains(secret), "{secret} in {}", run.stderr);
        }
    }
}
