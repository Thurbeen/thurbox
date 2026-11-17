//! The manager's own files in a data directory: `manager.lock`, which a
//! serving manager holds for its whole life, and `manager.json`, the status
//! it reports. The lock is the authority on whether a manager runs; the
//! record never is.

use std::fs::{File, OpenOptions, TryLockError};
use std::io;
use std::path::{Path, PathBuf};

use fleet_api::{Millis, StoreError};
use serde::{Deserialize, Serialize};

use crate::{FileStore, durable, io_unknown};

pub const LOCK: &str = "manager.lock";
pub const RECORD: &str = "manager.json";

/// An exclusive OS lock on `manager.lock`. The OS releases it when the
/// holder exits, however it exits; the descriptor is close-on-exec, so no
/// child keeps it.
#[derive(Debug)]
pub struct ManagerLock {
    _file: File,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ManagerState {
    Starting,
    Serving,
    Stopped,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ManagerRecord {
    pub pid: u32,
    pub boot: Option<String>,
    pub version: String,
    pub exe: PathBuf,
    pub config: PathBuf,
    pub started_at: Millis,
    pub state: ManagerState,
}

impl FileStore {
    /// Take `manager.lock` without waiting. `Ok(None)`: another process
    /// holds it.
    pub fn lock_manager(&self) -> Result<Option<ManagerLock>, StoreError> {
        let (path, file) = self.open_manager_lock()?;
        match file.try_lock() {
            Ok(()) => Ok(Some(ManagerLock { _file: file })),
            Err(TryLockError::WouldBlock) => Ok(None),
            Err(TryLockError::Error(e)) => Err(io_unknown(&path, e)),
        }
    }

    /// Take `manager.lock`, waiting for as long as another process holds
    /// it. `waiting` runs once, when the wait starts.
    pub fn wait_for_manager_lock(&self, waiting: impl FnOnce()) -> Result<ManagerLock, StoreError> {
        if let Some(lock) = self.lock_manager()? {
            return Ok(lock);
        }
        waiting();
        let (path, file) = self.open_manager_lock()?;
        file.lock().map_err(|e| io_unknown(&path, e))?;
        Ok(ManagerLock { _file: file })
    }

    fn open_manager_lock(&self) -> Result<(PathBuf, File), StoreError> {
        let path = self.root.join(LOCK);
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .map_err(|e| io_unknown(&path, e))?;
        Ok((path, file))
    }

    /// Replace `manager.json`. Only the holder of `manager.lock` writes it.
    pub fn record_manager(
        &self,
        _held: &ManagerLock,
        record: &ManagerRecord,
    ) -> Result<(), StoreError> {
        let _guard = self.lock()?;
        let path = self.root.join(RECORD);
        let bytes =
            serde_json::to_vec_pretty(record).map_err(|e| StoreError::Invalid(e.to_string()))?;
        durable::replace(&self.tmp_dir(), &path, &bytes).map_err(|e| io_unknown(&path, e))
    }

    /// The manager record of the store at `root`, read without opening the
    /// store. `Ok(None)`: no manager has run.
    pub fn peek_manager(root: &Path) -> Result<Option<ManagerRecord>, StoreError> {
        let path = root.join(RECORD);
        match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes).map(Some).map_err(|e| {
                StoreError::Unknown(format!("{}: corrupt manager record: {e}", path.display()))
            }),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(io_unknown(&path, e)),
        }
    }
}
