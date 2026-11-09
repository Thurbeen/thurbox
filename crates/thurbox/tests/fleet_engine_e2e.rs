//! thurbox on Fleet (ADR-41): the snapshot reads the Tasks through `fleet
//! watch`, the commands act through `fleet apply`, `send` and `delete`, and
//! thurbox keeps a `fleet up` running. The real `fleet` binary against an
//! isolated store and Fleet's fake runtime; thurbox links no Fleet crate, so
//! the binary is found beside `thurbox-cli`, as a workspace build leaves it.
//!
//! Unix only: the configured program is a shell script.
#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::{Command as Process, Stdio};
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use thurbox::kernel::command::{Command, CommandBus, Phase};
use thurbox::kernel::snapshot::fleet::FleetWatch;
use thurbox::kernel::snapshot::SnapshotStore;

const BOUND: Duration = Duration::from_secs(60);

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
        eprintln!("skipping: no fleet binary; build it with `cargo build -p fleet-cli`");
    }
    found
}

/// One isolated Fleet: a store, the fake runtime, and `program`, the script
/// thurbox is configured with, which runs the real binary against them.
struct Fleet {
    root: tempfile::TempDir,
    program: PathBuf,
}

impl Fleet {
    fn new(bin: PathBuf) -> Fleet {
        let root = tempfile::Builder::new()
            .prefix("fleet-engine-")
            .tempdir_in(env!("CARGO_TARGET_TMPDIR"))
            .expect("root");
        let r = root.path();
        std::fs::create_dir_all(r.join("tmp")).expect("tmp");
        std::fs::write(
            r.join("fleet.toml"),
            "version = 1\n\n[data]\ndir = \"data\"\n\n[runtime]\nkind = \"fake\"\ndir = \"fake\"\n",
        )
        .expect("config");
        let program = r.join("fleet.sh");
        std::fs::write(
            &program,
            format!(
                "#!/bin/sh\nexec env -i HOME='{r}' TMPDIR='{r}/tmp' PATH=/usr/bin:/bin '{bin}' --config '{r}/fleet.toml' \"$@\"\n",
                r = r.display(),
                bin = bin.display()
            ),
        )
        .expect("program");
        make_executable(&program);
        Fleet { root, program }
    }

    /// The same, after `fleet init`, so `fleet up` has its starter Task.
    fn initialized(bin: PathBuf) -> Fleet {
        let fleet = Fleet::new(bin);
        let config = fleet.root.path().join("fleet.toml");
        std::fs::remove_file(&config).expect("fresh config");
        fleet.run(&["init"]).ok();
        let mut doc: toml::Table =
            toml::from_str(&std::fs::read_to_string(&config).unwrap()).expect("init's config");
        doc.remove("runtimes");
        doc.remove("hosts");
        let fake = fleet.root.path().join("fake");
        doc.insert(
            "runtime".into(),
            toml::Value::Table(toml::Table::from_iter([
                ("kind".to_string(), toml::Value::String("fake".into())),
                (
                    "dir".to_string(),
                    toml::Value::String(fake.to_string_lossy().into_owned()),
                ),
            ])),
        );
        doc.insert(
            "data".into(),
            toml::Value::Table(toml::Table::from_iter([(
                "dir".to_string(),
                toml::Value::String(fleet.data().to_string_lossy().into_owned()),
            )])),
        );
        std::fs::write(&config, toml::to_string(&doc).unwrap()).expect("config");
        fleet
    }

    fn data(&self) -> PathBuf {
        self.root.path().join("data")
    }

    fn run(&self, args: &[&str]) -> Run {
        self.run_stdin(args, "")
    }

    fn run_stdin(&self, args: &[&str], input: &str) -> Run {
        use std::io::Write;
        let mut child = Process::new(&self.program)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("fleet");
        child
            .stdin
            .take()
            .unwrap()
            .write_all(input.as_bytes())
            .expect("stdin");
        let out = child.wait_with_output().expect("fleet");
        Run {
            code: out.status.code(),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        }
    }

    fn apply(&self, manifest: Value) {
        self.run_stdin(&["apply", "-f", "-"], &manifest.to_string())
            .ok();
    }

    fn get(&self, name: &str) -> Option<Value> {
        let run = self.run(&["get", "task", name]);
        (run.code == Some(0)).then(|| serde_json::from_str(&run.stdout).expect("JSON"))
    }

    /// The pid of the `fleet up` holding the lease, if one does.
    fn manager(&self) -> Option<u32> {
        let lease: Value =
            serde_json::from_slice(&std::fs::read(self.data().join("lease.json")).ok()?).ok()?;
        lease["holder"]
            .as_str()?
            .strip_prefix("fleet-up-")?
            .parse()
            .ok()
    }
}

impl Drop for Fleet {
    fn drop(&mut self) {
        // A manager thurbox started outlives it on purpose; this one ends
        // with its test.
        if let Some(pid) = self.manager() {
            let _ = Process::new("kill").arg(pid.to_string()).status();
        }
    }
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
}

#[cfg(unix)]
fn make_executable(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
}

fn task(name: &str) -> Value {
    json!({
        "apiVersion": "fleet/v1alpha1",
        "kind": "Task",
        "metadata": {"name": name},
        "spec": {
            "harness": "claude-code",
            "host": "local",
            "brief": format!("Do the work for {name}."),
            "workspace": {"dir": format!("work/{name}")},
        }
    })
}

fn until(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + BOUND;
    while !done() {
        assert!(Instant::now() < deadline, "never happened: {what}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn store_over(fleet: &Fleet) -> SnapshotStore {
    store_of(FleetWatch::spawn(fleet.program.clone()))
}

fn store_of(source: FleetWatch) -> SnapshotStore {
    SnapshotStore::with_source(
        Box::new(source),
        &thurbox::backend::wiring::configured(&Default::default()).0,
        &Default::default(),
    )
}

fn names(store: &mut SnapshotStore) -> Vec<String> {
    store.refresh_if_due();
    let mut names: Vec<String> = store
        .current()
        .sessions
        .iter()
        .map(|row| row.name.clone())
        .collect();
    names.sort();
    names
}

#[test]
fn the_snapshot_shows_each_fleet_task_as_a_session_and_follows_a_deletion() {
    let Some(bin) = fleet_bin() else { return };
    let fleet = Fleet::new(bin);
    fleet.apply(json!([task("alpha"), task("beta")]));

    let mut store = store_over(&fleet);
    until("both Tasks are rows", || {
        names(&mut store) == ["alpha", "beta"]
    });
    let alpha = store.current().session("alpha").expect("by name").clone();
    assert_eq!(alpha.agent, "claude-code");
    assert_eq!(alpha.backend, "fleet");
    assert_eq!(alpha.cwd.as_deref(), Some(Path::new("work/alpha")));

    // The controller releases a deleted Task, then it leaves the store.
    fleet.run(&["delete", "task", "beta"]).ok();
    fleet.run(&["reconcile"]).ok();
    until("the deleted Task's row goes", || {
        names(&mut store) == ["alpha"]
    });
    assert_eq!(store.current().error, None);
}

#[test]
fn a_fleet_that_will_not_answer_is_an_error_beside_the_last_rows() {
    let Some(bin) = fleet_bin() else { return };
    let fleet = Fleet::new(bin);
    let mut store = SnapshotStore::with_source(
        Box::new(FleetWatch::spawn(fleet.root.path().join("no-such-fleet"))),
        &thurbox::backend::wiring::configured(&Default::default()).0,
        &Default::default(),
    );
    until("the error is published", || {
        store.refresh_if_due();
        store.current().error.is_some()
    });
    let error = store.current().error.clone().unwrap();
    assert!(error.contains("fleet watch"), "{error}");
}

fn run_command(bus: &mut CommandBus, command: Command) -> Result<(), String> {
    let id = bus.dispatch(command);
    let deadline = Instant::now() + BOUND;
    loop {
        bus.poll();
        match bus.inflight().into_iter().find(|entry| entry.id == id) {
            None => return Ok(()),
            Some(entry) if entry.phase == Phase::Failed => {
                return Err(entry.error.unwrap_or_default())
            }
            Some(_) => {}
        }
        assert!(Instant::now() < deadline, "command {id} never finished");
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn the_commands_create_send_to_and_delete_fleet_tasks() {
    let Some(bin) = fleet_bin() else { return };
    let fleet = Fleet::new(bin);
    let repo = fleet.root.path().join("repo");
    std::fs::create_dir_all(&repo).expect("repo");
    let mut bus = CommandBus::fleet(fleet.program.clone());

    // A session thurbox creates is an interactive Task: no brief, dispatched.
    run_command(
        &mut bus,
        Command::Create {
            name: "gamma".into(),
            repo: repo.to_string_lossy().into_owned(),
            branch: None,
            base: None,
            worktree_path: None,
            agent: Some("claude-code".into()),
            host: None,
            multiplexer: None,
            extras: Vec::new(),
        },
    )
    .expect("create");
    let gamma = fleet.get("gamma").expect("the Task");
    assert_eq!(gamma["spec"]["harness"], "claude-code");
    assert_eq!(gamma["spec"]["host"], "local");
    assert_eq!(gamma["spec"]["dispatch"], true);
    assert_eq!(
        gamma["spec"]["workspace"]["dir"],
        repo.to_string_lossy().as_ref()
    );
    assert!(gamma["spec"]["brief"].is_null(), "{gamma:#}");

    run_command(
        &mut bus,
        Command::Send {
            session: "gamma".into(),
            text: "hello there".into(),
        },
    )
    .expect("send");
    let inbox = fleet.get("gamma").unwrap()["status"]["inbox"].clone();
    assert_eq!(inbox[0]["text"], "hello there", "{inbox:#}");

    // What Fleet cannot do is refused by name, never dropped.
    let refused = run_command(
        &mut bus,
        Command::Create {
            name: "delta".into(),
            repo: repo.to_string_lossy().into_owned(),
            branch: Some("feature".into()),
            base: None,
            worktree_path: None,
            agent: None,
            host: None,
            multiplexer: None,
            extras: Vec::new(),
        },
    )
    .expect_err("a branch is the workspace's to make");
    assert!(refused.contains("branch"), "{refused}");
    assert!(fleet.get("delta").is_none());

    run_command(
        &mut bus,
        Command::Delete {
            session: "gamma".into(),
            force: false,
        },
    )
    .expect("delete");
    let gamma = fleet.get("gamma");
    assert!(
        gamma
            .as_ref()
            .is_none_or(|t| t["metadata"]["deletionTimestamp"].is_u64()),
        "{gamma:#?}"
    );

    let missing = run_command(
        &mut bus,
        Command::Send {
            session: "nobody".into(),
            text: "hi".into(),
        },
    )
    .expect_err("no such Task");
    assert!(missing.contains("nobody"), "{missing}");
}

#[test]
fn thurbox_starts_fleet_up_when_no_manager_holds_the_lease() {
    let Some(bin) = fleet_bin() else { return };
    let fleet = Fleet::initialized(bin);
    assert_eq!(fleet.manager(), None);

    let mut store = store_of(FleetWatch::with_manager(fleet.program.clone()));
    // `fleet up` ensures the starter Task and reconciles it.
    until("a manager holds the lease", || fleet.manager().is_some());
    until("the starter Task is a row", || {
        names(&mut store).contains(&"vega".to_string())
    });
    let first = fleet.manager();
    let phase = || fleet.get("vega").map(|t| t["status"]["phase"].clone());
    until("the manager reconciles", || {
        phase().is_some_and(|p| p.is_string())
    });

    // A second interface finds the lease held, and leaves that manager be.
    let mut second = store_of(FleetWatch::with_manager(fleet.program.clone()));
    until("the second store reads the Tasks", || {
        names(&mut second).contains(&"vega".to_string())
    });
    std::thread::sleep(Duration::from_secs(2));
    second.refresh_if_due();
    assert_eq!(second.current().error, None);
    assert_eq!(fleet.manager(), first);
}
