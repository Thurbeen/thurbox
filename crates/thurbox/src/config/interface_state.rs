//! What the interface remembers between runs that is no setting a user
//! writes by hand: the theme last chosen and the editor `Ctrl+O` opens, kept
//! in `interface.json` beside `settings.toml`.
//!
//! Plain JSON, rewritten whole and atomically on each change. A file that is
//! missing or unreadable is the defaults: losing a remembered theme is not
//! worth refusing to start over.

use serde::{Deserialize, Serialize};

use crate::session::settings::EditorMode;

const FILE: &str = "interface.json";

/// The remembered values. Every field is optional, so a file written by a
/// later release with more fields still reads.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct InterfaceState {
    /// The theme the picker last applied.
    pub theme: Option<String>,
    /// The editor command (`thurbox-cli editor set`).
    pub editor_command: Option<String>,
    /// How the editor is launched (`thurbox-cli editor mode`).
    pub editor_mode: EditorMode,
}

/// The state as stored, or the defaults when there is none to read.
pub fn load() -> InterfaceState {
    super::file::path(FILE)
        .and_then(|path| std::fs::read_to_string(path).ok())
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

/// Apply `change` to the stored state and write it back.
pub fn update(change: impl FnOnce(&mut InterfaceState)) -> std::io::Result<()> {
    let path = super::file::path(FILE).ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "could not resolve the config directory",
        )
    })?;
    let mut state = load();
    change(&mut state);
    let text = serde_json::to_string_pretty(&state).map_err(std::io::Error::other)?;
    super::file::save(&path, text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_update_is_read_back_and_keeps_the_other_fields() {
        let dir = tempfile::tempdir().unwrap();
        let _guard = super::super::file::isolate(dir.path());
        assert_eq!(load(), InterfaceState::default());
        update(|s| s.theme = Some("nord".into())).unwrap();
        update(|s| s.editor_command = Some("code --wait".into())).unwrap();
        let state = load();
        assert_eq!(state.theme.as_deref(), Some("nord"));
        assert_eq!(state.editor_command.as_deref(), Some("code --wait"));
    }
}
