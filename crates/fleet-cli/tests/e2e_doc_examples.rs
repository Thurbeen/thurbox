//! The examples in `docs/fleet/resources/` and the example plugin in
//! `examples/fleet/plugin/` do what the documentation says.
//!
//! Each resource page has an `## Example` section. Every JSON block in it is
//! applied, as written, to a Fleet configured as the pages assume: a GitLab
//! host, a Slack account and the example plugin. The Listener page's
//! manifest is the one `apply` refuses, and the page says so.
//!
//! The plugin is a Python program, so this file runs where `python3` is on
//! PATH: the Linux and macOS jobs.
#![cfg(unix)]

mod support;

use serde_json::{Value, json};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use support::Env;

const PLUGIN: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../examples/fleet/plugin/files.py"
);

/// An environment with the example plugin registered, its files under
/// `root/files`.
fn plugin_env(name: &str) -> Env {
    let env = Env::new(name);
    let files = env.root.join("files");
    fs::create_dir_all(&files).expect("plugin cwd");
    let mut cfg = fs::read_to_string(&env.config).expect("config");
    cfg.push_str(&format!(
        "\n[plugins.files]\nprogram = '{PLUGIN}'\ncwd = '{}'\n",
        files.display()
    ));
    fs::write(&env.config, cfg).expect("config");
    env
}

/// The JSON blocks of a page's `## Example` section.
fn examples(page: &str) -> Vec<Value> {
    let section = page
        .split("\n## Example\n")
        .nth(1)
        .expect("an ## Example section")
        .split("\n## ")
        .next()
        .expect("section");
    section
        .split("```json\n")
        .skip(1)
        .map(|block| {
            let json = block.split("```").next().expect("closed block");
            serde_json::from_str(json).unwrap_or_else(|e| panic!("{e}:\n{json}"))
        })
        .collect()
}

#[test]
fn every_resource_example_applies_as_written() {
    let env = plugin_env("doc-examples");
    let curl = env.root.join("slack-curl.conf");
    fs::write(&curl, "header = \"Authorization: Bearer unused\"\n").expect("curl config");
    fs::write(env.root.join("brief.md"), "Answer the thread.\n").expect("brief");
    let mut cfg = fs::read_to_string(&env.config).expect("config");
    cfg.push_str(&format!(
        "\n[forge.hosts]\n\"gitlab.example.com\" = {{ kind = \"gitlab\", cli = \"glab\" }}\n\
         \n[gateway.accounts.slack-main]\nplatform = \"slack\"\nteam = \"T0123ABCD\"\nbot = \"U0BOT0001\"\n\
         inbound = \"socket-mode\"\ncli = \"curl\"\ncurlConfig = '{}'\n\
         \n[gateway.people.alex]\nslack-main = \"U0123ABCD\"\n\
         \n[gateway.routes.eng]\naccount = \"slack-main\"\nchannels = [\"C0123ABCD\"]\nmention = \"required\"\n\
         allow = [\"alex\"]\ntaskConfig = \"conversation\"\nbrief = '{}'\nworkspace = {{ base = '{}' }}\n",
        curl.display(),
        env.root.join("brief.md").display(),
        env.root.join("work").display(),
    ));
    fs::write(&env.config, cfg).expect("config");

    for page in [
        include_str!("../../../docs/fleet/resources/task.md"),
        include_str!("../../../docs/fleet/resources/task-config.md"),
        include_str!("../../../docs/fleet/resources/env-provider.md"),
        include_str!("../../../docs/fleet/resources/change-request.md"),
        include_str!("../../../docs/fleet/resources/conversation.md"),
        include_str!("../../../docs/fleet/resources/plugin-kinds.md"),
        include_str!("../../../docs/fleet/resources/workspace.md"),
        include_str!("../../../docs/fleet/resources/cron-task.md"),
    ] {
        let docs = examples(page);
        assert!(!docs.is_empty(), "an example with no JSON block");
        for doc in docs {
            env.apply(doc).ok();
        }
    }

    let listener = examples(include_str!("../../../docs/fleet/resources/listener.md"));
    env.apply(listener[0].clone())
        .code(1)
        .stderr_has("created by the manager");

    // This test stops at apply for the ChangeRequest and the Conversation:
    // reconciling them reads gitlab.example.com through `glab` and Slack
    // through `curl`, which a test must not reach. The fake forge and the
    // fake platform take other URL and address shapes than the pages show;
    // e2e_forge and e2e_gateway reconcile those kinds against the fakes.
    // The Task, TaskConfig and EnvProvider examples settle in
    // `the_task_examples_settle_under_the_fake_runtime`, and the plugin
    // kind's in the plugin tests below.
}

#[test]
fn the_task_examples_settle_under_the_fake_runtime() {
    let env = Env::new("doc-task-examples");
    for page in [
        include_str!("../../../docs/fleet/resources/task.md"),
        include_str!("../../../docs/fleet/resources/task-config.md"),
        include_str!("../../../docs/fleet/resources/env-provider.md"),
    ] {
        for doc in examples(page) {
            env.apply(doc).ok();
        }
    }
    let report = env.reconcile().ok().json();
    assert_eq!(report["errors"], json!([]));

    // The dispatched Task is spawned and briefed on `local`.
    assert_eq!(env.status("design-305")["phase"], "Briefed");
    // The Task that uses the TaskConfig is not dispatched: it waits. When it
    // is, its EnvProvider's file source, /work/config/agent-env.json, is
    // read on the manager, and a test cannot create that path; the
    // resolution itself is held by e2e_task_config.
    assert_eq!(env.status("review")["phase"], "Pending");
    for (kind, name) in [("taskconfig", "reviewer"), ("envprovider", "project-env")] {
        assert!(
            env.fleet(&["get", kind, name]).ok().json()["status"].is_null(),
            "{kind}/{name} has no controller and no status"
        );
    }

    // The worker's answer concludes it.
    env.fleet(&[
        "result",
        "design-305",
        "--generation",
        "1",
        "--outcome",
        "shipped",
        "--output",
        "prUrl=\"https://github.com/example/app/pull/1\"",
    ])
    .ok();
    env.reconcile().ok();
    assert_eq!(env.status("design-305")["phase"], "Concluded");
}

#[test]
fn the_example_plugin_writes_its_file_and_reports_ready() {
    let env = plugin_env("doc-plugin");
    let file = env.root.join("files/hello.txt");
    let plugins = env.fleet(&["list", "plugins", "--json"]).ok().json();
    assert_eq!(plugins[0]["name"], "files");
    assert_eq!(plugins[0]["kinds"], json!(["File"]));

    let mut hello = examples(include_str!(
        "../../../docs/fleet/resources/plugin-kinds.md"
    ))
    .remove(0);
    env.apply(hello.clone()).ok();
    let report = env.reconcile().ok().json();
    assert_eq!(report["errors"], json!([]));
    assert_eq!(report["effects"], 1);
    assert_eq!(
        fs::read_to_string(&file).expect("written"),
        hello["spec"]["content"].as_str().unwrap()
    );
    let st = env.fleet(&["get", "file", "hello"]).ok().json()["status"].clone();
    assert!(st["effects"]["write/1/1"]["receipt"]["outcome"]["done"].is_object());

    env.expire_wait_of("File", "hello");
    env.reconcile().ok();
    let st = env.fleet(&["get", "file", "hello"]).ok().json()["status"].clone();
    assert_eq!(st["phase"], "Ready", "{st:#}");
    assert_eq!(st["observedGeneration"], 1);

    // The file drifts after Ready: the next pass writes it again, under a
    // new key, since the first one already has its receipt.
    fs::remove_file(&file).expect("drift");
    env.expire_wait_of("File", "hello");
    env.reconcile().ok();
    assert_eq!(
        fs::read_to_string(&file).expect("repaired"),
        hello["spec"]["content"].as_str().unwrap()
    );

    hello["spec"]["content"] = json!("Hello again.\n");
    env.apply(hello).ok();
    env.reconcile().ok();
    assert_eq!(fs::read_to_string(&file).unwrap(), "Hello again.\n");
    let st = env.fleet(&["get", "file", "hello"]).ok().json()["status"].clone();
    assert!(st["effects"]["write/2/1"]["receipt"]["outcome"]["done"].is_object());

    let mut bad = examples(include_str!(
        "../../../docs/fleet/resources/plugin-kinds.md"
    ))
    .remove(0);
    bad["metadata"]["name"] = json!("escape");
    bad["spec"]["name"] = json!("../outside.txt");
    env.apply(bad).code(1).stderr_has("JSON Schema");

    env.fleet(&["delete", "file", "hello"]).ok();
    env.fleet(&["get", "file", "hello"]).code(3);
}

#[test]
fn the_example_plugin_retries_a_failed_write() {
    let env = plugin_env("doc-plugin-retry");
    // A read-only directory: the plugin reads that the file is absent, and
    // the first write fails.
    let dir = env.root.join("files");
    let blocked = dir.join("hello.txt");
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o555)).expect("read-only");
    let probe = dir.join(".probe");
    if fs::write(&probe, "").is_ok() {
        let _ = fs::remove_file(&probe);
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).expect("restore");
        return; // running as root: the mode does not refuse the write
    }
    let hello = examples(include_str!(
        "../../../docs/fleet/resources/plugin-kinds.md"
    ))
    .remove(0);
    env.apply(hello.clone()).ok();
    env.reconcile().ok();
    let st = env.fleet(&["get", "file", "hello"]).ok().json()["status"].clone();
    let failed = st["effects"]
        .as_object()
        .expect("effects")
        .values()
        .filter(|e| e["receipt"]["outcome"]["failed"].is_object())
        .count();
    // One bounded run may pass the one-second recheck and try again: each
    // attempt has its own key and its own failed receipt.
    assert!(failed >= 1, "{st:#}");

    fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).expect("unblock");
    env.expire_wait_of("File", "hello");
    env.reconcile().ok();
    assert_eq!(
        fs::read_to_string(&blocked).expect("written on retry"),
        hello["spec"]["content"].as_str().unwrap()
    );
}
