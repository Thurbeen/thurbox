//! Creating, syncing and removing the worktrees sessions live in.
//!
//! A worktree's path is derived, never stored: repo name plus a stable FNV-1a
//! hash of the repo path plus the branch, so two repos with the same basename
//! do not collide and the same repo is always the same directory.
//!
//! Syncing is where the sharp edges are, and each retry here is a bug that
//! happened: a stale `index.lock` left by a killed git, a stash that loses a
//! race with an agent writing in the same tree, and errors that are *transient*
//! (`could not write index`) rather than real. `is_transient_error` is
//! deliberately a matcher over messages — git offers no code for these.

use super::*;

/// Deterministic worktree directory path for a repo + branch on the given host.
///
/// Local hosts use [`worktree_path`]. Remote hosts place worktrees under the
/// host's `worktrees_dir` (or `$HOME/.local/share/thurbox/worktrees` resolved
/// over ssh), preserving the same `<repo-hash>/<sanitized-branch>` layout.
pub(super) fn worktree_path_for(
    host: Option<&HostDef>,
    repo_path: &Path,
    branch: &str,
) -> Result<PathBuf> {
    match host {
        None => worktree_path(repo_path, branch).context("failed to resolve worktrees directory"),
        Some(h) => {
            let base = match &h.worktrees_dir {
                Some(dir) => dir.clone(),
                None => format!("{}/.local/share/thurbox/worktrees", remote_home(h)?),
            };
            // The host is remote (always POSIX), so the path must be `/`-joined
            // even when thurbox itself runs on Windows — `PathBuf::join` would
            // otherwise insert `\` and produce a path the remote shell rejects.
            Ok(PathBuf::from(worktree_subpath_posix(
                &base, repo_path, branch,
            )))
        }
    }
}

/// Create a git worktree on a new branch and return the worktree directory path.
///
/// Creates `new_branch` starting from `base_branch`.
/// Path format: `~/.local/share/thurbox/worktrees/<repo-hash>/<sanitized-branch>`
pub fn create_worktree(repo_path: &Path, new_branch: &str, base_branch: &str) -> Result<PathBuf> {
    create_worktree_on(None, repo_path, new_branch, base_branch)
}

/// [`create_worktree`], optionally on a remote `host` (via `ssh <dest> git …`).
/// On a remote host the worktree is created under the host's remote worktrees
/// directory and the returned path is a remote path.
pub fn create_worktree_on(
    host: Option<&HostDef>,
    repo_path: &Path,
    new_branch: &str,
    base_branch: &str,
) -> Result<PathBuf> {
    let wt_path = worktree_path_for(host, repo_path, new_branch)?;

    let output = git_command(
        host,
        repo_path,
        &[
            "worktree",
            "add",
            "-b",
            new_branch,
            &wt_path.display().to_string(),
            base_branch,
        ],
    )
    .output()
    .context("failed to run git worktree add")?;

    if !output.status.success() {
        let stderr = reportable_stderr(&output.stderr);
        anyhow::bail!("git worktree add failed: {stderr}");
    }

    Ok(wt_path)
}

/// Idempotently provision a worktree on `branch`, returning its directory.
///
/// Unlike [`create_worktree`] (which always passes `-b` and fails if the branch
/// already exists), this is safe to call repeatedly — the case that recurring
/// **spawn automations** hit on every fire:
///
/// - worktree directory already present → reuse it as-is;
/// - branch exists but has no worktree → attach a worktree to it
///   ([`add_existing_worktree`]);
/// - neither exists → create the branch + worktree off `base_branch`.
pub fn create_or_attach_worktree(
    repo_path: &Path,
    branch: &str,
    base_branch: &str,
) -> Result<PathBuf> {
    let wt_path =
        worktree_path(repo_path, branch).context("failed to resolve worktrees directory")?;
    if wt_path.exists() {
        return Ok(wt_path);
    }
    if branch_exists(repo_path, branch) {
        return add_existing_worktree(repo_path, branch);
    }
    create_worktree(repo_path, branch, base_branch)
}

/// Remove a git worktree (force removal).
pub fn remove_worktree(repo_path: &Path, worktree_path: &Path) -> Result<()> {
    remove_worktree_on(None, repo_path, worktree_path)
}

/// [`remove_worktree`], optionally on a remote `host`.
pub fn remove_worktree_on(
    host: Option<&HostDef>,
    repo_path: &Path,
    worktree_path: &Path,
) -> Result<()> {
    let output = git_command(
        host,
        repo_path,
        &[
            "worktree",
            "remove",
            "--force",
            &worktree_path.display().to_string(),
        ],
    )
    .output()
    .context("failed to run git worktree remove")?;

    if !output.status.success() {
        let stderr = reportable_stderr(&output.stderr);
        anyhow::bail!("git worktree remove failed: {stderr}");
    }

    Ok(())
}

/// Detect the repository's default branch name.
///
/// Tries `git symbolic-ref refs/remotes/origin/HEAD` first (most reliable),
/// then falls back to checking for `main` or `master` among local branches.
pub fn default_branch(repo_path: &Path, local_branches: &[String]) -> Option<String> {
    default_branch_on(None, repo_path, local_branches)
}

/// [`default_branch`], optionally on a remote `host`.
pub fn default_branch_on(
    host: Option<&HostDef>,
    repo_path: &Path,
    local_branches: &[String],
) -> Option<String> {
    default_branch_with_remote(
        default_branch_from_remote_on(host, repo_path).as_deref(),
        local_branches,
    )
}

/// The pure half of [`default_branch_on`]: pick the local default given an
/// already-probed remote answer. Split out so a caller that needs the remote
/// default *itself* as well (`kernel::repos`' branch ordering) runs the
/// `symbolic-ref` probe once instead of once per question.
pub fn default_branch_with_remote(
    remote_default: Option<&str>,
    local_branches: &[String],
) -> Option<String> {
    if let Some(name) = remote_default {
        if local_branches.iter().any(|b| b == name) {
            return Some(name.to_string());
        }
    }

    for candidate in ["main", "master"] {
        if local_branches.iter().any(|b| b == candidate) {
            return Some(candidate.to_string());
        }
    }

    None
}

/// Query the remote's default branch via `git symbolic-ref`.
pub fn default_branch_from_remote(repo_path: &Path) -> Option<String> {
    default_branch_from_remote_on(None, repo_path)
}

/// [`default_branch_from_remote`], optionally on a remote `host`.
pub fn default_branch_from_remote_on(host: Option<&HostDef>, repo_path: &Path) -> Option<String> {
    let output = git_command(
        host,
        repo_path,
        &["symbolic-ref", "refs/remotes/origin/HEAD", "--short"],
    )
    .stderr(Stdio::null())
    .output()
    .ok()?;

    if !output.status.success() {
        return None;
    }

    let full_ref = String::from_utf8_lossy(&output.stdout).trim().to_string();
    full_ref.strip_prefix("origin/").map(str::to_string)
}

/// Add an existing branch as a worktree (no `-b` flag — branch must already exist).
///
/// Returns the worktree directory path. If the worktree path already exists on
/// disk the function returns early with `Ok(path)`.
pub fn add_existing_worktree(repo_path: &Path, branch: &str) -> Result<PathBuf> {
    let wt_path =
        worktree_path(repo_path, branch).context("failed to resolve worktrees directory")?;

    if wt_path.exists() {
        return Ok(wt_path);
    }

    let output = git_program()
        .args(["worktree", "add", &wt_path.display().to_string(), branch])
        .current_dir(repo_path)
        .output()
        .context("failed to run git worktree add (existing branch)")?;

    if !output.status.success() {
        let stderr = reportable_stderr(&output.stderr);
        anyhow::bail!("git worktree add (existing) failed: {stderr}");
    }

    Ok(wt_path)
}

/// Check whether a local branch exists in the repository.
pub fn branch_exists(repo_path: &Path, branch: &str) -> bool {
    branch_exists_on(None, repo_path, branch)
}

/// [`branch_exists`], optionally on a remote `host`.
pub fn branch_exists_on(host: Option<&HostDef>, repo_path: &Path, branch: &str) -> bool {
    git_command(host, repo_path, &["rev-parse", "--verify", branch])
        .stderr(Stdio::null())
        .stdout(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Deterministic worktree directory path for a repo + branch.
///
/// Worktrees are placed under the XDG data directory to avoid being inside
/// the source repo (which would cause Claude Code to discover duplicate
/// `.claude/commands/` skill files).
///
/// Path format: `~/.local/share/thurbox/worktrees/<repo-hash>/<sanitized-branch>`
pub(super) fn worktree_path(repo_path: &Path, branch: &str) -> Option<PathBuf> {
    Some(worktree_subpath(
        paths::worktrees_directory()?,
        repo_path,
        branch,
    ))
}

/// Stable 64-bit FNV-1a hash of `input`, rendered as 16 lowercase hex digits.
///
/// This is the `<repo-hash>` segment of a worktree path. It must be a **fixed**
/// algorithm: `std::collections::hash_map::DefaultHasher` (SipHash) is
/// explicitly documented as *not* guaranteed stable across Rust versions or
/// builds, so the same repo path could hash to different directories after a
/// toolchain bump — orphaning every persisted worktree. FNV-1a is a tiny,
/// dependency-free, fully specified algorithm, so a given path always maps to
/// the same directory.
pub(super) fn stable_repo_hash(input: &str) -> String {
    // FNV-1a 64-bit constants (offset basis + prime).
    const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut hash = FNV_OFFSET;
    for &byte in input.as_bytes() {
        hash ^= byte as u64;
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    format!("{hash:016x}")
}

/// The two load-bearing path segments of the worktree layout: the stable
/// repo-path hash and the sanitized branch name. Both the local
/// ([`worktree_subpath`]) and remote ([`worktree_subpath_posix`]) variants
/// derive their `<repo-hash>/<sanitized-branch>` tail from this, so the
/// hash+sanitize logic lives in one place.
pub(super) fn worktree_segments(repo_path: &Path, branch: &str) -> (String, String) {
    let repo_hash = stable_repo_hash(&repo_path.display().to_string());
    let sanitized = branch.replace('/', "-");
    (repo_hash, sanitized)
}

/// The deterministic `<base>/<repo-hash>/<sanitized-branch>` worktree layout,
/// shared by local ([`worktree_path`]) and remote ([`worktree_path_for`])
/// resolution so both produce identical sub-paths under their own base.
pub(super) fn worktree_subpath(base: PathBuf, repo_path: &Path, branch: &str) -> PathBuf {
    let (repo_hash, sanitized) = worktree_segments(repo_path, branch);
    base.join(repo_hash).join(sanitized)
}

/// The same `<base>/<repo-hash>/<sanitized-branch>` layout as [`worktree_subpath`],
/// but rendered as a POSIX (`/`-joined) string for a **remote** host. This is
/// separate from the `PathBuf` form because on Windows `PathBuf::join` inserts
/// `\`, which the remote login shell would not accept.
pub(super) fn worktree_subpath_posix(base: &str, repo_path: &Path, branch: &str) -> String {
    let (repo_hash, sanitized) = worktree_segments(repo_path, branch);
    format!("{}/{repo_hash}/{sanitized}", base.trim_end_matches('/'))
}

/// Result of attempting to sync a worktree with its base ref.
#[derive(Debug)]
pub enum SyncResult {
    /// Rebase succeeded (includes already-up-to-date).
    Synced,
    /// Rebase failed due to conflicts (aborted, stash restored).
    Conflict(String),
    /// Unexpected failure.
    Error(String),
}

/// Stash uncommitted changes. Returns `true` if anything was stashed.
pub(super) fn git_stash(host: Option<&HostDef>, worktree_path: &Path) -> Result<bool> {
    let output = git_command(host, worktree_path, &["stash"])
        .output()
        .context("failed to run git stash")?;

    if !output.status.success() {
        let stderr = reportable_stderr(&output.stderr);
        anyhow::bail!("git stash failed: {stderr}");
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    Ok(!stdout.contains("No local changes to save"))
}

/// Pop the most recent stash entry.
pub(super) fn git_stash_pop(host: Option<&HostDef>, worktree_path: &Path) -> Result<()> {
    let output = git_command(host, worktree_path, &["stash", "pop"])
        .output()
        .context("failed to run git stash pop")?;

    if !output.status.success() {
        let stderr = reportable_stderr(&output.stderr);
        anyhow::bail!("git stash pop failed: {stderr}");
    }

    Ok(())
}

/// Rebase the current branch onto `base_ref`. Returns `Ok(())` on success,
/// or an error if there are conflicts (rebase is aborted before returning).
pub(super) fn git_rebase_onto(
    host: Option<&HostDef>,
    worktree_path: &Path,
    base_ref: &str,
) -> Result<()> {
    let output = git_command(host, worktree_path, &["rebase", base_ref])
        .output()
        .context("failed to run git rebase")?;

    if !output.status.success() {
        let _ = git_command(host, worktree_path, &["rebase", "--abort"]).output();

        let stderr = reportable_stderr(&output.stderr);
        anyhow::bail!("rebase conflict: {stderr}");
    }

    Ok(())
}

/// Fetch from origin.
pub fn git_fetch(worktree_path: &Path) -> Result<()> {
    git_fetch_on(None, worktree_path)
}

/// [`git_fetch`], optionally on a remote `host`.
pub fn git_fetch_on(host: Option<&HostDef>, worktree_path: &Path) -> Result<()> {
    let output = git_command(host, worktree_path, &["fetch", "origin"])
        .output()
        .context("failed to run git fetch")?;

    if !output.status.success() {
        let stderr = reportable_stderr(&output.stderr);
        anyhow::bail!("git fetch failed: {stderr}");
    }

    Ok(())
}

/// Age threshold for mtime-based stale lock removal.
pub(super) const STALE_LOCK_AGE: Duration = Duration::from_secs(60);

/// Remove a stale `index.lock` if we can confirm no live process holds it.
///
/// On Linux: reads the PID from the lock file content (if present) and checks `/proc/{pid}`.
/// Fallback (all platforms): removes if the lock file's mtime exceeds [`STALE_LOCK_AGE`].
pub(super) fn cleanup_stale_index_lock(worktree_path: &Path) {
    let Some(git_dir) = git_common_dir(worktree_path) else {
        return;
    };
    let lock_path = git_dir.join("index.lock");
    if !lock_path.exists() {
        return;
    }

    #[cfg(target_os = "linux")]
    if try_remove_by_pid(&lock_path) {
        return;
    }

    try_remove_by_age(&lock_path);
}

/// Attempt to remove a lock file by checking if the owning PID is still alive.
///
/// Returns `true` if the PID was parseable (regardless of removal outcome),
/// meaning the caller should not fall through to the mtime-based check.
#[cfg(target_os = "linux")]
pub(super) fn try_remove_by_pid(lock_path: &Path) -> bool {
    let Ok(content) = std::fs::read_to_string(lock_path) else {
        return false;
    };
    let Some(pid_str) = content.split_whitespace().next() else {
        return false;
    };
    let Ok(pid) = pid_str.parse::<u32>() else {
        return false;
    };

    if Path::new(&format!("/proc/{pid}")).exists() {
        return true;
    }

    if std::fs::remove_file(lock_path).is_ok() {
        warn!(
            "Removed stale index.lock (dead PID {pid}) at {}",
            lock_path.display()
        );
    }
    true
}

/// Remove a lock file if its mtime exceeds [`STALE_LOCK_AGE`].
pub(super) fn try_remove_by_age(lock_path: &Path) {
    let age = std::fs::metadata(lock_path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.elapsed().ok());

    let Some(age) = age else { return };

    if age > STALE_LOCK_AGE && std::fs::remove_file(lock_path).is_ok() {
        warn!(
            "Removed stale index.lock (age {:?}) at {}",
            age,
            lock_path.display()
        );
    }
}

/// Per-attempt delays for `stash_with_retry`. The first entry (zero) is the
/// initial attempt; subsequent entries are the backoff delays before each retry.
pub(super) const STASH_ATTEMPT_DELAYS: &[Duration] = &[
    Duration::ZERO,
    Duration::from_millis(100),
    Duration::from_millis(500),
    Duration::from_secs(1),
];

/// Run `git stash` with retries on transient index-lock errors.
///
/// Returns `Ok(true)` if changes were stashed, `Ok(false)` if nothing to stash.
pub(super) fn stash_with_retry(host: Option<&HostDef>, worktree_path: &Path) -> Result<bool> {
    let max_retries = STASH_ATTEMPT_DELAYS.len() - 1;
    let mut last_err = String::new();

    for (attempt, delay) in STASH_ATTEMPT_DELAYS.iter().enumerate() {
        if attempt > 0 {
            warn!(
                "Retrying git stash (retry {}/{}) in {}",
                attempt,
                max_retries,
                worktree_path.display()
            );
            std::thread::sleep(*delay);
            // The stale-lock sweep touches the local filesystem (/proc, mtime),
            // so it only applies to a local worktree — a remote path can't be
            // stat'd here.
            if host.is_none() {
                cleanup_stale_index_lock(worktree_path);
            }
        }
        match git_stash(host, worktree_path) {
            Ok(stashed) => return Ok(stashed),
            Err(e) => {
                let msg = format!("{e:#}");
                if !is_transient_error(&msg) {
                    anyhow::bail!("{msg}");
                }
                last_err = msg;
            }
        }
    }

    anyhow::bail!("transient error persisted after retries: {last_err}")
}

/// Resolve the base ref a worktree should be compared/rebased against.
///
/// Prefers the branch's configured upstream (`@{upstream}`), then the remote's
/// advertised default branch (`origin/HEAD`, e.g. `origin/main` or
/// `origin/master` — whatever the repo actually uses), then the conventional
/// `origin/main` / `origin/master`. Returns `None` when none resolve, so the
/// caller can surface a clear error instead of blindly rebasing onto a
/// possibly-missing `origin/main`. Shared by [`sync_worktree`] (the rebase
/// target) and [`ahead_behind`] (the comparison base) so the "behind" count is
/// always measured against the ref sync would rebase onto.
pub(super) fn resolve_base_ref(host: Option<&HostDef>, worktree_path: &Path) -> Option<String> {
    // A branch with an explicit upstream rebases onto exactly that.
    if run_git_capture_on(
        host,
        &["rev-parse", "--verify", "--quiet", "@{upstream}"],
        worktree_path,
    )
    .is_some()
    {
        return Some("@{upstream}".to_string());
    }

    // The remote's advertised HEAD (origin/HEAD → e.g. "origin/main").
    if let Some(out) = run_git_capture_on(
        host,
        &["symbolic-ref", "--short", "refs/remotes/origin/HEAD"],
        worktree_path,
    ) {
        let advertised = out.trim();
        if !advertised.is_empty() {
            return Some(advertised.to_string());
        }
    }

    // Conventional fallbacks.
    ["origin/main", "origin/master"]
        .into_iter()
        .find(|r| {
            run_git_capture_on(
                host,
                &["rev-parse", "--verify", "--quiet", r],
                worktree_path,
            )
            .is_some()
        })
        .map(str::to_string)
}

/// High-level sync: stash, fetch, rebase onto the base ref, pop stash.
///
/// `base_ref` pins the ref to rebase onto; when `None` it is derived from the
/// worktree via `resolve_base_ref` (upstream → `origin/HEAD` →
/// `origin/main` → `origin/master`) rather than hardcoding `origin/main`.
/// On conflict the rebase is aborted and any stash is restored.
/// Retries `git stash` on transient index-lock errors.
pub fn sync_worktree(worktree_path: &Path, base_ref: Option<&str>) -> SyncResult {
    sync_worktree_on(None, worktree_path, base_ref)
}

/// [`sync_worktree`], optionally on a remote `host`. Every git subcommand runs
/// through the host launcher (`ssh …` / `wsl.exe …`), so `worktree_path` is
/// interpreted on the host — a remote worktree path never touches the local
/// filesystem (which was the "no such file or directory" bug).
pub fn sync_worktree_on(
    host: Option<&HostDef>,
    worktree_path: &Path,
    base_ref: Option<&str>,
) -> SyncResult {
    // Local-only stale-lock sweep; a remote worktree can't be stat'd here.
    if host.is_none() {
        cleanup_stale_index_lock(worktree_path);
    }

    let stashed = match stash_with_retry(host, worktree_path) {
        Ok(s) => s,
        Err(e) => return SyncResult::Error(format!("stash: {e:#}")),
    };

    let restore_stash = || {
        if stashed {
            let _ = git_stash_pop(host, worktree_path);
        }
    };

    if let Err(e) = git_fetch_on(host, worktree_path) {
        restore_stash();
        return SyncResult::Error(format!("fetch: {e:#}"));
    }

    // Resolve the rebase target after the fetch so derived refs (origin/HEAD,
    // origin/main, …) reflect the just-fetched remote state.
    let base_ref = match base_ref
        .map(str::to_string)
        .or_else(|| resolve_base_ref(host, worktree_path))
    {
        Some(r) => r,
        None => {
            restore_stash();
            return SyncResult::Error(
                "could not resolve a base ref to sync onto (no upstream, \
                 origin/HEAD, origin/main, or origin/master)"
                    .to_string(),
            );
        }
    };

    if let Err(e) = git_rebase_onto(host, worktree_path, &base_ref) {
        restore_stash();
        return SyncResult::Conflict(format!("{e:#}"));
    }

    if stashed {
        if let Err(e) = git_stash_pop(host, worktree_path) {
            return SyncResult::Error(format!("stash pop: {e:#}"));
        }
    }

    SyncResult::Synced
}

/// Check whether a git error message indicates a transient index-lock failure.
pub(super) fn is_transient_error(msg: &str) -> bool {
    const PATTERNS: &[&str] = &[
        "could not write index",
        "Unable to write new index file",
        "index.lock': File exists",
        "Another git process seems to be running",
    ];
    PATTERNS.iter().any(|p| msg.contains(p))
}

/// Find the shared git directory for a worktree (handles linked worktrees).
pub(super) fn git_common_dir(worktree_path: &Path) -> Option<PathBuf> {
    let output = git_program()
        .args(["rev-parse", "--git-common-dir"])
        .current_dir(worktree_path)
        .stderr(Stdio::null())
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let dir = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let path = Path::new(&dir);
    // git may return a relative path; resolve it against the worktree
    if path.is_absolute() {
        Some(PathBuf::from(dir))
    } else {
        Some(worktree_path.join(dir))
    }
}
