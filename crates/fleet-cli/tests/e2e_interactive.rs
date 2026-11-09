//! An interactive Task: a Task with no brief spawns its session and waits
//! for its user. Nothing is briefed, a message still reaches the session,
//! and an amendment is refused. The real `fleet` binary on the fake host.

mod support;

use serde_json::json;
use support::{Env, assert_condition, task};

fn briefs(env: &Env, name: &str) -> Vec<String> {
    env.host()
        .session(&env.session_key(name))
        .unwrap()
        .expect("a session")
        .briefs
}

#[test]
fn a_task_with_no_brief_spawns_and_waits_for_its_user() {
    let env = Env::new("interactive");
    let mut t = task("t1", true);
    t["spec"]["brief"] = json!("");
    t["spec"]["outputs"] = json!({});
    env.apply(json!([t])).ok();
    env.reconcile().ok();

    let st = env.status("t1");
    assert_eq!(st["phase"], "Spawned", "{st:#}");
    assert_condition(&st, "Briefed", "False", "Interactive");
    assert_eq!(env.calls("brief"), 0, "nothing is typed into the session");
    assert!(briefs(&env, "t1").is_empty());

    // A message is delivered as to any session.
    let entry = env.fleet(&["send", "t1", "hello", "--json"]).ok().json();
    env.reconcile().ok();
    assert_eq!(
        briefs(&env, "t1"),
        vec![entry["key"].as_str().unwrap().to_string()]
    );

    // There is no brief to add to.
    env.fleet(&["amend", "t1", "--text", "more"])
        .code(1)
        .stderr_has("interactive");
}

#[test]
fn a_blank_brief_is_refused() {
    let env = Env::new("interactive-blank");
    let mut t = task("t1", true);
    t["spec"]["brief"] = json!("   ");
    env.apply(json!([t])).code(1).stderr_has("spec.brief");
}

/// `--create-only` makes a new Task or nothing: a name already taken is
/// refused, and the Task there is left as it was, however close together
/// two clients apply it.
#[test]
fn a_create_only_apply_never_changes_a_task_already_there() {
    let env = Env::new("create-only");
    let mut first = task("t1", false);
    first["metadata"]["labels"] = json!({"owner": "first"});
    env.fleet_stdin(&["apply", "--create-only", "-f", "-"], &first.to_string())
        .ok();
    let mut second = task("t1", false);
    second["metadata"]["labels"] = json!({"owner": "second"});
    env.fleet_stdin(&["apply", "--create-only", "-f", "-"], &second.to_string())
        .code(5)
        .stderr_has("t1");
    assert_eq!(env.get("t1")["metadata"]["labels"]["owner"], "first");
}
