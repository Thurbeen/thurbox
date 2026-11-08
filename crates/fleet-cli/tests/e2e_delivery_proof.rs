//! A brief or an inbox entry counts as delivered only when the agent's
//! prompt hook reports it through `fleet signal`, not when its runtime typed
//! it. The real `fleet` binary, its file store and the fake host, whose
//! agents report every text they are given unless a test silences them.

mod support;

use fleet_runtime::Fault;
use serde_json::{Value, json};
use support::{Env, assert_condition, wait};

const BRIEF: &str = "Do the work for t1.";

fn setup(name: &str) -> Env {
    let env = Env::new(name);
    env.apply(json!([support::task("t1", true)])).ok();
    env
}

fn signal_file(env: &Env) -> String {
    env.host()
        .session(&env.session_key("t1"))
        .unwrap()
        .expect("a session")
        .env
        .get("FLEET_SIGNAL_FILE")
        .cloned()
        .expect("FLEET_SIGNAL_FILE")
}

/// The agent's prompt hook, reporting `text` as Claude Code gives it.
fn prompt_hook(env: &Env, text: &str) {
    let payload = json!({"prompt": text, "session_id": "conv-1"}).to_string();
    let mut cmd = env.command(&["signal", "prompt", &payload]);
    cmd.env("FLEET_SIGNAL_FILE", signal_file(env));
    wait(cmd.spawn().expect("fleet signal")).ok();
}

fn full_status(env: &Env) -> Value {
    env.get("t1")["status"].clone()
}

fn delivered_keys(env: &Env) -> Vec<Value> {
    full_status(env)["deliveries"]["receipts"]
        .as_array()
        .cloned()
        .unwrap_or_default()
}

#[test]
fn a_brief_is_delivered_only_once_the_agents_prompt_hook_reports_it() {
    let env = setup("proof-brief");
    env.host().set_prompt_hook_silent(true).unwrap();
    env.reconcile().ok();
    // The runtime typed it; nothing proves the agent took it.
    let st = env.status("t1");
    assert_eq!(st["phase"], "Spawned", "{st:#}");
    assert_condition(&st, "Briefed", "Unknown", "AwaitingPrompt");
    env.reconcile().ok();
    assert_eq!(env.calls("brief"), 1, "never typed twice");

    prompt_hook(&env, BRIEF);
    env.reconcile().ok();
    let st = env.status("t1");
    assert_eq!(st["phase"], "Briefed", "{st:#}");
    assert_eq!(env.calls("brief"), 1);
}

#[test]
fn a_brief_whose_answer_was_lost_is_settled_by_the_prompt_hook_and_sent_once() {
    let env = setup("proof-lost");
    // The brief lands and the agent reports it; the runtime's answer is lost.
    env.fault(Fault::new("brief", "unknown").performed());
    env.reconcile().ok();
    let full = full_status(&env);
    assert_eq!(full["phase"], "Briefed", "{full:#}");
    assert_eq!(full["brief"]["recovered"], true, "{full:#}");
    assert_eq!(env.calls("brief"), 1);
    assert_eq!(env.calls("briefDelivered"), 0, "the runtime is never asked");
}

#[test]
fn a_lost_answer_the_agent_never_reports_is_never_sent_again() {
    let env = setup("proof-lost-silent");
    env.host().set_prompt_hook_silent(true).unwrap();
    env.fault(Fault::new("brief", "unknown"));
    env.reconcile().ok();
    env.reconcile().ok();
    let st = env.status("t1");
    assert_eq!(st["phase"], "Spawned", "{st:#}");
    assert_condition(&st, "Briefed", "Unknown", "BriefUnconfirmed");
    assert_eq!(
        env.calls("brief"),
        1,
        "Unknown never concludes, never resends"
    );
}

#[test]
fn an_inbox_entry_is_delivered_only_once_the_agent_reports_it() {
    let env = setup("proof-inbox");
    env.reconcile().ok();
    assert_eq!(env.status("t1")["phase"], "Briefed");
    env.host().set_prompt_hook_silent(true).unwrap();
    env.fleet(&["send", "t1", "next message"]).ok();
    env.reconcile().ok();
    assert_eq!(env.calls("brief"), 2, "typed once");
    assert!(delivered_keys(&env).is_empty(), "{:#}", full_status(&env));

    prompt_hook(&env, "next message");
    env.reconcile().ok();
    assert_eq!(delivered_keys(&env).len(), 1, "{:#}", full_status(&env));
    assert_eq!(env.calls("brief"), 2);
}
