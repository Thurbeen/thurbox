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

#[test]
fn an_earlier_report_of_the_same_text_proves_no_later_send() {
    let env = setup("proof-repeat");
    env.reconcile().ok();
    assert_eq!(env.status("t1")["phase"], "Briefed");
    env.fleet(&["send", "t1", "same text"]).ok();
    env.reconcile().ok();
    assert_eq!(delivered_keys(&env).len(), 1, "{:#}", full_status(&env));

    // The same text again, lost before it reaches the agent: the earlier
    // report is not this send's.
    env.host().set_prompt_hook_silent(true).unwrap();
    env.fault(Fault::new("brief", "unknown"));
    env.fleet(&["send", "t1", "same text"]).ok();
    env.reconcile().ok();
    env.reconcile().ok();
    assert_eq!(delivered_keys(&env).len(), 1, "{:#}", full_status(&env));
}

#[test]
fn a_typed_entry_the_agent_reports_as_another_prompt_holds_back_no_later_entry() {
    let env = setup("proof-inbox-mangled");
    env.reconcile().ok();
    assert_eq!(env.status("t1")["phase"], "Briefed");
    env.host().set_prompt_hook_silent(true).unwrap();
    env.fleet(&["send", "t1", "first\nline"]).ok();
    env.reconcile().ok();
    prompt_hook(&env, "firstline");
    env.host().set_prompt_hook_silent(false).unwrap();
    env.fleet(&["send", "t1", "second"]).ok();
    env.reconcile().ok();
    env.reconcile().ok();
    env.reconcile().ok();
    let st = env.status("t1");
    assert_condition(&st, "NeedsAttention", "True", "DeliveryUnconfirmed");
    assert_eq!(
        env.calls("brief"),
        3,
        "the brief, one send of each entry, never a second of the first"
    );
    let full = full_status(&env);
    assert_eq!(
        queued_texts(&full),
        vec!["first\nline"],
        "the unproven entry is kept"
    );
    assert_eq!(delivered_keys(&env).len(), 1, "{full:#}");
}

#[test]
fn a_typed_entry_waits_for_its_report_however_long_the_turn_runs() {
    let env = setup("proof-inbox-long-turn");
    env.reconcile().ok();
    assert_eq!(env.status("t1")["phase"], "Briefed");
    env.host().set_prompt_hook_silent(true).unwrap();
    env.fleet(&["send", "t1", "first"]).ok();
    env.reconcile().ok();
    env.fleet(&["send", "t1", "second"]).ok();
    env.expire_receipts("t1");
    env.reconcile().ok();
    env.reconcile().ok();
    assert_eq!(env.calls("brief"), 2, "the second waits behind the first");
    assert!(delivered_keys(&env).is_empty(), "{:#}", full_status(&env));

    prompt_hook(&env, "first");
    env.host().set_prompt_hook_silent(false).unwrap();
    env.reconcile().ok();
    env.reconcile().ok();
    env.reconcile().ok();
    let full = full_status(&env);
    assert_eq!(delivered_keys(&env).len(), 2, "{full:#}");
    assert!(queued_texts(&full).is_empty(), "{full:#}");
    assert_eq!(env.calls("brief"), 3);
}

fn queued_texts(status: &Value) -> Vec<&str> {
    status["inbox"]
        .as_array()
        .map(|a| a.iter().map(|e| e["text"].as_str().unwrap()).collect())
        .unwrap_or_default()
}

#[test]
fn a_report_stamped_ahead_by_a_fast_host_clock_fails_no_later_send() {
    let env = setup("proof-inbox-clock-ahead");
    env.reconcile().ok();
    assert_eq!(env.status("t1")["phase"], "Briefed");
    let an_hour_ahead = support::now_ms() + 3_600_000;
    let earlier_report =
        json!({"at": an_hour_ahead, "state": "prompt", "prompts": ["0123456789abcdef"]});
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(signal_file(&env))
        .expect("signal file");
    std::io::Write::write_all(&mut file, format!("{earlier_report}\n").as_bytes()).unwrap();
    env.host().set_prompt_hook_silent(true).unwrap();
    env.fleet(&["send", "t1", "first"]).ok();
    env.reconcile().ok();
    env.reconcile().ok();
    env.reconcile().ok();
    let full = full_status(&env);
    assert_eq!(queued_texts(&full), vec!["first"], "{full:#}");
    assert!(
        full["deliveryAttempts"]
            .as_object()
            .is_some_and(|a| a.values().all(|a| a["failed"] != true)),
        "a report made before the send is no evidence against it: {full:#}"
    );
}

#[test]
fn a_mangled_report_of_a_text_reported_before_holds_back_no_later_entry() {
    let env = setup("proof-inbox-mangled-again");
    env.reconcile().ok();
    assert_eq!(env.status("t1")["phase"], "Briefed");
    env.fleet(&["send", "t1", "firstline"]).ok();
    env.reconcile().ok();
    env.reconcile().ok();
    assert_eq!(delivered_keys(&env).len(), 1, "{:#}", full_status(&env));

    env.host().set_prompt_hook_silent(true).unwrap();
    env.fleet(&["send", "t1", "first\nline"]).ok();
    env.reconcile().ok();
    prompt_hook(&env, "firstline");
    env.host().set_prompt_hook_silent(false).unwrap();
    env.fleet(&["send", "t1", "second"]).ok();
    env.reconcile().ok();
    env.reconcile().ok();
    env.reconcile().ok();
    let st = env.status("t1");
    assert_condition(&st, "NeedsAttention", "True", "DeliveryUnconfirmed");
    let full = full_status(&env);
    assert_eq!(queued_texts(&full), vec!["first\nline"], "{full:#}");
    assert_eq!(delivered_keys(&env).len(), 2, "{full:#}");
}

#[test]
fn an_entry_an_older_fleet_typed_is_released_by_the_next_other_prompt() {
    let env = setup("proof-inbox-legacy");
    env.reconcile().ok();
    assert_eq!(env.status("t1")["phase"], "Briefed");
    env.host().set_prompt_hook_silent(true).unwrap();
    env.fleet(&["send", "t1", "first"]).ok();
    env.reconcile().ok();
    env.forget_prompt_boundaries("t1");
    env.fleet(&["send", "t1", "second"]).ok();
    env.reconcile().ok();
    assert_eq!(env.calls("brief"), 2, "the second waits behind the first");

    prompt_hook(&env, "a prompt typed by a person");
    env.host().set_prompt_hook_silent(false).unwrap();
    env.reconcile().ok();
    env.reconcile().ok();
    env.reconcile().ok();
    let full = full_status(&env);
    assert_eq!(queued_texts(&full), vec!["first"], "{full:#}");
    assert_eq!(delivered_keys(&env).len(), 1, "{full:#}");
    assert_eq!(env.calls("brief"), 3, "never a second send of the first");
}

#[test]
fn a_report_after_the_hosts_clock_stepped_back_still_releases_a_mangled_entry() {
    let env = setup("proof-inbox-clock-back");
    env.reconcile().ok();
    assert_eq!(env.status("t1")["phase"], "Briefed");
    let file = signal_file(&env);
    let offset = std::fs::metadata(&file).expect("signal file").len();
    let an_hour_ahead = support::now_ms() + 3_600_000;
    let before_the_step = json!({"at": an_hour_ahead, "state": "prompt", "prompts": ["0123456789abcdef"], "seq": offset});
    let mut appended = std::fs::OpenOptions::new()
        .append(true)
        .open(&file)
        .expect("signal file");
    std::io::Write::write_all(&mut appended, format!("{before_the_step}\n").as_bytes()).unwrap();

    env.host().set_prompt_hook_silent(true).unwrap();
    env.fleet(&["send", "t1", "first\nline"]).ok();
    env.reconcile().ok();
    prompt_hook(&env, "firstline");
    env.host().set_prompt_hook_silent(false).unwrap();
    env.fleet(&["send", "t1", "second"]).ok();
    env.reconcile().ok();
    env.reconcile().ok();
    env.reconcile().ok();
    let st = env.status("t1");
    assert_condition(&st, "NeedsAttention", "True", "DeliveryUnconfirmed");
    let full = full_status(&env);
    assert_eq!(queued_texts(&full), vec!["first\nline"], "{full:#}");
    assert_eq!(delivered_keys(&env).len(), 1, "{full:#}");
}

#[test]
fn the_first_report_a_session_ever_makes_can_release_a_mangled_entry() {
    let env = setup("proof-inbox-first-report");
    env.host().set_prompt_hook_silent(true).unwrap();
    env.reconcile().ok();
    env.fleet(&["send", "t1", "first\nline"]).ok();
    env.reconcile().ok();
    env.reconcile().ok();
    assert!(
        std::fs::read_to_string(signal_file(&env)).unwrap_or_default().is_empty(),
        "nothing reported before the entry"
    );
    prompt_hook(&env, "firstline");
    env.host().set_prompt_hook_silent(false).unwrap();
    env.fleet(&["send", "t1", "second"]).ok();
    env.reconcile().ok();
    env.reconcile().ok();
    env.reconcile().ok();
    let st = env.status("t1");
    assert_condition(&st, "NeedsAttention", "True", "DeliveryUnconfirmed");
    let full = full_status(&env);
    assert_eq!(queued_texts(&full), vec!["first\nline"], "{full:#}");
}
