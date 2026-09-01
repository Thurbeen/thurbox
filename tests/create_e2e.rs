//! Creating a session end to end, against a real git repository.
//!
//! This is the one test that runs the whole pipeline for real: a repo on disk,
//! a worktree, a tmux window, a process. It is skipped when tmux is absent
//! rather than failing, because a missing multiplexer is an environment fact,
//! not a regression — but it *runs* wherever tmux exists, including CI.
//!
//! Everything it touches is scoped to a throwaway socket and a temporary
//! directory, so it can never disturb a real session.

use std::path::Path;
use std::process::Command;

/// A throwaway tmux socket, so this never touches the real one.
const SOCKET: &str = "thurbox-create-e2e";

fn have_tmux() -> bool {
    Command::new("tmux")
        .arg("-V")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn git(dir: &Path, args: &[&str]) {
    let ok = Command::new("git")
        .args(args)
        .current_dir(dir)
        // Scrubbed so an inherited GIT_* var cannot reach into this repo.
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .output()
        .expect("run git")
        .status
        .success();
    assert!(ok, "git {args:?} failed");
}

/// A repository with one commit, which is the minimum a worktree needs.
fn repo() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    git(dir.path(), &["init", "-q", "-b", "main"]);
    git(dir.path(), &["config", "user.email", "t@example.com"]);
    git(dir.path(), &["config", "user.name", "thurbox-test"]);
    // Commit signing is a user setting that fails in a bare environment, and
    // this repo is not the place to be signing anything.
    git(dir.path(), &["config", "commit.gpgsign", "false"]);
    std::fs::write(dir.path().join("README.md"), "# probe\n").expect("write");
    git(dir.path(), &["add", "."]);
    // Signing would make this depend on a key in the user's agent; the repo is
    // throwaway, so it is disabled here rather than required of the machine.
    git(dir.path(), &["config", "commit.gpgsign", "false"]);
    git(dir.path(), &["commit", "-qm", "init"]);
    dir
}

/// Point the spawn at a private socket in a private directory, so it can never
/// see — or race — the shared dev server (`thurbox-dev`). Without this the
/// pipeline lands on the real socket: the "throwaway socket" was aspirational,
/// `cleanup` killed a server nothing used, and the spawned windows leaked into
/// (and interfered with) whatever else ran there. nextest runs one process per
/// test, so env mutation is safe. Returns the tempdir so it outlives the test.
fn isolate_tmux() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    std::env::set_var("TMUX_TMPDIR", dir.path());
    std::env::set_var(thurbox::agent::tmux::SOCKET_OVERRIDE_ENV, SOCKET);
    dir
}

fn cleanup() {
    let _ = Command::new("tmux")
        .args(["-L", SOCKET, "kill-server"])
        .output();
}

#[test]
fn creating_a_session_produces_a_worktree_a_row_and_a_window() {
    if !have_tmux() {
        eprintln!("skipping: tmux is not installed");
        return;
    }

    let repo = repo();
    let db = thurbox::storage::Database::open_in_memory().expect("db");
    let _tmux_dir = isolate_tmux();

    // Isolate config and data, so this uses neither the real agents.toml nor
    // the real database. nextest runs each test in its own process, so a
    // process-wide path override is safe here.
    let home = tempfile::tempdir().expect("tempdir");
    thurbox::paths::set_test_dir(home.path());

    // A shell rather than a real agent: the pipeline is what is under test, and
    // launching a coding agent would want credentials and a network.
    let config = thurbox::paths::config_file()
        .expect("config path")
        .parent()
        .expect("config dir")
        .to_path_buf();
    std::fs::create_dir_all(&config).expect("mkdir");
    std::fs::write(
        config.join("agents.toml"),
        "default = \"shell\"\n\n[[agents]]\nname = \"shell\"\ncommand = \"sh\"\nargs = []\n",
    )
    .expect("write agents.toml");

    let result = thurbox::session_ops::spawn::spawn_session_headless(
        &db,
        thurbox::session_ops::spawn::SpawnRequest {
            name: "e2e-probe".into(),
            repo_path: repo.path().to_path_buf(),
            worktree_branch: Some("feat/e2e".into()),
            base_branch: Some("main".into()),
            agent: Some("shell".into()),
            command: None,
            args: Vec::new(),
            env: Default::default(),
            resume_session_id: None,
            agent_session_id: None,
            host: None,
            parent_session_id: None,
            task_id: None,
            extra_repos: Vec::new(),
            fork_session_id: None,
            inherit_worktrees: Vec::new(),
        },
    );

    let spawned = match result {
        Ok(spawned) => spawned,
        Err(e) => {
            cleanup();
            // A tmux server that will not start is an environment problem.
            if e.contains("tmux") {
                eprintln!("skipping: tmux would not spawn a window: {e}");
                return;
            }
            panic!("creation failed: {e}");
        }
    };

    // The row exists, and carries what a plugin needs to draw it.
    let row = db
        .get_session_by_id(spawned.session_id)
        .expect("query")
        .expect("the session should be persisted");
    assert_eq!(row.name, "e2e-probe");
    assert_eq!(row.agent, "shell");

    // The local spawn learned its pane id up front (`new-window -P`), so the
    // row never depends on its window name — which is not unique.
    #[cfg(not(windows))]
    {
        assert!(
            spawned.backend_id.starts_with('%'),
            "expected a pane id, got {:?}",
            spawned.backend_id
        );
        assert_eq!(row.backend_id, spawned.backend_id);
    }

    // The worktree exists on disk, on the branch that was asked for.
    let worktree = row
        .worktrees
        .first()
        .expect("a branch was requested, so there must be a worktree");
    assert_eq!(worktree.branch, "feat/e2e");
    assert!(
        worktree.worktree_path.is_dir(),
        "{} is not a directory",
        worktree.worktree_path.display()
    );
    assert!(
        worktree.worktree_path.join("README.md").is_file(),
        "the worktree should carry the repo's content"
    );

    // And the snapshot the kernel publishes sees it.
    let store = thurbox::kernel::snapshot::SnapshotStore::with_database(db);
    let published = store
        .current()
        .sessions
        .iter()
        .find(|s| s.name == "e2e-probe")
        .expect("the new session should reach the snapshot");
    assert_eq!(published.branch.as_deref(), Some("feat/e2e"));

    cleanup();
}

/// Two sessions sharing a name — the state accepting the creation flow's
/// proposed default twice produces. Each spawn must learn its *own* pane id,
/// or the second one can never attach (their windows share the `tb-` name,
/// which the interface refuses to guess between).
#[test]
#[cfg(not(windows))]
fn two_sessions_sharing_a_name_get_distinct_pane_ids() {
    if !have_tmux() {
        eprintln!("skipping: tmux is not installed");
        return;
    }

    let repo = repo();
    let db = thurbox::storage::Database::open_in_memory().expect("db");
    let _tmux_dir = isolate_tmux();
    let home = tempfile::tempdir().expect("tempdir");
    thurbox::paths::set_test_dir(home.path());
    let config = thurbox::paths::config_file()
        .expect("config path")
        .parent()
        .expect("config dir")
        .to_path_buf();
    std::fs::create_dir_all(&config).expect("mkdir");
    std::fs::write(
        config.join("agents.toml"),
        "default = \"shell\"\n\n[[agents]]\nname = \"shell\"\ncommand = \"sh\"\nargs = []\n",
    )
    .expect("write agents.toml");

    // No worktree: the duplicate-default repro is the plain-directory path (a
    // repeated branch would fail loudly long before the window spawns).
    let request = || thurbox::session_ops::spawn::SpawnRequest {
        name: "twin".into(),
        repo_path: repo.path().to_path_buf(),
        agent: Some("shell".into()),
        ..Default::default()
    };

    let first = match thurbox::session_ops::spawn::spawn_session_headless(&db, request()) {
        Ok(spawned) => spawned,
        Err(e) => {
            cleanup();
            if e.contains("tmux") {
                eprintln!("skipping: tmux would not spawn a window: {e}");
                return;
            }
            panic!("first creation failed: {e}");
        }
    };
    let second = thurbox::session_ops::spawn::spawn_session_headless(&db, request())
        .expect("second creation");
    cleanup();

    assert!(first.backend_id.starts_with('%'), "{:?}", first.backend_id);
    assert!(
        second.backend_id.starts_with('%'),
        "{:?}",
        second.backend_id
    );
    assert_ne!(
        first.backend_id, second.backend_id,
        "each session must be addressable by its own pane"
    );
}

/// Regression: a resume brings in an *existing* conversation id from outside
/// thurbox — "the checkout comes in as a path, the conversation as this id"
/// (`session_ops/mod.rs`). For an agent that pins a specific conversation id
/// rather than "resume whatever's latest" (`resume_latest = false`, with
/// `resume_args` to emit), the persisted `agent_session_id` must be that same
/// id, not a freshly minted UUID — otherwise the very next `restart` looks for
/// a transcript under the wrong id and silently starts a brand-new
/// conversation instead of the one that arrived.
#[test]
#[cfg(not(windows))]
fn resuming_an_id_pinned_agent_persists_the_resumed_id() {
    if !have_tmux() {
        eprintln!("skipping: tmux is not installed");
        return;
    }

    let repo = repo();
    let db = thurbox::storage::Database::open_in_memory().expect("db");
    let _tmux_dir = isolate_tmux();
    let home = tempfile::tempdir().expect("tempdir");
    thurbox::paths::set_test_dir(home.path());
    let config = thurbox::paths::config_file()
        .expect("config path")
        .parent()
        .expect("config dir")
        .to_path_buf();
    std::fs::create_dir_all(&config).expect("mkdir");
    // Claude-like: it pins a conversation id via `{id}` rather than resuming
    // "latest", which is what makes `resumes_latest()` false and puts it on
    // the id-persisting path under test.
    std::fs::write(
        config.join("agents.toml"),
        "default = \"resumable\"\n\n\
         [[agents]]\n\
         name = \"resumable\"\n\
         command = \"sh\"\n\
         args = []\n\
         resume_args = [\"-c\", \"true {id}\"]\n\
         resume_latest = false\n",
    )
    .expect("write agents.toml");

    let external_conversation_id = "external-conv-1234";
    let result = thurbox::session_ops::spawn::spawn_session_headless(
        &db,
        thurbox::session_ops::spawn::SpawnRequest {
            name: "arrived".into(),
            repo_path: repo.path().to_path_buf(),
            agent: Some("resumable".into()),
            resume_session_id: Some(external_conversation_id.into()),
            ..Default::default()
        },
    );

    let spawned = match result {
        Ok(spawned) => spawned,
        Err(e) => {
            cleanup();
            if e.contains("tmux") {
                eprintln!("skipping: tmux would not spawn a window: {e}");
                return;
            }
            panic!("creation failed: {e}");
        }
    };

    assert_eq!(
        spawned.agent_session_id, external_conversation_id,
        "the reported agent_session_id must be the resumed conversation, not a fresh uuid"
    );
    let persisted = db
        .get_session_by_id(spawned.session_id)
        .expect("query")
        .expect("session persisted")
        .agent_session_id;
    cleanup();
    assert_eq!(
        persisted.as_deref(),
        Some(external_conversation_id),
        "the persisted row must carry the resumed id, or a later restart can't find its transcript"
    );
}

// ---------------------------------------------------------------------------
// Session lifecycle hooks (`hooks.toml`), fired around the same pipeline.
// ---------------------------------------------------------------------------

/// Isolate config + data under a fresh home, install a `sh` agent, and return
/// the config directory. Every hooks test below starts here; the process-wide
/// override is safe because nextest runs one process per test.
#[cfg(unix)]
fn isolated_config() -> (tempfile::TempDir, std::path::PathBuf) {
    let home = tempfile::tempdir().expect("tempdir");
    // Both forms: the thread-local override for this thread, and the
    // process-wide env overrides for any thread the pipeline spawns — the
    // command bus runs each command on its own, and a worker resolving the
    // real XDG paths would create a real session in the real database.
    thurbox::paths::set_test_dir(home.path());
    std::env::set_var(thurbox::paths::CONFIG_DIR_OVERRIDE_ENV, home.path());
    std::env::set_var(thurbox::paths::DATA_DIR_OVERRIDE_ENV, home.path());
    let config = thurbox::paths::config_file()
        .expect("config path")
        .parent()
        .expect("config dir")
        .to_path_buf();
    std::fs::create_dir_all(&config).expect("mkdir");
    std::fs::write(
        config.join("agents.toml"),
        "default = \"shell\"\n\n[[agents]]\nname = \"shell\"\ncommand = \"sh\"\nargs = []\n",
    )
    .expect("write agents.toml");
    (home, config)
}

/// The database at the path a `thurbox-cli` run *inside* a hook resolves —
/// the same file, so what the hook reads is what the pipeline wrote.
#[cfg(unix)]
fn on_disk_db() -> thurbox::storage::Database {
    let path = thurbox::paths::database_file().expect("db path");
    std::fs::create_dir_all(path.parent().expect("data dir")).expect("mkdir");
    thurbox::storage::Database::open(&path).expect("open db")
}

#[cfg(unix)]
fn write_hooks(config: &Path, body: &str) {
    std::fs::write(config.join("hooks.toml"), body).expect("write hooks.toml");
}

/// A hook entry that appends `$THURBOX_HOOK_EVENT` (and, for the create pair,
/// the paths) to `log`.
#[cfg(unix)]
fn logging_hook(event: &str, log: &Path) -> String {
    format!(
        "[[hooks]]\nevent = \"{event}\"\ncommand = 'echo \"$THURBOX_HOOK_EVENT ${{THURBOX_CWD:-unset}} ${{THURBOX_REPO:-unset}} ${{THURBOX_SESSION:-unset}}\" >> {}'\n\n",
        log.display()
    )
}

#[cfg(unix)]
fn shell_request(repo: &Path, branch: Option<&str>) -> thurbox::session_ops::spawn::SpawnRequest {
    thurbox::session_ops::spawn::SpawnRequest {
        name: "hooked".into(),
        repo_path: repo.to_path_buf(),
        worktree_branch: branch.map(String::from),
        base_branch: branch.map(|_| "main".to_string()),
        agent: Some("shell".into()),
        ..Default::default()
    }
}

#[test]
#[cfg(unix)]
fn create_hooks_fire_once_each_with_the_facts_and_can_reach_the_database() {
    if !have_tmux() {
        eprintln!("skipping: tmux is not installed");
        return;
    }
    let repo = repo();
    let _tmux_dir = isolate_tmux();
    let (home, config) = isolated_config();
    let db = on_disk_db();
    let log = home.path().join("hooks.log");
    let seen_by_cli = home.path().join("session.json");
    // The post hook asks thurbox-cli about the session it was told of — the
    // dev binary, by absolute path, so PATH plays no part.
    let mut hooks = logging_hook("session.pre_create", &log);
    hooks.push_str(&logging_hook("session.post_create", &log));
    hooks.push_str(&format!(
        "[[hooks]]\nevent = \"session.post_create\"\ncommand = '{} session get \"$THURBOX_SESSION\" --json > {}'\n",
        env!("CARGO_BIN_EXE_thurbox-cli"),
        seen_by_cli.display()
    ));
    write_hooks(&config, &hooks);

    let spawned = match thurbox::session_ops::spawn::spawn_session_headless(
        &db,
        shell_request(repo.path(), Some("feat/hooked")),
    ) {
        Ok(spawned) => spawned,
        Err(e) => {
            cleanup();
            if e.contains("tmux") {
                eprintln!("skipping: tmux would not spawn a window: {e}");
                return;
            }
            panic!("creation failed: {e}");
        }
    };
    cleanup();

    assert!(
        spawned.hook_failures.is_empty(),
        "{:?}",
        spawned.hook_failures
    );
    let seen = std::fs::read_to_string(&log).expect("the hooks ran");
    let lines: Vec<Vec<&str>> = seen
        .lines()
        .map(|l| l.split_whitespace().collect())
        .collect();
    assert_eq!(lines.len(), 2, "one pre, one post:\n{seen}");
    let sid = spawned.session_id.to_string();
    let worktree = spawned.worktrees[0].worktree_path.display().to_string();
    let repo_path = repo.path().display().to_string();
    assert_eq!(lines[0], ["session.pre_create", "unset", &repo_path, &sid]);
    assert_eq!(
        lines[1],
        ["session.post_create", &worktree, &repo_path, &sid]
    );

    let cli = std::fs::read_to_string(&seen_by_cli).expect("thurbox-cli ran inside the hook");
    assert!(
        cli.contains(&sid),
        "the hook's thurbox-cli must see the row the pipeline wrote: {cli}"
    );
}

#[test]
#[cfg(unix)]
fn a_pre_create_veto_leaves_nothing_behind() {
    // No tmux needed: the veto lands before anything is spawned, and that is
    // the point — so this runs everywhere.
    use std::sync::{Arc, Mutex};
    let repo = repo();
    let _tmux_dir = isolate_tmux();
    let (_home, config) = isolated_config();
    let db = on_disk_db();
    write_hooks(
        &config,
        "[[hooks]]\nevent = \"session.pre_create\"\ncommand = 'echo \"refusing: protected branch\" >&2; exit 1'\n\
         [[hooks]]\nevent = \"session.post_create\"\ncommand = 'touch post-ran'\n",
    );

    let phases: Arc<Mutex<Vec<&'static str>>> = Arc::new(Mutex::new(Vec::new()));
    let recorder = {
        let phases = phases.clone();
        move |phase: thurbox::session_ops::spawn::SpawnPhase| {
            phases.lock().unwrap().push(phase.as_str());
        }
    };
    let err = thurbox::session_ops::spawn::spawn_session_headless_with_progress(
        &db,
        shell_request(repo.path(), Some("feat/vetoed")),
        Some(&recorder),
    )
    .expect_err("a vetoed creation fails");

    assert!(err.contains("refusing: protected branch"), "{err}");
    assert!(err.contains("session.pre_create"), "{err}");
    assert_eq!(*phases.lock().unwrap(), ["resolving", "hooks"]);

    // Nothing happened: no row, no worktree, no window, no post hook.
    let store = thurbox::kernel::snapshot::SnapshotStore::with_database(db);
    assert!(store.current().sessions.is_empty());
    let worktrees = Command::new("git")
        .args(["worktree", "list", "--porcelain"])
        .current_dir(repo.path())
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .output()
        .expect("git");
    let listed = String::from_utf8_lossy(&worktrees.stdout);
    assert_eq!(
        listed.matches("worktree ").count(),
        1,
        "only the main checkout:\n{listed}"
    );
    assert!(!repo.path().join("post-ran").exists());
    let server = Command::new("tmux")
        .args(["-L", SOCKET, "list-windows"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    assert!(
        !server,
        "no window was ever spawned, so no server should be up"
    );
}

#[test]
#[cfg(unix)]
fn a_vetoed_creation_reports_through_the_command_bus() {
    // The TUI's path: the creation flow dispatches, the worker runs the same
    // pipeline, and the refusal is the in-flight error the placeholder shows.
    use thurbox::kernel::command::{Command, CommandBus, Phase};
    let repo = repo();
    let _tmux_dir = isolate_tmux();
    let (_home, config) = isolated_config();
    drop(on_disk_db());
    write_hooks(
        &config,
        "[[hooks]]\nevent = \"session.pre_create\"\ncommand = 'echo \"not on my watch\" >&2; exit 7'\n",
    );

    let mut bus = CommandBus::new();
    bus.dispatch(Command::Create {
        name: "vetoed".into(),
        repo: repo.path().display().to_string(),
        branch: None,
        base: None,
        agent: Some("shell".into()),
        host: None,
        extras: Vec::new(),
    });
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while std::time::Instant::now() < deadline {
        bus.poll();
        if let Some(failed) = bus
            .inflight()
            .into_iter()
            .find(|entry| entry.phase == Phase::Failed)
        {
            let error = failed.error.unwrap_or_default();
            assert!(error.contains("not on my watch"), "{error}");
            assert!(error.contains("exited 7"), "{error}");
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    panic!("the veto never surfaced through the bus");
}

#[test]
#[cfg(unix)]
fn a_post_create_failure_leaves_the_session_running() {
    if !have_tmux() {
        eprintln!("skipping: tmux is not installed");
        return;
    }
    let repo = repo();
    let _tmux_dir = isolate_tmux();
    let (_home, config) = isolated_config();
    let db = on_disk_db();
    write_hooks(
        &config,
        "[[hooks]]\nevent = \"session.post_create\"\ncommand = 'echo \"could not warm the cache\" >&2; exit 2'\n",
    );

    let spawned = match thurbox::session_ops::spawn::spawn_session_headless(
        &db,
        shell_request(repo.path(), None),
    ) {
        Ok(spawned) => spawned,
        Err(e) => {
            cleanup();
            if e.contains("tmux") {
                eprintln!("skipping: tmux would not spawn a window: {e}");
                return;
            }
            panic!("creation failed: {e}");
        }
    };
    cleanup();

    assert_eq!(
        spawned.hook_failures.len(),
        1,
        "{:?}",
        spawned.hook_failures
    );
    assert!(
        spawned.hook_failures[0].contains("exited 2: could not warm the cache"),
        "{:?}",
        spawned.hook_failures
    );
    assert!(
        db.get_session_by_id(spawned.session_id)
            .expect("query")
            .is_some(),
        "the session stands"
    );
}

#[test]
#[cfg(unix)]
fn delete_restart_and_restore_fire_their_pairs_once_and_pre_delete_can_refuse() {
    if !have_tmux() {
        eprintln!("skipping: tmux is not installed");
        return;
    }
    let repo = repo();
    let _tmux_dir = isolate_tmux();
    let (home, config) = isolated_config();
    let db = on_disk_db();
    let log = home.path().join("hooks.log");
    let mut hooks = String::new();
    for event in [
        "session.pre_delete",
        "session.post_delete",
        "session.pre_restart",
        "session.post_restart",
        "session.pre_restore",
        "session.post_restore",
    ] {
        hooks.push_str(&logging_hook(event, &log));
    }
    write_hooks(&config, &hooks);

    let spawned = match thurbox::session_ops::spawn::spawn_session_headless(
        &db,
        shell_request(repo.path(), None),
    ) {
        Ok(spawned) => spawned,
        Err(e) => {
            cleanup();
            if e.contains("tmux") {
                eprintln!("skipping: tmux would not spawn a window: {e}");
                return;
            }
            panic!("creation failed: {e}");
        }
    };
    let id = spawned.session_id;
    let events = |log: &Path| -> Vec<String> {
        std::fs::read_to_string(log)
            .unwrap_or_default()
            .lines()
            .map(|l| l.split_whitespace().next().unwrap_or_default().to_string())
            .collect()
    };

    let restart = thurbox::session_ops::restart_session_headless(&db, id).expect("restart");
    assert!(
        restart.hook_failures.is_empty(),
        "{:?}",
        restart.hook_failures
    );
    assert_eq!(
        events(&log),
        ["session.pre_restart", "session.post_restart"]
    );

    let soft = thurbox::session_ops::delete_session_headless(&db, id, false).expect("soft delete");
    assert!(soft.hook_failures.is_empty());
    let restore = thurbox::session_ops::restore_session_headless(&db, id, false).expect("restore");
    assert!(
        restore.hook_failures.is_empty(),
        "{:?}",
        restore.hook_failures
    );
    let forced =
        thurbox::session_ops::delete_session_headless(&db, id, true).expect("force delete");
    assert!(forced.hook_failures.is_empty());
    cleanup();
    assert_eq!(
        events(&log),
        [
            "session.pre_restart",
            "session.post_restart",
            "session.pre_delete",
            "session.post_delete",
            "session.pre_restore",
            "session.post_restore",
            "session.pre_delete",
            "session.post_delete",
        ]
    );
    // The delete hooks were told which kind each was.
    let seen = std::fs::read_to_string(&log).unwrap();
    assert!(seen.contains(&id.to_string()));

    // A second session, and a pre-delete that refuses: the row is untouched.
    write_hooks(
        &config,
        "[[hooks]]\nevent = \"session.pre_delete\"\ncommand = 'echo \"build still running\" >&2; exit 1'\n",
    );
    let kept = match thurbox::session_ops::spawn::spawn_session_headless(
        &db,
        shell_request(repo.path(), None),
    ) {
        Ok(spawned) => spawned,
        Err(e) => {
            cleanup();
            panic!("second creation failed: {e}");
        }
    };
    let err = thurbox::session_ops::delete_session_headless(&db, kept.session_id, true)
        .expect_err("the veto refuses the delete");
    cleanup();
    assert!(err.contains("build still running"), "{err}");
    let row = db
        .get_session_by_id(kept.session_id)
        .expect("query")
        .expect("still an active row");
    assert_eq!(row.name, "hooked");
}

/// The full arrival-and-parking story on a real tmux server: a session created
/// from a **raw command** (no `agents.toml` entry at all), restarted so its
/// persisted recipe has to be replayed, then stopped and started again.
///
/// These four verbs share one fixture deliberately — each is only meaningful
/// against a session the previous one left behind, and a `--command` session is
/// the shape that has no registry entry to fall back on at any step.
#[test]
#[cfg(not(windows))]
fn a_command_session_survives_restart_and_can_be_parked() {
    if !have_tmux() {
        eprintln!("skipping: tmux is not installed");
        return;
    }

    let repo = repo();
    let db = thurbox::storage::Database::open_in_memory().expect("db");
    let _tmux_dir = isolate_tmux();
    let home = tempfile::tempdir().expect("tempdir");
    thurbox::paths::set_test_dir(home.path());

    // No agents.toml is written: the point is that this session names no agent.
    let result = thurbox::session_ops::spawn::spawn_session_headless(
        &db,
        thurbox::session_ops::spawn::SpawnRequest {
            name: "recipe-probe".into(),
            repo_path: repo.path().to_path_buf(),
            worktree_branch: None,
            base_branch: None,
            agent: None,
            command: Some("sh".into()),
            args: vec!["-c".into(), "while :; do sleep 1; done".into()],
            env: [("THURBOX_E2E_MARKER".to_string(), "kept".to_string())]
                .into_iter()
                .collect(),
            resume_session_id: None,
            agent_session_id: None,
            host: None,
            parent_session_id: None,
            task_id: None,
            extra_repos: Vec::new(),
            fork_session_id: None,
            inherit_worktrees: Vec::new(),
        },
    );
    let spawned = match result {
        Ok(spawned) => spawned,
        Err(e) => {
            cleanup();
            if e.contains("tmux") {
                eprintln!("skipping: tmux would not spawn a window: {e}");
                return;
            }
            panic!("creation failed: {e}");
        }
    };
    let id = spawned.session_id;

    // The command's own name identifies it, and no registry lookup produced it.
    assert_eq!(spawned.agent, "sh");

    // The recipe is on the row — which is the only record of how to start this
    // session again, since there is no `agents.toml` entry to re-resolve.
    let recipe = db
        .load_launch_recipe(id)
        .expect("query")
        .expect("a command session persists its recipe");
    assert_eq!(recipe.command, "sh");
    assert_eq!(
        recipe.env.get("THURBOX_E2E_MARKER").map(String::as_str),
        Some("kept")
    );

    // A registry agent stores none, so restart keeps resolving it by name and
    // an `agents.toml` edit still takes effect.
    assert!(
        thurbox::session_ops::restart::restart_session_headless(&db, id).is_ok(),
        "a command session restarts from its recipe"
    );
    assert_eq!(
        db.load_launch_recipe(id).expect("query").map(|r| r.command),
        Some("sh".to_string()),
        "the recipe outlives the restart it drove"
    );

    // Park it: the pane goes, the row stays.
    thurbox::session_ops::restart::stop_session_headless(&db, id).expect("stop");
    assert!(
        db.session_stopped_at(id).expect("query").is_some(),
        "the stop is recorded, not merely performed"
    );
    assert!(
        db.get_session_by_id(id).expect("query").is_some(),
        "stopping is not deleting"
    );

    // And nothing puts it back on its own: a peer asking for "relaunch what is
    // missing" must not undo a deliberate stop.
    thurbox::session_ops::restart::restart_session_headless_with(&db, id, true)
        .expect("relaunch is a no-op here");
    assert!(
        db.session_stopped_at(id).expect("query").is_some(),
        "`restart --if-missing` left the stop alone"
    );

    // `start` is the one caller that may, and the identity survives it.
    thurbox::session_ops::restart::start_session_headless(&db, id).expect("start");
    assert!(
        db.session_stopped_at(id).expect("query").is_none(),
        "starting clears the mark"
    );
    assert_eq!(
        db.get_session_by_id(id).expect("query").expect("row").id,
        id,
        "the session kept its identity across the whole cycle"
    );

    cleanup();
}

/// Forking a registry-agent session must carry over its recorded `--env`.
///
/// A registry agent has no [`LaunchRecipe`](thurbox::session::LaunchRecipe) —
/// only a command session does — so a fork that read its env from the recipe
/// would always find one and silently produce a fork with no env at all,
/// unlike a command session's fork, which keeps its env via the recipe. Both
/// now read the same `launch_env` column instead.
#[test]
#[cfg(not(windows))]
fn a_forked_registry_agent_session_keeps_its_recorded_env() {
    if !have_tmux() {
        eprintln!("skipping: tmux is not installed");
        return;
    }

    let repo = repo();
    let db = thurbox::storage::Database::open_in_memory().expect("db");
    let _tmux_dir = isolate_tmux();
    let home = tempfile::tempdir().expect("tempdir");
    thurbox::paths::set_test_dir(home.path());
    let config = thurbox::paths::config_file()
        .expect("config path")
        .parent()
        .expect("config dir")
        .to_path_buf();
    std::fs::create_dir_all(&config).expect("mkdir");
    std::fs::write(
        config.join("agents.toml"),
        "default = \"shell\"\n\n[[agents]]\nname = \"shell\"\ncommand = \"sh\"\nargs = []\n",
    )
    .expect("write agents.toml");

    let result = thurbox::session_ops::spawn::spawn_session_headless(
        &db,
        thurbox::session_ops::spawn::SpawnRequest {
            name: "env-probe".into(),
            repo_path: repo.path().to_path_buf(),
            worktree_branch: None,
            base_branch: None,
            agent: Some("shell".into()),
            command: None,
            args: Vec::new(),
            env: [("FM_PROBE".to_string(), "1".to_string())]
                .into_iter()
                .collect(),
            resume_session_id: None,
            agent_session_id: None,
            host: None,
            parent_session_id: None,
            task_id: None,
            extra_repos: Vec::new(),
            fork_session_id: None,
            inherit_worktrees: Vec::new(),
        },
    );
    let spawned = match result {
        Ok(spawned) => spawned,
        Err(e) => {
            cleanup();
            if e.contains("tmux") {
                eprintln!("skipping: tmux would not spawn a window: {e}");
                return;
            }
            panic!("creation failed: {e}");
        }
    };

    // A registry agent carries no recipe — its `--env` lives only in the
    // shared `launch_env` column.
    assert!(db
        .load_launch_recipe(spawned.session_id)
        .expect("query")
        .is_none());
    assert_eq!(
        db.load_launch_env(spawned.session_id)
            .expect("query")
            .get("FM_PROBE")
            .map(String::as_str),
        Some("1"),
        "the spawn recorded its own --env"
    );

    let fork = match thurbox::session_ops::fork_session_headless(
        &db,
        spawned.session_id,
        "env-probe-fork",
    ) {
        Ok(fork) => fork,
        Err(e) => {
            cleanup();
            if e.contains("tmux") {
                eprintln!("skipping: tmux would not spawn a window: {e}");
                return;
            }
            panic!("fork failed: {e}");
        }
    };

    assert_eq!(
        db.load_launch_env(fork.session_id)
            .expect("query")
            .get("FM_PROBE")
            .map(String::as_str),
        Some("1"),
        "a fork of a registry-agent session must keep the env its parent recorded"
    );

    cleanup();
}
