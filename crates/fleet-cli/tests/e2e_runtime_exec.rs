//! The `exec` runtime kind end to end: the real `fleet` binary against the
//! real `runtime-mux` program and a real tmux on a private socket directory,
//! and `fleet runtime check` against it and against a runtime that breaks
//! the protocol.
//!
//! Fleet does not link `runtime-mux`: the program is found beside `fleet`
//! in cargo's target directory (built by a workspace build), or at
//! `RUNTIME_MUX_BIN`. Without it or without tmux these tests say so and
//! skip; CI's runtime-protocol job builds it and has tmux.

#![cfg(unix)]

mod support;

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use support::Env;

fn runtime_mux() -> Option<PathBuf> {
    let found = std::env::var_os("RUNTIME_MUX_BIN")
        .map(PathBuf::from)
        .or_else(|| {
            Path::new(env!("CARGO_BIN_EXE_fleet"))
                .parent()
                .map(|dir| dir.join("runtime-mux"))
        })
        .filter(|p| p.is_file());
    if found.is_none() {
        eprintln!("skipping: no runtime-mux binary; build it with `cargo build -p runtime-mux`");
    }
    found
}

fn tmux() -> Option<PathBuf> {
    let found = std::env::var_os("PATH").and_then(|path| {
        std::env::split_paths(&path)
            .map(|dir| dir.join("tmux"))
            .find(|p| p.is_file())
    });
    if found.is_none() {
        eprintln!("skipping: tmux is not installed");
    }
    found
}

/// A private `TMUX_TMPDIR`, short enough for a socket path, whose servers die
/// with it.
struct Sockets(PathBuf);

impl Sockets {
    fn new(name: &str) -> Sockets {
        let base = std::env::var_os("XDG_RUNTIME_DIR")
            .map(PathBuf::from)
            .filter(|d| d.is_dir())
            .unwrap_or_else(std::env::temp_dir);
        let dir = base.join(format!("fx-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("socket dir");
        Sockets(dir)
    }

    fn tmux(&self, args: &[&str]) -> std::process::Output {
        Command::new("tmux")
            .env("TMUX_TMPDIR", &self.0)
            .env_remove("TMUX")
            .args(["-L", "runtime-mux"])
            .args(args)
            .output()
            .expect("tmux")
    }
}

impl Drop for Sockets {
    fn drop(&mut self) {
        let _ = self.tmux(&["kill-server"]);
        let _ = Command::new("tmux")
            .env("TMUX_TMPDIR", &self.0)
            .args(["-L", "runtime-tmux-sh", "kill-server"])
            .output();
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn the_reference_script_passes_the_conformance_suite() {
    if tmux().is_none() {
        return;
    }
    if Command::new("jq").arg("--version").output().is_err() {
        eprintln!("skipping: jq is not installed");
        return;
    }
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/runtimes/tmux.sh");
    let sockets = Sockets::new("script");
    let mut env = Env::new("runtime-check-script");
    env.set_var("TMUX_TMPDIR", &sockets.0.to_string_lossy());
    let run = env.fleet(&["runtime", "check", "--", script.to_str().unwrap()]);
    run.ok()
        .stdout_has("ok   two starts with one key leave one process")
        .stdout_has("ok   concurrent starts with one key leave one process")
        .stdout_has("ok   an exited process answers Exited with its code, or None");
    assert!(!run.stdout.contains("FAIL"), "{}", run.stdout);
}

/// An environment whose one host, `local`, is served by `runtime-mux`.
fn mux_env(name: &str, program: &Path, sockets: &Sockets) -> Env {
    let mut env = Env::new(name);
    fs::write(
        &env.config,
        format!(
            "version = 1\n\n[data]\ndir = \"data\"\n\n[runtimes.mux]\nprogram = '{}'\ncallTimeoutMs = 10000\n\n[hosts.local]\nruntime = \"mux\"\n",
            program.display()
        ),
    )
    .expect("config");
    env.set_var("TMUX_TMPDIR", &sockets.0.to_string_lossy());
    env
}

#[test]
fn runtime_mux_passes_the_conformance_suite() {
    let (Some(program), Some(_)) = (runtime_mux(), tmux()) else {
        return;
    };
    let sockets = Sockets::new("check");
    let env = mux_env("runtime-check", &program, &sockets);
    let run = env.fleet(&["runtime", "check", "--", program.to_str().unwrap()]);
    run.ok()
        .stdout_has("ok   two starts with one key leave one process")
        .stdout_has("ok   concurrent starts with one key leave one process")
        .stdout_has("ok   a protocol mismatch answers Unknown")
        .stdout_has("ok   an error answers Unknown, never None")
        .stdout_has("ok   a stop of an absent process succeeds")
        .stdout_has("ok   workspace: idempotent by key")
        .stdout_has("ok   workspace: release never removes what it did not make");
    assert!(!run.stdout.contains("FAIL"), "{}", run.stdout);
}

#[test]
fn a_runtime_that_answers_none_to_everything_fails_the_check() {
    let env = Env::new("runtime-check-broken");
    let script = env.root.join("broken.sh");
    fs::write(
        &script,
        "#!/bin/sh\ncat >/dev/null\nif [ \"$1\" = manifest ]; then\n  echo '{\"known\":{\"protocol\":1,\"verbs\":[\"manifest\",\"start\",\"inspect\",\"stop\"]}}'\nelse\n  echo '{\"known\":\"none\"}'\nfi\n",
    )
    .expect("script");
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).expect("chmod");
    let run = env.fleet(&["runtime", "check", "--", script.to_str().unwrap()]);
    run.code(1)
        .stdout_has("FAIL a protocol mismatch answers Unknown")
        .stdout_has("FAIL an error answers Unknown, never None")
        .stdout_has("ok   inspect of an absent key answers None");
}

#[test]
fn a_missing_multiplexer_is_reported_by_name_at_the_first_start() {
    let Some(program) = runtime_mux() else { return };
    let sockets = Sockets::new("missing");
    let mut env = mux_env("runtime-mux-missing", &program, &sockets);
    let empty = env.root.join("empty-path");
    fs::create_dir_all(&empty).expect("empty PATH");
    env.only_on_path(&empty);
    let mut task = support::task("t1", true);
    task["spec"]["workspace"] = json!([format!("file://{}", env.root.display())]);
    env.apply(json!([task])).ok();
    env.reconcile();
    let status = env.get("t1")["status"].clone();
    assert!(
        status.to_string().contains("tmux"),
        "the status names the missing multiplexer: {status:#}"
    );
    assert!(status["session"].is_null(), "{status:#}");
}

/// A URL whose scheme the host's runtime does not list is refused before
/// any spawn: the Task waits, and the runtime holds nothing under its key.
#[test]
fn a_scheme_the_runtime_does_not_list_is_refused_before_any_spawn() {
    let (Some(program), Some(_)) = (runtime_mux(), tmux()) else {
        return;
    };
    let sockets = Sockets::new("scheme");
    let mut env = mux_env("runtime-mux-scheme", &program, &sockets);
    env.set_var("RUNTIME_MUX_CONFIG", &mux_config(&env).to_string_lossy());
    let mut task = support::task("t1", true);
    task["spec"]["workspace"] = json!(["hg+https://example.test/app"]);
    env.apply(json!([task])).ok();
    env.reconcile();
    let status = env.get("t1")["status"].clone();
    assert_eq!(status["phase"], "Pending", "{status:#}");
    support::assert_condition(&status, "WorkspaceReady", "False", "WorkspaceUnsupported");
    assert!(
        status.to_string().contains("hg+https://example.test/app"),
        "the status names the URL: {status:#}"
    );
    assert!(status["session"].is_null(), "{status:#}");
    let key = format!(
        "fleet.{}",
        env.get("t1")["metadata"]["uid"].as_str().unwrap()
    );
    assert_eq!(
        mux_call(
            &program,
            &sockets,
            "inspect",
            &json!({"protocol": 1, "key": key})
        ),
        json!({"known": "none"}),
        "nothing was started"
    );
}

/// runtime-mux's own config: its workspace root under the environment.
fn mux_config(env: &Env) -> PathBuf {
    let config = env.root.join("runtime-mux.toml");
    fs::write(
        &config,
        format!("[workspace]\nroot = '{}'\n", env.root.join("ws").display()),
    )
    .expect("runtime-mux config");
    config
}

/// One verb of `runtime-mux` and its answer.
fn mux_call(program: &Path, sockets: &Sockets, verb: &str, request: &Value) -> Value {
    use std::io::Write;
    let out = Command::new(program)
        .arg(verb)
        .env("TMUX_TMPDIR", &sockets.0)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .and_then(|mut child| {
            child
                .stdin
                .take()
                .unwrap()
                .write_all(request.to_string().as_bytes())?;
            child.wait_with_output()
        })
        .expect(verb);
    serde_json::from_slice(&out.stdout).expect("JSON")
}

/// A view asks which hosts a Task may name, and which harnesses Fleet has a
/// profile for, before it applies one.
#[test]
fn the_hosts_and_the_harness_profiles_are_listed_for_a_view() {
    let env = Env::new("runtime-hosts");
    fs::write(
        &env.config,
        "version = 1\n\n[data]\ndir = \"data\"\n\n[runtimes.mux]\nprogram = \"runtime-mux\"\n\n[hosts.local]\nruntime = \"mux\"\n\n[hosts.devbox]\nruntime = \"mux\"\n",
    )
    .expect("config");
    let hosts = env.fleet(&["runtime", "hosts", "--json"]).ok().json();
    assert_eq!(hosts, json!({"hosts": ["devbox", "local"]}));

    // The one `[runtime]` of the linked adapters serves whatever host a Task
    // names: there is no list.
    let single = Env::new("runtime-hosts-single");
    let any = single.fleet(&["runtime", "hosts", "--json"]).ok().json();
    assert_eq!(any, json!({"hosts": null}));

    let version = env.fleet(&["version", "--json"]).ok().json();
    assert_eq!(
        version["harnesses"],
        json!(["claude", "claude-code", "codex", "pi", "opencode", "kilo"])
    );
}

#[test]
fn a_host_naming_no_runtime_is_refused_at_load() {
    let env = Env::new("runtime-unnamed");
    fs::write(
        &env.config,
        "version = 1\n\n[data]\ndir = \"data\"\n\n[runtimes.mux]\nprogram = \"runtime-mux\"\n\n[hosts.local]\nruntime = \"muxx\"\n",
    )
    .expect("config");
    env.fleet(&["list", "task"])
        .code(1)
        .stderr_has("hosts.local.runtime \"muxx\" names no [runtimes.muxx]");
}

#[test]
fn a_task_starts_and_is_released_through_runtime_mux() {
    let (Some(program), Some(_)) = (runtime_mux(), tmux()) else {
        return;
    };
    let sockets = Sockets::new("task");
    let mut env = mux_env("runtime-mux-task", &program, &sockets);
    // The harness profile's program; this one waits and reports nothing.
    let bin = env.root.join("bin");
    fs::create_dir_all(&bin).expect("bin");
    let harness = bin.join("claude");
    fs::write(&harness, "#!/bin/sh\nexec sleep 300\n").expect("harness");
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(&harness, fs::Permissions::from_mode(0o755)).expect("chmod");
    let path = std::env::var("PATH").unwrap_or_default();
    env.only_on_path(Path::new(&format!("{}:{path}", bin.display())));
    let work = env.root.join("work").join("t1");
    fs::create_dir_all(&work).expect("workspace");

    let mut task = support::task("t1", true);
    task["spec"]["workspace"] = json!([format!("file://{}", work.display())]);
    env.apply(json!([task])).ok();
    env.reconcile().ok();

    let session = env.get("t1")["status"]["session"].clone();
    let uid = env.get("t1")["metadata"]["uid"]
        .as_str()
        .unwrap()
        .to_string();
    let key = format!("fleet.{uid}");
    // The session is the key's, whatever process runs under it.
    assert_eq!(session["id"], key.as_str(), "{session:#}");
    assert_eq!(session["host"], "local", "{session:#}");
    let panes = |format: &str| {
        String::from_utf8_lossy(&sockets.tmux(&["list-panes", "-a", "-F", format]).stdout)
            .into_owned()
    };
    let listed = panes("#{pane_id} #{pane_current_path}");
    let id = listed
        .lines()
        .find(|l| l.ends_with(&*work.to_string_lossy()))
        .and_then(|l| l.split(' ').next())
        .unwrap_or_else(|| panic!("no pane in {}: {listed}", work.display()))
        .to_string();

    // A second pass adopts the session rather than starting another.
    env.reconcile().ok();
    assert_eq!(env.get("t1")["status"]["session"]["id"], key.as_str());
    // Beside the session's own first window, which tmux keeps the server on.
    let listed = panes("#{pane_id} #{window_name}");
    assert_eq!(
        listed.lines().filter(|l| l.contains(" tb-")).count(),
        1,
        "{listed}"
    );

    // A running agent is never proven at rest, so the release waits for it;
    // once it exits, the release stops what is left under the key.
    env.fleet(&["delete", "task", "t1"]).ok();
    env.reconcile().ok();
    assert_eq!(env.get("t1")["status"]["phase"], "Releasing");
    sockets.tmux(&["send-keys", "-t", &id, "C-c"]);
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        env.reconcile();
        let gone = env.fleet(&["get", "task", "t1"]).code != Some(0);
        if gone {
            break;
        }
        assert!(Instant::now() < deadline, "{:#}", env.get("t1"));
        std::thread::sleep(Duration::from_millis(200));
    }
    let inspect = Command::new(&program)
        .arg("inspect")
        .env("TMUX_TMPDIR", &sockets.0)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .and_then(|mut child| {
            use std::io::Write;
            child
                .stdin
                .take()
                .unwrap()
                .write_all(json!({"protocol": 1, "key": key}).to_string().as_bytes())?;
            child.wait_with_output()
        })
        .expect("inspect");
    let answer: Value = serde_json::from_slice(&inspect.stdout).expect("JSON");
    assert_eq!(answer, json!({"known": "none"}), "the release stopped it");
}

/// Claude Code as Fleet starts it: it records its argv, reports its start as
/// its SessionStart hook would, and each line typed to it as its prompt hook
/// would, through the `fleet` on its PATH and the file Fleet passed it.
const CLAUDE: &str = r#"#!/bin/sh
{ printf '%s\n' "$@"; echo --; } >> "$PWD/argv.log"
printf '%s' "$FLEET_SIGNAL_FILE" > "$PWD/signal-file"
fleet signal idle
while IFS= read -r line; do
  line=$(printf '%s' "$line" | tr -d '\033' | sed 's/\[20[01]~//g')
  fleet signal prompt "{\"prompt\":\"$line\",\"session_id\":\"conv-1\"}"
  fleet signal idle
done
"#;

/// `env` reconciled until `done` holds, or a panic naming the Task.
fn reconcile_until(env: &Env, what: &str, done: impl Fn(&Env) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(60);
    while !done(env) {
        assert!(
            Instant::now() < deadline,
            "{what}: {:#}",
            env.get("t1")["status"]
        );
        std::thread::sleep(Duration::from_millis(300));
        env.reconcile();
    }
}

/// The argv of each start of the agent, from its own log.
fn starts(work: &Path) -> Vec<Vec<String>> {
    fs::read_to_string(work.join("argv.log"))
        .unwrap_or_default()
        .split("--\n")
        .filter(|s| !s.is_empty())
        .map(|s| s.lines().map(str::to_string).collect())
        .collect()
}

#[test]
fn a_task_on_runtime_mux_reports_through_fleet_signal_and_revives_on_its_conversation() {
    let (Some(program), Some(_)) = (runtime_mux(), tmux()) else {
        return;
    };
    let sockets = Sockets::new("signal");
    let mut env = mux_env("runtime-mux-signal", &program, &sockets);
    let bin = env.root.join("bin");
    fs::create_dir_all(&bin).expect("bin");
    let claude = bin.join("claude");
    fs::write(&claude, CLAUDE).expect("harness");
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(&claude, fs::Permissions::from_mode(0o755)).expect("chmod");
    let fleet_dir = Path::new(env!("CARGO_BIN_EXE_fleet")).parent().unwrap();
    let path = std::env::var("PATH").unwrap_or_default();
    env.only_on_path(Path::new(&format!(
        "{}:{}:{path}",
        bin.display(),
        fleet_dir.display()
    )));
    let work = env.root.join("work").join("t1");
    fs::create_dir_all(&work).expect("workspace");
    let mut task = support::task("t1", true);
    task["spec"]["workspace"] = json!([format!("file://{}", work.display())]);
    // Watched for idleness, which is what reads the agent's activity.
    task["spec"]["configRef"] = json!({"name": "pausing"});
    let config = support::object("TaskConfig", "pausing", json!({"pauseAfterMs": 1}));
    env.apply(json!([config, task])).ok();

    // Started on the harness profile's launch argv, with the file its hooks
    // report to; its start report makes it ready, and its prompt hook proves
    // the brief.
    reconcile_until(&env, "briefed on the prompt hook's proof", |env| {
        env.get("t1")["status"]["phase"] == "Briefed"
    });
    let first = &starts(&work)[0];
    let settings: Vec<&String> = first
        .iter()
        .enumerate()
        .filter(|(_, a)| *a == "--settings")
        .map(|(i, _)| &first[i + 1])
        .collect();
    // Claude Code keeps the last of several --settings: Fleet passes one.
    assert_eq!(settings.len(), 1, "{first:?}");
    let settings: Value = serde_json::from_str(settings[0]).expect("settings JSON");
    for event in ["SessionStart", "UserPromptSubmit", "PreToolUse", "Stop"] {
        let command = settings["hooks"][event][0]["hooks"][0]["command"]
            .as_str()
            .unwrap_or_default();
        assert!(
            command.starts_with("fleet signal "),
            "{event}: {settings:#}"
        );
    }
    let file = fs::read_to_string(work.join("signal-file")).expect("FLEET_SIGNAL_FILE");
    assert!(Path::new(&file).is_file(), "the agent reported to {file:?}");
    reconcile_until(&env, "idle, as its hooks report", |env| {
        env.get("t1")["status"]["sessionState"] == "Idle"
    });
    assert_eq!(env.get("t1")["status"]["conversation"], "conv-1");
    // Idle long enough, but the runtime cannot tell a stopped session, and
    // Unknown never pauses.
    env.reconcile().ok();
    let status = env.get("t1")["status"].clone();
    assert!(
        status["conditions"].as_array().is_some_and(|cs| !cs
            .iter()
            .any(|c| c["type"] == "Paused" && c["status"] == "True")),
        "{status:#}"
    );

    // The agent exits on its own: the session is lost, and revived by a
    // start of the resume argv on the conversation its hooks reported,
    // under the same identity.
    let id = env.get("t1")["status"]["session"]["id"].clone();
    let panes = sockets.tmux(&["list-panes", "-a", "-F", "#{pane_id} #{pane_current_path}"]);
    let listed = String::from_utf8_lossy(&panes.stdout).into_owned();
    let pane = listed
        .lines()
        .find(|l| l.ends_with(&*work.to_string_lossy()))
        .and_then(|l| l.split(' ').next())
        .unwrap_or_else(|| panic!("no pane in {}: {listed}", work.display()))
        .to_string();
    sockets.tmux(&["send-keys", "-t", &pane, "C-c"]);
    reconcile_until(&env, "revived", |_| starts(&work).len() >= 2);
    let second = &starts(&work)[1];
    assert_eq!(
        &second[second.len() - 2..],
        ["--resume", "conv-1"],
        "{second:?}"
    );
    reconcile_until(&env, "the window back", |env| {
        support::condition(&env.get("t1")["status"], "SessionLost")["status"] == "False"
    });
    let status = env.get("t1")["status"].clone();
    assert_eq!(status["session"]["id"], id, "{status:#}");
    assert!(
        status["conditions"].as_array().is_some_and(|cs| !cs
            .iter()
            .any(|c| c["type"] == "WorkerGone" && c["status"] == "True")),
        "{status:#}"
    );
}

/// [`CLAUDE`], except that it logs each line typed to it, and a resumed one
/// reports nothing at its start, as an agent still starting.
const SLOW_CLAUDE: &str = r#"#!/bin/sh
{ printf '%s\n' "$@"; echo --; } >> "$PWD/argv.log"
case " $* " in *" --resume "*) ;; *) fleet signal idle ;; esac
while IFS= read -r line; do
  line=$(printf '%s' "$line" | tr -d '\033' | sed 's/\[20[01]~//g')
  printf '%s\n' "$line" >> "$PWD/typed.log"
  fleet signal prompt "{\"prompt\":\"$line\",\"session_id\":\"conv-1\"}"
  fleet signal idle
done
"#;

#[test]
fn a_revived_agent_is_ready_only_once_it_reports_again() {
    let (Some(program), Some(_)) = (runtime_mux(), tmux()) else {
        return;
    };
    let sockets = Sockets::new("restart");
    let mut env = mux_env("runtime-mux-restart", &program, &sockets);
    let bin = env.root.join("bin");
    fs::create_dir_all(&bin).expect("bin");
    let claude = bin.join("claude");
    fs::write(&claude, SLOW_CLAUDE).expect("harness");
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(&claude, fs::Permissions::from_mode(0o755)).expect("chmod");
    let fleet_dir = Path::new(env!("CARGO_BIN_EXE_fleet")).parent().unwrap();
    let path = std::env::var("PATH").unwrap_or_default();
    env.only_on_path(Path::new(&format!(
        "{}:{}:{path}",
        bin.display(),
        fleet_dir.display()
    )));
    let work = env.root.join("work").join("t1");
    fs::create_dir_all(&work).expect("workspace");
    let mut task = support::task("t1", true);
    task["spec"]["workspace"] = json!([format!("file://{}", work.display())]);
    env.apply(json!([task])).ok();
    reconcile_until(&env, "briefed", |env| {
        env.get("t1")["status"]["phase"] == "Briefed"
    });
    let typed = || {
        fs::read_to_string(work.join("typed.log"))
            .unwrap_or_default()
            .lines()
            .count()
    };
    assert_eq!(typed(), 1, "the brief");

    let panes = sockets.tmux(&["list-panes", "-a", "-F", "#{pane_id} #{pane_current_path}"]);
    let listed = String::from_utf8_lossy(&panes.stdout).into_owned();
    let pane = listed
        .lines()
        .find(|l| l.ends_with(&*work.to_string_lossy()))
        .and_then(|l| l.split(' ').next())
        .unwrap_or_else(|| panic!("no pane in {}: {listed}", work.display()))
        .to_string();
    sockets.tmux(&["send-keys", "-t", &pane, "C-c"]);
    reconcile_until(&env, "revived", |_| starts(&work).len() >= 2);
    // The previous process's last report was idle; the new one has said
    // nothing, so the catch-up note waits.
    for _ in 0..3 {
        std::thread::sleep(Duration::from_millis(300));
        env.reconcile();
    }
    assert_eq!(typed(), 1, "nothing typed before the agent reports");

    // The agent reports its start: the note goes.
    let file = env
        .data
        .join("signals")
        .join(format!("{}.jsonl", env.session_key("t1")));
    let mut cmd = env.command(&["signal", "idle"]);
    cmd.env("FLEET_SIGNAL_FILE", &file);
    support::wait(cmd.spawn().expect("fleet signal")).ok();
    reconcile_until(&env, "the catch-up note typed", |_| typed() >= 2);
}

/// An agent that exists for Fleet only as a harness profile in the user's
/// config dir. It reports through `fleet signal`, as any harness's hook
/// would, and then waits.
const STUB_AGENT: &str = r#"#!/bin/sh
printf '%s' "$FLEET_HARNESS" > "$PWD/harness"
fleet signal working
printf '%s' '{"session_id":"s1"}' | fleet signal idle
exec sleep 300
"#;

const STUB_PROFILE: &str = r#"name = "stub"
program = "stub-agent"
authority = "full"

[payload]
conversation = ["session_id"]
"#;

#[test]
fn a_harness_that_is_only_a_user_profile_runs_and_reports_on_runtime_mux() {
    let (Some(program), Some(_)) = (runtime_mux(), tmux()) else {
        return;
    };
    let sockets = Sockets::new("stub");
    let mut env = mux_env("runtime-mux-stub", &program, &sockets);
    let harnesses = env.config.parent().unwrap().join("harnesses");
    fs::create_dir_all(&harnesses).expect("harnesses dir");
    fs::write(harnesses.join("stub.toml"), STUB_PROFILE).expect("profile");
    let bin = env.root.join("bin");
    fs::create_dir_all(&bin).expect("bin");
    let agent = bin.join("stub-agent");
    fs::write(&agent, STUB_AGENT).expect("agent");
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(&agent, fs::Permissions::from_mode(0o755)).expect("chmod");
    let fleet_dir = Path::new(env!("CARGO_BIN_EXE_fleet")).parent().unwrap();
    let path = std::env::var("PATH").unwrap_or_default();
    env.only_on_path(Path::new(&format!(
        "{}:{}:{path}",
        bin.display(),
        fleet_dir.display()
    )));
    let version = env.fleet(&["version", "--json"]).ok().json();
    assert_eq!(
        version["harnesses"],
        json!([
            "stub",
            "claude",
            "claude-code",
            "codex",
            "pi",
            "opencode",
            "kilo"
        ]),
        "a view offers the user's harness"
    );
    let work = env.root.join("work").join("t1");
    fs::create_dir_all(&work).expect("workspace");
    let mut task = support::task("t1", true);
    task["spec"]["harness"] = json!("stub");
    task["spec"].as_object_mut().unwrap().remove("brief");
    task["spec"].as_object_mut().unwrap().remove("outputs");
    task["spec"]["workspace"] = json!([format!("file://{}", work.display())]);
    env.apply(json!([task])).ok();

    reconcile_until(&env, "idle on conversation s1", |env| {
        let status = env.get("t1")["status"].clone();
        status["agentState"] == "idle" && status["conversation"] == "s1"
    });
    assert_eq!(
        fs::read_to_string(work.join("harness")).unwrap_or_default(),
        "stub",
        "Fleet names the harness to its hooks"
    );
}

/// An agent whose hooks are full but have not reported yet. Its first start
/// draws Codex's approval and sets Codex's title, then reports idle once the
/// test lets it. A second start draws nothing and reports nothing.
const SCREEN_AGENT: &str = r#"#!/bin/sh
echo start >> "$PWD/starts"
if [ "$(wc -l < "$PWD/starts")" -gt 1 ]; then exec sleep 300; fi
printf '\033]2;[ . ] Action Required | stub\007'
printf '  Would you like to run the following command?\n\n  $ touch x\n\n'
printf '\342\200\272 1. Yes, proceed (y)\n\n  Press enter to confirm or esc to cancel\n'
while [ ! -e "$PWD/go" ]; do sleep 0.2; done
fleet signal idle
exec sleep 300
"#;

const SCREEN_PROFILE: &str = r#"name = "stub"
program = "stub-agent"
authority = "full"

[screen]
rules = "codex"
"#;

/// The agent state of a harness whose hooks have not reported is read off
/// its screen, and shown as such; the hooks win once they report, and an
/// agent with neither is unknown, never idle.
#[test]
fn an_agent_state_comes_from_the_screen_below_the_hooks_on_runtime_mux() {
    let (Some(program), Some(_)) = (runtime_mux(), tmux()) else {
        return;
    };
    let sockets = Sockets::new("screen");
    let mut env = mux_env("runtime-mux-screen", &program, &sockets);
    let harnesses = env.config.parent().unwrap().join("harnesses");
    fs::create_dir_all(&harnesses).expect("harnesses dir");
    fs::write(harnesses.join("stub.toml"), SCREEN_PROFILE).expect("profile");
    let bin = env.root.join("bin");
    fs::create_dir_all(&bin).expect("bin");
    let agent = bin.join("stub-agent");
    fs::write(&agent, SCREEN_AGENT).expect("agent");
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(&agent, fs::Permissions::from_mode(0o755)).expect("chmod");
    let fleet_dir = Path::new(env!("CARGO_BIN_EXE_fleet")).parent().unwrap();
    let path = std::env::var("PATH").unwrap_or_default();
    env.only_on_path(Path::new(&format!(
        "{}:{}:{path}",
        bin.display(),
        fleet_dir.display()
    )));
    let work = env.root.join("work").join("t1");
    fs::create_dir_all(&work).expect("workspace");
    let mut task = support::task("t1", true);
    task["spec"]["harness"] = json!("stub");
    task["spec"].as_object_mut().unwrap().remove("brief");
    task["spec"].as_object_mut().unwrap().remove("outputs");
    task["spec"]["workspace"] = json!([format!("file://{}", work.display())]);
    env.apply(json!([task])).ok();

    let shows = |env: &Env, state: &str, source: &str| {
        let status = env.get("t1")["status"].clone();
        status["agentState"] == state && status["activity"]["source"] == source
    };
    reconcile_until(&env, "blocked, from the screen", |env| {
        shows(env, "attention", "screen")
    });
    assert_eq!(
        env.get("t1")["status"]["activity"]["rule"],
        "codex/action-required"
    );
    let shown = env.fleet(&["get", "task", "t1"]).ok().json();
    assert_eq!(
        shown["status"]["activity"]["source"], "screen",
        "fleet get task prints it"
    );

    fs::write(work.join("go"), "").expect("go");
    reconcile_until(&env, "idle, from the hooks", |env| {
        shows(env, "idle", "hook")
    });
    assert!(env.get("t1")["status"]["activity"]["rule"].is_null());

    // Started again: the hooks have not reported since, and the screen
    // shows no rule's text.
    let panes = sockets.tmux(&["list-panes", "-a", "-F", "#{pane_id} #{pane_current_path}"]);
    let listed = String::from_utf8_lossy(&panes.stdout).into_owned();
    let pane = listed
        .lines()
        .find(|l| l.ends_with(&*work.to_string_lossy()))
        .and_then(|l| l.split(' ').next())
        .unwrap_or_else(|| panic!("no pane in {}: {listed}", work.display()))
        .to_string();
    sockets.tmux(&["send-keys", "-t", &pane, "C-c"]);
    reconcile_until(&env, "unknown, from neither", |env| {
        shows(env, "unknown", "none")
            && fs::read_to_string(work.join("starts")).is_ok_and(|s| s.lines().count() >= 2)
    });
}

/// A PATH holding only what the reference script and the conformance cases
/// run, without `flock`: the script takes its lock through perl, as on
/// macOS.
fn path_without_flock(dir: &Path) -> String {
    fs::create_dir_all(dir).expect("PATH dir");
    let path = std::env::var_os("PATH").unwrap_or_default();
    for tool in [
        "sh", "tmux", "jq", "id", "perl", "cat", "awk", "grep", "sleep", "tail", "kill", "rm",
        "env", "ls", "mkdir",
    ] {
        let found = std::env::split_paths(&path)
            .map(|d| d.join(tool))
            .find(|p| p.is_file())
            .unwrap_or_else(|| panic!("no {tool} on PATH"));
        let _ = std::os::unix::fs::symlink(found, dir.join(tool));
    }
    dir.to_string_lossy().into_owned()
}

fn reference_script_runs() -> bool {
    if tmux().is_none() {
        return false;
    }
    for tool in ["jq", "perl"] {
        if Command::new(tool).arg("--version").output().is_err() {
            eprintln!("skipping: {tool} is not installed");
            return false;
        }
    }
    true
}

#[test]
fn the_reference_script_passes_the_conformance_suite_with_its_perl_lock() {
    if !reference_script_runs() {
        return;
    }
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/runtimes/tmux.sh");
    let sockets = Sockets::new("perl");
    let mut env = Env::new("runtime-check-script-perl");
    env.set_var("TMUX_TMPDIR", &sockets.0.to_string_lossy());
    let path = path_without_flock(&env.root.join("no-flock"));
    env.only_on_path(Path::new(&path));
    let run = env.fleet(&["runtime", "check", "--", script.to_str().unwrap()]);
    run.ok()
        .stdout_has("ok   concurrent starts with one key leave one process")
        .stdout_has("ok   two starts with one key leave one process");
    assert!(!run.stdout.contains("FAIL"), "{}", run.stdout);
}

/// A start that takes the lock and hangs in its tmux is SIGKILLed while four
/// more wait: the kernel releases its lock, and the four make one process.
fn a_killed_holder_releases_the_lock(name: &str, path: &str) {
    use std::io::Write;
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::process::CommandExt;
    use std::process::Stdio;
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/runtimes/tmux.sh");
    let sockets = Sockets::new(name);
    let hang = sockets.0.join("hang");
    fs::create_dir_all(&hang).expect("hang dir");
    fs::write(
        hang.join("tmux"),
        "#!/bin/sh\n: > \"$TMUX_TMPDIR/hung\"\nexec sleep 30\n",
    )
    .expect("fake tmux");
    fs::set_permissions(hang.join("tmux"), fs::Permissions::from_mode(0o755)).expect("chmod");
    let request = json!({"protocol": 1, "key": "k.one", "argv": ["sleep", "300"], "env": {}});
    let start = |path: &str| {
        let mut child = Command::new(&script)
            .process_group(0)
            .arg("start")
            .env("TMUX_TMPDIR", &sockets.0)
            .env("PATH", path)
            .env_remove("TMUX")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .expect("tmux.sh");
        child
            .stdin
            .take()
            .unwrap()
            .write_all(request.to_string().as_bytes())
            .expect("request");
        child
    };
    let mut holder = start(&format!("{}:{path}", hang.display()));
    let deadline = Instant::now() + Duration::from_secs(10);
    while !sockets.0.join("hung").exists() {
        assert!(Instant::now() < deadline, "the first start never hung");
        std::thread::sleep(Duration::from_millis(20));
    }
    let waiters: Vec<_> = (0..4).map(|_| start(path)).collect();
    std::thread::sleep(Duration::from_millis(300));
    // SIGKILL, to it and the tmux it waits on: no trap of its runs.
    Command::new("kill")
        .args(["-KILL", &format!("-{}", holder.id())])
        .status()
        .expect("SIGKILL");
    holder.wait().expect("reaped");

    let begun = Instant::now();
    let answers: Vec<Value> = waiters
        .into_iter()
        .map(|w| {
            serde_json::from_slice(&w.wait_with_output().expect("start").stdout).expect("JSON")
        })
        .collect();
    assert!(
        begun.elapsed() < Duration::from_secs(5),
        "waited {:?} for a dead holder",
        begun.elapsed()
    );
    let started: Vec<&Value> = answers.iter().map(|a| &a["known"]["started"]).collect();
    assert!(
        started.iter().all(|s| s["id"] == started[0]["id"]),
        "{answers:?}"
    );
    assert_eq!(
        started.iter().filter(|s| s["adopted"] == false).count(),
        1,
        "{answers:?}"
    );
}

#[test]
fn a_reference_script_killed_holding_its_flock_does_not_block_the_next_starts() {
    if !reference_script_runs() {
        return;
    }
    if Command::new("flock").arg("--version").output().is_err() {
        eprintln!("skipping: flock is not installed");
        return;
    }
    a_killed_holder_releases_the_lock("killed-flock", &std::env::var("PATH").unwrap_or_default());
}

#[test]
fn a_reference_script_killed_holding_its_perl_lock_does_not_block_the_next_starts() {
    if !reference_script_runs() {
        return;
    }
    let dir = std::path::PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("no-flock-{}", std::process::id()));
    a_killed_holder_releases_the_lock("killed-perl", &path_without_flock(&dir));
}

/// Codex as Fleet starts it: its welcome screen and empty composer, the
/// cursor in it, and no hook until a turn ends, when its `notify` runs
/// `fleet signal idle` with the turn's input as its last argument.
const CODEX: &str = r#"#!/bin/sh
{ printf '%s\n' "$@"; echo --; } >> "$PWD/argv.log"
printf '>_ OpenAI Codex (v0.164.0)\n\nTo get started, describe a task or try one of these commands:\n\n'
printf '\342\200\272 Ask Codex to do anything\r\033[2C'
while IFS= read -r line; do
  line=$(printf '%s' "$line" | tr -d '\033' | sed 's/\[20[01]~//g')
  printf '%s\n' "$line" >> "$PWD/typed.log"
  fleet signal idle "{\"type\":\"agent-turn-complete\",\"thread-id\":\"t9\",\"input-messages\":[\"$line\"]}"
done
"#;

#[test]
fn a_codex_task_on_runtime_mux_is_briefed_before_its_first_turn_ends() {
    let (Some(program), Some(_)) = (runtime_mux(), tmux()) else {
        return;
    };
    let sockets = Sockets::new("codex");
    let mut env = mux_env("runtime-mux-codex", &program, &sockets);
    let bin = env.root.join("bin");
    fs::create_dir_all(&bin).expect("bin");
    // Run by a shell named `codex`, so the screen's foreground process is
    // Codex, as it is for the real one.
    let shell = env.root.join("shell");
    fs::create_dir_all(&shell).expect("shell dir");
    std::os::unix::fs::symlink("/bin/sh", shell.join("codex")).expect("shell");
    fs::write(env.root.join("codex.sh"), CODEX).expect("harness");
    let codex = bin.join("codex");
    fs::write(
        &codex,
        format!(
            "#!/bin/sh\nexec '{}' '{}' \"$@\"\n",
            shell.join("codex").display(),
            env.root.join("codex.sh").display()
        ),
    )
    .expect("harness");
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(&codex, fs::Permissions::from_mode(0o755)).expect("chmod");
    let fleet_dir = Path::new(env!("CARGO_BIN_EXE_fleet")).parent().unwrap();
    let path = std::env::var("PATH").unwrap_or_default();
    env.only_on_path(Path::new(&format!(
        "{}:{}:{path}",
        bin.display(),
        fleet_dir.display()
    )));
    let work = env.root.join("work").join("t1");
    fs::create_dir_all(&work).expect("workspace");
    let mut task = support::task("t1", true);
    task["spec"]["harness"] = json!("codex");
    task["spec"]["workspace"] = json!([format!("file://{}", work.display())]);
    env.apply(json!([task])).ok();

    // Nothing reported before the brief: its empty composer on the screen
    // makes it ready, and its trust override trusts it.
    reconcile_until(&env, "briefed on its notify's proof", |env| {
        env.get("t1")["status"]["phase"] == "Briefed"
    });
    let typed = fs::read_to_string(work.join("typed.log")).unwrap_or_default();
    assert_eq!(typed, "Do the work for t1.\n");
    assert_eq!(env.get("t1")["status"]["conversation"], "t9");
}

/// The variables that point git at a repository other than its `-C`
/// directory: a hook that runs these tests exports some of them.
const GIT_LOCATION: &[&str] = &[
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_INDEX_FILE",
    "GIT_COMMON_DIR",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_PREFIX",
    "GIT_NAMESPACE",
];

/// `git` in `dir`, blind to any repository a parent process points at, and
/// what it printed.
fn git(dir: &Path, args: &[&str]) -> String {
    let mut cmd = Command::new("git");
    for var in GIT_LOCATION {
        cmd.env_remove(var);
    }
    let out = cmd
        .arg("-C")
        .arg(dir)
        .args(["-c", "user.name=t", "-c", "user.email=t@example.test"])
        .args(args)
        .output()
        .expect("git");
    assert!(out.status.success(), "git {args:?}: {out:?}");
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// Every preserved ref in `repository`.
fn preserved_refs(repository: &Path) -> Vec<String> {
    git(
        repository,
        &[
            "for-each-ref",
            "--format=%(refname)",
            "refs/fleet/preserved",
        ],
    )
    .lines()
    .map(str::to_string)
    .collect()
}

/// A Task naming a local repository as a `git+file` URL runs in a worktree
/// of that checkout under runtime-mux's workspace root, and its release keeps
/// the work the session left uncommitted in the checkout.
#[test]
fn a_git_url_task_on_runtime_mux_runs_in_a_worktree_and_its_release_keeps_the_work() {
    let (Some(program), Some(_)) = (runtime_mux(), tmux()) else {
        return;
    };
    let sockets = Sockets::new("repo");
    let mut env = mux_env("runtime-mux-repo", &program, &sockets);
    env.set_var("RUNTIME_MUX_CONFIG", &mux_config(&env).to_string_lossy());
    let src = env.root.join("src");
    fs::create_dir_all(&src).expect("source");
    git(&src, &["init", "-q", "-b", "main"]);
    git(&src, &["commit", "-q", "--allow-empty", "-m", "first"]);
    let bin = env.root.join("bin");
    fs::create_dir_all(&bin).expect("bin");
    let claude = bin.join("claude");
    fs::write(&claude, CLAUDE).expect("harness");
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(&claude, fs::Permissions::from_mode(0o755)).expect("chmod");
    let fleet_dir = Path::new(env!("CARGO_BIN_EXE_fleet")).parent().unwrap();
    let path = std::env::var("PATH").unwrap_or_default();
    env.only_on_path(Path::new(&format!(
        "{}:{}:{path}",
        bin.display(),
        fleet_dir.display()
    )));
    let mut task = support::task("t1", true);
    task["spec"]["workspace"] = json!([format!("git+file://{}#main", src.display())]);
    env.apply(json!([task])).ok();

    reconcile_until(&env, "briefed in its worktree", |env| {
        env.get("t1")["status"]["phase"] == "Briefed"
    });
    let status = env.get("t1")["status"].clone();
    let dir = PathBuf::from(
        status["workspace"]["dirs"][0]
            .as_str()
            .unwrap_or_else(|| panic!("no workspace dirs: {status:#}")),
    );
    let root = env.root.join("ws");
    assert!(
        dir.starts_with(&root),
        "{} under {}",
        dir.display(),
        root.display()
    );
    assert!(dir.join(".git").is_file(), "a worktree: {status:#}");
    assert_ne!(dir, src, "never the checkout itself");
    assert_eq!(starts(&dir).len(), 1, "the agent started in the worktree");

    // Work the session leaves uncommitted outlives the release.
    fs::write(dir.join("notes.txt"), "kept\n").expect("work");
    env.fleet(&["delete", "task", "t1"]).ok();
    let deadline = Instant::now() + Duration::from_secs(60);
    while env.fleet(&["get", "task", "t1"]).code == Some(0) {
        assert!(Instant::now() < deadline, "{:#}", env.get("t1"));
        std::thread::sleep(Duration::from_millis(300));
        env.reconcile();
    }
    assert!(!dir.exists(), "the release removed the worktree");
    // The worktree was the checkout's own, so the work is kept there.
    let refs = preserved_refs(&src);
    assert_eq!(refs.len(), 1, "one preserved ref: {refs:?}");
    assert_eq!(
        git(&src, &["show", &format!("{}:notes.txt", refs[0])]),
        "kept\n"
    );
}

fn uid() -> String {
    let out = Command::new("id").arg("-u").output().expect("id -u");
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// One verb of the reference script under `tmpdir`, and its answer.
fn script_call(tmpdir: &Path, verb: &str, request: &Value) -> Value {
    use std::io::Write;
    use std::process::Stdio;
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/runtimes/tmux.sh");
    let mut child = Command::new(script)
        .arg(verb)
        .env("TMUX_TMPDIR", tmpdir)
        .env_remove("TMUX")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("tmux.sh");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(request.to_string().as_bytes())
        .expect("request");
    serde_json::from_slice(&child.wait_with_output().expect("answer").stdout).expect("JSON")
}

#[test]
fn the_reference_script_truncates_nothing_through_a_planted_link() {
    if !reference_script_runs() {
        return;
    }
    let sockets = Sockets::new("planted");
    // Before the lock moved into tmux's private directory it lived in the
    // shared one, where anyone could plant a link at its name.
    let sentinel = sockets.0.join("sentinel");
    fs::write(&sentinel, "keep\n").expect("sentinel");
    std::os::unix::fs::symlink(
        &sentinel,
        sockets
            .0
            .join(format!("runtime-tmux-sh-{}-runtime-tmux-sh.lock", uid())),
    )
    .expect("link");
    let request = json!({"protocol": 1, "key": "k.one", "argv": ["sleep", "300"], "env": {}});
    let answer = script_call(&sockets.0, "start", &request);
    assert!(answer["known"]["started"].is_object(), "{answer}");
    assert_eq!(fs::read_to_string(&sentinel).unwrap(), "keep\n");
}

#[test]
fn the_reference_script_refuses_a_socket_directory_others_can_write() {
    if !reference_script_runs() {
        return;
    }
    use std::os::unix::fs::PermissionsExt;
    let sockets = Sockets::new("foreign");
    let dir = sockets.0.join(format!("tmux-{}", uid()));
    fs::create_dir_all(&dir).expect("socket dir");
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o777)).expect("chmod");
    let request = json!({"protocol": 1, "key": "k.one", "argv": ["sleep", "300"], "env": {}});
    let answer = script_call(&sockets.0, "start", &request);
    let why = answer["unknown"]["why"].as_str().unwrap_or_default();
    assert!(why.contains("writable by others"), "{answer}");
}

/// A stand-in `claude` on the environment's PATH that waits and reports
/// nothing, and the Task `t1` started on it in a directory of its own.
fn waiting_task(env: &mut Env) -> PathBuf {
    task_running(env, "#!/bin/sh\nexec sleep 300\n")
}

/// The Task `t1` started on a stand-in `claude` that runs `script`, and an
/// undispatched `t2`.
fn task_running(env: &mut Env, script: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let bin = env.root.join("bin");
    fs::create_dir_all(&bin).expect("bin");
    let harness = bin.join("claude");
    fs::write(&harness, script).expect("harness");
    fs::set_permissions(&harness, fs::Permissions::from_mode(0o755)).expect("chmod");
    let path = std::env::var("PATH").unwrap_or_default();
    env.only_on_path(Path::new(&format!("{}:{path}", bin.display())));
    // An empty workspace: the reference script makes none, so a session
    // on it runs in the runtime's default directory.
    let work = env.root.join("work").join("t1");
    fs::create_dir_all(&work).expect("workspace");
    env.apply(json!([
        support::task("t1", true),
        support::task("t2", false)
    ]))
    .ok();
    env.reconcile().ok();
    work
}

#[test]
fn fleet_attach_wraps_a_tty_answer_in_its_bridge() {
    if tmux().is_none() {
        return;
    }
    if Command::new("jq").arg("--version").output().is_err() {
        eprintln!("skipping: jq is not installed");
        return;
    }
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/runtimes/tmux.sh");
    let sockets = Sockets::new("attach-tty");
    let mut env = mux_env("attach-tty", &script, &sockets);
    waiting_task(&mut env);

    // A Task that never started has nothing to attach to.
    env.fleet(&["attach", "t2", "--json"])
        .code(3)
        .stderr_has("no session");

    let answer = env.fleet(&["attach", "t1", "--json"]).ok().json();
    let argv: Vec<&str> = answer["stream"]["argv"]
        .as_array()
        .unwrap_or_else(|| panic!("a stream: {answer:#}"))
        .iter()
        .map(|a| a.as_str().expect("a string"))
        .collect();
    assert_eq!(argv[0], env!("CARGO_BIN_EXE_fleet"), "{argv:?}");
    assert_eq!(&argv[1..4], ["term-bridge", "--", "tmux"], "{argv:?}");
    assert!(argv.contains(&"attach-session"), "{argv:?}");
}

#[test]
fn fleet_attach_streams_a_runtime_mux_session_and_a_detach_leaves_it_running() {
    use support::view::View;
    use term_stream::{Capability, Down, Size, Up};
    let (Some(program), Some(_)) = (runtime_mux(), tmux()) else {
        return;
    };
    let sockets = Sockets::new("attach-mux");
    let mut env = mux_env("attach-mux", &program, &sockets);
    task_running(
        &mut env,
        "#!/bin/sh\necho ready-marker\nwhile IFS= read -r l; do echo \"pong-$l\"; done\n",
    );

    let answer = env.fleet(&["attach", "t1", "--json"]).ok().json();
    let argv: Vec<String> = serde_json::from_value(answer["stream"]["argv"].clone())
        .unwrap_or_else(|e| panic!("a stream ({e}): {answer:#}"));
    assert_eq!(Path::new(&argv[0]), program, "{argv:?}");
    assert_eq!(argv[1], "view", "{argv:?}");

    let mut view = View::argv(&argv);
    let Down::Hello(hello) = view.next() else {
        panic!("no hello first")
    };
    assert!(hello.has(Capability::Snapshot), "{hello:?}");
    view.send(&Up::Resize(Size { rows: 24, cols: 80 }));
    view.until("a snapshot", |f| matches!(f, Down::Snapshot(_)));
    view.until_text("ready-marker");
    view.send(&Up::Input(b"ping\r".to_vec()));
    view.until_text("pong-ping");
    view.send(&Up::Detach);
    assert!(view.close().success());

    // The agent keeps running: a second pass adopts it.
    env.reconcile().ok();
    assert_eq!(env.status("t1")["sessionState"], "Running");
}

#[test]
fn term_bridge_relays_a_raw_command_as_a_stream() {
    use std::process::Stdio;
    use support::view::View;
    use term_stream::{Capability, Down, Exit, Size, Up};
    let env = Env::new("term-bridge");
    let mut cmd = env.command(&[
        "term-bridge",
        "--",
        "sh",
        "-c",
        "echo bridged; read l; echo got-$l; exit 3",
    ]);
    cmd.stderr(Stdio::inherit());
    let mut view = View::open(cmd);
    let Down::Hello(hello) = view.next() else {
        panic!("no hello first")
    };
    // A bridge keeps no screen: there is no snapshot to wait for.
    assert!(!hello.has(Capability::Snapshot), "{hello:?}");
    view.send(&Up::Resize(Size {
        rows: 30,
        cols: 100,
    }));
    view.until("the size", |f| {
        *f == Down::Resized(Size {
            rows: 30,
            cols: 100,
        })
    });
    view.until_text("bridged");
    view.send(&Up::Input(b"x\r".to_vec()));
    view.until_text("got-x");
    let exit = view.until("the exit", |f| matches!(f, Down::Exit(_)));
    assert_eq!(exit, Down::Exit(Exit::Known { code: Some(3) }));
}

/// The bridge on `script`, past its hello and sized, its output read up to
/// `ready`.
fn bridge(env: &Env, script: &str, ready: &str) -> support::view::View {
    use term_stream::{Down, Size, Up};
    let mut cmd = env.command(&["term-bridge", "--", "sh", "-c", script]);
    cmd.stderr(std::process::Stdio::inherit());
    let mut view = support::view::View::open(cmd);
    assert!(matches!(view.next(), Down::Hello(_)), "no hello first");
    view.send(&Up::Resize(Size { rows: 24, cols: 80 }));
    view.until_text(ready);
    view
}

#[test]
fn term_bridge_brackets_a_paste_the_program_asked_for() {
    use term_stream::Up;
    let env = Env::new("term-bridge-paste");
    let mut view = bridge(
        &env,
        r#"stty -icanon -echo; printf '\033[?2004h'; echo ready; head -c 20 | od -An -c"#,
        "ready",
    );
    // Six bytes of bracket on each side: twenty in all.
    view.send(&Up::Paste("abcdefgh".into()));
    view.until_text("2   0   0   ~   a   b");
}

#[test]
fn term_bridge_lets_go_of_a_command_that_closed_its_terminal() {
    use term_stream::Up;
    let env = Env::new("term-bridge-detach");
    let mut view = bridge(
        &env,
        "echo started; exec </dev/null >/dev/null 2>&1; exec sleep 300",
        "started",
    );
    std::thread::sleep(Duration::from_millis(300));
    view.send(&Up::Detach);
    view.close();
}

#[test]
fn a_tty_attach_runs_through_the_bridge_end_to_end() {
    use support::view::View;
    use term_stream::{Down, Size, Up};
    if tmux().is_none() {
        return;
    }
    if Command::new("jq").arg("--version").output().is_err() {
        eprintln!("skipping: jq is not installed");
        return;
    }
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/runtimes/tmux.sh");
    let sockets = Sockets::new("attach-bridge");
    let mut env = mux_env("attach-bridge", &script, &sockets);
    task_running(
        &mut env,
        "#!/bin/sh\necho ready-marker\nwhile IFS= read -r l; do echo \"pong-$l\"; done\n",
    );
    let answer = env.fleet(&["attach", "t1", "--json"]).ok().json();
    let argv: Vec<String> = serde_json::from_value(answer["stream"]["argv"].clone())
        .unwrap_or_else(|e| panic!("a stream ({e}): {answer:#}"));
    assert_eq!(argv[1], "term-bridge", "{argv:?}");

    // The view runs the argv as given; the tmux client needs a terminal type.
    let mut cmd = Command::new(&argv[0]);
    cmd.args(&argv[1..])
        .env("TERM", "xterm-256color")
        .env_remove("TMUX");
    let mut view = View::open(cmd);
    assert!(matches!(view.next(), Down::Hello(_)), "no hello first");
    view.send(&Up::Resize(Size { rows: 24, cols: 80 }));
    view.until_text("ready-marker");
    view.send(&Up::Input(b"ping\r".to_vec()));
    view.until_text("pong-ping");
    view.send(&Up::Resize(Size {
        rows: 30,
        cols: 100,
    }));
    view.until("the new size", |f| {
        *f == Down::Resized(Size {
            rows: 30,
            cols: 100,
        })
    });
    view.send(&Up::Detach);
    view.close();

    // The detach ended the tmux client, not the agent.
    env.reconcile().ok();
    assert_eq!(env.status("t1")["sessionState"], "Running");
}

#[test]
fn the_lease_outlasts_one_whole_runtime_operation() {
    let (Some(program), Some(_)) = (runtime_mux(), tmux()) else {
        return;
    };
    let sockets = Sockets::new("lease");
    let env = mux_env("runtime-mux-lease", &program, &sockets);
    // One operation is two calls of up to 2 s each, with their grace: a
    // lease that cannot hold it is refused before anything runs.
    let cfg = fs::read_to_string(&env.config).expect("config");
    fs::write(
        &env.config,
        cfg.replace("callTimeoutMs = 10000", "callTimeoutMs = 2000"),
    )
    .expect("config");
    env.apply(support::task("t1", false)).ok();
    env.fleet(&["reconcile", "--lease-ttl-ms", "5000"])
        .code(1)
        .stderr_has("lease");
    assert!(env.get("t1")["status"]["phase"].is_null(), "nothing ran");
    // The default lease is sized to hold it.
    env.reconcile().ok();
    assert_eq!(env.get("t1")["status"]["phase"], "Pending");
}

#[test]
fn a_message_typed_into_the_agent_is_recorded_as_one_the_route_may_hold() {
    let (Some(program), Some(_)) = (runtime_mux(), tmux()) else {
        return;
    };
    let sockets = Sockets::new("may-hold");
    let mut env = mux_env("runtime-mux-may-hold", &program, &sockets);
    let bin = env.root.join("bin");
    fs::create_dir_all(&bin).expect("bin");
    let claude = bin.join("claude");
    fs::write(&claude, CLAUDE).expect("harness");
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(&claude, fs::Permissions::from_mode(0o755)).expect("chmod");
    let fleet_dir = Path::new(env!("CARGO_BIN_EXE_fleet")).parent().unwrap();
    let path = std::env::var("PATH").unwrap_or_default();
    env.only_on_path(Path::new(&format!(
        "{}:{}:{path}",
        bin.display(),
        fleet_dir.display()
    )));
    let work = env.root.join("work").join("t1");
    fs::create_dir_all(&work).expect("workspace");
    let mut task = support::task("t1", true);
    task["spec"]["workspace"] = json!([format!("file://{}", work.display())]);
    env.apply(json!([task])).ok();
    reconcile_until(&env, "briefed", |env| {
        env.get("t1")["status"]["phase"] == "Briefed"
    });

    env.fleet(&["send", "t1", "are you there?"]).ok();
    reconcile_until(&env, "the message delivered", |env| {
        env.get("t1")["status"]["deliveries"]["receipts"]
            .as_array()
            .is_some_and(|r| !r.is_empty())
    });
    // A typed text may sit in the composer: a fact about the route, which
    // no person can clear, so it raises nothing.
    let status = env.get("t1")["status"].clone();
    let receipt = &status["deliveries"]["receipts"][0];
    assert_eq!(receipt["mayHold"], true, "{status:#}");
    assert_eq!(receipt["deliveredVia"], "input", "{status:#}");
    assert!(
        status["conditions"].as_array().is_some_and(|cs| !cs
            .iter()
            .any(|c| c["type"] == "NeedsAttention" && c["status"] == "True")),
        "{status:#}"
    );
}

/// A coding agent's CLI on the PATH this test runs with. Only a developer's
/// machine has one, signed in: without it a test that drives it says so and
/// skips, and CI never runs one.
fn agent(name: &str) -> Option<PathBuf> {
    let found = std::env::var_os("PATH").and_then(|path| {
        std::env::split_paths(&path)
            .map(|dir| dir.join(name))
            .find(|p| p.is_file())
    });
    if found.is_none() {
        eprintln!("skipping: {name} is not installed");
    }
    found
}

/// Every file under `dir` but those under `skip`, with its bytes.
fn snapshot(dir: &Path, skip: &[&str]) -> BTreeMap<PathBuf, Vec<u8>> {
    let mut files = BTreeMap::new();
    let mut todo = vec![dir.to_path_buf()];
    while let Some(d) = todo.pop() {
        let Ok(entries) = fs::read_dir(&d) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let rel = path.strip_prefix(dir).unwrap().to_path_buf();
            if skip.iter().any(|s| rel.starts_with(s)) {
                continue;
            }
            match entry.file_type() {
                Ok(t) if t.is_dir() => todo.push(path),
                Ok(t) if t.is_file() => {
                    files.insert(rel, fs::read(&path).unwrap_or_default());
                }
                _ => {}
            }
        }
    }
    files
}

/// The states and conversations a session's hooks reported, from its
/// signal file, in order.
fn reported(env: &Env, task: &str) -> Vec<(String, Option<String>)> {
    let file = env
        .data
        .join("signals")
        .join(format!("{}.jsonl", env.session_key(task)));
    fs::read_to_string(file)
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .map(|s| {
            (
                s["state"].as_str().unwrap_or_default().to_string(),
                s["conversation"].as_str().map(str::to_string),
            )
        })
        .collect()
}

/// Whether `want` occurs in `states` in order, others between.
fn in_order(states: &[(String, Option<String>)], want: &[&str]) -> bool {
    let mut want = want.iter().peekable();
    for (s, _) in states {
        if want.peek().is_some_and(|w| *w == s) {
            want.next();
        }
    }
    want.peek().is_none()
}

/// An environment that runs a real agent: the operator's PATH after
/// `fleet`'s directory. Its Task gives the agent the operator's HOME, where
/// its sign-in is; Fleet itself keeps the test's.
fn agent_env(name: &str, program: &Path, sockets: &Sockets) -> Env {
    let mut env = mux_env(name, program, sockets);
    let fleet_dir = Path::new(env!("CARGO_BIN_EXE_fleet")).parent().unwrap();
    let path = std::env::var("PATH").unwrap_or_default();
    env.only_on_path(Path::new(&format!("{}:{path}", fleet_dir.display())));
    env
}

/// The id of the session's pane: the one in `work`.
fn pane_in(sockets: &Sockets, work: &Path) -> String {
    let panes = sockets.tmux(&["list-panes", "-a", "-F", "#{pane_id} #{pane_current_path}"]);
    let listed = String::from_utf8_lossy(&panes.stdout).into_owned();
    listed
        .lines()
        .find(|l| l.ends_with(&*work.to_string_lossy()))
        .and_then(|l| l.split(' ').next())
        .unwrap_or_else(|| panic!("no pane in {}: {listed}", work.display()))
        .to_string()
}

/// Allows, once, each request the agent waits on until `done`. A model may
/// call its shell tool several times in one message, and each call opens a
/// request of its own: the agent waits on a person until the last one is
/// answered. Enter picks "allow once", the default choice, on the request
/// shown first.
fn allow_each_request_until(
    env: &Env,
    sockets: &Sockets,
    work: &Path,
    what: &str,
    done: impl Fn(&Env) -> bool,
) {
    let pane = pane_in(sockets, work);
    reconcile_until(env, what, |env| {
        if reported(env, "t1")
            .last()
            .is_some_and(|(s, _)| s == "blocked")
        {
            sockets.tmux(&["send-keys", "-t", &pane, "Enter"]);
        }
        done(env)
    });
}

/// An agent whose one turn opens three requests, as a model that calls its
/// shell tool three times in one message does. Each Enter answers one.
const ASKING_AGENT: &str = r#"#!/bin/sh
printf '%s' '{"session_id":"s1"}' | fleet signal idle
fleet signal working
for request in 1 2 3; do
  fleet signal blocked
  read -r answer
done
fleet signal working
fleet signal idle
exec sleep 300
"#;

#[test]
fn every_request_of_a_turn_is_allowed_before_it_ends() {
    let (Some(program), Some(_)) = (runtime_mux(), tmux()) else {
        return;
    };
    let sockets = Sockets::new("asking");
    let mut env = mux_env("runtime-mux-asking", &program, &sockets);
    let harnesses = env.config.parent().unwrap().join("harnesses");
    fs::create_dir_all(&harnesses).expect("harnesses dir");
    fs::write(harnesses.join("stub.toml"), STUB_PROFILE).expect("profile");
    let bin = env.root.join("bin");
    fs::create_dir_all(&bin).expect("bin");
    let agent = bin.join("stub-agent");
    fs::write(&agent, ASKING_AGENT).expect("agent");
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(&agent, fs::Permissions::from_mode(0o755)).expect("chmod");
    let fleet_dir = Path::new(env!("CARGO_BIN_EXE_fleet")).parent().unwrap();
    let path = std::env::var("PATH").unwrap_or_default();
    env.only_on_path(Path::new(&format!(
        "{}:{}:{path}",
        bin.display(),
        fleet_dir.display()
    )));
    let work = env.root.join("work").join("t1");
    fs::create_dir_all(&work).expect("workspace");
    let mut task = support::task("t1", true);
    task["spec"]["harness"] = json!("stub");
    task["spec"].as_object_mut().unwrap().remove("brief");
    task["spec"].as_object_mut().unwrap().remove("outputs");
    task["spec"]["workspace"] = json!([format!("file://{}", work.display())]);
    env.apply(json!([task])).ok();

    reconcile_until(&env, "waiting on the first request", |env| {
        env.get("t1")["status"]["agentState"] == "attention"
    });
    allow_each_request_until(&env, &sockets, &work, "the turn ended", |env| {
        in_order(&reported(env, "t1"), &["blocked", "working", "idle"])
            && env.get("t1")["status"]["agentState"] == "idle"
    });
    let blocked = reported(&env, "t1")
        .iter()
        .filter(|(s, _)| s == "blocked")
        .count();
    assert_eq!(blocked, 3, "{:?}", reported(&env, "t1"));
}

/// The agent's process under the session's pane, ended as a crash would.
fn end_agent(sockets: &Sockets, work: &Path) {
    let panes = sockets.tmux(&["list-panes", "-a", "-F", "#{pane_pid} #{pane_current_path}"]);
    let listed = String::from_utf8_lossy(&panes.stdout).into_owned();
    let pid = listed
        .lines()
        .find(|l| l.ends_with(&*work.to_string_lossy()))
        .and_then(|l| l.split(' ').next())
        .unwrap_or_else(|| panic!("no pane in {}: {listed}", work.display()))
        .to_string();
    let _ = Command::new("pkill").args(["-TERM", "-P", &pid]).status();
    let _ = Command::new("kill").args(["-TERM", &pid]).status();
}

/// pi, as Fleet starts it: its hooks are an extension Fleet wrote under its
/// own data directory and passed with `-e`. Its state follows them, its
/// session id is the conversation a revive resumes, and pi's own config
/// directory is left byte for byte as it was. Whatever pi's provider
/// answers, a turn starts and settles.
#[test]
fn pi_reports_its_state_through_the_extension_fleet_passes_at_launch() {
    let (Some(program), Some(_), Some(_)) = (runtime_mux(), tmux(), agent("pi")) else {
        return;
    };
    let home = PathBuf::from(std::env::var("HOME").expect("HOME"));
    let config = std::env::var_os("PI_CODING_AGENT_DIR")
        .map_or_else(|| home.join(".pi/agent"), PathBuf::from);
    // pi's own transcripts: another pi on this machine may write them.
    let before = snapshot(&config, &["sessions"]);
    let sockets = Sockets::new("pi");
    let env = agent_env("runtime-mux-pi", &program, &sockets);
    let work = env.root.join("work").join("t1");
    fs::create_dir_all(&work).expect("workspace");
    let sessions = env.root.join("pi-sessions");
    let mut task = support::task("t1", true);
    task["spec"]["harness"] = json!("pi");
    task["spec"]["brief"] = json!("Reply with the single word ok.");
    task["spec"]["workspace"] = json!([format!("file://{}", work.display())]);
    // No update check, and this test's transcripts in its own directory.
    task["spec"]["env"] = json!({
        "HOME": home.to_string_lossy(),
        "PI_OFFLINE": "1",
        "PI_CODING_AGENT_SESSION_DIR": sessions.to_string_lossy(),
    });
    env.apply(json!([task])).ok();

    reconcile_until(&env, "briefed on pi's prompt hook", |env| {
        env.get("t1")["status"]["phase"] == "Briefed"
    });
    reconcile_until(&env, "the turn settled", |env| {
        in_order(&reported(env, "t1"), &["idle", "prompt", "working", "idle"])
            && env.get("t1")["status"]["agentState"] == "idle"
    });
    let conversation = env.get("t1")["status"]["conversation"]
        .as_str()
        .expect("a conversation")
        .to_string();
    let transcript = snapshot(&sessions, &[])
        .keys()
        .any(|f| f.to_string_lossy().contains(&conversation));
    assert!(
        transcript,
        "pi's session {conversation} is the conversation"
    );

    // pi ends: the revive resumes its session, whose start reports it again.
    end_agent(&sockets, &work);
    reconcile_until(&env, "resumed on its session", |env| {
        let states = reported(env, "t1");
        states
            .iter()
            .rposition(|(s, _)| s == "started")
            .is_some_and(|at| {
                states[at..]
                    .iter()
                    .any(|(s, c)| s == "idle" && c.as_deref() == Some(&*conversation))
            })
    });
    end_agent(&sockets, &work);
    assert!(
        snapshot(&config, &["sessions"]) == before,
        "pi's config directory {} changed",
        config.display()
    );
}

/// opencode, as Fleet starts it: its own server, with Fleet's plugin named
/// in `OPENCODE_CONFIG_CONTENT`, which opencode merges with the user's and
/// the project's config. The project asks before each shell command: the
/// agent waits on a person until each is answered. Its session id is the
/// conversation a revive resumes, and opencode's config directory is left
/// byte for byte as it was.
#[test]
fn opencode_reports_its_state_through_the_plugin_fleet_names_at_launch() {
    let (Some(program), Some(_), Some(_)) = (runtime_mux(), tmux(), agent("opencode")) else {
        return;
    };
    let home = PathBuf::from(std::env::var("HOME").expect("HOME"));
    let config = std::env::var_os("XDG_CONFIG_HOME")
        .map_or_else(|| home.join(".config"), PathBuf::from)
        .join("opencode");
    let before = snapshot(&config, &[]);
    let sockets = Sockets::new("opencode");
    let env = agent_env("runtime-mux-opencode", &program, &sockets);
    let work = env.root.join("work").join("t1");
    fs::create_dir_all(&work).expect("workspace");
    fs::write(
        work.join("opencode.jsonc"),
        r#"{"permissions":[{"action":"shell","resource":"*","effect":"ask"}]}"#,
    )
    .expect("project config");
    let mut task = support::task("t1", true);
    task["spec"]["harness"] = json!("opencode");
    task["spec"]["env"] = json!({"HOME": home.to_string_lossy()});
    task["spec"]["brief"] =
        json!("Run the shell command `echo fleet` yourself, with your shell tool, then stop.");
    task["spec"]["workspace"] = json!([format!("file://{}", work.display())]);
    env.apply(json!([task])).ok();

    reconcile_until(&env, "briefed on opencode's prompt hook", |env| {
        env.get("t1")["status"]["phase"] == "Briefed"
    });
    reconcile_until(&env, "waiting on the permission", |env| {
        env.get("t1")["status"]["agentState"] == "attention"
    });
    let conversation = env.get("t1")["status"]["conversation"]
        .as_str()
        .expect("a conversation")
        .to_string();
    assert!(conversation.starts_with("ses_"), "{conversation}");
    allow_each_request_until(&env, &sockets, &work, "the turn ended", |env| {
        in_order(
            &reported(env, "t1"),
            &["idle", "prompt", "working", "blocked", "working", "idle"],
        ) && env.get("t1")["status"]["agentState"] == "idle"
    });

    // opencode ends: the revive resumes its session, which the next prompt
    // names again.
    end_agent(&sockets, &work);
    reconcile_until(&env, "resumed on its session", |env| {
        let states = reported(env, "t1");
        states
            .iter()
            .rposition(|(s, _)| s == "started")
            .is_some_and(|at| {
                states[at..]
                    .iter()
                    .any(|(s, c)| s == "prompt" && c.as_deref() == Some(&*conversation))
            })
    });
    end_agent(&sockets, &work);
    assert!(
        snapshot(&config, &[]) == before,
        "opencode's config directory {} changed",
        config.display()
    );
}

/// kilo, as Fleet starts it: its own embedded server, with Fleet's server
/// plugin named in `KILO_CONFIG_CONTENT` and its TUI plugin in the file
/// `KILO_TUI_CONFIG` names. The user's config picks a free model, which
/// needs no sign-in, and asks before each shell command: kilo merges Fleet's
/// plugins with it, so the agent waits on a person until each is answered.
/// Its session id is the conversation a revive resumes, and the user's
/// config directory is left byte for byte as it was.
///
/// The user is one this test makes, in a home of its own: kilo needs no
/// sign-in, and the operator's shell startup files, which may put another
/// `fleet` first on PATH, are not read.
#[test]
fn kilo_reports_its_state_through_the_plugins_fleet_names_at_launch() {
    let (Some(program), Some(_), Some(_)) = (runtime_mux(), tmux(), agent("kilo")) else {
        return;
    };
    let sockets = Sockets::new("kilo");
    let env = agent_env("runtime-mux-kilo", &program, &sockets);
    let config = env.root.join("user-config").join("kilo");
    fs::create_dir_all(&config).expect("user config");
    fs::write(
        config.join("kilo.json"),
        r#"{"model":"kilo/cohere/north-mini-code:free","permission":{"bash":"ask"}}"#,
    )
    .expect("user config");
    fs::write(config.join("tui.json"), r#"{"scroll_speed":3}"#).expect("user TUI config");
    // kilo's own first start rewrites the user's config (a `$schema`, a
    // migration marker, a `.gitignore`): it runs once without Fleet first.
    let settled = Command::new("kilo")
        .args(["debug", "config"])
        .current_dir(&env.root)
        .env("HOME", &env.root)
        .env("XDG_CONFIG_HOME", env.root.join("user-config"))
        .env("KILO_NO_DAEMON", "1")
        .output()
        .expect("kilo debug config");
    assert!(settled.status.success(), "{settled:?}");
    let before = snapshot(&config, &[]);
    let work = env.root.join("work").join("t1");
    fs::create_dir_all(&work).expect("workspace");
    let mut task = support::task("t1", true);
    task["spec"]["harness"] = json!("kilo");
    task["spec"]["env"] = json!({
        "XDG_CONFIG_HOME": env.root.join("user-config").to_string_lossy(),
    });
    task["spec"]["brief"] =
        json!("Run the shell command `echo fleet` yourself, with your shell tool, then stop.");
    task["spec"]["workspace"] = json!([format!("file://{}", work.display())]);
    env.apply(json!([task])).ok();

    reconcile_until(&env, "briefed on kilo's prompt hook", |env| {
        env.get("t1")["status"]["phase"] == "Briefed"
    });
    reconcile_until(&env, "waiting on the permission", |env| {
        env.get("t1")["status"]["agentState"] == "attention"
    });
    let conversation = env.get("t1")["status"]["conversation"]
        .as_str()
        .expect("a conversation")
        .to_string();
    assert!(conversation.starts_with("ses_"), "{conversation}");
    allow_each_request_until(&env, &sockets, &work, "the turn ended", |env| {
        in_order(
            &reported(env, "t1"),
            &["idle", "prompt", "working", "blocked", "working", "idle"],
        ) && env.get("t1")["status"]["agentState"] == "idle"
    });

    // kilo ends: the revive resumes its session, which the next prompt
    // names again.
    end_agent(&sockets, &work);
    reconcile_until(&env, "resumed on its session", |env| {
        let states = reported(env, "t1");
        states
            .iter()
            .rposition(|(s, _)| s == "started")
            .is_some_and(|at| {
                states[at..]
                    .iter()
                    .any(|(s, c)| s == "prompt" && c.as_deref() == Some(&*conversation))
            })
    });
    end_agent(&sockets, &work);
    assert!(
        snapshot(&config, &[]) == before,
        "kilo's config directory {} changed",
        config.display()
    );
}
