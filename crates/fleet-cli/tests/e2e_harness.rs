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
        json!(["vendor", "claude", "claude-code", "codex"])
    );
}
