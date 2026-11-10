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
