//! `fleet amend` end to end: an amendment appended to a Task's spec through
//! the real binary, delivered to the same session through TaskController's
//! inbox protocol, and the result rules that tie a result to what was
//! added, against the fake host, across restarts and injected faults.

mod support;

use std::thread;
use std::time::Duration;

use fleet_runtime::Fault;
use serde_json::{Value, json};
use support::{Env, assert_condition, base_config, task};

fn fresh(name: &str) -> Env {
    let mut env = Env::new(name);
    env.set_var("USER", "tester");
    env.apply(base_config()).ok();
    env
}

/// A Task briefed on the fake host at generation 1.
fn briefed(name: &str) -> Env {
    let env = fresh(name);
    env.apply(task("t1", true)).ok();
    env.reconcile().ok();
    assert_eq!(env.status("t1")["phase"], "Briefed");
    env
}

fn concluded(name: &str) -> Env {
    let env = briefed(name);
    env.fleet(&result_args("1")).ok();
    env.reconcile().ok();
    assert_eq!(env.status("t1")["phase"], "Concluded");
    env
}

fn result_args(generation: &str) -> Vec<&str> {
    vec![
        "result",
        "t1",
        "--generation",
        generation,
        "--outcome",
        "shipped",
        "--output",
        "prUrl=\"https://example.test/pr/1\"",
    ]
}

fn full(env: &Env) -> Value {
    env.get("t1")
}

fn uid(env: &Env) -> String {
    full(env)["metadata"]["uid"].as_str().unwrap().to_string()
}

fn session(env: &Env) -> fleet_runtime::fake::FakeSession {
    env.host()
        .session(&env.session_key("t1"))
        .unwrap()
        .expect("a session")
}

fn keys(list: &Value) -> Vec<String> {
    list.as_array()
        .map(|a| {
            a.iter()
                .map(|e| e["key"].as_str().unwrap().to_string())
                .collect()
        })
        .unwrap_or_default()
}

fn brief_generation(env: &Env) -> u64 {
    full(env)["status"]["brief"]["generation"].as_u64().unwrap()
}

#[test]
fn an_amendment_to_a_briefed_task_is_appended_and_delivered_to_the_same_session() {
    let env = briefed("amend-briefed");
    let uid = uid(&env);
    env.fleet(&[
        "amend",
        "t1",
        "--text",
        "Also cover the docs.",
        "--reason",
        "the user asked: docs too",
    ])
    .ok()
    .stdout_has("Task/t1 amended: amendment 1 appended, generation 1 → 2");
    let obj = full(&env);
    assert_eq!(obj["metadata"]["generation"], 2);
    assert_eq!(
        obj["spec"]["brief"], "Do the work for t1.",
        "the brief stays"
    );
    let amendment = &obj["spec"]["amendments"][0];
    assert_eq!(amendment["text"], "Also cover the docs.");
    assert_eq!(amendment["reason"], "the user asked: docs too");
    assert_eq!(amendment["from"], "cli/tester");
    assert_eq!(amendment["generation"], 2);
    assert!(amendment["at"].is_u64());
    let key = format!("task/t1/{uid}/amend/2");
    let inbox = &obj["status"]["inbox"];
    assert_eq!(keys(inbox), vec![key.clone()]);
    assert_eq!(inbox[0]["amends"], 2);

    // A result is refused while the amendment waits, for either generation.
    env.fleet(&result_args("1")).code(1).stderr_has("amendment");
    env.fleet(&result_args("2")).code(1);

    env.reconcile().ok();
    let obj = full(&env);
    let st = &obj["status"];
    assert_eq!(st["brief"]["generation"], 2, "{st:#}");
    assert_eq!(st["brief"]["key"], key);
    assert!(keys(&st["inbox"]).is_empty());
    assert_eq!(keys(&st["deliveries"]["receipts"]), vec![key.clone()]);
    let s = session(&env);
    assert_eq!(s.briefs, vec![format!("task/t1/{uid}/brief"), key.clone()]);
    let text = s.texts.last().unwrap();
    for needle in [
        "an amendment to your brief (generation 2) from cli/tester",
        "Reason: the user asked: docs too",
        "fleet result t1 --generation 2",
        "Also cover the docs.",
    ] {
        assert!(text.contains(needle), "{needle:?} not in {text}");
    }
    assert_eq!(env.calls("spawn"), 1, "the same worker");
    assert_eq!(st["phase"], "Briefed");

    env.fleet(&result_args("1"))
        .code(1)
        .stderr_has("stale generation 1");
    env.fleet(&result_args("2")).ok();
    env.reconcile().ok();
    assert_eq!(env.status("t1")["phase"], "Concluded");
    env.assert_no_temp();
}

#[test]
fn an_amendment_waits_while_the_worker_is_busy() {
    let env = briefed("amend-busy");
    let key = env.session_key("t1");
    env.host().set_ready(&key, false).unwrap();
    env.fleet(&["amend", "t1", "--text", "One more ask."]).ok();
    let sent = env.calls("brief");
    env.reconcile().ok();
    assert_eq!(env.calls("brief"), sent, "nothing typed mid-turn");
    assert_eq!(brief_generation(&env), 1);
    assert_eq!(keys(&full(&env)["status"]["inbox"]).len(), 1);
    env.fleet(&result_args("1")).code(1).stderr_has("amendment");

    env.host().set_ready(&key, true).unwrap();
    env.reconcile().ok();
    assert_eq!(env.calls("brief"), sent + 1);
    assert_eq!(brief_generation(&env), 2);
}

#[test]
fn an_amendment_before_the_brief_goes_out_with_the_brief() {
    let env = fresh("amend-early");
    env.apply(task("t1", false)).ok();
    env.fleet(&["amend", "t1", "--file", "-"]).code(1);
    env.fleet_stdin(&["amend", "t1", "--file", "-"], "Start with the tests.")
        .ok();
    // Flipping dispatch with the original manifest keeps the amendment.
    env.apply(task("t1", true))
        .ok()
        .stdout_has("Task/t1 configured");
    let obj = full(&env);
    assert_eq!(obj["spec"]["amendments"].as_array().unwrap().len(), 1);
    assert_eq!(obj["metadata"]["generation"], 3);
    env.reconcile().ok();
    let st = full(&env)["status"].clone();
    assert_eq!(st["phase"], "Briefed");
    assert_eq!(st["brief"]["generation"], 3, "{st:#}");
    let s = session(&env);
    assert_eq!(s.briefs.len(), 1, "one message: {:?}", s.briefs);
    let text = &s.texts[0];
    assert!(text.starts_with("Do the work for t1."), "{text}");
    assert!(text.contains("Start with the tests."), "{text}");
    assert!(keys(&st["inbox"]).is_empty(), "recorded without a send");
    assert_eq!(keys(&st["deliveries"]["receipts"]).len(), 1);
    env.fleet(&result_args("3")).ok();
}

#[test]
fn an_amendment_while_the_brief_is_in_flight_goes_out_after_it() {
    let env = fresh("amend-in-flight");
    env.apply(task("t1", true)).ok();
    env.fault(Fault::new("brief", "abort"));
    env.fleet(&["reconcile", "--lease-ttl-ms", "3000"])
        .crashed();
    assert_eq!(session(&env).briefs.len(), 1, "the brief went out");
    env.fleet(&["amend", "t1", "--text", "Added while in flight."])
        .ok();
    thread::sleep(Duration::from_millis(3_100));
    env.reconcile().ok();
    let s = session(&env);
    assert_eq!(s.briefs.len(), 2, "the amendment followed: {:?}", s.briefs);
    assert!(s.texts[1].contains("Added while in flight."));
    let st = full(&env)["status"].clone();
    assert_eq!(st["brief"]["generation"], 2, "{st:#}");
    assert!(st["brief"]["key"].as_str().unwrap().ends_with("/amend/2"));
}

#[test]
fn an_amendment_reopens_a_concluded_task() {
    let env = concluded("amend-concluded");
    env.fleet(&["amend", "t1", "--text", "Please also answer: why X?"])
        .ok();
    env.reconcile().ok();
    let st = env.status("t1");
    assert_eq!(st["phase"], "Briefed", "{st:#}");
    assert_condition(&st, "Concluded", "False", "Amended");
    let raw = full(&env)["status"].clone();
    assert_eq!(raw["result"]["generation"], 1, "the old result stays");
    assert_eq!(raw["brief"]["generation"], 2);
    env.reconcile().ok();
    assert_eq!(env.status("t1")["phase"], "Briefed", "a stale result");
    env.fleet(&result_args("2")).ok();
    env.reconcile().ok();
    let st = env.status("t1");
    assert_eq!(st["phase"], "Concluded");
    assert_condition(&st, "Concluded", "True", "ResultRecorded");
    // Within one generation a result stays write-once.
    let mut other = result_args("2");
    other[5] = "stuck";
    env.fleet(&other).code(1).stderr_has("already recorded");
    assert_eq!(env.calls("spawn"), 1);
}

#[test]
fn if_generation_is_an_optional_precondition() {
    let env = briefed("amend-precondition");
    env.fleet(&["amend", "t1", "--text", "first", "--if-generation", "1"])
        .ok();
    env.fleet(&["amend", "t1", "--text", "late", "--if-generation", "1"])
        .code(5)
        .stderr_has("Task/t1 is at generation 2, not 1");
    let obj = full(&env);
    assert_eq!(obj["spec"]["amendments"].as_array().unwrap().len(), 1);
    // Without one, an amend lands on the latest state; two identical
    // amendments are both kept.
    let out = env
        .fleet(&["amend", "t1", "--text", "first", "--json"])
        .ok()
        .json();
    assert_eq!(out["generation"], 3, "{out:#}");
    assert_eq!(out["amendment"], 2, "{out:#}");
    let obj = full(&env);
    assert_eq!(obj["spec"]["amendments"].as_array().unwrap().len(), 2);
    assert_eq!(keys(&obj["status"]["inbox"]).len(), 2);

    // Only the newest is sent, and it carries both.
    env.reconcile().ok();
    let s = session(&env);
    assert_eq!(s.briefs.len(), 2, "{:?}", s.briefs);
    let text = s.texts.last().unwrap();
    assert_eq!(text.matches("first").count(), 2, "{text}");
    let st = full(&env)["status"].clone();
    assert_eq!(st["brief"]["generation"], 3);
    assert!(keys(&st["inbox"]).is_empty());
    assert_eq!(keys(&st["deliveries"]["receipts"]).len(), 2);
}

#[test]
fn an_amend_is_refused_with_nothing_to_deliver_or_no_one_to_deliver_it() {
    let env = briefed("amend-refusals");
    env.fleet(&["amend", "t1", "--text", "  "])
        .code(1)
        .stderr_has("empty");
    env.fleet(&["amend", "nope", "--text", "x"]).code(3);
    // A full inbox refuses the amend itself.
    let key = env.session_key("t1");
    env.host().set_ready(&key, false).unwrap();
    for i in 0..=fleet_api::inbox::MAX_INBOX {
        env.fleet(&["send", "t1", &format!("m{i}")]).ok();
    }
    env.fleet(&["amend", "t1", "--text", "x"])
        .code(1)
        .stderr_has("inbox holds");
    assert!(full(&env)["spec"].get("amendments").is_none());

    // Deleting.
    env.host().set_at_rest(&key, false).unwrap();
    env.fleet(&["delete", "task", "t1"]).ok();
    env.fleet(&["amend", "t1", "--text", "x"])
        .code(1)
        .stderr_has("being deleted");

    // Stalled.
    let env = fresh("amend-stalled");
    env.apply(task("t2", true)).ok();
    env.fault(Fault::new("spawn", "fail").times(10));
    env.reconcile().ok();
    for _ in 0..2 {
        env.expire_wait("t2");
        env.reconcile().ok();
    }
    assert_eq!(env.status("t2")["phase"], "Stalled");
    env.fleet(&["amend", "t2", "--text", "x"])
        .code(1)
        .stderr_has("Stalled");
}

#[test]
fn an_amendment_may_add_outputs_but_never_redeclare_one() {
    let env = briefed("amend-outputs");
    env.fleet(&["amend", "t1", "--text", "x", "--output", "prUrl=string"])
        .code(1)
        .stderr_has("already declared");
    env.fleet(&["amend", "t1", "--text", "x", "--output", "docs=bogus"])
        .code(1);
    env.fleet(&[
        "amend",
        "t1",
        "--text",
        "Link the docs page.",
        "--output",
        "docsUrl=string!",
    ])
    .ok();
    env.fleet(&["amend", "t1", "--text", "y", "--output", "docsUrl=integer"])
        .code(1)
        .stderr_has("already declared");
    env.reconcile().ok();
    let text = session(&env).texts.last().unwrap().clone();
    assert!(text.contains("docsUrl: string, required"), "{text}");
    env.fleet(&result_args("2"))
        .code(1)
        .stderr_has("\"docsUrl\" is required");
    let mut with = result_args("2");
    with.extend(["--output", "docsUrl=\"https://example.test/docs\""]);
    env.fleet(&with).ok();
}

#[test]
fn apply_keeps_amendments_and_refuses_to_change_them() {
    let env = briefed("amend-apply");
    env.fleet(&["amend", "t1", "--text", "kept"]).ok();
    env.apply(task("t1", true))
        .ok()
        .stdout_has("Task/t1 unchanged");
    let mut rewritten = task("t1", true);
    rewritten["spec"]["amendments"] = json!([{
        "text": "rewritten", "from": "cli/tester", "at": 1, "generation": 2
    }]);
    env.apply(rewritten).code(1).stderr_has("fleet amend");
    let mut brief = task("t1", true);
    brief["spec"]["brief"] = json!("A different brief.");
    env.apply(brief)
        .code(1)
        .stderr_has("immutable except spec.dispatch");
    assert_eq!(full(&env)["spec"]["amendments"][0]["text"], "kept");
}

#[test]
fn a_lost_amendment_send_is_recovered_from_the_journal() {
    let env = briefed("amend-journal");
    env.fault(Fault::new("brief", "unknown").performed());
    env.fleet(&["amend", "t1", "--text", "Did it arrive?"]).ok();
    env.reconcile().ok();
    let st = full(&env)["status"].clone();
    assert_eq!(st["brief"]["generation"], 2, "{st:#}");
    assert_eq!(st["brief"]["recovered"], true);
    assert_eq!(session(&env).briefs.len(), 2, "delivered once");
}
