//! Tearing a session down must not kill a live session's window.
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
//! The fix is not reap-shaped, so neither is this suite any more: every window
//! carries the id of the session row that owns it (`@thurbox_session`, ADR-25),
//! and force delete, stop and restart resolve that stamp exactly as the reap
//! does. The last test here walks all three.
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

/// Open a session's companion shell window, stamped the way the interface
/// stamps the one it spawns. Raw tmux because nothing headless opens a shell:
/// it is created lazily by `Session::ensure_shell_pane` when the user asks for
/// it, which is exactly why so many rows have no `shell_backend_id` and the
/// window has to be found by its stamp.
fn open_shell_window(session_id: &str, name: &str) -> String {
    let sessions = tmux(&["list-sessions", "-F", "#{session_name}"]);
    let target = String::from_utf8_lossy(&sessions.stdout)
        .lines()
        .next()
        .expect("a thurbox tmux session")
        .to_string();
    let out = tmux(&[
        "new-window",
        "-d",
        "-t",
        &target,
        "-n",
        &format!("tbs-{name}"),
        "-P",
        "-F",
        "#{pane_id}",
        "sh",
    ]);
    let pane = String::from_utf8_lossy(&out.stdout).trim().to_string();
    assert!(pane.starts_with('%'), "new-window said {pane:?}");
    for (option, value) in [
        (thurbox::agent::tmux::WINDOW_SESSION_OPTION, session_id),
        (thurbox::agent::tmux::WINDOW_ROLE_OPTION, "shell"),
    ] {
        tmux(&["set-option", "-w", "-t", &pane, option, value]);
    }
    pane
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
            existing_worktree: None,
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

/// The name a reap resolves is not the session's name but the *window's*, and
/// `sanitize_window_name` maps every character outside `[A-Za-z0-9_-]` to `_`.
/// So two legal, distinct session names — 'fleet 1' and 'fleet_1' — share one
/// `tb-fleet_1`, and an ownership test settled on the raw names would call the
/// stale row's name unclaimed and hand it its namesake's live window.
#[test]
fn reaping_spares_a_live_window_whose_name_only_collides_once_sanitized() {
    if !have_tmux() {
        eprintln!("skipping: tmux is not installed");
        return;
    }

    let repo = repo();
    let db = thurbox::storage::Database::open_in_memory().expect("db");
    let _tmux_dir = isolate_tmux();
    let home = tempfile::tempdir().expect("tempdir");
    isolate_paths(home.path());

    let Some(stale) = spawn(&db, repo.path(), "fleet 1") else {
        return;
    };
    thurbox::session_ops::delete_session_headless(&db, stale.session_id, false).expect("delete");
    // Its agent exits, so the row's pane id resolves to nothing and only the
    // name is left to go on.
    let _ = tmux(&["kill-pane", "-t", &stale.backend_id]);
    assert!(
        !pane_alive(&stale.backend_id),
        "the stale row's pane should be gone"
    );

    // A different name, the same window: `tb-fleet_1`.
    let Some(live) = spawn(&db, repo.path(), "fleet_1") else {
        return;
    };
    assert_ne!(stale.session_id, live.session_id);
    assert!(
        pane_alive(&live.backend_id),
        "the replacement should be running"
    );

    let reaped = thurbox::session_ops::reap_soft_deleted(&db, stale.session_id).expect("reap");
    let survived = pane_alive(&live.backend_id);
    let windows_after = windows();
    cleanup();

    assert!(
        survived,
        "reaping 'fleet 1' killed the live 'fleet_1' pane {} through the window \
         name the two share (reaped={reaped}); windows left: {windows_after:?}",
        live.backend_id
    );
}

/// The reap was never the only path that killed by name. Force delete, `stop`
/// and `restart` all resolved `tb-<name>` too, so each of them destroyed a live
/// namesake's window whenever the row being torn down no longer had one of its
/// own — the state a frozen-and-recreated session leaves behind. Ownership is
/// one rule now, so one test walks all three.
///
/// Each path gets a name of its own, and so a live namesake of its own: sharing
/// one would make the second and third answers vacuous the moment the first
/// path killed it.
#[test]
fn force_delete_stop_and_restart_all_spare_a_live_namesakes_window() {
    if !have_tmux() {
        eprintln!("skipping: tmux is not installed");
        return;
    }

    let repo = repo();
    let db = thurbox::storage::Database::open_in_memory().expect("db");
    let _tmux_dir = isolate_tmux();
    let home = tempfile::tempdir().expect("tempdir");
    isolate_paths(home.path());

    // Per path: a stale row whose own window is gone, beside a live namesake.
    let mut cases = Vec::new();
    for name in ["fleet-delete", "fleet-stop", "fleet-restart"] {
        let Some(stale) = spawn(&db, repo.path(), name) else {
            return;
        };
        let _ = tmux(&["kill-pane", "-t", &stale.backend_id]);
        let Some(live) = spawn(&db, repo.path(), name) else {
            return;
        };
        assert!(
            pane_alive(&live.backend_id),
            "the live '{name}' should be up"
        );
        cases.push((stale, live));
    }

    let forced = thurbox::session_ops::delete_session_headless(&db, cases[0].0.session_id, true);
    let after_delete = pane_alive(&cases[0].1.backend_id);

    let stopped = thurbox::session_ops::restart::stop_session_headless(&db, cases[1].0.session_id);
    let after_stop = pane_alive(&cases[1].1.backend_id);

    let restarted = thurbox::session_ops::restart_session_headless(&db, cases[2].0.session_id);
    let after_restart = pane_alive(&cases[2].1.backend_id);

    let windows_after = windows();
    cleanup();

    assert!(forced.is_ok(), "force delete: {forced:?}");
    assert!(stopped.is_ok(), "stop: {stopped:?}");
    assert!(restarted.is_ok(), "restart: {restarted:?}");
    // Reported together, so one run says which paths killed rather than only
    // the first.
    assert_eq!(
        (after_delete, after_stop, after_restart),
        (true, true, true),
        "tearing a stale row down killed a live namesake's window \
         (delete kept={after_delete}, stop kept={after_stop}, \
         restart kept={after_restart}); windows left: {windows_after:?}"
    );
}

/// The other half of that contract, for the path with the least cover: a
/// force delete must still take down the window the row really does own.
#[test]
fn force_delete_still_kills_the_rows_own_window() {
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
    let report = thurbox::session_ops::delete_session_headless(&db, session.session_id, true)
        .expect("force delete");
    let still_there = pane_alive(&session.backend_id);
    cleanup();

    assert!(report.killed_window, "the force delete reported no kill");
    assert!(
        !still_there,
        "the force delete must kill the row's own window (pane {} survived)",
        session.backend_id
    );
}

/// A row that never recorded a pane id — the psmux shape, and every row
/// persisted before local spawns reported one — is still found by its stamp.
/// Nothing here consults `backend_id`, which is the whole point: after a tmux
/// server restart it names somebody else's pane.
#[test]
fn a_row_with_no_pane_id_still_resolves_its_own_stamped_window() {
    if !have_tmux() {
        eprintln!("skipping: tmux is not installed");
        return;
    }

    let repo = repo();
    let db = thurbox::storage::Database::open_in_memory().expect("db");
    let _tmux_dir = isolate_tmux();
    let home = tempfile::tempdir().expect("tempdir");
    isolate_paths(home.path());

    let Some(session) = spawn(&db, repo.path(), "stamped") else {
        return;
    };
    db.set_backend_id(session.session_id, "")
        .expect("clear the pane id");

    let located =
        thurbox::agent::tmux::agent_window(None, &session.session_id.to_string(), "stamped");
    let outcome = located.map(|l| l.pane());
    cleanup();

    assert_eq!(
        outcome.expect("list windows"),
        Some(session.backend_id.clone()),
        "the window's stamp, not the row's pane id, is what finds it"
    );
}

/// Restoring a session must not adopt a live namesake's window.
///
/// `respawn` asks "is the window still alive? then adopt it rather than
/// launching a second agent" — a real case, since a soft-deleted row keeps its
/// agent until the reaper lets it go. Resolved by name, that adopted whichever
/// `tb-<name>` was there, putting two rows on one pane: the next kill by id
/// then destroys the other session's agent. Extension self-heal sets this up
/// routinely, since it matches its declared sessions by name.
#[test]
fn restoring_a_session_never_adopts_a_live_namesakes_window() {
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
    // Its agent exits, so the row has no window of its own to come back to.
    let _ = tmux(&["kill-pane", "-t", &stale.backend_id]);

    let Some(live) = spawn(&db, repo.path(), "fleet") else {
        return;
    };

    let restored = thurbox::session_ops::restore_session_headless(&db, stale.session_id, false);
    let adopted = db
        .get_session_by_id(stale.session_id)
        .expect("query")
        .map(|row| row.backend_id);
    let survived = pane_alive(&live.backend_id);
    let windows_after = windows();
    cleanup();

    assert!(restored.is_ok(), "restore: {restored:?}");
    assert!(survived, "the live namesake's pane must still be running");
    assert_ne!(
        adopted.as_deref(),
        Some(live.backend_id.as_str()),
        "the restore adopted the live namesake's pane {} instead of spawning \
         its own agent; windows left: {windows_after:?}",
        live.backend_id
    );
}

/// The companion shell (`tbs-`) is the second window a session owns, and until
/// now no teardown path read it: `Session::kill_shell_pane` has no headless
/// caller, and `shell_backend_id` is written only once the interface opens one,
/// so even a kill by pane id would have missed most of them. The result was a
/// `tbs-` window per force-deleted or reaped session, alive for as long as the
/// server was — the orphans seen on the operator's own machine and on the
/// remote host.
///
/// Both teardowns are walked here, each on its own session, because the second
/// would be vacuous if the first had already taken the window down.
#[test]
fn force_delete_and_reap_both_collect_the_companion_shell() {
    if !have_tmux() {
        eprintln!("skipping: tmux is not installed");
        return;
    }

    let repo = repo();
    let db = thurbox::storage::Database::open_in_memory().expect("db");
    let _tmux_dir = isolate_tmux();
    let home = tempfile::tempdir().expect("tempdir");
    isolate_paths(home.path());

    let Some(forced) = spawn(&db, repo.path(), "forced") else {
        return;
    };
    let forced_shell = open_shell_window(&forced.session_id.to_string(), "forced");
    let Some(reaped) = spawn(&db, repo.path(), "reaped") else {
        return;
    };
    let reaped_shell = open_shell_window(&reaped.session_id.to_string(), "reaped");

    thurbox::session_ops::delete_session_headless(&db, forced.session_id, true).expect("delete");
    let forced_shell_alive = pane_alive(&forced_shell);

    thurbox::session_ops::delete_session_headless(&db, reaped.session_id, false).expect("delete");
    thurbox::session_ops::reap_soft_deleted(&db, reaped.session_id).expect("reap");
    let reaped_shell_alive = pane_alive(&reaped_shell);
    let windows_after = windows();
    cleanup();

    assert!(
        !forced_shell_alive,
        "force delete left the shell pane {forced_shell} running; windows: {windows_after:?}"
    );
    assert!(
        !reaped_shell_alive,
        "the reap left the shell pane {reaped_shell} running; windows: {windows_after:?}"
    );
}

/// And the shell is held to the same ownership rule as the agent: `tbs-<name>`
/// is no more unique than `tb-<name>`, so a teardown that reached for the name
/// would take a live namesake's shell down with it.
#[test]
fn a_teardown_spares_a_live_namesakes_companion_shell() {
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
    // Its own windows go: the row is left owning nothing, which is when a
    // teardown used to fall through to the name.
    let _ = tmux(&["kill-pane", "-t", &stale.backend_id]);

    let Some(live) = spawn(&db, repo.path(), "fleet") else {
        return;
    };
    let live_shell = open_shell_window(&live.session_id.to_string(), "fleet");

    thurbox::session_ops::delete_session_headless(&db, stale.session_id, true).expect("delete");
    let survived = pane_alive(&live_shell);
    let windows_after = windows();
    cleanup();

    assert!(
        survived,
        "force-deleting the stale 'fleet' row killed the live one's shell pane \
         {live_shell}; windows left: {windows_after:?}"
    );
}

/// A teardown asks the server what it holds; it must not bring one into being.
///
/// The remote path used to call `ensure_ready` first, which starts the server
/// *and* creates the thurbox session on the host — so a one-shot `thurbox-cli`
/// tearing a session down left an empty server on somebody else's machine,
/// often on a socket the host's own thurbox does not even use. Both paths read
/// the same one-shot listing now, and this pins the property where it can be
/// observed: on a socket with nothing running.
#[test]
fn a_teardown_never_brings_a_tmux_server_into_being() {
    if !have_tmux() {
        eprintln!("skipping: tmux is not installed");
        return;
    }

    let db = thurbox::storage::Database::open_in_memory().expect("db");
    let _tmux_dir = isolate_tmux();
    let home = tempfile::tempdir().expect("tempdir");
    isolate_paths(home.path());
    // Nothing was spawned, so there is no server on this socket.
    assert!(!tmux(&["has-session"]).status.success());

    let id = thurbox::session::SessionId::default();
    let _ = thurbox::agent::tmux::kill_window(&id.to_string(), "ghost");
    let _ = thurbox::agent::tmux::kill_shell_window(&id.to_string(), "ghost");
    let _ = thurbox::session_ops::reap_soft_deleted(&db, id);

    let started = tmux(&["has-session"]).status.success();
    cleanup();
    assert!(!started, "a teardown started a tmux server on the socket");
}

/// The one reaper is DB-driven, and this is what that buys: a row soft-deleted
/// while no interface was running is collected by the next sweep from a process
/// that never saw it alive. The reaper it replaced watched ids leave a
/// snapshot, so it held no opinion at all about a session it had never had in
/// one — and a restart of the interface lost the opinions it did hold.
#[test]
fn the_sweep_collects_a_row_deleted_while_nothing_was_watching() {
    if !have_tmux() {
        eprintln!("skipping: tmux is not installed");
        return;
    }

    let repo = repo();
    let db = thurbox::storage::Database::open_in_memory().expect("db");
    let _tmux_dir = isolate_tmux();
    let home = tempfile::tempdir().expect("tempdir");
    isolate_paths(home.path());

    let Some(session) = spawn(&db, repo.path(), "unwatched") else {
        return;
    };
    thurbox::session_ops::delete_session_headless(&db, session.session_id, false).expect("delete");

    // Inside the undo window: the sweep leaves it, and the agent runs on.
    assert!(
        thurbox::session_ops::reap_overdue_soft_deletes(&db).is_empty(),
        "a delete still inside its undo window is not overdue"
    );
    let untouched = pane_alive(&session.backend_id);

    // Now past it. Nothing here has ever held the id in a snapshot.
    db.conn_ref()
        .execute(
            "UPDATE sessions SET deleted_at = 0 WHERE id = ?1",
            [session.session_id.to_string()],
        )
        .expect("backdate the delete");
    let reaped = thurbox::session_ops::reap_overdue_soft_deletes(&db);
    let collected = !pane_alive(&session.backend_id);
    // Idempotent: the row owns nothing on the next pass, so it is not reported
    // again on every tick for as long as it stays deleted.
    let second = thurbox::session_ops::reap_overdue_soft_deletes(&db);
    cleanup();

    assert!(untouched, "the agent runs on until the undo window closes");
    assert_eq!(
        reaped,
        vec![session.session_id.to_string()],
        "the overdue row is the one the sweep collects"
    );
    assert!(collected, "and its window comes down");
    assert!(second.is_empty(), "a row reaped once owns nothing to reap");
}
