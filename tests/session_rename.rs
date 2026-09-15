//! `thurbox-cli session rename`, through the real binary.
//!
//! The refusals need a row and never a pane, so they seed one and are answered
//! before any multiplexer is asked. The rest reach the window step — a session's
//! windows are named after it, and a rename that left them behind would be half
//! of one — so they skip where tmux is absent, as `tests/create_e2e.rs` does.

#[cfg(unix)]
use std::path::Path;
use std::path::PathBuf;
use std::process::{Command, Output};

use serde_json::Value;
use thurbox::session::SessionId;
use thurbox::sync::SharedSession;

/// The `GIT_*` location variables git exports to hook processes. A suite run
/// under this repository's own pre-commit hook inherits them, and they would
/// point the test repository's git at the real one.
const GIT_LOCATION_ENV: [&str; 3] = ["GIT_DIR", "GIT_WORK_TREE", "GIT_INDEX_FILE"];

fn have_tmux() -> bool {
    Command::new("tmux")
        .arg("-V")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// A throwaway instance: its own config, data, home and multiplexer socket, so
/// nothing here reads or writes the operator's.
struct Env {
    root: tempfile::TempDir,
    /// `TMUX_TMPDIR`, its own short directory: an AF_UNIX socket path is limited
    /// to ~104 bytes, which a tempdir under a long `TMPDIR` blows through.
    sockets: PathBuf,
    socket: String,
}

impl Env {
    fn new() -> Self {
        let root = tempfile::TempDir::new().expect("tempdir");
        for sub in ["home", "config", "data"] {
            std::fs::create_dir_all(root.path().join(sub)).expect("mkdir");
        }
        // Nothing that reaches the network or arms a keeper window.
        std::fs::write(
            root.path().join("config/settings.toml"),
            "[features]\nautomations = false\nversion_check = false\nauto_update = false\n",
        )
        .expect("seed settings");
        std::fs::write(
            root.path().join("config/agents.toml"),
            "default = \"shell\"\n\n[[agents]]\nname = \"shell\"\ncommand = \"sh\"\nargs = []\n",
        )
        .expect("seed agents");
        let socket = format!("thurbox-rename-{}", std::process::id());
        let sockets = std::env::var_os("XDG_RUNTIME_DIR")
            .map(PathBuf::from)
            .filter(|dir| dir.is_dir())
            .unwrap_or_else(std::env::temp_dir)
            .join(&socket);
        std::fs::create_dir_all(&sockets).expect("mkdir sockets");
        Self {
            root,
            sockets,
            socket,
        }
    }

    fn path(&self, sub: &str) -> PathBuf {
        self.root.path().join(sub)
    }

    fn db(&self) -> thurbox::storage::Database {
        thurbox::storage::Database::open(&self.path("data").join("thurbox.db"))
            .expect("open the instance database")
    }

    fn run(&self, args: &[&str]) -> Output {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_thurbox-cli"));
        cmd.args(args)
            .current_dir(self.root.path())
            .env("HOME", self.path("home"))
            .env("USERPROFILE", self.path("home"))
            .env("THURBOX_CONFIG_DIR", self.path("config"))
            .env("THURBOX_DATA_DIR", self.path("data"))
            .env("TMUX_TMPDIR", &self.sockets)
            .env(thurbox::agent::tmux::SOCKET_OVERRIDE_ENV, &self.socket)
            // Run from inside a thurbox pane, the owner names that instance's
            // data dir, the override reads as inherited and is dropped, and the
            // windows land on a derived socket this test never looks at.
            .env_remove(thurbox::agent::tmux::SOCKET_OWNER_ENV)
            .env_remove("TMUX")
            .env_remove("THURBOX_SESSION")
            .env_remove("THURBOX_SESSION_ID");
        for var in GIT_LOCATION_ENV {
            cmd.env_remove(var);
        }
        cmd.output().expect("run thurbox-cli")
    }

    /// A session row with no pane — all a refusal needs.
    fn seed(&self, name: &str) -> SessionId {
        let row = SharedSession {
            id: SessionId::default(),
            name: name.into(),
            agent: "shell".into(),
            backend_id: String::new(),
            backend_type: "local-tmux".into(),
            agent_session_id: None,
            cwd: None,
            additional_dirs: Vec::new(),
            worktrees: Vec::new(),
            shell_backend_id: None,
            parent_session_id: None,
            display_order: None,
            tombstone: false,
            tombstone_at: None,
        };
        self.db().upsert_session(&row).expect("persist");
        row.id
    }

    fn name_of(&self, id: SessionId) -> String {
        self.db()
            .get_session_by_id(id)
            .expect("read the row")
            .expect("the row is still active")
            .name
    }

    /// Gated with its only callers: the Windows `clippy -D warnings` job counts
    /// an unused helper as dead code and fails the build.
    #[cfg(unix)]
    fn tmux(&self, args: &[&str]) -> Output {
        Command::new("tmux")
            .env("TMUX_TMPDIR", &self.sockets)
            .env_remove("TMUX")
            .args(["-L", &self.socket])
            .args(args)
            .output()
            .expect("run tmux")
    }

    /// Every window name on this instance's private server.
    #[cfg(unix)]
    fn windows(&self) -> Vec<String> {
        let out = self.tmux(&["list-windows", "-a", "-F", "#{window_name}"]);
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .map(str::to_string)
            .collect()
    }

    /// Open a session's companion shell window, stamped the way the interface
    /// stamps the one it spawns. Raw tmux because nothing headless opens one.
    #[cfg(unix)]
    fn open_shell_window(&self, session_id: &str, name: &str) {
        let sessions = self.tmux(&["list-sessions", "-F", "#{session_name}"]);
        let target = String::from_utf8_lossy(&sessions.stdout)
            .lines()
            .next()
            .expect("a thurbox tmux session")
            .to_string();
        let out = self.tmux(&[
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
            self.tmux(&["set-option", "-w", "-t", &pane, option, value]);
        }
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        let _ = Command::new("tmux")
            .env("TMUX_TMPDIR", &self.sockets)
            .args(["-L", &self.socket, "kill-server"])
            .output();
        let _ = std::fs::remove_dir_all(&self.sockets);
    }
}

/// Both streams: which one a refusal lands on is `cli_driver_contracts`' to pin.
fn said(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

fn json(out: &Output) -> Value {
    serde_json::from_slice(&out.stdout).unwrap_or_else(|e| panic!("not JSON ({e}):\n{}", said(out)))
}

#[cfg(unix)]
fn git(dir: &Path, args: &[&str]) {
    let mut cmd = Command::new("git");
    for var in GIT_LOCATION_ENV {
        cmd.env_remove(var);
    }
    let ok = cmd
        .args(args)
        .current_dir(dir)
        .output()
        .expect("run git")
        .status
        .success();
    assert!(ok, "git {args:?} failed");
}

/// A repository with one commit — the least a session's cwd can be.
#[cfg(unix)]
fn repo(under: &Path) -> PathBuf {
    let dir = under.join("repo");
    std::fs::create_dir_all(&dir).expect("mkdir");
    git(&dir, &["init", "-q", "-b", "main"]);
    git(&dir, &["config", "user.email", "t@example.com"]);
    git(&dir, &["config", "user.name", "thurbox-test"]);
    git(&dir, &["config", "commit.gpgsign", "false"]);
    std::fs::write(dir.join("README.md"), "# probe\n").expect("write");
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-qm", "init"]);
    dir
}

#[test]
fn a_name_create_would_refuse_is_refused_and_the_row_keeps_its_own() {
    let env = Env::new();
    let id = env.seed("probe");
    let too_long = "n".repeat(65);
    for (bad, reason) in [
        ("", "cannot be empty"),
        ("a/b", "invalid characters"),
        ("a\\b", "invalid characters"),
        ("x..y", "invalid characters"),
        (".hidden", "cannot start with '.'"),
        (too_long.as_str(), "too long"),
    ] {
        let out = env.run(&["session", "rename", "probe", bad]);
        assert!(
            !out.status.success(),
            "{bad:?} must be refused:\n{}",
            said(&out)
        );
        assert!(
            said(&out).contains(reason),
            "refusing {bad:?} must say why ({reason}):\n{}",
            said(&out)
        );
        assert_eq!(env.name_of(id), "probe", "a refused rename changes nothing");
    }
}

#[test]
fn a_name_whose_window_name_another_session_holds_is_refused() {
    // A window is named after its session with everything outside
    // `[A-Za-z0-9_-]` folded to `_`, so `a:b` and `a.b` share `tb-a_b`. Where a
    // window carries no stamp (psmux) that name is all that finds it, and two
    // sessions answering to one window could no longer be told apart.
    let env = Env::new();
    let id = env.seed("probe");
    let other = env.seed("a.b");

    let out = env.run(&["session", "rename", "probe", "a:b"]);
    assert!(
        !out.status.success(),
        "a window-name collision must be refused:\n{}",
        said(&out)
    );
    assert!(
        said(&out).contains(&other.to_string()),
        "the refusal names the session in the way:\n{}",
        said(&out)
    );
    assert_eq!(env.name_of(id), "probe");
}

#[test]
fn a_name_another_session_holds_on_the_same_backend_is_refused() {
    // `session create` lets two sessions share a name by default, and a name
    // matching several is then refused wherever one is typed. A rename is a
    // deliberate choice of name, so it does not walk into that on purpose: it
    // refuses the way `--on-existing fail` does.
    let env = Env::new();
    let id = env.seed("probe");
    let other = env.seed("taken");

    let out = env.run(&["session", "rename", "probe", "taken"]);
    assert!(
        !out.status.success(),
        "a collision must be refused:\n{}",
        said(&out)
    );
    assert!(
        said(&out).contains(&other.to_string()),
        "the refusal names the session in the way:\n{}",
        said(&out)
    );
    assert_eq!(env.name_of(id), "probe");
    assert_eq!(env.name_of(other), "taken");
}

#[test]
fn a_rename_resolves_the_session_like_get_and_answers_in_json() {
    if !have_tmux() {
        eprintln!("skipping: tmux is not installed");
        return;
    }
    let env = Env::new();
    let id = env.seed("probe");
    let prefix = &id.to_string()[..8];

    let out = env.run(&["session", "rename", prefix, "renamed", "--json"]);
    assert!(out.status.success(), "rename failed:\n{}", said(&out));
    let answer = json(&out);
    assert_eq!(answer["renamed"], true, "{answer}");
    assert_eq!(answer["session_id"], id.to_string(), "{answer}");
    assert_eq!(answer["session_name"], "renamed", "{answer}");
    assert_eq!(answer["previous_name"], "probe", "{answer}");
    assert_eq!(env.name_of(id), "renamed");

    // The name it already has is not a collision with itself.
    let again = env.run(&["session", "rename", "renamed", "renamed", "--json"]);
    assert!(
        again.status.success(),
        "no-op rename failed:\n{}",
        said(&again)
    );
    assert_eq!(json(&again)["renamed"], false);
}

#[cfg(unix)]
#[test]
fn renaming_a_running_session_renames_its_windows_and_keeps_its_pane() {
    if !have_tmux() {
        eprintln!("skipping: tmux is not installed");
        return;
    }
    let env = Env::new();
    let repo = repo(env.root.path());
    let created = env.run(&[
        "session",
        "create",
        "--name",
        "probe",
        "--repo-path",
        repo.to_str().expect("utf-8 path"),
        "--agent",
        "shell",
        "--json",
    ]);
    assert!(
        created.status.success(),
        "create failed:\n{}",
        said(&created)
    );
    let id = json(&created)["id"]
        .as_str()
        .expect("the created session's id")
        .to_string();
    // A companion shell is named after the session too, and has to follow it.
    env.open_shell_window(&id, "probe");
    let windows = env.windows();
    assert!(
        windows.iter().any(|w| w == "tb-probe") && windows.iter().any(|w| w == "tbs-probe"),
        "precondition: {windows:?}"
    );

    let out = env.run(&["session", "rename", "probe", "renamed agent"]);
    assert!(out.status.success(), "rename failed:\n{}", said(&out));

    // Sanitised exactly as a spawn names a window, so the two cannot disagree.
    let windows = env.windows();
    assert!(
        windows.iter().any(|w| w == "tb-renamed_agent")
            && windows.iter().any(|w| w == "tbs-renamed_agent")
            && !windows.iter().any(|w| w.ends_with("-probe")),
        "both windows must follow the session: {windows:?}"
    );

    // The pane is still the session's: typing into it by the new name works.
    let sent = env.run(&["session", "send", "renamed agent", "true"]);
    assert!(sent.status.success(), "send after rename:\n{}", said(&sent));
}
