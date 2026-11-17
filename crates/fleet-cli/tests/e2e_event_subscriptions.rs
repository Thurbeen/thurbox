//! Board subscriptions end to end: `fleet event subscribe` writes a Task's
//! `spec.subscriptions`, and on each pass TaskController reads each
//! subscribed Board after the subscription's cursor in `status.subscriptions`
//! and queues one `event-notice` digest in the Task's inbox, through the real
//! `fleet` binary on a throwaway store and the fake host.

mod support;

use serde_json::{Value, json};
use support::{Env, task};

fn subscribed_env(name: &str) -> Env {
    let mut env = Env::new(name);
    env.set_var("USER", "tester");
    env.fleet(&["init"]).ok();
    env.apply(task("a", true)).ok();
    env.reconcile().ok();
    assert_eq!(env.status("a")["phase"], "Briefed");
    env
}

fn publish(env: &Env, args: &[&str]) {
    let mut all = vec!["event", "publish"];
    all.extend_from_slice(args);
    env.fleet(&all).ok();
}

fn publish_type(env: &Env, event_type: &str) {
    publish(env, &["--type", event_type, "--data", "{}"]);
}

fn status(env: &Env) -> Value {
    env.get("a")["status"].clone()
}

fn notices(status: &Value) -> Vec<Value> {
    status["inbox"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter(|e| e.get("notice").is_some())
                .cloned()
                .collect()
        })
        .unwrap_or_default()
}

fn texts(env: &Env) -> Vec<String> {
    env.host()
        .session(&env.session_key("a"))
        .unwrap()
        .expect("a session")
        .texts
}

fn delivered_notices(env: &Env) -> Vec<String> {
    texts(env)
        .into_iter()
        .filter(|t| t.contains("fleet event list"))
        .collect()
}

fn set_ready(env: &Env, ready: bool) {
    env.host().set_ready(&env.session_key("a"), ready).unwrap();
}

fn cursor(status: &Value, i: usize) -> Value {
    status["subscriptions"]["cursors"][i].clone()
}

#[test]
fn a_subscribed_task_hears_of_new_matching_events_in_one_digest_notice() {
    let env = subscribed_env("sub-digest");
    env.fleet(&["event", "subscribe", "a", "--type", "git.*"])
        .ok();
    assert_eq!(
        env.get("a")["spec"]["subscriptions"],
        json!([{"board": "shared", "filter": {"type": "git.*"}}])
    );
    env.reconcile().ok();
    assert_eq!(cursor(&status(&env), 0)["after"], 0);

    publish_type(&env, "git.push");
    publish_type(&env, "ci.done");
    publish_type(&env, "git.tag");
    env.reconcile().ok();

    let delivered = delivered_notices(&env);
    assert_eq!(delivered.len(), 1, "{delivered:#?}");
    let text = &delivered[0];
    assert!(text.contains("2 new events"), "{text}");
    assert!(text.contains("Board shared"), "{text}");
    assert!(text.contains("seq 1 to 3"), "{text}");
    assert!(
        text.contains("fleet event list --board shared --after 0 --type 'git.*'"),
        "{text}"
    );
    let st = status(&env);
    assert_eq!(cursor(&st, 0)["after"], 3, "{st:#}");
    assert!(notices(&st).is_empty(), "delivered: {st:#}");
    let receipt = &st["deliveries"]["receipts"]
        .as_array()
        .unwrap()
        .last()
        .unwrap()
        .clone();
    assert_eq!(receipt["from"], "board/shared");

    let again = env.reconcile().ok().json();
    assert_eq!(again["statusWrites"], 0, "{again:#}");
    assert_eq!(delivered_notices(&env).len(), 1);

    publish_type(&env, "ci.done");
    env.reconcile().ok();
    assert_eq!(delivered_notices(&env).len(), 1, "no match, no notice");
    assert_eq!(cursor(&status(&env), 0)["after"], 4);

    publish_type(&env, "git.push");
    env.reconcile().ok();
    let delivered = delivered_notices(&env);
    assert_eq!(delivered.len(), 2, "{delivered:#?}");
    assert!(delivered[1].contains("1 new event "), "{}", delivered[1]);
    assert!(delivered[1].contains("--after 4"), "{}", delivered[1]);
}

#[test]
fn an_undelivered_notice_holds_its_subscription_so_notices_never_pile_up() {
    let env = subscribed_env("sub-coalesce");
    env.fleet(&["event", "subscribe", "a"]).ok();
    env.reconcile().ok();
    set_ready(&env, false);

    publish_type(&env, "one");
    env.reconcile().ok();
    let st = status(&env);
    let queued = notices(&st);
    assert_eq!(queued.len(), 1, "{st:#}");
    assert_eq!(
        queued[0]["notice"],
        json!({"board": "shared", "count": 1, "first": 1, "last": 1})
    );

    publish_type(&env, "two");
    publish_type(&env, "three");
    env.reconcile().ok();
    env.reconcile().ok();
    let st = status(&env);
    assert_eq!(notices(&st), queued, "still the one notice: {st:#}");
    assert_eq!(cursor(&st, 0)["after"], 1);

    set_ready(&env, true);
    env.fleet(&["reconcile", "--json", "--max-passes", "20"])
        .ok();
    let delivered = delivered_notices(&env);
    assert_eq!(delivered.len(), 2, "{delivered:#?}");
    assert!(delivered[1].contains("2 new events"), "{}", delivered[1]);
    assert!(delivered[1].contains("seq 2 to 3"), "{}", delivered[1]);
    assert_eq!(cursor(&status(&env), 0)["after"], 3);
}

#[test]
fn a_cursor_behind_the_oldest_event_gives_a_notice_that_events_expired() {
    let env = subscribed_env("sub-expired");
    env.apply(json!({
        "apiVersion": "fleet/v1alpha1",
        "kind": "Board",
        "metadata": {"name": "tiny"},
        "spec": {"retention": {"maxCount": 2}}
    }))
    .ok();
    env.fleet(&["event", "subscribe", "a", "--board", "tiny"])
        .ok();
    env.reconcile().ok();
    set_ready(&env, false);
    for _ in 0..5 {
        publish(&env, &["--board", "tiny", "--type", "tick", "--data", "1"]);
    }
    env.reconcile().ok();
    let st = status(&env);
    let queued = notices(&st);
    assert_eq!(queued.len(), 1, "{st:#}");
    assert_eq!(
        queued[0]["notice"],
        json!({"board": "tiny", "count": 2, "first": 4, "last": 5, "expiredBefore": 4})
    );
    let text = queued[0]["text"].as_str().unwrap();
    assert!(text.contains("up to seq 3 expired"), "{text}");
    assert!(text.contains("--after 3"), "{text}");
}

fn segments(env: &Env, board: &str) -> Vec<std::path::PathBuf> {
    let dir = env.data.join("boards").join(board);
    let mut found: Vec<_> = walk(&dir)
        .into_iter()
        .filter(|p| p.extension().is_some_and(|e| e == "jsonl"))
        .collect();
    found.sort();
    found
}

fn walk(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let path = entry.path();
        if path.is_dir() {
            out.extend(walk(&path));
        } else {
            out.push(path);
        }
    }
    out
}

#[test]
fn an_unreadable_board_moves_no_cursor_and_queues_no_notice() {
    let env = subscribed_env("sub-unknown");
    env.fleet(&["event", "subscribe", "a"]).ok();
    env.reconcile().ok();
    set_ready(&env, false);
    publish_type(&env, "one");
    let segment = segments(&env, "shared")
        .pop()
        .expect("a segment of the shared board");
    let good = std::fs::read(&segment).unwrap();
    std::fs::write(&segment, b"not an event\n").unwrap();

    let report = env.reconcile().ok().json();
    let st = status(&env);
    assert!(notices(&st).is_empty(), "{st:#}");
    assert_eq!(cursor(&st, 0)["after"], 0, "{st:#}");
    assert!(
        report["unknowns"]
            .as_array()
            .unwrap()
            .iter()
            .any(|u| u.as_str().unwrap().contains("subscription")),
        "{report:#}"
    );

    std::fs::write(&segment, good).unwrap();
    env.reconcile().ok();
    let st = status(&env);
    assert_eq!(notices(&st).len(), 1, "{st:#}");
    assert_eq!(cursor(&st, 0)["after"], 1);
}

#[test]
fn a_caught_up_subscription_reads_no_log() {
    let env = subscribed_env("sub-caught-up");
    publish_type(&env, "one");
    env.fleet(&["event", "subscribe", "a"]).ok();
    env.reconcile().ok();
    assert_eq!(cursor(&status(&env), 0)["after"], 1);
    let segment = segments(&env, "shared").pop().expect("a segment");
    std::fs::write(&segment, b"not an event\n").unwrap();
    let report = env.reconcile().ok().json();
    assert_eq!(report["unknowns"], json!([]), "{report:#}");
}

#[test]
fn a_subscription_starts_at_the_head_unless_it_names_a_cursor() {
    let env = subscribed_env("sub-start");
    set_ready(&env, false);
    publish_type(&env, "old");
    publish_type(&env, "old");
    env.fleet(&["event", "subscribe", "a", "--type", "old"])
        .ok();
    env.fleet(&["event", "subscribe", "a", "--type", "o*", "--after", "1"])
        .ok();
    env.reconcile().ok();
    let st = status(&env);
    assert_eq!(cursor(&st, 0)["after"], 2, "{st:#}");
    let queued = notices(&st);
    assert_eq!(queued.len(), 1, "{st:#}");
    assert_eq!(
        queued[0]["notice"],
        json!({"board": "shared", "count": 1, "first": 2, "last": 2})
    );
}

#[test]
fn subscribe_refuses_a_duplicate_and_unsubscribe_drops_the_cursor() {
    let env = subscribed_env("sub-edit");
    env.fleet(&["event", "subscribe", "a", "--type", "git.*"])
        .ok();
    env.fleet(&["event", "subscribe", "a", "--type", "git.*"])
        .code(1)
        .stderr_has("already");
    env.fleet(&["event", "subscribe", "a", "--board", "nowhere"])
        .ok();
    env.reconcile().ok();
    let st = status(&env);
    assert_eq!(
        st["subscriptions"]["cursors"].as_array().unwrap().len(),
        2,
        "{st:#}"
    );

    env.fleet(&["event", "unsubscribe", "a", "--type", "git.*"])
        .ok();
    env.fleet(&["event", "unsubscribe", "a", "--type", "git.*"])
        .code(3);
    assert_eq!(
        env.get("a")["spec"]["subscriptions"],
        json!([{"board": "nowhere"}])
    );
    env.reconcile().ok();
    let st = status(&env);
    let cursors = st["subscriptions"]["cursors"].as_array().unwrap().clone();
    assert_eq!(cursors.len(), 1, "{st:#}");
    assert_eq!(cursors[0]["board"], "nowhere");

    env.fleet(&["event", "subscribe", "missing", "--type", "a"])
        .code(3);
}

#[test]
fn a_board_made_after_the_subscription_is_read_from_its_first_event() {
    let env = subscribed_env("sub-later-board");
    env.fleet(&["event", "subscribe", "a", "--board", "later"])
        .ok();
    env.reconcile().ok();
    set_ready(&env, false);
    env.apply(json!({
        "apiVersion": "fleet/v1alpha1",
        "kind": "Board",
        "metadata": {"name": "later"},
        "spec": {}
    }))
    .ok();
    publish(&env, &["--board", "later", "--type", "a", "--data", "1"]);
    env.reconcile().ok();
    let queued = notices(&status(&env));
    assert_eq!(queued.len(), 1);
    assert_eq!(
        queued[0]["notice"],
        json!({"board": "later", "count": 1, "first": 1, "last": 1})
    );
}

#[test]
fn a_task_applied_with_subscriptions_is_notified_and_a_concluded_one_is_not() {
    let mut env = Env::new("sub-apply");
    env.set_var("USER", "tester");
    env.fleet(&["init"]).ok();
    let mut t = task("a", true);
    t["spec"]["subscriptions"] = json!([{"board": "shared", "filter": {"subject": "repo/*"}}]);
    env.apply(t).ok();
    env.reconcile().ok();
    publish(
        &env,
        &["--type", "x", "--subject", "repo/app", "--data", "1"],
    );
    env.reconcile().ok();
    assert_eq!(delivered_notices(&env).len(), 1);

    env.fleet(&[
        "result",
        "a",
        "--generation",
        "1",
        "--outcome",
        "shipped",
        "--output",
        "prUrl=\"https://example.test/pr/1\"",
    ])
    .ok();
    env.reconcile().ok();
    assert_eq!(env.status("a")["phase"], "Concluded");
    publish(
        &env,
        &["--type", "x", "--subject", "repo/app", "--data", "2"],
    );
    env.reconcile().ok();
    assert_eq!(
        delivered_notices(&env).len(),
        1,
        "a Concluded Task hears nothing"
    );

    let mut bad = task("b", false);
    bad["spec"]["subscriptions"] = json!([{"board": "shared", "filter": {"kind": "x"}}]);
    env.apply(bad).code(1);
}
