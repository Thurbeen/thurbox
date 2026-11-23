//! Workspace trust at agent startup, end to end: the real `fleet` binary,
//! its file store and TaskController against the fake host, whose agents
//! either trust their workspace or wait for a person to.

mod support;

use fleet_runtime::Fault;
use serde_json::Value;
use support::{Env, assert_condition, task};

fn condition(status: &Value, type_: &str) -> Value {
    status["conditions"]
        .as_array()
        .and_then(|cs| cs.iter().find(|c| c["type"] == type_))
        .cloned()
        .unwrap_or(Value::Null)
}

/// Every new agent waits for a person to trust its workspace.
fn untrusting(name: &str) -> Env {
    let env = Env::new(name);
    env.host().update(|s| s.spawn_untrusted = true).unwrap();
    env
}

/// A person trusts the workspace once; the agent passes its dialog.
fn trust(env: &Env, task: &str) {
    let key = env.session_key(task);
    env.host()
        .update(|s| s.sessions.get_mut(&key).unwrap().trusted = true)
        .unwrap();
}

#[test]
fn an_agent_waiting_for_trust_is_never_briefed_and_asks_for_it_once() {
    let env = untrusting("trust-untrusted");
    env.apply(task("t1", true)).ok();
    env.reconcile().ok();

    let st = env.status("t1");
    assert_eq!(st["phase"], "Spawned", "{st:#}");
    assert_eq!(env.calls("brief"), 0, "never a keystroke into a dialog");
    assert_condition(&st, "WorkspaceTrusted", "False", "Unsupported");
    assert_condition(&st, "NeedsAttention", "True", "WorkspaceUntrusted");

    // Looked at again after the retry wait, not in a hot loop.
    assert!(env.get("t1")["status"]["notBefore"].is_u64());
    env.reconcile().ok();
    assert_eq!(env.calls("brief"), 0);

    // The same Task now gets its brief, without a respawn.
    trust(&env, "t1");
    env.expire_wait("t1");
    env.reconcile().ok();
    let st = env.status("t1");
    assert_eq!(st["phase"], "Briefed", "{st:#}");
    assert_condition(&st, "WorkspaceTrusted", "True", "Trusted");
    assert_condition(&st, "NeedsAttention", "False", "Resolved");
    assert_eq!((env.calls("spawn"), env.calls("brief")), (1, 1));
}

#[test]
fn trust_that_cannot_be_read_is_unknown_never_a_brief_and_recovers() {
    let env = untrusting("trust-unknown");
    env.fault(Fault::new("workspaceTrust", "unknown").times(100));
    env.apply(task("u1", true)).ok();
    env.reconcile().ok();
    let st = env.status("u1");
    assert_eq!(env.calls("brief"), 0);
    assert_condition(&st, "WorkspaceTrusted", "Unknown", "TrustUnknown");
    assert!(
        env.get("u1")["status"]["notBefore"].is_null(),
        "Unknown is probed again on the next pass, without a retry wait"
    );
    assert_ne!(
        condition(&st, "NeedsAttention")["reason"],
        "WorkspaceUntrusted",
        "Unknown is not a known refusal: {st:#}"
    );

    trust(&env, "u1");
    env.host().update(|s| s.faults.clear()).unwrap();
    env.reconcile().ok();
    assert_eq!(env.status("u1")["phase"], "Briefed");
    assert_eq!(env.calls("brief"), 1);
}

fn backdate_condition(env: &Env, task: &str, type_: &str, by_ms: u64) {
    let path = env.object_path("Task", task);
    let mut obj: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    let c = obj["status"]["conditions"]
        .as_array_mut()
        .and_then(|cs| cs.iter_mut().find(|c| c["type"] == type_))
        .unwrap();
    let at = c["lastTransitionTime"].as_u64().unwrap();
    c["lastTransitionTime"] = at.saturating_sub(by_ms).into();
    std::fs::write(&path, obj.to_string()).unwrap();
}

#[test]
fn trust_unknown_past_a_minute_names_the_missing_hook_report_and_fleet_doctor() {
    let env = untrusting("trust-unknown-long");
    env.fault(Fault::new("workspaceTrust", "unknown").times(100));
    env.apply(task("u2", true)).ok();
    env.reconcile().ok();
    let fresh = condition(&env.status("u2"), "WorkspaceTrusted")["message"].clone();
    assert!(
        !fresh.as_str().unwrap_or_default().contains("fleet doctor"),
        "a starting agent gets no hint yet: {fresh}"
    );

    backdate_condition(&env, "u2", "WorkspaceTrusted", 61_000);
    env.reconcile().ok();
    let st = env.status("u2");
    assert_condition(&st, "WorkspaceTrusted", "Unknown", "TrustUnknown");
    let message = condition(&st, "WorkspaceTrusted")["message"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    assert!(
        message.contains("no hook report yet for harness claude-code")
            && message.contains("check `fleet doctor` for its integration"),
        "{message}"
    );
}
