//! What the shipped hook payloads report across a whole turn, not per event.
//!
//! The regression these guard: an agent fires a notification when it asks for
//! permission, and the payload turns that into `blocked` — but nothing put the
//! session back to `working` once the permission was granted. The next signal
//! was the *following* tool call, so a session that was told to go ahead stayed
//! red for the whole tool run (a long build, a test suite), and for a turn that
//! granted its last permission and then only wrote text, right up to the end.
//! A per-event assertion cannot see this: every single event maps to the right
//! state, and only the sequence is wrong.
//!
//! No agent has an "approved" event, so the edge back is whatever each one says
//! first once the prompt is answered — the tool completing, or (opencode) the
//! permission reply itself. Each is a real event name, verified against the
//! installed CLI rather than assumed.
//!
//! For the JSON payloads the hook commands are *run*, in order, with the event
//! body the agent would pipe in on stdin — claude's `case "$(cat)"` matcher
//! included, since whether a body reads as a permission prompt is half the
//! behaviour. The `thurbox-cli` they call is a stub that records the state
//! word. The script payloads (opencode, pi, omp) need their agent's own
//! runtime to run, so those are read instead.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn payload_path(file: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("extensions/hooks")
        .join(file)
}

fn payload_json(file: &str) -> serde_json::Value {
    let text = std::fs::read_to_string(payload_path(file)).expect("read payload");
    serde_json::from_str(&text).expect("payload is valid JSON")
}

/// A `thurbox-cli` that records `--state <s>` instead of writing a database,
/// first on `PATH` so the hook commands resolve to it.
fn stub_cli(dir: &Path) -> PathBuf {
    let log = dir.join("states");
    let bin = dir.join("thurbox-cli");
    std::fs::write(
        &bin,
        // `session signal --state <s>`: the state is the fourth argument.
        format!("#!/bin/sh\nprintf '%s\\n' \"$4\" >> {}\n", log.display()),
    )
    .expect("write stub");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    }
    log
}

/// Every shell command an event's hooks carry, whichever schema the agent
/// wraps them in: claude and antigravity nest `hooks[].command`, copilot puts
/// a `bash`/`powershell` pair straight in the list. Only the POSIX half is run.
fn commands_for(value: &serde_json::Value) -> Vec<String> {
    match value {
        serde_json::Value::Object(map) => map
            .iter()
            .flat_map(|(key, v)| match (key.as_str(), v.as_str()) {
                ("command" | "bash", Some(command)) => vec![command.to_string()],
                ("powershell", Some(_)) => Vec::new(),
                _ => commands_for(v),
            })
            .collect(),
        serde_json::Value::Array(items) => items.iter().flat_map(commands_for).collect(),
        _ => Vec::new(),
    }
}

/// Run every hook the payload registers for `event`, feeding it `body` on
/// stdin exactly as the agent does.
fn fire(payload: &serde_json::Value, dir: &Path, event: &str, body: &str) {
    let path = format!(
        "{}:{}",
        dir.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let Some(hooks) = payload["hooks"].get(event) else {
        return;
    };
    for command in commands_for(hooks) {
        let mut child = Command::new("sh")
            .arg("-c")
            .arg(&command)
            .env("PATH", &path)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .spawn()
            .expect("spawn hook");
        child
            .stdin
            .take()
            .expect("stdin")
            .write_all(body.as_bytes())
            .expect("write body");
        assert!(
            child.wait().expect("wait hook").success(),
            "{event} hook failed"
        );
    }
}

/// The state the dot would show right now: the last one signalled.
fn current(log: &Path) -> String {
    std::fs::read_to_string(log)
        .unwrap_or_default()
        .lines()
        .last()
        .unwrap_or("<none>")
        .to_string()
}

fn body(event: &str, message: &str) -> String {
    serde_json::json!({
        "session_id": "abc",
        "transcript_path": "/tmp/t.jsonl",
        "cwd": "/repo",
        "hook_event_name": event,
        "message": message,
    })
    .to_string()
}

/// Each JSON-payload agent's event names, in the order one turn fires them:
/// prompt (claude and copilot only), tool call, permission prompt, the tool
/// completing, end of turn.
const TURNS: &[(&str, &str, [&str; 5])] = &[
    (
        "claude",
        "claude.json",
        [
            "UserPromptSubmit",
            "PreToolUse",
            "Notification",
            "PostToolUse",
            "Stop",
        ],
    ),
    (
        // agy adopted claude's schema, minus UserPromptSubmit.
        "antigravity",
        "antigravity-hooks.json",
        ["", "PreToolUse", "Notification", "PostToolUse", "Stop"],
    ),
    (
        // copilot matches `permission_prompt` itself, so its notification hook
        // signals blocked unconditionally.
        "copilot",
        "copilot-hooks.json",
        [
            "userPromptSubmitted",
            "preToolUse",
            "notification",
            "postToolUse",
            "agentStop",
        ],
    ),
];

/// A turn that asks for permission, is granted it, and keeps going.
#[test]
fn granting_a_permission_puts_the_session_back_to_working() {
    for (agent, file, [prompt, pre, notify, post, stop]) in TURNS {
        let dir = tempfile::tempdir().expect("tempdir");
        let payload = payload_json(file);
        let log = stub_cli(dir.path());

        fire(&payload, dir.path(), prompt, &body(prompt, ""));
        fire(&payload, dir.path(), pre, &body(pre, ""));
        assert_eq!(current(&log), "working", "{agent}: a tool call is work");

        fire(
            &payload,
            dir.path(),
            notify,
            &body(notify, "Claude needs your permission to use Bash"),
        );
        assert_eq!(current(&log), "blocked", "{agent}: a prompt is a block");

        // The user approves and the tool runs to completion. Whatever the agent
        // does next — another tool, minutes of output, or just prose until the
        // turn ends — it is no longer waiting on anyone.
        fire(&payload, dir.path(), post, &body(post, ""));
        assert_eq!(
            current(&log),
            "working",
            "{agent}: granted permission left the session blocked"
        );

        fire(&payload, dir.path(), stop, &body(stop, ""));
        assert_eq!(current(&log), "done", "{agent}: the turn ended");
    }
}

/// The other reason claude fires a notification: nobody has typed for 60s.
/// That is the session at rest, not a block, and the payload's own matcher is
/// the only thing that tells the two apart (copilot's agent matches for it).
#[test]
fn the_idle_nudge_is_not_a_block() {
    for file in ["claude.json", "antigravity-hooks.json"] {
        let dir = tempfile::tempdir().expect("tempdir");
        let payload = payload_json(file);
        let log = stub_cli(dir.path());

        fire(&payload, dir.path(), "PreToolUse", &body("PreToolUse", ""));
        fire(
            &payload,
            dir.path(),
            "Notification",
            &body("Notification", "Claude is waiting for your input"),
        );
        assert_eq!(current(&log), "working", "{file}: the nudge blocked");
    }
}

/// The same edge in the payloads that are code: each subscribes to the event
/// that ends its block and reports `working` from it. They need their agent's
/// runtime to run, so the pairing is read — the handler body immediately
/// following the event name, which is where the state word lives in all three.
#[test]
fn the_script_payloads_report_working_when_the_block_clears() {
    // (payload, the event that resolves a block, why it resolves one)
    let cases = [
        // opencode alone has a real permission-reply event.
        ("opencode-status.js", "\"permission.replied\""),
        // pi and omp block on a question *tool*, so the answer arriving is
        // that tool completing.
        ("pi-status.ts", "\"tool_execution_end\""),
        ("omp-status.ts", "\"tool_execution_end\""),
    ];
    for (file, event) in cases {
        let text = std::fs::read_to_string(payload_path(file)).expect("read payload");
        let at = text
            .find(event)
            .unwrap_or_else(|| panic!("{file} never subscribes to {event}"));
        let handler = &text[at + event.len()..];
        let end = handler.find('\n').unwrap_or(handler.len());
        assert!(
            handler[..end].contains("\"working\""),
            "{file}: {event} does not report working"
        );
    }
}
