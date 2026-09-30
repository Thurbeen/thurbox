//! Persisted routes — `sessions.backend_type` — read the way they were written.
//!
//! A row keeps meaning what it meant when it was written, whatever a host's
//! preference says now, and a route naming a multiplexer nothing here
//! implements is refused by name rather than driven with some other binary.
//!
//! Remote hosts are reached through a stand-in `ssh` that records what it was
//! asked to run and then fails the way an unreachable host does, so each test
//! reads the argv a real host would have received. POSIX-only for that
//! stand-in, which is a shell script.

#![cfg(unix)]

use std::collections::BTreeSet;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::{Command, Output};

use serde_json::Value;
use thurbox::cli::sessions::{run, Action};
use thurbox::session::SessionId;
use thurbox::session_ops::mirror::{self, Transitive};
use thurbox::storage::Database;
use thurbox::sync::SharedSession;

#[path = "support/tmux_server.rs"]
mod tmux_server;

use tmux_server::TmuxServer;

/// Every name a route may give its multiplexer.
const MULTIPLEXERS: [&str; 4] = ["tmux", "psmux", "rmux", "herdr"];

const AGENTS_TOML: &str =
    "default = \"shell\"\n\n[[agents]]\nname = \"shell\"\ncommand = \"sh\"\nargs = []\n";

/// A throwaway instance whose `ssh` is the recording stand-in.
struct Env {
    root: tempfile::TempDir,
    /// Named outright: a local call made by mistake lands on a server this
    /// test owns and reaps, never on the operator's.
    server: TmuxServer,
}

impl Env {
    fn new(hosts_toml: &str) -> Self {
        let root = tempfile::TempDir::new().expect("tempdir");
        for sub in ["home", "config", "data", "bin"] {
            std::fs::create_dir_all(root.path().join(sub)).expect("mkdir");
        }
        let env = Self {
            root,
            server: TmuxServer::private("thurbox-routes-test"),
        };
        std::fs::write(env.path("config/agents.toml"), AGENTS_TOML).expect("agents.toml");
        std::fs::write(env.path("config/hosts.toml"), hosts_toml).expect("hosts.toml");
        let ssh = env.path("bin/ssh");
        std::fs::write(
            &ssh,
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\nexit 255\n",
                env.path("ssh.log").display()
            ),
        )
        .expect("ssh stand-in");
        std::fs::set_permissions(&ssh, std::fs::Permissions::from_mode(0o700)).expect("chmod");
        env
    }

    fn path(&self, sub: &str) -> PathBuf {
        self.root.path().join(sub)
    }

    fn db(&self) -> Database {
        Database::open(&self.path("data/thurbox.db")).expect("open the instance database")
    }

    /// A row as an earlier build (or a peer) wrote it, spelling and all.
    fn row(&self, name: &str, backend_type: &str) -> SessionId {
        let id = SessionId::default();
        self.db()
            .upsert_session(&session(id, name, backend_type))
            .expect("seed a row");
        id
    }

    fn cli(&self, args: &[&str]) -> Output {
        let path = std::env::var_os("PATH").unwrap_or_default();
        let mut dirs = vec![self.path("bin")];
        dirs.extend(std::env::split_paths(&path));
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_thurbox-cli"));
        cmd.arg("--json").args(args);
        cmd.env("PATH", std::env::join_paths(dirs).expect("PATH"));
        cmd.env("HOME", self.path("home"));
        cmd.env("XDG_DATA_HOME", self.path("home/xdg-data"));
        cmd.env("XDG_CONFIG_HOME", self.path("home/xdg-config"));
        cmd.env("THURBOX_CONFIG_DIR", self.path("config"));
        cmd.env("THURBOX_DATA_DIR", self.path("data"));
        self.server.scope(&mut cmd);
        cmd.env_remove("THURBOX_SESSION");
        cmd.env_remove("THURBOX_SESSION_ID");
        cmd.env_remove("TMUX");
        cmd.env_remove("TMUX_PANE");
        cmd.output().expect("run thurbox-cli")
    }

    fn cli_json(&self, args: &[&str]) -> Value {
        let out = self.cli(args);
        serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
            panic!(
                "thurbox-cli {args:?} printed no JSON ({e}): {}\n{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            )
        })
    }

    /// What the stand-in was asked to run, one call per line.
    fn ssh_calls(&self) -> Vec<String> {
        std::fs::read_to_string(self.path("ssh.log"))
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    }

    /// The multiplexer binaries the host was asked to run.
    fn driven(&self) -> BTreeSet<String> {
        self.ssh_calls()
            .iter()
            .flat_map(|call| {
                call.split_whitespace()
                    .map(str::to_string)
                    .collect::<Vec<_>>()
            })
            .filter(|word| MULTIPLEXERS.contains(&word.as_str()))
            .collect()
    }

    fn forget_ssh_calls(&self) {
        let _ = std::fs::remove_file(self.path("ssh.log"));
    }
}

fn session(id: SessionId, name: &str, backend_type: &str) -> SharedSession {
    SharedSession {
        id,
        name: name.into(),
        agent: "shell".into(),
        backend_id: "%3".into(),
        backend_type: backend_type.into(),
        agent_session_id: Some(format!("conv-{name}")),
        cwd: Some(PathBuf::from("/srv/repo")),
        additional_dirs: Vec::new(),
        worktrees: Vec::new(),
        shell_backend_id: None,
        parent_session_id: None,
        display_order: None,
        tombstone: false,
        tombstone_at: None,
    }
}

fn names(set: &[&str]) -> BTreeSet<String> {
    set.iter().copied().map(str::to_string).collect()
}

/// A host whose preference moved to rmux after its rows were written, driven
/// directly (`share_sessions = false`, so nothing is delegated).
const HOST_NOW_ON_RMUX: &str = "[[hosts]]\n\
     name = \"box\"\n\
     destination = \"e2e@box.invalid\"\n\
     multiplexer = \"rmux\"\n\
     share_sessions = false\n";

/// An unqualified `ssh:box` row was written for tmux — the only thing an
/// unsuffixed key could mean when it was written. The host's preference moving
/// to rmux later must not turn the force-delete, the owed-teardown retry or
/// the headless status poll into rmux calls, while the interface keeps
/// attaching through tmux: that split would leave the row's panes running.
#[test]
fn a_legacy_remote_row_keeps_its_multiplexer_after_the_host_changes_preference() {
    let env = Env::new(HOST_NOW_ON_RMUX);
    let gone = env.row("gone", "ssh:box");
    env.row("live", "ssh:box");

    env.cli(&["session", "delete", &gone.to_string(), "--force"]);
    assert!(
        !env.ssh_calls().is_empty(),
        "the force-delete never reached the host"
    );
    assert_eq!(
        env.driven(),
        names(&["tmux"]),
        "force-delete ran {:?}",
        env.ssh_calls()
    );

    // The host did not answer, so the teardown is owed, and the tick both
    // retries it and polls the live row's status.
    env.forget_ssh_calls();
    env.cli(&["automation", "tick"]);
    assert!(
        !env.ssh_calls().is_empty(),
        "the tick never reached the host"
    );
    assert_eq!(
        env.driven(),
        names(&["tmux"]),
        "the tick ran {:?}",
        env.ssh_calls()
    );
}

const HOST_ON_TMUX: &str = "[[hosts]]\n\
     name = \"box\"\n\
     destination = \"e2e@box.invalid\"\n\
     share_sessions = false\n";

/// A row naming a multiplexer nothing here implements is refused by name. The
/// tmux adapter is not an rmux implementation, and running tmux (the host's
/// preference) or `rmux` through the tmux command grammar would both pretend
/// it were.
#[test]
fn a_row_on_an_unimplemented_multiplexer_drives_no_binary() {
    let env = Env::new(HOST_ON_TMUX);
    let id = env.row("r", "ssh:box:rmux");

    env.cli(&["session", "delete", &id.to_string(), "--force"]);
    assert_eq!(
        env.driven(),
        BTreeSet::new(),
        "an rmux row was driven with {:?}",
        env.ssh_calls()
    );
    let row = env
        .db()
        .get_deleted_session_by_id(id)
        .expect("read the tombstone")
        .expect("the row is tombstoned all the same");
    assert!(row.force_deleted);
}

/// A lifecycle hook is told the host a row runs on, not the host plus the
/// route's multiplexer: `ssh:box:rmux` is on `box`.
#[test]
fn a_lifecycle_hook_is_told_the_bare_host_of_a_qualified_row() {
    let env = Env::new(HOST_ON_TMUX);
    let told = env.path("data/host.txt");
    std::fs::write(
        env.path("config/hooks.toml"),
        format!(
            "[[hooks]]\nevent = \"session.pre_delete\"\ncommand = 'printf %s \"$THURBOX_HOST\" > \"{}\"'\n",
            told.display()
        ),
    )
    .expect("hooks.toml");
    let id = env.row("r", "ssh:box:rmux");

    env.cli(&["session", "delete", &id.to_string(), "--force"]);
    assert_eq!(
        std::fs::read_to_string(&told).expect("the pre-delete hook ran"),
        "box"
    );
}

/// A host alias holding `:` would make `ssh:<alias>` ambiguous with a
/// multiplexer-qualified route, so the config load refuses the entry out loud
/// — `config validate` fails naming it — and every other host still loads.
#[test]
fn a_host_alias_with_a_colon_is_refused_at_config_load() {
    let env = Env::new(
        "[[hosts]]\nname = \"box:rmux\"\ndestination = \"e2e@box.invalid\"\n\n\
         [[hosts]]\nname = \"ok\"\ndestination = \"e2e@ok.invalid\"\n",
    );

    let shown = env.cli_json(&["config", "show"]);
    assert_eq!(
        shown["hosts"]["names"],
        serde_json::json!(["ok"]),
        "{shown}"
    );

    let out = env.cli(&["config", "validate"]);
    assert!(!out.status.success(), "a colon in a host alias validated");
    let report: Value = serde_json::from_slice(&out.stdout).expect("validate prints JSON");
    let problems = report["hosts_toml"]["problems"].to_string();
    assert!(problems.contains("box:rmux"), "{report}");
}

/// What `session list --json` prints for a database — the answer a mirror
/// pass reads from a host.
fn listing(db: &Database, deleted: bool) -> Value {
    run(
        Action::List {
            parent: None,
            deleted,
            verify: false,
        },
        db,
    )
    .expect("session list")
    .json
}

/// A mirrored row keeps the multiplexer the host recorded for it: the host's
/// `local-rmux` (legacy) or `local:psmux` session is `ssh:devbox:<mux>` here,
/// while a host row written before routes carried one stays unqualified and
/// keeps its legacy reading.
#[test]
fn a_mirrored_row_keeps_the_multiplexer_its_host_recorded() {
    let host = Database::open_in_memory().unwrap();
    let qualified = SessionId::default();
    host.upsert_session(&session(qualified, "q", "local-rmux"))
        .unwrap();
    let legacy = SessionId::default();
    host.upsert_session(&session(legacy, "l", "local-tmux"))
        .unwrap();
    let current = SessionId::default();
    host.upsert_session(&session(current, "c", "local:psmux"))
        .unwrap();

    let observer = Database::open_in_memory().unwrap();
    mirror::reconcile_with(
        &observer,
        "ssh:devbox",
        &listing(&host, false),
        &listing(&host, true),
        Transitive::Hide,
    );

    let on = |id| {
        observer
            .get_session_by_id(id)
            .unwrap()
            .expect("mirrored")
            .backend_type
    };
    assert_eq!(on(qualified), "ssh:devbox:rmux");
    assert_eq!(on(current), "ssh:devbox:psmux");
    assert_eq!(on(legacy), "ssh:devbox");

    // And the next pass recognises both as the host's own rather than
    // re-adopting or forgetting them.
    let again = mirror::reconcile_with(
        &observer,
        "ssh:devbox",
        &listing(&host, false),
        &listing(&host, true),
        Transitive::Hide,
    );
    assert!(again.adopted.is_empty(), "re-adopted {:?}", again.adopted);
    assert!(
        again.unknown_local.is_empty(),
        "lost track of {:?}",
        again.unknown_local
    );
}

/// A row stored as `tmux` — the column's old default — is a local session on
/// the local server, so its name is held there like any `local-tmux` row's:
/// a second session of that name would be a second `tb-<name>` window.
#[test]
fn a_legacy_tmux_row_holds_its_name_on_the_local_server() {
    let db = Database::open_in_memory().unwrap();
    let id = SessionId::default();
    db.upsert_session(&session(id, "build", "tmux")).unwrap();

    for key in ["local-tmux", "", "tmux"] {
        let held = thurbox::session_ops::names::live_namesakes(&db, "build", key).unwrap();
        assert_eq!(
            held.iter().map(|s| s.id).collect::<Vec<_>>(),
            vec![id],
            "{key:?}"
        );
        let windows = thurbox::session_ops::names::window_namesakes(&db, "build", key).unwrap();
        assert_eq!(windows.len(), 1, "{key:?}");
    }
}

/// A row whose window cannot be taken down keeps its checkout too: removing a
/// worktree from under an agent that is still running there is the one
/// teardown worse than none. Both stay owed.
#[test]
fn an_undrivable_rows_checkout_outlives_its_window() {
    let env = Env::new(HOST_ON_TMUX);
    let id = SessionId::default();
    let mut row = session(id, "r", "ssh:box:rmux");
    row.worktrees = vec![thurbox::sync::SharedWorktree {
        repo_path: PathBuf::from("/srv/repo"),
        worktree_path: PathBuf::from("/srv/worktrees/r"),
        branch: "r".into(),
        created_by_thurbox: true,
    }];
    env.db().upsert_session(&row).expect("seed a row");

    env.cli(&["session", "delete", &id.to_string(), "--force"]);
    assert_eq!(
        env.ssh_calls(),
        Vec::<String>::new(),
        "the host was asked to change something for a row nothing here drives"
    );
}

/// `stop` on a row nothing here drives refuses, rather than recording a park
/// while the window it could not kill keeps running.
#[test]
fn a_row_on_an_unimplemented_multiplexer_is_not_marked_stopped() {
    let env = Env::new(HOST_ON_TMUX);
    let id = env.row("r", "ssh:box:rmux");

    let out = env.cli(&["session", "stop", &id.to_string()]);
    assert!(!out.status.success(), "stop claimed success");
    assert_eq!(
        env.db().session_stopped_at(id).expect("read the mark"),
        None,
        "the row reads as parked while its window runs"
    );
}

/// A creation that names its multiplexer launches with it, whatever the
/// host's entry prefers: the row says `ssh:box:tmux`, so its window has to be
/// on tmux, not on the preference.
#[test]
fn a_created_session_is_launched_with_the_multiplexer_its_route_names() {
    let env = Env::new(HOST_NOW_ON_RMUX);
    let repo = env.path("repo");
    std::fs::create_dir_all(&repo).expect("repo dir");

    env.cli(&[
        "session",
        "create",
        "--name",
        "made",
        "--repo-path",
        repo.to_str().expect("utf-8 path"),
        "--host",
        "box",
        "--multiplexer",
        "tmux",
    ]);
    assert!(
        !env.driven().is_empty(),
        "the create never asked the host's multiplexer: {:?}",
        env.ssh_calls()
    );
    assert_eq!(
        env.driven(),
        names(&["tmux"]),
        "create ran {:?}",
        env.ssh_calls()
    );
}

/// This machine runs its platform default and nothing else, so a local row
/// naming another multiplexer is refused by `stop` too, rather than parked
/// while a window on some other server runs on.
#[test]
fn a_local_row_on_another_multiplexer_is_not_marked_stopped() {
    let env = Env::new("");
    let id = env.row("r", "local:rmux");

    let out = env.cli(&["session", "stop", &id.to_string()]);
    assert!(!out.status.success(), "stop claimed success");
    assert_eq!(
        env.db().session_stopped_at(id).expect("read the mark"),
        None
    );
}

/// The `GIT_*` location variables git exports to hook processes (the list
/// `git::GIT_LOCATION_ENV` scrubs, which is crate-private): a suite run from
/// this repository's own pre-commit hook would otherwise aim every git here at
/// the real repository.
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

fn have(program: &str) -> bool {
    Command::new(program)
        .arg("--version")
        .output()
        .map(|out| out.status.success())
        .unwrap_or(false)
        || Command::new(program)
            .arg("-V")
            .output()
            .map(|out| out.status.success())
            .unwrap_or(false)
}

fn git(dir: &std::path::Path, args: &[&str]) {
    let mut cmd = Command::new("git");
    cmd.args(args).current_dir(dir);
    for var in GIT_LOCATION_ENV {
        cmd.env_remove(var);
    }
    assert!(
        cmd.output().expect("run git").status.success(),
        "git {args:?}"
    );
}

/// The tmux session the local backend groups its windows under in a test
/// build (`backend::tmux::TMUX_SESSION`, private; `thurbox-dev` because a test
/// build carries the dev marker).
const LOCAL_SESSION: &str = "thurbox-dev";

impl Env {
    /// A repository with one commit and a live thurbox-made worktree of it on
    /// branch `name`, holding uncommitted work.
    fn checkout(&self, name: &str) -> thurbox::sync::SharedWorktree {
        let repo = self.path("repo");
        std::fs::create_dir_all(&repo).expect("repo dir");
        git(&repo, &["init", "-q", "-b", "main"]);
        git(&repo, &["config", "user.email", "t@example.com"]);
        git(&repo, &["config", "user.name", "routes-e2e"]);
        git(&repo, &["config", "commit.gpgsign", "false"]);
        std::fs::write(repo.join("README.md"), "# probe\n").expect("write");
        git(&repo, &["add", "."]);
        git(&repo, &["commit", "-qm", "init"]);
        let worktree = self.path(&format!("worktrees/{name}"));
        git(
            &repo,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                name,
                worktree.to_str().expect("utf-8"),
            ],
        );
        std::fs::write(worktree.join("unsaved.txt"), "work in progress").expect("work");
        thurbox::sync::SharedWorktree {
            repo_path: repo,
            worktree_path: worktree,
            branch: name.into(),
            created_by_thurbox: true,
        }
    }

    /// A window on this machine's own server called `tb-<name>` with no owner
    /// stamp — what every psmux window is, and every tmux one made before
    /// stamping — belonging to some other session of that name.
    fn unstamped_namesake(&self, name: &str) {
        let window = format!("tb-{name}");
        let out = self.server.tmux(&[
            "new-session",
            "-d",
            "-s",
            LOCAL_SESSION,
            "-n",
            &window,
            "sleep",
            "600",
        ]);
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    fn local_windows(&self) -> Vec<String> {
        let out = self
            .server
            .tmux(&["list-windows", "-t", LOCAL_SESSION, "-F", "#{window_name}"]);
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .map(str::to_string)
            .collect()
    }
}

/// A local row naming a multiplexer this machine does not run lives on a
/// server nothing here drives, so a force-delete fails closed: it neither
/// removes the checkout its agent may still be working in, nor kills the
/// same-named window of another session on the local server, nor marks the
/// row force-deleted as though its teardown had happened.
#[test]
fn force_deleting_a_local_row_on_another_multiplexer_fails_closed() {
    if !have("git") || !have("tmux") {
        eprintln!("skipping: needs git and tmux");
        return;
    }
    for backend in ["local:rmux", "local-rmux"] {
        let env = Env::new("");
        let checkout = env.checkout("r");
        env.unstamped_namesake("r");
        let id = SessionId::default();
        let mut row = session(id, "r", backend);
        row.backend_id = String::new();
        row.cwd = Some(checkout.worktree_path.clone());
        row.worktrees = vec![checkout.clone()];
        env.db().upsert_session(&row).expect("seed a row");

        let out = env.cli(&["session", "delete", &id.to_string(), "--force"]);

        assert!(
            checkout.worktree_path.join("unsaved.txt").exists(),
            "{backend}: force-delete removed a checkout nothing here drives"
        );
        assert!(
            env.local_windows().contains(&"tb-r".to_string()),
            "{backend}: force-delete killed a local namesake's window"
        );
        assert!(
            !out.status.success(),
            "{backend}: force-delete claimed success"
        );
        assert!(
            env.db().get_session_by_id(id).expect("read").is_some(),
            "{backend}: the row was deleted though nothing was torn down"
        );
    }
}

/// The same for the reap of a soft-deleted one: its windows are on a server
/// nothing here drives, so the local window a name resolves to is somebody
/// else's, and the reap kills nothing.
#[test]
fn reaping_a_local_row_on_another_multiplexer_kills_no_local_window() {
    if !have("tmux") {
        eprintln!("skipping: needs tmux");
        return;
    }
    for backend in ["local:rmux", "local-rmux"] {
        let env = Env::new("");
        env.unstamped_namesake("r");
        let id = SessionId::default();
        let mut row = session(id, "r", backend);
        row.backend_id = String::new();
        env.db().upsert_session(&row).expect("seed a row");
        env.db().soft_delete_session(id).expect("soft delete");

        let out = env.cli(&["session", "reap", &id.to_string()]);

        assert!(
            env.local_windows().contains(&"tb-r".to_string()),
            "{backend}: the reap killed a local namesake's window"
        );
        assert!(!out.status.success(), "{backend}: the reap claimed success");
    }
}

/// A Windows host whose entry names its platform, on a multiplexer that is not
/// psmux, and never delegated to (`share_sessions = false`).
const WINDOWS_HOST_ON_TMUX: &str = "[[hosts]]\n\
     name = \"box\"\n\
     destination = \"e2e@box.invalid\"\n\
     platform = \"windows\"\n\
     multiplexer = \"tmux\"\n\
     share_sessions = false\n";

/// A host's platform is its own, not its multiplexer's: a Windows host on tmux
/// is asked for `%USERPROFILE%` in PowerShell, never for a `$HOME` its shell
/// would echo back literally. The
/// stand-in `ssh` records what the host would have been sent.
#[test]
fn a_windows_host_on_a_non_psmux_multiplexer_is_asked_natively() {
    let env = Env::new(WINDOWS_HOST_ON_TMUX);
    let repo = env.path("repo");
    std::fs::create_dir_all(&repo).expect("repo dir");

    env.cli(&[
        "session",
        "create",
        "--name",
        "made",
        "--repo-path",
        repo.to_str().expect("utf-8 path"),
        "--host",
        "box",
    ]);
    let calls = env.ssh_calls();
    assert!(!calls.is_empty(), "the create never reached the host");
    assert!(
        calls.iter().any(|call| call.contains("$env:USERPROFILE")),
        "a Windows host's home was not asked of PowerShell: {calls:?}"
    );
    assert!(
        !calls.iter().any(|call| call.contains("$HOME")),
        "a Windows host was asked for a POSIX $HOME: {calls:?}"
    );
}

/// The same entry without a platform is a POSIX host, which is what it always
/// was: an entry that never named one keeps its old meaning.
#[test]
fn a_host_entry_without_a_platform_keeps_its_posix_meaning() {
    let env = Env::new(HOST_ON_TMUX);
    let repo = env.path("repo");
    std::fs::create_dir_all(&repo).expect("repo dir");

    env.cli(&[
        "session",
        "create",
        "--name",
        "made",
        "--repo-path",
        repo.to_str().expect("utf-8 path"),
        "--host",
        "box",
    ]);
    let calls = env.ssh_calls();
    assert!(
        calls.iter().any(|call| call.contains("echo $HOME")),
        "a POSIX host's home was not asked of its shell: {calls:?}"
    );
    assert!(
        !calls.iter().any(|call| call.contains("USERPROFILE")),
        "a POSIX host was asked for a Windows profile: {calls:?}"
    );
}
