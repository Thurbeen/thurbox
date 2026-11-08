//! Acceptance: ChangeRequests through the real `fleet` binary, the file
//! store and a forge, across process restarts and injected faults. The fake
//! forge covers the controller's numbering, cadence and retention; the stub
//! `gh` and `glab` cover the adapters' truth tables, pagination, rate limits,
//! logins and bounded processes.

mod support;

use std::fs;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use fleet_api::change_request::{ChangeRequestDefaults, NOTE_CAP};
use fleet_api::forge::{
    ChangeSnapshot, ChangeState, Checks, FailingCheck, HeadSeen, Mergeable, Note, NoteKind, Review,
    ReviewDecision, ReviewState, Threads,
};
use fleet_api::{DeleteOutcome, FieldManager, LeaseToken, Object, Observation, Store, StoreError};
use fleet_forge::{FakeForge, ForgeFault};
use fleet_manager::{Manager, RunOptions};
use fleet_runtime::{FakeHost, FakeRuntime};
use fleet_store::{FileStore, SystemClock};
use serde_json::{Value, json};
use support::{
    AZURE_DEVOPS_HOST, BITBUCKET_HOST, Env, FORGE_HOST, GITHUB_HOST, GITLAB_HOST, alive_after,
    assert_condition, change_request, derived_name, now_ms,
};

const PROJECT: &str = "acme/widgets";
const URL: &str = "https://forge.test/acme/widgets/pull/7";

fn note(id: &str, at: u64, body: &str) -> Note {
    Note {
        id: id.into(),
        kind: NoteKind::Comment,
        author: "ann".into(),
        bot: false,
        thread: None,
        resolved: None,
        path: None,
        line: None,
        at,
        edited_at: None,
        url: format!("https://forge.test/acme/widgets/pull/7#note-{id}"),
        body: body.into(),
    }
}

fn inline(id: &str, at: u64, thread: &str, resolved: bool) -> Note {
    Note {
        kind: NoteKind::Inline,
        thread: Some(thread.into()),
        resolved: Some(resolved),
        path: Some("src/lib.rs".into()),
        line: Some(12),
        ..note(id, at, "please rename this")
    }
}

fn open_change(notes: Vec<Note>) -> ChangeSnapshot {
    ChangeSnapshot {
        state: ChangeState::Open,
        head: HeadSeen {
            id: "aaa111".into(),
            checks: Checks::Pending,
            failing: Vec::new(),
        },
        review_decision: ReviewDecision::None,
        mergeable: Mergeable {
            value: None,
            reason: None,
        },
        threads: Threads {
            unresolved: notes.iter().filter(|n| n.resolved == Some(false)).count() as u64,
        },
        reviews: Vec::new(),
        notes,
    }
}

fn setup(name: &str) -> (Env, FakeForge, String) {
    let env = Env::forge(name);
    let forge = env.fake_forge();
    forge
        .set_change(
            PROJECT,
            "7",
            open_change(vec![note("n1", 10, "first"), note("n2", 20, "second")]),
        )
        .unwrap();
    let cr = derived_name("fake", FORGE_HOST, PROJECT, "7");
    (env, forge, cr)
}

fn seqs(st: &Value) -> Vec<(String, u64)> {
    st["notes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| {
            (
                n["id"].as_str().unwrap().to_string(),
                n["seq"].as_u64().unwrap(),
            )
        })
        .collect()
}

#[test]
fn a_change_is_mirrored_by_url_under_a_derived_name_and_numbered_on_first_sight() {
    let (env, _forge, cr) = setup("mirror");
    env.apply(change_request(URL, json!({"pollMs": 30000})))
        .ok()
        .stdout_has(&format!("ChangeRequest/{cr} created"));
    env.apply(change_request(URL, json!({"pollMs": 30000})))
        .ok()
        .stdout_has(&format!("ChangeRequest/{cr} unchanged"));
    let listed = env.fleet(&["list", "changerequests", "--json"]).ok().json();
    assert_eq!(listed.as_array().unwrap().len(), 1);

    let report = env.reconcile().ok().json();
    assert_eq!(report["errors"], json!([]), "{report:#}");
    let st = env.change_request_status(&cr);
    assert_eq!(st["phase"], "Watching", "{st:#}");
    assert_eq!(
        st["ref"],
        json!({"kind": "fake", "host": FORGE_HOST, "project": PROJECT, "id": "7"})
    );
    assert_eq!(st["revision"], 1);
    assert_eq!(st["everReady"], true);
    assert_eq!(st["state"], json!({"value": "open", "seq": 1}));
    assert_eq!(
        (st["head"]["id"].as_str(), st["head"]["seq"].as_u64()),
        (Some("aaa111"), Some(1))
    );
    assert_eq!(
        seqs(&st),
        vec![("n1".to_string(), 1), ("n2".to_string(), 1)]
    );
    assert_eq!(st["droppedBefore"], 0);
    assert_condition(&st, "Reachable", "True", "Observed");
    assert_condition(&st, "Mergeable", "Unknown", "Computing");
    assert!(st["notBefore"].as_u64().unwrap() > st["observedAt"].as_u64().unwrap());
    env.fleet(&["status", "changerequest", &cr])
        .ok()
        .stdout_has("revision 1 state open head aaa111");
    env.fleet(&["list", "changerequest"])
        .ok()
        .stdout_has("Watching");
    assert_eq!(env.change_request(&cr)["status"]["revision"], 1);
    env.assert_no_temp();
}

#[test]
fn a_name_cannot_alias_another_change() {
    let (env, forge, cr) = setup("alias");
    forge.set_change(PROJECT, "8", open_change(vec![])).unwrap();
    env.apply(change_request(URL, json!({}))).ok();
    // Before the first poll, the existing object's own URL is what it mirrors.
    let mut other = change_request("https://forge.test/acme/widgets/pull/8", json!({}));
    other["metadata"]["name"] = json!(cr);
    env.apply(other.clone())
        .code(1)
        .stderr_has("a change names one object");
    // After it, the recorded ref is.
    env.reconcile().ok();
    assert_eq!(env.change_request_status(&cr)["ref"]["id"], "7");
    env.apply(other)
        .code(1)
        .stderr_has("mirrors fake/forge.test/acme/widgets/7");
    // Another name for the same change is allowed, and a second mirror.
    let mut twin = change_request(URL, json!({}));
    twin["metadata"]["name"] = json!("watch-7");
    env.apply(twin)
        .ok()
        .stdout_has("ChangeRequest/watch-7 created");
    // The spec is fixed once created.
    env.apply(change_request(URL, json!({"pollMs": 5})))
        .code(1)
        .stderr_has("immutable");
    assert_eq!(env.change_request(&cr)["metadata"]["generation"], 1);
}

#[test]
fn an_unchanged_poll_writes_bookkeeping_only() {
    let (env, _forge, cr) = setup("quiet");
    env.apply(change_request(URL, json!({}))).ok();
    env.reconcile().ok();
    let before = env.change_request_status(&cr);
    env.reconcile().ok();
    assert_eq!(
        env.change_request_status(&cr),
        before,
        "not due: nothing written"
    );
    env.expire_wait_of("ChangeRequest", &cr);
    let report = env.reconcile().ok().json();
    assert_eq!(report["statusWrites"], 1, "{report:#}");
    let after = env.change_request_status(&cr);
    assert_eq!(after["revision"], 1);
    assert!(after["observedAt"].as_u64() >= before["observedAt"].as_u64());
    assert_eq!(seqs(&after), seqs(&before));
    let strip = |mut v: Value| {
        v["observedAt"] = Value::Null;
        v["notBefore"] = Value::Null;
        v
    };
    assert_eq!(strip(after), strip(before), "only the bookkeeping moved");
}

#[test]
fn edits_unresolves_and_head_transitions_take_the_revision_that_saw_them() {
    let (env, forge, cr) = setup("numbering");
    env.apply(change_request(URL, json!({}))).ok();
    env.reconcile().ok();

    // A new note and an inline thread.
    let mut change = open_change(vec![
        note("n1", 10, "first"),
        note("n2", 20, "second"),
        note("n3", 30, "third"),
        inline("i1", 40, "t1", false),
    ]);
    forge.set_change(PROJECT, "7", change.clone()).unwrap();
    env.expire_wait_of("ChangeRequest", &cr);
    env.reconcile().ok();
    let st = env.change_request_status(&cr);
    assert_eq!(st["revision"], 2);
    assert_eq!(
        seqs(&st),
        vec![
            ("n1".to_string(), 1),
            ("n2".to_string(), 1),
            ("n3".to_string(), 2),
            ("i1".to_string(), 2)
        ]
    );
    assert_eq!(st["threads"]["unresolved"], 1);

    // An edit moves a number; a resolve moves the revision but not the note.
    change.notes[0].body = "first, clarified".into();
    change.notes[0].edited_at = Some(50);
    change.notes[3].resolved = Some(true);
    change.threads.unresolved = 0;
    forge.set_change(PROJECT, "7", change.clone()).unwrap();
    env.expire_wait_of("ChangeRequest", &cr);
    env.reconcile().ok();
    let st = env.change_request_status(&cr);
    assert_eq!(st["revision"], 3);
    assert_eq!(
        seqs(&st),
        vec![
            ("n1".to_string(), 3),
            ("n2".to_string(), 1),
            ("n3".to_string(), 2),
            ("i1".to_string(), 2)
        ]
    );
    assert_eq!(st["notes"][3]["resolved"], true);

    // An unresolve moves it.
    change.notes[3].resolved = Some(false);
    change.threads.unresolved = 1;
    forge.set_change(PROJECT, "7", change.clone()).unwrap();
    env.expire_wait_of("ChangeRequest", &cr);
    env.reconcile().ok();
    let st = env.change_request_status(&cr);
    assert_eq!(
        (st["revision"].as_u64(), st["notes"][3]["seq"].as_u64()),
        (Some(4), Some(4))
    );

    // New head, failing checks, a review and the forge's verdict.
    change.head = HeadSeen {
        id: "bbb222".into(),
        checks: Checks::Failed,
        failing: vec![FailingCheck {
            name: "ci".into(),
            url: Some("https://forge.test/ci/1".into()),
        }],
    };
    change.review_decision = ReviewDecision::ChangesRequested;
    change.reviews = vec![Review {
        id: "r1".into(),
        author: "bob".into(),
        bot: false,
        state: ReviewState::ChangesRequested,
        head: "bbb222".into(),
    }];
    change.mergeable = Mergeable {
        value: Some(false),
        reason: Some("blocked".into()),
    };
    forge.set_change(PROJECT, "7", change.clone()).unwrap();
    env.expire_wait_of("ChangeRequest", &cr);
    env.reconcile().ok();
    let st = env.change_request_status(&cr);
    assert_eq!(st["revision"], 5);
    assert_eq!(st["head"]["id"], "bbb222");
    assert_eq!(st["head"]["seq"], 5);
    assert_eq!(st["head"]["failing"][0]["name"], "ci");
    assert_eq!(
        st["reviewDecision"],
        json!({"value": "changes-requested", "seq": 5})
    );
    assert_eq!(st["reviews"][0]["author"], "bob");
    assert_eq!(st["state"]["seq"], 1, "the state never moved");
    assert_condition(&st, "Mergeable", "False", "Blocked");

    // Draft, then back: everReady never returns to false.
    change.state = ChangeState::Draft;
    forge.set_change(PROJECT, "7", change).unwrap();
    env.expire_wait_of("ChangeRequest", &cr);
    env.reconcile().ok();
    let st = env.change_request_status(&cr);
    assert_eq!(st["state"], json!({"value": "draft", "seq": 6}));
    assert_eq!(st["everReady"], true);
}

#[test]
fn more_than_a_hundred_notes_rotate_and_raise_dropped_before() {
    let (env, forge, cr) = setup("rotate");
    let mut notes: Vec<Note> = (0..NOTE_CAP as u64)
        .map(|i| note(&format!("n{i:03}"), 1_000 + i, "x"))
        .collect();
    forge
        .set_change(PROJECT, "7", open_change(notes.clone()))
        .unwrap();
    env.apply(change_request(URL, json!({}))).ok();
    env.reconcile().ok();
    let st = env.change_request_status(&cr);
    assert_eq!(
        (
            st["notes"].as_array().unwrap().len(),
            st["droppedBefore"].as_u64()
        ),
        (NOTE_CAP, Some(0))
    );

    notes.push(note("late1", 5_000, "y"));
    notes.push(note("late2", 5_001, "y"));
    forge
        .set_change(PROJECT, "7", open_change(notes.clone()))
        .unwrap();
    env.expire_wait_of("ChangeRequest", &cr);
    env.reconcile().ok();
    let st = env.change_request_status(&cr);
    assert_eq!(st["revision"], 2);
    assert_eq!(st["notes"].as_array().unwrap().len(), NOTE_CAP);
    assert_eq!(st["droppedBefore"], 1, "the two oldest, numbered 1, left");
    let ids: Vec<&str> = st["notes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| n["id"].as_str().unwrap())
        .collect();
    assert!(!ids.contains(&"n000") && !ids.contains(&"n001") && ids.contains(&"late2"));

    // The same answer again is not news: the rotated notes are not renumbered.
    env.expire_wait_of("ChangeRequest", &cr);
    let report = env.reconcile().ok().json();
    assert_eq!(report["statusWrites"], 1);
    assert_eq!(env.change_request_status(&cr)["revision"], 2);
}

#[test]
fn an_unknown_answer_leaves_the_snapshot_untouched_and_retry_after_is_honoured() {
    let (env, forge, cr) = setup("unknown");
    env.apply(change_request(URL, json!({"pollMs": 7000}))).ok();
    env.reconcile().ok();
    let before = env.change_request_status(&cr);

    for (mode, reason) in [
        ("timeout", "Timeout"),
        ("lost", "Refused"),
        ("incomplete", "Incomplete"),
        ("malformed", "Unreadable"),
    ] {
        forge.add_fault(ForgeFault::new("change", mode)).unwrap();
        // The forge changed meanwhile, but an Unknown read must not show it.
        forge
            .set_change(PROJECT, "7", open_change(vec![note("n9", 90, "unseen")]))
            .unwrap();
        env.expire_wait_of("ChangeRequest", &cr);
        let t = now_ms();
        env.reconcile().ok();
        let st = env.change_request_status(&cr);
        assert_condition(&st, "Reachable", "Unknown", reason);
        assert_eq!(st["revision"], 1, "{mode}: the snapshot is untouched");
        assert_eq!(seqs(&st), seqs(&before));
        assert_eq!(
            st["phase"], "Watching",
            "{mode}: Unknown never means closed or gone"
        );
        assert_eq!(st["observedAt"], before["observedAt"]);
        let nb = st["notBefore"].as_u64().unwrap();
        assert!(
            nb >= t + 7000 && nb < t + 7000 + 60_000,
            "{mode}: waits one interval"
        );
    }

    // Throttled: the forge's own advice sets the next poll.
    forge
        .add_fault(ForgeFault::new("change", "throttle").retry_after(90_000))
        .unwrap();
    env.expire_wait_of("ChangeRequest", &cr);
    let t = now_ms();
    env.reconcile().ok();
    let st = env.change_request_status(&cr);
    assert_condition(&st, "Reachable", "Unknown", "Throttled");
    let nb = st["notBefore"].as_u64().unwrap();
    assert!(nb >= t + 90_000 && nb < t + 90_000 + 60_000, "{st:#}");

    // Back: the change is read, including what Unknown hid.
    env.expire_wait_of("ChangeRequest", &cr);
    env.reconcile().ok();
    let st = env.change_request_status(&cr);
    assert_condition(&st, "Reachable", "True", "Observed");
    assert_eq!(st["revision"], 2);
    assert_eq!(seqs(&st), vec![("n9".to_string(), 2)]);
}

#[test]
fn a_throttled_host_defers_every_change_request_on_it_together() {
    let (env, forge, cr7) = setup("deferral");
    forge.set_change(PROJECT, "8", open_change(vec![])).unwrap();
    let cr8 = derived_name("fake", FORGE_HOST, PROJECT, "8");
    env.apply(json!([
        change_request(URL, json!({})),
        change_request("https://forge.test/acme/widgets/pull/8", json!({}))
    ]))
    .ok();
    env.reconcile().ok();
    assert_eq!(forge.calls("change").unwrap(), 2);
    forge
        .add_fault(ForgeFault::new("change", "throttle").retry_after(120_000))
        .unwrap();
    env.expire_wait_of("ChangeRequest", &cr7);
    env.expire_wait_of("ChangeRequest", &cr8);
    let t = now_ms();
    let report = env.reconcile().ok().json();
    assert_eq!(
        forge.calls("change").unwrap(),
        3,
        "the second was not asked: {report:#}"
    );
    for cr in [&cr7, &cr8] {
        let st = env.change_request_status(cr);
        assert_condition(&st, "Reachable", "Unknown", "Throttled");
        assert!(
            st["notBefore"].as_u64().unwrap() >= t + 120_000,
            "{cr}: {st:#}"
        );
    }
    assert!(
        report["unknowns"]
            .to_string()
            .contains("deferred with the host"),
        "{report:#}"
    );
}

#[test]
fn a_closed_change_is_watched_slowly_and_may_reopen_or_settle() {
    let (env, forge, cr) = setup("closed");
    env.apply(change_request(
        URL,
        json!({"pollMs": 30000, "closedGraceMs": 2000}),
    ))
    .ok();
    env.reconcile().ok();
    let mut change = open_change(vec![note("n1", 10, "first")]);
    change.state = ChangeState::Closed;
    forge.set_change(PROJECT, "7", change.clone()).unwrap();
    env.expire_wait_of("ChangeRequest", &cr);
    let t = now_ms();
    env.reconcile().ok();
    let st = env.change_request_status(&cr);
    assert_eq!(st["phase"], "Closed", "{st:#}");
    assert_eq!(st["state"], json!({"value": "closed", "seq": 2}));
    assert!(st["closedAt"].as_u64().unwrap() >= t);
    assert!(
        st["notBefore"].as_u64().unwrap() >= t + 300_000,
        "ten times the interval"
    );

    change.state = ChangeState::Open;
    forge.set_change(PROJECT, "7", change.clone()).unwrap();
    env.expire_wait_of("ChangeRequest", &cr);
    env.reconcile().ok();
    let st = env.change_request_status(&cr);
    assert_eq!(st["phase"], "Watching");
    assert_eq!(st["state"], json!({"value": "open", "seq": 3}));
    assert!(st["closedAt"].is_null());

    change.state = ChangeState::Closed;
    forge.set_change(PROJECT, "7", change).unwrap();
    env.expire_wait_of("ChangeRequest", &cr);
    env.reconcile().ok();
    assert_eq!(env.change_request_status(&cr)["phase"], "Closed");
    thread::sleep(Duration::from_millis(2100));
    env.expire_wait_of("ChangeRequest", &cr);
    env.reconcile().ok();
    let st = env.change_request_status(&cr);
    assert_eq!(st["phase"], "Settled", "{st:#}");
    assert_condition(&st, "Settled", "True", "ClosedGraceExpired");
    assert!(st["settledAt"].is_u64());
}

#[test]
fn a_merged_change_settles_and_retention_deletes_it() {
    let (env, forge, cr) = setup("merged");
    env.apply(change_request(URL, json!({"settledTtlMs": 4000})))
        .ok();
    env.reconcile().ok();
    let mut change = open_change(vec![]);
    change.state = ChangeState::Merged;
    forge.set_change(PROJECT, "7", change).unwrap();
    env.expire_wait_of("ChangeRequest", &cr);
    env.reconcile().ok();
    let st = env.change_request_status(&cr);
    assert_eq!(st["phase"], "Settled", "{st:#}");
    assert_condition(&st, "Settled", "True", "Merged");
    assert_eq!(st["state"]["value"], "merged");
    let calls = forge.calls("change").unwrap();
    // Retained: no poll, no delete, until the retention passed.
    env.expire_wait_of("ChangeRequest", &cr);
    env.reconcile().ok();
    assert_eq!(
        forge.calls("change").unwrap(),
        calls,
        "a settled change is not polled"
    );
    env.fleet(&["get", "changerequest", &cr]).ok();
    thread::sleep(Duration::from_millis(4100));
    env.expire_wait_of("ChangeRequest", &cr);
    let report = env.reconcile().ok().json();
    assert_eq!(report["statusWrites"], 1, "the delete: {report:#}");
    env.fleet(&["get", "changerequest", &cr])
        .code(3)
        .stderr_has("not found");
    // Declared again, it is a new mirror with a new uid.
    forge.set_change(PROJECT, "7", open_change(vec![])).unwrap();
    env.apply(change_request(URL, json!({})))
        .ok()
        .stdout_has("created");
    let fresh = env.change_request(&cr);
    assert!(fresh["status"].is_null());
    env.assert_no_temp();
}

#[test]
fn proven_absence_settles_as_gone_and_a_person_may_delete_at_any_time() {
    let (env, forge, cr) = setup("gone");
    env.apply(change_request(URL, json!({}))).ok();
    env.reconcile().ok();
    forge.remove_change(PROJECT, "7").unwrap();
    env.expire_wait_of("ChangeRequest", &cr);
    env.reconcile().ok();
    let st = env.change_request_status(&cr);
    assert_eq!(st["phase"], "Settled");
    assert_condition(&st, "Settled", "True", "Gone");
    assert_eq!(st["revision"], 1, "the last snapshot stays");
    env.fleet(&["delete", "changerequest", &cr])
        .ok()
        .stdout_has(&format!("ChangeRequest/{cr} deleted"));
    env.fleet(&["get", "changerequest", &cr]).code(3);
    assert_eq!(
        forge.calls("change").unwrap(),
        2,
        "nothing on the forge changed"
    );
}

/// A store that, on the first list of ChangeRequests, deletes the listed
/// object and recreates it under the same name: what another writer could do
/// between the manager's read and its delete.
struct Racing<'a> {
    inner: &'a FileStore,
    name: String,
    swapped: std::cell::Cell<bool>,
}

impl Store for Racing<'_> {
    fn get(&self, kind: &str, name: &str) -> Observation<Option<Object>> {
        self.inner.get(kind, name)
    }
    fn list(&self, kind: &str) -> Observation<Vec<Object>> {
        let out = self.inner.list(kind);
        if kind == "ChangeRequest" && !self.swapped.replace(true) {
            self.inner
                .delete(kind, &self.name, FieldManager::Apply)
                .unwrap();
            let mut fresh: Object = serde_json::from_value(change_request(URL, json!({}))).unwrap();
            fresh.metadata.name = self.name.clone();
            self.inner.create(fresh, FieldManager::Apply).unwrap();
        }
        out
    }
    fn create(&self, obj: Object, by: FieldManager) -> Result<Object, StoreError> {
        self.inner.create(obj, by)
    }
    fn update_spec(
        &self,
        kind: &str,
        name: &str,
        spec: Value,
        labels: Option<std::collections::BTreeMap<String, String>>,
        expected: u64,
        by: FieldManager,
    ) -> Result<Object, StoreError> {
        self.inner
            .update_spec(kind, name, spec, labels, expected, by)
    }
    fn update_status(
        &self,
        kind: &str,
        name: &str,
        status: Value,
        expected: u64,
        by: FieldManager,
        lease: Option<&LeaseToken>,
    ) -> Result<Object, StoreError> {
        self.inner
            .update_status(kind, name, status, expected, by, lease)
    }
    fn append_inbox(
        &self,
        name: &str,
        entry: fleet_api::inbox::InboxEntry,
        expected: u64,
        by: FieldManager,
        lease: Option<&LeaseToken>,
    ) -> Result<Object, StoreError> {
        self.inner.append_inbox(name, entry, expected, by, lease)
    }
    fn record_delivery(
        &self,
        name: &str,
        status: Value,
        receipt: fleet_api::inbox::DeliveryReceipt,
        expected: u64,
        by: FieldManager,
        lease: &LeaseToken,
    ) -> Result<Object, StoreError> {
        self.inner
            .record_delivery(name, status, receipt, expected, by, lease)
    }
    fn record_relay(
        &self,
        name: &str,
        forge: Value,
        appends: Vec<fleet_api::inbox::InboxEntry>,
        expected: u64,
        by: FieldManager,
        lease: &LeaseToken,
    ) -> Result<Object, StoreError> {
        self.inner
            .record_relay(name, forge, appends, expected, by, lease)
    }
    fn delete(
        &self,
        kind: &str,
        name: &str,
        by: FieldManager,
    ) -> Result<DeleteOutcome, StoreError> {
        self.inner.delete(kind, name, by)
    }
    fn delete_fenced(
        &self,
        kind: &str,
        name: &str,
        uid: &str,
        expected: u64,
        by: FieldManager,
        lease: &LeaseToken,
    ) -> Result<DeleteOutcome, StoreError> {
        self.inner
            .delete_fenced(kind, name, uid, expected, by, lease)
    }
    fn remove_finalizer(
        &self,
        kind: &str,
        name: &str,
        finalizer: &str,
        expected: u64,
        by: FieldManager,
        lease: &LeaseToken,
    ) -> Result<DeleteOutcome, StoreError> {
        self.inner
            .remove_finalizer(kind, name, finalizer, expected, by, lease)
    }
    fn acquire_lease(&self, holder: &str, ttl: u64) -> Result<LeaseToken, StoreError> {
        self.inner.acquire_lease(holder, ttl)
    }
    fn renew_lease(&self, lease: &LeaseToken, ttl: u64) -> Result<LeaseToken, StoreError> {
        self.inner.renew_lease(lease, ttl)
    }
    fn release_lease(&self, lease: &LeaseToken) -> Result<(), StoreError> {
        self.inner.release_lease(lease)
    }
}

#[test]
fn a_stale_delete_plan_cannot_remove_a_recreated_object() {
    let (env, forge, cr) = setup("stale-delete");
    env.apply(change_request(URL, json!({"settledTtlMs": 4000})))
        .ok();
    let mut change = open_change(vec![]);
    change.state = ChangeState::Merged;
    forge.set_change(PROJECT, "7", change).unwrap();
    env.reconcile().ok();
    let settled = env.change_request(&cr);
    assert_eq!(settled["status"]["phase"], "Settled");
    thread::sleep(Duration::from_millis(4100));
    env.expire_wait_of("ChangeRequest", &cr);

    // The manager, in this process, over a store that swaps the object
    // between its read and its fenced delete.
    let store = FileStore::open(&env.data).unwrap();
    let racing = Racing {
        inner: &store,
        name: cr.clone(),
        swapped: std::cell::Cell::new(false),
    };
    let runtime = FakeRuntime::new(FakeHost::open(&env.fake).unwrap());
    let forge_port = Arc::new(forge);
    let manager = Manager {
        plugins: None,
        store: &racing,
        runtime: &runtime,
        clock: &SystemClock,
        holder: "stale-delete-test".into(),
        forge: Some(forge_port.as_ref()),
        workspaces: None,
        change_request_defaults: ChangeRequestDefaults::default(),
        forge_defaults: Default::default(),
        gateway: None,
    };
    let report = manager
        .run(RunOptions {
            max_passes: 1,
            lease_ttl: 30_000,
        })
        .unwrap();
    assert_eq!(
        report.conflicts, 1,
        "the stale plan was refused: {report:#?}"
    );
    assert_eq!(report.errors, Vec::<String>::new());
    let fresh = env.change_request(&cr);
    assert_ne!(fresh["metadata"]["uid"], settled["metadata"]["uid"]);
    assert!(
        fresh["status"].is_null(),
        "the recreated mirror is untouched"
    );
}

#[test]
fn a_change_request_needs_a_configured_forge_host_and_a_bad_policy_is_refused_at_load() {
    let env = Env::new("no-forge");
    env.apply(change_request(URL, json!({})))
        .code(1)
        .stderr_has("needs a [forge] section");
    let env = Env::forge("wrong-host");
    env.apply(change_request(
        "https://elsewhere.test/acme/widgets/pull/7",
        json!({}),
    ))
    .code(1)
    .stderr_has("not configured under [forge.hosts]");
    env.apply(change_request("https://forge.test/acme/widgets", json!({})))
        .code(1)
        .stderr_has("does not name a change");
    env.apply(change_request("not a url", json!({})))
        .code(1)
        .stderr_has("spec.url");
    env.apply(change_request(URL, json!({"relayNotes": true})))
        .code(1)
        .stderr_has("unknown field");
    let mut owned = change_request(URL, json!({}));
    owned["metadata"]["ownerReferences"] = json!([{"kind": "Task", "name": "t", "uid": "u"}]);
    env.apply(owned).code(1).stderr_has("ownerReferences");
    let mut with_status = change_request(URL, json!({}));
    with_status["status"] = json!({"phase": "Settled"});
    env.apply(with_status)
        .code(1)
        .stderr_has("written by controllers");
    // The ready policies load; a predicate outside the vocabulary does not.
    let cfg = fs::read_to_string(&env.config).unwrap();
    fs::write(
        &env.config,
        format!("{cfg}\n[forge.readyPolicies.checks]\nchecksPassed = true\n"),
    )
    .unwrap();
    env.fleet(&["list", "changerequest"]).ok();
    fs::write(
        &env.config,
        format!("{cfg}\n[forge.readyPolicies.checks]\napproved = true\n"),
    )
    .unwrap();
    env.fleet(&["list", "changerequest"])
        .code(1)
        .stderr_has("unknown field `approved`");
    fs::write(&env.config, cfg).unwrap();
    let v = Env::new("version")
        .fleet(&["version", "--json"])
        .ok()
        .json();
    assert!(
        v["kinds"]
            .as_array()
            .unwrap()
            .contains(&json!("ChangeRequest"))
    );
    assert_eq!(
        v["forges"],
        json!([
            "azure-devops",
            "bitbucket-cloud",
            "github",
            "gitlab",
            "fake"
        ])
    );
}

#[test]
fn bitbucket_cloud_pull_request_urls_use_workspace_repository_identity() {
    let env = Env::forge_stubs("bitbucket-url");
    let url = "https://bitbucket.org/Acme/Widgets/pull-requests/7/?tab=activity#comment-1";
    let cr = derived_name("bitbucket-cloud", BITBUCKET_HOST, "acme/widgets", "7");
    env.apply(change_request(url, json!({})))
        .ok()
        .stdout_has(&format!("ChangeRequest/{cr} created"));
    let object = env.change_request(&cr);
    assert_eq!(object["spec"]["url"], url);
}

#[test]
fn bitbucket_cloud_observation_is_complete_bounded_and_unknown_on_gaps() {
    let env = Env::forge_stubs("bitbucket-cloud");
    env.edit_stub(|s| {
        s["login"] = json!("dev");
        s["pageSize"] = json!(4);
        s["changes"] = json!({"acme/widgets#7": github_model()});
    });
    let url = "https://bitbucket.org/acme/widgets/pull-requests/7";
    let cr = derived_name("bitbucket-cloud", BITBUCKET_HOST, PROJECT, "7");
    env.apply(change_request(url, json!({})))
        .ok()
        .stdout_has(&format!("ChangeRequest/{cr} created"));
    let report = env.reconcile().ok().json();
    assert_eq!(report["errors"], json!([]), "{report:#}");
    let st = env.change_request_status(&cr);
    assert_eq!(
        st["ref"],
        json!({"kind": "bitbucket-cloud", "host": BITBUCKET_HOST, "project": PROJECT, "id": "7"})
    );
    assert_eq!(st["state"]["value"], "open");
    assert_eq!(st["head"]["id"], "h1");
    assert_eq!(st["head"]["checks"], "failed");
    assert_eq!(st["reviewDecision"]["value"], "changes-requested");
    assert_eq!(
        st["mergeable"],
        json!({"value": false, "reason": "git-mergeability-check"})
    );
    assert_eq!(st["threads"]["unresolved"], 1);
    assert!(st["reviews"].is_null() || st["reviews"].as_array().is_some_and(Vec::is_empty));
    assert_eq!(st["notes"].as_array().unwrap().len(), 6);

    let calls = env.forge_calls(BITBUCKET_HOST);
    assert_eq!(
        calls.len(),
        6,
        "two head reads, two comment pages, statuses and mergeability: {calls:#?}"
    );
    assert!(calls.iter().all(|call| {
        let argv = call["argv"].as_array().unwrap();
        argv.starts_with(&[json!("--include"), json!("--silent"), json!("--show-error")])
            && argv.contains(&json!("--suppress-connect-headers"))
            && argv.contains(&json!("--netrc-optional"))
            && !argv.iter().any(|arg| {
                arg.as_str().is_some_and(|s| {
                    let s = s.to_ascii_lowercase();
                    s.contains("authorization") || s.contains("token") || s.contains("password")
                })
            })
            && !call["envNames"].as_array().unwrap().iter().any(|name| {
                name.as_str()
                    .is_some_and(|s| s.to_ascii_uppercase().contains("TOKEN"))
            })
    }));
    assert!(
        calls
            .iter()
            .any(|call| call["argv"].to_string().contains("opaque=x&cursor=4"))
    );
    assert!(
        calls
            .iter()
            .all(|call| !call["argv"].to_string().contains("/pipelines"))
    );

    env.edit_stub(|s| {
        s["moveHeadOnFinal"] = json!(true);
        s["bitbucketPrReads"] = json!(0);
    });
    env.expire_wait_of("ChangeRequest", &cr);
    env.reconcile().ok();
    let st = env.change_request_status(&cr);
    assert_condition(&st, "Reachable", "Unknown", "Unreadable");
    assert_eq!(
        st["revision"], 1,
        "statuses are never attached to a moved head"
    );
    env.edit_stub(|s| {
        s["moveHeadOnFinal"] = json!(false);
        s["moveTargetOnFinal"] = json!(true);
        s["bitbucketPrReads"] = json!(0);
    });
    env.expire_wait_of("ChangeRequest", &cr);
    env.reconcile().ok();
    let st = env.change_request_status(&cr);
    assert_condition(&st, "Reachable", "Unknown", "Unreadable");
    assert_eq!(
        st["revision"], 1,
        "mergeability is never attached after the destination moves"
    );
    env.edit_stub(|s| {
        s["moveTargetOnFinal"] = json!(false);
        s["bitbucketPrReads"] = json!(0);
        s["wrongStatusHead"] = json!(true);
    });
    env.expire_wait_of("ChangeRequest", &cr);
    env.reconcile().ok();
    let st = env.change_request_status(&cr);
    assert_condition(&st, "Reachable", "Unknown", "Unreadable");
    assert_eq!(st["revision"], 1, "stale statuses never attach to the head");
    env.edit_stub(|s| s["wrongStatusHead"] = json!(false));

    env.edit_stub(|s| s["retryAfter"] = json!(45));
    env.stub_fault("curl", "rate-limit", 1);
    env.expire_wait_of("ChangeRequest", &cr);
    let before = now_ms();
    env.reconcile().ok();
    let st = env.change_request_status(&cr);
    assert_condition(&st, "Reachable", "Unknown", "Throttled");
    assert_eq!(st["revision"], 1);
    assert!(st["notBefore"].as_u64().unwrap() >= before + 45_000);

    for (mode, reason) in [
        ("auth", "NotLoggedIn"),
        ("malformed", "Unreadable"),
        ("fail", "Refused"),
    ] {
        env.stub_fault("curl", mode, 1);
        env.expire_wait_of("ChangeRequest", &cr);
        env.reconcile().ok();
        let st = env.change_request_status(&cr);
        assert_condition(&st, "Reachable", "Unknown", reason);
        assert_eq!(st["revision"], 1, "{mode}");
    }

    env.edit_stub(|s| s["missingResolution"] = json!(true));
    env.expire_wait_of("ChangeRequest", &cr);
    env.reconcile().ok();
    let st = env.change_request_status(&cr);
    assert_condition(&st, "Reachable", "Unknown", "Unreadable");
    assert_eq!(
        st["revision"], 1,
        "missing thread evidence cannot become zero"
    );
    env.edit_stub(|s| {
        s["missingResolution"] = json!(false);
        s["pageSize"] = json!(1);
    });
    env.expire_wait_of("ChangeRequest", &cr);
    env.reconcile().ok();
    let st = env.change_request_status(&cr);
    assert_condition(&st, "Reachable", "Unknown", "Incomplete");
    assert_eq!(
        st["revision"], 1,
        "a truncated observation is never numbered"
    );
}

#[test]
fn bitbucket_cloud_mergeability_checks_are_paginated() {
    let env = Env::forge_stubs("bitbucket-mergeability-pages");
    env.edit_stub(|s| {
        s["pageSize"] = json!(1);
        s["changes"] = json!({"acme/widgets#7": {
            "state": "open", "head": "h1", "checks": [],
            "reviewDecision": null, "mergeable": "mergeable",
            "reviews": [], "comments": [], "threads": []
        }});
        s["mergeabilityChecks"] = json!([
            {"type": "current_user_permission_check", "status": "PASSED", "required": true, "blocking": false},
            {"type": "git_mergeability_check", "status": "PASSED", "required": true, "blocking": false}
        ]);
    });
    let cr = derived_name("bitbucket-cloud", BITBUCKET_HOST, PROJECT, "7");
    env.apply(change_request(
        "https://bitbucket.org/acme/widgets/pull-requests/7",
        json!({}),
    ))
    .ok();
    env.reconcile().ok();
    let st = env.change_request_status(&cr);
    assert_condition(&st, "Reachable", "True", "Observed");
    assert_eq!(st["mergeable"]["value"], true);
    assert!(env.forge_calls(BITBUCKET_HOST).iter().any(|call| {
        call["argv"]
            .to_string()
            .contains("mergeability/checks?opaque=x&cursor=1")
    }));
}

#[test]
fn bitbucket_cloud_mergeability_checks_handle_documented_statuses() {
    let env = Env::forge_stubs("bitbucket-mergeability-statuses");
    env.edit_stub(|s| {
        s["changes"] = json!({"acme/widgets#7": {
            "state": "open", "head": "h1", "checks": [],
            "reviewDecision": null, "mergeable": "mergeable",
            "reviews": [], "comments": [], "threads": []
        }});
        s["mergeabilityChecks"] = json!([
            {"type": "custom_pre_merge_check", "status": "SKIPPED", "required": false, "blocking": false}
        ]);
    });
    let cr = derived_name("bitbucket-cloud", BITBUCKET_HOST, PROJECT, "7");
    env.apply(change_request(
        "https://bitbucket.org/acme/widgets/pull-requests/7",
        json!({}),
    ))
    .ok();
    env.reconcile().ok();
    let st = env.change_request_status(&cr);
    assert_condition(&st, "Reachable", "True", "Observed");
    assert_eq!(st["mergeable"]["value"], true);

    env.edit_stub(|s| {
        s["mergeabilityChecks"] = json!([
            {"type": "custom_pre_merge_check", "status": "UNKNOWN", "required": false, "blocking": false}
        ]);
    });
    env.expire_wait_of("ChangeRequest", &cr);
    env.reconcile().ok();
    let st = env.change_request_status(&cr);
    assert_condition(&st, "Reachable", "True", "Observed");
    assert!(st["mergeable"]["value"].is_null());
}

#[test]
fn bitbucket_cloud_state_identity_and_absence_are_not_guessed() {
    let env = Env::forge_stubs("bitbucket-states");
    let model = |state: &str, head: &str| {
        json!({
            "state": state, "head": head, "checks": [], "reviewDecision": null,
            "mergeable": if state == "draft" { "draft" } else { "mergeable" },
            "reviews": [], "comments": [], "threads": []
        })
    };
    env.edit_stub(|s| {
        s["changes"] = json!({
            "acme/widgets#11": model("draft", "d1"),
            "acme/widgets#12": model("open", "o1"),
            "acme/widgets#13": model("closed", "c1"),
            "acme/widgets#14": model("merged", "m1")
        })
    });
    for id in 11..=14 {
        env.apply(change_request(
            &format!("https://bitbucket.org/acme/widgets/pull-requests/{id}"),
            json!({}),
        ))
        .ok();
    }
    env.reconcile().ok();
    for (id, state, phase) in [
        (11, "draft", "Watching"),
        (12, "open", "Watching"),
        (13, "closed", "Closed"),
        (14, "merged", "Settled"),
    ] {
        let cr = derived_name("bitbucket-cloud", BITBUCKET_HOST, PROJECT, &id.to_string());
        let st = env.change_request_status(&cr);
        assert_eq!(st["state"]["value"], state, "{st:#}");
        assert_eq!(st["phase"], phase, "{st:#}");
    }

    let cr = derived_name("bitbucket-cloud", BITBUCKET_HOST, PROJECT, "12");
    env.edit_stub(|s| s["wrongRepo"] = json!(true));
    env.expire_wait_of("ChangeRequest", &cr);
    env.reconcile().ok();
    let st = env.change_request_status(&cr);
    assert_condition(&st, "Reachable", "Unknown", "Unreadable");
    assert_eq!(st["state"]["value"], "open");

    env.edit_stub(|s| {
        s["wrongRepo"] = json!(false);
        s["noRepo"] = json!(true);
    });
    env.expire_wait_of("ChangeRequest", &cr);
    env.reconcile().ok();
    let st = env.change_request_status(&cr);
    assert_eq!(st["phase"], "Watching");
    assert_condition(&st, "Reachable", "Unknown", "Refused");

    env.edit_stub(|s| {
        s["noRepo"] = json!(false);
        s["repoRateLimit"] = json!(true);
        s["retryAfter"] = json!(45);
        s["changes"]
            .as_object_mut()
            .unwrap()
            .remove("acme/widgets#12");
    });
    env.expire_wait_of("ChangeRequest", &cr);
    let before = now_ms();
    env.reconcile().ok();
    let st = env.change_request_status(&cr);
    assert_condition(&st, "Reachable", "Unknown", "Throttled");
    assert!(st["notBefore"].as_u64().unwrap() >= before + 45_000);

    env.edit_stub(|s| s["repoRateLimit"] = json!(false));
    env.expire_wait_of("ChangeRequest", &cr);
    env.reconcile().ok();
    let st = env.change_request_status(&cr);
    assert_eq!(st["phase"], "Settled");
    assert_condition(&st, "Settled", "True", "Gone");

    env.apply(change_request(
        "https://bitbucket.example/acme/widgets/pull-requests/7",
        json!({}),
    ))
    .code(1)
    .stderr_has("not configured under [forge.hosts]");
}

#[test]
fn the_shipped_example_mirrors_a_change_offline_with_no_tools_on_path() {
    let mut env = Env::without_tools("shipped-forge-example");
    env.fake = env.root.join("fake-host");
    fs::write(
        &env.config,
        include_str!("../../../examples/fleet/fleet.toml"),
    )
    .unwrap();
    let manifest: Value =
        serde_json::from_str(include_str!("../../../examples/fleet/change-request.json")).unwrap();
    let forge = FakeForge::open("forge.example", &env.root.join("fake-forge")).unwrap();
    forge
        .set_change(PROJECT, "7", open_change(vec![note("n1", 10, "hello")]))
        .unwrap();
    let cr = derived_name("fake", "forge.example", PROJECT, "7");
    env.apply(manifest)
        .ok()
        .stdout_has(&format!("ChangeRequest/{cr} created"));
    env.reconcile().ok();
    let st = env.change_request_status(&cr);
    assert_eq!(st["revision"], 1);
    assert_eq!(st["everReady"], true);
    assert_eq!(seqs(&st), vec![("n1".to_string(), 1)]);
    env.assert_no_temp();
}

fn github_model() -> Value {
    json!({
        "state": "open", "head": "h1",
        "checks": [
            {"name": "ci", "state": "failure", "url": "https://github.test/acme/widgets/runs/1"},
            {"name": "lint", "state": "pending"},
            {"name": "docs", "state": "skipped"},
            {"name": "deploy", "kind": "status", "state": "success"}
        ],
        "reviewDecision": "changes-requested", "mergeable": "unstable",
        "reviews": [
            {"id": "r1", "author": "ann", "state": "changes-requested", "body": "needs work", "head": "h1", "at": 1_000, "editedAt": null},
            {"id": "r2", "author": "renovate", "bot": true, "state": "commented", "body": "lgtm", "head": "h0", "at": 900, "editedAt": null},
            {"id": "r3", "author": "old", "state": "dismissed", "body": "", "head": "h1", "at": 800, "editedAt": null}
        ],
        "comments": [
            {"id": "c1", "author": "ann", "body": "one", "at": 2_000, "editedAt": null},
            {"id": "c2", "author": "bob", "body": "two", "at": 2_001, "editedAt": 2_500},
            {"id": "c3", "author": "cat", "body": "three", "at": 2_002, "editedAt": null}
        ],
        "threads": [
            {"id": "t1", "resolved": false, "path": "src/lib.rs", "line": 3, "comments": [
                {"id": "ic1", "author": "ann", "body": "rename", "at": 3_000, "editedAt": null},
                {"id": "ic2", "author": "dev", "body": "done", "at": 3_001, "editedAt": null}
            ]},
            {"id": "t2", "resolved": true, "path": "README.md", "line": 1, "comments": [
                {"id": "ic3", "author": "ann", "body": "typo", "at": 3_002, "editedAt": null}
            ]}
        ]
    })
}

#[test]
fn github_pull_requests_are_read_through_gh_with_pagination_and_bounded_calls() {
    let env = Env::forge_stubs("github");
    env.edit_stub(|s| {
        s["pageSize"] = json!(2);
        s["changes"] = json!({"acme/widgets#7": github_model()});
    });
    let url = "https://github.test/acme/widgets/pull/7";
    let cr = derived_name("github", GITHUB_HOST, PROJECT, "7");
    env.apply(change_request(url, json!({})))
        .ok()
        .stdout_has(&format!("ChangeRequest/{cr} created"));
    let report = env.reconcile().ok().json();
    assert_eq!(report["errors"], json!([]), "{report:#}");
    let st = env.change_request_status(&cr);
    assert_eq!(st["phase"], "Watching", "{st:#}");
    assert_eq!(
        st["ref"],
        json!({"kind": "github", "host": GITHUB_HOST, "project": PROJECT, "id": "7"})
    );
    assert_eq!(st["state"]["value"], "open");
    assert_eq!(st["head"]["id"], "h1");
    assert_eq!(st["head"]["checks"], "failed");
    assert_eq!(
        st["head"]["failing"],
        json!([{"name": "ci", "url": "https://github.test/acme/widgets/runs/1"}])
    );
    assert_eq!(st["reviewDecision"]["value"], "changes-requested");
    assert_eq!(
        st["mergeable"],
        json!({"value": false, "reason": "unstable"})
    );
    assert_condition(&st, "Mergeable", "False", "Unstable");
    assert_eq!(st["threads"]["unresolved"], 1);
    let reviews = st["reviews"].as_array().unwrap();
    assert_eq!(
        reviews.len(),
        1,
        "only submitted reviews on the head: {reviews:#?}"
    );
    assert_eq!(reviews[0]["author"], "ann");
    let notes = st["notes"].as_array().unwrap();
    let ids: Vec<&str> = notes.iter().map(|n| n["id"].as_str().unwrap()).collect();
    assert_eq!(
        ids,
        vec!["r2", "r1", "c1", "c2", "c3", "ic1", "ic2", "ic3"],
        "every page, in creation order"
    );
    let by_id = |id: &str| notes.iter().find(|n| n["id"] == id).unwrap().clone();
    assert_eq!(by_id("r2")["bot"], true);
    assert_eq!(by_id("r2")["kind"], "review");
    assert_eq!(by_id("c2")["editedAt"], 2_500);
    assert_eq!(by_id("ic1")["thread"], "t1");
    assert_eq!(by_id("ic1")["resolved"], false);
    assert_eq!(by_id("ic1")["path"], "src/lib.rs");
    assert_eq!(by_id("ic3")["resolved"], true);
    assert!(notes.iter().all(|n| n["seq"] == 1));
    let calls = env.forge_calls(GITHUB_HOST);
    assert_eq!(
        calls.len(),
        2,
        "one call for everything's first page, one for the second pages: {calls:#?}"
    );
    let second = calls[1]["argv"].as_array().unwrap();
    for arg in [
        "withCore=false",
        "withReviews=true",
        "withComments=true",
        "withThreads=false",
        "reviewsAfter=1",
        "commentsAfter=1",
    ] {
        assert!(
            second.contains(&json!(arg)),
            "the continuation fetches only what has pages: {second:?}"
        );
    }
    for c in &calls {
        let argv = c["argv"].as_array().unwrap();
        assert_eq!(
            &argv[..4],
            &[
                json!("api"),
                json!("graphql"),
                json!("--hostname"),
                json!(GITHUB_HOST)
            ]
        );
        assert!(argv.contains(&json!("--include")));
        assert!(
            !c["envNames"]
                .as_array()
                .unwrap()
                .iter()
                .any(|n| n.as_str().unwrap().contains("TOKEN")),
            "no credential is passed"
        );
    }

    // The forge's rate limit: Unknown, with its Retry-After as the next poll.
    env.edit_stub(|s| s["retryAfter"] = json!(45));
    env.stub_fault("gh", "rate-limit", 1);
    env.expire_wait_of("ChangeRequest", &cr);
    let t = now_ms();
    env.reconcile().ok();
    let st = env.change_request_status(&cr);
    assert_condition(&st, "Reachable", "Unknown", "Throttled");
    assert_eq!(st["revision"], 1);
    let nb = st["notBefore"].as_u64().unwrap();
    assert!(nb >= t + 45_000 && nb < t + 45_000 + 60_000, "{st:#}");

    // No login, a malformed answer, a refusal: each Unknown, each with its reason.
    for (mode, reason) in [
        ("auth", "NotLoggedIn"),
        ("malformed", "Unreadable"),
        ("fail", "Unreadable"),
    ] {
        env.stub_fault("gh", mode, 1);
        env.expire_wait_of("ChangeRequest", &cr);
        env.reconcile().ok();
        let st = env.change_request_status(&cr);
        assert_condition(&st, "Reachable", "Unknown", reason);
        assert_eq!(st["revision"], 1, "{mode}");
    }

    // A hung gh costs one bound and leaves no process behind.
    env.stub_fault("gh", "hang", 1);
    env.expire_wait_of("ChangeRequest", &cr);
    env.reconcile().ok();
    let st = env.change_request_status(&cr);
    assert_condition(&st, "Reachable", "Unknown", "Timeout");
    let pids = env.grandchildren();
    assert!(!pids.is_empty(), "the stub hung with a grandchild");
    for pid in pids {
        assert!(
            !alive_after(pid, Duration::from_secs(10)),
            "process {pid} outlived the bound"
        );
    }

    // A pull request the repository no longer has is proven absent.
    env.edit_stub(|s| s["changes"] = json!({}));
    env.expire_wait_of("ChangeRequest", &cr);
    env.reconcile().ok();
    let st = env.change_request_status(&cr);
    assert_eq!(st["phase"], "Settled", "{st:#}");
    assert_condition(&st, "Settled", "True", "Gone");
}

#[test]
fn a_github_pull_request_whose_repository_cannot_be_seen_is_unknown_not_gone() {
    let env = Env::forge_stubs("github-no-repo");
    env.edit_stub(|s| {
        s["changes"] = json!({"acme/widgets#7": github_model()});
    });
    let cr = derived_name("github", GITHUB_HOST, PROJECT, "7");
    env.apply(change_request(
        "https://github.test/acme/widgets/pull/7",
        json!({}),
    ))
    .ok();
    env.reconcile().ok();
    env.edit_stub(|s| s["noRepo"] = json!(true));
    env.expire_wait_of("ChangeRequest", &cr);
    env.reconcile().ok();
    let st = env.change_request_status(&cr);
    assert_eq!(
        st["phase"], "Watching",
        "an ambiguous not-found is never gone: {st:#}"
    );
    assert_condition(&st, "Reachable", "Unknown", "Refused");
}

#[test]
fn azure_devops_services_pull_requests_use_canonical_organization_identity_and_complete_cli_observations()
 {
    let env = Env::forge_stubs("azure-devops");
    env.edit_stub(|s| {
        s["login"] = json!("identity-1");
        let mut model = github_model();
        model["checks"] = json!([
            {"name": "ci", "state": "success", "url": "https://ci.test/1"},
            {"name": "retired", "state": "skipped"}
        ]);
        model["policies"] = json!([
            {"name": "required-build", "blocking": true, "status": "rejected", "url": "https://policy.test/1"},
            {"name": "advisory", "blocking": false, "status": "broken", "url": "https://policy.test/2"}
        ]);
        s["changes"] = json!({"wingtip/project/widgets#7": model});
    });
    let canonical = "https://dev.azure.com/wingtip/project/_git/widgets/pullrequest/7";
    let cr = derived_name(
        "azure-devops",
        "wingtip.visualstudio.com",
        "project/widgets",
        "7",
    );
    env.apply(change_request(canonical, json!({})))
        .ok()
        .stdout_has(&format!("ChangeRequest/{cr} created"));
    let report = env.reconcile().ok().json();
    assert_eq!(report["errors"], json!([]), "{report:#}");
    let st = env.change_request_status(&cr);
    assert_eq!(
        st["ref"],
        json!({
            "kind": "azure-devops",
            "host": "wingtip.visualstudio.com",
            "project": "project/widgets",
            "id": "7"
        })
    );
    assert_eq!(st["state"]["value"], "open");
    assert_eq!(st["head"]["id"], "h1");
    assert_eq!(st["head"]["checks"], "failed");
    assert_eq!(
        st["head"]["failing"],
        json!([{"name": "required-build", "url": "https://policy.test/1"}])
    );
    assert_eq!(st["reviewDecision"]["value"], "changes-requested");
    assert_eq!(st["mergeable"], json!({"value": false, "reason": "policy"}));
    assert_eq!(st["threads"]["unresolved"], 1);
    assert!(
        st["reviews"].is_null() || st["reviews"].as_array().is_some_and(Vec::is_empty),
        "votes do not prove a head-bound review"
    );
    let notes = st["notes"].as_array().unwrap();
    assert_eq!(
        notes.len(),
        6,
        "text comments only, including resolved inline notes: {notes:#?}"
    );
    assert_eq!(notes.iter().filter(|n| n["resolved"] == false).count(), 2);

    let calls = env.forge_calls(AZURE_DEVOPS_HOST);
    assert_eq!(
        calls.len(),
        5,
        "one bounded call per documented collection: {calls:#?}"
    );
    assert!(calls.iter().all(|c| {
        let argv = c["argv"].as_array().unwrap();
        argv.starts_with(&[json!("devops"), json!("invoke")])
            && argv.contains(&json!("https://dev.azure.com/wingtip"))
            && !c["envNames"]
                .as_array()
                .unwrap()
                .iter()
                .any(|n| n.as_str().unwrap().contains("TOKEN") || n == "AZURE_DEVOPS_EXT_PAT")
    }));
    let policy_call = calls
        .iter()
        .find(|c| c["argv"].to_string().contains("policyEvaluations"))
        .expect("policy evaluation read");
    assert!(
        policy_call["argv"]
            .to_string()
            .contains("artifactId=vstfs:///CodeReview/CodeReviewId/project-id/7"),
        "the policy API has a different artifact identity from Git pull requests: {policy_call:#?}"
    );
    assert!(
        policy_call["argv"].to_string().contains("$top=1000"),
        "the bounded policy read requests an explicit page: {policy_call:#?}"
    );

    env.edit_stub(|s| {
        s["changes"]["wingtip/project/widgets#7"]["policies"] = json!(
            (0..1000)
                .map(|i| json!({"name": format!("policy-{i}"), "blocking": true, "status": "approved"}))
                .collect::<Vec<_>>()
        );
    });
    env.expire_wait_of("ChangeRequest", &cr);
    env.reconcile().ok();
    let st = env.change_request_status(&cr);
    assert_condition(&st, "Reachable", "Unknown", "Incomplete");
    assert_eq!(st["revision"], 1, "a full bounded page is not complete");
    env.edit_stub(|s| {
        s["changes"]["wingtip/project/widgets#7"]["policies"] = json!([
            {"name": "required-build", "blocking": true, "status": "rejected", "url": "https://policy.test/1"},
            {"name": "advisory", "blocking": false, "status": "broken", "url": "https://policy.test/2"}
        ]);
    });

    env.edit_stub(|s| s["retryAfter"] = json!(45));
    env.stub_fault("az", "rate-limit", 1);
    env.expire_wait_of("ChangeRequest", &cr);
    env.reconcile().ok();
    let st = env.change_request_status(&cr);
    assert_condition(&st, "Reachable", "Unknown", "Throttled");
    assert_eq!(st["revision"], 1);

    for (mode, reason) in [
        ("auth", "NotLoggedIn"),
        ("malformed", "Unreadable"),
        ("truncated", "Incomplete"),
        ("fail", "Unreadable"),
    ] {
        env.stub_fault("az", mode, 1);
        env.expire_wait_of("ChangeRequest", &cr);
        env.reconcile().ok();
        let st = env.change_request_status(&cr);
        assert_condition(&st, "Reachable", "Unknown", reason);
        assert_eq!(st["revision"], 1, "{mode}");
    }

    env.edit_stub(|s| s["malformedIterations"] = json!(true));
    env.expire_wait_of("ChangeRequest", &cr);
    env.reconcile().ok();
    let st = env.change_request_status(&cr);
    assert_condition(&st, "Reachable", "Unknown", "Unreadable");
    assert_eq!(st["revision"], 1, "a malformed later iteration is not h1");
    env.edit_stub(|s| s["malformedIterations"] = json!(false));

    env.edit_stub(|s| s["noRepo"] = json!(true));
    env.expire_wait_of("ChangeRequest", &cr);
    env.reconcile().ok();
    let st = env.change_request_status(&cr);
    assert_condition(&st, "Reachable", "Unknown", "Refused");
    assert_eq!(st["phase"], "Watching", "an ambiguous 404 is not absence");

    env.edit_stub(|s| {
        s["noRepo"] = json!(false);
        s["changes"]
            .as_object_mut()
            .unwrap()
            .remove("wingtip/project/widgets#7");
    });
    env.expire_wait_of("ChangeRequest", &cr);
    env.reconcile().ok();
    let st = env.change_request_status(&cr);
    assert_eq!(st["phase"], "Settled", "{st:#}");
    assert_condition(&st, "Settled", "True", "Gone");

    let other_org = env.apply(change_request(
        "https://dev.azure.com/other/project/_git/widgets/pullrequest/7",
        json!({}),
    ));
    other_org.ok();
    assert_ne!(
        cr,
        derived_name(
            "azure-devops",
            "other.visualstudio.com",
            "project/widgets",
            "7"
        ),
        "the organization participates in canonical identity"
    );
}

#[test]
fn azure_devops_draft_open_closed_and_completed_states_are_not_conflated() {
    let env = Env::forge_stubs("azure-states");
    env.edit_stub(|s| {
        let model = |state: &str, head: &str| {
            json!({
                "state": state,
                "head": head,
                "checks": [],
                "mergeable": if state == "draft" { "draft" } else { "mergeable" },
                "reviews": [],
                "comments": [],
                "threads": []
            })
        };
        s["changes"] = json!({
            "wingtip/project/widgets#11": model("draft", "d1"),
            "wingtip/project/widgets#12": model("open", "o1"),
            "wingtip/project/widgets#13": model("closed", "c1"),
            "wingtip/project/widgets#14": model("merged", "m1")
        });
    });
    for id in 11..=14 {
        env.apply(change_request(
            &format!("https://dev.azure.com/wingtip/project/_git/widgets/pullrequest/{id}"),
            json!({}),
        ))
        .ok();
    }
    env.reconcile().ok();
    for (id, state, phase) in [
        (11, "draft", "Watching"),
        (12, "open", "Watching"),
        (13, "closed", "Closed"),
        (14, "merged", "Settled"),
    ] {
        let cr = derived_name(
            "azure-devops",
            "wingtip.visualstudio.com",
            "project/widgets",
            &id.to_string(),
        );
        let status = env.change_request_status(&cr);
        assert_eq!(status["state"]["value"], state, "{status:#}");
        assert_eq!(status["phase"], phase, "{status:#}");
    }
}

#[test]
fn a_self_hosted_gitlab_merge_request_is_read_through_glab_with_its_hostname() {
    let env = Env::forge_stubs("gitlab");
    let project = "group/sub/project";
    let mut model = github_model();
    model["mergeable"] = json!("ci_must_pass");
    model["approvedBy"] = json!(["ann"]);
    // Enough discussions for a second page at the adapter's page size, all
    // older than the inline threads.
    let comments: Vec<Value> = (0..105u64)
        .map(|i| json!({"id": format!("c{i}"), "author": "ann", "body": format!("note {i}"), "at": 10 + i, "editedAt": null}))
        .collect();
    model["comments"] = json!(comments);
    env.edit_stub(|s| s["changes"] = json!({format!("{project}#3"): model}));
    let url = "https://gitlab.test/group/sub/project/-/merge_requests/3";
    let cr = derived_name("gitlab", GITLAB_HOST, project, "3");
    env.apply(change_request(url, json!({})))
        .ok()
        .stdout_has(&format!("ChangeRequest/{cr} created"));
    let report = env.reconcile().ok().json();
    assert_eq!(report["errors"], json!([]), "{report:#}");
    let st = env.change_request_status(&cr);
    assert_eq!(st["phase"], "Watching", "{st:#}");
    assert_eq!(
        st["ref"],
        json!({"kind": "gitlab", "host": GITLAB_HOST, "project": project, "id": "3"})
    );
    assert_eq!(st["head"]["checks"], "failed", "a failed pipeline");
    assert_eq!(st["head"]["failing"][0]["name"], "pipeline");
    assert_eq!(st["reviewDecision"]["value"], "approved");
    assert_eq!(
        st["reviews"],
        json!([{"id": "approval:ann", "author": "ann", "bot": false, "state": "approved", "head": "h1"}])
    );
    assert_eq!(
        st["mergeable"],
        json!({"value": false, "reason": "ci_must_pass"})
    );
    assert_condition(&st, "Mergeable", "False", "CiMustPass");
    assert_eq!(st["threads"]["unresolved"], 1);
    assert_eq!(
        st["notes"].as_array().unwrap().len(),
        NOTE_CAP,
        "capped from 108"
    );
    assert_eq!(
        st["droppedBefore"], 0,
        "nothing stored was rotated out: the oldest were never numbered"
    );
    let inline: Vec<&Value> = st["notes"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|n| n["kind"] == "inline")
        .collect();
    assert_eq!(inline.len(), 3);
    assert_eq!(inline[0]["thread"], "t1");
    assert_eq!(inline[0]["path"], "src/lib.rs");
    assert!(inline[0]["url"].as_str().unwrap().ends_with("#note_ic1"));
    let calls = env.forge_calls(GITLAB_HOST);
    let endpoints: Vec<String> = calls
        .iter()
        .map(|c| {
            c["argv"]
                .as_array()
                .unwrap()
                .last()
                .unwrap()
                .as_str()
                .unwrap()
                .to_string()
        })
        .collect();
    assert!(
        endpoints
            .iter()
            .all(|e| e.starts_with("projects/group%2Fsub%2Fproject/merge_requests/3")),
        "{endpoints:?}"
    );
    assert!(
        endpoints
            .iter()
            .any(|e| e.contains("discussions?per_page=100&page=2")),
        "a second page: {endpoints:?}"
    );
    assert!(endpoints.iter().any(|e| e.ends_with("/approvals")));
    for c in &calls {
        let argv = c["argv"].as_array().unwrap();
        assert_eq!(
            &argv[..3],
            &[json!("api"), json!("--hostname"), json!(GITLAB_HOST)]
        );
    }

    // 429 with Retry-After.
    env.edit_stub(|s| s["retryAfter"] = json!(60));
    env.stub_fault("glab", "rate-limit", 1);
    env.expire_wait_of("ChangeRequest", &cr);
    let t = now_ms();
    env.reconcile().ok();
    let st = env.change_request_status(&cr);
    assert_condition(&st, "Reachable", "Unknown", "Throttled");
    assert!(st["notBefore"].as_u64().unwrap() >= t + 60_000);

    // Gone: the merge request 404s while the project answers.
    env.edit_stub(|s| s["changes"] = json!({}));
    env.expire_wait_of("ChangeRequest", &cr);
    env.reconcile().ok();
    let st = env.change_request_status(&cr);
    assert_eq!(st["phase"], "Settled");
    assert_condition(&st, "Settled", "True", "Gone");

    // A project that cannot be seen is not absence.
    env.edit_stub(|s| {
        s["changes"] = json!({format!("{project}#4"): github_model()});
    });
    let cr4 = derived_name("gitlab", GITLAB_HOST, project, "4");
    env.apply(change_request(
        "https://gitlab.test/group/sub/project/-/merge_requests/4",
        json!({}),
    ))
    .ok();
    env.reconcile().ok();
    env.edit_stub(|s| s["noRepo"] = json!(true));
    env.expire_wait_of("ChangeRequest", &cr4);
    env.reconcile().ok();
    let st = env.change_request_status(&cr4);
    assert_eq!(st["phase"], "Watching");
    assert_condition(&st, "Reachable", "Unknown", "Refused");
}

#[test]
fn a_closed_draft_github_pull_request_is_closed_and_settles() {
    // GitHub keeps isDraft true on a draft pull request that is closed
    // without being marked ready. The forge's state is terminal; the draft
    // flag must not keep the mirror Watching forever.
    let env = Env::forge_stubs("github-closed-draft");
    let mut model = github_model();
    model["state"] = json!("closed");
    model["draft"] = json!(true);
    env.edit_stub(|s| s["changes"] = json!({"acme/widgets#7": model}));
    let url = "https://github.test/acme/widgets/pull/7";
    let cr = derived_name("github", GITHUB_HOST, PROJECT, "7");
    env.apply(change_request(url, json!({"closedGraceMs": 1000})))
        .ok();
    let report = env.reconcile().ok().json();
    assert_eq!(report["errors"], json!([]), "{report:#}");
    let st = env.change_request_status(&cr);
    assert_eq!(st["state"]["value"], "closed", "{st:#}");
    assert_eq!(st["phase"], "Closed", "{st:#}");
    thread::sleep(Duration::from_millis(1100));
    env.expire_wait_of("ChangeRequest", &cr);
    env.reconcile().ok();
    let st = env.change_request_status(&cr);
    assert_eq!(st["phase"], "Settled", "{st:#}");
    assert_condition(&st, "Settled", "True", "ClosedGraceExpired");
}

#[test]
fn a_closed_draft_is_closed_on_every_forge() {
    // GitLab, Azure DevOps and Bitbucket Cloud can also close a draft
    // without marking it ready. Each adapter must let the terminal state win.
    for (kind, host, project, url) in [
        (
            "gitlab",
            GITLAB_HOST,
            "group/project",
            "https://gitlab.test/group/project/-/merge_requests/7",
        ),
        (
            "azure-devops",
            "wingtip.visualstudio.com",
            "project/widgets",
            "https://dev.azure.com/wingtip/project/_git/widgets/pullrequest/7",
        ),
        (
            "bitbucket-cloud",
            BITBUCKET_HOST,
            PROJECT,
            "https://bitbucket.org/acme/widgets/pull-requests/7",
        ),
    ] {
        let env = Env::forge_stubs(&format!("{kind}-closed-draft"));
        let key = match kind {
            "azure-devops" => format!("wingtip/{project}#7"),
            _ => format!("{project}#7"),
        };
        let model = json!({
            "state": "closed", "draft": true, "head": "h1", "checks": [],
            "reviewDecision": null, "mergeable": "mergeable",
            "reviews": [], "comments": [], "threads": []
        });
        env.edit_stub(|s| s["changes"] = json!({key.as_str(): model}));
        env.apply(change_request(url, json!({}))).ok();
        let report = env.reconcile().ok().json();
        assert_eq!(report["errors"], json!([]), "{kind}: {report:#}");
        let st = env.change_request_status(&derived_name(kind, host, project, "7"));
        assert_eq!(st["state"]["value"], "closed", "{kind}: {st:#}");
        assert_eq!(st["phase"], "Closed", "{kind}: {st:#}");
    }
}
