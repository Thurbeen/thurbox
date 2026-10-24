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
            key: None,
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

#[cfg(unix)]
#[test]
fn a_delegated_crash_loop_hold_reaches_the_calling_cli() {
    use std::os::unix::fs::PermissionsExt;

    let env = Env::new();
    let bin = env.path("bin");
    std::fs::create_dir(&bin).unwrap();
    std::fs::write(
        env.path("config/settings.toml"),
        "[features]\nautomations = false\nversion_check = false\nauto_update = false\n",
    )
    .unwrap();
    let version = env.run(&["version", "--json"]);
    assert!(version.status.success());
    std::fs::write(env.path("data/host-version"), version.stdout).unwrap();
    std::fs::write(
        env.path("config/hosts.toml"),
        "[[hosts]]\nname = 'probe'\ndestination = 'probe'\nshare_sessions = true\n",
    )
    .unwrap();
    std::fs::write(
        env.path("config/hooks.toml"),
        r#"[[hooks]]
event = 'session.post_restart'
command = 'touch "$THURBOX_DATA_DIR/post-hook"'
"#,
    )
    .unwrap();
    let ssh = bin.join("ssh");
    std::fs::write(
        &ssh,
        r#"#!/bin/sh
case "$*" in
    *'version --json'*)
        printf '%s\n' '@cli /probe/thurbox-cli' '@status 0'
        cat "$THURBOX_DATA_DIR/host-version"
        ;;
    *session*restart*) printf '%s\n' '{"restarted":false,"held_back":true,"hook_failures":[]}' ;;
    *session*list*) printf '%s\n' '[]' ;;
esac
"#,
    )
    .unwrap();
    std::fs::set_permissions(&ssh, std::fs::Permissions::from_mode(0o755)).unwrap();
    let id = env.seed("delegated-probe", None);
    let db = env.db();
    let mut session = db.get_session_by_id(id).unwrap().unwrap();
    session.backend_type = "ssh:probe".into();
    db.upsert_session(&session).unwrap();
    let mut cmd = env.command(&[
        "session",
        "restart",
        &id.to_string(),
        "--if-missing",
        "--json",
    ]);
    cmd.env(
        "PATH",
        format!("{}:{}", bin.display(), std::env::var("PATH").unwrap()),
    );
    let out = cmd.output().unwrap();
    assert!(out.status.success(), "{}", stderr_of(&out));
    let report: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(report["held_back"], true, "{report}");
    assert_eq!(report["restarted"], false, "{report}");
    assert!(!env.path("data/post-hook").exists());
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

/// `session exec` on a host carries the session's environment without putting a
/// single value on a command line.
///
/// That environment holds secrets: the session's own `--env`, and the
/// `[telemetry] headers` token a host with an `otel_endpoint` is handed. A
/// value in ssh's arguments is readable by every user on both machines through
/// `ps`, so the values travel on ssh's stdin, and only the fixed words of the
/// launcher are left in its argv. The stand-in `ssh` records that argv and then
/// runs the command here, stdin included, so the child proves the values still
/// arrived.
#[cfg(unix)]
#[test]
fn exec_on_a_host_keeps_every_env_value_off_the_command_line() {
    use std::os::unix::fs::PermissionsExt;

    let env = Env::new();
    let bin = env.path("bin");
    std::fs::create_dir(&bin).unwrap();
    std::fs::write(
        env.path("config/settings.toml"),
        "[features]\nautomations = false\nversion_check = false\nauto_update = false\n\
         [telemetry]\nenabled = true\nheaders = { x-token = \"header-secret\" }\n",
    )
    .unwrap();
    std::fs::write(
        env.path("config/hosts.toml"),
        "[[hosts]]\nname = 'probe'\ndestination = 'probe'\nshare_sessions = false\n\
         otel_endpoint = 'http://collector.probe:4318'\n",
    )
    .unwrap();
    let argv_log = env.path("ssh-argv");
    let ssh = bin.join("ssh");
    std::fs::write(
        &ssh,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\n\
             while [ \"$#\" -gt 0 ]; do\n\
             \x20 case \"$1\" in\n\
             \x20   -o) shift; shift ;;\n\
             \x20   -*) shift ;;\n\
             \x20   *) break ;;\n\
             \x20 esac\n\
             done\n\
             shift\n\
             eval \"exec $*\"\n",
            argv_log.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&ssh, std::fs::Permissions::from_mode(0o755)).unwrap();

    let work = env.path("work");
    let id = env.seed("remote", Some(&work));
    let db = env.db();
    let mut session = db.get_session_by_id(id).unwrap().unwrap();
    session.backend_type = "ssh:probe".into();
    db.upsert_session(&session).unwrap();
    db.set_launch_env(
        id,
        &[("FM_SECRET".to_string(), "env-secret".to_string())]
            .into_iter()
            .collect(),
    )
    .unwrap();

    let mut cmd = env.command(&[
        "session",
        "exec",
        &id.to_string(),
        "--json",
        "--",
        "sh",
        "-c",
        "printf '%s|%s' \"$OTEL_EXPORTER_OTLP_HEADERS\" \"$FM_SECRET\"",
    ]);
    cmd.env(
        "PATH",
        format!("{}:{}", bin.display(), std::env::var("PATH").unwrap()),
    );
    let out = cmd.output().expect("run thurbox-cli");
    let doc: Value = serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
        panic!(
            "stdout is JSON ({e}): {}{}",
            stdout_of(&out),
            stderr_of(&out)
        )
    });
    assert_eq!(
        doc["stdout"], "x-token=header-secret|env-secret",
        "the child must still receive both values: {doc}"
    );
    let argv = std::fs::read_to_string(&argv_log).expect("ssh was run");
    for secret in ["header-secret", "env-secret"] {
        assert!(
            !argv.contains(secret),
            "{secret} reached ssh's command line, readable in ps: {argv}"
        );
    }
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

#[test]
fn missing_session_errors_carry_a_kind_for_remote_callers() {
    let env = Env::new();
    let out = env.run(&[
        "session",
        "reap",
        "00000000-0000-0000-0000-000000000001",
        "--json",
    ]);
    assert_eq!(out.status.code(), Some(1));
    let document: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(document["error_kind"], "not_found", "{document}");
}

/// A refusal mentioning an unrelated missing file must never authorize the
/// fallback which kills the remote session's windows.
#[cfg(unix)]
fn remote_reap_reply(document: &str, code: i32) -> (Output, bool) {
    use std::os::unix::fs::PermissionsExt;
    let env = Env::new();
    let bin = env.path("bin");
    std::fs::create_dir(&bin).unwrap();
    let version = env.run(&["version", "--json"]);
    assert!(version.status.success());
    std::fs::write(env.path("data/host-version"), version.stdout).unwrap();
    std::fs::write(
        env.path("config/hosts.toml"),
        "[[hosts]]\nname = 'probe'\ndestination = 'probe'\nshare_sessions = true\n",
    )
    .unwrap();
    let ssh = bin.join("ssh");
    std::fs::write(
        &ssh,
        r#"#!/bin/sh
case "$*" in
    *'version --json'*)
        printf '%s\n' '@cli /probe/thurbox-cli' '@status 0'
        cat "$THURBOX_DATA_DIR/host-version" ;;
    *session*reap*)
        cat "$THURBOX_DATA_DIR/peer-error"
        exit "$(cat "$THURBOX_DATA_DIR/peer-code")" ;;
    *list-windows*|*kill-window*)
        touch "$THURBOX_DATA_DIR/fallback-attempted"
        printf '%s\n' "can't find session: thurbox" >&2
        exit 1 ;;
    *)
        printf '%s\n' "can't find session: thurbox" >&2
        exit 1 ;;
esac
"#,
    )
    .unwrap();
    std::fs::set_permissions(&ssh, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::write(env.path("data/peer-error"), document).unwrap();
    std::fs::write(env.path("data/peer-code"), code.to_string()).unwrap();
    let id = env.seed("refused-reap", None);
    let db = env.db();
    let mut row = db.get_session_by_id(id).unwrap().unwrap();
    row.backend_type = "ssh:probe".into();
    db.upsert_session(&row).unwrap();
    db.soft_delete_session(id).unwrap();
    db.set_host_lifecycle_generation(id, 7).unwrap();
    let mut command = env.command(&["session", "reap", &id.to_string(), "--json"]);
    command.env(
        "PATH",
        format!("{}:{}", bin.display(), std::env::var("PATH").unwrap()),
    );
    let out = command.output().unwrap();
    (out, env.path("data/fallback-attempted").exists())
}

#[cfg(unix)]
#[test]
fn remote_reap_refusal_is_not_mistaken_for_an_absent_session() {
    let (out, fallback) = remote_reap_reply(
        r#"{"error":"policy file not found; teardown refused","error_kind":"refused"}"#,
        1,
    );
    assert!(
        !fallback,
        "a refusal must not trigger backend teardown: {}",
        stdout_of(&out)
    );
    assert_eq!(out.status.code(), Some(1), "{}", stdout_of(&out));
    let answer: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(answer["error_kind"], "refused");
}

#[cfg(unix)]
#[test]
fn delegated_errors_preserve_kinds_codes_and_transport_reach() {
    for (kind, code) in [
        ("usage", 2),
        ("ambiguous", 3),
        ("timeout", 4),
        ("conflict", 5),
        ("gone", 6),
        ("unreachable", 1),
        ("undetermined", 1),
    ] {
        let document =
            serde_json::json!({"error":"same wording for every failure", "error_kind":kind});
        let (out, fallback) = remote_reap_reply(&document.to_string(), code);
        assert!(!fallback, "{kind} must not authorize teardown");
        assert_eq!(out.status.code(), Some(code), "{kind}: {}", stdout_of(&out));
        let answer: Value = serde_json::from_slice(&out.stdout).unwrap();
        assert_eq!(answer["error_kind"], kind);
    }
    for document in [
        r#"{"error":"Session not found"}"#,
        r#"{"error":"Session not found", "error_kind":"future_kind"}"#,
    ] {
        let (out, fallback) = remote_reap_reply(document, 1);
        assert!(!fallback, "an opaque peer error is not absence");
        let answer: Value = serde_json::from_slice(&out.stdout).unwrap();
        assert_eq!(answer["error_kind"], "other");
    }
    let (out, fallback) =
        remote_reap_reply(r#"{"error":"missing", "error_kind":"not_found"}"#, 255);
    assert!(
        !fallback,
        "SSH's failure status takes precedence over stdout"
    );
    let answer: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(answer["error_kind"], "unreached");
}

/// A delegated `send --wait-turn` that times out on the host has still
/// delivered the text. The host's whole answer — `sent` included — reaches the
/// caller with the host's exit code, so a driver does not read exit 4 as "send
/// it again" and type the prompt twice.
#[cfg(unix)]
#[test]
fn a_delegated_wait_turn_timeout_keeps_the_hosts_send_result() {
    use std::os::unix::fs::PermissionsExt;
    let env = Env::new();
    let bin = env.path("bin");
    std::fs::create_dir(&bin).unwrap();
    let version = env.run(&["version", "--json"]);
    assert!(version.status.success());
    std::fs::write(env.path("data/host-version"), version.stdout).unwrap();
    std::fs::write(
        env.path("config/hosts.toml"),
        "[[hosts]]\nname = 'probe'\ndestination = 'probe'\nshare_sessions = true\n",
    )
    .unwrap();
    let ssh = bin.join("ssh");
    std::fs::write(
        &ssh,
        r#"#!/bin/sh
case "$*" in
    *'version --json'*)
        printf '%s\n' '@cli /probe/thurbox-cli' '@status 0'
        cat "$THURBOX_DATA_DIR/host-version" ;;
    *session*send*)
        printf '%s\n' '{"sent":true,"submitted":true,"turn":null,"error":"wait timed out","error_kind":"timeout"}'
        exit 4 ;;
    *)
        exit 1 ;;
esac
"#,
    )
    .unwrap();
    std::fs::set_permissions(&ssh, std::fs::Permissions::from_mode(0o755)).unwrap();
    let id = env.seed("remote-worker", None);
    let db = env.db();
    let mut row = db.get_session_by_id(id).unwrap().unwrap();
    row.backend_type = "ssh:probe".into();
    db.upsert_session(&row).unwrap();
    let mut command = env.command(&[
        "session",
        "send",
        &id.to_string(),
        "hello",
        "--wait-turn",
        "--timeout",
        "1",
        "--json",
    ]);
    command.env(
        "PATH",
        format!("{}:{}", bin.display(), std::env::var("PATH").unwrap()),
    );
    let out = command.output().unwrap();
    assert_eq!(out.status.code(), Some(4), "{}", stdout_of(&out));
    let answer: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(answer["sent"], true, "{answer}");
    assert_eq!(answer["error_kind"], "timeout", "{answer}");
    assert!(answer["turn"].is_null(), "{answer}");
}

/// A waiting send makes the same local checks a plain send makes before it
/// asks the host: a row parked here is refused here, with the local remedy,
/// and the host is never asked to type into it.
#[cfg(unix)]
#[test]
fn a_waiting_send_to_a_parked_remote_row_is_refused_locally() {
    use std::os::unix::fs::PermissionsExt;
    let env = Env::new();
    let bin = env.path("bin");
    std::fs::create_dir(&bin).unwrap();
    let version = env.run(&["version", "--json"]);
    assert!(version.status.success());
    std::fs::write(env.path("data/host-version"), version.stdout).unwrap();
    std::fs::write(
        env.path("config/hosts.toml"),
        "[[hosts]]\nname = 'probe'\ndestination = 'probe'\nshare_sessions = true\n",
    )
    .unwrap();
    let ssh = bin.join("ssh");
    std::fs::write(
        &ssh,
        r#"#!/bin/sh
case "$*" in
    *'version --json'*)
        printf '%s\n' '@cli /probe/thurbox-cli' '@status 0'
        cat "$THURBOX_DATA_DIR/host-version" ;;
    *session*send*)
        touch "$THURBOX_DATA_DIR/host-asked-to-send"
        printf '%s\n' '{"sent":true,"submitted":true}' ;;
    *)
        exit 1 ;;
esac
"#,
    )
    .unwrap();
    std::fs::set_permissions(&ssh, std::fs::Permissions::from_mode(0o755)).unwrap();
    let id = env.seed("parked-remote", None);
    let db = env.db();
    let mut row = db.get_session_by_id(id).unwrap().unwrap();
    row.backend_type = "ssh:probe".into();
    db.upsert_session(&row).unwrap();
    db.set_session_stopped(id, true).unwrap();
    for flag in ["--strict", "--wait-turn"] {
        let mut command = env.command(&["session", "send", &id.to_string(), "hi", flag, "--json"]);
        command.env(
            "PATH",
            format!("{}:{}", bin.display(), std::env::var("PATH").unwrap()),
        );
        let out = command.output().unwrap();
        assert!(!out.status.success(), "{flag}: {}", stdout_of(&out));
        assert!(
            stdout_of(&out).contains("session start"),
            "{flag}: {}",
            stdout_of(&out)
        );
        assert!(
            !env.path("data/host-asked-to-send").exists(),
            "{flag}: the host was asked to type into a parked row"
        );
    }
}

#[cfg(unix)]
#[test]
fn a_typed_missing_host_row_keeps_fenced_cleanup_from_falling_back() {
    let (out, fallback) = remote_reap_reply(r#"{"error":"absent", "error_kind":"not_found"}"#, 1);
    assert!(!out.status.success(), "{}", stdout_of(&out));
    assert!(
        !fallback,
        "a typed absence cannot bypass a lifecycle precondition"
    );
    let answer: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(answer["error_kind"], "not_found");
}

/// A host on an older build sends no `error_kind`. Its own absence sentence is
/// still recognized as absence, but a fenced reap never falls back to a kill.
#[cfg(unix)]
#[test]
fn a_legacy_absence_reply_cannot_bypass_fenced_cleanup() {
    let (out, fallback) = remote_reap_reply(
        r#"{"error":"Deleted session not found: 00000000-0000-0000-0000-000000000001"}"#,
        1,
    );
    assert!(!out.status.success(), "{}", stdout_of(&out));
    assert!(
        !fallback,
        "an absence answer cannot bypass the lifecycle precondition"
    );
    let answer: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(answer["error_kind"], "not_found");
}

/// The host may hold the row live (restored there) while this machine still
/// holds it deleted. That is not absence, so it must not let the caller kill
/// the restored agent's windows.
#[test]
fn reaping_a_live_session_is_a_conflict_not_an_absence() {
    let env = Env::new();
    let id = env.seed("restored-elsewhere", None);
    let out = env.run(&["session", "reap", &id.to_string(), "--json"]);
    assert_eq!(out.status.code(), Some(5), "{}", stdout_of(&out));
    let answer: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(answer["error_kind"], "conflict");
}

#[test]
fn a_taken_session_key_uses_the_shared_conflict_exit_code() {
    let env = Env::new();
    let id = env.seed("existing", None);
    let db = env.db();
    let mut row = db.get_session_by_id(id).unwrap().unwrap();
    row.key = Some("claimed-key".into());
    db.upsert_session(&row).unwrap();
    let out = env.run(&[
        "session",
        "create",
        "--name",
        "another",
        "--repo-path",
        env.path("work").to_str().unwrap(),
        "--agent",
        "shell",
        "--key",
        "claimed-key",
        "--on-existing",
        "fail",
        "--json",
    ]);
    assert_eq!(out.status.code(), Some(5), "{}", stdout_of(&out));
    let answer: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(answer["error_kind"], "conflict");
}

/// An id a deleted session still holds is not free: reusing it would collide
/// with that row's restore. Refused before anything is spawned, with the
/// shared conflict code.
#[test]
fn a_create_under_a_deleted_sessions_id_is_a_conflict() {
    let env = Env::new();
    let id = env.seed("gone", None);
    env.db().soft_delete_session(id).unwrap();
    let out = env.run(&[
        "session",
        "create",
        "--name",
        "again",
        "--repo-path",
        env.path("work").to_str().unwrap(),
        "--agent",
        "shell",
        "--id",
        &id.to_string(),
        "--json",
    ]);
    assert_eq!(out.status.code(), Some(5), "{}", stdout_of(&out));
    let answer: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(answer["error_kind"], "conflict");
}

/// `session create --env-stdin` with `stdin` written to it.
fn create_with_env_stdin(env: &Env, stdin: &str, extra: &[&str]) -> Output {
    use std::io::Write;
    let work = env.path("work");
    let mut args = vec![
        "session",
        "create",
        "--name",
        "piped",
        "--repo-path",
        work.to_str().unwrap(),
        "--env-stdin",
        "--json",
    ];
    args.extend_from_slice(extra);
    let mut child = env
        .command(&args)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("run thurbox-cli");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(stdin.as_bytes())
        .unwrap();
    child.wait_with_output().unwrap()
}

/// What `--env-stdin` cannot read is a usage error, as a malformed `--env`
/// is: a key that is no variable name, or anything but one object of strings.
#[test]
fn env_stdin_refuses_what_is_not_an_environment() {
    let env = Env::new();
    for stdin in [r#"{"A=B":"x"}"#, r#"{"":"x"}"#, r#"{"A":1}"#, "not json"] {
        let out = create_with_env_stdin(&env, stdin, &["--agent", "shell"]);
        assert_eq!(out.status.code(), Some(2), "{stdin}: {}", stdout_of(&out));
        let answer: Value = serde_json::from_slice(&out.stdout).unwrap();
        assert_eq!(answer["error_kind"], "usage", "{stdin}");
    }
}

/// `--env` wins a clash with `--env-stdin`, and what only stdin names still
/// arrives.
#[cfg(unix)]
#[test]
fn env_on_the_command_line_wins_over_env_stdin() {
    if thurbox::paths::resolve_on_path("tmux").is_none() {
        eprintln!("skipping: tmux is not installed");
        return;
    }
    let env = Env::new();
    let seen = env.path("work").join("seen");
    let script = format!(
        "printf '%s|%s' \"$K\" \"$J\" > '{}'; exec sleep 600",
        seen.display()
    );
    let out = create_with_env_stdin(
        &env,
        r#"{"K":"from-stdin","J":"only-stdin"}"#,
        &[
            "--command",
            "sh",
            "--arg",
            "-c",
            "--arg",
            &script,
            "--env",
            "K=from-flag",
        ],
    );
    assert!(out.status.success(), "{}", stdout_of(&out));
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    let written = loop {
        if let Ok(text) = std::fs::read_to_string(&seen) {
            if !text.is_empty() {
                break text;
            }
        }
        assert!(std::time::Instant::now() < deadline, "the pane never wrote");
        std::thread::sleep(std::time::Duration::from_millis(100));
    };
    assert_eq!(written, "from-flag|only-stdin");
}

/// A deleted id makes the creation impossible, so it is refused before
/// `--on-existing replace` tears anything down: tearing down a live namesake
/// for a creation that then fails costs that session its uncommitted work.
#[test]
fn replace_with_a_deleted_id_tears_nothing_down() {
    let env = Env::new();
    let victim = env.seed("victim", None);
    let gone = env.seed("gone", None);
    env.db().soft_delete_session(gone).unwrap();
    let out = env.run(&[
        "session",
        "create",
        "--name",
        "victim",
        "--repo-path",
        env.path("work").to_str().unwrap(),
        "--agent",
        "shell",
        "--id",
        &gone.to_string(),
        "--on-existing",
        "replace",
        "--json",
    ]);
    assert_eq!(out.status.code(), Some(5), "{}", stdout_of(&out));
    let answer: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(answer["error_kind"], "conflict");
    let error = answer["error"].as_str().unwrap_or_default();
    assert!(
        !error.contains("could not be spawned"),
        "the namesake was replaced before the id was refused: {error}"
    );
    assert!(env.db().get_session_by_id(victim).unwrap().is_some());
}

/// What each verb does with a session on a host `hosts.toml` no longer names
/// (`session_ops::Executor`). Those that drive the session's own machine —
/// rename, set-key, restart — refuse: doing it here would act on the wrong
/// one. A parent link and a delete are this side's own record, so they are
/// taken here.
#[test]
fn each_verb_keeps_its_answer_for_a_host_hosts_toml_no_longer_names() {
    let env = Env::new();
    let seed = |name: &str| {
        let id = env.seed(name, None);
        let db = env.db();
        let mut row = db.get_session_by_id(id).unwrap().unwrap();
        row.backend_type = "ssh:forgotten".into();
        db.upsert_session(&row).unwrap();
        id.to_string()
    };

    let id = seed("orphan");
    for args in [
        vec!["session", "rename", id.as_str(), "renamed"],
        vec!["session", "set-key", id.as_str(), "k9"],
        vec!["session", "restart", id.as_str()],
    ] {
        let mut args = args.clone();
        args.push("--json");
        let out = env.run(&args);
        assert!(!out.status.success(), "{args:?} acted: {}", stdout_of(&out));
        let answer: Value = serde_json::from_slice(&out.stdout).unwrap();
        assert!(
            answer["error"]
                .as_str()
                .unwrap_or_default()
                .contains("not in hosts.toml"),
            "{args:?}: {answer}"
        );
    }

    let row = env
        .db()
        .get_session_by_id(id.parse().unwrap())
        .unwrap()
        .unwrap();
    assert_eq!(row.name, "orphan", "refused, so nothing was written here");
    assert_eq!(row.key, None);

    let reparented = env.run(&["session", "reparent", &id, "--clear", "--json"]);
    assert!(reparented.status.success(), "{}", stdout_of(&reparented));

    let deleted = env.run(&["session", "delete", &id, "--force", "--json"]);
    assert!(deleted.status.success(), "{}", stdout_of(&deleted));
    let id: SessionId = id.parse().unwrap();
    assert!(
        env.db().get_session_by_id(id).unwrap().is_none(),
        "deleted here"
    );
}

#[test]
fn builtin_named_variants_keep_their_templates_telemetry_optout() {
    let env = Env::new();
    std::fs::write(
        env.path("config/settings.toml"),
        "[telemetry]\nenabled=true\n",
    )
    .unwrap();
    for name in ["claude", "codex"] {
        std::fs::write(
            env.path("config/agents.toml"),
            format!(
                "[[agents]]\nname='template'\ncommand='{name}'\ntelemetry_env={{}}\ntelemetry_args=[]\n\
                 [[agents]]\nname='{name}'\nextends='template'\n"
            ),
        )
        .unwrap();
        let out = env.run(&["agent", "launch-args", name, "--json"]);
        assert!(out.status.success(), "{}", stdout_of(&out));
        let answer: Value = serde_json::from_slice(&out.stdout).unwrap();
        assert_eq!(answer["args"], serde_json::json!([]), "{answer}");
        assert!(
            answer["env"].get("CLAUDE_CODE_ENABLE_TELEMETRY").is_none(),
            "{answer}"
        );
    }
}

#[test]
fn agent_variants_resolve_through_the_real_cli_and_validate_bad_chains() {
    let env = Env::new();
    let file = env.path("config/agents.toml");
    std::fs::write(
        &file,
        r#"
default = "variant"
[[agents]]
name = "variant"
extends = "base"
append_args = ["--model", "example"]
[[agents]]
name = "base"
command = "sh"
args = ["--base"]
resume_args = ["--resume", "{id}"]
hook_schema = "claude"
"#,
    )
    .unwrap();
    let out = env.run(&["agent", "launch-args", "variant", "--json"]);
    assert!(
        out.status.success(),
        "{} {}",
        stdout_of(&out),
        stderr_of(&out)
    );
    let answer: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(
        answer["args"],
        serde_json::json!(["--base", "--model", "example"])
    );
    let valid = env.run(&["config", "validate", "--json"]);
    assert!(valid.status.success(), "{}", stdout_of(&valid));
    for invalid in [
        "[[agents]]\nname='a'\nextends='missing'\n",
        "[[agents]]\nname='a'\nextends='b'\n[[agents]]\nname='b'\nextends='a'\n",
    ] {
        std::fs::write(&file, invalid).unwrap();
        let out = env.run(&["config", "validate", "--json"]);
        assert!(!out.status.success(), "{}", stdout_of(&out));
        assert!(stdout_of(&out).contains("extends"), "{}", stdout_of(&out));
    }
    for field in [
        "default = 1",
        "config_version = -1",
        "config_version = 'bad'",
    ] {
        std::fs::write(
            &file,
            format!("{field}\n[[agents]]\nname='ok'\ncommand='sh'\n"),
        )
        .unwrap();
        let out = env.run(&["config", "validate", "--json"]);
        assert!(!out.status.success(), "{}", stdout_of(&out));
    }
    let mut deep = "[[agents]]\nname='base'\ncommand='sh'\n".to_string();
    for i in 0..33 {
        let base = if i == 0 {
            "base".into()
        } else {
            format!("v{}", i - 1)
        };
        deep.push_str(&format!("[[agents]]\nname='v{i}'\nextends='{base}'\n"));
    }
    std::fs::write(&file, deep).unwrap();
    let out = env.run(&["config", "validate", "--json"]);
    assert!(!out.status.success());
    assert!(
        stdout_of(&out).contains("extends depth"),
        "{}",
        stdout_of(&out)
    );
}

#[cfg(unix)]
#[test]
fn registry_agent_arguments_survive_creation_restart_resume_and_fork() {
    use std::os::unix::fs::PermissionsExt;
    if thurbox::paths::resolve_on_path("tmux").is_none() {
        return;
    }
    let env = Env::new();
    let recorder = env.path("work/recorder");
    std::fs::write(
        &recorder,
        "#!/bin/sh\nprintf '%s\\n' \"$@\" > \"$CAPTURE\"\nexec sleep 600\n",
    )
    .unwrap();
    std::fs::set_permissions(&recorder, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::write(env.path("config/agents.toml"), format!(
        "[[agents]]\nname='record'\ncommand='{}'\nargs=['base']\nresume_args=['resume','{{id}}']\nfork_args=['fork','{{id}}']\nnew_session_args=['new','{{id}}']\nresume_latest=true\n", recorder.display()
    )).unwrap();
    let capture = env.path("work/capture");
    let out = env.run(&[
        "session",
        "create",
        "--name",
        "original",
        "--repo-path",
        env.path("work").to_str().unwrap(),
        "--agent",
        "record",
        "--agent-arg=--opaque",
        "--agent-arg",
        "space ; $literal {home}",
        "--env",
        &format!("CAPTURE={}", capture.display()),
        "--resume",
        "conversation",
        "--json",
    ]);
    assert!(
        out.status.success(),
        "{} {}",
        stdout_of(&out),
        stderr_of(&out)
    );
    let answer: Value = serde_json::from_slice(&out.stdout).unwrap();
    let id = answer["id"].as_str().unwrap();
    let check = |selection: &str| {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        loop {
            let text = std::fs::read_to_string(&capture).unwrap_or_default();
            if text.starts_with(selection) {
                assert!(
                    text.ends_with("base\n--opaque\nspace ; $literal {home}\n"),
                    "{text}"
                );
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "missing {selection}: {text}"
            );
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    };
    check("resume\nconversation\n");
    let out = env.run(&["session", "get", id, "--json"]);
    let row: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(
        row["agent_args"],
        serde_json::json!(["--opaque", "space ; $literal {home}"])
    );
    std::fs::remove_file(&capture).unwrap();
    let out = env.run(&[
        "session",
        "create",
        "--name",
        "fresh",
        "--repo-path",
        env.path("work").to_str().unwrap(),
        "--agent",
        "record",
        "--agent-arg=--opaque",
        "--agent-arg",
        "space ; $literal {home}",
        "--env",
        &format!("CAPTURE={}", capture.display()),
        "--json",
    ]);
    assert!(
        out.status.success(),
        "{} {}",
        stdout_of(&out),
        stderr_of(&out)
    );
    check("new\n");
    std::fs::remove_file(&capture).unwrap();
    let out = env.run(&["session", "restart", id, "--json"]);
    assert!(out.status.success(), "{}", stdout_of(&out));
    check("resume\n");
    let out = env.run(&["session", "stop", id, "--json"]);
    assert!(out.status.success(), "{}", stdout_of(&out));
    std::fs::remove_file(&capture).unwrap();
    let out = env.run(&["session", "start", id, "--json"]);
    assert!(out.status.success(), "{}", stdout_of(&out));
    check("resume\n");
    let out = env.run(&["session", "delete", id, "--json"]);
    assert!(out.status.success(), "{}", stdout_of(&out));
    let out = env.run(&["session", "reap", id, "--json"]);
    assert!(out.status.success(), "{}", stdout_of(&out));
    std::fs::remove_file(&capture).unwrap();
    let out = env.run(&["session", "restore", id, "--json"]);
    assert!(out.status.success(), "{}", stdout_of(&out));
    check("resume\n");
    std::fs::remove_file(&capture).unwrap();
    let out = env.run(&["session", "fork", id, "--name", "forked", "--json"]);
    assert!(
        out.status.success(),
        "{} {}",
        stdout_of(&out),
        stderr_of(&out)
    );
    check("fork\n");
}

#[test]
fn agent_arguments_require_an_explicit_registry_agent() {
    let env = Env::new();
    for extra in [vec![], vec!["--command", "sh"]] {
        let work = env.path("work");
        let mut args = vec![
            "session",
            "create",
            "--name",
            "invalid",
            "--repo-path",
            work.to_str().unwrap(),
            "--agent-arg=--opaque",
        ];
        args.extend(extra);
        let out = env.run(&args);
        assert_eq!(out.status.code(), Some(2));
        let error = format!("{} {}", stdout_of(&out), stderr_of(&out));
        assert!(
            error.contains("--agent") && !error.contains("unexpected argument"),
            "{error}"
        );
    }
}

#[test]
fn extension_uninstall_cleans_both_arrays_after_a_variant_edit() {
    let env = Env::new();
    env.db()
        .set_builtin_extension_optout("hooks", true)
        .unwrap();
    let config = env.path("config/agents.toml");
    std::fs::write(
        &config,
        "[[agents]]\nname='base'\ncommand='sh'\nargs=['base-arg']\n\
        [[agents]]\nname='variant'\nextends='base'\nhook_schema='claude'\nappend_args=['tail']\n",
    )
    .unwrap();
    let source = env.path("extension");
    std::fs::create_dir(&source).unwrap();
    std::fs::write(source.join("extension.toml"), "name='edited-variant-patch'\n[[agent_patches]]\nname='claude'\nappend_args=['--patch','payload']\n").unwrap();
    let home = env.path("extension-home");
    let install = || {
        let out = env.run(&[
            "extension",
            "install",
            source.to_str().unwrap(),
            "--home",
            home.to_str().unwrap(),
            "--json",
        ]);
        assert!(out.status.success(), "{}", stdout_of(&out));
    };
    let args = || {
        let out = env.run(&["agent", "launch-args", "variant", "--json"]);
        assert!(out.status.success(), "{}", stdout_of(&out));
        serde_json::from_slice::<Value>(&out.stdout).unwrap()["args"].clone()
    };
    install();
    assert_eq!(
        args(),
        serde_json::json!(["base-arg", "--patch", "payload", "tail"])
    );
    let mut doc: toml_edit::DocumentMut =
        std::fs::read_to_string(&config).unwrap().parse().unwrap();
    doc["agents"]
        .as_array_of_tables_mut()
        .unwrap()
        .get_mut(1)
        .unwrap()["args"] = toml_edit::value(toml_edit::Array::from_iter(["replacement"]));
    std::fs::write(&config, doc.to_string()).unwrap();
    install();
    install();
    let reinstalled = args();
    let mut doc: toml_edit::DocumentMut =
        std::fs::read_to_string(&config).unwrap().parse().unwrap();
    // An older install could have persisted the same patch in both arrays.
    doc["agents"]
        .as_array_of_tables_mut()
        .unwrap()
        .get_mut(1)
        .unwrap()["append_args"] =
        toml_edit::value(toml_edit::Array::from_iter(["--patch", "payload", "tail"]));
    std::fs::write(&config, doc.to_string()).unwrap();
    let out = env.run(&[
        "extension",
        "uninstall",
        "edited-variant-patch",
        "--purge",
        "--json",
    ]);
    assert!(out.status.success(), "{}", stdout_of(&out));
    assert!(!home.exists());
    assert_eq!(
        (reinstalled, args()),
        (
            serde_json::json!(["--patch", "payload", "replacement", "tail"]),
            serde_json::json!(["replacement", "tail"]),
        )
    );
}

#[test]
fn extension_patches_cover_replacing_and_appending_variants_and_uninstall_cleanly() {
    let env = Env::new();
    env.db()
        .set_builtin_extension_optout("hooks", true)
        .unwrap();
    std::fs::write(
        env.path("config/agents.toml"),
        r#"
[[agents]]
name = "base"
command = "sh"
args = ["base-arg"]
hook_schema = "claude"
[[agents]]
name = "append"
extends = "base"
append_args = ["variant-arg"]
[[agents]]
name = "replace"
extends = "append"
args = ["replacement"]
append_args = ["tail"]
[[agents]]
name = "empty"
extends = "base"
args = []
"#,
    )
    .unwrap();
    let source = env.path("extension");
    std::fs::create_dir(&source).unwrap();
    std::fs::write(source.join("extension.toml"), "name='variant-patch'\n[[agent_patches]]\nname='base'\nappend_args=['--patch', 'payload']\n").unwrap();
    let install = || {
        let out = env.run(&[
            "extension",
            "install",
            source.to_str().unwrap(),
            "--home",
            env.path("extension-home").to_str().unwrap(),
            "--json",
        ]);
        assert!(
            out.status.success(),
            "{} {}",
            stdout_of(&out),
            stderr_of(&out)
        );
    };
    install();
    install();
    let args = |agent: &str| {
        let out = env.run(&["agent", "launch-args", agent, "--json"]);
        assert!(out.status.success(), "{}", stdout_of(&out));
        let doc: Value = serde_json::from_slice(&out.stdout).unwrap();
        assert_eq!(doc["hook_coverage"], "full");
        doc["args"].clone()
    };
    assert_eq!(
        args("append"),
        serde_json::json!(["base-arg", "--patch", "payload", "variant-arg"])
    );
    assert_eq!(
        args("replace"),
        serde_json::json!(["--patch", "payload", "replacement", "tail"])
    );
    assert_eq!(args("empty"), serde_json::json!(["--patch", "payload"]));
    let out = env.run(&["extension", "uninstall", "variant-patch", "--json"]);
    assert!(
        out.status.success(),
        "{} {}",
        stdout_of(&out),
        stderr_of(&out)
    );
    assert_eq!(
        args("append"),
        serde_json::json!(["base-arg", "variant-arg"])
    );
    assert_eq!(args("replace"), serde_json::json!(["replacement", "tail"]));
    assert_eq!(args("empty"), serde_json::json!([]));
}
