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
    /// Variables every call sets, such as the runtime's own config.
    env: Vec<(String, String)>,
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
            env: Vec::new(),
        })
    }

    fn call(&self, verb: &str, request: Value) -> Value {
        self.call_on(None, verb, request)
    }

    fn call_on(&self, socket: Option<&str>, verb: &str, request: Value) -> Value {
        let mut command = Command::new(env!("CARGO_BIN_EXE_runtime-mux"));
        // The guard pins the interface's socket name, as a runtime started
        // from a thurbox pane inherits it; the runtime must not use it.
        self.server.scope(&mut command);
        command.env("XDG_DATA_HOME", self.server.tmpdir().join("data"));
        command.envs(self.env.iter().map(|(k, v)| (k, v)));
        command
            .arg(verb)
            .env_remove("TMUX")
            .env_remove("RUNTIME_MUX_SOCKET");
        if let Some(socket) = socket {
            command.env("RUNTIME_MUX_SOCKET", socket);
        }
        let mut child = command
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
    assert_eq!(answer["known"]["protocol"], 2, "{answer}");
    let verbs = answer["known"]["verbs"].as_array().expect("verbs");
    for verb in ["manifest", "start", "inspect", "stop", "shell"] {
        assert!(verbs.iter().any(|v| v == verb), "{verb} in {answer}");
    }
    for verb in ["workspace", "browse", "branches"] {
        assert!(verbs.iter().any(|v| v == verb), "{verb} in {answer}");
    }
    assert_eq!(
        answer["known"]["workspace"],
        json!({"schemes": ["git+https", "git+ssh", "git+file", "file"], "survivesStop": true}),
        "{answer}"
    );
}

/// The place `local` as Fleet passes it when its config names no places.
fn local() -> Value {
    json!({"name": "local", "config": {}})
}

#[test]
fn the_manifest_reports_the_local_place_it_was_asked_about() {
    let Some(rt) = Runtime::new() else { return };
    let answer = rt.call("manifest", json!({"protocol": 2, "places": [local()]}));
    assert_eq!(
        answer["known"]["places"],
        json!([{"name": "local", "platform": "posix", "dynamic": false, "ready": true}]),
        "{answer}"
    );
}

#[test]
fn a_manifest_that_names_no_places_reports_local() {
    let Some(rt) = Runtime::new() else { return };
    for request in [
        json!({"protocol": 1}),
        json!({"protocol": 2}),
        json!({"protocol": 2, "places": []}),
    ] {
        let answer = rt.call("manifest", request.clone());
        let places = answer["known"]["places"].as_array().expect("places");
        assert_eq!(places.len(), 1, "{request}: {answer}");
        assert_eq!(places[0]["name"], "local", "{request}: {answer}");
    }
}

/// An ssh place nothing answers on, a place of a kind runtime-mux does not
/// know and one with a key it does not take are each reported not ready,
/// with why; `local` beside them stays ready.
#[test]
fn a_place_runtime_mux_cannot_serve_is_reported_not_ready_with_why() {
    let Some(rt) = Runtime::new() else { return };
    let closed = std::net::TcpListener::bind("127.0.0.1:0")
        .and_then(|l| l.local_addr())
        .expect("a free port")
        .port()
        .to_string();
    let down = json!({"name": "down", "config": {
        "kind": "ssh", "destination": "127.0.0.1", "ssh_opts": ["-F", "/dev/null", "-p", closed],
    }});
    let vm = json!({"name": "vm", "config": {"kind": "vm"}});
    let old =
        json!({"name": "old", "config": {"kind": "ssh", "destination": "d", "runtime": "mux"}});
    let answer = rt.call(
        "manifest",
        json!({"protocol": 2, "places": [local(), down, vm, old]}),
    );
    let places = answer["known"]["places"].as_array().expect("places");
    assert_eq!(places.len(), 4, "{answer}");
    assert_eq!(places[0]["ready"], true, "{answer}");
    for (i, name, why) in [
        (1, "down", "cannot be reached"),
        (2, "vm", "unknown variant `vm`"),
        (3, "old", "unknown field `runtime`"),
    ] {
        assert_eq!(places[i]["name"], name, "{answer}");
        assert_eq!(places[i]["ready"], false, "{answer}");
        assert!(
            places[i]["why"].as_str().is_some_and(|w| w.contains(why)),
            "{name}: {answer}"
        );
    }
}

/// A place reached over an ssh that never answers: its proxy sleeps, and
/// the place's own `ConnectTimeout`, which wins over runtime-mux's 5 s,
/// gives up only after 30 s.
fn silent_place(name: &str) -> Value {
    json!({"name": name, "config": {
        "kind": "ssh",
        "destination": "silent.invalid",
        "ssh_opts": ["-F", "/dev/null", "-o", "ProxyCommand=sleep 60",
                     "-o", "ConnectTimeout=30"],
    }})
}

/// `exec`'s deadline starts with the call: a place whose `PATH` cannot be
/// read spends the command's time, not time of its own before it.
#[test]
fn a_remote_exec_answers_by_its_deadline() {
    let Some(rt) = Runtime::new() else { return };
    let started = Instant::now();
    let answer = rt.call(
        "exec",
        json!({"protocol": 2, "place": silent_place("silent"), "in": "host",
               "argv": ["true"], "timeoutMs": 500}),
    );
    let took = started.elapsed();
    assert!(answer["known"]["timedOut"].is_object(), "{answer}");
    assert!(took < Duration::from_secs(4), "took {took:?}: {answer}");
}

/// A place that does not answer costs `manifest` a bounded wait, well
/// inside Fleet's 15 s bound on a call, and leaves `local` its report.
#[test]
fn a_silent_place_does_not_hold_up_the_manifest() {
    let Some(rt) = Runtime::new() else { return };
    let started = Instant::now();
    let answer = rt.call(
        "manifest",
        json!({"protocol": 2, "places": [local(), silent_place("silent")]}),
    );
    let took = started.elapsed();
    assert!(took < Duration::from_secs(12), "took {took:?}: {answer}");
    let places = answer["known"]["places"].as_array().expect("places");
    assert_eq!(places[0]["ready"], true, "{answer}");
    assert_eq!(places[1]["ready"], false, "{answer}");
    assert!(
        places[1]["why"]
            .as_str()
            .is_some_and(|w| w.contains("timed out")),
        "{answer}"
    );
}

/// A remote place that does not answer leaves a workspace, a browse and a
/// branches call Unknown: none of them answers about this machine instead,
/// and none is a failure the caller would act on.
#[test]
fn a_remote_place_that_does_not_answer_leaves_its_reads_unknown() {
    let Some(rt) = Runtime::new() else { return };
    let closed = std::net::TcpListener::bind("127.0.0.1:0")
        .and_then(|l| l.local_addr())
        .expect("a free port")
        .port();
    let down = json!({"name": "down", "config": {
        "kind": "ssh",
        "destination": "127.0.0.1",
        "ssh_opts": ["-F", "/dev/null", "-p", closed.to_string()],
    }});
    for (verb, request) in [
        (
            "workspace",
            json!({"key": "w.one", "urls": ["git+https://example.test/app.git"]}),
        ),
        ("browse", json!({})),
        ("branches", json!({"repo": "/r"})),
    ] {
        let mut request = request;
        request["protocol"] = json!(2);
        request["place"] = down.clone();
        let answer = rt.call(verb, request);
        let why = answer["unknown"]["why"].as_str().unwrap_or_default();
        assert!(why.contains("\"down\""), "{verb}: {answer}");
    }
}

#[test]
fn a_missing_multiplexer_makes_the_local_place_not_ready() {
    let empty = tempfile::tempdir().expect("an empty PATH");
    let out = Command::new(env!("CARGO_BIN_EXE_runtime-mux"))
        .arg("manifest")
        .env("PATH", empty.path())
        .env_remove("TMUX")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .and_then(|mut child| {
            child.stdin.take().expect("stdin").write_all(
                json!({"protocol": 2, "places": [local()]})
                    .to_string()
                    .as_bytes(),
            )?;
            child.wait_with_output()
        })
        .expect("run runtime-mux");
    let answer: Value = serde_json::from_slice(&out.stdout).expect("JSON");
    let place = &answer["known"]["places"][0];
    assert_eq!(place["ready"], false, "{answer}");
    assert!(
        place["why"].as_str().is_some_and(|w| w.contains("tmux")),
        "{answer}"
    );
}

#[test]
fn a_version_2_request_runs_at_its_place() {
    let Some(rt) = Runtime::new() else { return };
    let answer = rt.call(
        "start",
        json!({"protocol": 2, "place": local(), "key": "p.one", "argv": ["sleep", "300"], "env": {}}),
    );
    started(&answer);
    let inspected = rt.call(
        "inspect",
        json!({"protocol": 2, "place": local(), "key": "p.one"}),
    );
    assert!(inspected["known"]["running"].is_object(), "{inspected}");
    let list = rt.call("list", json!({"protocol": 2, "place": local()}));
    assert_eq!(list["known"]["keys"], json!(["p.one"]), "{list}");
    // A version 1 request still reaches the same place.
    assert!(rt.inspect("p.one")["known"]["running"].is_object());
}

#[test]
fn a_version_2_request_at_a_place_it_cannot_serve_is_unknown() {
    let Some(rt) = Runtime::new() else { return };
    for place in [
        None,
        Some(json!({"name": "devbox", "config": {"kind": "ssh"}})),
        Some(json!({"name": "local", "config": {"kind": "ssh"}})),
        Some(json!("local")),
    ] {
        let mut request =
            json!({"protocol": 2, "key": "p.two", "argv": ["sleep", "300"], "env": {}});
        if let Some(place) = &place {
            request["place"] = place.clone();
        }
        let answer = rt.call("start", request);
        assert!(answer["unknown"]["why"].is_string(), "{place:?}: {answer}");
    }
    assert_eq!(rt.inspect("p.two")["known"], "none", "nothing started");
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
fn a_started_key_reads_exited_once_its_server_is_gone_until_a_stop() {
    let Some(rt) = Runtime::new() else { return };
    let (id, _) = started(&rt.start("rebooted", &["sleep", "300"]));
    Command::new("tmux")
        .env("TMUX_TMPDIR", rt.server.tmpdir())
        .env_remove("TMUX")
        .args(["-L", SOCKET, "kill-server"])
        .output()
        .expect("run tmux");
    let answer = rt.inspect_until("rebooted", |a| a.get("known").is_some());
    assert_eq!(
        answer["known"],
        json!({"exited": {"id": id, "code": null}}),
        "{answer}"
    );
    let list = rt.call("list", json!({"protocol": 1}));
    assert_eq!(list["known"]["keys"], json!(["rebooted"]), "{list}");

    let (_, adopted) = started(&rt.start("rebooted", &["sleep", "300"]));
    assert!(!adopted, "the lost process is started again");
    assert!(rt.inspect("rebooted")["known"]["running"].is_object());
    assert_eq!(
        rt.call("stop", json!({"protocol": 1, "key": "rebooted"}))["known"],
        "stopped"
    );
    assert_eq!(rt.inspect("rebooted")["known"], "none");
}

#[test]
fn a_stop_on_another_server_keeps_a_started_keys_record() {
    let Some(rt) = Runtime::new() else { return };
    let (id, _) = started(&rt.start("x.started-k", &["sleep", "300"]));
    let other = rt.call_on(
        Some("runtime-mux.started-x"),
        "stop",
        json!({"protocol": 1, "key": "k"}),
    );
    assert_eq!(other["known"], "stopped", "{other}");
    Command::new("tmux")
        .env("TMUX_TMPDIR", rt.server.tmpdir())
        .env_remove("TMUX")
        .args(["-L", SOCKET, "kill-server"])
        .output()
        .expect("run tmux");
    let answer = rt.inspect_until("x.started-k", |a| a.get("known").is_some());
    assert_eq!(
        answer["known"],
        json!({"exited": {"id": id, "code": null}}),
        "{answer}"
    );
}

fn kill_runtime_server(rt: &Runtime) {
    Command::new("tmux")
        .env("TMUX_TMPDIR", rt.server.tmpdir())
        .env_remove("TMUX")
        .args(["-L", SOCKET, "kill-server"])
        .output()
        .expect("run tmux");
}

fn exited_once_known(rt: &Runtime, key: &str, id: &str) {
    let answer = rt.inspect_until(key, |a| a.get("known").is_some());
    assert_eq!(
        answer["known"],
        json!({"exited": {"id": id, "code": null}}),
        "{key}: {answer}"
    );
}

#[test]
fn a_dot_key_reads_exited_once_its_server_is_gone() {
    let Some(rt) = Runtime::new() else { return };
    let ids: Vec<(&str, String)> = [".", ".."]
        .into_iter()
        .map(|key| (key, started(&rt.start(key, &["sleep", "300"])).0))
        .collect();
    kill_runtime_server(&rt);
    for (key, id) in &ids {
        exited_once_known(&rt, key, id);
    }
}

#[test]
fn an_adopting_start_writes_the_record_back() {
    let Some(rt) = Runtime::new() else { return };
    let (id, _) = started(&rt.start("adopted", &["sleep", "300"]));
    let records = rt.server.tmpdir().join("data/runtime-mux/started");
    std::fs::remove_dir_all(&records).expect("the start records");
    let (again, adopted) = started(&rt.start("adopted", &["sleep", "300"]));
    assert!(adopted && again == id, "{again} adopted={adopted}");
    kill_runtime_server(&rt);
    exited_once_known(&rt, "adopted", &id);
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

/// The title an agent sets with OSC 0 or 2 is part of its screen: Fleet's
/// screen rules read it.
#[test]
fn capture_reads_the_title_the_process_sets() {
    let Some(rt) = Runtime::new() else { return };
    started(&rt.start(
        "titled",
        &[
            "sh",
            "-c",
            r"printf '\033]2;⠋ Action Required\007ready\n'; exec sleep 300",
        ],
    ));
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let screen = rt.call("capture", json!({"protocol": 1, "key": "titled"}));
        if screen["known"]["screen"]["title"] == "⠋ Action Required" {
            break;
        }
        assert!(Instant::now() < deadline, "{screen}");
        std::thread::sleep(Duration::from_millis(100));
    }
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

#[test]
fn concurrent_starts_with_one_key_leave_one_process() {
    let Some(rt) = Runtime::new() else { return };
    for round in 0..5 {
        let key = format!("race.r{round}");
        let answers: Vec<Value> = std::thread::scope(|s| {
            let calls: Vec<_> = (0..4)
                .map(|_| s.spawn(|| rt.start(&key, &["sleep", "300"])))
                .collect();
            calls.into_iter().map(|c| c.join().unwrap()).collect()
        });
        let ids: Vec<(String, bool)> = answers.iter().map(started).collect();
        assert!(ids.iter().all(|(id, _)| *id == ids[0].0), "{answers:?}");
        assert_eq!(
            ids.iter().filter(|(_, adopted)| !adopted).count(),
            1,
            "{answers:?}"
        );
        assert!(rt.inspect(&key)["known"]["running"].is_object(), "{key}");
    }
}

#[test]
fn exec_output_past_the_cap_is_dropped_and_the_command_still_finishes() {
    let Some(rt) = Runtime::new() else { return };
    let ran = rt.call(
        "exec",
        json!({"protocol": 1, "in": "host", "argv": ["head", "-c", "4000000", "/dev/zero"], "timeoutMs": 10000, "outputCap": 16}),
    );
    let exited = &ran["known"]["exited"];
    assert_eq!(exited["code"], 0, "{ran}");
    assert_eq!(exited["stdout"].as_str().map(str::len), Some(16), "{ran}");
}

#[test]
fn exec_answers_by_its_deadline_when_an_escaped_child_holds_its_output() {
    let Some(rt) = Runtime::new() else { return };
    if runtime_mux::path_lookup::resolve_on_path("setsid").is_none() {
        eprintln!("skipping: setsid is not installed");
        return;
    }
    let started = Instant::now();
    let ran = rt.call(
        "exec",
        json!({"protocol": 1, "in": "host", "argv": ["sh", "-c", "setsid sleep 20 & echo hi"], "timeoutMs": 1000}),
    );
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "answered after {:?}: {ran}",
        started.elapsed()
    );
    let answer = &ran["known"];
    let stdout = answer["exited"]["stdout"]
        .as_str()
        .or(answer["timedOut"]["stdout"].as_str());
    assert_eq!(stdout, Some("hi\n"), "{ran}");
}

/// A server on the socket tmux derives with no `TMUX_TMPDIR`, killed when
/// the test ends.
struct DefaultDirServer(String);

impl Drop for DefaultDirServer {
    fn drop(&mut self) {
        let _ = Command::new("tmux")
            .env_remove("TMUX_TMPDIR")
            .env_remove("TMUX")
            .args(["-L", &self.0, "kill-server"])
            .output();
    }
}

#[test]
fn concurrent_starts_under_different_tmpdirs_leave_one_process() {
    if runtime_mux::path_lookup::resolve_on_path("tmux").is_none() {
        eprintln!("skipping: tmux is not installed");
        return;
    }
    let server = DefaultDirServer(format!("rmux-tmpdir-{}", std::process::id()));
    let base = std::path::PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(&server.0);
    // Each caller has a TMPDIR of its own and no TMUX_TMPDIR, so tmux puts
    // every one of them on the one socket under /tmp.
    let call = |n: usize, verb: &str, request: Value| -> Value {
        let tmpdir = base.join(format!("caller-{n}"));
        std::fs::create_dir_all(&tmpdir).expect("TMPDIR");
        let mut child = Command::new(env!("CARGO_BIN_EXE_runtime-mux"))
            .arg(verb)
            .env_remove("TMUX_TMPDIR")
            .env_remove("TMUX")
            .env_remove("THURBOX_SOCKET")
            .env_remove("THURBOX_SOCKET_FOR")
            .env("TMPDIR", &tmpdir)
            .env("RUNTIME_MUX_SOCKET", &server.0)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .expect("run runtime-mux");
        child
            .stdin
            .take()
            .expect("stdin")
            .write_all(request.to_string().as_bytes())
            .expect("write the request");
        let out = child.wait_with_output().expect("runtime-mux output");
        serde_json::from_slice(&out.stdout).expect("JSON")
    };
    for round in 0..5 {
        let key = format!("race.t{round}");
        let request = json!({"protocol": 1, "key": key, "argv": ["sleep", "300"], "env": {}});
        let answers: Vec<Value> = std::thread::scope(|s| {
            let calls: Vec<_> = (0..4)
                .map(|n| {
                    let request = request.clone();
                    let call = &call;
                    s.spawn(move || call(n, "start", request))
                })
                .collect();
            calls.into_iter().map(|c| c.join().unwrap()).collect()
        });
        let ids: Vec<(String, bool)> = answers.iter().map(started).collect();
        assert!(ids.iter().all(|(id, _)| *id == ids[0].0), "{answers:?}");
        assert_eq!(
            ids.iter().filter(|(_, adopted)| !adopted).count(),
            1,
            "{answers:?}"
        );
        let inspected = call(0, "inspect", json!({"protocol": 1, "key": key}));
        assert!(inspected["known"]["running"].is_object(), "{inspected}");
    }
}

#[test]
fn a_start_killed_holding_the_lock_does_not_block_the_next_one() {
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::process::CommandExt;
    let Some(rt) = Runtime::new() else { return };
    // A tmux that never answers: the start that took the lock hangs in it.
    let hang = std::path::PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("rmux-hang-{}", std::process::id()));
    std::fs::create_dir_all(&hang).expect("hang dir");
    std::fs::write(hang.join("tmux"), "#!/bin/sh\nexec sleep 30\n").expect("fake tmux");
    std::fs::set_permissions(hang.join("tmux"), std::fs::Permissions::from_mode(0o755))
        .expect("chmod");
    let path = std::env::var("PATH").unwrap_or_default();
    let mut command = Command::new(env!("CARGO_BIN_EXE_runtime-mux"));
    rt.server.scope(&mut command);
    let mut holder = command
        .process_group(0)
        .arg("start")
        .env_remove("TMUX")
        .env_remove("RUNTIME_MUX_SOCKET")
        .env("PATH", format!("{}:{path}", hang.display()))
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .spawn()
        .expect("runtime-mux");
    holder
        .stdin
        .take()
        .expect("stdin")
        .write_all(
            json!({"protocol": 1, "key": "k.held", "argv": ["sleep", "300"], "env": {}})
                .to_string()
                .as_bytes(),
        )
        .expect("request");
    std::thread::sleep(Duration::from_millis(500));
    assert!(
        holder.try_wait().expect("holder").is_none(),
        "the first start hangs holding the lock"
    );
    // SIGKILL: the kernel drops its flock with its descriptors.
    Command::new("kill")
        .args(["-KILL", &format!("-{}", holder.id())])
        .status()
        .expect("SIGKILL");
    holder.wait().expect("reaped");

    let begun = Instant::now();
    let (_, adopted) = started(&rt.start("k.next", &["sleep", "300"]));
    assert!(!adopted);
    assert!(
        begun.elapsed() < Duration::from_secs(5),
        "{:?}",
        begun.elapsed()
    );
}

fn uid() -> String {
    let out = Command::new("id").arg("-u").output().expect("id -u");
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

#[test]
fn the_lock_follows_no_link_planted_where_it_used_to_live() {
    let Some(rt) = Runtime::new() else { return };
    // Before the lock moved into tmux's private directory it lived in the
    // shared one, where anyone could plant a link at its name.
    let planted = rt.server.tmpdir().join("planted");
    std::os::unix::fs::symlink(
        &planted,
        rt.server
            .tmpdir()
            .join(format!("runtime-mux-{}-{SOCKET}.lock", uid())),
    )
    .expect("link");
    started(&rt.start("k.link", &["sleep", "300"]));
    assert!(!planted.exists(), "the lock opened through the link");
}

/// The `workspace-git` program cargo built beside this one, which
/// runtime-mux runs; `None`, and a note, when the workspace has not built it.
fn workspace_git() -> Option<std::path::PathBuf> {
    let found = std::path::Path::new(env!("CARGO_BIN_EXE_runtime-mux"))
        .parent()
        .map(|dir| dir.join("workspace-git"))
        .filter(|p| p.is_file());
    if found.is_none() {
        eprintln!(
            "skipping: no workspace-git binary; build it with `cargo build -p workspace-git`"
        );
    }
    found
}

/// A git command for a fixture, with the variables a hook pins git with
/// cleared.
fn git(dir: &std::path::Path, args: &[&str]) -> String {
    let mut cmd = Command::new("git");
    for var in [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_INDEX_FILE",
        "GIT_COMMON_DIR",
        "GIT_OBJECT_DIRECTORY",
        "GIT_ALTERNATE_OBJECT_DIRECTORIES",
        "GIT_PREFIX",
        "GIT_NAMESPACE",
        "GIT_CONFIG_PARAMETERS",
        "GIT_CONFIG_COUNT",
    ] {
        cmd.env_remove(var);
    }
    let out = cmd
        .current_dir(dir)
        .args(["-c", "user.name=T", "-c", "user.email=t@example.invalid"])
        .args([
            "-c",
            "commit.gpgsign=false",
            "-c",
            "init.defaultBranch=main",
        ])
        .args(args)
        .output()
        .expect("git");
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// A runtime with a directory of its own for repositories, a workspace root
/// and its config, which [`Workspaces::configure`] writes.
struct Workspaces {
    rt: Runtime,
    base: std::path::PathBuf,
    program: std::path::PathBuf,
    _dir: tempfile::TempDir,
}

impl Workspaces {
    /// Configured with no steps.
    fn new() -> Option<Workspaces> {
        let mut ws = Workspaces::unconfigured()?;
        ws.configure("");
        Some(ws)
    }

    fn unconfigured() -> Option<Workspaces> {
        let program = workspace_git()?;
        let rt = Runtime::new()?;
        let dir = tempfile::tempdir().expect("dir");
        let base = dir.path().canonicalize().expect("canonical");
        Some(Workspaces {
            rt,
            base,
            program,
            _dir: dir,
        })
    }

    /// The runtime's config: the root, the program, and `extra` TOML.
    fn configure(&mut self, extra: &str) {
        let config = self.base.join("runtime-mux.toml");
        std::fs::write(
            &config,
            format!(
                "[workspace]\nroot = '{}'\nprogram = '{}'\n{extra}",
                self.base.join("root").display(),
                self.program.display()
            ),
        )
        .expect("config");
        self.rt.env = vec![("RUNTIME_MUX_CONFIG".into(), config.display().to_string())];
    }

    fn step_waiting_for_the_gate(&self) -> String {
        format!(
            "i=0; while [ ! -e \"{}\" ] && [ $i -lt 50 ]; do sleep 0.1; i=$((i + 1)); done",
            self.gate().display()
        )
    }

    fn open_gate(&self) {
        std::fs::write(self.gate(), "").expect("gate");
    }

    fn gate(&self) -> std::path::PathBuf {
        self.base.join("gate")
    }

    fn origin(&self, name: &str) -> String {
        let dir = self.base.join(name);
        std::fs::create_dir_all(&dir).expect("origin");
        git(&dir, &["init", "-q"]);
        std::fs::write(dir.join("README"), "hi\n").expect("README");
        git(&dir, &["add", "README"]);
        git(&dir, &["commit", "-q", "-m", "first"]);
        format!("git+file://{}", dir.display())
    }

    fn workspace(&self, key: &str, urls: &[&str]) -> Value {
        self.rt.call(
            "workspace",
            json!({"protocol": 1, "key": key, "urls": urls}),
        )
    }
}

#[test]
fn a_workspace_is_made_and_a_release_takes_it_with_the_process() {
    let Some(ws) = Workspaces::new() else { return };
    let url = ws.origin("app");
    let made = ws.workspace("w.one", &[&url]);
    let tree = ws.base.join("root/w.one/app");
    assert_eq!(
        made["known"]["made"],
        json!({"dirs": [tree.display().to_string()], "adopted": false}),
        "{made}"
    );
    assert_eq!(
        ws.workspace("w.one", &[&url])["known"]["made"]["adopted"],
        true,
        "a second call adopts"
    );
    let answer = ws.rt.call(
        "start",
        json!({"protocol": 1, "key": "w.one", "argv": ["sleep", "300"], "env": {}, "dir": tree}),
    );
    started(&answer);
    std::fs::write(tree.join("work.txt"), "unsaved").expect("work");
    let stopped = ws.rt.call(
        "stop",
        json!({"protocol": 1, "key": "w.one", "release": true}),
    );
    assert_eq!(stopped["known"], "stopped", "{stopped}");
    assert_eq!(ws.rt.inspect("w.one")["known"], "none");
    assert!(!tree.exists(), "the release removes the worktree");
    // A local checkout's worktree keeps its preserved work in the checkout.
    git(
        &ws.base.join("app"),
        &["rev-parse", "--verify", "refs/fleet/preserved/w.one/app"],
    );
}

#[test]
fn a_stop_without_release_keeps_the_workspace() {
    let Some(ws) = Workspaces::new() else { return };
    let url = ws.origin("app");
    ws.workspace("w.kept", &[&url]);
    let stopped = ws.rt.call("stop", json!({"protocol": 1, "key": "w.kept"}));
    assert_eq!(stopped["known"], "stopped", "{stopped}");
    assert!(ws.base.join("root/w.kept/app/README").is_file());
}

#[test]
fn setup_steps_run_in_a_new_workspace_and_a_failing_one_rolls_it_back() {
    let Some(mut ws) = Workspaces::unconfigured() else {
        return;
    };
    let good = ws.origin("good");
    let bad = ws.origin("bad");
    ws.configure(&format!(
        "[[workspace.setup]]\nmatch = '{good}'\nrun = ['printf ready > setup.txt']\n\n\
         [[workspace.setup]]\nmatch = '{bad}'\nrun = ['exit 3']\n"
    ));
    let made = ws.workspace("w.setup", &[&good]);
    assert_eq!(made["known"]["made"]["adopted"], false, "{made}");
    let tree = ws.base.join("root/w.setup/good");
    assert_eq!(
        std::fs::read_to_string(tree.join("setup.txt")).unwrap_or_default(),
        "ready"
    );
    let failed = ws.workspace("w.broken", &[&bad]);
    let why = failed["known"]["failed"]["why"]
        .as_str()
        .unwrap_or_default();
    assert!(
        why.contains("exit 3") || why.contains("exited 3"),
        "{failed}"
    );
    assert!(!ws.base.join("root/w.broken").exists(), "rolled back");
}

/// A slow workspace call, such as a first clone or a long setup, holds only
/// its own key: a start on another key goes on at once.
#[test]
fn a_slow_workspace_call_does_not_hold_up_a_start_on_another_key() {
    let Some(mut ws) = Workspaces::unconfigured() else {
        return;
    };
    let url = ws.origin("app");
    let step = ws.step_waiting_for_the_gate();
    ws.configure(&format!(
        "[[workspace.setup]]\nmatch = '{url}'\nrun = ['{step}']\n"
    ));
    let ws = &ws;
    std::thread::scope(|scope| {
        let slow = scope.spawn(|| ws.workspace("w.slow", &[url.as_str()]));
        // The worktree exists: the workspace call holds its lock and runs
        // its setup.
        let deadline = Instant::now() + Duration::from_secs(10);
        while !ws.base.join("root/w.slow/app").exists() {
            assert!(Instant::now() < deadline, "the workspace call never began");
            std::thread::sleep(Duration::from_millis(50));
        }
        let begun = Instant::now();
        started(&ws.rt.start("w.other", &["sleep", "300"]));
        let waited = begun.elapsed();
        assert!(
            waited < Duration::from_secs(3),
            "the start waited {waited:?} for another key's workspace"
        );
        ws.open_gate();
        let made = slow.join().expect("the workspace call");
        assert!(made["known"]["made"].is_object(), "{made}");
    });
}

/// A releasing stop runs the workspace's teardown, which may be slow, under
/// its key's lock only: a start on another key goes on at once.
#[test]
fn a_slow_releasing_stop_does_not_hold_up_a_start_on_another_key() {
    let Some(mut ws) = Workspaces::unconfigured() else {
        return;
    };
    let url = ws.origin("app");
    let step = ws.step_waiting_for_the_gate();
    ws.configure(&format!(
        "[[workspace.teardown]]\nmatch = '{url}'\nrun = ['printf x > ../tearing; {step}']\n"
    ));
    let made = ws.workspace("w.down", &[url.as_str()]);
    assert!(made["known"]["made"].is_object(), "{made}");
    let ws = &ws;
    std::thread::scope(|scope| {
        let stopping = scope.spawn(|| {
            ws.rt.call(
                "stop",
                json!({"protocol": 1, "key": "w.down", "release": true}),
            )
        });
        // The teardown runs: the stop holds its locks.
        let deadline = Instant::now() + Duration::from_secs(10);
        while !ws.base.join("root/w.down/tearing").exists() {
            assert!(Instant::now() < deadline, "the teardown never began");
            std::thread::sleep(Duration::from_millis(50));
        }
        let begun = Instant::now();
        started(&ws.rt.start("w.other", &["sleep", "300"]));
        let waited = begun.elapsed();
        assert!(
            waited < Duration::from_secs(3),
            "the start waited {waited:?} for another key's releasing stop"
        );
        ws.open_gate();
        let stopped = stopping.join().expect("the stop");
        assert_eq!(stopped["known"], "stopped", "{stopped}");
    });
}

/// A runtime that stopped after `workspace-git` made the workspace and
/// before its setup finished leaves a workspace that is whole to
/// `workspace-git`. The next call runs the setup steps before it answers.
#[test]
fn a_workspace_whose_setup_never_finished_runs_it_when_adopted() {
    let Some(mut ws) = Workspaces::unconfigured() else {
        return;
    };
    let url = ws.origin("app");
    ws.configure(&format!(
        "[[workspace.setup]]\nmatch = '{url}'\nrun = ['printf ready > setup.txt']\n"
    ));
    // `workspace-git` alone: what a runtime killed during setup leaves.
    let mut child = Command::new(&ws.program)
        .arg("make")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .expect("workspace-git");
    std::io::Write::write_all(
        child.stdin.as_mut().expect("stdin"),
        json!({"root": ws.base.join("root"), "key": "w.half", "urls": [url]})
            .to_string()
            .as_bytes(),
    )
    .expect("request");
    drop(child.stdin.take());
    assert!(child.wait().expect("wait").success());
    let tree = ws.base.join("root/w.half/app");
    assert!(!tree.join("setup.txt").exists());
    let made = ws.workspace("w.half", &[&url]);
    assert!(made["known"]["made"].is_object(), "{made}");
    assert_eq!(
        std::fs::read_to_string(tree.join("setup.txt")).unwrap_or_default(),
        "ready",
        "setup runs before the workspace is answered: {made}"
    );
}

/// A failed setup's discard that `workspace-git` could not finish is no
/// known failure: what is left may still be adopted.
#[cfg(unix)]
#[test]
fn a_discard_that_does_not_finish_leaves_the_workspace_unknown() {
    let Some(mut ws) = Workspaces::unconfigured() else {
        return;
    };
    let stub = ws.base.join("stub-workspace-git");
    let dir = ws.base.join("made");
    std::fs::create_dir_all(&dir).expect("dir");
    std::fs::write(
        &stub,
        format!(
            "#!/bin/sh\ncat >/dev/null\ncase \"$1\" in\n  make) printf '{{\"made\":{{\"dirs\":[\"{}\"],\"adopted\":false}}}}' ;;\n  discard) printf '{{\"unknown\":{{\"why\":\"busy\"}}}}' ;;\nesac\n",
            dir.display()
        ),
    )
    .expect("stub");
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    ws.program = stub;
    ws.configure("[[workspace.setup]]\nmatch = 'file:///x'\nrun = ['exit 3']\n");
    let answer = ws.workspace("w.stuck", &["file:///x"]);
    assert!(answer["unknown"]["why"].is_string(), "{answer}");
}

#[test]
fn teardown_steps_run_before_the_release() {
    let Some(mut ws) = Workspaces::unconfigured() else {
        return;
    };
    let url = ws.origin("app");
    let mark = ws.base.join("torn-down");
    ws.configure(&format!(
        "[[workspace.teardown]]\nmatch = '{url}'\nrun = ['test -f README && printf gone > \"{}\"']\n",
        mark.display()
    ));
    ws.workspace("w.torn", &[&url]);
    let stopped = ws.rt.call(
        "stop",
        json!({"protocol": 1, "key": "w.torn", "release": true}),
    );
    assert_eq!(stopped["known"], "stopped", "{stopped}");
    assert_eq!(std::fs::read_to_string(&mark).unwrap_or_default(), "gone");
}

#[test]
fn a_url_no_scheme_of_the_runtime_handles_is_unsupported() {
    let Some(ws) = Workspaces::new() else { return };
    let answer = ws.workspace("w.hg", &["hg+https://example.invalid/r"]);
    assert_eq!(
        answer["known"]["unsupported"]["url"], "hg+https://example.invalid/r",
        "{answer}"
    );
}

#[test]
fn a_missing_workspace_program_fails_the_workspace_by_name() {
    let Some(mut rt) = Runtime::new() else { return };
    let dir = tempfile::tempdir().expect("dir");
    let config = dir.path().join("runtime-mux.toml");
    std::fs::write(
        &config,
        format!(
            "[workspace]\nroot = '{}'\nprogram = '/nonexistent/workspace-git'\n",
            dir.path().join("root").display()
        ),
    )
    .expect("config");
    rt.env
        .push(("RUNTIME_MUX_CONFIG".into(), config.display().to_string()));
    let notes = dir.path().join("notes");
    std::fs::create_dir_all(&notes).expect("notes");
    let answer = rt.call(
        "workspace",
        json!({"protocol": 1, "key": "w.none", "urls": [format!("file://{}", notes.display())]}),
    );
    let why = answer["known"]["failed"]["why"]
        .as_str()
        .unwrap_or_default();
    assert!(why.contains("workspace-git"), "{answer}");
}

/// `browse` lists a directory of the place: its subdirectories by name, each
/// marked when it is a git checkout. Files are left out, and a directory
/// the request names none of is the place's home.
#[test]
fn browse_lists_the_subdirectories_and_marks_a_repository() {
    let Some(mut rt) = Runtime::new() else { return };
    let dir = tempfile::tempdir().expect("dir");
    let root = dir.path();
    std::fs::create_dir_all(root.join("plain")).expect("plain");
    std::fs::create_dir_all(root.join("repo")).expect("repo");
    git(&root.join("repo"), &["init", "-q"]);
    std::fs::write(root.join("notes.txt"), "a file").expect("file");
    let answer = rt.call(
        "browse",
        json!({"protocol": 2, "place": local(), "dir": root.display().to_string()}),
    );
    assert_eq!(
        answer["known"]["listing"],
        json!({
            "dir": root.display().to_string(),
            "entries": [
                {"name": "plain", "repo": false},
                {"name": "repo", "repo": true},
            ],
        }),
        "{answer}"
    );

    rt.env.push(("HOME".into(), root.display().to_string()));
    let home = rt.call("browse", json!({"protocol": 2, "place": local()}));
    assert_eq!(
        home["known"]["listing"]["dir"],
        root.display().to_string(),
        "{home}"
    );
}

/// A directory that is not there is a failed browse, and a relative one is
/// no request at all.
#[test]
fn browse_of_a_missing_or_relative_directory_is_refused() {
    let Some(rt) = Runtime::new() else { return };
    let missing = rt.call(
        "browse",
        json!({"protocol": 2, "place": local(), "dir": "/nonexistent/tb-browse"}),
    );
    assert!(
        missing["known"]["failed"]["why"]
            .as_str()
            .is_some_and(|why| why.contains("/nonexistent/tb-browse")),
        "{missing}"
    );
    let relative = rt.call(
        "browse",
        json!({"protocol": 2, "place": local(), "dir": "src"}),
    );
    assert!(relative["unknown"]["why"].is_string(), "{relative}");
}

/// `branches` lists a repository's branches on the place, local ones then
/// `origin/*`, with the one a new branch starts from by default.
#[test]
fn branches_lists_a_repositorys_branches_and_its_default() {
    let Some(rt) = Runtime::new() else { return };
    let dir = tempfile::tempdir().expect("dir");
    let repo = dir.path().join("repo");
    std::fs::create_dir_all(&repo).expect("repo");
    git(&repo, &["init", "-q"]);
    git(&repo, &["commit", "-q", "--allow-empty", "-m", "first"]);
    git(&repo, &["branch", "feat"]);
    let answer = rt.call(
        "branches",
        json!({"protocol": 2, "place": local(), "repo": repo.display().to_string()}),
    );
    assert_eq!(
        answer["known"]["branches"],
        json!({"names": ["feat", "main"], "default": "main"}),
        "{answer}"
    );
    // A directory inside a checkout is not its repository.
    let plain = repo.join("plain");
    std::fs::create_dir_all(&plain).expect("plain");
    let refused = rt.call(
        "branches",
        json!({"protocol": 2, "place": local(), "repo": plain.display().to_string()}),
    );
    assert!(refused["known"]["failed"]["why"].is_string(), "{refused}");
}

/// A local branch named like a remote one (`origin/main`) is not `origin`'s
/// default: with no `origin`, the default is the branch checked out.
#[test]
fn a_local_branch_named_like_origins_is_not_its_default() {
    let Some(rt) = Runtime::new() else { return };
    let dir = tempfile::tempdir().expect("dir");
    let repo = dir.path().join("repo");
    std::fs::create_dir_all(&repo).expect("repo");
    git(&repo, &["init", "-q"]);
    git(&repo, &["commit", "-q", "--allow-empty", "-m", "first"]);
    git(&repo, &["branch", "origin/main"]);
    let answer = rt.call(
        "branches",
        json!({"protocol": 2, "place": local(), "repo": repo.display().to_string()}),
    );
    assert_eq!(
        answer["known"]["branches"],
        json!({"names": ["main", "origin/main"], "default": "main"}),
        "{answer}"
    );
}

/// `repo` marks the top of a git work tree, as git reads one: a checkout,
/// or a linked worktree whose `.git` is a file. An empty `.git` directory
/// is not one.
#[test]
fn browse_marks_a_work_tree_as_git_reads_one() {
    let Some(rt) = Runtime::new() else { return };
    let dir = tempfile::tempdir().expect("dir");
    let root = dir.path();
    std::fs::create_dir_all(root.join("fake/.git")).expect("fake");
    std::fs::create_dir_all(root.join("repo")).expect("repo");
    git(&root.join("repo"), &["init", "-q"]);
    git(
        &root.join("repo"),
        &["commit", "-q", "--allow-empty", "-m", "first"],
    );
    git(
        &root.join("repo"),
        &[
            "worktree",
            "add",
            "-q",
            &root.join("linked").display().to_string(),
        ],
    );
    let answer = rt.call(
        "browse",
        json!({"protocol": 2, "place": local(), "dir": root.display().to_string()}),
    );
    assert_eq!(
        answer["known"]["listing"]["entries"],
        json!([
            {"name": "fake", "repo": false},
            {"name": "linked", "repo": true},
            {"name": "repo", "repo": true},
        ]),
        "{answer}"
    );
}

/// `origin`'s default comes first: a clone's `origin/HEAD`, else
/// `origin/main`, else `origin/master`, before the branch checked out.
#[test]
fn branches_defaults_to_origins_default() {
    let Some(rt) = Runtime::new() else { return };
    let dir = tempfile::tempdir().expect("dir");
    let upstream = dir.path().join("upstream");
    std::fs::create_dir_all(&upstream).expect("upstream");
    git(&upstream, &["init", "-q", "-b", "trunk"]);
    git(&upstream, &["commit", "-q", "--allow-empty", "-m", "first"]);
    git(&upstream, &["branch", "master"]);
    let branches = |repo: &std::path::Path| {
        rt.call(
            "branches",
            json!({"protocol": 2, "place": local(), "repo": repo.display().to_string()}),
        )["known"]["branches"]
            .clone()
    };

    // A clone: `origin/HEAD` names upstream's `trunk`.
    git(
        dir.path(),
        &["clone", "-q", &upstream.display().to_string(), "clone"],
    );
    let clone = dir.path().join("clone");
    git(&clone, &["checkout", "-q", "-b", "work"]);
    assert_eq!(
        branches(&clone),
        json!({"names": ["trunk", "work", "origin/master", "origin/trunk"], "default": "origin/trunk"})
    );

    // Fetched, its `origin/HEAD` removed: `origin/master` (no
    // `origin/main`), not the branch checked out.
    let fetched = dir.path().join("fetched");
    std::fs::create_dir_all(&fetched).expect("fetched");
    git(&fetched, &["init", "-q"]);
    git(&fetched, &["commit", "-q", "--allow-empty", "-m", "own"]);
    git(
        &fetched,
        &["remote", "add", "origin", &upstream.display().to_string()],
    );
    git(&fetched, &["fetch", "-q", "origin"]);
    git(&fetched, &["remote", "set-head", "origin", "-d"]);
    assert_eq!(
        branches(&fetched),
        json!({"names": ["main", "origin/master", "origin/trunk"], "default": "origin/master"})
    );
}

/// The names of the key windows on the runtime's own server, leaving out
/// the window a new server starts with.
fn windows(rt: &Runtime) -> Vec<String> {
    let out = Command::new("tmux")
        .env("TMUX_TMPDIR", rt.server.tmpdir())
        .env_remove("TMUX")
        .args(["-L", SOCKET, "list-windows", "-a", "-F", "#{window_name}"])
        .output()
        .expect("run tmux");
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|name| name.starts_with("tb"))
        .map(str::to_string)
        .collect()
}

/// A stream `argv` run as a view runs it, killed when dropped.
struct Stream {
    child: std::process::Child,
    stdin: std::process::ChildStdin,
    frames: std::sync::mpsc::Receiver<term_stream::Down>,
}

impl Stream {
    /// The stream, sized once its hello came and past its first screen,
    /// within one deadline. A stream that ends or stalls first fails the
    /// test, and the drop kills it.
    fn open(argv: &[String]) -> Stream {
        use std::sync::mpsc::RecvTimeoutError;
        use term_stream::{Down, Size, Up};
        let mut child = Command::new(&argv[0])
            .args(&argv[1..])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .expect("run the stream");
        let stdin = child.stdin.take().expect("stdin");
        let mut stdout = child.stdout.take().expect("stdout");
        let (tx, frames) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            while let Ok(Some(frame)) = term_stream::read::<Down>(&mut stdout) {
                if tx.send(frame).is_err() {
                    break;
                }
            }
        });
        let mut stream = Stream {
            child,
            stdin,
            frames,
        };
        let deadline = Instant::now() + Duration::from_secs(30);
        let mut sized = false;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match stream.frames.recv_timeout(left) {
                Ok(Down::Hello(_)) if !sized => {
                    stream.send(&Up::Resize(Size { rows: 24, cols: 80 }));
                    sized = true;
                }
                Ok(Down::Snapshot(_)) if sized => return stream,
                Ok(_) => {}
                Err(RecvTimeoutError::Timeout) => panic!("no first screen within 30 s"),
                Err(RecvTimeoutError::Disconnected) => {
                    panic!("the stream ended before its first screen")
                }
            }
        }
    }

    fn send(&mut self, frame: &term_stream::Up) {
        term_stream::write(&mut self.stdin, frame).expect("send a frame");
    }

    fn type_text(&mut self, text: &str) {
        self.send(&term_stream::Up::Input(text.as_bytes().to_vec()));
    }
}

impl Drop for Stream {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// The text of `file` once it has some, within ten seconds.
fn read_when_written(file: &std::path::Path) -> String {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let text = std::fs::read_to_string(file).unwrap_or_default();
        if !text.is_empty() || Instant::now() >= deadline {
            return text;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// A runtime-mux call that opens a shell runs `/bin/sh` with a home of the
/// test's own: never the operator's shell, never the real home.
fn shell_env(rt: &mut Runtime, home: &std::path::Path) {
    rt.env.push(("SHELL".into(), "/bin/sh".into()));
    rt.env.push(("HOME".into(), home.display().to_string()));
}

#[test]
fn shell_of_a_key_no_process_holds_answers_none() {
    let Some(rt) = Runtime::new() else { return };
    let answer = rt.call(
        "shell",
        json!({"protocol": 2, "place": local(), "key": "no.one"}),
    );
    assert_eq!(answer["known"], "none", "{answer}");
    assert!(windows(&rt).is_empty(), "no shell was opened");
}

#[test]
fn shell_opens_a_shell_in_the_keys_directory_and_a_stop_closes_it() {
    let Some(mut rt) = Runtime::new() else { return };
    let home = tempfile::tempdir().expect("home");
    shell_env(&mut rt, home.path());
    let dir = tempfile::tempdir().expect("dir");
    let dir_path = dir.path().canonicalize().expect("canonical dir");
    started(&rt.call(
        "start",
        json!({"protocol": 2, "place": local(), "key": "sh.one",
               "argv": ["sleep", "300"], "env": {}, "dir": dir_path}),
    ));
    let shell = || {
        rt.call(
            "shell",
            json!({"protocol": 2, "place": local(), "key": "sh.one"}),
        )
    };

    let answer = shell();
    let argv: Vec<String> = serde_json::from_value(answer["known"]["stream"]["argv"].clone())
        .unwrap_or_else(|e| panic!("a stream ({e}): {answer}"));
    assert_eq!(argv[1], "view", "{argv:?}");

    let mut view = Stream::open(&argv);
    view.type_text("pwd > shell-pwd; FLEET_MARK=typed-before\r");
    let pwd = read_when_written(&dir_path.join("shell-pwd"));
    drop(view);
    assert_eq!(
        pwd.trim(),
        dir_path.display().to_string(),
        "the shell runs in the directory the key's process is in"
    );

    // Idempotent by key: a second call opens the shell the first view typed
    // into, which still holds the variable it set.
    let again = shell();
    assert_eq!(again["known"], answer["known"], "the shell is adopted");
    let mut view = Stream::open(&argv);
    view.type_text("printf %s \"$FLEET_MARK\" > shell-mark\r");
    let mark = read_when_written(&dir_path.join("shell-mark"));
    drop(view);
    assert_eq!(mark, "typed-before", "the second call opens the same shell");
    assert_eq!(
        windows(&rt).len(),
        2,
        "one agent, one shell: {:?}",
        windows(&rt)
    );
    // The key's agent is still the only key listed.
    let list = rt.call("list", json!({"protocol": 2, "place": local()}));
    assert_eq!(list["known"]["keys"], json!(["sh.one"]), "{list}");

    assert_eq!(
        rt.call(
            "stop",
            json!({"protocol": 2, "place": local(), "key": "sh.one"})
        )["known"],
        "stopped"
    );
    assert!(
        windows(&rt).is_empty(),
        "the stop closed the shell: {:?}",
        windows(&rt)
    );
    assert_eq!(shell()["known"], "none");
}

/// The clients attached to the runtime's own server.
fn clients(rt: &Runtime) -> Vec<String> {
    String::from_utf8_lossy(&runtime_tmux(rt, &["list-clients", "-F", "#{client_pid}"]).stdout)
        .lines()
        .map(str::to_string)
        .collect()
}

fn clients_gone(rt: &Runtime) -> Vec<String> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let left = clients(rt);
        if left.is_empty() || Instant::now() >= deadline {
            return left;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn view_argv(rt: &Runtime, key: &str) -> Vec<String> {
    let answer = rt.call(
        "attach",
        json!({"protocol": 2, "place": local(), "key": key}),
    );
    serde_json::from_value(answer["known"]["stream"]["argv"].clone())
        .unwrap_or_else(|e| panic!("a stream ({e}): {answer}"))
}

#[test]
fn a_view_killed_by_its_viewer_takes_its_tmux_client_down_with_it() {
    let Some(rt) = Runtime::new() else { return };
    started(&rt.start("viewed", &["sleep", "300"]));
    let view = Stream::open(&view_argv(&rt, "viewed"));
    assert_eq!(clients(&rt).len(), 1, "the view's own client");
    drop(view);
    assert_eq!(
        clients_gone(&rt),
        Vec::<String>::new(),
        "a client outlived its view"
    );
}

/// `tmux <args>` on the runtime's own server.
fn runtime_tmux(rt: &Runtime, args: &[&str]) -> std::process::Output {
    Command::new("tmux")
        .env("TMUX_TMPDIR", rt.server.tmpdir())
        .env_remove("TMUX")
        .args(["-L", SOCKET])
        .args(args)
        .output()
        .expect("run tmux")
}

#[test]
fn a_shell_whose_directory_is_gone_opens_where_the_server_says() {
    let Some(mut rt) = Runtime::new() else { return };
    // The server starts with one home, and the shell call runs with
    // another: on a place that is another machine, the call's own disk and
    // home say nothing about the place's.
    let server_home = tempfile::tempdir().expect("server home");
    let server_home_path = server_home.path().canonicalize().expect("canonical");
    shell_env(&mut rt, &server_home_path);
    let gone = tempfile::tempdir().expect("dir");
    let gone_path = gone.path().canonicalize().expect("canonical dir");
    started(&rt.call(
        "start",
        json!({"protocol": 2, "place": local(), "key": "sh.gone",
               "argv": ["sleep", "300"], "env": {}, "dir": gone_path}),
    ));
    drop(gone);
    let call_home = tempfile::tempdir().expect("call home");
    shell_env(&mut rt, call_home.path());

    let answer = rt.call(
        "shell",
        json!({"protocol": 2, "place": local(), "key": "sh.gone"}),
    );
    let argv: Vec<String> = serde_json::from_value(answer["known"]["stream"]["argv"].clone())
        .unwrap_or_else(|e| panic!("a stream ({e}): {answer}"));
    let out = tempfile::tempdir().expect("out");
    let file = out.path().join("pwd");
    let mut view = Stream::open(&argv);
    view.type_text(&format!("pwd > '{}'\r", file.display()));
    let pwd = read_when_written(&file);
    drop(view);
    assert_eq!(
        pwd.trim(),
        server_home_path.display().to_string(),
        "tmux falls back to the server's home, not the caller's"
    );
}

#[test]
fn a_stop_closes_every_shell_window_of_the_key() {
    let Some(mut rt) = Runtime::new() else { return };
    let home = tempfile::tempdir().expect("home");
    shell_env(&mut rt, home.path());
    started(&rt.start("sh.two", &["sleep", "300"]));
    let shell = json!({"protocol": 2, "place": local(), "key": "sh.two"});
    assert!(rt.call("shell", shell.clone())["known"]["stream"].is_object());
    // A second window of the same name, as a crash between two calls could
    // leave: the key's shell is ambiguous now.
    let name = windows(&rt)
        .into_iter()
        .find(|name| name.starts_with("tbs-"))
        .expect("the shell window");
    let made = runtime_tmux(&rt, &["new-window", "-d", "-n", &name, "sleep 300"]);
    assert!(made.status.success(), "{made:?}");
    assert_eq!(windows(&rt).len(), 3, "{:?}", windows(&rt));

    assert_eq!(
        rt.call("stop", json!({"protocol": 1, "key": "sh.two"}))["known"],
        "stopped",
        "an ambiguous shell does not wedge the stop"
    );
    assert!(windows(&rt).is_empty(), "{:?}", windows(&rt));
}

#[test]
fn a_pause_cut_short_before_the_shell_closed_is_not_paused_until_a_pause_finishes_it() {
    use std::os::unix::fs::MetadataExt;
    let Some(mut rt) = Runtime::new() else { return };
    let home = tempfile::tempdir().expect("home");
    shell_env(&mut rt, home.path());
    started(&rt.start("sh.cut", &["sleep", "300"]));
    let shell = json!({"protocol": 2, "place": local(), "key": "sh.cut"});
    assert!(rt.call("shell", shell)["known"]["stream"].is_object());
    // As a pause cut short after the agent's window went: its mark is
    // written, and the key's shell still runs.
    let listed = runtime_tmux(
        &rt,
        &["list-windows", "-a", "-F", "#{window_id} #{window_name}"],
    );
    let listed = String::from_utf8_lossy(&listed.stdout).into_owned();
    let agent = listed
        .lines()
        .find_map(|l| l.split_once(" tb-").map(|(id, _)| id.to_string()))
        .unwrap_or_else(|| panic!("no agent window in {listed}"));
    let killed = runtime_tmux(&rt, &["kill-window", "-t", &agent]);
    assert!(killed.status.success(), "{killed:?}");
    let uid = std::fs::metadata(rt.server.tmpdir()).expect("tmpdir").uid();
    let marks = rt.server.tmpdir().join(format!("tmux-{uid}"));
    std::fs::write(
        marks.join(format!("runtime-mux-{SOCKET}.paused-sh.cut")),
        "",
    )
    .expect("the mark");

    let inspected = rt.inspect("sh.cut");
    assert!(
        inspected.get("unknown").is_some(),
        "a shell still runs, so the pause has not finished: {inspected}"
    );
    assert_eq!(
        rt.call("pause", json!({"protocol": 1, "key": "sh.cut"}))["known"],
        "paused"
    );
    assert!(windows(&rt).is_empty(), "{:?}", windows(&rt));
    assert_eq!(rt.inspect("sh.cut")["known"], "paused");
}

/// A session's environment reaches its window and no argv: the `tmux`
/// client that makes the window, which every user of the machine can read,
/// is given it on stdin. Names a shell keeps read-only (`UID`, `EUID`,
/// `PPID`, `SHELLOPTS`, `BASHOPTS`) start the window as any other, under
/// bash and dash, and as one token through the window's shell.
#[test]
fn a_sessions_environment_reaches_its_window_and_no_argv() {
    let Some(mut rt) = Runtime::new() else { return };
    let real = runtime_mux::path_lookup::resolve_on_path("tmux").expect("tmux");
    let dir = tempfile::tempdir().expect("dir");
    let log = dir.path().join("argv.log");
    let shim = dir.path().join("tmux");
    std::fs::write(
        &shim,
        format!(
            "#!/bin/sh\nfor a in \"$@\"; do printf '%s\\n' \"$a\"; done >> '{}'\nexec '{}' \"$@\"\n",
            log.display(),
            real.display()
        ),
    )
    .expect("shim");
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    let path = std::env::var("PATH").unwrap_or_default();
    rt.env
        .push(("PATH".into(), format!("{}:{path}", dir.path().display())));
    const SECRET: &str = "s3cr3t-4a7e it's $HOME `x`";
    let env = json!({
        "FLEET_SECRET": SECRET,
        "UID": "4242",
        "EUID": "4242",
        "PPID": "4242",
        "SHELLOPTS": "braceexpand",
        "BASHOPTS": "checkwinsize",
    });
    let mut shells: Vec<&str> = vec!["bash", "sh"];
    if runtime_mux::path_lookup::resolve_on_path("dash").is_some() {
        shells.push("dash");
    }
    let mut cases: Vec<(String, Vec<String>)> = Vec::new();
    for shell in &shells {
        let out = dir.path().join(format!("{shell}.env"));
        let script = format!("env > '{}'; exec sleep 300", out.display());
        cases.push((
            format!("env.{shell}"),
            vec![(*shell).to_string(), "-c".into(), script],
        ));
    }
    // One token: the window's shell splits it, as tmux runs it.
    let out = dir.path().join("token.env");
    cases.push((
        "env.token".into(),
        vec![format!("env > '{}'; exec sleep 300", out.display())],
    ));
    for (key, argv) in &cases {
        let answer = rt.call(
            "start",
            json!({"protocol": 1, "key": key, "argv": argv, "env": env}),
        );
        started(&answer);
    }
    for name in shells
        .iter()
        .map(|s| format!("{s}.env"))
        .chain(["token.env".into()])
    {
        let seen = read_when_written(&dir.path().join(&name));
        assert!(
            seen.lines().any(|l| l == format!("FLEET_SECRET={SECRET}")),
            "{name}: {seen}"
        );
    }
    let argv = std::fs::read_to_string(&log).expect("argv log");
    assert!(argv.contains("source-file"), "{argv}");
    assert!(!argv.contains("s3cr3t"), "{argv}");
    for (key, _) in &cases {
        rt.call("stop", json!({"protocol": 1, "key": key}));
    }
}
