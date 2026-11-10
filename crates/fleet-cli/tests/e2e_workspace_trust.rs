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
