//! `fleet watch --json`: one NDJSON event per changed Task, resumable from
//! the store's write counter with `--since`. The real `fleet` binary against
//! its file store; nothing is dispatched, so no runtime is involved.

mod support;

use std::io::{BufRead, BufReader};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use serde_json::{Value, json};
use support::{Env, task};

fn events(stdout: &str) -> Vec<Value> {
    stdout
        .lines()
        .map(|l| serde_json::from_str(l).unwrap_or_else(|e| panic!("not NDJSON ({e}): {l}")))
        .collect()
}

fn revision(event: &Value) -> u64 {
    event["revision"].as_u64().expect("revision")
}

#[test]
fn watch_reports_each_changed_task_and_resumes_from_a_revision() {
    let env = Env::new("watch");
    env.apply(json!([task("t1", false), task("t2", false)]))
        .ok();

    // The catch-up: every Task, oldest write first, then the names present.
    let first = events(&env.fleet(&["watch", "--json", "--once"]).ok().stdout);
    assert_eq!(first.len(), 3, "{first:#?}");
    assert_eq!(first[0]["type"], "changed");
    assert_eq!(first[0]["object"]["metadata"]["name"], "t1");
    assert_eq!(first[1]["object"]["metadata"]["name"], "t2");
    assert!(revision(&first[0]) < revision(&first[1]));
    assert_eq!(first[2]["type"], "synced");
    assert_eq!(first[2]["names"], json!(["t1", "t2"]));
    let synced = revision(&first[2]);
    assert_eq!(synced, revision(&first[1]));

    // Resumed from there: only what was written since.
    env.fleet(&["send", "t1", "hello"]).ok();
    let next = events(
        &env.fleet(&["watch", "--json", "--once", "--since", &synced.to_string()])
            .ok()
            .stdout,
    );
    assert_eq!(next.len(), 2, "{next:#?}");
    assert_eq!(next[0]["object"]["metadata"]["name"], "t1");
    assert!(revision(&next[0]) > synced);
    assert_eq!(next[1]["type"], "synced");

    // Running: a deletion arrives as its own event while the watch is open.
    let since = revision(&next[1]).to_string();
    let mut child = env.spawn(&["watch", "--json", "--since", &since, "--poll-ms", "100"]);
    let stdout = child.stdout.take().expect("stdout");
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if tx.send(line).is_err() {
                break;
            }
        }
    });
    let next_event = || -> Value {
        let line = rx
            .recv_timeout(Duration::from_secs(30))
            .expect("an event within the bound");
        serde_json::from_str(&line).expect("NDJSON")
    };
    assert_eq!(next_event()["type"], "synced");
    // A deletion marks the Task; its controller releases it, then it goes.
    env.fleet(&["delete", "task", "t2"]).ok();
    env.reconcile().ok();
    let deleted = loop {
        let event = next_event();
        if event["type"] == "deleted" {
            break event;
        }
        assert_eq!(event["type"], "changed", "{event:#}");
    };
    let _ = child.kill();
    let _ = child.wait();
    assert_eq!(deleted["name"], "t2", "{deleted:#}");
    assert!(deleted["uid"].is_string(), "{deleted:#}");
}

/// A store that cannot be read is a warning while the watch runs, and the
/// read that works again is reported as `synced`, so a view knows its rows
/// are current once more.
#[test]
fn a_read_that_works_again_is_synced_again() {
    let env = Env::new("watch-unreadable");
    env.apply(json!([task("t1", false)])).ok();
    let mut child = env.spawn(&["watch", "--json", "--poll-ms", "100"]);
    let stdout = child.stdout.take().expect("stdout");
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if tx.send(line).is_err() {
                break;
            }
        }
    });
    let next_event = || -> Value {
        let line = rx
            .recv_timeout(Duration::from_secs(30))
            .expect("an event within the bound");
        serde_json::from_str(&line).expect("NDJSON")
    };
    while next_event()["type"] != "synced" {}

    // A file the store does not know makes the Tasks unreadable.
    let junk = env.data.join("objects").join("Task").join("junk.txt");
    std::fs::write(&junk, "?").expect("junk");
    thread::sleep(Duration::from_millis(500));
    std::fs::remove_file(&junk).expect("junk");
    let again = next_event();
    let _ = child.kill();
    let _ = child.wait();
    assert_eq!(again["type"], "synced", "{again:#}");
    assert_eq!(again["names"], json!(["t1"]), "{again:#}");
}
