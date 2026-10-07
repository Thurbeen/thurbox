//! A session's lifecycle is driven through the backend its route names, and
//! through nothing else.
//!
//! Every verb here runs through the real entry points — `cli::run` in-process,
//! with the registry the binary would build handed in, and the `session_ops`
//! sweeps the heartbeat drives — against a registry with an in-memory backend
//! registered for `local:rmux` and `ssh:probehost:rmux`, overriding the real
//! adapter to record each call. The probe never runs a process or speaks the tmux
//! command grammar, so a window it holds can only have got there through the
//! trait, and a private tmux server that stays empty proves nothing went the
//! old way. The same seam lets the real RMUX adapter own lifecycle without
//! `session_ops` naming it.
//!
//! The second test is the other half. A route nothing is registered for is
//! refused by every verb, with the row left as it was and no window opened
//! anywhere — never quietly driven on the local tmux server.

#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use clap::Parser;
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

const SOCKET: &str = "thurbox-lifecycle-routing";

/// Where the probe hosts sit in `hosts.toml`. Driven directly: sharing off,
/// so nothing is delegated to a CLI on the host.
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

/// The instance under test: its own config and data, a private tmux server, and
/// an `ssh` on `PATH` that records being asked and never connects.
struct Instance {
    root: tempfile::TempDir,
    server: TmuxServer,
    repo: tempfile::TempDir,
}

impl Instance {
    fn new() -> Self {
        let root = tempfile::tempdir().expect("tempdir");
        let server = TmuxServer::pin(SOCKET);
        thurbox::paths::set_test_dir(root.path());
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
        // No automations, so no heartbeat: `session create` arms one, and it is
        // a supervisor window on this machine's own multiplexer whatever the
        // session's route — status and the heartbeat are routed separately.
        let mut settings = thurbox::session::settings::Settings::default();
        settings.features.automations = false;
        thurbox::session::settings::init(settings);

        Self {
            root,
            server,
            repo: repo(),
        }
    }

    fn repo(&self) -> &Path {
        self.repo.path()
    }

    /// What `ssh` was asked, one invocation per line.
    fn ssh_log(&self) -> String {
        std::fs::read_to_string(self.root.path().join("ssh.log")).unwrap_or_default()
    }

    /// Every window the private tmux server holds; empty when it never
    /// started, which is the state every test here expects.
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
}

/// `thurbox-cli <args>`, run in-process against `db` and `backends` — the seam
/// the binary itself uses, with no switch a test could flip in it.
fn cli(db: &Database, backends: &thurbox::cli::Backends<'_>, args: &[&str]) -> Result<(), String> {
    let parsed =
        thurbox::cli::Cli::try_parse_from(["thurbox-cli", "--json"].iter().chain(args).copied())
            .map_err(|e| format!("parse {args:?}: {e}"))?;
    match thurbox::cli::run(parsed, db, backends) {
        Ok(thurbox::cli::Outcome::Ok) => Ok(()),
        Ok(thurbox::cli::Outcome::Failed { message, .. }) => Err(message),
        Err(e) => Err(e.message),
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

/// A row as a peer or an earlier build would have written it.
fn seed_row(db: &Database, name: &str, backend_type: &str, pane: &str, cwd: &Path) -> SessionId {
    let id = SessionId::default();
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
    id
}

fn backdate_delete(db: &Database, id: SessionId) {
    db.conn_ref()
        .execute(
            "UPDATE sessions SET deleted_at = 0 WHERE id = ?1",
            [id.to_string()],
        )
        .expect("backdate the delete");
}

/// The one agent window `probe` holds for `id`, asserting there is exactly one.
fn agent_window(probe: &RecordingBackend, id: SessionId, after: &str) -> recording_backend::Window {
    let windows = probe.windows_of(&id.to_string());
    assert_eq!(
        windows.len(),
        1,
        "after {after}, the probe should hold one window for the session: {:?} \
         (every window it holds: {:?})",
        windows,
        probe.windows()
    );
    let window = windows.into_iter().next().unwrap();
    assert_eq!(window.role, WindowRole::Agent, "after {after}");
    assert!(window.alive, "after {after}");
    window
}

fn the_registry() -> (
    thurbox::cli::Backends<'static>,
    Arc<RecordingBackend>,
    Arc<RecordingBackend>,
) {
    let (mut backends, _hosts, _warnings) = thurbox::backend::wiring::configured();
    let local_route = Route::local(Some(Multiplexer::Rmux));
    let local = RecordingBackend::new(&local_route);
    backends.register(local_route, local.clone());
    let remote_route = Route::remote(Via::Ssh, "probehost", Some(Multiplexer::Rmux));
    let remote = RecordingBackend::new(&remote_route);
    backends.register(remote_route, remote.clone());
    (thurbox::cli::Backends::ready(backends), local, remote)
}

#[test]
fn every_lifecycle_verb_reaches_the_backend_the_route_names() {
    if !have_tmux() {
        eprintln!("skipping: tmux is not installed");
        return;
    }
    let instance = Instance::new();
    let db = Database::open_in_memory().expect("db");
    let (backends, probe, far) = the_registry();
    let repo = instance.repo().display().to_string();

    // create
    cli(
        &db,
        &backends,
        &[
            "session",
            "create",
            "--name",
            "probe",
            "--repo-path",
            &repo,
            "--agent",
            "shell",
            "--multiplexer",
            "rmux",
        ],
    )
    .expect("create on the probe route");
    let row = db
        .get_session_by_name("probe")
        .expect("read")
        .expect("the row was written");
    let id = row.id;
    let uuid = id.to_string();
    assert_eq!(
        row.backend_type, "local:rmux",
        "the row names the route asked for"
    );
    instance.assert_tmux_untouched("create");
    let created = agent_window(&probe, id, "create");
    assert!(
        created.pane.starts_with("pane-"),
        "the lifecycle must accept a pane ID outside tmux's % grammar"
    );
    probe
        .send_text(&created.pane, "hello", false)
        .expect("input");
    assert_eq!(
        probe.capture(&created.pane, 1, false).expect("capture"),
        "hello"
    );
    assert_eq!(created.name, "tb-probe");
    assert_eq!(
        row.backend_id, created.pane,
        "the row points at the probe's pane"
    );

    // restart: the old window is replaced, not joined
    cli(&db, &backends, &["session", "restart", &uuid]).expect("restart");
    let restarted = agent_window(&probe, id, "restart");
    assert_ne!(restarted.pane, created.pane, "restart replaces the window");
    assert_eq!(
        db.get_session_by_id(id).unwrap().unwrap().backend_id,
        restarted.pane
    );
    instance.assert_tmux_untouched("restart");

    // stop, then start
    cli(&db, &backends, &["session", "stop", &uuid]).expect("stop");
    assert!(
        probe.windows_of(&uuid).is_empty(),
        "stop leaves the session no window: {:?}",
        probe.windows()
    );
    cli(&db, &backends, &["session", "start", &uuid]).expect("start");
    agent_window(&probe, id, "start");
    instance.assert_tmux_untouched("stop and start");

    // rename: the window follows the row, and keeps its owner
    cli(&db, &backends, &["session", "rename", &uuid, "renamed"]).expect("rename");
    assert_eq!(agent_window(&probe, id, "rename").name, "tb-renamed");
    instance.assert_tmux_untouched("rename");

    // a soft delete keeps the window for the undo, and a restore inside the
    // undo window adopts it rather than launching a second agent
    cli(&db, &backends, &["session", "delete", &uuid, "--soft"]).expect("soft delete");
    let kept = agent_window(&probe, id, "soft delete");
    cli(&db, &backends, &["session", "restore", &uuid]).expect("restore");
    assert_eq!(agent_window(&probe, id, "restore").pane, kept.pane);
    instance.assert_tmux_untouched("delete and restore");

    // the reap sweep lets a soft-deleted session's agent go once the undo
    // window has closed
    cli(&db, &backends, &["session", "delete", &uuid, "--soft"]).expect("soft delete");
    backdate_delete(&db, id);
    let reaped = thurbox::session_ops::reap_overdue_soft_deletes(&db, backends.get());
    assert_eq!(
        reaped,
        vec![uuid.clone()],
        "the sweep reaps the overdue row"
    );
    assert!(
        probe.windows_of(&uuid).is_empty(),
        "the reap released the window: {:?}",
        probe.windows()
    );

    // a restore after the reap launches through the probe again
    cli(&db, &backends, &["session", "restore", &uuid]).expect("restore after reap");
    agent_window(&probe, id, "restore after reap");

    // force delete
    cli(&db, &backends, &["session", "delete", &uuid, "--force"]).expect("force delete");
    assert!(
        probe.windows_of(&uuid).is_empty(),
        "force delete killed the window: {:?}",
        probe.windows()
    );
    assert!(
        db.get_deleted_session_by_id(id)
            .unwrap()
            .is_some_and(|row| row.force_deleted),
        "the row is marked torn down"
    );
    instance.assert_tmux_untouched("reap and force delete");

    // owed teardown: a force delete taken while the host did not answer is
    // written down, and the sweep finishes it through the same backend once the
    // host answers again
    let far_id = SessionId::default();
    let far_pane = far.open("tb-far", &far_id.to_string(), WindowRole::Agent);
    db.upsert_session(&SharedSession {
        id: far_id,
        name: "far".into(),
        agent: "shell".into(),
        backend_id: far_pane,
        backend_type: "ssh:probehost:rmux".into(),
        agent_session_id: Some(uuid::Uuid::new_v4().to_string()),
        cwd: Some(PathBuf::from("/srv/far")),
        additional_dirs: Vec::new(),
        worktrees: Vec::new(),
        shell_backend_id: None,
        parent_session_id: None,
        display_order: None,
        tombstone: false,
        tombstone_at: None,
    })
    .expect("seed the remote row");
    far.set_reachable(false);
    cli(
        &db,
        &backends,
        &["session", "delete", &far_id.to_string(), "--force"],
    )
    .expect("a force delete of an unreachable host's session stands");
    assert_eq!(far.windows_of(&far_id.to_string()).len(), 1);
    assert!(
        db.list_owed_teardowns()
            .unwrap()
            .iter()
            .any(|row| row.id == far_id),
        "the teardown the host missed is owed"
    );
    far.set_reachable(true);
    let finished = thurbox::session_ops::retry_owed_remote_teardowns(&db, backends.get());
    assert_eq!(finished, vec![far_id.to_string()]);
    assert!(
        far.windows_of(&far_id.to_string()).is_empty(),
        "the owed teardown reached the host's backend: {:?}",
        far.windows()
    );
    assert!(db.list_owed_teardowns().unwrap().is_empty());

    instance.assert_tmux_untouched("the owed teardown");
    assert_eq!(
        instance.ssh_log(),
        "",
        "nothing spoke to the host but its backend"
    );
}

/// What a refused verb must leave exactly as it was.
#[derive(Debug, PartialEq, Eq)]
struct RowState {
    name: String,
    backend_id: String,
    deleted: bool,
    stopped: bool,
}

fn row_state(db: &Database, id: SessionId) -> RowState {
    let active = db.get_session_by_id(id).expect("read");
    let deleted = db.get_deleted_session_by_id(id).expect("read deleted");
    let (name, backend_id) = match (&active, &deleted) {
        (Some(row), _) => (row.name.clone(), row.backend_id.clone()),
        (None, Some(row)) => (row.name.clone(), row.backend_id.clone()),
        (None, None) => panic!("row {id} is gone"),
    };
    RowState {
        name,
        backend_id,
        deleted: active.is_none(),
        stopped: db.session_stopped_at(id).expect("stopped").is_some(),
    }
}

#[test]
fn a_route_nothing_serves_is_refused_by_every_lifecycle_verb() {
    if !have_tmux() {
        eprintln!("skipping: tmux is not installed");
        return;
    }
    let instance = Instance::new();
    let db = Database::open_in_memory().expect("db");
    let backends = thurbox::cli::Backends::ready(thurbox::backend::wiring::configured().0);
    let repo = instance.repo().display().to_string();

    // create
    let refused = cli(
        &db,
        &backends,
        &[
            "session",
            "create",
            "--name",
            "nothing",
            "--repo-path",
            &repo,
            "--agent",
            "shell",
            "--multiplexer",
            "herdr",
        ],
    );
    assert!(
        refused.is_err(),
        "a create on a route nothing serves was taken"
    );
    assert!(
        db.list_active_sessions().unwrap().is_empty(),
        "no row was written"
    );
    instance.assert_tmux_untouched("the refused create");

    // A key naming a multiplexer nothing is registered for, and one naming no
    // multiplexer at all.
    for key in ["local:herdr", "local-probe"] {
        let id = seed_row(&db, "stranded", key, "%7", instance.repo());
        let uuid = id.to_string();
        let before = row_state(&db, id);

        for verb in [
            vec!["session", "restart", &uuid],
            vec!["session", "stop", &uuid],
            vec!["session", "rename", &uuid, "moved"],
            vec!["session", "delete", &uuid, "--force"],
        ] {
            let outcome = cli(&db, &backends, &verb);
            assert!(outcome.is_err(), "{key}: `{}` was taken", verb.join(" "));
            assert_eq!(
                row_state(&db, id),
                before,
                "{key}: `{}` changed the row",
                verb.join(" ")
            );
            instance.assert_tmux_untouched(&format!("{key}: {}", verb.join(" ")));
        }

        // start clears a stop only once it can put the window back
        db.set_session_stopped(id, true).expect("park");
        let parked = row_state(&db, id);
        assert!(
            cli(&db, &backends, &["session", "start", &uuid]).is_err(),
            "{key}: start was taken"
        );
        assert_eq!(row_state(&db, id), parked, "{key}: start changed the row");
        db.set_session_stopped(id, false).expect("unpark");
        instance.assert_tmux_untouched(&format!("{key}: start"));

        // A soft delete touches no window and stays allowed; what comes after
        // it must not open or close one.
        cli(&db, &backends, &["session", "delete", &uuid, "--soft"]).expect("soft delete");
        let deleted = row_state(&db, id);
        assert!(deleted.deleted);
        assert!(
            cli(&db, &backends, &["session", "restore", &uuid]).is_err(),
            "{key}: restore was taken"
        );
        assert_eq!(
            row_state(&db, id),
            deleted,
            "{key}: restore changed the row"
        );
        assert!(
            cli(&db, &backends, &["session", "reap", &uuid]).is_err(),
            "{key}: reap was taken"
        );
        backdate_delete(&db, id);
        let reaped = thurbox::session_ops::reap_overdue_soft_deletes(&db, backends.get());
        assert!(reaped.is_empty(), "{key}: the sweep reaped {reaped:?}");
        assert!(row_state(&db, id).deleted);
        instance.assert_tmux_untouched(&format!("{key}: restore and reap"));
    }

    // An owed teardown on a remote route nothing serves stays owed.
    let far = seed_row(
        &db,
        "far",
        "ssh:probehost:herdr",
        "%3",
        Path::new("/srv/far"),
    );
    cli(
        &db,
        &backends,
        &["session", "delete", &far.to_string(), "--force"],
    )
    .expect("a remote force delete stands and owes its teardown");
    assert!(db
        .list_owed_teardowns()
        .unwrap()
        .iter()
        .any(|r| r.id == far));
    let finished = thurbox::session_ops::retry_owed_remote_teardowns(&db, backends.get());
    assert!(
        finished.is_empty(),
        "an undrivable teardown was marked done"
    );
    assert!(db
        .list_owed_teardowns()
        .unwrap()
        .iter()
        .any(|r| r.id == far));
    instance.assert_tmux_untouched("the owed teardown");
    assert_eq!(
        instance.ssh_log(),
        "",
        "nothing reached for the host over ssh"
    );
}
