mod support;

use serde_json::json;
use support::{Env, object, task};

#[test]
fn task_config_is_passive_and_launch_defaults_are_frozen_across_restart() {
    let e = Env::thurbox("config-freeze");
    let mut cfg = std::fs::read_to_string(&e.config).unwrap();
    cfg.push_str("\n[runtime.modelEnv]\ncodex = \"CODEX_MODEL\"\n");
    std::fs::write(&e.config, cfg).unwrap();
    let config = object(
        "TaskConfig",
        "shared",
        json!({"harness":"codex", "host":"local", "model":"configured-model", "env":{"GREETING":"bonjour = 🌍"}}),
    );
    e.apply(config.clone()).ok();
    assert_eq!(
        e.fleet(&["list", "taskconfig", "--json"])
            .ok()
            .json()
            .as_array()
            .unwrap()
            .len(),
        1
    );
    let mut t = task("inherited", false);
    t["spec"].as_object_mut().unwrap().remove("harness");
    t["spec"].as_object_mut().unwrap().remove("host");
    // Below the test's HOME: a home directory is never trusted automatically.
    t["spec"]["workspace"]["dir"] = json!(e.root.join("work").to_string_lossy());
    t["spec"]["configRef"] = json!({"name":"shared"});
    e.apply(t.clone()).ok();
    e.reconcile().ok();
    assert!(e.stub_sessions().is_empty());
    let mut changed = config.clone();
    changed["spec"]["model"] = json!("dispatch-model");
    e.apply(changed.clone()).ok();
    t["spec"]["dispatch"] = json!(true);
    e.apply(t).ok();
    e.reconcile().ok();
    let snapshot = e.status("inherited")["execution"].clone();
    assert_eq!(snapshot["harness"], "codex");
    assert_eq!(snapshot["model"], "dispatch-model");
    assert_eq!(launch(&e)["env"]["GREETING"], "bonjour = 🌍");
    assert_eq!(snapshot["sources"][0]["generation"], 2);
    changed["spec"]["model"] = json!("later-model");
    e.apply(changed).ok();
    e.fleet(&["delete", "taskconfig", "shared"]).ok();
    e.apply(config).ok();
    e.reconcile().ok();
    assert_eq!(e.status("inherited")["execution"], snapshot);
    assert_eq!(e.stub_calls_of("create"), 1);
    assert_eq!(e.stub_calls_of("message send"), 1);
    let create = e
        .stub_calls()
        .into_iter()
        .find(|c| c["argv"][1] == "create")
        .unwrap();
    let args = create["argv"].as_array().unwrap();
    assert!(args.contains(&json!("--env-stdin")));
    assert!(!create.to_string().contains("bonjour"));
    assert!(
        e.stub_sessions()[0]["env"]
            .as_array()
            .unwrap()
            .contains(&json!("CODEX_MODEL"))
    );
}

#[test]
fn references_overrides_validation_and_missing_source_do_not_launch_wrong_worker() {
    let e = Env::new("config-errors");
    let mut t = task("referenced", true);
    t["spec"]["configRef"] = json!({"name":"absent"});
    t["spec"]["env"] = json!({"LANGUAGE":"task"});
    t["spec"]["model"] = json!("task-model");
    e.apply(t.clone()).ok();
    e.reconcile().code(1).stdout_has("absent");
    assert_eq!(e.calls("spawn"), 0);
    let c = object(
        "TaskConfig",
        "absent",
        json!({"harness":"codex","host":"remote","model":"default-model","env":{"LANGUAGE":"config","OTHER":"kept"}}),
    );
    e.apply(c).ok();
    e.reconcile().ok();
    let s = e.status("referenced")["execution"].clone();
    assert_eq!(s["host"], "local");
    assert_eq!(s["harness"], "claude-code");
    assert_eq!(s["model"], "task-model");
    assert_eq!(
        e.host()
            .session(&e.session_key("referenced"))
            .unwrap()
            .unwrap()
            .model
            .as_deref(),
        Some("task-model")
    );
    assert_eq!(launch(&e)["env"], json!({"LANGUAGE":"task","OTHER":"kept"}));
    let mut invalid = task("invalid", true);
    invalid["spec"]["env"] = json!({"BAD=KEY":"value-never-in-error"});
    let r = e.apply(invalid);
    r.code(1);
    assert!(!r.stderr.contains("value-never-in-error"));
    let mut invalid = task("invalid", true);
    invalid["spec"]["configRef"] = json!({"name":"../escape"});
    e.apply(invalid).code(1);
    for env in [
        json!({"X":"nul\u{0}value"}),
        json!({"Path":"a", "PATH":"b"}),
    ] {
        e.apply(object("TaskConfig", "invalid", json!({"env":env})))
            .code(1);
    }
    let mut no_harness = task("invalid", true);
    no_harness["spec"]
        .as_object_mut()
        .unwrap()
        .remove("harness");
    e.apply(no_harness).code(1);
    e.apply(object("TaskConfig", "invalid", json!({"harness":"--flag"})))
        .code(1);
    e.fleet(&["status", "taskconfig", "absent", "--json"]).ok();
    e.fleet(&["delete", "taskconfig", "absent"])
        .ok()
        .stdout_has("deleted");
    e.fleet(&["get", "taskconfig", "absent"]).code(3);
    e.reconcile().ok();
    assert_eq!(e.calls("spawn"), 1);
}

#[test]
fn providers_resolve_before_launch_and_keep_values_private_on_restart() {
    let e = Env::thurbox("provider-private");
    let secret = "private-provider-value";
    let file = e.root.join("vars.json");
    std::fs::write(&file, json!({"TOKEN":secret,"WIN_CASE":"file"}).to_string()).unwrap();
    e.apply(object(
        "EnvProvider",
        "file",
        json!({"data":{"STATIC":"stored"},"sources":[{"type":"file","path":file}]}),
    ))
    .ok();
    e.apply(object(
        "TaskConfig",
        "shared",
        json!({"harness":"codex","envFrom":[{"name":"file"}],"env":{"WIN_CASE":"config"}}),
    ))
    .ok();
    let mut t = task("private", true);
    t["spec"]["configRef"] = json!({"name":"shared"});
    t["spec"]["env"] = json!({"win_case":"task"});
    t["spec"]["workspace"]["dir"] = json!(e.root.to_string_lossy());
    e.apply(t).ok();
    e.reconcile().ok();
    assert!(!e.get("private").to_string().contains(secret));
    assert!(
        !serde_json::to_string(&e.stub_calls())
            .unwrap()
            .contains(secret)
    );
    let private_dir = e.data.join("launches");
    let snapshots: Vec<_> = std::fs::read_dir(&private_dir).unwrap().flatten().collect();
    assert_eq!(snapshots.len(), 1);
    let snapshot: serde_json::Value =
        serde_json::from_slice(&std::fs::read(snapshots[0].path()).unwrap()).unwrap();
    assert_eq!(snapshot["env"]["TOKEN"], secret);
    assert_eq!(snapshot["env"]["win_case"], "task");
    assert!(snapshot["env"].get("WIN_CASE").is_none());
    std::fs::write(&file, "invalid changed content").unwrap();
    e.fleet(&["delete", "envprovider", "file"]).ok();
    e.reconcile().ok();
    assert_eq!(e.stub_calls_of("create"), 1);
    assert_eq!(e.stub_calls_of("message send"), 1);
    assert_eq!(
        e.status("private")["execution"]["sources"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
}

fn launch(e: &Env) -> serde_json::Value {
    let path = std::fs::read_dir(e.data.join("launches"))
        .unwrap()
        .flatten()
        .find(|p| p.path().extension().is_some_and(|e| e == "json"))
        .unwrap()
        .path();
    serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
}

#[test]
fn external_errors_never_launch_or_echo_values_and_can_recover() {
    let e = Env::thurbox("provider-errors");
    let file = e.root.join("source.json");
    let provider = |source| object("EnvProvider", "external", json!({"sources":[source]}));
    let source = json!({"type":"file","path":file});
    e.apply(provider(source.clone())).ok();
    e.apply(object(
        "TaskConfig",
        "shared",
        json!({"envFrom":[{"name":"external"}]}),
    ))
    .ok();
    let mut t = task("blocked", true);
    t["spec"]["workspace"]["dir"] = json!(e.root.to_string_lossy());
    t["spec"]["configRef"] = json!({"name":"shared"});
    e.apply(t).ok();
    e.reconcile()
        .code(1)
        .stdout_has("environment file unavailable");
    for contents in [
        "private-bad-value".to_owned(),
        json!({"TOKEN":123}).to_string(),
        "x".repeat(65537),
    ] {
        std::fs::write(&file, contents).unwrap();
        let r = e.reconcile();
        r.code(1);
        assert!(!r.stdout.contains("private-bad-value"));
        assert!(!r.stderr.contains("private-bad-value"));
    }
    let command = json!({"type":"command","program":support::stub_exe(),"args":["sleep"],"cwd":e.root,"timeoutMs":30});
    e.apply(provider(command)).ok();
    e.reconcile().code(1).stdout_has("timed out");
    assert_eq!(e.stub_calls_of("create"), 0);
    std::fs::write(&file, json!({"TOKEN":"private-good-value"}).to_string()).unwrap();
    let command = json!({"type":"command","program":support::stub_exe(),"args":["env-output",file],"cwd":e.root,"timeoutMs":1000});
    e.apply(provider(command)).ok();
    e.reconcile().ok();
    assert_eq!(launch(&e)["env"]["TOKEN"], "private-good-value");
    assert!(!e.get("blocked").to_string().contains("private-good-value"));
    assert!(
        !serde_json::to_string(&e.stub_calls())
            .unwrap()
            .contains("private-good-value")
    );
    std::fs::write(&file, "invalid changed command output").unwrap();
    e.reconcile().ok();
    assert_eq!(e.stub_calls_of("create"), 1);
}

#[test]
fn failed_spawn_restarts_from_private_snapshot_and_missing_snapshot_blocks() {
    let e = Env::thurbox("private-retry");
    let file = e.root.join("vars.json");
    std::fs::write(&file, json!({"TOKEN":"first-value"}).to_string()).unwrap();
    e.apply(object(
        "EnvProvider",
        "external",
        json!({"sources":[{"type":"file","path":file}]}),
    ))
    .ok();
    e.apply(object(
        "TaskConfig",
        "shared",
        json!({"envFrom":[{"name":"external"}]}),
    ))
    .ok();
    let mut t = task("retry", true);
    t["spec"]["workspace"]["dir"] = json!(e.root.to_string_lossy());
    t["spec"]["configRef"] = json!({"name":"shared"});
    e.apply(t).ok();
    e.stub_fault("create", "fail", 1);
    e.reconcile().ok();
    std::fs::write(&file, json!({"TOKEN":"changed-value"}).to_string()).unwrap();
    e.expire_wait("retry");
    e.reconcile().ok();
    assert_eq!(launch(&e)["env"]["TOKEN"], "first-value");
    assert_eq!(e.stub_sessions().len(), 1);
    assert_eq!(e.stub_calls_of("message send"), 1);
    let launch_path = std::fs::read_dir(e.data.join("launches"))
        .unwrap()
        .flatten()
        .next()
        .unwrap()
        .path();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&launch_path)
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        assert_eq!(
            std::fs::metadata(e.data.join("launches"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
    }
    std::fs::remove_file(launch_path).unwrap();
    e.reconcile().code(1).stdout_has("private launch missing");
    assert_eq!(e.stub_sessions().len(), 1);
}

#[cfg(unix)]
#[test]
fn an_exposed_private_snapshot_is_refused_instead_of_used() {
    use std::os::unix::fs::PermissionsExt;
    let e = Env::new("exposed-launch");
    let mut t = task("private", true);
    t["spec"]["env"] = json!({"TOKEN":"test-sensitive-value"});
    e.apply(t).ok();
    e.reconcile().ok();
    let path = std::fs::read_dir(e.data.join("launches"))
        .unwrap()
        .flatten()
        .next()
        .unwrap()
        .path();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
    e.reconcile()
        .code(1)
        .stdout_has("private launch permissions");
}

#[test]
fn escaped_environment_values_survive_snapshot_restart_and_final_deletion() {
    let e = Env::new("snapshot-escaping");
    let mut t = task("escaped", true);
    t["spec"]["env"] = json!({"CONTROL":"\u{1}".repeat(50_000)});
    e.apply(t).ok();
    e.reconcile().ok();
    assert_eq!(e.status("escaped")["phase"], "Briefed");
    e.reconcile().ok();
    assert_eq!(e.calls("spawn"), 1);
    e.host()
        .set_at_rest(&e.session_key("escaped"), true)
        .unwrap();
    std::fs::write(
        e.data.join("launches").join("crashed.tmp"),
        "test-private-residue",
    )
    .unwrap();
    e.fleet(&["delete", "task", "escaped"]).ok();
    e.reconcile().ok();
    assert_eq!(
        std::fs::read_dir(e.data.join("launches")).unwrap().count(),
        0
    );
}

#[test]
fn all_references_are_validated_before_any_provider_command_runs() {
    let e = Env::thurbox("source-preflight");
    let file = e.root.join("env.json");
    let marker = e.root.join("command-called");
    std::fs::write(&file, "{}").unwrap();
    e.apply(object("EnvProvider","external",json!({"sources":[{"type":"command","program":support::stub_exe(),"args":["env-output",file,marker],"cwd":e.root,"timeoutMs":1000}]}))).ok();
    e.apply(object(
        "TaskConfig",
        "shared",
        json!({"envFrom":[{"name":"external"},{"name":"missing"}]}),
    ))
    .ok();
    let mut t = task("preflight", true);
    t["spec"]["configRef"] = json!({"name":"shared"});
    e.apply(t).ok();
    e.reconcile().code(1).stdout_has("EnvProvider/missing");
    assert!(
        !marker.exists(),
        "invalid reference must be detected before command execution"
    );
}
