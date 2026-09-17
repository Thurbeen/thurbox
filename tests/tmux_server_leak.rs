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
//! Being *reachable* is only half of it. A harness that pinned a socket
//! correctly and then panicked before its `cleanup()` left the server running
//! and took its socket file away with the tempdir, so nothing could connect to
//! reap it either — 400 of them on one machine, 4 sockets between 433 servers
//! (issue #1175). Reaching teardown was never something a test could promise:
//! a failed assertion, a `.expect()` on a racing tmux and a nextest
//! `slow-timeout` termination all skip it.
//!
//! `tests/cli_socket_isolation.rs` owns the resolution and pins it. What is
//! here is its consequence for a test harness, in four tests: the recipe that
//! avoids the leak driven end to end, a harness panicked on purpose to show
//! the reap survives it, and two gates — one holding
//! `tests/support/tmux_server.rs` to the recipe, one holding every harness in
//! this directory to the guard. Read off the sources rather than by running
//! them, because the leak is silent: a run that leaks still passes every
//! assertion it makes.

use std::path::{Path, PathBuf};
use std::process::Command;

/// The guard every harness in this directory reaps through.
#[path = "support/tmux_server.rs"]
mod tmux_server;

use tmux_server::TmuxServer;

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
    /// The instance's own server — and, being a guard, its reaper.
    server: TmuxServer,
}

impl Profile {
    fn new() -> Self {
        let root = tempfile::tempdir().expect("tempdir");
        for sub in ["home", "config", "data"] {
            std::fs::create_dir_all(root.path().join(sub)).expect("mkdir");
        }
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
            server: TmuxServer::private(&format!("thurbox-leak-{}", std::process::id())),
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
        // Pinned socket, cleared owner tag, private socket directory. The tag
        // is the one a suite run inside a thurbox pane inherits: left in place
        // it would rule the pin inherited — `cli_socket_isolation` owns that
        // resolution and pins it; here it simply has to be gone.
        self.server.scope(&mut cmd);
        cmd.env_remove("TMUX");
        cmd.env_remove("THURBOX_SESSION");
        cmd.env_remove("THURBOX_SESSION_ID");
        for var in GIT_LOCATION_ENV {
            cmd.env_remove(var);
        }
        cmd.output().expect("run thurbox-cli")
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
        profile.server.sockets(),
        vec![profile.server.socket().to_string()],
        "the run put its windows on a socket it did not pin — its teardown \
         would kill a name nothing created and leave a server running"
    );

    let killed = profile.server.tmux(&["kill-server"]);
    assert!(
        killed.status.success(),
        "kill-server failed: {}",
        String::from_utf8_lossy(&killed.stderr)
    );

    assert!(
        profile.server.alive().is_empty(),
        "a tmux server outlived the run: {:?}",
        profile.server.alive()
    );
}

// --- the panic that used to leak ------------------------------------------

/// Where the probe below writes the socket it started a server on. Read out of
/// the environment rather than fixed, so the probe can only run when the test
/// that drives it asked for it.
const PROBE_REPORT_ENV: &str = "THURBOX_PANIC_PROBE_REPORT";

/// The probe test's name, as the libtest harness spells it.
const PROBE: &str = "a_harness_that_panics_after_starting_a_server";

/// Whether any tmux **server** process is still running on `socket`.
///
/// Asked of the process table rather than of tmux, because the whole failure
/// is a server with no socket file: the file lived in the harness's own
/// directory and went away with it, so `tmux -L <name> list-sessions` answers
/// "no server" for a server that is very much alive, spinning on a CPU. That
/// is how a machine came to hold 433 of them with only 4 reachable.
#[cfg(unix)]
fn server_processes(socket: &str) -> Vec<String> {
    let out = Command::new("ps")
        .args(["-eo", "pid=,args="])
        .output()
        .expect("run ps");
    let needle = format!("-L {socket}");
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|line| line.contains(&needle))
        .map(str::trim)
        .map(str::to_string)
        .collect()
}

/// A harness that panics between starting its server and its teardown leaves
/// no tmux server behind.
///
/// The panic is the point. Teardown spelled as a `cleanup()` call at each exit
/// point is reached by the paths that return; it is not reached by the one
/// that unwinds, and an unwind is what a failed assertion, a `.expect()` on a
/// racing tmux, or a nextest `slow-timeout` termination all look like. So this
/// runs a real harness — a child copy of this very test binary, the way
/// `paths::tests::no_unit_test_temp_dir_outlives_the_test_process` does —
/// panics it on purpose, and then asks the process table what survived.
#[test]
#[cfg(unix)]
fn a_panicking_harness_still_reaps_its_tmux_server() {
    if !have_tmux() {
        eprintln!("skipping: tmux is not installed");
        return;
    }

    let report_dir = tempfile::tempdir().expect("tempdir");
    let report = report_dir.path().join("socket");
    let exe = std::env::current_exe().expect("this test binary");
    let out = Command::new(&exe)
        .args(["--exact", "--nocapture", PROBE])
        .env(PROBE_REPORT_ENV, &report)
        .output()
        .expect("run a child copy of the test binary");

    assert!(
        !out.status.success(),
        "the probe was supposed to panic, and did not:\n{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );

    // An empty report means the probe never got a server started, and there is
    // nothing for the assertion below to be about: passing it then would be
    // passing for the wrong reason.
    let socket = std::fs::read_to_string(&report).unwrap_or_default();
    let socket = socket.trim();
    if socket.is_empty() {
        eprintln!("skipping: the probe could not start a tmux server");
        return;
    }

    let survivors = server_processes(socket);
    assert!(
        survivors.is_empty(),
        "a harness panicked and left its tmux server running — and with its \
         socket directory gone with it, nothing can connect to reap it:\n  {}",
        survivors.join("\n  ")
    );
}

/// The probe [`a_panicking_harness_still_reaps_its_tmux_server`] drives: start
/// a server with a pane that outlives the test, say where it is, then panic
/// the way a failed assertion does.
///
/// Inert unless that test asked for it — it is the only caller, and it names
/// this one by [`PROBE`] rather than discovering it, so a rename fails the run
/// instead of passing vacuously.
#[test]
#[cfg(unix)]
fn a_harness_that_panics_after_starting_a_server() {
    let Some(report) = std::env::var_os(PROBE_REPORT_ENV) else {
        return;
    };
    if !have_tmux() {
        return;
    }

    let server = TmuxServer::pin(&format!("thurbox-panic-probe-{}", std::process::id()));
    // A pane that outlives the test: one that exits takes the server with it
    // and would turn this probe into a test of tmux's own idle shutdown.
    let started = server.tmux(&["new-session", "-d", "-s", "probe", "sleep", "300"]);
    // The report is what says a server exists to leak; the panic happens
    // either way, so the caller reads one failing child rather than having to
    // tell "could not start a server" from "did not panic".
    if started.status.success() {
        std::fs::write(&report, server.socket()).expect("report the socket");
    }

    panic!("the probe panics here, as a failed assertion would");
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

/// How far from a pin the clear and the socket directory may sit. The real
/// gaps are a handful of lines, so this is headroom rather than latitude: the
/// point is that the three are one piece of setup, not three places in a file.
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

/// The guard's own file: the one place a socket is pinned, and the one place
/// that has to spell the whole recipe out.
const GUARD: &str = "support/tmux_server.rs";

/// The guard type, as the harnesses spell it.
const GUARD_TYPE: &str = "TmuxServer";

/// Every pin the guard makes clears the inherited owner tag and points
/// `TMUX_TMPDIR` somewhere of its own.
///
/// The rule used to be read off every harness, because every harness spelled it
/// out. They now route through [`GUARD`] instead, which is a better place for
/// it to be right and a worse place for it to be wrong: one mistake here is
/// every harness's mistake. So the rule did not go away, it moved.
///
/// Read off the source rather than by running it: the leak only shows up on a
/// machine where the suite runs inside a thurbox pane, and a run that does leak
/// still passes every assertion it makes. Comments are stripped first, the way
/// `tests/architecture_rules.rs` strips them before extracting references — a
/// rule read off raw text is satisfied by prose.
#[test]
fn the_guard_scopes_every_socket_it_pins() {
    let src = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join(GUARD),
    )
    .expect("read the guard");
    let (pins, failures) = scope_failures(&src);
    assert!(
        pins > 0,
        "{GUARD} pins no socket — this gate has stopped looking at anything"
    );
    assert!(
        failures.is_empty(),
        "these pins in {GUARD} leak a tmux server when the suite runs inside a \
         thurbox pane:\n  {}",
        failures.join("\n  ")
    );
}

/// No harness pins a tmux socket by hand: every one of them goes through the
/// guard, whose `Drop` reaps whatever the pin started.
///
/// Per pin *site*, not per file — and that distinction is the whole reason this
/// is worth reading off the sources. A file-wide check is satisfied by one
/// correctly written test while a sibling beside it leaks: `attach_by_name`
/// scopes five times, and a sixth test that pinned a socket of its own would
/// sit behind the other five. The previous version of this gate asked each pin
/// for a cleared owner tag and a private socket directory, which is what makes
/// a server reachable; it did not ask who kills it. Nobody did, on any path
/// that panicked or timed out, and a machine ended up holding 433 servers with
/// 4 sockets between them (issue #1175).
///
/// So the question here is narrower and stronger: a harness may not pin at all.
/// `TmuxServer::pin` and `TmuxServer::private` are the only two pins in this
/// directory, `the_guard_scopes_every_socket_it_pins` holds them to the recipe,
/// and holding one is what reaps.
#[test]
fn no_harness_pins_a_socket_outside_the_guard() {
    // The one file that is *about* the resolution, sets the pair on purpose,
    // and never starts a multiplexer.
    const EXEMPT: [&str; 1] = ["cli_socket_isolation.rs"];

    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests");
    let mut offenders = Vec::new();
    let mut guards = 0usize;
    for entry in std::fs::read_dir(&dir).expect("read tests/").flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if !name.ends_with(".rs") || EXEMPT.contains(&name.as_str()) {
            continue;
        }
        let src = std::fs::read_to_string(entry.path()).expect("read test source");
        let code = code_of(&src);
        let lines: Vec<&str> = code.lines().map(str::trim_end).collect();
        for at in marks(
            &lines,
            Some("SOCKET_OVERRIDE_ENV"),
            "\"THURBOX_SOCKET\"",
            &SET,
        ) {
            offenders.push(format!(
                "{name}:{} pins a socket by hand — pin it with {GUARD_TYPE} \
                 ({GUARD}), which reaps the server when it is dropped",
                at + 1
            ));
        }
        for (i, line) in lines.iter().enumerate() {
            let held = held_guard(line);
            guards += usize::from(held == Some(true));
            if held == Some(false) {
                offenders.push(format!(
                    "{name}:{} drops its guard where it builds it — the server \
                     is reaped before the test has started",
                    i + 1
                ));
            }
        }
    }

    assert!(
        guards > 0,
        "no harness holds a {GUARD_TYPE} — this gate has stopped looking at \
         anything"
    );
    offenders.sort();
    assert!(
        offenders.is_empty(),
        "these leak a tmux server whenever the test does not reach its own \
         teardown:\n  {}",
        offenders.join("\n  ")
    );
}

/// Whether `line` builds a guard and, if it does, whether it keeps it.
///
/// A guard reaps when it is *dropped*, so building one and not binding it —
/// `TmuxServer::pin(SOCKET);` as a statement, or `let _ = …`, both of which
/// drop at the end of that statement — kills the server before the test has
/// started one. Neither is a compile error and neither fails an assertion; the
/// test simply runs unscoped, which is the state this whole file is about.
fn held_guard(line: &str) -> Option<bool> {
    let line = line.trim_start();
    if line.starts_with("//") || !line.contains(&format!("{GUARD_TYPE}::")) {
        return None;
    }
    Some(!(line.starts_with(&format!("{GUARD_TYPE}::")) || line.starts_with("let _ =")))
}
