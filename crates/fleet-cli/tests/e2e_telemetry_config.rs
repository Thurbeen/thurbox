//! Acceptance: the `[telemetry]` table of Fleet's config file turns on
//! OpenTelemetry export with no `OTEL_*` variable set. `fleet up`, a one-shot
//! command and each session Fleet starts export to the file's collector,
//! with its headers and resource attributes. An `OTEL_*` variable wins over
//! the file's value for that variable, and the file's headers go only to
//! the file's endpoint.

mod support;

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::process::Child;
use std::time::{Duration, Instant};

use support::otlp::{Collector, Span};
use support::{Env, task};

const HEADERS: &str = "authorization=Bearer file-header-3c9a";

struct Stopped(Child);

impl Drop for Stopped {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn with_telemetry(env: &Env, table: &str) {
    let mut text = fs::read_to_string(&env.config).expect("config");
    text.push_str("\n[telemetry]\n");
    text.push_str(table);
    fs::write(&env.config, text).expect("config");
}

fn headers_file(env: &Env) -> String {
    let path = env.root.join("otel-headers");
    fs::write(&path, format!("{HEADERS}\n")).expect("headers");
    path.display().to_string()
}

fn file_table(endpoint: &str, headers: &Path) -> String {
    format!(
        "endpoint = \"{endpoint}\"\nheaders = {{ file = '{}' }}\nresource = {{ \"deployment.environment.name\" = \"from-file\" }}\n",
        headers.display()
    )
}

fn session_env(env: &Env, name: &str) -> BTreeMap<String, String> {
    env.host()
        .session(&env.session_key(name))
        .unwrap()
        .expect("a session")
        .env
}

fn named<'a>(spans: &'a [Span], name: &str) -> Vec<&'a Span> {
    spans.iter().filter(|s| s.name == name).collect()
}

fn wait_for(what: &str, mut ready: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while !ready() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn the_config_file_alone_makes_commands_the_manager_and_sessions_export() {
    let collector = Collector::start();
    let env = Env::new("otel-file-only");
    let headers = headers_file(&env);
    with_telemetry(&env, &file_table(&collector.endpoint, Path::new(&headers)));

    env.apply(task("t1", true)).ok();
    env.reconcile().ok();

    let spans = collector.spans();
    let apply = named(&spans, "fleet apply");
    assert!(!apply.is_empty(), "no `fleet apply` span in {spans:#?}");
    assert_eq!(apply[0].resource["service.name"], "fleet");
    assert_eq!(
        apply[0].resource["deployment.environment.name"],
        "from-file"
    );
    assert!(
        collector
            .headers()
            .iter()
            .all(|h| h.get("authorization").map(String::as_str) == Some("Bearer file-header-3c9a")),
        "each request carries the file's headers: {:?}",
        collector.headers()
    );

    let vars = session_env(&env, "t1");
    assert_eq!(vars["CLAUDE_CODE_ENABLE_TELEMETRY"], "1", "{vars:#?}");
    assert_eq!(vars["OTEL_EXPORTER_OTLP_ENDPOINT"], collector.endpoint);
    assert_eq!(vars["OTEL_EXPORTER_OTLP_HEADERS"], HEADERS);

    let up = Stopped(env.spawn(&["up", "--poll-ms", "100"]));
    wait_for("a `reconcile pass` span from fleet up", || {
        !named(&collector.spans(), "reconcile pass").is_empty()
    });
    drop(up);
}

#[test]
fn an_otel_variable_wins_over_the_file_and_the_files_headers_stay_with_its_endpoint() {
    let file_collector = Collector::start();
    let env_collector = Collector::start();
    let mut env = Env::new("otel-env-wins");
    let headers = headers_file(&env);
    with_telemetry(
        &env,
        &file_table(&file_collector.endpoint, Path::new(&headers)),
    );
    env.set_var("OTEL_EXPORTER_OTLP_ENDPOINT", &env_collector.endpoint);
    env.set_var(
        "OTEL_RESOURCE_ATTRIBUTES",
        "deployment.environment.name=from-env",
    );

    env.apply(task("t1", true)).ok();
    env.reconcile().ok();

    let spans = env_collector.spans();
    let apply = named(&spans, "fleet apply");
    assert!(!apply.is_empty(), "no `fleet apply` span in {spans:#?}");
    assert_eq!(apply[0].resource["deployment.environment.name"], "from-env");
    assert!(
        env_collector
            .headers()
            .iter()
            .all(|h| !h.contains_key("authorization")),
        "the file's headers never go to another collector"
    );
    assert!(
        file_collector.paths().is_empty(),
        "the file's endpoint lost"
    );

    let vars = session_env(&env, "t1");
    assert_eq!(vars["OTEL_EXPORTER_OTLP_ENDPOINT"], env_collector.endpoint);
    assert!(
        !vars.contains_key("OTEL_EXPORTER_OTLP_HEADERS"),
        "{vars:#?}"
    );
}

#[test]
fn a_signal_switched_off_in_the_file_is_not_exported() {
    let collector = Collector::start();
    let env = Env::new("otel-file-signals");
    with_telemetry(
        &env,
        &format!("endpoint = \"{}\"\ntraces = false\n", collector.endpoint),
    );
    env.apply(task("t1", true)).ok();
    env.reconcile().ok();
    wait_for("a metrics export", || {
        collector.paths().iter().any(|p| p == "/v1/metrics")
    });
    assert!(
        !collector.paths().iter().any(|p| p == "/v1/traces"),
        "{:?}",
        collector.paths()
    );
}

#[test]
fn an_invalid_telemetry_table_refuses_the_config() {
    for (table, needle) in [
        (
            "endpoint = \"http://127.0.0.1:9\"\nprotocol = \"grpc\"\n",
            "telemetry.protocol",
        ),
        (
            "endpoint = \"http://127.0.0.1:9\"\nheaders = { file = \"relative\" }\n",
            "telemetry.headers.file",
        ),
        (
            "endpoint = \"http://127.0.0.1:9\"\nresource = { \"service.name\" = \"x\" }\n",
            "service.name",
        ),
        ("endpont = \"http://127.0.0.1:9\"\n", "endpont"),
    ] {
        let env = Env::new("otel-file-invalid");
        with_telemetry(&env, table);
        env.fleet(&["list", "Task"]).code(1).stderr_has(needle);
    }
}

#[test]
fn doctor_reports_the_effective_telemetry_and_where_each_value_comes_from() {
    let mut env = Env::new("otel-doctor");
    let headers = headers_file(&env);
    with_telemetry(&env, &file_table("http://127.0.0.1:9", Path::new(&headers)));
    env.set_var("OTEL_METRICS_EXPORTER", "none");
    let run = env.fleet(&["doctor"]);
    run.stdout_has("telemetry")
        .stdout_has("http://127.0.0.1:9 (config file)")
        .stdout_has("traces")
        .stdout_has("metrics off (OTEL_METRICS_EXPORTER)")
        .stdout_has("headers (config file)");
    assert!(!run.stdout.contains("file-header"), "{}", run.stdout);
}

#[test]
fn an_unreadable_headers_file_turns_all_export_off_even_with_an_otel_endpoint() {
    let collector = Collector::start();
    let mut env = Env::new("otel-file-unreadable-headers");
    let missing = env.root.join("missing-otel-headers");
    with_telemetry(
        &env,
        &format!(
            "traces = false\nheaders = {{ file = '{}' }}\n",
            missing.display()
        ),
    );
    env.set_var("OTEL_EXPORTER_OTLP_ENDPOINT", &collector.endpoint);

    env.apply(task("t1", true))
        .ok()
        .stderr_has("warning: telemetry is off: telemetry.headers.file");
    env.reconcile().ok();

    std::thread::sleep(Duration::from_millis(300));
    assert!(collector.paths().is_empty(), "{:?}", collector.paths());
    let vars = session_env(&env, "t1");
    assert!(
        !vars.contains_key("CLAUDE_CODE_ENABLE_TELEMETRY"),
        "no agent telemetry while Fleet's is off: {vars:#?}"
    );
}

#[test]
fn a_signals_own_endpoint_variable_wins_over_the_files_endpoint_for_that_signal() {
    let file_collector = Collector::start();
    let traces_collector = Collector::start();
    let mut env = Env::new("otel-signal-endpoint-wins");
    with_telemetry(
        &env,
        &format!("endpoint = \"{}\"\n", file_collector.endpoint),
    );
    env.set_var(
        "OTEL_EXPORTER_OTLP_TRACES_ENDPOINT",
        &format!("{}/v1/traces", traces_collector.endpoint),
    );

    env.apply(task("t1", true)).ok();
    env.reconcile().ok();

    assert!(
        !named(&traces_collector.spans(), "fleet apply").is_empty(),
        "{:?}",
        traces_collector.paths()
    );
    wait_for("the file's collector to get metrics", || {
        file_collector.paths().iter().any(|p| p == "/v1/metrics")
    });
    assert!(
        !file_collector.paths().iter().any(|p| p == "/v1/traces"),
        "{:?}",
        file_collector.paths()
    );
}

#[test]
fn an_invalid_table_turns_all_export_off_even_where_the_config_is_not_loaded() {
    let collector = Collector::start();
    let mut env = Env::new("otel-file-invalid-off");
    with_telemetry(&env, "traces = false\nendpont = \"http://127.0.0.1:9\"\n");
    env.set_var("OTEL_EXPORTER_OTLP_ENDPOINT", &collector.endpoint);

    env.fleet(&["version"])
        .ok()
        .stderr_has("warning: telemetry is off:")
        .stderr_has("endpont");

    std::thread::sleep(Duration::from_millis(300));
    assert!(collector.paths().is_empty(), "{:?}", collector.paths());
}
