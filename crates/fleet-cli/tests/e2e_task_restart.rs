mod support;

use fleet_runtime::{FakeSession, Fault};
use serde_json::{Value, json};
use support::{Env, assert_condition};

fn setup(name: &str) -> Env {
    let env = Env::new(name);
    env.apply(json!([support::task("t1", true)])).ok();
    env.reconcile().ok();
    assert_eq!(env.status("t1")["phase"], "Briefed");
    env
}

fn session(env: &Env) -> FakeSession {
    env.host()
        .session(&env.session_key("t1"))
        .unwrap()
        .expect("a session")
}

fn conversation(env: &Env) -> Value {
    env.get("t1")["status"]["conversation"].clone()
}

#[test]
fn fleet_restart_starts_the_agent_again_on_its_conversation_and_types_nothing() {
    let env = setup("restart-resume");
    let before = session(&env);
    let conv = format!("conv-{}", before.id);
    assert_eq!(conversation(&env), json!(conv));

    env.fleet(&["restart", "t1"])
        .ok()
        .stdout_has("Task/t1 restart requested");
    assert_eq!(env.calls("restart"), 0, "the verb only asks");
    env.reconcile().ok();

    assert_eq!(env.calls("restart"), 1);
    let after = session(&env);
    assert_eq!(after.id, before.id, "the same session");
    assert!(!after.paused);
    let started = after.restarts.last().expect("restarted");
    assert_eq!(
        started[started.len() - 2..],
        ["--resume".to_string(), conv.clone()]
    );
    let st = env.status("t1");
    assert_eq!(st["phase"], "Briefed", "{st:#}");
    assert_condition(&st, "Restarted", "True", "Resumed");
    assert_eq!(env.get("t1")["status"]["restarts"], 1);
    assert_eq!(conversation(&env), json!(conv));

    for _ in 0..3 {
        env.reconcile().ok();
    }
    assert_eq!(env.calls("restart"), 1, "a request is honoured once");
    assert_eq!(session(&env).texts, ["Do the work for t1."]);
}

#[test]
fn fleet_restart_fresh_starts_a_new_conversation_briefs_it_again_then_delivers_the_inbox() {
    let env = setup("restart-fresh");
    let before = session(&env);
    let old = format!("conv-{}", before.id);
    env.fleet(&[
        "declare",
        "t1",
        "--json",
        r#"{"kind":"Question","id":"q1","text":"Which branch?"}"#,
    ])
    .ok();
    env.host().set_ready(&env.session_key("t1"), false).unwrap();
    env.fleet(&["send", "t1", "queued while busy"]).ok();
    env.reconcile().ok();
    assert_eq!(session(&env).texts.len(), 1, "the agent was busy");

    env.fleet(&["restart", "t1", "--fresh"])
        .ok()
        .stdout_has("Task/t1 fresh restart requested");
    env.reconcile().ok();

    assert_eq!(env.calls("restart"), 1);
    let after = session(&env);
    assert_eq!(
        after.restarts.last().expect("restarted"),
        &before.argv,
        "the launch argv, resuming nothing"
    );
    assert_condition(&env.status("t1"), "Restarted", "True", "Fresh");
    for _ in 0..3 {
        env.reconcile().ok();
    }
    let texts = session(&env).texts;
    assert_eq!(texts.len(), 3, "{texts:#?}");
    assert!(
        texts[1].contains("new conversation") && texts[1].contains("Do the work for t1."),
        "the catch-up note briefs it again: {}",
        texts[1]
    );
    assert!(
        texts[1].contains("q1") && texts[1].contains("Which branch?"),
        "it names the open Question: {}",
        texts[1]
    );
    assert_eq!(texts[2], "queued while busy");
    let conv = conversation(&env);
    assert!(conv.is_string(), "the new conversation is recorded");
    assert_ne!(conv, json!(old), "never the old one");
    assert_eq!(env.status("t1")["phase"], "Briefed");
}

#[test]
fn fleet_restart_starts_a_paused_task_again() {
    let env = setup("restart-paused");
    env.fleet(&["pause", "t1"]).ok();
    env.reconcile().ok();
    assert!(session(&env).paused);

    env.fleet(&["restart", "t1"]).ok();
    env.reconcile().ok();
    env.reconcile().ok();
    assert_eq!(env.calls("restart"), 1);
    assert!(!session(&env).paused);
    let st = env.status("t1");
    assert_condition(&st, "Restarted", "True", "Resumed");
    assert_condition(&st, "Paused", "False", "Resumed");
}

#[test]
fn fleet_restart_refuses_a_held_task_and_writes_nothing() {
    let env = setup("restart-held");
    let mut held = support::task("t1", true);
    held["spec"]["dispatch"] = json!(false);
    env.apply(json!([held])).ok();
    env.fleet(&["restart", "t1"]).code(1).stderr_has("dispatch");
    assert!(env.get("t1")["status"]["restartRequested"].is_null());
    env.reconcile().ok();
    assert_eq!(env.calls("restart"), 0);
}

#[test]
fn fleet_restart_refuses_a_task_with_no_session_yet() {
    let env = Env::new("restart-pending");
    env.apply(json!([support::task("t1", true)])).ok();
    env.fleet(&["restart", "t1"])
        .code(1)
        .stderr_has("no session");
    assert!(env.get("t1")["status"]["restartRequested"].is_null());
}

#[test]
fn fleet_restart_refuses_a_task_being_deleted() {
    let env = setup("restart-deleting");
    env.fleet(&["delete", "task", "t1"]).ok();
    env.fleet(&["restart", "t1"])
        .code(1)
        .stderr_has("being deleted");
}

#[test]
fn a_runtime_that_cannot_pause_restarts_nothing() {
    let env = setup("restart-unsupported");
    env.host().update(|s| s.pause_unsupported = true).unwrap();
    env.fleet(&["restart", "t1"]).ok();
    env.reconcile().ok();
    let st = env.status("t1");
    assert_condition(&st, "Restarted", "False", "Unsupported");
    assert!(session(&env).restarts.is_empty(), "nothing stopped");
    env.reconcile().ok();
    assert_eq!(env.calls("restart"), 1, "a request is honoured once");
}

#[test]
fn a_failed_restart_says_so_and_is_not_repeated() {
    let env = setup("restart-failed");
    env.fault(Fault::new("restart", "fail"));
    env.fleet(&["restart", "t1"]).ok();
    env.reconcile().ok();
    env.reconcile().ok();
    assert_eq!(env.calls("restart"), 1);
    assert_condition(&env.status("t1"), "Restarted", "False", "RestartFailed");
}

#[test]
fn fleet_restart_names_a_missing_task() {
    let env = setup("restart-missing");
    env.fleet(&["restart", "nope"])
        .code(3)
        .stderr_has("Task/nope");
}

#[test]
fn fleet_restart_starts_an_interactive_task_again_and_briefs_nothing() {
    let env = Env::new("restart-interactive");
    let mut t1 = support::task("t1", true);
    t1["spec"].as_object_mut().unwrap().remove("brief");
    env.apply(json!([t1])).ok();
    env.reconcile().ok();
    let st = env.status("t1");
    assert_eq!(st["phase"], "Spawned", "{st:#}");
    assert_condition(&st, "Briefed", "False", "Interactive");
    let before = session(&env);

    env.fleet(&["restart", "t1", "--fresh"]).ok();
    env.reconcile().ok();

    assert_eq!(env.calls("restart"), 1);
    let after = session(&env);
    assert_eq!(after.id, before.id, "the same session");
    assert!(!after.paused);
    let st = env.status("t1");
    assert_eq!(st["phase"], "Spawned", "{st:#}");
    assert_condition(&st, "Restarted", "True", "Fresh");
    for _ in 0..3 {
        env.reconcile().ok();
    }
    assert_eq!(env.calls("restart"), 1, "a request is honoured once");
    assert!(session(&env).texts.is_empty(), "nothing to brief again");
}
