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
//! behaviour. A payload's own `matcher` key is the one thing here that is
//! modelled rather than executed: it selects which groups run, because it is
//! the agent and not the shell that applies it ([`matcher_applies`]). The
//! `thurbox-cli` they call is a stub that records the state
//! word. The script payloads (opencode, pi, omp) are run too, under Node's
//! own (type-stripped for the two TypeScript ones) ESM loader: each is
//! imported for real and driven through its actual `pi.on` registration, or
//! opencode's default export (1.x's `server` and 2.x's `setup`), with only the
//! one thing outside the module's own control — `pi`'s injected API, or
//! opencode's hook and event hosts — stood in for. Each still runs its real
//! `exec()` against the same stub `thurbox-cli`, on `PATH`, that the JSON
//! turns use.

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

/// Whether a group carrying `matcher` applies to the tool `tool_name`.
///
/// codex matches a `matcher` against the **whole** tool name, verified against
/// codex-cli 0.154.0 by driving a turn with a `request_user` group registered
/// alongside a `^request_user_input$` one: only the anchored full name fired.
/// The anchors are therefore decoration codex neither needs nor rejects, and
/// they are stripped rather than interpreted, because this is a literal
/// comparison and not a regex engine.
///
/// Which is why anything left over that a *real* engine would read as syntax is
/// a panic rather than a literal to compare. A matcher widened to
/// `^request_user_input.*$` would otherwise pass
/// [`an_async_question_is_not_a_block`] — this function would compare it to
/// `request_user_input_async` and find them different — while codex matched it
/// and reported `blocked` on a question it never waits on. Failing loudly here
/// is the honest answer: the payloads ship literal tool names, and one that
/// grows real syntax needs this helper replaced, not quietly believed.
fn matcher_applies(group: &serde_json::Value, tool_name: &str) -> bool {
    let Some(matcher) = group.get("matcher").and_then(|m| m.as_str()) else {
        return true;
    };
    if matcher.is_empty() || matcher == ".*" {
        return true;
    }
    let literal = matcher.trim_start_matches('^').trim_end_matches('$');
    assert!(
        !literal.contains(|c| "\\.+*?()[]{}|^$".contains(c)),
        "matcher {matcher:?} is a regex, which this literal comparison would \
         silently get wrong — give it a real engine or keep the payloads literal"
    );
    literal == tool_name
}

/// Run every hook the payload registers for `event`, feeding it `body` on
/// stdin exactly as the agent does.
///
/// `tool_name` is the tool the event is about, for the events that have one;
/// a group whose `matcher` names a different tool is skipped, as the agent
/// skips it. `None` fires every group — which is what the events that carry no
/// tool want, and what keeps copilot's `notification` matcher (a notification
/// *kind*, not a tool) out of a tool-name comparison it would always lose.
fn fire(payload: &serde_json::Value, dir: &Path, event: &str, body: &str, tool_name: Option<&str>) {
    let path = path_with(dir);
    let Some(hooks) = payload["hooks"].get(event) else {
        return;
    };
    let groups: Vec<&serde_json::Value> = match (hooks.as_array(), tool_name) {
        (Some(groups), Some(tool)) => groups
            .iter()
            .filter(|group| matcher_applies(group, tool))
            .collect(),
        _ => vec![hooks],
    };
    for command in groups.into_iter().flat_map(commands_for) {
        let mut child = Command::new("sh")
            .arg("-c")
            .arg(&command)
            .env("PATH", &path)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .spawn()
            .expect("spawn hook");
        // Most of these commands (everything but `Notification`) never read
        // stdin at all, so the child can exit and close its end of the pipe
        // before this write lands — a race that's more likely to lose under
        // the CPU contention of a full parallel test run. That's not a real
        // failure: the write was never going to be consumed either way.
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
/// prompt (all but antigravity), tool call, permission prompt, the tool
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
        // codex has a real approval event, so this turn's block edge needs no
        // matcher and PostToolUse is the edge back out. (Its *other* block edge,
        // the question tool, is matcher-gated — see
        // `asking_the_user_a_question_blocks_the_session`.)
        "codex",
        "codex-hooks.json",
        [
            "UserPromptSubmit",
            "PreToolUse",
            "PermissionRequest",
            "PostToolUse",
            "Stop",
        ],
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

        fire(&payload, dir.path(), prompt, &body(prompt, ""), None);
        fire(&payload, dir.path(), pre, &body(pre, ""), Some("shell"));
        assert_eq!(current(&log), "working", "{agent}: a tool call is work");

        fire(
            &payload,
            dir.path(),
            notify,
            &body(notify, "Claude needs your permission to use Bash"),
            None,
        );
        assert_eq!(current(&log), "blocked", "{agent}: a prompt is a block");

        // The user approves and the tool runs to completion. Whatever the agent
        // does next — another tool, minutes of output, or just prose until the
        // turn ends — it is no longer waiting on anyone.
        fire(&payload, dir.path(), post, &body(post, ""), Some("shell"));
        assert_eq!(
            current(&log),
            "working",
            "{agent}: granted permission left the session blocked"
        );

        fire(&payload, dir.path(), stop, &body(stop, ""), None);
        assert_eq!(current(&log), "done", "{agent}: the turn ended");
    }
}

/// The *other* way a turn stops and waits for you: the agent asks a question
/// rather than for permission. codex does that by calling a tool
/// (`request_user_input`, the one plan mode leans on), so the only event that
/// fires is `PreToolUse` — and a payload that reads every tool call as work
/// left a session sitting on an unanswered question spinning "working", which
/// is exactly the state the dot exists to distinguish. The answer arriving is
/// the tool completing, the same edge back out an approval takes.
///
/// pi and omp's equivalent (`ask_user_question` / `ask`) is driven for real in
/// `the_script_payloads_report_working_when_the_block_clears`.
#[test]
fn asking_the_user_a_question_blocks_the_session() {
    let dir = tempfile::tempdir().expect("tempdir");
    let payload = payload_json("codex-hooks.json");
    let log = stub_cli(dir.path());
    let question = Some("request_user_input");

    fire(
        &payload,
        dir.path(),
        "UserPromptSubmit",
        &body("UserPromptSubmit", ""),
        None,
    );
    assert_eq!(current(&log), "working", "codex: the turn started");

    fire(
        &payload,
        dir.path(),
        "PreToolUse",
        &body("PreToolUse", ""),
        question,
    );
    assert_eq!(
        current(&log),
        "blocked",
        "codex: an unanswered question is a block"
    );

    fire(
        &payload,
        dir.path(),
        "PostToolUse",
        &body("PostToolUse", ""),
        question,
    );
    assert_eq!(current(&log), "working", "codex: the answer arrived");

    fire(&payload, dir.path(), "Stop", &body("Stop", ""), None);
    assert_eq!(current(&log), "done", "codex: the turn ended");
}

/// `request_user_input_async` poses a question the agent does **not** wait on —
/// it keeps working while the question sits there — so the block edge is the
/// exact tool name and nothing that merely starts with it. codex full-matches a
/// matcher, so this is what the payload must *not* claim.
#[test]
fn an_async_question_is_not_a_block() {
    let dir = tempfile::tempdir().expect("tempdir");
    let payload = payload_json("codex-hooks.json");
    let log = stub_cli(dir.path());

    fire(
        &payload,
        dir.path(),
        "UserPromptSubmit",
        &body("UserPromptSubmit", ""),
        None,
    );
    fire(
        &payload,
        dir.path(),
        "PreToolUse",
        &body("PreToolUse", ""),
        Some("request_user_input_async"),
    );
    assert_eq!(
        current(&log),
        "working",
        "codex: an async question stopped nothing"
    );
}

/// After a turn ends, claude can start a background agent of its own (the one
/// that writes the project's memory) whose tool calls fire the session's
/// `PreToolUse`/`PostToolUse` — with an `agent_id` and no `agent_type` — and
/// nothing reports when it finishes. Verified against Claude Code 2.1.301: on a
/// fresh session its `PreToolUse` landed 2.4s after `Stop`, and the session
/// read `working` until the next turn. A subagent the turn starts through the
/// Agent tool carries an `agent_type`, and its tool calls are work the session
/// is doing.
#[test]
fn claudes_background_agent_does_not_undo_done() {
    let dir = tempfile::tempdir().expect("tempdir");
    let payload = payload_json("claude.json");
    let log = stub_cli(dir.path());
    let tool_body = |event: &str, extra: serde_json::Value| {
        let mut body = serde_json::json!({
            "session_id": "abc",
            "hook_event_name": event,
            "tool_name": "Bash",
        });
        body.as_object_mut()
            .expect("object")
            .extend(extra.as_object().expect("object").clone());
        body.to_string()
    };

    fire(&payload, dir.path(), "Stop", &body("Stop", ""), None);
    for event in ["PreToolUse", "PostToolUse"] {
        let background = tool_body(event, serde_json::json!({ "agent_id": "a74b3db0" }));
        fire(&payload, dir.path(), event, &background, Some("Bash"));
        assert_eq!(
            current(&log),
            "done",
            "claude: its background agent's {event} undid the turn's done"
        );
    }

    let subagent = tool_body(
        "PreToolUse",
        serde_json::json!({ "agent_id": "a9096816", "agent_type": "general-purpose" }),
    );
    fire(&payload, dir.path(), "PreToolUse", &subagent, Some("Bash"));
    assert_eq!(
        current(&log),
        "working",
        "claude: a subagent's tool is work"
    );
}

/// copilot fires `sessionStart` lazily: not when the CLI opens, nor when
/// `--continue` reopens a conversation, but as part of the first prompt — and
/// *after* `userPromptSubmitted` (verified against copilot 1.0.98: on three
/// separate launches `working` arrived, then `sessionStart` ~60ms later). A
/// `sessionStart` that signals `idle` therefore overwrote the turn's `working`,
/// and a first turn that ran no tool showed idle until it ended.
#[test]
fn copilots_first_turn_reads_as_working() {
    let dir = tempfile::tempdir().expect("tempdir");
    let payload = payload_json("copilot-hooks.json");
    let log = stub_cli(dir.path());

    for event in ["userPromptSubmitted", "sessionStart"] {
        fire(&payload, dir.path(), event, &body(event, ""), None);
    }
    assert_eq!(
        current(&log),
        "working",
        "copilot: the first turn read as idle"
    );

    fire(
        &payload,
        dir.path(),
        "agentStop",
        &body("agentStop", ""),
        None,
    );
    assert_eq!(current(&log), "done", "copilot: the turn ended");
}

/// The background-agent check reads only the payload's own fields, never the
/// tool's: a main-agent call whose `tool_input` or `tool_response` carries an
/// `agent_id` (an Agent-tool call reporting the subagent it started, a JSON
/// file the tool wrote) is still the turn working. The bodies are spelled in
/// claude's own key order, which puts `agent_id`/`agent_type` ahead of
/// `tool_input` and `tool_response`.
#[test]
fn an_agent_id_inside_the_tool_payload_is_still_work() {
    let payload = payload_json("claude.json");
    for (event, tool_call) in [
        (
            "PreToolUse",
            r#"{"session_id":"abc","hook_event_name":"PreToolUse","tool_name":"Bash","tool_input":{"command":"cat state.json","agent_id":"a1"}}"#,
        ),
        (
            "PostToolUse",
            r#"{"session_id":"abc","hook_event_name":"PostToolUse","tool_name":"Agent","tool_input":{"prompt":"ls"},"tool_response":{"agent_id":"a9096816","agent_type":""}}"#,
        ),
    ] {
        let dir = tempfile::tempdir().expect("tempdir");
        let log = stub_cli(dir.path());
        fire(&payload, dir.path(), "Stop", &body("Stop", ""), None);
        fire(&payload, dir.path(), event, tool_call, Some("Bash"));
        assert_eq!(
            current(&log),
            "working",
            "claude: an agent_id inside the tool payload hid the main agent's {event}"
        );
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

        fire(
            &payload,
            dir.path(),
            "PreToolUse",
            &body("PreToolUse", ""),
            Some("shell"),
        );
        fire(
            &payload,
            dir.path(),
            "Notification",
            &body("Notification", "Claude is waiting for your input"),
            None,
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

/// The part of the opencode drivers both API generations share: the stub
/// `thurbox-cli`'s line count, and waiting for (or ruling out) one more signal.
const OPENCODE_DRIVER_PRELUDE: &str = r#"
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

async function expectNoSignal(before, what) {
  await new Promise((r) => setTimeout(r, 300));
  if (lineCount() !== before) throw new Error(`${what} signalled a state`);
}

const mod = await import(pathToFileURL(modulePath).href);
"#;

/// opencode 1.x: a default export carrying `id` and `server(input)`, which
/// returns the V1 hooks (`chat.message`, `event`). 1.x *also* calls a module's
/// `setup` through its partial V2 bridge, whose context has neither `event` nor
/// `session` — so that call is made here too, and must neither throw nor
/// signal, or every 1.x turn would report twice.
const OPENCODE_V1_DRIVER: &str = r#"
const plugin = mod.default;
if (!plugin || typeof plugin.id !== "string" || typeof plugin.server !== "function")
  throw new Error("opencode 1.x needs a default export with an id and server()");

const before = lineCount();
await plugin.setup({ options: {}, agent: {}, command: {}, plugin: {}, skill: {} });
await expectNoSignal(before, "setup on the 1.x bridge");

const handlers = await plugin.server({});
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

/// opencode 2.x: the loader decodes the module's `default` as `{ id, setup }`
/// (or `{ id, effect }`) and refuses anything else with "Plugin must export a
/// default definition with an id and an effect or setup function." — the
/// check below is that schema. `setup` then gets a context whose `event`
/// domain is an async-iterable subscription over `{ type, data }` events.
const OPENCODE_V2_DRIVER: &str = r#"
const plugin = mod.default;
if (
  !plugin ||
  typeof plugin.id !== "string" ||
  (typeof plugin.setup !== "function" && typeof plugin.effect !== "function")
)
  throw new Error(
    "Plugin must export a default definition with an id and an effect or setup function.",
  );

const queue = [];
let wake = () => {};
const ctx = {
  session: { hook: async () => ({ dispose: async () => {} }) },
  event: {
    subscribe: () => ({
      async *[Symbol.asyncIterator]() {
        for (;;) {
          while (queue.length) yield queue.shift();
          await new Promise((r) => (wake = r));
        }
      },
    }),
  },
};
await plugin.setup(ctx);

for (const step of events) {
  const before = lineCount();
  queue.push({ type: step.type, data: step.data });
  wake();
  if (step.silent) await expectNoSignal(before, step.type);
  else await waitForSignal(before);
}
// The subscription never ends on its own; the turn is over.
process.exit(0);
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
        &format!("{OPENCODE_DRIVER_PRELUDE}{OPENCODE_V1_DRIVER}"),
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
        "opencode-status.js (1.x): the turn's signalled states"
    );
}

/// The same turn on opencode 2.x, which refuses the 1.x module shape outright
/// (issue #1417): the plugin has to load under its schema, and then report the
/// turn from the 2.x event stream. A subagent runs in a child session of its
/// own, whose creation and completion are not the session's — signalling them
/// would put the dot at `idle`/`done` while the parent is still working.
#[test]
fn the_opencode_plugin_loads_and_reports_a_turn_on_opencode_2() {
    if !have_node() {
        eprintln!("skipping: node is not installed");
        return;
    }
    let dir = tempfile::tempdir().expect("tempdir");
    let log = stub_cli(dir.path());
    let root = serde_json::json!({"sessionID": "ses_root"});
    let child = serde_json::json!({"sessionID": "ses_child", "parentID": "ses_root"});
    let events = serde_json::json!([
        {"type": "session.created", "data": root},
        {"type": "session.execution.started", "data": root},
        {"type": "permission.asked", "data": root},
        {"type": "permission.replied", "data": root},
        {"type": "session.created", "data": child, "silent": true},
        {"type": "session.execution.succeeded", "data": {"sessionID": "ses_child"}, "silent": true},
        {"type": "session.execution.succeeded", "data": root},
    ])
    .to_string();
    run_node_driver(
        dir.path(),
        &format!("{OPENCODE_DRIVER_PRELUDE}{OPENCODE_V2_DRIVER}"),
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
        "opencode-status.js (2.x): the turn's signalled states"
    );
}

/// opencode 2.x's TUI connects by default to one background server
/// (`serve --service`) that every opencode on the machine shares, started with
/// the environment of whichever launch came first. Its `$THURBOX_SESSION` is no
/// particular session's, so a plugin hosted there must report nothing at all
/// rather than every session's turns as that one's.
#[test]
fn the_opencode_plugin_is_silent_in_the_shared_opencode_2_server() {
    if !have_node() {
        eprintln!("skipping: node is not installed");
        return;
    }
    let dir = tempfile::tempdir().expect("tempdir");
    let log = stub_cli(dir.path());
    let root = serde_json::json!({"sessionID": "ses_root"});
    let events = serde_json::json!([
        {"type": "session.created", "data": root, "silent": true},
        {"type": "session.execution.started", "data": root, "silent": true},
        {"type": "permission.asked", "data": root, "silent": true},
        {"type": "session.execution.succeeded", "data": root, "silent": true},
    ])
    .to_string();
    run_node_driver(
        dir.path(),
        &format!(
            "{OPENCODE_DRIVER_PRELUDE}process.argv.push(\"serve\", \"--service\");\n{OPENCODE_V2_DRIVER}"
        ),
        "opencode-status.js",
        &events,
        &log,
    );
    assert!(!log.exists(), "the shared server reported a state");
}
