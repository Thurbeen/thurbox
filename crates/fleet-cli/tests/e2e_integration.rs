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
# Its hooks never run, so Windows refuses nothing here, and the
# Windows write path (the backup, the lock) is exercised.
windows = true
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

#[test]
fn a_reinstall_that_cannot_complete_keeps_the_working_integration() {
    let (mut env, home) = setup("integration-keep");
    env.fleet(&["integration", "install", "stub"]).ok();
    let installed = tree(&home);
    fs::write(env.root.join("harnesses/stub.toml"), stub_profile(2)).expect("profile");
    // The agent now reads a directory that is not there: nothing is removed.
    let absent = env.root.join("absent");
    env.set_var("STUB_AGENT_HOME", &absent.to_string_lossy());
    env.fleet(&["integration", "install", "stub"])
        .code(1)
        .stderr_has("agent-missing");
    assert_eq!(tree(&home), installed, "the working hooks were removed");
}

#[test]
fn a_block_whose_closing_line_is_gone_is_modified_and_never_extended() {
    let (env, home) = setup("integration-broken-fence");
    env.fleet(&["integration", "install", "stub"]).ok();
    let config = fs::read_to_string(home.join("config.toml")).unwrap();
    let broken = config.replace("# <<< fleet integration stub\n", "");
    let broken = format!("{broken}user = 1\n");
    fs::write(home.join("config.toml"), &broken).unwrap();
    assert_eq!(state(&env, "stub"), "modified");
    env.fleet(&["integration", "install", "stub"]).code(1);
    env.fleet(&["integration", "uninstall", "stub"]).code(1);
    assert_eq!(
        fs::read_to_string(home.join("config.toml")).unwrap(),
        broken,
        "a broken block is left for a person"
    );
}

/// With no home and no override, the refusal names the variable this
/// platform reads the home from: `USERPROFILE` on Windows, `HOME` elsewhere.
#[test]
fn an_install_without_a_home_names_the_variable_to_set() {
    let (env, _home) = setup("integration-no-home");
    let mut cmd = env.command(&["integration", "install", "stub"]);
    cmd.env_remove("HOME").env_remove("USERPROFILE");
    let home = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
    support::wait(cmd.spawn().expect("spawn fleet"))
        .code(1)
        .stderr_has(&format!("set {home} or one of"));
}

/// The test process holds the lock through the standard library, which is
/// `flock` on Unix and `LockFileEx` on Windows, so this runs everywhere,
/// Windows included, where no `flock` program is.
#[test]
fn a_lock_held_by_another_process_stops_install_until_it_is_released() {
    let (env, home) = setup("integration-lock-std");
    env.fleet(&["list", "task"]).ok();
    let held = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .open(env.data.join("integrations.lock"))
        .expect("lock file");
    held.lock().expect("take the lock");
    let before = tree(&home);
    env.fleet(&["integration", "install", "stub"])
        .code(1)
        .stderr_has("integrations.lock");
    assert_eq!(tree(&home), before);
    held.unlock().expect("release the lock");
    env.fleet(&["integration", "install", "stub"]).ok();
    assert_eq!(state(&env, "stub"), "current");
}

/// `flock` on PATH, which holds a lock the way a `fleet` process does; a
/// system without it skips the test.
fn flock() -> Option<PathBuf> {
    let found = std::env::var_os("PATH").and_then(|path| {
        std::env::split_paths(&path)
            .map(|dir| dir.join("flock"))
            .find(|p| p.is_file())
    });
    if found.is_none() {
        eprintln!("skipping: flock is not installed");
    }
    found
}

#[test]
fn a_lock_held_by_a_live_command_waits_and_one_left_by_a_killed_command_does_not() {
    let Some(flock) = flock() else {
        return;
    };
    let (env, home) = setup("integration-lock");
    env.fleet(&["list", "task"]).ok();
    let lock = env.data.join("integrations.lock");
    // A holder that keeps the lock, then dies as a killed command does.
    // One process from lock to death: the shell takes the lock on its fd 9
    // and becomes `sleep`, which keeps it.
    let mut holder = std::process::Command::new("sh")
        .arg("-c")
        .arg(r#"exec 9>"$1"; "$2" 9; exec sleep 300"#)
        .arg("sh")
        .arg(&lock)
        .arg(&flock)
        .spawn()
        .expect("holder");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        let file = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .open(&lock);
        if file.is_ok_and(|f| f.try_lock().is_err()) {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "flock never held the lock"
        );
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    let before = tree(&home);
    env.fleet(&["integration", "install", "stub"])
        .code(1)
        .stderr_has("integrations.lock");
    assert_eq!(tree(&home), before);
    holder.kill().expect("kill");
    holder.wait().expect("reap");
    env.fleet(&["integration", "install", "stub"]).ok();
    assert_eq!(state(&env, "stub"), "current");
}

#[test]
fn an_upgrade_never_takes_a_file_whose_block_alone_was_fleets() {
    let (env, home) = setup("integration-block-to-file");
    env.fleet(&["integration", "install", "stub"]).ok();
    let installed = tree(&home);
    let ledger = fs::read(env.data.join("integrations.json")).expect("ledger");
    // Version 2 claims the whole of the file whose block alone Fleet owns.
    let whole = stub_profile(2).replace(
        "[[install.blocks]]\npath = \"config.toml\"",
        "[[install.files]]\npath = \"config.toml\"",
    );
    fs::write(env.root.join("harnesses/stub.toml"), whole).expect("profile");
    env.fleet(&["integration", "install", "stub"])
        .code(1)
        .stderr_has("not Fleet's");
    assert_eq!(tree(&home), installed, "the user's settings were touched");
    assert_eq!(
        fs::read(env.data.join("integrations.json")).unwrap(),
        ledger,
        "the old integration is still the one recorded"
    );
}

#[test]
fn a_block_naming_fleet_that_the_ledger_does_not_list_is_never_extended() {
    let (env, home) = setup("integration-unlisted-block");
    // A block of Fleet's shape that no ledger entry records: from another
    // data directory, or copied by hand.
    let stray = "# mine\nmodel = \"m\"\n\n# >>> fleet integration stub, version 1: `fleet integration uninstall stub` removes this block\n[[hooks]]\nevent = \"Stop\"\n# <<< fleet integration stub\n";
    fs::write(home.join("config.toml"), stray).unwrap();
    env.fleet(&["integration", "install", "stub"])
        .code(1)
        .stderr_has("the ledger does not list");
    assert_eq!(fs::read_to_string(home.join("config.toml")).unwrap(), stray);
}
