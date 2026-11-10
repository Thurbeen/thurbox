//! workspace-git: a runtime's workspace made from a list of URLs
//! (docs/runtime/README.md, "Workspaces" and "workspace-git").
//!
//! ```text
//! <root>/<key>/<name>/                  one worktree per git URL
//! <root>/<key>/.fleet-workspace.json    written last; without it the workspace is partial
//! <root>/.sources/<digest>.git          a bare source per repository, fetched before each make
//! ```
//!
//! A source fetches its remote's branches into `refs/remotes/origin/*`, so a
//! fetch never moves a branch a worktree has checked out. Every branch under
//! `refs/heads/` is one this program made; the preserved work of a release is
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

/// Where preserved work is kept, per key and repository name.
const PRESERVED: &str = "refs/fleet/preserved";

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
    /// A worktree of `remote`.
    Git {
        remote: String,
        name: String,
        branch: Option<String>,
        base: Option<String>,
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
            } => worktree(cfg, key, remote, name, branch.as_deref(), base.as_deref()).map(|tree| {
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
        let path = Path::new(&tree.path);
        if path.is_dir() {
            preserve(Path::new(&tree.source), path, key, &tree.name)?;
        }
        remove_worktree(Path::new(&tree.source), path)?;
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

/// Undo whatever of the key's workspace exists: each source's worktrees under
/// the key's directory, then the directory. Nothing is preserved: a partial
/// workspace was never answered, so no session worked in it.
fn rollback(cfg: &Config, key: &str) -> Result<(), String> {
    let dir = cfg.root.join(key);
    if !dir.exists() {
        return Ok(());
    }
    for source in sources(cfg) {
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

/// Commit the worktree's uncommitted work, untracked files included, onto
/// `refs/fleet/preserved/<key>/<name>` through a temporary index. A clean
/// worktree preserves nothing: its branch already holds its commits.
fn preserve(source: &Path, tree: &Path, key: &str, name: &str) -> Result<(), String> {
    let status = git(
        tree,
        &["status", "--porcelain", "--untracked-files=all"],
        &[],
    )?;
    if status.is_empty() {
        return Ok(());
    }
    let index = source.join(format!("fleet-preserve-{key}-{name}.index"));
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

/// Make one worktree at `<root>/<key>/<name>` from the remote's source.
fn worktree(
    cfg: &Config,
    key: &str,
    remote: &str,
    name: &str,
    branch: Option<&str>,
    base: Option<&str>,
) -> Result<Tree, String> {
    let source = source(cfg, remote)?;
    let path = cfg.root.join(key).join(name);
    fs::create_dir_all(cfg.root.join(key))
        .map_err(|e| format!("cannot make {}: {e}", cfg.root.join(key).display()))?;
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
        let start = resolve_base(&source, key, name, base)?;
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

/// The commit a new branch starts from: the fragment, as a remote branch, a
/// tag, a branch of this source or a ref or commit as given, else the
/// remote's default branch.
fn resolve_base(
    source: &Path,
    key: &str,
    name: &str,
    base: Option<&str>,
) -> Result<String, String> {
    let Some(base) = base else {
        return verify(source, "refs/remotes/origin/HEAD")
            .ok_or_else(|| "the repository has no default branch".to_string());
    };
    for candidate in [
        format!("refs/remotes/origin/{base}"),
        format!("refs/tags/{base}"),
        format!("refs/heads/{base}"),
        base.to_string(),
    ] {
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

/// The bare source for `remote`, made on first use and fetched every time.
fn source(cfg: &Config, remote: &str) -> Result<PathBuf, String> {
    let digest = Sha256::digest(remote.as_bytes());
    let hex: String = digest.iter().take(8).map(|b| format!("{b:02x}")).collect();
    let dir = cfg.root.join(".sources").join(format!("{hex}.git"));
    let fresh = !dir.join("HEAD").is_file();
    let made = (|| {
        if fresh {
            fs::create_dir_all(&dir).map_err(|e| format!("cannot make {}: {e}", dir.display()))?;
            git(&dir, &["init", "-q", "--bare"], &[])?;
            git(&dir, &["remote", "add", "origin", remote], &[])?;
        }
        git(&dir, &["fetch", "-q", "--tags", "origin"], &[])
            .map_err(|why| format!("cannot fetch {remote}: {why}"))
    })();
    if let Err(why) = made {
        // A source this call made and could not fill is not kept.
        if fresh {
            let _ = fs::remove_dir_all(&dir);
        }
        return Err(why);
    }
    // A remote with no default branch, such as another source, still serves
    // a URL that names its base.
    let _ = git(&dir, &["remote", "set-head", "origin", "--auto"], &[]);
    Ok(dir)
}

/// The retention, run on each make: preserved work older than
/// `preserve_days`, and branches not checked out whose head the remote
/// already has.
fn prune(cfg: &Config) {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let keep = cfg.preserve_days.saturating_mul(86_400);
    for source in sources(cfg) {
        let preserved = git(
            &source,
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
                let _ = git(&source, &["update-ref", "-d", name], &[]);
            }
        }
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
    if std::env::var_os("GIT_SSH_COMMAND").is_none() && std::env::var_os("GIT_SSH").is_none() {
        cmd.env("GIT_SSH_COMMAND", "ssh -o BatchMode=yes");
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
        let path = PathBuf::from(decode(rest));
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
        decode(rest)
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
            },
            Request::Git {
                remote: "b".into(),
                name: "app".into(),
                branch: None,
                base: None,
            },
        ];
        dedupe_names(&mut requests);
        assert!(matches!(&requests[1], Request::Git { name, .. } if name == "app-2"));
    }
}
