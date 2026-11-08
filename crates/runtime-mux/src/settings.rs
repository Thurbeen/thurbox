//! What the embedding program injects: the settings the runtime honours and
//! where that instance keeps its data. Passed in by whoever loaded them —
//! thurbox builds one from its `settings.toml` and its data directory — and
//! never read from the embedder's own state.

use std::path::PathBuf;

/// Lines a terminal keeps when nothing says otherwise: the default of
/// thurbox's `scrollback_lines` setting.
pub const DEFAULT_SCROLLBACK_LINES: usize = 1000;

/// The runtime's injected configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MuxSettings {
    /// Lines a terminal on a backend keeps, which bounds the history it
    /// captures.
    pub scrollback_lines: usize,
    /// The instance's data directory. Anchors the local socket's directory
    /// and tells an inherited socket override from this instance's own
    /// (ADR-12, [`crate::instance`]). `None` leaves both to the multiplexer.
    pub data_dir: Option<PathBuf>,
    /// The data directory when the instance has been relocated from its
    /// default, which puts its sessions on a socket of their own.
    pub relocated_data_dir: Option<PathBuf>,
}

impl Default for MuxSettings {
    fn default() -> Self {
        Self {
            scrollback_lines: DEFAULT_SCROLLBACK_LINES,
            data_dir: None,
            relocated_data_dir: None,
        }
    }
}

/// The settings a unit test runs a backend with: the defaults, with a data
/// directory in a sandbox removed when the test process exits, so a local
/// server's socket directory never lands in the operator's own.
#[cfg(test)]
pub(crate) fn for_tests() -> MuxSettings {
    MuxSettings {
        data_dir: Some(test_sandbox().join("data")),
        ..MuxSettings::default()
    }
}

#[cfg(test)]
fn test_sandbox() -> PathBuf {
    /// The `atexit` handler: `SANDBOX` is initialized by the time the process
    /// can reach an exit, and reading a `OnceLock` needs no lock.
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
