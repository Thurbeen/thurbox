//! The Gateway's log channel end to end: a tied thread carries only what is
//! useful to the person in it (a Comment, a Question, and one notice when a
//! change is first ready for review), and every Task's Drafts, result and
//! change news go to the one `gateway.logs` channel as top-level posts that
//! name the Task and, for a tied Task, its thread. An untied Task and a
//! child log there too. Without `gateway.logs`, Drafts, results and change
//! news are posted nowhere; the thread's ready notice still is. A log post is
//! a keyed effect recovered by its marker in the log channel's history.

mod support;

use std::fs;

use fleet_api::forge::{
    ChangeSnapshot, ChangeState, Checks, HeadSeen, Mergeable, ReviewDecision, Threads,
};
use fleet_api::gateway::{Author, AuthorKind, ConversationRef, Message};
use fleet_gateway::fake::{GatewayFault, PostRecord};
use serde_json::{Value, json};
use support::{
    ALEX_ID, Env, FORGE_HOST, GATEWAY_ACCOUNT, SAM_ID, conversation_name, derived_name,
    message_event,
};

const T0: u64 = 1_730_000_000_000;
const THREAD: &str = "fake:C0123ABCD:1730000000000";
/// The log channel: no route covers it.
const LOGS: &str = "C0456EFGH";
const LOGS_ADDRESS: &str = "fake:C0456EFGH";
const BOT: &str = "B0FLEET001";
const PROJECT: &str = "acme/widgets";
const URL: &str = "https://forge.test/acme/widgets/pull/7";

/// An environment with the fake forge and the fake gateway, and
/// `gateway.logs` naming the log channel when `logs` is true.
fn env(name: &str, logs: bool) -> Env {
    let mut env = Env::forge(name);
    env.set_var("USER", "tester");
    let mut section = support::gateway_section(&env.root);
    if logs {
        section = section.replacen(
            "[gateway]\n",
            &format!(
                "[gateway]\nlogs = {{ account = \"{GATEWAY_ACCOUNT}\", channel = \"{LOGS}\" }}\n"
            ),
            1,
        );
    }
    let mut cfg = fs::read_to_string(&env.config).expect("config");
    cfg.push_str(&section);
    fs::write(&env.config, cfg).expect("config");
    fs::create_dir_all(env.root.join("briefs")).expect("briefs dir");
    fs::write(
        env.root.join("briefs/conversation.md"),
        "Help the person in this thread.\n",
    )
    .expect("brief");
    env
}

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

/// The Task Alex's mention started in the mirrored thread, joined.
fn tied(env: &Env) -> String {
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
    let task = format!("{}-1", conversation_name(GATEWAY_ACCOUNT, THREAD));
    clock(env);
    assert_eq!(
        env.get(&task)["status"]["gateway"]["conversation"]["state"],
        "tied",
        "the Task is tied and joined"
    );
    task
}

/// The platform's clock reads like Fleet's, as a real one would: a
/// recovery read asks for what was posted since the intent.
fn clock(env: &Env) {
    env.fake_gateway()
        .set_clock(support::now_ms() + 60_000)
        .unwrap();
}

fn uid_of(env: &Env, name: &str) -> String {
    env.fleet(&["get", "task", name]).ok().json()["metadata"]["uid"]
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

/// Report generation 1 shipped. A Task made by `support::task` requires its
/// `prUrl` output; a conversation Task declares none.
fn result(env: &Env, task: &str, summary: &str) {
    let mut args = vec![
        "result",
        task,
        "--generation",
        "1",
        "--outcome",
        "shipped",
        "--summary",
        summary,
    ];
    if !task.starts_with("conv-") {
        args.extend(["--output", "prUrl=\"https://example.test/pr/1\""]);
    }
    env.fleet(&args).ok();
}

fn posts(env: &Env) -> Vec<PostRecord> {
    env.fake_gateway().posts().unwrap()
}

fn in_logs(env: &Env) -> Vec<PostRecord> {
    posts(env)
        .into_iter()
        .filter(|p| p.conversation.address() == LOGS_ADDRESS)
        .collect()
}

fn in_thread(env: &Env) -> Vec<PostRecord> {
    posts(env)
        .into_iter()
        .filter(|p| p.conversation.address() == THREAD)
        .collect()
}

fn keys(posts: &[PostRecord]) -> Vec<String> {
    posts.iter().map(|p| p.key.clone()).collect()
}

fn reconcile_clean(env: &Env) -> Value {
    let report = env.reconcile().ok().json();
    assert_eq!(report["errors"], json!([]), "{report:#}");
    report
}

fn attention_open(gw: &Value) -> bool {
    gw["conditions"].as_array().is_some_and(|cs| {
        cs.iter()
            .any(|c| c["type"] == "NeedsAttention" && c["status"] == "True")
    })
}

fn change(state: ChangeState) -> ChangeSnapshot {
    ChangeSnapshot {
        state,
        head: HeadSeen {
            id: "aaa111".into(),
            checks: Checks::Passed,
            failing: Vec::new(),
        },
        review_decision: ReviewDecision::None,
        mergeable: Mergeable {
            value: Some(true),
            reason: None,
        },
        threads: Threads { unresolved: 0 },
        reviews: Vec::new(),
        notes: Vec::new(),
    }
}

#[test]
fn a_tied_tasks_thread_gets_its_comment_and_one_ready_notice_and_the_log_gets_the_rest() {
    let env = env("logs-tied", true);
    let task = tied(&env);
    let uid = uid_of(&env, &task);
    let forge = env.fake_forge();
    forge
        .update(|s| s.login = Some("fleet-bot".into()))
        .unwrap();
    forge
        .set_change(PROJECT, "7", change(ChangeState::Draft))
        .unwrap();
    let thread_before = in_thread(&env).len();
    declare(
        &env,
        &task,
        json!({"kind": "Comment", "id": "c1", "text": "The export runs again: 2+2 = 4."}),
    );
    declare(
        &env,
        &task,
        json!({"kind": "Draft", "id": "d1", "title": "Exporter fix", "url": URL}),
    );
    env.fleet(&["declare", &task, "--change-request", URL]).ok();
    reconcile_clean(&env);
    let cr = derived_name("fake", FORGE_HOST, PROJECT, "7");
    // The change leaves draft: ready for review, then merged.
    for state in [ChangeState::Open, ChangeState::Merged] {
        forge.set_change(PROJECT, "7", change(state)).unwrap();
        env.expire_wait_of("ChangeRequest", &cr);
        reconcile_clean(&env);
        reconcile_clean(&env);
    }
    result(&env, &task, "Answered in the thread.");
    reconcile_clean(&env);
    reconcile_clean(&env);
    let cr_uid = env.change_request(&cr)["metadata"]["uid"]
        .as_str()
        .unwrap()
        .to_string();
    let base = format!("task/{task}/{uid}/gateway");
    let thread = in_thread(&env);
    assert_eq!(
        keys(&thread[thread_before..]),
        vec![
            format!("{base}/comment/c1"),
            format!("{base}/ready/{cr_uid}")
        ],
        "the thread gets the Comment and one ready notice: {thread:#?}"
    );
    let notice = &thread[thread_before + 1].text;
    assert!(notice.contains(URL), "the notice carries the URL: {notice}");
    let logs = in_logs(&env);
    let got = keys(&logs);
    assert!(got.contains(&format!("{base}/draft/d1")), "{logs:#?}");
    assert!(got.contains(&format!("{base}/result/1")), "{logs:#?}");
    let news: Vec<&PostRecord> = logs
        .iter()
        .filter(|p| p.key.starts_with(&format!("{base}/change/{cr_uid}/")))
        .collect();
    assert!(
        news.iter().any(|p| p.text.contains("merged")),
        "the merge is logged: {logs:#?}"
    );
    assert!(
        news.iter().any(|p| p.text.contains("ready for review")),
        "the ready notice is logged too: {logs:#?}"
    );
    for p in &logs {
        assert!(p.text.contains(&task), "each post names the Task: {p:?}");
        assert!(
            p.text
                .contains("https://fake.invalid/C0123ABCD/1730000000000"),
            "a tied Task's log post links its thread: {p:?}"
        );
    }
    let result_post = logs.iter().find(|p| p.key.ends_with("/result/1")).unwrap();
    assert!(
        result_post.text.contains("shipped")
            && result_post.text.contains("Answered in the thread."),
        "{result_post:?}"
    );
    let gw = gateway(&env, &task);
    let record = &gw["posts"][format!("{base}/result/1")];
    assert_eq!(record["intent"]["action"], "log", "{gw:#}");
    assert_eq!(record["receipt"]["outcome"]["known"]["type"], "posted");
    // A log post opens no thread and creates no Conversation.
    let conv = conversation_name(GATEWAY_ACCOUNT, LOGS_ADDRESS);
    assert!(
        env.fleet(&["get", "conversation", &conv]).code != Some(0),
        "no Conversation for the log channel"
    );
    assert_eq!(env.fake_gateway().calls("open").unwrap(), 0);
    // Nothing is posted twice.
    let all = posts(&env).len();
    reconcile_clean(&env);
    assert_eq!(posts(&env).len(), all);
}

#[test]
fn an_untied_tasks_result_and_change_news_go_to_the_log_channel() {
    let env = env("logs-untied", true);
    clock(&env);
    let forge = env.fake_forge();
    forge
        .update(|s| s.login = Some("fleet-bot".into()))
        .unwrap();
    forge
        .set_change(PROJECT, "7", change(ChangeState::Open))
        .unwrap();
    env.apply(support::base_config()).ok();
    env.apply(support::task("t1", true)).ok();
    env.reconcile().ok();
    let uid = uid_of(&env, "t1");
    env.fleet(&["declare", "t1", "--change-request", URL]).ok();
    env.reconcile().ok();
    let cr = derived_name("fake", FORGE_HOST, PROJECT, "7");
    forge
        .set_change(PROJECT, "7", change(ChangeState::Merged))
        .unwrap();
    env.expire_wait_of("ChangeRequest", &cr);
    reconcile_clean(&env);
    result(&env, "t1", "The exporter retries now.");
    reconcile_clean(&env);
    reconcile_clean(&env);
    let cr_uid = env.change_request(&cr)["metadata"]["uid"]
        .as_str()
        .unwrap()
        .to_string();
    let logs = in_logs(&env);
    let base = format!("task/t1/{uid}/gateway");
    let got = keys(&logs);
    assert!(
        got.contains(&format!("{base}/result/1")),
        "the result is logged: {logs:#?}"
    );
    let merged: Vec<&PostRecord> = logs
        .iter()
        .filter(|p| p.key.starts_with(&format!("{base}/change/{cr_uid}/")))
        .collect();
    assert!(
        merged
            .iter()
            .any(|p| p.text.contains("merged") && p.text.contains(URL)),
        "the merge is logged: {logs:#?}"
    );
    for p in &logs {
        assert!(p.text.contains("t1"), "each post names the Task: {p:?}");
        assert!(!p.text.contains("Thread:"), "an untied Task has no thread");
    }
    assert!(in_thread(&env).is_empty());
    assert_eq!(env.fake_gateway().calls("open").unwrap(), 0);
}

#[test]
fn a_childs_result_goes_to_its_parents_inbox_and_to_the_log_channel() {
    let env = env("logs-child", true);
    clock(&env);
    env.apply(support::task("p", true)).ok();
    env.reconcile().ok();
    let p_uid = uid_of(&env, "p");
    let mut child = support::task("c", true);
    child["metadata"]["labels"] = json!({
        "gateway.fleet.dev/parent": "p",
        "gateway.fleet.dev/parent-uid": p_uid,
    });
    env.apply(child).ok();
    env.reconcile().ok();
    let c_uid = uid_of(&env, "c");
    result(&env, "c", "Child work done.");
    reconcile_clean(&env);
    reconcile_clean(&env);
    assert_eq!(
        keys(&in_logs(&env)),
        vec![format!("task/c/{c_uid}/gateway/result/1")]
    );
    let texts = env
        .host()
        .session(&env.session_key("p"))
        .unwrap()
        .expect("a session")
        .texts;
    assert!(
        texts.iter().any(|t| t.contains("Child Task c")),
        "the parent still hears its child: {texts:#?}"
    );
}

#[test]
fn without_gateway_logs_results_and_change_news_are_posted_nowhere() {
    let env = env("logs-absent", false);
    let task = tied(&env);
    let forge = env.fake_forge();
    forge
        .update(|s| s.login = Some("fleet-bot".into()))
        .unwrap();
    forge
        .set_change(PROJECT, "7", change(ChangeState::Open))
        .unwrap();
    let thread_before = in_thread(&env).len();
    declare(
        &env,
        &task,
        json!({"kind": "Comment", "id": "c1", "text": "Done."}),
    );
    declare(
        &env,
        &task,
        json!({"kind": "Draft", "id": "d1", "title": "Exporter fix", "url": URL}),
    );
    env.fleet(&["declare", &task, "--change-request", URL]).ok();
    result(&env, &task, "Answered in the thread.");
    env.apply(support::task("t1", true)).ok();
    env.reconcile().ok();
    result(&env, "t1", "Untied work done.");
    reconcile_clean(&env);
    reconcile_clean(&env);
    let thread = in_thread(&env);
    assert_eq!(thread.len(), thread_before + 2, "{thread:#?}");
    assert!(thread[thread_before].key.ends_with("/comment/c1"));
    assert!(
        thread[thread_before + 1].key.contains("/ready/")
            && thread[thread_before + 1].text.contains(URL),
        "the ready notice is the thread's, logs or not: {thread:#?}"
    );
    assert!(in_logs(&env).is_empty());
    assert_eq!(posts(&env).len(), thread.len(), "{:#?}", posts(&env));
    let gw = gateway(&env, &task);
    assert_eq!(gw["result"], 1, "read, and posted nowhere: {gw:#}");
    assert!(!attention_open(&gw), "{gw:#}");
}

#[test]
fn a_log_post_whose_answer_was_lost_is_found_in_the_log_channel_and_not_sent_again() {
    let env = env("logs-lost", true);
    clock(&env);
    env.apply(support::task("t1", true)).ok();
    env.reconcile().ok();
    let uid = uid_of(&env, "t1");
    env.fake_gateway()
        .add_fault(GatewayFault::new("post", "lost-applied"))
        .unwrap();
    result(&env, "t1", "Done.");
    env.reconcile().ok();
    env.reconcile().ok();
    env.reconcile().ok();
    let logs = in_logs(&env);
    assert_eq!(logs.len(), 1, "posted once: {logs:#?}");
    let key = format!("task/t1/{uid}/gateway/result/1");
    let gw = gateway(&env, "t1");
    assert_eq!(
        gw["posts"][&key]["receipt"]["outcome"]["known"]["type"], "posted",
        "recovered from the marker: {gw:#}"
    );
    assert_eq!(
        gw["posts"][&key]["receipt"]["outcome"]["known"]["message"],
        logs[0].message
    );
    assert!(!attention_open(&gw), "{gw:#}");
}

#[test]
fn gateway_logs_is_refused_without_its_account_or_on_a_channel_the_adapter_cannot_read() {
    for (logs, want) in [
        (
            "logs = { account = \"nobody\", channel = \"C0456EFGH\" }".to_string(),
            "gateway.logs: account \"nobody\" is not under [gateway.accounts]",
        ),
        (
            format!("logs = {{ account = \"{GATEWAY_ACCOUNT}\", channel = \"C0 456\" }}"),
            "gateway.logs: channel \"C0 456\"",
        ),
        (
            format!("logs = {{ account = \"{GATEWAY_ACCOUNT}\", channel = \"C0123ABCD\" }}"),
            "gateway.logs: channel \"C0123ABCD\" belongs to route",
        ),
    ] {
        let env = Env::gateway("logs-refused");
        let cfg = fs::read_to_string(&env.config).expect("config");
        fs::write(
            &env.config,
            cfg.replacen("[gateway]\n", &format!("[gateway]\n{logs}\n"), 1),
        )
        .expect("config");
        let run = env.fleet(&["reconcile"]);
        assert_ne!(run.code, Some(0), "{logs}");
        assert!(run.stderr.contains(want), "{logs}: {}", run.stderr);
    }
}
