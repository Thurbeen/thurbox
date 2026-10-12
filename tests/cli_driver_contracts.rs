//! The contracts an external driver reads off `thurbox-cli`'s streams.
//!
//! Each test here asserts something that is only observable from *outside* the
//! process: what a child of `session exec` inherits, what a `$(…)` capture
//! actually receives, and which stream a failure lands on. Driven through the
//! real binary for that reason — the answers are a matter of the process's own
//! environment and of stdout not being a terminal, and nothing below `main` can
//! see either.

use std::path::PathBuf;
use std::process::{Command, Output};

use serde_json::Value;
use thurbox::session::SessionId;
use thurbox::sync::SharedSession;

/// The guard every tmux server in this file is reaped by — see its own doc.
#[path = "support/tmux_server.rs"]
mod tmux_server;

use tmux_server::TmuxServer;

/// A throwaway thurbox instance: its own config, data, home and multiplexer
/// socket, so no test here reads or writes the operator's.
struct Env {
    root: tempfile::TempDir,
    /// Named outright and reaped structurally: a relocated data dir derives a
    /// socket of its own, nothing here may reach the operator's server even by
    /// accident, and a server started here by mistake goes with this `Env`.
    server: TmuxServer,
}

impl Env {
    fn new() -> Self {
        let root = tempfile::TempDir::new().expect("tempdir");
        for sub in ["home", "config", "data", "work"] {
            std::fs::create_dir_all(root.path().join(sub)).expect("mkdir");
        }
        Self {
            root,
            server: TmuxServer::private("thurbox-driver-contract-test"),
        }
    }

    fn path(&self, sub: &str) -> PathBuf {
        self.root.path().join(sub)
    }

    fn db(&self) -> thurbox::storage::Database {
        thurbox::storage::Database::open(&self.path("data").join("thurbox.db"))
            .expect("open the instance database")
    }

    /// Run the CLI with stdout and stderr as pipes — which is what a driver
    /// capturing output gives it, and what makes the piped format apply.
    fn run(&self, args: &[&str]) -> Output {
        self.command(args).output().expect("run thurbox-cli")
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_thurbox-cli"));
        cmd.args(args);
        cmd.env("HOME", self.path("home"));
        cmd.env("USERPROFILE", self.path("home"));
        cmd.env("THURBOX_CONFIG_DIR", self.path("config"));
        cmd.env("THURBOX_DATA_DIR", self.path("data"));
        self.server.scope(&mut cmd);
        cmd.env_remove("THURBOX_SESSION");
        cmd.env_remove("THURBOX_SESSION_ID");
        cmd
    }

    /// Seed a session row directly, optionally with a working directory and an
    /// agent conversation id. Written through `storage::Database` because these
    /// tests need a *row*, never a pane: `session create` would spawn one.
    fn seed(&self, name: &str, cwd: Option<&std::path::Path>) -> SessionId {
        let row = SharedSession {
            id: SessionId::default(),
            name: name.into(),
            agent: "shell".into(),
            backend_id: String::new(),
            backend_type: "local-tmux".into(),
            agent_session_id: Some("conversation-1".into()),
            cwd: cwd.map(std::path::Path::to_path_buf),
            additional_dirs: Vec::new(),
            worktrees: Vec::new(),
            shell_backend_id: None,
            parent_session_id: None,
            display_order: None,
            tombstone: false,
            tombstone_at: None,
        };
        self.db().upsert_session(&row).expect("persist");
        row.id
    }
}

fn stdout_of(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).to_string()
}

fn stderr_of(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).to_string()
}

/// A multiplexer can acknowledge a kill without removing the window. Restart
/// must refuse before putting a second agent on the same conversation.
#[cfg(unix)]
#[test]
fn restart_refuses_a_false_successful_kill_and_explicit_retry_recovers() {
    use std::os::unix::fs::PermissionsExt;

    let Some(real_tmux) = thurbox::paths::resolve_on_path("tmux") else {
        eprintln!("skipping: tmux is not installed");
        return;
    };
    let env = Env::new();
    std::fs::write(
        env.path("config/agents.toml"),
        "default = \"shell\"\n[[agents]]\nname = \"shell\"\ncommand = \"sh\"\n\
         args = [\"-c\", \"while :; do sleep 1; done\"]\n",
    )
    .unwrap();
    let work = env.path("work");
    let history = work.join("history");
    std::fs::write(&history, "author work\n").unwrap();
    let id = env.seed("recovery", Some(&work));
    let id_text = id.to_string();
    let first = env.run(&["session", "restart", &id_text, "--json"]);
    assert!(first.status.success(), "{}", stdout_of(&first));
    let before = env.db().get_session_by_id(id).unwrap().unwrap();

    let bin = env.path("bin");
    std::fs::create_dir(&bin).unwrap();
    let wrapper = bin.join("tmux");
    std::fs::write(
        &wrapper,
        "#!/bin/sh\nfor arg in \"$@\"; do\n\
         if [ \"$arg\" = kill-window ]; then exit 0; fi\n\
         done\nexec \"$RECOVERY_REAL_TMUX\" \"$@\"\n",
    )
    .unwrap();
    std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o755)).unwrap();
    let path = std::env::join_paths(std::iter::once(bin).chain(std::env::split_paths(
        &std::env::var_os("PATH").unwrap_or_default(),
    )))
    .unwrap();
    for _ in 0..2 {
        let refused = env
            .command(&["session", "restart", &id_text, "--json"])
            .env("PATH", &path)
            .env("RECOVERY_REAL_TMUX", &real_tmux)
            .output()
            .unwrap();
        assert!(
            !refused.status.success(),
            "restart accepted a kill that left the old agent alive: {}",
            stdout_of(&refused)
        );
        assert!(
            stdout_of(&refused).contains("still present"),
            "{}",
            stdout_of(&refused)
        );
        let after = env.db().get_session_by_id(id).unwrap().unwrap();
        assert_eq!(after.backend_id, before.backend_id);
        assert_eq!(after.agent_session_id, before.agent_session_id);
        let panes = env
            .server
            .tmux(&["list-panes", "-a", "-F", "#{pane_id}|#{window_name}"]);
        let panes = stdout_of(&panes);
        assert_eq!(
            panes
                .lines()
                .filter(|p| p.ends_with("|tb-recovery"))
                .count(),
            1
        );
        assert!(panes
            .lines()
            .any(|p| p == format!("{}|tb-recovery", before.backend_id)));
    }

    let retry = env.run(&["session", "restart", &id_text, "--json"]);
    assert!(retry.status.success(), "{}", stdout_of(&retry));
    let after = env.db().get_session_by_id(id).unwrap().unwrap();
    assert_ne!(after.backend_id, before.backend_id);
    assert_eq!(after.agent_session_id, before.agent_session_id);
    assert_eq!(std::fs::read_to_string(history).unwrap(), "author work\n");
}

/// `session exec` runs "in the session's context", and the environment is part
/// of that context.
///
/// The failure this pins is not that the session's own `--env` was missing —
/// it is that the **caller's** `THURBOX_SESSION` was inherited by the child. A
/// driver reaching into a session from inside another one would then have
/// `thurbox-cli session signal` record state for the *calling* session,
/// silently and with exit 0, which is the one outcome that cannot be correct.
#[cfg(unix)]
#[test]
fn exec_carries_the_targets_identity_and_environment_not_the_callers() {
    let env = Env::new();
    let work = env.path("work");
    let target = env.seed("target", Some(&work));
    env.db()
        .set_launch_env(
            target,
            &[("FM_PROBE".to_string(), "1".to_string())]
                .into_iter()
                .collect(),
        )
        .expect("record the session's --env");

    let caller = SessionId::default();
    let mut cmd = env.command(&[
        "session",
        "exec",
        &target.to_string(),
        "--json",
        "--",
        "sh",
        "-c",
        "printf '%s|%s' \"$THURBOX_SESSION\" \"$FM_PROBE\"",
    ]);
    // The calling driver is itself inside a session, which is the ordinary
    // case and the one that used to leak.
    cmd.env("THURBOX_SESSION", caller.to_string());
    let out = cmd.output().expect("run thurbox-cli");

    let doc: Value = serde_json::from_slice(&out.stdout)
        .unwrap_or_else(|e| panic!("stdout is JSON ({e}): {}", stdout_of(&out)));
    let printed = doc["stdout"].as_str().expect("the child's stdout");
    let (session, probe) = printed.split_once('|').expect("both values");

    assert_eq!(
        session,
        target.to_string(),
        "the child must carry the target session's identity, not the caller's \
         ({caller}): {printed}"
    );
    assert_eq!(
        probe, "1",
        "the session's own --env must be there: {printed}"
    );
}

/// `session meta get`'s answer is one value, and being captured into a shell
/// variable is exactly what makes stdout not a terminal.
///
/// The piped default is TOON because the reader of a pipe is usually an agent
/// reading a record — but this getter's reader is `v=$(…)`, so the format meant
/// for a pipe replaced the value with the record in precisely the case the
/// command exists for.
#[test]
fn meta_get_answers_with_the_bare_value_when_piped() {
    let env = Env::new();
    let id = env.seed("probe", None).to_string();
    env.run(&["session", "meta", "set", &id, "fm.state", "plain-value"]);

    let out = env.run(&["session", "meta", "get", &id, "fm.state"]);
    assert_eq!(
        stdout_of(&out).trim_end_matches('\n'),
        "plain-value",
        "the value and nothing else: {}",
        stdout_of(&out)
    );

    // An unset key is nothing at all, matching the help — not the string
    // `value: null`, which no caller can tell from a value.
    let unset = env.run(&["session", "meta", "get", &id, "fm.never-set"]);
    assert!(
        stdout_of(&unset).trim().is_empty(),
        "an unset key produces nothing: {:?}",
        stdout_of(&unset)
    );

    // …and the record is still one flag away, which is where a caller that
    // needs to tell a null value from an unset key goes.
    let json = env.run(&["session", "meta", "get", &id, "fm.state", "--json"]);
    let doc: Value = serde_json::from_slice(&json.stdout).expect("a record");
    assert_eq!(doc["key"], Value::String("fm.state".into()));
    assert_eq!(doc["value"], Value::String("plain-value".into()));
}

/// A failing `session send` keeps its output on one stream.
///
/// `send` used to write tmux's own `can't find window: tb-<name>` to stderr in
/// addition to the structured error on stdout, while `capture` in the same
/// situation did not — one error contract, two behaviours. An agent reads one
/// stream (AXI principle 6), so the multiplexer's sentence belongs inside the
/// document, not beside it.
#[test]
fn send_to_a_session_with_no_pane_keeps_its_error_on_stdout() {
    let env = Env::new();
    let id = env.seed("gone", None).to_string();
    env.run(&["session", "stop", &id]);

    for verb in [
        vec!["session", "send", &id, "hello", "--json"],
        vec!["session", "capture", &id, "--json"],
    ] {
        let out = env.run(&verb);
        assert_eq!(out.status.code(), Some(1), "{verb:?} ran and failed");
        assert!(
            stderr_of(&out).trim().is_empty(),
            "{verb:?} put something on stderr: {:?}",
            stderr_of(&out)
        );
        let doc: Value = serde_json::from_slice(&out.stdout)
            .unwrap_or_else(|e| panic!("{verb:?} stdout is JSON ({e}): {}", stdout_of(&out)));
        assert!(
            doc["error"].as_str().is_some_and(|e| e.contains("stopped")),
            "{verb:?} says what is actually wrong: {doc}"
        );
    }
}

/// The same rule where the window is merely gone rather than parked.
///
/// This is the path that actually reached the multiplexer: the one-shot helpers
/// ran `tmux` with `status()`, so the child inherited this process's stderr and
/// wrote its own diagnosis straight onto it. Captured, the same sentence is
/// part of the one document instead of a second stream beside it.
#[test]
fn a_pane_verb_on_a_missing_window_says_so_on_stdout_alone() {
    let env = Env::new();
    // Not stopped: nothing knows the window is absent until the multiplexer is
    // asked, which is exactly the case that leaked.
    let id = env.seed("vanished", None).to_string();

    for verb in [
        vec!["session", "send", &id, "hello", "--json"],
        vec!["session", "key", &id, "enter", "--json"],
    ] {
        let out = env.run(&verb);
        assert_eq!(out.status.code(), Some(1), "{verb:?} ran and failed");
        assert!(
            stderr_of(&out).trim().is_empty(),
            "{verb:?} put the multiplexer's own message on stderr: {:?}",
            stderr_of(&out)
        );
        let doc: Value = serde_json::from_slice(&out.stdout)
            .unwrap_or_else(|e| panic!("{verb:?} stdout is JSON ({e}): {}", stdout_of(&out)));
        assert!(
            !doc["error"].as_str().unwrap_or_default().is_empty(),
            "{verb:?} still says what went wrong: {doc}"
        );
    }
}

/// A driver that launches its own agent can obtain the hook wiring.
///
/// Status hooks are installed by appending to an agent's `args`, so they only
/// reach the process when thurbox builds the command line. Without a verb that
/// reports those args, a driver launching the agent itself got no hooks — and
/// so an empty `state` and a `watch` stream that never mentioned the session.
#[test]
fn agent_launch_args_reports_what_to_run() {
    let env = Env::new();
    std::fs::write(
        env.path("config").join("agents.toml"),
        "config_version = 1\ndefault = \"driver\"\n\n\
         [[agents]]\nname = \"driver\"\ncommand = \"driver-cli\"\n\
         args = [\"--settings\", \"/opt/hooks/driver.json\"]\n",
    )
    .expect("write agents.toml");
    let id = env.seed("wired", None).to_string();

    let out = env.run(&["agent", "launch-args", "driver", "--session", &id, "--json"]);
    assert_eq!(out.status.code(), Some(0), "{}", stdout_of(&out));
    let doc: Value = serde_json::from_slice(&out.stdout).expect("a record");

    assert_eq!(doc["command"], Value::String("driver-cli".into()), "{doc}");
    assert_eq!(
        doc["args"],
        serde_json::json!(["--settings", "/opt/hooks/driver.json"]),
        "the hook wiring is the answer: {doc}"
    );
    // And the identity the agent's own `session signal` will report under.
    assert_eq!(doc["env"]["THURBOX_SESSION"], Value::String(id), "{doc}");
}

/// A psmux agent window closes with its agent, so the window reports the exit
/// itself on the way out: `LASTEXITCODE` piped into `session exited`. What a
/// driver tailing `watch` then sees is the status, not a window that vanished.
#[test]
fn an_exited_agent_leaves_its_status_in_the_event_log() {
    use std::io::Write;

    let env = Env::new();
    let target = env.seed("target", None);
    let head = env.db().latest_session_event_seq().expect("head");

    let mut cmd = env.command(&["session", "exited", "--json"]);
    cmd.env("THURBOX_SESSION", target.to_string());
    cmd.stdin(std::process::Stdio::piped());
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());
    let mut child = cmd.spawn().expect("run thurbox-cli");
    // As PowerShell pipes it: a 0xC0000005 crash, with its line ending.
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(b"-1073741819\r\n")
        .expect("write the status");
    let out = child.wait_with_output().expect("wait");
    assert!(out.status.success(), "{}", stderr_of(&out));

    let events = env
        .db()
        .session_events_since(head, Some(target), 10)
        .expect("events");
    assert_eq!(events.len(), 1, "{events:?}");
    assert_eq!(events[0].reason, "exited");
    assert_eq!(events[0].exit_code, Some(-1073741819));
}

/// Outside a session there is nothing to record against, and nothing to fail:
/// the call is the last thing a closing window runs.
#[test]
fn an_exit_outside_a_session_records_nothing() {
    let env = Env::new();
    let out = env.run(&["session", "exited", "--json"]);
    assert!(out.status.success(), "{}", stderr_of(&out));
    let doc: Value = serde_json::from_slice(&out.stdout).expect("JSON");
    assert_eq!(doc["recorded"], Value::Bool(false));
}
