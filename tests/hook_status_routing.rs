//! Hook status reaches a session's row through the backend its route names,
//! and through nothing else.
//!
//! Every pathway runs through the real entry points — `cli`'s command modules
//! in-process, with the registry the binary would build handed in. The
//! registry carries an in-memory backend for each route a row here is on: a
//! local and a remote one no adapter in this build serves (`rmux`), and
//! recorded stand-ins for the tmux and psmux routes of two hosts, so each
//! multiplexer's route is owned by the backend registered for it rather than
//! by a guess from its name or the host's OS. The stand-ins never run a
//! process and never speak the tmux command grammar — they carry no tmux
//! user options — so a state that lands on a row can only have come through
//! the contract, and a private tmux server that stays empty and an `ssh` that
//! is never asked prove nothing went the old way.
//!
//! What the real psmux adapter itself reports is pinned in its own unit tests:
//! its status channel is unproven against psmux, and stays closed.

#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;

use clap::Parser;
use serde_json::Value;
use thurbox::backend::{SessionBackend, WindowRole};
use thurbox::session::{Multiplexer, Route, SessionId, Via};
use thurbox::storage::Database;
use thurbox::sync::SharedSession;

#[path = "support/tmux_server.rs"]
mod tmux_server;

#[path = "support/recording_backend.rs"]
mod recording_backend;

use recording_backend::RecordingBackend;
use tmux_server::TmuxServer;

const SOCKET: &str = "thurbox-hook-status-routing";

/// Sharing off on every host, so nothing here delegates to a host's own CLI:
/// the backend registered for each route is the only thing that can answer.
const HOSTS_TOML: &str = "[[hosts]]\n\
     name = \"probehost\"\n\
     destination = \"e2e@probehost.invalid\"\n\
     multiplexer = \"rmux\"\n\
     share_sessions = false\n\n\
     [[hosts]]\n\
     name = \"tmuxhost\"\n\
     destination = \"e2e@tmuxhost.invalid\"\n\
     multiplexer = \"tmux\"\n\
     share_sessions = false\n\n\
     [[hosts]]\n\
     name = \"winhost\"\n\
     destination = \"e2e@winhost.invalid\"\n\
     multiplexer = \"psmux\"\n\
     platform = \"windows\"\n\
     share_sessions = false\n\n\
     [[hosts]]\n\
     name = \"downhost\"\n\
     destination = \"e2e@downhost.invalid\"\n\
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
}

impl Instance {
    fn new(automations: bool) -> Self {
        let root = tempfile::tempdir().expect("tempdir");
        let server = TmuxServer::pin(SOCKET);
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
        std::fs::write(
            config.join("settings.toml"),
            format!("[features]\nautomations = {automations}\n"),
        )
        .expect("settings.toml");

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
        let mut settings = thurbox::session::settings::Settings::default();
        settings.features.automations = automations;
        thurbox::session::settings::init(settings);
        Self { root, server }
    }

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

    /// Neither the private tmux server nor any host was touched.
    fn assert_untouched(&self, after: &str) {
        let windows = self.tmux_windows();
        assert!(
            windows.is_empty(),
            "after {after}, the private tmux server gained windows: {windows:?}"
        );
        let ssh = self.ssh_log();
        assert!(
            ssh.is_empty(),
            "after {after}, a host was asked over ssh:\n{ssh}"
        );
    }
}

fn seed_row(db: &Database, name: &str, backend_type: &str, pane: &str) -> SessionId {
    let id = SessionId::default();
    db.upsert_session(&SharedSession {
        id,
        name: name.into(),
        agent: "shell".into(),
        backend_id: pane.into(),
        backend_type: backend_type.into(),
        agent_session_id: Some(uuid::Uuid::new_v4().to_string()),
        cwd: Some(PathBuf::from("/")),
        additional_dirs: Vec::new(),
        worktrees: Vec::new(),
        shell_backend_id: None,
        parent_session_id: None,
        display_order: None,
        tombstone: false,
        tombstone_at: None,
    })
    .expect("seed a row");
    id
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
        Some(thurbox::cli::Command::Runtime { action }) => {
            thurbox::cli::runtime::run(action, backends)
        }
        other => panic!("not a command this test drives: {other:?}"),
    };
    match output.failure {
        Some(failure) => Err(failure),
        None => Ok(output.json),
    }
}

fn hook_state(db: &Database, id: SessionId) -> Option<String> {
    db.load_hook_state(id)
        .expect("read the hook state")
        .and_then(|row| row.state)
}

/// One backend per route a row is on, each holding that row's agent window.
struct Routes {
    local: Arc<RecordingBackend>,
    far: Arc<RecordingBackend>,
    tmux_host: Arc<RecordingBackend>,
    psmux_host: Arc<RecordingBackend>,
    down: Arc<RecordingBackend>,
}

fn local_probe() -> Route {
    Route::local(Some(Multiplexer::Rmux))
}

fn far_probe() -> Route {
    Route::remote(Via::Ssh, "probehost", Some(Multiplexer::Rmux))
}

fn tmux_host() -> Route {
    Route::remote(Via::Ssh, "tmuxhost", Some(Multiplexer::Tmux))
}

fn psmux_host() -> Route {
    Route::remote(Via::Ssh, "winhost", Some(Multiplexer::Psmux))
}

fn down_host() -> Route {
    Route::remote(Via::Ssh, "downhost", Some(Multiplexer::Rmux))
}

impl Routes {
    fn new() -> Self {
        Self {
            local: RecordingBackend::new(&local_probe()),
            far: RecordingBackend::new(&far_probe()),
            tmux_host: RecordingBackend::new(&tmux_host()),
            psmux_host: RecordingBackend::new(&psmux_host()),
            down: RecordingBackend::new(&down_host()),
        }
    }

    /// The registry the binary builds, with every route above served by its
    /// recorded backend — the tmux and psmux hosts' real adapters replaced.
    fn registry(&self) -> thurbox::backend::BackendRegistry {
        let (mut backends, _hosts, _warnings) = thurbox::backend::wiring::configured();
        backends.register(local_probe(), self.local.clone());
        backends.register(far_probe(), self.far.clone());
        backends.register(tmux_host(), self.tmux_host.clone());
        backends.register(psmux_host(), self.psmux_host.clone());
        backends.register(down_host(), self.down.clone());
        backends
    }
}

/// Each backend reports a hook state for a pane of the same id, and
/// `automation tick` writes each to the row on that backend's route — with no
/// local tmux pane, no host contacted, and nothing inferred for a route that
/// could not answer.
#[test]
fn automation_tick_records_each_routes_own_hook_state() {
    if !have_tmux() {
        eprintln!("skipping: tmux not installed");
        return;
    }
    let instance = Instance::new(false);
    let db = instance.db();
    let routes = Routes::new();

    // The same pane id on every backend: only the route tells them apart.
    let mut rows = Vec::new();
    for (name, route, backend, state) in [
        ("local-probe", local_probe(), &routes.local, "working"),
        ("far-probe", far_probe(), &routes.far, "blocked"),
        ("on-tmux", tmux_host(), &routes.tmux_host, "done"),
        ("on-psmux", psmux_host(), &routes.psmux_host, "idle"),
    ] {
        let id = seed_row(&db, name, &route.format(), "pane-0");
        let pane = backend.open(&format!("tb-{name}"), &id.to_string(), WindowRole::Agent);
        assert_eq!(pane, "pane-0");
        backend.hook(&pane, state);
        rows.push((name, id, state));
    }
    // A route nothing here serves, and one whose machine does not answer:
    // neither may be told anything, and the state held is kept, never idle.
    let unserved = seed_row(
        &db,
        "unserved",
        &Route::remote(Via::Ssh, "probehost", Some(Multiplexer::Herdr)).format(),
        "pane-0",
    );
    let down = seed_row(&db, "down", &down_host().format(), "pane-0");
    let pane = routes
        .down
        .open("tb-down", &down.to_string(), WindowRole::Agent);
    routes.down.hook(&pane, "done");
    routes.down.set_reachable(false);
    db.set_hook_state(down, "working")
        .expect("seed a held state");

    let backends = thurbox::cli::Backends::ready(routes.registry());
    cli(&db, &backends, &["automation", "tick"]).expect("automation tick");

    for (name, id, state) in &rows {
        assert_eq!(
            hook_state(&db, *id).as_deref(),
            Some(*state),
            "{name}: the tick did not record the state its own backend reported"
        );
    }
    assert_eq!(
        hook_state(&db, unserved),
        None,
        "a route no backend serves has no status to read"
    );
    assert_eq!(
        hook_state(&db, down).as_deref(),
        Some("working"),
        "a backend that did not answer is no news, and never idle"
    );
    instance.assert_untouched("automation tick");
}

/// `session signal` puts the state on the row and on the row's own pane,
/// through the row's backend — the channel a peer's interface reads live —
/// rather than on whatever tmux pane the caller happens to be in.
#[test]
fn session_signal_reaches_the_rows_own_backend() {
    if !have_tmux() {
        eprintln!("skipping: tmux not installed");
        return;
    }
    let instance = Instance::new(false);
    let db = instance.db();
    let routes = Routes::new();
    let id = seed_row(&db, "local-probe", &local_probe().format(), "pane-0");
    let pane = routes
        .local
        .open("tb-local-probe", &id.to_string(), WindowRole::Agent);

    let backends = thurbox::cli::Backends::ready(routes.registry());
    cli(
        &db,
        &backends,
        &[
            "session",
            "signal",
            "--state",
            "blocked",
            "--session",
            &id.to_string(),
        ],
    )
    .expect("session signal");

    assert_eq!(hook_state(&db, id).as_deref(), Some("blocked"));
    let window = routes
        .local
        .windows()
        .into_iter()
        .find(|w| w.pane == pane)
        .expect("the row's window");
    assert_eq!(
        window.hook.as_deref(),
        Some("blocked"),
        "the row's backend was never told; calls: {:?}",
        routes.local.calls()
    );
    instance.assert_untouched("session signal");
}

/// A hook runs `session signal` on every tool call, so for a local row it must
/// not build the registry that reads `hosts.toml` and, on Windows and inside
/// WSL, runs `wsl.exe` to discover distros: this machine's backends are
/// enough to find the row's.
#[test]
fn session_signal_on_a_local_row_builds_no_host_registry() {
    if !have_tmux() {
        eprintln!("skipping: tmux not installed");
        return;
    }
    let instance = Instance::new(false);
    let db = instance.db();
    let routes = Routes::new();
    let id = seed_row(&db, "local-probe", &local_probe().format(), "pane-0");
    let pane = routes
        .local
        .open("tb-local-probe", &id.to_string(), WindowRole::Agent);

    // Host discovery runs `wsl.exe -l -q` wherever one is on PATH: a stand-in
    // that records being asked is what shows whether this signal read hosts.
    let wsl = instance.root.path().join("bin/wsl.exe");
    std::fs::write(
        &wsl,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\n",
            instance.root.path().join("wsl.log").display()
        ),
    )
    .expect("wsl.exe stand-in");
    std::fs::set_permissions(&wsl, std::fs::Permissions::from_mode(0o700)).expect("chmod");
    let hosts = || -> thurbox::backend::BackendRegistry {
        panic!("a local signal built the registry of every host")
    };
    // What the binary's root hands down: this machine's adapters alone, with
    // the probe serving the row's local route.
    let here = || {
        let mut registry = thurbox::backend::wiring::local_only();
        registry.register(local_probe(), routes.local.clone());
        registry
    };
    let backends = thurbox::cli::Backends::lazy(&hosts).with_local(&here);
    cli(
        &db,
        &backends,
        &[
            "session",
            "signal",
            "--state",
            "done",
            "--session",
            &id.to_string(),
        ],
    )
    .expect("session signal");
    let window = routes
        .local
        .windows()
        .into_iter()
        .find(|w| w.pane == pane)
        .expect("the row's window");
    assert_eq!(window.hook.as_deref(), Some("done"));
    let asked = std::fs::read_to_string(instance.root.path().join("wsl.log")).unwrap_or_default();
    assert!(
        asked.is_empty(),
        "a local signal discovered WSL hosts: {asked}"
    );
}

/// Arming the heartbeat is a request to this machine's backend — the
/// registry's default — and not a window on whatever tmux server is local.
#[test]
fn the_heartbeat_is_kept_by_the_local_backend() {
    if !have_tmux() {
        eprintln!("skipping: tmux not installed");
        return;
    }
    let instance = Instance::new(true);
    let db = instance.db();
    let routes = Routes::new();
    let mut registry = routes.registry();
    let default = registry.default_route().clone();
    let here = RecordingBackend::new(&default);
    registry.register(default, here.clone());
    let backends = thurbox::cli::Backends::ready(registry);

    cli(
        &db,
        &backends,
        &[
            "automation",
            "create",
            "--name",
            "nightly",
            "--trigger",
            "hourly",
            "--command",
            "true",
        ],
    )
    .expect("automation create");

    instance.assert_untouched("arming the heartbeat");
    assert!(
        here.calls()
            .iter()
            .any(|c| c.starts_with("ensure_heartbeat")),
        "the local backend was never asked to keep the heartbeat; calls: {:?}",
        here.calls()
    );

    // What `runtime` reports and stops is that backend's heartbeat, and each
    // local backend answers for its own status channel.
    let status = cli(&db, &backends, &["runtime", "status"]).expect("runtime status");
    assert_eq!(
        status["automation_heartbeat"],
        Value::Bool(true),
        "{status}"
    );
    assert_eq!(status["backend"], Value::String(here.name().to_string()));
    assert_eq!(
        status["hook_status"][local_probe().format()],
        Value::Bool(true),
        "{status}"
    );
    let stopped = cli(&db, &backends, &["runtime", "stop"]).expect("runtime stop");
    assert_eq!(stopped["stopped"], Value::Bool(true));
    let status = cli(&db, &backends, &["runtime", "status"]).expect("runtime status");
    assert_eq!(status["automation_heartbeat"], Value::Bool(false));
    instance.assert_untouched("runtime status and stop");
}
