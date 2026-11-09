//! The Gateway controller's third step end to end: a Task tied to a
//! Conversation joins with the whole retained thread, later admitted
//! messages are relayed into its inbox under a cursor that moves in the
//! same write, messages from people off the allowlist wait for the next
//! admitted one, Fleet's own posts are never relayed, `fleet send
//! --answers` records `status.answered`, the parent labels are enforced by
//! the store, and a child's declarations reach its parent's inbox. Task
//! creation from a mention is step 2 and posting is step 4: here the tie is
//! written into the records directly.

mod support;

use std::fs;

use fleet_api::gateway::{Author, AuthorKind, ConversationRef, Message};
use serde_json::{Value, json};
use support::{
    ALEX_ID, Env, GATEWAY_ACCOUNT, SAM_ID, assert_condition, conversation_name, message_event,
};

const T0: u64 = 1_730_000_000_000;
const THREAD: &str = "fake:C0123ABCD:1730000000000";
const BOT: &str = "B0FLEET001";
const STRANGER: &str = "U0999ZZZZ";

fn person(id: &str, at: u64, text: &str) -> Message {
    Message {
        id: at.to_string(),
        author: Author {
            id: id.into(),
            name: "Sam".into(),
            kind: AuthorKind::Person,
        },
        mentions_fleet: false,
        at,
        text: text.into(),
        key: None,
        starts_thread: false,
    }
}

fn mention(id: &str, at: u64) -> Value {
    message_event(
        THREAD,
        id,
        ALEX_ID,
        at,
        &format!("<@{BOT}> can you look into it?"),
    )
}

fn read_record(env: &Env, kind: &str, name: &str) -> Value {
    serde_json::from_slice(&fs::read(env.object_path(kind, name)).unwrap()).unwrap()
}

fn write_record(env: &Env, kind: &str, name: &str, obj: &Value) {
    fs::write(env.object_path(kind, name), obj.to_string()).unwrap();
}

/// A gateway environment with one mirrored thread (Sam's message, then
/// Alex's mention) and the Task that mention started, tied to it by step
/// 2's create. Returns the environment, the Conversation's name and the
/// Task's.
fn tied(name: &str) -> (Env, String, String) {
    let mut env = Env::gateway(name);
    env.set_var("USER", "tester");
    env.fake_gateway()
        .seed(
            &ConversationRef::split(THREAD).unwrap(),
            vec![person(SAM_ID, T0, "The nightly export failed again.")],
        )
        .unwrap();
    env.apply(support::object(
        "TaskConfig",
        "conversation",
        json!({"harness": "claude-code", "host": "local"}),
    ))
    .ok();
    let conv = conversation_name(GATEWAY_ACCOUNT, THREAD);
    env.receive("Ev01", mention("1730000300000", T0 + 300_000))
        .ok();
    env.reconcile().ok();
    let task_name = format!("{conv}-1");
    assert_eq!(
        env.conversation_status(&conv)["task"]["state"],
        "created",
        "step 2 started the Task"
    );
    (env, conv, task_name)
}

fn gateway(env: &Env, task: &str) -> Value {
    env.get(task)["status"]["gateway"].clone()
}

fn receipts(env: &Env, task: &str) -> Vec<Value> {
    env.get(task)["status"]["deliveries"]["receipts"]
        .as_array()
        .cloned()
        .unwrap_or_default()
}

/// The texts the fake session was given, the brief first.
fn texts(env: &Env, task: &str) -> Vec<String> {
    env.host()
        .session(&env.session_key(task))
        .unwrap()
        .expect("a session")
        .texts
}

fn uid_of(env: &Env, kind: &str, name: &str) -> String {
    env.fleet(&["get", kind, name]).ok().json()["metadata"]["uid"]
        .as_str()
        .unwrap()
        .to_string()
}

#[test]
fn a_tied_task_joins_with_the_whole_thread_and_the_relay_moves_the_cursor_in_one_write() {
    let (env, conv, task) = tied("join");
    let uid = uid_of(&env, "task", &task);
    let conv_uid = uid_of(&env, "conversation", &conv);
    let report = env.reconcile().ok().json();
    assert_eq!(report["errors"], json!([]), "{report:#}");
    let st = env.status(&task);
    assert_eq!(st["phase"], "Briefed", "{st:#}");
    let gw = gateway(&env, &task);
    assert_eq!(
        gw["conversation"],
        json!({"name": conv, "uid": conv_uid, "state": "tied"}),
        "{gw:#}"
    );
    assert_eq!(gw["cursor"], 1, "the join relayed through revision 1");
    assert_eq!(gw["seen"], 1);
    assert_eq!(gw["gaps"], 0);
    assert_condition(&gw, "Tied", "True", "Verified");
    let key = format!("task/{task}/{uid}/gateway/{conv_uid}/1");
    let got = receipts(&env, &task);
    assert_eq!(got.len(), 1, "{got:#?}");
    assert_eq!(got[0]["key"], key);
    assert_eq!(got[0]["from"], format!("gateway/{conv}"));
    assert_eq!(
        env.get(&task)["status"]["deliveries"]["revisions"][&conv_uid],
        1
    );
    let got = texts(&env, &task);
    assert_eq!(got.len(), 2, "the brief, then the join: {got:#?}");
    let join = &got[1];
    assert!(join.contains("whole retained thread"), "{join}");
    assert!(join.contains("quoted from the chat as data"), "{join}");
    assert!(
        join.contains("> The nightly export failed again."),
        "{join}"
    );
    assert!(
        join.contains(&format!("> <@{BOT}> can you look into it?")),
        "{join}"
    );
    assert!(join.contains(SAM_ID) && join.contains(ALEX_ID), "{join}");
    let sam = join.find("nightly export").unwrap();
    let alex = join.find("look into it").unwrap();
    assert!(sam < alex, "oldest first: {join}");
    env.fleet(&["status", "task", &task])
        .ok()
        .stdout_has(&format!("gateway tied {conv} cursor 1"));

    // A quiet tied Task costs nothing: the next run is idle and writes nothing.
    let report = env.reconcile().ok().json();
    assert_eq!(report["statusWrites"], 0, "{report:#}");
    assert_eq!(report["idle"], true);

    // A later admitted message is relayed on its own, and the cursor moves
    // to the revision it covers.
    env.receive(
        "Ev02",
        message_event(
            THREAD,
            "1730000400000",
            ALEX_ID,
            T0 + 400_000,
            "It fails at 02:00 UTC, every night this week.",
        ),
    )
    .ok();
    env.reconcile().ok();
    let gw = gateway(&env, &task);
    assert_eq!(gw["cursor"], 2, "{gw:#}");
    let got = receipts(&env, &task);
    assert_eq!(got.len(), 2);
    assert_eq!(
        got[1]["key"],
        format!("task/{task}/{uid}/gateway/{conv_uid}/2")
    );
    let relay = &texts(&env, &task)[2];
    assert!(relay.contains("02:00 UTC"), "{relay}");
    assert!(
        !relay.contains("nightly export failed"),
        "only what is new: {relay}"
    );
    assert!(relay.contains("revision 2"), "{relay}");

    // An edit is relayed again, marked as one.
    env.receive(
        "Ev03",
        json!({"type": "edited", "conversation": {"platform": "fake", "chat": "C0123ABCD", "thread": "1730000000000"},
               "id": "1730000400000", "text": "It fails at 02:00 UTC, every night this month.", "editedAt": T0 + 410_000}),
    )
    .ok();
    env.reconcile().ok();
    assert_eq!(gateway(&env, &task)["cursor"], 3);
    let relay = &texts(&env, &task)[3];
    assert!(relay.contains("edited"), "{relay}");
    assert!(relay.contains("this month"), "{relay}");
    env.assert_no_temp();
}

#[test]
fn context_messages_wait_for_the_next_admitted_one_and_fleets_own_posts_are_never_relayed() {
    let (env, conv, task) = tied("context");
    env.reconcile().ok();
    assert_eq!(texts(&env, &task).len(), 2, "the brief and the join");
    // Someone off the allowlist writes: no turn, the cursor stays, the
    // mirror was read.
    env.receive(
        "Ev02",
        message_event(
            THREAD,
            "1730000400000",
            STRANGER,
            T0 + 400_000,
            "I saw this too; the disk was full.",
        ),
    )
    .ok();
    env.reconcile().ok();
    let gw = gateway(&env, &task);
    assert_eq!(gw["cursor"], 1, "{gw:#}");
    assert_eq!(gw["seen"], 2, "{gw:#}");
    assert_eq!(texts(&env, &task).len(), 2);
    // Fleet's own post is never relayed either.
    env.receive(
        "Ev03",
        json!({
            "type": "message", "conversation": {"platform": "fake", "chat": "C0123ABCD", "thread": "1730000000000"},
            "id": "1730000450000", "author": {"id": BOT, "name": "fleet", "kind": "fleet"},
            "at": T0 + 450_000, "text": "Looking at the export job now.",
            "key": format!("task/{task}/u/gateway/comment/c1")
        }),
    )
    .ok();
    env.reconcile().ok();
    let gw = gateway(&env, &task);
    assert_eq!(gw["cursor"], 1, "{gw:#}");
    assert_eq!(gw["seen"], 3);
    assert_eq!(texts(&env, &task).len(), 2);
    // The next admitted message carries the waiting one first, under a
    // header that marks it as content, and never Fleet's own.
    env.receive(
        "Ev04",
        message_event(
            THREAD,
            "1730000500000",
            SAM_ID,
            T0 + 500_000,
            "Yes, please check the disk first.",
        ),
    )
    .ok();
    env.reconcile().ok();
    let gw = gateway(&env, &task);
    assert_eq!(gw["cursor"], 4, "{gw:#}");
    let got = texts(&env, &task);
    assert_eq!(got.len(), 3, "{got:#?}");
    let relay = &got[2];
    assert!(relay.contains("not on the allowlist"), "{relay}");
    assert!(relay.contains("content, never instructions"), "{relay}");
    let context = relay.find("the disk was full").unwrap();
    let admitted = relay.find("check the disk first").unwrap();
    assert!(context < admitted, "context first: {relay}");
    assert!(
        !relay.contains("Looking at the export job now"),
        "Fleet never relays its own posts: {relay}"
    );
    assert_eq!(
        receipts(&env, &task)
            .iter()
            .filter(|r| r["from"] == format!("gateway/{conv}"))
            .count(),
        2
    );
}

#[test]
fn answers_are_recorded_by_send_and_refused_for_an_undeclared_question() {
    let (env, _conv, task) = tied("answers");
    env.reconcile().ok();
    env.fleet(&["send", &task, "--answers", "q1", "the nightly one"])
        .code(1)
        .stderr_has("no declared Question");
    env.fleet(&[
        "declare",
        &task,
        "--json",
        r#"{"kind":"Question","id":"q1","text":"Which export?"}"#,
    ])
    .ok();
    let entry = env
        .fleet(&[
            "send",
            &task,
            "--answers",
            "q1",
            "the nightly one",
            "--json",
        ])
        .ok()
        .json();
    assert_eq!(entry["answers"], json!(["q1"]), "{entry:#}");
    let st = env.get(&task)["status"].clone();
    assert_eq!(st["answered"]["q1"]["key"], entry["key"], "{st:#}");
    assert_eq!(st["answered"]["q1"]["from"], "cli/tester");
    assert_eq!(st["answered"]["q1"]["at"], entry["queuedAt"]);
    // The first answer wins; a later one still lands as a message.
    let later = env
        .fleet(&["send", &task, "--answers", "q1", "or the weekly", "--json"])
        .ok()
        .json();
    let st = env.get(&task)["status"].clone();
    assert_eq!(st["answered"]["q1"]["key"], entry["key"]);
    assert_eq!(st["inbox"].as_array().unwrap().len(), 2);
    assert_eq!(st["inbox"][1]["key"], later["key"]);
    env.fleet(&["send", &task, "--answers", "q9", "nothing"])
        .code(1)
        .stderr_has("no declared Question");
    // The record survives delivery: nobody owns it, so no status write
    // moves it.
    env.reconcile().ok();
    let st = env.get(&task)["status"].clone();
    assert_eq!(st["answered"]["q1"]["key"], entry["key"], "{st:#}");
    assert!(st["inbox"].as_array().is_none_or(Vec::is_empty));
    env.fleet(&["status", "task", &task])
        .ok()
        .stdout_has("answered q1");
}

#[test]
fn a_deleted_conversation_raises_conversation_gone_and_the_task_keeps_running() {
    let (env, conv, task) = tied("gone");
    env.reconcile().ok();
    assert_eq!(gateway(&env, &task)["conversation"]["state"], "tied");
    env.fleet(&["delete", "conversation", &conv]).ok();
    env.reconcile().ok();
    let gw = gateway(&env, &task);
    assert!(gw["conversation"].is_null(), "{gw:#}");
    assert_condition(&gw, "Tied", "False", "ConversationGone");
    assert_condition(&gw, "NeedsAttention", "True", "ConversationGone");
    assert_eq!(gw["cursor"], 1, "what was relayed stays relayed");
    assert_eq!(env.status(&task)["phase"], "Briefed");
    // Stable: a later run writes nothing.
    let report = env.reconcile().ok().json();
    assert_eq!(report["statusWrites"], 0, "{report:#}");
}

#[test]
fn a_task_waits_for_its_conversation_to_name_it() {
    let (env, conv, task) = tied("waiting");
    let joined = texts(&env, &task).len();
    env.apply(support::task("other", false)).ok();
    let other_uid = uid_of(&env, "task", "other");
    // The Conversation names another Task: gone, not waiting.
    let mut obj = read_record(&env, "Conversation", &conv);
    obj["status"]["task"]["uid"] = json!(other_uid);
    obj["status"]["task"]["name"] = json!("other");
    // A tied Task looks again when its Conversation moves.
    obj["status"]["revision"] = json!(obj["status"]["revision"].as_u64().unwrap() + 1);
    write_record(&env, "Conversation", &conv, &obj);
    env.reconcile().ok();
    let gw = gateway(&env, &task);
    assert_condition(&gw, "Tied", "False", "ConversationGone");
    assert_eq!(texts(&env, &task).len(), joined, "nothing relayed");
    // No tie recorded on the Conversation: waiting.
    let mut obj = read_record(&env, "Conversation", &conv);
    obj["status"]["task"] = Value::Null;
    obj["status"]["phase"] = json!("Untied");
    // A tied Task looks again when its Conversation moves.
    obj["status"]["revision"] = json!(obj["status"]["revision"].as_u64().unwrap() + 1);
    write_record(&env, "Conversation", &conv, &obj);
    env.reconcile().ok();
    let gw = gateway(&env, &task);
    assert_eq!(gw["conversation"]["state"], "waiting", "{gw:#}");
    assert_condition(&gw, "Tied", "False", "Waiting");
    assert_condition(&gw, "NeedsAttention", "False", "Resolved");
    assert_eq!(texts(&env, &task).len(), joined);
    // Then it names this Task again: tied, under the cursor it kept.
    let uid = uid_of(&env, "task", &task);
    let mut obj = read_record(&env, "Conversation", &conv);
    obj["status"]["task"] = json!({"name": task, "uid": uid, "n": 1, "state": "created"});
    obj["status"]["phase"] = json!("Tied");
    // A tied Task looks again when its Conversation moves.
    obj["status"]["revision"] = json!(obj["status"]["revision"].as_u64().unwrap() + 1);
    write_record(&env, "Conversation", &conv, &obj);
    env.reconcile().ok();
    let gw = gateway(&env, &task);
    assert_eq!(gw["conversation"]["state"], "tied", "{gw:#}");
    assert_eq!(gw["cursor"], 1);
    assert_eq!(texts(&env, &task).len(), joined);
}

fn with_plugin(name: &str, tasks: &str) -> Env {
    let mut env = Env::new(name);
    env.set_var("USER", "tester");
    let mut cfg = fs::read_to_string(&env.config).unwrap();
    cfg.push_str(&format!(
        "\n[plugins.test]\nprogram = '{}'\nargs = ['plugin', '{}']\ncwd = '{}'\ncallTimeoutMs = 1000\ntasks = \"{tasks}\"\n",
        env!("CARGO_BIN_EXE_fleet-test-stub"),
        env.root.display(),
        env.root.display()
    ));
    fs::write(&env.config, cfg).unwrap();
    // Every verb registers the plugin, so its manifest must be there first.
    scenario(&env, json!({}));
    env
}

fn scenario(env: &Env, plan: Value) {
    fs::write(env.root.join("plugin.json"), serde_json::to_vec(&json!({
        "manifest":{"protocol":1,"name":"test","kinds":[{"kind":"Issue","pollMs":1000,"finalizer":false,"spec":{"type":"object","required":["number"],"additionalProperties":false,"properties":{"number":{"type":"integer"}}}}]},
        "plan":plan
    })).unwrap()).unwrap();
}

fn child_of(name: &str, parent: &str, parent_uid: &str, dispatch: bool) -> Value {
    let mut t = support::task(name, dispatch);
    t["metadata"]["labels"]["gateway.fleet.dev/parent"] = json!(parent);
    t["metadata"]["labels"]["gateway.fleet.dev/parent-uid"] = json!(parent_uid);
    t
}

#[test]
fn the_parent_labels_are_set_only_at_creation_by_a_writer_with_scope_over_the_parent() {
    let env = with_plugin("parent-labels", "own");
    env.apply(support::task("p", false)).ok();
    let p_uid = uid_of(&env, "task", "p");
    // One label without the other, a parent that does not exist, and a
    // parent with another uid are each refused.
    let mut half = support::task("c", false);
    half["metadata"]["labels"]["gateway.fleet.dev/parent"] = json!("p");
    env.apply(half)
        .code(1)
        .stderr_has("gateway.fleet.dev/parent-uid");
    env.apply(child_of("c", "nobody", &p_uid, false))
        .code(1)
        .stderr_has("Task/nobody");
    env.apply(child_of("c", "p", "u0-wrong", false))
        .code(1)
        .stderr_has("uid");
    // A parent the store cannot read is Unknown: retried, never refused.
    let path = env.object_path("Task", "p");
    let bytes = fs::read(&path).unwrap();
    fs::write(&path, &bytes[..bytes.len() / 2]).unwrap();
    env.apply(child_of("c", "p", &p_uid, false)).code(4);
    fs::write(&path, &bytes).unwrap();
    // A person may name any parent.
    env.apply(child_of("c", "p", &p_uid, false)).ok();
    // Once set, the labels never change: not removed, not pointed elsewhere.
    env.apply(support::task("c", false))
        .code(1)
        .stderr_has("gateway.fleet.dev/parent");
    env.apply(support::task("q", false)).ok();
    let q_uid = uid_of(&env, "task", "q");
    env.apply(child_of("c", "q", &q_uid, false))
        .code(1)
        .stderr_has("gateway.fleet.dev/parent");
    env.apply(child_of("c", "p", &p_uid, true)).ok();
    // A plugin whose Task scope is its own Tasks may not name a parent it
    // does not own: it could not append to that inbox either.
    env.apply(json!({"apiVersion":"fleet/v1alpha1","kind":"Issue","metadata":{"name":"issue-1"},"spec":{"number":1}}))
        .ok();
    scenario(
        &env,
        json!({"creates":[child_of("plugin-child", "p", &p_uid, false)]}),
    );
    env.reconcile().ok();
    let issue = env.fleet(&["get", "issue", "issue-1"]).ok().json();
    let verbs = issue["status"]["host"]["verbs"].clone();
    assert!(
        verbs[0]["error"]
            .as_str()
            .is_some_and(|e| e.contains("parent")),
        "{issue:#}"
    );
    env.fleet(&["get", "task", "plugin-child"]).code(3);
    // The same plugin may name a parent it created.
    scenario(
        &env,
        json!({"creates":[support::task("own-parent", false)]}),
    );
    env.expire_wait_of("Issue", "issue-1");
    env.reconcile().ok();
    let own_uid = uid_of(&env, "task", "own-parent");
    scenario(
        &env,
        json!({"creates":[child_of("plugin-child", "own-parent", &own_uid, false)]}),
    );
    env.expire_wait_of("Issue", "issue-1");
    env.reconcile().ok();
    env.fleet(&["get", "task", "plugin-child"]).ok();
    // A plugin over any Task may name any parent.
    let env = with_plugin("parent-labels-any", "any");
    env.apply(support::task("p", false)).ok();
    let p_uid = uid_of(&env, "task", "p");
    env.apply(json!({"apiVersion":"fleet/v1alpha1","kind":"Issue","metadata":{"name":"issue-1"},"spec":{"number":1}}))
        .ok();
    scenario(
        &env,
        json!({"creates":[child_of("plugin-child", "p", &p_uid, false)]}),
    );
    env.reconcile().ok();
    let child = env.fleet(&["get", "task", "plugin-child"]).ok().json();
    assert_eq!(child["metadata"]["labels"]["gateway.fleet.dev/parent"], "p");
}

#[test]
fn a_child_relays_its_declarations_and_result_into_its_parents_inbox() {
    let mut env = Env::new("child");
    env.set_var("USER", "tester");
    env.apply(support::task("p", true)).ok();
    env.reconcile().ok();
    assert_eq!(env.status("p")["phase"], "Briefed");
    let p_uid = uid_of(&env, "task", "p");
    env.apply(child_of("c", "p", &p_uid, true)).ok();
    env.reconcile().ok();
    assert_eq!(env.status("c")["phase"], "Briefed");
    let c_uid = uid_of(&env, "task", "c");
    assert_eq!(
        texts(&env, "p").len(),
        1,
        "a child that said nothing sends nothing"
    );
    env.fleet(&[
        "declare",
        "c",
        "--json",
        r#"{"kind":"Comment","id":"c1","text":"I started on the export job."}"#,
    ])
    .ok();
    env.fleet(&[
        "declare",
        "c",
        "--json",
        r#"{"kind":"Note","text":"ignored"}"#,
    ])
    .ok();
    env.fleet(&[
        "declare",
        "c",
        "--json",
        r#"{"kind":"Question","id":"q1","text":"May I restart the exporter?"}"#,
    ])
    .ok();
    let report = env.reconcile().ok().json();
    assert_eq!(report["errors"], json!([]), "{report:#}");
    let got = receipts(&env, "p");
    assert_eq!(got.len(), 2, "{got:#?}");
    assert_eq!(
        got[0]["key"],
        format!("task/p/{p_uid}/gateway/child/{c_uid}/0")
    );
    assert_eq!(got[0]["from"], "gateway/child/c");
    assert_eq!(
        got[1]["key"],
        format!("task/p/{p_uid}/gateway/child/{c_uid}/2")
    );
    let got = texts(&env, "p");
    assert_eq!(got.len(), 3, "{got:#?}");
    assert!(
        got[1].contains("Comment") && got[1].contains("> I started on the export job."),
        "{}",
        got[1]
    );
    assert!(
        got[2].contains("Question")
            && got[2].contains("q1")
            && got[2].contains("fleet send c --answers q1"),
        "{}",
        got[2]
    );
    let gw = gateway(&env, "p");
    assert_eq!(gw["children"]["c"]["uid"], c_uid, "{gw:#}");
    assert_eq!(gw["children"]["c"]["declared"], 3);
    assert!(gw["children"]["c"]["result"].is_null());
    // The parent answers the child's Question.
    env.fleet(&["send", "c", "--answers", "q1", "Yes, go ahead."])
        .ok();
    let st = env.get("c")["status"].clone();
    assert_eq!(st["answered"]["q1"]["from"], "cli/tester", "{st:#}");
    // The child's result is one more entry, once per generation.
    env.fleet(&[
        "result",
        "c",
        "--generation",
        "1",
        "--outcome",
        "shipped",
        "--summary",
        "restarted",
        "--output",
        "prUrl=\"https://example.test/pr/1\"",
    ])
    .ok();
    env.reconcile().ok();
    let got = receipts(&env, "p");
    assert_eq!(got.len(), 3, "{got:#?}");
    assert_eq!(
        got[2]["key"],
        format!("task/p/{p_uid}/gateway/child/{c_uid}/result/1")
    );
    let result = &texts(&env, "p")[3];
    assert!(
        result.contains("shipped") && result.contains("restarted"),
        "{result}"
    );
    assert_eq!(gateway(&env, "p")["children"]["c"]["result"], 1);
    // Quiet afterwards.
    let report = env.reconcile().ok().json();
    assert_eq!(report["idle"], true, "{report:#}");
    env.fleet(&["status", "task", "p"])
        .ok()
        .stdout_has("gateway children 1");
    // Amended, the child re-opens, and its new result reaches the parent
    // once more, under the new generation.
    env.fleet(&["amend", "c", "--text", "Also restart the importer."])
        .ok();
    env.reconcile().ok();
    assert_eq!(env.status("c")["phase"], "Briefed");
    assert_eq!(receipts(&env, "p").len(), 3, "an amendment relays nothing");
    env.fleet(&[
        "result",
        "c",
        "--generation",
        "2",
        "--outcome",
        "shipped",
        "--summary",
        "both restarted",
        "--output",
        "prUrl=\"https://example.test/pr/1\"",
    ])
    .ok();
    env.reconcile().ok();
    let got = receipts(&env, "p");
    assert_eq!(got.len(), 4, "{got:#?}");
    assert_eq!(
        got[3]["key"],
        format!("task/p/{p_uid}/gateway/child/{c_uid}/result/2")
    );
    assert!(texts(&env, "p")[4].contains("both restarted"));
    env.assert_no_temp();
}

#[test]
fn a_long_child_result_reaches_its_parent_as_a_short_pointer() {
    // A 16 KiB summary, as a review child reported to Mission Control: the
    // parent is told where to read it, not given it to type.
    let mut env = Env::new("child-long-result");
    env.set_var("USER", "tester");
    env.apply(support::task("p", true)).ok();
    env.reconcile().ok();
    let p_uid = uid_of(&env, "task", "p");
    env.apply(child_of("c", "p", &p_uid, true)).ok();
    env.reconcile().ok();
    let summary = "A review line of markdown.\n".repeat(600);
    env.fleet(&[
        "result",
        "c",
        "--generation",
        "1",
        "--outcome",
        "shipped",
        "--summary",
        &summary,
        "--output",
        "prUrl=\"https://example.test/pr/1\"",
    ])
    .ok();
    env.reconcile().ok();
    let got = texts(&env, "p");
    let relayed = got.last().unwrap();
    assert!(relayed.len() < 1_000, "{} bytes: {relayed}", relayed.len());
    assert!(
        relayed.contains("outcome shipped") && relayed.contains("fleet get task c"),
        "{relayed}"
    );
    assert!(!relayed.contains("A review line"), "{relayed}");
}

#[test]
fn a_parent_relays_more_unread_child_items_than_its_inbox_holds_in_bounded_batches() {
    let mut env = Env::new("child-burst");
    env.set_var("USER", "tester");
    env.apply(support::task("p", true)).ok();
    env.reconcile().ok();
    let p_uid = uid_of(&env, "task", "p");
    env.apply(child_of("c", "p", &p_uid, true)).ok();
    env.reconcile().ok();
    let c_uid = uid_of(&env, "task", "c");
    // More unread items than the store accepts into one inbox: the child
    // declares them while the parent is not swept, as after a downtime.
    const BURST: usize = 30;
    for i in 0..BURST {
        env.fleet(&[
            "declare",
            "c",
            "--json",
            &format!(r#"{{"kind":"Comment","id":"c{i}","text":"step {i} done"}}"#),
        ])
        .ok();
    }
    env.fleet(&[
        "result",
        "c",
        "--generation",
        "1",
        "--outcome",
        "shipped",
        "--summary",
        "all steps done",
        "--output",
        "prUrl=\"https://example.test/pr/1\"",
    ])
    .ok();
    // Each sweep relays what the inbox has room for and delivers it; no
    // sweep fails, and the parent drains every item in order.
    let mut idle = false;
    for _ in 0..BURST {
        let report = env.reconcile().ok().json();
        assert_eq!(report["errors"], json!([]), "{report:#}");
        let pending = env.status("p")["inbox"].as_array().map_or(0, Vec::len);
        assert!(pending <= 21, "{pending} undelivered entries");
        if report["idle"] == true {
            idle = true;
            break;
        }
    }
    assert!(idle, "the parent never settled");
    let keys: Vec<String> = receipts(&env, "p")
        .iter()
        .map(|r| r["key"].as_str().unwrap().to_string())
        .collect();
    let mut want: Vec<String> = (0..BURST)
        .map(|i| format!("task/p/{p_uid}/gateway/child/{c_uid}/{i}"))
        .collect();
    want.push(format!("task/p/{p_uid}/gateway/child/{c_uid}/result/1"));
    assert_eq!(keys, want);
    let gw = gateway(&env, "p");
    assert_eq!(gw["children"]["c"]["declared"], BURST, "{gw:#}");
    assert_eq!(gw["children"]["c"]["result"], 1, "{gw:#}");
    env.assert_no_temp();
}
