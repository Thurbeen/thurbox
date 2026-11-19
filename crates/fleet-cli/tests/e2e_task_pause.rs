//! Pausing an idle agent session and resuming it when work arrives, end to
//! end: the real `fleet` binary, its file store, TaskController and the fake
//! host. The agent's activity is what its hooks report through `fleet
//! signal`.

mod support;

use fleet_runtime::{FakeSession, Fault};
use serde_json::{Value, json};
use support::{Env, assert_condition, object, wait};

const PAUSE_AFTER_MS: u64 = 600_000;

/// A Task whose TaskConfig pauses it when idle.
fn task(name: &str) -> Value {
    let mut t = support::task(name, true);
    t["spec"]["configRef"] = json!({"name": "pausing"});
    t
}

/// A Task with no pause setting.
fn plain_task(name: &str) -> Value {
    support::task(name, true)
}

fn setup(name: &str, tasks: Vec<Value>) -> Env {
    let env = Env::new(name);
    let mut items = Vec::new();
    if tasks.iter().any(|t| !t["spec"]["configRef"].is_null()) {
        items.push(object(
            "TaskConfig",
            "pausing",
            json!({"pauseAfterMs": PAUSE_AFTER_MS}),
        ));
    }
    items.extend(tasks);
    env.apply(json!(items)).ok();
    env
}

fn session(env: &Env) -> FakeSession {
    env.host()
        .session(&env.session_key("t1"))
        .unwrap()
        .expect("a session")
}

/// What the agent's hooks report: `idle`, `active` (working), or nothing,
/// which leaves its report empty.
fn set_activity(env: &Env, activity: Option<&str>) {
    let file = env
        .data
        .join("signals")
        .join(format!("{}.jsonl", env.session_key("t1")));
    let state = match activity {
        Some("idle") => "idle",
        Some("active") => "working",
        _ => {
            let _ = std::fs::remove_file(&file);
            return;
        }
    };
    let mut cmd = env.command(&["signal", state]);
    cmd.env("FLEET_SIGNAL_FILE", &file);
    wait(cmd.spawn().expect("fleet signal")).ok();
}

/// Let the Task's idle time run past `pauseAfterMs` now, as time would.
fn expire_idle(env: &Env, name: &str) {
    let path = env.object_path("Task", name);
    let mut obj: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert!(
        obj["status"]["idleSince"].is_u64(),
        "no idle time to expire: {:#} {:#?}",
        obj["status"],
        session(env)
    );
    obj["status"]["idleSince"] = json!(0);
    std::fs::write(&path, obj.to_string()).unwrap();
}

/// Every text the session was given, in order.
fn sent(env: &Env) -> Vec<String> {
    session(env).texts
}

fn stopped(env: &Env) -> bool {
    session(env).paused
}

/// Brief the Task, then let it be seen idle for longer than `pauseAfterMs`.
fn pause(env: &Env, name: &str) {
    env.reconcile().ok();
    assert_eq!(env.status(name)["phase"], "Briefed");
    assert!(
        session(env).env.contains_key("FLEET_SIGNAL_FILE"),
        "the agent was given its signal file"
    );
    set_activity(env, Some("idle"));
    env.reconcile().ok();
    assert_eq!(env.calls("pause"), 0, "idle, but not for long enough yet");
    expire_idle(env, name);
    env.reconcile().ok();
}

#[test]
fn an_idle_task_is_paused_and_an_entry_resumes_it_with_no_note_of_fleets() {
    let env = setup("pause-resume", vec![task("t1")]);
    pause(&env, "t1");

    assert_eq!(env.calls("pause"), 1);
    assert!(stopped(&env));
    let st = env.status("t1");
    assert_eq!(st["phase"], "Briefed", "{st:#}");
    assert_condition(&st, "Paused", "True", "Idle");
    for c in ["WorkerGone", "SessionLost"] {
        let raised = st["conditions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|x| x["type"] == c && x["status"] == "True");
        assert!(!raised, "{c} in {st:#}");
    }

    // A paused session rests: nothing is stopped twice, nothing restarts.
    env.reconcile().ok();
    assert_eq!(env.calls("pause"), 1);
    assert_eq!(env.calls("resume"), 0);
    assert_eq!(env.calls("revive"), 0);

    env.fleet(&["send", "t1", "next message"]).ok();
    env.fleet(&["send", "t1", "and another"]).ok();
    env.reconcile().ok();

    assert_eq!(env.calls("resume"), 1, "one resume for both entries");
    assert!(!stopped(&env));
    let st = env.status("t1");
    assert_condition(&st, "Paused", "False", "Resumed");
    let texts = sent(&env);
    assert_eq!(
        texts,
        ["Do the work for t1.", "next message", "and another"],
        "the entries, and nothing of Fleet's own"
    );

    env.reconcile().ok();
    assert_eq!(sent(&env).len(), 3, "nothing is sent twice");
    assert_eq!(env.calls("resume"), 1);
}

#[test]
fn unknown_activity_never_pauses() {
    let env = setup("pause-unknown", vec![task("t1")]);
    env.reconcile().ok();
    for activity in [None, Some("unknown"), Some("active")] {
        set_activity(&env, activity);
        env.reconcile().ok();
        env.reconcile().ok();
        assert!(
            env.get("t1")["status"]["idleSince"].is_null(),
            "{activity:?}"
        );
    }
    // Active resets the idle time: it starts again from the next Idle.
    set_activity(&env, Some("idle"));
    env.reconcile().ok();
    expire_idle(&env, "t1");
    set_activity(&env, Some("active"));
    env.reconcile().ok();
    assert!(env.get("t1")["status"]["idleSince"].is_null());
    set_activity(&env, Some("idle"));
    env.reconcile().ok();
    assert_eq!(env.calls("pause"), 0);
}

#[test]
fn a_task_without_a_pause_setting_is_never_paused() {
    let env = setup("pause-absent", vec![plain_task("t1")]);
    env.reconcile().ok();
    set_activity(&env, Some("idle"));
    for _ in 0..3 {
        env.reconcile().ok();
    }
    assert_eq!(env.calls("pause"), 0);
    assert!(env.get("t1")["status"]["idleSince"].is_null());
}

#[test]
fn a_concluded_task_is_paused_too() {
    let env = setup("pause-concluded", vec![task("t1")]);
    env.reconcile().ok();
    env.fleet(&[
        "result",
        "t1",
        "--generation",
        "1",
        "--outcome",
        "shipped",
        "--output",
        "prUrl=\"https://example.invalid/pr/1\"",
    ])
    .ok();
    env.reconcile().ok();
    assert_eq!(env.status("t1")["phase"], "Concluded");
    set_activity(&env, Some("idle"));
    env.reconcile().ok();
    expire_idle(&env, "t1");
    env.reconcile().ok();
    assert_eq!(env.calls("pause"), 1);
    let st = env.status("t1");
    assert_eq!(st["phase"], "Concluded");
    assert_condition(&st, "Paused", "True", "Idle");

    env.fleet(&["send", "t1", "Review: rename the flag."]).ok();
    env.reconcile().ok();
    assert_eq!(env.calls("resume"), 1);
    let texts = sent(&env);
    assert_eq!(texts.len(), 2, "{texts:#?}");
    assert_eq!(texts[1], "Review: rename the flag.");
}

#[test]
fn a_session_stopped_outside_fleet_resumes_on_the_next_entry() {
    // Before, a stopped session only read as not ready: the entry waited in
    // the inbox for good, and nothing said why.
    let env = setup("pause-external", vec![plain_task("t1")]);
    env.reconcile().ok();
    let key = env.session_key("t1");
    env.host()
        .update(|s| {
            let x = s.sessions.get_mut(&key).unwrap();
            x.paused = true;
            x.ready = false;
        })
        .unwrap();
    env.reconcile().ok();
    assert_condition(&env.status("t1"), "Paused", "True", "External");
    assert_eq!(env.calls("resume"), 0, "nothing to deliver yet");

    env.fleet(&["send", "t1", "are you there?"]).ok();
    env.reconcile().ok();
    assert_eq!(env.calls("resume"), 1);
    let texts = sent(&env);
    assert_eq!(texts, ["Do the work for t1.", "are you there?"]);
    assert_condition(&env.status("t1"), "Paused", "False", "Resumed");
}

#[test]
fn a_held_task_is_never_resumed() {
    let env = setup("pause-held", vec![task("t1")]);
    pause(&env, "t1");
    assert!(stopped(&env));
    let mut held = task("t1");
    held["spec"]["dispatch"] = json!(false);
    env.apply(json!([held])).ok();
    env.fleet(&["send", "t1", "wait"]).ok();
    env.reconcile().ok();
    assert_eq!(env.calls("resume"), 0);
    assert!(stopped(&env));
}

#[test]
fn deleting_a_paused_task_releases_it() {
    let env = setup("pause-delete", vec![task("t1")]);
    pause(&env, "t1");
    assert!(stopped(&env));
    let key = env.session_key("t1");
    env.fleet(&["delete", "task", "t1"]).ok();
    env.reconcile().ok();
    assert_eq!(env.calls("resume"), 0, "a deleted Task is not resumed");
    let released = env.host().session(&key).unwrap().expect("a session");
    assert!(released.released, "{released:#?}");
    env.fleet(&["get", "task", "t1"]).code(3);
}

#[test]
fn a_pause_whose_answer_was_lost_is_read_back_and_never_repeated() {
    let env = setup("pause-lost-answer", vec![task("t1")]);
    // The stop happens, and its answer never comes back.
    env.fault(Fault::new("pause", "unknown").performed());
    pause(&env, "t1");
    assert!(stopped(&env));
    env.reconcile().ok();
    assert_eq!(env.calls("pause"), 1, "read back, never repeated");
    assert_condition(&env.status("t1"), "Paused", "True", "Idle");
}

#[test]
fn a_resume_whose_answer_was_lost_is_read_back_and_never_repeated() {
    let env = setup("resume-lost-answer", vec![task("t1")]);
    pause(&env, "t1");
    env.fault(Fault::new("resume", "unknown").performed());
    env.fleet(&["send", "t1", "next message"]).ok();
    env.reconcile().ok();
    env.reconcile().ok();
    assert_eq!(env.calls("resume"), 1, "read back, never repeated");
    assert_condition(&env.status("t1"), "Paused", "False", "Resumed");
    let texts = sent(&env);
    assert_eq!(texts, ["Do the work for t1.", "next message"]);
}

#[test]
fn an_agent_fleet_cannot_resume_is_never_paused() {
    // aider has no resume argv: a stop would end its conversation.
    let mut aider = task("t1");
    aider["spec"]["harness"] = json!("aider");
    let env = setup("pause-aider", vec![aider]);
    env.reconcile().ok();
    assert_eq!(env.status("t1")["phase"], "Briefed");
    set_activity(&env, Some("idle"));
    for _ in 0..3 {
        env.reconcile().ok();
    }
    assert!(env.get("t1")["status"]["idleSince"].is_null());
    assert_eq!(env.calls("pause"), 0);
    assert!(!stopped(&env));
}

#[test]
fn a_view_opened_on_a_paused_task_resumes_it_with_no_entry() {
    let env = setup("pause-view", vec![task("t1")]);
    pause(&env, "t1");
    assert!(stopped(&env));

    let mut attach = env.spawn(&["attach", "t1", "--wake", "--json"]);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    while attach.try_wait().expect("attach").is_none() {
        assert!(
            std::time::Instant::now() < deadline,
            "attach never returned"
        );
        env.reconcile().ok();
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    let run = wait(attach);
    assert!(!run.stderr.contains("is paused"), "{}", run.stderr);

    assert_eq!(env.calls("resume"), 1);
    assert!(!stopped(&env));
    let st = env.status("t1");
    assert_condition(&st, "Paused", "False", "Resumed");
    assert!(
        st["inbox"].as_array().is_none_or(Vec::is_empty),
        "a view queues nothing: {st:#}"
    );
    for _ in 0..3 {
        env.reconcile().ok();
    }
    assert_eq!(sent(&env), ["Do the work for t1."], "a wake types nothing");
}

#[test]
fn a_view_asked_for_before_the_pause_resumes_nothing() {
    let env = setup("pause-stale-view", vec![task("t1")]);
    pause(&env, "t1");
    let path = env.object_path("Task", "t1");
    let mut obj: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    obj["status"]["wakeRequestedAt"] = json!(1);
    std::fs::write(&path, obj.to_string()).unwrap();

    env.reconcile().ok();
    env.reconcile().ok();
    assert_eq!(env.calls("resume"), 0);
    assert!(stopped(&env));
}

#[test]
fn task_controller_keeps_a_views_request_as_it_is() {
    let env = setup("pause-view-owner", vec![task("t1")]);
    pause(&env, "t1");
    let path = env.object_path("Task", "t1");
    let mut obj: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    obj["status"]["wakeRequestedAt"] = json!(u64::MAX);
    std::fs::write(&path, obj.to_string()).unwrap();
    env.reconcile().ok();
    assert_eq!(
        env.get("t1")["status"]["wakeRequestedAt"],
        json!(u64::MAX),
        "TaskController keeps the view's field as it is"
    );
}

#[test]
fn a_view_on_a_paused_held_task_says_so_at_once() {
    let env = setup("pause-view-held", vec![task("t1")]);
    pause(&env, "t1");
    let mut held = task("t1");
    held["spec"]["dispatch"] = json!(false);
    env.apply(json!([held])).ok();

    let started = std::time::Instant::now();
    env.fleet(&["attach", "t1", "--wake", "--json"])
        .code(3)
        .stderr_has("Task/t1 is paused and held");
    assert!(started.elapsed() < std::time::Duration::from_secs(10));
    env.reconcile().ok();
    assert_eq!(env.calls("resume"), 0);
}

#[test]
fn a_view_opened_on_a_paused_concluded_task_resumes_it_and_types_nothing() {
    let env = setup("pause-view-concluded", vec![task("t1")]);
    env.reconcile().ok();
    env.fleet(&[
        "result",
        "t1",
        "--generation",
        "1",
        "--outcome",
        "shipped",
        "--output",
        "prUrl=\"https://example.invalid/pr/1\"",
    ])
    .ok();
    env.reconcile().ok();
    set_activity(&env, Some("idle"));
    env.reconcile().ok();
    expire_idle(&env, "t1");
    env.reconcile().ok();
    assert_condition(&env.status("t1"), "Paused", "True", "Idle");
    assert_eq!(env.status("t1")["phase"], "Concluded");

    let mut attach = env.spawn(&["attach", "t1", "--wake", "--json"]);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    while attach.try_wait().expect("attach").is_none() && std::time::Instant::now() < deadline {
        env.reconcile().ok();
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    let _ = attach.kill();
    let run = wait(attach);
    assert!(!run.stderr.contains("is paused"), "{}", run.stderr);
    assert_eq!(env.calls("resume"), 1);
    assert_condition(&env.status("t1"), "Paused", "False", "Resumed");
    assert_eq!(env.status("t1")["phase"], "Concluded");

    for _ in 0..3 {
        env.reconcile().ok();
    }
    assert_eq!(sent(&env), ["Do the work for t1."], "a wake types nothing");
    assert!(env.get("t1")["status"]["catchUp"].is_null());
}

#[test]
fn a_background_attach_on_a_paused_task_says_so_and_wakes_nothing() {
    let env = setup("pause-view-background", vec![task("t1")]);
    pause(&env, "t1");

    env.fleet(&["attach", "t1", "--json"])
        .code(3)
        .stderr_has("Task/t1 is paused")
        .stderr_has("--wake");
    env.fleet(&["shell", "t1", "--json"])
        .code(3)
        .stderr_has("Task/t1 is paused");
    env.reconcile().ok();
    env.reconcile().ok();
    assert_eq!(env.calls("resume"), 0);
    assert!(stopped(&env));
    assert!(env.get("t1")["status"]["wakeRequestedAt"].is_null());
}

#[test]
fn a_view_on_a_paused_task_being_deleted_says_so_and_offers_no_wake() {
    let env = setup("pause-view-deleting", vec![task("t1")]);
    pause(&env, "t1");
    env.fleet(&["delete", "task", "t1"]).ok();
    assert!(!env.get("t1")["metadata"]["deletionTimestamp"].is_null());

    for args in [
        &["attach", "t1", "--json"][..],
        &["attach", "t1", "--wake", "--json"],
        &["shell", "t1", "--json"],
    ] {
        let run = env.fleet(args);
        run.code(3).stderr_has("Task/t1 is being deleted");
        assert!(!run.stderr.contains("--wake"), "{}", run.stderr);
    }
    assert!(env.get("t1")["status"]["wakeRequestedAt"].is_null());
}

fn wake_by_view(env: &Env) {
    let mut attach = env.spawn(&["attach", "t1", "--wake", "--json"]);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    while attach.try_wait().expect("attach").is_none() {
        assert!(
            std::time::Instant::now() < deadline,
            "attach never returned"
        );
        env.reconcile().ok();
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    let run = wait(attach);
    assert!(!run.stderr.contains("is paused"), "{}", run.stderr);
}

#[test]
fn fleet_pause_pauses_a_working_task_without_a_pause_setting_and_a_wake_resumes_it() {
    let env = setup("pause-requested", vec![plain_task("t1")]);
    env.reconcile().ok();
    assert_eq!(env.status("t1")["phase"], "Briefed");
    set_activity(&env, Some("active"));

    env.fleet(&["pause", "t1"])
        .ok()
        .stdout_has("Task/t1 pause requested");
    assert_eq!(env.calls("pause"), 0, "the verb only asks");
    env.reconcile().ok();

    assert_eq!(env.calls("pause"), 1);
    assert!(stopped(&env));
    let st = env.status("t1");
    assert_eq!(st["phase"], "Briefed", "{st:#}");
    assert_condition(&st, "Paused", "True", "Requested");
    env.reconcile().ok();
    assert_eq!(env.calls("pause"), 1, "a request is honoured once");

    wake_by_view(&env);
    assert_eq!(env.calls("resume"), 1);
    assert!(!stopped(&env));
    assert_condition(&env.status("t1"), "Paused", "False", "Resumed");
    for _ in 0..3 {
        env.reconcile().ok();
    }
    assert_eq!(
        env.calls("pause"),
        1,
        "an honoured request pauses nothing again"
    );
    assert_eq!(sent(&env), ["Do the work for t1."], "a pause types nothing");
}

#[test]
fn fleet_pause_pauses_a_concluded_task_at_rest() {
    let env = setup("pause-requested-concluded", vec![plain_task("t1")]);
    env.reconcile().ok();
    env.fleet(&[
        "result",
        "t1",
        "--generation",
        "1",
        "--outcome",
        "shipped",
        "--output",
        "prUrl=\"https://example.invalid/pr/1\"",
    ])
    .ok();
    env.reconcile().ok();
    env.reconcile().ok();
    assert_eq!(env.status("t1")["phase"], "Concluded");

    env.fleet(&["pause", "t1"]).ok();
    env.reconcile().ok();
    assert_eq!(env.calls("pause"), 1);
    assert_condition(&env.status("t1"), "Paused", "True", "Requested");

    env.fleet(&["send", "t1", "Review: rename the flag."]).ok();
    env.reconcile().ok();
    assert_eq!(env.calls("resume"), 1);
    assert_eq!(sent(&env)[1], "Review: rename the flag.");
}

#[test]
fn fleet_pause_names_a_missing_task() {
    let env = setup("pause-requested-missing", vec![plain_task("t1")]);
    env.fleet(&["pause", "nope"])
        .code(3)
        .stderr_has("Task/nope");
}
