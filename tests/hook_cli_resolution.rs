//! A spawned pane must be able to resolve `thurbox-cli` by bare name.
//!
//! Every agent's status hook is wired to `thurbox-cli session signal --state
//! <s> || true` — a **bare** name, looked up against the pane's `PATH`, which
//! tmux copies from the thurbox that spawned it. That is the right answer right
//! up until the spawning thurbox is not something a login shell started.
//!
//! The case that made this visible is a **shared-sessions host** (ADR-24). The
//! local TUI delegates `session create` to the host by running its CLI at an
//! absolute path over ssh (`ssh <host> ~/.local/share/thurbox/bin/thurbox-cli
//! session create …`), and sshd hands a non-interactive command its own `PATH`
//! — `/usr/local/bin:/usr/bin:/bin:/usr/games`, with no `~/.local/bin`, which
//! is where `thurbox-cli` installs. Every pane spawned on that host therefore
//! carried a `PATH` that could not find the very binary the hooks call.
//! `|| true` swallowed the failure, so the host's own rows never gained a
//! `hook_state` and the session read as statusless on the TUI mirroring them.
//!
//! Driven through the **real** `thurbox-cli`, because that is the process whose
//! environment is the subject: it is the one that knows where its own CLI is
//! even when `PATH` does not.
//!
//! Skipped when tmux or git is absent rather than failing: a missing dependency
//! is an environment fact, not a regression.

#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::Command;

/// A throwaway tmux socket, so this never touches the real one.
const SOCKET: &str = "thurbox-hook-cli-e2e";

fn cleanup() {
    let _ = Command::new("tmux")
        .args(["-L", SOCKET, "kill-server"])
        .output();
}

/// Where `name` lives, or `None` when the machine has no such program.
fn locate(name: &str) -> Option<PathBuf> {
    let out = Command::new("sh")
        .args(["-c", &format!("command -v {name}")])
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| PathBuf::from(String::from_utf8_lossy(&out.stdout).trim()))
}

fn git(dir: &Path, args: &[&str]) {
    let ok = Command::new("git")
        .args(args)
        .current_dir(dir)
        // Scrubbed so an inherited GIT_* var cannot reach into this repo.
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .output()
        .expect("run git")
        .status
        .success();
    assert!(ok, "git {args:?} failed");
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

#[test]
fn a_spawned_pane_resolves_the_cli_its_hooks_call() {
    let (Some(tmux), Some(git_bin)) = (locate("tmux"), locate("git")) else {
        eprintln!("skipping: tmux or git is not installed");
        return;
    };

    let cli = PathBuf::from(env!("CARGO_BIN_EXE_thurbox-cli"));
    let cli_dir = cli.parent().expect("the CLI has a directory").to_path_buf();

    let root = tempfile::tempdir().expect("tempdir");
    let home = root.path().join("home");
    let config = root.path().join("config");
    let data = root.path().join("data");
    let checkout = root.path().join("repo");
    for dir in [&home, &config, &data] {
        std::fs::create_dir_all(dir).expect("mkdir");
    }
    repo(&checkout);

    // The stripped `PATH` sshd hands a delegated command: enough to run the
    // tools the spawn itself shells out to, and deliberately without the
    // directory this build's `thurbox-cli` sits in.
    let mut dirs: Vec<PathBuf> = Vec::new();
    for tool in [&tmux, &git_bin] {
        let dir = tool.parent().expect("tool directory").to_path_buf();
        if dir != cli_dir && !dirs.contains(&dir) {
            dirs.push(dir);
        }
    }
    if dirs.is_empty() {
        eprintln!("skipping: this build's CLI shares a directory with tmux/git");
        return;
    }
    let stripped = std::env::join_paths(&dirs).expect("join PATH");

    // The agent reports what the pane's own `PATH` finds — exactly the lookup
    // every status hook does before it can signal anything.
    let seen = root.path().join("cli-seen");
    std::fs::write(
        config.join("agents.toml"),
        format!(
            "default = \"probe\"\n\n[[agents]]\nname = \"probe\"\ncommand = \"sh\"\n\
             args = [\"-c\", \"command -v thurbox-cli > {} 2>&1; sleep 30\"]\n",
            seen.display()
        ),
    )
    .expect("write agents.toml");

    cleanup();
    let out = Command::new(&cli)
        .args([
            "session",
            "create",
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
            "--json",
        ])
        .env("PATH", &stripped)
        .env("HOME", &home)
        .env("THURBOX_CONFIG_DIR", &config)
        .env("THURBOX_DATA_DIR", &data)
        .env("TMUX_TMPDIR", root.path())
        .env("THURBOX_SOCKET", SOCKET)
        // An injected socket is ruled inherited when its paired data dir is
        // not this one's (see `socket_for`); this test is somebody typing it.
        .env_remove("THURBOX_SOCKET_FOR")
        .env_remove("THURBOX_SESSION")
        .env_remove("THURBOX_SESSION_ID")
        .output()
        .expect("run thurbox-cli session create");
    if !out.status.success() {
        cleanup();
        let stderr = String::from_utf8_lossy(&out.stderr);
        if stderr.contains("tmux") || stderr.contains("multiplexer") {
            eprintln!("skipping: tmux would not spawn a window: {stderr}");
            return;
        }
        panic!(
            "the spawn itself failed:\nstdout: {}\nstderr: {stderr}",
            String::from_utf8_lossy(&out.stdout)
        );
    }

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while std::time::Instant::now() < deadline && !seen.exists() {
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    let found = std::fs::read_to_string(&seen);
    cleanup();

    // Separated from the lookup below so a pane that never ran the agent at all
    // cannot read as a pane whose `PATH` came up empty.
    let found = found.expect("the agent ran and reported what its PATH found");
    assert_eq!(
        found.trim(),
        cli.to_string_lossy(),
        "the pane's PATH did not find the thurbox-cli its status hooks call, \
         so every signal resolved nothing and `|| true` swallowed it"
    );
}
