use std::path::Path;

use anyhow::{Context, Result};

use super::{git_command, git_program, non_interactive, reportable_stderr, run_git};

const SHA1_ID: usize = 40;
const SHA256_ID: usize = 64;

const SHORTEST_ABBREVIATION: usize = 7;

fn is_hex(git_ref: &str) -> bool {
    git_ref.chars().all(|c| c.is_ascii_hexdigit())
}

pub(super) fn names_a_commit(git_ref: &str) -> bool {
    (SHORTEST_ABBREVIATION..=SHA256_ID).contains(&git_ref.len()) && is_hex(git_ref)
}

pub(super) fn abbreviated_object_id(git_ref: &str) -> bool {
    let len = git_ref.len();
    (SHORTEST_ABBREVIATION..SHA256_ID).contains(&len) && len != SHA1_ID && is_hex(git_ref)
}

pub fn clone_mod(url: &str, dest: &Path, git_ref: Option<&str>) -> Result<()> {
    if dest.exists() {
        anyhow::bail!("{} already exists", dest.display());
    }
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    let mut cmd = git_program();
    non_interactive(&mut cmd);
    cmd.arg("clone").arg("--depth").arg("1");
    // A commit id is not a `--branch`: clone the default tip, then go and get it.
    let commit = git_ref.filter(|git_ref| names_a_commit(git_ref));
    if let Some(name) = git_ref.filter(|git_ref| !names_a_commit(git_ref)) {
        cmd.arg("--branch").arg(name);
    }
    cmd.arg(url).arg(dest);
    run_git(cmd, "git clone")?;

    if let Some(commit) = commit {
        let fetched = fetch_ref(dest, commit);
        // A fetch that failed on a ref shaped like a prefix is offered the reading
        // the shape suggests, rather than the one below. The clone succeeded a
        // moment ago, so an unreachable remote is the unlikely half — but it is
        // not impossible, which is why the message says "unless git's own reason
        // is a different one" and leaves git's in the chain to compare.
        let prefix = fetched.is_err() && abbreviated_object_id(commit);
        // `FETCH_HEAD`, not the ref's own name: a shallow fetch writes no local
        // ref, so a *name* the remote did serve still resolves to nothing here —
        // and the object it handed over is `FETCH_HEAD` whichever the ref was.
        // `converge_repository` checks the same thing out, for the same reason.
        let obtained = fetched.and_then(|()| checkout_mod(dest, "FETCH_HEAD"));
        if let Err(e) = obtained {
            // Clone succeeded, the pin did not: this working copy is at the wrong
            // revision and nobody asked for that one.
            let _ = std::fs::remove_dir_all(dest);
            if prefix {
                return Err(e).with_context(|| {
                    format!(
                        "the repository cloned, but {commit} could not be fetched from \
                         it — and it reads as an abbreviated commit id, which no remote \
                         can serve: one answers with branches, tags and whole object \
                         ids, never a prefix of one. So unless git's own reason below \
                         is a different one, pin the full id, or the branch or tag that \
                         carries it"
                    )
                });
            }
            // Said distinctly from a failed clone, because the two have different
            // fixes and the common cause is invisible: a rebase or squash merge
            // *replaces* commits, so a pin taken from a pull request that has since
            // been merged names an object the remote no longer has. Reachable
            // without the pin, which is the actual next step.
            return Err(e).with_context(|| {
                format!(
                    "the repository cloned, but commit {commit} could not be obtained \
                     from it — if that commit came from a branch which was since \
                     rebased, squashed or deleted, the remote no longer has it"
                )
            });
        }
    }
    Ok(())
}

pub fn fetch_ref(repo: &Path, git_ref: &str) -> Result<()> {
    let mut cmd = git_command(repo, &["fetch", "--depth", "1", "origin", git_ref]);
    non_interactive(&mut cmd);
    run_git(cmd, "git fetch")
}

pub fn fetch_tip(repo: &Path) -> Result<()> {
    let mut cmd = git_command(repo, &["fetch", "--depth", "1", "origin"]);
    non_interactive(&mut cmd);
    run_git(cmd, "git fetch")
}

pub fn checkout_mod(repo: &Path, rev: &str) -> Result<()> {
    let cmd = git_command(repo, &["checkout", "--detach", rev]);
    run_git(cmd, "git checkout")
}

pub fn head_commit(repo: &Path) -> Result<String> {
    let output = git_command(repo, &["rev-parse", "HEAD"])
        .output()
        .context("failed to run git rev-parse")?;
    if !output.status.success() {
        let stderr = reportable_stderr(&output.stderr);
        anyhow::bail!("git rev-parse failed: {stderr}");
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

pub fn is_dirty(repo: &Path) -> Result<bool> {
    let output = git_command(repo, &["status", "--porcelain"])
        .output()
        .context("failed to run git status")?;
    if !output.status.success() {
        let stderr = reportable_stderr(&output.stderr);
        anyhow::bail!("git status failed: {stderr}");
    }
    Ok(!String::from_utf8_lossy(&output.stdout).trim().is_empty())
}

pub fn is_working_copy(dir: &Path) -> bool {
    dir.join(".git").exists()
}
