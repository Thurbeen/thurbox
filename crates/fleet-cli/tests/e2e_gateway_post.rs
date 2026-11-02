//! The Gateway controller's fourth step end to end: what a tied Task
//! declares is posted in its thread, in declaration order, as keyed
//! effects that never post twice; a post with an unknown outcome is looked
//! for by its marker and otherwise waits for a person under
//! `NeedsAttention/PostUnconfirmed`, holding later posts back; a Question
//! from an untied Task opens a thread tied to it; an untied Task's other
//! words and a child's words are never posted.

mod support;

use fleet_api::gateway::{Author, AuthorKind, Capabilities, ConversationRef, Marker, Message};
use fleet_gateway::fake::{GatewayFault, PostRecord};
use serde_json::{Value, json};
use support::{
    ALEX_ID, Env, GATEWAY_ACCOUNT, SAM_ID, assert_condition, conversation_name, message_event,
};

const T0: u64 = 1_730_000_000_000;
const THREAD: &str = "fake:C0123ABCD:1730000000000";
const BOT: &str = "B0FLEET001";

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

/// A gateway environment with one mirrored thread and the Task Alex's
/// mention started there, joined. Returns the environment and the Task.
fn tied(name: &str) -> (Env, String) {
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
    env.receive(
        "Ev01",
        message_event(
            THREAD,
            "1730000300000",
            ALEX_ID,
            T0 + 300_000,
            &format!("<@{BOT}> can you look into it?"),
        ),
    )
    .ok();
    env.reconcile().ok();
    env.reconcile().ok();
    let task = format!("{conv}-1");
    // The platform's clock reads like Fleet's, as a real one would: a
    // recovery read asks for what was posted since the intent.
    env.fake_gateway()
        .set_clock(support::now_ms() + 60_000)
        .unwrap();
    assert_eq!(
        env.get(&task)["status"]["gateway"]["conversation"]["state"],
        "tied",
        "the Task is tied and joined"
    );
    (env, task)
}

fn uid_of(env: &Env, kind: &str, name: &str) -> String {
    env.fleet(&["get", kind, name]).ok().json()["metadata"]["uid"]
        .as_str()
        .unwrap()
        .to_string()
}

fn gateway(env: &Env, task: &str) -> Value {
    env.get(task)["status"]["gateway"].clone()
}

fn declare(env: &Env, task: &str, item: Value) {
    env.fleet(&["declare", task, "--json", &item.to_string()])
        .ok();
}

/// Fleet's posts on the fake platform, the refusals and notices included.
fn posts(env: &Env) -> Vec<PostRecord> {
    env.fake_gateway().posts().unwrap()
}

fn keys(posts: &[PostRecord]) -> Vec<String> {
    posts.iter().map(|p| p.key.clone()).collect()
}

fn reconcile_clean(env: &Env) -> Value {
    let report = env.reconcile().ok().json();
    assert_eq!(report["errors"], json!([]), "{report:#}");
    report
}

#[test]
fn a_tied_tasks_declarations_and_result_are_posted_in_order_and_never_twice() {
    let (env, task) = tied("in-order");
    let uid = uid_of(&env, "task", &task);
    let before = posts(&env).len();
    declare(
        &env,
        &task,
        json!({"kind": "Comment", "id": "c1", "text": "Looking at the exporter now."}),
    );
    declare(
        &env,
        &task,
        json!({"kind": "Note", "text": "not the Gateway's"}),
    );
    declare(
        &env,
        &task,
        json!({"kind": "Question", "id": "q1", "text": "Which export, nightly or weekly?"}),
    );
    declare(
        &env,
        &task,
        json!({"kind": "Draft", "id": "d1", "title": "Exporter fix", "url": "https://forge.test/o/r/pull/7"}),
    );
    env.fleet(&[
        "result",
        &task,
        "--generation",
        "1",
        "--outcome",
        "shipped",
        "--summary",
        "The exporter retries now.",
    ])
    .ok();
    reconcile_clean(&env);
    let got = posts(&env);
    let base = format!("task/{task}/{uid}/gateway");
    assert_eq!(
        keys(&got[before..]),
        vec![
            format!("{base}/comment/c1"),
            format!("{base}/question/q1"),
            format!("{base}/draft/d1"),
            format!("{base}/result/1"),
        ],
        "{got:#?}"
    );
    for p in &got[before..] {
        assert_eq!(p.conversation.address(), THREAD, "{p:?}");
    }
    assert_eq!(got[before].text, "Looking at the exporter now.");
    assert!(got[before + 1].text.contains("Which export"), "{got:#?}");
    assert!(
        got[before + 2].text.contains("Exporter fix")
            && got[before + 2]
                .text
                .contains("https://forge.test/o/r/pull/7"),
        "{:?}",
        got[before + 2].text
    );
    assert!(
        got[before + 3].text.contains("shipped")
            && got[before + 3].text.contains("The exporter retries now."),
        "{:?}",
        got[before + 3].text
    );
    let gw = gateway(&env, &task);
    assert_eq!(gw["posted"], json!(["c1", "q1", "d1"]), "{gw:#}");
    assert_eq!(gw["result"], 1);
    assert_eq!(gw["declared"], 4);
    let conv = conversation_name(GATEWAY_ACCOUNT, THREAD);
    assert_eq!(gw["questions"]["q1"]["conversation"], conv, "{gw:#}");
    assert_eq!(gw["questions"]["q1"]["at"], got[before + 1].at);
    for key in keys(&got[before..]) {
        let record = &gw["posts"][&key];
        assert_eq!(record["intent"]["action"], "post", "{gw:#}");
        assert_eq!(record["receipt"]["outcome"]["known"]["type"], "posted");
    }
    // Every later pass posts nothing again, and Fleet's own posts are
    // never relayed back to the Task.
    let inbox_before = env.get(&task)["status"]["deliveries"]["receipts"]
        .as_array()
        .map_or(0, Vec::len);
    reconcile_clean(&env);
    reconcile_clean(&env);
    assert_eq!(posts(&env).len(), got.len(), "nothing is posted twice");
    let inbox_after = env.get(&task)["status"]["deliveries"]["receipts"]
        .as_array()
        .map_or(0, Vec::len);
    assert_eq!(inbox_before, inbox_after, "Fleet's posts are not relayed");
    // A Question answered in the thread is recorded on the Task.
    env.receive(
        "Ev02",
        message_event(
            THREAD,
            "x2",
            ALEX_ID,
            got[before + 1].at + 5_000,
            "Nightly.",
        ),
    )
    .ok();
    reconcile_clean(&env);
    reconcile_clean(&env);
    let st = env.get(&task)["status"].clone();
    assert_eq!(
        st["answered"]["q1"]["from"],
        format!("gateway/{conv}"),
        "{st:#}"
    );
}

#[test]
fn a_declaration_without_an_id_is_skipped_and_named() {
    let (env, task) = tied("no-id");
    let before = posts(&env).len();
    // `fleet declare` may refuse an item without an id itself; either way
    // nothing is posted for it.
    let _ = env.fleet(&[
        "declare",
        &task,
        "--json",
        r#"{"kind":"Comment","text":"no id"}"#,
    ]);
    declare(
        &env,
        &task,
        json!({"kind": "Comment", "id": "c2", "text": "Second."}),
    );
    reconcile_clean(&env);
    let got = posts(&env);
    assert_eq!(got.len(), before + 1, "{got:#?}");
    assert_eq!(got[before].text, "Second.");
}

#[test]
fn a_post_whose_answer_was_lost_is_found_by_its_marker_and_not_sent_again() {
    let (env, task) = tied("lost-applied");
    let uid = uid_of(&env, "task", &task);
    let before = posts(&env).len();
    env.fake_gateway()
        .add_fault(GatewayFault::new("post", "lost-applied"))
        .unwrap();
    declare(
        &env,
        &task,
        json!({"kind": "Comment", "id": "c1", "text": "On it."}),
    );
    env.reconcile().ok();
    env.reconcile().ok();
    env.reconcile().ok();
    let got = posts(&env);
    assert_eq!(got.len(), before + 1, "posted once: {got:#?}");
    let key = format!("task/{task}/{uid}/gateway/comment/c1");
    let gw = gateway(&env, &task);
    assert_eq!(
        gw["posts"][&key]["receipt"]["outcome"]["known"]["type"], "posted",
        "recovered from the marker: {gw:#}"
    );
    assert_eq!(
        gw["posts"][&key]["receipt"]["outcome"]["known"]["message"],
        got[before].message
    );
    let open = gw["conditions"].as_array().is_some_and(|cs| {
        cs.iter()
            .any(|c| c["type"] == "NeedsAttention" && c["status"] == "True")
    });
    assert!(!open, "{gw:#}");
}

#[test]
fn a_post_that_never_landed_is_sent_once_more_under_its_key() {
    let (env, task) = tied("lost");
    let uid = uid_of(&env, "task", &task);
    let before = posts(&env).len();
    env.fake_gateway()
        .add_fault(GatewayFault::new("post", "lost"))
        .unwrap();
    declare(
        &env,
        &task,
        json!({"kind": "Comment", "id": "c1", "text": "On it."}),
    );
    env.reconcile().ok();
    env.reconcile().ok();
    env.reconcile().ok();
    let got = posts(&env);
    assert_eq!(got.len(), before + 1, "{got:#?}");
    let key = format!("task/{task}/{uid}/gateway/comment/c1");
    assert_eq!(got[before].key, key);
    let gw = gateway(&env, &task);
    assert_eq!(gw["posts"][&key]["attempts"], 2, "{gw:#}");
    assert_eq!(
        gw["posts"][&key]["receipt"]["outcome"]["known"]["type"],
        "posted"
    );
}

#[test]
fn an_unconfirmed_post_waits_for_a_person_and_holds_back_later_posts() {
    let (env, task) = tied("unconfirmed");
    let before = posts(&env).len();
    let gateway_fake = env.fake_gateway();
    gateway_fake
        .set_capabilities(Capabilities {
            history: true,
            marker: Marker::None,
            unmentioned: true,
            threads: true,
        })
        .unwrap();
    gateway_fake
        .add_fault(GatewayFault::new("post", "lost-applied"))
        .unwrap();
    declare(
        &env,
        &task,
        json!({"kind": "Comment", "id": "c1", "text": "First."}),
    );
    declare(
        &env,
        &task,
        json!({"kind": "Comment", "id": "c2", "text": "Second."}),
    );
    env.reconcile().ok();
    env.reconcile().ok();
    env.reconcile().ok();
    let got = posts(&env);
    assert_eq!(got.len(), before + 1, "c2 waits behind c1: {got:#?}");
    assert_eq!(got[before].text, "First.");
    let gw = gateway(&env, &task);
    assert_condition(&gw, "NeedsAttention", "True", "PostUnconfirmed");
    assert_eq!(gw["declared"], 1, "c2 is not read yet: {gw:#}");
}

#[test]
fn a_question_from_an_untied_task_opens_a_thread_tied_to_it() {
    let mut env = Env::gateway("open");
    env.set_var("USER", "tester");
    env.fake_gateway()
        .set_clock(support::now_ms() + 60_000)
        .unwrap();
    env.apply(support::task("asker", true)).ok();
    env.reconcile().ok();
    let uid = uid_of(&env, "task", "asker");
    // A Comment before the Question has nowhere to go and is never posted.
    declare(
        &env,
        "asker",
        json!({"kind": "Comment", "id": "c0", "text": "Early."}),
    );
    reconcile_clean(&env);
    assert!(
        posts(&env).is_empty(),
        "an untied Task's Comment is skipped"
    );
    declare(
        &env,
        "asker",
        json!({"kind": "Question", "id": "q1", "text": "May I restart the exporter?"}),
    );
    reconcile_clean(&env);
    reconcile_clean(&env);
    let got = posts(&env);
    let key = format!("task/asker/{uid}/gateway/question/q1");
    assert_eq!(keys(&got), vec![key.clone()], "{got:#?}");
    assert_eq!(env.fake_gateway().calls("open").unwrap(), 1);
    let thread = got[0].conversation.address();
    assert!(thread.starts_with("fake:C0123ABCD:"), "{thread}");
    assert!(got[0].text.contains("May I restart the exporter?"));
    let conv = conversation_name(GATEWAY_ACCOUNT, &thread);
    let cs = env.conversation_status(&conv);
    assert_eq!(cs["phase"], "Tied", "{cs:#}");
    assert_eq!(cs["task"]["uid"], uid, "{cs:#}");
    let gw = gateway(&env, "asker");
    assert_eq!(gw["conversation"]["name"], conv, "{gw:#}");
    assert_eq!(gw["conversation"]["state"], "tied", "{gw:#}");
    assert_eq!(gw["questions"]["q1"]["conversation"], conv);
    assert_eq!(
        gw["posts"][&key]["receipt"]["outcome"]["known"]["type"],
        "opened"
    );
    assert_condition(&gw, "Tied", "True", "Verified");
    // The reply reaches the Task and answers the Question.
    env.receive(
        "Ev10",
        message_event(&thread, "r1", ALEX_ID, got[0].at + 5_000, "Yes, go ahead."),
    )
    .ok();
    reconcile_clean(&env);
    reconcile_clean(&env);
    let st = env.get("asker")["status"].clone();
    assert_eq!(
        st["answered"]["q1"]["from"],
        format!("gateway/{conv}"),
        "{st:#}"
    );
    // Later words go to the same thread; nothing is opened again.
    declare(
        &env,
        "asker",
        json!({"kind": "Comment", "id": "c1", "text": "Restarted."}),
    );
    reconcile_clean(&env);
    let got = posts(&env);
    assert_eq!(got.len(), 2, "{got:#?}");
    assert_eq!(got[1].conversation.address(), thread);
    assert_eq!(got[1].key, format!("task/asker/{uid}/gateway/comment/c1"));
    assert_eq!(env.fake_gateway().calls("open").unwrap(), 1);
}

#[test]
fn a_child_and_an_untied_task_post_nothing() {
    let mut env = Env::gateway("child-silent");
    env.set_var("USER", "tester");
    env.apply(support::task("p", true)).ok();
    env.reconcile().ok();
    let p_uid = uid_of(&env, "task", "p");
    let mut child = support::task("c", true);
    child["metadata"]["labels"] = json!({
        "gateway.fleet.dev/parent": "p",
        "gateway.fleet.dev/parent-uid": p_uid,
    });
    env.apply(child).ok();
    env.reconcile().ok();
    declare(
        &env,
        "c",
        json!({"kind": "Question", "id": "q1", "text": "May I restart it?"}),
    );
    declare(
        &env,
        "p",
        json!({"kind": "Comment", "id": "c1", "text": "Nobody hears this."}),
    );
    env.fleet(&[
        "result",
        "p",
        "--generation",
        "1",
        "--outcome",
        "shipped",
        "--summary",
        "done",
        "--output",
        "prUrl=\"https://example.test/pr/1\"",
    ])
    .ok();
    reconcile_clean(&env);
    reconcile_clean(&env);
    assert!(posts(&env).is_empty(), "{:#?}", posts(&env));
    assert_eq!(env.fake_gateway().calls("open").unwrap(), 0);
    // The child's Question reached its parent instead.
    let texts = env
        .host()
        .session(&env.session_key("p"))
        .unwrap()
        .expect("a session")
        .texts;
    assert!(
        texts
            .iter()
            .any(|t| t.contains("fleet send c --answers q1")),
        "{texts:#?}"
    );
}
