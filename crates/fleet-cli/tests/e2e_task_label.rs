//! `fleet label` end to end: a Task's labels set through the real binary,
//! its spec and generation untouched, and the labels only Fleet's own
//! controllers write left alone and refused.

mod support;

use fleet_api::conversation::{CONVERSATION_LABEL, CONVERSATION_UID_LABEL};
use fleet_api::store::{FieldManager, Store};
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
        "gateway.fleet.dev/parent",
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
    env.fleet(&["label", "task", "t2", "thurbox.dev/name=thread"])
        .ok();
    let labels = &env.get("t2")["metadata"]["labels"];
    assert_eq!(labels["thurbox.dev/name"], "thread");
    assert_eq!(labels[CONVERSATION_LABEL], "conv-0123456789ab");
    assert_eq!(labels[CONVERSATION_UID_LABEL], "u-1");
}
