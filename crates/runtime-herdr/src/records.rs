use std::fs::{self, File, OpenOptions};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

const LOCK_WAIT: Duration = Duration::from_secs(10);
const START_SUFFIX: &str = ".start";
const PAUSED_SUFFIX: &str = ".paused";

#[derive(Serialize, Deserialize)]
pub struct Started {
    pub terminal: Option<String>,
    pub pid: Option<u32>,
}

impl Started {
    pub fn intended() -> Started {
        Started {
            terminal: None,
            pid: None,
        }
    }
}

pub struct Records {
    dir: PathBuf,
}

pub fn state_home() -> Option<PathBuf> {
    std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/state")))
}

pub fn data_home() -> Option<PathBuf> {
    std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
}

impl Records {
    pub fn for_place(core_id: &str, place: &str) -> Result<Records, String> {
        let base = state_home().ok_or("neither XDG_STATE_HOME nor HOME is set")?;
        let dir = base.join("runtime-herdr").join(core_id).join(place);
        make_private_dir(&dir)?;
        Ok(Records { dir })
    }

    pub fn lock(&self) -> Result<File, String> {
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.dir.join("lock"))
            .map_err(|e| format!("cannot open the lock file: {e}"))?;
        let deadline = Instant::now() + LOCK_WAIT;
        loop {
            match file.try_lock() {
                Ok(()) => return Ok(file),
                Err(std::fs::TryLockError::WouldBlock) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(50));
                }
                Err(std::fs::TryLockError::WouldBlock) => {
                    return Err("another start or stop holds the lock".to_string());
                }
                Err(std::fs::TryLockError::Error(e)) => {
                    return Err(format!("cannot take the lock: {e}"));
                }
            }
        }
    }

    fn path(&self, key: &str, suffix: &str) -> PathBuf {
        self.dir.join(format!("{key}{suffix}"))
    }

    pub fn started(&self, key: &str) -> Result<Option<Started>, String> {
        match fs::read(self.path(key, START_SUFFIX)) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map(Some)
                .map_err(|e| format!("the start record of {key} does not parse: {e}")),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(format!("cannot read the start record of {key}: {e}")),
        }
    }

    pub fn record_start(&self, key: &str, started: &Started) -> Result<(), String> {
        let json = serde_json::to_vec(started).map_err(|e| e.to_string())?;
        write_atomically(&self.path(key, START_SUFFIX), &json)
    }

    pub fn forget(&self, key: &str) -> Result<(), String> {
        remove(&self.path(key, START_SUFFIX))?;
        self.unmark_paused(key)
    }

    pub fn is_paused(&self, key: &str) -> bool {
        self.path(key, PAUSED_SUFFIX).is_file()
    }

    pub fn mark_paused(&self, key: &str) -> Result<(), String> {
        write_atomically(&self.path(key, PAUSED_SUFFIX), b"")
    }

    pub fn unmark_paused(&self, key: &str) -> Result<(), String> {
        remove(&self.path(key, PAUSED_SUFFIX))
    }

    pub fn keys(&self) -> Result<Vec<String>, String> {
        let entries = fs::read_dir(&self.dir)
            .map_err(|e| format!("cannot list {}: {e}", self.dir.display()))?;
        let mut keys: Vec<String> = entries
            .filter_map(|entry| entry.ok()?.file_name().into_string().ok())
            .filter_map(|name| {
                [START_SUFFIX, PAUSED_SUFFIX]
                    .iter()
                    .find_map(|suffix| name.strip_suffix(suffix).map(String::from))
            })
            .collect();
        keys.sort();
        keys.dedup();
        Ok(keys)
    }
}

fn remove(path: &Path) -> Result<(), String> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(format!("cannot remove {}: {e}", path.display())),
    }
}

fn write_atomically(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let mut partial = path.as_os_str().to_owned();
    partial.push(".partial");
    let partial = PathBuf::from(partial);
    fs::write(&partial, bytes).map_err(|e| format!("cannot write {}: {e}", partial.display()))?;
    fs::rename(&partial, path).map_err(|e| format!("cannot write {}: {e}", path.display()))
}

fn make_private_dir(dir: &Path) -> Result<(), String> {
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
    builder
        .create(dir)
        .map_err(|e| format!("cannot make {}: {e}", dir.display()))
}
