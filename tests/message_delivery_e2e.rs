//! `thurbox-cli message send` delivering into a Codex session's own thread.
//!
//! Driven through the real binaries, with a stand-in `codex` that logs what it
//! was asked to queue: the question is which conversation a body lands in, and
//! that is decided across two commands (the agent's `SessionStart` hook binding
//! a thread, then a peer's send reading it back), so neither half alone says it.

#![cfg(unix)]

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

use serde_json::Value;
use thurbox::session::SessionId;
use thurbox::storage::Database;
use thurbox::sync::SharedSession;

#[path = "support/codex_hook.rs"]
mod codex_hook;

const CODEX_THREAD_META: &str = "thurbox.codex_conversation_id";

/// Point this process, and every `thurbox-cli` it runs, at `home`. Both forms,
/// as `tests/create_e2e.rs` does: nextest runs one process per test.
fn isolate(home: &Path) -> Database {
    thurbox::paths::set_test_dir(home);
    std::env::set_var(thurbox::paths::CONFIG_DIR_OVERRIDE_ENV, home);
    std::env::set_var(thurbox::paths::DATA_DIR_OVERRIDE_ENV, home);
    // An empty Claude registry, so no Claude socket on this machine is a route.
    std::env::set_var("CLAUDE_CONFIG_DIR", home.join("claude"));
    let path = thurbox::paths::database_file().expect("db path");
    std::fs::create_dir_all(path.parent().expect("data dir")).expect("mkdir");
    Database::open(&path).expect("open db")
}

fn codex_row(agent_session_id: &str) -> SharedSession {
    SharedSession {
        id: SessionId::default(),
        name: "coder".into(),
        agent: "codex".into(),
        backend_id: String::new(),
        backend_type: "local-tmux".into(),
        agent_session_id: Some(agent_session_id.into()),
        cwd: None,
        additional_dirs: Vec::new(),
        worktrees: Vec::new(),
        shell_backend_id: None,
        parent_session_id: None,
        display_order: None,
        tombstone: false,
        tombstone_at: None,
        key: None,
    }
}

/// A `codex` on `PATH` that appends the thread of each `queue --thread <id>`
/// it is asked for to `$FAKE_CODEX_LOG` and accepts it, as the real one does
/// for any thread with a rollout.
fn fake_codex(bin: &Path) -> std::ffi::OsString {
    use std::os::unix::fs::PermissionsExt;

    std::fs::create_dir_all(bin).expect("mkdir bin");
    let fake = bin.join("codex");
    std::fs::write(
        &fake,
        "#!/bin/sh\nprintf '%s\\n' \"$3\" >> \"$FAKE_CODEX_LOG\"\n",
    )
    .expect("write fake codex");
    std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    let mut path = bin.as_os_str().to_owned();
    if let Some(rest) = std::env::var_os("PATH") {
        path.push(":");
        path.push(rest);
    }
    path
}

/// What Codex's `SessionStart` hook runs, fed the payload Codex gives it.
fn codex_session_start(row: &SharedSession, conversation: &str, source: &str) {
    let home = tempfile::tempdir().unwrap();
    let mut hook = codex_hook::command(home.path())
        .env("THURBOX_SESSION", row.id.to_string())
        .env(
            "THURBOX_SESSION_ID",
            row.agent_session_id.as_deref().unwrap(),
        )
        .env_remove("THURBOX_CODEX_PICKER")
        .stdin(Stdio::piped())
        .spawn()
        .expect("spawn bind-codex");
    write!(
        hook.stdin.take().unwrap(),
        "{{\"session_id\":\"{conversation}\",\"source\":\"{source}\",\"hook_event_name\":\"SessionStart\"}}"
    )
    .unwrap();
    assert!(hook.wait().unwrap().success(), "bind-codex failed");
}

fn send(to: &SharedSession, body: &str, path: &std::ffi::OsStr, log: &Path) -> Value {
    let out = Command::new(env!("CARGO_BIN_EXE_thurbox-cli"))
        .args(["message", "send", "--to", &to.name, "--kind", "note"])
        .args(["--body", body, "--json"])
        .env("PATH", path)
        .env("FAKE_CODEX_LOG", log)
        .env_remove("THURBOX_SESSION")
        .output()
        .expect("run message send");
    assert!(
        out.status.success(),
        "message send failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).expect("send --json")
}

/// Codex's `/new` opens a fresh thread in the same process and reports it as
/// `startup` (the TUI sends no start source, and the app server defaults it),
/// not `clear`. A send after it must not queue the body on the thread the pane
/// left: Codex accepts it there, thurbox marks the row read, and the agent the
/// user is looking at never sees it.
#[test]
fn a_send_after_codex_new_does_not_queue_on_the_abandoned_thread() {
    let home = tempfile::tempdir().expect("tempdir");
    let db = isolate(home.path());
    let log = home.path().join("codex-queue.log");
    let path = fake_codex(&home.path().join("bin"));

    let row = codex_row("5f0c5a3e-6d7b-4c2a-9a55-1b2c3d4e5f60");
    db.upsert_session(&row).expect("insert row");
    let first = "11111111-1111-4111-8111-111111111111";
    codex_session_start(&row, first, "startup");
    assert_eq!(
        db.get_session_meta(row.id, CODEX_THREAD_META)
            .unwrap()
            .as_deref(),
        Some(first)
    );

    let before = send(&row, "before /new", &path, &log);
    assert_eq!(before["delivered_via"], "codex-queue", "{before}");

    let second = "22222222-2222-4222-8222-222222222222";
    codex_session_start(&row, second, "startup");
    let after = send(&row, "after /new", &path, &log);

    let queued = std::fs::read_to_string(&log).unwrap_or_default();
    assert_eq!(
        queued.lines().collect::<Vec<_>>(),
        [first],
        "only the send before `/new` may be queued, and on the thread it was bound to"
    );
    assert_eq!(after["delivered_via"], "mailbox", "{after}");
    let unread = db.list_messages(row.id, true, None).expect("list unread");
    assert_eq!(
        unread.iter().map(|m| m.body.as_str()).collect::<Vec<_>>(),
        ["after /new"],
        "the undelivered body must still be waiting in the mailbox"
    );
}

#[test]
fn ambiguous_codex_binding_reports_unbound_and_never_queues() {
    let home = tempfile::tempdir().unwrap();
    let db = isolate(home.path());
    let row = codex_row("55555555-5555-4555-8555-555555555555");
    db.upsert_session(&row).unwrap();
    db.set_session_meta(row.id, CODEX_THREAD_META, "picker-required")
        .unwrap();
    let mut hook = codex_hook::command(home.path())
        .env("THURBOX_SESSION", row.id.to_string())
        .env(
            "THURBOX_SESSION_ID",
            row.agent_session_id.as_deref().unwrap(),
        )
        .env_remove("THURBOX_CODEX_PICKER")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    hook.stdin.take().unwrap().write_all(br#"{"session_id":"11111111-1111-4111-8111-111111111111","source":"startup","hook_event_name":"SessionStart"}"#).unwrap();
    let out = hook.wait_with_output().unwrap();
    assert!(out.status.success());
    let output: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(output["bound"], false, "{output}");
    let log = home.path().join("queue.log");
    let path = fake_codex(&home.path().join("bin"));
    assert_eq!(
        send(&row, "held safely", &path, &log)["delivered_via"],
        "mailbox"
    );
    assert!(!log.exists());
    for invalid in [
        "picker-required",
        "00000000-0000-0000-0000-000000000000",
        "ffffffff-ffff-ffff-ffff-ffffffffffff",
    ] {
        db.set_session_meta(row.id, CODEX_THREAD_META, invalid)
            .unwrap();
        let result = send(&row, "still held", &path, &log);
        assert_eq!(result["delivered_via"], "mailbox");
        assert!(
            result["delivery_note"]
                .as_str()
                .unwrap()
                .contains("invalid or ambiguous"),
            "{result}"
        );
        assert!(!log.exists());
    }
    assert_eq!(db.list_messages(row.id, true, None).unwrap().len(), 4);
}

fn hook_output(
    row: &SharedSession,
    payload: Value,
    no_daemon: bool,
    pane: &str,
    picker: bool,
) -> std::process::Output {
    let home = tempfile::tempdir().unwrap();
    let mut command = codex_hook::command_with_mode(home.path(), no_daemon);
    command
        .env("THURBOX_SESSION", row.id.to_string())
        .env(
            "THURBOX_SESSION_ID",
            row.agent_session_id.as_deref().unwrap(),
        )
        .env("TMUX_PANE", pane)
        .env_remove("THURBOX_CODEX_PICKER")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if picker {
        command.env("THURBOX_CODEX_PICKER", "1");
    }
    let mut child = command.spawn().unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(payload.to_string().as_bytes())
        .unwrap();
    child.wait_with_output().unwrap()
}

#[test]
fn codex_binding_requires_valid_event_and_pane_local_process() {
    let home = tempfile::tempdir().unwrap();
    let db = isolate(home.path());
    let mut row = codex_row("55555555-5555-4555-8555-555555555555");
    row.backend_id = "%42".into();
    db.upsert_session(&row).unwrap();
    let thread = "11111111-1111-4111-8111-111111111111";
    let payload = serde_json::json!({"session_id":thread,"source":"startup","hook_event_name":"SessionStart"});
    for invalid in [
        Value::Null,
        serde_json::json!(42),
        serde_json::json!("picker-required"),
        serde_json::json!(""),
        serde_json::json!("00000000-0000-0000-0000-000000000000"),
        serde_json::json!("ffffffff-ffff-ffff-ffff-ffffffffffff"),
    ] {
        let mut bad = payload.clone();
        bad["session_id"] = invalid;
        assert!(!hook_output(&row, bad, true, "%42", false).status.success());
        assert_eq!(
            db.get_session_meta(row.id, CODEX_THREAD_META).unwrap(),
            None
        );
    }
    let mut absent = payload.clone();
    absent.as_object_mut().unwrap().remove("hook_event_name");
    assert!(!hook_output(&row, absent, true, "%42", false)
        .status
        .success());
    let mut bad = payload.clone();
    bad["hook_event_name"] = serde_json::json!("Stop");
    assert!(!hook_output(&row, bad, true, "%42", false).status.success());
    for (no_daemon, pane) in [(false, "%42"), (true, "%99")] {
        let out = hook_output(&row, payload.clone(), no_daemon, pane, false);
        assert!(out.status.success());
        let result: Value = serde_json::from_slice(&out.stdout).unwrap();
        assert_eq!(result["bound"], false, "{result}");
        assert_eq!(result["reason"], "unproven_process");
        assert_eq!(
            db.get_session_meta(row.id, CODEX_THREAD_META).unwrap(),
            None
        );
    }
    let out = hook_output(&row, payload.clone(), true, "%42", false);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&out.stdout).unwrap()["bound"],
        true
    );
    assert_eq!(
        db.get_session_meta(row.id, CODEX_THREAD_META)
            .unwrap()
            .as_deref(),
        Some(thread)
    );
    // Inherited daemon identity must not invalidate an already proven binding.
    let mut other = payload.clone();
    other["session_id"] = serde_json::json!("22222222-2222-4222-8222-222222222222");
    hook_output(&row, other, false, "%42", false);
    assert_eq!(
        db.get_session_meta(row.id, CODEX_THREAD_META)
            .unwrap()
            .as_deref(),
        Some(thread)
    );
    db.set_session_meta(row.id, CODEX_THREAD_META, "picker-required")
        .unwrap();
    assert_eq!(
        serde_json::from_slice::<Value>(
            &hook_output(&row, payload.clone(), true, "%42", false).stdout
        )
        .unwrap()["bound"],
        false
    );
    let mut resume = payload;
    resume["source"] = serde_json::json!("resume");
    assert_eq!(
        serde_json::from_slice::<Value>(&hook_output(&row, resume, true, "%42", true).stdout)
            .unwrap()["bound"],
        true
    );
}

#[test]
fn nested_codex_cannot_bind_or_claim_the_resume_picker() {
    let home = tempfile::tempdir().unwrap();
    let db = isolate(home.path());
    let mut row = codex_row("55555555-5555-4555-8555-555555555555");
    row.backend_id = "%42".into();
    db.upsert_session(&row).unwrap();
    for existing in [None, Some("picker-required")] {
        if let Some(value) = existing {
            db.set_session_meta(row.id, CODEX_THREAD_META, value)
                .unwrap();
        }
        let mut child = codex_hook::nested_command(home.path())
            .env("THURBOX_SESSION", row.id.to_string())
            .env(
                "THURBOX_SESSION_ID",
                row.agent_session_id.as_deref().unwrap(),
            )
            .env("TMUX_PANE", "%42")
            .env("THURBOX_CODEX_PICKER", "1")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(br#"{"session_id":"11111111-1111-4111-8111-111111111111","source":"resume","hook_event_name":"SessionStart"}"#).unwrap();
        let out = child.wait_with_output().unwrap();
        assert!(out.status.success());
        assert_eq!(
            serde_json::from_slice::<Value>(&out.stdout).unwrap()["bound"],
            false
        );
        assert_eq!(
            db.get_session_meta(row.id, CODEX_THREAD_META)
                .unwrap()
                .as_deref(),
            existing
        );
    }
}

#[test]
fn a_node_launcher_is_not_a_second_codex_tui() {
    let home = tempfile::tempdir().unwrap();
    let db = isolate(home.path());
    let mut row = codex_row("55555555-5555-4555-8555-555555555555");
    row.backend_id = "%42".into();
    db.upsert_session(&row).unwrap();
    let mut child = codex_hook::node_launcher_command(home.path())
        .env("THURBOX_SESSION", row.id.to_string())
        .env(
            "THURBOX_SESSION_ID",
            row.agent_session_id.as_deref().unwrap(),
        )
        .env("TMUX_PANE", "%42")
        .env_remove("THURBOX_CODEX_PICKER")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(br#"{"session_id":"11111111-1111-4111-8111-111111111111","source":"startup","hook_event_name":"SessionStart"}"#).unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success());
    assert_eq!(
        serde_json::from_slice::<Value>(&out.stdout).unwrap()["bound"],
        true
    );
}
