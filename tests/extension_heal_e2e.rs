//! Self-heal must not put two sessions of one name on one backend.
//!
//! `ensure_extension` recreates an active extension's declared session whenever
//! it is missing, unattended, from the 60 s heartbeat tick. Its only guard was a
//! `HashMap<name, id>` snapshotted from `list_active_sessions()` before the
//! spawn — not scoped to a backend, blind to a delete that can still be undone,
//! and not a claim, so two healers both missed and both spawned. Each test here
//! is one of the sequences that produced the pair (issue #1192).
//!
//! Scoped to a throwaway socket through `TmuxServer`, in temporary
//! directories, and skipped when tmux is absent, like the other end-to-end
//! tests here.

use std::path::Path;
use std::process::Command;

use thurbox::session::{ExtensionDef, ExtensionSession};
use thurbox::session_ops::spawn::LOCAL_TMUX_BACKEND_TYPE;
use thurbox::storage::Database;

/// The guard every tmux server in this file is reaped by — see its own doc.
#[path = "support/tmux_server.rs"]
mod tmux_server;

use tmux_server::TmuxServer;

/// A throwaway tmux socket, so this never touches the real one.
const SOCKET: &str = "thurbox-heal-e2e";

/// The declared session every test here heals.
const DECLARED: &str = "mission-control";

fn have_tmux() -> bool {
    Command::new("tmux")
        .arg("-V")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// The `GIT_*` location variables git exports to hook processes — the list
/// `git::GIT_LOCATION_ENV` scrubs, which is crate-private. A suite running
/// under this repository's own pre-commit hook inherits a `GIT_DIR` pointing
/// at the real repository, so every git process here drops them.
const GIT_LOCATION_ENV: [&str; 8] = [
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_INDEX_FILE",
    "GIT_COMMON_DIR",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_PREFIX",
    "GIT_NAMESPACE",
];

fn git(dir: &Path, args: &[&str]) {
    let mut cmd = Command::new("git");
    cmd.args(args).current_dir(dir);
    for var in GIT_LOCATION_ENV {
        cmd.env_remove(var);
    }
    let ok = cmd.output().expect("run git").status.success();
    assert!(ok, "git {args:?} failed");
}

/// A repository with one commit, which is the minimum a spawn needs.
fn repo() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    git(dir.path(), &["init", "-q", "-b", "main"]);
    git(dir.path(), &["config", "user.email", "t@example.com"]);
    git(dir.path(), &["config", "user.name", "thurbox-test"]);
    git(dir.path(), &["config", "commit.gpgsign", "false"]);
    std::fs::write(dir.path().join("README.md"), "# probe\n").expect("write");
    git(dir.path(), &["add", "."]);
    git(dir.path(), &["commit", "-qm", "init"]);
    dir
}

/// A shell rather than a real agent: self-heal's name guard is what is under
/// test, and launching a coding agent would want credentials and a network.
/// Thread-local, so every thread a test fans out to calls this itself.
fn isolate_paths(home: &Path) {
    thurbox::paths::set_test_dir(home);
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
}

/// A manifest declaring one session and nothing else.
fn probe_def(repo: &Path) -> ExtensionDef {
    ExtensionDef {
        name: "probe".into(),
        description: None,
        config_version: Some(1),
        version: None,
        min_thurbox_version: None,
        installed_with: None,
        source: None,
        home: None,
        agents: Vec::new(),
        files: Vec::new(),
        external_files: Vec::new(),
        agent_patches: Vec::new(),
        config_merges: Vec::new(),
        symlinks: Vec::new(),
        sessions: vec![ExtensionSession {
            name: DECLARED.into(),
            agent: "shell".into(),
            repo_path: repo.to_path_buf(),
        }],
        automations: Vec::new(),
    }
}

/// Lay the manifest where discovery reads it, so anything asking "is this name
/// one an extension declares" can find out. `activate_extension` records the
/// active set; it does not write the file.
fn publish_manifest(def: &ExtensionDef) {
    let path = thurbox::agent::extension_config::manifest_path(&def.name).expect("manifest path");
    std::fs::create_dir_all(path.parent().expect("extensions dir")).expect("mkdir");
    std::fs::write(&path, toml::to_string(def).expect("serialize")).expect("write manifest");
}

/// The active rows carrying the declared name on the local backend — the
/// population the whole issue is about.
fn local_namesakes(db: &Database) -> Vec<thurbox::sync::SharedSession> {
    db.find_sessions_by_name(DECLARED)
        .expect("find_sessions_by_name")
        .into_iter()
        .filter(|s| s.backend_type == LOCAL_TMUX_BACKEND_TYPE)
        .collect()
}

/// Heal once, skipping the test when the environment cannot spawn at all.
fn heal(db: &Database, def: &ExtensionDef) -> Option<thurbox::session_ops::EnsureReport> {
    match thurbox::session_ops::ensure_extension(db, def) {
        Ok(report) => Some(report),
        Err(e) => {
            assert!(e.contains("tmux"), "ensure_extension failed: {e}");
            eprintln!("skipping: tmux would not spawn a window: {e}");
            None
        }
    }
}

/// Repro 2 of the issue. A soft delete keeps the agent for a 10 s undo window;
/// the keeper ticks every 60 s, so roughly one delete in six has a tick land
/// inside it. Heal spawned a replacement, the undo brought the original back,
/// and the name then addressed two live sessions.
#[test]
fn a_heal_inside_the_undo_window_leaves_one_session_of_the_name() {
    if !have_tmux() {
        eprintln!("skipping: tmux is not installed");
        return;
    }
    let repo = repo();
    let home = tempfile::tempdir().expect("tempdir");
    let _server = TmuxServer::pin(SOCKET);
    isolate_paths(home.path());
    let db = Database::open_in_memory().expect("db");
    let def = probe_def(repo.path());

    let Some(first) = heal(&db, &def) else { return };
    assert_eq!(
        first.sessions_created,
        [DECLARED],
        "the first heal creates it"
    );
    let original = local_namesakes(&db)[0].id;

    // The operator deletes it. Softly: the undo is still on offer.
    thurbox::session_ops::delete_session_headless(&db, original, false).expect("delete");

    // The tick lands inside the undo window.
    let Some(second) = heal(&db, &def) else {
        return;
    };

    // The undo.
    let restored = thurbox::session_ops::restore_session_headless(&db, original, true);

    let named = local_namesakes(&db);

    assert!(restored.is_ok(), "the undo must still work: {restored:?}");
    assert_eq!(
        named.len(),
        1,
        "'{DECLARED}' addresses {} live sessions on {LOCAL_TMUX_BACKEND_TYPE} \
         after an undone delete (heal created {:?})",
        named.len(),
        second.sessions_created,
    );
    assert_eq!(
        named[0].id, original,
        "the undone delete gives back the original"
    );
    // Refusing is only half of it: this pass runs with nobody watching, so a
    // refusal it does not report is indistinguishable from nothing to do.
    assert!(second.sessions_created.is_empty(), "nothing was created");
    assert_eq!(second.sessions_blocked.len(), 1, "{second:?}");
    let said = &second.sessions_blocked[0];
    assert!(said.contains(DECLARED), "names the session: {said}");
    assert!(said.contains("undo"), "says what holds the name: {said}");
}

/// The other end of the same sequence. Once the undo window has closed,
/// self-heal does recreate the session — and the original then has to be
/// refused, or un-deleting it puts both on the backend at once.
#[test]
fn a_restore_does_not_un_delete_a_name_something_else_now_answers_to() {
    if !have_tmux() {
        eprintln!("skipping: tmux is not installed");
        return;
    }
    let repo = repo();
    let home = tempfile::tempdir().expect("tempdir");
    let _server = TmuxServer::pin(SOCKET);
    isolate_paths(home.path());
    let db = Database::open_in_memory().expect("db");
    let def = probe_def(repo.path());

    let Some(_) = heal(&db, &def) else { return };
    let original = local_namesakes(&db)[0].id;
    // Force-deleted so the name is free at once: this test is about the restore,
    // not about waiting out the undo window.
    thurbox::session_ops::delete_session_headless(&db, original, true).expect("delete");
    let Some(_) = heal(&db, &def) else { return };

    let restored = thurbox::session_ops::restore_session_headless(&db, original, true);
    let named = local_namesakes(&db);

    let err = restored.expect_err("the name is taken; the restore must say so");
    assert!(err.contains(DECLARED), "names the session: {err}");
    assert!(
        err.contains(LOCAL_TMUX_BACKEND_TYPE),
        "names the backend the pair would be on: {err}"
    );
    assert_eq!(named.len(), 1, "one live session of the name, not two");
}

/// Repro 3 of the issue. Two healers run concurrently with the session absent —
/// in the field, TUI startup beside the keeper's tick. Both snapshotted, both
/// missed, both spawned. Two connections to one database file, because that is
/// what the two processes are.
#[test]
fn two_heals_racing_leave_one_session_of_the_name() {
    if !have_tmux() {
        eprintln!("skipping: tmux is not installed");
        return;
    }
    let repo = repo();
    let home = tempfile::tempdir().expect("tempdir");
    let _server = TmuxServer::pin(SOCKET);
    isolate_paths(home.path());
    let db_path = home.path().join("race.db");
    // The schema, once, before either racer opens its own connection.
    drop(Database::open(&db_path).expect("db"));

    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let mut healers = Vec::new();
    for _ in 0..2 {
        let barrier = std::sync::Arc::clone(&barrier);
        let home = home.path().to_path_buf();
        let repo = repo.path().to_path_buf();
        let db_path = db_path.clone();
        healers.push(std::thread::spawn(move || {
            // Path resolution is thread-local; the tmux socket is not.
            isolate_paths(&home);
            let db = Database::open(&db_path).expect("db");
            let def = probe_def(&repo);
            barrier.wait();
            thurbox::session_ops::ensure_extension(&db, &def)
        }));
    }
    let reports: Vec<_> = healers
        .into_iter()
        .map(|h| h.join().expect("healer thread"))
        .collect();

    let db = Database::open(&db_path).expect("db");
    let named = local_namesakes(&db);

    assert_eq!(
        named.len(),
        1,
        "two concurrent heals left {} live sessions called '{DECLARED}' on \
         {LOCAL_TMUX_BACKEND_TYPE}; reports: {reports:?}",
        named.len(),
    );
    let created: usize = reports
        .iter()
        .map(|r| r.as_ref().expect("heal").sessions_created.len())
        .sum();
    let blocked: Vec<&String> = reports
        .iter()
        .flat_map(|r| &r.as_ref().expect("heal").sessions_blocked)
        .collect();
    assert_eq!(created, 1, "exactly one healer created it: {reports:?}");
    // One blocked message when the claim was contended, none when the second
    // healer was descheduled past the first's whole spawn and simply reused the
    // session it found. Both are the contract; two creations is not, and that is
    // what the count above pins.
    assert!(blocked.len() <= 1, "at most one refusal: {reports:?}");
    for said in &blocked {
        assert!(
            said.contains(DECLARED),
            "a refusal names the session: {said}"
        );
    }
}

/// Self-heal's lookup spanned every backend, so a namesake on another machine —
/// which ADR-24 mirroring legitimately puts in this database — answered for the
/// local session and the local one was never healed.
#[test]
fn a_namesake_on_another_backend_does_not_answer_for_the_local_session() {
    if !have_tmux() {
        eprintln!("skipping: tmux is not installed");
        return;
    }
    let repo = repo();
    let home = tempfile::tempdir().expect("tempdir");
    let _server = TmuxServer::pin(SOCKET);
    isolate_paths(home.path());
    let db = Database::open_in_memory().expect("db");

    // A mirrored row from another machine, carrying the declared name.
    db.upsert_session(&thurbox::sync::SharedSession {
        id: thurbox::session::SessionId::default(),
        name: DECLARED.into(),
        agent: "shell".into(),
        backend_id: String::new(),
        backend_type: "ssh:elsewhere".into(),
        agent_session_id: Some(uuid::Uuid::new_v4().to_string()),
        cwd: None,
        additional_dirs: Vec::new(),
        worktrees: Vec::new(),
        shell_backend_id: None,
        parent_session_id: None,
        display_order: None,
        tombstone: false,
        tombstone_at: None,
    })
    .expect("upsert");

    let def = probe_def(repo.path());
    let Some(report) = heal(&db, &def) else {
        return;
    };
    let named = local_namesakes(&db);

    assert_eq!(
        report.sessions_created,
        [DECLARED],
        "the local session is missing and must still be healed",
    );
    assert_eq!(named.len(), 1, "exactly one local session of the name");
}

/// The other half of the contract: refusing when the name is not free must not
/// have been bought by making self-heal a no-op. A force-deleted session cannot
/// be undone, so its name *is* free and the extension's session comes back —
/// which is what self-heal is for.
#[test]
fn heal_still_recreates_a_force_deleted_session() {
    if !have_tmux() {
        eprintln!("skipping: tmux is not installed");
        return;
    }
    let repo = repo();
    let home = tempfile::tempdir().expect("tempdir");
    let _server = TmuxServer::pin(SOCKET);
    isolate_paths(home.path());
    let db = Database::open_in_memory().expect("db");
    let def = probe_def(repo.path());

    let Some(_) = heal(&db, &def) else { return };
    let original = local_namesakes(&db)[0].id;
    thurbox::session_ops::delete_session_headless(&db, original, true).expect("force delete");

    let Some(report) = heal(&db, &def) else {
        return;
    };
    let named = local_namesakes(&db);

    assert_eq!(
        report.sessions_created,
        [DECLARED],
        "a force delete cannot be undone, so the name is free and heal recreates it",
    );
    assert_eq!(named.len(), 1, "exactly one live session of the name");
    assert_ne!(named[0].id, original, "a new session, not the deleted row");
}

/// A name that only collides once tmux has sanitised it is still one name to
/// every later teardown: `deploy prod` and `deploy.prod` share `tb-deploy_prod`,
/// and on a multiplexer that stamps no window that name is all a reap has to go
/// on. `session rename` has always refused on that test; so does the restore.
#[test]
fn a_restore_is_refused_by_a_namesake_it_only_shares_a_window_name_with() {
    if !have_tmux() {
        eprintln!("skipping: tmux is not installed");
        return;
    }
    let repo = repo();
    let home = tempfile::tempdir().expect("tempdir");
    let _server = TmuxServer::pin(SOCKET);
    isolate_paths(home.path());
    let db = Database::open_in_memory().expect("db");

    let mut def = probe_def(repo.path());
    def.sessions[0].name = "deploy.prod".into();
    let Some(_) = heal(&db, &def) else { return };
    let dotted = db
        .find_sessions_by_name("deploy.prod")
        .expect("find")
        .remove(0)
        .id;
    thurbox::session_ops::delete_session_headless(&db, dotted, true).expect("delete");

    // The name the operator types next differs, and the window does not.
    def.sessions[0].name = "deploy prod".into();
    let Some(_) = heal(&db, &def) else { return };

    let err = thurbox::session_ops::restore_session_headless(&db, dotted, true)
        .expect_err("the window name is taken; the restore must say so");
    assert!(err.contains("deploy.prod"), "names the deleted one: {err}");
    assert!(err.contains("deploy prod"), "names the live one: {err}");
}

/// Renaming the live session frees the name — except when self-heal takes it
/// straight back, which is every session an active extension declares. Telling
/// the user to rename there sends them into a race with the keeper, so the
/// refusal names the extension and its off-switch instead.
#[test]
fn a_refused_restore_names_the_extension_when_self_heal_owns_the_name() {
    if !have_tmux() {
        eprintln!("skipping: tmux is not installed");
        return;
    }
    let repo = repo();
    let home = tempfile::tempdir().expect("tempdir");
    let _server = TmuxServer::pin(SOCKET);
    isolate_paths(home.path());
    let db = Database::open_in_memory().expect("db");
    let def = probe_def(repo.path());
    publish_manifest(&def);

    let report = match thurbox::session_ops::activate_extension(&db, &def) {
        Ok(report) => report,
        Err(e) => {
            assert!(e.contains("tmux"), "activate failed: {e}");
            eprintln!("skipping: tmux would not spawn a window: {e}");
            return;
        }
    };
    assert_eq!(report.sessions_created, [DECLARED]);
    let original = local_namesakes(&db)[0].id;
    thurbox::session_ops::delete_session_headless(&db, original, true).expect("delete");
    let Some(_) = heal(&db, &def) else { return };

    let err = thurbox::session_ops::restore_session_headless(&db, original, true)
        .expect_err("the name is taken; the restore must say so");
    assert!(
        err.contains("extension deactivate probe"),
        "the advice has to terminate, and renaming does not: {err}"
    );
    assert!(
        !err.contains("session rename"),
        "renaming loses a race with the keeper here: {err}"
    );
}
