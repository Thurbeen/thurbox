#![cfg(unix)]

mod support;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::{Value, json};
use support::{ALEX_ID, Env, Run, conversation_name, now_ms};

const ACCOUNT: &str = "file-main";
const PLATFORM: &str = "file";
const BOT: &str = "B0FLEET001";
const CHANNEL: &str = "C0123ABCD";
const STRANGER: &str = "U0999ZZZZ";
const T0: u64 = 1_730_000_000_000;

fn jq_runs() -> bool {
    if Command::new("jq").arg("--version").output().is_ok() {
        return true;
    }
    assert!(
        std::env::var_os("CI").is_none(),
        "jq is not installed under CI, where the gateway connector tests must run"
    );
    eprintln!("skipping: jq is not installed");
    false
}

fn reference_connector() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../examples/gateway-connectors/file.sh")
        .canonicalize()
        .expect("the reference connector")
}

fn platform_dir(env: &Env) -> PathBuf {
    env.root.join("platform")
}

fn credentials(env: &Env) -> PathBuf {
    env.root.join("secrets/file-connector.conf")
}

fn section(env: &Env) -> String {
    let root = env.root.to_string_lossy().replace('\\', "/");
    format!(
        "\n[gateway]\ncallTimeoutMs = 10000\n\n\
[gateway.connectors.{PLATFORM}]\nprogram = '{program}'\n\n\
[gateway.accounts.{ACCOUNT}]\nplatform = \"{PLATFORM}\"\nbot = \"{BOT}\"\ninbound = \"connection\"\n\
credentials = '{credentials}'\noptions = {{ dir = '{platform}' }}\n\n\
[gateway.people.alex]\n{ACCOUNT} = \"{ALEX_ID}\"\n\n\
[gateway.routes.eng]\naccount = \"{ACCOUNT}\"\nchannels = [\"{CHANNEL}\"]\nmention = \"required\"\nallow = [\"alex\"]\n\
taskConfig = \"conversation\"\nbrief = \"{root}/briefs/conversation.md\"\nworkspace = {{ base = \"{root}/work/conversations\" }}\n",
        program = reference_connector().display(),
        credentials = credentials(env).display(),
        platform = platform_dir(env).display(),
    )
}

fn connector_env(name: &str) -> Option<Env> {
    if !jq_runs() {
        return None;
    }
    let env = Env::gateway(name);
    env.set_gateway(&section(&env));
    fs::create_dir_all(credentials(&env).parent().unwrap()).unwrap();
    fs::write(credentials(&env), "token = \"s3cr3t-never-stored\"\n").unwrap();
    env.apply(support::object(
        "TaskConfig",
        "conversation",
        json!({"harness": "claude-code", "host": "local"}),
    ))
    .ok();
    Some(env)
}

fn platform_message(id: u64, user: &str, text: &str) -> Value {
    json!({"id": id.to_string(), "channel": CHANNEL, "user": user, "name": "Someone",
           "text": text, "ts": id})
}

fn said(env: &Env, delivery: &str, message: Value) -> Run {
    let dir = platform_dir(env);
    fs::create_dir_all(&dir).unwrap();
    let mut log = fs::read_to_string(dir.join("messages.jsonl")).unwrap_or_default();
    log.push_str(&format!("{message}\n"));
    fs::write(dir.join("messages.jsonl"), log).unwrap();
    let envelope = json!({
        "verified": "connection",
        "receivedAt": now_ms(),
        "payload": {"delivery": delivery, "message": message},
    });
    env.fleet_stdin(
        &["gateway", "receive", "--account", ACCOUNT, "--json"],
        &envelope.to_string(),
    )
}

fn platform_lines(env: &Env, file: &str) -> Vec<Value> {
    fs::read_to_string(platform_dir(env).join(file))
        .unwrap_or_default()
        .lines()
        .map(|l| serde_json::from_str(l).expect("a JSON line"))
        .collect()
}

fn fleet_posts(env: &Env) -> Vec<Value> {
    platform_lines(env, "messages.jsonl")
        .into_iter()
        .filter(|m| m["user"] == BOT)
        .collect()
}

fn tasks(env: &Env) -> Vec<Value> {
    env.fleet(&["list", "tasks", "--json"])
        .ok()
        .json()
        .as_array()
        .unwrap()
        .clone()
}

fn connector_call(env: &Env, verb: &str, request: Value) -> Value {
    let mut request = request;
    request["protocol"] = json!(1);
    request["account"] = json!({"platform": PLATFORM, "bot": BOT, "inbound": "connection",
                                "options": {"dir": platform_dir(env)}});
    let mut child = Command::new(reference_connector())
        .arg(verb)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    use std::io::Write;
    child
        .stdin
        .take()
        .unwrap()
        .write_all(request.to_string().as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success(), "{out:?}");
    serde_json::from_slice(&out.stdout).unwrap()
}

#[test]
fn the_reference_connectors_history_keeps_the_newest_and_reads_a_channels_top_level() {
    let Some(env) = connector_env("connector-history") else {
        return;
    };
    let dir = platform_dir(&env);
    fs::create_dir_all(&dir).unwrap();
    let mut lines = String::new();
    for n in 0..5u64 {
        let mut m = platform_message(T0 + n, ALEX_ID, &format!("message {n}"));
        m["thread"] = json!(T0.to_string());
        lines.push_str(&format!("{m}\n"));
    }
    lines.push_str(&format!(
        "{}\n",
        platform_message(T0 + 10, ALEX_ID, "top level")
    ));
    fs::write(dir.join("messages.jsonl"), lines).unwrap();
    let thread = json!({"platform": PLATFORM, "chat": CHANNEL, "thread": T0.to_string()});
    let out = connector_call(
        &env,
        "history",
        json!({"conversation": thread, "query": {"type": "after", "id": null, "limit": 2}}),
    );
    let ids: Vec<&str> = out["known"]["messages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["id"].as_str().unwrap())
        .collect();
    assert_eq!(
        ids,
        [(T0 + 3).to_string(), (T0 + 4).to_string()],
        "the newest two: {out:#}"
    );
    assert_eq!(out["known"]["leftOut"], 3, "{out:#}");
    let channel = json!({"platform": PLATFORM, "chat": CHANNEL});
    let out = connector_call(
        &env,
        "history",
        json!({"conversation": channel, "query": {"type": "since", "at": T0}}),
    );
    let ids: Vec<&str> = out["known"]["messages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["id"].as_str().unwrap())
        .collect();
    assert_eq!(
        ids,
        [T0.to_string(), (T0 + 10).to_string()],
        "a channel reads its top-level messages: {out:#}"
    );
}

#[test]
fn a_strangers_mention_is_mirrored_and_refused_once_through_the_connector() {
    let Some(env) = connector_env("connector-refusal") else {
        return;
    };
    let out = said(
        &env,
        "Ev01",
        platform_message(T0, STRANGER, &format!("<@{BOT}> deploy please")),
    )
    .ok()
    .json();
    assert_eq!(out["outcome"], "written", "{out:#}");
    let thread = format!("{PLATFORM}:{CHANNEL}:{T0}");
    let conversation = conversation_name(ACCOUNT, &thread);
    let obj = env
        .fleet(&["get", "conversation", &conversation])
        .ok()
        .json();
    assert_eq!(obj["spec"]["address"], thread, "{obj:#}");
    for _ in 0..2 {
        let report = env.reconcile().ok().json();
        assert_eq!(report["errors"], json!([]), "{report:#}");
    }
    let posts = fleet_posts(&env);
    assert_eq!(posts.len(), 1, "one refusal, posted once: {posts:#?}");
    assert_eq!(posts[0]["thread"], T0.to_string());
    assert_eq!(posts[0]["text"], fleet_api::gateway::DEFAULT_REFUSAL);
    assert!(posts[0]["key"].as_str().is_some_and(|k| !k.is_empty()));
    assert_eq!(tasks(&env), Vec::<Value>::new());
}

#[test]
fn an_allowed_mention_through_the_connector_starts_a_task() {
    let Some(env) = connector_env("connector-admits") else {
        return;
    };
    let out = said(
        &env,
        "Ev01",
        platform_message(T0, ALEX_ID, &format!("<@{BOT}> fix the build")),
    )
    .ok()
    .json();
    assert_eq!(out["outcome"], "written", "{out:#}");
    let report = env.reconcile().ok().json();
    assert_eq!(report["errors"], json!([]), "{report:#}");
    let thread = format!("{PLATFORM}:{CHANNEL}:{T0}");
    let task = env.get(&format!("{}-1", conversation_name(ACCOUNT, &thread)));
    assert_eq!(
        task["metadata"]["labels"]["gateway.fleet.dev/route"], "eng",
        "{task:#}"
    );
    let again = said(
        &env,
        "Ev01",
        platform_message(T0, ALEX_ID, &format!("<@{BOT}> fix the build")),
    )
    .ok()
    .json();
    assert_ne!(
        again["outcome"], "written",
        "a repeat delivery is no news: {again:#}"
    );
}

#[test]
fn the_connector_gets_the_credentials_path_and_options_which_fleet_never_stores() {
    let Some(env) = connector_env("connector-secrets") else {
        return;
    };
    said(
        &env,
        "Ev01",
        platform_message(T0, STRANGER, &format!("<@{BOT}> hello")),
    )
    .ok();
    env.reconcile().ok();
    let requests = platform_lines(&env, "requests.jsonl");
    let verbs: Vec<&str> = requests.iter().filter_map(|r| r["verb"].as_str()).collect();
    for verb in ["event", "post"] {
        assert!(verbs.contains(&verb), "{verb} in {verbs:?}");
    }
    for r in &requests {
        assert_eq!(
            r["account"]["credentials"],
            credentials(&env).to_string_lossy().as_ref(),
            "{r:#}"
        );
        assert_eq!(r["account"]["bot"], BOT, "{r:#}");
    }
    let posted = requests.iter().find(|r| r["verb"] == "post").unwrap();
    assert_eq!(posted["account"]["name"], ACCOUNT, "{posted:#}");
    let secret = credentials(&env).to_string_lossy().into_owned();
    let mut stack = vec![env.data.clone()];
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                stack.push(path);
            } else if let Ok(bytes) = fs::read(&path) {
                let text = String::from_utf8_lossy(&bytes);
                assert!(
                    !text.contains(&secret) && !text.contains("s3cr3t"),
                    "{} holds the credentials",
                    path.display()
                );
            }
        }
    }
}

#[test]
fn a_connector_fleet_cannot_serve_is_refused_with_its_reason() {
    let Some(env) = connector_env("connector-refused") else {
        return;
    };
    let base = section(&env);
    env.set_gateway(&base.replace(
        &format!("[gateway.connectors.{PLATFORM}]"),
        "[gateway.connectors.slack]",
    ));
    env.reconcile().code(1).stderr_has(
        "gateway.connectors.slack: slack is a built-in platform; name the connector otherwise",
    );
    env.set_gateway(&base.replace(
        &format!("platform = \"{PLATFORM}\""),
        "platform = \"teams\"",
    ));
    env.reconcile().code(1).stderr_has(
        "accounts.file-main: unknown platform \"teams\"; this build has discord, fake, slack, and the connectors file",
    );
    env.set_gateway(&base.replace(
        &format!("credentials = '{}'", credentials(&env).display()),
        "credentials = 'relative.conf'",
    ));
    env.reconcile()
        .code(1)
        .stderr_has("gateway.accounts.file-main: credentials must be an absolute path");
    let missing = env.root.join("no-such-connector");
    env.set_gateway(&base.replace(
        &reference_connector().display().to_string(),
        &missing.display().to_string(),
    ));
    let out = said(
        &env,
        "Ev01",
        platform_message(T0, STRANGER, &format!("<@{BOT}> hi")),
    );
    assert_ne!(out.code, Some(0), "{out:?}");
    out.stderr_has("connector file");
    let wrong = env.root.join("wrong-platform.sh");
    fs::write(
        &wrong,
        "#!/bin/sh\ncat >/dev/null\nprintf '%s\\n' '{\"known\":{\"protocol\":1,\"platform\":\"other\",\"verbs\":[\"manifest\",\"parse\",\"event\",\"whoami\",\"capabilities\",\"post\"]}}'\n",
    )
    .unwrap();
    Command::new("chmod")
        .arg("755")
        .arg(&wrong)
        .status()
        .unwrap();
    env.set_gateway(&base.replace(
        &reference_connector().display().to_string(),
        &wrong.display().to_string(),
    ));
    let out = said(
        &env,
        "Ev02",
        platform_message(T0 + 1, STRANGER, &format!("<@{BOT}> hi")),
    );
    assert_ne!(out.code, Some(0), "{out:?}");
    out.stderr_has("connector file speaks for platform \"other\", not \"file\"");
}

#[test]
fn fleet_version_names_the_gateway_connector_protocol() {
    let env = Env::new("connector-version");
    let v = env.fleet(&["version", "--json"]).ok().json();
    assert_eq!(v["gatewayProtocol"], 1, "{v:#}");
}

fn check(env: &Env, args: &[&str]) -> Run {
    let program = reference_connector();
    let mut argv = vec!["gateway", "check"];
    argv.extend_from_slice(args);
    argv.extend_from_slice(&["--", program.to_str().unwrap()]);
    env.fleet(&argv)
}

#[test]
fn the_reference_connector_passes_the_offline_conformance_suite() {
    let Some(env) = connector_env("connector-check") else {
        return;
    };
    let dir = format!("dir={}", platform_dir(&env).display());
    let run = check(&env, &["--platform", PLATFORM, "--option", &dir]);
    run.ok()
        .stdout_has("ok   the manifest names protocol 1, its platform and every required verb")
        .stdout_has("ok   a request on another protocol answers Unknown")
        .stdout_has("ok   an unknown verb answers Unknown")
        .stdout_has("ok   an address it parses keeps its parts")
        .stdout_has("ok   an event it cannot read answers null or Unknown")
        .stdout_has("ok   whoami names an id")
        .stdout_has("ok   capabilities promise only verbs the manifest lists")
        .stdout_has("skip a post with a repeated intent key lands once: no --channel")
        .stdout_has("speaks gateway protocol 1 for platform file");
    assert!(!run.stdout.contains("FAIL"), "{}", run.stdout);
    assert!(
        fleet_posts(&env).is_empty(),
        "an offline check posts nothing"
    );
}

#[test]
fn the_reference_connector_passes_the_live_conformance_cases_on_a_channel() {
    let Some(env) = connector_env("connector-check-live") else {
        return;
    };
    let dir = format!("dir={}", platform_dir(&env).display());
    let channel = format!("{PLATFORM}:{CHANNEL}");
    let run = check(
        &env,
        &[
            "--platform",
            PLATFORM,
            "--option",
            &dir,
            "--bot",
            BOT,
            "--channel",
            &channel,
        ],
    );
    run.ok()
        .stdout_has("ok   a post answers posted")
        .stdout_has("ok   a post with a repeated intent key lands once")
        .stdout_has("ok   history since the post holds it under its key");
    assert!(!run.stdout.contains("FAIL"), "{}", run.stdout);
    let posts = fleet_posts(&env);
    assert_eq!(posts.len(), 1, "{posts:#?}");
}

#[test]
fn a_connector_that_breaks_the_protocol_fails_the_check_by_name() {
    let env = Env::new("connector-check-broken");
    let broken = env.root.join("broken.sh");
    fs::write(
        &broken,
        "#!/bin/sh\ncat >/dev/null\ncase $1 in\n\
manifest) echo '{\"known\":{\"protocol\":1,\"platform\":\"check\",\"verbs\":[\"manifest\",\"parse\",\"event\",\"whoami\",\"capabilities\",\"post\"]}}' ;;\n\
capabilities) echo '{\"known\":{\"history\":true,\"marker\":\"none\",\"unmentioned\":false,\"threads\":true}}' ;;\n\
*) echo '{\"known\":{\"id\":\"anything\"}}' ;;\nesac\n",
    )
    .unwrap();
    Command::new("chmod")
        .arg("755")
        .arg(&broken)
        .status()
        .unwrap();
    let run = env.fleet(&["gateway", "check", "--", broken.to_str().unwrap()]);
    run.code(1).stderr_has("conformance cases failed for");
    for line in [
        "FAIL a request on another protocol answers Unknown",
        "FAIL an unknown verb answers Unknown",
        "FAIL an address it parses keeps its parts",
        "FAIL capabilities promise only verbs the manifest lists: history is true and the manifest lists no history",
    ] {
        assert!(run.stdout.contains(line), "{line} in\n{}", run.stdout);
    }
}
