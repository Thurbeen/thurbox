//! `fleet integration`: the hooks Fleet installs for an agent that takes
//! none at launch. Fleet writes only what it owns, under the agent's own
//! config directory, keeps a ledger of it, and removes exactly that. The
//! real `fleet` binary, a stub harness profile with an `[install]` table and
//! a config directory of the test's own.

mod support;

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;
use support::Env;

/// A harness Fleet ships no profile for: an own file in a directory Fleet
/// creates, and a block appended to the user's own config file.
fn stub_profile(version: u32) -> String {
    format!(
        r#"name = "stub"
program = "stub-agent"
authority = "full"

[install]
version = {version}
dir_env = ["STUB_AGENT_HOME"]
dir = "~/.stub-agent"

[[install.files]]
path = "hooks.d/fleet.json"
text = '{{"fleet":{{"integration":"stub","version":{{version}}}},"hooks":["fleet signal idle"]}}'

[[install.blocks]]
path = "config.toml"
text = '''
[[hooks]]
event = "Stop"
command = "fleet signal idle"
'''
"#
    )
}

/// Every file and directory under `dir`, a file with its bytes.
fn tree(dir: &Path) -> BTreeMap<PathBuf, Option<Vec<u8>>> {
    let mut found = BTreeMap::new();
    let mut todo = vec![dir.to_path_buf()];
    while let Some(d) = todo.pop() {
        for entry in fs::read_dir(&d).expect("read dir").flatten() {
            let path = entry.path();
            let rel = path.strip_prefix(dir).unwrap().to_path_buf();
            if path.is_dir() {
                found.insert(rel, None);
                todo.push(path);
            } else {
                found.insert(rel, Some(fs::read(&path).expect("read file")));
            }
        }
    }
    found
}

fn state(env: &Env, agent: &str) -> Value {
    let rows = env
        .fleet(&["integration", "status", agent, "--json"])
        .ok()
        .json();
    rows[0]["state"].clone()
}

fn setup(name: &str) -> (Env, PathBuf) {
    let env = Env::new(name);
    let harnesses = env.root.join("harnesses");
    fs::create_dir_all(&harnesses).expect("harnesses");
    fs::write(harnesses.join("stub.toml"), stub_profile(1)).expect("profile");
    let home = env.root.join(".stub-agent");
    fs::create_dir_all(home.join("hooks")).expect("agent config dir");
    // The user's own config, its last line unterminated, and a hook file of
    // their own beside the directory Fleet's goes in.
    fs::write(home.join("config.toml"), "# mine\nmodel = \"m\"").expect("user config");
    fs::write(home.join("hooks/user.json"), "{\"user\":true}\n").expect("user hook");
    (env, home)
}

#[test]
fn an_installed_integration_is_versioned_and_its_removal_leaves_the_users_tree_as_it_was() {
    let (env, home) = setup("integration-lifecycle");
    let before = tree(&home);
    assert_eq!(state(&env, "stub"), "not-installed");

    env.fleet(&["integration", "install", "stub"]).ok();
    let owned = fs::read_to_string(home.join("hooks.d/fleet.json")).expect("owned file");
    assert!(owned.contains(r#""version":1"#), "{owned}");
    let config = fs::read_to_string(home.join("config.toml")).expect("config");
    assert!(
        config.starts_with("# mine\nmodel = \"m\"\n"),
        "the user's bytes come first: {config}"
    );
    assert!(
        config.contains("# >>> fleet integration stub, version 1"),
        "{config}"
    );
    assert!(config.contains("# <<< fleet integration stub"), "{config}");
    assert_eq!(
        fs::read(home.join("hooks/user.json")).unwrap(),
        before[Path::new("hooks/user.json")].clone().unwrap(),
        "the user's own hook file is untouched"
    );
    assert_eq!(state(&env, "stub"), "current");
    let ledger: Value =
        serde_json::from_slice(&fs::read(env.data.join("integrations.json")).expect("ledger"))
            .expect("ledger JSON");
    assert_eq!(ledger["integrations"]["stub"]["version"], 1, "{ledger:#}");
    // A second install of a current one changes nothing.
    env.fleet(&["integration", "install", "stub"]).ok();
    assert_eq!(
        fs::read_to_string(home.join("config.toml")).unwrap(),
        config
    );

    // A newer profile: what is installed is outdated, and is listed as such.
    fs::write(env.root.join("harnesses/stub.toml"), stub_profile(2)).expect("profile");
    assert_eq!(state(&env, "stub"), "outdated");
    let version = env.fleet(&["version", "--json"]).ok().json();
    assert_eq!(
        version["outdatedIntegrations"],
        serde_json::json!(["stub"]),
        "{version:#}"
    );
    env.fleet(&["integration", "install", "stub"]).ok();
    assert_eq!(state(&env, "stub"), "current");
    let config = fs::read_to_string(home.join("config.toml")).expect("config");
    assert_eq!(
        config.matches(">>> fleet integration stub").count(),
        1,
        "{config}"
    );
    assert!(config.contains("stub, version 2"), "{config}");

    // A person edits Fleet's file: it is modified, and an install refuses to
    // overwrite it.
    fs::write(home.join("hooks.d/fleet.json"), "{}").expect("edit");
    assert_eq!(state(&env, "stub"), "modified");
    env.fleet(&["integration", "install", "stub"])
        .code(1)
        .stderr_has("modified");

    env.fleet(&["integration", "uninstall", "stub"]).ok();
    assert_eq!(tree(&home), before, "the tree is back as it was");
    assert_eq!(state(&env, "stub"), "not-installed");
}

#[test]
fn fleet_never_creates_an_agents_config_dir_and_honours_its_override() {
    let (mut env, home) = setup("integration-dirs");
    let elsewhere = env.root.join("elsewhere");
    env.set_var("STUB_AGENT_HOME", &elsewhere.to_string_lossy());
    assert_eq!(state(&env, "stub"), "agent-missing");
    env.fleet(&["integration", "install", "stub"])
        .code(1)
        .stderr_has("agent-missing");
    assert!(!elsewhere.exists(), "Fleet created the agent's config dir");

    // The override names a directory that is there: Fleet writes there, and
    // not in the default one.
    fs::create_dir_all(&elsewhere).expect("override dir");
    let default = tree(&home);
    env.fleet(&["integration", "install", "stub"]).ok();
    assert!(elsewhere.join("hooks.d/fleet.json").is_file());
    assert_eq!(tree(&home), default);
    env.fleet(&["integration", "uninstall", "--all"]).ok();
    assert_eq!(tree(&elsewhere), BTreeMap::new());
}

#[test]
fn a_launch_time_agent_needs_no_integration() {
    let (env, _) = setup("integration-launch-time");
    assert_eq!(state(&env, "claude"), "not-needed");
    env.fleet(&["integration", "install", "claude"])
        .code(1)
        .stderr_has("not needed: launch-time hooks");
    let all = env
        .fleet(&["integration", "status", "--all", "--json"])
        .ok()
        .json();
    let agents: Vec<&str> = all
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["agent"].as_str().unwrap())
        .collect();
    assert!(
        agents.contains(&"stub") && agents.contains(&"kimi"),
        "{all:#}"
    );
    env.fleet(&["integration", "status", "no-such-agent"])
        .code(1)
        .stderr_has("no harness profile");
}

#[test]
fn a_file_fleet_did_not_write_is_never_taken_over() {
    let (env, home) = setup("integration-foreign");
    fs::create_dir_all(home.join("hooks.d")).unwrap();
    fs::write(home.join("hooks.d/fleet.json"), "theirs").unwrap();
    env.fleet(&["integration", "install", "stub"])
        .code(1)
        .stderr_has("not Fleet's");
    assert_eq!(
        fs::read_to_string(home.join("hooks.d/fleet.json")).unwrap(),
        "theirs"
    );
    assert_eq!(
        fs::read_to_string(home.join("config.toml")).unwrap(),
        "# mine\nmodel = \"m\""
    );
}

#[test]
fn an_integration_is_removed_after_its_profile_is_gone() {
    let (env, home) = setup("integration-orphan");
    let before = tree(&home);
    env.fleet(&["integration", "install", "stub"]).ok();
    fs::remove_file(env.root.join("harnesses/stub.toml")).expect("profile");
    env.fleet(&["integration", "uninstall", "stub"])
        .ok()
        .stdout_has("stub uninstalled");
    assert_eq!(tree(&home), before);
}
