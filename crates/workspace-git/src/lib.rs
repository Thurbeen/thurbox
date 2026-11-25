use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const SCHEMES: &[&str] = &["git+https", "git+ssh", "git+file", "file"];

pub const DEFAULT_PRESERVE_DAYS: u64 = 30;

const RECORD: &str = ".fleet-workspace.json";

const KEY_SOURCES: &str = ".fleet-sources";

const CHECKOUTS: &str = ".checkouts";

const PRESERVED: &str = "refs/fleet/preserved";

const FETCH_DEADLINE: std::time::Duration = std::time::Duration::from_secs(30);

const MAX_KEY: usize = 128;

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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Answer {
    Made { dirs: Vec<String>, adopted: bool },
    Unsupported { url: String },
    Failed { why: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Request {
    Dir(PathBuf),
    Git {
        remote: String,
        name: String,
        branch: Option<String>,
        base: Option<String>,
        local: bool,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Record {
    urls: Vec<String>,
    dirs: Vec<String>,
    trees: Vec<Tree>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Tree {
    name: String,
    source: String,
    path: String,
}

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

pub fn release(cfg: &Config, key: &str) -> Result<(), String> {
    check_key(key)?;
    let dir = cfg.root.join(key);
    let Some(record) = read_record(&dir)? else {
        return rollback(cfg, key);
    };
    for tree in &record.trees {
        let source = Path::new(&tree.source);
        let _lock = lock_repository(cfg, source)?;
        let path = Path::new(&tree.path);
        if is_worktree(path)? {
            preserve(path, key, &tree.name)?;
        }
        remove_worktree(source, path)?;
    }
    remove_dir(&dir)
}

pub fn discard(cfg: &Config, key: &str) -> Result<(), String> {
    check_key(key)?;
    rollback(cfg, key)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Workspace {
    pub urls: Vec<String>,
    pub dirs: Vec<String>,
}

pub fn inspect(cfg: &Config, key: &str) -> Result<Option<Workspace>, String> {
    check_key(key)?;
    Ok(read_record(&cfg.root.join(key))?.map(|r| Workspace {
        urls: r.urls,
        dirs: r.dirs,
    }))
}

fn remove_dir(dir: &Path) -> Result<(), String> {
    let mut result = fs::remove_dir_all(dir);
    if result
        .as_ref()
        .is_err_and(|e| e.kind() == std::io::ErrorKind::PermissionDenied)
    {
        grant_owner_write_without_following_links(dir);
        result = fs::remove_dir_all(dir);
    }
    match result {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(format!("cannot remove {}: {e}", dir.display())),
    }
}

fn grant_owner_write_without_following_links(dir: &Path) {
    let Ok(meta) = fs::symlink_metadata(dir) else {
        return;
    };
    if !meta.is_dir() {
        return;
    }
    let mut permissions = meta.permissions();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        permissions.set_mode(permissions.mode() | 0o200);
    }
    #[cfg(not(unix))]
    #[allow(clippy::permissions_set_readonly_false)]
    permissions.set_readonly(false);
    let _ = fs::set_permissions(dir, permissions);
    for entry in fs::read_dir(dir).into_iter().flatten().flatten() {
        grant_owner_write_without_following_links(&entry.path());
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

fn is_worktree(path: &Path) -> Result<bool, String> {
    let dot_git = path.join(".git");
    if dot_git.is_dir() {
        return Ok(true);
    }
    let gitfile = match fs::read_to_string(&dot_git) {
        Ok(gitfile) => gitfile,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(format!("cannot read {}: {e}", dot_git.display())),
    };
    let gitdir = gitfile
        .trim_end()
        .strip_prefix("gitdir: ")
        .map(|gitdir| path.join(gitdir))
        .ok_or_else(|| format!("{} names no git directory", dot_git.display()))?;
    match fs::metadata(&gitdir) {
        Ok(_) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(format!("cannot read {}: {e}", gitdir.display())),
    }
}

fn remove_worktree(source: &Path, path: &Path) -> Result<(), String> {
    if is_worktree(path)? {
        if let Err(first) = git_remove_worktree(source, path) {
            remove_dir(path)
                .map_err(|second| format!("{first}; removing what is left: {second}"))?;
        }
    } else {
        remove_dir(path)?;
    }
    git(source, &["worktree", "prune"], &[]).map(|_| ())
}

fn git_remove_worktree(source: &Path, path: &Path) -> Result<String, String> {
    let path = path.display().to_string();
    git(
        source,
        &["worktree", "remove", "--force", "--force", &path],
        &[],
    )
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

fn preserve(tree: &Path, key: &str, name: &str) -> Result<(), String> {
    let status = git(
        tree,
        &["status", "--porcelain", "--untracked-files=all"],
        &[],
    )?;
    if status.is_empty() {
        return Ok(());
    }
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
    let _lock = lock_repository(cfg, &source)?;
    let key_dir = cfg.root.join(key);
    fs::create_dir_all(&key_dir).map_err(|e| format!("cannot make {}: {e}", key_dir.display()))?;
    if checkout.is_some() {
        list_path(&cfg.root.join(CHECKOUTS), &source)?;
        fetch_upstream(&source, FETCH_DEADLINE);
    } else {
        fill_source(&source, remote)?;
    }
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
    if base.starts_with("origin/") {
        candidates.insert(0, format!("refs/remotes/{base}"));
    }
    candidates.push(base.to_string());
    for candidate in candidates {
        if let Some(commit) = verify(source, &candidate) {
            return Ok(commit);
        }
    }
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

fn free_branch(source: &Path, branch: &str) -> Result<String, String> {
    (2..1000)
        .map(|n| format!("{branch}-{n}"))
        .find(|b| !has_ref(source, &format!("refs/heads/{b}")))
        .ok_or_else(|| format!("no free branch name after {branch}"))
}

fn digest(text: &str) -> String {
    Sha256::digest(text.as_bytes())
        .iter()
        .take(8)
        .map(|b| format!("{b:02x}"))
        .collect()
}

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

fn lock_repository(cfg: &Config, repository: &Path) -> Result<fs::File, String> {
    let file = lock_file(cfg, repository)?;
    file.lock()
        .map_err(|e| format!("cannot lock {}: {e}", repository.display()))?;
    Ok(file)
}

fn try_lock_repository(cfg: &Config, repository: &Path) -> Option<fs::File> {
    let file = lock_file(cfg, repository).ok()?;
    file.try_lock().ok()?;
    Some(file)
}

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
        if fresh {
            let _ = fs::remove_dir_all(dir);
        }
        return Err(why);
    }
    let _ = git(dir, &["remote", "set-head", "origin", "--auto"], &[]);
    Ok(())
}

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

fn reaches_remote(args: &[&str]) -> bool {
    args.iter()
        .any(|arg| matches!(*arg, "fetch" | "set-head" | "clone" | "ls-remote"))
}

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
        // git clones a local path, a non-bare checkout included.
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
