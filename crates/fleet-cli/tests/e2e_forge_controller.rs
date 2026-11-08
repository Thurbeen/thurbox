//! The Forge controller end to end: `fleet declare` through the real
//! binary, the ChangeRequest it ensures, the join message and the relay
//! landing in the Task's inbox through the file store's typed mutation, and
//! TaskController delivering them to the fake host, across process restarts.

mod support;

use std::fs;

use fleet_api::follow::worker_mark;
use fleet_api::forge::{
    ChangeSnapshot, ChangeState, Checks, FailingCheck, HeadSeen, Mergeable, Note, NoteKind,
    ReviewDecision, Threads,
};
use fleet_forge::FakeForge;
use serde_json::{Value, json};
use support::{Env, FORGE_HOST, assert_condition, base_config, derived_name, task};

const PROJECT: &str = "acme/widgets";
const URL: &str = "https://forge.test/acme/widgets/pull/7";
const LOGIN: &str = "fleet-bot";

fn note(id: &str, at: u64, author: &str, body: &str) -> Note {
    Note {
        id: id.into(),
        kind: NoteKind::Comment,
        author: author.into(),
        bot: false,
        thread: None,
        resolved: None,
        path: None,
        line: None,
        at,
        edited_at: None,
        url: format!("{URL}#note-{id}"),
        body: body.into(),
    }
}

fn inline(id: &str, at: u64, author: &str, thread: &str, resolved: bool, body: &str) -> Note {
    Note {
        kind: NoteKind::Inline,
        thread: Some(thread.into()),
        resolved: Some(resolved),
        path: Some("src/lib.rs".into()),
        line: Some(12),
        ..note(id, at, author, body)
    }
}

fn draft(notes: Vec<Note>) -> ChangeSnapshot {
    ChangeSnapshot {
        state: ChangeState::Draft,
        head: HeadSeen {
            id: "aaa111".into(),
            checks: Checks::Pending,
            failing: Vec::new(),
        },
        review_decision: ReviewDecision::None,
        mergeable: Mergeable {
            value: Some(false),
            reason: Some("draft".into()),
        },
        threads: Threads {
            unresolved: notes.iter().filter(|n| n.resolved == Some(false)).count() as u64,
        },
        reviews: Vec::new(),
        notes,
    }
}

/// A briefed Task on the fake host beside a fake forge holding change 7
/// with one note, Fleet logged in as `fleet-bot`.
fn setup(name: &str) -> (Env, FakeForge, String) {
    let mut env = Env::forge(name);
    env.set_var("USER", "tester");
    let forge = env.fake_forge();
    forge.update(|s| s.login = Some(LOGIN.into())).unwrap();
    forge
        .set_change(
            PROJECT,
            "7",
            draft(vec![note("n1", 10, "ann", "first look")]),
        )
        .unwrap();
    env.apply(base_config()).ok();
    env.apply(task("t1", true)).ok();
    env.reconcile().ok();
    assert_eq!(env.status("t1")["phase"], "Briefed");
    let cr = derived_name("fake", FORGE_HOST, PROJECT, "7");
    (env, forge, cr)
}

fn declare(env: &Env, task: &str, url: &str) -> support::Run {
    env.fleet(&["declare", task, "--change-request", url])
}

fn full_status(env: &Env, task: &str) -> Value {
    env.get(task)["status"].clone()
}

fn entry<'a>(status: &'a Value, cr: &str) -> &'a Value {
    &status["forge"]["changes"][cr]
}

fn delivered(status: &Value) -> Vec<Value> {
    status["deliveries"]["receipts"]
        .as_array()
        .cloned()
        .unwrap_or_default()
}

/// The texts the fake session was given, the brief first.
fn texts(env: &Env, task: &str) -> Vec<String> {
    env.host()
        .session(&env.session_key(task))
        .unwrap()
        .expect("a session")
        .texts
}

fn poll(env: &Env, cr: &str) {
    env.expire_wait_of("ChangeRequest", cr);
    env.reconcile().ok();
}

#[test]
fn declare_creates_the_change_request_and_the_worker_is_joined_at_the_present() {
    let (env, forge, cr) = setup("declare-join");
    let uid = env.get("t1")["metadata"]["uid"]
        .as_str()
        .unwrap()
        .to_string();
    declare(&env, "t1", URL)
        .ok()
        .stdout_has(&format!("declared ChangeRequest/{cr} (created; declared)"));
    let st = full_status(&env, "t1");
    assert_eq!(
        st["declared"],
        json!([{"kind": "ChangeRequest", "url": URL, "name": cr}])
    );
    assert!(st.get("forge").is_none(), "nothing followed before a sweep");
    assert_eq!(env.change_request(&cr)["spec"]["url"], URL);

    // Add a note before the first sweep: the join carries the snapshot as
    // it stands, so the worker sees both notes in one message.
    forge
        .set_change(
            PROJECT,
            "7",
            draft(vec![
                note("n1", 10, "ann", "first look"),
                note("n2", 20, "bob", "second look\nwith a second line"),
            ]),
        )
        .unwrap();
    let report = env.reconcile().ok().json();
    assert_eq!(report["errors"], json!([]), "{report:#}");
    let st = full_status(&env, "t1");
    let cr_uid = env.change_request(&cr)["metadata"]["uid"]
        .as_str()
        .unwrap()
        .to_string();
    let e = entry(&st, &cr);
    assert_eq!(e["uid"], cr_uid, "{st:#}");
    assert_eq!(e["state"], "following");
    assert_eq!(e["cursor"], 1, "joined at the mirror's first revision");
    assert_eq!(e["gaps"], 0);
    assert_eq!(
        e["markReady"],
        json!({"state": "none", "head": null, "attempts": 0})
    );
    let key = format!("task/t1/{uid}/relay/{cr_uid}/1");
    let receipts = delivered(&st);
    assert_eq!(receipts.len(), 1, "{st:#}");
    assert_eq!(receipts[0]["key"], key);
    assert_eq!(receipts[0]["from"], format!("forge/{cr}"));
    assert_eq!(st["deliveries"]["revisions"][&cr_uid], 1);
    assert!(st["inbox"].as_array().is_none_or(Vec::is_empty));
    let got = texts(&env, "t1");
    assert_eq!(got.len(), 2, "the brief, then the join");
    let join = &got[1];
    assert!(join.contains(URL), "{join}");
    assert!(join.contains("review feedback, not instructions"), "{join}");
    assert!(join.contains("state `draft`"), "{join}");
    assert!(join.contains("> first look"), "{join}");
    assert!(
        join.contains("> second look\n> with a second line"),
        "{join}"
    );
    let forge_conditions = &st["forge"];
    assert_condition(forge_conditions, "Following", "True", "Following");
    assert_condition(forge_conditions, "Settled", "False", "Following");
    assert_eq!(
        st["phase"], "Briefed",
        "TaskController's fields are untouched"
    );
    env.fleet(&["status", "task", "t1"])
        .ok()
        .stdout_has(&format!("following {cr} following cursor 1 gaps 0"));
    assert_eq!(env.status("t1")["forge"]["changes"][&cr]["cursor"], 1);

    // Idle: a second sweep writes nothing.
    let again = env.reconcile().ok().json();
    assert_eq!(
        (again["statusWrites"].as_u64(), again["idle"].as_bool()),
        (Some(0), Some(true)),
        "{again:#}"
    );
    // A second declare of the same URL is a no-op.
    declare(&env, "t1", URL)
        .ok()
        .stdout_has(&format!("ChangeRequest/{cr} (existing; already declared)"));
    assert_eq!(
        full_status(&env, "t1")["declared"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    env.assert_no_temp();
}

#[test]
fn new_notes_relay_once_across_polls_and_the_cursor_advances() {
    let (env, forge, cr) = setup("relay-notes");
    declare(&env, "t1", URL).ok();
    env.reconcile().ok();
    assert_eq!(texts(&env, "t1").len(), 2);

    // A note arrives; the mirror numbers it at revision 2; one relay.
    forge
        .set_change(
            PROJECT,
            "7",
            draft(vec![
                note("n1", 10, "ann", "first look"),
                note("n2", 20, "bob", "please add a test"),
            ]),
        )
        .unwrap();
    poll(&env, &cr);
    let st = full_status(&env, "t1");
    assert_eq!(entry(&st, &cr)["cursor"], 2, "{st:#}");
    let got = texts(&env, "t1");
    assert_eq!(got.len(), 3, "{got:#?}");
    assert!(got[2].contains("> please add a test"), "{}", got[2]);
    assert!(!got[2].contains("> first look"), "only what is new");
    assert!(got[2].contains("revision 2"), "{}", got[2]);

    // Several quiet polls: nothing more.
    for _ in 0..3 {
        poll(&env, &cr);
    }
    assert_eq!(texts(&env, "t1").len(), 3, "once and only once");
    assert_eq!(delivered(&full_status(&env, "t1")).len(), 2);

    // An edit moves the note's number: it is relayed again.
    let mut edited = note("n2", 20, "bob", "please add a test, and a doc");
    edited.edited_at = Some(30);
    forge
        .set_change(
            PROJECT,
            "7",
            draft(vec![note("n1", 10, "ann", "first look"), edited]),
        )
        .unwrap();
    poll(&env, &cr);
    let got = texts(&env, "t1");
    assert_eq!(got.len(), 4);
    assert!(
        got[3].contains("> please add a test, and a doc"),
        "{}",
        got[3]
    );
    assert_eq!(entry(&full_status(&env, "t1"), &cr)["cursor"], 3);
    env.assert_no_temp();
}

#[test]
fn the_workers_own_marked_note_is_not_relayed_and_another_tasks_is_labelled() {
    let (env, forge, cr) = setup("relay-marks");
    env.apply(task("t2", true)).ok();
    env.reconcile().ok();
    declare(&env, "t1", URL).ok();
    env.reconcile().ok();
    let uid1 = env.get("t1")["metadata"]["uid"]
        .as_str()
        .unwrap()
        .to_string();
    let uid2 = env.get("t2")["metadata"]["uid"]
        .as_str()
        .unwrap()
        .to_string();
    forge
        .set_change(
            PROJECT,
            "7",
            draft(vec![
                note("n1", 10, "ann", "first look"),
                note(
                    "n2",
                    20,
                    LOGIN,
                    &format!("Fixed in the next push.\n{}", worker_mark(&uid1)),
                ),
                note(
                    "n3",
                    30,
                    LOGIN,
                    &format!("I will take the docs.\n{}", worker_mark(&uid2)),
                ),
            ]),
        )
        .unwrap();
    poll(&env, &cr);
    let got = texts(&env, "t1");
    assert_eq!(got.len(), 3, "{got:#?}");
    let relay = &got[2];
    assert!(!relay.contains("Fixed in the next push"), "{relay}");
    assert!(relay.contains("> I will take the docs."), "{relay}");
    assert!(
        relay.contains("from another Fleet worker, Task t2"),
        "{relay}"
    );
    assert_eq!(entry(&full_status(&env, "t1"), &cr)["cursor"], 2);
}

#[test]
fn a_resolved_thread_is_not_relayed_and_a_checks_transition_names_what_failed() {
    let (env, forge, cr) = setup("relay-threads-checks");
    declare(&env, "t1", URL).ok();
    env.reconcile().ok();
    let mut change = draft(vec![
        note("n1", 10, "ann", "first look"),
        inline("i1", 20, "ann", "t1", true, "rename this"),
        inline("i2", 25, "ann", "t2", false, "and this"),
    ]);
    change.head = HeadSeen {
        id: "bbb222".into(),
        checks: Checks::Failed,
        failing: vec![
            FailingCheck {
                name: "build".into(),
                url: Some("https://forge.test/ci/9".into()),
            },
            FailingCheck {
                name: "lint".into(),
                url: None,
            },
        ],
    };
    forge.set_change(PROJECT, "7", change).unwrap();
    poll(&env, &cr);
    let got = texts(&env, "t1");
    assert_eq!(got.len(), 3, "{got:#?}");
    let relay = &got[2];
    assert!(!relay.contains("> rename this"), "resolved: {relay}");
    assert!(relay.contains("> and this"), "{relay}");
    assert!(relay.contains("src/lib.rs:12"), "{relay}");
    assert!(
        relay.contains(
            "checks `failed` on head `bbb222`, failing: build (https://forge.test/ci/9), lint"
        ),
        "{relay}"
    );
    let st = full_status(&env, "t1");
    assert_eq!(
        st["deliveries"]["receipts"][1]["from"],
        format!("forge/{cr}")
    );
}

#[test]
fn spec_forge_relay_notes_false_stops_notes_and_not_checks() {
    let (env, forge, cr) = setup("relay-spec");
    declare(&env, "t1", URL).ok();
    env.reconcile().ok();
    // spec.forge is mutable like dispatch.
    let mut t = task("t1", true);
    t["spec"]["forge"] = json!({"relay": {"notes": false}, "draft": {"policy": null}});
    env.apply(t.clone()).ok().stdout_has("Task/t1 configured");
    assert_eq!(
        env.get("t1")["spec"]["forge"],
        json!({"relay": {"notes": false}, "draft": {"policy": null}}),
        "an explicit null policy is stored as such"
    );
    let mut change = draft(vec![
        note("n1", 10, "ann", "first look"),
        note("n2", 20, "bob", "a new note"),
    ]);
    change.head.checks = Checks::Passed;
    forge.set_change(PROJECT, "7", change).unwrap();
    poll(&env, &cr);
    let got = texts(&env, "t1");
    assert_eq!(got.len(), 3, "{got:#?}");
    assert!(!got[2].contains("> a new note"), "{}", got[2]);
    assert!(
        got[2].contains("checks `passed` on head `aaa111`"),
        "{}",
        got[2]
    );
    assert_eq!(entry(&full_status(&env, "t1"), &cr)["cursor"], 2);
    // The brief stays immutable.
    t["spec"]["brief"] = json!("another brief");
    env.apply(t)
        .code(1)
        .stderr_has("immutable except spec.dispatch and spec.forge");
}

#[test]
fn two_tasks_declaring_one_url_each_get_their_own_join_and_cursor() {
    let (env, forge, cr) = setup("two-followers");
    env.apply(task("t2", true)).ok();
    env.reconcile().ok();
    declare(&env, "t1", URL)
        .ok()
        .stdout_has("(created; declared)");
    env.reconcile().ok();
    // t1 is at revision 1; the forge moves before t2 joins.
    forge
        .set_change(
            PROJECT,
            "7",
            draft(vec![
                note("n1", 10, "ann", "first look"),
                note("n2", 20, "bob", "later"),
            ]),
        )
        .unwrap();
    poll(&env, &cr);
    declare(&env, "t2", URL)
        .ok()
        .stdout_has("(existing; declared)");
    env.reconcile().ok();
    let listed = env.fleet(&["list", "changerequests", "--json"]).ok().json();
    assert_eq!(listed.as_array().unwrap().len(), 1, "one mirror");
    let s1 = full_status(&env, "t1");
    let s2 = full_status(&env, "t2");
    assert_eq!(entry(&s1, &cr)["cursor"], 2);
    assert_eq!(entry(&s2, &cr)["cursor"], 2);
    assert_eq!(texts(&env, "t1").len(), 3, "join, then the relay");
    let t2 = texts(&env, "t2");
    assert_eq!(t2.len(), 2, "one join holding both notes");
    assert!(
        t2[1].contains("> first look") && t2[1].contains("> later"),
        "{}",
        t2[1]
    );
}

#[test]
fn a_gap_delivers_the_snapshot_and_advances_the_cursor() {
    let (env, forge, cr) = setup("relay-gap");
    declare(&env, "t1", URL).ok();
    env.reconcile().ok();
    // Rotate more than the cap past the cursor in one poll: everything the
    // follower would have been owed below the window is gone.
    let many = |n: u64| -> Vec<Note> {
        let mut notes = vec![note("n1", 10, "ann", "first look")];
        notes.extend(
            (0..fleet_api::change_request::NOTE_CAP as u64 + n)
                .map(|i| note(&format!("m{i}"), 100 + i, "ann", &format!("note {i}"))),
        );
        notes
    };
    forge.set_change(PROJECT, "7", draft(many(5))).unwrap();
    poll(&env, &cr);
    let cr_st = env.change_request_status(&cr);
    assert_eq!(cr_st["droppedBefore"], 1, "{cr_st:#}");
    // The follower's cursor (1) is at droppedBefore, not below it: no gap
    // yet. One more rotation drops a note numbered 2.
    let st = full_status(&env, "t1");
    assert_eq!(entry(&st, &cr)["cursor"], 2, "{st:#}");
    assert_eq!(entry(&st, &cr)["gaps"], 0);
    // Move the cursor back as a follower that fell behind would be, then
    // rotate again.
    let path = env.object_path("Task", "t1");
    let mut raw: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    raw["status"]["forge"]["changes"][&cr]["cursor"] = json!(1);
    fs::write(&path, raw.to_string()).unwrap();
    forge.set_change(PROJECT, "7", draft(many(10))).unwrap();
    poll(&env, &cr);
    let st = full_status(&env, "t1");
    assert_eq!(entry(&st, &cr)["cursor"], 3, "{st:#}");
    assert_eq!(entry(&st, &cr)["gaps"], 1);
    assert_condition(&st["forge"], "NeedsAttention", "True", "RelayGap");
    let got = texts(&env, "t1");
    let last = got.last().unwrap();
    assert!(last.contains("were lost"), "{last}");
    assert!(last.contains("whole retained snapshot"), "{last}");
    assert!(
        last.contains(&format!(
            "note {}",
            fleet_api::change_request::NOTE_CAP as u64 + 9
        )),
        "{last}"
    );
    // The next clean relay clears the attention.
    let mut notes = many(10);
    notes.push(note("fresh", 900, "bob", "after the gap"));
    forge.set_change(PROJECT, "7", draft(notes)).unwrap();
    poll(&env, &cr);
    let st = full_status(&env, "t1");
    assert_condition(&st["forge"], "NeedsAttention", "False", "Resolved");
    assert!(
        texts(&env, "t1")
            .last()
            .unwrap()
            .contains("> after the gap")
    );
}

#[test]
fn a_retired_mirror_is_change_gone_and_a_redeclare_rebinds_to_the_new_uid() {
    let (env, forge, cr) = setup("rebind");
    declare(&env, "t1", URL).ok();
    env.reconcile().ok();
    let old_uid = env.change_request(&cr)["metadata"]["uid"]
        .as_str()
        .unwrap()
        .to_string();
    // Retention: a person deletes the mirror, as its controller would after
    // the settled TTL; the follower sees readable absence.
    env.fleet(&["delete", "changerequest", &cr]).ok();
    env.reconcile().ok();
    let st = full_status(&env, "t1");
    assert_eq!(entry(&st, &cr)["uid"], old_uid, "the entry is kept");
    assert_condition(&st["forge"], "NeedsAttention", "True", "ChangeGone");
    env.fleet(&["status", "task", "t1"])
        .ok()
        .stdout_has("ChangeGone");
    // Re-declare: a new object, a new uid, a new join at its first revision.
    forge
        .set_change(
            PROJECT,
            "7",
            draft(vec![
                note("n1", 10, "ann", "first look"),
                note("n2", 20, "bob", "second look"),
            ]),
        )
        .unwrap();
    declare(&env, "t1", URL)
        .ok()
        .stdout_has("(created; already declared)");
    env.reconcile().ok();
    let new_uid = env.change_request(&cr)["metadata"]["uid"]
        .as_str()
        .unwrap()
        .to_string();
    assert_ne!(new_uid, old_uid);
    let st = full_status(&env, "t1");
    let e = entry(&st, &cr);
    assert_eq!(e["uid"], new_uid, "{st:#}");
    assert_eq!(e["cursor"], 1);
    assert_condition(&st["forge"], "NeedsAttention", "False", "Resolved");
    let got = texts(&env, "t1");
    assert_eq!(got.len(), 3, "{got:#?}");
    assert!(got[2].contains("declared again"), "{}", got[2]);
    assert!(got[2].contains("> second look"), "{}", got[2]);
    let receipts = delivered(&st);
    assert!(
        receipts[1]["key"]
            .as_str()
            .unwrap()
            .contains(&format!("/relay/{new_uid}/1")),
        "{receipts:#?}"
    );
    assert_eq!(st["deliveries"]["revisions"][&new_uid], 1);
}

#[test]
fn a_concluded_task_still_receives_a_relay_and_a_deleted_task_leaves_the_mirror_alone() {
    let (env, forge, cr) = setup("concluded-and-deleted");
    declare(&env, "t1", URL).ok();
    env.reconcile().ok();
    env.fleet(&[
        "result",
        "t1",
        "--generation",
        "1",
        "--outcome",
        "shipped",
        "--output",
        "prUrl=\"https://example.test/pr/1\"",
    ])
    .ok();
    env.reconcile().ok();
    assert_eq!(env.status("t1")["phase"], "Concluded");
    forge
        .set_change(
            PROJECT,
            "7",
            draft(vec![
                note("n1", 10, "ann", "first look"),
                note("n2", 20, "bob", "after the result"),
            ]),
        )
        .unwrap();
    poll(&env, &cr);
    let got = texts(&env, "t1");
    assert_eq!(got.len(), 3, "{got:#?}");
    assert!(got[2].contains("> after the result"));
    assert_eq!(env.status("t1")["phase"], "Concluded");
    // A declaration after the result is still accepted; it concludes nothing.
    env.fleet(&[
        "declare",
        "t1",
        "--json",
        r#"{"kind": "Note", "text": "done"}"#,
    ])
    .ok()
    .stdout_has("declared Note (declared)");
    assert_eq!(
        full_status(&env, "t1")["declared"]
            .as_array()
            .unwrap()
            .len(),
        2
    );

    env.host()
        .set_at_rest(&env.session_key("t1"), true)
        .unwrap();
    env.fleet(&["delete", "task", "t1"]).ok();
    env.reconcile().ok();
    env.fleet(&["get", "task", "t1"]).code(3);
    assert_eq!(
        env.change_request(&cr)["status"]["revision"],
        2,
        "untouched"
    );
    assert_eq!(env.change_request(&cr)["metadata"]["name"], cr);
}

#[test]
fn an_orphan_from_a_crash_between_the_two_steps_is_found_by_the_next_declare() {
    let (env, _forge, cr) = setup("orphan");
    declare(&env, "t1", URL)
        .ok()
        .stdout_has("(created; declared)");
    // The second step is lost: the record as it would be after a crash
    // between the ChangeRequest's creation and the declaration's append.
    let path = env.object_path("Task", "t1");
    let mut raw: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    raw["status"].as_object_mut().unwrap().remove("declared");
    fs::write(&path, raw.to_string()).unwrap();
    env.reconcile().ok();
    assert!(
        full_status(&env, "t1").get("forge").is_none(),
        "nobody follows it"
    );
    assert_eq!(
        env.change_request(&cr)["status"]["revision"],
        1,
        "polled anyway"
    );
    declare(&env, "t1", URL)
        .ok()
        .stdout_has("(existing; declared)");
    env.reconcile().ok();
    assert_eq!(entry(&full_status(&env, "t1"), &cr)["cursor"], 1);
    assert_eq!(texts(&env, "t1").len(), 2);
}

#[test]
fn declare_is_refused_before_the_brief_for_an_unknown_host_and_without_a_kind() {
    let mut env = Env::forge("declare-refused");
    env.set_var("USER", "tester");
    env.apply(task("t1", false)).ok();
    declare(&env, "t1", URL)
        .code(1)
        .stderr_has("no brief has a receipt yet");
    assert!(
        env.fleet(&["list", "changerequests", "--json"])
            .ok()
            .json()
            .as_array()
            .unwrap()
            .is_empty(),
        "an ineligible Task creates no orphan"
    );
    declare(&env, "t9", URL).code(3);
    let mut t = task("t1", true);
    t["spec"]["dispatch"] = json!(true);
    env.apply(t).ok();
    env.reconcile().ok();
    declare(&env, "t1", "https://elsewhere.test/acme/widgets/pull/7")
        .code(1)
        .stderr_has("not configured under [forge.hosts]");
    declare(&env, "t1", "https://forge.test/acme/widgets")
        .code(1)
        .stderr_has("does not name a change");
    env.fleet(&["declare", "t1", "--json", r#"{"url": "x"}"#])
        .code(1)
        .stderr_has("string `kind`");
    env.fleet(&["declare", "t1", "--json", "not json"])
        .code(1)
        .stderr_has("not a JSON object");
    env.fleet(&["declare", "t1"])
        .code(1)
        .stderr_has("exactly one of");
    // Without a [forge] section a change cannot be declared, but a fact can.
    let plain = Env::new("declare-no-forge");
    plain.apply(task("t1", true)).ok();
    plain.reconcile().ok();
    declare(&plain, "t1", URL)
        .code(1)
        .stderr_has("needs a [forge] section");
    plain
        .fleet(&["declare", "t1", "--json", r#"{"kind": "Fact"}"#])
        .ok();
    plain
        .fleet(&["declare", "t1", "--json", r#"{"kind": "Fact"}"#])
        .ok()
        .stdout_has("already declared");
    // A deleting Task refuses both.
    env.fleet(&["delete", "task", "t1"]).ok();
    declare(&env, "t1", URL).code(1).stderr_has("being deleted");
}

#[test]
fn the_relay_and_mark_ready_keys_load_in_the_config_and_a_bad_one_is_refused() {
    let (env, forge, cr) = setup("config-keys");
    let cfg = fs::read_to_string(&env.config).unwrap().replace(
        "[forge]\n",
        "[forge]\nrelayNotes = false\nrelayChecks = true\nrelayBots = true\n",
    );
    assert!(cfg.contains("relayBots"), "{cfg}");
    fs::write(&env.config, cfg.clone()).unwrap();
    declare(&env, "t1", URL).ok();
    env.reconcile().ok();
    let mut bot = note("b1", 20, "coverage[bot]", "coverage 93%");
    bot.bot = true;
    let mut change = draft(vec![note("n1", 10, "ann", "first look"), bot]);
    change.head.checks = Checks::Passed;
    forge.set_change(PROJECT, "7", change).unwrap();
    poll(&env, &cr);
    let got = texts(&env, "t1");
    assert_eq!(got.len(), 3, "{got:#?}");
    assert!(
        !got[2].contains("coverage 93%"),
        "notes are off: {}",
        got[2]
    );
    assert!(got[2].contains("checks `passed`"), "{}", got[2]);
    // Bots relay where configured, once notes are back on for this Task.
    let mut t = task("t1", true);
    t["spec"]["forge"] = json!({"relay": {"notes": true}});
    env.apply(t).ok();
    let mut bot = note("b2", 30, "coverage[bot]", "coverage 94%");
    bot.bot = true;
    let mut change = draft(vec![note("n1", 10, "ann", "first look"), bot]);
    change.head.checks = Checks::Passed;
    forge.set_change(PROJECT, "7", change).unwrap();
    poll(&env, &cr);
    let got = texts(&env, "t1");
    assert_eq!(got.len(), 4, "{got:#?}");
    assert!(got[3].contains("coverage 94%"), "{}", got[3]);
    assert!(got[3].contains("(comment, bot)"), "{}", got[3]);
    // The mark-ready keys load together; a default naming no policy, a
    // predicate outside the vocabulary, and the two fixed predicates named
    // as if they were optional are each refused at load.
    fs::write(
        &env.config,
        format!(
            "{}\n[forge.readyPolicies.checks]\nchecksPassed = true\n",
            cfg.replace(
                "[forge]\n",
                "[forge]\nnoCheckGraceMs = 1\ndraftPolicy = \"checks\"\n"
            )
        ),
    )
    .unwrap();
    env.fleet(&["list", "changerequest"]).ok();
    fs::write(
        &env.config,
        cfg.replace("[forge]\n", "[forge]\ndraftPolicy = \"checks\"\n"),
    )
    .unwrap();
    env.fleet(&["list", "changerequest"])
        .code(1)
        .stderr_has("forge.draftPolicy \"checks\" is not defined under [forge.readyPolicies]");
    for bad in ["approved = true", "workerDone = false", "workerIdle = true"] {
        fs::write(
            &env.config,
            format!("{cfg}\n[forge.readyPolicies.checks]\n{bad}\n"),
        )
        .unwrap();
        env.fleet(&["list", "changerequest"])
            .code(1)
            .stderr_has("unknown field");
    }
}

#[test]
fn the_shipped_example_follows_a_declared_change_offline() {
    let mut env = Env::without_tools("shipped-forge-controller");
    env.fake = env.root.join("fake-host");
    fs::write(
        &env.config,
        include_str!("../../../examples/fleet/fleet.toml"),
    )
    .unwrap();
    let forge = FakeForge::open("forge.example", &env.root.join("fake-forge")).unwrap();
    forge
        .set_change(PROJECT, "7", draft(vec![note("n1", 10, "ann", "hello")]))
        .unwrap();
    let cr = derived_name("fake", "forge.example", PROJECT, "7");
    let mut resources: Value =
        serde_json::from_str(include_str!("../../../examples/fleet/resources.json")).unwrap();
    resources[0]["spec"]["dispatch"] = json!(true);
    env.apply(resources).ok();
    env.reconcile().ok();
    assert_eq!(env.status("example")["phase"], "Briefed");
    declare(&env, "example", "https://forge.example/acme/widgets/pull/7")
        .ok()
        .stdout_has(&format!("ChangeRequest/{cr} (created; declared)"));
    forge
        .set_change(
            PROJECT,
            "7",
            draft(vec![
                note("n1", 10, "ann", "hello"),
                note("n2", 20, "bob", "a note"),
            ]),
        )
        .unwrap();
    env.reconcile().ok();
    let st = full_status(&env, "example");
    assert_eq!(entry(&st, &cr)["cursor"], 1, "{st:#}");
    let receipts = delivered(&st);
    assert_eq!(receipts.len(), 1, "{st:#}");
    assert_eq!(receipts[0]["from"], format!("forge/{cr}"));
    let got = texts(&env, "example");
    assert!(got[1].contains("> a note"), "{}", got[1]);
    env.assert_no_temp();
}
