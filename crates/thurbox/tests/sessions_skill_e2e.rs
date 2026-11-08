//! The built-in `sessions-skill` extension, end to end through the real
//! binaries: the `thurbox-sessions` skill lands in each installed coding CLI's
//! personal skill directory when a session is created, follows its own toggle,
//! and every command it teaches works between two live sessions.
//!
//! Scoped to a throwaway tmux socket, a temporary `HOME` and thurbox's
//! config/data overrides, and skipped when tmux is absent, like the other
//! end-to-end tests here.

#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::Value;

#[path = "../../runtime-mux/tests/support/tmux_server.rs"]
mod tmux_server;

use tmux_server::TmuxServer;

const SOCKET: &str = "thurbox-sessions-skill-e2e";

/// The payload the binary embeds, read from where the skill is authored.
const SKILL: &str = include_str!("../../../extensions/sessions-skill/SKILL.md");

/// The `GIT_*` location variables a pre-commit hook exports; a git call here
/// that inherited them would write to the real repository.
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
        .is_ok_and(|o| o.status.success())
}

fn git(dir: &Path, args: &[&str]) {
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

fn repo() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    git(dir.path(), &["init", "-q", "-b", "main"]);
    git(dir.path(), &["config", "user.email", "t@example.com"]);
    git(dir.path(), &["config", "user.name", "thurbox-test"]);
    git(dir.path(), &["config", "commit.gpgsign", "false"]);
    std::fs::write(dir.path().join("README.md"), "# probe\n").expect("write");
    git(dir.path(), &["add", "."]);
    git(dir.path(), &["commit", "-qm", "init"]);
    dir
}

/// An interactive bash with Readline, pinned rather than `sh`: `session send`
/// pastes bracketed, which bash consumes and dash (`/bin/sh` on Debian and
/// Ubuntu) types into its line as literal escape codes.
const SHELL_AGENT: &str = "default = \"shell\"\n\n[[agents]]\nname = \"shell\"\n\
    command = \"env\"\nargs = [\"INPUTRC=/dev/null\", \"PS1=$ \", \"bash\", \"--norc\", \"--noprofile\", \"-i\"]\n";

/// A `HOME` of its own holding `~/.claude` and `~/.codex` (so those two skill
/// destinations are live and the others are not), with thurbox's config and
/// data pointed into it and a `shell` agent as the default — a real coding
/// agent would want credentials and a network.
fn sandbox() -> (tempfile::TempDir, PathBuf) {
    let home = tempfile::tempdir().expect("tempdir");
    std::env::set_var("HOME", home.path());
    for variable in ["XDG_CONFIG_HOME", "XDG_DATA_HOME", "XDG_CACHE_HOME"] {
        let directory = home.path().join(variable);
        std::fs::create_dir_all(&directory).unwrap();
        std::env::set_var(variable, directory);
    }
    for agent_dir in [".claude", ".codex"] {
        std::fs::create_dir_all(home.path().join(agent_dir)).unwrap();
    }
    thurbox::paths::set_test_dir(home.path());
    std::env::set_var(thurbox::paths::CONFIG_DIR_OVERRIDE_ENV, home.path());
    std::env::set_var(thurbox::paths::DATA_DIR_OVERRIDE_ENV, home.path());
    for inherited in ["THURBOX_SESSION", "THURBOX_SESSION_NAME", "THURBOX_TASK"] {
        std::env::remove_var(inherited);
    }
    let config = thurbox::paths::config_file()
        .expect("config path")
        .parent()
        .expect("config dir")
        .to_path_buf();
    std::fs::create_dir_all(&config).unwrap();
    std::fs::write(config.join("agents.toml"), SHELL_AGENT).unwrap();
    let home_path = home.path().to_path_buf();
    (home, home_path)
}

/// Run `thurbox-cli --json <args>`, as the session `me` when given — the
/// environment a command run by an agent inside that session carries.
fn cli_as(me: Option<&str>, args: &[&str]) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_thurbox-cli"));
    cmd.arg("--json").args(args);
    if let Some(me) = me {
        cmd.env("THURBOX_SESSION", me);
    }
    cmd.output().expect("run thurbox-cli")
}

fn json_as(me: Option<&str>, args: &[&str]) -> Value {
    let out = cli_as(me, args);
    assert!(
        out.status.success(),
        "thurbox-cli {args:?}: {}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).expect("--json output")
}

fn json(args: &[&str]) -> Value {
    json_as(None, args)
}

fn create(name: &str, repo: &Path) -> String {
    let created = json(&[
        "session",
        "create",
        "--name",
        name,
        "--repo-path",
        repo.to_str().unwrap(),
        "--agent",
        "shell",
    ]);
    created["id"].as_str().expect("created id").to_string()
}

fn skill_copies(home: &Path, skill: &str) -> Vec<PathBuf> {
    [
        ".claude/skills",
        ".codex/skills",
        ".config/opencode/skills",
        ".copilot/skills",
        ".agents/skills",
    ]
    .iter()
    .map(|dir| home.join(dir).join(skill).join("SKILL.md"))
    .filter(|p| p.is_file())
    .collect()
}

/// Creating a session wires the built-ins before the agent launches, so the
/// first session on a machine already has the skill; the toggle takes every
/// copy back and restores them, and leaves the other skill built-in alone.
#[test]
fn the_skill_ships_with_a_new_session_and_follows_its_toggle() {
    if !have_tmux() {
        eprintln!("skipping: needs tmux");
        return;
    }
    let repo = repo();
    let _server = TmuxServer::pin(SOCKET);
    let (_guard, home) = sandbox();

    create("alpha", repo.path());

    let expected = [
        home.join(".claude/skills/thurbox-sessions/SKILL.md"),
        home.join(".codex/skills/thurbox-sessions/SKILL.md"),
    ];
    assert_eq!(skill_copies(&home, "thurbox-sessions"), expected);
    for copy in &expected {
        assert_eq!(std::fs::read_to_string(copy).unwrap(), SKILL);
    }
    assert!(
        !home.join(".copilot").exists(),
        "a CLI that is not installed must not get a config tree"
    );

    json(&["extension", "deactivate", "sessions-skill"]);
    assert!(skill_copies(&home, "thurbox-sessions").is_empty());
    assert_eq!(
        skill_copies(&home, "thurbox-ui").len(),
        2,
        "turning one skill off leaves the other"
    );
    create("beta", repo.path());
    assert!(
        skill_copies(&home, "thurbox-sessions").is_empty(),
        "an opted-out skill must not come back with the next session"
    );

    json(&["extension", "activate", "sessions-skill"]);
    assert_eq!(skill_copies(&home, "thurbox-sessions"), expected);
}

/// Every `thurbox-cli <group> <verb>` the skill names must exist, so the skill
/// cannot teach a command a release renamed or removed.
#[test]
fn every_command_the_skill_names_exists() {
    let pattern = regex::Regex::new(r"thurbox-cli ([a-z]+) ([a-z][a-z-]*)").unwrap();
    let mut named: Vec<(String, String)> = pattern
        .captures_iter(SKILL)
        .map(|c| (c[1].to_string(), c[2].to_string()))
        .collect();
    named.sort();
    named.dedup();
    assert!(named.len() >= 6, "the skill teaches commands: {named:?}");
    for (group, verb) in named {
        let out = Command::new(env!("CARGO_BIN_EXE_thurbox-cli"))
            .args([&group, &verb, "--help"])
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "the skill names `thurbox-cli {group} {verb}`, which does not exist: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

/// The skill's workflow between two live sessions, each command run as the
/// calling session would run it: find the peer, read it without touching it,
/// type into its idle prompt, and hand it a message through the mailbox.
#[test]
fn the_documented_commands_work_between_two_sessions() {
    if !have_tmux() {
        eprintln!("skipping: needs tmux");
        return;
    }
    let repo = repo();
    let _server = TmuxServer::pin(SOCKET);
    let (_guard, _home) = sandbox();

    let alpha = create("alpha", repo.path());
    let beta = create("beta", repo.path());
    let me = Some(alpha.as_str());

    let listed = json_as(me, &["session", "list"]);
    let names: Vec<&str> = listed
        .as_array()
        .expect("session list rows")
        .iter()
        .filter_map(|r| r["name"].as_str())
        .collect();
    assert!(
        names.contains(&"alpha") && names.contains(&"beta"),
        "{listed}"
    );

    let mine = json_as(me, &["session", "get", &alpha]);
    assert_eq!(mine["name"], "alpha", "THURBOX_SESSION names me: {mine}");
    let peer = json_as(me, &["session", "get", "beta"]);
    assert_eq!(peer["id"], beta, "{peer}");
    assert!(peer.get("state").is_some(), "get reports a state: {peer}");

    // Wait for the shell's prompt to draw. The pattern is a regex: a bare `$`
    // would match the empty pane at once.
    json_as(
        me,
        &[
            "session",
            "wait",
            "beta",
            "--pane-matches",
            r"(?m)\$\s*$",
            "--timeout",
            "20",
        ],
    );
    json_as(me, &["session", "send", "beta", "printf 'PO%s\\n' NG"]);
    let waited = cli_as(
        me,
        &[
            "session",
            "wait",
            "beta",
            "--pane-matches",
            "PONG",
            "--timeout",
            "20",
        ],
    );
    assert!(
        waited.status.success(),
        "the peer never ran what was sent: {}",
        String::from_utf8_lossy(&waited.stdout)
    );
    let captured = json_as(me, &["session", "capture", "beta", "--lines", "50"]);
    assert!(
        captured["output"].as_str().unwrap_or("").contains("PONG"),
        "{captured}"
    );

    let sent = json_as(
        me,
        &[
            "message",
            "send",
            "--to",
            "beta",
            "--kind",
            "note",
            "--body",
            "hello beta",
        ],
    );
    assert_eq!(sent["delivered_via"], "mailbox", "{sent}");
    let inbox = json_as(Some(&beta), &["message", "inbox"]);
    let rows = inbox
        .get("messages")
        .and_then(Value::as_array)
        .or_else(|| inbox.as_array())
        .unwrap_or_else(|| panic!("inbox rows: {inbox}"));
    let got = rows
        .iter()
        .find(|m| m["body"] == "hello beta")
        .unwrap_or_else(|| panic!("beta never received the message: {inbox}"));
    assert_eq!(
        got["from_session_id"].as_str(),
        Some(alpha.as_str()),
        "{got}"
    );
}
