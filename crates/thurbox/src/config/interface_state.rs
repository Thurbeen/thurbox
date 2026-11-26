use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};

use crate::session::settings::EditorMode;

const FILE: &str = "interface.json";

static GENERATION: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct InterfaceState {
    pub theme: Option<String>,
    pub editor_command: Option<String>,
    pub editor_mode: EditorMode,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub order: Vec<String>,
}

pub fn load() -> InterfaceState {
    super::file::path(FILE)
        .and_then(|path| std::fs::read_to_string(path).ok())
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

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
