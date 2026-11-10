//! Acceptance: OpenTelemetry export through the real `fleet` binary. The
//! standard OTEL_* variables point it at an in-process OTLP/HTTP collector;
//! the commands' and the manager's spans and metrics must arrive there with
//! the resource's identity and outcome, and never a brief, a message or an
//! environment value. Unset or disabled, nothing is sent; an unreachable
//! collector changes nothing a command does.

mod support;

use std::collections::BTreeSet;
use std::net::TcpListener;
use std::time::{Duration, Instant};

use fleet_api::forge::{
    ChangeSnapshot, ChangeState, Checks, HeadSeen, Mergeable, ReviewDecision, Threads,
};
use fleet_forge::ForgeFault;
use serde_json::{Value, json};
use support::otlp::{Collector, Point, Span};
use support::{Env, FORGE_HOST, base_config, change_request, derived_name, object, task};

const BRIEF_SECRET: &str = "brief-sentinel-7f3a";
const ENV_SECRET: &str = "env-sentinel-91c2";
const MESSAGE_SECRET: &str = "message-sentinel-4d8e";

fn exporting(env: &mut Env, endpoint: &str) {
    env.set_var("USER", "tester");
    env.set_var("OTEL_EXPORTER_OTLP_ENDPOINT", endpoint);
    env.set_var("OTEL_SERVICE_NAME", "fleet-e2e");
    env.set_var(
        "OTEL_RESOURCE_ATTRIBUTES",
        "deployment.environment.name=e2e",
    );
}

fn secret_task() -> Value {
    let mut t = task("t1", true);
    t["spec"]["brief"] = json!(format!("Do the work. {BRIEF_SECRET}"));
    t["spec"]["env"] = json!({"TOKEN": ENV_SECRET});
    t
}

fn named<'a>(spans: &'a [Span], name: &str) -> Vec<&'a Span> {
    spans.iter().filter(|s| s.name == name).collect()
}

fn effect<'a>(spans: &'a [Span], kind: &str) -> &'a Span {
    spans
        .iter()
        .find(|s| s.name == format!("effect {kind}"))
        .unwrap_or_else(|| panic!("no `effect {kind}` span in {:#?}", names(spans)))
}

fn names(spans: &[Span]) -> BTreeSet<String> {
    spans.iter().map(|s| s.name.clone()).collect()
}

fn points<'a>(all: &'a [Point], metric: &str) -> Vec<&'a Point> {
    all.iter().filter(|p| p.metric == metric).collect()
}

/// The full lifecycle: spawn, brief, a message delivered, release. Every
/// command is one trace; the reconcile commands carry their passes, objects
/// and effects beneath them.
#[test]
fn a_task_lifecycle_exports_its_spans_and_metrics_to_the_collector() {
    let collector = Collector::start();
    let mut env = Env::new("otel-lifecycle");
    exporting(&mut env, &collector.endpoint);
    env.apply(base_config()).ok();
    env.apply(secret_task()).ok();
    env.reconcile().ok();
    env.fleet(&["send", "t1", MESSAGE_SECRET]).ok();
    // One pass, so the gauge's last value is the queued message.
    env.fleet(&["reconcile", "--max-passes", "1"]).ok();
    env.reconcile().ok();
    env.fleet(&["amend", "t1", "--text", MESSAGE_SECRET]).ok();
    env.fleet(&[
        "amend",
        "t1",
        "--text",
        MESSAGE_SECRET,
        "--if-generation",
        "1",
    ])
    .code(5);
    env.reconcile().ok();
    env.host()
        .set_at_rest(&env.session_key("t1"), true)
        .unwrap();
    env.fleet(&["delete", "Task", "t1"]).ok();
    env.reconcile().ok();
    env.fleet(&["get", "Task", "t1"]).code(3);

    let spans = collector.spans();
    for command in [
        "fleet apply",
        "fleet reconcile",
        "fleet send",
        "fleet amend",
        "fleet delete",
    ] {
        let roots = named(&spans, command);
        assert!(
            !roots.is_empty(),
            "no `{command}` span in {:#?}",
            names(&spans)
        );
        // The one refused amend (`--if-generation` past) exits 5.
        let (refused, roots): (Vec<_>, Vec<_>) = roots
            .into_iter()
            .partition(|r| command == "fleet amend" && r.error);
        assert_eq!(refused.len(), usize::from(command == "fleet amend"));
        for root in refused {
            assert_eq!(root.attrs["process.exit.code"], "5", "{root:#?}");
        }
        for root in roots {
            assert!(root.parent.is_empty(), "{command} is a root: {root:#?}");
            assert_eq!(root.attrs["process.exit.code"], "0", "{root:#?}");
            assert_eq!(root.attrs["fleet.command"], &command[6..], "{root:#?}");
            assert!(!root.error, "{root:#?}");
        }
    }
    let missing: Vec<_> = named(&spans, "fleet get")
        .into_iter()
        .filter(|s| s.error)
        .collect();
    assert_eq!(missing.len(), 1, "one failed get: {:#?}", names(&spans));
    assert_eq!(missing[0].attrs["process.exit.code"], "3");
    for span in &spans {
        assert_eq!(span.resource["service.name"], "fleet-e2e", "{span:#?}");
        assert_eq!(
            span.resource["deployment.environment.name"], "e2e",
            "{span:#?}"
        );
        assert!(span.resource.contains_key("service.version"), "{span:#?}");
    }

    // A reconcile pass is a child of its command, an object's reconcile a
    // child of its pass, and an effect a child of its object's reconcile.
    let reconciles = named(&spans, "reconcile Task");
    assert!(!reconciles.is_empty(), "{:#?}", names(&spans));
    let first = reconciles[0];
    assert_eq!(first.attrs["fleet.controller"], "TaskController");
    assert_eq!(first.attrs["fleet.kind"], "Task");
    assert_eq!(first.attrs["fleet.name"], "t1");
    assert!(first.attrs["fleet.uid"].starts_with('u'), "{first:#?}");
    assert!(first.attrs.contains_key("fleet.generation"), "{first:#?}");
    assert!(first.attrs.contains_key("fleet.outcome"), "{first:#?}");
    let pass = spans
        .iter()
        .find(|s| s.span_id == first.parent)
        .expect("the object's pass");
    assert_eq!(pass.name, "reconcile pass");
    let command = spans
        .iter()
        .find(|s| s.span_id == pass.parent)
        .expect("the pass's command");
    assert_eq!(command.name, "fleet reconcile");
    assert_eq!(command.trace_id, first.trace_id);

    for (kind, outcome) in [
        ("spawn", "spawned"),
        ("brief", "delivered"),
        ("deliver", "delivered"),
        ("release", "released"),
    ] {
        let span = effect(&spans, kind);
        assert_eq!(span.attrs["fleet.effect"], kind, "{span:#?}");
        assert_eq!(span.attrs["fleet.outcome"], outcome, "{span:#?}");
        assert_eq!(span.attrs["fleet.name"], "t1", "{span:#?}");
        let parent = spans
            .iter()
            .find(|s| s.span_id == span.parent)
            .expect("the effect's reconcile");
        assert_eq!(parent.name, "reconcile Task", "{span:#?}");
    }

    let all = collector.points();
    let durations = points(&all, "fleet.reconcile.duration");
    assert!(
        durations
            .iter()
            .any(|p| p.attrs["fleet.controller"] == "TaskController"),
        "{all:#?}"
    );
    assert_eq!(durations[0].unit, "s");
    let effects = points(&all, "fleet.effects");
    for kind in ["spawn", "brief", "deliver", "release"] {
        assert!(
            effects.iter().any(|p| p.attrs["fleet.effect"] == kind
                && p.attrs.contains_key("fleet.outcome")
                && p.value >= 1.0),
            "{kind} in {effects:#?}"
        );
    }
    let amendments = points(&all, "fleet.task.amendments");
    for outcome in ["appended", "precondition"] {
        assert!(
            amendments
                .iter()
                .any(|p| p.attrs["fleet.outcome"] == outcome && p.value >= 1.0),
            "{outcome} in {amendments:#?}"
        );
    }
    let depth = points(&all, "fleet.inbox.depth");
    assert!(depth.iter().any(|p| p.value >= 1.0), "{all:#?}");

    // Names and outcomes only: no brief, message or environment value.
    let raw = String::from_utf8_lossy(&collector.raw()).to_string();
    for secret in [BRIEF_SECRET, ENV_SECRET, MESSAGE_SECRET] {
        assert!(!raw.contains(secret), "{secret} was exported");
    }
}

fn open_change() -> ChangeSnapshot {
    ChangeSnapshot {
        state: ChangeState::Open,
        head: HeadSeen {
            id: "aaa111".into(),
            checks: Checks::Pending,
            failing: Vec::new(),
        },
        review_decision: ReviewDecision::None,
        mergeable: Mergeable {
            value: None,
            reason: None,
        },
        threads: Threads { unresolved: 0 },
        reviews: Vec::new(),
        notes: Vec::new(),
    }
}

/// Every forge poll is a span and a counted call, and a throttle is its
/// outcome.
#[test]
fn forge_polls_and_throttles_are_exported() {
    let collector = Collector::start();
    let mut env = Env::forge("otel-forge");
    exporting(&mut env, &collector.endpoint);
    let forge = env.fake_forge();
    forge
        .set_change("acme/widgets", "7", open_change())
        .unwrap();
    let cr = derived_name("fake", FORGE_HOST, "acme/widgets", "7");
    env.apply(change_request(
        "https://forge.test/acme/widgets/pull/7",
        json!({}),
    ))
    .ok();
    env.reconcile().ok();
    forge
        .add_fault(ForgeFault::new("change", "throttle").retry_after(120_000))
        .unwrap();
    env.expire_wait_of("ChangeRequest", &cr);
    env.reconcile().ok();

    let spans = collector.spans();
    let polls = named(&spans, "forge poll");
    let outcomes: BTreeSet<&str> = polls
        .iter()
        .map(|s| s.attrs["fleet.outcome"].as_str())
        .collect();
    assert_eq!(
        outcomes,
        BTreeSet::from(["known", "throttled"]),
        "{polls:#?}"
    );
    for poll in &polls {
        assert_eq!(poll.attrs["fleet.forge.host"], FORGE_HOST, "{poll:#?}");
        assert_eq!(poll.attrs["fleet.kind"], "ChangeRequest", "{poll:#?}");
        assert_eq!(poll.attrs["fleet.name"], cr, "{poll:#?}");
    }
    assert!(
        named(&spans, "reconcile ChangeRequest")
            .iter()
            .all(|s| s.attrs["fleet.controller"] == "ChangeRequestController"),
        "{spans:#?}"
    );
    let all = collector.points();
    let calls = points(&all, "fleet.forge.calls");
    for outcome in ["known", "throttled"] {
        assert!(
            calls
                .iter()
                .any(|p| p.attrs["fleet.outcome"] == outcome && p.value >= 1.0),
            "{outcome} in {calls:#?}"
        );
    }
}

/// Off unless asked: without an endpoint, or with the SDK disabled, a run
/// sends nothing, prints nothing about telemetry, and behaves as before.
#[test]
fn nothing_is_exported_unless_an_endpoint_is_set_and_the_sdk_enabled() {
    let collector = Collector::start();

    let mut disabled = Env::new("otel-disabled");
    exporting(&mut disabled, &collector.endpoint);
    disabled.set_var("OTEL_SDK_DISABLED", "true");
    disabled.apply(task("t1", true)).ok();
    let report = disabled.reconcile();
    report.ok();
    assert!(report.stderr.is_empty(), "{report:#?}");

    let mut unset = Env::new("otel-unset");
    unset.set_var("OTEL_SERVICE_NAME", "fleet-e2e");
    unset.set_var("OTEL_TRACES_EXPORTER", "otlp");
    unset.apply(task("t1", true)).ok();
    let report = unset.reconcile();
    report.ok();
    assert!(report.stderr.is_empty(), "{report:#?}");
    assert_eq!(report.json()["effects"], 2);

    // One signal's exporter cannot be built: the warning says export is
    // off, and it is off for both signals.
    let mut broken = Env::new("otel-broken");
    exporting(&mut broken, &collector.endpoint);
    broken.set_var("OTEL_EXPORTER_OTLP_METRICS_ENDPOINT", "not a url");
    // Batches leave every 10 ms, as a long `fleet up` would send them.
    broken.set_var("OTEL_BSP_SCHEDULE_DELAY", "10");
    broken.apply(task("t1", true)).ok();
    let report = broken.reconcile();
    report.ok().stderr_has("warning: telemetry is off");
    assert_eq!(report.json()["effects"], 2);

    assert!(collector.paths().is_empty(), "{:?}", collector.paths());
}

/// A collector that refuses connections, or accepts and never answers,
/// changes neither what a command does nor its exit, and delays it only
/// within the shutdown bound.
#[test]
fn an_unreachable_collector_never_changes_a_command() {
    let refused = {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        format!("http://{}", l.local_addr().unwrap())
    };
    let silent = TcpListener::bind("127.0.0.1:0").unwrap();
    let silent_endpoint = format!("http://{}", silent.local_addr().unwrap());

    for (name, endpoint) in [
        ("otel-refused", &refused),
        ("otel-silent", &silent_endpoint),
    ] {
        let mut env = Env::new(name);
        exporting(&mut env, endpoint);
        env.apply(task("t1", true)).ok();
        let started = Instant::now();
        let report = env.reconcile();
        report.ok();
        let took = started.elapsed();
        assert!(report.stderr.is_empty(), "{name}: {report:#?}");
        let json = report.json();
        assert_eq!(
            (json["idle"].clone(), json["effects"].clone()),
            (json!(true), json!(2))
        );
        assert_eq!(env.status("t1")["phase"], "Briefed", "{name}");
        // Inside the shutdown bound, well short of the exporter's own
        // 10-second timeout.
        assert!(
            took < Duration::from_secs(7),
            "{name}: the command took {took:?}"
        );
    }
    drop(silent);
}

/// The manager's telemetry identity is not a session's: no OTEL_* variable
/// of the manager reaches the environment Fleet starts a session with. The
/// OTEL_* variables the Task declares itself, from its spec, a TaskConfig or
/// an EnvProvider, do.
#[test]
fn a_spawned_session_does_not_inherit_the_managers_otel_identity() {
    let mut e = Env::new("otel-session-env");
    e.set_var("OTEL_EXPORTER_OTLP_ENDPOINT", "http://127.0.0.1:9");
    e.set_var("OTEL_SERVICE_NAME", "fleet");
    e.set_var(
        "OTEL_RESOURCE_ATTRIBUTES",
        "service.namespace=fleet-v2,fleet.manager=manager",
    );
    e.set_var("OTEL_SDK_DISABLED", "true");
    e.apply(object(
        "EnvProvider",
        "otel",
        json!({"data":{"OTEL_EXPORTER_OTLP_ENDPOINT":"http://collector.test:4318"}}),
    ))
    .ok();
    e.apply(object(
        "TaskConfig",
        "otel",
        json!({"envFrom":[{"name":"otel"}],"env":{"OTEL_RESOURCE_ATTRIBUTES":"fleet.task=t1"}}),
    ))
    .ok();
    let mut t = task("t1", true);
    t["spec"]["configRef"] = json!({"name":"otel"});
    t["spec"]["env"] = json!({"OTEL_SERVICE_NAME":"agent"});
    e.apply(t).ok();
    e.reconcile().ok();

    let session = e
        .host()
        .session(&e.session_key("t1"))
        .unwrap()
        .expect("a session");
    let otel: Vec<(&str, &str)> = session
        .env
        .iter()
        .filter(|(k, _)| k.starts_with("OTEL_"))
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();
    assert_eq!(
        otel,
        [
            ("OTEL_EXPORTER_OTLP_ENDPOINT", "http://collector.test:4318"),
            ("OTEL_RESOURCE_ATTRIBUTES", "fleet.task=t1"),
            ("OTEL_SERVICE_NAME", "agent"),
        ],
        "the Task's own OTEL_*, and none of the manager's"
    );
}
