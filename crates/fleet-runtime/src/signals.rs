//! The signal port's adapter: one file per session under Fleet's data
//! directory, on this machine.
//!
//! A session on the local host gets `<dir>/<key>.jsonl` and its hooks
//! append to it through `fleet signal`. A session on any other host gets a
//! file on that host, under its home ([`place_file`]), which the `fleet`
//! there appends to. This adapter cannot read it: the exec runtime reads it
//! back through the runtime's `exec`, over the connection the runtime
//! already has to the host.
//!
//! ```text
//! local:  hook ─fleet signal─▶ <data>/signals/<key>.jsonl ◀─read── Fleet
//! remote: hook ─fleet signal─▶ ~/.local/state/fleet/signals/<key>.jsonl
//!                                    ◀─runtime exec (tail)── Fleet
//! ```

use std::fs::{self, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use fleet_api::Observation;
use fleet_api::signal::{MAX_LINE, Reported, Signal, Signals};
use fleet_api::task::{LOCAL_HOST, SessionRef};

/// The most of one file Fleet reads, from its end, so a runaway hook cannot
/// stall a pass.
const MAX_FILE: u64 = 8 * 1024 * 1024;

/// Where a session on a host other than this machine reports, under the
/// host's home: `fleet signal` there expands the `~/`, which this machine
/// cannot, as it does not know that home.
pub const PLACE_DIR: &str = "~/.local/state/fleet/signals";

/// The file a session under `key` on another host reports to, as that
/// host's `fleet signal` reads it; `None` for a key that is not a plain
/// name.
pub fn place_file(key: &str) -> Option<String> {
    let plain = !key.is_empty() && !key.starts_with('.') && !key.contains(['/', '\\']);
    plain.then(|| format!("{PLACE_DIR}/{key}.jsonl"))
}

/// `file` as `fleet signal` opens it: a leading `~/` is the user's home.
pub fn expand_home(file: &Path) -> PathBuf {
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"));
    match (file.to_string_lossy().strip_prefix("~/"), home) {
        (Some(rest), Some(home)) => Path::new(&home).join(rest),
        _ => file.to_path_buf(),
    }
}

#[derive(Debug, Clone)]
pub struct FileSignals {
    dir: PathBuf,
}

impl FileSignals {
    /// The adapter over `dir`, made when missing.
    pub fn open(dir: impl Into<PathBuf>) -> io::Result<FileSignals> {
        let dir = dir.into();
        fs::create_dir_all(&dir)?;
        Ok(FileSignals { dir })
    }

    fn path(&self, key: &str) -> Option<PathBuf> {
        let plain = !key.is_empty() && !key.starts_with('.') && !key.contains(['/', '\\']);
        plain.then(|| self.dir.join(format!("{key}.jsonl")))
    }
}

impl FileSignals {
    /// Keep `offset`, the length of the place's signal file when the session
    /// under `key` on another host was last started: what the file holds
    /// past it is the new process's. Fleet does not write that file, so the
    /// restart is kept here, and as a length rather than a time, so a
    /// place's clock need not agree with Fleet's.
    pub fn mark_started(&self, key: &str, offset: u64) -> io::Result<()> {
        let path = self
            .started_path(key)
            .ok_or_else(|| io::Error::other("the session key is not a plain name"))?;
        let tmp = path.with_extension("started.tmp");
        fs::write(&tmp, offset.to_string())?;
        fs::rename(&tmp, &path)
    }

    /// Forget the restart of the session under `key`, when the length of its
    /// file could not be read: nothing is filtered.
    pub fn clear_started(&self, key: &str) -> io::Result<()> {
        match self.started_path(key).map(fs::remove_file) {
            Some(Err(e)) if e.kind() != io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        }
    }

    /// The length the place's file had at the session's last start, as
    /// [`FileSignals::mark_started`] kept it.
    pub fn started(&self, key: &str) -> Option<u64> {
        let path = self.started_path(key)?;
        fs::read_to_string(path).ok()?.trim().parse().ok()
    }

    fn started_path(&self, key: &str) -> Option<PathBuf> {
        self.path(key).map(|p| p.with_extension("started"))
    }
}

/// Append `signal` to `file` as one line, in one write, so lines from hooks
/// running at once never interleave.
pub fn append(file: &Path, signal: &Signal) -> io::Result<()> {
    let mut line = serde_json::to_vec(signal).map_err(io::Error::other)?;
    line.push(b'\n');
    if line.len() > MAX_LINE {
        return Err(io::Error::other("the signal is too long"));
    }
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(file)?
        .write_all(&line)
}

impl Signals for FileSignals {
    fn file(&self, host: &str, key: &str) -> Option<String> {
        if host != LOCAL_HOST {
            return place_file(key);
        }
        self.path(key).map(|p| p.to_string_lossy().into_owned())
    }

    fn read(&self, session: &SessionRef) -> Observation<Reported> {
        if session.host != LOCAL_HOST {
            return Observation::unknown(format!(
                "signals from host {} are read through the runtime's exec, which this runtime does not offer",
                session.host
            ));
        }
        let Some(path) = self.path(&session.key) else {
            return Observation::unknown("the session key is not a plain name");
        };
        let read = || -> io::Result<String> {
            let mut file = fs::File::open(&path)?;
            // Only the tail of a long file: the last state is at its end,
            // and the cut line is skipped as unreadable.
            let len = file.metadata()?.len();
            file.seek(SeekFrom::Start(len.saturating_sub(MAX_FILE)))?;
            let mut bytes = Vec::new();
            file.read_to_end(&mut bytes)?;
            Ok(String::from_utf8_lossy(&bytes).into_owned())
        };
        match read() {
            Ok(text) => Observation::Known(Reported::fold(&text)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                Observation::Known(Reported::default())
            }
            Err(e) => Observation::unknown(format!("cannot read the signal file: {e}")),
        }
    }
}
