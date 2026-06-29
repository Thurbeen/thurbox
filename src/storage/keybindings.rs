//! File IO for the user keybindings JSON file.
//!
//! Lives outside SQLite because the file is hand-edited (e.g. via `$EDITOR`)
//! and version-controlled by the user. The TUI reads it once at startup and
//! falls back to defaults when the file is missing or malformed.

use std::fs;
use std::io;
use std::path::PathBuf;

use crate::paths;

/// Read the keybindings JSON from disk, or `Ok(None)` if the file doesn't
/// exist. Errors only on IO failure or unresolved path.
pub fn load_keybindings_json() -> Result<Option<String>, String> {
    let path = path()?;
    match fs::read_to_string(&path) {
        Ok(s) => Ok(Some(s)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("read {}: {e}", path.display())),
    }
}

/// Write the keybindings JSON to disk, creating parent directories as needed.
pub fn save_keybindings_json(json: &str) -> Result<(), String> {
    let path = path()?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("create {}: {e}", parent.display()))?;
    }
    fs::write(&path, json).map_err(|e| format!("write {}: {e}", path.display()))
}

/// Remove the keybindings JSON file (resets to defaults). Returns `Ok(())`
/// if the file is already absent.
pub fn delete_keybindings_json() -> Result<(), String> {
    let path = path()?;
    match fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(format!("delete {}: {e}", path.display())),
    }
}

fn path() -> Result<PathBuf, String> {
    paths::keybindings_file().ok_or_else(|| "could not resolve keybindings file path".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paths::TestPathGuard;

    #[test]
    fn load_returns_none_when_file_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let _g = TestPathGuard::new(tmp.path());
        assert_eq!(load_keybindings_json().unwrap(), None);
    }

    #[test]
    fn save_then_load_round_trip() {
        let tmp = tempfile::tempdir().unwrap();
        let _g = TestPathGuard::new(tmp.path());
        let body = r#"{"QuitApp": ["ctrl+a"]}"#;
        save_keybindings_json(body).unwrap();
        assert_eq!(load_keybindings_json().unwrap().as_deref(), Some(body));
    }

    #[test]
    fn delete_is_idempotent() {
        let tmp = tempfile::tempdir().unwrap();
        let _g = TestPathGuard::new(tmp.path());
        delete_keybindings_json().unwrap();
        save_keybindings_json("{}").unwrap();
        delete_keybindings_json().unwrap();
        assert_eq!(load_keybindings_json().unwrap(), None);
    }
}
