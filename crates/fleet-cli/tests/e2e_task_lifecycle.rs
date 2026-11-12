//! Acceptance: the Task lifecycle through the real `fleet` binary, the file
//! store and the fake host, across process restarts and injected faults.
//! Every scenario goes CLI -> manager -> TaskController plan -> runtime
//! effect -> receipt, with no step mocked out.

mod support;

use std::fs;
use std::thread;
use std::time::Duration;

use fleet_runtime::Fault;
use serde_json::json;
use support::{Env, assert_condition, base_config, object, task, wait};

fn setup(name: &str, dispatch: bool) -> Env {
    let env = Env::new(name);
    env.apply(base_config()).ok();
    env.apply(task("t1", dispatch))
        .ok()
        .stdout_has("Task/t1 created");
    env
}

/// A Task whose harness has no prompt hook, so the runtime's journal, not
/// the agent, settles a delivery whose answer was lost.
fn setup_unhooked(name: &str) -> Env {
    let env = Env::new(name);
    let mut t = task("t1", true);
    t["spec"]["harness"] = serde_json::json!("aider");
    env.apply(t).ok().stdout_has("Task/t1 created");
    env
}

fn result_args<'a>(generation: &'a str, extra: &[&'a str]) -> Vec<&'a str> {
    let mut args = vec![
        "result",
        "t1",
        "--generation",
        generation,
        "--outcome",
        "shipped",
        "--summary",
        "done",
    ];
    args.extend_from_slice(extra);
    args
}

#[test]
fn a_task_runs_from_apply_to_release_with_one_spawn_and_one_brief() {
    let env = setup("lifecycle", true);
    let report = env.reconcile().ok().json();
    assert_eq!(report["idle"], true, "{report:#}");

    let st = env.status("t1");
    assert_eq!(st["phase"], "Briefed", "{st:#}");
    assert_condition(&st, "Spawned", "True", "Spawned");
    assert_condition(&st, "Briefed", "True", "ReceiptWritten");
    let key = env.session_key("t1");
    let session = env
        .host()
        .session(&key)
        .unwrap()
        .expect("a session for the Task's key");
    let uid = env.get("t1")["metadata"]["uid"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(
        session.briefs,
        vec![format!("task/t1/{uid}/brief")],
        "briefed exactly once, under the Task's brief key"
    );
    assert_eq!((env.calls("spawn"), env.calls("brief")), (1, 1));

    // A duplicate reconcile, from a new process, changes nothing.
    let again = env.reconcile().ok().json();
    assert_eq!(
        (again["effects"].as_u64(), again["statusWrites"].as_u64()),
        (Some(0), Some(0)),
        "{again:#}"
    );
    assert_eq!((env.calls("spawn"), env.calls("brief")), (1, 1));

    // The worker answers: validated against spec.outputs and fenced by generation.
    env.fleet(&result_args(
        "2",
        &["--output", "prUrl=\"https://example.test/pr/1\""],
    ))
    .code(1)
    .stderr_has("stale generation 2");
    env.fleet(&result_args("1", &[]))
        .code(1)
        .stderr_has("output \"prUrl\" is required");
    env.fleet(&result_args("1", &["--output", "prUrl=7"]))
        .code(1)
        .stderr_has("must be String");
    env.fleet(&result_args(
        "1",
        &["--output", "prUrl=\"u\"", "--output", "extra=1"],
    ))
    .code(1)
    .stderr_has("not declared");
    let good = result_args(
        "1",
        &[
            "--output",
            "prUrl=\"https://example.test/pr/1\"",
            "--output",
            "tests=12",
        ],
    );
    env.fleet(&good).ok().stdout_has("result recorded");
    env.fleet(&good).ok().stdout_has("result unchanged");
    env.fleet(&result_args(
        "1",
        &["--output", "prUrl=\"https://example.test/pr/2\""],
    ))
    .code(1)
    .stderr_has("already recorded");

    env.reconcile().ok();
    let st = env.status("t1");
    assert_eq!(st["phase"], "Concluded", "{st:#}");
    assert_condition(&st, "Concluded", "True", "ResultRecorded");
    assert_eq!(st["result"]["outputs"]["tests"], 12);

    // Deleting waits for a session proven at rest (P7), then releases once.
    env.fleet(&["delete", "task", "t1"])
        .ok()
        .stdout_has("marked for deletion");
    env.reconcile().ok();
    let st = env.status("t1");
    assert_eq!(st["phase"], "Releasing", "{st:#}");
    assert_condition(&st, "Released", "False", "SessionBusy");
    assert_eq!(env.calls("release"), 0, "a busy session is never released");

    env.host().set_at_rest(&key, true).unwrap();
    env.reconcile().ok();
    assert_eq!(env.calls("release"), 1);
    let session = env.host().session(&key).unwrap().unwrap();
    assert!(session.released && session.gone, "released means deleted");
    env.fleet(&["get", "task", "t1"])
        .code(3)
        .stderr_has("not found");
    env.reconcile().ok();
    assert_eq!(env.calls("release"), 1, "released exactly once");
    env.assert_no_temp();
}

#[test]
fn a_task_waits_for_dispatch_and_its_spec_is_fixed_except_dispatch() {
    let env = setup("dispatch", false);
    env.reconcile().ok();
    let st = env.status("t1");
    assert_eq!(st["phase"], "Pending");
    assert_condition(&st, "Spawned", "False", "AwaitingDispatch");
    assert_eq!(env.calls("spawn"), 0);

    let mut changed = task("t1", false);
    changed["spec"]["brief"] = json!("A different brief.");
    env.apply(changed)
        .code(1)
        .stderr_has("immutable except spec.dispatch");

    env.apply(task("t1", true))
        .ok()
        .stdout_has("Task/t1 configured");
    env.apply(task("t1", true))
        .ok()
        .stdout_has("Task/t1 unchanged");
    env.reconcile().ok();
    let st = env.status("t1");
    assert_eq!(
        (
            st["phase"].as_str(),
            st["generation"].as_u64(),
            st["observedGeneration"].as_u64()
        ),
        (Some("Briefed"), Some(2), Some(2))
    );
    assert_eq!(st["current"], true);
    // The brief was delivered for generation 2, so that is what a result names.
    env.fleet(&result_args("1", &["--output", "prUrl=\"u\""]))
        .code(1)
        .stderr_has("stale generation 1");
    env.fleet(&result_args("2", &["--output", "prUrl=\"u\""]))
        .ok();
}

#[test]
fn a_crash_after_spawn_before_its_receipt_adopts_the_session() {
    let env = setup("crash-spawn", true);
    env.fault(Fault::new("spawn", "abort"));
    env.fleet(&["reconcile", "--lease-ttl-ms", "3000"])
        .crashed();
    let raw = env.get("t1");
    assert_eq!(
        raw["status"]["phase"], "Spawning",
        "the intent was written before the effect: {raw:#}"
    );
    assert!(
        raw["status"]["receipts"]
            .as_object()
            .is_none_or(|r| r.is_empty()),
        "no receipt: {raw:#}"
    );
    assert_eq!(
        env.host().state().unwrap().sessions.len(),
        1,
        "the effect happened"
    );

    // The dead holder's lease must expire before the next manager can take it.
    thread::sleep(Duration::from_millis(3_100));
    env.reconcile().ok();
    let st = env.status("t1");
    assert_eq!(st["phase"], "Briefed", "{st:#}");
    assert_condition(&st, "Spawned", "True", "Adopted");
    assert_eq!(
        env.host().state().unwrap().sessions.len(),
        1,
        "adopted, never twinned"
    );
    assert_eq!(env.calls("spawn"), 1, "no respawn: the key decided");
    env.assert_no_temp();
}

#[test]
fn a_crash_after_the_brief_before_its_receipt_is_recovered_without_a_resend() {
    let env = setup("crash-brief", true);
    env.fault(Fault::new("brief", "abort"));
    env.fleet(&["reconcile", "--lease-ttl-ms", "3000"])
        .crashed();
    // The dead holder's lease must expire before the next manager can take it.
    thread::sleep(Duration::from_millis(3_100));
    env.reconcile().ok();
    let st = env.status("t1");
    assert_eq!(st["phase"], "Briefed", "{st:#}");
    assert_condition(&st, "Briefed", "True", "ReceiptRecovered");
    assert_eq!(st["brief"]["recovered"], true);
    let session = env.host().session(&env.session_key("t1")).unwrap().unwrap();
    assert_eq!(session.briefs.len(), 1, "delivered once");
    assert_eq!(
        env.calls("brief"),
        1,
        "the agent's prompt hook settled it, never a resend"
    );
}

#[test]
fn a_brief_whose_delivery_cannot_be_told_asks_for_attention_and_is_never_resent() {
    let env = setup_unhooked("brief-unknown");
    env.fault(Fault::new("brief", "unknown"));
    env.fault(Fault::new("briefDelivered", "unknown").times(100));
    env.reconcile().ok();
    for _ in 0..3 {
        env.expire_wait("t1");
        env.reconcile().ok();
    }
    let st = env.status("t1");
    assert_eq!(st["phase"], "Spawned", "{st:#}");
    assert_condition(&st, "NeedsAttention", "True", "BriefUnconfirmed");
    assert_eq!(env.calls("brief"), 1, "never a blind resend");
}

#[test]
fn refused_brief_sends_wait_across_manager_runs_and_stop_after_three_attempts() {
    let env = setup("brief-refused-wait", true);
    env.fault(Fault::new("brief", "fail").times(3));
    env.reconcile().ok();
    assert_eq!(
        env.calls("brief"),
        1,
        "one attempt before the persisted wait"
    );
    env.reconcile().ok();
    assert_eq!(env.calls("brief"), 1, "a new manager must honour the wait");
    for attempt in 2..=3 {
        env.expire_wait("t1");
        env.reconcile().ok();
        assert_eq!(env.calls("brief"), attempt);
    }
    assert_eq!(env.status("t1")["phase"], "Stalled");
    env.expire_wait("t1");
    env.reconcile().ok();
    assert_eq!(env.calls("brief"), 3);
}

#[test]
fn a_brief_retry_waits_until_the_session_is_ready_again() {
    for mode in ["fail", "unknown"] {
        let env = setup_unhooked(&format!("brief-retry-ready-{mode}"));
        env.fault(Fault::new("brief", mode));
        if mode == "unknown" {
            env.fault(Fault::new("briefDelivered", "unknown"));
        }
        env.reconcile().ok();
        assert_eq!(env.calls("brief"), 1);
        let key = env.session_key("t1");
        env.host().set_ready(&key, false).unwrap();
        env.expire_wait("t1");
        env.reconcile().ok();
        assert_eq!(
            env.calls("brief"),
            1,
            "{mode}: a retry still needs readiness"
        );
        env.host().set_ready(&key, true).unwrap();
        env.reconcile().ok();
        assert_eq!(env.status("t1")["phase"], "Briefed");
        assert_eq!(env.calls("brief"), 2);
    }
}

#[test]
fn a_brief_known_not_to_have_arrived_is_sent_again_once() {
    let env = setup_unhooked("brief-lost");
    env.fault(Fault::new("brief", "unknown"));
    env.reconcile().ok();
    let st = env.status("t1");
    assert_eq!(st["phase"], "Briefed", "{st:#}");
    assert_eq!(env.calls("brief"), 2);
    let session = env.host().session(&env.session_key("t1")).unwrap().unwrap();
    assert_eq!(session.briefs.len(), 1);
}

#[test]
fn a_transient_unknown_delivery_probe_recovers_without_resending() {
    let env = setup("brief-probe-recovers", true);
    env.fault(Fault::new("brief", "unknown").performed());
    env.fault(Fault::new("briefDelivered", "unknown"));
    env.reconcile().ok();
    env.expire_wait("t1");
    env.reconcile().ok();
    let status = env.status("t1");
    assert_eq!(
        status["phase"], "Briefed",
        "a transient read failure must not strand a delivered brief"
    );
    assert_condition(&status, "Briefed", "True", "ReceiptRecovered");
    assert_eq!(env.calls("brief"), 1, "only re-read the receipt");
    assert_eq!(
        env.host()
            .session(&env.session_key("t1"))
            .unwrap()
            .unwrap()
            .briefs
            .len(),
        1
    );
}

#[test]
fn unknown_delivery_probes_wait_between_tries_and_never_resend() {
    let env = setup_unhooked("brief-probe-bounded");
    env.fault(Fault::new("brief", "unknown").performed());
    env.fault(Fault::new("briefDelivered", "unknown").times(3));
    env.reconcile().ok();
    assert_eq!(
        env.calls("briefDelivered"),
        1,
        "one try per wait, not three in one run"
    );
    for _ in 0..2 {
        env.expire_wait("t1");
        env.reconcile().ok();
    }
    let st = env.status("t1");
    assert_eq!(env.calls("briefDelivered"), 3);
    assert_eq!(
        st["phase"], "Spawned",
        "a read never stalls the Task: {st:#}"
    );
    assert_condition(&st, "NeedsAttention", "True", "BriefUnconfirmed");
    // The journal answers again: the delivered brief is recovered.
    env.expire_wait("t1");
    env.reconcile().ok();
    let st = env.status("t1");
    assert_eq!(st["phase"], "Briefed", "{st:#}");
    assert_eq!(st["brief"]["recovered"], true);
    assert_eq!(env.calls("brief"), 1, "never resent on an unknown receipt");
}

#[test]
fn a_lost_brief_answer_after_delivery_is_recovered_by_asking() {
    let env = setup("brief-answer-lost", true);
    env.fault(Fault::new("brief", "unknown").performed());
    env.reconcile().ok();
    let st = env.status("t1");
    assert_eq!(st["phase"], "Briefed", "{st:#}");
    assert_eq!(st["brief"]["recovered"], true);
    assert_eq!(env.calls("brief"), 1);
}

#[test]
fn spawn_failures_wait_between_tries_and_are_bounded() {
    let env = setup("spawn-bounded", true);
    env.fault(Fault::new("spawn", "fail").times(10));
    env.reconcile().ok();
    assert_eq!(
        env.calls("spawn"),
        1,
        "one try per wait, not three in one run"
    );
    for _ in 0..2 {
        env.expire_wait("t1");
        env.reconcile().ok();
    }
    let st = env.status("t1");
    assert_eq!(st["phase"], "Stalled", "{st:#}");
    assert_condition(&st, "Stalled", "True", "SpawnAttemptsExhausted");
    assert_condition(&st, "NeedsAttention", "True", "Stalled");
    assert_eq!(env.calls("spawn"), 3);
    env.expire_wait("t1");
    env.reconcile().ok();
    assert_eq!(env.calls("spawn"), 3, "a stalled Task waits for a decision");
}

#[test]
fn an_unknown_or_down_host_never_spawns() {
    let env = setup("host", true);
    env.host().set_host("local", "unknown").unwrap();
    env.reconcile().ok();
    let st = env.status("t1");
    assert_eq!(st["phase"], "Pending");
    assert_condition(&st, "HostReachable", "Unknown", "ProbeFailed");
    env.host().set_host("local", "down").unwrap();
    env.reconcile().ok();
    assert_condition(&env.status("t1"), "HostReachable", "False", "Unreachable");
    assert_eq!(env.calls("spawn"), 0);
    env.host().set_host("local", "up").unwrap();
    env.reconcile().ok();
    assert_eq!(env.status("t1")["phase"], "Briefed");
}

#[test]
fn an_unreadable_session_list_is_not_absence() {
    let env = setup("find-unknown", true);
    env.fault(Fault::new("spawn", "unknown"));
    env.fault(Fault::new("find", "unknown").times(1000));
    for _ in 0..3 {
        env.reconcile().ok();
        let st = env.status("t1");
        assert_eq!(st["phase"], "Spawning", "{st:#}");
        assert_condition(&st, "Spawned", "Unknown", "SessionListUnreadable");
        assert_eq!(
            env.calls("spawn"),
            1,
            "Unknown is never read as 'no session': no respawn"
        );
    }
    env.host().update(|s| s.faults.clear()).unwrap();
    env.reconcile().ok();
    env.expire_wait("t1");
    env.reconcile().ok();
    assert_eq!(env.status("t1")["phase"], "Briefed");
    assert_eq!(
        env.calls("spawn"),
        2,
        "a readable empty list allows the next attempt"
    );
}

#[test]
fn a_vanished_worker_requires_attention_without_abandonment() {
    let env = setup("worker-gone", true);
    env.reconcile().ok();
    env.host().vanish(&env.session_key("t1")).unwrap();
    env.reconcile().ok();
    let st = env.status("t1");
    assert_eq!(st["phase"], "Briefed", "Q13: an origin decides: {st:#}");
    assert_condition(&st, "WorkerGone", "True", "SessionGone");
    assert_condition(&st, "NeedsAttention", "True", "WorkerGone");
    assert_eq!(env.calls("spawn"), 1);
    // Re-deriving the same fact is not news: the next run writes nothing.
    let again = env.reconcile().ok().json();
    assert_eq!(
        (again["statusWrites"].as_u64(), again["idle"].as_bool()),
        (Some(0), Some(true)),
        "{again:#}"
    );
}

#[test]
fn a_concluded_task_whose_session_vanishes_requires_attention() {
    let env = setup("concluded-gone", true);
    env.reconcile().ok();
    env.fleet(&result_args(
        "1",
        &["--output", "prUrl=\"https://example.test/pr/1\""],
    ))
    .ok();
    env.reconcile().ok();
    assert_eq!(env.status("t1")["phase"], "Concluded");
    env.host().vanish(&env.session_key("t1")).unwrap();

    // At rest, the session is looked up once per recheck interval, not on
    // every pass.
    let finds = env.calls("find");
    let again = env.reconcile().ok().json();
    assert_eq!(env.calls("find"), finds, "{again:#}");
    assert_eq!(again["statusWrites"], 0, "{again:#}");

    env.expire_session_check("t1");
    env.reconcile().ok();
    let st = env.status("t1");
    assert_eq!(st["phase"], "Concluded", "{st:#}");
    assert_condition(&st, "WorkerGone", "True", "SessionGone");
    assert_condition(&st, "NeedsAttention", "True", "WorkerGone");
    assert_eq!(env.calls("spawn"), 1, "never respawned");
}

#[test]
fn one_manager_at_a_time_and_a_lost_lease_writes_nothing() {
    let env = setup("lease", true);
    // A holds the lease while its spawn is slow; B is refused.
    env.fault(Fault::new("spawn", "normal").delay(1500));
    let a = env.spawn(&["reconcile", "--lease-ttl-ms", "30000"]);
    thread::sleep(Duration::from_millis(400));
    env.fleet(&["reconcile"])
        .code(6)
        .stderr_has("lease held by");
    wait(a).ok();
    assert_eq!(env.status("t1")["phase"], "Briefed");

    // C's lease expires during its slow spawn; D takes over and finishes;
    // C's receipt write is refused and it exits with the lease error.
    env.apply(task("t2", true)).ok();
    env.fault(Fault::new("spawn", "normal").delay(1500));
    let c = env.spawn(&["reconcile", "--lease-ttl-ms", "300"]);
    thread::sleep(Duration::from_millis(700));
    // D takes the lease as soon as C's has expired: C renews it before every
    // probe and effect, so on a loaded machine that can be a little later.
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        let d = env.reconcile();
        if d.code == Some(6) && std::time::Instant::now() < deadline {
            thread::sleep(Duration::from_millis(50));
            continue;
        }
        d.ok();
        break;
    }
    let c = wait(c);
    c.code(6).stderr_has("lease lost");
    // Whatever D saw of C's spawn, a waiting retry adopts it by its key.
    env.expire_wait("t2");
    env.reconcile().ok();
    let st = env.status("t2");
    assert_eq!(st["phase"], "Briefed", "{st:#}");
    let key = env.session_key("t2");
    let sessions: Vec<_> = env
        .host()
        .state()
        .unwrap()
        .sessions
        .into_values()
        .filter(|s| s.key == key)
        .collect();
    assert_eq!(
        sessions.len(),
        1,
        "one session for the key, whoever spawned it"
    );
    assert_eq!(sessions[0].briefs.len(), 1, "briefed once");
    env.assert_no_temp();
}

#[test]
fn a_corrupt_record_is_unknown_never_absent() {
    let env = setup("corrupt", true);
    fs::write(
        env.object_path("Task", "t1"),
        b"{\"apiVersion\": \"fleet/v1al",
    )
    .unwrap();
    env.fleet(&["get", "task", "t1"])
        .code(4)
        .stderr_has("corrupt");
    env.fleet(&["get", "task", "nope"]).code(3);
    env.fleet(&["list", "task"]).code(4);
    let report = env.reconcile().ok().json();
    assert!(
        report["unknowns"].to_string().contains("list Task"),
        "{report:#}"
    );
    assert_eq!(
        env.calls("spawn"),
        0,
        "nothing acts on what it could not read"
    );
    env.apply(task("t1", true))
        .code(4)
        .stderr_has("could not tell");
}

#[cfg(unix)]
#[test]
fn an_inaccessible_record_is_unknown_and_a_missing_one_is_not_found() {
    use std::os::unix::fs::PermissionsExt;
    let env = setup("inaccessible", true);
    let path = env.object_path("Task", "t1");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o000)).unwrap();
    // Root reads anything; the distinction only exists for other users.
    if fs::read(&path).is_ok() {
        return;
    }
    env.fleet(&["get", "task", "t1"])
        .code(4)
        .stderr_has("inaccessible");
    env.fleet(&["get", "task", "t2"]).code(3);
    fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
}

#[test]
fn a_failing_release_asks_for_attention_and_keeps_its_finalizer_until_it_succeeds() {
    let env = setup("release-bounded", true);
    env.reconcile().ok();
    env.host()
        .set_at_rest(&env.session_key("t1"), true)
        .unwrap();
    env.fault(Fault::new("release", "fail").times(3));
    env.fleet(&["delete", "task", "t1"]).ok();
    env.reconcile().ok();
    assert_eq!(
        env.calls("release"),
        1,
        "one try per wait, not three in one run"
    );
    for _ in 0..2 {
        env.expire_wait("t1");
        env.reconcile().ok();
    }
    let st = env.status("t1");
    assert_condition(&st, "Released", "False", "ReleaseFailed");
    assert_condition(&st, "NeedsAttention", "True", "ReleaseFailed");
    assert_eq!(
        st["finalizers"],
        json!(["fleet.dev/release"]),
        "the finalizer holds the record"
    );
    // The runtime recovers: the release goes through and the Task goes.
    env.expire_wait("t1");
    env.reconcile().ok();
    assert_eq!(env.calls("release"), 4);
    env.fleet(&["get", "task", "t1"]).code(3);
}

#[test]
fn a_task_deleted_before_it_spawned_goes_at_once() {
    let env = setup("delete-pending", false);
    env.fleet(&["delete", "task", "t1"])
        .ok()
        .stdout_has("marked for deletion");
    env.reconcile().ok();
    env.fleet(&["get", "task", "t1"]).code(3);
    assert_eq!(env.calls("release"), 0);
    // A deleting Task takes no worker result and no new spec.
    env.fleet(&["delete", "task", "t1"]).code(3);
}

#[test]
fn concurrent_applies_create_exactly_once() {
    let env = Env::new("apply-race");
    env.apply(base_config()).ok();
    let path = env.root.join("t.json");
    fs::write(&path, serde_json::to_vec(&task("race", true)).unwrap()).unwrap();
    let path = path.to_string_lossy().into_owned();
    let children: Vec<_> = (0..8).map(|_| env.spawn(&["apply", "-f", &path])).collect();
    let runs: Vec<_> = children.into_iter().map(wait).collect();
    for r in &runs {
        r.ok();
    }
    let created = runs.iter().filter(|r| r.stdout.contains("created")).count();
    assert_eq!(created, 1, "{runs:#?}");
    assert_eq!(env.get("race")["metadata"]["generation"], 1);
    env.assert_no_temp();
}

#[test]
fn invalid_and_retired_requests_are_refused_with_a_reason() {
    let env = setup("unsupported", false);
    let run = json!({"apiVersion": "fleet/v1alpha1", "kind": "Run", "metadata": {"name": "r"}, "spec": {}});
    env.apply(run).code(1).stderr_has("unknown kind");
    for args in [
        &["list", "policy"][..],
        &["get", "modelprovider", "worker"][..],
    ] {
        env.fleet(args).code(1).stderr_has("unknown kind");
    }
    let mut blocked = task("x", false);
    blocked["spec"]["blockedBy"] = json!(["y"]);
    env.apply(blocked)
        .code(1)
        .stderr_has("unknown field `blockedBy`");
    let mut with_status = task("t9", false);
    with_status["status"] = json!({"phase": "Concluded"});
    env.apply(with_status)
        .code(1)
        .stderr_has("status is written by controllers");
    let mut old_api = task("t8", false);
    old_api["apiVersion"] = json!("fleet/v0");
    env.apply(old_api).code(1).stderr_has("not served");
    let mut owned = task("t7", false);
    owned["metadata"]["ownerReferences"] =
        json!([{"kind": "Task", "name": "ghost", "uid": "u999"}]);
    env.apply(owned)
        .code(1)
        .stderr_has("ownerReferences is not served");

    fs::write(
        &env.config,
        "version = 1\n[data]\ndir = \"data\"\n[runtime]\nkind = \"docker\"\n",
    )
    .unwrap();
    env.fleet(&["reconcile"])
        .code(1)
        .stderr_has("unknown runtime kind");
    fs::write(&env.config, "version = 2\n").unwrap();
    env.fleet(&["list", "task"])
        .code(1)
        .stderr_has("version 2 is not served");
}

#[test]
fn a_foreign_directory_is_never_turned_into_a_store() {
    let env = Env::new("foreign");
    let foreign = env.root.join("someones-files");
    fs::create_dir_all(&foreign).unwrap();
    fs::write(foreign.join("notes.txt"), b"keep me").unwrap();
    let dir = foreign.to_string_lossy().into_owned();
    env.fleet(&["--data-dir", &dir, "list", "task"])
        .code(1)
        .stderr_has("not a fleet store");
    assert_eq!(fs::read(foreign.join("notes.txt")).unwrap(), b"keep me");
    assert!(!foreign.join("store.json").exists());
}

#[test]
fn version_reports_every_schema() {
    let env = Env::new("version");
    let v = env.fleet(&["version", "--json"]).ok().json();
    assert_eq!(v["apiVersions"], json!(["fleet/v1alpha1"]));
    assert_eq!(v["configVersion"], 1);
    assert_eq!(v["storeFormat"]["version"], 1);
    assert!(v["version"].as_str().is_some_and(|s| !s.is_empty()));
    env.fleet(&["--version"]).ok().stdout_has("fleet ");
}

#[test]
fn a_runtime_section_is_the_fake_and_carries_no_repository_map() {
    let env = Env::new("runtime-section");
    let refused = |config: &str, why: &str| {
        std::fs::write(&env.config, config).expect("config");
        let run = env.fleet(&["reconcile", "--json"]);
        assert_ne!(run.code, Some(0), "{config} must be refused: {run:?}");
        run.stderr_has(why);
    };
    refused(
        "version = 1\n\n[data]\ndir = \"data\"\n\n[runtime]\nkind = \"thurbox\"\n",
        "unknown runtime kind \"thurbox\"",
    );
    refused(
        "version = 1\n\n[data]\ndir = \"data\"\n\n[runtime]\nkind = \"fake\"\ndir = \"fake\"\n\n[runtime.repositories]\n\"https://example.test/r\" = \"/src/r\"\n",
        "unknown field `repositories`",
    );
    std::fs::write(
        &env.config,
        "version = 1\n\n[data]\ndir = \"data\"\n\n[runtime]\nkind = \"fake\"\ndir = \"fake\"\n",
    )
    .expect("config");
    assert_eq!(
        env.fleet(&["version", "--json"]).ok().json()["runtimes"],
        json!(["exec", "fake"])
    );
}

#[test]
fn the_core_runs_with_no_fleet_extension_and_no_tools_on_path() {
    // No runtime program, no fleet extension, no Lua pane, no shell: PATH is an
    // empty directory and HOME is the test's own. The whole lifecycle still
    // runs, because the foundation reaches nothing outside its data
    // directory and the configured runtime.
    let env = Env::without_tools("no-extension");
    env.apply(base_config()).ok();
    env.apply(task("t1", true)).ok();
    env.reconcile().ok();
    assert_eq!(env.status("t1")["phase"], "Briefed");
    env.fleet(&result_args("1", &["--output", "prUrl=\"u\""]))
        .ok();
    env.reconcile().ok();
    assert_eq!(env.status("t1")["phase"], "Concluded");
    env.host()
        .set_at_rest(&env.session_key("t1"), true)
        .unwrap();
    env.fleet(&["delete", "task", "t1"]).ok();
    env.reconcile().ok();
    env.fleet(&["get", "task", "t1"]).code(3);

    // Nothing was written outside the test's directories: no extension
    // home, no default data directory under HOME.
    let mut entries: Vec<String> = fs::read_dir(&env.root)
        .unwrap()
        .filter_map(Result::ok)
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    entries.retain(|e| !e.starts_with("apply-"));
    entries.sort();
    assert_eq!(entries, ["data", "empty-path", "fake", "fleet.toml", "tmp"]);
    assert!(
        fs::read_dir(env.root.join("empty-path"))
            .unwrap()
            .next()
            .is_none()
    );
}

/// The examples in docs/fleet/contracts.md are what the binary accepts.
#[test]
fn the_contract_examples_apply_as_written() {
    // A Windows checkout ends each line with CRLF.
    let doc = fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/fleet/contracts.md"),
    )
    .unwrap()
    .replace("\r\n", "\n");
    let block = |lang: &str| -> String {
        let start = doc
            .find(&format!("```{lang}\n"))
            .unwrap_or_else(|| panic!("no {lang} block"))
            + lang.len()
            + 4;
        let end = start + doc[start..].find("```").unwrap();
        doc[start..end].to_string()
    };
    let env = Env::new("contract-examples");
    fs::write(&env.config, block("toml")).unwrap();
    let resources: serde_json::Value = serde_json::from_str(&block("json")).unwrap();
    let applied = env.apply(resources).ok().stdout.clone();
    assert!(applied.contains("Task/design-305 created"), "{applied}");
    env.reconcile().ok();
    let st = env
        .fleet(&["status", "task", "design-305", "--json"])
        .ok()
        .json();
    assert_eq!(st["phase"], "Briefed", "{st:#}");
}

#[test]
fn a_task_alone_runs_from_apply_to_release_and_retired_kinds_are_refused() {
    let env = Env::new("task-only");
    env.apply(json!({
        "apiVersion": "fleet/v1alpha1", "kind": "Task", "metadata": {"name": "solo"},
        "spec": {"harness": "claude-code", "host": "local", "brief": "Do it.",
                 "workspace": [], "dispatch": true}
    }))
    .ok()
    .stdout_has("Task/solo created");
    env.reconcile().ok();
    let st = env.status("solo");
    assert_eq!(st["phase"], "Briefed", "{st:#}");
    env.fleet(&[
        "result",
        "solo",
        "--generation",
        "1",
        "--outcome",
        "shipped",
    ])
    .ok();
    env.reconcile().ok();
    assert_eq!(env.status("solo")["phase"], "Concluded");
    env.host()
        .set_at_rest(&env.session_key("solo"), true)
        .unwrap();
    env.fleet(&["delete", "task", "solo"]).ok();
    env.reconcile().ok();
    env.fleet(&["get", "task", "solo"]).code(3);
    for kind in [
        "Policy",
        "Host",
        "ModelProvider",
        "Conductor",
        "Forge",
        "ForgeTask",
        "ControllerPlugin",
    ] {
        env.apply(object(kind, "x", json!({})))
            .code(1)
            .stderr_has("unknown kind");
    }
}

#[test]
fn empty_owner_references_are_an_idempotent_apply() {
    let env = Env::new("empty-owner-references");
    let mut manifest = task("t1", false);
    manifest["metadata"]["ownerReferences"] = json!([]);
    env.apply(manifest).ok();
    let before = env.get("t1");
    env.apply(task("t1", false)).ok().stdout_has("unchanged");
    assert_eq!(env.get("t1"), before);
}

#[test]
fn incomplete_persisted_metadata_is_unknown_not_repaired() {
    for field in [
        "uid",
        "resourceVersion",
        "generation",
        "creationTimestamp",
        "finalizers",
    ] {
        let env = setup(&format!("incomplete-metadata-{field}"), false);
        let path = env.object_path("Task", "t1");
        let mut stored = env.get("t1");
        stored["metadata"].as_object_mut().unwrap().remove(field);
        let bytes = serde_json::to_vec(&stored).unwrap();
        fs::write(&path, &bytes).unwrap();
        env.fleet(&["get", "task", "t1"]).code(4);
        env.apply(task("t1", false)).code(4);
        assert_eq!(
            fs::read(&path).unwrap(),
            bytes,
            "never repair corrupt identity"
        );
    }
}

#[test]
fn unsupported_store_format_is_refused_without_changing_its_files() {
    let env = Env::new("unsupported-store-format");
    fs::create_dir_all(&env.data).unwrap();
    let marker = env.data.join("store.json");
    let bytes = br#"{"format":"fleet-store","version":2}"#;
    fs::write(&marker, bytes).unwrap();
    env.apply(task("t1", false)).code(1).stderr_has("version 2");
    assert_eq!(fs::read(marker).unwrap(), bytes);
    assert_eq!(fs::read_dir(&env.data).unwrap().count(), 1);
}

#[test]
fn a_released_task_recovers_when_finalizer_clear_landed_before_unlink() {
    let env = setup("finalizer-unlink-recovery", true);
    env.reconcile().ok();
    env.host()
        .set_at_rest(&env.session_key("t1"), true)
        .unwrap();
    env.fleet(&["delete", "task", "t1"]).ok();
    env.fleet(&["reconcile", "--max-passes", "1"]).ok();
    let mut stored = env.get("t1");
    assert!(
        stored["status"]["receipts"]
            .as_object()
            .unwrap()
            .values()
            .any(|r| r["outcome"]["known"]["type"] == "released")
    );
    // A crash after durable finalizer clearing, before the record's unlink.
    stored["metadata"]["finalizers"] = json!([]);
    fs::write(
        env.object_path("Task", "t1"),
        serde_json::to_vec(&stored).unwrap(),
    )
    .unwrap();
    env.reconcile().ok();
    env.fleet(&["get", "task", "t1"]).code(3);
    assert_eq!(env.calls("release"), 1, "the release already succeeded");
}

#[test]
fn unsupported_resource_envelope_and_metadata_are_refused() {
    let mut root_field = task("t1", false);
    root_field["unexpected"] = json!(true);
    let env = Env::new("unsupported-resource-field");
    env.apply(root_field).code(1).stderr_has("unknown field");
    env.fleet(&["get", "task", "t1"]).code(3);
    for (field, value) in [
        ("namespace", json!("another-namespace")),
        ("annotations", json!({"purpose": "do not silently discard"})),
    ] {
        let env = Env::new(&format!("unsupported-metadata-{field}"));
        let mut manifest = task("t1", false);
        manifest["metadata"][field] = value;
        env.apply(manifest).code(1).stderr_has("unknown field");
        env.fleet(&["get", "task", "t1"]).code(3);
    }
}

#[test]
fn native_resource_metadata_survives_task_updates() {
    let env = setup("native-metadata", false);
    let created = env.get("t1");
    let version = env.fleet(&["version", "--json"]).ok().json();
    assert_eq!(version["storeFormat"]["version"], 1);
    assert!(created["metadata"]["resourceVersion"].is_u64());
    assert!(created["metadata"]["creationTimestamp"].is_u64());
    assert_eq!(created["metadata"]["generation"], 1);
    env.apply(task("t1", true)).ok();
    env.reconcile().ok();
    let briefed = env.get("t1");
    assert_eq!(briefed["metadata"]["uid"], created["metadata"]["uid"]);
    assert_eq!(briefed["metadata"]["generation"], 2);
    assert_eq!(briefed["status"]["phase"], "Briefed");
    assert!(briefed["metadata"]["resourceVersion"].is_u64());
}

#[test]
fn exhausted_resource_version_refuses_apply_without_changing_data() {
    let env = setup("resource-version-exhausted", false);
    let path = env.object_path("Task", "t1");
    let before = fs::read(&path).unwrap();
    let sequence = env.data.join("seq");
    let maximum = u64::MAX.to_string();
    fs::write(&sequence, &maximum).unwrap();
    env.apply(task("t1", true))
        .code(1)
        .stderr_has("resourceVersion exhausted");
    assert_eq!(fs::read(path).unwrap(), before);
    assert_eq!(fs::read_to_string(sequence).unwrap(), maximum);
}

#[test]
fn exhausted_generation_refuses_apply_without_changing_the_record() {
    let env = setup("generation-exhausted", false);
    let path = env.object_path("Task", "t1");
    let mut stored = env.get("t1");
    stored["metadata"]["generation"] = json!(u64::MAX);
    let before = serde_json::to_vec(&stored).unwrap();
    fs::write(&path, &before).unwrap();
    env.apply(task("t1", true))
        .code(1)
        .stderr_has("generation exhausted");
    assert_eq!(fs::read(path).unwrap(), before);
}

#[test]
fn a_reboot_revives_the_session_and_resumes_the_brief_once() {
    let env = setup("reboot", true);
    env.reconcile().ok();
    let key = env.session_key("t1");
    let id = env.host().session(&key).unwrap().unwrap().id;

    env.host().reboot().unwrap();
    env.reconcile().ok();

    assert_eq!((env.calls("spawn"), env.calls("revive")), (1, 1));
    let session = env.host().session(&key).unwrap().unwrap();
    assert_eq!(session.id, id, "the same session, not a second one");
    let uid = env.get("t1")["metadata"]["uid"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(
        session.briefs,
        vec![
            format!("task/t1/{uid}/brief"),
            format!("task/t1/{uid}/catchup/1")
        ],
        "the brief once, then one catch-up note"
    );
    // The revived worker is told it was restarted, and given its brief
    // again, for the generation it was briefed for.
    assert_eq!(session.texts.len(), 2, "{:#?}", session.texts);
    let note = &session.texts[1];
    assert!(note.contains("restarted"), "{note}");
    assert!(note.contains("Do the work for t1."), "{note}");
    assert!(note.contains("generation 1"), "{note}");
    let st = env.status("t1");
    assert_eq!(st["phase"], "Briefed", "{st:#}");
    assert_condition(&st, "SessionLost", "False", "Revived");

    let again = env.reconcile().ok().json();
    assert_eq!(again["effects"], 0, "{again:#}");
    // The generation the worker was briefed for still fences its result.
    env.fleet(&result_args(
        "2",
        &["--output", "prUrl=\"https://example.test/pr/1\""],
    ))
    .code(1)
    .stderr_has("stale generation 2");
    env.fleet(&result_args(
        "1",
        &["--output", "prUrl=\"https://example.test/pr/1\""],
    ))
    .ok();
}

#[test]
fn a_host_that_cannot_be_read_is_unknown_for_its_own_tasks_only() {
    let env = Env::new("host-unknown-scope");
    env.host().set_host("remote", "up").unwrap();
    let mut far = task("far", true);
    far["spec"]["host"] = json!("remote");
    // The fake host reads no remote agent's prompt hook: the runtime's own
    // answer proves this harness's brief.
    far["spec"]["harness"] = json!("aider");
    env.apply(far).ok();
    env.reconcile().ok();
    let st = env.status("far");
    assert_eq!(st["phase"], "Briefed", "{st:#}");

    // The remote host stops answering. Its Task is Unknown: no WorkerGone
    // clock, no respawn. A local Task carries on in the same run.
    env.host().set_host("remote", "unknown").unwrap();
    env.apply(task("near", true)).ok();
    let report = env.reconcile().ok().json();
    assert_eq!(env.status("near")["phase"], "Briefed");
    let st = env.status("far");
    assert_eq!(st["phase"], "Briefed", "{st:#}");
    assert!(
        st["conditions"]
            .as_array()
            .unwrap()
            .iter()
            .all(|c| c["type"] != "WorkerGone"),
        "{st:#}"
    );
    assert!(
        report["unknowns"].to_string().contains("remote"),
        "{report:#}"
    );
    assert_eq!(env.calls("spawn"), 2, "nothing respawned");

    // A host known to be down is False: nothing is spawned there.
    env.host().set_host("remote", "down").unwrap();
    let mut blocked = task("blocked", true);
    blocked["spec"]["host"] = json!("remote");
    env.apply(blocked).ok();
    env.reconcile().ok();
    assert_condition(
        &env.status("blocked"),
        "HostReachable",
        "False",
        "Unreachable",
    );
    assert_eq!(env.calls("spawn"), 2);
}

#[test]
fn a_release_whose_answer_is_unknown_is_never_assumed_done() {
    // The release happened but its answer was lost: the finalizer holds the
    // record until a later attempt knows it is done.
    let env = setup("release-answer-lost", true);
    env.reconcile().ok();
    env.host()
        .set_at_rest(&env.session_key("t1"), true)
        .unwrap();
    env.fault(Fault::new("release", "unknown").performed());
    env.fleet(&["delete", "task", "t1"]).ok();
    env.reconcile().ok();
    assert_eq!(env.status("t1")["finalizers"], json!(["fleet.dev/release"]));
    env.expire_wait("t1");
    env.reconcile().ok();
    env.fleet(&["get", "task", "t1"]).code(3);

    // The release never answers. After three attempts, each after a wait,
    // a person is asked; the finalizer holds the record meanwhile.
    let env = setup("release-unknown", true);
    env.reconcile().ok();
    env.host()
        .set_at_rest(&env.session_key("t1"), true)
        .unwrap();
    env.fault(Fault::new("release", "unknown").times(3));
    env.fleet(&["delete", "task", "t1"]).ok();
    env.reconcile().ok();
    for _ in 0..2 {
        env.expire_wait("t1");
        env.reconcile().ok();
    }
    let st = env.status("t1");
    assert_condition(&st, "Released", "False", "ReleaseFailed");
    assert_condition(&st, "NeedsAttention", "True", "ReleaseFailed");
    assert_eq!(st["finalizers"], json!(["fleet.dev/release"]));
    assert_eq!(env.calls("release"), 3);
}

#[test]
fn a_different_session_reusing_the_key_does_not_hide_a_vanished_worker() {
    let env = setup("worker-identity", true);
    env.reconcile().ok();
    let original = env.get("t1")["status"]["session"]["id"].clone();
    // Another session now holds the Task's key: the one Fleet started is gone.
    let key = env.session_key("t1");
    env.host()
        .update(|s| s.sessions.get_mut(&key).unwrap().id = "fake-other".into())
        .unwrap();
    env.reconcile().ok();
    let st = env.status("t1");
    assert_condition(&st, "WorkerGone", "True", "SessionGone");
    assert_condition(&st, "NeedsAttention", "True", "WorkerGone");
    assert_eq!(env.get("t1")["status"]["session"]["id"], original);
    assert_eq!(env.calls("spawn"), 1, "never respawned");
    assert_eq!(env.calls("brief"), 1, "never rebriefed");
}

#[test]
fn a_new_store_never_adopts_a_session_an_old_store_left_behind() {
    let env = setup("two-stores", true);
    env.reconcile().ok();
    assert_eq!(env.host().state().unwrap().sessions.len(), 1);
    // A fresh data directory on the same host: same names, same order.
    let cfg = fs::read_to_string(&env.config).unwrap();
    fs::write(
        &env.config,
        cfg.replace("dir = \"data\"", "dir = \"data2\""),
    )
    .unwrap();
    env.apply(task("t1", true)).ok();
    env.reconcile().ok();
    let sessions = env.host().state().unwrap().sessions;
    assert_eq!(
        sessions.len(),
        2,
        "the second store's t1 has its own session: {sessions:#?}"
    );
}

#[test]
fn slow_probes_never_outlive_the_lease() {
    let env = setup("slow-probes", true);
    // Each probe fits the lease; the probes of one Task together do not.
    for probe in ["host", "find", "ready"] {
        env.fault(Fault::new(probe, "normal").delay(1_200).times(2));
    }
    env.fleet(&["reconcile", "--json", "--lease-ttl-ms", "3000"])
        .ok();
    assert_eq!(env.status("t1")["phase"], "Briefed");
}

#[test]
fn a_spec_that_would_reach_a_runtime_as_a_flag_is_refused_when_written() {
    let env = Env::new("flag-shaped-spec");
    for (field, value) in [("harness", "--help"), ("host", "-x")] {
        let mut t = task("dash", false);
        t["spec"][field] = json!(value);
        env.apply(t).code(1).stderr_has(field);
    }
    env.fleet(&["get", "task", "dash"]).code(3);
}

#[test]
fn a_reboot_revives_only_work_in_flight_or_feedback() {
    let env = Env::new("reboot-scope");
    env.apply(task("done", true)).ok();
    env.apply(task("held", false)).ok();
    env.reconcile().ok();
    env.fleet(&[
        "result",
        "done",
        "--generation",
        "1",
        "--outcome",
        "shipped",
        "--output",
        "prUrl=\"https://example.test/pr/1\"",
    ])
    .ok();
    env.reconcile().ok();
    assert_eq!(env.status("done")["phase"], "Concluded");
    assert_eq!(env.status("held")["phase"], "Pending");

    env.host().reboot().unwrap();
    env.expire_session_check("done");
    env.reconcile().ok();
    assert_eq!(env.calls("revive"), 0, "a concluded worker rests");
    assert_eq!(env.status("held")["phase"], "Pending", "held stays held");
    assert_eq!(env.calls("spawn"), 1);

    // Feedback for the concluded worker brings its session back to read it.
    env.fleet(&["send", "done", "Review: rename the flag."])
        .ok();
    env.reconcile().ok();
    assert_eq!((env.calls("revive"), env.calls("spawn")), (1, 1));
    // The note that it was restarted goes first; the feedback follows it.
    let texts = env
        .host()
        .session(&env.session_key("done"))
        .unwrap()
        .unwrap()
        .texts;
    assert_eq!(texts.len(), 3, "{texts:#?}");
    assert!(texts[1].contains("already recorded"), "{}", texts[1]);
    assert_eq!(texts[2], "Review: rename the flag.");
    assert_eq!(env.status("done")["phase"], "Concluded");
}

#[test]
fn a_task_deleted_after_a_reboot_releases_its_lost_session() {
    let env = setup("reboot-delete", true);
    env.reconcile().ok();
    let key = env.session_key("t1");
    env.host().reboot().unwrap();
    env.fleet(&["delete", "task", "t1"]).ok();
    env.reconcile().ok();
    assert_eq!(env.calls("revive"), 0, "a deleted Task is not revived");
    // Whether a lost session is at rest is the runtime's to say.
    env.host().set_at_rest(&key, true).unwrap();
    env.reconcile().ok();
    assert_eq!(env.calls("revive"), 0);
    let session = env.host().session(&key).unwrap().unwrap();
    assert!(session.released, "{session:#?}");
    env.fleet(&["get", "task", "t1"]).code(3);
}

#[test]
fn a_task_reports_its_session_and_what_the_runtime_proves_of_it() {
    let env = setup("session-state", true);
    env.reconcile().ok();
    let st = env.status("t1");
    assert_eq!(st["phase"], "Briefed", "{st:#}");
    assert_eq!(st["session"]["key"], env.session_key("t1"), "{st:#}");
    assert_eq!(st["sessionState"], "Running", "{st:#}");
    env.fleet(&["status", "task", "t1"])
        .ok()
        .stdout_has("session fake-1 on local Running")
        .stdout_has("Running");

    // The runtime still holds the session, but its agent's window is gone:
    // the pass that sees it says so, and the revive brings it back.
    env.host().reboot().unwrap();
    env.fleet(&["reconcile", "--max-passes", "1"]).ok();
    assert_eq!(env.status("t1")["sessionState"], "Lost");
    env.reconcile().ok();
    assert_eq!(env.status("t1")["sessionState"], "Running");

    // A readable list that no longer shows the session proves it gone.
    env.host().vanish(&env.session_key("t1")).unwrap();
    env.reconcile().ok();
    let st = env.status("t1");
    assert_eq!(st["sessionState"], "Gone", "{st:#}");
    assert_condition(&st, "WorkerGone", "True", "SessionGone");
}
