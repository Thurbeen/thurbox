//! Acceptance: the runtime makes a Task's workspace, a list of URLs,
//! through its `workspace` call before the first start; Fleet hands the
//! spawn the ready `{host, dirs}`, and has the runtime release the workspace
//! after the session. Every scenario runs the real `fleet` binary against
//! the file store and the fake host, which keeps its workspaces as records.

mod support;

use std::fs;

use fleet_runtime::Fault;
use serde_json::{Value, json};
use support::{Env, assert_condition, base_config, task};

fn env(name: &str) -> Env {
    let env = Env::new(name);
    env.apply(base_config()).ok();
    env
}

fn repositories_task(name: &str) -> Value {
    let mut t = task(name, true);
    t["spec"]["workspace"] = json!([
        "git+https://example.test/app.git#main",
        "git+https://example.test/docs.git"
    ]);
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
    let dir = st["workspace"]["dirs"][0]
        .as_str()
        .expect("dir")
        .to_string();
    assert!(dir.ends_with("/app"), "{st:#}");
    assert!(
        st["workspace"]["dirs"][1]
            .as_str()
            .is_some_and(|d| d.ends_with("/docs")),
        "{st:#}"
    );

    let ws = workspace_of(&env, &key).expect("one workspace under the key");
    assert_eq!(
        ws["urls"][0], "git+https://example.test/app.git#main",
        "{ws:#}"
    );
    let session = env.host().session(&key).unwrap().expect("session");
    let placed = session
        .materialized
        .expect("the spawn got a ready workspace");
    assert_eq!(
        placed.dir(),
        Some(dir.as_str()),
        "the session runs in the workspace the runtime made"
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

/// A `file://` URL is a directory the runtime uses as it is: it is made
/// through the same call, and the session runs in it.
#[test]
fn a_file_url_is_its_directory_as_it_is() {
    let env = env("workspace-file");
    let dir = env.root.join("work").join("t1");
    let mut t = task("t1", true);
    t["spec"]["workspace"] = json!([format!("file://{}", dir.display())]);
    env.apply(t).ok();
    env.reconcile().ok();
    let st = env.status("t1");
    assert_eq!(st["phase"], "Briefed", "{st:#}");
    assert_eq!(
        st["workspace"]["dirs"],
        json!([dir.to_string_lossy()]),
        "{st:#}"
    );
    assert_eq!(env.calls("materialize"), 1);
}

#[test]
fn an_empty_workspace_is_never_made() {
    let env = env("workspace-empty");
    env.apply(task("t1", true)).ok();
    env.reconcile().ok();
    let st = env.status("t1");
    assert_eq!(st["phase"], "Briefed", "{st:#}");
    assert!(st["workspace"].is_null(), "{st:#}");
    assert_eq!(env.calls("materialize"), 0);
    let key = env.session_key("t1");
    let session = env.host().session(&key).unwrap().expect("session");
    assert_eq!(
        session.materialized, None,
        "the runtime's default directory"
    );
}

/// A URL the host's runtime does not handle is refused before any spawn,
/// spends no attempt, and waits for a runtime that handles it.
#[test]
fn a_url_the_runtime_does_not_handle_waits_and_spawns_nothing() {
    let env = env("workspace-unsupported");
    env.host()
        .update(|s| {
            s.unsupported
                .push("git+https://example.test/docs.git".into())
        })
        .unwrap();
    env.apply(repositories_task("t1")).ok();
    env.reconcile().ok();
    let st = env.status("t1");
    assert_eq!(st["phase"], "Pending", "{st:#}");
    assert_condition(&st, "WorkspaceReady", "False", "WorkspaceUnsupported");
    assert_condition(&st, "NeedsAttention", "True", "WorkspaceUnsupported");
    assert_eq!(st["materializeAttempts"].as_u64().unwrap_or(0), 0, "{st:#}");
    assert_eq!(env.calls("spawn"), 0);
}

/// A workspace is a list of absolute URLs. The shapes before it, and a list
/// of paths, are refused when they are applied.
#[test]
fn a_workspace_that_is_not_a_list_of_urls_is_refused_at_apply() {
    let env = Env::new("workspace-old-shape");
    env.apply(base_config()).ok();
    for old in [
        json!({"dir": "/srv/app"}),
        json!({"repositories": [{"name": "app", "url": "https://example.test/app"}]}),
        json!(["/srv/app"]),
    ] {
        let mut t = task("t1", true);
        t["spec"]["workspace"] = old.clone();
        env.apply(t).code(1).stderr_has("a list of absolute URLs");
        env.get_absent("t1");
    }
}

/// The `[workspaces]` section is gone: each runtime makes its own
/// workspaces, so a config that still has one is refused at load.
#[test]
fn a_workspaces_section_is_refused_at_load() {
    let env = Env::new("workspace-section");
    let mut config = fs::read_to_string(&env.config).expect("config");
    config.push_str("\n[workspaces]\nkind = \"worktree\"\n");
    fs::write(&env.config, config).expect("config");
    env.fleet(&["list", "task"])
        .code(1)
        .stderr_has("workspaces");
}
