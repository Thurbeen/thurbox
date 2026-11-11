//! Agents report their state through `fleet signal`: a stand-in agent's
//! hook runs `fleet signal` with the file Fleet passed it at start, and the
//! Task's session state and its pause follow. The real `fleet` binary, its
//! file store and the fake host, which reports no activity of its own.

mod support;

use serde_json::{Value, json};
use support::{Env, assert_condition, object, wait};

const PAUSE_AFTER_MS: u64 = 600_000;

fn setup(name: &str) -> Env {
    let env = Env::new(name);
    let mut t = support::task("t1", true);
    t["spec"]["configRef"] = json!({"name": "pausing"});
    env.apply(json!([
        object(
            "TaskConfig",
            "pausing",
            json!({"pauseAfterMs": PAUSE_AFTER_MS})
        ),
        t
    ]))
    .ok();
    env
}

/// The file Fleet passed the session at start.
fn signal_file(env: &Env) -> String {
    let session = env
        .host()
        .session(&env.session_key("t1"))
        .unwrap()
        .expect("a session");
    session
        .env
        .get("FLEET_SIGNAL_FILE")
        .cloned()
        .expect("FLEET_SIGNAL_FILE in the session's environment")
}

/// What the agent's hook runs: `fleet signal <args>`, with the session's
/// environment.
fn signal(env: &Env, file: &str, args: &[&str]) {
    let mut cmd = env.command(&[&["signal"], args].concat());
    cmd.env("FLEET_SIGNAL_FILE", file);
    wait(cmd.spawn().expect("fleet signal")).ok();
}

fn session_state(env: &Env) -> Value {
    env.get("t1")["status"]["sessionState"].clone()
}

fn expire_idle(env: &Env) {
    let path = env.object_path("Task", "t1");
    let mut obj: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert!(obj["status"]["idleSince"].is_u64(), "{:#}", obj["status"]);
    obj["status"]["idleSince"] = json!(0);
    std::fs::write(&path, obj.to_string()).unwrap();
}

#[test]
fn the_agents_hooks_drive_its_session_state_and_an_idle_one_pauses() {
    let env = setup("signal-state");
    env.reconcile().ok();
    assert_eq!(env.status("t1")["phase"], "Briefed");
    let file = signal_file(&env);
    // Nothing reported yet: the session is present, not idle.
    assert_eq!(session_state(&env), "Running");

    signal(&env, &file, &["working"]);
    env.reconcile().ok();
    assert_eq!(session_state(&env), "Running");
    assert!(env.get("t1")["status"]["idleSince"].is_null());

    signal(&env, &file, &["idle"]);
    env.reconcile().ok();
    assert_eq!(session_state(&env), "Idle");
    assert_eq!(env.calls("pause"), 0, "idle, but not for long enough yet");
    expire_idle(&env);
    env.reconcile().ok();
    assert_eq!(env.calls("pause"), 1);
    assert_condition(&env.status("t1"), "Paused", "True", "Idle");
}

#[test]
fn a_signal_file_fleet_cannot_read_concludes_nothing() {
    let env = setup("signal-unreadable");
    env.reconcile().ok();
    let file = signal_file(&env);
    signal(&env, &file, &["idle"]);
    env.reconcile().ok();
    assert_eq!(session_state(&env), "Idle");
    expire_idle(&env);
    // The file goes unreadable: a directory where it was.
    std::fs::remove_file(&file).unwrap();
    std::fs::create_dir(&file).unwrap();
    env.reconcile().ok();
    assert_eq!(env.calls("pause"), 0, "Unknown never pauses");
    assert_ne!(
        session_state(&env),
        "Idle",
        "an unread report proves no idleness"
    );
}

#[test]
fn a_signal_outside_a_fleet_session_does_nothing() {
    let env = Env::new("signal-outside");
    // No FLEET_SIGNAL_FILE: a hook installed for every session of the
    // harness may run where Fleet started nothing.
    env.fleet(&["signal", "idle"]).ok();
    env.fleet(&["signal", "busy"]).code(2);
}

#[test]
fn a_turn_that_ends_with_background_work_is_not_idle_and_never_pauses() {
    let env = setup("signal-background");
    env.reconcile().ok();
    let file = signal_file(&env);
    // Claude Code's Stop payload lists a shell still running.
    let stop = json!({"session_id": "c1", "background_tasks": [{"id": "b1", "type": "shell", "status": "running"}], "session_crons": []});
    signal(&env, &file, &["idle", &stop.to_string()]);
    env.reconcile().ok();
    assert_eq!(session_state(&env), "Running");
    assert!(env.get("t1")["status"]["idleSince"].is_null());
    // A cron that will wake it later is work too.
    let cron = json!({"background_tasks": [], "session_crons": [{"id": "k1"}]});
    signal(&env, &file, &["idle", &cron.to_string()]);
    env.reconcile().ok();
    assert_eq!(session_state(&env), "Running");
    assert_eq!(env.calls("pause"), 0);

    // Nothing left in flight: idle.
    let done = json!({"background_tasks": [], "session_crons": []});
    signal(&env, &file, &["idle", &done.to_string()]);
    env.reconcile().ok();
    assert_eq!(session_state(&env), "Idle");
}

/// An interactive session (no brief, no pause) shows what its agent's hooks
/// report: a person reads `sessionState` to know whether it works or waits.
#[test]
fn an_interactive_sessions_hooks_drive_its_session_state() {
    let env = Env::new("signal-interactive");
    let mut t = support::task("t1", true);
    t["spec"].as_object_mut().unwrap().remove("brief");
    t["spec"].as_object_mut().unwrap().remove("outputs");
    env.apply(json!([t])).ok();
    env.reconcile().ok();
    let file = signal_file(&env);
    assert_eq!(session_state(&env), "Running", "nothing reported yet");

    signal(&env, &file, &["idle"]);
    env.reconcile().ok();
    assert_eq!(session_state(&env), "Idle", "{:#}", env.get("t1")["status"]);

    signal(&env, &file, &["working"]);
    env.reconcile().ok();
    assert_eq!(session_state(&env), "Running");

    signal(&env, &file, &["idle"]);
    env.reconcile().ok();
    assert_eq!(session_state(&env), "Idle");
}

/// The agent's own report, finer than `sessionState`: `agentState` is
/// `unknown` until the agent reports, then follows its hooks. `prompt` and
/// `working` are a turn at work, `blocked` waits on a person (attention),
/// and `idle` is a finished turn.
#[test]
fn an_interactive_sessions_hooks_drive_its_agent_state() {
    let env = Env::new("signal-agent-state");
    let mut t = support::task("t1", true);
    t["spec"].as_object_mut().unwrap().remove("brief");
    t["spec"].as_object_mut().unwrap().remove("outputs");
    env.apply(json!([t])).ok();
    env.reconcile().ok();
    let file = signal_file(&env);
    let agent_state = |env: &Env| env.get("t1")["status"]["agentState"].clone();
    assert_eq!(agent_state(&env), "unknown", "nothing reported yet");
    for (word, state) in [
        ("working", "working"),
        ("blocked", "attention"),
        ("prompt", "working"),
        ("idle", "idle"),
    ] {
        signal(&env, &file, &[word]);
        env.reconcile().ok();
        assert_eq!(
            agent_state(&env),
            state,
            "after `fleet signal {word}`: {:#}",
            env.get("t1")["status"]
        );
    }
}
