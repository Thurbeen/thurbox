//! `fleet integration`'s I/O on this machine: the agent's config directory
//! as the agent finds it, the ledger under Fleet's data directory, and each
//! write. What is Fleet's and what state it is in is decided in
//! `fleet_api::integration`; this module reads and writes only.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use fleet_api::harness::Profile;
use fleet_api::integration::{
    self as api, Entry, LEDGER, Ledger, Observed, State, WrittenBlock, WrittenFile,
};

/// This machine's platform, for [`api::supported`].
const PLATFORM: api::Platform = if cfg!(windows) {
    api::Platform::Windows
} else {
    api::Platform::Unix
};

/// One agent's integration as `status` reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    pub state: State,
    /// The config directory the agent reads now, when one can be named.
    pub dir: Option<PathBuf>,
    /// The version installed, from the ledger.
    pub installed: Option<u32>,
}

/// What `install` did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Installed {
    Written,
    /// It was current: nothing was written.
    Unchanged,
}

/// The agent's config directory, as the agent finds it from `env`.
pub fn config_dir(profile: &Profile, env: &dyn Fn(&str) -> Option<String>) -> Option<PathBuf> {
    let install = profile.install.as_ref()?;
    let home = if cfg!(windows) {
        env("USERPROFILE")
    } else {
        env("HOME")
    };
    let dir = PathBuf::from(api::config_dir(install, env, home.as_deref())?);
    // Recorded and compared as an absolute path: a relative one names
    // another directory from each working directory.
    if dir.is_relative() {
        return std::env::current_dir().ok().map(|cwd| cwd.join(dir));
    }
    Some(dir)
}

/// The ledger's lock: one `install` or `uninstall` at a time, so two never
/// read the same ledger and the last write drops the other's entry. It is
/// the operating system's advisory lock on a file (`flock`, `LockFileEx`),
/// so it goes with its holder: a killed command leaves the file, not the
/// lock. The file stays, and is never what is locked against.
struct LedgerLock(#[allow(dead_code, reason = "held for its lock")] fs::File);

impl LedgerLock {
    fn take(data_dir: &Path) -> Result<LedgerLock, String> {
        let path = data_dir.join(LOCK);
        let file = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .map_err(|e| format!("{}: {e}", path.display()))?;
        for _ in 0..100 {
            match file.try_lock() {
                Ok(()) => return Ok(LedgerLock(file)),
                Err(fs::TryLockError::WouldBlock) => {
                    std::thread::sleep(std::time::Duration::from_millis(50));
                }
                Err(fs::TryLockError::Error(e)) => {
                    return Err(format!("{}: {e}", path.display()));
                }
            }
        }
        Err(format!(
            "{} is locked: another `fleet integration` is running; run this again once it ends",
            path.display()
        ))
    }
}

/// The ledger lock's file name, beside the ledger.
pub const LOCK: &str = "integrations.lock";

/// The ledger under `data_dir`; none written is an empty one.
pub fn read_ledger(data_dir: &Path) -> io::Result<Ledger> {
    let path = data_dir.join(LEDGER);
    match fs::read(&path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map_err(|e| io::Error::other(format!("{}: {e}", path.display()))),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Ledger::default()),
        Err(e) => Err(e),
    }
}

fn write_ledger(data_dir: &Path, ledger: &Ledger) -> io::Result<()> {
    fs::create_dir_all(data_dir)?;
    let text = serde_json::to_vec_pretty(ledger).map_err(io::Error::other)?;
    write_atomic(&data_dir.join(LEDGER), &text)
}

/// What is on disk for `entry`, in its directory; with no entry, whether
/// `dir` is there.
fn observe(name: &str, entry: Option<&Entry>, dir: Option<&Path>) -> io::Result<Observed> {
    let Some(entry) = entry else {
        return Ok(Observed {
            dir_exists: dir.is_some_and(Path::is_dir),
            found: Default::default(),
        });
    };
    let root = Path::new(&entry.dir);
    let mut found = std::collections::BTreeMap::new();
    for f in &entry.files {
        let now = match fs::read(root.join(&f.path)) {
            Ok(bytes) => Some(api::hash(&bytes)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => None,
            Err(e) => return Err(e),
        };
        found.insert(f.path.clone(), now);
    }
    for b in &entry.blocks {
        let now = match fs::read_to_string(root.join(&b.path)) {
            // A block that lost a comment line is no longer as Fleet wrote
            // it: modified.
            Ok(text) => match api::find_block(&text, name, &b.comment) {
                Ok(range) => range.map(|r| api::hash(text[r].as_bytes())),
                Err(_) => Some("broken".into()),
            },
            Err(e) if e.kind() == io::ErrorKind::NotFound => None,
            Err(e) => return Err(e),
        };
        found.insert(b.path.clone(), now);
    }
    Ok(Observed {
        dir_exists: root.is_dir(),
        found,
    })
}

/// The state of `profile`'s integration on this machine.
pub fn status(
    profile: &Profile,
    data_dir: &Path,
    env: &dyn Fn(&str) -> Option<String>,
) -> io::Result<Report> {
    let ledger = read_ledger(data_dir)?;
    let entry = ledger.integrations.get(&profile.name);
    let dir = config_dir(profile, env);
    let observed = observe(&profile.name, entry, dir.as_deref())?;
    let resolved = dir.as_ref().map(|d| d.to_string_lossy().into_owned());
    Ok(Report {
        state: api::state_on(profile, entry, resolved.as_deref(), &observed, PLATFORM),
        dir,
        installed: entry.map(|e| e.version),
    })
}

/// Write `profile`'s integration into the agent's config directory, after
/// removing an older one. The ledger records what will be written before
/// any of it is, so an interrupted install is removed whole.
pub fn install(
    profile: &Profile,
    data_dir: &Path,
    env: &dyn Fn(&str) -> Option<String>,
) -> Result<Installed, String> {
    if let Some(install) = &profile.install {
        api::supported(install, PLATFORM).map_err(|e| format!("{}: {e}", profile.name))?;
    }
    let _lock = LedgerLock::take(data_dir)?;
    let report = status(profile, data_dir, env).map_err(|e| e.to_string())?;
    let Some(install) = &profile.install else {
        return Err(match report.state {
            State::NotNeeded => format!(
                "{}: not needed: launch-time hooks report its state",
                profile.name
            ),
            _ => format!("{}: Fleet knows no hooks for this harness", profile.name),
        });
    };
    match report.state {
        State::Current => return Ok(Installed::Unchanged),
        State::Modified => {
            return Err(format!(
                "{}: modified: a file or block Fleet wrote was changed since; `fleet integration uninstall {}` removes it, then install again",
                profile.name, profile.name
            ));
        }
        _ => {}
    }
    let dir = report.dir.ok_or_else(|| {
        format!(
            "{}: no config directory: set HOME or one of {:?}",
            profile.name, install.dir_env
        )
    })?;
    if !dir.is_dir() {
        return Err(format!(
            "{}: agent-missing: {} is not there, and Fleet never creates an agent's config directory; start the agent once",
            profile.name,
            dir.display()
        ));
    }
    let mut ledger = read_ledger(data_dir).map_err(|e| e.to_string())?;
    let old = ledger.integrations.get(&profile.name).cloned();
    // Everything that would stop the new install is checked while the old
    // one still works: it is removed only once the new one can be written.
    // Owning a block in a shared file is not owning the file: a whole file
    // is Fleet's to replace only when the old entry wrote it whole.
    let same_dir = old.as_ref().filter(|o| Path::new(&o.dir) == dir);
    let our_file = |rel: &str| same_dir.is_some_and(|o| o.files.iter().any(|f| f.path == rel));
    let our_block = |rel: &str| same_dir.is_some_and(|o| o.blocks.iter().any(|b| b.path == rel));
    for f in &install.files {
        let path = dir.join(&f.path);
        if fs::symlink_metadata(&path).is_ok() && !our_file(&f.path) {
            return Err(format!(
                "{}: {} is there and not Fleet's: move it, then install again",
                profile.name,
                path.display()
            ));
        }
    }
    for b in &install.blocks {
        let path = dir.join(&b.path);
        let text = match fs::read_to_string(&path) {
            Ok(text) => text,
            Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
            Err(e) => return Err(format!("{}: {e}", path.display())),
        };
        match api::find_block(&text, &profile.name, &b.comment) {
            Err(e) => return Err(format!("{}: {}: {e}", profile.name, path.display())),
            Ok(Some(_)) if !our_block(&b.path) => {
                return Err(format!(
                    "{}: {} already holds a block naming Fleet that the ledger does not list: remove it, then install again",
                    profile.name,
                    path.display()
                ));
            }
            Ok(_) => {}
        }
    }
    if let Some(old) = &old {
        remove(&profile.name, old).map_err(|e| format!("{}: {e}", old.dir))?;
        ledger.integrations.remove(&profile.name);
        write_ledger(data_dir, &ledger).map_err(|e| e.to_string())?;
    }

    // What will be written, recorded before any of it is.
    let version = install.version;
    let mut entry = Entry {
        version,
        dir: dir.to_string_lossy().into_owned(),
        files: Vec::new(),
        blocks: Vec::new(),
        dirs: Vec::new(),
    };
    let mut files = Vec::new();
    for f in &install.files {
        let path = dir.join(&f.path);
        let text = api::file_text(&f.text, version);
        entry.files.push(WrittenFile {
            path: f.path.clone(),
            hash: api::hash(text.as_bytes()),
        });
        for d in missing_dirs(&dir, &path) {
            if !entry.dirs.contains(&d) {
                entry.dirs.push(d);
            }
        }
        files.push((path, text));
    }
    let mut blocks = Vec::new();
    for b in &install.blocks {
        let path = dir.join(&b.path);
        let current = match fs::read_to_string(&path) {
            Ok(text) => Some(text),
            Err(e) if e.kind() == io::ErrorKind::NotFound => None,
            Err(e) => return Err(format!("{}: {e}", path.display())),
        };
        let user = current.as_deref().unwrap_or_default();
        let block = api::block_text(&profile.name, b, version);
        let (text, prefix) = api::append_block(user, &block);
        entry.blocks.push(WrittenBlock {
            path: b.path.clone(),
            hash: api::hash(block.as_bytes()),
            comment: b.comment.clone(),
            prefix,
            created_file: current.is_none(),
        });
        for d in missing_dirs(&dir, &path) {
            if !entry.dirs.contains(&d) {
                entry.dirs.push(d);
            }
        }
        blocks.push((path, text));
    }
    ledger
        .integrations
        .insert(profile.name.clone(), entry.clone());
    write_ledger(data_dir, &ledger).map_err(|e| e.to_string())?;

    let wrote = || -> io::Result<()> {
        for d in &entry.dirs {
            fs::create_dir(Path::new(&entry.dir).join(d))?;
        }
        for (path, text) in files.iter().chain(&blocks) {
            write_atomic(path, text.as_bytes())?;
        }
        Ok(())
    };
    wrote().map_err(|e| {
        format!(
            "{}: {e}; `fleet integration uninstall {}` removes what was written",
            profile.name, profile.name
        )
    })?;
    Ok(Installed::Written)
}

/// Every directory between `dir` and `path`'s parent that is not there,
/// outermost first, relative to `dir`.
fn missing_dirs(dir: &Path, path: &Path) -> Vec<String> {
    let mut missing = Vec::new();
    let mut at = path.parent();
    while let Some(d) = at {
        if d == dir || d.exists() {
            break;
        }
        if let Ok(rel) = d.strip_prefix(dir) {
            missing.push(rel.to_string_lossy().into_owned());
        }
        at = d.parent();
    }
    missing.reverse();
    missing
}

/// Remove `name`'s integration: exactly what the ledger lists. `false` when
/// none is installed.
pub fn uninstall(name: &str, data_dir: &Path) -> Result<bool, String> {
    let _lock = LedgerLock::take(data_dir)?;
    let mut ledger = read_ledger(data_dir).map_err(|e| e.to_string())?;
    let Some(entry) = ledger.integrations.get(name).cloned() else {
        return Ok(false);
    };
    remove(name, &entry).map_err(|e| format!("{name}: {e}"))?;
    ledger.integrations.remove(name);
    write_ledger(data_dir, &ledger).map_err(|e| e.to_string())?;
    Ok(true)
}

/// The names of every integration the ledger lists.
pub fn installed(data_dir: &Path) -> io::Result<Vec<String>> {
    Ok(read_ledger(data_dir)?.integrations.into_keys().collect())
}

/// Remove what `entry` wrote, whatever of it is still there: an own file,
/// Fleet's block with what Fleet put before it, a file Fleet created that
/// the block alone held, then each directory Fleet created that is empty.
fn remove(name: &str, entry: &Entry) -> io::Result<()> {
    let root = Path::new(&entry.dir);
    for f in &entry.files {
        match fs::remove_file(root.join(&f.path)) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
            _ => {}
        }
    }
    for b in &entry.blocks {
        let path = root.join(&b.path);
        let text = match fs::read_to_string(&path) {
            Ok(text) => text,
            Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e),
        };
        let range = api::find_block(&text, name, &b.comment).map_err(|e| {
            io::Error::other(format!(
                "{}: {e}: remove the block by hand, then uninstall again",
                path.display()
            ))
        })?;
        let Some(range) = range else {
            continue;
        };
        let rest = api::remove_block(&text, range, &b.prefix);
        if rest.is_empty() && b.created_file {
            fs::remove_file(&path)?;
        } else {
            write_atomic(&path, rest.as_bytes())?;
        }
    }
    for d in entry.dirs.iter().rev() {
        let path = root.join(d);
        let empty = fs::read_dir(&path).is_ok_and(|mut e| e.next().is_none());
        if empty {
            fs::remove_dir(&path)?;
        }
    }
    Ok(())
}

static SEQ: AtomicU64 = AtomicU64::new(0);

/// Write `bytes` to `path` whole: through a symlink to its real target,
/// never over a file with other hard links or one that is not a regular
/// file, and keeping an existing file's permissions; a new file, and the
/// temporary one before it gets them, is its owner's alone. Elsewhere than
/// Windows a file in the same directory is renamed over the target, so a
/// reader sees the old bytes or the new. On Windows a rename can lose the
/// target's ACL, so the file is written in place with a backup kept until
/// the write is done.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let target = match fs::symlink_metadata(path) {
        Ok(_) => fs::canonicalize(path)?,
        Err(e) if e.kind() == io::ErrorKind::NotFound => path.to_path_buf(),
        Err(e) => return Err(e),
    };
    let existing = match fs::metadata(&target) {
        Ok(m) => Some(m),
        Err(e) if e.kind() == io::ErrorKind::NotFound => None,
        Err(e) => return Err(e),
    };
    if let Some(m) = &existing {
        if !m.is_file() {
            return Err(io::Error::other(format!(
                "{}: not a regular file",
                target.display()
            )));
        }
        if links(m) > 1 {
            return Err(io::Error::other(format!(
                "{}: has other hard links, which a write would split from it",
                target.display()
            )));
        }
        if cfg!(windows) {
            return write_with_backup(&target, bytes);
        }
    }
    let parent = target.parent().unwrap_or(Path::new("."));
    let tmp = parent.join(format!(
        ".fleet-{}-{}.tmp",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    let written = create_private(&tmp)
        .and_then(|mut file| io::Write::write_all(&mut file, bytes))
        .and_then(|()| match &existing {
            Some(m) => fs::set_permissions(&tmp, m.permissions()),
            None => Ok(()),
        })
        .and_then(|()| fs::rename(&tmp, &target));
    if written.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    written
}

/// A new file only its owner can read until its permissions are set: the
/// bytes it receives are a config's, which may be private.
fn create_private(path: &Path) -> io::Result<fs::File> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    options.open(path)
}

#[cfg(unix)]
fn links(m: &fs::Metadata) -> u64 {
    std::os::unix::fs::MetadataExt::nlink(m)
}

/// Stable Rust reads no link count on Windows.
#[cfg(not(unix))]
fn links(_: &fs::Metadata) -> u64 {
    1
}

/// `<file>.fleet-backup`, and the copy that becomes it.
fn backups(target: &Path) -> (PathBuf, PathBuf) {
    let mut backup = target.as_os_str().to_os_string();
    backup.push(".fleet-backup");
    let mut pending = backup.clone();
    pending.push(".pending");
    (PathBuf::from(backup), PathBuf::from(pending))
}

/// Write an existing file in place, a backup of it kept until the write is
/// done. A backup left by an earlier write stops this one: it may be the
/// only good copy.
fn write_with_backup(target: &Path, bytes: &[u8]) -> io::Result<()> {
    let (backup, pending) = backups(target);
    for left in [&backup, &pending] {
        if fs::symlink_metadata(left).is_ok() {
            return Err(io::Error::other(format!(
                "{} is left from an interrupted write: compare it with {}, keep the right one, and remove it",
                left.display(),
                target.display()
            )));
        }
    }
    fs::copy(target, &pending)?;
    fs::rename(&pending, &backup)?;
    fs::write(target, bytes)?;
    fs::remove_file(&backup)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("fleet-integration-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_write_replaces_the_file_whole_and_leaves_no_temp_file() {
        let dir = scratch("whole");
        let file = dir.join("f");
        write_atomic(&file, b"one").unwrap();
        write_atomic(&file, b"two").unwrap();
        assert_eq!(fs::read(&file).unwrap(), b"two");
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 1);
        assert!(write_atomic(&dir, b"x").is_err(), "a directory is no file");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn a_write_keeps_permissions_follows_a_symlink_and_refuses_a_hard_link() {
        use std::os::unix::fs::PermissionsExt;
        let dir = scratch("unix");
        let real = dir.join("real");
        fs::write(&real, "old").unwrap();
        fs::set_permissions(&real, fs::Permissions::from_mode(0o600)).unwrap();
        let link = dir.join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        write_atomic(&link, b"new").unwrap();
        assert!(
            fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(fs::read(&real).unwrap(), b"new");
        let mode = fs::metadata(&real).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);

        let new = dir.join("new");
        write_atomic(&new, b"x").unwrap();
        let mode = fs::metadata(&new).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "a new file is its owner's alone");

        let hard = dir.join("hard");
        fs::hard_link(&real, &hard).unwrap();
        assert!(write_atomic(&real, b"x").is_err());
        assert_eq!(fs::read(&hard).unwrap(), b"new");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn a_windows_write_keeps_a_backup_until_done_and_stops_at_a_left_one() {
        let dir = scratch("windows");
        let file = dir.join("config.toml");
        fs::write(&file, "old").unwrap();
        write_atomic(&file, b"new").unwrap();
        assert_eq!(fs::read(&file).unwrap(), b"new");
        let (backup, pending) = backups(&file);
        assert!(!backup.exists() && !pending.exists());
        fs::write(&backup, "old").unwrap();
        assert!(write_atomic(&file, b"newer").is_err());
        assert_eq!(fs::read(&file).unwrap(), b"new");
        fs::remove_dir_all(&dir).unwrap();
    }
}
