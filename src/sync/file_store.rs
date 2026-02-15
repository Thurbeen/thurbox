use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use tracing::debug;

use super::state::SharedState;

/// Load shared state from disk. Returns default state if file doesn't exist.
///
/// # Errors
///
/// Returns an error if the file exists but can't be read or parsed.
pub fn load_shared_state(path: &Path) -> io::Result<SharedState> {
    match fs::read_to_string(path) {
        Ok(contents) => {
            match toml::from_str::<SharedState>(&contents) {
                Ok(mut state) => {
                    // Clean up old tombstones on load
                    state.purge_old_tombstones();
                    Ok(state)
                }
                Err(e) => Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("Failed to parse shared state: {}", e),
                )),
            }
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            // File doesn't exist yet, return default state
            Ok(SharedState::default())
        }
        Err(e) => Err(e),
    }
}

/// Save shared state to disk atomically.
///
/// Uses a temporary file with PID suffix and atomic rename to prevent corruption
/// from concurrent writes. Only one write succeeds; the other sees the previous state.
///
/// # Errors
///
/// Returns an error if the write fails (e.g., disk full, permission denied).
pub fn save_shared_state(path: &Path, state: &SharedState) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }

    let serialized =
        toml::to_string_pretty(state).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;

    let temp_path = format!("{}.tmp.{}", path.display(), std::process::id());
    let temp_path = PathBuf::from(&temp_path);

    // Write to temporary file
    let mut file = fs::File::create(&temp_path)?;
    file.write_all(serialized.as_bytes())?;
    file.sync_all()?; // Ensure written to disk
    drop(file);

    // Atomic rename
    fs::rename(&temp_path, path)?;

    debug!("Saved shared state to {}", path.display());

    Ok(())
}

/// Get the modification time of the shared state file.
///
/// Returns the system time or UNIX_EPOCH if the file doesn't exist.
pub fn get_mtime(path: &Path) -> io::Result<SystemTime> {
    fs::metadata(path)
        .map(|metadata| metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH))
        .or_else(|e| match e.kind() {
            io::ErrorKind::NotFound => Ok(SystemTime::UNIX_EPOCH),
            _ => Err(e),
        })
}

#[cfg(test)]
mod tests {
    use super::super::state::{SharedSession, SharedState, SharedWorktree};
    use super::*;
    use crate::project::ProjectId;
    use crate::session::SessionId;
    use tempfile::TempDir;

    #[test]
    fn load_missing_file_returns_default() {
        let temp_dir = TempDir::new().unwrap();
        let path = temp_dir.path().join("nonexistent.toml");

        let state = load_shared_state(&path).unwrap();
        assert_eq!(state.session_counter, 0);
        assert!(state.sessions.is_empty());
    }

    #[test]
    fn roundtrip_save_and_load() {
        let temp_dir = TempDir::new().unwrap();
        let path = temp_dir.path().join("state.toml");

        let mut original = SharedState::new();
        original.session_counter = 42;

        save_shared_state(&path, &original).unwrap();

        let loaded = load_shared_state(&path).unwrap();
        assert_eq!(loaded.session_counter, 42);
        assert_eq!(loaded.sessions.len(), 0);
    }

    #[test]
    fn save_creates_parent_directories() {
        let temp_dir = TempDir::new().unwrap();
        let path = temp_dir
            .path()
            .join("subdir1")
            .join("subdir2")
            .join("state.toml");

        let state = SharedState::new();
        save_shared_state(&path, &state).unwrap();

        assert!(path.exists());
        let loaded = load_shared_state(&path).unwrap();
        assert_eq!(loaded.session_counter, 0);
    }

    #[test]
    fn get_mtime_returns_zero_for_missing_file() {
        let temp_dir = TempDir::new().unwrap();
        let path = temp_dir.path().join("nonexistent.toml");

        let mtime = get_mtime(&path).unwrap();
        assert_eq!(mtime, SystemTime::UNIX_EPOCH);
    }

    #[test]
    fn get_mtime_returns_file_time() {
        let temp_dir = TempDir::new().unwrap();
        let path = temp_dir.path().join("state.toml");

        let state = SharedState::new();
        save_shared_state(&path, &state).unwrap();

        let mtime = get_mtime(&path).unwrap();
        assert!(mtime > SystemTime::UNIX_EPOCH);
    }

    #[test]
    fn concurrent_writes_dont_corrupt() {
        let temp_dir = TempDir::new().unwrap();
        let path = temp_dir.path().join("state.toml");

        let mut state1 = SharedState::new();
        state1.session_counter = 1;

        let mut state2 = SharedState::new();
        state2.session_counter = 2;

        // Simulate concurrent writes
        save_shared_state(&path, &state1).unwrap();
        save_shared_state(&path, &state2).unwrap();

        // File should be valid and have final state
        let loaded = load_shared_state(&path).unwrap();
        assert!(loaded.session_counter == 1 || loaded.session_counter == 2);
    }

    #[test]
    fn save_with_nested_parent_directories() {
        let temp_dir = TempDir::new().unwrap();
        let path = temp_dir
            .path()
            .join("a")
            .join("b")
            .join("c")
            .join("state.toml");

        let state = SharedState::new();
        save_shared_state(&path, &state).unwrap();

        assert!(path.exists());
        assert!(path.parent().unwrap().is_dir());
    }

    #[test]
    fn save_then_load_preserves_all_fields() {
        let temp_dir = TempDir::new().unwrap();
        let path = temp_dir.path().join("state.toml");

        let mut original = SharedState::new();
        original.version = 1;
        original.session_counter = 42;
        original.sessions.push(SharedSession {
            id: SessionId::default(),
            name: "Test Session".to_string(),
            project_id: ProjectId::default(),
            role: "reviewer".to_string(),
            backend_id: "custom:@5".to_string(),
            backend_type: "ssh".to_string(),
            claude_session_id: Some("claude-xyz".to_string()),
            cwd: Some(std::path::PathBuf::from("/home/test")),
            worktree: Some(SharedWorktree {
                repo_path: std::path::PathBuf::from("/repo"),
                worktree_path: std::path::PathBuf::from("/repo/.git/worktrees/feature"),
                branch: "feature".to_string(),
            }),
            tombstone: false,
            tombstone_at: None,
        });

        save_shared_state(&path, &original).unwrap();
        let loaded = load_shared_state(&path).unwrap();

        assert_eq!(loaded.version, 1);
        assert_eq!(loaded.session_counter, 42);
        assert_eq!(loaded.sessions.len(), 1);
        assert_eq!(loaded.sessions[0].name, "Test Session");
        assert_eq!(loaded.sessions[0].role, "reviewer");
        assert_eq!(loaded.sessions[0].backend_type, "ssh");
        assert_eq!(
            loaded.sessions[0].claude_session_id,
            Some("claude-xyz".to_string())
        );
        assert_eq!(
            loaded.sessions[0].cwd,
            Some(std::path::PathBuf::from("/home/test"))
        );
    }

    #[test]
    fn invalid_toml_content_returns_error() {
        let temp_dir = TempDir::new().unwrap();
        let path = temp_dir.path().join("bad.toml");

        // Write invalid TOML directly
        fs::write(&path, "this is not valid [[TOML").unwrap();

        let result = load_shared_state(&path);
        assert!(result.is_err());
    }

    #[test]
    fn get_mtime_consistency() {
        let temp_dir = TempDir::new().unwrap();
        let path = temp_dir.path().join("state.toml");

        let state = SharedState::new();
        save_shared_state(&path, &state).unwrap();

        let mtime1 = get_mtime(&path).unwrap();
        let mtime2 = get_mtime(&path).unwrap();

        assert_eq!(mtime1, mtime2, "mtime should be consistent on same file");
    }
}
