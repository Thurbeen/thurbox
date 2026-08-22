//! Answering "what is here" — locally and over a transport.
//!
//! Directory listings, path classification and child-repo scans, each shipped as
//! **two scripts emitting one line protocol** (POSIX and PowerShell) so a
//! Windows host answers the same shape a Linux one does. The parsers are
//! separate from the scripts on purpose: they are the half worth unit-testing,
//! and they are what the two script flavours have to agree on.

use super::*;

/// Global cache for repo display names (path → name).
pub(super) static REPO_NAME_CACHE: std::sync::OnceLock<Mutex<HashMap<PathBuf, String>>> =
    std::sync::OnceLock::new();

pub(super) fn repo_name_cache() -> &'static Mutex<HashMap<PathBuf, String>> {
    REPO_NAME_CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Get a short display name for a repo directory.
///
/// Tries to extract the repo name from `git remote get-url origin`
/// (e.g. `github.com/user/thurbox.git` → `"thurbox"`).
/// Falls back to the directory's file name if no remote is found.
/// Results are cached globally.
pub fn repo_display_name(path: &Path) -> Option<String> {
    let cache = repo_name_cache();
    if let Ok(guard) = cache.lock() {
        if let Some(name) = guard.get(path) {
            return Some(name.clone());
        }
    }
    let name = repo_name_from_remote(path).or_else(|| {
        path.file_name()
            .and_then(std::ffi::OsStr::to_str)
            .map(str::to_string)
    })?;
    if let Ok(mut guard) = cache.lock() {
        guard.insert(path.to_path_buf(), name.clone());
    }
    Some(name)
}

/// True if `path` is itself a git repo: it contains a `.git` directory OR a
/// `.git` file (worktree checkouts and submodules use a `.git` file).
pub fn is_git_repo(path: &Path) -> bool {
    let git = path.join(".git");
    git.is_dir() || git.is_file()
}

/// Immediate child directories of `parent` that are git repos, sorted by file
/// name. Hidden (`.`-prefixed) entries are skipped. Returns an empty vec when
/// `parent` can't be read (missing, permission denied, …).
pub fn scan_child_repos(parent: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(parent) else {
        return Vec::new();
    };
    let mut repos: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .filter(|e| e.file_type().map(|ft| ft.is_dir()).unwrap_or(false))
        .filter(|e| {
            !e.file_name()
                .to_str()
                .map(|n| n.starts_with('.'))
                .unwrap_or(true)
        })
        .map(|e| e.path())
        .filter(|p| is_git_repo(p))
        .collect();
    repos.sort();
    repos
}

/// Immediate sub-directories of a path, each flagged as a git repo or not —
/// the repo picker's path-browser payload. `Missing` distinguishes "the dir
/// isn't there" (a user typo, reported inline) from a transport error (`Err`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DirListing {
    /// The requested directory does not exist on the target filesystem.
    Missing,
    /// `(name, is_git)` per immediate sub-directory, sorted by name. Hidden
    /// (`.`-prefixed) entries are included — the picker's filter decides
    /// their visibility (offered only to a `.`-prefix, like the local
    /// completer).
    Entries(Vec<(String, bool)>),
}

/// What a committed repo-picker path turned out to be on the target
/// filesystem. `Dir` (a non-repo directory) is still selectable — it becomes
/// a plain `--add-dir`-style member — but can't take the worktree toggle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathClass {
    Git,
    Dir,
    Missing,
}

/// List the immediate sub-directories of `dir` on `host` (`None` = local),
/// flagging each as a git repo. One round trip on a remote host: a `sh -c`
/// loop using `test -d` per entry (which, unlike an `ls -p` probe, follows a
/// symlink to a directory) and `test -e <e>/.git` (matching
/// [`is_git_repo`]'s `.git`-file worktree handling). A leading `~` expands
/// against the target's home. Blocking — call from a worker thread for a
/// remote host.
pub fn list_dir_entries_on(host: Option<&HostDef>, dir: &str) -> Result<DirListing> {
    let Some(host) = host else {
        return Ok(list_dir_entries_local(&paths::expand_tilde(dir)));
    };
    let dir = expand_remote_tilde(host, dir)?;
    let output = host_probe(
        host,
        &list_dir_entries_script(&dir),
        &list_dir_entries_script_windows(&dir),
    )
    .stdout(Stdio::piped())
    .stderr(Stdio::piped())
    .output()
    .context("failed to list remote directory")?;
    let stdout = remote_output_or_stderr(output, "dir listing")?;
    Ok(parse_dir_listing(&String::from_utf8_lossy(&stdout)))
}

/// The [`list_dir_entries_on`] script for a **native-Windows** host, emitting
/// the identical line protocol to [`list_dir_entries_script`] so the same
/// parser reads both. `-Force` includes hidden entries, matching the POSIX
/// loop's `* .*`; a directory that exists but cannot be enumerated throws
/// (`-ErrorAction Stop`) and exits non-zero, so its stderr surfaces as a real
/// error rather than an empty listing.
pub(super) fn list_dir_entries_script_windows(dir: &str) -> String {
    format!(
        "$d={q}\n\
         if (-not (Test-Path -LiteralPath $d -PathType Container)) {{ \
         Write-Output '!missing'; exit 0 }}\n\
         Get-ChildItem -LiteralPath $d -Force -Directory -ErrorAction Stop | ForEach-Object {{ \
         if (Test-Path -LiteralPath ($_.FullName + '/.git')) \
         {{ Write-Output ('g ' + $_.Name) }} else {{ Write-Output ('d ' + $_.Name) }} }}\n\
         exit 0",
        q = powershell_quote(dir),
    )
}

/// The [`list_dir_entries_on`] shell script (`dir` already tilde-expanded,
/// quoted here). Line protocol: `!missing` alone when the dir is absent; else
/// one line per entry, `g <name>` (git repo) or `d <name>` (plain dir). A
/// `cd` failure on an *existing* dir (permissions) exits non-zero so the
/// stderr surfaces as a real error rather than a bogus "missing". Pure so the
/// quoting is testable without a host.
pub(super) fn list_dir_entries_script(dir: &str) -> String {
    format!(
        "d={q}; [ -d \"$d\" ] || {{ echo '!missing'; exit 0; }}; cd \"$d\" || exit 1; \
         for e in * .*; do [ -d \"$e\" ] || continue; \
         case \"$e\" in .|..) continue;; esac; \
         if [ -e \"$e/.git\" ]; then printf 'g %s\\n' \"$e\"; \
         else printf 'd %s\\n' \"$e\"; fi; done; exit 0",
        q = posix_quote(dir),
    )
}

/// Local branch of [`list_dir_entries_on`] (`dir` already tilde-expanded).
pub(super) fn list_dir_entries_local(dir: &Path) -> DirListing {
    if !dir.is_dir() {
        return DirListing::Missing;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        // Exists but unreadable (permissions): an empty listing keeps the
        // browser usable; the path itself may still be committed.
        return DirListing::Entries(Vec::new());
    };
    let mut names: Vec<(String, bool)> = entries
        .filter_map(Result::ok)
        // `path().is_dir()` follows symlinks, matching the remote `test -d`.
        .filter(|e| e.path().is_dir())
        .filter_map(|e| {
            let name = e.file_name().to_str()?.to_string();
            let is_git = is_git_repo(&e.path());
            Some((name, is_git))
        })
        .collect();
    names.sort();
    DirListing::Entries(names)
}

/// Parse the [`list_dir_entries_on`] line protocol. Unknown lines are skipped
/// (a host's shell may emit noise) rather than failing the whole listing.
pub(super) fn parse_dir_listing(stdout: &str) -> DirListing {
    if stdout.lines().next().map(str::trim) == Some("!missing") {
        return DirListing::Missing;
    }
    let mut entries: Vec<(String, bool)> = stdout
        .lines()
        .filter_map(|line| {
            let (tag, name) = line.split_once(' ')?;
            let is_git = match tag {
                "g" => true,
                "d" => false,
                _ => return None,
            };
            (!name.is_empty()).then(|| (name.to_string(), is_git))
        })
        .collect();
    entries.sort();
    DirListing::Entries(entries)
}

/// Classify `path` on `host` in one round trip: does it exist, and is it a
/// git repo? The repo picker's Enter-commit validation — replacing the old
/// exists-only `ls` probe with the same trip cost. Blocking — call
/// from a worker thread.
pub fn classify_path_on(host: &HostDef, path: &str) -> Result<PathClass> {
    let path = expand_remote_tilde(host, path)?;
    let output = host_probe(
        host,
        &classify_path_script(&path),
        &classify_path_script_windows(&path),
    )
    .stdout(Stdio::piped())
    .stderr(Stdio::piped())
    .output()
    .context("failed to check remote path")?;
    let stdout = remote_output_or_stderr(output, "path check")?;
    parse_path_class(&String::from_utf8_lossy(&stdout))
}

/// The [`classify_path_on`] script for a **native-Windows** host, emitting the
/// same single word as [`classify_path_script`].
pub(super) fn classify_path_script_windows(path: &str) -> String {
    format!(
        "$p={q}\n\
         if (-not (Test-Path -LiteralPath $p -PathType Container)) {{ Write-Output 'missing' }}\n\
         elseif (Test-Path -LiteralPath ($p + '/.git')) {{ Write-Output 'git' }}\n\
         else {{ Write-Output 'dir' }}\n\
         exit 0",
        q = powershell_quote(path),
    )
}

/// The [`classify_path_on`] shell script (`path` already tilde-expanded).
pub(super) fn classify_path_script(path: &str) -> String {
    format!(
        "p={q}; if [ ! -d \"$p\" ]; then echo missing; \
         elif [ -e \"$p/.git\" ]; then echo git; else echo dir; fi",
        q = posix_quote(path),
    )
}

/// Parse the single-word [`classify_path_on`] output.
pub(super) fn parse_path_class(stdout: &str) -> Result<PathClass> {
    match stdout.trim() {
        "git" => Ok(PathClass::Git),
        "dir" => Ok(PathClass::Dir),
        "missing" => Ok(PathClass::Missing),
        other => anyhow::bail!("unexpected path-check output: {other:?}"),
    }
}

/// Remote analogue of [`scan_child_repos`]: the immediate child git repos of
/// `parent` on `host`, as absolute paths, sorted. Hidden entries are skipped
/// (matching the local scan). One round trip; blocking — call from a worker
/// thread. A missing/unreadable parent is an error (unlike the local scan's
/// empty vec) so the picker can tell the user instead of silently importing
/// nothing.
pub fn scan_child_repos_on(host: &HostDef, parent: &str) -> Result<Vec<PathBuf>> {
    let parent = expand_remote_tilde(host, parent)?;
    let output = host_probe(
        host,
        &scan_child_repos_script(&parent),
        &scan_child_repos_script_windows(&parent),
    )
    .stdout(Stdio::piped())
    .stderr(Stdio::piped())
    .output()
    .context("failed to scan remote parent directory")?;
    let stdout = remote_output_or_stderr(output, "parent scan")?;
    let parent = Path::new(&parent);
    collect_scanned_children(parent, &String::from_utf8_lossy(&stdout))
}

/// The [`scan_child_repos_on`] shell script (`parent` already tilde-expanded).
/// `*` skips hidden entries, matching the local scan; a missing/unreadable
/// parent exits non-zero (surfaced as an error, unlike the local scan's empty
/// vec, so the picker can tell the user instead of silently importing nothing).
pub(super) fn scan_child_repos_script(parent: &str) -> String {
    format!(
        "d={q}; cd \"$d\" || exit 1; \
         for e in *; do if [ -d \"$e\" ] && [ -e \"$e/.git\" ]; then \
         printf '%s\\n' \"$e\"; fi; done; exit 0",
        q = posix_quote(parent),
    )
}

/// The [`scan_child_repos_on`] script for a **native-Windows** host, emitting
/// the same one-name-per-line output as [`scan_child_repos_script`].
///
/// The `.`-prefix filter is explicit because that is the contract being
/// mirrored — the POSIX `*` glob's, which is about the *name*, whereas "hidden"
/// on Windows is a file attribute. Omitting `-Force` additionally skips a
/// directory carrying that attribute, so this is marginally stricter than the
/// POSIX twin; both refuse the same `.`-prefixed names, which is the rule
/// callers rely on.
///
/// Unlike the POSIX script's `cd … || exit 1`, a missing parent has no shell
/// message to surface, so one is written explicitly — an exit code alone would
/// reach the user as a bare "parent scan failed".
pub(super) fn scan_child_repos_script_windows(parent: &str) -> String {
    format!(
        "$d={q}\n\
         if (-not (Test-Path -LiteralPath $d -PathType Container)) {{ \
         [Console]::Error.WriteLine('no such directory: ' + $d); exit 1 }}\n\
         Get-ChildItem -LiteralPath $d -Directory -ErrorAction Stop \
         | Where-Object {{ -not $_.Name.StartsWith('.') }} \
         | Where-Object {{ Test-Path -LiteralPath ($_.FullName + '/.git') }} \
         | ForEach-Object {{ Write-Output $_.Name }}\n\
         exit 0",
        q = powershell_quote(parent),
    )
}

/// Join the scanned child names onto `parent`, sorted. A literal `*` line
/// (an empty dir's unexpanded glob never passes the `-d` test, but keep the
/// guard) and blank lines are skipped.
pub(super) fn collect_scanned_children(parent: &Path, stdout: &str) -> Result<Vec<PathBuf>> {
    let mut repos: Vec<PathBuf> = stdout
        .lines()
        .filter(|name| !name.is_empty() && *name != "*")
        .map(|name| parent.join(name))
        .collect();
    repos.sort();
    Ok(repos)
}

/// Parse repo name from the origin remote URL.
pub(super) fn repo_name_from_remote(path: &Path) -> Option<String> {
    let output = git_program()
        .args(["remote", "get-url", "origin"])
        .current_dir(path)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let url = String::from_utf8_lossy(&output.stdout).trim().to_string();
    parse_repo_name_from_url(&url)
}

/// Extract repo name from a git remote URL.
///
/// Handles common formats:
/// - `git@github.com:user/repo.git` → `"repo"`
/// - `https://github.com/user/repo.git` → `"repo"`
/// - `https://github.com/user/repo` → `"repo"`
pub(super) fn parse_repo_name_from_url(url: &str) -> Option<String> {
    // Split on the last '/' (HTTPS/SSH with path) or ':' (SSH shorthand).
    let after_sep = if url.contains('/') {
        url.rsplit('/').next()?
    } else {
        url.rsplit(':').next()?
    };
    let name = after_sep.strip_suffix(".git").unwrap_or(after_sep);
    if name.is_empty() {
        None
    } else {
        Some(name.to_string())
    }
}
