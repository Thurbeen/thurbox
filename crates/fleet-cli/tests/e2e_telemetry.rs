//! Acceptance: OpenTelemetry export through the real `fleet` binary. The
//! standard OTEL_* variables point it at an in-process OTLP/HTTP collector;
//! the commands' and the manager's spans and metrics must arrive there with
//! the resource's identity and outcome, and never a brief, a message or an
//! environment value. Unset or disabled, nothing is sent; an unreachable
//! collector changes nothing a command does.

mod support;

use std::collections::BTreeSet;
use std::net::TcpListener;
use std::process::Child;
use std::time::{Duration, Instant};

use fleet_api::forge::{
    ChangeSnapshot, ChangeState, Checks, HeadSeen, Mergeable, ReviewDecision, Threads,
};
use fleet_forge::ForgeFault;
use fleet_runtime::Fault;
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

fn inbox_depth(all: &[Point], name: &str, state: &str) -> Vec<f64> {
    points(all, "fleet.inbox.depth")
        .into_iter()
        .filter(|p| {
            p.attrs.get("fleet.name").map(String::as_str) == Some(name)
                && p.attrs.get("fleet.inbox.state").map(String::as_str) == Some(state)
        })
        .map(|p| p.value)
        .collect()
}

fn manager_info(collector: &Collector, pid: u32, lease: bool) -> Option<f64> {
    points(&collector.points(), "fleet.manager.info")
        .into_iter()
        .filter(|p| {
            p.attrs.get("process.pid") == Some(&pid.to_string())
                && p.attrs.get("fleet.manager.lease") == Some(&lease.to_string())
        })
        .map(|p| p.value)
        .next_back()
}

fn wait_for(what: &str, mut ready: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(40);
    while !ready() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

struct Running(Child);

impl Drop for Running {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
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

    // A reconcile pass is a child of its command. An object's reconcile is
    // in the object's own trace and links to its pass, and an effect is a
    // child of its object's reconcile.
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
        .find(|s| first.links.contains(&s.span_id))
        .expect("the object's pass");
    assert_eq!(pass.name, "reconcile pass");
    let command = spans
        .iter()
        .find(|s| s.span_id == pass.parent)
        .expect("the pass's command");
    assert_eq!(command.name, "fleet reconcile");
    assert_eq!(command.trace_id, pass.trace_id);
    // Every reconcile of the Task, over every command, is one trace.
    assert!(
        reconciles.iter().all(|r| r.trace_id == first.trace_id),
        "{reconciles:#?}"
    );
    assert_ne!(first.trace_id, command.trace_id);

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
    let depth = inbox_depth(&all, "t1", "pending");
    assert!(depth.iter().any(|v| *v >= 1.0), "{all:#?}");
    assert_eq!(
        depth.last(),
        Some(&0.0),
        "the released Task's backlog drops to 0: {all:#?}"
    );

    // Names and outcomes only: no brief, message or environment value.
    let raw = String::from_utf8_lossy(&collector.raw()).to_string();
    for secret in [BRIEF_SECRET, ENV_SECRET, MESSAGE_SECRET] {
        assert!(!raw.contains(secret), "{secret} was exported");
    }
}

#[test]
fn inbox_depth_splits_pending_from_given_up_entries_per_task() {
    let collector = Collector::start();
    let mut env = Env::new("otel-inbox");
    exporting(&mut env, &collector.endpoint);
    env.apply(base_config()).ok();
    env.apply(task("t1", true)).ok();
    env.reconcile().ok();
    env.fault(Fault::new("brief", "fail").times(3));
    env.fleet(&["send", "t1", "first"]).ok();
    env.reconcile().ok();
    env.expire_wait("t1");
    env.reconcile().ok();
    env.expire_wait("t1");
    env.reconcile().ok();
    let attempts = env.get("t1")["status"]["deliveryAttempts"].clone();
    assert!(
        attempts
            .as_object()
            .is_some_and(|a| a.values().any(|a| a["failed"] == true)),
        "the first entry is given up on: {attempts:#}"
    );
    env.fleet(&["send", "t1", "second"]).ok();
    let before = collector.points().len();
    env.fleet(&["reconcile", "--max-passes", "1"]).ok();
    let after = collector.points()[before..].to_vec();
    assert_eq!(inbox_depth(&after, "t1", "pending"), [1.0], "{after:#?}");
    assert_eq!(inbox_depth(&after, "t1", "failed"), [1.0], "{after:#?}");

    env.host()
        .set_at_rest(&env.session_key("t1"), true)
        .unwrap();
    env.fleet(&["delete", "Task", "t1"]).ok();
    env.reconcile().ok();
    env.fleet(&["get", "Task", "t1"]).code(3);
    let all = collector.points();
    for state in ["pending", "failed"] {
        assert_eq!(
            inbox_depth(&all, "t1", state).last(),
            Some(&0.0),
            "{state}: {all:#?}"
        );
    }
}

fn inbox_points_after_last_of(collector: &Collector, gone: &str, kept: &str) -> usize {
    let all = collector.points();
    let depth: Vec<&Point> = points(&all, "fleet.inbox.depth");
    let last_gone = depth
        .iter()
        .rposition(|p| p.attrs.get("fleet.name").map(String::as_str) == Some(gone))
        .map_or(0, |i| i + 1);
    depth[last_gone..]
        .iter()
        .filter(|p| p.attrs.get("fleet.name").map(String::as_str) == Some(kept))
        .count()
}

#[test]
fn a_serving_manager_stops_reporting_a_released_task_after_its_zero() {
    let collector = Collector::start();
    let mut env = Env::new("otel-inbox-gone");
    exporting(&mut env, &collector.endpoint);
    env.set_var("OTEL_METRIC_EXPORT_INTERVAL", "100");
    env.apply(base_config()).ok();
    env.apply(task("t1", true)).ok();
    env.apply(task("t2", true)).ok();
    let _up = Running(env.spawn(&["up", "--poll-ms", "100"]));
    wait_for("both Tasks are reported", || {
        let all = collector.points();
        !inbox_depth(&all, "t1", "pending").is_empty()
            && !inbox_depth(&all, "t2", "pending").is_empty()
    });
    wait_for("t1 is briefed", || env.status("t1")["phase"] == "Briefed");
    env.host()
        .set_at_rest(&env.session_key("t1"), true)
        .unwrap();
    env.fleet(&["delete", "Task", "t1"]).ok();
    wait_for("t1 is released", || {
        env.fleet(&["get", "Task", "t1"]).code == Some(3)
    });
    wait_for("t1 is no longer exported while t2 is", || {
        inbox_points_after_last_of(&collector, "t1", "t2") >= 6
    });
    assert_eq!(
        inbox_depth(&collector.points(), "t1", "pending").last(),
        Some(&0.0)
    );
}

fn two_managers(env: &mut Env, collector: &Collector) -> (Running, Running) {
    exporting(env, &collector.endpoint);
    env.set_var("OTEL_METRIC_EXPORT_INTERVAL", "100");
    let first = Running(env.spawn(&["up", "--poll-ms", "100", "--lease-ttl-ms", "3000"]));
    let first_pid = first.0.id();
    wait_for("the first manager holds the lease", || {
        manager_info(collector, first_pid, true) == Some(1.0)
    });
    let second = Running(env.spawn(&["up", "--poll-ms", "100"]));
    let second_pid = second.0.id();
    wait_for("the second manager waits without the lease", || {
        manager_info(collector, second_pid, false) == Some(1.0)
    });
    (first, second)
}

#[test]
fn manager_info_names_each_manager_and_whether_it_holds_the_lease() {
    let collector = Collector::start();
    let mut env = Env::new("otel-manager-info");
    let (first, second) = two_managers(&mut env, &collector);
    assert_eq!(manager_info(&collector, first.0.id(), false), Some(0.0));
    assert_eq!(manager_info(&collector, second.0.id(), true), Some(0.0));
    let all = collector.points();
    let held = points(&all, "fleet.manager.info")
        .into_iter()
        .rfind(|p| p.attrs.get("process.pid") == Some(&first.0.id().to_string()))
        .cloned()
        .expect("a point");
    assert_eq!(held.unit, "{manager}", "{held:#?}");
    assert_eq!(
        held.attrs["fleet.manager.version"],
        env!("CARGO_PKG_VERSION"),
        "{held:#?}"
    );
    assert!(!held.attrs["host.name"].is_empty(), "{held:#?}");
}

#[cfg(target_os = "linux")]
#[test]
fn manager_info_moves_to_the_lease_when_a_waiting_manager_takes_over() {
    let collector = Collector::start();
    let mut env = Env::new("otel-manager-takeover");
    let (mut first, second) = two_managers(&mut env, &collector);
    let _ = first.0.kill();
    let _ = first.0.wait();
    let second_pid = second.0.id();
    wait_for("the second manager takes the lease over", || {
        manager_info(&collector, second_pid, true) == Some(1.0)
            && manager_info(&collector, second_pid, false) == Some(0.0)
    });
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

/// A caller's TRACEPARENT, as thurbox passes it, makes the command's root
/// span its child; a malformed one is ignored and the command is a root.
#[test]
fn a_command_joins_the_trace_its_caller_names_in_traceparent() {
    let collector = Collector::start();
    let mut env = Env::new("otel-traceparent");
    exporting(&mut env, &collector.endpoint);
    env.set_var(
        "TRACEPARENT",
        "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01",
    );
    env.apply(task("t1", true)).ok();
    env.set_var("TRACEPARENT", "not a traceparent");
    env.fleet(&["get", "Task", "t1"]).ok();

    let spans = collector.spans();
    let apply = named(&spans, "fleet apply");
    assert_eq!(apply.len(), 1, "{:#?}", names(&spans));
    let hex = |b: &[u8]| b.iter().map(|x| format!("{x:02x}")).collect::<String>();
    assert_eq!(hex(&apply[0].trace_id), "4bf92f3577b34da6a3ce929d0e0e4736");
    assert_eq!(hex(&apply[0].parent), "00f067aa0ba902b7");
    let get = named(&spans, "fleet get");
    assert_eq!(get.len(), 1, "{:#?}", names(&spans));
    assert!(get[0].parent.is_empty(), "{:#?}", get[0]);
}

/// A one-shot command answers a caller that bounds it (thurbox probes
/// `fleet version --json` within seconds, `doctor` within 300 ms), so a
/// collector that accepts and never answers adds at most the call bound to
/// its exit, never the 2-second bound of `fleet up`.
#[test]
fn a_stalled_collector_adds_at_most_the_call_bound_to_a_command() {
    let silent = TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = format!("http://{}", silent.local_addr().unwrap());
    let mut env = Env::new("otel-call-bound");
    exporting(&mut env, &endpoint);
    // Warm the binary's page cache so the measure is the flush alone.
    env.fleet(&["version", "--json"]).ok();
    let started = Instant::now();
    env.fleet(&["version", "--json"]).ok();
    let took = started.elapsed();
    // The 2-second bound took 2.0 s here; the call bound takes about 0.2 s.
    // 1.5 s tells them apart with room for a loaded runner.
    assert!(
        took < Duration::from_millis(1500),
        "the command took {took:?} with a stalled collector"
    );
    drop(silent);
}

/// A command's root span names the object it acts on, so a trace view finds
/// a Task's commands by `fleet.name` (docs/observability.md, *Turn it on*),
/// with a built-in `fleet.kind` as the store serves it, whatever spelling the
/// command took. A command that names no one object carries neither. A
/// command whose export the call bound dropped is run again.
#[test]
fn a_command_span_names_the_kind_and_object_it_acts_on() {
    let collector = Collector::start();
    let mut env = Env::new("otel-command-names");
    exporting(&mut env, &collector.endpoint);
    env.apply(task("t1", true)).ok();
    let runs: [(&[&str], &str, Option<&str>); 17] = [
        (&["get", "task", "t1"], "Task", Some("t1")),
        (&["status", "tasks", "t1"], "Task", Some("t1")),
        (&["list", "task"], "Task", None),
        (&["send", "t1", "hello"], "Task", Some("t1")),
        (&["amend", "t1", "--text", "more"], "Task", Some("t1")),
        (&["label", "task", "t1", "team=e2e"], "Task", Some("t1")),
        (
            &["declare", "t1", "--json", r#"{"kind":"note"}"#],
            "Task",
            Some("t1"),
        ),
        (
            &["result", "t1", "--outcome", "shipped"],
            "Task",
            Some("t1"),
        ),
        (&["attach", "t1", "--json"], "Task", Some("t1")),
        (&["shell", "t1", "--json"], "Task", Some("t1")),
        (
            &["event", "subscribe", "t1", "--board", "shared"],
            "Task",
            Some("t1"),
        ),
        (
            &["event", "list", "--board", "shared"],
            "Board",
            Some("shared"),
        ),
        (
            &["event", "unsubscribe", "t1", "--board", "shared"],
            "Task",
            Some("t1"),
        ),
        (
            &[
                "event", "publish", "--board", "shared", "--type", "e2e.note", "--data", "{}",
            ],
            "Board",
            Some("shared"),
        ),
        (
            &[
                "event",
                "wait",
                "--board",
                "shared",
                "--after",
                "0",
                "--timeout-ms",
                "1",
            ],
            "Board",
            Some("shared"),
        ),
        (&["get", "widgets", "w1"], "widgets", Some("w1")),
        (
            &["delete", "taskconfig", "default"],
            "TaskConfig",
            Some("default"),
        ),
    ];
    let span_of = |command: &str, kind: &str, seen: &BTreeSet<Vec<u8>>| {
        named(&collector.spans(), command)
            .into_iter()
            .find(|s| {
                !seen.contains(&s.span_id) && s.attrs.get("fleet.kind").is_some_and(|k| k == kind)
            })
            .cloned()
    };
    for (args, kind, name) in runs {
        let command = format!("fleet {}", args[0]);
        let seen: BTreeSet<Vec<u8>> = collector.spans().into_iter().map(|s| s.span_id).collect();
        let mut root = None;
        for _ in 0..3 {
            env.fleet(args);
            root = span_of(&command, kind, &seen);
            if root.is_some() {
                break;
            }
        }
        let root = root.unwrap_or_else(|| {
            panic!(
                "no `{command}` span with fleet.kind={kind} in {:#?}",
                named(&collector.spans(), &command)
            )
        });
        assert_eq!(
            root.attrs.get("fleet.name").map(String::as_str),
            name,
            "{args:?}: {root:#?}"
        );
    }
    let spans = collector.spans();
    let apply = named(&spans, "fleet apply");
    assert!(!apply.is_empty(), "no `fleet apply` span");
    assert!(
        apply
            .iter()
            .all(|s| !s.attrs.contains_key("fleet.name") && !s.attrs.contains_key("fleet.kind")),
        "apply names no one object: {apply:#?}"
    );
}
