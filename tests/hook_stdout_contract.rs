//! What a shipped hook payload leaves on **stdout**, not just what it signals.
//!
//! `thurbox-cli` auto-detects its output format from stdout, and a hook's
//! stdout is a pipe — so every `session signal` in a hook payload answers in
//! TOON (`Format::resolve_with(.., stdout_is_tty: false)`), the agent-facing
//! rendering. No agent asked for it, and every agent here reads it, in one of
//! two ways:
//!
//! - **codex** rejects a `Stop` hook whose stdout is not JSON outright —
//!   *"hook returned invalid stop hook JSON output"*, every turn. `{}` is the
//!   no-op decision it wants.
//! - claude, codex, grok and antigravity fold a hook's plain-text stdout into
//!   the model's context for their prompt/session events, and copilot reads it
//!   for its own decision keys — so the signal's own answer is billed to the
//!   user as developer context on every turn.
//!
//! Driven through the real binary against a real database: the format is
//! chosen from the *process's* stdout, so nothing below `main` can observe it
//! and a stub `thurbox-cli` (`tests/hook_turn_sequence.rs`) prints whatever the
//! stub prints.

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use serde_json::Value;
use thurbox::session::SessionId;
use thurbox::sync::SharedSession;

/// The keys codex's `Stop` hook output accepts. Its schema is
/// `deny_unknown_fields`, so anything else is the same hard failure as plain
/// text — `hookSpecificOutput`, which every other event takes, included.
const CODEX_STOP_KEYS: &[&str] = &[
    "decision",
    "reason",
    "systemMessage",
    "continue",
    "stopReason",
    "suppressOutput",
];

/// A throwaway thurbox instance whose `thurbox-cli` on `PATH` is the real
/// binary, so a hook command resolves to it exactly as it would in a session.
struct Env {
    root: tempfile::TempDir,
}

impl Env {
    fn new() -> Self {
        let root = tempfile::TempDir::new().expect("tempdir");
        for sub in ["home", "config", "data", "bin"] {
            std::fs::create_dir_all(root.path().join(sub)).expect("mkdir");
        }
        // A `thurbox-cli` on PATH: hook commands invoke it by bare name.
        let shim = root.path().join("bin").join("thurbox-cli");
        std::fs::write(
            &shim,
            format!(
                "#!/bin/sh\nexec {} \"$@\"\n",
                env!("CARGO_BIN_EXE_thurbox-cli")
            ),
        )
        .expect("write shim");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o755))
                .expect("chmod shim");
        }
        Self { root }
    }

    fn path(&self, sub: &str) -> PathBuf {
        self.root.path().join(sub)
    }

    fn db(&self) -> thurbox::storage::Database {
        thurbox::storage::Database::open(&self.path("data").join("thurbox.db"))
            .expect("open the instance database")
    }

    fn seed_session(&self, name: &str, agent: &str) -> SessionId {
        let row = SharedSession {
            id: SessionId::default(),
            name: name.into(),
            agent: agent.into(),
            backend_id: String::new(),
            backend_type: "local-tmux".into(),
            agent_session_id: None,
            cwd: None,
            additional_dirs: Vec::new(),
            worktrees: Vec::new(),
            shell_backend_id: None,
            parent_session_id: None,
            display_order: None,
            tombstone: false,
            tombstone_at: None,
        };
        self.db().upsert_session(&row).expect("persist");
        row.id
    }

    /// Run a hook command the way the agent does: through `sh -c`, with the
    /// event body on stdin and **stdout captured** — the pipe that decides the
    /// CLI's output format.
    fn fire(&self, command: &str, session: SessionId, body: &str) -> Output {
        let path = std::env::join_paths(std::iter::once(self.path("bin")).chain(
            std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()),
        ))
        .expect("join PATH");

        let mut child = Command::new("sh")
            .arg("-c")
            .arg(command)
            .env("PATH", path)
            .env("HOME", self.path("home"))
            .env("THURBOX_CONFIG_DIR", self.path("config"))
            .env("THURBOX_DATA_DIR", self.path("data"))
            .env("THURBOX_SESSION", session.to_string())
            .env_remove("THURBOX_SOCKET")
            .env_remove("THURBOX_SESSION_ID")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn hook");
        // Most of these commands never read stdin, so they can exit before the
        // write lands. That is not a failure — see `hook_turn_sequence::fire`.
        use std::io::Write;
        if let Err(err) = child
            .stdin
            .take()
            .expect("stdin")
            .write_all(body.as_bytes())
        {
            assert_eq!(
                err.kind(),
                std::io::ErrorKind::BrokenPipe,
                "write body: {err}"
            );
        }
        child.wait_with_output().expect("wait hook")
    }
}

fn payload(file: &str) -> Value {
    let text = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("extensions/hooks")
            .join(file),
    )
    .expect("read payload");
    serde_json::from_str(&text).expect("payload is valid JSON")
}

/// Every shell command an event's hook list carries, whichever schema the agent
/// wraps them in: claude-shaped payloads nest `hooks[].command`, copilot puts a
/// `bash`/`powershell` pair straight in the list. Only the POSIX half is run.
fn commands_for(value: &Value) -> Vec<String> {
    match value {
        Value::Object(map) => map
            .iter()
            .flat_map(|(key, v)| match (key.as_str(), v.as_str()) {
                ("command" | "bash", Some(c)) => vec![c.to_string()],
                ("powershell", Some(_)) => Vec::new(),
                _ => commands_for(v),
            })
            .collect(),
        Value::Array(items) => items.iter().flat_map(commands_for).collect(),
        _ => Vec::new(),
    }
}

fn body(event: &str) -> String {
    serde_json::json!({
        "session_id": "abc",
        "transcript_path": "/tmp/t.jsonl",
        "cwd": "/repo",
        "hook_event_name": event,
        "last_assistant_message": "done",
    })
    .to_string()
}

fn hook_state(env: &Env, id: SessionId) -> Option<String> {
    env.db()
        .load_hook_state(id)
        .expect("query hook state")
        .and_then(|row| row.state)
}

/// The reported bug: codex refuses the `Stop` hook's output every turn.
#[test]
#[cfg_attr(not(unix), ignore = "the payload's commands are POSIX shell")]
fn codex_stop_hook_prints_json_codex_accepts() {
    let env = Env::new();
    let id = env.seed_session("codex-session", "codex");
    let commands = commands_for(&payload("codex-hooks.json")["hooks"]["Stop"]);
    assert!(!commands.is_empty(), "codex payload registers a Stop hook");

    for command in &commands {
        let out = env.fire(command, id, &body("Stop"));
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(out.status.success(), "the Stop hook must exit 0: {stdout}");

        let doc: Value = serde_json::from_str(stdout.trim()).unwrap_or_else(|e| {
            panic!(
                "codex rejects a Stop hook whose stdout is not JSON \
                 (\"hook returned invalid stop hook JSON output\"); got ({e}):\n{stdout}"
            )
        });
        let object = doc
            .as_object()
            .unwrap_or_else(|| panic!("a Stop decision is a JSON object: {stdout}"));
        for key in object.keys() {
            assert!(
                CODEX_STOP_KEYS.contains(&key.as_str()),
                "codex's Stop schema is deny_unknown_fields; `{key}` is rejected: {stdout}"
            );
        }
    }

    // And the fix must not have silenced the thing the hook is for.
    assert_eq!(hook_state(&env, id).as_deref(), Some("done"));
}

/// The quieter half: a hook's plain-text stdout is folded into the model's
/// context by claude, codex, grok and antigravity, so a signal that answers on
/// stdout bills the user for its own receipt on every prompt and tool call.
/// copilot reads it for its own decision keys, which is the same argument.
#[test]
#[cfg_attr(not(unix), ignore = "the payloads' commands are POSIX shell")]
fn status_hooks_say_nothing_on_stdout() {
    let env = Env::new();
    for (file, events) in [
        (
            "codex-hooks.json",
            &["SessionStart", "UserPromptSubmit", "PreToolUse"][..],
        ),
        (
            "claude.json",
            &[
                "SessionStart",
                "UserPromptSubmit",
                "PreToolUse",
                "PostToolUse",
                "Stop",
            ][..],
        ),
        (
            "antigravity-hooks.json",
            &["SessionStart", "PreToolUse", "PostToolUse", "Stop"][..],
        ),
        (
            "grok-hooks.json",
            &[
                "SessionStart",
                "UserPromptSubmit",
                "PreToolUse",
                "PostToolUse",
                "Stop",
            ][..],
        ),
        (
            // copilot's own schema, and its own decision keys — a `session
            // signal` receipt on stdout is not one of them.
            "copilot-hooks.json",
            &[
                "sessionStart",
                "userPromptSubmitted",
                "preToolUse",
                "postToolUse",
                "agentStop",
            ][..],
        ),
    ] {
        let doc = payload(file);
        let id = env.seed_session(file, "claude");
        for event in events {
            let Some(hooks) = doc["hooks"].get(event) else {
                panic!("{file} registers {event}");
            };
            for command in commands_for(hooks) {
                let out = env.fire(&command, id, &body(event));
                assert!(
                    out.stdout.is_empty(),
                    "{file}/{event} writes to stdout, which the agent folds into \
                     the model's context:\n{}",
                    String::from_utf8_lossy(&out.stdout)
                );
                // Silence is the assertion above, and a command that is broken
                // shell is silent too — so say what the quiet has to mean.
                assert!(
                    out.status.success(),
                    "{file}/{event} exits non-zero:\n{}",
                    String::from_utf8_lossy(&out.stderr)
                );
            }
            assert!(
                hook_state(&env, id).is_some(),
                "{file}/{event} wrote nothing and signalled nothing"
            );
        }
    }
}
