//! Acceptance: Fleet materializes a Task's workspace through the workspace
//! port before the spawn, hands the runtime the ready `{host, dir}`, and
//! cleans the workspace after the release. Every scenario runs the real
//! `fleet` binary against the file store and the fake host, whose workspace
//! adapter keeps its workspaces as records.

mod support;

use std::fs;

use fleet_runtime::Fault;
use serde_json::{Value, json};
use support::{Env, assert_condition, base_config, task};

/// An environment whose `[workspaces]` section names the fake adapter on
/// the fake host's own directory.
fn env(name: &str) -> Env {
    let env = Env::new(name);
    let mut config = fs::read_to_string(&env.config).expect("config");
    config.push_str("\n[workspaces]\nkind = \"fake\"\ndir = \"fake\"\n");
    fs::write(&env.config, config).expect("config");
    env.apply(base_config()).ok();
    env
}

fn repositories_task(name: &str) -> Value {
    let mut t = task(name, true);
    t["spec"]["workspace"] = json!({"repositories": [
        {"name": "app", "url": "https://example.test/app", "ref": "main"},
        {"name": "docs", "url": "https://example.test/docs"}
    ]});
    t
}

fn workspace_of(env: &Env, key: &str) -> Option<Value> {
    let state = env.host().state().expect("fake state");
    state
        .workspaces
        .get(key)
        .map(|w| serde_json::to_value(w).expect("json"))
}

#[test]
fn fleet_materializes_the_workspace_spawns_into_it_and_cleans_it_after_release() {
    let env = env("workspace-lifecycle");
    env.apply(repositories_task("t1")).ok();
    env.reconcile().ok();

    let key = env.session_key("t1");
    let st = env.status("t1");
    assert_eq!(st["phase"], "Briefed", "{st:#}");
    assert_condition(&st, "WorkspaceReady", "True", "Materialized");
    assert_eq!(st["workspace"]["key"], key.as_str(), "{st:#}");
    assert_eq!(st["workspace"]["host"], "local", "{st:#}");
    let dir = st["workspace"]["dir"].as_str().expect("dir").to_string();

    let ws = workspace_of(&env, &key).expect("one workspace under the key");
    assert_eq!(ws["repositories"][0]["name"], "app", "{ws:#}");
    let session = env.host().session(&key).unwrap().expect("session");
    let placed = session
        .materialized
        .expect("the spawn got a ready workspace");
    assert_eq!(
        placed.dir, dir,
        "the session runs in the workspace Fleet made"
    );
    assert_eq!(env.calls("materialize"), 1);

    // A second run changes nothing and makes nothing twice.
    env.reconcile().ok();
    assert_eq!((env.calls("materialize"), env.calls("spawn")), (1, 1));

    // Deleting releases the session, then cleans the workspace, then lets go.
    env.host().set_at_rest(&key, true).unwrap();
    env.fleet(&["delete", "task", "t1"]).ok();
    env.reconcile().ok();
    assert_eq!(env.calls("release"), 1);
    assert_eq!(env.calls("cleanWorkspace"), 1);
    let ws = workspace_of(&env, &key).expect("the record stays, cleaned");
    assert_eq!(ws["cleaned"], true, "{ws:#}");
    env.get_absent("t1");
}

#[test]
fn a_crash_mid_materialization_is_recovered_by_observing_the_key() {
    let env = env("workspace-crash");
    env.apply(repositories_task("t1")).ok();
    env.fault(Fault::new("materialize", "abort"));
    env.fleet(&["reconcile", "--lease-ttl-ms", "3000"])
        .crashed();
    // The dead holder's lease must expire before the next manager can take it.
    std::thread::sleep(std::time::Duration::from_millis(3_100));
    assert_eq!(env.calls("materialize"), 1);

    // The intent has no receipt: the next run finds the workspace under the
    // key and adopts it, and never materializes it again.
    env.reconcile().ok();
    let st = env.status("t1");
    assert_eq!(st["phase"], "Briefed", "{st:#}");
    assert_condition(&st, "WorkspaceReady", "True", "Adopted");
    assert_eq!(env.calls("materialize"), 1, "never materialized twice");
}

#[test]
fn an_unreadable_workspace_key_concludes_nothing() {
    let env = env("workspace-unreadable");
    env.apply(repositories_task("t1")).ok();
    env.fault(Fault::new("materialize", "unknown"));
    env.fault(Fault::new("findWorkspace", "unknown").times(100));
    env.reconcile().ok();
    let st = env.status("t1");
    assert_eq!(st["phase"], "Pending", "{st:#}");
    assert_condition(&st, "WorkspaceReady", "Unknown", "WorkspaceUnreadable");
    assert_eq!((env.calls("materialize"), env.calls("spawn")), (1, 0));
}

#[test]
fn a_lost_request_is_tried_again_under_the_same_workspace_key() {
    let env = env("workspace-lost");
    env.apply(repositories_task("t1")).ok();
    env.fault(Fault::new("materialize", "unknown"));
    env.reconcile().ok();

    // The key reads absent: the request never landed, so it is tried again
    // after the wait, and the Task goes on.
    let st = env.status("t1");
    assert_eq!(st["phase"], "Pending", "{st:#}");
    assert_condition(&st, "WorkspaceReady", "False", "MaterializeRetry");
    env.expire_wait("t1");
    env.reconcile().ok();
    let st = env.status("t1");
    assert_eq!(st["phase"], "Briefed", "{st:#}");
    assert_eq!(env.calls("materialize"), 2);
    let key = env.session_key("t1");
    let state = env.host().state().unwrap();
    assert_eq!(
        state.workspaces.keys().collect::<Vec<_>>(),
        vec![&key],
        "one workspace, under the Task's key"
    );
}

#[test]
fn a_refused_workspace_is_retried_then_stalls_and_spawns_nothing() {
    let env = env("workspace-refused");
    env.apply(repositories_task("t1")).ok();
    env.fault(Fault::new("materialize", "fail").times(3));
    env.reconcile().ok();
    for _ in 0..2 {
        env.expire_wait("t1");
        env.reconcile().ok();
    }
    let st = env.status("t1");
    assert_eq!(st["phase"], "Stalled", "{st:#}");
    assert_condition(&st, "WorkspaceReady", "False", "MaterializeFailed");
    assert_eq!((env.calls("materialize"), env.calls("spawn")), (3, 0));
}

#[test]
fn without_a_workspaces_section_the_runtime_still_materializes() {
    let env = Env::new("workspace-legacy");
    env.apply(base_config()).ok();
    env.apply(repositories_task("t1")).ok();
    env.reconcile().ok();
    let st = env.status("t1");
    assert_eq!(st["phase"], "Briefed", "{st:#}");
    assert!(st["workspace"].is_null(), "{st:#}");
    assert_eq!(env.calls("materialize"), 0);
    let key = env.session_key("t1");
    let session = env.host().session(&key).unwrap().expect("session");
    assert!(
        session.materialized.is_none(),
        "the runtime got the named workspace"
    );
}

#[test]
fn a_dir_workspace_is_never_materialized() {
    let env = env("workspace-dir");
    env.apply(task("t1", true)).ok();
    env.reconcile().ok();
    let st = env.status("t1");
    assert_eq!(st["phase"], "Briefed", "{st:#}");
    assert!(st["workspace"].is_null(), "{st:#}");
    assert_eq!(env.calls("materialize"), 0);
}
