//! The thurbox runtime end to end: the real `fleet` binary, its file store,
//! TaskController, and the thurbox adapter driving a bounded stand-in for
//! `thurbox-cli` (`tests/stubs/stub.rs`). No real session or host is touched.

mod support;

use std::time::Duration;

use serde_json::{Value, json};
use support::{Env, alive_after, assert_condition, runtime_config};

/// A Task whose workspace is an absolute path, as thurbox requires.
fn task(name: &str, dispatch: bool) -> Value {
    let mut t = support::task(name, dispatch);
    let dir = if cfg!(windows) {
        format!("C:\\work\\{name}")
    } else {
        format!("/work/{name}")
    };
    t["spec"]["workspace"]["dir"] = json!(dir);
    t
}

fn setup(name: &str, extra: Vec<Value>) -> Env {
    let env = Env::thurbox(name);
    let mut items = runtime_config();
    items.as_array_mut().unwrap().extend(extra);
    env.apply(items).ok();
    env
}

/// The key the thurbox adapter gives a Task's session: the Task's uid under
/// the configured prefix (`fleet` by default). Short enough for thurbox's
/// 64-character keys whatever the Task's name.
fn thurbox_key(env: &Env, task: &str) -> String {
    let uid = env.get(task)["metadata"]["uid"]
        .as_str()
        .unwrap()
        .to_string();
    format!("fleet.{uid}")
}

fn result_args<'a>(task: &'a str, generation: &'a str) -> Vec<&'a str> {
    vec![
        "result",
        task,
        "--generation",
        generation,
        "--outcome",
        "shipped",
        "--output",
        "prUrl=\"https://example.invalid/pr/1\"",
    ]
}

fn set_session_state(env: &Env, key: &str, state: &str) {
    env.edit_stub(|s| {
        for sess in s["sessions"].as_array_mut().unwrap() {
            if sess["key"] == key {
                sess["state"] = json!(state);
            }
        }
    });
}

fn argv_of(env: &Env, sub: &str) -> Vec<Vec<String>> {
    env.stub_calls()
        .iter()
        .filter(|c| support::is_call(c, sub))
        .map(|c| {
            c["argv"]
                .as_array()
                .unwrap()
                .iter()
                .map(|a| a.as_str().unwrap().to_string())
                .collect()
        })
        .collect()
}

fn receipts(status: &Value) -> Vec<Value> {
    status["receipts"]
        .as_object()
        .map(|m| m.values().cloned().collect())
        .unwrap_or_default()
}

fn full_status(env: &Env, name: &str) -> Value {
    env.get(name)["status"].clone()
}

#[test]
fn a_task_lives_its_whole_life_on_thurbox_cli() {
    let env = setup("thurbox-life", vec![task("t1", true)]);
    env.reconcile().ok();

    let st = env.status("t1");
    assert_eq!(st["phase"], "Briefed", "{st:#}");
    let full = full_status(&env, "t1");
    assert_eq!(full["session"]["host"], "local", "{full:#}");

    let key = thurbox_key(&env, "t1");
    let sessions = env.stub_sessions();
    assert_eq!(sessions.len(), 1, "{sessions:#?}");
    assert_eq!(sessions[0]["key"], key.as_str());
    assert_eq!(sessions[0]["agent"], "claude-code");
    assert_eq!(sessions[0]["sent"], json!(["Do the work for t1."]));
    let create = &argv_of(&env, "create")[0];
    for want in ["--key", "--on-existing", "adopt", "--agent", "--json"] {
        assert!(create.iter().any(|a| a == want), "{want} in {create:?}");
    }
    assert!(
        !create.iter().any(|a| a == "--host"),
        "the local host is thurbox's own, not a --host: {create:?}"
    );

    env.fleet(&result_args("t1", "1")).ok();
    env.reconcile().ok();
    assert_eq!(env.status("t1")["phase"], "Concluded");

    // Releasing waits for the session to be at rest (P7), then deletes it.
    env.fleet(&["delete", "task", "t1"]).ok();
    env.reconcile().ok();
    assert_condition(&env.status("t1"), "Released", "False", "SessionBusy");
    assert_eq!(env.stub_sessions().len(), 1);
    set_session_state(&env, &key, "idle");
    env.reconcile().ok();
    assert!(env.stub_sessions().is_empty(), "{:#?}", env.stub_sessions());
    env.fleet(&["get", "task", "t1"]).code(3);
    env.assert_no_temp();
}

#[test]
fn a_host_that_cannot_be_read_is_unknown_for_its_own_tasks_only() {
    let mut on_remote = task("far", true);
    on_remote["spec"]["host"] = json!("remote");
    let env = setup("thurbox-unknown-host", vec![]);
    env.edit_stub(|s| s["hosts"] = json!({"remote": "up"}));
    env.apply(on_remote).ok();
    env.reconcile().ok();
    // thurbox 2.81 reaches no remote agent's inbox: it keeps the brief in its
    // mailbox. That is no delivery, and the brief is never sent again.
    let far = env.status("far");
    assert_eq!(far["phase"], "Spawned", "{far:#}");
    assert_condition(&far, "Briefed", "False", "BriefUndelivered");
    assert_condition(&far, "NeedsAttention", "True", "BriefUndelivered");
    for _ in 0..3 {
        env.expire_wait("far");
        env.reconcile().ok();
    }
    assert_eq!(argv_of(&env, "message send").len(), 1, "never sent again");
    assert_eq!(env.stub_sessions()[0]["sent"], json!([]), "nothing typed");
    let create = &argv_of(&env, "create")[0];
    let at = create.iter().position(|a| a == "--host").expect("--host");
    assert_eq!(create[at + 1], "remote");

    // The remote host stops answering. Its Task is Unknown: no WorkerGone
    // clock, no respawn. A local Task carries on in the same run.
    env.edit_stub(|s| s["hosts"]["remote"] = json!("unknown"));
    env.apply(task("near", true)).ok();
    let report = env.reconcile().ok().json();
    assert_eq!(env.status("near")["phase"], "Briefed");
    let far = env.status("far");
    assert_eq!(far["phase"], "Spawned", "{far:#}");
    assert!(
        far["conditions"]
            .as_array()
            .unwrap()
            .iter()
            .all(|c| c["type"] != "WorkerGone"),
        "{far:#}"
    );
    assert!(
        report["unknowns"].to_string().contains("remote"),
        "{report:#}"
    );
    assert_eq!(argv_of(&env, "create").len(), 2, "nothing respawned");

    // A host thurbox says it cannot reach is False: nothing is spawned there.
    env.edit_stub(|s| s["hosts"]["remote"] = json!("down"));
    let mut blocked = task("blocked", true);
    blocked["spec"]["host"] = json!("remote");
    env.apply(blocked).ok();
    env.reconcile().ok();
    assert_condition(
        &env.status("blocked"),
        "HostReachable",
        "False",
        "Unreachable",
    );
    assert_eq!(argv_of(&env, "create").len(), 2);
}

#[test]
fn a_spawn_that_times_out_is_adopted_by_its_key_and_its_process_tree_is_killed() {
    let env = setup("thurbox-spawn-timeout", vec![]);
    // The session is created, then the answer never comes.
    env.stub_fault("create", "hang-after", 1);
    env.apply(task("t1", true)).ok();
    env.reconcile().ok();

    let st = env.status("t1");
    assert_eq!(st["phase"], "Briefed", "{st:#}");
    assert_condition(&st, "Spawned", "True", "Adopted");
    assert_eq!(env.stub_sessions().len(), 1, "never twinned");
    assert_eq!(argv_of(&env, "create").len(), 1, "never respawned blindly");
    let pids = env.grandchildren();
    assert!(!pids.is_empty(), "the stub hung with a grandchild");
    for pid in pids {
        assert!(
            !alive_after(pid, Duration::from_secs(10)),
            "process {pid} outlived the timeout: the whole tree must be killed"
        );
    }
}

#[test]
fn an_orphan_session_holding_the_key_is_adopted_not_twinned() {
    let env = setup("thurbox-orphan", vec![task("t1", false)]);
    let key = thurbox_key(&env, "t1");
    // A manager that died after its create left this session behind.
    env.edit_stub(|s| {
        s["sessions"] = json!([{
            "id": "00000000-0000-4000-8000-000000000099", "key": key, "host": "local",
            "name": key, "agent": "claude-code", "state": "idle", "hook_reported": true,
            "stopped": false, "meta": {}, "sent": [], "env": []
        }]);
    });
    env.apply(task("t1", true)).ok();
    env.reconcile().ok();
    assert_eq!(env.stub_sessions().len(), 1);
    assert_eq!(env.status("t1")["phase"], "Briefed");
    let adopted = receipts(&full_status(&env, "t1"))
        .iter()
        .any(|r| r["outcome"]["known"]["adopted"] == true);
    assert!(adopted, "{:#}", full_status(&env, "t1"));
}

#[test]
fn a_brief_whose_answer_was_lost_is_read_back_and_never_sent_twice() {
    // Recovered: the session recorded the delivery, the answer was lost.
    let env = setup("thurbox-brief-recovered", vec![]);
    // meta-set #1 marks the brief pending, #2 marks it delivered.
    env.stub_fault_after("meta-set", "hang-after", 1, 1);
    env.apply(task("t1", true)).ok();
    env.reconcile().ok();
    let st = env.status("t1");
    assert_eq!(st["phase"], "Briefed", "{st:#}");
    assert_condition(&st, "Briefed", "True", "ReceiptRecovered");
    assert_eq!(argv_of(&env, "message send").len(), 1);

    // Unconfirmed: the send itself never answered. Fleet cannot tell, so it
    // asks for a person rather than typing the brief again.
    let env = setup("thurbox-brief-unconfirmed", vec![]);
    env.stub_fault("message-send", "hang-after", 1);
    env.apply(task("t1", true)).ok();
    env.reconcile().ok();
    for _ in 0..3 {
        env.expire_wait("t1");
        env.reconcile().ok();
    }
    let st = env.status("t1");
    assert_eq!(st["phase"], "Spawned", "{st:#}");
    assert_condition(&st, "Briefed", "Unknown", "BriefUnconfirmed");
    assert_condition(&st, "NeedsAttention", "True", "BriefUnconfirmed");
    assert_eq!(
        argv_of(&env, "message send").len(),
        1,
        "never a second brief"
    );

    // A send that fails may have typed part of the brief before failing:
    // thurbox-cli cannot say nothing arrived, so neither does fleet.
    let env = setup("thurbox-brief-failed-send", vec![]);
    env.stub_fault("message-send", "fail", 1);
    env.apply(task("t1", true)).ok();
    env.reconcile().ok();
    env.expire_wait("t1");
    env.reconcile().ok();
    let st = env.status("t1");
    assert_eq!(st["phase"], "Spawned", "{st:#}");
    assert_condition(&st, "Briefed", "Unknown", "BriefUnconfirmed");
    assert_eq!(argv_of(&env, "message send").len(), 1, "never typed twice");
}

#[test]
fn an_inbox_entry_too_long_for_tmux_is_refused_and_never_blocks_the_inbox() {
    // A relayed result of 16 440 bytes, as Mission Control received: tmux
    // refuses it before typing, so it must never wait on a journal forever.
    let env = setup("thurbox-inbox-too-long", vec![task("t1", true)]);
    env.reconcile().ok();
    let key = thurbox_key(&env, "t1");
    let long = "x".repeat(16_440);
    env.fleet(&["send", "t1", &long]).ok();
    env.fleet(&["send", "t1", "short"]).ok();
    for _ in 0..6 {
        set_session_state(&env, &key, "idle");
        env.expire_wait("t1");
        env.reconcile().ok();
    }
    let sent = &env.stub_sessions()[0]["sent"];
    assert!(
        sent.as_array().unwrap().iter().any(|t| t == "short"),
        "the entry behind the long one was delivered: {sent:#} {:#}",
        env.status("t1")["conditions"]
    );
    assert!(
        !argv_of(&env, "message send")
            .iter()
            .any(|a| a.iter().any(|x| x.contains(&long))),
        "the long text never reached thurbox-cli"
    );
    let st = env.status("t1");
    assert_condition(&st, "NeedsAttention", "True", "DeliveryFailed");
    let attention = st["conditions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["type"] == "NeedsAttention")
        .unwrap()["message"]
        .to_string();
    assert!(attention.contains("16440 bytes"), "{attention}");
    let inbox = full_status(&env, "t1")["inbox"].clone();
    assert_eq!(inbox.as_array().unwrap().len(), 1, "{inbox:#}");
    assert_eq!(inbox[0]["text"], json!(long), "the refused entry is kept");
}

#[test]
fn an_unconfirmed_inbox_entry_is_given_up_and_later_entries_go_past_it() {
    // A send that fails may have typed part of the entry: it is never typed
    // again, but neither does it hold every later entry forever.
    let env = setup("thurbox-inbox-unconfirmed", vec![task("t1", true)]);
    env.reconcile().ok();
    let key = thurbox_key(&env, "t1");
    env.fleet(&["send", "t1", "first"]).ok();
    env.fleet(&["send", "t1", "second"]).ok();
    env.stub_fault("message-send", "fail", 1);
    for _ in 0..6 {
        set_session_state(&env, &key, "idle");
        env.expire_wait("t1");
        env.reconcile().ok();
    }
    let sends = argv_of(&env, "message send");
    assert_eq!(
        sends
            .iter()
            .filter(|a| a.contains(&"--body=first".into()))
            .count(),
        1,
        "never typed twice: {sends:?}"
    );
    let sent = &env.stub_sessions()[0]["sent"];
    assert!(
        sent.as_array().unwrap().iter().any(|t| t == "second"),
        "the entry behind it was delivered: {sent:#} {:#}",
        env.status("t1")["conditions"]
    );
    let st = env.status("t1");
    assert_condition(&st, "NeedsAttention", "True", "DeliveryUnconfirmed");
    let inbox = full_status(&env, "t1")["inbox"].clone();
    assert_eq!(inbox.as_array().unwrap().len(), 1, "{inbox:#}");
    assert_eq!(inbox[0]["text"], "first", "the unconfirmed entry is kept");
}

#[test]
fn a_release_whose_answer_is_unknown_is_never_assumed_done() {
    let env = setup("thurbox-release", vec![task("t1", true), task("t2", true)]);
    env.reconcile().ok();
    for t in ["t1", "t2"] {
        let k = thurbox_key(&env, t);
        set_session_state(&env, &k, "idle");
    }

    // t1: the delete happened but its answer was lost; the next attempt finds
    // the session gone and records the release without deleting twice.
    env.stub_fault("delete", "hang-after", 1);
    env.fleet(&["delete", "task", "t1"]).ok();
    env.reconcile().ok();
    assert_eq!(env.status("t1")["finalizers"], json!(["fleet.dev/release"]));
    env.expire_wait("t1");
    env.reconcile().ok();
    env.fleet(&["get", "task", "t1"]).code(3);
    assert_eq!(argv_of(&env, "delete").len(), 1);

    // t2: the delete never answers. After three attempts, each after a
    // wait, a person is asked; the finalizer holds the record meanwhile.
    env.stub_fault("delete", "hang", 3);
    env.fleet(&["delete", "task", "t2"]).ok();
    env.reconcile().ok();
    for _ in 0..2 {
        env.expire_wait("t2");
        env.reconcile().ok();
    }
    let st = env.status("t2");
    assert_condition(&st, "Released", "False", "ReleaseFailed");
    assert_condition(&st, "NeedsAttention", "True", "ReleaseFailed");
    assert_eq!(st["finalizers"], json!(["fleet.dev/release"]));
    assert_eq!(argv_of(&env, "delete").len(), 4);
}

#[test]
fn release_never_deletes_a_different_session_that_reuses_the_key() {
    let env = setup("thurbox-release-identity", vec![task("t1", true)]);
    env.reconcile().ok();
    let key = thurbox_key(&env, "t1");
    let original_id = env.stub_sessions()[0]["id"].clone();
    let replacement_id = "00000000-0000-4000-8000-000000000999";
    env.edit_stub(|s| {
        let mut replacement = s["sessions"][0].clone();
        replacement["id"] = json!(replacement_id);
        replacement["state"] = json!("working");
        // The stored session is idle, but no longer holds this Task's key.
        s["sessions"][0]["key"] = json!("retired-key");
        s["sessions"][0]["state"] = json!("idle");
        s["sessions"].as_array_mut().unwrap().push(replacement);
    });
    env.fleet(&["delete", "task", "t1"]).ok();
    env.reconcile().ok();
    assert!(
        env.stub_sessions()
            .iter()
            .any(|s| s["id"] == replacement_id && s["state"] == "working"),
        "a different session's working state was never checked: {:#?}",
        env.stub_sessions()
    );
    assert_eq!(argv_of(&env, "delete").len(), 0);
    assert_eq!(env.get("t1")["status"]["session"]["id"], original_id);
    assert_eq!(
        env.get("t1")["metadata"]["finalizers"],
        json!(["fleet.dev/release"])
    );
    assert_eq!(
        env.stub_sessions()
            .iter()
            .filter(|s| s["key"] == key)
            .count(),
        1
    );
}

#[test]
fn a_different_session_reusing_the_key_does_not_hide_a_vanished_worker() {
    let env = setup("thurbox-worker-identity", vec![task("t1", true)]);
    env.reconcile().ok();
    let original_id = env.stub_sessions()[0]["id"].clone();
    env.edit_stub(|s| {
        // The real worker vanished; another session now holds its key.
        s["sessions"][0]["id"] = json!("00000000-0000-4000-8000-000000000999");
    });
    env.reconcile().ok();
    let st = env.status("t1");
    assert_condition(&st, "WorkerGone", "True", "SessionGone");
    assert_condition(&st, "NeedsAttention", "True", "WorkerGone");
    assert_eq!(env.get("t1")["status"]["session"]["id"], original_id);
    assert_eq!(argv_of(&env, "create").len(), 1, "never respawned");
    assert_eq!(argv_of(&env, "message send").len(), 1, "never rebriefed");
}

#[test]
fn a_relative_workspace_is_refused_before_any_session_is_made() {
    let mut t = support::task("rel", true);
    t["spec"]["workspace"]["dir"] = json!("work/rel");
    let env = setup("thurbox-relative", vec![t]);
    env.reconcile().ok();
    let st = env.status("rel");
    let spawned = support::condition(&st, "Spawned");
    assert!(
        spawned["message"].as_str().unwrap().contains("absolute"),
        "{st:#}"
    );
    assert_eq!(argv_of(&env, "create").len(), 0, "thurbox is never asked");
}

#[test]
fn a_new_store_never_adopts_a_session_an_old_store_left_behind() {
    let env = setup("thurbox-two-stores", vec![task("t1", true)]);
    env.reconcile().ok();
    assert_eq!(env.stub_sessions().len(), 1);
    // A fresh data directory on the same hosts: same names, same order.
    let cfg = std::fs::read_to_string(&env.config).unwrap();
    std::fs::write(
        &env.config,
        cfg.replace("dir = \"data\"", "dir = \"data2\""),
    )
    .unwrap();
    let mut items = runtime_config();
    items.as_array_mut().unwrap().push(task("t1", true));
    env.apply(items).ok();
    env.reconcile().ok();
    assert_eq!(
        env.stub_sessions().len(),
        2,
        "the second store's t1 has its own session: {:#?}",
        env.stub_sessions()
    );
}

#[test]
fn slow_probes_never_outlive_the_lease() {
    let env = setup("thurbox-slow", vec![task("t1", true)]);
    // Each call fits the lease; the probes of one Task together do not.
    let cfg = std::fs::read_to_string(&env.config).unwrap();
    std::fs::write(
        &env.config,
        cfg.replace("callTimeoutMs = 3000", "callTimeoutMs = 2000"),
    )
    .unwrap();
    env.stub_fault("list", "slow", 2);
    env.stub_fault("doctor", "slow", 2);
    // A regression guard, not a proof of the per-probe renewal: the lease
    // must outlast a brief's four calls with their grace, and no Task's
    // probes add up to more than that.
    env.fleet(&["reconcile", "--json", "--lease-ttl-ms", "30000"])
        .ok();
    assert_eq!(env.status("t1")["phase"], "Briefed");
}

#[test]
fn the_lease_outlasts_one_whole_runtime_operation() {
    // A brief is four thurbox-cli calls: a lease that cannot hold four of
    // them is refused before anything runs.
    let env = setup("thurbox-op-bound", vec![task("t1", true)]);
    let cfg = std::fs::read_to_string(&env.config).unwrap();
    std::fs::write(
        &env.config,
        cfg.replace("callTimeoutMs = 3000", "callTimeoutMs = 2000"),
    )
    .unwrap();
    env.fleet(&["reconcile", "--lease-ttl-ms", "5000"])
        .code(1)
        .stderr_has("lease");
    assert_eq!(argv_of(&env, "create").len(), 0);
    // The default lease is sized to hold it.
    env.reconcile().ok();
    assert_eq!(env.status("t1")["phase"], "Briefed");
}

#[test]
fn a_spec_that_would_reach_thurbox_cli_as_a_flag_is_refused_when_written() {
    let env = setup("thurbox-written-bounds", vec![]);
    for (field, value) in [("harness", "--help"), ("host", "-x")] {
        let mut t = task("dash", false);
        t["spec"][field] = json!(value);
        env.apply(t).code(1).stderr_has(field);
    }
    env.fleet(&["get", "task", "dash"]).code(3);
}

#[test]
fn a_brief_that_starts_with_a_dash_is_sent_as_text() {
    let mut t = task("dash", true);
    t["spec"]["brief"] = json!("- fix the failing test");
    let env = setup("thurbox-dash-brief", vec![t]);
    env.reconcile().ok();
    let st = env.status("dash");
    assert_eq!(st["phase"], "Briefed", "{st:#}");
    assert_eq!(
        env.stub_sessions()[0]["sent"],
        json!(["- fix the failing test"])
    );
}

/// A fresh Codex has not emitted a hook yet. The process state alone cannot
/// distinguish its empty initial composer from a dialog or an active turn.
fn initial_codex(env: &Env) {
    env.edit_stub(|s| {
        s["spawnState"] = json!("running");
        s["spawnFields"] = json!({
            "agent": "codex", "state_source": "process", "hook_reported": false,
            "hook_state": null, "hook_state_contradicted": false,
            "hook_corroboration": "agent", "foreground_process": "/bin/codex"
        });
        s["capture"] = json!({
            "ansi": false, "lines": 0, "cursor_row": 3, "cursor_col": 2,
            "foreground_process": "/bin/codex",
            "output": "  >_ OpenAI Codex (v0.164.0)\n  To get started, describe a task or try one of these commands:\n\n› Ask Codex to do anything\n  ? for shortcuts\n"
        });
    });
}

#[test]
fn a_never_reported_codex_initial_composer_receives_its_brief_once() {
    let env = setup("codex-first-start", vec![]);
    initial_codex(&env);
    let mut t = task("first", true);
    t["spec"]["harness"] = json!("codex");
    env.apply(t).ok();
    // The first manager sees startup still in progress, then exits. A new
    // process must recover the same session when the composer becomes ready.
    env.edit_stub(|s| s["capture"]["cursor_row"] = json!(0));
    env.reconcile().ok();
    assert_eq!(env.status("first")["phase"], "Spawned");
    assert_eq!(argv_of(&env, "message send").len(), 0);
    let id = env.stub_sessions()[0]["id"].clone();
    env.edit_stub(|s| s["capture"]["cursor_row"] = json!(3));
    env.reconcile().ok();
    assert_eq!(env.status("first")["phase"], "Briefed");
    // Each reconcile is a new CLI process: reconnect keeps identity and receipt.
    env.reconcile().ok();
    assert_eq!(env.stub_sessions()[0]["id"], id);
    assert_eq!(argv_of(&env, "message send").len(), 1);
    assert_eq!(argv_of(&env, "create").len(), 1);
    assert!(
        argv_of(&env, "capture")[0]
            .windows(2)
            .any(|a| a == ["--lines", "0"])
    );
    // Keep the initial composer evidence visible: even when ready() accepts
    // it, at_rest() must not use that proof to authorize deletion.
    set_session_state(&env, &thurbox_key(&env, "first"), "running");
    env.fleet(&["delete", "task", "first"]).ok();
    env.reconcile().ok();
    assert_eq!(argv_of(&env, "delete").len(), 0);
}

#[test]
fn uncertain_or_occupied_codex_startup_never_receives_a_brief() {
    // A Codex reporting `working` takes its brief on its own queue, which
    // never types into the composer, so it is no case here.
    let cases = [
        ("unknown", "spawnState", json!("unknown")),
        ("wrong-key", "key", json!("another-task")),
        ("hook-working", "hook_state", json!("working")),
        ("blocked", "spawnState", json!("blocked")),
        ("shell", "agent", json!("shell")),
        ("stopped", "stopped", json!(true)),
        ("reported", "hook_reported", json!(true)),
        ("contradicted", "hook_state_contradicted", json!(true)),
        ("uncorroborated", "hook_corroboration", json!("unknown")),
        ("dead", "foreground_process", json!("/bin/bash")),
        ("cursor-row", "cursor_row", json!(0)),
        ("cursor-col", "cursor_col", json!(5)),
        ("capture-shell", "capture_process", json!("/bin/bash")),
        (
            "dialog",
            "output",
            json!("Trust this folder?\n› Ask Codex to do anything\n"),
        ),
        ("occupied", "output", json!("Codex\n\n› do something\n")),
        ("missing", "cursor_row", Value::Null),
        ("missing-process", "foreground_process", Value::Null),
        ("scrollback", "lines", json!(200)),
        ("ansi", "ansi", json!(true)),
    ];
    for (name, field, value) in cases {
        let env = setup(&format!("codex-guard-{name}"), vec![]);
        initial_codex(&env);
        env.edit_stub(|s| match field {
            "spawnState" => s[field] = value,
            "cursor_row" | "cursor_col" | "output" | "lines" | "ansi" => {
                s["capture"][field] = value
            }
            "capture_process" => s["capture"]["foreground_process"] = value,
            _ => s["spawnFields"][field] = value,
        });
        let mut t = task("guard", true);
        t["spec"]["harness"] = json!("codex");
        env.apply(t).ok();
        env.reconcile().ok();
        assert_eq!(argv_of(&env, "message send").len(), 0, "{name}");
    }
    let env = setup("codex-unreadable-capture", vec![]);
    initial_codex(&env);
    env.stub_fault("capture", "fail", 10);
    env.apply(task("guard", true)).ok();
    env.reconcile().ok();
    assert_eq!(argv_of(&env, "message send").len(), 0);
}

/// A Task that names repositories instead of a directory: the runtime config
/// maps each repository to its source checkout on the host.
fn repositories_task(name: &str) -> Value {
    let mut t = support::task(name, true);
    t["spec"]["workspace"] = json!({"repositories": [
        {"name": "fleet", "url": "https://example.invalid/org/fleet", "ref": "main"},
        {"name": "docs", "url": "https://example.invalid/org/docs"}
    ]});
    t
}

fn map_repositories(env: &Env) {
    let (fleet, docs) = if cfg!(windows) {
        ("C:\\\\src\\\\fleet", "C:\\\\src\\\\docs")
    } else {
        ("/src/fleet", "/src/docs")
    };
    let mut cfg = std::fs::read_to_string(&env.config).expect("config");
    cfg.push_str(&format!(
        "\n[runtime.repositories]\n\"https://example.invalid/org/fleet\" = \"{fleet}\"\n\"https://example.invalid/org/docs\" = \"{docs}\"\n"
    ));
    std::fs::write(&env.config, cfg).expect("config");
}

#[test]
fn a_workspace_fleet_made_reaches_thurbox_as_a_ready_directory() {
    let env = Env::thurbox("thurbox-ready-workspace");
    map_repositories(&env);
    let mut cfg = std::fs::read_to_string(&env.config).expect("config");
    cfg.push_str("\n[workspaces]\nkind = \"fake\"\ndir = \"fake\"\n");
    std::fs::write(&env.config, cfg).expect("config");
    let mut items = runtime_config();
    items.as_array_mut().unwrap().push(repositories_task("r1"));
    env.apply(items).ok();
    env.reconcile().ok();

    let st = full_status(&env, "r1");
    assert_eq!(st["phase"], "Briefed", "{st:#}");
    let dir = st["workspace"]["dir"]
        .as_str()
        .expect("a workspace")
        .to_string();
    let create = &argv_of(&env, "create")[0];
    let after = |flag: &str| {
        create
            .iter()
            .position(|a| a == flag)
            .map(|i| create[i + 1].clone())
    };
    // thurbox runs the session in the directory and makes no worktree.
    assert_eq!(after("--repo-path"), Some(dir), "{create:?}");
    // Every other worktree Fleet made is spanned as it is.
    let added: Vec<&String> = create
        .windows(2)
        .filter(|w| w[0] == "--add-dir")
        .map(|w| &w[1])
        .collect();
    let extra: Vec<&str> = st["workspace"]["extraDirs"]
        .as_array()
        .expect("the other worktrees")
        .iter()
        .map(|d| d.as_str().expect("a dir"))
        .collect();
    assert_eq!(extra.len(), 1, "{st:#}");
    assert!(extra[0].ends_with("docs"), "{st:#}");
    assert_eq!(added, extra, "{create:?}");
    for flag in ["--worktree-branch", "--base-branch", "--add-repo"] {
        assert_eq!(after(flag), None, "{create:?}");
    }
}

#[test]
fn a_repositories_workspace_becomes_thurbox_worktrees_the_adapter_names() {
    let env = Env::thurbox("thurbox-repositories");
    map_repositories(&env);
    let mut items = runtime_config();
    items.as_array_mut().unwrap().push(repositories_task("r1"));
    env.apply(items).ok();
    env.reconcile().ok();

    assert_eq!(
        env.status("r1")["phase"],
        "Briefed",
        "{:#}",
        full_status(&env, "r1")
    );
    let uid = env.get("r1")["metadata"]["uid"]
        .as_str()
        .unwrap()
        .to_string();
    let create = &argv_of(&env, "create")[0];
    let after = |flag: &str| {
        create
            .iter()
            .position(|a| a == flag)
            .map(|i| create[i + 1].clone())
    };
    let (fleet, docs) = if cfg!(windows) {
        ("C:\\src\\fleet", "C:\\src\\docs")
    } else {
        ("/src/fleet", "/src/docs")
    };
    assert_eq!(after("--repo-path").as_deref(), Some(fleet), "{create:?}");
    assert_eq!(
        after("--worktree-branch"),
        Some(format!("fleet/r1.{uid}")),
        "{create:?}"
    );
    assert_eq!(
        after("--base-branch").as_deref(),
        Some("main"),
        "{create:?}"
    );
    assert_eq!(after("--add-repo").as_deref(), Some(docs), "{create:?}");

    // Deleting the Task deletes the session; by default the runtime tears its
    // worktrees down with it (`--force`). Fleet never names a worktree.
    env.fleet(&result_args("r1", "1")).ok();
    env.reconcile().ok();
    env.fleet(&["delete", "task", "r1"]).ok();
    set_session_state(&env, &thurbox_key(&env, "r1"), "idle");
    env.reconcile().ok();
    assert!(env.stub_sessions().is_empty(), "{:#?}", env.stub_sessions());
    let delete = &argv_of(&env, "delete")[0];
    assert!(delete.iter().any(|a| a == "--force"), "{delete:?}");
    assert!(
        !delete
            .iter()
            .any(|a| a.contains("src") || a.contains("worktree")),
        "{delete:?}"
    );
    env.fleet(&["get", "task", "r1"]).code(3);
}

#[test]
fn a_runtime_configured_to_keep_workspaces_deletes_the_session_softly() {
    let env = Env::thurbox("thurbox-keep");
    let cfg = std::fs::read_to_string(&env.config).expect("config");
    let cfg = cfg.replace("[runtime]\n", "[runtime]\nworkspaceCleanup = \"keep\"\n");
    std::fs::write(&env.config, cfg).expect("config");
    let mut items = runtime_config();
    items.as_array_mut().unwrap().push(task("k1", true));
    env.apply(items).ok();
    env.reconcile().ok();
    env.fleet(&result_args("k1", "1")).ok();
    env.reconcile().ok();
    env.fleet(&["delete", "task", "k1"]).ok();
    set_session_state(&env, &thurbox_key(&env, "k1"), "idle");
    env.reconcile().ok();
    let delete = &argv_of(&env, "delete")[0];
    assert!(delete.iter().any(|a| a == "--soft"), "{delete:?}");
    assert!(!delete.iter().any(|a| a == "--force"), "{delete:?}");
}

#[test]
fn a_runtime_workspace_setting_that_cannot_work_is_refused_at_load() {
    for (extra, want) in [
        ("workspaceCleanup = \"later\"\n", "runtime.workspaceCleanup"),
        (
            "[runtime.repositories]\n\"https://example.invalid/a\" = \"src/a\"\n",
            "runtime.repositories",
        ),
    ] {
        let env = Env::thurbox("thurbox-bad-workspace-config");
        let cfg = std::fs::read_to_string(&env.config).expect("config");
        let cfg = if extra.starts_with('[') {
            format!("{cfg}\n{extra}")
        } else {
            cfg.replace("[runtime]\n", &format!("[runtime]\n{extra}"))
        };
        std::fs::write(&env.config, cfg).expect("config");
        env.fleet(&["reconcile"]).code(1).stderr_has(want);
    }
}

#[test]
fn a_repository_the_runtime_does_not_map_is_refused_at_spawn() {
    let env = setup("thurbox-unmapped", vec![repositories_task("r2")]);
    env.reconcile().ok();

    assert!(argv_of(&env, "create").is_empty(), "{:?}", env.stub_calls());
    let full = full_status(&env, "r2");
    assert!(
        full.to_string()
            .contains("https://example.invalid/org/fleet is not in runtime.repositories"),
        "{full:#}"
    );
    // The fix is a config change, not a retry: no spawn attempt is spent,
    // so the Task waits for the mapping instead of stalling.
    assert_eq!(full["phase"], "Pending", "{full:#}");
    assert_eq!(full["spawnAttempts"].as_u64().unwrap_or(0), 0, "{full:#}");
    assert_condition(&full, "Spawned", "False", "RepositoryUnmapped");
    assert_condition(&full, "NeedsAttention", "True", "RepositoryUnmapped");
}

/// A host reboot as thurbox shows it: every session row survives with its
/// id, key and last reported state, but its window is gone.
fn reboot(env: &Env) {
    env.edit_stub(|s| {
        for sess in s["sessions"].as_array_mut().unwrap() {
            sess["hook_corroboration"] = json!("missing");
        }
    });
}

fn sent(env: &Env, i: usize) -> Vec<String> {
    env.stub_sessions()[i]["sent"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t.as_str().unwrap().to_string())
        .collect()
}

#[test]
fn a_briefed_task_resumes_in_its_own_session_after_a_reboot() {
    let env = setup("thurbox-reboot", vec![task("t1", true)]);
    env.reconcile().ok();
    assert_eq!(env.status("t1")["phase"], "Briefed");
    let id = env.stub_sessions()[0]["id"].clone();
    // The worker was mid-turn when the host went down.
    set_session_state(&env, &thurbox_key(&env, "t1"), "working");

    reboot(&env);
    env.reconcile().ok();

    let restarts = argv_of(&env, "restart");
    assert_eq!(restarts.len(), 1, "{restarts:?}");
    assert_eq!(restarts[0][2], id.as_str().unwrap());
    assert!(
        restarts[0].iter().any(|a| a == "--if-missing"),
        "{restarts:?}"
    );
    assert_eq!(argv_of(&env, "create").len(), 1, "never a second session");
    let st = full_status(&env, "t1");
    assert_eq!(st["phase"], "Briefed", "{st:#}");
    assert_eq!(st["session"]["id"], id, "{st:#}");
    assert_eq!(st["brief"]["generation"], 1, "{st:#}");

    // The resumed worker is told it was restarted, and given its brief again.
    let texts = sent(&env, 0);
    assert_eq!(texts.len(), 2, "{texts:#?}");
    assert_eq!(texts[0], "Do the work for t1.");
    assert!(texts[1].contains("restarted"), "{}", texts[1]);
    assert!(texts[1].contains("Do the work for t1."), "{}", texts[1]);
    assert!(texts[1].contains("generation 1"), "{}", texts[1]);

    // Nothing repeats on the next pass.
    set_session_state(&env, &thurbox_key(&env, "t1"), "working");
    env.reconcile().ok();
    assert_eq!(argv_of(&env, "restart").len(), 1);
    assert_eq!(sent(&env, 0).len(), 2);

    // The generation the worker was briefed for still fences its result.
    env.fleet(&result_args("t1", "2"))
        .code(1)
        .stderr_has("stale generation 2");
    env.fleet(&result_args("t1", "1")).ok();
    env.reconcile().ok();
    assert_eq!(env.status("t1")["phase"], "Concluded");
}

#[test]
fn a_reboot_revives_only_work_in_flight_or_feedback() {
    let env = setup(
        "thurbox-reboot-scope",
        vec![task("done", true), task("held", false)],
    );
    env.reconcile().ok();
    env.fleet(&result_args("done", "1")).ok();
    env.reconcile().ok();
    assert_eq!(env.status("done")["phase"], "Concluded");
    assert_eq!(env.status("held")["phase"], "Pending");

    reboot(&env);
    env.expire_session_check("done");
    env.reconcile().ok();
    assert!(
        argv_of(&env, "restart").is_empty(),
        "a concluded worker rests"
    );
    assert_eq!(env.status("held")["phase"], "Pending", "held stays held");
    assert_eq!(argv_of(&env, "create").len(), 1);

    // Feedback for the concluded worker brings its session back to read it.
    env.fleet(&["send", "done", "Review: rename the flag."])
        .ok();
    env.reconcile().ok();
    assert_eq!(argv_of(&env, "restart").len(), 1);
    assert_eq!(argv_of(&env, "create").len(), 1);
    // The note that it was restarted goes first; the feedback follows it
    // on the agent's own inbox, without waiting for the turn to end.
    let texts = sent(&env, 0);
    assert_eq!(texts.len(), 3, "{texts:#?}");
    assert!(texts[1].contains("already recorded"), "{}", texts[1]);
    assert_eq!(texts[2], "Review: rename the flag.");
    assert_eq!(env.status("done")["phase"], "Concluded");
}

#[test]
fn a_task_deleted_after_a_reboot_releases_its_lost_session() {
    let env = setup("thurbox-reboot-delete", vec![task("t1", true)]);
    env.reconcile().ok();
    // Its last report before the host went down was `working`.
    set_session_state(&env, &thurbox_key(&env, "t1"), "working");
    reboot(&env);
    env.fleet(&["delete", "task", "t1"]).ok();
    env.reconcile().ok();
    assert!(
        argv_of(&env, "restart").is_empty(),
        "a deleted Task is not revived"
    );
    assert!(env.stub_sessions().is_empty(), "{:#?}", env.stub_sessions());
    env.fleet(&["get", "task", "t1"]).code(3);
}

#[test]
fn a_message_reaches_a_working_claude_session_through_its_native_inbox() {
    // Mission Control's report: a `fleet send` to a worker mid-turn waited
    // for the turn to end, so a running worker could not be steered. thurbox
    // has a native route, `message send`, that reaches a Claude Code turn
    // between tool calls without typing into the terminal composer.
    let env = setup("thurbox-inbox-native", vec![task("t1", true)]);
    env.reconcile().ok();
    let key = thurbox_key(&env, "t1");
    assert_eq!(env.status("t1")["phase"], "Briefed");
    // The brief started a turn; the worker is busy.
    set_session_state(&env, &key, "working");

    env.fleet(&["send", "t1", "steer: prefer the smaller fix"])
        .ok();
    env.expire_wait("t1");
    env.reconcile().ok();

    let full = full_status(&env, "t1");
    let delivered = full["deliveries"]["receipts"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert_eq!(
        delivered.len(),
        1,
        "the message reached the working session, not after its turn: {full:#}"
    );
    assert_eq!(delivered[0]["deliveredVia"], "claude-socket", "{full:#}");
    assert!(
        argv_of(&env, "send").is_empty(),
        "nothing was typed into the composer: {:?}",
        argv_of(&env, "send")
    );
    let messages = argv_of(&env, "message send");
    assert!(
        messages
            .iter()
            .any(|a| a.contains(&"--body=steer: prefer the smaller fix".to_string())),
        "{messages:?}"
    );
}

#[test]
fn a_route_that_may_hold_the_text_is_recorded_on_the_receipt_only() {
    // thurbox says may_hold for every Claude Code socket delivery: a fact
    // about the route, which no person can clear, so it raises nothing.
    let env = setup("thurbox-inbox-may-hold", vec![task("t1", true)]);
    env.reconcile().ok();
    env.fleet(&["send", "t1", "are you there?"]).ok();
    env.expire_wait("t1");
    env.reconcile().ok();
    let full = full_status(&env, "t1");
    let receipt = &full["deliveries"]["receipts"][0];
    assert_eq!(receipt["mayHold"], true, "{full:#}");
    assert_eq!(receipt["deliveredVia"], "claude-socket", "{full:#}");
    let held = env.status("t1")["conditions"]
        .as_array()
        .unwrap()
        .iter()
        .any(|c| c["type"] == "NeedsAttention" && c["status"] == "True");
    assert!(!held, "{full:#}");
}

#[test]
fn a_message_thurbox_only_queues_is_given_up_and_never_sent_again() {
    // thurbox falls back to its mailbox when it cannot reach the agent, and
    // still exits 0. The agent never reads it there.
    let env = setup("thurbox-inbox-queued", vec![task("t1", true)]);
    env.reconcile().ok();
    env.edit_stub(|s| s["mailboxNote"] = json!("claude inbox socket refused"));
    env.fleet(&["send", "t1", "first"]).ok();
    for _ in 0..4 {
        env.expire_wait("t1");
        env.reconcile().ok();
    }
    let st = env.status("t1");
    assert_condition(&st, "NeedsAttention", "True", "DeliveryQueued");
    let attention = st["conditions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["type"] == "NeedsAttention")
        .unwrap()["message"]
        .to_string();
    assert!(
        attention.contains("claude inbox socket refused"),
        "{attention}"
    );
    let firsts = argv_of(&env, "message send")
        .iter()
        .filter(|a| a.contains(&"--body=first".to_string()))
        .count();
    assert_eq!(firsts, 1, "never sent again");
    let full = full_status(&env, "t1");
    assert_eq!(full["inbox"].as_array().unwrap().len(), 1, "{full:#}");
    assert!(full["deliveries"]["receipts"].is_null(), "{full:#}");

    // A later entry goes past it once thurbox reaches the agent again.
    env.edit_stub(|s| s["mailboxNote"] = Value::Null);
    env.fleet(&["send", "t1", "second"]).ok();
    env.expire_wait("t1");
    env.reconcile().ok();
    let sent = &env.stub_sessions()[0]["sent"];
    assert!(
        sent.as_array().unwrap().iter().any(|t| t == "second"),
        "{sent:#}"
    );
}

#[test]
fn given_up_entries_never_fill_the_inbox() {
    // A Claude session whose inbox socket thurbox cannot find has every
    // message kept in the mailbox. Each one is given up and stays in
    // status.inbox, but none can ever be delivered, so none may hold back
    // the next message once thurbox reaches the agent again.
    let env = setup("thurbox-inbox-given-up-full", vec![task("t1", true)]);
    env.reconcile().ok();
    env.edit_stub(|s| s["mailboxNote"] = json!("no Claude inbox socket"));
    let bound = fleet_api::inbox::MAX_INBOX;
    for i in 0..=bound {
        env.fleet(&["send", "t1", &format!("lost {i}")]).ok();
        env.expire_wait("t1");
        env.reconcile().ok();
    }
    let full = full_status(&env, "t1");
    assert_eq!(
        full["inbox"].as_array().unwrap().len(),
        bound + 1,
        "{full:#}"
    );
    let given_up = full["deliveryAttempts"]
        .as_object()
        .unwrap()
        .values()
        .filter(|a| a["failed"] == true)
        .count();
    assert_eq!(given_up, bound + 1, "every entry given up: {full:#}");

    env.edit_stub(|s| s["mailboxNote"] = Value::Null);
    env.fleet(&["send", "t1", "after the outage"]).ok();
    env.expire_wait("t1");
    env.reconcile().ok();
    let sent = &env.stub_sessions()[0]["sent"];
    assert!(
        sent.as_array()
            .unwrap()
            .iter()
            .any(|t| t == "after the outage"),
        "{sent:#}"
    );
    let st = env.status("t1");
    assert_condition(&st, "NeedsAttention", "True", "DeliveryQueued");
}
