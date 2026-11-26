use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::Duration;

static TEMP_SEQ: AtomicU64 = AtomicU64::new(0);

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

// A hard link is atomic and fails on a taken name, on Unix and on NTFS alike.
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

pub fn replace(tmp_dir: &Path, path: &Path, bytes: &[u8]) -> io::Result<()> {
    let tmp = write_temp(tmp_dir, bytes)?;
    if let Err(e) = rename_retrying(&tmp, path) {
        let _ = fs::remove_file(&tmp);
        return Err(e);
    }
    sync_parent(path)
}

pub fn overwrite_in_place(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut f = OpenOptions::new().write(true).open(path)?;
    let old = usize::try_from(f.metadata()?.len()).unwrap_or(usize::MAX);
    if bytes.len() > old {
        return Err(io::ErrorKind::StorageFull.into());
    }
    let mut padded = bytes.to_vec();
    padded.resize(old, b' ');
    f.write_all(&padded)?;
    f.sync_all()
}

pub fn remove(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Ok(()) => sync_parent(path),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

// Windows fails a rename onto a file another process has open until that reader closes it.
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

// Windows has no directory handle to sync; NTFS journals the rename itself.
pub fn sync_parent(path: &Path) -> io::Result<()> {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_overwrite_in_place_pads_to_the_old_length_and_never_grows_the_file() {
        let dir = std::env::temp_dir().join(format!("fleet-overwrite-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("manager.json");
        fs::write(&path, br#"{"state":"serving"}"#).unwrap();
        overwrite_in_place(&path, br#"{"state":"paused"}"#).unwrap();
        let written = fs::read(&path).unwrap();
        assert_eq!(written, br#"{"state":"paused"} "#);
        let longer = overwrite_in_place(&path, br#"{"state":"starting!!"}"#).unwrap_err();
        assert_eq!(longer.kind(), io::ErrorKind::StorageFull);
        assert_eq!(fs::read(&path).unwrap(), written);
        fs::remove_dir_all(dir).unwrap();
    }
}
