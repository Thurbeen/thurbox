//! Every `git` invocation starts here.
//!
//! One thing matters more than the rest and is why this is its own file: git
//! exports `GIT_DIR`/`GIT_INDEX_FILE`/… to hook processes, so a thurbox running
//! under a `pre-commit` hook — or its own test suite under this project's
//! `cargo nextest` hook — inherits them, and a call aimed at an explicit
//! worktree silently operates on the *hook's* repo instead. Every invocation
//! scrubs them. Route new git calls through [`git_program`]/[`git_command`]
//! rather than `Command::new("git")`, and that stays true for free.

use super::*;

/// The ambient `GIT_*` variables that pin git to a specific repo/index/worktree,
/// overriding the path we point it at via `current_dir`/`-C`. Git exports these
/// to hook processes (a `pre-commit` hook runs with `GIT_DIR`/`GIT_INDEX_FILE`
/// set), so if thurbox — or its test suite under the project's pre-commit
/// `cargo nextest` hook — inherits them, a `git` call targeting an explicit
/// worktree would silently operate on the *hook's* repo instead (writing the
/// wrong index, running the wrong hooks). Every git invocation scrubs them so it
/// always discovers the repo from the path it was given.
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

/// Remove the [`GIT_LOCATION_ENV`] variables from `cmd` so an inherited git-hook
/// environment can't redirect a path-targeted git call to the wrong repo.
pub(crate) fn scrub_git_location_env(cmd: &mut Command) {
    for var in GIT_LOCATION_ENV {
        cmd.env_remove(var);
    }
}

// ── working copies for installed interface plugins ─────────────────────────
//
// A plugin that carries more than Lua is a repository: `git clone` delivers
// arbitrary bytes, preserves whatever layout the author chose, identifies exactly
// what it delivered, and refuses to clobber a dirty working tree. These are the
// operations `kernel::packages` needs for that, local only — a plugin's pane has no
// session and therefore no host to run on.

/// A `git` [`Command`] with the ambient repo-location environment scrubbed (see
/// [`scrub_git_location_env`]). Every git invocation — production and tests —
/// must start from this rather than a bare `Command::new("git")`, so it always
/// targets the path it is pointed at regardless of any inherited `GIT_*` vars.
pub(crate) fn git_program() -> Command {
    let mut cmd = Command::new("git");
    scrub_git_location_env(&mut cmd);
    cmd
}

/// Make a git invocation refuse to ask the user anything.
///
/// A clone of a private repository can otherwise block forever on an SSH
/// passphrase or a host-key confirmation, and an install runs from the command
/// drain — so a prompt is a frozen interface, with no indication why. Two knobs
/// cover it: `GIT_TERMINAL_PROMPT=0` stops git asking for HTTPS credentials, and
/// `GIT_SSH_COMMAND` carries the same appended set every other ssh use here
/// applies ([`crate::shell::ssh_appended_opts`] — hardening plus connection
/// multiplexing, so repeated fetches against an ssh remote reuse one
/// connection). A clone that would have prompted fails with a message instead.
pub(super) fn non_interactive(cmd: &mut Command) {
    cmd.env("GIT_TERMINAL_PROMPT", "0");
    let ssh = std::iter::once("ssh")
        .chain(crate::shell::ssh_appended_opts())
        .collect::<Vec<_>>()
        .join(" ");
    cmd.env("GIT_SSH_COMMAND", ssh);
}

/// Run a git command that produces no output worth keeping, or fail with stderr.
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

/// Run `git <args>` in `cwd` locally, returning stdout on success (`None`
/// otherwise).
pub(super) fn run_git_capture(args: &[&str], cwd: &Path) -> Option<String> {
    run_git_capture_on(None, args, cwd)
}

/// [`run_git_capture`], optionally on a remote `host`.
pub(super) fn run_git_capture_on(
    host: Option<&HostDef>,
    args: &[&str],
    cwd: &Path,
) -> Option<String> {
    let output = git_command(host, cwd, args)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Build the launcher [`Command`] for an off-local host: `ssh <opts> <dest>`
/// for an SSH host, or `wsl.exe -d <distro>` for a WSL distro. Both join and
/// shell-interpret the trailing tokens identically, so callers append the same
/// POSIX-quoted command afterward.
pub(super) fn host_launcher(h: &HostDef) -> Command {
    if h.is_wsl() {
        wsl_command(&h.distro_name())
    } else {
        ssh_command(&h.destination, &h.ssh_opts)
    }
}

/// Build a `git` [`Command`] targeting `cwd`, run locally, over SSH, or inside
/// a WSL distro.
///
/// For an off-local host the command becomes
/// `<launcher> git -C <cwd> <args…>` (launcher = `ssh <opts> <dest>` or
/// `wsl.exe -d <distro>`), with each token shell-escaped so it survives the
/// host login shell's re-splitting.
pub(super) fn git_command(host: Option<&HostDef>, cwd: &Path, args: &[&str]) -> Command {
    match host {
        None => {
            let mut cmd = git_program();
            cmd.current_dir(cwd);
            cmd.args(args);
            cmd
        }
        Some(h) => {
            let mut cmd = host_launcher(h);
            cmd.arg(posix_quote("git"));
            cmd.arg(posix_quote("-C"));
            cmd.arg(posix_quote(&cwd.to_string_lossy()));
            for a in args {
                cmd.arg(posix_quote(a));
            }
            cmd
        }
    }
}
