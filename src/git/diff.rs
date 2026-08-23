//! Diffs, branches, commits, and a worktree's stats.
//!
//! The working-tree diff folds in **untracked files** (`git diff --no-index --
//! /dev/null <path>`, capped at [`UNTRACKED_FILE_CAP`]): `git diff HEAD` cannot
//! show a file git has never been told about, which made the default review
//! target report "no changes" after an agent wrote new ones. There is
//! deliberately no body-only `git diff HEAD` helper — having one is how that
//! omission happened in the first place (ADR-P6).

use super::*;

/// List local branch names for a repo.
pub fn list_branches(repo_path: &Path) -> Result<Vec<String>> {
    list_branches_on(None, repo_path)
}

/// [`list_branches`], optionally on a remote `host`.
pub fn list_branches_on(host: Option<&HostDef>, repo_path: &Path) -> Result<Vec<String>> {
    let output = git_command(host, repo_path, &["branch", "--format=%(refname:short)"])
        .output()
        .context("failed to run git branch")?;

    if !output.status.success() {
        let stderr = reportable_stderr(&output.stderr);
        anyhow::bail!("git branch failed: {stderr}");
    }

    let branches = String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .collect();

    Ok(branches)
}

/// Raw unified `git diff <base>..HEAD` output for a worktree, for the native
/// code-review view, optionally on a remote `host` (via `ssh <dest> git …`).
/// Returns `None` on failure (not a git dir, bad base, …); the caller falls
/// back to a narrower range or surfaces a status.
///
/// `--no-color` keeps the output parseable; the result is fed to
/// [`crate::session::parse_unified_diff`].
pub fn diff_against_on(host: Option<&HostDef>, worktree: &Path, base: &str) -> Option<String> {
    let range = format!("{base}..HEAD");
    run_diff(host, worktree, &["diff", "--no-color", &range])
}

// The "working changes" target is [`working_diff_on`], below. There is
// deliberately no diff-body-only counterpart to [`diff_against_on`] for it: one
// would be `git diff HEAD`, which silently omits untracked files, and having it
// available is how that omission happened.

/// The **complete** list of changed files as `--numstat -M -z`, with exact counts.
///
/// Separate from the diff text because the two have different bounds: a diff body is
/// capped (see `kernel::diff::MAX_DIFF_BYTES`) and this is not. Deriving the file
/// list from the capped body made it silently short — on this repository's own diff,
/// 310 files of 433 — so a reviewer navigating it could not tell it ended early, and
/// the totals were a fraction of the truth. Twelve kilobytes for four hundred files
/// is not worth capping.
///
/// `-z` rather than the human format: a rename in `--numstat` otherwise arrives as
/// `old => new` (or a brace form) and has to be un-guessed. NUL-separated, a rename
/// is an empty path field followed by two more records.
pub fn diff_numstat_on(
    host: Option<&HostDef>,
    worktree: &Path,
    base: Option<&str>,
) -> Option<String> {
    let range = base.map_or_else(|| "HEAD".to_string(), |base| format!("{base}..HEAD"));
    run_diff(
        host,
        worktree,
        &["diff", "--no-color", "--numstat", "-M", "-z", &range],
    )
}

/// Each changed file's status (`M`/`A`/`D`/`R…`) as `--name-status -M -z`.
///
/// The companion to [`diff_numstat_on`]: `--numstat` carries the counts and cannot
/// tell a deletion from a rewrite. Cheap — a fraction of the cost of the diff itself.
pub fn diff_name_status_on(
    host: Option<&HostDef>,
    worktree: &Path,
    base: Option<&str>,
) -> Option<String> {
    let range = base.map_or_else(|| "HEAD".to_string(), |base| format!("{base}..HEAD"));
    run_diff(
        host,
        worktree,
        &["diff", "--no-color", "--name-status", "-M", "-z", &range],
    )
}

/// Raw unified diff of a single commit (`git show`), for the review view's
/// per-commit target. `--format=` suppresses the log message, leaving the patch.
pub fn show_commit_on(host: Option<&HostDef>, worktree: &Path, sha: &str) -> Option<String> {
    run_diff(host, worktree, &["show", "--no-color", "--format=", sha])
}

/// List the commits in `<base>..HEAD` as `(short-sha, subject)`, newest first —
/// the choices for the review view's per-commit target picker.
pub fn list_commits_on(
    host: Option<&HostDef>,
    worktree: &Path,
    base: &str,
) -> Vec<(String, String)> {
    let range = format!("{base}..HEAD");
    let Some(out) = run_diff(
        host,
        worktree,
        &["log", "--no-color", "--format=%h%x09%s", &range],
    ) else {
        return Vec::new();
    };
    out.lines()
        .filter_map(|l| l.split_once('\t'))
        .map(|(sha, subj)| (sha.to_string(), subj.to_string()))
        .collect()
}

/// Run a `git` command and capture stdout, logging + returning `None` on
/// failure. Shared by the review-view diff/log/listing helpers.
///
/// `core.quotepath=false` keeps non-ASCII paths verbatim (git otherwise
/// C-quotes them, e.g. `"caf\303\251.rs"`), so the parser keys comments/marks on
/// the real UTF-8 path.
pub(super) fn run_diff(host: Option<&HostDef>, worktree: &Path, args: &[&str]) -> Option<String> {
    let mut full = vec!["-c", "core.quotepath=false"];
    full.extend_from_slice(args);
    let output = git_command(host, worktree, &full).output().ok()?;
    if !output.status.success() {
        let stderr = reportable_stderr(&output.stderr);
        warn!("git {args:?} failed: {stderr}");
        return None;
    }
    Some(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// How many untracked files a working-tree diff represents.
///
/// Each costs a process, so an un-ignored build directory in a scratch worktree
/// would otherwise turn one review into thousands of `git` invocations. The
/// number *found* is reported alongside, so a list that stops early says so
/// instead of reading as complete.
pub const UNTRACKED_FILE_CAP: usize = 200;

/// A worktree's uncommitted state, in the three shapes `kernel::diff` needs.
///
/// One type rather than three calls because untracked files have to be folded
/// into all three, and gathering them once is what stops the body, the counts
/// and the statuses disagreeing about which files they covered.
pub struct WorkingDiff {
    /// Unified diff text, tracked changes followed by one patch per untracked
    /// file.
    pub body: String,
    /// `--numstat -M -z` records, likewise extended.
    pub numstat_z: String,
    /// `--name-status -M -z` records, likewise extended.
    pub name_status_z: String,
    /// Untracked files found, before [`UNTRACKED_FILE_CAP`].
    pub untracked_total: usize,
    /// How many of them the three fields above actually describe.
    pub untracked_shown: usize,
}

/// The uncommitted diff of a worktree — staged, unstaged **and untracked**.
///
/// `git diff HEAD` cannot show a file git has never been told about, and a new
/// file is the most common thing a coding agent produces: a session with no base
/// branch, which is exactly the scratch worktree someone watches an agent work
/// in, reported "no changes" after three files had been written. So each
/// untracked file is diffed against nothing
/// (`git diff --no-index -- /dev/null <path>`), which emits an ordinary
/// `new file mode` patch and needs nothing downstream to change.
///
/// The obvious one-process alternative — a temporary index (`GIT_INDEX_FILE`,
/// `git add -A`, `git diff --cached`) — is refused: it writes loose objects into
/// the repository being reviewed, and for a pane refreshing every few seconds
/// against a worktree an agent is editing, that is a reader mutating what it
/// reads. Not destructive (blobs are content-addressed and gc prunes them), just
/// not a reviewer's business.
pub fn working_diff_on(host: Option<&HostDef>, worktree: &Path) -> Option<WorkingDiff> {
    let mut body = run_diff(host, worktree, &["diff", "--no-color", "HEAD"])?;
    let mut numstat_z = diff_numstat_on(host, worktree, None)?;
    let mut name_status_z = diff_name_status_on(host, worktree, None)?;

    let untracked = untracked_files_on(host, worktree)?;
    let mut shown = 0;
    for path in untracked.iter().take(UNTRACKED_FILE_CAP) {
        // Counts and patch from one invocation — `--numstat --patch` prints the
        // stat records first, split back apart below — where asking for each
        // shape separately cost two processes per untracked file.
        let Some(combined) = untracked_diff_on(
            host,
            worktree,
            path,
            &["--no-color", "--numstat", "-z", "--patch"],
        ) else {
            continue;
        };
        let (counts, patch) = split_untracked_diff(&combined);
        // An empty patch means the file went away between the listing and here —
        // an agent deleting its own scratch file, which costs nothing to skip.
        if patch.is_empty() {
            continue;
        }
        if !body.is_empty() && !body.ends_with('\n') {
            body.push('\n');
        }
        body.push_str(patch);
        numstat_z.push_str(counts);
        // `A\0<path>\0` is byte-identical to what `--name-status -z` prints for
        // one of these — an untracked file is always an addition — so it is
        // built rather than asked for, not run as a third process.
        name_status_z.push_str("A\0");
        name_status_z.push_str(path);
        name_status_z.push('\0');
        shown += 1;
    }

    Some(WorkingDiff {
        body,
        numstat_z,
        name_status_z,
        untracked_total: untracked.len(),
        untracked_shown: shown,
    })
}

/// The worktree's untracked, non-ignored files. `--exclude-standard` is what
/// keeps `.gitignore`d build output out; `-z` is what keeps a path with a space
/// or a newline in it whole.
pub(super) fn untracked_files_on(host: Option<&HostDef>, worktree: &Path) -> Option<Vec<String>> {
    let out = run_diff(
        host,
        worktree,
        &["ls-files", "--others", "--exclude-standard", "-z"],
    )?;
    Some(
        out.split('\0')
            .filter(|path| !path.is_empty())
            .map(str::to_string)
            .collect(),
    )
}

/// One untracked file's diff against nothing, with `extra` selecting the shape
/// ([`working_diff_on`] asks for `--numstat` and `--patch` together and splits
/// the result with [`split_untracked_diff`]).
///
/// `--no-index` exits **1** whenever the two paths differ, which for a file
/// against `/dev/null` is always — so unlike every other git call here a
/// non-zero status is the answer, not a failure. A file that vanished since the
/// listing also exits 1, with empty output, which is why that race needs no
/// handling beyond ignoring an empty patch. Anything above 1 is a real error
/// (128 = not a repository) and drops the file.
///
/// `/dev/null` is understood by Git for Windows too (verified against a psmux
/// host), so this needs no platform branch.
pub(super) fn untracked_diff_on(
    host: Option<&HostDef>,
    worktree: &Path,
    path: &str,
    extra: &[&str],
) -> Option<String> {
    let mut args = vec!["-c", "core.quotepath=false", "diff", "--no-index"];
    args.extend_from_slice(extra);
    // `--` so a path that begins with a dash is a path, not a flag.
    args.extend_from_slice(&["--", "/dev/null", path]);
    let output = git_command(host, worktree, &args).output().ok()?;
    match output.status.code() {
        Some(0 | 1) => Some(String::from_utf8_lossy(&output.stdout).into_owned()),
        _ => {
            let stderr = reportable_stderr(&output.stderr);
            warn!("git diff --no-index for {path} failed: {stderr}");
            None
        }
    }
}

/// Split a combined `--numstat -z --patch` output into its numstat records and
/// its patch. The stat records come first (NUL-terminated under `-z`), the
/// patch begins at the first `diff --git` header — recognised only at the start
/// or right after a terminator, so a path containing the literal text cannot
/// split early.
pub(super) fn split_untracked_diff(combined: &str) -> (&str, &str) {
    let mut from = 0;
    while let Some(rel) = combined[from..].find("diff --git ") {
        let at = from + rel;
        if at == 0 || matches!(combined.as_bytes()[at - 1], b'\0' | b'\n') {
            return (&combined[..at], &combined[at..]);
        }
        from = at + 1;
    }
    (combined, "")
}

/// Sum `git diff --numstat` output into `(files_changed, insertions, deletions)`.
/// Binary files (`-\t-\tpath`) count toward `files_changed` with zero lines.
pub(super) fn parse_numstat(out: &str) -> (usize, usize, usize) {
    let (mut files, mut ins, mut dels) = (0usize, 0usize, 0usize);
    for line in out.lines() {
        let mut cols = line.split('\t');
        let added = cols.next();
        let deleted = cols.next();
        let path = cols.next();
        if path.is_none() {
            continue;
        }
        files += 1;
        if let Some(n) = added.and_then(|s| s.parse::<usize>().ok()) {
            ins += n;
        }
        if let Some(n) = deleted.and_then(|s| s.parse::<usize>().ok()) {
            dels += n;
        }
    }
    (files, ins, dels)
}

/// Commits the worktree's HEAD is `(ahead, behind)` relative to its base ref,
/// resolved by `resolve_base_ref` (upstream → `origin/HEAD` → `origin/main` →
/// `origin/master`) — the same chain [`sync_worktree`] rebases onto, so the
/// "behind" count is measured against the ref sync would use. Returns `(0, 0)`
/// when no base can be resolved.
pub fn ahead_behind(cwd: &Path) -> (usize, usize) {
    let Some(base) = resolve_base_ref(None, cwd) else {
        return (0, 0);
    };
    // `--left-right --count <base>...HEAD` → "<behind>\t<ahead>".
    let range = format!("{base}...HEAD");
    let Some(out) = run_git_capture(&["rev-list", "--left-right", "--count", &range], cwd) else {
        return (0, 0);
    };
    let mut parts = out.split_whitespace();
    let behind = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0);
    let ahead = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0);
    (ahead, behind)
}

/// What one `git status --porcelain=v2 --branch` run carries for
/// [`worktree_stats`]: dirtiness, the untracked count, and — when the branch
/// has a usable upstream — the ahead/behind counts, sparing the
/// `resolve_base_ref` probe chain entirely.
#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct StatusV2 {
    /// Any changed, unmerged or untracked entry — the same "any output" rule
    /// the v1 porcelain gave, minus v2's `#` headers.
    pub dirty: bool,
    /// `?` entries: files a worktree removal would lose but that `diff HEAD`
    /// never reports.
    pub untracked: usize,
    /// `(ahead, behind)` from the `# branch.ab` header. Absent when no upstream
    /// is configured (or the upstream ref is gone), in which case the caller
    /// falls back to [`ahead_behind`]'s base-ref resolution.
    pub ahead_behind: Option<(usize, usize)>,
}

/// Parse `git status --porcelain=v2 --branch` output. Pure, so the header and
/// record handling is testable without a repository.
pub(super) fn parse_status_v2(out: &str) -> StatusV2 {
    let mut status = StatusV2::default();
    for line in out.lines() {
        if let Some(ab) = line.strip_prefix("# branch.ab ") {
            // "+<ahead> -<behind>".
            let mut parts = ab.split_whitespace();
            let ahead = parts
                .next()
                .and_then(|s| s.strip_prefix('+'))
                .and_then(|s| s.parse().ok());
            let behind = parts
                .next()
                .and_then(|s| s.strip_prefix('-'))
                .and_then(|s| s.parse().ok());
            if let (Some(ahead), Some(behind)) = (ahead, behind) {
                status.ahead_behind = Some((ahead, behind));
            }
        } else if line.starts_with('?') {
            status.untracked += 1;
            status.dirty = true;
        } else if line.starts_with('1') || line.starts_with('2') || line.starts_with('u') {
            status.dirty = true;
        }
    }
    status
}

/// Compute combined git stats (uncommitted diff + dirty + ahead/behind) for a
/// worktree. Returns `None` when the path is not a usable git worktree.
pub fn worktree_stats(cwd: &Path) -> Option<crate::session::GitStats> {
    // One status call carries dirty, the untracked count AND — via the
    // `# branch.ab` header — ahead/behind, and doubles as the "is this a work
    // tree" probe: outside one it fails, exactly as a `rev-parse` would.
    let status = run_git_capture(&["status", "--porcelain=v2", "--branch"], cwd)?;
    let status = parse_status_v2(&status);
    let numstat = run_git_capture(&["diff", "--numstat", "HEAD"], cwd).unwrap_or_default();
    let (files_changed, insertions, deletions) = parse_numstat(&numstat);
    // `branch.ab` counts against the configured upstream — the same ref the
    // probe chain would pick first. Only a branch without one (no upstream, or
    // its ref gone) pays for [`ahead_behind`]'s resolution (`origin/HEAD` →
    // `origin/main` → `origin/master`), preserving the old answer there.
    let (ahead, behind) = status.ahead_behind.unwrap_or_else(|| ahead_behind(cwd));
    Some(crate::session::GitStats {
        files_changed,
        insertions,
        deletions,
        untracked: status.untracked,
        dirty: status.dirty,
        ahead,
        behind,
    })
}
