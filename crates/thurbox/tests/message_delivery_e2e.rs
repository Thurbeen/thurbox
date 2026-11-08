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
use thurbox::session::SharedSession;
use thurbox::storage::Database;

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
    // Codex queues the body as the thread's next turn; nothing asks first.
    assert_eq!(before["may_hold"], false, "{before}");

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

#[test]
fn codex_hook_waits_for_the_restarted_pane_to_be_published() {
    let home = tempfile::tempdir().unwrap();
    let db = isolate(home.path());
    let mut row = codex_row("55555555-5555-4555-8555-555555555555");
    row.backend_id = "%41".into();
    db.upsert_session(&row).unwrap();
    db.set_session_meta(row.id, CODEX_THREAD_META, "picker-required")
        .unwrap();
    let mut child = codex_hook::command(home.path())
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
    // The old row remains visible while the replacement process starts.
    std::thread::sleep(std::time::Duration::from_millis(300));
    row.backend_id = "%42".into();
    db.upsert_session(&row).unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success());
    let result: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(result["bound"], true, "{result}");
    assert_eq!(
        db.get_session_meta(row.id, CODEX_THREAD_META)
            .unwrap()
            .as_deref(),
        Some("11111111-1111-4111-8111-111111111111")
    );
}

/// What a running Claude Code leaves behind for session `row`: a process
/// carrying its identity, an inbox socket, and a registry entry naming both.
/// Returns the process and a reader that yields the first line the socket gets.
#[cfg(target_os = "linux")]
fn stand_in_claude(
    home: &Path,
    registry: &Path,
    row: &SharedSession,
) -> (std::process::Child, std::thread::JoinHandle<String>) {
    use std::io::BufRead;
    use std::os::unix::net::UnixListener;

    let socket = home.join("inbox.sock");
    let listener = UnixListener::bind(&socket).expect("bind inbox");
    let reader = std::thread::spawn(move || {
        let (stream, _) = listener.accept().expect("accept");
        let mut line = String::new();
        let _ = std::io::BufReader::new(stream).read_line(&mut line);
        line
    });
    let agent = Command::new("sleep")
        .arg("60")
        .env("THURBOX_SESSION", row.id.to_string())
        .spawn()
        .expect("spawn stand-in agent");
    std::fs::create_dir_all(registry).expect("mkdir registry");
    std::fs::write(
        registry.join(format!("{}.json", agent.id())),
        serde_json::json!({
            "pid": agent.id(),
            "kind": "interactive",
            "messagingSocketPath": socket,
            "startedAt": 1,
        })
        .to_string(),
    )
    .expect("write registry entry");
    (agent, reader)
}

#[cfg(target_os = "linux")]
fn claude_row(agent_session_id: &str) -> SharedSession {
    SharedSession {
        agent: "claude".into(),
        ..codex_row(agent_session_id)
    }
}

/// A send over a Claude inbox socket is reported as one that may be held
/// (issue #1521): the socket takes the body, and Claude Code can then hold it
/// behind an approval dialog unless the recipient accepts cross-session input.
/// The field describes the route; thurbox cannot see the dialog.
#[cfg(target_os = "linux")]
#[test]
fn a_claude_socket_send_says_it_may_be_held() {
    let home = tempfile::tempdir().expect("tempdir");
    let db = isolate(home.path());
    let row = claude_row("33333333-3333-4333-8333-333333333333");
    db.upsert_session(&row).expect("insert row");
    let (mut agent, reader) =
        stand_in_claude(home.path(), &home.path().join("claude/sessions"), &row);

    let path = std::env::var_os("PATH").unwrap_or_default();
    let sent = send(
        &row,
        "are you there",
        &path,
        &home.path().join("unused.log"),
    );
    let _ = agent.kill();
    let _ = agent.wait();

    assert_eq!(sent["delivered_via"], "claude-socket", "{sent}");
    assert_eq!(sent["may_hold"], true, "{sent}");
    assert!(
        reader.join().expect("reader").contains("are you there"),
        "the body reached the socket"
    );
}

/// The text form says so too, naming the setting that removes the hold.
#[cfg(target_os = "linux")]
#[test]
fn a_claude_socket_send_says_so_in_text() {
    let home = tempfile::tempdir().expect("tempdir");
    let db = isolate(home.path());
    let row = claude_row("44444444-4444-4444-8444-444444444444");
    db.upsert_session(&row).expect("insert row");
    let (mut agent, reader) =
        stand_in_claude(home.path(), &home.path().join("claude/sessions"), &row);

    let out = Command::new(env!("CARGO_BIN_EXE_thurbox-cli"))
        .args(["message", "send", "--to", &row.name, "--kind", "note"])
        .args(["--body", "hello", "--text"])
        .env_remove("THURBOX_SESSION")
        .output()
        .expect("run message send");
    let _ = agent.kill();
    let _ = agent.wait();

    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(text.contains("claude-socket"), "{text}");
    assert!(text.contains("approval"), "{text}");
    assert!(text.contains("crossSessionInbound"), "{text}");
    // Joined last: it returns only once the socket was written to.
    let _ = reader.join();
}

/// A Claude whose launcher sets its own `CLAUDE_CONFIG_DIR`, reached by a
/// sender that has none. Its `SessionStart` hook runs before Claude Code writes
/// its registry entry, and no other hook runs until a first message arrives, so
/// what that one signal records is all a sender ever learns of where to look.
#[cfg(target_os = "linux")]
#[test]
fn a_claude_signalled_before_its_registry_entry_is_still_reachable() {
    let home = tempfile::tempdir().expect("tempdir");
    let db = isolate(home.path());
    let row = claude_row("66666666-6666-4666-8666-666666666666");
    db.upsert_session(&row).expect("insert row");
    let theirs = home.path().join("their-claude");

    let signal = Command::new(env!("CARGO_BIN_EXE_thurbox-cli"))
        .args(["session", "signal", "--state", "idle"])
        .env("THURBOX_SESSION", row.id.to_string())
        .env("CLAUDE_CONFIG_DIR", &theirs)
        .env(
            "CLAUDE_CODE_MESSAGING_SOCKET",
            home.path().join("inbox.sock"),
        )
        .stdin(Stdio::null())
        .output()
        .expect("run session signal");
    assert!(
        signal.status.success(),
        "session signal failed: {}",
        String::from_utf8_lossy(&signal.stderr)
    );
    let (mut agent, reader) = stand_in_claude(home.path(), &theirs.join("sessions"), &row);

    let path = std::env::var_os("PATH").unwrap_or_default();
    let sent = send(&row, "your brief", &path, &home.path().join("unused.log"));
    let delivered = sent["delivered_via"] == "claude-socket";
    if !delivered {
        // Unblock the reader, which otherwise waits for a connection forever.
        let _ = std::os::unix::net::UnixStream::connect(home.path().join("inbox.sock"));
    }
    let _ = agent.kill();
    let _ = agent.wait();

    assert!(delivered, "{sent}");
    assert!(reader.join().expect("reader").contains("your brief"));
}

/// A send from no thurbox session (a script, a control plane such as a task
/// manager) says so in its provenance line. Claude Code frames every socket
/// message as coming from another Claude session, so "unknown sender" there
/// reads as an unidentified peer agent rather than a process outside thurbox.
#[cfg(target_os = "linux")]
#[test]
fn a_send_from_outside_any_session_names_no_peer() {
    let home = tempfile::tempdir().expect("tempdir");
    let db = isolate(home.path());
    let row = claude_row("77777777-7777-4777-8777-777777777777");
    db.upsert_session(&row).expect("insert row");
    let (mut agent, reader) =
        stand_in_claude(home.path(), &home.path().join("claude/sessions"), &row);

    let path = std::env::var_os("PATH").unwrap_or_default();
    let sent = send(&row, "notice", &path, &home.path().join("unused.log"));
    let _ = agent.kill();
    let _ = agent.wait();

    assert_eq!(sent["delivered_via"], "claude-socket", "{sent}");
    let line = reader.join().expect("reader");
    assert!(!line.contains("unknown sender"), "{line}");
    assert!(
        line.contains("from: a process outside thurbox, not a session"),
        "{line}"
    );
}

/// A caller whose `THURBOX_SESSION` names no session here (a remote peer's
/// session, whose row lives in another thurbox's database) is a session all
/// the same, so it must not be labelled as a process outside thurbox.
#[cfg(target_os = "linux")]
#[test]
fn a_send_from_a_session_unknown_here_is_not_called_outside_thurbox() {
    let home = tempfile::tempdir().expect("tempdir");
    let db = isolate(home.path());
    let row = claude_row("88888888-8888-4888-8888-888888888888");
    db.upsert_session(&row).expect("insert row");
    let (mut agent, reader) =
        stand_in_claude(home.path(), &home.path().join("claude/sessions"), &row);

    let out = Command::new(env!("CARGO_BIN_EXE_thurbox-cli"))
        .args(["message", "send", "--to", &row.name, "--kind", "note"])
        .args(["--body", "from a peer", "--json"])
        .env("THURBOX_SESSION", "99999999-9999-4999-8999-999999999999")
        .output()
        .expect("run message send");
    let _ = agent.kill();
    let _ = agent.wait();

    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let line = reader.join().expect("reader");
    assert!(!line.contains("outside thurbox"), "{line}");
    assert!(
        line.contains("from: a session this thurbox has no record of"),
        "{line}"
    );
}

/// `message send` with `args` and `--json` against the isolated database,
/// whatever it exits with.
fn send_with(to: &str, args: &[&str], path: &std::ffi::OsStr, log: &Path) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_thurbox-cli"))
        .args(["message", "send", "--to", to, "--kind", "note", "--json"])
        .args(args)
        .env("PATH", path)
        .env("FAKE_CODEX_LOG", log)
        .env_remove("THURBOX_SESSION")
        .output()
        .expect("run message send")
}

/// A send that reached only the mailbox says so in a field of its own (issue
/// #1643): a sender must not have to know that one value of `delivered_via`
/// means nobody was told. `--require-native` makes it a failed command, with
/// the message still enqueued for a retry or a drain.
#[test]
fn a_send_that_reached_only_the_mailbox_says_it_was_not_delivered() {
    let home = tempfile::tempdir().expect("tempdir");
    let db = isolate(home.path());
    let log = home.path().join("codex-queue.log");
    let path = fake_codex(&home.path().join("bin"));
    let row = codex_row("aaaaaaaa-1111-4111-8111-111111111111");
    db.upsert_session(&row).expect("insert row");

    let plain = send(&row, "nobody reads this", &path, &log);
    assert_eq!(plain["delivered"], false, "{plain}");

    let out = send_with(
        &row.name,
        &["--body", "or this", "--require-native"],
        &path,
        &log,
    );
    assert!(
        !out.status.success(),
        "a mailbox-only send must fail under --require-native"
    );
    let doc: Value = serde_json::from_slice(&out.stdout).expect("the answer is still printed");
    assert_eq!(doc["delivered"], false, "{doc}");
    assert_eq!(doc["enqueued"], true, "{doc}");
    let unread = db.list_messages(row.id, true, None).expect("list unread");
    assert_eq!(unread.len(), 2, "both bodies wait in the mailbox");

    codex_session_start(&row, "bbbbbbbb-2222-4222-8222-222222222222", "startup");
    let out = send_with(
        &row.name,
        &["--body", "now bound", "--require-native"],
        &path,
        &log,
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
    let doc: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(doc["delivered"], true, "{doc}");
}

/// A Codex session is sent its first prompt moments after it is spawned,
/// before its `SessionStart` hook has bound a thread. `--wait-bound` holds the
/// send until the binding lands and then queues it on that thread, instead of
/// leaving the brief in a mailbox nobody reads.
#[test]
fn a_send_waiting_for_the_binding_reaches_a_codex_session_that_binds_later() {
    let home = tempfile::tempdir().expect("tempdir");
    let db = isolate(home.path());
    let log = home.path().join("codex-queue.log");
    let path = fake_codex(&home.path().join("bin"));
    let row = codex_row("cccccccc-3333-4333-8333-333333333333");
    db.upsert_session(&row).expect("insert row");

    let sender = Command::new(env!("CARGO_BIN_EXE_thurbox-cli"))
        .args(["message", "send", "--to", &row.name, "--kind", "brief"])
        .args(["--body", "your first job", "--wait-bound", "30", "--json"])
        .env("PATH", &path)
        .env("FAKE_CODEX_LOG", &log)
        .env_remove("THURBOX_SESSION")
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn message send");
    std::thread::sleep(std::time::Duration::from_millis(500));
    let thread = "dddddddd-4444-4444-8444-444444444444";
    codex_session_start(&row, thread, "startup");

    let out = sender.wait_with_output().expect("message send");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
    let doc: Value = serde_json::from_slice(&out.stdout).expect("send --json");
    assert_eq!(doc["delivered_via"], "codex-queue", "{doc}");
    assert_eq!(doc["delivered"], true, "{doc}");
    let queued = std::fs::read_to_string(&log).unwrap_or_default();
    assert_eq!(queued.lines().collect::<Vec<_>>(), [thread]);
}

/// A session on a host that shares its sessions is reached through the host's
/// own `thurbox-cli` (ADR-24): its agent's inbox, and the mailbox it drains, are
/// on that host, where the binding was recorded. Nothing is faked but `ssh`,
/// which runs the real CLI against the host's data directory, and `codex`.
#[test]
fn a_send_to_a_session_on_a_sharing_host_is_delivered_there() {
    use std::os::unix::fs::PermissionsExt;

    let root = tempfile::tempdir().expect("tempdir");
    let path = |sub: &str| root.path().join(sub);
    for sub in [
        "bin",
        "home",
        "config",
        "data",
        "host-bin",
        "host-home",
        "host-config",
        "host-data",
    ] {
        std::fs::create_dir_all(path(sub)).expect("mkdir");
    }
    let settings = "[features]\nautomations = false\nversion_check = false\nauto_update = false\n";
    std::fs::write(path("config/settings.toml"), settings).unwrap();
    std::fs::write(path("host-config/settings.toml"), settings).unwrap();
    std::fs::write(
        path("config/hosts.toml"),
        "[[hosts]]\nname = 'probe'\ndestination = 'probe'\nshare_sessions = true\n",
    )
    .unwrap();
    let cli_dir = Path::new(env!("CARGO_BIN_EXE_thurbox-cli"))
        .parent()
        .unwrap();
    let ssh = path("bin/ssh");
    std::fs::write(
        &ssh,
        format!(
            "#!/bin/sh\n\
             while [ $# -gt 0 ]; do case \"$1\" in -o) shift 2 ;; -*) shift ;; *) shift; break ;; esac; done\n\
             printf '%s\\n' \"$*\" >> '{root}/ssh.log'\n\
             export HOME='{root}/host-home' THURBOX_CONFIG_DIR='{root}/host-config' THURBOX_DATA_DIR='{root}/host-data'\n\
             export CLAUDE_CONFIG_DIR='{root}/host-home/claude'\n\
             export PATH='{root}/host-bin':'{cli}':/usr/bin:/bin\n\
             unset THURBOX_SESSION THURBOX_SESSION_ID THURBOX_SOCKET THURBOX_SOCKET_FOR\n\
             exec sh -c \"$*\"\n",
            root = root.path().display(),
            cli = cli_dir.display(),
        ),
    )
    .unwrap();
    std::fs::set_permissions(&ssh, std::fs::Permissions::from_mode(0o755)).unwrap();
    let log = path("host-codex.log");
    let codex = path("host-bin/codex");
    std::fs::write(
        &codex,
        format!("#!/bin/sh\nprintf '%s\\n' \"$3\" >> '{}'\n", log.display()),
    )
    .unwrap();
    std::fs::set_permissions(&codex, std::fs::Permissions::from_mode(0o755)).unwrap();

    // The host's record, bound by its own hook; the caller's mirror of it.
    let thread = "eeeeeeee-5555-4555-8555-555555555555";
    let on_host = codex_row("ffffffff-6666-4666-8666-666666666666");
    let host_db = Database::open(&path("host-data/thurbox.db")).expect("host db");
    host_db.upsert_session(&on_host).unwrap();
    host_db
        .set_session_meta(on_host.id, CODEX_THREAD_META, thread)
        .unwrap();
    let caller_db = Database::open(&path("data/thurbox.db")).expect("caller db");
    caller_db
        .upsert_session(&SharedSession {
            backend_type: "ssh:probe".into(),
            ..on_host.clone()
        })
        .unwrap();

    let out = Command::new(env!("CARGO_BIN_EXE_thurbox-cli"))
        .args([
            "message",
            "send",
            "--to",
            &on_host.id.to_string(),
            "--kind",
            "brief",
        ])
        .args(["--body=--across the wire", "--require-native", "--json"])
        .env("HOME", path("home"))
        .env("THURBOX_CONFIG_DIR", path("config"))
        .env("THURBOX_DATA_DIR", path("data"))
        .env("CLAUDE_CONFIG_DIR", path("home/claude"))
        .env(
            "PATH",
            format!(
                "{}:{}",
                path("bin").display(),
                std::env::var("PATH").unwrap()
            ),
        )
        .env_remove("THURBOX_SESSION")
        .env_remove("THURBOX_SESSION_ID")
        .output()
        .expect("run message send");
    let doc: Value = serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
        panic!(
            "send --json ({e}): {}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        )
    });
    assert!(out.status.success(), "{doc}");
    assert_eq!(doc["delivered_via"], "codex-queue", "{doc}");
    assert_eq!(doc["delivered"], true, "{doc}");
    assert_eq!(doc["host"], "probe", "{doc}");
    let queued = std::fs::read_to_string(&log).unwrap_or_default();
    assert_eq!(queued.lines().collect::<Vec<_>>(), [thread]);
    // The host's mailbox holds the record; the caller's holds no stray copy.
    let host_rows = host_db.list_messages(on_host.id, false, None).unwrap();
    assert_eq!(host_rows.len(), 1);
    assert_eq!(host_rows[0].delivered_via.as_deref(), Some("codex-queue"));
    assert_eq!(host_rows[0].body, "--across the wire");
    // Over stdin, not the command line: a Windows host's shell takes 8191
    // characters, and `ps` on either machine shows a command line to everyone.
    let link = std::fs::read_to_string(path("ssh.log")).unwrap_or_default();
    assert!(
        !link.contains("across the wire"),
        "the body crossed as an argument:\n{link}"
    );
    assert!(caller_db
        .list_messages(on_host.id, false, None)
        .unwrap()
        .is_empty());
}
