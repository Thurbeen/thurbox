//! A test run leaves no tmux server behind.
//!
//! thurbox injects `THURBOX_SOCKET` **and** `THURBOX_SOCKET_FOR` into every
//! pane it spawns, so a suite run from inside a thurbox session — which is how
//! this repository is developed — inherits both. `agent::tmux::socket_for`
//! reads the pair: an override tagged for somebody else's data directory is an
//! inherited one and is dropped, and an instance that also relocated
//! `THURBOX_DATA_DIR` then lands on a socket *derived* from that directory.
//!
//! That rule is right — a harness that isolated its database but not its server
//! would be spawning windows on the operator's tmux — but it means a harness
//! which pins a socket without clearing the inherited tag spawns its windows on
//! a name it never chose, and the `kill-server` in its teardown kills a name
//! nothing created. One orphan tmux server, with the agent processes inside it,
//! per test run, forever. That is how a developer's machine came to hold a
//! hundred of them.
//!
//! Three tests: the resolution that makes the mistake possible, the recipe that
//! avoids it end to end, and a gate holding every other harness in this
//! directory to that recipe — because the leak is silent, and a run that leaks
//! still passes every assertion it makes.

use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;

/// The `GIT_*` location variables git exports to hook processes, scrubbed so a
/// suite running under this repository's own pre-commit hook does not point the
/// spawn at the real repository. Mirrors `tui_e2e`'s list.
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

fn have_tmux() -> bool {
    Command::new("tmux")
        .arg("-V")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn git(dir: &Path, args: &[&str]) {
    let mut cmd = Command::new("git");
    cmd.args(args).current_dir(dir);
    for var in GIT_LOCATION_ENV {
        cmd.env_remove(var);
    }
    let ok = cmd.output().expect("run git").status.success();
    assert!(ok, "git {args:?} failed");
}

/// A repository with one commit — the minimum a session needs.
fn repo(under: &Path) -> PathBuf {
    let dir = under.join("repo");
    std::fs::create_dir_all(&dir).expect("mkdir");
    git(&dir, &["init", "-q", "-b", "main"]);
    git(&dir, &["config", "user.email", "t@example.com"]);
    git(&dir, &["config", "user.name", "thurbox-leak"]);
    git(&dir, &["config", "commit.gpgsign", "false"]);
    std::fs::write(dir.join("README.md"), "# probe\n").expect("write");
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-qm", "init"]);
    dir
}

/// One isolated instance: its own config and data, its own socket *name*, and
/// its own socket *directory* — so "did this run leak a server" is answerable
/// by looking at one directory nothing else writes to.
struct Profile {
    root: tempfile::TempDir,
    /// `TMUX_TMPDIR`. An AF_UNIX path is limited to ~104 bytes, so this is a
    /// short directory of its own rather than one under the tempdir.
    sockets: PathBuf,
    socket: String,
}

impl Profile {
    fn new() -> Self {
        let root = tempfile::tempdir().expect("tempdir");
        for sub in ["home", "config", "data"] {
            std::fs::create_dir_all(root.path().join(sub)).expect("mkdir");
        }
        // Per *profile*, not per process: `cargo test` runs this file's tests in
        // one process on several threads, and a shared name would have one
        // test's `Drop` remove the socket directory another is still using.
        // (nextest, the runner this repo uses, gives each test a process of its
        // own and would have hidden that.)
        static NTH: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let nth = NTH.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let socket = format!("thurbox-leak-{}-{nth}", std::process::id());
        let sockets = std::env::var_os("XDG_RUNTIME_DIR")
            .map(PathBuf::from)
            .filter(|dir| dir.is_dir())
            .unwrap_or_else(std::env::temp_dir)
            .join(&socket);
        std::fs::create_dir_all(&sockets).expect("mkdir sockets");
        // No network and no heartbeat keeper: what is counted below is the
        // server this test asked for, not one a background feature armed.
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
        Self {
            root,
            sockets,
            socket,
        }
    }

    fn path(&self, sub: &str) -> PathBuf {
        self.root.path().join(sub)
    }

    /// Run `thurbox-cli` in this profile.
    ///
    /// `inherit_owner` decides whether the run carries the `THURBOX_SOCKET_FOR`
    /// tag a suite started inside a thurbox pane inherits — the one variable
    /// between a harness that scopes itself and one that only thinks it does.
    fn cli(&self, args: &[&str], inherit_owner: bool) -> std::process::Output {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_thurbox-cli"));
        cmd.args(args);
        cmd.current_dir(self.root.path());
        cmd.env("HOME", self.path("home"));
        cmd.env("USERPROFILE", self.path("home"));
        cmd.env("THURBOX_CONFIG_DIR", self.path("config"));
        cmd.env("THURBOX_DATA_DIR", self.path("data"));
        cmd.env("TMUX_TMPDIR", &self.sockets);
        cmd.env(thurbox::agent::tmux::SOCKET_OVERRIDE_ENV, &self.socket);
        if inherit_owner {
            cmd.env(
                thurbox::agent::tmux::SOCKET_OWNER_ENV,
                "/somebody/elses/data/dir",
            );
        } else {
            cmd.env_remove(thurbox::agent::tmux::SOCKET_OWNER_ENV);
        }
        cmd.env_remove("TMUX");
        cmd.env_remove("THURBOX_SESSION");
        cmd.env_remove("THURBOX_SESSION_ID");
        for var in GIT_LOCATION_ENV {
            cmd.env_remove(var);
        }
        cmd.output().expect("run thurbox-cli")
    }

    /// The socket this run actually resolved, as `version --json` reports it.
    fn resolved_socket(&self, inherit_owner: bool) -> String {
        let out = self.cli(&["version", "--json"], inherit_owner);
        assert!(
            out.status.success(),
            "thurbox-cli version failed:\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let doc: Value = serde_json::from_slice(&out.stdout).expect("version --json is JSON");
        doc["tmux_socket"]
            .as_str()
            .expect("version --json reports tmux_socket")
            .to_string()
    }

    /// Every socket file under this profile's private `TMUX_TMPDIR`. tmux nests
    /// them one level down (`tmux-<uid>/<name>`), so this walks rather than
    /// lists — and the directory is this instance's alone, so every name here
    /// is a server this run asked for.
    ///
    /// A *file*, not a server: tmux never unlinks a socket, so one outlives the
    /// server that made it. That is why a private socket directory is part of
    /// the recipe rather than a nicety — a harness on the shared one leaves a
    /// dead socket behind on every run however carefully it kills its server.
    /// [`Self::servers`] is the liveness question.
    fn sockets(&self) -> Vec<String> {
        fn walk(dir: &Path, found: &mut Vec<String>) {
            let Ok(entries) = std::fs::read_dir(dir) else {
                return;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    walk(&path, found);
                } else {
                    found.push(entry.file_name().to_string_lossy().into_owned());
                }
            }
        }
        let mut found = Vec::new();
        walk(&self.sockets, &mut found);
        found.sort();
        found
    }

    /// The sockets in this profile that a tmux server still answers on.
    fn servers(&self) -> Vec<String> {
        self.sockets()
            .into_iter()
            .filter(|socket| {
                Command::new("tmux")
                    .env("TMUX_TMPDIR", &self.sockets)
                    .args(["-L", socket, "list-sessions"])
                    .output()
                    .is_ok_and(|out| out.status.success())
            })
            .collect()
    }
}

impl Drop for Profile {
    fn drop(&mut self) {
        // Every socket that turned up, not just the pinned one: a scenario here
        // deliberately runs unscoped, and its server is this test's to reap.
        for socket in self.sockets() {
            let _ = Command::new("tmux")
                .env("TMUX_TMPDIR", &self.sockets)
                .args(["-L", &socket, "kill-server"])
                .output();
        }
        let _ = std::fs::remove_dir_all(&self.sockets);
    }
}

/// An inherited `THURBOX_SOCKET_FOR` moves a relocated instance off the socket
/// it was told to use — which is why clearing it is a harness's job and not a
/// nicety.
///
/// The product behaviour is deliberate (`socket_for`): a name injected for
/// another instance must not win. Pinned here so the cost of leaving the tag in
/// place stays visible, because it is invisible everywhere else — the run
/// succeeds, the assertions pass, and only the leftover server says otherwise.
#[test]
fn an_inherited_owner_tag_moves_a_relocated_instance_off_its_pinned_socket() {
    let profile = Profile::new();

    assert_eq!(
        profile.resolved_socket(false),
        profile.socket,
        "a cleared tag leaves the pinned socket in force"
    );
    assert_ne!(
        profile.resolved_socket(true),
        profile.socket,
        "an inherited tag is what makes the pin silently stop applying"
    );
}

/// The recipe end to end: a run that pins a socket, clears the tag and keeps
/// its own socket directory puts its windows where it said, and killing that
/// one server leaves nothing behind.
///
/// Both halves matter: a run that landed somewhere else would also leave the
/// pinned socket clean, so the first assertion is what makes the second mean
/// "nothing leaked" rather than "nothing was ever there".
#[test]
fn a_scoped_run_leaves_no_tmux_server_behind() {
    if !have_tmux() {
        eprintln!("skipping: tmux is not installed");
        return;
    }
    let profile = Profile::new();
    let repo = repo(profile.root.path());

    let created = profile.cli(
        &[
            "session",
            "create",
            "--name",
            "probe",
            "--repo-path",
            repo.to_str().expect("utf-8 path"),
            "--agent",
            "shell",
        ],
        false,
    );
    assert!(
        created.status.success(),
        "session create failed:\n{}",
        String::from_utf8_lossy(&created.stderr)
    );

    assert_eq!(
        profile.sockets(),
        vec![profile.socket.clone()],
        "the run put its windows on a socket it did not pin — its teardown \
         would kill a name nothing created and leave a server running"
    );

    let killed = Command::new("tmux")
        .env("TMUX_TMPDIR", &profile.sockets)
        .args(["-L", &profile.socket, "kill-server"])
        .output()
        .expect("run tmux");
    assert!(
        killed.status.success(),
        "kill-server failed: {}",
        String::from_utf8_lossy(&killed.stderr)
    );

    assert!(
        profile.servers().is_empty(),
        "a tmux server outlived the run: {:?}",
        profile.servers()
    );
}

/// Every harness that pins a tmux socket clears the inherited owner tag and
/// gives itself a private socket directory.
///
/// Read off the sources rather than by running them: the leak only shows up on
/// a machine where the suite runs inside a thurbox pane, and a run that does
/// leak still passes every assertion it makes. The two things checked are the
/// two halves of "a server this harness starts is one this harness can find
/// again" — the name it goes by, and the directory it lives in.
#[test]
fn every_harness_that_pins_a_socket_scopes_it_completely() {
    // A *pin* is a set, so lines that remove the variable are dropped first: a
    // harness that clears `THURBOX_SOCKET` outright is already scoped by the
    // derivation and has nothing to disarm. Both spellings are looked for —
    // some harnesses reach for the constant, others set the literal name on a
    // child `Command`.
    const PINS: [&str; 2] = ["SOCKET_OVERRIDE_ENV", "\"THURBOX_SOCKET\""];
    const CLEARS: [&str; 2] = ["SOCKET_OWNER_ENV", "THURBOX_SOCKET_FOR"];

    // The one file that is *about* the resolution, sets the pair on purpose,
    // and never starts a multiplexer.
    const EXEMPT: [&str; 1] = ["cli_socket_isolation.rs"];

    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests");
    let mut offenders = Vec::new();
    let mut checked = 0usize;
    for entry in std::fs::read_dir(&dir).expect("read tests/").flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if !name.ends_with(".rs") || EXEMPT.contains(&name.as_str()) {
            continue;
        }
        let src = std::fs::read_to_string(entry.path()).expect("read test source");
        let pinned: String = src
            .lines()
            .filter(|line| !line.contains("remove"))
            .collect::<Vec<_>>()
            .join("\n");
        if !PINS.iter().any(|p| pinned.contains(p)) {
            continue;
        }
        checked += 1;
        let mut missing = Vec::new();
        if !CLEARS.iter().any(|c| src.contains(c)) {
            missing.push("clear THURBOX_SOCKET_FOR");
        }
        if !src.contains("TMUX_TMPDIR") {
            missing.push("set TMUX_TMPDIR to a directory of its own");
        }
        if !missing.is_empty() {
            offenders.push(format!("{name}: must {}", missing.join(" and ")));
        }
    }

    assert!(
        checked > 0,
        "no harness pins a socket — this gate has stopped looking at anything"
    );
    assert!(
        offenders.is_empty(),
        "these harnesses leak a tmux server when the suite runs inside a \
         thurbox pane:\n  {}",
        offenders.join("\n  ")
    );
}
