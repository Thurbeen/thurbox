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
