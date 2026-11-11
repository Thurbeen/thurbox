//! Harness profiles own launch, resume and trust: each Task's session is
//! started with its harness profile's argv, and revived with its resume
//! argv on the conversation its hooks reported. The real `fleet` binary,
//! its file store and the fake host, a runtime that only runs an argv.

mod support;

use fleet_runtime::FakeSession;
use serde_json::json;
use support::Env;

fn session(env: &Env, task: &str) -> FakeSession {
    env.host()
        .session(&env.session_key(task))
        .unwrap()
        .expect("a session")
}

#[test]
fn each_harness_starts_with_its_profiles_argv_and_revives_on_its_conversation() {
    let env = Env::new("harness-profiles");
    let mut codex = support::task("c1", true);
    codex["spec"]["harness"] = json!("codex");
    // Codex is trusted in exactly the directory it starts in.
    codex["spec"]["workspace"] = json!([format!("file://{}/work/c1", env.root.display())]);
    env.apply(json!([support::task("t1", true), codex])).ok();
    env.reconcile().ok();
    assert_eq!(env.status("t1")["phase"], "Briefed");
    assert_eq!(env.status("c1")["phase"], "Briefed");

    // Claude Code: its hooks in --settings, reporting through fleet signal.
    let claude = session(&env, "t1");
    assert_eq!(
        claude.argv[..2],
        ["claude", "--settings"],
        "{:?}",
        claude.argv
    );
    let settings: serde_json::Value = serde_json::from_str(&claude.argv[2]).unwrap();
    let prompt = settings["hooks"]["UserPromptSubmit"][0]["hooks"][0]["command"]
        .as_str()
        .unwrap();
    assert!(prompt.starts_with("fleet signal prompt"), "{prompt}");
    // Codex: its notify hook, then its trust of exactly its workspace.
    let codex = session(&env, "c1");
    assert_eq!(codex.argv[0], "codex");
    assert_eq!(
        codex.argv[1..3],
        ["-c", r#"notify=["fleet","signal","idle"]"#]
    );
    assert_eq!(codex.argv[3], "-c");
    assert!(codex.argv[4].starts_with("projects="), "{:?}", codex.argv);

    // The hooks reported each conversation; a reboot revives both on it.
    for (task, id) in [("t1", &claude.id), ("c1", &codex.id)] {
        assert_eq!(
            env.get(task)["status"]["conversation"],
            format!("conv-{id}"),
            "{task}"
        );
    }
    env.host().reboot().unwrap();
    env.reconcile().ok();
    let claude = session(&env, "t1");
    let resumed = claude.restarts.last().expect("revived");
    assert_eq!(
        resumed[resumed.len() - 2..],
        ["--resume".to_string(), format!("conv-{}", claude.id)]
    );
    let codex = session(&env, "c1");
    let resumed = codex.restarts.last().expect("revived");
    assert_eq!(
        resumed[..3],
        [
            "codex".to_string(),
            "resume".into(),
            format!("conv-{}", codex.id)
        ]
    );
}

fn write_profile(env: &Env, file: &str, text: &str) {
    let dir = env.config.parent().unwrap().join("harnesses");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join(file), text).unwrap();
}

const VENDOR: &str = "name = \"vendor\"\nprogram = \"vendor\"\nauthority = \"full\"\n\n[launch]\nenv = { API_KEY = \"profile\", VENDOR_HOOKS = \"on\" }\n";

/// A profile's `[launch] env` sits under the Task's own, whatever the
/// spelling of a name, and Fleet names the profile directory to the hooks.
#[test]
fn a_profiles_launch_env_sits_under_the_tasks_and_names_its_directory() {
    let env = Env::new("harness-env");
    write_profile(&env, "vendor.toml", VENDOR);
    let mut frozen = support::task("t1", true);
    frozen["spec"]["harness"] = json!("vendor");
    frozen["spec"]["env"] = json!({"api_key": "task"});
    let mut bare = support::task("t2", true);
    bare["spec"]["harness"] = json!("vendor");
    env.apply(json!([frozen, bare])).ok();
    env.reconcile().ok();

    let t1 = session(&env, "t1");
    assert_eq!(
        t1.env.get("api_key").map(String::as_str),
        Some("task"),
        "{:?}",
        t1.env
    );
    assert!(!t1.env.contains_key("API_KEY"), "{:?}", t1.env);
    assert_eq!(t1.env.get("VENDOR_HOOKS").map(String::as_str), Some("on"));
    let t2 = session(&env, "t2");
    assert_eq!(t2.env.get("API_KEY").map(String::as_str), Some("profile"));
    let dir = env.config.parent().unwrap().join("harnesses");
    assert_eq!(
        t2.env.get("FLEET_HARNESS_DIR").map(String::as_str),
        Some(&*dir.to_string_lossy())
    );
}

/// A profile file that does not parse stops a command that runs Tasks, but
/// a view still lists every other user harness.
#[test]
fn a_broken_profile_stops_a_reconcile_and_leaves_the_others_listed() {
    let env = Env::new("harness-broken");
    write_profile(&env, "vendor.toml", VENDOR);
    write_profile(&env, "zz-broken.toml", "name = \"broken\"\n");
    env.fleet(&["reconcile"])
        .code(1)
        .stderr_has("zz-broken.toml");
    let version = env.fleet(&["version", "--json"]).ok().json();
    assert_eq!(
        version["harnesses"],
        json!([
            "vendor",
            "claude",
            "claude-code",
            "codex",
            "pi",
            "opencode",
            "kilo",
            "omp",
            "qwen"
        ])
    );
}

/// The `node` on PATH, which runs the plugin files Fleet ships for pi and
/// opencode. Without it a test that runs one says so and skips, except in
/// CI: every runner there has node, so a skip would hide the test.
#[cfg(unix)]
fn node() -> Option<std::path::PathBuf> {
    let found = std::env::var_os("PATH").and_then(|path| {
        std::env::split_paths(&path)
            .map(|dir| dir.join("node"))
            .find(|p| p.is_file())
    });
    if found.is_none() {
        assert!(
            std::env::var_os("CI").is_none(),
            "node is not on PATH in CI, where the plugin files must be run"
        );
        eprintln!("skipping: node is not installed");
    }
    found
}

/// A `fleet` that records each signal it is asked for: its arguments, then
/// the payload on its stdin. Shell builtins only: its PATH holds nothing
/// but itself and `node`.
#[cfg(unix)]
const STUB_FLEET: &str = "#!/bin/sh\nIFS= read -r payload\nprintf '%s\\t%s\\n' \"$*\" \"$payload\" >> \"$FLEET_SIGNAL_FILE\"\n";

/// Run `driver` under `node` on the plugin file `asset`, with `steps`, and
/// the stub `fleet` first on PATH unless `with_fleet` is false; each signal
/// the plugin sent, as its state and its payload. `signal_file`: whether
/// FLEET_SIGNAL_FILE is set, as in a session Fleet started.
#[cfg(unix)]
fn run_asset(
    env: &Env,
    node: &std::path::Path,
    (driver, asset): (&str, &std::path::Path),
    steps: serde_json::Value,
    (with_fleet, signal_file): (bool, bool),
) -> Vec<(String, serde_json::Value)> {
    use std::os::unix::fs::PermissionsExt;
    let bin = env.root.join("stub-bin");
    std::fs::create_dir_all(&bin).unwrap();
    let fleet = bin.join("fleet");
    if with_fleet {
        std::fs::write(&fleet, STUB_FLEET).unwrap();
        std::fs::set_permissions(&fleet, std::fs::Permissions::from_mode(0o755)).unwrap();
    } else {
        let _ = std::fs::remove_file(&fleet);
    }
    let log = env.root.join("signals.log");
    let _ = std::fs::remove_file(&log);
    // The plugin finds `fleet` and nothing else, wherever node is installed.
    let _ = std::fs::remove_file(bin.join("node"));
    std::os::unix::fs::symlink(node, bin.join("node")).unwrap();
    let mut cmd = std::process::Command::new(node);
    cmd.env_clear()
        .env("PATH", &bin)
        .args(["--input-type=module", "-e", driver, "--"])
        .arg(asset)
        .arg(steps.to_string());
    if signal_file {
        cmd.env("FLEET_SIGNAL_FILE", &log);
    }
    let out = cmd.output().expect("node");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    std::fs::read_to_string(&log)
        .unwrap_or_default()
        .lines()
        .map(|l| {
            let (args, payload) = l.split_once('\t').unwrap();
            let state = args.strip_prefix("signal ").unwrap_or(args).to_string();
            (state, serde_json::from_str(payload).unwrap())
        })
        .collect()
}

/// pi's extension API, as much of it as Fleet's extension uses: each step
/// is an event and its payload, given to every handler of the event.
#[cfg(unix)]
const PI_DRIVER: &str = r#"
const [asset, steps] = process.argv.slice(1);
const { default: extension } = await import(asset);
const handlers = new Map();
extension({ on: (name, fn) => handlers.set(name, [...(handlers.get(name) ?? []), fn]) });
const ctx = { sessionManager: { getSessionId: () => "pi-1" } };
for (const [name, event] of JSON.parse(steps)) {
  for (const fn of handlers.get(name) ?? []) await fn(event, ctx);
}
"#;

/// The context an OpenCode 2 plugin's server part gets, as much of it as
/// Fleet's plugin uses, after its TUI part's start. A step `["prompt",
/// event]` runs the prompt hook; a step
/// `["event", event]` is one event of the stream, and the next step waits
/// until the plugin asked for the event after it. `ses-sub` is a subagent's.
/// A plugin that registered nothing is given nothing.
#[cfg(unix)]
const OPENCODE_DRIVER: &str = r#"
const [asset, steps] = process.argv.slice(1);
const { default: plugin } = await import(asset + "/index.js");
const { default: tui } = await import(asset + "/tui.js");
const sessions = { "ses-root": { id: "ses-root" }, "ses-sub": { id: "ses-sub", parentID: "ses-root" } };
let prompt, deliver, onWait, subscribed;
let waiting = new Promise((r) => (onWait = r));
const stream = {
  async *[Symbol.asyncIterator]() {
    for (;;) yield await new Promise((r) => { deliver = r; onWait(); });
  },
};
const ctx = {
  session: {
    hook: async (name, fn) => { if (name === "prompt") prompt = fn; return { dispose() {} }; },
    get: async ({ sessionID }) => sessions[sessionID],
  },
  event: { subscribe: () => ((subscribed = true), stream) },
};
const dispose = await plugin.setup(ctx);
await tui.setup({});
for (const [kind, event] of JSON.parse(steps)) {
  if (kind === "prompt") {
    await prompt?.(event);
  } else if (subscribed) {
    await waiting;
    waiting = new Promise((r) => (onWait = r));
    deliver(event);
    await waiting;
  }
}
dispose?.();
"#;

/// pi and opencode take their hooks at launch from a plugin file Fleet
/// writes under its own data directory: pi with `-e`, opencode through
/// `OPENCODE_CONFIG_CONTENT`, which it merges with the user's config.
#[test]
fn pi_and_opencode_launch_with_the_plugin_files_fleet_writes() {
    let env = Env::new("harness-launch-hooks");
    let mut pi = support::task("p1", true);
    pi["spec"]["harness"] = json!("pi");
    pi["spec"]["workspace"] = json!([format!("file://{}/work/p1", env.root.display())]);
    let mut opencode = support::task("o1", true);
    opencode["spec"]["harness"] = json!("opencode");
    env.apply(json!([pi, opencode])).ok();
    env.reconcile().ok();
    let assets = env.data.join("harness-assets");

    let pi = session(&env, "p1");
    let extension = assets.join("fleet-pi.ts");
    // `--approve`: the project is trusted for this run only.
    assert_eq!(
        pi.argv,
        ["pi", "-e", &*extension.to_string_lossy(), "--approve"],
        "{:?}",
        pi.argv
    );
    assert!(extension.is_file(), "{}", extension.display());

    let opencode = session(&env, "o1");
    // Its own server: a shared background service was started with another
    // environment, and would load neither the plugin nor the signal file.
    assert_eq!(opencode.argv, ["opencode", "--standalone"]);
    let content: serde_json::Value = serde_json::from_str(
        opencode
            .env
            .get("OPENCODE_CONFIG_CONTENT")
            .expect("OPENCODE_CONFIG_CONTENT"),
    )
    .unwrap();
    let plugin = assets.join("fleet-opencode");
    assert_eq!(content, json!({"plugins": [plugin.to_string_lossy()]}));
    for part in ["index.js", "tui.js", "signal.js"] {
        assert!(plugin.join(part).is_file(), "{}/{part}", plugin.display());
    }

    // A revive resumes each on the conversation its hooks reported.
    env.host().reboot().unwrap();
    env.reconcile().ok();
    let pi = session(&env, "p1");
    let resumed = pi.restarts.last().expect("revived");
    assert_eq!(
        resumed[resumed.len() - 2..],
        ["--session".to_string(), format!("conv-{}", pi.id)]
    );
    let opencode = session(&env, "o1");
    let resumed = opencode.restarts.last().expect("revived");
    assert_eq!(
        resumed,
        &[
            "opencode".to_string(),
            "--standalone".into(),
            "--session".into(),
            format!("conv-{}", opencode.id)
        ]
    );
}

/// Fleet's pi extension, run on pi's events: a start, a prompt, a run and
/// its end, each reported with the session id.
#[cfg(unix)]
#[test]
fn the_pi_extension_reports_each_event_with_its_session() {
    let Some(node) = node() else { return };
    let env = Env::new("asset-pi");
    let mut task = support::task("p1", true);
    task["spec"]["harness"] = json!("pi");
    env.apply(json!([task])).ok();
    env.reconcile().ok();
    // Node runs a module by its extension; the file is plain JavaScript.
    let asset = env.root.join("fleet-pi.mjs");
    std::fs::copy(env.data.join("harness-assets/fleet-pi.ts"), &asset).unwrap();
    let steps = json!([
        ["session_start", {"reason": "startup"}],
        ["before_agent_start", {"prompt": "the brief"}],
        ["agent_start", {}],
        ["agent_end", {"messages": []}],
        ["agent_settled", {}],
    ]);
    let sent = run_asset(
        &env,
        &node,
        (PI_DRIVER, &asset),
        steps.clone(),
        (true, true),
    );
    let session = json!({"session_id": "pi-1"});
    assert_eq!(
        sent,
        [
            ("idle".to_string(), session.clone()),
            (
                "prompt".into(),
                json!({"session_id": "pi-1", "prompt": "the brief"})
            ),
            ("working".into(), session.clone()),
            ("idle".into(), session),
        ]
    );
    // Outside a session Fleet started, nothing; without `fleet`, no failure.
    assert!(
        run_asset(
            &env,
            &node,
            (PI_DRIVER, &asset),
            steps.clone(),
            (true, false)
        )
        .is_empty()
    );
    assert!(run_asset(&env, &node, (PI_DRIVER, &asset), steps, (false, true)).is_empty());
}

/// Fleet's opencode plugin, run on opencode's prompt hook and events: a
/// turn whose subagent asks a permission and a question. The agent works
/// until every session ends, waits on a person while a request is open, and
/// a subagent's prompt is neither a prompt nor the conversation.
#[cfg(unix)]
#[test]
fn the_opencode_plugin_reports_each_change_of_state_with_its_session() {
    let Some(node) = node() else { return };
    let env = Env::new("asset-opencode");
    let mut task = support::task("o1", true);
    task["spec"]["harness"] = json!("opencode");
    env.apply(json!([task])).ok();
    env.reconcile().ok();
    let asset = env.data.join("harness-assets/fleet-opencode");
    let event =
        |kind: &str, data: serde_json::Value| json!(["event", {"type": kind, "data": data}]);
    let steps = json!([
        ["prompt", {"sessionID": "ses-root", "prompt": {"text": "the brief"}}],
        event("session.execution.started", json!({"sessionID": "ses-root"})),
        ["prompt", {"sessionID": "ses-sub", "prompt": {"text": "a subagent's task"}}],
        event("session.execution.started", json!({"sessionID": "ses-sub"})),
        event("permission.asked", json!({"id": "per-1", "sessionID": "ses-sub"})),
        event("permission.replied", json!({"requestID": "per-1", "sessionID": "ses-sub"})),
        event("session.execution.succeeded", json!({"sessionID": "ses-sub"})),
        event("form.created", json!({"form": {"id": "frm-1", "sessionID": "ses-root"}})),
        event("form.replied", json!({"id": "frm-1", "sessionID": "ses-root"})),
        event("session.execution.succeeded", json!({"sessionID": "ses-root"})),
    ]);
    let sent = run_asset(
        &env,
        &node,
        (OPENCODE_DRIVER, &asset),
        steps.clone(),
        (true, true),
    );
    let states: Vec<&str> = sent.iter().map(|(s, _)| s.as_str()).collect();
    assert_eq!(
        states,
        [
            "idle", "prompt", "working", "working", "blocked", "working", "working", "blocked",
            "working", "idle"
        ],
        "{sent:?}"
    );
    // The TUI part reports the start, before any session.
    assert_eq!(sent[0].1, json!({}));
    assert_eq!(
        sent[1].1,
        json!({"session_id": "ses-root", "prompt": "the brief"})
    );
    for (state, payload) in &sent[2..] {
        assert_eq!(payload, &json!({"session_id": "ses-root"}), "{state}");
    }
    assert!(
        run_asset(
            &env,
            &node,
            (OPENCODE_DRIVER, &asset),
            steps.clone(),
            (true, false)
        )
        .is_empty()
    );
    assert!(run_asset(&env, &node, (OPENCODE_DRIVER, &asset), steps, (false, true)).is_empty());
}

/// kilo takes its hooks at launch from two files Fleet writes: its server
/// plugin, named in `KILO_CONFIG_CONTENT`, and the terminal UI's config that
/// names its TUI plugin, in `KILO_TUI_CONFIG`. kilo merges each with the
/// user's. `KILO_NO_DAEMON`: a running background service was started with
/// another environment, and would load neither the plugin nor the signal
/// file.
#[test]
fn kilo_launches_with_the_plugin_files_fleet_writes() {
    let env = Env::new("harness-launch-kilo");
    let mut task = support::task("k1", true);
    task["spec"]["harness"] = json!("kilo");
    env.apply(json!([task])).ok();
    env.reconcile().ok();
    let plugin = env.data.join("harness-assets").join("fleet-kilo");

    let kilo = session(&env, "k1");
    assert_eq!(kilo.argv, ["kilo"]);
    let content: serde_json::Value = serde_json::from_str(
        kilo.env
            .get("KILO_CONFIG_CONTENT")
            .expect("KILO_CONFIG_CONTENT"),
    )
    .unwrap();
    let server = plugin.join("index.js");
    assert_eq!(content, json!({"plugin": [server.to_string_lossy()]}));
    let tui = plugin.join("tui.json");
    assert_eq!(
        kilo.env.get("KILO_TUI_CONFIG").map(String::as_str),
        Some(&*tui.to_string_lossy())
    );
    assert_eq!(
        kilo.env.get("KILO_NO_DAEMON").map(String::as_str),
        Some("1")
    );
    for part in ["index.js", "tui.js", "signal.js", "tui.json"] {
        assert!(plugin.join(part).is_file(), "{}/{part}", plugin.display());
    }
    // The TUI config names its plugin beside it, wherever the data dir is.
    let config: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&tui).unwrap()).unwrap();
    assert_eq!(config, json!({"plugin": ["./tui.js"]}));

    // A revive resumes kilo on the conversation its hooks reported.
    env.host().reboot().unwrap();
    env.reconcile().ok();
    let kilo = session(&env, "k1");
    let resumed = kilo.restarts.last().expect("revived");
    assert_eq!(
        resumed,
        &[
            "kilo".to_string(),
            "--session".into(),
            format!("conv-{}", kilo.id)
        ]
    );
}

/// The context a kilo plugin's server part gets, after its TUI part's start.
/// A step `["chat.message", input, output]` runs the prompt hook; a step
/// `["event", event]` is one event of the bus. As kilo does, each handler
/// runs without waiting for the one before it.
#[cfg(unix)]
const KILO_DRIVER: &str = r#"
const [asset, steps] = process.argv.slice(1);
const { default: plugin } = await import(asset + "/index.js");
const { default: tui } = await import(asset + "/tui.js");
const hooks = await plugin.server({});
const running = [tui.tui({})];
for (const [kind, input, output] of JSON.parse(steps)) {
  if (kind === "event") running.push(hooks.event?.({ event: input }));
  else running.push(hooks[kind]?.(input, output));
}
await Promise.all(running);
"#;

/// Fleet's kilo plugin, run on kilo's prompt hook and events: a turn whose
/// subagent asks a permission, then a question, suggestions and network
/// requests the agent asks itself, two of them at once. The
/// agent works until every session is idle, waits on a person while a
/// request is open, and a subagent's prompt is neither a prompt nor the
/// conversation.
#[cfg(unix)]
#[test]
fn the_kilo_plugin_reports_each_change_of_state_with_its_session() {
    let Some(node) = node() else { return };
    let env = Env::new("asset-kilo");
    let mut task = support::task("k1", true);
    task["spec"]["harness"] = json!("kilo");
    env.apply(json!([task])).ok();
    env.reconcile().ok();
    let asset = env.data.join("harness-assets/fleet-kilo");
    let event = |kind: &str, properties: serde_json::Value| json!(["event", {"type": kind, "properties": properties}]);
    let status = |session: &str, kind: &str| {
        event(
            "session.status",
            json!({"sessionID": session, "status": {"type": kind}}),
        )
    };
    let text = |session: &str, text: &str| {
        json!([
            "chat.message",
            {"sessionID": session},
            {"parts": [
                {"type": "text", "text": text},
                {"type": "text", "text": "a reminder kilo adds", "synthetic": true},
            ]}
        ])
    };
    // A request is opened with its `id` and closed with its `requestID`.
    let open = |kind: &str, id: &str| event(kind, json!({"id": id, "sessionID": "ses-root"}));
    let close =
        |kind: &str, id: &str| event(kind, json!({"requestID": id, "sessionID": "ses-root"}));
    let steps = json!([
        event(
            "session.created",
            json!({"sessionID": "ses-root", "info": {"id": "ses-root"}})
        ),
        text("ses-root", "the brief"),
        status("ses-root", "busy"),
        event(
            "session.created",
            json!({"sessionID": "ses-sub", "info": {"id": "ses-sub", "parentID": "ses-root"}})
        ),
        text("ses-sub", "a subagent's task"),
        status("ses-sub", "busy"),
        event(
            "permission.asked",
            json!({"id": "per-1", "sessionID": "ses-sub"})
        ),
        event(
            "permission.replied",
            json!({"requestID": "per-1", "sessionID": "ses-sub", "reply": "once"})
        ),
        status("ses-sub", "idle"),
        event(
            "question.asked",
            json!({"id": "que-1", "sessionID": "ses-root"})
        ),
        event(
            "question.rejected",
            json!({"requestID": "que-1", "sessionID": "ses-root"})
        ),
        // Two requests at once: the agent waits until both are closed.
        open("suggestion.shown", "sug-1"),
        open("session.network.asked", "net-1"),
        close("suggestion.dismissed", "sug-1"),
        close("session.network.replied", "net-1"),
        open("suggestion.shown", "sug-2"),
        close("suggestion.accepted", "sug-2"),
        open("session.network.asked", "net-2"),
        close("session.network.rejected", "net-2"),
        event("message.updated", json!({"sessionID": "ses-root"})),
        status("ses-root", "idle"),
    ]);
    let sent = run_asset(
        &env,
        &node,
        (KILO_DRIVER, &asset),
        steps.clone(),
        (true, true),
    );
    let states: Vec<&str> = sent.iter().map(|(s, _)| s.as_str()).collect();
    assert_eq!(
        states,
        [
            "idle", "prompt", "working", "working", "blocked", "working", "working", "blocked",
            "working", "blocked", "blocked", "blocked", "working", "blocked", "working", "blocked",
            "working", "idle"
        ],
        "{sent:?}"
    );
    // The TUI part reports the start, before any session.
    assert_eq!(sent[0].1, json!({}));
    assert_eq!(
        sent[1].1,
        json!({"session_id": "ses-root", "prompt": "the brief"})
    );
    for (state, payload) in &sent[2..] {
        assert_eq!(payload, &json!({"session_id": "ses-root"}), "{state}");
    }
    assert!(
        run_asset(
            &env,
            &node,
            (KILO_DRIVER, &asset),
            steps.clone(),
            (true, false)
        )
        .is_empty()
    );
    assert!(run_asset(&env, &node, (KILO_DRIVER, &asset), steps, (false, true)).is_empty());
}

/// omp takes its hooks at launch as an extension file Fleet writes, passed
/// with `-e`; qwen as a settings file Fleet writes, named in
/// `QWEN_CODE_SYSTEM_SETTINGS_PATH`, whose hooks qwen merges with the user's
/// and the project's.
#[test]
fn omp_and_qwen_launch_with_the_hook_files_fleet_writes() {
    let env = Env::new("harness-launch-omp-qwen");
    let mut omp = support::task("m1", true);
    omp["spec"]["harness"] = json!("omp");
    let mut qwen = support::task("q1", true);
    qwen["spec"]["harness"] = json!("qwen");
    env.apply(json!([omp, qwen])).ok();
    env.reconcile().ok();
    let assets = env.data.join("harness-assets");

    let omp = session(&env, "m1");
    let extension = assets.join("fleet-omp.ts");
    assert_eq!(omp.argv, ["omp", "-e", &*extension.to_string_lossy()]);
    assert!(extension.is_file(), "{}", extension.display());

    let qwen = session(&env, "q1");
    assert_eq!(qwen.argv, ["qwen"]);
    let settings = assets.join("fleet-qwen.json");
    assert_eq!(
        qwen.env
            .get("QWEN_CODE_SYSTEM_SETTINGS_PATH")
            .map(String::as_str),
        Some(&*settings.to_string_lossy())
    );
    assert!(settings.is_file(), "{}", settings.display());

    // A revive resumes each on the conversation its hooks reported.
    env.host().reboot().unwrap();
    env.reconcile().ok();
    for (task, program) in [("m1", "omp"), ("q1", "qwen")] {
        let s = session(&env, task);
        let resumed = s.restarts.last().expect("revived");
        assert_eq!(resumed[0], program);
        assert_eq!(
            resumed[resumed.len() - 2..],
            ["--resume".to_string(), format!("conv-{}", s.id)],
            "{resumed:?}"
        );
    }
}

/// omp's extension API, as much of it as Fleet's extension uses: each step
/// is an event and its payload, given to every handler of the event without
/// waiting for the one before it. A step whose third element is `"sub"`
/// comes from a subagent, which runs in omp's process with the same
/// extensions, its own session and no UI.
#[cfg(unix)]
const OMP_DRIVER: &str = r#"
const [asset, steps] = process.argv.slice(1);
const { default: extension } = await import(asset);
const handlers = new Map();
extension({ on: (name, fn) => handlers.set(name, [...(handlers.get(name) ?? []), fn]) });
const ctx = (id, hasUI) => ({ hasUI, sessionManager: { getSessionId: () => id } });
const main = ctx("omp-1", true);
const sub = ctx("omp-sub", false);
const running = [];
for (const [name, event, from] of JSON.parse(steps)) {
  for (const fn of handlers.get(name) ?? []) running.push(fn(event, from === "sub" ? sub : main));
}
await Promise.all(running);
"#;

/// Fleet's omp extension, run on omp's events: a start, a prompt, a run
/// with two tool calls waiting on a person at once, a subagent, and the
/// run's end, each reported with the session id. A run that will continue
/// is not an end, and a subagent reports nothing.
#[cfg(unix)]
#[test]
fn the_omp_extension_reports_each_event_with_its_session() {
    let Some(node) = node() else { return };
    let env = Env::new("asset-omp");
    let mut task = support::task("m1", true);
    task["spec"]["harness"] = json!("omp");
    env.apply(json!([task])).ok();
    env.reconcile().ok();
    // Node runs a module by its extension; the file is plain JavaScript.
    let asset = env.root.join("fleet-omp.mjs");
    std::fs::copy(env.data.join("harness-assets/fleet-omp.ts"), &asset).unwrap();
    let approval = |kind: &str, id: &str| json!([kind, {"toolCallId": id, "toolName": "bash"}]);
    let steps = json!([
        ["session_start", {}],
        ["before_agent_start", {"reason": "prompt", "prompt": "the brief"}],
        ["agent_start", {}],
        approval("tool_approval_requested", "call-1"),
        approval("tool_approval_requested", "call-2"),
        approval("tool_approval_resolved", "call-1"),
        approval("tool_approval_resolved", "call-2"),
        ["session_start", {}, "sub"],
        ["before_agent_start", {"reason": "prompt", "prompt": "a subagent's task"}, "sub"],
        ["agent_start", {}, "sub"],
        ["agent_end", {"messages": []}, "sub"],
        ["agent_end", {"messages": [], "willContinue": true}],
        ["agent_end", {"messages": []}],
    ]);
    let sent = run_asset(
        &env,
        &node,
        (OMP_DRIVER, &asset),
        steps.clone(),
        (true, true),
    );
    let states: Vec<&str> = sent.iter().map(|(s, _)| s.as_str()).collect();
    assert_eq!(
        states,
        [
            "idle", "prompt", "working", "blocked", "blocked", "blocked", "working", "idle"
        ],
        "{sent:?}"
    );
    assert_eq!(
        sent[1].1,
        json!({"session_id": "omp-1", "prompt": "the brief"})
    );
    for (state, payload) in sent.iter().filter(|(s, _)| s != "prompt") {
        assert_eq!(payload, &json!({"session_id": "omp-1"}), "{state}");
    }
    assert!(
        run_asset(
            &env,
            &node,
            (OMP_DRIVER, &asset),
            steps.clone(),
            (true, false)
        )
        .is_empty()
    );
    assert!(run_asset(&env, &node, (OMP_DRIVER, &asset), steps, (false, true)).is_empty());
}

/// Each hook command of Fleet's qwen settings, run by `sh` with a qwen hook
/// payload on stdin: the state it sent and the payload `fleet` read, or
/// nothing. `with_fleet`: whether the stub `fleet` is on PATH.
#[cfg(unix)]
fn run_qwen_hook(env: &Env, command: &str, with_fleet: bool) -> Vec<(String, serde_json::Value)> {
    use std::os::unix::fs::PermissionsExt;
    let bin = env.root.join("qwen-bin");
    std::fs::create_dir_all(&bin).unwrap();
    let fleet = bin.join("fleet");
    if with_fleet {
        std::fs::write(&fleet, STUB_FLEET).unwrap();
        std::fs::set_permissions(&fleet, std::fs::Permissions::from_mode(0o755)).unwrap();
    } else {
        let _ = std::fs::remove_file(&fleet);
    }
    let log = env.root.join("qwen-signals.log");
    let _ = std::fs::remove_file(&log);
    let mut child = std::process::Command::new("/bin/sh")
        .args(["-c", command])
        .env_clear()
        .env("PATH", &bin)
        .env("FLEET_SIGNAL_FILE", &log)
        .stdin(std::process::Stdio::piped())
        .spawn()
        .expect("sh");
    use std::io::Write;
    child
        .stdin
        .take()
        .unwrap()
        .write_all(br#"{"session_id":"qwen-1","prompt":"the brief"}"#)
        .unwrap();
    let status = child.wait().unwrap();
    assert!(status.success(), "{command}: {status}");
    std::fs::read_to_string(&log)
        .unwrap_or_default()
        .lines()
        .map(|l| {
            let (args, payload) = l.split_once('\t').unwrap();
            let state = args.strip_prefix("signal ").unwrap_or(args).to_string();
            (state, serde_json::from_str(payload).unwrap())
        })
        .collect()
}

/// Fleet's qwen settings: each hook qwen runs reports its state with qwen's
/// payload, and a missing `fleet` never fails qwen's turn. qwen's own
/// notifications are not used: it sends `idle_prompt` while its approval
/// dialog opens, inside a turn.
#[cfg(unix)]
#[test]
fn the_qwen_settings_report_each_event_through_fleet_signal() {
    let env = Env::new("asset-qwen");
    let mut task = support::task("q1", true);
    task["spec"]["harness"] = json!("qwen");
    env.apply(json!([task])).ok();
    env.reconcile().ok();
    let text = std::fs::read_to_string(env.data.join("harness-assets/fleet-qwen.json")).unwrap();
    let settings: serde_json::Value = serde_json::from_str(&text).unwrap();
    // qwen migrates, and so rewrites, a settings file of an older version.
    assert_eq!(settings["$version"], 4, "{settings:#}");
    let hooks = settings["hooks"].as_object().expect("hooks");
    let mut events: Vec<&str> = hooks.keys().map(String::as_str).collect();
    events.sort_unstable();
    assert_eq!(
        events,
        [
            "PermissionRequest",
            "PostToolUse",
            "PostToolUseFailure",
            "PreToolUse",
            "SessionStart",
            "Stop",
            "StopFailure",
            "UserPromptSubmit"
        ]
    );
    let payload = json!({"session_id": "qwen-1", "prompt": "the brief"});
    for (event, state) in [
        ("SessionStart", "idle"),
        ("UserPromptSubmit", "prompt"),
        ("PreToolUse", "working"),
        ("PermissionRequest", "blocked"),
        ("PostToolUse", "working"),
        ("PostToolUseFailure", "working"),
        ("Stop", "idle"),
        ("StopFailure", "idle"),
    ] {
        let entries = hooks[event].as_array().expect("matchers");
        assert_eq!(entries.len(), 1, "{event}");
        assert!(entries[0].get("matcher").is_none(), "{event}");
        let command = entries[0]["hooks"][0]["command"].as_str().expect(event);
        assert_eq!(entries[0]["hooks"][0]["type"], "command", "{event}");
        assert_eq!(
            run_qwen_hook(&env, command, true),
            [(state.to_string(), payload.clone())],
            "{event}"
        );
        assert!(run_qwen_hook(&env, command, false).is_empty(), "{event}");
    }
}

/// A plugin file is named inside a JSON string (opencode's config), where a
/// double quote cannot stand as it is: a data directory whose path holds one
/// stops each command that starts sessions, and names the directory.
#[test]
fn a_data_directory_with_a_quote_in_its_path_is_refused_before_any_spawn() {
    let env = Env::new("harness-quoted-data");
    std::fs::write(
        &env.config,
        "version = 1\n\n[data]\ndir = 'da\"ta'\n\n[runtime]\nkind = \"fake\"\ndir = \"fake\"\n",
    )
    .unwrap();
    let mut task = support::task("o1", true);
    task["spec"]["harness"] = json!("opencode");
    env.apply(json!([task])).ok();
    env.fleet(&["reconcile"]).code(1).stderr_has("double quote");
    assert!(
        env.host()
            .session(&env.session_key("o1"))
            .unwrap()
            .is_none()
    );
}
