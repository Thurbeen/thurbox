//! The `runtime-mux` binary speaking the runtime protocol
//! (docs/runtime/README.md), end to end: the real binary, a real tmux, a
//! private socket directory per test.

#![cfg(unix)]

use std::io::Write;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

#[path = "support/tmux_server.rs"]
mod tmux_server;

/// The socket the runtime names when nothing overrides it.
const SOCKET: &str = "runtime-mux";

/// The interface's socket, which the guard pins and the runtime ignores.
const INTERFACE_SOCKET: &str = "thurbox-interface";

struct Runtime {
    server: tmux_server::TmuxServer,
}

impl Runtime {
    /// `None`, and a note, where tmux is not installed.
    fn new() -> Option<Runtime> {
        if runtime_mux::path_lookup::resolve_on_path("tmux").is_none() {
            eprintln!("skipping: tmux is not installed");
            return None;
        }
        Some(Runtime {
            server: tmux_server::TmuxServer::private(INTERFACE_SOCKET),
        })
    }

    fn call(&self, verb: &str, request: Value) -> Value {
        let mut command = Command::new(env!("CARGO_BIN_EXE_runtime-mux"));
        // The guard pins the interface's socket name, as a runtime started
        // from a thurbox pane inherits it; the runtime must not use it.
        self.server.scope(&mut command);
        let mut child = command
            .arg(verb)
            .env_remove("TMUX")
            .env_remove("RUNTIME_MUX_SOCKET")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("run runtime-mux");
        child
            .stdin
            .take()
            .expect("stdin")
            .write_all(request.to_string().as_bytes())
            .expect("write the request");
        let out = child.wait_with_output().expect("runtime-mux output");
        assert!(
            out.status.success(),
            "runtime-mux {verb} exited {:?}: {}",
            out.status,
            String::from_utf8_lossy(&out.stderr)
        );
        serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
            panic!(
                "runtime-mux {verb} answered no JSON ({e}): {}",
                String::from_utf8_lossy(&out.stdout)
            )
        })
    }

    fn start(&self, key: &str, argv: &[&str]) -> Value {
        self.call(
            "start",
            json!({"protocol": 1, "key": key, "argv": argv, "env": {}}),
        )
    }

    fn inspect(&self, key: &str) -> Value {
        self.call("inspect", json!({"protocol": 1, "key": key}))
    }

    /// The first `inspect` answer `done` accepts, within ten seconds.
    fn inspect_until(&self, key: &str, done: impl Fn(&Value) -> bool) -> Value {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let answer = self.inspect(key);
            if done(&answer) || Instant::now() >= deadline {
                return answer;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}

fn started(answer: &Value) -> (String, bool) {
    let started = &answer["known"]["started"];
    assert!(started.is_object(), "{answer}");
    (
        started["id"].as_str().expect("id").to_string(),
        started["adopted"].as_bool().expect("adopted"),
    )
}

#[test]
fn the_manifest_names_the_protocol_and_the_verbs() {
    let Some(rt) = Runtime::new() else { return };
    let answer = rt.call("manifest", json!({"protocol": 1}));
    assert_eq!(answer["known"]["protocol"], 1, "{answer}");
    let verbs = answer["known"]["verbs"].as_array().expect("verbs");
    for verb in ["manifest", "start", "inspect", "stop"] {
        assert!(verbs.iter().any(|v| v == verb), "{verb} in {answer}");
    }
    assert!(
        !verbs.iter().any(|v| v == "workspace"),
        "runtime-mux makes no workspace yet: {answer}"
    );
}

#[test]
fn two_starts_with_one_key_leave_one_process() {
    let Some(rt) = Runtime::new() else { return };
    let (first, adopted) = started(&rt.start("t1.uid", &["sleep", "300"]));
    assert!(!adopted);
    let (second, adopted) = started(&rt.start("t1.uid", &["sleep", "300"]));
    assert!(adopted, "the second start adopts the first process");
    assert_eq!(first, second);

    let running = rt.inspect("t1.uid");
    assert_eq!(
        running["known"]["running"]["id"],
        first.as_str(),
        "{running}"
    );
    let list = rt.call("list", json!({"protocol": 1}));
    assert_eq!(list["known"]["keys"], json!(["t1.uid"]), "{list}");

    assert_eq!(
        rt.call("stop", json!({"protocol": 1, "key": "t1.uid"}))["known"],
        "stopped"
    );
    assert_eq!(rt.inspect("t1.uid")["known"], "none");
    assert_eq!(
        rt.call("stop", json!({"protocol": 1, "key": "t1.uid"}))["known"],
        "stopped",
        "a process already gone counts as stopped"
    );
}

#[test]
fn keys_a_window_name_would_fold_together_stay_apart() {
    let Some(rt) = Runtime::new() else { return };
    let (dot, _) = started(&rt.start("a.b", &["sleep", "300"]));
    let (underscore, adopted) = started(&rt.start("a_b", &["sleep", "300"]));
    assert!(!adopted, "a_b is not a.b");
    assert_ne!(dot, underscore);
    let list = rt.call("list", json!({"protocol": 1}));
    assert_eq!(list["known"]["keys"], json!(["a.b", "a_b"]), "{list}");
    rt.call("stop", json!({"protocol": 1, "key": "a_b"}));
    assert_eq!(rt.inspect("a.b")["known"]["running"]["id"], dot.as_str());
}

#[test]
fn the_runtime_runs_on_its_own_socket_not_the_interfaces() {
    let Some(rt) = Runtime::new() else { return };
    started(&rt.start("own.socket", &["sleep", "300"]));
    assert_eq!(rt.server.alive(), vec![SOCKET.to_string()]);
}

#[test]
fn start_runs_argv_with_env_in_dir() {
    let Some(rt) = Runtime::new() else { return };
    let dir = tempfile::tempdir().expect("dir");
    let answer = rt.call(
        "start",
        json!({
            "protocol": 1,
            "key": "env.dir",
            "argv": ["sh", "-c", "printf %s \"$GREETING\" > out; exec sleep 300"],
            "env": {"GREETING": "hello there"},
            "dir": dir.path(),
        }),
    );
    started(&answer);
    let out = dir.path().join("out");
    let deadline = Instant::now() + Duration::from_secs(10);
    while std::fs::read_to_string(&out).unwrap_or_default().is_empty() && Instant::now() < deadline
    {
        std::thread::sleep(Duration::from_millis(50));
    }
    assert_eq!(std::fs::read_to_string(&out).unwrap(), "hello there");
}

#[test]
fn an_exited_process_reports_its_code() {
    let Some(rt) = Runtime::new() else { return };
    let (id, _) = started(&rt.start("exits", &["sh", "-c", "sleep 0.3; exit 3"]));
    let answer = rt.inspect_until("exits", |a| a["known"]["exited"].is_object());
    assert_eq!(
        answer["known"]["exited"],
        json!({"id": id, "code": 3}),
        "{answer}"
    );
    let (again, adopted) = started(&rt.start("exits", &["sleep", "300"]));
    assert!(!adopted, "an exited process is replaced, not adopted");
    assert_eq!(
        rt.inspect("exits")["known"]["running"]["id"],
        again.as_str()
    );
}

#[test]
fn input_reaches_the_process_and_capture_reads_it_back() {
    let Some(rt) = Runtime::new() else { return };
    started(&rt.start("typed", &["cat"]));
    let sent = rt.call(
        "input",
        json!({"protocol": 1, "key": "typed", "text": "hello runtime", "submit": true}),
    );
    assert_eq!(sent["known"], "sent", "{sent}");
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let screen = rt.call("capture", json!({"protocol": 1, "key": "typed"}));
        let text = screen["known"]["screen"]["text"].as_str().unwrap_or("");
        if text.matches("hello runtime").count() >= 2 {
            break;
        }
        assert!(Instant::now() < deadline, "{screen}");
        std::thread::sleep(Duration::from_millis(100));
    }
    assert_eq!(
        rt.call(
            "input",
            json!({"protocol": 1, "key": "nobody", "text": "x"})
        )["known"],
        "none"
    );
}

#[test]
fn exec_runs_a_bounded_command_on_the_host() {
    let Some(rt) = Runtime::new() else { return };
    let ran = rt.call(
        "exec",
        json!({"protocol": 1, "in": "host", "argv": ["sh", "-c", "echo out; echo err >&2; exit 4"], "timeoutMs": 5000}),
    );
    assert_eq!(
        ran["known"]["exited"],
        json!({"code": 4, "stdout": "out\n", "stderr": "err\n"}),
        "{ran}"
    );
    let slow = rt.call(
        "exec",
        json!({"protocol": 1, "in": "host", "argv": ["sleep", "30"], "timeoutMs": 200}),
    );
    assert!(slow["known"]["timedOut"].is_object(), "{slow}");
}

#[test]
fn a_missing_multiplexer_fails_the_start_by_name() {
    let empty = tempfile::tempdir().expect("an empty PATH");
    let out = Command::new(env!("CARGO_BIN_EXE_runtime-mux"))
        .arg("start")
        .env("PATH", empty.path())
        .env_remove("TMUX")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .and_then(|mut child| {
            child.stdin.take().expect("stdin").write_all(
                json!({"protocol": 1, "key": "k", "argv": ["sleep", "1"]})
                    .to_string()
                    .as_bytes(),
            )?;
            child.wait_with_output()
        })
        .expect("run runtime-mux");
    let answer: Value = serde_json::from_slice(&out.stdout).expect("JSON");
    let why = answer["known"]["failed"]["why"]
        .as_str()
        .unwrap_or_default();
    assert!(why.contains("tmux"), "{answer}");
}

#[test]
fn what_the_runtime_cannot_answer_is_unknown() {
    let Some(rt) = Runtime::new() else { return };
    for (verb, request) in [
        ("inspect", json!({"protocol": 999, "key": "k"})),
        ("inspect", json!({"key": "k"})),
        ("inspect", json!({"protocol": 1, "key": "bad key"})),
        ("start", json!({"protocol": 1, "key": "k", "argv": []})),
        ("no-such-verb", json!({"protocol": 1})),
    ] {
        let answer = rt.call(verb, request.clone());
        assert!(
            answer["unknown"]["why"].is_string(),
            "{verb} {request} answered {answer}"
        );
    }
}
