//! Live reload of hand-edited config files.
//!
//! Grouped out of the [`App`](super::App) god object. `tick` polls the
//! mtimes of `agents.toml` and `keybindings.json` (~1/s, two `stat` calls)
//! and reloads them in place on change — no restart needed after editing.
//!
//! Deliberately *not* reloaded live: `hosts.toml` (SSH backends are
//! registered in main's `BackendRegistry` at startup) and `settings.toml`
//! (published through a write-once global so values can't drift mid-frame).
//! Both stay restart-only, as documented in `docs/CONFIG.md`.

use std::path::Path;
use std::time::SystemTime;

/// Last-seen mtimes of the live-reloadable config files.
#[derive(Default)]
pub(crate) struct ConfigReloadState {
    pub(crate) agents_mtime: Option<SystemTime>,
    pub(crate) keybindings_mtime: Option<SystemTime>,
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
