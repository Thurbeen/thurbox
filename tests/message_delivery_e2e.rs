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
    }
}

/// A `codex` on `PATH` that records the thread of each `queue --thread <id>`
/// it is asked for and accepts it, as the real one does for any thread with a
/// rollout.
fn fake_codex(bin: &Path, log: &Path) -> std::ffi::OsString {
    use std::os::unix::fs::PermissionsExt;

    std::fs::create_dir_all(bin).expect("mkdir bin");
    let fake = bin.join("codex");
    std::fs::write(
        &fake,
        format!("#!/bin/sh\nprintf '%s\\n' \"$3\" >> '{}'\n", log.display()),
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
    let mut hook = Command::new(env!("CARGO_BIN_EXE_thurbox-cli"))
        .args(["session", "bind-codex"])
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
        "{{\"session_id\":\"{conversation}\",\"source\":\"{source}\"}}"
    )
    .unwrap();
    assert!(hook.wait().unwrap().success(), "bind-codex failed");
}

fn send(to: &SharedSession, body: &str, path: &std::ffi::OsStr) -> Value {
    let out = Command::new(env!("CARGO_BIN_EXE_thurbox-cli"))
        .args(["message", "send", "--to", &to.name, "--kind", "note"])
        .args(["--body", body, "--json"])
        .env("PATH", path)
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
    let path = fake_codex(&home.path().join("bin"), &log);

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

    let before = send(&row, "before /new", &path);
    assert_eq!(before["delivered_via"], "codex-queue", "{before}");

    let second = "22222222-2222-4222-8222-222222222222";
    codex_session_start(&row, second, "startup");
    let after = send(&row, "after /new", &path);

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
