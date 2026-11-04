mod support;

use serde_json::{Value, json};
use std::fs;
use support::Env;

fn setup(name: &str) -> Env {
    let env = Env::new(name);
    let mut cfg = fs::read_to_string(&env.config).unwrap();
    cfg.push_str(&format!("\n[plugins.test]\nprogram = '{}'\nargs = ['plugin', '{}']\ncwd = '{}'\ncallTimeoutMs = 1000\npassEnv = ['PLUGIN_SECRET']\n", env!("CARGO_BIN_EXE_fleet-test-stub"), env.root.display(), env.root.display()));
    fs::write(&env.config, cfg).unwrap();
    scenario(
        &env,
        json!({"status":{"phase":"Ready","observedGeneration":1}}),
    );
    env
}

fn scenario(env: &Env, plan: Value) {
    fs::write(env.root.join("plugin.json"), serde_json::to_vec(&json!({
        "manifest":{"protocol":1,"name":"test","kinds":[{"kind":"Issue","pollMs":1000,"finalizer":true,"spec":{"type":"object","required":["number"],"additionalProperties":false,"properties":{"number":{"type":"integer"}}}}]},
        "plan":plan
    })).unwrap()).unwrap();
}

fn issue() -> Value {
    json!({"apiVersion":"fleet/v1alpha1","kind":"Issue","metadata":{"name":"issue-1"},"spec":{"number":1}})
}

#[test]
fn registered_kind_reconciles_and_finalizes_across_process_restarts() {
    let env = setup("plugin-lifecycle");
    env.apply(issue()).ok();
    let report = env.reconcile().ok().json();
    assert_eq!(report["errors"], json!([]));
    let obj = env.fleet(&["get", "issues", "issue-1"]).ok().json();
    assert_eq!(obj["status"]["phase"], "Ready");
    assert!(obj["status"]["notBefore"].as_u64().is_some());
    assert_eq!(
        env.fleet(&["list", "plugins", "--json"]).ok().json()[0]["name"],
        "test"
    );
    env.fleet(&["delete", "issue", "issue-1"]).ok();
    scenario(&env, json!({"removeFinalizer":true}));
    env.reconcile().ok();
    env.fleet(&["get", "issue", "issue-1"]).code(3);
}

#[test]
fn schema_and_reserved_fields_are_refused() {
    let env = setup("plugin-admission");
    let mut bad = issue();
    bad["spec"]["number"] = json!("wrong");
    env.apply(bad).code(1);
    env.apply(issue()).ok();
    scenario(
        &env,
        json!({"status":{"host":{"forged":true}},"creates":[support::task("unwanted",false)]}),
    );
    env.reconcile().code(1);
    env.fleet(&["get", "task", "unwanted"]).code(3);
    let obj = env.fleet(&["get", "issue", "issue-1"]).ok().json();
    assert!(obj["status"]["host"]["error"].is_string());
}

#[test]
fn effects_are_journaled_and_never_repeated() {
    let mut env = setup("plugin-effects");
    env.set_var("PLUGIN_SECRET", "private-fixture-value");
    env.apply(issue()).ok();
    scenario(
        &env,
        json!({"effects":[{"key":"comment/1","request":{"op":"comment"}}]}),
    );
    env.reconcile().ok();
    let obj = env.fleet(&["get", "issue", "issue-1"]).ok().json();
    assert_eq!(
        obj["status"]["effects"]["comment/1"]["receipt"]["outcome"]["done"]["posted"],
        true
    );
    env.expire_wait_of("Issue", "issue-1");
    env.reconcile().ok();
    let calls: Vec<Value> = fs::read_to_string(env.root.join("plugin-calls.jsonl"))
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(calls.iter().filter(|c| c["call"] == "perform").count(), 1);
    for call in calls {
        let secret = call["envNames"]
            .as_array()
            .unwrap()
            .contains(&json!("PLUGIN_SECRET"));
        assert_eq!(secret, call["call"] == "perform");
    }
    assert!(
        !serde_json::to_string(&obj)
            .unwrap()
            .contains("private-fixture-value")
    );
}

#[test]
fn plugin_creates_tasks_and_operator_can_dispatch_without_forging_creator() {
    let env = setup("plugin-task-dispatch");
    env.apply(issue()).ok();
    scenario(
        &env,
        json!({"status":{"observedGeneration":1},"creates":[support::task("child",false)]}),
    );
    env.reconcile().ok();
    let task = env.get("child");
    assert_eq!(task["metadata"]["labels"]["fleet.dev/plugin"], "test");
    env.apply(support::task("child", true)).ok();
    assert_eq!(
        env.get("child")["metadata"]["labels"]["fleet.dev/plugin"],
        "test"
    );
}

#[test]
fn own_task_verbs_and_store_probes_work_without_status_impersonation() {
    let env = setup("plugin-task-verbs");
    env.apply(issue()).ok();
    scenario(&env, json!({"creates":[support::task("child",false)]}));
    env.reconcile().ok();
    let uid = env.get("child")["metadata"]["uid"].clone();
    scenario(
        &env,
        json!({"status":{"observedGeneration":1},"patches":[{"kind":"Task","name":"child","uid":uid,"spec":{"dispatch":true}}],"appends":[{"task":"child","uid":uid,"key":"message/1","text":"Plugin follow-up"}]}),
    );
    env.expire_wait_of("Issue", "issue-1");
    env.reconcile().ok();
    assert_eq!(env.get("child")["spec"]["dispatch"], true);
    let task = env.get("child");
    assert!(
        serde_json::to_string(&task["status"])
            .unwrap()
            .contains("plugin/test")
    );
    assert!(task["status"]["result"].is_null());
}

#[test]
fn task_ownership_and_uid_refuse_the_whole_plan() {
    let env = setup("plugin-task-rights");
    env.apply(issue()).ok();
    env.apply(support::task("foreign", false)).ok();
    let uid = env.get("foreign")["metadata"]["uid"].clone();
    scenario(
        &env,
        json!({"creates":[support::task("unwanted",false)],"patches":[{"kind":"Task","name":"foreign","uid":uid,"spec":{"dispatch":true}}]}),
    );
    env.reconcile().code(1);
    env.fleet(&["get", "task", "unwanted"]).code(3);
    assert_eq!(env.get("foreign")["spec"]["dispatch"], false);
}

#[test]
fn unregistered_objects_remain_readable_and_cannot_be_applied() {
    let env = setup("plugin-unregistered");
    env.apply(issue()).ok();
    let cfg = fs::read_to_string(&env.config).unwrap();
    fs::write(&env.config, cfg.split("[plugins.test]").next().unwrap()).unwrap();
    env.fleet(&["get", "issue", "issue-1"]).ok();
    env.apply(issue()).code(1);
}

fn edit_scenario(env: &Env, edit: impl FnOnce(&mut Value)) {
    let path = env.root.join("plugin.json");
    let mut value: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    edit(&mut value);
    fs::write(path, serde_json::to_vec(&value).unwrap()).unwrap();
}

#[test]
fn timeout_and_output_overflow_leave_unknown_receipts_and_do_not_replay() {
    for failure in ["hang", "flood"] {
        let env = setup(&format!("plugin-{failure}"));
        env.apply(issue()).ok();
        scenario(
            &env,
            json!({"status":{"observedGeneration":1},"effects":[{"key":"once","request":{}}]}),
        );
        edit_scenario(&env, |s| {
            s["failCall"] = json!("perform");
            s["failure"] = json!(failure);
        });
        env.reconcile().ok();
        let obj = env.fleet(&["get", "issue", "issue-1"]).ok().json();
        assert_eq!(
            obj["status"]["effects"]["once"]["receipt"]["outcome"]["unknown"]["why"],
            "LostAnswer"
        );
        env.expire_wait_of("Issue", "issue-1");
        env.reconcile().ok();
        let calls = fs::read_to_string(env.root.join("plugin-calls.jsonl")).unwrap();
        assert_eq!(
            calls
                .lines()
                .filter(|l| serde_json::from_str::<Value>(l).unwrap()["call"] == "perform")
                .count(),
            1
        );
    }
}

#[test]
fn observers_receive_providers_and_unknown_is_preserved_alongside_store_absence() {
    let mut env = setup("plugin-observers");
    env.set_var("PLUGIN_SECRET", "inherited-value");
    let mut cfg = fs::read_to_string(&env.config).unwrap();
    cfg.push_str("envFrom = [{name = 'observer'}]\n");
    fs::write(&env.config, cfg).unwrap();
    env.apply(json!({"apiVersion":"fleet/v1alpha1","kind":"EnvProvider","metadata":{"name":"observer"},"spec":{"data":{"PLUGIN_SECRET":"provider-value"}}})).ok();
    env.apply(issue()).ok();
    edit_scenario(&env, |s| {
        s["reflect"] = json!(true);
        s["probes"] = json!([{"id":"outside","type":"plugin","request":{}},{"id":"missing","type":"object","kind":"Task","name":"absent"},{"id":"list","type":"list","kind":"Issue","labels":{}}]);
    });
    env.reconcile().ok();
    let obj = env.fleet(&["get", "issue", "issue-1"]).ok().json();
    assert_eq!(
        obj["status"]["observations"]["outside"]["known"]["providerWon"],
        true
    );
    assert!(
        obj["status"]["observations"]["missing"]
            .get("known")
            .unwrap()
            .is_null()
    );
    assert_eq!(
        obj["status"]["observations"]["list"]["known"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    edit_scenario(&env, |s| s["observeUnknown"] = json!(true));
    env.expire_wait_of("Issue", "issue-1");
    env.reconcile().ok();
    let obj = env.fleet(&["get", "issue", "issue-1"]).ok().json();
    assert_eq!(
        obj["status"]["observations"]["outside"]["unknown"]["why"],
        "Throttled"
    );
}

#[test]
fn lost_pending_intent_is_recovered_without_performing_it() {
    let env = setup("plugin-lost-intent");
    env.apply(issue()).ok();
    env.reconcile().ok();
    let mut obj = env.fleet(&["get", "issue", "issue-1"]).ok().json();
    obj["status"]["effects"] = json!({"once":{"intent":{"at":1,"generation":1}}});
    obj["status"]["notBefore"] = json!(0);
    fs::write(
        env.data.join("objects/Issue/issue-1.json"),
        serde_json::to_vec(&obj).unwrap(),
    )
    .unwrap();
    scenario(
        &env,
        json!({"status":{"observedGeneration":1},"effects":[{"key":"once","request":{}}]}),
    );
    env.reconcile().ok();
    let obj = env.fleet(&["get", "issue", "issue-1"]).ok().json();
    assert_eq!(
        obj["status"]["effects"]["once"]["receipt"]["outcome"]["unknown"]["why"],
        "LostAnswer"
    );
    let calls = fs::read_to_string(env.root.join("plugin-calls.jsonl")).unwrap();
    assert!(
        calls
            .lines()
            .all(|l| serde_json::from_str::<Value>(l).unwrap()["call"] != "perform")
    );
}

#[test]
fn builtin_and_plural_alias_collisions_refuse_registration() {
    for kind in [
        "TASK",
        "Tasks",
        "Taskconfig",
        "Envproviders",
        "CHANGErequest",
        "Conversation",
        "Listeners",
    ] {
        let env = setup(&format!("plugin-alias-{}", kind.to_ascii_lowercase()));
        edit_scenario(&env, |s| s["manifest"]["kinds"][0]["kind"] = json!(kind));
        env.fleet(&["list", "plugins", "--json"]).code(1);
    }
    let env = setup("plugin-plural-alias");
    edit_scenario(&env, |s| {
        let mut plural = s["manifest"]["kinds"][0].clone();
        plural["kind"] = json!("Issues");
        s["manifest"]["kinds"].as_array_mut().unwrap().push(plural);
    });
    env.fleet(&["list", "plugins", "--json"]).code(1);
}

#[test]
fn successful_probes_do_not_hide_repeated_reconcile_failures() {
    for failure in ["exit", "malformed"] {
        let env = setup(&format!("plugin-availability-{failure}"));
        for n in 0..5 {
            let mut obj = issue();
            obj["metadata"]["name"] = json!(format!("issue-{n}"));
            env.apply(obj).ok();
        }
        edit_scenario(&env, |s| {
            s["failCall"] = json!("reconcile");
            s["failure"] = json!(failure);
        });
        env.reconcile().code(1).stdout_has("unavailable");
        let calls = fs::read_to_string(env.root.join("plugin-calls.jsonl")).unwrap();
        assert_eq!(
            calls
                .lines()
                .filter(|l| serde_json::from_str::<Value>(l).unwrap()["call"] == "reconcile")
                .count(),
            3
        );
    }
}

#[test]
fn self_delete_cannot_remove_the_journal_before_performing_an_effect() {
    let env = setup("plugin-self-delete");
    edit_scenario(&env, |s| {
        s["manifest"]["kinds"][0]["finalizer"] = json!(false)
    });
    env.apply(issue()).ok();
    let uid = env.fleet(&["get", "issue", "issue-1"]).ok().json()["metadata"]["uid"].clone();
    edit_scenario(
        &env,
        |s| s["plan"] = json!({"deletes":[{"kind":"Issue","name":"issue-1","uid":uid}],"effects":[{"key":"after-delete","request":{}}]}),
    );
    env.reconcile().code(1);
    env.fleet(&["get", "issue", "issue-1"]).ok();
    let calls = fs::read_to_string(env.root.join("plugin-calls.jsonl")).unwrap();
    assert!(
        calls
            .lines()
            .all(|l| serde_json::from_str::<Value>(l).unwrap()["call"] != "perform")
    );
}

#[test]
fn failed_plugin_manifest_does_not_block_builtin_reads_or_worker_verbs() {
    let env = setup("plugin-builtin-recovery");
    env.apply(support::task("worker", true)).ok();
    env.reconcile().ok();
    let generation = env.get("worker")["metadata"]["generation"]
        .as_u64()
        .unwrap()
        .to_string();
    edit_scenario(&env, |s| {
        s["failCall"] = json!("manifest");
        s["failure"] = json!("exit");
    });
    env.fleet(&["get", "task", "worker"]).ok();
    env.fleet(&["send", "worker", "Inspect the plugin failure"])
        .ok();
    env.fleet(&[
        "result",
        "worker",
        "--generation",
        &generation,
        "--outcome",
        "shipped",
        "--summary",
        "done",
        "--output",
        "prUrl=\"https://forge.test/example/project/pull/1\"",
    ])
    .ok();
    env.fleet(&["delete", "task", "worker"]).ok();
}

#[test]
fn declarations_reuse_existing_mirrors_without_changing_spec_or_labels() {
    let env = setup("plugin-declare-existing");
    let mut cfg = fs::read_to_string(&env.config).unwrap();
    cfg.push_str("\n[forge.hosts]\n\"forge.test\" = {kind = 'fake', dir = 'forge'}\n");
    fs::write(&env.config, cfg).unwrap();
    env.apply(issue()).ok();
    scenario(&env, json!({"creates":[support::task("child",true)]}));
    env.reconcile().ok();
    let child = env.get("child");
    assert!(child["status"]["brief"].is_object());
    let url = "https://forge.test/example/project/pull/1";
    let mirror_name = support::derived_name("fake", "forge.test", "example/project", "1");
    let mirror = json!({"apiVersion":"fleet/v1alpha1","kind":"ChangeRequest","metadata":{"name":mirror_name,"labels":{"keep":"operator"}},"spec":{"url":url,"pollMs":12345}});
    env.apply(mirror.clone()).ok();
    scenario(
        &env,
        json!({"declares":[{"task":"child","uid":child["metadata"]["uid"],"changeRequest":url}]}),
    );
    env.expire_wait_of("Issue", "issue-1");
    env.reconcile().ok();
    assert_eq!(
        env.get("child")["status"]["declared"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    let after = env
        .fleet(&["get", "changerequest", &mirror_name])
        .ok()
        .json();
    assert_eq!(after["spec"], mirror["spec"]);
    assert_eq!(after["metadata"]["labels"], mirror["metadata"]["labels"]);
}

#[test]
fn unregistered_plugin_objects_are_reported_by_the_manager() {
    let env = setup("plugin-orphan-report");
    env.apply(issue()).ok();
    let cfg = fs::read_to_string(&env.config).unwrap();
    fs::write(&env.config, cfg.split("[plugins.test]").next().unwrap()).unwrap();
    env.reconcile()
        .code(1)
        .stdout_has("Issue")
        .stdout_has("unregistered");
}

#[test]
fn a_plugin_amends_its_own_task_once_per_generation_and_reopens_it() {
    let env = setup("plugin-amend");
    env.apply(issue()).ok();
    scenario(&env, json!({"creates":[support::task("child",true)]}));
    env.reconcile().ok();
    let uid = env.get("child")["metadata"]["uid"].clone();
    env.fleet(&[
        "result",
        "child",
        "--generation",
        "1",
        "--outcome",
        "shipped",
        "--output",
        "prUrl=\"https://forge.test/example/project/pull/1\"",
    ])
    .ok();
    env.reconcile().ok();
    assert_eq!(env.status("child")["phase"], "Concluded");

    let amend = json!({"amends":[{"task":"child","uid":uid,"text":"A new push: review it again.","reason":"head moved","outputs":{"verdict":{"type":"string"}},"ifGeneration":1}]});
    scenario(&env, amend.clone());
    env.expire_wait_of("Issue", "issue-1");
    env.reconcile().ok();
    env.reconcile().ok();
    let child = env.get("child");
    assert_eq!(child["metadata"]["generation"], 2);
    let amendments = child["spec"]["amendments"].as_array().unwrap();
    assert_eq!(amendments.len(), 1);
    assert_eq!(amendments[0]["from"], "plugin/test");
    assert_eq!(amendments[0]["reason"], "head moved");
    assert_eq!(amendments[0]["generation"], 2);
    assert_eq!(amendments[0]["outputs"]["verdict"]["type"], "string");
    let st = env.status("child");
    assert_eq!(st["phase"], "Briefed", "{st:#}");
    support::assert_condition(&st, "Concluded", "False", "Amended");
    assert_eq!(child["status"]["brief"]["generation"], 2);

    // The same plan again, as a repeated or recovered pass: a refused verb,
    // recorded on the plugin object, and nothing appended.
    scenario(&env, amend);
    env.expire_wait_of("Issue", "issue-1");
    env.reconcile().ok();
    let child = env.get("child");
    assert_eq!(child["metadata"]["generation"], 2);
    assert_eq!(child["spec"]["amendments"].as_array().unwrap().len(), 1);
    let verbs =
        env.fleet(&["get", "issue", "issue-1"]).ok().json()["status"]["host"]["verbs"].clone();
    assert!(
        verbs[0]["error"]
            .as_str()
            .unwrap()
            .contains("generation 2, not 1"),
        "{verbs:#}"
    );

    env.fleet(&[
        "result",
        "child",
        "--generation",
        "2",
        "--outcome",
        "shipped",
        "--output",
        "prUrl=\"https://forge.test/example/project/pull/1\"",
        "--output",
        "verdict=\"approve\"",
    ])
    .ok();
    env.reconcile().ok();
    assert_eq!(env.status("child")["phase"], "Concluded");
}

#[test]
fn a_plugin_amend_of_a_foreign_task_refuses_the_whole_plan() {
    let env = setup("plugin-amend-foreign");
    env.apply(issue()).ok();
    env.apply(support::task("foreign", false)).ok();
    let uid = env.get("foreign")["metadata"]["uid"].clone();
    scenario(
        &env,
        json!({"amends":[{"task":"foreign","uid":uid,"text":"Not yours.","ifGeneration":1}]}),
    );
    env.reconcile().code(1);
    let foreign = env.get("foreign");
    assert_eq!(foreign["metadata"]["generation"], 1);
    assert!(foreign["spec"].get("amendments").is_none_or(Value::is_null));
    let host = &env.fleet(&["get", "issue", "issue-1"]).ok().json()["status"]["host"];
    assert!(
        host["error"].as_str().unwrap().contains("foreign Task"),
        "{host:#}"
    );
}
