//! Acceptance for the Gateway's first step: the `[gateway]` section loads,
//! validates and is refused for each invalid case the config page lists,
//! the fake adapter is wired when the manager runs, and the build names the
//! platforms it has. Then the mirror: `fleet gateway receive` creates a
//! Conversation and spools events on it, ConversationController numbers
//! them with the thread's history, dedupes, edits, orphans and retires, and
//! `--beat`, `fleet gateway account` and `fleet gateway status` report each
//! account's listener. And the fifth step: a Slack account is served by the
//! Slack adapter, which reads and writes a thread through the stub `curl`
//! with the operator's curl config named and no credential of its own.
//! Admission, Task creation and GatewayController are later steps.

mod support;

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use fleet_api::Observation;
use fleet_api::gateway::{
    AccountConfig, Author, AuthorKind, Capabilities, ConversationRef, Gateway, HistoryObservation,
    HistoryQuery, Identity, Inbound, Marker, Message, Starter, Uncertainty,
};
use fleet_api::plan::EffectOutcome;
use fleet_gateway::SlackGateway;
use fleet_gateway::slack::POST_LIMIT;
use serde_json::{Value, json};
use support::{
    ALEX_ID, Env, GATEWAY_ACCOUNT, GATEWAY_CHANNEL, GATEWAY_ROUTE, SAM_ID, assert_condition,
    conversation_name, gateway_section, message_event, stub_exe,
};

/// The fake platform's clock, and the thread the scenarios mirror.
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

/// A gateway environment whose fake platform holds one thread with one
/// earlier message from Sam, with the route's TaskConfig applied so the
/// Task an admitted mention starts can run, and the name its Conversation
/// takes.
fn mirror(name: &str) -> (Env, String) {
    let env = Env::gateway(name);
    env.fake_gateway()
        .seed(
            &ConversationRef::split(THREAD).unwrap(),
            vec![person(SAM_ID, T0, "The nightly export failed again.")],
        )
        .unwrap();
    env.apply(task_config()).ok();
    (env, conversation_name(GATEWAY_ACCOUNT, THREAD))
}

/// The TaskConfig the route names.
fn task_config() -> Value {
    support::object(
        "TaskConfig",
        "conversation",
        json!({"harness": "claude-code", "host": "local"}),
    )
}

/// Let a conversation Task's session rest, delete the Task, and reconcile
/// until it is released.
fn delete_task(env: &Env, name: &str) {
    env.host()
        .set_at_rest(&env.session_key(name), true)
        .unwrap();
    env.fleet(&["delete", "task", name]).ok();
    for _ in 0..3 {
        env.reconcile().ok();
        if env.fleet(&["get", "task", name]).code == Some(3) {
            return;
        }
    }
    env.get_absent(name);
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

fn message_ids(st: &Value) -> Vec<(String, u64, u64, u64)> {
    st["messages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| {
            (
                m["id"].as_str().unwrap().to_string(),
                m["seq"].as_u64().unwrap(),
                m["firstSeq"].as_u64().unwrap(),
                m["position"].as_u64().unwrap(),
            )
        })
        .collect()
}

/// The Slack workspace the stub `curl` answers for, and Fleet's bot there.
const SLACK_TEAM: &str = "T0123ABCD";
const SLACK_BOT: &str = "U0BOT0001";

/// A gateway environment with the stub binary copied as `curl` and a curl
/// config file for the account to name. The stub finds its state through
/// the `stub-dir` file beside it, since an adapter driven in-process sets no
/// environment. That directory is the children's whole PATH, so an account
/// without `cli` finds the stub and never a real curl. Fleet never reads
/// the config file: the test asserts only that its path reaches curl.
fn slack_env(name: &str) -> (Env, PathBuf, PathBuf) {
    let mut env = Env::gateway(name);
    let bin = env.root.join("slack-bin");
    env.only_on_path(&bin);
    fs::create_dir_all(&bin).expect("slack bin");
    let exe = if cfg!(windows) { ".exe" } else { "" };
    let curl = bin.join(format!("curl{exe}"));
    fs::copy(stub_exe(), &curl).expect("curl stub");
    fs::create_dir_all(env.stub_dir()).expect("stub dir");
    fs::write(
        bin.join("stub-dir"),
        env.stub_dir().to_string_lossy().as_bytes(),
    )
    .expect("stub-dir");
    let curl_config = env.root.join("slack-curl.conf");
    fs::write(
        &curl_config,
        "# The operator's curl config. The bot token header lives here; Fleet names this file and never reads it.\n",
    )
    .expect("curl config");
    (env, curl, curl_config)
}

/// The gateway section with the fake account replaced by a Slack one on the
/// same name, served by `curl` with `curl_config`, delivering by `inbound`.
fn slack_section(env: &Env, curl: &Path, curl_config: &Path, inbound: &str) -> String {
    let section = gateway_section(&env.root).replace(
        "platform = \"fake\"\nbot = \"B0FLEET001\"\ninbound = \"connection\"\ndir = \"gateway\"\n",
        &format!(
            "platform = \"slack\"\nteam = \"{SLACK_TEAM}\"\nbot = \"{SLACK_BOT}\"\ninbound = \"{inbound}\"\ncli = '{}'\ncurlConfig = \"{}\"\n",
            curl.to_string_lossy(),
            curl_config.to_string_lossy().replace('\\', "/")
        ),
    );
    assert!(
        section.contains("platform = \"slack\""),
        "the fake account was replaced"
    );
    section
}

fn root(env: &Env) -> String {
    env.root.to_string_lossy().replace('\\', "/")
}

/// A second route over the fake account, with its own channel, brief and
/// workspace; a case overrides one field to collide with the first.
fn second_route(env: &Env, channel: &str, dir: &str) -> String {
    let root = root(env);
    format!(
        "\n[gateway.routes.ops]\naccount = \"{GATEWAY_ACCOUNT}\"\nchannels = [\"{channel}\"]\nallow = [\"alex\"]\n\
taskConfig = \"conversation\"\nbrief = \"{root}/briefs/ops.md\"\nworkspace = {{ dir = \"{dir}\" }}\n"
    )
}

#[test]
fn the_gateway_section_loads_and_the_fake_platform_is_wired() {
    let env = Env::gateway("loads");
    env.fleet(&["list", "tasks"]).ok();
    assert!(
        !env.root.join("gateway").exists(),
        "no verb opened the adapter yet"
    );
    env.reconcile().ok();
    assert!(
        env.root.join("gateway").is_dir(),
        "the manager run opened the fake adapter at the account's dir"
    );
    let gateway = env.fake_gateway();
    let calls = gateway.state().unwrap().calls;
    assert_eq!(
        calls.keys().collect::<Vec<_>>(),
        ["capabilities", "whoami"],
        "the account's Listener asked for its identity and nothing else was called: {calls:?}"
    );
    let v = Env::new("gateway-version")
        .fleet(&["version", "--json"])
        .ok()
        .json();
    assert_eq!(v["gateways"], json!(["discord", "fake", "slack"]));
}

#[test]
fn each_invalid_gateway_case_is_refused_at_load() {
    let env = Env::gateway("invalid");
    let base = gateway_section(&env.root);
    let root = root(&env);
    let dir = format!("{root}/work/conversations");
    let cases: Vec<(String, &str)> = vec![
        (
            base.replace(
                &format!("account = \"{GATEWAY_ACCOUNT}\""),
                "account = \"nobody\"",
            ),
            "routes.eng: account \"nobody\" is not under [gateway.accounts]",
        ),
        (
            base.replace("allow = [\"alex\", \"sam\"]", "allow = [\"alex\", \"kim\"]"),
            "routes.eng: allow names \"kim\", which is not under [gateway.people]",
        ),
        (
            format!("{base}\n[gateway.people.kim]\nelsewhere = \"U0789IJKL\"\n"),
            "people.kim: account \"elsewhere\" is not under [gateway.accounts]",
        ),
        (
            format!(
                "{}\n[gateway.people.kim]\nelsewhere = \"U0789IJKL\"\n",
                base.replace(
                    "allow = [\"alex\", \"sam\"]",
                    "allow = [\"alex\", \"kim\"]"
                )
                .replace(
                    &format!("[gateway.accounts.{GATEWAY_ACCOUNT}]"),
                    &format!(
                        "[gateway.accounts.elsewhere]\nplatform = \"fake\"\nbot = \"B0FLEET002\"\ninbound = \"connection\"\ndir = \"elsewhere\"\n\n[gateway.accounts.{GATEWAY_ACCOUNT}]"
                    )
                )
            ),
            "routes.eng: allow names \"kim\", who has no id on account \"fake-main\"",
        ),
        (
            format!("{base}{}", second_route(&env, GATEWAY_CHANNEL, &format!("{root}/work/ops"))),
            "channel \"C0123ABCD\" belongs to routes eng and ops",
        ),
        (
            format!("{base}{}", second_route(&env, "C0789IJKL", &dir)),
            "workspace of route ops equals, contains or lies inside route eng's",
        ),
        (
            format!("{base}{}", second_route(&env, "C0789IJKL", &format!("{root}/work"))),
            "workspace of route ops equals, contains or lies inside route eng's",
        ),
        (
            format!("{base}{}", second_route(&env, "C0789IJKL", &format!("{dir}/deeper"))),
            "workspace of route ops equals, contains or lies inside route eng's",
        ),
        (
            base.replace(
                &format!("questions = \"{GATEWAY_ROUTE}\""),
                "questions = \"nowhere\"",
            ),
            "gateway.questions \"nowhere\" is not under [gateway.routes]",
        ),
        (
            base.replace("mention = \"required\"", "mention = \"always\""),
            "unknown variant `always`",
        ),
        (
            base.replace("inbound = \"connection\"", "inbound = \"carrier-pigeon\""),
            "unknown variant `carrier-pigeon`",
        ),
        (
            base.replace("inbound = \"connection\"\n", ""),
            "missing field `inbound`",
        ),
        (
            base.replace(
                &format!("brief = \"{root}/briefs/conversation.md\""),
                "brief = \"briefs/conversation.md\"",
            ),
            "routes.eng: brief must be an absolute path",
        ),
        (
            base.replace(
                &format!("workspace = {{ dir = \"{dir}\" }}"),
                "workspace = { dir = \"work/conversations\" }",
            ),
            "routes.eng: workspace.dir must be an absolute path",
        ),
        (
            base.replace("allow = [\"alex\", \"sam\"]", "allow = []"),
            "routes.eng: the route admits nobody",
        ),
        (
            base.replace(
                &format!("channels = [\"{GATEWAY_CHANNEL}\"]"),
                "channels = []",
            ),
            "routes.eng: the route covers no channel",
        ),
        (
            base.replace("mention = \"required\"", "colour = \"red\""),
            "unknown field `colour`",
        ),
        (
            base.replace("callTimeoutMs = 3000", "retainMs = 0"),
            "gateway.retainMs must be at least 1",
        ),
        (
            base.replace(
                &format!("[gateway.people.alex]\n{GATEWAY_ACCOUNT} = \"{ALEX_ID}\""),
                &format!("[gateway.people.alex]\n{GATEWAY_ACCOUNT} = \"\""),
            ),
            "people.alex: the id on account \"fake-main\" is empty",
        ),
    ];
    for (section, needle) in cases {
        env.set_gateway(&section);
        env.fleet(&["list", "tasks"]).code(1).stderr_has(needle);
    }
    // The valid section loads again.
    env.set_gateway(&base);
    env.fleet(&["list", "tasks"]).ok();
}

#[test]
fn an_account_this_build_cannot_serve_is_refused_when_the_gateway_opens() {
    let env = Env::gateway("platform");
    let base = gateway_section(&env.root);
    let telegram = base.replace(
        "platform = \"fake\"\nbot = \"B0FLEET001\"\ninbound = \"connection\"\ndir = \"gateway\"\n",
        "platform = \"telegram\"\nbot = \"7000000001\"\ninbound = \"connection\"\n",
    );
    assert_ne!(telegram, base);
    env.set_gateway(&telegram);
    // The section is well formed, so every verb but the manager's runs.
    env.fleet(&["list", "tasks"]).ok();
    env.reconcile().code(1).stderr_has(
        "accounts.fake-main: unknown platform \"telegram\"; this build has discord, fake, slack",
    );
    env.set_gateway(&base.replace("dir = \"gateway\"\n", ""));
    env.reconcile()
        .code(1)
        .stderr_has("accounts.fake-main: a fake account needs a dir");
    env.set_gateway(&base.replace("inbound = \"connection\"", "inbound = \"socket-mode\""));
    env.reconcile().code(1).stderr_has(
        "accounts.fake-main: inbound \"socket-mode\" is not how a fake listener delivers",
    );
    env.set_gateway(&base);
    env.reconcile().ok();
}

#[test]
fn a_mention_creates_the_conversation_and_the_first_pass_numbers_it_with_the_history() {
    let (env, conv) = mirror("mirror");
    let out = env
        .receive("Ev01", mention("1730000300000", T0 + 300_000))
        .ok()
        .json();
    assert_eq!(out["outcome"], "written", "{out:#}");
    assert_eq!(out["conversation"], conv);
    let obj = env.fleet(&["get", "conversation", &conv]).ok().json();
    assert_eq!(
        obj["spec"],
        json!({"account": GATEWAY_ACCOUNT, "address": THREAD, "route": GATEWAY_ROUTE})
    );
    assert_eq!(obj["status"]["received"].as_array().unwrap().len(), 1);
    assert_eq!(obj["status"]["received"][0]["id"], "Ev01");

    let report = env.reconcile().ok().json();
    assert_eq!(report["errors"], json!([]), "{report:#}");
    let st = env.conversation_status(&conv);
    assert_eq!(st["phase"], "Tied", "the mention started a Task: {st:#}");
    assert_eq!(
        st["ref"],
        json!({"platform": "fake", "chat": GATEWAY_CHANNEL, "thread": "1730000000000"})
    );
    assert_eq!(st["revision"], 1);
    assert_eq!(
        message_ids(&st),
        vec![
            ("1730000000000".to_string(), 1, 1, 0),
            ("1730000300000".to_string(), 1, 1, 1),
        ],
        "the history and the event are numbered in one revision, oldest first: {st:#}"
    );
    assert_eq!(st["messages"][0]["via"]["verified"], "history");
    assert_eq!(st["messages"][0]["author"]["id"], SAM_ID);
    assert_eq!(st["messages"][1]["via"]["verified"], "connection");
    assert_eq!(st["messages"][1]["mentionsFleet"], true);
    assert_eq!(
        st["starter"],
        json!({"id": "1730000000000", "kind": "person"})
    );
    assert_eq!(
        st["received"],
        json!([]),
        "the spool was drained in the same write"
    );
    assert_eq!(st["seenEvents"], json!(["Ev01"]));
    assert_eq!(st["droppedBefore"], 0);
    assert_eq!(st["lastMessageAt"], T0 + 300_000);
    assert_condition(&st, "Reachable", "True", "Observed");
    assert_condition(&st, "Tied", "True", "TaskKnown");
    assert!(
        st["notBefore"].as_u64().unwrap() > st["observedAt"].as_u64().unwrap(),
        "the backstop is the next look: {st:#}"
    );
    env.fleet(&["status", "conversation", &conv])
        .ok()
        .stdout_has("revision 1")
        .stdout_has("2 message(s)");
    env.fleet(&["list", "conversations"])
        .ok()
        .stdout_has("Tied");
    assert_eq!(
        env.fake_gateway().calls("history").unwrap(),
        1,
        "one first read"
    );
    // Quiet: the thread is not read again before its backstop interval.
    env.reconcile().ok();
    assert_eq!(env.conversation_status(&conv), st);
    env.assert_no_temp();
}

#[test]
fn a_redelivered_event_changes_nothing_and_edits_and_removals_move_the_mirror() {
    let (env, conv) = mirror("dedupe");
    env.receive("Ev01", mention("1730000300000", T0 + 300_000))
        .ok();
    env.reconcile().ok();
    let before = env.conversation_status(&conv);
    let again = env
        .receive("Ev01", mention("1730000300000", T0 + 300_000))
        .ok()
        .json();
    assert_eq!(again["outcome"], "duplicate", "{again:#}");
    assert_eq!(env.conversation_status(&conv), before);
    // Redelivered and numbered from history too: still one message.
    env.receive(
        "Ev02",
        message_event(
            THREAD,
            "1730000000000",
            SAM_ID,
            T0,
            "The nightly export failed again.",
        ),
    )
    .ok();
    env.reconcile().ok();
    let st = env.conversation_status(&conv);
    assert_eq!(st["revision"], 1, "nothing new was numbered: {st:#}");
    assert_eq!(st["messages"].as_array().unwrap().len(), 2);
    assert_eq!(st["seenEvents"], json!(["Ev01", "Ev02"]));

    // A newer edit moves the message's number; an older one changes nothing.
    let edited = |id: &str, text: &str, edited_at: u64| {
        json!({"type": "edited", "conversation": {"platform": "fake", "chat": GATEWAY_CHANNEL, "thread": "1730000000000"},
               "id": id, "text": text, "editedAt": edited_at})
    };
    env.receive(
        "Ev03",
        edited(
            "1730000300000",
            "can you look into the export?",
            T0 + 400_000,
        ),
    )
    .ok();
    env.reconcile().ok();
    let st = env.conversation_status(&conv);
    assert_eq!(st["revision"], 2);
    assert_eq!(
        message_ids(&st)[1],
        ("1730000300000".to_string(), 2, 1, 1),
        "seq moved, firstSeq and position did not: {st:#}"
    );
    assert_eq!(st["messages"][1]["text"], "can you look into the export?");
    assert_eq!(st["messages"][1]["editedAt"], T0 + 400_000);
    env.receive("Ev04", edited("1730000300000", "stale text", T0 + 350_000))
        .ok();
    env.reconcile().ok();
    let st = env.conversation_status(&conv);
    assert_eq!(
        st["revision"], 2,
        "an older edit delivered late changes nothing"
    );
    assert_eq!(st["messages"][1]["text"], "can you look into the export?");

    // A removal leaves the mirror.
    env.receive(
        "Ev05",
        json!({"type": "removed", "conversation": {"platform": "fake", "chat": GATEWAY_CHANNEL, "thread": "1730000000000"},
               "id": "1730000000000"}),
    )
    .ok();
    env.reconcile().ok();
    let st = env.conversation_status(&conv);
    assert_eq!(st["revision"], 3);
    assert_eq!(message_ids(&st).len(), 1);
    assert_eq!(st["starter"], json!({"removed": true}), "{st:#}");
    env.assert_no_temp();
}

#[test]
fn the_spool_holds_a_hundred_events_and_refuses_the_next() {
    let (env, conv) = mirror("spool");
    env.reconcile().ok();
    for i in 0..100 {
        let at = T0 + 300_000 + i * 1000;
        let text = if i == 0 {
            format!("<@{BOT}> look")
        } else {
            format!("message {i}")
        };
        env.receive(
            &format!("Ev{i:03}"),
            message_event(THREAD, &at.to_string(), ALEX_ID, at, &text),
        )
        .ok();
    }
    let at = T0 + 999_000;
    env.receive(
        "Ev100",
        message_event(THREAD, &at.to_string(), ALEX_ID, at, "one too many"),
    )
    .code(1)
    .stderr_has("holds 100 unread events");
    let listener = env.listener_status(&format!("listener-{GATEWAY_ACCOUNT}"));
    assert_eq!(
        listener["refused"]["counts"]["SpoolFull"], 1,
        "{listener:#}"
    );
    assert!(
        listener["refused"]["lastFaultAt"].is_null(),
        "a full queue is one thread's trouble, not the listener's fault: {listener:#}"
    );
    env.reconcile().ok();
    let st = env.conversation_status(&conv);
    assert_eq!(st["revision"], 1);
    assert_eq!(st["messages"].as_array().unwrap().len(), 101, "{st:#}");
    assert_eq!(st["received"], json!([]));
    assert_eq!(st["seenEvents"].as_array().unwrap().len(), 100);
    env.receive(
        "Ev100",
        message_event(THREAD, &at.to_string(), ALEX_ID, at, "one too many"),
    )
    .ok();
}

#[test]
fn refusals_and_ignored_events_each_answer_as_the_interface_says() {
    let (env, conv) = mirror("refused");
    env.reconcile().ok();
    let listener = format!("listener-{GATEWAY_ACCOUNT}");
    env.fleet_stdin(
        &["gateway", "receive", "--account", "nobody"],
        &json!({"verified": "connection", "receivedAt": 1, "payload": {}}).to_string(),
    )
    .code(1)
    .stderr_has("account \"nobody\" is not configured under [gateway.accounts]");
    // The wrong `verified` is the listener's own fault.
    env.fleet_stdin(
        &["gateway", "receive", "--account", GATEWAY_ACCOUNT],
        &json!({"verified": "signature", "receivedAt": 1, "payload": {"id": "Ev01", "event": mention("1730000300000", T0 + 300_000)}}).to_string(),
    )
    .code(1)
    .stderr_has("verified \"signature\" is not how account fake-main's listener delivers");
    let st = env.listener_status(&listener);
    assert_eq!(st["refused"]["counts"]["WrongVerified"], 1, "{st:#}");
    assert!(st["refused"]["lastFaultAt"].is_u64());
    // Not an envelope: the listener's fault too, and Unknown.
    env.fleet_stdin(
        &["gateway", "receive", "--account", GATEWAY_ACCOUNT],
        "not json",
    )
    .code(4);
    let st = env.listener_status(&listener);
    assert_eq!(st["refused"]["counts"]["NotEnvelope"], 1, "{st:#}");
    // A payload the adapter cannot read: Unknown, one thread's trouble.
    env.fleet_stdin(
        &["gateway", "receive", "--account", GATEWAY_ACCOUNT],
        &json!({"verified": "connection", "receivedAt": 1, "payload": {"id": "Ev02", "event": {"type": "wave"}}}).to_string(),
    )
    .code(4);
    let st = env.listener_status(&listener);
    assert_eq!(st["refused"]["counts"]["UnreadablePayload"], 1, "{st:#}");
    assert_eq!(st["beatRevision"], 3);
    env.reconcile().ok();
    assert_condition(
        &env.listener_status(&listener),
        "InboundReady",
        "False",
        "NoListener",
    );
    // Ignored, with exit 0 and no Conversation: a reaction, a channel no
    // route covers, and chatter in a thread Fleet does not mirror.
    let ignored = |delivery: &str, payload: Value| -> Value {
        env.fleet_stdin(
            &["gateway", "receive", "--account", GATEWAY_ACCOUNT, "--json"],
            &json!({"verified": "connection", "receivedAt": 1, "payload": {"id": delivery, "event": payload}}).to_string(),
        )
        .ok()
        .json()
    };
    let out = env
        .fleet_stdin(
            &["gateway", "receive", "--account", GATEWAY_ACCOUNT, "--json"],
            &json!({"verified": "connection", "receivedAt": 1, "payload": {"id": "Ev03", "reaction": "eyes"}}).to_string(),
        )
        .ok()
        .json();
    assert_eq!(out["outcome"], "ignored", "{out:#}");
    let out = ignored(
        "Ev04",
        message_event(
            "fake:C0789IJKL:1",
            "1",
            ALEX_ID,
            T0,
            &format!("<@{BOT}> hi"),
        ),
    );
    assert_eq!(out["outcome"], "ignored");
    assert_eq!(out["why"], "no route covers channel C0789IJKL");
    let out = ignored(
        "Ev05",
        message_event(
            THREAD,
            "1730000300000",
            ALEX_ID,
            T0 + 300_000,
            "no mention here",
        ),
    );
    assert_eq!(out["outcome"], "ignored");
    assert_eq!(out["why"], "chatter in a thread Fleet does not mirror");
    env.fleet(&["get", "conversation", &conv]).code(3);
    // A mention from someone not on the allowlist earns a refusal, so it
    // creates the Conversation.
    let out = ignored(
        "Ev06",
        message_event(
            THREAD,
            "1730000300000",
            "U0999ZZZZ",
            T0 + 300_000,
            &format!("<@{BOT}> hi"),
        ),
    );
    assert_eq!(out["outcome"], "written", "{out:#}");
    env.fleet(&["get", "conversation", &conv]).ok();
    // Once mirrored, chatter is spooled too.
    let out = ignored(
        "Ev07",
        message_event(
            THREAD,
            "1730000400000",
            ALEX_ID,
            T0 + 400_000,
            "no mention here",
        ),
    );
    assert_eq!(out["outcome"], "written");
    // A Conversation being deleted takes nothing.
    env.fleet(&["delete", "conversation", &conv])
        .ok()
        .stdout_has(&format!("Conversation/{conv} deleted"));
    env.fleet(&["get", "conversation", &conv]).code(3);
    assert_eq!(
        env.fake_gateway().calls("post").unwrap(),
        0,
        "nothing on the platform changes"
    );
}

#[test]
fn a_beat_is_recorded_on_the_listener_and_the_account_verbs_report_it() {
    let env = Env::gateway("beat");
    let v = Env::new("gateway-kinds")
        .fleet(&["version", "--json"])
        .ok()
        .json();
    for kind in ["Conversation", "Listener"] {
        assert!(v["kinds"].as_array().unwrap().contains(&json!(kind)));
    }
    let account = env
        .fleet(&["gateway", "account", GATEWAY_ACCOUNT, "--json"])
        .ok()
        .json();
    assert_eq!(account["name"], GATEWAY_ACCOUNT);
    assert_eq!(account["platform"], "fake");
    assert_eq!(account["bot"], BOT);
    assert_eq!(account["inbound"], "connection");
    let digest = account["digest"].as_str().unwrap().to_string();
    assert!(digest.starts_with("sha256:") && digest.len() == 7 + 64);
    env.fleet(&["gateway", "account", "nobody"])
        .code(1)
        .stderr_has("account \"nobody\" is not configured");
    let listener = format!("listener-{GATEWAY_ACCOUNT}");
    // Before the manager ever ran: the verb creates the Listener.
    let out = env.beat("a1", "connected", &digest).ok().json();
    assert_eq!(out["outcome"], "beat", "{out:#}");
    let obj = env.fleet(&["get", "listener", &listener]).ok().json();
    assert_eq!(obj["spec"], json!({"account": GATEWAY_ACCOUNT}));
    let beat = &obj["status"]["beats"]["a1"];
    assert_eq!(beat["state"], "connected");
    assert_eq!(beat["digest"], digest);
    assert!(
        beat["at"].as_u64().unwrap() > 0,
        "stamped by the verb's clock"
    );
    assert!(beat["sentAt"].as_u64().unwrap() > 0);
    assert_eq!(obj["status"]["beatRevision"], 1);
    env.fleet_stdin(
        &["gateway", "receive", "--account", "nobody", "--beat"],
        &json!({"instance": "a1", "state": "connected", "digest": digest, "sentAt": 1}).to_string(),
    )
    .code(1);
    env.fleet_stdin(
        &["gateway", "receive", "--account", GATEWAY_ACCOUNT, "--beat"],
        "{\"instance\": \"a1\"}",
    )
    .code(4);
    assert_eq!(
        env.listener_status(&listener)["refused"]["counts"]["NotEnvelope"],
        1
    );

    let report = env.reconcile().ok().json();
    assert_eq!(report["errors"], json!([]), "{report:#}");
    let st = env.listener_status(&listener);
    assert_eq!(st["accepted"]["digest"], digest, "{st:#}");
    assert_condition(&st, "Accepted", "True", "Loaded");
    assert_condition(&st, "Resolved", "True", "IdentityMatches");
    assert_condition(&st, "InboundReady", "False", "DeliveriesRefused");
    assert_eq!(st["seen"], 2);
    env.fleet(&["gateway", "status"])
        .ok()
        .stdout_has(GATEWAY_ACCOUNT)
        .stdout_has("InboundReady False DeliveriesRefused");
    let rows = env.fleet(&["gateway", "status", "--json"]).ok().json();
    assert_eq!(rows[0]["account"], GATEWAY_ACCOUNT);
    assert_eq!(rows[0]["inbound"]["reason"], "DeliveriesRefused");
    assert_eq!(rows[0]["lastDeliveryAt"], Value::Null);
    // Time passes on the record, not on the clock: the fault ages out, then
    // the beat goes stale.
    let day = 86_400_000u64;
    let age = |f: &dyn Fn(&mut Value)| {
        let path = env.object_path("Listener", &listener);
        let mut obj: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        f(&mut obj["status"]);
        obj["status"]["notBefore"] = json!(0);
        fs::write(&path, obj.to_string()).unwrap();
    };
    age(&|st| {
        let at = st["refused"]["lastFaultAt"].as_u64().unwrap();
        st["refused"]["lastFaultAt"] = json!(at - day);
    });
    env.reconcile().ok();
    assert_condition(
        &env.listener_status(&listener),
        "InboundReady",
        "True",
        "Connected",
    );
    age(&|st| {
        let at = st["beats"]["a1"]["at"].as_u64().unwrap();
        st["beats"]["a1"]["at"] = json!(at - day);
    });
    env.reconcile().ok();
    assert_condition(
        &env.listener_status(&listener),
        "InboundReady",
        "False",
        "ListenerStale",
    );
    env.beat("a1", "connected", &digest).ok();
    env.reconcile().ok();
    assert_condition(
        &env.listener_status(&listener),
        "InboundReady",
        "True",
        "Connected",
    );
    env.beat("a2", "connected", "sha256:old").ok();
    env.beat("a1", "closing", &digest).ok();
    env.reconcile().ok();
    let st = env.listener_status(&listener);
    assert_condition(&st, "InboundReady", "False", "Closing");
    assert_eq!(st["beats"].as_object().unwrap().len(), 2);
    age(&|st| {
        let at = st["beats"]["a1"]["at"].as_u64().unwrap();
        st["beats"]["a1"]["at"] = json!(at - day);
    });
    env.beat("a2", "connecting", "sha256:old").ok();
    env.reconcile().ok();
    assert_condition(
        &env.listener_status(&listener),
        "InboundReady",
        "False",
        "ListenerConfigOld",
    );
    // The platform's identity is checked against the configured bot, every
    // pollMs.
    env.fake_gateway()
        .set_identity(fleet_api::gateway::Identity {
            id: "B0OTHER".into(),
            name: None,
        })
        .unwrap();
    age(&|st| st["observedAt"] = json!(0));
    env.reconcile().ok();
    assert_condition(
        &env.listener_status(&listener),
        "Resolved",
        "False",
        "IdentityMismatch",
    );
    // An account that left the config loses its Listener.
    env.set_gateway(&gateway_section(&env.root).replace(
        &format!("[gateway.accounts.{GATEWAY_ACCOUNT}]"),
        &format!(
            "[gateway.accounts.other]\nplatform = \"fake\"\nbot = \"B0FLEET002\"\ninbound = \"connection\"\ndir = \"gateway-other\"\n\n[gateway.accounts.{GATEWAY_ACCOUNT}]"
        ),
    ));
    env.reconcile().ok();
    env.fleet(&["get", "listener", "listener-other"]).ok();
    assert_condition(
        &env.listener_status("listener-other"),
        "Accepted",
        "False",
        "NoRoute",
    );
    env.set_gateway(&gateway_section(&env.root));
    env.expire_wait_of("Listener", "listener-other");
    env.reconcile().ok();
    env.fleet(&["get", "listener", "listener-other"]).code(3);
    env.fleet(&["get", "listener", &listener]).ok();
}

#[test]
fn an_orphaned_conversation_keeps_its_messages_and_is_retired_after_its_retention() {
    let (env, conv) = mirror("orphan");
    env.receive("Ev01", mention("1730000300000", T0 + 300_000))
        .ok();
    env.reconcile().ok();
    let task_name = format!("{conv}-1");
    let path = env.object_path("Conversation", &conv);
    let st = env.conversation_status(&conv);
    assert_eq!(st["phase"], "Tied", "{st:#}");
    assert_condition(&st, "Tied", "True", "TaskKnown");
    assert_eq!(st["task"]["state"], "created");
    // A Task the store cannot read keeps the thread Tied: the manager
    // reports the unreadable record and decides nothing from it.
    let task_path = env.object_path("Task", &task_name);
    let bytes = fs::read(&task_path).unwrap();
    fs::write(&task_path, &bytes[..bytes.len() / 2]).unwrap();
    env.expire_wait_of("Conversation", &conv);
    let report = env.reconcile().ok().json();
    assert!(
        report["unknowns"]
            .as_array()
            .unwrap()
            .iter()
            .any(|u| u.as_str().unwrap().contains(&task_name)),
        "{report:#}"
    );
    assert_eq!(env.conversation_status(&conv)["phase"], "Tied");
    fs::write(&task_path, &bytes).unwrap();
    // Readably gone: Orphaned, the messages kept through a retention that
    // counts from the orphaning, not from the thread's last message.
    delete_task(&env, &task_name);
    env.expire_wait_of("Conversation", &conv);
    env.reconcile().ok();
    let st = env.conversation_status(&conv);
    assert_eq!(st["phase"], "Orphaned", "{st:#}");
    assert_condition(&st, "Tied", "False", "TaskGone");
    assert_eq!(st["task"]["state"], "gone");
    assert_eq!(st["messages"].as_array().unwrap().len(), 2);
    let orphaned_at = support::condition(&st, "Tied")["lastTransitionTime"]
        .as_u64()
        .unwrap();
    let retain = 2_592_000_000u64;
    assert_eq!(st["notBefore"], orphaned_at + retain, "{st:#}");
    env.expire_wait_of("Conversation", &conv);
    env.reconcile().ok();
    env.fleet(&["get", "conversation", &conv]).ok();
    // Time passes on the record: the retention runs out, and the object is
    // retired by uid and resourceVersion.
    let mut obj: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    let old = orphaned_at - retain - 1;
    obj["metadata"]["creationTimestamp"] = json!(old);
    obj["status"]["lastMessageAt"] = json!(old);
    for c in obj["status"]["conditions"].as_array_mut().unwrap() {
        if c["type"] == "Tied" {
            c["lastTransitionTime"] = json!(old);
        }
    }
    obj["status"]["notBefore"] = json!(0);
    fs::write(&path, obj.to_string()).unwrap();
    let report = env.reconcile().ok().json();
    assert_eq!(report["errors"], json!([]), "{report:#}");
    env.fleet(&["get", "conversation", &conv]).code(3);
    assert_eq!(env.fake_gateway().calls("post").unwrap(), 0);
    // A later message there is a new thread: the mention rule applies again.
    let out = env
        .receive(
            "Ev02",
            message_event(
                THREAD,
                "1730000500000",
                ALEX_ID,
                T0 + 500_000,
                "still broken",
            ),
        )
        .ok()
        .json();
    assert_eq!(out["outcome"], "ignored", "{out:#}");
    env.receive("Ev03", mention("1730000600000", T0 + 600_000))
        .ok();
    env.fleet(&["get", "conversation", &conv]).ok();
}

#[test]
fn manifests_follow_the_write_ownership_rules() {
    let (env, conv) = mirror("manifests");
    env.apply(json!({"apiVersion": "fleet/v1alpha1", "kind": "Listener",
        "metadata": {"name": "listener-fake-main"}, "spec": {"account": GATEWAY_ACCOUNT}}))
        .code(1)
        .stderr_has("the config is the only source");
    // A Conversation by manifest takes its derived name, or none.
    let conversation = |name: Option<&str>, address: &str| {
        let mut metadata = json!({});
        if let Some(n) = name {
            metadata["name"] = json!(n);
        }
        json!({"apiVersion": "fleet/v1alpha1", "kind": "Conversation", "metadata": metadata,
               "spec": {"account": GATEWAY_ACCOUNT, "address": address, "route": GATEWAY_ROUTE}})
    };
    env.apply(conversation(Some("conv-000000000000"), THREAD))
        .code(1)
        .stderr_has("names one object");
    env.apply(conversation(None, "fake:C0123ABCD:nope nope"))
        .code(1)
        .stderr_has("is not one token");
    let mut wrong_route = conversation(None, THREAD);
    wrong_route["spec"]["route"] = json!("nowhere");
    env.apply(wrong_route)
        .code(1)
        .stderr_has("route \"nowhere\" is not under [gateway.routes]");
    env.apply(conversation(None, THREAD))
        .ok()
        .stdout_has(&format!("Conversation/{conv} created"));
    env.apply(conversation(Some(&conv), THREAD))
        .ok()
        .stdout_has(&format!("Conversation/{conv} unchanged"));
    // One thread's derived name with another's address is refused, and so
    // is any change to the spec once created.
    env.apply(conversation(Some(&conv), "fake:C0123ABCD:2"))
        .code(1)
        .stderr_has("names one object");
    let mut other = conversation(Some(&conv), THREAD);
    other["spec"]["pollMs"] = json!(5000);
    env.apply(other).code(1).stderr_has("immutable");
    env.reconcile().ok();
    let st = env.conversation_status(&conv);
    assert_eq!(
        st["revision"], 1,
        "the history alone numbers the thread: {st:#}"
    );
    assert_eq!(st["messages"].as_array().unwrap().len(), 1);
    // A manifest may not carry status, and the verb refuses a Task label it owns.
    let mut with_status = conversation(None, THREAD);
    with_status["status"] = json!({"phase": "Tied"});
    env.apply(with_status)
        .code(1)
        .stderr_has("status is written by controllers");
    let mut labelled = support::task("labelled", false);
    labelled["metadata"]["labels"]["gateway.fleet.dev/conversation"] = json!(conv);
    env.apply(labelled)
        .code(1)
        .stderr_has("gateway.fleet.dev/conversation");
}

#[test]
fn a_slack_account_is_served_by_curl_and_refused_without_what_curl_needs() {
    let (env, curl, curl_config) = slack_env("slack-config");
    let base = slack_section(&env, &curl, &curl_config, "socket-mode");
    env.set_gateway(&base);
    env.fleet(&["list", "tasks"]).ok();
    assert!(
        env.stub_calls().is_empty(),
        "no verb but the manager's opens the adapter: {:?}",
        env.stub_calls()
    );
    let account = env
        .fleet(&["gateway", "account", GATEWAY_ACCOUNT, "--json"])
        .ok()
        .json();
    assert_eq!(account["platform"], "slack");
    assert_eq!(account["bot"], SLACK_BOT);
    assert_eq!(account["inbound"], "socket-mode");
    // The manager run resolves the account's Listener: the adapter asks
    // Slack who it is, through the stub curl, and nothing else.
    let report = env.reconcile().ok().json();
    assert_eq!(report["errors"], json!([]), "{report:#}");
    let calls = env.stub_calls();
    assert!(!calls.is_empty(), "the Listener asked for Fleet's identity");
    for call in &calls {
        let argv = call["argv"].as_array().unwrap();
        assert_eq!(
            argv.last().unwrap(),
            &json!("https://slack.com/api/auth.test"),
            "{call:#}"
        );
        // The path as the section spells it, with forward slashes.
        assert!(argv.contains(&json!(curl_config.to_string_lossy().replace('\\', "/"))));
    }
    let listener = format!("listener-{GATEWAY_ACCOUNT}");
    let st = env.listener_status(&listener);
    assert_condition(&st, "Accepted", "True", "Loaded");
    assert_condition(&st, "Resolved", "True", "IdentityMatches");
    // The identity is asked again every pollMs: make the next poll due.
    let poll_due = || {
        let path = env.object_path("Listener", &listener);
        let mut obj: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        obj["status"]["observedAt"] = json!(0);
        obj["status"]["notBefore"] = json!(0);
        fs::write(&path, obj.to_string()).unwrap();
    };
    // A bot id that is not what `auth.test` answers is a mismatch, not a crash.
    env.set_gateway(&base.replace(&format!("bot = \"{SLACK_BOT}\""), "bot = \"U0OTHER001\""));
    poll_due();
    env.reconcile().ok();
    let st = env.listener_status(&listener);
    assert_condition(&st, "Resolved", "False", "IdentityMismatch");
    env.set_gateway(&base);
    let v = Env::new("slack-version")
        .fleet(&["version", "--json"])
        .ok()
        .json();
    assert_eq!(v["gateways"], json!(["discord", "fake", "slack"]));
    // Either Slack delivery method is a listener Slack has; the fake's is not.
    env.set_gateway(&slack_section(&env, &curl, &curl_config, "events-api"));
    env.reconcile().ok();
    env.set_gateway(&slack_section(&env, &curl, &curl_config, "connection"));
    env.reconcile().code(1).stderr_has(
        "accounts.fake-main: inbound \"connection\" is not how a Slack listener delivers; use \"socket-mode\" or \"events-api\"",
    );
    // The curl config is what carries the credential, so it is required; the client defaults to `curl` on PATH.
    let without_config = base.replace(
        &format!(
            "curlConfig = \"{}\"\n",
            curl_config.to_string_lossy().replace('\\', "/")
        ),
        "",
    );
    assert_ne!(without_config, base);
    env.set_gateway(&without_config);
    env.reconcile()
        .code(1)
        .stderr_has("accounts.fake-main: a slack account needs a curlConfig");
    // Without `cli`, `curl` is found on PATH: the stub, since that is all
    // the children's PATH holds. Not on Windows, where process creation
    // searches the system directories before PATH and finds the system's
    // own curl.exe, which a test must not run.
    if cfg!(windows) {
        return;
    }
    let without_cli = base.replace(&format!("cli = '{}'\n", curl.to_string_lossy()), "");
    assert_ne!(without_cli, base);
    env.set_gateway(&without_cli);
    let before = env.stub_calls().len();
    poll_due();
    env.reconcile().ok();
    assert!(
        env.stub_calls().len() > before,
        "the stub curl on PATH served the Listener"
    );
    assert_condition(
        &env.listener_status(&listener),
        "Resolved",
        "True",
        "IdentityMatches",
    );
}

/// The thread the stub Slack workspace starts with: Alex's mention and
/// Sam's reply, beside an unrelated top-level post in the same channel.
const PARENT_TS: &str = "1730000000.000100";
const REPLY_TS: &str = "1730000060.000200";
const OTHER_TS: &str = "1730000100.000000";

fn seed_slack(env: &Env) {
    env.edit_stub(|s| {
        s["slack"] = json!({
            "team": SLACK_TEAM, "botUser": SLACK_BOT, "botId": "B0FLEET001", "botName": "fleet",
            "clock": 1_730_000_500u64,
            "messages": {GATEWAY_CHANNEL: [
                {"type": "message", "ts": PARENT_TS, "user": ALEX_ID, "text": format!("<@{SLACK_BOT}> please look"),
                 "thread_ts": PARENT_TS, "reply_count": 1},
                {"type": "message", "ts": REPLY_TS, "user": SAM_ID, "text": "here is the log", "thread_ts": PARENT_TS},
                {"type": "message", "ts": OTHER_TS, "user": SAM_ID, "text": "an unrelated top-level post"},
            ]},
        });
    });
}

/// The adapter as the manager would build it for the account, driven
/// in-process against the stub `curl`.
fn slack_adapter(curl: &Path, curl_config: &Path, timeout: Duration) -> SlackGateway {
    SlackGateway::new(
        GATEWAY_ACCOUNT,
        &AccountConfig {
            platform: "slack".into(),
            team: Some(SLACK_TEAM.into()),
            bot: SLACK_BOT.into(),
            inbound: Inbound::SocketMode,
            message_content: None,
            cli: Some(curl.to_string_lossy().into_owned()),
            curl_config: Some(curl_config.to_path_buf()),
            dir: None,
        },
        timeout,
    )
    .expect("a slack adapter")
}

fn thread(ts: &str) -> ConversationRef {
    ConversationRef {
        platform: "slack".into(),
        chat: GATEWAY_CHANNEL.into(),
        thread: Some(ts.into()),
    }
}

fn ids(h: &HistoryObservation) -> Vec<String> {
    h.known()
        .unwrap_or_else(|| panic!("{h:?}"))
        .messages
        .iter()
        .map(|m| m.id.clone())
        .collect()
}

fn slack_posts(env: &Env) -> Vec<Value> {
    env.stub()["slack"]["posts"]
        .as_array()
        .cloned()
        .unwrap_or_default()
}

fn slack_calls(env: &Env) -> Vec<Value> {
    env.stub()["slack"]["calls"]
        .as_array()
        .cloned()
        .unwrap_or_default()
}

#[test]
fn the_slack_adapter_reads_identity_capabilities_and_a_thread_through_the_stub_curl() {
    let (env, curl, curl_config) = slack_env("slack-reads");
    seed_slack(&env);
    let g = slack_adapter(&curl, &curl_config, Duration::from_secs(30));
    assert_eq!(
        g.whoami(GATEWAY_ACCOUNT),
        Observation::Known(Identity {
            id: SLACK_BOT.into(),
            name: Some("fleet".into())
        })
    );
    assert_eq!(
        g.capabilities(GATEWAY_ACCOUNT),
        Observation::Known(Capabilities {
            history: true,
            marker: Marker::Metadata,
            unmentioned: true,
            threads: true
        })
    );
    let t = thread(PARENT_TS);
    let first = HistoryQuery::After {
        id: None,
        limit: 200,
    };
    let h = g.history(GATEWAY_ACCOUNT, &t, &first);
    assert_eq!(ids(&h), [PARENT_TS, REPLY_TS]);
    let known = h.known().unwrap();
    assert_eq!(known.left_out, 0);
    assert_eq!(
        known.starter,
        Some(Starter {
            id: Some(PARENT_TS.into()),
            kind: Some(AuthorKind::Person),
            key: None,
            removed: false
        })
    );
    assert!(known.messages[0].mentions_fleet && known.messages[0].starts_thread);
    assert_eq!(known.messages[1].author.id, SAM_ID);
    assert!(!known.messages[1].starts_thread);
    // Paged one message at a time, the read is the same, through the cursor.
    let before = slack_calls(&env).len();
    env.edit_stub(|s| s["slack"]["pageSize"] = json!(1));
    assert_eq!(
        ids(&g.history(GATEWAY_ACCOUNT, &t, &first)),
        [PARENT_TS, REPLY_TS]
    );
    let paged: Vec<Value> = slack_calls(&env).drain(before..).collect();
    assert_eq!(paged.len(), 2, "{paged:?}");
    assert_eq!(paged[1]["body"]["cursor"], "c:1");
    assert_eq!(paged[0]["body"]["include_all_metadata"], true);
    env.edit_stub(|s| s["slack"]["pageSize"] = json!(200));
    // After the parent: the reply alone. The stub leaves the parent out of
    // a bounded read, so the adapter asks for it alone and still answers
    // the starter.
    let h = g.history(
        GATEWAY_ACCOUNT,
        &t,
        &HistoryQuery::After {
            id: Some(PARENT_TS.into()),
            limit: 200,
        },
    );
    assert_eq!(ids(&h), [REPLY_TS]);
    assert_eq!(
        h.known()
            .unwrap()
            .starter
            .as_ref()
            .and_then(|s| s.id.clone()),
        Some(PARENT_TS.into())
    );
    let h = g.history(
        GATEWAY_ACCOUNT,
        &t,
        &HistoryQuery::Since {
            at: 1_730_000_060_000,
        },
    );
    assert_eq!(ids(&h), [REPLY_TS]);
    // A channel read is its top-level posts, with no starter.
    let h = g.history(GATEWAY_ACCOUNT, &t.channel(), &first);
    assert_eq!(ids(&h), [PARENT_TS, OTHER_TS]);
    assert_eq!(h.known().unwrap().starter, None);
    // A parent Slack reports deleted.
    env.edit_stub(|s| s["slack"]["tombstones"] = json!([PARENT_TS]));
    let h = g.history(GATEWAY_ACCOUNT, &t, &first);
    assert_eq!(ids(&h), [REPLY_TS]);
    assert_eq!(h.known().unwrap().starter, Some(Starter::removed()));
    // A thread Slack does not know is a refusal, never proven absence.
    assert!(matches!(
        g.history(GATEWAY_ACCOUNT, &thread("1730009999.000000"), &first),
        HistoryObservation::Unknown { kind: Uncertainty::Refused, why, .. } if why.contains("thread_not_found")
    ));
    // What reached curl: the operator's config file named, the method's
    // URL, and no credential on any command line.
    let calls = env.stub_calls();
    assert!(!calls.is_empty());
    for call in &calls {
        let argv: Vec<&str> = call["argv"]
            .as_array()
            .unwrap()
            .iter()
            .map(|a| a.as_str().unwrap())
            .collect();
        assert_eq!(argv[0], "--include");
        let at = argv
            .iter()
            .position(|a| *a == "--config")
            .expect("--config");
        assert_eq!(argv[at + 1], curl_config.to_string_lossy());
        assert!(argv.contains(&"=https"));
        assert!(
            argv.iter()
                .all(|a| !a.contains("Authorization") && !a.contains("xoxb")),
            "{argv:?}"
        );
        assert!(
            argv.last().unwrap().starts_with("https://slack.com/api/"),
            "{argv:?}"
        );
    }
    assert_eq!(slack_calls(&env)[0]["method"], "auth.test");
}

#[test]
fn the_slack_adapter_opens_and_posts_keyed_mrkdwn_through_the_stub_curl() {
    let (env, curl, curl_config) = slack_env("slack-writes");
    seed_slack(&env);
    let g = slack_adapter(&curl, &curl_config, Duration::from_secs(30));
    let channel = thread(PARENT_TS).channel();
    let opened = g.open(
        GATEWAY_ACCOUNT,
        &channel,
        "**Question:** which DB?\n\n- a & b\n- [docs](https://example.test/db)",
        "task/t/u7/gateway/question/q1",
    );
    let Observation::Known(EffectOutcome::Opened {
        conversation,
        message,
        at,
    }) = opened
    else {
        panic!("{opened:?}")
    };
    assert_eq!(message, "1730000500.000000");
    assert_eq!(at, 1_730_000_500_000, "Slack's own time");
    assert_eq!(conversation, thread(&message));
    let posts = slack_posts(&env);
    assert_eq!(posts.len(), 1);
    assert_eq!(posts[0]["channel"], GATEWAY_CHANNEL);
    assert!(
        posts[0]["thread_ts"].is_null(),
        "an open is a top-level post"
    );
    assert_eq!(
        posts[0]["text"],
        "*Question:* which DB?\n\n• a &amp; b\n• <https://example.test/db|docs>"
    );
    assert_eq!(posts[0]["metadata"]["event_type"], "fleet_post");
    assert_eq!(
        posts[0]["metadata"]["event_payload"]["key"],
        "task/t/u7/gateway/question/q1"
    );
    let posted = g.post(
        GATEWAY_ACCOUNT,
        &conversation,
        "Thanks <you> & bye",
        "task/t/u7/gateway/comment/c1",
    );
    assert_eq!(
        posted,
        Observation::Known(EffectOutcome::Posted {
            message: "1730000501.000000".into(),
            at: 1_730_000_501_000
        })
    );
    let posts = slack_posts(&env);
    assert_eq!(
        posts[1]["thread_ts"], message,
        "a post replies to the parent"
    );
    assert_eq!(posts[1]["text"], "Thanks &lt;you&gt; &amp; bye");
    // Fleet's posts read back as Fleet's, with their keys, from history.
    let h = g.history(
        GATEWAY_ACCOUNT,
        &conversation,
        &HistoryQuery::Since { at: 0 },
    );
    let h = h.known().unwrap_or_else(|| panic!("{h:?}"));
    assert_eq!(h.messages.len(), 2);
    assert_eq!(h.messages[0].author.kind, AuthorKind::Fleet);
    assert_eq!(
        h.messages[0].key.as_deref(),
        Some("task/t/u7/gateway/question/q1")
    );
    assert!(h.messages[0].starts_thread);
    assert_eq!(
        h.messages[1].key.as_deref(),
        Some("task/t/u7/gateway/comment/c1")
    );
    assert_eq!(
        h.starter.as_ref().and_then(|s| s.key.clone()).as_deref(),
        Some("task/t/u7/gateway/question/q1")
    );
    // A long text is split at the limit into consecutive replies under
    // derived keys; the outcome names the first.
    let long: String = (1..=3)
        .map(|i| format!("paragraph {i} {}", "x".repeat(POST_LIMIT * 2 / 3)))
        .collect::<Vec<_>>()
        .join("\n\n");
    let posted = g.post(
        GATEWAY_ACCOUNT,
        &conversation,
        &long,
        "task/t/u7/gateway/result/1",
    );
    assert_eq!(
        posted,
        Observation::Known(EffectOutcome::Posted {
            message: "1730000502.000000".into(),
            at: 1_730_000_502_000
        })
    );
    let posts = slack_posts(&env);
    assert_eq!(posts.len(), 5);
    for (i, post) in posts[2..].iter().enumerate() {
        assert_eq!(
            post["metadata"]["event_payload"]["key"],
            format!("task/t/u7/gateway/result/1/{}/3", i + 1)
        );
        assert_eq!(post["thread_ts"], message);
        let text = post["text"].as_str().unwrap();
        assert!(text.chars().count() <= POST_LIMIT);
        assert!(
            text.starts_with(&format!("paragraph {}", i + 1)),
            "{text:.20}"
        );
    }
    // A refusal from Slack is Known, and nothing lands.
    env.edit_stub(|s| s["slack"]["postError"] = json!("not_in_channel"));
    assert!(matches!(
        g.post(GATEWAY_ACCOUNT, &conversation, "x", "k"),
        Observation::Known(EffectOutcome::Failed { why }) if why.contains("not_in_channel")
    ));
    env.edit_stub(|s| s["slack"]["postError"] = Value::Null);
    assert!(matches!(
        g.open(GATEWAY_ACCOUNT, &ConversationRef::split("slack:C0999ZZZZ").unwrap(), "x", "k"),
        Observation::Known(EffectOutcome::Failed { why }) if why.contains("channel_not_found")
    ));
    assert_eq!(slack_posts(&env).len(), 5);
    // Every post named mrkdwn and carried its text on stdin, never in argv.
    for call in slack_calls(&env)
        .iter()
        .filter(|c| c["method"] == "chat.postMessage")
    {
        assert_eq!(call["body"]["mrkdwn"], true);
    }
    for call in env.stub_calls() {
        assert!(
            call["argv"]
                .as_array()
                .unwrap()
                .iter()
                .all(|a| !a.as_str().unwrap().contains("paragraph")),
            "the text travels on stdin"
        );
    }
}

#[test]
fn the_slack_adapters_unknowns_carry_slacks_reasons() {
    let (env, curl, curl_config) = slack_env("slack-faults");
    seed_slack(&env);
    // A bound a loaded test host never reaches, and a short one for the
    // two cases that need the stub to run past it.
    let g = slack_adapter(&curl, &curl_config, Duration::from_secs(30));
    let quick = slack_adapter(&curl, &curl_config, Duration::from_secs(2));
    let t = thread(PARENT_TS);
    let since = HistoryQuery::Since { at: 0 };
    // `internal_error` and `fatal_error` are the two `ok: false` answers
    // Slack documents as "some aspect of the operation may have succeeded":
    // the stub lands the post and then answers so. The write is Unknown,
    // never a Known failure that step 4 would retry under the same key.
    for error in ["internal_error", "fatal_error"] {
        env.edit_stub(|s| s["slack"]["postErrorAfter"] = json!(error));
        let landed = slack_posts(&env).len();
        assert!(matches!(
            g.post(GATEWAY_ACCOUNT, &t, "x", &format!("k-{error}")),
            Observation::Unknown { why } if why.contains(error) && why.contains("may have")
        ));
        assert_eq!(
            slack_posts(&env).len(),
            landed + 1,
            "the post landed under {error}"
        );
    }
    env.edit_stub(|s| s["slack"]["postErrorAfter"] = Value::Null);
    env.edit_stub(|s| s["retryAfter"] = json!(45));
    env.stub_fault("curl", "rate-limit", 1);
    assert!(matches!(
        g.history(GATEWAY_ACCOUNT, &t, &since),
        HistoryObservation::Unknown {
            kind: Uncertainty::Throttled,
            retry_after: Some(45_000),
            ..
        }
    ));
    env.stub_fault("curl", "rate-limit", 1);
    assert!(matches!(
        g.post(GATEWAY_ACCOUNT, &t, "x", "k1"),
        Observation::Unknown { why } if why.contains("45000ms")
    ));
    env.stub_fault("curl", "auth", 1);
    assert!(matches!(
        g.history(GATEWAY_ACCOUNT, &t, &since),
        HistoryObservation::Unknown { kind: Uncertainty::Refused, why, .. } if why.contains("invalid_auth")
    ));
    env.stub_fault("curl", "auth", 1);
    assert!(matches!(
        g.post(GATEWAY_ACCOUNT, &t, "x", "k2"),
        Observation::Known(EffectOutcome::Failed { why }) if why.contains("invalid_auth")
    ));
    env.stub_fault("curl", "malformed", 1);
    assert!(g.whoami(GATEWAY_ACCOUNT).is_unknown());
    env.stub_fault("curl", "malformed", 1);
    assert!(matches!(
        g.history(GATEWAY_ACCOUNT, &t, &since),
        HistoryObservation::Unknown {
            kind: Uncertainty::Unreadable,
            ..
        }
    ));
    env.stub_fault("curl", "fail", 1);
    assert!(matches!(
        g.capabilities(GATEWAY_ACCOUNT),
        Observation::Unknown { why } if why.contains("curl exited 1")
    ));
    assert_eq!(
        slack_posts(&env).len(),
        2,
        "no fault but Slack's two maybe-performed errors let a post land"
    );
    // A split post refused after its first chunk landed is Unknown, naming
    // what landed, for a recovery read to settle.
    env.stub_fault_after("curl", "auth", 1, 1);
    let long = format!("{}\n\n{}", "a".repeat(POST_LIMIT), "b".repeat(10));
    assert!(matches!(
        g.post(GATEWAY_ACCOUNT, &t, &long, "k4"),
        Observation::Unknown { why } if why.contains("1 of 2 posts landed under k4")
    ));
    assert_eq!(slack_posts(&env).len(), 3);
    // A curl that cannot start: nothing was sent, so a write is a Known
    // refusal and a read is Unavailable.
    let missing = slack_adapter(
        &env.root.join("no-such-curl"),
        &curl_config,
        Duration::from_secs(2),
    );
    assert!(matches!(
        missing.post(GATEWAY_ACCOUNT, &t, "x", "k5"),
        Observation::Known(EffectOutcome::Failed { why }) if why.contains("could not start")
    ));
    assert!(matches!(
        missing.history(GATEWAY_ACCOUNT, &t, &since),
        HistoryObservation::Unknown {
            kind: Uncertainty::Unavailable,
            ..
        }
    ));
    assert!(missing.whoami(GATEWAY_ACCOUNT).is_unknown());
    // A call past its bound is a timeout. Last, because the stub takes its
    // fault only once it runs, and a loaded host can see it killed first,
    // leaving the fault armed for whatever call comes next.
    env.stub_fault("curl", "hang", 1);
    assert!(matches!(
        quick.history(GATEWAY_ACCOUNT, &t, &since),
        HistoryObservation::Unknown {
            kind: Uncertainty::Timeout,
            ..
        }
    ));
    env.stub_fault("curl", "hang", 1);
    assert!(quick.post(GATEWAY_ACCOUNT, &t, "x", "k3").is_unknown());
    assert_eq!(
        slack_posts(&env).len(),
        3,
        "a timed-out post landed nothing"
    );
}

// ----------------------------------------------------------------------
// Step 2: admission, Task creation under the workspace precondition, the
// busy notice and the refusal post.
// ----------------------------------------------------------------------

/// A gateway environment ready to start Tasks.
fn admitting(name: &str) -> (Env, String) {
    mirror(name)
}

fn uid_of(env: &Env, kind: &str, name: &str) -> String {
    env.fleet(&["get", kind, name]).ok().json()["metadata"]["uid"]
        .as_str()
        .expect("uid")
        .to_string()
}

#[test]
fn an_admitted_mention_starts_one_task_tied_to_the_thread() {
    let (env, conv) = admitting("admit");
    env.receive("Ev01", mention("1730000300000", T0 + 300_000))
        .ok();
    let report = env.reconcile().ok().json();
    assert_eq!(report["errors"], json!([]), "{report:#}");
    let conv_uid = uid_of(&env, "conversation", &conv);
    let task_name = format!("{conv}-1");
    let task = env.get(&task_name);
    assert_eq!(
        task["metadata"]["labels"],
        json!({
            "gateway.fleet.dev/conversation": conv,
            "gateway.fleet.dev/conversation-uid": conv_uid,
            "gateway.fleet.dev/route": GATEWAY_ROUTE,
        }),
        "{task:#}"
    );
    assert_eq!(
        task["spec"],
        json!({
            "configRef": {"name": "conversation"},
            "brief": support::GATEWAY_BRIEF,
            "workspace": {"dir": env.gateway_workspace()},
            "dispatch": true,
        }),
        "the Task is the interface page's JSON: {task:#}"
    );
    let task_uid = task["metadata"]["uid"].as_str().unwrap().to_string();
    let st = env.conversation_status(&conv);
    assert_eq!(st["phase"], "Tied", "{st:#}");
    assert_condition(&st, "Tied", "True", "TaskKnown");
    assert_eq!(
        st["task"],
        json!({"name": task_name, "uid": task_uid, "n": 1, "state": "created"})
    );
    let key = format!("conversation/{conv_uid}/task/1");
    let record = &st["effects"][&key];
    assert_eq!(record["intent"]["action"], "create", "{st:#}");
    assert_eq!(record["receipt"]["outcome"]["known"]["type"], "created");
    assert_eq!(record["receipt"]["outcome"]["known"]["uid"], task_uid);
    assert_eq!(record["attempts"], 1);
    assert_eq!(
        st["judged"], st["revision"],
        "every message was judged: {st:#}"
    );
    // One Task, however many passes: the key has its receipt.
    env.expire_wait_of("Conversation", &conv);
    env.reconcile().ok();
    let tasks = env.fleet(&["list", "tasks", "--json"]).ok().json();
    assert_eq!(tasks.as_array().unwrap().len(), 1, "{tasks:#}");
    assert_eq!(env.conversation_status(&conv)["task"]["n"], 1);
    // A second mention in the tied thread is a turn for the relay, not a
    // new Task.
    env.receive("Ev02", mention("1730000400000", T0 + 400_000))
        .ok();
    env.reconcile().ok();
    let st = env.conversation_status(&conv);
    assert_eq!(st["task"]["n"], 1, "{st:#}");
    assert_eq!(st["revision"], 2);
    assert_eq!(
        env.fleet(&["list", "tasks", "--json"])
            .ok()
            .json()
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        env.fake_gateway().calls("post").unwrap(),
        0,
        "nothing was posted"
    );
    // The labels are the controller's: apply refuses them on another Task.
    let mut labelled = support::task("labelled", false);
    labelled["metadata"]["labels"]["gateway.fleet.dev/conversation-uid"] = json!(conv_uid);
    env.apply(labelled)
        .code(1)
        .stderr_has("gateway.fleet.dev/conversation-uid");
    env.assert_no_temp();
}

#[test]
fn a_mention_from_someone_not_on_the_allowlist_earns_one_refusal() {
    let (env, conv) = admitting("refusal");
    let stranger = |id: &str, delivery: &str, at: u64| {
        env.receive(
            delivery,
            message_event(THREAD, &at.to_string(), id, at, &format!("<@{BOT}> hi")),
        )
        .ok();
    };
    stranger("U0999ZZZZ", "Ev01", T0 + 300_000);
    let report = env.reconcile().ok().json();
    assert_eq!(report["errors"], json!([]), "{report:#}");
    assert_eq!(report["effects"], 1, "{report:#}");
    let conv_uid = uid_of(&env, "conversation", &conv);
    let posts = env.fake_gateway().posts().unwrap();
    assert_eq!(posts.len(), 1, "{posts:#?}");
    let key = format!("conversation/{conv_uid}/refuse/U0999ZZZZ");
    assert_eq!(posts[0].key, key);
    assert_eq!(posts[0].conversation.address(), THREAD);
    assert_eq!(posts[0].text, fleet_api::gateway::DEFAULT_REFUSAL);
    let st = env.conversation_status(&conv);
    assert_eq!(st["phase"], "Untied", "{st:#}");
    assert_eq!(st["refusals"], json!(["U0999ZZZZ"]));
    assert_eq!(st["effects"][&key]["intent"]["action"], "post");
    assert_eq!(
        st["effects"][&key]["receipt"]["outcome"]["known"]["type"],
        "posted"
    );
    assert!(st["task"].is_null(), "no Task starts: {st:#}");
    assert_eq!(
        env.fleet(&["list", "tasks", "--json"])
            .ok()
            .json()
            .as_array()
            .unwrap()
            .len(),
        0
    );
    // The same author again: no second refusal. Another stranger: one more.
    stranger("U0999ZZZZ", "Ev02", T0 + 400_000);
    env.reconcile().ok();
    assert_eq!(env.fake_gateway().posts().unwrap().len(), 1);
    stranger("U0888YYYY", "Ev03", T0 + 500_000);
    env.reconcile().ok();
    let posts = env.fake_gateway().posts().unwrap();
    assert_eq!(posts.len(), 2);
    assert_eq!(
        posts[1].key,
        format!("conversation/{conv_uid}/refuse/U0888YYYY")
    );
    let st = env.conversation_status(&conv);
    assert_eq!(st["refusals"], json!(["U0999ZZZZ", "U0888YYYY"]));
    // Fleet's own post reached the mirror through history and is never a
    // candidate for anything.
    assert!(st["task"].is_null());
    env.assert_no_temp();
}

#[test]
fn an_orphaned_thread_starts_the_next_task_on_any_admitted_message() {
    let (env, conv) = admitting("orphan-next");
    env.receive("Ev01", mention("1730000300000", T0 + 300_000))
        .ok();
    env.reconcile().ok();
    let first = format!("{conv}-1");
    env.get(&first);
    delete_task(&env, &first);
    env.expire_wait_of("Conversation", &conv);
    env.reconcile().ok();
    let st = env.conversation_status(&conv);
    assert_eq!(st["phase"], "Orphaned", "{st:#}");
    assert_eq!(st["task"]["state"], "gone");
    // Context from a stranger starts nothing; an admitted message without a
    // mention starts Task 2 with the whole thread behind it.
    env.receive(
        "Ev02",
        message_event(THREAD, "1730000400000", "U0999ZZZZ", T0 + 400_000, "me too"),
    )
    .ok();
    env.reconcile().ok();
    let st = env.conversation_status(&conv);
    assert_eq!(st["phase"], "Orphaned", "{st:#}");
    env.receive(
        "Ev03",
        message_event(
            THREAD,
            "1730000500000",
            SAM_ID,
            T0 + 500_000,
            "still broken",
        ),
    )
    .ok();
    let report = env.reconcile().ok().json();
    assert_eq!(report["errors"], json!([]), "{report:#}");
    let second = format!("{conv}-2");
    let task = env.get(&second);
    assert_eq!(
        task["metadata"]["labels"]["gateway.fleet.dev/conversation"],
        conv
    );
    let st = env.conversation_status(&conv);
    assert_eq!(st["phase"], "Tied", "{st:#}");
    assert_eq!(st["task"]["name"], second);
    assert_eq!(st["task"]["n"], 2);
    assert_eq!(st["task"]["state"], "created");
    assert_eq!(st["messages"].as_array().unwrap().len(), 4, "{st:#}");
    env.get_absent(&first);
    env.assert_no_temp();
}

#[test]
fn a_busy_workspace_holds_the_thread_says_so_once_and_starts_when_free() {
    let (env, conv) = admitting("busy");
    // Another Task already runs in the route's directory, spelled with a
    // trailing slash: the same directory.
    let mut holder = support::task("holder", false);
    holder["spec"]["workspace"]["dir"] = json!(format!("{}/", env.gateway_workspace()));
    env.apply(holder).ok();
    env.receive("Ev01", mention("1730000300000", T0 + 300_000))
        .ok();
    let report = env.reconcile().ok().json();
    assert_eq!(report["errors"], json!([]), "{report:#}");
    let conv_uid = uid_of(&env, "conversation", &conv);
    let task_name = format!("{conv}-1");
    env.get_absent(&task_name);
    let st = env.conversation_status(&conv);
    assert_eq!(st["phase"], "Untied", "{st:#}");
    assert_eq!(
        st["task"],
        json!({"name": task_name, "n": 1, "state": "busy", "trigger": "1730000300000"})
    );
    assert_condition(&st, "NeedsAttention", "True", "WorkspaceBusy");
    assert!(
        support::condition(&st, "NeedsAttention")["message"]
            .as_str()
            .unwrap()
            .contains("Task/holder"),
        "{st:#}"
    );
    let create_key = format!("conversation/{conv_uid}/task/1");
    assert_eq!(
        st["effects"][&create_key]["receipt"]["outcome"]["known"]["type"],
        "workspaceBusy"
    );
    let busy_key = format!("conversation/{conv_uid}/busy/1");
    let posts = env.fake_gateway().posts().unwrap();
    assert_eq!(posts.len(), 1, "one busy notice: {posts:#?}");
    assert_eq!(posts[0].key, busy_key);
    assert!(posts[0].text.contains("queued"), "{}", posts[0].text);
    assert_eq!(
        st["effects"][&busy_key]["receipt"]["outcome"]["known"]["type"],
        "posted"
    );
    // Planned again every pollMs, still busy: no second notice, and no
    // retention while it waits.
    env.expire_wait_of("Conversation", &conv);
    env.reconcile().ok();
    env.get_absent(&task_name);
    let st = env.conversation_status(&conv);
    assert_eq!(st["task"]["state"], "busy", "{st:#}");
    assert_eq!(env.fake_gateway().posts().unwrap().len(), 1);
    let now = support::now_ms();
    let poll = 300_000u64;
    assert!(
        st["notBefore"].as_u64().unwrap() <= now + poll + 5_000,
        "the next look is one pollMs away, not the retention: {st:#}"
    );
    // The directory frees: the Task starts, with the whole thread.
    env.fleet(&["delete", "task", "holder"]).ok();
    env.reconcile().ok();
    env.get_absent("holder");
    env.expire_wait_of("Conversation", &conv);
    let report = env.reconcile().ok().json();
    assert_eq!(report["errors"], json!([]), "{report:#}");
    let task = env.get(&task_name);
    let st = env.conversation_status(&conv);
    assert_eq!(st["phase"], "Tied", "{st:#}");
    assert_eq!(st["task"]["state"], "created");
    assert_eq!(st["task"]["uid"], task["metadata"]["uid"]);
    assert_condition(&st, "NeedsAttention", "False", "Resolved");
    assert_eq!(
        st["effects"][&create_key]["attempts"], 1,
        "a busy refusal is not an attempt: {st:#}"
    );
    assert_eq!(env.fake_gateway().posts().unwrap().len(), 1);
    env.assert_no_temp();
}

#[test]
fn a_crash_between_the_create_intent_and_its_receipt_adopts_the_task() {
    let (env, conv) = admitting("adopt");
    env.receive("Ev01", mention("1730000300000", T0 + 300_000))
        .ok();
    env.reconcile().ok();
    let task_name = format!("{conv}-1");
    let task_uid = uid_of(&env, "task", &task_name);
    let conv_uid = uid_of(&env, "conversation", &conv);
    let key = format!("conversation/{conv_uid}/task/1");
    // The manager wrote the intent and created the Task, then died before
    // the receipt: the record holds the intent alone and the tie is still
    // pending.
    let path = env.object_path("Conversation", &conv);
    let mut obj: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    obj["status"]["effects"][&key]
        .as_object_mut()
        .unwrap()
        .remove("receipt");
    obj["status"]["task"] =
        json!({"name": task_name, "n": 1, "state": "pending", "trigger": "1730000300000"});
    obj["status"]["phase"] = json!("Untied");
    obj["status"]["notBefore"] = json!(0);
    fs::write(&path, obj.to_string()).unwrap();
    let report = env.reconcile().ok().json();
    assert_eq!(report["errors"], json!([]), "{report:#}");
    let st = env.conversation_status(&conv);
    assert_eq!(st["phase"], "Tied", "{st:#}");
    assert_eq!(st["task"]["state"], "created");
    assert_eq!(
        st["task"]["uid"], task_uid,
        "the Task was adopted, not made again"
    );
    let receipt = &st["effects"][&key]["receipt"];
    assert_eq!(receipt["outcome"]["known"]["type"], "created", "{st:#}");
    assert_eq!(receipt["outcome"]["known"]["adopted"], true);
    assert_eq!(st["effects"][&key]["attempts"], 1);
    assert_eq!(
        env.fleet(&["list", "tasks", "--json"])
            .ok()
            .json()
            .as_array()
            .unwrap()
            .len(),
        1
    );
    env.assert_no_temp();
}

#[test]
fn a_mention_read_from_history_before_the_listener_delivers_it_still_starts_a_task() {
    let env = Env::gateway("history-then-delivery");
    let mut stranger = person("U0999ZZZZ", T0 + 100_000, "<@B0FLEET001> me too");
    stranger.mentions_fleet = true;
    let mut alex = person(ALEX_ID, T0 + 300_000, "<@B0FLEET001> can you look into it?");
    alex.mentions_fleet = true;
    env.fake_gateway()
        .seed(
            &ConversationRef::split(THREAD).unwrap(),
            vec![
                person(SAM_ID, T0, "The nightly export failed again."),
                stranger.clone(),
                alex,
            ],
        )
        .unwrap();
    env.apply(task_config()).ok();
    let conv = conversation_name(GATEWAY_ACCOUNT, THREAD);
    // A stranger's mention opens the mirror; its first history read
    // numbers Alex's mention before the listener delivers it.
    env.receive(
        "Ev01",
        message_event(
            THREAD,
            "1730000100000",
            "U0999ZZZZ",
            T0 + 100_000,
            "<@B0FLEET001> me too",
        ),
    )
    .ok();
    let report = env.reconcile().ok().json();
    assert_eq!(report["errors"], json!([]), "{report:#}");
    let st = env.conversation_status(&conv);
    assert_eq!(st["messages"].as_array().map(Vec::len), Some(3), "{st:#}");
    assert!(st["task"].is_null(), "history alone starts nothing: {st:#}");
    // The listener's delivery of that mention arrives one pass later.
    env.receive("Ev02", mention("1730000300000", T0 + 300_000))
        .ok();
    env.expire_wait_of("Conversation", &conv);
    let report = env.reconcile().ok().json();
    assert_eq!(report["errors"], json!([]), "{report:#}");
    let st = env.conversation_status(&conv);
    assert_eq!(
        st["task"]["n"], 1,
        "the delivered mention was judged: {st:#}"
    );
    env.fleet(&["get", "task", &format!("{conv}-1")]).ok();
    env.assert_no_temp();
}

#[test]
fn a_same_named_task_created_for_another_thread_is_never_adopted() {
    let (env, conv) = admitting("adopt-refused");
    // `<conversation>-1` exists already, in another directory, labelled for
    // another Conversation's uid, as that one's create would leave it.
    let task_name = format!("{conv}-1");
    env.apply(support::task(&task_name, false)).ok();
    let other_uid = uid_of(&env, "task", &task_name);
    let path = env.object_path("Task", &task_name);
    let mut task: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    assert_ne!(
        task["spec"]["workspace"]["dir"],
        json!(env.gateway_workspace())
    );
    task["metadata"]["labels"]["gateway.fleet.dev/conversation"] = json!(conv);
    task["metadata"]["labels"]["gateway.fleet.dev/conversation-uid"] = json!("u0-another");
    fs::write(&path, task.to_string()).unwrap();
    env.receive("Ev01", mention("1730000300000", T0 + 300_000))
        .ok();
    env.reconcile().ok();
    let conv_uid = uid_of(&env, "conversation", &conv);
    let st = env.conversation_status(&conv);
    let receipt = &st["effects"][&format!("conversation/{conv_uid}/task/1")]["receipt"];
    assert_eq!(receipt["outcome"]["known"]["type"], "failed", "{st:#}");
    assert!(
        receipt["outcome"]["known"]["why"]
            .as_str()
            .is_some_and(|w| w.contains("was not created for this thread")),
        "{st:#}"
    );
    assert_ne!(st["task"]["state"], "created", "{st:#}");
    assert_ne!(st["task"]["uid"], json!(other_uid), "{st:#}");
    assert_eq!(uid_of(&env, "task", &task_name), other_uid);
    env.assert_no_temp();
}
