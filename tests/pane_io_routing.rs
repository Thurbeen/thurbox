//! Text, keys and reads reach a session's pane through the backend its route
//! names, and through nothing else.
//!
//! Every pathway here runs through the real entry points: `cli::run`'s
//! command modules in-process with the registry the binary would build handed
//! in, the kernel's command bus, and the snapshot store's pane probe. The
//! registry carries an in-memory backend for `local:rmux` and
//! `ssh:probehost:rmux`, routes no adapter in this build serves. The probe
//! never runs a process and never speaks the tmux command grammar, so text
//! that shows on its screen can only have got there through the trait, and a
//! private tmux server that stays empty proves nothing went the old way.
//!
//! The namesake test is the reason this matters beyond a second backend: a
//! row on a host and a local window of the same name are different sessions,
//! and a send that resolved the local server by name alone typed one row's
//! prompt into the other's agent.

#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, Instant};

use clap::Parser;
use serde_json::Value;
use thurbox::backend::{SessionBackend, WindowRole};
use thurbox::kernel::command::{Command as KernelCommand, CommandBus, Phase};
use thurbox::session::{Multiplexer, Route, SessionId, Via};
use thurbox::storage::Database;
use thurbox::sync::SharedSession;

#[path = "support/tmux_server.rs"]
mod tmux_server;

#[path = "support/recording_backend.rs"]
mod recording_backend;

use recording_backend::RecordingBackend;
use tmux_server::TmuxServer;

const SOCKET: &str = "thurbox-pane-io-routing";

/// Driven directly: sharing off, so the kernel and the sweeps reach the host
/// through its backend, and a CLI pane verb delegates to the host's own CLI
/// (which the stand-in `ssh` refuses).
const HOSTS_TOML: &str = "[[hosts]]\n\
     name = \"probehost\"\n\
     destination = \"e2e@probehost.invalid\"\n\
     multiplexer = \"rmux\"\n\
     share_sessions = false\n";

const AGENTS_TOML: &str =
    "default = \"shell\"\n\n[[agents]]\nname = \"shell\"\ncommand = \"sh\"\nargs = []\n";

fn have_tmux() -> bool {
    Command::new("tmux")
        .arg("-V")
        .output()
        .is_ok_and(|o| o.status.success())
}

/// The instance under test: its own config and data, a private tmux server,
/// and an `ssh` on `PATH` that records being asked and never connects.
struct Instance {
    root: tempfile::TempDir,
    server: TmuxServer,
    repo: tempfile::TempDir,
}

impl Instance {
    /// `multiplexer` is the settings default a new session is created on.
    fn new(multiplexer: Option<&str>) -> Self {
        let root = tempfile::tempdir().expect("tempdir");
        let server = TmuxServer::pin(SOCKET);
        // Both forms: the thread-local override for this thread, and the
        // process-wide ones for the command bus's workers, which would
        // otherwise resolve the real paths.
        thurbox::paths::set_test_dir(root.path());
        std::env::set_var(thurbox::paths::CONFIG_DIR_OVERRIDE_ENV, root.path());
        std::env::set_var(thurbox::paths::DATA_DIR_OVERRIDE_ENV, root.path());
        let config = thurbox::paths::config_file()
            .expect("config path")
            .parent()
            .expect("config dir")
            .to_path_buf();
        std::fs::create_dir_all(&config).expect("mkdir config");
        std::fs::write(config.join("agents.toml"), AGENTS_TOML).expect("agents.toml");
        std::fs::write(config.join("hosts.toml"), HOSTS_TOML).expect("hosts.toml");

        let bin = root.path().join("bin");
        std::fs::create_dir_all(&bin).expect("mkdir bin");
        let ssh = bin.join("ssh");
        std::fs::write(
            &ssh,
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\nexit 255\n",
                root.path().join("ssh.log").display()
            ),
        )
        .expect("ssh stand-in");
        std::fs::set_permissions(&ssh, std::fs::Permissions::from_mode(0o700)).expect("chmod");
        let path = std::env::var_os("PATH").unwrap_or_default();
        let mut dirs = vec![bin];
        dirs.extend(std::env::split_paths(&path));
        std::env::set_var("PATH", std::env::join_paths(dirs).expect("PATH"));
        for var in ["TMUX", "TMUX_PANE", "THURBOX_SESSION", "THURBOX_SESSION_ID"] {
            std::env::remove_var(var);
        }
        // No heartbeat: arming one is a supervisor window on this machine's
        // own multiplexer whatever a session's route, and it is routed with
        // status, not here. Automations still fire through `automation tick`.
        let mut settings = thurbox::session::settings::Settings::default();
        settings.features.automations = false;
        settings.multiplexer = multiplexer.map(str::to_string);
        thurbox::session::settings::init(settings);
        // What a headless create reads its default multiplexer from.
        if let Some(mux) = multiplexer {
            std::fs::write(
                config.join("settings.toml"),
                format!("multiplexer = \"{mux}\"\n\n[features]\nautomations = false\n"),
            )
            .expect("settings.toml");
        }

        let repo = repo();
        Self { root, server, repo }
    }

    fn repo(&self) -> String {
        self.repo.path().display().to_string()
    }

    /// The database the command bus opens for itself, so both halves read one.
    fn db(&self) -> Database {
        Database::open(&thurbox::paths::database_file().expect("db path")).expect("open db")
    }

    fn ssh_log(&self) -> String {
        std::fs::read_to_string(self.root.path().join("ssh.log")).unwrap_or_default()
    }

    fn tmux_windows(&self) -> Vec<String> {
        let out = self
            .server
            .tmux(&["list-windows", "-a", "-F", "#{window_name}"]);
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .map(str::to_string)
            .collect()
    }

    fn assert_tmux_untouched(&self, after: &str) {
        let windows = self.tmux_windows();
        assert!(
            windows.is_empty(),
            "after {after}, the private tmux server gained windows: {windows:?}"
        );
    }

    /// What a pane on the private tmux server shows.
    fn tmux_screen(&self, pane: &str) -> String {
        let out = self.server.tmux(&["capture-pane", "-p", "-J", "-t", pane]);
        String::from_utf8_lossy(&out.stdout).into_owned()
    }
}

fn git(dir: &Path, args: &[&str]) {
    let mut cmd = Command::new("git");
    cmd.args(args).current_dir(dir);
    for var in [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_INDEX_FILE",
        "GIT_COMMON_DIR",
        "GIT_OBJECT_DIRECTORY",
        "GIT_ALTERNATE_OBJECT_DIRECTORIES",
        "GIT_PREFIX",
        "GIT_NAMESPACE",
    ] {
        cmd.env_remove(var);
    }
    assert!(cmd.output().expect("git").status.success(), "git {args:?}");
}

fn repo() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    git(dir.path(), &["init", "-q", "-b", "main"]);
    git(dir.path(), &["config", "user.email", "t@example.com"]);
    git(dir.path(), &["config", "user.name", "thurbox-test"]);
    git(dir.path(), &["config", "commit.gpgsign", "false"]);
    std::fs::write(dir.path().join("README.md"), "# probe\n").expect("write");
    git(dir.path(), &["add", "."]);
    git(dir.path(), &["commit", "-qm", "init"]);
    dir
}

fn seed_row(db: &Database, id: SessionId, name: &str, backend_type: &str, pane: &str, cwd: &str) {
    db.upsert_session(&SharedSession {
        id,
        name: name.into(),
        agent: "shell".into(),
        backend_id: pane.into(),
        backend_type: backend_type.into(),
        agent_session_id: Some(uuid::Uuid::new_v4().to_string()),
        cwd: Some(PathBuf::from(cwd)),
        additional_dirs: Vec::new(),
        worktrees: Vec::new(),
        shell_backend_id: None,
        parent_session_id: None,
        display_order: None,
        tombstone: false,
        tombstone_at: None,
    })
    .expect("seed a row");
}

/// The configured registry, with the two probe routes registered on it.
fn configured_with(
    local: &Arc<RecordingBackend>,
    far: &Arc<RecordingBackend>,
) -> thurbox::backend::BackendRegistry {
    let (mut backends, _hosts, _warnings) = thurbox::backend::wiring::configured();
    backends.register(Route::local(Some(Multiplexer::Rmux)), local.clone());
    backends.register(
        Route::remote(Via::Ssh, "probehost", Some(Multiplexer::Rmux)),
        far.clone(),
    );
    backends
}

/// One registry for the CLI and one for the kernel, as the two binaries each
/// build their own — serving the same two probes.
struct Registries {
    cli: thurbox::cli::Backends<'static>,
    kernel: Arc<thurbox::backend::BackendRegistry>,
    probe: Arc<RecordingBackend>,
    far: Arc<RecordingBackend>,
}

fn registries() -> Registries {
    let probe = RecordingBackend::new(&Route::local(Some(Multiplexer::Rmux)));
    let far = RecordingBackend::new(&Route::remote(
        Via::Ssh,
        "probehost",
        Some(Multiplexer::Rmux),
    ));
    Registries {
        cli: thurbox::cli::Backends::ready(configured_with(&probe, &far)),
        kernel: Arc::new(configured_with(&probe, &far)),
        probe,
        far,
    }
}

/// `thurbox-cli --json <args>` in-process, returning the document it printed.
fn cli(
    db: &Database,
    backends: &thurbox::cli::Backends<'_>,
    args: &[&str],
) -> Result<Value, String> {
    let parsed =
        thurbox::cli::Cli::try_parse_from(["thurbox-cli", "--json"].iter().chain(args).copied())
            .map_err(|e| format!("parse {args:?}: {e}"))?;
    let output = match parsed.command {
        Some(thurbox::cli::Command::Session { action }) => {
            thurbox::cli::sessions::run(action, db, backends).map_err(|e| e.message)?
        }
        Some(thurbox::cli::Command::Automation { action }) => {
            thurbox::cli::automations::run(action, db, backends)?
        }
        Some(thurbox::cli::Command::Task { action }) => {
            thurbox::cli::tasks::run(action, db, backends)?
        }
        Some(thurbox::cli::Command::Message { action }) => {
            thurbox::cli::messages::run(action, db).map_err(|e| e.message)?
        }
        // A stream rather than a document: run it through the dispatcher,
        // which prints each line, and report only whether it ran.
        Some(thurbox::cli::Command::Watch(_)) => {
            let parsed = thurbox::cli::Cli::try_parse_from(
                ["thurbox-cli", "--json"].iter().chain(args).copied(),
            )
            .expect("parsed once already");
            thurbox::cli::run(parsed, db, backends).map_err(|e| e.message)?;
            return Ok(Value::Null);
        }
        other => panic!("not a command this test drives: {other:?}"),
    };
    match output.failure {
        Some(failure) => Err(failure),
        None => Ok(output.json),
    }
}

/// [`cli`], returning the document whether or not the command failed.
fn cli_doc(db: &Database, backends: &thurbox::cli::Backends<'_>, args: &[&str]) -> Value {
    let parsed =
        thurbox::cli::Cli::try_parse_from(["thurbox-cli", "--json"].iter().chain(args).copied())
            .expect("parse");
    match parsed.command {
        Some(thurbox::cli::Command::Session { action }) => {
            thurbox::cli::sessions::run(action, db, backends)
                .map_err(|e| e.message)
                .expect("a document")
                .json
        }
        other => panic!("not a command this test drives: {other:?}"),
    }
}

/// The check named `check` wherever it sits in a document.
fn find_check(doc: &Value, check: &str) -> Option<Value> {
    match doc {
        Value::Object(map) if map.get("check").and_then(Value::as_str) == Some(check) => {
            Some(doc.clone())
        }
        Value::Object(map) => map.values().find_map(|v| find_check(v, check)),
        Value::Array(items) => items.iter().find_map(|v| find_check(v, check)),
        _ => None,
    }
}

/// Dispatch `command` on the kernel's bus and wait for it to finish.
fn kernel(bus: &mut CommandBus, command: KernelCommand) -> Result<(), String> {
    let id = bus.dispatch(command);
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline {
        bus.poll();
        match bus.inflight().into_iter().find(|entry| entry.id == id) {
            None => return Ok(()),
            Some(entry) if entry.phase == Phase::Failed => {
                return Err(entry.error.unwrap_or_default())
            }
            Some(_) => std::thread::sleep(Duration::from_millis(20)),
        }
    }
    panic!("the kernel command never finished");
}

/// Mark an automation due and fire it headlessly, returning its last run.
fn fire(db: &Database, backends: &thurbox::cli::Backends<'_>, id: i64) -> Value {
    cli(db, backends, &["automation", "run", &id.to_string()]).expect("mark due");
    cli(db, backends, &["automation", "tick"]).expect("tick");
    let runs = cli(db, backends, &["automation", "runs", &id.to_string()]).expect("runs");
    runs.as_array()
        .and_then(|runs| runs.first())
        .cloned()
        .unwrap_or_else(|| panic!("automation {id} has no run: {runs}"))
}

fn run_status(run: &Value) -> (String, String) {
    (
        run["status"].as_str().unwrap_or_default().to_string(),
        run["detail"].as_str().unwrap_or_default().to_string(),
    )
}

/// The one agent window `probe` holds for `id`.
fn agent_pane(probe: &RecordingBackend, id: &str) -> String {
    let windows = probe.windows_of(id);
    assert_eq!(windows.len(), 1, "{id} should own one window: {windows:?}");
    windows[0].pane.clone()
}

fn assert_screen_shows(probe: &RecordingBackend, pane: &str, text: &str, after: &str) {
    let screen = probe.screen(pane);
    assert!(
        screen.contains(text),
        "after {after}, {pane} on {} should show {text:?}; it shows {screen:?} \
         (every call it received: {:?})",
        probe.name(),
        probe.calls()
    );
}

#[test]
fn every_pane_pathway_reaches_the_backend_the_route_names() {
    if !have_tmux() {
        eprintln!("skipping: tmux is not installed");
        return;
    }
    // New sessions — a task's, an automation's — are created on the probe too.
    let instance = Instance::new(Some("rmux"));
    let db = instance.db();
    let reg = registries();
    let (backends, probe) = (&reg.cli, &reg.probe);
    let repo = instance.repo();

    let id = SessionId::default();
    let uuid = id.to_string();
    let pane = probe.open("tb-probe", &uuid, WindowRole::Agent);
    seed_row(&db, id, "probe", "local:rmux", &pane, &repo);

    // CLI send, unsubmitted and submitted, and a key.
    cli(
        &db,
        backends,
        &["session", "send", &uuid, "typed only", "--no-enter"],
    )
    .expect("send --no-enter");
    assert_screen_shows(probe, &pane, "typed only", "session send --no-enter");
    cli(&db, backends, &["session", "key", &uuid, "enter"]).expect("key");
    assert!(probe.called("send_key"), "session key: {:?}", probe.calls());
    cli(&db, backends, &["session", "send", &uuid, "from the cli"]).expect("send");
    assert_screen_shows(probe, &pane, "from the cli", "session send");

    // CLI capture reads the probe's screen, and its pane state beside it.
    let captured = cli(&db, backends, &["session", "capture", &uuid]).expect("capture");
    assert!(
        captured["output"]
            .as_str()
            .is_some_and(|out| out.contains("from the cli")),
        "capture reads the probe's pane: {captured}"
    );
    assert!(probe.called("pane_state"), "capture's pane state");
    instance.assert_tmux_untouched("send, key and capture");

    // The kernel's Send, as the interface issues it.
    let mut bus = CommandBus::new(Arc::clone(&reg.kernel));
    kernel(
        &mut bus,
        KernelCommand::Send {
            session: uuid.clone(),
            text: "from the kernel".into(),
        },
    )
    .expect("kernel send");
    assert_screen_shows(probe, &pane, "from the kernel", "the kernel's Send");

    // A task handed to the running session, through the kernel and the CLI.
    let task = cli(
        &db,
        backends,
        &[
            "task",
            "create",
            "--title",
            "kernel dispatch",
            "--session",
            &uuid,
        ],
    )
    .expect("task create");
    let task_id = task["id"].as_i64().expect("task id");
    kernel(
        &mut bus,
        KernelCommand::DispatchTask {
            task: task_id,
            session: Some(uuid.clone()),
        },
    )
    .expect("dispatch to the session");
    assert_screen_shows(probe, &pane, "kernel dispatch", "dispatch_task");
    let task = cli(
        &db,
        backends,
        &[
            "task",
            "create",
            "--title",
            "cli task run",
            "--session",
            &uuid,
        ],
    )
    .expect("task create");
    cli(
        &db,
        backends,
        &["task", "run", &task["id"].as_i64().unwrap().to_string()],
    )
    .expect("task run");
    assert_screen_shows(probe, &pane, "cli task run", "task run");

    // A task with no session creates one — on the probe, the settings'
    // default — and hands it the prompt once it boots.
    let task = cli(&db, backends, &["task", "create", "--title", "a fresh one"]).expect("task");
    let fresh_task = task["id"].as_i64().unwrap();
    kernel(
        &mut bus,
        KernelCommand::DispatchTask {
            task: fresh_task,
            session: None,
        },
    )
    .expect("dispatch to a new session");
    let fresh = db
        .get_session_by_name(&format!("task-{fresh_task}"))
        .expect("read")
        .expect("the task's session");
    assert_eq!(fresh.backend_type, "local:rmux");
    let fresh_pane = agent_pane(probe, &fresh.id.to_string());
    assert_screen_shows(
        probe,
        &fresh_pane,
        "a fresh one",
        "dispatch to a new session",
    );
    instance.assert_tmux_untouched("the kernel's send and dispatch");

    // A send automation, and a spawn automation fired twice: the first
    // creates its session, the second reuses it.
    let send = cli(
        &db,
        backends,
        &[
            "automation",
            "create",
            "--name",
            "send",
            "--trigger",
            "hourly",
            "--session",
            &uuid,
            "--prompt",
            "automation send",
        ],
    )
    .expect("create a send automation");
    let run = fire(&db, backends, send["id"].as_i64().unwrap());
    assert_eq!(run_status(&run).0, "success", "fire_send: {run}");
    assert_screen_shows(probe, &pane, "automation send", "fire_send");

    let spawn = cli(
        &db,
        backends,
        &[
            "automation",
            "create",
            "--name",
            "spawn",
            "--trigger",
            "hourly",
            "--repo",
            &repo,
            "--prompt",
            "automation spawn",
        ],
    )
    .expect("create a spawn automation");
    let spawn_id = spawn["id"].as_i64().unwrap();
    let run = fire(&db, backends, spawn_id);
    assert_eq!(run_status(&run).0, "success", "fire_spawn: {run}");
    let spawned = db
        .get_session_by_name(&format!("auto-{spawn_id}"))
        .expect("read")
        .expect("the automation's session");
    assert_eq!(spawned.backend_type, "local:rmux");
    let spawned_pane = agent_pane(probe, &spawned.id.to_string());
    assert_screen_shows(probe, &spawned_pane, "automation spawn", "fire_spawn");
    let run = fire(&db, backends, spawn_id);
    assert_eq!(
        run_status(&run),
        ("success".into(), format!("reused auto-{spawn_id}"))
    );
    assert_eq!(
        agent_pane(probe, &spawned.id.to_string()),
        spawned_pane,
        "a second fire reuses the window"
    );
    instance.assert_tmux_untouched("the automations");

    // Every reader of a pane's state: the list, the watch, the doctor, and the
    // interface's own pane probe.
    probe.forget_calls();
    cli(&db, backends, &["session", "list", "--verify"]).expect("list --verify");
    assert!(probe.called("pane_state"), "session list --verify");
    probe.forget_calls();
    cli(
        &db,
        backends,
        &["watch", "--initial", "--verify", "--for-secs", "0"],
    )
    .expect("watch --verify");
    assert!(probe.called("pane_state"), "watch --verify");
    probe.forget_calls();
    // Its verdict is about a probe nothing wired hooks into; what matters is
    // where it read the pane's `PATH` from.
    let _ = cli(&db, backends, &["session", "doctor", &uuid]);
    assert!(
        probe.called("pane_path"),
        "session doctor: {:?}",
        probe.calls()
    );

    probe.forget_calls();
    let mut store =
        thurbox::kernel::snapshot::SnapshotStore::with_database(instance.db(), &reg.kernel);
    let deadline = Instant::now() + Duration::from_secs(10);
    while !probe.called("pane_state") && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(100));
        store.refresh_if_due();
    }
    assert!(probe.called("pane_state"), "the snapshot's pane probe");

    // The mailbox hands a message to the agent's own inbox, never its pane.
    probe.forget_calls();
    cli(
        &db,
        backends,
        &[
            "message",
            "send",
            "--to",
            &uuid,
            "--kind",
            "note",
            "--body",
            "mailbox body",
        ],
    )
    .expect("message send");
    assert!(
        !probe.screen(&pane).contains("mailbox body") && !probe.called("send_text"),
        "a message was typed into the pane: {:?}",
        probe.calls()
    );

    instance.assert_tmux_untouched("every pane pathway");
    assert_eq!(instance.ssh_log(), "", "nothing spoke to the host");
}

/// A row on a host and a local window of the same name are two sessions. A
/// local `tb-x` nobody stamped used to answer a send meant for the host's `x`,
/// because the one-shot helpers resolved the local server by name.
#[test]
fn a_remote_rows_text_never_reaches_a_local_namesake() {
    if !have_tmux() {
        eprintln!("skipping: tmux is not installed");
        return;
    }
    let instance = Instance::new(None);
    let db = instance.db();
    let reg = registries();
    let (backends, far) = (&reg.cli, &reg.far);

    // The local namesake: `tb-x` on this machine's server, unstamped, echoing
    // whatever it is sent.
    let local = thurbox::backend::tmux::TmuxBackend::new();
    local.ensure_ready().expect("ready the private server");
    let local_pane = local
        .spawn("tb-x", "cat", &[], None, &Default::default(), 24, 80)
        .expect("spawn the local namesake")
        .backend_id;
    local.shutdown();

    let id = SessionId::default();
    let uuid = id.to_string();
    let far_pane = far.open("tb-x", &uuid, WindowRole::Agent);
    seed_row(&db, id, "x", "ssh:probehost:rmux", &far_pane, "/srv/x");

    let mut bus = CommandBus::new(Arc::clone(&reg.kernel));
    kernel(
        &mut bus,
        KernelCommand::Send {
            session: uuid.clone(),
            text: "for the host".into(),
        },
    )
    .expect("kernel send to the host's row");
    assert_screen_shows(far, &far_pane, "for the host", "the kernel's Send");

    let send = cli(
        &db,
        backends,
        &[
            "automation",
            "create",
            "--name",
            "far",
            "--trigger",
            "hourly",
            "--session",
            &uuid,
            "--prompt",
            "automation for the host",
        ],
    )
    .expect("create a send automation");
    let run = fire(&db, backends, send["id"].as_i64().unwrap());
    assert_eq!(run_status(&run).0, "success", "fire_send: {run}");
    assert_screen_shows(far, &far_pane, "automation for the host", "fire_send");

    let task = cli(
        &db,
        backends,
        &[
            "task",
            "create",
            "--title",
            "task for the host",
            "--session",
            &uuid,
        ],
    )
    .expect("task create");
    cli(
        &db,
        backends,
        &["task", "run", &task["id"].as_i64().unwrap().to_string()],
    )
    .expect("task run");
    assert_screen_shows(far, &far_pane, "task for the host", "task run");

    // The CLI's pane verbs delegate to the host's own CLI, which the stand-in
    // cannot reach: an honest refusal, and still nothing local.
    assert!(
        cli(
            &db,
            backends,
            &["session", "send", &uuid, "cli for the host"]
        )
        .is_err(),
        "a send the host's CLI never answered reported success"
    );

    // Give a mistyped delivery the Enter delay to land before reading.
    std::thread::sleep(Duration::from_millis(500));
    let screen = instance.tmux_screen(&local_pane);
    assert!(
        !screen.contains("host"),
        "the local namesake received the host row's text: {screen:?}"
    );
}

/// A route nothing here serves is refused by name, not typed on the local
/// server.
#[test]
fn a_pane_verb_on_an_unserved_route_is_refused_by_name() {
    if !have_tmux() {
        eprintln!("skipping: tmux is not installed");
        return;
    }
    let instance = Instance::new(None);
    let db = instance.db();
    let reg = registries();
    let id = SessionId::default();
    seed_row(&db, id, "herd", "local:herdr", "%0", &instance.repo());
    let uuid = id.to_string();

    let mut bus = CommandBus::new(Arc::clone(&reg.kernel));
    let refused = kernel(
        &mut bus,
        KernelCommand::Send {
            session: uuid.clone(),
            text: "nowhere".into(),
        },
    )
    .expect_err("a send to an unserved route");
    assert!(refused.contains("no backend here serves"), "{refused}");
    let refused = cli(&db, &reg.cli, &["session", "send", &uuid, "nowhere"]).expect_err("cli send");
    assert!(refused.contains("no backend here serves"), "{refused}");
    instance.assert_tmux_untouched("the refusals");
}

/// A window that no row owns is not a session to deliver to. A spawn
/// automation used to type its prompt into any lone local `tb-auto-<id>` by
/// name; it now finds its session by the row, as every other send does.
#[test]
fn a_spawn_automation_never_types_into_a_window_no_row_owns() {
    if !have_tmux() {
        eprintln!("skipping: tmux is not installed");
        return;
    }
    let instance = Instance::new(None);
    let db = instance.db();
    let reg = registries();

    let spawn = cli(
        &db,
        &reg.cli,
        &[
            "automation",
            "create",
            "--name",
            "spawn",
            "--trigger",
            "hourly",
            "--repo",
            &instance.repo(),
            "--prompt",
            "not for a stranger",
        ],
    )
    .expect("create a spawn automation");
    let spawn_id = spawn["id"].as_i64().unwrap();

    let local = thurbox::backend::tmux::TmuxBackend::new();
    local.ensure_ready().expect("ready the private server");
    let stranger = local
        .spawn(
            &format!("tb-auto-{spawn_id}"),
            "cat",
            &[],
            None,
            &Default::default(),
            24,
            80,
        )
        .expect("a window no row owns")
        .backend_id;
    local.shutdown();

    fire(&db, &reg.cli, spawn_id);
    std::thread::sleep(Duration::from_millis(500));
    let screen = instance.tmux_screen(&stranger);
    assert!(
        !screen.contains("stranger"),
        "the prompt was typed into a window no row owns: {screen:?}"
    );
}

/// A row on a host `hosts.toml` no longer names cannot be the session a spawn
/// automation is reusing here, so it must not stop the automation from
/// spawning; a row whose backend cannot say whether it runs must stop a
/// spawn, never launch a second session beside it.
#[test]
fn a_spawn_reuses_only_what_it_can_see_and_refuses_what_it_cannot_tell() {
    if !have_tmux() {
        eprintln!("skipping: tmux is not installed");
        return;
    }
    let instance = Instance::new(Some("rmux"));
    let db = instance.db();
    let reg = registries();
    let repo = instance.repo();

    let spawn = cli(
        &db,
        &reg.cli,
        &[
            "automation",
            "create",
            "--name",
            "spawn",
            "--trigger",
            "hourly",
            "--repo",
            &repo,
            "--prompt",
            "fresh",
        ],
    )
    .expect("create a spawn automation");
    let spawn_id = spawn["id"].as_i64().unwrap();
    let stale = SessionId::default();
    seed_row(
        &db,
        stale,
        &format!("auto-{spawn_id}"),
        "ssh:gone:rmux",
        "%0",
        "/srv",
    );
    let run = fire(&db, &reg.cli, spawn_id);
    assert_eq!(
        run_status(&run),
        ("success".into(), format!("spawned auto-{spawn_id}")),
        "a row on a host nothing here knows blocked the spawn: {run}"
    );

    // A task whose earlier session sits on a host that does not answer,
    // while a new one could still be created here.
    let task = cli(
        &db,
        &reg.cli,
        &["task", "create", "--title", "unsure", "--repo", &repo],
    )
    .expect("task create");
    let task_id = task["id"].as_i64().unwrap();
    let earlier = SessionId::default();
    let pane = reg.far.open(
        &format!("tb-task-{task_id}"),
        &earlier.to_string(),
        WindowRole::Agent,
    );
    seed_row(
        &db,
        earlier,
        &format!("task-{task_id}"),
        "ssh:probehost:rmux",
        &pane,
        "/srv",
    );
    reg.far.set_reachable(false);
    let before = db.list_active_sessions().expect("list").len();
    assert!(
        cli(&db, &reg.cli, &["task", "run", &task_id.to_string()]).is_err(),
        "a task run that could not tell whether its session runs spawned anyway"
    );
    assert_eq!(db.list_active_sessions().expect("list").len(), before);
}

/// Two running sessions answer to a task's tag: which one it meant cannot be
/// told, so neither is typed into.
#[test]
fn a_task_run_refuses_to_pick_between_two_running_sessions() {
    if !have_tmux() {
        eprintln!("skipping: tmux is not installed");
        return;
    }
    let instance = Instance::new(Some("rmux"));
    let db = instance.db();
    let reg = registries();
    let task = cli(
        &db,
        &reg.cli,
        &[
            "task",
            "create",
            "--title",
            "twice",
            "--repo",
            &instance.repo(),
        ],
    )
    .expect("task create");
    let task_id = task["id"].as_i64().unwrap();
    let mut panes = Vec::new();
    for _ in 0..2 {
        let id = SessionId::default();
        let pane = reg.probe.open(
            &format!("tb-task-{task_id}"),
            &id.to_string(),
            WindowRole::Agent,
        );
        seed_row(
            &db,
            id,
            &format!("task-{task_id}"),
            "local:rmux",
            &pane,
            "/srv",
        );
        panes.push(pane);
    }
    assert!(
        cli(&db, &reg.cli, &["task", "run", &task_id.to_string()]).is_err(),
        "a task run picked one of two running sessions"
    );
    for pane in &panes {
        assert_eq!(reg.probe.screen(pane), "", "{pane} was typed into");
    }
}

/// A pane `session doctor` could not read is unverified, never "no pane" — no
/// pane is what lets it answer from its own `PATH` instead.
#[test]
fn the_doctor_reports_a_pane_it_could_not_read_as_unverified() {
    if !have_tmux() {
        eprintln!("skipping: tmux is not installed");
        return;
    }
    let instance = Instance::new(None);
    let db = instance.db();
    let reg = registries();
    let id = SessionId::default();
    let pane = reg.probe.open("tb-doc", &id.to_string(), WindowRole::Agent);
    seed_row(&db, id, "doc", "local:rmux", &pane, &instance.repo());
    reg.probe.set_reachable(false);
    let doc = cli_doc(&db, &reg.cli, &["session", "doctor", &id.to_string()]);
    let check = find_check(&doc, "cli").unwrap_or_else(|| panic!("no cli check: {doc}"));
    assert_eq!(check["level"], "warn", "{check}");
    assert!(
        check["detail"]
            .as_str()
            .is_some_and(|d| d.contains("could not be read")),
        "{check}"
    );
}
