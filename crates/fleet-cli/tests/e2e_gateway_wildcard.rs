//! A wildcard route end to end: `channels = ["*"]` covers every channel of
//! its account that no other route lists and that is not the log channel.
//! The admission rules still hold there, chatter in an unmirrored thread
//! still costs no object, an explicit route still wins its own channels, and
//! the config refuses a second wildcard on one account, a top-level
//! wildcard, and a questions route with no channel id.

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
/// The log channel: no route lists it, and the wildcard never covers it.
const LOGS: &str = "C0456EFGH";
/// A channel that no route lists.
const ELSEWHERE: &str = "C0999WILD";
const WILDCARD_ROUTE: &str = "anywhere";

fn root(env: &Env) -> String {
    env.root.to_string_lossy().replace('\\', "/")
}

/// The wildcard route over the fake account, with its own brief and base
/// directory, covering `channels`.
fn wildcard_route(env: &Env, name: &str, channels: &str) -> String {
    let root = root(env);
    format!(
        "\n[gateway.routes.{name}]\naccount = \"{GATEWAY_ACCOUNT}\"\nchannels = {channels}\nallow = [\"alex\"]\n\
taskConfig = \"conversation\"\nbrief = \"{root}/briefs/anywhere.md\"\nworkspace = {{ base = \"{root}/work/{name}\" }}\n"
    )
}

/// The gateway environment's section with a log channel and the wildcard
/// route beside the explicit route `eng`.
fn section(env: &Env) -> String {
    let base = gateway_section(&env.root).replacen(
        "[gateway]\n",
        &format!("[gateway]\nlogs = {{ account = \"{GATEWAY_ACCOUNT}\", channel = \"{LOGS}\" }}\n"),
        1,
    );
    format!("{base}{}", wildcard_route(env, WILDCARD_ROUTE, "[\"*\"]"))
}

fn wildcard_env(name: &str) -> Env {
    let env = Env::gateway(name);
    fs::write(
        env.root.join("briefs/anywhere.md"),
        "Help anyone who asks.\n",
    )
    .expect("brief");
    env.set_gateway(&section(&env));
    env.apply(support::object(
        "TaskConfig",
        "conversation",
        json!({"harness": "claude-code", "host": "local"}),
    ))
    .ok();
    env
}

fn mention(thread: &str, author: &str, at: u64) -> Value {
    message_event(
        thread,
        &at.to_string(),
        author,
        at,
        &format!("<@{BOT}> can you help?"),
    )
}

/// Put the event's message on the fake platform's thread, as a real
/// platform's history holds it, then hand the event to the receive verb.
fn deliver(env: &Env, delivery: &str, event: Value) -> Value {
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
    env.receive(delivery, event).ok().json()
}

#[test]
fn a_wildcard_route_starts_a_task_in_a_channel_no_route_lists() {
    let env = wildcard_env("wildcard-admits");
    let thread = format!("fake:{ELSEWHERE}:{T0}");
    let out = deliver(&env, "Ev01", mention(&thread, ALEX_ID, T0));
    assert_eq!(out["outcome"], "written", "{out:#}");
    let conv = conversation_name(GATEWAY_ACCOUNT, &thread);
    let obj = env.fleet(&["get", "conversation", &conv]).ok().json();
    assert_eq!(obj["spec"]["route"], WILDCARD_ROUTE, "{obj:#}");
    let report = env.reconcile().ok().json();
    assert_eq!(report["errors"], json!([]), "{report:#}");
    let task = env.get(&format!("{conv}-1"));
    assert_eq!(
        task["metadata"]["labels"]["gateway.fleet.dev/route"], WILDCARD_ROUTE,
        "{task:#}"
    );
    assert_eq!(task["spec"]["brief"], "Help anyone who asks.\n");
    assert_eq!(
        task["spec"]["workspace"][0],
        format!("file://{}/work/{WILDCARD_ROUTE}/{conv}", root(&env))
    );
}

#[test]
fn an_explicit_route_wins_its_channel_and_the_log_channel_stays_uncovered() {
    let env = wildcard_env("wildcard-precedence");
    let listed = format!("fake:{GATEWAY_CHANNEL}:{T0}");
    deliver(&env, "Ev01", mention(&listed, ALEX_ID, T0));
    let conv = conversation_name(GATEWAY_ACCOUNT, &listed);
    let obj = env.fleet(&["get", "conversation", &conv]).ok().json();
    assert_eq!(obj["spec"]["route"], GATEWAY_ROUTE, "{obj:#}");
    // A reply under a log post is never admitted.
    let logged = format!("fake:{LOGS}:{T0}");
    let out = env
        .receive("Ev02", mention(&logged, ALEX_ID, T0))
        .ok()
        .json();
    assert_eq!(out["outcome"], "ignored", "{out:#}");
    assert_eq!(out["why"], format!("no route covers channel {LOGS}"));
    env.fleet(&[
        "get",
        "conversation",
        &conversation_name(GATEWAY_ACCOUNT, &logged),
    ])
    .code(3);
}

#[test]
fn a_wildcard_route_keeps_the_admission_rules_and_costs_nothing_for_chatter() {
    let env = wildcard_env("wildcard-admission");
    // Chatter in a channel only the wildcard covers: no object.
    let quiet = format!("fake:C0777CHAT:{T0}");
    let out = env
        .receive(
            "Ev01",
            message_event(&quiet, &T0.to_string(), ALEX_ID, T0, "lunch?"),
        )
        .ok()
        .json();
    assert_eq!(out["outcome"], "ignored", "{out:#}");
    assert_eq!(out["why"], "chatter in a thread Fleet does not mirror");
    env.fleet(&[
        "get",
        "conversation",
        &conversation_name(GATEWAY_ACCOUNT, &quiet),
    ])
    .code(3);
    // A stranger's mention earns the route's refusal and starts no Task.
    let asked = format!("fake:C0888ASK:{T0}");
    deliver(&env, "Ev02", mention(&asked, "U0999ZZZZ", T0));
    let report = env.reconcile().ok().json();
    assert_eq!(report["errors"], json!([]), "{report:#}");
    let posts = env.fake_gateway().posts().unwrap();
    assert_eq!(posts.len(), 1, "{posts:#?}");
    assert_eq!(posts[0].conversation.address(), asked);
    assert_eq!(posts[0].text, fleet_api::gateway::DEFAULT_REFUSAL);
    let tasks = env.fleet(&["list", "tasks", "--json"]).ok().json();
    assert_eq!(tasks, json!([]), "{tasks:#}");
}

#[test]
fn the_config_refuses_a_second_wildcard_and_a_questions_route_without_a_channel() {
    let env = wildcard_env("wildcard-invalid");
    env.fleet(&["list", "tasks"]).ok();
    let base = section(&env);
    let cases: Vec<(String, &str)> = vec![
        (
            format!("{base}{}", wildcard_route(&env, "everywhere", "[\"*\"]")),
            "gateway.routes: routes anywhere and everywhere are both the wildcard route of account \"fake-main\"",
        ),
        (
            base.replace(
                &format!("questions = \"{GATEWAY_ROUTE}\""),
                &format!("questions = \"{WILDCARD_ROUTE}\""),
            ),
            "gateway.questions \"anywhere\" lists no channel id to open a thread in",
        ),
        (
            base.replace(
                "channels = [\"*\"]",
                "channels = [\"*\"]\nmention = \"top-level\"",
            ),
            "routes.anywhere: a wildcard route needs mention = \"required\"",
        ),
        (
            base.replace("channels = [\"*\"]", "channels = [\"*\", \"*\"]"),
            "routes.anywhere: channel \"*\" is listed twice",
        ),
        (
            base.replace("channels = [\"*\"]", "channels = [\"*\", \" \"]"),
            "routes.anywhere: a channel id is empty",
        ),
    ];
    for (section, needle) in cases {
        env.set_gateway(&section);
        env.fleet(&["list", "tasks"]).code(1).stderr_has(needle);
    }
    // A wildcard route that also lists a channel id can take Questions.
    env.set_gateway(
        &base
            .replace("channels = [\"*\"]", "channels = [\"*\", \"C0789IJKL\"]")
            .replace(
                &format!("questions = \"{GATEWAY_ROUTE}\""),
                &format!("questions = \"{WILDCARD_ROUTE}\""),
            ),
    );
    env.fleet(&["list", "tasks"]).ok();
}
