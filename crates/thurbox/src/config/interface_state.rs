//! What the interface remembers between runs that is no setting a user
//! writes by hand: the theme last chosen, the editor `Ctrl+O` opens, the
//! manual order of the session list and each session's companion shell, kept
//! in `interface.json` beside `settings.toml`.
//!
//! The order and the shells are the interface's own: Fleet keeps Tasks, and
//! neither the order a person sorted them in nor the shell window an
//! interface opened beside one is a fact about a Task.
//!
//! Plain JSON, rewritten whole and atomically on each change, under a lock
//! every process takes ([`crate::paths`]'s file lock), so two writers never
//! lose each other's change. A file that is missing or unreadable is the
//! defaults: losing a remembered theme is not worth refusing to start over.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};

use crate::session::settings::EditorMode;

const FILE: &str = "interface.json";

/// Moves on every update this process makes, so a reader of the order or the
/// shells (the snapshot) reads the file again only when it changed.
static GENERATION: AtomicU64 = AtomicU64::new(0);

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
    /// The session list's manual order: session ids, first on top. A session
    /// not named sorts after every one that is.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub order: Vec<String>,
    /// Each session's companion shell, by session id: it outlives the
    /// interface in a window of its own and is adopted again by the next one.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub shells: BTreeMap<String, ShellRecord>,
}

/// Where one companion shell is, and how to know it again.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShellRecord {
    /// The qualified route of the multiplexer it runs on (`local:tmux`,
    /// `wsl:ubuntu:tmux`).
    pub route: String,
    /// Its pane on that multiplexer. A restarted server can reissue the id.
    pub pane: String,
    /// The id its window is stamped with: a pane is adopted again only where
    /// the window still carries it, never because the id came back.
    pub stamp: String,
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
    super::file::locked(&path, || {
        let mut state = load();
        change(&mut state);
        let text = serde_json::to_string_pretty(&state).map_err(std::io::Error::other)?;
        super::file::save(&path, text)
    })??;
    GENERATION.fetch_add(1, Ordering::Relaxed);
    Ok(())
}

/// Where this process's updates stand: compare two reads to learn whether
/// the file changed in between.
pub fn generation() -> u64 {
    GENERATION.load(Ordering::Relaxed)
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

    /// Writers that interleave their read-change-write each keep their
    /// change: every thread's entry is in the file at the end.
    #[test]
    fn interleaved_writers_lose_no_update() {
        let dir = tempfile::tempdir().unwrap();
        let writers: Vec<_> = (0..8)
            .map(|writer| {
                let dir = dir.path().to_path_buf();
                std::thread::spawn(move || {
                    let _guard = super::super::file::isolate(&dir);
                    for n in 0..10 {
                        update(|s| {
                            s.order.push(format!("{writer}-{n}"));
                        })
                        .unwrap();
                    }
                })
            })
            .collect();
        for writer in writers {
            writer.join().unwrap();
        }
        let _guard = super::super::file::isolate(dir.path());
        assert_eq!(load().order.len(), 80);
    }
}
