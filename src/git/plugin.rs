//! Cloning and updating a plugin's working copy.
//!
//! A plugin that carries more than Lua is a repository, and it arrives *only*
//! by clone: the plain fetch path returns a `String` and decodes remote output
//! lossily, so a binary through it would be corrupted rather than refused. The
//! working copy keeps its `.git`, which is what makes `update` a fetch and lets
//! git own "your edits are yours" — a dirty tree is never moved.

use super::*;

/// Does this ref name a commit rather than a branch or a tag?
///
/// The distinction is forced on us by git: `clone --branch` accepts a branch or a
/// tag and **rejects a commit id**, so a pin that is one has to be obtained the
/// long way round. Ambiguity resolves toward the commit — a branch could be named
/// `deadbeef` and would be read as one — because a hex-looking branch name is a
/// curiosity and pinning a commit is the whole point of a lock.
pub(super) fn names_a_commit(git_ref: &str) -> bool {
    (7..=40).contains(&git_ref.len()) && git_ref.chars().all(|c| c.is_ascii_hexdigit())
}

/// Clone `url` into `dest`, shallow, optionally at `git_ref`.
///
/// `--depth 1` because running a pane needs no history and a shallow clone is
/// dramatically faster; the two properties keeping `.git` is *for* — refusing to
/// overwrite a dirty tree, and `git diff` against the checkout — need none either.
///
/// `git_ref` may be a branch, a tag **or a commit id**; the last cannot be cloned
/// directly, so it is fetched and checked out afterwards. A failure at either step
/// takes the clone back, so a caller that has written nothing yet leaves nothing
/// behind.
pub fn clone_plugin(url: &str, dest: &Path, git_ref: Option<&str>) -> Result<()> {
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
        let obtained = fetch_ref(dest, commit).and_then(|()| checkout_plugin(dest, commit));
        if let Err(e) = obtained {
            // Clone succeeded, the pin did not: this working copy is at the wrong
            // revision and nobody asked for that one.
            let _ = std::fs::remove_dir_all(dest);
            // Said distinctly from a failed clone, because the two have different
            // fixes and the common cause is invisible: a rebase or squash merge
            // *replaces* commits, so a pin taken from a pull request that has since
            // been merged names an object the remote no longer has. Reachable
            // without the pin, which is the actual next step.
            return Err(e.context(format!(
                "the repository cloned, but commit {commit} could not be obtained \
                 from it — if that commit came from a branch which was since \
                 rebased, squashed or deleted, the remote no longer has it"
            )));
        }
    }
    Ok(())
}

/// Fetch one ref — a commit id, a branch or a tag — into a shallow working copy.
///
/// What makes a recorded commit reproducible: a shallow clone has only the tip, so
/// reproducing a spec elsewhere has to ask for the exact object the lock names.
/// Checking out the result means checking out `FETCH_HEAD`, not the ref's name: a
/// fetch does not move the local branch a `--branch` clone left checked out.
pub fn fetch_ref(repo: &Path, git_ref: &str) -> Result<()> {
    let mut cmd = git_command(None, repo, &["fetch", "--depth", "1", "origin", git_ref]);
    non_interactive(&mut cmd);
    run_git(cmd, "git fetch")
}

/// Fetch whatever the remote's default branch now points at.
pub fn fetch_tip(repo: &Path) -> Result<()> {
    let mut cmd = git_command(None, repo, &["fetch", "--depth", "1", "origin"]);
    non_interactive(&mut cmd);
    run_git(cmd, "git fetch")
}

/// Check out a revision in a working copy.
pub fn checkout_plugin(repo: &Path, rev: &str) -> Result<()> {
    let cmd = git_command(None, repo, &["checkout", "--detach", rev]);
    run_git(cmd, "git checkout")
}

/// The commit a working copy is on.
///
/// Recorded in the lock instead of the ref that was asked for: `main` moves, and a
/// spec that reproduces "whatever main is now" reproduces nothing.
pub fn head_commit(repo: &Path) -> Result<String> {
    let output = git_command(None, repo, &["rev-parse", "HEAD"])
        .output()
        .context("failed to run git rev-parse")?;
    if !output.status.success() {
        let stderr = reportable_stderr(&output.stderr);
        anyhow::bail!("git rev-parse failed: {stderr}");
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// Does this working copy have uncommitted changes?
///
/// The question that makes "your edits are yours" git's job rather than the
/// delivery matrix's: a dirty copy is never moved, and the caller reports it as
/// kept.
pub fn is_dirty(repo: &Path) -> Result<bool> {
    let output = git_command(None, repo, &["status", "--porcelain"])
        .output()
        .context("failed to run git status")?;
    if !output.status.success() {
        let stderr = reportable_stderr(&output.stderr);
        anyhow::bail!("git status failed: {stderr}");
    }
    Ok(!String::from_utf8_lossy(&output.stdout).trim().is_empty())
}

/// Whether `dir` is the root of a working copy.
pub fn is_working_copy(dir: &Path) -> bool {
    dir.join(".git").exists()
}
