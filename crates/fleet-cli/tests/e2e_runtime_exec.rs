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
        .stdout_has("ok   a protocol mismatch answers Unknown")
        .stdout_has("ok   an error answers Unknown, never None")
        .stdout_has("ok   a stop of an absent process succeeds");
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
    task["spec"]["workspace"] = json!({"dir": env.root.to_string_lossy()});
    env.apply(json!([task])).ok();
    env.reconcile();
    let status = env.get("t1")["status"].clone();
    assert!(
        status.to_string().contains("tmux"),
        "the status names the missing multiplexer: {status:#}"
    );
    assert!(status["session"].is_null(), "{status:#}");
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
    task["spec"]["workspace"] = json!({"dir": work.to_string_lossy()});
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
    task["spec"]["workspace"] = json!({"dir": work.to_string_lossy()});
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
