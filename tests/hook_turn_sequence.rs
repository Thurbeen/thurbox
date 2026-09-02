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
//! word. The script payloads (opencode, pi, omp) are run too, under Node's
//! own (type-stripped for the two TypeScript ones) ESM loader: each is
//! imported for real and driven through its actual `pi.on`/`ThurboxStatus`
//! registration, with only the one thing outside the module's own control —
//! `pi`'s injected API, or opencode's shell tag — stood in for. The `pi`/`omp`
//! stand-in still runs `report()`'s real `exec()` against the same stub
//! `thurbox-cli`, on `PATH`, that the JSON turns use.

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
///
/// Hook commands run two different ways depending on the payload: the JSON
/// payloads' commands go through `sh -c` ([`fire`]), while pi/omp's
/// TypeScript calls Node's `child_process.exec`, which on Windows is `cmd.exe`
/// rather than a POSIX shell. A `#!/bin/sh` script (no extension) satisfies
/// the former on every OS — MSYS's `sh` reads the shebang directly, an exact
/// filename match that never involves an extension search. `cmd.exe` cannot
/// run that script at all, so on Windows it additionally gets a `.cmd` batch
/// file — but *not* in the same directory: `cmd.exe`'s PATH search resolves a
/// bare `thurbox-cli` by trying an exact match before appending `PATHEXT`
/// extensions, so an extension-less `thurbox-cli` sitting next to
/// `thurbox-cli.cmd` would make it fail that exact-match attempt (the
/// shebang script isn't a real executable) and never fall through to the
/// batch file. Putting the batch file in its own directory keeps each
/// interpreter's search finding only the stub it can actually run.
fn stub_cli(dir: &Path) -> PathBuf {
    let log = dir.join("states");
    let bin = dir.join("thurbox-cli");
    std::fs::write(
        &bin,
        // The log path is quoted so an unquoted Windows `C:\...` redirect
        // target doesn't have its backslashes eaten as POSIX shell escapes.
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$4\" >> \"{}\"\n",
            log.display()
        ),
    )
    .expect("write stub");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        // Owner-only: this stub is a `sh -c` implementation detail of the
        // test process, not something any other user on the machine needs
        // to run.
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o700)).expect("chmod");
    }
    if cfg!(windows) {
        let cmd_dir = cmd_stub_dir(dir);
        std::fs::create_dir(&cmd_dir).expect("mkdir cmd stub dir");
        let cmd_bin = cmd_dir.join("thurbox-cli.cmd");
        // `session signal --state <s>`: the state is the fourth argument.
        // Batch doesn't treat `\` as an escape character, so the path needs
        // no more than the quoting any Windows path with spaces would.
        std::fs::write(&cmd_bin, format!("@echo %4>>\"{}\"\r\n", log.display()))
            .expect("write cmd stub");
    }
    log
}

/// Where [`stub_cli`] puts the batch-file stub, kept separate from the
/// shebang script so `cmd.exe` and `sh` each only ever see the one they can
/// run — see [`stub_cli`].
fn cmd_stub_dir(dir: &Path) -> PathBuf {
    dir.join("cmd-stub")
}

/// The current `PATH`, with `dir` (and, on Windows, its `.cmd`-stub
/// subdirectory) prepended, using this OS's search-path separator — a
/// hardcoded `:` leaves Windows's own entries (joined with `;`, and each
/// containing a drive-letter `:`) unparseable.
fn path_with(dir: &Path) -> std::ffi::OsString {
    let existing = std::env::var_os("PATH").unwrap_or_default();
    let dirs = [dir.to_path_buf(), cmd_stub_dir(dir)]
        .into_iter()
        .chain(std::env::split_paths(&existing));
    std::env::join_paths(dirs).expect("join PATH")
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
    let path = path_with(dir);
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

fn have_node() -> bool {
    Command::new("node")
        .arg("--version")
        .output()
        .map(|out| out.status.success())
        .unwrap_or(false)
}

/// Drives a `pi.on`-shaped module (pi, omp) through a real turn: registers a
/// stand-in `pi` that just records handlers, imports the module for real, and
/// calls each handler in turn. `report()`'s `exec()` is not intercepted — it
/// runs for real against the stub `thurbox-cli` on `PATH` — so this proves
/// what the shipped code actually signals, not an assumption about it.
const PI_DRIVER: &str = r#"
import { existsSync, readFileSync } from "node:fs";
import { pathToFileURL } from "node:url";

const [, , modulePath, logPath, eventsJson] = process.argv;
const events = JSON.parse(eventsJson);

function lineCount() {
  if (!existsSync(logPath)) return 0;
  return readFileSync(logPath, "utf8").split("\n").filter(Boolean).length;
}

async function waitForSignal(before) {
  const deadline = Date.now() + 5000;
  while (Date.now() < deadline) {
    if (lineCount() > before) return;
    await new Promise((r) => setTimeout(r, 10));
  }
  throw new Error("timed out waiting for a thurbox-cli signal");
}

const handlers = {};
const pi = {
  on(event, handler) {
    handlers[event] = handler;
  },
};

const mod = await import(pathToFileURL(modulePath).href);
mod.default(pi);

for (const { event, toolName } of events) {
  const handler = handlers[event];
  if (!handler) throw new Error(`no handler registered for ${event}`);
  const before = lineCount();
  toolName === null ? handler() : handler({ toolName });
  await waitForSignal(before);
}
"#;

/// Same idea for opencode's `ThurboxStatus({ $ })`: `$` is opencode's own
/// shell tag, so the stand-in builds the same command string a real one would
/// and actually runs it, against the same stub `thurbox-cli`.
const OPENCODE_DRIVER: &str = r#"
import { exec } from "node:child_process";
import { existsSync, readFileSync } from "node:fs";
import { pathToFileURL } from "node:url";

const [, , modulePath, logPath, eventsJson] = process.argv;
const events = JSON.parse(eventsJson);

function lineCount() {
  if (!existsSync(logPath)) return 0;
  return readFileSync(logPath, "utf8").split("\n").filter(Boolean).length;
}

async function waitForSignal(before) {
  const deadline = Date.now() + 5000;
  while (Date.now() < deadline) {
    if (lineCount() > before) return;
    await new Promise((r) => setTimeout(r, 10));
  }
  throw new Error("timed out waiting for a thurbox-cli signal");
}

function $(strings, ...values) {
  let command = strings[0];
  for (let i = 0; i < values.length; i++) command += String(values[i]) + strings[i + 1];
  const promise = new Promise((resolve) => exec(command, () => resolve()));
  promise.quiet = () => promise;
  promise.nothrow = () => promise;
  return promise;
}

const mod = await import(pathToFileURL(modulePath).href);
const handlers = await mod.ThurboxStatus({ $ });

for (const step of events) {
  const before = lineCount();
  if (step.kind === "chat.message") {
    await handlers["chat.message"]();
  } else {
    await handlers.event({ event: { type: step.type } });
  }
  await waitForSignal(before);
}
"#;

fn run_node_driver(dir: &Path, driver: &str, module: &str, events_json: &str, log: &Path) {
    let driver_path = dir.join("driver.mjs");
    std::fs::write(&driver_path, driver).expect("write driver");
    let path = path_with(dir);
    let output = Command::new("node")
        .arg(&driver_path)
        .arg(payload_path(module))
        .arg(log)
        .arg(events_json)
        .env("PATH", path)
        .output()
        .expect("run node driver");
    assert!(
        output.status.success(),
        "{module} driver failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// The same edge as `granting_a_permission_puts_the_session_back_to_working`,
/// for the payloads that are code rather than declarative hook commands: each
/// is imported and driven through session start, a tool call, the question
/// tool that blocks the turn, that tool completing (the fix), and the turn
/// ending.
#[test]
fn the_script_payloads_report_working_when_the_block_clears() {
    if !have_node() {
        eprintln!("skipping: node is not installed");
        return;
    }

    // pi and omp block on their own structured question tool; the tool
    // completing is the user's answer arriving. omp additionally recognizes
    // pi's tool name, but "ask" is the one it documents as its own.
    for (module, blocking_tool) in [
        ("pi-status.ts", "ask_user_question"),
        ("omp-status.ts", "ask"),
    ] {
        let dir = tempfile::tempdir().expect("tempdir");
        let log = stub_cli(dir.path());
        let events = serde_json::json!([
            {"event": "session_start", "toolName": null},
            {"event": "agent_start", "toolName": null},
            {"event": "tool_execution_start", "toolName": blocking_tool},
            {"event": "tool_execution_end", "toolName": null},
            {"event": "agent_end", "toolName": null},
        ])
        .to_string();
        run_node_driver(dir.path(), PI_DRIVER, module, &events, &log);
        assert_eq!(
            std::fs::read_to_string(&log)
                .unwrap_or_default()
                .lines()
                .collect::<Vec<_>>(),
            vec!["idle", "working", "blocked", "working", "done"],
            "{module}: the turn's signalled states"
        );
    }

    // opencode alone has a real permission-reply event.
    let dir = tempfile::tempdir().expect("tempdir");
    let log = stub_cli(dir.path());
    let events = serde_json::json!([
        {"kind": "event", "type": "session.created"},
        {"kind": "chat.message"},
        {"kind": "event", "type": "permission.asked"},
        {"kind": "event", "type": "permission.replied"},
        {"kind": "event", "type": "session.idle"},
    ])
    .to_string();
    run_node_driver(
        dir.path(),
        OPENCODE_DRIVER,
        "opencode-status.js",
        &events,
        &log,
    );
    assert_eq!(
        std::fs::read_to_string(&log)
            .unwrap_or_default()
            .lines()
            .collect::<Vec<_>>(),
        vec!["idle", "working", "blocked", "working", "done"],
        "opencode-status.js: the turn's signalled states"
    );
}
