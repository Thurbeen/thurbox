//! The `background` state, end to end through the shipped claude hooks (issue
//! #1510).
//!
//! A claude turn can end while an Agent-tool subagent it started in the
//! background is still running. Read as `done`, the session is torn down or
//! handed new work too early. The order these tests replay is the one the
//! issue's reproduction recorded on Claude Code 2.1.303, three runs alike, and
//! that a headless run on 2.1.308 recorded again:
//!
//! ```text
//! SubagentStart  agent_type=general-purpose agent_id=…
//! Stop
//! SubagentStop   agent_type=general-purpose agent_id=…
//! Stop
//! ```
//!
//! Driven through the real payload and the real binary against a real
//! database: each hook command runs under `sh -c` with the event body on
//! stdin, and the `thurbox-cli` it calls is this build's, so what is asserted
//! is what a session's own hooks would write.
#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use serde_json::{json, Value};
use thurbox::session::SessionId;
use thurbox::storage::Database;
use thurbox::sync::SharedSession;

/// The subagent of the issue's reproduction.
const SUBAGENT: &str = "a2a3df31469ca24ca";

struct Env {
    root: tempfile::TempDir,
    id: SessionId,
}

impl Env {
    fn new(agent: &str) -> Self {
        let root = tempfile::TempDir::new().expect("tempdir");
        for sub in ["home", "config", "data", "bin"] {
            std::fs::create_dir_all(root.path().join(sub)).expect("mkdir");
        }
        let shim = root.path().join("bin").join("thurbox-cli");
        std::fs::write(
            &shim,
            format!(
                "#!/bin/sh\nexec {} \"$@\"\n",
                env!("CARGO_BIN_EXE_thurbox-cli")
            ),
        )
        .expect("write shim");
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        std::fs::write(
            root.path().join("config/settings.toml"),
            "[features]\nautomations = false\nversion_check = false\nauto_update = false\n",
        )
        .expect("settings");
        let env = Self {
            root,
            id: SessionId::default(),
        };
        let row = SharedSession {
            id: env.id,
            name: "worker".into(),
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
            key: None,
        };
        env.db().upsert_session(&row).expect("persist");
        env
    }

    fn path(&self, sub: &str) -> PathBuf {
        self.root.path().join(sub)
    }

    fn db(&self) -> Database {
        Database::open(&self.path("data").join("thurbox.db")).expect("open")
    }

    fn command(&self, program: &str) -> Command {
        let path = std::env::join_paths(std::iter::once(self.path("bin")).chain(
            std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()),
        ))
        .expect("join PATH");
        let mut cmd = Command::new(program);
        cmd.env("PATH", path)
            .env("HOME", self.path("home"))
            .env("THURBOX_CONFIG_DIR", self.path("config"))
            .env("THURBOX_DATA_DIR", self.path("data"))
            .env("THURBOX_SESSION", self.id.to_string())
            .env_remove("THURBOX_SOCKET")
            .env_remove("THURBOX_SOCKET_FOR")
            .env_remove("THURBOX_SESSION_ID")
            .env_remove("THURBOX_SIGNAL_WORK");
        cmd
    }

    /// Run every hook `file` registers for `event`, the way the agent does.
    fn fire(&self, file: &str, event: &str, body: Value) {
        let payload = payload(file);
        let body = serde_json::to_string(&body).expect("body");
        for command in commands_for(&payload["hooks"][event]) {
            let mut child = self
                .command("sh")
                .arg("-c")
                .arg(&command)
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .expect("spawn hook");
            // A command that never reads stdin may exit before the write
            // lands; that is not a failure.
            use std::io::Write;
            let _ = child
                .stdin
                .take()
                .expect("stdin")
                .write_all(body.as_bytes());
            assert!(child.wait().expect("wait hook").success(), "{command}");
        }
    }

    fn claude(&self, event: &str, body: Value) {
        self.fire("claude.json", event, body);
    }

    /// The stored word, verbatim.
    fn hook_state(&self) -> Option<String> {
        self.db()
            .load_hook_state(self.id)
            .expect("hook state")
            .and_then(|row| row.state)
    }

    /// The word `session get` answers with.
    fn state(&self) -> String {
        let out = self
            .command(env!("CARGO_BIN_EXE_thurbox-cli"))
            .args(["--json", "session", "get", "worker", "--no-verify"])
            .output()
            .expect("session get");
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let value: Value = serde_json::from_slice(&out.stdout).expect("json");
        value["state"].as_str().expect("state").to_string()
    }

    /// Every state transition the event log holds, as `from → to`.
    fn transitions(&self) -> Vec<(Option<String>, Option<String>)> {
        self.db()
            .session_events_since(0, Some(self.id), 1000)
            .expect("events")
            .into_iter()
            .filter(|e| e.reason == "state")
            .map(|e| (e.from_state, e.to_state))
            .collect()
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

/// An event body as claude pipes it: compact JSON, top-level fields first.
fn event(name: &str, extra: Value) -> Value {
    let mut body = json!({
        "session_id": "conversation",
        "transcript_path": "/home/user/.claude/projects/p/conversation.jsonl",
        "cwd": "/repo",
        "hook_event_name": name,
    });
    for (key, value) in extra.as_object().expect("object") {
        body[key] = value.clone();
    }
    body
}

fn typed(name: &str, agent_id: &str) -> Value {
    event(
        name,
        json!({ "agent_id": agent_id, "agent_type": "general-purpose" }),
    )
}

/// The main agent's own tool call: no `agent_id` at the top level.
fn main_tool(name: &str, tool: &str) -> Value {
    event(
        name,
        json!({ "tool_name": tool, "tool_input": { "description": "x" } }),
    )
}

/// A typed subagent's tool call.
fn subagent_tool(name: &str, agent_id: &str) -> Value {
    event(
        name,
        json!({
            "agent_id": agent_id,
            "agent_type": "general-purpose",
            "tool_name": "Bash",
            "tool_input": { "command": "sleep 30" },
        }),
    )
}

/// The main turn that launches a background subagent and ends at once.
fn turn_that_starts_a_background_subagent(env: &Env, agent_id: &str) {
    env.claude("UserPromptSubmit", event("UserPromptSubmit", json!({})));
    env.claude("PreToolUse", main_tool("PreToolUse", "Agent"));
    env.claude("SubagentStart", typed("SubagentStart", agent_id));
    env.claude("PostToolUse", main_tool("PostToolUse", "Agent"));
    env.claude("Stop", event("Stop", json!({})));
}

#[test]
fn a_turn_that_ends_with_a_subagent_running_is_background_until_it_stops() {
    let env = Env::new("claude");
    env.claude("SessionStart", event("SessionStart", json!({})));
    turn_that_starts_a_background_subagent(&env, SUBAGENT);

    assert_eq!(env.hook_state().as_deref(), Some("background"));
    assert_eq!(env.state(), "background");

    // A driver waiting for the session to finish is still waiting.
    let mut wait = env
        .command(env!("CARGO_BIN_EXE_thurbox-cli"))
        .args([
            "--json",
            "session",
            "wait",
            "worker",
            "--state",
            "done",
            "--timeout",
            "60",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn wait");
    std::thread::sleep(Duration::from_millis(500));
    assert!(
        wait.try_wait().expect("poll wait").is_none(),
        "session wait --state done returned while the subagent was still running"
    );

    // The subagent's own work keeps the resting turn where it is: it is not a
    // new turn, and it is not the session coming back to `working`.
    env.claude("PreToolUse", subagent_tool("PreToolUse", SUBAGENT));
    env.claude("PostToolUse", subagent_tool("PostToolUse", SUBAGENT));
    assert_eq!(env.hook_state().as_deref(), Some("background"));

    env.claude("SubagentStop", typed("SubagentStop", SUBAGENT));
    assert_eq!(env.hook_state().as_deref(), Some("done"));

    let out = wait.wait_with_output().expect("wait");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let answer: Value = serde_json::from_slice(&out.stdout).expect("wait json");
    assert_eq!(answer["state"], "done", "{answer}");

    // The edge is in the log `watch` streams.
    let transitions = env.transitions();
    assert!(
        transitions.contains(&(Some("background".into()), Some("done".into()))),
        "{transitions:?}"
    );

    // Claude wakes the main agent to report back, which ends with a `Stop`.
    env.claude("Stop", event("Stop", json!({})));
    assert_eq!(env.hook_state().as_deref(), Some("done"));
}

#[test]
fn the_last_of_several_subagents_ends_the_background() {
    let env = Env::new("claude");
    env.claude("UserPromptSubmit", event("UserPromptSubmit", json!({})));
    env.claude("SubagentStart", typed("SubagentStart", "first"));
    env.claude("SubagentStart", typed("SubagentStart", "second"));
    env.claude("Stop", event("Stop", json!({})));
    assert_eq!(env.hook_state().as_deref(), Some("background"));

    env.claude("SubagentStop", typed("SubagentStop", "first"));
    // The wake-up turn that reports the first one back. Its own tool calls are
    // not a new prompt, so the second subagent stays counted through them.
    env.claude("PreToolUse", main_tool("PreToolUse", "Read"));
    env.claude("PostToolUse", main_tool("PostToolUse", "Read"));
    assert_eq!(env.hook_state().as_deref(), Some("working"));
    env.claude("Stop", event("Stop", json!({})));
    assert_eq!(env.hook_state().as_deref(), Some("background"));

    env.claude("SubagentStop", typed("SubagentStop", "second"));
    assert_eq!(env.hook_state().as_deref(), Some("done"));
}

/// A hook delivered twice, or two hook processes racing so that a stop lands
/// before its own start, must not leave the session counting work that ended.
#[test]
fn duplicate_and_out_of_order_edges_do_not_strand_the_session() {
    let env = Env::new("claude");
    env.claude("UserPromptSubmit", event("UserPromptSubmit", json!({})));
    env.claude("SubagentStart", typed("SubagentStart", "twice"));
    env.claude("SubagentStart", typed("SubagentStart", "twice"));
    env.claude("SubagentStop", typed("SubagentStop", "early"));
    env.claude("SubagentStart", typed("SubagentStart", "early"));
    env.claude("Stop", event("Stop", json!({})));
    assert_eq!(env.hook_state().as_deref(), Some("background"));

    // One stop ends the twice-started subagent; the early one never counted.
    env.claude("SubagentStop", typed("SubagentStop", "twice"));
    assert_eq!(env.hook_state().as_deref(), Some("done"));
    env.claude("SubagentStop", typed("SubagentStop", "twice"));
    assert_eq!(env.hook_state().as_deref(), Some("done"));
    let transitions = env.transitions();
    let done_edges = transitions
        .iter()
        .filter(|(from, to)| from.as_deref() == Some("background") && to.as_deref() == Some("done"))
        .count();
    assert_eq!(done_edges, 1, "{transitions:?}");
}

/// The #1428 case: claude's own post-turn agent carries an `agent_id` and no
/// `agent_type`, runs tool calls after `Stop`, and fires no end anything waits
/// on. It must never make a finished session read anything but `done`.
#[test]
fn claudes_own_post_turn_agent_never_produces_background() {
    let env = Env::new("claude");
    env.claude("UserPromptSubmit", event("UserPromptSubmit", json!({})));
    env.claude(
        "SubagentStart",
        event("SubagentStart", json!({ "agent_id": "memory" })),
    );
    env.claude("Stop", event("Stop", json!({})));
    assert_eq!(env.hook_state().as_deref(), Some("done"));

    env.claude(
        "PreToolUse",
        event(
            "PreToolUse",
            json!({ "agent_id": "memory", "tool_name": "Write", "tool_input": {} }),
        ),
    );
    env.claude(
        "SubagentStop",
        event("SubagentStop", json!({ "agent_id": "memory" })),
    );
    assert_eq!(env.hook_state().as_deref(), Some("done"));
    assert!(env
        .transitions()
        .iter()
        .all(|(_, to)| to.as_deref() != Some("background")));
}

#[test]
fn a_subagent_that_finishes_inside_its_turn_ends_the_turn_done() {
    let env = Env::new("claude");
    env.claude("UserPromptSubmit", event("UserPromptSubmit", json!({})));
    env.claude("PreToolUse", main_tool("PreToolUse", "Agent"));
    env.claude("SubagentStart", typed("SubagentStart", SUBAGENT));
    env.claude("PreToolUse", subagent_tool("PreToolUse", SUBAGENT));
    env.claude("SubagentStop", typed("SubagentStop", SUBAGENT));
    env.claude("PostToolUse", main_tool("PostToolUse", "Agent"));
    assert_eq!(env.hook_state().as_deref(), Some("working"));
    env.claude("Stop", event("Stop", json!({})));
    assert_eq!(env.hook_state().as_deref(), Some("done"));
}

/// A missed `SubagentStop` must not leave the session `background` for good.
/// The next prompt starts a turn that forgets the earlier turn's work.
#[test]
fn a_missed_end_is_forgotten_by_the_next_turn() {
    let env = Env::new("claude");
    turn_that_starts_a_background_subagent(&env, SUBAGENT);
    assert_eq!(env.hook_state().as_deref(), Some("background"));

    env.claude("UserPromptSubmit", event("UserPromptSubmit", json!({})));
    assert_eq!(env.hook_state().as_deref(), Some("working"));
    env.claude("Stop", event("Stop", json!({})));
    assert_eq!(env.hook_state().as_deref(), Some("done"));

    // Its end arriving late changes nothing.
    env.claude("SubagentStop", typed("SubagentStop", SUBAGENT));
    assert_eq!(env.hook_state().as_deref(), Some("done"));
}

/// ...and the agent starting again forgets all of it: whatever was in flight
/// died with the process before it.
#[test]
fn a_missed_end_is_forgotten_when_the_agent_starts_again() {
    let env = Env::new("claude");
    turn_that_starts_a_background_subagent(&env, SUBAGENT);
    assert_eq!(env.hook_state().as_deref(), Some("background"));

    env.claude("SessionStart", event("SessionStart", json!({})));
    assert_eq!(env.hook_state().as_deref(), Some("idle"));
    env.claude("UserPromptSubmit", event("UserPromptSubmit", json!({})));
    env.claude("Stop", event("Stop", json!({})));
    assert_eq!(env.hook_state().as_deref(), Some("done"));
}

/// ...and so does `session stop`, which clears the hook columns with it.
#[test]
fn a_missed_end_is_forgotten_by_session_stop() {
    let env = Env::new("claude");
    turn_that_starts_a_background_subagent(&env, SUBAGENT);
    assert_eq!(env.hook_state().as_deref(), Some("background"));

    let db = env.db();
    db.set_session_stopped(env.id, true).expect("stop");
    db.set_session_stopped(env.id, false).expect("start");
    env.claude("UserPromptSubmit", event("UserPromptSubmit", json!({})));
    env.claude("Stop", event("Stop", json!({})));
    assert_eq!(env.hook_state().as_deref(), Some("done"));
}

/// An interrupted turn fires no `Stop` and its subagent no `SubagentStop`, so
/// the session still reads `working` when the next prompt arrives; that prompt
/// starts a turn of its own, which must not end as `background`.
#[test]
fn an_interrupted_subagent_does_not_outlive_the_next_prompt() {
    let env = Env::new("claude");
    env.claude("UserPromptSubmit", event("UserPromptSubmit", json!({})));
    env.claude("PreToolUse", main_tool("PreToolUse", "Agent"));
    env.claude("SubagentStart", typed("SubagentStart", SUBAGENT));
    assert_eq!(env.hook_state().as_deref(), Some("working"));

    env.claude("UserPromptSubmit", event("UserPromptSubmit", json!({})));
    env.claude("Stop", event("Stop", json!({})));
    assert_eq!(env.hook_state().as_deref(), Some("done"));
}

/// An agent with no mapping reports exactly what it reported before: the same
/// states, in the same order, and never `background`.
#[test]
fn an_agent_without_the_mapping_reports_as_before() {
    let env = Env::new("codex");
    let mut seen = Vec::new();
    for (name, body) in [
        (
            "SessionStart",
            event("SessionStart", json!({ "source": "startup" })),
        ),
        ("UserPromptSubmit", event("UserPromptSubmit", json!({}))),
        // Its `PreToolUse` group is matched to one tool, which the agent and
        // not the shell applies, so it is left out here.
        ("PostToolUse", main_tool("PostToolUse", "shell")),
        ("Stop", event("Stop", json!({}))),
    ] {
        env.fire("codex-hooks.json", name, body);
        seen.push(env.hook_state().unwrap_or_default());
    }
    assert_eq!(seen, ["idle", "working", "working", "done"]);

    // A claude turn with no subagent in it is just as it was.
    let env = Env::new("claude");
    let mut seen = Vec::new();
    for (name, body) in [
        ("SessionStart", event("SessionStart", json!({}))),
        ("UserPromptSubmit", event("UserPromptSubmit", json!({}))),
        ("PreToolUse", main_tool("PreToolUse", "Bash")),
        ("PostToolUse", main_tool("PostToolUse", "Bash")),
        ("Stop", event("Stop", json!({}))),
    ] {
        env.claude(name, body);
        seen.push(env.hook_state().unwrap_or_default());
    }
    assert_eq!(seen, ["idle", "working", "working", "working", "done"]);
}

/// Any agent may report the state itself; with no work counted, its next
/// `done` is simply `done`.
#[test]
fn any_agent_can_signal_background_directly() {
    let env = Env::new("codex");
    let signal = |state: &str| {
        let out = env
            .command(env!("CARGO_BIN_EXE_thurbox-cli"))
            .args(["--json", "session", "signal", "--state", state])
            .output()
            .expect("signal");
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
    };
    signal("background");
    assert_eq!(env.state(), "background");
    signal("done");
    assert_eq!(env.state(), "done");
}
