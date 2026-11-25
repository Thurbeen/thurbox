use std::path::PathBuf;

pub const DEFAULT_SCROLLBACK_LINES: usize = 1000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MuxSettings {
    pub scrollback_lines: usize,
    pub data_dir: Option<PathBuf>,
    pub relocated_data_dir: Option<PathBuf>,
    pub socket: Option<String>,
}

impl Default for MuxSettings {
    fn default() -> Self {
        Self {
            scrollback_lines: DEFAULT_SCROLLBACK_LINES,
            data_dir: None,
            relocated_data_dir: None,
            socket: None,
        }
    }
}

#[cfg(test)]
pub(crate) fn for_tests() -> MuxSettings {
    MuxSettings {
        data_dir: Some(test_sandbox().join("data")),
        ..MuxSettings::default()
    }
}

#[cfg(test)]
fn test_sandbox() -> PathBuf {
    extern "C" fn remove_sandbox() {
        if let Some(dir) = SANDBOX.get() {
            let _ = std::fs::remove_dir_all(dir);
        }
    }

    static SANDBOX: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();

    SANDBOX
        .get_or_init(|| {
            let dir = tempfile::Builder::new()
                .prefix("runtime-mux-unittest-")
                .tempdir()
                .expect("temp sandbox for the unit-test data dir");
            // SAFETY: registered once, from inside `get_or_init`, with a plain
            // function pointer that outlives the process. `keep()` disarms
            // `TempDir`'s own `Drop` so the two never race.
            unsafe { libc::atexit(remove_sandbox) };
            dir.keep()
        })
        .clone()
}
