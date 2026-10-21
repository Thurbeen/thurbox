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

use zbus::zvariant::OwnedValue;

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
        _hints: HashMap<String, OwnedValue>,
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

pub struct NotificationBus {
    daemon: Child,
    socket: PathBuf,
    shown: Arc<Mutex<Vec<Shown>>>,
    _connection: zbus::blocking::Connection,
}

impl NotificationBus {
    /// Start a bus listening on `<dir>/bus` — the path `XDG_RUNTIME_DIR/bus`
    /// names when `dir` is the runtime directory — and claim the notification
    /// service on it.
    pub fn start(dir: &Path) -> Self {
        let socket = dir.join("bus");
        let daemon = Command::new("dbus-daemon")
            .args(["--session", "--nofork", "--nopidfile"])
            .arg(format!("--address=unix:path={}", socket.display()))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("dbus-daemon on PATH");
        let deadline = Instant::now() + Duration::from_secs(10);
        while !socket.exists() {
            assert!(Instant::now() < deadline, "the private bus never listened");
            std::thread::sleep(Duration::from_millis(20));
        }
        let shown = Arc::new(Mutex::new(Vec::new()));
        let connection = zbus::blocking::connection::Builder::address(
            format!("unix:path={}", socket.display()).as_str(),
        )
        .expect("bus address")
        .name("org.freedesktop.Notifications")
        .expect("service name")
        .serve_at(
            "/org/freedesktop/Notifications",
            Receiver {
                shown: shown.clone(),
            },
        )
        .expect("serve the receiver")
        .build()
        .expect("connect the receiver");
        Self {
            daemon,
            socket,
            shown,
            _connection: connection,
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

impl Drop for NotificationBus {
    fn drop(&mut self) {
        let _ = self.daemon.kill();
        let _ = self.daemon.wait();
    }
}
