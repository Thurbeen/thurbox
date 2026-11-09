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
use thurbox::kernel::snapshot::{SessionRow, SnapshotSource, SnapshotStore, SourceRead};

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
        Fleet::with(bin, "[runtime]\nkind = \"fake\"\ndir = \"fake\"\n", "")
    }

    /// A Fleet whose runtime is `runtime` (TOML), its program run with
    /// `env` (`NAME='value' …`) and `<root>/bin` first on its `PATH`.
    fn with(bin: PathBuf, runtime: &str, env: &str) -> Fleet {
        let root = tempfile::Builder::new()
            .prefix("fleet-engine-")
            .tempdir_in(env!("CARGO_TARGET_TMPDIR"))
            .expect("root");
        let r = root.path();
        std::fs::create_dir_all(r.join("tmp")).expect("tmp");
        // thurbox's own config and data too, so the registry it reads (and
        // seeds when absent) is the test's. Each test is a process of its own.
        for (var, dir) in [
            ("THURBOX_CONFIG_DIR", "thurbox-config"),
            ("THURBOX_DATA_DIR", "thurbox-data"),
        ] {
            std::fs::create_dir_all(r.join(dir)).expect("thurbox dir");
            std::env::set_var(var, r.join(dir));
        }
        std::fs::write(
            r.join("fleet.toml"),
            format!("version = 1\n\n[data]\ndir = \"data\"\n\n{runtime}"),
        )
        .expect("config");
        let program = r.join("fleet.sh");
        std::fs::write(
            &program,
            format!(
                "#!/bin/sh\nexec env -i HOME='{r}' TMPDIR='{r}/tmp' PATH='{r}/bin:/usr/bin:/bin' {env} '{bin}' --config '{r}/fleet.toml' \"$@\"\n",
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

/// A confirmed delete (the local control API's) is checked against the Task
/// as it is now: one that still matches what was confirmed is deleted, one
/// that moved is not.
#[test]
fn a_confirmed_delete_is_checked_then_deletes_the_task() {
    let Some(bin) = fleet_bin() else { return };
    let fleet = Fleet::new(bin);
    fleet.apply(json!([task("alpha"), task("beta")]));
    let mut bus = CommandBus::fleet(fleet.program.clone());
    let guarded = |session: &str, cwd: &str| Command::Guarded {
        inner: Box::new(Command::Delete {
            session: session.into(),
            force: true,
        }),
        session: session.into(),
        backend_id: None,
        cwd: Some(PathBuf::from(cwd)),
        member_dirs: vec![PathBuf::from(cwd)],
    };

    let moved = run_command(&mut bus, guarded("beta", "work/elsewhere"))
        .expect_err("the target moved since it was confirmed");
    assert!(moved.contains("confirmation target changed"), "{moved}");
    assert!(fleet.get("beta").unwrap()["metadata"]["deletionTimestamp"].is_null());

    run_command(&mut bus, guarded("alpha", "work/alpha")).expect("the confirmed delete");
    assert!(fleet.get("alpha").unwrap()["metadata"]["deletionTimestamp"].is_u64());
}

/// A source the test feeds by hand.
struct Fed(std::sync::mpsc::Receiver<SourceRead>);

impl SnapshotSource for Fed {
    fn poll(&mut self) -> Option<SourceRead> {
        self.0.try_recv().ok()
    }
}

fn row(name: &str, backend: &str, cwd: &Path) -> SessionRow {
    let mut row = thurbox::kernel::snapshot::fleet::row_of(&json!({
        "metadata": {"name": name},
        "spec": {"harness": "claude-code", "workspace": {"dir": cwd}},
    }));
    row.backend = backend.into();
    row
}

/// A refresh asked of a source is fresh once the source answers after it
/// (or, since a source speaks only when something moved, once it had time
/// to): a creation is reported with the row it made, not before it.
#[test]
fn a_refresh_ticket_waits_for_the_source_to_answer() {
    let (tx, rx) = std::sync::mpsc::channel();
    let mut store = SnapshotStore::with_source(
        Box::new(Fed(rx)),
        &thurbox::backend::wiring::configured(&Default::default()).0,
        &Default::default(),
    );
    let ticket = store.request_refresh();
    assert!(!store.fresh_since(ticket), "nothing was read since");
    tx.send(SourceRead {
        sessions: vec![row("made", "fleet", Path::new("/w"))],
        error: None,
    })
    .unwrap();
    store.refresh_if_due();
    assert!(store.fresh_since(ticket));
    assert!(store.current().session("made").is_some());

    let quiet = store.request_refresh();
    assert!(!store.fresh_since(quiet));
    std::thread::sleep(Duration::from_secs(2));
    assert!(store.fresh_since(quiet), "a source that has nothing to say");
}

/// A Fleet row's workspace may be on another machine, so no git runs
/// against it here; a local row beside it is examined as ever.
#[test]
fn a_fleet_row_is_not_examined_by_git() {
    let repo = tempfile::tempdir().expect("repo");
    let git = |args: &[&str]| {
        let mut command = Process::new("git");
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
            command.env_remove(var);
        }
        let status = command
            .args(args)
            .current_dir(repo.path())
            .status()
            .expect("git");
        assert!(status.success(), "git {args:?}");
    };
    git(&["init", "-q"]);
    std::fs::write(repo.path().join("file"), "x").unwrap();

    let (tx, rx) = std::sync::mpsc::channel();
    let mut store = SnapshotStore::with_source(
        Box::new(Fed(rx)),
        &thurbox::backend::wiring::configured(&Default::default()).0,
        &Default::default(),
    );
    tx.send(SourceRead {
        sessions: vec![
            row("on-fleet", "fleet", repo.path()),
            row("local", "tmux", repo.path()),
        ],
        error: None,
    })
    .unwrap();
    until("the local row has its git state", || {
        store.refresh_if_due();
        store
            .current()
            .session("local")
            .is_some_and(|r| r.git.is_some())
    });
    assert_eq!(store.current().session("on-fleet").unwrap().git, None);
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

/// A private `TMUX_TMPDIR`, short enough for a socket path, whose server dies
/// with it.
struct Sockets(PathBuf);

impl Sockets {
    fn new() -> Sockets {
        let base = std::env::var_os("XDG_RUNTIME_DIR")
            .map(PathBuf::from)
            .filter(|d| d.is_dir())
            .unwrap_or_else(std::env::temp_dir);
        let dir = base.join(format!("tfp-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("socket dir");
        Sockets(dir)
    }
}

impl Drop for Sockets {
    fn drop(&mut self) {
        let _ = Process::new("tmux")
            .env("TMUX_TMPDIR", &self.0)
            .env_remove("TMUX")
            .args(["-L", "runtime-mux", "kill-server"])
            .output();
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn a_fleet_session_has_a_terminal_pane_through_fleet_attach() {
    let Some(bin) = fleet_bin() else { return };
    let runtime_mux = bin.with_file_name("runtime-mux");
    if !runtime_mux.is_file() {
        eprintln!("skipping: no runtime-mux binary; build it with `cargo build -p runtime-mux`");
        return;
    }
    if thurbox::paths::resolve_on_path("tmux").is_none() {
        eprintln!("skipping: tmux is not installed");
        return;
    }
    let sockets = Sockets::new();
    let fleet = Fleet::with(
        bin,
        &format!(
            "[runtimes.mux]\nprogram = '{}'\ncallTimeoutMs = 10000\n\n[hosts.local]\nruntime = \"mux\"\n",
            runtime_mux.display()
        ),
        &format!("TMUX_TMPDIR='{}'", sockets.0.display()),
    );
    // The harness's program: it prints a marker, then answers each line.
    let harness_dir = fleet.root.path().join("bin");
    std::fs::create_dir_all(&harness_dir).expect("bin");
    let harness = harness_dir.join("claude");
    std::fs::write(
        &harness,
        "#!/bin/sh\necho ready-marker\nwhile IFS= read -r l; do echo \"pong-$l\"; done\n",
    )
    .expect("harness");
    make_executable(&harness);
    let work = fleet.root.path().join("work");
    std::fs::create_dir_all(&work).expect("work");
    let mut t1 = task("t1");
    t1["spec"]["workspace"] = json!({"dir": work});
    t1["spec"]["dispatch"] = json!(true);
    fleet.apply(t1);
    fleet.run(&["reconcile"]).ok();

    let mut store = store_over(&fleet);
    until("the Task has its session", || {
        store.refresh_if_due();
        store
            .current()
            .session("t1")
            .is_some_and(|row| row.key.is_some())
    });
    // A pane's reader and writer run on the process's runtime, as in the
    // interface.
    let runtime = tokio::runtime::Runtime::new().expect("runtime");
    let _entered = runtime.enter();
    let mut terminals = thurbox::kernel::terminal::Terminals::with_registry(
        std::sync::Arc::new(thurbox::backend::wiring::configured(&Default::default()).0),
        &Default::default(),
    );
    terminals.attach_fleet_through(fleet.program.clone());
    let mut screen = |terminals: &mut thurbox::kernel::terminal::Terminals| {
        store.refresh_if_due();
        terminals.sync(store.current(), 24, 80);
        if let Some(failure) = terminals.failure("t1") {
            eprintln!("attach: {failure}");
        }
        terminals.visible_text("t1").unwrap_or_default()
    };
    until("the pane shows the agent's screen", || {
        screen(&mut terminals).contains("ready-marker")
    });
    assert!(
        terminals.send("t1", b"ping\r".to_vec()),
        "the pane takes input"
    );
    until("the agent answers what was typed", || {
        screen(&mut terminals).contains("pong-ping")
    });
}
