//! A private session bus with a stand-in notification daemon, for the tests
//! that assert what thurbox delivers to the desktop. Nothing here reaches the
//! user's own bus: the daemon listens on a socket inside the test's directory
//! and the receiver records every `Notify` call instead of showing it.
//!
//! Needs `dbus-daemon` on `PATH`, as the PTY tests need `tmux`.
#![allow(dead_code)]

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use zbus::OwnedVariant;

/// One notification the receiver took.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Shown {
    pub app_name: String,
    pub summary: String,
    pub body: String,
}

struct Receiver {
    shown: Arc<Mutex<Vec<Shown>>>,
}

#[zbus::interface(name = "org.freedesktop.Notifications")]
impl Receiver {
    #[allow(clippy::too_many_arguments)]
    fn notify(
        &self,
        app_name: String,
        _replaces_id: u32,
        _app_icon: String,
        summary: String,
        body: String,
        _actions: Vec<String>,
        _hints: HashMap<String, OwnedVariant>,
        _expire_timeout: i32,
    ) -> u32 {
        let mut shown = self.shown.lock().unwrap();
        shown.push(Shown {
            app_name,
            summary,
            body,
        });
        shown.len() as u32
    }

    fn close_notification(&self, _id: u32) {}

    fn get_capabilities(&self) -> Vec<String> {
        vec!["body".into(), "actions".into()]
    }

    fn get_server_information(&self) -> (String, String, String, String) {
        (
            "fixture".into(),
            "thurbox-tests".into(),
            "0".into(),
            "1.2".into(),
        )
    }
}

/// The private `dbus-daemon`, killed when dropped — from the moment it is
/// spawned, so a setup step that panics after it does not leave it running.
struct Daemon(Child);

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

pub struct NotificationBus {
    socket: PathBuf,
    shown: Arc<Mutex<Vec<Shown>>>,
    _connection: zbus::Connection,
    // Tokio must keep polling the receiver after setup returns to the test.
    _runtime: tokio::runtime::Runtime,
    // Last: fields drop in order, so the connection closes before the bus.
    _daemon: Daemon,
}

impl NotificationBus {
    /// Start a bus listening on `<dir>/bus` — the path `XDG_RUNTIME_DIR/bus`
    /// names when `dir` is the runtime directory — and claim the notification
    /// service on it.
    pub fn start(dir: &Path) -> Self {
        let socket = dir.join("bus");
        let daemon = Daemon(
            Command::new("dbus-daemon")
                .args(["--session", "--nofork", "--nopidfile"])
                .arg(format!("--address=unix:path={}", socket.display()))
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .expect("dbus-daemon on PATH"),
        );
        let deadline = Instant::now() + Duration::from_secs(10);
        while !socket.exists() {
            assert!(Instant::now() < deadline, "the private bus never listened");
            std::thread::sleep(Duration::from_millis(20));
        }
        let shown = Arc::new(Mutex::new(Vec::new()));
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .expect("notification receiver runtime");
        let connection = runtime
            .block_on(
                zbus::connection::Builder::address(
                    format!("unix:path={}", socket.display()).as_str(),
                )
                .name("org.freedesktop.Notifications")
                .serve_at(
                    "/org/freedesktop/Notifications",
                    Receiver {
                        shown: shown.clone(),
                    },
                )
                .build(),
            )
            .expect("connect the receiver");
        Self {
            socket,
            shown,
            _connection: connection,
            _runtime: runtime,
            _daemon: daemon,
        }
    }

    /// The value `DBUS_SESSION_BUS_ADDRESS` takes to reach this bus.
    pub fn address(&self) -> String {
        format!("unix:path={}", self.socket.display())
    }

    /// Every notification received so far, in order.
    pub fn shown(&self) -> Vec<Shown> {
        self.shown.lock().unwrap().clone()
    }
}
