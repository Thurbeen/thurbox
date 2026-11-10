//! A route's DM entry end to end: `"dm:*"` in `channels` covers every
//! one-to-one direct message between the bot and a person on the route's
//! account. There a top-level message starts a Task without a mention, a
//! reply in a tied DM thread is a turn for its Task, and a stranger's DM
//! earns the route's one refusal. A config without the entry behaves as
//! before, and the config refuses a second DM route on one account, the
//! entry on a Discord account, and a questions route with no channel id.

mod support;

use std::fs;

use fleet_api::gateway::Event;
use serde_json::{Value, json};
use support::{
    ALEX_ID, Env, GATEWAY_ACCOUNT, GATEWAY_CHANNEL, GATEWAY_ROUTE, conversation_name,
    gateway_section, message_event,
};

const T0: u64 = 1_730_000_000_000;
const BOT: &str = "B0FLEET001";
/// A one-to-one DM channel that no route lists.
const DM: &str = "D0123ALEX";
const ROUTE: &str = "randy";
const STRANGER: &str = "U0999ZZZZ";

fn root(env: &Env) -> String {
    env.root.to_string_lossy().replace('\\', "/")
}

/// A route over the fake account, with its own brief and base directory,
/// covering `channels`.
fn route(env: &Env, name: &str, channels: &str) -> String {
    let root = root(env);
    format!(
        "\n[gateway.routes.{name}]\naccount = \"{GATEWAY_ACCOUNT}\"\nchannels = {channels}\nallow = [\"alex\"]\n\
taskConfig = \"conversation\"\nbrief = \"{root}/briefs/randy.md\"\nworkspace = {{ base = \"{root}/work/{name}\" }}\n"
    )
}

/// The gateway environment's section with `channels` on the route `randy`
/// beside the explicit route `eng`.
fn section(env: &Env, channels: &str) -> String {
    format!(
        "{}{}",
        gateway_section(&env.root),
        route(env, ROUTE, channels)
    )
}

fn dm_env(name: &str, channels: &str) -> Env {
    let env = Env::gateway(name);
    fs::write(env.root.join("briefs/randy.md"), "Help whoever DMs.\n").expect("brief");
    env.set_gateway(&section(&env, channels));
    env.apply(support::object(
        "TaskConfig",
        "conversation",
        json!({"harness": "claude-code", "host": "local"}),
    ))
    .ok();
    env
}

/// A message `id` in `thread`. It starts the thread when the thread's id is
/// its own, as the payload says.
fn said(thread: &str, id: u64, author: &str, text: &str) -> Value {
    let mut event = message_event(thread, &id.to_string(), author, id, text);
    if thread.ends_with(&format!(":{id}")) {
        event["startsThread"] = json!(true);
    }
    event
}

/// Hand `event` to the receive verb as the fake platform delivers it, with
/// `direct` when the platform says the chat is a one-to-one DM. The message
/// is put on the fake platform's thread first, as a real platform's history
/// holds it.
fn deliver(env: &Env, delivery: &str, event: Value, direct: bool) -> Value {
    let Event::Message {
        conversation,
        message,
    } = serde_json::from_value(event.clone()).expect("a message event")
    else {
        unreachable!("a message event")
    };
    env.fake_gateway()
        .seed(&conversation, vec![message])
        .unwrap();
    let mut payload = json!({"id": delivery, "event": event});
    if direct {
        payload["direct"] = json!(true);
    }
    let envelope = json!({
        "verified": "connection",
        "receivedAt": support::now_ms(),
        "payload": payload,
    });
    env.fleet_stdin(
        &["gateway", "receive", "--account", GATEWAY_ACCOUNT, "--json"],
        &envelope.to_string(),
    )
    .ok()
    .json()
}

fn tasks(env: &Env) -> Vec<Value> {
    env.fleet(&["list", "tasks", "--json"])
        .ok()
        .json()
        .as_array()
        .unwrap()
        .clone()
}

#[test]
fn a_top_level_dm_starts_a_task_without_a_mention_and_a_reply_is_its_turn() {
    let env = dm_env("dm-admits", "[\"*\", \"dm:*\"]");
    let thread = format!("fake:{DM}:{T0}");
    let out = deliver(&env, "Ev01", said(&thread, T0, ALEX_ID, "hello"), true);
    assert_eq!(out["outcome"], "written", "{out:#}");
    let conv = conversation_name(GATEWAY_ACCOUNT, &thread);
    let obj = env.fleet(&["get", "conversation", &conv]).ok().json();
    assert_eq!(obj["spec"]["route"], ROUTE, "{obj:#}");
    assert_eq!(obj["spec"]["direct"], true, "{obj:#}");
    let report = env.reconcile().ok().json();
    assert_eq!(report["errors"], json!([]), "{report:#}");
    let task = env.get(&format!("{conv}-1"));
    assert_eq!(
        task["metadata"]["labels"]["gateway.fleet.dev/route"], ROUTE,
        "{task:#}"
    );
    assert_eq!(task["spec"]["brief"], "Help whoever DMs.\n");
    assert_eq!(
        task["spec"]["workspace"][0],
        format!("file://{}/work/{ROUTE}/{conv}", root(&env))
    );
    let st = env.conversation_status(&conv);
    assert_eq!(st["phase"], "Tied", "{st:#}");
    // A reply in the DM thread, no mention, is the tied Task's turn.
    let out = deliver(
        &env,
        "Ev02",
        said(&thread, T0 + 1_000, ALEX_ID, "and another thing"),
        true,
    );
    assert_eq!(out["outcome"], "written", "{out:#}");
    env.reconcile().ok();
    let st = env.conversation_status(&conv);
    assert_eq!(st["task"]["n"], 1, "{st:#}");
    assert_eq!(st["revision"], 2, "{st:#}");
    assert_eq!(st["judged"], st["revision"], "{st:#}");
    assert_eq!(tasks(&env).len(), 1);
    assert_eq!(env.fake_gateway().calls("post").unwrap(), 0);
}

#[test]
fn a_strangers_dm_earns_one_refusal_and_starts_no_task() {
    let env = dm_env("dm-refusal", "[\"dm:*\", \"C0789IJKL\"]");
    let thread = format!("fake:{DM}:{T0}");
    deliver(&env, "Ev01", said(&thread, T0, STRANGER, "hi bot"), true);
    deliver(
        &env,
        "Ev02",
        said(&thread, T0 + 1_000, STRANGER, "please?"),
        true,
    );
    let report = env.reconcile().ok().json();
    assert_eq!(report["errors"], json!([]), "{report:#}");
    let posts = env.fake_gateway().posts().unwrap();
    assert_eq!(posts.len(), 1, "one refusal per thread: {posts:#?}");
    assert_eq!(posts[0].conversation.address(), thread);
    assert_eq!(posts[0].text, fleet_api::gateway::DEFAULT_REFUSAL);
    assert_eq!(tasks(&env), Vec::<Value>::new());
    // An edit that does not say it is in a DM still reaches the thread: no
    // wildcard covers the chat, but its Conversation is a DM.
    let edit = json!({
        "type": "edited",
        "conversation": {"platform": "fake", "chat": DM, "thread": T0.to_string()},
        "id": T0.to_string(), "text": "hi bot, edited", "editedAt": T0 + 2_000,
    });
    let envelope = json!({
        "verified": "connection",
        "receivedAt": support::now_ms(),
        "payload": {"id": "Ev03", "event": edit},
    });
    let out = env
        .fleet_stdin(
            &["gateway", "receive", "--account", GATEWAY_ACCOUNT, "--json"],
            &envelope.to_string(),
        )
        .ok()
        .json();
    assert_eq!(out["outcome"], "written", "{out:#}");
    // In a chat with no Conversation, it is still ignored.
    let other = edit.to_string().replace(DM, "D0456OTHER");
    let envelope = json!({
        "verified": "connection",
        "receivedAt": support::now_ms(),
        "payload": {"id": "Ev04", "event": serde_json::from_str::<Value>(&other).unwrap()},
    });
    let out = env
        .fleet_stdin(
            &["gateway", "receive", "--account", GATEWAY_ACCOUNT, "--json"],
            &envelope.to_string(),
        )
        .ok()
        .json();
    assert_eq!(out["outcome"], "ignored", "{out:#}");
}

#[test]
fn without_the_dm_entry_a_dm_keeps_the_wildcard_rules() {
    let env = dm_env("dm-unchanged", "[\"*\"]");
    // An unmentioned top-level DM is chatter, as before.
    let quiet = format!("fake:{DM}:{T0}");
    let out = deliver(&env, "Ev01", said(&quiet, T0, ALEX_ID, "hello"), true);
    assert_eq!(out["outcome"], "ignored", "{out:#}");
    assert_eq!(out["why"], "chatter in a thread Fleet does not mirror");
    // A mention there is the wildcard's, without the DM rule.
    let asked = format!("fake:{DM}:{}", T0 + 1_000);
    let out = deliver(
        &env,
        "Ev02",
        said(&asked, T0 + 1_000, ALEX_ID, &format!("<@{BOT}> hi")),
        true,
    );
    assert_eq!(out["outcome"], "written", "{out:#}");
    let conv = conversation_name(GATEWAY_ACCOUNT, &asked);
    let obj = env.fleet(&["get", "conversation", &conv]).ok().json();
    assert_eq!(obj["spec"]["route"], ROUTE, "{obj:#}");
    assert_eq!(obj["spec"].get("direct"), None, "{obj:#}");
}

#[test]
fn a_listed_channel_wins_and_a_group_chat_is_no_dm() {
    let env = dm_env("dm-precedence", "[\"*\", \"dm:*\"]");
    // A channel id another route lists keeps that route's rules, even when
    // the platform calls it a DM.
    let base = section(&env, "[\"*\", \"dm:*\"]").replace(
        &format!("channels = [\"{GATEWAY_CHANNEL}\"]"),
        &format!("channels = [\"{GATEWAY_CHANNEL}\", \"{DM}\"]"),
    );
    env.set_gateway(&base);
    let listed = format!("fake:{DM}:{T0}");
    let out = deliver(&env, "Ev01", said(&listed, T0, ALEX_ID, "hello"), true);
    assert_eq!(out["outcome"], "ignored", "{out:#}");
    let out = deliver(
        &env,
        "Ev02",
        said(&listed, T0 + 1_000, ALEX_ID, &format!("<@{BOT}> hi")),
        true,
    );
    assert_eq!(out["outcome"], "written", "{out:#}");
    let conv = conversation_name(GATEWAY_ACCOUNT, &listed);
    let obj = env.fleet(&["get", "conversation", &conv]).ok().json();
    assert_eq!(obj["spec"]["route"], GATEWAY_ROUTE, "{obj:#}");
    assert_eq!(obj["spec"].get("direct"), None, "{obj:#}");
    // A chat the platform does not call a one-to-one DM, such as a group
    // DM, is a channel: the wildcard covers it under the mention rule.
    let group = format!("fake:G0123GRP:{T0}");
    let out = deliver(&env, "Ev03", said(&group, T0, ALEX_ID, "hello"), false);
    assert_eq!(out["outcome"], "ignored", "{out:#}");
}

#[test]
fn the_config_refuses_a_second_dm_route_and_a_dm_entry_on_discord() {
    let env = dm_env("dm-invalid", "[\"*\", \"dm:*\"]");
    env.fleet(&["list", "tasks"]).ok();
    let base = section(&env, "[\"*\", \"dm:*\"]");
    let discord = base.replacen(
        "[gateway.routes.",
        "[gateway.accounts.discord-main]\nplatform = \"discord\"\nbot = \"1100000000000000001\"\n\
inbound = \"discord-gateway\"\ncurlConfig = \"/etc/fleet/discord-curl.conf\"\n\n[gateway.routes.",
        1,
    );
    let cases: Vec<(String, &str)> = vec![
        (
            format!("{base}{}", route(&env, "second", "[\"dm:*\"]")),
            "gateway.routes: routes randy and second are both the DM route of account \"fake-main\"",
        ),
        (
            base.replace(
                &format!("questions = \"{GATEWAY_ROUTE}\""),
                &format!("questions = \"{ROUTE}\""),
            ),
            "gateway.questions \"randy\" lists no channel id to open a thread in",
        ),
        (
            base.replace("[\"*\", \"dm:*\"]", "[\"dm:*\", \"dm:*\"]"),
            "routes.randy: channel \"dm:*\" is listed twice",
        ),
        (
            format!(
                "{discord}{}",
                route(&env, "dms", "[\"dm:*\"]")
                    .replace(&format!("\"{GATEWAY_ACCOUNT}\""), "\"discord-main\"")
            ),
            "routes.dms: the Discord adapter reads no direct message; \"dm:*\" needs a slack or fake account",
        ),
    ];
    for (section, needle) in cases {
        env.set_gateway(&section);
        env.fleet(&["list", "tasks"]).code(1).stderr_has(needle);
    }
    // A DM-only route needs no mention rule, and may be top-level.
    env.set_gateway(&base.replace("[\"*\", \"dm:*\"]", "[\"dm:*\"]\nmention = \"top-level\""));
    env.fleet(&["list", "tasks"]).ok();
}
