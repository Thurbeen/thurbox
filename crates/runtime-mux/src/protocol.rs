//! The runtime protocol (docs/runtime/README.md) over one backend: what the
//! `runtime-mux` program answers for each verb.
//!
//! Each call is one request in and one answer out, `{"known": …}` or
//! `{"unknown": {"why": …}}`. Nothing here is kept between calls: the
//! multiplexer's server is the whole state, and a key is found again by its
//! window's name (`name_of`), never by a record of ours.
//!
//! A process is the key's agent window. A pane kept after its program exits is
//! how `inspect` reports an exit code; a `start` replaces it.

use std::collections::HashMap;
use std::io::Read;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde::Deserialize;
use serde_json::{json, Value};

use crate::contract::{Located, Owner, SessionBackend, WindowRole, WindowSpec};
use crate::identity::window_name_for;

/// The protocol version this program speaks.
pub const PROTOCOL: u64 = 1;

/// The verbs it answers, `manifest` first.
pub const VERBS: &[&str] = &[
    "manifest", "start", "inspect", "stop", "list", "input", "capture", "exec",
];

/// The tmux socket the program's server runs on when [`SOCKET_ENV`] names
/// none: its own, never the interface's (`thurbox`), which it does not read.
pub const SOCKET: &str = "runtime-mux";

/// Names another socket, for an operator running two Fleets on one machine
/// and for tests.
pub const SOCKET_ENV: &str = "RUNTIME_MUX_SOCKET";

/// The longest key: Fleet's are `<prefix>.<uid>`, well inside it.
const MAX_KEY: usize = 128;

/// Rows `capture` reads when the request names none.
const DEFAULT_CAPTURE_LINES: u32 = 50;

/// The most `exec` keeps of each stream when the request names no cap.
const DEFAULT_OUTPUT_CAP: usize = 1024 * 1024;
const MAX_OUTPUT_CAP: usize = 16 * 1024 * 1024;
const MAX_EXEC_TIMEOUT_MS: u64 = 600_000;

fn known(value: Value) -> Value {
    json!({ "known": value })
}

fn unknown(why: impl std::fmt::Display) -> Value {
    json!({ "unknown": { "why": why.to_string() } })
}

/// The answer to `verb` with `request`, the JSON read from stdin.
pub fn serve(backend: &dyn SessionBackend, verb: &str, request: &[u8]) -> Value {
    if verb == "manifest" {
        return known(json!({ "protocol": PROTOCOL, "verbs": VERBS }));
    }
    let request: Value = match serde_json::from_slice(request) {
        Ok(value) => value,
        Err(e) => return unknown(format!("the request is not JSON: {e}")),
    };
    match request.get("protocol").and_then(Value::as_u64) {
        Some(PROTOCOL) => {}
        Some(other) => {
            return unknown(format!(
                "protocol {other} is not served; this runtime speaks {PROTOCOL}"
            ))
        }
        None => return unknown("the request names no protocol"),
    }
    let result = match verb {
        "start" => parse(request).map(|r| start(backend, r)),
        "inspect" => parse(request).map(|r: Keyed| inspect(backend, &r.key)),
        "stop" => parse(request).map(|r: Stop| stop(backend, &r.key)),
        "list" => Ok(list(backend)),
        "input" => parse(request).map(|r| input(backend, r)),
        "capture" => parse(request).map(|r| capture(backend, r)),
        "exec" => parse(request).map(exec),
        other => Err(format!("unknown verb {other:?}")),
    };
    result.unwrap_or_else(unknown)
}

/// A request as its verb's shape, with every key it names checked.
fn parse<T: for<'de> Deserialize<'de> + Validate>(request: Value) -> Result<T, String> {
    let parsed: T = serde_json::from_value(request).map_err(|e| format!("bad request: {e}"))?;
    parsed.validate()?;
    Ok(parsed)
}

trait Validate {
    fn validate(&self) -> Result<(), String>;
}

/// A key the program accepts: what Fleet makes, and nothing tmux would read
/// as a target or a format.
fn check_key(key: &str) -> Result<(), String> {
    if key.is_empty()
        || key.len() > MAX_KEY
        || !key
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_'))
    {
        return Err(format!(
            "key {key:?}: use 1 to {MAX_KEY} ASCII letters, digits, '.', '-' or '_'"
        ));
    }
    Ok(())
}

#[derive(Deserialize)]
struct Keyed {
    key: String,
}

impl Validate for Keyed {
    fn validate(&self) -> Result<(), String> {
        check_key(&self.key)
    }
}

#[derive(Deserialize)]
struct Stop {
    key: String,
    /// Removes the key's workspace too; this runtime makes none, so there is
    /// nothing more to remove.
    #[serde(default)]
    #[allow(dead_code)]
    release: bool,
}

impl Validate for Stop {
    fn validate(&self) -> Result<(), String> {
        check_key(&self.key)
    }
}

#[derive(Deserialize)]
struct Start {
    key: String,
    argv: Vec<String>,
    #[serde(default)]
    env: HashMap<String, String>,
    dir: Option<PathBuf>,
}

impl Validate for Start {
    fn validate(&self) -> Result<(), String> {
        check_key(&self.key)?;
        if self.argv.first().is_none_or(|program| program.is_empty()) {
            return Err("argv names no program".into());
        }
        if self.dir.as_ref().is_some_and(|dir| !dir.is_absolute()) {
            return Err("dir must be an absolute path".into());
        }
        Ok(())
    }
}

#[derive(Deserialize)]
struct Input {
    key: String,
    text: String,
    #[serde(default)]
    submit: bool,
}

impl Validate for Input {
    fn validate(&self) -> Result<(), String> {
        check_key(&self.key)
    }
}

#[derive(Deserialize)]
struct Capture {
    key: String,
    lines: Option<u32>,
}

impl Validate for Capture {
    fn validate(&self) -> Result<(), String> {
        check_key(&self.key)
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Exec {
    argv: Vec<String>,
    #[serde(rename = "in", default = "host")]
    place: String,
    dir: Option<PathBuf>,
    timeout_ms: u64,
    output_cap: Option<usize>,
}

fn host() -> String {
    "host".into()
}

impl Validate for Exec {
    fn validate(&self) -> Result<(), String> {
        if self.place != "host" {
            return Err(format!(
                "exec in {:?}: this runtime makes no workspace, so it runs only `in: host`",
                self.place
            ));
        }
        if self.argv.first().is_none_or(|program| program.is_empty()) {
            return Err("argv names no program".into());
        }
        if !(1..=MAX_EXEC_TIMEOUT_MS).contains(&self.timeout_ms) {
            return Err(format!("timeoutMs must be 1 to {MAX_EXEC_TIMEOUT_MS}"));
        }
        Ok(())
    }
}

/// The name a key's window is found by. A window name keeps only
/// `[A-Za-z0-9_-]` and folds the rest to `_` ([`crate::identity`]), which
/// would make `a.b` and `a_b` one window; escaping `_` and `.` keeps every key
/// its own name, and [`key_of`] reads it back. Nothing but this program makes
/// windows on its socket, so the name alone is the identity: no stamp.
fn name_of(key: &str) -> String {
    let mut name = String::with_capacity(key.len() + 4);
    for c in key.chars() {
        match c {
            '_' => name.push_str("_u"),
            '.' => name.push_str("_d"),
            c => name.push(c),
        }
    }
    name
}

/// The key a window name carries, or `None` for a name [`name_of`] never
/// makes.
fn key_of(name: &str) -> Option<String> {
    let mut key = String::with_capacity(name.len());
    let mut chars = name.chars();
    while let Some(c) = chars.next() {
        match c {
            '_' => match chars.next()? {
                'u' => key.push('_'),
                'd' => key.push('.'),
                _ => return None,
            },
            c => key.push(c),
        }
    }
    check_key(&key).ok().map(|()| key)
}

fn owner(name: &str) -> Owner<'_> {
    Owner::new("", name)
}

/// The key's agent window, where one listing places it.
fn locate(backend: &dyn SessionBackend, key: &str) -> Result<Located, String> {
    backend
        .locate(owner(&name_of(key)))
        .map(|placed| placed.agent)
        .map_err(|e| e.to_string())
}

/// Whether the pane's program runs: `Some(code)` once it exited (`None` inside
/// for a signal), `None` while it runs, `Err` when the server cannot tell.
fn exited(backend: &dyn SessionBackend, pane: &str) -> Result<Option<Option<i64>>, String> {
    let mut state = backend.pane_state(pane).map_err(|e| e.to_string())?;
    if state.dead == Some(true) && state.reaped == Some(false) {
        // tmux before 3.7 reads a pane dead before it reaps the program.
        backend.reap_exited();
        state = backend.pane_state(pane).map_err(|e| e.to_string())?;
    }
    match (state.dead, state.reaped) {
        (Some(false), _) => Ok(None),
        (Some(true), Some(false)) => Err("the process exited and is not reaped yet".into()),
        (Some(true), _) => Ok(Some(state.exit_status)),
        (None, _) => Err("the multiplexer did not say whether the process runs".into()),
    }
}

fn start(backend: &dyn SessionBackend, request: Start) -> Value {
    // The one Known failure: nothing can have started without the binary.
    if let Err(e) = backend.check_available() {
        return known(json!({ "failed": { "why": e.to_string() } }));
    }
    match locate(backend, &request.key) {
        Err(why) => return unknown(why),
        Ok(Located::Unknown) => {
            return unknown("several windows answer to the key and none is stamped with it")
        }
        Ok(Located::At(pane)) => match exited(backend, &pane) {
            Err(why) => return unknown(why),
            Ok(None) => return known(json!({ "started": { "id": pane, "adopted": true } })),
            Ok(Some(_)) => {
                if let Err(e) = backend.kill(&pane) {
                    return unknown(e);
                }
            }
        },
        Ok(Located::Absent) => {}
    }
    let name = name_of(&request.key);
    let spec = WindowSpec {
        owner: owner(&name),
        role: WindowRole::Agent,
        command: &request.argv[0],
        args: &request.argv[1..],
        cwd: request.dir.as_deref(),
        env: &request.env,
    };
    // An error after the create went out may still have made the window, so
    // it is Unknown: the next start adopts it.
    let pane = match backend.create_window(&spec) {
        Ok(pane) => pane,
        Err(e) => return unknown(e),
    };
    if let Err(e) = backend.set_pane_retention(&pane, true) {
        return unknown(e);
    }
    known(json!({ "started": { "id": pane.as_str(), "adopted": false } }))
}

fn inspect(backend: &dyn SessionBackend, key: &str) -> Value {
    match locate(backend, key) {
        Err(why) => unknown(why),
        Ok(Located::Unknown) => unknown("several windows answer to the key"),
        Ok(Located::Absent) => known(json!("none")),
        Ok(Located::At(pane)) => match exited(backend, &pane) {
            Err(why) => unknown(why),
            Ok(None) => known(json!({ "running": { "id": pane } })),
            Ok(Some(code)) => known(json!({ "exited": { "id": pane, "code": code } })),
        },
    }
}

fn stop(backend: &dyn SessionBackend, key: &str) -> Value {
    match locate(backend, key) {
        Err(why) => unknown(why),
        Ok(Located::Unknown) => unknown("several windows answer to the key"),
        Ok(Located::Absent) => known(json!("stopped")),
        Ok(Located::At(pane)) => match backend.kill(&pane) {
            Ok(()) => known(json!("stopped")),
            Err(e) => unknown(e),
        },
    }
}

fn list(backend: &dyn SessionBackend) -> Value {
    match backend.discover() {
        Err(e) => unknown(e),
        Ok(windows) => {
            let prefix = window_name_for(WindowRole::Agent, "");
            let mut keys: Vec<String> = windows
                .iter()
                .filter(|w| w.role == WindowRole::Agent)
                .filter_map(|w| key_of(w.name.strip_prefix(&prefix)?))
                .collect();
            keys.sort();
            keys.dedup();
            known(json!({ "keys": keys }))
        }
    }
}

fn input(backend: &dyn SessionBackend, request: Input) -> Value {
    match locate(backend, &request.key) {
        Err(why) => unknown(why),
        Ok(Located::Unknown) => unknown("several windows answer to the key"),
        Ok(Located::Absent) => known(json!("none")),
        Ok(Located::At(pane)) => match backend.send_text(&pane, &request.text, request.submit) {
            Ok(()) => known(json!("sent")),
            Err(e) => unknown(e),
        },
    }
}

fn capture(backend: &dyn SessionBackend, request: Capture) -> Value {
    let pane = match locate(backend, &request.key) {
        Err(why) => return unknown(why),
        Ok(Located::Unknown) => return unknown("several windows answer to the key"),
        Ok(Located::Absent) => return known(json!("none")),
        Ok(Located::At(pane)) => pane,
    };
    let lines = request.lines.unwrap_or(DEFAULT_CAPTURE_LINES);
    let text = match backend.capture(&pane, lines, false) {
        Ok(text) => text,
        Err(e) => return unknown(e),
    };
    let state = backend.pane_state(&pane).unwrap_or_default();
    known(json!({ "screen": {
        "text": text,
        "cursorRow": state.cursor_row,
        "cursorCol": state.cursor_col,
        "foreground": state.foreground_process,
    }}))
}

/// One bounded command on this machine: its process group is killed past the
/// deadline, and once it exits, and each stream is kept up to the cap.
fn exec(request: Exec) -> Value {
    let cap = request
        .output_cap
        .unwrap_or(DEFAULT_OUTPUT_CAP)
        .min(MAX_OUTPUT_CAP);
    let mut command = Command::new(&request.argv[0]);
    command
        .args(&request.argv[1..])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(dir) = &request.dir {
        command.current_dir(dir);
    }
    #[cfg(unix)]
    std::os::unix::process::CommandExt::process_group(&mut command, 0);
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(e) => return known(json!({ "failed": { "why": e.to_string() } })),
    };
    let read = |pipe: Option<Box<dyn Read + Send>>| {
        std::thread::spawn(move || {
            let mut kept = Vec::new();
            if let Some(pipe) = pipe {
                let _ = pipe.take(cap as u64).read_to_end(&mut kept);
            }
            String::from_utf8_lossy(&kept).into_owned()
        })
    };
    let stdout = read(
        child
            .stdout
            .take()
            .map(|p| Box::new(p) as Box<dyn Read + Send>),
    );
    let stderr = read(
        child
            .stderr
            .take()
            .map(|p| Box::new(p) as Box<dyn Read + Send>),
    );
    let deadline = Instant::now() + Duration::from_millis(request.timeout_ms);
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(10)),
            Ok(None) | Err(_) => break None,
        }
    };
    // Whatever the command left in its group goes with it, so no grandchild
    // holds a pipe open past the bound.
    kill_tree(&mut child);
    let (stdout, stderr) = (
        stdout.join().unwrap_or_default(),
        stderr.join().unwrap_or_default(),
    );
    match status {
        Some(status) => known(json!({ "exited": {
            "code": status.code(),
            "stdout": stdout,
            "stderr": stderr,
        }})),
        None => known(json!({ "timedOut": { "stdout": stdout, "stderr": stderr } })),
    }
}

/// Kill `child`, if it still runs, and the process group it leads.
fn kill_tree(child: &mut std::process::Child) {
    #[cfg(unix)]
    if let Ok(pid) = i32::try_from(child.id()) {
        // SAFETY: a plain syscall on a group this process created for the
        // child (`process_group(0)`); a group already gone is ESRCH.
        unsafe {
            libc::kill(-pid, libc::SIGKILL);
        }
    }
    let _ = child.kill();
    let _ = child.wait();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_key_has_a_window_name_of_its_own_and_reads_back() {
        for key in ["fleet.0f3a-uid", "a.b", "a_b", "a__d", "_u.", "x"] {
            let name = name_of(key);
            assert_eq!(
                crate::identity::sanitize_window_name(&name),
                name,
                "a window keeps the name as given"
            );
            assert_eq!(key_of(&name).as_deref(), Some(key));
        }
        assert_ne!(name_of("a.b"), name_of("a_b"));
        assert_eq!(key_of("a_x"), None);
        assert_eq!(key_of("trailing_"), None);
    }
}
