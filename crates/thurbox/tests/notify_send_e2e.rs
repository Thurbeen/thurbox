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
    fn new(features: &str, notifications: &str) -> Self {
        let root = tempfile::TempDir::new().expect("isolated instance");
        for sub in ["home", "config", "data", "tmux", "runtime"] {
            std::fs::create_dir_all(root.path().join(sub)).expect("instance directory");
        }
        std::fs::write(
            root.path().join("config/settings.toml"),
            format!(
                "[features]\nversion_check = false\nauto_update = false\n\
                 {features}\n[notifications]\n{notifications}\n"
            ),
        )
        .expect("notification settings");
        Self { root }
    }

    fn with_backend(backend: &str, extra: &str) -> Self {
        Self::new(
            "",
            &format!("backend = '{backend}'\nsound = false\n{extra}"),
        )
    }

    fn path(&self, sub: &str) -> PathBuf {
        self.root.path().join(sub)
    }

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
        ] {
            cmd.env_remove(var);
        }
        cmd
    }

    fn run(&self, bus: Option<&str>, args: &[&str]) -> Output {
        self.command(bus, args).output().expect("run thurbox-cli")
    }

    fn ok(&self, bus: Option<&str>, args: &[&str]) -> Value {
        let out = self.run(bus, args);
        assert!(out.status.success(), "{args:?} failed:\n{}", describe(&out));
        // from_slice rejects trailing data, including a second JSON document.
        serde_json::from_slice(&out.stdout).expect("one JSON result")
    }

    fn send(&self, bus: Option<&str>, extra: &[&str]) -> Value {
        self.ok(bus, &send_args(extra))
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
    // An off switch short-circuits before the bus or the dedupe key is
    // looked at.
    assert_disabled(&instance.send(Some(NO_BUS), DEDUPED));
    assert_disabled(&instance.send(Some(NO_BUS), DEDUPED));
}

#[test]
fn notify_send_honours_backend_off() {
    let instance = Instance::with_backend("off", "");
    assert_disabled(&instance.send(Some(NO_BUS), DEDUPED));
}

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

#[test]
fn an_interval_beyond_a_year_is_refused() {
    let instance = Instance::with_backend("off", "");
    for secs in ["18446744073709551615", "31536001"] {
        let out = instance.run(
            Some(NO_BUS),
            &send_args(&["--key", "k", "--min-interval", secs]),
        );
        assert_eq!(
            out.status.code(),
            Some(2),
            "--min-interval {secs}:\n{}",
            describe(&out)
        );
    }
    let year = instance.send(Some(NO_BUS), &["--key", "k", "--min-interval", "31536000"]);
    assert_disabled(&year);
}

#[cfg(target_os = "linux")]
#[test]
fn notification_bus_keeps_serving_between_cli_processes() {
    let instance = Instance::with_backend("dbus", "");
    let bus = NotificationBus::start(&instance.path("runtime"));

    for title in ["First build", "Second build"] {
        let value = instance.ok(
            Some(&bus.address()),
            &[
                "notify",
                "send",
                "--title",
                title,
                "--body",
                "Inspect the run log",
            ],
        );
        assert_eq!(value["delivered"], true, "{value}");
        assert!(value["suppressed"].is_null(), "{value}");
        assert!(value["error"].is_null(), "{value}");
    }

    let shown = bus.shown();
    assert_eq!(shown.len(), 2, "{shown:?}");
    assert_eq!(shown[0].summary, "First build");
    assert_eq!(shown[1].summary, "Second build");
    assert!(shown
        .iter()
        .all(|n| n.app_name == "thurbox" && n.body == "Inspect the run log"));
}

#[cfg(target_os = "linux")]
#[test]
fn notify_send_finds_the_runtime_bus_without_a_bus_address() {
    let instance = Instance::with_backend("auto", "");
    let bus = NotificationBus::start(&instance.path("runtime"));

    let value = instance.send(None, &[]);
    assert_eq!(value["delivered"], true, "{value}");
    assert_eq!(bus.shown().len(), 1);
}

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
#[cfg(unix)]
fn fake_toast(instance: &Instance, body: &str) -> String {
    use std::os::unix::fs::PermissionsExt;
    let bin = instance.path("bin");
    std::fs::create_dir_all(&bin).expect("bin dir");
    let script = bin.join("powershell.exe");
    std::fs::write(
        &script,
        format!(
            "#!/bin/sh\n{}\n",
            body.replace(
                "{pid}",
                &instance.path("notifier.pid").display().to_string()
            )
        ),
    )
    .expect("stand-in notifier");
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    format!(
        "{}:{}",
        bin.display(),
        std::env::var("PATH").unwrap_or_default()
    )
}

#[cfg(unix)]
#[test]
fn a_loud_failing_notifier_is_not_mistaken_for_a_hung_one() {
    let instance = Instance::with_backend("windows", "");
    let path = fake_toast(
        &instance,
        "head -c 200000 /dev/zero | tr '\\000' x >&2\necho toast-refused >&2\nexit 1",
    );

    let started = std::time::Instant::now();
    let out = instance
        .command(Some(NO_BUS), &send_args(&[]))
        .env("PATH", path)
        .output()
        .expect("run thurbox-cli");
    let waited = started.elapsed();

    assert!(!out.status.success(), "{}", describe(&out));
    let value: Value = serde_json::from_slice(&out.stdout).expect("one JSON result");
    let error = value["error"].as_str().unwrap_or_default();
    assert!(error.contains("toast-refused"), "{value}");
    assert!(
        waited < std::time::Duration::from_secs(5),
        "a notifier that exited at once was waited on for {waited:?}"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn a_late_failure_keeps_a_newer_hold() {
    let instance = Instance::with_backend("windows", "");
    let path = fake_toast(&instance, "echo $$ > '{pid}'\nsleep 3\nexit 1");
    let short = ["--key", "k", "--min-interval", "1"];
    // Slow and failing: holds `k` for one second, fails after three.
    let slow = instance
        .command(Some(NO_BUS), &send_args(&short))
        .env("PATH", path)
        .spawn()
        .expect("start the slow send");
    // Spawning the CLI does not mean it has read its settings or taken the
    // lease yet. The notifier starting proves both before we replace them.
    let started = instance.path("notifier.pid");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while !started.exists() {
        assert!(
            std::time::Instant::now() < deadline,
            "the slow notifier never started"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    std::thread::sleep(std::time::Duration::from_millis(1_500));

    std::fs::write(
        instance.path("config/settings.toml"),
        "[features]\nversion_check = false\nauto_update = false\n\
         [notifications]\nbackend = 'dbus'\nsound = false\n",
    )
    .expect("switch to dbus");
    let bus = NotificationBus::start(&instance.path("runtime"));
    let address = bus.address();
    let held = ["--key", "k", "--min-interval", "60"];
    let delivered = instance.send(Some(&address), &held);
    assert_eq!(delivered["delivered"], true, "{delivered}");

    let slow = slow.wait_with_output().expect("the slow send ends");
    assert!(!slow.status.success(), "{}", describe(&slow));

    let repeat = instance.send(Some(&address), &held);
    assert_eq!(repeat["suppressed"], "deduped", "{repeat}");
    assert_eq!(bus.shown().len(), 1);
}
