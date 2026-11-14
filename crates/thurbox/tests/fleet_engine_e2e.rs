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
use thurbox::kernel::snapshot::{Deleting, SessionRow, SnapshotSource, SnapshotStore, SourceRead};

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
        assert!(
            std::env::var_os("CI").is_none(),
            "no fleet binary in CI, where the tests that drive Fleet must run"
        );
        eprintln!("skipping: no fleet binary; build it with `cargo build -p fleet-cli`");
    }
    found
}

/// Make `dir` a git repository with one commit. Git's location variables
/// are scrubbed, so a run under a commit hook never reaches the real one.
fn git_init(dir: &Path) {
    for args in [
        &["init", "-q", "-b", "main"][..],
        &[
            "-c",
            "user.email=t@example.com",
            "-c",
            "user.name=t",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            "init",
        ][..],
    ] {
        let mut git = Process::new("git");
        git.args(args).current_dir(dir);
        for var in [
            "GIT_DIR",
            "GIT_WORK_TREE",
            "GIT_INDEX_FILE",
            "GIT_OBJECT_DIRECTORY",
            "GIT_ALTERNATE_OBJECT_DIRECTORIES",
            "GIT_COMMON_DIR",
            "GIT_PREFIX",
            "GIT_NAMESPACE",
            "GIT_CONFIG_PARAMETERS",
        ] {
            git.env_remove(var);
        }
        assert!(git.status().expect("git").success(), "git {args:?}");
    }
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

    /// The same, from the config `fleet init` writes, on the fake runtime.
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

    /// Make `repo` a git repository, of which a session gets a worktree.
    fn map(&self, repo: &Path) {
        git_init(repo);
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
            "workspace": [format!("file:///work/{name}")],
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
    SnapshotStore::with_source(Box::new(source), &Default::default())
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
    assert_eq!(alpha.cwd.as_deref(), Some(Path::new("/work/alpha")));

    // The controller releases a deleted Task, then it leaves the store.
    fleet.run(&["delete", "task", "beta"]).ok();
    fleet.run(&["reconcile"]).ok();
    until("the deleted Task's row goes", || {
        names(&mut store) == ["alpha"]
    });
    assert_eq!(store.current().error, None);
}

/// A store `fleet watch` cannot read is the snapshot's error while the
/// watch runs, beside the last rows, and it clears once a read works again.
#[test]
fn an_unreadable_store_is_an_error_until_it_reads_again() {
    let Some(bin) = fleet_bin() else { return };
    let fleet = Fleet::new(bin);
    fleet.apply(json!([task("alpha")]));
    let mut store = store_over(&fleet);
    until("the Task is a row", || names(&mut store) == ["alpha"]);

    let junk = fleet.data().join("objects").join("Task").join("junk.txt");
    std::fs::write(&junk, "?").expect("junk");
    until("the error is published", || {
        store.refresh_if_due();
        store
            .current()
            .error
            .as_deref()
            .is_some_and(|e| e.contains("unexpected file"))
    });
    assert_eq!(names(&mut store), ["alpha"], "the last rows stand");

    std::fs::remove_file(&junk).expect("junk");
    until("the error clears", || {
        store.refresh_if_due();
        store.current().error.is_none()
    });
}

#[test]
fn a_fleet_that_will_not_answer_is_an_error_beside_the_last_rows() {
    let Some(bin) = fleet_bin() else { return };
    let fleet = Fleet::new(bin);
    let mut store = SnapshotStore::with_source(
        Box::new(FleetWatch::spawn(fleet.root.path().join("no-such-fleet"))),
        &Default::default(),
    );
    until("the error is published", || {
        store.refresh_if_due();
        store.current().error.is_some()
    });
    let error = store.current().error.clone().unwrap();
    assert!(error.contains("fleet watch"), "{error}");
}

fn fleet_bus(program: &Path) -> CommandBus {
    CommandBus::fleet(program.to_path_buf())
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
    fleet.map(&repo);
    let mut bus = fleet_bus(&fleet.program);

    // A session thurbox creates is an interactive Task: no brief, dispatched.
    run_command(
        &mut bus,
        Command::Create {
            name: "gamma".into(),
            repo: repo.to_string_lossy().into_owned(),
            agent: Some("claude-code".into()),
            host: None,
            worktree: None,
            branch: None,
            base: None,
            extras: Vec::new(),
        },
    )
    .expect("create");
    let gamma = fleet.get("gamma").expect("the Task");
    assert_eq!(gamma["spec"]["harness"], "claude-code");
    assert_eq!(gamma["spec"]["host"], "local");
    assert_eq!(gamma["spec"]["dispatch"], true);
    // A repository on this machine: the runtime makes the session a
    // worktree of its own, on a branch named after the session.
    assert_eq!(
        gamma["spec"]["workspace"],
        json!([format!("git+file://{}?branch=gamma", repo.display())])
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

/// A session keeps the name it was given, which is what a finished creation
/// finds its row by, while its Task is named as Fleet takes names.
#[test]
fn a_created_session_keeps_the_name_it_was_given() {
    let Some(bin) = fleet_bin() else { return };
    let fleet = Fleet::new(bin);
    fleet.map(fleet.root.path());
    let mut bus = fleet_bus(&fleet.program);
    run_command(
        &mut bus,
        Command::Create {
            name: "Read the code".into(),
            repo: fleet.root.path().to_string_lossy().into_owned(),
            agent: Some("claude-code".into()),
            host: None,
            worktree: None,
            branch: None,
            base: None,
            extras: Vec::new(),
        },
    )
    .expect("create");

    let mut store = store_over(&fleet);
    until("the row arrives", || {
        store.refresh_if_due();
        store.current().session("read-the-code").is_some()
    });
    assert_eq!(
        store.current().session("read-the-code").unwrap().name,
        "Read the code"
    );
}

/// Two sessions whose names make the same Task name are two sessions: the
/// second is refused, and the first is left as it was.
#[test]
fn a_name_already_taken_is_refused_and_the_task_left_alone() {
    let Some(bin) = fleet_bin() else { return };
    let fleet = Fleet::new(bin);
    fleet.map(fleet.root.path());
    let mut bus = fleet_bus(&fleet.program);
    let create = |name: &str| Command::Create {
        name: name.into(),
        repo: fleet.root.path().to_string_lossy().into_owned(),
        agent: Some("claude-code".into()),
        host: None,
        worktree: None,
        branch: None,
        base: None,
        extras: Vec::new(),
    };
    run_command(&mut bus, create("Review")).expect("the first");
    let refused = run_command(&mut bus, create("review")).expect_err("the same Task");
    assert!(refused.contains("already"), "{refused}");
    assert_eq!(
        fleet.get("review").unwrap()["metadata"]["labels"]["thurbox.dev/name"],
        "Review"
    );
}

/// A runtime's `tty` answer reaches the pane through `fleet term-bridge`,
/// which has no screen to send first: the pane starts on a full grid and
/// shows what follows. The `fleet` here answers `attach` itself and runs
/// the real bridge.
#[test]
fn a_bridged_stream_with_no_snapshot_shows_its_output() {
    let Some(bin) = fleet_bin() else { return };
    // For its isolated thurbox config and data; its store is not used.
    let _isolated = Fleet::new(bin.clone());
    let dir = tempfile::tempdir().expect("dir");
    let program = dir.path().join("fleet");
    let argv = json!([
        bin,
        "term-bridge",
        "--",
        "sh",
        "-c",
        "echo bridged-marker; exec cat"
    ]);
    std::fs::write(
        &program,
        format!("#!/bin/sh\necho '{}'\n", json!({"stream": {"argv": argv}})),
    )
    .expect("fleet");
    make_executable(&program);

    let (tx, rx) = std::sync::mpsc::channel();
    let mut store = SnapshotStore::with_source(Box::new(Fed(rx)), &Default::default());
    let mut bridged = row("bridged", "fleet", dir.path());
    bridged.key = Some("bridged.u1".into());
    tx.send(SourceRead {
        harnesses: None,
        hosts: None,
        sessions: vec![bridged],
        error: None,
        removed: Vec::new(),
    })
    .unwrap();
    let runtime = tokio::runtime::Runtime::new().expect("runtime");
    let _entered = runtime.enter();
    let mut terminals = thurbox::kernel::terminal::Terminals::with_registry(
        std::sync::Arc::new(thurbox::backend::wiring::configured(&Default::default()).0),
        &Default::default(),
    );
    terminals.attach_fleet_through(program);
    until("the pane shows the bridged output", || {
        store.refresh_if_due();
        terminals.sync(store.current(), 24, 80);
        if let Some(failure) = terminals.failure("bridged") {
            eprintln!("attach: {failure}");
        }
        terminals
            .visible_text("bridged")
            .is_some_and(|text| text.contains("bridged-marker"))
    });
}

/// A confirmed delete (the local control API's) is checked against the Task
/// as it is now: one that still matches what was confirmed is deleted, one
/// that moved is not.
#[test]
fn a_confirmed_delete_is_checked_then_deletes_the_task() {
    let Some(bin) = fleet_bin() else { return };
    let fleet = Fleet::new(bin);
    fleet.apply(json!([task("alpha"), task("beta")]));
    let mut bus = fleet_bus(&fleet.program);
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

    let moved = run_command(&mut bus, guarded("beta", "/work/elsewhere"))
        .expect_err("the target moved since it was confirmed");
    assert!(moved.contains("confirmation target changed"), "{moved}");
    assert!(fleet.get("beta").unwrap()["metadata"]["deletionTimestamp"].is_null());

    run_command(&mut bus, guarded("alpha", "/work/alpha")).expect("the confirmed delete");
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
        "spec": {"harness": "claude-code", "workspace": [format!("file://{}", cwd.display())]},
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
    let mut store = SnapshotStore::with_source(Box::new(Fed(rx)), &Default::default());
    let ticket = store.request_refresh();
    assert!(!store.fresh_since(ticket), "nothing was read since");
    tx.send(SourceRead {
        harnesses: None,
        hosts: None,
        sessions: vec![row("made", "fleet", Path::new("/w"))],
        error: None,
        removed: Vec::new(),
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

/// A session whose delete succeeded stays hidden while the source still
/// lists it, and a session of the same name made after the source dropped
/// it, or reported it removed, is shown again.
#[test]
fn a_deleted_session_stays_hidden_until_the_source_drops_it() {
    let (tx, rx) = std::sync::mpsc::channel();
    let mut store = SnapshotStore::with_source(Box::new(Fed(rx)), &Default::default());
    let read = |names: &[&str]| SourceRead {
        sessions: names
            .iter()
            .map(|name| row(name, "fleet", Path::new("/w")))
            .collect(),
        ..Default::default()
    };
    tx.send(read(&["kept", "gone"])).unwrap();
    store.refresh_if_due();
    store.hide_until_gone("gone".into());
    assert_eq!(names(&mut store), ["kept"]);

    tx.send(read(&["kept", "gone"])).unwrap();
    assert_eq!(names(&mut store), ["kept"], "a read from before the delete");
    tx.send(read(&["kept"])).unwrap();
    assert_eq!(names(&mut store), ["kept"]);
    tx.send(read(&["kept", "gone"])).unwrap();
    assert_eq!(names(&mut store), ["gone", "kept"], "made again since");

    // Removed and made again between two answers: the name never leaves the
    // rows, and the source says it went.
    store.hide_until_gone("gone".into());
    tx.send(SourceRead {
        removed: vec!["gone".into()],
        ..read(&["kept", "gone"])
    })
    .unwrap();
    assert_eq!(names(&mut store), ["gone", "kept"], "the new one shows");
}

/// Deleting a stuck deletion again changes nothing in Fleet, which reports
/// no removal for a Task already marked: its row stays listed as stuck rather
/// than hidden for the rest of the run.
#[test]
fn a_stuck_deletion_deleted_again_stays_listed() {
    let (tx, rx) = std::sync::mpsc::channel();
    let mut store = SnapshotStore::with_source(Box::new(Fed(rx)), &Default::default());
    let mut stuck = row("stuck", "fleet", Path::new("/w"));
    stuck.deleting = Some(Deleting {
        since_ms: 1,
        why: Some("waiting for the session to be at rest".into()),
    });
    let read = || SourceRead {
        sessions: vec![stuck.clone()],
        ..Default::default()
    };
    tx.send(read()).unwrap();
    assert_eq!(names(&mut store), ["stuck"]);

    store.hide_until_gone("stuck".into());
    tx.send(read()).unwrap();
    assert_eq!(names(&mut store), ["stuck"]);
}

/// Input a stream refused is said in the interface's own notices, not in
/// the pane's grid, which an agent's full-screen interface would paint
/// over. The stream: the real `fleet term-bridge` over a command that never
/// reads, whose terminal fills and then stops taking input.
#[test]
fn refused_input_is_a_notice_and_leaves_the_grid_alone() {
    use std::io::Write;
    let Some(bin) = fleet_bin() else { return };
    let argv: Vec<String> = [
        bin.to_string_lossy().as_ref(),
        "term-bridge",
        "--",
        "sh",
        "-c",
        "exec sleep 60",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    let (tx, notices) = std::sync::mpsc::channel();
    let mut adopted =
        thurbox::kernel::terminal::stream_pane::adopt_reporting(&argv, 24, 80, tx, "busy")
            .expect("the stream opens");
    let output = std::mem::replace(&mut adopted.output, Box::new(std::iter::empty()));
    let (chunks_tx, chunks) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        for chunk in output {
            if chunks_tx.send(format!("{chunk:?}")).is_err() {
                break;
            }
        }
    });
    let deadline = Instant::now() + BOUND;
    let notice = loop {
        adopted
            .input
            .write_all(&vec![b'x'; 256 << 10])
            .expect("a refusal is no error");
        if let Ok(notice) = notices.try_recv() {
            break notice;
        }
        assert!(Instant::now() < deadline, "no notice");
        std::thread::sleep(Duration::from_millis(20));
    };
    assert_eq!(notice, "input dropped: busy isn't reading");
    let drawn: Vec<String> = chunks.try_iter().collect();
    assert!(
        drawn.iter().all(|chunk| !chunk.contains("input dropped")),
        "the notice reached the grid: {drawn:?}"
    );
}

/// A creation is reported with its row: a refresh that awaits a created
/// session stays owed until the source publishes it, however late, and
/// not past a bound.
#[test]
fn a_refresh_awaiting_a_created_session_waits_for_its_row() {
    let (tx, rx) = std::sync::mpsc::channel();
    let mut store = SnapshotStore::with_source(Box::new(Fed(rx)), &Default::default());
    let ticket = store.request_refresh_awaiting(vec!["Made".into()]);
    // A watch that answers late: past the second a quiet source is given.
    std::thread::sleep(Duration::from_millis(2_000));
    store.refresh_if_due();
    assert!(!store.fresh_since(ticket), "fresh before the row arrived");
    let mut made = row("made", "fleet", Path::new("/w"));
    made.name = "Made".into();
    tx.send(SourceRead {
        harnesses: None,
        hosts: None,
        sessions: vec![made],
        error: None,
        removed: Vec::new(),
    })
    .unwrap();
    store.refresh_if_due();
    assert!(store.fresh_since(ticket));
    assert!(store.current().sessions.iter().any(|r| r.name == "Made"));
}

/// A repository with an untracked file: a diff or a git stat of it says so.
fn dirty_repo() -> tempfile::TempDir {
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
    repo
}

/// A Fleet Task on another machine names a path there, so no git runs
/// against it here; a local row beside it is examined as ever.
#[test]
fn a_fleet_row_elsewhere_is_not_examined_by_git() {
    let repo = dirty_repo();
    let mut elsewhere = row("on-fleet", "fleet", repo.path());
    elsewhere.remote_host = Some("devbox".into());
    let (tx, rx) = std::sync::mpsc::channel();
    let mut store = SnapshotStore::with_source(Box::new(Fed(rx)), &Default::default());
    tx.send(SourceRead {
        harnesses: None,
        hosts: None,
        sessions: vec![elsewhere, row("local", "tmux", repo.path())],
        error: None,
        removed: Vec::new(),
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

/// Nor is it diffed here: the diff is refused, and says where the Task is.
#[test]
fn a_fleet_row_elsewhere_is_not_diffed_here() {
    use thurbox::kernel::diff::{Diff, DiffStore};
    let repo = dirty_repo();
    let mut elsewhere = row("on-fleet", "fleet", repo.path());
    elsewhere.remote_host = Some("devbox".into());
    let mut diffs = DiffStore::new();
    diffs.request_row(&elsewhere);
    until("the diff is answered", || {
        diffs.poll();
        !matches!(diffs.get("on-fleet"), Some(Diff::Pending) | None)
    });
    match diffs.get("on-fleet") {
        Some(Diff::Failed(why)) => assert!(why.contains("devbox"), "{why}"),
        other => panic!("diffed here: {other:?}"),
    }
}

#[test]
fn thurbox_starts_fleet_up_when_no_manager_holds_the_lease() {
    let Some(bin) = fleet_bin() else { return };
    let fleet = Fleet::initialized(bin);
    assert_eq!(fleet.manager(), None);

    fleet.apply(json!([task("alpha")]));
    let mut store = store_of(FleetWatch::with_manager(fleet.program.clone()));
    // The `fleet up` thurbox starts reconciles the Tasks it finds.
    until("a manager holds the lease", || fleet.manager().is_some());
    until("the Task is a row", || {
        names(&mut store).contains(&"alpha".to_string())
    });
    let first = fleet.manager();
    let phase = || fleet.get("alpha").map(|t| t["status"]["phase"].clone());
    until("the manager reconciles", || {
        phase().is_some_and(|p| p.is_string())
    });

    // A second interface finds the lease held, and leaves that manager be.
    let mut second = store_of(FleetWatch::with_manager(fleet.program.clone()));
    until("the second store reads the Tasks", || {
        names(&mut second).contains(&"alpha".to_string())
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
    // runtime-mux prepares the Task's `file://` workspace with the
    // workspace-git beside it.
    if !runtime_mux.is_file() || !bin.with_file_name("workspace-git").is_file() {
        assert!(
            std::env::var_os("CI").is_none(),
            "no runtime-mux or workspace-git binary beside fleet in CI, where this test must run"
        );
        eprintln!(
            "skipping: build runtime-mux and workspace-git (`cargo build -p runtime-mux -p workspace-git`)"
        );
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
            "[runtimes.mux]\nprogram = '{}'\ncallTimeoutMs = 10000\n",
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
    t1["spec"]["workspace"] = json!([format!("file://{}", work.display())]);
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

/// An interactive session's row follows what its agent's hooks report:
/// `fleet signal idle` makes it idle, `working` working, and idle again.
#[test]
fn an_interactive_rows_status_follows_its_agents_hooks() {
    use thurbox::session::SessionState;
    let Some(bin) = fleet_bin() else { return };
    let fleet = Fleet::new(bin.clone());
    let mut t = task("alpha");
    t["spec"].as_object_mut().unwrap().remove("brief");
    t["spec"]["dispatch"] = json!(true);
    fleet.apply(json!([t]));
    fleet.run(&["reconcile"]).ok();
    let key = fleet.get("alpha").expect("the Task")["status"]["session"]["key"]
        .as_str()
        .expect("a session key")
        .to_string();
    let file = fleet.data().join("signals").join(format!("{key}.jsonl"));
    let signal = |state: &str| {
        let ran = Process::new(&bin)
            .args(["signal", state])
            .env("FLEET_SIGNAL_FILE", &file)
            .stdin(std::process::Stdio::null())
            .status()
            .expect("fleet signal");
        assert!(ran.success());
        fleet.run(&["reconcile"]).ok();
    };
    let mut store = store_over(&fleet);
    let status = |store: &mut SnapshotStore| {
        store.refresh_if_due();
        store.current().session("alpha").map(|row| row.status)
    };
    signal("idle");
    until("the row is idle", || {
        status(&mut store) == Some(SessionState::Idle)
    });
    signal("working");
    until("the row works", || {
        status(&mut store) == Some(SessionState::Working)
    });
    signal("idle");
    until("the row is idle again", || {
        status(&mut store) == Some(SessionState::Idle)
    });
}

/// The row shows the agent's finer state the way v2 did: working, then
/// waiting on a person (blocked, v2's red diamond), then idle.
#[test]
fn an_interactive_rows_status_shows_working_attention_and_idle() {
    use thurbox::session::SessionState;
    let Some(bin) = fleet_bin() else { return };
    let fleet = Fleet::new(bin.clone());
    let mut t = task("alpha");
    t["spec"].as_object_mut().unwrap().remove("brief");
    t["spec"]["dispatch"] = json!(true);
    fleet.apply(json!([t]));
    fleet.run(&["reconcile"]).ok();
    let key = fleet.get("alpha").expect("the Task")["status"]["session"]["key"]
        .as_str()
        .expect("a session key")
        .to_string();
    let file = fleet.data().join("signals").join(format!("{key}.jsonl"));
    let signal = |state: &str| {
        let ran = Process::new(&bin)
            .args(["signal", state])
            .env("FLEET_SIGNAL_FILE", &file)
            .stdin(std::process::Stdio::null())
            .status()
            .expect("fleet signal");
        assert!(ran.success());
        fleet.run(&["reconcile"]).ok();
    };
    let mut store = store_over(&fleet);
    let status = |store: &mut SnapshotStore| {
        store.refresh_if_due();
        store.current().session("alpha").map(|row| row.status)
    };
    signal("working");
    until("the row works", || {
        status(&mut store) == Some(SessionState::Working)
    });
    signal("blocked");
    until("the row waits on a person", || {
        status(&mut store) == Some(SessionState::Blocked)
    });
    signal("idle");
    until("the row is idle", || {
        status(&mut store) == Some(SessionState::Idle)
    });
}

/// The link reaches a Fleet over ssh, though no config selects one yet: the
/// real binary behind a stand-in `ssh` that logs each remote command line and
/// runs it, as `tests/tui_e2e.rs`'s does. A Task goes in on stdin, free text
/// arrives as the one word it was, a JSON answer reads back, and Fleet's exit
/// 3 (no such Task) survives the hop.
#[test]
fn the_link_reaches_a_fleet_over_ssh() {
    use thurbox::fleet_link::FleetLink;
    use thurbox::shell::HostLauncher;
    let Some(bin) = fleet_bin() else { return };
    let fleet = Fleet::new(bin);
    let root = fleet.root.path();
    let ssh_bin = root.join("ssh-bin");
    std::fs::create_dir_all(&ssh_bin).expect("ssh-bin");
    let log = root.join("ssh.log");
    let ssh = ssh_bin.join("ssh");
    std::fs::write(
        &ssh,
        format!(
            "#!/bin/sh\n\
             while [ $# -gt 0 ]; do case \"$1\" in -o) shift 2 ;; *) break ;; esac; done\n\
             [ \"$1\" = devbox ] || exit 255\n\
             shift\n\
             printf '%s\\n' \"$*\" >> '{log}'\n\
             eval \"exec $*\"\n",
            log = log.display()
        ),
    )
    .expect("ssh");
    make_executable(&ssh);
    let path = std::env::var_os("PATH").unwrap_or_default();
    let mut dirs = vec![ssh_bin];
    dirs.extend(std::env::split_paths(&path));
    std::env::set_var("PATH", std::env::join_paths(dirs).expect("PATH"));

    let link = FleetLink::over(
        HostLauncher::Ssh {
            destination: "devbox".into(),
            ssh_opts: Vec::new(),
        },
        fleet.program.to_string_lossy(),
    );
    link.run(
        &["apply", "--create-only", "-f", "-"],
        Some(&task("remote").to_string()),
    )
    .expect("apply over ssh");
    let text = "it's \"done\"; $HOME `x` | y";
    link.run(&["send", "remote", text], None)
        .expect("send over ssh");
    let got = link
        .command(&["get", "task", "remote"])
        .stdin(Stdio::null())
        .output()
        .expect("get over ssh");
    assert_eq!(got.status.code(), Some(0));
    let task: Value = serde_json::from_slice(&got.stdout).expect("JSON over ssh");
    assert_eq!(task["status"]["inbox"][0]["text"], text, "{task:#}");
    let gone = link
        .command(&["get", "task", "absent"])
        .stdin(Stdio::null())
        .output()
        .expect("get over ssh");
    assert_eq!(gone.status.code(), Some(3), "not found, through ssh");
    let calls = std::fs::read_to_string(&log).expect("ssh log");
    assert_eq!(calls.lines().count(), 4, "{calls}");
    assert!(
        calls
            .lines()
            .all(|call| call.starts_with(&*fleet.program.to_string_lossy())),
        "{calls}"
    );
}
