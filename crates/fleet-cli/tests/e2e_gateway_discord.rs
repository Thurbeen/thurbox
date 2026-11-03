//! A Discord account end to end: the listener's Gateway dispatches go
//! through `fleet gateway receive` into a Conversation on the thread's
//! address, an admitted mention starts a Task, and a refusal reaches
//! Discord through the stub `curl`: the thread is started from the
//! mention, the post carries a nonce with `enforce_nonce` and mentions
//! nobody, and a post whose answer was lost is recovered by resending the
//! same nonce inside the window. Fleet names the operator's curl config and
//! never builds an Authorization header.

mod support;

use std::fs;
use std::path::PathBuf;
use std::time::Duration;

use fleet_api::Observation;
use fleet_api::gateway::{
    AccountConfig, ConversationRef, Gateway, HistoryObservation, HistoryQuery, Inbound, Marker,
    Uncertainty,
};
use fleet_api::plan::EffectOutcome;
use fleet_gateway::DiscordGateway;

use serde_json::{Value, json};
use support::{Env, GATEWAY_BRIEF, assert_condition, conversation_name, stub_exe};

const ACCOUNT: &str = "discord-main";
const GUILD: &str = "1200000000000000000";
const BOT: &str = "1210000000000000000";
const ALEX: &str = "1220000000000000000";
const STRANGER: &str = "1240000000000000000";
const CHANNEL: &str = "1230000000000000000";
const EPOCH: u64 = 1_420_070_400_000;
/// GUILDS | GUILD_MESSAGES: what a listener identifies with when the
/// account does not hold MESSAGE_CONTENT.
const INTENTS: u64 = (1 << 0) | (1 << 9);

/// The snowflake of a message sent at `ms`.
fn snowflake(ms: u64) -> String {
    ((ms - EPOCH) << 22).to_string()
}

/// A gateway environment whose one account is on Discord, served by the
/// stub copied as `curl`, which is all the children's PATH holds.
fn discord_env(name: &str) -> (Env, PathBuf) {
    let mut env = Env::new(name);
    let bin = env.root.join("discord-bin");
    env.only_on_path(&bin);
    fs::create_dir_all(&bin).expect("bin");
    let exe = if cfg!(windows) { ".exe" } else { "" };
    let curl = bin.join(format!("curl{exe}"));
    fs::copy(stub_exe(), &curl).expect("curl stub");
    fs::create_dir_all(env.stub_dir()).expect("stub dir");
    fs::write(
        bin.join("stub-dir"),
        env.stub_dir().to_string_lossy().as_bytes(),
    )
    .expect("stub-dir");
    let curl_config = env.root.join("discord-curl.conf");
    fs::write(
        &curl_config,
        "# The operator's curl config: the bot token header lives here, and Fleet never reads it.\n",
    )
    .expect("curl config");
    let root = env.root.to_string_lossy().replace('\\', "/");
    let section = format!(
        "\n[gateway]\ncallTimeoutMs = 3000\n\n\
[gateway.accounts.{ACCOUNT}]\nplatform = \"discord\"\nteam = \"{GUILD}\"\nbot = \"{BOT}\"\n\
inbound = \"discord-gateway\"\ncli = '{}'\ncurlConfig = \"{root}/discord-curl.conf\"\n\n\
[gateway.people.alex]\n{ACCOUNT} = \"{ALEX}\"\n\n\
[gateway.routes.eng]\naccount = \"{ACCOUNT}\"\nchannels = [\"{CHANNEL}\"]\nmention = \"required\"\nallow = [\"alex\"]\n\
taskConfig = \"conversation\"\nbrief = \"{root}/briefs/conversation.md\"\nworkspace = {{ dir = \"{root}/work/conversations\" }}\n",
        curl.to_string_lossy(),
    );
    let mut cfg = fs::read_to_string(&env.config).expect("config");
    cfg.push_str(&section);
    fs::write(&env.config, cfg).expect("config");
    fs::create_dir_all(env.root.join("briefs")).expect("briefs");
    fs::write(env.root.join("briefs/conversation.md"), GATEWAY_BRIEF).expect("brief");
    env.apply(support::object(
        "TaskConfig",
        "conversation",
        json!({"harness": "claude-code", "host": "local"}),
    ))
    .ok();
    (env, curl_config)
}

/// A top-level message in the route's channel, in Discord's shape.
fn message(id: &str, author: &str, at: u64, content: &str, mentions_bot: bool) -> Value {
    json!({
        "id": id, "channel_id": CHANNEL, "type": 0,
        "author": {"id": author, "username": "alex", "global_name": "Alex"},
        "content": content, "timestamp": null, "edited_timestamp": null,
        "mentions": if mentions_bot { json!([{"id": BOT, "username": "fleet", "bot": true}]) } else { json!([]) },
        "_at": at,
    })
}

/// Seed the stub guild with `messages` in the route's channel.
fn seed(env: &Env, messages: &[Value]) {
    let stored: Vec<Value> = messages
        .iter()
        .map(|m| {
            let mut m = m.clone();
            let at = m["_at"].as_u64().unwrap();
            m.as_object_mut().unwrap().remove("_at");
            m["timestamp"] = json!(iso(at));
            m
        })
        .collect();
    env.edit_stub(|s| {
        s["discord"] = json!({
            "guild": GUILD, "botUser": BOT, "botName": "fleet", "appFlags": 0,
            "clock": 1_730_000_500_000u64,
            "channels": {CHANNEL: {"parent": null, "messages": stored}},
        });
    });
}

/// `fleet gateway receive` with the listener's envelope: the raw Gateway
/// dispatch, and a `null` parentId for a message in a text channel.
fn receive(env: &Env, seq: u64, m: &Value) -> support::Run {
    let mut d = m.clone();
    let at = d["_at"].as_u64().unwrap();
    d.as_object_mut().unwrap().remove("_at");
    d["timestamp"] = json!(iso(at));
    d["guild_id"] = json!(GUILD);
    let envelope = json!({
        "verified": "connection",
        "receivedAt": support::now_ms(),
        "payload": {"dispatch": {"op": 0, "t": "MESSAGE_CREATE", "s": seq, "d": d}, "parentId": null},
    });
    env.fleet_stdin(
        &["gateway", "receive", "--account", ACCOUNT, "--json"],
        &envelope.to_string(),
    )
}

fn iso(ms: u64) -> String {
    let secs = ms / 1000;
    let (days, rem) = (secs / 86_400, secs % 86_400);
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}.{:03}000+00:00",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60,
        ms % 1000
    )
}

fn discord(env: &Env) -> Value {
    env.stub()["discord"].clone()
}

#[test]
fn a_discord_account_reports_what_its_listener_identifies_with() {
    let (env, _) = discord_env("discord-account");
    let account = env
        .fleet(&["gateway", "account", ACCOUNT, "--json"])
        .ok()
        .json();
    assert_eq!(account["platform"], "discord");
    assert_eq!(account["team"], GUILD);
    assert_eq!(account["bot"], BOT);
    assert_eq!(account["inbound"], "discord-gateway");
    assert_eq!(account["intents"], INTENTS, "{account:#}");
    assert!(account.get("curlConfig").is_none() && account.get("cli").is_none());
    // Asking for the message content intent changes what the listener
    // identifies with, and so the digest its beats carry.
    let cfg = fs::read_to_string(&env.config).unwrap();
    fs::write(
        &env.config,
        cfg.replace(
            "inbound = \"discord-gateway\"\n",
            "inbound = \"discord-gateway\"\nmessageContent = true\n",
        ),
    )
    .unwrap();
    let with_content = env
        .fleet(&["gateway", "account", ACCOUNT, "--json"])
        .ok()
        .json();
    assert_eq!(with_content["intents"], INTENTS | (1 << 15));
    assert_ne!(with_content["digest"], account["digest"]);
    let v = Env::new("discord-version")
        .fleet(&["version", "--json"])
        .ok()
        .json();
    assert_eq!(v["gateways"], json!(["discord", "fake", "slack"]));
}

#[test]
fn a_discord_mention_reaches_a_conversation_and_the_reply_starts_its_thread() {
    let (env, curl_config) = discord_env("discord-e2e");
    let t0 = 1_730_000_000_000u64;
    let stranger_at = t0 + 300_000;
    let stranger_id = snowflake(stranger_at);
    let stranger = message(
        &stranger_id,
        STRANGER,
        stranger_at,
        &format!("<@{BOT}> hi"),
        true,
    );
    let alex_at = t0 + 400_000;
    let alex_id = snowflake(alex_at);
    let alex = message(
        &alex_id,
        ALEX,
        alex_at,
        &format!("<@{BOT}> the nightly export failed, can you look?"),
        true,
    );
    seed(&env, &[stranger.clone(), alex.clone()]);

    // A stranger's top-level mention is a Conversation on the thread the
    // message will start: `discord:<channel>:<message>`.
    receive(&env, 41, &stranger).ok();
    let address = format!("discord:{CHANNEL}:{stranger_id}");
    let conv = conversation_name(ACCOUNT, &address);
    let got = env.fleet(&["get", "conversation", &conv]).ok().json();
    assert_eq!(got["spec"]["address"], address, "{got:#}");
    // The same dispatch again, as after a resume, is the same delivery.
    receive(&env, 41, &stranger).ok();

    // Its answer is the one refusal. Discord has no thread yet, so the post
    // starts it from the mention, and the first answer is lost: the
    // resend inside the nonce window finds the post that landed.
    env.edit_stub(|s| s["discord"]["loseNextPost"] = json!(true));
    // No clock is moved here: a post on a nonce platform brings the
    // Conversation back at once, while the window is open.
    let report = env.reconcile().ok().json();
    assert_eq!(report["errors"], json!([]), "{report:#}");
    for _ in 0..3 {
        env.reconcile().ok();
    }
    let d = discord(&env);
    assert_eq!(
        d["threads"],
        json!([{"parent": CHANNEL, "id": stranger_id, "name": fleet_api::gateway::DEFAULT_REFUSAL}]),
        "{d:#}"
    );
    let posts = d["posts"].as_array().cloned().unwrap_or_default();
    assert_eq!(posts.len(), 1, "one post, however many sends: {d:#}");
    let body = &posts[0]["body"];
    assert_eq!(posts[0]["channel"], stranger_id.as_str());
    assert_eq!(body["content"], fleet_api::gateway::DEFAULT_REFUSAL);
    assert_eq!(body["enforce_nonce"], true);
    assert_eq!(body["allowed_mentions"], json!({"parse": []}));
    let nonce = body["nonce"].as_str().unwrap();
    assert_eq!(nonce.len(), 25, "{nonce}");
    let sends: Vec<&Value> = d["calls"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|c| {
            c["method"] == "POST" && c["path"] == format!("channels/{stranger_id}/messages")
        })
        .collect();
    assert!(
        sends.len() >= 3,
        "a send before the thread, the lost one, and the resend: {sends:#?}"
    );
    assert!(sends.iter().skip(1).all(|c| c["body"]["nonce"] == nonce));
    let st = env.conversation_status(&conv);
    let refusal = st["effects"]
        .as_object()
        .unwrap()
        .iter()
        .find(|(k, _)| k.contains("/refuse/"))
        .map(|(_, v)| v.clone())
        .expect("the refusal's record");
    assert_eq!(
        refusal["receipt"]["outcome"]["known"]["type"], "posted",
        "{st:#}"
    );
    assert_eq!(st["refusals"], json!([STRANGER]));
    assert!(st["task"].is_null(), "no Task for a stranger: {st:#}");

    // Every call named the operator's curl config, and no argument carries
    // a credential.
    for call in env.stub_calls() {
        let argv: Vec<String> = call["argv"]
            .as_array()
            .unwrap()
            .iter()
            .map(|a| a.as_str().unwrap_or("").to_string())
            .collect();
        if !argv
            .iter()
            .any(|a| a.starts_with("https://discord.com/api/v10/"))
        {
            continue;
        }
        assert!(
            argv.contains(&curl_config.to_string_lossy().replace('\\', "/")),
            "{argv:?}"
        );
        assert!(
            !argv
                .iter()
                .any(|a| a.to_ascii_lowercase().contains("authorization")),
            "{argv:?}"
        );
    }

    // Alex is on the list: the mention starts one Task tied to the thread.
    receive(&env, 42, &alex).ok();
    let report = env.reconcile().ok().json();
    assert_eq!(report["errors"], json!([]), "{report:#}");
    let conv = conversation_name(ACCOUNT, &format!("discord:{CHANNEL}:{alex_id}"));
    let st = env.conversation_status(&conv);
    assert_eq!(st["phase"], "Tied", "{st:#}");
    assert_condition(&st, "Tied", "True", "TaskKnown");
    let task = env.get(&format!("{conv}-1"));
    assert_eq!(task["metadata"]["labels"]["gateway.fleet.dev/route"], "eng");
    // The Listener resolved the bot through the same curl.
    assert_condition(
        &env.listener_status(&format!("listener-{ACCOUNT}")),
        "Resolved",
        "True",
        "IdentityMatches",
    );
    env.assert_no_temp();
}

/// The adapter as the manager builds it, driven in-process against the
/// stub `curl` that `discord_env` placed.
fn adapter(env: &Env, curl_config: &std::path::Path) -> DiscordGateway {
    let exe = if cfg!(windows) { ".exe" } else { "" };
    DiscordGateway::new(
        ACCOUNT,
        &AccountConfig {
            platform: "discord".into(),
            team: Some(GUILD.into()),
            bot: BOT.into(),
            inbound: Inbound::DiscordGateway,
            message_content: Some(true),
            cli: Some(
                env.root
                    .join(format!("discord-bin/curl{exe}"))
                    .to_string_lossy()
                    .into_owned(),
            ),
            curl_config: Some(curl_config.to_path_buf()),
            dir: None,
        },
        Duration::from_secs(3),
    )
    .expect("a discord adapter")
}

#[test]
fn the_discord_adapter_opens_reads_and_answers_unknown_where_discord_leaves_doubt() {
    let (env, curl_config) = discord_env("discord-adapter");
    seed(&env, &[]);
    let g = adapter(&env, &curl_config);
    let channel = ConversationRef {
        platform: "discord".into(),
        chat: CHANNEL.into(),
        thread: None,
    };
    // Capabilities: the nonce window, and unmentioned only with the
    // application's message content flag.
    let caps = g
        .capabilities(ACCOUNT)
        .known()
        .copied()
        .expect("capabilities");
    assert_eq!(caps.marker, Marker::Nonce { window_ms: 60_000 });
    assert!(!caps.unmentioned, "the application lacks the intent");
    env.edit_stub(|s| s["discord"]["appFlags"] = json!(1u64 << 19));
    assert!(g.capabilities(ACCOUNT).known().unwrap().unmentioned);
    // Open: a post in the channel, then a thread from it, named after the
    // first line; a long text's later chunks go into the thread.
    let long = format!("Question: which export?\n{}", "word ".repeat(500));
    let opened = g.open(ACCOUNT, &channel, &long, "gateway/u1/open/q1");
    let Observation::Known(EffectOutcome::Opened {
        conversation,
        message,
        ..
    }) = opened
    else {
        panic!("{opened:?}");
    };
    assert_eq!(
        conversation.address(),
        format!("discord:{CHANNEL}:{message}")
    );
    let d = discord(&env);
    assert_eq!(d["threads"][0]["name"], "Question: which export?");
    let posts = d["posts"].as_array().unwrap();
    assert_eq!(
        posts.len(),
        3,
        "the first line, then the long line in two: {d:#}"
    );
    assert_eq!(posts[0]["channel"], CHANNEL);
    assert_eq!(posts[1]["channel"], message.as_str());
    assert_eq!(posts[2]["channel"], message.as_str());
    assert_ne!(posts[0]["body"]["nonce"], posts[1]["body"]["nonce"]);
    // History: the starter and the thread, oldest first; a thread Discord
    // does not know yet is its starter alone.
    let h = g.history(
        ACCOUNT,
        &conversation,
        &HistoryQuery::After {
            id: None,
            limit: 200,
        },
    );
    let known = h.known().unwrap_or_else(|| panic!("{h:?}"));
    assert_eq!(known.messages.len(), 3);
    assert_eq!(known.messages[0].id, message);
    assert_eq!(
        known.starter.as_ref().unwrap().id.as_deref(),
        Some(message.as_str())
    );
    // A first read of a long thread takes its newest messages, paging
    // backward, and never runs out of budget walking from the oldest.
    let first_at = 1_730_000_600_000u64;
    env.edit_stub(|s| {
        let thread = &mut s["discord"]["channels"][message.as_str()]["messages"];
        for i in 0..1_200u64 {
            let at = first_at + i * 1000;
            thread.as_array_mut().unwrap().push(json!({
                "id": snowflake(at), "channel_id": message, "type": 0,
                "author": {"id": ALEX, "username": "alex"}, "content": format!("line {i}"),
                "timestamp": iso(at), "mentions": [],
            }));
        }
    });
    let h = g.history(
        ACCOUNT,
        &conversation,
        &HistoryQuery::After {
            id: None,
            limit: 200,
        },
    );
    let known = h.known().unwrap_or_else(|| panic!("{h:?}"));
    assert_eq!(known.messages.len(), 200);
    assert_eq!(known.messages[199].text, "line 1199");
    assert_eq!(known.messages[0].text, "line 1000");
    // A throttled read carries Discord's retry_after.
    env.stub_fault("curl", "rate-limit", 1);
    match g.history(ACCOUNT, &conversation, &HistoryQuery::Since { at: 0 }) {
        HistoryObservation::Unknown {
            kind: Uncertainty::Throttled,
            retry_after: Some(1500),
            ..
        } => {}
        other => panic!("{other:?}"),
    }
    // The opening post lands and the thread start is refused: something
    // landed, so the outcome is Unknown, never a clean refusal.
    env.stub_fault_after("curl", "auth", 1, 1);
    let half = g.open(ACCOUNT, &channel, "Another question", "gateway/u1/open/q2");
    assert!(matches!(half, Observation::Unknown { .. }), "{half:?}");
    // Nothing landed and Discord said no: a Known refusal.
    env.stub_fault("curl", "auth", 1);
    let refused = g.open(ACCOUNT, &channel, "A third", "gateway/u1/open/q3");
    assert!(
        matches!(refused, Observation::Known(EffectOutcome::Failed { .. })),
        "{refused:?}"
    );
}

#[test]
fn the_discord_adapter_creates_and_deletes_its_own_reactions_with_the_emoji_url_encoded() {
    let (env, curl_config) = discord_env("discord-reactions");
    let starter = snowflake(1_730_000_100_000);
    let reply = snowflake(1_730_000_200_000);
    seed(
        &env,
        &[message(
            &starter,
            ALEX,
            1_730_000_100_000,
            "<@x> look",
            true,
        )],
    );
    // The thread started from the starter message, holding one reply.
    env.edit_stub(|s| {
        s["discord"]["channels"][&starter] = json!({"parent": CHANNEL, "messages": [
            {"id": reply, "channel_id": starter, "type": 0, "content": "more",
             "author": {"id": ALEX, "username": "alex"}, "mentions": [], "timestamp": iso(1_730_000_200_000)}
        ]});
    });
    let g = adapter(&env, &curl_config);
    let thread = ConversationRef {
        platform: "discord".into(),
        chat: CHANNEL.into(),
        thread: Some(starter.clone()),
    };
    let done = Observation::Known(EffectOutcome::Reacted { already: false });
    // The starter stays in the parent channel; a reply is in the thread.
    assert_eq!(g.react(ACCOUNT, &thread, &starter, "eyes", true), done);
    assert_eq!(g.react(ACCOUNT, &thread, &reply, "eyes", true), done);
    assert_eq!(
        g.react(ACCOUNT, &thread, &reply, "white_check_mark", true),
        done
    );
    assert_eq!(g.react(ACCOUNT, &thread, &reply, "eyes", false), done);
    // A repeat changes nothing, and Discord answers it the same way.
    assert_eq!(g.react(ACCOUNT, &thread, &reply, "eyes", false), done);
    assert_eq!(
        g.react(ACCOUNT, &thread, &reply, "fleet:1250000000000000000", true),
        done,
        "a custom emoji as name:id"
    );
    let d = discord(&env);
    assert_eq!(
        d["reactions"][format!("{CHANNEL}/{starter}")],
        json!(["\u{1F440}"])
    );
    assert_eq!(
        d["reactions"][format!("{starter}/{reply}")],
        json!(["\u{2705}", "fleet:1250000000000000000"])
    );
    let calls: Vec<Value> = d["calls"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|c| {
            c["path"]
                .as_str()
                .is_some_and(|p| p.contains("/reactions/"))
        })
        .cloned()
        .collect();
    assert_eq!(calls.len(), 6, "{calls:#?}");
    assert_eq!(calls[0]["method"], "PUT");
    assert_eq!(
        calls[0]["path"],
        format!("channels/{CHANNEL}/messages/{starter}/reactions/%F0%9F%91%80/@me")
    );
    assert_eq!(calls[3]["method"], "DELETE");
    // A name Fleet does not map is refused without a call; an unknown
    // message is Discord's refusal.
    assert!(matches!(
        g.react(ACCOUNT, &thread, &reply, "no_such_emoji", true),
        Observation::Known(EffectOutcome::Failed { .. })
    ));
    assert!(matches!(
        g.react(
            ACCOUNT,
            &thread,
            &snowflake(1_730_000_300_000),
            "eyes",
            true
        ),
        Observation::Known(EffectOutcome::Failed { .. })
    ));
    assert_eq!(
        discord(&env)["calls"].as_array().unwrap().len(),
        d["calls"].as_array().unwrap().len() + 1
    );
}
