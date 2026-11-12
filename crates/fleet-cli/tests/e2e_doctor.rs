//! `fleet doctor`: one line per check, ok, info, warn or fail, with a fix,
//! and a non-zero exit only on a fail. The real `fleet` binary, a stub
//! harness profile whose integration is outdated, a PATH of the test's own
//! and a runtime program that is not there.

mod support;

use std::fs;
use std::path::PathBuf;

use serde_json::Value;
use support::{Env, stub_exe};

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
text = '{{"version":{{version}}}}'
"#
    )
}

/// An environment with the stub's integration installed at version 1, then
/// its profile raised to version 2, and only `stub-agent` on PATH.
fn setup(name: &str) -> Env {
    let mut env = Env::new(name);
    let harnesses = env.root.join("harnesses");
    fs::create_dir_all(&harnesses).expect("harnesses");
    fs::write(harnesses.join("stub.toml"), stub_profile(1)).expect("profile");
    fs::create_dir_all(env.root.join(".stub-agent")).expect("agent config dir");
    env.fleet(&["integration", "install", "stub"]).ok();
    fs::write(harnesses.join("stub.toml"), stub_profile(2)).expect("profile");
    let bin = env.root.join("bin");
    fs::create_dir_all(&bin).expect("bin");
    let exe = if cfg!(windows) { ".exe" } else { "" };
    fs::copy(stub_exe(), bin.join(format!("stub-agent{exe}"))).expect("stub agent");
    env.only_on_path(&bin);
    env
}

fn check<'a>(report: &'a Value, area: &str, name: &str) -> &'a Value {
    report["checks"]
        .as_array()
        .expect("checks")
        .iter()
        .find(|c| c["area"] == area && c["name"] == name)
        .unwrap_or_else(|| panic!("no {area} check for {name}: {report:#}"))
}

#[test]
fn an_unreachable_runtime_fails_and_the_rest_are_reported_with_their_fix() {
    let env = setup("doctor-fail");
    let missing: PathBuf = env.root.join("no-such-runtime");
    fs::write(
        &env.config,
        format!(
            "version = 1\n\n[data]\ndir = \"data\"\n\n[runtimes.mux]\nprogram = '{}'\ncallTimeoutMs = 5000\n\n[hosts.local]\nruntime = \"mux\"\n",
            missing.to_string_lossy()
        ),
    )
    .expect("config");

    let run = env.fleet(&["doctor", "--json"]);
    run.code(1);
    let report = run.json();

    let runtime = check(&report, "runtime", "local");
    assert_eq!(runtime["level"], "fail", "{report:#}");
    assert!(
        runtime["hint"].as_str().is_some_and(|h| !h.is_empty()),
        "{report:#}"
    );

    let integration = check(&report, "integration", "stub");
    assert_eq!(integration["level"], "warn", "{report:#}");
    assert_eq!(integration["detail"], "outdated", "{report:#}");
    assert_eq!(integration["hint"], "fleet integration install stub");

    assert_eq!(check(&report, "harness", "stub")["level"], "ok");
    // A bundled harness whose program is not on PATH: users install only
    // the agents they run.
    let claude = check(&report, "harness", "claude");
    assert_eq!(claude["level"], "info", "{report:#}");

    assert_eq!(report["failed"], 1, "{report:#}");
}

#[test]
fn warnings_alone_exit_zero() {
    let env = setup("doctor-warn");
    let run = env.fleet(&["doctor"]);
    run.ok()
        .stdout_has("warn")
        .stdout_has("fleet integration install stub");
    let report = env.fleet(&["doctor", "--json"]).ok().json();
    assert_eq!(check(&report, "runtime", "fake")["level"], "ok");
    assert_eq!(report["failed"], 0, "{report:#}");
}

#[test]
fn a_runtime_table_fleet_cannot_open_fails() {
    let env = setup("doctor-bad-runtime");
    for runtime in ["kind = \"fake\"\n", "kind = \"other\"\ndir = \"fake\"\n"] {
        fs::write(
            &env.config,
            format!("version = 1\n\n[data]\ndir = \"data\"\n\n[runtime]\n{runtime}"),
        )
        .expect("config");
        let run = env.fleet(&["doctor", "--json"]);
        run.code(1);
        let report = run.json();
        assert_eq!(
            check(&report, "runtime", "fake")["level"],
            "fail",
            "{report:#}"
        );
    }
}

/// Every file and directory under `dir`, a file with its bytes.
fn tree(dir: &std::path::Path) -> std::collections::BTreeMap<PathBuf, Option<Vec<u8>>> {
    let mut found = std::collections::BTreeMap::new();
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

/// The stub's integration installed with a block in the user's own config,
/// then its profile replaced by `after`: `None` removes it, `Some` is the
/// profile's new text. Returns the agent's config directory as it was
/// before the install.
fn left_behind(
    name: &str,
    after: Option<&str>,
) -> (
    Env,
    PathBuf,
    std::collections::BTreeMap<PathBuf, Option<Vec<u8>>>,
) {
    let env = Env::new(name);
    let harnesses = env.root.join("harnesses");
    fs::create_dir_all(&harnesses).expect("harnesses");
    let profile = format!(
        "{}\n[[install.blocks]]\npath = \"config.toml\"\ntext = 'event = \"Stop\"'\n",
        stub_profile(1)
    );
    fs::write(harnesses.join("stub.toml"), profile).expect("profile");
    let home = env.root.join(".stub-agent");
    fs::create_dir_all(&home).expect("agent config dir");
    fs::write(home.join("config.toml"), "# mine\n").expect("user config");
    let before = tree(&home);
    env.fleet(&["integration", "install", "stub"]).ok();
    assert!(home.join("hooks.d/fleet.json").is_file());
    match after {
        Some(text) => fs::write(harnesses.join("stub.toml"), text).expect("profile"),
        None => fs::remove_file(harnesses.join("stub.toml")).expect("profile"),
    }
    (env, home, before)
}

/// The warn row doctor gives a ledger entry no harness profile installs,
/// and its hint, which is a `fleet` command line.
fn left_behind_row(env: &Env) -> String {
    let report = env.fleet(&["doctor", "--json"]).ok().json();
    let row = check(&report, "integration", "stub");
    assert_eq!(row["level"], "warn", "{report:#}");
    assert!(
        row["detail"]
            .as_str()
            .is_some_and(|d| d.contains(".stub-agent")),
        "the detail names where the files are: {report:#}"
    );
    assert_eq!(report["failed"], 0, "{report:#}");
    env.fleet(&["doctor"]).ok().stdout_has("warn");
    row["hint"].as_str().expect("hint").to_string()
}

#[test]
fn an_integration_whose_profile_is_gone_warns_and_its_hint_removes_it() {
    let (env, home, before) = left_behind("doctor-orphan-gone", None);
    let hint = left_behind_row(&env);
    let args: Vec<&str> = hint
        .strip_prefix("fleet ")
        .unwrap_or_else(|| panic!("the hint is a fleet command: {hint}"))
        .split(' ')
        .collect();
    assert_eq!(args, ["integration", "uninstall", "stub"], "{hint}");
    env.fleet(&args).ok().stdout_has("stub uninstalled");
    assert_eq!(tree(&home), before, "the agent's tree is as before install");
    let report = env.fleet(&["doctor", "--json"]).ok().json();
    assert!(
        !report["checks"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["area"] == "integration" && c["name"] == "stub"),
        "{report:#}"
    );
}

#[test]
fn an_integration_whose_profile_lost_its_install_table_warns_and_uninstall_all_removes_it() {
    let bare = "name = \"stub\"\nprogram = \"stub-agent\"\nauthority = \"full\"\n";
    let (env, home, before) = left_behind("doctor-orphan-bare", Some(bare));
    let hint = left_behind_row(&env);
    assert!(hint.contains("fleet integration uninstall stub"), "{hint}");
    env.fleet(&["integration", "uninstall", "--all"])
        .ok()
        .stdout_has("stub uninstalled");
    assert_eq!(tree(&home), before, "the agent's tree is as before install");
}

#[test]
fn an_unreadable_ledger_fails_even_when_no_profile_installs_anything() {
    let env = Env::new("doctor-unreadable-ledger");
    let harnesses = env.root.join("harnesses");
    fs::create_dir_all(&harnesses).expect("harnesses");
    // The one bundled profile with an `[install]` table, replaced by one
    // without it: no profile row reads the ledger.
    fs::write(
        harnesses.join("kimi.toml"),
        "name = \"kimi\"\nprogram = \"kimi\"\nauthority = \"full\"\n",
    )
    .expect("profile");
    fs::create_dir_all(&env.data).expect("data");
    fs::write(env.data.join("integrations.json"), "not json").expect("ledger");
    let run = env.fleet(&["doctor", "--json"]);
    run.code(1);
    let report = run.json();
    let row = check(&report, "integration", "integrations.json");
    assert_eq!(row["level"], "fail", "{report:#}");
}
