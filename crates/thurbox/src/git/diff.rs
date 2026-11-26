use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{LazyLock, Mutex};

use tracing::{debug, warn};

use super::{git_command, reportable_stderr_line, run_git_capture, run_git_capture_env};

pub fn diff_against(worktree: &Path, base: &str) -> Option<String> {
    let range = format!("{base}..HEAD");
    run_diff(worktree, &["diff", "--no-color", &range])
}

// `git diff HEAD` omits untracked files, so working changes go through `working_diff` alone.

pub fn diff_numstat(worktree: &Path, base: Option<&str>) -> Option<String> {
    let range = base.map_or_else(|| "HEAD".to_string(), |base| format!("{base}..HEAD"));
    run_diff(
        worktree,
        &["diff", "--no-color", "--numstat", "-M", "-z", &range],
    )
}

pub fn diff_name_status(worktree: &Path, base: Option<&str>) -> Option<String> {
    let range = base.map_or_else(|| "HEAD".to_string(), |base| format!("{base}..HEAD"));
    run_diff(
        worktree,
        &["diff", "--no-color", "--name-status", "-M", "-z", &range],
    )
}

pub(super) fn run_diff(worktree: &Path, args: &[&str]) -> Option<String> {
    // git quotes non-ASCII paths unless `core.quotepath=false`.
    let mut full = vec!["-c", "core.quotepath=false"];
    full.extend_from_slice(args);
    let output = git_command(worktree, &full).output().ok()?;
    if !output.status.success() {
        let stderr = reportable_stderr_line(&output.stderr);
        warn!("git {args:?} failed: {stderr}");
        return None;
    }
    Some(String::from_utf8_lossy(&output.stdout).into_owned())
}

pub const UNTRACKED_FILE_CAP: usize = 200;

pub struct WorkingDiff {
    pub body: String,
    pub numstat_z: String,
    pub name_status_z: String,
    pub untracked_total: usize,
    pub untracked_shown: usize,
}

pub fn working_diff(worktree: &Path) -> Option<WorkingDiff> {
    if worktree.is_dir() && work_tree(worktree) == WorkTree::Outside {
        note_outside_work_tree(worktree);
        return Some(WorkingDiff::empty());
    }
    let mut body = run_diff(worktree, &["diff", "--no-color", "HEAD"])?;
    let mut numstat_z = diff_numstat(worktree, None)?;
    let mut name_status_z = diff_name_status(worktree, None)?;

    let untracked = untracked_files(worktree)?;
    let mut shown = 0;
    for path in untracked.iter().take(UNTRACKED_FILE_CAP) {
        let Some(combined) = untracked_diff(
            worktree,
            path,
            &["--no-color", "--numstat", "-z", "--patch"],
        ) else {
            continue;
        };
        let (counts, patch) = split_untracked_diff(&combined);
        if patch.is_empty() {
            continue;
        }
        if !body.is_empty() && !body.ends_with('\n') {
            body.push('\n');
        }
        body.push_str(patch);
        numstat_z.push_str(counts);
        // `--name-status -z` prints exactly `A\0<path>\0` for an untracked file.
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

impl WorkingDiff {
    fn empty() -> Self {
        Self {
            body: String::new(),
            numstat_z: String::new(),
            name_status_z: String::new(),
            untracked_total: 0,
            untracked_shown: 0,
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum WorkTree {
    Inside,
    Outside,
    Unknown,
}

pub(super) fn work_tree_answer(output: &std::process::Output) -> WorkTree {
    const NOT_A_REPOSITORY: &str = "fatal: not a git repository";
    match output.status.code() {
        Some(0) if output.stdout.trim_ascii() == b"true" => WorkTree::Inside,
        Some(0) if output.stdout.trim_ascii() == b"false" => WorkTree::Outside,
        Some(128) if reportable_stderr_line(&output.stderr).starts_with(NOT_A_REPOSITORY) => {
            WorkTree::Outside
        }
        _ => WorkTree::Unknown,
    }
}

pub(super) fn work_tree_probe(dir: &Path) -> std::process::Command {
    let mut probe = git_command(dir, &["rev-parse", "--is-inside-work-tree"]);
    probe.env("LC_ALL", "C");
    probe
}

fn work_tree(dir: &Path) -> WorkTree {
    work_tree_probe(dir)
        .output()
        .map_or(WorkTree::Unknown, |output| work_tree_answer(&output))
}

static NOTED_OUTSIDE_WORK_TREE: LazyLock<Mutex<HashSet<PathBuf>>> = LazyLock::new(Mutex::default);

fn note_outside_work_tree(dir: &Path) {
    let first_time = NOTED_OUTSIDE_WORK_TREE
        .lock()
        .is_ok_and(|mut noted| noted.insert(dir.to_path_buf()));
    if first_time {
        debug!("{} is not a git work tree; it has no diff", dir.display());
    }
}

pub(super) fn untracked_files(worktree: &Path) -> Option<Vec<String>> {
    let out = run_diff(
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

pub(super) fn untracked_diff(worktree: &Path, path: &str, extra: &[&str]) -> Option<String> {
    let mut args = vec!["-c", "core.quotepath=false", "diff", "--no-index"];
    args.extend_from_slice(extra);
    // `--` so a path that begins with a dash is a path, not a flag.
    args.extend_from_slice(&["--", "/dev/null", path]);
    let output = git_command(worktree, &args).output().ok()?;
    match output.status.code() {
        Some(0 | 1) => Some(String::from_utf8_lossy(&output.stdout).into_owned()),
        _ => {
            let stderr = reportable_stderr_line(&output.stderr);
            warn!("git diff --no-index for {path} failed: {stderr}");
            None
        }
    }
}

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

fn remote_default_ref(cwd: &Path) -> Option<String> {
    if let Some(out) = run_git_capture(
        &["symbolic-ref", "--short", "refs/remotes/origin/HEAD"],
        cwd,
    ) {
        let advertised = out.trim();
        if !advertised.is_empty() {
            return Some(advertised.to_string());
        }
    }
    ["origin/main", "origin/master"]
        .into_iter()
        .find(|r| run_git_capture(&["rev-parse", "--verify", "--quiet", r], cwd).is_some())
        .map(str::to_string)
}

pub fn merged_into_default(cwd: &Path) -> Option<bool> {
    let default = remote_default_ref(cwd)?;
    if reachable_from(cwd, &default)? {
        return Some(true);
    }
    if same_tree_as(cwd, &default)? {
        return Some(true);
    }
    let base = run_git_capture(&["merge-base", &default, "HEAD"], cwd)?;
    let base = base.trim();
    if every_commit_upstream(cwd, &default, base)? {
        return Some(true);
    }
    squashed_upstream(cwd, &default, base)
}

fn git_predicate(cwd: &Path, args: &[&str]) -> Option<bool> {
    let status = git_command(cwd, args)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .ok()?;
    match status.code() {
        Some(0) => Some(true),
        Some(1) => Some(false),
        _ => None,
    }
}

fn reachable_from(cwd: &Path, default: &str) -> Option<bool> {
    git_predicate(cwd, &["merge-base", "--is-ancestor", "HEAD", default])
}

fn same_tree_as(cwd: &Path, default: &str) -> Option<bool> {
    git_predicate(cwd, &["diff", "--quiet", default, "HEAD"])
}

// `git cherry` prefixes `-` to a commit whose patch is already upstream.
fn every_commit_upstream(cwd: &Path, default: &str, base: &str) -> Option<bool> {
    let cherry = run_git_capture(&["cherry", default, "HEAD", base], cwd)?;
    let mut lines = cherry.lines().filter(|l| !l.trim().is_empty()).peekable();
    Some(lines.peek().is_some() && lines.all(|l| l.trim_start().starts_with('-')))
}

const PROBE_IDENT: [(&str, &str); 6] = [
    ("GIT_AUTHOR_NAME", "thurbox"),
    ("GIT_AUTHOR_EMAIL", "thurbox@invalid"),
    ("GIT_AUTHOR_DATE", "@0 +0000"),
    ("GIT_COMMITTER_NAME", "thurbox"),
    ("GIT_COMMITTER_EMAIL", "thurbox@invalid"),
    ("GIT_COMMITTER_DATE", "@0 +0000"),
];

fn squashed_upstream(cwd: &Path, default: &str, base: &str) -> Option<bool> {
    let squashed = run_git_capture_env(
        &["commit-tree", "HEAD^{tree}", "-p", base, "-m", "squash"],
        cwd,
        &PROBE_IDENT,
    )?;
    let cherry = run_git_capture(&["cherry", default, squashed.trim()], cwd)?;
    Some(cherry.trim_start().starts_with('-'))
}

pub fn ahead_behind(cwd: &Path) -> (usize, usize) {
    let Some(base) = resolve_base_ref(cwd) else {
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

#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct StatusV2 {
    pub dirty: bool,
    pub tracked: bool,
    pub untracked: usize,
    pub ahead_behind: Option<(usize, usize)>,
    pub head: Option<String>,
}

pub(super) fn parse_status_v2(out: &str) -> StatusV2 {
    let mut status = StatusV2::default();
    for line in out.lines() {
        if let Some(oid) = line.strip_prefix("# branch.oid ") {
            let oid = oid.trim();
            // git prints `(initial)` for an unborn branch.
            if !oid.is_empty() && oid != "(initial)" {
                status.head = Some(oid.to_string());
            }
        } else if let Some(ab) = line.strip_prefix("# branch.ab ") {
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
            status.tracked = true;
        }
    }
    status
}

#[derive(Debug, Clone, Copy)]
pub struct KnownMerge<'a> {
    pub head: &'a str,
    pub merged: bool,
}

pub fn worktree_stats(
    cwd: &Path,
    known: Option<KnownMerge<'_>>,
) -> Option<crate::session::GitStats> {
    let status = run_git_capture(&["status", "--porcelain=v2", "--branch"], cwd)?;
    let status = parse_status_v2(&status);
    let (files_changed, insertions, deletions) = if status.tracked {
        let numstat = run_git_capture(&["diff", "--numstat", "HEAD"], cwd).unwrap_or_default();
        parse_numstat(&numstat)
    } else {
        (0, 0, 0)
    };
    let (ahead, behind) = status.ahead_behind.unwrap_or_else(|| ahead_behind(cwd));
    let settled = known.filter(|k| Some(k.head) == status.head.as_deref());
    let merged = match settled {
        Some(known) => Some(known.merged),
        None => (ahead > 0).then(|| merged_into_default(cwd)).flatten(),
    };
    Some(crate::session::GitStats {
        files_changed,
        insertions,
        deletions,
        untracked: status.untracked,
        dirty: status.dirty,
        ahead,
        behind,
        merged,
        head: status.head,
    })
}

pub(super) fn resolve_base_ref(worktree_path: &Path) -> Option<String> {
    if run_git_capture(
        &["rev-parse", "--verify", "--quiet", "@{upstream}"],
        worktree_path,
    )
    .is_some()
    {
        return Some("@{upstream}".to_string());
    }

    if let Some(out) = run_git_capture(
        &["symbolic-ref", "--short", "refs/remotes/origin/HEAD"],
        worktree_path,
    ) {
        let advertised = out.trim();
        if !advertised.is_empty() {
            return Some(advertised.to_string());
        }
    }

    ["origin/main", "origin/master"]
        .into_iter()
        .find(|r| {
            run_git_capture(&["rev-parse", "--verify", "--quiet", r], worktree_path).is_some()
        })
        .map(str::to_string)
}
