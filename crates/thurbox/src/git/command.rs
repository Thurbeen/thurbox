use std::path::Path;
use std::process::{Command, Stdio};

use anyhow::{Context, Result};

use super::reportable_stderr;

pub(super) const GIT_LOCATION_ENV: [&str; 8] = [
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_INDEX_FILE",
    "GIT_COMMON_DIR",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_PREFIX",
    "GIT_NAMESPACE",
];

pub(crate) fn scrub_git_location_env(cmd: &mut Command) {
    for var in GIT_LOCATION_ENV {
        cmd.env_remove(var);
    }
}

pub(crate) fn git_program() -> Command {
    let mut cmd = Command::new("git");
    scrub_git_location_env(&mut cmd);
    cmd
}

pub(super) fn non_interactive(cmd: &mut Command) {
    cmd.env("GIT_TERMINAL_PROMPT", "0");
    let ssh = std::iter::once("ssh")
        .chain(crate::shell::ssh_appended_opts())
        .collect::<Vec<_>>()
        .join(" ");
    cmd.env("GIT_SSH_COMMAND", ssh);
}

pub(super) fn run_git(mut cmd: Command, what: &str) -> Result<()> {
    let output = cmd
        .output()
        .with_context(|| format!("failed to run {what}"))?;
    if !output.status.success() {
        let stderr = reportable_stderr(&output.stderr);
        anyhow::bail!("{what} failed: {stderr}");
    }
    Ok(())
}

pub(super) fn run_git_capture(args: &[&str], cwd: &Path) -> Option<String> {
    capture(git_command(cwd, args))
}

pub(super) fn run_git_capture_env(
    args: &[&str],
    cwd: &Path,
    env: &[(&str, &str)],
) -> Option<String> {
    let mut cmd = git_command(cwd, args);
    cmd.envs(env.iter().copied());
    capture(cmd)
}

fn capture(mut cmd: Command) -> Option<String> {
    let output = cmd
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&output.stdout).into_owned())
}

pub(super) fn git_command(cwd: &Path, args: &[&str]) -> Command {
    let mut cmd = git_program();
    cmd.current_dir(cwd);
    cmd.args(args);
    cmd
}
