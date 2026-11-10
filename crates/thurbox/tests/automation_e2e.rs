//! Automations through the real `thurbox-cli`, in an isolated instance: the
//! schedule in `automations.json`, an exec fired on this machine, and a send
//! and a spawn fired through Fleet's command line, which a stand-in `fleet`
//! records here.
//!
//! Unix-only: the stand-in `fleet` and the exec commands are `sh` scripts.
#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::Value;

struct Instance {
    root: tempfile::TempDir,
}

impl Instance {
    fn new() -> Self {
        let root = tempfile::TempDir::new().expect("isolated instance");
        for sub in ["home", "config", "data", "bin"] {
            std::fs::create_dir_all(root.path().join(sub)).expect("instance directory");
        }
        let fleet = root.path().join("bin/fleet");
        std::fs::write(
            &fleet,
            format!(
                "#!/bin/sh\nlog='{}'\necho \"$*\" >> \"$log\"\nif [ \"$1\" = apply ]; then cat >> \"$log\"; echo >> \"$log\"; fi\n",
                root.path().join("fleet.log").display()
            ),
        )
        .expect("stand-in fleet");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&fleet, std::fs::Permissions::from_mode(0o755))
                .expect("chmod");
        }
        std::fs::write(
            root.path().join("config/settings.toml"),
            format!(
                "[features]\nversion_check = false\nauto_update = false\n\n[fleet]\nprogram = '{}'\n",
                fleet.display()
            ),
        )
        .expect("settings");
        Self { root }
    }

    fn path(&self, sub: &str) -> PathBuf {
        self.root.path().join(sub)
    }

    fn run(&self, args: &[&str]) -> Output {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_thurbox-cli"));
        cmd.arg("--json")
            .args(args)
            .env("HOME", self.path("home"))
            .env("USERPROFILE", self.path("home"))
            .env("THURBOX_CONFIG_DIR", self.path("config"))
            .env("THURBOX_DATA_DIR", self.path("data"));
        for var in ["THURBOX_SOCKET", "THURBOX_SOCKET_FOR", "THURBOX_SESSION"] {
            cmd.env_remove(var);
        }
        cmd.output().expect("run thurbox-cli")
    }

    /// Run `args`; it must succeed. Its one JSON document.
    fn ok(&self, args: &[&str]) -> Value {
        let out = self.run(args);
        assert!(out.status.success(), "{args:?} failed:\n{}", describe(&out));
        serde_json::from_slice(&out.stdout).expect("one JSON result")
    }

    fn fleet_log(&self) -> String {
        std::fs::read_to_string(self.path("fleet.log")).unwrap_or_default()
    }
}

fn describe(out: &Output) -> String {
    format!(
        "status: {}\nstdout: {}\nstderr: {}",
        out.status,
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_millis() as u64
}

fn lines(path: &Path) -> usize {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .count()
}

#[test]
fn an_exec_fires_once_when_due_and_its_run_is_recorded() {
    let instance = Instance::new();
    let out = instance.path("out.txt");
    let at = format!("at:{}", now_ms() + 1_500);
    let command = format!(
        "echo \"ran $THURBOX_AUTOMATION_NAME\" >> '{}'",
        out.display()
    );
    instance.ok(&[
        "automation",
        "create",
        "--name",
        "job",
        "--trigger",
        &at,
        "--command",
        &command,
    ]);

    let early = instance.ok(&["automation", "tick"]);
    assert_eq!(early["fired"].as_array().map(Vec::len), Some(0), "{early}");
    std::thread::sleep(Duration::from_millis(1_700));
    let fired = instance.ok(&["automation", "tick"]);
    assert_eq!(fired["fired"][0]["name"], "job", "{fired}");
    assert_eq!(fired["fired"][0]["run"]["status"], "success", "{fired}");
    instance.ok(&["automation", "tick"]);

    assert_eq!(std::fs::read_to_string(&out).unwrap(), "ran job\n");
    let shown = instance.ok(&["automation", "show", "job"]);
    assert_eq!(shown["runs"].as_array().map(Vec::len), Some(1), "{shown}");
    assert!(
        shown["next_run_at"].is_null(),
        "a fired one-shot has no next run: {shown}"
    );
    assert!(instance.path("config/automations.json").is_file());
}

#[test]
fn a_cron_automation_is_due_at_its_next_occurrence_and_not_before() {
    let instance = Instance::new();
    let out = instance.path("out.txt");
    let command = format!("echo x >> '{}'", out.display());
    let created = instance.ok(&[
        "automation",
        "create",
        "--name",
        "minutely",
        "--trigger",
        "cron:* * * * *",
        "--command",
        &command,
    ]);
    let next = created["next_run_at"].as_u64().expect("a next run");
    assert!(next > now_ms() && next <= now_ms() + 60_000, "{created}");
    instance.ok(&["automation", "tick"]);
    assert_eq!(lines(&out), 0, "not due yet");
}

#[test]
fn a_send_queues_its_prompt_for_the_session_through_fleet() {
    let instance = Instance::new();
    instance.ok(&[
        "automation",
        "create",
        "--name",
        "nudge",
        "--trigger",
        "daily",
        "--session",
        "probe",
        "--prompt",
        "check the build",
    ]);
    let run = instance.ok(&["automation", "run", "nudge"]);
    assert_eq!(run["run"]["status"], "success", "{run}");
    assert_eq!(run["run"]["session"], "probe", "{run}");
    assert_eq!(instance.fleet_log(), "send probe check the build\n");
}

#[test]
fn a_spawn_creates_a_task_and_queues_its_prompt() {
    let instance = Instance::new();
    let repo = instance.path("repo");
    std::fs::create_dir_all(&repo).unwrap();
    instance.ok(&[
        "automation",
        "create",
        "--name",
        "Nightly Review",
        "--trigger",
        "weekdays",
        "--time",
        "03:00",
        "--repo",
        &repo.to_string_lossy(),
        "--agent",
        "claude",
        "--prompt",
        "review main",
    ]);
    let run = instance.ok(&["automation", "run", "Nightly Review"]);
    assert_eq!(run["run"]["status"], "success", "{run}");
    let task = run["run"]["session"].as_str().expect("the Task it made");
    assert!(task.starts_with("nightly-review-"), "{task}");

    let log = instance.fleet_log();
    let mut entries = log.lines();
    assert_eq!(entries.next(), Some("apply --create-only -f -"), "{log}");
    let applied: Value = serde_json::from_str(entries.next().expect("the Task")).expect("JSON");
    assert_eq!(applied["metadata"]["name"], task, "{applied}");
    assert_eq!(
        applied["spec"]["workspace"]["dir"],
        repo.to_string_lossy().as_ref()
    );
    assert_eq!(applied["spec"]["harness"], "claude");
    assert_eq!(applied["spec"]["dispatch"], true);
    assert_eq!(
        entries.next(),
        Some(format!("send {task} review main").as_str()),
        "{log}"
    );
}

#[test]
fn an_exec_past_its_timeout_is_stopped_and_recorded_as_timed_out() {
    let instance = Instance::new();
    instance.ok(&[
        "automation",
        "create",
        "--name",
        "hang",
        "--trigger",
        "hourly",
        "--command",
        "sleep 30",
        "--timeout-secs",
        "1",
    ]);
    let started = std::time::Instant::now();
    let out = instance.run(&["automation", "run", "hang"]);
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "waited on the command"
    );
    assert!(!out.status.success(), "{}", describe(&out));
    let run: Value = serde_json::from_slice(&out.stdout).expect("one JSON result");
    assert_eq!(run["run"]["status"], "timed_out", "{run}");
}

#[test]
fn a_disabled_automation_is_not_fired_and_edits_and_removal_stick() {
    let instance = Instance::new();
    instance.ok(&[
        "automation",
        "create",
        "--name",
        "job",
        "--trigger",
        "hourly",
        "--command",
        "true",
        "--disabled",
    ]);
    let refused = instance.run(&["automation", "run", "job"]);
    assert!(!refused.status.success(), "{}", describe(&refused));

    let enabled = instance.ok(&[
        "automation",
        "edit",
        "job",
        "--enable",
        "--trigger",
        "daily",
    ]);
    assert_eq!(enabled["enabled"], true, "{enabled}");
    assert!(
        enabled["trigger"].as_str().unwrap().starts_with("cron:"),
        "{enabled}"
    );
    assert!(enabled["next_run_at"].is_u64(), "{enabled}");

    let listed = instance.ok(&["automation", "list"]);
    assert_eq!(listed.as_array().map(Vec::len), Some(1), "{listed}");
    instance.ok(&["automation", "remove", "job"]);
    let listed = instance.ok(&["automation", "list"]);
    assert_eq!(listed.as_array().map(Vec::len), Some(0), "{listed}");
}

#[test]
fn a_create_needs_one_action_a_trigger_and_a_free_name() {
    let instance = Instance::new();
    for args in [
        &["--name", "a", "--trigger", "daily"][..],
        &[
            "--name",
            "a",
            "--trigger",
            "daily",
            "--session",
            "s",
            "--command",
            "true",
        ][..],
        &["--name", "a", "--command", "true"][..],
        &[
            "--name",
            "a",
            "--trigger",
            "fortnightly",
            "--command",
            "true",
        ][..],
        &["--name", "a", "--trigger", "at:1000", "--command", "true"][..],
    ] {
        let out = instance.run(&[&["automation", "create"][..], args].concat());
        assert_eq!(out.status.code(), Some(2), "{args:?}:\n{}", describe(&out));
    }
    let create = [
        "automation",
        "create",
        "--name",
        "a",
        "--trigger",
        "daily",
        "--command",
        "true",
    ];
    instance.ok(&create);
    let again = instance.run(&create);
    assert!(!again.status.success(), "{}", describe(&again));
}
