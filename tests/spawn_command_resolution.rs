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

use thurbox::agent::backend::SessionBackend;
use thurbox::agent::tmux::{TmuxBackend, SOCKET_OVERRIDE_ENV, SOCKET_OWNER_ENV};

/// A throwaway socket, so this never touches the real one.
const SOCKET: &str = "thurbox-spawn-cmd-e2e";

fn have_tmux() -> bool {
    Command::new("tmux")
        .arg("-V")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn cleanup() {
    let _ = Command::new("tmux")
        .args(["-L", SOCKET, "kill-server"])
        .output();
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
fn create_session(root: &Path, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_thurbox-cli"))
        .args(["session", "create"])
        .args(args)
        .arg("--json")
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", root.join("home"))
        .env("THURBOX_CONFIG_DIR", root.join("config"))
        .env("THURBOX_DATA_DIR", root.join("data"))
        .env("TMUX_TMPDIR", root)
        .env(SOCKET_OVERRIDE_ENV, SOCKET)
        // An injected socket is ruled inherited when its paired data dir is not
        // this one's (see `socket_for`); this test is somebody typing it.
        .env_remove(SOCKET_OWNER_ENV)
        .env_remove("THURBOX_SESSION")
        .env_remove("THURBOX_SESSION_ID")
        .output()
        .expect("run thurbox-cli session create")
}

/// A scratch instance: its own config, data and git repository.
fn instance() -> (tempfile::TempDir, PathBuf) {
    let root = tempfile::tempdir().expect("tempdir");
    for sub in ["home", "config", "data"] {
        std::fs::create_dir_all(root.path().join(sub)).expect("mkdir");
    }
    let checkout = root.path().join("repo");
    repo(&checkout);
    (root, checkout)
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

    // nextest runs one process per test, so process-wide env is safe here.
    std::env::set_var("TMUX_TMPDIR", dir.path());
    std::env::set_var(SOCKET_OVERRIDE_ENV, SOCKET);
    // The override is dropped when it was injected for someone else's data dir
    // (see `socket_for`); this test *is* somebody typing it.
    std::env::remove_var(SOCKET_OWNER_ENV);
    thurbox::paths::set_test_dir(dir.path());

    cleanup();
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
        .env("TMUX_TMPDIR", dir.path())
        .output()
        .expect("run tmux");
    if !started.status.success() {
        cleanup();
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
        cleanup();
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
            cleanup();
            panic!("the spawn itself failed: {e:#}");
        }
    };

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while std::time::Instant::now() < deadline && !marker.exists() {
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    let ran = marker.exists();
    drop(spawned);
    cleanup();

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
    let (root, checkout) = instance();
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

    cleanup();
    let out = create_session(
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
        cleanup();
        let stderr = String::from_utf8_lossy(&out.stderr);
        if stderr.contains("tmux") || stderr.contains("multiplexer") {
            eprintln!("skipping: tmux would not spawn a window: {stderr}");
            return;
        }
        panic!("the spawn itself failed: {stderr}");
    }

    wait_for(&seen);
    let found = std::fs::read_to_string(&seen);
    cleanup();

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
    let (root, checkout) = instance();
    let seen = root.path().join("cli-seen");

    cleanup();
    let out = create_session(
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
        cleanup();
        let stderr = String::from_utf8_lossy(&out.stderr);
        if stderr.contains("tmux") || stderr.contains("multiplexer") {
            eprintln!("skipping: tmux would not spawn a window: {stderr}");
            return;
        }
        panic!("the spawn itself failed: {stderr}");
    }

    wait_for(&seen);
    let found = std::fs::read_to_string(&seen);
    cleanup();

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
