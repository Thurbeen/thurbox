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
        let mut command = Command::new(env!("CARGO_BIN_EXE_runtime-mux"));
        // The guard pins the interface's socket name, as a runtime started
        // from a thurbox pane inherits it; the runtime must not use it.
        self.server.scope(&mut command);
        command.envs(self.env.iter().map(|(k, v)| (k, v)));
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
    assert!(verbs.iter().any(|v| v == "workspace"), "{answer}");
    assert_eq!(
        answer["known"]["workspace"],
        json!({"schemes": ["git+https", "git+ssh", "git+file", "file"], "survivesStop": true}),
        "{answer}"
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
    let sources: Vec<_> = std::fs::read_dir(ws.base.join("root/.sources"))
        .expect("sources")
        .map(|e| e.expect("entry").path())
        .filter(|p| p.is_dir())
        .collect();
    git(
        &sources[0],
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
