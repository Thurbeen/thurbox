//! The bookmark command and the branch list, against a real database and a real
//! repository.
//!
//! Both are the halves of the creation flow that touch the world, so neither can
//! be proved from the plugin side: what matters is that a typed path is *refused*
//! before it can become a session, that forgetting a folder takes its members
//! with it, and that a repository with no reachable remote still offers its
//! branches. Each of those is a shell-out or a write.
//!
//! Isolation is by **environment variable**, not `paths::set_test_dir`: that
//! override is thread-local, and every command runs on its own thread, so a
//! worker would resolve the developer's real database and write bookmarks into
//! it. `THURBOX_CONFIG_DIR`/`THURBOX_DATA_DIR` are process-wide and are what the
//! spawned thread reads. nextest runs a process per test, so setting them here is
//! safe.

use std::path::Path;
use std::process::Command as Process;

use thurbox::kernel::command::{Args, Command, CommandBus};
use thurbox::kernel::repos::{Branches, RepoStore};

/// Run a command through the bus and wait for it, so its effect is observable.
///
/// The bus is asynchronous on purpose; a test that asserts on the outcome has to
/// wait for it, and waiting *here* rather than inside the bus is what keeps the
/// production path non-blocking.
fn run(bus: &mut CommandBus, command: Command) -> Option<String> {
    bus.dispatch(command);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    while std::time::Instant::now() < deadline {
        bus.poll();
        let inflight = bus.inflight();
        match inflight.first() {
            // Gone from the list means it succeeded.
            None => return None,
            Some(item) if item.error.is_some() => return item.error.clone(),
            Some(_) => std::thread::sleep(std::time::Duration::from_millis(10)),
        }
    }
    panic!("the command never finished");
}

fn isolate() -> tempfile::TempDir {
    let home = tempfile::tempdir().expect("tempdir");
    let config = home.path().join("config");
    let data = home.path().join("data");
    std::fs::create_dir_all(&config).expect("mkdir");
    std::fs::create_dir_all(&data).expect("mkdir");
    // Process-wide, so the command's own thread resolves the same sandbox. A
    // thread-local override would leave the worker writing to the real database
    // — which is exactly what happened while this used `set_test_dir`.
    std::env::set_var("THURBOX_CONFIG_DIR", &config);
    std::env::set_var("THURBOX_DATA_DIR", &data);
    // And for this thread's own reads, which go through the same resolver.
    thurbox::paths::set_test_dir(&data);
    home
}

fn database() -> thurbox::storage::Database {
    let path = thurbox::paths::database_file().expect("database path");
    thurbox::storage::Database::open(&path).expect("open database")
}

fn add(path: &str) -> Command {
    Command::parse(
        "bookmark",
        Args {
            repo: Some(path.to_string()),
            action: Some("add".into()),
            ..Args::default()
        },
    )
    .expect("parse")
}

fn edit(path: &str, verb: &str) -> Command {
    Command::parse(
        "bookmark",
        Args {
            repo: Some(path.to_string()),
            action: Some(verb.to_string()),
            ..Args::default()
        },
    )
    .expect("parse")
}

/// A git repository with one commit, and its default branch.
fn repo(at: &Path) {
    let git = |args: &[&str]| {
        let mut command = Process::new("git");
        command
            .args(args)
            .current_dir(at)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        // The same scrub `git::GIT_LOCATION_ENV` exists for, and for the reason
        // named there: git exports these to hook processes, so the suite running
        // under the project's own pre-commit `cargo nextest` inherits a `GIT_DIR`
        // pointing at the real repository. `git init` then fails — and had it
        // succeeded, the `commit` below would have landed in that repository
        // rather than this tempdir. `tests/create_e2e.rs` scrubs them too.
        for name in [
            "GIT_DIR",
            "GIT_WORK_TREE",
            "GIT_INDEX_FILE",
            "GIT_COMMON_DIR",
            "GIT_OBJECT_DIRECTORY",
            "GIT_ALTERNATE_OBJECT_DIRECTORIES",
            "GIT_PREFIX",
            "GIT_NAMESPACE",
            // Config injected on the *command line* — `git -c commit.gpgsign=true
            // commit` — travels to every child through these, hooks included, and
            // outranks the repo config a test could set. `GIT_CONFIG_GLOBAL` above
            // does not cover it. Left in place, a contributor signing this
            // project's own commits fails these tests and nothing else: HOME is
            // isolated here, so there is no key to sign the fixture commit with.
            "GIT_CONFIG_PARAMETERS",
            "GIT_CONFIG_COUNT",
        ] {
            command.env_remove(name);
        }
        let status = command.status().expect("run git");
        assert!(status.success(), "git {args:?}");
    };
    git(&["init", "-q", "-b", "main"]);
    git(&["config", "user.email", "test@example.com"]);
    git(&["config", "user.name", "Test"]);
    git(&["commit", "-q", "--allow-empty", "-m", "root"]);
}

#[test]
fn a_path_that_does_not_exist_is_refused_and_not_remembered() {
    let _home = isolate();
    let mut bus = CommandBus::new();
    let missing = _home.path().join("nope");

    let error = run(&mut bus, add(&missing.display().to_string()));
    assert!(
        error
            .as_deref()
            .unwrap_or_default()
            .contains("Path not found"),
        "{error:?}"
    );
    assert!(
        database().list_repo_bookmarks("").expect("list").is_empty(),
        "a refused path must leave no memory behind"
    );
}

#[test]
fn an_added_repository_is_remembered_with_its_git_ness() {
    let _home = isolate();
    let mut bus = CommandBus::new();
    let checkout = _home.path().join("thing");
    std::fs::create_dir_all(&checkout).expect("mkdir");
    repo(&checkout);

    assert_eq!(run(&mut bus, add(&checkout.display().to_string())), None);
    let remembered = database().list_repo_bookmarks("").expect("list");
    assert_eq!(remembered.len(), 1);
    assert_eq!(remembered[0].repo_path, checkout);
    assert_eq!(
        remembered[0].is_git,
        Some(true),
        "git-ness is established once and recorded, because it gates worktree mode"
    );
}

#[test]
fn a_plain_directory_is_remembered_as_one() {
    // Not a repository, but still a valid member of a multi-repo session — v1
    // renders it `(dir)` and refuses only the worktree toggle.
    let _home = isolate();
    let mut bus = CommandBus::new();
    let plain = _home.path().join("reference");
    std::fs::create_dir_all(&plain).expect("mkdir");

    assert_eq!(run(&mut bus, add(&plain.display().to_string())), None);
    let remembered = database().list_repo_bookmarks("").expect("list");
    assert_eq!(remembered[0].is_git, Some(false));
}

#[test]
fn adding_a_remembered_path_again_touches_it_rather_than_duplicating_it() {
    // This is what makes "select the newest row" identify the row that was just
    // added, which is how the flow re-selects a path it already knew.
    let _home = isolate();
    let mut bus = CommandBus::new();
    let first = _home.path().join("one");
    let second = _home.path().join("two");
    for path in [&first, &second] {
        std::fs::create_dir_all(path).expect("mkdir");
        repo(path);
    }

    run(&mut bus, add(&first.display().to_string()));
    run(&mut bus, add(&second.display().to_string()));
    // `two` is now the most recent; re-adding `one` must put it back on top.
    run(&mut bus, add(&first.display().to_string()));

    let remembered = database().list_repo_bookmarks("").expect("list");
    assert_eq!(remembered.len(), 2, "no duplicate row");
    assert_eq!(
        remembered[0].repo_path, first,
        "memory is published most-recent-first, and an add touches recency"
    );
}

#[test]
fn importing_a_folder_remembers_its_repositories_and_reports_an_empty_one() {
    let _home = isolate();
    let mut bus = CommandBus::new();
    let folder = _home.path().join("src");
    let inside = folder.join("thing");
    std::fs::create_dir_all(&inside).expect("mkdir");
    repo(&inside);
    // A plain directory alongside it, which is not a repository and so not a
    // member of the folder.
    std::fs::create_dir_all(folder.join("notes")).expect("mkdir");

    assert_eq!(
        run(&mut bus, edit(&folder.display().to_string(), "parent")),
        None
    );
    let remembered = database().list_repo_bookmarks("").expect("list");
    let parent = remembered
        .iter()
        .find(|row| row.repo_path == folder)
        .expect("the folder is remembered");
    assert!(parent.is_parent);
    let members: Vec<_> = remembered
        .iter()
        .filter(|row| row.parent_path.as_deref() == Some(folder.as_path()))
        .collect();
    assert_eq!(members.len(), 1, "only the repository under it");
    assert_eq!(members[0].repo_path, inside);

    // A folder with nothing in it is reported rather than looking like an import
    // that silently did nothing.
    let empty = _home.path().join("empty");
    std::fs::create_dir_all(&empty).expect("mkdir");
    let error = run(&mut bus, edit(&empty.display().to_string(), "parent"));
    assert!(
        error
            .as_deref()
            .unwrap_or_default()
            .contains("No repositories found"),
        "{error:?}"
    );
}

#[test]
fn forgetting_a_folder_takes_its_members_with_it() {
    let _home = isolate();
    let mut bus = CommandBus::new();
    let folder = _home.path().join("src");
    let inside = folder.join("thing");
    std::fs::create_dir_all(&inside).expect("mkdir");
    repo(&inside);

    run(&mut bus, edit(&folder.display().to_string(), "parent"));
    assert!(database().list_repo_bookmarks("").expect("list").len() >= 2);

    assert_eq!(
        run(&mut bus, edit(&folder.display().to_string(), "remove")),
        None
    );
    assert!(
        database().list_repo_bookmarks("").expect("list").is_empty(),
        "a folder and its members are forgotten together"
    );
}

#[test]
fn a_bookmark_can_be_forgotten_after_its_host_is_gone() {
    // Removal names a path already in the memory, so it needs neither the
    // filesystem nor the host — which is what lets a leftover row be cleaned up
    // after the host was taken out of `hosts.toml`.
    let _home = isolate();
    let mut bus = CommandBus::new();
    let path = std::path::Path::new("/srv/thing");
    database()
        .upsert_repo_bookmark("ssh:gone", path)
        .expect("remember");

    let removed = Command::parse(
        "bookmark",
        Args {
            repo: Some("/srv/thing".into()),
            host: Some("ssh:gone".into()),
            action: Some("remove".into()),
            ..Args::default()
        },
    )
    .expect("parse");
    assert_eq!(run(&mut bus, removed), None);
    assert!(database()
        .list_repo_bookmarks("ssh:gone")
        .expect("list")
        .is_empty());
}

#[test]
fn adding_for_a_host_that_is_gone_is_refused_by_name() {
    // The other half of the same rule: adding *does* need the host, to expand a
    // tilde and to establish what the path is, so an unknown one is refused
    // rather than silently treated as local.
    let _home = isolate();
    let mut bus = CommandBus::new();
    let add_remote = Command::parse(
        "bookmark",
        Args {
            repo: Some("/srv/thing".into()),
            host: Some("ssh:gone".into()),
            action: Some("add".into()),
            ..Args::default()
        },
    )
    .expect("parse");
    let error = run(&mut bus, add_remote);
    assert!(
        error
            .as_deref()
            .unwrap_or_default()
            .contains("no such host"),
        "{error:?}"
    );
}

#[test]
fn forgetting_something_never_remembered_says_so() {
    let _home = isolate();
    let mut bus = CommandBus::new();
    let error = run(&mut bus, edit("/nowhere/at/all", "remove"));
    assert!(
        error
            .as_deref()
            .unwrap_or_default()
            .contains("not a remembered repository"),
        "{error:?}"
    );
}

#[test]
fn a_repository_with_no_remote_still_offers_its_branches() {
    // The fetch fails — there is no origin — and that is non-fatal, exactly as
    // it is in v1: the branches known locally are still what you pick from.
    let home = isolate();
    let checkout = home.path().join("thing");
    std::fs::create_dir_all(&checkout).expect("mkdir");
    repo(&checkout);

    let mut store = RepoStore::with_hosts(Default::default());
    let path = checkout.display().to_string();
    store.request_branches("", &path);
    assert_eq!(
        store.branches("", &path),
        Some(&Branches::Pending),
        "asking must not wait for git"
    );

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    while std::time::Instant::now() < deadline {
        store.poll();
        match store.branches("", &path) {
            Some(Branches::Ready(branches)) => {
                assert!(
                    branches.iter().any(|branch| branch == "main"),
                    "{branches:?}"
                );
                return;
            }
            Some(Branches::Failed(error)) => panic!("{error}"),
            _ => std::thread::sleep(std::time::Duration::from_millis(20)),
        }
    }
    panic!("the branch list never arrived");
}
