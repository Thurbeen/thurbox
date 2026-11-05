//! Pausing an idle agent session and resuming it when work arrives, end to
//! end: the real `fleet` binary, its file store, TaskController and the
//! thurbox adapter driving the bounded `thurbox-cli` stand-in.

mod support;

use serde_json::{Value, json};
use support::{Env, assert_condition, object, runtime_config};

const PAUSE_AFTER_MS: u64 = 600_000;

/// A Task on the thurbox stub whose TaskConfig pauses it when idle.
fn task(name: &str) -> Value {
    let mut t = support::task(name, true);
    t["spec"]["workspace"]["dir"] = json!(format!("/work/{name}"));
    t["spec"]["configRef"] = json!({"name": "pausing"});
    t
}

/// A Task with no pause setting.
fn plain_task(name: &str) -> Value {
    let mut t = support::task(name, true);
    t["spec"]["workspace"]["dir"] = json!(format!("/work/{name}"));
    t
}

fn setup(name: &str, tasks: Vec<Value>) -> Env {
    let env = Env::thurbox(name);
    let mut items = runtime_config();
    if tasks.iter().any(|t| !t["spec"]["configRef"].is_null()) {
        items.as_array_mut().unwrap().push(object(
            "TaskConfig",
            "pausing",
            json!({"pauseAfterMs": PAUSE_AFTER_MS}),
        ));
    }
    items.as_array_mut().unwrap().extend(tasks);
    env.apply(items).ok();
    env
}

fn calls(env: &Env, sub: &str) -> usize {
    env.stub_calls_of(sub)
}

fn set_activity(env: &Env, activity: Option<&str>) {
    env.edit_stub(|s| {
        for sess in s["sessions"].as_array_mut().unwrap() {
            match activity {
                Some(a) => sess["activity"] = json!(a),
                None => {
                    sess.as_object_mut().unwrap().remove("activity");
                }
            }
        }
    });
}

/// Let the Task's idle time run past `pauseAfterMs` now, as time would.
fn expire_idle(env: &Env, name: &str) {
    let path = env.object_path("Task", name);
    let mut obj: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert!(
        obj["status"]["idleSince"].is_u64(),
        "no idle time to expire: {:#} {:#}",
        obj["status"],
        env.stub_sessions()[0]
    );
    obj["status"]["idleSince"] = json!(0);
    std::fs::write(&path, obj.to_string()).unwrap();
}

fn sent(env: &Env) -> Vec<String> {
    env.stub_sessions()[0]["sent"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t.as_str().unwrap().to_string())
        .collect()
}

fn stopped(env: &Env) -> bool {
    env.stub_sessions()[0]["stopped"] == true
}

/// Brief the Task, then let it be seen idle for longer than `pauseAfterMs`.
fn pause(env: &Env, name: &str) {
    env.reconcile().ok();
    assert_eq!(env.status(name)["phase"], "Briefed");
    set_activity(env, Some("idle"));
    env.reconcile().ok();
    assert_eq!(calls(env, "stop"), 0, "idle, but not for long enough yet");
    expire_idle(env, name);
    env.reconcile().ok();
}

#[test]
fn an_idle_task_is_paused_and_an_entry_resumes_it_after_a_catch_up_note() {
    let env = setup("pause-resume", vec![task("t1")]);
    pause(&env, "t1");

    assert_eq!(calls(&env, "stop"), 1);
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
    assert_eq!(calls(&env, "stop"), 1);
    assert_eq!(calls(&env, "start"), 0);
    assert_eq!(calls(&env, "restart"), 0);

    env.fleet(&["send", "t1", "next message"]).ok();
    env.fleet(&["send", "t1", "and another"]).ok();
    env.reconcile().ok();

    assert_eq!(calls(&env, "start"), 1, "one resume for both entries");
    assert!(!stopped(&env));
    let st = env.status("t1");
    assert_condition(&st, "Paused", "False", "Resumed");
    let texts = sent(&env);
    assert_eq!(texts.len(), 4, "{texts:#?}");
    assert_eq!(texts[0], "Do the work for t1.");
    assert!(texts[1].contains("paused"), "{}", texts[1]);
    assert!(texts[1].contains("Do the work for t1."), "{}", texts[1]);
    assert_eq!(texts[2], "next message");
    assert_eq!(texts[3], "and another");

    // Nothing is sent twice.
    env.reconcile().ok();
    assert_eq!(sent(&env).len(), 4);
    assert_eq!(calls(&env, "start"), 1);
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
    assert_eq!(calls(&env, "stop"), 0);
}

#[test]
fn a_task_without_a_pause_setting_is_never_paused() {
    let env = setup("pause-absent", vec![plain_task("t1")]);
    env.reconcile().ok();
    set_activity(&env, Some("idle"));
    for _ in 0..3 {
        env.reconcile().ok();
    }
    assert_eq!(calls(&env, "stop"), 0);
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
    assert_eq!(calls(&env, "stop"), 1);
    let st = env.status("t1");
    assert_eq!(st["phase"], "Concluded");
    assert_condition(&st, "Paused", "True", "Idle");

    env.fleet(&["send", "t1", "Review: rename the flag."]).ok();
    env.reconcile().ok();
    assert_eq!(calls(&env, "start"), 1);
    let texts = sent(&env);
    assert_eq!(texts.len(), 3, "{texts:#?}");
    assert!(texts[1].contains("already recorded"), "{}", texts[1]);
    assert_eq!(texts[2], "Review: rename the flag.");
}

#[test]
fn a_session_stopped_outside_fleet_resumes_on_the_next_entry() {
    // Before, a stopped session only read as not ready: the entry waited in
    // the inbox for good, and nothing said why.
    let env = setup("pause-external", vec![plain_task("t1")]);
    env.reconcile().ok();
    env.edit_stub(|s| {
        s["sessions"][0]["stopped"] = json!(true);
        s["sessions"][0]["state"] = json!("stopped");
    });
    env.reconcile().ok();
    assert_condition(&env.status("t1"), "Paused", "True", "External");
    assert_eq!(calls(&env, "start"), 0, "nothing to deliver yet");

    env.fleet(&["send", "t1", "are you there?"]).ok();
    env.reconcile().ok();
    assert_eq!(calls(&env, "start"), 1);
    let texts = sent(&env);
    assert_eq!(texts.len(), 3, "{texts:#?}");
    assert!(texts[1].contains("Do the work for t1."), "{}", texts[1]);
    assert_eq!(texts[2], "are you there?");
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
    assert_eq!(calls(&env, "start"), 0);
    assert!(stopped(&env));
}

#[test]
fn deleting_a_paused_task_releases_it() {
    let env = setup("pause-delete", vec![task("t1")]);
    pause(&env, "t1");
    assert!(stopped(&env));
    env.fleet(&["delete", "task", "t1"]).ok();
    env.reconcile().ok();
    assert_eq!(calls(&env, "start"), 0, "a deleted Task is not resumed");
    assert!(env.stub_sessions().is_empty(), "{:#?}", env.stub_sessions());
    env.fleet(&["get", "task", "t1"]).code(3);
}

#[test]
fn a_pause_whose_answer_was_lost_is_read_back_and_never_repeated() {
    let env = setup("pause-lost-answer", vec![task("t1")]);
    // The stop happens, and its answer never comes back.
    env.stub_fault("stop", "hang-after", 1);
    pause(&env, "t1");
    assert!(stopped(&env));
    env.reconcile().ok();
    assert_eq!(calls(&env, "stop"), 1, "read back, never repeated");
    assert_condition(&env.status("t1"), "Paused", "True", "Idle");
}

#[test]
fn a_resume_whose_answer_was_lost_is_read_back_and_never_repeated() {
    let env = setup("resume-lost-answer", vec![task("t1")]);
    pause(&env, "t1");
    env.stub_fault("start", "hang-after", 1);
    env.fleet(&["send", "t1", "next message"]).ok();
    env.reconcile().ok();
    env.reconcile().ok();
    assert_eq!(calls(&env, "start"), 1, "read back, never repeated");
    assert_condition(&env.status("t1"), "Paused", "False", "Resumed");
    let texts = sent(&env);
    assert_eq!(texts.len(), 3, "{texts:#?}");
    assert_eq!(texts[2], "next message");
}
