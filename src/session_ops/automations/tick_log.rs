//! What the last `automation tick` did, and a log of every one that failed
//! (issue #1508).
//!
//! The heartbeat runs the tick with its output thrown away, so a tick that
//! failed before it claimed anything — the database would not open, the due
//! scan could not read a row — used to leave no trace at all: scheduling could
//! stop for an hour and nothing said so. Each tick now records its outcome in
//! `automation-tick.json` beside the database, and a failed one also appends a
//! timestamped line to `automation-tick.log`. `runtime status` reads the first.
//!
//! Files rather than rows, because the failure worth recording most is the one
//! where the database is what failed — and a tick every minute writing a row
//! would rebuild the interface's snapshot for nothing.

use std::io::Write;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Bytes the log may grow to before its older half is dropped.
const LOG_CAP: usize = 64 * 1024;

/// The outcome of one tick.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LastTick {
    /// Unix millis, when the tick ended.
    pub at: u64,
    /// Whether the tick itself ran. A tick whose fires failed still ran.
    pub ok: bool,
    /// Why it did not, when it did not.
    pub error: Option<String>,
    /// How many automations it fired, and how many of those failed or timed
    /// out.
    pub fired: usize,
    pub failed: usize,
}

/// The directory the database lives in: the instance's own, so a sandbox or a
/// dev build keeps its tick record apart from the installed one's.
fn dir() -> Option<PathBuf> {
    crate::paths::database_file().and_then(|p| p.parent().map(std::path::Path::to_path_buf))
}

/// Where a failed tick is logged.
pub fn log_path() -> Option<PathBuf> {
    dir().map(|d| d.join("automation-tick.log"))
}

fn state_path() -> Option<PathBuf> {
    dir().map(|d| d.join("automation-tick.json"))
}

/// Record `tick` as the last one, and log it if it failed. Best-effort: a
/// record that cannot be written is a warning, never a second failure.
pub fn record(tick: &LastTick) {
    if let Err(e) = write_state(tick) {
        tracing::warn!("could not record the tick's outcome: {e}");
    }
    if let (false, Some(error)) = (tick.ok, &tick.error) {
        if let Err(e) = append_log(tick.at, error) {
            tracing::warn!("could not log the failed tick: {e}");
        }
    }
}

/// A tick that failed with `error`, now.
pub fn failed(error: &str) -> LastTick {
    LastTick {
        at: crate::sync::current_time_millis(),
        ok: false,
        error: Some(error.to_string()),
        fired: 0,
        failed: 0,
    }
}

/// The last tick recorded, if any has been.
pub fn last() -> Option<LastTick> {
    let raw = std::fs::read_to_string(state_path()?).ok()?;
    serde_json::from_str(&raw).ok()
}

/// Written beside the target and renamed over it, so a reader never sees half
/// a document.
fn write_state(tick: &LastTick) -> std::io::Result<()> {
    let path = state_path().ok_or_else(no_data_dir)?;
    let json = serde_json::to_string(tick).map_err(std::io::Error::other)?;
    let tmp = path.with_extension(format!("json.{}", std::process::id()));
    std::fs::write(&tmp, json)?;
    std::fs::rename(&tmp, &path)
}

fn append_log(at: u64, error: &str) -> std::io::Result<()> {
    let path = log_path().ok_or_else(no_data_dir)?;
    let stamp = chrono::DateTime::<chrono::Utc>::from_timestamp_millis(at as i64)
        .map(|t| t.to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
        .unwrap_or_else(|| at.to_string());
    let line = format!("{stamp} tick failed: {}\n", error.replace('\n', " "));
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)?;
    file.write_all(line.as_bytes())?;
    drop(file);
    trim(&path)
}

/// Keep the log under [`LOG_CAP`] by dropping its older half, on a line
/// boundary. A failing heartbeat writes a line a minute; this bounds it.
fn trim(path: &std::path::Path) -> std::io::Result<()> {
    if std::fs::metadata(path)?.len() as usize <= LOG_CAP {
        return Ok(());
    }
    let text = std::fs::read_to_string(path)?;
    let cut = text.len() - LOG_CAP / 2;
    let start = text[cut..].find('\n').map_or(text.len(), |i| cut + i + 1);
    std::fs::write(path, &text[start..])
}

fn no_data_dir() -> std::io::Error {
    std::io::Error::other("the data directory could not be resolved")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_failed_tick_is_logged_and_the_last_outcome_kept() {
        let temp = tempfile::TempDir::new().unwrap();
        let _guard = crate::paths::TestPathGuard::new(temp.path());
        assert_eq!(last(), None);
        let failure = failed("due_automations: invalid schedule\nsecond line");
        record(&failure);
        assert_eq!(last(), Some(failure));
        let log = std::fs::read_to_string(log_path().unwrap()).unwrap();
        assert!(log.ends_with("tick failed: due_automations: invalid schedule second line\n"));

        let ok = LastTick {
            at: 5,
            ok: true,
            error: None,
            fired: 2,
            failed: 1,
        };
        record(&ok);
        assert_eq!(last(), Some(ok));
        assert_eq!(
            std::fs::read_to_string(log_path().unwrap()).unwrap(),
            log,
            "a tick that ran is not logged"
        );
    }

    #[test]
    fn the_log_drops_its_older_half_past_the_cap() {
        let temp = tempfile::TempDir::new().unwrap();
        let _guard = crate::paths::TestPathGuard::new(temp.path());
        let error = "x".repeat(200);
        for _ in 0..(LOG_CAP / 200 + 50) {
            record(&failed(&error));
        }
        let log = std::fs::read_to_string(log_path().unwrap()).unwrap();
        assert!(log.len() <= LOG_CAP, "{}", log.len());
        assert!(log.lines().all(|l| l.ends_with(&error)), "lines stay whole");
    }
}
