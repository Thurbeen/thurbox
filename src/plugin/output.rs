//! Per-plugin log file writer with single-backup rotation.
//!
//! Stdout *and* stderr from a spawned plugin go here. Stdout matters because
//! the JSON-RPC reader strips well-formed response lines off it before they
//! reach the log — anything that *isn't* a valid response line is captured
//! verbatim, which is what plugin authors want when debugging.
//!
//! Rotation: when the current file would exceed [`MAX_LOG_BYTES`] after a
//! write, the writer renames `<name>.log` → `<name>.log.1` (clobbering any
//! older backup) and starts a fresh `<name>.log`. There is exactly one
//! backup; older history is intentionally discarded so a chatty plugin
//! cannot fill the disk.
//!
//! Logs live under `~/.local/share/thurbox/logs/plugins/<plugin>.log` (see
//! [`crate::paths::plugin_log_path`]). The runtime owns one writer per plugin.

use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::PathBuf;

/// Hard cap before rotation. Mirrors the figure in `docs/PLUGINS.md`.
pub const MAX_LOG_BYTES: u64 = 5 * 1024 * 1024;

/// Append-only writer for a single plugin's log file.
///
/// Not async on purpose — the runtime calls it from a `spawn_blocking` context
/// for stderr/stdout draining, where blocking writes are fine.
pub struct PluginLogWriter {
    path: PathBuf,
    file: File,
    bytes_written: u64,
    max_bytes: u64,
}

impl PluginLogWriter {
    /// Open (or create) the log at `path`, ensuring its parent directory exists.
    /// `max_bytes` lets tests force rotation without writing 5 MiB.
    pub fn open(path: PathBuf, max_bytes: u64) -> io::Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let file = OpenOptions::new().create(true).append(true).open(&path)?;
        let bytes_written = file.metadata().map(|m| m.len()).unwrap_or(0);
        Ok(Self {
            path,
            file,
            bytes_written,
            max_bytes,
        })
    }

    /// Append `data` to the log, rotating first if the write would exceed
    /// the cap. Best-effort: a rotation IO error is logged and writing
    /// continues to whatever file handle is current — the runtime should
    /// never crash because logs misbehaved.
    pub fn write_chunk(&mut self, data: &[u8]) -> io::Result<()> {
        if self.bytes_written + data.len() as u64 > self.max_bytes {
            if let Err(e) = self.rotate() {
                tracing::warn!(error = %e, path = %self.path.display(), "Plugin log rotate failed");
            }
        }
        self.file.write_all(data)?;
        self.bytes_written += data.len() as u64;
        Ok(())
    }

    /// Path to the active log file (the `.log`, not the `.log.1` backup).
    pub fn path(&self) -> &std::path::Path {
        &self.path
    }

    fn rotate(&mut self) -> io::Result<()> {
        let backup = self.path.with_extension("log.1");
        // Drop the file handle before rename — Linux is fine with renaming an
        // open file but Windows objects, and being explicit is cheap.
        let _ = std::mem::replace(
            &mut self.file,
            OpenOptions::new()
                .create(true)
                .append(true)
                .open(&self.path)?,
        );
        // Move the current log aside; ignore "doesn't exist" since rotation
        // can race with an external delete (e.g., uninstall).
        match std::fs::rename(&self.path, &backup) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
        // Re-open the (now nonexistent) primary path, creating a fresh file.
        self.file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        self.bytes_written = 0;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_creates_parent_directory() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested/deeper/test.log");
        let mut w = PluginLogWriter::open(path.clone(), MAX_LOG_BYTES).unwrap();
        w.write_chunk(b"hello\n").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"hello\n");
    }

    #[test]
    fn rotates_when_exceeding_cap() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("plug.log");
        let mut w = PluginLogWriter::open(path.clone(), 10).unwrap();

        w.write_chunk(b"123456").unwrap();
        // Next write would push us past 10 bytes total — should rotate first.
        w.write_chunk(b"7890ABCDEF").unwrap();

        let backup = path.with_extension("log.1");
        assert!(backup.exists(), "expected backup to be created");
        assert_eq!(std::fs::read(&backup).unwrap(), b"123456");
        assert_eq!(std::fs::read(&path).unwrap(), b"7890ABCDEF");
    }

    #[test]
    fn rotation_keeps_only_one_backup() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("plug.log");
        let mut w = PluginLogWriter::open(path.clone(), 5).unwrap();

        w.write_chunk(b"AAAA").unwrap();
        w.write_chunk(b"BBBB").unwrap(); // rotates, .1 = AAAA, log = BBBB
        w.write_chunk(b"CCCC").unwrap(); // rotates, .1 = BBBB (AAAA dropped), log = CCCC

        let backup = path.with_extension("log.1");
        assert_eq!(std::fs::read(&backup).unwrap(), b"BBBB");
        assert_eq!(std::fs::read(&path).unwrap(), b"CCCC");
    }

    #[test]
    fn append_preserves_existing_content() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("plug.log");
        std::fs::write(&path, b"prior\n").unwrap();
        let mut w = PluginLogWriter::open(path.clone(), MAX_LOG_BYTES).unwrap();
        w.write_chunk(b"new\n").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"prior\nnew\n");
    }
}
