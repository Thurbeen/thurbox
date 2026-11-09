//! The three durable primitives underneath the store (P2, Q11, F12):
//! exclusive create, atomic replace, and the store lock. Every byte reaches
//! disk through a temp file that is fsynced before it is linked or renamed
//! into place, so a reader sees the old record or the new one, never a torn
//! one.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::Duration;

static TEMP_SEQ: AtomicU64 = AtomicU64::new(0);

/// Write `bytes` to a fresh temp file in `tmp_dir` and fsync it.
pub fn write_temp(tmp_dir: &Path, bytes: &[u8]) -> io::Result<PathBuf> {
    let n = TEMP_SEQ.fetch_add(1, Ordering::Relaxed);
    let path = tmp_dir.join(format!("{}-{n}.tmp", std::process::id()));
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut f = options.open(&path)?;
    let written = f.write_all(bytes).and_then(|()| f.sync_all());
    if let Err(e) = written {
        drop(f);
        let _ = fs::remove_file(&path);
        return Err(e);
    }
    Ok(path)
}

/// Create `path` with `bytes` only if it does not exist. The content is
/// complete before the name appears: a hard link of a synced temp file is
/// atomic and fails if the name is taken, on Unix and on NTFS alike.
/// Returns false when the name already existed.
pub fn create_exclusive(tmp_dir: &Path, path: &Path, bytes: &[u8]) -> io::Result<bool> {
    let tmp = write_temp(tmp_dir, bytes)?;
    let linked = fs::hard_link(&tmp, path);
    let _ = fs::remove_file(&tmp);
    match linked {
        Ok(()) => {
            sync_parent(path)?;
            Ok(true)
        }
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => Ok(false),
        Err(e) => Err(e),
    }
}

/// Replace `path` with `bytes` atomically and durably.
pub fn replace(tmp_dir: &Path, path: &Path, bytes: &[u8]) -> io::Result<()> {
    let tmp = write_temp(tmp_dir, bytes)?;
    if let Err(e) = rename_retrying(&tmp, path) {
        let _ = fs::remove_file(&tmp);
        return Err(e);
    }
    sync_parent(path)
}

/// Remove `path` durably. Absent is fine: removal is idempotent.
pub fn remove(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Ok(()) => sync_parent(path),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

/// On Windows a rename onto a file another process has open fails with a
/// sharing violation until that reader closes it. Readers hold files only
/// for one read, so a short bounded retry absorbs the race; past the bound
/// the error stands.
fn rename_retrying(from: &Path, to: &Path) -> io::Result<()> {
    let mut tries = 0;
    loop {
        match fs::rename(from, to) {
            Ok(()) => return Ok(()),
            Err(e)
                if e.kind() == io::ErrorKind::PermissionDenied && cfg!(windows) && tries < 50 =>
            {
                tries += 1;
                thread::sleep(Duration::from_millis(20));
            }
            Err(e) => return Err(e),
        }
    }
}

/// fsync the directory so the new name survives a crash. Windows has no
/// directory handle to sync; NTFS journals the rename itself.
fn sync_parent(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        if let Some(dir) = path.parent() {
            File::open(dir)?.sync_all()?;
        }
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

/// The store lock: one exclusive OS file lock, held for one read-check-write.
/// It is released when the guard drops, and by the OS when a holder dies.
///
/// A waiter blocks in the kernel rather than polling against a deadline:
/// every holder is bounded (one durable write) and a dead one frees the lock,
/// so a deadline only ever turned a slow, loaded machine into spurious Busy
/// failures.
#[derive(Debug)]
pub struct LockGuard {
    file: File,
}

impl LockGuard {
    pub fn acquire(path: &Path) -> io::Result<LockGuard> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)?;
        file.lock()?;
        Ok(LockGuard { file })
    }
}

impl Drop for LockGuard {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}
