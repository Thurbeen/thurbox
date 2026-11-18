#![cfg(unix)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde_json::Value;

const AGENTS_TOML: &str = r#"config_version = 1
default = "claude"

[[agents]]
name = "aider-x"
command = "aider"
args = ["--no-auto-commits"]
resume_args = ["--restore", "{id}"]
setup = ["pip install aider-chat"]
"#;

const HOSTS_TOML: &str = r#"config_version = 1

[[hosts]]
name = "devbox"
destination = "me@devbox"
ssh_opts = ["-o", "ControlMaster=auto"]
socket = "thurbox"
worktrees_dir = "/srv/worktrees"
"#;

const SCHEMA: &str = "
CREATE TABLE sessions (id TEXT PRIMARY KEY, name TEXT NOT NULL, agent TEXT NOT NULL DEFAULT 'claude',
  backend_type TEXT NOT NULL DEFAULT 'tmux', agent_session_id TEXT, cwd TEXT,
  created_at INTEGER NOT NULL, deleted_at INTEGER);
CREATE TABLE worktrees (session_id TEXT, repo_path TEXT, worktree_path TEXT, branch TEXT,
  created_at INTEGER, deleted_at INTEGER);
CREATE TABLE automations (id INTEGER PRIMARY KEY, name TEXT NOT NULL, enabled INTEGER NOT NULL,
  schedule_kind TEXT NOT NULL, schedule_spec TEXT NOT NULL, timezone TEXT, action_kind TEXT NOT NULL,
  target_session TEXT, repo_path TEXT, worktree_branch TEXT, base_branch TEXT, agent TEXT,
  prompt TEXT NOT NULL, action_command TEXT);
INSERT INTO sessions VALUES ('s1', 'Fix Login', 'claude', 'tmux', 'abc-123', '/src/app-wt', 1, NULL);
INSERT INTO sessions VALUES ('s2', 'notes', 'aider-x', 'ssh:devbox', NULL, '/home/me/notes', 2, NULL);
INSERT INTO sessions VALUES ('s3', 'gone', 'claude', 'tmux', NULL, '/tmp/gone', 3, 99);
INSERT INTO worktrees VALUES ('s1', '/src/app', '/src/app-wt', 'thurbox/fix-login', 1, NULL);
INSERT INTO automations VALUES (1, 'Nightly review', 1, 'cron', '0 3 * * *', NULL, 'spawn', NULL,
  '/src/app', NULL, 'main', 'claude', 'Review yesterday''s commits.', NULL);
INSERT INTO automations VALUES (2, 'poke', 1, 'cron', '*/5 * * * *', 'UTC', 'send', 's1', NULL, NULL,
  NULL, NULL, 'continue', NULL);
INSERT INTO automations VALUES (3, 'paris', 1, 'cron', '0 9 * * *', 'Europe/Paris', 'spawn', NULL,
  '/src/app', NULL, NULL, 'claude', 'Morning triage.', NULL);
";

fn fleet_bin() -> Option<PathBuf> {
    let found = std::env::var_os("FLEET_BIN")
        .map(PathBuf::from)
        .or_else(|| {
            Path::new(env!("CARGO_BIN_EXE_thurbox-cli"))
                .parent()
                .map(|dir| dir.join("fleet"))
        })
        .filter(|p| p.is_file());
    if found.is_none() {
        assert!(
            std::env::var_os("CI").is_none(),
            "no fleet binary in CI, where the tests that drive Fleet must run"
        );
        eprintln!("skipping: no fleet binary; build it with `cargo build -p fleet-cli`");
    }
    found
}

fn sqlite3() -> Option<PathBuf> {
    let found = std::env::var_os("PATH").and_then(|path| {
        std::env::split_paths(&path)
            .map(|dir| dir.join("sqlite3"))
            .find(|p| p.is_file())
    });
    if found.is_none() {
        assert!(
            std::env::var_os("CI").is_none(),
            "no sqlite3 in CI, where the 2.x migration must be tested"
        );
        eprintln!("skipping: no sqlite3 on PATH");
    }
    found
}

fn make_executable(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
}

struct Run {
    code: Option<i32>,
    stdout: String,
    stderr: String,
}

impl Run {
    fn ok(&self) -> &Run {
        assert_eq!(
            self.code,
            Some(0),
            "stdout:\n{}\nstderr:\n{}",
            self.stdout,
            self.stderr
        );
        self
    }

    fn has(&self, needle: &str) -> &Run {
        assert!(
            self.stdout.contains(needle),
            "{needle:?} not in stdout:\n{}\nstderr:\n{}",
            self.stdout,
            self.stderr
        );
        self
    }

    fn lacks(&self, needle: &str) -> &Run {
        assert!(
            !self.stdout.contains(needle),
            "{needle:?} in stdout:\n{}",
            self.stdout
        );
        self
    }
}

struct V2 {
    root: tempfile::TempDir,
    fleet: PathBuf,
    bin: PathBuf,
}

impl V2 {
    fn new(fleet_bin: &Path, sqlite3: &Path) -> V2 {
        let root = tempfile::Builder::new()
            .prefix("migrate-v2-")
            .tempdir_in(env!("CARGO_TARGET_TMPDIR"))
            .expect("root");
        let r = root.path();
        for dir in ["tmp", "bin", "config", "data"] {
            std::fs::create_dir_all(r.join(dir)).expect("dir");
        }
        std::fs::write(
            r.join("fleet.toml"),
            "version = 1\n\n[data]\ndir = \"store\"\n\n[runtime]\nkind = \"fake\"\ndir = \"fake\"\n",
        )
        .expect("fleet config");
        let fleet = r.join("fleet.sh");
        std::fs::write(
            &fleet,
            format!(
                "#!/bin/sh\nexec /usr/bin/env -i HOME='{r}' TMPDIR='{r}/tmp' PATH='/usr/bin:/bin' '{bin}' --config '{r}/fleet.toml' \"$@\"\n",
                r = r.display(),
                bin = fleet_bin.display()
            ),
        )
        .expect("fleet wrapper");
        make_executable(&fleet);
        std::os::unix::fs::symlink(sqlite3, r.join("bin/sqlite3")).expect("sqlite3 link");
        std::fs::write(
            r.join("config/settings.toml"),
            format!(
                "config_version = 1\nmultiplexer = \"tmux\"\n\n[fleets.local]\ntransport = \"local\"\nprogram = \"{}\"\n",
                fleet.display()
            ),
        )
        .expect("settings");
        std::fs::write(r.join("config/agents.toml"), AGENTS_TOML).expect("agents");
        std::fs::write(r.join("config/hosts.toml"), HOSTS_TOML).expect("hosts");
        let out = Command::new(sqlite3)
            .arg(r.join("data/thurbox.db"))
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .spawn()
            .and_then(|mut child| {
                use std::io::Write;
                child.stdin.take().unwrap().write_all(SCHEMA.as_bytes())?;
                child.wait()
            })
            .expect("sqlite3");
        assert!(out.success(), "the 2.x database fixture");
        V2 {
            bin: r.join("bin"),
            root,
            fleet,
        }
    }

    fn path(&self, rel: &str) -> PathBuf {
        self.root.path().join(rel)
    }

    fn migrate(&self, args: &[&str]) -> Run {
        self.migrate_with_path(args, &format!("{}:/usr/bin:/bin", self.bin.display()))
    }

    fn migrate_with_path(&self, args: &[&str], path: &str) -> Run {
        let out = Command::new(env!("CARGO_BIN_EXE_thurbox-cli"))
            .arg("--text")
            .arg("migrate")
            .args(args)
            .env_clear()
            .env("HOME", self.root.path())
            .env("TMPDIR", self.path("tmp"))
            .env("PATH", path)
            .env("THURBOX_CONFIG_DIR", self.path("config"))
            .env("THURBOX_DATA_DIR", self.path("data"))
            .stdin(Stdio::null())
            .output()
            .expect("thurbox-cli");
        Run {
            code: out.status.code(),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        }
    }

    fn fleet_json(&self, args: &[&str]) -> Option<Value> {
        let out = Command::new(&self.fleet)
            .args(args)
            .output()
            .expect("fleet");
        out.status
            .success()
            .then(|| serde_json::from_slice(&out.stdout).expect("fleet JSON"))
    }

    fn files(&self) -> BTreeMap<PathBuf, Vec<u8>> {
        let mut out = BTreeMap::new();
        for dir in ["config", "data"] {
            for entry in std::fs::read_dir(self.path(dir)).expect("dir") {
                let path = entry.expect("entry").path();
                if path.is_file() {
                    out.insert(path.clone(), std::fs::read(&path).expect("file"));
                }
            }
        }
        out
    }
}

fn setup() -> Option<V2> {
    let fleet = fleet_bin()?;
    let sqlite3 = sqlite3()?;
    Some(V2::new(&fleet, &sqlite3))
}

#[test]
fn a_dry_run_prints_the_plan_and_changes_nothing() {
    let Some(v2) = setup() else { return };
    let before = v2.files();
    v2.migrate(&["--dry-run"])
        .ok()
        .has("dry run")
        .has("Task/fix-login")
        .has("CronTask/nightly-review");
    assert_eq!(before, v2.files());
    assert!(v2.fleet_json(&["get", "task", "fix-login"]).is_none());
}

#[test]
fn the_2x_state_becomes_fleet_objects_and_ready_to_paste_tables() {
    let Some(v2) = setup() else { return };
    let db = std::fs::read(v2.path("data/thurbox.db")).expect("db");
    let settings = std::fs::read_to_string(v2.path("config/settings.toml")).expect("settings");
    v2.migrate(&[])
        .ok()
        .has("Task/fix-login")
        .has("Task/notes")
        .lacks("Task/gone")
        .has("claude --resume abc-123")
        .has("CronTask/nightly-review")
        .has("poke")
        .has("paris")
        .has("[runtimes.mux.places.devbox]")
        .has("destination = \"me@devbox\"")
        .has("worktrees_dir")
        .has("harnesses/aider-x.toml")
        .has("program = \"aider\"")
        .has("\"--restore\"")
        .has("{conversation}")
        .has("setup")
        .has("multiplexer");

    let fix = v2
        .fleet_json(&["get", "task", "fix-login"])
        .expect("Task fix-login");
    assert_eq!(
        fix["spec"]["workspace"],
        serde_json::json!(["git+file:///src/app?branch=thurbox/fix-login"])
    );
    assert_eq!(fix["spec"]["harness"], "claude");
    assert_eq!(fix["spec"]["host"], "local");
    assert_eq!(fix["spec"]["dispatch"], false);
    assert_eq!(fix["spec"]["brief"], "");
    assert_eq!(fix["metadata"]["labels"]["thurbox.dev/name"], "Fix Login");

    let notes = v2
        .fleet_json(&["get", "task", "notes"])
        .expect("Task notes");
    assert_eq!(
        notes["spec"]["workspace"],
        serde_json::json!(["file:///home/me/notes"])
    );
    assert_eq!(notes["spec"]["host"], "mux/devbox");
    assert_eq!(notes["spec"]["harness"], "aider-x");

    let cron = v2
        .fleet_json(&["get", "crontask", "nightly-review"])
        .expect("CronTask nightly-review");
    assert_eq!(cron["spec"]["schedule"], "0 3 * * *");
    assert_eq!(cron["spec"]["suspend"], true);
    assert_eq!(cron["spec"]["template"]["spec"]["harness"], "claude");
    assert_eq!(
        cron["spec"]["template"]["spec"]["brief"],
        "Review yesterday's commits."
    );
    assert_eq!(
        cron["spec"]["template"]["spec"]["workspace"],
        serde_json::json!(["git+file:///src/app#main"])
    );
    assert!(v2.fleet_json(&["get", "crontask", "paris"]).is_none());
    assert!(v2.fleet_json(&["get", "crontask", "poke"]).is_none());

    assert_eq!(std::fs::read(v2.path("data/thurbox.db")).expect("db"), db);
    assert_eq!(
        std::fs::read_to_string(v2.path("config/settings.toml.v2")).expect("settings.v2"),
        settings
    );
    let cleaned = std::fs::read_to_string(v2.path("config/settings.toml")).expect("settings");
    assert!(!cleaned.contains("multiplexer"), "{cleaned}");
    assert!(cleaned.contains("[fleets.local]"), "{cleaned}");
    assert_eq!(
        std::fs::read_to_string(v2.path("config/agents.toml")).expect("agents"),
        AGENTS_TOML
    );
    assert_eq!(
        std::fs::read_to_string(v2.path("config/hosts.toml")).expect("hosts"),
        HOSTS_TOML
    );
}

#[test]
fn a_second_run_imports_nothing_again() {
    let Some(v2) = setup() else { return };
    v2.migrate(&[]).ok();
    let files = v2.files();
    let run = v2.migrate(&[]);
    run.ok().has("Already imported");
    assert_eq!(files, v2.files());
    let tasks = v2.fleet_json(&["list", "task", "--json"]).expect("tasks");
    assert_eq!(tasks.as_array().map(Vec::len), Some(2), "{tasks:#}");
}

#[test]
fn without_sqlite3_the_rest_is_migrated_and_the_database_named() {
    let Some(v2) = setup() else { return };
    v2.migrate_with_path(&[], "/nonexistent")
        .ok()
        .has("sqlite3")
        .has("[runtimes.mux.places.devbox]")
        .has("harnesses/aider-x.toml");
    assert!(v2.fleet_json(&["get", "task", "fix-login"]).is_none());
    assert!(!v2.path("data/thurbox.db.migrated").exists());
}
