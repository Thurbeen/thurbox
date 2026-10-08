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
}

impl Env {
    fn new() -> Self {
        let root = tempfile::TempDir::new().expect("tempdir");
        for sub in ["home", "config", "data", "tmux"] {
            std::fs::create_dir_all(root.path().join(sub)).expect("mkdir");
        }
        Self { root }
    }

    fn path(&self, sub: &str) -> PathBuf {
        self.root.path().join(sub)
    }

    fn db(&self) -> Database {
        Database::open(&self.path("data").join("thurbox.db")).expect("open the instance database")
    }

    fn tmux(&self, args: &[&str]) -> std::process::Output {
        Command::new("tmux")
            .env("TMUX_TMPDIR", self.path("tmux"))
            .args(["-L", SOCKET])
            .args(args)
            .output()
            .expect("run tmux")
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
        cmd.env("HOME", self.path("home"));
        cmd.env("XDG_DATA_HOME", self.path("home").join("xdg-data"));
        cmd.env("XDG_CONFIG_HOME", self.path("home").join("xdg-config"));
        cmd.env("THURBOX_CONFIG_DIR", self.path("config"));
        cmd.env("THURBOX_DATA_DIR", self.path("data"));
        cmd.env("THURBOX_SOCKET", SOCKET);
        cmd.env("TMUX_TMPDIR", self.path("tmux"));
        cmd.env_remove("TMUX");
        cmd.env_remove("THURBOX_SOCKET_FOR");
        cmd.env_remove("THURBOX_SESSION");
        cmd.env_remove("THURBOX_SESSION_ID");
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
}

impl Drop for Env {
    fn drop(&mut self) {
        let _ = self.tmux(&["kill-server"]);
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
