//! Live reload of hand-edited config files.
//!
//! Grouped out of the [`App`](super::App) god object. `tick` polls the
//! mtimes of `agents.toml`, `keybindings.json`, and `settings.toml` (~1/s, a
//! few `stat` calls) and reloads them in place on change — no restart needed
//! after editing (or after the in-TUI settings panel writes the file).
//!
//! `settings.toml` reloads only re-apply the **live** feature flags (the UI
//! panel switches read from `App.features` every frame); the restart-only
//! values stay published through the write-once global so they can't drift
//! mid-frame, and the reload toast says so. Deliberately *not* reloaded live:
//! `hosts.toml` (SSH backends are registered in main's `BackendRegistry` at
//! startup). See `docs/CONFIG.md`.

use std::path::Path;
use std::time::SystemTime;

/// Last-seen mtimes of the live-reloadable config files.
#[derive(Default)]
pub(crate) struct ConfigReloadState {
    pub(crate) agents_mtime: Option<SystemTime>,
    pub(crate) keybindings_mtime: Option<SystemTime>,
    pub(crate) settings_mtime: Option<SystemTime>,
}

/// The file's modification time, `None` when it is absent/unreadable.
pub(crate) fn mtime(path: Option<&Path>) -> Option<SystemTime> {
    path.and_then(|p| std::fs::metadata(p).ok())
        .and_then(|m| m.modified().ok())
}

/// Current mtime of `agents.toml`.
pub(crate) fn agents_mtime() -> Option<SystemTime> {
    mtime(crate::agent::agent_config::agents_config_path().as_deref())
}

/// Current mtime of `keybindings.json`.
pub(crate) fn keybindings_mtime() -> Option<SystemTime> {
    mtime(crate::paths::keybindings_file().as_deref())
}

/// Current mtime of `settings.toml`.
pub(crate) fn settings_mtime() -> Option<SystemTime> {
    mtime(crate::agent::settings_config::settings_config_path().as_deref())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paths::TestPathGuard;

    #[test]
    fn mtime_is_none_for_none_and_missing_paths() {
        assert!(mtime(None).is_none());
        let missing = std::path::Path::new("/nonexistent/thurbox/does-not-exist.toml");
        assert!(mtime(Some(missing)).is_none());
    }

    #[test]
    fn mtime_is_some_for_an_existing_file() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("config.toml");
        std::fs::write(&file, "x").unwrap();
        assert!(mtime(Some(&file)).is_some());
    }

    #[test]
    fn config_wrappers_are_none_before_files_exist() {
        let dir = tempfile::tempdir().unwrap();
        let _guard = TestPathGuard::new(dir.path());
        // Fresh sandbox: none of the live-reloadable config files exist yet.
        assert!(agents_mtime().is_none());
        assert!(keybindings_mtime().is_none());
        assert!(settings_mtime().is_none());
    }

    #[test]
    fn config_wrappers_report_some_once_their_file_exists() {
        let dir = tempfile::tempdir().unwrap();
        let _guard = TestPathGuard::new(dir.path());

        // Materialize each config file at its own resolved path, then confirm
        // the matching wrapper observes it.
        for path in [
            crate::agent::agent_config::agents_config_path(),
            crate::paths::keybindings_file(),
            crate::agent::settings_config::settings_config_path(),
        ]
        .into_iter()
        .flatten()
        {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).unwrap();
            }
            std::fs::write(&path, "x").unwrap();
        }

        assert!(agents_mtime().is_some());
        assert!(keybindings_mtime().is_some());
        assert!(settings_mtime().is_some());
    }
}
