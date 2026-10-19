//! The built-in `shell` agent opens the interactive shell of the machine the
//! session runs on, not the one thurbox was built for (#1458).
//!
//! `session create` runs in-process against a registry whose route to a
//! Windows SSH host is served by a recording backend, with sharing off so the
//! session is created from here — the path a fork, or a host whose CLI is
//! unusable, also takes. The local `agents.toml` is the stock one this build
//! seeds, so its `shell` entry is spelled for this machine.

#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;
use std::sync::Arc;

use clap::Parser;
use thurbox::session::{Multiplexer, Route, Via};
use thurbox::storage::Database;

#[path = "support/recording_backend.rs"]
mod recording_backend;

use recording_backend::RecordingBackend;

const HOSTS_TOML: &str = "[[hosts]]\n\
     name = \"winhost\"\n\
     destination = \"e2e@winhost.invalid\"\n\
     multiplexer = \"psmux\"\n\
     platform = \"windows\"\n\
     share_sessions = false\n";

fn cli(db: &Database, backends: &thurbox::cli::Backends<'_>, args: &[&str]) -> Result<(), String> {
    let parsed =
        thurbox::cli::Cli::try_parse_from(["thurbox-cli", "--json"].iter().chain(args).copied())
            .map_err(|e| format!("parse {args:?}: {e}"))?;
    match thurbox::cli::run(parsed, db, backends) {
        Ok(thurbox::cli::Outcome::Ok) => Ok(()),
        Ok(thurbox::cli::Outcome::Failed { message, .. }) => Err(message),
        Err(e) => Err(e.message),
    }
}

fn git(dir: &Path, args: &[&str]) {
    let mut cmd = Command::new("git");
    cmd.args(args).current_dir(dir);
    for var in [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_INDEX_FILE",
        "GIT_COMMON_DIR",
        "GIT_OBJECT_DIRECTORY",
        "GIT_ALTERNATE_OBJECT_DIRECTORIES",
        "GIT_PREFIX",
        "GIT_NAMESPACE",
    ] {
        cmd.env_remove(var);
    }
    assert!(cmd.output().expect("git").status.success(), "git {args:?}");
}

#[test]
fn the_shell_agent_on_a_windows_host_is_powershell() {
    let root = tempfile::tempdir().expect("tempdir");
    thurbox::paths::set_test_dir(root.path());
    let config = thurbox::paths::config_file()
        .expect("config path")
        .parent()
        .expect("config dir")
        .to_path_buf();
    std::fs::create_dir_all(&config).expect("mkdir config");
    std::fs::write(config.join("hosts.toml"), HOSTS_TOML).expect("hosts.toml");

    // An `ssh` that never connects: nothing about the host is learnt from it.
    let bin = root.path().join("bin");
    std::fs::create_dir_all(&bin).expect("mkdir bin");
    let ssh = bin.join("ssh");
    std::fs::write(&ssh, "#!/bin/sh\nexit 255\n").expect("ssh stand-in");
    std::fs::set_permissions(&ssh, std::fs::Permissions::from_mode(0o700)).expect("chmod");
    let path = std::env::var_os("PATH").unwrap_or_default();
    let mut dirs = vec![bin];
    dirs.extend(std::env::split_paths(&path));
    std::env::set_var("PATH", std::env::join_paths(dirs).expect("PATH"));
    for var in ["TMUX", "TMUX_PANE", "THURBOX_SESSION", "THURBOX_SESSION_ID"] {
        std::env::remove_var(var);
    }
    let mut settings = thurbox::session::settings::Settings::default();
    settings.features.automations = false;
    thurbox::session::settings::init(settings);

    let repo = tempfile::tempdir().expect("tempdir");
    git(repo.path(), &["init", "-q", "-b", "main"]);

    let registry = |far: &Arc<RecordingBackend>| {
        let (mut backends, _hosts, _warnings) = thurbox::backend::wiring::configured();
        backends.register(
            Route::remote(Via::Ssh, "winhost", Some(Multiplexer::Psmux)),
            far.clone(),
        );
        backends
    };
    let far = RecordingBackend::new(&Route::remote(
        Via::Ssh,
        "winhost",
        Some(Multiplexer::Psmux),
    ));
    let backends = thurbox::cli::Backends::ready(registry(&far));
    let db = Database::open_in_memory().expect("db");

    cli(
        &db,
        &backends,
        &[
            "session",
            "create",
            "--name",
            "win",
            "--repo-path",
            &repo.path().display().to_string(),
            "--agent",
            "shell",
            "--host",
            "winhost",
        ],
    )
    .expect("create on the Windows host");

    let row = db.get_session_by_name("win").unwrap().expect("the row");
    let windows = Arc::clone(&far).windows_of(&row.id.to_string());
    assert_eq!(windows.len(), 1, "{:?}", far.windows());
    let command = &windows[0].command;
    assert!(
        command.contains("powershell") && command.contains("-NoLogo"),
        "the Windows host must get its own shell, not this build's: {command}"
    );
    assert!(!command.contains("bash"), "{command}");

    // What `agent launch-args shell --session` tells a driver to run must be
    // what the spawn ran.
    let plan = thurbox::session_ops::agent_launch_plan(&db, &registry(&far), "shell", Some(&row))
        .expect("the plan for the session");
    assert_eq!(
        (plan.command.as_str(), plan.args.as_slice()),
        ("powershell", ["-NoLogo".to_string()].as_slice())
    );
}
