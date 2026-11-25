mod support;

use std::collections::BTreeMap;

use serde_json::json;
use support::otlp::Collector;
use support::{Env, object, task};

const HEADERS: &str = "authorization=Bearer agent-header-5e1d";

fn exporting(env: &mut Env, endpoint: &str) {
    env.set_var("OTEL_EXPORTER_OTLP_ENDPOINT", endpoint);
    env.set_var("OTEL_EXPORTER_OTLP_HEADERS", HEADERS);
    env.set_var("OTEL_SERVICE_NAME", "fleet-e2e");
    env.set_var(
        "OTEL_RESOURCE_ATTRIBUTES",
        "deployment.environment.name=e2e",
    );
}

fn session(env: &Env, name: &str) -> (BTreeMap<String, String>, Vec<String>) {
    let s = env
        .host()
        .session(&env.session_key(name))
        .unwrap()
        .expect("a session");
    (s.env, s.argv)
}

fn agent_vars(env: &BTreeMap<String, String>) -> Vec<&String> {
    env.keys()
        .filter(|k| {
            k.starts_with("OTEL_")
                || k.as_str() == "CLAUDE_CODE_ENABLE_TELEMETRY"
                || k.as_str() == "TRACEPARENT"
                || k.as_str() == "TRACESTATE"
        })
        .collect()
}

#[test]
fn a_claude_session_exports_its_own_telemetry_to_fleets_collector_tagged_by_task() {
    let collector = Collector::start();
    let mut env = Env::new("agent-otel-claude");
    exporting(&mut env, &collector.endpoint);
    env.apply(task("t1", true)).ok();
    env.reconcile().ok();
    let uid = env.get("t1")["metadata"]["uid"]
        .as_str()
        .unwrap()
        .to_string();

    let (vars, _) = session(&env, "t1");
    assert_eq!(vars["CLAUDE_CODE_ENABLE_TELEMETRY"], "1", "{vars:#?}");
    assert_eq!(vars["OTEL_METRICS_EXPORTER"], "otlp");
    assert_eq!(vars["OTEL_LOGS_EXPORTER"], "otlp");
    assert_eq!(vars["OTEL_EXPORTER_OTLP_ENDPOINT"], collector.endpoint);
    assert_eq!(vars["OTEL_EXPORTER_OTLP_PROTOCOL"], "http/protobuf");
    assert_eq!(vars["OTEL_EXPORTER_OTLP_HEADERS"], HEADERS);
    // The agent's own name, never Fleet's.
    assert_eq!(vars["OTEL_SERVICE_NAME"], "claude");
    let resource = &vars["OTEL_RESOURCE_ATTRIBUTES"];
    for attr in [
        "service.name=claude".to_string(),
        "fleet.task=t1".to_string(),
        format!("fleet.uid={uid}"),
    ] {
        assert!(
            resource.split(',').any(|a| a == attr),
            "{attr} in {resource}"
        );
    }
    assert!(
        !resource.contains("deployment.environment.name"),
        "{resource}"
    );
    // Agents start their own traces; prompt content stays off.
    for absent in [
        "TRACEPARENT",
        "TRACESTATE",
        "OTEL_LOG_USER_PROMPTS",
        "OTEL_LOG_TOOL_DETAILS",
    ] {
        assert!(!vars.contains_key(absent), "{absent} in {vars:#?}");
    }
    // Fleet exported too: the agent's collector is Fleet's own.
    assert!(!collector.spans().is_empty());
    // The collector's headers are in no frozen launch: telemetry is added
    // at each start, from Fleet's environment then.
    let launches = env.data.join("launches");
    if launches.is_dir() {
        for f in std::fs::read_dir(launches).unwrap().flatten() {
            let text = std::fs::read_to_string(f.path()).unwrap();
            assert!(!text.contains("agent-header"), "{text}");
        }
    }
}

#[test]
fn a_codex_session_takes_its_telemetry_as_config_overrides() {
    let collector = Collector::start();
    let mut env = Env::new("agent-otel-codex");
    exporting(&mut env, &collector.endpoint);
    let mut t = task("cx", true);
    t["spec"]["harness"] = json!("codex");
    env.apply(t).ok();
    env.reconcile().ok();
    let (vars, argv) = session(&env, "cx");
    let otel: Vec<&String> = argv.iter().filter(|a| a.starts_with("otel.")).collect();
    assert!(
        otel.iter()
            .any(|a| a.contains(&format!("{}/v1/logs", collector.endpoint))),
        "{argv:#?}"
    );
    assert!(otel.iter().any(|a| a.contains("binary")), "{argv:#?}");
    // A header is a secret: never in an argument list.
    assert!(
        !argv.iter().any(|a| a.contains("agent-header")),
        "{argv:#?}"
    );
    assert!(!vars.contains_key("TRACEPARENT"), "{vars:#?}");
    assert!(!vars.contains_key("CLAUDE_CODE_ENABLE_TELEMETRY"));
}

#[test]
fn without_fleets_export_a_session_gets_no_telemetry() {
    let env = Env::new("agent-otel-off");
    env.apply(task("t1", true)).ok();
    env.reconcile().ok();
    let (vars, _) = session(&env, "t1");
    assert!(agent_vars(&vars).is_empty(), "{vars:#?}");

    let collector = Collector::start();
    let mut env = Env::new("agent-otel-disabled");
    exporting(&mut env, &collector.endpoint);
    env.set_var("OTEL_SDK_DISABLED", "true");
    env.apply(task("t1", true)).ok();
    env.reconcile().ok();
    let (vars, _) = session(&env, "t1");
    assert!(agent_vars(&vars).is_empty(), "{vars:#?}");
}

#[test]
fn agent_telemetry_false_on_the_task_or_its_task_config_turns_it_off() {
    let collector = Collector::start();
    let mut env = Env::new("agent-otel-opt-out");
    exporting(&mut env, &collector.endpoint);
    let mut own = task("own", true);
    own["spec"]["agentTelemetry"] = json!(false);
    env.apply(own).ok();
    env.apply(object(
        "TaskConfig",
        "quiet",
        json!({"agentTelemetry": false}),
    ))
    .ok();
    let mut configured = task("configured", true);
    configured["spec"]["configRef"] = json!({"name": "quiet"});
    env.apply(configured).ok();
    // The Task's own field overrides its TaskConfig's.
    let mut back_on = task("back-on", true);
    back_on["spec"]["configRef"] = json!({"name": "quiet"});
    back_on["spec"]["agentTelemetry"] = json!(true);
    env.apply(back_on).ok();
    env.reconcile().ok();
    for name in ["own", "configured"] {
        let (vars, _) = session(&env, name);
        assert!(agent_vars(&vars).is_empty(), "{name}: {vars:#?}");
    }
    let (vars, _) = session(&env, "back-on");
    assert_eq!(vars["CLAUDE_CODE_ENABLE_TELEMETRY"], "1", "{vars:#?}");
}

#[test]
fn a_task_with_its_own_otlp_endpoint_gets_none_of_fleets_collector() {
    let collector = Collector::start();
    let mut env = Env::new("agent-otel-own-endpoint");
    exporting(&mut env, &collector.endpoint);
    for (name, var) in [
        ("base", "OTEL_EXPORTER_OTLP_ENDPOINT"),
        ("signal", "otel_exporter_otlp_metrics_endpoint"),
    ] {
        let mut t = task(name, true);
        t["spec"]["env"] = json!({ var: "http://elsewhere:4318" });
        env.apply(t).ok();
    }
    env.apply(object(
        "TaskConfig",
        "elsewhere",
        json!({"env": {"OTEL_EXPORTER_OTLP_TRACES_ENDPOINT": "http://elsewhere:4318/v1/traces"}}),
    ))
    .ok();
    let mut configured = task("configured", true);
    configured["spec"]["configRef"] = json!({"name": "elsewhere"});
    env.apply(configured).ok();
    env.reconcile().ok();
    for name in ["base", "signal", "configured"] {
        let (vars, _) = session(&env, name);
        let fleets: Vec<_> = vars
            .iter()
            .filter(|(k, v)| {
                v.contains(&collector.endpoint)
                    || v.contains("agent-header")
                    || k.as_str() == "CLAUDE_CODE_ENABLE_TELEMETRY"
                    || k.as_str() == "OTEL_RESOURCE_ATTRIBUTES"
            })
            .collect();
        assert!(fleets.is_empty(), "{name}: {vars:#?}");
    }
}

#[test]
fn a_revive_and_a_resume_take_fleets_collector_as_it_is_then() {
    let first = Collector::start();
    let second = Collector::start();
    let mut env = Env::new("agent-otel-restart");
    exporting(&mut env, &first.endpoint);
    let mut cx = task("cx", true);
    cx["spec"]["harness"] = json!("codex");
    env.apply(json!([
        object("TaskConfig", "pausing", json!({"pauseAfterMs": 600_000})),
        task("t1", true),
        cx
    ]))
    .ok();
    let mut paused = task("p1", true);
    paused["spec"]["configRef"] = json!({"name": "pausing"});
    env.apply(paused).ok();
    env.reconcile().ok();
    assert_eq!(session(&env, "t1").0["OTEL_EXPORTER_OTLP_HEADERS"], HEADERS);

    // Fleet now exports elsewhere, with other headers.
    env.set_var("OTEL_EXPORTER_OTLP_ENDPOINT", &second.endpoint);
    env.set_var("OTEL_EXPORTER_OTLP_HEADERS", "authorization=Bearer second");
    let host = env.host();
    for name in ["t1", "cx"] {
        host.update(|s| {
            let x = s.sessions.get_mut(&env.session_key(name)).unwrap();
            x.lost = true;
            x.ready = false;
        })
        .unwrap();
    }
    // p1 goes idle long enough to pause, then a message resumes it.
    signal_idle(&env, "p1");
    env.reconcile().ok();
    expire_idle(&env, "p1");
    env.reconcile().ok();
    assert!(
        env.host()
            .session(&env.session_key("p1"))
            .unwrap()
            .unwrap()
            .paused
    );
    env.fleet(&["send", "p1", "wake up"]).ok();
    env.reconcile().ok();
    assert!(env.calls("revive") >= 2 && env.calls("resume") >= 1);

    for name in ["t1", "p1"] {
        let (vars, _) = session(&env, name);
        assert_eq!(
            vars["OTEL_EXPORTER_OTLP_ENDPOINT"], second.endpoint,
            "{name}: {vars:#?}"
        );
        assert_eq!(
            vars["OTEL_EXPORTER_OTLP_HEADERS"], "authorization=Bearer second",
            "{name}"
        );
    }
    let restarted = env
        .host()
        .session(&env.session_key("cx"))
        .unwrap()
        .unwrap()
        .restarts;
    let last = restarted.last().expect("a revive");
    assert!(
        last.iter()
            .any(|a| a.contains(&format!("{}/v1/logs", second.endpoint))),
        "{last:#?}"
    );
    assert!(
        !last.iter().any(|a| a.contains(&first.endpoint)),
        "{last:#?}"
    );
}

fn signal_idle(env: &Env, name: &str) {
    let file = env
        .data
        .join("signals")
        .join(format!("{}.jsonl", env.session_key(name)));
    let mut cmd = env.command(&["signal", "idle"]);
    cmd.env("FLEET_SIGNAL_FILE", &file);
    support::wait(cmd.spawn().expect("fleet signal")).ok();
}

fn expire_idle(env: &Env, name: &str) {
    let path = env.object_path("Task", name);
    let mut obj: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert!(obj["status"]["idleSince"].is_u64(), "{:#}", obj["status"]);
    obj["status"]["idleSince"] = json!(0);
    std::fs::write(&path, obj.to_string()).unwrap();
}
