//! Reactions end to end: a person's mention starts a Task, and the message
//! gets the "working" reaction when it is relayed into the Task's inbox;
//! once Fleet posts in the thread for the Task, the message gets the
//! "replied" reaction and loses "working". A reaction that fails holds
//! back neither the relay nor the post and raises nothing, and
//! `reactions = false` turns them off.

mod support;

use fleet_api::gateway::{Author, AuthorKind, ConversationRef, Message};
use fleet_gateway::fake::GatewayFault;
use serde_json::{Value, json};
use support::{ALEX_ID, Env, GATEWAY_ACCOUNT, SAM_ID, conversation_name, message_event};

const T0: u64 = 1_730_000_000_000;
const THREAD: &str = "fake:C0123ABCD:1730000000000";
const BOT: &str = "B0FLEET001";
/// The id of Alex's mention, the message that starts the Task.
const MENTION: &str = "1730000300000";
const WORKING: &str = "eyes";
const REPLIED: &str = "white_check_mark";

fn thread() -> ConversationRef {
    ConversationRef::split(THREAD).unwrap()
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

/// A gateway environment whose config `setup` may change first, with one
/// thread and the Task Alex's mention started there, tied and joined.
fn tied(name: &str, setup: impl FnOnce(&Env)) -> (Env, String) {
    let mut env = Env::gateway(name);
    env.set_var("USER", "tester");
    setup(&env);
    env.fake_gateway()
        .seed(
            &thread(),
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
            MENTION,
            ALEX_ID,
            T0 + 300_000,
            &format!("<@{BOT}> can you look into it?"),
        ),
    )
    .ok();
    env.reconcile().ok();
    env.reconcile().ok();
    let task = format!("{}-1", conversation_name(GATEWAY_ACCOUNT, THREAD));
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

/// Put `line` at the head of the config's `[gateway]` section.
fn gateway_line(env: &Env, line: &str) {
    let cfg = std::fs::read_to_string(&env.config).unwrap();
    let cfg = cfg.replacen("\n[gateway]\n", &format!("\n[gateway]\n{line}\n"), 1);
    std::fs::write(&env.config, cfg).unwrap();
}

fn gateway(env: &Env, task: &str) -> Value {
    env.get(task)["status"]["gateway"].clone()
}

fn declare(env: &Env, task: &str, item: Value) {
    env.fleet(&["declare", task, "--json", &item.to_string()])
        .ok();
}

fn reconcile_clean(env: &Env) -> Value {
    let report = env.reconcile().ok().json();
    assert_eq!(report["errors"], json!([]), "{report:#}");
    report
}

fn reacted(env: &Env, message: &str) -> Vec<String> {
    env.fake_gateway().reacted(&thread(), message).unwrap()
}

/// The texts the fake session was given, the brief first.
fn texts(env: &Env, task: &str) -> Vec<String> {
    env.host()
        .session(&env.session_key(task))
        .unwrap()
        .expect("a session")
        .texts
}

fn attention(gw: &Value) -> Value {
    gw["conditions"]
        .as_array()
        .and_then(|cs| cs.iter().find(|c| c["type"] == "NeedsAttention").cloned())
        .unwrap_or(Value::Null)
}

fn comment_posted(env: &Env, task: &str) -> bool {
    env.fake_gateway().posts().unwrap().iter().any(|p| {
        p.key.starts_with(&format!("task/{task}/")) && p.key.ends_with("/gateway/comment/c1")
    })
}

#[test]
fn a_mention_is_marked_working_then_replied_once_the_tasks_comment_is_posted() {
    let (env, task) = tied("working-then-replied", |_| {});
    // The join relayed the mention and the allowlisted message before it.
    assert_eq!(reacted(&env, MENTION), vec![WORKING.to_string()]);
    assert_eq!(reacted(&env, &T0.to_string()), vec![WORKING.to_string()]);
    let gw = gateway(&env, &task);
    assert_eq!(gw["reactions"]["marks"][MENTION], "working", "{gw:#}");

    declare(
        &env,
        &task,
        json!({"kind": "Comment", "id": "c1", "text": "Looking at the exporter now."}),
    );
    reconcile_clean(&env);
    assert!(comment_posted(&env, &task), "the Comment is posted");
    assert_eq!(
        reacted(&env, MENTION),
        vec![REPLIED.to_string()],
        "replied, and no longer working"
    );
    assert_eq!(reacted(&env, &T0.to_string()), vec![REPLIED.to_string()]);
    let gw = gateway(&env, &task);
    assert_eq!(gw["reactions"]["marks"][MENTION], "replied", "{gw:#}");
    assert_ne!(attention(&gw)["status"], "True", "{gw:#}");

    // Nothing is reacted twice, and Fleet's own Comment gets no reaction.
    let calls = env.fake_gateway().reactions().unwrap();
    assert_eq!(
        calls.len(),
        6,
        "two adds, two adds and two removals: {calls:#?}"
    );
    reconcile_clean(&env);
    reconcile_clean(&env);
    assert_eq!(env.fake_gateway().reactions().unwrap(), calls);

    // A later message is marked working on its relay, and a context message
    // from someone off the allowlist is not.
    env.receive(
        "Ev02",
        message_event(THREAD, "x2", ALEX_ID, T0 + 400_000, "Also the weekly one."),
    )
    .ok();
    env.receive(
        "Ev03",
        message_event(THREAD, "x3", "U0STRANGER", T0 + 410_000, "me too"),
    )
    .ok();
    reconcile_clean(&env);
    assert_eq!(reacted(&env, "x2"), vec![WORKING.to_string()]);
    assert_eq!(reacted(&env, "x3"), Vec::<String>::new());
    assert_eq!(reacted(&env, MENTION), vec![REPLIED.to_string()]);
}

#[test]
fn a_failing_reaction_holds_back_neither_the_relay_nor_the_post() {
    let (env, task) = tied("reaction-fails", |env| {
        env.fake_gateway()
            .add_fault(GatewayFault::new("react", "failed").times(u32::MAX))
            .unwrap();
    });
    assert_eq!(texts(&env, &task).len(), 2, "the brief and the join");
    assert_eq!(reacted(&env, MENTION), Vec::<String>::new());
    declare(
        &env,
        &task,
        json!({"kind": "Comment", "id": "c1", "text": "On it."}),
    );
    reconcile_clean(&env);
    assert!(comment_posted(&env, &task), "the Comment is posted");
    let gw = gateway(&env, &task);
    assert_ne!(attention(&gw)["status"], "True", "{gw:#}");

    // The same with an answer that is lost: the reaction is Unknown.
    env.fake_gateway()
        .update(|s| {
            s.faults.clear();
            s.faults
                .push(GatewayFault::new("react", "lost").times(u32::MAX));
        })
        .unwrap();
    env.receive(
        "Ev02",
        message_event(THREAD, "x2", ALEX_ID, T0 + 400_000, "Also the weekly one."),
    )
    .ok();
    reconcile_clean(&env);
    assert_eq!(texts(&env, &task).len(), 3, "the relay went out");
    declare(
        &env,
        &task,
        json!({"kind": "Comment", "id": "c2", "text": "Weekly too."}),
    );
    reconcile_clean(&env);
    assert!(
        env.fake_gateway()
            .posts()
            .unwrap()
            .iter()
            .any(|p| p.key.contains("/gateway/comment/c2")),
        "the second Comment is posted"
    );
    let gw = gateway(&env, &task);
    assert_ne!(attention(&gw)["status"], "True", "{gw:#}");
    // Retries are bounded: at most three attempts per reaction key, and no
    // record is left waiting.
    let calls = env.fake_gateway().calls("react").unwrap();
    assert!(calls <= 3 * 8, "{calls} react calls");
    let pending: Vec<&String> = gw["posts"]
        .as_object()
        .map(|o| o.keys().filter(|k| k.contains("/gateway/react/")).collect())
        .unwrap_or_default();
    assert!(
        pending.len() <= 8,
        "react records stay bounded: {pending:?}"
    );
}

#[test]
fn reactions_false_turns_them_off() {
    let (env, task) = tied("reactions-off", |env| {
        gateway_line(env, "reactions = false")
    });
    assert_eq!(texts(&env, &task).len(), 2, "the brief and the join");
    declare(
        &env,
        &task,
        json!({"kind": "Comment", "id": "c1", "text": "On it."}),
    );
    reconcile_clean(&env);
    assert!(comment_posted(&env, &task), "the Comment is posted");
    assert_eq!(env.fake_gateway().calls("react").unwrap(), 0);
    assert!(env.fake_gateway().reactions().unwrap().is_empty());
}

#[test]
fn reactions_can_be_renamed_in_the_config() {
    let (env, task) = tied("reactions-renamed", |env| {
        gateway_line(
            env,
            "reactions = { working = \"gear\", replied = \"thumbsup\" }",
        )
    });
    assert_eq!(reacted(&env, MENTION), vec!["gear".to_string()]);
    declare(
        &env,
        &task,
        json!({"kind": "Comment", "id": "c1", "text": "On it."}),
    );
    reconcile_clean(&env);
    assert_eq!(reacted(&env, MENTION), vec!["thumbsup".to_string()]);
}

#[test]
fn a_reaction_name_that_is_not_one_emoji_name_is_refused() {
    for (line, want) in [
        (
            "reactions = { working = \"two words\" }",
            "gateway.reactions.working: \"two words\" is not one emoji name",
        ),
        (
            "reactions = { working = \"white_check_mark\" }",
            "gateway.reactions: working and replied name the same emoji",
        ),
        (
            "reactions = { queued = \"eyes\" }",
            "gateway.reactions: working and queued name the same emoji",
        ),
        (
            "reactions = { queued = \"\" }",
            "gateway.reactions.queued: \"\" is not one emoji name",
        ),
        ("reactions = { done = \"x\" }", "reactions"),
    ] {
        let env = Env::gateway("reactions-refused");
        gateway_line(&env, line);
        let run = env.fleet(&["reconcile"]);
        assert_ne!(run.code, Some(0), "{line}");
        assert!(run.stderr.contains(want), "{line}: {}", run.stderr);
    }
}

const QUEUED: &str = "hourglass_flowing_sand";

/// The adds of `reaction` Fleet made on `message`, in order.
fn adds(env: &Env, message: &str, reaction: &str) -> usize {
    env.fake_gateway()
        .reactions()
        .unwrap()
        .iter()
        .filter(|r| r.message == message && r.reaction == reaction && r.on)
        .count()
}

fn conv() -> String {
    conversation_name(GATEWAY_ACCOUNT, THREAD)
}

/// The Conversation's reaction effect records, by key.
fn conv_reactions(env: &Env) -> Vec<String> {
    env.conversation_status(&conv())["effects"]
        .as_object()
        .map(|o| {
            o.keys()
                .filter(|k| k.contains("/react/"))
                .cloned()
                .collect()
        })
        .unwrap_or_default()
}

/// Move the Conversation's reaction records ten minutes into the past, as
/// time would, so a retry is due.
fn age_reactions(env: &Env) {
    let path = env.object_path("Conversation", &conv());
    let mut obj: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    if let Some(effects) = obj["status"]["effects"].as_object_mut() {
        for (key, record) in effects.iter_mut() {
            if !key.contains("/react/") {
                continue;
            }
            for part in ["intent", "receipt"] {
                if let Some(t) = record[part]["at"].as_u64() {
                    record[part]["at"] = json!(t - 600_000);
                }
            }
        }
    }
    std::fs::write(&path, obj.to_string()).unwrap();
}

/// A gateway environment with one thread seeded and nothing received yet.
fn admitting(name: &str) -> Env {
    let mut env = Env::gateway(name);
    env.set_var("USER", "tester");
    env.fake_gateway()
        .seed(
            &thread(),
            vec![person(SAM_ID, T0, "The nightly export failed again.")],
        )
        .unwrap();
    env.apply(support::object(
        "TaskConfig",
        "conversation",
        json!({"harness": "claude-code", "host": "local"}),
    ))
    .ok();
    env
}

fn mention(env: &Env, delivery: &str, id: &str, at: u64) {
    env.receive(
        delivery,
        message_event(
            THREAD,
            id,
            ALEX_ID,
            at,
            &format!("<@{BOT}> can you look into it?"),
        ),
    )
    .ok();
}

#[test]
fn an_admitted_mention_is_marked_working_at_admission_and_not_again_at_the_join() {
    let env = admitting("working-at-admission");
    mention(&env, "Ev01", MENTION, T0 + 300_000);
    reconcile_clean(&env);
    let task = format!("{}-1", conv());
    // Admission reacted, under the Conversation's own key: the mark exists
    // before the Task's first pass needs to run.
    let st = env.conversation_status(&conv());
    assert_eq!(st["reactions"]["marks"][MENTION], "working", "{st:#}");
    let keys = conv_reactions(&env);
    assert!(
        keys.iter()
            .any(|k| k.ends_with(&format!("/react/{MENTION}/working/on"))),
        "the working reaction is the Conversation's effect: {keys:?}"
    );
    assert_eq!(reacted(&env, MENTION), vec![WORKING.to_string()]);
    // The join relayed the mention and marked it working, without a second
    // reaction; the earlier message, which started nothing, is the join's.
    assert_eq!(
        gateway(&env, &task)["conversation"]["state"],
        "tied",
        "{:#}",
        gateway(&env, &task)
    );
    assert_eq!(
        gateway(&env, &task)["reactions"]["marks"][MENTION],
        "working"
    );
    assert_eq!(adds(&env, MENTION, WORKING), 1, "added once");
    assert_eq!(reacted(&env, &T0.to_string()), vec![WORKING.to_string()]);
    assert_eq!(adds(&env, &T0.to_string(), WORKING), 1);
    // A thread post replies to both, as before.
    declare(
        &env,
        &task,
        json!({"kind": "Comment", "id": "c1", "text": "Looking at it."}),
    );
    reconcile_clean(&env);
    reconcile_clean(&env);
    assert_eq!(reacted(&env, MENTION), vec![REPLIED.to_string()]);
    assert_eq!(reacted(&env, &T0.to_string()), vec![REPLIED.to_string()]);
    assert_eq!(adds(&env, MENTION, WORKING), 1, "never added twice");
}

#[test]
fn a_queued_request_is_marked_queued_then_working_once_its_task_starts() {
    let env = admitting("queued-then-working");
    // Another Task holds the thread's own directory: the request queues.
    let mut holder = support::task("holder", false);
    holder["spec"]["workspace"] = json!([format!("file://{}/{}", env.gateway_workspace(), conv())]);
    env.apply(holder).ok();
    mention(&env, "Ev01", MENTION, T0 + 300_000);
    reconcile_clean(&env);
    let task = format!("{}-1", conv());
    env.get_absent(&task);
    let st = env.conversation_status(&conv());
    assert_eq!(st["task"]["state"], "busy", "{st:#}");
    assert_eq!(st["reactions"]["marks"][MENTION], "queued", "{st:#}");
    assert_eq!(reacted(&env, MENTION), vec![QUEUED.to_string()]);
    // A later admitted message waits with it, and is marked queued too; a
    // stranger's is not.
    env.receive(
        "Ev02",
        message_event(THREAD, "x2", ALEX_ID, T0 + 400_000, "Also the weekly one."),
    )
    .ok();
    env.receive(
        "Ev03",
        message_event(THREAD, "x3", "U0STRANGER", T0 + 410_000, "me too"),
    )
    .ok();
    reconcile_clean(&env);
    assert_eq!(reacted(&env, "x2"), vec![QUEUED.to_string()]);
    assert_eq!(reacted(&env, "x3"), Vec::<String>::new());
    // The busy notice is no reply: the marks stay queued.
    assert_eq!(reacted(&env, MENTION), vec![QUEUED.to_string()]);
    // The directory frees: the Task starts, and every queued message is
    // marked working instead, once.
    env.fleet(&["delete", "task", "holder"]).ok();
    env.reconcile().ok();
    env.expire_wait_of("Conversation", &conv());
    reconcile_clean(&env);
    reconcile_clean(&env);
    assert_eq!(
        gateway(&env, &task)["conversation"]["state"],
        "tied",
        "{:#}",
        gateway(&env, &task)
    );
    for id in [MENTION, "x2"] {
        assert_eq!(reacted(&env, id), vec![WORKING.to_string()], "{id}");
        assert_eq!(adds(&env, id, WORKING), 1, "{id}: added once");
        assert_eq!(adds(&env, id, QUEUED), 1, "{id}: queued once");
    }
    let st = env.conversation_status(&conv());
    assert_eq!(st["reactions"]["marks"][MENTION], "working", "{st:#}");
    assert_eq!(reacted(&env, "x3"), Vec::<String>::new());
    // A thread post replies to them.
    declare(
        &env,
        &task,
        json!({"kind": "Comment", "id": "c1", "text": "Looking at it."}),
    );
    reconcile_clean(&env);
    reconcile_clean(&env);
    for id in [MENTION, "x2"] {
        assert_eq!(reacted(&env, id), vec![REPLIED.to_string()], "{id}");
    }
}

#[test]
fn the_queued_reaction_can_be_renamed_and_a_failing_one_holds_nothing_back() {
    let env = admitting("queued-renamed");
    gateway_line(&env, "reactions = { queued = \"pause_button\" }");
    env.fake_gateway()
        .add_fault(GatewayFault::new("react", "failed").times(1))
        .unwrap();
    let mut holder = support::task("holder", false);
    holder["spec"]["workspace"] = json!([format!("file://{}/{}", env.gateway_workspace(), conv())]);
    env.apply(holder).ok();
    mention(&env, "Ev01", MENTION, T0 + 300_000);
    reconcile_clean(&env);
    let st = env.conversation_status(&conv());
    assert_eq!(st["task"]["state"], "busy", "{st:#}");
    // The first attempt was refused: the busy notice went out all the
    // same, and no condition names the reaction.
    assert_eq!(env.fake_gateway().posts().unwrap().len(), 1);
    let attention = support::condition(&st, "NeedsAttention");
    assert_eq!(attention["reason"], "WorkspaceBusy", "{st:#}");
    // Tried again after the delay.
    age_reactions(&env);
    env.expire_wait_of("Conversation", &conv());
    reconcile_clean(&env);
    assert_eq!(reacted(&env, MENTION), vec!["pause_button".to_string()]);
}
