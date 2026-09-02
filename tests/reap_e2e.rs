//! Reaping a soft-deleted session must not kill a live session's window.
//!
//! The reaper resolved its victim through `agent_target`, which falls back from
//! a stale pane id to the `tb-<name>` window name unconditionally. That fallback
//! is right for a *live* session — you still want to reach its window after a
//! tmux restart renumbered the panes — but a reap must first know that nobody
//! else answers to the name: once the deleted row's pane no longer resolves, a
//! `tb-<name>` window is its own only while no live session shares the name.
//!
//! The sequence below is the one that bites in practice. A session freezes, the
//! operator deletes the row and recreates it, and 30-60s later the reaper for
//! the *deleted* row closes its undo window and kills the *replacement*. Each
//! delete-and-recreate arms one more of these, so the session dies faster every
//! time.
//!
//! Scoped to a throwaway socket and temporary directories, and skipped when
//! tmux is absent, like the other end-to-end tests here.

use std::path::Path;
use std::process::Command;

/// A throwaway tmux socket, so this never touches the real one.
const SOCKET: &str = "thurbox-reap-e2e";

fn have_tmux() -> bool {
    Command::new("tmux")
        .arg("-V")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn tmux(args: &[&str]) -> std::process::Output {
    Command::new("tmux")
        .args(["-L", SOCKET])
        .args(args)
        .output()
        .expect("run tmux")
}

/// Every window currently on the throwaway server.
fn windows() -> Vec<String> {
    let out = tmux(&["list-windows", "-a", "-F", "#{window_name}"]);
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::to_string)
        .collect()
}

/// Whether `pane_id` (`%N`) still exists.
fn pane_alive(pane_id: &str) -> bool {
    let out = tmux(&["list-panes", "-a", "-F", "#{pane_id}"]);
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .any(|l| l.trim() == pane_id)
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
    // Signing would make this depend on a key in the user's agent; the repo is
    // throwaway, so it is disabled rather than required of the machine.
    git(dir.path(), &["config", "commit.gpgsign", "false"]);
    std::fs::write(dir.path().join("README.md"), "# probe\n").expect("write");
    git(dir.path(), &["add", "."]);
    git(dir.path(), &["commit", "-qm", "init"]);
    dir
}

/// Point the spawn at a private socket in a private directory, so it can never
/// see — or race — a real server. nextest runs one process per test, so env
/// mutation is safe. Returns the tempdir so it outlives the test.
fn isolate_tmux() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    std::env::set_var("TMUX_TMPDIR", dir.path());
    std::env::set_var(thurbox::agent::tmux::SOCKET_OVERRIDE_ENV, SOCKET);
    dir
}

/// A shell rather than a real agent: the reap path is what is under test, and
/// launching a coding agent would want credentials and a network.
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

fn cleanup() {
    let _ = tmux(&["kill-server"]);
}

fn spawn(
    db: &thurbox::storage::Database,
    repo: &Path,
    name: &str,
) -> Option<thurbox::session_ops::SpawnResult> {
    let result = thurbox::session_ops::spawn_session_headless(
        db,
        thurbox::session_ops::SpawnRequest {
            name: name.into(),
            repo_path: repo.to_path_buf(),
            // In place: a worktree is irrelevant to which window a reap targets.
            worktree_branch: None,
            base_branch: None,
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
    match result {
        Ok(spawned) => Some(spawned),
        Err(e) => {
            cleanup();
            // A tmux server that will not start is an environment problem.
            assert!(e.contains("tmux"), "spawn failed: {e}");
            eprintln!("skipping: tmux would not spawn a window: {e}");
            None
        }
    }
}

#[test]
fn reaping_a_stale_row_spares_the_live_window_of_the_same_name() {
    if !have_tmux() {
        eprintln!("skipping: tmux is not installed");
        return;
    }

    let repo = repo();
    let db = thurbox::storage::Database::open_in_memory().expect("db");
    let _tmux_dir = isolate_tmux();
    let home = tempfile::tempdir().expect("tempdir");
    isolate_paths(home.path());

    // 1. The session that will go stale.
    let Some(stale) = spawn(&db, repo.path(), "fleet") else {
        return;
    };

    // 2. Soft-deleted: the row is kept for undo, the window is left alone.
    let report = thurbox::session_ops::delete_session_headless(&db, stale.session_id, false)
        .expect("delete");
    assert!(
        !report.killed_window,
        "a soft delete must leave the window for the undo window"
    );

    // 3. Its agent exits and the window goes away — the state every frozen
    //    session ends up in, and what makes the row's pane id unresolvable.
    let _ = tmux(&["kill-pane", "-t", &stale.backend_id]);
    assert!(
        !pane_alive(&stale.backend_id),
        "the stale row's pane should be gone"
    );

    // 4. The operator recreates the session under the same name.
    let Some(live) = spawn(&db, repo.path(), "fleet") else {
        return;
    };
    assert!(
        pane_alive(&live.backend_id),
        "the replacement should be running"
    );

    // 5. The undo window closes and the reaper collects the stale row.
    let reaped = thurbox::session_ops::reap_soft_deleted(&db, stale.session_id).expect("reap");

    // The reap has nothing of its own left to kill, so it must not have reached
    // for the name — the replacement is the only `tb-fleet` there is.
    let survived = pane_alive(&live.backend_id);
    let windows_after = windows();
    cleanup();

    assert!(
        survived,
        "reaping the stale 'fleet' row killed the live one's pane {} \
         (reaped={reaped}); windows left: {windows_after:?}",
        live.backend_id
    );
    assert!(
        db.get_session_by_id(live.session_id)
            .expect("query")
            .is_some(),
        "the live row must survive its namesake's reap"
    );
}

/// The other half of the contract. Sparing a same-named window must not have
/// been bought by making the reap a no-op: while the row's *own* pane still
/// resolves, reaping it has to take the window down, or a soft-deleted session
/// keeps its agent running and writing forever — the thing the reaper exists
/// to prevent.
#[test]
fn reaping_still_kills_the_row_its_own_window() {
    if !have_tmux() {
        eprintln!("skipping: tmux is not installed");
        return;
    }

    let repo = repo();
    let db = thurbox::storage::Database::open_in_memory().expect("db");
    let _tmux_dir = isolate_tmux();
    let home = tempfile::tempdir().expect("tempdir");
    isolate_paths(home.path());

    let Some(session) = spawn(&db, repo.path(), "solo") else {
        return;
    };
    assert!(
        pane_alive(&session.backend_id),
        "the spawn should be running"
    );

    thurbox::session_ops::delete_session_headless(&db, session.session_id, false).expect("delete");

    // The undo window closes with the pane still there: this row owns it, so it
    // is exactly what the reap should collect.
    let reaped = thurbox::session_ops::reap_soft_deleted(&db, session.session_id).expect("reap");
    let still_there = pane_alive(&session.backend_id);
    cleanup();

    assert!(reaped, "a soft-deleted row with a live pane must be reaped");
    assert!(
        !still_there,
        "the reap must kill the row's own window (pane {} survived)",
        session.backend_id
    );
}

/// The pane id is not always there to be strict about. A row persisted before
/// local spawns recorded one, a pane renumbered by a tmux server restart, and
/// every session on psmux (where `spawn_window` records no id at all) all reach
/// the reap with an id that resolves to nothing. Strictness must not turn those
/// into a permanent no-op: while no live session answers to the name, the sole
/// `tb-<name>` window can only be this row's, and leaving it up means the
/// soft-deleted agent keeps running and writing forever.
#[test]
fn reaping_collects_its_window_when_the_pane_id_resolves_to_nothing() {
    if !have_tmux() {
        eprintln!("skipping: tmux is not installed");
        return;
    }

    let repo = repo();
    let db = thurbox::storage::Database::open_in_memory().expect("db");
    let _tmux_dir = isolate_tmux();
    let home = tempfile::tempdir().expect("tempdir");
    isolate_paths(home.path());

    let Some(session) = spawn(&db, repo.path(), "orphan") else {
        return;
    };
    // The psmux shape: the window is up, the row remembers no pane for it.
    assert!(
        db.set_backend_id(session.session_id, "")
            .expect("clear the pane id"),
        "the spawned row should be there to update"
    );
    thurbox::session_ops::delete_session_headless(&db, session.session_id, false).expect("delete");

    let reaped = thurbox::session_ops::reap_soft_deleted(&db, session.session_id).expect("reap");
    let still_there = pane_alive(&session.backend_id);
    let windows_after = windows();
    cleanup();

    assert!(
        reaped,
        "a soft-deleted row with a live window must be reaped"
    );
    assert!(
        !still_there,
        "the reap must collect the row's own window with no pane id to go on \
         (pane {} survived); windows left: {windows_after:?}",
        session.backend_id
    );
}

/// A remembered pane id is not proof of ownership either. tmux restarts its
/// pane-id counter with the server, so after a reboot the id a soft-deleted row
/// still remembers can be the pane of a *replacement* window — and the window
/// name, the check that normally catches a reused id, confirms nothing when the
/// two sessions share a name. A test cannot make a server restart renumber a
/// pane onto a chosen `%N`, so the row state a restart leaves behind is written
/// through the storage API instead: a soft-deleted row remembering the pane id
/// its live namesake now holds.
#[test]
fn reaping_spares_a_namesakes_pane_the_stale_row_remembers() {
    if !have_tmux() {
        eprintln!("skipping: tmux is not installed");
        return;
    }

    let repo = repo();
    let db = thurbox::storage::Database::open_in_memory().expect("db");
    let _tmux_dir = isolate_tmux();
    let home = tempfile::tempdir().expect("tempdir");
    isolate_paths(home.path());

    let Some(stale) = spawn(&db, repo.path(), "fleet") else {
        return;
    };
    thurbox::session_ops::delete_session_headless(&db, stale.session_id, false).expect("delete");
    let _ = tmux(&["kill-pane", "-t", &stale.backend_id]);

    let Some(live) = spawn(&db, repo.path(), "fleet") else {
        return;
    };

    // Post-restart: the stale row's remembered pane id is now the live
    // namesake's pane, in a window that carries the very name it expects.
    // `set_backend_id` only touches live rows, so the row is revived for the
    // write and deleted again — the persisted state, not the route to it, is
    // what the reap sees.
    db.restore_session(stale.session_id).expect("revive");
    assert!(
        db.set_backend_id(stale.session_id, &live.backend_id)
            .expect("renumber the stale row's pane"),
        "the stale row should be there to update"
    );
    db.soft_delete_session(stale.session_id)
        .expect("soft-delete again");

    let reaped = thurbox::session_ops::reap_soft_deleted(&db, stale.session_id).expect("reap");
    let survived = pane_alive(&live.backend_id);
    let windows_after = windows();
    cleanup();

    assert!(
        survived,
        "reaping the stale 'fleet' row killed the live one's pane {} through a \
         renumbered id (reaped={reaped}); windows left: {windows_after:?}",
        live.backend_id
    );
}

/// A name is claimed by soft-deleted rows too, not just live ones. A row keeps
/// its agent until the reaper lets it go — that is what makes an undo restore a
/// session rather than respawn it — so while one soft-deleted 'fleet' is inside
/// its undo window, an *older* 'fleet' row's reap must not resolve `tb-fleet`
/// and destroy the work the undo would have brought back. The older row's reap
/// still has to collect its own window, which the second half asserts.
#[test]
fn reaping_spares_a_soft_deleted_namesake_still_inside_its_undo_window() {
    if !have_tmux() {
        eprintln!("skipping: tmux is not installed");
        return;
    }

    let repo = repo();
    let db = thurbox::storage::Database::open_in_memory().expect("db");
    let _tmux_dir = isolate_tmux();
    let home = tempfile::tempdir().expect("tempdir");
    isolate_paths(home.path());

    let Some(stale) = spawn(&db, repo.path(), "fleet") else {
        return;
    };
    thurbox::session_ops::delete_session_headless(&db, stale.session_id, false).expect("delete");
    let _ = tmux(&["kill-pane", "-t", &stale.backend_id]);

    // The undoable one: soft-deleted, its agent and window still up, so no
    // *active* session answers to 'fleet' any more.
    let Some(undoable) = spawn(&db, repo.path(), "fleet") else {
        return;
    };
    thurbox::session_ops::delete_session_headless(&db, undoable.session_id, false).expect("delete");
    assert!(
        pane_alive(&undoable.backend_id),
        "a soft delete must leave the window for the undo window"
    );

    let stale_reaped =
        thurbox::session_ops::reap_soft_deleted(&db, stale.session_id).expect("reap stale");
    let survived = pane_alive(&undoable.backend_id);

    // And the strictness is not a leak: the undoable row's own reap, when its
    // turn comes, takes its window down.
    let own_reaped =
        thurbox::session_ops::reap_soft_deleted(&db, undoable.session_id).expect("reap own");
    let released = !pane_alive(&undoable.backend_id);
    let windows_after = windows();
    cleanup();

    assert!(
        survived,
        "reaping the stale 'fleet' row killed the pane {} of a namesake still \
         inside its undo window (reaped={stale_reaped}); windows left: \
         {windows_after:?}",
        undoable.backend_id
    );
    assert!(
        own_reaped,
        "a soft-deleted row with a live pane must be reaped"
    );
    assert!(
        released,
        "the undoable row's own reap must collect its window (pane {} survived)",
        undoable.backend_id
    );
}
