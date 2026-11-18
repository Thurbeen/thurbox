//! `fleet migrate`: a Fleet 2 config and store become Fleet 3's, and the 2.x
//! files stay as `.v2`, byte for byte.
mod support;

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};
use support::Env;

const FLEET_2_CONFIG: &str = r#"version = 1

[data]
dir = "store-v1"

[runtime]
kind = "thurbox"
cli = "thurbox-cli"
callTimeoutMs = 120000
keyPrefix = "fold"
workspaceCleanup = "keep"

[runtime.modelEnv]
claude-code = "ANTHROPIC_MODEL"

[runtime.repositories]
"https://example.invalid/app.git" = "/src/app"

[starter]
workspace = "/src/vega"

[workspaces]
kind = "worktree"

[forge]
callTimeoutMs = 30000

[forge.hosts."github.com"]
kind = "github"
"#;

fn object(kind: &str, name: &str, spec: Value, status: Value) -> Value {
    json!({
        "apiVersion": "fleet/v1alpha1",
        "kind": kind,
        "metadata": {
            "name": name,
            "uid": format!("u-{name}"),
            "resourceVersion": 7,
            "generation": 1,
            "creationTimestamp": 1,
            "labels": {"team": "web"},
            "finalizers": ["fleet.dev/release"],
        },
        "spec": spec,
        "status": status,
    })
}

fn fleet_2_store(store: &Path) {
    let objects = [
        object(
            "Task",
            "fix-login",
            json!({
                "harness": "claude-code",
                "host": "devbox",
                "model": "opus",
                "brief": "Fix the login redirect.",
                "workspace": {"repositories": [
                    {"name": "app", "url": "https://example.invalid/app.git", "ref": "main"},
                    {"name": "lib", "url": "https://example.invalid/lib.git"},
                ]},
                "dispatch": true,
            }),
            json!({"phase": "Briefed", "session": {"id": "s1", "host": "devbox", "key": "fix-login.u-fix-login"}}),
        ),
        object(
            "Task",
            "notes",
            json!({
                "harness": "codex",
                "brief": "",
                "workspace": {"dir": "/home/me/notes"},
                "dispatch": false,
            }),
            json!({"phase": "Pending"}),
        ),
        object(
            "Task",
            "done",
            json!({
                "harness": "codex",
                "brief": "Ship it.",
                "workspace": {"dir": "/home/me/done"},
                "dispatch": true,
            }),
            json!({"phase": "Concluded", "result": {"outcome": "shipped"}}),
        ),
        object(
            "TaskConfig",
            "default",
            json!({"pauseAfterMs": 60000, "model": "sonnet"}),
            Value::Null,
        ),
        object(
            "ChangeRequest",
            "github-com-o-r-1",
            json!({"url": "https://github.com/o/r/pull/1"}),
            json!({}),
        ),
    ];
    fs::create_dir_all(store).unwrap();
    fs::write(
        store.join("store.json"),
        r#"{"format":"fleet-store","version":1}"#,
    )
    .unwrap();
    fs::write(store.join("seq"), "7").unwrap();
    for obj in objects {
        let dir = store.join("objects").join(obj["kind"].as_str().unwrap());
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join(format!(
                "{}.json",
                obj["metadata"]["name"].as_str().unwrap()
            )),
            serde_json::to_vec_pretty(&obj).unwrap(),
        )
        .unwrap();
    }
}

fn tree(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    let mut out = BTreeMap::new();
    let mut dirs = vec![root.to_path_buf()];
    while let Some(dir) = dirs.pop() {
        for entry in fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                dirs.push(path);
            } else if path.file_name().is_some_and(|n| n != "bootstrap.lock") {
                let bytes = fs::read(&path).unwrap();
                out.insert(path.strip_prefix(root).unwrap().to_path_buf(), bytes);
            }
        }
    }
    out
}

struct Fleet2 {
    env: Env,
    store: PathBuf,
    config_v2: PathBuf,
    store_v2: PathBuf,
}

fn fleet_2(name: &str) -> Fleet2 {
    let env = Env::without_tools(name);
    fs::write(&env.config, FLEET_2_CONFIG).unwrap();
    let store = env.root.join("store-v1");
    fleet_2_store(&store);
    Fleet2 {
        config_v2: env.root.join("fleet.toml.v2"),
        store_v2: env.root.join("store-v1.v2"),
        store,
        env,
    }
}

fn task(env: &Env, name: &str) -> Value {
    env.fleet(&["get", "task", name]).ok().json()
}

#[test]
fn a_dry_run_prints_the_plan_and_changes_nothing() {
    let f = fleet_2("migrate-dry-run");
    let before = tree(&f.env.root);
    let run = f.env.fleet(&["migrate", "--dry-run"]);
    run.ok()
        .stdout_has("dry run")
        .stdout_has("Task/fix-login")
        .stdout_has("Task/notes")
        .stdout_has("[starter]");
    assert_eq!(before, tree(&f.env.root), "a dry run writes nothing");
}

#[test]
fn a_fleet_2_config_and_store_become_fleet_3_and_the_2x_files_stay() {
    let f = fleet_2("migrate-full");
    let store_before = tree(&f.store);
    let run = f.env.fleet(&["migrate"]);
    run.ok()
        .stdout_has("Task/fix-login")
        .stdout_has("spec.model")
        .stdout_has("mux/devbox")
        .stdout_has("Task/done")
        .stdout_has("ChangeRequest/github-com-o-r-1")
        .stdout_has("[starter]")
        .stdout_has("[workspaces]")
        .stdout_has("modelEnv")
        .stdout_has("dispatch");

    assert_eq!(
        fs::read_to_string(&f.config_v2).unwrap(),
        FLEET_2_CONFIG,
        "the 2.x config is kept as .v2, unchanged"
    );
    assert_eq!(
        tree(&f.store_v2),
        store_before,
        "the 2.x store is kept as .v2, unchanged"
    );

    let config: toml::Table = toml::from_str(&fs::read_to_string(&f.env.config).unwrap()).unwrap();
    assert!(config.get("runtime").is_none(), "{config:#?}");
    assert_eq!(
        config["runtimes"]["mux"]["program"].as_str(),
        Some("runtime-mux")
    );
    assert_eq!(config["forge"]["callTimeoutMs"].as_integer(), Some(30000));
    assert!(config["forge"]["hosts"]["github.com"].is_table());
    assert!(config["forge"]["readyPolicies"].is_table());
    assert!(config.get("starter").is_none() && config.get("workspaces").is_none());

    let fix = task(&f.env, "fix-login");
    assert_eq!(
        fix["spec"]["workspace"],
        json!([
            "git+file:///src/app?branch=fold/fix-login.u-fix-login#main",
            "git+https://example.invalid/lib.git?branch=fold/fix-login.u-fix-login",
        ])
    );
    assert_eq!(fix["spec"]["host"], "mux/devbox");
    assert_eq!(fix["spec"]["dispatch"], false);
    assert!(fix["spec"].get("model").is_none());
    assert_eq!(fix["metadata"]["labels"]["team"], "web");

    let notes = task(&f.env, "notes");
    assert_eq!(notes["spec"]["workspace"], json!(["file:///home/me/notes"]));
    assert_eq!(notes["spec"]["dispatch"], false);

    f.env.fleet(&["get", "task", "done"]).code(3);
    let default = f.env.fleet(&["get", "taskconfig", "default"]).ok().json();
    assert_eq!(default["spec"]["pauseAfterMs"], 60000);
    assert!(default["spec"].get("model").is_none());
    f.env.fleet(&["get", "board", "shared"]).ok();
    f.env.fleet(&["get", "board", "fleet"]).ok();
}

#[test]
fn a_second_run_finds_nothing_to_migrate_and_changes_nothing() {
    let f = fleet_2("migrate-twice");
    f.env.fleet(&["migrate"]).ok();
    let after = tree(&f.env.root);
    f.env
        .fleet(&["migrate"])
        .ok()
        .stdout_has("nothing to migrate");
    assert_eq!(after, tree(&f.env.root));
}

fn copy_tree(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap() {
        let path = entry.unwrap().path();
        let target = to.join(path.file_name().unwrap());
        if path.is_dir() {
            copy_tree(&path, &target);
        } else {
            fs::copy(&path, &target).unwrap();
        }
    }
}

fn migrated_store(name: &str, to: &Path) {
    let twin = fleet_2(name);
    twin.env.fleet(&["migrate"]).ok();
    copy_tree(&twin.store, to);
}

fn journal(f: &Fleet2) {
    fs::write(
        f.env.root.join("fleet.toml.migrating"),
        r#"{"store2x":true}"#,
    )
    .unwrap();
}

fn completes(f: &Fleet2, store_2x: &BTreeMap<PathBuf, Vec<u8>>) {
    f.env.fleet(&["migrate"]).ok().stdout_has("Task/fix-login");
    assert_eq!(fs::read_to_string(&f.config_v2).unwrap(), FLEET_2_CONFIG);
    assert_eq!(&tree(&f.store_v2), store_2x);
    assert!(!f.env.root.join("fleet.toml.migrating").exists());
    assert!(!f.env.root.join("store-v1.v3").exists());
    assert_eq!(task(&f.env, "notes")["spec"]["dispatch"], false);
    f.env
        .fleet(&["migrate"])
        .ok()
        .stdout_has("nothing to migrate");
}

#[test]
fn a_migration_stopped_after_staging_completes_from_the_2x_store_as_it_is_now() {
    let f = fleet_2("migrate-stop-staged");
    journal(&f);
    migrated_store("migrate-stop-staged-twin", &f.env.root.join("store-v1.v3"));
    let later = object(
        "Task",
        "later",
        json!({"harness": "codex", "brief": "", "workspace": {"dir": "/home/me/later"}, "dispatch": true}),
        json!({"phase": "Pending"}),
    );
    fs::write(
        f.store.join("objects/Task/later.json"),
        serde_json::to_vec_pretty(&later).unwrap(),
    )
    .unwrap();
    let store_2x = tree(&f.store);
    completes(&f, &store_2x);
    assert_eq!(task(&f.env, "later")["spec"]["dispatch"], false);
}

#[test]
fn a_migration_stopped_after_moving_the_2x_store_aside_completes() {
    let f = fleet_2("migrate-stop-moved");
    let store_2x = tree(&f.store);
    journal(&f);
    fs::rename(&f.store, &f.store_v2).unwrap();
    migrated_store("migrate-stop-moved-twin", &f.env.root.join("store-v1.v3"));
    completes(&f, &store_2x);
}

#[test]
fn a_migration_stopped_before_writing_the_config_completes() {
    let f = fleet_2("migrate-stop-config");
    let store_2x = tree(&f.store);
    journal(&f);
    fs::rename(&f.store, &f.store_v2).unwrap();
    migrated_store("migrate-stop-config-twin", &f.store);
    fs::copy(&f.env.config, &f.config_v2).unwrap();
    completes(&f, &store_2x);
}

#[test]
fn a_partial_copy_of_the_2x_config_is_written_again() {
    let f = fleet_2("migrate-stop-copy");
    let store_2x = tree(&f.store);
    journal(&f);
    migrated_store("migrate-stop-copy-twin", &f.env.root.join("store-v1.v3"));
    fs::write(&f.config_v2, "version = 1\n[da").unwrap();
    completes(&f, &store_2x);
}

#[test]
fn a_journal_left_after_the_config_was_written_is_removed() {
    let f = fleet_2("migrate-stop-journal");
    f.env.fleet(&["migrate"]).ok();
    journal(&f);
    f.env
        .fleet(&["migrate"])
        .ok()
        .stdout_has("nothing to migrate");
    assert!(!f.env.root.join("fleet.toml.migrating").exists());
}

#[test]
fn a_staged_store_left_without_a_journal_is_never_installed() {
    let f = fleet_2("migrate-stale-v3");
    fs::create_dir_all(f.env.root.join("store-v1.v3")).unwrap();
    let before = tree(&f.env.root);
    f.env.fleet(&["migrate"]).code(1).stderr_has("store-v1.v3");
    assert_eq!(before, tree(&f.env.root));
}

#[test]
fn a_config_without_a_data_dir_migrates_the_default_store() {
    let env = Env::without_tools("migrate-default-store");
    let config = FLEET_2_CONFIG.replace("[data]\ndir = \"store-v1\"\n", "");
    fs::write(&env.config, &config).unwrap();
    let store = env.root.join(".local/share/fleet");
    fleet_2_store(&store);
    let store_2x = tree(&store);
    env.fleet(&["migrate"]).ok().stdout_has("Task/notes");
    assert_eq!(tree(&env.root.join(".local/share/fleet.v2")), store_2x);
    assert_eq!(task(&env, "notes")["spec"]["dispatch"], false);
}

#[test]
fn an_expired_fleet_2_lease_does_not_stop_the_migration() {
    let f = fleet_2("migrate-expired-lease");
    fs::write(
        f.store.join("lease.json"),
        json!({"holder": "fleet-up-1@boot", "token": 1, "expiresAt": 1}).to_string(),
    )
    .unwrap();
    f.env.fleet(&["migrate"]).ok();
}

#[test]
fn a_left_over_v2_copy_is_never_overwritten() {
    let f = fleet_2("migrate-v2-taken");
    fs::write(&f.config_v2, "an older copy").unwrap();
    f.env
        .fleet(&["migrate"])
        .code(1)
        .stderr_has("fleet.toml.v2");
    assert_eq!(fs::read_to_string(&f.config_v2).unwrap(), "an older copy");
    assert_eq!(fs::read_to_string(&f.env.config).unwrap(), FLEET_2_CONFIG);
}

#[test]
fn a_fleet_2_manager_holding_the_store_stops_the_migration() {
    let f = fleet_2("migrate-lease");
    fs::write(
        f.store.join("lease.json"),
        json!({"holder": "fleet-up-1@boot", "token": 1, "expiresAt": u64::MAX / 2}).to_string(),
    )
    .unwrap();
    let before = tree(&f.env.root);
    f.env
        .fleet(&["migrate"])
        .code(1)
        .stderr_has("fleet-up-1@boot");
    assert_eq!(before, tree(&f.env.root));
}

#[test]
fn a_fleet_2_config_names_fleet_migrate() {
    let f = fleet_2("migrate-named");
    f.env
        .fleet(&["list", "task"])
        .code(1)
        .stderr_has("is a Fleet 2.x config")
        .stderr_has("fleet migrate");
}
