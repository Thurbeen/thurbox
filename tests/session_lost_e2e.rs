//! A session whose multiplexer died under it is reported as `lost`.
//!
//! A tmux segfault takes every pane on the server with it, and before this
//! nothing in the event stream said so: the rows stood, their last hook state
//! still read `working`, and a driver tailing `watch` waited on workers that no
//! longer existed. These tests crash a real tmux server and read what the real
//! `thurbox-cli watch` makes of it.
//!
//! Skipped when tmux is absent, and scoped to a throwaway socket in a private
//! directory, so it can never touch a real session.

use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use serde_json::Value;
use thurbox::session::SessionId;
use thurbox::storage::Database;
use thurbox::sync::SharedSession;

/// The guard every tmux server in this file is reaped by — see its own doc.
#[path = "support/tmux_server.rs"]
mod tmux_server;

use tmux_server::TmuxServer;

const SOCKET: &str = "thurbox-lost-e2e";

/// Mirrors `agent::tmux::TMUX_SESSION` for a dev build, which a test binary is.
const THURBOX_TMUX_SESSION: &str = "thurbox-dev";

/// Longer than the watcher needs to confirm a loss, so a silence this long is
/// a real "nothing to report".
const QUIET: Duration = Duration::from_secs(8);

/// How long a line that should be on its way may take.
const WAIT: Duration = Duration::from_secs(20);

fn have_tmux() -> bool {
    Command::new("tmux")
        .arg("-V")
        .output()
        .is_ok_and(|o| o.status.success())
}

struct Env {
    root: tempfile::TempDir,
    /// The private server the sessions live on, and the one `watch` lists.
    server: TmuxServer,
}

impl Env {
    fn new() -> Self {
        let root = tempfile::TempDir::new().expect("tempdir");
        for sub in ["home", "config", "data"] {
            std::fs::create_dir_all(root.path().join(sub)).expect("mkdir");
        }
        Self {
            root,
            server: TmuxServer::private(SOCKET),
        }
    }

    fn path(&self, sub: &str) -> PathBuf {
        self.root.path().join(sub)
    }

    fn db(&self) -> Database {
        Database::open(&self.path("data").join("thurbox.db")).expect("open the instance database")
    }

    fn tmux(&self, args: &[&str]) -> std::process::Output {
        self.server.tmux(args)
    }

    /// A window on the private server, named the way thurbox names a session's
    /// agent window. Returns its pane id.
    fn window(&self, session_name: &str) -> String {
        let window = format!("tb-{session_name}");
        let out = if self
            .tmux(&["has-session", "-t", THURBOX_TMUX_SESSION])
            .status
            .success()
        {
            self.tmux(&[
                "new-window",
                "-d",
                "-t",
                THURBOX_TMUX_SESSION,
                "-n",
                &window,
                "-P",
                "-F",
                "#{pane_id}",
                "cat",
            ])
        } else {
            self.tmux(&[
                "new-session",
                "-d",
                "-s",
                THURBOX_TMUX_SESSION,
                "-n",
                &window,
                "-P",
                "-F",
                "#{pane_id}",
                "cat",
            ])
        };
        assert!(out.status.success(), "tmux would not start a window");
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    /// What a segfault does: the server goes, and its socket file stays behind.
    fn crash_server(&self) {
        let out = self.tmux(&["display-message", "-p", "#{pid}"]);
        let pid = String::from_utf8_lossy(&out.stdout).trim().to_string();
        assert!(!pid.is_empty(), "no server pid");
        let killed = Command::new("kill")
            .args(["-SEGV", &pid])
            .status()
            .expect("kill");
        assert!(killed.success());
        while self.tmux(&["has-session"]).status.success() {
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    fn watch(&self) -> Watch {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_thurbox-cli"));
        cmd.args(["watch", "--json", "--for-secs", "120"]);
        self.spawn_watch(cmd)
    }

    #[cfg(unix)]
    fn cli(&self, args: &[&str]) -> std::process::Output {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_thurbox-cli"));
        cmd.args(args).arg("--json");
        self.scope(&mut cmd);
        cmd.output().expect("run thurbox-cli")
    }

    fn scope(&self, cmd: &mut Command) {
        cmd.env("HOME", self.path("home"));
        cmd.env("XDG_DATA_HOME", self.path("home").join("xdg-data"));
        cmd.env("XDG_CONFIG_HOME", self.path("home").join("xdg-config"));
        cmd.env("THURBOX_CONFIG_DIR", self.path("config"));
        cmd.env("THURBOX_DATA_DIR", self.path("data"));
        self.server.scope(cmd);
        cmd.env_remove("TMUX");
        cmd.env_remove("THURBOX_SOCKET_FOR");
        cmd.env_remove("THURBOX_SESSION");
        cmd.env_remove("THURBOX_SESSION_ID");
    }

    fn spawn_watch(&self, mut cmd: Command) -> Watch {
        self.scope(&mut cmd);
        let mut child = cmd
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn watch");
        let stdout = child.stdout.take().expect("piped stdout");
        let (tx, lines) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { return };
                if tx.send(line).is_err() {
                    return;
                }
            }
        });
        Watch { child, lines }
    }

    #[cfg(unix)]
    fn watch_paused_before_discovery(&self) -> (Watch, DiscoveryGate) {
        use std::os::unix::fs::PermissionsExt;

        let real_tmux = Command::new("sh")
            .args(["-c", "command -v tmux"])
            .output()
            .expect("locate tmux");
        assert!(real_tmux.status.success(), "tmux is not on PATH");
        let real_tmux = String::from_utf8(real_tmux.stdout)
            .expect("tmux path is utf-8")
            .trim()
            .to_string();
        let shim = self.path("tmux-shim");
        std::fs::create_dir_all(&shim).expect("create shim directory");
        let script = shim.join("tmux");
        std::fs::write(
            &script,
            "#!/bin/sh\n\
             case \" $* \" in\n\
               *\" list-windows \"*)\n\
                 \"$THURBOX_TEST_REAL_TMUX\" \"$@\" > \"$THURBOX_TEST_DISCOVERY_OUTPUT\"\n\
                 status=$?\n\
                 : > \"$THURBOX_TEST_DISCOVERY_READY\"\n\
                 while [ ! -e \"$THURBOX_TEST_DISCOVERY_CONTINUE\" ]; do sleep 0.01; done\n\
                 cat \"$THURBOX_TEST_DISCOVERY_OUTPUT\"\n\
                 exit \"$status\"\n\
                 ;;\n\
             esac\n\
             exec \"$THURBOX_TEST_REAL_TMUX\" \"$@\"\n",
        )
        .expect("write tmux shim");
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755))
            .expect("make tmux shim executable");

        let ready = self.path("discovery-ready");
        let resume = self.path("discovery-continue");
        let output = self.path("discovery-output");
        let path = std::env::join_paths(std::iter::once(shim).chain(std::env::split_paths(
            &std::env::var_os("PATH").unwrap_or_default(),
        )))
        .expect("build PATH");
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_thurbox-cli"));
        cmd.args(["watch", "--json", "--for-secs", "120"])
            .env("PATH", path)
            .env("THURBOX_TEST_REAL_TMUX", real_tmux)
            .env("THURBOX_TEST_DISCOVERY_READY", &ready)
            .env("THURBOX_TEST_DISCOVERY_CONTINUE", &resume)
            .env("THURBOX_TEST_DISCOVERY_OUTPUT", output);
        let watch = self.spawn_watch(cmd);
        // The shim captures tmux's real answer before it signals. Holding that
        // answer back freezes the watcher after observation but before recovery.
        let deadline = std::time::Instant::now() + WAIT;
        while !ready.exists() && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(ready.exists(), "watcher did not reach tmux discovery");
        (watch, DiscoveryGate { resume })
    }
}

#[cfg(unix)]
struct DiscoveryGate {
    resume: PathBuf,
}

#[cfg(unix)]
impl DiscoveryGate {
    fn resume(self) {
        std::fs::write(self.resume, "").expect("resume tmux discovery");
    }
}

struct Watch {
    child: Child,
    lines: mpsc::Receiver<String>,
}

impl Watch {
    fn event(&self, what: &str) -> Value {
        let line = self
            .lines
            .recv_timeout(WAIT)
            .unwrap_or_else(|_| panic!("watch produced no line while waiting for {what}"));
        serde_json::from_str(&line).expect("one JSON object per line")
    }

    fn silent_for(&self, grace: Duration, why: &str) {
        if let Ok(line) = self.lines.recv_timeout(grace) {
            panic!("{why}, yet watch emitted: {line}");
        }
    }
}

impl Drop for Watch {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn row(name: &str, pane: String) -> SharedSession {
    SharedSession {
        id: SessionId::default(),
        name: name.into(),
        agent: "cat".into(),
        backend_id: pane,
        backend_type: "local-tmux".into(),
        agent_session_id: None,
        cwd: None,
        additional_dirs: Vec::new(),
        worktrees: Vec::new(),
        shell_backend_id: None,
        parent_session_id: None,
        display_order: None,
        tombstone: false,
        tombstone_at: None,
    }
}

#[test]
fn a_crashed_backend_reports_each_running_session_lost_once() {
    if !have_tmux() {
        eprintln!("skipping: tmux is not installed");
        return;
    }
    let env = Env::new();
    let db = env.db();
    let worker = row("worker", env.window("worker"));
    db.upsert_session(&worker).expect("persist worker");
    db.set_hook_state(worker.id, "working").expect("working");
    // Parked: its pane is gone on purpose, so its loss is no news.
    let parked = row("parked", env.window("parked"));
    db.upsert_session(&parked).expect("persist parked");
    db.set_session_stopped(parked.id, true).expect("park it");

    let watch = env.watch();
    watch.silent_for(QUIET, "every running session still has its pane");

    env.crash_server();

    let lost = watch.event("the worker's loss");
    assert_eq!(lost["event"], "changed");
    assert_eq!(lost["reason"], "lost");
    assert_eq!(lost["session"], worker.id.to_string());
    assert_eq!(lost["name"], "worker");
    watch.silent_for(
        QUIET,
        "a loss is reported once, and a parked session is not lost",
    );
    drop(watch);

    // The next watcher — a driver runs them back to back — has nothing to add.
    let again = env.watch();
    again.silent_for(QUIET, "the loss was already in the log");
}

/// A fresh tmux server numbers its panes from `%0` again, so a worker brought
/// back after a crash can sit on the very pane id it had. Its next death is
/// still news.
#[test]
fn a_session_back_on_the_same_pane_is_reported_when_it_dies_again() {
    if !have_tmux() {
        eprintln!("skipping: tmux is not installed");
        return;
    }
    let env = Env::new();
    let db = env.db();
    let worker = row("worker", env.window("worker"));
    db.upsert_session(&worker).expect("persist worker");

    let watch = env.watch();
    env.crash_server();
    assert_eq!(watch.event("the first loss")["reason"], "lost");

    // The agent comes back by hand on a new server, on the same pane id.
    assert_eq!(
        env.window("worker"),
        worker.backend_id,
        "tmux reused the pane id"
    );
    watch.silent_for(QUIET, "the window is back");

    env.crash_server();
    let again = watch.event("the second loss");
    assert_eq!(again["reason"], "lost");
    assert_eq!(again["session"], worker.id.to_string());
}

#[test]
fn a_lost_session_does_not_recover_onto_a_live_namesakes_unstamped_pane() {
    if !have_tmux() {
        eprintln!("skipping: tmux is not installed");
        return;
    }
    let env = Env::new();
    let db = env.db();
    let live = row("duplicate", env.window("duplicate"));
    db.upsert_session(&live).expect("persist live namesake");
    let lost = row("duplicate", "%999".into());
    db.upsert_session(&lost).expect("persist lost namesake");
    assert!(db.record_session_lost(lost.id, "%999").unwrap());

    let watch = env.watch();
    watch.silent_for(QUIET, "recovery does not emit an event");
    drop(watch);

    assert!(
        db.load_lost_sessions().unwrap().contains_key(&lost.id),
        "the live namesake's unstamped window must not recover the lost row"
    );
    assert_eq!(
        db.get_session_by_id(lost.id).unwrap().unwrap().backend_id,
        "",
        "the lost row must not claim the live row's pane"
    );
    assert_eq!(
        db.get_session_by_id(live.id).unwrap().unwrap().backend_id,
        live.backend_id,
        "the live row keeps its pane"
    );
}

#[test]
fn two_lost_namesakes_do_not_both_recover_onto_one_unstamped_pane() {
    if !have_tmux() {
        eprintln!("skipping: tmux is not installed");
        return;
    }
    let env = Env::new();
    let db = env.db();
    let pane = env.window("duplicate");
    let first = row("duplicate", "%998".into());
    db.upsert_session(&first).expect("persist first namesake");
    assert!(db.record_session_lost(first.id, "%998").unwrap());
    let second = row("duplicate", "%999".into());
    db.upsert_session(&second).expect("persist second namesake");
    assert!(db.record_session_lost(second.id, "%999").unwrap());

    let watch = env.watch();
    watch.silent_for(QUIET, "recovery does not emit an event");
    drop(watch);

    let rows = [first.id, second.id].map(|id| db.get_session_by_id(id).unwrap().unwrap());
    assert_eq!(
        rows.iter().filter(|row| row.backend_id == pane).count(),
        1,
        "one unstamped pane can recover only one lost namesake: {rows:#?}"
    );
    let lost = db.load_lost_sessions().unwrap();
    assert_eq!(
        [first.id, second.id]
            .iter()
            .filter(|id| lost.contains_key(id))
            .count(),
        1,
        "the other namesake must remain lost"
    );
}

#[test]
#[cfg(unix)]
fn recovery_does_not_overwrite_a_pane_claimed_by_a_concurrent_restart() {
    if !have_tmux() {
        eprintln!("skipping: tmux is not installed");
        return;
    }
    let env = Env::new();
    let db = env.db();
    let lost = row("duplicate", "%998".into());
    db.upsert_session(&lost).expect("persist lost namesake");
    assert!(db.record_session_lost(lost.id, "%998").unwrap());

    let mut restarting = row("worker", "%999".into());
    restarting.agent_session_id = Some(SessionId::default().to_string());
    db.upsert_session(&restarting)
        .expect("persist restarting namesake");
    db.set_launch_recipe(
        restarting.id,
        &thurbox::session::LaunchRecipe {
            command: "cat".into(),
            args: Vec::new(),
            env: Default::default(),
        },
    )
    .expect("persist restart recipe");
    let _server_anchor = env.window("unrelated");
    let stale_pane = env.window("duplicate");

    let (watch, discovery) = env.watch_paused_before_discovery();
    env.crash_server();
    let restarted = env.cli(&["session", "restart", &restarting.id.to_string()]);
    assert!(
        restarted.status.success(),
        "restart failed: {}",
        String::from_utf8_lossy(&restarted.stderr)
    );
    let pane = db
        .get_session_by_id(restarting.id)
        .unwrap()
        .unwrap()
        .backend_id;
    assert_eq!(
        pane, stale_pane,
        "the fresh server must reuse the pane in recovery's stale listing"
    );

    discovery.resume();
    let restarted_event = watch.event("the concurrent restart");
    assert_eq!(restarted_event["session"], restarting.id.to_string());
    assert_eq!(restarted_event["reason"], "updated");
    drop(watch);

    assert!(
        db.load_lost_sessions().unwrap().contains_key(&lost.id),
        "the lost row must not recover onto the concurrently restarted row's pane"
    );
    assert_eq!(
        db.get_session_by_id(lost.id).unwrap().unwrap().backend_id,
        "",
        "the restart's pane belongs only to the row that launched it"
    );
    assert_eq!(
        db.get_session_by_id(restarting.id)
            .unwrap()
            .unwrap()
            .backend_id,
        pane,
        "the restarted row keeps its pane"
    );
}

#[test]
#[cfg(unix)]
fn a_concurrent_restart_does_not_overwrite_a_pane_claimed_by_recovery() {
    if !have_tmux() {
        eprintln!("skipping: tmux is not installed");
        return;
    }
    let env = Env::new();
    let db = env.db();
    let lost = row("duplicate", "%998".into());
    db.upsert_session(&lost).expect("persist lost namesake");
    assert!(db.record_session_lost(lost.id, "%998").unwrap());

    let mut restarting = row("worker", "%999".into());
    restarting.agent_session_id = Some(SessionId::default().to_string());
    db.upsert_session(&restarting)
        .expect("persist restarting namesake");
    db.set_launch_recipe(
        restarting.id,
        &thurbox::session::LaunchRecipe {
            command: "cat".into(),
            args: Vec::new(),
            env: Default::default(),
        },
    )
    .expect("persist restart recipe");
    let _server_anchor = env.window("unrelated");
    let stale_pane = env.window("duplicate");

    let (watch, discovery) = env.watch_paused_before_discovery();
    env.crash_server();
    discovery.resume();

    let deadline = std::time::Instant::now() + WAIT;
    while db.load_lost_sessions().unwrap().contains_key(&lost.id)
        && std::time::Instant::now() < deadline
    {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(
        !db.load_lost_sessions().unwrap().contains_key(&lost.id),
        "recovery did not claim the stale pane before restart"
    );
    assert_eq!(
        db.get_session_by_id(lost.id).unwrap().unwrap().backend_id,
        stale_pane,
        "recovery must first record the pane from its stale listing"
    );

    let restarted = env.cli(&["session", "restart", &restarting.id.to_string()]);
    assert!(
        !restarted.status.success(),
        "restart must refuse to duplicate recovery's pane claim"
    );
    let refusal: Value = serde_json::from_slice(&restarted.stdout).expect("restart refusal JSON");
    assert!(
        refusal["error"]
            .as_str()
            .is_some_and(|error| error.contains("another live session already claims it")),
        "unexpected restart refusal: {refusal}"
    );
    let windows = env.tmux(&[
        "list-windows",
        "-t",
        THURBOX_TMUX_SESSION,
        "-F",
        "#{window_name} #{pane_id}",
    ]);
    assert!(windows.status.success(), "list restarted windows");
    let windows = String::from_utf8(windows.stdout).expect("tmux listing is utf-8");
    assert!(
        windows
            .lines()
            .any(|line| line == format!("tb-worker {stale_pane}")),
        "the real restart must have created the reused pane: {windows}"
    );
    let restarted_pane = db
        .get_session_by_id(restarting.id)
        .unwrap()
        .unwrap()
        .backend_id;
    assert_eq!(
        restarting.backend_id, restarted_pane,
        "a refused claim must not overwrite the restarting row either"
    );
    drop(watch);

    let rows = [lost.id, restarting.id].map(|id| db.get_session_by_id(id).unwrap().unwrap());
    assert_eq!(
        rows.iter()
            .filter(|row| row.backend_id == stale_pane)
            .count(),
        1,
        "recovery and restart must not both claim one pane: {rows:#?}"
    );
}
