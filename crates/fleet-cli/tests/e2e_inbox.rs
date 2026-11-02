//! The Task inbox end to end: `fleet send` through the real binary, the
//! file store's typed mutations, TaskController's delivery and the runtime's
//! journal, against the fake host and the stub `thurbox-cli`, across
//! process restarts and injected faults.

mod support;

use std::fs;
use std::thread;
use std::time::Duration;

use fleet_runtime::Fault;
use serde_json::{Value, json};
use support::{Env, assert_condition, base_config, task};

/// A Task briefed on the fake host, sent by a known user.
fn briefed(name: &str) -> Env {
    let mut env = Env::new(name);
    env.set_var("USER", "tester");
    env.apply(base_config()).ok();
    env.apply(task("t1", true)).ok();
    env.reconcile().ok();
    assert_eq!(env.status("t1")["phase"], "Briefed");
    env
}

fn concluded(name: &str) -> Env {
    let env = briefed(name);
    env.fleet(&[
        "result",
        "t1",
        "--generation",
        "1",
        "--outcome",
        "shipped",
        "--output",
        "prUrl=\"https://example.test/pr/1\"",
    ])
    .ok();
    env.reconcile().ok();
    assert_eq!(env.status("t1")["phase"], "Concluded");
    env
}

/// `fleet send --json`: the entry as appended, and its key.
fn send(env: &Env, text: &str) -> Value {
    env.fleet(&["send", "t1", text, "--json"]).ok().json()
}

fn full_status(env: &Env) -> Value {
    env.get("t1")["status"].clone()
}

fn inbox_keys(status: &Value) -> Vec<String> {
    status["inbox"]
        .as_array()
        .map(|a| {
            a.iter()
                .map(|e| e["key"].as_str().unwrap().to_string())
                .collect()
        })
        .unwrap_or_default()
}

fn delivered_keys(status: &Value) -> Vec<String> {
    status["deliveries"]["receipts"]
        .as_array()
        .map(|a| {
            a.iter()
                .map(|e| e["key"].as_str().unwrap().to_string())
                .collect()
        })
        .unwrap_or_default()
}

fn briefs(env: &Env) -> Vec<String> {
    env.host()
        .session(&env.session_key("t1"))
        .unwrap()
        .expect("a session")
        .briefs
}

#[test]
fn a_message_to_a_briefed_task_is_delivered_once_under_its_key() {
    let env = briefed("inbox-briefed");
    let uid = env.get("t1")["metadata"]["uid"]
        .as_str()
        .unwrap()
        .to_string();
    let entry = send(&env, "Please also update the docs.");
    let key = entry["key"].as_str().unwrap().to_string();
    assert!(
        key.starts_with(&format!("task/t1/{uid}/send/")),
        "{entry:#}"
    );
    assert_eq!(entry["from"], "cli/tester");
    assert_eq!(entry["text"], "Please also update the docs.");
    assert!(entry.get("about").is_none(), "a send has no about");
    assert!(entry["queuedAt"].is_u64());
    assert_eq!(inbox_keys(&full_status(&env)), vec![key.clone()]);
    env.fleet(&["status", "task", "t1"])
        .ok()
        .stdout_has("inbox 1 queued, 0 delivered");

    let report = env.reconcile().ok().json();
    assert_eq!(report["idle"], true, "{report:#}");
    let st = full_status(&env);
    assert!(inbox_keys(&st).is_empty(), "{st:#}");
    assert_eq!(delivered_keys(&st), vec![key.clone()]);
    assert_eq!(st["deliveries"]["receipts"][0]["from"], "cli/tester");
    assert_eq!(st["deliveries"]["receipts"][0]["recovered"], false);
    assert_eq!(
        briefs(&env),
        vec![format!("task/t1/{uid}/brief"), key.clone()],
        "the session got the brief, then the message, each once"
    );
    assert_eq!(env.calls("brief"), 2);
    assert!(
        st["intents"].get(&key).is_none() && st["receipts"].get(&key).is_none(),
        "the effect records left with the entry: {st:#}"
    );
    assert_eq!(st["phase"], "Briefed");
    assert_condition(&env.status("t1"), "Briefed", "True", "ReceiptWritten");
    env.fleet(&["status", "task", "t1"])
        .ok()
        .stdout_has("inbox 0 queued, 1 delivered");

    // A second reconcile, a new process, changes nothing.
    let again = env.reconcile().ok().json();
    assert_eq!(
        (again["statusWrites"].as_u64(), again["effects"].as_u64()),
        (Some(0), Some(0)),
        "{again:#}"
    );
    // A second message gets its own key and goes out behind the first.
    let second = send(&env, "And one more thing.");
    assert_ne!(second["key"], key);
    env.reconcile().ok();
    assert_eq!(delivered_keys(&full_status(&env)).len(), 2);
    assert_eq!(briefs(&env).len(), 3);
    env.fleet(&["send", "t1", "   "])
        .code(1)
        .stderr_has("nothing to send");
    env.assert_no_temp();
}

#[test]
fn a_message_to_a_concluded_task_is_delivered_and_concludes_nothing_else() {
    let env = concluded("inbox-concluded");
    let before = env.status("t1")["conditions"].clone();
    let entry = send(&env, "Thanks; one follow-up question.");
    let key = entry["key"].as_str().unwrap().to_string();
    env.reconcile().ok();
    let st = full_status(&env);
    assert_eq!(st["phase"], "Concluded");
    assert_eq!(delivered_keys(&st), vec![key.clone()]);
    assert!(inbox_keys(&st).is_empty());
    assert!(briefs(&env).contains(&key));
    assert_eq!(
        env.status("t1")["conditions"],
        before,
        "delivery in Concluded changes no condition"
    );
    assert_eq!(env.calls("spawn"), 1, "never respawned");
    // At rest again: a Concluded Task with an empty inbox is not due.
    let looked_up = env.calls("find");
    let again = env.reconcile().ok().json();
    assert_eq!(
        (again["statusWrites"].as_u64(), again["idle"].as_bool()),
        (Some(0), Some(true)),
        "{again:#}"
    );
    assert_eq!(env.calls("find"), looked_up, "not even looked up");
}

#[test]
fn a_duplicate_key_is_refused() {
    let env = briefed("inbox-duplicate");
    // The key `fleet send` will compute next is the Task's resourceVersion;
    // an entry already holding it, however it got there, is refused.
    let path = env.object_path("Task", "t1");
    let mut stored: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    let rv = stored["metadata"]["resourceVersion"].as_u64().unwrap();
    let uid = stored["metadata"]["uid"].as_str().unwrap().to_string();
    let key = format!("task/t1/{uid}/send/{rv}");
    stored["status"]["inbox"] = json!([{
        "key": key, "from": "cli/earlier", "text": "already here", "queuedAt": 1
    }]);
    fs::write(&path, serde_json::to_vec(&stored).unwrap()).unwrap();
    env.fleet(&["send", "t1", "again"])
        .code(1)
        .stderr_has("already exists");
    let st = full_status(&env);
    assert_eq!(inbox_keys(&st), vec![key.clone()], "nothing appended");
    // Delivered, the key stays taken.
    env.reconcile().ok();
    assert_eq!(delivered_keys(&full_status(&env)), vec![key.clone()]);
    let mut stored: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    let rv = stored["metadata"]["resourceVersion"].as_u64().unwrap();
    // Make the next computed key collide with the receipted one.
    stored["status"]["deliveries"]["receipts"][0]["key"] =
        json!(format!("task/t1/{uid}/send/{rv}"));
    fs::write(&path, serde_json::to_vec(&stored).unwrap()).unwrap();
    env.fleet(&["send", "t1", "again"])
        .code(1)
        .stderr_has("already exists");
}

#[test]
fn the_inbox_is_bounded_and_drains_one_entry_per_pass() {
    let env = briefed("inbox-bound");
    let key = env.session_key("t1");
    env.host().set_ready(&key, false).unwrap();
    let bound = fleet_api::inbox::MAX_INBOX;
    for i in 0..=bound {
        send(&env, &format!("message {i}"));
    }
    assert_eq!(inbox_keys(&full_status(&env)).len(), bound + 1);
    env.fleet(&["send", "t1", "one too many"])
        .code(1)
        .stderr_has("undelivered");
    env.reconcile().ok();
    assert_condition(&env.status("t1"), "NeedsAttention", "True", "InboxFull");
    assert_eq!(env.calls("brief"), 1, "not ready: nothing typed");

    env.host().set_ready(&key, true).unwrap();
    let report = env
        .fleet(&["reconcile", "--json", "--max-passes", "100"])
        .ok()
        .json();
    assert_eq!(report["idle"], true, "{report:#}");
    let st = full_status(&env);
    assert!(inbox_keys(&st).is_empty(), "{st:#}");
    assert_eq!(delivered_keys(&st).len(), bound + 1);
    assert_eq!(briefs(&env).len(), bound + 2, "each once, in order");
    assert_eq!(
        &briefs(&env)[1..],
        delivered_keys(&st).as_slice(),
        "oldest first"
    );
    assert_condition(&env.status("t1"), "NeedsAttention", "False", "Resolved");
    env.fleet(&["send", "t1", "room again"]).ok();
}

#[test]
fn a_lost_delivery_answer_is_recovered_from_the_journal_without_a_resend() {
    let env = briefed("inbox-answer-lost");
    env.fault(Fault::new("brief", "unknown").performed());
    let entry = send(&env, "Did this arrive?");
    env.reconcile().ok();
    let st = full_status(&env);
    assert_eq!(delivered_keys(&st), vec![entry["key"].as_str().unwrap()]);
    assert_eq!(st["deliveries"]["receipts"][0]["recovered"], true);
    assert_eq!(env.calls("brief"), 2, "asked the journal, never resent");
    assert_eq!(briefs(&env).len(), 2);

    // A request lost before it arrived: the journal says no, so it is sent
    // again under the same key, and the session sees it once.
    env.fault(Fault::new("brief", "unknown"));
    send(&env, "Second.");
    env.reconcile().ok();
    assert_eq!(delivered_keys(&full_status(&env)).len(), 2);
    assert_eq!(briefs(&env).len(), 3);
    assert_eq!(env.calls("brief"), 4);
}

#[test]
fn three_refused_sends_raise_attention_and_a_later_entry_still_goes_out() {
    let env = briefed("inbox-refused");
    env.fault(Fault::new("brief", "fail").times(3));
    let first = send(&env, "first");
    let second = send(&env, "second");
    env.reconcile().ok();
    assert_eq!(env.calls("brief"), 2, "one attempt, then a persisted wait");
    env.reconcile().ok();
    assert_eq!(env.calls("brief"), 2, "a new manager honours the wait");
    env.expire_wait("t1");
    env.reconcile().ok();
    assert_eq!(env.calls("brief"), 3);
    // The third refusal spends the last send: the entry is given up on in
    // the same run, and the next entry goes out behind it.
    env.expire_wait("t1");
    env.reconcile().ok();
    let st = env.status("t1");
    assert_condition(&st, "NeedsAttention", "True", "DeliveryFailed");
    let message = support::condition(&st, "NeedsAttention")["message"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(
        message.contains(first["key"].as_str().unwrap()),
        "{message}"
    );
    assert_eq!(st["phase"], "Briefed", "a failed delivery is not a stall");
    let full = full_status(&env);
    assert_eq!(
        inbox_keys(&full),
        vec![first["key"].as_str().unwrap()],
        "the failed entry stays, the later one went out"
    );
    assert_eq!(delivered_keys(&full), vec![second["key"].as_str().unwrap()]);
    assert_eq!(
        env.calls("brief"),
        5,
        "three for the first, one for the second"
    );
    // Nothing more to do: the next run writes nothing.
    let again = env.reconcile().ok().json();
    assert_eq!(again["statusWrites"], 0, "{again:#}");
}

#[test]
fn a_gone_session_is_worker_gone_and_never_respawned() {
    let env = concluded("inbox-gone");
    env.host().vanish(&env.session_key("t1")).unwrap();
    send(&env, "Anyone there?");
    env.reconcile().ok();
    let st = env.status("t1");
    assert_eq!(st["phase"], "Concluded", "{st:#}");
    assert_condition(&st, "WorkerGone", "True", "SessionGone");
    assert_condition(&st, "NeedsAttention", "True", "WorkerGone");
    assert_eq!(env.calls("spawn"), 1, "never respawned");
    assert_eq!(env.calls("brief"), 1, "nothing typed into nobody");
    assert_eq!(inbox_keys(&full_status(&env)).len(), 1);
    let again = env.reconcile().ok().json();
    assert_eq!(again["statusWrites"], 0, "{again:#}");

    // An unreadable list is not absence: no WorkerGone from it, and
    // nothing typed into a session whose readiness cannot be read either.
    let env = concluded("inbox-list-unknown");
    env.host().vanish(&env.session_key("t1")).unwrap();
    env.fault(Fault::new("find", "unknown").times(100));
    send(&env, "Anyone there?");
    env.reconcile().ok();
    let st = env.status("t1");
    assert!(
        st["conditions"]
            .as_array()
            .unwrap()
            .iter()
            .all(|c| c["type"] != "WorkerGone"),
        "{st:#}"
    );
    assert_eq!(env.calls("brief"), 1);
}

#[test]
fn nothing_is_delivered_to_a_stalled_or_deleting_task() {
    let mut env = Env::new("inbox-stalled");
    env.set_var("USER", "tester");
    env.apply(task("t1", true)).ok();
    env.fault(Fault::new("spawn", "fail").times(10));
    env.reconcile().ok();
    for _ in 0..2 {
        env.expire_wait("t1");
        env.reconcile().ok();
    }
    assert_eq!(env.status("t1")["phase"], "Stalled");
    send(&env, "Hello?");
    env.expire_wait("t1");
    env.reconcile().ok();
    assert_eq!(env.calls("brief"), 0);
    assert_eq!(inbox_keys(&full_status(&env)).len(), 1);
    assert_eq!(env.status("t1")["phase"], "Stalled");

    let env = briefed("inbox-deleting");
    send(&env, "Before the delete.");
    env.fleet(&["delete", "task", "t1"]).ok();
    env.fleet(&["send", "t1", "After the delete."])
        .code(1)
        .stderr_has("being deleted");
    env.reconcile().ok();
    let st = env.status("t1");
    assert_eq!(st["phase"], "Releasing", "{st:#}");
    assert_eq!(env.calls("brief"), 1, "deletion keeps precedence");
    assert_eq!(inbox_keys(&full_status(&env)).len(), 1);
    env.host()
        .set_at_rest(&env.session_key("t1"), true)
        .unwrap();
    env.reconcile().ok();
    env.fleet(&["get", "task", "t1"]).code(3);
    assert_eq!(env.calls("brief"), 1);
}

#[test]
fn a_manager_crash_between_send_and_delivery_is_recovered_without_a_resend() {
    let env = briefed("inbox-crash");
    let entry = send(&env, "Survive the crash.");
    env.fault(Fault::new("brief", "abort"));
    env.fleet(&["reconcile", "--lease-ttl-ms", "3000"])
        .crashed();
    let raw = full_status(&env);
    assert!(
        raw["intents"].get(entry["key"].as_str().unwrap()).is_some(),
        "the intent was written before the effect: {raw:#}"
    );
    assert_eq!(inbox_keys(&raw).len(), 1, "still queued: no receipt yet");
    assert_eq!(briefs(&env).len(), 2, "the effect happened");
    // The dead holder's lease must expire before the next manager can take it.
    thread::sleep(Duration::from_millis(3_100));
    env.reconcile().ok();
    let st = full_status(&env);
    assert_eq!(delivered_keys(&st), vec![entry["key"].as_str().unwrap()]);
    assert_eq!(st["deliveries"]["receipts"][0]["recovered"], true);
    assert!(inbox_keys(&st).is_empty());
    assert_eq!(briefs(&env).len(), 2, "delivered once");
    assert_eq!(
        env.calls("brief"),
        2,
        "asked the session instead of resending"
    );
    env.assert_no_temp();
}

// --- The same, against thurbox-cli. ----------------------------------------

/// A Task whose workspace is an absolute path, as thurbox requires.
fn thurbox_task(name: &str) -> Value {
    let mut t = task(name, true);
    let dir = if cfg!(windows) {
        format!("C:\\work\\{name}")
    } else {
        format!("/work/{name}")
    };
    t["spec"]["workspace"]["dir"] = json!(dir);
    t
}

/// Every message Fleet gave a session, on the agent's own inbox.
fn sends(env: &Env) -> Vec<Vec<String>> {
    env.stub_calls()
        .iter()
        .filter(|c| support::is_call(c, "message send"))
        .map(|c| {
            c["argv"]
                .as_array()
                .unwrap()
                .iter()
                .map(|a| a.as_str().unwrap().to_string())
                .collect()
        })
        .collect()
}

#[test]
fn on_thurbox_cli_a_message_is_sent_once_and_journaled_under_its_key() {
    let mut env = Env::thurbox("inbox-thurbox");
    env.set_var("USER", "tester");
    env.apply(thurbox_task("t1")).ok();
    env.reconcile().ok();
    assert_eq!(env.status("t1")["phase"], "Briefed");
    // The brief left the session working; it is ready again now.
    env.edit_stub(|s| s["sessions"][0]["state"] = json!("idle"));

    let entry = send(&env, "- also run the linter");
    let key = entry["key"].as_str().unwrap().to_string();
    env.reconcile().ok();
    let session = &env.stub_sessions()[0];
    assert_eq!(
        session["sent"],
        json!(["Do the work for t1.", "- also run the linter"]),
        "sent once, as text even when it starts with a dash"
    );
    let journal: Value =
        serde_json::from_str(session["meta"]["fleet.brief"].as_str().unwrap()).unwrap();
    assert_eq!(journal[&key], "delivered", "{journal:#}");
    assert_eq!(delivered_keys(&full_status(&env)), vec![key.clone()]);
    assert_eq!(sends(&env).len(), 2);

    // The delivered mark landed but its answer was lost: the next pass reads
    // the journal and recovers the receipt without typing again.
    env.edit_stub(|s| s["sessions"][0]["state"] = json!("idle"));
    env.stub_fault_after("meta-set", "hang-after", 1, 1);
    let second = send(&env, "Second message.");
    env.reconcile().ok();
    let st = full_status(&env);
    assert_eq!(
        delivered_keys(&st),
        vec![key.clone(), second["key"].as_str().unwrap().to_string()]
    );
    assert_eq!(st["deliveries"]["receipts"][1]["recovered"], true);
    assert_eq!(sends(&env).len(), 3, "never sent twice");

    // A send that never answered may have enqueued the text: never
    // repeated; after three unreadable journal reads a person is asked.
    env.edit_stub(|s| s["sessions"][0]["state"] = json!("idle"));
    env.stub_fault("message-send", "hang-after", 1);
    let third = send(&env, "Third message.");
    env.reconcile().ok();
    for _ in 0..3 {
        env.expire_wait("t1");
        env.reconcile().ok();
    }
    let st = env.status("t1");
    assert_condition(&st, "NeedsAttention", "True", "DeliveryUnconfirmed");
    assert_eq!(
        sends(&env).len(),
        4,
        "never a second attempt at the same text"
    );
    assert_eq!(
        inbox_keys(&full_status(&env)),
        vec![third["key"].as_str().unwrap()],
        "it stays queued until the journal answers"
    );
}
