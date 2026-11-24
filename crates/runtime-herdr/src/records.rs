use std::fs::{self, File, OpenOptions};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const LOCK_WAIT: Duration = Duration::from_secs(10);
const START_SUFFIX: &str = ".start";
const EXIT_SUFFIX: &str = ".exit";

#[derive(Serialize, Deserialize)]
pub struct Started {
    pub pane: String,
    pub terminal: Option<String>,
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

fn server_digest(socket: &Path) -> String {
    let digest = Sha256::digest(socket.as_os_str().as_encoded_bytes());
    digest[..8].iter().map(|b| format!("{b:02x}")).collect()
}

impl Records {
    pub fn for_server(socket: &Path) -> Result<Records, String> {
        let base = state_home().ok_or("neither XDG_STATE_HOME nor HOME is set")?;
        let dir = base.join("runtime-herdr").join(server_digest(socket));
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

    pub fn exit_file(&self, key: &str) -> PathBuf {
        self.path(key, EXIT_SUFFIX)
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

    pub fn exit_code(&self, key: &str) -> Option<i64> {
        fs::read_to_string(self.exit_file(key))
            .ok()
            .and_then(|code| code.trim().parse().ok())
    }

    pub fn forget_exit(&self, key: &str) -> Result<(), String> {
        remove(&self.exit_file(key))
    }

    pub fn forget(&self, key: &str) -> Result<(), String> {
        for suffix in [START_SUFFIX, EXIT_SUFFIX] {
            remove(&self.path(key, suffix))?;
        }
        Ok(())
    }

    pub fn keys(&self) -> Result<Vec<String>, String> {
        let entries = fs::read_dir(&self.dir)
            .map_err(|e| format!("cannot list {}: {e}", self.dir.display()))?;
        let mut keys: Vec<String> = entries
            .filter_map(|entry| entry.ok()?.file_name().into_string().ok())
            .filter_map(|name| name.strip_suffix(START_SUFFIX).map(String::from))
            .collect();
        keys.sort();
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
