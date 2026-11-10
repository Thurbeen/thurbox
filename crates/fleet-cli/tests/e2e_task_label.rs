//! `fleet label` end to end: a Task's labels set through the real binary,
//! its spec and generation untouched, and the labels only Fleet's own
//! controllers write left alone and refused.

mod support;

use fleet_api::conversation::{CONVERSATION_LABEL, CONVERSATION_UID_LABEL};
use fleet_api::plugin::CREATOR;
use fleet_api::store::{FieldManager, Store};
use fleet_api::talk::{PARENT_LABEL, PARENT_UID_LABEL};
use fleet_store::FileStore;
use serde_json::json;
use support::{Env, base_config, task};

fn with_task(name: &str) -> Env {
    let env = Env::new(name);
    env.apply(base_config()).ok();
    env.apply(task("t1", true)).ok();
    env
}

#[test]
fn a_label_is_set_and_nothing_else_of_the_task_changes() {
    let env = with_task("label-set");
    let before = env.get("t1");
    env.fleet(&["label", "task", "t1", "thurbox.dev/name=Review main"])
        .ok();
    let after = env.get("t1");
    assert_eq!(
        after["metadata"]["labels"]["thurbox.dev/name"],
        "Review main"
    );
    assert_eq!(after["spec"], before["spec"]);
    assert_eq!(
        after["metadata"]["generation"],
        before["metadata"]["generation"]
    );
    for (key, value) in before["metadata"]["labels"]
        .as_object()
        .into_iter()
        .flatten()
    {
        assert_eq!(&after["metadata"]["labels"][key], value, "{key} kept");
    }
}

#[test]
fn a_label_only_fleet_writes_is_refused() {
    let env = with_task("label-refused");
    for key in [
        CONVERSATION_LABEL,
        CONVERSATION_UID_LABEL,
        PARENT_LABEL,
        CREATOR,
    ] {
        env.fleet(&["label", "task", "t1", &format!("{key}=x")])
            .code(1)
            .stderr_has(key);
    }
    env.fleet(&["label", "task", "t1", "no-equals-sign"])
        .code(2);
    env.fleet(&["label", "task", "nobody", "a=b"]).code(3);
}

/// A Task a Conversation made carries the labels `apply` refuses; a label
/// set on it keeps them.
#[test]
fn a_task_a_conversation_made_keeps_its_labels() {
    let env = with_task("label-conversation");
    let store = FileStore::open(&env.data).unwrap();
    let mut made = task("t2", true);
    made["metadata"]["labels"] = json!({
        CONVERSATION_LABEL: "conv-0123456789ab",
        CONVERSATION_UID_LABEL: "u-1",
    });
    // As the Conversation's controller makes one: under the manager lease.
    let lease = store.acquire_lease("test", 60_000).unwrap();
    store
        .create_fenced(
            serde_json::from_value(made).unwrap(),
            FieldManager::ConversationController,
            &lease,
        )
        .unwrap();
    store.release_lease(&lease).unwrap();
    // And as a plugin's child of another Task: the labels only a plugin's
    // host and a parent's creation write, put where the store keeps them.
    let file = env.data.join("objects/Task/t2.json");
    let mut stored: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
    let held = stored["metadata"]["labels"].as_object_mut().unwrap();
    held.insert(CREATOR.into(), json!("planner"));
    held.insert(PARENT_LABEL.into(), json!("t1"));
    held.insert(
        PARENT_UID_LABEL.into(),
        json!(env.get("t1")["metadata"]["uid"]),
    );
    std::fs::write(&file, stored.to_string()).unwrap();
    let before = env.get("t2")["metadata"]["labels"].clone();

    env.fleet(&["label", "task", "t2", "thurbox.dev/name=thread"])
        .ok();
    let labels = &env.get("t2")["metadata"]["labels"];
    assert_eq!(labels["thurbox.dev/name"], "thread");
    for key in [
        CONVERSATION_LABEL,
        CONVERSATION_UID_LABEL,
        CREATOR,
        PARENT_LABEL,
        PARENT_UID_LABEL,
    ] {
        assert_eq!(labels[key], before[key], "{key} kept");
    }
}
