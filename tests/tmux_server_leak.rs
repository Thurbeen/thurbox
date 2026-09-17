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
//! `tests/cli_socket_isolation.rs` owns that resolution and pins it. What is
//! here is its consequence for a test harness: the recipe that avoids the leak,
//! driven end to end, and a gate holding every other harness in this directory
//! to the same recipe — because the leak is silent, and a run that leaks still
//! passes every assertion it makes.

use std::path::{Path, PathBuf};
use std::process::Command;

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
        // Per *profile*, not per process, so a second test added to this file
        // cannot take this one's socket directory out from under it when its
        // `Drop` runs. nextest gives each test a process of its own, so only a
        // plain `cargo test` would ever notice — which is the run that would
        // notice it as a mystery.
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

    /// Run `thurbox-cli` in this profile, scoped the way a harness must scope
    /// itself: pinned socket, cleared owner tag, private socket directory.
    fn cli(&self, args: &[&str]) -> std::process::Output {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_thurbox-cli"));
        cmd.args(args);
        cmd.current_dir(self.root.path());
        cmd.env("HOME", self.path("home"));
        cmd.env("USERPROFILE", self.path("home"));
        cmd.env("THURBOX_CONFIG_DIR", self.path("config"));
        cmd.env("THURBOX_DATA_DIR", self.path("data"));
        cmd.env("TMUX_TMPDIR", &self.sockets);
        cmd.env(thurbox::agent::tmux::SOCKET_OVERRIDE_ENV, &self.socket);
        // The tag a suite run inside a thurbox pane inherits. Left in place it
        // would rule the pin above inherited — `cli_socket_isolation` owns that
        // resolution and pins it; here it simply has to be gone.
        cmd.env_remove(thurbox::agent::tmux::SOCKET_OWNER_ENV);
        cmd.env_remove("TMUX");
        cmd.env_remove("THURBOX_SESSION");
        cmd.env_remove("THURBOX_SESSION_ID");
        for var in GIT_LOCATION_ENV {
            cmd.env_remove(var);
        }
        cmd.output().expect("run thurbox-cli")
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
        // Every socket that turned up, not just the pinned one. The failure
        // this file exists to catch is a run landing on a name it did not
        // choose, and a test that fails that way has started a server too —
        // reaping only the pinned name would leak exactly the thing under
        // test, on the one run where it went wrong.
        for socket in self.sockets() {
            let _ = Command::new("tmux")
                .env("TMUX_TMPDIR", &self.sockets)
                .args(["-L", &socket, "kill-server"])
                .output();
        }
        let _ = std::fs::remove_dir_all(&self.sockets);
    }
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

    let created = profile.cli(&[
        "session",
        "create",
        "--name",
        "probe",
        "--repo-path",
        repo.to_str().expect("utf-8 path"),
        "--agent",
        "shell",
    ]);
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

/// `src` with its line comments removed, so a rule below is answered by code
/// rather than by a comment that talks about it. Line comments are all this
/// needs: `tests/` carries no block comments, and one appearing later costs a
/// false *failure* — a harness reported as unscoped when it is not — which is
/// the direction that gets noticed and fixed rather than trusted.
fn code_of(src: &str) -> String {
    src.lines()
        .map(|line| match line.find("//") {
            Some(at) => &line[..at],
            None => line,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// How far from a pin the clear and the socket directory may sit. The widest
/// real gap is nine lines, so this is headroom rather than latitude: the point
/// is that they are part of the same setup, not somewhere else in the file.
const WINDOW: usize = 15;

/// Verbs that *set* an environment variable, and verbs that *clear* one.
const SET: [&str; 2] = ["set_var(", ".env("];
const CLEAR: [&str; 2] = ["remove_var(", "env_remove("];

/// Whether `line` sets or clears the variable spelled `literal`, whose crate
/// constant — where it has one — is `konst`.
///
/// The constant counts without a verb beside it, because the multi-line form
/// `set_var(\n  SOCKET_OVERRIDE_ENV,\n  …)` puts the two on different lines;
/// `prev` is how that case is recognised. An import naming the constant is not
/// a use of it, and a bare literal needs the verb, or every mention would count.
fn touches(line: &str, prev: &str, konst: Option<&str>, literal: &str, verbs: &[&str]) -> bool {
    let line = line.trim_start();
    if line.starts_with("use ") {
        return false;
    }
    let verbed = verbs.iter().any(|v| line.contains(v));
    // A real use of the constant, not this file's own description of one:
    // `marks(Some("SOCKET_OVERRIDE_ENV"), …)` names it inside a string, and a
    // use never is.
    let names_konst = konst.is_some_and(|k| {
        line.match_indices(k)
            .any(|(at, _)| at == 0 || !line[..at].ends_with('"'))
    });
    if names_konst {
        // …and it still has to be a set or a clear. `var(SOCKET_OVERRIDE_ENV)`
        // reads the socket rather than moving it, and counting that as a pin
        // would fail a harness for asking a question.
        let continued = prev.trim_end().ends_with('(') && verbs.iter().any(|v| prev.contains(v));
        return verbed || continued;
    }
    line.contains(literal) && verbed
}

/// The lines of `lines` that touch one variable — see [`touches`].
fn marks(lines: &[&str], konst: Option<&str>, literal: &str, verbs: &[&str]) -> Vec<usize> {
    lines
        .iter()
        .enumerate()
        .filter(|(i, l)| {
            touches(
                l,
                if *i == 0 { "" } else { lines[i - 1] },
                konst,
                literal,
                verbs,
            )
        })
        .map(|(i, _)| i)
        .collect()
}

/// Take the line nearest `at` within [`WINDOW`] out of `xs`, or answer that
/// there is none left to take.
///
/// Taking is what makes this one-for-one: two pins ten lines apart cannot both
/// point at the same `remove_var`, which under a plain proximity test would let
/// the unscoped one borrow its neighbour's and pass.
fn claim_nearest(xs: &mut Vec<usize>, at: usize) -> bool {
    let found = xs
        .iter()
        .enumerate()
        .filter(|(_, x)| x.abs_diff(at) <= WINDOW)
        .min_by_key(|(_, x)| x.abs_diff(at))
        .map(|(i, _)| i);
    match found {
        Some(i) => {
            xs.remove(i);
            true
        }
        None => false,
    }
}

/// Every pin in `src`, and for each one what it failed to do: how many pins
/// were seen, and a line-numbered complaint per pin that is not scoped.
fn scope_failures(src: &str) -> (usize, Vec<String>) {
    let code = code_of(src);
    let lines: Vec<&str> = code.lines().map(str::trim_end).collect();

    let pinned = marks(
        &lines,
        Some("SOCKET_OVERRIDE_ENV"),
        "\"THURBOX_SOCKET\"",
        &SET,
    );
    let mut clears = marks(
        &lines,
        Some("SOCKET_OWNER_ENV"),
        "\"THURBOX_SOCKET_FOR\"",
        &CLEAR,
    );
    // No crate constant for this one: tmux's own variable, set by name.
    let mut scopes = marks(&lines, None, "\"TMUX_TMPDIR\"", &SET);

    let mut failures = Vec::new();
    for at in &pinned {
        let mut missing = Vec::new();
        if !claim_nearest(&mut clears, *at) {
            missing.push("clear THURBOX_SOCKET_FOR");
        }
        if !claim_nearest(&mut scopes, *at) {
            missing.push("set TMUX_TMPDIR to a directory of its own");
        }
        if !missing.is_empty() {
            failures.push(format!("{} must {}", at + 1, missing.join(" and ")));
        }
    }
    (pinned.len(), failures)
}

/// Every *place* a harness pins a tmux socket also clears the inherited owner
/// tag and points `TMUX_TMPDIR` somewhere of its own.
///
/// Per pin site, not per file. A file-wide check is satisfied by one correctly
/// scoped test while a sibling beside it leaks: `attach_by_name` scopes five
/// times, and a sixth test that pinned a socket and forgot the rest would sit
/// behind the other five and leak a server on every run.
///
/// Read off the sources rather than by running them: the leak only shows up on
/// a machine where the suite runs inside a thurbox pane, and a run that does
/// leak still passes every assertion it makes. Comments are stripped first, the
/// way `tests/architecture_rules.rs` strips them before extracting references —
/// a rule read off raw text is satisfied by prose, and a harness whose comment
/// merely *mentions* the owner tag would pass while leaking.
#[test]
fn every_socket_a_harness_pins_is_scoped_where_it_is_pinned() {
    // The one file that is *about* the resolution, sets the pair on purpose,
    // and never starts a multiplexer.
    const EXEMPT: [&str; 1] = ["cli_socket_isolation.rs"];

    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests");
    let mut offenders = Vec::new();
    let mut pins = 0usize;
    for entry in std::fs::read_dir(&dir).expect("read tests/").flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if !name.ends_with(".rs") || EXEMPT.contains(&name.as_str()) {
            continue;
        }
        let src = std::fs::read_to_string(entry.path()).expect("read test source");
        let (found, failures) = scope_failures(&src);
        pins += found;
        offenders.extend(failures.into_iter().map(|f| format!("{name}:{f}")));
    }

    assert!(
        pins > 0,
        "no harness pins a socket — this gate has stopped looking at anything"
    );
    offenders.sort();
    assert!(
        offenders.is_empty(),
        "these pins leak a tmux server when the suite runs inside a thurbox \
         pane:\n  {}",
        offenders.join("\n  ")
    );
}
