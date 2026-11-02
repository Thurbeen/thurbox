//! Mark-ready end to end: a declared draft change leaves draft through the
//! real `fleet` binary, the file store, the fake host and the fake forge,
//! under a configured ready policy, once and only once; every refusal,
//! unknown outcome and stale observation the design names; and the same
//! effect through the stub `gh`, `glab` and `az`, after a fresh read.

mod support;

use std::fs;

use fleet_api::follow::worker_mark;
use fleet_api::forge::{
    ChangeSnapshot, ChangeState, Checks, HeadSeen, Mergeable, Note, NoteKind, ReviewDecision,
    Threads,
};
use fleet_forge::{FakeForge, ForgeFault};
use serde_json::{Value, json};
use support::{
    AZURE_DEVOPS_HOST, BITBUCKET_HOST, Env, FORGE_HOST, GITHUB_HOST, GITLAB_HOST, assert_condition,
    derived_name, now_ms, task,
};

const PROJECT: &str = "acme/widgets";
const URL: &str = "https://forge.test/acme/widgets/pull/7";
const URL_8: &str = "https://forge.test/acme/widgets/pull/8";
const LOGIN: &str = "fleet-bot";
const HEAD: &str = "aaa111";

/// The three shipped policies as `fleet init` writes them, and one that
/// waits on the forge's own answer.
const POLICIES: &str = "[forge.readyPolicies.checks]\nchecksPassed = true\n\n[forge.readyPolicies.checks-and-bots]\nbotReviewsAddressed = true\nchecksPassed = true\n\n[forge.readyPolicies.strict]\nbotReviewsAddressed = true\nchecksPassed = true\nnoUnresolvedThreads = true\n\n[forge.readyPolicies.merge]\nforgeMergeable = true\n";

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

fn bot_inline(id: &str, at: u64, thread: &str, resolved: bool) -> Note {
    Note {
        kind: NoteKind::Inline,
        bot: true,
        thread: Some(thread.into()),
        resolved: Some(resolved),
        path: Some("src/lib.rs".into()),
        line: Some(12),
        ..note(id, at, "linter[bot]", "unused import")
    }
}

/// A draft on head `aaa111` with the given checks and one person's note.
fn draft(checks: Checks) -> ChangeSnapshot {
    change(
        ChangeState::Draft,
        HEAD,
        checks,
        vec![note("n1", 10, "ann", "first look")],
    )
}

fn change(state: ChangeState, head: &str, checks: Checks, notes: Vec<Note>) -> ChangeSnapshot {
    ChangeSnapshot {
        state,
        head: HeadSeen {
            id: head.into(),
            checks,
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

/// Give the forge environment a default policy, extra `[forge]` keys and
/// the policies above.
fn configure(env: &Env, policy: &str, extra: &str) {
    let cfg = fs::read_to_string(&env.config).unwrap();
    let cfg = cfg.replace(
        "[forge]\n",
        &format!("[forge]\ndraftPolicy = \"{policy}\"\n{extra}"),
    );
    fs::write(&env.config, format!("{cfg}\n{POLICIES}")).unwrap();
}

/// A briefed Task `t1` following change 7 on the fake forge, Fleet logged
/// in as `fleet-bot`, under `policy`.
fn setup(name: &str, policy: &str, snapshot: ChangeSnapshot) -> (Env, FakeForge, String) {
    setup_task(name, policy, snapshot, task("t1", true))
}

fn setup_task(
    name: &str,
    policy: &str,
    snapshot: ChangeSnapshot,
    t1: Value,
) -> (Env, FakeForge, String) {
    let mut env = Env::forge(name);
    env.set_var("USER", "tester");
    configure(&env, policy, "");
    let forge = env.fake_forge();
    forge.update(|s| s.login = Some(LOGIN.into())).unwrap();
    forge.set_change(PROJECT, "7", snapshot).unwrap();
    env.apply(t1).ok();
    env.reconcile().ok();
    assert_eq!(env.status("t1")["phase"], "Briefed");
    let cr = derived_name("fake", FORGE_HOST, PROJECT, "7");
    declare(&env, "t1", URL).ok();
    let report = env.reconcile().ok().json();
    assert_eq!(report["errors"], json!([]), "{report:#}");
    assert_eq!(entry(&env, "t1", &cr)["cursor"], 1, "joined");
    (env, forge, cr)
}

fn declare(env: &Env, task: &str, url: &str) -> support::Run {
    env.fleet(&["declare", task, "--change-request", url])
}

/// The worker reports `shipped`; the Task concludes on the next pass.
fn ship(env: &Env, task: &str) {
    env.fleet(&[
        "result",
        task,
        "--generation",
        "1",
        "--outcome",
        "shipped",
        "--output",
        "prUrl=\"https://example.test/pr/1\"",
    ])
    .ok();
    env.reconcile().ok();
    assert_eq!(env.status(task)["phase"], "Concluded");
}

fn rest(env: &Env, task: &str, at_rest: bool) {
    env.host()
        .set_at_rest(&env.session_key(task), at_rest)
        .unwrap();
}

fn entry(env: &Env, task: &str, cr: &str) -> Value {
    env.get(task)["status"]["forge"]["changes"][cr].clone()
}

fn record(env: &Env, task: &str, cr: &str) -> Value {
    entry(env, task, cr)["markReady"].clone()
}

fn forge_status(env: &Env, task: &str) -> Value {
    env.get(task)["status"]["forge"].clone()
}

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

/// Let an entry's own wait after a failed mark-ready run out now.
fn expire_entry_wait(env: &Env, task: &str, cr: &str) {
    let path = env.object_path("Task", task);
    let mut raw: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    if raw["status"]["forge"]["changes"][cr]["notBefore"].is_u64() {
        raw["status"]["forge"]["changes"][cr]["notBefore"] = json!(0);
        fs::write(&path, raw.to_string()).unwrap();
    }
}

fn ready_key(env: &Env, task: &str, cr: &str, head: &str) -> String {
    let uid = env.get(task)["metadata"]["uid"]
        .as_str()
        .unwrap()
        .to_string();
    let cr_uid = env.change_request(cr)["metadata"]["uid"]
        .as_str()
        .unwrap()
        .to_string();
    format!("task/{task}/{uid}/ready/{cr_uid}/{head}")
}

#[test]
fn a_draft_passes_checks_once_the_worker_shipped_and_rests_and_is_marked_ready_once() {
    let (env, forge, cr) = setup("checks", "checks", draft(Checks::Pending));
    // Not shipped: nothing is evaluated, so the session is not even asked.
    rest(&env, "t1", true);
    env.reconcile().ok();
    assert_eq!(env.calls("atRest"), 0);
    assert_eq!(record(&env, "t1", &cr)["state"], "none");
    rest(&env, "t1", false);
    ship(&env, "t1");
    // Shipped, but checks still pending: not asked either, since the
    // session is asked last, once the snapshot's predicates hold.
    assert_eq!(env.calls("atRest"), 0);
    assert_eq!(record(&env, "t1", &cr)["state"], "none");
    // Checks pass; the session is mid-turn: asked, and refused.
    forge
        .set_change(PROJECT, "7", draft(Checks::Passed))
        .unwrap();
    poll(&env, &cr);
    assert!(
        env.calls("atRest") > 0,
        "workerIdle is asked once everything else holds"
    );
    assert_eq!(record(&env, "t1", &cr)["state"], "none");
    assert!(forge.marked_ready().unwrap().is_empty());
    assert!(
        texts(&env, "t1")
            .last()
            .unwrap()
            .contains("checks `passed`"),
        "the relay went on meanwhile"
    );

    // At rest: one intent, one effect, one receipt, done, in one run.
    rest(&env, "t1", true);
    let report = env.reconcile().ok().json();
    assert_eq!(report["errors"], json!([]), "{report:#}");
    assert_eq!(report["effects"], 1, "{report:#}");
    let key = ready_key(&env, "t1", &cr, HEAD);
    let r = record(&env, "t1", &cr);
    assert_eq!(r["state"], "done", "{r:#}");
    assert_eq!(r["head"], HEAD);
    assert_eq!(r["attempts"], 1);
    assert!(
        r["intents"].as_object().is_none_or(|m| m.is_empty()),
        "answered: {r:#}"
    );
    assert_eq!(r["receipts"][&key]["action"], "markReady");
    assert_eq!(
        r["receipts"][&key]["outcome"],
        json!({"known": {"type": "markedReady", "already": false}}),
        "{r:#}"
    );
    assert_eq!(forge.marked_ready().unwrap(), vec![key.clone()]);
    assert_eq!(
        forge.change(PROJECT, "7").unwrap().unwrap().state,
        ChangeState::Open
    );
    env.fleet(&["status", "task", "t1"])
        .ok()
        .stdout_has(&format!(
            "following {cr} following cursor 2 gaps 0 ready done"
        ));
    assert_eq!(
        env.status("t1")["forge"]["changes"][&cr]["markReady"]["state"],
        "done"
    );

    // The mirror sees it ready; the worker is told; nothing is repeated.
    poll(&env, &cr);
    let cr_st = env.change_request_status(&cr);
    assert_eq!(cr_st["state"]["value"], "open");
    assert_eq!(cr_st["everReady"], true);
    assert!(
        texts(&env, "t1")
            .last()
            .unwrap()
            .contains("state changed to `open`"),
        "{:?}",
        texts(&env, "t1")
    );
    for _ in 0..2 {
        poll(&env, &cr);
    }
    assert_eq!(forge.calls("markReady").unwrap(), 1, "once and only once");
    let again = env.reconcile().ok().json();
    assert_eq!(again["idle"], true, "{again:#}");
    env.assert_no_temp();
}

#[test]
fn a_change_opened_ready_is_never_marked_and_neither_is_one_put_back_into_draft() {
    let (env, forge, cr) = setup(
        "ever-ready",
        "checks",
        change(
            ChangeState::Open,
            HEAD,
            Checks::Passed,
            vec![note("n1", 10, "ann", "hi")],
        ),
    );
    assert_eq!(env.change_request_status(&cr)["everReady"], true);
    ship(&env, "t1");
    rest(&env, "t1", true);
    env.reconcile().ok();
    assert_eq!(env.calls("atRest"), 0, "nothing to evaluate, nothing asked");
    assert_eq!(forge.calls("markReady").unwrap(), 0);

    // A person puts it back into draft: everReady protects it, for this
    // Task and for a second one that declares it afterwards.
    forge
        .set_change(PROJECT, "7", draft(Checks::Passed))
        .unwrap();
    poll(&env, &cr);
    let cr_st = env.change_request_status(&cr);
    assert_eq!(cr_st["state"]["value"], "draft");
    assert_eq!(cr_st["everReady"], true);
    env.reconcile().ok();
    assert_eq!(record(&env, "t1", &cr)["state"], "none");
    env.apply(task("t2", true)).ok();
    env.reconcile().ok();
    declare(&env, "t2", URL)
        .ok()
        .stdout_has("(existing; declared)");
    env.reconcile().ok();
    ship(&env, "t2");
    rest(&env, "t2", true);
    env.reconcile().ok();
    assert_eq!(record(&env, "t2", &cr)["state"], "none");
    assert_eq!(forge.calls("markReady").unwrap(), 0);
    assert_eq!(
        forge.change(PROJECT, "7").unwrap().unwrap().state,
        ChangeState::Draft
    );
}

#[test]
fn a_moved_head_is_refused_without_counting_an_attempt_and_the_new_head_is_marked() {
    let (env, forge, cr) = setup("moved", "checks", draft(Checks::Passed));
    ship(&env, "t1");
    // The worker pushes again; the mirror has not polled yet.
    forge
        .set_change(
            PROJECT,
            "7",
            change(
                ChangeState::Draft,
                "bbb222",
                Checks::Passed,
                vec![note("n1", 10, "ann", "first look")],
            ),
        )
        .unwrap();
    rest(&env, "t1", true);
    let report = env.reconcile().ok().json();
    assert_eq!(report["effects"], 1, "{report:#}");
    let old_key = ready_key(&env, "t1", &cr, HEAD);
    let r = record(&env, "t1", &cr);
    assert_eq!(r["state"], "none", "{r:#}");
    assert_eq!(r["attempts"], 0, "not an attempt against the change");
    assert_eq!(
        r["receipts"][&old_key]["outcome"],
        json!({"known": {"type": "moved", "head": "bbb222", "state": "draft"}})
    );
    assert!(forge.marked_ready().unwrap().is_empty(), "nothing mutated");
    // The mirror still shows the old head: not tried again against it.
    env.reconcile().ok();
    assert_eq!(forge.calls("markReady").unwrap(), 1);
    // The mirror catches up: the new head is evaluated afresh and marked.
    poll(&env, &cr);
    let r = record(&env, "t1", &cr);
    assert_eq!(r["state"], "done", "{r:#}");
    assert_eq!(r["head"], "bbb222");
    assert_eq!(r["attempts"], 1);
    assert_eq!(
        forge.marked_ready().unwrap(),
        vec![ready_key(&env, "t1", &cr, "bbb222")]
    );
}

#[test]
fn a_known_failure_retries_after_the_wait_on_that_entry_while_another_keeps_relaying() {
    let (env, forge, cr) = setup("failed-retry", "checks", draft(Checks::Passed));
    forge
        .set_change(
            PROJECT,
            "8",
            change(
                ChangeState::Draft,
                "ccc333",
                Checks::Pending,
                vec![note("m1", 10, "bob", "eight")],
            ),
        )
        .unwrap();
    declare(&env, "t1", URL_8).ok();
    env.reconcile().ok();
    let cr8 = derived_name("fake", FORGE_HOST, PROJECT, "8");
    assert_eq!(entry(&env, "t1", &cr8)["cursor"], 1);
    forge
        .add_fault(ForgeFault::new("markReady", "failed"))
        .unwrap();
    ship(&env, "t1");
    rest(&env, "t1", true);
    let t = now_ms();
    env.reconcile().ok();
    let e = entry(&env, "t1", &cr);
    assert_eq!(e["markReady"]["state"], "failed", "{e:#}");
    assert_eq!(e["markReady"]["attempts"], 1);
    let nb = e["notBefore"].as_u64().unwrap();
    assert!(nb >= t + 30_000 && nb < t + 30_000 + 60_000, "{e:#}");
    assert!(
        e["markReady"]["receipts"][ready_key(&env, "t1", &cr, HEAD)]["outcome"]["known"]["why"]
            .as_str()
            .unwrap()
            .contains("refused")
    );
    assert!(
        entry(&env, "t1", &cr8).get("notBefore").is_none(),
        "the other entry does not wait"
    );
    assert!(
        forge_status(&env, "t1")["conditions"]
            .as_array()
            .unwrap()
            .iter()
            .all(|c| c["type"] != "NeedsAttention"),
        "one failure is not attention"
    );

    // Change 8 moves on and is relayed while 7 waits.
    forge
        .set_change(
            PROJECT,
            "8",
            change(
                ChangeState::Draft,
                "ccc333",
                Checks::Pending,
                vec![
                    note("m1", 10, "bob", "eight"),
                    note("m2", 20, "bob", "nine"),
                ],
            ),
        )
        .unwrap();
    poll(&env, &cr8);
    assert!(texts(&env, "t1").last().unwrap().contains("> nine"));
    assert_eq!(record(&env, "t1", &cr)["state"], "failed", "still waiting");
    assert_eq!(forge.calls("markReady").unwrap(), 1);

    // The wait over: the second attempt succeeds, once the worker, busy
    // with the relayed note, is at rest again.
    expire_entry_wait(&env, "t1", &cr);
    env.reconcile().ok();
    assert_eq!(
        record(&env, "t1", &cr)["state"],
        "failed",
        "the session is mid-turn"
    );
    rest(&env, "t1", true);
    env.reconcile().ok();
    let r = record(&env, "t1", &cr);
    assert_eq!(r["state"], "done", "{r:#}");
    assert_eq!(r["attempts"], 2);
    assert_eq!(forge.marked_ready().unwrap().len(), 1);
    assert_eq!(forge.calls("markReady").unwrap(), 2);
}

#[test]
fn an_unknown_outcome_is_answered_by_the_next_fresh_revision_never_by_repeating() {
    // The answer was lost after the mutation landed.
    let (env, forge, cr) = setup("unknown-applied", "checks", draft(Checks::Passed));
    forge
        .add_fault(ForgeFault::new("markReady", "lost-applied"))
        .unwrap();
    ship(&env, "t1");
    rest(&env, "t1", true);
    env.reconcile().ok();
    let r = record(&env, "t1", &cr);
    assert_eq!(r["state"], "pending", "{r:#}");
    let key = ready_key(&env, "t1", &cr, HEAD);
    assert!(
        r["receipts"][&key]["outcome"]["unknown"].is_object(),
        "{r:#}"
    );
    assert!(r["intents"][&key].is_object(), "the question stays open");
    assert_eq!(forge.marked_ready().unwrap().len(), 1);
    for _ in 0..2 {
        env.reconcile().ok();
    }
    assert_eq!(record(&env, "t1", &cr)["state"], "pending");
    assert_eq!(
        forge.calls("markReady").unwrap(),
        1,
        "never repeated blindly"
    );
    poll(&env, &cr);
    let r = record(&env, "t1", &cr);
    assert_eq!(
        r["state"], "done",
        "everReady is a recovered receipt: {r:#}"
    );
    assert_eq!(r["attempts"], 1);
    assert_eq!(forge.calls("markReady").unwrap(), 1);

    // The answer was lost and the mutation did not land.
    let (env, forge, cr) = setup("unknown-lost", "checks", draft(Checks::Passed));
    forge
        .add_fault(ForgeFault::new("markReady", "lost"))
        .unwrap();
    ship(&env, "t1");
    rest(&env, "t1", true);
    env.reconcile().ok();
    assert_eq!(record(&env, "t1", &cr)["state"], "pending");
    poll(&env, &cr);
    let r = record(&env, "t1", &cr);
    assert_eq!(r["state"], "failed", "still a draft on a fresh read: {r:#}");
    assert_eq!(r["attempts"], 1);
    assert_eq!(forge.calls("markReady").unwrap(), 1);
    expire_entry_wait(&env, "t1", &cr);
    env.reconcile().ok();
    assert_eq!(record(&env, "t1", &cr)["state"], "done");
    assert_eq!(forge.marked_ready().unwrap().len(), 1);
}

#[test]
fn three_failures_raise_mark_ready_failed_and_a_fourth_is_never_tried() {
    let (env, forge, cr) = setup("three-failures", "checks", draft(Checks::Passed));
    forge
        .add_fault(ForgeFault::new("markReady", "failed").times(3))
        .unwrap();
    ship(&env, "t1");
    rest(&env, "t1", true);
    for attempt in 1..=3 {
        env.reconcile().ok();
        let r = record(&env, "t1", &cr);
        assert_eq!(r["state"], "failed", "{r:#}");
        assert_eq!(r["attempts"], attempt);
        expire_entry_wait(&env, "t1", &cr);
    }
    let st = forge_status(&env, "t1");
    assert_condition(&st, "NeedsAttention", "True", "MarkReadyFailed");
    let message = st["conditions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["type"] == "NeedsAttention")
        .unwrap()["message"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(message.contains(&cr), "{message}");
    assert!(message.contains("3 attempt(s)"), "{message}");
    env.fleet(&["status", "task", "t1"])
        .ok()
        .stdout_has("MarkReadyFailed");
    env.reconcile().ok();
    assert_eq!(forge.calls("markReady").unwrap(), 3, "bounded at three");
    assert!(forge.marked_ready().unwrap().is_empty());
    assert_eq!(
        forge.change(PROJECT, "7").unwrap().unwrap().state,
        ChangeState::Draft
    );
}

#[test]
fn a_stale_mirror_never_marks() {
    let (env, forge, cr) = setup("stale", "checks", draft(Checks::Passed));
    ship(&env, "t1");
    forge.add_fault(ForgeFault::new("change", "lost")).unwrap();
    poll(&env, &cr);
    assert_condition(
        &env.change_request_status(&cr),
        "Reachable",
        "Unknown",
        "Refused",
    );
    rest(&env, "t1", true);
    env.reconcile().ok();
    assert_eq!(record(&env, "t1", &cr)["state"], "none");
    assert_eq!(forge.calls("markReady").unwrap(), 0);
    // Reachable again: marked.
    poll(&env, &cr);
    assert_condition(
        &env.change_request_status(&cr),
        "Reachable",
        "True",
        "Observed",
    );
    assert_eq!(record(&env, "t1", &cr)["state"], "done");
    assert_eq!(forge.calls("markReady").unwrap(), 1);
}

#[test]
fn checks_and_bots_waits_on_a_bots_unresolved_thread_and_relays_it_even_with_bots_off() {
    let blocked = change(
        ChangeState::Draft,
        HEAD,
        Checks::Passed,
        vec![
            note("n1", 10, "ann", "first look"),
            bot_inline("b1", 20, "t1", false),
        ],
    );
    let (env, forge, cr) = setup("checks-and-bots", "checks-and-bots", blocked.clone());
    assert!(
        texts(&env, "t1")[1].contains("linter[bot] (inline, bot, src/lib.rs:12, thread t1)"),
        "the blocker is in the join although relayBots is false: {}",
        texts(&env, "t1")[1]
    );
    ship(&env, "t1");
    rest(&env, "t1", true);
    env.reconcile().ok();
    assert_eq!(record(&env, "t1", &cr)["state"], "none");
    assert_eq!(forge.calls("markReady").unwrap(), 0);
    // A bot's status comment is not a blocker and is not relayed.
    let mut with_status = blocked.clone();
    let mut status = note("b2", 30, "coverage[bot]", "coverage 93%");
    status.bot = true;
    with_status.notes.push(status);
    forge.set_change(PROJECT, "7", with_status).unwrap();
    let before = texts(&env, "t1").len();
    poll(&env, &cr);
    assert_eq!(texts(&env, "t1").len(), before, "status, not review");
    assert_eq!(
        entry(&env, "t1", &cr)["cursor"],
        2,
        "the cursor still advanced"
    );
    assert_eq!(forge.calls("markReady").unwrap(), 0);
    // The worker answers in the thread with its mark: addressed, and marked.
    let uid = env.get("t1")["metadata"]["uid"]
        .as_str()
        .unwrap()
        .to_string();
    let mut answered = blocked;
    let mut reply = bot_inline("r1", 40, "t1", false);
    reply.bot = false;
    reply.author = LOGIN.into();
    reply.body = format!("Removed.\n{}", worker_mark(&uid));
    answered.notes.push(reply);
    forge.set_change(PROJECT, "7", answered).unwrap();
    poll(&env, &cr);
    assert_eq!(record(&env, "t1", &cr)["state"], "done");
    assert_eq!(forge.calls("markReady").unwrap(), 1);
}

#[test]
fn the_no_check_grace_passes_a_checkless_head_only_after_it() {
    let (env, forge, cr) = setup("no-check-grace", "checks", draft(Checks::None));
    let cfg = fs::read_to_string(&env.config).unwrap();
    fs::write(
        &env.config,
        cfg.replace("[forge]\n", "[forge]\nnoCheckGraceMs = 600000\n"),
    )
    .unwrap();
    ship(&env, "t1");
    rest(&env, "t1", true);
    env.reconcile().ok();
    assert_eq!(record(&env, "t1", &cr)["state"], "none", "within the grace");
    assert_eq!(forge.calls("markReady").unwrap(), 0);
    // The head was first seen at the join, longer than one millisecond ago.
    fs::write(
        &env.config,
        cfg.replace("[forge]\n", "[forge]\nnoCheckGraceMs = 1\n"),
    )
    .unwrap();
    env.reconcile().ok();
    assert_eq!(record(&env, "t1", &cr)["state"], "done");
    assert_eq!(forge.calls("markReady").unwrap(), 1);
}

#[test]
fn a_policy_removed_from_the_config_raises_policy_unknown_and_an_undefined_one_is_refused_at_apply()
{
    let mut t1 = task("t1", true);
    t1["spec"]["forge"] = json!({"draft": {"policy": "strict"}});
    let (env, forge, cr) = setup_task("policy-unknown", "checks", draft(Checks::Passed), t1);
    let mut undefined = task("t3", false);
    undefined["spec"]["forge"] = json!({"draft": {"policy": "careful"}});
    env.apply(undefined).code(1).stderr_has(
        "spec.forge.draft.policy \"careful\" is not defined under [forge.readyPolicies]",
    );
    ship(&env, "t1");
    rest(&env, "t1", true);
    let cfg = fs::read_to_string(&env.config).unwrap();
    let without_strict = cfg.replace(
        "[forge.readyPolicies.strict]\nbotReviewsAddressed = true\nchecksPassed = true\nnoUnresolvedThreads = true\n",
        "",
    );
    assert_ne!(without_strict, cfg);
    fs::write(&env.config, &without_strict).unwrap();
    env.reconcile().ok();
    assert_condition(
        &forge_status(&env, "t1"),
        "NeedsAttention",
        "True",
        "PolicyUnknown",
    );
    assert_eq!(record(&env, "t1", &cr)["state"], "none");
    assert_eq!(forge.calls("markReady").unwrap(), 0);
    env.fleet(&["status", "task", "t1"])
        .ok()
        .stdout_has("PolicyUnknown");
    fs::write(&env.config, cfg).unwrap();
    env.reconcile().ok();
    assert_condition(
        &forge_status(&env, "t1"),
        "NeedsAttention",
        "False",
        "Resolved",
    );
    assert_eq!(record(&env, "t1", &cr)["state"], "done");
}

#[test]
fn forge_mergeable_gates_on_the_forges_answer() {
    let (env, forge, cr) = setup("mergeable", "merge", draft(Checks::Pending));
    ship(&env, "t1");
    rest(&env, "t1", true);
    env.reconcile().ok();
    assert_eq!(
        record(&env, "t1", &cr)["state"],
        "none",
        "the forge says no (draft)"
    );
    let mut yes = draft(Checks::Pending);
    yes.mergeable = Mergeable {
        value: Some(true),
        reason: None,
    };
    forge.set_change(PROJECT, "7", yes).unwrap();
    poll(&env, &cr);
    assert_eq!(
        record(&env, "t1", &cr)["state"],
        "done",
        "checks are not this policy's"
    );
    assert_eq!(forge.calls("markReady").unwrap(), 1);
}

#[test]
fn the_shipped_example_marks_a_declared_draft_ready_offline() {
    let mut env = Env::without_tools("shipped-mark-ready");
    env.fake = env.root.join("fake-host");
    fs::write(&env.config, include_str!("../../../examples/fleet.toml")).unwrap();
    let forge = FakeForge::open("forge.example", &env.root.join("fake-forge")).unwrap();
    forge
        .set_change(PROJECT, "7", draft(Checks::Pending))
        .unwrap();
    let cr = derived_name("fake", "forge.example", PROJECT, "7");
    let mut resources: Value =
        serde_json::from_str(include_str!("../../../examples/resources.json")).unwrap();
    resources[0]["spec"]["dispatch"] = json!(true);
    env.apply(resources).ok();
    env.reconcile().ok();
    declare(&env, "example", "https://forge.example/acme/widgets/pull/7").ok();
    env.reconcile().ok();
    env.fleet(&[
        "result",
        "example",
        "--generation",
        "1",
        "--outcome",
        "shipped",
        "--summary",
        "done",
    ])
    .ok();
    env.reconcile().ok();
    rest(&env, "example", true);
    forge
        .set_change(PROJECT, "7", draft(Checks::Passed))
        .unwrap();
    poll(&env, &cr);
    let st = env.status("example");
    assert_eq!(
        st["forge"]["changes"][&cr]["markReady"]["state"], "done",
        "{st:#}"
    );
    assert_eq!(forge.marked_ready().unwrap().len(), 1);
    assert_eq!(
        forge.change(PROJECT, "7").unwrap().unwrap().state,
        ChangeState::Open
    );
    env.assert_no_temp();
}

/// A draft in the stub forges' model, checks green.
fn stub_draft(head: &str) -> Value {
    json!({
        "state": "draft", "head": head, "title": "Draft: add it",
        "checks": [{"name": "ci", "state": "success", "url": "https://ci.test/1"}],
        "reviewDecision": null, "mergeable": "draft",
        "reviews": [], "comments": [{"id": "c1", "author": "ann", "body": "one", "at": 2_000, "editedAt": null}], "threads": []
    })
}

#[test]
fn github_and_gitlab_drafts_are_marked_ready_through_gh_and_glab_after_a_fresh_read() {
    let mut env = Env::forge_stubs("stub-mark-ready");
    env.set_var("USER", "tester");
    configure(&env, "checks", "");
    env.edit_stub(|s| {
        s["login"] = json!(LOGIN);
        s["changes"] = json!({
            "acme/widgets#7": stub_draft("h1"),
            "acme/widgets#3": stub_draft("g1"),
        });
    });
    env.apply(task("t1", true)).ok();
    env.reconcile().ok();
    let gh_url = format!("https://{GITHUB_HOST}/acme/widgets/pull/7");
    let gl_url = format!("https://{GITLAB_HOST}/acme/widgets/-/merge_requests/3");
    declare(&env, "t1", &gh_url).ok();
    declare(&env, "t1", &gl_url).ok();
    env.reconcile().ok();
    let gh_cr = derived_name("github", GITHUB_HOST, PROJECT, "7");
    let gl_cr = derived_name("gitlab", GITLAB_HOST, PROJECT, "3");
    assert_eq!(entry(&env, "t1", &gh_cr)["cursor"], 1);
    assert_eq!(entry(&env, "t1", &gl_cr)["cursor"], 1);
    ship(&env, "t1");
    // The GitHub head moves before the mirror sees it: the fresh read
    // refuses, nothing is mutated; GitLab's goes through.
    env.edit_stub(|s| s["changes"]["acme/widgets#7"]["head"] = json!("h2"));
    rest(&env, "t1", true);
    let report = env.reconcile().ok().json();
    assert_eq!(report["errors"], json!([]), "{report:#}");
    let gh = record(&env, "t1", &gh_cr);
    assert_eq!(gh["state"], "none", "{gh:#}");
    assert_eq!(
        gh["receipts"][ready_key(&env, "t1", &gh_cr, "h1")]["outcome"],
        json!({"known": {"type": "moved", "head": "h2", "state": "draft"}})
    );
    let gl = record(&env, "t1", &gl_cr);
    assert_eq!(gl["state"], "done", "{gl:#}");
    let stub = env.stub();
    assert_eq!(stub["changes"]["acme/widgets#7"]["state"], "draft");
    assert_eq!(stub["changes"]["acme/widgets#3"]["state"], "open");
    assert_eq!(stub["changes"]["acme/widgets#3"]["title"], "add it");
    let gh_calls = env.forge_calls(GITHUB_HOST);
    assert!(
        !gh_calls.iter().any(|c| c["argv"]
            .to_string()
            .contains("markPullRequestReadyForReview")),
        "no mutation after a refused fresh read: {gh_calls:#?}"
    );
    let gl_calls = env.forge_calls(GITLAB_HOST);
    let puts: Vec<&Value> = gl_calls
        .iter()
        .filter(|c| c["argv"].as_array().unwrap().contains(&json!("PUT")))
        .collect();
    assert_eq!(puts.len(), 1, "{gl_calls:#?}");
    let argv = puts[0]["argv"].as_array().unwrap();
    assert!(argv.contains(&json!("title=add it")), "{argv:?}");
    assert_eq!(
        argv.last().unwrap(),
        &json!("projects/acme%2Fwidgets/merge_requests/3")
    );
    assert!(
        !puts[0]["envNames"]
            .as_array()
            .unwrap()
            .iter()
            .any(|n| n.as_str().unwrap().contains("TOKEN")),
        "no credential is passed"
    );

    // The GitHub mirror catches up: the mutation runs under the intent key.
    poll(&env, &gh_cr);
    let gh = record(&env, "t1", &gh_cr);
    assert_eq!(gh["state"], "done", "{gh:#}");
    let key = ready_key(&env, "t1", &gh_cr, "h2");
    let stub = env.stub();
    assert_eq!(stub["changes"]["acme/widgets#7"]["state"], "open");
    assert_eq!(
        stub["changes"]["acme/widgets#7"]["readyIntents"],
        json!([key])
    );
    let gh_calls = env.forge_calls(GITHUB_HOST);
    let mutations: Vec<&Value> = gh_calls
        .iter()
        .filter(|c| {
            c["argv"]
                .to_string()
                .contains("markPullRequestReadyForReview")
        })
        .collect();
    assert_eq!(mutations.len(), 1, "{gh_calls:#?}");
    let argv = mutations[0]["argv"].as_array().unwrap();
    assert!(argv.contains(&json!(format!("intent={key}"))), "{argv:?}");
    assert!(argv.contains(&json!("id=PR_acme/widgets#7")), "{argv:?}");
    let position = |needle: &str| {
        gh_calls
            .iter()
            .rposition(|c| c["argv"].to_string().contains(needle))
            .unwrap()
    };
    assert!(
        position("{ id isDraft state headRefOid }") < position("markPullRequestReadyForReview"),
        "the fresh read comes right before the mutation: {gh_calls:#?}"
    );
}

#[test]
fn azure_devops_drafts_fail_closed_without_an_atomic_head_and_state_precondition() {
    let mut env = Env::forge_stubs("azure-ready");
    env.set_var("USER", "tester");
    configure(&env, "checks", "");
    env.edit_stub(|s| {
        s["login"] = json!(LOGIN);
        s["changes"] = json!({"acme/project/widgets#9": stub_draft("a1")});
    });
    env.apply(task("t1", true)).ok();
    env.reconcile().ok();
    let url = "https://dev.azure.com/acme/project/_git/widgets/pullrequest/9";
    declare(&env, "t1", url).ok();
    env.reconcile().ok();
    let cr = derived_name(
        "azure-devops",
        "acme.visualstudio.com",
        "project/widgets",
        "9",
    );
    assert_eq!(entry(&env, "t1", &cr)["cursor"], 1);
    ship(&env, "t1");
    rest(&env, "t1", true);
    let report = env.reconcile().ok().json();
    assert_eq!(report["errors"], json!([]), "{report:#}");
    assert_eq!(record(&env, "t1", &cr)["state"], "failed");
    assert_eq!(
        env.stub()["changes"]["acme/project/widgets#9"]["state"],
        "draft"
    );

    let calls = env.forge_calls(AZURE_DEVOPS_HOST);
    assert!(
        calls
            .iter()
            .all(|c| !c["argv"].as_array().unwrap().starts_with(&[
                json!("repos"),
                json!("pr"),
                json!("update"),
            ])),
        "mark-ready must not use a check-then-update race: {calls:#?}"
    );
    assert!(
        calls.iter().any(|c| {
            let argv = c["argv"].as_array().unwrap();
            argv.contains(&json!("profiles")) && argv.contains(&json!("https://dev.azure.com/acme"))
        }),
        "whoami is read in the change's organization: {calls:#?}"
    );
}

#[test]
fn bitbucket_cloud_drafts_fail_closed_without_an_atomic_head_and_state_precondition() {
    let mut env = Env::forge_stubs("bitbucket-ready");
    env.set_var("USER", "tester");
    configure(&env, "checks", "");
    env.edit_stub(|s| {
        s["login"] = json!(LOGIN);
        s["changes"] = json!({"acme/widgets#9": stub_draft("b1")});
    });
    env.apply(task("t1", true)).ok();
    env.reconcile().ok();
    let url = "https://bitbucket.org/acme/widgets/pull-requests/9";
    declare(&env, "t1", url).ok();
    env.reconcile().ok();
    let cr = derived_name("bitbucket-cloud", BITBUCKET_HOST, PROJECT, "9");
    assert_eq!(entry(&env, "t1", &cr)["cursor"], 1);
    ship(&env, "t1");
    rest(&env, "t1", true);
    let report = env.reconcile().ok().json();
    assert_eq!(report["errors"], json!([]), "{report:#}");
    assert_eq!(record(&env, "t1", &cr)["state"], "failed");
    assert_eq!(env.stub()["changes"]["acme/widgets#9"]["state"], "draft");

    let calls = env.forge_calls(BITBUCKET_HOST);
    assert!(
        calls
            .iter()
            .any(|call| call["argv"].to_string().contains("/2.0/user")),
        "whoami uses the stable account identity from the authenticated Cloud user: {calls:#?}"
    );
    assert!(
        calls
            .iter()
            .all(|call| !call["argv"].as_array().unwrap().contains(&json!("PUT"))),
        "mark-ready must not use a check-then-update race: {calls:#?}"
    );
}

#[test]
fn a_shipped_result_an_amendment_superseded_never_marks_ready() {
    let (env, forge, cr) = setup("amended", "checks", draft(Checks::Passed));
    ship(&env, "t1");
    env.fleet(&["amend", "t1", "--text", "Also cover the edge case."])
        .ok();
    env.reconcile().ok();
    assert_eq!(env.status("t1")["phase"], "Briefed", "re-opened");
    rest(&env, "t1", true);
    poll(&env, &cr);
    assert!(
        forge.marked_ready().unwrap().is_empty(),
        "the shipped result answers generation 1; the worker now has 2"
    );
    assert_eq!(record(&env, "t1", &cr)["state"], "none");

    env.fleet(&[
        "result",
        "t1",
        "--generation",
        "2",
        "--outcome",
        "shipped",
        "--output",
        "prUrl=\"https://example.test/pr/1\"",
    ])
    .ok();
    env.reconcile().ok();
    poll(&env, &cr);
    assert_eq!(record(&env, "t1", &cr)["state"], "done");
    assert_eq!(forge.marked_ready().unwrap().len(), 1);
}
