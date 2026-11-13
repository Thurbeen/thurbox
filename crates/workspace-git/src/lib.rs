//! workspace-git: a runtime's workspace made from a list of URLs
//! (docs/runtime/README.md, "Workspaces" and "workspace-git").
//!
//! ```text
//! <root>/<key>/<name>/                  one worktree per git URL
//! <root>/<key>/.fleet-workspace.json    written last; without it the workspace is partial
//! <root>/<key>/.fleet-sources           the repositories the key's worktrees belong to
//! <root>/.sources/<digest>.git          a bare source per remote repository, fetched before each make
//! <root>/.sources/<digest>.lock         one lock per repository a worktree is made from
//! <root>/.checkouts                     the local checkouts worktrees were made from
//! ```
//!
//! A `git+file` URL naming a local checkout (a repository with a working
//! tree) gets a worktree of that checkout itself: its branches are the
//! user's own, a commit made in the worktree is in the checkout at once, and
//! the preserved work of a release is `refs/fleet/preserved/<key>/<name>` in
//! the checkout. No branch of a checkout is ever deleted. The checkout's
//! `origin` is fetched before each make, so a new branch with no base starts
//! from the upstream's default branch as it is now.
//!
//! Any other git URL, a remote or a bare repository, gets a bare source of
//! its own. A source fetches its remote's branches into
//! `refs/remotes/origin/*`, so a fetch never moves a branch a worktree has
//! checked out. Every branch under its `refs/heads/` is one this program
//! made; the preserved work of a release is
//! `refs/fleet/preserved/<key>/<name>`, in the same source.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// The URL schemes this program makes a directory for.
pub const SCHEMES: &[&str] = &["git+https", "git+ssh", "git+file", "file"];

/// The days preserved work is kept when the runtime names no other.
pub const DEFAULT_PRESERVE_DAYS: u64 = 30;

/// The file that marks a workspace whole.
const RECORD: &str = ".fleet-workspace.json";

/// The file, in the key's directory, naming each repository the key's
/// worktrees were added to, written before each one: a rollback of a partial
/// workspace finds a checkout's worktrees through it.
const KEY_SOURCES: &str = ".fleet-sources";

/// The file, in the root, naming each local checkout a worktree was made
/// from, so the retention reaches the preserved work kept there.
const CHECKOUTS: &str = ".checkouts";

/// Where preserved work is kept, per key and repository name.
const PRESERVED: &str = "refs/fleet/preserved";

/// How long a make waits on a fetch of a checkout's `origin` before it uses
/// the refs already there.
const FETCH_DEADLINE: std::time::Duration = std::time::Duration::from_secs(30);

/// The longest key: what the runtime protocol accepts.
const MAX_KEY: usize = 128;

/// The ambient variables that pin git to another repository than the one a
/// call names: a hook exports them, and a runtime started under one inherits
/// them.
const GIT_LOCATION_ENV: [&str; 10] = [
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_INDEX_FILE",
    "GIT_COMMON_DIR",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_PREFIX",
    "GIT_NAMESPACE",
    "GIT_CONFIG_PARAMETERS",
    "GIT_CONFIG_COUNT",
];

/// Where workspaces are made, and how long preserved work is kept.
#[derive(Debug, Clone)]
pub struct Config {
    pub root: PathBuf,
    pub preserve_days: u64,
}

impl Config {
    pub fn new(root: PathBuf) -> Config {
        Config {
            root,
            preserve_days: DEFAULT_PRESERVE_DAYS,
        }
    }
}

/// The answer to a make, as the runtime protocol's `workspace` verb gives it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Answer {
    /// One directory per URL, in order; `adopted` when the key's workspace
    /// already existed.
    Made { dirs: Vec<String>, adopted: bool },
    /// This program makes nothing for that URL.
    Unsupported { url: String },
    /// Nothing was made, or what was made was rolled back.
    Failed { why: String },
}

/// What one URL asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Request {
    /// A directory, used as it is.
    Dir(PathBuf),
    /// A worktree of `remote`. `local` when the URL names a path on this
    /// machine (`git+file`).
    Git {
        remote: String,
        name: String,
        branch: Option<String>,
        base: Option<String>,
        local: bool,
    },
}

/// The record of a whole workspace.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Record {
    urls: Vec<String>,
    dirs: Vec<String>,
    trees: Vec<Tree>,
}

/// One worktree the workspace holds.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Tree {
    name: String,
    source: String,
    path: String,
}

/// Make the key's workspace, or adopt it. `Err` is an answer this program
/// cannot give: a rollback that did not finish.
pub fn make(cfg: &Config, key: &str, urls: &[String]) -> Result<Answer, String> {
    if let Err(why) = check_key(key) {
        return Ok(Answer::Failed { why });
    }
    let mut requests = Vec::with_capacity(urls.len());
    for url in urls {
        match parse(url) {
            Parsed::Ok(r) => requests.push(r),
            Parsed::Unsupported => return Ok(Answer::Unsupported { url: url.clone() }),
            Parsed::Bad(why) => return Ok(Answer::Failed { why }),
        }
    }
    dedupe_names(&mut requests);
    let dir = cfg.root.join(key);
    if let Some(record) = read_record(&dir)? {
        if record.urls == urls {
            return Ok(Answer::Made {
                dirs: record.dirs,
                adopted: true,
            });
        }
        return Ok(Answer::Failed {
            why: format!("key {key} already holds a workspace of other URLs"),
        });
    }
    // A partial workspace is never answered: roll it back, then make it again.
    rollback(cfg, key)?;
    prune(cfg);
    let mut trees = Vec::new();
    let mut dirs = Vec::with_capacity(requests.len());
    for request in &requests {
        let made = match request {
            Request::Dir(path) => Ok(path.display().to_string()),
            Request::Git {
                remote,
                name,
                branch,
                base,
                local,
            } => worktree(
                cfg,
                key,
                remote,
                *local,
                name,
                branch.as_deref(),
                base.as_deref(),
            )
            .map(|tree| {
                let path = tree.path.clone();
                trees.push(tree);
                path
            }),
        };
        match made {
            Ok(path) => dirs.push(path),
            Err(why) => {
                rollback(cfg, key)?;
                return Ok(Answer::Failed { why });
            }
        }
    }
    let record = Record {
        urls: urls.to_vec(),
        dirs: dirs.clone(),
        trees,
    };
    let written = fs::create_dir_all(&dir)
        .and_then(|()| {
            let json = serde_json::to_vec_pretty(&record).map_err(std::io::Error::other)?;
            let partial = dir.join(format!("{RECORD}.partial"));
            fs::write(&partial, json)?;
            fs::rename(&partial, dir.join(RECORD))
        })
        .map_err(|e| format!("cannot write {}: {e}", dir.join(RECORD).display()));
    if let Err(why) = written {
        rollback(cfg, key)?;
        return Ok(Answer::Failed { why });
    }
    Ok(Answer::Made {
        dirs,
        adopted: false,
    })
}

/// Release the key's workspace: work that exists nowhere else is preserved,
/// then its worktrees go. A directory it did not make is left as it is, and
/// no branch is deleted. A key with no workspace is released already.
pub fn release(cfg: &Config, key: &str) -> Result<(), String> {
    check_key(key)?;
    let dir = cfg.root.join(key);
    let Some(record) = read_record(&dir)? else {
        return rollback(cfg, key);
    };
    for tree in &record.trees {
        let source = Path::new(&tree.source);
        // A make or a retention on the same repository waits for this one.
        let _lock = lock_repository(cfg, source)?;
        let path = Path::new(&tree.path);
        if path.is_dir() {
            preserve(path, key, &tree.name)?;
        }
        remove_worktree(source, path)?;
    }
    remove_dir(&dir)
}

/// Undo a workspace no session has worked in, such as one whose runtime's
/// setup failed: its worktrees go with nothing preserved. A `file://`
/// directory is left as it is.
pub fn discard(cfg: &Config, key: &str) -> Result<(), String> {
    check_key(key)?;
    rollback(cfg, key)
}

/// A whole workspace: its URLs, and the directories made for them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Workspace {
    pub urls: Vec<String>,
    pub dirs: Vec<String>,
}

/// The key's whole workspace, or `None` when the key holds none.
pub fn inspect(cfg: &Config, key: &str) -> Result<Option<Workspace>, String> {
    check_key(key)?;
    Ok(read_record(&cfg.root.join(key))?.map(|r| Workspace {
        urls: r.urls,
        dirs: r.dirs,
    }))
}

fn remove_dir(dir: &Path) -> Result<(), String> {
    match fs::remove_dir_all(dir) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(format!("cannot remove {}: {e}", dir.display())),
    }
}

fn read_record(dir: &Path) -> Result<Option<Record>, String> {
    let path = dir.join(RECORD);
    match fs::read(&path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|e| format!("cannot read {}: {e}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("cannot read {}: {e}", path.display())),
    }
}

/// Undo whatever of the key's workspace exists: the worktrees under the
/// key's directory, in each repository the key names, then the directory.
/// Only those repositories are locked, so another key's fetch never holds
/// a rollback up. Nothing is preserved: a partial workspace was never
/// answered, so no session worked in it.
fn rollback(cfg: &Config, key: &str) -> Result<(), String> {
    let dir = cfg.root.join(key);
    if !dir.exists() {
        return Ok(());
    }
    for source in listed_paths(&dir.join(KEY_SOURCES)) {
        if !source.is_dir() {
            continue;
        }
        let _lock = lock_repository(cfg, &source)?;
        let listed = git(&source, &["worktree", "list", "--porcelain"], &[]).unwrap_or_default();
        for line in listed.lines() {
            if let Some(path) = line.strip_prefix("worktree ")
                && Path::new(path).starts_with(&dir)
            {
                remove_worktree(&source, Path::new(path))?;
            }
        }
    }
    remove_dir(&dir)
}

fn remove_worktree(source: &Path, path: &Path) -> Result<(), String> {
    if path.exists() {
        let path = path.display().to_string();
        git(
            source,
            &["worktree", "remove", "--force", "--force", &path],
            &[],
        )?;
    }
    git(source, &["worktree", "prune"], &[]).map(|_| ())
}

fn sources(cfg: &Config) -> Vec<PathBuf> {
    fs::read_dir(cfg.root.join(".sources"))
        .map(|entries| {
            entries
                .filter_map(Result::ok)
                .map(|e| e.path())
                .filter(|p| p.is_dir())
                .collect()
        })
        .unwrap_or_default()
}

/// The paths a list file names, one per line, each once; none when the file
/// is missing.
fn listed_paths(file: &Path) -> Vec<PathBuf> {
    let mut paths: Vec<PathBuf> = Vec::new();
    for line in fs::read_to_string(file).unwrap_or_default().lines() {
        let path = PathBuf::from(line);
        if !line.is_empty() && !paths.contains(&path) {
            paths.push(path);
        }
    }
    paths
}

/// Add `path` to the list file unless it names it already.
fn list_path(file: &Path, path: &Path) -> Result<(), String> {
    use std::io::Write;
    if listed_paths(file).iter().any(|p| p == path) {
        return Ok(());
    }
    if let Some(dir) = file.parent() {
        fs::create_dir_all(dir).map_err(|e| format!("cannot make {}: {e}", dir.display()))?;
    }
    fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(file)
        .and_then(|mut f| writeln!(f, "{}", path.display()))
        .map_err(|e| format!("cannot write {}: {e}", file.display()))
}

/// The main checkout a repository at `repo` belongs to, from the common git
/// directory `git rev-parse --git-common-dir` printed there. git before 2.31
/// prints it relative to where it ran (`.git`, `../.git`), so it is read
/// against `repo`, then canonicalized: one checkout has one identity
/// whichever directory of it a URL names. A common directory named `.git`
/// belongs to the directory holding it; any other is a git directory kept
/// apart from its working tree, and git runs in it.
fn main_checkout(repo: &Path, common: &str) -> Result<PathBuf, String> {
    let joined = repo.join(common);
    let common = joined
        .canonicalize()
        .map(plain)
        .map_err(|e| format!("cannot read {}: {e}", joined.display()))?;
    if common.file_name().is_some_and(|n| n == ".git") {
        common
            .parent()
            .map(Path::to_path_buf)
            .ok_or_else(|| format!("{} has no parent directory", common.display()))
    } else {
        Ok(common)
    }
}

/// `path` as git reads it back: without the verbatim prefix Windows'
/// `canonicalize` adds. A drive path `\\?\C:\x` is `C:\x`; a network share
/// `\\?\UNC\server\share\x` keeps its UNC form, `\\server\share\x`.
fn plain(path: PathBuf) -> PathBuf {
    let Some(text) = path.to_str() else {
        return path;
    };
    if let Some(share) = text.strip_prefix(r"\\?\UNC\") {
        return PathBuf::from(format!(r"\\{share}"));
    }
    match text.strip_prefix(r"\\?\") {
        Some(rest) => PathBuf::from(rest),
        None => path,
    }
}

/// The local checkout a `git+file` URL names, when it is one: a repository
/// with a working tree. `None` for a bare repository, which gets a source
/// of its own, and for a path that is no directory, whose source then fails
/// by name. A linked worktree or a subdirectory names its main checkout
/// ([`main_checkout`]): one repository has one identity, so one lock, and a
/// linked worktree removed later never leaves a release running git in a
/// missing directory. A directory git cannot read as a repository, or a
/// checkout whose main checkout cannot be found, fails the make: a local
/// checkout never falls back to a copy of itself.
fn checkout(remote: &str) -> Result<Option<PathBuf>, String> {
    let path = PathBuf::from(remote);
    if !path.is_dir() {
        return Ok(None);
    }
    let bare = git(&path, &["rev-parse", "--is-bare-repository"], &[])
        .map_err(|why| format!("{remote} is no git repository: {why}"))?;
    if bare != "false" {
        return Ok(None);
    }
    let common = git(&path, &["rev-parse", "--git-common-dir"], &[])
        .map_err(|why| format!("cannot find the checkout {remote} belongs to: {why}"))?;
    main_checkout(&path, &common)
        .map(Some)
        .map_err(|why| format!("cannot find the checkout {remote} belongs to: {why}"))
}

/// Commit the worktree's uncommitted work, untracked files included, onto
/// `refs/fleet/preserved/<key>/<name>` through a temporary index. A clean
/// worktree preserves nothing: its branch already holds its commits.
fn preserve(tree: &Path, key: &str, name: &str) -> Result<(), String> {
    let status = git(
        tree,
        &["status", "--porcelain", "--untracked-files=all"],
        &[],
    )?;
    if status.is_empty() {
        return Ok(());
    }
    // Beside the worktree, in the key's directory: never in a checkout's
    // working tree, where it would show as a file of the user's.
    let index = tree
        .parent()
        .unwrap_or(tree)
        .join(format!(".fleet-preserve-{name}.index"));
    let index_s = index.display().to_string();
    let env = [("GIT_INDEX_FILE", index_s.as_str())];
    let result = (|| {
        git(tree, &["read-tree", "HEAD"], &env)?;
        git(tree, &["add", "-A"], &env)?;
        let tree_id = git(tree, &["write-tree"], &env)?;
        let commit = git(
            tree,
            &[
                "commit-tree",
                "--no-gpg-sign",
                &tree_id,
                "-p",
                "HEAD",
                "-m",
                &format!("fleet: work preserved when {key} was released"),
            ],
            &[
                ("GIT_AUTHOR_NAME", "Fleet"),
                ("GIT_AUTHOR_EMAIL", "fleet@localhost"),
                ("GIT_COMMITTER_NAME", "Fleet"),
                ("GIT_COMMITTER_EMAIL", "fleet@localhost"),
            ],
        )?;
        git(
            tree,
            &["update-ref", &format!("{PRESERVED}/{key}/{name}"), &commit],
            &[],
        )
        .map(|_| ())
    })();
    let _ = fs::remove_file(&index);
    result
}

/// Make one worktree at `<root>/<key>/<name>`: of the checkout itself when
/// a `git+file` URL names one, else of the remote's bare source.
fn worktree(
    cfg: &Config,
    key: &str,
    remote: &str,
    local: bool,
    name: &str,
    branch: Option<&str>,
    base: Option<&str>,
) -> Result<Tree, String> {
    let checkout = if local { checkout(remote)? } else { None };
    let source = match &checkout {
        Some(dir) => dir.clone(),
        None => source_dir(cfg, remote),
    };
    // Held until the worktree is added: the repository is made, fetched and
    // given a worktree by one maker at a time.
    let _lock = lock_repository(cfg, &source)?;
    let key_dir = cfg.root.join(key);
    fs::create_dir_all(&key_dir).map_err(|e| format!("cannot make {}: {e}", key_dir.display()))?;
    if checkout.is_some() {
        list_path(&cfg.root.join(CHECKOUTS), &source)?;
        fetch_upstream(&source, FETCH_DEADLINE);
    } else {
        fill_source(&source, remote)?;
    }
    // Before the worktree is added, so a rollback of a partial workspace
    // finds it; a registration a removed partial directory left is pruned.
    list_path(&key_dir.join(KEY_SOURCES), &source)?;
    let _ = git(&source, &["worktree", "prune"], &[]);
    let path = key_dir.join(name);
    let path_s = path.display().to_string();
    let wanted = branch.unwrap_or(key);
    git(&source, &["check-ref-format", "--branch", wanted], &[])
        .map_err(|_| format!("{wanted:?} is not a branch name"))?;
    let local = format!("refs/heads/{wanted}");
    let remote_branch = format!("refs/remotes/origin/{wanted}");
    if branch.is_some() && has_ref(&source, &local) {
        if checked_out(&source, wanted)? {
            let free = free_branch(&source, wanted)?;
            git(
                &source,
                &["worktree", "add", "-q", "-b", &free, &path_s, &local],
                &[],
            )?;
        } else {
            git(&source, &["worktree", "add", "-q", &path_s, wanted], &[])?;
        }
    } else if branch.is_some() && has_ref(&source, &remote_branch) {
        git(
            &source,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                wanted,
                &path_s,
                &remote_branch,
            ],
            &[],
        )?;
    } else {
        // A checkout with no remote default starts from what the directory
        // the URL names has checked out: a linked worktree's own HEAD.
        let named = checkout.as_ref().map(|_| Path::new(remote));
        let start = resolve_base(&source, named, key, name, base)?;
        let new = if has_ref(&source, &local) {
            free_branch(&source, wanted)?
        } else {
            wanted.to_string()
        };
        git(
            &source,
            &["worktree", "add", "-q", "-b", &new, &path_s, &start],
            &[],
        )?;
    }
    Ok(Tree {
        name: name.to_string(),
        source: source.display().to_string(),
        path: path_s,
    })
}

/// Fetch a checkout's `origin`, so a base read from `refs/remotes/origin/*`,
/// `origin/HEAD` included (`followRemoteHEAD=always`, git 2.48+), is the
/// upstream as it is now, not as the user last fetched it. A checkout
/// with no `origin`, or one that cannot be reached, is made from what it
/// already holds: an offline machine still gets its worktree.
///
/// Bounded by `deadline`: an http `origin` gives up when it has moved no
/// bytes for that long, and [`git`]'s default ssh (used when the user
/// configured none) has a connect timeout and keepalives, so an `origin`
/// that never answers cannot hold the make.
fn fetch_upstream(checkout: &Path, deadline: std::time::Duration) {
    let secs = deadline.as_secs().max(1).to_string();
    let _ = git(
        checkout,
        &[
            "-c",
            "remote.origin.followRemoteHEAD=always",
            "fetch",
            "-q",
            "origin",
        ],
        &[
            ("GIT_HTTP_LOW_SPEED_LIMIT", "1"),
            ("GIT_HTTP_LOW_SPEED_TIME", &secs),
        ],
    );
}

/// The commit a new branch starts from: the fragment, as a branch, a tag or
/// a ref or commit as given, else the default branch (`origin/HEAD`, else
/// `origin/main`, else `origin/master`). A checkout reads its own branches
/// first, then its remote's; a source, its remote's first; `origin/<b>`
/// is always the remote's.
/// `named` is the directory a checkout's URL names: with no remote default,
/// its HEAD is the start.
fn resolve_base(
    source: &Path,
    named: Option<&Path>,
    key: &str,
    name: &str,
    base: Option<&str>,
) -> Result<String, String> {
    let is_checkout = named.is_some();
    let Some(base) = base else {
        let default = ["HEAD", "main", "master"]
            .iter()
            .find_map(|branch| verify(source, &format!("refs/remotes/origin/{branch}")));
        let default = default.or_else(|| named.and_then(|dir| verify(dir, "HEAD")));
        return default.ok_or_else(|| "the repository has no default branch".to_string());
    };
    let remote = format!("refs/remotes/origin/{base}");
    let local = format!("refs/heads/{base}");
    let tag = format!("refs/tags/{base}");
    let mut candidates = if is_checkout {
        vec![local, tag, remote]
    } else {
        vec![remote, tag, local]
    };
    // `origin/<b>` is the remote-tracking branch the new-session flow offers,
    // ahead of a local branch or tag spelled the same.
    if base.starts_with("origin/") {
        candidates.insert(0, format!("refs/remotes/{base}"));
    }
    candidates.push(base.to_string());
    for candidate in candidates {
        if let Some(commit) = verify(source, &candidate) {
            return Ok(commit);
        }
    }
    // A ref the remote has but a fetch of its branches does not bring, such
    // as preserved work: fetched by name.
    if base.starts_with("refs/") {
        let into = format!("refs/fleet/base/{key}/{name}");
        git(
            source,
            &["fetch", "-q", "origin", &format!("+{base}:{into}")],
            &[],
        )
        .map_err(|why| format!("cannot fetch {base}: {why}"))?;
        return verify(source, &into).ok_or_else(|| format!("{base} names no commit"));
    }
    Err(format!("{base} names no branch, tag or commit"))
}

fn verify(source: &Path, rev: &str) -> Option<String> {
    git(
        source,
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("{rev}^{{commit}}"),
        ],
        &[],
    )
    .ok()
    .filter(|s| !s.is_empty())
}

fn has_ref(source: &Path, name: &str) -> bool {
    git(source, &["show-ref", "--verify", "--quiet", name], &[]).is_ok()
}

fn checked_out(source: &Path, branch: &str) -> Result<bool, String> {
    let listed = git(source, &["worktree", "list", "--porcelain"], &[])?;
    let line = format!("branch refs/heads/{branch}");
    Ok(listed.lines().any(|l| l == line))
}

/// `<branch>-<n>` for the first `n` from 2 that names no branch.
fn free_branch(source: &Path, branch: &str) -> Result<String, String> {
    (2..1000)
        .map(|n| format!("{branch}-{n}"))
        .find(|b| !has_ref(source, &format!("refs/heads/{b}")))
        .ok_or_else(|| format!("no free branch name after {branch}"))
}

/// A short digest of `text`, which names a source or a lock in `.sources`.
fn digest(text: &str) -> String {
    Sha256::digest(text.as_bytes())
        .iter()
        .take(8)
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Where the bare source for `remote` is kept.
fn source_dir(cfg: &Config, remote: &str) -> PathBuf {
    cfg.root
        .join(".sources")
        .join(format!("{}.git", digest(remote)))
}

fn lock_file(cfg: &Config, repository: &Path) -> Result<fs::File, String> {
    let dir = cfg.root.join(".sources");
    fs::create_dir_all(&dir).map_err(|e| format!("cannot make {}: {e}", dir.display()))?;
    let path = dir.join(format!(
        "{}.lock",
        digest(&repository.display().to_string())
    ));
    fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&path)
        .map_err(|e| format!("cannot open {}: {e}", path.display()))
}

/// An exclusive lock on one repository a worktree is made from, a source
/// or a checkout, released when the file is dropped or the process ends.
/// Makes, releases, rollbacks and the retention of one repository take it,
/// and two runtimes that share a root take the same one, so none removes a
/// source, a worktree or a ref another is using.
fn lock_repository(cfg: &Config, repository: &Path) -> Result<fs::File, String> {
    let file = lock_file(cfg, repository)?;
    file.lock()
        .map_err(|e| format!("cannot lock {}: {e}", repository.display()))?;
    Ok(file)
}

/// The lock on `repository` when no one holds it; `None` when another
/// does, or it cannot be taken.
fn try_lock_repository(cfg: &Config, repository: &Path) -> Option<fs::File> {
    let file = lock_file(cfg, repository).ok()?;
    file.try_lock().ok()?;
    Some(file)
}

/// Make the bare source at `dir` for `remote` on first use, and fetch it
/// every time. The caller holds [`lock_repository`] on `dir`.
fn fill_source(dir: &Path, remote: &str) -> Result<(), String> {
    let fresh = !dir.join("HEAD").is_file();
    let made = (|| {
        if fresh {
            fs::create_dir_all(dir).map_err(|e| format!("cannot make {}: {e}", dir.display()))?;
            git(dir, &["init", "-q", "--bare"], &[])?;
            git(dir, &["remote", "add", "origin", remote], &[])?;
        }
        git(dir, &["fetch", "-q", "--tags", "origin"], &[])
            .map_err(|why| format!("cannot fetch {remote}: {why}"))
    })();
    if let Err(why) = made {
        // A source this call made and could not fill is not kept.
        if fresh {
            let _ = fs::remove_dir_all(dir);
        }
        return Err(why);
    }
    // A remote with no default branch, such as another source, still serves
    // a URL that names its base.
    let _ = git(dir, &["remote", "set-head", "origin", "--auto"], &[]);
    Ok(())
}

/// The retention, run on each make: preserved work older than
/// `preserve_days`, in sources and in checkouts, and a source's branches not
/// checked out whose head the remote already has. A checkout's branches are
/// the user's and are never deleted. A repository another call holds is
/// left for the next make.
fn prune(cfg: &Config) {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let keep = cfg.preserve_days.saturating_mul(86_400);
    let prune_preserved = |repository: &Path| {
        let preserved = git(
            repository,
            &[
                "for-each-ref",
                "--format=%(refname) %(committerdate:unix)",
                PRESERVED,
            ],
            &[],
        )
        .unwrap_or_default();
        for line in preserved.lines() {
            let Some((name, when)) = line.rsplit_once(' ') else {
                continue;
            };
            let when: u64 = when.parse().unwrap_or(now);
            if now.saturating_sub(when) >= keep {
                let _ = git(repository, &["update-ref", "-d", name], &[]);
            }
        }
    };
    for checkout in listed_paths(&cfg.root.join(CHECKOUTS)) {
        if !checkout.is_dir() {
            continue;
        }
        let Some(_lock) = try_lock_repository(cfg, &checkout) else {
            continue;
        };
        prune_preserved(&checkout);
    }
    for source in sources(cfg) {
        let Some(_lock) = try_lock_repository(cfg, &source) else {
            continue;
        };
        prune_preserved(&source);
        let Ok(listed) = git(&source, &["worktree", "list", "--porcelain"], &[]) else {
            continue;
        };
        let heads = git(
            &source,
            &["for-each-ref", "--format=%(refname:short)", "refs/heads"],
            &[],
        )
        .unwrap_or_default();
        for branch in heads.lines() {
            if listed
                .lines()
                .any(|l| l == format!("branch refs/heads/{branch}"))
            {
                continue;
            }
            let held = git(
                &source,
                &[
                    "for-each-ref",
                    "--format=%(refname)",
                    "--contains",
                    &format!("refs/heads/{branch}"),
                    "refs/remotes/origin",
                ],
                &[],
            )
            .unwrap_or_default();
            if !held.is_empty() {
                let _ = git(&source, &["branch", "-q", "-D", branch], &[]);
            }
        }
    }
}

/// One git command in `dir`, its trimmed stdout, or its stderr as the error.
/// Never waits for a person: no prompt for a password, ssh in batch mode.
fn git(dir: &Path, args: &[&str], env: &[(&str, &str)]) -> Result<String, String> {
    let mut cmd = Command::new("git");
    for var in GIT_LOCATION_ENV {
        cmd.env_remove(var);
    }
    cmd.env("GIT_TERMINAL_PROMPT", "0");
    if reaches_remote(args) && !own_ssh_command(dir) {
        cmd.env(
            "GIT_SSH_COMMAND",
            "ssh -o BatchMode=yes -o ConnectTimeout=10 -o ServerAliveInterval=10 -o ServerAliveCountMax=3",
        );
    }
    // Every call that reaches a remote (a source's fetch, `set-head --auto`,
    // a base fetched by name) gives up on an http remote that stalls, as
    // the ssh default above gives up on an ssh one.
    if std::env::var_os("GIT_HTTP_LOW_SPEED_TIME").is_none() {
        cmd.env("GIT_HTTP_LOW_SPEED_LIMIT", "1").env(
            "GIT_HTTP_LOW_SPEED_TIME",
            FETCH_DEADLINE.as_secs().to_string(),
        );
    }
    for (k, v) in env {
        cmd.env(k, v);
    }
    let out = cmd
        .current_dir(dir)
        .args(args)
        .output()
        .map_err(|e| format!("cannot run git: {e}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    } else {
        Err(format!(
            "git {}: {}",
            args.first().copied().unwrap_or_default(),
            String::from_utf8_lossy(&out.stderr).trim()
        ))
    }
}

/// Whether a call can reach a remote: the calls the default ssh command and
/// its check in [`own_ssh_command`] are for.
fn reaches_remote(args: &[&str]) -> bool {
    args.iter()
        .any(|arg| matches!(*arg, "fetch" | "set-head" | "clone" | "ls-remote"))
}

/// Whether the user configured how git runs ssh (`GIT_SSH_COMMAND`, `GIT_SSH`
/// or `core.sshCommand` for `dir`): a wrapper, a key or a proxy git must keep,
/// so the default ssh command is not set over it.
fn own_ssh_command(dir: &Path) -> bool {
    if std::env::var_os("GIT_SSH_COMMAND").is_some() || std::env::var_os("GIT_SSH").is_some() {
        return true;
    }
    let mut cmd = Command::new("git");
    for var in GIT_LOCATION_ENV {
        cmd.env_remove(var);
    }
    cmd.current_dir(dir)
        .args(["config", "--get", "core.sshCommand"])
        .output()
        .is_ok_and(|out| out.status.success() && !out.stdout.trim_ascii().is_empty())
}

/// A key this program accepts: the runtime protocol's.
fn check_key(key: &str) -> Result<(), String> {
    if key.is_empty()
        || key.len() > MAX_KEY
        || key.starts_with('.')
        || !key
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_'))
    {
        return Err(format!(
            "key {key:?}: use 1 to {MAX_KEY} ASCII letters, digits, '.', '-' or '_', not starting with '.'"
        ));
    }
    Ok(())
}

enum Parsed {
    Ok(Request),
    Unsupported,
    Bad(String),
}

/// What a URL asks for. Only the scheme decides whether this program handles
/// it.
fn parse(url: &str) -> Parsed {
    let Some((scheme, rest)) = url.split_once("://") else {
        return Parsed::Bad(format!("{url} is not an absolute URL"));
    };
    if !SCHEMES.contains(&scheme) {
        return Parsed::Unsupported;
    }
    let (rest, fragment) = match rest.split_once('#') {
        Some((r, f)) => (r, Some(f)),
        None => (rest, None),
    };
    let (rest, query) = match rest.split_once('?') {
        Some((r, q)) => (r, Some(q)),
        None => (rest, None),
    };
    if scheme == "file" {
        if query.is_some() || fragment.is_some() {
            return Parsed::Bad(format!("{url}: a file:// URL takes no query or fragment"));
        }
        let path = PathBuf::from(local_path(&decode(rest)));
        if !path.is_absolute() {
            return Parsed::Bad(format!("{url} names no absolute path"));
        }
        if !path.is_dir() {
            return Parsed::Bad(format!("{url}: {} is not a directory", path.display()));
        }
        return Parsed::Ok(Request::Dir(path));
    }
    let transport = scheme.trim_start_matches("git+");
    let remote = if transport == "file" {
        // A local path, as git reads one: it also clones a non-bare checkout.
        local_path(&decode(rest))
    } else {
        format!("{transport}://{rest}")
    };
    let mut branch = None;
    if let Some(query) = query {
        for pair in query.split('&').filter(|p| !p.is_empty()) {
            match pair.split_once('=') {
                Some(("branch", value)) if !value.is_empty() => branch = Some(decode(value)),
                _ => return Parsed::Bad(format!("{url}: unknown query {pair:?}")),
            }
        }
    }
    Parsed::Ok(Request::Git {
        name: repo_name(rest),
        remote,
        branch,
        base: fragment.filter(|f| !f.is_empty()).map(decode),
        local: transport == "file",
    })
}

/// The worktree's directory name: the URL path's last segment, without
/// `.git`.
fn repo_name(path: &str) -> String {
    let last = path
        .trim_end_matches('/')
        .rsplit(['/', ':'])
        .next()
        .unwrap_or_default();
    let last = decode(last);
    let last = last.strip_suffix(".git").unwrap_or(&last);
    let name: String = last
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_') {
                c
            } else {
                '_'
            }
        })
        .collect();
    let name = name.trim_start_matches('.');
    if name.is_empty() {
        "repo".into()
    } else {
        name.into()
    }
}

/// Two URLs of one repository name get `<name>-2`, `<name>-3`.
fn dedupe_names(requests: &mut [Request]) {
    let mut seen: Vec<String> = Vec::new();
    for request in requests.iter_mut() {
        if let Request::Git { name, .. } = request {
            let mut candidate = name.clone();
            let mut n = 2;
            while seen.contains(&candidate) {
                candidate = format!("{name}-{n}");
                n += 1;
            }
            seen.push(candidate.clone());
            *name = candidate;
        }
    }
}

/// The local path a decoded `file`/`git+file` URL path names here. On
/// Windows a URL spells `C:\\dir` as `/C:/dir`: the leading `/` goes and the
/// separators become `\\`.
fn local_path(path: &str) -> String {
    if cfg!(windows) {
        windows_path(path)
    } else {
        path.to_string()
    }
}

fn windows_path(path: &str) -> String {
    let bytes = path.as_bytes();
    let drive =
        bytes.len() >= 3 && bytes[0] == b'/' && bytes[1].is_ascii_alphabetic() && bytes[2] == b':';
    let path = if drive { &path[1..] } else { path };
    path.replace('/', "\\")
}

/// `%XX` decoded; anything else as it is.
fn decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && let Some(hex) = bytes.get(i + 1..i + 3)
            && let Ok(hex) = std::str::from_utf8(hex)
            && let Ok(b) = u8::from_str_radix(hex, 16)
        {
            out.push(b);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A checkout's own `core.sshCommand` is the one its fetch runs: the
    /// default ssh options never replace it.
    #[cfg(unix)]
    #[test]
    fn a_fetch_keeps_the_checkouts_own_ssh_command() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().expect("tempdir");
        let repo = dir.path().join("repo");
        fs::create_dir_all(&repo).expect("mkdir");
        git(&repo, &["init", "-q"], &[]).expect("init");
        let marker = dir.path().join("used");
        let wrapper = dir.path().join("ssh-wrapper");
        fs::write(
            &wrapper,
            format!("#!/bin/sh\ntouch '{}'\nexit 1\n", marker.display()),
        )
        .expect("wrapper");
        fs::set_permissions(&wrapper, fs::Permissions::from_mode(0o755)).expect("chmod");
        git(
            &repo,
            &["remote", "add", "origin", "ssh://example.invalid/repo.git"],
            &[],
        )
        .expect("origin");
        git(
            &repo,
            &[
                "config",
                "core.sshCommand",
                wrapper.to_str().expect("utf-8"),
            ],
            &[],
        )
        .expect("sshCommand");

        fetch_upstream(&repo, std::time::Duration::from_secs(1));

        assert!(marker.exists(), "the configured ssh command was not run");
    }

    /// An `origin` that accepts the connection and never answers costs the
    /// make the deadline, not forever: the checkout's refs are used as they are.
    #[test]
    fn a_fetch_from_an_origin_that_never_answers_gives_up() {
        let silent = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = silent.local_addr().expect("addr").port();
        let dir = tempfile::tempdir().expect("tempdir");
        git(dir.path(), &["init", "-q"], &[]).expect("init");
        let url = format!("http://127.0.0.1:{port}/repo.git");
        git(dir.path(), &["remote", "add", "origin", &url], &[]).expect("origin");

        let started = std::time::Instant::now();
        fetch_upstream(dir.path(), std::time::Duration::from_secs(1));

        assert!(
            started.elapsed() < std::time::Duration::from_secs(20),
            "gave up after {:?}",
            started.elapsed()
        );
        drop(silent);
    }

    /// A URL spells a Windows path with a leading `/` and `/` separators.
    #[test]
    fn a_windows_drive_path_is_read_from_its_url_spelling() {
        assert_eq!(windows_path("/C:/Users/me/app"), "C:\\Users\\me\\app");
        assert_eq!(windows_path("C:/x"), "C:\\x");
        assert_eq!(windows_path("/srv/app"), "\\srv\\app");
    }

    #[test]
    fn a_git_url_names_its_remote_branch_and_base() {
        let Parsed::Ok(r) = parse("git+ssh://git@host/org/repo.git?branch=feat-x#main") else {
            panic!("parsed");
        };
        assert_eq!(
            r,
            Request::Git {
                remote: "ssh://git@host/org/repo.git".into(),
                name: "repo".into(),
                branch: Some("feat-x".into()),
                base: Some("main".into()),
                local: false,
            }
        );
        assert!(matches!(parse("s3://bucket/x"), Parsed::Unsupported));
        assert!(matches!(parse("relative/path"), Parsed::Bad(_)));
        assert!(matches!(parse("git+https://h/r?depth=1"), Parsed::Bad(_)));
        assert_eq!(decode("a%20b%2"), "a b%2");
    }

    #[test]
    fn two_urls_of_one_name_get_their_own_directories() {
        let mut requests = vec![
            Request::Git {
                remote: "a".into(),
                name: "app".into(),
                branch: None,
                base: None,
                local: false,
            },
            Request::Git {
                remote: "b".into(),
                name: "app".into(),
                branch: None,
                base: None,
                local: false,
            },
        ];
        dedupe_names(&mut requests);
        assert!(matches!(&requests[1], Request::Git { name, .. } if name == "app-2"));
    }
    /// git before 2.31 has no `--path-format`, and prints the common
    /// directory relative to where it ran: `.git` at the top, `../.git` in a
    /// subdirectory. Both name the same checkout.
    #[test]
    fn a_relative_common_dir_resolves_to_the_main_checkout() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("app");
        fs::create_dir_all(repo.join(".git")).unwrap();
        fs::create_dir_all(repo.join("sub")).unwrap();
        let top = main_checkout(&repo, ".git").unwrap();
        let below = main_checkout(&repo.join("sub"), "../.git").unwrap();
        let canonical = repo.canonicalize().unwrap();
        assert_eq!(top.canonicalize().unwrap(), canonical);
        assert_eq!(top, below, "one checkout, one identity");
        let absolute =
            main_checkout(&repo.join("sub"), &repo.join(".git").display().to_string()).unwrap();
        assert_eq!(absolute, top, "an absolute answer names it the same way");
    }
    /// `canonicalize` on Windows adds `\\?\`, and `\\?\UNC\` for a network
    /// share: a drive path loses the prefix, a share keeps its `\\` form.
    #[test]
    fn a_verbatim_path_reads_back_as_git_reads_it() {
        assert_eq!(
            plain(PathBuf::from(r"\\?\C:\Users\me\app")),
            PathBuf::from(r"C:\Users\me\app")
        );
        assert_eq!(
            plain(PathBuf::from(r"\\?\UNC\server\share\app")),
            PathBuf::from(r"\\server\share\app")
        );
        assert_eq!(plain(PathBuf::from("/srv/app")), PathBuf::from("/srv/app"));
    }
}
