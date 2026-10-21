//! `notify send` and the failing-run alert built on it (issues #1519, #1508),
//! driven through the real binary in an isolated instance.
//!
//! Delivery is asserted against a private bus with a stand-in notification
//! daemon (`support/notification_bus.rs`); every command here points the bus
//! at that socket or at one that does not exist, so nothing reaches the
//! user's desktop.

use std::path::PathBuf;
use std::process::{Command, Output};

use serde_json::Value;

#[cfg(target_os = "linux")]
#[path = "support/notification_bus.rs"]
mod notification_bus;

#[cfg(target_os = "linux")]
use notification_bus::NotificationBus;

const NO_BUS: &str = "unix:path=/nonexistent-notification-bus";

struct Instance {
    root: tempfile::TempDir,
}

impl Instance {
    /// An instance with `features` added to `[features]` and `notifications`
    /// as `[notifications]`.
    fn new(features: &str, notifications: &str) -> Self {
        let root = tempfile::TempDir::new().expect("isolated instance");
        for sub in ["home", "config", "data", "tmux", "runtime"] {
            std::fs::create_dir_all(root.path().join(sub)).expect("instance directory");
        }
        std::fs::write(
            root.path().join("config/settings.toml"),
            format!(
                "[features]\nautomations = false\nversion_check = false\nauto_update = false\n\
                 {features}\n[notifications]\n{notifications}\n"
            ),
        )
        .expect("notification settings");
        Self { root }
    }

    /// An instance whose notifications go to the configured `backend`.
    fn with_backend(backend: &str, extra: &str) -> Self {
        Self::new(
            "",
            &format!("backend = '{backend}'\nsound = false\n{extra}"),
        )
    }

    fn path(&self, sub: &str) -> PathBuf {
        self.root.path().join(sub)
    }

    /// `thurbox-cli --json <args>` against this instance, its bus at `bus`
    /// (`None`: no bus address at all, only `XDG_RUNTIME_DIR`).
    fn command(&self, bus: Option<&str>, args: &[&str]) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_thurbox-cli"));
        cmd.arg("--json").args(args);
        cmd.env("HOME", self.path("home"))
            .env("USERPROFILE", self.path("home"))
            .env("THURBOX_CONFIG_DIR", self.path("config"))
            .env("THURBOX_DATA_DIR", self.path("data"))
            .env("TMUX_TMPDIR", self.path("tmux"))
            .env("XDG_RUNTIME_DIR", self.path("runtime"));
        match bus {
            Some(address) => cmd.env("DBUS_SESSION_BUS_ADDRESS", address),
            None => cmd.env_remove("DBUS_SESSION_BUS_ADDRESS"),
        };
        for var in [
            "THURBOX_SOCKET",
            "THURBOX_SOCKET_FOR",
            "THURBOX_SESSION",
            "THURBOX_SESSION_ID",
            "THURBOX_AUTOMATION_ID",
            "THURBOX_AUTOMATION_NAME",
        ] {
            cmd.env_remove(var);
        }
        cmd
    }

    fn run(&self, bus: Option<&str>, args: &[&str]) -> Output {
        self.command(bus, args).output().expect("run thurbox-cli")
    }

    /// Run `args`, assert it succeeded, and return its one JSON document.
    fn ok(&self, bus: Option<&str>, args: &[&str]) -> Value {
        let out = self.run(bus, args);
        assert!(out.status.success(), "{args:?} failed:\n{}", describe(&out));
        // from_slice rejects trailing data, including a second JSON document.
        serde_json::from_slice(&out.stdout).expect("one JSON result")
    }

    fn send(&self, bus: Option<&str>, extra: &[&str]) -> Value {
        self.ok(bus, &send_args(extra))
    }

    fn db(&self) -> thurbox::storage::Database {
        thurbox::storage::Database::open(&self.path("data").join("thurbox.db"))
            .expect("open the instance database")
    }
}

fn send_args<'a>(extra: &[&'a str]) -> Vec<&'a str> {
    let mut args = vec![
        "notify",
        "send",
        "--title",
        "Build failed",
        "--body",
        "Inspect the run log",
    ];
    args.extend_from_slice(extra);
    args
}

fn describe(out: &Output) -> String {
    format!(
        "status: {}\nstdout: {}\nstderr: {}",
        out.status,
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

fn assert_disabled(value: &Value) {
    assert_eq!(value["suppressed"], "disabled", "{value}");
    assert_eq!(value["delivered"], false, "{value}");
    assert!(value["error"].is_null(), "{value}");
}

const DEDUPED: &[&str] = &["--key", "build-failure", "--min-interval", "60"];

#[test]
fn notify_send_honours_the_notifications_feature_switch() {
    let instance = Instance::new("notifications = false", "backend = 'dbus'");
    // An off switch short-circuits before the bus, the dedupe key or a
    // session is looked at.
    assert_disabled(&instance.send(Some(NO_BUS), DEDUPED));
    assert_disabled(&instance.send(Some(NO_BUS), DEDUPED));
}

#[test]
fn notify_send_honours_backend_off() {
    let instance = Instance::with_backend("off", "");
    assert_disabled(&instance.send(Some(NO_BUS), DEDUPED));
}

/// A dedupe key needs its interval and the other way round: either alone is a
/// usage error, not a silently undeduplicated send.
#[test]
fn a_key_and_its_interval_come_together() {
    let instance = Instance::with_backend("off", "");
    for extra in [&["--key", "k"][..], &["--min-interval", "60"][..]] {
        let out = instance.run(Some(NO_BUS), &send_args(extra));
        assert_eq!(out.status.code(), Some(2), "{extra:?}:\n{}", describe(&out));
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            stdout.contains("required arguments were not provided"),
            "{extra:?} is refused for its missing half:\n{}",
            describe(&out)
        );
    }
}

#[cfg(target_os = "linux")]
#[test]
fn notify_send_delivers_through_the_configured_backend() {
    let instance = Instance::with_backend("dbus", "");
    let bus = NotificationBus::start(&instance.path("runtime"));

    let value = instance.send(Some(&bus.address()), &[]);
    assert_eq!(value["delivered"], true, "{value}");
    assert!(value["suppressed"].is_null(), "{value}");
    assert!(value["error"].is_null(), "{value}");

    let shown = bus.shown();
    assert_eq!(shown.len(), 1, "{shown:?}");
    assert_eq!(shown[0].app_name, "thurbox");
    assert_eq!(shown[0].summary, "Build failed");
    assert_eq!(shown[0].body, "Inspect the run log");
}

/// An Exec automation, a hook or a heartbeat may run without
/// `DBUS_SESSION_BUS_ADDRESS`; the per-user bus at `XDG_RUNTIME_DIR/bus` is
/// still found.
#[cfg(target_os = "linux")]
#[test]
fn notify_send_finds_the_runtime_bus_without_a_bus_address() {
    let instance = Instance::with_backend("auto", "");
    let bus = NotificationBus::start(&instance.path("runtime"));

    let value = instance.send(None, &[]);
    assert_eq!(value["delivered"], true, "{value}");
    assert_eq!(bus.shown().len(), 1);
}

/// The same key inside its interval is dropped, between separate processes.
#[cfg(target_os = "linux")]
#[test]
fn notify_send_dedupes_a_key_across_processes() {
    let instance = Instance::with_backend("dbus", "");
    let bus = NotificationBus::start(&instance.path("runtime"));
    let address = bus.address();

    assert_eq!(instance.send(Some(&address), DEDUPED)["delivered"], true);
    let repeat = instance.send(Some(&address), DEDUPED);
    assert_eq!(repeat["suppressed"], "deduped", "{repeat}");
    assert_eq!(repeat["delivered"], false, "{repeat}");
    let other = instance.send(Some(&address), &["--key", "other", "--min-interval", "60"]);
    assert_eq!(other["delivered"], true, "a key of its own: {other}");
    assert_eq!(bus.shown().len(), 2, "{:?}", bus.shown());

    // An interval that has passed lets the key through again.
    let short = ["--key", "short", "--min-interval", "1"];
    assert_eq!(instance.send(Some(&address), &short)["delivered"], true);
    std::thread::sleep(std::time::Duration::from_millis(1_100));
    assert_eq!(instance.send(Some(&address), &short)["delivered"], true);
    assert_eq!(bus.shown().len(), 4);
}

/// A delivery that fails says so, exits non-zero, and does not spend its key:
/// the retry is delivered rather than deduped against a notification nobody
/// saw.
#[cfg(target_os = "linux")]
#[test]
fn a_failed_delivery_reports_its_error_and_keeps_the_key() {
    let instance = Instance::with_backend("dbus", "");

    let out = instance.run(Some(NO_BUS), &send_args(DEDUPED));
    assert!(!out.status.success(), "{}", describe(&out));
    let value: Value = serde_json::from_slice(&out.stdout).expect("one JSON result");
    assert_eq!(value["delivered"], false, "{value}");
    assert!(value["suppressed"].is_null(), "{value}");
    assert!(value["error"].is_string(), "{value}");

    let bus = NotificationBus::start(&instance.path("runtime"));
    let retry = instance.send(Some(&bus.address()), DEDUPED);
    assert_eq!(retry["delivered"], true, "{retry}");
    assert_eq!(bus.shown().len(), 1);
}

/// `--session` names the session the notification is about; sending it never
/// touches that session's state.
#[cfg(target_os = "linux")]
#[test]
fn notify_send_attaches_a_session_and_leaves_its_state_alone() {
    let instance = Instance::with_backend("dbus", "");
    let bus = NotificationBus::start(&instance.path("runtime"));
    let address = bus.address();
    let id = seed_session(&instance, "reviewer");
    instance.ok(
        Some(&address),
        &["session", "signal", "--session", &id, "--state", "blocked"],
    );
    let before = instance.ok(Some(&address), &["session", "show", "reviewer"]);
    let seq = instance.db().latest_session_event_seq().unwrap();

    let value = instance.send(Some(&address), &["--session", "reviewer"]);
    assert_eq!(value["delivered"], true, "{value}");
    assert_eq!(value["session_id"], id.as_str(), "{value}");

    let after = instance.ok(Some(&address), &["session", "show", "reviewer"]);
    assert_eq!(after["state"], before["state"], "{after}");
    assert_eq!(after["state"], "blocked", "{after}");
    assert_eq!(instance.db().latest_session_event_seq().unwrap(), seq);

    let unknown = instance.run(Some(&address), &send_args(&["--session", "nobody"]));
    assert!(!unknown.status.success(), "{}", describe(&unknown));
    assert_eq!(bus.shown().len(), 1, "nothing sent for an unknown session");
}

#[cfg(target_os = "linux")]
fn seed_session(instance: &Instance, name: &str) -> String {
    let row = thurbox::sync::SharedSession {
        id: thurbox::session::SessionId::default(),
        name: name.into(),
        agent: "claude".into(),
        backend_id: String::new(),
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
        key: None,
    };
    instance.db().upsert_session(&row).expect("persist");
    row.id.to_string()
}

#[cfg(target_os = "linux")]
fn fail_once(instance: &Instance, bus: &str, id: &str) -> Value {
    instance.ok(Some(bus), &["automation", "run", id]);
    instance.ok(Some(bus), &["automation", "tick"])
}

#[cfg(target_os = "linux")]
fn exec(instance: &Instance, bus: &str, name: &str, command: &str) -> String {
    let doc = instance.ok(
        Some(bus),
        &[
            "automation",
            "create",
            "--name",
            name,
            "--trigger",
            "hourly",
            "--command",
            command,
        ],
    );
    doc["id"].as_i64().expect("an automation id").to_string()
}

/// With the alert on, a failing run raises one notification; the failures
/// after it are deduplicated until the automation succeeds again.
#[cfg(target_os = "linux")]
#[test]
fn a_failing_run_alerts_once_per_failing_streak_when_opted_in() {
    let instance = Instance::with_backend("dbus", "on_automation_failure = true\n");
    let bus = NotificationBus::start(&instance.path("runtime"));
    let address = bus.address();
    let id = exec(&instance, &address, "nightly-sync", "exit 3");

    let tick = fail_once(&instance, &address, &id);
    assert_eq!(tick["fired"][0]["status"], "error", "{tick}");
    assert_eq!(tick["alerts"][0]["delivered"], true, "{tick}");
    let shown = bus.shown();
    assert_eq!(shown.len(), 1, "{shown:?}");
    assert!(shown[0].summary.contains("nightly-sync"), "{shown:?}");
    assert!(shown[0].body.contains("exit 3"), "{shown:?}");

    let tick = fail_once(&instance, &address, &id);
    assert_eq!(tick["alerts"][0]["suppressed"], "deduped", "{tick}");
    assert_eq!(
        bus.shown().len(),
        1,
        "a second failure in a row is not news"
    );

    instance.ok(
        Some(&address),
        &["automation", "edit", &id, "--command", "true"],
    );
    let tick = fail_once(&instance, &address, &id);
    assert_eq!(tick["fired"][0]["status"], "success", "{tick}");
    instance.ok(
        Some(&address),
        &["automation", "edit", &id, "--command", "exit 4"],
    );
    fail_once(&instance, &address, &id);
    assert_eq!(bus.shown().len(), 2, "a new streak alerts again");
}

#[cfg(target_os = "linux")]
#[test]
fn a_failing_run_raises_nothing_by_default() {
    let instance = Instance::with_backend("dbus", "");
    let bus = NotificationBus::start(&instance.path("runtime"));
    let address = bus.address();
    let id = exec(&instance, &address, "nightly-sync", "exit 3");

    let tick = fail_once(&instance, &address, &id);
    assert_eq!(tick["fired"][0]["status"], "error", "{tick}");
    assert!(
        tick["alerts"].as_array().map_or(true, Vec::is_empty),
        "{tick}"
    );
    assert!(bus.shown().is_empty(), "{:?}", bus.shown());
}
