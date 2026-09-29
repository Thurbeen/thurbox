//! A local spawn must launch the agent **thurbox** resolved, not whatever the
//! multiplexer's own `PATH` happens to resolve.
//!
//! thurbox used to hand tmux a bare command name (`claude`) and let tmux
//! resolve it. Which resolver ran, and with which `PATH`, was not thurbox's to
//! choose: tmux copies the *client's* `PATH` into the new pane only for an
//! **unattached** client (`spawn.c`: "the session one is replaced from the
//! client ... only unattached clients"). thurbox's control-mode client is
//! attached, so its windows got the `PATH` of whatever first started the tmux
//! **server** — and a single-token command is handed to that server's
//! `default-shell` rather than `execvp`, so the resolver could be a shell
//! thurbox never chose.
//!
//! Under zsh/bash the two `PATH`s agree, because the interactive additions live
//! in `~/.zshenv` / `~/.profile`, which any shell that starts a server sources.
//! Under **fish** they do not: `fish_add_path` writes `fish_user_paths`, which
//! only fish applies — so a server started from anything else never sees them,
//! for the life of that server.
//!
//! The same spawn also decides the `PATH` its pane runs on, and the tests below
//! the first one are about that half: a pane resolves `thurbox-cli` by bare
//! name for every status hook, and how many arguments the window command has
//! decides whether tmux runs it through a shell at all.
//!
//! Skipped when tmux is absent rather than failing: a missing multiplexer is an
//! environment fact, not a regression.

#![cfg(unix)]

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use thurbox::backend::tmux::TmuxBackend;
use thurbox::backend::SessionBackend;

/// The guard every tmux server in this file is reaped by — see its own doc.
#[path = "support/tmux_server.rs"]
mod tmux_server;

use tmux_server::TmuxServer;

/// A throwaway socket, so this never touches the real one.
const SOCKET: &str = "thurbox-spawn-cmd-e2e";

fn have_tmux() -> bool {
    Command::new("tmux")
        .arg("-V")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// The `GIT_*` location variables, scrubbed from every `git` call below: git
/// exports them to hook processes, so this suite running under the project's
/// own pre-commit hook would otherwise rewrite the real repository.
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
    assert!(
        cmd.output().expect("run git").status.success(),
        "git {args:?} failed"
    );
}

/// A repository with one commit, which is the minimum a worktree needs.
fn repo(at: &Path) {
    std::fs::create_dir_all(at).expect("mkdir");
    git(at, &["init", "-q", "-b", "main"]);
    git(at, &["config", "user.email", "t@example.com"]);
    git(at, &["config", "user.name", "thurbox-test"]);
    // Signing is a user setting that fails in a bare environment, and this
    // throwaway repo is not the place to be signing anything.
    git(at, &["config", "commit.gpgsign", "false"]);
    std::fs::write(at.join("README.md"), "# probe\n").expect("write");
    git(at, &["add", "."]);
    git(at, &["commit", "-qm", "init"]);
}

/// `thurbox-cli session create …` run the way a **delegated** spawn runs it:
/// as a child process, on a `PATH` that cannot find `thurbox-cli` itself.
///
/// That is the shape of a shared-sessions host (ADR-24), where the TUI invokes
/// the host's CLI at an absolute path over ssh and sshd hands the command its
/// own stripped `PATH`.
fn create_session(server: &TmuxServer, root: &Path, args: &[&str]) -> std::process::Output {
    cli(server, root, &[&["session", "create"], args].concat())
}

/// Any `thurbox-cli` command, run the way [`create_session`] runs `session
/// create`.
fn cli(server: &TmuxServer, root: &Path, args: &[&str]) -> std::process::Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_thurbox-cli"));
    cmd.args(args)
        .arg("--json")
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", root.join("home"))
        .env("THURBOX_CONFIG_DIR", root.join("config"))
        .env("THURBOX_DATA_DIR", root.join("data"))
        .env_remove("THURBOX_SESSION")
        .env_remove("THURBOX_SESSION_ID");
    server.scope(&mut cmd);
    cmd.output().expect("run thurbox-cli")
}

/// A scratch instance: its own config, data, git repository and tmux server.
///
/// The server is a guard: whatever this test does next, dropping it reaps.
fn instance() -> (tempfile::TempDir, PathBuf, TmuxServer) {
    let root = tempfile::tempdir().expect("tempdir");
    for sub in ["home", "config", "data"] {
        std::fs::create_dir_all(root.path().join(sub)).expect("mkdir");
    }
    let checkout = root.path().join("repo");
    repo(&checkout);
    (root, checkout, TmuxServer::private(SOCKET))
}

/// Wait for `path` to appear, so an assertion reads what the pane wrote rather
/// than racing tmux's asynchronous window start.
fn wait_for(path: &Path) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while std::time::Instant::now() < deadline && !path.exists() {
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}

#[test]
fn a_local_spawn_finds_the_agent_the_multiplexer_cannot() {
    if !have_tmux() {
        eprintln!("skipping: tmux is not installed");
        return;
    }

    let dir = tempfile::tempdir().expect("tempdir");
    let bin = dir.path().join("bin");
    std::fs::create_dir_all(&bin).expect("bin dir");
    let marker = dir.path().join("agent-ran");
    let agent = bin.join("tb-probe-agent");
    std::fs::write(
        &agent,
        format!(
            "#!/bin/sh\nprintf ok > {}\nsleep 30\n",
            marker.to_string_lossy()
        ),
    )
    .expect("write probe agent");
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&agent, std::fs::Permissions::from_mode(0o700)).expect("chmod");
    }

    let server = TmuxServer::pin(SOCKET);
    thurbox::paths::set_test_dir(dir.path());

    // The server starts without the agent's directory on `PATH` — the shape a
    // fish user's machine is in whenever the server was started by anything
    // that is not fish.
    let started = Command::new("tmux")
        .args([
            "-L",
            SOCKET,
            "new-session",
            "-d",
            "-s",
            "thurbox",
            "-x",
            "80",
            "-y",
            "24",
        ])
        .env("PATH", "/usr/bin:/bin")
        .env("TMUX_TMPDIR", server.tmpdir())
        .output()
        .expect("run tmux");
    if !started.status.success() {
        eprintln!(
            "skipping: tmux would not start a server: {}",
            String::from_utf8_lossy(&started.stderr).trim()
        );
        return;
    }

    // thurbox's own `PATH` *does* have it: this is the user's interactive PATH,
    // the one the agent was installed onto.
    let path = format!(
        "{}:{}",
        bin.to_string_lossy(),
        std::env::var("PATH").unwrap_or_default()
    );
    std::env::set_var("PATH", &path);

    let backend = TmuxBackend::local();
    if let Err(e) = backend.ensure_ready() {
        eprintln!("skipping: tmux control mode would not start: {e:#}");
        return;
    }
    let spawned = backend.spawn(
        "tb-probe",
        "tb-probe-agent",
        &[],
        Some(dir.path()),
        &HashMap::new(),
        24,
        80,
    );
    let spawned = match spawned {
        Ok(s) => s,
        Err(e) => {
            panic!("the spawn itself failed: {e:#}");
        }
    };

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while std::time::Instant::now() < deadline && !marker.exists() {
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    let ran = marker.exists();
    drop(spawned);

    assert!(
        ran,
        "the agent thurbox resolved on its own PATH never ran: the window command \
         was left for the multiplexer to resolve, and the multiplexer's PATH does \
         not have it"
    );
}

/// A pane must be able to resolve `thurbox-cli` by bare name, because that is
/// how every status hook is spelled: `thurbox-cli session signal --state <s>
/// || true`.
///
/// A pane runs on the `PATH` of whichever thurbox spawned it, which tmux copies
/// in by itself. On a shared-sessions host that thurbox is a `thurbox-cli` the
/// TUI invoked over ssh, and sshd's `PATH` for a non-interactive command has no
/// `~/.local/bin` on it — where `thurbox-cli` installs. Every hook then resolved
/// nothing and `|| true` swallowed it, so the host's own rows never gained a
/// `hook_state` and every session on it read as statusless on the TUI mirroring
/// them.
#[test]
fn a_spawned_pane_resolves_the_cli_its_hooks_call() {
    if !have_tmux() {
        eprintln!("skipping: tmux is not installed");
        return;
    }
    let (root, checkout, server) = instance();
    let seen = root.path().join("cli-seen");

    // The agent reports what the pane's own `PATH` finds — exactly the lookup
    // every status hook does before it can signal anything.
    std::fs::write(
        root.path().join("config/agents.toml"),
        format!(
            "default = \"probe\"\n\n[[agents]]\nname = \"probe\"\ncommand = \"sh\"\n\
             args = [\"-c\", \"command -v thurbox-cli > {} 2>&1; sleep 30\"]\n",
            seen.display()
        ),
    )
    .expect("write agents.toml");

    let out = create_session(
        &server,
        root.path(),
        &[
            "--name",
            "hook-cli",
            "--repo-path",
            checkout.to_str().expect("utf-8 path"),
            "--worktree-branch",
            "feat/hook-cli",
            "--base-branch",
            "main",
            "--agent",
            "probe",
        ],
    );
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        if stderr.contains("tmux") || stderr.contains("multiplexer") {
            eprintln!("skipping: tmux would not spawn a window: {stderr}");
            return;
        }
        panic!("the spawn itself failed: {stderr}");
    }

    wait_for(&seen);
    let found = std::fs::read_to_string(&seen);

    // Separated from the lookup below so a pane that never ran the agent at all
    // cannot read as a pane whose `PATH` came up empty.
    let found = found.expect("the agent ran and reported what its PATH found");
    assert_eq!(
        found.trim(),
        env!("CARGO_BIN_EXE_thurbox-cli"),
        "the pane's PATH did not find the thurbox-cli its status hooks call, so \
         every signal resolved nothing and `|| true` swallowed it"
    );
}

/// The **first** session a fresh install creates is launched with its status
/// hooks wired.
///
/// The built-in hooks extension patches claude's `--settings` into
/// `agents.toml`, and only the TUI's boot and the heartbeat's tick used to run
/// that install. A machine driven by `thurbox-cli` alone — a lead agent, a
/// shared-sessions host — armed the heartbeat *after* its first `session
/// create`, so that session launched without the flag and never reported a
/// state, while `session doctor` found the payload on disk and passed it.
#[test]
fn the_first_cli_session_on_a_fresh_install_launches_with_its_hooks() {
    if !have_tmux() {
        eprintln!("skipping: tmux is not installed");
        return;
    }
    let (root, checkout, server) = instance();
    let argv = root.path().join("argv");

    // A claude-family agent (the patch fans out by `hook_schema`) that writes
    // down the arguments it was launched with.
    std::fs::write(
        root.path().join("config/agents.toml"),
        format!(
            "default = \"probe\"\n\n[[agents]]\nname = \"probe\"\ncommand = \"sh\"\n\
             hook_schema = \"claude\"\n\
             args = [\"-c\", \"echo \\\"$*\\\" > {}; sleep 30\", \"probe\"]\n",
            argv.display()
        ),
    )
    .expect("write agents.toml");

    let out = create_session(
        &server,
        root.path(),
        &[
            "--name",
            "first",
            "--repo-path",
            checkout.to_str().expect("utf-8 path"),
            "--agent",
            "probe",
        ],
    );
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        if stderr.contains("tmux") || stderr.contains("multiplexer") {
            eprintln!("skipping: tmux would not spawn a window: {stderr}");
            return;
        }
        panic!("the spawn itself failed: {stderr}");
    }

    wait_for(&argv);
    let launched = std::fs::read_to_string(&argv).expect("the agent ran and wrote its argv");
    let settings = root.path().join("config/hooks/claude.json");
    assert_eq!(
        launched.trim(),
        format!("--settings {}", settings.display()),
        "the first session launched without the hooks' --settings, so it can \
         never report a state"
    );
}

/// The claude-family probe of the first-session tests: it writes down the
/// arguments it was launched with, so a test can read whether the hooks'
/// `--settings` reached it.
fn write_argv_probe(root: &Path, argv: &Path) {
    std::fs::write(
        root.join("config/agents.toml"),
        format!(
            "default = \"probe\"\n\n[[agents]]\nname = \"probe\"\ncommand = \"sh\"\n\
             hook_schema = \"claude\"\n\
             args = [\"-c\", \"echo \\\"$*\\\" > {}; sleep 30\", \"probe\"]\n",
            argv.display()
        ),
    )
    .expect("write agents.toml");
}

/// `task run` spawns through its own path (`spawn_and_deliver`), so the first
/// session a task starts on a CLI-only machine needs the same wiring as a
/// `session create`.
#[test]
fn the_first_task_session_on_a_fresh_install_launches_with_its_hooks() {
    if !have_tmux() {
        eprintln!("skipping: tmux is not installed");
        return;
    }
    let (root, checkout, server) = instance();
    let argv = root.path().join("argv");
    write_argv_probe(root.path(), &argv);

    let repo = checkout.to_str().expect("utf-8 path");
    let task = cli(
        &server,
        root.path(),
        &[
            "task", "create", "--title", "probe", "--repo", repo, "--agent", "probe",
        ],
    );
    assert!(
        task.status.success(),
        "task create failed: {}",
        String::from_utf8_lossy(&task.stderr)
    );
    let id = serde_json::from_slice::<serde_json::Value>(&task.stdout).expect("task JSON")["id"]
        .to_string();
    let run = cli(&server, root.path(), &["task", "run", id.trim_matches('"')]);
    if !run.status.success() {
        let stderr = String::from_utf8_lossy(&run.stderr);
        if stderr.contains("tmux") || stderr.contains("multiplexer") {
            eprintln!("skipping: tmux would not spawn a window: {stderr}");
            return;
        }
        panic!("task run failed: {stderr}");
    }

    wait_for(&argv);
    let launched = std::fs::read_to_string(&argv).expect("the agent ran and wrote its argv");
    let settings = root.path().join("config/hooks/claude.json");
    assert_eq!(
        launched.trim(),
        format!("--settings {}", settings.display()),
        "the task's session launched without the hooks' --settings"
    );
}

/// A `session create` refused before it spawns anything leaves the agent
/// config as it found it: wiring the hooks is a side effect of launching an
/// agent, not of asking to.
#[test]
fn a_refused_create_leaves_the_agent_config_alone() {
    let (root, checkout, server) = instance();
    let argv = root.path().join("argv");
    write_argv_probe(root.path(), &argv);
    let before = std::fs::read_to_string(root.path().join("config/agents.toml")).unwrap();

    let out = create_session(
        &server,
        root.path(),
        &[
            "--name",
            "refused",
            "--repo-path",
            checkout.to_str().expect("utf-8 path"),
            "--agent",
            "probe",
            "--reports-as",
            "no-such-agent",
        ],
    );
    assert!(
        !out.status.success(),
        "an unknown --reports-as was accepted"
    );
    assert_eq!(
        std::fs::read_to_string(root.path().join("config/agents.toml")).unwrap(),
        before,
        "a create that spawned nothing rewrote agents.toml"
    );
}

/// A `task run` whose session name is refused leaves the agent config alone.
///
/// `task create` takes any title, but the spawned session is named after it,
/// and a `/` makes that name unsafe as a path — so the run is refused only once
/// it reaches the spawn.
#[test]
fn a_refused_task_run_leaves_the_agent_config_alone() {
    let (root, checkout, server) = instance();
    let argv = root.path().join("argv");
    write_argv_probe(root.path(), &argv);
    let before = std::fs::read_to_string(root.path().join("config/agents.toml")).unwrap();

    let repo = checkout.to_str().expect("utf-8 path");
    let task = cli(
        &server,
        root.path(),
        &[
            "task",
            "create",
            "--title",
            "Fix foo/bar",
            "--repo",
            repo,
            "--agent",
            "probe",
        ],
    );
    assert!(
        task.status.success(),
        "task create failed: {}",
        String::from_utf8_lossy(&task.stderr)
    );
    let id = serde_json::from_slice::<serde_json::Value>(&task.stdout).expect("task JSON")["id"]
        .to_string();
    let run = cli(&server, root.path(), &["task", "run", id.trim_matches('"')]);
    assert!(
        !run.status.success(),
        "a task named with a '/' spawned a session"
    );
    assert_eq!(
        std::fs::read_to_string(root.path().join("config/agents.toml")).unwrap(),
        before,
        "a task run that spawned nothing rewrote agents.toml"
    );
}

/// A command session with **no arguments** is still split by a shell.
///
/// tmux runs a one-argument window command through its `default-shell` and a
/// multi-argument one through `execvp` (`spawn.c`), so `--command "sleep 300"`
/// only ever worked because that shell split it. Putting the `PATH` prefix in
/// as two more argv entries moved the command to `execvp`, which has no
/// splitting to do: the pane died instantly with status 127. The prefix has to
/// carry the `PATH` *and* leave the argument count tmux would have seen.
///
/// Asserted through what the pane resolves rather than only that it lives, so
/// the `PATH` cannot be quietly dropped from this branch to make it pass.
#[test]
fn a_command_session_with_no_args_keeps_the_shell_that_splits_it() {
    if !have_tmux() {
        eprintln!("skipping: tmux is not installed");
        return;
    }
    let (root, checkout, server) = instance();
    let seen = root.path().join("cli-seen");

    let out = create_session(
        &server,
        root.path(),
        &[
            "--name",
            "split-me",
            "--repo-path",
            checkout.to_str().expect("utf-8 path"),
            // One string, several words, no `--arg`: the shape that reaches
            // tmux as a single argument.
            "--command",
            &format!(
                "sh -c 'command -v thurbox-cli > {} 2>&1; sleep 30'",
                seen.display()
            ),
        ],
    );
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        if stderr.contains("tmux") || stderr.contains("multiplexer") {
            eprintln!("skipping: tmux would not spawn a window: {stderr}");
            return;
        }
        panic!("the spawn itself failed: {stderr}");
    }

    wait_for(&seen);
    let found = std::fs::read_to_string(&seen);

    let found = found.expect(
        "the command ran: a pane that died on `execvp` of the whole string \
         never got as far as writing this",
    );
    assert_eq!(
        found.trim(),
        env!("CARGO_BIN_EXE_thurbox-cli"),
        "the shell split the command but the pane lost the PATH prefix"
    );
}
